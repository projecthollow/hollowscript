# Type representation and interning

`hollowscript_check::types::TypeInterner` gives every type of a program a
`TypeId`: a small integer, unique within one program's interner, such that
two types are identical exactly when their ids are equal. Section 2.5 states
this requirement directly: "identity is a comparison of the same node, not a
walk of the whole type."

## Structural deduplication

The interner keeps a table from a `Shape` (the type's structure: `Int`,
`List(TypeId)`, `Record(Vec<(Symbol, TypeId)>)`, and so on) to the `TypeId`
that already stands for it. Interning a shape a second time returns the
existing id rather than creating a new one, so `Int` has exactly one id for
the whole program, and two record literals with the same fields, built
separately, share a type: this is what section 2.5's structural equality for
records requires, given directly by the data structure rather than by a
separate equality check run every time two record types are compared.

The one exception is the error type (section 2.11): every call to
`TypeInterner::error` returns a fresh, distinct id, so that an error type
introduced by one mistake never compares equal to an error type introduced by
another, and using it produces no further diagnostic (the error type is
assignable to and from everything, unconditionally, by every check that
consults `is_error`).

## Optionality does not nest

`TypeInterner::optional` implements section 2.4 directly: applying `optional`
to a type that is already `Optional(_)` returns that same id rather than
building `Optional(Optional(_))`. This holds for every optional type
regardless of how it was reached, including through generic substitution, so
a generic `T?` instantiated with `T = Int?` collapses to `Int?` without a
separate normalisation pass.

## Named types, generics, and expansion

A `record` or `enum` declaration is a `DeclId` (the declaring module and a
per-module counter) together with its parameter list and either its fields
(`DeclShape::Record`) or its variants (`DeclShape::Enum`). A use of the
declaration, `Named { decl, args }`, keeps its own type arguments as a list of
`TypeId`s rather than eagerly substituting them, which is what keeps
`Pair<Int, Int>` a single node instead of a full copy of `Pair`'s fields with
`Int` already substituted in.

`TypeInterner::expand_record` does the substitution on demand, for a record:
it walks the declaration's fields, replacing each type parameter with the
corresponding argument, and interns the result, so `Pair<Int, Int>` and
`{ left: Int, right: Int }` become the same id once expanded, matching
section 2.5's rule that a record is structural even under instantiation. An
enum is nominal instead (section 6.3): expansion is defined for it too (its
variants substitute the same way), but the checker never calls it to decide
whether two enum types are the same, only the declaration identity.

### The expanded size bound (HS0335)

Section 2.5 requires the *expanded size* of a type, the count of every
position after all aliases and type arguments are substituted, with sharing
ignored, to be bounded at 10,000 nodes, specifically so that a self-applied
generic record (`record Pair<A> = { l: A, r: A }` applied to itself
repeatedly, which doubles in size at every step) cannot describe an
astronomically large type in a few lines of source.
`TypeInterner::expanded_size` computes this count by walking the same
substitution `expand_record` does, but with a budget: every recursive call
passes down how much budget is left, and the walk stops and reports
`u32::MAX` (a saturated size, larger than the permitted limit) as soon as the
budget is spent, rather than continuing to build a number that could not be
represented usefully anyway. The count does not follow an enum's own
payloads, matching the specification's rule that the trace stops at an enum:
a cycle that passes through an enum payload is finite in the type system
even though the enum's own values can be recursively infinite in principle.

## Equatable, orderable and key types

`is_equatable` and `is_key_type` implement sections 2.8's classification
directly over the shape: a type is equatable unless it is `Filter`, a
function type, a type parameter, or contains one of those; a key type is
exactly `Int`, `Bool` or `String`. Nothing outside the interner needs to
repeat this logic, because every part of the checker that needs to know
asks the interner.

## What is not built yet

Substitution for a general (non-record) position, used when checking a
generic function body, is what `hollowscript_check::infer::Substitution` and
`unify` work over: they operate on `TypeVarId` (the unbound
variables of one call's inference, kept separate from `TypeId` because a
variable is a placeholder, not a type of the finished program) and are typed
and tested for their data structures (binding, and reporting which variables
were never bound, matching section 2.10's HS0308). The unification algorithm
itself is in `infer.rs` and runs for every generic call.
