# 19. Conformance

An implementation of HollowScript conforms to this specification if it produces, for every program in the shared conformance corpus, the outcome the corpus records. The corpus is the executable form of this specification: programs with the diagnostics, output and results that a correct implementation produces. It is language neutral, so any implementation, in any language, is checked against the same files. Where this text and a corpus case disagree, the case is wrong and is corrected; the corpus never overrides the specification.

## 19.1 What conforming means

An implementation conforms if it:

1. accepts and rejects programs exactly as this specification says, with the diagnostics of [Chapter 17](17-diagnostics.md) (same code, severity, file and line, in the canonical order), except that a program may exceed a documented limit of the implementation, which is reported with HS0901 or HS1015 ([section 1.10](01-lexical-structure.md#110-structural-limits));
2. runs accepted programs with the meaning defined in Chapters [3](03-expressions.md) to [16](16-standard-capabilities.md), including the resource limits of [Chapter 11](11-resource-limits.md) with the same counters and defaults;
3. implements the filter language of [Appendix A](appendix-a-filters.md) to the vectors of section A.11 and the cases of the corpus;
4. never crashes, hangs or exhausts its host on any input within the limits of [section 1.10](01-lexical-structure.md#110-structural-limits): parse, check and run return diagnostics or faults for every input, including arbitrary bytes.

Behaviour not covered by the corpus is still governed by this specification.

## 19.2 Layout of the corpus

The corpus is a directory tree `corpus/<area>/` of *cases*, where an area groups the cases for one part of the language. A case is a program and a description of the outcomes. The file `corpus/FORMAT.md` describes the same format for people who write runners; where it and this chapter differ, this chapter governs and the format document is corrected.

| File | Meaning |
|---|---|
| `<name>.hws` | the entry program, exactly as bytes |
| `<name>.expected.json` | the expected outcomes ([section 19.3](#193-the-expected-file)) |
| `<name>.modules/` | optional: the other modules of the case. A file is stored at its canonical path ([section 10.3](10-modules.md#103-import-paths-and-canonical-paths)), for instance `<name>.modules/sub/util.hws` for `sub/util.hws`. A path that would climb above the root is not stored: the loader for the corpus reports it as not found, and the case checks that the checker refuses the import (HS0605) before asking for it |
| `<name>.host.json` | optional: the host configuration of the case ([section 19.4](#194-the-corpus-host)); the default host if absent |

Cases are grouped in directories named for the chapter or topic they test (`lexical`, `types`, `patterns`, `limits`, `instances`, `filters`, and so on). Names are lower case with hyphens.

A separate directory `filters/` holds the filter vectors of Appendix A as data: a JSON file with a list of objects, each `{ "source": "...", "parse": "..." | { "error": "HS0701" }, "validate": [...codes], "matches": [true, false] }` against the schema and records given in the file. The rows of section A.11 are part of it.

## 19.3 The expected file

```json
{
  "title": "A short sentence stating the rule the case tests",
  "spec": "4.8",
  "parse": [ ],
  "check": [ ],
  "run": { "ok": true, "output": ["line one"], "result": "Int(3)" }
}
```

- `title` is a sentence. `spec` is the section of this specification the case tests. `grants` is an optional list of grants in the textual form of [section 9.5](09-effects-and-capabilities.md#95-scoped-grants), given to the implementation for this case in addition to the defaults of [section 19.4](#194-the-corpus-host).
- `format`, if present, records what a source formatter does with the program. The output of a formatter is not defined by this specification, so an implementation without a formatter ignores the member.
- `parse` lists the diagnostics that lexing and parsing alone produce ([Chapters 1](01-lexical-structure.md) and [18](18-grammar.md)), in the canonical order.
- `check` lists all diagnostics of checking the whole program, including those of `parse` and the warnings, in the canonical order ([section 17.1](17-diagnostics.md#171-the-diagnostic-record)).
- `run` says what happens when the program is loaded and run with the case's host.
  - `{ "ok": true, "output": [lines], "result": text-or-null }`: the program was accepted and finished; `output` is the lines the `print` capability wrote; `result` is the rendered value of the top-level `return`, or `null` if none ran.
  - `{ "ok": false, "stage": "check", "output": [], "diagnostics": [...] }`: the program was rejected; the diagnostics are the errors of the check (warnings are not listed).
  - `{ "ok": false, "stage": "run", "output": [lines], "diagnostics": [...] }`: the program faulted; `output` is what was written before the fault; the diagnostic is the fault.
- A case may also have `calls`: a list of exported-function calls made after loading ([Chapter 12](12-instances.md)), each `{ "name": "update", "arguments": [0.5], "result": "Float(1.0)" }` or `{ "name": "...", "arguments": [...], "error": "HS1202" }` or `{ "name": "...", "fault": "HS1001" }`, with `"output"` for the lines written during that call. Arguments are JSON values typed by the parameter's declared type: `Int` and `Float` are JSON numbers, read as exact decimal values; an `Int` parameter accepts only a number with no fraction and no exponent that fits in an `Int`, and any other number is HS1202; a `Float` parameter accepts any JSON number whose value is a finite `Float`; `Bool` is `true` or `false`; `String` is a JSON string; an absent optional is `null`; a list is an array; a record is an object with exactly the fields of the record; an enum value is `{ "variant": "Circle", "payload": [ ... ] }` with the payload omitted for a variant that has none; a map is an array of `[key, value]` pairs; a set is an array of its elements. Conversions apply to the top level of an argument only ([section 12.4](12-instances.md#124-calling-exported-functions)).

A diagnostic is `{ "severity": "error" | "warning", "code": "HS0301", "line": 3 }`, and `"file": "m.hws"` for a module other than the entry module.

**Comparison.** The expected and the reported diagnostics are each put in the order of `(file, line, severity with errors first, code)`, where `file` is ordered by load order with the entry module first, and are then compared pairwise as `(severity, code, file, line)`; the number of diagnostics must be equal. The message text and the exact span are not compared, so the order in which an implementation reports them is checked separately by cases that only test the canonical order of [section 17.1](17-diagnostics.md#171-the-diagnostic-record). A diagnostic may also carry `"contains"`, a substring of the message, where a case pins part of the wording. `output` lines are compared exactly. `result` is compared as text.

**Value rendering.** The `result` of a run and the `result` of a call are the *rendering* of a value:

| Value | Rendering |
|---|---|
| `Int` | `Int(5)`, `Int(-3)` |
| `Float` | `Float(` the `Float` text of [section 3.9](03-expressions.md#39-string-interpolation) `)`, for example `Float(2.0)` |
| `Bool` | `Bool(true)` |
| `String` | `String(` the JSON string form of the text `)`, for example `String("a\nb")` |
| `Filter` | `Filter(` the JSON string form of the filter source `)` |
| `none` | `None` |
| list | `[` renderings separated by `, ` `]` |
| record | `{` `name: rendering` pairs in ascending field name order separated by `, ` `}` |
| `Map` | `Map{` `key rendering: value rendering` pairs in ascending key order separated by `, ` `}` |
| `Set` | `Set{` renderings in ascending order separated by `, ` `}` |
| enum value | `Enum.Variant(` payload renderings separated by `, ` `)`, or `Enum.Variant` without payload |
| function | `Function` |

The *JSON string form* of a text is the text between double quotes in which only these are escaped, exactly as `json.stringify` writes a `String` ([section 15.1](15-stdlib-data.md#151-json)): `"` and `\`, the control characters U+0008, U+000C, U+000A, U+000D and U+0009 as `\b`, `\f`, `\n`, `\r` and `\t`, every other code point below U+0020 and U+007F as `\u00xx` with lower-case hexadecimal digits. Every other code point is written as itself in UTF-8, so `/` and U+2028 are not escaped.

## 19.4 The corpus host

Unless `<name>.host.json` says otherwise, a case runs against a host with these properties:

- Granted: `print`, `clock`, `random`, and the corpus-only capability `kind`. Nothing else is granted, so a program that says `uses fs` is HS0404.
- `print` adds the text of each call to the case's output as one entry, without the line feed.
- `clock.now()` is `2026-01-01T00:00:00Z`, `clock.today()` is `2026-01-01`, `clock.unixMillis()` is `1767225600000` and `clock.monotonicMillis()` is `0`.
- `random.float()` is `0.5`, and `random.int(min, upTo)` is `min + floor(0.5 * (upTo - min))`.
- `kind` is `effect fn(Float): String`. It returns the text `Float` if the value it received is a `Float` and `Int` if it received an `Int`, so a case can check that an `Int` argument was converted at the boundary ([section 9.6](09-effects-and-capabilities.md#96-capability-values)). Every implementation's corpus runner provides it.
- The limits are the defaults of [section 11.1](11-resource-limits.md#111-the-limits).
- The loader serves the files of `<name>.modules/` and the entry program; the entry's canonical path is `main.hws`.

A `host.json` may set any of these members:

| Member | Meaning |
|---|---|
| `limits` | overrides for `maxSteps`, `maxMemoryBytes`, `maxOutputBytes`, `maxCallDepth` |
| `stdin` | the text of standard input |
| `args` | the list of program arguments |
| `env` | an object of environment variables |
| `files` | an object mapping paths to file contents; directories are implied by their files; the base directory is the root of the object |
| `http` | a list of canned exchanges `{ "method": "GET", "url": "...", "status": 200, "headers": { }, "body": "..." }`; a request that matches none returns `Err(Failed)` |
| `filterSchema` | a filter schema ([section A.6](appendix-a-filters.md#a6-the-host-interface)) as JSON, used to validate filter literals |
| `yieldCancelAt` | the number of the yield callback invocation at which the callback returns *cancel* ([section 11.7](11-resource-limits.md#117-cancellation-and-yield)) |
| `entryPath` | the canonical path of the entry module, if not `main.hws` |

## 19.5 Coverage the corpus must have

The corpus MUST contain:

- at least one case for every rule of this specification that states an observable behaviour, each named in the case's `spec` member;
- at least one case that produces each diagnostic code of [Chapter 17](17-diagnostics.md), except HS1099, which reports an implementation bug and cannot be provoked, and HS0901 and HS1015, which report limits that differ between implementations; the corpus instead has cases wide and deep enough that no implementation limit may reject them;
- a case for each of the limits of Chapter 11, using `limits` to keep the case small, and one for cancellation using `yieldCancelAt`;
- a case for every function of the standard library, including its edge cases (empty inputs, negative and out-of-range arguments, Unicode above the Basic Multilingual Plane). A case does not depend on the last digit of the results of `exp`, `ln`, `pow`, `sin`, `cos`, `tan` and `atan2`: it rounds them with `fixed` or compares them with a tolerance;
- a case for each numeric edge in [section 13.2](13-stdlib-core.md#132-numbers) (rounding of halves, `div` and `mod` of negative numbers, overflow, the smallest `Int`, integers above 2^53);
- cases for the instance lifecycle of [Chapter 12](12-instances.md), including every error of [section 12.6](12-instances.md#126-faults-in-instances);
- cases that the parser and checker terminate with diagnostics on inputs at and beyond every structural limit;
- every example program in this specification, as a case (see below).

## 19.6 Examples in this specification

Every code block in this specification that is tagged `hollow`, `hollow-error`, `hollow-fault`, `hollow-fragment`, `hollow-module` or `hollow-module-error` is part of the conformance material. Blocks tagged `text` give expected output, and blocks tagged `json` are illustrative and are not programs.

| Tag | Meaning |
|---|---|
| `hollow` | a complete program that the checker accepts with no errors; if an `Output:` block follows, it is the exact output when run with the corpus host and the grants the text names, and if the text states a result, it is the result |
| `hollow-error` | a complete program the checker rejects; the line that carries the comment `// HSnnnn` is the line of the diagnostic with that code, and no other error is reported |
| `hollow-fault` | a complete program that the checker accepts and that faults when run; the comment names the code and marks the line |
| `hollow-fragment` | an excerpt that is not a program |
| `hollow-module`, `hollow-module-error` | one file of a program of several files; the first line is `// file: name.hws`; consecutive blocks together form one program whose entry module is `main.hws` |
| `text` | expected output |
| `json` | illustrative data, not a program |

## 19.7 Running the corpus

A corpus runner is a small program in the implementation's own language. For each case it loads the entry program and modules, runs `check`, `parse` and `run` with the case's host, renders the outcomes as in [section 19.3](#193-the-expected-file), and compares. A runner reports the case names that do not match and exits with failure if there are any. Two implementations are equivalent for the purpose of this specification if they produce the same outcomes for the whole corpus.

The corpus is versioned with the specification: a release of the specification is a tag of the corpus, and an implementation states the specification version it conforms to.
