//! The whole pipeline over arbitrary bytes: check, compile, verify, load,
//! run, then call every export that takes no arguments and read every
//! exported constant.
//!
//! `crates/hollowscript/tests/fuzz_pipeline.rs` already checks this
//! deterministically over a seeded mutation of the corpus; this target
//! complements it with libFuzzer's coverage-guided search, starting from
//! the same seed corpus so the two overlap in what they cover rather than
//! starting from nothing.
//!
//! Limits are kept small so every input terminates quickly: a mutation that
//! tries to loop forever must reach the step limit (HS1001), not
//! libFuzzer's own timeout, and a slow input is exactly the kind of thing
//! that should shrink to a minimal repro rather than time the whole target
//! out.
//!
//! Two properties, exactly as the deterministic test: nothing panics at any
//! stage, and a program the checker accepts must reach an instance (a
//! `CheckFailed` after a clean check is the compiler or the verifier
//! refusing something the checker already accepted, which is always a
//! defect in one of them).

#![no_main]

use hollowscript::{Control, Diagnostic, Instance, Limits, LoadOutcome, NoLoader, Options};
use libfuzzer_sys::fuzz_target;

fn limits() -> Limits {
    Limits {
        max_steps: 200_000,
        max_memory_bytes: 4_000_000,
        max_output_bytes: 100_000,
        max_call_depth: 64,
    }
}

fn exercise(instance: &mut Instance) {
    for export in instance.exports() {
        let name = export.name;
        if export.arity == Some(0) {
            let _ = instance.call(&name, &[], &mut Control::none());
        }
        let _ = instance.read(&name);
    }
}

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data).into_owned();
    let options = Options {
        limits: limits(),
        // `HOLLOWSCRIPT_GC_STRESS=1 scripts/fuzz run pipeline <seconds>`
        // forces a collection at every safepoint the mid-run collector
        // reaches, for the same reason `hollowscript-conformance` reads
        // this variable (see its `gc_stress_enabled`): libFuzzer's
        // coverage-guided search covers shapes the seeded, deterministic
        // `crates/hollowscript/tests/fuzz_pipeline.rs` does not.
        gc_stress_mode: std::env::var_os("HOLLOWSCRIPT_GC_STRESS").is_some(),
        ..Options::default()
    };
    let diagnostics = hollowscript::check(
        &source,
        "fuzz.hws",
        &options.capabilities,
        // No filter schema: this target does not exercise Appendix A's
        // schema validation (section 9.5), only the rest of the pipeline.
        None,
        &mut NoLoader,
        &mut Control::none(),
    );
    let clean = !diagnostics.iter().any(Diagnostic::is_error);
    let outcome = hollowscript::load(
        &source,
        "fuzz.hws",
        &options,
        &mut NoLoader,
        &mut |_, _| None,
        &mut Control::none(),
    );
    match outcome {
        LoadOutcome::CheckFailed(found) => {
            assert!(
                !clean,
                "the checker accepted this program and loading refused it: {:?}",
                found.iter().map(|d| d.code.to_string()).collect::<Vec<_>>()
            );
        }
        LoadOutcome::Instance(mut instance) => {
            exercise(&mut instance);
            let _ = instance.dispose();
        }
    }
});
