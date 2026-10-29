# How the VM calls capabilities, and how a host binds them

Section 9.6 leaves the binding mechanism to the implementation: the
specification defines what a capability call means, converts, faults on, and
costs, but not how a host's Rust, C, or game-engine code is actually reached
from bytecode. `hollowscript_vm::capability` gives three shapes, covering a
synchronous embedder, a C caller, and a frame-based host that cannot be
re-entered from inside a callback.

## The synchronous path: `HostFn`

```rust
pub trait HostFn {
    fn call(&mut self, args: &[Value], memory_remaining: u64) -> Result<Value, Fault>;
}
```

Any `FnMut(&[Value], u64) -> Result<Value, Fault>` implements this
automatically, so binding the CLI's `print` capability is writing an ordinary
closure that calls `hollowscript_cli::capabilities::print`, not implementing
a trait by hand. `memory_remaining` is `maxMemoryBytes` less the current
allocation counter, passed in on every call so that a capability can refuse
to build a result that would not fit before doing the work of building it,
which is what section 9.6 requires of `fs.read`, for instance ("checks the
size of the file before it reads it").

`CapabilityTable` is a `Vec<Box<dyn HostFn>>`, indexed the same way the
compiled module's `cap_calls` table is: one entry per `(capability, function)`
pair the compiled code actually calls, resolved once when an instance is
created. The interpreter calls through this table alone and never needs to
know whether the boxed closure is talking to standard input,
the filesystem, or a mock used in a test.

## Output a host writes for the VM: `with_output_charged`

Section 11.6 counts the bytes `print` writes against `maxOutputBytes`, and
section 16.2 makes that part of `print`. A `HostFn` that writes the output
itself, as the command line tool's `print` does, charges it with
`HostCall::write_output` before it writes, which is what makes a call over the
limit write nothing. A table whose `print` is a function the interpreter
cannot look inside (the WebAssembly build's, which forwards to JavaScript) has
nowhere to do that, and on the batched path the host is not even called until
after the run has paused.

`CapabilityTable::with_output_charged` marks those functions instead. The
interpreter then charges the text the call's arguments carry and one LF
(`Interpreter::rendered_size`: for the standard `print`'s one `String` that is
its UTF-8 bytes; for any other shape, the sum of what it holds, once for every
path to each part), in `charge_capability_arguments`, which the synchronous path and
the batched path both run before the call is made or queued, and which the
call of a capability held as a value runs too. The charge comes after the
step charge of the same call, as section 11.3's order of faults requires. A
function marked here must not also call `write_output`, or it is charged
twice, so the two ways are for different tables and never mixed.
`hollowscript-wasm` marks every `cap_calls` entry whose capability is named
`print`, whatever shape it declares: a host binds handlers by name alone, so a
`print` declared as taking `[String]` reaches a handler that writes whatever it
is given, and marking only the standard shape let 256 MiB through under an output
limit of 1,000.

## The C boundary

`hollowscript-capi` exposes the same shape behind a C function pointer,
carrying the same argument and result conversion rules; a non-Rust host
implements a capability by supplying a function pointer with a fixed
signature, which the C crate wraps in a `HostFn` the same way a Rust closure
would be. This is why `HostFn` is a trait rather than, for example, a bare
`fn` pointer type: a trait object can be built from either a Rust closure or
a wrapped C function pointer, so the interpreter's calling convention is the
same either way.

## The batched path for a frame-based host

A game engine that drives `update` and `draw` (the lifecycle convention of
section 12.7) typically cannot be re-entered from inside a capability call:
its own frame loop owns the thread, and a synchronous callback into
arbitrary engine state from the middle of a script's execution is often
unsafe or simply architecturally forbidden. For this shape,
`hollowscript_vm::capability::CommandBuffer` lets the interpreter record a
`Command` (which capability function, with which already-converted
arguments) and continue, rather than blocking on the host immediately. The
host drains the buffer at its own frame boundary, performs every queued call
in order, and supplies one `CommandResult` per `Command` before the next
slice of script execution resumes.

This is the mechanism the launch plan's game-layer decisions call for
directly: a standard game library needs "one frame's capability calls to
cross the host boundary once," because the measured per-call cost of the
previous implementation's bridge was significant per call. Batching turns N
per-frame capability calls into one crossing of the boundary, which is a
performance property, not just a convenience; see
[performance.md](performance.md) for what "measured" will mean.

## Suspend and resume

The batched path is wired through, from the interpreter to the embedding
API. `Interpreter::use_command_buffer` puts an interpreter on it, and
`Options::batch_capability_calls` does the same through the facade. On that
path `Op::CallCap` charges its arguments exactly as the synchronous path
does, appends a `Command` to the buffer, and pauses: the frames, the run's
`RunCounters` and the location of the instruction that was running move into
the interpreter, and the entry point answers `Run::Suspended`
(`CallOutcome::Suspended` through the facade, with the instance in
`State::Suspended`).

The host then drains `commands()`, performs the calls in order at its own
frame boundary, and calls `resume` with one `CommandResult` per `Command`.
The resume puts the frames and the counters back, runs the result through
exactly the same acceptance check and charges the synchronous path applies
(section 9.6's HS1013, section 11.3's result charges, section 11.7's
safepoint), writes it into the register the paused call site named, and
carries on from the instruction after it. A pause is not a new run: section
11.2's "what a run is" and section 11.3's charging hold across it because
the counters cross it untouched.

Because a run pauses on the call it needs the result of, the buffer holds
one command per pause today. The batched contract is one result per queued
command, in order, so that a later change that lets a call whose result is
discarded queue and continue costs the host nothing: it is already written
to drain a list.

What a host gets wrong is reported, not assumed away. The wrong number of
results is HS1012 naming the capability and both counts; a result of the
wrong type is the same HS1013 the synchronous path raises; a
`CommandResult::Err` is the host's own fault, located at the call that
raised it. Resuming an interpreter that is not paused, starting a run while
one is paused, and calling a synchronous entry point on the batched path are
all HS1099, since each is a defect in the host's own driving of the loop
rather than anything a script did.

A load can pause too. `Interpreter::resume` remembers which entry point the
paused run started in, so a resume finishes it the way that entry point
would have: the remaining module initialisers for a load, the call depth and
the retained-memory check for an exported call. Through the facade a resume
that finishes the load settles the instance Ready, or Finished with the
entry module's top-level `return` value, exactly as an unpaused load would.

## A host function builds an enum value through the module's own table

An enum value names its variant by an index into the *module's* variant
table, so a host, which has neither that index nor any way to invent one,
could not once build a `Result<_, IoError>`: every standard capability of
chapter 16 except `print`, `clock`, `random`, `args` and `env` returns one,
so none of them could be bound at all.

The capability interface's own types close it. `hollowscript::hosttypes`
runs between compiling and verifying a program and puts every record shape
and every enum variant a capability's declared type mentions into the
module's tables, whether or not the program itself writes them, and hands
the positions to the instance as a `HostTypes`. `HostCall` then names a
variant the way a host writes it, `call.enum_value("IoError", "NotFound",
vec![message])` or the `ok` and `err` shorthands, and looks the position up.
That is why `hollowscript-cli` can bind `fs` and `stdin`.
## What is not decided yet

**A host function cannot build an enum value.** `HostFn` is handed the heap,
which can hold an `Object::Enum`, so building one looks possible. It is not:
an enum value names its variant by an index into the *module's* variant
table, and a host has neither that index nor any way to add an entry for a
variant the program itself never writes. Every standard capability of
chapter 16 except `print`, `clock`, `random`, `args` and `env` returns a
`Result<_, IoError>`, so none of them can be bound, and
`hollowscript-cli`'s `--allow fs` and `--allow stdin` refuse with that
reason rather than pretending. Closing it means carrying the capability
interface's own `TransferType::Enum` through to the compiler so that the
variants a capability's declared type mentions are in the module's table
whether or not the program writes them, and giving `HostFn` a way to name
one. Nothing about the three batched types above depends on it; the two are
independent pieces of work.
