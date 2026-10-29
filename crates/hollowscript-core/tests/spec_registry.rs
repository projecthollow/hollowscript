//! Cross-checks the code registry against chapter 17 of the specification.
//!
//! The registry is written by hand in `src/codes.rs`. This test parses the
//! tables of `docs/spec/17-diagnostics.md` and requires the two to agree on
//! every code, severity, stage, message template and span description, and on
//! the ranges of section 17.5. Set `HOLLOWSCRIPT_REQUIRE_SPEC` to make a
//! missing specification an error instead of a skipped test.

#![allow(clippy::print_stderr, reason = "reports a skipped test")]

use hollowscript_core::codes::{Code, Family, Severity, all};
use std::collections::BTreeMap;
use std::path::Path;

/// The ranges of section 17.5: first code, last code and description.
type Ranges = Vec<(u16, u16, String)>;

#[derive(Debug)]
struct Row {
    severity: Severity,
    stages: String,
    template: String,
    span: String,
}

/// Undoes the escaping of a Markdown table cell: a backslash quotes the
/// character after it.
fn unescape(cell: &str) -> String {
    let mut out = String::new();
    let mut chars = cell.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    out
}

fn read_spec() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/spec/17-diagnostics.md");
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

/// The rows of the tables of sections 17.2 to 17.4, and the ranges of 17.5.
fn parse(text: &str) -> (BTreeMap<u16, Row>, Ranges) {
    let mut rows = BTreeMap::new();
    let mut ranges = Vec::new();
    let mut section = "";
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            section = if heading.starts_with("17.2") {
                "errors"
            } else if heading.starts_with("17.3") {
                "warnings"
            } else if heading.starts_with("17.4") {
                "faults"
            } else if heading.starts_with("17.5") {
                "ranges"
            } else {
                ""
            };
            continue;
        }
        let Some(inner) = line.strip_prefix("| ").and_then(|l| l.strip_suffix(" |")) else {
            continue;
        };
        let cells: Vec<&str> = inner.split(" | ").map(str::trim).collect();
        if section == "ranges" {
            if let [range, what] = cells[..] {
                if let Some((from, to)) = range.split_once(" to ") {
                    let number = |c: &str| c.strip_prefix("HS").and_then(|d| d.parse::<u16>().ok());
                    if let (Some(from), Some(to)) = (number(from), number(to)) {
                        ranges.push((from, to, what.to_owned()));
                    }
                }
            }
            continue;
        }
        let Some(number) = cells
            .first()
            .and_then(|c| c.strip_prefix("HS"))
            .and_then(|d| d.parse::<u16>().ok())
        else {
            continue;
        };
        let row = match (section, &cells[..]) {
            ("errors" | "warnings", [_, stage, message, span]) => Row {
                severity: if section == "warnings" {
                    Severity::Warning
                } else {
                    Severity::Error
                },
                stages: (*stage).to_owned(),
                template: unescape(message),
                span: unescape(span),
            },
            ("faults", [_, message, span]) => Row {
                severity: Severity::Error,
                stages: "run".to_owned(),
                template: unescape(message),
                span: unescape(span),
            },
            _ => continue,
        };
        assert!(
            rows.insert(number, row).is_none(),
            "HS{number:04} is listed twice in the specification"
        );
    }
    (rows, ranges)
}

#[test]
fn the_registry_is_the_specification() {
    let Some(text) = read_spec() else { return };
    let (spec, _) = parse(&text);
    assert!(
        spec.len() > 100,
        "only {} codes were parsed; the table format may have changed",
        spec.len()
    );

    let registry: BTreeMap<u16, _> = all().iter().map(|i| (i.code.number(), i)).collect();
    let missing: Vec<_> = spec
        .keys()
        .filter(|n| !registry.contains_key(n))
        .map(|n| format!("HS{n:04}"))
        .collect();
    let extra: Vec<_> = registry
        .keys()
        .filter(|n| !spec.contains_key(n))
        .map(|n| format!("HS{n:04}"))
        .collect();
    assert!(
        missing.is_empty(),
        "in the specification but not in the registry: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "in the registry but not in the specification: {extra:?}"
    );

    for (number, info) in &registry {
        let row = &spec[number];
        let code = info.code;
        assert_eq!(info.severity, row.severity, "{code}: severity");
        let stages = info
            .stages
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(stages, row.stages, "{code}: stage");
        assert_eq!(info.template, row.template, "{code}: message template");
        assert_eq!(info.span, row.span, "{code}: span");
    }
}

#[test]
fn the_registry_is_ordered_and_parses_back() {
    let numbers: Vec<u16> = all().iter().map(|i| i.code.number()).collect();
    assert!(
        numbers.windows(2).all(|w| w[0] < w[1]),
        "the registry must be sorted and free of duplicates"
    );
    for info in all() {
        assert_eq!(Code::parse(&info.code.to_text()), Some(info.code));
        assert_eq!(Code::from_number(info.code.number()), Some(info.code));
    }
    assert_eq!(Code::parse("HS9999"), None);
    assert_eq!(Code::parse("HS301"), None);
    assert_eq!(Code::parse("XX0301"), None);
}

#[test]
fn families_follow_the_ranges_of_the_specification() {
    let Some(text) = read_spec() else { return };
    let (_, ranges) = parse(&text);
    assert_eq!(ranges.len(), 12, "section 17.5 lists twelve ranges");
    let first_of = |family: Family| match family {
        Family::Lexical => 1,
        Family::Syntax => 101,
        Family::Names => 201,
        Family::Types => 301,
        Family::Effects => 401,
        Family::Patterns => 501,
        Family::Modules => 601,
        Family::Filters => 701,
        Family::Warnings => 801,
        Family::Limits => 901,
        Family::Faults => 1001,
        Family::Host => 1201,
    };
    for info in all() {
        let n = info.code.number();
        let (from, to, _) = ranges
            .iter()
            .find(|(from, to, _)| (*from..=*to).contains(&n))
            .unwrap_or_else(|| panic!("{} is in no range of section 17.5", info.code));
        assert_eq!(*from, first_of(info.code.family()), "{}: family", info.code);
        assert!(*to > *from);
        let warning = info.code.family() == Family::Warnings;
        assert_eq!(
            warning,
            info.severity == Severity::Warning,
            "{}: only warnings are in the warning range",
            info.code
        );
    }
}
