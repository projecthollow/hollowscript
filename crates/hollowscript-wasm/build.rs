//! Build script: gives the WebAssembly module its memory contract and sizes
//! the stack of the `tooling` build.
//!
//! Both jobs are link arguments for the `cdylib`, they apply on `wasm32` only,
//! and they are added side by side: one never replaces the other. They are set
//! here and not in `.cargo/config.toml` because a build script's link
//! arguments follow the crate wherever it is built from, while a
//! `RUSTFLAGS` environment variable (the release build sets one) replaces the
//! configuration's `rustflags` silently, which would drop them.
//!
//! # Memory
//!
//! The module imports its linear memory (`env.memory`) instead of defining its
//! own, and declares a maximum of 256 MiB (4,096 pages) for it. A WebAssembly
//! memory cannot grow past the maximum its module declares, and a host cannot
//! supply a memory with a larger one, so a program that allocates for real
//! (not only in the language's own accounting, which is not a bound on the
//! machine's memory) is stopped at the ceiling, and a host that wants a lower
//! one creates the memory with a lower maximum. A build for a host that needs
//! a different ceiling sets `HOLLOWSCRIPT_WASM_MAX_MEMORY_MIB` to a whole
//! number of MiB (4,096 at most, the 4 GiB a 32-bit memory can address; 2 at
//! least, or [`TOOLING_LEAST_MIB`] for the `tooling` build, see below) when it
//! builds.
//!
//! # Stack
//!
//! The front end (parsing, checking, compiling and formatting) recurses once
//! per level of nesting a program uses, up to the structural limits of
//! section 1.10, and section 1.10 requires that a program within them "MUST
//! NOT exhaust the host's stack". Every other target gets that by running the
//! front end on a dedicated thread with a 16 MiB stack
//! (`hollowscript_core::run_on_dedicated_stack`), but `wasm32-unknown-unknown`
//! has no threads: the front end runs on the one stack the module has, the
//! shadow stack in its linear memory, which the linker sizes at 1 MiB unless
//! told otherwise. A legal program of 235 nested closures needed more than
//! that and trapped the module. So the `tooling` build is linked with a stack
//! of [`TOOLING_STACK_BYTES`], which `docs/design/security.md` records
//! together with the nesting depth that was measured to fit in it. The plain
//! build, which holds only the virtual machine and runs script calls on an
//! explicit frame stack, keeps the default.
//!
//! # How the two meet
//!
//! The shadow stack lives in the linear memory, so with an imported memory the
//! `tooling` module declares an initial size that holds the stack (a little
//! over 16 MiB) and the host has to create the memory with at least that, and
//! the maximum cannot be below it, which is why a ceiling set for that build
//! has a higher least. The
//! module's own declared minimum and maximum are what a host reads, so a host
//! that follows them gets this right for both builds.

use std::process::ExitCode;

/// The module's default ceiling, in MiB. `web/README.md` and
/// `docs/design/security.md` state the same figure, and
/// `web/node-e2e/memory-check.mjs` checks the built module against it.
const DEFAULT_MAX_MEMORY_MIB: u64 = 256;

/// The environment variable that changes it for one build.
const OVERRIDE: &str = "HOLLOWSCRIPT_WASM_MAX_MEMORY_MIB";

/// The size, in bytes, of the shadow stack of the `tooling` build: 16 MiB, the
/// size of the dedicated thread every other target gives the front end.
const TOOLING_STACK_BYTES: u32 = 16 * 1024 * 1024;

/// The least ceiling, in MiB, the `tooling` build can be given: its stack, in
/// whole MiB, and one more for its static data (the linker would otherwise
/// refuse with "maximum memory too small").
const TOOLING_LEAST_MIB: u64 = TOOLING_STACK_BYTES as u64 / (1024 * 1024) + 1;

#[allow(
    clippy::print_stdout,
    reason = "a build script talks to cargo by printing directives to standard output"
)]
fn main() -> ExitCode {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={OVERRIDE}");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TOOLING");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("wasm32") {
        return ExitCode::SUCCESS;
    }
    let tooling = std::env::var_os("CARGO_FEATURE_TOOLING").is_some();
    let least = if tooling { TOOLING_LEAST_MIB } else { 2 };
    let mib = match std::env::var(OVERRIDE) {
        Err(_) => DEFAULT_MAX_MEMORY_MIB,
        Ok(text) => match text.parse::<u64>() {
            Ok(mib) if (least..=4096).contains(&mib) => mib,
            _ => {
                println!(
                    "cargo:warning={OVERRIDE} must be a whole number of MiB from {least} to 4096 for this build"
                );
                return ExitCode::FAILURE;
            }
        },
    };
    println!("cargo:rustc-link-arg-cdylib=--import-memory");
    println!(
        "cargo:rustc-link-arg-cdylib=--max-memory={}",
        mib * 1024 * 1024
    );
    if tooling {
        println!("cargo:rustc-link-arg-cdylib=-zstack-size={TOOLING_STACK_BYTES}");
    }
    ExitCode::SUCCESS
}
