//! `check`, `build` and `load` must not exhaust their caller's stack
//! (section 1.10: "a construct within the limits MUST NOT exhaust the
//! host's stack"), including on a legal program that nests well within the
//! 250-level structural limit but recurses heavily while it is checked: a
//! chain of immediately invoked closures, each one binding and returning
//! its own argument.
//!
//! Before checking and compiling ran on their own dedicated, generously
//! sized stack the way [`hollowscript_syntax::parse`] already did, this
//! reliably aborted the process (SIGBUS or SIGABRT on a small enough
//! caller stack) instead of returning diagnostics or a result: exactly the
//! failure a host cannot catch, reported against `hs_check_json` and
//! `hs_load` in the C API (see `crates/hollowscript-capi/src/lib.rs`) from
//! a caller thread as small as 512 KiB or 1024 KiB.
//!
//! `clippy.toml`'s allowances for `unwrap`/`expect`/`panic` in test code
//! cover a `#[cfg(test)] mod` inside a crate but not a `tests/*.rs`
//! integration test's own functions, so they are spelled out here too.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hollowscript::instance::{BuildOutcome, LoadOutcome, build, check, load};
use hollowscript::options::{CapabilityInterface, NoLoader, Options};
use hollowscript_core::cancel::Control;

/// A caller stack far smaller than any default (a release build's default
/// worker stack can be as small as 256 KiB), matching the smaller of the two
/// sizes the security review reproduced a crash with.
const SMALL_STACK: usize = 512 * 1024;

/// A program that immediately invokes a closure `depth` times, each level
/// wrapping the one before it as its own single argument: `(fn(x: Int):
/// Int { return x })((fn(x: Int): Int { return x })(...))`. Well within the
/// 250-level nesting limit of section 1.10, but each level is a full
/// closure body, which the checker gives its own frame, scope and type
/// inference state, heavier per level of recursion than the parser's own
/// walk of the same source.
fn nested_closure_calls(depth: usize) -> String {
    let mut inner = "1".to_owned();
    for _ in 0..depth {
        inner = format!("(fn(x: Int): Int {{ return x }})({inner})");
    }
    format!("return {inner}\n")
}

/// Runs `f` on a thread with [`SMALL_STACK`] of stack and returns its
/// answer, or fails the test if the thread could not be started or did not
/// return normally (a crash the join cannot distinguish from a panic, but
/// either is a failure this test must not let escape as a whole-process
/// abort).
fn on_small_stack<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(f)
        .expect("could not start the test's small-stack thread")
        .join()
        .expect("the entry point aborted (or panicked) on a small-stack caller")
}

#[test]
fn check_survives_a_deeply_nested_program_called_from_a_small_stack() {
    let source = nested_closure_calls(200);
    let diagnostics = on_small_stack(move || {
        check(
            &source,
            "main.hws",
            &CapabilityInterface::default(),
            None,
            &mut NoLoader,
            &mut Control::none(),
        )
    });
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn build_survives_a_deeply_nested_program_called_from_a_small_stack() {
    // Shallower than `check`'s own test: `compile` gives each of these
    // closures a register of its own, and 200 of them would legitimately
    // exceed the compiler's unrelated 256-register-per-function limit. This
    // depth still exercises the same recursion, well past where a small
    // caller stack used to crash the process before checking and compiling
    // moved onto their own dedicated thread.
    let source = nested_closure_calls(40);
    let outcome = on_small_stack(move || {
        build(
            &source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut Control::none(),
        )
    });
    assert!(matches!(outcome, BuildOutcome::Bytecode(_)), "{outcome:?}");
}

#[test]
fn load_survives_a_deeply_nested_program_called_from_a_small_stack() {
    // See `build`'s own test for why this is shallower than `check`'s.
    let source = nested_closure_calls(40);
    // `LoadOutcome::Instance` is not `Send` (it owns boxed host functions
    // the VM may call later), so the assertion runs inside the small-stack
    // thread itself, and only a plain `bool` crosses back to this one.
    let succeeded = on_small_stack(move || {
        let outcome = load(
            &source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        );
        matches!(outcome, LoadOutcome::Instance(_))
    });
    assert!(succeeded);
}
