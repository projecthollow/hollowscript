# 17. Diagnostics

Every problem that the lexer, parser, checker or runtime can report has a stable **code** of the form `HS` and four digits. The code is part of this specification. Codes are never reused: a code that is retired stays reserved, and a new problem gets a new number. The registry in this chapter lists every diagnostic, its severity and its message template.

## 17.1 The diagnostic record

A diagnostic has these fields:

| Field | Meaning |
|---|---|
| `code` | the code, for example `HS0301` |
| `severity` | `error` or `warning` |
| `message` | text for a person, from the template with the placeholders filled in |
| `file` | the canonical path of the module the diagnostic is about ([section 10.3](10-modules.md#103-import-paths-and-canonical-paths)), and for a program that is not run from files, the name the host gave the source |
| `span` | the half-open byte range `[start, end)` in that file ([section 1.1](01-lexical-structure.md#11-source-text)) |
| `line`, `column` | the 1-based line and column of `start` |
| `related` | zero or more further locations, each a `file`, a `span` and a short message (for example the first declaration for HS0201) |
| `frames` | for faults only: the innermost call frames, at most 32, each with a function name, file and span |
| `data` | for the limit faults HS1001 to HS1005: an object with `option` (the option name, for example `maxSteps`), `limit` (the configured value) and `used` (the counter value at the fault); absent otherwise |

Every source diagnostic has a file and a real span. A diagnostic is identified by its code, never by its span, and the only source diagnostics with an empty span are HS0002 (the file as a whole, at offset 0), HS0004 (at the start of the first suppressed diagnostic), HS1003 when the run executed no statement (at the start of the entry module) and the diagnostics at the end of a file, which follow the empty span of `EOF` ([section 1.8](01-lexical-structure.md#18-statement-terminators)). HS1201 to HS1205 are not source diagnostics: they answer a host operation and have no file, span, line or column.

**Severity.** An **error** prevents the program from running. A **warning** does not: it is reported and the program runs. Every fault is an error. Exactly the warnings listed in [section 17.3](#173-warnings) exist.

**Templates.** A template names its placeholders in braces, such as `{name}`, `{found}` and `{expected}`. A type in a message is written as in source (`Int?`, `[String]`, `fn(Int): Int`, `effect fn(String)`). The wording of a message is guidance: an implementation MAY word it differently, but MUST report the same code, severity and file, and the span and line specified by the *span* column of the tables, which name what the span covers. Conformance ([Chapter 19](19-conformance.md)) compares code, severity, file and line.

**Order.** Diagnostics of one run are reported in this order: by file, in the order in which the modules were first reached when loading (the entry module first, then imports depth first in the order of the `import` lines); within a file by `start` ascending, then by `end` ascending, then errors before warnings, then by code ascending. Implementations MUST produce this order.

**Limit.** At most 100 diagnostics of any severity, warnings included, are reported for one file. When more were found, the first 100 in the order above are reported, followed by exactly one HS0004, so at most 101 are reported. A diagnostic reported at a `TERM` or at `EOF` has the line and column of that token's span ([section 1.8](01-lexical-structure.md#18-statement-terminators)).

**No cascades.** A diagnostic is not reported for an expression whose problem has already been reported ([section 2.11](02-types.md#211-when-types-are-checked)). One mistake gives one diagnostic wherever this is possible.

## 17.2 Errors

Every diagnostic in this section has severity `error`. The *stage* is `lex`, `parse`, `check` (all of the semantic analysis, which runs before anything executes) or `load` (module loading).

### Lexical

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0001 | lex | The source is not valid UTF-8 (invalid byte at offset {offset}). | the byte |
| HS0002 | lex | The source is {size} bytes; the limit is {limit}. | empty, offset 0 |
| HS0004 | check | Too many diagnostics; only the first {limit} are shown. | empty, at the start of the first suppressed diagnostic |
| HS0005 | lex | '{char}' is not a character HollowScript recognises here. | the code point |
| HS0006 | lex | This string is missing its closing quote before the end of the line or of the file. | the string from its opening quote |
| HS0007 | lex | This interpolation is missing its closing '}'. | the `${` |
| HS0008 | lex | This is not a valid escape sequence in a string; use \\" \\\\ \\n \\t \\r \\$ or \\u{...}. | the escape |
| HS0009 | lex | This unicode escape is not valid; \\u{...} needs 1 to 6 hexadecimal digits that name a Unicode scalar value. | the escape |
| HS0010 | lex | This filter is missing its closing backtick before the end of the line. | the filter from its opening backtick |
| HS0011 | lex | A number needs a digit after its decimal point, for example 2.0 instead of 2. | the number and the point |
| HS0012 | lex | A number needs a digit before its decimal point, for example 0.5 instead of .5. | the point and digits |
| HS0013 | lex | This number is followed by characters that cannot be part of it. | the number and the characters |
| HS0014 | lex, check | This integer literal is too large; an Int is at most 9223372036854775807. | the literal |
| HS0015 | lex | This number is too large to be a Float. | the literal |
| HS0016 | lex | An exponent needs at least one digit. | the number and its `e` |

### Syntax

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0101 | parse | Expected {expected}, found {found}. | the token found |
| HS0102 | parse | Expected an expression, found {found}. | the token found |
| HS0103 | parse | Expected a statement, found {found}. | the token found |
| HS0104 | parse | '{word}' is reserved for a future version of HollowScript. | the word |
| HS0106 | parse | 'uses' lines must come before every import and statement. | the `uses` line |
| HS0107 | parse | 'import' lines must come before every statement. | the `import` line |
| HS0108 | parse | 'export' cannot be used on '{kind}'; only fn, record, enum and constant can be exported. | the declaration |
| HS0109 | parse | Only a call may stand alone as a statement; an expression on its own computes a value and discards it. | the expression |
| HS0110 | parse | A record literal cannot appear directly here because it is ambiguous with the block that follows; wrap it in parentheses. | the literal |
| HS0111 | parse | A type may have only one trailing '?'; write Int?, not Int??. | the second `?` |
| HS0112 | parse | A {kind} needs at least one {member}. | the braces |
| HS0113 | parse | Nesting is too deep; the limit is {limit} levels. | the construct that exceeded it |
| HS0114 | parse | This chain has too many links; the limit is {limit}. | the chain |
| HS0115 | lex | String interpolations are nested too deeply; the limit is {limit}. | the innermost `${` |
| HS0116 | parse | A variant name must start with an upper-case letter. | the name |
| HS0117 | parse | {kind} is only allowed at the top level of a file. | the declaration |
| HS0120 | parse | A name bound by a pattern must start with a lower-case letter or '_'. | the name |
| HS0121 | parse | '..' can only be the last element of a list pattern. | the `..` |
| HS0122 | parse | 'else' must be on the same line as the closing '}' before it. | the `else` |
| HS0123 | parse | A field cannot be named '_'. | the name |

### Names and scope

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0201 | check | '{name}' is already declared. | the second declaration; related: the first |
| HS0202 | check | The {kind} '{name}' is already declared in this {container}. | the second; related: the first |
| HS0203 | check | The {kind} '{name}' is already set in this {container}. | the second; related: the first |
| HS0204 | check | The name '{name}' is already bound by this pattern. | the second binding |
| HS0205 | check | '{name}' is not defined here. | the name |
| HS0206 | check | '{name}' cannot be assigned; only a name declared with 'variable' can be. | the assignment target |
| HS0208 | check | Unknown type '{name}'. | the type name |
| HS0209 | check | '{name}' is a {kind} and cannot be used as a value. | the name |
| HS0210 | check | The function '{function}' uses '{name}', which is not initialised yet at the place the function is used (line {line}). | the use of `{name}` in the function; related: the statement |
| HS0212 | check | The type parameter '{name}' is declared more than once. | the second declaration |
| HS0213 | check | '_' cannot be used as a value. | the `_` |

### Types and expressions

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0301 | check | Expected {expected}, found {found}. | the expression |
| HS0302 | check | '{name}' does not take type arguments. | the type |
| HS0303 | check | '{name}' takes {expected} type argument(s), found {found}. | the type |
| HS0304 | check | The type of {what} cannot be inferred; add a type annotation. | the declaration or literal |
| HS0305 | check | The record '{name}' contains itself, directly or through other types; use an enum for recursive data. | the declaration |
| HS0306 | check | A map key must be Int, String or Bool, found {found}. | the type |
| HS0307 | check | A set element must be Int, String or Bool, found {found}. | the type or argument |
| HS0308 | check | The type parameter '{name}' of '{function}' cannot be inferred; add a type annotation. | the call or name |
| HS0309 | check | The type of the parameter '{name}' cannot be inferred from where this closure is used; add an annotation. | the parameter |
| HS0310 | check | The unary '-' requires an Int or a Float, found {found}. | the operand |
| HS0311 | check | '{op}' requires two numbers, found {left} and {right}. | the operator expression |
| HS0312 | check | '{op}' requires two numbers or two Strings, found {left} and {right}. | the operator expression |
| HS0313 | check | Cannot compare {left} with {right} using '{op}'. | the operator expression |
| HS0314 | check | Values of type {found} cannot be compared with '==' or '!='. | the operator expression |
| HS0315 | check | '{op}' requires Bool, found {found}. | the operand |
| HS0316 | check | A condition must be a Bool, found {found}. | the condition |
| HS0317 | check | Interpolation accepts Int, Float, Bool and String, found {found}. | the interpolated expression |
| HS0318 | check | This is not a function and cannot be called (its type is {found}). | the callee |
| HS0319 | check | Expected {expected} argument(s), found {found}. | the argument list |
| HS0320 | check | {type} has no method '{name}'. | the method name |
| HS0321 | check | This record has no field '{name}'. | the field name |
| HS0322 | check | Only a record has fields; '.{name}' is not valid on {found}. | the field name |
| HS0323 | check | Only lists and maps can be indexed, found {found}. | the indexed expression |
| HS0325 | check | 'for' can only iterate a list, a set or a map, found {found}. | the collection |
| HS0326 | check | The record is missing the field '{name}'. | the literal |
| HS0327 | check | The record has a field '{name}' that {type} does not have. | the field |
| HS0328 | check | 'none' can only be compared with an optional value, found {found}. | the operator expression |
| HS0330 | check | {operation} requires {requirement}, found {found}. | the call |
| HS0331 | check | The '?' operator needs a Result, found {found}. | the operand |
| HS0332 | check | '?' can only be used where the enclosing function returns a Result with the same error type ({expected}). | the `?` |
| HS0333 | check | '{enum}' has no variant '{name}'. | the variant name |
| HS0334 | check | '{name}' must be called; a method or a function that takes any record is not a value. | the name |
| HS0340 | check | This function returns nothing, so 'return' cannot carry a value. | the `return` |
| HS0341 | check | This function declares a return type, so 'return' must carry a value. | the `return` |
| HS0342 | check | {what} declares a return type, so every path must return a value. | the function name or `fn` |
| HS0343 | check | Unreachable statement: nothing after return, break or continue in the same block ever runs. | the first unreachable statement |
| HS0335 | check | This type is too large; the limit is {limit} nodes. | the type |
| HS0344 | check | '{word}' can only be used inside a loop in the same function. | the `break` or `continue` |

### Effects and capabilities

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0401 | check | This call goes through a value of type {found}, which is effectful, so it must be marked 'effect'. | the call |
| HS0402 | check | 'effect' is only for a call through an effectful function; this callee has type {found}. | the `effect` |
| HS0403 | check | 'effect' must be followed directly by a call. | the `effect` |
| HS0404 | check | The capability '{name}' is not available in this run. Ask the host to grant it, or remove it from 'uses'. | the name in `uses` |

### Patterns

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0501 | check | This match does not handle every value{example}. | the `match` keyword |
| HS0502 | check | This value is already handled by an earlier match arm. | the repeated pattern |
| HS0503 | check | This match arm can never run, because earlier arms already match every value it could match. | the pattern |
| HS0504 | check | This pattern cannot match a value of type {found}. | the pattern |
| HS0505 | check | '{enum}' has no variant '{name}'. | the variant name |
| HS0506 | check | The variant '{name}' has {expected} value(s), found {found} pattern(s). | the pattern |
| HS0507 | check | The record type has no field '{name}'. | the field name |
| HS0508 | check | The alternatives of a match arm cannot bind names. | the binding |
| HS0509 | check | This pattern is not allowed in a declaration or a 'for'; only names, '_', and record and list patterns made of them are. Use 'match'. | the pattern |
| HS0510 | check | A value of type {found} cannot be matched. | the scrutinee |
| HS0511 | check | This match is too complex to check; the limits are {limit}. | the `match` keyword |

### Modules

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0601 | load | Cannot find the module '{path}'. | the path string |
| HS0602 | check | An import path must be a plain string beginning './' or '../' with valid segments ({reason}). | the path string |
| HS0603 | check | Import cycle: {chain}. | the path string that closes the cycle |
| HS0604 | check | The module '{path}' does not export '{name}'. | the name in the import |
| HS0605 | check | The import path climbs above the root of the program. | the path string |
| HS0606 | check | The exported {kind} '{name}' cannot be read or called by a host because {reason}. | the name |
| HS0607 | load | The module '{path}' could not be read: {reason}. | the path string |
| HS0609 | check | Leave out the '.hws' extension in an import path. | the path string |
| HS0611 | check | A top-level 'return' is only allowed in the entry module. | the `return` |
| HS0612 | load | The program has too many modules or too much source; the limit is {limit}. | the path string |
| HS0613 | check | The exported {kind} '{name}' exposes a capability: its type contains an effectful function type. | the name |

### Filter literals

These are reported with a span inside the filter literal, mapped from the filter source as described in [Appendix A](appendix-a-filters.md#a1-overview).

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0701 | check | Invalid filter: {detail}. | the offending token, or the end of the filter |
| HS0702 | check | A quoted term in this filter is missing its closing quote. | from the opening quote |
| HS0703 | check | This filter is nested too deeply; the limit is {limit}. | the group or negation that exceeded it |
| HS0704 | check | This filter is too long ({size} bytes); the limit is {limit}. | the literal |
| HS0705 | check | '{text}' is not a valid calendar date. | the value |
| HS0706 | check | The relative date offset is too large; the limit is {limit} days. | the value |
| HS0707 | check | The operator '{op}' cannot be used with {what}. | the operator |
| HS0708 | check | The filter has no field named '{field}'. | the field |
| HS0709 | check | '{value}' is not a valid value for the {type} field '{field}'. | the value |
| HS0710 | check | A filter cannot be empty. | the literal |
| HS0711 | check | The date range {from}..{to} ends before it starts. | the value |

### Implementation limits

A limit of the implementation rather than of the language ([section 1.10](01-lexical-structure.md#110-structural-limits)). An implementation that reports this code documents the limit, and `{limit}` names it.

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0901 | check | This exceeds a limit of this implementation: {limit}. | the construct that exceeds the limit |

## 17.3 Warnings

Warnings are reported at severity `warning` and do not stop a program from running. These three are the only warnings, and this section is the one place that defines when they are reported.

| Code | Stage | Message | Span |
|---|---|---|---|
| HS0801 | check | Unused constant or variable '{name}'. | the name in its declaration |
| HS0802 | check | Unused import '{name}'. | the name in the import |
| HS0803 | check | The Result of this call is ignored; handle it, or use '?'. | the call |

A name is *used* if a reference that resolves to it is read anywhere, including inside a closure and including inside an expression that has another error; a reference that does not resolve is not a use. Warnings are reported whether or not the program has errors. Assigning to a `variable` is not a use. HS0801 is reported for every name that a `constant` or `variable` declaration binds, local or top-level, including the names of a destructuring pattern, when it is never read. It is not reported for a parameter of a `fn` or a closure, for a `for` loop variable or a name bound by a `for` pattern, for a name bound in the pattern of a `match` arm, for a name that begins with `_`, or for an exported declaration. A parameter never warns because it is part of a signature and a callback may ignore what it is given, and a loop variable or an arm binding names part of a value that the construct must bind.

## 17.4 Faults and host errors

These are reported at run time, with severity `error` and stage `run`. A fault carries `frames`. [Section 8.1](08-errors-and-results.md#81-faults-and-recoverable-errors) explains what a fault is; [Chapter 11](11-resource-limits.md) defines the limits; [Chapter 12](12-instances.md) defines the host errors.

| Code | Message | Span |
|---|---|---|
| HS1001 | The step limit of {limit} was reached. | the statement, iteration or call being charged |
| HS1002 | The memory limit of {limit} bytes was reached by an allocation of {size} bytes. | the allocating operation |
| HS1003 | The instance retains {size} bytes, above the limit of {limit}. | the last statement executed |
| HS1004 | The output limit of {limit} bytes was reached. | the `print` call |
| HS1005 | The call depth limit of {limit} was exceeded. Check for a recursive call with no base case. | the call |
| HS1006 | The run was cancelled. | the operation at the safepoint |
| HS1007 | This calculation overflows the range of an Int. | the operation |
| HS1008 | Division by zero. | the operation |
| HS1009 | This calculation is not a number. | the operation |
| HS1010 | This calculation is infinite. | the operation |
| HS1011 | Invalid argument to '{function}': {reason}. | the call |
| HS1012 | The capability '{name}' failed: {message}. | the call |
| HS1013 | The capability '{name}' returned a value of the wrong type; expected {expected}. | the call |
| HS1014 | The list has {found} element(s) but this pattern needs {expected}. | the pattern |
| HS1015 | A limit of this implementation was reached: {limit}. | the operation |
| HS1099 | Internal error: {message}. | the operation, if known |
| HS1201 | The program exports no function or constant named '{name}'. | none: reported to the host |
| HS1202 | Cannot call '{name}': {reason}. | none: reported to the host |
| HS1203 | The instance has faulted and cannot run again. | none: reported to the host |
| HS1204 | The instance is already running; a call cannot start inside it. | none: reported to the host |
| HS1205 | The instance cannot be called because it {reason}. | none: reported to the host |

HS1201 to HS1205 are errors of the host's request ([section 12.6](12-instances.md#126-faults-in-instances)) and are not source diagnostics: they have no file, span, line or column.

**Frames.** A frame is named after the function it runs. A closure frame is named `<closure>` and has the span of the closure literal; a library function adds no frame, so a fault inside a callback that `map` calls shows the closure's frame under the frame that called `map`. The span of HS1001 for a loop iteration is the `for` or `while` keyword. The span of HS1003 is the last statement executed; if the run executed none, it is the empty span at the start of the entry module.

## 17.5 Summary by range

| Range | What |
|---|---|
| HS0001 to HS0099 | source text and lexical errors |
| HS0101 to HS0199 | syntax |
| HS0201 to HS0299 | names and scope |
| HS0301 to HS0399 | types, expressions and statements |
| HS0401 to HS0499 | effects and capabilities |
| HS0501 to HS0599 | patterns |
| HS0601 to HS0699 | modules |
| HS0701 to HS0799 | filter literals |
| HS0801 to HS0899 | warnings |
| HS0901 to HS0999 | implementation limits |
| HS1001 to HS1099 | runtime faults |
| HS1201 to HS1299 | host errors for instances |

Numbers inside a range that this chapter does not list are reserved.
