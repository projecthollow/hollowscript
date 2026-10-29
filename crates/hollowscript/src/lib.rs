//! The HollowScript embedding facade.
//!
//! This is the crate most Rust hosts depend on directly: [`check`] and
//! [`load`] are the host operations of section 12.1, [`Instance`] is the
//! state machine of section 12.2 with `call`, `read`, `cancel` and
//! `dispose`, and [`Options`] bundles the resource limits and capability
//! interface a run needs (section 9.7). It depends on every other
//! implementation crate and adds no bytecode or VM concepts of its own:
//! it orchestrates the front end, the compiler, the verifier and the VM in
//! the order chapter 12 specifies and never lets a host observe a state the
//! specification does not allow, which is what [`crate::instance`]'s tests check.
//!
//! The front end, the checker, the compiler and the verifier
//! ([`hollowscript_syntax`], [`hollowscript_check`], [`hollowscript_compiler`],
//! [`hollowscript_bytecode`]) are wired together here in the order chapter 12
//! specifies: a program with an error at the check stage makes [`load`]
//! return [`LoadOutcome::CheckFailed`] with no instance created, exactly as
//! section 12.3 requires; a clean program is compiled, verified and handed to
//! the interpreter, which some native calls and imports still leave
//! incomplete underneath this facade.
//!
//! [`format()`] is separate from [`check`] and [`load`]: it needs only the
//! front end, not the checker, so it works the same whether or not a program
//! would check cleanly.

#![forbid(unsafe_code)]

pub mod formatting;
mod hosttypes;
pub mod instance;
pub mod options;

pub use formatting::format;
pub use hollowscript_check::effects::{CapabilityDecl, CapabilityShape};
pub use hollowscript_check::{CapabilityInterface, FilterLiteral};
pub use hollowscript_core::fault::{Fault, FaultClass, FaultCode, HostError, HostErrorCode};
pub use hollowscript_core::{
    CancellationToken, Code, Control, Diagnostic, Limits, Severity, Yield, YieldDecision,
};
pub use hollowscript_vm::hostvalues::{ERR, OK, RESULT};
/// The language's own JSON reader and writer (section 15.1), re-exported
/// so that a host which must read JSON of its own does not need a second
/// one whose behaviour differs from the language's.
pub use hollowscript_vm::json;
pub use hollowscript_vm::{
    CapabilityTable, Command, CommandBuffer, CommandResult, Heap, HostCall, HostFn, HostTypes,
    MapEntry, MapKey, Object, Run, RunCounters, Value,
};
pub use instance::{
    BuildOutcome, BytecodeError, CallOutcome, CapabilityBinder, Export, Instance, LoadOutcome,
    LoadResult, State, build, check, filter_literals, is_bytecode, load, load_bytecode,
    parse_diagnostics,
};
pub use options::Loader;
pub use options::{LoaderError, NoLoader, Options};
