# Architecture: the crate graph

## Goals

- **Parallel ownership.** Each crate has one clear owner and a fixed public
  interface, so several people can implement their crate's insides without
  coordinating on anything but the types at the boundary.
- **A small, auditable core.** `hollowscript-syntax`, `hollowscript-check`,
  `hollowscript-compiler` and `hollowscript-vm` depend on nothing outside the
  workspace. A person auditing the interpreter that runs untrusted code reads
  four crates and the standard library, nothing else.
- **The browser gets only the VM.** `hollowscript-wasm` cannot depend on the
  parser or the checker even by accident, because the dependency graph does
  not offer them (the opt-in `tooling` build, which an editor page loads and a
  published game never does, is the one exception, below). A published game
  bundles bytecode compiled at publish time; the browser never holds source.
- **Unsafe code is contained.** Only `hollowscript-capi` and `hollowscript-wasm`
  may use it, for exactly the boundary where a foreign caller can only be met
  at a raw pointer or a JavaScript value. Every other crate is safe Rust from
  top to bottom, checked by `#![forbid(unsafe_code)]` at its root.

## The graph

```text
hollowscript-core        (no workspace dependencies)
  |
  +-- hollowscript-bytecode
  |     |
  |     +-- hollowscript-vm
  |     |     |
  |     |     +-- hollowscript-wasm
  |     |
  |     +-- hollowscript-compiler --+
  |                                 |
  +-- hollowscript-syntax           |
  |     |                           |
  |     +-- hollowscript-check -----+
  |           |                     |
  |           +---------------------+
  |                                 |
  +-------- hollowscript -----------+   (facade: syntax, check, compiler, bytecode, vm, core)
              |         |
              |         +-- hollowscript-capi
              |         +-- hollowscript-conformance (dev only)
              |
              +-- hollowscript-cli (also depends on hollowscript-syntax, for `format`,
                                    and on hollowscript-hostfs)

hollowscript-hostfs       (how a host opens a file it has a name for; no workspace crate)

hollowscript-ide          (core, syntax, check; no transport, no disk)
  |
  +-- hollowscript-lsp    (also core, syntax, check; independent of the facade)

xtask                     (dev only; depends on hollowscript, for its bench subcommand)
```

The rule this diagram encodes is checked by machine, not left to review
discipline: `cargo xtask graph` reads the dependency graph with `cargo tree`
and fails if any crate depends on something this document does not list for
it. `scripts/check` runs it alongside the compiler and the tests.

## The crates, and why each boundary is where it is

### `hollowscript-core`

Shared vocabulary: source text and spans with line and column mapping, the
diagnostic record and the full registry of chapter 17 (cross-checked against
the specification text by a test), resource and structural limits, typed
faults and host errors, cancellation tokens and the safepoint rule, interned
symbols, and the descriptions of the transferable types that cross a host
boundary. It depends on nothing but the standard library, which is what lets
every other crate, including the browser build, share one definition of a
diagnostic and one definition of what `maxSteps` means.

It also holds the filter language's data types (`filter.rs`) and the roster
of native library functions (`native.rs`), because both are shared between
the checker (which types them), the compiler (which resolves calls against
the roster) and the VM (which implements them): putting them in one crate
with no dependencies keeps three crates from drifting on what `list.map`'s
arity is.

### `hollowscript-syntax`

Source text to a syntax tree: tokens, the lexer, the AST (defined in full,
see [ast-and-hir.md](ast-and-hir.md)), a parser with error recovery, and the
canonical formatter. No semantic knowledge lives here: the parser does not
know what a valid capability name is, only what the grammar allows.

### `hollowscript-check`

Name resolution, types and inference, optional narrowing, effects and
capability grants, and pattern exhaustiveness, over the syntax tree, producing
the typed HIR (see [ast-and-hir.md](ast-and-hir.md)) and diagnostics. This is
the largest single piece of analysis and the one most likely to need internal
restructuring as it is written; nothing outside this crate depends on its
internals, only on the HIR shape it produces, which is fixed by
`hollowscript-check::hir`.

### `hollowscript-compiler`

HIR to bytecode. Depends on `hollowscript-check` (to read the HIR) and
`hollowscript-bytecode` (to write the container format), and on nothing else:
it does not run anything, so it needs no heap and no capability model of its
own.

### `hollowscript-bytecode`

The instruction set, the container format, and the verifier (see
[bytecode.md](bytecode.md)). This is a separate crate from both the compiler
and the VM because both must agree on it independently: the compiler proves a
program compiles to valid bytecode, and the VM proves it only runs bytecode
that has passed the verifier, and those are different guarantees that should
not share one crate's assumptions.

### `hollowscript-vm`

Values, the heap, the interpreter, resource limits, cancellation, the
capability calling convention, and the standard library of chapters 13 to 15
(see [values-and-heap.md](values-and-heap.md),
[limits-and-cancellation.md](limits-and-cancellation.md) and
[capabilities.md](capabilities.md)). Depends on `hollowscript-bytecode` (to
read a `VerifiedModule`) and nothing else in the workspace, which is what
lets `hollowscript-wasm` reuse it unchanged.

The library functions live here, in `natives`, rather than in a crate of
their own, because they are the behaviour of instructions the interpreter
executes: `Op::CallNative` is as much a part of the machine as `Op::AddInt`.
`hollowscript-core`'s roster is the shared list of names and arities that
the checker, the compiler and the VM all key on; `natives` is the only place
their behaviour is defined. `render` turns a value into the text the
conformance corpus fixes for a result, which is also what a host shows for a
load result or an exported constant.

### `hollowscript` (the facade)

The public API most Rust users see: `check`, `load`, `call`, `read`,
`cancel`, `dispose`, with `Options`, grants and limits (see
[instances.md](instances.md)). It is the only crate that depends on the
whole pipeline (syntax, check, compiler, bytecode, vm), because orchestrating
them in the order chapter 12 specifies is its entire job.

### `hollowscript-capi`

A small, stable C ABI over opaque handles, for a host that is not Rust. One
of the two crates unsafe code is allowed in.

### `hollowscript-cli`

The `hollowscript` command: `run`, `check`, `format`, `build`, `lsp`, a
`--json` mode for tooling (an interactive `repl` is planned, not
implemented), and the standard capabilities implemented for a
host process (see [capabilities.md](capabilities.md)). It depends on
`hollowscript-syntax` directly, in addition to the facade, only for
`format`, which runs the formatter without needing a full check. It is the
one crate in the workspace with third-party dependencies of its own (the
others that link third-party code, `hollowscript-hostfs` and, through it,
`hollowscript-lsp`, are host-side too): `cap-std`, needed
to give the `fs` capability's scope check the resolve-and-use-in-one-step
handle section 16.8 requires, and `ureq` with `rustls`, needed to implement
the `http` capability's requests, redirects and address safety (section
16.9); see [dependencies.md](dependencies.md). The rest of the workspace
stays free of external dependencies. `lsp` starts the language server, but
as a child process, not an in-process call: `hollowscript-cli` does not
depend on `hollowscript-lsp`'s crate (the next section, and `xtask graph`,
which checks this edge is never granted).

### `hollowscript-hostfs`

How a host opens a file it has only a name for, shared by the command line
host and the language server so that they cannot disagree: relative to a
`cap-std` directory handle, never through a symbolic link, only a regular
file with one name, opened without waiting on a named pipe, and read through a
limit. It also serves a program's modules from a directory per section 10.3
(`RootDir`). It depends on nothing in the workspace, and on `cap-std` (and,
on Unix, `libc`) outside it; see [dependencies.md](dependencies.md) and
[security.md](security.md).

### `hollowscript-ide`

The editor services a language server and a browser editor share, over
`hollowscript-syntax` and `hollowscript-check` only: no transport, no disk,
no state between calls, and no third-party dependency (it is on the list of
crates `cargo xtask graph` holds to that). It does not depend on the
facade, the compiler or the VM, because an editor never runs a program.

Its one input is a `Workspace`: the text of an entry module, checked
against a caller-supplied `CapabilityInterface`, with every `import`
resolved through a caller-supplied loader (a function from canonical path
to source text) or an in-memory map of modules (`Workspace::from_modules`).
The caller decides what a module is and where it comes from: the language
server answers from open documents and then disk, a browser editor from the
files of a project it holds in memory. The crate reads nothing itself.

The checker's own HIR resolves a name used through an import to the
exporting module's declaration, which is what makes hover and diagnostics
correct across files. But a source-level feature (go to definition landing
on the declaration's own span, an outline, a rename) needs the *syntax* of
the file the name is declared in, and the checker does not hand back the
trees of the modules it loaded, only their typed HIR. So `Workspace` parses
the entry module's own direct imports a second time, the same way the
checker resolved them, and keeps their trees and text alongside the checked
program. Only direct imports are parsed this way: every language feature a
name can be used through is a direct import (section 10.2 gives a module no
way to re-export a name it did not declare itself), so this is complete for
what a program can actually write.

Every position is a half-open range of UTF-8 byte offsets into one module's
text, the unit of `hollowscript_core::Span`; a consumer that counts lines
and characters, or UTF-16 code units, converts at its own edge, and `text`
holds the conversion between bytes and UTF-16 (a `Utf16Map` that records only
a text's non-ASCII characters). The services built on it are:

- the binders and occurrences of names (`symbols`), each binder carrying its
  region, the offset it comes into view at, whether a function body sees it
  wherever it is declared (the hoisted top-level names of section 4.6), its
  kind, and a signature or type to show;
- the type of the expression under a position (`types`), found from the
  checker's own spans and only for the entry module, since the nodes of
  another module are numbered from the start of its own file;
- completion (`complete`): the names in scope at a position in declaration
  order, then the prelude and the keywords a program can write today, and
  nothing in a comment, in the text of a string or after a `.` (member
  completion needs the type of the receiver and is not done);
- go to definition, from the binders, for a name declared here or brought in
  by an import;
- the outline and the foldable spans (`outline`), built from the syntax tree
  alone so they work on a file that does not check yet, with `uses` and
  `import` items, an `exported` flag and a detail beside each item;
- semantic tokens (`semantic`), every identifier resolved to a declaration,
  and lexical highlighting (`highlight`), every token and comment classified
  from the lexer alone, which needs no parse and no check;
- the keyword and prelude vocabularies (`vocabulary`), whose keyword list is
  `Keyword::ALL`, so a word the language adds cannot be forgotten.

### `hollowscript-lsp`

A language server over `hollowscript-ide`, speaking the Language Server
Protocol over standard input and output with no third-party dependency of
its own (the crate has its own minimal JSON value, parser and writer, and its
own message framing; the one third-party code it links comes through
`hollowscript-hostfs`, which it reads files with). It is a thin adapter: the
protocol, the open documents, and the disk. The analysis is `hollowscript-ide`'s. It does not depend on the
facade, the compiler or the VM, because a language server never runs a
program: it only ever needs source-level analysis.

Every position-based request runs against a `Workspace`
(`src/workspace.rs`): the document's own text, checked with its imports
loaded for real, from an open document first and then disk, joined onto
the document's own directory the same way `hollowscript-cli`'s loader
roots a program (section 10.3), by the same code (`hollowscript-hostfs`): no
symbolic link is followed, only a regular file with one name is read, a named
pipe is refused rather than waited on, and the read is bounded by the
language's own source limits. The adapter supplies what the analysis
leaves to its caller: the entry module's name and directory from the
document's URI, a loader that prefers open documents over disk, and the
mapping from a module's canonical name back to the URI a client knows it
by. This is what lets hover, go to definition, completion, find references
and rename resolve a name declared in another file, not only the one open
in the editor. The server checks against the default, empty capability
interface: it knows nothing of the host a program will run in.

Implemented, all against the real checker and syntax tree: `initialize`,
document sync (incremental: `src/sync.rs` patches the stored text at an
edited range), diagnostics, formatting, hover, go to definition,
completion, find references, rename and prepare rename (refusing a
keyword or a prelude name), signature help, an outline (document symbols
and folding ranges, built from the syntax tree alone so they work on a
file that does not check yet), semantic tokens (every identifier this
server already resolves to a declaration; a keyword, a literal and a
comment are left to a client's own TextMate grammar), and code actions for
three diagnostics with one unambiguous fix each (HS0401 add `effect`,
HS0205 add a missing `uses` line for one of the eight standard
capabilities, HS0801/HS0802 remove a single unused name). Positions on the
wire are counted in code points, which agrees with the protocol's UTF-16
for everything but a character outside the basic multilingual plane.

### `editors/vscode`

A thin VS Code client outside the Rust workspace entirely (its own
`package.json`, not a Cargo crate): a TextMate grammar derived from the
lexical grammar of [the specification](../spec/01-lexical-structure.md),
a language configuration, and an extension that spawns `hollowscript lsp`
with `vscode-languageclient` and implements no language feature itself.
See `editors/vscode/THIRD-PARTY-LICENSES.md` for its one runtime npm
dependency and what it in turn pulls in. It replaces an old private
extension, which must never be published; this one is not published
either, only built and packaged locally.

### `hollowscript-wasm`

A WebAssembly build of the VM only: it loads and runs verified bytecode and
depends on `hollowscript-core`, `hollowscript-bytecode` and
`hollowscript-vm`, nothing else. The other crate unsafe code is allowed in,
for the JavaScript boundary.

The optional `tooling` feature is a separate, larger build for an editor
page, never what a published game ships. It adds the facade (check, format,
compile) and `hollowscript-ide`, and exports the editor services of a browser
editor through `src/abi_ide.rs`: stateless highlighting, keywords and the
standard capability declarations, and an analysis handle that holds a checked
`Workspace` so that hover, completion, definition and the outline do not
re-check on every call. The handle follows the idiom of the VM's instance
handles: the page holds a number from a registry, not a pointer, a number the
registry does not know is refused, a number is never reused, and disposing
twice is harmless; the registry also caps how many analyses may be held, since
each keeps a checked program in memory. This crate converts every offset to
and from UTF-16 at its boundary, since that is what a web editor counts in.
`cargo xtask graph` reads the default features, so it sees none of these
optional edges; `scripts/check` builds, lints and tests the feature
separately.

Values and faults cross that boundary as text, in either of two formats for
a result and one for a fault. A result is the rendering `corpus/FORMAT.md`
fixes by default; `hs_wasm_set_result_format` switches every result to the
JSON value encoding the boundary already uses for arguments and capability
answers, which gives a host data instead of a string to parse. JSON has one
number type, so the encoding differs in one place: an `Int` that a double
cannot hold exactly (above 2^53 - 1 in magnitude) is written as
`{"bigint": "<decimal>"}`, never as a number that reads back as another
`Int`. A fault is still reported as its message, and `hs_wasm_fault_json`
adds the rest of section 17.1's record for a fault: the code, class, file,
line, column, the span in bytes and the limit data. The VM holds no source
text, so a UTF-16 span is the host's to compute from the bytes. See
`crates/hollowscript-wasm/web/README.md`, "Structured results and faults".

### `hollowscript-conformance` and `xtask`

Development tools, not published. `hollowscript-conformance` runs the shared
corpus through the facade and compares all four stages of every case against
the case's own expected file, failing both on a new failure and on a stale
baseline entry (see [testing.md](testing.md)); `scripts/check` runs it.
Like the published crates it may use no external dependency, so it carries
its own JSON reader. `xtask` is the repository hygiene scanner and helper
commands; its rules for this document are enforced by `cargo xtask graph`.
Its `bench` subcommand (`docs/design/performance.md`) is the one place
`xtask` depends on a workspace crate rather than none: it needs
`hollowscript`, the same facade a real embedder calls, for the same reason
`hollowscript-conformance` does, so that what a benchmark measures is what
a host actually experiences rather than a private fast path into the VM.

## Workspace-wide policy

Set once, centrally, in the workspace manifest, so no crate can quietly opt
out:

- **Edition 2024, Rust 1.85, resolver 3.** One toolchain baseline for every crate.
- **Lints denied, not just warned, in CI-style local checks.** `scripts/check`
  runs `clippy --all-targets -- -D warnings`, so a pedantic-level finding
  fails the same way a compile error would. Individual lints that cost more
  than they give for this code base are turned off with a reason at the
  workspace level (for example `module_name_repetitions`, which fires
  constantly on a crate family all named `hollowscript-*`); a lint that is
  right in general but wrong at one call site is allowed locally with a
  `reason` explaining why.
- **No panics in library code.** `unwrap_used`, `expect_used`, `panic`,
  `todo`, `unimplemented` and `dbg_macro` are denied everywhere except test
  code, which the specification's own requirement motivates directly:
  section 19.1 requires that a conforming implementation "never crashes,
  hangs or exhausts its host on any input." A function this skeleton does
  not implement yet returns `Err` of a typed error, or the internal
  not-yet-implemented diagnostic (`HS1099`), never a panic.
- **Deterministic collections.** `HashMap` and `HashSet` are denied by a
  `clippy.toml` rule, because their iteration order is not fixed and the
  specification requires the same program to behave identically on every
  conforming implementation (see
  [errors-and-determinism.md](errors-and-determinism.md)). `BTreeMap` and
  `BTreeSet` are used instead wherever order can be observed; a hash map used
  only for lookup, never iterated, is allowed locally with a `reason` (the
  type interner is the one example so far).
- **`unsafe_code` denied by default, allowed by name.** The workspace lint is
  `deny`, not `forbid`, specifically so that `hollowscript-capi` and
  `hollowscript-wasm` can override it locally; every other crate additionally
  states `#![forbid(unsafe_code)]` at its own root, which cannot be overridden,
  as a second, crate-local guarantee.
- **No external dependency in the crates that touch untrusted input.**
  `hollowscript-syntax`, `hollowscript-check`, `hollowscript-compiler` and
  `hollowscript-vm` depend on nothing but `hollowscript-core` and the standard
  library. Every dependency that does exist anywhere in the workspace is
  listed, with its licence, in [dependencies.md](dependencies.md).
