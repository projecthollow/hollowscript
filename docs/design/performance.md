# Performance: measured numbers and budgets

The plan this document used to hold ("no budget is a measured number yet")
is now done: `cargo xtask bench` (`xtask/src/bench.rs`) is the harness, and
the numbers below are its output on one machine, on one run, dated
2026-09-23. They are not a controlled benchmarking environment's numbers
(see "A caveat on the numbers" at the end), but they are real, not
projected.

## Running it

```
cargo xtask bench            # everything, including a WebAssembly build
cargo xtask bench --no-wasm  # skip the WebAssembly size measurement
```

Loads and runs each representative program through the `hollowscript`
facade a real embedder uses (never a private fast path into the VM), with
one discarded warm-up run and five timed samples, the median reported,
matching the previous implementation's own benchmark methodology so the
two sets of numbers are produced the same way. It also builds
`hollowscript-cli` and, unless `--no-wasm` is given, `hollowscript-wasm`
for `wasm32-unknown-unknown`, both at the workspace's `min-size` profile,
and reports their binary size, and measures the CLI's own cold start.

Peak memory is **not** measured by `cargo xtask bench`: reading a child
process's own maximum resident set size needs a platform-specific call
(`/usr/bin/time -l` on macOS, `/usr/bin/time -v` on Linux) that this
harness does not yet wrap portably. The numbers in this document's memory
table were taken by hand, on macOS, the same way the command's own output
tells you to.

## The representative programs

Eight programs, chosen to stress the interpreter's cost centres separately
rather than lumped into one script, matching the specification's own list
of what a benchmark suite should cover (an arithmetic loop, recursion,
string building, list sort and map, JSON parse and stringify, a
closure-heavy workload):

| Program | What it measures |
|---|---|
| `arithmetic-loop` | statement and expression evaluation overhead, nothing else |
| `recursive-fibonacci` | the call path and the call-depth guard, at volume (naive `fib(24)`, 150,049 calls) |
| `list-map-filter-reduce` | closure invocation through the standard library's dispatch |
| `record-field-access` | record construction and structural field lookup |
| `string-interpolation-loop` | repeated `"${text}x"` interpolation, deliberately quadratic, the same way it is by accident in a real script |
| `list-sort` | `List.sort(compare)`'s merge schedule (section 14.1) over 5,000 pseudo-random `Int`s |
| `json-roundtrip` | `json.stringify` then `json.parse` over 1,000 built `Json.Object` values |
| `closure-heavy` | closure allocation (1,000 to 4,000 closures each capturing one `Int`) and indirect calls through `reduce`, separately from `recursive-fibonacci`'s direct, non-closure calls |

The first five use byte-for-byte the same HollowScript source as the
previous implementation's own benchmark suite, so their numbers describe
the same work and a direct comparison is fair. The last three are not in
that suite and have no baseline entry from it.

## Measured: throughput and load time

```
program                           ops/sec    load (ms)
arithmetic-loop                 1,508,653      133.928
recursive-fibonacci             1,601,518       93.848
list-map-filter-reduce            331,913        4.482
record-field-access               361,658        5.190
string-interpolation-loop       1,013,157        3.011
list-sort                         140,660       28.969
json-roundtrip                     54,403       18.585
closure-heavy                     232,176       16.543
```

`load (ms)` is `hollowscript::load` alone (check, compile, verify,
initialise), not included in the throughput run's own timing; it is
dominated by checking a program the size of these, not by the VM.

## Compared against the previous implementation's baseline

The previous implementation's own committed benchmark baseline was read,
not modified, and its package was not run (running it needs a separate
toolchain and build, which was outside this change's scope once the
committed baseline already gave a number to compare against by hand).

| Program | This implementation | Previous implementation | Ratio |
|---|---:|---:|---:|
| `arithmetic-loop` | 1,508,653 | 862,205 | 1.75x |
| `recursive-fibonacci` | 1,601,518 | 988,618 | 1.62x |
| `list-map-filter-reduce` | 331,913 | 266,883 | 1.24x |
| `record-field-access` | 361,658 | 247,234 | 1.46x |
| `string-interpolation-loop` | 1,013,157 | 892,204 | 1.14x |

A bytecode virtual machine outperforming a tree-walking interpreter on
every one of the five shared benchmarks is the expected result, not a
surprising one; the margin is modest rather than dramatic on the two
closure-dispatch-heavy benchmarks (`list-map-filter-reduce`,
`record-field-access`), which says the gain is concentrated in raw
statement and call overhead rather than everywhere equally.

## Compared against Lua

`bench-lua/run.lua` (`scripts/bench-lua` runs it; needs Lua 5.4, for
example `brew install lua@5.4` on macOS, where it installs keg-only) is a
Lua 5.4 program with the same eight benchmarks, each doing the same
abstract work as its HollowScript counterpart, written the straightforward
way a Lua programmer would write it: the same element counts, the same
operations (build, map, filter, reduce, sort, a JSON encode and decode,
allocate and call a closure), the same one-discarded-warm-up-then-five-
timed-samples-median methodology (`os.clock()` in place of
`std::time::Instant`, which is the natural choice in Lua's own standard
library and, at the resolution these programs run at, is not the limiting
factor). Not a byte-for-byte port: HollowScript keeps values immutable, but
the VM now grows a list in place when the receiver has no other live alias
and its result immediately replaces that binding. An aliased receiver still
copies to preserve the old value; Lua's tables are ordinary mutable arrays,
so `numbers[#numbers+1] = x` is amortised O(1) in either case. Both
benchmarks call append/insert the same number of times; writing an
artificially slow Lua program to match HollowScript's own copying would hide
a real, honestly reportable difference rather than reveal one.
`string-interpolation-loop` is the one program where the two languages'
own complexity agrees: Lua strings are immutable too, so repeated `..`
concatenation is quadratic in both.

```
program                           HollowScript      Lua 5.4       Ratio
arithmetic-loop                     1,508,653    99,255,583      0.015x
recursive-fibonacci                 1,601,518   131,621,930      0.012x
list-map-filter-reduce                331,913    19,801,980      0.017x
record-field-access                   361,658     9,661,836      0.037x
string-interpolation-loop           1,013,157    15,789,474      0.064x
list-sort                             140,660     3,526,093      0.040x
json-roundtrip                         54,403        99,721      0.546x
closure-heavy                         232,176     9,132,420      0.025x
```

Lua's own mature, C-based interpreter is faster than this implementation
on every one of the eight, by a wide margin on most: this is the honest
result, not a favourable framing of it. Two things narrow the gap on
inspection rather than explain it away. First, `json-roundtrip`, the one
benchmark closest to parity (0.546x), is also the one where Lua carries
extra, unavoidable cost this comparison charges it for fairly: Lua 5.4 has
no JSON support at all, so `bench-lua/run.lua` includes a small hand-written
encoder and a general recursive-descent decoder (visible in the file),
where HollowScript's `json.stringify` and `json.parse` are native library
calls; a benchmark that instead measured only table-building would show a
wider gap here too, the same shape as the other seven. Second, per the
note above, `list-sort` and `closure-heavy` both build their input through
a loop of appends, which is a full copy each time in this implementation
and O(1) in Lua; `docs/design/security.md`'s memory section quantifies
that difference directly (a 348 MB-to-3 MB gap for `list-sort`'s own input
alone) and it very likely accounts for a real share of this implementation's
slowdown on those two specifically, on top of the throughput gap the other
six already show. Neither observation changes the headline number: a
mature, purpose-built C interpreter with none of this implementation's
per-operation resource accounting (every step, every allocation charged
and checked against a host's limits, section 11) is faster, and closing
that gap, if it is worth closing, is real future work, not something this
comparison's own existence does.

### Binary size

```
                                min-size profile    readily available build
hollowscript-cli (native)             1.04 MB       lua5.4 (Homebrew, native)   52.2 KB
hollowscript-wasm (wasm32)            0.41 MB       wasmoon's glue.wasm         265.2 KB
```

The native comparison is not apples to apples and is not presented as one:
`lua5.4` is the Homebrew build's own binary, not built at this workspace's
`min-size` profile (fat LTO, symbols stripped, `opt-level = "z"`) or even
necessarily statically linked, and Lua's own standard library and language
surface is far smaller than HollowScript's (no static type system, no
effects and capabilities, no `Result`, a smaller standard library). It is
recorded because the task asked for it, not because the two numbers answer
the same question.

For WebAssembly, a reputable, actively maintained Lua-in-WebAssembly build
was readily available and used rather than skipped: `wasmoon`
(`npm view wasmoon`, a real Lua 5.4 VM compiled to WebAssembly with JavaScript
bindings, 54 published versions at the time of writing), whose package
(`npm pack wasmoon`) contains `dist/glue.wasm`, 265.2 KB. This is a closer
comparison than the native one (both are the VM's own compiled core, nothing
else), though still not perfectly matched: `hollowscript-wasm` deliberately
excludes the parser and checker (`docs/design/architecture.md`; a published
game bundles bytecode compiled at publish time, so the browser VM could not
compile source if it wanted to), where a Lua-in-the-browser build more often
includes Lua's own compiler, since Lua scripts are typically shipped as
source. HollowScript's WASM build is still the larger of the two by roughly
1.5x, which, given it excludes a whole pipeline stage Lua's build very
likely does not, is worth noting honestly rather than presented as a win.

## Measured: binary size (`min-size` profile)

```
hollowscript-cli    1.04 MB
hollowscript-wasm   0.41 MB
```

`hollowscript-cli` is the `hollowscript` binary (see its crate's `[[bin]]`
name) built with `opt-level = "z"`, fat LTO and symbols stripped.
`hollowscript-wasm` is `hollowscript_wasm.wasm` for
`wasm32-unknown-unknown`, same profile. Both are comfortably within any
budget either audience (a command-line tool competing with other
interpreters, a browser build competing with a published game's asset
budget) would reasonably set; see "Budgets" below for the numbers actually
proposed.

## Measured: cold start

```
median wall time: 2.57 ms
```

The `hollowscript-cli run` process, from spawn to exit, for a two-line
program (`uses print` and one `effect print` call), median of five runs
after one discarded warm-up. This is **not** "time to first output" in
isolation: a child process's stdout is only read back after the process
exits in this harness, so the number is process start, plus `load`, plus
the run, plus the process's own teardown, together. A streaming reader that
timestamps the first byte written would be a truer number and is not
implemented; this is recorded as a limitation rather than presented as more
precise than it is.

## Measured: peak memory (macOS, `/usr/bin/time -l`, "maximum resident set size")

```
                                        before the fix   after the fix
arithmetic-loop (200,000 iterations)          2.3 MB          2.3 MB
json-roundtrip (1,000 objects)               31.7 MB         31.7 MB
list-sort (5,000 Ints)                      349.4 MB        348.3 MB
closure-heavy (4,000 closures)              232.7 MB        232.8 MB
range(0, 8,000,000), retained                578.4 MB        194.3 MB
```

The specification's own logical accounting (section 11.4) bounds a run to
`Limits::default().max_memory_bytes`, 64 MiB, and every program above ran
inside that limit without faulting: none of this is a program escaping its
resource limits, all of it is the gap between what section 11.4 counts and
what the process actually holds, which is disclaimed on purpose
(`docs/design/security.md`, "what it deliberately does not promise") but
worth knowing the size of. `arithmetic-loop`'s real overhead over its
logical footprint is small and unremarkable; the other three were each
investigated, with two different results.

### Fixed: a traversal that copied every scalar element for nothing

`range(0, 8000000)` (a top-level `constant` bound to an 8,000,000-element
list of plain `Int`s, well within the 64 MiB accounting limit at 8 bytes
charged per element) measured at 578 MB before this fix: roughly nine
times the ~64 MB the language's own accounting counts. `heap.rs`'s
`Heap::children` (walked by both `Heap::tree_size`, run after every load
and every call to check section 11.4's retained-size limit, and
`Heap::collect`) and two near-identical hand-rolled traversals in
`interp.rs` (`Interpreter::check_retained`, the same per-call check;
`Interpreter::capability_size`, a capability result's own size) each
pushed *every* element of a list, record or enum payload onto an auxiliary
work list to visit it, including a plain `Int`, `Float`, `Bool` or `none`
that cannot itself lead anywhere else in the heap and, once popped, was
immediately discarded by exactly that check (`Heap::node_cost` already
returns 0 for a non-heap value without looking anything up). For a
1,000,000-element list, that is 1,000,000 needless 24-byte
(`size_of::<Value>()`, on a 64-bit host) copies into a `Vec` on top of the
list's own storage, once per traversal, and this one program's run reaches
that walk at least twice (`check_retained` after the call, plus the
per-binding `tree_size` it calls into). All three sites now filter to only
`Value::Heap` elements before pushing (`hollowscript-vm/src/heap.rs`,
`hollowscript-vm/src/interp.rs`; each has its own comment explaining why,
and `heap.rs`'s own test suite gained a direct regression test for
`Heap::children`). This changes no observable behaviour: a fault that was
reported before is still reported, at the same size, since the filtered
elements only ever contributed 0 to any count. Verified with `cargo test -p
hollowscript-vm`, the full conformance corpus and specification examples
(unchanged), and this program specifically: 578 MB to 194 MB, roughly 9.0x
the logical accounting down to roughly 3.0x. That remaining 3.0x is
`Value`'s own representation size (24 bytes for a tagged union that must
hold either a full 64-bit scalar or a generational handle, `index: u32` and
a deliberately wide `generation: u64` for long-lived hosts;
`hollowscript-vm/src/arena.rs`'s own doc comment explains why the
generation is not narrower) against section 11.4's 8-bytes-per-element
formula; shrinking it further would mean either narrowing that generation
counter (undoing a documented safety decision) or an unsafe, packed
representation (forbidden in this crate), so it is recorded here as a
measured floor, not chased further under this change.

### Fixed: `list-sort` and `closure-heavy` retained every discarded intermediate generation

The fix above did not move `list-sort` or `closure-heavy` at all (349.4 MB
to 348.3 MB, 232.7 MB to 232.8 MB: noise). Their own retained data is tiny
(5,000 `Int`s, 4,000 one-capture closures), so the traversal fix, which
only helps a walk over a *large retained* structure, had nothing to do
there. Building the same final list a different way isolated the real
cause directly:

```
list-sort's input, built by 5,000 sequential `numbers.append(...)`     348 MB
the same 5,000-element list, built by one `range(0, 5000)` call          3 MB

closure-heavy's closures, built by 4,000 sequential `.append(...)`     233 MB
the same 4,000 closures, built by one `range(0, 4000).map(...)` call     5 MB
```

`list.append` (like every operation on an immutable collection; section 6)
copies the whole collection to produce the new one: correct, and exactly
what the specification's own accounting charges for (the delta, not a
running total; section 11.3's allocation counter is why a loop of 5,000
appends does not itself fault). What it did not account for is that
nothing froze the 4,999 discarded intermediate lists it left behind: the
collector used to be "safe only between runs, where the interpreter holds
no frame with registers in it", so a `while` loop's own frame, mid-run,
was never a safe point to collect at, and every generation of `numbers` a
loop iteration replaced stayed reachable through the arena, unfreed, until
the call returned. The total was not a fixed multiple of the final list's
own size: it was the sum of every intermediate generation's size, which
grows with the count of appends, not with the final result.

`natives.rs`'s own merge sort (`advance_sort`) was read looking for an
avoidable clone, since that was one of the suggested places to look: it
already moves values with `std::mem::take` rather than cloning them, and
`range(0, 5000).sort(...)`'s own 3 MB confirms the merge itself was never
the cost here.

**The fix (mid-run collection).** The collector now runs during a call, not
only between calls, closing this gap without giving up the mark-and-sweep
collector's own safety property (only a value nothing rooted can be freed)
and without a structurally-shared persistent list, which would have been a
much larger redesign.

*Rooting.* `Interpreter::run_frames`'s frame loop already
keeps every active call's registers in a `Vec<Frame>` it owns (never on the
host Rust stack: "The call stack is the interpreter's own", that module's
own doc), specifically so that call depth is bounded by a counter, not by
the host's stack. That turned out to already be almost everything a
collection needs: the one place in the whole loop where every frame's
registers are guaranteed to be back in that `Vec` (rather than moved out
into a local for the one instruction currently touching them) is the very
top of the loop, before the next instruction is even decoded. Collecting
only there, and never anywhere else, means the root set is always exactly
`self.globals`, the host's own held-over roots (`Interpreter::host_roots`,
new: previously these lived only in the facade's `Instance`, which a
mid-run collection inside the VM crate cannot reach out to mid-call, so the
VM now keeps its own copy that `Instance::remember` and
`Instance::release_values` mirror into) and whatever `push_frame_roots`
finds in the frame stack: a script frame's registers and the cells its
closure captured (read directly by `GetUpval`/`SetUpval`, not necessarily
copied into any register), and a library function's own job state
(`NativeJob::push_roots`: the callback, the elements, what has been kept so
far, the reduce accumulator, and a `sort` job's own in-progress merge).
Nothing is ever transiently reachable only from a Rust-stack local across
this one safepoint, so no separate bookkeeping was needed to make it
precise; a suspended run's frames are rooted the same way, from
`Interpreter::suspended`, so `Interpreter::collect` is now just as safe to
call while a batched-path run is paused waiting for the host's capability
results as it is between calls.

*Trigger.* A safepoint check (a single integer comparison,
`Heap::due_for_collection`) runs on every pass through the frame loop; an
actual collection is gated behind it. `Heap` tracks bytes of node cost
allocated since the last collection and compares that against a threshold
that starts at 2 MiB and, after each collection, is recomputed as twice the
live bytes that collection found (floored at 2 MiB): the classic doubling
policy of a generational or semispace collector, chosen so collection cost
stays amortised against bytes allocated (section 11.7) rather than
triggered on a fixed schedule regardless of how much garbage there is to
find. 2 MiB, not something much smaller, because a workload that keeps
almost everything it allocates live (`closure-heavy`, `record-field-access`:
see "Measured: throughput" below) pays a real, measurable throughput cost
for a collection that finds almost no garbage, and 2 MiB
keeps that cost in the noise while still keeping a genuinely leaky
workload's resident memory a small fraction of `maxMemoryBytes`'s 64 MiB
default. A `Heap::enable_collect_stress_mode` test mode ignores the
threshold and collects at every safepoint; `hollowscript::Options::gc_stress_mode`
exposes it to a whole run (the load included), and
`scripts/check-gc-stress` runs the conformance corpus and the
specification's examples under it (see below).

*A collector called mid-run needed a second look at its own constant
factor.* `Heap::collect`'s mark set used to be a `BTreeSet<u32>`, fine when
a collection only ran once or twice per call. Once safepoints call it
dozens or hundreds of times per call, marking a mostly-live heap with tree
inserts every time became visible in the numbers below (`closure-heavy`
regressed 13x on the first pass); it is now a `Vec<bool>` the width of the
arena's own slot count (`Arena::slot_count`, new), a flat write per mark
with no allocation, and the live-bytes-for-the-next-threshold sum is now
folded into the mark walk itself rather than a second pass over the
survivors after the sweep.

**Proof of safety.** `hollowscript-vm`'s own unit tests
(`interp.rs::discarding_loop_module` and the tests around it) force a
collection at every safepoint of a hand-built loop that repeats exactly
this shape and check the result is unchanged and that the discarded lists
do not pile up; `crates/hollowscript/tests/fuzz_pipeline.rs` gained a
second property test that drives every mutated corpus program twice, once
under the ordinary threshold policy and once under stress mode, and
compares a full signature of every call outcome and every read between the
two; `scripts/check-gc-stress` runs the conformance corpus (272 cases) and
the specification's examples (99) under `HOLLOWSCRIPT_GC_STRESS=1` and
reports the identical 244/272 and 93/99 pass counts (against the same
baseline files) that an ordinary run reports; and the `pipeline` fuzz
target accepts the same environment variable for a coverage-guided run
under stress mode alongside its ordinary one. Section 11.4's allocation
counter and fault ordering are unaffected by any of this: a collection
never touches a `RunCounters` and cannot itself fault, by construction (it
runs at a point with no charge pending and nothing left to decide).

**Measured: before and after**, `/usr/bin/time -l` on the `min-size`
profile CLI, this document's own `list-sort` and `closure-heavy` sources,
plus a third program in the same shape as this section's own worked
example (`variable xs: [Int] = []` then `while i < n { xs = xs.append(i); i
= i + 1 }`, `docs/design/security.md`'s illustration of the amplification
this fix closes):

| program | before | after | factor |
|---|---|---|---|
| `list-sort` (5,000 appends, then a sort) | 347.8 MB | 29.8 MB | 11.7x |
| `closure-heavy` (4,000 appends of a closure) | 231.6 MB | 41.1 MB | 5.6x |
| `xs = xs.append(i)`, n = 20,000 | 5,019.6 MB | 50.6 MB | 99.2x |

(`n = 20,000` rather than `closure-heavy`'s and `list-sort`'s own 4,000 and
5,000 to keep the "before" run, which never collects mid-call, inside a
few seconds; `list.append`'s own O(n) cost per call makes n sequential
appends O(n²) regardless of this fix, so a much larger n was not run
against the unfixed baseline at all, and was not needed to make the point.)

**Measured: throughput**, `cargo xtask bench`, before and after, min-size
profile, the widest deltas from the eight representative programs (every
other program moved by 5% or less, within this harness's own run-to-run
noise):

| program | before (ops/sec) | after (ops/sec) | change |
|---|---|---|---|
| `closure-heavy` | 61,474 | 51,080 | -17% |
| `list-sort` | 42,666 | 45,049 | +6% (noise) |
| `json-roundtrip` | 15,129 | 18,913 | +25% (noise) |
| `record-field-access` | 96,996 | 94,492 | -3% |

`closure-heavy`'s -17% is the one program of the eight that is mostly
*live* data (4,000 closures the final `reduce` needs all of), so it pays
for collections that find little to reclaim; every other program's
movement here is within the harness's own sample-to-sample noise (compare
`json-roundtrip`'s and `list-sort`'s own "improvement", which is not a
real one). This is the throughput side of the 2 MiB threshold decision
above: a smaller threshold measured a much larger regression here
(13x on `closure-heavy` before the threshold was widened from an initial
256 KiB), for memory numbers that were already comfortably under
`maxMemoryBytes` at 2 MiB.

### Measured: appending `String`s with copy-on-write

Everything above this point measures a list of `Int`s: the cheapest
possible element, with no `String` node cost of its own to add on top of
`Value`'s 24 bytes. `docs/design/security.md`'s "what it deliberately does
not promise" used to generalise the resulting 3.0x (`range(0, 8000000)`)
and roughly-3x (`list-sort`, after the mid-run collection fix above) into
a single floor for every shape; two further measurements show that a
`String` built inside the loop rather than reused as a constant does not
follow that floor. Read with `cargo xtask mem` (`xtask/src/mem.rs`,
committed, in place of the scratch, uncommitted harness
(`crates/hollowscript/examples/mem_probe.rs`) an earlier measurement here
used), which runs `crates/hollowscript/examples/mem_shape.rs` under
`/usr/bin/time -l` on macOS and reads "peak memory footprint" against a
charged figure it computes independently from section 11.4's own formula:

| program | count | logical (section 11.4) | real (peak) | factor |
|---|---:|---:|---:|---:|
| a function appending an interpolated empty `String` to a list | 100,000 | 2.4 MB | 30.5 MB | 12.7x |
| the same, appending an interpolated 1,000-character `String` | 100,000 | 102.4 MB | 131.1 MB | 1.3x |

The new figures are from 2026-10-06 after `list.append` began reusing a
receiver only when the VM can prove the old value is not observable. The
100,000-append empty-string case fell from 238.0 MB to 30.5 MB; the
1,000-character case fell from 1,581.2 MB to 131.1 MB. The small-string
case still has a 12.7x peak-to-logical ratio because the logical charge does
not account for this VM's per-object, arena-slot and allocator overhead.
That residual is not quadratic discarded-list storage. A host running
untrusted code still needs an OS or container memory cap for a hard RSS
bound.

(Interpolation, `"${""}"` and `"${"..."}"`, rather than a bare literal,
because a literal with no interpolation is not itself charged, section
11.4, and this implementation may reuse the same constant `String` for
every append of one; interpolating forces a fresh value each time, which
is what a real program building strings at runtime, rather than repeating
one fixed literal, actually does.)

The empty-string shape remains above the list-of-scalars factor because
each empty `String` still occupies a heap object and arena slot that the
logical charge does not model. Its peak is now linear in the number of
appends for the unaliased rebind pattern; the remaining factor is a
representation overhead rather than a discarded full-list copy.

**Earlier copy and collector fixes (historical measurements).** Before the
copy-on-write change measured above, `list.append` (and `Map.with`, `Set.with`) cloned their receiver at
its exact current length (`to_vec`) and then called `push` (or `insert`)
to add the one new element or entry, which, finding no spare capacity,
grew the copy again: every element was copied twice, not once, on every
call. Sizing the copy for its final length up front (`natives.rs`'s own
comment on `list.append`) halves that historical copy: the same
empty-`String` shape measured 274.0 MB (114x) before the exact-size fix,
at the same 2.4 MB charge, and the loop itself ran in roughly half the wall
time (measured with
`/usr/bin/time`'s "real" figure around a full CLI `run`: about 15 s before,
about 8 s after, for this shape at 100,000 appends). The current VM also
appends in place when it proves the receiver has no other live alias and
the result immediately replaces that binding. When another register, heap
object, global, host root or pending host command can still observe the old
value, the VM copies it. The old-value path remains O(N), as immutability
requires; the common unaliased `xs = xs.append(value)` loop now grows
geometrically and amortizes to O(1) per append.

**Fixed in part: the collector's own trigger was counting the wrong bytes.**
`Heap::due_for_collection` (`hollowscript-vm/src/heap.rs`) used to compare
bytes allocated since the last collection against its threshold using
section 11.4's own node-cost formula, the same figure a run's own counters
charge: 8 bytes for a list element, matching the specification's own
`Value`. What `list.append` actually allocates for that element is a
`Value` in this implementation's own representation, 24 bytes (the
tagged-union size `docs/design/security.md`'s "a list of scalars" already
explains), so a threshold sized in node-cost bytes let roughly three times
more real garbage than intended sit between collections before the trigger
noticed. `Heap::object_real_bytes` (this module's own comment explains it
fully) is a second, separate estimate, used only to decide *when* to
collect: `std::mem::size_of` of the object's own Rust representation plus
its buffer's *capacity* (not length, since capacity is what the host's
allocator actually reserved) times the size of one element. Section 11.4's
own node cost, and every counter and fault it feeds, is completely
unchanged: `Heap::alloc` and `Heap::collect` are the only two places that
read the new estimate, and neither is reachable from anything a program can
observe. `scripts/check-gc-stress` (which forces a collection at every
safepoint regardless of any threshold) still passes: this changes when
`Heap::collect` runs, never whether it is safe to call.

Re-measured with `cargo xtask mem`, before and after, on the same machine
and immediately back to back (this machine's own absolute figures run
noticeably lower than this document's other tables, which were taken on a
different one; the ratios are what to compare):

| program | count | logical | real, before | real, after | ratio, before | ratio, after |
|---|---:|---:|---:|---:|---:|---:|
| empty-`String` appends | 100,000 | 2.4 MB | 254 to 273 MB | 241 to 284 MB | 106x to 114x | 101x to 114x |
| 1,000-character `String` appends | 100,000 | 102.4 MB | 1,648 to 1,998 MB | 683 to 769 MB | 16x to 20x | 6.6x to 6.7x |
| `range(0, 8,000,000)`, retained | 8,000,000 | 64.0 MB | 257.2 MB | 257.2 MB | 4.0x | 4.0x |

(A range, not a single figure, for the first two: this machine's own
run-to-run variance turned out to be wide enough, 5 to 10%, that a single
sample either side of the fix would overstate how precise either number
is; each range is three to five runs.)

The 1,000-character shape is a real, repeatable improvement, well outside
that noise band: the `String` itself, at 1,016 bytes real per element,
dominates both the node-cost and the real-bytes formula for this shape, so
correcting the *list's* own 8-versus-24-byte gap collects the growing list
of them substantially more often, and the loop runs faster too (`/usr/bin/time`'s
"real": about 8.0 s before, about 5.0 s after, for a full run of this
shape). The empty-`String` shape, whose element is nearly all list
overhead and almost no string content, did not move outside its own noise
band, and running the same before/after comparison at every threshold this
change was tried at (down to the unmodified 2 MiB floor) found the same
result: it costs roughly 20 to 25% more wall time (about 7.5 s before, about
9.0 to 9.3 s after) for no measurable memory improvement. `docs/design/
security.md`'s own section explains why: this shape's real cost is
dominated by `list.append`'s O(N) copy itself (below), which a more
attentive trigger cannot shrink, only collect quickly once it exists, and
collecting a value already close to `maxMemoryBytes` in real size quickly
and often is itself not free.

**The threshold floor had to move, to keep the fix from costing
`closure-heavy` its own throughput budget.** `MIN_COLLECT_THRESHOLD_BYTES`
was 2 MiB in node-cost units; left at 2 MiB but now compared against real
bytes, which run several times larger for the same structure,
`closure-heavy` (four thousand closures, kept almost entirely live, so a
collection here finds almost nothing to free) collected roughly three
times as often for no more live data and measured a 48% fall in
throughput, well past this document's own 50%-of-baseline budget. Raising
the floor to 4 MiB restores comfortable headroom (roughly 70 to 90% of this
same machine's own before-the-fix figure, run to run, against a 50% floor)
at the cost of a further, smaller real-memory regression for the
already-unimproved empty-`String` shape above (that shape's own appends
never allocate past 4 MiB of real bytes before the loop ends, so raising
the floor delays its very first collection for longer, and the range
recorded in the table above is already after this floor was raised). 6
MiB, also measured, recovered `closure-heavy` closer to its own baseline
(roughly 85 to 90%) at a further, larger cost to the same shape (around 116x
rather than 101x to 114x); 4 MiB was kept as the more conservative choice,
since it already clears the budget with headroom and the memory cost of
going further did not look proportionate to the throughput bought.
`COLLECT_GROWTH_FACTOR` (the 2x-live-bytes-after-a-collection rule) was
left unchanged: lowering it, tried during this investigation, moved this
shape's own numbers by less than run-to-run noise while adding real
collector overhead, so it was not worth the cost either.

| program | before (ops/sec) | after, 4 MiB floor (ops/sec) | budget (50% of baseline) |
|---|---:|---:|---:|
| `closure-heavy` | 48,314 | 34,385 to 36,810 | 24,157 |
| `record-field-access` | 89,020 | 70,550 to 83,504 | 44,510 |
| `json-roundtrip` | 19,375 | 16,222 to 19,585 | 9,688 |
| `list-sort` | 38,211 | 35,842 to 37,852 | 19,106 |

Every other one of the eight representative programs moved by single
digits either way, within this machine's own noise, both before and after.

**Regression test.** `hollowscript-vm/src/heap.rs`'s own unit tests gained
`the_trigger_notices_a_loop_of_appends_real_bytes_not_only_their_node_cost`,
which simulates `list.append`'s own allocation shape directly through
`Heap::alloc` (no parser, compiler or interpreter needed) and asserts that
`Heap::due_for_collection` answers `true` well before the append count
node cost alone would produce; it fails, at the exact append count the
node-cost formula predicts, if `Heap::alloc` or `Heap::collect` is changed
back to feeding the trigger `object_node_cost`.

**Not fixed: the O(N) copy itself.** Eliminating it needs either mutating
the receiver in place once nothing else can still observe it (safe only
if the implementation can tell a value is not shared, which needs
reference counting or an equivalent compile-time uniqueness proof; this
implementation has neither, and `heap.rs`'s own module doc explains why
reference counting was deliberately not chosen for this heap) or a
persistent, structurally shared list representation (assessed and
declined once already, for the mid-run collection problem above, as "a
much larger redesign"). Both are substantially larger pieces of work than
this document's other fixes, carry real correctness risk for a
security-critical interpreter if a uniqueness proof is ever wrong, and
were not attempted here; `docs/design/security.md`'s "what it deliberately
does not promise" section explains why raising section 11.4's own byte
charge for `append` is not a sound substitute either. `docs/design/
security.md`'s own section now records the current range rather than one
floor.

## Budgets

Proposed from the numbers above, with headroom rather than pinned to
today's exact figures, since "a change that halves throughput is caught"
(the previous implementation's own regression bar) is the right kind of
tolerance for a number that varies with the machine it runs on:

| Metric | Budget |
|---|---|
| `hollowscript-cli` binary size (`min-size`) | 4 MB |
| `hollowscript-wasm` size (`min-size`) | 2 MB |
| Cold start (`hollowscript-cli run`, trivial program) | 50 ms |
| Throughput, any representative program above | no less than 50% of this document's own recorded figure |
| Real memory, for a retained structure with no large intra-call garbage (the `range(0, 8000000)` shape above) | no more than 4.5x the value's own section 11.4 accounting |
| Real memory, for a loop that builds and discards through `maxMemoryBytes`-worth of intermediate generations in one call (the `list-sort`/`closure-heavy` shape above) | no more than `maxMemoryBytes` itself |

`scripts/check` does not enforce these yet: binary size is fast and stable
enough to gate on directly (build once, read a file's length), but the
throughput floor needs `cargo xtask bench`'s full run, which takes tens of
seconds and rebuilds two release profiles, too slow for a check that runs
on every change. `scripts/bench-budget` is a separate script for the
binary-size half of this table; the throughput half stays a number a human
reads from `cargo xtask bench`'s own output until there is a machine
stable enough, and a reason (CI, see
[release-and-milestones.md](release-and-milestones.md)), to gate on it
automatically.

## A caveat on the numbers

Wall-clock measurements on whatever machine ran them, on one run, not a
controlled benchmarking environment: not a claim that the absolute figures
mean anything in isolation, only that they are real rather than assumed,
and that the harness which produced them (`cargo xtask bench`) is
committed and can be rerun by anyone who wants a number for their own
machine, or a second run to see how much these move.
