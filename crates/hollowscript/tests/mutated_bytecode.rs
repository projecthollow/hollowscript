//! A deterministic, seeded mutation test over bytecode containers, the way a
//! hostile host would present them: valid containers built from the corpus,
//! one to three of their bytes replaced with the values that break an
//! implementation (the largest integers, `0xFF`, a single flipped bit), the
//! checksum made valid again so the mutation reaches the decoder and the
//! verifier instead of stopping at the digest, and then loaded.
//!
//! The property: for every mutant, [`verify_bytes`] either refuses it or the
//! module it accepts runs to the end of its limits (or to a fault) without
//! panicking, without hanging, and without asking the host for memory out of
//! proportion to the container. A string id of `u32::MAX` in a constant once
//! made the interpreter resize a cache to four billion entries, which no
//! limit of the language could prevent because no instruction had run yet;
//! the verifier accepted it because it never looked inside a constant.
//!
//! Like the other fuzz tests here this is seeded, not free-running, so a
//! failure reproduces from the printed iteration in an ordinary test run. The
//! coverage-guided counterpart is `fuzz/fuzz_targets/bytecode_verify.rs`.
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

use hollowscript::{BuildOutcome, Control, Limits, NoLoader, Options};
use hollowscript_bytecode::container::HEADER_LEN;
use hollowscript_bytecode::module::ExportKind;
use hollowscript_bytecode::{fix_checksum, verify::verify_bytes};
use hollowscript_vm::{CapabilityTable, Interpreter};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// How many mutants are loaded. Enough for every table of a small container
/// to be hit many times over; it runs in about ten seconds.
const ITERATIONS: usize = 12_000;

/// A fixed seed: the whole run is reproducible from this one number.
const SEED: u64 = 0x00B1_7EC0_DE5E_ED01;

/// How long one mutant may take, run and all. Every limit below is tight, so
/// a mutant that outlasts this is a hang the limits did not stop.
const DEADLINE: Duration = Duration::from_secs(60);

fn limits() -> Limits {
    Limits {
        max_steps: 200_000,
        max_memory_bytes: 4_000_000,
        max_output_bytes: 100_000,
        max_call_depth: 64,
    }
}

/// The values that break an implementation: the largest ones, and the ones
/// either side of a signed or an unsigned boundary.
const HOSTILE_U32: [u32; 8] = [
    u32::MAX,
    u32::MAX - 1,
    0x7fff_ffff,
    0x8000_0000,
    0x00ff_ffff,
    0x1000_0000,
    0x0001_0000,
    0x0000_ffff,
];

fn mutate(bytes: &mut [u8], rng: &mut Lcg) {
    let payload = bytes.len() - HEADER_LEN;
    for _ in 0..=rng.below(3) {
        let at = HEADER_LEN + rng.below(payload);
        match rng.below(6) {
            0 | 1 => {
                let value = HOSTILE_U32[rng.below(HOSTILE_U32.len())].to_le_bytes();
                for (offset, byte) in value.into_iter().enumerate() {
                    if let Some(slot) = bytes.get_mut(at + offset) {
                        *slot = byte;
                    }
                }
            }
            2 => bytes[at] = 0xff,
            3 => bytes[at] = u8::try_from(rng.below(256)).unwrap_or(0),
            4 => bytes[at] ^= 1 << rng.below(8),
            _ => {
                for offset in 0..8 {
                    if let Some(slot) = bytes.get_mut(at + offset) {
                        *slot = 0xff;
                    }
                }
            }
        }
    }
}

/// Loads `mutant` and, if the verifier accepts it, runs its initialisers and
/// every export that takes no arguments. Returns whether it was accepted.
fn load_and_run(mutant: &[u8]) -> bool {
    let Ok(verified) = verify_bytes(mutant) else {
        return false;
    };
    let functions: Vec<u32> = verified
        .module()
        .exports
        .iter()
        .filter_map(|export| match &export.kind {
            ExportKind::Function {
                function,
                signature,
            } if signature.params.is_empty() => Some(*function),
            _ => None,
        })
        .collect();
    let mut interpreter = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
    if interpreter
        .initialise(&limits(), &mut Control::none())
        .is_err()
    {
        return true;
    }
    for function in functions {
        let _ = interpreter.call_export(function, &[], &limits(), &mut Control::none());
    }
    true
}

#[test]
fn a_mutated_container_is_refused_or_runs_within_its_limits() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    if !root.exists() {
        eprintln!("skipping: corpus not found at {}", root.display());
        return;
    }
    let mut containers: Vec<Vec<u8>> = Vec::new();
    for path in walk_hws(&root) {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let BuildOutcome::Bytecode(bytes) = hollowscript::build(
            &source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut Control::none(),
        ) {
            containers.push(bytes);
        }
    }
    assert!(
        containers.len() > 20,
        "expected corpus programs that build, found {}",
        containers.len()
    );
    let mut rng = Lcg(SEED);
    let mut accepted = 0usize;
    for iteration in 0..ITERATIONS {
        let mut mutant = containers[rng.below(containers.len())].clone();
        mutate(&mut mutant, &mut rng);
        fix_checksum(&mut mutant);
        let copy = mutant.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(load_and_run(&copy));
        });
        match receiver.recv_timeout(DEADLINE) {
            Ok(was_accepted) => accepted += usize::from(was_accepted),
            Err(error) => panic!(
                "iteration {iteration} (seed {SEED:#x}) panicked or hung ({error:?}) on a \
                 mutant of {} bytes",
                mutant.len()
            ),
        }
    }
    // A run in which nothing was ever accepted would hold the property
    // vacuously, so it must have loaded a real share of the mutants.
    assert!(
        accepted > ITERATIONS / 20,
        "only {accepted} of {ITERATIONS} mutants were accepted by the verifier"
    );
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

/// A linear congruential generator: small, deterministic, and no dependency.
struct Lcg(u64);

impl Lcg {
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
