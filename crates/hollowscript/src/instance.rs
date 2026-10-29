//! Instances and host-callable functions (chapter 12).
//!
//! [`load`] orchestrates the front end and, once they exist, the compiler and
//! verifier: parse, check, compile, verify, then hand a
//! [`hollowscript_vm::Interpreter`] to a new [`Instance`] in the
//! [`State::Ready`] state. [`Instance`] then owns exactly the state machine of
//! chapter 12: which operations are allowed in which state, and the host
//! errors of section 12.6 for the ones that are not. It does not itself
//! interpret bytecode; that stays [`hollowscript_vm::Interpreter`]'s job, so
//! this module has one thing to get right: never letting a host observe a
//! state the specification does not allow.

use crate::options::{Loader, Options};
use hollowscript_check::CapabilityInterface;
use hollowscript_core::cancel::Control;
use hollowscript_core::fault::{Fault, HostError, HostErrorCode};
use hollowscript_core::{Diagnostic, DiagnosticSet};
use hollowscript_core::{Interner, Limits};
use hollowscript_vm::{CapabilityTable, Command, CommandResult, HostFn, Interpreter, Run, Value};

/// A lookup from one capability function `load` needs to the real
/// implementation a host binds it to; see [`load`]'s own documentation.
pub type CapabilityBinder<'a> = dyn FnMut(&str, Option<&str>) -> Option<Box<dyn HostFn>> + 'a;
use std::sync::Arc;

/// The state of an instance (section 12.2). Exactly one at a time; every
/// operation on an instance either belongs to its current state or fails
/// with the host error of section 12.6, and never runs script code when it fails.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Loaded; initialisation ran to the end of the entry module. Exported
    /// functions may be called.
    Ready,
    /// A load or call is in progress. Nothing but `cancel` may be done.
    Running,
    /// Initialisation ended early through a top-level `return` or `?` in the
    /// entry module. The load result is available; nothing may be called or read.
    Finished,
    /// A load or a call paused at a capability call, on the batched path
    /// (`Options::batch_capability_calls`). The host drains
    /// [`Instance::commands`], performs them, and continues the run with
    /// [`Instance::resume`]; nothing else may be done meanwhile.
    Suspended,
    /// A fault occurred during load or a call. Nothing further can be done
    /// but inspect the fault and dispose the instance.
    Faulted,
    /// [`Instance::dispose`] was called.
    Disposed,
}

/// The outcome of [`load`]: either the check failed and nothing was created
/// (section 12.3 step 1), or an instance now exists, in whichever of
/// [`State::Ready`], [`State::Finished`] or [`State::Faulted`] its
/// initialisation reached.
#[derive(Debug)]
pub enum LoadOutcome {
    /// The entry module or an import had an error; the diagnostics of the
    /// whole check, in canonical order. No instance was created.
    CheckFailed(Vec<Diagnostic>),
    /// An instance now exists.
    Instance(Box<Instance>),
}

/// The load result of a Ready or Finished instance (section 12.1's `result`):
/// present only once initialisation ended through a top-level `return`.
#[derive(Debug)]
pub enum LoadResult {
    /// A Ready instance: initialisation ran to the end with no top-level `return`.
    NoValue,
    /// A Finished instance: the value of the top-level `return`, or the
    /// `Err` that a top-level `?` met.
    Value(Value),
}

/// What one call of an exported function produced.
#[derive(Debug)]
pub enum CallOutcome {
    /// The call ended normally, with the function's return value, or `None`
    /// for a function that returns nothing. The instance stays Ready.
    Returned(Option<Value>),
    /// The call faulted. The instance is now Faulted and this fault is what
    /// [`Instance::result`] will report from now on.
    Faulted(Fault),
    /// The call paused at a capability call, on the batched path. The
    /// instance is now [`State::Suspended`]: drain [`Instance::commands`],
    /// perform them in order, and continue with [`Instance::resume`].
    Suspended,
}

/// One export of the entry module, as a host sees it.
///
/// Section 12.1's operations take an export's name and assume the host
/// knows it, which an embedder usually does. This is the convenience for
/// one that does not: a command line tool listing what a program offers, a
/// test harness calling everything it finds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// The name the program exported it under.
    pub name: String,
    /// The number of parameters of an exported function, or `None` for an
    /// exported `constant`, which is read rather than called.
    pub arity: Option<usize>,
}

/// One loaded program (chapter 12).
pub struct Instance {
    state: State,
    limits: Limits,
    interpreter: Interpreter,
    load_result: Option<LoadResult>,
    /// The load result has not yet been given out through [`Instance::result`].
    result_unread: bool,
    fault: Option<Fault>,
    /// Whether the paused run is the load rather than a call, so that a
    /// resume that finishes it settles the instance the way the load would
    /// have: Ready, or Finished with the top-level `return` value.
    suspended_load: bool,
    /// Every heap value this instance has handed to the host and has not
    /// been told to let go of. A `Value` of a container kind is a handle
    /// into this instance's heap, so the collector has to treat these as
    /// roots: nothing else here can know whether the host still holds one.
    host_roots: Vec<Value>,
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Instance {
    /// Builds a Ready instance around an interpreter whose module has already
    /// been initialised successfully. [`load`] is the public way to reach
    /// this; kept `pub(crate)` so a caller can never construct an instance
    /// that skipped checking.
    pub(crate) fn ready(interpreter: Interpreter, limits: Limits) -> Instance {
        Instance {
            state: State::Ready,
            limits,
            interpreter,
            load_result: None,
            result_unread: false,
            fault: None,
            suspended_load: false,
            host_roots: Vec::new(),
        }
    }

    /// Builds a Finished instance: initialisation ended early with a value.
    pub(crate) fn finished(mut interpreter: Interpreter, limits: Limits, value: Value) -> Instance {
        // The load result is the host's to read, so it is rooted from the
        // start even though no top-level binding holds it; told to the
        // interpreter too, so a mid-run collection during whatever call
        // comes next still finds it (`Instance::remember` does the same for
        // every later result).
        interpreter.remember_host_root(value);
        Instance {
            state: State::Finished,
            limits,
            interpreter,
            load_result: Some(LoadResult::Value(value)),
            result_unread: true,
            fault: None,
            suspended_load: false,
            host_roots: match value {
                Value::Heap(_) => vec![value],
                _ => Vec::new(),
            },
        }
    }

    /// Builds a Suspended instance: initialisation paused at a capability
    /// call on the batched path. [`Instance::resume`] finishes it.
    pub(crate) fn suspended(interpreter: Interpreter, limits: Limits) -> Instance {
        Instance {
            state: State::Suspended,
            limits,
            interpreter,
            load_result: None,
            result_unread: false,
            fault: None,
            suspended_load: true,
            host_roots: Vec::new(),
        }
    }

    /// Builds a Faulted instance: loading itself faulted.
    pub(crate) fn faulted(interpreter: Interpreter, limits: Limits, fault: Fault) -> Instance {
        Instance {
            state: State::Faulted,
            limits,
            interpreter,
            load_result: None,
            result_unread: false,
            fault: Some(fault),
            suspended_load: false,
            host_roots: Vec::new(),
        }
    }

    /// Every export of the entry module, in the order the module records
    /// them: an exported function with its parameter count, an exported
    /// `constant` with none.
    #[must_use]
    pub fn exports(&self) -> Vec<Export> {
        let module = self.interpreter.module().module();
        module
            .exports
            .iter()
            .map(|e| Export {
                name: module.string(e.name).to_owned(),
                arity: match &e.kind {
                    hollowscript_bytecode::module::ExportKind::Function { signature, .. } => {
                        Some(signature.params.len())
                    }
                    hollowscript_bytecode::module::ExportKind::Constant { .. } => None,
                },
            })
            .collect()
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// `call(instance, name, arguments, token)` (section 12.4). Requires the
    /// instance to be Ready. `control` carries the cancellation token and
    /// yield callback of this one call (section 12.1); pass
    /// [`Control::none`] for a call the host never needs to cancel.
    ///
    /// Every argument is checked by section 9.6's acceptance rule (arity,
    /// then each argument's full shape, not only its top-level kind), the
    /// same rule a capability's own result is held to
    /// ([`hollowscript_vm::Interpreter::accept_argument`]). A `String`,
    /// list, map, set, record or enum argument is a handle into *some*
    /// instance's heap; acceptance also refuses one that is not a handle
    /// into this instance's own (section 12.5's "no shared memory
    /// between instances"), which a host mixing up two `Instance`s it holds
    /// would otherwise be able to construct, by luck, from an unrelated live
    /// object at the same slot of a different instance's heap.
    ///
    /// # Errors
    ///
    /// Returns [`HostError`] (HS1201, HS1202, HS1203, HS1204 or HS1205)
    /// exactly when section 12.6 says to, and leaves the instance unchanged
    /// (HS1203 leaves it Faulted, which is what it already was). If
    /// `control`'s token is already set when the call starts, the call
    /// itself succeeds (it reached a Ready instance with a valid export and
    /// arguments) but its outcome is [`CallOutcome::Faulted`] with the
    /// HS1006 fault, before any script code runs, per section 12.1's
    /// cancellation rule.
    pub fn call(
        &mut self,
        name: &str,
        arguments: &[Value],
        control: &mut Control<'_>,
    ) -> Result<CallOutcome, HostError> {
        self.require_ready()?;
        let mut accepted: Vec<Value>;
        let module = self.interpreter.module().module();
        let function = match find_function_export(module, name) {
            FindExport::NotFound => {
                let name_owned = name.to_owned();
                return Err(HostError::new(
                    HostErrorCode::UnknownExport,
                    &[("name", &name_owned)],
                ));
            }
            FindExport::Found { arity, .. } if arity != arguments.len() => {
                let reason = format!("expected {arity} argument(s), found {}", arguments.len());
                return Err(HostError::new(
                    HostErrorCode::BadArguments,
                    &[("name", &name), ("reason", &reason)],
                ));
            }
            FindExport::Found { index, params, .. } => {
                // Section 12.4's step 2: every argument is accepted by
                // section 9.6's rule, which also converts an `Int` for a
                // `Float` parameter. A refusal is reported to the host and
                // does not fault the instance, because nothing has run.
                accepted = Vec::with_capacity(arguments.len());
                for (position, (&value, &ty)) in arguments.iter().zip(params.iter()).enumerate() {
                    let Some(value) = self.interpreter.accept_argument(value, ty) else {
                        let reason = format!(
                            "argument {} is not a value of {}",
                            position + 1,
                            self.interpreter.type_name(ty)
                        );
                        return Err(HostError::new(
                            HostErrorCode::BadArguments,
                            &[("name", &name), ("reason", &reason)],
                        ));
                    };
                    accepted.push(value);
                }
                index
            }
        };
        let arguments = accepted.as_slice();
        self.state = State::Running;
        let outcome = if control.is_cancelled() {
            Err(Fault::cancelled())
        } else {
            self.interpreter
                .call_export_batched(function, arguments, &self.limits, control)
        };
        self.suspended_load = false;
        Ok(self.settle(outcome))
    }

    /// The capability calls queued since the last drain, in order, removing
    /// them from the buffer. A host on the batched path drains these when a
    /// run answers [`CallOutcome::Suspended`], performs them in order at its
    /// own frame boundary, and hands one result per call to
    /// [`Instance::resume`]. Empty on the synchronous path.
    pub fn commands(&mut self) -> Vec<Command> {
        self.interpreter.commands()
    }

    /// Continues a run that [`CallOutcome::Suspended`] paused, with one
    /// [`CommandResult`] per [`Command`] drained, in the order the commands
    /// were queued.
    ///
    /// A resume that finishes the *load* leaves the instance Ready, or
    /// Finished with the value of the entry module's top-level `return`, and
    /// answers [`CallOutcome::Returned`] with that same value; a resume that
    /// finishes a call answers what the call produced.
    ///
    /// # Errors
    ///
    /// Returns HS1204 unless the instance is Suspended.
    pub fn resume(
        &mut self,
        results: Vec<CommandResult>,
        control: &mut Control<'_>,
    ) -> Result<CallOutcome, HostError> {
        if self.state != State::Suspended {
            return Err(HostError::new(HostErrorCode::Busy, &[]));
        }
        self.state = State::Running;
        let outcome = self.interpreter.resume(results, &self.limits, control);
        Ok(self.settle(outcome))
    }

    /// Reads one run's outcome into the instance's state (section 12.2) and
    /// answers what the host sees. Shared by [`Instance::call`] and
    /// [`Instance::resume`], so a paused call settles exactly as an
    /// unpaused one would have.
    fn settle(&mut self, outcome: Result<Run, Fault>) -> CallOutcome {
        let settled = self.settled(outcome);
        if let CallOutcome::Returned(Some(value)) = settled {
            self.remember(value);
        }
        // Everything a finished run allocated and did not keep is freed
        // here. `Interpreter::collect` is now safe to call while suspended
        // too (it roots a paused run's frames the same way a mid-run
        // safepoint collection does; see `Interpreter::live_roots`), but
        // this instance still only collects between calls: a capability
        // call can be pending for an arbitrary amount of host time, and
        // collecting here on every settle, suspended or not, would make
        // this call pay for a collection whose garbage the *next* resume,
        // not this settle, produced. The automatic mid-run safepoint inside
        // the interpreter already keeps a long call's own resident memory
        // down without this instance's help.
        if self.state != State::Suspended {
            self.interpreter.collect(&self.host_roots);
        }
        settled
    }

    /// Remembers a heap value handed to the host, so that the collector
    /// keeps it alive until [`Instance::release_values`] says otherwise. Also
    /// tells the interpreter, whose own `host_roots` a mid-run collection at
    /// a safepoint reads directly: this instance's own `host_roots` still
    /// exists too, because [`Instance::settle`] passes it as `collect`'s
    /// `also` on top of what the interpreter already knows, which is
    /// harmless duplication, not a second, different set of roots.
    fn remember(&mut self, value: Value) {
        if matches!(value, Value::Heap(_)) {
            self.host_roots.push(value);
            self.interpreter.remember_host_root(value);
        }
    }

    /// How many objects this instance's heap holds. A measure of the
    /// host's real memory, not of anything the language defines: section
    /// 11.4's counters are computed from size formulas and are unaffected by
    /// when, or whether, anything is collected.
    #[must_use]
    pub fn heap_len(&self) -> usize {
        self.interpreter.heap().len()
    }

    /// Lets go of every value this instance has handed out: a host that no
    /// longer holds any `Value` from this instance calls this, and the next
    /// collection can then free them.
    ///
    /// A host that holds none in the first place (one that renders every
    /// result to text as it gets it, as the C ABI does) can call this after
    /// every call. A host that does hold one must not: a `Value` of a
    /// container kind is a handle into this instance's heap, and using one
    /// after it has been freed reads whatever now occupies the slot.
    pub fn release_values(&mut self) {
        self.host_roots.clear();
        self.interpreter.forget_host_roots();
    }

    /// The state the run leaves the instance in, and what the host sees.
    fn settled(&mut self, outcome: Result<Run, Fault>) -> CallOutcome {
        match outcome {
            Ok(Run::Suspended) => {
                self.state = State::Suspended;
                CallOutcome::Suspended
            }
            Ok(Run::Done(value)) => {
                if self.suspended_load {
                    self.suspended_load = false;
                    // A load that ends in a top-level `return` leaves a
                    // Finished instance carrying that value (section 12.2),
                    // exactly as an unpaused load would have.
                    if let Some(value) = value {
                        self.state = State::Finished;
                        self.load_result = Some(LoadResult::Value(value));
                        self.result_unread = true;
                        return CallOutcome::Returned(Some(value));
                    }
                    self.load_result = Some(LoadResult::NoValue);
                }
                self.state = State::Ready;
                CallOutcome::Returned(value)
            }
            Err(fault) => {
                self.state = State::Faulted;
                self.fault = Some(fault.clone());
                CallOutcome::Faulted(fault)
            }
        }
    }

    /// `read(instance, name)` (section 12.4): the current value of an
    /// exported `constant` of the entry module.
    ///
    /// # Errors
    ///
    /// Returns HS1205 if the instance is Finished or Disposed, or HS1203 if
    /// it is Faulted, or HS1201 if there is no such exported constant.
    pub fn read(&mut self, name: &str) -> Result<Value, HostError> {
        match self.state {
            State::Finished => {
                return Err(HostError::new(
                    HostErrorCode::BadState,
                    &[("reason", &"has finished")],
                ));
            }
            State::Disposed => {
                return Err(HostError::new(
                    HostErrorCode::BadState,
                    &[("reason", &"was disposed")],
                ));
            }
            State::Faulted => return Err(HostError::new(HostErrorCode::Faulted, &[])),
            State::Running | State::Suspended => {
                return Err(HostError::new(HostErrorCode::Busy, &[]));
            }
            State::Ready => {}
        }
        let module = self.interpreter.module().module();
        let global = find_constant_export(module, name);
        if let Some(index) = global {
            let value = self.interpreter.global(index).unwrap_or(Value::None);
            // A later call may reassign the global this came from, after
            // which only this root keeps the value the host is holding.
            self.remember(value);
            Ok(value)
        } else {
            let name_owned = name.to_owned();
            Err(HostError::new(
                HostErrorCode::UnknownExport,
                &[("name", &name_owned)],
            ))
        }
    }

    /// `result(instance)` (section 12.1) for a Ready or Finished instance:
    /// the load result, once. A Ready instance has no value
    /// ([`LoadResult::NoValue`], returned every time it is asked, since there
    /// is nothing to consume); a Finished instance gives its value the first
    /// time and `None` after. On a Faulted instance the load result is not
    /// meaningful; read [`Instance::fault`] instead, which section 12.1
    /// describes as the same operation answering the fault "as often as asked".
    pub fn result(&mut self) -> Option<LoadResult> {
        match self.state {
            State::Ready => Some(LoadResult::NoValue),
            State::Finished if self.result_unread => {
                self.result_unread = false;
                self.load_result.take()
            }
            _ => None,
        }
    }

    /// Builds a value against this instance's heap, for a host that needs
    /// to pass a `String`, list, map, set, record or enum value to
    /// [`Instance::call`]: those travel as handles into this instance's own
    /// heap and cannot be made anywhere else.
    ///
    /// # Errors
    ///
    /// Whatever `build` returns.
    pub fn heap_string(
        &mut self,
        build: impl FnOnce(&mut hollowscript_vm::HostCall<'_>) -> Result<Value, Fault>,
    ) -> Result<Value, Fault> {
        let value = self.interpreter.with_host_call(&self.limits, build)?;
        self.remember(value);
        Ok(value)
    }

    /// `value` as text, in the form `corpus/FORMAT.md` fixes for a result:
    /// `Int(3)`, `String("a")`, `[Int(1), Int(2)]`, `Colour.Red`. A `Value`
    /// of a container kind is a handle into this instance's heap, so only
    /// the instance it came from can render it.
    #[must_use]
    pub fn render(&self, value: Value) -> String {
        self.interpreter.render(value)
    }

    /// The compiled module this instance runs: an embedder that marshals
    /// values across a foreign boundary (`hollowscript-capi`'s C ABI) reads
    /// `exports`/`capabilities`, together with `types`, to know the
    /// declared, transferable type of an exported function's own parameters
    /// and result, or of a capability's own parameters and result, so it can
    /// decode a JSON argument or answer against that type rather than
    /// guessing from the JSON text alone (a bare number, for one, cannot
    /// otherwise say whether it denotes an `Int` or a `Float`).
    #[must_use]
    pub fn module(&self) -> &hollowscript_bytecode::module::Module {
        self.interpreter.module().module()
    }

    /// The fault of a Faulted instance, retained and returned as often as asked.
    #[must_use]
    pub fn fault(&self) -> Option<&Fault> {
        self.fault.as_ref()
    }

    /// `cancel(instance)` (section 11.7): sets the token of the operation in
    /// progress, if any. This build runs every operation to completion
    /// before returning, so `cancel` itself, which needs a second thread to
    /// call it while a call is in flight, never has an operation in progress
    /// to catch; a host that must not lose a cancellation instead builds a
    /// [`Control`] from its own [`hollowscript_core::CancellationToken`] and passes it to
    /// [`Instance::call`]; a clone of that same token can then be cancelled
    /// concurrently, from another thread, by whatever mechanism the host uses.
    pub fn cancel(&self) {}

    /// `dispose(instance)` (sections 12.2 and 12.5).
    ///
    /// # Errors
    ///
    /// Returns HS1204 while a call is in progress (unreachable in this
    /// build, which has no concurrent calls) and HS1205 if already Disposed.
    pub fn dispose(&mut self) -> Result<(), HostError> {
        match self.state {
            State::Running => Err(HostError::new(HostErrorCode::Busy, &[])),
            State::Disposed => Err(HostError::new(
                HostErrorCode::BadState,
                &[("reason", &"was already disposed")],
            )),
            _ => {
                self.state = State::Disposed;
                Ok(())
            }
        }
    }

    fn require_ready(&self) -> Result<(), HostError> {
        match self.state {
            State::Ready => Ok(()),
            State::Faulted => Err(HostError::new(HostErrorCode::Faulted, &[])),
            State::Running | State::Suspended => Err(HostError::new(HostErrorCode::Busy, &[])),
            State::Finished => Err(HostError::new(
                HostErrorCode::BadState,
                &[("reason", &"has finished")],
            )),
            State::Disposed => Err(HostError::new(
                HostErrorCode::BadState,
                &[("reason", &"was disposed")],
            )),
        }
    }
}

/// The outcome of looking an export up by name, keeping "not found" (HS1201)
/// and "found but the wrong shape" (HS1202) distinct, because the
/// specification reports different codes for them.
enum FindExport {
    /// No export of this name exists.
    NotFound,
    /// An exported function of this name exists, with this arity.
    Found {
        /// Its index into the module's function table.
        index: u32,
        /// Its declared number of parameters.
        arity: usize,
        /// Its declared parameter types, by position.
        params: Vec<hollowscript_core::transfer::TypeRef>,
    },
}

/// Builds the run-time [`CapabilityTable`] the interpreter needs, from the
/// compiled module's own `cap_calls` (section 9.6): for each capability
/// function the code actually calls, in that order, asks `bind_capability`
/// for the real Rust function. A pair the host meant to grant but did not
/// bind is a host defect, reported inside the returned table as an
/// [`HostFn`] that faults with HS1099 the first time it is actually called,
/// rather than panicking here before the program has even run.
fn build_capability_table(
    module: &hollowscript_bytecode::Module,
    bind_capability: &mut CapabilityBinder<'_>,
    host_types: hollowscript_vm::HostTypes,
) -> CapabilityTable {
    use hollowscript_bytecode::module::CapabilityKind;

    let functions = module
        .cap_calls
        .iter()
        .map(|call| {
            let Some(capability) = module.capabilities.get(call.capability as usize) else {
                return unbound_capability("an out of range capability index");
            };
            let name = module.string(capability.name).to_owned();
            let function_name = match &capability.kind {
                CapabilityKind::Function(_) => None,
                CapabilityKind::Namespace(functions) => functions
                    .get(call.function as usize)
                    .map(|(id, _)| module.string(*id).to_owned()),
            };
            bind_capability(&name, function_name.as_deref()).unwrap_or_else(|| {
                unbound_capability(&format!(
                    "'{name}' was granted for checking but not bound for running"
                ))
            })
        })
        .collect();
    CapabilityTable::with_types(functions, host_types)
}

/// A [`HostFn`] standing in for a capability the host did not bind, so a
/// host defect is reported through the normal fault path the first time the
/// program actually calls it, instead of failing before `load` returns.
fn unbound_capability(reason: &str) -> Box<dyn HostFn> {
    let reason = reason.to_owned();
    Box::new(move |_: &[Value], _: &mut hollowscript_vm::HostCall<'_>| {
        Err(Fault::internal(&reason))
    })
}

fn find_function_export(module: &hollowscript_bytecode::Module, name: &str) -> FindExport {
    module
        .exports
        .iter()
        .find_map(|e| match &e.kind {
            hollowscript_bytecode::module::ExportKind::Function {
                function,
                signature,
            } if module.string(e.name) == name => Some(FindExport::Found {
                index: *function,
                arity: signature.params.len(),
                params: signature.params.clone(),
            }),
            _ => None,
        })
        .unwrap_or(FindExport::NotFound)
}

fn find_constant_export(module: &hollowscript_bytecode::Module, name: &str) -> Option<u32> {
    module.exports.iter().find_map(|e| match &e.kind {
        hollowscript_bytecode::module::ExportKind::Constant { global, .. }
            if module.string(e.name) == name =>
        {
            Some(*global)
        }
        _ => None,
    })
}

/// What [`build`] produced: the bytecode, or the reason there is none.
#[derive(Debug)]
pub enum BuildOutcome {
    /// The program checks and compiles; these are the bytes of its
    /// container, which [`hollowscript_bytecode::verify_bytes`] reads and
    /// any VM of this version runs.
    Bytecode(Vec<u8>),
    /// The entry module or an import had an error; the diagnostics of the
    /// whole check, in canonical order.
    CheckFailed(Vec<Diagnostic>),
}

/// Compiles a program to a bytecode container, without running it.
///
/// This is what puts a program somewhere a virtual machine can pick it up
/// later: a published game bundles bytecode compiled here, and the browser
/// VM (`hollowscript-wasm`) verifies and runs it with no parser and no
/// checker of its own. The bytes go through the verifier here too, so a
/// container this produces is one a VM accepts, and a defect in the
/// compiler is caught where the program was built rather than where it is
/// run.
///
/// `options.capabilities` must be the interface the program will actually
/// run against: the module carries the declared type of every capability it
/// uses, and the host records it mentions.
#[must_use]
pub fn build(
    source: &str,
    entry_name: &str,
    options: &Options,
    loader: &mut dyn Loader,
    control: &mut Control<'_>,
) -> BuildOutcome {
    let src =
        match hollowscript_core::Source::new(hollowscript_core::SourceId(0), entry_name, source) {
            Ok(s) => s,
            Err(e) => {
                return BuildOutcome::CheckFailed(vec![e.to_diagnostic(&Arc::from(entry_name))]);
            }
        };
    let mut compiled = match compile_on_dedicated_stack(
        src,
        entry_name,
        &options.capabilities,
        options.filter_schema.as_ref(),
        loader,
        control,
    ) {
        Compiled::Module(module, _interner) => *module,
        Compiled::Failed(diagnostics) => return BuildOutcome::CheckFailed(diagnostics),
    };
    let _ = crate::hosttypes::register(&mut compiled, &options.capabilities);
    let bytes = hollowscript_bytecode::encode(&compiled);
    // Verified here, not only where it runs: a container this hands back is
    // one a VM accepts, so a defect in the compiler is found where the
    // program was built.
    if let Err(e) = hollowscript_bytecode::verify(compiled) {
        return BuildOutcome::CheckFailed(vec![Diagnostic::not_implemented(&format!(
            "a defect in the compiler: {e}"
        ))]);
    }
    BuildOutcome::Bytecode(bytes)
}

/// `check(entry, options, token)` (section 12.1): loads and checks the entry
/// module and its imports and returns the diagnostics, without running
/// anything. `entry_name` is the canonical path reported with each
/// diagnostic. `control` carries the cancellation token and yield callback
/// of this one check (section 12.1): if the token is already set when
/// `check` starts, it faults at once with HS1006, before any parsing or
/// checking runs; otherwise the yield callback is called at every safepoint
/// of the check (every 4,096 tokens scanned or every 1,024 nodes checked).
/// Pass [`Control::none`] for a check the host never needs to cancel.
#[must_use]
pub fn check(
    source: &str,
    entry_name: &str,
    capabilities: &CapabilityInterface,
    filter_schema: Option<&hollowscript_core::filter::FilterSchema>,
    loader: &mut dyn Loader,
    control: &mut Control<'_>,
) -> Vec<Diagnostic> {
    let mut diagnostics = DiagnosticSet::new();
    if control.is_cancelled() {
        diagnostics.push(Fault::cancelled().to_diagnostic());
        return diagnostics.finish(&[Arc::from(entry_name)]);
    }
    let src =
        match hollowscript_core::Source::new(hollowscript_core::SourceId(0), entry_name, source) {
            Ok(src) => src,
            Err(e) => {
                diagnostics.push(e.to_diagnostic(&Arc::from(entry_name)));
                return diagnostics.finish(&[Arc::from(entry_name)]);
            }
        };
    let diagnostics =
        run_check_on_dedicated_stack(src, capabilities, filter_schema, loader, control);
    diagnostics.finish(&[Arc::from(entry_name)])
}

/// Every filter literal of a program (Appendix A), found by the checker, for a
/// host that evaluates filters and needs to know, before anything runs, which
/// ones a program can ask it to evaluate: a `Filter` reaches a capability as
/// its source text alone (section A.1). Each is the decoded source and the
/// location of the whole literal, in source order within a module, the
/// imported modules before the modules that import them and the entry module
/// last.
///
/// The program need not check cleanly. A literal is listed whether or not it
/// is valid, and whether or not the program around it has errors, so a host
/// can show what a program asked even where it will not run; use [`check`] for
/// whether it will. A literal the lexer rejected (an unterminated backtick) is
/// not a token and is not listed. `entry_name`, `capabilities`, `loader` and
/// `control` are as [`check`]'s; a filter schema is not taken, since a
/// literal's source does not depend on one. Answers nothing if `control`
/// cancels before the work starts or the work cannot be done at all.
#[must_use]
pub fn filter_literals(
    source: &str,
    entry_name: &str,
    capabilities: &CapabilityInterface,
    loader: &mut dyn Loader,
    control: &mut Control<'_>,
) -> Vec<hollowscript_check::FilterLiteral> {
    if control.is_cancelled() {
        return Vec::new();
    }
    let Ok(src) =
        hollowscript_core::Source::new(hollowscript_core::SourceId(0), entry_name, source)
    else {
        return Vec::new();
    };
    let run = move || {
        let mut interner = Interner::new();
        let parsed = hollowscript_syntax::parse(&src, &mut interner, control);
        hollowscript_check::check(
            &parsed.file,
            &src,
            &mut interner,
            capabilities,
            None,
            &mut |path| {
                let bytes = loader.load(path)?;
                String::from_utf8(bytes).map_err(|_| {
                    hollowscript_core::LoadModuleError::Failed("not valid UTF-8".to_owned())
                })
            },
        )
        .filter_literals
    };
    hollowscript_core::run_on_dedicated_stack(run).unwrap_or_default()
}

/// Parses and checks `src`, both of which recurse over the syntax tree up to
/// section 1.10's own structural limits: both run together on
/// [`hollowscript_core::run_on_dedicated_stack`]'s dedicated thread, the
/// same one [`hollowscript_syntax::parse`] already needs on its own account
/// (see `hollowscript_core::stack`'s module documentation), so that a small
/// caller stack cannot be exhausted by either pass. [`check`], [`build`] and
/// [`load`] all funnel through here for that reason: `hs_check_json` was not
/// the only entry point a deeply nested program could reach on a small
/// thread.
fn run_check_on_dedicated_stack(
    src: hollowscript_core::Source,
    capabilities: &CapabilityInterface,
    filter_schema: Option<&hollowscript_core::filter::FilterSchema>,
    loader: &mut dyn Loader,
    control: &mut Control<'_>,
) -> DiagnosticSet {
    let run = move || {
        let mut interner = Interner::new();
        let mut diagnostics = DiagnosticSet::new();
        let parsed = hollowscript_syntax::parse(&src, &mut interner, control);
        for d in parsed.diagnostics {
            diagnostics.push(d);
        }
        let checked = hollowscript_check::check(
            &parsed.file,
            &src,
            &mut interner,
            capabilities,
            filter_schema,
            &mut |path| {
                let bytes = loader.load(path)?;
                String::from_utf8(bytes).map_err(|_| {
                    hollowscript_core::LoadModuleError::Failed("not valid UTF-8".to_owned())
                })
            },
        );
        for d in checked.diagnostics {
            diagnostics.push(d);
        }
        // Section 1.10 reports a program that exceeds a limit of this
        // implementation (HS0901) among the diagnostics of checking, and
        // only compiling finds one, so a program that checks clean is
        // compiled here too, and what compiling refuses is reported. The
        // module itself is not kept: `build` and `load` compile it again.
        if !diagnostics.has_errors() {
            if let Err(e) = hollowscript_compiler::compile(
                &checked.hir,
                &interner,
                hollowscript_compiler::Options::default(),
            ) {
                diagnostics.push(e.into_diagnostic());
            }
        }
        diagnostics
    };
    match hollowscript_core::run_on_dedicated_stack(run) {
        Ok(diagnostics) => diagnostics,
        // Checking must never panic; a `Panicked` here only guards against
        // a defect elsewhere actually doing so. `SpawnFailed` (the host
        // could not create the thread) is not a case this function can
        // usefully recover from. Both are reported the same way
        // `hollowscript_syntax::parse`'s identical branch reports its own.
        Err(hollowscript_core::DedicatedStackError::Panicked) => {
            let mut diagnostics = DiagnosticSet::new();
            diagnostics.push(Diagnostic::new(
                hollowscript_core::Code::HS1099,
                &[("message", &"checking did not complete")],
            ));
            diagnostics
        }
        Err(hollowscript_core::DedicatedStackError::SpawnFailed) => {
            let mut diagnostics = DiagnosticSet::new();
            diagnostics.push(Diagnostic::new(
                hollowscript_core::Code::HS1099,
                &[("message", &"could not start checking")],
            ));
            diagnostics
        }
    }
}

/// What parsing, checking and compiling a program on the dedicated
/// large-stack thread produced: the compiled module and the interner its
/// strings still need (for [`crate::hosttypes::register`]), or, if parsing,
/// checking or compiling itself found an error, the whole check's
/// diagnostics in canonical order (exactly what [`check`] alone would have
/// answered).
enum Compiled {
    /// A clean program, compiled.
    Module(Box<hollowscript_bytecode::module::Module>, Interner),
    /// An error was found before compiling could finish.
    Failed(Vec<Diagnostic>),
}

/// [`build`] and [`load`] both parse, check and, once that is clean,
/// compile the same program; this is the one place that pipeline is
/// written; see `run_check_on_dedicated_stack`'s own documentation for why
/// it all runs on one dedicated thread rather than only the parsing and
/// checking part of it.
fn compile_on_dedicated_stack(
    src: hollowscript_core::Source,
    entry_name: &str,
    capabilities: &CapabilityInterface,
    filter_schema: Option<&hollowscript_core::filter::FilterSchema>,
    loader: &mut dyn Loader,
    control: &mut Control<'_>,
) -> Compiled {
    let run = move || {
        let mut interner = Interner::new();
        let mut diagnostics = DiagnosticSet::new();
        let parsed = hollowscript_syntax::parse(&src, &mut interner, control);
        for d in parsed.diagnostics {
            diagnostics.push(d);
        }
        let checked = hollowscript_check::check(
            &parsed.file,
            &src,
            &mut interner,
            capabilities,
            filter_schema,
            &mut |path| {
                let bytes = loader.load(path)?;
                String::from_utf8(bytes).map_err(|_| {
                    hollowscript_core::LoadModuleError::Failed("not valid UTF-8".to_owned())
                })
            },
        );
        let hir = checked.hir;
        for d in checked.diagnostics {
            diagnostics.push(d);
        }
        if diagnostics.has_errors() {
            return Compiled::Failed(diagnostics.finish(&[Arc::from(entry_name)]));
        }
        match hollowscript_compiler::compile(
            &hir,
            &interner,
            hollowscript_compiler::Options::default(),
        ) {
            Ok(module) => Compiled::Module(Box::new(module), interner),
            // With the warnings checking found, exactly as `check` reports
            // them: a limit of this implementation (HS0901) is a diagnostic
            // of checking (section 1.10).
            Err(e) => {
                diagnostics.push(e.into_diagnostic());
                Compiled::Failed(diagnostics.finish(&[Arc::from(entry_name)]))
            }
        }
    };
    match hollowscript_core::run_on_dedicated_stack(run) {
        Ok(result) => result,
        // See `run_check_on_dedicated_stack`'s identical branch: neither
        // case is one this function can usefully recover from.
        Err(hollowscript_core::DedicatedStackError::Panicked) => {
            Compiled::Failed(vec![Diagnostic::new(
                hollowscript_core::Code::HS1099,
                &[("message", &"checking or compiling did not complete")],
            )])
        }
        Err(hollowscript_core::DedicatedStackError::SpawnFailed) => {
            Compiled::Failed(vec![Diagnostic::new(
                hollowscript_core::Code::HS1099,
                &[("message", &"could not start checking or compiling")],
            )])
        }
    }
}

/// The diagnostics of the parse stage alone (`corpus/FORMAT.md`'s `parse`):
/// lexing and parsing the entry module, with no imports loaded, no names
/// resolved and no types checked. [`check`] reports these and more; this is
/// separate because chapter 19 makes the two separate stages of a
/// conformance case, and because a tool that only wants to know whether text
/// is syntactically a program should not pay for a whole check.
#[must_use]
pub fn parse_diagnostics(
    source: &str,
    entry_name: &str,
    control: &mut Control<'_>,
) -> Vec<Diagnostic> {
    let mut interner = Interner::new();
    let mut diagnostics = DiagnosticSet::new();
    let src =
        match hollowscript_core::Source::new(hollowscript_core::SourceId(0), entry_name, source) {
            Ok(src) => src,
            Err(e) => {
                diagnostics.push(e.to_diagnostic(&Arc::from(entry_name)));
                return diagnostics.finish(&[Arc::from(entry_name)]);
            }
        };
    let parsed = hollowscript_syntax::parse(&src, &mut interner, control);
    for d in parsed.diagnostics {
        diagnostics.push(d);
    }
    diagnostics.finish(&[Arc::from(entry_name)])
}

/// `load(entry, options, token)` (section 12.3). `loader` resolves imports.
/// `bind_capability` gives the real function for one capability function:
/// `name` is as written in `uses`, and `function` is the member name for a
/// namespace capability such as `clock.now`, or `None` for a single-function
/// capability such as `print`. It is asked only for a `(name, function)`
/// pair the compiled program actually calls, in the order the compiler's own
/// bytecode assigns them, which is why this is a lookup rather than a table
/// the caller builds ahead of time: that order is not known until the
/// program is compiled, which happens inside this call. Returning `None`
/// for a pair the host meant to grant is a host defect, reported as HS1099
/// rather than left to panic. `control` is as [`check`] documents it; the
/// same token and callback govern the whole load, including the
/// initialisation run of step 3 of section 12.3.
///
/// `check` runs first, exactly as section 12.3 requires: a program with an
/// error returns [`LoadOutcome::CheckFailed`] with no instance created. A
/// clean program is compiled, verified and initialised; some native calls
/// and imports are still incomplete beneath this facade (in the interpreter,
/// not here), so a program that reaches one faults rather than succeeding.
pub fn load(
    source: &str,
    entry_name: &str,
    options: &Options,
    loader: &mut dyn Loader,
    bind_capability: &mut CapabilityBinder<'_>,
    control: &mut Control<'_>,
) -> LoadOutcome {
    let src =
        match hollowscript_core::Source::new(hollowscript_core::SourceId(0), entry_name, source) {
            Ok(s) => s,
            Err(e) => {
                return LoadOutcome::CheckFailed(vec![e.to_diagnostic(&Arc::from(entry_name))]);
            }
        };
    let mut compiled = match compile_on_dedicated_stack(
        src,
        entry_name,
        &options.capabilities,
        options.filter_schema.as_ref(),
        loader,
        control,
    ) {
        Compiled::Module(module, _interner) => *module,
        Compiled::Failed(diagnostics) => return LoadOutcome::CheckFailed(diagnostics),
    };
    // Every record and enum the host's own capabilities mention is put into
    // the module's tables here, between compiling and verifying, so that a
    // capability function can build one (see [`crate::hosttypes`]).
    let mut host_types = crate::hosttypes::register(&mut compiled, &options.capabilities);
    // Then the other direction: every record shape and enum variant the
    // module already has, whether it came from that step or the program's
    // own source (including an exported function's signature), is named in
    // `host_types` too, so a host reading a value off the heap (a
    // capability argument, or an `hs_call` argument or result) is not
    // limited to ones a capability's declared type happened to mention.
    crate::hosttypes::register_module_types(&mut compiled, &mut host_types);
    let verified = match hollowscript_bytecode::verify(compiled) {
        Ok(v) => v,
        Err(e) => {
            return LoadOutcome::CheckFailed(vec![Diagnostic::not_implemented(&format!(
                "a defect in the compiler: {e}"
            ))]);
        }
    };
    start(verified, host_types, options, bind_capability, control)
}

/// What [`load_bytecode`] could not load.
#[derive(Debug)]
pub enum BytecodeError {
    /// The bytes are not a container this version reads, or the program in
    /// it breaks a rule of the verifier.
    Invalid(hollowscript_bytecode::LoadError),
}

impl std::fmt::Display for BytecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BytecodeError::Invalid(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for BytecodeError {}

/// Whether `bytes` start like a bytecode container ([`build`]'s output)
/// rather than source text: the container's four-byte magic number, which
/// no HollowScript source can begin with. Says nothing about whether the
/// rest is valid; [`load_bytecode`] verifies that.
#[must_use]
pub fn is_bytecode(bytes: &[u8]) -> bool {
    bytes.starts_with(&hollowscript_bytecode::container::MAGIC)
}

/// `load` for a program already compiled by [`build`]: verifies the
/// container exactly as a VM that has no compiler does, checks that every
/// capability it uses is one `options.capabilities` declares with the same
/// signature (the same promise section 12.3's check makes for source, so a
/// container built with `--allow fs` cannot run where `fs` is not granted),
/// then initialises it as [`load`] does.
///
/// A capability the container uses that the host does not declare, or
/// declares with a different signature, is HS0404 in
/// [`LoadOutcome::CheckFailed`], with no location, since there is no source
/// to point into.
///
/// # Errors
///
/// [`BytecodeError::Invalid`] if the bytes do not verify.
pub fn load_bytecode(
    bytes: &[u8],
    options: &Options,
    bind_capability: &mut CapabilityBinder<'_>,
    control: &mut Control<'_>,
) -> Result<LoadOutcome, BytecodeError> {
    let verified = hollowscript_bytecode::verify_bytes(bytes).map_err(BytecodeError::Invalid)?;
    let missing =
        crate::hosttypes::ungranted_capabilities(verified.module(), &options.capabilities);
    if !missing.is_empty() {
        return Ok(LoadOutcome::CheckFailed(
            missing
                .iter()
                .map(|name| Diagnostic::new(hollowscript_core::Code::HS0404, &[("name", name)]))
                .collect(),
        ));
    }
    let host_types = crate::hosttypes::of_module(verified.module());
    Ok(start(
        verified,
        host_types,
        options,
        bind_capability,
        control,
    ))
}

/// The part of [`load`] and [`load_bytecode`] after verification: binds the
/// capabilities and runs the initialisers.
fn start(
    verified: hollowscript_bytecode::VerifiedModule,
    host_types: hollowscript_vm::HostTypes,
    options: &Options,
    bind_capability: &mut CapabilityBinder<'_>,
    control: &mut Control<'_>,
) -> LoadOutcome {
    let capability_table = build_capability_table(verified.module(), bind_capability, host_types);
    let mut interpreter = Interpreter::new(verified, capability_table);
    if options.batch_capability_calls {
        interpreter.use_command_buffer();
    }
    if options.gc_stress_mode {
        // Set before the initialiser runs, so the stress mode covers the
        // load itself, not only calls made after it.
        interpreter.enable_gc_stress_mode();
    }
    match interpreter.initialise_batched(&options.limits, control) {
        Ok(Run::Done(Some(value))) => {
            // Whatever initialisation built on the way to this value and no
            // top-level binding kept is freed before the instance is handed
            // over; the value itself is a root, since the host may read it.
            interpreter.collect(&[value]);
            LoadOutcome::Instance(Box::new(Instance::finished(
                interpreter,
                options.limits,
                value,
            )))
        }
        Ok(Run::Done(None)) => {
            interpreter.collect(&[]);
            LoadOutcome::Instance(Box::new(Instance::ready(interpreter, options.limits)))
        }
        Ok(Run::Suspended) => {
            LoadOutcome::Instance(Box::new(Instance::suspended(interpreter, options.limits)))
        }
        Err(fault) => LoadOutcome::Instance(Box::new(Instance::faulted(
            interpreter,
            options.limits,
            fault,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{CallOutcome, Instance, LoadOutcome, State, check, filter_literals, load};
    use crate::options::{CapabilityInterface, NoLoader, Options};
    use hollowscript_bytecode::module::{
        Export, ExportKind, Function, FunctionKind, Global, Module, ModuleEntry, Signature,
    };
    use hollowscript_bytecode::opcode::{Instruction, Op};
    use hollowscript_bytecode::verify::verify;
    use hollowscript_core::Code;
    use hollowscript_core::cancel::{CancellationToken, Control};
    use hollowscript_core::fault::HostErrorCode;
    use hollowscript_vm::{CapabilityTable, CommandResult, Interpreter, Value};

    #[test]
    fn checking_a_clean_program_reports_no_diagnostics() {
        let diagnostics = check(
            "return 1",
            "main.hws",
            &CapabilityInterface::default(),
            None,
            &mut NoLoader,
            &mut Control::none(),
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn a_token_already_set_faults_check_at_once_with_hs1006() {
        let token = CancellationToken::new();
        token.cancel();
        let diagnostics = check(
            "return 1",
            "main.hws",
            &CapabilityInterface::default(),
            None,
            &mut NoLoader,
            &mut Control::new(Some(&token), None),
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Code::HS1006);
    }

    #[test]
    fn loading_a_clean_program_with_no_imports_or_capabilities_succeeds() {
        let outcome = load(
            "return 1",
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        );
        match outcome {
            LoadOutcome::Instance(_) => {}
            LoadOutcome::CheckFailed(diagnostics) => {
                panic!("expected a loaded instance, got check failures: {diagnostics:?}");
            }
        }
    }

    /// A module with one exported function `double(Int): Int` (its body is
    /// `return n`, an identity, so the interesting thing about it is its
    /// signature, not its result), one exported function `explode(): Int`
    /// whose body is an unconditional `Trap` (HS1099), and one exported
    /// constant `answer`, used to test the instance state machine directly,
    /// without going through `load`.
    fn sample_instance() -> Instance {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let init_name = m.add_string("main").unwrap();
        let fn_name = m.add_string("double").unwrap();
        let explode_name = m.add_string("explode").unwrap();
        let const_name = m.add_string("answer").unwrap();
        let int = m.types.add(hollowscript_core::transfer::TransferType::Int);
        let ret = Instruction::abc(Op::ReturnNone, 0, 0, 0);
        let init = m.functions.len();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![ret.encode()],
            spans: vec![],
        });
        let double = m.functions.len();
        m.functions.push(Function {
            name: fn_name,
            kind: FunctionKind::Function,
            arity: 1,
            register_count: 1,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::Return, 0, 0, 0).encode()],
            spans: vec![],
        });
        let explode = m.functions.len();
        m.functions.push(Function {
            name: explode_name,
            kind: FunctionKind::Function,
            arity: 0,
            register_count: 1,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abx(Op::Trap, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry {
            path,
            init: u32::try_from(init).unwrap(),
        });
        m.globals.push(Global {
            module: 0,
            name: const_name,
            mutable: false,
        });
        m.exports.push(Export {
            name: fn_name,
            kind: ExportKind::Function {
                function: u32::try_from(double).unwrap(),
                signature: Signature {
                    params: vec![int],
                    ret: Some(int),
                },
            },
        });
        m.exports.push(Export {
            name: explode_name,
            kind: ExportKind::Function {
                function: u32::try_from(explode).unwrap(),
                signature: Signature {
                    params: vec![],
                    ret: Some(int),
                },
            },
        });
        m.exports.push(Export {
            name: const_name,
            kind: ExportKind::Constant { global: 0, ty: int },
        });
        let verified = verify(m).unwrap();
        Instance::ready(
            Interpreter::new(verified, CapabilityTable::new(vec![])),
            hollowscript_core::Limits::default(),
        )
    }

    #[test]
    fn calling_an_unknown_export_is_a_host_error_and_leaves_the_instance_ready() {
        let mut instance = sample_instance();
        let err = instance
            .call("missing", &[], &mut Control::none())
            .unwrap_err();
        assert_eq!(err.code, HostErrorCode::UnknownExport);
        assert_eq!(instance.state(), State::Ready);
    }

    #[test]
    fn calling_with_the_wrong_arity_is_bad_arguments_not_unknown_export() {
        let mut instance = sample_instance();
        let err = instance
            .call("double", &[], &mut Control::none())
            .unwrap_err();
        assert_eq!(err.code, HostErrorCode::BadArguments);
        assert_eq!(instance.state(), State::Ready);
    }

    #[test]
    fn a_call_that_faults_moves_the_instance_to_faulted() {
        let mut instance = sample_instance();
        let outcome = instance.call("explode", &[], &mut Control::none()).unwrap();
        assert!(matches!(outcome, CallOutcome::Faulted(_)));
        assert_eq!(instance.state(), State::Faulted);
        assert!(instance.fault().is_some());
        // Every further call reports HS1203 and does not change the instance.
        let err = instance
            .call("double", &[Value::Int(1)], &mut Control::none())
            .unwrap_err();
        assert_eq!(err.code, HostErrorCode::Faulted);
        assert_eq!(instance.state(), State::Faulted);
    }

    #[test]
    fn a_call_that_returns_runs_the_interpreter_end_to_end() {
        let mut instance = sample_instance();
        let outcome = instance
            .call("double", &[Value::Int(21)], &mut Control::none())
            .unwrap();
        assert!(matches!(
            outcome,
            CallOutcome::Returned(Some(Value::Int(21)))
        ));
        assert_eq!(instance.state(), State::Ready);
    }

    #[test]
    fn a_token_already_set_faults_the_call_at_once_with_hs1006() {
        let mut instance = sample_instance();
        let token = CancellationToken::new();
        token.cancel();
        let outcome = instance
            .call(
                "double",
                &[Value::Int(21)],
                &mut Control::new(Some(&token), None),
            )
            .unwrap();
        match outcome {
            CallOutcome::Faulted(fault) => {
                assert_eq!(fault.code, hollowscript_core::fault::FaultCode::Cancelled);
            }
            other => panic!("expected the call to be cancelled at once, got {other:?}"),
        }
        assert_eq!(instance.state(), State::Faulted);
    }

    #[test]
    fn reading_a_constant_works_when_ready_and_is_refused_otherwise() {
        let mut instance = sample_instance();
        assert!(instance.read("answer").is_ok());
        assert_eq!(
            instance.read("missing").unwrap_err().code,
            HostErrorCode::UnknownExport
        );
        instance.dispose().unwrap();
        assert_eq!(
            instance.read("answer").unwrap_err().code,
            HostErrorCode::BadState
        );
    }

    /// A whole program, from source text, whose exports a host then calls
    /// and reads: the part of chapter 12 that had nothing to work against
    /// until the compiler built `Module::exports`.
    #[test]
    fn a_host_calls_and_reads_the_exports_of_a_real_program() {
        let source = concat!(
            "export fn twice(n: Int): Int {\n",
            "    return n * 2\n",
            "}\n",
            "export fn greet(who: String): String {\n",
            "    return \"hello, ${who}\"\n",
            "}\n",
            "export fn nothing() {\n",
            "    return\n",
            "}\n",
            "export constant answer = 42\n",
            "export constant names = [\"a\", \"b\"]\n",
        );
        let outcome = load(
            source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        );
        let LoadOutcome::Instance(mut instance) = outcome else {
            panic!("the program checks and loads")
        };

        let CallOutcome::Returned(Some(value)) = instance
            .call("twice", &[Value::Int(21)], &mut Control::none())
            .expect("twice is exported")
        else {
            panic!("twice returns a value")
        };
        assert!(matches!(value, Value::Int(42)));

        let text = instance
            .heap_string(|call| call.string("world"))
            .expect("a string argument");
        let CallOutcome::Returned(Some(greeting)) = instance
            .call("greet", &[text], &mut Control::none())
            .expect("greet is exported")
        else {
            panic!("greet returns a value")
        };
        assert_eq!(instance.render(greeting), r#"String("hello, world")"#);

        let CallOutcome::Returned(None) = instance
            .call("nothing", &[], &mut Control::none())
            .expect("nothing is exported")
        else {
            panic!("nothing returns nothing")
        };

        let answer = instance.read("answer").expect("answer is exported");
        assert_eq!(instance.render(answer), "Int(42)");
        let names = instance.read("names").expect("names is exported");
        assert_eq!(instance.render(names), r#"[String("a"), String("b")]"#);

        assert_eq!(
            instance
                .call("twice", &[], &mut Control::none())
                .unwrap_err()
                .code,
            HostErrorCode::BadArguments
        );
        assert_eq!(
            instance.read("twice").unwrap_err().code,
            HostErrorCode::UnknownExport
        );
        instance.dispose().unwrap();
    }

    /// Section 12.5's "no shared memory between instances": a
    /// `Value` built on one instance's heap must never be accepted as an
    /// argument by a different instance, even though both are indistinct
    /// index-and-generation handles into their own heaps and so can, and
    /// here do, name the same slot number. Before the fix, `b.call` read
    /// whatever live object happened to occupy that slot of `b`'s own
    /// heap, which is this test's `"kept secret"`, not the string `a` built.
    #[test]
    fn a_handle_from_one_instance_is_refused_by_another() {
        let load_one = |source: &str| {
            let outcome = load(
                source,
                "main.hws",
                &Options::default(),
                &mut NoLoader,
                &mut |_, _| None,
                &mut Control::none(),
            );
            let LoadOutcome::Instance(instance) = outcome else {
                panic!("the program checks and loads: {source}")
            };
            instance
        };
        let mut a = load_one(concat!(
            "export fn make(): String {\n",
            "    return \"made in a\"\n",
            "}\n",
        ));
        let mut b = load_one(concat!(
            "constant kept = \"kept secret\"\n",
            "export fn echo(s: String): String {\n",
            "    return s\n",
            "}\n",
        ));

        // Both instances' first heap allocation lands at the same slot
        // index and generation of their own, separate heaps: exactly the
        // coincidence this closes off.
        let from_a = a
            .heap_string(|call| call.string("built by a"))
            .expect("a builds its own string");

        let refused = b
            .call("echo", &[from_a], &mut Control::none())
            .expect_err("a's handle is foreign to b's heap");
        assert_eq!(refused.code, HostErrorCode::BadArguments);
        assert_eq!(b.state(), State::Ready, "b is left exactly as it was");

        // b still works normally with a value of its own.
        let from_b = b
            .heap_string(|call| call.string("built by b"))
            .expect("b builds its own string");
        let CallOutcome::Returned(Some(echoed)) = b
            .call("echo", &[from_b], &mut Control::none())
            .expect("b accepts its own handle")
        else {
            panic!("echo returns a value")
        };
        assert_eq!(b.render(echoed), r#"String("built by b")"#);
    }

    #[test]
    fn dispose_is_idempotent_free_and_then_refused() {
        let mut instance = sample_instance();
        assert!(instance.dispose().is_ok());
        assert_eq!(instance.state(), State::Disposed);
        assert_eq!(
            instance.dispose().unwrap_err().code,
            HostErrorCode::BadState
        );
    }

    /// A container `build` wrote loads through `load_bytecode` exactly as
    /// the source loads through `load`, but only where every capability it
    /// uses is declared with the signature it was built against.
    #[test]
    fn load_bytecode_runs_what_build_wrote_under_the_same_grants() {
        use crate::instance::{BuildOutcome, LoadResult, build, is_bytecode, load_bytecode};
        use hollowscript_check::effects::{CapabilityDecl, CapabilityShape};
        use hollowscript_core::transfer::{TransferType, TypeTable};
        use hollowscript_vm::HostFn;

        let interface = |ret: TransferType| {
            let mut types = TypeTable::new();
            let ret = types.add(ret);
            let mut capabilities = CapabilityInterface::default();
            capabilities.capabilities.push(CapabilityDecl {
                name: "answer".to_owned(),
                shape: CapabilityShape::Function {
                    params: vec![],
                    ret: Some(ret),
                },
            });
            capabilities.types = types;
            Options {
                capabilities,
                ..Options::default()
            }
        };
        let options = interface(TransferType::Int);
        let source = "uses answer\nreturn effect answer() + 1\n";
        let BuildOutcome::Bytecode(bytes) = build(
            source,
            "main.hws",
            &options,
            &mut NoLoader,
            &mut Control::none(),
        ) else {
            panic!("the program should build");
        };
        assert!(is_bytecode(&bytes));
        assert!(!is_bytecode(source.as_bytes()));

        let mut bind = |_: &str, _: Option<&str>| -> Option<Box<dyn HostFn>> {
            Some(Box::new(
                |_: &[Value], _: &mut hollowscript_vm::HostCall<'_>| Ok(Value::Int(41)),
            ))
        };
        let Ok(LoadOutcome::Instance(mut instance)) =
            load_bytecode(&bytes, &options, &mut bind, &mut Control::none())
        else {
            panic!("the container should load");
        };
        assert!(matches!(
            instance.result(),
            Some(LoadResult::Value(Value::Int(42)))
        ));

        for other in [Options::default(), interface(TransferType::String)] {
            let Ok(LoadOutcome::CheckFailed(diagnostics)) =
                load_bytecode(&bytes, &other, &mut bind, &mut Control::none())
            else {
                panic!("a missing or different capability is refused");
            };
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code, Code::HS0404);
        }

        let half = bytes.get(..bytes.len() / 2).unwrap();
        assert!(load_bytecode(half, &options, &mut bind, &mut Control::none()).is_err());
    }

    /// A frame-based host end to end through the facade: a program that
    /// calls a capability pauses, the host drains the queued call at its own
    /// boundary, performs it, and continues the run.
    #[test]
    fn a_batched_host_drains_a_call_and_resumes_the_run() {
        use hollowscript_check::effects::{CapabilityDecl, CapabilityShape};
        use hollowscript_core::transfer::{TransferType, TypeTable};

        let mut types = TypeTable::new();
        let string_ty = types.add(TransferType::String);
        let mut capabilities = CapabilityInterface::default();
        capabilities.capabilities.push(CapabilityDecl {
            name: "shout".to_owned(),
            shape: CapabilityShape::Function {
                params: vec![string_ty],
                ret: Some(string_ty),
            },
        });
        capabilities.types = types;
        let options = Options {
            capabilities,
            batch_capability_calls: true,
            ..Options::default()
        };
        let source = concat!(
            "uses shout
",
            "export fn loud(word: String): String {
",
            "    return effect shout(word)
",
            "}
",
        );
        // Nothing is bound: on the batched path a capability never calls a
        // host function, it queues a command.
        let LoadOutcome::Instance(mut instance) = load(
            source,
            "main.hws",
            &options,
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        ) else {
            panic!("the program should load");
        };
        assert_eq!(instance.state(), State::Ready);

        let word = instance
            .heap_string(|call| call.string("hello"))
            .expect("a heap string");
        let outcome = instance
            .call("loud", &[word], &mut Control::none())
            .expect("the call is accepted");
        assert!(matches!(outcome, CallOutcome::Suspended));
        assert_eq!(instance.state(), State::Suspended);

        // While suspended the instance is busy: nothing else may be done.
        let busy = instance
            .call("loud", &[word], &mut Control::none())
            .expect_err("a suspended instance is busy");
        assert_eq!(busy.code.code(), Code::HS1204);

        let commands = instance.commands();
        assert_eq!(commands.len(), 1);
        let shouted = {
            let text = instance.render(commands[0].args[0]);
            assert_eq!(text, r#"String("hello")"#);
            instance
                .heap_string(|call| call.string("HELLO!"))
                .expect("a heap string")
        };
        let outcome = instance
            .resume(vec![CommandResult::Ok(shouted)], &mut Control::none())
            .expect("the resume is accepted");
        let CallOutcome::Returned(Some(value)) = outcome else {
            panic!("expected a value, got {outcome:?}");
        };
        assert_eq!(instance.render(value), r#"String("HELLO!")"#);
        assert_eq!(instance.state(), State::Ready);
    }

    /// Everything a call built and did not keep is freed when the call ends,
    /// so a long-lived instance does not grow with every call. Section 11.4
    /// keeps the counters out of it: a collection changes the host's real
    /// memory, never when a limit is reached.
    #[test]
    fn a_call_does_not_leave_its_working_values_in_the_heap() {
        let source = concat!(
            "export fn churn(n: Int): Int {\n",
            "    variable total = 0\n",
            "    for i in [1, 2, 3, 4, 5, 6, 7, 8] {\n",
            "        constant text = \"row ${i} of ${n}\"\n",
            "        total = total + text.length()\n",
            "    }\n",
            "    return total\n",
            "}\n",
        );
        let LoadOutcome::Instance(mut instance) = load(
            source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        ) else {
            panic!("the program should load");
        };
        let mut sizes = Vec::new();
        for _ in 0..3 {
            let outcome = instance
                .call("churn", &[Value::Int(1)], &mut Control::none())
                .expect("the call is accepted");
            assert!(matches!(
                outcome,
                CallOutcome::Returned(Some(Value::Int(_)))
            ));
            instance.release_values();
            sizes.push(instance.heap_len());
        }
        // The heap is the same size after the third call as after the
        // first: the strings each call built are gone.
        assert_eq!(sizes[0], sizes[1], "{sizes:?}");
        assert_eq!(sizes[1], sizes[2], "{sizes:?}");
    }

    /// A value the host is holding is a root, so a collection cannot free it
    /// underneath the host.
    #[test]
    fn a_value_the_host_holds_survives_a_later_call() {
        let source = concat!(
            "export constant name = \"kept\"\n",
            "export fn churn(): Int {\n",
            "    constant junk = \"${1} ${2} ${3}\"\n",
            "    return junk.length()\n",
            "}\n",
        );
        let LoadOutcome::Instance(mut instance) = load(
            source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        ) else {
            panic!("the program should load");
        };
        let held = instance.read("name").expect("an exported constant");
        instance
            .call("churn", &[], &mut Control::none())
            .expect("the call is accepted");
        assert_eq!(instance.render(held), r#"String("kept")"#);
    }

    /// Section 12.4's step 2: an argument that is not a value of the
    /// parameter's type is HS1202, reported to the host, and the instance is
    /// untouched because nothing ran.
    #[test]
    fn an_argument_of_the_wrong_type_is_reported_and_does_not_fault() {
        let source = concat!(
            "export fn twice(n: Int): Int {\n",
            "    return n * 2\n",
            "}\n",
            "export fn half(x: Float): Float {\n",
            "    return x / 2.0\n",
            "}\n",
        );
        let LoadOutcome::Instance(mut instance) = load(
            source,
            "main.hws",
            &Options::default(),
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        ) else {
            panic!("the program should load");
        };
        let error = instance
            .call("twice", &[Value::Bool(true)], &mut Control::none())
            .expect_err("a Bool is not an Int");
        assert_eq!(error.code.code(), Code::HS1202);
        assert!(
            error.to_diagnostic().message.contains("not a value of Int"),
            "{}",
            error.to_diagnostic().message
        );
        assert_eq!(instance.state(), State::Ready);

        // The one conversion section 9.6 allows: an `Int` for a `Float`
        // parameter, at the top level.
        let outcome = instance
            .call("half", &[Value::Int(5)], &mut Control::none())
            .expect("an Int is accepted for a Float parameter");
        let CallOutcome::Returned(Some(value)) = outcome else {
            panic!("expected a value, got {outcome:?}");
        };
        assert_eq!(instance.render(value), "Float(2.5)");
    }

    /// A host that evaluates filters reads every literal's source and place
    /// before running anything, from a program that need not be valid.
    #[test]
    fn filter_literals_lists_each_literal_with_its_source_and_place() {
        let source = "constant a = `status:open`\nconstant b = `nope:`\nreturn missing\n";
        let found = filter_literals(
            source,
            "main.hws",
            &CapabilityInterface::default(),
            &mut NoLoader,
            &mut Control::none(),
        );
        let seen: Vec<(&str, u32, u32)> = found
            .iter()
            .map(|l| (l.source.as_str(), l.location.line, l.location.column))
            .collect();
        assert_eq!(seen, [("status:open", 1, 14), ("nope:", 2, 14)]);
        assert_eq!(found[0].location.file.as_ref(), "main.hws");
        assert_eq!(
            &source[found[1].location.span.start as usize..found[1].location.span.end as usize],
            "`nope:`"
        );
    }

    /// A cancelled operation lists nothing, as `check` does not check.
    #[test]
    fn filter_literals_honours_a_token_that_is_already_set() {
        let token = CancellationToken::new();
        token.cancel();
        let found = filter_literals(
            "constant a = `status:open`\n",
            "main.hws",
            &CapabilityInterface::default(),
            &mut NoLoader,
            &mut Control::new(Some(&token), None),
        );
        assert!(found.is_empty());
    }
}
