# 14. Standard library: lists, maps and sets

The conventions of [section 13.1](13-stdlib-core.md#131-conventions) apply: notation, numeric arguments, cost and faults. Every operation returns a new value; none changes its receiver. In the tables `T` is the element type of a list or set, `K` and `V` the key and value types of a map, and `N` the number of elements of the receiver unless stated. Where the result is a new collection its size is charged to the memory counter before it is built, as [section 11.4](11-resource-limits.md#114-memory) states (`append`, `with` and `without` are charged for what they add), whatever an implementation does internally.

Callbacks are pure function values ([section 9.4](09-effects-and-capabilities.md#94-effect-in-function-types)) and are called in the order stated, on the elements as they are at the time of the call. Each function calls its callback at most once per element, except `sort(compare)`, which calls the comparator exactly as the algorithm of [section 14.1](#141-lists) does. A fault in a callback is a fault of the whole call.

## 14.1 Lists

Methods of `[T]`:

| Signature | Meaning | Cost |
|---|---|---|
| `length(): Int` | number of elements | 1 |
| `isEmpty(): Bool` | whether there are no elements | 1 |
| `first(): T?` | the first element, or `none` if empty | 1 |
| `last(): T?` | the last element, or `none` if empty | 1 |
| `contains(item: T): Bool` | whether some element is `==` to `item`; `T` must be equatable (HS0330) | n (N x (1 + t)) |
| `indexOf(item: T): Int?` | the index of the first element `==` to `item`, or `none`; `T` must be equatable (HS0330) | n (N x (1 + t)) |
| `append(item: T): [T]` | the list with `item` added at the end | n |
| `concat(other: [T]): [T]` | this list followed by `other` | n (length of the result) |
| `slice(start: Int, upTo: Int): [T]` | the elements from `start` (inclusive) to `upTo` (exclusive); both are clamped into `0 .. length`; if `start >= upTo` the result is `[]` | n (length of the result) |
| `take(count: Int): [T]` | the first `count` elements; all of them if `count` is at least the length; `[]` if `count <= 0` | n (length of the result) |
| `drop(count: Int): [T]` | all but the first `count` elements; `[]` if `count` is at least the length; the whole list if `count <= 0` | n (length of the result) |
| `reverse(): [T]` | the elements in reverse order | n |
| `sort(): [T]` | ascending natural order; `T` must be `Int`, `Float` or `String` (HS0330) | n log n, see below |
| `sort(compare: fn(T, T): Int): [T]` | sorted by the comparator, any `T` | n log n, see below |
| `map<U>(transform: fn(T): U): [U]` | the results of calling `transform` on each element in order | n + callbacks |
| `flatMap<U>(transform: fn(T): [U]): [U]` | the concatenation of the lists returned for each element in order | n + callbacks |
| `filter(keep: fn(T): Bool): [T]` | the elements for which `keep` returns `true`, in order | n + callbacks |
| `find(matches: fn(T): Bool): T?` | the first element for which `matches` returns `true`, or `none`; stops calling at the first match | n + callbacks |
| `any(matches: fn(T): Bool): Bool` | whether `matches` is `true` for some element; stops at the first `true` | n + callbacks |
| `all(matches: fn(T): Bool): Bool` | whether `matches` is `true` for every element; stops at the first `false` | n + callbacks |
| `each(action: fn(T))` | calls `action` on each element in order | n + callbacks |
| `reduce<A>(initial: A, combine: fn(A, T): A): A` | starting with `initial`, replaces the accumulator by `combine(accumulator, element)` for each element in order and returns it | n + callbacks |
| `join(separator: String): String` | the elements, all `String`, separated by `separator` (HS0330 if `T` is not `String`); `[]` gives `""` | n (bytes of the result) |

The cost `n` for a list method is `1 + floor(N / 64)` where `N` is the number of elements of the receiver, except where the table says otherwise; sorting is charged for `N * ceil(log2(N + 1))` element steps, that is `1 + floor(N * ceil(log2(N + 1)) / 64)`. In `contains` and `indexOf`, `t` is the tree size of `item` ([section 11.4](11-resource-limits.md#114-memory)), so the charge covers the comparisons whatever their result. Callback calls are charged as calls of closures ([section 11.3](11-resource-limits.md#113-steps)); the cost of `sort` does not include the calls of the comparator, each of which is charged as a call of a closure.

**Sorting.** Both forms of `sort` are *stable*: elements that are equal, or that the comparator places at zero, keep their input order. The natural order is numeric for `Int` and `Float` (with `Int` and `Float` never mixed in one list) and code point order for `String`. `sort(compare)` calls `compare(a, b)` and interprets a negative result as "`a` before `b`", zero as "keep the order", positive as "`b` before `a`". To make the calls to `compare` the same on every implementation, the algorithm is fixed as a top-down merge sort:

```
sort(a):
    if length(a) <= 1: return a
    m = floor(length(a) / 2)
    left  = sort(a[0 .. m))
    right = sort(a[m .. length(a)))
    result = []
    i = 0; j = 0
    while i < length(left) and j < length(right):
        if compare(left[i], right[j]) <= 0: append left[i] to result; i = i + 1
        else: append right[j] to result; j = j + 1
    append the remaining elements of left, then of right, to result
    return result
```

Recursive calls run first on the left half, then on the right half. A comparator that is not a consistent ordering (for example one that is not transitive) still gives a deterministic result, which is the one this algorithm produces.

```hollow
uses print

constant xs = [3, 1, 2]
effect print("${xs.length()} ${xs.isEmpty()} ${xs.contains(2)} ${xs.contains(7)}")

constant empty: [Int] = []
effect print("${xs.first() == 3} ${xs.last() == 2} ${empty.first() == none} ${empty.isEmpty()}")

effect print(xs.append(4).concat([9, 8]).reverse().map(fn(n) { return "${n}" }).join(" "))
effect print(xs.sort().map(fn(n) { return "${n}" }).join(" "))

constant items = ["bb", "a", "cc", "d"]
constant byLength = items.sort(fn(a, b) { return a.length() - b.length() })
effect print(byLength.join(","))
```

Output:

```text
3 false true false
true true true true
8 9 4 2 1 3
1 2 3
a,d,bb,cc
```

The type of `reduce` is fixed by its first argument, and a callback that returns a wider type does not change it ([section 2.10](02-types.md#210-generics)):

```hollow-error
constant xs: [Float] = [1.5, 2.5]
constant total = xs.reduce(0, fn(a, x) { return a + x }) // HS0301
return total
```

The accumulator must be written as a `Float`: `xs.reduce(0.0, ...)`.

## 14.2 Maps

`Map<K, V>` is an immutable map from key type `K` (`Int`, `String` or `Bool`) to values of type `V`, iterated in ascending key order ([section 6.2](06-data-types.md#62-lists-maps-and-sets)). A map literal is `["a": 1]`; `m[k]` and `m.get(k)` are the same operation. `MapEntry<K, V>` is the prelude record type `{ key: K, value: V }`.

Methods of `Map<K, V>` (`N` is the number of entries, and `N'` that of the argument):

| Signature | Meaning | Cost |
|---|---|---|
| `length(): Int` | number of entries | 1 |
| `isEmpty(): Bool` | whether there are no entries | 1 |
| `get(key: K): V?` | the value for `key`, or `none` if absent | 1 |
| `has(key: K): Bool` | whether `key` is present | 1 |
| `with(key: K, value: V): Map<K, V>` | the map with `key` set to `value`, replacing any existing entry | n (entries of the result) |
| `without(key: K): Map<K, V>` | the map without `key`; unchanged if `key` is absent | n |
| `merge(other: Map<K, V>): Map<K, V>` | the entries of this map with the entries of `other` added, `other` winning for keys in both | n (N + N', the entries of both maps) |
| `keys(): [K]` | the keys in ascending order | n |
| `values(): [V]` | the values in ascending key order | n |
| `entries(): [MapEntry<K, V>]` | the entries in ascending key order | n |
| `each(action: fn(K, V))` | calls `action` for each entry in ascending key order | n + callbacks |
| `filter(keep: fn(K, V): Bool): Map<K, V>` | the entries for which `keep` returns `true`; called in ascending key order | n + callbacks |
| `mapValues<W>(transform: fn(V): W): Map<K, W>` | the same keys with `transform` applied to each value, called in ascending key order | n + callbacks |

Free function:

| Signature | Meaning | Cost |
|---|---|---|
| `mapFrom(entries: [MapEntry<K, V>]): Map<K, V>` | a map from a list of entries; if a key occurs more than once, the later entry wins; `K` must be a key type (HS0306) | n (length of `entries`) |

If `V` is an optional type, `get` returns that same optional type, and an absent key cannot be told from a present key whose value is `none`; use `has` to tell them apart.

```hollow
uses print

constant m = ["b": 2, "a": 1]
constant merged = m.merge(["a": 10, "c": 3])
effect print("${merged.keys().join(",")} ${merged.values().length()} ${merged.get("a") == 10} ${merged.has("z")}")

constant built = mapFrom([{ key: 1, value: "one" }, { key: 1, value: "uno" }, { key: 2, value: "two" }])
effect print("${built.length()} ${built.get(1) == "uno"}")
```

Output:

```text
a,b,c 3 true false
2 true
```

## 14.3 Sets

`Set<T>` is an immutable set of elements of key type `T` (`Int`, `String` or `Bool`), iterated in ascending order. There is no set literal.

Free function:

| Signature | Meaning | Cost |
|---|---|---|
| `set(items: [T]): Set<T>` | the set of the elements of `items`, ignoring duplicates; `T` must be a key type (HS0307) | n (length of `items`) |

Methods of `Set<T>` (`N` is the number of elements, and `N'` that of the argument):

| Signature | Meaning | Cost |
|---|---|---|
| `length(): Int` | number of elements | 1 |
| `isEmpty(): Bool` | whether there are no elements | 1 |
| `has(item: T): Bool` | whether `item` is in the set | 1 |
| `with(item: T): Set<T>` | the set with `item` added | n (elements of the result) |
| `without(item: T): Set<T>` | the set without `item` | n |
| `union(other: Set<T>): Set<T>` | elements in either set | n (N + N') |
| `intersect(other: Set<T>): Set<T>` | elements in both sets | n (N + N') |
| `difference(other: Set<T>): Set<T>` | elements of this set that are not in `other` | n (N + N') |
| `isSubsetOf(other: Set<T>): Bool` | whether every element of this set is in `other` | n (N + N') |
| `toList(): [T]` | the elements in ascending order | n |
| `each(action: fn(T))` | calls `action` for each element in ascending order | n + callbacks |
| `filter(keep: fn(T): Bool): Set<T>` | the elements for which `keep` returns `true`, called in ascending order | n + callbacks |

```hollow
uses print

constant a = set([1, 2, 3])
constant b = set([3, 4])
effect print("${a.union(b).length()} ${a.intersect(b).toList().first() == 3} ${a.difference(b).length()} ${a.has(2)}")
```

Output:

```text
4 true 2 true
```

## 14.4 Ordering of keys and elements

Wherever this chapter says *ascending*, the order is: `Int` numerically; `String` by comparing code point sequences, which is the same as comparing their UTF-8 bytes, a prefix sorting before a longer string; `Bool` with `false` before `true`. There is no other iteration order for maps and sets, and equality of maps and sets does not depend on how they were built.
