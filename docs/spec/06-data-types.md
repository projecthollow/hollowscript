# 6. Records, enums, lists, maps and sets

This chapter defines the data structures of the language. Library operations on lists, maps and sets are in [Chapter 14](14-stdlib-collections.md); destructuring and matching are in [Chapter 7](07-patterns.md).

All of these values are immutable. There is no way to change a record field, a list element, a map entry or a set element in place. Every operation that "changes" one returns a new value. Implementations may share structure or update a uniquely referenced value in place when that cannot be observed; the language definition never depends on it.

## 6.1 Records

```
recordDecl = "record" identifier [ "<" typeParams ">" ] "=" recordType ;
```

A record is a fixed set of named fields. A `record` declaration gives a name to a record type ([section 2.5](02-types.md#25-record-types)); the name is an alias and records are structural. A record is created with a record literal ([section 3.8](03-expressions.md#38-list-map-and-record-literals)) and its fields are read with `.`.

```hollow
record Task = { title: String, priority: Int, owner: String? }

constant task: Task = { title: "Ship", priority: 2, owner: none }
constant reassigned: Task = { title: task.title, priority: task.priority, owner: "Ada" }

return reassigned.owner == "Ada" and task.owner == none
```

The program returns `true`.

- Every field is always present. A field that may be absent has an optional type and is written `none` when absent.
- Field names are identifiers that are not keywords, and must be distinct (HS0202). A field name may start with any letter or `_`, but a field cannot be named `_` (HS0123), in a record type, a record literal or a record pattern.
- Two record values are equal when every field is equal ([section 3.5](03-expressions.md#35-comparison-and-equality)).
- To build a record that differs from another in one field, write a new literal that copies the other fields. There is no spread or update syntax.
- The record functions `keys` and `has` are in [section 13.5](13-stdlib-core.md#135-records-and-results).

A `record` declaration may be exported and imported ([Chapter 10](10-modules.md)).

## 6.2 Lists, maps and sets

**Lists** `[T]` are ordered, may contain duplicates, and are indexed from zero. `xs[i]` has type `T?` and is `none` when `i` is negative or not less than the length ([section 3.7](03-expressions.md#37-calls-fields-methods-and-indexing)). A list literal is written `[a, b, c]`.

**Maps** `Map<K, V>` associate keys with values. The key type `K` must be `Int`, `String` or `Bool` ([section 2.8](02-types.md#28-equatable-and-orderable-types)). A map has at most one entry per key. Its entries are always ordered by key, in ascending order: `Int` keys numerically, `String` keys by code point sequence (the order of their UTF-8 bytes), and `Bool` keys with `false` before `true`. That order is what `for`, `keys`, `values` and `entries` follow, what equality ignores, and what makes the result of any program independent of the order in which entries were added. A map literal is `["a": 1, "b": 2]`; the empty map is `[:]` and needs an expected type. `m[k]` has type `V?`. Operations are in [section 14.2](14-stdlib-collections.md#142-maps).

**Sets** `Set<T>` contain each element once. The element type must be `Int`, `String` or `Bool`. Elements are ordered ascending as for map keys. A set is created with the library function `set(items)` ([section 14.3](14-stdlib-collections.md#143-sets)); there is no set literal.

```hollow
uses print

constant scores = ["grace": 5, "ada": 3]
constant more = scores.with("linus", 4)
for entry in more {
    effect print("${entry.key}=${entry.value}")
}
effect print("${set([3, 1, 2, 3]).length()}")
```

Output:

```text
ada=3
grace=5
linus=4
3
```

## 6.3 Enums

```
enumDecl = "enum" identifier [ "<" typeParams ">" ]
           "{" variant { "," variant } [ "," ] "}" ;
variant  = variantName [ "(" typeList ")" ] ;
```

An enum is a type whose values are one of a fixed set of *variants*, each of which may carry a fixed number of *payload* values of declared types. Enums are nominal: an enum type is identified by the file that declares it and its name, together with its type arguments if it is generic. Two enums with the same variants are different types.

- A variant name must start with an upper-case ASCII letter (HS0116). Variant names within one enum must be distinct (HS0202). An enum needs at least one variant (HS0112).
- A payload is a list of types in parentheses: `Rect(Float, Float)`. There are no named payload fields; a variant that needs names carries a record: `Named({ first: String, last: String })`.
- An enum may refer to itself in its payloads, directly or through other declarations, which is how recursive data is written. (Records may not.)
- An enum may be generic ([section 2.10](02-types.md#210-generics)).

**Construction.** A variant is named through the enum: `Shape.Circle(1.5)` builds a value of type `Shape`, and a variant without a payload is written without parentheses: `Shape.Empty`. The enum name is in the value namespace for this purpose ([section 2.1](02-types.md#21-namespaces)). A variant with a payload used without a call, `Shape.Circle`, is a pure function value whose parameters are the payload types and whose result is the enum type. An unknown variant is HS0333; a payload of the wrong arity is HS0319; calling a variant without a payload, `Shape.Empty()`, is HS0318. For a generic enum whose type arguments cannot be determined from the payload or the expected type, as in `constant x = Maybe.Nothing`, the checker reports HS0308.

**Use.** The only way to look inside an enum value is `match` ([Chapter 7](07-patterns.md)), where variants are written without the enum name because the scrutinee's type is known. Enum values can be compared with `==` when all payload types are equatable, and cannot be ordered or interpolated. They have no methods; `Result` is the one built-in enum that does ([Chapter 8](08-errors-and-results.md)).

```hollow
enum Shape {
    Circle(Float),
    Rect(Float, Float),
    Empty,
}

fn area(s: Shape): Float {
    match s {
        Circle(r) {
            return 3.0 * r * r
        }
        Rect(w, h) {
            return w * h
        }
        Empty {
            return 0.0
        }
    }
}

return area(Shape.Rect(2.0, 4.5)) + area(Shape.Circle(2))
```

The program returns `21.0`: the rectangle contributes `9.0`, and the circle with the `Int` argument `2` (converted to `2.0` at the call) contributes `3.0 * 2.0 * 2.0`, which is `12.0`.

```hollow
enum Tree {
    Leaf,
    Node(Tree, Int, Tree),
}

fn sum(t: Tree): Int {
    match t {
        Leaf {
            return 0
        }
        Node(left, value, right) {
            return sum(left) + value + sum(right)
        }
    }
}

constant tree = Tree.Node(Tree.Node(Tree.Leaf, 1, Tree.Leaf), 2, Tree.Leaf)
return sum(tree)
```

The program returns `3`.

```hollow
enum Maybe<T> {
    Nothing,
    Just(T),
}

fn orElse<T>(m: Maybe<T>, fallback: T): T {
    match m {
        Just(v) {
            return v
        }
        Nothing {
            return fallback
        }
    }
}

constant a: Maybe<Int> = Maybe.Just(4)
constant b: Maybe<Int> = Maybe.Nothing
return orElse(a, 0) + orElse(b, 10)
```

The program returns `14`.

An `enum` declaration may be exported and imported with its variants ([Chapter 10](10-modules.md)).

## 6.4 Sizes and ordering summary

| Type | Equality | Ordering `<` | Can be a key | Iteration order |
|---|---|---|---|---|
| `Int` | numeric | numeric | yes | n/a |
| `Float` | numeric | numeric | no | n/a |
| `Bool` | same value | no | yes | n/a |
| `String` | same code points | by code point | yes | n/a |
| `[T]` | elementwise | no | no | index order |
| `Map<K, V>` | same keys and values | no | no | ascending key |
| `Set<T>` | same elements | no | no | ascending element |
| record | fieldwise | no | no | n/a |
| enum | same variant and payload | no | no | n/a |
