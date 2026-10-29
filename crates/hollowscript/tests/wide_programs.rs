//! Programs wider than one byte of an instruction reaches, through the whole
//! pipeline: section 1.10 bounds the number of elements, arguments, fields
//! and statements of one construct only by the size of a source file, so
//! none of them may stop at 256 registers or at the reach of a short jump.
//! The corpus (`corpus/widths`) pins what each program does; this file
//! pins what the corpus cannot: what a host sees of an export this wide,
//! and the one limit left, which is this implementation's and not the
//! language's.
//!
//! `clippy.toml` allows `unwrap`/`expect`/`panic`/indexing in test code, but
//! that covers a `#[cfg(test)] mod` inside a crate and not a `tests/*.rs`
//! integration test, so the same policy is spelled out here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use hollowscript::{
    CallOutcome, Code, Control, Diagnostic, Instance, LoadOutcome, LoadResult, NoLoader, Options,
    Value,
};

fn load(source: &str) -> LoadOutcome {
    hollowscript::load(
        source,
        "main.hws",
        &Options::default(),
        &mut NoLoader,
        &mut |_, _| None,
        &mut Control::none(),
    )
}

fn instance(source: &str) -> Instance {
    match load(source) {
        LoadOutcome::Instance(instance) => *instance,
        LoadOutcome::CheckFailed(found) => panic!("{found:?}"),
    }
}

fn result(source: &str) -> String {
    let mut instance = instance(source);
    match instance.result() {
        Some(LoadResult::Value(value)) => instance.render(value),
        other => panic!("no value: {other:?} {:?}", instance.fault()),
    }
}

/// An exported function of 300 parameters: the host's arguments arrive in
/// registers past the first 256, and the entry point that calls the
/// function moves every one of them into place.
#[test]
fn a_host_calls_an_export_of_300_parameters() {
    let params: Vec<String> = (0..300).map(|i| format!("a{i}: Int")).collect();
    let source = format!(
        "export fn pick({}): Int {{\n    return a0 * 1000000 + a255 * 1000 + a299\n}}\n",
        params.join(", ")
    );
    let mut instance = instance(&source);
    let export = instance
        .exports()
        .into_iter()
        .find(|e| e.name == "pick")
        .expect("pick is exported");
    assert_eq!(export.arity, Some(300));
    let args: Vec<Value> = (0..300).map(|i| Value::Int(i + 1)).collect();
    match instance.call("pick", &args, &mut Control::none()) {
        Ok(CallOutcome::Returned(Some(value))) => {
            assert_eq!(instance.render(value), "Int(1256300)");
        }
        other => panic!("{other:?}"),
    }
}

/// A binary chain, however long, needs two temporaries: the value so far
/// and the next operand. Every intermediate register is free again once
/// its operator has read it.
#[test]
fn a_chain_of_additions_does_not_grow_the_frame() {
    let chain = |n: usize| -> String {
        let terms: Vec<String> = (0..=n).map(|i| i.to_string()).collect();
        format!("return {}\n", terms.join(" + "))
    };
    let frame = |source: &str| {
        let instance = instance(source);
        instance.module().functions.last().unwrap().register_count
    };
    assert_eq!(frame(&chain(2)), frame(&chain(500)));
    assert_eq!(result(&chain(500)), "Int(125250)");
}

/// A list literal of any length needs one chunk of registers for its
/// elements, not one register per element.
#[test]
fn a_long_list_literal_needs_one_chunk_of_registers() {
    let list = |n: usize| -> String {
        let items: Vec<String> = (0..n).map(|i| (i % 7).to_string()).collect();
        format!(
            "constant xs = [{}]\n\nreturn xs.length()\n",
            items.join(", ")
        )
    };
    let frame = |source: &str| {
        let instance = instance(source);
        instance.module().functions.last().unwrap().register_count
    };
    assert!(frame(&list(20_000)) < 100, "{}", frame(&list(20_000)));
    assert_eq!(result(&list(20_000)), "Int(20000)");
}

/// The one width limit this implementation keeps: every register one
/// function needs at once has to fit in a frame of 65,535. Section 1.10
/// allows an implementation such a limit, and reports exceeding it as
/// HS0901, among the diagnostics of checking, located at the construct that
/// went over, and never as an internal error. An interpolation holds all its parts
/// at once, a text segment around every value, and computes each value in
/// one more register before moving it into place, so `n` values need
/// `2n + 2` registers: 32,766 fit and 32,767 do not.
#[test]
fn a_function_wider_than_a_frame_is_a_located_error() {
    let interpolation = |values: usize| -> String { "${0}".repeat(values) };
    let source = format!(
        "constant ok = 1\n\nconstant s = \"{}\"\n\nreturn s\n",
        interpolation(32_767)
    );
    let found = match load(&source) {
        LoadOutcome::CheckFailed(found) => found,
        LoadOutcome::Instance(_) => panic!("a frame of 65,536 registers must be refused"),
    };
    let errors: Vec<&Diagnostic> = found.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{found:?}");
    let error = errors[0];
    assert_eq!(error.code, Code::HS0901);
    assert!(error.message.contains("65535 values"), "{}", error.message);
    let location = error.location.as_ref().expect("located");
    assert_eq!((&*location.file, location.line), ("main.hws", 3));
    // `check` reports it too, with the same location: it is a diagnostic of
    // checking, not something only loading finds.
    let checked = hollowscript::check(
        &source,
        "main.hws",
        &Options::default().capabilities,
        None,
        &mut NoLoader,
        &mut Control::none(),
    );
    let codes: Vec<(Code, u32)> = checked
        .iter()
        .filter(|d| d.is_error())
        .map(|d| (d.code, d.location.as_ref().map_or(0, |l| l.line)))
        .collect();
    assert_eq!(codes, vec![(Code::HS0901, 3)]);

    let source = format!(
        "constant s = \"{}\"\n\nreturn s.length()\n",
        interpolation(32_766)
    );
    assert_eq!(result(&source), "Int(32766)");
}
