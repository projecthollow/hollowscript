//! The heap: a precise mark and sweep collector over the index-based arena of
//! [`crate::arena`], with generational handles and no unsafe code.
//!
//! **Why mark and sweep, not reference counting.** Every value in the
//! language is immutable, but a *binding* is not: a closure captures a
//! [`Object::Cell`] by handle, and two closures can hold cells that point back
//! into structures containing each other (`a`'s cell captures a closure that
//! captures a cell holding a list that contains `a`'s own closure). Reference
//! counting leaks a cycle like that; a tracing collector does not, and no
//! unsafe code is needed to break a cycle by hand.
//!
//! **Byte accounting.** [`Heap::node_cost`] and [`Heap::tree_size`] implement
//! the formulas of section 11.4 exactly, node cost by matching on the kind of
//! [`Object`] and tree size by summing node costs over what a value directly
//! contains, iteratively rather than by recursion so that a value nested as
//! deep as the memory limit allows cannot exhaust the host's stack (section
//! 11.4, "deep values"), with the running total saturating at [`u64::MAX`] as
//! the specification requires.
//!
//! **Collection.** [`Heap::collect`] traces from a set of roots (the
//! top-level bindings and the cells that reachable closures captured, per
//! section 11.4's definition of what is reachable) with an explicit work
//! list, marks every handle it finds, then sweeps every arena, freeing
//! anything unmarked. It is precise: only handles the traced objects actually
//! contain are visited, never a conservative scan of memory that safe Rust
//! could not do anyway.
//!
//! **Mid-run collection.** Nothing here decides *when* to collect: that is
//! [`Interpreter::run_frames`](crate::interp::Interpreter), which calls
//! [`Heap::due_for_collection`] at a safepoint and, if it answers `true`,
//! gathers every value currently live (every frame's registers and
//! captures, library-function job state, the globals and the host's own
//! roots; see `docs/design/performance.md`, "mid-run collection") and passes
//! them to [`Heap::collect`]. This module only tracks the trigger: bytes
//! allocated since the last collection, against a threshold that grows
//! geometrically each time a collection runs, so that the amortised cost of
//! collecting stays proportional to the bytes allocated (section 11.7's
//! "cost stays amortised" requirement) rather than to the heap's size.
//!
//! **The trigger is fed real bytes, not the specification's node cost.**
//! `Heap::object_real_bytes` is a separate estimate from
//! `Heap::object_node_cost`, used only by [`Heap::alloc`] and
//! [`Heap::collect`] to decide *when* to run, never by anything a program
//! can observe (see that function's own comment for why the two must
//! diverge and `docs/design/security.md`, "what it deliberately does not
//! promise", for the loop-of-appends shape this closes most of the gap on).

use crate::arena::{Arena, ArenaExhausted, Handle};
use crate::value::{MapEntry, MapKey, Object, Value};
use hollowscript_bytecode::module::StringId;
use std::cmp::Ordering;
use std::collections::BTreeSet;

/// The smallest a collection's own trigger threshold ever shrinks to
/// (bytes of [`Heap::object_real_bytes`]'s estimate, not section 11.4's
/// node cost, and so not necessarily what the host's own allocator would
/// report either): a heap that has just been collected down to almost
/// nothing still waits for this many bytes of fresh allocation before
/// trying again, so a program that allocates and immediately frees a
/// little does not collect on every safepoint.
///
/// 4 MiB, not the much smaller value a "collect as soon as there is
/// anything worth collecting" instinct suggests, because `cargo xtask
/// bench`'s `closure-heavy` and `record-field-access` programs (which keep
/// almost everything they allocate alive in a growing list, so a collection
/// finds hardly any garbage) measured a large fall in throughput at a
/// smaller threshold: most of a collection's own cost is marking the live
/// set it has to re-confirm every time, so collecting far more often than a
/// program's actual garbage-to-live ratio calls for pays a real,
/// measurable cost for very little memory in return. This was 2 MiB before
/// this heap started feeding the trigger real bytes instead of node cost
/// (see this module's own doc): real bytes runs a few times larger than
/// node cost for the same structure (`object_real_bytes`'s own comment
/// explains why), so the old 2 MiB figure, unchanged, made `closure-heavy`
/// collect roughly three times as often for no more live data, which
/// `docs/design/performance.md`'s own re-measurement after this change
/// found costs about half its throughput. Raising the floor recovers it: 4
/// MiB (measured, not chosen to land on a round old-to-new ratio) leaves
/// `closure-heavy` with comfortable headroom over the budget that document
/// sets, at the cost of somewhat more real memory for a small-element loop
/// of appends that never gets past this floor before it finishes (that
/// document's own memory table records the size of that trade-off, made
/// deliberately rather than chased away by shrinking the floor further,
/// since a smaller floor was measured not to help that shape much in
/// return for the throughput it costs). A mostly-live workload's
/// collections stay rare enough that this cost disappears into the noise,
/// while a genuinely leaky workload's resident memory stays a small
/// fraction of `maxMemoryBytes`'s 64 MiB default rather than a multiple of
/// it.
const MIN_COLLECT_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024;

/// How much the trigger threshold grows, relative to the live bytes a
/// collection just found, each time [`Heap::collect`] runs: the classic
/// doubling policy of a generational or semispace collector, chosen so that
/// a long-lived program's collections become rarer as its steady-state
/// working set grows, keeping the total collector time a small, bounded
/// fraction of total allocation rather than a fixed per-byte cost.
const COLLECT_GROWTH_FACTOR: u64 = 2;

/// The heap of one instance: one arena holding every heap-allocated value.
#[derive(Debug)]
pub struct Heap {
    objects: Arena<Object>,
    /// Lists that are reachable from another heap object or have crossed the
    /// VM/host boundary. `list.append` may grow an unshared list in place,
    /// but must preserve the old value once another object or the host can
    /// still observe it.
    shared_lists: BTreeSet<Handle>,
    /// Real bytes ([`Heap::object_real_bytes`]'s estimate, not section
    /// 11.4's node cost) allocated since the last collection (or since the
    /// heap was created, if none has run yet). Compared against
    /// `collect_threshold` by [`Heap::due_for_collection`]; never read by
    /// anything that affects an observable result of a program, so it plays
    /// no part in section 11.4's own byte counters, which are charged
    /// separately, from [`Heap::node_cost`], at every allocation site the
    /// specification lists (`Interpreter::alloc_charged`).
    bytes_since_collect: u64,
    /// The value `bytes_since_collect` must reach before
    /// [`Heap::due_for_collection`] answers `true`. Recomputed after every
    /// collection from the live bytes that collection found, growing
    /// geometrically (see [`COLLECT_GROWTH_FACTOR`]).
    collect_threshold: u64,
    /// Set by [`Heap::enable_collect_stress_mode`]: makes
    /// [`Heap::due_for_collection`] always answer `true`, so that a test
    /// harness can force a collection at every safepoint a run passes
    /// through (`docs/design/performance.md`, "stress mode").
    stress: bool,
}

impl Default for Heap {
    fn default() -> Heap {
        Heap {
            objects: Arena::default(),
            shared_lists: BTreeSet::new(),
            bytes_since_collect: 0,
            collect_threshold: MIN_COLLECT_THRESHOLD_BYTES,
            stress: false,
        }
    }
}

impl Heap {
    /// An empty heap.
    #[must_use]
    pub fn new() -> Heap {
        Heap::default()
    }

    /// Allocates an object and returns a value referring to it.
    ///
    /// # Errors
    ///
    /// [`ArenaExhausted`] in the (practically unreachable) case that this
    /// heap's historical object count has already reached [`u32::MAX`], which
    /// the interpreter turns into the internal-error fault rather than let it
    /// happen silently.
    pub fn alloc(&mut self, object: Object) -> Result<Value, ArenaExhausted> {
        // The collector's own trigger is fed `object_real_bytes`, not the
        // specification's node cost: see that function's own comment and
        // this module's doc for why the two must diverge. Nothing else
        // here changes: the object itself, and what a caller can later
        // charge against section 11.4's counters through `Heap::node_cost`,
        // are exactly as before.
        let real = Self::object_real_bytes(&object);
        self.mark_object_children_shared(&object);
        let handle = self.objects.alloc(object)?;
        self.bytes_since_collect = self.bytes_since_collect.saturating_add(real);
        Ok(Value::Heap(handle))
    }

    /// Whether a safepoint should collect: the heap has grown by at least
    /// `collect_threshold` bytes of node cost since the last collection, or
    /// [`Heap::enable_collect_stress_mode`] was called.
    #[must_use]
    pub fn due_for_collection(&self) -> bool {
        self.stress || self.bytes_since_collect >= self.collect_threshold
    }

    /// Forces every future call of [`Heap::due_for_collection`] to answer
    /// `true`, for the stress-test mode of `docs/design/performance.md`
    /// that proves a collection at every safepoint never changes a
    /// program's observable result. Not exposed to a host: only test code
    /// within this workspace calls it.
    pub fn enable_collect_stress_mode(&mut self) {
        self.stress = true;
    }

    /// The object of a handle, if it is still live.
    #[must_use]
    pub fn get(&self, handle: Handle) -> Option<&Object> {
        self.objects.get(handle)
    }

    /// Records that a value may be observed outside the current VM register
    /// holding it. Such a list must retain copy-on-append semantics.
    pub(crate) fn mark_escaped(&mut self, value: Value) {
        self.mark_list_shared(value);
    }

    /// Whether the list can be extended without preserving a separately
    /// reachable old value. The interpreter also checks live register roots.
    pub(crate) fn can_append_in_place(&self, handle: Handle) -> bool {
        matches!(self.get(handle), Some(Object::List(_))) && !self.shared_lists.contains(&handle)
    }

    /// Marks list children of an object before it is installed in the heap.
    fn mark_object_children_shared(&mut self, object: &Object) {
        match object {
            Object::List(items) | Object::Record { fields: items, .. } => {
                for &value in items {
                    self.mark_list_shared(value);
                }
            }
            Object::Map(entries) => {
                for entry in entries {
                    self.mark_list_shared(entry.value);
                }
            }
            Object::Enum { payload, .. } => {
                for &value in payload {
                    self.mark_list_shared(value);
                }
            }
            Object::Cell(value) => self.mark_list_shared(*value),
            Object::Str(_)
            | Object::Filter(_)
            | Object::Set(_)
            | Object::Closure { .. }
            | Object::Builtin(_) => {}
        }
    }

    fn mark_list_shared(&mut self, value: Value) {
        let Value::Heap(handle) = value else { return };
        if matches!(self.get(handle), Some(Object::List(_))) {
            self.shared_lists.insert(handle);
        }
    }

    /// The number of live objects.
    #[must_use]
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Whether the heap holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// The `String` content of a key, empty if the handle is stale or not a string.
    fn key_text(&self, handle: Handle) -> &str {
        match self.objects.get(handle) {
            Some(Object::Str(s)) => s.as_str(),
            _ => "",
        }
    }

    /// Whether two keys are equal: by value for `Bool` and `Int`, by content
    /// for `Str` (section 3.5: strings are equal when their code points are).
    #[must_use]
    pub fn keys_equal(&self, a: MapKey, b: MapKey) -> bool {
        self.compare_keys(a, b) == Ordering::Equal
    }

    /// Compares two keys in the ascending order of section 6.2: `Bool` before
    /// `Int` before `String`; within a kind, `false` before `true`, numeric
    /// order for `Int`, and code point order for `String`.
    #[must_use]
    pub fn compare_keys(&self, a: MapKey, b: MapKey) -> Ordering {
        let rank = |k: MapKey| match k {
            MapKey::Bool(_) => 0,
            MapKey::Int(_) => 1,
            MapKey::Str(_) => 2,
        };
        match (a, b) {
            (MapKey::Bool(x), MapKey::Bool(y)) => x.cmp(&y),
            (MapKey::Int(x), MapKey::Int(y)) => x.cmp(&y),
            (MapKey::Str(x), MapKey::Str(y)) => self.key_text(x).cmp(self.key_text(y)),
            _ => rank(a).cmp(&rank(b)),
        }
    }

    /// Overwrites the value held by the cell at `handle`, the mutation a
    /// captured `variable` binding needs (`Op::SetCell`, `Op::SetUpval`).
    ///
    /// Returns `false` and changes nothing if `handle` is stale or does not
    /// refer to an [`Object::Cell`].
    pub fn set_cell(&mut self, handle: Handle, value: Value) -> bool {
        self.mark_list_shared(value);
        match self.objects.get_mut(handle) {
            Some(Object::Cell(slot)) => {
                *slot = value;
                true
            }
            _ => false,
        }
    }

    /// Extends the list at `handle` in place by `items`, the one mutation of
    /// a list there is (`Op::ListAppend`, which builds a long list literal
    /// after its first chunk). The growth is fed to the collector's trigger
    /// like an allocation of the same size.
    ///
    /// Returns `false` and changes nothing if `handle` is stale or does not
    /// refer to an [`Object::List`].
    pub(crate) fn extend_list(&mut self, handle: Handle, items: &[Value]) -> bool {
        for &item in items {
            self.mark_list_shared(item);
        }
        let Some(Object::List(list)) = self.objects.get_mut(handle) else {
            return false;
        };
        let before = list.capacity();
        list.extend_from_slice(items);
        let grown = list.capacity().saturating_sub(before);
        self.note_growth(grown, size_of::<Value>());
        true
    }

    /// Adds one value to a list whose only language-visible reference is the
    /// caller's receiver register. Capacity growth is amortized by `Vec`.
    pub(crate) fn append_list_in_place(&mut self, handle: Handle, value: Value) -> bool {
        self.mark_list_shared(value);
        let Some(Object::List(list)) = self.objects.get_mut(handle) else {
            return false;
        };
        let before = list.capacity();
        list.push(value);
        let grown = list.capacity().saturating_sub(before);
        self.note_growth(grown, size_of::<Value>());
        true
    }

    /// Takes the entries of the map at `handle` out of it, leaving it empty
    /// until [`Heap::put_map_entries`] puts them back, so that they can be
    /// merged in place (`Op::MapPut`) while this heap is still borrowed to
    /// compare keys. `None` if `handle` is stale or not an [`Object::Map`].
    pub(crate) fn take_map_entries(&mut self, handle: Handle) -> Option<Vec<MapEntry>> {
        match self.objects.get_mut(handle) {
            Some(Object::Map(entries)) => Some(std::mem::take(entries)),
            _ => None,
        }
    }

    /// Puts entries taken by [`Heap::take_map_entries`] back, feeding the
    /// growth of their buffer beyond `taken_capacity`, the capacity they had
    /// when they were taken, to the collector's trigger.
    pub(crate) fn put_map_entries(
        &mut self,
        handle: Handle,
        entries: Vec<MapEntry>,
        taken_capacity: usize,
    ) -> bool {
        for entry in &entries {
            self.mark_list_shared(entry.value);
        }
        let grown = entries.capacity().saturating_sub(taken_capacity);
        let Some(Object::Map(slot)) = self.objects.get_mut(handle) else {
            return false;
        };
        *slot = entries;
        self.note_growth(grown, size_of::<MapEntry>());
        true
    }

    fn note_growth(&mut self, slots: usize, each: usize) {
        let bytes = (slots as u64).saturating_mul(each as u64);
        self.bytes_since_collect = self.bytes_since_collect.saturating_add(bytes);
    }

    /// The node cost of one value, in bytes (section 11.4's table). A scalar
    /// costs nothing; a heap value costs its own header plus, for a
    /// collection, 8 (or, for a map, 24, and for a set, 16) bytes per element,
    /// not the cost of what it contains, which is [`Heap::tree_size`]'s job.
    #[must_use]
    pub fn node_cost(&self, value: Value) -> u64 {
        let Value::Heap(h) = value else { return 0 };
        self.get(h).map_or(0, Self::object_node_cost)
    }

    fn object_node_cost(object: &Object) -> u64 {
        let per =
            |n: usize, each: u64, base: u64| base.saturating_add((n as u64).saturating_mul(each));
        match object {
            Object::Str(s) | Object::Filter(s) => per(s.len(), 1, 16),
            Object::List(items) => per(items.len(), 8, 16),
            Object::Map(entries) => per(entries.len(), 24, 32),
            Object::Set(items) => per(items.len(), 16, 32),
            Object::Record { fields, .. } => per(fields.len(), 8, 16),
            Object::Enum { payload, .. } => {
                if payload.is_empty() {
                    0
                } else {
                    per(payload.len(), 8, 16)
                }
            }
            Object::Closure { captures, .. } => per(captures.len(), 8, 32),
            // A built-in function value holds only which one it is.
            Object::Builtin(_) | Object::Cell(_) => 0,
        }
    }

    /// An estimate, in bytes, of how much of the host's actual heap one
    /// object occupies: its own Rust representation
    /// (`size_of::<T>()`, the fixed header a `Vec` or `String`
    /// carries alongside its buffer pointer) plus, for a buffer-backed
    /// variant, that buffer's *capacity*, not its length, times the size of
    /// one element. Capacity, not length, because capacity is what the
    /// host's allocator actually reserved; `list.append` and its relatives
    /// (`natives.rs`'s own comment on `list.append`) size every buffer for
    /// its exact final length up front, so the two agree for this heap's
    /// own allocations, but a value that arrived a different way (an
    /// embedder's own capability result, say) is not guaranteed to.
    ///
    /// This is deliberately a different number from [`Heap::object_node_cost`],
    /// which is section 11.4's own formula, charged against a run's own
    /// counters at every allocation site the specification lists and read
    /// back by a fault a program can observe: that number must stay exactly
    /// what the specification defines, forever, so this heap's own
    /// day-to-day bookkeeping (when to collect, not what a program sees)
    /// cannot use it as a proxy for real memory without silently drifting
    /// from what "real" means. The two diverge most for a `List`: node cost
    /// charges the specification's 8-bytes-per-element figure (a `Value`'s
    /// own size chosen for the language, not this implementation's), where
    /// a `Value` here is actually [`size_of::<Value>()`], 24 bytes
    /// on a 64-bit host (`docs/design/security.md`'s "a list of scalars"
    /// explains the gap: a tagged union wide enough to hold either a full
    /// 64-bit scalar or a generational [`Handle`]). Only [`Heap::alloc`] and
    /// [`Heap::collect`] call this, to decide *when* [`Heap::due_for_collection`]
    /// answers `true`; nothing a program can observe depends on it, so it
    /// is free to be an estimate rather than an exact figure (no attempt is
    /// made here to account for an allocator's own bookkeeping overhead or
    /// fragmentation, only for what this heap itself asked the allocator
    /// for).
    fn object_real_bytes(object: &Object) -> u64 {
        // `cap` is the buffer's capacity, `each` the size of one element it
        // holds, `header` the fixed `size_of` of the `Vec` or
        // `String` field itself (its buffer pointer, length and capacity,
        // not what the buffer points at).
        let buf = |cap: usize, each: usize, header: usize| -> u64 {
            (header as u64).saturating_add((cap as u64).saturating_mul(each as u64))
        };
        match object {
            Object::Str(s) | Object::Filter(s) => buf(s.capacity(), 1, size_of::<String>()),
            Object::List(items) => buf(
                items.capacity(),
                size_of::<Value>(),
                size_of::<Vec<Value>>(),
            ),
            Object::Map(entries) => buf(
                entries.capacity(),
                size_of::<MapEntry>(),
                size_of::<Vec<MapEntry>>(),
            ),
            Object::Set(items) => buf(
                items.capacity(),
                size_of::<MapKey>(),
                size_of::<Vec<MapKey>>(),
            ),
            Object::Record { shape, fields } => buf(
                shape.capacity(),
                size_of::<StringId>(),
                size_of::<Vec<StringId>>(),
            )
            .saturating_add(buf(
                fields.capacity(),
                size_of::<Value>(),
                size_of::<Vec<Value>>(),
            )),
            Object::Enum { payload, .. } => buf(
                payload.capacity(),
                size_of::<Value>(),
                size_of::<Vec<Value>>(),
            ),
            Object::Closure { captures, .. } => buf(
                captures.capacity(),
                size_of::<Handle>(),
                size_of::<Vec<Handle>>(),
            ),
            // Neither holds a separate buffer: a `Builtin` is a small enum
            // of which built-in it names, a `Cell` one `Value`. The
            // object's own in-arena representation is already the whole of
            // what allocating it cost.
            Object::Builtin(_) | Object::Cell(_) => size_of::<Object>() as u64,
        }
    }

    /// The direct children of an object worth visiting one level down, for
    /// [`Heap::tree_size`] and [`Heap::collect`]: only values that are
    /// themselves [`Value::Heap`], since [`Heap::node_cost`] charges nothing
    /// for a scalar (`Int`, `Float`, `Bool`, `none`; section 11.4) and
    /// [`Heap::collect`]'s mark loop discards anything that is not
    /// `Value::Heap` immediately, without marking it or looking at its
    /// children (it has none). Pushing every scalar element onto the
    /// traversal's own work list anyway, only to have both callers throw it
    /// straight back away, cost nothing in *what* either walk computes, but
    /// a lot in what it had to allocate to get there: a list of a million
    /// plain `Int`s made both `tree_size` and `collect` copy all one million
    /// of them into a scratch `Vec<Value>`, on top of the list's own
    /// storage, for a value the walk was always going to skip. Filtering
    /// here, once, is the fix; the two-branch shape below (an early return
    /// for a value with no possible heap children, then everything else run
    /// through one filter) keeps the common case, a list or record of
    /// scalars, from allocating anything at all.
    fn children(object: &Object, out: &mut Vec<Value>) {
        let key_value = |k: MapKey| match k {
            MapKey::Str(h) => Some(Value::Heap(h)),
            MapKey::Bool(_) | MapKey::Int(_) => None,
        };
        let is_heap = |v: &Value| matches!(v, Value::Heap(_));
        match object {
            Object::Str(_) | Object::Filter(_) | Object::Closure { .. } | Object::Builtin(_) => {}
            Object::List(items) => out.extend(items.iter().copied().filter(is_heap)),
            Object::Map(entries) => {
                for e in entries {
                    out.extend(key_value(e.key));
                    if is_heap(&e.value) {
                        out.push(e.value);
                    }
                }
            }
            Object::Set(items) => out.extend(items.iter().copied().filter_map(key_value)),
            Object::Record { fields, .. } => out.extend(fields.iter().copied().filter(is_heap)),
            Object::Enum { payload, .. } => out.extend(payload.iter().copied().filter(is_heap)),
            Object::Cell(v) => {
                if is_heap(v) {
                    out.push(*v);
                }
            }
        }
    }

    /// The handles a [`Object::Cell`] of a closure's captures should also
    /// reach, kept apart from [`Heap::children`] because a closure's captures
    /// are handles to cells directly, not through a [`Value`].
    fn capture_handles(object: &Object, out: &mut Vec<Handle>) {
        if let Object::Closure { captures, .. } = object {
            out.extend(captures.iter().copied());
        }
    }

    /// The tree size of a value (section 11.4): its node cost plus the tree
    /// sizes of what it directly contains, computed iteratively with an
    /// explicit stack so that a value nested as deep as the memory limit
    /// allows cannot exhaust the host's stack. The total saturates at
    /// [`u64::MAX`]. Equivalent to [`Heap::tree_size_bounded`] with a budget
    /// of `u64::MAX`; see that function for the sharing and cycle-safety
    /// guarantee this one inherits.
    #[must_use]
    pub fn tree_size(&self, root: Value) -> u64 {
        self.tree_size_bounded(root, u64::MAX)
    }

    /// The tree size of a value (section 11.4), visiting each distinct
    /// object it contains once no matter how many paths reach it: a handle
    /// already counted contributes nothing the second time. This is what
    /// keeps the walk both linear in the number of distinct objects rather
    /// than the (possibly exponential, for a value built by repeated
    /// doubling) number of paths to them, and safe over a value that reaches
    /// itself, directly or through a chain of cells and lists, the way a
    /// captured `variable` binding can (`heap.rs`'s module doc): the second
    /// time the walk would visit that handle, the "already counted" check
    /// stops it rather than pushing the same children back onto the stack
    /// forever.
    ///
    /// `budget` bounds the work rather than only the answer: once the
    /// running total already exceeds it, the walk stops and returns
    /// whatever it has counted so far, which is guaranteed to be at least
    /// `budget` but not necessarily the value's exact tree size. Every
    /// caller of this function only ever compares its result against a
    /// remaining budget of its own (a step charge, a memory limit), for
    /// which "at least this much" answers the question exactly as well as
    /// the precise total would, so pass `u64::MAX` (see [`Heap::tree_size`])
    /// when the exact figure genuinely matters, such as in a test.
    #[must_use]
    pub fn tree_size_bounded(&self, root: Value, budget: u64) -> u64 {
        let mut total: u64 = 0;
        let mut stack = vec![root];
        let mut scratch = Vec::new();
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        while let Some(value) = stack.pop() {
            if let Value::Heap(h) = value {
                // Keyed by slot index alone, not the full generation pair:
                // sound for the same reason `Heap::collect`'s mark bitmap is
                // (see its comment), because this walk never allocates or
                // frees anything, so no handle it meets can be a stale alias
                // of a slot a later allocation has since reused.
                if !seen.insert(h.index()) {
                    continue;
                }
            }
            total = total.saturating_add(self.node_cost(value));
            if total > budget {
                return total;
            }
            if let Value::Heap(h) = value {
                if let Some(object) = self.get(h) {
                    scratch.clear();
                    Self::children(object, &mut scratch);
                    stack.append(&mut scratch);
                }
            }
        }
        total
    }

    /// Marks every handle reachable from `roots` and from the cells that a
    /// reachable closure captured, iteratively, then frees every unmarked
    /// object. Returns the number of objects freed.
    ///
    /// `roots` should be the current values of every top-level binding of
    /// every module, per section 11.4's definition of what a retained-size
    /// walk (and so a collection) must keep.
    pub fn collect(&mut self, roots: &[Value]) -> usize {
        // A mark bitmap the width of the arena's own slot count, rather
        // than a `BTreeSet<u32>` of marked indices: this collector is now
        // called far more often than the old between-runs-only policy ever
        // called it (a mid-run safepoint, not only a load or a call
        // boundary), so a collection's own constant factor matters far more
        // than it used to, and a `Vec<bool>` indexed directly by slot is a
        // flat array write with no allocation or tree traversal per mark,
        // where a `BTreeSet` insert is a node lookup or allocation every
        // time. Keying by index alone, rather than by the full `(index,
        // generation)` pair, is sound only because every handle reachable
        // during one collection names the object currently in its slot: the
        // arena's generation counter is wide enough (`u64`, see
        // `crate::arena`'s module doc) that no handle still reachable from a
        // root can be a stale alias of a slot that has since been reused.
        let mut marked = vec![false; self.objects.slot_count()];
        let mut work: Vec<Value> = roots.to_vec();
        let mut scratch_values = Vec::new();
        let mut scratch_handles = Vec::new();
        // Summed as each object is marked, not by a second pass over the
        // survivors after the sweep below: this is the same walk either
        // way, one object at a time, so folding the sum into it costs
        // nothing extra and halves how many times a live heap is visited
        // per collection. In `object_real_bytes`'s units, not
        // `object_node_cost`'s: this total only ever feeds
        // `collect_threshold` below, this module's own trigger, never a
        // program-observable figure (section 11.4's own retained-size walk
        // is `Heap::tree_size`, entirely separate, still node cost).
        let mut live_bytes: u64 = 0;
        while let Some(value) = work.pop() {
            let Value::Heap(h) = value else { continue };
            let index = h.index() as usize;
            match marked.get_mut(index) {
                Some(slot) if !*slot => *slot = true,
                _ => continue,
            }
            let Some(object) = self.get(h) else { continue };
            live_bytes = live_bytes.saturating_add(Self::object_real_bytes(object));
            scratch_values.clear();
            Self::children(object, &mut scratch_values);
            work.append(&mut scratch_values);
            scratch_handles.clear();
            Self::capture_handles(object, &mut scratch_handles);
            work.extend(scratch_handles.drain(..).map(Value::Heap));
        }
        let freed = self
            .objects
            .retain(|h, _| marked.get(h.index() as usize).copied().unwrap_or(false));
        self.shared_lists
            .retain(|handle| matches!(self.objects.get(*handle), Some(Object::List(_))));
        // The trigger resets against what this collection actually found
        // live, not against zero: a heap that stays at, say, 10 MB of live
        // data collects again once another `collect_threshold` bytes have
        // been allocated on top of that 10 MB, not once 10 MB has been
        // allocated from an assumed-empty heap.
        self.bytes_since_collect = 0;
        self.collect_threshold = live_bytes
            .saturating_mul(COLLECT_GROWTH_FACTOR)
            .max(MIN_COLLECT_THRESHOLD_BYTES);
        freed
    }
}

#[cfg(test)]
mod tests {
    use super::Heap;
    use crate::value::{MapEntry, MapKey, Object, Value};

    #[test]
    fn node_cost_matches_the_specification_table() {
        let mut heap = Heap::new();
        let s = heap.alloc(Object::Str("hello".into())).unwrap();
        assert_eq!(heap.node_cost(s), 16 + 5);
        let list = heap
            .alloc(Object::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
            ]))
            .unwrap();
        assert_eq!(heap.node_cost(list), 16 + 8 * 3);
        let empty_variant = heap
            .alloc(Object::Enum {
                variant: 0,
                payload: vec![],
            })
            .unwrap();
        assert_eq!(heap.node_cost(empty_variant), 0);
        let with_payload = heap
            .alloc(Object::Enum {
                variant: 1,
                payload: vec![Value::Int(1)],
            })
            .unwrap();
        assert_eq!(heap.node_cost(with_payload), 16 + 8);
        assert_eq!(heap.node_cost(Value::Int(5)), 0);
    }

    /// A regression test for the collector's trigger being fed real bytes
    /// (`Heap::object_real_bytes`), not section 11.4's node cost
    /// (`docs/design/security.md`, "what it deliberately does not
    /// promise"): simulates `list.append`'s own real allocation pattern
    /// directly through the heap, a growing `List` of `String`s where every
    /// append copies the whole list to a fresh buffer sized to its exact
    /// final length up front, exactly as `natives.rs`'s own `list.append`
    /// does (so this test does not need a parser, a compiler or the
    /// interpreter to reproduce the shape that motivated the fix).
    ///
    /// The two formulas cross [`MIN_COLLECT_THRESHOLD_BYTES`] (4 MiB) at
    /// different append counts for this shape: node cost only charges the
    /// specification's 8-bytes-per-list-element figure, so it takes roughly
    /// 1,020 appends; real bytes charges [`Value`]'s own 24-byte
    /// representation instead, so it takes roughly 589. A regression back
    /// to node cost (charging `object_node_cost` in [`Heap::alloc`] and
    /// [`Heap::collect`] instead of `object_real_bytes`) would push this
    /// test's own first-due append count well past the bound checked here.
    #[test]
    fn the_trigger_notices_a_loop_of_appends_real_bytes_not_only_their_node_cost() {
        let mut heap = Heap::new();
        let mut items: Vec<Value> = Vec::new();
        let mut first_due_at = None;
        for i in 0..3_000u32 {
            let s = heap.alloc(Object::Str(String::new())).unwrap();
            items.push(s);
            // `list.append`'s own shape (`natives.rs`'s own comment on it):
            // sized to the final length up front, one copy, not a `to_vec`
            // that `push` then grows again.
            let mut out = Vec::with_capacity(items.len());
            out.extend_from_slice(&items);
            let _list = heap.alloc(Object::List(out)).unwrap();
            if first_due_at.is_none() && heap.due_for_collection() {
                first_due_at = Some(i + 1);
            }
        }
        let first_due_at = first_due_at
            .expect("3,000 appends of a String each must cross the 4 MiB floor at least once");
        assert!(
            first_due_at < 800,
            "a collection should already be due well before the 800th append once the trigger \
             is fed real bytes (Value is 24 bytes, not the 8 the specification charges per list \
             element); it first fired at {first_due_at}, close to the roughly 1,020th append that \
             the specification's own node-cost formula alone would produce"
        );
        assert!(
            first_due_at > 200,
            "collecting this much earlier than the real-bytes formula predicts would mean the \
             threshold shrank far more than this fix intended: first fired at {first_due_at}"
        );
    }

    /// A regression test for the memory-amplification fix: `children` must
    /// push only a value that could itself lead somewhere else in the heap,
    /// not every scalar a list, record, enum or set happens to hold. Before
    /// this fix, a list of a million plain `Int`s made both `tree_size` and
    /// `collect` copy all one million of them into a scratch `Vec<Value>`
    /// for nothing, since a scalar costs nothing and has no children; a
    /// program that built and retained an eight-million-element list of
    /// `Int`s (`range(0, 8000000)`, within the default 64 MiB accounting
    /// limit) measured at roughly 578 MB of real resident memory before this
    /// fix and roughly 194 MB after it.
    #[test]
    fn children_only_visits_values_that_could_reach_further_heap_objects() {
        let mut out = Vec::new();
        let scalars = Object::List(vec![
            Value::Int(1),
            Value::Float(2.0),
            Value::Bool(true),
            Value::None,
        ]);
        Heap::children(&scalars, &mut out);
        assert!(
            out.is_empty(),
            "a list of scalars has nothing worth visiting: {out:?}"
        );

        let mut heap = Heap::new();
        let inner = heap.alloc(Object::Str("x".into())).unwrap();
        let Value::Heap(inner_handle) = inner else {
            panic!("alloc always returns a Value::Heap")
        };
        let mixed = Object::List(vec![Value::Int(1), inner, Value::None, inner]);
        let mut out = Vec::new();
        Heap::children(&mixed, &mut out);
        assert_eq!(
            out.len(),
            2,
            "the two heap values are kept and nothing else is: {out:?}"
        );
        for value in out {
            assert!(matches!(value, Value::Heap(h) if h == inner_handle));
        }

        let scalar_record = Object::Record {
            shape: Vec::new(),
            fields: vec![Value::Int(1), Value::Bool(false)],
        };
        let mut out = Vec::new();
        Heap::children(&scalar_record, &mut out);
        assert!(out.is_empty());

        let scalar_enum = Object::Enum {
            variant: 0,
            payload: vec![Value::Int(1), Value::None],
        };
        let mut out = Vec::new();
        Heap::children(&scalar_enum, &mut out);
        assert!(out.is_empty());

        // A map's non-string key contributes no handle (it is not itself a
        // heap value), and neither does a scalar entry value.
        let scalar_map = Object::Map(vec![MapEntry {
            key: MapKey::Int(1),
            value: Value::Int(2),
        }]);
        let mut out = Vec::new();
        Heap::children(&scalar_map, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn tree_size_sums_node_costs_without_recursion() {
        let mut heap = Heap::new();
        let mut list = Value::None;
        // A chain nested far deeper than the default call stack would allow
        // if this walked with ordinary recursion.
        for i in 0..200_000 {
            let cell = if i == 0 {
                vec![Value::Int(0)]
            } else {
                vec![list]
            };
            list = heap.alloc(Object::List(cell)).unwrap();
        }
        let size = heap.tree_size(list);
        assert_eq!(size, 200_000 * (16 + 8));
    }

    /// A cell that (through a list) holds itself used to send `tree_size`
    /// into an endless loop, because nothing stopped it visiting the same
    /// handle's children over and over. It must now terminate, counting the
    /// cycle's objects once each.
    #[test]
    fn tree_size_terminates_over_a_self_referential_cell() {
        let mut heap = Heap::new();
        let Value::Heap(cell) = heap.alloc(Object::Cell(Value::None)).unwrap() else {
            unreachable!()
        };
        let list = heap.alloc(Object::List(vec![Value::Heap(cell)])).unwrap();
        assert!(heap.set_cell(cell, list));
        let size = heap.tree_size(Value::Heap(cell));
        // The cell and the one-element list it (indirectly) holds, each
        // counted once: 0 (a cell's own node cost) + 16 + 8 (the list).
        assert_eq!(size, 24);
    }

    /// A value built by repeated doubling (`t = Tree.Node(t, t)`) shares
    /// almost everything between its two "halves"; a walk that does not
    /// notice the sharing revisits the same subtree twice per level and
    /// costs time exponential in the depth. Deduplicating by handle keeps it
    /// linear in the number of distinct objects instead.
    #[test]
    fn tree_size_is_linear_in_distinct_objects_not_paths() {
        let mut heap = Heap::new();
        let mut t = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..48 {
            t = heap.alloc(Object::List(vec![t, t])).unwrap();
        }
        // Without sharing this would be roughly 2^48 node visits; with 49
        // distinct list objects (the leaf plus 48 doublings, each a
        // two-element list costing 16 + 8*2) it finishes immediately.
        let size = heap.tree_size(t);
        assert_eq!(size, 16 + 48 * 32);
    }

    #[test]
    fn tree_size_bounded_stops_once_past_the_budget() {
        let mut heap = Heap::new();
        let mut list = Value::None;
        for i in 0..1000 {
            let cell = if i == 0 {
                vec![Value::Int(0)]
            } else {
                vec![list]
            };
            list = heap.alloc(Object::List(cell)).unwrap();
        }
        let full = heap.tree_size(list);
        let bounded = heap.tree_size_bounded(list, 100);
        assert!(bounded > 100, "stops only once past the budget");
        assert!(
            bounded < full,
            "stopping early counts less than a full walk would: {bounded} vs {full}"
        );
    }

    #[test]
    fn keys_compare_by_content_not_by_handle() {
        let mut heap = Heap::new();
        let a = heap.alloc(Object::Str("x".into())).unwrap();
        let b = heap.alloc(Object::Str("x".into())).unwrap();
        let (Value::Heap(ah), Value::Heap(bh)) = (a, b) else {
            unreachable!()
        };
        assert_ne!(ah, bh, "two separate allocations get different handles");
        assert!(
            heap.keys_equal(MapKey::Str(ah), MapKey::Str(bh)),
            "but equal content compares equal"
        );
        assert_eq!(
            heap.compare_keys(MapKey::Bool(true), MapKey::Int(0)),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            heap.compare_keys(MapKey::Int(9), MapKey::Str(ah)),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn collect_frees_unreachable_objects_including_a_cycle() {
        let mut heap = Heap::new();
        let kept = heap.alloc(Object::Str("kept".into())).unwrap();
        let garbage = heap.alloc(Object::Str("garbage".into())).unwrap();
        assert_eq!(heap.len(), 2);
        let freed = heap.collect(&[kept]);
        assert_eq!(freed, 1);
        assert_eq!(heap.len(), 1);
        let Value::Heap(garbage_handle) = garbage else {
            unreachable!()
        };
        assert!(heap.get(garbage_handle).is_none());

        // A cycle through two cells, unreachable from the roots, is collected
        // in full: this is what mark and sweep buys over reference counting.
        let a = heap.alloc(Object::Cell(Value::None)).unwrap();
        let b = heap.alloc(Object::Cell(Value::None)).unwrap();
        let (Value::Heap(ah), Value::Heap(bh)) = (a, b) else {
            unreachable!()
        };
        *match heap.objects.get_mut(ah) {
            Some(Object::Cell(v)) => v,
            _ => unreachable!(),
        } = b;
        *match heap.objects.get_mut(bh) {
            Some(Object::Cell(v)) => v,
            _ => unreachable!(),
        } = a;
        assert_eq!(
            heap.collect(&[kept]),
            2,
            "the cycle is collected because nothing roots it"
        );
    }

    #[test]
    fn set_cell_overwrites_a_live_cell_and_rejects_anything_else() {
        let mut heap = Heap::new();
        let Value::Heap(cell) = heap.alloc(Object::Cell(Value::Int(1))).unwrap() else {
            unreachable!()
        };
        assert!(heap.set_cell(cell, Value::Int(2)));
        assert!(matches!(heap.get(cell), Some(Object::Cell(Value::Int(2)))));

        let Value::Heap(not_a_cell) = heap.alloc(Object::Str("x".into())).unwrap() else {
            unreachable!()
        };
        assert!(!heap.set_cell(not_a_cell, Value::Int(9)));

        heap.collect(&[]);
        assert!(
            !heap.set_cell(cell, Value::Int(3)),
            "a freed handle is stale"
        );
    }

    #[test]
    fn a_map_entry_keeps_its_key_and_value_reachable() {
        let mut heap = Heap::new();
        let key = heap.alloc(Object::Str("k".into())).unwrap();
        let value = heap.alloc(Object::Str("v".into())).unwrap();
        let Value::Heap(key_handle) = key else {
            unreachable!()
        };
        let map = heap
            .alloc(Object::Map(vec![MapEntry {
                key: MapKey::Str(key_handle),
                value,
            }]))
            .unwrap();
        assert_eq!(
            heap.collect(&[map]),
            0,
            "the key and the value are both reachable through the entry"
        );
        assert_eq!(heap.len(), 3);
    }
}
