//! A hand-rolled, deterministic fuzz test: character- and token-level
//! mutation and truncation of every program in the conformance corpus,
//! checked only for one property: the parser never panics. The mutation
//! engine (`support::mutate` and its seeded generator) is shared with the
//! formatter's own fuzz test (`format_fuzz.rs`), which reuses this same
//! corpus and mutation code against `format` instead of `parse`.
//!
//! `clippy.toml` allows `unwrap`/`expect`/`panic`/indexing in test code;
//! that detection covers a `#[cfg(test)] mod` inside a crate but not a
//! `tests/*.rs` integration test's own helper functions, so it is spelled
//! out explicitly here for the same, already-stated policy.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use hollowscript_core::{Control, Interner, Source, SourceId};
use hollowscript_syntax::parse;
use support::{Lcg, mutate};

/// The number of mutated programs this test parses. "At least ten thousand"
/// with headroom.
const ITERATIONS: usize = 12_000;

/// A fixed seed: the whole run is reproducible from this one number.
const SEED: u64 = 0x5EED_C0FFEE_u64;

#[test]
fn fuzz_mutated_and_truncated_corpus_never_panics() {
    let root = support::corpus_root();
    if !root.exists() {
        eprintln!("skipping: corpus not found at {}", root.display());
        return;
    }
    let files = support::walk_hws(&root);
    let sources: Vec<(std::path::PathBuf, String)> = files
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect();
    assert!(!sources.is_empty(), "expected corpus sources to fuzz from");

    let mut rng = Lcg::new(SEED);
    let mut failures: Vec<String> = Vec::new();
    let mut ran = 0usize;

    for i in 0..ITERATIONS {
        let (path, base) = &sources[rng.below(sources.len())];
        let mutated = mutate(base, &mut rng);
        ran += 1;

        let attempt = mutated.clone();
        let outcome = std::panic::catch_unwind(|| {
            let Ok(source) = Source::new(SourceId(0), "fuzz.hws", attempt) else {
                return;
            };
            let mut interner = Interner::new();
            let out = parse(&source, &mut interner, &mut Control::none());
            // Touch the result so the optimiser cannot discard the work,
            // and so a panic deep in tree construction is not hidden by
            // dead-code elimination in a release-mode run of this test.
            std::hint::black_box(out.file.node_count);
            std::hint::black_box(out.diagnostics.len());
        });

        if outcome.is_err() {
            let preview: String = mutated.chars().take(160).collect();
            failures.push(format!(
                "iteration {i} from {}: parser panicked on:\n{preview:?}",
                path.display()
            ));
        }
    }

    assert_eq!(ran, ITERATIONS);
    assert!(
        failures.is_empty(),
        "{} of {ITERATIONS} mutated inputs panicked (seed {SEED:#x}):\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

#[test]
fn the_generator_is_deterministic() {
    let mut a = Lcg::new(SEED);
    let mut b = Lcg::new(SEED);
    let seq_a: Vec<u64> = (0..50).map(|_| a.next_u64()).collect();
    let seq_b: Vec<u64> = (0..50).map(|_| b.next_u64()).collect();
    assert_eq!(seq_a, seq_b);
    // Sanity: it is not a constant function.
    assert!(seq_a.windows(2).any(|w| w[0] != w[1]));
}
