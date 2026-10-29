//! The formatter's round-trip property: for a source that parses cleanly
//! (no lexical or syntax error), formatting must either
//!
//! - succeed with output that itself parses cleanly, parses to the same
//!   tree as the source (span and node id aside), carries exactly the same
//!   comments (compared as a multiset of texts) and is idempotent
//!   (`format(format(source)) == format(source)`), or
//! - refuse, which is only acceptable when the source has a comment this
//!   module's canonical layout has no place for.
//!
//! Checked over the whole conformance corpus, over every corpus program with
//! comments injected at random positions, over generated expressions placed
//! in every statement position (deliberately including deep nesting, long
//! chains and parentheses in every position that matters, such as a
//! parenthesised record literal used as a condition), and over mutated
//! corpus programs that still happen to parse cleanly. `format_fuzz.rs`
//! separately checks the weaker, unconditional property that `format` never
//! panics, including on input that does not parse at all.
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
use hollowscript_syntax::{Comment, format, parse};
use support::{Lcg, mutate, normalize_file};

fn has_error(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}

fn comment_keys(text: &str, comments: &[Comment]) -> Vec<String> {
    comments
        .iter()
        .map(|c| {
            text.get(c.span.start as usize..c.span.end as usize)
                .unwrap_or("")
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The whole contract, for one source already known to parse cleanly.
/// Returns whether it was formatted (`false` means it was refused, which
/// `check_contract` itself has already established is legitimate).
fn check_contract(interner: &mut Interner, source: &str, label: &str) -> bool {
    let source_a = Source::new(SourceId(0), "roundtrip.hws", source.to_string())
        .expect("test sources fit the size limit");
    let mut parsed_a = parse(&source_a, interner, &mut Control::none());
    assert!(
        !has_error(&parsed_a.diagnostics),
        "{label}: called with a source that does not parse cleanly: {:?}\n{source}",
        parsed_a.diagnostics
    );

    let out = format(&source_a, interner, &mut Control::none());
    match out.output {
        None => {
            assert!(
                out.diagnostics.iter().any(Diagnostic::is_error),
                "{label}: refused with no error diagnostic: {:?}",
                out.diagnostics
            );
            assert!(
                out.diagnostics
                    .iter()
                    .any(|d| d.message.to_ascii_lowercase().contains("comment")),
                "{label}: a clean source was refused for a reason other than a comment: {:?}\n{source}",
                out.diagnostics
            );
            false
        }
        Some(formatted) => {
            let source_b = Source::new(SourceId(0), "roundtrip.hws", formatted.clone())
                .expect("formatted text fits the size limit");
            let mut parsed_b = parse(&source_b, interner, &mut Control::none());
            assert!(
                !has_error(&parsed_b.diagnostics),
                "{label}: formatted output does not parse\n--- input\n{source}\n--- output\n{formatted}"
            );

            normalize_file(&mut parsed_a.file);
            normalize_file(&mut parsed_b.file);
            assert_eq!(
                parsed_a.file.uses, parsed_b.file.uses,
                "{label}: the `uses` lines changed\n--- input\n{source}\n--- output\n{formatted}"
            );
            assert_eq!(
                parsed_a.file.imports, parsed_b.file.imports,
                "{label}: the `import` lines changed\n--- input\n{source}\n--- output\n{formatted}"
            );
            assert_eq!(
                parsed_a.file.items, parsed_b.file.items,
                "{label}: the tree changed\n--- input\n{source}\n--- output\n{formatted}"
            );

            let mut before = comment_keys(source, &parsed_a.file.comments);
            let mut after = comment_keys(&formatted, &parsed_b.file.comments);
            before.sort();
            after.sort();
            assert_eq!(
                before, after,
                "{label}: the comment multiset changed\n--- input\n{source}\n--- output\n{formatted}"
            );

            let again = format(&source_b, interner, &mut Control::none()).output;
            assert_eq!(
                again.as_deref(),
                Some(formatted.as_str()),
                "{label}: not idempotent\n--- input\n{source}\n--- output\n{formatted}"
            );
            true
        }
    }
}

// ---- the whole conformance corpus --------------------------------------

#[test]
fn every_clean_corpus_program_round_trips_and_is_idempotent() {
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

    let mut formatted = 0;
    let mut refused = 0;
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut interner = Interner::new();
        let probe = Source::new(SourceId(0), "probe.hws", text.clone())
            .expect("corpus files fit the size limit");
        if has_error(&parse(&probe, &mut interner, &mut Control::none()).diagnostics) {
            // Out of scope: many corpus cases are there to pin a diagnostic,
            // and `format` refusing a source with a syntax error is already
            // covered by `format.rs`'s own unit tests, not this property.
            continue;
        }
        let label = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        if check_contract(&mut interner, &text, &label) {
            formatted += 1;
        } else {
            refused += 1;
        }
    }
    eprintln!(
        "corpus round-trip: {formatted} formatted, {refused} refused (a stray comment), \
         out of {} clean corpus files",
        formatted + refused
    );
    assert!(
        formatted > 0,
        "expected at least one clean corpus file to format"
    );
}

// ---- comments injected at random own-line and trailing positions -------

#[test]
fn comments_injected_at_random_positions_are_never_lost() {
    let root = support::corpus_root();
    if !root.exists() {
        eprintln!("skipping: {} not found", root.display());
        return;
    }
    let files = support::walk_hws(&root);
    let sources: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    assert!(
        !sources.is_empty(),
        "expected corpus sources to inject comments into"
    );

    let mut rng = Lcg::new(0x5eed_1234);
    let mut formatted = 0;
    let mut refused = 0;
    for (i, original) in sources.iter().enumerate() {
        for variant in 0..3 {
            let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
            if lines.is_empty() {
                continue;
            }
            for n in 0..rng.count(3) {
                let at = rng.below(lines.len());
                let text = format!(" c{i}.{variant}.{n} with, punctuation: and \"quotes\" ");
                if rng.one_in(2) {
                    lines.insert(at, format!("//{text}"));
                } else {
                    lines[at] = format!("{} //{text}", lines[at]);
                }
            }
            let source = lines.join("\n");
            let mut interner = Interner::new();
            let Ok(probe) = Source::new(SourceId(0), "probe.hws", source.clone()) else {
                continue;
            };
            if has_error(&parse(&probe, &mut interner, &mut Control::none()).diagnostics) {
                continue;
            }
            let label = format!("injected[{i}.{variant}]");
            if check_contract(&mut interner, &source, &label) {
                formatted += 1;
            } else {
                refused += 1;
            }
        }
    }
    eprintln!("comment injection: {formatted} formatted, {refused} refused");
    assert!(
        formatted > refused,
        "too many refusals: {refused} refused against {formatted} formatted"
    );
}

// ---- generated expressions, with and without load-bearing parentheses --

fn generate_expression(rng: &mut Lcg, depth: u32) -> String {
    if depth == 0 || rng.one_in(5) {
        const LEAVES: &[&str] = &[
            "1",
            "2.5",
            "x",
            "none",
            "true",
            "\"s\"",
            "`a:b`",
            "({ a: 1 })",
            "({ a: [1], b: { c: 2 } })",
            "{ a: 1 }",
            "[1, 2]",
            "[{ a: 1 }]",
            "fn(): Int { return 1 }",
            "\"v ${x}\"",
            "\"v ${({ a: 1 }).a}\"",
        ];
        return LEAVES[rng.below(LEAVES.len())].to_string();
    }
    match rng.below(8) {
        0 | 1 => {
            const OPS: &[&str] = &[
                "or", "and", "==", "!=", "<", ">", "<=", ">=", "+", "-", "*", "/",
            ];
            let a = generate_expression(rng, depth - 1);
            let op = OPS[rng.below(OPS.len())];
            let b = generate_expression(rng, depth - 1);
            maybe_wrap(format!("{a} {op} {b}"), rng)
        }
        2 => {
            const PREFIXES: &[&str] = &["not ", "- ", "-"];
            let prefix = PREFIXES[rng.below(PREFIXES.len())];
            let inner = generate_expression(rng, depth - 1);
            maybe_wrap(format!("{prefix}{inner}"), rng)
        }
        3 => {
            let f = generate_expression(rng, depth - 1);
            let a = generate_expression(rng, depth - 1);
            let b = generate_expression(rng, depth - 1);
            maybe_wrap(format!("{f}({a}, {b})"), rng)
        }
        4 => {
            let a = generate_expression(rng, depth - 1);
            let b = generate_expression(rng, depth - 1);
            maybe_wrap(format!("{a}[{b}]"), rng)
        }
        5 => {
            let a = generate_expression(rng, depth - 1);
            maybe_wrap(format!("{a}.f"), rng)
        }
        6 => {
            let a = generate_expression(rng, depth - 1);
            let b = generate_expression(rng, depth - 1);
            format!("[{a}, {b}]")
        }
        _ => format!("({})", generate_expression(rng, depth - 1)),
    }
}

fn maybe_wrap(text: String, rng: &mut Lcg) -> String {
    if rng.one_in(3) {
        format!("({text})")
    } else {
        text
    }
}

const CONTEXTS: &[fn(&str) -> String] = &[
    |e| format!("constant v = {e}"),
    |e| format!("variable v = 1\nv = {e}"),
    |e| format!("return {e}"),
    |e| format!("f({e})"),
    |e| format!("f({e}, 1)"),
    |e| format!("effect f({e})"),
    |e| format!("if {e} {{ return 1 }}"),
    |e| format!("if a {{ return 1 }} else if {e} {{ return 2 }} else {{ return 3 }}"),
    |e| format!("while {e} {{ break }}"),
    |e| format!("for x in {e} {{ continue }}"),
    |e| format!("match {e} {{ 1 {{ return \"a\" }} else {{ return \"b\" }} }}"),
    |e| format!("fn g() {{\n    if {e} {{\n        return\n    }}\n}}"),
    |e| format!("constant c = fn(): Int {{\n    while {e} {{ break }}\n    return 1\n}}"),
    |e| e.to_string(),
    |e| format!("({e})()"),
];

/// Whether some line of `source` starts with a condition-like keyword and
/// also contains `({`, the shape a parenthesised record literal used as a
/// condition, scrutinee or collection takes.
fn looks_like_a_parenthesised_record_condition(source: &str) -> bool {
    const STARTS: &[&str] = &["if ", "while ", "for ", "match "];
    source
        .lines()
        .any(|line| STARTS.iter().any(|s| line.starts_with(s)) && line.contains("({"))
}

#[test]
fn generated_expressions_in_every_statement_position_keep_the_parentheses_they_need() {
    let mut rng = Lcg::new(0x00c0_ffee);
    let mut interner = Interner::new();
    let mut parsed = 0;
    let mut with_record_condition = 0;
    for n in 0..6000 {
        let context = CONTEXTS[n % CONTEXTS.len()];
        let depth = 1 + u32::try_from(rng.below(4)).unwrap_or(0);
        let expression = generate_expression(&mut rng, depth);
        let source = context(&expression);
        let Ok(probe) = Source::new(SourceId(0), "probe.hws", source.clone()) else {
            continue;
        };
        if has_error(&parse(&probe, &mut interner, &mut Control::none()).diagnostics) {
            continue;
        }
        parsed += 1;
        if looks_like_a_parenthesised_record_condition(&source) {
            with_record_condition += 1;
        }
        check_contract(&mut interner, &source, &format!("generated[{n}]"));
    }
    eprintln!(
        "generated: {parsed}/6000 parsed cleanly, {with_record_condition} put a parenthesised record in a condition"
    );
    assert!(parsed >= 1500, "only {parsed} generated programs parsed");
    assert!(
        with_record_condition >= 200,
        "only {with_record_condition} generated programs put a parenthesised record in a condition"
    );
}

// ---- mutated corpus programs that still happen to parse cleanly --------

#[test]
fn mutated_corpus_programs_that_still_parse_cleanly_round_trip() {
    const ITERATIONS: usize = 4000;

    let root = support::corpus_root();
    if !root.exists() {
        eprintln!("skipping: {} not found", root.display());
        return;
    }
    let files = support::walk_hws(&root);
    let sources: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    assert!(!sources.is_empty(), "expected corpus sources to mutate");

    let mut rng = Lcg::new(0x0bad_c0de);
    let mut interner = Interner::new();
    let mut checked = 0;
    for n in 0..ITERATIONS {
        let base = &sources[rng.below(sources.len())];
        let mut text = base.clone();
        for _ in 0..rng.count(4) {
            text = mutate(&text, &mut rng);
        }
        let Ok(probe) = Source::new(SourceId(0), "probe.hws", text.clone()) else {
            continue;
        };
        if has_error(&parse(&probe, &mut interner, &mut Control::none()).diagnostics) {
            continue;
        }
        checked += 1;
        check_contract(&mut interner, &text, &format!("mutated[{n}]"));
    }
    eprintln!(
        "mutated corpus: {checked}/{ITERATIONS} mutations still parsed cleanly and were checked"
    );
}
