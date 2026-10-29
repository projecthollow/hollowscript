# 11. Resource limits

A HollowScript program is safe to run when it is hostile, because every run is bounded. The bounds are part of the language, not of an implementation: the counters below are defined by this chapter so that the same program reaches the same limit at the same operation on every conforming implementation, and the defaults are the same in every host.

## 11.1 The limits

There are four limits and one cancellation mechanism.

| Limit | Option name | Bounds | Default |
|---|---|---|---|
| Steps | `maxSteps` | units of work executed | 100,000,000 |
| Memory | `maxMemoryBytes` | bytes allocated by a run, and bytes retained by an instance | 67,108,864 (64 MiB) |
| Output | `maxOutputBytes` | bytes written by the `print` capability | 16,777,216 (16 MiB) |
| Call depth | `maxCallDepth` | active function and closure calls | 1,000 |

Wall-clock time is not a language limit. It is the host's job, done through cancellation ([section 11.7](#117-cancellation-and-yield)).

A host may set each option to any value from 1 to 9,223,372,036,854,775,807. A limit cannot be switched off; the largest value is unreachable in practice. The defaults apply when the host sets nothing, so a program that fits the defaults behaves the same in the command line tool, in a workflow runner, in an editor and in a cloud function. A host that lowers a limit is stricter than the language; a host that raises one accepts more risk.

Reaching a limit is a **fault** ([section 8.1](08-errors-and-results.md#81-faults-and-recoverable-errors)): it aborts the run, cannot be caught, and cannot be defeated by the program. The diagnostic names the limit, its value, and the amount that was used or requested.

The parse-time limits of [section 1.10](01-lexical-structure.md#110-structural-limits) are fixed and are not host options. An implementation's own limits, which that section also describes, are not host options either, and not limits of the language: a run that reaches one faults with HS1015.

## 11.2 What a run is

Each of these is a *run* with its own fresh steps, output, call depth and allocation counters:

- the load of a program (checking excluded): initialising the entry module and everything it imports;
- each call of an exported function ([section 12.4](12-instances.md#124-calling-exported-functions)).

Checking a program consumes no steps; it is bounded by the structural limits. Checking takes the cancellation token and the yield callback of its operation, and has its own safepoints ([section 12.1](12-instances.md#121-host-operations)).

## 11.3 Steps

A **step** is the unit of work. A run has a counter, starting at 0. Work is *charged* before it is done: if charging `c` more steps would make the counter exceed `maxSteps`, the run faults with HS1001 at that operation and the work is not done. So a program can execute exactly `maxSteps` steps.

The charges are:

| Operation | Charge |
|---|---|
| Executing any statement: a declaration, an assignment, a call statement, `return`, `break`, `continue`, an `if`, a `match`, and the start of a `for` or `while` | 1 |
| Each iteration of a `for` or `while` loop, charged when the iteration begins | 1 |
| A call of a script function or closure, charged when the call is made | 1 |
| A call of an enum variant constructor (`Shape.Circle(1.0)`, `Result.Ok(x)`) | 1 |
| A call of a library function or method | its cost, given with the function: **1** for a constant-time operation, or **1 + floor(n / 64)** where `n` is the number stated with the function (elements or bytes processed) |
| A call of a capability function | **1 + floor(a / 64)** before the call, where `a` is the size of the arguments, and **floor(r / 64)** immediately after it returns, where `r` is the size of the result |
| `==`, `!=`, `<`, `>`, `<=` or `>=` | 0 if both operands are scalars (`Int`, `Float`, `Bool`, `none`), otherwise **1 + floor(m / 64)**, where `m` is the smaller of the tree sizes ([section 11.4](#114-memory)) of the two operands |
| Evaluating any other expression that is not a call (literals, other operators, field access, indexing, closure creation, interpolation) | 0 |

The statement that calls a function is charged as a statement and the call as a call. A callback that a library function invokes (as `map` invokes its argument) is a call of a closure and is charged as one, in addition to the library function's own cost.

**Sizes for a capability charge.** The size of a value is: for a `String`, its number of UTF-8 bytes; for a list or a set, 1 for each element plus the size of the element; for a map, 1 for each entry plus the sizes of its key and its value; for a record, the sizes of its field values; for an enum value, the sizes of its payload values; and 0 for a scalar. The size is therefore the same for equal values on every implementation.

**Comparison charges.** The comparison charge does not depend on how an implementation compares (a pointer check, interning, a hash), so two implementations charge the same for the same program. It is charged before the comparison is made. `contains` and `indexOf` on a list ([section 14.1](14-stdlib-collections.md#141-lists)) charge each comparison in the same way, in one charge up front.

**Slices.** No single charge is larger than 1,024. A larger charge is applied as consecutive charges of at most 1,024, and an operation whose work is proportional to its charge does that work in matching slices: for each slice it charges, applies the safepoint rule of [section 11.7](#117-cancellation-and-yield), and then does the work of that slice. The result is built in private storage, so a fault or a cancellation between slices leaves nothing visible except the effects of capability calls already made. For every operation that the language itself performs, at most 1,024 steps of work therefore happen between two safepoints. A capability call is one operation from the language's point of view: its argument charge is applied before the call and its result charge right after it returns, each in slices with a safepoint after each, so HS1001 can be raised after the call has already had its effects.

**Order of faults.** When one operation would exceed both `maxSteps` and `maxCallDepth`, the call charge is applied first, so the fault is HS1001. Where an operation has a step charge and a memory charge, the step charge is applied first.

**Worked example.** With the default limits:

```hollow
variable total = 0
for n in range(0, 3) {
    total = total + n
}
return total
```

Charges: the declaration 1; the `for` statement 1; the call of `range`, whose result has 3 elements, 1; three iterations, each 1 for the iteration and 1 for the assignment, 6; the `return` 1. The total is 10, so this program faults with HS1001 if `maxSteps` is 9 and completes if it is 10.

A call of `fs.read` with a 9-byte path that returns a 200-byte file is charged 1 + floor(9 / 64) = 1 before the call and floor(200 / 64) = 3 after it, 4 in all, in addition to 1 for the statement that contains it.

## 11.4 Memory

The memory limit bounds two quantities, both computed from a fixed size formula so that the result does not depend on how an implementation stores values or on when it reclaims memory.

**Node cost.** Each value has a *node cost* in bytes:

| Value | Node cost |
|---|---|
| `Int`, `Float`, `Bool`, `none` | 0 (they are held in the slot that contains them) |
| `String` with `b` bytes of UTF-8 | 16 + b |
| `Filter` whose source has `b` bytes | 16 + b |
| list with `n` elements | 16 + 8n |
| record with `k` fields | 16 + 8k |
| enum value with `m` payload values | 16 + 8m for `m > 0`, and 0 for a variant with no payload |
| `Map` with `n` entries | 32 + 24n |
| `Set` with `n` elements | 32 + 16n |
| closure or function value that captured `c` bindings | 32 + 8c |

**Tree size.** The *tree size* of a value is its node cost plus the tree sizes of the values it directly contains (elements, fields, payloads, keys, map values). Sharing is ignored: a value that is contained twice is counted twice. The bindings that a closure captured are not part of the tree size of the closure; the retained size below counts them. Tree sizes and all sums of them are unsigned 64-bit numbers that saturate at 2^64 - 1, and a saturated value is larger than every permitted `maxMemoryBytes`, which is at most 2^63 - 1. A program can build a value whose tree size is astronomical in a few statements by sharing (`t = Tree.Node(t, 1, t)` doubles it), which is why saturation is part of the definition.

**Allocation counter.** A run has a second counter, starting at 0. It is increased by the node cost of a new value, and only of the new value (its children were charged when they were created), in these cases and no others:

- evaluating a string with interpolation: the final string only, 16 + b with `b` its total bytes; the text of each `${}` on its own is not charged. String literals with no interpolation and filter literals that appear in the source are not charged;
- evaluating a list, map or record literal, and evaluating a closure literal, a `fn` name used as a value, or an enum variant with a payload used as a value: each evaluation creates a value;
- constructing an enum value with a payload, including the `Ok` and `Err` that a library function returns and the `Err` that `?` returns;
- the `MapEntry` record of each iteration of a `for` over a map, and the list bound by a `..rest` pattern;
- a `String`, list, record, map, set, enum value with a payload or closure that a library function returns, unless it is an element, field or payload of the receiver or of an argument, or the receiver itself, returned unchanged: those are charged nothing (`first`, `unwrapOr`, `get`, `slice` of the whole list, `trim` of a string with nothing to trim). `append`, `Map.with` and `Set.with` are charged as the next paragraph says;
- a value that a capability returns, after the call, at its node cost with its children.

`none` and a present optional value have no node of their own and are free. If the result of an operation has a size that is known before it is built (`range`, `concat`, `repeat`, `join`, interpolation), the charge is made before building. If it is not (`replace`, `toUpper`, `toLower`, `split`, `fixed`, `json.stringify`, `json.pretty`), the implementation first computes the exact size without building the result, in unbounded arithmetic, and charges that before it builds. Either way a huge result faults without being built; the step charge of the operation is applied first, so `range(-9223372036854775808, 9223372036854775807)`, whose length is 2^64 - 1, faults with HS1001 when the step charge of 2^58 does not fit in what remains of `maxSteps`, and with HS1002 otherwise. If the counter would exceed `maxMemoryBytes`, the run faults with HS1002.

`append`, `Map.with` and `Set.with` are charged only for what they add: 8 bytes for the element of a list, 24 for a map entry that did not exist, 16 for a set element that did not exist, and nothing for a map entry or set element that is replaced, plus the node cost of any new value inside the element. The counter measures how much a run allocates, and what an instance may keep is bounded by the retained size. A loop that grows a list one element at a time is limited by the step charge (1 + floor(N / 64) per `append`) long before it is limited by memory.

**Retained size.** When a run ends normally, and for an instance after each call and after loading, the implementation computes the *retained size*: the sum of the tree sizes of the values of all top-level bindings of all modules, plus, for every binding that a closure reachable from those values captured, 16 plus the tree size of its current value, counting each binding once however many closures captured it. A reachable value is one that is found from a top-level binding through elements, fields and payloads, and through the current values of captured bindings. A captured binding that is itself a top-level binding is counted once, as a top-level binding. If the retained size exceeds `maxMemoryBytes`, the run faults with HS1003. A closure therefore cannot hide memory: a program that stores in a top-level variable a closure over a large local list pays for the list.

An implementation may cache the tree size of every value at its construction and skip the walk for a value that holds no closure; it MUST produce the same sum. The walk visits each captured binding and each distinct stored value once, so its time is proportional to what the instance can reach, which the memory limit bounds. It consumes no steps.

**Deep values.** A program can build a value as deeply nested as the memory limit allows (a loop that wraps a list in a list a million times). Equality, comparison, tree size, the retained size walk, releasing a value and every other operation of the language and the library MUST work for any depth that fits in `maxMemoryBytes` without exhausting the host's stack, for example by iterating with an explicit stack. `json.stringify` and `json.pretty` limit their own depth ([section 15.1](15-stdlib-data.md#151-json)).

These counters describe the program, not the machine: an implementation may use more or less real memory, and a garbage collector never changes when a limit is reached.

## 11.5 Call depth

The *call depth* is the number of script function and closure calls that are active: called and not yet returned. Library functions and capability functions do not count, but a closure that a library function calls does. Top-level code of a module has depth 0. A call that would make the depth exceed `maxCallDepth` faults with HS1005, reported at the call, with the innermost frames.

An implementation MUST make the default `maxCallDepth` of 1,000 safe: recursion must always reach the language's counter before it can exhaust the implementation's own stack or any other host resource, on every supported platform. How that is arranged (an explicit frame stack, growth of the native stack, trampolining) is not specified; the requirement is that it holds. An implementation may also bound the memory that its active calls hold between them, as an implementation limit ([section 1.10](01-lexical-structure.md#110-structural-limits)); such a bound MUST NOT stop any recursion whose frames are no larger than a program of the conformance corpus needs before it reaches `maxCallDepth`, and reaching it faults with HS1015, not HS1005.

```hollow-fault
fn forever(n: Int): Int {
    return forever(n + 1) // HS1005
}

return forever(0)
```

## 11.6 Output

The `print` capability ([section 16.2](16-standard-capabilities.md#162-print)) writes the UTF-8 bytes of its text followed by one LF byte (U+000A). The number of bytes written by a run is counted. If a call to `print` would bring the total above `maxOutputBytes`, nothing is written by that call and the run faults with HS1004. Other capabilities have their own limits set through their grants ([section 9.5](09-effects-and-capabilities.md#95-scoped-grants)).

## 11.7 Cancellation and yield

A host can cancel an operation from any thread by setting its *cancellation token* ([section 12.1](12-instances.md#121-host-operations)). It can also install a *yield callback*, which the run calls periodically so that the host can do its own scheduling work, such as checking a wall-clock deadline.

A **safepoint** occurs:

- after each charge ([section 11.3](#113-steps)) that makes the step counter reach or pass a multiple of 1,024. One safepoint occurs per charge, however many multiples it passes, and a charge is never larger than 1,024. It occurs after the limit check for that charge, so a charge that would exceed `maxSteps` faults with HS1001 and has no safepoint, and before the work that the charge pays for;
- immediately after a capability function returns, and after the result charge of that call. This safepoint is in addition to the first kind and does not depend on the counter.

A charge of 100 that takes the counter from 1,000 to 1,100 crosses 1,024, so the safepoint occurs once and a yield callback is called once, before the work it pays for; a charge of 1 from 1,023 to 1,024 does the same, and a charge of 1 from 1,024 to 1,025 does not.

At a safepoint the run does the following, in order:

1. If the cancellation token is set, the run faults with HS1006.
2. If a yield callback is installed, calls it. The callback returns *continue* or *cancel*. On *cancel* the run faults with HS1006. The callback must not call into the instance (HS1204).

Consequently cancellation is noticed after at most 1,024 further steps of the language's own work, or when the current capability call returns; a wait inside a capability is the capability's own responsibility, and it MUST end as soon as it can once cancellation is requested ([section 9.6](09-effects-and-capabilities.md#96-capability-values)). A token that is already set when an operation starts faults that operation at once with HS1006, before any script code runs ([section 12.1](12-instances.md#121-host-operations)). Wall-clock limits are implemented by a host timer that sets the token, or by the yield callback comparing a deadline.

A cancelled run has the effects it had produced up to the fault, and the instance is faulted ([section 12.6](12-instances.md#126-faults-in-instances)).

The examples of this specification that use an infinite loop assume the default limits:

```hollow-fault
variable n = 0
while true { // HS1001
    n = n + 1
}
```

## 11.8 Reporting

A limit fault (HS1001 to HS1005) carries the code, the `data` of [section 17.1](17-diagnostics.md#171-the-diagnostic-record) (the option name, the configured value and the counter value at the fault), and the span and frames of the operation that hit it. A message names the limit in words, for example `The step limit of 100000000 was reached.`, so that a person can tell which limit to raise or which loop to fix.
