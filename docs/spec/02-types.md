# 2. Types

HollowScript is statically typed. Every expression has a type that is known before the program runs ([section 2.11](#211-when-types-are-checked)). There is no `any` type, no implicit conversion other than the numeric one in [section 2.7](#27-assignability-and-numeric-coercion), and no runtime type inspection.

## 2.1 Namespaces

Names live in two namespaces that never conflict with each other:

- The **type namespace** holds type names: the built-in types, the prelude types, the enum types that a capability interface declares ([section 9.7](09-effects-and-capabilities.md#97-the-capability-interface-of-a-run)), names declared with `record` and `enum`, and type parameters.
- The **value namespace** holds constants, variables, functions, parameters, capability names, imported names, enum names (as the namespace of their variants, [section 6.3](06-data-types.md#63-enums)) and the prelude values ([Chapter 13](13-stdlib-core.md) onwards).

A record name is only in the type namespace, and using it where a value is expected is HS0209. An enum name is in both. A type name that is not declared anywhere is HS0208. A declaration of a name that is already visible in the same namespace is an error (HS0201), as specified in [section 4.6](04-statements.md#46-scope-and-shadowing). The rule applies to type names as well: a `record` or `enum` name, an imported type name and a type parameter may not have the name of a type that is already visible, whether declared, imported or a type parameter in scope. So a second `record Point`, a `record Foo` beside an `enum Foo` (an enum name is also a type name) and `fn f<Int>(x: Int)` are all HS0201; a repeated name inside one list of type parameters is HS0212.

The **prelude** is the set of names that every file sees without importing them. It is defined by this specification and is versioned with it. Its type names are `Int`, `Float`, `Bool`, `String`, `Filter`, `Result`, `Map`, `Set`, `MapEntry`, `Json`, `JsonError` and `TimeParts`. Its value names are the functions, constants and namespaces listed in [section 13.6](13-stdlib-core.md#136-the-prelude). Unlike every other name, a prelude name may be redeclared by a program and hidden, as [section 4.6](04-statements.md#46-scope-and-shadowing) states, with one restriction: `Int`, `Float`, `Bool`, `String`, `Filter`, `Result`, `Map`, `Set` and `MapEntry` are always the built-in types, and declaring a type of one of those names is HS0201. The error types of the standard capabilities, `IoError` and `HttpError`, are not prelude names: a run's capability interface declares them ([section 9.7](09-effects-and-capabilities.md#97-the-capability-interface-of-a-run), [section 16.1](16-standard-capabilities.md#161-shared-types)).

## 2.2 The types

| Type | Written | Values |
|---|---|---|
| Integer | `Int` | 64-bit two's complement signed integers |
| Floating point | `Float` | IEEE 754 binary64 values that are finite; there is no NaN, no infinity and no negative zero |
| Boolean | `Bool` | `true` and `false` |
| Text | `String` | a sequence of Unicode scalar values (code points other than surrogates), stored as UTF-8 and measured in code points ([section 13.3](13-stdlib-core.md#133-strings)) |
| Filter | `Filter` | an opaque, validated filter expression ([Appendix A](appendix-a-filters.md)) |
| List | `[T]` | a finite, ordered, immutable sequence of `T` |
| Map | `Map<K, V>` | a finite, immutable association from keys of type `K` to values of type `V`, ordered by key |
| Set | `Set<T>` | a finite, immutable set of `T`, ordered by element |
| Optional | `T?` | either a `T` or `none` |
| Record | `{ a: A, b: B }` or a name declared by `record` | a fixed set of named fields |
| Enum | a name declared by `enum`, and `Result<T, E>` | one of a fixed set of variants, each with zero or more payload values |
| Function | `fn(A, B): C` and `effect fn(A, B): C` | a function or closure |

There is no separate unit, void, tuple, union, class or `any` type. A function that returns nothing has no return type; a call to it is not a value ([section 5.1](05-functions-and-closures.md#51-function-declarations)).

`none` is the single value of the absent case of every optional type. It has no type of its own: an expression `none` can only appear where an optional type is expected or is being inferred ([section 2.9](#29-inference)).

All values are immutable ([section 4.2](04-statements.md#42-declarations)). Aliasing is therefore unobservable, and implementations may share or copy values freely.

## 2.3 Type syntax

```
type      = typeAtom [ "?" ] ;
typeAtom  = "[" type "]"
          | "(" type ")"
          | fnType
          | recordType
          | namedType ;
fnType    = [ "effect" ] "fn" "(" [ typeList ] ")" [ ":" type ] ;
recordType = "{" field { "," field } [ "," ] "}" ;
field     = identifier ":" type ;
namedType = identifier [ "<" typeList ">" ] ;
typeList  = type { "," type } [ "," ] ;
```

- A `?` suffix makes a type optional. It is written at most once: `Int??`, `[Int]??` and `(Int?)?` are HS0111. `[Int]?` is an optional list of `Int`; `[Int?]` is a list of optional `Int`.
- A function type without `: type` returns nothing. In `fn(Int): Int?` the `?` belongs to the return type, and `(fn(Int): Int)?` is an optional function.
- A type argument given to a type that takes none (`Int<String>`) is HS0302. Generic types are written with angle brackets only in type positions. `<` in an expression is always a comparison ([section 2.10](#210-generics)).
- A record type needs at least one field (HS0112) and its field names must be distinct (HS0202) and not `_` (HS0123).
- A `>` that closes a type argument list must be a `>` token: the lexer takes the longest match ([section 1.2](01-lexical-structure.md#12-tokens-whitespace-and-comments)) and `>=` is never split, so `Set<Int>= x` is HS0101. Write a space before the `=`.

```hollow-error
constant s: Set<Int>= set([1]) // HS0101
return 1
```

## 2.4 Optional types

`T?` contains every value of `T` and `none`. Optionality does not nest: for any type `T`, `T?` applied to a type that is already optional is that same type. Writing the nesting is a syntax error (HS0111), but the same collapse happens when a type is computed by substitution, for example when a generic `T?` is instantiated with `T` equal to `Int?`. The result is `Int?`. This means `first()` of a `[Int?]` yields `Int?`, and a script cannot distinguish an absent element from a present element whose value is `none`; a script that must distinguish them uses a list of records or an enum.

An optional value is used as its inner type only after it has been narrowed ([section 4.8](04-statements.md#48-optional-narrowing)) or matched ([Chapter 7](07-patterns.md)).

## 2.5 Record types

A record type is a set of fields, each with a name and a type. Record types are **structural**: two record types are the same type if and only if they have exactly the same set of field names and, for each name, identical field types. Field order is irrelevant. There is no constructor call, no tag at the literal site and no subtyping: a value with extra fields is not accepted where fewer are expected, and a value with fewer is not accepted where more are expected.

A `record` declaration gives a name to a record type. The name is an alias: `record Point = { x: Int, y: Int }` makes `Point` and `{ x: Int, y: Int }` the same type.

```hollow
record Point = { x: Int, y: Int }
record Vector = { x: Int, y: Int }

fn length2(p: Point): Int {
    return p.x * p.x + p.y * p.y
}

constant v: Vector = { x: 3, y: 4 }
return length2(v)
```

The program returns `25`, because `Vector` and `Point` are the same type.

A record type may not contain itself, directly or through the fields, list elements, map values, set elements, optional inner types, function types or type arguments of records and enums: this is HS0305. The trace stops at an enum: the payloads of an enum are not followed, so a cycle that passes through an enum payload is allowed, while an enum that is used as a type argument inside the record is part of the record's own type and is followed. Recursive data is written with an `enum` ([section 6.3](06-data-types.md#63-enums)).

```hollow
record Branch = { tree: Tree }

enum Tree {
    Leaf,
    Node(Branch),
}

constant t = Tree.Node({ tree: Tree.Leaf })
return t == Tree.Node({ tree: Tree.Leaf })
```

The program returns `true`; `record Node = { next: Maybe<Node> }` would be HS0305, because `Node` occurs in its own type arguments.

Types are represented so that identity is a comparison of the same node, not a walk of the whole type: the *expanded size* of a type, which counts every position of the type after all aliases and type arguments are substituted, with sharing ignored and saturating, may not exceed 10,000 nodes (HS0335). This stops `record P<A> = { l: A, r: A }` applied to itself many times from denoting a type of astronomical size. A type in a diagnostic message is cut after 200 characters, with an ellipsis.

A record declaration may have type parameters: `record Pair<A, B> = { left: A, right: B }`. `Pair<Int, String>` is the alias instantiated with those arguments. A record that is generic and refers to itself is HS0305 like any other.

## 2.6 Function types and effect

A function type lists its parameter types and an optional return type: `fn(Int, String): Bool`. Parameters have types only, never names.

The prefix `effect` makes the type *effectful*: `effect fn(String)`. An effectful function type is the type of a capability function and of anything that may reach a capability through the type system. The rules are in [Chapter 9](09-effects-and-capabilities.md). In summary: a call through a value of effectful type must be marked with `effect`; a pure function may be used where an effectful function type is expected, but not the reverse.

## 2.7 Assignability and numeric coercion

A value of type `S` is *assignable* to a target type `T`, written `S <: T`, if any of these hold:

1. `S` and `T` are identical types.
2. `S` is the type of `none` and `T` is an optional type.
3. `T` is `U?` and `S <: U`. (So `Int` is assignable to `Int?`.)
4. `S` and `T` are function types that are identical except that `S` is pure and `T` is effectful.

Nothing else is assignable. In particular lists, maps, sets, records and function types are *invariant*: `[Int]` is not assignable to `[Float]`, `[Int]` is not assignable to `[Int?]`, and `fn(Int): Int` is not assignable to `fn(Float): Float`.

**Numeric coercion.** An expression of type `Int` may appear where the expected type is `Float` or `Float?`. The value is converted to the `Float` nearest to it (ties to even significand) at that point. The places where an expected type is known, and where this conversion therefore applies, are the *coercion sites*:

- the initialiser of a `constant` or `variable` declaration that has a type annotation;
- the right-hand side of an assignment to a `variable` whose type is `Float` or `Float?`;
- an argument to any call whose corresponding parameter type is `Float` or `Float?`, including calls of user functions, closures, standard library functions, capability functions, enum variant constructors and calls made by a host into an exported function ([section 12.4](12-instances.md#124-calling-exported-functions));
- the operand of `return` in a function or closure whose return type is `Float` or `Float?`;
- the top level of a value that a host hands to the language, for a `Float` or `Float?` position ([section 9.6](09-effects-and-capabilities.md#96-capability-values) and [section 12.4](12-instances.md#124-calling-exported-functions));
- an element of a list literal, a key or value of a map literal, an element of a set built by `set`, and a field of a record literal, when the literal's expected type has `Float` or `Float?` at that position ([section 2.9](#29-inference));
- an operand of `+`, `-`, `*` or `/` when the other operand is a `Float` ([section 3.4](03-expressions.md#34-arithmetic));
- an `Int` operand that a join of `Int` and `Float` turns into a `Float` ([section 2.9](#29-inference)): an element of a list literal, a key or value of a map literal, and an operand of `return` in a closure whose return type is fixed by the join. The conversion is applied to the expression that produced the operand once the join is known, never to a value that is already stored.

Comparisons are not coercion sites: an `Int` and a `Float` are compared by their exact values, optional or not, without converting either ([section 3.5](03-expressions.md#35-comparison-and-equality)).
Coercion is applied to the expression that appears at the site and never through a type constructor: an existing `[Int]` value is not converted to `[Float]`. If a type parameter is inferred as `Int` from one argument, a `Float` argument for the same parameter is an error (HS0301); coercion does not change an inferred type parameter.

A `Float` is never converted to an `Int` implicitly. `floor`, `ceil`, `round` and `trunc` ([section 13.2](13-stdlib-core.md#132-numbers)) convert explicitly.

## 2.8 Equatable and orderable types

A type is **equatable** if `==` and `!=` are defined on it: `Int`, `Float`, `Bool`, `String`, and lists, maps, sets, optionals, records and enums whose component types are all equatable. `Filter`, function types and type parameters are not equatable, and neither is any type that contains one.

A type is **orderable** if `<`, `>`, `<=` and `>=` are defined on it: `Int`, `Float` and `String`. `Int` and `Float` are orderable against each other.

A type is a **key type** if it can be the key of a `Map` or the element of a `Set`: `Int`, `String` and `Bool`. Using another type is HS0306 (map key) or HS0307 (set element). `Float` is excluded deliberately, so that key equality never involves a rounding question.

The meaning of equality and ordering is in [section 3.5](03-expressions.md#35-comparison-and-equality).

## 2.9 Inference

A declared type is required on the parameters of a `fn` declaration, on the return type of a `fn` declaration that returns a value, and on the fields of records and the payloads of enum variants. Everywhere else the type is inferred, in a single pass that propagates an *expected type* downwards into an expression when one is known and otherwise computes the type of the expression bottom-up.

**Declarations.** `constant x = e` and `variable x = e` give `x` the type of `e` when there is no annotation. A declaration whose initialiser has no inferable type, such as `constant x = []`, `constant x = [:]` or `constant x = none`, is HS0304 and needs an annotation. A `variable` annotated or inferred `T` keeps that type; assignments must be assignable to it.

**Literals.**

- An `Int` literal has type `Int`; a `Float` literal has type `Float`; `true` and `false` have type `Bool`; a string literal has type `String`; a filter literal has type `Filter`.
- `none` has the expected optional type. With no expected optional type it is HS0304.
- A list literal `[e1, e2]` has an expected element type when its expected type is `[T]`, and each element is checked against `T` at a coercion site. Without an expected type, the element type is the *join* of the element types, which is defined over the whole list at once so that it does not depend on the order of the elements:
  1. the `none` elements are set aside, and it is noted whether there were any;
  2. the types of the remaining elements, where an optional type counts as its inner type, must be all identical, or all `Int` or `Float` with at least one of each (the join is `Float`), or all function types that differ only in effect (the join is the effectful one);
  3. if a `none` element or an optional element type was present, the join is made optional.

  If step 2 fails, HS0301 is reported at the first element whose type does not join with the join of the elements before it, ignoring `none`. So `[1, none, 2.5]` and `[1, 2.5, none]` are both `[Float?]`, and `[1, "a"]` is HS0301 at `"a"`. A list of only `none` elements and an empty list need an expected type (HS0304). Where the join is `Float`, the `Int` elements are converted ([section 2.7](#27-assignability-and-numeric-coercion)).
- A map literal `[k1: v1, k2: v2]` is treated the same way, the join of all the keys giving the key type and the join of all the values the value type, and `[:]` is the empty map and needs an expected type.
- A record literal with an expected record type has each field checked against the field's type; the set of names must match exactly (HS0326 for a missing field, HS0327 for a field the type does not have). Without an expected record type, its type is the record type whose fields have the types of the field expressions.
- A closure literal is inferred as described in [section 5.3](05-functions-and-closures.md#53-closures).

```hollow
constant mixed = [1, none, 2.5]
constant first: Float? = mixed[0]
return [mixed.length() == 3, first == 1.0]
```

The program returns `[true, true]`: `mixed` has type `[Float?]` with three elements, and its first element is the `Float` 1.0.

**Expected types.** An expected type is known for: the initialiser of an annotated declaration, the right-hand side of an assignment, an argument (from the parameter type after generic instantiation, [section 2.10](#210-generics)), the operand of `return`, and the elements and fields of a literal with an expected type. The operand of `?` has no expected type ([section 8.3](08-errors-and-results.md#83-the--operator)).

## 2.10 Generics

A `fn`, `record` or `enum` declaration may declare type parameters in angle brackets after its name. The parameter names must be distinct (HS0212). A type parameter is a type name visible in the declaration's signature and, for a `fn`, in its whole body, where it can be used in any type position: an annotation on a local, a closure parameter, a closure return type.

```hollow
fn first<T>(items: [T]): T? {
    if items.isEmpty() {
        return none
    }
    return items[0]
}

record Pair<A, B> = { left: A, right: B }

fn swap<A, B>(p: Pair<A, B>): Pair<B, A> {
    return { left: p.right, right: p.left }
}

constant a = first([10, 20])
constant b = first(["x"])
constant empty: [Int] = []
constant c = first(empty)
constant swapped = swap({ left: 1, right: "one" })
return [a == 10, b == "x", c == none, swapped.left == "one"]
```

The program returns `[true, true, true, true]`.

**Values of a type parameter are opaque.** Inside the declaration, a value whose type is a type parameter can be bound, passed, returned, stored in lists, maps, sets and records, and given to other generic code. It is not equatable, not orderable, not interpolatable and not a key type, because nothing is known about it. `first` above works for every `T`; a generic function that needs to compare its elements takes a comparison closure as a parameter instead. There are no constraints or bounds in version 1.

**Type arguments are never written at a call site.** They are inferred. Angle brackets appear only in type positions, so in an expression `<` is always the comparison operator.

**Inference of a call.** For a call to a generic function, a generic enum variant constructor, or a generic standard library function, the checker:

1. Instantiates each type parameter as a fresh, unbound *type variable*.
2. Checks the receiver (for a method) and then the arguments that are not closure literals, left to right, each against its parameter type under the bindings so far. Checking an argument of type `A` against a parameter type `P` *unifies* `P` with `A`: a type variable in `P` that is unbound becomes bound to the corresponding part of `A`; a variable that is already bound must be identical to that part (HS0301 otherwise); the remaining structure of `P` and `A` must be assignable by [section 2.7](#27-assignability-and-numeric-coercion). Where `P` is `V?` and `V` is an unbound variable, `A` may be `X`, `X?`, and binds `V` to `X`. A type variable bound to `Int` does not accept a `Float`.
3. If any variable is still unbound and the call has an expected type ([section 2.9](#29-inference)), unifies the return type with the expected type, binding only variables that are still unbound.
4. Checks each closure-literal argument, left to right, against its parameter type under the bindings so far. A closure parameter that has no annotation takes its type from the parameter type of the function type it is checked against; that type must by now contain no unbound variable, or the closure is HS0309. The closure body's return type is unified with the return type of that function type, which may bind further variables ([section 5.3](05-functions-and-closures.md#53-closures)).
5. Reports HS0308 for any variable that is still unbound.

Because the checker resolves arguments before closures, `numbers.map(fn(n) { return n * 2 })` needs no annotation: `T` is bound from `numbers`, and `U` from the closure's `return`.

Unification is standard first-order unification with an occurs check. Two unbound variables unify by linking, and a variable unifies with any type that does not contain it. An expected type that contains unbound variables of an enclosing call (the expected type of a call in the body of a closure passed to `map`, whose return type `U` is not yet known) is passed down as it is: unifying the return type of the inner call with it binds the variable of the enclosing call to a type that contains the inner call's own unbound variables, which step 4 then binds. A variable that is still unbound when the outermost call has been checked is HS0308, and so is a variable that only an empty list or map literal argument could have bound, as in `set([])`; it is not HS0304.

```hollow
constant xs = [1, 2]
constant ys = ["a", "b"]
constant grid = xs.map(fn(x) {
    return ys.map(fn(y) {
        return "${x}${y}"
    })
})
return grid.flatMap(fn(row) {
    return row
}).join(",")
```

The program returns `"1a,1b,2a,2b"`.

```hollow-error
constant s = set([]) // HS0308
return s
```

A generic function name used as a value, without being called, has all its type variables unbound and is HS0308 unless there is an expected function type that binds them (`constant f: fn([Int]): Int? = first` is valid).

A generic function or method that calls itself sees its own type parameters as ordinary opaque types inside its body and instantiates fresh variables for the recursive call.

## 2.11 When types are checked

A file and everything it imports is checked in full before any of its statements run. The checker reports, together: lexical and syntax errors, unknown names, type errors, effect errors, pattern errors, module errors, ungranted capabilities ([section 9.2](09-effects-and-capabilities.md#92-declaring-and-granting)), and errors inside filter literals ([Appendix A](appendix-a-filters.md)). A file with any error diagnostic does not run. There is no partial or lazy checking, and no way to run a file that has a type error in a branch that would not be taken.

**Error recovery.** When an expression is malformed or has no valid type, the checker gives it the internal *error type*. The error type is assignable to and from every type and every operation on it is accepted without a diagnostic, so one mistake produces one diagnostic instead of a cascade. The error type never appears in a program that is accepted. A `constant` or `variable` whose initialiser is in error has the annotated type if it has an annotation and the error type otherwise; a call that is in error has the error type; a name that could not be resolved has the error type.
