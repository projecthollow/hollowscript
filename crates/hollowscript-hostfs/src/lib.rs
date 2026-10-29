//! How a host opens a file it has only a name for.
//!
//! A host that runs or analyses a program it did not write is handed names:
//! the imports of a program, the paths a script passes to `fs`. Reading
//! "the file at that name" is where a host is steered into reading what it
//! should not (a symbolic link to a file elsewhere, a second hard link to
//! one), blocking for ever (a named pipe waits for a writer that never
//! comes), or buffering far more than a program could ever use (a file of
//! gigabytes). This crate is the one place that decides how a name becomes an
//! open file, so the command line host and the language server cannot
//! disagree about it.
//!
//! What it does, for every file it opens:
//!
//! - **Relative to a handle.** A name is resolved against a
//!   [`cap_std::fs::Dir`] opened once on a root. `cap_std` resolves it in one
//!   step and cannot leave the root, so a link swapped in after a check has
//!   nothing to race against ([`open_regular`]).
//! - **Without waiting.** On Unix the file is opened with `O_NONBLOCK`, so
//!   opening a named pipe returns at once instead of waiting for the other
//!   end, and the kind is then read from the handle just opened
//!   (`fstat`), never from a second look at the name.
//! - **Regular files only.** A directory, a named pipe, a device or a socket
//!   is refused ([`OpenFailure::NotRegular`]).
//! - **One name only (Unix).** A regular file with more than one hard link is
//!   refused ([`OpenFailure::HardLinked`]): a hard link inside a root can be
//!   the same file as one outside it, and nothing in the handle can tell
//!   which name is the "real" one. This is a deliberate trade: a file that is
//!   legitimately hard linked (a deduplicating store, say) is refused too.
//!   On Windows the number of links is not read, so a hard link there is
//!   *not* detected; every other guarantee above still holds.
//! - **Only so many bytes.** [`read_up_to`] reads at most `limit + 1` bytes
//!   however large the file really is, so a caller learns it was too large
//!   without having held the file in memory.
//!
//! [`RootDir`] is the whole of this for a loader that serves a program's
//! modules from a directory, per section 10.3 of the specification: every
//! segment of a canonical path must match a real directory entry byte for
//! byte, no symbolic link is followed at any level, and the result is a
//! regular file read under a size limit.

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, OpenOptions};
use std::ffi::OsStr;
use std::io::Read as _;
use std::path::Path;

/// What a file is opened for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// Reading.
    Read,
    /// Writing, creating the file if it is absent. An existing file is *not*
    /// truncated by the open, so that it can be checked first; the caller
    /// truncates it (`File::set_len(0)`) once [`open_regular`] has accepted it.
    Write,
    /// Appending, creating the file if it is absent.
    Append,
}

/// Why [`open_regular`] did not hand back a file.
#[derive(Debug)]
pub enum OpenFailure {
    /// The open itself failed. The caller classifies the error: whether it
    /// means "absent" or "refused by the scope" depends on what it was
    /// looking for.
    Io(std::io::Error),
    /// The entry is a directory, a named pipe, a device, a socket or
    /// something else that is not a regular file.
    NotRegular,
    /// The entry is a regular file with more than one hard link (Unix).
    HardLinked,
}

/// Opens `relative` inside `dir` for `access`, and returns it only if it is a
/// regular file with a single name (see the module documentation).
///
/// The kind and the link count are read from the open handle, after the
/// open, so there is no gap between what was checked and what is used.
///
/// # Errors
///
/// [`OpenFailure`].
pub fn open_regular(dir: &Dir, relative: &Path, access: Access) -> Result<File, OpenFailure> {
    let mut options = OpenOptions::new();
    match access {
        Access::Read => {
            options.read(true);
        }
        Access::Write => {
            options.write(true).create(true);
        }
        Access::Append => {
            options.append(true).create(true);
        }
    }
    non_blocking(&mut options);
    let file = dir.open_with(relative, &options).map_err(OpenFailure::Io)?;
    let metadata = file.metadata().map_err(OpenFailure::Io)?;
    if !metadata.is_file() {
        return Err(OpenFailure::NotRegular);
    }
    if is_hard_linked(&metadata) {
        return Err(OpenFailure::HardLinked);
    }
    Ok(file)
}

#[cfg(unix)]
fn non_blocking(options: &mut OpenOptions) {
    use cap_std::fs::OpenOptionsExt as _;
    options.custom_flags(libc::O_NONBLOCK);
}

#[cfg(not(unix))]
fn non_blocking(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn is_hard_linked(metadata: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt as _;
    metadata.nlink() > 1
}

#[cfg(not(unix))]
fn is_hard_linked(_metadata: &cap_std::fs::Metadata) -> bool {
    false
}

/// Reads at most `limit + 1` bytes of `reader`: the whole of it if it is no
/// longer than `limit`, otherwise exactly `limit + 1`, which is how a caller
/// tells "too large" from "exactly the limit" without ever holding more.
///
/// # Errors
///
/// Whatever reading returns.
pub fn read_up_to(reader: &mut impl std::io::Read, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Why [`RootDir::read_module`] did not return a module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// There is nothing at that name, or what is there is a symbolic link or
    /// its name differs in case: to a program, exactly as if it were absent.
    NotFound,
    /// Something is there but is not read (not a regular file, a file with
    /// more than one hard link, or an I/O error): the reason, for a
    /// diagnostic.
    Failed(String),
}

/// A directory a program's modules are served from, opened once.
#[derive(Debug)]
pub struct RootDir {
    dir: std::io::Result<Dir>,
}

impl RootDir {
    /// Opens `path` with the host's own authority: the one filesystem access
    /// made outside the handle, on a path the person running the host gave,
    /// not anything a program controls. If `path` does not exist or is not a
    /// directory, every later read reports [`ReadError::NotFound`].
    #[must_use]
    pub fn open(path: &Path) -> RootDir {
        RootDir {
            dir: Dir::open_ambient_dir(path, ambient_authority()),
        }
    }

    /// Reads the module at `canonical_path` (a `/`-separated path relative to
    /// the root, with no `.` or `..`), at most `limit + 1` bytes of it (see
    /// [`read_up_to`]).
    ///
    /// Each segment must match a real directory entry byte for byte (so a
    /// case-insensitive file system cannot answer `Util.hws` for `util.hws`),
    /// and no symbolic link is followed at any level; both are
    /// [`ReadError::NotFound`]. The last segment must be a regular file with
    /// a single name, opened without waiting (see the module documentation);
    /// anything else is [`ReadError::Failed`] with the reason.
    ///
    /// # Errors
    ///
    /// [`ReadError`].
    pub fn read_module(&self, canonical_path: &str, limit: u64) -> Result<Vec<u8>, ReadError> {
        let Ok(root) = &self.dir else {
            return Err(ReadError::NotFound);
        };
        let segments: Vec<&str> = canonical_path
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect();
        let Some((last, dirs)) = segments.split_last() else {
            return Err(ReadError::NotFound);
        };
        // `..` and `.` are only names, which no directory entry matches
        // byte for byte; a leading `/` is an empty segment, filtered out.
        let mut owned_dirs: Vec<Dir> = Vec::with_capacity(dirs.len());
        for segment in dirs {
            let current = owned_dirs.last().unwrap_or(root);
            let sub = open_exact_dir(current, segment)?;
            owned_dirs.push(sub);
        }
        let current = owned_dirs.last().unwrap_or(root);
        read_exact_file(current, last, limit)
    }
}

/// Whether `dir` has an entry named exactly `segment`.
fn has_exact_entry(dir: &Dir, segment: &str) -> bool {
    let Ok(entries) = dir.entries() else {
        return false;
    };
    let wanted = OsStr::new(segment);
    entries
        .filter_map(Result::ok)
        .any(|entry| entry.file_name() == wanted)
}

fn open_exact_dir(dir: &Dir, segment: &str) -> Result<Dir, ReadError> {
    if !has_exact_entry(dir, segment) {
        return Err(ReadError::NotFound);
    }
    let metadata = dir
        .symlink_metadata(segment)
        .map_err(|_| ReadError::NotFound)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReadError::NotFound);
    }
    dir.open_dir(segment).map_err(|_| ReadError::NotFound)
}

fn read_exact_file(dir: &Dir, segment: &str, limit: u64) -> Result<Vec<u8>, ReadError> {
    if !has_exact_entry(dir, segment) {
        return Err(ReadError::NotFound);
    }
    let metadata = dir
        .symlink_metadata(segment)
        .map_err(|_| ReadError::NotFound)?;
    if metadata.file_type().is_symlink() {
        return Err(ReadError::NotFound);
    }
    // The open and the checks of its kind are one step on the handle (the
    // look at the name above only decides symbolic link or not, which a
    // handle cannot say: it would have followed the link).
    let mut file = match open_regular(dir, Path::new(segment), Access::Read) {
        Ok(file) => file,
        Err(OpenFailure::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReadError::NotFound);
        }
        Err(OpenFailure::Io(e)) => return Err(ReadError::Failed(e.to_string())),
        Err(OpenFailure::NotRegular) => {
            return Err(ReadError::Failed("it is not a regular file".to_owned()));
        }
        Err(OpenFailure::HardLinked) => {
            return Err(ReadError::Failed(
                "it has more than one hard link, which could be a file outside the program's directory"
                    .to_owned(),
            ));
        }
    };
    read_up_to(&mut file, limit).map_err(|e| ReadError::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{Access, OpenFailure, ReadError, RootDir, open_regular, read_up_to};
    use cap_std::ambient_authority;
    use cap_std::fs::Dir;
    use std::path::Path;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hollowscript-hostfs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_a_module_beside_the_entry_and_in_a_subdirectory() {
        let dir = temp_dir("read");
        std::fs::write(dir.join("util.hws"), "export constant two = 2\n").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/deep.hws"), "export constant three = 3\n").unwrap();
        let root = RootDir::open(&dir);
        assert_eq!(
            root.read_module("util.hws", 1024),
            Ok(b"export constant two = 2\n".to_vec())
        );
        assert!(root.read_module("sub/deep.hws", 1024).is_ok());
        assert_eq!(root.read_module("nope.hws", 1024), Err(ReadError::NotFound));
        assert_eq!(
            root.read_module("../x.hws", 1024),
            Err(ReadError::NotFound),
            "a name that climbs above the root is only a name nothing matches"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_that_differs_in_case_is_not_found() {
        let dir = temp_dir("case");
        std::fs::write(dir.join("lib.hws"), "x\n").unwrap();
        let root = RootDir::open(&dir);
        assert!(root.read_module("lib.hws", 1024).is_ok());
        assert_eq!(root.read_module("LIB.hws", 1024), Err(ReadError::NotFound));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_root_makes_everything_not_found() {
        let root = RootDir::open(Path::new("/nonexistent-hollowscript-root"));
        assert_eq!(root.read_module("a.hws", 10), Err(ReadError::NotFound));
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_not_found_whether_it_points_in_or_out() {
        let dir = temp_dir("symlink");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.hws"), "top secret\n").unwrap();
        let prog = dir.join("prog");
        std::fs::create_dir_all(&prog).unwrap();
        std::fs::write(prog.join("real.hws"), "fine\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.hws"), prog.join("leak.hws")).unwrap();
        std::os::unix::fs::symlink(prog.join("real.hws"), prog.join("alias.hws")).unwrap();
        std::os::unix::fs::symlink(&outside, prog.join("dir")).unwrap();
        let root = RootDir::open(&prog);
        assert!(root.read_module("real.hws", 1024).is_ok());
        assert_eq!(root.read_module("leak.hws", 1024), Err(ReadError::NotFound));
        assert_eq!(
            root.read_module("alias.hws", 1024),
            Err(ReadError::NotFound)
        );
        assert_eq!(
            root.read_module("dir/secret.hws", 1024),
            Err(ReadError::NotFound),
            "a directory that is a link is not walked through"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Reading a named pipe must come back at once, with a refusal, not wait
    /// for a writer that never comes. The test would hang, not fail, without
    /// the fix, so the read runs on a thread the test gives a few seconds.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_without_waiting() {
        let dir = temp_dir("fifo");
        let fifo = dir.join("pipe.hws");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !matches!(made, Ok(status) if status.success()) {
            eprintln!("skipping: mkfifo is not available");
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let root_path = dir.clone();
        std::thread::spawn(move || {
            let root = RootDir::open(&root_path);
            let _ = tx.send(root.read_module("pipe.hws", 1024));
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("reading a named pipe must not block");
        assert!(
            matches!(&result, Err(ReadError::Failed(reason)) if reason.contains("not a regular file")),
            "{result:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_file_with_a_second_hard_link_is_refused() {
        let dir = temp_dir("hardlink");
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "elsewhere\n").unwrap();
        let prog = dir.join("prog");
        std::fs::create_dir_all(&prog).unwrap();
        std::fs::hard_link(&outside, prog.join("linked.hws")).unwrap();
        std::fs::write(prog.join("alone.hws"), "mine\n").unwrap();
        let root = RootDir::open(&prog);
        assert!(root.read_module("alone.hws", 1024).is_ok());
        assert!(
            matches!(root.read_module("linked.hws", 1024), Err(ReadError::Failed(reason)) if reason.contains("hard link"))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_is_not_a_module() {
        let dir = temp_dir("isdir");
        std::fs::create_dir_all(dir.join("thing.hws")).unwrap();
        let root = RootDir::open(&dir);
        assert!(matches!(
            root.read_module("thing.hws", 1024),
            Err(ReadError::Failed(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A large file is read no further than the limit plus one byte, so the
    /// caller can tell it is too large without buffering it. A sparse file
    /// of 4 GiB stands in for one that would exhaust memory.
    #[cfg(unix)]
    #[test]
    fn a_large_file_is_read_only_up_to_the_limit() {
        let dir = temp_dir("large");
        {
            let file = std::fs::File::create(dir.join("big.hws")).unwrap();
            file.set_len(4 * 1024 * 1024 * 1024).unwrap();
        }
        let root = RootDir::open(&dir);
        let bytes = root.read_module("big.hws", 4096).unwrap();
        assert_eq!(bytes.len(), 4097);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_up_to_returns_everything_at_or_under_the_limit_and_one_over_beyond_it() {
        let mut exact = std::io::Cursor::new(vec![1u8; 10]);
        assert_eq!(read_up_to(&mut exact, 10).unwrap().len(), 10);
        let mut over = std::io::Cursor::new(vec![1u8; 11]);
        assert_eq!(read_up_to(&mut over, 10).unwrap().len(), 11);
        let mut far_over = std::io::Cursor::new(vec![1u8; 1000]);
        assert_eq!(read_up_to(&mut far_over, 10).unwrap().len(), 11);
    }

    #[test]
    fn open_regular_refuses_a_directory() {
        let dir = temp_dir("open-dir");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let handle = Dir::open_ambient_dir(&dir, ambient_authority()).unwrap();
        assert!(matches!(
            open_regular(&handle, Path::new("sub"), Access::Read),
            Err(OpenFailure::NotRegular)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
