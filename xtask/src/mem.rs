//! `cargo xtask mem`: real peak memory against section 11.4's own charge, for
//! the shapes `docs/design/security.md` ("what it deliberately does not
//! promise") and `docs/design/performance.md` ("Measured: a loop that keeps
//! what it builds") record. Built so the numbers in those two documents can
//! be reproduced and rechecked by anyone, rather than trusted on the word of
//! whichever session first measured them (the previous method,
//! `crates/hollowscript/examples/mem_probe.rs`, was a scratch harness never
//! committed).
//!
//! **Real peak memory** is read from `/usr/bin/time -l`'s "peak memory
//! footprint" line, the same source `docs/design/performance.md`'s own
//! benchmark harness (`xtask/src/bench.rs`) names as the platform-specific
//! way to do this that it does not itself implement. macOS only, for the
//! same reason: Linux's equivalent is `/usr/bin/time -v`'s "Maximum resident
//! set size", a different tool and a different line to parse, not attempted
//! here because nothing in this workspace runs its checks anywhere else yet.
//! On another platform this command says so and reports the charged figure
//! alone.
//!
//! **The run itself** is `crates/hollowscript/examples/mem_shape.rs`, not
//! `hollowscript-cli`: the CLI always runs with section 11's default
//! `maxMemoryBytes` (64 MiB), which the larger shape below charges past on
//! its own, so a harness that can raise the limit is needed to let the
//! shape run to completion at all.
//!
//! **The charged figure** is computed directly from section 11.4's own
//! table (16 bytes per list, plus 8 per element; 16 + b for a `String` of
//! `b` bytes), not read back from a run: the two must agree independently of
//! each other for the ratio to mean anything, and every shape here has a
//! charge simple enough to state in closed form.

#![allow(
    clippy::cast_precision_loss,
    reason = "reporting numbers for a human to read (megabytes, a ratio), not a value the \
              language or a budget check compares bit for bit, the same reasoning \
              xtask/src/bench.rs's own module doc gives"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

struct Shape {
    name: &'static str,
    /// The program's source, given `count`.
    source: fn(u64) -> String,
    /// The charged figure section 11.4's formula gives for this shape at
    /// `count`, in bytes.
    charged: fn(u64) -> u64,
}

/// A list of `count` `String`s, each built by interpolating a literal of
/// `text_len` bytes so that a fresh `String` value is allocated every
/// iteration rather than one constant literal being reused (see
/// `docs/design/performance.md`'s own note on why interpolation, not a bare
/// literal, is what makes this shape representative).
fn append_strings_source(count: u64, text_len: usize) -> String {
    let text = "x".repeat(text_len);
    format!(
        "variable xs: [String] = []\nvariable i = 0\nwhile i < {count} {{\n    xs = xs.append(\"${{\"{text}\"}}\")\n    i = i + 1\n}}\nreturn xs.length()\n"
    )
}

fn append_strings_charged(count: u64, text_len: u64) -> u64 {
    // The list's own node cost (16 + 8 per element) plus each String's
    // (16 + b), section 11.4's table.
    (16 + 8 * count) + count * (16 + text_len)
}

const SHAPES: &[Shape] = &[
    Shape {
        name: "append-empty-string",
        source: |count| append_strings_source(count, 0),
        charged: |count| append_strings_charged(count, 0),
    },
    Shape {
        name: "append-1000-char-string",
        source: |count| append_strings_source(count, 1000),
        charged: |count| append_strings_charged(count, 1000),
    },
    Shape {
        name: "range-int-list",
        source: |count| format!("return range(0, {count}).length()\n"),
        // range(0, count) is retained by nothing (the program only asks
        // its length), so the charged figure here is what the run
        // allocates building it: 16 + 8 per element, section 11.4.
        charged: |count| 16 + 8 * count,
    },
];

/// This shape's own default `count`: large enough that the pattern being
/// measured (a loop of appends, or one bulk call) dominates a run's fixed
/// costs, matching the counts `docs/design/security.md` and
/// `docs/design/performance.md` already report.
fn default_count(name: &str) -> u64 {
    if name == "range-int-list" {
        8_000_000
    } else {
        100_000
    }
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map_or(manifest.clone(), PathBuf::from)
}

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

/// Builds `mem_shape` (see this module's own doc) and returns its path.
fn build_mem_shape(root: &Path) -> Result<PathBuf, String> {
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "-p",
            "hollowscript",
            "--example",
            "mem_shape",
        ])
        .current_dir(root)
        .status()
        .map_err(|e| format!("could not run cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build --release -p hollowscript --example mem_shape failed".to_string());
    }
    Ok(target_dir(root)
        .join("release")
        .join("examples")
        .join("mem_shape"))
}

/// Runs `mem_shape program_path` under `/usr/bin/time -l` and returns the
/// "peak memory footprint" it reports, in bytes. `None` if `/usr/bin/time`
/// or its `-l` output is not available (any platform but macOS).
fn peak_memory_footprint(mem_shape: &Path, program_path: &Path) -> Result<Option<u64>, String> {
    if !Path::new("/usr/bin/time").exists() {
        return Ok(None);
    }
    let output = Command::new("/usr/bin/time")
        .args(["-l"])
        .arg(mem_shape)
        .arg(program_path)
        .output()
        .map_err(|e| format!("could not run /usr/bin/time: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "mem_shape exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(figure) = trimmed.strip_suffix("peak memory footprint") {
            if let Ok(bytes) = figure.trim().parse::<u64>() {
                return Ok(Some(bytes));
            }
        }
    }
    // `-l` is a BSD/macOS `time` extension; on a platform whose
    // `/usr/bin/time` does not understand it, there is simply no such line
    // to find, which is not an error this command can do anything about.
    Ok(None)
}

pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    let mut only: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--shape" => only = Some(iter.next().ok_or("--shape needs a name")?.clone()),
            other => return Err(format!("unknown option '{other}'")),
        }
    }

    let root = workspace_root();
    let mem_shape = build_mem_shape(&root)?;
    let program_path = target_dir(&root).join("xtask-mem-probe.hws");

    println!(
        "{:<26} {:>10} {:>14} {:>14} {:>8}",
        "shape", "count", "charged", "real (peak)", "ratio"
    );
    for shape in SHAPES {
        if let Some(name) = &only {
            if name != shape.name {
                continue;
            }
        }
        let count = default_count(shape.name);
        let source = (shape.source)(count);
        std::fs::write(&program_path, &source)
            .map_err(|e| format!("could not write {}: {e}", program_path.display()))?;
        let charged = (shape.charged)(count);
        let real = peak_memory_footprint(&mem_shape, &program_path)?;
        let ratio = real.map(|r| r as f64 / charged as f64);
        println!(
            "{:<26} {:>10} {:>12.1} MB {:>12} {:>7}",
            shape.name,
            count,
            charged as f64 / 1_000_000.0,
            real.map_or_else(
                || "not measured".to_string(),
                |r| format!("{:.1} MB", r as f64 / 1_000_000.0)
            ),
            ratio.map_or_else(|| "-".to_string(), |r| format!("{r:.1}x")),
        );
    }
    let _ = std::fs::remove_file(&program_path);

    if SHAPES
        .iter()
        .all(|s| only.as_deref().is_none_or(|n| n == s.name))
        && !Path::new("/usr/bin/time").exists()
    {
        println!(
            "\nReal peak memory is not measured on this platform: it needs `/usr/bin/time -l` \
             (macOS). The charged figures above are still exact."
        );
    }

    Ok(true)
}
