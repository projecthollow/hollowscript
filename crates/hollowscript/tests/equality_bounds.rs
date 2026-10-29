//! Comparing values must cost what its charge says, however the two values
//! share structure (section 11.3 and section 11.4, "deep values"): a value
//! built by repeated doubling has one object per level but 2^levels paths
//! through it, and a comparison that walked every path would run for ever
//! while charging for a few dozen nodes. These tests run whole programs, on
//! a thread with a deadline, because the failure they guard against is a
//! hang rather than a wrong answer.
//!
//! `clippy.toml` allows `unwrap`/`expect`/`panic` in test code, but that
//! covers a `#[cfg(test)] mod` inside a crate and not a `tests/*.rs`
//! integration test, so the same policy is spelled out here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hollowscript::{Control, LoadOutcome, LoadResult, NoLoader, Options};
use hollowscript_core::Limits;
use std::sync::mpsc;
use std::time::Duration;

/// How long a comparison that is meant to be cheap may take before the test
/// calls it a hang. The longest of these takes a few milliseconds when it is
/// right and, before it was fixed, longer than anyone would wait.
const DEADLINE: Duration = Duration::from_secs(30);

/// Loads `source` under `limits` on its own thread and returns what the
/// top-level `return` produced rendered as text, or the fault's code as
/// text. Fails the test if the thread has not answered by [`DEADLINE`].
fn outcome(source: &str, limits: Limits) -> String {
    let source = source.to_owned();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let options = Options {
            limits,
            ..Options::default()
        };
        let loaded = hollowscript::load(
            &source,
            "main.hws",
            &options,
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        );
        let text = match loaded {
            LoadOutcome::Instance(mut instance) => match instance.result() {
                Some(LoadResult::Value(value)) => instance.render(value),
                _ => match instance.fault() {
                    Some(fault) => format!("{:?}", fault.code),
                    None => "no value and no fault".to_owned(),
                },
            },
            LoadOutcome::CheckFailed(found) => format!("{found:?}"),
        };
        let _ = sender.send(text);
    });
    receiver
        .recv_timeout(DEADLINE)
        .expect("the program did not finish: a comparison is doing work its charge does not cover")
}

/// A program that builds two separate values by the same doubling and then
/// evaluates `tail` over them as `a` and `b`.
fn doubled(levels: u32, tail: &str) -> String {
    format!(
        "variable a = Json.Int(1)\n\
         variable b = Json.Int(1)\n\
         for i in range(0, {levels}) {{\n\
         \x20   a = Json.Array([a, a])\n\
         \x20   b = Json.Array([b, b])\n\
         }}\n\
         {tail}\n"
    )
}

#[test]
fn equality_of_two_doubled_values_finishes_and_is_true() {
    // 2^36 paths: about as long as the whole run's patience, before the fix.
    let source = doubled(36, "return a == b");
    assert_eq!(outcome(&source, Limits::default()), "Bool(true)");
}

#[test]
fn inequality_of_two_doubled_values_finishes_and_is_false() {
    let source = doubled(36, "return a != b");
    assert_eq!(outcome(&source, Limits::default()), "Bool(false)");
}

#[test]
fn doubled_values_that_differ_at_the_bottom_are_not_equal() {
    let source = "variable a = Json.Int(1)\n\
        variable b = Json.Int(2)\n\
        for i in range(0, 40) {\n\
        \x20   a = Json.Array([a, a])\n\
        \x20   b = Json.Array([b, b])\n\
        }\n\
        return a == b\n";
    assert_eq!(outcome(source, Limits::default()), "Bool(false)");
}

#[test]
fn contains_and_index_of_over_doubled_values_finish() {
    let source = doubled(
        45,
        "return [[a].contains(b), [Json.Null, a].indexOf(b) == 1]",
    );
    assert_eq!(
        outcome(&source, Limits::default()),
        "[Bool(true), Bool(true)]"
    );
}

#[test]
fn ordering_two_long_strings_is_charged_like_every_other_comparison() {
    // Section 11.3: `<` costs 1 + floor(m / 64) steps for two strings, where
    // m is the smaller of their tree sizes, here about a million, so each
    // comparison below is about 15,000 steps. Two hundred of them are three
    // million steps, far over a limit that two hundred uncharged comparisons
    // would sit well inside.
    let source = "constant s = \"x\".repeat(1000000)\n\
        constant t = \"y\".repeat(1000000)\n\
        variable n = 0\n\
        for i in range(0, 200) {\n\
        \x20   if s < t {\n\
        \x20       n = n + 1\n\
        \x20   }\n\
        }\n\
        return n\n";
    let limits = Limits {
        max_steps: 100_000,
        ..Limits::default()
    };
    assert_eq!(outcome(source, limits), "StepLimit");
}

#[test]
fn sorting_a_list_that_holds_one_long_string_many_times_does_not_read_it_each_time() {
    // 200,000 handles to one 32 MiB string. Every comparison that read the
    // string would read 32 MiB, and sorting a list that long makes at least
    // 200,000 of them: more than six terabytes.
    let source = "constant s = \"x\".repeat(33554432)\n\
        constant items = range(0, 200000).map(fn(i: Int): String { return s })\n\
        return items.sort().length()\n";
    let limits = Limits {
        max_memory_bytes: 1 << 30,
        ..Limits::default()
    };
    assert_eq!(outcome(source, limits), "Int(200000)");
}
