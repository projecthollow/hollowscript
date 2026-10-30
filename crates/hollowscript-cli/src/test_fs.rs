//! Native filesystem fixtures for capability tests.

/// Make a logically large file without allocating its full size on NTFS.
pub(crate) fn sparse_file(path: &std::path::Path, len: u64) {
    let file = std::fs::File::create(path).unwrap();
    #[cfg(windows)]
    {
        let output = std::process::Command::new("fsutil")
            .args(["sparse", "setflag"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "marking the NTFS fixture sparse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    file.set_len(len).unwrap();
}
