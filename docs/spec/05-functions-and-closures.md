# 5. Functions and closures

## 5.1 Function declarations

```
fnDecl     = "fn" identifier [ "<" typeParams ">" ]
             "(" [ params ] ")" [ ":" type ] block ;
params     = param { "," param } [ "," ] ;
param      = identifier ":" type ;
typeParams = identifier { "," identifier } [ "," ] ;
```

A function declaration appears only at the top level of a file and may be prefixed with `export` ([Chapter 10](10-modules.md)). It gives a name to a function whose parameters and return type are written out:

- Every parameter has a declared type. Parameter names must be distinct (HS0201).
- If the function returns a value, its return type is declared after the parameter list. If the return type is omitted, the function returns nothing, and a call to it can only be used as a statement ([section 4.9](04-statements.md#49-expression-statements)).
- Type parameters, if any, follow the name ([section 2.10](02-types.md#210-generics)).
- Parameters are constants and cannot be assigned (HS0206).
- A function that declares a return type must return a value on every path ([section 4.7](04-statements.md#47-return-break-continue-and-reachability)).

```hollow
uses print

fn add(a: Int, b: Int): Int {
    return a + b
}

fn announce(text: String) {
    effect print(text)
}

announce("sum is ${add(2, 3)}")
```

Output:

```text
sum is 5
```

A function declaration is visible throughout its file, so functions may call each other regardless of order, and a function may call itself ([section 4.10](04-statements.md#410-top-level-declaration-order)). The name of a function used without a call is a value of a function type built from its signature; that type is pure ([section 5.5](#55-function-types-and-effect)).

## 5.2 Calls

A call `f(a, b)` evaluates the callee, then the arguments left to right ([section 3.2](03-expressions.md#32-order-of-evaluation)), then transfers control to the function with each parameter bound to the corresponding argument. Arguments are passed by value; because all values are immutable, there is no observable difference between passing a value and passing a reference.

- The number of arguments must equal the number of parameters (HS0319).
- Each argument is checked against its parameter type and is a coercion site ([section 2.7](02-types.md#27-assignability-and-numeric-coercion)).
- There are no default arguments, named arguments, optional arguments or variadic functions in user code.
- The value of the call is the value of the `return` that ends it, or nothing.
- Each active call is counted against the call depth limit ([Chapter 11](11-resource-limits.md)). There is no guarantee of tail-call elimination: a tail call uses a frame like any other call.

## 5.3 Closures

```
closure = "fn" "(" [ closureParams ] ")" [ ":" type ] block ;
closureParams = closureParam { "," closureParam } [ "," ] ;
closureParam  = identifier [ ":" type ] ;
```

A closure is an anonymous function used as a value, with the same syntax as a declaration minus the name and type parameters. It is created each time its expression is evaluated.

**Parameter types.** A closure parameter may omit its annotation when the closure appears where an expected function type is known: as an argument whose parameter type is a function type, in an annotated declaration, or as an operand of `return` in a function whose return type is a function type. Then the parameter takes the type at the same position in the expected function type; if there is also an annotation, it must be identical to that type (HS0301). A parameter with no annotation and no expected function type, or whose expected type still contains an unbound type variable ([section 2.10](02-types.md#210-generics)), is HS0309. The number of parameters must equal the number in the expected function type (HS0319).

**Return type.** The return type of a closure is decided as follows:

1. If the closure has a `: type` annotation, that is its return type.
2. Otherwise, if the expected function type has a return type, that is the closure's return type, and every `return` operand is a coercion site for it.
3. Otherwise, if the expected function type has no return type (for example the parameter of `each`), the closure returns nothing and `return e` in it is HS0340.
4. Otherwise (no expected function type), the closure's return type is the join ([section 2.9](02-types.md#29-inference), taken over all the operands at once) of the types of its `return` operands, or none if it has no `return e`. A closure that mixes `return e` and a bare `return` is HS0341.

A closure that has a return type must return on every path (HS0342). If the expected return type is an unbound type variable (as `U` in `map`), the join of the `return` operands binds it ([section 2.10](02-types.md#210-generics)).

**Type.** The type of a closure is `fn(P1, P2): R` with `R` omitted if it returns nothing. It is pure. Because a pure function type is assignable to the corresponding effectful one ([section 2.7](02-types.md#27-assignability-and-numeric-coercion)), a closure may be passed where an `effect fn(...)` is expected.

```hollow
record Person = { name: String, age: Int }

constant people: [Person] = [
    { name: "Priya", age: 34 },
    { name: "Alex", age: 29 },
]

constant doubled = [1, 2, 3].map(fn(n) {
    return n * 2
})

constant widened = [1, 2].map(fn(n): Float {
    return n
})

constant byAge = people.sort(fn(a, b) {
    return a.age - b.age
})

constant names = byAge.map(fn(p) {
    return p.name
})

return [doubled[2] == 6, widened[0] == 1.0, names.first() == "Alex"]
```

The program returns `[true, true, true]`.

## 5.4 Capture

A closure sees the bindings of the scopes that enclose it, and it sees them by reference: the closure and the enclosing code share one binding, not a copy of its value at the time the closure was created. A closure that reads a `variable` sees its current value when the closure runs, and a closure that assigns a `variable` ([section 4.3](04-statements.md#43-assignment)) changes what the enclosing code and every other closure over that binding see. There is no capture mode and no `move`, because there is no concurrency ([section 12.5](12-instances.md#125-concurrency)).

A binding lives as long as anything can still reach it: a closure returned from a function keeps the bindings it captured alive after the function has returned.

**Fresh bindings.** Each execution of a declaration creates a new binding. In particular the loop variable of a `for` loop is a new `constant` in each iteration, and a `constant` or `variable` declared inside a loop body is new in each iteration, so closures created in different iterations capture different bindings.

```hollow
fn makeCounter(): fn(): Int {
    variable n = 0
    return fn(): Int {
        n = n + 1
        return n
    }
}

constant next = makeCounter()
constant first = next()
constant second = next()
constant third = next()
return [first, second, third]
```

The program returns `[1, 2, 3]`.

```hollow
variable makers: [fn(): Int] = []
for i in range(0, 3) {
    makers = makers.append(fn(): Int {
        return i * 10
    })
}

variable results: [Int] = []
for make in makers {
    results = results.append(make())
}
return results
```

The program returns `[0, 10, 20]`, because each iteration has its own `i`.

A closure cannot refer to the name it is being bound to, because the name is not in scope until after the declaration ([section 4.2](04-statements.md#42-declarations)); a recursive local function is written as a top-level `fn`.

## 5.5 Function types and effect

A function type `fn(A, B): R` describes any function or closure with those parameter types and that return type. `effect fn(A, B): R` describes one that is *effectful*: a call through a value of this type must be marked `effect`. Capability functions have effectful types. The full rules are in [Chapter 9](09-effects-and-capabilities.md); the parts that concern functions are:

- A `fn` declaration and a closure literal have pure types, however their bodies are written. A function may perform effects internally by calling capabilities with `effect`; calling that function needs no marker.
- A value of a pure function type is assignable to the same function type with `effect`; the reverse is a type error. So a higher-order function that wants to accept `print` declares its parameter as `effect fn(String)`, and a library function such as `map` that declares a pure parameter refuses `print`.
- A call through a value of effectful type needs the `effect` marker, whether the value is a capability itself, a parameter, a record field or a local alias.

```hollow
uses print

fn apply(f: effect fn(String), text: String) {
    effect f(text)
}

apply(print, "hello")
apply(fn(s) {
    effect print("closure says ${s}")
}, "hi")
```

Output:

```text
hello
closure says hi
```

## 5.6 Recursion

A function may call itself directly or through other functions. The depth of active calls is bounded by the call depth limit ([section 11.5](11-resource-limits.md#115-call-depth)); exceeding it faults with HS1005. Recursion that is not bounded by the program's own logic will therefore always end in a fault or, earlier, a step limit fault, and never in a crash of the host.

```hollow
fn factorial(n: Int): Int {
    if n <= 1 {
        return 1
    }
    return n * factorial(n - 1)
}

return factorial(10)
```

The program returns `3628800`.
