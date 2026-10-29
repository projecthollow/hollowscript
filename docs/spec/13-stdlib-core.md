# 13. Standard library: numbers, strings and records

The standard library is pure and deterministic except where a function says otherwise (only `exp`, `ln`, `pow`, `sin`, `cos`, `tan` and `atan2` of [section 13.2](#132-numbers), which are exact only to within one unit in the last place). It performs no input or output, reads no clock and produces no randomness; those are capabilities ([Chapter 16](16-standard-capabilities.md)). It is always available and never imported. Chapters [13](13-stdlib-core.md), [14](14-stdlib-collections.md) and [15](15-stdlib-data.md) define the prelude functions; [section 13.6](#136-the-prelude) lists every prelude name and says where it is defined.

## 13.1 Conventions

**Notation.** Functions are written as signatures. A *free function* is called as `name(args)`. A *method* is called as `receiver.name(args)` and is listed under the type it belongs to, with the receiver omitted from the parameter list. Type parameters are written in angle brackets after the name; they are inferred and never written at a call ([section 2.10](02-types.md#210-generics)). Where a function has several signatures (an overload set), the one used is decided by the argument types; this is permitted only for prelude functions and is written as separate signatures. An overloaded function (`min`, `max`, `abs`, `clamp`) used as a value, without being called, needs an expected function type that selects exactly one of its signatures, and is HS0308 otherwise. In a signature the word `record` stands for any record type; a program cannot write it as a type, and a function that takes one (`keys`, `has`) is not a value: using it without a call is HS0334.

**Numeric arguments.** An `Int` argument passed for a `Float` parameter is converted ([section 2.7](02-types.md#27-assignability-and-numeric-coercion)). For an overload set with an `Int` signature and a `Float` signature, the `Int` signature is used when every argument is an `Int`, and otherwise the `Float` signature is used with the `Int` arguments converted.

**Cost.** Each function has a *cost* in steps ([section 11.3](11-resource-limits.md#113-steps)). `1` means constant cost. `n` means `1 + floor(N / 64)` steps, where `N` is stated with the function (a count of elements or UTF-8 bytes). Functions that call a closure charge the calls of the closure separately. `N` is never less than the number of elements or bytes that the function reads from all of its operands.

**Faults.** A function that is given an argument outside its stated domain faults with HS1011. A function whose result cannot be represented faults with HS1007 (`Int`), HS1009 (a `Float` that would not be a number) or HS1010 (a `Float` that would be infinite). A failure that is expected in a correct program is returned as a `Result` instead.

**Strings.** Positions and lengths in strings count code points ([section 13.3](#133-strings)).

## 13.2 Numbers

All are free functions. `Float` results are never negative zero.

| Signature | Meaning | Cost |
|---|---|---|
| `abs(x: Int): Int` | absolute value; faults HS1007 for -9223372036854775808 | 1 |
| `abs(x: Float): Float` | absolute value | 1 |
| `min(a: Int, b: Int): Int`, `min(a: Float, b: Float): Float` | the smaller | 1 |
| `max(a: Int, b: Int): Int`, `max(a: Float, b: Float): Float` | the larger | 1 |
| `clamp(x: Int, low: Int, high: Int): Int`, `clamp(x: Float, low: Float, high: Float): Float` | `low` if `x < low`, `high` if `x > high`, else `x`; faults HS1011 if `low > high` | 1 |
| `sign(x: Float): Int` | -1, 0 or 1 | 1 |
| `round(x: Float): Int` | nearest integer, halves away from zero: `round(2.5)` is `3`, `round(-2.5)` is `-3`, `round(-0.5)` is `-1` | 1 |
| `floor(x: Float): Int` | greatest integer not above `x` | 1 |
| `ceil(x: Float): Int` | least integer not below `x` | 1 |
| `trunc(x: Float): Int` | `x` with the fractional part removed, towards zero | 1 |
| `toFloat(n: Int): Float` | the nearest `Float` | 1 |
| `div(a: Int, b: Int): Int` | floor division: the greatest integer not above the exact quotient. Faults HS1008 if `b` is 0 and HS1007 for `div(-9223372036854775808, -1)` | 1 |
| `mod(a: Int, b: Int): Int` | floor modulo: `a - b * div(a, b)`, which has the sign of `b` (or is 0) and magnitude below `|b|`. Faults HS1008 if `b` is 0. `mod(-9223372036854775808, -1)` is 0 | 1 |
| `remainder(a: Int, b: Int): Int` | truncated remainder: `a - b * q` where `q` is the exact quotient truncated towards zero, so the result has the sign of `a` (or is 0). Faults HS1008 if `b` is 0. `remainder(-9223372036854775808, -1)` is 0 | 1 |
| `sqrt(x: Float): Float` | square root, correctly rounded; faults HS1009 if `x < 0` | 1 |
| `pow(base: Float, exponent: Float): Float` | `base` to the power `exponent`; the exact result is returned when it is representable; `pow(x, 0.0)` is `1.0`; faults HS1009 for a negative base with a non-integer exponent, and HS1010 if the result is infinite (including `pow(0.0, e)` for negative `e`) | 1 |
| `ipow(base: Int, exponent: Int): Int` | integer power; `ipow(x, 0)` is 1, including `ipow(0, 0)`; faults HS1011 if `exponent < 0`, and HS1007 if and only if the mathematical result is outside `Int`: no intermediate value may fault, so `ipow(-2, 63)` is -9223372036854775808, and `ipow(1, e)` and `ipow(-1, e)` never fault | 1 |
| `exp(x: Float): Float` | e to the power `x`; faults HS1010 on overflow | 1 |
| `ln(x: Float): Float` | natural logarithm; faults HS1009 if `x < 0` and HS1010 if `x` is 0 | 1 |
| `sin(x: Float): Float`, `cos(x: Float): Float`, `tan(x: Float): Float` | trigonometric functions of an angle in radians | 1 |
| `atan2(y: Float, x: Float): Float` | the angle in radians of the point `(x, y)`, in [-pi, pi]; `atan2(0.0, 0.0)` is `0.0` | 1 |
| `range(start: Int, upTo: Int): [Int]` | the list `start, start + 1, ..., upTo - 1`; empty if `start >= upTo`. The length is computed in unbounded arithmetic. The step charge is applied first and can fault with HS1001; then the allocation is charged before the list is built and can fault with HS1002 ([section 11.4](11-resource-limits.md#114-memory)) | n = length of the result |

`pi` is a prelude constant of type `Float` with the value 3.141592653589793.

**Accuracy.** `sqrt` and the basic operators are correctly rounded. `exp`, `ln`, `sin`, `cos`, `tan`, `atan2` and `pow` return a result within one unit in the last place of the exact value; an implementation is deterministic (the same inputs give the same result on that implementation) but two conforming implementations may differ in the last bit, so a program that must be portable to the last digit compares with a tolerance. The conformance corpus compares such results with a tolerance or does not print them ([section 19.3](19-conformance.md#193-the-expected-file)). Every other function in this chapter is exact.

**Rounding to an integer.** `round`, `floor`, `ceil` and `trunc` fault with HS1007 if the result is outside the range of `Int` (for example `floor(1e30)`).

```hollow
uses print

effect print("${round(-2.5)} ${round(2.5)} ${floor(-0.5)} ${ceil(0.2)} ${trunc(-3.9)}")
effect print("${abs(-3)} ${abs(-2.5)} ${min(1, 2.5)} ${max(3, 9)} ${clamp(15, 0, 10)}")
effect print("${div(7, 2)} ${mod(-7, 3)} ${remainder(-7, 3)} ${ipow(2, 62)}")
effect print("${sqrt(16)} ${sqrt(2)} ${pow(2, 10)} ${toFloat(3)}")
```

Output:

```text
-3 3 -1 1 -3
3 2.5 1.0 9 10
3 2 -1 4611686018427387904
4.0 1.4142135623730951 1024.0 3.0
```

`div` and `mod` are functions and not operators because `//` starts a comment ([section 1.2](01-lexical-structure.md#12-tokens-whitespace-and-comments)). `remainder` is the truncating remainder that other languages write `%`; `mod` is the floor modulo. For non-negative operands they agree.

## 13.3 Strings

A `String` is a sequence of Unicode scalar values, held as UTF-8. **Length, positions and all indexes count code points**, not bytes, UTF-16 units or grapheme clusters: `"é"` written as one precomposed code point has length 1, and the same letter written as `e` followed by a combining accent has length 2. There is no normalisation and no case folding except where a function says so. The byte length is a separate method, `byteLength()`.

`s[i]` is not defined for strings ([section 3.7](03-expressions.md#37-calls-fields-methods-and-indexing)). Positions are addressed with `at`, `substring` and `indexOf`, whose cost is linear in the string: an implementation may cache the code point count, but the specified cost does not assume it. Positions are zero-based.

Methods of `String` (the receiver is written `s`; `N` is the number of UTF-8 bytes of the receiver, and `M` that of the first argument, unless stated). `contains`, `indexOf` and `replace` MUST run in time linear in N + M, which the charge assumes.

| Signature | Meaning | Cost |
|---|---|---|
| `length(): Int` | number of code points | n (N) |
| `byteLength(): Int` | number of UTF-8 bytes | 1 |
| `isEmpty(): Bool` | whether `s` has no code points | 1 |
| `at(index: Int): String?` | the code point at `index` as a one-code-point string, or `none` if `index` is negative or at least the length | n |
| `chars(): [String]` | every code point as a one-code-point string, in order; `""` gives `[]` | n |
| `contains(part: String): Bool` | whether `part` occurs in `s`; the empty string occurs in every string | n (N + M) |
| `startsWith(prefix: String): Bool` | whether `s` begins with `prefix` | n |
| `endsWith(suffix: String): Bool` | whether `s` ends with `suffix` | n |
| `indexOf(part: String): Int?` | the position of the first occurrence of `part`, or `none`; the empty string is at 0 | n (N + M) |
| `substring(start: Int, upTo: Int): String` | the code points from `start` (inclusive) to `upTo` (exclusive); both positions are first clamped into `0 .. length`, and if `start >= upTo` the result is `""` | n |
| `toUpper(): String` | `s` with each code point converted to upper case | n |
| `toLower(): String` | `s` with each code point converted to lower case | n |
| `trim(): String` | `s` without leading and trailing white space | n |
| `trimStart(): String` | without leading white space | n |
| `trimEnd(): String` | without trailing white space | n |
| `split(separator: String): [String]` | see below | n |
| `lines(): [String]` | see below | n |
| `replace(target: String, replacement: String): String` | every non-overlapping occurrence of `target`, scanning left to right, replaced by `replacement`; if `target` is empty, `s` is returned unchanged | n (N + M) |
| `repeat(count: Int): String` | `s` repeated `count` times; faults HS1011 if `count < 0`; the result size is charged before it is built | n (result bytes) |
| `padStart(width: Int, fill: String): String` | if `s` has fewer than `width` code points, `s` preceded by the first `width - length` code points of `fill` repeated as often as needed; otherwise `s`. Faults HS1011 if `fill` is empty, whether or not any padding is needed | n (result bytes) |
| `padEnd(width: Int, fill: String): String` | the same with the padding after `s` | n (result bytes) |

**Case conversion** uses the Unicode Default Case Conversion algorithm of Unicode 16.0 (full mappings, including the unconditional mappings of the special casing data, and the Final_Sigma context rule), independent of any locale. So `"ß".toUpper()` is `"SS"`. A later Unicode version may be adopted in a minor version of the language, which can change the results of `toUpper`, `toLower`, `trim` and of filter matching ([Appendix A](appendix-a-filters.md#a8-evaluation)).

**White space** for `trim` is the Unicode `White_Space` property.

**`split`.** `split(separator)` with a non-empty separator divides `s` at each non-overlapping occurrence of `separator`, scanning left to right, and returns the pieces in order, including empty pieces: `"a,b,,c".split(",")` is `["a", "b", "", "c"]`, `"a,".split(",")` is `["a", ""]` and `"".split(",")` is `[""]`. With the empty separator it returns the code points, as `chars()` does, so `"a😀b".split("")` is `["a", "😀", "b"]` and `"".split("")` is `[]`.

**`lines`.** `lines()` divides `s` at each line terminator, LF or CR LF, and returns the lines without their terminators. A terminator at the very end of `s` does not create a final empty line: `"a\nb\n".lines()` is `["a", "b"]`, `"a\n\nb".lines()` is `["a", "", "b"]`, and `"".lines()` is `[]`. A CR that is not followed by LF is an ordinary character.

```hollow
uses print

constant s = "  Hello, World  "
constant t = s.trim()
effect print("${t.length()} ${t.byteLength()} ${t.toUpper()} ${t.substring(7, 100)}")
effect print("${"a😀b".length()} ${"a😀b".byteLength()} ${"a😀b".split("").length()}")
effect print("${"a,b,,c".split(",").length()} ${"x".repeat(3)} ${"7".padStart(3, "0")}")
```

Output:

```text
12 12 HELLO, WORLD World
3 6 3
4 xxx 007
```

## 13.4 Converting between numbers and text

There is no implicit conversion between numbers and strings. The ways to obtain text from a number are string interpolation ([section 3.9](03-expressions.md#39-string-interpolation)) and `fixed`; the ways to obtain a number from text are `parseInt` and `parseFloat`.

| Signature | Meaning | Cost |
|---|---|---|
| `parseInt(text: String): Result<Int, String>` | see below | n (bytes of `text`) |
| `parseFloat(text: String): Result<Float, String>` | see below | n (bytes of `text`) |
| `fixed(x: Float, digits: Int): String` | `x` in positional notation with exactly `digits` digits after the decimal point | n (bytes of the result) |

**`parseInt`.** The text must be an optional `+` or `-` followed by one or more ASCII digits and nothing else: no white space, no underscores, no other characters. Leading zeros are allowed. The result is `Ok` with the value, or `Err("invalid integer")` if the text does not have that form, or `Err("integer out of range")` if the value is not representable. `parseInt("-0")` is `Ok(0)`.

**`parseFloat`.** The text must be an optional `+` or `-`, then one or more ASCII digits, optionally `.` and one or more digits, optionally an exponent (`e` or `E`, an optional sign and one or more digits), and nothing else. There is no `.5`, `5.`, `nan`, `inf`, hexadecimal or underscore form. The result is `Ok` with the `Float` nearest to the decimal value (ties to the even significand), or `Err("invalid number")` if the text does not have that form, or `Err("number out of range")` if the value would be infinite. A value that underflows is `Ok(0.0)`, and a parsed negative zero (`-0`, `-0.0`) is `0.0`. The exponent is evaluated in unbounded arithmetic: an implementation may treat an exponent of magnitude above 2^63 as saturated, because the significand has fewer digits than that and cannot change the outcome, which gives `Err("number out of range")` for a positive exponent and a non-zero significand, `Ok(0.0)` for a negative exponent, and `Ok(0.0)` for a zero significand. A significand of more than 800 digits may be cut after the 800th if a non-zero digit that follows is kept as one sticky digit, which never changes the correctly rounded result.

**`fixed`.** `digits` must be from 0 to 100 (HS1011 otherwise). The result is the decimal expansion of the exact value of `x`, rounded to `digits` fractional digits with halves rounded away from zero; a `-` sign is written only if the rounded value is not zero; there is no decimal point when `digits` is 0. `fixed(3.14159, 2)` is `"3.14"`, `fixed(2.5, 0)` is `"3"`, `fixed(-2.5, 0)` is `"-3"`, `fixed(0.125, 2)` is `"0.13"`, `fixed(1.0, 3)` is `"1.000"`, and `fixed(-0.001, 2)` is `"0.00"`.

```hollow
uses print

effect print("${parseInt("42").unwrapOr(0)} ${parseInt("4x2").isErr()} ${parseFloat("2.5e2").unwrapOr(0.0)} ${fixed(3.14159, 2)}")
```

Output:

```text
42 true 250.0 3.14
```

## 13.5 Records and results

| Signature | Meaning | Cost |
|---|---|---|
| `keys(r): [String]` | the names of the fields of the record `r`, sorted ascending by code point sequence | n (number of fields) |
| `has(r, name: String): Bool` | whether the record type of `r` has a field called `name` | 1 |

`r` is a `record` in the sense of [section 13.1](#131-conventions), a record of any type; passing any other type is HS0301. Record types have no field order, so `keys` returns names in sorted order. The result of `has` depends only on the record's type. `values` is deliberately absent: it would have to return a list whose elements have different types.

`success` and `failure` build `Result` values ([section 8.2](08-errors-and-results.md#82-the-result-type)). The `Result` methods are in [section 8.4](08-errors-and-results.md#84-result-methods).

```hollow
uses print

record Task = { title: String, priority: Int }

constant t: Task = { title: "Ship", priority: 2 }
effect print("${keys(t).join(",")} ${has(t, "title")} ${has(t, "owner")}")
```

Output:

```text
priority,title true false
```

## 13.6 The prelude

These are all the names the prelude defines, with the section that defines each. A program may declare a name from this list ([section 4.6](04-statements.md#46-scope-and-shadowing)), except the built-in types named in [section 2.1](02-types.md#21-namespaces); it cannot redeclare a capability.

| Names | Kind | Defined in |
|---|---|---|
| `Int`, `Float`, `Bool`, `String`, `Filter` | types | [Chapter 2](02-types.md) |
| `Result` | enum type | [Chapter 8](08-errors-and-results.md) |
| `Map`, `Set`, `MapEntry` | types | [Chapter 14](14-stdlib-collections.md) |
| `Json`, `JsonError`, `TimeParts` | types | [Chapter 15](15-stdlib-data.md) |
| `abs`, `min`, `max`, `clamp`, `sign`, `round`, `floor`, `ceil`, `trunc`, `toFloat`, `div`, `mod`, `remainder`, `sqrt`, `pow`, `ipow`, `exp`, `ln`, `sin`, `cos`, `tan`, `atan2`, `range`, `pi` | values | [section 13.2](#132-numbers) |
| `parseInt`, `parseFloat`, `fixed` | values | [section 13.4](#134-converting-between-numbers-and-text) |
| `keys`, `has` | values | [section 13.5](#135-records-and-results) |
| `success`, `failure` | values | [Chapter 8](08-errors-and-results.md) |
| `set`, `mapFrom` | values | [Chapter 14](14-stdlib-collections.md) |
| `json`, `time` | namespaces | [Chapter 15](15-stdlib-data.md) |

Methods are not prelude names: they exist only on values of their types. New names may be added to the prelude in a minor version of the language; because a program's own declaration takes precedence, that cannot break it.
