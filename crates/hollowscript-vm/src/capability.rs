//! How the VM calls capabilities and how a host binds them (section 9.6).
//!
//! A [`CapabilityTable`] is the run-time counterpart of the checker's
//! `Grants`: one [`HostFn`] per capability function the module's `cap_calls`
//! table refers to, resolved by the embedder once when an instance is
//! created. The interpreter calls through this trait alone and never
//! inspects a host's own types.
//!
//! Three shapes cover every host, from a single Rust closure to a frame-based
//! game engine:
//!
//! - [`HostFn`]: an ordinary Rust closure or function pointer, for an embedder
//!   that can call back into its own state synchronously (the CLI's `std::io` capabilities).
//! - The same trait boxed behind a C function pointer is how
//!   `hollowscript-capi` exposes it to a host that is not Rust.
//! - [`CommandBuffer`]: for a frame-based host (a game engine that must not
//!   be re-entered from arbitrary points), the interpreter instead appends a
//!   [`Command`] and yields; the host drains the buffer between frames and
//!   supplies the results before the next call, so a whole frame's
//!   capability calls cross the boundary once, batched.

use crate::hostvalues::{HostCall, HostTypes};
use crate::value::Value;
use hollowscript_core::cancel::CancellationToken;
use hollowscript_core::fault::Fault;

/// A capability function the VM can call synchronously.
///
/// The [`HostCall`] is everything the function needs beyond its arguments: a
/// `String`, list, map, set, record or enum value travels as a
/// [`Value::Heap`] handle, never as its own Rust value, so a host can
/// neither read nor build one without it. It also carries the record and
/// enum names the host declared, which is what lets a fallible capability
/// return `Ok(value)` or `Err(IoError.NotFound(message))`, and
/// `maxMemoryBytes` less the allocation counter (section 9.6), so that a
/// host can refuse to build a result that would not fit before it does the
/// work of building it.
pub trait HostFn {
    /// Calls the function with `args`, already converted to the parameter
    /// types (section 9.6). Returns the result value, or a fault: HS1012 if
    /// the host reports its own failure, HS1013 if it returns a value that
    /// does not fit the declared result type (checked by the caller, not this trait).
    ///
    /// # Errors
    ///
    /// Returns a [`Fault`] exactly when the specification says the call
    /// faults: see the code's own documentation.
    fn call(&mut self, args: &[Value], call: &mut HostCall<'_>) -> Result<Value, Fault>;
}

impl<F: FnMut(&[Value], &mut HostCall<'_>) -> Result<Value, Fault>> HostFn for F {
    fn call(&mut self, args: &[Value], call: &mut HostCall<'_>) -> Result<Value, Fault> {
        self(args, call)
    }
}

/// The capability functions bound for one instance, indexed the same way as
/// the module's `cap_calls` table (one entry per `(capability, function)` pair
/// that the compiled code actually calls).
pub struct CapabilityTable {
    functions: Vec<Box<dyn HostFn>>,
    types: HostTypes,
    /// The `cap_calls` indices of the functions that write output
    /// (section 11.6), sorted: see [`CapabilityTable::with_output_charged`].
    output_charged: Vec<u32>,
}

impl std::fmt::Debug for CapabilityTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityTable")
            .field("len", &self.functions.len())
            .field("types", &self.types)
            .field("output_charged", &self.output_charged)
            .finish()
    }
}

impl CapabilityTable {
    /// A table with these functions, in the order the module's `cap_calls`
    /// expects, and no host types: for a host whose capabilities return only
    /// scalars, strings and collections.
    #[must_use]
    pub fn new(functions: Vec<Box<dyn HostFn>>) -> CapabilityTable {
        CapabilityTable::with_types(functions, HostTypes::new())
    }

    /// The same, with the record and enum positions a host may build
    /// (see [`crate::hostvalues`]).
    #[must_use]
    pub fn with_types(functions: Vec<Box<dyn HostFn>>, types: HostTypes) -> CapabilityTable {
        CapabilityTable {
            functions,
            types,
            output_charged: Vec::new(),
        }
    }

    /// Marks the functions at these indices (positions in the module's
    /// `cap_calls`, as [`CapabilityTable::call`] takes them) as the
    /// `print` (section 16.2), whose output counts against `maxOutputBytes`
    /// (section 11.6): the interpreter charges the text the arguments carry,
    /// which for the standard `print`'s one `String` is its UTF-8 bytes, and one
    /// LF byte before the call, after the call's step charge, and faults with
    /// HS1004, without calling the function, if that would take the run past
    /// the limit. A function of another shape is charged by the text its
    /// arguments carry (a list of strings by the sum of its elements, each as
    /// often as the list holds it).
    ///
    /// This is for a table whose `print` is a function the interpreter cannot
    /// look inside, such as one that forwards to a host outside Rust, and is
    /// on both the synchronous path and the batched one, where the host does
    /// not see the call until after the charge. A function that writes the
    /// output itself, as the command line tool's `print` does, charges it with
    /// [`HostCall::write_output`] and must not also be marked here, or it is
    /// charged twice.
    #[must_use]
    pub fn with_output_charged(
        mut self,
        indices: impl IntoIterator<Item = u32>,
    ) -> CapabilityTable {
        self.output_charged = indices.into_iter().collect();
        self.output_charged.sort_unstable();
        self.output_charged.dedup();
        self
    }

    /// Whether the function at `index` is one marked by
    /// [`CapabilityTable::with_output_charged`].
    #[must_use]
    pub fn charges_output(&self, index: u32) -> bool {
        self.output_charged.binary_search(&index).is_ok()
    }

    /// The record and enum positions a host may build.
    #[must_use]
    pub fn host_types(&self) -> &HostTypes {
        &self.types
    }

    /// Calls the function at `index`. `cancel` is the run's own
    /// cancellation token, if it has one, so a capability whose work spans
    /// more than one step (`fs.read` reading a large file in chunks,
    /// section 16.8) can poll it through [`HostCall::is_cancelled`] without
    /// waiting for the interpreter's own safepoint, which only runs once
    /// the call returns.
    ///
    /// # Errors
    ///
    /// Returns HS1099 if `index` is out of range, which is a defect of the
    /// module (the verifier should have refused it), or whatever the function
    /// itself faults with.
    pub fn call(
        &mut self,
        index: u32,
        args: &[Value],
        heap: &mut crate::heap::Heap,
        counters: &mut crate::limits::RunCounters,
        limits: &hollowscript_core::Limits,
        cancel: Option<CancellationToken>,
    ) -> Result<Value, Fault> {
        match self.functions.get_mut(index as usize) {
            Some(f) => {
                let mut call = HostCall::new(heap, &self.types, counters, limits, cancel);
                f.call(args, &mut call)
            }
            None => Err(Fault::internal("a capability call index is out of range")),
        }
    }
}

/// One capability call recorded for later, for the [`CommandBuffer`] path.
///
/// A host on that path drains these with
/// [`crate::interp::Interpreter::commands`] when a run answers
/// [`crate::interp::Run::Suspended`], performs them in order at its own
/// frame boundary, and continues the run with
/// [`crate::interp::Interpreter::resume`], giving one [`CommandResult`] per
/// [`Command`] in the same order.
#[derive(Debug)]
pub struct Command {
    /// The index into the module's `cap_calls`, as [`CapabilityTable::call`] takes.
    pub function: u32,
    /// The arguments, already converted.
    pub args: Vec<Value>,
}

/// The outcome the host supplies for one queued [`Command`], in the order the
/// commands were queued.
#[derive(Debug)]
pub enum CommandResult {
    /// The call succeeded with this value.
    Ok(Value),
    /// The call faulted.
    Err(Fault),
}

/// A batch of capability calls queued during one slice of execution, for a
/// host that must not be re-entered until its own frame boundary (a game
/// engine driving `update` or `draw`). The interpreter appends to this and
/// yields; the host drains it, performs the calls in order at its own frame
/// boundary, and hands back one [`CommandResult`] per [`Command`] before the
/// next slice of the run resumes.
#[derive(Debug, Default)]
pub struct CommandBuffer {
    queued: Vec<Command>,
}

impl CommandBuffer {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> CommandBuffer {
        CommandBuffer::default()
    }

    /// Queues a call.
    pub fn push(&mut self, command: Command) {
        self.queued.push(command);
    }

    /// The queued calls, in order, removing them from the buffer.
    pub fn drain(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.queued)
    }

    /// Whether anything is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    /// How many calls are queued, which is how many results a resume needs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queued.len()
    }
}

#[cfg(test)]
mod tests {
    use super::HostCall;
    use super::{CapabilityTable, Command, CommandBuffer};
    use crate::value::Value;
    use hollowscript_core::fault::Fault;

    #[test]
    fn a_closure_is_a_host_fn() {
        let mut table = CapabilityTable::new(vec![Box::new(
            |args: &[Value], _: &mut HostCall<'_>| match args {
                [Value::Int(n)] => Ok(Value::Int(n * 2)),
                _ => Err(Fault::internal("wrong arity")),
            },
        )]);
        let mut heap = crate::heap::Heap::new();
        let mut counters = crate::limits::RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let result = table
            .call(
                0,
                &[Value::Int(21)],
                &mut heap,
                &mut counters,
                &limits,
                None,
            )
            .unwrap();
        assert!(matches!(result, Value::Int(42)));
        assert!(
            table
                .call(1, &[], &mut heap, &mut counters, &limits, None)
                .is_err()
        );
    }

    #[test]
    fn a_command_buffer_batches_and_drains_in_order() {
        let mut buf = CommandBuffer::new();
        assert!(buf.is_empty());
        buf.push(Command {
            function: 0,
            args: vec![Value::Int(1)],
        });
        buf.push(Command {
            function: 1,
            args: vec![],
        });
        let drained = buf.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].function, 0);
        assert!(buf.is_empty());
    }
}
