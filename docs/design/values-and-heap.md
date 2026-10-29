# Values and the heap

## The recommendation

The heap is a precise mark and sweep collector over an index-based arena with
generational handles, and it contains no unsafe code. This is what
`hollowscript-vm::arena` and `hollowscript-vm::heap` implement, and it is a
recommendation this document argues for, not the only design that could have
satisfied the specification: reference counting was the main alternative
considered, and section 5.4's capture semantics are exactly why it was rejected.

### Why not reference counting

Every value in the language is immutable, but a *binding* is not (section
4.3): a `variable` a closure captures can be reassigned after the closure is
created, and section 5.4 states plainly that "a binding lives as long as
anything can still reach it." A closure captures a *cell* (the mutable
storage of one binding), and two closures can hold cells that reach back into
structures containing each other: a cell whose value is a closure that
captures a cell whose value is a list that contains the first closure. This
is an ordinary, legal program (a pair of closures that reference each other
through captured state), and reference counting cannot reclaim a cycle like
it without a separate cycle collector bolted on, which reintroduces exactly
the tracing machinery a mark and sweep design has from the start. A tracing
collector reclaims the cycle for free, which
`heap::tests::collect_frees_unreachable_objects_including_a_cycle` in
`hollowscript-vm` demonstrates directly.

### Why an index-based arena with generational handles, and no unsafe code

A `Handle` (`hollowscript_vm::arena::Handle`) is a slot index plus the
generation the slot had when the value was allocated into it. Freeing a slot
bumps its generation and returns the index to a free list for reuse; reading
through a stale handle compares generations and returns `None` rather than
whatever was allocated into the reused slot. This is the same guarantee a raw
pointer with manual lifetime tracking would need `unsafe` to provide (the
classic use-after-free), given here by an ordinary bounds check and an
integer comparison in safe Rust: `Arena<T>` has no `unsafe` block anywhere in
it, and neither does anything built on it. A garbage collector is exactly the
part of a language runtime most often written in `unsafe` Rust for speed;
this one is not, because nothing measured yet says it has to be, and every
crate that touches untrusted script state benefits from staying inside the
borrow checker's guarantees.

## The value representation

`hollowscript_vm::value::Value` is `Copy`: an `Int`, `Float`, `Bool` or
`none` held inline, or a `Handle` into the heap for everything else
(`String`, `Filter`, list, map, set, record, enum value, closure, or cell).
Copying a `Value` copies at most a tag and a handle, which is the concrete
form of section 6's "aliasing is therefore unobservable, and implementations
may share or copy values freely": there is nothing to deep-copy, because
every heap kind is reached through a handle, and two `Value`s that name the
same handle already observe the same immutable data.

`hollowscript_vm::value::Object` is the single enum stored in the one arena
that makes up the heap (`Heap` wraps one `Arena<Object>`, not one arena per
kind): a string's bytes, a list's elements, a map's entries, a set's
elements, a record's fields alongside the field-name shape it was built
with, an enum value's variant index and payload, a closure's function index
and captured cells, and a cell's current value. One arena rather than several
was chosen so that a single sweep pass, and a single generation counter
scheme, covers every kind of heap value; nothing about the design depends on it.

### Map and set keys compare by content, not by handle

A map key or set element restricted to `Int`, `String` or `Bool` (section
2.8) is `hollowscript_vm::value::MapKey`. A `Str` key is a handle to a
string, and two separately built strings with the same content must compare
equal (section 3.5: "the same sequence of code points"), which a derived
`Eq` on the handle would get wrong, since two different allocations get
different handles. `MapKey` therefore has no derived `Eq` or `Ord` at all;
`Heap::keys_equal` and `Heap::compare_keys` are the only way to compare two
keys, and they read the string's actual bytes out of the heap for a `Str`
key, so the comparison is always by content. This is checked directly by
`heap::tests::keys_compare_by_content_not_by_handle`, which builds two equal
strings on purpose and confirms they get different handles but compare
equal as keys.

## Byte accounting (chapter 11)

Section 11.4 defines two quantities purely as formulas over a value's shape,
specifically so that two implementations that store values completely
differently still agree on when a program has used too much memory.

**Node cost** (`Heap::node_cost`) matches the specification's table exactly:
a scalar costs nothing, because it is held inline and never occupies a heap
slot; a string or filter costs 16 bytes plus its UTF-8 length; a list costs
16 plus 8 bytes per element; a map costs 32 plus 24 bytes per entry; a set
costs 32 plus 16 bytes per element; a record costs 16 plus 8 bytes per field;
an enum value with no payload costs nothing, and one with a payload costs 16
plus 8 bytes per payload value; a closure costs 32 plus 8 bytes per captured
binding.

**Tree size** (`Heap::tree_size`) is a value's own node cost plus the tree
sizes of everything it directly contains, with sharing ignored (a value
reachable twice is counted twice, matching the specification's own warning
that this is what lets a program build an astronomically large tree size in
a few statements by sharing). It is computed with an explicit stack, not
recursion, specifically because section 11.4 requires this: "a program can
build a value as deeply nested as the memory limit allows... every operation
of the language and the library MUST work for any depth that fits in
`maxMemoryBytes` without exhausting the host's stack." A recursive
implementation would violate this the moment a script built a list nested a
few hundred thousand deep, which the default 64 MiB memory limit permits;
`heap::tests::tree_size_sums_node_costs_without_recursion` builds exactly
such a structure (200,000 levels) and checks it does not overflow the host's
call stack. Every future traversal of a value (equality, the retained-size
walk, releasing a value) must keep this same property, and the collector's
own mark phase already does, using its own explicit work list rather than recursion.

**The allocation counter and the retained size** are per-run and per-instance
respectively (section 11.4); this crate provides the primitives
(`node_cost`, `tree_size`) that a finished interpreter charges against
`hollowscript_vm::limits::RunCounters::charge_memory` for the former, and
`hollowscript_vm::limits::check_retained_size` implements the retained size
formula directly: the tree sizes of every top-level binding, plus, for every
binding a reachable closure captured, 16 bytes plus the tree size of its
current value, with each captured binding counted once however many closures
share it (a `BTreeSet` of already-counted bindings enforces this, tested by
`limits::tests::retained_size_counts_a_shared_capture_once`).

**String and filter constants** are the one allocation section 11.4
deliberately leaves uncharged ("string literals with no interpolation and
filter literals that appear in the source are not charged"). Leaving them
uncharged is only safe if loading one costs no new real memory either, so
`LoadK` does not copy a `String` or `Filter` constant onto the heap each time
it runs: `Interpreter::constant_object` allocates one object per distinct
constant the first time it is loaded and hands out that same immutable
object every time after, and every such object is a root of every
collection. Real memory spent on constants is therefore bounded by the size
of the loaded module, however often a loop loads one. The compiler lowers a
literal with no interpolation to an `Interp` of that one constant, which
`Interpreter::interpolate` returns unchanged and uncharged; any `${}` makes
at least three parts and takes the charged path. An earlier version copied
the constant on every `LoadK` uncharged (unbounded real memory), and a later
one charged each copy (so `print("spam")` in a long loop faulted HS1002,
which the specification does not allow); sharing fixes both.
`crates/hollowscript-vm/tests/review_verifier.rs`'s
`loadk_shares_one_uncharged_object_per_constant` and the corpus cases
`limits/string-literal-in-a-loop-is-not-charged`,
`limits/printing-a-literal-in-a-loop-is-not-charged` and
`limits/interpolated-string-in-a-loop-is-charged` pin both halves.

## When a collection happens, and what roots it

`Heap::collect` is a single mark-and-sweep pass given a set of roots, and it
follows exactly the reachability section 11.4 defines for the retained-size
walk: every top-level binding's current value, and every cell a reachable
closure captured. Because the two use one definition of reachable, the
collector can never free something the retained-size check still counts, and
vice versa.

A collection happens between runs (at the end of a load and at the end of
every call of an exported function) and, since `docs/design/performance.md`'s
"mid-run collection" fix, at a safepoint inside a run too, not only between
them. Neither changes what a program observes: section 11.4's own words are
"a garbage collector never changes when a limit is reached", and the
counters (`RunCounters`) come from the size formulas alone and are never
touched by `Heap::collect`.

Between runs, the roots are every top-level binding and every cell a
reachable closure captured, plus whatever the host still holds
(`Instance::host_roots`, mirrored into `Interpreter::host_roots` so a
mid-run collection can see it too; see below). Mid-run, `Interpreter::
run_frames`'s frame loop adds one more thing: every value the run currently
in progress is holding. This is smaller than it sounds, because of how the
frame loop already had to be built for an unrelated reason (section 11.5's
call-depth guard, "The call stack is the interpreter's own"): every active
call's registers live in a `Vec<Frame>` the interpreter owns, not on the
host's own Rust stack, and the only place in the whole loop where a
particular frame's registers are briefly *not* in that `Vec` (moved out to a
local for the one instruction currently using them) is inside the match
over one decoded instruction. Running the safepoint check at the very top of
the loop, before the next instruction is even decoded, means every frame's
registers, plus the cells its closure captured (read directly by
`GetUpval`/`SetUpval`, which do not have to pass through a register) and a
library function's own job state (`NativeJob::push_roots`: the elements, the
values kept so far, the reduce accumulator, an in-progress sort's own merge
state) are always exactly where `push_frame_roots` expects them, with
nothing left transiently reachable only from a Rust-stack local. A suspended
run (`docs/design/capabilities.md`) is rooted the same way, from
`Interpreter::suspended`, which is why `Interpreter::collect` is now safe to
call while a run is paused too, even though nothing calls it there
automatically yet (a capability call can be pending for an amount of host
time this crate has no way to bound, and collecting on every settle would
make whichever call happens to be in progress when the host finally responds
pay for garbage an earlier call produced).

*Trigger.* Collecting on every safepoint regardless of whether there is
anything worth collecting would itself cost real time for real memory
back, so a threshold decides. `Heap` tracks bytes of node cost allocated
since its last collection and compares that to a threshold that starts at
2 MiB and, after each collection, becomes twice the live bytes that
collection found (floored at 2 MiB): a collection's own cost stays
amortised against bytes allocated (the "cost stays amortised" of section
11.7) rather than fixed per safepoint. `Heap::enable_collect_stress_mode`
overrides this and collects at every safepoint regardless, for the test
mode `docs/design/performance.md`'s "Proof of safety" describes.

A single run's heap growth was already bounded even before this fix,
because every allocation is charged against `maxMemoryBytes` and the run
faults when it passes it: what was unbounded was real, physical memory
*within* that bound, not the language's own counters (`docs/design/
performance.md` and `docs/design/security.md` describe the gap this closed,
and by how much, in full).

**The host's own values are roots.** A `Value` of a container kind is a
handle into one instance's heap, and a host may hold one: the load result,
what `read` answered, what a call returned, or one it built with
`heap_string` to pass as an argument. The interpreter cannot know which of
those the host still has, so `Instance` remembers every one it hands out and
roots them all. A host that keeps none, one that renders each result to text
as it gets it, as the C ABI does, calls `Instance::release_values` and the
next collection frees them too. Section 11.4 settles what this does to the
counters: "a garbage collector never changes when a limit is reached". The
counters come from the size formulas and know nothing about collection, so
this is a question of the host's real memory only.
