# Diagnostics: the registry, its specification cross-check, ordering and the cap

## A typed registry a wrong code cannot be written against

`hollowscript_core::codes::Code` is not a bare integer: it is a set of
associated constants (`Code::HS0301`, `Code::HS1007`, and so on) built by
one macro from one table, so a caller writes `Code::HS0301` and the compiler
rejects a code that does not exist, rather than accepting an arbitrary
`u16`. Every entry in the table carries its severity, the stages that report
it, its message template, and what its span covers, exactly as chapter 17's
tables state them; `Code::info()` and `Code::family()` read this table rather
than switching on the number by hand anywhere else in the workspace.

## The specification cross-check

`hollowscript-core/tests/spec_registry.rs` parses `docs/spec/17-diagnostics.md`
directly (the tables of sections 17.2 to 17.4, and the ranges of section
17.5) and asserts that the hand-written registry and the specification text
agree on every code, in both directions: a code in the specification but
missing from the registry fails the test, and so does a code in the registry
that the specification does not define. This is deliberately not a snapshot
or a generated file: the registry is written by hand in
`hollowscript_core::codes`, and the test is what keeps it from silently
drifting as the specification is edited, without needing a code generation
step in the build. The same file checks that every code's family (its
hundred-number range, per section 17.5) matches the range its severity
implies (only the warning range contains warnings), and that `Code::parse`
and `Code::to_text` round-trip every registry entry.

The test can run without the specification present (for a build that does
not carry `docs/spec/`, such as a released package) by skipping with a
message; setting `HOLLOWSCRIPT_REQUIRE_SPEC=1`, which `scripts/check` does,
turns a missing specification into a hard failure instead, so a normal local
check always runs the cross-check.

The same technique is used for two more specification tables:
`hollowscript-syntax/tests/spec_keywords.rs` cross-checks the keyword table
of `hollowscript_syntax::token::Keyword` against both lists in the
specification (sections 1.3 and 18.2), and
`hollowscript-core/tests/spec_natives.rs` cross-checks the standard library
roster (`hollowscript_core::native::ROSTER`) against every signature table
of chapters 8 and 13 to 15, in both directions.

## Message templates

A template's placeholders (`{name}`, `{found}`, and so on) are filled by
`hollowscript_core::diagnostic::render_template`, which recognises a
placeholder as a lower-case identifier in braces and leaves anything else
(the literal `{...}` inside HS0301's own message, or a placeholder no
argument was given for) untouched rather than guessing. `placeholders()`
lists a template's own placeholder names, which is what a future
implementer would use to check, mechanically, that every code's call site
supplies the arguments its template expects.

## Canonical ordering and the cap

`hollowscript_core::diagnostic::DiagnosticSet` collects diagnostics as they
are found and produces them, on `finish`, in exactly the order section 17.1
specifies: by file, in the load order the caller supplies (the entry module
first, then each import depth first, in the order of its `import` lines,
matching section 10.5's own loading order); within one file, by the start of
the span, then its end, then errors before warnings, then the code itself.
Diagnostics with no location (the host errors, HS1201 to HS1205) sort after
every file's diagnostics, because they answer a host request rather than
describing a position in a program.

The cap of section 17.1 (at most 100 diagnostics of any severity for one
file, followed by exactly one HS0004 and no more from that file) is applied
during the same pass that establishes the order, per file, with the HS0004
notice placed at the empty span at the start of the first diagnostic that
was dropped, matching "the start of the first suppressed diagnostic." Both
the ordering and the cap are unit tested directly, including the boundary
case of exactly 100 diagnostics (no notice) against 101 or more (exactly one
notice, and no more than 100 of the original diagnostics kept).

### The cap is applied as diagnostics are found

The cap is not only a rule about what is reported, it is what keeps a source
that provokes a diagnostic for nearly every byte from holding them all: a
3.9 MiB file of `"` and a line feed is two diagnostics a line, four million of
them, and collecting every one before capping cost 3.7 GB. `BoundedDiagnostics`
holds, for each file, the smallest 101 of what it has been given in the order
above (the first 100 and the one whose position the notice takes), sorting and
cutting a file back whenever its pile reaches 202, so the memory for a file is
fixed and what `finish` answers is exactly what capping at the end would have
given, whatever order the diagnostics were found in. It also remembers whether
any diagnostic it dropped was an error. `DiagnosticSet` is one of these, and so
are the places a stage collects its own before handing them on: the lexer and
the parser (together, for one file), the resolver, the `uses` check and the type
checker, each of which therefore returns at most 101 for a file. The type
checker sometimes checks an expression only to learn a fact about it and throws
the diagnostics away (`begin_speculation` and `discard_speculation`); those are
held apart, uncapped, until the speculation ends, and are as many as the
expression itself can provoke. `cap_diagnostics` applies the same rule to a list
a caller already has, as the formatter does with the syntax errors that made it
refuse. A diagnostic without a location is not capped; only host errors have
none.

What this does not bound is the rest of the front end's memory for such a file:
the tree the parser builds for the 2,000,000 statements of that file is about 0.7
KiB each, and the checker adds as much again, so the 3.9 MiB file still takes
about 2 GB to check (3.7 GB before the cap), and 740 MiB of the module's memory
in the browser build for a million lines. That is linear in the size of the
source, which the structural limits bound at 4 MiB for a file, not in what the
source says; see `docs/design/security.md`.

## What is not decided yet

`DiagnosticSet` orders and caps whatever diagnostics it is given; it has no
opinion on how a caller assembles the correct set for one file. That is the
checker's job (module loading order, section 10.5), and it does it: the
facade hands `DiagnosticSet::finish` the modules in load order with the
entry first.

A fault carries the location of the operation that failed (file, byte span,
line and column), taken from the span table of the bytecode, so it is
absent only for bytecode built with its positions stripped. What a fault
does not carry yet is its call frames: `Fault::frames` exists, as section
17.1's record has it, but nothing in the interpreter fills it in, so a
fault's `frames` are empty.
