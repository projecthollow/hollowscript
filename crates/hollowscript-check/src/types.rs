//! Type representation and interning (chapter 2).
//!
//! Every type the checker produces is a [`TypeId`], an index into a
//! [`TypeInterner`] that is shared by one program. Two types are identical
//! exactly when their ids are equal, which is what section 2.5 requires:
//! "identity is a comparison of the same node, not a walk of the whole type".
//! The interner deduplicates structurally, so `Int` has one id everywhere and
//! two occurrences of `{ x: Int, y: Int }` share one id however they were built.
//!
//! Generic declarations are not expanded here: a record or enum type keeps its
//! own type arguments as a list of [`TypeId`]s, so a value of `Pair<Int, Int>`
//! applied to itself does not, by itself, create new nodes; substitution
//! (instantiating a declaration's parameters with arguments) is what section
//! 2.5's *expanded size* counts, and [`TypeInterner::expanded_size`] computes
//! it the same way: by walking the substituted structure with sharing ignored
//! and the count saturating, so that it can be compared against the limit of
//! 10,000 nodes (HS0335) without the walk itself needing to be bounded first.

use hollowscript_core::{Interner, Symbol};
use std::collections::BTreeMap;

/// An interned type. Comparing two ids is comparing two types.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(u32);

/// An unbound type variable created during the inference of one call
/// (section 2.10). Distinct from [`TypeId`] because a variable is a
/// placeholder for unification, not a type of the program.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeVarId(pub u32);

/// The identity of a `record` or `enum` declaration: the module that declares
/// it and its name. Two declarations of the same name in different modules
/// are different types (section 6.3); a record is additionally structural
/// (section 2.5), so two record declarations with the same fields are the
/// same type regardless of this identity, but an enum is nominal and uses it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeclId {
    /// The declaring module, as an index into the program's module list, or
    /// [`PRELUDE_MODULE`] for a declaration the checker itself owns (`Result`).
    pub module: u32,
    /// A per-module counter, assigned in declaration order.
    pub index: u32,
}

/// The pseudo-module index of a declaration the checker owns rather than a
/// program module (`Result`, section 2.2): out of range of any real module,
/// since a program has at most
/// [`hollowscript_core::limits::structural::MAX_MODULES`].
pub const PRELUDE_MODULE: u32 = u32::MAX;

/// One field of a record type, structural and sorted by name when a record
/// type is built ([`TypeInterner::record`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// The field name.
    pub name: Symbol,
    /// The field's type.
    pub ty: TypeId,
}

/// One variant of an enum type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    /// The variant name.
    pub name: Symbol,
    /// The payload types, by position.
    pub payload: Vec<TypeId>,
}

/// A borrowed view of a type's structure (section 2.2), for a caller outside
/// this module that needs to match on it: inference and unification walk a
/// type's shape this way rather than through the private `Shape` itself, so
/// that `Shape` stays free to add an interning-only detail without every
/// caller needing to change.
#[derive(Copy, Clone, Debug)]
pub enum TypeView<'a> {
    /// `Int`.
    Int,
    /// `Float`.
    Float,
    /// `Bool`.
    Bool,
    /// `String`.
    String,
    /// `Filter`.
    Filter,
    /// `[element]`.
    List(TypeId),
    /// `Map<key, value>`.
    Map(TypeId, TypeId),
    /// `Set<element>`.
    Set(TypeId),
    /// `inner?`.
    Optional(TypeId),
    /// A record's fields, sorted by resolved name text.
    Record(&'a [(Symbol, TypeId)]),
    /// A named record or enum applied to its type arguments (section 6.3);
    /// after [`TypeInterner::expand_record`] a record no longer appears this
    /// way, so in practice this is an enum once the checker has resolved a
    /// type expression, though nothing here enforces that.
    Named {
        /// The declaration.
        decl: DeclId,
        /// The type arguments, empty for a non-generic declaration.
        args: &'a [TypeId],
    },
    /// `fn(params): ret`, or `effect fn(params): ret`.
    Function {
        /// Whether the type is effectful.
        effect: bool,
        /// The parameter types.
        params: &'a [TypeId],
        /// The return type, absent for a function that returns nothing.
        ret: Option<TypeId>,
    },
    /// The type parameter at `index` of `decl`, opaque within it, or the
    /// unification variable of that same position when `parameter` is the
    /// callee's own declared type of the call currently being checked
    /// (section 2.10): which of the two it is depends on who is asking, not
    /// on the type itself, so [`crate::infer::unify`] is the only caller that
    /// treats it as a variable.
    Param {
        /// The declaration that introduces the parameter.
        decl: DeclId,
        /// Its position in the declaration's parameter list.
        index: u32,
    },
    /// An error already reported (section 2.11).
    Error,
}

/// The shape of a type, before interning. Two shapes that are `==` intern to
/// the same id, except for [`Shape::Error`], each occurrence of which is
/// distinct, so that one mistake never silently unifies with another.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Shape {
    Int,
    Float,
    Bool,
    String,
    Filter,
    List(TypeId),
    Map(TypeId, TypeId),
    Set(TypeId),
    Optional(TypeId),
    /// Fields sorted by name, which is what makes two record literals with
    /// the same fields in different orders intern to one id.
    Record(Vec<(Symbol, TypeId)>),
    /// A named record or enum, applied to its type arguments if generic. The
    /// interner keeps this distinct from `Record` so that an enum's identity
    /// survives even when, for one instantiation, its structure happens to
    /// coincide with another type; only [`TypeInterner::expand_record`] unfolds it.
    Named {
        decl: DeclId,
        args: Vec<TypeId>,
    },
    Function {
        effect: bool,
        params: Vec<TypeId>,
        ret: Option<TypeId>,
    },
    /// A type parameter, opaque within the declaration that introduces it
    /// (section 2.10). Distinguished by the declaration and its position so
    /// that two declarations sharing a parameter name do not share a type.
    Param {
        decl: DeclId,
        index: u32,
    },
    /// One error already reported; assignable to and from everything
    /// (section 2.11), and never merged with another `Error`.
    Error(u32),
}

/// What kind of declaration a [`DeclId`] names, needed to expand a `Named`
/// type and to compute its expanded size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclShape {
    /// A structural record: expansion substitutes the arguments into the
    /// fields and the result is compared structurally, per section 2.5.
    Record {
        /// The type's own parameters, in order.
        params: Vec<Symbol>,
        /// The field types, in terms of `params`.
        fields: Vec<Field>,
    },
    /// A nominal enum: expansion substitutes the arguments into the payloads,
    /// but two enum declarations are never the same type (section 6.3).
    Enum {
        /// The type's own parameters, in order.
        params: Vec<Symbol>,
        /// The variants, in terms of `params`.
        variants: Vec<Variant>,
    },
}

/// A program's types, interned. One interner belongs to one program; ids from
/// different interners must not be mixed.
#[derive(Debug)]
pub struct TypeInterner {
    shapes: Vec<Shape>,
    // Only looked up, never iterated: the map from shape to id is not part of
    // any observable order, only `shapes` (indexed by `TypeId`) is.
    #[allow(clippy::disallowed_types, reason = "lookup only; never iterated")]
    by_shape: std::collections::HashMap<Shape, TypeId>,
    decls: BTreeMap<DeclId, DeclShape>,
    decl_names: BTreeMap<DeclId, Symbol>,
    result_declared: bool,
    json_declared: bool,
    next_error: u32,
    // The built-in types, interned once and returned by their accessor.
    int: TypeId,
    float: TypeId,
    bool_: TypeId,
    string: TypeId,
    filter: TypeId,
}

/// The prelude declaration index of `Result`, which is reserved.
///
/// Numerically equal to `hollowscript_core::transfer::RESULT_INDEX`, which
/// exists for the same reason and is where the name `RESULT_DECL_INDEX`
/// would otherwise have to be duplicated: see
/// `hollowscript_check::effects::transfer_type_id`'s own documentation.
/// How the error type is written in a diagnostic message. The checker
/// never lets a message that mentions it reach a person (see
/// `Checker::error_at`), since section 2.11 says an expression of the error
/// type produces no further diagnostic.
pub(crate) const ERROR_TYPE_TEXT: &str = "<error>";

const RESULT_DECL_INDEX: u32 = 0;
/// The prelude declaration index of `Json`, also reserved. Numerically equal
/// to `hollowscript_core::transfer::JSON_INDEX`, for the same reason as
/// [`RESULT_DECL_INDEX`].
const JSON_DECL_INDEX: u32 = 1;

/// The limit of section 2.5: a type whose expanded size would exceed this is
/// HS0335 (`hollowscript_core::limits::structural::MAX_TYPE_NODES`).
pub use hollowscript_core::limits::structural::MAX_TYPE_NODES;

impl Default for TypeInterner {
    fn default() -> TypeInterner {
        TypeInterner::new()
    }
}

impl TypeInterner {
    /// A fresh interner with the built-in scalar types already interned.
    #[must_use]
    pub fn new() -> TypeInterner {
        let mut t = TypeInterner {
            shapes: Vec::new(),
            #[allow(clippy::disallowed_types, reason = "lookup only; never iterated")]
            by_shape: std::collections::HashMap::new(),
            decls: BTreeMap::new(),
            decl_names: BTreeMap::new(),
            result_declared: false,
            json_declared: false,
            next_error: 0,
            int: TypeId(0),
            float: TypeId(0),
            bool_: TypeId(0),
            string: TypeId(0),
            filter: TypeId(0),
        };
        t.int = t.intern(Shape::Int);
        t.float = t.intern(Shape::Float);
        t.bool_ = t.intern(Shape::Bool);
        t.string = t.intern(Shape::String);
        t.filter = t.intern(Shape::Filter);
        t
    }

    fn intern(&mut self, shape: Shape) -> TypeId {
        if let Shape::Error(_) = shape {
            let id = TypeId(u32::try_from(self.shapes.len()).unwrap_or(u32::MAX));
            self.shapes.push(shape);
            return id;
        }
        if let Some(&id) = self.by_shape.get(&shape) {
            return id;
        }
        let id = TypeId(u32::try_from(self.shapes.len()).unwrap_or(u32::MAX));
        self.shapes.push(shape.clone());
        self.by_shape.insert(shape, id);
        id
    }

    /// `Int`.
    #[must_use]
    pub fn int(&self) -> TypeId {
        self.int
    }
    /// `Float`.
    #[must_use]
    pub fn float(&self) -> TypeId {
        self.float
    }
    /// `Bool`.
    #[must_use]
    pub fn bool(&self) -> TypeId {
        self.bool_
    }
    /// `String`.
    #[must_use]
    pub fn string(&self) -> TypeId {
        self.string
    }
    /// `Filter`.
    #[must_use]
    pub fn filter(&self) -> TypeId {
        self.filter
    }
    /// `[element]`.
    pub fn list(&mut self, element: TypeId) -> TypeId {
        self.intern(Shape::List(element))
    }
    /// `Map<key, value>`.
    pub fn map(&mut self, key: TypeId, value: TypeId) -> TypeId {
        self.intern(Shape::Map(key, value))
    }
    /// `Set<element>`.
    pub fn set(&mut self, element: TypeId) -> TypeId {
        self.intern(Shape::Set(element))
    }
    /// `inner?`. Optionality does not nest (section 2.4): optional of an
    /// already optional type is that same type.
    pub fn optional(&mut self, inner: TypeId) -> TypeId {
        if matches!(self.shapes.get(inner.0 as usize), Some(Shape::Optional(_))) {
            return inner;
        }
        self.intern(Shape::Optional(inner))
    }
    /// A record type. Fields are sorted by their resolved name text, so that
    /// field order never affects identity and, just as importantly, so that
    /// [`hir::ExprKind::Field::index`](crate::hir::ExprKind::Field), which
    /// this order defines, agrees with `hollowscript_bytecode::Shape`'s own
    /// alphabetical field order: the compiler builds every `Shape` and
    /// `NewRecord` by sorting field names as text, so this interner sorting
    /// by [`Symbol`] (first-use order) instead would silently point
    /// `GetField` at the wrong position for any record whose fields are not
    /// declared in alphabetical order. `symbols` is the same interner the
    /// names being sorted came from; passing a different one is a caller
    /// error this type cannot detect.
    pub fn record(&mut self, symbols: &Interner, mut fields: Vec<Field>) -> TypeId {
        fields.sort_by(|a, b| symbols.resolve(a.name).cmp(symbols.resolve(b.name)));
        let shape = fields.into_iter().map(|f| (f.name, f.ty)).collect();
        self.intern(Shape::Record(shape))
    }
    /// A named record or enum applied to its type arguments.
    pub fn named(&mut self, decl: DeclId, args: Vec<TypeId>) -> TypeId {
        self.intern(Shape::Named { decl, args })
    }
    /// `fn(params): ret`, or `effect fn(params): ret`.
    pub fn function(&mut self, effect: bool, params: Vec<TypeId>, ret: Option<TypeId>) -> TypeId {
        self.intern(Shape::Function {
            effect,
            params,
            ret,
        })
    }
    /// The type parameter at `index` of `decl`, opaque within it.
    pub fn param(&mut self, decl: DeclId, index: u32) -> TypeId {
        self.intern(Shape::Param { decl, index })
    }
    /// A fresh error type. Every call returns a distinct id, so an error type
    /// is never mistaken for another (section 2.11).
    pub fn error(&mut self) -> TypeId {
        let id = self.next_error;
        self.next_error += 1;
        self.intern(Shape::Error(id))
    }

    /// Whether `id` is the error type.
    #[must_use]
    pub fn is_error(&self, id: TypeId) -> bool {
        matches!(self.shapes.get(id.0 as usize), Some(Shape::Error(_)))
    }

    /// Whether `id` is `T?` for some `T`, and if so, `T`.
    #[must_use]
    pub fn as_optional(&self, id: TypeId) -> Option<TypeId> {
        match self.shapes.get(id.0 as usize) {
            Some(&Shape::Optional(inner)) => Some(inner),
            _ => None,
        }
    }

    /// Registers the name, and the parameters, fields or variants, of a
    /// `record` or `enum` declaration, so that [`TypeInterner::expand_record`]
    /// and [`TypeInterner::expanded_size`] can substitute into it and so that
    /// [`TypeInterner::decl_name`] and [`TypeInterner::display`] can name it.
    pub fn declare(&mut self, decl: DeclId, name: Symbol, shape: DeclShape) {
        self.decl_names.insert(decl, name);
        self.decls.insert(decl, shape);
    }

    /// The name a declaration was given with [`TypeInterner::declare`].
    #[must_use]
    pub fn decl_name(&self, decl: DeclId) -> Option<Symbol> {
        self.decl_names.get(&decl).copied()
    }

    /// The registered shape of a declaration: its parameters and, for a
    /// record, its fields, or, for an enum, its variants.
    #[must_use]
    pub fn decl_shape(&self, decl: DeclId) -> Option<&DeclShape> {
        self.decls.get(&decl)
    }

    /// The declaration of a named type, if `id` is one and it was declared.
    #[must_use]
    pub fn declaration_of(&self, id: TypeId) -> Option<(DeclId, &[TypeId])> {
        match self.shapes.get(id.0 as usize) {
            Some(Shape::Named { decl, args }) => Some((*decl, args.as_slice())),
            _ => None,
        }
    }

    /// A borrowed view of `id`'s structure (section 2.2), for a caller
    /// outside this module: inference and unification match on this instead
    /// of the private `Shape`.
    #[must_use]
    pub fn view(&self, id: TypeId) -> TypeView<'_> {
        match self.shapes.get(id.0 as usize) {
            Some(Shape::Int) => TypeView::Int,
            Some(Shape::Float) => TypeView::Float,
            Some(Shape::Bool) => TypeView::Bool,
            Some(Shape::String) => TypeView::String,
            Some(Shape::Filter) => TypeView::Filter,
            Some(&Shape::List(t)) => TypeView::List(t),
            Some(&Shape::Map(k, v)) => TypeView::Map(k, v),
            Some(&Shape::Set(t)) => TypeView::Set(t),
            Some(&Shape::Optional(t)) => TypeView::Optional(t),
            Some(Shape::Record(fields)) => TypeView::Record(fields.as_slice()),
            Some(&Shape::Named { decl, ref args }) => TypeView::Named {
                decl,
                args: args.as_slice(),
            },
            Some(&Shape::Function {
                effect,
                ref params,
                ret,
            }) => TypeView::Function {
                effect,
                params: params.as_slice(),
                ret,
            },
            Some(&Shape::Param { decl, index }) => TypeView::Param { decl, index },
            None | Some(Shape::Error(_)) => TypeView::Error,
        }
    }

    /// The declaration of the prelude's `Result<T, E>` (section 2.2),
    /// declaring it with `symbols` the first time it is needed. `Result` has
    /// two variants, `Ok` (payload the success type) and `Err` (payload the
    /// error type), like any other enum, so pattern matching and the native
    /// `result.*` functions need no special case beyond knowing this id.
    pub fn result_decl(&mut self, symbols: &mut Interner) -> DeclId {
        let decl = DeclId {
            module: PRELUDE_MODULE,
            index: RESULT_DECL_INDEX,
        };
        if !self.result_declared {
            self.result_declared = true;
            let params = vec![symbols.intern("T"), symbols.intern("E")];
            let ok = symbols.intern("Ok");
            let err = symbols.intern("Err");
            let name = symbols.intern("Result");
            let payload_t = self.param(decl, 0);
            let payload_e = self.param(decl, 1);
            self.declare(
                decl,
                name,
                DeclShape::Enum {
                    params,
                    variants: vec![
                        Variant {
                            name: ok,
                            payload: vec![payload_t],
                        },
                        Variant {
                            name: err,
                            payload: vec![payload_e],
                        },
                    ],
                },
            );
        }
        decl
    }

    /// `Result<ok, err>`.
    pub fn result(&mut self, symbols: &mut Interner, ok: TypeId, err: TypeId) -> TypeId {
        let decl = self.result_decl(symbols);
        self.named(decl, vec![ok, err])
    }

    /// The declaration of the prelude's `Json` (section 15.1), declaring it
    /// with `symbols` the first time it is needed. It has seven variants and
    /// no type parameters, and refers to itself through `Array` and
    /// `Object`, which is why it is declared in two steps: the entry exists
    /// before its own payload types are built.
    pub fn json_decl(&mut self, symbols: &mut Interner) -> DeclId {
        let decl = DeclId {
            module: PRELUDE_MODULE,
            index: JSON_DECL_INDEX,
        };
        if self.json_declared {
            return decl;
        }
        self.json_declared = true;
        let name = symbols.intern("Json");
        // Declared empty first, so that `named` below resolves to this same
        // declaration while its own payloads are being built.
        self.declare(
            decl,
            name,
            DeclShape::Enum {
                params: Vec::new(),
                variants: Vec::new(),
            },
        );
        let json = self.named(decl, Vec::new());
        let list = self.list(json);
        let string = self.string();
        let map = self.map(string, json);
        let variants = vec![
            ("Null", Vec::new()),
            ("Bool", vec![self.bool_]),
            ("Int", vec![self.int]),
            ("Float", vec![self.float]),
            ("String", vec![string]),
            ("Array", vec![list]),
            ("Object", vec![map]),
        ];
        let variants = variants
            .into_iter()
            .map(|(variant, payload)| Variant {
                name: symbols.intern(variant),
                payload,
            })
            .collect();
        self.declare(
            decl,
            name,
            DeclShape::Enum {
                params: Vec::new(),
                variants,
            },
        );
        decl
    }

    /// The `Result` declaration, if anything has asked for it.
    #[must_use]
    pub fn result_decl_if_declared(&self) -> Option<DeclId> {
        self.result_declared.then_some(DeclId {
            module: PRELUDE_MODULE,
            index: RESULT_DECL_INDEX,
        })
    }

    /// The `Json` declaration, if anything has asked for it. The compiler
    /// needs to know whether the checker built one without building one
    /// itself, because it holds the interner immutably.
    #[must_use]
    pub fn json_decl_if_declared(&self) -> Option<DeclId> {
        self.json_declared.then_some(DeclId {
            module: PRELUDE_MODULE,
            index: JSON_DECL_INDEX,
        })
    }

    /// `JsonError`, the prelude record `{ message: String, offset: Int }`
    /// (section 15.1).
    pub fn json_error(&mut self, symbols: &mut Interner) -> TypeId {
        let message = symbols.intern("message");
        let offset = symbols.intern("offset");
        let string = self.string();
        let int = self.int();
        self.record(
            symbols,
            vec![
                Field {
                    name: message,
                    ty: string,
                },
                Field {
                    name: offset,
                    ty: int,
                },
            ],
        )
    }

    /// `TimeParts`, the prelude record of section 15.2, whose eight fields
    /// are all `Int`.
    pub fn time_parts(&mut self, symbols: &mut Interner) -> TypeId {
        let int = self.int();
        let fields = [
            "year",
            "month",
            "day",
            "hour",
            "minute",
            "second",
            "millisecond",
            "weekday",
        ]
        .into_iter()
        .map(|name| Field {
            name: symbols.intern(name),
            ty: int,
        })
        .collect();
        self.record(symbols, fields)
    }

    /// The `Ok` and `Err` payload types of `id`, if it is `Result<T, E>`.
    #[must_use]
    pub fn as_result(&self, id: TypeId) -> Option<(TypeId, TypeId)> {
        let (decl, args) = self.declaration_of(id)?;
        if decl.module != PRELUDE_MODULE || decl.index != RESULT_DECL_INDEX {
            return None;
        }
        match (args.first(), args.get(1)) {
            (Some(&t), Some(&e)) => Some((t, e)),
            _ => None,
        }
    }

    /// Substitutes `args` for the parameters of `decl` inside `ty`.
    /// The variants of the enum `id` denotes, with its own type arguments
    /// substituted into every payload, so that the payload of `Ok` in a
    /// `Result<String, IoError>` is `String` and not the declaration's own
    /// first parameter.
    ///
    /// Returns `None` for a type that is not an enum.
    pub fn instantiated_variants(
        &mut self,
        symbols: &Interner,
        id: TypeId,
    ) -> Option<Vec<Variant>> {
        let (decl, args) = self.declaration_of(id).map(|(d, a)| (d, a.to_vec()))?;
        let DeclShape::Enum { variants, .. } = self.decl_shape(decl)?.clone() else {
            return None;
        };
        Some(
            variants
                .into_iter()
                .map(|variant| Variant {
                    name: variant.name,
                    payload: variant
                        .payload
                        .into_iter()
                        .map(|p| self.substitute(symbols, p, decl, &args))
                        .collect(),
                })
                .collect(),
        )
    }

    fn substitute(
        &mut self,
        symbols: &Interner,
        ty: TypeId,
        decl: DeclId,
        args: &[TypeId],
    ) -> TypeId {
        match self.shapes.get(ty.0 as usize).cloned() {
            Some(Shape::Param { decl: d, index }) if d == decl => {
                args.get(index as usize).copied().unwrap_or(ty)
            }
            Some(Shape::List(t)) => {
                let t = self.substitute(symbols, t, decl, args);
                self.list(t)
            }
            Some(Shape::Set(t)) => {
                let t = self.substitute(symbols, t, decl, args);
                self.set(t)
            }
            Some(Shape::Optional(t)) => {
                let t = self.substitute(symbols, t, decl, args);
                self.optional(t)
            }
            Some(Shape::Map(k, v)) => {
                let k = self.substitute(symbols, k, decl, args);
                let v = self.substitute(symbols, v, decl, args);
                self.map(k, v)
            }
            Some(Shape::Record(fields)) => {
                let fields = fields
                    .into_iter()
                    .map(|(name, t)| Field {
                        name,
                        ty: self.substitute(symbols, t, decl, args),
                    })
                    .collect();
                self.record(symbols, fields)
            }
            Some(Shape::Named {
                decl: inner_decl,
                args: inner_args,
            }) => {
                let inner_args = inner_args
                    .into_iter()
                    .map(|t| self.substitute(symbols, t, decl, args))
                    .collect();
                self.named(inner_decl, inner_args)
            }
            Some(Shape::Function {
                effect,
                params,
                ret,
            }) => {
                let params = params
                    .into_iter()
                    .map(|t| self.substitute(symbols, t, decl, args))
                    .collect();
                let ret = ret.map(|t| self.substitute(symbols, t, decl, args));
                self.function(effect, params, ret)
            }
            _ => ty,
        }
    }

    /// The record type that a `Named` record type stands for, with its
    /// arguments substituted into its fields. Returns `None` for an enum,
    /// which section 2.5 does not expand this way, or an undeclared name.
    /// `symbols` must be the interner the declaration's field names came
    /// from, so that the expanded record's fields sort the same way
    /// [`TypeInterner::record`] always does.
    #[must_use]
    pub fn expand_record(&mut self, symbols: &Interner, id: TypeId) -> Option<TypeId> {
        let (decl, args) = self.declaration_of(id)?;
        let args = args.to_vec();
        let DeclShape::Record { fields, .. } = self.decls.get(&decl)?.clone() else {
            return None;
        };
        let fields = fields
            .into_iter()
            .map(|f| Field {
                name: f.name,
                ty: self.substitute(symbols, f.ty, decl, &args),
            })
            .collect();
        Some(self.record(symbols, fields))
    }

    /// The *expanded size* of a type (section 2.5): every position after all
    /// aliases and type arguments are substituted, sharing ignored, saturating
    /// at [`u32::MAX`] rather than overflowing. The trace does not follow the
    /// payloads of an enum, so a cycle through one is finite; a record cannot
    /// contain itself (HS0305) and so this always terminates for an accepted
    /// program, but the saturating count keeps it terminating for any input.
    #[must_use]
    pub fn expanded_size(&mut self, symbols: &Interner, id: TypeId) -> u32 {
        self.expanded_size_bounded(symbols, id, MAX_TYPE_NODES + 1)
    }

    fn expanded_size_bounded(&mut self, symbols: &Interner, id: TypeId, budget: u32) -> u32 {
        if budget == 0 {
            return u32::MAX;
        }
        let add = |a: u32, b: u32| a.saturating_add(b);
        match self.shapes.get(id.0 as usize).cloned() {
            None
            | Some(
                Shape::Int
                | Shape::Float
                | Shape::Bool
                | Shape::String
                | Shape::Filter
                | Shape::Param { .. }
                | Shape::Error(_),
            ) => 1,
            Some(Shape::List(t) | Shape::Set(t) | Shape::Optional(t)) => add(
                1,
                self.expanded_size_bounded(symbols, t, budget.saturating_sub(1)),
            ),
            Some(Shape::Map(k, v)) => {
                let ks = self.expanded_size_bounded(symbols, k, budget.saturating_sub(1));
                let vs = self.expanded_size_bounded(
                    symbols,
                    v,
                    budget.saturating_sub(1).saturating_sub(ks),
                );
                add(1, add(ks, vs))
            }
            Some(Shape::Record(fields)) => {
                let mut total = 1u32;
                for (_, t) in fields {
                    total = add(
                        total,
                        self.expanded_size_bounded(symbols, t, budget.saturating_sub(total)),
                    );
                }
                total
            }
            Some(Shape::Function { params, ret, .. }) => {
                let mut total = 1u32;
                for t in params {
                    total = add(
                        total,
                        self.expanded_size_bounded(symbols, t, budget.saturating_sub(total)),
                    );
                }
                if let Some(t) = ret {
                    total = add(
                        total,
                        self.expanded_size_bounded(symbols, t, budget.saturating_sub(total)),
                    );
                }
                total
            }
            Some(Shape::Named { .. }) => match self.expand_record(symbols, id) {
                Some(expanded) if expanded != id => {
                    self.expanded_size_bounded(symbols, expanded, budget)
                }
                // An enum, or a name with no declaration: counted as one node;
                // its payloads are not followed (section 2.5).
                _ => 1,
            },
        }
    }

    /// Whether `id` is equatable (section 2.8): every type except `Filter`,
    /// function types, type parameters and anything containing one.
    #[must_use]
    pub fn is_equatable(&self, id: TypeId) -> bool {
        match self.shapes.get(id.0 as usize) {
            Some(Shape::Filter | Shape::Function { .. } | Shape::Param { .. }) => false,
            Some(Shape::List(t) | Shape::Set(t) | Shape::Optional(t)) => self.is_equatable(*t),
            Some(Shape::Map(k, v)) => self.is_equatable(*k) && self.is_equatable(*v),
            Some(Shape::Record(fields)) => fields.iter().all(|(_, t)| self.is_equatable(*t)),
            _ => true,
        }
    }

    /// Whether `id` is a key type (section 2.8): `Int`, `String` or `Bool`.
    #[must_use]
    pub fn is_key_type(&self, id: TypeId) -> bool {
        id == self.int || id == self.bool_ || id == self.string
    }

    /// The written form of a type, cut after 200 characters with an ellipsis
    /// as section 2.5 requires for a diagnostic message.
    #[must_use]
    pub fn display(&self, id: TypeId, symbol: impl Fn(Symbol) -> String) -> String {
        let mut out = String::new();
        self.write(id, &symbol, &mut out);
        if out.chars().count() > 200 {
            out = out.chars().take(200).collect();
            out.push('\u{2026}');
        }
        out
    }

    fn write(&self, id: TypeId, symbol: &impl Fn(Symbol) -> String, out: &mut String) {
        match self.shapes.get(id.0 as usize) {
            Some(Shape::Int) => out.push_str("Int"),
            Some(Shape::Float) => out.push_str("Float"),
            Some(Shape::Bool) => out.push_str("Bool"),
            Some(Shape::String) => out.push_str("String"),
            Some(Shape::Filter) => out.push_str("Filter"),
            None | Some(Shape::Error(_)) => out.push_str(ERROR_TYPE_TEXT),
            Some(Shape::List(t)) => {
                out.push('[');
                self.write(*t, symbol, out);
                out.push(']');
            }
            Some(Shape::Set(t)) => {
                out.push_str("Set<");
                self.write(*t, symbol, out);
                out.push('>');
            }
            Some(Shape::Optional(t)) => {
                self.write(*t, symbol, out);
                out.push('?');
            }
            Some(Shape::Map(k, v)) => {
                out.push_str("Map<");
                self.write(*k, symbol, out);
                out.push_str(", ");
                self.write(*v, symbol, out);
                out.push('>');
            }
            Some(Shape::Record(fields)) => {
                out.push_str("{ ");
                for (i, (name, t)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&symbol(*name));
                    out.push_str(": ");
                    self.write(*t, symbol, out);
                }
                out.push_str(" }");
            }
            Some(Shape::Named { decl, args }) => {
                match self.decl_names.get(decl) {
                    Some(&name) => out.push_str(&symbol(name)),
                    None => out.push_str("<named>"),
                }
                if !args.is_empty() {
                    out.push('<');
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        self.write(*a, symbol, out);
                    }
                    out.push('>');
                }
            }
            Some(Shape::Function {
                effect,
                params,
                ret,
            }) => {
                if *effect {
                    out.push_str("effect ");
                }
                out.push_str("fn(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.write(*p, symbol, out);
                }
                out.push(')');
                if let Some(r) = ret {
                    out.push_str(": ");
                    self.write(*r, symbol, out);
                }
            }
            Some(Shape::Param { .. }) => out.push_str("<param>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeclId, DeclShape, Field, TypeInterner, TypeView, Variant};
    use hollowscript_core::Interner;

    fn sym(interner: &mut Interner, s: &str) -> hollowscript_core::Symbol {
        interner.intern(s)
    }

    #[test]
    #[allow(
        clippy::many_single_char_names,
        reason = "t is the interner and a, b, x, y name the sample fields"
    )]
    fn structurally_equal_types_share_one_id() {
        let mut t = TypeInterner::new();
        let mut names = Interner::new();
        let x = sym(&mut names, "x");
        let y = sym(&mut names, "y");
        let a = t.record(
            &names,
            vec![
                Field {
                    name: x,
                    ty: t.int(),
                },
                Field {
                    name: y,
                    ty: t.int(),
                },
            ],
        );
        let b = t.record(
            &names,
            vec![
                Field {
                    name: y,
                    ty: t.int(),
                },
                Field {
                    name: x,
                    ty: t.int(),
                },
            ],
        );
        assert_eq!(a, b, "field order does not affect identity");
        assert_eq!(t.list(t.int()), t.list(t.int()));
        assert_ne!(t.list(t.int()), t.list(t.float()));
    }

    #[test]
    fn record_fields_sort_by_resolved_text_not_by_intern_order() {
        // Interning "name" before "age" and building the record literal
        // {name, age} reproduces the mismatch the compiler's module
        // documentation describes: sorting by `Symbol` (first-use order)
        // would put "name" first, but `hollowscript_bytecode::Shape` and
        // this compiler's own `NewRecord`/`GetField` always sort fields
        // alphabetically by text, so `ExprKind::Field::index` (defined by
        // this order) must agree with that or `GetField` reads the wrong
        // field.
        let mut t = TypeInterner::new();
        let mut names = Interner::new();
        let name = sym(&mut names, "name");
        let age = sym(&mut names, "age");
        let record = t.record(
            &names,
            vec![
                Field { name, ty: t.int() },
                Field {
                    name: age,
                    ty: t.int(),
                },
            ],
        );
        assert_eq!(
            t.display(record, |s| names.resolve(s).to_owned()),
            "{ age: Int, name: Int }",
            "fields must sort alphabetically by resolved text, not by intern order"
        );
    }

    #[test]
    fn optionality_does_not_nest() {
        let mut t = TypeInterner::new();
        let once = t.optional(t.int());
        let twice = t.optional(once);
        assert_eq!(once, twice);
    }

    #[test]
    fn every_error_type_is_distinct() {
        let mut t = TypeInterner::new();
        let a = t.error();
        let b = t.error();
        assert_ne!(a, b);
        assert!(t.is_error(a) && t.is_error(b));
        assert!(!t.is_error(t.int()));
    }

    #[test]
    fn equatable_and_key_types_follow_section_2_8() {
        let mut t = TypeInterner::new();
        assert!(t.is_equatable(t.int()) && t.is_equatable(t.string()));
        assert!(!t.is_equatable(t.filter()));
        let f = t.function(false, vec![t.int()], None);
        assert!(!t.is_equatable(f));
        let list_of_filters = t.list(t.filter());
        assert!(!t.is_equatable(list_of_filters));
        assert!(t.is_key_type(t.int()) && t.is_key_type(t.bool()) && t.is_key_type(t.string()));
        assert!(!t.is_key_type(t.float()));
    }

    #[test]
    #[allow(
        clippy::many_single_char_names,
        reason = "l and r name the fields of the sample record"
    )]
    fn generic_records_expand_and_are_bounded() {
        let mut t = TypeInterner::new();
        let mut names = Interner::new();
        let decl = DeclId {
            module: 0,
            index: 0,
        };
        let l = sym(&mut names, "l");
        let r = sym(&mut names, "r");
        let a_sym = sym(&mut names, "A");
        let pair_sym = sym(&mut names, "Pair");
        let param = t.param(decl, 0);
        t.declare(
            decl,
            pair_sym,
            DeclShape::Record {
                params: vec![a_sym],
                fields: vec![Field { name: l, ty: param }, Field { name: r, ty: param }],
            },
        );
        let pair_of_int = t.named(decl, vec![t.int()]);
        let expanded = t.expand_record(&names, pair_of_int).unwrap();
        assert_eq!(
            expanded,
            t.record(
                &names,
                vec![
                    Field {
                        name: l,
                        ty: t.int()
                    },
                    Field {
                        name: r,
                        ty: t.int()
                    }
                ]
            )
        );
        assert_eq!(t.expanded_size(&names, pair_of_int), 3);

        // record P<A> = { l: A, r: A } applied to itself doubles at each step.
        let self_applied = t.named(decl, vec![pair_of_int]);
        let doubled = t.named(decl, vec![self_applied]);
        assert!(t.expanded_size(&names, doubled) > t.expanded_size(&names, self_applied));
        assert!(t.expanded_size(&names, doubled) < u32::MAX);
    }

    #[test]
    fn enum_payloads_are_not_followed_by_expansion() {
        let mut t = TypeInterner::new();
        let mut names = Interner::new();
        let decl = DeclId {
            module: 0,
            index: 1,
        };
        let leaf = sym(&mut names, "Leaf");
        let node = sym(&mut names, "Node");
        let named = t.named(decl, vec![]);
        let tree_sym = sym(&mut names, "Tree");
        t.declare(
            decl,
            tree_sym,
            DeclShape::Enum {
                params: vec![],
                variants: vec![
                    Variant {
                        name: leaf,
                        payload: vec![],
                    },
                    Variant {
                        name: node,
                        payload: vec![named],
                    },
                ],
            },
        );
        assert_eq!(t.expand_record(&names, named), None);
        assert_eq!(
            t.expanded_size(&names, named),
            1,
            "an enum's own payloads are not followed"
        );
    }

    #[test]
    fn display_is_cut_at_two_hundred_characters() {
        let mut t = TypeInterner::new();
        let mut list = t.int();
        for _ in 0..120 {
            list = t.list(list);
        }
        let text = t.display(list, |_| String::new());
        assert!(text.ends_with('\u{2026}'));
        // 200 characters kept, plus the ellipsis that marks the cut.
        assert_eq!(text.chars().count(), 201);
        assert_eq!(t.display(t.int(), |_| String::new()), "Int");
    }

    #[test]
    fn view_matches_every_shape_a_caller_needs_to_see() {
        let mut t = TypeInterner::new();
        let mut names = Interner::new();
        assert!(matches!(t.view(t.int()), TypeView::Int));
        let list = t.list(t.float());
        assert!(matches!(t.view(list), TypeView::List(e) if e == t.float()));
        let map = t.map(t.string(), t.bool());
        assert!(matches!(t.view(map), TypeView::Map(k, v) if k == t.string() && v == t.bool()));
        let f = t.function(true, vec![t.int()], Some(t.string()));
        assert!(matches!(
            t.view(f),
            TypeView::Function { effect: true, params, ret: Some(r) }
                if params == [t.int()] && r == t.string()
        ));
        let x = sym(&mut names, "x");
        let rec = t.record(
            &names,
            vec![Field {
                name: x,
                ty: t.int(),
            }],
        );
        assert!(matches!(t.view(rec), TypeView::Record(fields) if fields.len() == 1));
        let err = t.error();
        assert!(matches!(t.view(err), TypeView::Error));
    }

    #[test]
    fn result_is_a_declared_enum_with_ok_and_err_variants() {
        let mut t = TypeInterner::new();
        let mut names = Interner::new();
        let result = t.result(&mut names, t.int(), t.string());
        assert_eq!(t.as_result(result), Some((t.int(), t.string())));
        assert_eq!(
            t.display(result, |s| names.resolve(s).to_owned()),
            "Result<Int, String>"
        );
        let decl = t.result_decl(&mut names);
        assert_eq!(names.resolve(t.decl_name(decl).unwrap()), "Result");
        let DeclShape::Enum { variants, .. } = t.decl_shape(decl).unwrap() else {
            unreachable!("Result is declared as an enum")
        };
        assert_eq!(variants.len(), 2);
        assert_eq!(names.resolve(variants[0].name), "Ok");
        assert_eq!(names.resolve(variants[1].name), "Err");
        assert_eq!(t.as_result(t.int()), None);
    }
}
