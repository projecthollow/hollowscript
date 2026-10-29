//! Compiles `crates/hollowscript-wasm/web/node-e2e/async-sample.hws` to a
//! bytecode container, granting the `tick` capability (a plain function,
//! `Int -> Int`) that file's own JavaScript host
//! (`web/node-e2e/async-check.mjs`) answers asynchronously, on the batched
//! suspend/resume path. See `node_e2e_fixture.rs` for why this cannot be
//! `hollowscript-cli`'s own `build` command.
//!
//! Usage: `cargo run -p hollowscript --example async_e2e_fixture -- <source.hws> <out.hsb>`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line fixture builder reports to the terminal by design; \
              the workspace's no-stray-output rule is about the language \
              implementation, not this dev-only tool"
)]

use hollowscript::{
    BuildOutcome, CapabilityDecl, CapabilityInterface, CapabilityShape, Control, NoLoader, Options,
    build,
};
use hollowscript_core::transfer::{TransferType, TypeTable};
use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let (Some(source_path), Some(out_path)) = (args.next(), args.next()) else {
        eprintln!("usage: async_e2e_fixture <source.hws> <out.hsb>");
        return ExitCode::FAILURE;
    };
    let source = match fs::read_to_string(&source_path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("reading {source_path}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut types = TypeTable::new();
    let int_ty = types.add(TransferType::Int);
    let capabilities = CapabilityInterface {
        capabilities: vec![CapabilityDecl {
            name: "tick".to_owned(),
            shape: CapabilityShape::Function {
                params: vec![int_ty],
                ret: Some(int_ty),
            },
        }],
        types,
    };
    let options = Options {
        capabilities,
        ..Options::default()
    };

    match build(
        &source,
        "async-sample.hws",
        &options,
        &mut NoLoader,
        &mut Control::none(),
    ) {
        BuildOutcome::Bytecode(bytes) => {
            let len = bytes.len();
            if let Err(error) = fs::write(&out_path, bytes) {
                eprintln!("writing {out_path}: {error}");
                return ExitCode::FAILURE;
            }
            println!("wrote {out_path} ({len} bytes)");
            ExitCode::SUCCESS
        }
        BuildOutcome::CheckFailed(diagnostics) => {
            for diagnostic in diagnostics {
                eprintln!("{}: {}", diagnostic.code.to_text(), diagnostic.message);
            }
            ExitCode::FAILURE
        }
    }
}
