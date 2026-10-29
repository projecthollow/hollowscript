//! The examples of the specification itself, run as conformance material.
//!
//! Section 19.6 makes every code block tagged `hollow`, `hollow-error`,
//! `hollow-fault`, `hollow-module` or `hollow-module-error` part of the
//! conformance material, with a meaning per tag:
//!
//! - `hollow`: the checker accepts it with no errors, and when an `Output:`
//!   block follows, running it with the corpus host prints exactly that;
//! - `hollow-error`: the checker rejects it, the line carrying `// HSnnnn`
//!   is where the diagnostic with that code is, and no other error is
//!   reported;
//! - `hollow-fault`: the checker accepts it and running it faults, with the
//!   comment naming the code and marking the line;
//! - `hollow-module` and `hollow-module-error`: one file each of a program
//!   of several files, named by its first line (`// file: name.hws`).
//!   Consecutive blocks form one program, and a block naming `main.hws`
//!   starts the next one.
//!
//! `hollow-fragment` blocks are excerpts and are not run. Nothing in the
//! prose around a block is read except an `Output` line ending in `:`
//! directly before the output block, so a result the text states in words
//! and a grant the text names are not judged here; an example that needs a
//! grant the corpus host does not give fails and belongs in the baseline.
//!
//! An example's id is the number of the section it appears in and its
//! position within that section, `5.4/1`, which survives edits elsewhere in
//! the chapter. (Not `#`, which starts a comment in a baseline file.)

use crate::case::{ENTRY, ModuleLoader};
use crate::host;
use crate::report::Report;
use hollowscript::{Control, Diagnostic, LoadOutcome, Options};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What section 19.6 says the implementation must do with an example.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `hollow` or `hollow-module`: accepted, and runs without a fault.
    Accepts,
    /// `hollow-error` or `hollow-module-error`: rejected.
    Rejects,
    /// `hollow-fault`: accepted, and faults when run.
    Faults,
}

/// A `// HSnnnn` comment: the file and line it is on, and the code.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Marker {
    file: String,
    line: u32,
    code: String,
}

/// One example program, already assembled from its block or blocks.
#[derive(Debug)]
pub(crate) struct Example {
    /// `5.4/1`: the section and the example's position within it.
    pub id: String,
    where_: String,
    kind: Kind,
    /// Every file of the program by canonical path, the entry `main.hws`.
    files: BTreeMap<String, String>,
    markers: Vec<Marker>,
    /// The `text` block after an `Output:` line, when there is one.
    output: Option<String>,
}

/// A fenced block as found in a chapter.
struct Block {
    tag: String,
    body: String,
    /// The line of the opening fence, 1-based, for a report.
    line: usize,
    /// The number of the section the block is in.
    section: String,
    /// Whether the last non-blank line before the fence is an `Output` line
    /// ending in `:`.
    after_output_line: bool,
}

/// Reads every example under `spec_dir`, chapter by chapter in file name
/// order.
pub(crate) fn discover(spec_dir: &Path) -> std::io::Result<Vec<Example>> {
    let mut chapters: Vec<_> = std::fs::read_dir(spec_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .collect();
    chapters.sort();
    let mut examples = Vec::new();
    for chapter in chapters {
        let text = std::fs::read_to_string(&chapter)?;
        let name = chapter
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        examples.extend(examples_of(&name, &blocks_of(&text)));
    }
    Ok(examples)
}

/// The fenced blocks of one chapter, each with the section it is in.
fn blocks_of(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut section = String::new();
    let mut last_prose = String::new();
    let mut lines = text.lines().enumerate();
    while let Some((index, line)) = lines.next() {
        if let Some(heading) = line.strip_prefix('#') {
            let title = heading.trim_start_matches('#').trim();
            if let Some(number) = title.split_whitespace().next() {
                if number.starts_with(|c: char| c.is_ascii_digit()) {
                    number.trim_end_matches('.').clone_into(&mut section);
                }
            }
        }
        let Some(tag) = line.strip_prefix("```") else {
            if !line.trim().is_empty() {
                line.trim().clone_into(&mut last_prose);
            }
            continue;
        };
        let mut body = String::new();
        for (_, inner) in lines.by_ref() {
            if inner.starts_with("```") {
                break;
            }
            body.push_str(inner);
            body.push('\n');
        }
        blocks.push(Block {
            tag: tag.trim().to_owned(),
            body,
            line: index + 1,
            section: section.clone(),
            after_output_line: last_prose.starts_with("Output") && last_prose.ends_with(':'),
        });
        last_prose.clear();
    }
    blocks
}

/// The examples the blocks of one chapter make.
fn examples_of(chapter: &str, blocks: &[Block]) -> Vec<Example> {
    let mut examples: Vec<Example> = Vec::new();
    let mut ordinal: BTreeMap<String, usize> = BTreeMap::new();
    let mut open_module = false;
    for block in blocks {
        let kind = match block.tag.as_str() {
            "hollow" | "hollow-module" => Kind::Accepts,
            "hollow-error" | "hollow-module-error" => Kind::Rejects,
            "hollow-fault" => Kind::Faults,
            "text" => {
                if block.after_output_line {
                    if let Some(last) = examples.last_mut() {
                        if last.output.is_none() {
                            last.output = Some(block.body.clone());
                        }
                    }
                }
                open_module = false;
                continue;
            }
            _ => {
                open_module = false;
                continue;
            }
        };
        let module = block.tag.starts_with("hollow-module");
        let file = if module {
            block
                .body
                .lines()
                .next()
                .and_then(|first| first.trim().strip_prefix("// file:"))
                .map_or_else(|| ENTRY.to_owned(), |name| name.trim().to_owned())
        } else {
            ENTRY.to_owned()
        };
        let markers = markers_of(&file, &block.body);
        // A module block joins the program before it, unless it is a new
        // entry module or nothing is open.
        if module && open_module && file != ENTRY {
            if let Some(last) = examples.last_mut() {
                last.files.insert(file, block.body.clone());
                last.markers.extend(markers);
                if kind == Kind::Rejects {
                    last.kind = Kind::Rejects;
                }
                continue;
            }
        }
        let count = ordinal.entry(block.section.clone()).or_insert(0);
        *count += 1;
        examples.push(Example {
            id: format!("{}/{count}", block.section),
            where_: format!("{chapter}:{}", block.line),
            kind,
            files: BTreeMap::from([(file, block.body.clone())]),
            markers,
            output: None,
        });
        open_module = module;
    }
    examples
}

/// Every `// HSnnnn` comment of one file, with its 1-based line.
fn markers_of(file: &str, body: &str) -> Vec<Marker> {
    let mut out = Vec::new();
    for (index, line) in body.lines().enumerate() {
        let Some((_, comment)) = line.split_once("//") else {
            continue;
        };
        let comment = comment.trim();
        let is_code = comment.len() == 6
            && comment.starts_with("HS")
            && comment[2..].bytes().all(|b| b.is_ascii_digit());
        if is_code {
            out.push(Marker {
                file: file.to_owned(),
                line: u32::try_from(index + 1).unwrap_or(u32::MAX),
                code: comment.to_owned(),
            });
        }
    }
    out
}

/// Runs every example against `baseline`, in the shape the corpus gate
/// reports.
pub(crate) fn run(examples: &[Example], baseline: BTreeSet<String>) -> Report {
    let results = examples
        .iter()
        .map(|example| {
            let problems = judge(example);
            let outcome = if problems.is_empty() {
                crate::case::Outcome::Passed
            } else {
                crate::case::Outcome::Failed(
                    std::iter::once(format!("at {}", example.where_))
                        .chain(problems)
                        .collect(),
                )
            };
            (example.id.clone(), outcome)
        })
        .collect();
    Report { results, baseline }
}

/// The fixed grants and host files a handful of examples need to run at
/// all, keyed by example id (section 19.4: what a `<name>.host.json` gives
/// a corpus case, given here instead because a specification example has
/// no file of its own beside it). Nothing in the specification's prose is
/// read to build this table: it is a fact about this runner's corpus host,
/// recorded here and in `corpus/FORMAT.md`, the same principled and
/// documented way `<name>.host.json` gives a real case what it needs,
/// rather than an undocumented convention read out of the specification
/// text itself.
fn config_for(example_id: &str) -> crate::hostconfig::HostConfig {
    let mut config = crate::hostconfig::HostConfig::default();
    match example_id {
        // "Run with the grant fs:write=notes and an existing notes
        // directory" (section 16.8): the directory is implied by seeding
        // an empty file inside it (`corpus/FORMAT.md`: "directories are
        // implied by their files").
        "16.8/1" => {
            config.apply_grants(&["fs:write=notes".to_owned()]);
            config
                .files
                .borrow_mut()
                .insert(crate::hostconfig::normalize("notes/.keep"), String::new());
        }
        // "With fs:read=data this returns Ok of the number of lines of
        // data/input.txt, and Err(...) if absent" (section 16.8): both
        // outcomes complete without a fault, so no file is needed to make
        // the example runnable, only the grant.
        "16.8/2" => config.apply_grants(&["fs:read=data".to_owned()]),
        // "Run with http=api.example.com" (section 16.9): the example
        // handles every HttpError generically, including the Failed a
        // request with no canned exchange gets, so no exchange is needed
        // either.
        "16.9/1" => config.apply_grants(&["http=api.example.com".to_owned()]),
        _ => {}
    }
    config
}

/// The errors of the parse and check stages together: section 19.6 says
/// what is reported, whichever stage reports it.
fn errors(example: &Example) -> Vec<Diagnostic> {
    let Some(source) = example.files.get(ENTRY) else {
        return Vec::new();
    };
    let config = config_for(&example.id);
    let mut out: Vec<Diagnostic> =
        hollowscript::parse_diagnostics(source, ENTRY, &mut Control::none());
    let checked = hollowscript::check(
        source,
        ENTRY,
        &host::capabilities(&config),
        None,
        &mut loader(example),
        &mut Control::none(),
    );
    for diagnostic in checked {
        let already = out
            .iter()
            .any(|d| d.code == diagnostic.code && position(d) == position(&diagnostic));
        if !already {
            out.push(diagnostic);
        }
    }
    out.retain(|d| d.severity == hollowscript::Severity::Error);
    out
}

fn loader(example: &Example) -> ModuleLoader {
    ModuleLoader {
        modules: example
            .files
            .iter()
            .filter(|(name, _)| name.as_str() != ENTRY)
            .map(|(name, body)| (name.clone(), body.clone().into_bytes()))
            .collect(),
    }
}

/// A diagnostic's file and line, with an unplaced one at line 0 of the
/// entry module.
fn position(diagnostic: &Diagnostic) -> (String, u32) {
    diagnostic
        .location
        .as_ref()
        .map_or_else(|| (ENTRY.to_owned(), 0), |l| (l.file.to_string(), l.line))
}

fn describe(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| {
            let (file, line) = position(d);
            format!("{} at {file}:{line} ({})", d.code, d.message)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Every way `example` departs from what section 19.6 says of it.
fn judge(example: &Example) -> Vec<String> {
    if !example.files.contains_key(ENTRY) {
        return vec!["the program has no main.hws".to_owned()];
    }
    let errors = errors(example);
    match example.kind {
        Kind::Rejects => judge_rejected(example, &errors),
        Kind::Accepts | Kind::Faults if !errors.is_empty() => vec![format!(
            "expected the checker to accept it, it reported: {}",
            describe(&errors)
        )],
        Kind::Accepts | Kind::Faults => judge_run(example),
    }
}

fn judge_rejected(example: &Example, errors: &[Diagnostic]) -> Vec<String> {
    let mut problems = Vec::new();
    if errors.is_empty() {
        problems.push("expected the checker to reject it, it accepted it".to_owned());
        return problems;
    }
    for marker in &example.markers {
        let found = errors.iter().any(|d| {
            d.code.to_string() == marker.code && position(d) == (marker.file.clone(), marker.line)
        });
        if !found {
            problems.push(format!(
                "expected {} at {}:{}, got: {}",
                marker.code,
                marker.file,
                marker.line,
                describe(errors)
            ));
        }
    }
    if !example.markers.is_empty() {
        let unexpected: Vec<Diagnostic> = errors
            .iter()
            .filter(|d| {
                !example.markers.iter().any(|m| {
                    d.code.to_string() == m.code && position(d) == (m.file.clone(), m.line)
                })
            })
            .cloned()
            .collect();
        if !unexpected.is_empty() {
            problems.push(format!(
                "section 19.6 allows no other error, got: {}",
                describe(&unexpected)
            ));
        }
    }
    problems
}

fn judge_run(example: &Example) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(source) = example.files.get(ENTRY) else {
        return problems;
    };
    let config = config_for(&example.id);
    let output = host::Output::default();
    let options = Options {
        limits: hollowscript::Limits::default(),
        capabilities: host::capabilities(&config),
        filter_schema: None,
        batch_capability_calls: false,
        gc_stress_mode: crate::gc_stress_enabled(),
    };
    let outcome = hollowscript::load(
        source,
        ENTRY,
        &options,
        &mut loader(example),
        &mut |name, function| host::bind(name, function, &output, &config),
        &mut Control::none(),
    );
    let LoadOutcome::Instance(mut instance) = outcome else {
        problems.push("the program was accepted by check but refused by load".to_owned());
        return problems;
    };
    let fault = instance.fault().map(hollowscript::Fault::to_diagnostic);
    let _ = instance.dispose();
    match (example.kind, fault) {
        (Kind::Faults, None) => problems.push("expected a fault, the program ran".to_owned()),
        (Kind::Faults, Some(fault)) => {
            for marker in &example.markers {
                if fault.code.to_string() != marker.code {
                    problems.push(format!(
                        "expected the fault {}, got {} ({})",
                        marker.code, fault.code, fault.message
                    ));
                } else if position(&fault) != (marker.file.clone(), marker.line) {
                    let (file, line) = position(&fault);
                    problems.push(format!(
                        "expected {} at {}:{}, it was at {file}:{line}",
                        marker.code, marker.file, marker.line
                    ));
                }
            }
        }
        (_, Some(fault)) => problems.push(format!(
            "expected the program to run, it faulted with {} ({})",
            fault.code, fault.message
        )),
        (_, None) => {}
    }
    if let Some(want) = &example.output {
        // One printed entry per line, which is how the specification
        // writes an `Output:` block.
        let got = output.borrow().join("\n");
        if got.trim_end_matches('\n') != want.trim_end_matches('\n') {
            problems.push(format!("expected the output {want:?}, got {got:?}"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::{Kind, blocks_of, examples_of};

    #[test]
    fn blocks_are_grouped_and_numbered_by_section() {
        let text = concat!(
            "## 5.4 Closures\n\n",
            "```hollow\nreturn 1\n```\n\n",
            "Output:\n\n```text\n1\n```\n\n",
            "```hollow-error\nreturn x // HS0205\n```\n\n",
            "### 10.2 Modules\n\n",
            "```hollow-module\n// file: main.hws\nimport a from \"./a\"\nreturn a\n```\n\n",
            "```hollow-module\n// file: a.hws\nexport constant a = 1\n```\n\n",
            "```hollow-module-error\n// file: main.hws\nimport a from \"./main\" // HS0603\n```\n\n",
            "```hollow-fragment\nx\n```\n",
        );
        let examples = examples_of("t.md", &blocks_of(text));
        let ids: Vec<&str> = examples.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["5.4/1", "5.4/2", "10.2/1", "10.2/2"]);
        assert_eq!(examples[0].output.as_deref(), Some("1\n"));
        assert_eq!(examples[1].kind, Kind::Rejects);
        assert_eq!(examples[1].markers[0].line, 1);
        assert_eq!(examples[2].files.len(), 2);
        assert_eq!(examples[3].kind, Kind::Rejects);
        assert_eq!(examples[3].markers[0].code, "HS0603");
    }

    #[test]
    fn a_text_block_without_an_output_line_is_not_output() {
        let text = "## 1.1 A\n\n```hollow\nreturn 1\n```\n\nThe grammar:\n\n```text\nx\n```\n";
        let examples = examples_of("t.md", &blocks_of(text));
        assert!(examples[0].output.is_none());
    }
}
