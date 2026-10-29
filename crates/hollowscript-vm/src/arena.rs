//! An index-based arena with generational handles.
//!
//! This is the storage the heap ([`crate::heap::Heap`]) is built on, and it is
//! written once here so nothing else in the VM invents its own indexing
//! scheme. A [`Handle`] is a slot index plus the generation the slot had when
//! it was allocated. Freeing a slot bumps its generation, so a handle taken
//! before the free reads as absent afterwards rather than silently returning
//! whatever was allocated into the reused slot: the classic use-after-free of
//! a raw index is turned into a checked `None`, in ordinary safe Rust, with no
//! unsafe code anywhere in this crate.
//!
//! **The generation counter is a `u64`, not a `u32`, on purpose.** An
//! instance's heap is long lived: chapter 12 has globals and captured state
//! persist across many calls, and a host such as a game engine is expected to
//! call an exported function every frame for the life of a process. A hot
//! slot that is freed and reused enough times would eventually wrap a
//! narrower counter back to a value a stale, still-retained [`Handle`]
//! happens to carry, at which point that old handle would alias whatever
//! unrelated object now occupies the slot: a logic-level type confusion that
//! stays inside safe, bounds-checked Rust but breaks the invariant every
//! caller of [`Arena::get`] relies on. At even ten billion reuses of one slot
//! per second, a `u64` generation does not wrap within the working life of
//! any real process, which is what lets [`Arena::free`] use a plain
//! `wrapping_add` rather than having to detect and handle the wrap itself.
//!
//! **A [`Handle`] also carries the identity of the arena it came from.**
//! Section 12.5 promises "no shared memory between instances", but an
//! instance's heap is an [`Arena`] indexed by slot and generation alone: two
//! instances loaded from the same program allocate their first string at
//! index 0, generation 0, so a `Handle` taken from one instance's heap and
//! handed to another (a host mixing up which instance a value came from; an
//! instance's own argument acceptance for a host-called export is where this
//! was found reachable from) can by coincidence name a live slot of the
//! wrong arena, and every check in this module would pass: not a stale
//! handle, an *aliased* one, reading whatever unrelated object happens to
//! occupy that slot of a heap it was never allocated from. Each `Arena` is
//! minted a process-wide unique id when it is created,
//! every [`Handle`] it hands out carries that id, and every accessor here
//! refuses a handle whose id does not match before it looks at the slot at
//! all, the same way a generation mismatch already does for a stale handle
//! of the *same* arena.

use std::sync::atomic::{AtomicU64, Ordering};

/// A process-wide counter of arenas created, so that no two arenas (and so
/// no two [`Handle`]s from different arenas) ever share an id: relaxed
/// ordering is enough, since nothing but uniqueness of the returned value is
/// asked of it, not ordering with any other access.
static NEXT_ARENA_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh id no other arena in this process has been, or will be, given.
fn next_arena_id() -> u64 {
    NEXT_ARENA_ID.fetch_add(1, Ordering::Relaxed)
}

/// A reference to a slot of an [`Arena`]. Stable across the arena's own
/// growth and reuse: it names the slot only, so keys taken before the arena
/// resizes stay valid. Tied to the one arena that minted it: a handle from a
/// different arena, even one of the same shape at the same index and
/// generation, is never accepted (see the module documentation).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Handle {
    arena: u64,
    index: u32,
    generation: u64,
}

impl Handle {
    /// The slot index. Exposed for a caller that indexes its own side table
    /// by handle; such a caller must still check the generation and the
    /// arena identity itself, or use the arena's own accessors, which do.
    #[must_use]
    pub fn index(self) -> u32 {
        self.index
    }
}

/// An arena's slot index space is exhausted: its historical slot count
/// (live and freed slots together) has already reached [`u32::MAX`], so a
/// new slot cannot be given an index that fits [`Handle`]. This is not a
/// memory-safety hazard (the arena would still be indexed correctly), but
/// silently clamping the index instead of reporting this, as the arena used
/// to do, would alias every allocation past this point onto the handle of
/// whichever one happened first: two logically distinct heap objects sharing
/// one `Handle`. In practice this needs slightly more than four billion
/// slots to have existed at once in this arena's history, which nothing in
/// this workspace can reach today, but the caller, not this type, is the one
/// who gets to decide what "cannot happen" means for hostile input.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ArenaExhausted;

impl std::fmt::Display for ArenaExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the arena's slot index space is exhausted")
    }
}

impl std::error::Error for ArenaExhausted {}

/// The index of the next slot an arena of `len` existing slots would create,
/// or [`ArenaExhausted`] if `len` has already reached [`u32::MAX`]. A free
/// function of the length alone, rather than a method reading `self.slots`,
/// so that the boundary can be tested directly without actually growing a
/// `Vec` to billions of elements.
fn next_index(len: usize) -> Result<u32, ArenaExhausted> {
    u32::try_from(len).map_err(|_| ArenaExhausted)
}

struct Slot<T> {
    generation: u64,
    value: Option<T>,
}

/// A generational arena of values of one type.
#[derive(Debug)]
pub struct Arena<T> {
    /// This arena's own id, minted once at construction; every [`Handle`]
    /// this arena hands out carries it, and every accessor checks it first
    /// (see the module documentation).
    id: u64,
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

// A hand written impl, because `T: Default` is not needed to build an empty
// arena, only to fill one of its slots, and `#[derive(Default)]` would wrongly
// require it of every `T`.
impl<T> Default for Arena<T> {
    fn default() -> Arena<T> {
        Arena::new()
    }
}

impl<T> std::fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("generation", &self.generation)
            .field("live", &self.value.is_some())
            .finish()
    }
}

impl<T> Arena<T> {
    /// An empty arena, with an id no other arena in this process has.
    #[must_use]
    pub fn new() -> Arena<T> {
        Arena {
            id: next_arena_id(),
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    /// Stores `value` and returns a handle to it, reusing the most recently
    /// freed slot when there is one.
    ///
    /// # Errors
    ///
    /// [`ArenaExhausted`] if this arena's historical slot count has already
    /// reached [`u32::MAX`] and no freed slot is available to reuse, rather
    /// than silently aliasing a new object onto an old one's handle.
    pub fn alloc(&mut self, value: T) -> Result<Handle, ArenaExhausted> {
        if let Some(index) = self.free.pop() {
            if let Some(slot) = self.slots.get_mut(index as usize) {
                slot.value = Some(value);
                return Ok(Handle {
                    arena: self.id,
                    index,
                    generation: slot.generation,
                });
            }
        }
        let index = next_index(self.slots.len())?;
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        Ok(Handle {
            arena: self.id,
            index,
            generation: 0,
        })
    }

    /// The value of `handle`, or `None` if it was freed, never allocated by
    /// this arena, or (section 12.5) allocated by a different arena entirely.
    #[must_use]
    pub fn get(&self, handle: Handle) -> Option<&T> {
        if handle.arena != self.id {
            return None;
        }
        let slot = self.slots.get(handle.index as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    /// A mutable reference to the value of `handle`.
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut T> {
        if handle.arena != self.id {
            return None;
        }
        let slot = self.slots.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        slot.value.as_mut()
    }

    /// Whether `handle` still refers to a live value of this arena.
    #[must_use]
    pub fn contains(&self, handle: Handle) -> bool {
        self.get(handle).is_some()
    }

    /// Frees the slot of `handle` and returns its value, bumping the slot's
    /// generation so that `handle`, and every copy of it, reads as absent from
    /// now on. Freeing an already-freed or unknown handle, or one of a
    /// different arena, does nothing and returns `None`.
    pub fn free(&mut self, handle: Handle) -> Option<T> {
        if handle.arena != self.id {
            return None;
        }
        let slot = self.slots.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(handle.index);
        Some(value)
    }

    /// The number of live values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    /// Whether the arena holds no live values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// This arena's slot count, live and freed slots together: one past the
    /// largest [`Handle::index`] a handle of this arena can ever have had.
    /// For a collector's mark bitmap ([`Heap::collect`](crate::heap::Heap::collect)),
    /// sized once per collection rather than looked up per handle.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Every live handle and its value, in slot order. Used by a collector to
    /// sweep: nothing else in this crate needs to enumerate an arena.
    ///
    /// The index-to-`u32` conversion below cannot fail: `index` is always
    /// less than `self.slots.len()`, and [`Arena::alloc`] never lets that
    /// length exceed [`u32::MAX`], so this never hits the fallback.
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &T)> {
        let id = self.id;
        self.slots
            .iter()
            .enumerate()
            .filter_map(move |(index, slot)| {
                slot.value.as_ref().map(|v| {
                    (
                        Handle {
                            arena: id,
                            index: u32::try_from(index).unwrap_or(u32::MAX),
                            generation: slot.generation,
                        },
                        v,
                    )
                })
            })
    }

    /// Frees every slot for which `keep` returns `false`. Returns the number freed.
    ///
    /// As with [`Arena::iter`], the index-to-`u32` conversion below cannot
    /// fail in practice, for the same reason.
    pub fn retain(&mut self, mut keep: impl FnMut(Handle, &T) -> bool) -> usize {
        let mut freed = 0;
        let id = self.id;
        for index in 0..self.slots.len() {
            let handle = {
                let Some(slot) = self.slots.get(index) else {
                    continue;
                };
                let Some(value) = slot.value.as_ref() else {
                    continue;
                };
                let h = Handle {
                    arena: id,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    generation: slot.generation,
                };
                if keep(h, value) {
                    continue;
                }
                h
            };
            self.free(handle);
            freed += 1;
        }
        freed
    }
}

#[cfg(test)]
mod tests {
    use super::{Arena, ArenaExhausted, Handle, next_index};

    #[test]
    fn alloc_and_get_round_trip() {
        let mut a: Arena<i32> = Arena::new();
        let h = a.alloc(42).unwrap();
        assert_eq!(a.get(h), Some(&42));
        assert_eq!(a.len(), 1);
        *a.get_mut(h).unwrap() = 43;
        assert_eq!(a.get(h), Some(&43));
    }

    #[test]
    fn a_freed_handle_never_reads_a_reused_slot() {
        let mut a: Arena<&str> = Arena::new();
        let first = a.alloc("first").unwrap();
        assert_eq!(a.free(first), Some("first"));
        assert_eq!(a.get(first), None);
        assert!(!a.contains(first));
        let second = a.alloc("second").unwrap();
        assert_eq!(second.index(), first.index(), "the slot is reused");
        assert_ne!(second, first, "but the generation differs");
        assert_eq!(
            a.get(first),
            None,
            "the old handle still does not see the new value"
        );
        assert_eq!(a.get(second), Some(&"second"));
        assert_eq!(a.free(first), None, "freeing a stale handle does nothing");
        assert_eq!(a.get(second), Some(&"second"));
    }

    /// Section 12.5: a handle from one arena must never be accepted
    /// by another, even at the same index and generation, which is exactly
    /// what two instances loaded from the same program produce for their
    /// first allocation.
    #[test]
    fn a_handle_from_a_different_arena_is_never_accepted() {
        let mut a: Arena<&str> = Arena::new();
        let mut b: Arena<&str> = Arena::new();
        let from_a = a.alloc("a's string").unwrap();
        let from_b = b.alloc("b's string").unwrap();
        assert_eq!(
            from_a.index(),
            from_b.index(),
            "both arenas allocate their first slot at the same index"
        );
        assert_ne!(from_a, from_b, "but the handles are not equal");
        assert_eq!(b.get(from_a), None, "a's handle is foreign to b");
        assert_eq!(a.get(from_b), None, "b's handle is foreign to a");
        assert!(!b.contains(from_a));
        assert_eq!(
            b.free(from_a),
            None,
            "freeing a foreign handle does nothing"
        );
        assert_eq!(
            b.get(from_b),
            Some(&"b's string"),
            "b's own value is untouched"
        );
    }

    #[test]
    fn retain_sweeps_by_predicate() {
        let mut a: Arena<i32> = Arena::new();
        let keep = a.alloc(1).unwrap();
        let drop1 = a.alloc(2).unwrap();
        let drop2 = a.alloc(3).unwrap();
        let freed = a.retain(|h, _| h == keep);
        assert_eq!(freed, 2);
        assert_eq!(a.len(), 1);
        assert_eq!(a.get(keep), Some(&1));
        assert_eq!(a.get(drop1), None);
        assert_eq!(a.get(drop2), None);
    }

    #[test]
    fn iter_visits_only_live_values() {
        let mut a: Arena<i32> = Arena::new();
        let x = a.alloc(1).unwrap();
        let y = a.alloc(2).unwrap();
        a.free(x);
        let live: Vec<_> = a.iter().map(|(h, v)| (h, *v)).collect();
        assert_eq!(live, vec![(y, 2)]);
    }

    /// A stale handle from just before the generation counter would have
    /// wrapped must still be rejected once the slot is freed and reallocated
    /// twice more, rather than aliasing the new occupant. The counter is
    /// forced to the edge directly (through the crate-private field) instead
    /// of actually freeing and reallocating a slot `u64::MAX` times.
    #[test]
    fn a_handle_from_before_a_generation_wrap_is_still_rejected() {
        let mut a: Arena<&str> = Arena::new();
        let h0 = a.alloc("zero").unwrap();
        a.slots[h0.index() as usize].generation = u64::MAX - 1;
        let stale = Handle {
            arena: a.id,
            index: h0.index(),
            generation: u64::MAX - 1,
        };
        assert_eq!(a.get(stale), Some(&"zero"));

        assert_eq!(a.free(stale), Some("zero"));
        let h1 = a.alloc("one").unwrap();
        assert_eq!(h1.index(), stale.index());
        assert_eq!(h1.generation, u64::MAX);
        assert_eq!(a.get(stale), None, "the pre-wrap handle is already stale");

        assert_eq!(a.free(h1), Some("one"));
        let h2 = a.alloc("two").unwrap();
        assert_eq!(h2.index(), stale.index());
        assert_eq!(
            h2.generation, 0,
            "the counter has now wrapped past u64::MAX"
        );
        assert_eq!(
            a.get(stale),
            None,
            "the handle from before the wrap must not alias the new occupant"
        );
        assert_eq!(a.get(h1), None, "the intervening handle is also stale");
        assert_eq!(a.get(h2), Some(&"two"));
    }

    #[test]
    fn next_index_fails_once_the_slot_index_space_is_exhausted() {
        assert_eq!(next_index(0), Ok(0));
        assert_eq!(next_index(u32::MAX as usize), Ok(u32::MAX));
        assert_eq!(
            next_index(u32::MAX as usize + 1),
            Err(ArenaExhausted),
            "a slot count that has already reached u32::MAX has no next index left to hand out"
        );
    }
}
