//! Name resolution and modules (sections 2.1, 4.6 and chapter 10).
//!
//! Resolution turns every name of the syntax tree into a [`ValueBinding`] or a
//! [`TypeBinding`] and
//! builds the module graph in the load order of section 10.5: the entry
//! module and its imports, loaded depth first, each exactly once by canonical
//! path. It runs before types are known, because a name's kind (a value or a
//! type, a local or a capability) decides how the rest of the checker treats
//! it, and the no-shadowing rule of section 4.6 depends only on names.
//!
//! The two namespaces of section 2.1 are kept as two scope stacks: looking up
//! a name in an expression searches the value scopes, and looking up a type
//! name searches the type scopes. A capability name is entered into the value
//! scope of a file with a fact recorded alongside it so that no later
//! declaration, of either namespace where relevant, may reuse it.

use crate::types::{DeclId, TypeId, TypeInterner};
use hollowscript_core::{BoundedDiagnostics, Code, Diagnostic, Span, Symbol};

/// What a resolved value name refers to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ValueBinding {
    /// A local of the current function.
    Local(crate::hir::LocalId),
    /// A top-level `constant` or `variable`.
    Global(crate::hir::GlobalId),
    /// A top-level `fn`.
    Function(crate::hir::FunctionId),
    /// A capability named by a `uses` line.
    Capability(crate::hir::CapabilityId),
    /// An enum name used as a namespace of variant constructors.
    EnumNamespace(DeclId),
    /// A name of the prelude (section 13.6), by its index in a fixed table
    /// the checker owns. Hidden by any program declaration of the same name
    /// except the nine built-in types (section 2.1).
    Prelude(u32),
    /// An import that did not resolve, whose own failure (HS0601, HS0603,
    /// HS0604, HS0605) is already reported. The name still occupies the
    /// scope and reads as the error type, so that reading it says nothing
    /// further: section 17.3's "A diagnostic is not reported for an
    /// expression whose problem has already been reported".
    Unresolved,
}

/// What a resolved type name refers to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TypeBinding {
    /// A built-in type of fixed arity zero, such as `Int`.
    Builtin(TypeId),
    /// A prelude type that takes type arguments (section 2.1): `Result`,
    /// `Map`, `Set` and `MapEntry`.
    BuiltinGeneric(BuiltinGeneric),
    /// A `record` or `enum` declaration.
    Named(DeclId),
    /// An import that did not resolve, for the reason
    /// [`ValueBinding::Unresolved`] gives.
    Unresolved,
    /// A type parameter in scope in the declaration that introduces it.
    Param {
        /// The declaration that introduces the parameter.
        decl: DeclId,
        /// Its position in the declaration's parameter list.
        index: u32,
    },
}

/// One of the prelude's generic types (section 2.1), each with a fixed arity
/// and a fixed meaning that does not depend on any declaration in the
/// program: `Result<T, E>` is [`TypeInterner::result`], `MapEntry<K, V>` is
/// the record `{ key: K, value: V }` (section 4.5), and `Map`/`Set` are
/// [`TypeInterner::map`]/[`TypeInterner::set`] themselves.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BuiltinGeneric {
    /// `Result<T, E>`.
    Result,
    /// `Map<K, V>`.
    Map,
    /// `Set<T>`.
    Set,
    /// `MapEntry<K, V>`.
    MapEntry,
}

impl BuiltinGeneric {
    /// The number of type arguments this type takes (HS0303 otherwise).
    #[must_use]
    pub fn arity(self) -> usize {
        match self {
            BuiltinGeneric::Result | BuiltinGeneric::Map | BuiltinGeneric::MapEntry => 2,
            BuiltinGeneric::Set => 1,
        }
    }

    /// Builds the type for `args`, which must already have
    /// [`BuiltinGeneric::arity`] elements.
    pub fn instantiate(
        self,
        types: &mut TypeInterner,
        names: &mut hollowscript_core::Interner,
        args: &[TypeId],
    ) -> TypeId {
        match (self, args) {
            (BuiltinGeneric::Result, [t, e]) => types.result(names, *t, *e),
            (BuiltinGeneric::Map, [k, v]) => types.map(*k, *v),
            (BuiltinGeneric::Set, [t]) => types.set(*t),
            (BuiltinGeneric::MapEntry, [k, v]) => {
                let key = names.intern("key");
                let value = names.intern("value");
                types.record(
                    names,
                    vec![
                        crate::types::Field { name: key, ty: *k },
                        crate::types::Field {
                            name: value,
                            ty: *v,
                        },
                    ],
                )
            }
            // The caller always supplies `arity()` arguments; a mismatch
            // means the argument count was already reported (HS0303) and the
            // caller substitutes the error type instead of calling this.
            _ => types.error(),
        }
    }
}

/// The type name of the prelude (section 2.1), if `name` is one:
/// `Int`, `Float`, `Bool`, `String` and `Filter` are fixed types;
/// `Result`, `Map`, `Set` and `MapEntry` take type arguments.
#[must_use]
pub fn lookup_prelude_type(name: &str, types: &TypeInterner) -> Option<TypeBinding> {
    Some(match name {
        "Int" => TypeBinding::Builtin(types.int()),
        "Float" => TypeBinding::Builtin(types.float()),
        "Bool" => TypeBinding::Builtin(types.bool()),
        "String" => TypeBinding::Builtin(types.string()),
        "Filter" => TypeBinding::Builtin(types.filter()),
        "Result" => TypeBinding::BuiltinGeneric(BuiltinGeneric::Result),
        "Map" => TypeBinding::BuiltinGeneric(BuiltinGeneric::Map),
        "Set" => TypeBinding::BuiltinGeneric(BuiltinGeneric::Set),
        "MapEntry" => TypeBinding::BuiltinGeneric(BuiltinGeneric::MapEntry),
        _ => return None,
    })
}

/// A prelude value name (section 13.6), other than a method (resolved by
/// receiver type, section 3.7, not by name lookup). This build's roster is
/// the subset expressible with the types of chapters 1 to 10: the constant
/// `pi` and the free numeric and list-building functions of chapter 13.
/// `json` and `time` (namespaces) and the natives whose result type this
/// build has no representation for are not in this table yet; a program that
/// names one is HS0205, the ordinary not-defined error, rather than a
/// diagnostic naming the gap, which is the honest answer for a name this
/// build does not know rather than a crash.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PreludeValue {
    /// `pi`.
    Pi,
    /// A free function taking and returning `Float` (`sqrt`, `sin`, `cos`,
    /// `tan`, `exp`, `ln`, and the two-argument `atan2`/`pow`, which take two
    /// `Float`s).
    FloatFn {
        /// The number of `Float` parameters (1 or 2).
        arity: u32,
    },
    /// `floor`, `ceil`, `round`, `trunc`: `fn(Float): Int` (section 2.7).
    FloatToInt,
    /// `abs`, `sign`: `fn(Float): Float`, folded into [`PreludeValue::FloatFn`]
    /// with arity 1 for typing purposes; kept as a distinct name only so a
    /// caller matching on intent can tell them apart if it ever needs to.
    Abs,
    /// `min`, `max`, `clamp`: `Float` in every position.
    Clamp {
        /// The number of parameters (2 for `min`/`max`, 3 for `clamp`).
        arity: u32,
    },
    /// `div`, `mod`, `remainder`, `ipow`: `fn(Int, Int): Int`.
    IntBinOp,
    /// `range`: `fn(Int, Int): [Int]`.
    Range,
    /// `parseInt`: `fn(String): Int?`.
    ParseInt,
    /// `parseFloat`: `fn(String): Float?`.
    ParseFloat,
    /// `toFloat`: `fn(Int): Float`.
    ToFloat,
    /// `fixed`: `fn(Float, Int): String`.
    Fixed,
    /// `keys`, `has` (section 13.5), `set` and `mapFrom` (chapter 14), and
    /// `success` and `failure` (section 8.2): each
    /// is generic in a way no written type can stand for (a record of any
    /// type, an element type the argument fixes), so its signature is built
    /// at the call site rather than here, and it has no type of its own to
    /// give. Using one without calling it is HS0334.
    CallOnly,
}

/// The prelude value named `name`, if this build knows one.
#[must_use]
pub fn lookup_prelude_value(name: &str) -> Option<PreludeValue> {
    Some(match name {
        "pi" => PreludeValue::Pi,
        "sqrt" | "sin" | "cos" | "tan" | "exp" | "ln" => PreludeValue::FloatFn { arity: 1 },
        "atan2" | "pow" => PreludeValue::FloatFn { arity: 2 },
        "abs" | "sign" => PreludeValue::Abs,
        "floor" | "ceil" | "round" | "trunc" => PreludeValue::FloatToInt,
        "min" | "max" => PreludeValue::Clamp { arity: 2 },
        "clamp" => PreludeValue::Clamp { arity: 3 },
        "div" | "mod" | "remainder" | "ipow" => PreludeValue::IntBinOp,
        "range" => PreludeValue::Range,
        "parseInt" => PreludeValue::ParseInt,
        "parseFloat" => PreludeValue::ParseFloat,
        "toFloat" => PreludeValue::ToFloat,
        "fixed" => PreludeValue::Fixed,
        "keys" | "has" | "set" | "mapFrom" | "success" | "failure" => PreludeValue::CallOnly,
        _ => return None,
    })
}

impl PreludeValue {
    /// This value's type: a plain `Float` constant for [`PreludeValue::Pi`],
    /// a pure function type otherwise.
    pub fn type_id(self, types: &mut TypeInterner) -> TypeId {
        let float = types.float();
        let int = types.int();
        let string = types.string();
        match self {
            PreludeValue::Pi => float,
            PreludeValue::FloatFn { arity } | PreludeValue::Clamp { arity } => {
                types.function(false, vec![float; arity as usize], Some(float))
            }
            PreludeValue::Abs => types.function(false, vec![float], Some(float)),
            PreludeValue::FloatToInt => types.function(false, vec![float], Some(int)),
            PreludeValue::IntBinOp => types.function(false, vec![int, int], Some(int)),
            PreludeValue::Range => {
                let list = types.list(int);
                types.function(false, vec![int, int], Some(list))
            }
            PreludeValue::ParseInt => {
                let opt = types.optional(int);
                types.function(false, vec![string], Some(opt))
            }
            PreludeValue::ParseFloat => {
                let opt = types.optional(float);
                types.function(false, vec![string], Some(opt))
            }
            PreludeValue::ToFloat => types.function(false, vec![int], Some(float)),
            PreludeValue::Fixed => types.function(false, vec![float, int], Some(string)),
            // Built at the call site; using one as a value is HS0334, which
            // the checker reports before it ever asks for a type here.
            PreludeValue::CallOnly => types.error(),
        }
    }
}

/// One entry of a scope, kept so that the related span of a HS0201 can point
/// at the first declaration.
#[derive(Copy, Clone, Debug)]
pub struct ScopeEntry<B> {
    /// The declared name.
    pub name: Symbol,
    /// What it resolves to.
    pub binding: B,
    /// The span of the declaration's own name, for a related span.
    pub span: Span,
}

/// A stack of scopes for one namespace. Looking a name up searches from the
/// innermost scope outward; declaring one checks the same scopes for the
/// no-shadowing rule of section 4.6, whose one exception (the prelude) is
/// applied by the caller before it declares a name here.
#[derive(Debug, Default)]
pub struct Scopes<B> {
    stack: Vec<Vec<ScopeEntry<B>>>,
}

impl<B: Copy> Scopes<B> {
    /// An empty stack.
    #[must_use]
    pub fn new() -> Scopes<B> {
        Scopes { stack: Vec::new() }
    }

    /// Opens a new, innermost scope.
    pub fn push(&mut self) {
        self.stack.push(Vec::new());
    }

    /// Closes the innermost scope, discarding its bindings.
    pub fn pop(&mut self) {
        self.stack.pop();
    }

    /// The binding of `name`, searching from the innermost scope outward.
    #[must_use]
    pub fn lookup(&self, name: Symbol) -> Option<B> {
        self.stack.iter().rev().find_map(|scope| {
            scope
                .iter()
                .rev()
                .find(|e| e.name == name)
                .map(|e| e.binding)
        })
    }

    /// The binding of `name` and the span of its declaration, searching
    /// from the innermost scope outward: the span identifies the
    /// declaration a reference resolved to, which is how an unused-name
    /// warning knows which names were read (section 17.3).
    #[must_use]
    pub fn lookup_entry(&self, name: Symbol) -> Option<(B, Span)> {
        self.stack.iter().rev().find_map(|scope| {
            scope
                .iter()
                .rev()
                .find(|e| e.name == name)
                .map(|e| (e.binding, e.span))
        })
    }

    /// The span of the first declaration of `name` that is visible, for a
    /// related span, searching every open scope including the innermost.
    #[must_use]
    pub fn first_declaration(&self, name: Symbol) -> Option<Span> {
        self.stack
            .iter()
            .flatten()
            .find(|e| e.name == name)
            .map(|e| e.span)
    }

    /// Whether `name` is visible in any open scope (the no-shadowing check).
    #[must_use]
    pub fn is_visible(&self, name: Symbol) -> bool {
        self.first_declaration(name).is_some()
    }

    /// Declares `name` in the innermost scope, without checking for a clash;
    /// the caller applies HS0201 first.
    pub fn declare(&mut self, entry: ScopeEntry<B>) {
        if let Some(top) = self.stack.last_mut() {
            top.push(entry);
        }
    }

    /// The number of open scopes.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

/// One module during resolution: its canonical path and its state, which
/// implements the cycle detection of section 10.5 (an import cycle is HS0603,
/// reported at the import that closes it).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ModuleState {
    /// Being loaded: on the stack of the current depth-first walk. Importing a
    /// module in this state is a cycle.
    Loading,
    /// Checked. Its exports are available to importers.
    Done,
}

/// The result of resolving the module graph: every module reached, in load
/// order (entry first, then each import depth first in the order the `import`
/// lines appear), which is also the order diagnostics are attributed by
/// (section 17.1) and modules are initialised in (section 10.5), read in reverse.
#[derive(Debug, Default)]
pub struct ModuleGraph {
    /// Canonical paths in load order.
    pub order: Vec<String>,
}

/// Why an import path is not well formed (section 10.3), other than
/// climbing above the root, which [`resolve_import_path`] reports separately
/// since it is HS0605, not HS0602.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImportPathError {
    /// It does not begin with `./` or `../`.
    NotRelative,
    /// It contains a backslash.
    Backslash,
    /// It has an empty segment, or ends in `/`.
    EmptySegment,
    /// A segment is made only of dots, other than exactly `.` or `..`.
    DotsOnlySegment,
    /// A segment has a character other than an ASCII letter, digit, `_`, `-`
    /// or `.`.
    InvalidCharacter,
    /// The last segment is `.` or `..`.
    EndsInDotSegment,
    /// A segment other than `.`/`..` ends in `.`.
    SegmentEndsInDot,
    /// A segment's part before its first `.` is a reserved device name.
    ReservedName,
    /// The path ends in `.hws` (HS0609, not HS0602).
    HasExtension,
}

impl ImportPathError {
    /// The `{reason}` text of HS0602's message, for every variant except
    /// [`ImportPathError::HasExtension`], which is HS0609 and has no reason.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            ImportPathError::NotRelative => "it must begin with './' or '../'",
            ImportPathError::Backslash => "a path separator must be '/'",
            ImportPathError::EmptySegment => "a segment cannot be empty",
            ImportPathError::DotsOnlySegment => "a segment of only dots must be '.' or '..'",
            ImportPathError::InvalidCharacter => {
                "a segment may only contain letters, digits, '_', '-' and '.'"
            }
            ImportPathError::EndsInDotSegment => "the last segment cannot be '.' or '..'",
            ImportPathError::SegmentEndsInDot => "a segment cannot end in '.'",
            ImportPathError::ReservedName => "that segment is not a portable file name",
            ImportPathError::HasExtension => "",
        }
    }
}

const RESERVED_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn is_reserved_device_name(segment: &str) -> bool {
    let base = segment.split('.').next().unwrap_or(segment);
    RESERVED_DEVICE_NAMES
        .iter()
        .any(|reserved| base.eq_ignore_ascii_case(reserved))
}

fn validate_import_path_form(raw: &str) -> Result<Vec<&str>, ImportPathError> {
    if !(raw.starts_with("./") || raw.starts_with("../")) {
        return Err(ImportPathError::NotRelative);
    }
    if raw.contains('\\') {
        return Err(ImportPathError::Backslash);
    }
    // Canonical paths are case-sensitive (section 10.3: "Util.hws" and
    // "util.hws" are different modules); the extension check must be too.
    #[allow(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "the specification requires case-sensitive canonical paths"
    )]
    if raw.ends_with(".hws") {
        return Err(ImportPathError::HasExtension);
    }
    let segments: Vec<&str> = raw.split('/').collect();
    for segment in &segments {
        if segment.is_empty() {
            return Err(ImportPathError::EmptySegment);
        }
        let only_dots = segment.bytes().all(|b| b == b'.');
        if only_dots && *segment != "." && *segment != ".." {
            return Err(ImportPathError::DotsOnlySegment);
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        {
            return Err(ImportPathError::InvalidCharacter);
        }
        if *segment != "." && *segment != ".." {
            if segment.ends_with('.') {
                return Err(ImportPathError::SegmentEndsInDot);
            }
            if is_reserved_device_name(segment) {
                return Err(ImportPathError::ReservedName);
            }
        }
    }
    // `segments` is non-empty: `raw` starts with "./" or "../", so splitting
    // on '/' always yields at least two parts.
    let last = segments.last().copied().unwrap_or("");
    if last == "." || last == ".." {
        return Err(ImportPathError::EndsInDotSegment);
    }
    Ok(segments)
}

/// Resolves an import path written in a module whose canonical path has
/// directory segments `from_dir` (section 10.3), to the canonical path
/// segments of the module it denotes (join with `/` for the canonical path
/// text). `Ok(Err(()))` means the path climbs above the root (HS0605); every
/// other malformed path is `Err(ImportPathError)` (HS0602, or HS0609 for
/// [`ImportPathError::HasExtension`]).
///
/// # Errors
///
/// Returns [`ImportPathError`] when the path is not well formed.
pub fn resolve_import_path(
    from_dir: &[String],
    raw: &str,
) -> Result<Result<Vec<String>, ()>, ImportPathError> {
    let segments = validate_import_path_form(raw)?;
    let mut dir: Vec<String> = from_dir.to_vec();
    let Some((last, dir_segments)) = segments.split_last() else {
        return Err(ImportPathError::EmptySegment);
    };
    for segment in dir_segments {
        match *segment {
            "." => {}
            ".." => {
                if dir.pop().is_none() {
                    return Ok(Err(()));
                }
            }
            other => dir.push(other.to_owned()),
        }
    }
    dir.push(format!("{last}.hws"));
    Ok(Ok(dir))
}

/// One name a file's `import` line binds, resolved against the module it
/// names (section 10.2). `value`/`ty` are both absent when the import could
/// not be resolved (the module was not found, the cycle that closed on it,
/// or the name is not exported), in which case the caller still declares the
/// name with the error type so that using it does not cascade further
/// diagnostics (section 2.11); `value` is [`None`] and `ty` present for a
/// name that names only a type (a `record`, or an `enum` used only as a type
/// name would still resolve `value` too, since an enum name is in both
/// namespaces, section 2.1).
#[derive(Clone, Debug)]
pub struct ResolvedImport {
    /// The name as written in the importing file.
    pub name: hollowscript_syntax::ast::Ident,
    /// Its value binding, if the exporting module has one of this name.
    pub value: Option<ValueBinding>,
    /// Its type binding, if the exporting module has one of this name.
    pub ty: Option<TypeBinding>,
}

/// The result of the first pass over one already-parsed file (sections 4.6,
/// 4.10, 9.2 and 10.2): every top-level `fn`, `record`, `enum`, capability
/// and import name is declared into scope and given its identity (a
/// [`crate::types::DeclId`] or [`crate::hir::FunctionId`]), but no type
/// structure is resolved yet (a field, a parameter type, a return type),
/// because that needs every type name of the file to already be visible,
/// which finishing this pass is what guarantees. A top-level `constant` or
/// `variable` is deliberately not declared here: section 4.10 brings each
/// into scope only once its own statement has been checked, which is not
/// this pass's job.
#[derive(Debug)]
pub struct FileScope {
    /// The value namespace: capabilities, imports and top-level functions so
    /// far, with one scope already open for the driver to add top-level
    /// constants and variables to as it checks them, and to push further
    /// scopes on top of for function bodies.
    pub values: Scopes<ValueBinding>,
    /// The type namespace: imports and top-level `record`/`enum` names.
    pub types: Scopes<TypeBinding>,
    /// Every top-level `record`, by its identity and the index of its
    /// [`hollowscript_syntax::ast::Item`] in the file.
    pub records: Vec<(DeclId, usize)>,
    /// Every top-level `enum`, the same way.
    pub enums: Vec<(DeclId, usize)>,
    /// Every top-level `fn`, by its identity and item index.
    pub functions: Vec<(crate::hir::FunctionId, usize)>,
    /// Diagnostics found so far: HS0201 for a name that could not be
    /// declared, HS0212 for a repeated type parameter name, and whatever
    /// [`crate::effects::check_uses`] already found.
    pub diagnostics: BoundedDiagnostics,
}

/// Declares `name` in the innermost scope of `scopes`, unless it is already
/// visible in some open scope, in which case HS0201 is reported instead and
/// the name is left bound to whatever declared it first (section 4.6): a
/// clash never overwrites the original meaning of a name, so that code
/// written above the clash keeps resolving the way it did before it.
/// Returns whether the name was declared.
fn declare_or_report<B: Copy>(
    scopes: &mut Scopes<B>,
    name: Symbol,
    span: Span,
    binding: B,
    display_name: &str,
    source: &hollowscript_core::Source,
    diagnostics: &mut BoundedDiagnostics,
) -> bool {
    if let Some(first) = scopes.first_declaration(name) {
        diagnostics.push(
            Diagnostic::new(Code::HS0201, &[("name", &display_name)])
                .with_location(source.locate(span))
                .with_related(source.locate(first), "first declared here"),
        );
        return false;
    }
    scopes.declare(ScopeEntry {
        name,
        binding,
        span,
    });
    true
}

impl FileScope {
    /// An empty scope with one module-level level already open in each
    /// namespace, ready for [`resolve_file`] to declare into.
    #[must_use]
    fn new() -> FileScope {
        let mut values = Scopes::new();
        values.push();
        let mut types = Scopes::new();
        types.push();
        FileScope {
            values,
            types,
            records: Vec::new(),
            enums: Vec::new(),
            functions: Vec::new(),
            diagnostics: BoundedDiagnostics::new(),
        }
    }
}

/// The first pass over one already-parsed file (sections 4.6, 4.10, 9.2 and
/// 10.2). `module` is this file's index into the program's module list, used
/// to build the [`DeclId`] of every `record` and `enum` it declares.
/// `next_function_id` is the program's running [`crate::hir::FunctionId`]
/// counter, shared across every module so that a function's identity is
/// unique across the whole program, not just its own file.
/// `capabilities` is [`crate::effects::check_uses`]'s own result for this
/// file's `uses` lines; its diagnostics and entries are folded in here,
/// which is what makes a capability name unshadowable by anything else in
/// the file (section 9.2): capabilities are declared first, before imports
/// or any top-level item, so every later declaration in this pass sees them
/// already occupying the value scope.
#[must_use]
pub fn resolve_file(
    file: &hollowscript_syntax::ast::File,
    module: u32,
    imports: &[ResolvedImport],
    capabilities: crate::effects::UsesResult,
    next_function_id: &mut u32,
    names: &hollowscript_core::Interner,
    source: &hollowscript_core::Source,
) -> FileScope {
    let mut scope = FileScope::new();
    scope.diagnostics.extend(capabilities.diagnostics);
    for entry in capabilities.entries {
        let text = names.resolve(entry.name).to_owned();
        // An ungranted name (HS0404, already reported by `check_uses`) still
        // occupies the value scope, with an id past the end of the run's
        // real capabilities: a lookup against the capability types the
        // driver built for the run finds nothing there and substitutes the
        // error type, the same fallback used everywhere else in section 2.11
        // for a name that failed to resolve.
        declare_or_report(
            &mut scope.values,
            entry.name,
            entry.span,
            ValueBinding::Capability(crate::hir::CapabilityId(
                entry.capability.map_or(u32::MAX, |c| c.0),
            )),
            &text,
            source,
            &mut scope.diagnostics,
        );
    }
    for import in imports {
        let text = names.resolve(import.name.name).to_owned();
        if import.value.is_none() && import.ty.is_none() {
            declare_or_report(
                &mut scope.values,
                import.name.name,
                import.name.span,
                ValueBinding::Unresolved,
                &text,
                source,
                &mut scope.diagnostics,
            );
            declare_or_report(
                &mut scope.types,
                import.name.name,
                import.name.span,
                TypeBinding::Unresolved,
                &text,
                source,
                &mut scope.diagnostics,
            );
            continue;
        }
        if let Some(value) = import.value {
            declare_or_report(
                &mut scope.values,
                import.name.name,
                import.name.span,
                value,
                &text,
                source,
                &mut scope.diagnostics,
            );
        }
        if let Some(ty) = import.ty {
            declare_or_report(
                &mut scope.types,
                import.name.name,
                import.name.span,
                ty,
                &text,
                source,
                &mut scope.diagnostics,
            );
        }
    }
    let mut next_decl_index = 0u32;
    for (index, item) in file.items.iter().enumerate() {
        match &item.kind {
            hollowscript_syntax::ast::ItemKind::Fn(decl) => {
                let text = names.resolve(decl.name.name).to_owned();
                let id = crate::hir::FunctionId(*next_function_id);
                if declare_or_report(
                    &mut scope.values,
                    decl.name.name,
                    decl.name.span,
                    ValueBinding::Function(id),
                    &text,
                    source,
                    &mut scope.diagnostics,
                ) {
                    *next_function_id += 1;
                    scope.functions.push((id, index));
                }
            }
            hollowscript_syntax::ast::ItemKind::Record(decl) => {
                let text = names.resolve(decl.name.name).to_owned();
                if is_protected_builtin_type_name(&text) {
                    scope.diagnostics.push(
                        Diagnostic::new(Code::HS0201, &[("name", &text)])
                            .with_location(source.locate(decl.name.span)),
                    );
                } else {
                    let decl_id = DeclId {
                        module,
                        index: next_decl_index,
                    };
                    if declare_or_report(
                        &mut scope.types,
                        decl.name.name,
                        decl.name.span,
                        TypeBinding::Named(decl_id),
                        &text,
                        source,
                        &mut scope.diagnostics,
                    ) {
                        next_decl_index += 1;
                        scope.records.push((decl_id, index));
                    }
                }
                check_type_param_names(&decl.type_params, names, source, &mut scope.diagnostics);
            }
            hollowscript_syntax::ast::ItemKind::Enum(decl) => {
                let text = names.resolve(decl.name.name).to_owned();
                if is_protected_builtin_type_name(&text) {
                    scope.diagnostics.push(
                        Diagnostic::new(Code::HS0201, &[("name", &text)])
                            .with_location(source.locate(decl.name.span)),
                    );
                } else {
                    let decl_id = DeclId {
                        module,
                        index: next_decl_index,
                    };
                    let mut declared = declare_or_report(
                        &mut scope.types,
                        decl.name.name,
                        decl.name.span,
                        TypeBinding::Named(decl_id),
                        &text,
                        source,
                        &mut scope.diagnostics,
                    );
                    // An enum name is in both namespaces (section 2.1): it
                    // is also the namespace of its variant constructors.
                    if !declare_or_report(
                        &mut scope.values,
                        decl.name.name,
                        decl.name.span,
                        ValueBinding::EnumNamespace(decl_id),
                        &text,
                        source,
                        &mut scope.diagnostics,
                    ) {
                        declared = false;
                    }
                    if declared {
                        next_decl_index += 1;
                        scope.enums.push((decl_id, index));
                    }
                }
                check_type_param_names(&decl.type_params, names, source, &mut scope.diagnostics);
            }
            hollowscript_syntax::ast::ItemKind::Constant(_)
            | hollowscript_syntax::ast::ItemKind::Variable(_)
            | hollowscript_syntax::ast::ItemKind::Stmt(_) => {
                // Declared later, in source order, by the driver's next pass
                // (section 4.10): a top-level constant or variable is not
                // visible to code above it, only to every function body.
            }
        }
    }
    scope
}

/// HS0212: a `<...>` type parameter list may not repeat a name.
/// The prelude type names of section 2.1 that a program may never declare,
/// even though every other prelude name may be hidden: "`Int`, `Float`,
/// `Bool`, `String`, `Filter`, `Result`, `Map`, `Set` and `MapEntry` are
/// always the built-in types, and declaring a type of one of those names is
/// HS0201."
fn is_protected_builtin_type_name(text: &str) -> bool {
    [
        "Int", "Float", "Bool", "String", "Filter", "Result", "Map", "Set", "MapEntry",
    ]
    .contains(&text)
}

fn check_type_param_names(
    type_params: &[hollowscript_syntax::ast::Ident],
    names: &hollowscript_core::Interner,
    source: &hollowscript_core::Source,
    diagnostics: &mut BoundedDiagnostics,
) {
    let mut seen: Vec<Symbol> = Vec::new();
    for param in type_params {
        if seen.contains(&param.name) {
            diagnostics.push(
                Diagnostic::new(Code::HS0212, &[("name", &names.resolve(param.name))])
                    .with_location(source.locate(param.span)),
            );
        } else {
            seen.push(param.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ScopeEntry, Scopes, TypeBinding, ValueBinding, resolve_file};
    use hollowscript_core::{Interner, Span};

    #[test]
    fn inner_scopes_shadow_and_popping_restores_the_outer_binding() {
        let mut names = Interner::new();
        let x = names.intern("x");
        let mut scopes: Scopes<ValueBinding> = Scopes::new();
        scopes.push();
        scopes.declare(ScopeEntry {
            name: x,
            binding: ValueBinding::Local(crate::hir::LocalId(0)),
            span: Span::new(0, 1),
        });
        scopes.push();
        scopes.declare(ScopeEntry {
            name: x,
            binding: ValueBinding::Local(crate::hir::LocalId(1)),
            span: Span::new(10, 11),
        });
        assert_eq!(
            scopes.lookup(x),
            Some(ValueBinding::Local(crate::hir::LocalId(1)))
        );
        assert_eq!(
            scopes.first_declaration(x),
            Some(Span::new(0, 1)),
            "the outer declaration is the related span"
        );
        scopes.pop();
        assert_eq!(
            scopes.lookup(x),
            Some(ValueBinding::Local(crate::hir::LocalId(0)))
        );
        scopes.pop();
        assert_eq!(scopes.lookup(x), None);
    }

    #[test]
    fn sibling_scopes_do_not_see_each_other() {
        let mut names = Interner::new();
        let x = names.intern("x");
        let mut scopes: Scopes<ValueBinding> = Scopes::new();
        scopes.push();
        scopes.push();
        scopes.declare(ScopeEntry {
            name: x,
            binding: ValueBinding::Local(crate::hir::LocalId(0)),
            span: Span::new(0, 1),
        });
        scopes.pop();
        scopes.push();
        assert!(!scopes.is_visible(x));
    }

    fn dir(segments: &[&str]) -> Vec<String> {
        segments.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn import_paths_resolve_relative_to_the_importing_modules_directory() {
        use super::resolve_import_path;
        // from app/main.hws
        let from = dir(&["app"]);
        assert_eq!(
            resolve_import_path(&from, "./util"),
            Ok(Ok(dir(&["app", "util.hws"])))
        );
        assert_eq!(
            resolve_import_path(&from, "../shared/util"),
            Ok(Ok(dir(&["shared", "util.hws"])))
        );
        assert_eq!(
            resolve_import_path(&from, "./sub/../util"),
            Ok(Ok(dir(&["app", "util.hws"])))
        );
        // from main.hws at the root
        assert_eq!(resolve_import_path(&[], "../secret"), Ok(Err(())));
    }

    #[test]
    fn a_bare_name_or_a_written_extension_is_rejected() {
        use super::{ImportPathError, resolve_import_path};
        assert_eq!(
            resolve_import_path(&[], "util"),
            Err(ImportPathError::NotRelative)
        );
        assert_eq!(
            resolve_import_path(&[], "./util.hws"),
            Err(ImportPathError::HasExtension)
        );
        assert_eq!(
            resolve_import_path(&[], "./a//b"),
            Err(ImportPathError::EmptySegment)
        );
        assert_eq!(
            resolve_import_path(&[], "./con"),
            Err(ImportPathError::ReservedName)
        );
    }

    fn parse(source: &str) -> (hollowscript_syntax::ast::File, Interner) {
        let mut names = Interner::new();
        let src =
            hollowscript_core::Source::new(hollowscript_core::SourceId(0), "main.hws", source)
                .unwrap();
        let mut control = hollowscript_core::Control::none();
        let parsed = hollowscript_syntax::parse(&src, &mut names, &mut control);
        assert!(
            parsed.diagnostics.is_empty(),
            "test source must parse cleanly: {:?}",
            parsed.diagnostics
        );
        (parsed.file, names)
    }

    fn empty_uses() -> crate::effects::UsesResult {
        crate::effects::UsesResult::default()
    }

    #[test]
    fn a_file_declares_its_own_functions_records_and_enums() {
        let (file, names) = parse(
            "fn double(n: Int): Int {\n    return n * 2\n}\n\nrecord Point = { x: Int, y: Int }\n\nenum Light {\n    Red,\n    Green,\n}\n",
        );
        let source =
            hollowscript_core::Source::new(hollowscript_core::SourceId(0), "main.hws", "").unwrap();
        let mut next_fn = 0u32;
        let scope = resolve_file(&file, 0, &[], empty_uses(), &mut next_fn, &names, &source);
        assert!(scope.diagnostics.is_empty());
        assert_eq!(scope.functions.len(), 1);
        assert_eq!(scope.records.len(), 1);
        assert_eq!(scope.enums.len(), 1);
        assert_eq!(next_fn, 1);
        let double = names.get("double").unwrap();
        assert!(matches!(
            scope.values.lookup(double),
            Some(ValueBinding::Function(crate::hir::FunctionId(0)))
        ));
        let point = names.get("Point").unwrap();
        assert!(matches!(
            scope.types.lookup(point),
            Some(TypeBinding::Named(_))
        ));
        let light = names.get("Light").unwrap();
        // An enum name is in both namespaces (section 2.1).
        assert!(scope.types.lookup(light).is_some());
        assert!(matches!(
            scope.values.lookup(light),
            Some(ValueBinding::EnumNamespace(_))
        ));
    }

    #[test]
    fn a_second_top_level_declaration_of_the_same_name_is_hs0201() {
        let (file, names) = parse("fn f(): Int {\n    return 1\n}\n\nrecord f = { x: Int }\n");
        let source =
            hollowscript_core::Source::new(hollowscript_core::SourceId(0), "main.hws", "").unwrap();
        let mut next_fn = 0u32;
        // "f" is a function name (value namespace) and "f" the record is a
        // type name (type namespace); they do not clash with each other.
        let scope = resolve_file(&file, 0, &[], empty_uses(), &mut next_fn, &names, &source);
        assert!(scope.diagnostics.is_empty());

        let (file, names) =
            parse("fn f(): Int {\n    return 1\n}\n\nfn f(): Int {\n    return 2\n}\n");
        let mut next_fn = 0u32;
        let scope = resolve_file(&file, 0, &[], empty_uses(), &mut next_fn, &names, &source);
        assert_eq!(scope.diagnostics.len(), 1);
        assert_eq!(
            scope.diagnostics.iter().next().map(|d| d.code).unwrap(),
            hollowscript_core::Code::HS0201
        );
        assert_eq!(
            scope.functions.len(),
            1,
            "the clashing declaration is not given an id"
        );
    }

    #[test]
    fn a_repeated_type_parameter_name_is_hs0212() {
        let (file, names) = parse("record Pair<A, A> = { left: A, right: A }\n");
        let source =
            hollowscript_core::Source::new(hollowscript_core::SourceId(0), "main.hws", "").unwrap();
        let mut next_fn = 0u32;
        let scope = resolve_file(&file, 0, &[], empty_uses(), &mut next_fn, &names, &source);
        assert_eq!(scope.diagnostics.len(), 1);
        assert_eq!(
            scope.diagnostics.iter().next().map(|d| d.code).unwrap(),
            hollowscript_core::Code::HS0212
        );
    }

    #[test]
    fn a_record_or_enum_cannot_be_named_after_a_builtin_type() {
        let (file, names) = parse("record Int = { x: Int }\n");
        let source =
            hollowscript_core::Source::new(hollowscript_core::SourceId(0), "main.hws", "").unwrap();
        let mut next_fn = 0u32;
        let scope = resolve_file(&file, 0, &[], empty_uses(), &mut next_fn, &names, &source);
        assert_eq!(scope.diagnostics.len(), 1);
        assert_eq!(
            scope.diagnostics.iter().next().map(|d| d.code).unwrap(),
            hollowscript_core::Code::HS0201
        );
        assert!(scope.records.is_empty());
    }
}
