# 1. Lexical structure

This chapter defines how a source file is turned into a sequence of tokens. The grammar that consumes the tokens is in [Chapter 18](18-grammar.md); the diagnostics named here are defined in [Chapter 17](17-diagnostics.md).

## 1.1 Source text

A source file is a sequence of bytes that MUST be valid UTF-8. The conventional extension is `.hws`.

- **Invalid UTF-8.** If the bytes are not valid UTF-8, the implementation reports HS0001 at the first offending byte, then processes no further.
- **Size.** A source file may be at most 4,194,304 bytes. A larger file is rejected with HS0002 and processed no further. A host that passes source as already-decoded text applies the same limit to its UTF-8 length. The size check is made first, on the byte length, before UTF-8 validation, so a file that is both too large and not valid UTF-8 is HS0002.
- **Byte order mark.** A single U+FEFF as the very first character of the file is skipped and is not part of any token. A U+FEFF anywhere else is an unrecognised character (HS0005).
- **Line terminators.** A line terminator is the two-character sequence CR LF (U+000D U+000A) or the single character LF (U+000A). Both are one line terminator. A CR that is not immediately followed by LF is an unrecognised character (HS0005) wherever it occurs, including inside strings and comments.
- **Positions.** A *span* is a half-open range `[start, end)` of byte offsets from the first byte of the file, counting the byte order mark if there is one. A *line* number is 1 for the first line and increases by one after each line terminator. A *column* is 1 plus the number of code points between the start of the line and the position (the byte order mark is not counted). Diagnostics report the span, the line and the column of the span start.
- **No normalisation.** Source text is never normalised. Two strings that differ only in Unicode normalisation form are different strings.
- **Bidirectional controls.** The characters U+202A to U+202E and U+2066 to U+2069 (the bidirectional embedding, override and isolate controls) are rejected wherever they occur, including inside strings and comments, with HS0005. A string that needs one writes it with a `\u{...}` escape ([section 1.5](#15-string-literals)).
- **Control characters.** The C0 controls U+0000 to U+001F other than TAB, and U+007F, are unrecognised characters (HS0005) wherever they occur, other than as part of a line terminator.
- **Escapes are not source text.** The restrictions of this section apply to the characters of the source file. A `\u{...}` escape in a string may name any Unicode scalar value, including U+0000 and the bidirectional controls ([section 1.5](#15-string-literals)).

## 1.2 Tokens, whitespace and comments

The lexer reads the text left to right and produces tokens by maximal munch: at each position it takes the longest token the rules below allow.

**Whitespace** is SPACE (U+0020) and TAB (U+0009). It separates tokens and is otherwise insignificant. Any other whitespace character (form feed, no-break space, U+2028 and so on) is an unrecognised character (HS0005).

**Comments** begin with `//` and run to, but not including, the next line terminator. There is no block comment. A comment is not a token and never ends a statement ([section 1.8](#18-statement-terminators)). Because `//` always starts a comment, HollowScript has no floor-division operator; integer division is the standard library function `div` ([Chapter 13](13-stdlib-core.md)).

**Tagged comments.** A comment whose text, after `//` and any spaces or tabs, begins with one of the words `todo`, `explain` or `note` in lower case immediately followed by `:` is a *tagged comment*. Tagged comments have no effect on the meaning of a program. Tools may retain and query them. Every other comment is a plain comment, and tools that rewrite source MUST preserve plain comments as well.

```hollow
// note: this line is a tagged comment
// this one is a plain comment
constant answer = 42 // trailing comments are allowed
return answer
```

## 1.3 Identifiers and keywords

```
identifier = ( letter | "_" ) { letter | digit | "_" } ;
letter     = "A" .. "Z" | "a" .. "z" ;
digit      = "0" .. "9" ;
```

Identifiers are ASCII only and case-sensitive. There is no maximum length other than the source size limit. A character outside ASCII that appears outside a string, a filter literal or a comment is an unrecognised character (HS0005); in particular `café` and `名前` are not identifiers.

The identifier `_` on its own is the *discard name*. It may be declared (as a parameter, a loop variable or a binding) any number of times in the same scope, it never conflicts with any other declaration, and it can never be read ([section 7.3](07-patterns.md#73-bindings-and-the-discard-name)).

**Keywords.** These 27 words are reserved and are never identifiers:

```
and       assert    break     constant  continue  effect    else
enum      export    false     fn        for       from      if
import    in        match     none      not       or        record
return    test      true      uses      variable  while
```

`test` and `assert` are reserved for a future version. Using either of them anywhere is HS0104. A keyword cannot be used as a variable, parameter, field, method or type name. The names of built-in types (`Int`, `Float`, `Bool`, `String`, `Filter`, `Result`, `Map`, `Set` and the prelude types in [Chapter 13](13-stdlib-core.md)) are not keywords; they live in the type namespace ([Chapter 2](02-types.md#21-namespaces)).

## 1.4 Number literals

```
intLiteral   = digit { [ "_" ] digit } ;
floatLiteral = intLiteral "." intLiteral [ exponent ]
             | intLiteral exponent ;
exponent     = ( "e" | "E" ) [ "+" | "-" ] intLiteral ;
```

- A literal has no sign. A leading `-` is the prefix operator ([section 3.3](03-expressions.md#33-operators-and-precedence)).
- Underscores may appear only between two digits and are ignored.
- There are no hexadecimal, octal or binary literals and no suffixes.
- A digit is required on both sides of the decimal point.
- Leading zeros are allowed and ignored: `007` is the `Int` 7 and `00.5` is the `Float` 0.5.

A literal is an `Int` literal if it has neither a fractional part nor an exponent, and a `Float` literal otherwise.

**Malformed numbers.** The lexer scans a number as follows and reports the first problem it finds. The malformed text is consumed as one token of the kind the literal was becoming (`Int` if no `.` or exponent was seen), flagged malformed, so the parser and checker do not report further errors about it.

| Text | Diagnostic |
|---|---|
| `2.` or `2.x` (a `.` directly after digits, not followed by a digit) | HS0011, the `.` is consumed |
| `.5` (a `.` at a token start directly followed by a digit) | HS0012, the `.` and digits are consumed |
| `1e`, `1e+`, `2.0E-` (an exponent with no digit) | HS0016 |
| `12abc`, `0x10`, `1_`, `1__0` (a number directly followed by a letter or `_`) | HS0013, the letters, digits and underscores are consumed |

**Ranges.** The lexer reads an `Int` literal of up to 19 significant digits exactly. A value above 9223372036854775808 (2^63) is HS0014. The value exactly 9223372036854775808 is accepted by the lexer, and the checker accepts it in exactly two places: as the operand of a unary minus token when no other token is between the two and no postfix operator is applied to the literal, and directly after the `-` of a literal pattern ([section 7.1](07-patterns.md#71-pattern-forms)). Anywhere else the checker reports HS0014, so `-(9223372036854775808)`, `-9223372036854775808?` and `-9223372036854775808.f()` are HS0014. `- -9223372036854775808` is accepted by the checker: the inner minus writes the minimum `Int` and the outer one overflows at run time (HS1007). This is what makes the minimum `Int` writable as `-9223372036854775808`.

A `Float` literal denotes the IEEE 754 binary64 value nearest to the decimal value written, with ties resolved to the even significand (correct rounding). A literal whose value rounds to an infinity is HS0015. A non-zero literal that rounds to zero or to a subnormal is accepted. `1e-400` is `0.0`.

```hollow
constant a = 1_000_000
constant b = 2.5e-3
constant c = 1E3
constant d = -9223372036854775808
return [a, d]
```

```hollow-error
constant a = -(9223372036854775808) // HS0014
return a
```

```hollow
fn isMinimum(n: Int): Bool {
    match n {
        -9223372036854775808 {
            return true
        }
        else {
            return false
        }
    }
}

return [isMinimum(-9223372036854775808), isMinimum(0)]
```

The second program returns `[true, false]`.

```hollow-fault
constant a = - -9223372036854775808 // HS1007
return a
```

## 1.5 String literals

A string literal is delimited by double quotes and contains a sequence of *parts*, each of which is literal text or an interpolation.

```
stringLiteral  = '"' { stringChar | escape | interpolation } '"' ;
stringChar     = any code point except '"', "\", LF, CR and the code points rejected in section 1.1 ;
escape         = "\" ( '"' | "\" | "n" | "t" | "r" | "$" )
               | "\u{" hexDigit { hexDigit } "}" ;
interpolation  = "${" expression "}" ;
```

**Escapes.**

| Escape | Meaning |
|---|---|
| `\"` | U+0022 |
| `\\` | U+005C |
| `\n` | U+000A |
| `\t` | U+0009 |
| `\r` | U+000D |
| `\$` | U+0024; needed only to write a literal `${` as `\${` |
| `\u{X}` | the code point with hexadecimal value `X`, 1 to 6 hex digits, either case; any Unicode scalar value is allowed, including U+0000 |

Any other backslash sequence is HS0008. A `\u{...}` escape whose value is a surrogate (U+D800 to U+DFFF) or above U+10FFFF, or which has zero or more than six digits, or is not closed by `}`, is HS0009. A `$` not followed by `{` is an ordinary character. A raw line terminator inside a string, or the end of the file before the closing quote, is HS0006.

**Interpolation.** `${` begins an interpolation. It contains one expression ([Chapter 3](03-expressions.md)) and ends at the matching `}`. The lexer tracks a stack of contexts: while inside an interpolation it lexes ordinary tokens, counting `{` and `}` so that record literals inside an interpolation do not end it, and a `"` inside an interpolation begins a nested string with the same rules, so interpolations nest. The interpolation ends at the first `}` that returns the brace count to zero.

Inside an interpolation:

- A line terminator or the end of the file ends the string abnormally. The lexer reports HS0007 at the `${`, emits the string as one malformed string token that covers the text up to the line terminator, and resumes lexing on the next line.
- Everything is lexed as code, so `//` starts a comment that runs to the end of the line, which then makes the interpolation unterminated (HS0007).
- The value is rendered as specified in [section 3.9](03-expressions.md#39-string-interpolation).

Interpolations may be nested at most 100 deep; beyond that the lexer reports HS0115.

```hollow
constant name = "world"
constant n = 3
return "hello ${name}, ${"nested ${n + 1}"} \u{1F600} \${literal}"
```

The program returns the string `hello world, nested 4 😀 ${literal}`.

## 1.6 Filter literals

A filter literal is delimited by backticks and is a value of the built-in type `Filter` ([Appendix A](appendix-a-filters.md)).

```
filterLiteral = "`" { filterChar | "\`" | "\\" } "`" ;
filterChar    = any code point except "`", "\", LF, CR and the code points rejected in section 1.1 ;
```

The two-character sequence `` \` `` stands for one backtick and `\\` stands for one backslash. A backslash followed by any other character is kept verbatim as a backslash and that character. The text obtained after this unescaping is the *filter source*; it is parsed and validated as specified in Appendix A. The backtick is used for nothing else. A line terminator inside a filter literal, or the end of the file before the closing backtick, is HS0010; the text to the line terminator is consumed as one malformed filter token.

## 1.7 Punctuation and operators

```
(  )  [  ]  {  }  ,  :  .  ..  ?
+  -  *  /
=  ==  !=  <  >  <=  >=
```

`==`, `!=`, `<=`, `>=` and `..` are single tokens. A `!` not followed by `=` is an unrecognised character (HS0005). There is no `&&`, `||`, `!`, `%`, `++`, `+=` or any other compound assignment operator; the boolean operators are the keywords `and`, `or` and `not`.

## 1.8 Statement terminators

The lexer inserts a virtual token `TERM` at a line terminator, and at the end of the file, if and only if the last token before it (ignoring whitespace and comments) is one of:

- an identifier
- an `Int` literal, a `Float` literal, a string literal or a filter literal
- `true`, `false` or `none`
- `return`, `break` or `continue`
- `)`, `]`, `}` or `?`

Consecutive `TERM` tokens collapse into one, the first of them being kept. A line with no tokens produces no `TERM`, so blank lines and comment-only lines are insignificant, and there is no empty statement.

**Spans of virtual tokens.** A `TERM` inserted at a line terminator has the span of that line terminator, from its first byte to its last. A `TERM` inserted at the end of a file that does not end with a line terminator, and `EOF`, have the empty span at the end offset of the file. A file that ends with a line terminator therefore places `EOF` on the line after the last one, at column 1. A diagnostic reported at a `TERM` or at `EOF` uses the line and column of the start of that span.

A statement is also ended by a closing `}` that follows it on the same line, so the terminator before a closing `}` is optional and `{ return 1 }` is a block containing one statement.

The lexer emits `TERM` by this rule unconditionally. The **parser** discards `TERM` tokens wherever a statement cannot begin: between the delimiters of a parenthesised expression, an argument or parameter list, a list, map or index bracket, and the braces of a record literal, record type, record pattern or enum declaration. This must not be done by counting bracket depth in the lexer: a closure passed as an argument contains a real block, where `TERM` is significant, inside an argument list, where it is not.

Because a `TERM` is inserted after a line ending in `}`, the keyword `else` must appear on the same line as the `}` that closes the preceding block (`} else {`). An `else` that begins a line is HS0122.

```hollow
constant total = 1 +
    2 +
    3
constant shape = {
    width: total,
    height: 2,
}
return shape.width * shape.height
```

A line that ends after an operator, a comma or an opening bracket continues onto the next line because none of those tokens can end a statement. A line that ends after an identifier, a literal or a closing bracket ends the statement even if the next line begins with `.` or an operator, so leading-dot method chains are not supported.

## 1.9 Lexical errors and recovery

The lexer never stops at the first error. The recovery rules are:

- An unrecognised character (HS0005) is reported once for each code point and produces no token; lexing continues after it. A code point outside the Basic Multilingual Plane is one code point and one diagnostic.
- A malformed number, string or filter literal produces one token of its kind, flagged *malformed*, with the diagnostic already reported. The parser accepts a malformed token wherever a token of its kind is allowed. The checker gives a malformed literal the type its kind would have and reports nothing further about it.
- After a malformed string caused by a line terminator, lexing resumes at the start of the next line.

Consequently one lexical mistake produces exactly one diagnostic in the common cases:

```hollow-error
constant a = 1 😀 // HS0005
return a
```

```hollow-error
constant a = "value ${1 + // HS0007
return a
```

## 1.10 Structural limits

To keep every implementation total on hostile input, these limits are part of the language. Each violation is one diagnostic and the offending construct is parsed no further:

| Limit | Value | Diagnostic |
|---|---|---|
| Nesting depth of parentheses, brackets, braces, blocks, patterns, type brackets and interpolation braces (one combined counter) | 250 | HS0113 |
| Weight of a token: enclosing constructs plus links of enclosing chains | 1,000 | HS0113 |
| Links in one chain of a single binary operator level, one postfix chain (calls, indexes, fields, `?`), or one `else if` run | 500 | HS0114 |
| Nesting depth of string interpolations | 100 | HS0115 |
| Source size in bytes | 4,194,304 | HS0002 |
| Diagnostics reported for one file, of any severity | 100, then one HS0004 | HS0004 |
| Modules in one program, and total bytes of their source | 1,024 and 67,108,864 | HS0612 |
| Expanded size of a type ([section 2.5](02-types.md#25-record-types)) | 10,000 nodes | HS0335 |
| Rows of one `match`, elements of one list pattern, and work of the exhaustiveness check ([section 7.7](07-patterns.md#77-exhaustiveness-and-unreachable-arms)) | 1,000, 250 and 100,000 units | HS0511 |

**Depth.** The *depth* of a token is the number of open constructs that enclose it: parentheses, square brackets, braces of every kind, blocks, patterns, the angle brackets of type arguments and the `${` `}` of an interpolation, all counted by one counter. The top level of a file has depth 0. A token at depth 251 is HS0113.

**Chains.** A chain of *n* operators has *n* + 1 operands and *n* links, so `a + b` is one link and a chain of 500 links has 501 operands. A chain of 501 links is HS0114. The same count applies to an `else if` run, where each `else if` is one link.

**Weight.** Chains are left-nested in the syntax tree, so a long chain is deep as well as long. The *weight* of a token is its depth plus, for each chain that encloses it as an operand, the number of links of that chain. A token of weight above 1,000 is HS0113. Every pass over a syntax tree therefore needs at most 1,000 levels of recursion, and an implementation can size its stack for that bound.

**Widths.** The number of elements, arguments, fields, arms, statements or declarations in one list is bounded only by the source size limit. Every pass MUST take time at most near-linear in a width; the one exception is the exhaustiveness check, whose work is bounded by the last row of the table.

**Implementation limits.** An implementation MAY impose limits of its own beyond the limits this specification fixes, where the machine or the code it compiles to needs one: how many values one function can hold at once, or how long the code of one function can be, for example. Such a limit is not a limit of the language, and another implementation may accept the same program. An implementation MUST document each limit it imposes, and no such limit may reject or fault a program of the conformance corpus ([Chapter 19](19-conformance.md)). A program that exceeds one of these limits before it runs is rejected with HS0901, reported among the diagnostics of checking at the construct that exceeds the limit. A run that reaches one while it runs (for example, the memory that the active calls of a deep recursion hold) faults with HS1015 at the operation that reached it. Neither is ever reported as an internal error (HS1099).

**Diagnostics.** Up to 100 diagnostics of any severity, warnings included, are reported for one file, taken in the canonical order of [section 17.1](17-diagnostics.md#171-the-diagnostic-record), followed by exactly one HS0004, so at most 101 are reported.

Parsing MUST NOT recurse without bound on any of these, and a construct within the limits MUST NOT exhaust the host's stack. Implementations that cannot guarantee that for the limits above are not conforming.
