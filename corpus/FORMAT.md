# Conformance corpus format

This directory holds a language-neutral conformance corpus for HollowScript. Every case is a program and a description of what any correct implementation does with it. An implementation runs each program through its own front end and compares what it observes with the expected file, using the rules below. The specification defines the same format in [Chapter 19](../docs/spec/19-conformance.md); where this file and that chapter differ, the chapter governs and this file is corrected.

## Layout

```
corpus/
  <area>/
    <name>.hws               the program
    <name>.expected.json     what a conforming implementation does with it
    <name>.modules/          optional: other modules the program imports
  spec-v1-pending.json       cases for version 1 behaviour (see below)
  FORMAT.md                  this file
```

The case id is the path without the extension, for example `expressions/int-overflow`. An area directory groups cases by the part of the language they exercise: `lexical`, `structure`, `modules`, `types`, `expressions`, `narrowing`, `statements`, `capabilities`, `stdlib`, `scope`, `diagnostics`, `formatting`, `integers`, `collections`, `enums`, `destructuring`, `results`, `grants`, `filters`, `widths` and `examples`. `widths` holds programs with very wide or very long literals, calls, records, strings, functions and bodies, whose widths section 1.10 bounds only by the size of a source file. The `guide` directory holds every complete sample from the language guide that runs cleanly; its expected files record what running the sample produces. `filters/` holds cases for Appendix A's filter language: a filter literal's grammar, its value classification and its structural limits, checked as an ordinary part of the program (`check`, `format`); validation against a schema (section A.7) is pinned by the cases that give a `<name>.host.json` a `filterSchema` (section 19.4). `matches` itself is not reachable from a program (Appendix A section A.1: "the language deliberately offers no way to evaluate a filter in the standard library"), so it is covered by `hollowscript-core::filter`'s own unit tests against section A.11's vectors instead, not by this corpus.

Case names describe the rule a case pins, not how the case came about. A case title is one sentence stating the rule.

### The entry module and imports

The program of a case is the entry module and is addressed as `main.hws`, whatever the file is called. Its imports resolve relative to that root. Files the program imports live in `<name>.modules/`, laid out by canonical path: `import x from "./dates"` reads `dates.hws` in that directory, and `sub/util.hws` is stored as `<name>.modules/sub/util.hws`. A path that climbs above the root, such as `../shared`, is not stored: the loader reports it as not found, and the case checks that the checker refuses the import (HS0605) before the loader is asked. A runner that works on real files lays a case out as

```
<dir>/main.hws               the program
<dir>/dates.hws              from <name>.modules/
```

and runs `main.hws`.

### The host

Every case runs against the same fixed host:

| Capability | Behaviour |
| --- | --- |
| `print(text: String)` | appends `text` to the case's output, one entry per call, without a newline |
| `clock.now(): String` | always `2026-01-01T00:00:00Z` |
| `clock.today(): String` | always `2026-01-01` |
| `clock.unixMillis(): Int` | always `1767225600000` |
| `clock.monotonicMillis(): Int` | always `0` |
| `random.float(): Float` | always `0.5` |
| `random.int(min: Int, upTo: Int): Int` | `min + floor(0.5 * (upTo - min))` |
| `kind(x: Float): String` | the runtime kind of the argument as received, `"Float"` or `"Int"`, so a case can observe whether an `Int` was promoted at a host boundary |

`kind` exists only for the corpus. A case that grants nothing else uses only these. Where a case needs further grants, its expected file lists them in `grants` (see below): this implementation's runner reads them and grants `fs` and `http` for real, scoped exactly as the case asked. A case that needs standard input, arguments, environment variables, files, canned network exchanges, different limits or a filter schema says so in `<name>.host.json`, which [section 19.4](../docs/spec/19-conformance.md#194-the-corpus-host) defines; this runner reads `files` and `http` from it (`crates/hollowscript-conformance/src/hostconfig.rs`) and `limits` (`crates/hollowscript-conformance/src/host_file.rs`), but not yet `stdin`, `args`, `env`, `filterSchema`, `yieldCancelAt` or `entryPath`, because no case has needed them so far.

The default resource limits apply to every case unless the host file changes them.

## The expected file

```json
{
  "title": "One sentence stating the rule the case pins",
  "spec": "4.8",
  "grants": ["fs:read=./data"],
  "parse":  [ Diagnostic ],
  "check":  [ Diagnostic ],
  "format": { "output": "same", "diagnostics": [ Diagnostic ] },
  "run":    { "ok": true,  "output": [ "line" ], "result": "Int(3)" }
}
```

`run` has a second form for a program that does not complete:

```json
"run": { "ok": false, "stage": "check", "output": [ "line" ], "diagnostics": [ Diagnostic ] }
```

| Key | Meaning |
| --- | --- |
| `title` | Required. A sentence stating the rule. |
| `spec` | Optional. The section of the specification that the case tests. |
| `grants` | Optional. Grant specifications given to the implementation for this case, each as one grant as the command line would take it after its allow flag. Omitted means the default host only. |
| `parse` | Required. Diagnostics from lexing and parsing alone. |
| `check` | Required. Every diagnostic from checking the program and everything it imports: syntax, imports, capability grants, types, scope, and warnings. It is a superset of `parse`. |
| `format` | Optional. What the formatter does. When absent the formatter is not pinned by the case. |
| `run` | Required. What running the program does. |

### Diagnostics

```json
{ "severity": "error", "line": 3, "code": "HS0201", "file": "main.hws", "contains": "declared" }
```

| Key | Meaning |
| --- | --- |
| `severity` | Required. `"error"` or `"warning"`. |
| `line` | Required. The 1-based line on which the offending construct starts. A `\r\n` pair counts as one line terminator, and a leading byte order mark does not count as a character. |
| `code` | A stable diagnostic code of [Chapter 17](../docs/spec/17-diagnostics.md). Chapter 19 requires it, and it is compared when present. Cases written before the registry existed do not carry codes yet, and are completed as part of the release. |
| `file` | Optional. The module the diagnostic is attributed to (`main.hws` for the entry). Compared when present. |
| `contains` | Optional. A substring the diagnostic message must contain. Compared when present. Used only where the specification fixes part of the wording. |

A list of diagnostics is compared as a whole. The number of diagnostics must be equal, and after both lists are put in the canonical order of `(file, line, severity with errors first, code)`, where files are ordered by load order with the entry module first, each pair must agree on every key the expected diagnostic gives. Message text is otherwise never compared. Cases that only test the order in which diagnostics are reported are separate ([section 17.1](../docs/spec/17-diagnostics.md#171-the-diagnostic-record)).

A warning is reported even when the file also has errors, so a `check` list can mix both.

### Output and result

`output` is the list of strings passed to `print`, in order. It is kept when a run fails part way, so it shows what was written before the failure.

`result` is `null` when the program ends without executing a top-level `return`, and otherwise the rendering of the returned value:

| Value | Rendering |
| --- | --- |
| `Int` | `Int(` decimal digits `)`, with a leading `-` when negative |
| `Float` | `Float(` the `Float` text of [section 3.9](../docs/spec/03-expressions.md#39-string-interpolation) `)`, for example `Float(2.0)` and `Float(1e-6)`; there is no negative zero |
| `Bool` | `Bool(true)` or `Bool(false)` |
| `String` | `String(` the string as a JSON string literal `)` |
| `Filter` | `Filter(` the filter source as a JSON string literal `)` |
| `none` | `None` |
| list | `[` elements each rendered, separated by a comma and a space `]` |
| record | `{` `name: value` pairs in ascending field name order, separated by a comma and a space `}` |
| `Map` | `Map{` `key: value` pairs in ascending key order, separated by a comma and a space `}` |
| `Set` | `Set{` elements in ascending order, separated by a comma and a space `}` |
| enum value | `Enum.Variant(` payloads separated by a comma and a space `)`, or `Enum.Variant` when there is no payload |
| function | `Function` |

### The four stages

**parse.** The diagnostics from syntax analysis alone.

**check.** All diagnostics from checking without running.

**format.** `output` is one of:

- `"same"`, the source unchanged, because it is already in canonical form;
- an array of lines, which are the formatted text joined with `\n` (a trailing newline is present in the formatted text, so the last array entry is the empty string when the text ends with one);
- `null`, when the formatter must refuse because the source has an error.

`diagnostics` lists what the formatter reports, which is the parse errors when it refuses. In addition, formatting the formatted text must reproduce it exactly and must succeed.

**run.** Runs the program. When the program checks with no errors and runs to completion, `ok` is `true`. Otherwise `ok` is `false` and `stage` says how it stopped:

- `"check"`: the program has at least one error, so nothing ran. `output` is empty and `diagnostics` is the same list the check stage reports, warnings included.
- `"run"`: the program checked cleanly and then failed while running, for example a division by zero or an overflow. `diagnostics` holds the runtime error, located at the line where the failing operation starts. `output` holds whatever was printed first.

Warnings never prevent a run.

## Pending cases

`spec-v1-pending.json` lists cases for behaviour that version 1 of the language defines: 64-bit `Int` arithmetic with checked overflow and the `div` and `mod` functions, `Map`, `Set`, enums, destructuring, `Result` with the `?` operator, scoped capability grants and a few syntax rules. A conforming version 1 implementation passes every case.

```json
{
  "cases": [
    {
      "id": "integers/div-floors",
      "feature": "div",
      "decision": "Integer division is provided as a function on Int",
      "dependsOnChoice": true,
      "choices": ["div rounds towards negative infinity rather than towards zero"]
    }
  ],
  "mayChange": [ { "id": "types/result-type", "reason": "..." } ]
}
```

The expected results of these cases were written from the decisions, not observed from any implementation. When `dependsOnChoice` is true the expectation also rests on a detail listed in `choices`: a name, a piece of syntax, an ordering. When the specification settles such a detail, the program or the expectation is made to match and the choice is dropped from the list. Unless a choice says otherwise, a pending case that expects an error assumes that no follow-on diagnostic is reported for a name whose own declaration failed.

`mayChange` lists cases outside the pending set that are likely to change when version 1 lands, with the reason.

## Looseness to be aware of

A few cases pin how many diagnostics one mistake produces (for example a reserved word used as a name). Where the specification leaves that open, those cases should be relaxed rather than an implementation bent to match them.

## Corrections made to this corpus

Where a case and the chapter it tests disagreed, the chapter governed and the case was corrected, as the first section of this file says. The corrections so far:

- **Variant patterns were written with their enum name** (`Shape.Square(side)`) in fifteen cases of `enums/` and `results/`. [Section 7.2](../docs/spec/07-patterns.md) is explicit that a variant pattern is written without it, "because the type of the scrutinee tells which enum is meant (`Circle(r)` and not `Shape.Circle(r)`)". The construction form `Shape.Square(3)`, which [section 6.3](../docs/spec/06-data-types.md) does write with the enum name, was left alone: only pattern positions changed.
- **`Result` was matched with `success` and `failure`** in the same cases. Those are the two prelude *functions* that build a `Result` ([section 8.2](../docs/spec/08-errors-and-results.md)); its patterns are `Ok(v)` and `Err(e)`, as for any other enum.
- **Two assignment cases expected no unused-name warning.** `statements/assign-constant` (`constant a = 1` then `a = 2`) and `statements/assign-type-mismatch` (`variable a = 1` then `a = "x"`) each listed only the error about the assignment. [Section 17.3](../docs/spec/17-diagnostics.md) reports HS0801 "for every name that a `constant` or `variable` declaration binds, local or top-level ... when it is never read", and says in the same paragraph that "Assigning to a `variable` is not a use". In both cases nothing reads `a`, so each gains the warning it earns.
- **A whole `Float` was written without its decimal point** in seven cases of `lexical/`, `expressions/`, `stdlib/` and `types/`: `Float(1000)`, `Float(2)`, `Float(1024)`, `Float(9007199254740994)` and the rest. [Section 3.9](../docs/spec/03-expressions.md#39-string-interpolation) states the rule and then its consequence outright: "a whole `Float` always shows a decimal point or an exponent", and its own table gives `100.0` for `100.0`. Each now reads `Float(1000.0)`, `Float(2.0)` and so on.
- **Five cases named a line two above the construct they are about**, in
  `narrowing/` and `capabilities/`. Two of the five named a blank line, which
  is what makes this a slip rather than a reading: a diagnostic cannot be on
  one. In each, the line two below is the construct
  [section 17.1](../docs/spec/17-diagnostics.md) gives the code's span
  ("the expression" for HS0301, "the operator expression" for HS0311, "the
  call" for HS0401), so each expected line moves down by two.
- **Six more cases named a line the code's span is not on**, in
  `statements/` and `narrowing/`. [Section 17.1](../docs/spec/17-diagnostics.md) gives HS0344
  the `break` or `continue`, HS0504 the pattern and HS0501 the `match`
  keyword; each expected line moves to that construct. One of them named a
  `print` call and another a closing brace.
- **`stdlib/result-values` wrote `Result<Int>` and expected a record.**
  [Section 8.2](../docs/spec/08-errors-and-results.md) states both halves
  outright: "Both type arguments are required; `Result<Int>` is HS0303", and
  `Result` is "a built-in enum". The source now writes
  `Result<Int, String>`, and the expected result is the enum rendering this
  file's own table gives, `Result.Ok(Int(1))`, not `{value: ..., error: ...}`.
- **Two more cases expected no unused-name warning**,
  `modules/import-missing-module` and
  `modules/import-private-declaration`, each naming only the error about
  the import. [Section 17.3](../docs/spec/17-diagnostics.md) says warnings
  "are reported whether or not the program has errors", and in both cases
  nothing reads the name the file declares.
- **Five of the six `grants/` cases modelled a denied `fs` or `http` call as
  a fault, and two called `fs.readText`/`fs.writeText`, names `fs` does not
  have.** [Section 9.5](../docs/spec/09-effects-and-capabilities.md) rule 3
  is explicit that a call outside the granted scope "does not perform the
  operation and reports the refusal to the program in the way its own
  definition says": for `fs` and `http`, `Err(Denied(_))`, a value the
  program receives from the call it made, not a fault it never gets to
  handle. [Section 16.8](../docs/spec/16-standard-capabilities.md#168-fs)
  and [16.9](../docs/spec/16-standard-capabilities.md#169-http) name the
  functions `fs.read`, `fs.write` and `http.get`. `host-must-match-exactly`,
  `host-outside-the-grant`, `parent-segments-cannot-escape`,
  `read-outside-the-granted-path` and `write-needs-a-write-grant` now
  `match` the `Result` the call actually returns and print `"denied"` for
  `Err(Denied(_))`, and the two `fs` cases call `fs.read`/`fs.write`. This
  was also the reason none of the six could run at all: the corpus host
  could not yet give a case a scoped grant. It can now
  ([`crates/hollowscript-conformance/src/hostconfig.rs`](../crates/hollowscript-conformance/src/hostconfig.rs)
  reads a case's `grants` and, per
  [section 19.4](../docs/spec/19-conformance.md#194-the-corpus-host), a
  `<name>.host.json`'s `files` and `http`), so `one-grant-does-not-imply-another`
  needed no change to its own content, only a host able to grant `fs`
  without also granting `http`.
- **`collections/map-set-and-get` named a parameter `scores`, the same name
  as a top-level `constant` declared later in the file.**
  [Section 4.6](../docs/spec/04-statements.md#46-scope-and-shadowing) is
  explicit that this is HS0201, "even when that declaration comes later in
  the file": that is what "top-level names in function bodies" means. The
  parameter is now `table`, which the case's own body never otherwise names.
- **`destructuring/names-may-not-shadow` listed only the HS0201 its own
  title is about, not the HS0801 that follows from it.** The pattern
  `constant { x, y } = point` still declares `x` even though shadowing
  the earlier `constant x = 1` refuses it (a declaration is "declared even
  after a clash", the same rule a local's own clash follows), so the
  original `x` is genuinely never read again, and
  [section 17.3](../docs/spec/17-diagnostics.md) reports that regardless of
  the error already found. The expected file now lists both. The
  implementation itself doubled the HS0201 here too, reporting it once from
  `check_declaration_pattern` and a second time from its caller checking
  the identical, unchanged condition again; fixed to report it once.
- **`lexical/reserved-word-identifier` and `statements/match-else-last`
  each listed the same diagnostic repeated several times over** (four and
  twelve entries respectively, all naming the one real mistake). Both are
  now the one diagnostic the case's own program actually raises.
- **`statements/expression-statement` and `statements/no-field-assignment`
  each expected an unused-name warning that does not hold.** Both name a
  binding in an expression that section 4.9/HS0109 refuses as a bare
  statement (`a + 1`, `r.a = 2`); the checker still checks that expression,
  which is a genuine read of `a`/`r`, so
  [section 17.3](../docs/spec/17-diagnostics.md)'s "a name is used if a
  reference that resolves to it is read anywhere" already gives it a use.
  The spurious warnings are removed.
- **`modules/import-basic` expected a warning that nothing in the case
  justifies**: both imported names (`double`, `base`) are read on the very
  next line. Removed.
- **`modules/import-unused` listed its one HS0802 warning twice.** Reduced
  to one.
- **`modules/import-cycle` never read the constant its import fed, so it
  also earned an unused-name warning the expected file did not list.**
  Rather than pin that interaction, the case now returns the value
  (`return x`), which is closer to its own title, and needs no warning.
- **`modules/module-return-ends-initialisation` tested a top-level `return`
  in a non-entry module and expected the run to still complete, with only a
  warning.** [Section 4.7](../docs/spec/04-statements.md#47-return) states
  the opposite outright: "A top-level `return` in any other module is
  HS0611", with no exception; section 10.5 repeats it ("A top-level
  `return` is allowed only in the entry module (HS0611 elsewhere)"). There
  is no feature under which a non-entry module's own `return` merely "ends
  that file"; every reading of "ends the file's initialisation" in section
  4.7 is about the entry module. Replaced by
  `modules/top-level-return-outside-the-entry-module`, which asserts the
  rule the chapter actually states.
- **`results/question-mark-at-the-top-level` was titled "the ? operator is
  an error outside a function", which section 4.7 contradicts as directly
  as the case above**: "A top-level `?` that meets `Err(x)` ends
  initialisation with the result `Result.Err(x)`" is the same allowance a
  top-level `return` gets, not an error. The case's own program did not
  test what its title claimed: its real failure was a match arm binding
  named `value` inside one function, colliding with a top-level `constant
  value` declared later in the file (section 4.6 again), which masked a
  real defect the corrected case now exercises directly: bytecode
  verification refused `Op::Try` in a module's own init "function" whenever
  `returns_value` was false, which it always is for one (it declares no
  return type to be false or true about), even though the check just above
  it already carved out the identical exception for `Op::Return`. Fixed in
  `hollowscript-bytecode`'s verifier and replaced by
  `results/top-level-question-mark-ends-initialisation`, which asserts
  section 4.7's own rule with no incidental name clash.
- **`enums/exported-enum-can-be-imported`'s imported module wrote its
  `match` patterns with the enum name** (`Shape.Square(side)`), the same
  mistake the "variant patterns were written with their enum name"
  correction above describes; this file, inside `.modules/`, was missed by
  that pass. Corrected the same way: the enum name removed from each
  pattern.
- **`lexical/int-literal-out-of-range` expected the out-of-range `Int`
  literal to be a parse error.** [Section 1.4](../docs/spec/01-lexical-structure.md#14-number-literals)
  says the lexer accepts the exact value 9223372036854775808 and only the
  *checker* refuses it outside the two positions section 1.4 names (HS0014).
  `constant big = 9223372036854775808` is neither position, so it is a
  check-time error, not a parse-time one; the formatter does not refuse it
  either, since nothing is syntactically wrong. Moved from `parse` to
  `check`, format now `"same"`.
- **`lexical/utf8-bom` expected the formatter's output without the blank
  line the formatter puts between every pair of top-level statements.** A
  stale expectation from before that convention; updated to match.
- **`statements/match-needs-else` named the closing `}` of the `match` as
  HS0501's location, and expected it as a parse error.**
  [Section 17.1](../docs/spec/17-diagnostics.md) gives HS0501 "the `match`
  keyword" as its span, and non-exhaustiveness is a check-time question, not
  a syntax one. Moved to `check`, at the `match` keyword's own line; the
  case's own indentation was also not canonical, so `format` is now
  `"same"` rather than refusing.
- **`types/generic-fn`'s `firstOr([], 9)` could not infer `T` from the
  first argument alone and was rejected with HS0304 before the second
  argument, which does determine it, was ever consulted.**
  [Section 2.10](../docs/spec/02-types.md#210-generics)'s own aside on
  `set([])` states the general rule this is a case of: a variable "only an
  empty list or map literal argument could have bound" is reported at the
  end, HS0308, "not HS0304", which only holds if such an argument's
  checking is deferred exactly as a closure literal's is, until every other
  argument has bound what it can. `check_call_args` deferred closures only;
  it now defers an empty list or map literal argument the same way, which
  is also what makes `types/generic-fn` pass without changing the case.
- **`types/result-type` wrote `Result<Int>`, the same one-argument mistake
  `stdlib/result-values` already had corrected above, and additionally
  divided with `/`, which always returns `Float` (section 13.2), where its
  own declared type was `Int`.** Now `Result<Int, String>`, and `div(n, 2)`
  in place of `n / 2`.
- **`diagnostics/hel-error-offset` expected a filter literal's HEL syntax
  error to be reported by `parse` and to make `format` refuse.**
  [Chapter 17](../docs/spec/17-diagnostics.md)'s own registry gives HS0701
  to HS0711 the stage `check`, not `parse`: a filter literal
  (`` `status:` ``) is syntactically an ordinary, opaque HollowScript
  literal ([section 1.6](../docs/spec/01-lexical-structure.md#16-filter-literals)),
  and only checking it unescapes and parses the text between the backticks
  against Appendix A ([section A.1](../docs/spec/appendix-a-filters.md#a1-overview)).
  This implementation's formatter never checks a program (it "has no need
  of imports", `hollowscript/src/formatting.rs`), so it has no way to see
  a HEL error either, and correctly leaves the literal's text untouched.
  `parse` is now `[]` and `format` is now `{"output": "same", "diagnostics":
  []}`; `check` and `run`, which already expected the HS0701 and the
  unused-constant warning, are unchanged.

## Disagreements this repository has not resolved

None at present. Both baselines,
`crates/hollowscript-conformance/known-failures.txt` and
`crates/hollowscript-conformance/spec-known-failures.txt`, are empty.

The last one listed here, whether a denied capability call ends the run
(the six `grants/` cases expected it to; [section 9.5](../docs/spec/09-effects-and-capabilities.md)
says the call reports `Err(Denied)` to the program instead), was settled by
the owner on 2026-09-23 in favour of the specification, and the six cases
were corrected to it; each correction is recorded above. A future
disagreement whose resolution would mean choosing what the language does,
rather than writing down what it already says, is listed here until the
owner settles it.

