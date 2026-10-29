//! Runs one program with raised `maxSteps` and `maxMemoryBytes`, for
//! `xtask mem`'s own use.
//!
//! `hollowscript-cli` always runs with section 11's default limits (there is
//! no flag to raise them), so it cannot run a shape whose charged figure is
//! above the default 64 MiB `maxMemoryBytes`, such as
//! `docs/design/security.md`'s 1,000-character-string append (102 MB
//! charged). This talks to the `hollowscript` facade directly, the same way
//! `xtask/src/bench.rs` does, so `xtask mem` can measure a shape's real peak
//! memory (by running this binary under `/usr/bin/time -l`) without being
//! limited to what fits under the defaults.
//!
//! Usage: `cargo run --release --example mem_shape -- <source-file>`

#![allow(
    clippy::print_stderr,
    reason = "a command-line measurement harness reports to the terminal by design; the \
              workspace's no-stray-output rule is about the language implementation, not \
              this dev-only tool"
)]

use hollowscript::{Control, Limits, LoadOutcome, NoLoader, Options};
use std::env;
use std::fs;
use std::process::ExitCode;

/// Far above anything a measurement shape needs charged, so the run is
/// bounded only by the shape itself, not by this harness.
const RAISED_LIMITS: Limits = Limits {
    // The largest value section 11.1 allows any limit to be set to
    // (2^63 - 1), so the run is bounded only by the shape itself.
    max_steps: i64::MAX as u64,
    max_memory_bytes: i64::MAX as u64,
    max_output_bytes: 16_777_216,
    max_call_depth: 1_000,
};

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: mem_shape <source-file>");
        return ExitCode::FAILURE;
    };
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("reading {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let options = Options {
        limits: RAISED_LIMITS,
        ..Options::default()
    };
    let outcome = hollowscript::load(
        &source,
        "mem_shape.hws",
        &options,
        &mut NoLoader,
        &mut |_, _| None,
        &mut Control::none(),
    );
    match outcome {
        LoadOutcome::CheckFailed(diagnostics) => {
            eprintln!("check failed: {diagnostics:?}");
            ExitCode::FAILURE
        }
        LoadOutcome::Instance(mut instance) => {
            let _ = instance.dispose();
            ExitCode::SUCCESS
        }
    }
}
