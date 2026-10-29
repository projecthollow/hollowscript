//! The benchmark harness of `docs/design/performance.md`: cold start,
//! instance load time, throughput and peak memory for a small set of
//! representative programs, and binary size for the CLI and the WebAssembly
//! build.
//!
//! No heavy dependency: `hollowscript` (the embedding facade a real host
//! actually calls, not a private fast path into the VM) and `std::time`,
//! the same reasoning `docs/design/performance.md` already recorded before
//! this existed. This prints numbers rather than gating on them; a separate
//! `scripts/bench-budget` script (see that document) compares this run's
//! own output against the budgets recorded there.
//!
//! Five of the programs below use byte-for-byte the same source as the
//! previous implementation's own benchmark suite, so its committed
//! baseline numbers are a fair comparison; that baseline belongs to the
//! other repository and is read by hand, once, to write
//! `docs/design/performance.md`'s own comparison table, not read or written
//! by this command.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "reporting numbers for a human to read (ops/sec, megabytes, a rounded thousands \
              separator), not a value the language or a budget check compares bit for bit"
)]

use hollowscript::{Control, Limits, LoadOutcome, NoLoader, Options};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How many timed samples a throughput measurement takes, after one
/// discarded warm-up run, matching the previous implementation's own
/// benchmark methodology (`SAMPLES = 5`) so the two numbers are
/// produced the same way.
const SAMPLES: usize = 5;

/// A program to measure, and how many "work units" its wall time is
/// divided by to report a throughput figure that means "one iteration of
/// the thing this program is about", not literally one statement.
struct Program {
    name: &'static str,
    source: &'static str,
    work_units: u64,
}

/// The five programs also measured by the previous implementation's own
/// benchmark suite, with byte-for-byte the same source text, so the two
/// implementations' numbers describe the same work. See that file for why
/// each one is shaped the way it is (arithmetic overhead, call overhead at
/// volume, closure dispatch through the standard library, record
/// construction and field lookup, and a deliberately quadratic string
/// pattern measured honestly rather than hidden).
const COMPARABLE: &[Program] = &[
    Program {
        name: "arithmetic-loop",
        work_units: 200_000,
        source: "variable sum = 0\nvariable i = 0\nwhile i < 200000 {\n    sum = sum + i * 2 - 1\n    i = i + 1\n}\nreturn sum\n",
    },
    Program {
        name: "recursive-fibonacci",
        work_units: 150_049,
        source: "fn fib(n: Int): Int {\n    if n <= 1 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nreturn fib(24)\n",
    },
    Program {
        name: "list-map-filter-reduce",
        work_units: 2_000,
        source: "variable numbers: [Int] = []\nvariable i = 0\nwhile i < 2000 {\n    numbers = numbers.append(i)\n    i = i + 1\n}\nconstant doubled = numbers.map(fn(n: Int): Int { return n * 2 })\nconstant large = doubled.filter(fn(n: Int): Bool { return n > 2000 })\nreturn large.reduce(0, fn(acc: Int, n: Int): Int { return acc + n })\n",
    },
    Program {
        name: "record-field-access",
        work_units: 2_000,
        source: "record Point = { x: Int, y: Int }\n\nvariable points: [Point] = []\nvariable i = 0\nwhile i < 2000 {\n    points = points.append({ x: i, y: i * 2 })\n    i = i + 1\n}\nreturn points.reduce(0, fn(acc: Int, p: Point): Int { return acc + p.x + p.y })\n",
    },
    Program {
        name: "string-interpolation-loop",
        work_units: 3_000,
        source: "variable text = \"\"\nvariable i = 0\nwhile i < 3000 {\n    text = \"${text}x\"\n    i = i + 1\n}\nreturn text.length()\n",
    },
];

/// Workloads the specification's preamble asks for beyond the five above:
/// a sort, a JSON round trip and a closure-heavy workload distinct from
/// `recursive-fibonacci`'s plain function calls. No baseline entry exists
/// for these; see this crate's own report for their numbers on their own.
const EXTRA: &[Program] = &[
    Program {
        name: "list-sort",
        work_units: 5_000,
        source: "variable numbers: [Int] = []\nvariable i = 0\nvariable seed = 1\nwhile i < 5000 {\n    seed = mod(seed * 1103515245 + 12345, 2147483648)\n    numbers = numbers.append(mod(seed, 1000000))\n    i = i + 1\n}\nreturn numbers.sort(fn(a: Int, b: Int): Int { return a - b }).length()\n",
    },
    Program {
        name: "json-roundtrip",
        work_units: 1_000,
        source: "variable entries: [Json] = []\nvariable i = 0\nwhile i < 1000 {\n    constant tags = Json.Array([Json.String(\"a\"), Json.String(\"b\"), Json.String(\"c\")])\n    entries = entries.append(Json.Object([\"id\": Json.Int(i), \"name\": Json.String(\"item-${i}\"), \"tags\": tags]))\n    i = i + 1\n}\nconstant encoded = json.stringify(Json.Array(entries))\nmatch json.parse(encoded) {\n    Ok(value) {\n        return encoded.length()\n    }\n    Err(error) {\n        return -1\n    }\n}\n",
    },
    Program {
        name: "closure-heavy",
        work_units: 4_000,
        source: "fn makeAdder(n: Int): fn(Int): Int {\n    return fn(x: Int): Int {\n        return x + n\n    }\n}\n\nvariable adders: [fn(Int): Int] = []\nvariable i = 0\nwhile i < 4000 {\n    adders = adders.append(makeAdder(i))\n    i = i + 1\n}\nreturn adders.reduce(0, fn(acc: Int, f: fn(Int): Int): Int { return acc + f(1) })\n",
    },
];

fn limits() -> Limits {
    Limits::default()
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    let mid = samples.len() / 2;
    samples.get(mid).copied().unwrap_or_default()
}

/// Loads and runs `program.source` once, returning the wall time of the
/// whole call, through the same `hollowscript` facade a real host uses.
fn measure_once(source: &str) -> Result<Duration, String> {
    let options = Options {
        limits: limits(),
        ..Options::default()
    };
    let start = Instant::now();
    let outcome = hollowscript::load(
        source,
        "bench.hws",
        &options,
        &mut NoLoader,
        &mut |_, _| None,
        &mut Control::none(),
    );
    let elapsed = start.elapsed();
    match outcome {
        LoadOutcome::CheckFailed(diagnostics) => Err(format!(
            "benchmark program failed to check: {:?}",
            diagnostics
                .iter()
                .map(|d| d.code.to_string())
                .collect::<Vec<_>>()
        )),
        LoadOutcome::Instance(mut instance) => {
            let _ = instance.dispose();
            Ok(elapsed)
        }
    }
}

/// Just the `load` half (check, compile, verify, initialise), discarding a
/// warm-up run for the same reason `measure_once`'s callers do.
fn measure_load_only(source: &str) -> Result<Duration, String> {
    let options = Options::default();
    let start = Instant::now();
    let outcome = hollowscript::load(
        source,
        "bench.hws",
        &options,
        &mut NoLoader,
        &mut |_, _| None,
        &mut Control::none(),
    );
    let elapsed = start.elapsed();
    match outcome {
        LoadOutcome::CheckFailed(diagnostics) => Err(format!(
            "benchmark program failed to check: {:?}",
            diagnostics
                .iter()
                .map(|d| d.code.to_string())
                .collect::<Vec<_>>()
        )),
        LoadOutcome::Instance(mut instance) => {
            let _ = instance.dispose();
            Ok(elapsed)
        }
    }
}

struct BenchResult {
    name: &'static str,
    ops_per_second: f64,
    load_ms: f64,
}

fn run_one(program: &Program) -> Result<BenchResult, String> {
    measure_once(program.source)?; // warm-up, discarded
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        samples.push(measure_once(program.source)?);
    }
    let run_ms = median(samples).as_secs_f64() * 1000.0;
    let ops_per_second = program.work_units as f64 / (run_ms / 1000.0);

    measure_load_only(program.source)?; // warm-up, discarded
    let mut load_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        load_samples.push(measure_load_only(program.source)?);
    }
    let load_ms = median(load_samples).as_secs_f64() * 1000.0;

    Ok(BenchResult {
        name: program.name,
        ops_per_second,
        load_ms,
    })
}

/// The workspace root, one level above this package.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map_or(manifest.clone(), PathBuf::from)
}

/// Builds `hollowscript-cli` at the given profile and returns the built
/// binary's path, or an error naming what went wrong.
fn build_cli(root: &Path, profile: &str) -> Result<PathBuf, String> {
    let status = Command::new("cargo")
        .args(["build", "--profile", profile, "-p", "hollowscript-cli"])
        .current_dir(root)
        .status()
        .map_err(|e| format!("could not run cargo build: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build --profile {profile} failed"));
    }
    let dir_name = if profile == "dev" { "debug" } else { profile };
    Ok(target_dir(root).join(dir_name).join("hollowscript"))
}

/// Cargo's own output directory: `$CARGO_TARGET_DIR` when the caller (or, in
/// this workspace, a worktree's own convention; see the preamble every
/// workstream reads) set one, otherwise the workspace's default `target`.
fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

/// Cold start: wall time from spawning the CLI process to it exiting, for a
/// minimal program the CLI runs and prints one line for. This measures
/// process start plus load plus run plus the process's own teardown, not
/// only "time to first output" in isolation: a child process's stdout is
/// only observed once it is fully captured after `wait`, and adding a
/// streaming reader to get a truer "time to first byte" would be more
/// machinery than the number is worth here. Documented as a limitation in
/// `docs/design/performance.md` rather than silently reported as something
/// more precise than it is.
fn cold_start(cli: &Path, program_path: &Path) -> Result<Duration, String> {
    // One discarded run so the OS's own page cache for the binary is warm,
    // matching every other measurement's warm-up discipline.
    let _ = Command::new(cli).arg("run").arg(program_path).output();
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let output = Command::new(cli)
            .arg("run")
            .arg(program_path)
            .output()
            .map_err(|e| format!("could not run the CLI: {e}"))?;
        let elapsed = start.elapsed();
        if !output.status.success() {
            return Err(format!(
                "hollowscript-cli run exited with {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        samples.push(elapsed);
    }
    Ok(median(samples))
}

fn file_size(path: &Path) -> Result<u64, String> {
    std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| format!("could not read {}: {e}", path.display()))
}

pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    let mut skip_wasm = false;
    for arg in args {
        match arg.as_str() {
            "--no-wasm" => skip_wasm = true,
            other => return Err(format!("unknown option '{other}'")),
        }
    }

    println!(
        "Running {} representative programs, {SAMPLES} timed samples each (median reported), after one discarded warm-up run.\n",
        COMPARABLE.len() + EXTRA.len()
    );

    println!("## Throughput and load time\n");
    println!("{:<26} {:>14} {:>12}", "program", "ops/sec", "load (ms)");
    for program in COMPARABLE.iter().chain(EXTRA) {
        let result = run_one(program)?;
        println!(
            "{:<26} {:>14} {:>12.3}",
            result.name,
            format_ops(result.ops_per_second),
            result.load_ms
        );
    }

    println!("\n## Binary size (min-size profile)\n");
    let root = workspace_root();
    let cli_path = build_cli(&root, "min-size")?;
    let cli_size = file_size(&cli_path)?;
    println!(
        "hollowscript-cli            {:>10.2} MB",
        cli_size as f64 / 1_000_000.0
    );

    if skip_wasm {
        println!("hollowscript-wasm           skipped (--no-wasm)");
    } else {
        match build_wasm_size(&root) {
            Ok(size) => println!(
                "hollowscript-wasm           {:>10.2} MB",
                size as f64 / 1_000_000.0
            ),
            Err(reason) => println!("hollowscript-wasm           could not be measured: {reason}"),
        }
    }

    println!("\n## Cold start (hollowscript-cli run, min-size binary)\n");
    let program_path = target_dir(&root).join("bench-cold-start.hws");
    std::fs::write(
        &program_path,
        "uses print\n\neffect print(\"hi\")\nreturn 0\n",
    )
    .map_err(|e| format!("could not write {}: {e}", program_path.display()))?;
    let elapsed = cold_start(&cli_path, &program_path)?;
    println!("median wall time: {:.2} ms", elapsed.as_secs_f64() * 1000.0);
    let _ = std::fs::remove_file(&program_path);

    println!(
        "\nPeak memory is not measured by this command: it needs a platform-specific way to \
         read a child process's own maximum resident set size (`/usr/bin/time -l` on macOS, \
         `/usr/bin/time -v` on Linux), which is a `docs/design/performance.md` limitation, not \
         a number to fabricate here. Run `/usr/bin/time -l target/min-size/hollowscript-cli run \
         <program>` by hand and read \"maximum resident set size\" until this command grows a \
         portable way to do the same."
    );

    Ok(true)
}

fn format_ops(value: f64) -> String {
    let rounded = value.round() as i64;
    let text = rounded.to_string();
    let mut out = String::new();
    for (i, c) in text.chars().rev().enumerate() {
        if i != 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// Builds the WebAssembly VM crate for `wasm32-unknown-unknown` at the
/// `min-size` profile and returns the built module's size.
fn build_wasm_size(root: &Path) -> Result<u64, String> {
    let status = Command::new("cargo")
        .args([
            "build",
            "--profile",
            "min-size",
            "--target",
            "wasm32-unknown-unknown",
            "-p",
            "hollowscript-wasm",
        ])
        .current_dir(root)
        .status()
        .map_err(|e| format!("could not run cargo build: {e}"))?;
    if !status.success() {
        return Err(
            "cargo build for wasm32-unknown-unknown failed; is the target installed \
             (`rustup target add wasm32-unknown-unknown`)?"
                .to_owned(),
        );
    }
    let path = target_dir(root)
        .join("wasm32-unknown-unknown")
        .join("min-size")
        .join("hollowscript_wasm.wasm");
    file_size(&path)
}
