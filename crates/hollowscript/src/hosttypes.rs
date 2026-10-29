//! Registering the records and enums a host's own capabilities mention, so
//! that a capability function can build one.
//!
//! An enum value on the heap names its variant by an index into the
//! *module's* variant table, and a record names its fields by `StringId`s
//! into the module's string table. A host has neither, and neither can be
//! invented at the point of use: both tables belong to the compiled module
//! and are fixed by the time it runs. The compiler puts an entry there only
//! for a variant or a field set the *program* writes, which is the wrong
//! question: a program that calls `fs.read` and ignores the error never
//! writes `IoError.NotFound`, yet the capability has to be able to return
//! it.
//!
//! So this runs between compiling and verifying, where both the module and
//! the host's declared `CapabilityInterface` are in hand, and adds an entry
//! for every record and enum any capability's declared type mentions, at any
//! depth, that the module does not already have. It adds nothing else: a
//! type no capability mentions cannot be built by a host anyway, and the
//! verifier checks what comes out exactly as it checks the compiler's own
//! output.

use hollowscript_bytecode::module::{
    CapabilityKind, Module, Shape, Signature, StringId, VariantDesc,
};
use hollowscript_check::CapabilityInterface;
use hollowscript_core::transfer::{EnumId, TransferType, TypeRef, TypeTable};
use hollowscript_vm::HostTypes;

/// Adds to `module` every record shape and enum variant the capability
/// interface mentions, and returns where each one landed.
///
/// Silently adds nothing for a type that is already there, so a program that
/// writes `Ok(...)` itself and a capability that returns one share the entry.
#[must_use]
pub(crate) fn register(module: &mut Module, capabilities: &CapabilityInterface) -> HostTypes {
    let mut types = HostTypes::new();
    register_from(module, &capabilities.types, &mut types);
    types
}

/// Adds to `host_types` every record shape and enum variant `module`
/// *already* has in its own compiled type table, adding a shape or variant
/// to `module` only for one that table names but the module does not have
/// yet (the same rule [`register`] follows, applied to a different
/// source): a record or enum an exported function's own signature declares
/// is in this table (section 12's transferable-type requirement puts it
/// there whether or not the program ever builds a literal of it), so a
/// host reading or building one of `hollowscript-capi`'s `hs_call`
/// arguments or results can name its fields or its variant without that
/// type having had to also be part of some capability's own declared type.
/// Call this after [`register`], so the two share an entry for a type both
/// mention, the same way [`register`] itself already avoids a duplicate
/// for a type the program's own source and a capability both use.
pub(crate) fn register_module_types(module: &mut Module, host_types: &mut HostTypes) {
    // `module.types` is borrowed by the walk and mutated by `register_from`
    // (which may add to `module.shapes`/`module.variants`, not to
    // `module.types` itself), so the table walked is a snapshot; a type
    // `register_from` itself adds to `module.shapes`/`module.variants` is
    // not also a new entry of `module.types`, so nothing is missed by not
    // re-reading it.
    let types = module.types.clone();
    register_from(module, &types, host_types);
}

/// The record shapes and enum variants a module that is already compiled
/// and verified has, named as [`register_module_types`] names them, without
/// changing the module: for a container [`crate::build`] produced, which
/// ran [`register`] before it was encoded, so every type a capability can
/// build is already in the module's own tables.
pub(crate) fn of_module(module: &Module) -> HostTypes {
    let mut types = HostTypes::new();
    for (index, variant) in module.variants.iter().enumerate() {
        let Ok(position) = u32::try_from(index) else {
            continue;
        };
        types.add_variant(
            module.string(variant.enum_name),
            module.string(variant.name),
            position,
        );
    }
    for (index, shape) in module.shapes.iter().enumerate() {
        let Ok(position) = u16::try_from(index) else {
            continue;
        };
        // Stored sorted by name already (see `register_record`), which is
        // the order `HostTypes::add_shape` expects.
        let names = shape
            .fields
            .iter()
            .map(|&id| module.string(id).to_owned())
            .collect();
        let field_ids = shape.fields.iter().map(|id| id.0).collect();
        types.add_shape(names, position, field_ids);
    }
    types
}

/// The name of every capability `module` uses that `capabilities` does not
/// declare, or declares with a different signature for a function the
/// module has, in the module's own order.
pub(crate) fn ungranted_capabilities(
    module: &Module,
    capabilities: &CapabilityInterface,
) -> Vec<String> {
    use hollowscript_check::effects::CapabilityShape;
    let mut missing = Vec::new();
    for capability in &module.capabilities {
        let name = module.string(capability.name);
        let granted = capabilities
            .capabilities
            .iter()
            .find(|decl| decl.name == name)
            .is_some_and(|decl| match (&capability.kind, &decl.shape) {
                (
                    CapabilityKind::Function(signature),
                    CapabilityShape::Function { params, ret },
                ) => same_signature(module, signature, &capabilities.types, params, *ret),
                (CapabilityKind::Namespace(functions), CapabilityShape::Namespace(declared)) => {
                    functions.iter().all(|(function, signature)| {
                        let function = module.string(*function);
                        declared.iter().any(|(n, params, ret)| {
                            n == function
                                && same_signature(
                                    module,
                                    signature,
                                    &capabilities.types,
                                    params,
                                    *ret,
                                )
                        })
                    })
                }
                _ => false,
            });
        if !granted {
            missing.push(name.to_owned());
        }
    }
    missing
}

fn same_signature(
    module: &Module,
    signature: &Signature,
    host: &TypeTable,
    params: &[TypeRef],
    ret: Option<TypeRef>,
) -> bool {
    signature.params.len() == params.len()
        && signature
            .params
            .iter()
            .zip(params)
            .all(|(&a, &b)| same_type(&module.types, a, host, b, 0))
        && match (signature.ret, ret) {
            (None, None) => true,
            (Some(a), Some(b)) => same_type(&module.types, a, host, b, 0),
            _ => false,
        }
}

/// Whether entry `a` of table `ta` and entry `b` of table `tb` are the same
/// transferable type, compared by structure (an enum by its name and its
/// variants, not by the id either table gave it). Transferable types are
/// not recursive, so the depth bound is only a guard against a malformed
/// table: past it the answer is `false`.
fn same_type(ta: &TypeTable, a: TypeRef, tb: &TypeTable, b: TypeRef, depth: u32) -> bool {
    const MAX_DEPTH: u32 = 64;
    if depth > MAX_DEPTH {
        return false;
    }
    let depth = depth + 1;
    let same = |x: TypeRef, y: TypeRef| same_type(ta, x, tb, y, depth);
    match (ta.get(a), tb.get(b)) {
        (Some(TransferType::Int), Some(TransferType::Int))
        | (Some(TransferType::Float), Some(TransferType::Float))
        | (Some(TransferType::Bool), Some(TransferType::Bool))
        | (Some(TransferType::String), Some(TransferType::String))
        | (Some(TransferType::Filter), Some(TransferType::Filter)) => true,
        (Some(TransferType::List(x)), Some(TransferType::List(y)))
        | (Some(TransferType::Set(x)), Some(TransferType::Set(y)))
        | (Some(TransferType::Optional(x)), Some(TransferType::Optional(y))) => same(*x, *y),
        (Some(TransferType::Map(xk, xv)), Some(TransferType::Map(yk, yv))) => {
            same(*xk, *yk) && same(*xv, *yv)
        }
        (Some(TransferType::Record(xs)), Some(TransferType::Record(ys))) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|((xn, xt), (yn, yt))| xn == yn && same(*xt, *yt))
        }
        (
            Some(TransferType::Enum {
                name: xn,
                variants: xs,
                ..
            }),
            Some(TransferType::Enum {
                name: yn,
                variants: ys,
                ..
            }),
        ) => {
            xn == yn
                && xs.len() == ys.len()
                && xs.iter().zip(ys).all(|(x, y)| {
                    x.name == y.name
                        && x.payload.len() == y.payload.len()
                        && x.payload.iter().zip(&y.payload).all(|(p, q)| same(*p, *q))
                })
        }
        _ => false,
    }
}

/// Shared by [`register`] and [`register_module_types`]: walks every entry
/// of `source`, adding a record shape or an enum variant to `module` (if it
/// does not have one already) and naming it in `host_types`.
fn register_from(module: &mut Module, source: &TypeTable, host_types: &mut HostTypes) {
    // Every entry of the table, not only the ones a signature names
    // directly: an enum's payload and a list's element are entries of their
    // own, and a capability can return a list of records.
    for index in 0..source.len() {
        let Some(entry) = source.get(TypeRef(u32::try_from(index).unwrap_or(u32::MAX))) else {
            continue;
        };
        match entry {
            TransferType::Record(fields) => register_record(module, host_types, fields),
            TransferType::Enum { id, name, variants } => {
                for variant in variants {
                    register_variant(
                        module,
                        host_types,
                        *id,
                        name,
                        &variant.name,
                        u16::try_from(variant.payload.len()).unwrap_or(u16::MAX),
                    );
                }
            }
            _ => {}
        }
    }
}

/// The string id of `text` in `module`, reusing an existing entry.
fn string_of(module: &mut Module, text: &str) -> Option<StringId> {
    if let Some(index) = module.strings.iter().position(|s| s == text) {
        return u32::try_from(index).ok().map(StringId);
    }
    module.add_string(text).ok()
}

fn register_record(module: &mut Module, types: &mut HostTypes, fields: &[(String, TypeRef)]) {
    // A record's fields are stored sorted by name, which is the order a
    // field read by position uses.
    let mut names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    let mut ids = Vec::with_capacity(names.len());
    for name in &names {
        match string_of(module, name) {
            Some(id) => ids.push(id),
            // The string table is full: nothing can be registered, and a
            // host that tries to build this record is told so by name.
            None => return,
        }
    }
    let position = if let Some(index) = module.shapes.iter().position(|s| s.fields == ids) {
        index
    } else {
        module.shapes.push(Shape {
            fields: ids.clone(),
        });
        module.shapes.len() - 1
    };
    if let Ok(shape) = u16::try_from(position) {
        types.add_shape(names, shape, ids.into_iter().map(|id| id.0).collect());
    }
}

fn register_variant(
    module: &mut Module,
    types: &mut HostTypes,
    id: EnumId,
    enum_name: &str,
    variant_name: &str,
    arity: u16,
) {
    // Identity, not spelling, is what makes two entries the same
    // declaration (section 10.4): a program's own `enum IoError`
    // and a host-declared `IoError` share nothing just because they share
    // a name, so matching (and, worse, widening) an existing entry by name
    // alone would let a program's own enum silently absorb a host variant,
    // or the reverse. `decl_module`/`decl_index` on the existing entry are
    // compared against `id` instead.
    let existing = module.variants.iter().position(|v| {
        v.decl_module == id.module
            && v.decl_index == id.index
            && module.string(v.name) == variant_name
    });
    if let Some(index) = existing {
        if let Ok(position) = u32::try_from(index) {
            // The compiler sizes a variant by the widest use it saw; a
            // capability that carries a payload must not shrink it. This is
            // still safe now the match above is identity-scoped: widening
            // only ever touches a variant of this exact declaration.
            if let Some(desc) = module.variants.get_mut(index) {
                desc.arity = desc.arity.max(arity);
            }
            types.add_variant(enum_name, variant_name, position);
        }
        return;
    }
    let (Some(enum_id), Some(name_id)) = (
        string_of(module, enum_name),
        string_of(module, variant_name),
    ) else {
        return;
    };
    // The variant's position within its own enum is the order the declared
    // type lists them in, which is the order this loop walks; scoped to
    // this declaration's identity, not to every entry that happens to share
    // its name.
    let index = module
        .variants
        .iter()
        .filter(|v| v.decl_module == id.module && v.decl_index == id.index)
        .count();
    let Ok(within) = u16::try_from(index) else {
        return;
    };
    let position = module.variants.len();
    module.variants.push(VariantDesc {
        enum_name: enum_id,
        name: name_id,
        index: within,
        arity,
        decl_module: id.module,
        decl_index: id.index,
    });
    if let Ok(position) = u32::try_from(position) {
        types.add_variant(enum_name, variant_name, position);
    }
}

#[cfg(test)]
mod tests {
    use super::register;
    use hollowscript_bytecode::module::Module;
    use hollowscript_check::CapabilityInterface;
    use hollowscript_check::effects::{CapabilityDecl, CapabilityShape};
    use hollowscript_core::transfer::{
        EnumId, HOST_MODULE, RESULT_INDEX, TransferType, TypeTable, VariantDef,
    };
    use hollowscript_vm::HostTypes;

    /// A capability interface shaped like `fs.read`: a function returning
    /// `Result<String, IoError>`.
    fn fallible_interface() -> CapabilityInterface {
        let mut types = TypeTable::new();
        let string = types.add(TransferType::String);
        let io_error = types.add_enum(
            "IoError",
            vec![
                VariantDef {
                    name: "NotFound".to_owned(),
                    payload: vec![string],
                },
                VariantDef {
                    name: "Denied".to_owned(),
                    payload: vec![string],
                },
            ],
        );
        // `Result` is not minted a fresh id by `add_enum`: it is the
        // language's own enum (section 8.2), and every host declaring one
        // means the same declaration, at the reserved id the compiler also
        // uses for the program's own `Result` values (`RESULT_INDEX`).
        let result = types.add(TransferType::Enum {
            id: EnumId {
                module: HOST_MODULE,
                index: RESULT_INDEX,
            },
            name: "Result".to_owned(),
            variants: vec![
                VariantDef {
                    name: "Ok".to_owned(),
                    payload: vec![string],
                },
                VariantDef {
                    name: "Err".to_owned(),
                    payload: vec![io_error],
                },
            ],
        });
        CapabilityInterface {
            capabilities: vec![CapabilityDecl {
                name: "fs".to_owned(),
                shape: CapabilityShape::Namespace(vec![(
                    "read".to_owned(),
                    vec![string],
                    Some(result),
                )]),
            }],
            types,
        }
    }

    #[test]
    fn every_variant_a_capability_mentions_is_registered() {
        let mut module = Module::new();
        let types = register(&mut module, &fallible_interface());
        for (enum_name, variant) in [
            ("Result", "Ok"),
            ("Result", "Err"),
            ("IoError", "NotFound"),
            ("IoError", "Denied"),
        ] {
            let position = types
                .variant(enum_name, variant)
                .unwrap_or_else(|| panic!("{enum_name}.{variant} should be registered"));
            let desc = module
                .variants
                .get(position as usize)
                .expect("the position is in range");
            assert_eq!(module.string(desc.enum_name), enum_name);
            assert_eq!(module.string(desc.name), variant);
        }
    }

    /// A variant's position within its own enum is what `Op::Try` and
    /// `Op::VariantIs` compare, so `Ok` must be 0 and `Err` 1.
    #[test]
    fn a_variants_index_within_its_enum_follows_the_declared_order() {
        let mut module = Module::new();
        let types = register(&mut module, &fallible_interface());
        let at = |e: &str, v: &str| {
            let position = types.variant(e, v).expect("registered");
            module
                .variants
                .get(position as usize)
                .expect("in range")
                .index
        };
        assert_eq!(at("Result", "Ok"), 0);
        assert_eq!(at("Result", "Err"), 1);
        assert_eq!(at("IoError", "NotFound"), 0);
        assert_eq!(at("IoError", "Denied"), 1);
    }

    /// A variant the program already writes is shared, not duplicated: two
    /// entries for one variant would make two values that a `match` treats
    /// as different variants of the same enum.
    #[test]
    fn a_variant_the_module_already_has_is_reused() {
        let mut module = Module::new();
        let enum_name = module.add_string("Result").unwrap();
        let ok = module.add_string("Ok").unwrap();
        module
            .variants
            .push(hollowscript_bytecode::module::VariantDesc {
                enum_name,
                name: ok,
                index: 0,
                arity: 1,
                // Matches the id `fallible_interface`'s `Result` entry
                // carries, so this pre-existing entry is the same
                // declaration and gets reused (see the test below for the
                // opposite case, where it is not).
                decl_module: HOST_MODULE,
                decl_index: RESULT_INDEX,
            });
        let before = module.variants.len();
        let types = register(&mut module, &fallible_interface());
        assert_eq!(types.variant("Result", "Ok"), Some(0));
        // Ok was reused; Err, NotFound and Denied are new.
        assert_eq!(module.variants.len(), before + 3);
    }

    #[test]
    fn an_interface_with_no_records_or_enums_registers_nothing() {
        let mut types = TypeTable::new();
        let string = types.add(TransferType::String);
        let interface = CapabilityInterface {
            capabilities: vec![CapabilityDecl {
                name: "print".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![string],
                    ret: None,
                },
            }],
            types,
        };
        let mut module = Module::new();
        assert!(register(&mut module, &interface).is_empty());
        assert!(module.variants.is_empty());
        assert!(module.shapes.is_empty());
    }

    /// The bug this guards against: a record used only as an exported
    /// function's *parameter* type, never built as a literal anywhere in
    /// the program's own source, still ends up in `module.types` (the
    /// compiler puts every transferable signature type there per section
    /// 12.4), but `module.shapes` has no entry for it until something adds
    /// one. A host reading such an argument off `hs_call` needs the shape
    /// registered all the same.
    #[test]
    fn a_record_named_only_in_a_signature_still_gets_a_shape() {
        use super::register_module_types;

        let mut module = Module::new();
        let int = module.types.add(TransferType::Int);
        // Nothing here adds a `module.shapes` entry directly: this mirrors
        // what the compiler leaves behind for a parameter type that no
        // literal in the program ever constructs.
        module.types.add(TransferType::Record(vec![
            ("x".to_owned(), int),
            ("y".to_owned(), int),
        ]));
        assert!(module.shapes.is_empty());

        let mut host_types = HostTypes::new();
        register_module_types(&mut module, &mut host_types);

        assert_eq!(module.shapes.len(), 1);
        let shape = module.shapes.first().expect("one shape");
        let names: Vec<&str> = shape.fields.iter().map(|&id| module.string(id)).collect();
        assert_eq!(names, ["x", "y"]);
    }

    /// The same, for an enum variant named only in a signature.
    #[test]
    fn a_variant_named_only_in_a_signature_still_gets_registered() {
        use super::register_module_types;

        let mut module = Module::new();
        module.types.add_enum(
            "Quadrant",
            vec![
                VariantDef {
                    name: "Origin".to_owned(),
                    payload: vec![],
                },
                VariantDef {
                    name: "Elsewhere".to_owned(),
                    payload: vec![],
                },
            ],
        );
        assert!(module.variants.is_empty());

        let mut host_types = HostTypes::new();
        register_module_types(&mut module, &mut host_types);

        assert_eq!(module.variants.len(), 2);
        assert!(host_types.variant("Quadrant", "Origin").is_some());
        assert!(host_types.variant("Quadrant", "Elsewhere").is_some());
    }

    /// A capability interface offering two distinct enums that
    /// happen to share a spelled name (`Colour`, declared twice with
    /// different variants, exactly as two independently loaded modules
    /// might) must register two independent `module.variants` entries, not
    /// widen or merge them, because doing so is how a host's own enum
    /// could be confused with an unrelated program declaration of the same
    /// name (section 10.4).
    #[test]
    fn two_same_named_enums_from_different_declarations_stay_distinct() {
        let mut types = TypeTable::new();
        let int = types.add(TransferType::Int);
        let colour_a = types.add_enum(
            "Colour",
            vec![
                VariantDef {
                    name: "Red".to_owned(),
                    payload: vec![],
                },
                VariantDef {
                    name: "Green".to_owned(),
                    payload: vec![],
                },
            ],
        );
        let colour_b = types.add_enum(
            "Colour",
            vec![VariantDef {
                name: "Green".to_owned(),
                payload: vec![int],
            }],
        );
        let interface = CapabilityInterface {
            capabilities: vec![CapabilityDecl {
                name: "paint".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![colour_a, colour_b],
                    ret: None,
                },
            }],
            types,
        };
        let mut module = Module::new();
        let _ = register(&mut module, &interface);
        // Two entries for "Green": one from each declaration, at arity 0
        // and arity 1. Before the fix, the second registration found the
        // first by name and widened it in place instead, leaving one
        // shared, wrongly-arity-1 "Green" that both declarations' code
        // would read as.
        let greens: Vec<&hollowscript_bytecode::module::VariantDesc> = module
            .variants
            .iter()
            .filter(|v| module.string(v.name) == "Green")
            .collect();
        assert_eq!(greens.len(), 2, "each declaration's Green is its own entry");
        let arities: Vec<u16> = greens.iter().map(|v| v.arity).collect();
        assert!(
            arities.contains(&0),
            "colour_a's payload-free Green is untouched: {arities:?}"
        );
        assert!(
            arities.contains(&1),
            "colour_b's one-payload Green is untouched: {arities:?}"
        );
    }

    #[test]
    fn a_record_a_capability_mentions_gets_a_shape() {
        let mut types = TypeTable::new();
        let string = types.add(TransferType::String);
        let int = types.add(TransferType::Int);
        let record = types.add(TransferType::Record(vec![
            ("status".to_owned(), int),
            ("text".to_owned(), string),
        ]));
        let interface = CapabilityInterface {
            capabilities: vec![CapabilityDecl {
                name: "http".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![string],
                    ret: Some(record),
                },
            }],
            types,
        };
        let mut module = Module::new();
        let host = register(&mut module, &interface);
        assert!(!host.is_empty());
        assert_eq!(module.shapes.len(), 1);
        let shape = module.shapes.first().expect("one shape");
        let names: Vec<&str> = shape.fields.iter().map(|&id| module.string(id)).collect();
        assert_eq!(names, ["status", "text"]);
    }
}
