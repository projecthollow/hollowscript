//! The per-run counters of chapter 11: steps, the allocation counter, output
//! bytes and call depth, checked against a host's [`hollowscript_core::Limits`].
//!
//! A [`RunCounters`] is fresh for the load of a program and for each call of
//! an exported function (section 11.2, "what a run is"). Charging is
//! `charge_steps` first, then the caller's own memory charge, matching
//! section 11.3's "order of faults" rule; the interpreter is the only caller
//! of these methods, so the rule only has to be kept in one place.

use hollowscript_core::fault::Fault;
use hollowscript_core::limits::structural::MAX_CHARGE;
use hollowscript_core::{Limits, Symbol};

/// The counters of one run.
#[derive(Copy, Clone, Debug, Default)]
pub struct RunCounters {
    steps: u64,
    allocated: u64,
    output: u64,
    depth: u64,
}

impl RunCounters {
    /// Fresh counters, all at zero.
    #[must_use]
    pub fn new() -> RunCounters {
        RunCounters::default()
    }

    /// The steps executed so far.
    #[must_use]
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// The bytes allocated so far.
    #[must_use]
    pub fn allocated(&self) -> u64 {
        self.allocated
    }

    /// The bytes written by `print` so far.
    #[must_use]
    pub fn output(&self) -> u64 {
        self.output
    }

    /// The number of active script calls.
    #[must_use]
    pub fn depth(&self) -> u64 {
        self.depth
    }

    /// Charges `amount` steps, at most [`MAX_CHARGE`] at a time as section
    /// 11.3 requires of every charge in the language. Larger amounts must be
    /// applied by the caller as several charges (section 11.3, "slices").
    ///
    /// # Errors
    ///
    /// Returns HS1001 if the charge would take the counter past `limits.max_steps`.
    ///
    /// # Panics
    ///
    /// If `amount` is 0 or greater than [`MAX_CHARGE`], which is a defect in
    /// the caller: every charge of the language is stated as 1 or more and
    /// sliced to at most 1,024 before it reaches here.
    pub fn charge_steps(&mut self, amount: u64, limits: &Limits) -> Result<(), Fault> {
        assert!(
            (1..=MAX_CHARGE).contains(&amount),
            "a step charge must be from 1 to {MAX_CHARGE}"
        );
        if self.steps.saturating_add(amount) > limits.max_steps {
            return Err(Fault::step_limit(limits.max_steps, self.steps));
        }
        // Cannot exceed `max_steps`, which a validated limit keeps under 2^63,
        // but a `Limits` is a plain record a host can fill with anything, and
        // nothing here may panic on any input, so the addition saturates.
        self.steps = self.steps.saturating_add(amount);
        Ok(())
    }

    /// Charges `bytes` against the allocation counter.
    ///
    /// # Errors
    ///
    /// Returns HS1002 if the charge would take the counter past `limits.max_memory_bytes`.
    pub fn charge_memory(&mut self, bytes: u64, limits: &Limits) -> Result<(), Fault> {
        if self.allocated.saturating_add(bytes) > limits.max_memory_bytes {
            return Err(Fault::memory_limit(
                limits.max_memory_bytes,
                self.allocated,
                bytes,
            ));
        }
        self.allocated = self.allocated.saturating_add(bytes);
        Ok(())
    }

    /// Charges `bytes` of `print` output.
    ///
    /// # Errors
    ///
    /// Returns HS1004 if the charge would take the counter past `limits.max_output_bytes`.
    pub fn charge_output(&mut self, bytes: u64, limits: &Limits) -> Result<(), Fault> {
        if self.output.saturating_add(bytes) > limits.max_output_bytes {
            return Err(Fault::output_limit(limits.max_output_bytes, self.output));
        }
        self.output = self.output.saturating_add(bytes);
        Ok(())
    }

    /// Enters one script call.
    ///
    /// # Errors
    ///
    /// Returns HS1005 if this would take the depth past `limits.max_call_depth`.
    pub fn enter_call(&mut self, limits: &Limits) -> Result<(), Fault> {
        if self.depth >= limits.max_call_depth {
            return Err(Fault::call_depth(limits.max_call_depth, self.depth));
        }
        self.depth += 1;
        Ok(())
    }

    /// Leaves one script call.
    ///
    /// Calling this more times than [`RunCounters::enter_call`] is a defect
    /// in the interpreter, never a property of a script. A debug build
    /// catches it with a debug assertion; a release build saturates at zero
    /// rather than panicking, because nothing in this crate may panic on any
    /// input, including its own bugs (section 19.1).
    pub fn leave_call(&mut self) {
        debug_assert!(self.depth > 0, "leave_call without a matching enter_call");
        self.depth = self.depth.saturating_sub(1);
    }
}

/// The retained size check of section 11.4, run after a successful load and
/// after each call: the tree sizes of every top-level binding, plus, for each
/// binding a reachable closure captured, 16 plus the tree size of its current
/// value, each captured binding counted once however many closures share it.
///
/// # Errors
///
/// Returns HS1003 if the total exceeds `limits.max_memory_bytes`.
pub fn check_retained_size(
    top_level_sizes: impl Iterator<Item = u64>,
    captured: impl Iterator<Item = (Symbol, u64)>,
    limits: &Limits,
) -> Result<(), Fault> {
    let mut total: u64 = 0;
    for size in top_level_sizes {
        total = total.saturating_add(size);
    }
    let mut counted = std::collections::BTreeSet::new();
    for (binding, size) in captured {
        if counted.insert(binding) {
            total = total.saturating_add(16).saturating_add(size);
        }
    }
    if total > limits.max_memory_bytes {
        Err(Fault::retained_limit(limits.max_memory_bytes, total))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{RunCounters, check_retained_size};
    use hollowscript_core::{Interner, Limits};

    /// A `Limits` is a record a host can fill with anything, including a limit
    /// no validation would let through. Charging against one must still never
    /// panic: the counters saturate instead of overflowing, which a build with
    /// overflow checks (every profile this workspace uses) would otherwise turn
    /// into a panic, and on `wasm32` a trap.
    #[test]
    fn a_limit_above_the_validated_range_cannot_overflow_a_counter() {
        let limits = Limits {
            max_steps: u64::MAX,
            max_memory_bytes: u64::MAX,
            max_output_bytes: u64::MAX,
            max_call_depth: u64::MAX,
        };
        let mut c = RunCounters::new();
        for _ in 0..3 {
            assert!(c.charge_memory(u64::MAX - 1, &limits).is_ok());
            assert!(c.charge_output(u64::MAX - 1, &limits).is_ok());
        }
        assert_eq!(c.allocated(), u64::MAX);
        assert_eq!(c.output(), u64::MAX);
        for _ in 0..3 {
            assert!(c.charge_steps(1, &limits).is_ok());
        }
        assert_eq!(c.steps(), 3);
    }

    #[test]
    fn steps_are_charged_up_to_the_limit() {
        let limits = Limits {
            max_steps: 10,
            ..Limits::default()
        };
        let mut c = RunCounters::new();
        assert!(c.charge_steps(9, &limits).is_ok());
        assert!(c.charge_steps(1, &limits).is_ok());
        assert_eq!(c.steps(), 10);
        let err = c.charge_steps(1, &limits).unwrap_err();
        assert_eq!(err.code, hollowscript_core::fault::FaultCode::StepLimit);
    }

    #[test]
    #[should_panic(expected = "a step charge must be from 1 to 1024")]
    fn a_charge_above_the_slice_size_is_a_caller_defect() {
        let mut c = RunCounters::new();
        let _ = c.charge_steps(1025, &Limits::default());
    }

    #[test]
    fn call_depth_tracks_enter_and_leave() {
        let limits = Limits {
            max_call_depth: 2,
            ..Limits::default()
        };
        let mut c = RunCounters::new();
        assert!(c.enter_call(&limits).is_ok());
        assert!(c.enter_call(&limits).is_ok());
        assert!(c.enter_call(&limits).is_err());
        c.leave_call();
        assert!(c.enter_call(&limits).is_ok());
        assert_eq!(c.depth(), 2);
    }

    #[test]
    fn retained_size_counts_a_shared_capture_once() {
        let mut names = Interner::new();
        let x = names.intern("x");
        let limits = Limits {
            max_memory_bytes: 100,
            ..Limits::default()
        };
        // Two closures capturing the same binding count it once: 40 + 16 + 20 = 76.
        let ok = check_retained_size(
            std::iter::once(40),
            vec![(x, 20), (x, 20)].into_iter(),
            &limits,
        );
        assert!(ok.is_ok());
        let limits2 = Limits {
            max_memory_bytes: 50,
            ..Limits::default()
        };
        assert!(
            check_retained_size(std::iter::once(40), vec![(x, 20)].into_iter(), &limits2).is_err()
        );
    }
}
