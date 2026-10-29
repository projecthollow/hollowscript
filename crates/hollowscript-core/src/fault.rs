//! Faults (section 8.1) and host errors (section 12.6).
//!
//! A fault aborts a run and cannot be caught by a script. A host error answers
//! a request that the host should not have made and leaves the instance as it
//! was. Both are typed here, so the VM and the bindings cannot invent a code.

use crate::diagnostic::{Code, Diagnostic, Frame, Location};
use crate::limits::{LimitData, LimitOption};
use std::fmt::{self, Display};

/// The classes of faults (section 8.1). They exist for hosts that report or
/// count faults differently and change nothing a script can do.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FaultClass {
    /// A limit of the language was reached.
    Resource,
    /// The call itself was wrong: a zero divisor, a negative repeat count.
    Domain,
    /// A capability misbehaved.
    Host,
    /// A defect of the implementation.
    Internal,
}

/// The codes a fault can have.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum FaultCode {
    /// HS1001: the step limit was exceeded.
    StepLimit,
    /// HS1002: the memory limit was exceeded by an allocation.
    MemoryLimit,
    /// HS1003: the retained memory limit was exceeded.
    RetainedLimit,
    /// HS1004: the output limit was exceeded.
    OutputLimit,
    /// HS1005: the call depth was exceeded.
    CallDepth,
    /// HS1006: the run was cancelled.
    Cancelled,
    /// HS1007: an `Int` operation overflowed.
    IntOverflow,
    /// HS1008: division by zero.
    DivisionByZero,
    /// HS1009: a `Float` result that is not a number.
    NotANumber,
    /// HS1010: a `Float` result that is infinite.
    Infinite,
    /// HS1011: an invalid argument to a library function.
    InvalidArgument,
    /// HS1012: a capability failed inside the host.
    CapabilityFailed,
    /// HS1013: a capability returned a value of the wrong type.
    CapabilityType,
    /// HS1014: a list pattern in a declaration or `for` did not fit the list.
    ListPattern,
    /// HS1015: a limit of the implementation, not of the language, was
    /// reached while running (section 1.10).
    ImplementationLimit,
    /// HS1099: an internal error in the implementation.
    Internal,
}

impl FaultCode {
    /// Every fault code.
    pub const ALL: [FaultCode; 16] = [
        FaultCode::StepLimit,
        FaultCode::MemoryLimit,
        FaultCode::RetainedLimit,
        FaultCode::OutputLimit,
        FaultCode::CallDepth,
        FaultCode::Cancelled,
        FaultCode::IntOverflow,
        FaultCode::DivisionByZero,
        FaultCode::NotANumber,
        FaultCode::Infinite,
        FaultCode::InvalidArgument,
        FaultCode::CapabilityFailed,
        FaultCode::CapabilityType,
        FaultCode::ListPattern,
        FaultCode::ImplementationLimit,
        FaultCode::Internal,
    ];

    /// The registry code.
    #[must_use]
    pub const fn code(self) -> Code {
        match self {
            FaultCode::StepLimit => Code::HS1001,
            FaultCode::MemoryLimit => Code::HS1002,
            FaultCode::RetainedLimit => Code::HS1003,
            FaultCode::OutputLimit => Code::HS1004,
            FaultCode::CallDepth => Code::HS1005,
            FaultCode::Cancelled => Code::HS1006,
            FaultCode::IntOverflow => Code::HS1007,
            FaultCode::DivisionByZero => Code::HS1008,
            FaultCode::NotANumber => Code::HS1009,
            FaultCode::Infinite => Code::HS1010,
            FaultCode::InvalidArgument => Code::HS1011,
            FaultCode::CapabilityFailed => Code::HS1012,
            FaultCode::CapabilityType => Code::HS1013,
            FaultCode::ListPattern => Code::HS1014,
            FaultCode::ImplementationLimit => Code::HS1015,
            FaultCode::Internal => Code::HS1099,
        }
    }

    /// The fault with this registry code, if it is one.
    #[must_use]
    pub fn from_code(code: Code) -> Option<FaultCode> {
        FaultCode::ALL.into_iter().find(|f| f.code() == code)
    }

    /// The class of the fault.
    #[must_use]
    pub const fn class(self) -> FaultClass {
        match self {
            FaultCode::StepLimit
            | FaultCode::MemoryLimit
            | FaultCode::RetainedLimit
            | FaultCode::OutputLimit
            | FaultCode::CallDepth
            | FaultCode::Cancelled
            | FaultCode::IntOverflow
            | FaultCode::ImplementationLimit => FaultClass::Resource,
            FaultCode::DivisionByZero
            | FaultCode::NotANumber
            | FaultCode::Infinite
            | FaultCode::InvalidArgument
            | FaultCode::ListPattern => FaultClass::Domain,
            FaultCode::CapabilityFailed | FaultCode::CapabilityType => FaultClass::Host,
            FaultCode::Internal => FaultClass::Internal,
        }
    }
}

/// A fault that ended a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// Which fault.
    pub code: FaultCode,
    /// The message.
    pub message: String,
    /// The operation that faulted, when known.
    pub location: Option<Location>,
    /// The innermost frames of the call stack, at most 32.
    pub frames: Vec<Frame>,
    /// The option, limit and counter of a limit fault.
    pub data: Option<LimitData>,
}

impl Fault {
    /// A fault with the message of its template filled in from `args`.
    #[must_use]
    pub fn new(code: FaultCode, args: &[(&str, &dyn Display)]) -> Fault {
        let d = Diagnostic::new(code.code(), args);
        Fault {
            code,
            message: d.message,
            location: None,
            frames: Vec::new(),
            data: None,
        }
    }

    fn limit(
        code: FaultCode,
        option: LimitOption,
        limit: u64,
        used: u64,
        args: &[(&str, &dyn Display)],
    ) -> Fault {
        let mut fault = Fault::new(code, args);
        fault.data = Some(LimitData {
            option,
            limit,
            used,
        });
        fault
    }

    /// HS1001. `used` is the counter value before the charge that failed.
    #[must_use]
    pub fn step_limit(limit: u64, used: u64) -> Fault {
        Fault::limit(
            FaultCode::StepLimit,
            LimitOption::MaxSteps,
            limit,
            used,
            &[("limit", &limit)],
        )
    }

    /// HS1002. `used` is the allocation counter before the allocation and
    /// `size` the bytes that were requested.
    #[must_use]
    pub fn memory_limit(limit: u64, used: u64, size: u64) -> Fault {
        let args: [(&str, &dyn Display); 2] = [("limit", &limit), ("size", &size)];
        Fault::limit(
            FaultCode::MemoryLimit,
            LimitOption::MaxMemoryBytes,
            limit,
            used,
            &args,
        )
    }

    /// HS1003. `size` is the retained size.
    #[must_use]
    pub fn retained_limit(limit: u64, size: u64) -> Fault {
        let args: [(&str, &dyn Display); 2] = [("limit", &limit), ("size", &size)];
        Fault::limit(
            FaultCode::RetainedLimit,
            LimitOption::MaxMemoryBytes,
            limit,
            size,
            &args,
        )
    }

    /// HS1004. `used` is the number of bytes written so far.
    #[must_use]
    pub fn output_limit(limit: u64, used: u64) -> Fault {
        Fault::limit(
            FaultCode::OutputLimit,
            LimitOption::MaxOutputBytes,
            limit,
            used,
            &[("limit", &limit)],
        )
    }

    /// HS1015: `limit` names the limit of this implementation that was
    /// reached, in words, and says what it is.
    #[must_use]
    pub fn implementation_limit(limit: &str) -> Fault {
        Fault::new(FaultCode::ImplementationLimit, &[("limit", &limit)])
    }

    /// HS1005. `used` is the depth of active calls.
    #[must_use]
    pub fn call_depth(limit: u64, used: u64) -> Fault {
        Fault::limit(
            FaultCode::CallDepth,
            LimitOption::MaxCallDepth,
            limit,
            used,
            &[("limit", &limit)],
        )
    }

    /// HS1006.
    #[must_use]
    pub fn cancelled() -> Fault {
        Fault::new(FaultCode::Cancelled, &[])
    }

    /// HS1011: `function` was called with an argument outside its domain.
    #[must_use]
    pub fn invalid_argument(function: &str, reason: &str) -> Fault {
        Fault::new(
            FaultCode::InvalidArgument,
            &[("function", &function), ("reason", &reason)],
        )
    }

    /// HS1099: a defect of the implementation, which is never the script's fault.
    #[must_use]
    pub fn internal(message: &str) -> Fault {
        Fault::new(FaultCode::Internal, &[("message", &message)])
    }

    /// Sets the location of the operation that faulted.
    #[must_use]
    pub fn at(mut self, location: Location) -> Fault {
        self.location = Some(location);
        self
    }

    /// Sets the frames, keeping the innermost 32.
    #[must_use]
    pub fn with_frames(mut self, mut frames: Vec<Frame>) -> Fault {
        frames.truncate(crate::limits::structural::MAX_FRAMES);
        self.frames = frames;
        self
    }

    /// The class of this fault.
    #[must_use]
    pub fn class(&self) -> FaultClass {
        self.code.class()
    }

    /// The fault as a diagnostic of severity error.
    #[must_use]
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic {
            code: self.code.code(),
            severity: self.code.code().severity(),
            message: self.message.clone(),
            location: self.location.clone(),
            related: Vec::new(),
            frames: self.frames.clone(),
            data: self.data,
        }
    }
}

impl Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.message)
    }
}

impl std::error::Error for Fault {}

/// The codes of host errors (section 12.6). They are not source diagnostics.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HostErrorCode {
    /// HS1201: no such exported function or constant. The instance is unchanged.
    UnknownExport,
    /// HS1202: wrong number or types of arguments. The instance is unchanged.
    BadArguments,
    /// HS1203: the instance is faulted and cannot run again.
    Faulted,
    /// HS1204: an operation other than `cancel` while a load or call is in progress.
    Busy,
    /// HS1205: `call` or `read` on a finished instance, or any operation on a disposed one.
    BadState,
}

impl HostErrorCode {
    /// Every host error code.
    pub const ALL: [HostErrorCode; 5] = [
        HostErrorCode::UnknownExport,
        HostErrorCode::BadArguments,
        HostErrorCode::Faulted,
        HostErrorCode::Busy,
        HostErrorCode::BadState,
    ];

    /// The registry code.
    #[must_use]
    pub const fn code(self) -> Code {
        match self {
            HostErrorCode::UnknownExport => Code::HS1201,
            HostErrorCode::BadArguments => Code::HS1202,
            HostErrorCode::Faulted => Code::HS1203,
            HostErrorCode::Busy => Code::HS1204,
            HostErrorCode::BadState => Code::HS1205,
        }
    }
}

/// An error in a request that a host made to an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    /// Which error.
    pub code: HostErrorCode,
    /// The message.
    pub message: String,
}

impl HostError {
    /// A host error with the message of its template filled in from `args`.
    #[must_use]
    pub fn new(code: HostErrorCode, args: &[(&str, &dyn Display)]) -> HostError {
        HostError {
            code,
            message: Diagnostic::new(code.code(), args).message,
        }
    }

    /// The error as a diagnostic without a location.
    #[must_use]
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic {
            code: self.code.code(),
            severity: self.code.code().severity(),
            message: self.message.clone(),
            location: None,
            related: Vec::new(),
            frames: Vec::new(),
            data: None,
        }
    }
}

impl Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.message)
    }
}

impl std::error::Error for HostError {}

#[cfg(test)]
mod tests {
    use super::{Fault, FaultClass, FaultCode, HostError, HostErrorCode};
    use crate::diagnostic::{Family, all};
    use crate::limits::LimitOption;

    #[test]
    fn every_registry_fault_and_host_code_has_a_variant() {
        for info in all() {
            match info.code.family() {
                Family::Faults => {
                    assert!(FaultCode::from_code(info.code).is_some(), "{}", info.code);
                }
                Family::Host => {
                    let known = HostErrorCode::ALL.iter().any(|h| h.code() == info.code);
                    assert!(known, "{}", info.code);
                }
                _ => {}
            }
        }
        for f in FaultCode::ALL {
            assert_eq!(FaultCode::from_code(f.code()), Some(f));
            assert!(f.code().info().stages.iter().all(|s| s.as_str() == "run"));
        }
    }

    #[test]
    fn limit_faults_carry_the_option_limit_and_counter() {
        let f = Fault::step_limit(9, 9);
        assert_eq!(f.message, "The step limit of 9 was reached.");
        let data = f.data.unwrap();
        assert_eq!(
            (data.option, data.limit, data.used),
            (LimitOption::MaxSteps, 9, 9)
        );
        let m = Fault::memory_limit(100, 90, 20);
        assert_eq!(
            m.message,
            "The memory limit of 100 bytes was reached by an allocation of 20 bytes."
        );
        assert_eq!(m.class(), FaultClass::Resource);
        assert!(
            Fault::call_depth(1000, 1000)
                .message
                .starts_with("The call depth limit of 1000 was exceeded.")
        );
    }

    #[test]
    fn classes_follow_the_specification() {
        assert_eq!(FaultCode::IntOverflow.class(), FaultClass::Resource);
        assert_eq!(FaultCode::DivisionByZero.class(), FaultClass::Domain);
        assert_eq!(FaultCode::CapabilityType.class(), FaultClass::Host);
        assert_eq!(FaultCode::Internal.class(), FaultClass::Internal);
    }

    #[test]
    fn host_errors_have_no_location() {
        let e = HostError::new(HostErrorCode::UnknownExport, &[("name", &"update")]);
        assert_eq!(
            e.message,
            "The program exports no function or constant named 'update'."
        );
        let d = e.to_diagnostic();
        assert!(d.location.is_none());
        assert_eq!(d.code.to_text(), "HS1201");
    }

    #[test]
    fn fault_frames_are_capped() {
        use crate::diagnostic::{Frame, Location};
        use crate::source::Span;
        use std::sync::Arc;
        let frame = Frame {
            function: "f".into(),
            location: Location {
                file: Arc::from("main.hws"),
                span: Span::empty(0),
                line: 1,
                column: 1,
            },
        };
        let f = Fault::internal("x").with_frames(vec![frame; 50]);
        assert_eq!(f.frames.len(), 32);
        assert_eq!(f.to_diagnostic().frames.len(), 32);
    }
}
