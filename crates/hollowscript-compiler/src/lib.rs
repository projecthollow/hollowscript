//! The HollowScript compiler.
//!
//! Turns the typed HIR of [`hollowscript_check`] into a
//! [`hollowscript_bytecode::Module`], which the caller then passes to
//! [`hollowscript_bytecode::verify`](hollowscript_bytecode::verify::verify) before it reaches a VM. This crate never
//! constructs a [`hollowscript_bytecode::verify::VerifiedModule`] itself: only
//! the verifier does, so that a defect in code generation is caught the same
//! way a hand-built or corrupted container would be.
//!
//! # Reading the HIR without a real checker yet
//!
//! The checker's name resolution and inference passes are not written yet
//! (`hollowscript-check`'s own `resolve`/`infer` modules), so this crate is
//! developed and tested against hand-built [`hollowscript_check::hir`] values
//! rather than programs `hollowscript_check::check` actually produced. The
//! HIR shape itself is fixed and is what this crate compiles against; see
//! `tests/fixtures.rs` for a value of every construct the HIR defines.
//!
//! # A note on `symbols`
//!
//! [`hollowscript_check::hir::Program`] names every declaration with a
//! [`Symbol`](hollowscript_core::Symbol), but keeps no [`Interner`] of its
//! own to resolve one to text (the interner belongs to the front end,
//! alongside the source it read names from). Building the bytecode's string
//! table needs that resolution, so [`compile`] takes the run's interner
//! alongside the program, which `hollowscript::instance::load` already had
//! in hand but was not yet threading through. See this crate's `lower`
//! module (private, but its doc comment is worth reading in the source) for
//! the other gaps found while writing this compiler.

#![forbid(unsafe_code)]

mod lower;
mod transfer;

use hollowscript_bytecode::module::Module;
use hollowscript_check::hir::Program;
use hollowscript_core::{Diagnostic, Interner, Location};

/// Options that shape a compilation.
#[derive(Copy, Clone, Debug, Default)]
pub struct Options {
    /// Whether to keep source positions in the output (`SpanEntry`), needed
    /// for fault frames and a debugger; a smaller build for the browser VM
    /// omits them with [`hollowscript_bytecode::Module::stripped`] instead,
    /// after compiling, so this option only controls whether the compiler
    /// does the (cheap) work of recording them as it goes.
    ///
    /// Currently has no effect: the compiler always records source
    /// positions, from the node spans the checker hands it with the HIR,
    /// because a fault is located from them (section 19.3). A host that
    /// wants a smaller module strips them afterwards with
    /// [`hollowscript_bytecode::Module::stripped`].
    pub debug_info: bool,
}

/// A checked program could not be compiled.
///
/// A correct compiler returns one of these for a program a real checker
/// accepted only when the program exceeds one of this implementation's own
/// limits, which section 1.10 allows an implementation to impose: a single
/// function that needs more than
/// [`hollowscript_bytecode::module::MAX_REGISTERS`] registers at once (a call
/// or a record literal with tens of thousands of values, or as many locals),
/// or one whose code is too long for a jump to cross. Each is HS0901,
/// located at the construct that exceeded the limit. Every other variant is
/// a defect: a construct the checker produced that this compiler does not
/// understand. Compilation never panics; every such case is reported
/// through this type instead.
#[derive(Debug)]
pub enum CompileError {
    /// The compiler cannot express this program, with the reason in the
    /// diagnostic's message and, where it is known, the construct that
    /// caused it as the diagnostic's location.
    Unsupported(Box<Diagnostic>),
}

impl CompileError {
    /// The error as a diagnostic.
    #[must_use]
    pub fn into_diagnostic(self) -> Diagnostic {
        match self {
            CompileError::Unsupported(d) => *d,
        }
    }

    /// A construct this compiler does not build: a defect, not a property
    /// of the program.
    pub(crate) fn unsupported(what: impl Into<String>) -> CompileError {
        CompileError::Unsupported(Box::new(Diagnostic::not_implemented(&what.into())))
    }

    /// A program that exceeds a limit of this implementation (HS0901,
    /// section 1.10); `limit` says what the limit is.
    pub(crate) fn limit(limit: impl Into<String>) -> CompileError {
        let limit = limit.into();
        CompileError::Unsupported(Box::new(Diagnostic::new(
            hollowscript_core::Code::HS0901,
            &[("limit", &limit)],
        )))
    }

    /// Gives the error `location` unless it already has one: the innermost
    /// construct being compiled when it was raised locates it best, and it
    /// is the first to ask.
    pub(crate) fn located(self, location: impl FnOnce() -> Option<Location>) -> CompileError {
        let CompileError::Unsupported(mut diagnostic) = self;
        if diagnostic.location.is_none() {
            diagnostic.location = location();
        }
        CompileError::Unsupported(diagnostic)
    }
}

/// Compiles a checked program to bytecode. The result has not been verified;
/// pass it to [`hollowscript_bytecode::verify::verify`] before it reaches a VM.
///
/// `symbols` resolves every [`hollowscript_core::Symbol`] the program names
/// (functions, globals, parameters, capabilities) to the text the bytecode's
/// string table stores; it must be the same interner `hollowscript_syntax`
/// and `hollowscript_check` used to produce `program`.
///
/// # Errors
///
/// See [`CompileError`].
pub fn compile(
    program: &Program,
    symbols: &Interner,
    options: Options,
) -> Result<Module, CompileError> {
    lower::compile_program(program, symbols, options)
}
