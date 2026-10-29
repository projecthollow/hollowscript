//! Repeatedly formats varied input in a plain (non-ASAN, non-libFuzzer)
//! release binary, so its resident memory can be watched with
//! `/usr/bin/time -l` (or the platform equivalent) over a long run, without
//! AddressSanitizer's own allocator in the way. Written to settle a
//! question `docs/design/fuzzing.md` records the answer to: whether the
//! `format_roundtrip` fuzz target's repeated out-of-memory aborts were a
//! real leak in `hollowscript-syntax` or an ASAN instrumentation artifact.
//! Kept here, not deleted once the question was answered, so the same
//! check can be rerun if the question ever needs asking again (a change to
//! `format` or the parser, a suspicious growth in a future fuzzing run).
//!
//! Not a fuzz target itself (no `#![no_main]`, no
//! `libfuzzer_sys::fuzz_target!`): an ordinary binary, like `gen_seeds`.
//!
//! Run with `cargo run --release --bin leak_check` from `fuzz/`.

use hollowscript_core::{Control, Interner, Source, SourceId};
use hollowscript_syntax::format;
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "hws") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push(text);
            }
        }
    }
}

fn main() {
    let mut sources = Vec::new();
    walk(Path::new("../corpus"), &mut sources);
    eprintln!("{} sources", sources.len());
    let mut state: u64 = 0x1234_5678;
    for i in 0..2_000_000u64 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let base = &sources[(state >> 33) as usize % sources.len()];
        // Corrupt one byte deterministically to mimic fuzzer-style mutation
        // without depending on the fuzz target's own mutator.
        let mut bytes = base.clone().into_bytes();
        if !bytes.is_empty() {
            let idx = (state >> 20) as usize % bytes.len();
            bytes[idx] = (state & 0xff) as u8;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let Ok(source) = Source::new(SourceId(0), "leak.hws", text) else {
            continue;
        };
        let mut interner = Interner::new();
        let _ = format(&source, &mut interner, &mut Control::none());
        if i % 200_000 == 0 {
            eprintln!("iteration {i}");
        }
    }
    eprintln!("done");
}
