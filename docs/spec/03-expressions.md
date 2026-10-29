# 3. Expressions and operators

This chapter defines the expressions of the language, their types and their values. The complete grammar is in [Chapter 18](18-grammar.md). Statements are in [Chapter 4](04-statements.md).

## 3.1 Kinds of expression

| Expression | Example | Section |
|---|---|---|
| Literal | `42`, `2.5`, `true`, `"text"`, `` `status:open` ``, `none` | [Chapter 1](01-lexical-structure.md) |
| Name | `total` | [section 4.6](04-statements.md#46-scope-and-shadowing) |
| List, map and record literal | `[1, 2]`, `["a": 1]`, `{ x: 1, y: 2 }` | [section 3.8](#38-list-map-and-record-literals) |
| Closure | `fn(n: Int): Int { return n + 1 }` | [section 5.3](05-functions-and-closures.md#53-closures) |
| Parenthesised | `(a + b)` | this section |
| Field access, method call, call, index | `p.x`, `xs.length()`, `f(1)`, `xs[0]` | [section 3.7](#37-calls-fields-methods-and-indexing) |
| Propagation | `parseInt(text)?` | [section 8.3](08-errors-and-results.md#83-the--operator) |
| Prefix and binary operators | `-a`, `not b`, `a + b`, `a and b` | [sections 3.3](#33-operators-and-precedence) to [3.6](#36-boolean-operators) |
| Effect call | `effect print("hi")` | [Chapter 9](09-effects-and-capabilities.md) |
| String interpolation | `"n = ${n}"` | [section 3.9](#39-string-interpolation) |

Parentheses group and never change a value or a type. Every expression has exactly one type, determined by the rules of this specification.

## 3.2 Order of evaluation

Evaluation is strictly left to right and depth first, and is the same on every conforming implementation, except in the last digit of the results of the functions that [section 13.2](13-stdlib-core.md#132-numbers) lists under Accuracy. Where an effect is observable (a capability call, a `print`, a fault, a step count) the order is part of the language.

- **Binary operators** evaluate the left operand, then the right operand, then apply the operator. The exceptions are `and` and `or`, which do not evaluate the right operand when the left decides the result ([section 3.6](#36-boolean-operators)).
- **Calls** evaluate the callee expression first (for a method call, the receiver; for a field call, the record and the field), then the arguments from left to right, then perform the call. Numeric coercion of an argument happens immediately after that argument is evaluated.
- **`effect` calls** evaluate exactly as the call inside them.
- **Literals** evaluate their elements in source order: the elements of a list, the key and then the value of each map entry, the fields of a record literal in the order written, and the parts of a string from left to right.
- **Index** `a[i]` evaluates `a` then `i`.
- **Assignment** `x = e` evaluates `e`, then stores it.
- **Postfix `?`** evaluates its operand, then decides.
- **Statements** in a block run in order; `for` evaluates its collection once, before the first iteration.

## 3.3 Operators and precedence

Binary operators are all left-associative. Precedence from lowest to highest:

| Level | Operators | Operand types |
|---|---|---|
| 1 | `or` | `Bool` |
| 2 | `and` | `Bool` |
| 3 | `==` `!=` | see [section 3.5](#35-comparison-and-equality) |
| 4 | `<` `>` `<=` `>=` | numbers or strings |
| 5 | `+` `-` | numbers |
| 6 | `*` `/` | numbers |
| 7 (prefix) | `not` `-` `effect` | `Bool`, number, call |
| 8 (postfix) | call `f(...)`, index `x[i]`, field `x.name`, propagate `x?` | |

A comparison of a comparison, `a < b < c`, is a type error because `a < b` is a `Bool` and `<` is not defined on `Bool`. `not ready and done` is `(not ready) and done`. `-a * b` is `(-a) * b`. `effect` and `?` are described in [Chapter 9](09-effects-and-capabilities.md) and [Chapter 8](08-errors-and-results.md).

## 3.4 Arithmetic

| Expression | Operand types | Result type |
|---|---|---|
| `a + b`, `a - b`, `a * b` | `Int`, `Int` | `Int` |
| `a + b`, `a - b`, `a * b` | any operand a `Float`, the other `Int` or `Float` | `Float` |
| `a / b` | any numbers | `Float` |
| `-a` | `Int` | `Int` |
| `-a` | `Float` | `Float` |

Any other operand types are a type error (HS0310 for unary minus, HS0311 for the binary operators). `+` never joins strings: string building uses interpolation ([section 3.9](#39-string-interpolation)) or `join`.

**Int arithmetic** is exact 64-bit two's complement arithmetic in which the result must be representable. If the mathematical result of `+`, `-`, `*` or unary `-` is below -9223372036854775808 or above 9223372036854775807, the program faults with HS1007. Nothing wraps or saturates. Integer overflow is classed as a resource fault: it cannot be recovered from and it faults the instance ([section 8.1](08-errors-and-results.md#81-faults-and-recoverable-errors)).

**Float arithmetic** is IEEE 754 binary64 arithmetic with round-to-nearest, ties-to-even, performed one operation at a time: a compound expression such as `a * b + c` is two roundings, and implementations MUST NOT fuse operations or use extended precision. When an `Int` operand meets a `Float` operand, the `Int` is first converted to the nearest `Float` ([section 2.7](02-types.md#27-assignability-and-numeric-coercion)); this conversion can lose precision above 2^53. After every `Float` operation:

- if the result is infinite, the program faults with HS1010;
- if a result would be not-a-number, the program faults with HS1009 (this can only arise from the library functions in [section 13.2](13-stdlib-core.md#132-numbers); the basic operators cannot produce it);
- a result of negative zero is replaced by zero. There is no negative zero anywhere in the language.

Results that underflow to a subnormal or to zero are allowed and do not fault.

**Division.** `a / b` converts both operands to `Float` as above and then divides. If the divisor is zero (`0` or `0.0`), the program faults with HS1008 before dividing. `7 / 2` is the `Float` `3.5`. Integer division and modulo are the library functions `div`, `mod` and `remainder`.

```hollow
uses print

constant a = 7 / 2
constant b = 7 + 2.5
constant c = 2 * 3
constant d = div(-7, 2)
constant e = mod(-7, 2)
constant f = remainder(-7, 2)
effect print("${a} ${b} ${c} ${d} ${e} ${f}")
```

Output:

```text
3.5 9.5 6 -4 1 -1
```

An overflowing operation faults:

```hollow-fault
constant big = 9223372036854775807
constant over = big + 1 // HS1007
return over
```

## 3.5 Comparison and equality

**Equality** `==` and `!=` are defined for the following operand type pairs, and are otherwise a type error (HS0313; HS0314 if an operand type is not equatable):

- two operands of the identical equatable type;
- `S` or `S?` with `T` or `T?`, where `S` and `T` are identical, or are `Int` and `Float` in either order. So `Int` with `Float`, `Int?` with `Float`, `Float?` with `Int` and `Int?` with `Float?` are all allowed;
- an optional type `T?` and the literal `none`.

Two optional operands are equal when both are `none`, or both are present and equal. An operand that is `none` is not equal to an operand that is present, and `Int` and `Float` are compared by exact value.

The literal `none` may only be compared with an optional type (HS0328).

The value semantics, by type:

| Type | `a == b` is true when |
|---|---|
| `Int`, `Float` | the two numbers are mathematically equal. Comparing an `Int` with a `Float` does not convert the `Int` to `Float`; it compares the exact values, so `9007199254740993 == 9007199254740992.0` is `false`. There is one zero. |
| `Bool` | the same truth value |
| `String` | the same sequence of code points; no normalisation and no case folding |
| `[T]` | the same length and equal elements at every index |
| `Map<K, V>` | the same set of keys and equal values for each key |
| `Set<T>` | the same elements |
| record | equal values in every field (field order is irrelevant) |
| enum | the same variant and equal payload values position by position |
| `T?` | both `none`, or both present and equal |

`!=` is the negation of `==`. `Filter` values, function values, and any type containing them cannot be compared at all.

**Ordering** `<`, `>`, `<=`, `>=` are defined for two numbers (`Int` or `Float`, in any combination, compared by exact mathematical value) and for two `String`s. Strings compare lexicographically by code point, which is the same as comparing their UTF-8 bytes, and a prefix sorts before a longer string. Other operand types are HS0312.

```hollow
uses print

effect print("${1 == 1.0} ${9007199254740993 == 9007199254740992.0} ${"a" < "b"} ${[1, 2] == [1, 2]}")
```

Output:

```text
true false true true
```

Optional numbers compare in the same way:

```hollow
constant x: Float? = 2.0
constant n: Int? = none
return [x == 2, n == 2.0, x != n]
```

The program returns `[true, false, true]`.

## 3.6 Boolean operators

`a and b` and `a or b` require `Bool` operands (HS0315) and produce a `Bool`. `and` does not evaluate `b` if `a` is `false`; `or` does not evaluate `b` if `a` is `true`. `not a` requires a `Bool` (HS0315).

```hollow
uses print

fn loud(): Bool {
    effect print("evaluated")
    return true
}

constant a = false and loud()
constant b = true or loud()
effect print("done ${a} ${b}")
```

Output:

```text
done false true
```

## 3.7 Calls, fields, methods and indexing

**Field access** `r.name` requires `r` to be a record (HS0322 otherwise, including when `r` is an optional that has not been narrowed) and `name` to be one of its fields (HS0321 otherwise). Its type is the field's type.

**Namespaces.** A namespace such as `json`, `time` or a capability like `clock` is an ordinary record whose fields are functions. `clock.now` is field access; `clock.now()` is a call of the field.

**Method call** `r.name(args)` applies to a receiver whose type has methods: `String`, lists, maps, sets and `Result` ([Chapters 13](13-stdlib-core.md) to [15](15-stdlib-data.md), and [section 8.4](08-errors-and-results.md#84-result-methods)). If the receiver type is a record and its field `name` has a function type, the same syntax calls that field. `Int`, `Float`, `Bool`, `Filter` and enums have no methods. A method name that is not defined for the receiver's type is HS0320. A method is not a value, so `xs.map` without a call is HS0334; a script that wants a function value writes a closure.

**Enum variant construction** `Shape.Circle(1.5)` and `Shape.Empty` name a variant through the enum name; they are specified in [section 6.3](06-data-types.md#63-enums).

**Call** `f(args)` requires the callee to have a function type (HS0318 otherwise) and the number of arguments to equal the number of parameters (HS0319 otherwise). Each argument must be assignable to its parameter type ([section 2.7](02-types.md#27-assignability-and-numeric-coercion), HS0301). The value of the call has the return type, and a call to a function with no return type is not an expression that has a value: it can be used only as a statement ([section 4.9](04-statements.md#49-expression-statements)). Calls may be marked with `effect` ([Chapter 9](09-effects-and-capabilities.md)). There are no named arguments, no default arguments and no variadic functions in user code.

**Index** `x[i]` applies to lists and maps. For a list `xs` of `[T]`, `i` must be an `Int` and the result has type `T?`: it is the element at zero-based position `i` if `0 <= i < length`, and `none` otherwise. For a `Map<K, V>`, `i` must be of type `K` and the result has type `V?`: the value for that key, or `none` if the key is absent. Indexing anything else (a string, a record, a set, a number) is HS0323. There is no slicing syntax; see `slice`.

```hollow
uses print

constant xs = [10, 20]
constant a = xs[1]
constant b = xs[2]
constant c = xs[-1]
constant ages = ["ada": 36, "grace": 45]
effect print("${a == none} ${b == none} ${c == none} ${ages["ada"] == 36} ${ages["linus"] == none}")
```

Output:

```text
false true true true true
```

## 3.8 List, map and record literals

**List literal** `[e1, e2, e3]` builds a list. The element type is determined as in [section 2.9](02-types.md#29-inference). A trailing comma is allowed. The empty list `[]` needs an expected type.

**Map literal** `[k1: v1, k2: v2]` builds a `Map<K, V>`. Every key must have the same key type ([section 2.8](02-types.md#28-equatable-and-orderable-types)) and every value the same value type, determined as for lists. `[:]` is the empty map and needs an expected type. If two entries have the same key, the entry that appears later replaces the earlier one. A key is a *literal key* if it is an `Int`, `String` (without interpolation) or `Bool` literal, optionally preceded by one unary minus. Two literal keys are equal when their values are equal (`1` and `01`, `"a"` and `"\u{61}"`), and the second of them is HS0203. Any other key expression is not compared before the program runs, and a duplicate at run time replaces the earlier entry. A map literal with a first key that is followed by a `:` is recognised by that colon; `[a, b]` is always a list.

```hollow-error
constant m = ["a": 1, "\u{61}": 2] // HS0203
return m
```

**Record literal** `{ x: 1, y: 2 }` builds a record with the listed fields. Field names must be distinct (HS0203). The empty record `{}` is HS0112. The type is determined as in [section 2.9](02-types.md#29-inference).

A record literal may not appear at the top level of the condition of an `if` or `while`, of the scrutinee of a `match`, or of the collection of a `for`, because a `{` after such an expression opens the block. It is HS0110; parenthesise the literal to use it there. A record literal is allowed there only inside parentheses, square brackets (an index, a list literal or a map literal), an argument list, or the `${` `}` of an interpolation, where it is unambiguous.

```hollow
record Person = { name: String, age: Int }

constant fallback: Person = { name: "unknown", age: 0 }

fn isFallback(p: Person): Bool {
    if (p == { name: "unknown", age: 0 }) {
        return true
    }
    return false
}

return isFallback(fallback)
```

## 3.9 String interpolation

`"${e}"` inside a string literal evaluates `e` and inserts its text. The type of `e` must be `Int`, `Float`, `Bool` or `String` (HS0317 otherwise; an optional type must be narrowed first). The text is:

- for `String`: the string itself;
- for `Bool`: `true` or `false`;
- for `Int`: base-10 digits, preceded by `-` if negative, with no leading zeros and no separators;
- for `Float`: the shortest text that identifies the value, formatted as below.

**Float text.** A zero is `0.0`. Otherwise, let the *digits* `d1 d2 ... dn` (with `d1` and `dn` not zero, `n >= 1`) and the integer `e` be the shortest decimal significand and exponent that read back as exactly the same `Float`, so that the magnitude is `d1.d2...dn` times 10 to the power `e`. When more than one shortest digit string reads back as that `Float`, the one nearest the exact value is used, and a tie is broken towards an even final digit. Then:

- if `-5 <= e <= 15`, the text is positional: for `e >= 0` the integer part is the first `e + 1` digits, padded on the right with zeros if `n < e + 1`, then `.` and the remaining digits, or `.0` if there are none; for `e < 0` the text is `0.` followed by `-e - 1` zeros and then all `n` digits;
- otherwise the text is `d1`, then `.` and `d2...dn` if `n > 1`, then `e`, then the exponent `e` in base 10 with a leading `-` if negative and no `+` and no leading zeros.

A negative value has a leading `-`. Consequently a whole `Float` always shows a decimal point or an exponent, and the text of any `Float` reads back as a `Float` literal.

| Value | Text |
|---|---|
| `1.0` | `1.0` |
| `100.0` | `100.0` |
| `1.5` | `1.5` |
| `0.1` | `0.1` |
| `0.00001` | `0.00001` |
| `0.000001` | `1e-6` |
| `1000000000000000.0` | `1000000000000000.0` |
| `10000000000000000.0` | `1e16` |
| `1.5e20` | `1.5e20` |
| `-2.5e-7` | `-2.5e-7` |

The text is produced without regard to the host's locale.

```hollow
uses print

effect print("${1.0} ${100.0} ${1.5} ${2 / 3} ${1.0e20} ${-0.000001}")
```

Output:

```text
1.0 100.0 1.5 0.6666666666666666 1e20 -1e-6
```

An interpolation may contain an `effect` call:

```hollow-fragment
constant line = "now is ${effect clock.now()}"
```
