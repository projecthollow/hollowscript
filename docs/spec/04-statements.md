# 4. Statements and control flow

HollowScript is statement oriented. `if`, `match`, `for` and `while` are statements, blocks are not expressions and do not yield a value, and a value leaves a function only through an explicit `return`.

## 4.1 Blocks and statements

A *block* is `{` followed by zero or more statements and `}`. Statements in a block are separated by statement terminators ([section 1.8](01-lexical-structure.md#18-statement-terminators)). The statements are:

- declarations: `constant` and `variable` ([section 4.2](#42-declarations));
- assignment ([section 4.3](#43-assignment));
- `if` ([section 4.4](#44-conditionals)) and `match` ([Chapter 7](07-patterns.md));
- `for` and `while`, with `break` and `continue` ([section 4.5](#45-loops));
- `return` ([section 4.7](#47-return-break-continue-and-reachability));
- an expression statement, which is a call ([section 4.9](#49-expression-statements)).

Declarations of `fn`, `record` and `enum`, and `uses`, `import` and `export`, are top-level items only ([Chapter 10](10-modules.md)); anywhere else they are HS0117. A local function is written as a closure bound to a `constant`.

Execution of a block runs its statements in order. A block that is the body of a `fn` or closure ends when a `return` runs or the last statement completes.

## 4.2 Declarations

```
constantDecl = "constant" bindingTarget [ ":" type ] "=" expression ;
variableDecl = "variable" bindingTarget [ ":" type ] "=" expression ;
bindingTarget = identifier | recordPattern | listPattern ;
```

`constant` binds a name once. `variable` binds a name that can later be assigned ([section 4.3](#43-assignment)). Neither makes a *value* editable: every value in HollowScript is immutable, and there is no field assignment, no element assignment and no mutating method. `variable` differs from `constant` only in that the name can be rebound.

```hollow
variable count = 0
count = count + 1

constant task = { title: "Ship", priority: 2 }
constant renamed = { title: "Ship it", priority: task.priority }

return count + renamed.priority
```

The initialiser is evaluated first, and the name is in scope only after the declaration, so `constant x = x + 1` is HS0205. If there is an annotation, the initialiser is a coercion site for it ([section 2.7](02-types.md#27-assignability-and-numeric-coercion)); if there is none, the type is inferred ([section 2.9](02-types.md#29-inference)).

**Destructuring.** A binding target may be a record pattern or a list pattern, which bind parts of a value: `constant { x, y } = point` declares constants `x` and `y`, and `constant [first, ..rest] = items` declares `first` and `rest`; with `variable` it declares variables. Only *declaration patterns* are allowed here: names, `_`, and record and list patterns made of them (HS0509 otherwise). A record pattern always matches a record of its type; a list pattern that does not match the length of the list faults ([section 7.8](07-patterns.md#78-destructuring-declarations-and-for)). Patterns are specified in [Chapter 7](07-patterns.md).

```hollow
record Point = { x: Int, y: Int }

constant origin: Point = { x: 0, y: 0 }
constant { x, y: height } = origin
return x + height
```

The declared names are visible from the end of the declaration. Names that begin with `_` are exempt from the unused-name warning ([section 17.3](17-diagnostics.md#173-warnings)).

## 4.3 Assignment

```
assignment = identifier "=" expression ;
```

An assignment rebinds a name that was declared with `variable`. Assigning to anything else (a `constant`, a parameter, a loop variable, a pattern binding, an imported name, a capability, a function) is HS0206. The right-hand side is a coercion site for the variable's type, so a `Float` variable may be assigned an `Int`, which is converted; any other type mismatch is HS0301. There is no assignment to a field or element and there are no compound assignment operators.

A closure may assign to a `variable` of an enclosing scope; the closure and the enclosing code share one binding ([section 5.4](05-functions-and-closures.md#54-capture)).

```hollow
variable f = 1.5
f = 2
return f
```

The program returns the `Float` `2.0`.

## 4.4 Conditionals

```
ifStmt = "if" expression block [ "else" ( ifStmt | block ) ] ;
```

The condition must have type `Bool` (HS0316). The first block runs if the condition is `true`; otherwise the `else` block, if there is one, runs. `else if` chains are `else` followed by an `if` statement and may be as long as the limit of [section 1.10](01-lexical-structure.md#110-structural-limits) allows (500 links, HS0114); an implementation MUST process them iteratively, not recursively. The `else` keyword must be on the same line as the `}` before it ([section 1.8](01-lexical-structure.md#18-statement-terminators)).

```hollow
fn describe(n: Int): String {
    if n < 0 {
        return "negative"
    } else if n == 0 {
        return "zero"
    } else {
        return "positive"
    }
}

return describe(-4)
```

Record literals are restricted in the condition ([section 3.8](03-expressions.md#38-list-map-and-record-literals)).

## 4.5 Loops

```
forStmt   = "for" bindingTarget "in" expression block ;
whileStmt = "while" expression block ;
```

**`for`** evaluates its collection expression once, then runs the block once per element in order, binding the loop variable to the element as a fresh `constant` in each iteration. The collection must be:

- a list `[T]`: elements in index order, the variable has type `T`;
- a `Set<T>`: elements in ascending order ([section 14.3](14-stdlib-collections.md#143-sets)), the variable has type `T`;
- a `Map<K, V>`: entries in ascending key order, the variable has type `MapEntry<K, V>`, a record with fields `key` and `value` ([section 14.2](14-stdlib-collections.md#142-maps)).

Anything else is HS0325. The loop variable may be a record pattern that binds fields of an element that is a record (`for { name, age } in people`). To count, iterate `range(from, upTo)`.

**`while`** evaluates its condition, which must be `Bool` (HS0316), and if it is `true` runs the block and evaluates the condition again; it stops when the condition is `false`.

**`break`** leaves the innermost enclosing loop, and **`continue`** skips to the next iteration of it. They take no label. They apply to a loop in the same function or closure: a `break` or `continue` that has no enclosing loop within its own function, closure or top level is HS0344, even when the function is called from inside a loop.

```hollow
variable total = 0
for n in range(0, 10) {
    if n == 7 {
        break
    }
    if mod(n, 2) == 0 {
        continue
    }
    total = total + n
}
return total
```

The program returns `9` (that is 1 + 3 + 5).

Every iteration of a loop, and every statement, counts against the step limit ([Chapter 11](11-resource-limits.md)); a loop with no exit runs until a limit is reached or the host cancels it.

## 4.6 Scope and shadowing

**Block scope.** A name is visible from the end of its declaration to the end of the innermost block that contains it. The parameters of a function or closure are visible in its body; the loop variable of a `for` and the bindings of a `match` arm are visible in that loop body or arm. Top-level `fn`, `record` and `enum` declarations are visible throughout their file ([section 4.10](#410-top-level-declaration-order)).

**No shadowing.** Declaring a name that is already visible, in any enclosing scope or in the same scope, is an error (HS0201), with a related span naming the first declaration. This applies uniformly to constants, variables, parameters, closure parameters, loop variables, pattern bindings, imports, capability names, top-level functions and enum names, and, in the type namespace, to record names, imported type names and type parameters ([section 2.1](02-types.md#21-namespaces)). Two sibling blocks may each declare the same name, because neither is visible inside the other.

**Top-level names in function bodies.** For the purposes of this rule, every top-level `fn`, `record`, `enum`, `constant` and `variable` name and every import of a file is visible in every function body of that file, wherever it is declared ([section 4.10](#410-top-level-declaration-order)). A parameter, local, loop variable or pattern binding in a function body whose name is that of a top-level declaration of its file is therefore HS0201, with the top-level declaration as the related span, even when that declaration comes later in the file. Outside function bodies, in top-level statements and in closures written there, only the names declared earlier are visible.

```hollow
constant total = 10

fn report(limit: Int): Int {
    if limit > 0 {
        constant label = 1
        return label
    } else {
        constant label = 2
        return label + total
    }
}

return report(1)
```

**The discard name.** `_` is not subject to HS0201 and cannot be read ([section 7.3](07-patterns.md#73-bindings-and-the-discard-name)).

**The prelude is the one exception.** A name from the prelude ([section 2.1](02-types.md#21-namespaces)) may be declared by a program, apart from the built-in types that section names, and the declaration then hides the prelude name from the place where the declaration's own name comes into scope:

- for a top-level `fn`, `record` or `enum` and for an import, in the whole file, including code above the declaration, because these are visible throughout their file;
- for a top-level `constant` or `variable`, from its declaration to the end of the file and in every function body of the file;
- for every other declaration (a local, a parameter, a closure parameter, a loop variable, a pattern binding), from its scope start to the end of its scope.

An initialiser is checked before the name comes into scope, so `constant max = max(3, 2)` calls the prelude `max`, and code above a top-level `constant` that hides a prelude name still refers to the prelude name.

**Hiding changes name lookup in the declaring file only.** Built-in functions, methods, capabilities and the `?` operator always use the prelude entities. A program that hides a prelude type such as `JsonError` declares a distinct type of the same name, and values of the two are not assignable to each other. There is no way to name a hidden prelude entity in that file.

This exception exists so that new prelude names added in later minor versions of the language can never break an existing program.

```hollow
fn min(a: Int, b: Int): Int {
    if a < b {
        return a
    }
    return b
}

return min(3, 2)
```

```hollow
constant max = max(3, 2)
return max
```

The second program returns `3`.

**Capabilities.** A capability name cannot be redeclared or hidden by anything in the file, including by an import, and there is no exception for it: HS0201.

**Types and values.** Type names are in a separate namespace ([section 2.1](02-types.md#21-namespaces)), so a variable may be called `result` or `filter`. A type parameter is visible in its declaration only. The no-shadowing rule applies to type names in the type namespace ([section 2.1](02-types.md#21-namespaces)).

**Duplicates.** Each of the following is an error, reported on the second occurrence with a related span for the first: a repeated name in one parameter list (HS0201); a repeated name in one `uses` or `import` clause, or across clauses (HS0201); a repeated field in a record type or a repeated variant in an enum (HS0202); a repeated field name in a record literal or a repeated literal key in a map literal (HS0203); a name bound twice in one pattern (HS0204).

**Unused names.** An unused declaration or import is a warning (HS0801, HS0802); the exact rule is in [section 17.3](17-diagnostics.md#173-warnings).

## 4.7 Return, break, continue and reachability

```
returnStmt = "return" [ expression ] ;
```

Inside a `fn` or closure, `return e` ends the call and yields the value of `e`, which is a coercion site for the return type. `return` without an expression ends the call of a function that has no return type. `return e` in a function with no return type is HS0340; `return` without a value in a function that declares a return type is HS0341. A function or closure that declares a return type must return a value on every path (HS0342). A block *always returns* if one of its statements always returns, where:

- a `return` statement always returns;
- an `if` statement with an `else` always returns if both blocks do (an `else if` chain counts by the same rule);
- a `match` statement always returns if every arm's block does.

No other statement always returns; in particular a loop is treated as if it might finish without returning. So a function whose last statement is `while true { ... }` still needs a `return` after it, which is not an unreachable statement.

**Unreachable statements.** A statement that follows a `return`, `break` or `continue` in the same block is HS0343. The check is purely syntactic and applies only to that one situation; it is reported once, on the first unreachable statement of the block, and does not suppress the analysis of the function's other paths.

**Top-level return.** At the top level of a file, `return e` ends the file's initialisation and, for the entry file, yields the value to the host as the result of the run ([Chapter 12](12-instances.md)). A top-level `return` in any other module is HS0611. A top-level `return` without an expression ends the initialisation of the entry module in the same way and yields no result; the instance is Finished ([section 12.2](12-instances.md#122-states)). A script that does not execute a top-level `return` produces no result. The value of a top-level `return` may have any type, including different types in different branches, because the host receives it dynamically typed. A top-level `?` that meets `Err(x)` ends initialisation with the result `Result.Err(x)` ([section 8.3](08-errors-and-results.md#83-the--operator)); the result of a run is not checked against the types of any other top-level `return`.

## 4.8 Optional narrowing

A value of type `T?` cannot be used where `T` is expected until it has been *narrowed*. Narrowing is a static analysis with no run-time cost. It is sound: an accepted program never finds a `none` where narrowing said there was a value.

**Paths.** A *path* is an identifier, or an identifier followed by one or more field accesses: `x`, `task.owner`, `a.b.c`. The *root* of a path is its first identifier. Narrowing applies to paths whose type is optional.

**Facts.** At each point of a function the checker holds a set of *facts*, each a path known not to be `none` at that point. A path that is in the fact set is treated as having its inner type: `x` of type `Int?` is used as `Int`. A fact is removed when its root is assigned (`x = e` removes every fact whose root is `x`).

**Truth sets.** For a condition `c`, let `T(c)` be the facts that hold when `c` evaluates to `true`, and `F(c)` the facts that hold when it evaluates to `false`:

| Condition `c` | `T(c)` | `F(c)` |
|---|---|---|
| `p != none` or `none != p` (p a path of optional type) | `{ p }` | `{ }` |
| `p == none` or `none == p` | `{ }` | `{ p }` |
| `not a` | `F(a)` | `T(a)` |
| `a and b` | `T(a)` together with `T(b)` | `F(a)` intersected with (`T(a)` together with `F(b)`) |
| `a or b` | `T(a)` intersected with (`F(a)` together with `T(b)`) | `F(a)` together with `F(b)` |
| `true`, `false`, a parenthesised condition, anything else | as for the inner condition, or `{ }` | as for the inner condition, or `{ }` |

The right operand of `and` is checked with the facts in force plus `T(a)`, and the right operand of `or` with the facts in force plus `F(a)`, so `a != none and a.x > 0` and `a == none or a.x > 0` are both valid.

**Statements.**

- `if c { A } else { B }`: block `A` is checked with the incoming facts plus `T(c)`; block `B` with the incoming facts plus `F(c)`. Without an `else`, the implicit empty else branch has the incoming facts plus `F(c)`. After the statement the facts are the intersection of the facts at the end of every branch that can complete normally. A branch *cannot complete normally* if it always exits, meaning its last reachable statement is `return`, `break` or `continue`, or an `if`/`else` all of whose branches cannot complete normally, or a `match` all of whose arms cannot. If no branch can complete normally, code after the statement is dead and the facts are the incoming facts. This is what makes guard clauses work:

```hollow
record Task = { title: String, owner: String? }

fn ownerName(task: Task): String {
    if task.owner == none {
        return "unassigned"
    }
    return task.owner
}

return ownerName({ title: "Ship", owner: "Ada" })
```

- `match`: see [section 7.6](07-patterns.md#76-narrowing-in-match).
- `while c { body }`: let `A` be the set of variables assigned anywhere inside the condition or body, including inside closures written there. The facts on entry to the loop, minus every fact whose root is in `A`, are the *loop facts*. The condition is checked with the loop facts. The body is checked with the loop facts plus `T(c)`. After the loop the facts are the loop facts plus `F(c)`, unless the body contains a `break` that targets this loop, in which case they are just the loop facts.
- `for x in e { body }`: the collection is checked with the incoming facts; the body with the incoming facts minus every fact whose root is assigned anywhere in the body; after the loop, the same reduced facts.
- Declarations and expression statements do not change the fact set except through the bindings they declare (a new `constant` or `variable` has no facts). An assignment `x = e` removes the facts rooted at `x`.

**Boundaries.** A closure body is checked with the facts in force at the closure literal, restricted to paths whose root is a *constant binding*: a `constant`, a parameter, a loop variable or a pattern binding. A fact rooted at a `variable` is dropped inside a closure, because the closure may run after the variable has been reassigned. The body of a top-level `fn` is checked with no facts at all.

**Shared-mutable variables.** A `variable` that is assigned inside a `fn` or closure other than the one that declares it (for a top-level variable, that is any assignment inside any function or closure body) is *shared-mutable*, because a call could change it at any time. The checker never establishes facts for a path rooted at a shared-mutable variable; the program copies the value into a `constant` and narrows that.

```hollow
variable x: Int? = 5
constant reset = fn() {
    x = none
}

constant copy = x
if copy != none {
    reset()
    return copy + 1
}
return 0
```

The program returns `6`. Narrowing `x` itself would be rejected because `reset` assigns it.

The following are rejected. Each narrowing here would be unsound:

```hollow-error
variable x: Int? = 5
if x != none {
    constant read = fn(): Int {
        return x // HS0301
    }
    x = none
    return read()
}
return 0
```

```hollow-error
variable x: Int? = 1
variable total = 0
if x != none {
    for i in range(0, 2) {
        constant y: Int = x // HS0301
        total = total + i + y
        x = none
    }
}
return total
```

```hollow-error
variable x: Int? = 1
if x == none {
    return 0
}

fn read(): Int {
    return x // HS0301
}

x = none
return read()
```

Path facts on a field path `a.b` are removed when `a` is assigned, and are established only when `a.b` has an optional type after the facts on `a`.

## 4.9 Expression statements

```
callStmt = expression ;
```

Only a call may stand alone as a statement. The expression must be a postfix chain ([section 18.8](18-grammar.md#188-expressions)), optionally prefixed by `effect`, whose last operation, ignoring trailing `?` operators ([section 8.3](08-errors-and-results.md#83-the--operator)), is a call of a function. A bare `1 + 1`, `task.title` on its own line, a parenthesised expression as a whole (even `(f())`) and the construction of an enum value with a payload (`Shape.Circle(1)`) are HS0109, because each computes a value and discards it. A call whose function has a return type may be used as a statement and its value is discarded; if that value has type `Result<T, E>` and the call is not followed by `?`, the checker warns (HS0803), because a possible failure is being ignored.

## 4.10 Top-level declaration order

At the top level of a file, statements run in source order. A top-level `fn`, `record` or `enum` declaration is visible from the first statement of its file, both to the checker and at run time, so a top-level function may be called from a statement above its declaration. A top-level `constant` or `variable` initialises when its declaration runs and is visible from its declaration to the end of the file.

The checker infers the types of top-level constants and variables in source order and only then checks function bodies, which is possible because every function declares its parameter and return types; so a function body sees the type of every top-level binding of its file. A reference from a top-level statement to a name declared later is HS0205. A function body may name top-level constants and variables that are declared later in the file, because by the time the function is called they will normally exist; the following *early-use rule* makes that exact. Such a name is visible in every function body for the purposes of the no-shadowing rule as well ([section 4.6](#46-scope-and-shadowing)).

For a top-level `fn` `f`, let `Refs(f)` be the set of top-level `constant` and `variable` bindings of the same file that are named anywhere in the body of `f` (including inside closures in it), together with `Refs(g)` for every top-level `fn` `g` whose name appears in the body of `f`, computed as a least fixed point (so mutual recursion is handled). For every top-level statement `S` that is not inside any function or closure body, whose text names a top-level `fn` `f`, every binding in `Refs(f)` must be declared by a statement that comes before `S`. A binding declared by `S` itself does not count as declared before `S`. If a binding is not, the checker reports HS0210, at the first place in the body of the function that mentions the binding directly, and the related span is the statement `S`.

An implementation MAY use any method that gives the same result, and its cost MUST NOT be worse than near-linear in the size of the source: for each top-level `fn`, compute the largest declaration index of any binding in `Refs(f)` by condensing the graph of name references between functions into its strongly connected components, and report HS0210 for a statement `S` that names `f` when that index is at or after the index of `S`.

The rule looks only at names. It does not follow calls made through values (a function stored in a variable or passed as an argument), which is why a closure literal cannot be used early: closures are not hoisted and see only names declared before them.

```hollow
uses print

effect print("${double(4)}")

fn double(n: Int): Int {
    return n * 2
}
```

Output:

```text
8
```

```hollow-error
fn read(): Int {
    return base // HS0210
}

constant early = read()

constant base = 5

return early + base
```

Exported functions and imports do not need special treatment: a module is fully initialised before the module that imports it starts, and a host only calls exported functions after initialisation ([Chapter 12](12-instances.md)).
