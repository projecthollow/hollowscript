//! End-to-end tests of the `hollowscript` executable itself: what reaches
//! standard output and standard error, and the exit status, for the cases
//! that only show up in a real process (a closed pipe, output that must
//! arrive before input is read, arguments as a shell passes them).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_hollowscript");

/// A fresh directory for one test, removed when dropped.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let dir =
            std::env::temp_dir().join(format!("hollowscript-cli-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> &Dir {
        std::fs::write(self.0.join(name), contents).unwrap();
        self
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the command in `dir` with no standard input and waits for it.
fn run(dir: &Dir, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

const MANY_LINES: &str = "uses print\n\nvariable i = 0\n\nwhile i < 100000 {\n    effect print(\"line ${i}\")\n    i = i + 1\n}\n";

#[cfg(any(unix, windows))]
#[test]
fn a_hard_linked_import_has_a_portable_diagnostic_and_is_not_read() {
    let dir = Dir::new("hardlink-diagnostic");
    std::fs::create_dir(dir.path().join("imports")).unwrap();
    dir.write(
        "outside.hws",
        "export constant greeting = \"single link\"\n",
    );
    dir.write(
        "imports/main.hws",
        "uses print\n\nimport greeting from \"./shared\"\n\neffect print(greeting)\n",
    );
    std::fs::hard_link(
        dir.path().join("outside.hws"),
        dir.path().join("imports/shared.hws"),
    )
    .unwrap();
    let result = run(&dir, &["run", "imports/main.hws"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(stderr(&result).is_empty());
    assert_eq!(
        stdout(&result).replace("\r\n", "\n"),
        "imports/main.hws:3:22: error[HS0607]: The module 'shared.hws' could not be read: its hard link count is not exactly one, so its name cannot establish the file scope.\n"
    );
    let json = run(&dir, &["run", "--json", "imports/main.hws"]);
    assert_eq!(json.status.code(), Some(1));
    assert!(stdout(&json).contains("\"file\":\"main.hws\""));
    std::fs::remove_file(dir.path().join("imports/shared.hws")).unwrap();
    std::fs::copy(
        dir.path().join("outside.hws"),
        dir.path().join("imports/shared.hws"),
    )
    .unwrap();
    let single = run(&dir, &["run", "imports/main.hws"]);
    assert!(single.status.success());
    assert_eq!(stdout(&single).trim(), "single link");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("outside.hws")).unwrap(),
        "export constant greeting = \"single link\"\n"
    );
}

/// A reader that stops reading (`head -1`) closes the pipe; the command
/// must stop quietly, not panic with "failed printing to stdout" and exit
/// code 101. Standard output's read end is closed before the command has
/// written anything, so every write it makes finds the pipe closed.
#[test]
fn a_closed_standard_output_is_not_a_panic() {
    let dir = Dir::new("pipe");
    dir.write("many.hws", MANY_LINES);
    for args in [
        &["run", "many.hws"][..],
        &["--json", "run", "many.hws"][..],
        &["format", "many.hws"][..],
        &["check", "many.hws"][..],
        &["help"][..],
        &["version"][..],
    ] {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let mut err = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(0), "{args:?}: {err}");
        assert_eq!(err, "", "{args:?}: nothing is said about a closed pipe");
    }
}

/// As above, with the reader taking one line first, which is what
/// `hollowscript run many.hws | head -1` does.
#[test]
fn a_reader_that_stops_after_one_line_stops_the_run_quietly() {
    let dir = Dir::new("head");
    dir.write("many.hws", MANY_LINES);
    let mut child = Command::new(BIN)
        .args(["run", "many.hws"])
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    assert_eq!(first, "line 0\n");
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0), "{err}");
    assert_eq!(err, "");
}

/// `print` reaches standard output as it happens: a question printed before
/// `stdin.readLine()` can be read before any input is given. Before this
/// was fixed, every line was written only after the run ended, so this
/// test would wait for the question forever (and the timeout fails it).
#[test]
fn a_question_is_printed_before_standard_input_is_read() {
    let dir = Dir::new("question");
    dir.write(
        "question.hws",
        concat!(
            "uses print, stdin\n\n",
            "effect print(\"Name?\")\n",
            "match effect stdin.readLine() {\n",
            "    Ok(line) { effect print(\"Thanks.\") }\n",
            "    Err(e) { effect print(\"failed\") }\n",
            "}\n",
        ),
    );
    let mut child = Command::new(BIN)
        .args(["run", "--allow", "stdin", "question.hws"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        tx.send(line).unwrap();
        let mut rest = String::new();
        out.read_to_string(&mut rest).unwrap();
        rest
    });
    let question = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the question must arrive before any input is written");
    assert_eq!(question, "Name?\n");
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"Bob\n").unwrap();
    drop(input);
    assert_eq!(reader.join().unwrap(), "Thanks.\n");
    assert_eq!(child.wait().unwrap().code(), Some(0));
}

/// `run` takes a container `build` wrote, recognised by its magic number,
/// and it runs under the grants it was built with, and not without them.
#[test]
fn run_loads_a_container_that_build_wrote() {
    let dir = Dir::new("hsb");
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    dir.write("data/notes.txt", "hello from a file\n");
    dir.write(
        "app.hws",
        concat!(
            "uses print, fs\n\n",
            "match effect fs.read(\"./data/notes.txt\") {\n",
            "    Ok(text) { effect print(text.trim()) }\n",
            "    Err(e) { effect print(\"failed\") }\n",
            "}\n\n",
            "return 6 * 7\n",
        ),
    );
    let built = run(&dir, &["build", "--allow", "fs:read=./data", "app.hws"]);
    assert_eq!(built.status.code(), Some(0), "{}", stderr(&built));
    let ran = run(&dir, &["run", "--allow", "fs:read=./data", "app.hsb"]);
    assert_eq!(ran.status.code(), Some(0), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "hello from a file\n");

    let json = run(
        &dir,
        &["--json", "run", "--allow", "fs:read=./data", "app.hsb"],
    );
    assert_eq!(
        stdout(&json),
        "{\"ok\":true,\"output\":[\"hello from a file\"],\"diagnostics\":[]}\n"
    );

    let refused = run(&dir, &["run", "app.hsb"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stdout(&refused).contains("HS0404"), "{}", stdout(&refused));

    let bytes = std::fs::read(dir.path().join("app.hsb")).unwrap();
    dir.write("damaged.hsb", &bytes[..bytes.len() / 2]);
    let damaged = run(&dir, &["run", "damaged.hsb"]);
    assert_eq!(damaged.status.code(), Some(2));
    assert!(stderr(&damaged).contains("not a container this version can run"));

    let checked = run(&dir, &["check", "app.hsb"]);
    assert_eq!(checked.status.code(), Some(2));
}

#[test]
fn every_command_has_its_own_help() {
    let dir = Dir::new("help");
    for command in ["check", "format", "run", "build", "lsp"] {
        for args in [[command, "--help"], [command, "-h"], ["help", command]] {
            let out = run(&dir, &args);
            assert_eq!(out.status.code(), Some(0), "{args:?}");
            assert!(
                stdout(&out).starts_with(&format!("Usage: hollowscript {command}")),
                "{args:?}: {}",
                stdout(&out)
            );
            assert_eq!(stderr(&out), "", "{args:?}");
        }
    }
    let general = run(&dir, &["--help"]);
    assert_eq!(general.status.code(), Some(0));
    assert!(stdout(&general).starts_with("Usage: hollowscript <command>"));
    assert!(!stdout(&general).contains("repl"));
}

/// With nothing asked for, the usage goes to standard error and the exit
/// status is 2, like any other invocation that cannot be carried out.
#[test]
fn no_arguments_prints_the_usage_to_standard_error_and_fails() {
    let dir = Dir::new("noargs");
    let out = run(&dir, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).starts_with("Usage: hollowscript <command>"));
}

#[test]
fn repl_is_not_a_command() {
    let dir = Dir::new("repl");
    let out = run(&dir, &["repl"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out),
        "hollowscript: unknown command 'repl'; try 'help'\n"
    );
}

/// A runtime fault names its file, line and column, like a diagnostic does.
#[test]
fn a_runtime_fault_shows_where_it_happened() {
    let dir = Dir::new("fault");
    dir.write(
        "div.hws",
        "uses print\n\nconstant a = 1\n\nconstant b = 0\n\neffect print(\"before\")\n\nreturn a / b\n",
    );
    let out = run(&dir, &["run", "div.hws"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "before\n");
    assert_eq!(
        stderr(&out),
        "div.hws:9:1: error[HS1008]: Division by zero.\n"
    );
}

/// `run --json` names the stage that stopped a run, as a check failure
/// does: "stage":"run" for a runtime fault, "stage":"check" for a program
/// that did not check, and no stage for a run that completed.
#[test]
fn run_json_names_the_stage_that_stopped_the_run() {
    let dir = Dir::new("stage");
    dir.write(
        "div.hws",
        "uses print\n\nconstant a = 1\n\nconstant b = 0\n\neffect print(\"before\")\n\nreturn a / b\n",
    );
    dir.write("bad.hws", "return nope\n");
    dir.write("good.hws", "return 1\n");

    let fault = run(&dir, &["run", "--json", "div.hws"]);
    assert_eq!(fault.status.code(), Some(1));
    assert_eq!(
        stdout(&fault),
        concat!(
            r#"{"ok":false,"stage":"run","output":["before"],"diagnostics":"#,
            r#"[{"severity":"error","line":9,"code":"HS1008","file":"div.hws","message":"Division by zero."}]}"#,
            "\n"
        )
    );

    let check = run(&dir, &["run", "--json", "bad.hws"]);
    assert_eq!(check.status.code(), Some(1));
    assert!(
        stdout(&check).starts_with(r#"{"ok":false,"stage":"check","output":[],"#),
        "{}",
        stdout(&check)
    );

    let ok = run(&dir, &["run", "--json", "good.hws"]);
    assert_eq!(ok.status.code(), Some(0));
    assert!(
        stdout(&ok).starts_with(r#"{"ok":true,"output":[]"#),
        "{}",
        stdout(&ok)
    );
    assert!(!stdout(&ok).contains("stage"), "{}", stdout(&ok));
}

/// A file that is not UTF-8 is the program's HS0001 (section 1.1), exit
/// status 1, in every command and both output modes; it is not a failure
/// to read the file.
#[test]
fn a_file_that_is_not_utf8_is_hs0001() {
    let dir = Dir::new("utf8");
    dir.write("bad.hws", b"return 1\n\xff\n");
    for command in ["check", "format", "run", "build"] {
        let out = run(&dir, &[command, "bad.hws"]);
        assert_eq!(out.status.code(), Some(1), "{command}");
        assert_eq!(
            stdout(&out),
            "bad.hws:2:1: error[HS0001]: The source is not valid UTF-8 (invalid byte at offset 9).\n",
            "{command}"
        );
        let json = run(&dir, &["--json", command, "bad.hws"]);
        assert_eq!(json.status.code(), Some(1), "{command}");
        assert!(
            stdout(&json).contains(r#""code":"HS0001""#),
            "{command}: {}",
            stdout(&json)
        );
    }
}

/// With `--json`, a problem with the invocation itself is JSON too, so a
/// tool reading standard output always gets a document.
#[test]
fn json_mode_reports_invocation_problems_as_json() {
    let dir = Dir::new("jsonio");
    for args in [
        &["--json", "check", "missing.hws"][..],
        &["--json", "run", "missing.hws"][..],
        &["--json", "frobnicate"][..],
        &["--json", "check", "--allow", "", "x.hws"][..],
    ] {
        let out = run(&dir, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let text = stdout(&out);
        assert!(
            text.starts_with(r#"{"error":""#) && text.ends_with("\"}\n"),
            "{args:?}: {text}"
        );
        assert_eq!(stderr(&out), "", "{args:?}");
    }
}

/// An ungranted capability is one diagnostic, not a cascade of errors
/// about its result, and the checker's internal error type never shows.
#[test]
fn an_ungranted_capability_is_one_diagnostic() {
    let dir = Dir::new("cascade");
    dir.write(
        "app.hws",
        concat!(
            "uses print, fs\n\n",
            "match effect fs.read(\"x\") {\n",
            "    Ok(t) { effect print(t) }\n",
            "    Err(e) { effect print(\"other\") }\n",
            "}\n",
        ),
    );
    let out = run(&dir, &["check", "app.hws"]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("error[HS0404]"), "{text}");
    assert!(!text.contains("<error>"), "{text}");
}
