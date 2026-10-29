# Continuous fuzzing

`fuzz/` is a separate `cargo-fuzz` crate: outside this workspace (the root
`Cargo.toml`'s `exclude`), never shipped, and not covered by this
workspace's own lint or dependency policy (`docs/design/dependencies.md`'s
"today: none" is about the crates that ship; libFuzzer needs nightly and
its own dependency, which this crate alone carries). Owner decision,
2026-09-23. This complements, rather than replaces, the deterministic,
seeded fuzz tests `scripts/check` already runs on every change
(`hollowscript-syntax/tests/fuzz.rs`, `format_fuzz.rs`, and
`hollowscript/tests/fuzz_pipeline.rs`); see
[testing.md](testing.md#deterministic-seeded-fuzzing) for how the two
relate. Those run in seconds, from one fixed seed; the targets here run for
minutes to hours and explore input libFuzzer's own coverage feedback finds,
not a fixed mutation script.

`scripts/check` builds this crate (`cargo build --manifest-path
fuzz/Cargo.toml --bins`, on stable, uninstrumented) as one of its own steps,
and separately builds `hollowscript-wasm`'s `tooling` feature (native, and
under `wasm32-unknown-unknown` when that target is installed): neither is
part of the default workspace build `cargo build --workspace --all-targets`
does, so both had silently fallen out of sync with the crates they cover
(one fuzz target calling `hollowscript::check` with a parameter list the
function no longer had; `hollowscript-wasm`'s own `tooling::check` the same
way) before anyone building the workspace the ordinary way noticed. This
does not run any target, only compiles every one of them, which is enough
to catch that class of drift without needing nightly or `cargo-fuzz` in the
gate itself.

## Running it

```
rustup toolchain install nightly
cargo install cargo-fuzz

scripts/fuzz list                     # the nine targets
scripts/fuzz build                    # build every target, instrumented
scripts/fuzz run <target> [seconds]   # run one target (default 60s)
scripts/fuzz run-all [seconds]        # run every target, one after another
scripts/fuzz seeds                    # regenerate the bytecode seed corpus
```

`scripts/fuzz run` and `run-all` are a thin wrapper over
`cargo +nightly fuzz run <target> -- -max_total_time=<seconds> corpus/<target> seeds/<target>`:
each target's own `corpus/<target>/` (gitignored, local, where libFuzzer
grows what it finds while running) and `seeds/<target>/` (committed, read
only, passed as an extra directory) are kept apart on purpose, so the
repository never accumulates thousands of fuzzer-mutated files as churn.
The order matters: libFuzzer writes every new input into the first
directory on its command line, and cargo-fuzz only supplies
`corpus/<target>` itself when no directory is given, appending it after
any that are. Passing `seeds/<target>` alone therefore made it the write
target, and a single 30 second run added some two thousand files to it.
Calling `cargo fuzz run` directly, without the `seeds/<target>` argument,
still works; it just starts from whatever `corpus/<target>/` already holds
locally (empty, the first time) rather than from the committed seeds.

### Seed corpora

- `lex_parse`, `format_roundtrip`, `check` and `pipeline` (the four
  source-based targets) are seeded from every `.hws` file in `corpus/`
  (255 cases at the time of writing) and every `hollow`, `hollow-error`,
  `hollow-fault`, `hollow-module` and `hollow-module-error` code block
  extracted from `docs/spec/*.md` (104 examples), one copy per target, in
  each target's own `fuzz/seeds/<target>/`.
- `bytecode_verify` and `wasm_abi` (the two targets that take raw bytecode
  bytes directly) are seeded, in the same layout, from `hollowscript::build`'s
  output for every corpus program that compiles cleanly, written by
  `fuzz/tools/gen_seeds.rs` (`scripts/fuzz seeds`, or `cargo run --bin
  gen_seeds` from `fuzz/`). This tool is part of the fuzz crate rather than
  a one-off script because the seed corpus is worth regenerating whenever
  the bytecode format or the corpus itself changes meaningfully, not only
  once.
- `capi` and `capi_ex` take HollowScript *source* text, not bytecode (they
  call `hs_check_json`/`hs_load`/`hs_load_ex` themselves, splitting their
  input into a source string and an arguments or call-arguments string; see
  `fuzz_targets/capi.rs`'s own `split`), so `gen_seeds.rs` writes them the
  corpus's own `.hws` text, copied as is, rather than bytecode: a bytecode
  seed for these two targets would never parse as the source they actually
  read, so it seeded nothing they exercise.
- `filter` takes *filter source*, not a HollowScript program, so it is
  seeded separately, by hand, in `fuzz/seeds/filter/`: one file per row of
  Appendix A section A.11's conformance vectors (both the valid filters and
  the ones that error), covering every diagnostic code and every value
  shape (dates, ranges, relative offsets, numbers, `none`/`any`, quoted and
  unquoted terms and predicates, nesting) the grammar defines.

## The targets, and what each one asserts

| Target | Asserts |
|---|---|
| `lex_parse` | The lexer and parser never panic on any byte string, matching section 19.1's "never crashes, hangs or exhausts its host on any input... including arbitrary bytes" for the front end specifically. |
| `format_roundtrip` | The formatter never panics on any byte string; and for a source that parses cleanly, formatting is idempotent (`format(format(x)) == format(x)`) and its own output parses cleanly again, or is refused with an error diagnostic naming why (a comment the canonical layout has no place for). |
| `check` | The checker never panics on any byte string, for a single-file program (an import always fails to resolve, which is a fine, checked answer). |
| `pipeline` | The whole pipeline (check, compile, verify, load, initialise, then call every zero-argument export and read every exported constant) never panics; and a program the checker accepts must reach an instance, never a `CheckFailed` after a clean check, which would mean the compiler or the verifier refused something the checker already accepted. Limits are kept small (200,000 steps, 4 MB, 64 call depth) so every input terminates quickly rather than reaching libFuzzer's own timeout. |
| `bytecode_verify` | The strongest property here: a module the verifier accepts must never make the VM panic or read out of bounds while running it. Fixes up the container checksum first (`hollowscript_bytecode::fix_checksum`, since a mutation has no way to land on the right SHA-256 by chance and almost every input would otherwise be refused before reaching anything interesting), decodes and verifies the result as a container directly, then, for every zero-argument function export the verifier accepted, initialises the module and calls it, under the same small limits as `pipeline`. The last five bytes of an input may, by their first byte's top bit, instead overwrite four bytes of the rest (from a position they give) with one of eight hostile integers (`u32::MAX` and its neighbours), because byte-level mutation reaches a string id of `u32::MAX` only by chance; a seeded, deterministic counterpart that runs in `scripts/check` is `hollowscript/tests/mutated_bytecode.rs`. |
| `capi` | The C ABI (`hollowscript-capi`), called the way a host that is not Rust, and cannot be trusted to pass well-formed input, would call it: arbitrary bytes as the source and as a JSON arguments text (each turned into a `CString` with embedded NUL bytes stripped, since a real C string cannot contain one), through `hs_check_json`, `hs_format_json`, `hs_load`, `hs_call`, `hs_read` and a double `hs_dispose` in sequence. Asserts nothing panics and that a handle may always be disposed, more than once, without the process crashing. |
| `capi_ex` | The extended C ABI `capi` never reaches: `hs_load_ex` and `hs_call_ex` with a bound capability (`cap`, an `Int`-returning function whose callback answers from a small static menu keyed by a byte of the program's own call arguments, covering both `Ok` and the refusal path) and a cancellation token, created fresh for the load and again for the call, and sometimes cancelled before the call it governs even starts. Same property as `capi`, plus that a token may be freed once nothing can still read it without crashing. |
| `wasm_abi` | The WebAssembly ABI (`hollowscript-wasm::abi`), called the same way: arbitrary bytes as the bytecode buffer to `hs_wasm_load`, arbitrary bytes as the call arguments to `hs_wasm_call` and `hs_wasm_read`, and a double `hs_wasm_dispose`. Runs the `extern "C"` functions directly on the host rather than inside an actual `wasm32` instance, which is fine: the functions themselves are the whole boundary, and nothing about their behaviour depends on the target triple they are compiled for. |
| `filter` | Appendix A's `parse`, `validate`, `matches` and `format` (`hollowscript-core::filter`) never panic on any byte string, and for a filter that parses, `format`'s own promise (section A.9) holds: its text parses back to an identical tree, and formatting that reparsed tree again produces the same text (idempotence, the same property `format_roundtrip` checks for a whole program). `validate` and `matches` are run against a fixed schema and adapter covering every field type, so a panic specific to schema checking or evaluation is reachable too. |

## What each run found, and its time

Run on this machine (macOS, aarch64, 15 logical cores), from the seed
corpora above, all seven running in parallel, on 2026-09-23, followed by
two further solo runs of `format_roundtrip` once its first run's crash was
fixed. Each of the first seven ran at least 20 minutes; `pipeline` and
`bytecode_verify`, the ones this document asks for longer on, ran 30.

| Target | Time | Runs | Final coverage | Result |
|---|---:|---:|---:|---|
| `lex_parse` | 20 min | 4,248,376 | 4,503 edges, 26,391 features | No crash. |
| `format_roundtrip` | 20 min, then a second 20 min run after the fix below, then a third, isolated 10 min run | first: 1,826,296 before its crash; second: 1,768,569 before an OOM abort; third (alone, no sibling targets): 799,245 before another OOM abort | n/a (each run ended by a finding rather than its own time limit) | **Found one crash** in the first run (below, fixed). Every run after that hit an out-of-memory abort well before its time limit; investigated below and found to be an AddressSanitizer instrumentation artifact, not a defect in the formatter. |
| `check` | 20 min | 2,792,706 | 14,666 edges, 72,209 features | No crash. |
| `pipeline` | 30 min | 2,551,340 | 19,620 edges, 78,726 features, the highest of any target here, which is expected: it is the only one that reaches the VM through every stage in front of it | No crash. |
| `bytecode_verify` | 30 min | 348,507,130 | 2,745 edges, 6,490 features | No crash. Most random byte strings fail the container's checksum or version check long before reaching the verifier's own rules, which is why this target's coverage is lower than the others despite running longest and executing far more inputs per second, and also exactly why it is seeded from real containers rather than left to find the file format from nothing. |
| `capi` | 20 min | 2,103,052 | 7,892 edges, 27,623 features | No crash. |
| `wasm_abi` | 20 min | 253,404,510 | 244 edges, 510 features | No crash. Coverage settled low: most of the arbitrary bytes given to `hs_wasm_load` fail `verify_bytes` immediately, the same way `bytecode_verify`'s do, before any call, read or dispose path is reached. |

`capi_ex` (the extended C ABI target) and `bytecode_verify`'s checksum
fix-up both post-date this run and are not reflected in the table above;
`capi`'s row also predates its seed corpus being corrected from bytecode to
source text (see "Seed corpora"). None of the three has had its own timed
run yet: recorded here as a gap, not run with invented numbers.

### Investigated: repeated out-of-memory aborts, found to be AddressSanitizer, not a leak

Every `format_roundtrip` run after the crash fix hit libFuzzer's
"out-of-memory" abort (against `-rss_limit_mb=4096`) well before its time
limit, including a third run of the target alone, with no sibling fuzzer
sharing the machine, which ruled out the first, cheaper explanation (seven
ASAN processes competing for real memory at once). Each run's own reported
`rss:` climbed steadily and near-linearly with the run's iteration count,
from tens of megabytes at the start to the 4096 MB limit; the corpus size
alongside it stayed roughly flat, so the growth was not "a larger and
larger corpus held in memory" either.

Two more direct checks settled it:

1. Replaying the exact saved input alone, once, against the built binary
   (`format_roundtrip -runs=1 <saved-input>`, `/usr/bin/time -l`): 42 MB
   peak, 50 ms. `cargo fuzz tmin` on the same input independently reported
   "the input did not crash" within 255 runs given only that input and no
   accumulated process state. Whatever was growing was not caused by this
   one input.
2. `fuzz/tools/leak_check.rs` (a diagnostic tool, kept alongside
   `gen_seeds.rs`, not one of the fuzz crate's own libFuzzer targets):
   `hollowscript-syntax::format`, called over a million times on varied, deterministically
   mutated input, in an ordinary **non-instrumented** release binary, no
   ASAN, no libFuzzer. `/usr/bin/time -l` after 1,046,296 calls (the run was
   stopped once the pattern was clear rather than run to its full
   2,000,000): 3.6 MB peak resident set size, no growth over the run's
   whole length. The same workload that drove the ASAN binary from tens of
   megabytes to 4 GB left a plain release binary flat at a few megabytes.

That is decisive: nothing in `hollowscript-syntax::format` (or the parser
underneath it) retains memory across calls. The growth is AddressSanitizer's
own allocator and quarantine, well documented to retain freed memory for
longer than a plain allocator would under a long-running, allocation-heavy
process, and `format`'s own design (a dedicated thread per call, for the
parser's and printer's recursion; see `format`'s doc comment in
`hollowscript-syntax/src/format.rs`) means this target creates and tears
down a thread on every one of its million-plus iterations, which is exactly
the kind of sustained allocation churn that pattern shows up under. Not a
defect in this workspace's code, and not fixed here, because there is
nothing to fix: recorded as a property of fuzzing this particular target
under ASAN for a long time, worth knowing before the next long run rather
than being mistaken for a regression. A host that wants to fuzz
`format_roundtrip` for longer than a few hundred thousand iterations in one
process should expect to raise `-rss_limit_mb` accordingly, or use
libFuzzer's `-fork` mode, which restarts workers periodically for exactly
this reason.

### Crash found and fixed: the formatter was not idempotent for a comment that opens the next construct

`format_roundtrip` found, within its first 20 minutes, a source where
formatting once and formatting the result again produced two different
outputs (`assertion left == right failed: format is not idempotent`),
which section 19.1's and this crate's own formatter contract both require
never to happen for a source that parses cleanly.

**Root cause**, in `hollowscript-syntax/src/format.rs`'s `Printer::is_trailing`:
a trailing comment (`x = 1 // why`) was recognised by checking only that
no newline lies between the previous construct's end and the comment's
start. A match statement can put two arms on one line
(`1 { a } 2 { // note` is valid: nothing requires a newline between arms),
and when the second arm's block opens with a comment right after its `{`,
that comment sits on the *first* arm's line with nothing between them but
the second arm's own pattern and brace, no newline anywhere. The old check
read that as "trailing the first arm" and moved the comment there; the
first format pass then printed a first arm that looked exactly like a
comment-bearing statement, the second pass re-parsed that printed form,
read the same textual relationship, and moved the comment again, but by
then the surrounding layout had changed enough that the two passes'
outputs no longer matched.

**Fix.** `is_trailing` now requires that only whitespace, not merely no
newline, separates the previous construct's end from the comment: nothing
of the next construct may lie between them. `has_comment_in` (which
decides whether an arm's own block contains a comment, forcing it onto
multiple lines) was already correct; the bug was only in what counted as
"trailing".

**Regression test**,
`hollowscript-syntax::format::tests::a_comment_that_opens_the_next_arm_is_not_read_as_trailing_the_previous_one`:
the exact shape (two match arms on one line, the second's block opening
with a comment), asserting both the corrected attribution and idempotence
across two format passes. Landed in the same commit as the fix, in
`hollowscript-syntax`, not only in `fuzz/`, per this workspace's own rule
that a fuzzer-found defect is fixed at its root with a test in the crate
that owns it.

No other target found a crash in its run.

## Coordinating with other workstreams

Fixing a crash found here touches the crate that owns the bug (in this
case `hollowscript-syntax`), which other workstreams may also be changing
concurrently. The fix above was kept small and self-contained (one
function's comparison, one new test) and landed as its own commit rather
than bundled with unrelated fuzzing infrastructure changes, so it could
rebase cleanly against other work landing on `main` at the same time.
