//! Effects and capabilities (chapter 9).
//!
//! Effect is a boolean carried by every function type
//! ([`crate::types::TypeInterner::function`]'s `effect` flag): a call whose
//! callee has an effectful type must be marked `effect` (HS0401), and `effect`
//! on anything else is an error (HS0402, HS0403); [`check_effect_marker`] is
//! that check, called once per call expression by [`crate::infer`]. This
//! module also holds the grant side: the interface a host declares for a run
//! (section 9.6), turning it into checker types once for the whole program
//! ([`capability_types`]), and the checked declaration of `uses` for one file
//! (section 9.2, [`check_uses`]).

use crate::types::{DeclId, DeclShape, Field, TypeId, TypeInterner, Variant};
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable};
use hollowscript_core::{BoundedDiagnostics, Code, Diagnostic, Interner, Source, Span, Symbol};
use hollowscript_syntax::ast::UsesLine;
use std::collections::BTreeMap;

/// The shape of one capability a host offers: a single function, or a
/// namespace record of functions (section 9.1).
#[derive(Clone, Debug)]
pub enum CapabilityShape {
    /// A single effectful function, with its parameter and result types
    /// referring to the interface's shared [`TypeTable`].
    Function {
        /// The parameter types.
        params: Vec<TypeRef>,
        /// The result type, if it returns one.
        ret: Option<TypeRef>,
    },
    /// A namespace: a fixed set of named functions, each with its own
    /// signature, such as `clock` with `now` and `today`.
    Namespace(Vec<(String, Vec<TypeRef>, Option<TypeRef>)>),
}

/// One capability a host declares for a run (section 9.6): its `uses` name
/// and its shape, described with the transferable types of section 12.4.
/// Names are plain text here, so that a host can build this before any
/// source has been read and any name interned; the checker interns them
/// itself when it compares them with a `uses` line.
#[derive(Clone, Debug)]
pub struct CapabilityDecl {
    /// The capability's `uses` name.
    pub name: String,
    /// Its shape.
    pub shape: CapabilityShape,
}

/// The capability interface of a run (section 9.7): what a host makes
/// available, given to the checker before it runs. This is
/// [`crate::CapabilityInterface`]'s own representation; the facade re-exports
/// it under that name.
#[derive(Clone, Debug, Default)]
pub struct Grants {
    /// The declared capabilities, in the order a host listed them; a
    /// capability's position here is its [`crate::hir::CapabilityId`].
    pub capabilities: Vec<CapabilityDecl>,
    /// The transferable types that the capabilities' signatures refer to.
    pub types: TypeTable,
}

impl Grants {
    /// The capability named `name` and its [`crate::hir::CapabilityId`], if
    /// the host granted one.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<(crate::hir::CapabilityId, &CapabilityDecl)> {
        self.capabilities
            .iter()
            .enumerate()
            .find(|(_, c)| c.name == name)
            .map(|(i, c)| {
                (
                    crate::hir::CapabilityId(u32::try_from(i).unwrap_or(u32::MAX)),
                    c,
                )
            })
    }
}

/// Converts one transferable type into a checker [`TypeId`], memoised by
/// `cache` so that a type reached from more than one capability, or through a
/// cycle (a recursive enum crossing the host boundary, section 9.6), is built
/// once and shares its identity.
///
/// An enum's [`DeclId`] is not minted here: it is read straight off the
/// entry's own `id` ([`hollowscript_core::transfer::EnumId`]), which
/// [`TypeTable::add_enum`](hollowscript_core::transfer::TypeTable::add_enum)
/// assigned when the host built its capability interface, long before any
/// program exists to check. Minting a fresh id here, by the order this walk
/// happens to visit entries in, is what let two same-named enums collide:
/// this function's own
/// visiting order (by a capability's parameters and result, not the table's
/// storage order) has no reason to agree with
/// `hollowscript::hosttypes::register`'s (by table storage order), so the two
/// independently-counted ids for what was meant to be one declaration could,
/// and did, disagree; reading the one id the entry already carries removes
/// the disagreement rather than trying to keep two counters in step.
/// `EnumId`'s own `HOST_MODULE` and this module's `PRELUDE_MODULE` are both
/// `u32::MAX` for exactly this reason: no translation is needed between the
/// two crates' identical ids.
fn transfer_type_id(
    r: TypeRef,
    table: &TypeTable,
    types: &mut TypeInterner,
    names: &mut Interner,
    cache: &mut BTreeMap<TypeRef, TypeId>,
    declared: &mut Vec<(String, DeclId)>,
) -> TypeId {
    if let Some(&id) = cache.get(&r) {
        return id;
    }
    let Some(entry) = table.get(r).cloned() else {
        return types.error();
    };
    let id = match entry {
        TransferType::Int => types.int(),
        TransferType::Float => types.float(),
        TransferType::Bool => types.bool(),
        TransferType::String => types.string(),
        TransferType::Filter => types.filter(),
        TransferType::List(e) => {
            let e = transfer_type_id(e, table, types, names, cache, declared);
            types.list(e)
        }
        TransferType::Set(e) => {
            let e = transfer_type_id(e, table, types, names, cache, declared);
            types.set(e)
        }
        TransferType::Map(k, v) => {
            let k = transfer_type_id(k, table, types, names, cache, declared);
            let v = transfer_type_id(v, table, types, names, cache, declared);
            types.map(k, v)
        }
        TransferType::Optional(inner) => {
            let inner = transfer_type_id(inner, table, types, names, cache, declared);
            types.optional(inner)
        }
        TransferType::Record(fields) => {
            let fields = fields
                .into_iter()
                .map(|(name, ty)| Field {
                    name: names.intern(&name),
                    ty: transfer_type_id(ty, table, types, names, cache, declared),
                })
                .collect();
            types.record(names, fields)
        }
        TransferType::Enum {
            id: enum_id,
            name: enum_name,
            variants,
        } => {
            // `Result` is the language's own enum, not a nominal type of the
            // host's: section 8.2 defines one `Result<T, E>`, and a
            // capability that returns one must return that, or a program
            // could not `match` it with `Ok` and `Err` or use `?` on it. This
            // is recognised by name and shape alone; `enum_id` plays no part
            // in it, so what id a host-declared `Result` entry happens to
            // carry makes no difference.
            if enum_name == "Result" {
                if let [ok, err] = variants.as_slice() {
                    if ok.name == "Ok" && err.name == "Err" {
                        let ok_ty = match ok.payload.first() {
                            Some(&p) => transfer_type_id(p, table, types, names, cache, declared),
                            None => types.error(),
                        };
                        let err_ty = match err.payload.first() {
                            Some(&p) => transfer_type_id(p, table, types, names, cache, declared),
                            None => types.error(),
                        };
                        let id = types.result(names, ok_ty, err_ty);
                        cache.insert(r, id);
                        return id;
                    }
                }
            }
            let decl = DeclId {
                module: enum_id.module,
                index: enum_id.index,
            };
            // Registered before its payloads are converted, so a variant that
            // refers back to this same entry (a recursive enum) resolves to
            // this id rather than recursing forever.
            let placeholder = types.named(decl, Vec::new());
            cache.insert(r, placeholder);
            let variants: Vec<Variant> = variants
                .into_iter()
                .map(|v| Variant {
                    name: names.intern(&v.name),
                    payload: v
                        .payload
                        .into_iter()
                        .map(|p| transfer_type_id(p, table, types, names, cache, declared))
                        .collect(),
                })
                .collect();
            let sym = names.intern(&enum_name);
            types.declare(
                decl,
                sym,
                DeclShape::Enum {
                    params: Vec::new(),
                    variants,
                },
            );
            // A program has to be able to write this type's name to match on
            // one (`Err(IoError.NotFound(message))`), so the declaration is
            // published for the checker's type scope to pick up.
            declared.push((enum_name.clone(), decl));
            return placeholder;
        }
    };
    cache.insert(r, id);
    id
}

/// Computes the checker type of every granted capability, once for the whole
/// program (section 9.6): a [`CapabilityShape::Function`] becomes an
/// effectful function type, and a [`CapabilityShape::Namespace`] becomes a
/// record whose fields are effectful function types. The result is indexed
/// by [`crate::hir::CapabilityId`].
pub fn capability_types(
    grants: &Grants,
    types: &mut TypeInterner,
    names: &mut Interner,
) -> CapabilityTypes {
    let mut cache = BTreeMap::new();
    let mut declared: Vec<(String, DeclId)> = Vec::new();
    let mut convert = |r: TypeRef,
                       types: &mut TypeInterner,
                       names: &mut Interner,
                       declared: &mut Vec<(String, DeclId)>| {
        transfer_type_id(r, &grants.types, types, names, &mut cache, declared)
    };
    let by_capability = grants
        .capabilities
        .iter()
        .map(|decl| match &decl.shape {
            CapabilityShape::Function { params, ret } => {
                let params = params
                    .iter()
                    .map(|p| convert(*p, types, names, &mut declared))
                    .collect();
                let ret = ret.map(|r| convert(r, types, names, &mut declared));
                types.function(true, params, ret)
            }
            CapabilityShape::Namespace(members) => {
                let fields = members
                    .iter()
                    .map(|(name, params, ret)| {
                        let params: Vec<TypeId> = params
                            .iter()
                            .map(|p| convert(*p, types, names, &mut declared))
                            .collect();
                        let ret = ret.map(|r| convert(r, types, names, &mut declared));
                        Field {
                            name: names.intern(name),
                            ty: types.function(true, params, ret),
                        }
                    })
                    .collect();
                types.record(names, fields)
            }
        })
        .collect();
    CapabilityTypes {
        by_capability,
        host_types: declared,
    }
}

/// What [`capability_types`] computed for one run.
#[derive(Clone, Debug, Default)]
pub struct CapabilityTypes {
    /// The checker type of every granted capability, indexed by
    /// [`crate::hir::CapabilityId`].
    pub by_capability: Vec<TypeId>,
    /// Every enum a capability's declared type mentions, by the name a
    /// program writes it as. A host's `IoError` is a type the program never
    /// declares, so nothing else would put it in scope.
    pub host_types: Vec<(String, DeclId)>,
}

/// The section 16.1 types a standard capability's signature mentions
/// (section 16.10's table): what a host that grants it declares. Used only to
/// keep an ungranted capability's HS0404 from cascading into HS0208 when a
/// program names one of these types.
#[must_use]
pub fn host_types_of_standard_capability(name: &str) -> &'static [&'static str] {
    match name {
        "fs" | "stdin" => &["IoError"],
        "http" => &["HttpError", "HttpRequest", "HttpResponse"],
        _ => &[],
    }
}

/// One capability name resolved from a file's `uses` lines (section 9.2).
#[derive(Clone, Debug)]
pub struct UsesEntry {
    /// The name as written.
    pub name: Symbol,
    /// The span of the name.
    pub span: Span,
    /// The capability it resolved to, absent when the host has not granted
    /// it (HS0404), in which case `ty` is the error type so that no further
    /// diagnostic cascades from using the name (section 2.11).
    pub capability: Option<crate::hir::CapabilityId>,
    /// The capability's type: an effectful function or a namespace record of
    /// them, or the error type when unresolved.
    pub ty: TypeId,
}

/// The outcome of checking one file's `uses` lines.
#[derive(Debug, Default)]
pub struct UsesResult {
    /// One entry per name, across every `uses` line, in the order written.
    pub entries: Vec<UsesEntry>,
    /// HS0404 for a name the host has not granted, and HS0201 for a name
    /// repeated across the file's own `uses` lines.
    pub diagnostics: BoundedDiagnostics,
}

/// Checks a file's `uses` lines against a run's grants (section 9.2): every
/// name must be one the host granted (HS0404 otherwise), and a name may not
/// repeat across the lines (HS0201). `capability_types` is
/// [`capability_types`]'s result for the whole program. The caller declares
/// each entry into the file's value scope in the order returned, which is
/// what then makes a capability name unshadowable by anything else in the
/// file (section 4.6): nothing declared after it can reuse the name, because
/// by the time anything else is declared, these entries already occupy it.
#[must_use]
pub fn check_uses(
    lines: &[UsesLine],
    grants: &Grants,
    capability_types: &[TypeId],
    types: &mut TypeInterner,
    names: &Interner,
    source: &Source,
) -> UsesResult {
    let mut result = UsesResult::default();
    let mut first_seen: BTreeMap<Symbol, Span> = BTreeMap::new();
    for line in lines {
        for ident in &line.names {
            if let Some(&first) = first_seen.get(&ident.name) {
                // Dropped rather than declared a second time: the caller
                // turns every entry into a scope declaration, and the first
                // occurrence already owns the name, so a second entry here
                // would either shadow it or double-report HS0201 once the
                // caller's own no-shadowing check saw it too.
                result.diagnostics.push(
                    Diagnostic::new(Code::HS0201, &[("name", &names.resolve(ident.name))])
                        .with_location(source.locate(ident.span))
                        .with_related(source.locate(first), "first declared here"),
                );
                continue;
            }
            first_seen.insert(ident.name, ident.span);
            let text = names.resolve(ident.name);
            if let Some((capability, _)) = grants.get(text) {
                let ty = capability_types
                    .get(capability.0 as usize)
                    .copied()
                    .unwrap_or_else(|| types.error());
                result.entries.push(UsesEntry {
                    name: ident.name,
                    span: ident.span,
                    capability: Some(capability),
                    ty,
                });
            } else {
                result.diagnostics.push(
                    Diagnostic::new(Code::HS0404, &[("name", &text)])
                        .with_location(source.locate(ident.span)),
                );
                result.entries.push(UsesEntry {
                    name: ident.name,
                    span: ident.span,
                    capability: None,
                    ty: types.error(),
                });
            }
        }
    }
    result
}

/// Whether a call is marked `effect` correctly for its callee's type
/// (section 9.3): every call through an effectful callee must be marked
/// (HS0401), and `effect` on a call through a pure callee is an error
/// (HS0402); `effect` on anything but a call is HS0403, checked by the
/// caller before this is reached, since only a caller that has already
/// parsed an `Effect` postfix chain knows whether its inner expression was a
/// call at all.
#[must_use]
pub fn check_effect_marker(
    callee_is_effectful: bool,
    marked_effect: bool,
) -> Option<EffectMarkerError> {
    match (callee_is_effectful, marked_effect) {
        (true, false) => Some(EffectMarkerError::MissingEffect),
        (false, true) => Some(EffectMarkerError::UnexpectedEffect),
        _ => None,
    }
}

/// What is wrong with a call's `effect` marker.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EffectMarkerError {
    /// The callee is effectful but the call was not marked (HS0401).
    MissingEffect,
    /// `effect` marks a call through a pure callee (HS0402).
    UnexpectedEffect,
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilityDecl, CapabilityShape, EffectMarkerError, Grants, capability_types,
        check_effect_marker, check_uses,
    };
    use crate::types::{DeclShape, TypeInterner, TypeView};
    use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable, VariantDef};
    use hollowscript_core::{Interner, Source, SourceId, Span};
    use hollowscript_syntax::ast::{Ident, NodeId, UsesLine};

    fn ident(names: &mut Interner, text: &str, start: u32) -> Ident {
        Ident {
            name: names.intern(text),
            span: Span::new(start, start + u32::try_from(text.len()).unwrap_or(0)),
        }
    }

    #[test]
    fn a_grant_is_found_by_name_and_keeps_its_position_as_the_capability_id() {
        let grants = Grants {
            capabilities: vec![
                CapabilityDecl {
                    name: "print".to_owned(),
                    shape: CapabilityShape::Function {
                        params: vec![],
                        ret: None,
                    },
                },
                CapabilityDecl {
                    name: "clock".to_owned(),
                    shape: CapabilityShape::Namespace(vec![]),
                },
            ],
            types: TypeTable::new(),
        };
        let (id, decl) = grants.get("clock").unwrap();
        assert_eq!(id.0, 1);
        assert_eq!(decl.name, "clock");
        assert!(grants.get("fs").is_none());
    }

    #[test]
    fn a_namespace_capability_becomes_a_record_of_effectful_functions() {
        let mut table = TypeTable::new();
        let string_ref = table.add(TransferType::String);
        let grants = Grants {
            capabilities: vec![CapabilityDecl {
                name: "clock".to_owned(),
                shape: CapabilityShape::Namespace(vec![(
                    "now".to_owned(),
                    vec![],
                    Some(string_ref),
                )]),
            }],
            types: table,
        };
        let mut types = TypeInterner::new();
        let mut names = Interner::new();
        let result = capability_types(&grants, &mut types, &mut names);
        let now_field = names.intern("now");
        assert!(matches!(
            types.view(result.by_capability[0]),
            TypeView::Record(fields) if fields.iter().any(|(n, _)| *n == now_field)
        ));
    }

    #[test]
    fn a_recursive_enum_crossing_the_boundary_shares_one_identity() {
        // The list element refers to the enum's own future index, exactly as
        // `hollowscript_core::transfer`'s own recursive-enum test does.
        let mut table = TypeTable::new();
        let tree_ref = TypeRef(1);
        let list = table.add(TransferType::List(tree_ref));
        let tree = table.add_enum(
            "Tree",
            vec![VariantDef {
                name: "Node".to_owned(),
                payload: vec![list],
            }],
        );
        assert_eq!(tree, tree_ref);
        let grants = Grants {
            capabilities: vec![CapabilityDecl {
                name: "store".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![],
                    ret: Some(tree),
                },
            }],
            types: table,
        };
        let mut types = TypeInterner::new();
        let mut names = Interner::new();
        let result = capability_types(&grants, &mut types, &mut names);
        let TypeView::Function { ret: Some(ret), .. } = types.view(result.by_capability[0]) else {
            unreachable!("store is a function capability")
        };
        let (decl, _) = types.declaration_of(ret).unwrap();
        let DeclShape::Enum { variants, .. } = types.decl_shape(decl).unwrap() else {
            unreachable!("Tree is an enum")
        };
        // Node's payload is a list of Tree, and that Tree is the very same
        // type as the enum it sits inside, not a second, unequal copy of it.
        let TypeView::List(element) = types.view(variants[0].payload[0]) else {
            unreachable!("Node's payload is a list")
        };
        assert_eq!(element, ret);
    }

    #[test]
    fn uses_reports_hs0404_for_an_ungranted_name_and_hs0201_for_a_repeat() {
        let mut names = Interner::new();
        let mut types = TypeInterner::new();
        let print = ident(&mut names, "print", 0);
        let repeat = ident(&mut names, "print", 10);
        let missing = ident(&mut names, "fs", 20);
        let line = UsesLine {
            id: NodeId(0),
            span: Span::new(0, 30),
            names: vec![print, repeat, missing],
        };
        let grants = Grants {
            capabilities: vec![CapabilityDecl {
                name: "print".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![],
                    ret: None,
                },
            }],
            types: TypeTable::new(),
        };
        let capability_types = capability_types(&grants, &mut types, &mut names);
        let source = Source::new(SourceId(0), "main.hws", "uses print, print, fs\n").unwrap();
        let result = check_uses(
            std::slice::from_ref(&line),
            &grants,
            &capability_types.by_capability,
            &mut types,
            &names,
            &source,
        );
        assert_eq!(
            result.entries.len(),
            2,
            "the repeated name is reported, not re-declared"
        );
        assert!(result.entries[0].capability.is_some());
        assert!(result.entries[1].capability.is_none());
        let codes: Vec<_> = result.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            vec![
                hollowscript_core::Code::HS0201,
                hollowscript_core::Code::HS0404
            ]
        );
    }

    #[test]
    fn the_effect_marker_must_match_the_callees_effect() {
        assert_eq!(
            check_effect_marker(true, false),
            Some(EffectMarkerError::MissingEffect)
        );
        assert_eq!(
            check_effect_marker(false, true),
            Some(EffectMarkerError::UnexpectedEffect)
        );
        assert_eq!(check_effect_marker(true, true), None);
        assert_eq!(check_effect_marker(false, false), None);
    }
}
