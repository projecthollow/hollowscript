//! `format` must not exhaust its caller's stack (section 1.10: "a construct
//! within the limits MUST NOT exhaust the host's stack"), including on a
//! legal, diagnostic-free program at the structural weight limit of 1,000: a
//! long chain of binary operators is a left-leaning tree, so `format`'s
//! plain, recursive printer walks as many levels deep as the chain is long.
//!
//! This builds such a program and formats it from a thread with a stack far
//! smaller than a typical default, standing in for a constrained caller
//! (an embedder's own worker thread, or a WebAssembly host). Before `format`
//! ran its own walk on a dedicated, generously sized stack the way `parse`
//! already does, this reliably aborted the process with a stack overflow, a
//! failure a caller cannot catch: the test would not report a failure, the
//! whole test binary would crash.
//!
//! `clippy.toml`'s allowances for `unwrap`/`expect`/`panic` in test code
//! cover a `#[cfg(test)] mod` inside a crate but not a `tests/*.rs`
//! integration test's own functions, so they are spelled out here too.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hollowscript_core::limits::structural::{MAX_CHAIN_LINKS, MAX_TOKEN_WEIGHT};
use hollowscript_core::{Control, Interner, Source, SourceId};
use hollowscript_syntax::format;

/// A stack much smaller than any default (a release build's default worker
/// stack can be as small as 256 KiB; a debug build needs more, so this picks
/// a size that would already have failed the finding's own reproduction in
/// an unoptimised build while remaining a deliberately tight constraint).
const SMALL_STACK: usize = 2 * 1024 * 1024;

/// A source with one binary-operator chain at exactly the weight limit of
/// section 1.10: a chain of [`MAX_CHAIN_LINKS`] `*` operators (weight
/// [`MAX_CHAIN_LINKS`], as the heaviest operand of the next chain), used as
/// the first operand of a chain of `MAX_TOKEN_WEIGHT - MAX_CHAIN_LINKS` `+`
/// operators, for a total weight of exactly [`MAX_TOKEN_WEIGHT`] and zero
/// diagnostics: legal input the formatter must handle.
fn program_at_the_weight_limit() -> String {
    let outer_links = MAX_TOKEN_WEIGHT - MAX_CHAIN_LINKS;
    let mut expr = String::from("a");
    for _ in 0..MAX_CHAIN_LINKS {
        expr.push_str("*a");
    }
    for _ in 0..outer_links {
        expr.push_str("+a");
    }
    format!("constant a = 1\nconstant r = {expr}\nreturn r\n")
}

#[test]
fn formats_a_program_at_the_weight_limit_from_a_small_stack_without_aborting() {
    let source_text = program_at_the_weight_limit();

    // Confirmed a legal, diagnostic-free program before testing the stack
    // behaviour, so a failure below is about stack safety, not about this
    // test's own construction having drifted from the limit.
    let mut interner = Interner::new();
    let source = Source::new(SourceId(0), "weight_limit.hws", source_text.clone()).unwrap();
    let baseline = format(&source, &mut interner, &mut Control::none());
    assert!(
        baseline.diagnostics.is_empty(),
        "the constructed program is not diagnostic-free: {:?}",
        baseline.diagnostics
    );
    assert!(baseline.output.is_some());

    let handle = std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let mut interner = Interner::new();
            let source = Source::new(SourceId(0), "weight_limit.hws", source_text).unwrap();
            format(&source, &mut interner, &mut Control::none())
        })
        .expect("could not start the test's small-stack thread");

    let out = handle
        .join()
        .expect("format aborted (or panicked) on a small-stack caller");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(out.output.is_some());
}
