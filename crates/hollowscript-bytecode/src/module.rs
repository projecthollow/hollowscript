//! The in-memory form of a compiled program.
//!
//! A [`Module`] is everything the VM needs and nothing it has to parse from
//! source: the functions, the tables they refer to, the capability interfaces
//! the program declared, the modules in initialisation order and the exports
//! that a host can call. Names are indexes into one string table, so equal
//! strings are stored once.

use hollowscript_core::transfer::{TypeRef, TypeTable};

/// The most registers in one frame. An instruction names the first 256
/// directly and the rest through an [`crate::opcode::Op::Ext`] prefix, so the
/// limit is what `register_count` holds, not what one operand byte reaches.
pub const MAX_REGISTERS: usize = 65_535;

/// An index into [`Module::strings`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StringId(pub u32);

/// [`Module::add_string`] was asked to add a string once [`Module::strings`]
/// already holds `u32::MAX` of them, so the new one cannot be given a
/// [`StringId`] that fits. Reported rather than silently clamped: a clamped
/// id would alias a fresh string onto whatever id the clamp lands on,
/// exactly the failure mode `hollowscript-vm`'s index-based arena avoids the
/// same way for its own handles.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StringTableFull;

impl std::fmt::Display for StringTableFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the module's string table is exhausted")
    }
}

impl std::error::Error for StringTableFull {}

/// A constant of one function.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    /// An `Int`.
    Int(i64),
    /// A finite `Float`. Negative zero is not a constant.
    Float(f64),
    /// A `String`.
    Str(StringId),
    /// The source of a filter literal, already validated by the checker.
    Filter(StringId),
}

/// Where a closure takes one of its upvalues from when it is created.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    /// The cell in this register of the creating frame.
    Register(u16),
    /// An upvalue of the creating closure.
    Upvalue(u16),
}

/// What a function is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    /// A top-level `fn`.
    Function,
    /// A closure literal.
    Closure,
    /// The top-level statements of one module.
    ModuleInit,
}

/// A position of the source, kept for faults. It maps the instruction at `pc`,
/// and every following one up to the next entry, to a span of a module.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SpanEntry {
    /// The first instruction the entry covers.
    pub pc: u32,
    /// The index into [`Module::modules`] of the module the span is in.
    pub module: u32,
    /// The byte offset where the span starts.
    pub start: u32,
    /// The byte offset where the span ends.
    pub end: u32,
    /// The 1-based line of `start`.
    pub line: u32,
    /// The 1-based column of `start`.
    pub column: u32,
}

/// A function or closure.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// The name, or `<closure>`.
    pub name: StringId,
    /// What kind of function it is.
    pub kind: FunctionKind,
    /// The number of parameters, which are in registers 0 to `arity - 1`.
    pub arity: u16,
    /// The size of the frame, from 1 to [`MAX_REGISTERS`].
    pub register_count: u16,
    /// Whether the function returns a value.
    pub returns_value: bool,
    /// The upvalues, in order, of a closure. Empty for other functions.
    pub captures: Vec<CaptureSource>,
    /// The constants.
    pub constants: Vec<Constant>,
    /// The instructions, one word each.
    pub code: Vec<u32>,
    /// Source positions, sorted by `pc`. Empty in a stripped module.
    pub spans: Vec<SpanEntry>,
}

/// The fields of a record type, sorted by name. A record value keeps its
/// fields in this order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// The field names.
    pub fields: Vec<StringId>,
}

/// A variant of an enum, as the instructions that build and test enum values
/// refer to it.
///
/// `enum_name` and `name` are what a fault message or a host reading this
/// variant by name (`hollowscript_vm::HostTypes`) uses; `decl_module` and
/// `decl_index` are what identifies the *type*: section 10.4 says "an enum
/// type is identified by its declaring module and name", so two entries with
/// the same `enum_name` are the same enum only if these also match
/// . `decl_module` is `hollowscript_core::transfer::HOST_MODULE` for
/// an enum a capability interface declares directly rather than a program
/// declaring with `enum`; otherwise it is the declaring module's index, the
/// same numbers `hollowscript_check::types::DeclId` carries for that
/// declaration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VariantDesc {
    /// The name of the enum.
    pub enum_name: StringId,
    /// The name of the variant.
    pub name: StringId,
    /// The position of the variant in its enum.
    pub index: u16,
    /// The number of payload values.
    pub arity: u16,
    /// The declaring module of the enum this variant belongs to (see above).
    pub decl_module: u32,
    /// The declaration's index within `decl_module` (see above).
    pub decl_index: u32,
}

/// A native library function that the module calls, by name. The VM resolves
/// each name when it loads the module and refuses a name it does not have or an
/// arity that differs from its own.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NativeImport {
    /// The name from the roster of the standard library, such as `list.map`.
    pub name: StringId,
    /// The number of arguments including the receiver of a method.
    pub arity: u8,
}

/// The types of a capability function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// The parameter types.
    pub params: Vec<TypeRef>,
    /// The result type, or `None` for a function that returns nothing.
    pub ret: Option<TypeRef>,
}

/// The shape of a capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityKind {
    /// A single function such as `print`.
    Function(Signature),
    /// A namespace of functions such as `clock`.
    Namespace(Vec<(StringId, Signature)>),
}

/// A capability that the program declared with `uses`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    /// The name as written in `uses`.
    pub name: StringId,
    /// Its interface.
    pub kind: CapabilityKind,
}

/// One capability function that the code calls: an entry of a capability and
/// one function of it. `CallCap` and `CapFn` refer to these.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CapCall {
    /// The index into [`Module::capabilities`].
    pub capability: u32,
    /// The index of the function within a namespace, and 0 for a single function.
    pub function: u32,
}

/// A source module of the program.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModuleEntry {
    /// The canonical path.
    pub path: StringId,
    /// The function that runs the top-level statements of the module.
    pub init: u32,
}

/// A top-level `constant` or `variable`, kept between calls.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Global {
    /// The index into [`Module::modules`] of the module that declares it.
    pub module: u32,
    /// The name.
    pub name: StringId,
    /// Whether it is a `variable`.
    pub mutable: bool,
}

/// What an export is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportKind {
    /// An `export fn` of the entry module, which a host can call.
    Function {
        /// The index into [`Module::functions`].
        function: u32,
        /// The transferable types of its parameters and result.
        signature: Signature,
    },
    /// An `export constant` of the entry module, which a host can read.
    Constant {
        /// The index into [`Module::globals`].
        global: u32,
        /// Its transferable type.
        ty: TypeRef,
    },
}

/// A name that the entry module exports to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// The name.
    pub name: StringId,
    /// What it is.
    pub kind: ExportKind,
}

/// A compiled program.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Module {
    /// The version of the language specification the program was compiled for,
    /// as major and minor.
    pub language: (u16, u16),
    /// Every string that the tables refer to.
    pub strings: Vec<String>,
    /// The transferable types of exports and capabilities.
    pub types: TypeTable,
    /// The functions.
    pub functions: Vec<Function>,
    /// The record shapes.
    pub shapes: Vec<Shape>,
    /// The enum variants.
    pub variants: Vec<VariantDesc>,
    /// The natives that the code calls.
    pub natives: Vec<NativeImport>,
    /// The capabilities that the program declared.
    pub capabilities: Vec<Capability>,
    /// The capability functions that the code calls.
    pub cap_calls: Vec<CapCall>,
    /// The source modules in initialisation order. The last is the entry module.
    pub modules: Vec<ModuleEntry>,
    /// The global slots.
    pub globals: Vec<Global>,
    /// The names the host can use.
    pub exports: Vec<Export>,
}

impl Module {
    /// An empty module for the language version 1.0.
    #[must_use]
    pub fn new() -> Module {
        Module {
            language: (1, 0),
            ..Module::default()
        }
    }

    /// Adds a string and returns its id. Equal strings are not merged here.
    ///
    /// # Errors
    ///
    /// [`StringTableFull`] if the table already holds `u32::MAX` strings.
    pub fn add_string(&mut self, text: impl Into<String>) -> Result<StringId, StringTableFull> {
        let id = u32::try_from(self.strings.len()).map_err(|_| StringTableFull)?;
        self.strings.push(text.into());
        Ok(StringId(id))
    }

    /// The text of a string id, or the empty string for an id that is out of range.
    #[must_use]
    pub fn string(&self, id: StringId) -> &str {
        self.strings.get(id.0 as usize).map_or("", String::as_str)
    }

    /// The index of the entry module, which is the last.
    #[must_use]
    pub fn entry(&self) -> Option<usize> {
        self.modules.len().checked_sub(1)
    }

    /// Whether the module carries source positions.
    #[must_use]
    pub fn has_debug_info(&self) -> bool {
        self.functions.iter().any(|f| !f.spans.is_empty())
    }

    /// The module with its source positions removed, for shipping.
    #[must_use]
    pub fn stripped(&self) -> Module {
        let mut copy = self.clone();
        for f in &mut copy.functions {
            f.spans.clear();
        }
        copy
    }
}

#[cfg(test)]
mod tests {
    use super::{Function, FunctionKind, Module, SpanEntry};

    #[test]
    fn strings_are_added_and_looked_up() {
        let mut m = Module::new();
        let a = m.add_string("alpha").unwrap();
        let b = m.add_string("beta").unwrap();
        assert_eq!((m.string(a), m.string(b)), ("alpha", "beta"));
        assert_eq!(m.string(super::StringId(99)), "");
        assert_eq!(m.language, (1, 0));
        assert_eq!(m.entry(), None);
    }

    #[test]
    fn stripping_removes_only_positions() {
        let mut m = Module::new();
        let name = m.add_string("f").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Function,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![0],
            spans: vec![SpanEntry {
                pc: 0,
                module: 0,
                start: 0,
                end: 1,
                line: 1,
                column: 1,
            }],
        });
        assert!(m.has_debug_info());
        let s = m.stripped();
        assert!(!s.has_debug_info());
        assert_eq!(s.functions[0].code, m.functions[0].code);
    }
}
