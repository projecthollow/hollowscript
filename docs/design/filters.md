# Appendix A's filter language

`crates/hollowscript-core/src/filter.rs` is the whole of Appendix A: a
hand-rolled lexer, a recursive descent parser, value classification, a
schema validator, an evaluator and a canonical printer, behind the four
operations section A.1 names (`parse`, `validate`, `matches`, `format`). No
third-party dependency, matching `docs/design/dependencies.md`'s "today:
none" for this crate. This document is about the choices the appendix left
to an implementation, how the checker and a host reach the four operations,
and what comparing this implementation against `@hollow/expression` (HEL),
the pre-existing implementation of the same language behind Cascade's
`tasks` capability today, found.

## Why the whole thing is private except four functions

`Node`, `Value`, `FilterSchema` and the rest of the public types are exactly
what section A.5's tree and section A.6's schema and adapter describe; the
lexer, the parser's grammar functions, value-shape matching and the
canonical-date arithmetic are all private. A host that implements filters
(section A.1's "a host or tool") only ever needs the four operations and the
public data they take and return; nothing about how a `-` is told apart from
a `WORD` at lex time is anyone's business outside this module. Keeping that
boundary meant the parser's internal structure (a `Parser` over a flat
`Vec<Tok>`, one method per grammar production) could be written the
straightforward way without worrying about it becoming part of the crate's
API.

## Bounding the work (chapter 11)

A filter's resource accounting is structural, not a running counter. Section
A.10 fixes the two limits that matter: 4,096 bytes of source (HS0704) and
100 levels of nesting for groups and negations together (HS0703). The
parser checks the nesting limit *before* it recurses one level further
(`parse_not` and the `LPAREN` arm of `parse_primary`), so it cannot be
driven to exhaust its own native stack by adversarial input, independently
of the byte cap; the byte cap alone already bounds the token count to a
few thousand. Every tree `parse` can build is therefore bounded in both
size and depth by those two checks alone, and `validate`, `matches` and
`format` each do one linear pass over a tree that size, so none of the four
operations can be made to do unbounded work by a filter literal; this is
recorded in `filter.rs`'s own module doc comment, not only here, since it is
the kind of thing a reader auditing one function for "could this loop
forever" needs right where the code is.

This is also why filter evaluation needs no counter of its own from chapter
11: there is nothing resembling a `for` loop or a capability call inside
`matches` that steps could meaningfully be charged to per section 11.3, and
the structural caps above already do the job an explicit charge would do
for a construct with no such caps. What chapter 11 *does* charge is a
`Filter` value's node cost when the language allocates one (`16 + b` for a
source of `b` bytes, section 11.4's table) and a capability call that takes
a `Filter` argument (the ordinary `1 + floor(a / 64)` capability charge,
section 11.3); both of those are the interpreter's job, not this module's,
and unaffected by this work.

## Two places a filter literal is checked

Section A.1 says a filter literal is "parsed and checked when the program
is checked, and is a compile-time error if it is not valid," and section
9.7 gives a run's capability interface an optional second part: "a filter
schema for validating filter literals." Both needed a place to live in
`hollowscript-check`.

- `infer.rs`'s handling of `ast::Literal::Filter` calls `filter::parse`
  unconditionally and reports its error as an ordinary diagnostic, with the
  span mapped from the decoded filter source back to the file (section
  A.1's "adding the widths of the escape sequences before it"), which needed
  a new `hollowscript_syntax::token::filter_raw_offset`, kept next to
  `decode_filter_source` since the two encode the same escape rule in
  opposite directions.
- `Checker` carries an optional `&FilterSchema` (threaded from
  `hollowscript_check::check` down through `check_module`), and when a host
  gave one, every filter literal is also `validate`d against it, with one
  diagnostic per invalid predicate (section A.7's own limit) and the whole
  literal treated as invalid if any predicate is, the same as a syntax
  error. `hollowscript::Options::filter_schema` is section 9.7's item 2 at
  the facade boundary; every existing caller across the workspace passes
  `None` except the conformance host, which reads it from section 19.4's
  `filterSchema` member of a case's `host.json` (`hostconfig.rs`).

## Telling a host which filters a program holds

A host that evaluates filters wants to know, before it runs a program, which
ones the program can hand it, and to refuse a program whose filters name a
field the host does not have at check time rather than at run time. Both are
the checker's to say: it already visits every literal and already has the
schema. `Checker` records each literal it meets (decoded source, and the
location of the whole literal) before it parses or validates it, so that a
literal that is not valid, or that sits in a program with other errors, is
still listed; the checker puts each module's literals in source order and
keeps one of each, because it looks at an expression twice in places (the
arguments of an overloaded numeric call are checked to choose the overload,
and again). `hollowscript_check::CheckOutput::filter_literals` carries them,
grouped by module in the order modules were checked (a module before the
modules that import it, the entry module last), and
`hollowscript::filter_literals` is the facade's entry point: parse and check,
without the compile `check` also does, since a literal's source does not
depend on any of that.

A literal that appears at two places is two entries. The host evaluates by
the text, so it removes repeats if it wants each once; collapsing them
here would lose the places, which a host showing a diagnostic needs.

The WebAssembly `tooling` build exposes both: `check` and `compile` take a
filter schema in the JSON shape of section 19.4's `filterSchema`, read
strictly where the conformance host reads it tolerantly (a schema that is
silently read as something looser accepts what the host meant to refuse), and
`filterLiterals` answers the list with each span in UTF-16 code units. See
`crates/hollowscript-wasm/web/README.md`.

`matches` and `format` are not reachable from a program at all: section
A.1 is explicit that "the language deliberately offers no way to evaluate a
filter in the standard library", so they are exercised by this module's
own unit tests against section A.11's vectors, by the fuzz target below,
and by whatever a host does with the raw text it reads out.

## The host-facing shape

A host never receives a parsed tree from the VM: `HostCall::filter_of`
(`hollowscript-vm`, landed by the C ABI workstream, commit `24ac6d2`) gives
a capability parameter's filter as its source text alone, "exactly as it
was written between backticks," and section A.1 says evaluating it is
"entirely the host's own responsibility." A host calls `filter::parse` (and
`validate`, `matches`, `format`) on that text itself.

This is exactly the shape Cascade's `tasks` capability already assumes,
independently: `hollowscript-tasks-capability.ts`'s
`collectFilterLiterals` walks a program's AST once, ahead of running it, to
find every `FilterLiteral` node's source text; `resolveTasksByFilter` then
parses and evaluates each one (today, with HEL) against the owner's real
Tasks and keys the results by the literal's own source string, "since a
Filter value carries that same source at runtime" and the running program
has no other way to ask a host to evaluate one. A native implementation
that wanted a Filter value to carry anything richer than its source text
across the VM boundary would have broken this pattern for no benefit: the
host always ends up parsing the text itself, on its own copy of the filter
engine, because the language never does.

## Comparing against HEL, a second implementation

HEL (read-only, never modified here) is the pre-existing implementation
of this language, already in production behind Cascade's `tasks`
capability. Its own test
suite (`src/*.test.ts`) and its source were read as a second, independent
reading of the same grammar this implementation was built from, and its
disagreements with Appendix A are recorded here rather than resolved by
changing what the language means, per this workstream's brief: where HEL
and Appendix A disagree, the spec governs.

Every disagreement found is HEL being **more lenient or more permissive**
than Appendix A, which is exactly what its own doc comments say it is for:
HEL is explicitly "error-tolerant" ("Parsing never throws... so a
half-typed query still yields a usable tree for live results"), built for
a search box a person is still typing into. Appendix A is a strict,
single-error, batch-checked embedded language, checked once at compile
time like every other HollowScript literal. Section A.3's "There is no
recovery and therefore there are never follow-on errors" is the appendix
choosing the opposite trade-off on purpose.

| # | HEL's behaviour | Appendix A's behaviour | This implementation |
|---|---|---|---|
| 1 | An unterminated `"` is tolerated: the quote runs to the end of input and lexing continues (`lexer.ts`: "tolerate an unterminated quote"). | HS0702. | Follows the appendix: HS0702. |
| 2 | A trailing lone `-` (nothing to negate) lexes as a one-character `WORD` `"-"`, no error. | HS0701, "a negation needs a term". | Follows the appendix: HS0701 at lex time. |
| 3 | `status: open` (white space inside a predicate) parses as a predicate: `parsePrimary` checks token *types* in sequence but never checks that the field/operator/value tokens are adjacent. | HS0701: "no white space is allowed inside a predicate." | Follows the appendix: adjacency is required (`Node::Predicate`'s field and value spans touch the operator with nothing between). |
| 4 | `status:or` classifies `or` as an ordinary string value; no error. | HS0701: a keyword can never be an unquoted value. | Follows the appendix: a `WORD` value that is a keyword is rejected before classification. |
| 5 | Parsing never stops at the first error: a stray token is consumed and flagged, and parsing continues, so one query can carry several errors. | "Parsing stops at the first error and reports only that one... never follow-on errors." | Follows the appendix: `parse` returns `Result<Filter, FilterError>`, one error, full stop. |
| 6 | Nesting depth 1,000, calibrated empirically against the V8 call stack (`parser.ts`'s own measured figures: parenthesis recursion overflows around 2,246). | 100, a language-level structural limit, the same on every implementation. | 100 (`MAX_FILTER_NESTING`), checked before recursing, matching the appendix, not HEL's host-specific figure. |
| 7 | An unknown field falls back to a free-text search on the predicate's raw text (`evalPredicate`: "unknown field → free text on the raw token"). | "A predicate whose field is not in the schema is false." | Follows the appendix: `eval_node`'s `Node::Predicate` arm is `false` when the field is absent from the schema, no fallback. |
| 8 | A resolved value is coerced loosely at evaluation time: `compareNumber` calls JS `Number(resolved)` regardless of its original kind (so, for instance, a resolved `boolean` silently becomes `0`/`1`), and `compareDate`'s `toIsoDate` accepts a `Date` object or anything `Date.parse` accepts, in the local time zone. | Section A.6's mismatch rule: "a resolved value whose kind does not match the field's type is treated as absent... nothing is converted," and a `date` field's resolved value "must be a string in exactly the form `YYYY-MM-DD`," with time zone conversion entirely the adapter's job. | Follows the appendix: `resolved_is_empty` and `eval_predicate`'s type-keyed match only ever compare a `Resolved` variant against the field type it exactly names; a mismatch is absent, not coerced. `parse_date_str` accepts only the exact `YYYY-MM-DD` shape. |
| 9 | Boolean target words include `"y"` as a fourth truthy synonym (`compareBoolean`: `['true', '1', 'yes', 'y']`). | `true`, `yes`, `1` only. | Follows the appendix: `"y"` is not recognised (it classifies as a plain string and so never matches `true`/`yes`/`1`). |
| 10 | **Round-trip fidelity gap**: HEL's `ExpressionValue`/`text` nodes do not remember whether they were written quoted, only their content, so `formatValue`/`formatExpression` decide quoting purely from the *content's shape* at format time. `n:"7"` therefore reformats to `n:7` (a quoted string silently becomes an unquoted number on the very next format), and a quoted term whose text happens to be `OR` reformats to the bare keyword `OR`, changing what either text parses back to. | Section A.9 requires the exact opposite for both cases: a value "quoted in the source" is written quoted again "if writing it bare would... classify it as anything other than a string," and a term is written quoted "if it was quoted in the source" at all, unconditionally. | Follows the appendix: `Value::quoted` and `Node::Text::quoted` are part of the tree (section A.5's "remembering which of the two it was"), so `bare_value_reclassifies` and `format_term`'s `quoted` check reproduce both rules; `n:"7"` and a quoted `"OR"` both round-trip. This is the one disagreement that is not merely a stricter-vs-looser design choice: it is a real correctness gap in HEL's formatter that Appendix A's extra rule was written to close, and this implementation's own fuzz target (below) checks exactly this property on every input. |
| 11 | The empty string (and white-space-only input) parses to `null`, and `evaluateExpression(null, ...)` is unconditionally `true`: an empty query "matches everything" (`evaluate.test.ts`). | HS0710: "A filter cannot be empty." | Follows the appendix: `parse` checks `source.trim().is_empty()` before lexing and refuses with HS0710; there is no all-matching filter. |
| 12 | A relative-date offset over 5,000 (or a digit run too long to parse to a finite number) is not an error: `classifyValue` falls through to the plain-string rule, silently, so `today+5001` degrades to a free-text search on the literal text `"today+5001"` (`parser.test.ts`'s own comment: "degrades to a plain string ... rather than a crashing relDate"). | HS0706. | Follows the appendix: `relative_offset_value` raises HS0706 once the shape `today[+-]digits` has matched, rather than silently reclassifying. |

No vector of section A.11 disagrees with HEL's own test suite in the
other direction (HEL rejecting something the appendix accepts); every
difference found is HEL accepting more than the appendix allows, consistent
with it being the older, UX-oriented implementation the appendix's stricter
grammar was written to supersede. Section A.11's own vectors (the
conformance table, both parse and evaluation) are ported as
`hollowscript-core::filter`'s unit tests (`parse_vectors_of_section_a11`,
`validate_vectors_of_section_a11`, `evaluation_vectors_of_section_a11`),
run verbatim against the schema and records the appendix itself gives.

## The `filter` fuzz target

`fuzz/fuzz_targets/filter.rs` (`docs/design/fuzzing.md` has the full list)
takes arbitrary bytes as filter source and, for whatever parses, checks
that `format`'s own text parses back cleanly and that reformatting the
reparsed tree is idempotent, not that the reparsed tree is identical to
the original one node for node, which section A.9 does not actually
promise at the byte level: the canonical-text rule deliberately lower-cases
the keyword parts of a relative date's value (`due<TODAY+7` formats as
`due<today+7`), so a tree built from `TODAY+7` and the tree reparsed from
its own canonical text differ in that one value's original text even
though they mean the same thing and format the same way from there on.
Idempotence is the invariant that holds unconditionally, the same property
`format_roundtrip` checks for a whole program. `validate` and `matches` are
also run, against a small fixed schema and adapter covering every field
type, so a panic specific to schema checking or evaluation is reachable
from arbitrary text too. Seeded, by hand, from every row of section A.11's
conformance table (`fuzz/seeds/filter/`), since a filter source is not a
`.hws` program and so is not covered by the seed corpora the other
source-based targets share.

Two bugs in the fuzz target itself, not in `filter.rs`, were found and
fixed while writing it, both recorded in the target's own doc comment:
asserting reparsed-tree equality is too strong (the `TODAY+7` case above),
and treating every reparse failure as a bug is too strong (canonical text
can grow past `MAX_FILTER_BYTES` for a source that was already at the
limit, since quoting and parenthesising only ever add bytes; `HS0704` on
the reparse in that specific combination is expected, not a defect).

Run on this machine (macOS, aarch64) for 35 minutes (2,101 seconds,
comfortably over the 30 minutes asked for), from the seed corpus above:
19,847,058 runs, final coverage 1,857 edges and 10,778 features, no crash.
