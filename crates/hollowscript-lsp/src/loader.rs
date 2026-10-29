//! Loading an imported module for a checker run over an open document
//! (section 10.3), the same way the CLI does, but with open documents
//! taking priority over disk.
//!
//! The checker calls back with a canonical path (`util.hws`, `sub/deep.hws`)
//! relative to the entry module's own directory, which
//! [`crate::paths::root_and_entry`] takes as the program's root. [`DocLoader`] joins that path onto the root
//! exactly as [`hollowscript_cli`'s loader](../../hollowscript-cli/src/loader.rs)
//! does (rejecting an absolute path or one that climbs above the root), then
//! looks the resulting path up as a `file://` URI among the documents the
//! client has open: an editor's unsaved buffer is what a `textDocument/*`
//! request should see, not the last-saved bytes on disk. Only when there is
//! no open document at that URI does it fall back to reading the file.
//!
//! The file is read the way the command line host reads a module, by the
//! same code (`hollowscript-hostfs`): relative to a handle on the root, so no
//! symbolic link is followed at any level (a link to a file elsewhere is not
//! found, and its tokens are never quoted in a diagnostic), opened without
//! waiting (a named pipe is refused, never waited on, which would hang the
//! whole server), accepted only as a regular file with a single name, and read
//! through the language's own limits for one module and for a program, never
//! in full.

use hollowscript_core::LoadModuleError;
use hollowscript_core::limits::structural::MAX_PROGRAM_BYTES;
use hollowscript_core::source::MAX_SOURCE_BYTES;
use hollowscript_hostfs::{ReadError, RootDir};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Resolves import targets for one document, against its open siblings and
/// disk.
pub(crate) struct DocLoader<'a> {
    root: PathBuf,
    /// The root, opened once, for reading modules from disk.
    disk: RootDir,
    documents: &'a BTreeMap<String, String>,
    /// What is left of the language's limit for the source of one program
    /// (HS0612): no read from disk is allowed more than that plus one byte.
    remaining: u64,
}

impl<'a> DocLoader<'a> {
    pub(crate) fn new(root: PathBuf, documents: &'a BTreeMap<String, String>) -> DocLoader<'a> {
        let disk = RootDir::open(&root);
        DocLoader {
            root,
            disk,
            documents,
            remaining: u64::try_from(MAX_PROGRAM_BYTES).unwrap_or(u64::MAX),
        }
    }

    /// `canonical_path` joined onto the root, or `None` if it would leave it.
    fn resolve(&self, canonical_path: &str) -> Option<PathBuf> {
        let relative = Path::new(canonical_path);
        if relative.is_absolute() {
            return None;
        }
        let mut out = self.root.clone();
        for component in relative.components() {
            match component {
                Component::Normal(part) => out.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        Some(out)
    }

    /// The URI an open document at `canonical_path` would have, if it were
    /// under this loader's root: `None` for a path that leaves the root.
    pub(crate) fn uri_of(&self, canonical_path: &str) -> Option<String> {
        self.resolve(canonical_path)
            .map(|p| crate::paths::from_path(&p))
    }

    /// Loads `canonical_path`, the shape [`hollowscript_check::check`]'s
    /// `load_module` callback needs.
    pub(crate) fn load(&mut self, canonical_path: &str) -> Result<String, LoadModuleError> {
        let Some(path) = self.resolve(canonical_path) else {
            return Err(LoadModuleError::NotFound);
        };
        let uri = crate::paths::from_path(&path);
        if let Some(text) = self.documents.get(&uri) {
            return Ok(text.clone());
        }
        let one_module = u64::try_from(MAX_SOURCE_BYTES).unwrap_or(u64::MAX);
        let limit = self.remaining.min(one_module);
        let bytes = match self.disk.read_module(canonical_path, limit) {
            Ok(bytes) => bytes,
            Err(ReadError::NotFound) => return Err(LoadModuleError::NotFound),
            Err(ReadError::Failed(reason)) => return Err(LoadModuleError::Failed(reason)),
        };
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.remaining = self.remaining.saturating_sub(length);
        if length > limit {
            // One byte past the limit: all the checker needs to refuse the
            // module (HS0002, or HS0612 for the program), so it need not be
            // valid UTF-8 to be told so.
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        String::from_utf8(bytes)
            .map_err(|_| LoadModuleError::Failed("stream did not contain valid UTF-8".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::DocLoader;
    use hollowscript_core::LoadModuleError;
    use std::collections::BTreeMap;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-lsp-loader-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_a_module_from_disk_when_no_document_overrides_it() {
        let dir = temp_dir("disk");
        std::fs::write(dir.join("util.hws"), "export constant two = 2\n").unwrap();
        let documents = BTreeMap::new();
        let mut loader = DocLoader::new(dir.clone(), &documents);
        assert_eq!(
            loader.load("util.hws"),
            Ok("export constant two = 2\n".to_owned())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_open_document_overrides_the_file_on_disk() {
        let dir = temp_dir("override");
        std::fs::write(dir.join("util.hws"), "export constant two = 2\n").unwrap();
        let uri = crate::paths::from_path(&dir.join("util.hws"));
        let mut documents = BTreeMap::new();
        documents.insert(uri, "export constant two = 99\n".to_owned());
        let mut loader = DocLoader::new(dir.clone(), &documents);
        assert_eq!(
            loader.load("util.hws"),
            Ok("export constant two = 99\n".to_owned())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_document_open_under_an_unsaved_name_is_not_reached_by_an_import() {
        // An unopened, never-saved module simply is not found: the same
        // outcome as the CLI loader gives for a missing file.
        let dir = temp_dir("missing");
        let documents = BTreeMap::new();
        let mut loader = DocLoader::new(dir.clone(), &documents);
        assert_eq!(loader.load("nope.hws"), Err(LoadModuleError::NotFound));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_path_that_leaves_the_root_is_refused() {
        let dir = temp_dir("escape");
        let inner = dir.join("program");
        std::fs::create_dir_all(&inner).unwrap();
        let documents = BTreeMap::new();
        let mut loader = DocLoader::new(inner, &documents);
        assert_eq!(loader.load("../secret.hws"), Err(LoadModuleError::NotFound));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An import whose file is a symbolic link to a file outside the
    /// repository: it is not found, and what is in the file is neither read
    /// nor quoted. The file here is not valid HollowScript, so that, were it
    /// read, the diagnostics would repeat its tokens.
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_file_outside_the_root_is_not_found_and_not_quoted() {
        let dir = temp_dir("symlink-out");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("creds.hws"),
            "constant = oopsSecretValue12345 !!\n",
        )
        .unwrap();
        let prog = dir.join("prog");
        std::fs::create_dir_all(&prog).unwrap();
        std::os::unix::fs::symlink(outside.join("creds.hws"), prog.join("creds.hws")).unwrap();
        let documents = BTreeMap::new();
        let mut loader = DocLoader::new(prog.clone(), &documents);
        assert_eq!(loader.load("creds.hws"), Err(LoadModuleError::NotFound));

        // End to end, through the same path a document's diagnostics take.
        let uri = crate::paths::from_path(&prog.join("main.hws"));
        let workspace =
            crate::workspace::build(&uri, "import x from \"./creds\"\nreturn x\n", &documents)
                .unwrap();
        let text = format!("{:?}", workspace.diagnostics);
        assert!(text.contains("HS0601"), "{text}");
        assert!(!text.contains("oopsSecretValue12345"), "{text}");
        assert!(workspace.imports.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_root_is_not_followed_either() {
        let dir = temp_dir("symlink-in");
        std::fs::write(dir.join("real.hws"), "export constant v = 1\n").unwrap();
        std::os::unix::fs::symlink(dir.join("real.hws"), dir.join("alias.hws")).unwrap();
        let documents = BTreeMap::new();
        let mut loader = DocLoader::new(dir.clone(), &documents);
        assert!(loader.load("real.hws").is_ok());
        assert_eq!(loader.load("alias.hws"), Err(LoadModuleError::NotFound));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An import of a symbolic link to a named pipe used to hang the server
    /// in the read. It is refused at once now (the load runs on a thread the
    /// test gives a few seconds, so a regression fails instead of hanging).
    /// The link itself is not followed, and the pipe itself is refused too.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_without_waiting() {
        let dir = temp_dir("fifo");
        let pipe = dir.join("pipe.hws");
        let made = std::process::Command::new("mkfifo").arg(&pipe).status();
        if !matches!(made, Ok(status) if status.success()) {
            eprintln!("skipping: mkfifo is not available");
            return;
        }
        std::os::unix::fs::symlink(&pipe, dir.join("link.hws")).unwrap();
        let root = dir.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let documents = BTreeMap::new();
            let mut loader = DocLoader::new(root, &documents);
            let _ = tx.send((loader.load("link.hws"), loader.load("pipe.hws")));
        });
        let (through_link, direct) = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("importing a named pipe must not block the server");
        assert_eq!(through_link, Err(LoadModuleError::NotFound));
        assert!(
            matches!(direct, Err(LoadModuleError::Failed(_))),
            "{direct:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_file_with_a_second_hard_link_is_refused() {
        let dir = temp_dir("hardlink");
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "export constant s = 1\n").unwrap();
        let prog = dir.join("prog");
        std::fs::create_dir_all(&prog).unwrap();
        std::fs::hard_link(&outside, prog.join("linked.hws")).unwrap();
        let documents = BTreeMap::new();
        let mut loader = DocLoader::new(prog, &documents);
        assert!(matches!(
            loader.load("linked.hws"),
            Err(LoadModuleError::Failed(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file far larger than the language's limit for one module is read
    /// only that far, and the checker is handed enough to say it is too
    /// large (HS0002).
    #[cfg(unix)]
    #[test]
    fn a_huge_file_is_read_only_up_to_the_source_limit() {
        let dir = temp_dir("huge");
        {
            let file = std::fs::File::create(dir.join("big.hws")).unwrap();
            file.set_len(3 * 512 * 1024 * 1024).unwrap();
        }
        let documents = BTreeMap::new();
        let mut loader = DocLoader::new(dir.clone(), &documents);
        let text = loader.load("big.hws").unwrap();
        assert_eq!(text.len(), hollowscript_core::source::MAX_SOURCE_BYTES + 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
