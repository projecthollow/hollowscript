# Testing strategy

Six kinds of test cover different properties, and no one of them is a
substitute for another: a unit test checks one function's own logic against
hand-chosen inputs, and a fuzzer checks that nothing in a much larger surface
ever panics, and neither tells you what the other does.

## Unit tests

Every module written so far carries its own `#[cfg(test)] mod tests`,
exercising the module's public behaviour directly rather than through a
larger pipeline: the arena's generational reuse, the heap's node cost
formulas, the verifier's individual rules, the grant grammar's edge cases,
and so on. This is deliberate and will stay the primary form of test for
each crate's own logic: a unit test for, say, the exhaustiveness algorithm's
budget should not need a working parser to set up, and should not have to
change just because an unrelated part of the checker changed.

The workspace's lint policy (`unwrap_used`, `expect_used` and `panic` denied
outside tests, but explicitly allowed inside them by `clippy.toml`) reflects
this split directly: a unit test may `unwrap()` a `Result` it has already
asserted is `Ok`, but the code under test may not, because the code under
test must handle every input, including a malicious one, and a test only
has to handle the inputs it chose.

## Snapshot tests

Not yet needed, because nothing produces output complex enough to warrant
one: the formatter (`hollowscript_syntax::format`) is the natural candidate
once it exists, since "does this program format to exactly this text" is
better expressed as a stored expected output than as a hand-written
assertion for every case. The corpus's own `format` field (see below) already
serves this purpose for the cases in `corpus/formatting/`; a dedicated
snapshot mechanism is worth adding only if the corpus's coverage turns out
to be too coarse for day-to-day formatter development, which will not be
known until the formatter is written.

## The corpus

`corpus/` (255 cases at the time of writing, spanning lexical structure,
types, expressions, statements, modules, capabilities, grants, patterns,
destructuring, results, enums, collections, the standard library, narrowing,
scope, formatting, and every example program the specification itself
contains) is the executable form of the specification, per section 19.1, and
running it correctly is what "conforms to the specification" means for any
implementation, in any language. `hollowscript-conformance` is this
workspace's own runner, and it is the project's main gate. For every case it
compares four things against `<name>.expected.json`: the parse diagnostics,
the check diagnostics, the formatter's output and its idempotence, and the
run's printed output, result value and fault. The corpus's fixed host
(`print`, a frozen `clock` and `random`, and `kind`) is bound behind it, and
a case's `<name>.modules/` directory is its loader, so a multi-module case
runs as one program.

A summary would be no gate at all: the corpus is a specification artefact,
so most of it fails while part of the language is unimplemented, and a
number that only goes up tells nobody whether a change broke something. The
gate is therefore a baseline. `crates/hollowscript-conformance/known-failures.txt`
lists, by case id, every case this implementation does not pass, and the run
fails in **both** directions: when a case outside the list fails, which is a
regression, and when a case inside it passes, which means the list is stale
and has to be shortened. The list can only shrink. `scripts/check` runs it,
so it gates every change like the tests do.

The crate may use no external dependency (see
[architecture.md](architecture.md)), so it carries its own JSON reader,
scoped to the shape the expected files use.

## Deterministic, seeded fuzzing

Two layers, not one, because they trade off differently.

**In `scripts/check`, on every change**: hand-rolled, seeded fuzz tests with
no extra tooling beyond `cargo test` (`hollowscript-syntax/tests/fuzz.rs`
and `format_fuzz.rs`, `hollowscript/tests/fuzz_pipeline.rs` for the
whole pipeline, and `hollowscript/tests/mutated_bytecode.rs` for bytecode,
which overwrites bytes of valid containers with the largest integers, repairs
the checksum and requires every mutant to be refused or to run within its
limits: the byte-level mutation of a coverage-guided fuzzer reaches a string id
of `u32::MAX` only by chance, and that one id once made the interpreter size a
cache to four billion entries). Each mutates the conformance corpus with a small, local
linear congruential generator seeded by one fixed number, so a failure is
reproducible in an ordinary test run without needing a fuzzer at all,
matching this workspace's wider preference for tests that run the same way
every time (see [errors-and-determinism.md](errors-and-determinism.md)).
These run in seconds and catch a regression before it lands.

**Continuous, coverage-guided, in `fuzz/`** (a separate `cargo-fuzz` crate,
outside this workspace and never shipped; owner decision, 2026-09-23): the
same properties, found by libFuzzer's own mutation and coverage feedback
rather than a fixed seed, over hours rather than seconds, which is what
finds the input the hand-rolled mutator's own move set does not happen to
reach. See [fuzzing.md](fuzzing.md) for the targets, how to run them, and
what each one has found.

The target surface:

1. **The lexer and parser** (`hollowscript-syntax`), on arbitrary byte
   strings: section 19.1 requires that parsing "never crashes,
   hangs or exhausts its host on any input... including arbitrary bytes",
   which a fuzzer is the direct way to gain confidence in, beyond what the
   structural-limit tests already check by construction (see
   `hollowscript_syntax::budget` and its tests, which check the parser's own
   depth and chain-length accounting rather than fuzzing it).
2. **The formatter**, for the never-panics property on any input and, on a
   source that parses cleanly, the round-trip contract (idempotence, output
   that parses back to the same tree).
3. **The checker**, on arbitrary source, for a single-file program.
4. **The whole pipeline** (check, compile, verify, load, run, then call
   every export), with small limits so every input terminates.
5. **The bytecode container and verifier** (`hollowscript-bytecode`), on
   arbitrary byte strings interpreted as a container: the container reader
   already refuses every count that does not fit the remaining buffer before
   allocating (see [bytecode.md](bytecode.md)), which a fuzzer would
   confirm holds for inputs no unit test happened to construct, and would
   also be the first line of defence against a defect in the verifier that
   let malformed bytecode reach the interpreter. The property is the
   strongest one this workspace fuzzes: a module the verifier accepts must
   never make the VM panic or read out of bounds while running it.
6. **The C ABI and the WebAssembly ABI**, called with arbitrary bytes and
   arguments, the way a host that is not Rust, and cannot be trusted to
   pass well-formed input, would call them.

## Property tests

Complementary to the corpus, for properties that hold across many inputs
rather than being pinned to one example: a value's tree size (section 11.4)
should never decrease under any operation the library defines except an
explicit narrowing one, a parsed-then-formatted-then-parsed-again program
should produce the identical tree (formatting is idempotent, which the
corpus's own format field already checks one case at a time, but a property
test can generate many more shapes than were written by hand), and a
verified module's execution should never observe an operand out of range
regardless of what the compiler emitted for it (checked today only by the
hand-written cases in `hollowscript-bytecode`'s verifier tests). No property
testing library is a dependency yet; a small local generator is enough for
the properties above and keeps with the tiny-dependency policy of
[dependencies.md](dependencies.md) unless a real need for shrinking or more
elaborate generation appears once there is more to generate over.

## End-to-end checks of the JavaScript boundary

`crates/hollowscript-wasm/web/node-e2e/` holds Node scripts that run
`web/hollowscript.js` against the real `.wasm` builds, the way a page does: a
worker cancelled from another thread, a deadline with no worker, the
suspend and resume path, structured results and faults, the output limit, the
bounds on everything written out for a host, the memory ceiling and the
trap that follows it, the tooling build and the editor services. A Rust test
cannot stand in for them, because the properties are those of the module as a
browser loads it (`wasm32-unknown-unknown` has no unwinding, an imported
memory, a real `memory.grow` that fails), and they were not part of
`scripts/check`, which is how a break of the wrapper once reached `main`.
`run-all.sh` in that directory builds what they need with a Cargo target
directory of its own and runs every one of them, and `scripts/check` runs it
whenever `node` and the `wasm32-unknown-unknown` target are installed (about
30 seconds from cold, 10 repeated; it says so when it skips). A check passes
only if it exits 0 and prints `all checks passed`, and a new check is added to
the list at the foot of the script.

## Differential runs

Comparing this implementation against a second one built independently from
the same specification (section 19.7's own definition of conformance: "two
implementations... behave identically on every program") is the strongest
check available, and cannot be done until a second implementation exists.
Nothing in this workspace performs such a comparison; `hollowscript-conformance`
is scoped to run this implementation against the shared corpus, which is the
comparison that is possible today.
