# Release and mirror process: implications for the tree layout, and the milestone plan

## The release model, and what it means for this tree

Development happens in a private repository; what the public sees is a
release mirror, written only by a release process, holding one commit per
release rather than the private repository's own history. This shapes the
tree in a few concrete ways:

- **Everything under this repository's own root is mirrored, except a
  top-level `internal/` directory**, which a release process must exclude
  entirely. This workspace keeps that boundary already: nothing under
  `crates/`, `xtask/`, `docs/design/`, `corpus/` or `docs/spec/` may name
  anything that is not fit for public release, and `cargo xtask hygiene`
  enforces the forbidden patterns (dashes of the wrong kind, absolute local
  paths, secret-shaped strings, and references to internal tooling or to
  another implementation) in `scripts/check`, and
  `scripts/release-mirror.sh` runs it again, with `gitleaks`, on every
  export before it is committed to the mirror ([release.md](release.md),
  section 4).
- **No file names or references any internal identifier scheme.** Commit
  messages, code comments and these design documents describe decisions in
  their own terms (what the specification requires, what a crate owns) never
  by a task or record identifier from an internal tracker.
- **Bytecode is explicitly not a stability promise** (see
  [bytecode.md](bytecode.md)), which matters for the mirror specifically: a
  release mirror is the one place compatibility promises become visible to
  the outside world, so the format's own documentation states plainly, in
  public terms, that it may change in any release and that only source
  compatibility and the C ABI (within a major version) are promised.
- **A dependency's licence is recorded once, publicly**, in
  [dependencies.md](dependencies.md), so a release process has one file to
  check for a licence incompatibility rather than needing to inspect every
  crate's manifest by hand.
- **Publishing is a separate, manual step.** Landing a change is a commit
  to this repository's own default branch. Building, signing and uploading
  a release, deploying `get.hollowscript.org` and writing the mirror commit
  are done by a person, from a tag, with the scripts
  [release.md](release.md) describes; none of them runs on a push.

## Milestones, mapping crates to owners

The crates are already split so that each can be owned and progressed by one
person without needing to coordinate beyond the interfaces this design
records. The suggested order below follows the dependency graph
(see [architecture.md](architecture.md)): a crate is not blocked waiting for
one later in the list, only for the ones it depends on.

A milestone is done when its crate's own unit tests pass, `scripts/check`
passes for the whole workspace, and the number of conformance cases that
pass has grown. That last one is the concrete, machine-checked signal that a
milestone's work has reached the pipeline end to end rather than only
compiling in isolation: `hollowscript-conformance` compares all four stages
of every case against the case's own expected file, and `scripts/check`
fails both when a case outside the baseline fails and when a case inside it
passes (see [testing.md](testing.md)). The cases that do not pass yet are
named in `crates/hollowscript-conformance/known-failures.txt`; this document
does not repeat the count, so that it cannot go stale.

The feature-completion table below is a **2026-09-26 implementation snapshot**;
its individual feature rows were not all re-tested on 2026-10-07. The latest
release rehearsal was on 2026-10-06 and used an uncommitted working-tree
snapshot. HollowScript v0.1.0 remains unreleased; rehearsal evidence is not
launch approval.

## Implementation snapshot (2026-09-26, v0.1.0 preview)

| Milestone | Crate | Status | What remains |
|---|---|---|---|
| M1 | `hollowscript-syntax` | Done | Nothing known |
| M1 | `hollowscript-vm` | Done | Chapters 13, 14 and 15 are implemented, including the functions that call a closure and section 14.1's fixed merge schedule for `sort(compare)`. Faults carry a source location (resolved only when a fault needs it), section 9.6's acceptance rule is applied to every capability result, the output limit is charged, the heap is collected between runs, a run can suspend at a capability call and be resumed, and frames reach 65,535 registers through the `Ext` operand prefix, with the registers of all active calls bounded (HS1015) |
| M2 | `hollowscript-check` | Done | Nothing known. Every corpus case passes, including the diagnostic counts and lines, the module-graph diagnostics and a generic type parameter inferred across arguments (`firstOr([], 9)`). A long line is located in linear time |
| M2 | `hollowscript-lsp` | Done | Nothing known. Diagnostics, formatting, hover, go to definition, type definition, find references, rename, signature help, completion, document symbols, folding, semantic tokens and quick fixes are real, and names declared in imported files are resolved |
| M3 | `hollowscript-compiler` | Done | Nothing known. Every construct lowers, of any width: long literals are built in chunks and far jumps become `JumpLong`; the one limit left, 65,535 registers in a function, is HS0901 ([bytecode.md](bytecode.md)). The export table carries each export's transferable signature |
| M4 | `hollowscript` (facade) | Done | Nothing known. `check` (which also reports HS0901), `build`, `load`, `load_bytecode`, `call`, `read`, `exports`, `format` and the batched command and resume path are real |
| M4 | `hollowscript-cli` | Done | `check`, `format`, `run` (source or a container), `build`, `lsp`, `version` and `help`, with per-command help. `--allow` grants `env`, `args`, scoped `fs` and `stdin`, and scoped `http` (`ureq` with `rustls`, see [dependencies.md](dependencies.md)). An interactive REPL is planned for a later release |
| M4 | `hollowscript-capi` | Done | Version, check, format and the whole instance lifecycle (`hs_load`, `hs_load_ex` with capabilities a C host binds, `hs_call`, `hs_read`, `hs_dispose`) are real, with a panic stopped at the boundary |
| M5 | `hollowscript-wasm` | Done | The JavaScript boundary and `web/` wrapper run a CLI-built container in a browser and in Node, with capabilities the page binds and the batched suspend and resume path; a separate `tooling` build checks, formats and compiles in the browser |
| M5 | `hollowscript-conformance` | Done | The corpus host gives a case real scoped `fs` and `http` grants, per its `grants` and an optional `<name>.host.json` (section 19.4), which also overrides `limits`; `stdin`, `args`, `env`, `yieldCancelAt` and `entryPath` are not read from `host.json` yet, because no case has needed them. `known-failures.txt` is empty |
| Ongoing | `xtask` | Done | The hygiene scan, the crate graph check, the benchmark harness (`cargo xtask bench`, see [performance.md](performance.md)), the peak-memory harness (`cargo xtask mem`), per-archive third-party licences, the SBOM and release signing are real; the hygiene scan and the crate graph check run in `scripts/check`, alongside the whole-pipeline fuzz test in `crates/hollowscript/tests/fuzz_pipeline.rs`. Continuous, coverage-guided fuzzing lives outside the workspace in `fuzz/`; see [fuzzing.md](fuzzing.md) |
| Release | `scripts/`, `editors/vscode/` | Rehearsed, not released | A later working-tree rehearsal on 2026-10-06 built 20 products reproducibly and passed the recorded installer and upload/withdrawal fixture suites. There is no `v0.1.0` tag or public release. The Visual Studio Marketplace and Open VSX publisher identities are not established; publish only after their setup and the release gates in [release.md](release.md). |

## Implementation capabilities in the 2026-09-26 snapshot

- Reading a program: the lexer, the parser with error recovery, and the
  canonical formatter.
- Understanding a program: name resolution, type inference, optional
  narrowing, effects and capabilities, pattern exhaustiveness and the
  unused-name warnings of section 17.3.
- Turning a checked program into verified bytecode, with its exports, for
  programs of any width up to this implementation's documented limits
  (section 1.10, HS0901 and HS1015).
- Running bytecode on the interpreter's own frame stack, with exact resource
  accounting, cancellation and located faults.
- The standard library of chapters 13, 14 and 15 and the `Result` methods of
  section 8.4.
- Resolving a program's imports from disk, rooted at the entry module's own
  directory as section 10.3 requires.
- Granting `print`, `clock` and `random` always, and `env`, `args`, `fs`,
  `stdin` and `http` on `--allow`, with section 9.5's scopes.
- The instance lifecycle of chapter 12, from Rust, from C and from
  JavaScript through the WebAssembly build, with host-bound capabilities in
  each.
- Diagnostics and formatting in the command line tool, the C ABI and the
  language server, and the language server's navigation and refactoring
  requests, with a VS Code extension over it.
- A differential conformance gate over the whole corpus, with no known
  failures, and a whole-pipeline fuzz test, both run by `scripts/check`.
- A benchmark harness and a peak-memory harness, so the budgets in
  [performance.md](performance.md) are measured numbers.
- Reproducible, signed release artifacts can be built for macOS, Linux,
  Windows and WebAssembly. Native installation support for the v0.1.0 target
  list remains unverified where noted below ([release.md](release.md)).

## What remains before v0.1.0

As of 2026-10-07, the implementation rehearsal is complete but public release
gates remain:

1. **Freeze and review the candidate.** The list-append memory fix is in the
   working tree and passed the recorded in-house review and checks. Repeat the
   checks on the clean release commit, then obtain the independent security
   review of that frozen candidate and resolve its findings.
2. **Set the supported-platform matrix.** The latest installer checks were
   run on macOS and in Linux containers; Windows PowerShell ran in a Linux
   container with placeholder executables. Native Windows, native Linux and
   physical Intel Mac installation remain unverified. Test any target that
   will be advertised or leave it out of v0.1.0. Smart App Control applies to
   eligible Windows 11 installations, not Windows 10 ([Microsoft requirements](https://learn.microsoft.com/windows/apps/develop/smart-app-control/overview)). The current recommendation is to launch with Apple Silicon macOS and WebAssembly, then add other targets after clean-host installer checks.
3. **Resolve release-scope decisions.** The current procedure publishes the
   signed downloads, curated source mirror and VS Code extension to the Visual
   Studio Marketplace and Open VSX. It does not publish Rust crates to
   crates.io; the recommendation is to defer that channel because no
   dependency-ordered publish procedure or account setup exists. The current
   process produces an SBOM and signed checksums but no build-provenance
   attestation; the recommendation is to publish a signed attestation with
   v0.1.0. Before the first real upload, verify an SSH-format release
   signature produced by the Cloud KMS key using the pinned public key and
   `ssh-keygen -Y verify`; existing SSH-signature tests used a local key.
   The unsigned `latest.txt` can also freeze installs at an older signed
   release if bucket-write access is compromised. The recommendation is to
   accept this documented limitation for the preview, with restricted bucket
   access, and add an expiring signed manifest before a stable release; the
   owner must record that disposition.
4. **Complete the owner and operations gates.** Confirm the rights holder and
   copyright notice; add a second owner to both GitHub organisations; restrict
   public repository creation in the private canonical-source organisation; verify registrar lock,
   renewal and MFA; verify the security and public-contact mailboxes plus
   two-person security monitoring; test support-form receipt; establish both
   extension-store identities; verify the applied Firebase and bucket
   permissions; exercise a real upload and withdrawal; deploy the corrected
   product-page copy; and update the site and download routes with the actual
   supported-platform matrix.
5. **Record the owner go decision before publication.** After all required
   checks and the final tagged build pass, sign off in the launch checklist.
   Then upload and verify the real artifacts; deploy `get.hollowscript.org`;
   publish the mirror and both extension-store listings; and replace the site's
   pre-release notice with verified installation instructions and the
   supported-platform matrix.

Moving Studio and Cascade to this runtime, retiring the older TypeScript
reference, and adding an interactive REPL remain later development work, not
first-release gates. The CLI currently has no REPL command.
