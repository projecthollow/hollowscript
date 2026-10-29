# Decisions

This document records the language decisions that the specification had to make where the design left a point open or ambiguous, with the alternatives that were considered and the reason for the choice. It is a companion to the specification: the rules themselves are stated once, in the chapters, and this document explains why they are as they are. Each decision points to the section that states the rule.

The guiding aim is a language that stays small, is safe to run when the program is not trusted, and behaves identically on every implementation. When two options were otherwise equal, the one with fewer rules to learn, fewer edge cases and fewer places where implementations could differ was chosen.

## Map and set key types, and their order

**Decision.** Map keys and set elements are `Int`, `String` or `Bool`. Maps and sets are ordered by key: numerically, by code point sequence, and `false` before `true`. Iteration, `keys`, `values`, `entries` and equality all follow that order. ([section 6.2](06-data-types.md#62-lists-maps-and-sets))

**Alternatives.** Allowing `Float` keys; allowing records and lists as keys; insertion-ordered maps.

**Reason.** `Float` keys raise questions about rounding and zero that the language avoids everywhere else. Composite keys need a total order and hashing rules for every type. Insertion order would make the iteration order, and so the output of a program, depend on how a map was built; a key-ordered map has one canonical form, makes equality a comparison of two sequences, and gives every implementation the same answer.

## Integer division, floor modulo and remainder

**Decision.** `//` starts a comment, so there is no operator for integer division. The library provides `div` (floor division), `mod` (floor modulo, sign of the divisor) and `remainder` (truncated remainder, sign of the dividend). `/` always produces a `Float`. ([section 13.2](13-stdlib-core.md#132-numbers))

**Alternatives.** A `%` operator; truncating division; making `/` on two `Int` values produce an `Int`.

**Reason.** A new operator symbol needs a precedence level and reopens the argument about negative operands. Named functions have neither cost. Floor semantics are the ones that keep `mod(-1, 3)` equal to 2, which is what indexing and cyclic arithmetic need, while `remainder` remains for callers who want the truncated form. A `/` that returns `Int` for two `Int` operands is a silent trap.

## Integer overflow is a resource-class fault

**Decision.** `Int` is a 64-bit signed integer. Any operation whose exact result does not fit faults with HS1007. Nothing wraps or saturates. The fault is classed with the resource limits and cannot be caught. ([section 3.4](03-expressions.md#34-arithmetic))

**Alternatives.** Silent wrapping; saturation; arbitrary precision integers; a recoverable error.

**Reason.** Wrapping hides bugs, arbitrary precision makes the cost of an operation unbounded, and a recoverable error would put a `Result` on every addition. A checked fault is cheap and predictable, and treating it as a resource fault keeps a script from using overflow as a way to alter its own limits.

## Float text

**Decision.** A `Float` is written with the shortest digits that read back as the same value, positionally when the decimal exponent is from -5 to 15 and in scientific form (`1e16`, `1.5e-7`) otherwise. A whole number always has a decimal point or an exponent, so `1.0` is `1.0`. The text of any `Float` reads back as a `Float` literal. ([section 3.9](03-expressions.md#39-string-interpolation))

**Alternatives.** Printing whole numbers without a point; a fixed number of digits; always positional.

**Reason.** Dropping the point loses the difference between `1` and `1.0`, which the language treats as different types. A round-trip text is what makes `parseFloat` and the JSON encoder agree with the printer. Positional notation for very large or very small values would produce hundreds of digits.

## No negative zero, no NaN, no infinity

**Decision.** Every `Float` operation whose result is infinite or not a number faults (HS1010, HS1009). A result of negative zero becomes zero. ([section 3.4](03-expressions.md#34-arithmetic))

**Alternatives.** Full IEEE behaviour with `NaN` and `Infinity` values.

**Reason.** With no `NaN`, `==` is an equivalence relation and sorting is total. With one zero, formatting and equality have no exception to state. The rules for how a `NaN` compares and propagates are a large surface that a small language does not need.

## Rounding

**Decision.** `round` takes halves away from zero: `round(2.5)` is 3 and `round(-2.5)` is -3. `fixed` rounds the same way on the exact decimal expansion. ([section 13.2](13-stdlib-core.md#132-numbers))

**Alternatives.** Halves to even; halves up.

**Reason.** Half away from zero is symmetric about zero and is what people expect when they check a result by hand.

## Int to Float conversion sites

**Decision.** An `Int` is converted to a `Float` at *coercion sites*: annotated initialisers, assignments to `Float` variables, arguments, return values, elements of literals with an expected type, and operands of mixed arithmetic. It is never converted through a type constructor (a `[Int]` is not a `[Float]`), and a type parameter inferred as `Int` does not accept a `Float`. ([section 2.7](02-types.md#27-assignability-and-numeric-coercion))

**Alternatives.** Full implicit conversion everywhere; no implicit conversion at all; covariant lists.

**Reason.** The site-based rule gives the intuitive results (`constant f: Float = 1`, `sqrt(4)`, a host function declaring a `Float` parameter receiving `3`) without a subtyping relation. Covariant lists would need a conversion of every element, which is a hidden linear cost.

## Exact comparison of mixed Int and Float

**Decision.** Comparing an `Int` with a `Float` compares the exact mathematical values, without converting the `Int`. ([section 3.5](03-expressions.md#35-comparison-and-equality))

**Alternatives.** Converting the `Int` to `Float` first.

**Reason.** Converting first makes `9007199254740993 == 9007199254740992.0` true and makes ordering intransitive between `Int` values. Exact comparison costs nothing and is easy to implement.

## Strings: code points, byte length and the cost of indexing

**Decision.** A string is a sequence of Unicode scalar values stored as UTF-8. Length, positions, `substring`, `at`, `split("")` and every index count code points. `byteLength()` gives the size in bytes. Strings are not indexable with `[]`; the equivalents are linear in the string, and the specified cost says so. There is no normalisation and no grapheme handling. ([section 13.3](13-stdlib-core.md#133-strings))

**Alternatives.** UTF-16 units; bytes; grapheme clusters; O(1) indexing by requiring a fixed-width representation.

**Reason.** Code points are the smallest unit that never splits a character into invalid pieces, and they are what the documentation of the language states. Grapheme segmentation depends on a large, changing table. O(1) indexing would force wasteful storage; an implementation can cache a count, but the language does not promise it.

## Identifiers are ASCII

**Decision.** An identifier is `[A-Za-z_][A-Za-z0-9_]*`. Any other code point outside a string, a filter literal or a comment is an unrecognised character. ([section 1.3](01-lexical-structure.md#13-identifiers-and-keywords))

**Alternatives.** Any Unicode letter.

**Reason.** Unicode identifiers require a normalisation rule and admit look-alike characters that make two different names read the same, which is a security problem for a language meant to run untrusted code. A non-ASCII name can be added in a later minor version with a proper rule; removing one could not be.

## Byte order mark, line terminators and dangerous characters

**Decision.** A single leading U+FEFF is ignored. CR LF and LF are the line terminators and a lone CR is an error. Bidirectional control characters and C0 controls other than tab are rejected everywhere, including in strings and comments. ([section 1.1](01-lexical-structure.md#11-source-text))

**Alternatives.** Rejecting a byte order mark; allowing bidirectional controls in comments and strings.

**Reason.** Editors on some platforms add the mark and users cannot see it. Bidirectional controls can make source display differently from how it lexes, so a program can hide its behaviour from a reviewer; a program that needs one writes an escape.

## String escapes

**Decision.** The escapes are `\"`, `\\`, `\n`, `\t`, `\r`, `\$` and `\u{X}`. ([section 1.5](01-lexical-structure.md#15-string-literals))

**Alternatives.** Only the first six; also `\0`, `\xNN`.

**Reason.** Without a code point escape there is no way to write a non-printing character or a bidirectional control in a literal, which the previous decision forbids writing raw. `\u{0}` covers `\0`.

## Equality

**Decision.** Equality is structural for lists, maps, sets, records, enums and optionals; `Filter` and function values, and anything containing them, cannot be compared. Strings are equal when their code points are. ([section 3.5](03-expressions.md#35-comparison-and-equality))

**Alternatives.** Identity equality for functions; case-insensitive strings.

**Reason.** Identity for functions makes results depend on whether an implementation shares closures. Strings compare exactly; anything else is a library function.

## Comparing an optional with its inner type

**Decision.** `==` and `!=` accept an optional and a value of its inner type (`owner == "Ada"` where `owner` is `String?`), and an optional and `none`. The comparison is false when the optional is absent. Comparing `none` with a value that is not optional is an error. ([section 3.5](03-expressions.md#35-comparison-and-equality))

**Alternatives.** Allowing only `== none`; requiring narrowing before every comparison.

**Reason.** Without this every equality test on an optional field would need a guard clause first, which is the noise the optional type is meant to avoid, and the result is unambiguous.

## Ordering and the sort algorithm

**Decision.** `<` is defined for numbers and strings. Sorting is stable, and `sort(compare)` is specified as a top-down merge sort in which the comparator is called on `(left, right)` and the left element is taken when the result is not positive. ([section 14.1](14-stdlib-collections.md#141-lists))

**Alternatives.** Leaving the algorithm to implementations; an unstable sort.

**Reason.** A comparator can have side effects and can be inconsistent. If the algorithm is not fixed, the order in which a comparator is called, and so the output of a program that prints inside it, differs between implementations.

## Faults and recoverable results

**Decision.** There are exactly two ways to fail. A `Result` value is recoverable and carries every failure a correct program can expect from its environment. A fault aborts the run and cannot be caught: limits, overflow, division by zero, a result that is not a number, an invalid argument to a library function, cancellation. There is no `try`. ([section 8.1](08-errors-and-results.md#81-faults-and-recoverable-errors))

**Alternatives.** Catchable faults; exceptions.

**Reason.** If a script can catch a fault it can catch a resource fault, and then a script can defeat its own limits. A fault for a wrong call (a zero divisor) is a programming error; a `Result` for bad input (a text that is not a number) is not, and the library draws that line the same way everywhere.

## Result is an enum, and the `?` operator

**Decision.** `Result<T, E>` is a built-in enum with the variants `Ok` and `Err`, both type arguments required. Postfix `?` unwraps `Ok` or returns the `Err` from the enclosing function, which must return a `Result` with the identical error type. There is no automatic conversion of error types. ([section 8.3](08-errors-and-results.md#83-the--operator))

**Alternatives.** A record with two optional fields; automatic conversion through a trait; a `try` statement.

**Reason.** A sum type removes the state in which both or neither field is present and makes the payload usable after a match without narrowing. Conversion of errors needs a type-class mechanism the language does not have; `mapError` covers the need.

## Optionals do not nest

**Decision.** `T??` is a syntax error; an optional applied to an optional type by substitution is the same optional type, so indexing a `[Int?]` gives `Int?`. ([section 2.4](02-types.md#24-optional-types))

**Alternatives.** Nested optionals with a distinct absent case at each level.

**Reason.** Nested optionals need a way to write and match the inner absence, and they interact awkwardly with narrowing. Programs that must distinguish the two cases use an enum.

## Evaluation order

**Decision.** Evaluation is left to right and depth first: callee before arguments, operands in order, record fields and list elements in order, map keys before their values. `and` and `or` short-circuit. ([section 3.2](03-expressions.md#32-order-of-evaluation))

**Alternatives.** Leaving the order of operands and arguments to the implementation.

**Reason.** Effects (`print`, capability calls) and faults are observable, so the order is part of the meaning of a program.

## Closures capture bindings

**Decision.** A closure shares the bindings of the enclosing scopes; it does not copy their values. Each execution of a declaration, including each iteration of a loop, creates a new binding. ([section 5.4](05-functions-and-closures.md#54-capture))

**Alternatives.** Capture by value.

**Reason.** It is what programmers who use closures for counters and callbacks expect, and it needs no capture mode because there is no concurrency. Fresh loop bindings avoid the classic mistake of every closure seeing the last iteration.

## Shadowing and the prelude

**Decision.** No name may be declared where it is already visible, in any scope, in the value namespace and in the type namespace, and every top-level name of a file counts as visible in every function body of that file. The one exception is the prelude: a program may declare a prelude name, other than the nine built-in types, and its declaration takes precedence from the place its own name comes into scope, in its own file only; built-in functions and the `?` operator keep using the prelude entities. Capability names have no exception. ([section 4.6](04-statements.md#46-scope-and-shadowing))

**Alternatives.** Ordinary lexical shadowing; no exception for the prelude.

**Reason.** Shadowing produces the most common confusing bug in review. But if every prelude name were reserved, adding `clamp` to the library in a minor version would break every program that had a variable called `clamp`. Letting program declarations win keeps additions safe. Capabilities are exempt from the exemption, because a reader must always be able to tell whether a call needs `effect`.

## Optional narrowing

**Decision.** Narrowing is sound and flow based. `and`, `or` and `not` narrow by their truth sets. Facts are dropped at loop entry for variables assigned in the loop (though the loop condition re-establishes its own), at closure boundaries for variables, and always for `variable`s that another function can assign. Facts on constants survive into closures. A top-level function body starts with no facts. ([section 4.8](04-statements.md#48-optional-narrowing))

**Alternatives.** Narrowing that ignores assignments in closures; no narrowing of variables at all.

**Reason.** Values are immutable, but bindings are not, so a closure that runs later can find a variable reassigned. The rules keep narrowing useful for the common cases (guard clauses, `while x != none`) and make an accepted program never meet a `none` that narrowing excluded.

## Generics and inference

**Decision.** Type parameters are declared after the name, usable throughout the body, opaque there, and never written at a call. They are inferred from the receiver and the non-closure arguments, then from the expected type, then from closure bodies. Closure parameters take their types from the expected function type. There are no constraints. ([section 2.10](02-types.md#210-generics))

**Alternatives.** Explicit type arguments at calls; trait bounds.

**Reason.** Explicit arguments would need `<` to be ambiguous in expressions. Bounds are a large feature; sorting was solved with a comparator and `contains` with an equatable requirement of the built-in operations. Closure parameter inference removes most of the annotation noise that made callbacks heavy.

## Effect typing

**Decision.** Effect is part of a function's type: `effect fn(A): B`. Capability functions have effectful types, everything else is pure, a pure function is assignable to an effectful type and not the reverse, and every call through a value of effectful type is written with `effect`. A user function that calls capabilities inside is still a pure function to its callers. ([section 9.4](09-effects-and-capabilities.md#94-effect-in-function-types))

**Alternatives.** Inferring effects through every user function; leaving aliasing unchecked.

**Reason.** Aliasing (`constant p = print`) or passing a capability as an argument would remove the marker, which would make it unreliable. Tracking the effect in the type closes that hole with a small change. Inferring effects through user functions would put `effect` on nearly every call in a program and would make a change deep inside a function change every caller.

## Modules: paths and initialisation

**Decision.** Imports are relative paths without extension, resolved lexically to a canonical case-sensitive path relative to a root; climbing above the root is an error; two spellings of one path are one module. Modules initialise once, depth first in import order. Only the entry module may return from its top level. Exports are `fn`, `record`, `enum` and `constant`. ([Chapter 10](10-modules.md))

**Alternatives.** Package names; re-exports and renaming; letting imported modules return early.

**Reason.** Canonical paths make the module graph independent of spelling and host, and confinement keeps an import from reaching files outside the program. An early return in an imported module would leave later names uninitialised and break the guarantee that imports are ready when used.

## Top-level declaration order

**Decision.** Top-level `fn`, `record` and `enum` declarations are visible from the first statement; constants and variables are initialised in source order. A top-level statement may not name a function that (transitively, by name) uses a top-level binding declared after that statement. ([section 4.10](04-statements.md#410-top-level-declaration-order))

**Alternatives.** A runtime error for early use; forbidding early calls entirely; requiring functions to be declared before use.

**Reason.** Hoisting functions is what lets programs read top-down. A compile-time rule that follows names is exact enough to make the run-time error unreachable without whole-program analysis of calls through values.

## Steps

**Decision.** A step is charged for every statement, every loop iteration and every function call, and a cost of 1 plus 1 per 64 elements or bytes for library operations, charged before the work. ([section 11.3](11-resource-limits.md#113-steps))

**Alternatives.** Counting bytecode instructions; counting only loop iterations.

**Reason.** Bytecode instructions are an implementation detail, and a limit measured in them would give different results on different implementations. Statements and calls are defined by the language. Charging library work by size keeps a single `sort` of a huge list from being free.

## Memory accounting

**Decision.** Memory is a logical count computed from fixed node sizes: a per-run counter of the bytes allocated, and, at the end of each run, the tree size of the values held by top-level bindings together with the bindings that reachable closures captured. ([section 11.4](11-resource-limits.md#114-memory))

**Alternatives.** Measuring the host's heap; counting live bytes with a collector.

**Reason.** The host's heap size depends on the allocator and collector, so a program would hit its limit at a different place on each implementation. A logical count does not, is checked before an allocation happens, and needs no cooperation from a garbage collector.

## Default limits

**Decision.** 100,000,000 steps, 64 MiB of memory, 16 MiB of output, and 1,000 for call depth, in every host. A host may change them but may not switch them off. ([section 11.1](11-resource-limits.md#111-the-limits))

**Reason.** The same program has to behave the same in a command line, a workflow runner, an editor and a cloud function. The values are far above what real scripts need and low enough that a runaway program ends in seconds. A call depth is a language limit, and the requirement is that an implementation can honour it safely, not that it uses a particular stack.

## Cancellation and yield

**Decision.** A safepoint occurs whenever a charge makes the step counter reach or pass a multiple of 1,024, and after every capability call. At a safepoint the run checks the cancellation token and calls an optional yield callback that may cancel. No charge exceeds 1,024, and long library operations do their work in slices of at most that size, so they reach safepoints. Cancellation is a fault. ([section 11.7](11-resource-limits.md#117-cancellation-and-yield))

**Alternatives.** Checking a timer inside the implementation; cancelling only at loop heads.

**Reason.** A cancellation that never fires against `while true {}` is not a cancellation. Defining the safepoints in steps makes the latency the same on every implementation, and the yield callback is where a host schedules other work or checks a wall clock, which is the host's business.

## Instance lifecycle

**Decision.** An instance is loaded once and called through exported functions of the entry module by name, with typed arguments. Top-level variables and everything reachable from top-level bindings persist. One call runs at a time. Any fault makes the instance faulted for good. There is no in-place code update. ([Chapter 12](12-instances.md))

**Alternatives.** A frame-pump capability the script calls into; letting an instance continue after a fault; hot replacement of code.

**Reason.** Exported functions keep control with the host, which is what a game, a server and a cloud function all need. A fault can leave several variables half updated, and no rule makes that state safe. Hot replacement raises the meaning of every closure that exists when the code changes, and can be added as a host convention later.

## Enums and patterns

**Decision.** Enum payloads are positional, variants are constructed through the enum name (`Shape.Circle(1.0)`) and written bare in patterns, where an initial upper-case letter tells a variant from a binding. Patterns are literals, `none`, `_`, bindings, variants, records and lists; there are no guards. ([Chapter 7](07-patterns.md))

**Alternatives.** Named payload fields; bare variant names in expressions; guards.

**Reason.** Qualified construction keeps names unambiguous without an import list, and bare patterns cost nothing because the scrutinee's type says which enum. The upper-case rule lets a pattern be read without the type. Guards are a second way to write a condition inside `match`, which `if` already covers.

## Destructuring declarations

**Decision.** `constant`, `variable` and `for` can bind through record patterns and list patterns (`constant { x, y } = p`, `constant [first, ..rest] = xs`). Literal, `none` and variant patterns are not allowed there; `match` handles them. A list pattern that does not fit the length of the list is a fault (HS1014). ([section 7.8](07-patterns.md#78-destructuring-declarations-and-for))

**Alternatives.** Record patterns only; treating a list mismatch as `none` bindings; making any refutable pattern a compile error.

**Reason.** Destructuring lists is one of the two forms the language needs, and requiring the length to fit is the expectation the pattern states. A list pattern in a declaration is a claim about the shape of the data; if the claim is wrong the program has a bug, which is what a fault is for, and code that does not know the length has `match`.

## Exhaustiveness and unreachable arms

**Decision.** A `match` must be exhaustive, decided by the standard usefulness algorithm, which the specification states. An arm that can never run is an error, and repeated literals have their own code. `else` is optional when the arms already cover everything. ([section 7.7](07-patterns.md#77-exhaustiveness-and-unreachable-arms))

**Alternatives.** Requiring `else` always; making unreachable arms a warning.

**Reason.** With enums, exhaustiveness is the main safety benefit of `match`. A dead arm is almost always a mistake and duplicate values are errors.

## Records stay structural and are not recursive

**Decision.** A record declaration names a structural type, and a record cannot contain itself. Recursive data is written with an enum. ([section 2.5](02-types.md#25-record-types))

**Alternatives.** Nominal records; recursive records.

**Reason.** Structural records need no constructor and were already the language's design. Recursive structural types would need equality and inference over cyclic types, which an enum with named cases avoids.

## Methods are not values

**Decision.** `xs.map` without a call is an error; methods exist only for built-in types, and a script cannot define one. Free functions are used for numbers and records. ([section 3.7](03-expressions.md#37-calls-fields-methods-and-indexing))

**Reason.** A method value would need a rule for what it captures and what its type is. A closure is the explicit way to get a function value.

## Trailing commas and terminators

**Decision.** Trailing commas are allowed in every bracketed list. They are not allowed in the unbracketed lists (`uses`, `import` and the alternatives of a match arm) because a comma at the end of a line continues onto the next line. There are no leading-dot method chains, and `else` must follow the `}` on the same line. ([section 1.8](01-lexical-structure.md#18-statement-terminators))

**Alternatives.** Leading-dot chains, which need lookahead across a line break.

**Reason.** The statement terminator rule is short enough to state completely and needs no lookahead. Leading-dot chains would need an exception to it.

## Reserved words

**Decision.** `test` and `assert` are reserved now, in addition to `enum`, so that testing can be added without breaking a program. ([section 1.3](01-lexical-structure.md#13-identifiers-and-keywords))

**Reason.** A word cannot be made reserved later without breaking programs that use it as a name.

## Diagnostics: codes, order, cap and comparison

**Decision.** Every diagnostic has a stable code, a severity, a file and a span. Diagnostics are reported in a canonical order, at most 100 per file followed by one notice that more were found. Conformance compares codes, severities, files and lines, not message text. Only three warnings exist. ([Chapter 17](17-diagnostics.md))

**Alternatives.** Comparing message text; more warnings.

**Reason.** Message text is for people and should be free to improve. A cap keeps hostile input from producing output larger than the input. Warnings that a program can safely ignore are not worth a place in the contract beyond the three that catch real mistakes.

## Filter literals as an embedded language

**Decision.** A filter literal is a backtick-delimited value of the type `Filter`, parsed and validated at check time and opaque afterwards. Its language, grammar and semantics are defined in a separate appendix, and there is no way to build a filter at run time. ([Appendix A](appendix-a-filters.md))

**Alternatives.** Building filters from strings at run time; leaving the language to the host.

**Reason.** A literal can be checked before the program runs, which is what turns a typo into a compile error. A filter built from a string would bring back injection.

## Filter strictness

**Decision.** A filter is parsed with no recovery and reports its first error. Half-typed input, a value that does not directly follow its operator, stray parentheses, unterminated quotes, impossible dates, a relative offset above 5000 days and, given a schema, unknown fields and values are all errors. ([sections A.3](appendix-a-filters.md#a3-grammar) and [A.7](appendix-a-filters.md#a7-validation-against-a-schema))

**Alternatives.** A recovering parser that turns mistakes into free text.

**Reason.** Recovery is right for a search box the user is still typing in, and wrong for a compiled program: a filter that silently matches everything can make an alert rule fire on every event. Reporting only the first error also removes the flood of near-identical errors from deeply nested input.

## Filter value semantics

**Decision.** A value that has the wrong kind for a field's type is treated as absent; nothing is converted (an absent number is not zero). An absent value satisfies only `none` and `!=`. Numbers compare exactly. Dates are calendar dates that the host has already put in the zone of `today()`. Comparisons of text are case-insensitive. ([section A.8](appendix-a-filters.md#a8-evaluation))

**Alternatives.** Converting null to zero and strings to dates.

**Reason.** Matching an absent count with `count:0` and reading `starred:ture` as false are the failures this rule prevents; a rule with no implicit conversion has nothing to memorise and cannot go wrong quietly.

## Capability set and grants

**Decision.** The standard set is `print`, `clock`, `random`, `stdin`, `args`, `env`, `fs` and `http`, of which only the first three are granted by default. Grants can be scoped: paths for `fs`, with separate `read` and `write`, host names for `http`, variable names for `env`. A textual form for grants is specified so that every command line accepts the same syntax. ([section 9.5](09-effects-and-capabilities.md#95-scoped-grants))

**Alternatives.** An interactive question at run time; a manifest file.

**Reason.** An interactive question has no answer for a script run without a terminal, and a manifest repeats what `uses` already says. Scoping is what makes "safe to run untrusted scripts" true once input, files and the network exist.

## Denied access is a recoverable error

**Decision.** A call outside the granted scope returns `Err(Denied(...))` for `fs` and `http`, and `none` for `env`, and does nothing else. ([section 9.5](09-effects-and-capabilities.md#95-scoped-grants))

**Alternatives.** A fault.

**Reason.** A denied call is an expected outcome of running a script with a narrow grant, and programs should be able to report it. It reveals nothing about paths outside the scope, because `exists` is denied in the same way. `env` returns `none` so a script cannot discover which variables exist.

## Shape of the standard capabilities

**Decision.** `print`, `args` and `env` are single functions; `clock`, `random`, `stdin`, `fs` and `http` are namespaces. Files are text. Expected failures are `Result` values whose errors are small enums. The messages inside them are for people and are not specified. ([Chapter 16](16-standard-capabilities.md))

**Alternatives.** Bytes and streams; one error string.

**Reason.** Text covers the scripts the language is for, and streams and binary data can be added in a later minor version. An enum error lets a program distinguish a missing file from a refused one without parsing text.

## JSON and time in the library

**Decision.** JSON is a `Json` enum with a strict parser and an encoder that writes keys in ascending order. Time is integers: an instant in milliseconds and a date in days, with UTC and fixed offsets only. ([Chapter 15](15-stdlib-data.md))

**Alternatives.** A dynamic value type; a time zone database.

**Reason.** There is no `any` type, so JSON has to be an enum. A time zone database is large and changes; the clock capability and `time` cover UTC, and richer calendar support belongs in a library written in the language.

## Accuracy of transcendental functions

**Decision.** `sqrt` and the basic operators are correctly rounded. `sin`, `cos`, `tan`, `atan2`, `exp`, `ln` and `pow` are within one unit in the last place, and `pow` is exact when the result is representable. Two implementations may therefore differ in the last digit of these results, which the evaluation-order guarantee and the corpus both state as an exception. ([section 13.2](13-stdlib-core.md#132-numbers))

**Alternatives.** Requiring correctly rounded results everywhere; requiring bit-identical results by naming one algorithm; leaving out trigonometry.

**Reason.** Correct rounding of these functions is a research problem and would forbid using the platform library. Games and graphics need trigonometry. Naming one algorithm would tie the language to one library and its bugs. The tolerance is stated so that nothing is left open.

## The join of a list of types

**Decision.** The type of a list or map literal without an expected type is the join of all its element types taken together: `none` elements are set aside, the rest must be identical or a mix of `Int` and `Float` (giving `Float`) or function types that differ only in effect, and the result is optional if a `none` or an optional element was present. So `[1, none, 2.5]` and `[1, 2.5, none]` have the same type. ([section 2.9](02-types.md#29-inference))

**Alternatives.** Joining pairwise from the left; requiring an annotation whenever the elements differ.

**Reason.** A pairwise join is not associative, so the accepted programs would depend on element order and on how an implementation folds. Joining the whole set has one answer.

## Exports carry no capability

**Decision.** The type of an exported constant, and the return type of an exported function, may not contain an effectful function type. Grants belong to a run, not to a module. ([section 10.4](10-modules.md#104-exports))

**Alternatives.** Allowing exports of capability values.

**Reason.** A capability is an ordinary value. Exporting one would give it to a module that has no `uses` line for it, and a reader could no longer tell from the first lines of a file what it can touch.

## Retained size counts what closures capture

**Decision.** The retained size of an instance is the tree size of its top-level bindings plus the bindings that reachable closures captured, each counted once. Tree sizes saturate at 2^64 - 1. ([section 11.4](11-resource-limits.md#114-memory))

**Alternatives.** Giving a closure a fixed size.

**Reason.** With a fixed size, a program could keep a large local value alive through a closure stored in a top-level variable, and the retained limit would never be reached.

## What comparisons and capability calls cost

**Decision.** A comparison of two values that are not scalars costs 1 plus 1 per 64 units of the smaller tree size, charged before it is made. A capability call is charged for the size of its arguments before it and the size of its result after it. ([section 11.3](11-resource-limits.md#113-steps))

**Alternatives.** Charging nothing for operators; charging the whole call before it.

**Reason.** Sharing lets a program build two values of astronomical tree size in a few statements, so an equality test with no charge would run for ever without a safepoint. The size of a result is not known before a capability has produced it, so it can only be charged after.

## Cancellation belongs to an operation

**Decision.** Each of `check`, `load` and `call` takes its own cancellation token, and a token that is already set faults the operation before any script code runs. `cancel(instance)` sets the token of the operation in progress and is the only operation allowed while one runs. ([section 12.1](12-instances.md#121-host-operations))

**Alternatives.** One token per instance.

**Reason.** A timer that fires just before an operation starts would otherwise be lost. A token per operation gives a host a deadline per request.

## The capability boundary

**Decision.** Capability interfaces use only transferable types, so a host never calls script code. A value that a host returns is accepted only if it is exactly a language value of the declared type, apart from an `Int` for a `Float` at the top level, and anything else is a fault. A capability must not build a result larger than the remaining memory allowance. ([section 9.6](09-effects-and-capabilities.md#96-capability-values))

**Alternatives.** Converting what a host returns to the nearest valid value.

**Reason.** A silent conversion of a `NaN` or of invalid text would hide a defect in the host and let a value into the language that the language has no way to represent.

## Files and the network are confined by handle and by address

**Decision.** The check that a path is inside the granted directory and the access to it are one operation on a handle, and a host name is resolved once and never to a loopback, private, link-local or metadata address unless the grant is that literal address. ([section 16.8](16-standard-capabilities.md#168-fs) and [section 16.9](16-standard-capabilities.md#169-http))

**Alternatives.** Checking a path string and then opening it; matching host names by text alone.

**Reason.** A link that changes between a check and an open, and a name that resolves to an internal address, are both ways out of a grant that a text comparison cannot see.

## JSON error messages and offsets

**Decision.** Each problem in JSON text has one message and one offset, given in a table. A number is scanned as one token, and a literal is matched byte by byte. ([section 15.1](15-stdlib-data.md#151-json))

**Alternatives.** Leaving the message and offset to the parser.

**Reason.** The message and the offset are values a program can read, so two implementations must agree on them.

## Which names warn

**Decision.** An unused `constant` or `variable`, including the names of a destructuring pattern, is a warning. Parameters, `for` variables, `match` arm bindings, exported names and names that begin with `_` are not. ([section 17.3](17-diagnostics.md#173-warnings))

**Alternatives.** Warning for every unused binding.

**Reason.** A parameter is part of a signature, and a loop variable or an arm binding names part of a value that the construct has to bind. A name that the program chose to declare is the one worth a warning.

## Checking is bounded too

**Decision.** The syntax tree has a bounded weight, the number of modules and their total source are bounded, the exhaustiveness check has a work budget, and the size of a type is bounded. Checking and loading take a cancellation token. ([section 1.10](01-lexical-structure.md#110-structural-limits))

**Alternatives.** Bounding only nesting depth and chain length.

**Reason.** The two limits together allow a tree of 125,000 levels, a match can take exponential time, and a type built from an alias applied to itself doubles in size at every step. A language that is safe to run when hostile has to be safe to check.

## Not in version 1

**Decision.** Left out of version 1 and possible later: bitwise operations and wrapping arithmetic, regular expressions, constraints on generics, `test` and `assert`, streams and binary files, a package manager, hot replacement of code, and non-ASCII identifiers. ([README](README.md#what-this-specification-does-not-define))

**Reason.** Each can be added in a minor version without changing any accepted program, so none needs to hold up version 1.

## Numeric literal limits

**Decision.** An integer literal is at most 9223372036854775807, except that the literal 9223372036854775808 is accepted as the operand of unary minus, and after the minus of a literal pattern, so the smallest `Int` can be written. A float literal that rounds to infinity is an error. ([section 1.4](01-lexical-structure.md#14-number-literals))

**Reason.** A literal that does not fit is known at compile time and belongs with the other compile errors, and the smallest integer has to be writable.

## Implementation limits

**Decision.** An implementation may impose limits of its own beyond those the specification fixes, provided it documents each one and none of them rejects or faults a program of the conformance corpus. A program that exceeds one before running is rejected with HS0901 at the construct that exceeds it; a run that reaches one faults with HS1015. Neither is an internal error. Adding the two codes made this version 1.1.0. ([section 1.10](01-lexical-structure.md#110-structural-limits))

**Alternatives.** No implementation limits at all, so that every width up to the source size limit must run; reporting such a limit as an internal error (HS1099); fixing one number for every implementation.

**Reason.** Section 1.10 bounds the widths of a program only by the size of its source, so a 4 MiB source can hold a call of two million arguments or a function of hundreds of thousands of locals. Every real implementation has some machine limit below that, and a register machine meets one first. HS1099 says the implementation is broken, which is wrong for a documented limit and gives the author nothing to act on. A single fixed number would bind every implementation to one design. Requiring the corpus to pass keeps a limit from reaching programs anyone writes, and the corpus has cases far wider and deeper than ordinary code for that purpose.

## Versioning and licence

**Decision.** The specification follows semantic versioning, promises source compatibility within a major version, and promises nothing about bytecode, message wording or tool protocols. The specification is licensed under CC BY 4.0. ([README](README.md#versioning))

**Reason.** A promise limited to what programs can observe leaves implementations free to improve everything else.
