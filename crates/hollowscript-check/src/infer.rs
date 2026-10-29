//! Types and inference (chapter 2), and the expression and statement
//! checking driver built on top of it.
//!
//! Checking an expression takes an optional *expected type* (section 2.9) and
//! either propagates it into the expression or computes a type bottom-up.
//! Generic calls (section 2.10) run their own inference: a call's type
//! parameters are the callee's own declared `TypeView::Param`
//! positions, and [`unify`] binds them one by one as arguments are checked.
//!
//! [`Checker`] is the driver: one instance checks one module, holding the
//! shared type and symbol interners, the file's value and type scopes from
//! [`crate::resolve::resolve_file`], and the stack of function frames
//! currently open (for locals, captures, `return` and loop context). It is
//! the "surrounding expression and statement type-checking driver" the
//! module comment of [`crate`] describes as larger than unification alone.

use crate::effects::{self, Grants};
use crate::exhaustive;
use crate::hir::{
    self, BinaryOp, Block, CapabilityId, Capture, Constant, Expr, ExprKind, Fact, Function,
    FunctionId, GlobalId, LocalBinding, LocalId, LogicalOp, Path, PathRoot, Pattern, PatternKind,
    Stmt, StmtKind, UnaryOp,
};
use crate::narrow::{self, FactSet};
use crate::resolve::{self, FileScope, PreludeValue, ResolvedImport, TypeBinding, ValueBinding};
use crate::types::{
    DeclId, DeclShape, ERROR_TYPE_TEXT, Field, TypeId, TypeInterner, TypeVarId, TypeView, Variant,
};
use hollowscript_core::{BoundedDiagnostics, Code, Diagnostic, Interner, Source, Span, Symbol};
use hollowscript_syntax::ast;
use std::collections::{BTreeMap, BTreeSet};

/// The expected type of an expression, threaded downward (section 2.9).
/// `None` where the specification says an expression has no expected type: a
/// top-level statement, a list literal with no annotation, or the operand of `?`.
pub type Expected = Option<TypeId>;

/// The bindings of the type variables of one call, built up as
/// [`unify`] runs (section 2.10, steps 1 to 4). A variable's identity is the
/// position of the callee's own type parameter it stands for: every
/// [`crate::types::TypeView::Param`] [`unify`] meets while walking a
/// callee's own declared parameter type belongs to that one callee, by
/// construction (a declaration's signature only ever mentions its own type
/// parameters), so the position alone identifies it for the one call this
/// substitution is scoped to.
#[derive(Debug, Default)]
pub struct Substitution {
    bound: BTreeMap<TypeVarId, TypeId>,
}

impl Substitution {
    /// An empty substitution, as at the start of checking a call.
    #[must_use]
    pub fn new() -> Substitution {
        Substitution::default()
    }

    /// The type bound to `var`, if any.
    #[must_use]
    pub fn get(&self, var: TypeVarId) -> Option<TypeId> {
        self.bound.get(&var).copied()
    }

    /// Binds `var` to `ty`. The caller has already checked that `var` was
    /// unbound or, if bound, that `ty` is identical to what it holds
    /// (section 2.10 step 2); this type does not check that itself, so that
    /// unification can report its own diagnostic (HS0301) on a mismatch.
    pub fn bind(&mut self, var: TypeVarId, ty: TypeId) {
        self.bound.insert(var, ty);
    }

    /// The variables that step 5 of section 2.10 reports as HS0308: every one
    /// of `declared` that this substitution never bound.
    #[must_use]
    pub fn unbound<'a>(&self, declared: &'a [TypeVarId]) -> Vec<&'a TypeVarId> {
        declared
            .iter()
            .filter(|v| !self.bound.contains_key(v))
            .collect()
    }
}

/// Why an argument could not be unified against its parameter type
/// (section 2.10 step 2).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnifyError {
    /// A variable that was already bound met a type other than what it holds.
    Rebound {
        /// The variable.
        var: TypeVarId,
        /// What it was already bound to.
        bound: TypeId,
        /// What it met this time.
        found: TypeId,
    },
    /// The structure outside any variable did not match: differing type
    /// constructors, or a mismatch that assignability (section 2.7) refuses.
    NotAssignable {
        /// The type required by the parameter's structure.
        expected: TypeId,
        /// The type found in the argument's structure.
        found: TypeId,
    },
    /// A variable would unify with a type that contains it (the occurs check).
    Occurs {
        /// The variable.
        var: TypeVarId,
    },
}

/// Whether `id` contains the type variable `var` anywhere in its structure
/// (the occurs check, section 2.10), walking only through the interned
/// structure: a variable never occurs inside a type that has no `Param` of
/// the callee's own declaration anywhere in it, so this is always finite.
fn occurs(types: &TypeInterner, id: TypeId, var: TypeVarId) -> bool {
    match types.view(id) {
        TypeView::Param { index, .. } => index == var.0,
        TypeView::List(t) | TypeView::Set(t) | TypeView::Optional(t) => occurs(types, t, var),
        TypeView::Map(k, v) => occurs(types, k, var) || occurs(types, v, var),
        TypeView::Record(fields) => fields.iter().any(|&(_, t)| occurs(types, t, var)),
        TypeView::Named { args, .. } => args.iter().any(|&t| occurs(types, t, var)),
        TypeView::Function { params, ret, .. } => {
            params.iter().any(|&t| occurs(types, t, var))
                || ret.is_some_and(|t| occurs(types, t, var))
        }
        TypeView::Int
        | TypeView::Float
        | TypeView::Bool
        | TypeView::String
        | TypeView::Filter
        | TypeView::Error => false,
    }
}

/// Unifies a parameter type, which may contain the callee's own unbound type
/// variables, with the type of an argument, which never does (section 2.10).
///
/// # Errors
///
/// Returns [`UnifyError`] on a mismatch or a rebinding.
pub fn unify(
    parameter: TypeId,
    argument: TypeId,
    types: &TypeInterner,
    substitution: &mut Substitution,
) -> Result<(), UnifyError> {
    if types.is_error(parameter) || types.is_error(argument) {
        return Ok(());
    }
    if let TypeView::Param { index, .. } = types.view(parameter) {
        let var = TypeVarId(index);
        if let Some(bound) = substitution.get(var) {
            return if bound == argument || is_assignable(argument, bound, types) {
                Ok(())
            } else {
                Err(UnifyError::Rebound {
                    var,
                    bound,
                    found: argument,
                })
            };
        }
        if occurs(types, argument, var) {
            return Err(UnifyError::Occurs { var });
        }
        substitution.bind(var, argument);
        return Ok(());
    }
    // "Where P is V? and V is an unbound variable, A may be X or X?, and
    // binds V to X" (section 2.10 step 2).
    if let TypeView::Optional(inner) = types.view(parameter) {
        if let TypeView::Param { index, .. } = types.view(inner) {
            let var = TypeVarId(index);
            if substitution.get(var).is_none() {
                let bind_to = match types.view(argument) {
                    TypeView::Optional(a) => a,
                    _ => argument,
                };
                if occurs(types, bind_to, var) {
                    return Err(UnifyError::Occurs { var });
                }
                substitution.bind(var, bind_to);
                return Ok(());
            }
        }
        // `inner` is not an unbound variable (or already bound): fall
        // through to a structural match, requiring `argument` to be
        // assignable to `parameter` as a whole (an ordinary optional
        // parameter accepts its inner type too, section 2.7 rule 3).
        if is_assignable(argument, parameter, types) {
            return Ok(());
        }
        if let TypeView::Optional(a) = types.view(argument) {
            return unify(inner, a, types, substitution);
        }
        return unify(inner, argument, types, substitution);
    }
    match (types.view(parameter), types.view(argument)) {
        (TypeView::List(p), TypeView::List(a)) | (TypeView::Set(p), TypeView::Set(a)) => {
            unify(p, a, types, substitution)
        }
        (TypeView::Map(pk, pv), TypeView::Map(ak, av)) => {
            unify(pk, ak, types, substitution)?;
            unify(pv, av, types, substitution)
        }
        (TypeView::Record(pf), TypeView::Record(af)) if pf.len() == af.len() => {
            for (&(pn, pt), &(an, at)) in pf.iter().zip(af.iter()) {
                if pn != an {
                    return Err(UnifyError::NotAssignable {
                        expected: parameter,
                        found: argument,
                    });
                }
                unify(pt, at, types, substitution)?;
            }
            Ok(())
        }
        (
            TypeView::Named {
                decl: pd,
                args: pargs,
            },
            TypeView::Named {
                decl: ad,
                args: aargs,
            },
        ) if pd == ad && pargs.len() == aargs.len() => {
            for (&p, &a) in pargs.iter().zip(aargs.iter()) {
                unify(p, a, types, substitution)?;
            }
            Ok(())
        }
        (
            TypeView::Function {
                effect: pe,
                params: pp,
                ret: pr,
            },
            TypeView::Function {
                effect: ae,
                params: ap,
                ret: ar,
            },
        ) if pp.len() == ap.len() && (pe || !ae) => {
            for (&p, &a) in pp.iter().zip(ap.iter()) {
                unify(p, a, types, substitution)?;
            }
            match (pr, ar) {
                (Some(p), Some(a)) => unify(p, a, types, substitution),
                (None, None) => Ok(()),
                _ => Err(UnifyError::NotAssignable {
                    expected: parameter,
                    found: argument,
                }),
            }
        }
        _ if is_assignable(argument, parameter, types) => Ok(()),
        _ => Err(UnifyError::NotAssignable {
            expected: parameter,
            found: argument,
        }),
    }
}

/// Whether `from` is assignable to `to` (section 2.7): identical types, or
/// `from` assignable to `to`'s inner type when `to` is optional, or `from`
/// pure and `to` the same effectful function type. Numeric coercion is not
/// assignability: it is applied at specific coercion sites by the caller,
/// never here.
#[must_use]
pub fn is_assignable(from: TypeId, to: TypeId, types: &TypeInterner) -> bool {
    if from == to || types.is_error(from) || types.is_error(to) {
        return true;
    }
    if let TypeView::Optional(inner) = types.view(to) {
        if is_assignable(from, inner, types) {
            return true;
        }
    }
    if let (
        TypeView::Function {
            effect: false,
            params: fp,
            ret: fr,
        },
        TypeView::Function {
            effect: true,
            params: tp,
            ret: tr,
        },
    ) = (types.view(from), types.view(to))
    {
        return fp == tp && fr == tr;
    }
    false
}

/// Whether an `Int`-typed expression is coerced to `Float` at a coercion
/// site whose expected type is `expected` (section 2.7).
#[must_use]
pub fn coerces_to_float(found: TypeId, expected: TypeId, types: &TypeInterner) -> bool {
    if found != types.int() {
        return false;
    }
    expected == types.float() || types.as_optional(expected) == Some(types.float())
}

/// Wraps `expr` in [`ExprKind::IntToFloat`] if it is an `Int` at a coercion
/// site whose expected type is `Float` or `Float?` (section 2.7), otherwise
/// returns it unchanged.
#[must_use]
pub fn apply_coercion(expr: Expr, expected: Option<TypeId>, types: &TypeInterner) -> Expr {
    let Some(expected) = expected else {
        return expr;
    };
    if coerces_to_float(expr.ty, expected, types) {
        let node = expr.node;
        Expr {
            node,
            ty: types.float(),
            kind: ExprKind::IntToFloat(Box::new(expr)),
        }
    } else {
        expr
    }
}

/// One function or closure body currently being checked (section 5.4): its
/// own local scope, the captures it has built so far, and the context
/// `return`, `break` and `continue` need.
#[derive(Debug)]
struct FnCtx {
    /// Names visible in this frame's own body: parameters, `let` bindings,
    /// loop variables and pattern bindings, one [`Scopes`] level per block.
    locals: resolve::Scopes<LocalId>,
    /// The number of [`LocalId`]s this frame has handed out.
    local_count: u32,
    /// The captures this frame has built, in the order first referenced
    /// (section 5.4); empty for a top-level function.
    captures: Vec<Capture>,
    /// The upvalue index already assigned in this frame for a name, so a
    /// second reference reuses it instead of capturing again.
    capture_by_name: BTreeMap<Symbol, u32>,
    /// The return type this frame's `fn` or closure declares, if it returns
    /// a value.
    return_ty: Option<TypeId>,
    /// If `return_ty` is `Result<T, E>`, `E`: the error type `?` needs
    /// (section 8.3) to build the early `Result.Err` return.
    result_err_ty: Option<TypeId>,
    /// Whether the point currently being checked is inside a loop of this
    /// same frame (section 4.5): `break` and `continue` outside one are
    /// HS0344.
    in_loop: bool,
    /// The type of each local this frame has declared, indexed by
    /// [`LocalId`]: locals are numbered from 0 without gaps, so a plain
    /// vector is the table.
    local_types: Vec<TypeId>,
    /// The type of every `return e` statement checked so far while this
    /// function's return type is being inferred (section 2.10): joined into
    /// the final return type once the body is fully checked.
    collected_returns: Vec<TypeId>,
    /// The locals declared with `variable` rather than `constant`: only
    /// these may be the target of an assignment (HS0206 otherwise).
    mutable_locals: BTreeSet<LocalId>,
    /// The `variable`s of this frame that section 4.8 calls shared-mutable,
    /// because a nested closure assigns them: no fact rooted at one is ever
    /// used. [`crate::shared`] finds them before any body is checked.
    shared_locals: BTreeSet<LocalId>,
    /// Whether this frame's return type is being inferred from its `return`
    /// statements (only ever true for a closure with neither an annotation
    /// nor an expected function type, section 2.10): distinguishes "no
    /// return type at all" (`return e` here is HS0340) from "not decided
    /// yet" (collect `e`'s type instead).
    infer_return: bool,
    /// Whether this is the pseudo-frame [`Checker::check_top_level`] pushes
    /// for a module's own top-level statements, which need somewhere to put
    /// loop context and local ids (a `for` loop variable, a destructured
    /// `for` pattern) exactly the way a real function's body does, without
    /// being one: a top-level `return`/`?` follows section 4.7's own rule,
    /// never a function's (HS0340/HS0341/HS0342 do not apply to it).
    is_top_level: bool,
}

impl FnCtx {
    fn new(return_ty: Option<TypeId>, result_err_ty: Option<TypeId>) -> FnCtx {
        let mut locals = resolve::Scopes::new();
        locals.push();
        FnCtx {
            locals,
            local_count: 0,
            captures: Vec::new(),
            capture_by_name: BTreeMap::new(),
            return_ty,
            result_err_ty,
            in_loop: false,
            local_types: Vec::new(),
            collected_returns: Vec::new(),
            mutable_locals: BTreeSet::new(),
            shared_locals: BTreeSet::new(),
            infer_return: false,
            is_top_level: false,
        }
    }

    /// Declares a fresh local of type `ty`, returning its identity.
    fn declare_local(&mut self, ty: TypeId) -> LocalId {
        let id = LocalId(self.local_count);
        self.local_count += 1;
        self.local_types.push(ty);
        id
    }
}

/// One name that will warn if nothing reads it (section 17.3).
#[derive(Copy, Clone, Debug)]
struct UnusedName {
    /// HS0801 for a `constant` or `variable`, HS0802 for an import.
    code: Code,
    /// The name, for the message.
    name: Symbol,
    /// The span of the name in its declaration, which is both where the
    /// warning points and the identity a use is matched against.
    span: Span,
}

/// Why a name is being resolved: a read is a *use* for section 17.3's
/// unused-name warnings, and everything else is not. Assigning to a
/// `variable` is explicitly not a use, and neither is a lookup that only
/// asks what a name means (whether an identifier in a pattern is an enum
/// namespace, for instance).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Purpose {
    /// The value is read.
    Read,
    /// The name is being looked up for some other reason.
    Inspect,
}

/// Where a captured value comes from, for [`Checker::capture_into`].
#[derive(Copy, Clone, Debug)]
enum CaptureSource {
    /// A local of the frame the capture starts from, which is the only
    /// thing a closure captures: see [`crate::hir::Capture`] for why a
    /// top-level binding is not one.
    Local(LocalId),
}

/// What a name resolved to (section 4.6), already accounting for capture
/// across closure boundaries (section 5.4): a name found in an outer frame
/// is never read as [`Resolved::Local`] from an inner one, only as
/// [`Resolved::Upvalue`] of the frame that needs it.
#[derive(Copy, Clone, Debug)]
enum Resolved {
    /// A local of the current frame.
    Local(LocalId),
    /// A capture of the current frame, already registered.
    Upvalue(u32),
    /// A top-level binding, read directly (the current frame is the
    /// top-level function itself, so no capture is needed).
    Global(GlobalId),
    /// A top-level `fn`.
    Function(FunctionId),
    /// A capability.
    Capability(CapabilityId),
    /// An enum name used as a namespace.
    EnumNamespace(DeclId),
    /// A prelude value.
    Prelude(PreludeValue),
    /// An import that did not resolve, whose failure is already reported:
    /// reading it is the error type and says nothing further.
    Unresolved,
    /// Not found in any scope or the prelude.
    NotFound,
}

/// One function's signature, known once its parameters and return type are
/// resolved (this module's phase 2), before its body is checked, so that a
/// forward or mutually recursive call can be checked against it.
#[derive(Clone, Debug)]
pub struct FnSig {
    /// The parameter types, each possibly containing one of this function's
    /// own type parameters (section 2.10) if `type_param_count` is nonzero.
    pub params: Vec<TypeId>,
    /// The return type, absent for a function with none.
    pub ret: Option<TypeId>,
    /// The number of type parameters this function declares. A call to it
    /// runs the generic-call algorithm of section 2.10 with this many
    /// variables, which degenerates to an ordinary assignability check at
    /// every parameter when this is 0.
    pub type_param_count: u32,
    /// The function's own name, for a diagnostic that names it (HS0308).
    pub name: Symbol,
    /// The declaration identity its own type parameters' `TypeView::Param`
    /// positions use, needed again when its body is checked so that a type
    /// annotation written in the body can still name a type parameter.
    pub type_param_decl: DeclId,
}

/// One module's checker (sections 2, 4, 7, 9 and 10): the shared type and
/// symbol interners, the run's capability grants, the file's own value and
/// type scopes, and the stack of function frames currently open.
#[derive(Debug)]
pub struct Checker<'a> {
    /// The program's shared type interner.
    pub types: &'a mut TypeInterner,
    /// The program's shared symbol interner.
    pub names: &'a mut Interner,
    /// This module's source, for building diagnostic locations.
    pub source: &'a Source,
    /// This module's index into the program's module list.
    pub module: u32,
    /// Whether this module is the entry module: a top-level `return` is
    /// only allowed there (section 4.7, HS0611 otherwise).
    pub is_entry: bool,
    /// The run's granted capabilities.
    pub grants: &'a Grants,
    /// The checker type of every granted capability, by [`CapabilityId`]
    /// (built once for the whole program by [`crate::effects::capability_types`]).
    pub capability_types: &'a [TypeId],
    /// Enums a capability's declared type mentions, by name, so that a
    /// program can write the type to match one (section 16.1's `IoError`).
    /// A program's own declaration of the same name takes precedence.
    pub host_types: Vec<(Symbol, DeclId)>,
    /// Section 16.1 types (`IoError`, `HttpError`, `HttpRequest`,
    /// `HttpResponse`) that this file's `uses` lines ask for through a
    /// capability the host did not grant, so no grant declared them. The
    /// HS0404 on the `uses` line is the one diagnostic for that mistake; a
    /// name here resolves to the error type silently instead of adding
    /// HS0208 (section 2.11). Any other unknown type name is still HS0208.
    pub ungranted_host_types: Vec<Symbol>,
    /// Set when a match pattern is checked against a value whose type is
    /// the error type, at any depth (see [`Checker::check_match_pattern`]).
    /// A match whose patterns met one cannot say which arms cover or
    /// overlap one another, so it reports neither (section 2.11). Saved and
    /// restored around each match, so a nested match does not leak into the
    /// one around it.
    patterns_met_error: bool,
    /// Every function's signature seen so far, across every module checked
    /// before or up to this one, keyed by [`FunctionId`] (this module's own
    /// functions are added to it during phase 2, before any body is checked,
    /// so a forward reference within the module resolves too).
    pub function_sigs: &'a mut BTreeMap<u32, FnSig>,
    /// Every top-level `constant`/`variable`'s type, keyed by `(module,
    /// index)` matching its [`GlobalId`], across every module checked before
    /// or up to this one.
    pub global_types: &'a mut BTreeMap<(u32, u32), (TypeId, bool)>,
    /// Every function and closure body checked so far, across the whole
    /// program, keyed by [`FunctionId`], collected into
    /// [`crate::hir::Program::functions`] once every module is checked.
    pub functions: &'a mut BTreeMap<u32, Function>,
    /// The program's running [`FunctionId`] counter, shared across modules.
    pub next_function_id: &'a mut u32,
    /// The value scope: capabilities, imports and top-level functions, plus
    /// every top-level `constant`/`variable` checked so far.
    pub values: resolve::Scopes<ValueBinding>,
    /// The type scope: imports and top-level `record`/`enum` names, plus,
    /// while a generic declaration's own signature is being resolved, its
    /// type parameters.
    pub type_scope: resolve::Scopes<TypeBinding>,
    /// The open function frames, innermost (currently being checked) last.
    frames: Vec<FnCtx>,
    /// The declaration span of every name a reference has read
    /// (section 17.3's definition of *used*). A declaration's own span
    /// identifies it within one file, which is what lets a use recorded
    /// deep inside a closure be matched against a declaration whose scope
    /// closed long before the warnings are reported.
    used: BTreeSet<Span>,
    /// Every name that warns when it turns out never to have been read, in
    /// declaration order, with the code to report it under: HS0801 for a
    /// `constant` or `variable`, HS0802 for an import.
    unused: Vec<UnusedName>,
    /// The declaration span of every shared-mutable `variable` of the module
    /// (section 4.8), from [`crate::shared::shared_mutable_variables`].
    shared_mutable: BTreeSet<Span>,
    /// The top-level `variable`s of this module that section 4.8 calls
    /// shared-mutable (their declaration span is in `shared_mutable`): no
    /// fact rooted at one is ever established, the same rule
    /// [`FnCtx::shared_locals`] applies to a function's own locals. Kept
    /// separately because a top-level binding's identity is a [`GlobalId`],
    /// not a [`LocalId`], and outlives any one frame.
    shared_globals: BTreeSet<GlobalId>,
    /// Every function and closure this module's own body checking has
    /// minted a [`FunctionId`] for, in the order checked, which becomes
    /// [`hir::Module::function_ids`].
    module_function_ids: Vec<FunctionId>,
    /// Diagnostics found so far.
    pub diagnostics: BoundedDiagnostics,
    /// The host's filter schema (section 9.7), if it gave one, used to
    /// validate a filter literal against it (Appendix A, section A.7) as
    /// well as parse it. `None` means the host gave no schema, so only the
    /// checks of sections A.2 to A.4 are made (section A.7's own rule).
    pub filter_schema: Option<&'a hollowscript_core::filter::FilterSchema>,
    /// Every filter literal met, in the order it was checked. A literal can
    /// be met more than once (the arguments of an overloaded numeric call are
    /// checked to choose the overload, and again), so [`check_module`] puts
    /// these in source order and keeps one of each.
    filter_literals: Vec<crate::FilterLiteral>,
    /// What [`Checker::every_argument_is_an_int`] answered for a call, by the
    /// node of its first argument, with the facts it was answered under. An
    /// overloaded numeric call checks its arguments to choose an overload and
    /// again against the one it chose, so a call nested in such a call is
    /// checked twice at each level of nesting, 2^n times for `n` levels
    /// (`abs(abs(abs(...)))` took a second at 22 levels). The answer is the
    /// same every time the same call is checked under the same facts, so it is
    /// kept, and the second visit to a call finds it.
    int_argument_answers: BTreeMap<ast::NodeId, (FactSet, bool)>,
}

impl<'a> Checker<'a> {
    /// A checker for one module, seeded with the [`FileScope`]
    /// [`crate::resolve::resolve_file`] already built.
    #[allow(
        clippy::too_many_arguments,
        reason = "one checker per module, built once"
    )]
    pub fn new(
        types: &'a mut TypeInterner,
        names: &'a mut Interner,
        source: &'a Source,
        module: u32,
        is_entry: bool,
        grants: &'a Grants,
        capability_types: &'a [TypeId],
        function_sigs: &'a mut BTreeMap<u32, FnSig>,
        global_types: &'a mut BTreeMap<(u32, u32), (TypeId, bool)>,
        functions: &'a mut BTreeMap<u32, Function>,
        next_function_id: &'a mut u32,
        scope: FileScope,
        filter_schema: Option<&'a hollowscript_core::filter::FilterSchema>,
    ) -> Checker<'a> {
        Checker {
            types,
            names,
            source,
            module,
            is_entry,
            grants,
            capability_types,
            function_sigs,
            global_types,
            functions,
            next_function_id,
            host_types: Vec::new(),
            ungranted_host_types: Vec::new(),
            patterns_met_error: false,
            values: scope.values,
            type_scope: scope.types,
            frames: Vec::new(),
            used: BTreeSet::new(),
            unused: Vec::new(),
            shared_mutable: BTreeSet::new(),
            shared_globals: BTreeSet::new(),
            module_function_ids: Vec::new(),
            diagnostics: scope.diagnostics,
            filter_schema,
            filter_literals: Vec::new(),
            int_argument_answers: BTreeMap::new(),
        }
    }

    fn error_at(&mut self, code: Code, args: &[(&str, &dyn std::fmt::Display)], span: Span) {
        let diagnostic = Diagnostic::new(code, args);
        // Section 2.11: an expression the checker could not type is
        // assignable to and from everything, so using it produces no
        // further diagnostic. Most checks test for the error type before
        // reporting; this catches any that do not, so that the checker's
        // internal `<error>` type is never shown to a person. It only drops
        // a diagnostic once an error has already been reported, so it can
        // never let a program with a mistake check cleanly.
        if diagnostic.message.contains(ERROR_TYPE_TEXT) && self.diagnostics.has_errors() {
            return;
        }
        self.diagnostics
            .push(diagnostic.with_location(self.source.locate(span)));
    }

    /// Reports a filter literal's syntax error (Appendix A, section A.1):
    /// `err`'s span is an offset into the *decoded* filter source (`` \` ``
    /// and `\\` unescaped), which is mapped back to a span in this module's
    /// own source by adding the widths of the escape sequences before it,
    /// exactly as section A.1 says. `literal_span` is the whole literal,
    /// backticks included.
    fn report_filter_error(
        &mut self,
        err: &hollowscript_core::filter::FilterError,
        literal_span: Span,
    ) {
        let inner = Span::new(literal_span.start + 1, literal_span.end.saturating_sub(1));
        let raw = self.source.slice(inner).unwrap_or("");
        let start =
            inner.start + hollowscript_syntax::token::filter_raw_offset(raw, err.span.start);
        let end = inner.start + hollowscript_syntax::token::filter_raw_offset(raw, err.span.end);
        let mut diag =
            Diagnostic::new(err.code, &[]).with_location(self.source.locate(Span::new(start, end)));
        diag.message.clone_from(&err.message);
        self.diagnostics.push(diag);
    }

    fn error_with_related(
        &mut self,
        code: Code,
        args: &[(&str, &dyn std::fmt::Display)],
        span: Span,
        related: Span,
        related_message: &str,
    ) {
        self.diagnostics.push(
            Diagnostic::new(code, args)
                .with_location(self.source.locate(span))
                .with_related(self.source.locate(related), related_message),
        );
    }

    fn text(&self, symbol: Symbol) -> String {
        self.names.resolve(symbol).to_owned()
    }

    fn type_text(&self, ty: TypeId) -> String {
        let names_ref = &*self.names;
        self.types.display(ty, |s| names_ref.resolve(s).to_owned())
    }

    /// Resolves `name`, walking the current frame outward through any
    /// enclosing closures and finally the file's own value scope
    /// (section 4.6), registering captures along the way (section 5.4).
    fn resolve_value(&mut self, name: Symbol, purpose: Purpose) -> Resolved {
        if self.frames.is_empty() {
            return self.resolve_file_level(name, purpose);
        }
        self.resolve_value_at(name, self.frames.len() - 1, purpose)
    }

    /// Records that the declaration at `span` was read, which is what stops
    /// it warning under section 17.3. Called for a read and for nothing
    /// else.
    fn record_use(&mut self, span: Span, purpose: Purpose) {
        if purpose == Purpose::Read {
            self.used.insert(span);
        }
    }

    /// Reports section 17.3's HS0801 and HS0802 for every declared name and
    /// every imported name that nothing read. Run once the whole file has
    /// been checked, because "a name is *used* if a reference that resolves
    /// to it is read anywhere": a local read by a closure declared after the
    /// scope that holds it closed still counts, so no scope can decide this
    /// as it pops. Warnings are reported whether or not the program has
    /// errors.
    pub fn report_unused_names(&mut self, imports: &[ResolvedImport]) {
        // An import binds into both namespaces, so it is collected here
        // rather than where it was declared: either use is a use.
        for import in imports {
            // An import that resolved to nothing is already HS0604 or
            // HS0601, and section 17.3 is explicit that "a reference that
            // does not resolve is not a use": there is no name here to be
            // unused.
            if import.value.is_none() && import.ty.is_none() {
                continue;
            }
            if self.used.contains(&import.name.span) {
                continue;
            }
            let text = self.text(import.name.name);
            self.error_at(Code::HS0802, &[("name", &text)], import.name.span);
        }
        for candidate in std::mem::take(&mut self.unused) {
            if self.used.contains(&candidate.span) {
                continue;
            }
            let text = self.text(candidate.name);
            self.error_at(candidate.code, &[("name", &text)], candidate.span);
        }
    }

    /// Registers a name that warns if nothing reads it, unless it begins
    /// with `_`, which section 17.3 exempts.
    fn warn_if_unused(&mut self, code: Code, name: Symbol, span: Span) {
        let text = self.names.resolve(name);
        // The parser's placeholder for a name it could not read (a reserved
        // word, a missing one) is the empty name: there is nothing there to
        // call unused, and the parser has already said what was wrong.
        if text.is_empty() || text.starts_with('_') {
            return;
        }
        self.unused.push(UnusedName { code, name, span });
    }

    fn resolve_file_level(&mut self, name: Symbol, purpose: Purpose) -> Resolved {
        let found = self.values.lookup_entry(name);
        if let Some((_, span)) = found {
            self.record_use(span, purpose);
        }
        match found.map(|(binding, _)| binding) {
            Some(ValueBinding::Global(gid)) => Resolved::Global(gid),
            Some(ValueBinding::Function(fid)) => Resolved::Function(fid),
            Some(ValueBinding::Capability(cid)) => Resolved::Capability(cid),
            Some(ValueBinding::EnumNamespace(decl)) => Resolved::EnumNamespace(decl),
            Some(ValueBinding::Unresolved) => Resolved::Unresolved,
            Some(ValueBinding::Local(_) | ValueBinding::Prelude(_)) | None => {
                self.resolve_prelude_value(name)
            }
        }
    }

    /// A name no scope declared: a prelude value, or `Result` itself, which
    /// is the one enum the prelude names (section 8.2's `Result.Ok(v)`).
    fn resolve_prelude_value(&mut self, name: Symbol) -> Resolved {
        let text = self.names.resolve(name).to_owned();
        if text == "Result" {
            let decl = self.types.result_decl(self.names);
            return Resolved::EnumNamespace(decl);
        }
        // Section 15.1: "A variant is written through the enum name
        // (`Json.Int(3)`, `Json.Null`)".
        if text == "Json" {
            let decl = self.types.json_decl(self.names);
            return Resolved::EnumNamespace(decl);
        }
        resolve::lookup_prelude_value(&text).map_or(Resolved::NotFound, Resolved::Prelude)
    }

    fn resolve_value_at(&mut self, name: Symbol, frame_idx: usize, purpose: Purpose) -> Resolved {
        let found_local = self
            .frames
            .get(frame_idx)
            .and_then(|f| f.locals.lookup_entry(name));
        if let Some((local, span)) = found_local {
            // A read inside a closure is a read of the binding it captured,
            // which section 17.3 states outright: "including inside a
            // closure".
            self.record_use(span, purpose);
            if frame_idx == self.frames.len() - 1 {
                return Resolved::Local(local);
            }
            let index = self.capture_into(frame_idx + 1, name, CaptureSource::Local(local));
            return Resolved::Upvalue(index);
        }
        if frame_idx == 0 {
            let found = self.values.lookup_entry(name);
            if let Some((_, span)) = found {
                self.record_use(span, purpose);
            }
            return match found.map(|(binding, _)| binding) {
                // A top-level binding is reachable from every function of
                // its module by its global slot, so it is read where it is
                // written and never captured: capturing would snapshot its
                // value, and section 5.4 requires a closure to see a later
                // assignment to it.
                Some(ValueBinding::Global(gid)) => Resolved::Global(gid),
                Some(ValueBinding::Function(fid)) => Resolved::Function(fid),
                Some(ValueBinding::Capability(cid)) => Resolved::Capability(cid),
                Some(ValueBinding::EnumNamespace(decl)) => Resolved::EnumNamespace(decl),
                Some(ValueBinding::Unresolved) => Resolved::Unresolved,
                Some(ValueBinding::Local(_) | ValueBinding::Prelude(_)) | None => {
                    self.resolve_prelude_value(name)
                }
            };
        }
        self.resolve_value_at(name, frame_idx - 1, purpose)
    }

    /// Registers a capture of `source` starting at `first_frame`, forwarding
    /// it as an upvalue through every frame up to and including the current
    /// one, and returns the current frame's upvalue index for it. Reuses an
    /// existing capture of the same name in a frame that already has one.
    fn capture_into(&mut self, first_frame: usize, name: Symbol, source: CaptureSource) -> u32 {
        let existing = self
            .frames
            .get(first_frame)
            .and_then(|f| f.capture_by_name.get(&name))
            .copied();
        let Some(mut index) = existing.or_else(|| {
            let frame = self.frames.get_mut(first_frame)?;
            let idx = u32::try_from(frame.captures.len()).unwrap_or(u32::MAX);
            frame.captures.push(match source {
                CaptureSource::Local(local) => Capture::Local(local),
            });
            frame.capture_by_name.insert(name, idx);
            Some(idx)
        }) else {
            return 0;
        };
        for frame_idx in (first_frame + 1)..self.frames.len() {
            let existing = self
                .frames
                .get(frame_idx)
                .and_then(|f| f.capture_by_name.get(&name))
                .copied();
            if let Some(existing) = existing {
                index = existing;
                continue;
            }
            let Some(frame) = self.frames.get_mut(frame_idx) else {
                break;
            };
            let idx = u32::try_from(frame.captures.len()).unwrap_or(u32::MAX);
            frame.captures.push(Capture::Upvalue(index));
            frame.capture_by_name.insert(name, idx);
            index = idx;
        }
        index
    }

    /// The frame currently being checked. Every caller of this runs either
    /// between [`Checker::check_function_body_inner`] pushing a frame and
    /// popping it, or between [`Checker::check_top_level`]'s own pseudo-frame
    /// push and pop, so the stack is never empty here; a fresh frame is
    /// pushed in the case it somehow is, rather than indexing an empty
    /// vector.
    fn current_frame(&mut self) -> &mut FnCtx {
        if self.frames.is_empty() {
            self.frames.push(FnCtx::new(None, None));
        }
        let len = self.frames.len();
        self.frames
            .get_mut(len - 1)
            .unwrap_or_else(|| unreachable!("just ensured the frame stack is non-empty"))
    }

    /// Declares a binding (a parameter, `let` name or pattern binding) in
    /// the current frame's innermost scope, reporting HS0201 if the name is
    /// already visible there or is a top-level name of the file
    /// (section 4.6). `_` is never declared into scope at all, which is what
    /// makes reading it always miss and fall through to HS0213.
    fn declare_binding(&mut self, name: Symbol, span: Span, ty: TypeId) -> LocalId {
        self.declare_binding_kind(name, span, ty, false)
    }

    fn declare_binding_kind(
        &mut self,
        name: Symbol,
        span: Span,
        ty: TypeId,
        mutable: bool,
    ) -> LocalId {
        let local = self.current_frame().declare_local(ty);
        if mutable {
            self.current_frame().mutable_locals.insert(local);
            if self.shared_mutable.contains(&span) {
                self.current_frame().shared_locals.insert(local);
            }
        }
        let text = self.names.resolve(name).to_owned();
        if text == "_" {
            return local;
        }
        let clash = self
            .current_frame()
            .locals
            .first_declaration(name)
            .or_else(|| self.values.first_declaration(name));
        if let Some(first) = clash {
            self.error_with_related(
                Code::HS0201,
                &[("name", &text)],
                span,
                first,
                "first declared here",
            );
        }
        // Declared even after a clash. Section 4.6 forbids the shadowing and
        // HS0201 says so, but the declaration is still a declaration: the
        // code the author wrote below it means the name they just bound, and
        // section 17.3's unused-name warning then lands on whichever of the
        // two nothing reads. Nothing above the clash can see it, because it
        // is in an inner scope.
        self.current_frame().locals.declare(resolve::ScopeEntry {
            name,
            binding: local,
            span,
        });
        local
    }

    /// Declares a name that a `constant` or `variable` binds, which is the
    /// only kind section 17.3 warns about when nothing reads it: a
    /// parameter, a `for` variable and a `match` arm binding all go through
    /// [`Checker::declare_binding`] and never warn.
    fn declare_declared_binding(
        &mut self,
        name: Symbol,
        span: Span,
        ty: TypeId,
        mutable: bool,
    ) -> LocalId {
        let local = self.declare_binding_kind(name, span, ty, mutable);
        self.warn_if_unused(Code::HS0801, name, span);
        local
    }

    /// Checks a block of statements in order (chapter 4), threading
    /// narrowing facts through. Returns the checked block, the facts true
    /// after it completes, and whether it "always returns" (section 4.7).
    fn check_block(&mut self, block: &ast::Block, facts: FactSet) -> (Block, FactSet, bool) {
        self.current_frame().locals.push();
        let mut stmts = Vec::with_capacity(block.stmts.len());
        let mut current_facts = facts;
        let mut always_returns = false;
        // Section 4.7's unreachable rule is about all three: "A statement
        // that follows a `return`, `break` or `continue` in the same block
        // is HS0343. The check is purely syntactic". `always_returns` is a
        // different question, about whether a function's paths all return,
        // which a loop's `break` does not settle.
        let mut ended = false;
        let mut reported_unreachable = false;
        for stmt in &block.stmts {
            if ended && !reported_unreachable {
                self.error_at(Code::HS0343, &[], stmt.span);
                reported_unreachable = true;
            }
            ended = ended
                || matches!(
                    stmt.kind,
                    ast::StmtKind::Return(_) | ast::StmtKind::Break | ast::StmtKind::Continue
                );
            let (checked, new_facts, this_always_returns) = self.check_stmt(stmt, current_facts);
            current_facts = new_facts;
            stmts.push(checked);
            always_returns = always_returns || this_always_returns;
        }
        self.current_frame().locals.pop();
        (Block { stmts }, current_facts, always_returns)
    }

    fn check_stmt(&mut self, stmt: &ast::Stmt, facts: FactSet) -> (Stmt, FactSet, bool) {
        match &stmt.kind {
            ast::StmtKind::Constant(decl) => self.check_binding_decl(stmt, decl, false, facts),
            ast::StmtKind::Variable(decl) => self.check_binding_decl(stmt, decl, true, facts),
            ast::StmtKind::Assign { target, value } => {
                self.check_assign(stmt, target, value, facts)
            }
            ast::StmtKind::If(if_stmt) => self.check_if(stmt, if_stmt, facts),
            ast::StmtKind::Match(match_stmt) => self.check_match_stmt(stmt, match_stmt, facts),
            ast::StmtKind::For {
                target,
                iterable,
                body,
            } => self.check_for(stmt, target, iterable, body, &facts),
            ast::StmtKind::While { condition, body } => {
                self.check_while(stmt, condition, body, &facts)
            }
            ast::StmtKind::Break => self.check_break_continue(stmt, true, facts),
            ast::StmtKind::Continue => self.check_break_continue(stmt, false, facts),
            ast::StmtKind::Return(value) => self.check_return(stmt, value.as_ref(), facts),
            ast::StmtKind::Call(expr) => self.check_call_stmt(stmt, expr, facts),
            ast::StmtKind::Misplaced | ast::StmtKind::Error => (
                Stmt {
                    node: stmt.id,
                    kind: StmtKind::Call(error_expr(stmt.id, self.types)),
                },
                facts,
                false,
            ),
        }
    }

    /// **Why a declaration does not report HS0304 of its own.**
    ///
    /// Every initialiser whose type genuinely cannot be inferred reports it
    /// where it is checked, and says which construct it is about: an empty
    /// list is "The type of this list cannot be inferred", an empty map is
    /// "this map", a bare `none` is "this expression". A declaration asking
    /// the same question again could only ever be one of two things: that
    /// same report a second time, or a report about a type that is the
    /// error type because something *else* already failed, where an
    /// annotation would not have helped. Section 17.1 forbids both: "A
    /// diagnostic is not reported for an expression whose problem has
    /// already been reported ... One mistake gives one diagnostic wherever
    /// this is possible."
    ///
    /// The second was real. `constant x = a`, where `a` is imported from a
    /// module in an import cycle, asked for an annotation on top of the
    /// cycle error, in a file that had nothing wrong with it.
    fn check_binding_decl(
        &mut self,
        stmt: &ast::Stmt,
        decl: &ast::BindingDecl,
        mutable: bool,
        facts: FactSet,
    ) -> (Stmt, FactSet, bool) {
        let expected = decl.ty.as_ref().map(|t| self.resolve_type(t));
        let checked_init = match expected {
            Some(t) => self.check_against(&decl.init, t, &facts),
            // An initialiser with no annotation is not asked here whether
            // its type can be inferred: see this function's own note.
            None => self.check_expr(&decl.init, None, &facts),
        };
        let ty = expected.unwrap_or(checked_init.ty);
        let mut bindings = Vec::new();
        let pattern = match &decl.target {
            ast::BindingTarget::Name(ident) => {
                self.declare_declared_binding(ident.name, ident.span, ty, mutable);
                None
            }
            ast::BindingTarget::Pattern(pat) => {
                Some(self.check_declaration_pattern(pat, ty, mutable, &mut bindings))
            }
        };
        if pattern.is_none() {
            if let ast::BindingTarget::Name(ident) = &decl.target {
                let local = self
                    .current_frame()
                    .locals
                    .lookup(ident.name)
                    .unwrap_or(LocalId(0));
                bindings.push(LocalBinding {
                    node: stmt.id,
                    local,
                    name: ident.name,
                    ty,
                });
            }
        }
        let kind = StmtKind::Let {
            bindings,
            pattern,
            init: checked_init,
        };
        (
            Stmt {
                node: stmt.id,
                kind,
            },
            facts,
            false,
        )
    }

    fn check_declaration_pattern(
        &mut self,
        pat: &ast::Pattern,
        ty: TypeId,
        mutable: bool,
        bindings: &mut Vec<LocalBinding>,
    ) -> Pattern {
        match &pat.kind {
            ast::PatternKind::Binding(ident) => {
                let local = self.declare_declared_binding(ident.name, ident.span, ty, mutable);
                bindings.push(LocalBinding {
                    node: pat.id,
                    local,
                    name: ident.name,
                    ty,
                });
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Binding(Some(local)),
                }
            }
            ast::PatternKind::Wildcard => Pattern {
                node: pat.id,
                ty,
                kind: PatternKind::Binding(None),
            },
            ast::PatternKind::Record(fields) => {
                let TypeView::Record(rec_fields) = self.types.view(ty) else {
                    self.error_at(Code::HS0509, &[], pat.span);
                    self.declare_pattern_names_as_error(pat, mutable, bindings);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::Record(Vec::new()),
                    };
                };
                let rec_fields = rec_fields.to_vec();
                let mut built = Vec::new();
                for f in fields {
                    // The field's rank in the *record type's* own order,
                    // which is what reads it: a pattern may write a subset,
                    // in any order, so its own position means nothing.
                    let Some(position) = rec_fields.iter().position(|(n, _)| *n == f.name.name)
                    else {
                        let text = self.text(f.name.name);
                        self.error_at(Code::HS0507, &[("name", &text)], f.name.span);
                        continue;
                    };
                    let field_ty = rec_fields
                        .get(position)
                        .map_or_else(|| self.types.error(), |&(_, ty)| ty);
                    let sub = if let Some(p) = &f.pattern {
                        self.check_declaration_pattern(p, field_ty, mutable, bindings)
                    } else {
                        let local = self.declare_declared_binding(
                            f.name.name,
                            f.name.span,
                            field_ty,
                            mutable,
                        );
                        bindings.push(LocalBinding {
                            node: pat.id,
                            local,
                            name: f.name.name,
                            ty: field_ty,
                        });
                        Pattern {
                            node: pat.id,
                            ty: field_ty,
                            kind: PatternKind::Binding(Some(local)),
                        }
                    };
                    built.push(hir::RecordFieldPattern {
                        name: f.name.name,
                        position: u32::try_from(position).unwrap_or(u32::MAX),
                        pattern: sub,
                    });
                }
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Record(built),
                }
            }
            ast::PatternKind::List(elements) => {
                let TypeView::List(elem_ty) = self.types.view(ty) else {
                    self.error_at(Code::HS0509, &[], pat.span);
                    self.declare_pattern_names_as_error(pat, mutable, bindings);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::List {
                            elements: Vec::new(),
                            rest: None,
                        },
                    };
                };
                let mut built = Vec::new();
                let mut rest = None;
                for el in elements {
                    match el {
                        ast::ListElement::Pattern(p) => {
                            built.push(
                                self.check_declaration_pattern(p, elem_ty, mutable, bindings),
                            );
                        }
                        ast::ListElement::Rest { binding, .. } => {
                            let rest_ty = self.types.list(elem_ty);
                            rest = Some(binding.as_ref().map(|ident| {
                                let local = self.declare_declared_binding(
                                    ident.name, ident.span, rest_ty, mutable,
                                );
                                bindings.push(LocalBinding {
                                    node: pat.id,
                                    local,
                                    name: ident.name,
                                    ty: rest_ty,
                                });
                                local
                            }));
                        }
                    }
                }
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::List {
                        elements: built,
                        rest,
                    },
                }
            }
            _ => {
                self.error_at(Code::HS0509, &[], pat.span);
                self.declare_pattern_names_as_error(pat, mutable, bindings);
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Binding(None),
                }
            }
        }
    }

    /// Declares every name `pat` would have bound, with the error type,
    /// after `check_declaration_pattern` already reported one HS0509 about
    /// the pattern as a whole: a later read of a name it would have bound
    /// must not also be HS0205 (section 17.1, one mistake gives one
    /// diagnostic). Bypasses the normal per-kind checking in
    /// `check_declaration_pattern` entirely, which is what keeps this from
    /// reporting a second HS0509 of its own for a nested pattern.
    fn declare_pattern_names_as_error(
        &mut self,
        pat: &ast::Pattern,
        mutable: bool,
        bindings: &mut Vec<LocalBinding>,
    ) {
        let mut names = Vec::new();
        crate::shared::pattern_names(pat, &mut names);
        let error_ty = self.types.error();
        for ident in names {
            let local = self.declare_declared_binding(ident.name, ident.span, error_ty, mutable);
            bindings.push(LocalBinding {
                node: pat.id,
                local,
                name: ident.name,
                ty: error_ty,
            });
        }
    }

    fn check_assign(
        &mut self,
        stmt: &ast::Stmt,
        target: &ast::Ident,
        value: &ast::Expr,
        facts: FactSet,
    ) -> (Stmt, FactSet, bool) {
        // Section 17.3: "Assigning to a `variable` is not a use."
        let resolved = self.resolve_value(target.name, Purpose::Inspect);
        match resolved {
            Resolved::Local(local) => {
                if !self
                    .frames
                    .last()
                    .is_some_and(|f| f.mutable_locals.contains(&local))
                {
                    let text = self.text(target.name);
                    self.error_at(Code::HS0206, &[("name", &text)], target.span);
                }
                let ty = self.local_type(local);
                let checked_value = self.check_against(value, ty, &facts);
                let new_facts = facts.without_root(PathRoot::Local(local));
                (
                    Stmt {
                        node: stmt.id,
                        kind: StmtKind::Assign {
                            local,
                            value: checked_value,
                        },
                    },
                    new_facts,
                    false,
                )
            }
            // Section 5.4: a closure captures a variable by reference, so
            // assigning it here is seen by the function that declared it
            // and by every other closure that captured it.
            Resolved::Upvalue(index) => {
                if !self.upvalue_is_mutable(index) {
                    let text = self.text(target.name);
                    self.error_at(Code::HS0206, &[("name", &text)], target.span);
                }
                let ty = self.upvalue_type(index);
                let checked_value = self.check_against(value, ty, &facts);
                (
                    Stmt {
                        node: stmt.id,
                        kind: StmtKind::AssignUpvalue {
                            index,
                            value: checked_value,
                        },
                    },
                    facts,
                    false,
                )
            }
            Resolved::Global(global) => {
                if !self.global_is_mutable(global) {
                    let text = self.text(target.name);
                    self.error_at(Code::HS0206, &[("name", &text)], target.span);
                }
                let ty = self.global_type(global);
                let checked_value = self.check_against(value, ty, &facts);
                let new_facts = facts.without_root(PathRoot::Global(global));
                (
                    Stmt {
                        node: stmt.id,
                        kind: StmtKind::AssignGlobal {
                            global,
                            value: checked_value,
                        },
                    },
                    new_facts,
                    false,
                )
            }
            _ => {
                let text = self.text(target.name);
                self.error_at(Code::HS0206, &[("name", &text)], target.span);
                self.check_expr(value, None, &facts);
                (
                    Stmt {
                        node: stmt.id,
                        kind: StmtKind::Call(error_expr(stmt.id, self.types)),
                    },
                    facts,
                    false,
                )
            }
        }
    }

    fn check_if(
        &mut self,
        stmt: &ast::Stmt,
        if_stmt: &ast::IfStmt,
        facts: FactSet,
    ) -> (Stmt, FactSet, bool) {
        let bool_ty = self.types.bool();
        let checked_condition = self.check_expr(&if_stmt.condition, Some(bool_ty), &facts);
        if checked_condition.ty != bool_ty && !self.types.is_error(checked_condition.ty) {
            let found = self.type_text(checked_condition.ty);
            self.error_at(Code::HS0316, &[("found", &found)], if_stmt.condition.span);
        }
        let truth = narrow::truth_sets(&checked_condition, self.types, self.module);
        let then_facts_set = facts.union(&truth.when_true);
        let (checked_then, then_exit, then_always) =
            self.check_block(&if_stmt.then, then_facts_set);
        let else_facts_set = facts.union(&truth.when_false);
        let (otherwise, else_exit, else_always) = match &if_stmt.otherwise {
            Some(else_branch) => {
                let (block, exit, always) = self.check_else(else_branch, else_facts_set);
                (Some(block), exit, always)
            }
            None => (None, else_facts_set, false),
        };
        let always_returns = then_always && else_always;
        let exit_facts = if always_returns {
            facts
        } else if then_always {
            else_exit
        } else if else_always {
            then_exit
        } else {
            then_exit.intersect(&else_exit)
        };
        let kind = StmtKind::If {
            condition: checked_condition,
            then_facts: truth.when_true.as_vec(),
            then: checked_then,
            else_facts: truth.when_false.as_vec(),
            otherwise,
        };
        (
            Stmt {
                node: stmt.id,
                kind,
            },
            exit_facts,
            always_returns,
        )
    }

    fn check_else(&mut self, else_branch: &ast::Else, facts: FactSet) -> (Block, FactSet, bool) {
        match else_branch {
            ast::Else::Block(block) => self.check_block(block, facts),
            ast::Else::If(inner_stmt) => {
                let ast::StmtKind::If(if_stmt) = &inner_stmt.kind else {
                    return (Block::default(), facts, false);
                };
                let (checked, exit, always) = self.check_if(inner_stmt, if_stmt, facts);
                (
                    Block {
                        stmts: vec![checked],
                    },
                    exit,
                    always,
                )
            }
        }
    }

    fn check_for(
        &mut self,
        stmt: &ast::Stmt,
        target: &ast::BindingTarget,
        iterable: &ast::Expr,
        body: &ast::Block,
        facts: &FactSet,
    ) -> (Stmt, FactSet, bool) {
        let checked_iterable = self.check_expr(iterable, None, facts);
        let elem_ty = match self.types.view(checked_iterable.ty) {
            TypeView::List(t) | TypeView::Set(t) => t,
            TypeView::Map(k, v) => {
                let key_sym = self.names.intern("key");
                let value_sym = self.names.intern("value");
                self.types.record(
                    self.names,
                    vec![
                        Field {
                            name: key_sym,
                            ty: k,
                        },
                        Field {
                            name: value_sym,
                            ty: v,
                        },
                    ],
                )
            }
            _ if self.types.is_error(checked_iterable.ty) => self.types.error(),
            _ => {
                let found = self.type_text(checked_iterable.ty);
                self.error_at(Code::HS0325, &[("found", &found)], iterable.span);
                self.types.error()
            }
        };
        let assigned = self.assigned_locals_in_block(body);
        let loop_facts = facts.without_roots(&assigned);
        self.current_frame().locals.push();
        let mut bindings = Vec::new();
        let pattern = match target {
            ast::BindingTarget::Name(ident) => {
                self.declare_binding(ident.name, ident.span, elem_ty);
                None
            }
            ast::BindingTarget::Pattern(pat) => {
                Some(self.check_declaration_pattern(pat, elem_ty, false, &mut bindings))
            }
        };
        if pattern.is_none() {
            if let ast::BindingTarget::Name(ident) = target {
                let local = self
                    .current_frame()
                    .locals
                    .lookup(ident.name)
                    .unwrap_or(LocalId(0));
                bindings.push(LocalBinding {
                    node: stmt.id,
                    local,
                    name: ident.name,
                    ty: elem_ty,
                });
            }
        }
        let was_in_loop = self.current_frame().in_loop;
        self.current_frame().in_loop = true;
        let (checked_body, _, _) = self.check_block(body, loop_facts.clone());
        self.current_frame().in_loop = was_in_loop;
        self.current_frame().locals.pop();
        let kind = StmtKind::For {
            bindings,
            pattern,
            element_ty: elem_ty,
            iterable: checked_iterable,
            body: checked_body,
        };
        (
            Stmt {
                node: stmt.id,
                kind,
            },
            loop_facts,
            false,
        )
    }

    fn check_while(
        &mut self,
        stmt: &ast::Stmt,
        condition: &ast::Expr,
        body: &ast::Block,
        facts: &FactSet,
    ) -> (Stmt, FactSet, bool) {
        let assigned = self.assigned_locals_in_expr_and_block(condition, body);
        let loop_facts = facts.without_roots(&assigned);
        let bool_ty = self.types.bool();
        let checked_condition = self.check_expr(condition, Some(bool_ty), &loop_facts);
        if checked_condition.ty != bool_ty && !self.types.is_error(checked_condition.ty) {
            let found = self.type_text(checked_condition.ty);
            self.error_at(Code::HS0316, &[("found", &found)], condition.span);
        }
        let truth = narrow::truth_sets(&checked_condition, self.types, self.module);
        let body_facts = loop_facts.union(&truth.when_true);
        let was_in_loop = self.current_frame().in_loop;
        self.current_frame().in_loop = true;
        let has_break = block_has_break(body);
        let (checked_body, _, _) = self.check_block(body, body_facts);
        self.current_frame().in_loop = was_in_loop;
        let exit_facts = if has_break {
            loop_facts.clone()
        } else {
            loop_facts.union(&truth.when_false)
        };
        (
            Stmt {
                node: stmt.id,
                kind: StmtKind::While {
                    condition: checked_condition,
                    loop_facts: loop_facts.as_vec(),
                    body: checked_body,
                },
            },
            exit_facts,
            false,
        )
    }

    fn check_break_continue(
        &mut self,
        stmt: &ast::Stmt,
        is_break: bool,
        facts: FactSet,
    ) -> (Stmt, FactSet, bool) {
        let in_loop = self.frames.last().is_some_and(|f| f.in_loop);
        if !in_loop {
            let word = if is_break { "break" } else { "continue" };
            self.error_at(Code::HS0344, &[("word", &word)], stmt.span);
        }
        let kind = if is_break {
            StmtKind::Break { depth: 0 }
        } else {
            StmtKind::Continue { depth: 0 }
        };
        (
            Stmt {
                node: stmt.id,
                kind,
            },
            facts,
            false,
        )
    }

    fn check_return(
        &mut self,
        stmt: &ast::Stmt,
        value: Option<&ast::Expr>,
        facts: FactSet,
    ) -> (Stmt, FactSet, bool) {
        if self.frames.last().is_some_and(|f| f.is_top_level) {
            // A top-level return (section 4.7): ends the module's own
            // initialisation, with a value of any type, but only in the
            // entry module (HS0611 otherwise).
            if !self.is_entry {
                self.error_at(Code::HS0611, &[], stmt.span);
            }
            let checked_value = value.map(|v| self.check_expr(v, None, &facts));
            return (
                Stmt {
                    node: stmt.id,
                    kind: StmtKind::Return(checked_value),
                },
                facts,
                true,
            );
        }
        let return_ty = self.frames.last().and_then(|f| f.return_ty);
        let infer_return = self.frames.last().is_some_and(|f| f.infer_return);
        let checked_value = match (value, return_ty) {
            (Some(v), Some(t)) => Some(self.check_against(v, t, &facts)),
            (Some(v), None) if infer_return => {
                let checked = self.check_expr(v, None, &facts);
                if let Some(frame) = self.frames.last_mut() {
                    frame.collected_returns.push(checked.ty);
                }
                Some(checked)
            }
            (Some(v), None) => {
                self.error_at(Code::HS0340, &[], stmt.span);
                Some(self.check_expr(v, None, &facts))
            }
            (None, Some(_)) => {
                self.error_at(Code::HS0341, &[], stmt.span);
                None
            }
            (None, None) => None,
        };
        (
            Stmt {
                node: stmt.id,
                kind: StmtKind::Return(checked_value),
            },
            facts,
            true,
        )
    }

    fn check_call_stmt(
        &mut self,
        stmt: &ast::Stmt,
        expr: &ast::Expr,
        facts: FactSet,
    ) -> (Stmt, FactSet, bool) {
        let checked = self.check_expr(expr, None, &facts);
        if self.types.as_result(checked.ty).is_some() {
            self.error_at(Code::HS0803, &[], stmt.span);
        }
        (
            Stmt {
                node: stmt.id,
                kind: StmtKind::Call(checked),
            },
            facts,
            false,
        )
    }

    /// Whether a value of `ty` has neither structure to match nor equality
    /// (section 7.2): a `Filter`, a function, or an optional of either.
    fn type_is_unmatchable(&mut self, ty: TypeId) -> bool {
        let inner = self.types.as_optional(ty).unwrap_or(ty);
        inner == self.types.filter() || matches!(self.types.view(inner), TypeView::Function { .. })
    }

    fn check_match_stmt(
        &mut self,
        stmt: &ast::Stmt,
        m: &ast::MatchStmt,
        facts: FactSet,
    ) -> (Stmt, FactSet, bool) {
        let checked_scrutinee = self.check_expr(&m.scrutinee, None, &facts);
        // Section 7.2: "A value of type `Filter` or of a function type
        // cannot be matched, because such values have no structure to match
        // and no equality (HS0510)." Reported on the scrutinee and the arms
        // left unchecked, since every one of them would report the same
        // thing again (section 17.1, no cascades).
        if self.type_is_unmatchable(checked_scrutinee.ty) {
            let found = self.type_text(checked_scrutinee.ty);
            self.error_at(Code::HS0510, &[("found", &found)], stmt.span);
            return (
                Stmt {
                    node: stmt.id,
                    kind: StmtKind::Call(checked_scrutinee),
                },
                facts,
                false,
            );
        }
        let scrutinee_path = narrow::path_of(&checked_scrutinee, self.types, self.module);
        let has_none_alt = m.arms.iter().any(|arm| {
            arm.patterns
                .iter()
                .any(|p| matches!(p.kind, ast::PatternKind::None))
        });
        let narrow_present = has_none_alt
            && scrutinee_path.is_some()
            && self.types.as_optional(checked_scrutinee.ty).is_some();
        let narrowed_ty = self.types.as_optional(checked_scrutinee.ty);

        let mut rows: Vec<exhaustive::Row> = Vec::new();
        let mut row_spans: Vec<Span> = Vec::new();
        let mut checked_arms: Vec<hir::Arm> = Vec::new();
        let mut exit_facts: Option<FactSet> = None;
        let mut all_always_return = true;
        let outer_patterns_met_error = std::mem::replace(&mut self.patterns_met_error, false);

        for arm in &m.arms {
            let multi = arm.patterns.len() > 1;
            self.current_frame().locals.push();
            let mut bindings = Vec::new();
            let mut hir_patterns = Vec::new();
            let arm_has_none = arm
                .patterns
                .iter()
                .any(|p| matches!(p.kind, ast::PatternKind::None));
            for pat in &arm.patterns {
                let mut columns = Vec::new();
                let mut alt_bindings = Vec::new();
                let top_narrowed = if narrow_present && !arm_has_none {
                    narrowed_ty
                } else {
                    None
                };
                let hir_pat = self.check_match_pattern(
                    pat,
                    checked_scrutinee.ty,
                    top_narrowed,
                    &mut alt_bindings,
                    &mut columns,
                );
                if multi {
                    if !alt_bindings.is_empty() {
                        self.error_at(Code::HS0508, &[], pat.span);
                    }
                } else {
                    bindings.extend(alt_bindings);
                }
                row_spans.push(pat.span);
                rows.push(exhaustive::Row { columns });
                hir_patterns.push(hir_pat);
            }
            let mut arm_facts = facts.clone();
            if narrow_present && !arm_has_none {
                if let Some(path) = &scrutinee_path {
                    arm_facts.insert(Fact { path: path.clone() });
                }
            }
            let (checked_body, arm_exit, arm_always) = self.check_block(&arm.body, arm_facts);
            self.current_frame().locals.pop();
            all_always_return = all_always_return && arm_always;
            if !arm_always {
                exit_facts = Some(match exit_facts {
                    None => arm_exit,
                    Some(e) => e.intersect(&arm_exit),
                });
            }
            checked_arms.push(hir::Arm {
                patterns: hir_patterns,
                bindings,
                body: checked_body,
            });
        }
        let has_else = m.else_arm.is_some();
        if let Some(else_block) = &m.else_arm {
            row_spans.push(stmt.span);
            rows.push(exhaustive::Row {
                columns: vec![exhaustive::Constructor::Wildcard],
            });
            // Section 7.6 narrows the scrutinee's path "in every arm that
            // does not have `none` among its patterns", and an `else` arm is
            // one of those arms: it is only reached once the `none` arm has
            // failed to match.
            let mut else_facts = facts.clone();
            if narrow_present {
                if let Some(path) = &scrutinee_path {
                    else_facts.insert(Fact { path: path.clone() });
                }
            }
            let (checked_body, arm_exit, arm_always) = self.check_block(else_block, else_facts);
            all_always_return = all_always_return && arm_always;
            if !arm_always {
                exit_facts = Some(match exit_facts {
                    None => arm_exit,
                    Some(e) => e.intersect(&arm_exit),
                });
            }
            checked_arms.push(hir::Arm {
                patterns: vec![Pattern {
                    node: stmt.id,
                    ty: if narrow_present {
                        narrowed_ty.unwrap_or(checked_scrutinee.ty)
                    } else {
                        checked_scrutinee.ty
                    },
                    kind: PatternKind::Binding(None),
                }],
                bindings: Vec::new(),
                body: checked_body,
            });
        }
        let scrutinee_constructors = self.scrutinee_constructors(checked_scrutinee.ty);
        let report = exhaustive::check_match(&rows, &scrutinee_constructors);
        let mut exhaustive_without_else = false;
        let patterns_met_error =
            std::mem::replace(&mut self.patterns_met_error, outer_patterns_met_error);
        if self.types.is_error(checked_scrutinee.ty) || patterns_met_error {
            // The scrutinee's own error was reported where it arose; whether
            // arms of a type nobody knows cover it, or overlap, is not a
            // second mistake (section 2.11). The same holds when only part
            // of the scrutinee's type is an error (`Result<Int, E>` with `E`
            // unknown): a pattern there matches unconditionally, so a later
            // arm would look unreachable only because of the first error.
        } else if report.too_complex {
            self.error_at(Code::HS0511, &[("limit", &"section 7.7's")], stmt.span);
        } else {
            exhaustive_without_else = report.exhaustive && !has_else;
            if !report.exhaustive && !has_else {
                self.error_at(Code::HS0501, &[("example", &"")], stmt.span);
            }
            for &(row_index, is_literal_repeat) in &report.unreachable {
                let span = row_spans.get(row_index).copied().unwrap_or(stmt.span);
                let code = if is_literal_repeat {
                    Code::HS0502
                } else {
                    Code::HS0503
                };
                self.error_at(code, &[], span);
            }
        }
        let final_facts = if all_always_return {
            facts
        } else {
            exit_facts.unwrap_or_else(FactSet::empty)
        };
        let kind = StmtKind::Match(hir::Match {
            scrutinee: checked_scrutinee,
            scrutinee_path,
            arms: checked_arms,
            exhaustive_without_else,
        });
        (
            Stmt {
                node: stmt.id,
                kind,
            },
            final_facts,
            all_always_return,
        )
    }

    /// The complete constructor set of a scrutinee's own type (section 7.7),
    /// consulted only for the outermost column of the exhaustiveness matrix;
    /// empty for a type that has none (`Int`, `Float`, `String`, `[T]`,
    /// `Map`, `Set`), for which the algorithm decides completeness from the
    /// rows themselves instead.
    fn scrutinee_constructors(&mut self, ty: TypeId) -> Vec<exhaustive::Constructor> {
        let ty = self.types.as_optional(ty).unwrap_or(ty);
        match self.types.view(ty) {
            TypeView::Bool => vec![
                exhaustive::Constructor::Bool(true),
                exhaustive::Constructor::Bool(false),
            ],
            TypeView::Record(fields) => vec![exhaustive::Constructor::Record {
                arity: u32::try_from(fields.len()).unwrap_or(0),
            }],
            TypeView::Named { decl, .. } => match self.types.decl_shape(decl).cloned() {
                Some(DeclShape::Enum { variants, .. }) => variants
                    .iter()
                    .enumerate()
                    .map(|(i, v)| exhaustive::Constructor::Variant {
                        index: u32::try_from(i).unwrap_or(0),
                        arity: u32::try_from(v.payload.len()).unwrap_or(0),
                    })
                    .collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn check_pattern_literal(
        &mut self,
        literal: &ast::Literal,
        negative: bool,
    ) -> (Constant, TypeId) {
        match literal {
            ast::Literal::Int { value, malformed } => {
                if *malformed || *value > 9_223_372_036_854_775_808 {
                    return (Constant::Int(0), self.types.error());
                }
                #[allow(
                    clippy::cast_possible_wrap,
                    reason = "checked above: value fits in an i64 magnitude"
                )]
                let magnitude = *value as i64;
                let v = if negative {
                    magnitude.wrapping_neg()
                } else {
                    magnitude
                };
                (Constant::Int(v), self.types.int())
            }
            ast::Literal::Float { value, malformed } => {
                if *malformed {
                    return (Constant::Float(0.0), self.types.error());
                }
                let v = if negative { -*value } else { *value };
                (Constant::Float(v), self.types.float())
            }
            ast::Literal::Bool(b) => (Constant::Bool(*b), self.types.bool()),
            ast::Literal::String(s) => {
                if s.malformed {
                    return (Constant::String(String::new()), self.types.error());
                }
                let text = s
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        ast::StringPart::Text(t) => Some(t.as_str()),
                        ast::StringPart::Interpolation(_) => None,
                    })
                    .collect();
                (Constant::String(text), self.types.string())
            }
            ast::Literal::None | ast::Literal::Filter { .. } => {
                (Constant::Int(0), self.types.error())
            }
        }
    }

    /// Declares every name `pat` binds, at any depth, with the error type
    /// `ty`, reporting nothing: the pattern is matched against a value
    /// whose type is already an error (see [`Checker::check_match_pattern`]).
    fn bind_error_pattern(
        &mut self,
        pat: &ast::Pattern,
        ty: TypeId,
        bindings: &mut Vec<LocalBinding>,
    ) {
        match &pat.kind {
            ast::PatternKind::Binding(ident) => self.bind_error_name(ident, pat.id, ty, bindings),
            ast::PatternKind::Variant { args, .. } => {
                for arg in args {
                    self.bind_error_pattern(arg, ty, bindings);
                }
            }
            ast::PatternKind::Record(fields) => {
                for field in fields {
                    match &field.pattern {
                        Some(p) => self.bind_error_pattern(p, ty, bindings),
                        None => self.bind_error_name(&field.name, pat.id, ty, bindings),
                    }
                }
            }
            ast::PatternKind::List(elements) => {
                for element in elements {
                    match element {
                        ast::ListElement::Pattern(p) => self.bind_error_pattern(p, ty, bindings),
                        ast::ListElement::Rest {
                            binding: Some(ident),
                            ..
                        } => self.bind_error_name(ident, pat.id, ty, bindings),
                        ast::ListElement::Rest { binding: None, .. } => {}
                    }
                }
            }
            ast::PatternKind::Literal { .. }
            | ast::PatternKind::None
            | ast::PatternKind::Wildcard
            | ast::PatternKind::Error => {}
        }
    }

    /// One name of [`Checker::bind_error_pattern`].
    fn bind_error_name(
        &mut self,
        ident: &ast::Ident,
        node: ast::NodeId,
        ty: TypeId,
        bindings: &mut Vec<LocalBinding>,
    ) {
        let local = self.declare_binding(ident.name, ident.span, ty);
        bindings.push(LocalBinding {
            node,
            local,
            name: ident.name,
            ty,
        });
    }

    /// Checks one pattern against a scrutinee type (chapter 7), building
    /// both the checked [`Pattern`] and the flattened
    /// [`exhaustive::Constructor`] columns [`exhaustive::check_match`] needs
    /// (see that module's own documentation for the flattening contract).
    /// `top_level_narrowed`, only meaningful for the top-level call from
    /// [`Checker::check_match_stmt`], is the type a binding or `_` should
    /// have when section 7.6 narrows the match's own optional scrutinee.
    fn check_match_pattern(
        &mut self,
        pat: &ast::Pattern,
        ty: TypeId,
        top_level_narrowed: Option<TypeId>,
        bindings: &mut Vec<LocalBinding>,
        columns: &mut Vec<exhaustive::Constructor>,
    ) -> Pattern {
        // A scrutinee (or a part of one) whose type is already an error has
        // been reported once where it went wrong (section 2.11: an error
        // type produces no further diagnostic). The pattern matches it
        // unconditionally, and every name it binds is declared with the
        // error type, so the arm's body does not cascade into HS0205 either.
        if self.types.is_error(ty) {
            self.patterns_met_error = true;
            columns.push(exhaustive::Constructor::Wildcard);
            self.bind_error_pattern(pat, ty, bindings);
            return Pattern {
                node: pat.id,
                ty,
                kind: PatternKind::Binding(None),
            };
        }
        if let Some(inner_ty) = self.types.as_optional(ty) {
            match &pat.kind {
                ast::PatternKind::None => {
                    columns.push(exhaustive::Constructor::None);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::None,
                    };
                }
                ast::PatternKind::Wildcard => {
                    columns.push(exhaustive::Constructor::Wildcard);
                    return Pattern {
                        node: pat.id,
                        ty: top_level_narrowed.unwrap_or(ty),
                        kind: PatternKind::Binding(None),
                    };
                }
                ast::PatternKind::Binding(ident) => {
                    columns.push(exhaustive::Constructor::Wildcard);
                    let bind_ty = top_level_narrowed.unwrap_or(ty);
                    let local = self.declare_binding(ident.name, ident.span, bind_ty);
                    bindings.push(LocalBinding {
                        node: pat.id,
                        local,
                        name: ident.name,
                        ty: bind_ty,
                    });
                    return Pattern {
                        node: pat.id,
                        ty: bind_ty,
                        kind: PatternKind::Binding(Some(local)),
                    };
                }
                _ => {
                    columns.push(exhaustive::Constructor::Some);
                    return self.check_match_pattern(pat, inner_ty, None, bindings, columns);
                }
            }
        }
        match &pat.kind {
            ast::PatternKind::Wildcard | ast::PatternKind::Error => {
                columns.push(exhaustive::Constructor::Wildcard);
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Binding(None),
                }
            }
            ast::PatternKind::Binding(ident) => {
                columns.push(exhaustive::Constructor::Wildcard);
                let local = self.declare_binding(ident.name, ident.span, ty);
                bindings.push(LocalBinding {
                    node: pat.id,
                    local,
                    name: ident.name,
                    ty,
                });
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Binding(Some(local)),
                }
            }
            ast::PatternKind::Literal { negative, literal } => {
                let (constant, lit_ty) = self.check_pattern_literal(literal, *negative);
                if !self.types.is_error(lit_ty) && lit_ty != ty && !self.types.is_error(ty) {
                    let found = self.type_text(ty);
                    self.error_at(Code::HS0504, &[("found", &found)], pat.span);
                }
                // `Bool` gets its own `Constructor::Bool`, not
                // `Constructor::Literal`: section 7.7 treats `Bool` as a
                // type with a complete, two-member signature (`true` and
                // `false` together cover it, with no `else` needed), which
                // `exhaustive::is_complete` only recognises for
                // `Constructor::Bool`. A `Bool` pattern pushing
                // `Constructor::Literal` instead, as any other literal
                // does, made that column permanently incomplete, in turn
                // rejecting matches over a plain `Bool` and, wherever a
                // `Bool` sits nested inside another pattern (`Bool?`, or
                // inside a list or record pattern), the matches around it.
                columns.push(match &constant {
                    Constant::Bool(b) => exhaustive::Constructor::Bool(*b),
                    _ => exhaustive::Constructor::Literal(constant.clone()),
                });
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Literal(constant),
                }
            }
            ast::PatternKind::None => {
                let found = self.type_text(ty);
                self.error_at(Code::HS0504, &[("found", &found)], pat.span);
                columns.push(exhaustive::Constructor::Wildcard);
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Binding(None),
                }
            }
            ast::PatternKind::Variant { name, args, .. } => {
                let variant_name_text = self.text(name.name);
                let decl_info = self.types.declaration_of(ty).map(|(d, _)| d);
                let Some(decl) = decl_info else {
                    let found = self.type_text(ty);
                    self.error_at(Code::HS0504, &[("found", &found)], pat.span);
                    columns.push(exhaustive::Constructor::Wildcard);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::Binding(None),
                    };
                };
                // The enum's own type arguments are substituted into every
                // payload, so that `Ok(text)` on a `Result<String, IoError>`
                // binds `text` as a `String` and not as the declaration's
                // first parameter.
                let variants = {
                    let names: &Interner = self.names;
                    self.types.instantiated_variants(names, ty)
                };
                let Some(variants) = variants else {
                    let found = self.type_text(ty);
                    self.error_at(Code::HS0504, &[("found", &found)], pat.span);
                    columns.push(exhaustive::Constructor::Wildcard);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::Binding(None),
                    };
                };
                let Some((index, variant)) = variants
                    .iter()
                    .enumerate()
                    .find(|(_, v)| v.name == name.name)
                else {
                    let enum_name = self
                        .types
                        .decl_name(decl)
                        .map(|s| self.text(s))
                        .unwrap_or_default();
                    self.error_at(
                        Code::HS0505,
                        &[("enum", &enum_name), ("name", &variant_name_text)],
                        name.span,
                    );
                    columns.push(exhaustive::Constructor::Wildcard);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::Binding(None),
                    };
                };
                if variant.payload.len() != args.len() {
                    self.error_at(
                        Code::HS0506,
                        &[
                            ("name", &variant_name_text),
                            ("expected", &variant.payload.len()),
                            ("found", &args.len()),
                        ],
                        pat.span,
                    );
                }
                let payload = variant.payload.clone();
                let idx = u32::try_from(index).unwrap_or(0);
                columns.push(exhaustive::Constructor::Variant {
                    index: idx,
                    arity: u32::try_from(payload.len()).unwrap_or(0),
                });
                let mut sub_patterns = Vec::new();
                for (i, p) in args.iter().enumerate() {
                    let sub_ty = payload
                        .get(i)
                        .copied()
                        .unwrap_or_else(|| self.types.error());
                    sub_patterns.push(self.check_match_pattern(p, sub_ty, None, bindings, columns));
                }
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Variant {
                        index: idx,
                        args: sub_patterns,
                    },
                }
            }
            ast::PatternKind::Record(fields) => {
                let TypeView::Record(rec_fields) = self.types.view(ty) else {
                    let found = self.type_text(ty);
                    self.error_at(Code::HS0504, &[("found", &found)], pat.span);
                    columns.push(exhaustive::Constructor::Wildcard);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::Binding(None),
                    };
                };
                let rec_fields = rec_fields.to_vec();
                columns.push(exhaustive::Constructor::Record {
                    arity: u32::try_from(rec_fields.len()).unwrap_or(0),
                });
                let mut built = Vec::new();
                // Walked in the record type's own order, so `position` is
                // the rank that reads the field: a pattern that writes a
                // subset leaves gaps in `built`, and its own indices mean
                // nothing.
                for (position, &(fname, fty)) in rec_fields.iter().enumerate() {
                    if let Some(fp) = fields.iter().find(|f| f.name.name == fname) {
                        let sub = if let Some(p) = &fp.pattern {
                            self.check_match_pattern(p, fty, None, bindings, columns)
                        } else {
                            columns.push(exhaustive::Constructor::Wildcard);
                            let local = self.declare_binding(fp.name.name, fp.name.span, fty);
                            bindings.push(LocalBinding {
                                node: pat.id,
                                local,
                                name: fp.name.name,
                                ty: fty,
                            });
                            Pattern {
                                node: pat.id,
                                ty: fty,
                                kind: PatternKind::Binding(Some(local)),
                            }
                        };
                        built.push(hir::RecordFieldPattern {
                            name: fname,
                            position: u32::try_from(position).unwrap_or(u32::MAX),
                            pattern: sub,
                        });
                    } else {
                        columns.push(exhaustive::Constructor::Wildcard);
                    }
                }
                for fp in fields {
                    if !rec_fields.iter().any(|(n, _)| *n == fp.name.name) {
                        let text = self.text(fp.name.name);
                        self.error_at(Code::HS0507, &[("name", &text)], fp.name.span);
                    }
                }
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::Record(built),
                }
            }
            ast::PatternKind::List(elements) => {
                let TypeView::List(elem_ty) = self.types.view(ty) else {
                    let found = self.type_text(ty);
                    self.error_at(Code::HS0504, &[("found", &found)], pat.span);
                    columns.push(exhaustive::Constructor::Wildcard);
                    return Pattern {
                        node: pat.id,
                        ty,
                        kind: PatternKind::List {
                            elements: Vec::new(),
                            rest: None,
                        },
                    };
                };
                // `exhaustive::Row`'s pre-order contract (see that module's
                // own documentation) puts a fixed-arity constructor before
                // the flattened columns of its own sub-patterns: the list's
                // own `ListLength` column must come first here, ahead of
                // each element's column, not after. A pass over `elements`
                // first, before descending into any of them, is what makes
                // that order possible: `length` and `or_more` are only
                // known once every element has been looked at.
                let length = u32::try_from(
                    elements
                        .iter()
                        .filter(|el| matches!(el, ast::ListElement::Pattern(_)))
                        .count(),
                )
                .unwrap_or(u32::MAX);
                let has_rest = elements
                    .iter()
                    .any(|el| matches!(el, ast::ListElement::Rest { .. }));
                columns.push(exhaustive::Constructor::ListLength {
                    length,
                    or_more: has_rest,
                });
                let mut built = Vec::new();
                let mut rest = None;
                for el in elements {
                    match el {
                        ast::ListElement::Pattern(p) => {
                            built.push(
                                self.check_match_pattern(p, elem_ty, None, bindings, columns),
                            );
                        }
                        ast::ListElement::Rest { binding, .. } => {
                            rest = Some(binding.as_ref().map(|ident| {
                                let rest_ty = self.types.list(elem_ty);
                                let local = self.declare_binding(ident.name, ident.span, rest_ty);
                                bindings.push(LocalBinding {
                                    node: pat.id,
                                    local,
                                    name: ident.name,
                                    ty: rest_ty,
                                });
                                local
                            }));
                        }
                    }
                }
                Pattern {
                    node: pat.id,
                    ty,
                    kind: PatternKind::List {
                        elements: built,
                        rest,
                    },
                }
            }
        }
    }

    /// The locals and top-level bindings assigned anywhere inside `block`,
    /// including inside a closure written there (section 4.8's rule for
    /// `while`): resolved by plain name lookup, first against the current
    /// frame's own scope and then, for a name that is not a local there,
    /// against the module's top-level bindings, so an assignment to a local
    /// of an *enclosing* frame reached through a closure is not found here,
    /// a narrower version of the rule than the specification's own (see
    /// [`FnCtx::collected_returns`]'s note on the same limitation).
    fn assigned_locals_in_block(&mut self, block: &ast::Block) -> Vec<PathRoot> {
        let mut names = Vec::new();
        assigned_names_in_block(block, &mut names);
        self.roots_of_assigned_names(names)
    }

    fn assigned_locals_in_expr_and_block(
        &mut self,
        expr: &ast::Expr,
        block: &ast::Block,
    ) -> Vec<PathRoot> {
        let mut names = Vec::new();
        assigned_names_in_expr(expr, &mut names);
        assigned_names_in_block(block, &mut names);
        self.roots_of_assigned_names(names)
    }

    /// Resolves each assigned name to the root it assigns: a local of the
    /// current frame if it is one there, otherwise a top-level binding of
    /// this module if it is one of those, otherwise dropped (an unresolved
    /// name is already reported elsewhere and narrows nothing).
    fn roots_of_assigned_names(&mut self, names: Vec<Symbol>) -> Vec<PathRoot> {
        names
            .into_iter()
            .filter_map(|n| {
                if let Some(local) = self.current_frame().locals.lookup(n) {
                    return Some(PathRoot::Local(local));
                }
                match self.values.lookup(n) {
                    Some(ValueBinding::Global(id)) => Some(PathRoot::Global(id)),
                    _ => None,
                }
            })
            .collect()
    }

    /// Checks one function or closure body (section 5), pushing a fresh
    /// frame, declaring its parameters, checking the body with no facts in
    /// force (a simplification of section 4.8's closure-boundary rule: never
    /// unsound, only more conservative than carrying restricted facts in
    /// from an enclosing scope), and registering the resulting
    /// [`hir::Function`] under a freshly minted [`FunctionId`].
    /// `infer_return` requests inferring the return type from the body's own
    /// `return` statements (section 2.10) when neither an annotation nor an
    /// expected function type gave one.
    #[allow(
        clippy::too_many_arguments,
        reason = "one entry point for every function and closure"
    )]
    fn check_function_body_inner(
        &mut self,
        node: ast::NodeId,
        name: Symbol,
        param_tys: &[TypeId],
        param_names: &[Symbol],
        param_spans: &[Span],
        return_ty: Option<TypeId>,
        infer_return: bool,
        body: &ast::Block,
        existing_id: Option<FunctionId>,
    ) -> (FunctionId, TypeId) {
        let result_err_ty = return_ty
            .and_then(|r| self.types.as_result(r))
            .map(|(_, e)| e);
        let mut frame = FnCtx::new(return_ty, result_err_ty);
        frame.infer_return = infer_return && return_ty.is_none();
        self.frames.push(frame);
        let mut params = Vec::with_capacity(param_tys.len());
        for (i, &ty) in param_tys.iter().enumerate() {
            let pname = param_names.get(i).copied().unwrap_or(name);
            let pspan = param_spans.get(i).copied().unwrap_or(body.span);
            let local = self.declare_binding(pname, pspan, ty);
            params.push(hir::Param {
                node,
                local,
                name: pname,
                ty,
            });
        }
        let (checked_body, _, always_returns) = self.check_block(body, FactSet::empty());
        if return_ty.is_some() && !infer_return && !always_returns {
            let text = self.text(name);
            self.error_at(Code::HS0342, &[("what", &text)], body.span);
        }
        let mut frame = self.frames.pop().unwrap_or_else(|| FnCtx::new(None, None));
        let final_ret = if infer_return {
            frame
                .collected_returns
                .drain(..)
                .reduce(|a, b| self.join_pair(a, b).unwrap_or(a))
        } else {
            return_ty
        };
        // A top-level function's id was already reserved in phase 1
        // (section 4.10's declaration pass), before any body, including its
        // own, was checked, so a forward reference to it resolves to the
        // same id its later body-check now fills in; only a closure, which
        // has no such reservation, mints a fresh one here.
        let id = existing_id.unwrap_or_else(|| {
            let id = FunctionId(*self.next_function_id);
            *self.next_function_id += 1;
            id
        });
        let function = Function {
            node,
            name,
            params,
            ret: final_ret,
            local_count: frame.local_count,
            captures: frame.captures,
            body: checked_body,
        };
        self.functions.insert(id.0, function);
        // Every function and closure of this module, not only its named
        // top-level ones: the compiler locates a fault by looking up the
        // function that raised it in whichever module named it here
        // (`hir::Module::function_ids`), and a closure that this list left
        // out was located nowhere at all (see that field's own note).
        self.module_function_ids.push(id);
        let fn_ty = self.types.function(false, param_tys.to_vec(), final_ret);
        (id, fn_ty)
    }

    /// Resolves a type expression to a [`TypeId`] (chapter 2, section 2.3):
    /// `Int`, `[T]`, `T?`, `(T)`, `{ ... }`, `fn(...)`/`effect fn(...)`, and a
    /// named type, either a type parameter in scope, a `record`/`enum`, or
    /// one of the prelude's generic types (`Result`, `Map`, `Set`,
    /// `MapEntry`). Reports HS0208 for an unknown name, HS0302/HS0303 for a
    /// wrong number of type arguments, and HS0335 if the result is too large
    /// (section 2.5); returns the error type on any failure, never panics.
    pub fn resolve_type(&mut self, ty: &ast::TypeExpr) -> TypeId {
        let result = match &ty.kind {
            ast::TypeKind::Named { name, args } => self.resolve_named_type(name, args),
            ast::TypeKind::List(inner) => {
                let inner = self.resolve_type(inner);
                self.types.list(inner)
            }
            ast::TypeKind::Optional(inner) => {
                let inner = self.resolve_type(inner);
                self.types.optional(inner)
            }
            ast::TypeKind::Paren(inner) => self.resolve_type(inner),
            ast::TypeKind::Record(fields) => self.resolve_record_type(fields, ty.span),
            ast::TypeKind::Fn {
                effect,
                params,
                ret,
            } => {
                let params = params.iter().map(|p| self.resolve_type(p)).collect();
                let ret = ret.as_ref().map(|r| self.resolve_type(r));
                self.types.function(*effect, params, ret)
            }
            ast::TypeKind::Error => self.types.error(),
        };
        if self.types.expanded_size(self.names, result) > crate::types::MAX_TYPE_NODES {
            self.error_at(
                Code::HS0335,
                &[("limit", &crate::types::MAX_TYPE_NODES)],
                ty.span,
            );
            return self.types.error();
        }
        result
    }

    fn resolve_record_type(&mut self, fields: &[ast::FieldType], span: Span) -> TypeId {
        let mut seen: Vec<Symbol> = Vec::new();
        let mut built = Vec::new();
        for field in fields {
            let text = self.text(field.name.name);
            if seen.contains(&field.name.name) {
                self.error_at(
                    Code::HS0202,
                    &[
                        ("kind", &"field"),
                        ("name", &text),
                        ("container", &"record type"),
                    ],
                    field.name.span,
                );
                continue;
            }
            seen.push(field.name.name);
            let ty = self.resolve_type(&field.ty);
            built.push(Field {
                name: field.name.name,
                ty,
            });
        }
        if built.is_empty() {
            self.error_at(
                Code::HS0112,
                &[("kind", &"record"), ("member", &"field")],
                span,
            );
            return self.types.error();
        }
        self.types.record(self.names, built)
    }

    fn resolve_named_type(&mut self, name: &ast::Ident, args: &[ast::TypeExpr]) -> TypeId {
        let text = self.text(name.name);
        // Writing an imported type in an annotation reads the import, which
        // is what stops an import that names only a type from warning.
        if let Some((binding, span)) = self.type_scope.lookup_entry(name.name) {
            self.record_use(span, Purpose::Read);
            return self.instantiate_type_binding(binding, args, name.span, &text);
        }
        if let Some(binding) = resolve::lookup_prelude_type(&text, self.types) {
            return self.instantiate_type_binding(binding, args, name.span, &text);
        }
        // The prelude types of chapter 15, which are built rather than
        // named: `Json` is an enum declaration and the other two are
        // records, so none of them fits `lookup_prelude_type`'s table.
        match text.as_str() {
            "Json" => {
                let decl = self.types.json_decl(self.names);
                return self.instantiate_type_binding(
                    TypeBinding::Named(decl),
                    args,
                    name.span,
                    &text,
                );
            }
            "JsonError" => return self.types.json_error(self.names),
            "TimeParts" => return self.types.time_parts(self.names),
            _ => {}
        }
        if let Some((_, decl)) = self.host_types.iter().find(|(n, _)| *n == name.name) {
            let binding = TypeBinding::Named(*decl);
            return self.instantiate_type_binding(binding, args, name.span, &text);
        }
        if self.ungranted_host_types.contains(&name.name) {
            return self.types.error();
        }
        self.error_at(Code::HS0208, &[("name", &text)], name.span);
        self.types.error()
    }

    fn instantiate_type_binding(
        &mut self,
        binding: TypeBinding,
        args: &[ast::TypeExpr],
        span: Span,
        text: &str,
    ) -> TypeId {
        match binding {
            // As for a value: the import that failed is the diagnostic.
            TypeBinding::Unresolved => self.types.error(),
            TypeBinding::Builtin(ty) => {
                if !args.is_empty() {
                    self.error_at(Code::HS0302, &[("name", &text)], span);
                    return self.types.error();
                }
                ty
            }
            TypeBinding::Param { decl, index } => {
                if !args.is_empty() {
                    self.error_at(Code::HS0302, &[("name", &text)], span);
                    return self.types.error();
                }
                self.types.param(decl, index)
            }
            TypeBinding::Named(decl) => {
                let expected_arity = match self.types.decl_shape(decl) {
                    Some(DeclShape::Record { params, .. } | DeclShape::Enum { params, .. }) => {
                        params.len()
                    }
                    None => 0,
                };
                if args.len() != expected_arity {
                    self.error_at(
                        Code::HS0303,
                        &[
                            ("name", &text),
                            ("expected", &expected_arity),
                            ("found", &args.len()),
                        ],
                        span,
                    );
                    return self.types.error();
                }
                let arg_tys: Vec<TypeId> = args.iter().map(|a| self.resolve_type(a)).collect();
                let named = self.types.named(decl, arg_tys);
                self.types.expand_record(self.names, named).unwrap_or(named)
            }
            TypeBinding::BuiltinGeneric(generic) => {
                if args.len() != generic.arity() {
                    self.error_at(
                        Code::HS0303,
                        &[
                            ("name", &text),
                            ("expected", &generic.arity()),
                            ("found", &args.len()),
                        ],
                        span,
                    );
                    return self.types.error();
                }
                let arg_tys: Vec<TypeId> = args.iter().map(|a| self.resolve_type(a)).collect();
                // Section 2.8: a `Map` key and a `Set` element must be a key
                // type. A map or set *literal* is checked where it is built;
                // this is the same rule where the type itself is written,
                // `Map<Float, Int>` in a signature or an annotation, which
                // no literal need ever reach.
                match generic {
                    resolve::BuiltinGeneric::Map => {
                        self.check_key_type(Code::HS0306, arg_tys.first().copied(), args, 0);
                    }
                    resolve::BuiltinGeneric::Set => {
                        self.check_key_type(Code::HS0307, arg_tys.first().copied(), args, 0);
                    }
                    resolve::BuiltinGeneric::Result | resolve::BuiltinGeneric::MapEntry => {}
                }
                generic.instantiate(self.types, self.names, &arg_tys)
            }
        }
    }

    /// Reports `code` when the type written at `position` is not a key type
    /// (section 2.8). An error type is left alone: whatever made it has
    /// already been reported.
    fn check_key_type(
        &mut self,
        code: Code,
        ty: Option<TypeId>,
        args: &[ast::TypeExpr],
        position: usize,
    ) {
        let Some(ty) = ty else { return };
        if self.types.is_key_type(ty) || self.types.is_error(ty) {
            return;
        }
        let Some(written) = args.get(position) else {
            return;
        };
        let found = self.type_text(ty);
        self.error_at(code, &[("found", &found)], written.span);
    }
}

fn error_expr(node: ast::NodeId, types: &mut TypeInterner) -> Expr {
    Expr {
        node,
        ty: types.error(),
        kind: ExprKind::Error,
    }
}

/// The value of a map literal's *literal key* (section 3.8), for comparing
/// two of them for equality without running the program.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LiteralMapKey {
    Int(i64),
    Bool(bool),
    Str(String),
}

impl LiteralMapKey {
    /// How the key reads in a diagnostic's message.
    fn display(&self) -> String {
        match self {
            LiteralMapKey::Int(n) => n.to_string(),
            LiteralMapKey::Bool(b) => b.to_string(),
            LiteralMapKey::Str(s) => format!("{s:?}"),
        }
    }
}

/// Section 3.8: a checked map key is a *literal key* if it is an Int, String
/// (without interpolation) or Bool literal, optionally preceded by one unary
/// minus. A string with no interpolation checks to
/// [`ExprKind::Interpolate`] with an empty `exprs`, its one `text` entry
/// already the decoded value, so this reads that rather than re-decoding the
/// source's own escapes.
fn literal_map_key(key: &Expr) -> Option<LiteralMapKey> {
    match &key.kind {
        ExprKind::Literal(Constant::Int(n)) => Some(LiteralMapKey::Int(*n)),
        ExprKind::Literal(Constant::Bool(b)) => Some(LiteralMapKey::Bool(*b)),
        ExprKind::Interpolate { text, exprs } if exprs.is_empty() => {
            text.first().cloned().map(LiteralMapKey::Str)
        }
        ExprKind::Unary {
            op: UnaryOp::NegInt,
            operand,
        } => match &operand.kind {
            ExprKind::Literal(Constant::Int(n)) => n.checked_neg().map(LiteralMapKey::Int),
            _ => None,
        },
        _ => None,
    }
}

impl Checker<'_> {
    fn error_expr_at(&mut self, node: ast::NodeId) -> Expr {
        error_expr(node, self.types)
    }

    /// Checks an argument or initialiser expression against an expected type
    /// (a coercion site, section 2.7), applying `Int`-to-`Float` coercion and
    /// reporting HS0301 if the result is still not assignable.
    fn check_against(&mut self, expr: &ast::Expr, expected: TypeId, facts: &FactSet) -> Expr {
        let checked = self.check_expr(expr, Some(expected), facts);
        let checked = apply_coercion(checked, Some(expected), self.types);
        if !is_assignable(checked.ty, expected, self.types) {
            let expected_text = self.type_text(expected);
            let found_text = self.type_text(checked.ty);
            self.error_at(
                Code::HS0301,
                &[("expected", &expected_text), ("found", &found_text)],
                expr.span,
            );
        }
        checked
    }

    /// Checks a top-level expression (chapter 3). `expected` is the expected
    /// type of section 2.9, threaded downward; `facts` are the narrowing
    /// facts in force at this point (section 4.8).
    pub fn check_expr(&mut self, expr: &ast::Expr, expected: Expected, facts: &FactSet) -> Expr {
        match &expr.kind {
            ast::ExprKind::Literal(lit) => self.check_literal(expr, lit, expected, facts),
            ast::ExprKind::Name(ident) => self.check_name(expr, ident, facts),
            ast::ExprKind::Paren(inner) => self.check_expr(inner, expected, facts),
            ast::ExprKind::List(items) => self.check_list(expr, items, expected, facts),
            ast::ExprKind::Map(entries) => self.check_map(expr, entries, expected, facts),
            ast::ExprKind::Record(fields) => self.check_record(expr, fields, expected, facts),
            ast::ExprKind::Closure(closure) => self.check_closure(expr, closure, expected),
            ast::ExprKind::Unary { op, operand } => self.check_unary(expr, *op, operand, facts),
            ast::ExprKind::Binary { op, lhs, rhs } => self.check_binary(expr, *op, lhs, rhs, facts),
            ast::ExprKind::Effect(inner) => self.check_effect(expr, inner, facts),
            ast::ExprKind::Call { callee, args } => {
                self.check_call(expr, callee, args, expected, facts, false)
            }
            ast::ExprKind::Index { target, index } => self.check_index(expr, target, index, facts),
            ast::ExprKind::Field { target, name } => {
                self.check_field(expr, target, name, expected, facts)
            }
            ast::ExprKind::Try(inner) => self.check_try(expr, inner, facts),
            ast::ExprKind::Error => self.error_expr_at(expr.id),
        }
    }

    fn check_literal(
        &mut self,
        expr: &ast::Expr,
        lit: &ast::Literal,
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        match lit {
            ast::Literal::Int { value, malformed } => {
                if *malformed || *value > i64::MAX as u64 {
                    if !*malformed {
                        self.error_at(Code::HS0014, &[], expr.span);
                    }
                    return self.error_expr_at(expr.id);
                }
                #[allow(
                    clippy::cast_possible_wrap,
                    reason = "checked above: value <= i64::MAX"
                )]
                let v = *value as i64;
                Expr {
                    node: expr.id,
                    ty: self.types.int(),
                    kind: ExprKind::Literal(Constant::Int(v)),
                }
            }
            ast::Literal::Float { value, malformed } => {
                if *malformed {
                    return self.error_expr_at(expr.id);
                }
                Expr {
                    node: expr.id,
                    ty: self.types.float(),
                    kind: ExprKind::Literal(Constant::Float(*value)),
                }
            }
            ast::Literal::Bool(b) => Expr {
                node: expr.id,
                ty: self.types.bool(),
                kind: ExprKind::Literal(Constant::Bool(*b)),
            },
            ast::Literal::None => {
                let Some(expected) = expected.and_then(|e| self.types.as_optional(e).map(|_| e))
                else {
                    self.error_at(Code::HS0304, &[("what", &"this expression")], expr.span);
                    return self.error_expr_at(expr.id);
                };
                Expr {
                    node: expr.id,
                    ty: expected,
                    kind: ExprKind::Literal(Constant::None),
                }
            }
            ast::Literal::String(s) => self.check_string_literal(expr, s, facts),
            ast::Literal::Filter { source, malformed } => {
                if *malformed {
                    return self.error_expr_at(expr.id);
                }
                // Recorded before the literal is parsed or validated, so that
                // a host that evaluates filters learns of a literal that is
                // not valid too, whatever the checker then says about it.
                self.filter_literals.push(crate::FilterLiteral {
                    source: source.clone(),
                    location: self.source.locate(expr.span),
                });
                let tree = match hollowscript_core::filter::parse(source) {
                    Ok(tree) => tree,
                    Err(e) => {
                        self.report_filter_error(&e, expr.span);
                        return self.error_expr_at(expr.id);
                    }
                };
                // Validated only when the host gave a schema (section A.7's
                // own rule: "without a schema only the checks that need
                // none are made"), one diagnostic per invalid predicate; a
                // filter with any such problem is invalid, the same as one
                // that failed to parse (section A.1).
                if let Some(schema) = self.filter_schema {
                    let problems = hollowscript_core::filter::validate(&tree, schema);
                    if !problems.is_empty() {
                        for problem in &problems {
                            self.report_filter_error(problem, expr.span);
                        }
                        return self.error_expr_at(expr.id);
                    }
                }
                Expr {
                    node: expr.id,
                    ty: self.types.filter(),
                    kind: ExprKind::Filter(source.clone()),
                }
            }
        }
    }

    /// A string literal, whose interpolations are ordinary expressions and
    /// so are checked with the facts in force: `"${n}"` after a guard clause
    /// that ruled out `none` reads the narrowed `n`, exactly as `return n`
    /// on the next line would.
    fn check_string_literal(
        &mut self,
        expr: &ast::Expr,
        s: &ast::StringLiteral,
        facts: &FactSet,
    ) -> Expr {
        if s.malformed {
            return self.error_expr_at(expr.id);
        }
        let mut text = vec![String::new()];
        let mut exprs = Vec::new();
        for part in &s.parts {
            match part {
                ast::StringPart::Text(t) => {
                    if let Some(last) = text.last_mut() {
                        last.push_str(t);
                    }
                }
                ast::StringPart::Interpolation(inner) => {
                    let checked = self.check_expr(inner, None, facts);
                    let string_ty = self.types.string();
                    let ok = checked.ty == self.types.int()
                        || checked.ty == self.types.float()
                        || checked.ty == self.types.bool()
                        || checked.ty == string_ty
                        || self.types.is_error(checked.ty);
                    if !ok {
                        let found_text = self.type_text(checked.ty);
                        self.error_at(Code::HS0317, &[("found", &found_text)], inner.span);
                    }
                    exprs.push(checked);
                    text.push(String::new());
                }
            }
        }
        Expr {
            node: expr.id,
            ty: self.types.string(),
            kind: ExprKind::Interpolate { text, exprs },
        }
    }

    fn check_name(&mut self, expr: &ast::Expr, ident: &ast::Ident, facts: &FactSet) -> Expr {
        let resolved = self.resolve_value(ident.name, Purpose::Read);
        match resolved {
            Resolved::Local(id) => {
                let path = Path {
                    root: PathRoot::Local(id),
                    fields: Vec::new(),
                };
                let ty = self.local_type(id);
                self.read_path_or_root(expr.id, ExprKind::Local(id), ty, path, facts)
            }
            Resolved::Upvalue(idx) => {
                let ty = self.upvalue_type(idx);
                Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::Upvalue(idx),
                }
            }
            Resolved::Global(id) => {
                let ty = self.global_type(id);
                let path = Path {
                    root: PathRoot::Global(id),
                    fields: Vec::new(),
                };
                self.read_path_or_root(expr.id, ExprKind::Global(id), ty, path, facts)
            }
            Resolved::Function(id) => {
                let ty = self.function_signature_type(id);
                Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::FunctionValue(id),
                }
            }
            Resolved::Capability(id) => {
                let ty = self.capability_type(id);
                Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::Capability(id),
                }
            }
            Resolved::EnumNamespace(_) => {
                let text = self.text(ident.name);
                self.error_at(
                    Code::HS0209,
                    &[("name", &text), ("kind", &"enum")],
                    ident.span,
                );
                self.error_expr_at(expr.id)
            }
            Resolved::Prelude(pv) => self.prelude_value(expr, ident, pv),
            // The import's own failure is the diagnostic; reading the name
            // it would have bound adds nothing (section 17.3, no cascades).
            Resolved::Unresolved => self.error_expr_at(expr.id),
            Resolved::NotFound => {
                if self.names.resolve(ident.name) == "_" {
                    self.error_at(Code::HS0213, &[], ident.span);
                } else {
                    let text = self.text(ident.name);
                    self.error_at(Code::HS0205, &[("name", &text)], ident.span);
                }
                self.error_expr_at(expr.id)
            }
        }
    }

    /// A prelude name used as a value rather than called (section 13.6
    /// lists them as values, so `constant f = sqrt` is a program).
    ///
    /// `pi` is a `Float` and is read as one. A name whose signature is built
    /// at the call site, because no written type stands for it (`keys` over
    /// any record, `set` over any element type), is HS0334: section 13.6
    /// says it "is not a value: using it without a call is HS0334".
    /// Everything else is the library function of the same name, which the
    /// VM can build a function value for.
    fn prelude_value(&mut self, expr: &ast::Expr, ident: &ast::Ident, pv: PreludeValue) -> Expr {
        let ty = pv.type_id(self.types);
        if matches!(pv, PreludeValue::Pi) {
            return Expr {
                node: expr.id,
                ty,
                kind: ExprKind::Literal(Constant::Float(std::f64::consts::PI)),
            };
        }
        let text = self.text(ident.name);
        if matches!(pv, PreludeValue::CallOnly) {
            self.error_at(Code::HS0334, &[("name", &text)], ident.span);
            return self.error_expr_at(expr.id);
        }
        let Some(position) = hollowscript_core::native::ROSTER
            .iter()
            .position(|n| n.name == text)
        else {
            // A prelude value this build has a type for and no library
            // function behind: reported rather than handed on as a value
            // nothing can build.
            self.error_at(Code::HS0334, &[("name", &text)], ident.span);
            return self.error_expr_at(expr.id);
        };
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::NativeValue(u32::try_from(position).unwrap_or(0)),
        }
    }

    /// Reads a local or top-level binding, using its narrowed inner type
    /// when `facts` says the path is not `none` (section 4.8). `plain` is
    /// the expression a bare, unnarrowed read of the root produces
    /// ([`ExprKind::Local`] or [`ExprKind::Global`]).
    fn read_path_or_root(
        &mut self,
        node: ast::NodeId,
        plain: ExprKind,
        ty: TypeId,
        path: Path,
        facts: &FactSet,
    ) -> Expr {
        if self.fact_holds(facts, &path) {
            if let Some(inner) = self.types.as_optional(ty) {
                return Expr {
                    node,
                    ty: inner,
                    kind: ExprKind::NarrowedPath(path),
                };
            }
        }
        Expr {
            node,
            ty,
            kind: plain,
        }
    }

    /// Whether `facts` says `path` is not `none` and section 4.8 lets the
    /// checker rely on it: never for a path rooted at a shared-mutable
    /// variable, local or top-level, which a call could change at any time.
    fn fact_holds(&self, facts: &FactSet, path: &Path) -> bool {
        if !facts.contains(path) {
            return false;
        }
        match path.root {
            PathRoot::Local(local) => !self
                .frames
                .last()
                .is_some_and(|f| f.shared_locals.contains(&local)),
            PathRoot::Global(global) => !self.shared_globals.contains(&global),
        }
    }

    /// The type of a local of the *current* frame (the only frame
    /// [`Resolved::Local`] is ever returned for).
    fn local_type(&mut self, id: LocalId) -> TypeId {
        self.frames
            .last()
            .and_then(|f| f.local_types.get(id.0 as usize))
            .copied()
            .unwrap_or_else(|| self.types.error())
    }

    /// The type of the current frame's upvalue `idx`, traced back through
    /// however many frames it was forwarded through to its original local
    /// or global (section 5.4).
    fn upvalue_type(&mut self, idx: u32) -> TypeId {
        if self.frames.is_empty() {
            return self.types.error();
        }
        self.upvalue_type_at(self.frames.len() - 1, idx)
    }

    fn upvalue_type_at(&mut self, frame: usize, idx: u32) -> TypeId {
        let capture = self
            .frames
            .get(frame)
            .and_then(|f| f.captures.get(idx as usize))
            .copied();
        match capture {
            Some(Capture::Local(id)) if frame > 0 => self
                .frames
                .get(frame - 1)
                .and_then(|f| f.local_types.get(id.0 as usize))
                .copied()
                .unwrap_or_else(|| self.types.error()),
            Some(Capture::Upvalue(forwarded)) if frame > 0 => {
                self.upvalue_type_at(frame - 1, forwarded)
            }
            _ => self.types.error(),
        }
    }

    /// Whether the current frame's upvalue `idx` was captured from a
    /// `variable`, traced back the same way as [`Checker::upvalue_type`].
    fn upvalue_is_mutable(&self, idx: u32) -> bool {
        let mut frame = self.frames.len();
        let mut idx = idx;
        while frame > 1 {
            frame -= 1;
            let capture = self
                .frames
                .get(frame)
                .and_then(|f| f.captures.get(idx as usize))
                .copied();
            match capture {
                Some(Capture::Local(id)) => {
                    return self
                        .frames
                        .get(frame - 1)
                        .is_some_and(|f| f.mutable_locals.contains(&id));
                }
                Some(Capture::Upvalue(forwarded)) => idx = forwarded,
                None => return false,
            }
        }
        false
    }

    fn global_type(&mut self, id: GlobalId) -> TypeId {
        self.global_types
            .get(&(id.module, id.index))
            .map_or_else(|| self.types.error(), |&(ty, _)| ty)
    }

    fn global_is_mutable(&self, id: GlobalId) -> bool {
        self.global_types
            .get(&(id.module, id.index))
            .is_some_and(|&(_, mutable)| mutable)
    }

    fn function_signature_type(&mut self, id: FunctionId) -> TypeId {
        match self.function_sigs.get(&id.0).cloned() {
            Some(sig) => self.types.function(false, sig.params, sig.ret),
            None => self.types.error(),
        }
    }

    fn capability_type(&mut self, id: CapabilityId) -> TypeId {
        self.capability_types
            .get(id.0 as usize)
            .copied()
            .unwrap_or_else(|| self.types.error())
    }
}

impl Checker<'_> {
    /// Substitutes every bound variable of `sub` into `ty`'s structure,
    /// leaving an unbound one (already reported as HS0308) as it is.
    fn substitute(&mut self, ty: TypeId, sub: &Substitution) -> TypeId {
        match self.types.view(ty) {
            TypeView::Param { index, .. } => sub.get(TypeVarId(index)).unwrap_or(ty),
            TypeView::List(t) => {
                let t = self.substitute(t, sub);
                self.types.list(t)
            }
            TypeView::Set(t) => {
                let t = self.substitute(t, sub);
                self.types.set(t)
            }
            TypeView::Optional(t) => {
                let t = self.substitute(t, sub);
                self.types.optional(t)
            }
            TypeView::Map(k, v) => {
                let k = self.substitute(k, sub);
                let v = self.substitute(v, sub);
                self.types.map(k, v)
            }
            TypeView::Record(fields) => {
                let fields: Vec<(Symbol, TypeId)> = fields.to_vec();
                let built = fields
                    .into_iter()
                    .map(|(name, t)| Field {
                        name,
                        ty: self.substitute(t, sub),
                    })
                    .collect();
                self.types.record(self.names, built)
            }
            TypeView::Named { decl, args } => {
                let args: Vec<TypeId> = args.to_vec();
                let built: Vec<TypeId> =
                    args.into_iter().map(|a| self.substitute(a, sub)).collect();
                self.types.named(decl, built)
            }
            TypeView::Function {
                effect,
                params,
                ret,
            } => {
                let params: Vec<TypeId> = params.to_vec();
                let built_params: Vec<TypeId> = params
                    .into_iter()
                    .map(|p| self.substitute(p, sub))
                    .collect();
                let built_ret = ret.map(|r| self.substitute(r, sub));
                self.types.function(effect, built_params, built_ret)
            }
            TypeView::Int
            | TypeView::Float
            | TypeView::Bool
            | TypeView::String
            | TypeView::Filter
            | TypeView::Error => ty,
        }
    }

    fn report_unify_error(&mut self, err: UnifyError, span: Span, function_name: &str) {
        match err {
            UnifyError::NotAssignable { expected, found } => {
                let expected_text = self.type_text(expected);
                let found_text = self.type_text(found);
                self.error_at(
                    Code::HS0301,
                    &[("expected", &expected_text), ("found", &found_text)],
                    span,
                );
            }
            UnifyError::Rebound { bound, found, .. } => {
                let expected_text = self.type_text(bound);
                let found_text = self.type_text(found);
                self.error_at(
                    Code::HS0301,
                    &[("expected", &expected_text), ("found", &found_text)],
                    span,
                );
            }
            UnifyError::Occurs { var } => {
                let name = format!("T{}", var.0);
                self.error_at(
                    Code::HS0308,
                    &[("name", &name), ("function", &function_name)],
                    span,
                );
            }
        }
    }

    /// Whether `ty`'s structure still refers to an unbound callee type
    /// parameter (section 2.10): true for a substituted parameter type that
    /// unification has not yet pinned down, so an argument checked against
    /// it should infer bottom-up instead of being held to a still-generic
    /// expected type.
    fn contains_unbound_param(&self, ty: TypeId) -> bool {
        match self.types.view(ty) {
            TypeView::Param { .. } => true,
            TypeView::List(elem) | TypeView::Set(elem) | TypeView::Optional(elem) => {
                self.contains_unbound_param(elem)
            }
            TypeView::Map(key, value) => {
                self.contains_unbound_param(key) || self.contains_unbound_param(value)
            }
            TypeView::Record(fields) => fields
                .iter()
                .any(|&(_, field_ty)| self.contains_unbound_param(field_ty)),
            TypeView::Named { args, .. } => args
                .iter()
                .any(|&arg_ty| self.contains_unbound_param(arg_ty)),
            TypeView::Function { params, ret, .. } => {
                params.iter().any(|&p| self.contains_unbound_param(p))
                    || ret.is_some_and(|r| self.contains_unbound_param(r))
            }
            TypeView::Int
            | TypeView::Float
            | TypeView::Bool
            | TypeView::String
            | TypeView::Filter
            | TypeView::Error => false,
        }
    }

    /// Checks a call's arguments against a callee's signature (section 2.10),
    /// running the generic-call algorithm; with `type_param_count` 0 this
    /// degenerates to an ordinary assignability check at every parameter,
    /// which is what makes this the one path both a plain and a generic call
    /// share. Reports HS0319 for a wrong argument count, HS0301/HS0308 from
    /// unification, and applies coercion (section 2.7) to each argument.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call site per kind of callee"
    )]
    fn check_call_args(
        &mut self,
        param_tys: &[TypeId],
        ret_ty: Option<TypeId>,
        type_param_count: u32,
        function_name: &str,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
        call_span: Span,
    ) -> (Vec<Expr>, TypeId) {
        if args.len() != param_tys.len() {
            self.error_at(
                Code::HS0319,
                &[("expected", &param_tys.len()), ("found", &args.len())],
                call_span,
            );
            let checked = args
                .iter()
                .map(|a| self.check_expr(a, None, facts))
                .collect();
            return (checked, self.types.error());
        }
        let mut sub = Substitution::new();
        let mut checked: Vec<Option<Expr>> = args.iter().map(|_| None).collect();
        // Section 2.10 defers a closure-literal argument to step 4, after
        // every other argument has bound what it can, because a closure
        // with no annotation needs its parameter type known first. An
        // empty list or map literal argument has exactly the same problem
        // for exactly the same reason (it has no expected type of its own
        // to fall back on either), which is what the section's own note on
        // `set([])` describes: a variable "only an empty list or map
        // literal argument could have bound" is reported at the end,
        // HS0308, "not HS0304", so it cannot have been checked, and failed,
        // in this first pass. Deferring it the same way a closure is
        // costs nothing when its parameter type was never generic: the
        // second pass then checks it against the very same expected type
        // the first pass would have.
        let is_deferred = |a: &ast::Expr| match &a.kind {
            ast::ExprKind::Closure(_) => true,
            ast::ExprKind::List(items) => items.is_empty(),
            ast::ExprKind::Map(entries) => entries.is_empty(),
            _ => false,
        };
        for (i, (&param, arg)) in param_tys.iter().zip(args.iter()).enumerate() {
            if is_deferred(arg) {
                continue;
            }
            let arg_expected = self.substitute(param, &sub);
            let expected_for_check = if self.contains_unbound_param(arg_expected) {
                None
            } else {
                Some(arg_expected)
            };
            let c = self.check_expr(arg, expected_for_check, facts);
            let c = apply_coercion(c, expected_for_check, self.types);
            if let Err(e) = unify(param, c.ty, self.types, &mut sub) {
                self.report_unify_error(e, arg.span, function_name);
            }
            if let Some(slot) = checked.get_mut(i) {
                *slot = Some(c);
            }
        }
        if type_param_count > 0 {
            if let (Some(ret), Some(exp)) = (ret_ty, expected) {
                let declared: Vec<TypeVarId> = (0..type_param_count).map(TypeVarId).collect();
                if !sub.unbound(&declared).is_empty() {
                    let _ = unify(ret, exp, self.types, &mut sub);
                }
            }
        }
        for (i, (&param, arg)) in param_tys.iter().zip(args.iter()).enumerate() {
            if !is_deferred(arg) {
                continue;
            }
            let resolved_param = self.substitute(param, &sub);
            let c = self.check_expr(arg, Some(resolved_param), facts);
            if let Err(e) = unify(param, c.ty, self.types, &mut sub) {
                self.report_unify_error(e, arg.span, function_name);
            }
            if let Some(slot) = checked.get_mut(i) {
                *slot = Some(c);
            }
        }
        if type_param_count > 0 {
            let declared: Vec<TypeVarId> = (0..type_param_count).map(TypeVarId).collect();
            for var in sub.unbound(&declared) {
                let name = format!("T{}", var.0);
                self.error_at(
                    Code::HS0308,
                    &[("name", &name), ("function", &function_name)],
                    call_span,
                );
            }
        }
        let mut final_args = Vec::with_capacity(checked.len());
        for (c, arg) in checked.into_iter().zip(args.iter()) {
            final_args.push(c.unwrap_or_else(|| error_expr(arg.id, self.types)));
        }
        let result_ty = match ret_ty {
            Some(r) => self.substitute(r, &sub),
            None => self.types.error(),
        };
        (final_args, result_ty)
    }

    fn check_unary(
        &mut self,
        expr: &ast::Expr,
        op: ast::UnaryOp,
        operand: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        match op {
            ast::UnaryOp::Not => {
                let checked = self.check_expr(operand, Some(self.types.bool()), facts);
                if checked.ty != self.types.bool() && !self.types.is_error(checked.ty) {
                    let found = self.type_text(checked.ty);
                    self.error_at(
                        Code::HS0315,
                        &[("op", &"not"), ("found", &found)],
                        operand.span,
                    );
                }
                Expr {
                    node: expr.id,
                    ty: self.types.bool(),
                    kind: ExprKind::Not(Box::new(checked)),
                }
            }
            ast::UnaryOp::Neg => {
                // 2^63 is only valid as an Int literal directly under a
                // minus sign, so it is folded here rather than checked as
                // its own (out of range) literal (section 1.10's HS0014).
                if let ast::ExprKind::Literal(ast::Literal::Int {
                    value: 9_223_372_036_854_775_808,
                    malformed: false,
                }) = operand.kind
                {
                    return Expr {
                        node: expr.id,
                        ty: self.types.int(),
                        kind: ExprKind::Literal(Constant::Int(i64::MIN)),
                    };
                }
                let checked = self.check_expr(operand, None, facts);
                if checked.ty == self.types.int() {
                    Expr {
                        node: expr.id,
                        ty: self.types.int(),
                        kind: ExprKind::Unary {
                            op: UnaryOp::NegInt,
                            operand: Box::new(checked),
                        },
                    }
                } else if checked.ty == self.types.float() {
                    Expr {
                        node: expr.id,
                        ty: self.types.float(),
                        kind: ExprKind::Unary {
                            op: UnaryOp::NegFloat,
                            operand: Box::new(checked),
                        },
                    }
                } else {
                    if !self.types.is_error(checked.ty) {
                        let found = self.type_text(checked.ty);
                        self.error_at(Code::HS0310, &[("found", &found)], operand.span);
                    }
                    self.error_expr_at(expr.id)
                }
            }
        }
    }

    fn check_binary(
        &mut self,
        expr: &ast::Expr,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        match op {
            ast::BinaryOp::And | ast::BinaryOp::Or => self.check_logical(expr, op, lhs, rhs, facts),
            ast::BinaryOp::Add | ast::BinaryOp::Sub | ast::BinaryOp::Mul => {
                self.check_arithmetic(expr, op, lhs, rhs, facts)
            }
            ast::BinaryOp::Div => self.check_division(expr, lhs, rhs, facts),
            ast::BinaryOp::Eq | ast::BinaryOp::Ne => self.check_equality(expr, op, lhs, rhs, facts),
            ast::BinaryOp::Lt | ast::BinaryOp::Gt | ast::BinaryOp::Le | ast::BinaryOp::Ge => {
                self.check_ordering(expr, op, lhs, rhs, facts)
            }
        }
    }

    fn check_logical(
        &mut self,
        expr: &ast::Expr,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        let bool_ty = self.types.bool();
        let checked_lhs = self.check_expr(lhs, Some(bool_ty), facts);
        if checked_lhs.ty != bool_ty && !self.types.is_error(checked_lhs.ty) {
            let found = self.type_text(checked_lhs.ty);
            let op_text = if matches!(op, ast::BinaryOp::And) {
                "and"
            } else {
                "or"
            };
            self.error_at(
                Code::HS0315,
                &[("op", &op_text), ("found", &found)],
                lhs.span,
            );
        }
        let lhs_sets = narrow::truth_sets(&checked_lhs, self.types, self.module);
        let rhs_facts = if matches!(op, ast::BinaryOp::And) {
            facts.union(&lhs_sets.when_true)
        } else {
            facts.union(&lhs_sets.when_false)
        };
        let checked_rhs = self.check_expr(rhs, Some(bool_ty), &rhs_facts);
        if checked_rhs.ty != bool_ty && !self.types.is_error(checked_rhs.ty) {
            let found = self.type_text(checked_rhs.ty);
            let op_text = if matches!(op, ast::BinaryOp::And) {
                "and"
            } else {
                "or"
            };
            self.error_at(
                Code::HS0315,
                &[("op", &op_text), ("found", &found)],
                rhs.span,
            );
        }
        let logical_op = if matches!(op, ast::BinaryOp::And) {
            LogicalOp::And
        } else {
            LogicalOp::Or
        };
        let extra_facts: Vec<Fact> = if matches!(op, ast::BinaryOp::And) {
            lhs_sets.when_true.as_vec()
        } else {
            lhs_sets.when_false.as_vec()
        };
        Expr {
            node: expr.id,
            ty: bool_ty,
            kind: ExprKind::Logical {
                op: logical_op,
                lhs: Box::new(checked_lhs),
                rhs: Box::new(checked_rhs),
                rhs_facts: extra_facts,
            },
        }
    }

    fn check_arithmetic(
        &mut self,
        expr: &ast::Expr,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        let checked_lhs = self.check_expr(lhs, None, facts);
        let checked_rhs = self.check_expr(rhs, None, facts);
        let int_ty = self.types.int();
        let float_ty = self.types.float();
        let both_error = self.types.is_error(checked_lhs.ty) || self.types.is_error(checked_rhs.ty);
        let is_num = |t: TypeId| t == int_ty || t == float_ty;
        if !both_error && (!is_num(checked_lhs.ty) || !is_num(checked_rhs.ty)) {
            let left = self.type_text(checked_lhs.ty);
            let right = self.type_text(checked_rhs.ty);
            let op_text = binary_op_text(op);
            self.error_at(
                Code::HS0311,
                &[("op", &op_text), ("left", &left), ("right", &right)],
                expr.span,
            );
            return self.error_expr_at(expr.id);
        }
        if both_error {
            return self.error_expr_at(expr.id);
        }
        let use_float = checked_lhs.ty == float_ty || checked_rhs.ty == float_ty;
        let lhs = apply_coercion(
            checked_lhs,
            Some(if use_float { float_ty } else { int_ty }),
            self.types,
        );
        let rhs = apply_coercion(
            checked_rhs,
            Some(if use_float { float_ty } else { int_ty }),
            self.types,
        );
        let (hir_op, ty) = match (op, use_float) {
            (ast::BinaryOp::Add, false) => (BinaryOp::AddInt, int_ty),
            (ast::BinaryOp::Sub, false) => (BinaryOp::SubInt, int_ty),
            (ast::BinaryOp::Mul, false) => (BinaryOp::MulInt, int_ty),
            (ast::BinaryOp::Add, true) => (BinaryOp::AddFloat, float_ty),
            (ast::BinaryOp::Sub, true) => (BinaryOp::SubFloat, float_ty),
            (ast::BinaryOp::Mul, true) => (BinaryOp::MulFloat, float_ty),
            _ => unreachable!("check_arithmetic is only called for +, - and *"),
        };
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::Binary {
                op: hir_op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        }
    }

    fn check_division(
        &mut self,
        expr: &ast::Expr,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        let checked_lhs = self.check_expr(lhs, None, facts);
        let checked_rhs = self.check_expr(rhs, None, facts);
        let int_ty = self.types.int();
        let float_ty = self.types.float();
        let both_error = self.types.is_error(checked_lhs.ty) || self.types.is_error(checked_rhs.ty);
        let is_num = |t: TypeId| t == int_ty || t == float_ty;
        if !both_error && (!is_num(checked_lhs.ty) || !is_num(checked_rhs.ty)) {
            let left = self.type_text(checked_lhs.ty);
            let right = self.type_text(checked_rhs.ty);
            self.error_at(
                Code::HS0311,
                &[("op", &"/"), ("left", &left), ("right", &right)],
                expr.span,
            );
            return self.error_expr_at(expr.id);
        }
        if both_error {
            return self.error_expr_at(expr.id);
        }
        let lhs = apply_coercion(checked_lhs, Some(float_ty), self.types);
        let rhs = apply_coercion(checked_rhs, Some(float_ty), self.types);
        Expr {
            node: expr.id,
            ty: float_ty,
            kind: ExprKind::Binary {
                op: BinaryOp::Div,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        }
    }

    fn check_equality(
        &mut self,
        expr: &ast::Expr,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        // A bare `none` literal has no type of its own (section 3.1), so
        // when one side of `==`/`!=` is written as `none` it is checked
        // against the other side's type rather than in isolation, letting
        // `x == none` infer `none`'s type from `x` instead of failing HS0304.
        let lhs_none_lit = matches!(lhs.kind, ast::ExprKind::Literal(ast::Literal::None));
        let rhs_none_lit = matches!(rhs.kind, ast::ExprKind::Literal(ast::Literal::None));
        let (checked_lhs, checked_rhs) = if lhs_none_lit && !rhs_none_lit {
            let checked_rhs = self.check_expr(rhs, None, facts);
            let expected = self
                .types
                .as_optional(checked_rhs.ty)
                .map(|_| checked_rhs.ty);
            (self.check_expr(lhs, expected, facts), checked_rhs)
        } else if rhs_none_lit && !lhs_none_lit {
            let checked_lhs = self.check_expr(lhs, None, facts);
            let expected = self
                .types
                .as_optional(checked_lhs.ty)
                .map(|_| checked_lhs.ty);
            (checked_lhs, self.check_expr(rhs, expected, facts))
        } else {
            (
                self.check_expr(lhs, None, facts),
                self.check_expr(rhs, None, facts),
            )
        };
        let bool_ty = self.types.bool();
        if self.types.is_error(checked_lhs.ty) || self.types.is_error(checked_rhs.ty) {
            return self.error_expr_at(expr.id);
        }
        let lhs_is_none = matches!(checked_lhs.kind, ExprKind::Literal(Constant::None));
        let rhs_is_none = matches!(checked_rhs.kind, ExprKind::Literal(Constant::None));
        let ok = if lhs_is_none || rhs_is_none {
            let other = if lhs_is_none {
                checked_rhs.ty
            } else {
                checked_lhs.ty
            };
            let other_optional =
                self.types.as_optional(other).is_some() || lhs_is_none && rhs_is_none;
            if other_optional {
                true
            } else {
                let found = self.type_text(other);
                self.error_at(Code::HS0328, &[("found", &found)], expr.span);
                false
            }
        } else {
            let equatable_pair = self.equatable_pair(checked_lhs.ty, checked_rhs.ty);
            match equatable_pair {
                Some(true) => true,
                Some(false) => {
                    let found = self.type_text(checked_lhs.ty);
                    self.error_at(Code::HS0314, &[("found", &found)], expr.span);
                    false
                }
                None => {
                    let left = self.type_text(checked_lhs.ty);
                    let right = self.type_text(checked_rhs.ty);
                    let op_text = if matches!(op, ast::BinaryOp::Eq) {
                        "=="
                    } else {
                        "!="
                    };
                    self.error_at(
                        Code::HS0313,
                        &[("left", &left), ("right", &right), ("op", &op_text)],
                        expr.span,
                    );
                    false
                }
            }
        };
        if !ok {
            return self.error_expr_at(expr.id);
        }
        let scalar = |t: TypeId| {
            t == self.types.int()
                || t == self.types.float()
                || t == self.types.bool()
                || self.types.as_optional(t).is_some()
        };
        let scalar_operands = scalar(checked_lhs.ty) && scalar(checked_rhs.ty);
        let hir_op = if matches!(op, ast::BinaryOp::Eq) {
            BinaryOp::Eq { scalar_operands }
        } else {
            BinaryOp::Ne { scalar_operands }
        };
        Expr {
            node: expr.id,
            ty: bool_ty,
            kind: ExprKind::Binary {
                op: hir_op,
                lhs: Box::new(checked_lhs),
                rhs: Box::new(checked_rhs),
            },
        }
    }

    /// Whether two operand types may be compared with `==`/`!=` (section 3.5):
    /// `Some(true)` if both equatable and either identical, or `Int`/`Float`
    /// in either combination with either optional; `Some(false)` if they
    /// would be comparable but are not equatable; `None` if the shapes do
    /// not correspond at all.
    fn equatable_pair(&mut self, lhs: TypeId, rhs: TypeId) -> Option<bool> {
        let lhs_inner = self.types.as_optional(lhs).unwrap_or(lhs);
        let rhs_inner = self.types.as_optional(rhs).unwrap_or(rhs);
        let int_ty = self.types.int();
        let float_ty = self.types.float();
        let numeric_pair = (lhs_inner == int_ty || lhs_inner == float_ty)
            && (rhs_inner == int_ty || rhs_inner == float_ty);
        if lhs_inner == rhs_inner || numeric_pair {
            Some(self.types.is_equatable(lhs) && self.types.is_equatable(rhs))
        } else {
            None
        }
    }

    fn check_ordering(
        &mut self,
        expr: &ast::Expr,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        let checked_lhs = self.check_expr(lhs, None, facts);
        let checked_rhs = self.check_expr(rhs, None, facts);
        let bool_ty = self.types.bool();
        if self.types.is_error(checked_lhs.ty) || self.types.is_error(checked_rhs.ty) {
            return self.error_expr_at(expr.id);
        }
        let int_ty = self.types.int();
        let float_ty = self.types.float();
        let string_ty = self.types.string();
        let is_num = |t: TypeId| t == int_ty || t == float_ty;
        let ok = (is_num(checked_lhs.ty) && is_num(checked_rhs.ty))
            || (checked_lhs.ty == string_ty && checked_rhs.ty == string_ty);
        if !ok {
            let left = self.type_text(checked_lhs.ty);
            let right = self.type_text(checked_rhs.ty);
            self.error_at(
                Code::HS0312,
                &[
                    ("op", &binary_op_text(op)),
                    ("left", &left),
                    ("right", &right),
                ],
                expr.span,
            );
            return self.error_expr_at(expr.id);
        }
        let hir_op = match op {
            ast::BinaryOp::Lt => BinaryOp::Lt,
            ast::BinaryOp::Gt => BinaryOp::Gt,
            ast::BinaryOp::Le => BinaryOp::Le,
            ast::BinaryOp::Ge => BinaryOp::Ge,
            _ => unreachable!("check_ordering is only called for <, >, <= and >="),
        };
        Expr {
            node: expr.id,
            ty: bool_ty,
            kind: ExprKind::Binary {
                op: hir_op,
                lhs: Box::new(checked_lhs),
                rhs: Box::new(checked_rhs),
            },
        }
    }
}

fn assigned_names_in_block(block: &ast::Block, names: &mut Vec<Symbol>) {
    for stmt in &block.stmts {
        assigned_names_in_stmt(stmt, names);
    }
}

fn assigned_names_in_stmt(stmt: &ast::Stmt, names: &mut Vec<Symbol>) {
    match &stmt.kind {
        ast::StmtKind::Assign { target, value } => {
            names.push(target.name);
            assigned_names_in_expr(value, names);
        }
        ast::StmtKind::Constant(decl) | ast::StmtKind::Variable(decl) => {
            assigned_names_in_expr(&decl.init, names);
        }
        ast::StmtKind::If(if_stmt) => {
            assigned_names_in_expr(&if_stmt.condition, names);
            assigned_names_in_block(&if_stmt.then, names);
            if let Some(otherwise) = &if_stmt.otherwise {
                match otherwise.as_ref() {
                    ast::Else::Block(b) => assigned_names_in_block(b, names),
                    ast::Else::If(s) => assigned_names_in_stmt(s, names),
                }
            }
        }
        ast::StmtKind::Match(m) => {
            assigned_names_in_expr(&m.scrutinee, names);
            for arm in &m.arms {
                assigned_names_in_block(&arm.body, names);
            }
            if let Some(else_arm) = &m.else_arm {
                assigned_names_in_block(else_arm, names);
            }
        }
        ast::StmtKind::For { iterable, body, .. } => {
            assigned_names_in_expr(iterable, names);
            assigned_names_in_block(body, names);
        }
        ast::StmtKind::While { condition, body } => {
            assigned_names_in_expr(condition, names);
            assigned_names_in_block(body, names);
        }
        ast::StmtKind::Return(Some(e)) | ast::StmtKind::Call(e) => assigned_names_in_expr(e, names),
        ast::StmtKind::Return(None)
        | ast::StmtKind::Break
        | ast::StmtKind::Continue
        | ast::StmtKind::Misplaced
        | ast::StmtKind::Error => {}
    }
}

fn assigned_names_in_expr(expr: &ast::Expr, names: &mut Vec<Symbol>) {
    match &expr.kind {
        ast::ExprKind::Closure(c) => assigned_names_in_block(&c.body, names),
        ast::ExprKind::List(items) => {
            for i in items {
                assigned_names_in_expr(i, names);
            }
        }
        ast::ExprKind::Map(entries) => {
            for e in entries {
                assigned_names_in_expr(&e.key, names);
                assigned_names_in_expr(&e.value, names);
            }
        }
        ast::ExprKind::Record(fields) => {
            for f in fields {
                assigned_names_in_expr(&f.value, names);
            }
        }
        ast::ExprKind::Paren(e) | ast::ExprKind::Effect(e) | ast::ExprKind::Try(e) => {
            assigned_names_in_expr(e, names);
        }
        ast::ExprKind::Unary { operand, .. } => assigned_names_in_expr(operand, names),
        ast::ExprKind::Binary { lhs, rhs, .. } => {
            assigned_names_in_expr(lhs, names);
            assigned_names_in_expr(rhs, names);
        }
        ast::ExprKind::Call { callee, args } => {
            assigned_names_in_expr(callee, names);
            for a in args {
                assigned_names_in_expr(a, names);
            }
        }
        ast::ExprKind::Index { target, index } => {
            assigned_names_in_expr(target, names);
            assigned_names_in_expr(index, names);
        }
        ast::ExprKind::Field { target, .. } => assigned_names_in_expr(target, names),
        ast::ExprKind::Literal(ast::Literal::String(s)) => {
            for p in &s.parts {
                if let ast::StringPart::Interpolation(e) = p {
                    assigned_names_in_expr(e, names);
                }
            }
        }
        ast::ExprKind::Literal(_) | ast::ExprKind::Name(_) | ast::ExprKind::Error => {}
    }
}

/// Whether `block` contains a `break` that targets its own innermost loop
/// (a `break` inside a loop nested in `block` targets that inner loop
/// instead, so this does not descend into one).
fn block_has_break(block: &ast::Block) -> bool {
    block.stmts.iter().any(stmt_has_break)
}

fn stmt_has_break(stmt: &ast::Stmt) -> bool {
    match &stmt.kind {
        ast::StmtKind::Break => true,
        ast::StmtKind::If(if_stmt) => {
            block_has_break(&if_stmt.then)
                || if_stmt
                    .otherwise
                    .as_ref()
                    .is_some_and(|e| match e.as_ref() {
                        ast::Else::Block(b) => block_has_break(b),
                        ast::Else::If(s) => stmt_has_break(s),
                    })
        }
        ast::StmtKind::Match(m) => {
            m.arms.iter().any(|a| block_has_break(&a.body))
                || m.else_arm.as_ref().is_some_and(block_has_break)
        }
        // A `for` or `while` here is a nested loop: its own `break` targets
        // it, not the loop this walk is checking, so it falls to the same
        // `false` as every other kind of statement.
        _ => false,
    }
}

fn binary_op_text(op: ast::BinaryOp) -> &'static str {
    match op {
        ast::BinaryOp::Or => "or",
        ast::BinaryOp::And => "and",
        ast::BinaryOp::Eq => "==",
        ast::BinaryOp::Ne => "!=",
        ast::BinaryOp::Lt => "<",
        ast::BinaryOp::Gt => ">",
        ast::BinaryOp::Le => "<=",
        ast::BinaryOp::Ge => ">=",
        ast::BinaryOp::Add => "+",
        ast::BinaryOp::Sub => "-",
        ast::BinaryOp::Mul => "*",
        ast::BinaryOp::Div => "/",
    }
}

impl Checker<'_> {
    /// The join of two element types (section 2.9): identical types join to
    /// themselves; `Int` and `Float` in either order join to `Float`; two
    /// function types that differ only in effect join to the effectful one.
    /// Anything else does not join.
    fn join_pair(&mut self, a: TypeId, b: TypeId) -> Option<TypeId> {
        if a == b {
            return Some(a);
        }
        let int_ty = self.types.int();
        let float_ty = self.types.float();
        if (a == int_ty && b == float_ty) || (a == float_ty && b == int_ty) {
            return Some(float_ty);
        }
        if let (
            TypeView::Function {
                effect: ea,
                params: pa,
                ret: ra,
            },
            TypeView::Function {
                effect: eb,
                params: pb,
                ret: rb,
            },
        ) = (self.types.view(a), self.types.view(b))
        {
            if pa == pb && ra == rb && ea != eb {
                return Some(if ea { a } else { b });
            }
        }
        None
    }

    /// Checks the elements of a list or map literal (section 2.9): with an
    /// expected element type, each is checked directly against it; without
    /// one, the join algorithm computes the element type from the elements
    /// themselves, an optional type or a `none` element making it optional.
    fn check_joined(
        &mut self,
        exprs: &[&ast::Expr],
        expected: Expected,
        facts: &FactSet,
        whole_span: Span,
    ) -> (Vec<Expr>, TypeId) {
        if let Some(expected_elem) = expected {
            let checked = exprs
                .iter()
                .map(|e| self.check_against(e, expected_elem, facts))
                .collect();
            return (checked, expected_elem);
        }
        let is_none_lit =
            |e: &ast::Expr| matches!(e.kind, ast::ExprKind::Literal(ast::Literal::None));
        let mut checked_non_none: Vec<Option<Expr>> = Vec::with_capacity(exprs.len());
        for e in exprs {
            checked_non_none.push(if is_none_lit(e) {
                None
            } else {
                Some(self.check_expr(e, None, facts))
            });
        }
        let mut join: Option<TypeId> = None;
        let mut saw_optional = exprs.iter().any(|e| is_none_lit(e));
        let mut reported = false;
        for (i, checked) in checked_non_none.iter().enumerate() {
            let Some(checked) = checked else { continue };
            if self.types.is_error(checked.ty) {
                reported = true;
                continue;
            }
            let (inner, was_optional) = match self.types.as_optional(checked.ty) {
                Some(inner) => (inner, true),
                None => (checked.ty, false),
            };
            saw_optional = saw_optional || was_optional;
            join = Some(match join {
                None => inner,
                Some(j) => {
                    if let Some(joined) = self.join_pair(j, inner) {
                        joined
                    } else {
                        if !reported {
                            let expected_text = self.type_text(j);
                            let found_text = self.type_text(inner);
                            self.error_at(
                                Code::HS0301,
                                &[("expected", &expected_text), ("found", &found_text)],
                                exprs.get(i).map_or(whole_span, |e| e.span),
                            );
                            reported = true;
                        }
                        j
                    }
                }
            });
        }
        let Some(mut elem_ty) = join else {
            self.error_at(Code::HS0304, &[("what", &"this expression")], whole_span);
            return (Vec::new(), self.types.error());
        };
        if saw_optional {
            elem_ty = self.types.optional(elem_ty);
        }
        let mut result = Vec::with_capacity(exprs.len());
        for (i, e) in exprs.iter().enumerate() {
            if is_none_lit(e) {
                result.push(Expr {
                    node: e.id,
                    ty: elem_ty,
                    kind: ExprKind::Literal(Constant::None),
                });
            } else {
                let checked = checked_non_none
                    .get_mut(i)
                    .and_then(Option::take)
                    .unwrap_or_else(|| error_expr(e.id, self.types));
                result.push(apply_coercion(checked, Some(elem_ty), self.types));
            }
        }
        (result, elem_ty)
    }

    fn check_list(
        &mut self,
        expr: &ast::Expr,
        items: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        let expected_elem = expected.and_then(|e| match self.types.view(e) {
            TypeView::List(inner) => Some(inner),
            _ => None,
        });
        if items.is_empty() {
            let Some(elem) = expected_elem else {
                self.error_at(Code::HS0304, &[("what", &"this list")], expr.span);
                return self.error_expr_at(expr.id);
            };
            let list_ty = self.types.list(elem);
            return Expr {
                node: expr.id,
                ty: list_ty,
                kind: ExprKind::List(Vec::new()),
            };
        }
        let refs: Vec<&ast::Expr> = items.iter().collect();
        let (checked, elem_ty) = self.check_joined(&refs, expected_elem, facts, expr.span);
        let list_ty = self.types.list(elem_ty);
        Expr {
            node: expr.id,
            ty: list_ty,
            kind: ExprKind::List(checked),
        }
    }

    /// Section 3.8's literal-key duplicate rule: a *literal key* is an Int,
    /// String (without interpolation) or Bool literal, optionally preceded
    /// by one unary minus; two of them are compared for equality before the
    /// program runs, `"a"` and `"\u{61}"` included since both decode to the
    /// same string, and the second is HS0203. Any other key expression is
    /// not compared here: a duplicate a run turns up instead replaces the
    /// earlier entry, per the same section. Reads `checked_keys`, already
    /// checked and therefore already carrying a string literal's decoded
    /// text, rather than re-decoding `entries`' own escapes.
    fn check_duplicate_literal_keys(&mut self, entries: &[ast::MapEntry], checked_keys: &[Expr]) {
        let mut seen: Vec<LiteralMapKey> = Vec::new();
        for (entry, checked_key) in entries.iter().zip(checked_keys) {
            let Some(literal) = literal_map_key(checked_key) else {
                continue;
            };
            if seen.contains(&literal) {
                let text = literal.display();
                self.error_at(
                    Code::HS0203,
                    &[("kind", &"key"), ("name", &text), ("container", &"map")],
                    entry.key.span,
                );
            } else {
                seen.push(literal);
            }
        }
    }

    fn check_map(
        &mut self,
        expr: &ast::Expr,
        entries: &[ast::MapEntry],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        let expected_kv = expected.and_then(|e| match self.types.view(e) {
            TypeView::Map(k, v) => Some((k, v)),
            _ => None,
        });
        if entries.is_empty() {
            let Some((k, v)) = expected_kv else {
                self.error_at(Code::HS0304, &[("what", &"this map")], expr.span);
                return self.error_expr_at(expr.id);
            };
            let map_ty = self.types.map(k, v);
            return Expr {
                node: expr.id,
                ty: map_ty,
                kind: ExprKind::Map(Vec::new()),
            };
        }
        let key_refs: Vec<&ast::Expr> = entries.iter().map(|e| &e.key).collect();
        let value_refs: Vec<&ast::Expr> = entries.iter().map(|e| &e.value).collect();
        let (checked_keys, key_ty) =
            self.check_joined(&key_refs, expected_kv.map(|(k, _)| k), facts, expr.span);
        if !self.types.is_key_type(key_ty) && !self.types.is_error(key_ty) {
            let found = self.type_text(key_ty);
            self.error_at(Code::HS0306, &[("found", &found)], expr.span);
        }
        self.check_duplicate_literal_keys(entries, &checked_keys);
        let (checked_values, value_ty) =
            self.check_joined(&value_refs, expected_kv.map(|(_, v)| v), facts, expr.span);
        let map_ty = self.types.map(key_ty, value_ty);
        let pairs = checked_keys.into_iter().zip(checked_values).collect();
        Expr {
            node: expr.id,
            ty: map_ty,
            kind: ExprKind::Map(pairs),
        }
    }

    fn check_record(
        &mut self,
        expr: &ast::Expr,
        fields: &[ast::FieldInit],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        let expected_fields = expected.and_then(|e| match self.types.view(e) {
            TypeView::Record(f) => Some(f.to_vec()),
            _ => None,
        });
        let mut seen: Vec<Symbol> = Vec::new();
        let mut checked_fields: Vec<(Symbol, Expr)> = Vec::new();
        for field in fields {
            if seen.contains(&field.name.name) {
                let text = self.text(field.name.name);
                self.error_at(
                    Code::HS0203,
                    &[
                        ("kind", &"field"),
                        ("name", &text),
                        ("container", &"record"),
                    ],
                    field.name.span,
                );
                continue;
            }
            seen.push(field.name.name);
            let field_expected = expected_fields.as_ref().and_then(|fs| {
                fs.iter()
                    .find(|(n, _)| *n == field.name.name)
                    .map(|&(_, t)| t)
            });
            let checked_value = match field_expected {
                Some(t) => self.check_against(&field.value, t, facts),
                None => self.check_expr(&field.value, None, facts),
            };
            checked_fields.push((field.name.name, checked_value));
        }
        if let Some(expected_fields) = &expected_fields {
            for (name, _) in expected_fields {
                if !seen.contains(name) {
                    let text = self.text(*name);
                    self.error_at(Code::HS0326, &[("name", &text)], expr.span);
                }
            }
            for name in &seen {
                if !expected_fields.iter().any(|(n, _)| n == name) {
                    let text = self.text(*name);
                    let type_text = expected.map(|e| self.type_text(e)).unwrap_or_default();
                    self.error_at(
                        Code::HS0327,
                        &[("name", &text), ("type", &type_text)],
                        expr.span,
                    );
                }
            }
        }
        // Section 2.9: a record literal with an expected type is checked
        // field by field against that type, and is that type. Rebuilding the
        // type from the checked fields instead would give `{ owner: String }`
        // for a literal written for a `{ owner: String? }` field, because
        // `String` is assignable to `String?` (section 2.7) without changing
        // the expression's own type; records are invariant, so the literal
        // would then be rejected against the very type it was checked
        // against. Any real mismatch has already been reported field by
        // field, so taking the expected type here also stops one bad field
        // from producing a second, whole-record diagnostic.
        let ty = match expected {
            Some(expected) if expected_fields.is_some() => expected,
            _ => {
                let record_fields: Vec<Field> = checked_fields
                    .iter()
                    .map(|(name, e)| Field {
                        name: *name,
                        ty: e.ty,
                    })
                    .collect();
                self.types.record(self.names, record_fields)
            }
        };
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::Record(checked_fields),
        }
    }

    fn check_closure(
        &mut self,
        expr: &ast::Expr,
        closure: &ast::Closure,
        expected: Expected,
    ) -> Expr {
        let expected_fn = expected.and_then(|e| match self.types.view(e) {
            TypeView::Function { params, ret, .. } => Some((params.to_vec(), ret)),
            _ => None,
        });
        let mut param_tys = Vec::with_capacity(closure.params.len());
        for (i, param) in closure.params.iter().enumerate() {
            let ty = match &param.ty {
                Some(t) => self.resolve_type(t),
                None => {
                    if let Some(&t) = expected_fn.as_ref().and_then(|(p, _)| p.get(i)) {
                        t
                    } else {
                        let text = self.text(param.name.name);
                        self.error_at(Code::HS0309, &[("name", &text)], param.name.span);
                        self.types.error()
                    }
                }
            };
            param_tys.push(ty);
        }
        let declared_ret = closure.ret.as_ref().map(|t| self.resolve_type(t));
        let return_ty = declared_ret.or_else(|| expected_fn.as_ref().and_then(|(_, r)| *r));
        let node = expr.id;
        let closure_name = self.names.intern("<closure>");
        let param_names: Vec<Symbol> = closure.params.iter().map(|p| p.name.name).collect();
        let param_spans: Vec<Span> = closure.params.iter().map(|p| p.name.span).collect();
        let infer_return =
            declared_ret.is_none() && expected_fn.as_ref().and_then(|(_, r)| *r).is_none();
        let (id, fn_ty) = self.check_function_body_inner(
            node,
            closure_name,
            &param_tys,
            &param_names,
            &param_spans,
            return_ty,
            infer_return,
            &closure.body,
            None,
        );
        Expr {
            node,
            ty: fn_ty,
            kind: ExprKind::Closure(id),
        }
    }

    fn report_effect_marker_error(
        &mut self,
        err: effects::EffectMarkerError,
        span: Span,
        callee_ty: TypeId,
    ) {
        let found = self.type_text(callee_ty);
        match err {
            effects::EffectMarkerError::MissingEffect => {
                self.error_at(Code::HS0401, &[("found", &found)], span);
            }
            effects::EffectMarkerError::UnexpectedEffect => {
                self.error_at(Code::HS0402, &[("found", &found)], span);
            }
        }
    }

    /// `effect call` (section 9.3): the marker applies to a postfix chain
    /// whose last operation, ignoring trailing `?`, is a call. Trailing `?`
    /// operators are peeled here and reapplied to the effect call's result.
    fn check_effect(&mut self, expr: &ast::Expr, inner: &ast::Expr, facts: &FactSet) -> Expr {
        let mut tries: u32 = 0;
        let mut base = inner;
        while let ast::ExprKind::Try(sub) = &base.kind {
            tries += 1;
            base = sub;
        }
        let ast::ExprKind::Call { callee, args } = &base.kind else {
            self.error_at(Code::HS0403, &[], expr.span);
            return self.check_expr(inner, None, facts);
        };
        let mut checked = self.check_call(base, callee, args, None, facts, true);
        for _ in 0..tries {
            checked = self.apply_try(checked, base.span);
        }
        checked
    }

    fn check_call(
        &mut self,
        expr: &ast::Expr,
        callee: &ast::Expr,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
        marked_effect: bool,
    ) -> Expr {
        if let ast::ExprKind::Field { target, name } = &callee.kind {
            if let ast::ExprKind::Name(ident) = &target.kind {
                // `json` and `time` are prelude namespaces (chapter 15):
                // records of pure functions, every one of which is a roster
                // entry, so a call of one is a native call. A program that
                // declares its own `json` shadows them, which
                // `resolve_value` finding something else expresses.
                let namespace = self.text(ident.name);
                if matches!(namespace.as_str(), "json" | "time")
                    && matches!(
                        self.resolve_value(ident.name, Purpose::Inspect),
                        Resolved::NotFound
                    )
                {
                    let member = self.text(name.name);
                    return self.check_data_namespace_call(
                        expr, &namespace, &member, name.span, args, expected, facts,
                    );
                }
                if let Resolved::EnumNamespace(decl) = self.resolve_value(ident.name, Purpose::Read)
                {
                    // `Result.Ok` and `Result.Err` are generic in both
                    // arguments, which the plain variant path does not
                    // infer.
                    if decl == self.types.result_decl(self.names) {
                        let variant = self.text(name.name);
                        if variant == "Ok" || variant == "Err" {
                            let written = format!("Result.{variant}");
                            return self.check_result_construct(
                                expr,
                                variant == "Ok",
                                args,
                                expected,
                                facts,
                                &written,
                            );
                        }
                    }
                    return self.check_variant_construct(expr, decl, name, args, expected, facts);
                }
            }
            return self.check_field_call(
                expr,
                callee,
                target,
                name,
                args,
                expected,
                facts,
                marked_effect,
            );
        }
        if let ast::ExprKind::Name(ident) = &callee.kind {
            match self.resolve_value(ident.name, Purpose::Read) {
                Resolved::Function(id) => {
                    return self.check_named_function_call(
                        expr,
                        callee,
                        id,
                        args,
                        expected,
                        facts,
                        marked_effect,
                    );
                }
                Resolved::Prelude(_) => {
                    let name = self.text(ident.name);
                    return self.check_prelude_call(expr, &name, args, expected, facts);
                }
                _ => {}
            }
        }
        self.check_plain_call(expr, callee, args, expected, facts, marked_effect)
    }

    /// A call of a prelude free function (sections 13.2, 13.4, 13.5, 14.2
    /// and 14.3). Every one of them is a roster entry with the same name, so
    /// the call lowers straight to a native call.
    ///
    /// Section 13.1's numeric rule decides the overload sets: "the `Int`
    /// signature is used when every argument is an `Int`, and otherwise the
    /// `Float` signature is used with the `Int` arguments converted". The
    /// arguments are therefore checked once with no expected type to see
    /// what they are, and then checked again against the signature that
    /// answer selects, which is what puts the `Int` to `Float` coercion in.
    #[allow(clippy::too_many_lines, reason = "one arm per prelude function")]
    fn check_prelude_call(
        &mut self,
        expr: &ast::Expr,
        name: &str,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        let int_ty = self.types.int();
        let float_ty = self.types.float();
        let string_ty = self.types.string();

        macro_rules! native {
            ($native:expr, $params:expr, $ret:expr) => {{
                let Some(position) = hollowscript_core::native::ROSTER
                    .iter()
                    .position(|n| n.name == $native)
                else {
                    return self.error_expr_at(expr.id);
                };
                let (checked_args, ty) = self
                    .check_call_args(&$params, $ret, 0, $native, args, expected, facts, expr.span);
                return Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::NativeCall {
                        native: u32::try_from(position).unwrap_or(0),
                        args: checked_args,
                    },
                };
            }};
        }

        match name {
            "abs" if self.every_argument_is_an_int(args, facts) => {
                native!("abs", [int_ty], Some(int_ty))
            }
            "abs" => native!("abs", [float_ty], Some(float_ty)),
            "sign" => native!("sign", [float_ty], Some(int_ty)),
            "min" | "max" if self.every_argument_is_an_int(args, facts) => {
                native!(name, [int_ty, int_ty], Some(int_ty))
            }
            "min" | "max" => native!(name, [float_ty, float_ty], Some(float_ty)),
            "clamp" if self.every_argument_is_an_int(args, facts) => {
                native!("clamp", [int_ty, int_ty, int_ty], Some(int_ty))
            }
            "clamp" => native!("clamp", [float_ty, float_ty, float_ty], Some(float_ty)),
            "round" | "floor" | "ceil" | "trunc" => native!(name, [float_ty], Some(int_ty)),
            "toFloat" => native!("toFloat", [int_ty], Some(float_ty)),
            "div" | "mod" | "remainder" | "ipow" => {
                native!(name, [int_ty, int_ty], Some(int_ty))
            }
            "sqrt" | "exp" | "ln" | "sin" | "cos" | "tan" => {
                native!(name, [float_ty], Some(float_ty))
            }
            "pow" | "atan2" => native!(name, [float_ty, float_ty], Some(float_ty)),
            "range" => {
                let list = self.types.list(int_ty);
                native!("range", [int_ty, int_ty], Some(list))
            }
            // Section 13.4: `Result<Int, String>` and `Result<Float, String>`.
            "parseInt" => {
                let names = &mut *self.names;
                let result = self.types.result(names, int_ty, string_ty);
                native!("parseInt", [string_ty], Some(result))
            }
            "parseFloat" => {
                let names = &mut *self.names;
                let result = self.types.result(names, float_ty, string_ty);
                native!("parseFloat", [string_ty], Some(result))
            }
            "fixed" => native!("fixed", [float_ty, int_ty], Some(string_ty)),
            // Section 8.2: `success(v)` is `Result.Ok(v)` and `failure(e)`
            // is `Result.Err(e)`. Neither is a native; the compiler builds
            // the enum value.
            "success" | "failure" => {
                self.check_result_construct(expr, name == "success", args, expected, facts, name)
            }
            "keys" | "has" => self.check_record_prelude_call(expr, name, args, facts),
            // `set` and `mapFrom` are generic in the element or entry type,
            // which the argument itself fixes, so the signature is built
            // from the argument rather than unified through a type variable.
            "set" | "mapFrom" => {
                self.check_collection_prelude_call(expr, name, args, expected, facts)
            }
            _ => {
                let text = name.to_owned();
                self.error_at(Code::HS0205, &[("name", &text)], expr.span);
                self.error_expr_at(expr.id)
            }
        }
    }

    /// `set(items)` (section 14.3) and `mapFrom(entries)` (section 14.2).
    /// Both are generic in a type the one argument fixes, so the argument is
    /// checked first, against the element or entry type an expected `Set` or
    /// `Map` implies when there is one, and the result type is read back off
    /// what the argument turned out to be.
    fn check_collection_prelude_call(
        &mut self,
        expr: &ast::Expr,
        name: &str,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        if args.len() != 1 {
            self.error_at(
                Code::HS0319,
                &[("expected", &1usize), ("found", &args.len())],
                expr.span,
            );
            return self.error_expr_at(expr.id);
        }
        let argument_expected = expected.and_then(|e| match (name, self.types.view(e)) {
            ("set", TypeView::Set(element)) => Some(self.types.list(element)),
            ("mapFrom", TypeView::Map(key, value)) => {
                let entry = self.map_entry_type(key, value);
                Some(self.types.list(entry))
            }
            _ => None,
        });
        let Some(argument) = args.first() else {
            return self.error_expr_at(expr.id);
        };
        // Section 2.10's own step 5: `set`/`mapFrom`'s type parameter is
        // bound only by this one argument, so an empty list literal with no
        // expected type to fall back to leaves it unbound, exactly the
        // "variable that is still unbound" step 5 reports as HS0308, not
        // the literal's own "cannot be inferred" HS0304 (`set([])`'s own
        // worked example).
        if argument_expected.is_none()
            && matches!(&argument.kind, ast::ExprKind::List(items) if items.is_empty())
        {
            let type_param = if name == "set" { "T" } else { "K" };
            self.error_at(
                Code::HS0308,
                &[("name", &type_param), ("function", &name)],
                expr.span,
            );
            return self.error_expr_at(expr.id);
        }
        let checked = self.check_expr(argument, argument_expected, facts);
        let ty = match (name, self.types.view(checked.ty)) {
            ("set", TypeView::List(element)) => self.types.set(element),
            ("mapFrom", TypeView::List(entry)) => match self.types.view(entry) {
                TypeView::Record(fields) if fields.len() == 2 => {
                    let key = fields.first().map(|&(_, t)| t);
                    let value = fields.get(1).map(|&(_, t)| t);
                    match (key, value) {
                        (Some(key), Some(value)) => self.types.map(key, value),
                        _ => self.types.error(),
                    }
                }
                _ => self.types.error(),
            },
            _ => self.types.error(),
        };
        if matches!(self.types.view(ty), TypeView::Error) {
            let found = self.type_text(checked.ty);
            let wanted = if name == "set" {
                "a list of a key type"
            } else {
                "a list of MapEntry"
            };
            self.error_at(
                Code::HS0301,
                &[("expected", &wanted), ("found", &found)],
                argument.span,
            );
        }
        let Some(position) = hollowscript_core::native::ROSTER
            .iter()
            .position(|n| n.name == name)
        else {
            return self.error_expr_at(expr.id);
        };
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::NativeCall {
                native: u32::try_from(position).unwrap_or(0),
                args: vec![checked],
            },
        }
    }

    /// `MapEntry<K, V>`, the prelude record type `{ key: K, value: V }`.
    fn map_entry_type(&mut self, key: TypeId, value: TypeId) -> TypeId {
        let key_name = self.names.intern("key");
        let value_name = self.names.intern("value");
        self.types.record(
            self.names,
            vec![
                Field {
                    name: key_name,
                    ty: key,
                },
                Field {
                    name: value_name,
                    ty: value,
                },
            ],
        )
    }

    /// A call of one of chapter 15's two prelude namespaces, `json.parse`
    /// and the rest. Each is a roster entry whose signature is fixed, so
    /// this is a table of them.
    #[allow(clippy::too_many_arguments, reason = "one call site")]
    fn check_data_namespace_call(
        &mut self,
        expr: &ast::Expr,
        namespace: &str,
        member: &str,
        member_span: Span,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        let int_ty = self.types.int();
        let string_ty = self.types.string();
        let json_decl = self.types.json_decl(self.names);
        let json = self.types.named(json_decl, Vec::new());
        let native = format!("{namespace}.{member}");

        macro_rules! native {
            ($params:expr, $ret:expr) => {{
                let Some(position) = hollowscript_core::native::ROSTER
                    .iter()
                    .position(|n| n.name == native)
                else {
                    return self.error_expr_at(expr.id);
                };
                let (checked_args, ty) = self
                    .check_call_args(&$params, $ret, 0, &native, args, expected, facts, expr.span);
                return Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::NativeCall {
                        native: u32::try_from(position).unwrap_or(0),
                        args: checked_args,
                    },
                };
            }};
        }

        match (namespace, member) {
            ("json", "parse") => {
                let error = self.types.json_error(self.names);
                let names = &mut *self.names;
                let result = self.types.result(names, json, error);
                native!([string_ty], Some(result))
            }
            ("json", "stringify") => native!([json], Some(string_ty)),
            ("json", "pretty") => native!([json, int_ty], Some(string_ty)),
            ("json", "field") => {
                let opt = self.types.optional(json);
                native!([json, string_ty], Some(opt))
            }
            ("json", "at") => {
                let opt = self.types.optional(json);
                native!([json, int_ty], Some(opt))
            }
            ("time", "parse" | "parseDate") => {
                let names = &mut *self.names;
                let result = self.types.result(names, int_ty, string_ty);
                native!([string_ty], Some(result))
            }
            ("time", "format" | "formatDate") => native!([int_ty], Some(string_ty)),
            ("time", "parts") => {
                let parts = self.types.time_parts(self.names);
                native!([int_ty], Some(parts))
            }
            ("time", "fromParts") => {
                let parts = self.types.time_parts(self.names);
                let names = &mut *self.names;
                let result = self.types.result(names, int_ty, string_ty);
                native!([parts], Some(result))
            }
            _ => {
                let namespace = namespace.to_owned();
                let member = member.to_owned();
                self.error_at(
                    Code::HS0320,
                    &[("type", &namespace), ("name", &member)],
                    member_span,
                );
                for a in args {
                    self.check_expr(a, None, facts);
                }
                self.error_expr_at(expr.id)
            }
        }
    }

    /// `keys(r)` and `has(r, name)` (section 13.5): the first argument is a
    /// record of any type, which no written type can stand for, so the
    /// signature is built from the argument itself.
    fn check_record_prelude_call(
        &mut self,
        expr: &ast::Expr,
        name: &str,
        args: &[ast::Expr],
        facts: &FactSet,
    ) -> Expr {
        let wanted = if name == "keys" { 1 } else { 2 };
        if args.len() != wanted {
            self.error_at(
                Code::HS0319,
                &[("expected", &wanted), ("found", &args.len())],
                expr.span,
            );
            return self.error_expr_at(expr.id);
        }
        let mut checked: Vec<Expr> = Vec::with_capacity(args.len());
        let record = match args.first() {
            Some(a) => self.check_expr(a, None, facts),
            None => return self.error_expr_at(expr.id),
        };
        if !matches!(self.types.view(record.ty), TypeView::Record(_)) {
            let found = self.type_text(record.ty);
            self.error_at(
                Code::HS0301,
                &[("expected", &"a record"), ("found", &found)],
                args.first().map_or(expr.span, |a| a.span),
            );
        }
        checked.push(record);
        if name == "has" {
            let string_ty = self.types.string();
            if let Some(a) = args.get(1) {
                let field = self.check_expr(a, Some(string_ty), facts);
                checked.push(field);
            }
        }
        let Some(position) = hollowscript_core::native::ROSTER
            .iter()
            .position(|n| n.name == name)
        else {
            return self.error_expr_at(expr.id);
        };
        let ty = if name == "keys" {
            let string_ty = self.types.string();
            self.types.list(string_ty)
        } else {
            self.types.bool()
        };
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::NativeCall {
                native: u32::try_from(position).unwrap_or(0),
                args: checked,
            },
        }
    }

    /// Whether every argument of an overloaded numeric prelude call is an
    /// `Int` (section 13.1). The arguments are checked with no expected type
    /// to find out, and thrown away: `check_call_args` checks them again
    /// against the signature this chooses.
    ///
    /// The answer is kept (see `int_argument_answers`), so that a call nested
    /// in another is not checked twice at every level of nesting.
    fn every_argument_is_an_int(&mut self, args: &[ast::Expr], facts: &FactSet) -> bool {
        let Some(first) = args.first() else {
            return true;
        };
        if let Some((known_facts, answer)) = self.int_argument_answers.get(&first.id) {
            if known_facts == facts {
                return *answer;
            }
        }
        self.diagnostics.begin_speculation();
        let all_int = args.iter().all(|a| {
            let checked = self.check_expr(a, None, facts);
            matches!(self.types.view(checked.ty), TypeView::Int)
        });
        self.diagnostics.discard_speculation();
        self.int_argument_answers
            .insert(first.id, (facts.clone(), all_int));
        all_int
    }

    #[allow(clippy::too_many_arguments, reason = "one call site")]
    fn check_named_function_call(
        &mut self,
        expr: &ast::Expr,
        callee: &ast::Expr,
        id: FunctionId,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
        marked_effect: bool,
    ) -> Expr {
        if marked_effect {
            self.error_at(Code::HS0402, &[("found", &"fn(...)")], expr.span);
        }
        let Some(sig) = self.function_sigs.get(&id.0).cloned() else {
            return self.error_expr_at(expr.id);
        };
        let function_name = self.text(sig.name);
        let (checked_args, result_ty) = self.check_call_args(
            &sig.params,
            sig.ret,
            sig.type_param_count,
            &function_name,
            args,
            expected,
            facts,
            expr.span,
        );
        let callee_ty = self.function_signature_type(id);
        let callee_expr = Expr {
            node: callee.id,
            ty: callee_ty,
            kind: ExprKind::FunctionValue(id),
        };
        let ty = if sig.ret.is_some() {
            result_ty
        } else {
            self.types.error()
        };
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::Call {
                callee: Box::new(callee_expr),
                args: checked_args,
            },
        }
    }

    fn check_plain_call(
        &mut self,
        expr: &ast::Expr,
        callee: &ast::Expr,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
        marked_effect: bool,
    ) -> Expr {
        let checked_callee = self.check_expr(callee, None, facts);
        if self.types.is_error(checked_callee.ty) {
            for a in args {
                self.check_expr(a, None, facts);
            }
            return self.error_expr_at(expr.id);
        }
        let TypeView::Function {
            effect,
            params,
            ret,
        } = self.types.view(checked_callee.ty)
        else {
            let found = self.type_text(checked_callee.ty);
            self.error_at(Code::HS0318, &[("found", &found)], callee.span);
            for a in args {
                self.check_expr(a, None, facts);
            }
            return self.error_expr_at(expr.id);
        };
        let params = params.to_vec();
        if let Some(err) = effects::check_effect_marker(effect, marked_effect) {
            self.report_effect_marker_error(err, expr.span, checked_callee.ty);
        }
        let (checked_args, result_ty) =
            self.check_call_args(&params, ret, 0, "", args, expected, facts, expr.span);
        let ty = if ret.is_some() {
            result_ty
        } else {
            self.types.error()
        };
        let kind = if marked_effect {
            ExprKind::EffectCall {
                callee: Box::new(checked_callee),
                args: checked_args,
            }
        } else {
            ExprKind::Call {
                callee: Box::new(checked_callee),
                args: checked_args,
            }
        };
        Expr {
            node: expr.id,
            ty,
            kind,
        }
    }

    #[allow(clippy::too_many_arguments, reason = "one call site")]
    fn check_field_call(
        &mut self,
        expr: &ast::Expr,
        callee: &ast::Expr,
        target: &ast::Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
        marked_effect: bool,
    ) -> Expr {
        let checked_target = self.check_expr(target, None, facts);
        if self.types.is_error(checked_target.ty) {
            for a in args {
                self.check_expr(a, None, facts);
            }
            return self.error_expr_at(expr.id);
        }
        if let TypeView::Record(fields) = self.types.view(checked_target.ty) {
            let field = fields.iter().find(|(n, _)| *n == name.name).copied();
            if let Some((_, field_ty)) = field {
                if let TypeView::Function {
                    effect,
                    params,
                    ret,
                } = self.types.view(field_ty)
                {
                    let params = params.to_vec();
                    if let Some(err) = effects::check_effect_marker(effect, marked_effect) {
                        self.report_effect_marker_error(err, expr.span, field_ty);
                    }
                    let index = self.field_index(checked_target.ty, name.name);
                    // A member of a namespace capability is not an ordinary
                    // field read: the compiler has to name it in the
                    // module's capability table, and a field access carries
                    // only a position.
                    let field_expr = match checked_target.kind {
                        ExprKind::Capability(capability) => Expr {
                            node: callee.id,
                            ty: field_ty,
                            kind: ExprKind::CapabilityMember {
                                capability,
                                name: name.name,
                                index,
                            },
                        },
                        _ => Expr {
                            node: callee.id,
                            ty: field_ty,
                            kind: ExprKind::Field {
                                target: Box::new(checked_target),
                                index,
                            },
                        },
                    };
                    let (checked_args, result_ty) =
                        self.check_call_args(&params, ret, 0, "", args, expected, facts, expr.span);
                    let ty = if ret.is_some() {
                        result_ty
                    } else {
                        self.types.error()
                    };
                    let kind = if marked_effect {
                        ExprKind::EffectCall {
                            callee: Box::new(field_expr),
                            args: checked_args,
                        }
                    } else {
                        ExprKind::Call {
                            callee: Box::new(field_expr),
                            args: checked_args,
                        }
                    };
                    return Expr {
                        node: expr.id,
                        ty,
                        kind,
                    };
                }
                let found = self.type_text(field_ty);
                self.error_at(Code::HS0318, &[("found", &found)], callee.span);
                for a in args {
                    self.check_expr(a, None, facts);
                }
                return self.error_expr_at(expr.id);
            }
        }
        self.check_method_call(
            expr,
            checked_target,
            name,
            args,
            expected,
            facts,
            marked_effect,
        )
    }

    fn field_index(&mut self, record_ty: TypeId, name: Symbol) -> u32 {
        if let TypeView::Record(fields) = self.types.view(record_ty) {
            if let Some(pos) = fields.iter().position(|(n, _)| *n == name) {
                return u32::try_from(pos).unwrap_or(0);
            }
        }
        0
    }

    fn check_variant_construct(
        &mut self,
        expr: &ast::Expr,
        decl: DeclId,
        name: &ast::Ident,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        let variant_name_text = self.text(name.name);
        let Some(DeclShape::Enum { params, variants }) = self.types.decl_shape(decl).cloned()
        else {
            self.error_at(
                Code::HS0505,
                &[("enum", &""), ("name", &variant_name_text)],
                name.span,
            );
            return self.error_expr_at(expr.id);
        };
        let Some((index, variant)) = variants
            .iter()
            .enumerate()
            .find(|(_, v)| v.name == name.name)
        else {
            let enum_name = self
                .types
                .decl_name(decl)
                .map(|s| self.text(s))
                .unwrap_or_default();
            self.error_at(
                Code::HS0505,
                &[("enum", &enum_name), ("name", &variant_name_text)],
                name.span,
            );
            for a in args {
                self.check_expr(a, None, facts);
            }
            return self.error_expr_at(expr.id);
        };
        if variant.payload.len() != args.len() {
            self.error_at(
                Code::HS0506,
                &[
                    ("name", &variant_name_text),
                    ("expected", &variant.payload.len()),
                    ("found", &args.len()),
                ],
                expr.span,
            );
            for a in args {
                self.check_expr(a, None, facts);
            }
            return self.error_expr_at(expr.id);
        }
        let payload = variant.payload.clone();
        // The enum's own type parameters, in terms of the same `TypeVarId`
        // indices its payload types already use (section 2.10, the same
        // instantiation-free scheme `resolve_function_signatures` gives a
        // generic `fn`): a generic return type `check_call_args` can
        // unify and substitute exactly as it does a function's.
        let type_param_count = u32::try_from(params.len()).unwrap_or(0);
        let generic_args: Vec<TypeId> = (0..type_param_count)
            .map(|i| self.types.param(decl, i))
            .collect();
        let ret_ty = self.types.named(decl, generic_args);
        let (checked_args, ty) = self.check_call_args(
            &payload,
            Some(ret_ty),
            type_param_count,
            &variant_name_text,
            args,
            expected,
            facts,
            expr.span,
        );
        let index = u32::try_from(index).unwrap_or(0);
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::VariantConstruct {
                decl,
                index,
                args: checked_args,
            },
        }
    }

    /// `success(v)`, `failure(e)`, `Result.Ok(v)` and `Result.Err(e)`
    /// (section 8.2). The argument determines one type argument; the other
    /// is taken from the expected type, and is HS0308 when there is none,
    /// because a `Result` with an unknown half is not a type.
    fn check_result_construct(
        &mut self,
        expr: &ast::Expr,
        is_ok: bool,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
        written_as: &str,
    ) -> Expr {
        if args.len() != 1 {
            self.error_at(
                Code::HS0319,
                &[("expected", &1usize), ("found", &args.len())],
                expr.span,
            );
            for a in args {
                self.check_expr(a, None, facts);
            }
            return self.error_expr_at(expr.id);
        }
        let from_expected = expected.and_then(|e| self.types.as_result(e));
        let payload_expected = from_expected.map(|(ok, err)| if is_ok { ok } else { err });
        let Some(argument) = args.first() else {
            return self.error_expr_at(expr.id);
        };
        let checked = self.check_expr(argument, payload_expected, facts);
        let checked = apply_coercion(checked, payload_expected, self.types);
        let (ok_ty, err_ty) = if let Some((ok, err)) = from_expected {
            (ok, err)
        } else {
            // Only the half the argument gives is known.
            let name = if is_ok { "E" } else { "T" };
            self.error_at(
                Code::HS0308,
                &[("name", &name), ("function", &written_as)],
                expr.span,
            );
            let error = self.types.error();
            if is_ok {
                (checked.ty, error)
            } else {
                (error, checked.ty)
            }
        };
        let decl = self.types.result_decl(self.names);
        let ty = self.types.result(self.names, ok_ty, err_ty);
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::VariantConstruct {
                decl,
                index: u32::from(!is_ok),
                args: vec![checked],
            },
        }
    }

    /// A bare `Enum.Variant`, not called (section 3.5): a zero-arity variant
    /// is the enum value itself, and a variant that takes a payload is a
    /// pure reference to its constructor.
    fn check_variant_reference(
        &mut self,
        expr: &ast::Expr,
        decl: DeclId,
        name: &ast::Ident,
        expected: Expected,
    ) -> Expr {
        let variant_name_text = self.text(name.name);
        let Some(DeclShape::Enum { params, variants }) = self.types.decl_shape(decl).cloned()
        else {
            self.error_at(
                Code::HS0505,
                &[("enum", &""), ("name", &variant_name_text)],
                name.span,
            );
            return self.error_expr_at(expr.id);
        };
        let Some((index, variant)) = variants
            .iter()
            .enumerate()
            .find(|(_, v)| v.name == name.name)
        else {
            let enum_name = self
                .types
                .decl_name(decl)
                .map(|s| self.text(s))
                .unwrap_or_default();
            self.error_at(
                Code::HS0505,
                &[("enum", &enum_name), ("name", &variant_name_text)],
                name.span,
            );
            return self.error_expr_at(expr.id);
        };
        let index = u32::try_from(index).unwrap_or(0);
        let type_param_count = u32::try_from(params.len()).unwrap_or(0);
        if variant.payload.is_empty() {
            // Section 2.10: a payload-free variant of a generic enum binds
            // its type arguments from the expected type alone, the same as
            // a generic function name used as a value; HS0308 for any that
            // are still unbound once that has been tried. Reuses
            // `check_call_args`' own generic machinery with no arguments,
            // rather than duplicating step 3 and step 5 of section 2.10
            // here.
            let generic_args: Vec<TypeId> = (0..type_param_count)
                .map(|i| self.types.param(decl, i))
                .collect();
            let ret_ty = self.types.named(decl, generic_args);
            let (_, ty) = self.check_call_args(
                &[],
                Some(ret_ty),
                type_param_count,
                &variant_name_text,
                &[],
                expected,
                &FactSet::empty(),
                expr.span,
            );
            return Expr {
                node: expr.id,
                ty,
                kind: ExprKind::VariantConstruct {
                    decl,
                    index,
                    args: Vec::new(),
                },
            };
        }
        // A variant with a payload used without a call (section 6.3) is a
        // pure reference to its constructor. `Result.Ok` and `Result.Err`
        // never reach here: `check_call` only takes the bare-reference path
        // for a variant that is not immediately called, and section 3.5's
        // own function-value rule leaves a generic one's type parameters
        // unbound unless an expected function type binds them, which this
        // build does not yet infer for an enum's own parameters here (no
        // corpus case exercises it: every generic enum variant used as a
        // bare value in the corpus and the specification is non-generic).
        let payload = variant.payload.clone();
        let ret = self.types.named(decl, Vec::new());
        let ty = self.types.function(false, payload, Some(ret));
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::VariantValue { decl, index },
        }
    }

    fn check_index(
        &mut self,
        expr: &ast::Expr,
        target: &ast::Expr,
        index: &ast::Expr,
        facts: &FactSet,
    ) -> Expr {
        let checked_target = self.check_expr(target, None, facts);
        if self.types.is_error(checked_target.ty) {
            self.check_expr(index, None, facts);
            return self.error_expr_at(expr.id);
        }
        match self.types.view(checked_target.ty) {
            TypeView::List(elem) => {
                let int_ty = self.types.int();
                let checked_index = self.check_against(index, int_ty, facts);
                let ty = self.types.optional(elem);
                Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::Index {
                        target: Box::new(checked_target),
                        index: Box::new(checked_index),
                    },
                }
            }
            TypeView::Map(key, value) => {
                let checked_index = self.check_against(index, key, facts);
                let ty = self.types.optional(value);
                Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::Index {
                        target: Box::new(checked_target),
                        index: Box::new(checked_index),
                    },
                }
            }
            _ => {
                let found = self.type_text(checked_target.ty);
                self.error_at(Code::HS0323, &[("found", &found)], target.span);
                self.check_expr(index, None, facts);
                self.error_expr_at(expr.id)
            }
        }
    }

    fn check_field(
        &mut self,
        expr: &ast::Expr,
        target: &ast::Expr,
        name: &ast::Ident,
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        if let ast::ExprKind::Name(ident) = &target.kind {
            if let Resolved::EnumNamespace(decl) = self.resolve_value(ident.name, Purpose::Read) {
                return self.check_variant_reference(expr, decl, name, expected);
            }
        }
        let checked_target = self.check_expr(target, None, facts);
        if self.types.is_error(checked_target.ty) {
            return self.error_expr_at(expr.id);
        }
        let TypeView::Record(fields) = self.types.view(checked_target.ty) else {
            let found = self.type_text(checked_target.ty);
            self.error_at(
                Code::HS0322,
                &[("name", &self.text(name.name)), ("found", &found)],
                name.span,
            );
            return self.error_expr_at(expr.id);
        };
        let Some((_, field_ty)) = fields.iter().find(|(n, _)| *n == name.name).copied() else {
            let text = self.text(name.name);
            self.error_at(Code::HS0321, &[("name", &text)], name.span);
            return self.error_expr_at(expr.id);
        };
        let index = self.field_index(checked_target.ty, name.name);
        let field_expr = Expr {
            node: expr.id,
            ty: field_ty,
            kind: ExprKind::Field {
                target: Box::new(checked_target),
                index,
            },
        };
        // A field read narrows the same way a local does (section 4.8): a
        // path like `task.owner` can appear in `facts` just as a bare local
        // can.
        if let Some(path) = narrow::path_of(&field_expr, self.types, self.module) {
            if self.fact_holds(facts, &path) {
                if let Some(inner) = self.types.as_optional(field_ty) {
                    return Expr {
                        node: field_expr.node,
                        ty: inner,
                        kind: ExprKind::NarrowedPath(path),
                    };
                }
            }
        }
        field_expr
    }

    fn check_try(&mut self, expr: &ast::Expr, inner: &ast::Expr, facts: &FactSet) -> Expr {
        let checked = self.check_expr(inner, None, facts);
        self.apply_try(checked, expr.span)
    }

    fn apply_try(&mut self, operand: Expr, span: Span) -> Expr {
        if self.types.is_error(operand.ty) {
            return operand;
        }
        let Some((ok_ty, err_ty)) = self.types.as_result(operand.ty) else {
            let found = self.type_text(operand.ty);
            self.error_at(Code::HS0331, &[("found", &found)], span);
            return error_expr(operand.node, self.types);
        };
        // A top-level `?` always ends the module's initialisation with
        // `Result.Err(x)` (section 8.3): unlike inside a function, there is
        // no enclosing return type to agree with.
        if !self.frames.last().is_some_and(|f| f.is_top_level) {
            let frame_err_ty = self.frames.last().and_then(|f| f.result_err_ty);
            match frame_err_ty {
                Some(fe) if fe == err_ty => {}
                Some(fe) => {
                    let expected = self.type_text(fe);
                    self.error_at(Code::HS0332, &[("expected", &expected)], span);
                }
                None => {
                    self.error_at(Code::HS0332, &[("expected", &"Result")], span);
                }
            }
        }
        let node = operand.node;
        Expr {
            node,
            ty: ok_ty,
            kind: ExprKind::Try {
                operand: Box::new(operand),
                err_ty,
            },
        }
    }

    /// A curated subset of chapters 13 to 15's native methods: enough for a
    /// program that uses lists, maps, sets, strings and `Result` in ordinary
    /// ways to check, without this build claiming full fidelity to every one
    /// of the roster's 118 entries. A name outside this subset is HS0320,
    /// the same diagnostic a genuinely undefined method gets, since both are
    /// "this build does not accept that call" as far as a program is concerned.
    #[allow(clippy::too_many_lines, reason = "one match arm per native method")]
    #[allow(clippy::too_many_arguments, reason = "one call site")]
    fn check_method_call(
        &mut self,
        expr: &ast::Expr,
        receiver: Expr,
        name: &ast::Ident,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
        marked_effect: bool,
    ) -> Expr {
        if marked_effect {
            self.error_at(Code::HS0402, &[("found", &"fn(...)")], expr.span);
        }
        let method = self.text(name.name);
        let receiver_view = self.types.view(receiver.ty);
        let bool_ty = self.types.bool();
        let int_ty = self.types.int();
        let string_ty = self.types.string();

        macro_rules! plain {
            ($native:expr, $params:expr, $ret:expr) => {{
                let Some(native_position) = hollowscript_core::native::ROSTER
                    .iter()
                    .position(|n| n.name == $native)
                else {
                    return self.method_not_found(expr, &receiver, name);
                };
                let native_index = u32::try_from(native_position).unwrap_or(0);
                let (mut checked_args, ret_ty) = self
                    .check_call_args(&$params, $ret, 0, $native, args, expected, facts, expr.span);
                checked_args.insert(0, receiver);
                let ty = ret_ty;
                return Expr {
                    node: expr.id,
                    ty,
                    kind: ExprKind::NativeCall {
                        native: native_index,
                        args: checked_args,
                    },
                };
            }};
        }

        match (receiver_view, method.as_str()) {
            (TypeView::List(_), "length") => plain!("list.length", [], Some(int_ty)),
            (TypeView::List(_), "isEmpty") => plain!("list.isEmpty", [], Some(bool_ty)),
            (TypeView::List(elem), "first" | "last") => {
                let opt = self.types.optional(elem);
                let native = if method == "first" {
                    "list.first"
                } else {
                    "list.last"
                };
                plain!(native, [], Some(opt));
            }
            (TypeView::List(elem), "contains") => plain!("list.contains", [elem], Some(bool_ty)),
            (TypeView::List(elem), "indexOf") => {
                let opt = self.types.optional(int_ty);
                plain!("list.indexOf", [elem], Some(opt));
            }
            (TypeView::List(elem), "reverse") => {
                let list = self.types.list(elem);
                plain!("list.reverse", [], Some(list));
            }
            // Section 14.1's first `sort`: "ascending natural order; `T`
            // must be `Int`, `Float` or `String` (HS0330)", which is
            // section 2.8's orderable types. Any other element type needs
            // the comparator form below.
            (TypeView::List(elem), "sort") if args.is_empty() => {
                if !matches!(
                    self.types.view(elem),
                    TypeView::Int | TypeView::Float | TypeView::String | TypeView::Error
                ) {
                    let found = self.type_text(elem);
                    self.error_at(
                        Code::HS0330,
                        &[
                            ("operation", &"sort"),
                            ("requirement", &"a list of Int, Float or String"),
                            ("found", &found),
                        ],
                        name.span,
                    );
                }
                let list = self.types.list(elem);
                plain!("list.sort", [], Some(list));
            }
            // Section 14.1's second `sort`: the comparator form, which
            // works for any element type, not only an orderable one.
            (TypeView::List(elem), "sort") => {
                let list = self.types.list(elem);
                let cb = self.types.function(false, vec![elem, elem], Some(int_ty));
                plain!("list.sortBy", [cb], Some(list));
            }
            (TypeView::List(elem), "append") => {
                let list = self.types.list(elem);
                plain!("list.append", [elem], Some(list));
            }
            (TypeView::List(elem), "concat") => {
                let list = self.types.list(elem);
                plain!("list.concat", [list], Some(list));
            }
            (TypeView::List(elem), "take" | "drop") => {
                let list = self.types.list(elem);
                let native = if method == "take" {
                    "list.take"
                } else {
                    "list.drop"
                };
                plain!(native, [int_ty], Some(list));
            }
            (TypeView::List(elem), "slice") => {
                let list = self.types.list(elem);
                plain!("list.slice", [int_ty, int_ty], Some(list));
            }
            // Section 14.1: `join` is defined for a list of `String` alone,
            // and is HS0330 for any other element type.
            (TypeView::List(elem), "join") => {
                if !matches!(self.types.view(elem), TypeView::String | TypeView::Error) {
                    let found = self.type_text(elem);
                    self.error_at(
                        Code::HS0330,
                        &[
                            ("operation", &"join"),
                            ("requirement", &"a list of String"),
                            ("found", &found),
                        ],
                        name.span,
                    );
                }
                plain!("list.join", [string_ty], Some(string_ty));
            }
            (TypeView::List(elem), "each") => {
                let cb = self.types.function(false, vec![elem], None);
                plain!("list.each", [cb], None);
            }
            (TypeView::List(elem), "all" | "any") => {
                let cb = self.types.function(false, vec![elem], Some(bool_ty));
                let native = if method == "all" {
                    "list.all"
                } else {
                    "list.any"
                };
                plain!(native, [cb], Some(bool_ty));
            }
            (TypeView::List(elem), "filter") => {
                let cb = self.types.function(false, vec![elem], Some(bool_ty));
                let list = self.types.list(elem);
                plain!("list.filter", [cb], Some(list));
            }
            (TypeView::List(elem), "find") => {
                let cb = self.types.function(false, vec![elem], Some(bool_ty));
                let opt = self.types.optional(elem);
                plain!("list.find", [cb], Some(opt));
            }
            (TypeView::List(elem), "map") => {
                self.check_map_like_callback(expr, receiver, elem, args, facts, "list.map", false)
            }
            _ if matches!(receiver_view, TypeView::List(_)) && method == "flatMap" => {
                let TypeView::List(elem) = receiver_view else {
                    unreachable!()
                };
                self.check_map_like_callback(
                    expr,
                    receiver,
                    elem,
                    args,
                    facts,
                    "list.flatMap",
                    true,
                )
            }
            (TypeView::String, "length" | "byteLength") => {
                let native = if method == "length" {
                    "string.length"
                } else {
                    "string.byteLength"
                };
                plain!(native, [], Some(int_ty));
            }
            (TypeView::String, "isEmpty") => plain!("string.isEmpty", [], Some(bool_ty)),
            (TypeView::String, "toUpper" | "toLower" | "trim" | "trimStart" | "trimEnd") => {
                let native = match method.as_str() {
                    "toUpper" => "string.toUpper",
                    "toLower" => "string.toLower",
                    "trim" => "string.trim",
                    "trimStart" => "string.trimStart",
                    _ => "string.trimEnd",
                };
                plain!(native, [], Some(string_ty));
            }
            (TypeView::String, "contains" | "startsWith" | "endsWith") => {
                let native = match method.as_str() {
                    "contains" => "string.contains",
                    "startsWith" => "string.startsWith",
                    _ => "string.endsWith",
                };
                plain!(native, [string_ty], Some(bool_ty));
            }
            (TypeView::String, "indexOf") => {
                let opt = self.types.optional(int_ty);
                plain!("string.indexOf", [string_ty], Some(opt));
            }
            (TypeView::String, "split") => {
                let list = self.types.list(string_ty);
                plain!("string.split", [string_ty], Some(list));
            }
            (TypeView::String, "lines") => {
                let list = self.types.list(string_ty);
                plain!("string.lines", [], Some(list));
            }
            (TypeView::String, "substring") => {
                plain!("string.substring", [int_ty, int_ty], Some(string_ty))
            }
            (TypeView::String, "at") => {
                let opt = self.types.optional(string_ty);
                plain!("string.at", [int_ty], Some(opt));
            }
            (TypeView::String, "repeat") => plain!("string.repeat", [int_ty], Some(string_ty)),
            (TypeView::String, "padStart" | "padEnd") => {
                let native = if method == "padStart" {
                    "string.padStart"
                } else {
                    "string.padEnd"
                };
                plain!(native, [int_ty, string_ty], Some(string_ty));
            }
            (TypeView::String, "replace") => {
                plain!("string.replace", [string_ty, string_ty], Some(string_ty));
            }
            (TypeView::String, "chars") => {
                let list = self.types.list(string_ty);
                plain!("string.chars", [], Some(list));
            }
            (TypeView::Map(key, value), "get") => {
                let opt = self.types.optional(value);
                plain!("map.get", [key], Some(opt));
            }
            (TypeView::Map(key, _), "has") => plain!("map.has", [key], Some(bool_ty)),
            (TypeView::Map(key, _), "keys") => {
                let list = self.types.list(key);
                plain!("map.keys", [], Some(list));
            }
            (TypeView::Map(_, value), "values") => {
                let list = self.types.list(value);
                plain!("map.values", [], Some(list));
            }
            (TypeView::Map(key, value), "entries") => {
                let entry = self.map_entry_type(key, value);
                let list = self.types.list(entry);
                plain!("map.entries", [], Some(list));
            }
            (TypeView::Map(_, _), "length") => plain!("map.length", [], Some(int_ty)),
            (TypeView::Map(_, _), "isEmpty") => plain!("map.isEmpty", [], Some(bool_ty)),
            (TypeView::Map(key, value), "with") => {
                let map = self.types.map(key, value);
                plain!("map.with", [key, value], Some(map));
            }
            (TypeView::Map(key, value), "without") => {
                let map = self.types.map(key, value);
                plain!("map.without", [key], Some(map));
            }
            (TypeView::Map(key, value), "merge") => {
                let map = self.types.map(key, value);
                plain!("map.merge", [map], Some(map));
            }
            (TypeView::Set(elem), "has") => plain!("set.has", [elem], Some(bool_ty)),
            (TypeView::Set(_), "length") => plain!("set.length", [], Some(int_ty)),
            (TypeView::Set(_), "isEmpty") => plain!("set.isEmpty", [], Some(bool_ty)),
            (TypeView::Set(elem), "toList") => {
                let list = self.types.list(elem);
                plain!("set.toList", [], Some(list));
            }
            (TypeView::Set(elem), "with" | "without") => {
                let set = self.types.set(elem);
                let native = if method == "with" {
                    "set.with"
                } else {
                    "set.without"
                };
                plain!(native, [elem], Some(set));
            }
            (TypeView::Set(elem), "union" | "intersect" | "difference") => {
                let set = self.types.set(elem);
                let native = match method.as_str() {
                    "union" => "set.union",
                    "intersect" => "set.intersect",
                    _ => "set.difference",
                };
                plain!(native, [set], Some(set));
            }
            (TypeView::Set(elem), "isSubsetOf") => {
                let set = self.types.set(elem);
                plain!("set.isSubsetOf", [set], Some(bool_ty));
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "isOk" => {
                plain!("result.isOk", [], Some(bool_ty));
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "isErr" => {
                plain!("result.isErr", [], Some(bool_ty));
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "ok" => {
                let (ok_ty, _) = self
                    .types
                    .as_result(receiver.ty)
                    .unwrap_or((int_ty, int_ty));
                let opt = self.types.optional(ok_ty);
                plain!("result.ok", [], Some(opt));
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "err" => {
                let (_, err_ty) = self
                    .types
                    .as_result(receiver.ty)
                    .unwrap_or((int_ty, int_ty));
                let opt = self.types.optional(err_ty);
                plain!("result.err", [], Some(opt));
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "unwrapOr" => {
                let (ok_ty, _) = self
                    .types
                    .as_result(receiver.ty)
                    .unwrap_or((int_ty, int_ty));
                plain!("result.unwrapOr", [ok_ty], Some(ok_ty));
            }
            (TypeView::Map(key, value), "each") => {
                let cb = self.types.function(false, vec![key, value], None);
                plain!("map.each", [cb], None);
            }
            (TypeView::Map(key, value), "filter") => {
                let cb = self.types.function(false, vec![key, value], Some(bool_ty));
                let map = self.types.map(key, value);
                plain!("map.filter", [cb], Some(map));
            }
            (TypeView::Set(elem), "each") => {
                let cb = self.types.function(false, vec![elem], None);
                plain!("set.each", [cb], None);
            }
            (TypeView::Set(elem), "filter") => {
                let cb = self.types.function(false, vec![elem], Some(bool_ty));
                let set = self.types.set(elem);
                plain!("set.filter", [cb], Some(set));
            }
            // These four have a result type that only the callback's own
            // return type fixes, so each checks the closure first and reads
            // the answer back, as `list.map` does.
            (TypeView::Map(key, value), "mapValues") => self.check_callback_result(
                expr,
                receiver,
                &[value],
                args,
                facts,
                "map.mapValues",
                &|me, ret| me.types.map(key, ret),
            ),
            (TypeView::List(elem), "reduce") => {
                self.check_reduce(expr, receiver, elem, args, expected, facts)
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "map" => {
                let (ok_ty, err_ty) = self
                    .types
                    .as_result(receiver.ty)
                    .unwrap_or((int_ty, int_ty));
                self.check_callback_result(
                    expr,
                    receiver,
                    &[ok_ty],
                    args,
                    facts,
                    "result.map",
                    &|me, ret| {
                        let names = &mut *me.names;
                        me.types.result(names, ret, err_ty)
                    },
                )
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "mapError" => {
                let (ok_ty, err_ty) = self
                    .types
                    .as_result(receiver.ty)
                    .unwrap_or((int_ty, int_ty));
                self.check_callback_result(
                    expr,
                    receiver,
                    &[err_ty],
                    args,
                    facts,
                    "result.mapError",
                    &|me, ret| {
                        let names = &mut *me.names;
                        me.types.result(names, ok_ty, ret)
                    },
                )
            }
            _ if self.types.as_result(receiver.ty).is_some() && method == "andThen" => {
                let (ok_ty, _) = self
                    .types
                    .as_result(receiver.ty)
                    .unwrap_or((int_ty, int_ty));
                // `andThen`'s callback returns the whole `Result`, so the
                // result type is that return type unchanged.
                self.check_callback_result(
                    expr,
                    receiver,
                    &[ok_ty],
                    args,
                    facts,
                    "result.andThen",
                    &|_, ret| ret,
                )
            }
            _ => self.method_not_found(expr, &receiver, name),
        }
    }

    fn method_not_found(&mut self, expr: &ast::Expr, receiver: &Expr, name: &ast::Ident) -> Expr {
        let found = self.type_text(receiver.ty);
        let text = self.text(name.name);
        self.error_at(
            Code::HS0320,
            &[("type", &found), ("name", &text)],
            name.span,
        );
        self.error_expr_at(expr.id)
    }

    /// A method whose result type is decided by what its one callback
    /// returns: the closure is checked with its parameters fixed and no
    /// expected return type, and `build` turns the return type it turned out
    /// to have into the method's own result type.
    #[allow(clippy::too_many_arguments, reason = "one call site per method")]
    fn check_callback_result(
        &mut self,
        expr: &ast::Expr,
        receiver: Expr,
        params: &[TypeId],
        args: &[ast::Expr],
        facts: &FactSet,
        native_name: &str,
        build: &dyn Fn(&mut Self, TypeId) -> TypeId,
    ) -> Expr {
        let Some(argument) = args.first() else {
            self.error_at(Code::HS0319, &[("expected", &1), ("found", &0)], expr.span);
            return self.error_expr_at(expr.id);
        };
        if args.len() > 1 {
            self.error_at(
                Code::HS0319,
                &[("expected", &1), ("found", &args.len())],
                expr.span,
            );
        }
        let param_ty = self.types.function(false, params.to_vec(), None);
        let checked_closure = self.check_expr(argument, Some(param_ty), facts);
        let ret = match self.types.view(checked_closure.ty) {
            TypeView::Function { ret: Some(r), .. } => r,
            _ => self.types.error(),
        };
        let ty = build(self, ret);
        let Some(native) = hollowscript_core::native::ROSTER
            .iter()
            .position(|n| n.name == native_name)
        else {
            return self.error_expr_at(expr.id);
        };
        Expr {
            node: expr.id,
            ty,
            kind: ExprKind::NativeCall {
                native: u32::try_from(native).unwrap_or(0),
                args: vec![receiver, checked_closure],
            },
        }
    }

    /// `list.reduce` (section 14.1). "The type of `reduce` is fixed by its
    /// first argument, and a callback that returns a wider type does not
    /// change it", so the accumulator is checked first and its type then
    /// fixes both the callback's first parameter and its return type.
    fn check_reduce(
        &mut self,
        expr: &ast::Expr,
        receiver: Expr,
        elem: TypeId,
        args: &[ast::Expr],
        expected: Expected,
        facts: &FactSet,
    ) -> Expr {
        let (Some(initial), Some(combine)) = (args.first(), args.get(1)) else {
            self.error_at(
                Code::HS0319,
                &[("expected", &2), ("found", &args.len())],
                expr.span,
            );
            return self.error_expr_at(expr.id);
        };
        if args.len() > 2 {
            self.error_at(
                Code::HS0319,
                &[("expected", &2), ("found", &args.len())],
                expr.span,
            );
        }
        let checked_initial = self.check_expr(initial, expected, facts);
        let accumulator = checked_initial.ty;
        let param_ty = self
            .types
            .function(false, vec![accumulator, elem], Some(accumulator));
        let checked_closure = self.check_expr(combine, Some(param_ty), facts);
        if let TypeView::Function { ret: Some(ret), .. } = self.types.view(checked_closure.ty) {
            if ret != accumulator && !self.types.is_error(ret) {
                let found = self.type_text(ret);
                let wanted = self.type_text(accumulator);
                self.error_at(
                    Code::HS0301,
                    &[("expected", &wanted), ("found", &found)],
                    combine.span,
                );
            }
        }
        let Some(native) = hollowscript_core::native::ROSTER
            .iter()
            .position(|n| n.name == "list.reduce")
        else {
            return self.error_expr_at(expr.id);
        };
        Expr {
            node: expr.id,
            ty: accumulator,
            kind: ExprKind::NativeCall {
                native: u32::try_from(native).unwrap_or(0),
                args: vec![receiver, checked_initial, checked_closure],
            },
        }
    }

    /// `list.map`/`list.flatMap`: the one shape among the curated methods
    /// whose result type depends on what the callback itself returns, so it
    /// cannot be expressed as a fixed native signature the way the others
    /// are; the callback is checked with its parameter fixed to the element
    /// type and no expected return, and the checked closure's own return
    /// type is read back to build the result (`[U]` for `map`, `U` folded
    /// flat one level for `flatMap`, where `U` must itself be a list).
    #[allow(clippy::too_many_arguments, reason = "one call site")]
    fn check_map_like_callback(
        &mut self,
        expr: &ast::Expr,
        receiver: Expr,
        elem: TypeId,
        args: &[ast::Expr],
        facts: &FactSet,
        native_name: &str,
        flatten: bool,
    ) -> Expr {
        let Some(arg) = args.first() else {
            self.error_at(Code::HS0319, &[("expected", &1), ("found", &0)], expr.span);
            return self.error_expr_at(expr.id);
        };
        if args.len() > 1 {
            self.error_at(
                Code::HS0319,
                &[("expected", &1), ("found", &args.len())],
                expr.span,
            );
        }
        let param_ty = self.types.function(false, vec![elem], None);
        let checked_closure = self.check_expr(arg, Some(param_ty), facts);
        let ret = match self.types.view(checked_closure.ty) {
            TypeView::Function { ret: Some(r), .. } => r,
            _ => self.types.error(),
        };
        let result_elem = if flatten {
            match self.types.view(ret) {
                TypeView::List(inner) => inner,
                _ if self.types.is_error(ret) => ret,
                _ => {
                    let found = self.type_text(ret);
                    self.error_at(
                        Code::HS0301,
                        &[("expected", &"[T]"), ("found", &found)],
                        arg.span,
                    );
                    self.types.error()
                }
            }
        } else {
            ret
        };
        let Some(native) = hollowscript_core::native::ROSTER
            .iter()
            .position(|n| n.name == native_name)
        else {
            return self.error_expr_at(expr.id);
        };
        let result_list = self.types.list(result_elem);
        Expr {
            node: expr.id,
            ty: result_list,
            kind: ExprKind::NativeCall {
                native: u32::try_from(native).unwrap_or(0),
                args: vec![receiver, checked_closure],
            },
        }
    }
}

/// The result of checking one module (used by [`crate::check`] to assemble
/// the whole program).
#[derive(Debug)]
pub struct CheckedModule {
    /// The checked module.
    pub module: hir::Module,
    /// Diagnostics found while checking it.
    pub diagnostics: Vec<Diagnostic>,
    /// The filter literals in it, in source order, each once.
    pub filter_literals: Vec<crate::FilterLiteral>,
}

/// Checks one already-parsed file as one module of the program (chapters 2,
/// 4, 7, 9 and 10), in the phases section 4.10 requires: every top-level
/// name is declared first (section 4.6, via [`resolve::resolve_file`]),
/// then every record and enum's own fields and variants are resolved (which
/// needs every type name to already be visible), then every function's
/// signature (needed by a call anywhere in the module, including a forward
/// or mutually recursive one), then the module's own top-level statements in
/// source order, and finally every function body, by which point every
/// top-level binding of the file has its real type.
#[allow(clippy::too_many_arguments, reason = "one entry point per module")]
pub fn check_module(
    file: &ast::File,
    module: u32,
    is_entry: bool,
    module_path: String,
    imports: &[ResolvedImport],
    grants: &Grants,
    capability_types: &effects::CapabilityTypes,
    types: &mut TypeInterner,
    names: &mut Interner,
    source: &Source,
    function_sigs: &mut BTreeMap<u32, FnSig>,
    global_types: &mut BTreeMap<(u32, u32), (TypeId, bool)>,
    functions: &mut BTreeMap<u32, Function>,
    next_function_id: &mut u32,
    filter_schema: Option<&hollowscript_core::filter::FilterSchema>,
) -> CheckedModule {
    let uses_result = effects::check_uses(
        &file.uses,
        grants,
        &capability_types.by_capability,
        types,
        names,
        source,
    );
    let ungranted_host_types: Vec<Symbol> = uses_result
        .entries
        .iter()
        .filter(|e| e.capability.is_none())
        .flat_map(|e| effects::host_types_of_standard_capability(names.resolve(e.name)))
        .copied()
        .collect::<Vec<&str>>()
        .into_iter()
        .map(|t| names.intern(t))
        .collect();
    let module_capabilities: Vec<(Symbol, CapabilityId)> = uses_result
        .entries
        .iter()
        .filter_map(|e| e.capability.map(|c| (e.name, c)))
        .collect();
    let scope = resolve::resolve_file(
        file,
        module,
        imports,
        uses_result,
        next_function_id,
        names,
        source,
    );
    let records = scope.records.clone();
    let enums = scope.enums.clone();
    let functions_scope = scope.functions.clone();
    let host_type_names: Vec<(Symbol, DeclId)> = capability_types
        .host_types
        .iter()
        .map(|(name, decl)| (names.intern(name), *decl))
        .collect();
    let shared_mutable = crate::shared::shared_mutable_variables(file);
    let mut checker = Checker::new(
        types,
        names,
        source,
        module,
        is_entry,
        grants,
        &capability_types.by_capability,
        function_sigs,
        global_types,
        functions,
        next_function_id,
        scope,
        filter_schema,
    );
    // A type the host's own capabilities declare (`IoError`) is one the
    // program never writes a declaration for, so nothing else would put it
    // in scope. A program's own declaration of the same name still wins,
    // because the file's own scope is consulted first.
    checker.host_types = host_type_names;
    checker.ungranted_host_types = ungranted_host_types;
    checker.shared_mutable = shared_mutable;
    checker.resolve_type_declarations(file, &records, &enums);
    let sigs = checker.resolve_function_signatures(file, &functions_scope);
    for (id, sig) in sigs {
        checker.function_sigs.insert(id, sig);
    }
    // Section 4.10's early-use rule (HS0210): syntactic, so it runs once
    // over the raw file rather than depending on `check_top_level`'s own
    // declaration order below (see that function's own note).
    for fault in crate::early_use::early_use_faults(file) {
        let function_text = checker.text(fault.function);
        let name_text = checker.text(fault.binding);
        let line = checker.source.locate(fault.related).line;
        checker.error_with_related(
            Code::HS0210,
            &[
                ("function", &function_text),
                ("name", &name_text),
                ("line", &line),
            ],
            fault.span,
            fault.related,
            "the statement",
        );
    }
    // `check_top_level` already registers each global into
    // `checker.global_types` as it is checked (not after every top-level
    // statement is, which would be too late for an earlier declaration to
    // see a later one, and section 4.10 requires the opposite order anyway).
    let (globals, init) = checker.check_top_level(file, module);
    checker.check_function_bodies(file, &functions_scope);
    let exports = checker.build_exports(file, &globals);
    checker.report_unused_names(imports);
    let module_hir = hir::Module {
        path: module_path,
        capabilities: module_capabilities,
        globals,
        function_ids: checker.module_function_ids.clone(),
        init,
        exports,
        node_spans: crate::spans::collect(file, source),
    };
    let mut filter_literals = checker.filter_literals;
    filter_literals.sort_by_key(|literal| literal.location.span);
    filter_literals.dedup_by_key(|literal| literal.location.span);
    CheckedModule {
        module: module_hir,
        diagnostics: checker.diagnostics.into_vec(),
        filter_literals,
    }
}

impl Checker<'_> {
    fn resolve_type_declarations(
        &mut self,
        file: &ast::File,
        records: &[(DeclId, usize)],
        enums: &[(DeclId, usize)],
    ) {
        for &(decl, index) in records {
            let Some(ast::ItemKind::Record(rdecl)) = file.items.get(index).map(|i| &i.kind) else {
                continue;
            };
            self.type_scope.push();
            for (i, tp) in rdecl.type_params.iter().enumerate() {
                self.type_scope.declare(resolve::ScopeEntry {
                    name: tp.name,
                    binding: TypeBinding::Param {
                        decl,
                        index: u32::try_from(i).unwrap_or(0),
                    },
                    span: tp.span,
                });
            }
            let mut fields = Vec::new();
            let mut seen = Vec::new();
            for f in &rdecl.fields {
                if seen.contains(&f.name.name) {
                    let text = self.text(f.name.name);
                    self.error_at(
                        Code::HS0202,
                        &[
                            ("kind", &"field"),
                            ("name", &text),
                            ("container", &"record"),
                        ],
                        f.name.span,
                    );
                    continue;
                }
                seen.push(f.name.name);
                let ty = self.resolve_type(&f.ty);
                fields.push(Field {
                    name: f.name.name,
                    ty,
                });
            }
            self.type_scope.pop();
            let params: Vec<Symbol> = rdecl.type_params.iter().map(|p| p.name).collect();
            self.types
                .declare(decl, rdecl.name.name, DeclShape::Record { params, fields });
        }
        for &(decl, index) in enums {
            let Some(ast::ItemKind::Enum(edecl)) = file.items.get(index).map(|i| &i.kind) else {
                continue;
            };
            self.type_scope.push();
            for (i, tp) in edecl.type_params.iter().enumerate() {
                self.type_scope.declare(resolve::ScopeEntry {
                    name: tp.name,
                    binding: TypeBinding::Param {
                        decl,
                        index: u32::try_from(i).unwrap_or(0),
                    },
                    span: tp.span,
                });
            }
            let mut variants = Vec::new();
            let mut seen = Vec::new();
            for v in &edecl.variants {
                if seen.contains(&v.name.name) {
                    let text = self.text(v.name.name);
                    self.error_at(
                        Code::HS0202,
                        &[
                            ("kind", &"variant"),
                            ("name", &text),
                            ("container", &"enum"),
                        ],
                        v.name.span,
                    );
                    continue;
                }
                seen.push(v.name.name);
                let payload: Vec<TypeId> = v.payload.iter().map(|t| self.resolve_type(t)).collect();
                variants.push(Variant {
                    name: v.name.name,
                    payload,
                });
            }
            self.type_scope.pop();
            let params: Vec<Symbol> = edecl.type_params.iter().map(|p| p.name).collect();
            self.types
                .declare(decl, edecl.name.name, DeclShape::Enum { params, variants });
        }
        for &(decl, index) in records {
            let Some(item) = file.items.get(index) else {
                continue;
            };
            if self.record_has_cycle(decl) {
                let text = self
                    .types
                    .decl_name(decl)
                    .map(|s| self.text(s))
                    .unwrap_or_default();
                self.error_at(Code::HS0305, &[("name", &text)], item.span);
                continue;
            }
            let named = self.types.named(decl, Vec::new());
            let size = self.types.expanded_size(self.names, named);
            if size > crate::types::MAX_TYPE_NODES {
                self.error_at(
                    Code::HS0335,
                    &[("limit", &crate::types::MAX_TYPE_NODES)],
                    item.span,
                );
            }
        }
    }

    /// Type arguments and enum payloads are followed (section 2.5: "an enum
    /// used as a type argument inside the record is part of the record's
    /// own type and is followed"); an enum's own declared payload fields are
    /// not (the trace "stops at an enum").
    fn record_type_neighbors(&mut self, ty: TypeId, out: &mut Vec<DeclId>) {
        match self.types.view(ty) {
            TypeView::Named { decl, args } => {
                let args = args.to_vec();
                if matches!(self.types.decl_shape(decl), Some(DeclShape::Record { .. })) {
                    out.push(decl);
                }
                for a in args {
                    self.record_type_neighbors(a, out);
                }
            }
            TypeView::List(t) | TypeView::Set(t) | TypeView::Optional(t) => {
                self.record_type_neighbors(t, out);
            }
            TypeView::Map(k, v) => {
                self.record_type_neighbors(k, out);
                self.record_type_neighbors(v, out);
            }
            TypeView::Function { params, ret, .. } => {
                let params = params.to_vec();
                for p in params {
                    self.record_type_neighbors(p, out);
                }
                if let Some(r) = ret {
                    self.record_type_neighbors(r, out);
                }
            }
            TypeView::Record(fields) => {
                let fields = fields.to_vec();
                for (_, t) in fields {
                    self.record_type_neighbors(t, out);
                }
            }
            TypeView::Int
            | TypeView::Float
            | TypeView::Bool
            | TypeView::String
            | TypeView::Filter
            | TypeView::Param { .. }
            | TypeView::Error => {}
        }
    }

    /// Whether `start`, a record declaration, reaches itself through its own
    /// fields (section 2.5, HS0305): a breadth-first search over the
    /// declaration graph [`Checker::record_type_neighbors`] builds, which
    /// approximates the specification's own substitution-based rule by
    /// working at the level of which declarations refer to which, not which
    /// particular instantiation does; this is sound for every example the
    /// specification itself gives and cannot loop forever, since each
    /// declaration is visited at most once.
    fn record_has_cycle(&mut self, start: DeclId) -> bool {
        let Some(DeclShape::Record { fields, .. }) = self.types.decl_shape(start).cloned() else {
            return false;
        };
        let mut frontier = Vec::new();
        for f in &fields {
            self.record_type_neighbors(f.ty, &mut frontier);
        }
        let mut seen = BTreeSet::new();
        while let Some(d) = frontier.pop() {
            if d == start {
                return true;
            }
            if !seen.insert(d) {
                continue;
            }
            if let Some(DeclShape::Record { fields, .. }) = self.types.decl_shape(d).cloned() {
                for f in &fields {
                    self.record_type_neighbors(f.ty, &mut frontier);
                }
            }
        }
        false
    }

    /// Phase 3: resolves every top-level `fn`'s parameter and return types,
    /// before any body is checked, so that a call anywhere in the module,
    /// including to a function declared later, sees its real signature. Each
    /// function's own type parameters, if any, are declared into a fresh
    /// [`DeclId`] scoped to it alone, following the record/enum counter of
    /// this module so every declaration in one module has a distinct id.
    fn resolve_function_signatures(
        &mut self,
        file: &ast::File,
        functions: &[(FunctionId, usize)],
    ) -> Vec<(u32, FnSig)> {
        let mut next_decl_index = u32::try_from(
            file.items
                .iter()
                .filter(|i| matches!(i.kind, ast::ItemKind::Record(_) | ast::ItemKind::Enum(_)))
                .count(),
        )
        .unwrap_or(0);
        let mut out = Vec::with_capacity(functions.len());
        for &(id, index) in functions {
            let Some(ast::ItemKind::Fn(decl)) = file.items.get(index).map(|i| &i.kind) else {
                continue;
            };
            let fn_decl = DeclId {
                module: self.module,
                index: next_decl_index,
            };
            next_decl_index += 1;
            self.type_scope.push();
            for (i, tp) in decl.type_params.iter().enumerate() {
                self.type_scope.declare(resolve::ScopeEntry {
                    name: tp.name,
                    binding: TypeBinding::Param {
                        decl: fn_decl,
                        index: u32::try_from(i).unwrap_or(0),
                    },
                    span: tp.span,
                });
            }
            // A duplicate parameter name is reported where the parameters
            // are declared into the body's scope, which is one place for a
            // function and a closure alike.
            let mut params = Vec::new();
            for p in &decl.params {
                params.push(self.resolve_type(&p.ty));
            }
            let ret = decl.ret.as_ref().map(|t| self.resolve_type(t));
            self.type_scope.pop();
            out.push((
                id.0,
                FnSig {
                    params,
                    ret,
                    type_param_count: u32::try_from(decl.type_params.len()).unwrap_or(0),
                    name: decl.name.name,
                    type_param_decl: fn_decl,
                },
            ));
        }
        out
    }

    /// Phase 4: checks every top-level statement in source order (section
    /// 4.10), declaring each `constant`/`variable` into the value scope only
    /// once its own statement has been checked, and building the module's
    /// initialisation block. The early-use rule itself (a top-level
    /// statement that names a function before a binding its `Refs` needs is
    /// declared, HS0210) is reported separately, by `check_module` calling
    /// `crate::early_use::early_use_faults` ahead of this phase: that pass
    /// is syntactic (section 4.10 "looks only at names") and needs no type,
    /// so it runs once over the raw file rather than duplicating this
    /// phase's own declaration order.
    fn check_top_level(&mut self, file: &ast::File, module: u32) -> (Vec<hir::Global>, Block) {
        let mut top_level_frame = FnCtx::new(None, None);
        top_level_frame.is_top_level = true;
        self.frames.push(top_level_frame);
        let mut globals = Vec::new();
        let mut stmts = Vec::new();
        let mut next_global_index = 0u32;
        // The facts in force at the top level (section 4.8): threaded
        // through the module's own statements exactly as a function body
        // threads them through its block, since nothing in the
        // specification's rules for `if`, the loops or an assignment
        // distinguishes a top-level path from a local one.
        let mut facts = FactSet::empty();
        for item in &file.items {
            match &item.kind {
                ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
                    let mutable = matches!(item.kind, ast::ItemKind::Variable(_));
                    let expected = decl.ty.as_ref().map(|t| self.resolve_type(t));
                    let checked_init = match expected {
                        Some(t) => self.check_against(&decl.init, t, &facts),
                        None => self.check_expr(&decl.init, None, &facts),
                    };
                    let ty = expected.unwrap_or(checked_init.ty);
                    let ident = match &decl.target {
                        ast::BindingTarget::Name(ident) => ident,
                        ast::BindingTarget::Pattern(pattern) => {
                            self.declare_top_level_pattern(
                                item,
                                pattern,
                                ty,
                                mutable,
                                checked_init,
                                module,
                                &mut next_global_index,
                                &mut globals,
                                &mut stmts,
                            );
                            continue;
                        }
                    };
                    let id = GlobalId {
                        module,
                        index: next_global_index,
                    };
                    next_global_index += 1;
                    let text = self.text(ident.name);
                    if self.values.first_declaration(ident.name).is_some() {
                        self.error_at(Code::HS0201, &[("name", &text)], ident.span);
                    }
                    // Declared even after a clash, as a local declaration is
                    // (see `declare_binding_kind`): the code below it means
                    // the name it just bound, and section 17.3's warning
                    // then lands on whichever of the two nothing reads.
                    self.values.declare(resolve::ScopeEntry {
                        name: ident.name,
                        binding: ValueBinding::Global(id),
                        span: ident.span,
                    });
                    // Section 17.3 does not warn for an exported
                    // declaration: another module is what reads it.
                    if item.export.is_none() {
                        self.warn_if_unused(Code::HS0801, ident.name, ident.span);
                    }
                    // Registered as soon as this one global is checked, not
                    // after every top-level statement is (which is too
                    // late for a later statement in this same loop that
                    // reads it, section 4.10's own top-level order): a
                    // constant is visible to the rest of the file from here.
                    self.global_types.insert((module, id.index), (ty, mutable));
                    let shared = mutable && self.shared_mutable.contains(&ident.span);
                    if shared {
                        self.shared_globals.insert(id);
                    }
                    globals.push(hir::Global {
                        id,
                        node: item.id,
                        name: ident.name,
                        mutable,
                        shared_mutable: shared,
                        ty,
                        init: Some(checked_init),
                    });
                    // Section 4.8: "Declarations ... do not change the fact
                    // set except through the bindings they declare (a new
                    // `constant` or `variable` has no facts)".
                }
                ast::ItemKind::Stmt(stmt) => {
                    let (checked, new_facts, _) = self.check_stmt(stmt, facts);
                    facts = new_facts;
                    stmts.push(checked);
                }
                ast::ItemKind::Fn(_) | ast::ItemKind::Record(_) | ast::ItemKind::Enum(_) => {}
            }
        }
        self.frames.pop();
        (globals, Block { stmts })
    }

    /// A top-level `constant` or `variable` whose target is a pattern
    /// (`constant { x, y } = point`).
    ///
    /// Each name it binds is a top-level binding of its own, because another
    /// module may import it and a function of this one may read it, and a
    /// top-level binding is a [`GlobalId`]. One initialiser gives all of
    /// them their values, which no single `Global` can express, so the
    /// destructuring becomes a statement of the module's initialiser: a
    /// `Let` that evaluates the initialiser once and binds the pattern to
    /// temporaries, then one `AssignGlobal` per name. The temporaries are
    /// declared in a scope that is popped immediately, so nothing resolves
    /// to them and every read of the name, here or anywhere else, goes to
    /// the global.
    #[allow(clippy::too_many_arguments, reason = "one call site")]
    fn declare_top_level_pattern(
        &mut self,
        item: &ast::Item,
        pattern: &ast::Pattern,
        ty: TypeId,
        mutable: bool,
        init: Expr,
        module: u32,
        next_global_index: &mut u32,
        globals: &mut Vec<hir::Global>,
        stmts: &mut Vec<Stmt>,
    ) {
        let mut bindings = Vec::new();
        self.current_frame().locals.push();
        // The temporaries this pattern binds are popped again below and
        // nothing ever resolves to them, so they are not names that can go
        // unused: the globals they feed are, and those are registered
        // further down. Their spans are worth keeping, though, because each
        // name needs its own: two globals sharing the item's span would
        // make a read of either look like a read of both.
        let before = self.unused.len();
        let checked_pattern = self.check_declaration_pattern(pattern, ty, mutable, &mut bindings);
        let name_spans: BTreeMap<u32, Span> = self
            .unused
            .drain(before..)
            .map(|candidate| (candidate.name.index(), candidate.span))
            .collect();
        self.current_frame().locals.pop();

        let mut assignments = Vec::with_capacity(bindings.len());
        for binding in &bindings {
            let id = GlobalId {
                module,
                index: *next_global_index,
            };
            *next_global_index += 1;
            let span = name_spans
                .get(&binding.name.index())
                .copied()
                .unwrap_or(item.span);
            // Section 17.1's "one mistake gives one diagnostic": a clash
            // between this binding and a name already visible (whether a
            // top-level one or another binding of this same pattern) was
            // already reported by check_declaration_pattern's own call to
            // declare_binding_kind above, which checked the identical
            // condition against the identical, still-unchanged self.values.
            // Checking it again here reported HS0201 twice for one mistake.
            self.values.declare(resolve::ScopeEntry {
                name: binding.name,
                binding: ValueBinding::Global(id),
                span,
            });
            if item.export.is_none() {
                self.warn_if_unused(Code::HS0801, binding.name, span);
            }
            self.global_types
                .insert((module, id.index), (binding.ty, mutable));
            let shared = mutable && self.shared_mutable.contains(&span);
            if shared {
                self.shared_globals.insert(id);
            }
            globals.push(hir::Global {
                id,
                node: item.id,
                name: binding.name,
                mutable,
                shared_mutable: shared,
                ty: binding.ty,
                init: None,
            });
            assignments.push((
                id,
                Expr {
                    node: binding.node,
                    ty: binding.ty,
                    kind: ExprKind::Local(binding.local),
                },
            ));
        }

        stmts.push(Stmt {
            node: item.id,
            kind: StmtKind::Let {
                bindings,
                pattern: Some(checked_pattern),
                init,
            },
        });
        for (global, value) in assignments {
            stmts.push(Stmt {
                node: item.id,
                kind: StmtKind::AssignGlobal { global, value },
            });
        }
    }

    /// Phase 5: checks every top-level function's body, now that every
    /// top-level binding of the module has its real type.
    fn check_function_bodies(&mut self, file: &ast::File, functions: &[(FunctionId, usize)]) {
        for &(id, index) in functions {
            let Some(item) = file.items.get(index) else {
                continue;
            };
            let ast::ItemKind::Fn(decl) = &item.kind else {
                continue;
            };
            let Some(sig) = self.function_sigs.get(&id.0).cloned() else {
                continue;
            };
            self.type_scope.push();
            for (i, tp) in decl.type_params.iter().enumerate() {
                self.type_scope.declare(resolve::ScopeEntry {
                    name: tp.name,
                    binding: TypeBinding::Param {
                        decl: sig.type_param_decl,
                        index: u32::try_from(i).unwrap_or(0),
                    },
                    span: tp.span,
                });
            }
            let param_names: Vec<Symbol> = decl.params.iter().map(|p| p.name.name).collect();
            let param_spans: Vec<Span> = decl.params.iter().map(|p| p.name.span).collect();
            let (built_id, _) = self.check_function_body_inner(
                item.id,
                decl.name.name,
                &sig.params,
                &param_names,
                &param_spans,
                sig.ret,
                false,
                &decl.body,
                Some(id),
            );
            self.type_scope.pop();
            debug_assert_eq!(built_id.0, id.0, "the reserved id was reused");
        }
    }

    fn build_exports(
        &mut self,
        file: &ast::File,
        globals: &[hir::Global],
    ) -> Vec<(Symbol, hir::Export)> {
        let mut exports = Vec::new();
        for item in &file.items {
            let Some(export_span) = item.export else {
                continue;
            };
            match &item.kind {
                ast::ItemKind::Fn(decl) => {
                    if let Some(ValueBinding::Function(id)) = self.values.lookup(decl.name.name) {
                        let fn_ty = self.function_signature_type(id);
                        self.check_export_leak(fn_ty, export_span, &decl.name);
                        if self.is_entry {
                            let sig = self.function_sigs.get(&id.0).cloned();
                            let generic = sig.as_ref().is_some_and(|s| s.type_param_count > 0);
                            // Section 12.4's rule is about the function's own
                            // parameter and return types, each transferable
                            // on its own, not about the function value's
                            // overall type, which is itself a function type
                            // and so would always fail the same check the
                            // exported *constant* case below runs directly.
                            let signature_ok = sig.is_none_or(|s| {
                                s.params
                                    .iter()
                                    .all(|&p| self.is_transferable(p, &mut Vec::new()))
                                    && s.ret
                                        .is_none_or(|r| self.is_transferable(r, &mut Vec::new()))
                            });
                            if generic || !signature_ok {
                                let text = self.text(decl.name.name);
                                let reason = if generic {
                                    "it has a type parameter"
                                } else {
                                    "its signature is not transferable"
                                };
                                self.error_at(
                                    Code::HS0606,
                                    &[("kind", &"function"), ("name", &text), ("reason", &reason)],
                                    export_span,
                                );
                            }
                        }
                        exports.push((decl.name.name, hir::Export::Function(id)));
                    }
                }
                ast::ItemKind::Constant(decl) => {
                    if let ast::BindingTarget::Name(ident) = &decl.target {
                        if let Some(global) = globals.iter().find(|g| g.name == ident.name) {
                            self.check_export_leak(global.ty, export_span, ident);
                            if self.is_entry {
                                self.check_export_transferable(
                                    "constant",
                                    global.ty,
                                    export_span,
                                    ident,
                                );
                            }
                            exports.push((ident.name, hir::Export::Constant(global.id)));
                        }
                    }
                }
                ast::ItemKind::Record(decl) => {
                    if let Some(TypeBinding::Named(decl_id)) =
                        self.type_scope.lookup(decl.name.name)
                    {
                        exports.push((decl.name.name, hir::Export::Type(decl_id)));
                    }
                }
                ast::ItemKind::Enum(decl) => {
                    if let Some(TypeBinding::Named(decl_id)) =
                        self.type_scope.lookup(decl.name.name)
                    {
                        exports.push((decl.name.name, hir::Export::Type(decl_id)));
                    }
                }
                ast::ItemKind::Variable(_) | ast::ItemKind::Stmt(_) => {}
            }
        }
        exports
    }

    /// HS0613: no capability leaves a module through an export.
    fn check_export_leak(&mut self, ty: TypeId, span: Span, ident: &ast::Ident) {
        if self.type_contains_effectful_function(ty, &mut Vec::new()) {
            let text = self.text(ident.name);
            self.error_at(
                Code::HS0613,
                &[("kind", &"declaration"), ("name", &text)],
                span,
            );
        }
    }

    /// HS0606 for an exported `constant` of the entry module (section
    /// 12.4): a host can only read one by name, so its type must be
    /// *transferable* (`Int`, `Float`, `Bool`, `String`, or a list, map,
    /// set, record, enum or optional of transferable types). An exported
    /// `fn`'s own signature is checked at its own call site instead of
    /// through this function, because a function's *value* is always a
    /// function type, which `is_transferable` always refuses, correctly for
    /// a constant that holds one (the section's own worked example) but not
    /// for the function declaration itself, whose parameter and return
    /// types are what section 12.4 actually restricts.
    fn check_export_transferable(
        &mut self,
        kind: &str,
        ty: TypeId,
        span: Span,
        ident: &ast::Ident,
    ) {
        if !self.is_transferable(ty, &mut Vec::new()) {
            let text = self.text(ident.name);
            self.error_at(
                Code::HS0606,
                &[
                    ("kind", &kind),
                    ("name", &text),
                    ("reason", &"its type is not transferable"),
                ],
                span,
            );
        }
    }

    /// Whether `ty` is transferable (section 12.4): `Int`, `Float`, `Bool`,
    /// `String`, or a list, map, set, record, enum or optional whose
    /// component types are transferable. A function type, `Filter` and a
    /// type parameter are not. An enum's payload types are read
    /// unsubstituted, exactly as `type_contains_effectful_function` reads
    /// them, and its own type arguments are checked separately: the same
    /// approximation, in the safe direction (only wider, never narrower,
    /// than substituting first), for the same reason.
    fn is_transferable(&mut self, ty: TypeId, seen: &mut Vec<TypeId>) -> bool {
        if seen.contains(&ty) {
            // A recursive enum does not become less transferable by
            // referring to itself; nothing further to learn by revisiting.
            return true;
        }
        seen.push(ty);
        match self.types.view(ty) {
            TypeView::List(t) | TypeView::Set(t) | TypeView::Optional(t) => {
                self.is_transferable(t, seen)
            }
            TypeView::Map(k, v) => self.is_transferable(k, seen) && self.is_transferable(v, seen),
            TypeView::Record(fields) => {
                let fields = fields.to_vec();
                fields.iter().all(|&(_, t)| self.is_transferable(t, seen))
            }
            TypeView::Named { decl, args } => {
                let args = args.to_vec();
                let payload_transferable = match self.types.decl_shape(decl).cloned() {
                    Some(DeclShape::Enum { variants, .. }) => variants
                        .iter()
                        .flat_map(|v| v.payload.clone())
                        .all(|t| self.is_transferable(t, seen)),
                    // A record reaching here (rather than as
                    // `TypeView::Record`, which `expand_record` normally
                    // gives) is not something this rule has an example of;
                    // treated as transferable rather than rejecting a
                    // program on a case nothing here can construct.
                    _ => true,
                };
                payload_transferable && args.iter().all(|&t| self.is_transferable(t, seen))
            }
            TypeView::Function { .. } | TypeView::Filter | TypeView::Param { .. } => false,
            // Error already reported (not this check's mistake to repeat);
            // the scalars are transferable outright.
            TypeView::Int
            | TypeView::Float
            | TypeView::Bool
            | TypeView::String
            | TypeView::Error => true,
        }
    }

    fn type_contains_effectful_function(&mut self, ty: TypeId, seen: &mut Vec<TypeId>) -> bool {
        if seen.contains(&ty) {
            return false;
        }
        seen.push(ty);
        match self.types.view(ty) {
            TypeView::Function { effect: true, .. } => true,
            TypeView::Function {
                effect: false,
                params,
                ret,
            } => {
                let params = params.to_vec();
                params
                    .iter()
                    .any(|&p| self.type_contains_effectful_function(p, seen))
                    || ret.is_some_and(|r| self.type_contains_effectful_function(r, seen))
            }
            TypeView::List(t) | TypeView::Set(t) | TypeView::Optional(t) => {
                self.type_contains_effectful_function(t, seen)
            }
            TypeView::Map(k, v) => {
                self.type_contains_effectful_function(k, seen)
                    || self.type_contains_effectful_function(v, seen)
            }
            TypeView::Record(fields) => {
                let fields = fields.to_vec();
                fields
                    .iter()
                    .any(|&(_, t)| self.type_contains_effectful_function(t, seen))
            }
            TypeView::Named { decl, args } => {
                let args = args.to_vec();
                let payload_effectful = match self.types.decl_shape(decl).cloned() {
                    Some(DeclShape::Enum { variants, .. }) => variants
                        .iter()
                        .flat_map(|v| v.payload.clone())
                        .any(|t| self.type_contains_effectful_function(t, seen)),
                    _ => false,
                };
                payload_effectful
                    || args
                        .iter()
                        .any(|&t| self.type_contains_effectful_function(t, seen))
            }
            TypeView::Int
            | TypeView::Float
            | TypeView::Bool
            | TypeView::String
            | TypeView::Filter
            | TypeView::Param { .. }
            | TypeView::Error => false,
        }
    }
}

#[cfg(test)]
mod unify_tests {
    use super::{Substitution, TypeVarId, UnifyError, is_assignable, unify};
    use crate::types::{DeclId, DeclShape, TypeInterner};
    use hollowscript_core::Interner;

    #[test]
    fn unbound_reports_only_variables_never_bound() {
        let mut s = Substitution::new();
        let a = TypeVarId(0);
        let b = TypeVarId(1);
        s.bind(a, TypeInterner::new().int());
        assert_eq!(s.unbound(&[a, b]), vec![&b]);
        assert!(s.get(a).is_some());
        assert!(s.get(b).is_none());
    }

    #[test]
    fn a_bare_variable_binds_to_the_argument() {
        let mut types = TypeInterner::new();
        let decl = DeclId {
            module: 0,
            index: 0,
        };
        let var = types.param(decl, 0);
        let mut sub = Substitution::new();
        assert_eq!(unify(var, types.int(), &types, &mut sub), Ok(()));
        assert_eq!(sub.get(TypeVarId(0)), Some(types.int()));
    }

    #[test]
    fn a_variable_bound_twice_to_different_types_is_rebound() {
        let mut types = TypeInterner::new();
        let decl = DeclId {
            module: 0,
            index: 0,
        };
        let var = types.param(decl, 0);
        let mut sub = Substitution::new();
        unify(var, types.int(), &types, &mut sub).unwrap();
        let err = unify(var, types.string(), &types, &mut sub).unwrap_err();
        assert!(matches!(err, UnifyError::Rebound { .. }));
    }

    #[test]
    fn optional_variable_binds_to_either_the_bare_or_optional_argument() {
        let mut types = TypeInterner::new();
        let decl = DeclId {
            module: 0,
            index: 0,
        };
        let var = types.param(decl, 0);
        let opt_var = types.optional(var);
        let mut sub = Substitution::new();
        assert_eq!(unify(opt_var, types.int(), &types, &mut sub), Ok(()));
        assert_eq!(sub.get(TypeVarId(0)), Some(types.int()));

        let mut sub = Substitution::new();
        let opt_int = types.optional(types.int());
        assert_eq!(unify(opt_var, opt_int, &types, &mut sub), Ok(()));
        assert_eq!(sub.get(TypeVarId(0)), Some(types.int()));
    }

    #[test]
    fn generic_records_unify_field_by_field() {
        let mut types = TypeInterner::new();
        let mut names = Interner::new();
        let decl = DeclId {
            module: 0,
            index: 0,
        };
        let l = names.intern("l");
        let r = names.intern("r");
        let pair_sym = names.intern("Pair");
        let a_param = types.param(decl, 0);
        types.declare(
            decl,
            pair_sym,
            DeclShape::Record {
                params: vec![names.intern("A")],
                fields: vec![
                    crate::types::Field {
                        name: l,
                        ty: a_param,
                    },
                    crate::types::Field {
                        name: r,
                        ty: a_param,
                    },
                ],
            },
        );
        let pair_of_var = types.named(decl, vec![a_param]);
        let expanded_param = types.expand_record(&names, pair_of_var).unwrap();
        let pair_of_int = types.named(decl, vec![types.int()]);
        let expanded_int = types.expand_record(&names, pair_of_int).unwrap();
        let mut sub = Substitution::new();
        assert_eq!(
            unify(expanded_param, expanded_int, &types, &mut sub),
            Ok(())
        );
        assert_eq!(sub.get(TypeVarId(0)), Some(types.int()));
    }

    #[test]
    fn occurs_check_rejects_a_variable_unifying_with_itself() {
        let mut types = TypeInterner::new();
        let decl = DeclId {
            module: 0,
            index: 0,
        };
        let var = types.param(decl, 0);
        let list_of_var = types.list(var);
        let mut sub = Substitution::new();
        let err = unify(var, list_of_var, &types, &mut sub).unwrap_err();
        assert!(matches!(err, UnifyError::Occurs { .. }));
    }

    #[test]
    fn assignability_allows_pure_where_effectful_is_expected_and_int_into_optional() {
        let mut types = TypeInterner::new();
        let pure = types.function(false, vec![types.int()], None);
        let effectful = types.function(true, vec![types.int()], None);
        assert!(is_assignable(pure, effectful, &types));
        assert!(!is_assignable(effectful, pure, &types));
        let opt_int = types.optional(types.int());
        assert!(is_assignable(types.int(), opt_int, &types));
        assert!(!is_assignable(types.string(), types.int(), &types));
    }
}
