//! Runs the shared conformance corpus (`corpus/FORMAT.md`) through the
//! embedding facade and compares every stage of every case against the
//! case's own expected file. A development tool; not published, and not part
//! of the public API of any crate.
//!
//! ```text
//! hollowscript-conformance [CORPUS_DIR] [--baseline FILE] [--show N] [--list-failures]
//! hollowscript-conformance --spec [SPEC_DIR] [--baseline FILE] [--show N] [--list-failures]
//! ```
//!
//! `CORPUS_DIR` defaults to `corpus` relative to the current directory, and
//! the baseline to `crates/hollowscript-conformance/known-failures.txt`.
//! With `--spec` it runs the examples of the specification instead
//! ([`spec`], section 19.6): `SPEC_DIR` defaults to `docs/spec` and the
//! baseline to `crates/hollowscript-conformance/spec-known-failures.txt`.
//! The exit code is the gate: see [`report`].

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "this tool's entire job is printing a summary to standard output and standard error"
)]

mod case;
mod compare;
mod discover;
mod expected;
mod host;
mod host_file;
mod hostconfig;
mod json;
mod report;
mod spec;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

/// Whether `HOLLOWSCRIPT_GC_STRESS` is set in the environment: the
/// stress-collection mode `scripts/check-gc-stress` runs this same corpus
/// and specification-examples pass under, with every case's
/// [`hollowscript::Options::gc_stress_mode`] forcing a collection at every
/// safepoint. A collector bug that only shows up mid-run, rather than
/// between calls, should then turn into an ordinary case failure here: the
/// collector never changes an observable result (`docs/design/security.md`),
/// so a result that differs between an ordinary run and a stress run of the
/// same case is that bug, not a new kind of expected difference the corpus
/// format has to represent.
pub(crate) fn gc_stress_enabled() -> bool {
    std::env::var_os("HOLLOWSCRIPT_GC_STRESS").is_some()
}

/// The command line, already parsed.
struct Args {
    /// Whether to run the specification's examples rather than the corpus.
    spec: bool,
    corpus: PathBuf,
    baseline: PathBuf,
    show: usize,
    list_failures: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut corpus = None;
    let mut baseline = None;
    let mut show = 3;
    let mut list_failures = false;
    let mut spec = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--baseline" => {
                baseline = Some(PathBuf::from(args.next().ok_or("--baseline needs a file")?));
            }
            "--show" => {
                show = args
                    .next()
                    .ok_or("--show needs a number")?
                    .parse()
                    .map_err(|_| "--show needs a number")?;
            }
            "--list-failures" => list_failures = true,
            "--spec" => spec = true,
            other if other.starts_with("--") => return Err(format!("unknown flag '{other}'")),
            other => corpus = Some(PathBuf::from(other)),
        }
    }
    let (default_dir, default_baseline) = if spec {
        (
            "docs/spec",
            "crates/hollowscript-conformance/spec-known-failures.txt",
        )
    } else {
        (
            "corpus",
            "crates/hollowscript-conformance/known-failures.txt",
        )
    };
    Ok(Args {
        spec,
        corpus: corpus.unwrap_or_else(|| PathBuf::from(default_dir)),
        baseline: baseline.unwrap_or_else(|| PathBuf::from(default_baseline)),
        show,
        list_failures,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("hollowscript-conformance: {e}");
            return ExitCode::from(2);
        }
    };
    let baseline: BTreeSet<String> = match std::fs::read_to_string(&args.baseline) {
        Ok(text) => report::read_baseline(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(e) => {
            eprintln!(
                "hollowscript-conformance: cannot read {}: {e}",
                args.baseline.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let (result, count, noun) = if args.spec {
        match spec::discover(&args.corpus) {
            Ok(examples) if !examples.is_empty() => {
                let count = examples.len();
                (spec::run(&examples, baseline), count, "examples")
            }
            Ok(_) => {
                eprintln!(
                    "hollowscript-conformance: no examples found under {}",
                    args.corpus.display()
                );
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!(
                    "hollowscript-conformance: cannot read {}: {e}",
                    args.corpus.display()
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        let cases = match discover::discover_cases(&args.corpus) {
            Ok(cases) => cases,
            Err(e) => {
                eprintln!(
                    "hollowscript-conformance: cannot read {}: {e}",
                    args.corpus.display()
                );
                return ExitCode::FAILURE;
            }
        };
        if cases.is_empty() {
            eprintln!(
                "hollowscript-conformance: no cases found under {}",
                args.corpus.display()
            );
            return ExitCode::FAILURE;
        }
        let count = cases.len();
        (report::run(&cases, baseline), count, "cases")
    };
    let passed = result.passed().len();
    println!(
        "{count} {noun} under {}: {passed} pass, {} fail, {} unreadable",
        args.corpus.display(),
        result.failed().len(),
        result.broken().len(),
    );
    println!(
        "  baseline: {} listed as known failures",
        result.baseline.len()
    );

    if args.list_failures {
        for id in result.failed() {
            println!("{id}");
        }
        for id in result.broken() {
            println!("{id}");
        }
    }

    let regressions = result.regressions();
    let newly_passing = result.newly_passing();
    if regressions.is_empty() && newly_passing.is_empty() {
        println!("\nconformance: ok (every difference is in the baseline)");
        return ExitCode::SUCCESS;
    }
    if !regressions.is_empty() {
        println!(
            "\n{} case(s) fail that the baseline does not list:",
            regressions.len()
        );
        for id in regressions.iter().take(args.show.max(1)) {
            println!("  {id}");
            for problem in result.problems_of(id) {
                println!("      {problem}");
            }
        }
        if regressions.len() > args.show {
            println!("  ... and {} more", regressions.len() - args.show);
        }
    }
    if !newly_passing.is_empty() {
        println!(
            "\n{} case(s) now pass but are still in the baseline; remove them:",
            newly_passing.len()
        );
        for id in &newly_passing {
            println!("  {id}");
        }
    }
    ExitCode::FAILURE
}
