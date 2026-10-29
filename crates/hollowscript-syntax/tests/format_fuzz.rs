//! A hand-rolled, deterministic fuzz test for `format`, checked only for one
//! property: it never panics, on any input, well formed or hostile.
//!
//! This reuses the parser's own fuzz corpus and mutation engine
//! (`support::mutate`, shared with `fuzz.rs`) rather than a separate one, on
//! the same reasoning `fuzz.rs` itself documents: the only source of
//! "randomness" is a fixed seed fed through a tiny linear congruential
//! generator, so a failure here is always reproducible.
//!
//! The round-trip contract itself (never loses a comment, never drops a
//! load-bearing parenthesis, is idempotent) is `format_roundtrip.rs`; this
//! file's only job is to keep `format` from ever panicking, including on
//! text that is not, and was never meant to be, a valid program.
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

use hollowscript_core::{Control, Diagnostic, Interner, Source, SourceId};
use hollowscript_syntax::format;
use support::{Lcg, mutate};

const SEED: u64 = 0xF0F0_FEED_u64;

/// Runs `format` under `catch_unwind`, asserting only that it does not
/// panic and that a refusal always carries at least one error diagnostic
/// (never `output: None` with no diagnostic explaining why, and never
/// `output: Some(_)` alongside an error).
fn attempt(source: String, label: &str) {
    let preview: String = source.chars().take(200).collect();
    let outcome = std::panic::catch_unwind(|| {
        let Ok(src) = Source::new(SourceId(0), "fuzz.hws", source) else {
            return;
        };
        let mut interner = Interner::new();
        let out = format(&src, &mut interner, &mut Control::none());
        let refused = out.diagnostics.iter().any(Diagnostic::is_error);
        assert_eq!(
            out.output.is_none(),
            refused,
            "{label}: `output` and the diagnostics disagree about whether formatting succeeded, for {preview:?}"
        );
    });
    if let Err(payload) = outcome {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        panic!("{label}: format panicked on {preview:?}: {message}");
    }
}

#[test]
fn every_corpus_file_formats_without_panicking() {
    let root = support::corpus_root();
    if !root.exists() {
        eprintln!("skipping: corpus not found at {}", root.display());
        return;
    }
    let files = support::walk_hws(&root);
    assert!(
        !files.is_empty(),
        "expected .hws files under {}",
        root.display()
    );
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        attempt(text, &path.display().to_string());
    }
}

#[test]
fn mutated_corpus_programs_never_make_format_panic() {
    let root = support::corpus_root();
    if !root.exists() {
        eprintln!("skipping: corpus not found at {}", root.display());
        return;
    }
    let files = support::walk_hws(&root);
    let sources: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    assert!(!sources.is_empty(), "expected corpus sources to fuzz from");

    let mut rng = Lcg::new(SEED);
    for i in 0..8_000 {
        let base = &sources[rng.below(sources.len())];
        let mut text = base.clone();
        for _ in 0..rng.count(4) {
            text = mutate(&text, &mut rng);
        }
        attempt(text, &format!("mutated[{i}]"));
    }
}

/// Code points and short snippets chosen the same way `fuzz.rs` chooses
/// them: delimiters, escape-adjacent characters and a few keywords, so a
/// generated string lands on a grammar boundary more often than uniformly
/// random bytes would.
const ALPHABET_CHARS: &[char] = &[
    'a', 'b', 'c', 'x', 'y', 'z', '0', '1', '9', '_', '+', '-', '*', '/', '<', '>', '=', '!', '.',
    ',', ':', ';', '(', ')', '{', '}', '[', ']', '"', '\'', '`', '$', '?', '\\', ' ', '\t', '\n',
    '\r',
];
const ALPHABET_WORDS: &[&str] = &[
    "and", "if", "else", "fn", "match", "none", "not", "return", "//", "// note:", "${", "}",
    "effect", "constant",
];

fn random_text(rng: &mut Lcg, max_len: usize) -> String {
    let mut text = String::new();
    let length = rng.below(max_len);
    for _ in 0..length {
        if rng.one_in(4) {
            text.push_str(ALPHABET_WORDS[rng.below(ALPHABET_WORDS.len())]);
        } else if rng.one_in(20) {
            let code = u32::try_from(rng.below(0x1_0000)).unwrap_or(0);
            if let Some(c) = char::from_u32(code) {
                text.push(c);
            }
        } else {
            text.push(ALPHABET_CHARS[rng.below(ALPHABET_CHARS.len())]);
        }
    }
    text
}

#[test]
fn random_and_hostile_text_never_makes_format_panic() {
    let mut rng = Lcg::new(SEED ^ 0xA5A5_A5A5);
    for i in 0..3_000 {
        let text = random_text(&mut rng, 160);
        attempt(text, &format!("random[{i}]"));
    }

    // A handful of specific shapes worth pinning by hand: the empty file, a
    // comment with nothing else, only a byte order mark (a lone surrogate,
    // by contrast, cannot occur here at all: `Source` guarantees valid
    // UTF-8, which excludes one structurally, not just by convention), and
    // several ways of running well past every structural limit of section
    // 1.10 at once.
    let hand_picked = [
        String::new(),
        "\n".to_string(),
        "//".to_string(),
        "// ".to_string(),
        "\r\n// x\r\n".to_string(),
        "// note:".to_string(),
        " // x".to_string(),
        "\u{feff}".to_string(),
        "\u{feff}\u{feff}".to_string(),
        "(".repeat(2_000),
        "[".repeat(2_000),
        format!("constant x = {}1{}", "(".repeat(500), ")".repeat(500)),
        format!("if {}1{} {{ }}", "{ a: ".repeat(300), " }".repeat(300)),
        "\"${".repeat(300),
        "fn f() {".repeat(300),
        "// c\n".repeat(5_000),
        "x".repeat(200_000),
        "/".repeat(10_001),
        format!("`{}", "\\".repeat(999)),
        "a + b + c + d + e + f + g + h + i + j + k + l + m".to_string(),
    ];
    for (i, text) in hand_picked.into_iter().enumerate() {
        attempt(text, &format!("hand_picked[{i}]"));
    }
}
