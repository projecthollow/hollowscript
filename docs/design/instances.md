# Instances and host-callable functions

Chapter 12 defines a host operation contract (`check`, `load`, `call`,
`read`, `result`, `cancel`, `dispose`) and a state machine
(Ready, Running, Finished, Faulted, Disposed) that every implementation's
embedding surface must present, whatever language it is written in. The
`hollowscript` facade crate is where the native implementation gives that
contract a concrete Rust shape.

## The state machine

`hollowscript::instance::State` is exactly the five states of section 12.2.
`hollowscript::instance::Instance` owns one loaded program's interpreter and
enforces which operations are allowed in which state, returning the host
error of section 12.6 for everything else:

- `call` requires Ready; on any other state it returns the matching host
  error (HS1203 Faulted, HS1204 Busy, HS1205 for Finished or Disposed) and
  leaves the instance exactly as it was, per the table in section 12.6.
- Within a Ready instance, `call` first distinguishes "no such export"
  (HS1201) from "wrong arity" (HS1202): the export table is searched by name
  first, and only once a function of that name is found is its parameter
  count compared against the arguments given, so a name that does not exist
  at all is never reported as a wrong-arity call.
- A fault during a call moves the instance to Faulted and the fault is kept
  (`Instance::fault`), retained and returned as many times as asked, matching
  "on a Faulted instance it returns the fault, as often as asked."
- `read` requires Ready and looks the name up in the same export table, as
  an exported constant rather than an exported function.
- `dispose` succeeds from any state except Running (HS1204) and Disposed
  itself (HS1205), and is otherwise idempotent to call once.

This state machine does not depend on the interpreter actually running
anything: it is exercised directly, by constructing an `Instance` around a
small hand-built verified module (the same technique `hollowscript-bytecode`'s
own verifier tests use), so every state transition and host error is tested
today, independently of whether the interpreter behind it is finished.

## `check` and `load`

`hollowscript::check` and `hollowscript::load` are the two host operations
that do not take an existing instance. `check` parses and checks a program
and returns its diagnostics in the canonical order, without creating
anything; `load` runs `check` first (per section 12.3's own ordering: "check,
then, if there are no errors, bind and initialise"), and only if there are no
errors does it go on to compile, verify, construct an interpreter and run
its initialisers.

`load`'s return type, `LoadOutcome`, mirrors section 12.3's own two
outcomes directly: `CheckFailed` (a `Vec<Diagnostic>`, no instance) or
`Instance` (boxed, to keep the enum small), whose own state then tells the
caller whether initialisation reached the end (Ready), returned early
(Finished, with a value available through `Instance::result`), or faulted
(Faulted, with the fault available through `Instance::fault`).

## What is real today, and what is not

`load` runs the whole pipeline: parse, check (resolving imports through the
host's loader), compile, verify, and initialise. A clean program produces a
Ready or Finished instance and its load result; a program with an error
produces `CheckFailed` and no instance, as section 12.3 requires.
`Instance::render` turns a value into text, which is how a host shows a load
result or an exported constant.

Two things a host can reach for are not there yet. `Instance::call` and
`Instance::read` work against `Module::exports`, which the compiler does not
populate: an export's signature needs a `TypeId` converted to a
`hollowscript_core::transfer::TransferType`, and nothing does that
conversion, so a compiled program has no exports for a host to name. And a
capability whose result type is an enum cannot be bound at all: a
`hollowscript_vm::HostFn` is handed the heap, which can hold an
`Object::Enum`, but an enum value names its variant by an index into the
module's own variant table, and nothing gives a host that index or puts an
entry there for a variant the program itself never writes. That is what
stops the standard `fs`, `stdin` and `http` capabilities, every one of whose
functions returns a `Result<_, IoError>`, from being granted. Closing it
means carrying the capability interface's own `TransferType::Enum` into the
compiler, which the HIR does not do today.

## The lifecycle convention (section 12.7)

`load`, `update`, `draw` and event functions are a convention over ordinary
exported functions, not a separate mechanism: a host that wants this shape
calls `Instance::call` with those names in that order, exactly as it would
call any other exported function. Nothing in the facade needs to know about
the convention by name; it exists once the compiler and checker allow a
program to export functions with those names and the right signatures.
