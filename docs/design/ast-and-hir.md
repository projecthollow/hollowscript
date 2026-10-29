# The syntax tree and the typed HIR

There are two trees, owned by two crates, and the boundary between them is
deliberate.

## Why two trees

The syntax tree (`hollowscript_syntax::ast`) is a direct transcription of the
grammar: every node corresponds to something the parser matched, spans point
at real source text, and a name is still just text. It exists so that the
parser, the formatter and the language server can work without knowing
anything about types, capabilities or generics: the formatter in particular
must reproduce exactly what a program means syntactically, including a
parenthesised expression that changes nothing semantically but must not be
dropped.

The HIR (`hollowscript_check::hir`) is what checking produces: the same
program after every name is resolved to a binding, every expression carries
its type, every generic call is resolved to one instantiation, and every
implicit coercion of section 2.7 is a real node rather than something a later
pass has to re-derive from types. The compiler reads only the HIR. It never
looks at the syntax tree and never does its own name lookup, which is what
makes "the checker accepted this program" and "the compiler can compile it"
the same guarantee: if the HIR is well formed, compilation cannot fail for a
semantic reason, only for a defect in the compiler itself.

Keeping them separate, rather than annotating the syntax tree in place, means
a syntax error and a type error are answered by two different trees that
never have to agree on a shape budget: the syntax tree tolerates
[`ExprKind::Error`](../../crates/hollowscript-syntax/src/ast.rs)-style nodes
for recovery, and the HIR tolerates its own `ExprKind::Error` for a checking
failure, and neither has to carry the other's escape hatch.

## What every node keeps

Both trees give every node an id (`NodeId` in the syntax tree) and a span, or,
in the HIR, the originating `NodeId` rather than a repeated span: a
diagnostic or a fault frame can always point at real source text without the
HIR duplicating position information the syntax tree already has.

## The syntax tree in outline

`hollowscript_syntax::ast` defines, in full, with every field documented:

- `File`: the `uses` lines, `import` lines and top-level items of one module,
  in the three-block order section 10.1 requires.
- `Item` and `ItemKind`: `fn`, `record`, `enum`, top-level `constant` and
  `variable`, and any other statement, each keeping the `export` span when present.
- `Stmt` and `StmtKind`: every statement of chapter 4, including `If` with its
  `else if` chain represented as a linked `Else` rather than nested blocks (so
  a 500-link chain, the limit of section 1.10, does not need 500 levels of
  tree depth to represent, only to walk).
- `Expr` and `ExprKind`: every expression of chapter 3, with `Unary`,
  `Binary`, `Effect`, `Call`, `Index`, `Field` and `Try` kept as distinct
  variants rather than a single generic "operation" node, so a pass can match
  on the shape it cares about without a secondary tag.
- `Pattern` and `PatternKind`: every pattern of chapter 7.
- `TypeExpr` and `TypeKind`: type syntax as written, including `Paren` (kept
  so `(Int?)?` can be diagnosed as HS0111 and reproduced by the formatter)
  and `Error` for a type that failed to parse.
- `Literal` and `StringLiteral`/`StringPart`: number and string literals,
  with a string cut into text and interpolation parts so that no later pass
  has to re-lex a string to find its interpolations.

## The HIR in outline

`hollowscript_check::hir` defines:

- `Program`, `Module`, `Global` and `Function`: the whole checked program, its
  modules in initialisation order (section 10.5), and every top-level binding
  and function, each carrying its resolved type.
- `LocalId`, `GlobalId`, `FunctionId` and `CapabilityId`: small integer
  identities in place of names, assigned once during checking, so the
  compiler indexes a fixed-size table instead of looking a name up again.
- `Capture`: how a closure reaches a binding of an enclosing scope, one of a
  local of the immediately enclosing function, an upvalue of the immediately
  enclosing closure (forwarded), or a top-level binding by its global
  identity. The last case exists because section 11.4 counts a captured
  top-level binding once, as a top-level binding, not once per closure that
  captures it, and the HIR keeps that distinction explicit rather than
  leaving the compiler to rediscover it.
- `Stmt`/`StmtKind`, `Pattern`/`PatternKind`, `Expr`/`ExprKind`: the checked
  counterparts of the syntax tree's kinds, each now typed and resolved.
  `ExprKind::IntToFloat` makes every coercion site of section 2.7 an explicit
  node, and `ExprKind::NativeCall` resolves a standard library call to its
  position in `hollowscript_core::native::ROSTER` rather than leaving it a
  general call the compiler must re-recognise.
- `Path` and `Fact`: the narrowing vocabulary of section 4.8, kept in the HIR
  itself (`StmtKind::If`'s `then_facts` and `else_facts`) so that the
  compiler never has to re-run narrowing to know which paths are safe to
  read unwrapped. A `PathField` carries both the field's name and its
  position, because the checker knows the position while it builds the path
  and nothing downstream can recover it: a `TypeId`'s field list is not
  reachable outside `hollowscript_check::types`, so a compiler given only
  the name could not emit the field read.
- `Match`: the scrutinee, its arms, and whether an `else` arm was needed for
  exhaustiveness, which is a fact the exhaustiveness algorithm computes once,
  during checking, and the HIR simply records.

## What is not decided yet

The HIR shape above is fixed and the algorithms that fill it in are written,
each in its own module of `hollowscript-check` (`resolve.rs`, `infer.rs`,
`narrow.rs`, `effects.rs`, `exhaustive.rs`).

One shape the HIR does not have yet: a declaration whose target is a pattern
(`constant { x, y } = p`) has no representation at the top level of a
module, because a global is one `GlobalId` per name and nothing builds one
slot per binding of a pattern. Inside a function it works, because a local
declaration lowers the pattern directly. A qualified variant pattern
(`Shape.Rectangle(w, h)`) does not reach the HIR either; the parser rejects
it before the checker sees it.
