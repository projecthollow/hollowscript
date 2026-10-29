//! Converting a checker [`TypeId`] into a transferable type (section 12.4).
//!
//! An export's signature is what a host reads to know how to call a function
//! or read a constant, and it is written in
//! [`hollowscript_core::transfer::TransferType`], which is a closed set: the
//! types a value can cross the host boundary as. Not every type of the
//! language is one. A function type is not transferable, so an exported
//! constant holding a closure is not exportable; a `Filter` is only ever a
//! capability parameter. A name that cannot be converted is left out of the
//! module's export table rather than reported, because section 10.4 lets a
//! module export a name for other *modules* to import whether or not a
//! *host* could name it, and refusing to compile the program would be the
//! wrong answer.

use hollowscript_bytecode::module::Module;
use hollowscript_check::types::{TypeId, TypeInterner, TypeView};
use hollowscript_core::Interner;
use hollowscript_core::transfer::{EnumId, TransferType, TypeRef, VariantDef};
use std::collections::BTreeMap;

/// Converts checker types into a module's own type table, reusing an entry
/// for a type already converted so that one type keeps one identity.
pub(crate) struct Transfer<'a> {
    types: &'a TypeInterner,
    symbols: &'a Interner,
    /// The module entry each converted type landed at.
    cache: BTreeMap<TypeId, TypeRef>,
    /// Types being converted, so that a recursive enum resolves to the entry
    /// reserved for it rather than recursing for ever.
    in_progress: BTreeMap<TypeId, TypeRef>,
}

impl<'a> Transfer<'a> {
    pub(crate) fn new(types: &'a TypeInterner, symbols: &'a Interner) -> Transfer<'a> {
        Transfer {
            types,
            symbols,
            cache: BTreeMap::new(),
            in_progress: BTreeMap::new(),
        }
    }

    /// `ty` as an entry of `module.types`, or `None` when it is not a
    /// transferable type.
    pub(crate) fn convert(&mut self, module: &mut Module, ty: TypeId) -> Option<TypeRef> {
        if let Some(&r) = self.cache.get(&ty) {
            return Some(r);
        }
        if let Some(&r) = self.in_progress.get(&ty) {
            return Some(r);
        }
        let converted = match self.types.view(ty) {
            TypeView::Int => TransferType::Int,
            TypeView::Float => TransferType::Float,
            TypeView::Bool => TransferType::Bool,
            TypeView::String => TransferType::String,
            TypeView::Filter => TransferType::Filter,
            TypeView::List(element) => {
                let element = self.convert(module, element)?;
                TransferType::List(element)
            }
            TypeView::Set(element) => {
                let element = self.convert(module, element)?;
                TransferType::Set(element)
            }
            TypeView::Map(key, value) => {
                let key = self.convert(module, key)?;
                let value = self.convert(module, value)?;
                TransferType::Map(key, value)
            }
            TypeView::Optional(inner) => {
                let inner = self.convert(module, inner)?;
                TransferType::Optional(inner)
            }
            TypeView::Record(fields) => {
                // A record's fields are sorted by name here and in the
                // checker, which is the order section 12.4 requires.
                let fields = fields.to_vec();
                let mut converted = Vec::with_capacity(fields.len());
                for (name, field_ty) in fields {
                    let field = self.convert(module, field_ty)?;
                    converted.push((self.symbols.resolve(name).to_owned(), field));
                }
                TransferType::Record(converted)
            }
            TypeView::Named { decl, args } => {
                let args = args.to_vec();
                let name = self
                    .types
                    .decl_name(decl)
                    .map(|s| self.symbols.resolve(s).to_owned())?;
                let shape = self.types.decl_shape(decl)?.clone();
                let hollowscript_check::types::DeclShape::Enum { variants, .. } = shape else {
                    // A record declaration is an alias for its structure,
                    // which `TypeInterner` expands, so reaching one here
                    // would be a checker defect rather than a program's.
                    return None;
                };
                // The entry is reserved before the payloads are converted,
                // so a variant that refers back to this enum resolves here.
                let reserved = TypeRef(u32::try_from(module.types.len()).ok()?);
                module.types.add(TransferType::Int);
                self.in_progress.insert(ty, reserved);
                let mut converted = Vec::with_capacity(variants.len());
                for variant in variants {
                    let mut payload = Vec::with_capacity(variant.payload.len());
                    for p in &variant.payload {
                        // The declaration's own parameters are substituted
                        // with this use's type arguments.
                        let instantiated = substitute(self.types, *p, decl, &args);
                        if let Some(r) = self.convert(module, instantiated) {
                            payload.push(r);
                        } else {
                            self.in_progress.remove(&ty);
                            return None;
                        }
                    }
                    payload.shrink_to_fit();
                    converted.push(VariantDef {
                        name: self.symbols.resolve(variant.name).to_owned(),
                        payload,
                    });
                }
                self.in_progress.remove(&ty);
                if let Some(slot) = module.types.entry_mut(reserved) {
                    // `decl` is the checker's own identity for this
                    // declaration (module and per-module index): carried
                    // through unchanged so that acceptance and rendering
                    // (section 10.4) see the same identity the
                    // checker used, rather than reconstructing one from the
                    // name, which two differently-declared enums can share.
                    *slot = TransferType::Enum {
                        id: EnumId {
                            module: decl.module,
                            index: decl.index,
                        },
                        name,
                        variants: converted,
                    };
                }
                self.cache.insert(ty, reserved);
                return Some(reserved);
            }
            // A function, a type parameter and the error type are not
            // values a host can be handed.
            TypeView::Function { .. } | TypeView::Param { .. } | TypeView::Error => return None,
        };
        let r = module.types.add(converted);
        self.cache.insert(ty, r);
        Some(r)
    }
}

/// `ty` with `decl`'s own parameters replaced by `args`.
///
/// `TypeInterner::substitute` is private to the checker and needs a mutable
/// interner; this reads the same shapes through the public view, which is
/// enough because an enum payload is a type the checker already interned.
fn substitute(
    types: &TypeInterner,
    ty: TypeId,
    decl: hollowscript_check::types::DeclId,
    args: &[TypeId],
) -> TypeId {
    match types.view(ty) {
        TypeView::Param { decl: d, index } if d == decl => {
            args.get(index as usize).copied().unwrap_or(ty)
        }
        _ => ty,
    }
}
