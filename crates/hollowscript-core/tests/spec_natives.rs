//! Cross-checks the roster of native functions against the specification.
//!
//! Every roster entry must be a function or method that chapters 8, 13, 14 or
//! 15 define, and every function those chapters define in a signature table
//! must be in the roster. Set `HOLLOWSCRIPT_REQUIRE_SPEC` to make a missing
//! specification an error.

#![allow(clippy::print_stderr, reason = "reports a skipped test")]

use hollowscript_core::native::ROSTER;
use std::path::Path;

fn read(chapter: &str) -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/spec")
        .join(chapter);
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) => {
            assert!(
                std::env::var_os("HOLLOWSCRIPT_REQUIRE_SPEC").is_none(),
                "the specification is required but {} cannot be read: {e}",
                path.display()
            );
            eprintln!("skipping: {} cannot be read: {e}", path.display());
            None
        }
    }
}

const CHAPTERS: [&str; 4] = [
    "08-errors-and-results.md",
    "13-stdlib-core.md",
    "14-stdlib-collections.md",
    "15-stdlib-data.md",
];

#[test]
fn every_roster_entry_is_defined_by_the_specification() {
    let texts: Vec<String> = CHAPTERS.iter().filter_map(|c| read(c)).collect();
    if texts.len() != CHAPTERS.len() {
        return;
    }
    let all = texts.join("\n");
    for entry in ROSTER {
        let base = entry.name.rsplit('.').next().unwrap_or(entry.name);
        // The comparator overload of `sort` is its own roster entry.
        let word = if base == "sortBy" { "sort" } else { base };
        let (namespace, _) = entry.name.split_once('.').unwrap_or(("", ""));
        let plain = format!("{word}(");
        let generic = format!("{word}<");
        assert!(
            all.contains(&plain) || all.contains(&generic),
            "{} is not defined by the specification",
            entry.name
        );
        if namespace == "json" || namespace == "time" {
            assert!(
                all.contains(&format!("{namespace}.{word}(")),
                "{}",
                entry.name
            );
        }
    }
}

#[test]
fn every_signature_row_of_the_tables_is_in_the_roster() {
    let mut missing = Vec::new();
    for chapter in [
        "13-stdlib-core.md",
        "14-stdlib-collections.md",
        "15-stdlib-data.md",
    ] {
        let Some(text) = read(chapter) else { return };
        for line in text.lines() {
            // Rows look like: | `name(args): Type` | meaning | cost |
            let Some(rest) = line.strip_prefix("| `") else {
                continue;
            };
            let Some(signature) = rest.split('`').next() else {
                continue;
            };
            let Some((head, _)) = signature.split_once('(') else {
                continue;
            };
            let head = head.split('<').next().unwrap_or(head);
            if head.is_empty() || !head.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
                continue;
            }
            let found = ROSTER
                .iter()
                .any(|n| n.name == head || n.name.ends_with(&format!(".{head}")));
            if !found {
                missing.push(format!("{chapter}: {head}"));
            }
        }
    }
    missing.sort();
    missing.dedup();
    // `success` and `failure` are built by the compiler, not natives, and
    // `keys` and `has` of maps share a name with the record functions.
    missing.retain(|m| !m.ends_with("success") && !m.ends_with("failure"));
    assert!(
        missing.is_empty(),
        "in the specification but not in the roster: {missing:?}"
    );
}
