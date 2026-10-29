//! Compiles and runs `tests/c_host.c`, a real C program, against the
//! library this crate builds, so that the crate's whole reason for
//! existing (that a host which is not Rust can genuinely embed
//! HollowScript) is checked by actually being a C host, not only by Rust
//! code that happens to go through `extern "C"` functions.
//!
//! Skipped, printing why, rather than failed, when no C compiler is on the
//! path: this crate's own tests must not depend on one being installed,
//! but a session that has one should exercise the real ABI boundary.
//!
//! The workspace lint policy denies `unwrap`, `expect` and `panic` outside
//! test code, and this file is a plain helper module as far as clippy's
//! that-is-a-test detection goes, not itself `#[cfg(test)]`, so every
//! fallible step below reports its problem as an `Err(String)` instead;
//! the one `#[test]` function returns `Result<(), String>`, which the test
//! harness turns into a failure with the message printed, and nothing here
//! reaches an `unwrap`/`expect`/`panic` that clippy would have to allow.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The directory this test binary's own executable was built into, which is
/// also where cargo puts the `cdylib` this crate builds (`crate-type =
/// ["staticlib", "cdylib", "lib"]` in `Cargo.toml`): both are outputs of the
/// same `cargo test` invocation, placed in the same profile directory.
fn build_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("the test binary's own path: {e}"))?;
    // `<target>/<profile>/deps/<test-binary>` -> `<target>/<profile>`.
    exe.parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            "a test binary is always two directories below its profile directory".to_owned()
        })
}

/// The dynamic library this crate builds, by this platform's naming
/// convention.
fn dylib_path(dir: &Path) -> PathBuf {
    let name = if cfg!(target_os = "windows") {
        "hollowscript_capi.dll"
    } else if cfg!(target_os = "macos") {
        "libhollowscript_capi.dylib"
    } else {
        "libhollowscript_capi.so"
    };
    dir.join(name)
}

#[test]
fn a_real_c_program_loads_calls_and_cancels_through_the_c_abi() -> Result<(), String> {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    if Command::new(&cc).arg("--version").output().is_err() {
        eprintln!("skipping: no C compiler ({cc}) is on the path");
        return Ok(());
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let build_dir = build_dir()?;
    let dylib = dylib_path(&build_dir);
    if !dylib.is_file() {
        return Err(format!(
            "expected `cargo test` to have built {dylib:?} alongside the test binary; \
             crate-type in Cargo.toml must include \"cdylib\""
        ));
    }

    let exe_path = build_dir.join("hollowscript_capi_c_host_test");
    let status = Command::new(&cc)
        .arg(manifest_dir.join("tests/c_host.c"))
        .arg("-I")
        .arg(manifest_dir.join("include"))
        .arg("-L")
        .arg(&build_dir)
        .arg("-lhollowscript_capi")
        .arg("-lpthread")
        .arg("-o")
        .arg(&exe_path)
        .status()
        .map_err(|e| format!("running {cc} failed: {e}"))?;
    if !status.success() {
        return Err("cc failed to compile and link tests/c_host.c".to_owned());
    }

    let mut run = Command::new(&exe_path);
    // The dynamic linker must find the library at run time too, not only at
    // link time: the platform-specific search path variable is set to the
    // directory it was just built into.
    if cfg!(target_os = "macos") {
        run.env("DYLD_LIBRARY_PATH", &build_dir);
    } else if !cfg!(target_os = "windows") {
        run.env("LD_LIBRARY_PATH", &build_dir);
    }
    let output = run
        .output()
        .map_err(|e| format!("running the compiled C host failed: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "the C host reported a failure:\nstdout: {stdout}\nstderr: {stderr}"
        ));
    }
    if !stdout.contains("all checks passed") {
        return Err(format!(
            "expected \"all checks passed\" in stdout: {stdout}"
        ));
    }
    Ok(())
}
