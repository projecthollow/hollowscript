//! Writing to standard output and standard error without ever panicking.
//!
//! `println!` and `eprintln!` panic when the write fails, which is what a
//! pipe closed early does (`hollowscript run many.hws | head -1`): the
//! standard library ignores `SIGPIPE`, so the write returns `EPIPE` and the
//! macro turns that into a panic and exit code 101. Every write this command
//! makes goes through here instead.
//!
//! A closed standard output is the conventional way for a reader to say it
//! has seen enough, so it is not an error: the first write that finds it
//! closed records [`Problem::Closed`], every later write to standard output
//! is skipped, a running program is stopped at its next `print`, and
//! `main` exits without printing anything about it. Any other write failure
//! (a full disk, say) is [`Problem::Failed`], which `main` reports once on
//! standard error. Standard error itself is best effort: if it cannot be
//! written either, there is nowhere left to say so.

use std::io::Write;
use std::sync::OnceLock;

/// Why standard output can no longer be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Problem {
    /// The reader closed it (`EPIPE`). Not an error.
    Closed,
    /// Any other failure, with the operating system's own description.
    Failed(String),
}

static PROBLEM: OnceLock<Problem> = OnceLock::new();

/// The first problem writing standard output ran into, if there was one.
pub(crate) fn problem() -> Option<&'static Problem> {
    PROBLEM.get()
}

/// Writes `text` and a line feed to standard output. Answers `false` if
/// standard output can no longer be written (see [`problem`]), in which case
/// nothing more should be written to it.
///
/// Standard output is line buffered, so each line reaches the reader as it
/// is written: a question printed before a program reads standard input
/// appears before the read, and a long loop shows its progress.
pub(crate) fn line(text: &str) -> bool {
    write_out(text.as_bytes(), true)
}

/// Writes `text` to standard output as it is, with no line feed added.
pub(crate) fn raw(text: &str) -> bool {
    write_out(text.as_bytes(), false)
}

fn write_out(bytes: &[u8], newline: bool) -> bool {
    if PROBLEM.get().is_some() {
        return false;
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let result = lock
        .write_all(bytes)
        .and_then(|()| {
            if newline {
                lock.write_all(b"\n")
            } else {
                Ok(())
            }
        })
        .and_then(|()| lock.flush());
    match result {
        Ok(()) => true,
        Err(e) => {
            let problem = if e.kind() == std::io::ErrorKind::BrokenPipe {
                Problem::Closed
            } else {
                Problem::Failed(e.to_string())
            };
            let _ = PROBLEM.set(problem);
            false
        }
    }
}

/// Writes `text` and a line feed to standard error, ignoring a failure.
pub(crate) fn err_line(text: &str) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = lock
        .write_all(text.as_bytes())
        .and_then(|()| lock.write_all(b"\n"));
}
