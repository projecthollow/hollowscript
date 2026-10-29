# 18. Formal grammar

This chapter gives the complete grammar of HollowScript in one place: the lexical grammar of the tokens and the syntactic grammar over them. Where a chapter shows a fragment of the grammar, it is the same as here. The grammar of filter expressions is in [Appendix A](appendix-a-filters.md#a3-grammar).

## 18.1 Notation

The notation is an extended BNF.

- `name = expression ;` defines a rule.
- Quoted text `"if"` or `"=="` is a terminal that appears literally.
- `a b` is a followed by b; `a | b` is a or b; `( ... )` groups.
- `[ a ]` is a optional; `{ a }` is a repeated zero or more times.
- `a .. b` in the lexical grammar is a code point range.
- A comment is written `(* like this *)`.
- Names in lower camel case are non-terminals. The terminals `TERM` and `EOF` are virtual tokens.

## 18.2 Lexical grammar

The lexer works on the code points of the file ([section 1.1](01-lexical-structure.md#11-source-text)) and produces tokens by taking the longest match. Whitespace (SPACE and TAB) and comments separate tokens and are otherwise ignored.

```
whitespace   = " " | TAB ;
comment      = "//" { anyCodePointExceptLineTerminator } ;
lineTerminator = LF | CR LF ;

identifier   = ( letter | "_" ) { letter | digit | "_" } ;
letter       = "A" .. "Z" | "a" .. "z" ;
digit        = "0" .. "9" ;
hexDigit     = digit | "A" .. "F" | "a" .. "f" ;

intLiteral   = digit { [ "_" ] digit } ;
floatLiteral = intLiteral "." intLiteral [ exponent ]
             | intLiteral exponent ;
exponent     = ( "e" | "E" ) [ "+" | "-" ] intLiteral ;

stringLiteral = '"' { stringPart } '"' ;
stringPart   = stringChar | escape | interpolation ;
stringChar   = (* any code point except '"', "\", LF and CR, and except the code points
                 rejected in section 1.1 *) ;
escape       = "\" ( '"' | "\" | "n" | "t" | "r" | "$" )
             | "\u{" hexDigit { hexDigit } "}" ;
interpolation = "${" expression "}" ;

filterLiteral = "`" { filterChar | "\`" | "\\" } "`" ;
filterChar   = (* any code point except "`", "\", LF and CR, and except the code points
                 rejected in section 1.1 *) ;
```

**Keywords** (27): `and`, `assert`, `break`, `constant`, `continue`, `effect`, `else`, `enum`, `export`, `false`, `fn`, `for`, `from`, `if`, `import`, `in`, `match`, `none`, `not`, `or`, `record`, `return`, `test`, `true`, `uses`, `variable`, `while`. An `identifier` is never one of them.

**Punctuation:** `(` `)` `[` `]` `{` `}` `,` `:` `.` `..` `?` `+` `-` `*` `/` `=` `==` `!=` `<` `>` `<=` `>=`.

**Terminator insertion.** `TERM` is inserted at a line terminator and at the end of the file after an identifier, a literal (`intLiteral`, `floatLiteral`, `stringLiteral`, `filterLiteral`), `true`, `false`, `none`, `return`, `break`, `continue`, `)`, `]`, `}` or `?`, and consecutive `TERM` tokens collapse ([section 1.8](01-lexical-structure.md#18-statement-terminators)). `EOF` is the end of the token stream.

**Terminators inside brackets.** In the productions below, `TERM` tokens are not shown inside parentheses, square brackets, and the braces of record literals, record types, record patterns and enum bodies. The parser ignores every `TERM` in those places. In blocks and the braces of `match`, `TERM` is significant and shown.

## 18.3 Files and top-level items

```
file        = { usesLine } { importDecl } [ topLevelSeq ] EOF ;
usesLine    = "uses" nameList TERM ;
importDecl  = "import" nameList "from" stringLiteral TERM ;
nameList    = identifier { "," identifier } ;

topLevelSeq = topLevelItem { TERM topLevelItem } [ TERM ] ;
topLevelItem = [ "export" ] ( fnDecl | recordDecl | enumDecl | constantDecl )
             | variableDecl
             | statement ;
```

`export` before `variable` parses and is then rejected (HS0108). A `uses` line after an `import` or an item, an `import` after an item, and the declarations `fn` (with a name), `record`, `enum`, `import` and `uses` anywhere but the top level of a file are diagnosed as described in [section 4.1](04-statements.md#41-blocks-and-statements) and [section 10.1](10-modules.md#101-program-structure-of-a-file).

A top-level `fn` followed by an identifier begins a `fnDecl`; a `fn` followed by `(` begins a closure, which is an expression, so a statement may begin with a closure only in parentheses.

## 18.4 Declarations

```
fnDecl      = "fn" identifier [ "<" typeParams ">" ]
              "(" [ params ] ")" [ ":" type ] block ;
params      = param { "," param } [ "," ] ;
param       = identifier ":" type ;
typeParams  = identifier { "," identifier } [ "," ] ;

recordDecl  = "record" identifier [ "<" typeParams ">" ] "=" recordType ;

enumDecl    = "enum" identifier [ "<" typeParams ">" ]
              "{" variant { "," variant } [ "," ] "}" ;
variant     = variantName [ "(" typeList ")" ] ;
variantName = identifier ;   (* first character upper case, checked after parsing: HS0116 *)

constantDecl = "constant" bindingTarget [ ":" type ] "=" expression ;
variableDecl = "variable" bindingTarget [ ":" type ] "=" expression ;
bindingTarget = identifier | recordPattern | listPattern ;   (* a declaration pattern: see section 7.8 *)
```

## 18.5 Types

```
type        = typeAtom [ "?" ] ;
typeAtom    = "[" type "]"
            | "(" type ")"
            | fnType
            | recordType
            | namedType ;
fnType      = [ "effect" ] "fn" "(" [ typeList ] ")" [ ":" type ] ;
recordType  = "{" field { "," field } [ "," ] "}" ;
field       = identifier ":" type ;
namedType   = identifier [ "<" typeList ">" ] ;
typeList    = type { "," type } [ "," ] ;
```

A `?` after a type that already ends in `?` is HS0111. In a `fnType` the optional `: type` extends as far as possible, so `fn(Int): Int?` returns `Int?`. The `>` that closes a `namedType` must be a `>` token: `>=` is never split, so `Set<Int>= x` is HS0101. A `field` name other than `_` is checked after parsing (HS0123).

## 18.6 Statements

```
block       = "{" [ statementSeq ] "}" ;
statementSeq = statement { TERM statement } [ TERM ] ;

statement   = constantDecl
            | variableDecl
            | assignment
            | ifStmt
            | matchStmt
            | forStmt
            | whileStmt
            | "break"
            | "continue"
            | returnStmt
            | callStmt ;

assignment  = identifier "=" expression ;

ifStmt      = "if" expression block [ "else" ( ifStmt | block ) ] ;
forStmt     = "for" bindingTarget "in" expression block ;
whileStmt   = "while" expression block ;
returnStmt  = "return" [ expression ] ;

callStmt    = expression ;   (* a postfix chain, optionally marked "effect", whose last operation apart from "?" is a call; else HS0109, section 4.9 *)

matchStmt   = "match" expression "{" matchBody "}" ;
matchBody   = matchArm { TERM matchArm } [ TERM elseArm ] [ TERM ]
            | elseArm [ TERM ] ;
matchArm    = pattern { "," pattern } block ;
elseArm     = "else" block ;
```

Between the arms of a `match`, `TERM` tokens are discarded by the parser (an arm ends with `}`, which inserts one). A block that holds one statement on the same line as its braces needs no `TERM`: `{ return 1 }`.

`else` must directly follow the `}` of the previous block on the same line. `else` at the start of a line is HS0122.

**An assignment or a call statement.** A statement that begins with an identifier followed by `=` is an `assignment`; otherwise a statement that begins with an identifier, `effect`, `(` or another expression start is a `callStmt`.

## 18.7 Patterns

```
pattern     = literalPattern
            | "none"
            | "_"
            | bindingName
            | variantPattern
            | recordPattern
            | listPattern ;
literalPattern = [ "-" ] intLiteral | [ "-" ] floatLiteral
               | stringLiteral | "true" | "false" ;
bindingName = identifier ;   (* first character lower case or "_", checked after parsing: HS0120 *)
variantPattern = identifier [ "(" pattern { "," pattern } [ "," ] ")" ] ;
recordPattern  = "{" fieldPattern { "," fieldPattern } [ "," ] "}" ;
fieldPattern   = identifier [ ":" pattern ] ;
listPattern    = "[" [ listElement { "," listElement } [ "," ] ] "]" ;
listElement    = pattern | restPattern ;
restPattern    = ".." [ bindingName ] ;
```

A `pattern` that is an identifier whose first character is upper case is a `variantPattern`; all others are `bindingName`. A `stringLiteral` in a pattern must contain no interpolation. A `..` is allowed only as the last `listElement` (HS0121).

## 18.8 Expressions

```
expression  = orExpr ;
orExpr      = andExpr { "or" andExpr } ;
andExpr     = eqExpr { "and" eqExpr } ;
eqExpr      = relExpr { ( "==" | "!=" ) relExpr } ;
relExpr     = addExpr { ( "<" | ">" | "<=" | ">=" ) addExpr } ;
addExpr     = mulExpr { ( "+" | "-" ) mulExpr } ;
mulExpr     = unary { ( "*" | "/" ) unary } ;

unary       = ( "not" | "-" ) unary
            | "effect" postfix
            | postfix ;

postfix     = primary { postfixOp } ;
postfixOp   = "(" [ argList ] ")"
            | "[" expression "]"
            | "." identifier
            | "?" ;
argList     = expression { "," expression } [ "," ] ;

primary     = intLiteral
            | floatLiteral
            | stringLiteral
            | filterLiteral
            | "true"
            | "false"
            | "none"
            | identifier
            | listLiteral
            | mapLiteral
            | recordLiteral
            | closure
            | "(" expression ")" ;

listLiteral = "[" [ expression { "," expression } [ "," ] ] "]" ;
mapLiteral  = "[" ":" "]"
            | "[" mapEntry { "," mapEntry } [ "," ] "]" ;
mapEntry    = expression ":" expression ;
recordLiteral = "{" recordField { "," recordField } [ "," ] "}" ;
recordField = identifier ":" expression ;

closure     = "fn" "(" [ closureParams ] ")" [ ":" type ] block ;
closureParams = closureParam { "," closureParam } [ "," ] ;
closureParam  = identifier [ ":" type ] ;
```

All binary operators are left-associative. Levels, lowest to highest: `or`; `and`; `==` `!=`; `<` `>` `<=` `>=`; `+` `-`; `*` `/`; prefix `not` `-` `effect`; postfix call, index, field and `?`.

- **`effect`** applies to the postfix chain that follows it: `effect a.b(c)` is `effect (a.b(c))`. The meaning is in [section 9.3](09-effects-and-capabilities.md#93-the-effect-marker).
- **Restricted expressions.** In the condition of `if` and `while`, the scrutinee of `match` and the collection of `for`, a `recordLiteral` may not appear except where [section 3.8](03-expressions.md#38-list-map-and-record-literals) allows it (HS0110). A `{` there would otherwise begin the block.
- **Generics.** `<` and `>` in an expression are always comparison operators. Type arguments are never written in expressions.
- **Map or list.** In a `[` expression, the literal is a `mapLiteral` if the first expression is followed by `:`, and a `listLiteral` otherwise.
- **Interpolation.** The expression inside `${` and `}` is an `expression` in the sense of this section, lexed as described in [section 1.5](01-lexical-structure.md#15-string-literals).

## 18.9 Syntax errors and recovery

The parser never throws. It reports the errors listed in [Chapter 17](17-diagnostics.md) (HS0101 "expected ...", HS0102 "expected an expression", HS0103 "expected a statement", and the specific ones) and then recovers by this rule, so that one bad line does not cascade and every implementation reports the same number of errors. At most one syntax diagnostic is reported for each statement or top-level item. When a syntax error is found at a token `t`, the parser reports it and discards tokens starting at `t` until it reaches either a `TERM` that is not inside a bracket opened after `t`, or a `}` that is not inside a bracket opened after `t` and closes a block that was open before the statement began. It consumes that `TERM` (but not that `}`) and continues with the next statement of the enclosing block or file. Syntax errors in the discarded tokens are not reported. Malformed tokens from the lexer ([section 1.9](01-lexical-structure.md#19-lexical-errors-and-recovery)) are accepted as tokens of their kind and do not cause a parse error. Nesting and chain limits are in [section 1.10](01-lexical-structure.md#110-structural-limits).

**Which error.** HS0103 is reported when a statement begins with a token that cannot begin a statement. HS0102 is reported when the parser expects an expression (after `=`, after a binary or prefix operator, after `(`, after a `,` in an argument list or a list or map literal, after `return` when more tokens follow on the line, after `if`, `while`, `for ... in` or `match`, and after `${`) and finds a token that cannot begin one. HS0101 is reported for every other expected token (a closing delimiter, `=`, `in`, `from`, a name, a type) and names that token in `{expected}`. A block, a list or a record that is not closed before the end of the file is HS0101, with `EOF` as the token found.

```hollow-error
constant x = ) // HS0102
return 1
```

```hollow-error
) // HS0103
return 1
```

```hollow-error
constant x 1 // HS0101
return 1
```
