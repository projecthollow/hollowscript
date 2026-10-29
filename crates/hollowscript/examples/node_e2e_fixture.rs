//! Compiles `crates/hollowscript-wasm/web/node-e2e/sample.hws` to a bytecode
//! container, granting the capability interface that file's own JavaScript
//! host (`web/node-e2e/worker.mjs`) binds: `print`, `clock` (a namespace
//! with `now` and `today`) and `cache` (a plain function declared
//! `Result<String, String>`, section 8.2). `hollowscript-cli`'s own `build`
//! command cannot compile this source, because its capability interface is
//! fixed to the CLI's own capabilities (print, clock, random, env, args,
//! fs, stdin) and knows nothing of a host-defined `cache`; this example
//! exists only so `web/node-e2e/README.md`'s instructions have a way to
//! produce that fixture's bytecode without hand-writing a second CLI.
//!
//! Usage: `cargo run -p hollowscript --example node_e2e_fixture -- <source.hws> <out.hsb>`

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
use hollowscript_core::transfer::{TransferType, TypeTable, VariantDef};
use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let (Some(source_path), Some(out_path)) = (args.next(), args.next()) else {
        eprintln!("usage: node_e2e_fixture <source.hws> <out.hsb>");
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
    let string_ty = types.add(TransferType::String);
    let result_ty = types.add(TransferType::Enum {
        // The language's own `Result`, at its reserved id (see
        // `hollowscript_capi::result_type_of`, which does the same).
        id: hollowscript_core::transfer::EnumId {
            module: hollowscript_core::transfer::HOST_MODULE,
            index: hollowscript_core::transfer::RESULT_INDEX,
        },
        name: hollowscript::RESULT.to_owned(),
        variants: vec![
            VariantDef {
                name: hollowscript::OK.to_owned(),
                payload: vec![string_ty],
            },
            VariantDef {
                name: hollowscript::ERR.to_owned(),
                payload: vec![string_ty],
            },
        ],
    });
    let capabilities = CapabilityInterface {
        capabilities: vec![
            CapabilityDecl {
                name: "print".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![string_ty],
                    ret: None,
                },
            },
            CapabilityDecl {
                name: "clock".to_owned(),
                shape: CapabilityShape::Namespace(vec![
                    ("now".to_owned(), vec![], Some(string_ty)),
                    ("today".to_owned(), vec![], Some(string_ty)),
                ]),
            },
            CapabilityDecl {
                name: "cache".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![string_ty],
                    ret: Some(result_ty),
                },
            },
        ],
        types,
    };
    let options = Options {
        capabilities,
        ..Options::default()
    };

    match build(
        &source,
        "sample.hws",
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
