# The HollowScript language specification

**Version 1.1.1**

HollowScript is a small, statically typed, embeddable programming language with capability-based security, in the class of Lua. This is its normative specification. It is written to be implemented independently: two implementations built from it alone, and checked against the shared conformance corpus ([Chapter 19](19-conformance.md)), behave identically on every program, except in the last binary digit of the results of the functions to which [section 13.2](13-stdlib-core.md#132-numbers) gives a tolerance.

## Contents

| Document | What it defines |
|---|---|
| [1. Lexical structure](01-lexical-structure.md) | source text, tokens, literals, statement terminators, structural limits |
| [2. Types](02-types.md) | the types, optionals, records, function types, assignability, inference, generics |
| [3. Expressions and operators](03-expressions.md) | operators, arithmetic, equality, evaluation order, literals, interpolation |
| [4. Statements and control flow](04-statements.md) | declarations, assignment, conditionals, loops, scope, narrowing, top-level order |
| [5. Functions and closures](05-functions-and-closures.md) | declarations, calls, closures, capture, recursion |
| [6. Records, enums, lists, maps and sets](06-data-types.md) | the data structures |
| [7. Pattern matching and destructuring](07-patterns.md) | patterns, `match`, exhaustiveness |
| [8. Errors and the Result type](08-errors-and-results.md) | faults, `Result`, the `?` operator |
| [9. Effects and capabilities](09-effects-and-capabilities.md) | `uses`, `effect`, effect in function types, scoped grants |
| [10. Modules and imports](10-modules.md) | imports, canonical paths, exports, initialisation order |
| [11. Resource limits](11-resource-limits.md) | steps, memory, output, call depth, cancellation, defaults |
| [12. Persistent instances and host-callable functions](12-instances.md) | load, exported functions, lifecycle |
| [13. Standard library: numbers, strings and records](13-stdlib-core.md) | numeric and string functions, the prelude |
| [14. Standard library: lists, maps and sets](14-stdlib-collections.md) | list, map and set operations, sorting |
| [15. Standard library: JSON and time](15-stdlib-data.md) | JSON encoding and decoding, calendar arithmetic |
| [16. Standard capabilities](16-standard-capabilities.md) | `print`, `clock`, `random`, `stdin`, `args`, `env`, `fs`, `http` |
| [17. Diagnostics](17-diagnostics.md) | the registry of stable diagnostic codes |
| [18. Formal grammar](18-grammar.md) | the complete grammar |
| [19. Conformance](19-conformance.md) | the shared corpus and what conforming means |
| [Appendix A. Filter expressions](appendix-a-filters.md) | the embedded filter language |
| [Decisions](decisions.md) | the language decisions this specification makes, with the reasons |

## How to read it

**Conventions.** The words MUST, MUST NOT, SHOULD and MAY are used in their usual specification sense. A statement in the present tense (for example "the result is") is a requirement. Every rule is stated once; other chapters refer to it by link.

**Code blocks.** Blocks tagged `hollow` are complete programs that are accepted and behave as the text says. `hollow-error` programs are rejected with the diagnostic whose code is in the comment on the line it concerns, `hollow-fault` programs fault at run time with the code in the comment, and `hollow-fragment` blocks are excerpts. Multi-file examples use `hollow-module` and `hollow-module-error`. Blocks tagged `text` give expected output and blocks tagged `json` are illustrative data. Untagged blocks are grammar or signatures. The full convention is in [section 19.6](19-conformance.md#196-examples-in-this-specification).

**Section numbers.** Chapter N has sections N.1, N.2 and so on; a reference such as "section 4.8" means that section of chapter 4. Appendix A has sections A.1 to A.11.

**Diagnostics.** A code such as HS0301 is defined in [Chapter 17](17-diagnostics.md). The text names the code where the rule is stated.

## What this specification does not define

- Any bytecode or binary format. Bytecode is an implementation matter and may change between releases.
- The programming interfaces through which a host embeds an implementation (a Rust crate, a C interface, a WebAssembly module). They implement the operations of [Chapter 12](12-instances.md) and are specified separately.
- The output of a source formatter, the protocol of a language server, or the wording of diagnostic messages beyond the codes and spans of [Chapter 17](17-diagnostics.md).
- Testing and assertions. The words `test` and `assert` are reserved for a later version ([section 1.3](01-lexical-structure.md#13-identifiers-and-keywords)).
- A package manager. Version 1 has relative imports only ([Chapter 10](10-modules.md)).

## Versioning

This specification follows semantic versioning.

- A **patch** version corrects wording or examples without changing the behaviour of any conforming program.
- A **minor** version adds: new prelude functions and types, new methods, new standard capabilities, syntax that was previously an error, new warnings. A program that is accepted and runs without a fault under version 1.x is accepted and behaves the same under every later 1.y, with these exceptions: new warnings may appear; the diagnostics of programs that were already rejected may change; the default resource limits may be raised (never lowered), which can only turn a fault into a success; the Unicode version behind case conversion and white space may advance ([section 13.3](13-stdlib-core.md#133-strings)); and the last digit of the results of the functions with a tolerance ([section 13.2](13-stdlib-core.md#132-numbers)) is not promised.
- A **major** version may change the meaning of accepted programs.

New names may be added to the prelude in a minor version without breaking programs, because a program's own declaration of a name takes precedence over the prelude ([section 4.6](04-statements.md#46-scope-and-shadowing)). Diagnostic codes are stable and are never reused. Nothing is promised about bytecode, message wording or tool protocols. The C interface for embedding is versioned separately and is stable within a major version; the Rust interface may change in a minor version. This specification promises nothing about either beyond the operations of [Chapter 12](12-instances.md).

## Licence

This specification is licensed under the Creative Commons Attribution 4.0 International licence (CC BY 4.0, full text in [`../LICENSE-CC-BY-4.0.txt`](../LICENSE-CC-BY-4.0.txt)), so that anyone may write conforming tools. The name HollowScript is a trademark, and the licence does not grant rights to the name.
