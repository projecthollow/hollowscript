//! Descriptions of the types that cross the boundary between a script and its
//! host (sections 9.6 and 12.4).
//!
//! Capability interfaces, exported functions and exported constants are
//! described with these types. The checker uses them to declare capabilities,
//! the bytecode container stores them, and the VM validates the values that
//! cross the boundary against them. A table is used instead of a tree because
//! an enum may contain itself.

use std::collections::BTreeSet;

/// An index into a [`TypeTable`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeRef(pub u32);

/// One variant of a transferable enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantDef {
    /// The variant name, which starts with an upper case ASCII letter.
    pub name: String,
    /// The payload types, by position.
    pub payload: Vec<TypeRef>,
}

/// The identity of an enum declaration that crosses the host boundary
/// (section 10.4: "an enum type is identified by its declaring module and
/// name", section 9.6). Two [`TransferType::Enum`] entries with the same
/// spelled name are still different types unless their `id`s are equal.
///
/// This mirrors `hollowscript_check::types::DeclId` field for field (a
/// declaring module index and a per-module declaration index), but is
/// defined here, in the crate every part of the boundary depends on, rather
/// than borrowed from the checker: `hollowscript-core` has no dependency on
/// `hollowscript-check` to borrow it from, and a host building a
/// [`TypeTable`] before any program is checked has no `DeclId` to give one
/// anyway. [`HOST_MODULE`] is numerically equal to the checker's own
/// `PRELUDE_MODULE`, and [`RESULT_INDEX`] to its `RESULT_DECL_INDEX`, on
/// purpose: `hollowscript_check::effects::transfer_type_id` reads a host
/// enum's `id` straight into a `DeclId` with no translation, so an id minted
/// here and one minted there are directly comparable without either crate
/// knowing about the other's type.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId {
    /// The declaring module, or [`HOST_MODULE`] for an enum a host declares
    /// directly in a capability interface rather than a program declaring
    /// with `enum`.
    pub module: u32,
    /// A counter unique within `module`.
    pub index: u32,
}

/// The pseudo-module id of an enum a host declares directly, rather than a
/// program module. Out of range of any real module index, and numerically
/// equal to `hollowscript_check::types::PRELUDE_MODULE`, so that an id
/// [`TypeTable::add_enum`] mints here and the `DeclId` the checker builds
/// from it name the same declaration with no translation.
pub const HOST_MODULE: u32 = u32::MAX;

/// The reserved index of the language's own `Result<T, E>` (section 8.2)
/// within [`HOST_MODULE`]. Not assigned by [`TypeTable::add_enum`]: the
/// checker recognises `Result` by name and shape and folds it into its one
/// builtin declaration regardless of what id a host-declared `Result` entry
/// carries, so this exists only to keep the reserved range documented and
/// to keep [`TypeTable::add_enum`]'s counter from ever handing it out.
pub const RESULT_INDEX: u32 = 0;

/// The reserved index of the language's own `Json` (section 15.1) within
/// [`HOST_MODULE`]. `Json` never crosses the host boundary as a
/// [`TransferType`] (section 9.6's transferable types do not include it), so
/// nothing here ever mints this index either; reserved so that the checker's
/// own `JSON_DECL_INDEX` and a host enum's id can never collide.
pub const JSON_INDEX: u32 = 1;

/// The first index [`TypeTable::add_enum`] hands out, leaving
/// [`RESULT_INDEX`] and [`JSON_INDEX`] reserved.
const FIRST_ENUM_INDEX: u32 = 2;

/// A transferable type (section 12.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferType {
    /// `Int`.
    Int,
    /// `Float`.
    Float,
    /// `Bool`.
    Bool,
    /// `String`.
    String,
    /// `Filter`, which may only be a parameter of a capability function.
    Filter,
    /// `[T]`.
    List(TypeRef),
    /// `Map<K, V>` with a key type of `Int`, `String` or `Bool`.
    Map(TypeRef, TypeRef),
    /// `Set<T>` with an element type of `Int`, `String` or `Bool`.
    Set(TypeRef),
    /// `T?`, which never wraps another optional.
    Optional(TypeRef),
    /// A record. The fields are sorted by name and the names are distinct.
    Record(Vec<(String, TypeRef)>),
    /// An enum, identified by [`EnumId`], not by name (section 10.4): two
    /// entries spelled the same are the same type only if `id` also matches.
    Enum {
        /// The declaration this entry names.
        id: EnumId,
        /// The enum name, for diagnostics and for the program to write.
        name: String,
        /// The variants, at least one.
        variants: Vec<VariantDef>,
    },
}

/// A problem with a type table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferError {
    /// The entry at fault.
    pub at: TypeRef,
    /// What is wrong.
    pub reason: &'static str,
}

/// A table of transferable types.
#[derive(Clone, Debug)]
pub struct TypeTable {
    types: Vec<TransferType>,
    /// The next index [`TypeTable::add_enum`] will mint within
    /// [`HOST_MODULE`], starting at [`FIRST_ENUM_INDEX`]. Not part of this
    /// table's own transferable content, so two tables built to hold the
    /// same entries by different sequences of calls still compare equal:
    /// excluded from `PartialEq` by hand below rather than derived.
    next_enum_index: u32,
}

impl PartialEq for TypeTable {
    fn eq(&self, other: &TypeTable) -> bool {
        self.types == other.types
    }
}

impl Eq for TypeTable {}

impl Default for TypeTable {
    fn default() -> TypeTable {
        TypeTable::new()
    }
}

/// Whether `name` and `variants` are section 8.2's `Result<T, E>` as a host
/// would spell it: exactly two variants, `Ok` then `Err`. Private to
/// [`TypeTable::add_enum`];
/// `hollowscript_check::effects::transfer_type_id` applies the identical
/// rule independently, since it runs in a crate this one does not depend on
/// and cannot call back into.
fn is_result_shaped(name: &str, variants: &[VariantDef]) -> bool {
    name == "Result" && matches!(variants, [ok, err] if ok.name == "Ok" && err.name == "Err")
}

impl TypeTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> TypeTable {
        TypeTable {
            types: Vec::new(),
            next_enum_index: FIRST_ENUM_INDEX,
        }
    }

    /// Adds a type and returns its reference. References inside the type may
    /// point at entries that are added later, which is how an enum refers to itself.
    pub fn add(&mut self, ty: TransferType) -> TypeRef {
        self.types.push(ty);
        TypeRef(u32::try_from(self.types.len() - 1).unwrap_or(u32::MAX))
    }

    /// Adds an enum a host declares directly (as opposed to one converted
    /// from a program's own `enum` declaration, which already has a
    /// `DeclId` to build an [`EnumId`] from): mints it a fresh
    /// [`EnumId`] within [`HOST_MODULE`], unique within this table, so that
    /// it is never mistaken for a program's own same-named declaration
    /// (section 10.4) or for another host enum's.
    ///
    /// The one exception is `Result` (section 8.2's own `Result<T, E>`, the
    /// enum every fallible capability returns): a name of `"Result"` with
    /// exactly the two variants `Ok` and `Err`, in that order, is given the
    /// fixed id `{ HOST_MODULE, RESULT_INDEX }` instead of a fresh one,
    /// which is what a program's own `Ok(...)`/`Err(...)` compiles to
    /// (`hollowscript_check::effects::transfer_type_id` recognises `Result`
    /// by this same name-and-shape rule and folds it into the checker's one
    /// builtin declaration, whose id is numerically the same). Without this,
    /// `hollowscript::hosttypes::register` would give a host-returned
    /// `Ok`/`Err` value a *different* `module.variants` entry than a
    /// program's own use of them, and a program's own `match ... { Ok(v) {
    /// } Err(e) { } }` on a capability's result would never match either arm
    /// (the same collision as an ordinary host-defined enum, just for the
    /// language's own enum
    /// rather than a host-defined one). A "Result" that does not have
    /// exactly this shape is not this enum (a host's own type may coincide
    /// in name) and is minted an ordinary fresh id like any other.
    pub fn add_enum(&mut self, name: impl Into<String>, variants: Vec<VariantDef>) -> TypeRef {
        let name = name.into();
        let id = if is_result_shaped(&name, &variants) {
            EnumId {
                module: HOST_MODULE,
                index: RESULT_INDEX,
            }
        } else {
            let index = self.next_enum_index;
            self.next_enum_index += 1;
            EnumId {
                module: HOST_MODULE,
                index,
            }
        };
        self.add(TransferType::Enum { id, name, variants })
    }

    /// The type of a reference.
    #[must_use]
    pub fn get(&self, r: TypeRef) -> Option<&TransferType> {
        self.types.get(r.0 as usize)
    }

    /// The entry at `r`, to replace it. Used when a reference must be
    /// reserved before the type it will hold can be built, which is how a
    /// recursive enum refers to itself.
    pub fn entry_mut(&mut self, r: TypeRef) -> Option<&mut TransferType> {
        self.types.get_mut(r.0 as usize)
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// All entries in order.
    #[must_use]
    pub fn entries(&self) -> &[TransferType] {
        &self.types
    }

    /// Checks the structure of every entry: references in range, key types,
    /// sorted and distinct record fields, non-empty enums with distinct
    /// variants that start with an upper case letter, and no nested optionals.
    ///
    /// # Errors
    ///
    /// Returns the first entry that is malformed.
    pub fn validate(&self) -> Result<(), TransferError> {
        for (index, ty) in self.types.iter().enumerate() {
            let at = TypeRef(u32::try_from(index).unwrap_or(u32::MAX));
            let fail = |reason| Err(TransferError { at, reason });
            let in_range = |r: TypeRef| (r.0 as usize) < self.types.len();
            let is_key = |r: TypeRef| {
                matches!(
                    self.get(r),
                    Some(TransferType::Int | TransferType::String | TransferType::Bool)
                )
            };
            match ty {
                TransferType::Int
                | TransferType::Float
                | TransferType::Bool
                | TransferType::String
                | TransferType::Filter => {}
                TransferType::List(t) => {
                    if !in_range(*t) {
                        return fail("a reference is out of range");
                    }
                }
                TransferType::Set(t) => {
                    if !in_range(*t) || !is_key(*t) {
                        return fail("a set element must be Int, String or Bool");
                    }
                }
                TransferType::Map(k, v) => {
                    if !in_range(*k) || !in_range(*v) || !is_key(*k) {
                        return fail("a map key must be Int, String or Bool");
                    }
                }
                TransferType::Optional(t) => {
                    if !in_range(*t) {
                        return fail("a reference is out of range");
                    }
                    if matches!(self.get(*t), Some(TransferType::Optional(_))) {
                        return fail("an optional does not nest");
                    }
                }
                TransferType::Record(fields) => {
                    if fields.is_empty() {
                        return fail("a record needs a field");
                    }
                    if fields.iter().any(|(_, t)| !in_range(*t)) {
                        return fail("a reference is out of range");
                    }
                    if fields.windows(2).any(|w| matches!(w, [a, b] if a.0 >= b.0)) {
                        return fail("record fields must be sorted and distinct");
                    }
                    if fields.iter().any(|(n, _)| n.is_empty() || n == "_") {
                        return fail("a field cannot be named _ or empty");
                    }
                }
                TransferType::Enum { variants, .. } => {
                    if variants.is_empty() {
                        return fail("an enum needs a variant");
                    }
                    let names: BTreeSet<&str> = variants.iter().map(|v| v.name.as_str()).collect();
                    if names.len() != variants.len() {
                        return fail("variant names must be distinct");
                    }
                    if variants.iter().any(|v| {
                        !v.name
                            .bytes()
                            .next()
                            .is_some_and(|b| b.is_ascii_uppercase())
                    }) {
                        return fail("a variant name starts with an upper case letter");
                    }
                    if variants
                        .iter()
                        .flat_map(|v| &v.payload)
                        .any(|t| !in_range(*t))
                    {
                        return fail("a reference is out of range");
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{TransferType, TypeRef, TypeTable, VariantDef};

    #[test]
    fn a_recursive_enum_is_valid() {
        let mut t = TypeTable::new();
        let int = t.add(TransferType::Int);
        let tree = TypeRef(2);
        let list = t.add(TransferType::List(tree));
        let e = t.add_enum(
            "Tree",
            vec![
                VariantDef {
                    name: "Leaf".into(),
                    payload: vec![],
                },
                VariantDef {
                    name: "Node".into(),
                    payload: vec![list, int],
                },
            ],
        );
        assert_eq!(e, tree);
        assert_eq!(t.validate(), Ok(()));
    }

    #[test]
    fn malformed_entries_are_reported_at_their_index() {
        let mut t = TypeTable::new();
        let f = t.add(TransferType::Float);
        let bad = t.add(TransferType::Set(f));
        let err = t.validate().unwrap_err();
        assert_eq!(err.at, bad);

        let mut t = TypeTable::new();
        let i = t.add(TransferType::Int);
        let o = t.add(TransferType::Optional(i));
        t.add(TransferType::Optional(o));
        assert_eq!(
            t.validate().unwrap_err().reason,
            "an optional does not nest"
        );

        let mut t = TypeTable::new();
        let i = t.add(TransferType::Int);
        t.add(TransferType::Record(vec![("b".into(), i), ("a".into(), i)]));
        assert_eq!(
            t.validate().unwrap_err().reason,
            "record fields must be sorted and distinct"
        );

        let mut t = TypeTable::new();
        t.add(TransferType::List(TypeRef(7)));
        assert!(t.validate().is_err());
    }
}
