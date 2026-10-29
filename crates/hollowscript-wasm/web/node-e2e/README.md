# A Node end-to-end check of the JavaScript boundary

This crate takes no dependency on Node or npm to build. These are small
scripts that exercise `web/hollowscript.js` the way a page would. The first,
`main.mjs`, runs the VM in a real Worker thread, binds capabilities from JavaScript, including
`clock.now`/`clock.today` (a namespace capability, proving the mechanism
is not special-cased to a single-function one like `print`) and `cache`
(declared `Result<String, String>`, section 8.2, answered both as `Ok` and
as `Err`, each pattern-matched by the program rather than faulting), and
cancels a running call from the main thread while the Worker is
synchronously blocked inside WebAssembly, which is the one situation
`postMessage` cannot reach.

## Running all of them

```
crates/hollowscript-wasm/web/node-e2e/run-all.sh
```

builds what the checks need (the VM-only and the `tooling` module, both
`--release`, the command line and the two fixture builders, with a Cargo target
directory of its own, `target/node-e2e`, or `HOLLOWSCRIPT_E2E_TARGET_DIR`),
compiles the sample programs to bytecode in a temporary directory, and runs
every check below: `main`, `output-check`, `async-check`, `deadline-check`,
`load-result-check`, `results-check`, `bounds-check`, `memory-check`,
`tooling-check`, `ide-check`, `deep-nesting-check`, `trap-check`,
`instance-churn-check`, `limits-check`, `wrapper-hygiene-check` and
`mutation-check`. A check passes only if it exits 0 and prints
`all checks passed`. When `wasm-opt` is installed it also checks an optimised
build of the VM for the memory ceiling. It takes about 90 seconds from cold and
a little over a minute when repeated, `scripts/check` runs it when `node` and the
`wasm32-unknown-unknown` target are installed (and says so when it skips it),
and a new check is added to the list at the foot of `run-all.sh`.

## Running one of them

From the repository root:

```
rustup target add wasm32-unknown-unknown
cargo build -p hollowscript-wasm --release --target wasm32-unknown-unknown
cargo run -p hollowscript --example node_e2e_fixture -- \
    crates/hollowscript-wasm/web/node-e2e/sample.hws \
    crates/hollowscript-wasm/web/node-e2e/sample.hsb
node crates/hollowscript-wasm/web/node-e2e/main.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/sample.hsb
```

`sample.hsb` is not committed (bytecode is not a source artifact, per
`docs/spec/README.md`'s own exclusion of the bytecode format); build it
first. `hollowscript-cli`'s own `build` command cannot compile
`sample.hws`, because its capability interface is fixed to the CLI's own
capabilities (print, clock, random, env, args, fs, stdin) and knows nothing
of the host-defined `cache` this fixture binds from JavaScript; the
`node_e2e_fixture` example (`crates/hollowscript/examples/`) grants exactly
the interface `worker.mjs` binds instead, using the facade directly. A page
with its own compiler, or a build step of its own, would grant its own
capability interface the same way `hollowscript-cli::default_grants` does.

A clean run prints `run() -> ...`, `printed -> hello from wasm`,
`today() -> ...`, `lookup(found) -> ...`, `lookup(missing) -> ...`,
`spin() error -> The run was cancelled.` and finally `all checks passed`,
and exits 0. Any other outcome exits non-zero with `FAIL: ...` on stderr.

## The `tooling` build

`tooling-check.mjs` is the equivalent check for the `tooling` feature
: checking, formatting and compiling source with no server. Build the
`tooling` `.wasm` and run it against source text directly, no `.hsb` needed:

```
cargo build -p hollowscript-wasm --release --features tooling --target wasm32-unknown-unknown
node crates/hollowscript-wasm/web/node-e2e/tooling-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm
```

It checks a program with an unresolved name (expects `HS0205`), formats a
messy `constant`, and compiles a clean program to bytecode bytes (and a
program with an error to diagnostics instead), printing `all checks passed`
and exiting 0 on success. It also checks and compiles a program that only
`uses print` with no capability declaration at all (the standard
always-granted set must apply, the same as the CLI with no `--allow`), and
a program that `uses` a host-defined capability, first undeclared (expects
`HS0404`) then declared through `check`'s own third argument.

This test caught two real defects while this surface was being built:

- The front end's parser and formatter (`hollowscript-syntax`) each run on
  a dedicated, generously sized thread for section 1.10's stack-safety
  guarantee, and `wasm32-unknown-unknown` has no thread backend at all, so
  every `check`/`format`/`compile` call failed with an internal error until
  `hollowscript-syntax::parser::parse` and `::format::format` were changed
  to run on the calling stack instead when spawning a thread is not the
  target's own choice to make (decided with `cfg!(target_arch = "wasm32")`,
  ahead of the attempt, since a closure `Builder::spawn` fails to hand to a
  thread is not returned for a second try). Every other target is
  unaffected: the dedicated thread still runs exactly as before there.
- `check`/`compile` checked against an *empty* capability interface, so
  even `uses print` answered HS0404: a page had no way to say a program
  could use the standard capabilities at all. Fixed by checking against the
  standard always-granted set (print, clock, random) by default, the same
  interface the CLI grants with no `--allow`, with a third argument for a
  host-defined capability beyond it.

`tooling-check.mjs` also checks the filter schema and the filter literals
(`web/README.md`'s own "Filter schemas and filter literals"): a program that
names a field the schema does not have is refused by `check` (HS0708) and by
`compile`, with each rule of section A.7 reaching the host (HS0707, HS0709),
an empty schema refuses every field, a malformed schema is a diagnostic that
says what is wrong, and `filterLiterals` lists the literals of a program and
of an imported module with spans that slice a JavaScript string to exactly the
literal as written, after an emoji on the same line.

## The editor services

`ide-check.mjs` checks the editor services of the `tooling` build: lexical
highlighting, the keyword list, the standard capability declarations and the
analysis handle (hover, completion, definition, the outline, the handle's
lifecycle and its limit). It uses the same `.wasm` as `tooling-check.mjs`:

```
cargo build -p hollowscript-wasm --release --features tooling --target wasm32-unknown-unknown
node crates/hollowscript-wasm/web/node-e2e/ide-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm
```

Every offset a service answers is a UTF-16 code unit offset, half-open, so
the script slices the JavaScript source string with them directly, with an
emoji (a surrogate pair in UTF-16, four bytes in UTF-8) before the text of
interest, which is what proves the conversion from the checker's byte spans
is right. A clean run prints `all checks passed` and exits 0; anything else
exits non-zero with `FAIL: ...` on stderr.

## Reading a program's load result

`load-result-check.mjs` checks that `HollowScript.loadProgram` exposes
section 12.3's load result (the value a top-level `return` in the entry
module produced), which it used to discard silently:

```
cargo run -p hollowscript-cli -- build crates/hollowscript-wasm/web/node-e2e/load-result-sample.hws \
    -o crates/hollowscript-wasm/web/node-e2e/load-result-sample.hsb
node crates/hollowscript-wasm/web/node-e2e/load-result-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/load-result-sample.hsb
```

`load-result-sample.hws` is `return 41 + 1`, which needs no capability, so
this uses `hollowscript build` directly. A clean run prints
`loadResult -> Int(42)` and `all checks passed`.

## Suspend and resume (an asynchronous host)

`async-check.mjs` exercises `HollowScript.loadProgramBatched` (see
`web/README.md`'s own "Suspend and resume" section): a program
(`async-sample.hws`) calls one capability, `tick`, eight times in a loop,
and the host answers each call only after a real `setTimeout`, proving a
run can be suspended and resumed more than once. No `SharedArrayBuffer` is
used anywhere in this script.

```
cargo build -p hollowscript-wasm --release --target wasm32-unknown-unknown
cargo run -p hollowscript --example async_e2e_fixture -- \
    crates/hollowscript-wasm/web/node-e2e/async-sample.hws \
    crates/hollowscript-wasm/web/node-e2e/async-sample.hsb
node crates/hollowscript-wasm/web/node-e2e/async-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/async-sample.hsb
```

`async-sample.hsb` is not committed, for the same reason `sample.hsb`
is not: `async_e2e_fixture` (`crates/hollowscript/examples/`) grants the
`tick` capability directly with the facade, the same way `node_e2e_fixture`
does for `sample.hws`, since `hollowscript-cli build` knows nothing of a
host-defined `tick` either.

A clean run prints `ticks -> 8`, `result -> Int(204)` (the sum of `i * i`
for `i` from 1 to 8, each squared by the simulated async host) and finally
`all checks passed`, and exits 0.

## A deadline without a Worker

`deadline-check.mjs` exercises `HollowScript`'s `deadlineMs` and
`shouldCancel` options (see `web/README.md`'s own "A deadline, with no
Worker"): on one thread, with no `SharedArrayBuffer` in the way, an endless
loop is stopped with the cancelled fault (HS1006), by a wall-clock budget
and by a callback, the instance is usable for the next call afterwards, a
callback that throws cancels instead of trapping, a load that never
finishes is stopped too, and the shared flag still works and is read first.

```
cargo build -p hollowscript-wasm --release --target wasm32-unknown-unknown
cargo run -p hollowscript-cli -- build crates/hollowscript-wasm/web/node-e2e/deadline-sample.hws \
    -o crates/hollowscript-wasm/web/node-e2e/deadline-sample.hsb
cargo run -p hollowscript-cli -- build crates/hollowscript-wasm/web/node-e2e/deadline-load-sample.hws \
    -o crates/hollowscript-wasm/web/node-e2e/deadline-load-sample.hsb
node crates/hollowscript-wasm/web/node-e2e/deadline-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/deadline-sample.hsb \
    crates/hollowscript-wasm/web/node-e2e/deadline-load-sample.hsb
```

Neither sample needs a capability, so `hollowscript build` compiles them
directly. A clean run prints the three lines that show how long each stop
took and finally `all checks passed`, and exits 0.

## Structured results and faults

`results-check.mjs` exercises `resultFormat: "json"` and `HollowScriptFault`
(see `web/README.md`'s own "Structured results and faults"). `results-sample.hws`
returns every kind of value (a scalar, an optional, a list, a map, a set, a
record, an enum, nothing, and a result nested twenty thousand deep), with an
`Int` above 2^53 in each position a value can hold one, so the check is that
none of them is read back as a different number. It also checks the fault of
a division by zero (code, class, file, line, column, span), of a limit
(`data`), of a load and of a cancelled run, that a refused call stays a plain
`Error`, and that the instance carries on afterwards. `async-check.mjs`
repeats its run in the JSON format and checks the fault of a call the host
answered with an `Error`.

```
cargo build -p hollowscript-wasm --release --target wasm32-unknown-unknown
cargo run -p hollowscript-cli -- build crates/hollowscript-wasm/web/node-e2e/results-sample.hws \
    -o crates/hollowscript-wasm/web/node-e2e/results-sample.hsb
cargo run -p hollowscript-cli -- build crates/hollowscript-wasm/web/node-e2e/results-load-fault-sample.hws \
    -o crates/hollowscript-wasm/web/node-e2e/results-load-fault-sample.hsb
node crates/hollowscript-wasm/web/node-e2e/results-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/results-sample.hsb \
    crates/hollowscript-wasm/web/node-e2e/results-load-fault-sample.hsb
```

A clean run prints three lines (the exact `BigInt`, the fault's code and
position) and `all checks passed`, and exits 0. The line and column it checks
are those of `results-sample.hws` as committed.

## The output limit and a host-bound `print`

`output-check.mjs` checks that what a host-bound `print` writes counts
against `maxOutputBytes` (sections 11.6 and 16.2; see `web/README.md`'s own
"`print` and the output limit"). It reuses `sample.hws`, whose `run` prints a
15 byte line (16 bytes with its line feed): a limit of 16 lets it through, a
limit of 15 faults with HS1004 and the host's `print` is never called for the
line, and the default limit leaves an ordinary run alone.

```
cargo build -p hollowscript-wasm --release --target wasm32-unknown-unknown
cargo run -p hollowscript --example node_e2e_fixture -- \
    crates/hollowscript-wasm/web/node-e2e/sample.hws \
    crates/hollowscript-wasm/web/node-e2e/sample.hsb
node crates/hollowscript-wasm/web/node-e2e/output-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/sample.hsb
```

A clean run prints the fault's code and what was written, `HS1004` and an
empty string, and `all checks passed`, and exits 0.

## Bounded writing, and exact capability arguments

`bounds-check.mjs` checks that what the module writes out for a host is
bounded (see `web/README.md`'s "Everything the module writes out for a host
is bounded") and that an `Int` above 2^53 - 1 reaches a capability handler as a
`BigInt`. `bounds-load-sample.hws` is the six-line program that used to make the
module allocate about 3 GB (a value of about 2^50 paths out of fifty shared
objects); `bounds-sample.hws` returns the same value from a call, reads it
from a constant and builds it at load; `bounds-sink-sample.hws` hands a
shared value, and a list of large `Int`s, to a capability. The programs are
compiled with the `tooling` build and run on the VM-only one, so the check
takes both `.wasm` files:

```
node crates/hollowscript-wasm/web/node-e2e/bounds-check.mjs vm.wasm tooling.wasm
```

It checks that each writer faults with HS1004 within milliseconds and with the
module's memory a few MiB, that a deadline reaches the writing (HS1006), that
the instance carries on after a fault of a result, and that the batched path
behaves the same.

## The memory ceiling

`memory-check.mjs` checks the module's memory contract (see `web/README.md`'s
"Memory"): the module imports `env.memory` and declares a maximum of 4,096
pages (256 MiB); WebAssembly refuses a memory with a larger maximum or none;
the wrapper makes the memory with the module's maximum, or a lower one for
`maxMemoryBytes` (and a `RangeError` for a value above or below what the module
allows); an ordinary program runs under either; the language's own limit still
faults (HS1002) first for a program that stays inside the ceiling; and an
allocation loop, with the logical limit raised out of the way, is stopped at the
ceiling by a trap, after which every call on the module throws
`HollowScriptTrap`, `dispose()` does nothing, and a fresh instance is
unaffected. It reports what 600,000 pairs takes as text and as JSON. Like
`bounds-check.mjs` it compiles `memory-sample.hws` with the `tooling` build and
runs it on the VM-only one, and every `.wasm` after the second argument is a
further build of the VM checked the same way, which is how an optimised build
(`wasm-opt -Oz`, the `min-size` profile) is shown to keep the maximum:

```
node crates/hollowscript-wasm/web/node-e2e/memory-check.mjs vm.wasm tooling.wasm [optimised-vm.wasm ...]
```

## Deep nesting and a module that traps

`deep-nesting-check.mjs` runs the front end of the `tooling` build over every
construct at the deepest nesting section 1.10 allows (found by bisection on the
`HS0113` and `HS0114` diagnostics) and one level past it, through `check`,
`format`, `compile` and `analyse`, and requires every one to finish with the
instance still working. It starts from the program that used to trap the module,
235 nested closures. The module's shadow stack is 1 MiB unless
`crates/hollowscript-wasm/build.rs` sizes it, so a `tooling` build without that
fails here. `trap-check.mjs` hands an export a pointer outside the module's
memory, which is a real trap, and requires the first call to throw a
`HollowScriptTrap` and every later call on the instance (and on a handle made
before the trap) to throw one before reaching the module. It takes the `tooling`
`.wasm` and, optionally, the plain one.

```
cargo build -p hollowscript-wasm --release --features tooling --target wasm32-unknown-unknown
node crates/hollowscript-wasm/web/node-e2e/deep-nesting-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm
cp target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm /tmp/tooling.wasm
cargo build -p hollowscript-wasm --release --target wasm32-unknown-unknown
node crates/hollowscript-wasm/web/node-e2e/trap-check.mjs \
    /tmp/tooling.wasm target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm
```

Both print `all checks passed` and exit 0.

## Loading and disposing in a loop

`instance-churn-check.mjs` loads a program and disposes it 20,000 times and
requires the module's memory not to have grown, then checks that disposing a
handle twice, after its place has been reused, leaves the new instance alone. It
takes the plain `.wasm` and a bytecode file, `load-result-sample.hsb` from
"Reading a program's load result" above:

```
node crates/hollowscript-wasm/web/node-e2e/instance-churn-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/load-result-sample.hsb
```

A clean run prints the growth (0 bytes) and `all checks passed`. Before places
were reused it grew by 38 MB.

## Limits

`limits-check.mjs` checks that `setLimits` refuses what is not a limit (a
negative number, a fraction, `NaN`, a string, a value above 2^63 - 1) and changes
none of the limits when it does, that the export behind it refuses the same, and
that the largest limit, 0 and a `BigInt` are accepted. It uses the endless loop
`deadline-sample.hws`, built to bytecode, to read back the step limit.

```
cargo run -p hollowscript-cli -- build crates/hollowscript-wasm/web/node-e2e/deadline-sample.hws \
    -o crates/hollowscript-wasm/web/node-e2e/deadline-sample.hsb
node crates/hollowscript-wasm/web/node-e2e/limits-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm \
    crates/hollowscript-wasm/web/node-e2e/deadline-sample.hsb
```

It prints `all checks passed` and exits 0.

## The wrapper's housekeeping

`wrapper-hygiene-check.mjs` checks that a call whose arguments cannot be turned
into JSON (a circular object, a value whose `toJSON` throws) throws and leaves nothing behind in the
module's memory (the name buffer used to be uploaded first and never freed), for
`call` and for the `tooling` build's `check` and `analyse`, and that an `Analysis`
that is garbage collected without `dispose()` gives its place back, so five
rounds of 60 forgotten ones do not reach the cap of 64. It turns the engine's
`gc()` on for itself.

```
node crates/hollowscript-wasm/web/node-e2e/wrapper-hygiene-check.mjs \
    tooling.wasm plain.wasm crates/hollowscript-wasm/web/node-e2e/load-result-sample.hsb
```

where `tooling.wasm` and `plain.wasm` are the two builds (see above). It prints
`all checks passed`.

## Mutating a container against the module

`mutation-check.mjs` overwrites every four-byte window of a container, in turn,
with the largest integers (`0xFFFFFFFF`, `0x7FFFFFFF`, `0x80000000`), makes the
checksum valid again so that the mutant reaches the decoder and the verifier, and
loads it into a fresh instance of the plain `.wasm`. A mutant must be refused, or
load and run; it must never trap the module. A string id of `0xFFFFFFFF` in a
constant used to make the module resize a cache to four billion entries, which
trapped it after a 316-byte container verified (three mutants of
`return "hello"` did so). Give it one or more bytecode files:

```
cargo run -p hollowscript-cli -- build hello.hws -o hello.hsb
node crates/hollowscript-wasm/web/node-e2e/mutation-check.mjs \
    target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm hello.hsb
```

It prints how many mutants were loaded and how many refused, and `all checks
passed`. The Rust counterpart, over the whole corpus and in `scripts/check`, is
`crates/hollowscript/tests/mutated_bytecode.rs`.
