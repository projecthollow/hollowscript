# 8. Errors and the Result type

HollowScript has exactly two ways for a computation to fail, and they are kept apart on purpose:

- A **recoverable error** is an ordinary value: a `Result` that holds an error. The program decides what to do with it. All failures that a correct program can expect (bad input, a missing file, a refused connection) are reported this way.
- A **fault** aborts the run. It cannot be caught. Faults are the boundary of the host: resource limits, arithmetic that has no answer, cancellation, and misuse of a library function.

There is no `try`, no `catch`, no exception, and no way for a script to observe or suppress a fault.

## 8.1 Faults and recoverable errors

A fault ends the current run (the load of a file, or one call of an exported function) at once. Nothing after the faulting operation runs. Output already written stays written. The fault is reported to the host as a diagnostic with severity `error`, one of the codes below, a message, the span of the operation that faulted, and the innermost frames of the call stack, at most 32 of them, each with a function name, a file and a span. After a fault, the instance that was running is *faulted* and can run nothing further ([section 12.6](12-instances.md#126-faults-in-instances)).

| Code | Fault | Class |
|---|---|---|
| HS1001 | step limit exceeded | resource |
| HS1002 | memory limit exceeded by allocation | resource |
| HS1003 | retained memory limit exceeded | resource |
| HS1004 | output limit exceeded | resource |
| HS1005 | call depth exceeded | resource |
| HS1006 | cancelled | resource |
| HS1007 | `Int` overflow | resource |
| HS1008 | division by zero | domain |
| HS1009 | a `Float` result that is not a number | domain |
| HS1010 | a `Float` result that is infinite | domain |
| HS1011 | invalid argument to a library function | domain |
| HS1012 | a capability failed inside the host | host |
| HS1013 | a capability returned a value of the wrong type | host |
| HS1014 | a list pattern in a declaration or `for` did not match the length of the list | domain |
| HS1015 | a limit of the implementation, not of the language, was reached ([section 1.10](01-lexical-structure.md#110-structural-limits)) | resource |
| HS1099 | internal error in the implementation | internal |

The classes exist for hosts that report or count faults differently; they do not change what a script can do about them. `Int` overflow is a resource-class fault because a checked 64-bit integer is a resource the language rations: it is never silently wrapped, and never recoverable.

A fault is never used for something a correct program could reasonably expect from its environment. A function that can fail for such a reason returns a `Result`. A fault is used when the call itself is wrong (a divisor of zero, a negative repeat count) or when a limit is reached.

## 8.2 The Result type

`Result<T, E>` is a built-in enum in the prelude, defined as if by:

```
enum Result<T, E> {
    Ok(T),
    Err(E),
}
```

Both type arguments are required; `Result<Int>` is HS0303. `T` may be any type, including an optional type, and `E` may be any type. `Result` is a real sum type: a value is either `Ok` with a `T` or `Err` with an `E`, never both and never neither, and after `match` selects an arm the payload is present with its type, with no narrowing needed.

**Construction.** `Result.Ok(value)` and `Result.Err(error)` follow the rule for every enum ([section 6.3](06-data-types.md#63-enums)). Two prelude functions are provided for brevity and are the usual way to write them:

```
success<T, E>(value: T): Result<T, E>
failure<T, E>(error: E): Result<T, E>
```

`success(v)` is the same value as `Result.Ok(v)`, and `failure(e)` the same as `Result.Err(e)`. The argument determines `T` (or `E`); the other type argument is taken from the expected type ([section 2.10](02-types.md#210-generics)), so `return failure("no")` in a function returning `Result<Int, String>` needs no annotation, and `constant r = success(1)` without an expected type is HS0308.

**Patterns.** `Ok(v)` and `Err(e)` are the variant patterns, as for any enum ([Chapter 7](07-patterns.md)). A `match` on a `Result` is exhaustive with an arm for each of them.

**Equality.** `==` compares the variant and the payload, and is defined when `T` and `E` are equatable.

Errors carry a value of any type. The standard library uses `String` for simple cases (`parseInt`) and small enums or records where a caller may want to tell errors apart ([section 16.1](16-standard-capabilities.md#161-shared-types) and [section 15.1](15-stdlib-data.md#151-json)).

## 8.3 The `?` operator

`e?` is a postfix operator, the `?` alternative of `postfixOp` in [section 18.8](18-grammar.md#188-expressions). The operand `e` must have type `Result<T, E>` (HS0331 otherwise). The expression `e?` has type `T`.

- If `e` evaluates to `Ok(v)`, the value of `e?` is `v`.
- If `e` evaluates to `Err(x)`, the innermost enclosing function or closure returns `Result.Err(x)` immediately, exactly as if the program had executed `return Result.Err(x)`.

For this the enclosing function or closure must have a return type `Result<U, E>` for some `U` and with the *identical* error type `E` (HS0332 otherwise). There is no automatic conversion of error types; a program that combines errors of different types converts one with `mapError` ([section 8.4](#84-result-methods)). A closure that uses `?` must have a known return type, from an annotation or from its expected function type; otherwise `?` in it is HS0332.

At the top level of the *entry* file of a run, `e?` may be used: on `Err(x)` the initialisation of the file ends and the result of the run is `Result.Err(x)` ([section 4.7](04-statements.md#47-return-break-continue-and-reachability)). The result of a run is dynamically typed and is not checked against the types of the file's other top-level `return` statements. At the top level of an imported file, `?` is HS0332.

The operand of `?` is evaluated with no expected type, so `constant r = success(1)?` cannot infer the error type of `success` and is HS0308, while `constant r = parseInt("1")?` is fine. `?` may be followed by another `?`, a call, a field access or an index: `read(path)?.length()`. When `e?` is used as a whole statement, the value of type `T` is discarded ([section 4.9](04-statements.md#49-expression-statements)); this is how a function that only cares about failure is written.

```hollow
fn parsePair(a: String, b: String): Result<Int, String> {
    constant x = parseInt(a)?
    constant y = parseInt(b)?
    return success(x + y)
}

fn show(r: Result<Int, String>): String {
    match r {
        Ok(n) {
            return "sum ${n}"
        }
        Err(message) {
            return "error: ${message}"
        }
    }
}

return [show(parsePair("4", "5")), show(parsePair("4", "x"))]
```

The program returns `["sum 9", "error: invalid integer"]`.

```hollow-error
constant r = success(1)? // HS0308
return r
```

## 8.4 Result methods

The methods of `Result<T, E>` are pure. Callbacks are ordinary pure function values.

```
isOk(): Bool
isErr(): Bool
ok(): T?
err(): E?
unwrapOr(fallback: T): T
map<U>(transform: fn(T): U): Result<U, E>
mapError<F>(transform: fn(E): F): Result<T, F>
andThen<U>(next: fn(T): Result<U, E>): Result<U, E>
```

- `isOk()` is true for `Ok` and false for `Err`; `isErr()` is the opposite.
- `ok()` is the payload of `Ok` as an optional, and `none` for `Err`. `err()` is the payload of `Err` as an optional, and `none` for `Ok`. If `T` (or `E`) is itself optional, the result is that same optional type and `none` cannot be told from an absent payload ([section 2.4](02-types.md#24-optional-types)).
- `unwrapOr(fallback)` is the `Ok` payload, or `fallback`. The argument is always evaluated, before the call.
- `map(f)` applies `f` to the `Ok` payload and wraps the result in `Ok`; an `Err` is returned unchanged and `f` is not called.
- `mapError(f)` applies `f` to the `Err` payload and wraps the result in `Err`; an `Ok` is returned unchanged.
- `andThen(f)` applies `f` to the `Ok` payload and returns its `Result` as the result; an `Err` is returned unchanged.

Each of these has cost 1 plus the cost of the callback ([section 11.3](11-resource-limits.md#113-steps)).

```hollow
constant good: Result<Int, String> = success(4)
constant bad: Result<Int, String> = failure("no")
constant doubled = good.map(fn(n) {
    return n * 2
})
return [doubled.unwrapOr(0) == 8, bad.unwrapOr(7) == 7, bad.isErr(), good.ok() == 4, bad.err() == "no"]
```

The program returns `[true, true, true, true, true]`: `doubled` is `Ok(8)`, `bad.unwrapOr(7)` is 7, and the optional payloads compare equal to 4 and `"no"`.

## 8.5 Discarded results

Ignoring a `Result` silently is almost always a bug, so an expression statement whose value has a `Result` type and that is not followed by `?` is warned about (HS0803). To acknowledge a result that is deliberately ignored, match on it or bind it to a name that starts with an underscore:

```hollow-fragment
uses fs

constant _outcome = effect fs.write("out.txt", "data")
```
