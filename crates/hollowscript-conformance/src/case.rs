//! Running one conformance case through all four stages and comparing each
//! against the case's expected file.

use crate::compare;
use crate::discover::Case;
use crate::expected::{Expected, ExpectedFormat, ExpectedRun, read};
use crate::host;
use crate::host_file;
use hollowscript::{Control, LoadOutcome, LoadResult, Loader, LoaderError, Options};
use std::collections::BTreeMap;
use std::path::Path;

/// The entry module's canonical path. `corpus/FORMAT.md`: "The program of a
/// case is the entry module and is addressed as `main.hws`, whatever the
/// file is called."
pub(crate) const ENTRY: &str = "main.hws";

/// What running one case produced.
pub(crate) enum Outcome {
    /// The case could not be read or its expected file could not be parsed.
    Broken(String),
    /// Every stage agreed with the expected file.
    Passed,
    /// At least one stage disagreed; one line per disagreement.
    Failed(Vec<String>),
}

/// A loader over the files of a case's `<name>.modules/` directory, keyed by
/// canonical path.
pub(crate) struct ModuleLoader {
    pub(crate) modules: BTreeMap<String, Vec<u8>>,
}

impl Loader for ModuleLoader {
    fn load(&mut self, canonical_path: &str) -> Result<Vec<u8>, LoaderError> {
        self.modules
            .get(canonical_path)
            .cloned()
            .ok_or(LoaderError::NotFound)
    }
}

/// Reads every file under `dir`, keyed by its path relative to `dir` with
/// `/` separators, which is exactly a canonical path.
fn read_modules(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let key = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if entry.path().is_dir() {
                stack.push((entry.path(), key));
            } else if let Ok(bytes) = std::fs::read(entry.path()) {
                out.insert(key, bytes);
            }
        }
    }
    out
}

/// Runs one case.
pub(crate) fn run(case: &Case) -> Outcome {
    let source = match std::fs::read_to_string(&case.program) {
        Ok(s) => s,
        Err(e) => return Outcome::Broken(format!("cannot read the program: {e}")),
    };
    let expected_text = match std::fs::read_to_string(&case.expected) {
        Ok(s) => s,
        Err(e) => return Outcome::Broken(format!("cannot read the expected file: {e}")),
    };
    let expected = match read(&expected_text) {
        Ok(e) => e,
        Err(e) => return Outcome::Broken(format!("cannot read the expected file: {e}")),
    };
    // Section 9.5's grants and, per section 19.4, `<name>.host.json`'s
    // files and canned network exchanges: what this case adds to the fixed
    // host (`crate::hostconfig`).
    let mut config = crate::hostconfig::HostConfig::default();
    config.apply_grants(&expected.grants);
    let host_json = case.program.with_extension("").with_extension("host.json");
    if host_json.is_file() {
        config.apply_host_json(&host_json);
    }
    let modules = read_modules(&case.program.with_extension("").with_extension("modules"));
    let limits = host_file::limits_for(&case.program);
    let problems = compare_stages(&source, &expected, modules, &config, limits);
    if problems.is_empty() {
        Outcome::Passed
    } else {
        // The expected file's own one-sentence statement of the rule this
        // case pins, first, so a failure reads as "what should be true"
        // before "what disagreed".
        Outcome::Failed(
            std::iter::once(expected.title.clone())
                .chain(problems)
                .collect(),
        )
    }
}

fn compare_stages(
    source: &str,
    expected: &Expected,
    modules: BTreeMap<String, Vec<u8>>,
    config: &crate::hostconfig::HostConfig,
    limits: hollowscript::Limits,
) -> Vec<String> {
    let mut problems = Vec::new();

    let parsed = hollowscript::parse_diagnostics(source, ENTRY, &mut Control::none());
    problems.extend(compare::diagnostics("parse", &parsed, &expected.parse));

    let capabilities = host::capabilities(config);
    let checked = hollowscript::check(
        source,
        ENTRY,
        &capabilities,
        config.filter_schema.as_ref(),
        &mut ModuleLoader {
            modules: modules.clone(),
        },
        &mut Control::none(),
    );
    problems.extend(compare::diagnostics("check", &checked, &expected.check));

    if let Some((want_output, want_diagnostics)) = &expected.format {
        problems.extend(compare_format(source, want_output, want_diagnostics));
    }

    problems.extend(compare_run(
        source, expected, &checked, modules, config, limits,
    ));
    problems
}

fn compare_format(
    source: &str,
    want: &ExpectedFormat,
    want_diagnostics: &[crate::expected::ExpectedDiagnostic],
) -> Vec<String> {
    let mut problems = Vec::new();
    let formatted = hollowscript::format(source, ENTRY, &mut Control::none());
    problems.extend(compare::diagnostics(
        "format",
        &formatted.diagnostics,
        want_diagnostics,
    ));
    match (want, formatted.output.as_deref()) {
        (ExpectedFormat::Refused, None) => {}
        (ExpectedFormat::Refused, Some(_)) => {
            problems.push("format: expected the formatter to refuse, it produced text".to_owned());
        }
        (_, None) => {
            problems.push("format: the formatter refused, the case expects text".to_owned());
        }
        (ExpectedFormat::Same, Some(text)) => {
            if text != source {
                problems.push(format!(
                    "format: expected the source unchanged, got {text:?}"
                ));
            }
        }
        (ExpectedFormat::Lines(lines), Some(text)) => {
            let want_text = lines.join("\n");
            if text != want_text {
                problems.push(format!("format: expected {want_text:?}, got {text:?}"));
            }
        }
    }
    // "In addition, formatting the formatted text must reproduce it exactly
    // and must succeed."
    if let Some(text) = formatted.output.as_deref() {
        let again = hollowscript::format(text, ENTRY, &mut Control::none());
        match again.output.as_deref() {
            Some(second) if second == text => {}
            Some(second) => problems.push(format!(
                "format: formatting the formatted text changed it, to {second:?}"
            )),
            None => problems.push("format: the formatted text does not format".to_owned()),
        }
    }
    problems
}

fn compare_run(
    source: &str,
    expected: &Expected,
    checked: &[hollowscript::Diagnostic],
    modules: BTreeMap<String, Vec<u8>>,
    config: &crate::hostconfig::HostConfig,
    limits: hollowscript::Limits,
) -> Vec<String> {
    let mut problems = Vec::new();
    let output = host::Output::default();
    let options = Options {
        limits,
        capabilities: host::capabilities(config),
        filter_schema: config.filter_schema.clone(),
        // The corpus fixes what a synchronous host sees; the batched path
        // has its own tests in the VM.
        batch_capability_calls: false,
        gc_stress_mode: crate::gc_stress_enabled(),
    };
    let outcome = hollowscript::load(
        source,
        ENTRY,
        &options,
        &mut ModuleLoader { modules },
        &mut |name, function| host::bind(name, function, &output, config),
        &mut Control::none(),
    );
    match (&expected.run, outcome) {
        (ExpectedRun::Completed { .. }, LoadOutcome::CheckFailed(diagnostics)) => {
            problems.push(format!(
                "run: expected the program to run, it stopped at the check stage: [{}]",
                codes(&diagnostics)
            ));
        }
        (
            ExpectedRun::Stopped {
                stage,
                output: want_output,
                diagnostics: want,
            },
            LoadOutcome::CheckFailed(diagnostics),
        ) => {
            if stage != "check" {
                problems.push(format!(
                    "run: expected the program to stop at the {stage} stage, it stopped at check"
                ));
            }
            problems.extend(compare::output(&output.borrow(), want_output));
            // The check stage's own list is what a check-stage stop reports.
            problems.extend(compare::diagnostics("run", checked, want));
            let _ = diagnostics;
        }
        (want, LoadOutcome::Instance(mut instance)) => {
            let fault = instance.fault().cloned();
            let result = instance.result();
            let rendered = match result {
                Some(LoadResult::Value(value)) => Some(instance.render(value)),
                Some(LoadResult::NoValue) | None => None,
            };
            match want {
                ExpectedRun::Completed {
                    output: want_output,
                    result: want_result,
                } => {
                    if let Some(fault) = &fault {
                        problems.push(format!(
                            "run: expected the program to complete, it faulted with {}",
                            fault.to_diagnostic().code
                        ));
                    }
                    problems.extend(compare::output(&output.borrow(), want_output));
                    if rendered.as_deref() != want_result.as_deref() {
                        problems.push(format!(
                            "run: expected the result {want_result:?}, got {rendered:?}"
                        ));
                    }
                }
                ExpectedRun::Stopped {
                    stage,
                    output: want_output,
                    diagnostics: want,
                } => {
                    if stage != "run" {
                        problems.push(format!(
                            "run: expected the program to stop at the {stage} stage, it ran"
                        ));
                    }
                    problems.extend(compare::output(&output.borrow(), want_output));
                    let produced: Vec<hollowscript::Diagnostic> = fault
                        .iter()
                        .map(hollowscript::Fault::to_diagnostic)
                        .collect();
                    problems.extend(compare::diagnostics("run", &produced, want));
                }
            }
            let _ = instance.dispose();
        }
    }
    problems
}

fn codes(diagnostics: &[hollowscript::Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
