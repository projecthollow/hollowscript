//! Converting between an LSP `file://` URI and a path on disk.
//!
//! This crate declares no third-party dependency (the `cap-std` it links comes
//! through `hollowscript-hostfs`, and opens files, not URIs), so there is no
//! general URI parser here: only what a `file://` URI needs, which is a scheme
//! prefix and percent-encoding of the handful of bytes a path may hold that a
//! URI cannot (mostly space and `#`). Every URI this server hands back was
//! built by [`from_path`], so the two are exact inverses for any URI this
//! server itself produces; a URI from the client is decoded the same way,
//! which covers the paths a real editor sends.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The `file://` URI for an absolute path.
pub(crate) fn from_path(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            // Unreserved, plus '/' so the path's own separators survive, and
            // ':' for a Windows drive letter's colon.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(byte as char);
            }
            // `write!` to a `String` is infallible; there is nothing useful
            // to do with an `Err` here, so it is dropped rather than unwrapped.
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// The path an LSP `file://` URI names, or `None` for any other scheme (a
/// document this server did not open from disk, which every cross-file
/// feature then simply has nothing to resolve against).
pub(crate) fn to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(rest)))
}

/// The directory and file name of the module a URI names, the same split
/// section 10.3 makes for a command-line entry module: the directory is the
/// program's root and the file name is the entry module's own canonical
/// path.
pub(crate) fn root_and_entry(uri: &str) -> Option<(PathBuf, String)> {
    let path = to_path(uri)?;
    let entry = path.file_name()?.to_string_lossy().into_owned();
    let root = path.parent()?.to_path_buf();
    Some((root, entry))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(byte) = bytes.get(i) {
        if *byte == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok());
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(*byte);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{from_path, root_and_entry, to_path};
    use std::path::Path;

    #[test]
    fn a_plain_path_round_trips() {
        let uri = from_path(Path::new("/a/b/c.hws"));
        assert_eq!(uri, "file:///a/b/c.hws");
        assert_eq!(to_path(&uri), Some(Path::new("/a/b/c.hws").to_path_buf()));
    }

    #[test]
    fn a_space_is_percent_encoded_and_decoded_back() {
        let uri = from_path(Path::new("/a b/c.hws"));
        assert_eq!(uri, "file:///a%20b/c.hws");
        assert_eq!(to_path(&uri), Some(Path::new("/a b/c.hws").to_path_buf()));
    }

    #[test]
    fn a_non_file_uri_has_no_path() {
        assert_eq!(to_path("untitled:Untitled-1"), None);
    }

    #[test]
    fn root_and_entry_splits_the_directory_from_the_file_name() {
        let (root, entry) = root_and_entry("file:///a/b/main.hws").unwrap();
        assert_eq!(root, Path::new("/a/b"));
        assert_eq!(entry, "main.hws");
    }
}
