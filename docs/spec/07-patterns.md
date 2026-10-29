# 7. Pattern matching and destructuring

A *pattern* describes the shape of a value and names parts of it. Patterns appear in three places: the arms of a `match` statement ([section 7.5](#75-the-match-statement)), the target of a `constant` or `variable` declaration, and the variable of a `for` loop ([section 7.8](#78-destructuring-declarations-and-for)). There are no guards and no `if let`.

## 7.1 Pattern forms

```
pattern         = literalPattern
                | "none"
                | "_"
                | bindingName
                | variantPattern
                | recordPattern
                | listPattern ;
literalPattern  = [ "-" ] intLiteral | [ "-" ] floatLiteral
                | stringLiteral | "true" | "false" ;
variantPattern  = variantName [ "(" pattern { "," pattern } [ "," ] ")" ] ;
recordPattern   = "{" fieldPattern { "," fieldPattern } [ "," ] "}" ;
fieldPattern    = identifier [ ":" pattern ] ;
listPattern     = "[" [ listElement { "," listElement } [ "," ] ] "]" ;
listElement     = pattern | restPattern ;
restPattern     = ".." [ bindingName ] ;
```

| Pattern | Matches |
|---|---|
| `3`, `-1`, `2.5`, `"open"`, `true` | a value equal to the literal |
| `none` | the absent case of an optional |
| `_` | anything, binds nothing |
| `name` | anything, binds it to `name` |
| `Circle` or `Rect(w, h)` | a value of that enum variant whose payload matches the sub-patterns, position by position |
| `{ x, y: 0 }` | a record whose listed fields match; `x` alone means `x: x` |
| `[]`, `[a, b]`, `[first, ..rest]` | a list of exactly that length, or (with a rest pattern) at least that length |

A string literal pattern cannot contain an interpolation (HS0101). A list pattern may contain a rest pattern only as its last element (HS0121). `..name` binds the remaining elements as a list of the element type; `..` on its own ignores them.

## 7.2 Variants and bindings are told apart by their first letter

A pattern that is an identifier is a *variant pattern* if its first character is an upper-case ASCII letter, and a *binding* otherwise. Binding names must start with a lower-case ASCII letter or `_` (HS0120 otherwise). This lets a pattern be read without knowing the type it is matched against.

A variant pattern is written without the enum name, because the type of the scrutinee tells which enum is meant (`Circle(r)` and not `Shape.Circle(r)`). Naming a variant that the scrutinee's enum does not have is HS0505; giving the wrong number of sub-patterns is HS0506. A variant pattern for a scrutinee that is not an enum is HS0504.

In a record pattern a field written on its own, `{ Name }`, is short for `Name: Name`, whose binding must obey this rule, so it is HS0120; write `{ Name: name }`. A field name may start with any letter or `_`, except that it cannot be `_` itself (HS0123).

## 7.3 Bindings and the discard name

A binding pattern `name` declares a constant `name` that is visible in the arm's block (or, for a declaration, in the rest of the scope). Its type is the type of the part of the value it matches. A name may be bound at most once in one pattern (HS0204), and it may not be a name that is already visible (HS0201), as for any declaration ([section 4.6](04-statements.md#46-scope-and-shadowing)).

`_` matches anything and binds nothing. It may appear any number of times in a pattern, a parameter list or a `for` loop, and it may never be read: using `_` as an expression is HS0213. A binding whose name begins with `_` (such as `_unused`) binds normally and is exempt from the unused-name warning.

## 7.4 Typing patterns

A pattern is checked against the type of the value it is matched against, the *scrutinee type* `S`.

- **Literal.** `S` must be `Int` and the literal an integer literal, or `S` must be `Float` and the literal a float literal, or `S` `String` with a string literal, or `S` `Bool` with `true` or `false` (HS0504 otherwise). An `Int` literal does not match a `Float` scrutinee. In general, a pattern form that cannot match `S` at all (a variant pattern for a non-enum, a record pattern for a non-record, a list pattern for a non-list) is HS0504.
- **`none`.** `S` must be an optional type (HS0504).
- **Binding and `_`.** Any `S`; the binding has type `S`.
- **Variant.** `S` must be an enum type with that variant; sub-patterns are checked against the payload types after substituting the enum's type arguments.
- **Record.** `S` must be a record type; each listed field must exist (HS0507) and its pattern is checked against the field's type. Fields that are not listed are ignored.
- **List.** `S` must be `[T]`; element patterns are checked against `T`, and `..name` has type `[T]`.
- **Optional scrutinee.** If `S` is `U?`, a pattern other than `none`, a binding or `_` is checked against `U` and matches only present values. A binding or `_` matches present and absent values and, unless the type is narrowed as described in [section 7.6](#76-narrowing-in-match), a binding has type `U?`.

A value of type `Filter` or of a function type cannot be matched, because such values have no structure to match and no equality (HS0510). A literal pattern requires its type to be equatable, which every literal type is.

## 7.5 The `match` statement

```
matchStmt = "match" expression "{" matchArm { matchArm } [ elseArm ] "}"
          | "match" expression "{" elseArm "}" ;
matchArm  = pattern { "," pattern } block ;
elseArm   = "else" block ;
```

`match` evaluates the scrutinee once, then tests the arms from first to last; the block of the first arm that has a matching pattern runs, and no other arm runs. There is no fall-through. An arm with several comma-separated patterns (its *alternatives*) matches if any alternative matches. Because a name bound by one alternative might not exist in another, alternatives may not bind names (HS0508); they may contain `_` and literals.

`else` matches anything, is the same as a `_` arm, and must be the last arm: an arm after `else` is HS0101 (expected `}`). It is optional when the arms already cover every value.

**Exhaustiveness.** Every `match` must cover every possible value of the scrutinee's type; otherwise it is HS0501. Whether it does is decided by the algorithm in [section 7.7](#77-exhaustiveness-and-unreachable-arms). In practice: a match on an enum needs an arm for every variant (or `else`); on a `Bool` needs `true` and `false` (or `else`); on any type with an unbounded set of values (`Int`, `Float`, `String`, lists, and so on) needs an arm that matches everything (`else`, `_` or a binding).

**Unreachable arms.** An alternative that can never match because earlier arms already match everything it could match is HS0503, reported on that alternative's pattern ([section 7.7](#77-exhaustiveness-and-unreachable-arms)). A literal that appears twice in one match is HS0502 on its second appearance. `else` after arms that already cover every case is HS0503.

**Scope.** Names bound by an arm's patterns are visible in that arm's block.

```hollow
fn describe(n: Int): String {
    match n {
        0 {
            return "zero"
        }
        1, -1 {
            return "one, either sign"
        }
        else {
            return "something else"
        }
    }
}

fn ownerLine(owner: String?): String {
    match owner {
        none {
            return "unowned"
        }
        "root" {
            return "owned by the administrator"
        }
        name {
            return "owned by ${name}"
        }
    }
}

return [describe(-1), ownerLine(none), ownerLine("root"), ownerLine("ada")]
```

The program returns `["one, either sign", "unowned", "owned by the administrator", "owned by ada"]`.

```hollow
fn describe(xs: [Int]): String {
    match xs {
        [] {
            return "empty"
        }
        [only] {
            return "one: ${only}"
        }
        [first, ..rest] {
            return "${first} then ${rest.length()} more"
        }
    }
}

record Point = { x: Int, y: Int }

fn where(p: Point): String {
    match p {
        { x: 0, y: 0 } {
            return "origin"
        }
        { x: 0 } {
            return "on the y axis"
        }
        { y: 0 } {
            return "on the x axis"
        }
        { x, y } {
            return "at ${x}, ${y}"
        }
    }
}

return [describe([]), describe([7]), describe([1, 2, 3]), where({ x: 0, y: 5 }), where({ x: 2, y: 3 })]
```

The program returns `["empty", "one: 7", "1 then 2 more", "on the y axis", "at 2, 3"]`.

The following are errors:

```hollow-error
enum Light {
    Red,
    Green,
}

fn go(l: Light): Bool {
    match l { // HS0501
        Red {
            return false
        }
    }
}

return go(Light.Green)
```

```hollow-error
constant n = 1
match n {
    1 {
        return 1
    }
    1 { // HS0502
        return 2
    }
    else {
        return 0
    }
}
```

```hollow-error
constant n = 1
match n {
    x {
        return x
    }
    1 { // HS0503
        return 1
    }
}
```

```hollow-error
constant n = 5
match n {
    "five" { // HS0504
        return 1
    }
    else {
        return 0
    }
}
```

## 7.6 Narrowing in `match`

If the scrutinee is a path ([section 4.8](04-statements.md#48-optional-narrowing)) of optional type `U?` and some arm has `none` as a pattern or as one of its alternatives, then in every arm that does not have `none` among its patterns:

- the path is narrowed to `U` in the arm's block, subject to the boundary and shared-mutable rules of [section 4.8](04-statements.md#48-optional-narrowing); and
- a binding pattern at the top level of the arm has type `U`.

After the `match` statement, the facts are the intersection of the facts at the end of every arm that can complete normally, as for `if` ([section 4.8](04-statements.md#48-optional-narrowing)); a `match` all of whose arms cannot complete normally leaves the incoming facts.

Since a binding or `_` before a `none` arm would match `none` first, it makes the `none` arm unreachable (HS0503); so a top-level binding that has type `U` is always reached after `none` has been ruled out.

## 7.7 Exhaustiveness and unreachable arms

Whether a `match` is exhaustive, and whether an arm is unreachable, is defined by the following algorithm. An implementation may use any method that gives the same answers.

Expand each arm into one *row* per alternative, in order; `else` is a row with `_`. A row is a sequence of patterns, initially of length 1. The notation `useful(P, q)` means that there is a value that the vector `q` matches and that no row of the matrix `P` matches.

- A `match` is **exhaustive** if and only if `useful(P, [_])` is false, where `P` is the matrix of all its rows.
- Row *i* is **unreachable** if and only if `useful(P_i, row_i)` is false, where `P_i` is the matrix of rows 1 to *i* - 1.

Every pattern in a column is one of: a *constructor pattern* (a literal, `none`, a variant, a record pattern, a list pattern) or a *wildcard* (`_` or a binding). The **constructors** of a type are:

- an enum: its variants, each with arity equal to its number of payload types;
- `Bool`: `true` and `false`, arity 0;
- an optional `U?`: `none` (arity 0) and `some` (arity 1, standing for a present value; any constructor pattern for `U` other than `none` is treated as `some` applied to that pattern);
- a record type: a single constructor with one argument per field of the type, in ascending order of field name; a record pattern lists a subset of the fields and the others are wildcards;
- a list `[T]`: for a column whose list patterns have largest length `M` (counting a rest pattern's preceding elements), the constructors `length 0`, `length 1`, ..., `length M`, and `length M+1 or more`, where `length n` has arity `n` and the last has arity `M+1`; a pattern of fixed length `k` is the constructor `length k` applied to its `k` element patterns; a pattern with `k` elements before a rest pattern is the wildcard-extended constructor for each `length n` with `n >= k`, with its `k` element patterns followed by `n - k` wildcards;
- `Int`, `Float` and `String`: these have unboundedly many values, so a set of literals never covers the type.
- `Map<K, V>` and `Set<T>`: these have no constructors. Only `_`, a binding or `else` can match such a scrutinee, and any other pattern is HS0504.

`useful(P, q)`, with `q = [q1, q2, ...]`:

1. If `q` is empty, it is true if `P` has no rows and false otherwise.
2. If `q1` is a constructor pattern `c(r1...rk)`: it is `useful(S(c, P), [r1...rk, q2, ...])`, where `S(c, P)` keeps the rows whose first pattern is `c(s1...sk)` or a wildcard, replacing that first pattern by `s1...sk` (or by `k` wildcards).
3. If `q1` is a wildcard: let `Σ` be the constructors that occur as the first pattern of some row of `P`. If `Σ` contains every constructor of the type (a *complete signature*), it is true if and only if for some constructor `c` in it `useful(S(c, P), [_ * arity(c), q2, ...])` is true. Otherwise it is `useful(D(P), [q2, ...])`, where `D(P)` keeps the rows whose first pattern is a wildcard and drops that first pattern.

Literal patterns are compared by value: `1` and `1` are the same constructor and `-1` and `1` differ, and `0.0`, `-0.0` and `0.00` are one constructor, as are `1` and `01`.

Reporting: an implementation reports HS0501 once per non-exhaustive `match`; it may include an example of an unmatched value in the message. It reports HS0502 for a literal that repeats an earlier row's top-level literal alternative, and HS0503 for every other unreachable row. Reachability is reported per row, on the pattern of that alternative, so an arm all of whose alternatives are unreachable reports one HS0503 for each of them, and a single dead alternative in a live arm reports just that alternative.

**Budget.** The algorithm can take exponential time on hostile input, so its work is bounded. Each call of `useful` counts as one unit, counted over the whole `match` (the exhaustiveness query and the query for every row), with the alternatives of an arm already expanded into rows. A `match` that has more than 1,000 rows, a list pattern with more than 250 elements, or a count above 100,000 units is HS0511 at the `match` keyword, and then neither HS0501, HS0502 nor HS0503 is reported for it.

## 7.8 Destructuring declarations and `for`

A `constant` or `variable` declaration and a `for` loop bind names through a *declaration pattern*: `_`, a name, a record pattern, or a list pattern, where every part inside a record or list pattern is again a declaration pattern. Literal patterns, `none` and variant patterns are not declaration patterns, because they can fail for reasons that a declaration has no way to handle; use `match` for those (HS0509). In the grammar the target is a name, a record pattern or a list pattern.

- A record pattern always matches a value of the record type it is used with, so it cannot fail.
- A list pattern matches only a list of the right length: a pattern of `k` elements matches a list with exactly `k` elements, and a pattern with a rest pattern matches a list with at least as many elements as it has patterns before the rest. If the value is a list of a different length, the run faults with HS1014. A program that cannot know the length uses `match`, which has an arm for each case.

```hollow
record Point = { x: Int, y: Int }
record Segment = { start: Point, end: Point }

constant s: Segment = { start: { x: 1, y: 2 }, end: { x: 4, y: 6 } }
constant { start: { x: x1, y: y1 }, end: { x: x2, y: y2 } } = s
constant people = [{ name: "Ada", age: 36 }, { name: "Grace", age: 45 }]

variable total = 0
for { age } in people {
    total = total + age
}

constant [first, second, ..rest] = [10, 20, 30, 40]
constant pairs = [[1, 2], [3, 4]]
variable dot = 0
for [a, b] in pairs {
    dot = dot + a * b
}

return [x2 - x1, y2 - y1, total, first + second, rest.length(), dot]
```

The program returns `[3, 4, 81, 30, 2, 14]`.

```hollow-error
record Point = { x: Int, y: Int }
constant p: Point = { x: 1, y: 2 }
constant { x: 0, y } = p // HS0509
return y
```

```hollow-fault
constant [a, b] = [1, 2, 3] // HS1014
return a + b
```
