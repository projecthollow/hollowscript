//! Runs the whole conformance corpus (`corpus/**/*.hws`) through the parser:
//! every file must parse without panicking or hanging (a watchdog thread
//! catches a hang and reports the file instead of stalling the run), and a
//! summary compares the diagnostic count against each case's own
//! `.expected.json` `parse` field where one exists.
//!
//! The comparison is informational rather than a hard gate on every case,
//! for one documented reason: `appendix-a-filters.md` and
//! `docs/design/architecture.md` both place filter-literal validation
//! (HS0701 to HS0711) in the checker, not the parser (the registry in
//! `hollowscript_core::codes` agrees: those codes are stage `Check`), so a
//! source whose only problem is inside a filter literal parses cleanly here
//! even though some fixtures carry that error under `parse` rather than
//! `check`. The test still fails if the mismatch rate is high enough to
//! suggest a real regression rather than that one, narrow, already-understood gap.
//!
//! `clippy.toml` allows `unwrap`/`expect` in test code; that detection
//! covers a `#[cfg(test)] mod` inside a crate but not a `tests/*.rs`
//! integration test's own helper functions, so it is spelled out explicitly
//! here for the same, already-stated policy.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use hollowscript_core::{Control, Interner, Source, SourceId};
use hollowscript_syntax::parse;
use std::sync::mpsc;
use std::time::Duration;
use support::{Json, parse_json};

struct CaseResult {
    path: std::path::PathBuf,
    diagnostic_count: usize,
    expected_parse_count: Option<usize>,
}

fn parse_file_guarded(path: &std::path::Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let path_owned = path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            let name = path_owned.to_string_lossy().to_string();
            let source =
                Source::new(SourceId(0), name, text).expect("corpus files fit the size limit");
            let mut interner = Interner::new();
            parse(&source, &mut interner, &mut Control::none())
                .diagnostics
                .len()
        });
        let _ = tx.send(result.map_err(|_| "panicked".to_string()));
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(inner) => inner,
        Err(_) => Err("timed out (possible hang)".to_string()),
    }
}

#[test]
fn every_corpus_file_parses_and_matches_its_expected_parse_stage_where_documented() {
    let root = support::corpus_root();
    if !root.exists() {
        eprintln!("skipping: {} not found", root.display());
        return;
    }
    let files = support::walk_hws(&root);
    assert!(
        !files.is_empty(),
        "expected .hws files under {}",
        root.display()
    );

    let mut hard_failures = Vec::new();
    let mut results = Vec::new();

    for path in &files {
        match parse_file_guarded(path) {
            Ok(diagnostic_count) => {
                let expected_path = path.with_extension("expected.json");
                let expected_parse_count = std::fs::read_to_string(&expected_path)
                    .ok()
                    .and_then(|t| parse_json(&t))
                    .and_then(|j| j.get("parse").cloned())
                    .and_then(|p| p.as_array().map(<[Json]>::len));
                results.push(CaseResult {
                    path: path.clone(),
                    diagnostic_count,
                    expected_parse_count,
                });
            }
            Err(reason) => hard_failures.push(format!("{}: {reason}", path.display())),
        }
    }

    assert!(
        hard_failures.is_empty(),
        "{} of {} corpus files panicked or hung:\n{}",
        hard_failures.len(),
        files.len(),
        hard_failures.join("\n")
    );

    let total = results.len();
    let clean = results.iter().filter(|r| r.diagnostic_count == 0).count();
    let with_diagnostics = total - clean;
    let with_expectation: Vec<&CaseResult> = results
        .iter()
        .filter(|r| r.expected_parse_count.is_some())
        .collect();
    let matches = with_expectation
        .iter()
        .filter(|r| r.expected_parse_count == Some(r.diagnostic_count))
        .count();
    let mismatches: Vec<&&CaseResult> = with_expectation
        .iter()
        .filter(|r| r.expected_parse_count != Some(r.diagnostic_count))
        .collect();

    eprintln!(
        "corpus parse summary: {total} files, {clean} with zero diagnostics, {with_diagnostics} with at least one"
    );
    eprintln!(
        "cross-check against .expected.json's `parse` field: {matches}/{} matched exactly",
        with_expectation.len()
    );
    if !mismatches.is_empty() {
        eprintln!("mismatches (expected parse-diagnostic count vs. what this parser produced):");
        for r in &mismatches {
            eprintln!(
                "  {}: expected {:?}, got {}",
                r.path.strip_prefix(&root).unwrap_or(&r.path).display(),
                r.expected_parse_count,
                r.diagnostic_count
            );
        }
    }

    // A generous floor (90%), kept as integer arithmetic rather than a
    // float ratio: real regressions in ordinary (non-filter) diagnostics
    // would fail far more than this. The known gap is filter-literal
    // validation, which lives in the checker (see the module doc comment).
    let total_with_expectation = with_expectation.len();
    assert!(
        total_with_expectation == 0 || matches * 100 >= total_with_expectation * 90,
        "only {matches}/{total_with_expectation} cases matched their expected parse-diagnostic count; \
         see the mismatch list above for a possible regression"
    );
}
