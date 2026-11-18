//! Fallible allocation for sizes a program controls.
//!
//! Section 11.4's counters bound what a program may build, but a counter is
//! only arithmetic: the machine can run out before it is reached. A failed
//! allocation aborts Rust, and in the WebAssembly build an abort is a trap that
//! ends the whole instance (`docs/design/security.md`, "The WebAssembly
//! ceiling"). Every allocation whose size grows with something a program
//! chooses (a range, a list, a copy of one, the text a string function builds)
//! therefore asks the allocator first through the helpers here, and a refusal is
//! [`OutOfMemory`], which becomes the implementation limit fault HS1015
//! (section 1.10) at the operation that asked. The instance is left as it was.
//!
//! The helpers use `try_reserve` and `try_reserve_exact`, which are stable. A
//! global out-of-memory hook is not, so this covers the sites that use the
//! helpers, not every allocation the crate makes; small, fixed-size
//! allocations still abort.
//!
//! Under `cfg(test)` a thread local byte budget stands in for a small machine:
//! every request through a helper is deducted from it first, and a request past
//! what is left is refused exactly as the allocator would. That is how the unit
//! tests drive HS1015 deterministically without exhausting the test host.

use hollowscript_core::fault::Fault;

/// The allocator refused, or the test budget did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct OutOfMemory;

impl From<OutOfMemory> for Fault {
    fn from(_: OutOfMemory) -> Fault {
        Fault::implementation_limit("the memory this host can give one run")
    }
}

#[cfg(test)]
thread_local! {
    static BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Runs `body` with `bytes` of budget for the helpers, restoring the previous
/// budget afterwards. Test builds only.
#[cfg(test)]
pub(crate) fn with_budget<R>(bytes: usize, body: impl FnOnce() -> R) -> R {
    let before = BUDGET.with(|b| b.replace(Some(bytes)));
    let result = body();
    BUDGET.with(|b| b.set(before));
    result
}

/// Deducts `bytes` from the test budget, if there is one.
#[cfg(test)]
fn permit(bytes: usize) -> Result<(), OutOfMemory> {
    BUDGET.with(|budget| match budget.get() {
        None => Ok(()),
        Some(left) if bytes <= left => {
            budget.set(Some(left - bytes));
            Ok(())
        }
        Some(_) => Err(OutOfMemory),
    })
}

#[cfg(not(test))]
#[inline]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the test build's version can refuse, and both are called the same way"
)]
fn permit(_bytes: usize) -> Result<(), OutOfMemory> {
    Ok(())
}

fn bytes_of<T>(count: usize) -> Result<usize, OutOfMemory> {
    count.checked_mul(size_of::<T>()).ok_or(OutOfMemory)
}

/// An empty vector with room for exactly `count` elements.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn vec_with_capacity<T>(count: usize) -> Result<Vec<T>, OutOfMemory> {
    permit(bytes_of::<T>(count)?)?;
    let mut out = Vec::new();
    out.try_reserve_exact(count).map_err(|_| OutOfMemory)?;
    Ok(out)
}

/// Room for `additional` more elements, growing the way `reserve` does.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn reserve<T>(vec: &mut Vec<T>, additional: usize) -> Result<(), OutOfMemory> {
    if vec.capacity() - vec.len() >= additional {
        return Ok(());
    }
    permit(bytes_of::<T>(additional)?)?;
    vec.try_reserve(additional).map_err(|_| OutOfMemory)
}

/// `vec.push(item)`, asking the allocator for room first.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn push<T>(vec: &mut Vec<T>, item: T) -> Result<(), OutOfMemory> {
    reserve(vec, 1)?;
    vec.push(item);
    Ok(())
}

/// A copy of `source` with room for `extra` more elements, in one allocation.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn copy_of<T: Copy>(source: &[T], extra: usize) -> Result<Vec<T>, OutOfMemory> {
    let mut out = vec_with_capacity(source.len().checked_add(extra).ok_or(OutOfMemory)?)?;
    out.extend_from_slice(source);
    Ok(out)
}

/// `vec.extend_from_slice(source)`, asking the allocator for room first.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn extend_from_slice<T: Copy>(
    vec: &mut Vec<T>,
    source: &[T],
) -> Result<(), OutOfMemory> {
    reserve(vec, source.len())?;
    vec.extend_from_slice(source);
    Ok(())
}

/// Collects `iter` into a vector, reserving what its size hint promises up
/// front and growing, fallibly, for any more.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn collect<T, I: Iterator<Item = T>>(iter: I) -> Result<Vec<T>, OutOfMemory> {
    let (low, high) = iter.size_hint();
    // An exact iterator reserves exactly. One that may stop early (a filter)
    // reserves what it is sure of and grows by doubling.
    let mut out = if high == Some(low) {
        vec_with_capacity(low)?
    } else {
        let mut out = Vec::new();
        reserve(&mut out, low)?;
        out
    };
    for item in iter {
        push(&mut out, item)?;
    }
    Ok(out)
}

/// An empty string with room for exactly `bytes` bytes.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn string_with_capacity(bytes: usize) -> Result<String, OutOfMemory> {
    permit(bytes)?;
    let mut out = String::new();
    out.try_reserve_exact(bytes).map_err(|_| OutOfMemory)?;
    Ok(out)
}

/// `text.to_owned()`, asking the allocator first.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn owned(text: &str) -> Result<String, OutOfMemory> {
    let mut out = string_with_capacity(text.len())?;
    out.push_str(text);
    Ok(out)
}

/// Checks that `bytes` could be allocated now, without keeping them. For a
/// standard library function whose own allocation cannot be made fallible
/// (the scratch buffer of `slice::sort_by`): the probe is freed before the
/// real allocation, which the allocator then serves from the same space.
///
/// # Errors
///
/// [`OutOfMemory`] if the allocator refuses.
pub(crate) fn probe(bytes: usize) -> Result<(), OutOfMemory> {
    permit(bytes)?;
    let mut held: Vec<u8> = Vec::new();
    held.try_reserve_exact(bytes).map_err(|_| OutOfMemory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_refuses_what_it_cannot_cover_and_nothing_else() {
        with_budget(64, || {
            assert!(vec_with_capacity::<u8>(40).is_ok());
            assert_eq!(vec_with_capacity::<u8>(40).err(), Some(OutOfMemory));
            assert!(vec_with_capacity::<u8>(24).is_ok());
        });
        assert!(vec_with_capacity::<u8>(1 << 20).is_ok());
    }

    #[test]
    fn a_size_that_overflows_is_refused() {
        assert_eq!(
            vec_with_capacity::<u64>(usize::MAX / 4).err(),
            Some(OutOfMemory)
        );
    }

    #[test]
    fn collect_reserves_for_an_exact_iterator_once() {
        let out = collect(0..100u32).expect("small");
        assert_eq!(out.len(), 100);
        assert_eq!(out.capacity(), 100);
    }

    #[test]
    fn out_of_memory_is_the_implementation_limit() {
        let fault: Fault = OutOfMemory.into();
        assert_eq!(
            fault.code,
            hollowscript_core::fault::FaultCode::ImplementationLimit
        );
    }
}
