# Running the virtual machine in a browser

The WebAssembly build contains the VM and the verifier and nothing else: no
lexer, no parser, no checker, no compiler. A page loads bytecode that was
compiled somewhere else, and the verifier is what it trusts instead of
trusting whoever supplied the bytes.

The bytes are a container written by `hollowscript build` (or
`hollowscript_bytecode::encode`) from the same release. The format carries a
version, and a VM reads only its own: HollowScript 0.1.0 reads and writes
format 0.2 and refuses anything else, including 0.1 containers built before
it, so rebuild containers whenever the VM is upgraded
([docs/design/bytecode.md](../../../docs/design/bytecode.md)).

## Build

From the repository root:

```
rustup target add wasm32-unknown-unknown
cargo build -p hollowscript-wasm --release --target wasm32-unknown-unknown
```

That writes `target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm`.
Copy it next to `index.html`, or serve both from one directory:

```
cp target/wasm32-unknown-unknown/release/hollowscript_wasm.wasm crates/hollowscript-wasm/web/
cd crates/hollowscript-wasm/web && python3 -m http.server
```

Then open the address it prints. A page must be served rather than opened
from the filesystem, because the wrapper `fetch`es the module.

### The `tooling` build (checking, formatting and editor services in the browser)

A second, separate build carries the front end and the checker as well, so
an editor page can check, format and compile source, and ask the editor
services below, with no server and no CLI in between (`crate::tooling`,
exported through `crate::abi_tooling` and, for the editor services,
`crate::abi_ide`):

```
cargo build -p hollowscript-wasm --release --features tooling --target wasm32-unknown-unknown
```

This is a deliberately different `.wasm` file from the plain one above, not
a superset a published game would ever load: `hs_wasm_check`,
`hs_wasm_format`, `hs_wasm_compile` and the editor service exports only
exist in it. A published game
always ships the feature-less build, which stays exactly what it
was.

`check` and `compile` check against the standard always-granted
capabilities (`print`, `clock`, `random`: the same ones the CLI grants with
no `--allow`), plus whatever a page declares beyond them, through
`HollowScriptTooling`'s own optional third argument:

```js
const t = await HollowScriptTooling.load("hollowscript_wasm_tooling.wasm");

// needs no declaration: print, clock and random already check
t.check('uses print\neffect print("Hello, world!")');

// a host-defined capability beyond the standard set
const diagnostics = t.check(source, "main.hws", [
  { name: "tasks", function: "list", params: ["String"], result: "String" },
  { name: "tasks", function: "add", params: ["String"], result: { ok: "Int", err: "String" } },
]);
```

Each entry is `{ name, function, params, result }`: `function` is a string
naming one function of a namespace capability (`tasks` above), or omitted
for a capability that is a single function; `params` is an array of *type
descriptions*, and `result` one type description, an object `{ ok, err }`
for a capability declared `Result<T, E>` (section 8.2), or omitted for a
function with no result. A type description is:

- a bare string, one of `"Int"`/`"Float"`/`"Bool"`/`"String"`/`"Filter"`
  (`"Filter"` valid only in `params`: Appendix A.1's own parameter-only
  restriction, matching Cascade's `tasks`);
- `{ list: T }`, `{ set: T }`, `{ optional: T }`, for `T` another type
  description;
- `{ map: { key: T, value: T } }`;
- `{ record: { field: T, ... } }`, at least one field;
- `{ enum: { name: "E", variants: { V: [T, ...], ... } } }`, at least one
  variant (a payload-less variant still needs its array, empty).

This is the same tagged shape the value encoding itself uses (below), one
level up: a capability whose declared result type is `{ record: { x:
"Int", y: "Int" } }` (Studio's `entities.get`, say) answers a value tagged
the same way, `{ record: { x: 3, y: 4 } }`; declaring only the four scalars
plus `Result` was HS0320 for anything else, which blocked exactly this.

### Nesting depth and the stack

Section 1.10 lets a program nest 250 levels deep (and, through the weight of
a chain, deeper still in the passes after the parser), and says a program
within the limits must not exhaust the host's stack. Every other build gets
that from a dedicated 16 MiB thread; `wasm32-unknown-unknown` has no threads,
so the `tooling` build parses, checks, compiles and formats on the module's
own shadow stack, which the linker sizes at 1 MiB unless told otherwise. That
was not enough: 233 nested closures (a 3.7 KB source, well inside the limit)
overflowed it, and the call trapped the module. `build.rs` now links the
`tooling` build with a 16 MiB stack (`-zstack-size=16777216`), the size of the
thread everywhere else; the VM-only build runs script calls on an explicit
frame stack and keeps the default. The memory costs address space, not
resident memory: the pages are not touched until a deep program uses them.

Measured on the 1 MiB stack (by building the module with smaller stacks and
finding the depth at which each construct first traps), the parser and the
checker cost, for each level of nesting: a closure about 4.5 KiB, a record or an
interpolation about 3.9 KiB, a parenthesis or a list about 3.6 KiB, a block
about 1.5 KiB, a link of a long chain about 0.7 KiB, and filters, types, type
arguments and patterns less than a block. Nothing else about the shape matters:
`check`, `format`, `compile` and `analyse` trap at the same depth, because it is
the parser's recursion that is deepest. So on the 1 MiB stack a closure nested
inside anything (a bare `fn() { return fn() { ... } }`, a closure in an operand
of a chain, a closure in a call) trapped from **233** levels up, a parenthesis
or a list from about 290 (so, at the limit of 250, with about 14% to spare),
and nothing else reached it. The deepest legal program, 250 closures, needs a
little over 1.1 MiB; 16 MiB is more than a dozen times that.
`web/node-e2e/deep-nesting-check.mjs` checks every shape, to its own deepest
legal level and one past it, so a change that makes a pass use more stack, or a
shrink of the stack, fails there.

### A module that traps is dead

A panic inside the module, or a stack the module overflows anyway, is a
`trap`, and WebAssembly abandons the Rust frames it was in without unwinding:
the shadow-stack pointer is never put back, so every later call would run on a
corrupted stack. `hollowscript.js` watches every call into the module. The
first one that traps (a `WebAssembly.RuntimeError`, which is also how a
program that allocates past the memory ceiling ends, or the engine's own stack
overflow) throws a `HollowScriptTrap` (an `Error` with `fatal: true` and the
trap as its `cause`) and marks the whole instance dead, `HollowScript` or
`HollowScriptTooling` and every `Instance`, `AsyncInstance` and `Analysis`
made from it. Every later call on any of them throws a `HollowScriptTrap`
before it reaches the module; `dead` on the two top-level classes says which.
A host that sees one discards the instance and loads a fresh one. An error a
host's own capability handler throws is not a trap and does not kill the
instance. `web/node-e2e/trap-check.mjs` checks this against a real trap.

### Diagnostics

Each diagnostic `check` answers (and `format`'s and `compile`'s own
`diagnostics`) carries what the facade's own `Diagnostic` has:

```js
{
  severity: "error", code: "HS1099", message: "...",
  file: "main.hws", line: 1, column: 5,
  spanStartUtf16: 12, spanEndUtf16: 19,
  related: [
    { file: "main.hws", line: 2, column: 1,
      spanStartUtf16: 20, spanEndUtf16: 25, message: "..." },
  ],
}
```

`line` and `column` are the specification's own units: 1-based, `column`
in code points. `spanStartUtf16`/`spanEndUtf16` are the half-open span in
UTF-16 code units, not bytes and not code points, named explicitly because
that is what Monaco (and every other web text editor) counts a position
in, so a page can hand them straight to `monaco.Range.fromPositions` or
`model.getPositionAt` without its own conversion. `file`, `line`, `column`,
`spanStartUtf16` and `spanEndUtf16` are `null` for a diagnostic (or a
`related` location) with no position at all; `related` is always present,
`[]` when there are none.

### Multi-file projects (a module loader)

`check` and `compile` take a fourth argument, `modules`, resolving every
`import` beyond the entry module itself (section 10.3), for a Code
workspace of more than one file: a plain object mapping each other
module's own canonical path (relative to the entry module, extension
included, no leading `./`, exactly as `import ... from "./path"` resolves
it) to its source text.

```js
const source = 'import double from "./lib/helpers"\nreturn double(3)\n';
const modules = {
  "lib/helpers.hws": "export fn double(n: Int): Int {\n    return n * 2\n}\n",
};
t.check(source, "main.hws", [], modules); // []
const bytes = t.compile(source, "main.hws", [], modules);
```

The default, an empty object, is a single-file program: an `import` in
`source` is HS0601 ("not found"), exactly as before this parameter
existed. `format` takes no `modules` (and no capabilities): formatting one
file needs no import resolved, the same as it has always needed no
capability interface.

### Filter schemas and filter literals (a host that evaluates filters)

A program can hold filter literals (Appendix A), written between backticks
(`` `status:open` ``). A literal is an opaque `Filter` value: the language
never evaluates it, and a capability that is handed one receives its source
text, `{ filter: "status:open" }`, to evaluate against the host's own
records. Two things in the `tooling` build serve a host that does.

**A schema, at check time.** `check` and `compile` take a fifth argument,
`filterSchema`, the host's description of its records (section A.6), in the
shape of section 19.4's `filterSchema`:

```js
const schema = {
  fields: {
    status: { type: "enum", values: ["open", "done"], aliases: { closed: "done" } },
    title: { type: "text" },
    due: { type: "date" },
    points: { type: "number" },
    starred: { type: "boolean" },
    label: { type: "textMulti" },
  },
};
t.check(source, "main.hws", capabilities, modules, schema);
// `colour:red` is now HS0708 (no such field), `title>x` HS0707 (an operator a
// text field does not allow), `status:blocked` HS0709 (not one of its values)
```

With a schema every filter literal is also validated against it (section A.7),
so a field the host does not have is a diagnostic at check time and not a
surprise at run time, and `compile` refuses the same programs, so one that
`check` rejects never becomes bytecode. The default, `null`, declares no
schema and a literal is checked for syntax only. `{ fields: {} }` is a schema
with no fields, which refuses every predicate; that is not the same as none.
`type` is one of `enum`, `text`, `textMulti`, `date`, `number` and `boolean`;
`values` (an `enum` only) and `aliases` are optional.

The schema is read strictly, because one that is silently read as something
looser accepts what the host meant to refuse. A member that is not defined, a
`type` that is not one of the six, a field name or an alias input that is not
lower case (a filter's field names are lower-cased, so such a field could
never match), `values` on a field that is not an `enum` and a missing `type`
are each a single HS1099 diagnostic that says what is wrong.

**The literals, before the program runs.** `filterLiterals(source, name,
modules, capabilities)` lists every filter literal of the program and of the
modules it imports, as the checker found them, in source order:

```js
t.filterLiterals(source, "main.hws", modules, capabilities);
// [{ source: "status:open", file: "main.hws", line: 3, column: 42,
//    spanStartUtf16: 87, spanEndUtf16: 100 }, ...]
```

`source` is the filter's text with `` \` `` and `\\` unescaped, exactly what a
capability receives as `{ filter: "..." }`, so a host that evaluates the
filters of a program once, ahead of running it, keys what it computed by that
text. The span is of the whole literal, backticks included, in UTF-16 code
units of that file's own text, so `text.slice(spanStartUtf16, spanEndUtf16)`
gives the literal as written; `line` and `column` are as a diagnostic's are.
The list is grouped by file, an imported module before the module that
imports it and the entry module last, and is in source order within a file.
A text that appears twice is listed twice, at each place, since each is a
place a program asks; a host that wants each text once removes the repeats.

The program does not have to be valid. A literal is listed whether or not it
is valid and whether or not the program around it checks, so a host can show
what a program asked even where it will not run; `check` says whether it will
(and, with a schema, whether each filter suits the host). `capabilities` and
`modules` are as `check` takes them (the fourth argument comes last because
most hosts need none), and a malformed one throws, so an empty array always
means there are no literals. A literal that is not even a token, an
unterminated backtick, is the lexer's problem and is not listed.

### Editor services

A browser editor needs more than diagnostics: colours, a keyword list, and
answers about names and types. These come from `hollowscript-ide`, the same
analysis the language server uses, and they all count a position the way a
web editor does.

**Every offset is a UTF-16 code unit offset, and every range is half-open.**
That is what Monaco's `model.getPositionAt` and `getOffsetAt` count, so an
offset a service answers goes straight into `monaco.Range.fromPositions` or
`String.prototype.slice`, and an offset a page passes in is
`model.getOffsetAt(position)`. The services do their own conversion from
the checker's UTF-8 byte spans, so a character outside the basic
multilingual plane (an emoji, two UTF-16 units and four UTF-8 bytes) before
the text of interest does not shift anything. An offset past the end of the
text is treated as the end of the text.

The boundary behind them is `hs_wasm_highlight`, `hs_wasm_keywords`,
`hs_wasm_standard_capabilities`, `hs_wasm_analyse` (which answers a handle, or
0 with the reason in the text channel), `hs_wasm_analysis_hover`,
`hs_wasm_analysis_completions`, `hs_wasm_analysis_definition`,
`hs_wasm_analysis_symbols` and `hs_wasm_analysis_dispose`, all answering JSON
through the text channel like `hs_wasm_check`; `hollowscript.js` wraps them.

`highlight`, `keywords` and `standardCapabilities` are stateless: they keep
nothing between calls, need no modules, and (for the first two) work on text
that does not parse. Hover, completion, definition and the outline belong to
an analysis handle (below), which checks once and answers many times.

```js
const t = await HollowScriptTooling.load("hollowscript_wasm_tooling.wasm");

// [{ start: 0, end: 8, class: "keyword" }, { start: 9, end: 14, class: "identifier" }, ...]
const ranges = t.highlight("constant total = 1 // one\n");
```

`highlight(source)` classifies every token and comment lexically, from the
lexer alone, as `{ start, end, class }` in order of where they start, never
overlapping. The classes are:

| `class` | What it covers |
|---|---|
| `keyword` | a reserved word: `constant`, `fn`, `true`, `none`, the word operators `and`, `or`, `not` and `in`, and the words reserved for a later version |
| `identifier` | a name, including a type name and a field name |
| `number` | an `Int` or a `Float` literal |
| `string` | a string literal; a string with interpolation is cut into pieces, each covering its own text including the `${` and `}` that bound it, with the interpolated expression's own tokens between them |
| `filter` | a filter literal, backticks included |
| `operator` | `+ - * / = == != < > <= >= .. ?` |
| `punctuation` | `( ) [ ] { } , : .` |
| `comment` | a `//` comment, up to but not including the line end |

Whitespace is not covered, and neither is a character the language has no
token for, so highlighting never colours text the language does not
recognise. A text longer than the source size limit of section 1.1 (4 MiB) is
not lexed, and the answer is empty. The language has line comments only, so there is one `comment`
class and no sub-kind. The tags `todo:`, `explain:` and `note:` the lexer
knows are not reported. `highlight` is the lexical layer only: it cannot tell a function from a
variable, which needs name resolution.

`keywords()` answers `[{ word, future }, ...]`, every reserved word in
alphabetical order, from the lexer's own table. `future` is `true` for a
word reserved for a later version (`assert` and `test` today), which is an
error to write now: offer the others.

`standardCapabilities()` answers the declarations of the standard
capabilities, `print`, `clock` and `random`, in the same shape the
`capabilities` argument of `check` accepts, so a host need not hand copy
them:

```js
t.standardCapabilities();
// [{ name: "print", params: ["String"] },
//  { name: "clock", function: "now", params: [], result: "String" },
//  ...
//  { name: "random", function: "int", params: ["Int", "Int"], result: "Int" },
//  { name: "random", function: "float", params: [], result: "Float" }]
```

`check`, `compile` and `analyse` already grant these on top of whatever
`capabilities` declares, so passing them again changes nothing; they are for
a page that wants to show a program's author what is available, or to hand
them to something else along with its own declarations. As everywhere, one
entry per function of a namespace capability, with `function` left out for a
capability that is a single function and `result` left out for a function
with no result.

#### The analysis handle

`analyse(source, entryName, capabilities, modules)` checks a module once and
holds the result, so that hover, completion, definition and the outline can be
asked many times without checking again. The four arguments mean what they
mean for `check`: the module's own name (default `"main.hws"`), the host
capabilities declared beyond the standard ones, and the other modules an
`import` resolves against, by canonical path.

```js
const analysis = t.analyse(source, "main.hws", [], { "lib/helpers.hws": helpers });

analysis.hover(offset);        // { start, end, type } or null
analysis.completions(offset);  // [{ label, kind, detail }, ...]
analysis.definition(offset);   // { file, start, end } or null
analysis.documentSymbols();    // [{ name, kind, detail, exported, start, end, selStart, selEnd, children }, ...]

analysis.dispose();            // idempotent
```

An analysis is a handle: the module refers to it by a number it handed out,
never by a pointer, and looks that number up on every call, so a number it
does not know is refused instead of reaching memory. `dispose()` releases the
checked program, may be called any number of times, and makes every other
method throw. The module hands out a number once and never again, so a
disposed analysis stays disposed even after another is made. Keep one per
version of the text (an editor takes a new one after an edit and disposes the
old one): it holds the whole checked program, and the module refuses a 65th
analysis held at once, with an error that says to dispose. Explicit `dispose()`
is still what to do: as a safety net, an analysis that is garbage collected
without it is disposed by a `FinalizationRegistry`, so a forgotten handle does
not hold one of the 64 places for the life of the module, but the engine decides
when (if ever) it collects, so a page that makes analyses faster than that still
reaches the cap.

An offset passed in must be a non-negative integer, or a `RangeError` is
thrown; one past the end of the text is the end of the text. A malformed
`capabilities` or `modules` argument, or a source longer than the size limit of
section 1.1, makes `analyse` throw with the reason. A program with errors
analyses fine: every answer is best effort over what checked, which is the
point of an editor.

**`hover(offset)`** answers the type of the smallest expression whose range
holds the offset, as the checker renders it (`Int`, `[String]`,
`fn(Int): Int`), with that expression's own half-open range, or `null` where
the offset is not inside an expression (a keyword, the gap between
statements) or the checker could not work the type out.

**`completions(offset)`** answers what can be written at the offset, as
`{ label, kind, detail }`: first the names in scope there, in the order they
are declared; then the prelude's value names and type names, except any the
program declares itself; then every keyword a program can write today (the
words reserved for a later version, `keywords()`'s `future`, are an error to
write, so they are left out). A name comes into scope where section 4.6 says:
a local after its declaration and to the end of its block, a parameter in its
body (a closure's too), a top-level `fn`, `record`, `enum`, import and
capability anywhere in the file, and a top-level `constant` or `variable`
only after its declaration in code that runs in order but anywhere inside a
function body. So a name declared further down is not offered. The list is
the same wherever in a word the cursor sits; filter it by what has been
typed.

| `kind` | What it is |
|---|---|
| `variable` | a `variable`, a `for` variable, a name a pattern bound |
| `constant` | a `constant`, or the prelude's `pi` |
| `function` | a `fn`, or a prelude function |
| `parameter` | a parameter of a function or a closure |
| `type` | a `record`, or a type of the prelude |
| `enum` | an `enum` |
| `capability` | a capability named by a `uses` line |
| `namespace` | a namespace of the prelude, `json` or `time` |
| `keyword` | a reserved word |

(`enumMember` is a kind the outline uses and completion does not yet offer:
a variant is written through its enum.) `detail` is a function's signature
(`fn(a: Int, b: Int): Int`), the type of a parameter, constant or variable
(its annotation, or the type the checker inferred), `prelude` for a prelude
name, `keyword`, or empty.

The list is empty where a name cannot be written: inside a comment, inside
the text of a string or a filter literal (but not inside a `${ }`
interpolation), and after a `.`. **Known gap:** completing a field or a
method after a `.` is not done. That needs the type of the receiver and the
members of that type, so a page gets nothing there rather than a list of names
that would be wrong.

**`definition(offset)`** answers where the name at the offset was declared:
`file` is the module's canonical path, one of the keys of `modules` for a name
brought in by `import` (and the entry name for one declared here), and
`start`/`end` the range of the name in *that* module's text, in UTF-16 code
units of that text. It answers `null` for a keyword, a prelude name, a field or
method name after a `.`, and text inside a comment or a string.

**`documentSymbols()`** answers the outline of the entry module as a tree, built
from the syntax tree alone, so it is there for a file that does not check yet:

| `kind` | Item |
|---|---|
| `capability` | one per name a `uses` line declares (`range` is the whole line) |
| `module` | one per `import` line, named by its path as written, with the imported names as `detail` (`selStart`/`selEnd` is the path) |
| `function` | a `fn`; `detail` is its signature |
| `record` | a `record`; `detail` is its type parameters, `<T>`, if any; `children` are its fields |
| `enum` | an `enum`; `detail` is its type parameters; `children` are its variants |
| `constant`, `variable` | a top-level declaration of one name; `detail` is its annotation or the inferred type |
| `field` | a field of a record; `detail` is its type |
| `enumMember` | a variant of an enum; `detail` is its payload, `(Float, Float)`, or empty |

`exported` is `true` for a declaration marked `export`. `start`/`end` is the
whole declaration (`export` included), `selStart`/`selEnd` its name, and
`children` is always present, empty for a leaf. A destructuring declaration has
no single name to show, so it is left out.

### Size

Measured on this machine (`profile.min-size` is the workspace's own
small-build profile; `wasm-opt` is from `binaryen`, `brew install binaryen`,
run as `wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int`,
the two features rustc's wasm32 target already emits by default that
`wasm-opt` otherwise refuses to touch; the result was re-run through the
same `web/node-e2e/` scripts to confirm it still behaves identically):

| Build | `release` | `min-size` | `min-size` + `wasm-opt -Oz` |
|---|---|---|---|
| VM only (default, no features) | 4.8 MiB | 542 KiB | 473 KiB |
| `tooling` (+ syntax, checker, compiler, editor services) | 10.5 MiB | 1,285 KiB | 1,054 KiB |

In bytes, `min-size` VM-only is 554,601 (532,569 before the hardening of the
verifier, the argument checks and the instance table, the memory contract and
the bounded writers) and `tooling` 1,316,228 (1,265,718); with `wasm-opt -Oz`
483,829 (464,182) and 1,079,434 (1,038,607). The memory contract itself (an
imported memory with a declared maximum) changed these figures by under 1 KiB
each way.

`release` carries `debug = "line-tables-only"` from the workspace profile,
which is most of the difference from `min-size`; a real deployment ships the
`min-size` build with a `wasm-opt -Oz` pass over it, a further 13-18%
smaller here. The VM-only build is what a published game cares about
staying small; `tooling` is for an editor page, loaded once per session
rather than once per game, where the extra size buys checking and
formatting and the editor services without a server. The editor services
(highlighting, hover, completion, definition, the outline and the analysis
handle) added 91 KiB to the `min-size` `tooling` build (1,145 KiB to 1,236 KiB,
1,172,810 to 1,265,712 bytes) and 67 KiB with `wasm-opt -Oz` (947 KiB to
1,014 KiB, 969,561 to 1,038,602 bytes); the VM-only build does not contain
them. Both builds are inside the 2 MB budget `docs/design/performance.md`
sets for `hollowscript-wasm`.

## The boundary

`hollowscript.js` is the whole wrapper, and it is small on purpose: there is
no generated glue, no `wasm-bindgen` and no build step beyond `cargo build`.
It calls the functions `src/abi.rs` exports, which work over the module's own
linear memory, and it supplies the memory (see "Memory" below) and four
imports the module calls back through:

| Export | What it does |
|---|---|
| `hs_wasm_alloc(len)`, `hs_wasm_free(ptr, len)` | reserve and release a buffer the page writes into |
| `hs_wasm_text_len()`, `hs_wasm_take_text()` | read back the text the last call produced |
| `hs_wasm_version()` | the VM's version |
| `hs_wasm_set_limits(steps, memory, output, depth)` | the limits of chapter 11 |
| `hs_wasm_set_result_format(format)` | how a result is written to the text channel: `0` the text below (the default), `1` JSON (see "Structured results and faults") |
| `hs_wasm_fault_json()` | the structured form of the fault the last call ended with, as JSON on the text channel, or empty |
| `hs_wasm_capability_names(ptr, len)` | the `(capability, function)` pairs bytecode calls, positionally, as JSON |
| `hs_wasm_load(ptr, len)` | verify bytecode and initialise it, answering a handle |
| `hs_wasm_call(handle, name, args)` | call an exported function |
| `hs_wasm_read(handle, name)` | read an exported constant |
| `hs_wasm_dispose(handle)` | end an instance |
| `hs_wasm_load_batched(ptr, len)` | as `hs_wasm_load`, on the suspend/resume path (below) |
| `hs_wasm_call_batched(handle, name, args)` | as `hs_wasm_call`, may answer `Suspended` |
| `hs_wasm_commands_json(handle)` | the capability calls a suspended run is paused at, as JSON; may answer `Faulted` (HS1004) when the arguments are too large to write, see "Structured results and faults" |
| `hs_wasm_resume_json(handle, results)` | continue a suspended run with the host's answers |

| Import (`env.*`) | What the host must supply |
|---|---|
| `hs_capability_call(index, argsPtr, argsLen)` | calls capability `index` (see `hs_wasm_capability_names`) with its JSON arguments; `0` for success, `1` for failure |
| `hs_capability_result_len()` | the length of the pending result the last `hs_capability_call` produced |
| `hs_capability_take_result(destPtr)` | copies that pending result into the module's own memory, consuming it |
| `memory` | the module's linear memory, which the host creates (see "Memory") |
| `hs_wasm_poll_cancel()` | polled at every safepoint; non-zero stops the run with the fault HS1006. `hollowscript.js` backs it with the shared flag, the `deadlineMs` budget and the `shouldCancel` callback (see "Cancellation") |

Values cross as text, for the same reason they do at the C ABI: a value of a
container kind is a handle into one instance's heap, which a caller outside
the VM cannot hold. A result comes back in the rendering `corpus/FORMAT.md`
fixes (`instance.call`'s own text result); arguments go in as a JSON array,
and a capability's own arguments and answer cross the same way, in both
directions, using this encoding for every transferable type of chapter 12,
not only a scalar, the same one `hollowscript-capi`'s C ABI uses:

| HollowScript | JSON |
|---|---|
| `Int`, `Float`, `Bool` | a number or `true`/`false` (a `Float` is written with a decimal point or an exponent, `3.0` and `1e21`, an `Int` never is; `JSON.parse` reads both as a number) |
| `String` | a string |
| `none` | `null` |
| `[T]` | `{ list: [...] }` |
| `Set<T>` | `{ set: [...] }` |
| `Map<K, V>` | `{ map: [[k, v], ...] }` |
| a record | `{ record: { field: value, ... } }` |
| an enum value | `{ enum: { name: "E", variant: "V", payload: [...] } }` |
| `Result<T, E>` | `{ Ok: value }` or `{ Err: value }` |
| `Filter` | `{ filter: "<source text>" }`, outbound only (a program builds one only from its own literal) |

A record or enum value crosses only once its shape or variant is one the
module already has (a capability's own declared type, or the program's
own source anywhere, including only in an exported function's signature).

**A bare JSON number is decoded against the declared type, not its own
spelling.** JSON cannot tell `3` from `3.0` (a number with no `.` and no
exponent is read as an integer, whichever the sender meant), and a
JavaScript host cannot write one that can: `JSON.stringify(3.0)` is
already `"3"` by the time it reaches this boundary. So a JSON number
crossing inbound, an `instance.call`/`instance.resume` argument, a
capability's own argument (bound with `bindCapability`), or its own
answer (`bindCapability`'s return value, or a batched `resume` answer),
decodes to the `Float` its position's declared type calls for even when
it looks integral, walking into a list element, a map's own value, a
record field, an enum's or `Result`'s own payload and an optional's own
inner type the same way. A number at a declared `Int` position, one
outside the `Int` range, or one that is not integral where `Int` is
declared, is still refused, by the same acceptance rule as before
(section 9.6): this is the encoding's own definition of which language
value a JSON number denotes, given the declared type, not the conversion
section 9.6 says never happens "inside a collection, a record or an enum
payload" (which is about a language *value* the boundary has already
decided on, not about deciding what one JSON number means in the first
place).

**What a call's arguments are held to.** `instance.call` and the batched
`call` check the arguments before anything runs, as section 12.4 says: the
count must be the export's number of parameters (step 1), and each argument
must be a value of its parameter's type by the acceptance rule of section 9.6
(step 2), which also converts an `Int` for a `Float` parameter. Decoding alone
only builds a value of the shape the JSON has, so without this a string for an
`Int` parameter, `1.5` or `1e300` for an `Int`, a set for a list, a map of
strings for a `Map<String, Int>` or a missing argument reached the program, which
then answered wrongly or faulted as an internal error. A refused call throws a
plain `Error` that says which argument and what type it should have (`argument 2
is not a value of Map<String, Int>`, `expected 1 argument(s), found 0`), the
program has not run, and the instance is unchanged: it is the host's mistake,
not a fault of the program. What a host answers a suspended capability call with
(`resume`, `bindCapability`) is held to the capability's declared result type
the same way (HS1013, a fault, because the program is already running).

```js
// entities.get answers { x: Float, y: Float }; both look integral, but
// decode as genuine Floats because the declared type says so.
vm.bindCapability("entities.get", (args) => ({ x: 1, y: 0 }));
```

An instance is named by a number from a registry rather than by a pointer, so
a page that passes back a number the module never gave it is refused instead
of reaching memory. The number holds a place in the registry and a count of how
many instances that place has held: disposing an instance frees its place for
the next load, so a page that loads and disposes in a loop (as an editor does on
every keystroke) does not grow the module (it grew 1.9 KiB a cycle, 38 MB in
20,000, before places were reused), and the count moves on at every dispose, so
the number of an instance that has been disposed is refused for good, even after
another instance takes its place, and disposing it again leaves the new one
alone. A place that has held 65,535 instances is retired, not wrapped. At most
65,535 instances can be live at once; a load past that answers an error that says
to dispose one first. `web/node-e2e/instance-churn-check.mjs` checks the loop.

`hollowscript.js` hides all eight of those behind three additions to the
class it already had:

```js
import { HollowScript, createCancelFlag, cancel } from "./hollowscript.js";

const vm = await HollowScript.load("hollowscript_wasm.wasm");

// bind a capability by name (or "capability.function" for a namespace
// member, such as "clock.now") before loading a program that calls it
vm.bindCapability("print", (args) => {
  console.log(args[0]);
});

// vm.capabilityNames(bytecode) answers the ordered list loadProgram needs
// every one of bound before it will load a program that calls them
const instance = vm.loadProgram(bytecode);
console.log(instance.loadResult); // section 12.3's load result, or null
instance.call("run");
```

`instance.loadResult` is section 12.3's load result: the rendered value a
top-level `return` in the entry module produced (`"Int(3)"`, the same
rendering `instance.call`'s own result already uses), or `null` for a
module that falls off the end without one. It is read once, from
`loadProgram` itself; a load-time fault is thrown as an `Error` instead,
exactly as `instance.call`'s own fault is.

A capability handler is called with a plain JS array of the arguments, each
in the encoding above (so a `Filter` parameter, such as Cascade's `tasks`
takes, arrives as `{ filter: "<source text>" }`, and an `Int` above 2^53 - 1
as a `BigInt`), and must return a value
in the same encoding (or nothing); throwing fails the call with HS1012 and
the thrown message. A handler's exception never crosses back into
WebAssembly itself: `hollowscript.js` catches it at the boundary, which is
what keeps a page's own capability bug from leaving the module unable to be
entered again.

**Namespace capabilities** (`clock`, with `now` and `today`; Cascade's
`tasks`, and so on) need no different binding: bind each function under its
own qualified name (`vm.bindCapability("clock.now", ...)`,
`vm.bindCapability("clock.today", ...)`), exactly as `capabilityNames()`
lists them. Binding is entirely positional by index against the bytecode's
own `cap_calls` table; the qualified name is only ever used as a key on the
JS side and in a fault's message. A capability's name, the name of each
function of a namespace capability and an export's name must each be an
identifier (an ASCII letter or `_`, then ASCII letters, digits and `_`) and
distinct, which the verifier checks, so a qualified name is always exactly one
namespace, one dot and one function, and `capabilityNames()` is a JSON array a
host can index by position without escaping; bytecode with any other name is
refused when it is loaded.

**A capability declared `Result<T, E>`** (section 8.2, such as `fs.read`,
or a host-defined one) answers success or an expected failure as a plain
value the program pattern-matches, not by failing the call: return a plain
object with exactly one field, `{ Ok: value }` or `{ Err: value }`, from the
handler, instead of a bare scalar. This is a different thing from the
handler throwing, which is always a call-level failure (HS1012) regardless
of the capability's declared type.

### Setting limits

`vm.setLimits({ steps, memory, output, depth })` sets the limits of chapter 11 for
every run that follows; a limit left out, or 0, keeps the value it had. Each is a
non-negative integer (a `Number` up to 2^53 - 1, or a `BigInt`) and no more than
2^63 - 1, the largest a limit can be. Anything else throws and changes none of the
limits: a negative number, a fraction, `NaN`, a string, a value above 2^63 - 1
(`RangeError` or `TypeError`), and, from a host that calls `hs_wasm_set_limits`
directly, any value outside 1 to 2^63 - 1, which the export refuses with the
reason in the text. A negative number was passed to the module as an unsigned
64-bit integer, so `setLimits({ steps: -1 })` made 2^64 - 1 the step limit, which
nothing ever reaches and which a counter added to it could overflow. The counters
now saturate rather than overflow as well, whatever a host puts in a `Limits`.

### `print` and the output limit

`print` is a capability the host binds, and what it prints counts against the
output limit, `maxOutputBytes` (16 MiB by default; `setLimits({ output })`
changes it). Section 11.6 counts the UTF-8 bytes of the text and one line
feed for each call, and section 16.2 makes that part of what a host that
offers `print` offers, so the module does the counting and not each host:
before the handler bound to `print` is called, the call is charged, and a
call that would take the run past the limit is not made. The run faults with
HS1004, the handler is never called for that line (so nothing of it is
written), and `fault.data` names the limit and what had been written:

```js
vm.setLimits({ output: 12 });
vm.bindCapability("print", (args) => console.log(args[0]));
instance.call("shout", [3]); // prints two lines of "hello" (6 bytes each),
                             // then throws a HollowScriptFault, HS1004,
                             // data: { option: "maxOutputBytes", limit: 12, used: 12 }
```

The count is per run (a load, or one call: section 11.2), in bytes and not in
characters (`"é"` is 3), and is made on the batched path too, before the
call is queued, so a host that answers `print` asynchronously is never told to
write a line over the limit. It is made in the order of section 11.3: the
call's step charge first, then the output.

**Every capability called `print` is counted, whatever it declares.** The host
binds handlers by name alone, and a handler cannot say what shape it was written
for, so bytecode can declare `print` as taking a list of strings (or a record, or
three strings) and the handler bound to that name receives and writes whatever
that is. An earlier build counted only the standard shape, a plain function with
one `String` parameter, and a `[String]` declaration delivered 256 MiB through a
handler under an output limit of 1,000. Now the text the arguments carry is
charged: a `String` is its bytes (which, for the standard shape, is exactly what
it was); an `Int` is its digits, a `Float` 24, a `Bool` 5 and `none` 4; a list,
a set, a record or an enum value is the sum of what it holds, and a map's key and
value, with one byte for each element as its separator and a line feed for the
call. A value a list holds twenty thousand times counts twenty thousand times,
because it is written that many times. It is an estimate of what a handler
writes, an upper bound for a handler that writes the text it is given, not a
figure the specification fixes (it fixes only the standard shape).

## Memory

**The module imports its linear memory, with a maximum of 256 MiB.** A
WebAssembly memory cannot grow past the maximum its module declares, and an
import is matched only by a memory whose own maximum is no larger, so a
program cannot make this module's memory grow past 256 MiB and a host cannot
give it a bigger one. The module declares `env.memory` with a minimum of 17
pages (1.1 MiB: the stack and the static data; 18 in the `min-size` build) and a maximum of 4,096 pages
(256 MiB). The `tooling` build declares the same maximum and a minimum of 258
pages (16.1 MiB), because its 16 MiB stack ("Nesting depth and the stack") is in
the linear memory and the module cannot start with less than it. `hollowscript.js` reads both from the
module's import section (`readMemoryLimits(bytes)`), makes the memory, and
passes it in; a host that does not use the wrapper does the same:

```js
const memory = new WebAssembly.Memory({ initial: 17, maximum: 4096 }); // 64 KiB pages
const { instance } = await WebAssembly.instantiate(bytes, { env: { memory, ...imports } });
```

A memory with a maximum above 4,096 pages, or with none, does not link.

**A host that wants a lower ceiling asks for one.** `HollowScript.load`,
`fromBytes` and (for `HollowScriptTooling`) the same take
`{ maxMemoryBytes }`, rounded up to whole pages, between what the module needs
to start and 256 MiB (otherwise a `RangeError` that says how much the module
needs to start, 1.1 MiB for the VM and 16.1 MiB for the `tooling` build, and
never a trap); `vm.memoryCeilingBytes` says what is in force and `vm.memoryBytes`
how much the module has taken so far (memory only grows). A ceiling at the least
the module needs leaves it nothing to allocate with, so the first call that
allocates ends in the trap of a failed allocation described below: give the
module some headroom (the `tooling` build was measured to work with 1 MiB above
its 16.1 MiB and to trap with 256 KiB; `web/node-e2e/memory-check.mjs` uses
4 MiB above either build's minimum). A build for a host that needs a different
ceiling sets `HOLLOWSCRIPT_WASM_MAX_MEMORY_MIB` (2 to 4,096, and 17 to 4,096 for
the `tooling` build, which cannot have a ceiling below its stack) when it
builds; the figure is
baked in by `crates/hollowscript-wasm/build.rs` as a linker argument, and it
survives `wasm-opt` and the `min-size` profile (checked in both: see
`web/node-e2e/memory-check.mjs`).

**The logical memory limit is not the real footprint.** `maxMemoryBytes` of
`setLimits` counts what the language's own size formulas say a value is worth
(section 11.4: 8 bytes an element of a list, and so on), so that the same
program reaches the same limit everywhere. The module's linear memory holds
more, by a factor that depends on how a value was built and not only on what it
holds. Measured on this build (`web/node-e2e/memory-sample.hws`, the module's
`memory.buffer.byteLength` after the call, Node 26, VM only):

| What the program builds | Logical size | Module memory |
|---|---|---|
| `range(0, 4000000)`, 4 million `Int`s | 32 MiB | 123 MiB |
| `range(0, 8000000)`, 8 million `Int`s | 64 MiB | 245 MiB |
| 600,000 pairs, `range(0, 600000).map(fn(i) { return [i, i] })` | 24 MiB | 199 MiB |
| the same, returned to the host as JSON or as text | 24 MiB | 199 MiB |

The last row used to be 360 MiB as JSON and 296 MiB as text: the writers made
a vector of a step for every element before writing any of them, and now hold
a position in each container instead. A loop of `append`s can reach much more
(`docs/design/security.md` has the figures), and a host's own conversion of a
large JSON result (`JSON.parse` of 15 MB of text) is outside the module and
more again. So a host that runs scripts for others should size by the ceiling,
not by the logical limit: a logical limit of 32 MiB or less keeps the ordinary
shapes above under the default 256 MiB, and the default 64 MiB logical limit
does not (the 8 million `Int`s above are at 245 MiB of 256, and a longer list
or a loop of appends is past it).

**What happens at the ceiling.** Rust aborts when an allocation fails, and on
`wasm32-unknown-unknown` an abort is a `trap` (`unreachable`). So a program
that allocates for real past the ceiling traps the module; it is not a fault of
the program (there is no HS code for it: the module cannot report anything
once it has trapped). The behaviour afterwards is defined:

- the call that was running throws a `HollowScriptTrap`, an `Error` whose
  `cause` is the `WebAssembly.RuntimeError`;
- the module is gone: every later call on that `HollowScript`, on its
  instances and on `HollowScriptTooling` throws the same error and does not
  enter the module again, since the trap abandoned Rust frames mid-operation
  and left no promise about what they were holding;
- `dispose()` and the release of buffers are no-ops, so a `finally` does not
  replace the error that matters;
- nothing else is affected: a new `HollowScript.fromBytes(...)` has a new
  memory. The old memory is released when nothing refers to the old object.

A host should treat a `HollowScriptTrap` as the end of that instance: drop
it, and load a fresh one. A panic of the implementation, which is a defect and
never something a program can cause by itself, ends the same way.

## Structured results and faults

Two things a host that runs scripts for other people needs are more than the
text above gives it: the value a script produced, as data, and the fault a
script ended with, with its code and position. Both are additions: nothing
above changes, and the text results and the `Error` message a fault always
had are exactly what they were.

### Results as values

`loadProgram` and `loadProgramBatched` take a second argument,
`{ resultFormat: "json" }` (the default is `"text"`). Every result of that
instance, its `loadResult`, `call`, `read` and, on the batched path, the
`value` of a finished `call` or `resume`, is then the value itself, not its
rendering:

```js
const instance = vm.loadProgram(bytecode, { resultFormat: "json" });
instance.loadResult;        // what a top-level `return` produced, as data
instance.call("total");     // 42, not "Int(42)"
instance.call("tags");      // { set: [2, 9007199254740993n] }
instance.call("nothing");   // undefined: the function returns nothing
```

The value is in the encoding of the table above (`{ list: [...] }`,
`{ record: { ... } }`, `{ enum: { name, variant, payload } }` and so on), the
same one that carries arguments and capability answers, read with
`JSON.parse`, with these rules:

- **An `Int` is never read back as a different `Int`.** JSON has one number
  type and a JavaScript number is a double, so an `Int` above 2^53 - 1 in
  magnitude cannot be a number without changing. An `Int` whose magnitude is at
  most 2^53 - 1 (`Number.MAX_SAFE_INTEGER`) is a plain number; any other is a
  `BigInt`. On the wire (`hs_wasm_set_result_format(1)`) that `Int` is the
  tagged decimal string `{"bigint": "9007199254740993"}`, which
  `hollowscript.js` turns into the `BigInt`; a host that reads the JSON text
  itself reads that tag. The tag is outbound only, and it is found only where a
  value is, so a record with a field called `bigint` is still a record. This
  covers a plain value, a list or set element, a map key or value, a record
  field and an enum payload.
- **`Float` and `Int` are told apart by the program's declared type, not by
  the JSON a JavaScript host reads.** On the wire they differ: a `Float` is
  always written with a decimal point or an exponent (`3.0`, `1.5`, `1e21`,
  `5e-324`) and an `Int` never is (`3`), and a `Float` is the shortest text that
  reads back as the same double, so none of its value is lost. But `JSON.parse`
  reads `3.0` and `3` as the same `Number`, and a `Float` above 2^53 (the literal
  `9007199254740993.0` is the double 9007199254740992) as the `Number` that is
  exactly that double. So a `Float` that happens to be whole cannot be told from
  an `Int` in a value `hollowscript.js` returns; the type of the exported
  function says which it is. A host that must keep the two apart where the type
  does not say (a value the program built from a `Json` enum, whose `Int` and
  `Float` variants are named, is already apart) reads the text format, which
  writes `Int(3)` and `Float(3.0)`, or reads the wire text with a parser that
  keeps the decimal point.
- **No value and `none` are different.** A function that returns nothing
  answers `undefined`; `none` answers `null`. (In the text format they are
  `null` and `"None"`.)
- A result can be nested as deeply as the memory limit allows; neither the VM
  nor the wrapper recurses over it.

**A capability handler's arguments are exact too.** An `Int` above 2^53 - 1
that a program passes to a capability arrives as a `BigInt` in the handler's
`args` (`9007199254740993n`, not `9007199254740992`), at any depth, in the
same tagged form as a result; one at or below it is a plain number. The same
holds for the `args` of a queued command on the batched path. A handler (or a
batched answer, or an argument a host passes to `call`) may answer with a
`BigInt` too: it is written as an exact integer literal with `JSON.rawJSON`,
which Node 21 and current browsers have, and a platform without it throws a
`RangeError` for one rather than write a different number.

**Everything the module writes out for a host is bounded.** A value can share
its sub-values (`t = Tree.Node(t, t)` fifty times is fifty objects and 2^50
paths), and JSON has no way to name a value once, so such a value written out
is astronomically long although it is small in memory and passes the memory
limit. The JSON of a result (`loadResult`, `call`, `read`, the `value` of a
batched run), of the arguments of one capability call and of the commands one
`pendingCommands` read drains is therefore written within a budget: the
output limit, `maxOutputBytes`, and never less than 64 KiB (so a host that sets
a very low output limit to stop `print` still exchanges ordinary values). The
text is charged as it is written, every piece is at least a byte, so the work
is bounded however much of the value is shared, and the walk asks the host's
cancellation (`cancelFlag`, `deadlineMs`, `shouldCancel`) every 1,024 pieces. When the text
would pass the budget the call faults with HS1004, a `HollowScriptFault` like
any other, with `data: { option: "maxOutputBytes", limit, used }` where `limit`
is the budget in force; when the host cancels it faults with HS1006.

- A fault while writing a **result** is raised after the run has finished, so
  the instance is exactly as after any call that returned (state kept, usable
  for the next call); only the value could not be delivered.
- A fault while writing the **arguments of a capability call** is the call's
  fault: the host's function is not called and the run faults.
- A fault while draining **queued commands** (`pendingCommands`, the batched
  path) leaves a run that cannot be resumed, since its commands were taken to
  be written. Dispose of the instance.
- The budget is its own allowance for each writing, not shared with what
  `print` has written. The text format keeps its own bound (16 MiB, ending in
  a truncation marker) and is unchanged.

### Faults as objects

A program that faults, at load, in a `call`, or in a `resume`, now throws a
`HollowScriptFault`, which is an `Error` (so `message` is the text it always
was, and `instanceof Error` and `catch (e)` code are unaffected) with the
parts of a fault that section 17.1 defines:

```js
try {
  instance.call("divide", [0]);
} catch (error) {
  if (error instanceof HollowScriptFault) {
    error.code;          // "HS1008"; "HS1006" for a cancelled run
    error.class;         // "domain" ("resource", "host" or "internal")
    error.file;          // "main.hws"
    error.line;          // 3, 1-based
    error.column;        // 12, 1-based, counted in code points
    error.spanStartByte; // 41: the half-open span, in bytes of the file's UTF-8
    error.spanEndByte;   // 47
    error.frames;        // [], the innermost call frames (see below)
    error.data;          // for HS1001 to HS1005: { option: "maxSteps", limit: 5000, used: 4096 }
  }
}
```

`file`, `line`, `column`, `spanStartByte` and `spanEndByte` are `null` when
the bytecode was built with its source positions stripped. `data.limit` and
`data.used` are numbers, or `BigInt`s where a number would not be exact.
`frames` is part of the record but the VM does not report call frames yet, so
it is empty today.

The span is in bytes because the VM has no source text: bytecode holds
positions, not the program. For a UTF-16 position (what a text editor counts
in), a host that kept the source converts it with `byteOffsetToUtf16(source,
fault.spanStartByte)`; `line` and `column` need no conversion. A failure that
is not a fault of the program, a call refused because the export does not
exist, bytes that are not bytecode, a missing capability handler, stays a
plain `Error`.

Both are in the module as two exports: `hs_wasm_set_result_format(format)`
chooses text (`0`, the default) or JSON (`1`) for every result written to the
text channel until it is changed again, and `hs_wasm_fault_json()` writes the
fault of the last call that ended in one to the text channel as JSON, taking
it, or writes nothing. `hollowscript.js` sets the format around each call and
puts it back, and reads the fault only after a call that failed.

`web/node-e2e/results-check.mjs` runs all of this against a program that
returns every kind of value, with an `Int` above 2^53 in each position, and
faults in several ways.

## Cancellation

A page that runs the VM in a Worker can cancel a call in progress from its
controller thread, which is the situation `postMessage` cannot reach (the
Worker's event loop is not running while WebAssembly runs synchronously):

```js
const cancelFlag = createCancelFlag(); // an Int32Array over a SharedArrayBuffer
const vm = await HollowScript.load(url, { cancelFlag });
// ... share the same SharedArrayBuffer with the Worker, e.g. via postMessage
// in workerData, and construct its own HollowScript with the same option ...

// from the controller, while the Worker is blocked inside instance.call(...):
cancel(cancelFlag);
```

`SharedArrayBuffer` needs the page to be cross-origin isolated (the
`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp` response headers); without
that, `createCancelFlag()` throws, and a page can still run the VM, just
without cancellation from another thread. `web/node-e2e/` is a runnable,
worked example of exactly this: two capabilities bound from JavaScript and a
`while true {}` call cancelled from the main thread while a Worker runs it.

### A deadline, with no Worker

A host that has no Worker (a server process that runs an untrusted script on
its own thread, say) cannot set a flag while the VM is running, but it does
not need to: the VM asks the host at every safepoint whether to stop
(section 11.7), and that question is answered by the host's own code, on the
same thread. Wall-clock time is not a language limit (section 11.1); it is
the host's job, and these two options are how the wrapper does it:

```js
// each run (the load of a program, every call, every resume) gets 250 ms
const vm = await HollowScript.fromBytes(wasmBytes, { deadlineMs: 250 });

// or any condition of the host's own, asked at every safepoint
const vm2 = await HollowScript.fromBytes(wasmBytes, {
  shouldCancel: () => performance.now() > stopAt || requestWasAborted,
});

vm.setDeadline(1000); // changes the budget of every run that follows
vm.setDeadline(null); // removes it
```

- `deadlineMs` is a budget for each run, measured on a monotonic clock
  (`performance.now()`) from the moment that run starts. A call that has used
  it up stops at its next safepoint with the fault HS1006, "The run was
  cancelled.". A suspended batched run is not charged for the time it spends
  waiting for the host: `call` and every `resume` each get the whole budget.
  It must be a finite number greater than 0 (anything else is a `RangeError`).
- `shouldCancel` is a function that answers `true` to stop. It is asked at
  every safepoint: at least once per 1,024 steps of the program's own work,
  and again after every capability call, so a tight loop is stopped within
  1,024 steps of the answer turning `true`. It must be quick, and it must not
  call into the instance. If it throws, the run is cancelled, since a host
  bug must never leave a run unwatched.
- `cancelFlag`, `deadlineMs` and `shouldCancel` compose: the flag is read
  first, then the deadline, then the callback, and the first to say stop ends
  the run (section 11.7's order: the token, then the host's callback). A
  cancelled run leaves the instance usable for the next call.

A deadline can only be noticed at a safepoint, so it does not interrupt a
capability handler that is itself taking a long time (section 11.7 makes
that wait the capability's own responsibility); it is noticed as soon as the
handler returns. `web/node-e2e/deadline-check.mjs` runs an endless loop under
each option on one thread and asserts that it is cancelled quickly.

## Suspend and resume (an asynchronous host)

`bindCapability`'s handler must answer synchronously: nothing in
`hs_capability_call` can make a WebAssembly import wait for a JS `Promise`.
A host that must answer a capability call asynchronously (`await
fetch(...)`, or post a message to a main thread and wait for its reply,
without `SharedArrayBuffer`, since the page may not be cross-origin
isolated) cannot use `loadProgram`/`Instance` at all. `loadProgramBatched`
is the other path, built on section 9.6's "deliberate simplifications"
command-buffer mechanism: instead of blocking, a run pauses
(`{ suspended: true, commands }`) at every capability call, and the host
resumes it with the answers once it has them, as many times as the program
keeps calling a capability, until it finishes or faults. No
`SharedArrayBuffer` and no cross-origin isolation are needed anywhere in
this path, because nothing in it ever blocks.

```js
const vm = await HollowScript.fromBytes(wasmBytes); // no cancelFlag needed
const instance = vm.loadProgramBatched(bytecode);

let result = instance.call("run");
while (result.suspended) {
  const answers = [];
  for (const command of result.commands) {
    // command = { function, name, args }; name is "clock.now"-qualified,
    // the same as bindCapability's key, and args is a plain JS array.
    const value = await answerAsynchronously(command.name, command.args);
    answers.push(value);
  }
  result = instance.resume(answers);
}
console.log(result.value); // the rendered result, or null
```

Each answer in `resume`'s array is either a plain JSON-compatible value (or,
for a capability declared `Result<T, E>`, `{ Ok: value }`/`{ Err: value }`,
exactly as the synchronous path's capability results already work), or an
`Error`, meaning that one call itself failed (HS1012), not a value the
program can match. `resume`'s array must have exactly one answer per
command in the `commands` it is answering, in the same order.

Cancelling a suspended run needs nothing new: `env.hs_wasm_poll_cancel`
still governs every synchronous slice (the run between one `call`/`resume`
and the `Suspended` it answers, or the run's own completion), the same as
on the synchronous path, and between slices nothing here is running at all,
so a host that wants to cancel a suspended instance simply does not call
`resume` again and calls `instance.dispose()` instead. Limits
(`setLimits`) hold across every suspension the same way, since the same
`Limits` govern every slice of the one run.

`web/node-e2e/async-check.mjs` is a runnable, worked example: a program
calls one capability eight times in a loop, each answered after a real
`setTimeout` (standing in for whatever a real async host would `await`),
proving a run can be suspended and resumed more than once, not only at its
first capability call.
