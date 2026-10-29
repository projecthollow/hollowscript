//! Regenerates the seed corpus for the targets whose seeds this tool can
//! derive mechanically from `corpus/`, rather than by hand (`filter`'s own
//! seeds, for instance, are written by hand: see `docs/design/fuzzing.md`).
//! Two shapes, for two different kinds of boundary:
//!
//! - `bytecode_verify` and `wasm_abi` take raw bytecode bytes, exactly as a
//!   host that decodes a container it did not itself produce would present
//!   it, so their seeds are [`hollowscript::build`]'s container bytes for
//!   every corpus program that compiles cleanly, the same path the CLI's
//!   own `build` command uses.
//! - `capi` and `capi_ex` take HollowScript *source* text (they call
//!   `hs_check_json`/`hs_load`/`hs_load_ex` themselves, on a source string,
//!   not a pre-built container: see `fuzz_targets/capi.rs`'s own `split`),
//!   so seeding them with bytecode, as this tool once did, gave libFuzzer a
//!   corpus that could never parse as the source these targets actually
//!   read; their seeds are corpus source text instead, copied as is.
//!
//! Writes into each target's own `seeds/` directory (committed; `corpus/`
//! itself, where libFuzzer grows what it finds, is not).
//!
//! Run with `scripts/fuzz seeds`, or `cargo run --bin gen_seeds` from
//! `fuzz/`. Not a fuzz target itself (no `#![no_main]`, no
//! `libfuzzer_sys::fuzz_target!`): an ordinary binary, built and run like
//! any other, that only cargo-fuzz's own `[[bin]]` list in `Cargo.toml`
//! happens to also list.
//!
//! Worth rerunning whenever the bytecode format or the corpus itself
//! changes meaningfully, not only once: see `docs/design/fuzzing.md`.

use hollowscript::{BuildOutcome, Control, NoLoader, Options};
use std::path::Path;

fn main() {
    let corpus_dir = Path::new("../corpus");
    let bytecode_dirs = [Path::new("seeds/bytecode_verify"), Path::new("seeds/wasm_abi")];
    let source_dirs = [Path::new("seeds/capi"), Path::new("seeds/capi_ex")];
    for dir in bytecode_dirs.iter().chain(source_dirs.iter()) {
        std::fs::create_dir_all(dir).expect("could not create a seed corpus directory");
    }

    let mut bytecode_written = 0usize;
    let mut source_written = 0usize;
    walk(corpus_dir, &mut |path| {
        let Ok(source) = std::fs::read_to_string(path) else {
            return;
        };

        // `capi`/`capi_ex` split their input on the first NUL byte into a
        // source text and a call-arguments text (see `capi.rs`'s own
        // `split`); a seed with no NUL at all just gives both targets an
        // empty arguments string on that seed, which is a fine starting
        // point, so the corpus source is written as is, no NUL appended.
        let name = format!("{:016x}.hws", fnv1a(source.as_bytes()));
        for dir in source_dirs {
            std::fs::write(dir.join(&name), &source).expect("could not write a seed file");
        }
        source_written += 1;

        let outcome = hollowscript::build(
            &source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut Control::none(),
        );
        let BuildOutcome::Bytecode(bytes) = outcome else {
            // A corpus case is not expected to compile cleanly by design
            // (an error case, a fault case): not every seed source, only a
            // best-effort majority, which is all a seed corpus needs to be.
            return;
        };
        let name = format!("{:016x}.bin", fnv1a(&bytes));
        for dir in bytecode_dirs {
            std::fs::write(dir.join(&name), &bytes).expect("could not write a seed file");
        }
        bytecode_written += 1;
    });
    eprintln!(
        "wrote {bytecode_written} bytecode seeds to each of {bytecode_dirs:?}, {source_written} source seeds to each of {source_dirs:?}"
    );
}

/// FNV-1a: not cryptographic, only a stable, collision-unlikely filename for
/// each distinct container, so running this twice does not pile up
/// duplicate seeds under different names.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn walk(dir: &Path, on_hws_file: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, on_hws_file);
        } else if path.extension().is_some_and(|ext| ext == "hws") {
            on_hws_file(&path);
        }
    }
}
