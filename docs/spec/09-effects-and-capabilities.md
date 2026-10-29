# 9. Effects and capabilities

HollowScript has no ambient authority. A program cannot print, read the clock, generate a random number, read a file, read its arguments or the environment, or use the network unless the host that runs it has granted that ability, and the program has declared that it uses it. Reading the first few lines of a file therefore tells a reader everything the file could possibly touch outside its own computation.

This chapter defines capabilities, the `effect` marker, how the marker is part of a function's type, and how grants are scoped.

## 9.1 Terms

- A **capability** is a named binding, supplied by the host, through which a program reaches outside itself. Its value is either a single function (`print`) or a namespace, which is a record of functions (`clock`, with the functions `now` and `today`).
- A **capability function** is a function supplied by the host. Every capability function has an *effectful* function type ([section 9.4](#94-effect-in-function-types)).
- A **grant** is the host's decision to make a capability available to a run, possibly limited by a scope ([section 9.5](#95-scoped-grants)).
- The **standard capabilities** are those defined by this specification: `print`, `clock`, `random`, `stdin`, `args`, `env`, `fs` and `http` ([Chapter 16](16-standard-capabilities.md)). Hosts may define others (a task store, a notifier); the language treats them identically.

## 9.2 Declaring and granting

```
usesLine = "uses" identifier { "," identifier } TERM ;
```

A `uses` line names capabilities. `uses print, clock` binds the names `print` and `clock` in the file's scope, spelled exactly as written: there is no renaming, no capitalisation rule and no implicit namespace object.

- All `uses` lines come first in a file, before every `import` and every other statement (HS0106). Several `uses` lines are the same as one combined line.
- A name that the host has not granted to this run, or that the host does not define, is HS0404. The error is reported before any statement runs.
- A capability name may not be redeclared or hidden anywhere in the file: not by a top-level declaration, an import, a parameter, a loop variable, a pattern binding or a local (HS0201). This has no exception. If a capability name could be hidden, a reader could not tell whether a given call needs `effect`. A `uses` line is itself a declaration, so a capability whose name is also a prelude name (a host may define `time`) hides the prelude name in every file that lists it, and the prelude entity is unreachable there ([section 4.6](04-statements.md#46-scope-and-shadowing)). A host may not define a capability whose name is a keyword.
- A name repeated in `uses`, in one line or across lines, is HS0201.
- Capability names are not keywords. A file that does not say `uses print` may declare its own `fn print`.
- `uses` is **not inherited** through imports. Each file declares the capabilities it uses. An imported function that performs effects requires its own file to declare them, and requires the capability to be granted to the run. Grants belong to a run, not to a module: a module that declares `uses fs` receives the whole `fs` grant of the run, so the loader, which decides which modules exist, is part of the trust boundary. Nothing that a module exports may carry a capability to a module that did not declare it ([section 10.4](10-modules.md#104-exports)).

```hollow
uses print, clock

constant now = effect clock.now()
effect print("started at ${now}")
```

## 9.3 The `effect` marker

The marker `effect` is the prefix alternative of the `unary` production ([section 18.8](18-grammar.md#188-expressions)): a prefix on a call. It is written on **every** call whose callee has an effectful function type, whether the call reads, writes or merely observes. There is no exemption for reads: reading the clock is as much a departure from pure computation as writing to a file. The marker is required or forbidden, never optional:

- A call through a callee of effectful type without `effect` is HS0401.
- `effect` on a call whose callee has a pure function type is HS0402.
- `effect` on something that is not a call is HS0403.

More exactly, `effect` applies to a postfix chain ([section 3.3](03-expressions.md#33-operators-and-precedence)). Let the chain be a primary expression followed by calls, field accesses, indexes and `?` operators. Ignoring any trailing `?` operators, the last operation of the chain must be a call; that call is the *effect call* and its callee must have an effectful type. The trailing `?` operators apply to the result of the effect call. The chain may contain other calls earlier, and those must be unmarked pure calls. To apply a method to the result of an effect call, parenthesise:

```hollow
uses clock

constant length = (effect clock.now()).length()
return length
```

`effect clock.now().length()` is HS0402, because the last call in the chain is `length()`, a pure method.

`effect` has the same precedence as `not` and unary `-`. An `effect` call is evaluated exactly like the call inside it ([section 3.2](03-expressions.md#32-order-of-evaluation)).

The marker is never decorative: each `effect` in a program is a checked statement that this call reaches a capability, and each missing one is an error.

## 9.4 Effect in function types

A function type is either **pure**, `fn(A): B`, or **effectful**, `effect fn(A): B` ([section 2.6](02-types.md#26-function-types-and-effect)). Effect is part of the type, not an annotation on a declaration, and it is tracked through the type system like any other part of a type:

- Every capability function has an effectful type. A capability that is a namespace is a record whose function fields have effectful types, so `clock.now` has type `effect fn(): String`.
- A `fn` declaration, a closure literal, a variant constructor and every standard library function have pure types, however their bodies are written. A function that calls capabilities internally is pure as far as its callers are concerned: calling it needs no marker. `effect` marks the point where a call *reaches a capability's function*, at the call that names it.
- A pure function type is assignable to the same function type with `effect`, and the reverse is a type error ([section 2.7](02-types.md#27-assignability-and-numeric-coercion)). The join of two function types that differ only in effect is the effectful one.
- The callee's static type decides. Whether the callee is a capability name, a local alias, a parameter, a field of a record, an element of a list or the result of a call, a call through a value of effectful type needs `effect`, and a call through a value of pure type must not have it.
- A generic type parameter may be instantiated with an effectful function type, and the effect is kept.

Therefore aliasing and passing a capability cannot remove the marker:

```hollow-error
uses print

constant p = print

p("x") // HS0401
```

```hollow-error
uses print

fn apply(f: fn(String), s: String) {
    f(s)
}

apply(print, "x") // HS0301
```

The second program is rejected because `print` is effectful and the parameter `f` says it accepts only pure functions. A function that accepts capabilities says so, and must mark its own call:

```hollow
uses print

fn apply(f: effect fn(String), s: String) {
    effect f(s)
}

apply(print, "x")
apply(fn(text) {
    effect print("closure ${text}")
}, "y")
```

Output:

```text
x
closure y
```

Library functions that take callbacks, such as `map`, `filter`, `each`, `sort`, `reduce` and the `Result` methods, take pure function parameters. A capability function therefore cannot be handed to them directly; a program that wants to call a capability once per element writes a `for` loop.

The other misuses are:

```hollow-error
fn double(x: Int): Int {
    return x * 2
}

return effect double(21) // HS0402
```

```hollow-error
uses clock

constant t = effect clock.now // HS0403
```

## 9.5 Scoped grants

A grant makes a capability available, and may limit *what* the capability can reach. The limit is the grant's **scope**. The checker sees only whether a capability is granted; scopes are enforced by the host each time a capability function is called, so a scope never changes the type of a capability.

A grant has:

- a **name**: the capability's `uses` name;
- an optional **kind**, for capabilities that have separately grantable parts (`read` and `write` for `fs`);
- a list of **scope values**, whose meaning depends on the capability: paths for `fs`, host names for `http`, variable names for `env`.

Grants are additive: several grants for the same capability and kind together allow the union of their scopes.

**Textual form.** Hosts that accept grants as text, such as a command line, use this grammar, so that the same grants can be written the same way everywhere:

```
grant  = name [ ":" kind ] [ "=" scope { "," scope } ] ;
```

Examples: `print`, `fs:read=./data`, `fs:read=./data,./config`, `fs:write=./out`, `http=api.example.com`, `env=HOME,PATH`, `stdin`. A scope value cannot contain a comma; a host that must grant such a value passes it through its programmatic interface instead.

The text is split at the first `:` that comes before any `=`, and then at the first `=`; the scope is split at each `,`. No white space is trimmed: a space is part of the token it is in, and a second `=` belongs to the scope value it is in. An empty scope value (`fs:read=`), an empty element (`a,,b`), an unknown kind (`fs:exec=./x`), a kind on a capability that has no kinds (`print:read`) and a capability name the host does not define are errors in the grant, reported by the host.

**Rules.**

1. Nothing is granted by default other than `print`, `clock` and `random`. A host is free to grant fewer; it may not grant more without being asked.
2. A grant with no scope on a capability that requires one (`fs`, `http`) grants nothing: it is an error in the grant, reported by the host.
3. When a capability function is called with something outside the granted scope, the capability function does not perform the operation and reports the refusal to the program in the way its own definition says ([Chapter 16](16-standard-capabilities.md)): for `fs` and `http`, a `Result` holding the error `Denied`; for `env`, `none` (the same as an unset variable, so a program cannot probe what exists).
4. Scopes are matched after the argument has been normalised in the way the capability defines (a canonical path, a lower-case host name). A scope can never be widened by how an argument is spelled.
5. Whether a scope was granted is the host's decision and is invisible to programs except through the results above.

A host may enforce further limits of its own (for example a maximum response size), and reports them through the same result types.

## 9.6 Capability values

A host declares each capability it offers as a *capability interface*: the capability's name and its type. The type of a single-function capability is an effectful function type. The type of a namespace capability is a record type whose fields are effectful function types (and possibly other values). The interface is given to the checker before it runs. The standard capabilities' interfaces are fixed by this specification.

**Types at the boundary.** The parameter and result types of a capability function are limited to the *transferable* types of [section 12.4](12-instances.md#124-calling-exported-functions): `Int`, `Float`, `Bool`, `String`, and lists, maps, sets, records, enums and optionals of them; a parameter may also be a `Filter` ([section 16.11](16-standard-capabilities.md#1611-host-specific-capabilities)). A function type or a type parameter cannot appear in a capability interface, so a host never calls script code from inside a capability ([section 12.5](12-instances.md#125-concurrency)).

**Arguments.** At a call, the language converts the arguments to the parameter types exactly as for any call ([section 5.2](05-functions-and-closures.md#52-calls)): an `Int` passed for a `Float` parameter is converted before the host sees it.

**Results.** A value that the host returns for the declared type `T` is accepted if it is a language value of `T`, or an `Int` where `T` is `Float` or `Float?`, in which case it is converted; that conversion is made at the top level of the result only, and nothing is converted inside a collection, a record or an enum payload. Anything else faults with HS1013. That includes a `Float` that is not finite, an `Int` outside the 64-bit range, text that is not a sequence of Unicode scalar values (invalid UTF-8, a lone surrogate), a map or set with a repeated key or a key that is not of a key type, a record with a missing or an extra field, and an enum value that names no variant of `T`. A negative zero is normalised to `0.0`, and a map or set given in any order is put in key order by the binding. The same rule accepts the arguments that a host passes to `call` ([section 12.4](12-instances.md#124-calling-exported-functions)), where a value that is not accepted is HS1202.

A host function that fails in a way its declared type cannot express faults with HS1012. A capability whose failures are expected declares a `Result` return type and reports them as `Err`.

**Call context.** Every capability call is given the cancellation state of the run and the remaining memory allowance, which is `maxMemoryBytes` less the allocation counter ([section 11.4](11-resource-limits.md#114-memory)). A capability MUST NOT build a `String` or a collection larger than the allowance: a result that would exceed it makes the run fault with HS1002 without the result being built, and `fs.read` checks the size of the file before it reads it. Once cancellation is set, a capability MUST return as soon as it can, and the run then faults with HS1006 at the safepoint whatever the capability returned.

A capability call may take any amount of real time, and the host implementation may perform asynchronous work. The language has no `await`: a capability call is a single operation whose value is available when the call ends, and the execution of one instance is strictly sequential. After a capability call returns, the runtime checks for cancellation before continuing ([section 11.7](11-resource-limits.md#117-cancellation-and-yield)).

The number of capability calls, and the sizes of what is passed and what is returned, count toward the limits of the run like any other work ([section 11.3](11-resource-limits.md#113-steps)).

## 9.7 The capability interface of a run

For a run, the host gives the checker:

1. the set of granted capability names and their types, with the enum types those types mention;
2. optionally, a *filter schema* for validating filter literals ([Appendix A](appendix-a-filters.md#a7-validation-against-a-schema)).

A `uses` name outside the set is HS0404.

**Types a capability interface declares.** An enum type that the type of a granted capability mentions, such as the `IoError` of `fs`, is in the type namespace of every file of the run, under the name the interface gives it, whether or not that file has a `uses` line for the capability. It is not a prelude name: in a run whose interface does not declare it, a use of the name is HS0208. When a file's `uses` line names a capability outside the set, only the HS0404 is reported for the names that capability would have declared; no HS0208 follows from it. A program may hide such a name by declaring its own, by the rule of [section 4.6](04-statements.md#46-scope-and-shadowing). A record type that a capability's type mentions is not given a name. Records are structural ([section 2.5](02-types.md#25-record-types)), so a program that needs the type declares a record with the same fields.

A program that has no `uses` line, or only `uses` lines for capabilities that do nothing, can only compute a value from its own literals and its imports.
