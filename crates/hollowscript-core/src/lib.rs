//! Shared vocabulary of the HollowScript implementation.
//!
//! Every other crate of the workspace depends on this one, and it depends on
//! nothing but the standard library. It holds the definitions that the front
//! end, the compiler and the VM must agree on and that none of them may own:
//!
//! - [`source`]: source text, spans, and the mapping from offsets to lines and columns;
//! - [`diagnostic`] and [`codes`]: the diagnostic record, the registry of every
//!   code of chapter 17, and the canonical ordering of diagnostics;
//! - [`limits`]: the resource limits of chapter 11 and the structural limits of chapter 1;
//! - [`fault`]: faults and host errors, typed by code;
//! - [`load`]: why a host's module loader could not produce a module,
//!   shared by every interface that carries that outcome;
//! - [`cancel`]: cancellation tokens, the yield callback and the safepoint rule;
//! - [`symbol`]: interned identifiers;
//! - [`transfer`]: descriptions of the types that cross the host boundary;
//! - [`native`]: the roster of the standard library, shared by the checker, the compiler and the VM;
//! - [`filter`]: the filter language of Appendix A, which hosts use on their own;
//! - [`stack`]: the dedicated large-stack thread every unbounded-recursion pass
//!   (parsing, formatting, checking, compiling) runs on, per section 1.10.
//!
//! The crate contains no parser, no checker and no interpreter. That is what
//! lets the browser VM depend on it without depending on the front end.

#![forbid(unsafe_code)]

pub mod cancel;
pub mod codes;
pub mod diagnostic;
pub mod fault;
pub mod filter;
pub mod limits;
pub mod load;
pub mod native;
pub mod source;
pub mod stack;
pub mod symbol;
pub mod transfer;

pub use cancel::{CancellationToken, Cancelled, Control, Yield, YieldDecision};
pub use codes::{Code, CodeInfo, Family, Severity, Stage};
pub use diagnostic::{
    BoundedDiagnostics, Diagnostic, DiagnosticSet, Frame, Location, Related, cap_diagnostics,
};
pub use fault::{Fault, FaultClass, FaultCode, HostError, HostErrorCode};
pub use limits::{LimitData, LimitOption, Limits};
pub use load::LoadModuleError;
pub use source::{
    FileName, LineCol, LineIndex, PositionCursor, Source, SourceError, SourceId, SourceMap, Span,
};
pub use stack::{DEDICATED_STACK_SIZE, DedicatedStackError, run_on_dedicated_stack};
pub use symbol::{Interner, Symbol};
