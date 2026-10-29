//! `hollowscript-lsp`: a language server over the syntax and check crates,
//! speaking the Language Server Protocol on standard input and output. It
//! declares no third-party dependency of its own (`json` and `framing` are
//! this crate's own); the only third-party code it links is `cap-std` (and,
//! on Unix, `libc`), which it reaches through `hollowscript-hostfs`, the crate
//! that opens an imported file without following a link or waiting.
//!
//! **What works today.** `initialize`, `shutdown` and `exit`; incremental
//! document sync (`sync`); diagnostics on every open or change; formatting;
//! hover; go to definition; completion; find references and rename, with
//! `prepareRename`; signature help; an outline (document symbols and
//! folding ranges); semantic tokens; and code actions for the diagnostics
//! that have one unambiguous fix. Every one of these resolves a name
//! declared in an imported file, not only the document open in the editor
//! (`workspace`, `loader`). Everything else this build does not implement
//! gets the standard `MethodNotFound` JSON-RPC error for a request, or is
//! silently ignored for a notification, rather than being dropped without
//! a reply.

#![forbid(unsafe_code)]

mod actions;
mod framing;
mod json;
mod loader;
mod outline;
mod paths;
mod refactor;
mod semtok;
mod server;
mod signature;
mod sync;
mod workspace;

use server::Server;
use std::io::{BufReader, Write};

fn main() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut server = Server::new();

    loop {
        let body = match framing::read_message(&mut reader) {
            Ok(Some(body)) => body,
            Ok(None) => return std::process::ExitCode::SUCCESS,
            Err(e) => {
                log(&format!("hollowscript-lsp: {e}"));
                return std::process::ExitCode::FAILURE;
            }
        };
        let Ok(text) = std::str::from_utf8(&body) else {
            if let Some(code) = send(&mut writer, &server::parse_error_response()) {
                return code;
            }
            continue;
        };
        let Ok(message) = json::parse(text) else {
            if let Some(code) = send(&mut writer, &server::parse_error_response()) {
                return code;
            }
            continue;
        };
        let method = message
            .get("method")
            .and_then(json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        for outgoing in server.handle(&message) {
            if let Some(code) = send(&mut writer, &outgoing.body) {
                return code;
            }
        }
        if Server::should_exit(&method) {
            return std::process::ExitCode::SUCCESS;
        }
    }
}

/// Writes one message. Answers the exit code to stop with if standard
/// output can no longer be written: the client is gone (a closed pipe,
/// which ends the session quietly) or the write failed some other way
/// (logged). A server that kept reading after that would answer nobody.
fn send(writer: &mut impl Write, body: &json::Value) -> Option<std::process::ExitCode> {
    match framing::write_message(writer, body.to_json().as_bytes()) {
        Ok(()) => None,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            Some(std::process::ExitCode::SUCCESS)
        }
        Err(e) => {
            log(&format!(
                "hollowscript-lsp: cannot write to standard output: {e}"
            ));
            Some(std::process::ExitCode::FAILURE)
        }
    }
}

/// Writes one line to standard error, the server's log, ignoring a failure:
/// unlike `eprintln!`, this never panics when standard error is closed.
fn log(text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{text}");
}
