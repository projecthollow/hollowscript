//! The `--json` output mode's small, dependency-free encoder, matching the
//! shape `corpus/FORMAT.md` and the conformance tooling that drives this
//! binary from outside expect.

use hollowscript_core::Diagnostic;
use std::fmt::Write as _;

/// One `check --json` document: `{"diagnostics": [...]}`.
#[must_use]
pub(crate) fn check_document(diagnostics: &[Diagnostic]) -> String {
    let mut out = String::from(r#"{"diagnostics":"#);
    diagnostics_array(diagnostics, &mut out);
    out.push('}');
    out
}

/// A `format --json` document: `{"output": null | [lines], "diagnostics": [...]}`.
/// `output` is `null` when the formatter refused.
#[must_use]
pub(crate) fn format_document(output: Option<&str>, diagnostics: &[Diagnostic]) -> String {
    let mut out = String::from(r#"{"output":"#);
    match output {
        None => out.push_str("null"),
        Some(text) => {
            out.push('[');
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                escape_json_string(line, &mut out);
                out.push('"');
            }
            out.push(']');
        }
    }
    out.push_str(r#","diagnostics":"#);
    diagnostics_array(diagnostics, &mut out);
    out.push('}');
    out
}

pub(crate) fn diagnostics_array(diagnostics: &[Diagnostic], out: &mut String) {
    out.push('[');
    for (i, d) in diagnostics.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"severity":""#);
        out.push_str(d.severity.as_str());
        out.push_str(r#"","line":"#);
        let line = d.location.as_ref().map_or(0, |l| l.line);
        let _ = write!(out, "{line}");
        out.push_str(r#","code":""#);
        out.push_str(&d.code.to_text());
        if let Some(l) = &d.location {
            out.push_str(r#"","file":""#);
            escape_json_string(&l.file, out);
        }
        out.push_str(r#"","message":""#);
        escape_json_string(&d.message, out);
        out.push_str(r#""}"#);
    }
    out.push(']');
}

/// Appends the JSON string-literal encoding of `text`, quotes not included.
pub(crate) fn escape_json_string(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{check_document, format_document};
    use hollowscript_core::{Code, Diagnostic};

    #[test]
    fn a_check_document_lists_every_diagnostic() {
        let d = Diagnostic::new(Code::HS1099, &[("message", &"x")]);
        let json = check_document(&[d]);
        assert!(
            json.starts_with(r#"{"diagnostics":[{"severity":"error","line":0,"code":"HS1099""#)
        );
        assert!(json.ends_with("}]}"));
    }

    #[test]
    fn a_format_document_has_null_output_when_refused() {
        let json = format_document(None, &[]);
        assert_eq!(json, r#"{"output":null,"diagnostics":[]}"#);
    }

    #[test]
    fn a_format_document_splits_output_into_lines() {
        let json = format_document(Some("a\nb\n"), &[]);
        assert_eq!(json, r#"{"output":["a","b",""],"diagnostics":[]}"#);
    }
}
