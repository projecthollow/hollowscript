//! The `hollowscript` command: `check`, `format`, `run`, `build` and `lsp`,
//! with a `--json` mode used by the conformance tooling
//! (`corpus/FORMAT.md`).
//!
//! `check` and `format` run the front of the pipeline: [`hollowscript::check`]
//! and, for `format`, `hollowscript_syntax::format` directly. `run` goes the
//! whole way, through [`crate::execute`], which checks, compiles, verifies
//! and interprets the program (or verifies and interprets a container that
//! `build` wrote); this module only decides how to print what that gives
//! back. Every write goes through [`crate::stdio`], so a closed pipe or a
//! full disk is never a panic.
//!
//! `--allow` is read by [`crate::granted::Granted`], which decides what one
//! run may reach, and the same decision reaches both `check` (through the
//! capability interface, so an ungranted `uses` is HS0404) and `run`
//! (through the bound functions).

#![forbid(unsafe_code)]

// `capabilities` and `civil` implement the standard capabilities and their
// calendar support, and `default_grants` binds them. A few helpers here
// (`print`, `args`, `date_of`, `weekday_of` and their calendar support) are
// tested but have no caller, because `default_grants` binds its own
// versions of those operations.
#[allow(
    dead_code,
    reason = "tested helpers that default_grants does not call; see the comment above"
)]
mod capabilities;
#[allow(
    dead_code,
    reason = "calendar support for the uncalled helpers in `capabilities`"
)]
mod civil;
mod cli;
mod default_grants;
mod execute;
mod granted;
mod grants;
mod http;
mod json;
mod loader;
mod random;
mod stdio;

use cli::{Command, Invocation, parse_args};
use default_grants::{PrintSink, capabilities_for};
use granted::Granted;
use hollowscript::Diagnostic;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                stdio::err_line(&format!(
                    "hollowscript: the argument '{}' is not valid UTF-8",
                    arg.to_string_lossy()
                ));
                return ExitCode::from(2);
            }
        }
    }
    let code = run(&parse_args(&args));
    ExitCode::from(match stdio::problem() {
        // The reader closed standard output: it has seen all it wants,
        // which is not an error of this command's (see `stdio`).
        None | Some(stdio::Problem::Closed) => code,
        Some(stdio::Problem::Failed(e)) => {
            stdio::err_line(&format!(
                "hollowscript: cannot write to standard output: {e}"
            ));
            code.max(1)
        }
    })
}

/// Runs one invocation and returns a process exit code: 0 for success, 1 for
/// a program with an error or a run that could not complete, 2 for a problem
/// with the invocation itself (a bad flag, a file that could not be read).
fn run(invocation: &Invocation) -> u8 {
    let json = invocation.json;
    let granted = match Granted::from_flags(&invocation.grants, &invocation.program_args) {
        Ok(granted) => granted,
        Err(message) => return invocation_error(json, &message),
    };
    match &invocation.command {
        Command::Help(topic) => {
            stdio::raw(cli::usage(*topic));
            0
        }
        Command::Usage => {
            for line in cli::usage(None).lines() {
                stdio::err_line(line);
            }
            2
        }
        Command::Version => {
            stdio::line(&format!("hollowscript {}", env!("CARGO_PKG_VERSION")));
            0
        }
        Command::Error(message) => invocation_error(json, message),
        Command::Lsp => run_lsp(),
        Command::Check { file } => run_check(file, json, &granted),
        Command::Format { file } => run_format(file, json),
        Command::Run { file } => run_run(file, json, granted),
        Command::Build { file, out } => run_build(file, out.as_deref(), json, &granted),
    }
}

/// Reports a problem with the invocation itself (a bad option, a file that
/// cannot be read or written) and answers exit code 2. With `--json` it is
/// one document, `{"error": "..."}`, on standard output, so a tool reading
/// the output always gets JSON; otherwise a line on standard error.
fn invocation_error(json: bool, message: &str) -> u8 {
    if json {
        let mut out = String::from(r#"{"error":""#);
        json::escape_json_string(message, &mut out);
        out.push_str(r#""}"#);
        stdio::line(&out);
    } else {
        stdio::err_line(&format!("hollowscript: {message}"));
    }
    2
}

/// What reading a program's file gave.
enum Read {
    /// Source text.
    Source(String),
    /// A bytecode container (only `run` accepts one).
    Bytecode(Vec<u8>),
    /// Bytes that are not valid source: not UTF-8 (HS0001) or too large
    /// (HS0002). Reported like any other diagnostic of the program, with
    /// exit code 1, not as a problem with the invocation.
    Invalid(Diagnostic),
}

/// Reads `file`, or reports why it could not be read (exit code 2).
fn read_program(file: &str, json: bool) -> Result<Read, u8> {
    let bytes = std::fs::read(file)
        .map_err(|e| invocation_error(json, &format!("cannot read '{file}': {e}")))?;
    if hollowscript::is_bytecode(&bytes) {
        return Ok(Read::Bytecode(bytes));
    }
    let program = loader::Program::at(file);
    let entry = std::sync::Arc::from(program.entry());
    match hollowscript_core::Source::from_bytes(
        hollowscript_core::SourceId(0),
        program.entry(),
        &bytes,
    ) {
        Ok(source) => Ok(Read::Source(source.text().to_owned())),
        Err(e) => Ok(Read::Invalid(e.to_diagnostic(&entry))),
    }
}

/// [`read_program`] for a command that only takes source text: a container
/// is a problem with the invocation.
fn read_source(file: &str, json: bool) -> Result<Result<String, Diagnostic>, u8> {
    match read_program(file, json)? {
        Read::Source(text) => Ok(Ok(text)),
        Read::Invalid(diagnostic) => Ok(Err(diagnostic)),
        Read::Bytecode(_) => Err(invocation_error(
            json,
            &format!("'{file}' is a bytecode container, not source; only 'run' takes one"),
        )),
    }
}

fn exit_for(diagnostics: &[Diagnostic]) -> u8 {
    u8::from(diagnostics.iter().any(Diagnostic::is_error))
}

/// `lsp`: runs `hollowscript-lsp` as a child process, standard input and
/// output connected straight through, and finishes with its exit code.
/// `hollowscript-cli` does not depend on the `hollowscript-lsp` crate
/// (`docs/design/architecture.md` documents the language server as
/// independent of the facade, and `xtask graph` checks it), so this is the
/// only way this command can start it: a separate binary, not a library
/// call. `HOLLOWSCRIPT_LSP_PATH` overrides where that binary is, for a
/// build where the two are not installed next to each other; otherwise the
/// search is the sibling of this executable's own path (where `cargo
/// build` and an installed release both put it), and failing that, `PATH`.
fn run_lsp() -> u8 {
    let path = lsp_binary_path();
    match std::process::Command::new(&path).status() {
        Ok(status) => u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
        Err(e) => {
            stdio::err_line(&format!(
                "hollowscript: cannot start the language server at '{}': {e}",
                path.display()
            ));
            2
        }
    }
}

/// Where to find the `hollowscript-lsp` binary, as [`run_lsp`] describes.
fn lsp_binary_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("HOLLOWSCRIPT_LSP_PATH") {
        return std::path::PathBuf::from(path);
    }
    if let Ok(current) = std::env::current_exe() {
        let name = if current.extension().is_some_and(|ext| ext == "exe") {
            "hollowscript-lsp.exe"
        } else {
            "hollowscript-lsp"
        };
        if let Some(sibling) = current.parent().map(|dir| dir.join(name)) {
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from("hollowscript-lsp")
}

fn run_check(file: &str, json: bool, granted: &Granted) -> u8 {
    let program = loader::Program::at(file);
    let diagnostics = match read_source(file, json) {
        Err(code) => return code,
        Ok(Err(diagnostic)) => vec![diagnostic],
        Ok(Ok(source)) => hollowscript::check(
            &source,
            program.entry(),
            &capabilities_for(granted),
            None,
            &mut program.loader(),
            &mut hollowscript::Control::none(),
        ),
    };
    if json {
        stdio::line(&json::check_document(&diagnostics));
    } else {
        print_diagnostics_human(&program, &diagnostics);
    }
    exit_for(&diagnostics)
}

/// `build FILE [-o OUT]`: compiles a program to a bytecode container and
/// writes it, without running anything.
///
/// This is what puts a program where a virtual machine can pick it up
/// later: the browser VM (`hollowscript-wasm`) verifies and runs a
/// container with no parser and no checker of its own, and `run` takes
/// one too. The grants given here are the capability interface the
/// program is compiled against, so a program built for a host that grants
/// `fs` must be built with `--allow fs`.
fn run_build(file: &str, out: Option<&str>, json: bool, granted: &Granted) -> u8 {
    let program = loader::Program::at(file);
    let source = match read_source(file, json) {
        Err(code) => return code,
        Ok(Err(diagnostic)) => return report_check_failed(&program, json, &[diagnostic]),
        Ok(Ok(source)) => source,
    };
    let options = hollowscript::Options {
        limits: hollowscript::Limits::default(),
        capabilities: capabilities_for(granted),
        filter_schema: None,
        batch_capability_calls: false,
        gc_stress_mode: false,
    };
    let outcome = hollowscript::build(
        &source,
        program.entry(),
        &options,
        &mut program.loader(),
        &mut hollowscript::Control::none(),
    );
    let bytes = match outcome {
        hollowscript::BuildOutcome::Bytecode(bytes) => bytes,
        hollowscript::BuildOutcome::CheckFailed(diagnostics) => {
            return report_check_failed(&program, json, &diagnostics);
        }
    };
    let target = out.map_or_else(|| default_output(file), str::to_owned);
    if let Err(e) = std::fs::write(&target, &bytes) {
        return invocation_error(json, &format!("cannot write '{target}': {e}"));
    }
    if json {
        let mut doc = format!(r#"{{"bytes":{},"output":""#, bytes.len());
        json::escape_json_string(&target, &mut doc);
        doc.push_str(r#""}"#);
        stdio::line(&doc);
    } else {
        stdio::line(&format!("wrote {} ({} bytes)", target, bytes.len()));
    }
    0
}

/// `check`'s own report, for `build` when the program has an error.
fn report_check_failed(program: &loader::Program, json: bool, diagnostics: &[Diagnostic]) -> u8 {
    if json {
        stdio::line(&json::check_document(diagnostics));
    } else {
        print_diagnostics_human(program, diagnostics);
    }
    exit_for(diagnostics)
}

/// The source file's own name with `.hsb` in place of its extension, which
/// is where `build` writes when no `-o` says otherwise.
fn default_output(file: &str) -> String {
    let path = std::path::Path::new(file);
    path.with_extension("hsb").to_string_lossy().into_owned()
}

fn run_format(file: &str, json: bool) -> u8 {
    let program = loader::Program::at(file);
    let source = match read_source(file, json) {
        Err(code) => return code,
        Ok(Err(d)) => {
            if json {
                stdio::line(&json::format_document(None, std::slice::from_ref(&d)));
            } else {
                print_diagnostics_human(&program, std::slice::from_ref(&d));
            }
            return 1;
        }
        Ok(Ok(source)) => source,
    };
    let mut interner = hollowscript_core::Interner::new();
    let src = match hollowscript_core::Source::new(hollowscript_core::SourceId(0), file, source) {
        Ok(s) => s,
        Err(e) => {
            let d = e.to_diagnostic(&std::sync::Arc::from(file));
            if json {
                stdio::line(&json::format_document(None, std::slice::from_ref(&d)));
            } else {
                print_diagnostics_human(&program, std::slice::from_ref(&d));
            }
            return 1;
        }
    };
    let mut control = hollowscript_core::Control::none();
    let formatted = hollowscript_syntax::format(&src, &mut interner, &mut control);
    if json {
        stdio::line(&json::format_document(
            formatted.output.as_deref(),
            &formatted.diagnostics,
        ));
    } else if let Some(text) = &formatted.output {
        stdio::raw(text);
    } else {
        print_diagnostics_human(&program, &formatted.diagnostics);
    }
    u8::from(formatted.output.is_none())
}

/// `run FILE`: source text, or a container `build` wrote (recognised by its
/// magic number, whatever the file is called).
///
/// Without `--json`, each `print` is written to standard output as it
/// happens; with it, what the program prints is collected and reported in
/// the one document written when the run ends, so the output stays a
/// single JSON value.
fn run_run(file: &str, json: bool, granted: Granted) -> u8 {
    let program = loader::Program::at(file);
    let sink = if json {
        PrintSink::collect()
    } else {
        PrintSink::stream()
    };
    let limits = hollowscript::Limits::default();
    let result = match read_program(file, json) {
        Err(code) => return code,
        Ok(Read::Invalid(diagnostic)) => execute::RunResult {
            ok: false,
            output: Vec::new(),
            diagnostics: vec![diagnostic],
            fault: None,
        },
        Ok(Read::Source(source)) => execute::execute_to(&source, file, granted, limits, &sink),
        Ok(Read::Bytecode(bytes)) => {
            match execute::execute_bytecode(&bytes, granted, limits, &sink) {
                Ok(result) => result,
                Err(e) => {
                    return invocation_error(
                        json,
                        &format!("'{file}' is not a container this version can run: {e}"),
                    );
                }
            }
        }
    };
    if !result.diagnostics.is_empty() {
        if json {
            stdio::line(&json_run_check_failed(&result.diagnostics));
        } else {
            print_diagnostics_human(&program, &result.diagnostics);
        }
        return 1;
    }
    if stdio::problem().is_some() {
        // Standard output went away part way through and the run was
        // stopped at its next `print`; `main` decides what, if anything, to
        // say about that. The fault that stopped it is this command's own
        // doing, not the program's.
        return 0;
    }
    if json {
        stdio::line(&json_run_result(
            result.ok,
            &result.output,
            &[],
            result.fault.as_ref(),
        ));
    } else if let Some(fault) = &result.fault {
        print_fault_human(&program, fault);
    }
    u8::from(!result.ok)
}

fn json_run_result(
    ok: bool,
    printed: &[String],
    diagnostics: &[Diagnostic],
    fault: Option<&hollowscript::Fault>,
) -> String {
    // A run that did not complete says which stage stopped it, as a check
    // failure does ("stage":"check"): here it is always the run itself.
    let stage = if ok { "" } else { r#","stage":"run""# };
    let mut out = format!(r#"{{"ok":{ok}{stage},"output":["#);
    for (i, line) in printed.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        json::escape_json_string(line, &mut out);
        out.push('"');
    }
    out.push_str(r#"],"diagnostics":"#);
    let fault_diagnostic = fault.map(hollowscript::Fault::to_diagnostic);
    let all: Vec<Diagnostic> = diagnostics
        .iter()
        .cloned()
        .chain(fault_diagnostic)
        .collect();
    json::diagnostics_array(&all, &mut out);
    out.push('}');
    out
}

fn json_run_check_failed(diagnostics: &[Diagnostic]) -> String {
    let mut out = String::from(r#"{"ok":false,"stage":"check","output":[],"diagnostics":"#);
    json::diagnostics_array(diagnostics, &mut out);
    out.push('}');
    out
}

/// One diagnostic as a person reads it: `FILE:LINE:COLUMN: severity[code]:
/// message`, the file named by the path it has on disk rather than the
/// canonical path the language works in (`main.hws`, `sub/util.hws`), so
/// that it matches what was typed.
fn human_line(program: &loader::Program, d: &Diagnostic) -> String {
    match &d.location {
        Some(l) => format!(
            "{}:{}:{}: {}[{}]: {}",
            program.display(&l.file),
            l.line,
            l.column,
            d.severity.as_str(),
            d.code,
            d.message
        ),
        None => format!("{}[{}]: {}", d.severity.as_str(), d.code, d.message),
    }
}

/// Prints `check`'s diagnostics for a person, on standard output (they are
/// what `check` was asked for).
fn print_diagnostics_human(program: &loader::Program, diagnostics: &[Diagnostic]) {
    for d in diagnostics {
        if !stdio::line(&human_line(program, d)) {
            return;
        }
    }
}

/// Prints a runtime fault for a person, on standard error (standard output
/// is the program's own), in the same form as a diagnostic, followed by the
/// calls it happened inside, innermost first.
fn print_fault_human(program: &loader::Program, fault: &hollowscript::Fault) {
    let diagnostic = fault.to_diagnostic();
    stdio::err_line(&human_line(program, &diagnostic));
    for frame in &fault.frames {
        let l = &frame.location;
        stdio::err_line(&format!(
            "    in {} at {}:{}:{}",
            frame.function,
            program.display(&l.file),
            l.line,
            l.column
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, Invocation, run};

    #[test]
    fn help_and_version_exit_successfully() {
        let inv = Invocation {
            command: Command::Help(None),
            json: false,
            grants: vec![],
            program_args: vec![],
        };
        assert_eq!(run(&inv), 0);
        let inv = Invocation {
            command: Command::Version,
            json: false,
            grants: vec![],
            program_args: vec![],
        };
        assert_eq!(run(&inv), 0);
    }

    #[test]
    fn a_missing_file_exits_with_two_not_a_panic() {
        let inv = Invocation {
            command: Command::Check {
                file: "/no/such/file.hws".into(),
            },
            json: false,
            grants: vec![],
            program_args: vec![],
        };
        assert_eq!(run(&inv), 2);
    }

    #[test]
    fn checking_a_clean_file_succeeds() {
        let dir =
            std::env::temp_dir().join(format!("hollowscript-cli-main-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.hws");
        std::fs::write(&file, "return 1\n").unwrap();
        let inv = Invocation {
            command: Command::Check {
                file: file.to_string_lossy().into_owned(),
            },
            json: true,
            grants: vec![],
            program_args: vec![],
        };
        assert_eq!(run(&inv), 0, "a clean program has no check errors");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn running_a_clean_program_executes_it_end_to_end() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-main-run-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.hws");
        std::fs::write(&file, "uses print\neffect print(\"hello\")\nreturn 1 + 2\n").unwrap();
        let inv = Invocation {
            command: Command::Run {
                file: file.to_string_lossy().into_owned(),
            },
            json: true,
            grants: vec![],
            program_args: vec![],
        };
        assert_eq!(
            run(&inv),
            0,
            "a clean program that prints and returns succeeds"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
