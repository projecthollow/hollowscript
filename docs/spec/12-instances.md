# 12. Persistent instances and host-callable functions

A program can run in two ways, and both are defined here in terms of host operations. This chapter is language-level: it says what each operation means and what state it leaves behind. The bindings that expose the operations to Rust, C and other hosts are defined outside this specification and must implement exactly these semantics.

- A **script** runs once: it is loaded, its top-level statements run, and it yields a result value, if it executed a top-level `return`. This is what a command line tool does.
- An **instance** is loaded once and then kept. The host calls its exported functions by name, any number of times, with typed arguments, and reads typed results. The program's top-level `variable`s, its module-level constants and every closure it created persist between calls. This is what a game (`load`, `update`, `draw`), a server handler and a cloud function use.

A script is an instance that is loaded, has its result read, and is then disposed.

## 12.1 Host operations

| Operation | Meaning |
|---|---|
| `check(entry, options, token)` | Loads and checks the entry module and its imports and returns the diagnostics, without running anything |
| `load(entry, options, token)` | `check`, then, if there are no errors, binds the granted capabilities and initialises the modules ([Chapter 10](10-modules.md)) in a run. Produces an instance and, for a successful initialisation, the *load result* |
| `call(instance, name, arguments, token)` | Runs an exported function of the entry module and returns its value or a fault |
| `read(instance, name)` | Returns the current value of an exported `constant` of the entry module |
| `result(instance)` | Returns the load result, once, from a Ready or Finished instance (a Ready instance has no value); on a Faulted instance it returns the fault, as often as asked |
| `cancel(instance)` | Sets the token of the operation in progress ([section 11.7](11-resource-limits.md#117-cancellation-and-yield)), if any |
| `dispose(instance)` | Ends the instance and releases everything it holds |

`options` are the capability interface and grants ([section 9.7](09-effects-and-capabilities.md#97-the-capability-interface-of-a-run)), the loader ([section 10.3](10-modules.md#103-import-paths-and-canonical-paths)), the resource limits ([Chapter 11](11-resource-limits.md)), and optionally the filter schema and a yield callback.

**Tokens.** The `token` of `check`, `load` and `call` is an optional cancellation token that belongs to that one operation. A token that is already set when the operation starts faults it at once with HS1006, before any script code runs (for `check`, the result is the one diagnostic HS1006). `cancel(instance)` is a convenience that sets the token of the operation in progress when it is called, and does nothing if there is none; a host that must not lose a cancellation passes its own token. `check` and `load` test their token, and call the yield callback, every 4,096 tokens scanned and every 1,024 nodes checked, so they can be stopped; a cancelled `check` returns the diagnostics found so far followed by HS1006. A host MAY check modules incrementally provided that the diagnostics it reports are exactly those a full check reports.
## 12.2 States

An instance is in exactly one state:

| State | Meaning |
|---|---|
| Ready | Loaded; initialisation ran to the end of the entry module. Exported functions may be called |
| Running | A load or call is in progress |
| Finished | Initialisation ended early through a top-level `return` or `?` in the entry module. The load result is available. Exported functions cannot be called and exported constants cannot be read (HS1205), because top-level bindings after the `return` were never initialised |
| Faulted | A fault occurred during load or a call. The instance can do nothing further ([section 12.6](#126-faults-in-instances)) |
| Disposed | `dispose` was called |

A script is normally loaded, its result read with `result`, and disposed, from state Finished or Ready. `call` and `read` need state Ready; `result` needs Ready or Finished (or Faulted, for the fault); `cancel` may be made in any state and has no effect unless an operation is in progress; `dispose` is described in [section 12.5](#125-concurrency). Anything else fails as stated in the table of [section 12.6](#126-faults-in-instances).

## 12.3 Loading

`load` proceeds in this order, and stops at the first step that fails:

1. **Check.** The entry module and everything it imports are loaded and checked ([section 2.11](02-types.md#211-when-types-are-checked)). Any error diagnostic means the load fails, no instance is created, and the diagnostics are returned. Warnings are returned alongside a successful load. This is the only way a load returns diagnostics instead of an instance.
2. **Bind.** Each capability named in a `uses` line is bound to the host's function or namespace ([section 9.6](09-effects-and-capabilities.md#96-capability-values)).
3. **Initialise.** In a run with fresh counters ([section 11.2](11-resource-limits.md#112-what-a-run-is)), the modules are initialised in dependency order ([section 10.5](10-modules.md#105-loading-and-initialisation-order)). A fault here does not return an error: the load returns a Faulted instance that holds the fault ([section 12.6](#126-faults-in-instances)).
4. **Complete.** If initialisation reaches the end of the entry module, the instance is Ready and the load result is *no value*. If it ended early because the entry module executed a top-level `return e` or a `?` that met an `Err`, the instance is Finished and the load result is the value of `e` (or the `Err`). The load result is available to the host once, through `result`.

After a successful load the retained memory check of [section 11.4](11-resource-limits.md#114-memory) runs.

## 12.4 Calling exported functions

The functions a host can call are the `export fn` declarations of the entry module. Exports of other modules are not callable by the host. The exported `constant`s of the entry module can be read.

**Signature restriction.** For a host to call a function, every parameter type and the return type must be *transferable*: `Int`, `Float`, `Bool`, `String`, or a list, map, set, record, enum or optional whose component types are transferable. A function type, `Filter`, or a type parameter is not transferable. An exported function of the entry module that has a type parameter, or whose signature contains a type that is not transferable, is HS0606, and so is an exported `constant` of the entry module whose type is not transferable, because a host could not read it. (Such a function or constant can still be imported and used by other modules.) A host may call or read an export by name only if it is transferable.

```hollow-error
export constant f = fn(): Int { // HS0606
    return 1
}
```

**Calling.** `call(instance, name, arguments)` requires the instance to be Ready and no other call to be in progress. It then:

1. Fails with HS1201 if the entry module has no exported function named `name`.
2. Fails with HS1202 if the number of arguments differs from the number of parameters, or if any argument is not a value of the parameter's type. A value is accepted by the rule of [section 9.6](09-effects-and-capabilities.md#96-capability-values): it must be a language value of the parameter's type, or an `Int` for a `Float` or `Float?` parameter, which is converted. The conversion applies to the top level of an argument only ([section 2.7](02-types.md#27-assignability-and-numeric-coercion)): an `Int` inside a `[Float]`, a record field or an enum payload is not accepted. A record argument must have exactly the fields of the parameter's record type; an optional parameter accepts an absent value; an enum argument names a variant of the declared enum with payloads of the right types; a `Float` that is not finite, or text that is not valid, is not accepted. These failures are reported to the host and do not fault the instance.
3. Starts a run with fresh counters ([section 11.2](11-resource-limits.md#112-what-a-run-is)); the exported function is called at call depth 1 and charged as a call.
4. Runs the function. If it ends normally the value it returned (with the function's declared type) is the result, or *no value* if it returns nothing.
5. Runs the retained memory check ([section 11.4](11-resource-limits.md#114-memory)).

State persists: assignments to top-level `variable`s made by one call are seen by the next, closures stored in top-level variables stay valid, and everything reachable from the top-level bindings stays alive.

The values that cross the boundary are language values; the host's representation of them is the host binding's business. Values are copied or shared as the binding likes, since they are immutable.

`read(instance, name)` requires the instance to be Ready (HS1205 if it is Finished or Disposed), returns the current value of an exported `constant`, and fails with HS1201 if there is no such exported constant. It does not run script code and costs no steps.

## 12.5 Concurrency

Execution of one instance is strictly sequential: one run at a time, statements in order, one capability call finishing before the next begins. There is no shared memory between instances, and there is no concurrency inside a script, which is why closures need no capture mode ([section 5.4](05-functions-and-closures.md#54-capture)).

A host must not call into an instance while a call or load of that instance is in progress. An attempt is refused with HS1204, and does not disturb the running call. This includes calls made from inside a capability function or a yield callback of the same instance. The one exception is `cancel`, which may be made at any time, from any thread. `dispose` while a load or call is in progress is HS1204 and leaves the instance unchanged; `dispose` on a Disposed instance is HS1205; in every other state `dispose` succeeds. A host that wants concurrency creates several instances; they may run on different threads if the host's capabilities and bindings allow it.

## 12.6 Faults in instances

If a load or call faults ([section 8.1](08-errors-and-results.md#81-faults-and-recoverable-errors)), the instance becomes Faulted. A Faulted instance may be inspected (`result` returns the fault, which is retained) and disposed, and every other operation on it fails with HS1203 without running any script code. The reason is that a fault can stop the program in the middle of updating several top-level variables, and no rule can make the partial state safe to continue from. A host that wants to continue creates a new instance, and if it wants the old state it keeps it itself.

HS1201, HS1202, HS1204 and HS1205 are errors of the host's request and leave the instance as it was.

| Code | Situation | Instance afterwards |
|---|---|---|
| HS1201 | no such exported function or constant | unchanged |
| HS1202 | wrong number or types of arguments | unchanged |
| HS1203 | the instance is Faulted | Faulted |
| HS1204 | any operation except `cancel` while a load or call is in progress, including `dispose` | unchanged, and the running call continues |
| HS1205 | `call` or `read` on a Finished instance, and any operation on a Disposed instance | unchanged |

## 12.7 The lifecycle convention

A host chooses which exports it calls and when. The convention for interactive programs is:

- `export fn load()`: called once after loading, for setup that needs the host;
- `export fn update(dt: Float)`: called for each fixed time step, where `dt` is the step in seconds; the host owns the time step, and with an injectable clock and a seeded random capability a program is deterministic;
- `export fn draw()`: called after `update` to produce output for the frame;
- event functions such as `export fn keyPressed(key: String)`, called by the host when the event happens.

These names are a convention and are not reserved words. A server handler or a cloud function exports whatever entry points its host defines.

```hollow
uses print

variable frames = 0
variable position = 0.0

export fn load() {
    position = 10.0
}

export fn update(dt: Float) {
    frames = frames + 1
    position = position + dt * 60.0
}

export fn draw() {
    effect print("frame ${frames} at ${position}")
}

export fn frameCount(): Int {
    return frames
}
```

If a host loads this program, calls `load()`, then `update(0.5)` (the `0.5` is a `Float`), then `draw()`, the program prints `frame 1 at 40.0`, and `frameCount()` returns `1`. If the host then calls `update(1)`, the `Int` is converted to `1.0` and `position` becomes `100.0`.

## 12.8 Changing the program

Version 1 has no way to replace the code of a running instance. A host that reloads a program creates a new instance, and carries over any state it wants through its own means (for instance by calling an exported function that takes the state as arguments). This keeps the meaning of top-level bindings and closures unambiguous.

## 12.9 Limits per run and per instance

Steps, output and call depth are counted per run: the load and each call each have the whole budget. Memory has both a per-run allocation counter and the retained size of the instance, which is what stops a program that adds a little to a global list at every call from growing without bound ([section 11.4](11-resource-limits.md#114-memory)).
