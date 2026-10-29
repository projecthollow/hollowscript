//! A deterministic, seeded fuzz test over the *whole* pipeline: lex, parse,
//! check, compile, verify, load and run.
//!
//! `hollowscript-syntax` already fuzzes its own two stages. This one is the
//! third target `docs/design/testing.md` names, "the checker and the
//! compiler ... to find inputs that pass parsing but crash or hang checking
//! or compilation", and it goes one stage further and runs what it loads,
//! because section 19.1's promise is about the whole implementation: it
//! "never crashes, hangs or exhausts its host on any input".
//!
//! Two properties are checked of every mutated program.
//!
//! 1. **Nothing panics, at any stage.** A fault, a diagnostic or a host
//!    error is a fine answer; unwinding is not.
//! 2. **A program the checker accepts compiles, verifies and loads.** The
//!    checker is what decides whether a program is well formed, so a
//!    program it reports no error for must reach an instance. A
//!    `CheckFailed` after a clean check means the compiler refused
//!    something it accepted, or emitted bytecode the verifier refused: the
//!    `LoadInt r, -1` that the verifier read as a jump to itself was
//!    exactly that shape.
//!
//! Seeded, not free-running, like every other fuzz test here: the whole run
//! comes from one number, so a failure is reproducible in an ordinary test
//! run without the fuzzer.
//!
//! The mutation engine is a copy of the one in
//! `hollowscript-syntax/tests/support`, deliberately: a `tests/` module
//! cannot be shared between crates without a dev-dependency crate, and the
//! crate graph is fixed on purpose (`cargo xtask graph`). Only the parts
//! this test uses are copied, and it is the smaller price.
//!
//! `clippy.toml` allows `unwrap`/`expect`/`panic`/indexing in test code, but
//! that covers a `#[cfg(test)] mod` inside a crate and not a `tests/*.rs`
//! integration test, so the same policy is spelled out here.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use hollowscript::{
    CallOutcome, Control, Diagnostic, Instance, Limits, LoadOutcome, NoLoader, Options,
};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// How many mutated programs go through the whole pipeline. Enough to reach
/// a wide spread of shapes in a second or two, which is what keeps it in
/// `scripts/check` rather than in a nightly job nobody runs.
const ITERATIONS: usize = 6_000;

/// A fixed seed: the whole run is reproducible from this one number.
const SEED: u64 = 0x11A5_7E57_F022_u64;

/// Limits tight enough that a mutated program cannot run long, and loose
/// enough that an ordinary corpus program still finishes. A mutation that
/// builds an endless loop must reach HS1001, not the test's own timeout.
fn limits() -> Limits {
    Limits {
        max_steps: 200_000,
        max_memory_bytes: 4_000_000,
        max_output_bytes: 100_000,
        max_call_depth: 64,
    }
}

#[test]
fn the_whole_pipeline_never_panics_and_accepts_what_the_checker_accepts() {
    let root = corpus_root();
    if !root.exists() {
        eprintln!("skipping: corpus not found at {}", root.display());
        return;
    }
    let sources: Vec<String> = walk_hws(&root)
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .collect();
    assert!(!sources.is_empty(), "expected corpus sources to fuzz from");

    let mut rng = Lcg::new(SEED);
    let mut failures: Vec<String> = Vec::new();
    let mut loaded = 0usize;

    for i in 0..ITERATIONS {
        let base = &sources[rng.below(sources.len())];
        let mutated = mutate(base, &mut rng);
        let attempt = mutated.clone();
        let outcome = std::panic::catch_unwind(move || drive(&attempt, false));
        match outcome {
            Err(_) => failures.push(format!(
                "iteration {i} panicked on:\n{}\n",
                preview(&mutated)
            )),
            Ok(Outcome::Panicked(reason)) => {
                failures.push(format!(
                    "iteration {i}: {reason} on:\n{}\n",
                    preview(&mutated)
                ));
            }
            Ok(Outcome::Loaded(_)) => loaded += 1,
            Ok(Outcome::Refused) => {}
        }
        if failures.len() >= 5 {
            break;
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // A run where nothing ever loaded would pass the two properties without
    // having exercised the stages past the checker at all.
    assert!(
        loaded > ITERATIONS / 100,
        "only {loaded} of {ITERATIONS} mutated programs reached an instance, \
         which is too few to have fuzzed anything past the checker"
    );
}

/// Every mutated program that reaches an instance, driven twice: once with
/// the collector's ordinary threshold policy, once with
/// `Options::gc_stress_mode`, which forces a collection at every safepoint
/// (`docs/design/performance.md`, "stress mode"). The mid-run collector's
/// whole safety argument is that collecting never changes what a run
/// observes, so the two drives of the same program must produce the same
/// signature (every call outcome and every read, rendered to text); this is
/// the corpus-wide half of that proof, the other half being
/// `hollowscript-vm`'s own unit tests and the corpus and specification runs
/// `scripts/check-gc-stress` makes under `HOLLOWSCRIPT_GC_STRESS=1`. Fewer
/// iterations than the property test above: every program here runs twice,
/// once more than that one, and small mutated programs are what stresses a
/// mid-run collector least, so the wider single-drive run above still does
/// most of the searching.
#[test]
fn a_forced_collection_at_every_safepoint_never_changes_the_observable_result() {
    let root = corpus_root();
    if !root.exists() {
        eprintln!("skipping: corpus not found at {}", root.display());
        return;
    }
    let sources: Vec<String> = walk_hws(&root)
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .collect();
    assert!(!sources.is_empty(), "expected corpus sources to fuzz from");

    let mut rng = Lcg::new(SEED ^ 0xA11C_0115);
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0usize;

    for i in 0..(ITERATIONS / 2) {
        let base = &sources[rng.below(sources.len())];
        let mutated = mutate(base, &mut rng);
        let normal = drive(&mutated, false);
        let stress = drive(&mutated, true);
        match (normal, stress) {
            (Outcome::Panicked(reason), _) | (_, Outcome::Panicked(reason)) => {
                failures.push(format!(
                    "iteration {i}: {reason} on:\n{}\n",
                    preview(&mutated)
                ));
            }
            (Outcome::Refused, Outcome::Refused) => {}
            (Outcome::Loaded(a), Outcome::Loaded(b)) => {
                compared += 1;
                if a != b {
                    failures.push(format!(
                        "iteration {i}: forcing a collection at every safepoint changed the \
                         observable result.\nnormal:\n{a}\nstress:\n{b}\non:\n{}\n",
                        preview(&mutated)
                    ));
                }
            }
            (a, b) => failures.push(format!(
                "iteration {i}: the normal and stress drives disagreed on whether the \
                 program even reached an instance ({} vs {}) on:\n{}\n",
                a.kind(),
                b.kind(),
                preview(&mutated)
            )),
        }
        if failures.len() >= 5 {
            break;
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        compared > ITERATIONS / 200,
        "only {compared} of {} mutated programs reached an instance under both drives, \
         which is too few to have compared anything past the checker",
        ITERATIONS / 2
    );
}

enum Outcome {
    /// The program reached an instance and was driven; here is its
    /// signature (every call outcome and read, rendered to text).
    Loaded(String),
    /// The checker or the parser reported an error, which is a fine answer.
    Refused,
    /// A property was broken without unwinding.
    Panicked(String),
}

impl Outcome {
    /// A short name for a mismatch message; never a program's own result,
    /// which `Outcome::Loaded` already carries in full for that.
    fn kind(&self) -> &'static str {
        match self {
            Outcome::Loaded(_) => "loaded",
            Outcome::Refused => "refused",
            Outcome::Panicked(_) => "panicked",
        }
    }
}

/// Runs one program through every stage, and then through an instance's own
/// surface: call every export that takes no arguments, read every exported
/// constant, and dispose. `stress` sets `Options::gc_stress_mode`.
fn drive(source: &str, stress: bool) -> Outcome {
    let options = Options {
        limits: limits(),
        gc_stress_mode: stress,
        ..Options::default()
    };
    let diagnostics = hollowscript::check(
        source,
        "fuzz.hws",
        &options.capabilities,
        options.filter_schema.as_ref(),
        &mut NoLoader,
        &mut Control::none(),
    );
    let clean = !diagnostics.iter().any(Diagnostic::is_error);
    let outcome = hollowscript::load(
        source,
        "fuzz.hws",
        &options,
        &mut NoLoader,
        &mut |_, _| None,
        &mut Control::none(),
    );
    match outcome {
        LoadOutcome::CheckFailed(found) => {
            if clean {
                let codes: Vec<String> = found.iter().map(|d| d.code.to_string()).collect();
                return Outcome::Panicked(format!(
                    "the checker accepted this program and loading refused it: {}",
                    codes.join(", ")
                ));
            }
            Outcome::Refused
        }
        LoadOutcome::Instance(mut instance) => {
            let signature = exercise(&mut instance);
            let _ = instance.dispose();
            Outcome::Loaded(signature)
        }
    }
}

/// Everything a host may do with an instance, so that the state machine of
/// chapter 12 is fuzzed too, not only the pipeline that built it. Answers a
/// signature of everything observed, one line per export: the call outcome
/// (rendered, for `Returned`) and the read result, both stable text so two
/// drives of the same program can be compared for equality.
fn exercise(instance: &mut Instance) -> String {
    let mut signature = String::new();
    for export in instance.exports() {
        let name = export.name;
        if export.arity == Some(0) {
            let called = match instance.call(&name, &[], &mut Control::none()) {
                Ok(CallOutcome::Returned(Some(value))) => {
                    format!("returned {}", instance.render(value))
                }
                Ok(CallOutcome::Returned(None)) => "returned none".to_owned(),
                Ok(CallOutcome::Faulted(fault)) => format!("faulted {:?}", fault.code),
                Ok(CallOutcome::Suspended) => "suspended".to_owned(),
                Err(e) => format!("host error {:?}", e.code),
            };
            let _ = writeln!(signature, "{name}(): {called}");
        }
        let read = match instance.read(&name) {
            Ok(value) => instance.render(value),
            Err(e) => format!("host error {:?}", e.code),
        };
        let _ = writeln!(signature, "{name} = {read}");
    }
    signature
}

/// The first few lines of a program, for a failure message that stays
/// readable when the input is a few kilobytes of mutated source.
fn preview(text: &str) -> String {
    let mut out: String = text.chars().take(400).collect();
    if text.len() > out.len() {
        out.push_str("\n...");
    }
    out
}

// ---- corpus discovery ---------------------------------------------------

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

fn walk_hws(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_into(dir, &mut out);
    out.sort();
    out
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(&path, out);
        } else if path.extension().is_some_and(|e| e == "hws") {
            out.push(path);
        }
    }
}

// ---- the mutation engine ------------------------------------------------

/// A linear congruential generator: small, deterministic, and no dependency.
struct Lcg(u64);

impl Lcg {
    const fn new(seed: u64) -> Lcg {
        Lcg(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// A value in `0..n`, or 0 if `n` is 0. Reads the high bits, which an
    /// LCG distributes far better than the low ones.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let r = self.next_u64() >> 32;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the modulus is `n`, which is a usize"
        )]
        {
            (r % n as u64) as usize
        }
    }
}

/// Characters that are interesting to a lexer: delimiters, operators, the
/// ones with their own rules, and a few beyond ASCII.
const INTERESTING_CHARS: &[char] = &[
    '{', '}', '(', ')', '[', ']', '"', '`', '$', '\\', ':', ',', '.', '?', '-', '+', '*', '/', '=',
    '<', '>', '!', '_', '\n', '\t', '0', '9', 'a', 'Z', '\u{feff}', 'é', '\u{202e}',
];

/// Fragments that steer a mutation towards a program that still checks,
/// which is what exercises the stages past the parser.
const INTERESTING_SNIPPETS: &[&str] = &[
    "return 0\n",
    "constant x = 1\n",
    "variable y = 0\n",
    "fn f(): Int {\n    return 1\n}\n",
    "export fn g(): Int {\n    return 2\n}\n",
    "export constant k = 3\n",
    "if true {\n    return 1\n}\n",
    "for i in range(0, 3) {\n}\n",
    "while true {\n}\n",
    "match 1 {\n    else { return 0 }\n}\n",
    "[1, 2, 3].map(fn(n: Int): Int {\n    return n\n})\n",
    "?",
    "effect ",
];

/// One mutation of `base`: a character edit, a spliced snippet, a truncation
/// or a whole-token edit.
fn mutate(base: &str, rng: &mut Lcg) -> String {
    let mut chars: Vec<char> = base.chars().collect();
    let edits = 1 + rng.below(3);
    for _ in 0..edits {
        match rng.below(4) {
            0 => mutate_char_level(&mut chars, rng),
            1 => splice_snippet(&mut chars, rng),
            2 => truncate(&mut chars, rng),
            _ => {
                let text: String = chars.iter().collect();
                chars = duplicate_line(&text, rng).chars().collect();
            }
        }
    }
    chars.into_iter().collect()
}

fn mutate_char_level(chars: &mut Vec<char>, rng: &mut Lcg) {
    if chars.is_empty() {
        chars.push(INTERESTING_CHARS[rng.below(INTERESTING_CHARS.len())]);
        return;
    }
    match rng.below(5) {
        0 => {
            let i = rng.below(chars.len());
            chars.remove(i);
        }
        1 => {
            let i = rng.below(chars.len() + 1);
            chars.insert(i, INTERESTING_CHARS[rng.below(INTERESTING_CHARS.len())]);
        }
        2 => {
            let i = rng.below(chars.len());
            chars[i] = INTERESTING_CHARS[rng.below(INTERESTING_CHARS.len())];
        }
        3 => {
            let i = rng.below(chars.len());
            let c = chars[i];
            chars.insert(i, c);
        }
        _ => {
            if chars.len() >= 2 {
                let i = rng.below(chars.len() - 1);
                chars.swap(i, i + 1);
            }
        }
    }
}

fn splice_snippet(chars: &mut Vec<char>, rng: &mut Lcg) {
    let snippet = INTERESTING_SNIPPETS[rng.below(INTERESTING_SNIPPETS.len())];
    let i = rng.below(chars.len() + 1);
    for (offset, c) in snippet.chars().enumerate() {
        chars.insert(i + offset, c);
    }
}

fn truncate(chars: &mut Vec<char>, rng: &mut Lcg) {
    if chars.is_empty() {
        return;
    }
    let keep = rng.below(chars.len());
    chars.truncate(keep);
}

/// Repeats one whole line, which is how a mutation reaches a duplicate
/// declaration, a second `return` or a deeper nesting without breaking the
/// lines around it.
fn duplicate_line(text: &str, rng: &mut Lcg) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return text.to_owned();
    }
    let i = rng.below(lines.len());
    let mut out = Vec::with_capacity(lines.len() + 1);
    out.extend_from_slice(&lines[..=i]);
    out.push(lines[i]);
    out.extend_from_slice(&lines[i + 1..]);
    out.join("\n")
}
