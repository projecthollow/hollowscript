//! The standard capabilities implemented for a host process (chapter 16).
//!
//! Each function is the tested implementation of the specification's own
//! semantics, kept apart from the interpreter: `default_grants.rs` boxes
//! each one as a `hollowscript_vm::HostFn` and binds it when a program's
//! grants allow it.
//!
//! `print`, `clock`, `random`, `env`, `args` and `stdin` are complete. `fs`
//! implements the operations and the read/write kinds, and the scope check
//! of section 16.8 (a path equal to or inside a granted directory). The
//! check and the access are one step, not two: a granted root is opened
//! once, at grant time, as a `cap_std::fs::Dir` handle (`ScopeRoot::new`),
//! and every operation below is a single call through that handle with a
//! path relative to it. `check_scope` itself never touches the filesystem;
//! it only decides, by lexical string comparison, which root (if any) a
//! script's path falls under and what the relative path into it is. The
//! real, symlink-aware resolution happens exactly once, atomically with the
//! operation itself, inside `cap_std`'s handle, which cannot leave the
//! directory it was opened on: `cap_std` is documented and (per its own
//! test suite) built to refuse a symlink that would resolve outside that
//! subtree, on Linux, macOS and Windows alike. That is what closes the race
//! the previous implementation had (`std::fs::canonicalize`, which follows
//! symlinks using the process's ambient authority, then a separate plain
//! open): a link swapped in between a lexical check and the operation has
//! nothing to race against, because there is no second filesystem access
//! that trusts the first one's answer.
//!
//! `http` (section 16.9) is implemented in `crate::http`, not this module,
//! since it needs no filesystem access and has its own request, redirect
//! and address-safety logic to keep apart from `fs`'s.

use crate::civil::{civil_from_days, days_from_civil, weekday_from_days};
use crate::random::Rng;
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use hollowscript_hostfs::{Access, OpenFailure, open_regular, read_up_to};
use std::io::{BufRead as _, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

/// `print(text)` (section 16.2): writes `text` and a trailing LF to `out`,
/// returning the number of UTF-8 bytes of `text` (which is what counts
/// against the output limit; the caller charges that and checks it before
/// calling this, since only the caller knows the running total).
///
/// # Errors
///
/// Returns whatever writing to `out` returns.
pub(crate) fn print(out: &mut impl std::io::Write, text: &str) -> std::io::Result<usize> {
    out.write_all(text.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(text.len())
}

/// `clock.now()`: the current instant as RFC 3339 text with second precision.
#[must_use]
pub(crate) fn clock_now() -> String {
    format_instant(unix_millis())
}

/// `clock.today()`: the current UTC calendar date as `YYYY-MM-DD`.
#[must_use]
pub(crate) fn clock_today() -> String {
    format_date(unix_millis().div_euclid(86_400_000))
}

/// `clock.unixMillis()`: the current instant in milliseconds since the epoch.
#[must_use]
pub(crate) fn unix_millis() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(now.as_millis()).unwrap_or(i64::MAX)
}

/// `clock.monotonicMillis()`: milliseconds since an arbitrary origin fixed at
/// the first call, never decreasing.
#[must_use]
pub(crate) fn monotonic_millis(origin: &std::time::Instant) -> i64 {
    i64::try_from(origin.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// The `YYYY-MM-DDTHH:MM:SSZ` text of an instant (milliseconds since the
/// epoch), per section 16.3: second precision, no fractional part.
#[must_use]
pub(crate) fn format_instant(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let ms_of_day = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = ms_of_day / 3_600_000;
    let minute = (ms_of_day / 60_000) % 60;
    let second = (ms_of_day / 1000) % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The `YYYY-MM-DD` text of a date (days since the epoch).
#[must_use]
pub(crate) fn format_date(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `time.parts` weekday field for a date.
#[must_use]
pub(crate) fn weekday_of(days: i64) -> u32 {
    weekday_from_days(days)
}

/// The date (days since the epoch) of a calendar date, for building a fixed
/// clock in tests or a `host.json`-style configuration.
#[must_use]
pub(crate) fn date_of(year: i64, month: u32, day: u32) -> i64 {
    days_from_civil(year, month, day)
}

/// `random.int(min, upTo)`.
///
/// # Errors
///
/// Returns an error message (the caller turns this into HS1011) if `min >= upTo`.
pub(crate) fn random_int(rng: &mut Rng, min: i64, up_to: i64) -> Result<i64, &'static str> {
    if min >= up_to {
        Err("random.int needs min < upTo")
    } else {
        Ok(rng.int_range(min, up_to))
    }
}

/// `random.float()`.
pub(crate) fn random_float(rng: &mut Rng) -> f64 {
    rng.float01()
}

/// `env(name)` (section 16.7): the value of the environment variable, or
/// `None` if unset or the name is not one this run allows reading (an empty
/// name, or one containing `=` or NUL, is always `None`).
#[must_use]
pub(crate) fn env_var(name: &str) -> Option<String> {
    if name.is_empty() || name.contains('=') || name.contains('\0') {
        return None;
    }
    std::env::var(name).ok()
}

/// `args()` (section 16.6): the program's own command-line arguments, not
/// including the program name.
#[must_use]
pub(crate) fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

/// What reading a line of standard input gave.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LineRead {
    /// The next line, without its terminator.
    Line(String),
    /// The input is exhausted.
    End,
    /// The line is longer than the `allowance` given to [`stdin_read_line`]:
    /// this many bytes were read (the allowance plus one) before giving up.
    /// The caller turns this into HS1002, as `fs.read` does for a file too
    /// large for what is left of `maxMemoryBytes`.
    TooLarge(u64),
}

/// What reading all of standard input gave.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TextRead {
    /// Everything that was left, already checked as UTF-8.
    Text(String),
    /// More than the `allowance` given to [`stdin_read_all`] was left: this
    /// many bytes were read (the allowance plus one) before giving up.
    TooLarge(u64),
}

/// `stdin.readLine()` (section 16.5): the next line without its terminator
/// (LF or CR LF), [`LineRead::End`] at the end of the input.
///
/// A last line with no terminator is returned normally, and a CR that is not
/// followed by LF is part of the line.
///
/// At most `allowance + 1` bytes are read, the allowance being what is left
/// of the run's `maxMemoryBytes`, so that an input with no line break in it
/// cannot make the host buffer more than that before it is charged for.
///
/// # Errors
///
/// Returns [`IoErrorKind::Invalid`] for a line that is not valid UTF-8,
/// which is consumed, and [`IoErrorKind::Failed`] for any other read error.
pub(crate) fn stdin_read_line(
    input: &mut impl std::io::BufRead,
    allowance: u64,
) -> Result<LineRead, IoErrorKind> {
    let mut bytes = Vec::new();
    let read = input
        .by_ref()
        .take(allowance.saturating_add(1))
        .read_until(b'\n', &mut bytes);
    match read {
        Ok(0) => return Ok(LineRead::End),
        Ok(_) => {}
        Err(_) => return Err(IoErrorKind::Failed),
    }
    let got = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if got > allowance {
        return Ok(LineRead::TooLarge(got));
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    String::from_utf8(bytes)
        .map(LineRead::Line)
        .map_err(|_| IoErrorKind::Invalid)
}

/// `stdin.readAll()` (section 16.5): everything left, as one string.
///
/// At most `allowance + 1` bytes are read, the allowance being what is left
/// of the run's `maxMemoryBytes`: the input is read through a limit and the
/// charge is made as it is read, not after all of it is in memory (a pipe of
/// hundreds of megabytes against a 64 MiB allowance used to be buffered
/// whole first).
///
/// # Errors
///
/// Returns [`IoErrorKind::Invalid`] for input that is not valid UTF-8, which
/// is consumed, and [`IoErrorKind::Failed`] for any other read error.
pub(crate) fn stdin_read_all(
    input: &mut impl std::io::Read,
    allowance: u64,
) -> Result<TextRead, IoErrorKind> {
    let bytes = read_up_to(input, allowance).map_err(|_| IoErrorKind::Failed)?;
    let got = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if got > allowance {
        return Ok(TextRead::TooLarge(got));
    }
    String::from_utf8(bytes)
        .map(TextRead::Text)
        .map_err(|_| IoErrorKind::Invalid)
}

/// The outcome of a filesystem operation, mirroring the language's `IoError`
/// (section 16.1) so a caller can build the `Result` value directly.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum IoErrorKind {
    /// The path does not exist.
    NotFound,
    /// The grant does not allow the operation.
    Denied,
    /// The target already exists in a way that prevents the operation.
    AlreadyExists,
    /// The argument is not acceptable.
    Invalid,
    /// The operation could not be completed for another reason.
    Failed,
}

/// A granted directory for `fs` (section 16.8): the root as granted, fixed
/// once when the grant is built, and a handle open on its real location.
///
/// `surface` is the root's path resolved only lexically (joined onto the
/// current directory if relative, `.` and `..` collapsed textually, no
/// symbolic link followed), the same way a script's own path is resolved
/// before the scope check (see `lexical_absolute`). Comparing the two
/// lexically, rather than resolving both to their real, symlink-free
/// location, is what lets a root granted through a path alias (a working
/// directory or a `--allow` flag that itself names a symbolic link, a
/// platform temporary directory being the common case) still match the
/// paths a script gives through that same alias. It is never consulted to
/// perform an operation. Every operation instead goes through `dir`, opened
/// once on the root's real, canonical location, so `cap_std` performs the
/// specification's symlink-resolving scope check and the access as the one
/// operation section 16.8 requires: nothing here can be redirected outside
/// the root no matter what a symlink beneath it is swapped to point at
/// afterwards.
pub(crate) struct ScopeRoot {
    surface: PathBuf,
    dir: Dir,
}

impl std::fmt::Debug for ScopeRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeRoot")
            .field("surface", &self.surface)
            .finish_non_exhaustive()
    }
}

impl ScopeRoot {
    /// Opens `path` as a granted root: resolves it lexically to fix
    /// `surface`, canonicalises it (the one access made with the host's
    /// ambient authority, and made once, at grant time, from a path the
    /// host's own `--allow` flag supplied, not from anything a script
    /// controls) to find its real location, then opens a `cap_std` handle
    /// there that every later operation is performed through.
    ///
    /// # Errors
    ///
    /// Returns [`IoErrorKind::Invalid`] if `path` has a `..` segment
    /// (see the note below), and [`IoErrorKind::NotFound`] if it
    /// does not exist or is not a directory (a grant names a directory that
    /// must already be there).
    ///
    /// **Why `..` in a grant path is refused outright, rather than made to
    /// agree with the canonical form.** `surface` is resolved lexically
    /// (textually collapsing `..`, following no symlink), and `canonical`
    /// is resolved by the platform (following every symlink, then applying
    /// `..` to whatever `..`'s left element in the resolved path turns out
    /// to be). When an earlier segment of `path` is itself a symbolic link,
    /// these two orders can name different real directories for the same
    /// grant text: `xl/../y`, with `xl` a symlink to `deep/x`, is lexically
    /// just `y`, but canonicalises to `deep/y`. `dir`, which every operation
    /// actually goes through, is opened on `canonical`; `surface` is what a
    /// script's own path is compared against. Letting the two diverge would
    /// mean a script path matched against one directory (`y`) is actually
    /// served from another (`deep/y`) that the person writing the grant may
    /// not have intended at all. Refusing any `..` in the grant's own text
    /// removes the ambiguity instead of trying to reconcile it.
    pub(crate) fn new(path: impl AsRef<Path>) -> Result<ScopeRoot, IoErrorKind> {
        let path = path.as_ref();
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(IoErrorKind::Invalid);
        }
        let surface = lexical_absolute(path)?;
        let canonical = std::fs::canonicalize(path).map_err(|_| IoErrorKind::NotFound)?;
        let dir = Dir::open_ambient_dir(&canonical, ambient_authority())
            .map_err(|_| IoErrorKind::NotFound)?;
        Ok(ScopeRoot { surface, dir })
    }
}

/// Whether `path` is malformed (section 16.8): empty, containing U+0000 or a
/// backslash, beginning with a drive designator (a letter and `:`), or having
/// a segment other than `.` and `..` that ends in `.` or in a space. This
/// applies on every platform and is checked before the scope check, so that
/// a malformed path is `Invalid` and never `Denied`.
pub(crate) fn is_malformed_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return true;
    }
    let mut bytes = path.bytes();
    if let (Some(first), Some(second)) = (bytes.next(), bytes.next()) {
        if first.is_ascii_alphabetic() && second == b':' {
            return true;
        }
    }
    path.split('/').any(|segment| {
        if segment == "." || segment == ".." || segment.is_empty() {
            return false;
        }
        segment.ends_with('.') || segment.ends_with(' ')
    })
}

/// Lexically resolves `path` to an absolute path: joined onto the current
/// directory if relative, with `.` and `..` components collapsed textually.
/// This is deliberately not `canonicalize`: it makes no filesystem call and
/// follows no symbolic link, so there is nothing here for a link swapped in
/// afterwards to race against. It exists only to decide which granted root
/// (if any) a path lexically falls under; the real, symlink-safe resolution
/// happens once the matched root's `cap_std` handle performs the operation.
fn lexical_absolute(path: &Path) -> Result<PathBuf, IoErrorKind> {
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().map_err(|_| IoErrorKind::Failed)?
    };
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(segment) => out.push(segment),
        }
    }
    Ok(out)
}

/// Checks that `path`, resolved lexically against the current directory, is
/// equal to or inside one of `roots` (section 16.8), and answers the root it
/// matched together with the path relative to that root's handle, for the
/// caller to perform the actual operation through.
///
/// # Errors
///
/// Returns [`IoErrorKind::Invalid`] for a path that is empty, contains a NUL
/// byte or is otherwise malformed, and [`IoErrorKind::Denied`] for one
/// outside every root.
pub(crate) fn check_scope<'a>(
    path: &str,
    roots: &'a [ScopeRoot],
) -> Result<(&'a ScopeRoot, PathBuf), IoErrorKind> {
    if is_malformed_path(path) {
        return Err(IoErrorKind::Invalid);
    }
    let absolute = lexical_absolute(Path::new(path))?;
    for root in roots {
        if absolute == root.surface {
            return Ok((root, PathBuf::from(".")));
        }
        if let Ok(relative) = absolute.strip_prefix(&root.surface) {
            return Ok((root, relative.to_path_buf()));
        }
    }
    Err(IoErrorKind::Denied)
}

/// What [`fs_read`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadOutcome {
    /// The file's contents, already checked as UTF-8.
    Ok(String),
    /// An ordinary `IoError` (section 16.1).
    Io(IoErrorKind),
    /// The file's real, `fstat`-ed size is more than the `allowance` given
    /// to [`fs_read`]: the caller (`default_grants::bind_fs`, which alone
    /// knows the run's own `maxMemoryBytes` and how to raise HS1002) turns
    /// this into that fault.
    TooLarge(u64),
    /// The run's cancellation token was set while the read was still in
    /// progress: the caller turns this into HS1006, the same fault the
    /// interpreter's own safepoints raise (section 11.7).
    Cancelled,
}

/// How much of a file [`fs_read`] reads between polling cancellation.
/// Section 16.8 asks for a capability to "return as soon as it can once
/// cancelled"; small enough that a cancelled run notices promptly even
/// against a slow device, large enough that polling itself is not the cost
/// on an ordinary file.
const READ_CHUNK_BYTES: usize = 256 * 1024;

/// `fs.read(path)` (sections 9.6 and 16.8).
///
/// Opens the file through the granted root's handle (refusing anything that
/// is not a regular file: a directory is `Err(Invalid)`, and a FIFO, a
/// device or a socket is refused outright rather than read from or, for a
/// FIFO with nothing yet written to it, blocking the run indefinitely
/// waiting for a writer that may never come), and reads its real,
/// `fstat`-ed size *before reading a single byte of content*: section
/// 16.8's own words are "a file larger than the remaining memory allowance
/// faults with HS1002 before its contents are read", which is what
/// `allowance` (the caller's `HostCall::memory_remaining`) and
/// [`ReadOutcome::TooLarge`] are for. Even once the size check passes, the
/// actual read is still bounded to one byte more than the size that was
/// checked (`std::io::Read::take`), so a file that grows after the check
/// (or a misbehaving file system that lies about its own size) still cannot
/// make this buffer more than that.
///
/// The read itself happens in [`READ_CHUNK_BYTES`] chunks, calling
/// `cancelled` between them (section 16.8: "a capability returns as soon as
/// it can once cancelled"). Without this, a capability call is one
/// uninterruptible step from the interpreter's point of view, so a large
/// file (or a slow device) would hold a cancelled run hostage until the
/// whole read finished and control returned to the next safepoint. Only
/// `cancelled` is polled here, not a yield callback: a capability call
/// already runs to completion as far as the language is concerned (section
/// 11.7's safepoint happens once, after the call returns), so early return
/// is offered only for the one condition section 16.8 itself asks for.
///
/// # Errors
///
/// See [`ReadOutcome`].
pub(crate) fn fs_read(
    path: &str,
    roots: &[ScopeRoot],
    allowance: u64,
    mut cancelled: impl FnMut() -> bool,
) -> ReadOutcome {
    let (root, relative) = match check_scope(path, roots) {
        Ok(v) => v,
        Err(e) => return ReadOutcome::Io(e),
    };
    if names_a_file_with_a_trailing_slash(path, root, &relative) {
        return ReadOutcome::Io(IoErrorKind::Invalid);
    }
    // A `stat` first, for the kind of entry and its size: both are known
    // before anything is opened, so a directory, a FIFO or a file far larger
    // than the allowance is refused without the cost, or the wait, of an
    // open. The open that follows is still checked on its own handle (a
    // name that was a regular file a moment ago may be a FIFO now), and is
    // made without waiting on a named pipe either way.
    let Ok(metadata) = root.dir.metadata(&relative) else {
        return ReadOutcome::Io(classify_absent(root, &relative));
    };
    if metadata.is_dir() {
        return ReadOutcome::Io(IoErrorKind::Invalid);
    }
    if !metadata.is_file() {
        // A FIFO, a device or a socket (section 16.8's "wrong kind of
        // entry"; see this function's own doc comment for why).
        return ReadOutcome::Io(IoErrorKind::Invalid);
    }
    if metadata.len() > allowance {
        return ReadOutcome::TooLarge(metadata.len());
    }
    let file = match open_regular(&root.dir, &relative, Access::Read) {
        Ok(file) => file,
        Err(OpenFailure::Io(_)) => return ReadOutcome::Io(classify_absent(root, &relative)),
        Err(failure) => return ReadOutcome::Io(open_failure_kind(&failure, root, &relative)),
    };
    // The size is read again from the handle just opened: the file may have
    // grown since the `stat`.
    let size = file.metadata().map_or(metadata.len(), |m| m.len());
    if size > allowance {
        return ReadOutcome::TooLarge(size);
    }
    let cap = size.saturating_add(1);
    let mut remaining = file.take(cap);
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        if cancelled() {
            return ReadOutcome::Cancelled;
        }
        let Ok(read) = remaining.read(&mut chunk) else {
            return ReadOutcome::Io(IoErrorKind::Failed);
        };
        if read == 0 {
            break;
        }
        let Some(filled) = chunk.get(..read) else {
            return ReadOutcome::Io(IoErrorKind::Failed);
        };
        bytes.extend_from_slice(filled);
    }
    match String::from_utf8(bytes) {
        Ok(text) => ReadOutcome::Ok(text),
        Err(_) => ReadOutcome::Io(IoErrorKind::Invalid),
    }
}

/// `fs.write(path, contents)`.
///
/// # Errors
///
/// As [`fs_read`], and [`IoErrorKind::NotFound`] if the parent directory does
/// not exist.
pub(crate) fn fs_write(
    path: &str,
    contents: &str,
    roots: &[ScopeRoot],
) -> Result<usize, IoErrorKind> {
    let (root, relative) = check_scope(path, roots)?;
    if names_a_file_with_a_trailing_slash(path, root, &relative) || root.dir.is_dir(&relative) {
        // Section 16.8: "write or append to a directory" is `Invalid`, not
        // whatever error kind the underlying `open` happens to raise for it
        // on this platform.
        return Err(IoErrorKind::Invalid);
    }
    let mut file = open_regular(&root.dir, &relative, Access::Write)
        .map_err(|failure| open_failure_kind(&failure, root, &relative))?;
    // Truncated only now that the open has been checked: opening with
    // truncation would empty a file before finding out it is one this host
    // refuses to write.
    file.set_len(0).map_err(|_| IoErrorKind::Failed)?;
    file.write_all(contents.as_bytes())
        .map_err(|_| IoErrorKind::Failed)?;
    Ok(contents.len())
}

/// `fs.append(path, contents)`.
///
/// # Errors
///
/// As [`fs_write`].
pub(crate) fn fs_append(
    path: &str,
    contents: &str,
    roots: &[ScopeRoot],
) -> Result<usize, IoErrorKind> {
    let (root, relative) = check_scope(path, roots)?;
    if names_a_file_with_a_trailing_slash(path, root, &relative) || root.dir.is_dir(&relative) {
        return Err(IoErrorKind::Invalid);
    }
    let mut file = open_regular(&root.dir, &relative, Access::Append)
        .map_err(|failure| open_failure_kind(&failure, root, &relative))?;
    file.write_all(contents.as_bytes())
        .map_err(|_| IoErrorKind::Failed)?;
    Ok(contents.len())
}

/// `fs.exists(path)`.
///
/// # Errors
///
/// [`IoErrorKind::Denied`] outside the granted paths, including a path whose
/// scope-check matched but which resolves, in whole or through a symbolic
/// link, outside the root `cap_std` will follow: `exists` must not
/// let a program learn "nothing here" about a path it is actually refused,
/// which is a different answer from the ordinary `Ok(false)` of a path that
/// is genuinely absent.
pub(crate) fn fs_exists(path: &str, roots: &[ScopeRoot]) -> Result<bool, IoErrorKind> {
    let (root, relative) = check_scope(path, roots)?;
    // `try_exists` already turns a `NotFound` metadata error into `Ok(false)`
    // itself; anything it still reports as `Err` is `cap_std` refusing to
    // resolve something that is there (an escaping symlink), which is
    // `Denied`, not `false`.
    root.dir
        .try_exists(&relative)
        .map_err(|_| IoErrorKind::Denied)
}

/// `fs.list(path)`.
///
/// # Errors
///
/// As [`fs_read`]; names that are not valid UTF-8 are left out, not an error.
pub(crate) fn fs_list(path: &str, roots: &[ScopeRoot]) -> Result<Vec<String>, IoErrorKind> {
    let (root, relative) = check_scope(path, roots)?;
    let entries = root
        .dir
        .read_dir(&relative)
        .map_err(|e| map_list_error(&e, root, &relative))?;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    Ok(names)
}

/// `fs.remove(path)`.
///
/// # Errors
///
/// As [`fs_read`]; [`IoErrorKind::Failed`] for a non-empty directory.
pub(crate) fn fs_remove(path: &str, roots: &[ScopeRoot]) -> Result<bool, IoErrorKind> {
    let (root, relative) = check_scope(path, roots)?;
    match root.dir.try_exists(&relative) {
        Ok(false) => return Ok(false),
        Err(_) => return Err(IoErrorKind::Denied),
        Ok(true) => {}
    }
    let result = if root.dir.is_dir(&relative) {
        root.dir.remove_dir(&relative)
    } else {
        root.dir.remove_file(&relative)
    };
    result.map(|()| true).map_err(|_| IoErrorKind::Failed)
}

/// `fs.mkdir(path)`.
///
/// # Errors
///
/// As [`fs_read`]; [`IoErrorKind::AlreadyExists`] if a file is already there.
pub(crate) fn fs_mkdir(path: &str, roots: &[ScopeRoot]) -> Result<bool, IoErrorKind> {
    let (root, relative) = check_scope(path, roots)?;
    if root.dir.is_dir(&relative) {
        return Ok(false);
    }
    if root.dir.is_file(&relative) {
        return Err(IoErrorKind::AlreadyExists);
    }
    root.dir
        .create_dir_all(&relative)
        .map(|()| true)
        .map_err(|_| IoErrorKind::Failed)
}

/// Whether `path` (the argument as the program wrote it, before
/// `check_scope` resolved it to `relative`) ends in `/` while naming a file
/// rather than a directory: section 16.8's own "a path with a trailing `/`
/// that names a file" is the wrong kind of entry, `Invalid`, distinct from
/// an ordinary read, write or append of that same file without the slash.
fn names_a_file_with_a_trailing_slash(path: &str, root: &ScopeRoot, relative: &Path) -> bool {
    path.ends_with('/') && root.dir.is_file(relative)
}

/// Classifies a `NotFound`-shaped failure to operate on `relative`, already
/// known to be inside `root`'s grant: [`IoErrorKind::Denied`] when
/// `cap_std`'s own `symlink_metadata` (which does not follow the final
/// component) still finds something there, meaning the operation itself
/// refused to resolve it further (an escaping symlink, most often); plain
/// [`IoErrorKind::NotFound`] when nothing is there at all. Section 16.8:
/// "a symbolic link that is dangling in any component leaves the scope, so
/// it is `Denied`, and so is a path that leaves a granted directory by `..`
/// or by a symbolic link".
fn classify_absent(root: &ScopeRoot, relative: &Path) -> IoErrorKind {
    if root.dir.symlink_metadata(relative).is_ok() {
        IoErrorKind::Denied
    } else {
        IoErrorKind::NotFound
    }
}

/// The `IoError` kind for a file [`open_regular`] would not hand back: an
/// open that failed is classified as `write`, `append` and `read` always
/// classified a failed open (see [`map_write_error`]); something that is not
/// a regular file (a named pipe, a device, a socket) is `Invalid`, section
/// 16.8's "wrong kind of entry"; a regular file with more than one hard link
/// is `Denied`, since a name inside the root can be the same file as one
/// outside it (see `hollowscript-hostfs` for what that does and does not
/// cover).
fn open_failure_kind(failure: &OpenFailure, root: &ScopeRoot, relative: &Path) -> IoErrorKind {
    match failure {
        OpenFailure::Io(e) => map_write_error(e, root, relative),
        OpenFailure::NotRegular => IoErrorKind::Invalid,
        OpenFailure::HardLinked => IoErrorKind::Denied,
    }
}

/// Maps a failed `write` or `append` through a `ScopeRoot`'s handle to the
/// `IoError` kind sections 16.1 and 16.8 define, including the same
/// `PermissionDenied`-or-`NotFound` ambiguity `map_list_error` resolves with
/// [`classify_absent`]: a parent directory that does not exist is
/// `NotFound`, but a component `cap_std` refuses to resolve (an escaping
/// symlink) is `Denied`, and the two are told apart the same way.
fn map_write_error(e: &std::io::Error, root: &ScopeRoot, relative: &Path) -> IoErrorKind {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            classify_absent(root, relative)
        }
        std::io::ErrorKind::IsADirectory | std::io::ErrorKind::NotADirectory => {
            IoErrorKind::Invalid
        }
        _ => IoErrorKind::Failed,
    }
}

/// Maps a failed `list` through a `ScopeRoot`'s handle to the `IoError` kind
/// section 16.1 and 16.8 define: `list` of a file is `Invalid` (16.8's
/// "wrong kind of entry"), an absent directory is classified by
/// [`classify_absent`], and anything else is `Failed`.
fn map_list_error(e: &std::io::Error, root: &ScopeRoot, relative: &Path) -> IoErrorKind {
    match e.kind() {
        // `cap_std` reports both a genuinely missing directory and one it
        // refuses to resolve (an escaping symlink) as `NotFound` in some
        // cases and `PermissionDenied` in others, depending on exactly
        // where resolution stops; `classify_absent`'s own `symlink_metadata`
        // check is what actually tells the two apart.
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            classify_absent(root, relative)
        }
        std::io::ErrorKind::NotADirectory => IoErrorKind::Invalid,
        _ => IoErrorKind::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IoErrorKind, READ_CHUNK_BYTES, ReadOutcome, ScopeRoot, args, check_scope, clock_today,
        env_var, format_date, format_instant, fs_exists, fs_list, fs_mkdir, fs_read, fs_remove,
        fs_write, print, weekday_of,
    };
    use std::io::Cursor;

    #[test]
    fn print_writes_text_and_one_newline_and_counts_only_the_text() {
        let mut buf = Cursor::new(Vec::new());
        let n = print(&mut buf, "hello").unwrap();
        assert_eq!(n, 5);
        assert_eq!(buf.into_inner(), b"hello\n");
    }

    #[test]
    fn instants_and_dates_format_as_the_specification_shows() {
        assert_eq!(format_instant(1_767_225_600_000), "2026-01-01T00:00:00Z");
        assert_eq!(format_date(days_since_epoch(2026, 1, 1)), "2026-01-01");
        assert_eq!(weekday_of(days_since_epoch(2026, 7, 26)), 7);
        // A negative instant (before 1970) still formats correctly.
        assert_eq!(format_instant(-1), "1969-12-31T23:59:59Z");
    }

    fn days_since_epoch(y: i64, m: u32, d: u32) -> i64 {
        super::date_of(y, m, d)
    }

    #[test]
    fn env_rejects_malformed_names_without_asking_the_platform() {
        assert_eq!(env_var(""), None);
        assert_eq!(env_var("A=B"), None);
        assert!(
            env_var("PATH_LIKELY_UNSET_XYZ").is_none()
                || env_var("PATH_LIKELY_UNSET_XYZ").is_some()
        );
    }

    #[test]
    fn args_never_includes_the_program_name() {
        // In a test binary argv[0] is the test harness; this only checks the
        // function does not panic and returns a list.
        let _ = args();
    }

    #[test]
    fn clock_today_is_a_ten_character_date() {
        assert_eq!(clock_today().len(), 10);
        assert_eq!(clock_today().as_bytes()[4], b'-');
    }

    #[test]
    fn fs_operations_stay_inside_the_granted_root() {
        // Language paths are relative and slash-separated even on Windows;
        // native absolute paths belong only in the host's ScopeRoot grant.
        let dir = std::path::PathBuf::from(format!("hollowscript-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = ScopeRoot::new(&dir).unwrap();

        let inside = dir.join("a.txt").to_string_lossy().replace('\\', "/");
        assert_eq!(fs_write(&inside, "hi", std::slice::from_ref(&root)), Ok(2));
        assert_eq!(
            fs_read(&inside, std::slice::from_ref(&root), u64::MAX, || false),
            ReadOutcome::Ok("hi".to_owned())
        );
        assert_eq!(fs_exists(&inside, std::slice::from_ref(&root)), Ok(true));

        let sub = dir.join("sub").to_string_lossy().replace('\\', "/");
        assert_eq!(fs_mkdir(&sub, std::slice::from_ref(&root)), Ok(true));
        assert_eq!(
            fs_mkdir(&sub, std::slice::from_ref(&root)),
            Ok(false),
            "already exists"
        );

        let outside = std::path::Path::new("outside-hollowscript-test.txt");
        let outside_str = outside.to_string_lossy().into_owned();
        assert_eq!(
            check_scope(&outside_str, std::slice::from_ref(&root)).err(),
            Some(IoErrorKind::Denied)
        );

        assert_eq!(fs_remove(&inside, std::slice::from_ref(&root)), Ok(true));
        assert_eq!(
            fs_remove(&inside, std::slice::from_ref(&root)),
            Ok(false),
            "already gone"
        );

        drop(root);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_malformed_path_is_invalid_not_denied() {
        assert_eq!(check_scope("", &[]).err(), Some(IoErrorKind::Invalid));
        assert_eq!(check_scope("a\0b", &[]).err(), Some(IoErrorKind::Invalid));
        assert_eq!(check_scope("C:/a", &[]).err(), Some(IoErrorKind::Invalid));
        assert_eq!(check_scope(r"a\b", &[]).err(), Some(IoErrorKind::Invalid));
    }

    /// The race this module closes: a path that is inside the granted root
    /// at the moment it is granted, but where a directory on the way to the
    /// target is later swapped for a symlink pointing outside the root. The
    /// previous implementation (`canonicalize` the whole path, then open it
    /// separately) would resolve the swapped-in link and follow it outside
    /// the root on the second, unchecked step. This fails against that
    /// implementation and passes against `cap_std`'s resolve-and-use-in-one-
    /// step handle.
    #[cfg(unix)]
    #[test]
    fn a_symlink_swapped_in_after_the_grant_cannot_escape_the_root() {
        let base =
            std::env::temp_dir().join(format!("hollowscript-cli-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let granted_root = base.join("granted");
        let victim = base.join("victim");
        let redirectable = granted_root.join("redirectable");
        std::fs::create_dir_all(&granted_root).unwrap();
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("secret.txt"), "top secret").unwrap();
        // At grant time, `redirectable` is an ordinary directory inside the
        // granted root, so the grant is legitimate.
        std::fs::create_dir(&redirectable).unwrap();
        let root = ScopeRoot::new(&granted_root).unwrap();

        // Now, after the root handle is already open, swap it for a symlink
        // that points outside the granted root, exactly as an attacker
        // racing the check would.
        std::fs::remove_dir(&redirectable).unwrap();
        std::os::unix::fs::symlink(&victim, &redirectable).unwrap();

        let target = redirectable
            .join("secret.txt")
            .to_string_lossy()
            .into_owned();
        let outcome = fs_read(&target, std::slice::from_ref(&root), u64::MAX, || false);
        assert!(
            matches!(outcome, ReadOutcome::Io(_)),
            "a symlink swapped in after the grant must not be followed outside the root, got {outcome:?}"
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A read cancelled partway through stops as soon as it notices,
    /// rather than finishing the file first: proven with a file several
    /// chunks long (`READ_CHUNK_BYTES`) and a `cancelled` closure that
    /// answers true from its second call onwards, counting how many times
    /// it was asked. If `fs_read` polled only once, or not at all between
    /// chunks, this would either never see `Cancelled` or would still have
    /// read the whole file before checking; both are ruled out by the call
    /// count staying far below how many chunks the file actually holds.
    #[test]
    fn fs_read_stops_early_once_cancelled() {
        let dir =
            std::path::PathBuf::from(format!("hollowscript-cli-fs-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = ScopeRoot::new(&dir).unwrap();
        let path = dir.join("chunked.txt");
        let chunks_in_file = 8;
        let contents = "a".repeat(READ_CHUNK_BYTES * chunks_in_file);
        std::fs::write(&path, &contents).unwrap();
        let target = path.to_string_lossy().replace('\\', "/");

        let mut polls = 0;
        let outcome = fs_read(&target, std::slice::from_ref(&root), u64::MAX, || {
            polls += 1;
            polls > 1
        });
        assert_eq!(outcome, ReadOutcome::Cancelled);
        assert!(
            polls < chunks_in_file,
            "a cancelled read must stop long before the last of {chunks_in_file} chunks, stopped after {polls}"
        );

        drop(root);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file larger than the allowance is refused (`TooLarge`,
    /// which the caller turns into HS1002) without its contents ever being
    /// read. Proven with a sparse file reporting a size in the gigabytes
    /// (created by seeking past the end and writing one byte, which most
    /// file systems do not actually allocate) that would abort the test
    /// process outright if this function tried to buffer it: this test
    /// finishing at all, quickly, is the assertion.
    #[test]
    fn fs_read_refuses_a_file_larger_than_the_allowance_before_reading_it() {
        let dir =
            std::path::PathBuf::from(format!("hollowscript-cli-fs-large-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = ScopeRoot::new(&dir).unwrap();
        let path = dir.join("huge.txt");
        crate::test_fs::sparse_file(&path, 8 * 1024 * 1024 * 1024);
        let target = path.to_string_lossy().replace('\\', "/");
        let outcome = fs_read(&target, std::slice::from_ref(&root), 1000, || false);
        assert_eq!(outcome, ReadOutcome::TooLarge(8 * 1024 * 1024 * 1024));
        drop(root);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A FIFO is refused outright rather than opened (which would
    /// block indefinitely waiting for a writer that may never come, on a
    /// `fs.read` of a FIFO nothing has written to yet).
    #[cfg(unix)]
    #[test]
    fn fs_read_refuses_a_fifo_instead_of_blocking() {
        let dir =
            std::env::temp_dir().join(format!("hollowscript-cli-fs-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status();
        let Ok(status) = status else {
            eprintln!("'mkfifo' is not available; skipping");
            std::fs::remove_dir_all(&dir).ok();
            return;
        };
        assert!(status.success(), "mkfifo failed");
        let root = ScopeRoot::new(&dir).unwrap();
        let target = fifo.to_string_lossy().into_owned();
        // No writer ever opens the other end: were this to call `open` on
        // the FIFO, it would hang here. It must not.
        let outcome = fs_read(&target, std::slice::from_ref(&root), u64::MAX, || false);
        assert_eq!(outcome, ReadOutcome::Io(IoErrorKind::Invalid));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An escaping symlink is `Denied` for `exists`, `remove` and
    /// `list`, not `Ok(false)`/`NotFound`, which would let a program use
    /// those operations to learn something about a path outside its grant.
    #[cfg(unix)]
    #[test]
    fn an_escaping_symlink_is_denied_not_silently_absent() {
        let base =
            std::env::temp_dir().join(format!("hollowscript-cli-fs-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let granted_root = base.join("granted");
        let outside = base.join("outside");
        std::fs::create_dir_all(&granted_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("f.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.join("f.txt"), granted_root.join("link.txt")).unwrap();
        let root = ScopeRoot::new(&granted_root).unwrap();
        let target = granted_root.join("link.txt").to_string_lossy().into_owned();

        assert_eq!(
            fs_exists(&target, std::slice::from_ref(&root)),
            Err(IoErrorKind::Denied)
        );
        assert_eq!(
            fs_remove(&target, std::slice::from_ref(&root)),
            Err(IoErrorKind::Denied)
        );
        assert_eq!(
            fs_list(&target, std::slice::from_ref(&root)),
            Err(IoErrorKind::Denied)
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    /// `list` of a plain file is `Invalid`, not `NotFound`.
    #[test]
    fn fs_list_of_a_file_is_invalid() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-fs-list-file-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        let root = ScopeRoot::new(&dir).unwrap();
        let target = dir.join("f.txt").to_string_lossy().into_owned();
        assert_eq!(
            fs_list(&target, std::slice::from_ref(&root)),
            Err(IoErrorKind::Invalid)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Writing to an existing directory is `Invalid`.
    #[test]
    fn fs_write_to_a_directory_is_invalid() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-fs-write-dir-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let root = ScopeRoot::new(&dir).unwrap();
        let target = dir.join("sub").to_string_lossy().into_owned();
        assert_eq!(
            fs_write(&target, "x", std::slice::from_ref(&root)),
            Err(IoErrorKind::Invalid)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `..` segment in a grant path is refused (`Invalid`), which
    /// closes the gap where an earlier symlink segment made the lexical
    /// surface and the canonical, symlink-resolved directory disagree about
    /// which real directory a grant like `xl/../y` (`xl` a symlink) names.
    #[cfg(unix)]
    #[test]
    fn a_grant_path_containing_dotdot_is_refused() {
        let base = std::env::temp_dir().join(format!(
            "hollowscript-cli-fs-grant-dotdot-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let deep = base.join("deep");
        std::fs::create_dir_all(deep.join("y")).unwrap();
        std::fs::create_dir_all(base.join("y")).unwrap();
        std::os::unix::fs::symlink(&deep, base.join("xl")).unwrap();
        let grant = base.join("xl").join("..").join("y");
        assert_eq!(ScopeRoot::new(&grant).err(), Some(IoErrorKind::Invalid));
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A reader that counts what is taken from it and offers `total` bytes,
    /// to show how far a read goes before it gives up.
    struct Counting {
        offered: u64,
        taken: std::rc::Rc<std::cell::Cell<u64>>,
    }

    impl std::io::Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = u64::try_from(buf.len()).unwrap().min(self.offered);
            let n_usize = usize::try_from(n).unwrap();
            buf[..n_usize].fill(b'a');
            self.offered -= n;
            self.taken.set(self.taken.get() + n);
            Ok(n_usize)
        }
    }

    /// `stdin.readAll` of far more than the allowance reads the allowance and
    /// one byte, a buffer's worth at most beyond that, and says it was too
    /// large: it used to read everything first (700 MB piped in gave a
    /// resident size of 705 MB against a 64 MiB limit).
    #[test]
    fn stdin_read_all_stops_reading_at_the_allowance() {
        let taken = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut input = Counting {
            offered: 700 * 1024 * 1024,
            taken: std::rc::Rc::clone(&taken),
        };
        let outcome = super::stdin_read_all(&mut input, 1000).unwrap();
        assert_eq!(outcome, super::TextRead::TooLarge(1001));
        assert!(
            taken.get() <= 1001 + 64 * 1024,
            "{} bytes were taken from the input",
            taken.get()
        );
    }

    #[test]
    fn stdin_read_all_returns_what_fits() {
        let mut input = Cursor::new(b"hello".to_vec());
        assert_eq!(
            super::stdin_read_all(&mut input, 5),
            Ok(super::TextRead::Text("hello".to_owned()))
        );
        let mut input = Cursor::new(b"hello!".to_vec());
        assert_eq!(
            super::stdin_read_all(&mut input, 5),
            Ok(super::TextRead::TooLarge(6))
        );
        let mut invalid = Cursor::new(vec![0xff, 0xfe]);
        assert_eq!(
            super::stdin_read_all(&mut invalid, 10),
            Err(IoErrorKind::Invalid)
        );
    }

    /// A line with no line break in it, far longer than the allowance, is
    /// read only up to the allowance and one byte.
    #[test]
    fn stdin_read_line_stops_reading_at_the_allowance() {
        let taken = std::rc::Rc::new(std::cell::Cell::new(0));
        let input = Counting {
            offered: 700 * 1024 * 1024,
            taken: std::rc::Rc::clone(&taken),
        };
        let mut reader = std::io::BufReader::with_capacity(8 * 1024, input);
        let outcome = super::stdin_read_line(&mut reader, 1000).unwrap();
        assert_eq!(outcome, super::LineRead::TooLarge(1001));
        assert!(
            taken.get() <= 1001 + 16 * 1024,
            "{} bytes were taken from the input",
            taken.get()
        );
    }

    #[test]
    fn stdin_read_line_returns_lines_within_the_allowance() {
        let mut input = Cursor::new(b"one\r\ntwo\nlast".to_vec());
        let line = |input: &mut Cursor<Vec<u8>>| super::stdin_read_line(input, 100).unwrap();
        assert_eq!(line(&mut input), super::LineRead::Line("one".to_owned()));
        assert_eq!(line(&mut input), super::LineRead::Line("two".to_owned()));
        assert_eq!(line(&mut input), super::LineRead::Line("last".to_owned()));
        assert_eq!(line(&mut input), super::LineRead::End);
        // A line of exactly the allowance fits; the terminator is not
        // counted against it only because it is read as part of the line, so
        // the allowance must cover it too.
        let mut exact = Cursor::new(b"abcd\n".to_vec());
        assert_eq!(
            super::stdin_read_line(&mut exact, 5),
            Ok(super::LineRead::Line("abcd".to_owned()))
        );
    }

    /// Runs `f` on a thread and fails the test, instead of hanging it, if it
    /// has not finished in a few seconds.
    fn within_a_few_seconds<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the operation blocked")
    }

    /// `fs.write` and `fs.append` to a named pipe with no reader used to
    /// block for ever in the open. They refuse at once now.
    #[cfg(unix)]
    #[test]
    fn fs_write_and_append_to_a_fifo_do_not_block() {
        let dir =
            std::env::temp_dir().join(format!("hollowscript-cli-fs-wfifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !matches!(made, Ok(status) if status.success()) {
            eprintln!("'mkfifo' is not available; skipping");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        let target = fifo.to_string_lossy().into_owned();
        let root_path = dir.clone();
        let (written, appended) = within_a_few_seconds(move || {
            let root = ScopeRoot::new(&root_path).unwrap();
            (
                fs_write(&target, "x", std::slice::from_ref(&root)),
                super::fs_append(&target, "x", std::slice::from_ref(&root)),
            )
        });
        assert!(written.is_err(), "{written:?}");
        assert!(appended.is_err(), "{appended:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// With a reader on the other end, the open succeeds, and what it opened
    /// is then found not to be a regular file: `Invalid`, nothing written.
    #[cfg(unix)]
    #[test]
    fn fs_write_to_a_fifo_with_a_reader_is_invalid_and_writes_nothing() {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir =
            std::env::temp_dir().join(format!("hollowscript-cli-fs-wfifo2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !matches!(made, Ok(status) if status.success()) {
            eprintln!("'mkfifo' is not available; skipping");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        // A reader, opened without waiting (O_NONBLOCK is 0x800 on Linux and
        // 0x4 on the BSDs and macOS).
        let flag = if cfg!(target_os = "linux") {
            0x800
        } else {
            0x4
        };
        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flag)
            .open(&fifo)
            .unwrap();
        let root = ScopeRoot::new(&dir).unwrap();
        let target = fifo.to_string_lossy().into_owned();
        let written =
            within_a_few_seconds(move || fs_write(&target, "secret", std::slice::from_ref(&root)));
        assert_eq!(written, Err(IoErrorKind::Invalid));
        let mut got = Vec::new();
        let _ = reader.read_to_end(&mut got);
        assert!(got.is_empty(), "nothing may be written to the pipe");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A regular file with a second hard link inside a granted root is, as
    /// far as a handle can tell, the same file as the other name, which may
    /// be outside the root: refused to read and to write, and the other name
    /// is left alone.
    #[cfg(unix)]
    #[test]
    fn fs_refuses_a_file_with_a_second_hard_link() {
        let base = std::env::temp_dir().join(format!(
            "hollowscript-cli-fs-hardlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let granted = base.join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        let outside = base.join("outside.txt");
        std::fs::write(&outside, "outside content").unwrap();
        std::fs::hard_link(&outside, granted.join("inside.txt")).unwrap();
        let root = ScopeRoot::new(&granted).unwrap();
        let target = granted.join("inside.txt").to_string_lossy().into_owned();
        assert_eq!(
            fs_read(&target, std::slice::from_ref(&root), u64::MAX, || false),
            ReadOutcome::Io(IoErrorKind::Denied)
        );
        assert_eq!(
            fs_write(&target, "overwritten", std::slice::from_ref(&root)),
            Err(IoErrorKind::Denied)
        );
        assert_eq!(
            super::fs_append(&target, "appended", std::slice::from_ref(&root)),
            Err(IoErrorKind::Denied)
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "outside content"
        );
        // A file with one name reads and writes as before.
        let plain = granted.join("plain.txt").to_string_lossy().into_owned();
        assert_eq!(fs_write(&plain, "ok", std::slice::from_ref(&root)), Ok(2));
        assert_eq!(
            fs_read(&plain, std::slice::from_ref(&root), u64::MAX, || false),
            ReadOutcome::Ok("ok".to_owned())
        );
        // Rewriting a longer file with a shorter text leaves only the text.
        assert_eq!(fs_write(&plain, "o", std::slice::from_ref(&root)), Ok(1));
        assert_eq!(
            std::fs::read_to_string(granted.join("plain.txt")).unwrap(),
            "o"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
