# Design documents

These documents record the architecture of the native implementation: the
crate graph, the data structures each stage passes to the next, the bytecode
format, the value representation, and the operational properties (limits,
determinism, diagnostics) the specification requires. They describe the
skeleton that has been built and the decisions it embeds, so that several
people can each implement one crate against a fixed interface without needing
to agree on it first.

The specification (`docs/spec/`) is the normative contract: it defines what a
conforming implementation must do. These documents are not normative; they
explain how this implementation meets that contract, and a few of the
implementation choices the specification deliberately leaves open (the exact
bytecode format, the heap design, the C ABI).

| Document | Covers |
|---|---|
| [architecture.md](architecture.md) | The crate graph, the dependency rules, and the public surface of each crate. |
| [ast-and-hir.md](ast-and-hir.md) | The syntax tree and the typed HIR: what each stage owns and why they are separate trees. |
| [types.md](types.md) | Type representation and interning, and the expanded size bound of section 2.5. |
| [bytecode.md](bytecode.md) | The instruction set, the container format, and the verifier the browser VM trusts instead of its supplier. |
| [values-and-heap.md](values-and-heap.md) | The value representation, the generational arena, mark and sweep collection, and the byte accounting of chapter 11. |
| [limits-and-cancellation.md](limits-and-cancellation.md) | How steps, memory, output and call depth are charged and checked, and how a safepoint works. |
| [capabilities.md](capabilities.md) | How the VM calls a capability and how a host binds one, including the batched path for a frame-based host. |
| [instances.md](instances.md) | The instance lifecycle of chapter 12 and the state machine that implements it. |
| [errors-and-determinism.md](errors-and-determinism.md) | The fault and host-error model in the Rust API, and what determinism this implementation promises. |
| [diagnostics.md](diagnostics.md) | The diagnostic registry, its specification cross-check, and the canonical ordering and cap of section 17.1. |
| [performance.md](performance.md) | Performance budgets, the benchmark harness, and the numbers it has measured. |
| [testing.md](testing.md) | The testing strategy: unit, snapshot, the corpus, fuzzing, property tests and differential runs. |
| [fuzzing.md](fuzzing.md) | The continuous, coverage-guided fuzz targets in `fuzz/`: how to run them, what each one asserts, and what each run has found. |
| [release-and-milestones.md](release-and-milestones.md) | What the release and mirror process implies for the tree layout, and the milestone plan mapping crates to owners. |
| [release.md](release.md) | The release pipeline's mechanics: reproducible builds, signing, the mirror commit, the installers, and withdrawal. |
| [security.md](security.md) | What the implementation promises about running untrusted code, what it does not, and where an independent review belongs. |
| [dependencies.md](dependencies.md) | Every external dependency, its licence, and the one deliberately deferred decision (an HTTP client for the CLI). |

## How to read the crates against these documents

Every crate's own `src/lib.rs` doc comment is the first thing to read: it
states what the crate owns, what it depends on, and, for a crate that is not
finished, exactly what is not written yet and what a caller gets in the
meantime (never a panic; a typed error or the internal not-yet-implemented
diagnostic, HS1099). These design documents give the reasons behind those
interfaces and the parts that do not fit in one crate's doc comment: how two
crates' data structures relate, and the properties (determinism, resource
safety) that no single crate can state on its own.
