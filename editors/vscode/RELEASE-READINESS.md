# HollowScript VS Code 0.1.1 preview release

Prepared on 2 November 2026 against the public HollowScript v0.1.1 release.
The selected Marketplace identity is `soulstack.hollowscript`, using the same
publisher as Hollow Code. Open VSX is a second, separately verified distribution target.
[HollowScript 0.1.1](https://marketplace.visualstudio.com/items?itemName=soulstack.hollowscript)
is live on Visual Studio Marketplace as Preview. Fresh installation by Marketplace
ID and version passed. Published VSIX SHA-256:
`e0171a6ac40ec74a22623ae373b9606c58cb40621a4033357841b05acff59007`.
Open VSX publication remains pending in internal ticket PRO-315.

## Implementation

- Member completion covers String, list, Map, Set and Result methods,
  record fields, enum variants and the json/time namespaces. It supports
  partial names, chains, generic records, imported declarations, unsaved
  imports, Unicode and flow narrowing.
- The canonical language tooling runs in a bounded worker. The released
  native v0.1.1 CLI and LSP remain usable without replacement.
- References and rename load the declaring module before asking v0.1.1
  for edits, so an unopened exported declaration is included.
- Windows file URIs match the released server's wire format, keeping open
  documents and imported declarations consistent. The acceptance installer
  handles both original and versioned Windows editor archive layouts.
  Editor test offsets are taken from the actual document, including CRLF
  line endings on Windows.
- A legacy-position adapter preserves Unicode during edits and rename,
  including references inside interpolation after an emoji; navigation and
  semantic highlighting use UTF-16 positions too.
- Quick fixes use exact canonical diagnostic spans to accommodate the
  released server's whole-line ranges. Missing uses, missing effect and
  unused-declaration fixes are exercised in the editor, including applying
  edits and checking the resulting diagnostics.
- Tracing follows its extension setting at the normal editor log level and
  remains off when disabled, independently of the editor's global log level.
- Windows discovery skips shell wrappers and searches native executables.
  Copied install commands select the supported public v0.1.1 release.
- Consumer-facing setup, help, privacy and refactoring limits are concise;
  the approved Script Fold icon matches its canonical brand exports and the
  current website artwork is used in the listing.
- Dependency versions and overrides are pinned. The package includes npm
  notices, project notices and the specification attribution/licence for
  its derived method-signature catalogue.

## Test evidence

| Check | Result |
|---|---|
| Rust workspace | 1,076 tests pass |
| Rust WebAssembly tooling feature | 120 tests pass |
| JavaScript/WebAssembly boundary suite | Pass, including exact UTF-16 diagnostic spans |
| Conformance corpus | 318/318 pass |
| Specification examples | 99/99 pass |
| Type checking and extension unit tests | 33/33 pass, no skips |
| Production and full npm dependency audits | Zero reported vulnerabilities |
| Production VSIX contents, notices, licence, preview flag and size | Pass |
| Fresh public macOS v0.1.1 install | Signature and checksum verified; CLI/LSP smoke passes |
| Installed VSIX, Apple Silicon macOS, VS Code 1.91.0 | 8/8 editor tests pass |
| Installed VSIX, Apple Silicon macOS, VS Code 1.144.0 | 8/8 editor tests pass |
| Installed VSIX, Restricted Mode, VS Code 1.91.0 | 8/8 editor tests pass; untrusted state asserted |
| Installed VSIX, invalid CLI path, VS Code 1.91.0 | Activation and member completion pass |
| Linux x86_64 container, Node 20 | 16 worker/discovery tests pass; released v0.1.1 LSP smoke passes |
| Native Linux x86_64, Ubuntu 24.04, VS Code 1.91.0 and 1.144.0 | Normal and Restricted Mode: 8/8 each; missing-server mode: 2/2 each |
| Native Windows x86_64, Server 2022, VS Code 1.91.0 and 1.144.0 | Normal and Restricted Mode: 8/8 each; missing-server mode: 2/2 each |

The macOS editor tests use isolated profiles and the installed production
package, with a freshly installed, signature-verified public v0.1.1 CLI.
The normal editor suite checks activation, diagnostics, formatting, hover,
definitions, outline, restart, member completion, cross-file references and
rename, signature help, folding, semantic tokens and quick fixes.

The container checks run under emulation on Apple Silicon. Native Linux
acceptance additionally runs the installed package in the real Electron
editor under Xvfb on a GitHub-hosted Ubuntu 24.04 x86_64 runner.
Windows acceptance runs the same real editor on a native Windows Server
2022 x86_64 runner. Both use freshly installed, signature-verified public
v0.1.1 CLI/LSP binaries and the exact same VSIX:
`20fc2b82b816536b3af547691fbaa5365f48404c30415c2b8d638ea9a640431d`.
The successful native matrix (private CI run 41437804680)
contains 12 scenarios and 72 passing editor assertions. Normal and Restricted
Mode scenarios also verify that protocol tracing is actually recorded without
forcing the editor itself to Trace logging. Evidence is retained
locally with the release output; temporary CI artifacts and the remote test
branch are removed after download. No cloud instances were provisioned.
GitHub reported zero net Actions charges, with the organisation's zero paid
Actions budget unchanged. Billing usage can arrive later.

## Open VSX acceptance

VSCodium 1.143.17232 passes the installed-package normal, Restricted Mode and
missing-server suites on native Apple Silicon macOS, Ubuntu 24.04 x86_64 and
Windows Server 2022 x86_64. Each platform runs 18 editor assertions against the
signature-verified public v0.1.1 CLI. The Windows/Linux
acceptance run (private CI run 41450026414)
uses VSIX SHA256 `e0171a6ac40ec74a22623ae373b9606c58cb40621a4033357841b05acff59007`.
The prior VS Code matrix above tested the same runtime with earlier README copy.
Native Linux Theia IDE (API 1.139.0) also passes all eight language checks in
the Theia acceptance run (private CI run 41450026862).
Theia omits some VS Code testing commands; those checks invoke the actual
registered providers against the released CLI while preserving their native
registration. No provider responses are mocked. The official AppImage SHA256
is `81b70a515884aa4a4094d06f04168779aee8699adae3f6e07aada5d7fb8d11a7`.

## Publisher access and remaining distribution work

- The existing `soulstack` Marketplace publisher was verified through owner
  sign-in. The exact reviewed VSIX was published after owner authorisation
  on 2 November 2026, and fresh Marketplace installation passed.
- Hollow Code's repository-scoped Marketplace federation was not broadened
  to the HollowScript repository. Cached Azure CLI credentials do not gain
  access merely because browser sign-in succeeded.
- Open VSX still needs Eclipse account linking, owner review and acceptance
  of its Publisher Agreement, and verified Soulstack namespace ownership.
  The authorised public [ownership request](https://github.com/EclipseFdn/open-vsx.org/issues/15148)
  has been submitted. PRO-315 tracks these remaining steps and publication
  of both exact reviewed packages, followed by fresh installation checks.
- Consumer-facing availability should say only "Open VSX coming soon".
  Account and agreement details belong in internal release records.

The language and extension remain previews. References and rename cover the
declaration and open importers, rather than a whole-workspace index. Source,
module, memory and time limits for local analysis are documented in README.
