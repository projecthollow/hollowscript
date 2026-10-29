# Appendix A. Filter expressions

A filter literal, written between backticks, is a value of the built-in type `Filter`. Its text is written in a small embedded language for describing which records match, in the style of a search box: `status:open priority>=high -tag:chore`. This appendix is the complete definition of that language: its text, its grammar, the values in it, how a host describes its records to it, how a filter is checked against that description, and how it is evaluated against a record.

The language is part of HollowScript and is normative for every implementation, and it is also usable on its own by a host, through the operations of [section A.1](#a1-overview).

## A.1 Overview

**In a program.** A filter literal ([section 1.6](01-lexical-structure.md#16-filter-literals)) is unescaped to its *filter source*, parsed and checked when the program is checked, and is a compile-time error if it is not valid. The value of the literal is an opaque `Filter`: it can be stored in a `constant`, passed to functions, returned, imported and exported, but it cannot be compared, indexed, taken apart, built at run time or interpolated into. A program passes a `Filter` to a host capability, which evaluates it against the host's records. The language deliberately offers no way to evaluate a filter in the standard library, because deciding whether a record matches needs the host's description of its records ([section A.6](#a6-the-host-interface)).

**Operations.** A host or tool that implements filters provides:

| Operation | Meaning |
|---|---|
| `parse(source)` | returns the filter tree or the single syntax error ([sections A.2 to A.5](#a2-lexical-structure)) |
| `validate(tree, schema)` | returns the list of problems of a tree against a host's schema ([section A.7](#a7-validation-against-a-schema)) |
| `matches(tree, record, adapter, schema)` | returns whether the record matches ([section A.8](#a8-evaluation)) |
| `format(tree)` | returns canonical text for a tree ([section A.9](#a9-canonical-text)) |

**Diagnostics.** Problems in a filter literal are reported as ordinary diagnostics of the program with the codes HS0701 to HS0711 ([Chapter 17](17-diagnostics.md)). The span of each is mapped from the filter source back to the position in the program file: the filter source is the text between the backticks with `` \` `` and `\\` unescaped, and an offset in it is turned into a file offset by adding the widths of the escape sequences before it. A diagnostic that concerns the whole literal has the span of the literal.

**Strictness.** A filter that is only partly typed, that contains a misspelled field, or that has an unterminated quote is an error. It is never silently read as something else. There is no implicit conversion of an absent value to zero or to an empty string.

## A.2 Lexical structure

The filter source is a sequence of code points. Its length must not exceed 4,096 bytes of UTF-8 (HS0704).

**White space** is any code point with the Unicode `White_Space` property (so a no-break space separates terms as a space does). It separates tokens and is otherwise insignificant.

**Tokens.**

| Token | Text |
|---|---|
| `LPAREN`, `RPAREN` | `(` and `)` |
| `QUOTED` | `"`, any number of code points other than `"`, then `"`. There is no escape sequence: a `"` always ends the quoted text, and a quoted term cannot contain a `"`. A `"` with no closing `"` before the end of the source is HS0702 |
| `OP` | one of `!=`, `<=`, `>=` (the longest match wins), or one of `:`, `=`, `<`, `>` |
| `MINUS` | a `-` that starts a token, where the previous token is not an `OP`, and that is directly followed by a code point that is not white space and not `)`. Otherwise see the next rows |
| `WORD` | a maximal run of code points that contains no white space and none of `(`, `)`, `"`, `:`, `=`, `<`, `>`, and no `!` that is directly followed by `=` |

A `-` that starts a token, is not directly after an `OP`, and is followed by white space, `)` or the end of the source is HS0701 ("a negation needs a term"). A `-` that starts a token directly after an `OP` is the first character of a `WORD`, whatever follows it (so `x:-3` has the value `-3` and `x:-` has the value `-`). A `-` that is not at the start of a token is part of the `WORD` it is in (`due:today-3`, `a-b`). A `!` that is not followed by `=` is part of a `WORD`.

Each token has a span in the filter source. Two tokens are *adjacent* if the end of the first is the start of the second, with nothing between them.

**Keywords.** A `WORD` whose whole text is `OR`, `AND` or `NOT` in any letter case is the keyword of that name, wherever a `WORD` may appear, so an unquoted value that is `or`, `and` or `not` (`status:or`) is HS0701. A term or value that should be the text `or`, `and` or `not` is quoted. A `QUOTED` token is never a keyword.

## A.3 Grammar

```
filter    = orExpr ;
orExpr    = andExpr { OR andExpr } ;
andExpr   = notExpr { [ AND ] notExpr } ;
notExpr   = ( NOT | MINUS ) notExpr | primary ;
primary   = LPAREN orExpr RPAREN | predicate | term ;
predicate = WORD OP value ;
term      = WORD | QUOTED ;
value     = WORD | QUOTED ;
```

`OR`, `AND` and `NOT` here are the keywords of [section A.2](#a2-lexical-structure). The precedence, lowest first, is `OR`, then `AND` (which may be left out: two terms next to each other are joined by `AND`), then `NOT`, which binds to the term that follows it.

**A predicate is one unit.** In `predicate`, the `WORD` and the `OP` are adjacent, and the `OP` and the `value` are adjacent: no white space is allowed inside a predicate. `status:open` is a predicate; `status: open`, `status :open` and `status : open` are HS0701. The field of a predicate is a `WORD`, never a `QUOTED`, and a keyword cannot be a field.

**Terms.** A `WORD` that is not the start of a predicate, and every `QUOTED` that is not the value of a predicate, is a *term*: text to search for ([section A.8](#a8-evaluation)). A `QUOTED` term must not be empty. The empty source, and a source with only white space, is HS0710.

**Errors.** Parsing stops at the first error and reports only that one. There is no recovery and therefore there are never follow-on errors. The errors of classifying a value ([section A.4](#a4-values): HS0705, HS0706 and HS0711) are raised as soon as the value is classified, during the left to right parse, and stop the parse like every other error, so `d:2026-13-45 )` is HS0705 and not HS0701, and two bad dates give one error, the first in source order. The error is HS0701 for every shape of mistake except an unterminated quote (HS0702), nesting that is too deep (HS0703), a source that is too long (HS0704) and an empty filter (HS0710). HS0701 is reported at the offending token, or at the end of the source when a token was expected and the source ended, and the message names what was expected. In particular these are all errors:

- a keyword where a term is expected: a leading `OR` or `AND`, `OR OR`, `a AND AND b`, `a OR AND b`;
- `OR`, `AND`, `NOT` or `-` with nothing after it: `status:open OR`, `status:open NOT`;
- an unclosed `(`, a stray `)`, and an empty group `()`;
- a predicate with no value: `status:`, or a `:` with white space after it;
- an `OP` where a term is expected: `:open`, `= x`, `a b : c` (white space before the operator);
- an empty quoted term `""`.

**Depth.** Groups (`(`) and negations (`NOT`, `-`) may nest at most 100 deep; deeper is HS0703. A parser must not recurse on its own stack more than that limit and so cannot be exhausted by a long run of `(` or `-`.

## A.4 Values

The `value` of a predicate is classified without any schema. A `QUOTED` value is always a string with the quoted text. A `WORD` value is classified by the first rule that fits (letters compared in any case where noted):

| Rule | Text | Value |
|---|---|---|
| 1 | `none` (any case) | the reserved value *none* |
| 2 | `any` (any case) | the reserved value *any* |
| 3 | `today`, `tomorrow`, `yesterday` (any case) | a relative date of offset 0, +1, -1 days |
| 4 | `today+N` or `today-N` (`today` in any case, `N` one or more ASCII digits) | a relative date of that offset in days. `N` above 5000 is HS0706 |
| 5 | `DDDD-DD-DD..DDDD-DD-DD` (`D` an ASCII digit) | a date range. Both ends must be valid calendar dates (HS0705), and the first must not be after the second (HS0711); inclusive at both ends |
| 6 | `DDDD-DD-DD` | a date. It must be a valid calendar date in the proleptic Gregorian calendar with year 0001 to 9999 (HS0705) |
| 7 | an optional `-`, one or more ASCII digits, and optionally `.` and one or more digits | a number: an `Int` if there is no `.` and it fits in an `Int`, otherwise a `Float` (the nearest binary64 value, an integer too large for `Int` included, and a negative zero is `0.0`). A number that would be infinite is classified as a string (rule 8). There is no exponent, no `+` sign, no hexadecimal, no `inf` or `nan` |
| 8 | anything else | a string with the text of the word |

Every value keeps its original text. Rules 1 and 2 (*none* and *any*) apply to every field type. For every other value, it is the original text that is used for a schema field of type `enum`, `text` or `textMulti` and for a bare term, whatever the value was classified as (so `priority:1` is the text `1` for an enum field).

The tokens `none` and `any` are reserved: a program cannot match a field literally named `none` or valued `none` unquoted. A quoted `"none"` is the string.

## A.5 The tree

The parser produces a tree of these nodes:

- `and(children)`, `or(children)` with two or more children, one `and` node for each `andExpr` that has two or more terms and one `or` node for each `orExpr` that has two or more branches; `not(child)`. A parenthesised group that is itself an `and` inside an `and`, or an `or` inside an `or`, stays a separate nested node;
- `predicate(field, op, value)` where `field` is the field word in lower case, `op` is one of `:`, `=`, `!=`, `<`, `>`, `<=`, `>=`, and `value` is a classified value with its original text;
- `text(string)` for a term, holding the text of the `WORD` or the content of the `QUOTED`, and remembering which of the two it was (a quoted term is never a boolean field name, [section A.8](#a8-evaluation)).

A group `( ... )` contributes its inner tree and no node of its own.

## A.6 The host interface

A host describes its records to filters with a *schema* and an *adapter*. Both are supplied by the host and are outside the filter text.

**Schema.** A schema maps *field names* to *field descriptions*. Field names are lower-case. Two names may map to the same description (`label` and `tag`). A description has:

- `type`, one of `enum`, `text`, `textMulti`, `date`, `number`, `boolean`;
- `aliases`, an optional map from lower-case input text to a canonical text (`done` to `completed`);
- `values`, for `enum` only, an optional list of the canonical texts in rank order, lowest first.

| Field type | Meaning |
|---|---|
| `enum` | one of a closed set of strings, ordered by rank if `values` is given |
| `text` | a free string |
| `textMulti` | a list of strings |
| `date` | a calendar date |
| `number` | an `Int` or a `Float` |
| `boolean` | `true` or `false` |

**Adapter.** For evaluation the host provides three operations:

- `resolve(record, field)`: the value of the named field of the record, as one of: absent, a string, a number (`Int` or `Float`), a boolean, or a list of strings.
- `freeText(record)`: a list of strings that a bare term is searched in.
- `today()`: today's date as `YYYY-MM-DD`, in the time zone the host wants relative dates to use. This is what `clock.today()` returns in UTC ([section 16.3](16-standard-capabilities.md#163-clock)).

**Resolved values are used as they are.** A resolved value whose kind does not match the field's type is treated as absent: a number is not converted from a string, an absent value is not converted to zero, a string is not converted to a date. For a `date` field, the resolved value must be a string in exactly the form `YYYY-MM-DD` and a valid date, and a string that is not is treated as absent; the adapter does any conversion from instants and time zones, so no time-zone arithmetic happens inside a filter.

## A.7 Validation against a schema

`validate(tree, schema)` checks each predicate of the tree in source order and returns every problem found, each with the span of the field or value concerned. It is called when the host has given the checker a filter schema ([section 9.7](09-effects-and-capabilities.md#97-the-capability-interface-of-a-run)); without a schema only the checks that need none (sections A.2 to A.4) are made.

For each predicate `field op value`:

1. If `field` is not in the schema: HS0708.
2. If `op` is not allowed for the field's type: HS0707. Allowed operators: `:`, `=`, `!=` for `text`, `textMulti` and `boolean`; all seven for `enum`, `date` and `number`; but `<`, `>`, `<=`, `>=` on an `enum` require the description to have `values` (otherwise HS0707). A date range as the value allows only `:`, `=` and `!=`. The values *none* and *any* allow only `:`, `=` and `!=`.
3. If the value is *none* or *any*: valid.
4. Otherwise the value must suit the type, or it is HS0709:
   - `enum`: the original text, lower-cased and mapped through `aliases`, must be one of `values` (compared in lower case) if `values` is given; any text is valid if it is not.
   - `text` and `textMulti`: any original text.
   - `number`: the value must be a number.
   - `boolean`: the original text, in lower case, must be one of `true`, `false`, `yes`, `no`, `1`, `0`.
   - `date`: the value must be a date, a date range or a relative date.

Terms are always valid. A tree with no problems is *valid for the schema*, and evaluating a valid tree never meets an unknown field.

## A.8 Evaluation

`matches(tree, record, adapter, schema)` is a Boolean, defined for any tree and record, and never fails. The schema tells which names are fields, which are boolean fields and what each field's type, `values` and `aliases` are; a host that has no schema passes an empty one, in which case every predicate is false and no term is a boolean field. It reads `today()` at most once per call.

- `and` is true if every child is true, `or` if some child is true, `not` if its child is false.
- **A term** is true if some string returned by `freeText(record)` contains the term as a substring, after both are converted to lower case with the full Unicode lower-case mapping of `toLower` ([section 13.3](13-stdlib-core.md#133-strings)). A `WORD` term (not a `QUOTED` one) whose lower-case text is the name of a `boolean` field in the schema is instead true if that field resolves to `true`: `starred` alone means `starred:true`.
- **A predicate** whose field is not in the schema is false (a validated tree has none).

**Step 0.** If the operator is not allowed for the field's type or value by rule 2 of [section A.7](#a7-validation-against-a-schema) (including `<`, `>`, `<=` and `>=` with *none* or *any*, or with a date range, and any ordering operator on a `boolean`, `text` or `textMulti` field), or if the value does not suit the field's type by rule 4 of that section (a value that is neither *none* nor *any*), the predicate is false, for every operator including `!=`, and nothing is converted. So `starred:ture`, `points>=abc`, `title>x` and `due<2026-07-01..2026-07-31` are false for every record even when the tree was not validated.

Otherwise the predicate is evaluated with `resolved`, the value of `resolve(record, field)` after the mismatch rule of [section A.6](#a6-the-host-interface). Call `resolved` *empty* if it is absent, an empty list, or a string that is empty after trimming Unicode white space.

1. If the value is *none*: true if `resolved` is empty, for `:` and `=`; true if it is not empty, for `!=`.
2. If the value is *any*: true if `resolved` is not empty, for `:` and `=`; true if it is empty, for `!=`.
3. If `resolved` is empty: true if the operator is `!=`, false for every other operator. An absent value is different from every value and is equal to none, and it is neither smaller nor greater than any.
4. Otherwise, by the field's type:
   - **`boolean`**: the target is true if the value's original text in lower case is `true`, `yes` or `1`, and false if it is `false`, `no` or `0`. `:` and `=` are true if `resolved` equals the target and `!=` if it does not.
   - **`number`**: `resolved` and the value are compared as numbers by exact mathematical value (as `==` and `<` do in the language, [section 3.5](03-expressions.md#35-comparison-and-equality)), with the operator; `:` means `=`.
   - **`text`**: both texts in lower case; `:` is true if `resolved` contains the value, `=` if they are equal, `!=` if they are not equal.
   - **`textMulti`**: `resolved` is a list of strings, compared in lower case; the target is the value's original text in lower case, mapped through `aliases`. `:` and `=` are true if the target is an element, `!=` if it is not.
   - **`enum`**: both in lower case, the value's text mapped through `aliases`. `:` and `=` are true if equal, `!=` if not equal. `<`, `>`, `<=`, `>=` compare the positions of the two in `values` (lower-cased), and are false if either is not in `values`.
   - **`date`**: the target is the date itself; a relative date is `today()` plus the offset in days by the proleptic Gregorian calendar (if `today()` is not a valid date, false). `:` and `=` are true if `resolved` equals the target, `!=` if not; `<`, `>`, `<=`, `>=` compare the two dates chronologically. A date range as the value makes `:` and `=` true if `resolved` is from the first date to the second inclusive, and `!=` if it is not.

Dates compare chronologically. Since a valid `YYYY-MM-DD` string sorts the same as the date, implementations may compare the strings.

## A.9 Canonical text

`format(tree)` returns text that parses back to an identical tree, including nodes of the same operator that were written nested. Canonical text is:

- a predicate as `field` `op` `value`, with no spaces, the field in lower case and the value as written in the source except that the keywords `none`, `any`, `today`, `tomorrow` and `yesterday`, including the `today` of a `today+N` or `today-N` relative date, are written in lower case;
- `and` children joined by one space; `or` children joined by ` OR `; `not` as `-` followed by its child;
- a child that is an `and` or an `or` node in parentheses if its operator is the same as its parent's or binds looser: an `and` or an `or` inside an `and`, an `or` inside an `or`, and an `and` or an `or` inside a `not`. An `and` inside an `or` needs none;
- a term as its text, in quotes if it was quoted in the source or if writing it bare would change its meaning: it is empty, contains white space or any of `( ) " : = < >`, contains `!=`, starts with `-`, or is `OR`, `AND` or `NOT` in any letter case;
- a value that was quoted in the source in quotes again if writing it bare would classify it as anything other than a string (a date, a number, `none`, `any`, a relative date), or would need quotes as a term.

Because there is no escape inside quotes, a string in a tree never contains `"`, and every tree can be written.

## A.10 Limits

| Limit | Value | Diagnostic |
|---|---|---|
| Length of filter source | 4,096 bytes | HS0704 |
| Nesting of groups and negations | 100 | HS0703 |
| Relative date offset | 5000 days either way | HS0706 |
| Diagnostics per literal | 1 for syntax (including HS0705, HS0706 and HS0711); one per invalid predicate for validation | |

## A.11 Conformance vectors

Each row of the first table gives a filter source and its parse outcome: the canonical text, or the diagnostic.

| Filter source | Outcome |
|---|---|
| `status:waiting` | `status:waiting` |
| `Status:Waiting priority>=high -tag:chore` | `status:Waiting priority>=high -tag:chore` |
| `due<today` | `due<today` |
| `due<TODAY+7` | `due<today+7` |
| `a OR b c` | `a OR b c` (which is `a OR (b AND c)`) |
| `(a OR b) c` | `(a OR b) c` |
| `NOT a` | `-a` |
| `-(a OR b)` | `-(a OR b)` |
| `a (b c)` | `a (b c)` |
| `a OR (b OR c)` | `a OR (b OR c)` |
| `"hello world"` | `"hello world"` |
| `-"a b"` | `-"a b"` |
| `a"b"` | `a "b"` |
| `x:-3` | `x:-3` |
| `a!=b` | `a!=b` |
| `a!b` | `a!b` |
| `a-b` | `a-b` |
| `d:2026-07-01..2026-07-31` | `d:2026-07-01..2026-07-31` |
| `n:"7"` | `n:"7"` |
| `"OR"` | `"OR"` |
| `status:waiting OR` | HS0701 |
| `OR a` | HS0701 |
| `status:waiting NOT` | HS0701 |
| `a AND AND b` | HS0701 |
| `a )` | HS0701 |
| `( a` | HS0701 |
| `()` | HS0701 |
| `status: priority:high` | HS0701 |
| `status :open` | HS0701 |
| `x: -3` | HS0701 |
| `status:or` | HS0701 |
| `-` | HS0701 |
| `""` | HS0701 |
| `"unterminated` | HS0702 |
| (empty, or spaces only) | HS0710 |
| `d:2026-13-45` | HS0705 |
| `d:2026-02-30` | HS0705 |
| `d:today+5001` | HS0706 |
| `d:2026-07-31..2026-07-01` | HS0711 |

The second table gives validation outcomes against the schema of the third table.

| Filter source | Outcome |
|---|---|
| `staus:doing` | HS0708 |
| `status:dwon` | HS0709 |
| `status:open` | valid (alias of `waiting`) |
| `status<any` | HS0707 |
| `points>=abc` | HS0709 |
| `starred:ture` | HS0709 |
| `starred>true` | HS0707 |
| `due<2026-07-01..2026-07-31` | HS0707 |
| `tag>work` | HS0707 |
| `status:doing priority:high` | valid |

**Schema used by the vectors.** `status`: enum, values `waiting`, `doing`, `done`, aliases `open` to `waiting` and `completed` to `done`. `priority`: enum, values `low`, `medium`, `high`. `tag`: textMulti. `title`: text. `due`: date. `points`: number. `starred`: boolean. Free text is the title. `today()` is `2026-07-26`.

**Records used by the vectors.**

- R1: status `doing`, priority `medium`, tag `["work", "q3"]`, title `Quarterly report`, due `2026-07-30`, points `5` (an `Int`), starred `true`.
- R2: status `waiting`, priority `high`, tag `[]`, title `Call back`, due absent, points absent, starred `false`.

Evaluation outcomes (R1, R2):

| Filter | R1 | R2 |
|---|---|---|
| `status:doing` | true | false |
| `status:open` | false | true |
| `status>=doing` | true | false |
| `priority>medium` | false | true |
| `-tag:work` | false | true |
| `tag:none` | false | true |
| `tag:any` | true | false |
| `due<today+5` | true | false |
| `due:none` | false | true |
| `due!=2026-07-30` | false | true |
| `due:2026-07-01..2026-07-31` | true | false |
| `points>=5` | true | false |
| `points!=5` | false | true |
| `points:0` | false | false |
| `starred` | true | false |
| `-starred` | false | true |
| `starred:no` | false | true |
| `report` | true | false |
| `"call back"` | false | true |
| `title:call` | false | true |
| `title=quarterly` | false | false |
| `title="quarterly report"` | true | false |
| `status:doing OR priority:high` | true | true |
| `status:doing priority:high` | false | false |
| `starred:ture` (not validated) | false | false |
| `points>=abc` (not validated) | false | false |
| `title>x` (not validated) | false | false |
| `due<2026-07-01..2026-07-31` (not validated) | false | false |
