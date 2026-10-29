# Resource limits and cancellation

Chapter 11 defines four limits and one cancellation mechanism as part of the
language itself, not as an implementation convenience, precisely so that the
same program reaches the same limit at the same operation on every
conforming implementation. This document is about how the native
implementation checks them; the numbers and the rules themselves belong to
the specification.

## Where the counters live

`hollowscript_vm::limits::RunCounters` holds the four per-run counters
(steps, bytes allocated, bytes of output, call depth) and one method per kind
of charge:

- `charge_steps(amount, limits)`: charges from 1 to 1,024 steps at once
  (`MAX_CHARGE`, the slice size section 11.3 fixes for every charge in the
  language) and faults with the step-limit fault (HS1001) if the charge
  would take the counter past `maxSteps`. Charging is checked before the
  counter is updated, so a program executes exactly `maxSteps` steps, never one more.
- `charge_memory(bytes, limits)`: the allocation counter, faulting HS1002.
- `charge_output(bytes, limits)`: the `print` output counter, faulting HS1004.
- `enter_call` / `leave_call`: the call depth counter, faulting HS1005 on
  entry when the depth is already at the limit.

Each of these returns a typed `hollowscript_core::fault::Fault` carrying the
option name, the configured limit, and the counter value at the fault
(section 11.8's required diagnostic data), built by the constructors in
`hollowscript_core::fault` (`Fault::step_limit`, `Fault::memory_limit`, and
so on) so that the wording and the `data` field are assembled in one place
rather than at every call site.

`check_retained_size` is a free function rather than a method on
`RunCounters`, because the retained size is not a per-run counter: it is
computed once, after a load or a call completes, over the instance's current
top-level bindings, as [values-and-heap.md](values-and-heap.md) describes.

## The order of faults

Section 11.3 requires that when one operation would exceed both the step
limit and another limit, the step charge is applied first, so the fault
reported is always HS1001 in that case. `RunCounters`'s methods are designed
to be called in that order by the interpreter (charge steps, then charge
memory for the same operation), rather than encoding the ordering rule
inside the counters themselves, because only the interpreter, sequencing an
actual instruction, knows which charges belong to one operation.

## Safepoints and the yield callback

`hollowscript_core::cancel` (not `hollowscript-vm`, because the front end's
`check` and `load` also need cancellation, per section 12.1) defines:

- `CancellationToken`: a shared, cloneable flag a host can set from any
  thread; once set it cannot be cleared, matching "a token that is already
  set when an operation starts faults it at once."
- `Yield` and `YieldDecision`: the callback a host may install, called at
  every safepoint, that can itself request cancellation.
- `Control`: the token and callback of one operation, with `safepoint()`
  implementing the exact order section 11.7 specifies: if the token is set,
  fault immediately (HS1006) without calling the yield callback at all; only
  if it is not set is the callback invoked, and only then can the callback
  itself cancel. `cancel::tests::the_token_is_tested_before_the_callback`
  checks this ordering directly, by counting how many times the callback runs.

## Wall-clock deadlines at the WebAssembly boundary

Wall-clock time is the host's job (section 11.1), done through the yield
callback (section 11.7). In the WebAssembly build the yield callback is the
`env.hs_wasm_poll_cancel` import, which the JavaScript wrapper answers from
three sources, in the order section 11.7 gives (the token, then the host's
callback): the shared cancel flag a controller thread sets, the
`deadlineMs` budget (a monotonic clock read at each safepoint, against an
absolute time armed when a run starts), and a `shouldCancel` function. The
last two exist for a host with no Worker, which cannot set a flag while the
VM runs on its only thread but can compare a clock when asked. Nothing on
the Rust side knows about any of them: it asks the import at every
safepoint, as it always did, so a safepoint every 1,024 steps is also the
granularity of a deadline, and a capability that blocks is not interrupted
(section 11.7 leaves that wait to the capability). Each run, meaning the
load, each call and each resume, arms its own deadline, so time a batched run
spends suspended is not charged to it.

## Native operations sliced

Section 11.3's "slices" rule (no single charge exceeds 1,024, and a longer
operation is charged and performed in matching pieces so a safepoint falls
between them) is a property the compiler and the library implementation must
give every long-running native operation once they are written; the
bytecode format supports it directly: `Op::Charge` takes an immediate amount
from 1 to 1,024 (checked by the verifier, described in
[bytecode.md](bytecode.md)), so a native function that processes, say, a
100,000-element list charges and does its work 1,024 elements at a time,
each slice followed by the ordinary per-1,024-steps safepoint the step
counter already produces. No separate "slice" primitive is needed in the
bytecode; the existing charge-then-safepoint mechanism already has the right shape.

## What ties these together

`hollowscript_vm::interp` does this: it calls `RunCounters::charge_steps` for
every `Op::Charge`, `enter_call` before pushing a frame for every
`Call`-family instruction, `charge_output` inside the `print` capability's
implementation (or, for a table that marks its `print` with
`CapabilityTable::with_output_charged`, in `charge_capability_arguments`
before the call is made or queued: see [capabilities.md](capabilities.md)), and `Control::safepoint` at the points section 11.7 defines
(after a charge crosses a multiple of 1,024, and immediately after a
capability call returns). The library functions of chapters 13 and 14 charge
their own cost the same way, through `hollowscript_vm::natives`.

**Call depth is the counter's, not the host's.** Section 11.5 requires that
recursion always reach `maxCallDepth` before it can exhaust the
implementation's own stack. An interpreter that recursed in Rust for every
script call would not satisfy that: in an unoptimised build an interpreter
frame is large enough that a hundred script calls overflow the real stack,
which aborts the process rather than raising a fault a host can catch.
`Interpreter::run_function` therefore owns an explicit vector of frames, so
the host's stack stays one frame deep however deeply a program calls. The
same vector is what lets a library function that calls a closure suspend
between callbacks instead of recursing, and it is the state a suspended
interpreter would have to keep for the batched capability path
(see [capabilities.md](capabilities.md)).
