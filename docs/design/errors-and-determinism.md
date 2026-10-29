# The error and fault model in the Rust API, and determinism

## Three kinds of failure, kept apart in the type system

The specification already keeps two kinds of failure apart at the language
level (section 8.1): a recoverable error is an ordinary `Result` value a
script can inspect, and a fault aborts the run and cannot be caught. The
native implementation's Rust API adds a third, host-level kind, and gives
each its own type rather than folding them into one generic error:

- **A recoverable error** is a value of the language's own `Result<T, E>`,
  built and matched by script code; the Rust API never represents this
  itself; it is a `hollowscript_vm::Value` like any other value.
- **A fault** is `hollowscript_core::fault::Fault`, carrying a
  `FaultCode` (one variant per HS1001 to HS1099), a message, the location of
  the operation that faulted, up to 32 call frames, and, for the five limit
  faults, the `LimitData` section 17.1 requires (which option, its
  configured limit, and the counter value at the fault). Every constructor
  (`Fault::step_limit`, `Fault::division_by_zero` and so on, in
  `hollowscript_core::fault`) fills in the message from the same template
  the diagnostic registry uses, so a fault's wording and a checker
  diagnostic's wording for the same code can never drift apart.
- **A host error** is `hollowscript_core::fault::HostError`, one of the five
  codes of section 12.6 (HS1201 to HS1205), which answer a request the host
  itself made incorrectly (an unknown export, the wrong number of arguments,
  a call while another is in progress, an operation on a Finished or
  Disposed instance) and, per the specification, are "not source
  diagnostics: they have no file, span, line or column." Keeping this as its
  own type, rather than a `Fault` with an empty location, makes the
  distinction the type checker enforces rather than a convention a caller
  has to remember: `Instance::call` returns `Result<CallOutcome, HostError>`,
  and only `CallOutcome::Faulted` carries an actual `Fault`, so a caller
  cannot mix up "your request was invalid" with "the script's own run went wrong."

`FaultCode::class()` gives the resource, domain, host or internal
classification of section 8.1 ("the classes exist for hosts that report or
count faults differently"), computed from the code rather than stored
separately, so the classification can never disagree with the code it
describes.

## Never a panic

Every one of these types is returned, never produced by unwinding. The
workspace-wide lint policy (see [architecture.md](architecture.md)) denies
`unwrap`, `expect`, `panic!`, `todo!()` and `unimplemented!()` outside test
code, which is the mechanical form of section 19.1's requirement that a
conforming implementation "never crashes, hangs or exhausts its host on any
input." A function that is not implemented yet returns
`Diagnostic::not_implemented` (which is exactly HS1099, "internal error in
the implementation", filled in with what is missing) through whichever
channel its finished version will use: a diagnostic from `check`, a
`CompileError` from the compiler, or a `Fault` from the interpreter. A caller
of an unfinished function always gets a typed value it can match on, never a panic.

## What determinism this implementation promises

Section 19.1 requires "the same outcome the corpus records" from every
conforming implementation, and section 3.2 requires the same evaluation
order, with one stated exception: "the last binary digit of the results of
[`exp`, `ln`, `pow`, `sin`, `cos`, `tan` and `atan2`]." Two implementation
choices in this workspace exist specifically to make that promise
achievable rather than accidental:

- **No non-deterministic collection anywhere an outcome could depend on it.**
  `HashMap` and `HashSet` are denied by a `clippy.toml` rule across the whole
  workspace (with a documented, reasoned exception where a hash map is used
  purely for lookup and never iterated, such as the type interner's
  shape-to-id table). Every map or set whose order a script or a diagnostic
  list could observe is a `BTreeMap` or `BTreeSet`, or, where the
  specification defines a different ascending order (numeric for `Int`,
  code-point for `String`, false-before-true for `Bool`, as section 14.4
  states), a value sorted by that order directly, as
  `hollowscript_vm::heap::Heap::compare_keys` does for map and set keys.
- **A fixed, tested diagnostic order**, independent of Rust's own hashing or
  iteration order, described in full in [diagnostics.md](diagnostics.md).

Byte-for-byte accounting (chapter 11's memory formulas) and the bytecode's
own explicit step charges (see [limits-and-cancellation.md](limits-and-cancellation.md))
serve the same goal from a different angle: they make a resource fault a
property of the specification's own formulas, computed identically
regardless of how this implementation's heap happens to lay values out in
real memory, rather than a property of this implementation's internals leaking through.

## What is not yet exercised

Determinism across two *different* implementations (this one and any other
built from the specification) can only be checked once both exist and are
run against the shared corpus (section 19.7); nothing in this workspace runs
that comparison today, because there is, at the time of writing, only this
implementation. `hollowscript-conformance` runs this implementation
against the corpus now, comparing all four stages of every case against the
case's own expected file; comparing its output against a second
implementation is future work outside this workspace's scope.
