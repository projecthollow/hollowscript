//! The dedicated large-stack thread every unbounded-recursion pass of the
//! front end runs on (section 1.10).
//!
//! Section 1.10 requires that a construct within the structural limits
//! "MUST NOT exhaust the host's stack", and that must hold whatever stack
//! the caller happens to be running on: a host embedding this implementation
//! is free to call it from a thread as small as it likes. Parsing, checking
//! and compiling each recurse over the syntax tree or the HIR roughly once
//! per level of nesting a program uses, up to the 250-level limit of section
//! 1.10 (or, through a chain's own weight, deeper still; see
//! `hollowscript_syntax::parser`'s own documentation), so none of the three
//! can promise not to exhaust a small caller stack on its own account. They
//! instead run on a thread this module spawns, sized generously for that
//! bound, and hand the answer back once it is done.
//!
//! `wasm32-unknown-unknown` has no threads, so there the same passes run on the
//! calling thread's own stack: the shadow stack of the WebAssembly module,
//! which `hollowscript-wasm`'s `build.rs` sizes at [`DEDICATED_STACK_SIZE`] for
//! the `tooling` build (see [`run_on_dedicated_stack`]). It had been left at
//! the linker's 1 MiB, against a comment that claimed 16 MiB, and 233 nested
//! closures trapped the module.
//!
//! [`DEDICATED_STACK_SIZE`] was chosen for the parser and reused here rather
//! than measured separately for each pass: measuring the checker's own
//! recursion (nested closure calls, the shape a security review reported
//! crashing a small caller thread) against the smallest stack size a host
//! is expected to hand this implementation, 512 KiB, found it survives to
//! roughly 90 levels of nesting on an unoptimised build before the guard
//! page is hit, and a linear extrapolation from that measurement puts the
//! full 250-level limit at under 1.5 MiB, comfortably inside this module's
//! 16 MiB. The parser's own comment records the same conclusion for its
//! pass; nothing here recurses more deeply per level than the checker's own
//! measurement covers.

use std::thread;

/// The stack size every dedicated large-stack thread in this workspace
/// spawns with. Sized against the checker's own measurement (see the module
/// documentation) with a wide margin, and shared by parsing, formatting,
/// checking and compiling so that one number, chosen once, need only be
/// reviewed once.
pub const DEDICATED_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Why [`run_on_dedicated_stack`] could not hand back `f`'s answer: the host
/// could not create the thread, or the thread did not return normally
/// (which, for a pass documented never to panic, only guards against a
/// defect elsewhere actually doing so).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DedicatedStackError {
    /// `std::thread::Builder::spawn` itself failed, for example because the
    /// host is out of resources for new threads.
    SpawnFailed,
    /// The thread panicked instead of returning `f`'s value.
    Panicked,
}

/// Runs `f` on a dedicated thread with [`DEDICATED_STACK_SIZE`] of stack, so
/// that recursion within the structural limits of section 1.10 cannot
/// exhaust whatever stack the calling thread happens to have. On
/// `wasm32-unknown-unknown`, which has no thread backend at all, `f` runs on
/// the calling thread instead, on the module's own shadow stack. That stack
/// is 1 MiB unless the build says otherwise, which is not enough for the
/// deepest legal program (250 nested closures use a little over 1.1 MiB), so
/// the `tooling` build of `hollowscript-wasm`, the only one that runs the
/// front end, is linked with a 16 MiB stack (that crate's `build.rs`), the
/// size of the thread every other target gets here. Section 1.10's guarantee
/// therefore holds there by the size of that stack and not by a thread.
///
/// `wasm32` is decided ahead of time with `cfg!`, not by reacting to
/// `Builder::spawn`'s `Err`, because `spawn`'s own contract drops a closure
/// it fails to hand to a thread: there would be no second chance to call it
/// once spawning had already been attempted and lost it.
///
/// # Errors
///
/// Returns [`DedicatedStackError::SpawnFailed`] when the host could not
/// create the thread, or [`DedicatedStackError::Panicked`] when it did not
/// return normally. Neither is a case a caller can usefully recover `f`'s
/// answer from; both are meant to be turned into the caller's own total,
/// internal-error answer instead.
pub fn run_on_dedicated_stack<F, T>(f: F) -> Result<T, DedicatedStackError>
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    if cfg!(target_arch = "wasm32") {
        return Ok(f());
    }
    thread::scope(|scope| {
        thread::Builder::new()
            .stack_size(DEDICATED_STACK_SIZE)
            .spawn_scoped(scope, f)
            .map_err(|_| DedicatedStackError::SpawnFailed)
            .and_then(|handle| handle.join().map_err(|_| DedicatedStackError::Panicked))
    })
}

#[cfg(test)]
mod tests {
    use super::{DedicatedStackError, run_on_dedicated_stack};

    #[test]
    fn runs_the_closure_and_returns_its_answer() {
        assert_eq!(run_on_dedicated_stack(|| 2 + 2), Ok(4));
    }

    #[test]
    fn a_panicking_closure_reports_panicked_rather_than_unwinding_into_the_caller() {
        // A defect elsewhere that manages to panic on the dedicated thread
        // must be turned into `Err` here, not propagate and take the
        // caller down with it.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = run_on_dedicated_stack(|| -> i32 { panic!("defect") });
        std::panic::set_hook(previous_hook);
        assert_eq!(result, Err(DedicatedStackError::Panicked));
    }
}
