//! The typed form of a case's `<name>.expected.json` (`corpus/FORMAT.md`).

use crate::json::{Json, parse};

/// One expected diagnostic. Every field but `severity` and `line` is
/// compared only when the case gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExpectedDiagnostic {
    /// `"error"` or `"warning"`.
    pub severity: String,
    /// The 1-based line the offending construct starts on.
    pub line: u32,
    /// The stable diagnostic code, when the case gives one.
    pub code: Option<String>,
    /// The module the diagnostic belongs to, when the case gives one.
    pub file: Option<String>,
    /// A substring the message must contain, when the case gives one.
    pub contains: Option<String>,
}

/// What the formatter is expected to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedFormat {
    /// The source is already canonical.
    Same,
    /// The formatted text, as lines joined with `\n`.
    Lines(Vec<String>),
    /// The formatter must refuse.
    Refused,
}

/// What running the program is expected to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExpectedRun {
    /// The program ran to completion.
    Completed {
        /// The strings passed to `print`, in order.
        output: Vec<String>,
        /// The rendered load result, or `None` for a program with no
        /// top-level `return`.
        result: Option<String>,
    },
    /// The program stopped, at the stage named.
    Stopped {
        /// `"check"` or `"run"`.
        stage: String,
        /// Whatever was printed before it stopped.
        output: Vec<String>,
        /// The diagnostics that explain the stop.
        diagnostics: Vec<ExpectedDiagnostic>,
    },
}

/// One case's whole expected document.
#[derive(Clone, Debug)]
pub(crate) struct Expected {
    /// The sentence stating the rule the case pins.
    pub title: String,
    /// Grants the case needs beyond the default host.
    pub grants: Vec<String>,
    /// The diagnostics of the parse stage.
    pub parse: Vec<ExpectedDiagnostic>,
    /// The diagnostics of the check stage.
    pub check: Vec<ExpectedDiagnostic>,
    /// What the formatter does, when the case pins it.
    pub format: Option<(ExpectedFormat, Vec<ExpectedDiagnostic>)>,
    /// What running the program does.
    pub run: ExpectedRun,
}

/// Reads one expected document.
pub(crate) fn read(text: &str) -> Result<Expected, String> {
    let document = parse(text).map_err(|e| e.to_string())?;
    let title = document
        .get("title")
        .and_then(Json::as_str)
        .ok_or("no title")?
        .to_owned();
    let grants = match document.get("grants") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("grants is not an array")?
            .iter()
            .map(|g| {
                g.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "a grant is not a string".to_owned())
            })
            .collect::<Result<_, _>>()?,
    };
    let parse_stage = diagnostics(document.get("parse").ok_or("no parse")?)?;
    let check = diagnostics(document.get("check").ok_or("no check")?)?;
    let format = match document.get("format") {
        None => None,
        Some(value) => Some(format_stage(value)?),
    };
    let run = run_stage(document.get("run").ok_or("no run")?)?;
    Ok(Expected {
        title,
        grants,
        parse: parse_stage,
        check,
        format,
        run,
    })
}

fn diagnostics(value: &Json) -> Result<Vec<ExpectedDiagnostic>, String> {
    let items = value
        .as_array()
        .ok_or("a diagnostic list is not an array")?;
    items.iter().map(diagnostic).collect()
}

fn diagnostic(value: &Json) -> Result<ExpectedDiagnostic, String> {
    let severity = value
        .get("severity")
        .and_then(Json::as_str)
        .ok_or("a diagnostic has no severity")?
        .to_owned();
    let line = value
        .get("line")
        .and_then(Json::as_i64)
        .ok_or("a diagnostic has no line")?;
    let line = u32::try_from(line).map_err(|_| "a line is out of range".to_owned())?;
    Ok(ExpectedDiagnostic {
        severity,
        line,
        code: value.get("code").and_then(Json::as_str).map(str::to_owned),
        file: value.get("file").and_then(Json::as_str).map(str::to_owned),
        contains: value
            .get("contains")
            .and_then(Json::as_str)
            .map(str::to_owned),
    })
}

fn format_stage(value: &Json) -> Result<(ExpectedFormat, Vec<ExpectedDiagnostic>), String> {
    let output = value.get("output").ok_or("a format stage has no output")?;
    let format = if output.is_null() {
        ExpectedFormat::Refused
    } else if let Some("same") = output.as_str() {
        ExpectedFormat::Same
    } else if let Some(lines) = output.as_array() {
        ExpectedFormat::Lines(
            lines
                .iter()
                .map(|l| {
                    l.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "a formatted line is not a string".to_owned())
                })
                .collect::<Result<_, _>>()?,
        )
    } else {
        return Err("a format output is not \"same\", an array or null".to_owned());
    };
    let diagnostics = match value.get("diagnostics") {
        None => Vec::new(),
        Some(value) => diagnostics(value)?,
    };
    Ok((format, diagnostics))
}

fn run_stage(value: &Json) -> Result<ExpectedRun, String> {
    let ok = value
        .get("ok")
        .and_then(Json::as_bool)
        .ok_or("a run stage has no ok")?;
    let output = match value.get("output") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("a run output is not an array")?
            .iter()
            .map(|l| {
                l.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "a printed line is not a string".to_owned())
            })
            .collect::<Result<_, _>>()?,
    };
    if ok {
        let result = match value.get("result") {
            None => None,
            Some(v) if v.is_null() => None,
            Some(v) => Some(v.as_str().ok_or("a result is not a string")?.to_owned()),
        };
        return Ok(ExpectedRun::Completed { output, result });
    }
    Ok(ExpectedRun::Stopped {
        stage: value
            .get("stage")
            .and_then(Json::as_str)
            .ok_or("a failed run has no stage")?
            .to_owned(),
        output,
        diagnostics: match value.get("diagnostics") {
            None => Vec::new(),
            Some(value) => diagnostics(value)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{ExpectedFormat, ExpectedRun, read};

    #[test]
    fn reads_a_completed_case() {
        let expected = read(
            r#"{
              "title": "A rule",
              "spec": "4.8",
              "parse": [],
              "check": [],
              "format": { "output": "same", "diagnostics": [] },
              "run": { "ok": true, "output": ["a"], "result": "Int(2)" }
            }"#,
        )
        .expect("the document reads");
        assert_eq!(expected.title, "A rule");
        assert!(expected.grants.is_empty());
        assert_eq!(expected.format.map(|f| f.0), Some(ExpectedFormat::Same));
        assert_eq!(
            expected.run,
            ExpectedRun::Completed {
                output: vec!["a".to_owned()],
                result: Some("Int(2)".to_owned()),
            }
        );
    }

    #[test]
    fn reads_a_case_that_stops_at_the_check_stage() {
        let expected = read(
            r#"{
              "title": "A rule",
              "parse": [],
              "check": [{ "severity": "error", "line": 2, "code": "HS0301" }],
              "run": { "ok": false, "stage": "check", "output": [], "diagnostics": [
                 { "severity": "error", "line": 2, "code": "HS0301" }
              ] }
            }"#,
        )
        .expect("the document reads");
        assert_eq!(expected.check.len(), 1);
        assert_eq!(expected.check[0].line, 2);
        assert_eq!(expected.check[0].code.as_deref(), Some("HS0301"));
        assert!(expected.format.is_none());
        match expected.run {
            ExpectedRun::Stopped { stage, .. } => assert_eq!(stage, "check"),
            ExpectedRun::Completed { .. } => panic!("expected a stopped run"),
        }
    }

    #[test]
    fn reads_a_null_result_and_a_refused_format() {
        let expected = read(
            r#"{
              "title": "A rule",
              "grants": ["fs:read=./data"],
              "parse": [],
              "check": [],
              "format": { "output": null, "diagnostics": [] },
              "run": { "ok": true, "output": [], "result": null }
            }"#,
        )
        .expect("the document reads");
        assert_eq!(expected.grants, ["fs:read=./data"]);
        assert_eq!(expected.format.map(|f| f.0), Some(ExpectedFormat::Refused));
        assert_eq!(
            expected.run,
            ExpectedRun::Completed {
                output: vec![],
                result: None
            }
        );
    }

    #[test]
    fn a_document_missing_a_required_key_is_an_error() {
        assert!(read(r#"{ "title": "x", "parse": [] }"#).is_err());
        assert!(read("not json").is_err());
    }
}
