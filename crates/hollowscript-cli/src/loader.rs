//! Locating a program on disk and resolving its imports (section 10.3).
//!
//! Section 10.3 fixes two things a command-line host has to get right. The
//! *root* is the directory that holds the entry module, and the entry
//! module's canonical path is its name relative to that root, so a program
//! run as `hollowscript run app/main.hws` is the module `main.hws` rooted at
//! `app`, and an import of `"../shared/util"` from it climbs above the root
//! and is HS0605. Passing the whole path as the entry's canonical path
//! instead would silently move the root to the file system's own root and
//! turn that rejected import into a read of `shared/util.hws` next to `app`.
//!
//! Section 10.3 also fixes how the loader itself must behave: it "opens
//! every file relative to a handle for the root, without following links",
//! and returns content "only if the actual name of the file matches the
//! requested canonical path segment by segment with the exact case".
//! [`DirLoader`] does both by walking the canonical path one segment at a
//! time through a `cap_std::fs::Dir` handle opened once on the root: at each
//! level it lists the directory's real entries and requires a byte-exact
//! match (so a case-insensitive file system cannot hand `Util.hws` back for
//! a request of `util.hws`), and it refuses a segment that is itself a
//! symbolic link before ever opening or reading it, so neither a link that
//! stays inside the root nor one that leaves it is ever followed; both are
//! HS0601, "not found", exactly like a path that names nothing at all. The
//! `Dir` handle also means the check and the read are one step: nothing
//! swapped in after the directory listing can be resolved by a later,
//! separate open, because there is no separate open.
//!
//! What the walk and the read are is in `hollowscript-hostfs`, which the
//! language server shares, so the two cannot disagree: besides the above, the
//! file is opened without waiting (a named pipe is refused, never waited on),
//! must be a regular file with a single hard link, and is read through a
//! limit, never in full: at most the language's own size limit for one module
//! (4,194,304 bytes, HS0002 beyond it) and what is left of its limit for a
//! whole program (67,108,864 bytes, HS0612 beyond it), plus one byte, so the
//! front end sees the file is too large and reports it, without the host
//! having held the file in memory.
//!
//! [`Program::display`] turns a canonical path back into a path the person
//! who typed the command recognises, so diagnostics still name the file they
//! gave rather than the bare `main.hws` the language works in.

use hollowscript::{Loader, LoaderError};
use hollowscript_core::limits::structural::MAX_PROGRAM_BYTES;
use hollowscript_core::source::MAX_SOURCE_BYTES;
use hollowscript_hostfs::{ReadError, RootDir};
use std::path::{Path, PathBuf};

/// One program on disk: the root directory and the entry module's canonical
/// path within it.
pub(crate) struct Program {
    root: PathBuf,
    entry: String,
}

impl Program {
    /// The program whose entry module is the file at `path`.
    pub(crate) fn at(path: &str) -> Program {
        let file = Path::new(path);
        let root = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let entry = file.file_name().map_or_else(
            || "main.hws".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        Program { root, entry }
    }

    /// The entry module's canonical path: what the language calls this file.
    pub(crate) fn entry(&self) -> &str {
        &self.entry
    }

    /// A loader rooted at this program's root: a `cap_std` handle opened
    /// once, here, with the host's own ambient authority (the one filesystem
    /// access this module makes outside that handle), on a path the person
    /// running the command gave, not anything a script controls. If the root
    /// does not exist or is not a directory, every load through the returned
    /// loader reports the entry module (and everything else) as not found,
    /// matching what a plain read of a missing entry did before this loader
    /// existed.
    pub(crate) fn loader(&self) -> DirLoader {
        DirLoader {
            root: RootDir::open(&self.root),
            remaining: u64::try_from(MAX_PROGRAM_BYTES).unwrap_or(u64::MAX),
        }
    }

    /// `canonical_path` as a path on disk, for a diagnostic to name.
    /// Display Windows separators as `/`, matching canonical module paths
    /// and the cross-platform diagnostic format. Opening still uses `root`.
    pub(crate) fn display(&self, canonical_path: &str) -> String {
        if self.root == Path::new(".") {
            return canonical_path.to_owned();
        }
        let display = self
            .root
            .join(canonical_path)
            .to_string_lossy()
            .into_owned();
        #[cfg(windows)]
        let display = display.replace('\\', "/");
        display
    }
}

/// Reads modules from a program's root directory, per section 10.3 (see the
/// module documentation for what that means, and `hollowscript-hostfs` for
/// where it is done).
pub(crate) struct DirLoader {
    root: RootDir,
    /// What is left of the language's limit for the source of one program
    /// (HS0612), so that no read is allowed more than that plus one byte.
    remaining: u64,
}

impl Loader for DirLoader {
    fn load(&mut self, canonical_path: &str) -> Result<Vec<u8>, LoaderError> {
        let one_module = u64::try_from(MAX_SOURCE_BYTES).unwrap_or(u64::MAX);
        let limit = self.remaining.min(one_module);
        match self.root.read_module(canonical_path, limit) {
            Ok(bytes) => {
                self.remaining = self
                    .remaining
                    .saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                Ok(bytes)
            }
            Err(ReadError::NotFound) => Err(LoaderError::NotFound),
            Err(ReadError::Failed(reason)) => Err(LoaderError::Failed(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Program;
    use hollowscript::{Loader, LoaderError};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-loader-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_root_is_the_entrys_directory_and_the_entry_is_its_name() {
        let program = Program::at("app/main.hws");
        assert_eq!(program.entry(), "main.hws");
        assert_eq!(program.display("main.hws"), "app/main.hws");
        assert_eq!(program.display("sub/util.hws"), "app/sub/util.hws");

        let bare = Program::at("main.hws");
        assert_eq!(bare.entry(), "main.hws");
        assert_eq!(bare.display("main.hws"), "main.hws");
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_entry_paths_display_forward_slashes() {
        let program = Program::at(r"app\main.hws");
        assert_eq!(program.display("main.hws"), "app/main.hws");
        assert_eq!(program.display("sub/util.hws"), "app/sub/util.hws");
    }

    #[test]
    fn reads_a_module_beside_the_entry_and_in_a_subdirectory() {
        let dir = temp_dir("read");
        std::fs::write(dir.join("util.hws"), "export constant two = 2\n").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/deep.hws"), "export constant three = 3\n").unwrap();
        let program = Program::at(&dir.join("main.hws").to_string_lossy());
        let mut loader = program.loader();
        assert_eq!(
            loader
                .load("util.hws")
                .map(|b| String::from_utf8(b).unwrap()),
            Ok("export constant two = 2\n".to_owned())
        );
        assert!(loader.load("sub/deep.hws").is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_module_is_not_found_not_a_read_failure() {
        let dir = temp_dir("missing");
        let mut loader = Program::at(&dir.join("main.hws").to_string_lossy()).loader();
        assert_eq!(loader.load("nope.hws"), Err(LoaderError::NotFound));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_path_that_leaves_the_root_is_refused_without_being_read() {
        let dir = temp_dir("escape");
        let outside = dir.join("secret.hws");
        std::fs::write(&outside, "export constant s = 1\n").unwrap();
        let inner = dir.join("program");
        std::fs::create_dir_all(&inner).unwrap();
        let mut loader = Program::at(&inner.join("main.hws").to_string_lossy()).loader();
        assert_eq!(loader.load("../secret.hws"), Err(LoaderError::NotFound));
        assert_eq!(
            loader.load(&outside.to_string_lossy()),
            Err(LoaderError::NotFound),
            "an absolute path is refused too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A module import that resolves, on disk, to a symbolic link
    /// pointing outside the root must not be read: section 10.3 requires it
    /// to be treated as not found, whether the link is followed to a file
    /// (which would leak that file's content into the diagnostic or the
    /// program) or not.
    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_root_is_not_found_not_read() {
        let dir = temp_dir("symlink-out");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("secret.hws"),
            "export constant secretText = \"top secret outside root\"\n",
        )
        .unwrap();
        let prog = dir.join("prog");
        std::fs::create_dir_all(&prog).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.hws"), prog.join("leak.hws")).unwrap();
        let mut loader = Program::at(&prog.join("main.hws").to_string_lossy()).loader();
        assert_eq!(
            loader.load("leak.hws"),
            Err(LoaderError::NotFound),
            "a symlink to a file outside the root must never be read"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Even a symbolic link that happens to stay inside the root is
    /// refused, because section 10.3 says the loader "opens every file
    /// relative to a handle for the root, without following links" with no
    /// exception for a link that would have resolved to somewhere legitimate.
    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_root_is_still_not_found() {
        let dir = temp_dir("symlink-in");
        std::fs::write(dir.join("real.hws"), "export constant v = 1\n").unwrap();
        std::os::unix::fs::symlink(dir.join("real.hws"), dir.join("alias.hws")).unwrap();
        let mut loader = Program::at(&dir.join("main.hws").to_string_lossy()).loader();
        assert!(loader.load("real.hws").is_ok());
        assert_eq!(loader.load("alias.hws"), Err(LoaderError::NotFound));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The loader must return content only when the requested
    /// canonical path matches the file's real name segment by segment with
    /// the exact case, even on a file system that would itself resolve the
    /// open case-insensitively.
    #[test]
    fn a_case_mismatched_name_is_not_found() {
        let dir = temp_dir("case");
        std::fs::write(dir.join("lib.hws"), "export constant v = 7\n").unwrap();
        let mut loader = Program::at(&dir.join("main.hws").to_string_lossy()).loader();
        assert!(loader.load("lib.hws").is_ok());
        assert_eq!(
            loader.load("LIB.hws"),
            Err(LoaderError::NotFound),
            "a request that differs only in case from the real file name must not resolve"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Importing a named pipe must not wait for a writer: the host reports
    /// it as unreadable (HS0607) at once. The load runs on a thread the test
    /// gives a few seconds, so a regression fails instead of hanging.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_without_waiting() {
        let dir = temp_dir("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join("pipe.hws"))
            .status();
        if !matches!(made, Ok(status) if status.success()) {
            eprintln!("skipping: mkfifo is not available");
            return;
        }
        let entry = dir.join("main.hws").to_string_lossy().into_owned();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut loader = Program::at(&entry).loader();
            let _ = tx.send(loader.load("pipe.hws"));
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("importing a named pipe must not block");
        assert!(matches!(result, Err(LoaderError::Failed(_))), "{result:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A module larger than the language's own limit is read no further than
    /// that limit plus one byte, so the front end can report HS0002 without
    /// the host having buffered the file (a sparse 1.5 GiB file stands in for
    /// one that would exhaust memory).
    #[cfg(unix)]
    #[test]
    fn a_huge_module_is_read_only_up_to_the_source_limit() {
        let dir = temp_dir("huge");
        {
            let file = std::fs::File::create(dir.join("big.hws")).unwrap();
            file.set_len(3 * 512 * 1024 * 1024).unwrap();
        }
        let mut loader = Program::at(&dir.join("main.hws").to_string_lossy()).loader();
        let bytes = loader.load("big.hws").unwrap();
        assert_eq!(
            bytes.len(),
            hollowscript_core::source::MAX_SOURCE_BYTES + 1,
            "one byte past the limit is all the front end needs to refuse it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What one program may import in total is bounded the same way: once
    /// the program's allowance is spent, a read is allowed one byte more
    /// than what is left, and no more.
    #[test]
    fn imports_share_the_programs_total_source_allowance() {
        let dir = temp_dir("total");
        std::fs::write(dir.join("a.hws"), vec![b'x'; 1000]).unwrap();
        std::fs::write(dir.join("b.hws"), vec![b'y'; 1000]).unwrap();
        let mut loader = Program::at(&dir.join("main.hws").to_string_lossy()).loader();
        loader.remaining = 1500;
        assert_eq!(loader.load("a.hws").map(|b| b.len()), Ok(1000));
        assert_eq!(
            loader.load("b.hws").map(|b| b.len()),
            Ok(501),
            "500 bytes were left, so 501 are read and the front end reports HS0612"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_module_that_is_a_second_hard_link_is_refused() {
        let dir = temp_dir("hardlink");
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "export constant s = 1\n").unwrap();
        let prog = dir.join("prog");
        std::fs::create_dir_all(&prog).unwrap();
        std::fs::hard_link(&outside, prog.join("linked.hws")).unwrap();
        let mut loader = Program::at(&prog.join("main.hws").to_string_lossy()).loader();
        assert!(matches!(
            loader.load("linked.hws"),
            Err(LoaderError::Failed(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
