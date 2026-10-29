//! Comparing what an implementation did with what a case expects
//! (`corpus/FORMAT.md`, "Diagnostics").
//!
//! A list of diagnostics is compared as a whole: the two lists must be the
//! same length, and after both are put in the canonical order of
//! `(file, line, severity with errors first, code)` each pair must agree on
//! every key the expected diagnostic gives. Message text is never compared
//! except through `contains`.

use crate::expected::ExpectedDiagnostic;
use hollowscript::{Diagnostic, Severity};

/// Compares a produced list against an expected one, returning one line per
/// disagreement.
pub(crate) fn diagnostics(
    stage: &str,
    actual: &[Diagnostic],
    expected: &[ExpectedDiagnostic],
) -> Vec<String> {
    let mut problems = Vec::new();
    // Files are ordered by load order with the entry module first, which is
    // the order they first appear in the produced list.
    let mut file_order: Vec<&str> = Vec::new();
    for d in actual {
        if let Some(location) = &d.location {
            if !file_order.iter().any(|f| *f == &*location.file) {
                file_order.push(&location.file);
            }
        }
    }
    let rank = |file: Option<&str>| -> usize {
        file.and_then(|f| file_order.iter().position(|k| *k == f))
            .unwrap_or(usize::MAX)
    };

    let mut actual: Vec<&Diagnostic> = actual.iter().collect();
    actual.sort_by_key(|d| {
        let location = d.location.as_ref();
        (
            rank(location.map(|l| &*l.file)),
            location.map_or(0, |l| l.line),
            u8::from(d.severity != Severity::Error),
            d.code.number(),
        )
    });
    let mut expected: Vec<&ExpectedDiagnostic> = expected.iter().collect();
    expected.sort_by_key(|d| {
        (
            rank(d.file.as_deref()),
            d.line,
            u8::from(d.severity != "error"),
            d.code
                .as_deref()
                .and_then(hollowscript::Code::parse)
                .map_or(u16::MAX, hollowscript::Code::number),
        )
    });

    if actual.len() != expected.len() {
        problems.push(format!(
            "{stage}: expected {} diagnostics, got {}: [{}]",
            expected.len(),
            actual.len(),
            actual
                .iter()
                .map(|d| format!("{}@{}", d.code, d.location.as_ref().map_or(0, |l| l.line)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return problems;
    }

    for (got, want) in actual.iter().zip(expected.iter()) {
        let severity = if got.severity == Severity::Error {
            "error"
        } else {
            "warning"
        };
        if severity != want.severity {
            problems.push(format!(
                "{stage}: expected a {} at line {}, got a {severity} ({})",
                want.severity, want.line, got.code
            ));
        }
        let line = got.location.as_ref().map_or(0, |l| l.line);
        if line != want.line {
            problems.push(format!(
                "{stage}: expected {} at line {}, got line {line}",
                want.code.as_deref().unwrap_or("a diagnostic"),
                want.line
            ));
        }
        if let Some(code) = &want.code {
            if hollowscript::Code::parse(code) != Some(got.code) {
                problems.push(format!(
                    "{stage}: expected {code} at line {}, got {}",
                    want.line, got.code
                ));
            }
        }
        if let Some(file) = &want.file {
            let actual_file = got.location.as_ref().map_or("", |l| &l.file);
            if actual_file != file {
                problems.push(format!(
                    "{stage}: expected {} in {file}, got it in {actual_file}",
                    want.code.as_deref().unwrap_or("a diagnostic")
                ));
            }
        }
        if let Some(contains) = &want.contains {
            if !got.message.contains(contains.as_str()) {
                problems.push(format!(
                    "{stage}: expected the message at line {} to contain {contains:?}, got {:?}",
                    want.line, got.message
                ));
            }
        }
    }
    problems
}

/// Compares two lists of printed output.
pub(crate) fn output(actual: &[String], expected: &[String]) -> Vec<String> {
    if actual == expected {
        return Vec::new();
    }
    vec![format!("run: expected output {expected:?}, got {actual:?}")]
}

#[cfg(test)]
mod tests {
    use super::{diagnostics, output};
    use crate::expected::ExpectedDiagnostic;
    use hollowscript::{Code, Diagnostic};

    fn want(line: u32, code: Option<&str>) -> ExpectedDiagnostic {
        ExpectedDiagnostic {
            severity: "error".to_owned(),
            line,
            code: code.map(str::to_owned),
            file: None,
            contains: None,
        }
    }

    fn got(line: u32, code: Code, message: &str) -> Diagnostic {
        let mut d = Diagnostic::new(code, &[]);
        d.message = message.to_owned();
        d.location = Some(hollowscript_core::Location {
            file: std::sync::Arc::from("main.hws"),
            span: hollowscript_core::Span { start: 0, end: 0 },
            line,
            column: 1,
        });
        d
    }

    #[test]
    fn an_exact_match_has_no_problems() {
        let actual = vec![got(3, Code::HS0301, "no")];
        assert!(diagnostics("check", &actual, &[want(3, Some("HS0301"))]).is_empty());
    }

    #[test]
    fn a_different_count_is_reported_once() {
        let actual = vec![got(3, Code::HS0301, "no")];
        let problems = diagnostics("check", &actual, &[]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("expected 0 diagnostics, got 1"));
    }

    #[test]
    fn a_different_line_or_code_is_reported() {
        let actual = vec![got(3, Code::HS0301, "no")];
        assert!(!diagnostics("check", &actual, &[want(4, Some("HS0301"))]).is_empty());
        assert!(!diagnostics("check", &actual, &[want(3, Some("HS0205"))]).is_empty());
    }

    #[test]
    fn a_code_the_case_does_not_give_is_not_compared() {
        let actual = vec![got(3, Code::HS0301, "no")];
        assert!(diagnostics("check", &actual, &[want(3, None)]).is_empty());
    }

    #[test]
    fn both_lists_are_put_in_canonical_order_first() {
        let actual = vec![got(5, Code::HS0205, "b"), got(2, Code::HS0301, "a")];
        let expected = vec![want(2, Some("HS0301")), want(5, Some("HS0205"))];
        assert!(diagnostics("check", &actual, &expected).is_empty());
    }

    #[test]
    fn contains_is_compared_when_given() {
        let actual = vec![got(3, Code::HS0301, "Expected String, found Int.")];
        let mut expected = want(3, Some("HS0301"));
        expected.contains = Some("found Int".to_owned());
        assert!(diagnostics("check", &actual, std::slice::from_ref(&expected)).is_empty());
        expected.contains = Some("found Bool".to_owned());
        assert!(!diagnostics("check", &actual, &[expected]).is_empty());
    }

    #[test]
    fn output_is_compared_as_a_whole() {
        assert!(output(&["a".to_owned()], &["a".to_owned()]).is_empty());
        assert!(!output(&["a".to_owned()], &["b".to_owned()]).is_empty());
    }
}
