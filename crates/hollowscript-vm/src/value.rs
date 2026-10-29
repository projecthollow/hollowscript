//! Values (chapter 2 and 6).
//!
//! A [`Value`] is either a scalar, held inline, or a [`Handle`] into the
//! [`crate::heap::Heap`]. This split matches the node cost table of section
//! 11.4: `Int`, `Float`, `Bool` and `none` cost nothing of their own because
//! they never occupy a heap slot.

use crate::arena::Handle;
use hollowscript_bytecode::module::StringId;

/// A language value. `Copy`, because every heap-allocated kind is a small
/// [`Handle`] rather than the data itself: copying a `Value` is exactly the
/// no-op that copying an immutable, possibly-shared value should be
/// (section 6, "aliasing is therefore unobservable").
#[derive(Copy, Clone, Debug)]
pub enum Value {
    /// An `Int`.
    Int(i64),
    /// A `Float`, always finite and never negative zero (section 3.4).
    Float(f64),
    /// A `Bool`.
    Bool(bool),
    /// The single value of an absent optional.
    None,
    /// A `String`, `Filter`, list, map, set, record, enum value, closure or
    /// cell, held in the heap.
    Heap(Handle),
}

/// One field of a record value, in the order of the fields listed in
/// [`Object::Record`]'s own `shape`.
pub type RecordFields = Vec<Value>;

/// An entry of a map value, kept sorted by [`MapKey`]'s ordering (through
/// [`crate::heap::Heap::compare_keys`]) so that iteration, equality and `for`
/// need no separate sort.
#[derive(Copy, Clone, Debug)]
pub struct MapEntry {
    /// The key.
    pub key: MapKey,
    /// The value.
    pub value: Value,
}

/// A key of a map or an element of a set: `Int`, `String` or `Bool`
/// (section 2.8). A `String` key is a heap handle to an [`Object::Str`].
///
/// This type deliberately has no `Eq` or `Ord` of its own: two keys of kind
/// `Str` are equal exactly when their *content* is, which needs the heap to
/// look the content up, not just the two handles. Comparing and ordering keys
/// is [`crate::heap::Heap::compare_keys`] and [`crate::heap::Heap::keys_equal`].
#[derive(Copy, Clone, Debug)]
pub enum MapKey {
    /// `false` sorts before `true` (section 6.2).
    Bool(bool),
    /// Compared and ordered numerically.
    Int(i64),
    /// A `String`, compared and ordered by content through the heap.
    Str(Handle),
}

/// Which built-in a [`Object::Builtin`] function value stands for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Builtin {
    /// The native import at this index of the module's own `natives` table
    /// (`Op::NativeFn`), the same index `Op::CallNative` takes.
    Native(u32),
    /// The variant at this index of the module's `variants` table
    /// (`Op::VariantFn`), the same index `Op::NewEnum` takes.
    Variant(u32),
    /// The capability function at this index of the module's `cap_calls`
    /// table (`Op::CapFn`), the same index `Op::CallCap` takes.
    Capability(u32),
}

/// A heap-allocated object. One arena of [`Object`] is the whole heap
/// ([`crate::heap::Heap`]); the object's own kind, not a separate arena,
/// distinguishes a list from a closure.
#[derive(Clone, Debug)]
pub enum Object {
    /// The UTF-8 bytes of a `String`.
    Str(String),
    /// The unescaped source of a `Filter` literal.
    Filter(String),
    /// A list's elements, in order.
    List(Vec<Value>),
    /// A map's entries, sorted ascending by key with no duplicate key.
    Map(Vec<MapEntry>),
    /// A set's elements, sorted ascending with no duplicate.
    Set(Vec<MapKey>),
    /// A record's field values, in the order of `shape`.
    Record {
        /// The field names, sorted, giving the order of `fields`.
        shape: Vec<StringId>,
        /// The field values, aligned with `shape`.
        fields: RecordFields,
    },
    /// An enum value.
    Enum {
        /// The index of the module's enum declaration.
        variant: u32,
        /// The payload values, empty for a variant with none.
        payload: Vec<Value>,
    },
    /// A closure: the function it runs and the cells it captured, aligned
    /// with the function's declared capture list.
    Closure {
        /// The index into the module's function table.
        function: u32,
        /// The captured cells, each a handle to an [`Object::Cell`].
        captures: Vec<Handle>,
    },
    /// A function value that is not a script closure: a library function, a
    /// variant constructor or a capability function, each named as its own
    /// module names it. Section 3.5 makes function values incomparable, and
    /// none of these captures anything, so the object holds only which one
    /// it is.
    Builtin(Builtin),
    /// A cell: the mutable storage of one captured `variable` binding. A
    /// `constant` a closure captures needs no cell, since its value never
    /// changes; the compiler only allocates one for a `variable`.
    Cell(Value),
}

#[cfg(test)]
mod tests {
    use super::Value;

    #[test]
    fn scalars_are_copy_and_need_no_heap() {
        let a = Value::Int(1);
        let b = a;
        assert!(matches!((a, b), (Value::Int(1), Value::Int(1))));
    }
}
