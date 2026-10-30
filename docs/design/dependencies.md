# Dependencies

## `xtask`'s own dependencies

`xtask` is a repository helper (`publish = false`), not a shipped crate, so
it is not covered by the "none in the language itself" below, which is
about what ships. Its release-signing test (`xtask/src/sign.rs`, proving
the minisign encoding `cargo xtask sign` writes is byte-compatible with
the real `minisign` tool and with `openssl`, without needing Cloud KMS
credentials to do it) needs an in-memory Ed25519 key that exists only for
that test:

| Crate | Licence | Used for |
|---|---|---|
| `ed25519-dalek` | BSD-3-Clause | The test-only local signing key |
| `rand` | MIT OR Apache-2.0 | Generating that key and its key id |
| `base64` | MIT OR Apache-2.0 | Encoding minisign's signature and public key files, and the SSH signature's armour (used outside tests too, by `sign_release` and `sign_ssh` themselves) |
| `sha2` | MIT OR Apache-2.0 | The SHA-512 digest the SSH signature format signs over (used outside tests too, by `sign_ssh`); already in the tree through `ed25519-dalek` |

`cargo xtask graph` enforces this the same way it enforces the crates
below: an entry must be in both that check's `EXTERNAL` table and this
file, or the dependency is refused.

## Today: none in the language itself, two in the CLI host

`hollowscript-core`, `hollowscript-bytecode`, `hollowscript-syntax`,
`hollowscript-check`, `hollowscript-compiler`, `hollowscript-vm`, the
`hollowscript` facade, `hollowscript-capi`, `hollowscript-wasm` and
`hollowscript-ide` (the editor services a browser build links) depend on
the standard library and, `hollowscript-core` aside, on other crates of this
same workspace, and nothing else: `cargo tree -e normal` for each of them
lists no third-party crate. This is deliberate policy, not an accident of
what has been implemented so far: the crates that touch untrusted script
input directly are required to have no dependency beyond
`hollowscript-core`, so that the surface a reviewer must audit for the
interpreter's own safety is exactly this workspace's own code. `cargo xtask
graph` enforces the crate graph this implies, including this zero-dependency
rule for those crates; a dependency outside the workspace would need an
entry in that check's own approved list before it could be added at all, and
this file exists to be the second place such an entry is recorded, with its
licence, so the two cannot drift apart.

`hollowscript-cli` is not held to that rule: it is the host process, not the
language, and section 16.8's `fs` capability needs `cap-std` (below) to
implement its scope check honestly. `hollowscript-lsp` and
`hollowscript-conformance` have no third-party dependency of their own
today either (the language server reads files through `hollowscript-hostfs`,
below, so it links `cap-std` through that crate), but, being host-side
tooling rather than the language itself, are not held to the rule and may
gain one without it being a policy change.

## Every dependency's licence must be Apache-2.0 or MIT compatible

The Windows host filesystem directly pins `cap-primitives` 4.0.3 (MIT OR
Apache-2.0, with an additional Apache-2.0 WITH LLVM-exception option), already
used by `cap-std`. Its narrow `_WindowsByHandle` trait preserves the optional
handle link count; the convenience trait in `cap-fs-ext` would panic if that
count were absent. Missing counts and counts other than one are refused
normally. This interface is version-pinned and adds no unsafe code.

The workspace's own licence is `Apache-2.0 OR MIT` (set once, in
`Cargo.toml`'s `[workspace.package]`, and inherited by every crate). A
dependency added in the future must carry a licence compatible with
distributing this workspace under that choice: Apache-2.0, MIT, BSD-2-Clause,
BSD-3-Clause, ISC, Zlib, Unicode-3.0 and similar plain permissive licences
qualify without further thought; anything copyleft (any version of the GPL,
LGPL, or MPL) needs a considered decision before it is added, because it can
change what the workspace as a whole may be licensed as. This is enforced
mechanically, not left to review discipline: `deny.toml` at the workspace
root configures `cargo-deny` with exactly that allow-list, plus
`sources.unknown-registry = "deny"` and `sources.unknown-git = "deny"` (a
dependency must come from crates.io, not an arbitrary git remote) and
`advisories.yanked = "deny"`. `scripts/check` runs `cargo deny check` as one
of its steps if `cargo-deny` is installed (`cargo install cargo-deny`), and
prints where to get it and continues, rather than failing the whole gate,
if it is not; a contributor who has it installed gets the real check, and
one who does not is told plainly rather than being blocked from every other
check by a tool that is not this workspace's own code.

**Third-party licence text for a release.** Every release archive carries
a `THIRD-PARTY-LICENSES` file written by `cargo xtask third-party-licenses`
(`xtask/src/licenses.rs`) for exactly the crates linked into that archive's
binaries or libraries, for that archive's target: `cargo tree -e normal
--target <target> -p <package>`, once per shipped package, so development,
build and `xtask` dependencies, procedural macros and other platforms'
dependencies are all left out. For each crate the file reproduces, verbatim,
every licence, copyright and notice file the crate itself ships (from its
unpacked source under Cargo's registry), with its declared authors; a crate
that ships none is an error, not a line pointing elsewhere. It also carries
the Rust standard library's own notices (the toolchain's
`COPYRIGHT-library.html`, as plain text) and, for the Linux and Windows
targets, the notices of the C runtime linked statically into them (musl,
mingw-w64). `cargo xtask sbom` writes the release's CycloneDX SBOM from the
same crate sets. Neither file is checked in: both are generated fresh for
each release by `scripts/release-build.sh` (docs/design/release.md,
section 1).

## Landed: `ureq` and `rustls`, the `http` capability

The specification's `http` capability (section 16.9) needs a real network
client with TLS: connecting, following redirects within the limits section
16.9 states, and refusing addresses that resolve to loopback, private,
link-local, multicast or metadata ranges unless the grant names that literal
address (section 16.9, "Addresses"). None of this can be built from the
standard library alone.

**The decision (owner-approved).** `hollowscript-cli` depends on
[`ureq`](https://crates.io/crates/ureq) `=3.4.2` (MIT OR Apache-2.0; pinned
exactly, not a range, for the reason below), `default-features = false`,
`features = ["rustls"]`. A small, synchronous HTTP client is the right shape
for a command-line tool that does not otherwise need an async runtime:
pulling in an asynchronous runtime crate purely to make one capability's few
request functions work would add far more to the binary's size and
dependency count than the capability itself is worth. `rustls` is the pure-Rust
TLS implementation this pulls in, rather than one that links a platform TLS
library, which keeps the CLI's binary reproducible and its size predictable
across the platforms it is built for.

**The crypto provider.** `rustls` needs a cryptography backend; `ureq`'s
`rustls` feature selects [`ring`](https://crates.io/crates/ring) (licence
`Apache-2.0 AND ISC`) rather than the other option in the ecosystem,
`aws-lc-rs`, which bundles a derivative of BoringSSL/OpenSSL under a more
complex licence mix and needs a C toolchain and `cmake` to build. `ring`'s
own licence is two plain permissive licences that both distribute cleanly
under this workspace's `Apache-2.0 OR MIT`, and it has no such build-time
dependency, which matters more for a CLI meant to build predictably
everywhere than the small run-time difference between the two would. The
root certificate bundle `rustls`'s `webpki-roots` (pulled in transitively)
ships is under `CDLA-Permissive-2.0`, a permissive data licence for the
bundle's data rather than code; `deny.toml` allows it explicitly, with the
same reasoning recorded there.

`ureq`'s other transitive dependencies, with the `rustls` feature and no
others, are all small and permissively licensed: `http`, `httparse`,
`ureq-proto`, `base64`, `log`, `percent-encoding`, `itoa`, `utf8-zero`
(MIT OR Apache-2.0), `rustls-pki-types`, `rustls-webpki`, `untrusted`
(ISC), `subtle` (BSD-3-Clause), `once_cell`, `zeroize`, `cfg-if`,
`getrandom` and, on the platforms that need them, `windows-sys` and its own
small `windows_*` support crates (MIT OR Apache-2.0). `cargo-deny` (see the
dependency-licence gate, above) checks all of this mechanically.

**Why `ureq` is pinned exactly.** `crate::http::SafeResolver`
(`crates/hollowscript-cli/src/http.rs`) implements `ureq`'s `Resolver` trait
to satisfy section 16.9's "Addresses" paragraph: it performs the one DNS
lookup a connection is allowed, refuses an address that paragraph refuses
unless the grant names it literally, and hands the connector exactly one
already-checked address, so nothing resolves again between the check and
the connection. This lives under `ureq::unversioned`, which the crate's own
documentation says it does not consider covered by semver: a minor version
bump could change or remove it without notice. `Cargo.toml` pins `=3.4.2`
so that upgrading is a deliberate, reviewed step instead of something
`cargo update` could do silently.

**What this implementation approximates rather than matches exactly**,
recorded honestly rather than left for a reviewer to discover:
internationalised host names are compared as lower-cased ASCII only, with no
IDNA/Punycode normalisation (section 16.9 asks for `xn--`-form comparison);
the check for a "disguised numeric host" in a non-canonical form covers the
specification's own named examples but not every historical `inet_aton`
acceptance rule; and response decompression is limited to `gzip` (`ureq`'s
`gzip` feature), so a request that itself sets `Accept-Encoding: br` and
gets a Brotli body back will fail to decode it as UTF-8 rather than
decompressing it. `crates/hollowscript-cli/src/http.rs`'s own module
documentation states these in the same terms.

`hollowscript-cli`'s other standard capabilities (`print`, `clock`,
`random`, `env`, `args`, `fs` and now `http`) are all implemented; no
standard capability is left unoffered.

## Landed: `hollowscript-hostfs`, and `libc` for one constant

The command line host and the language server both turn a name into an open
file, and the rules for doing it (relative to a directory handle, no symbolic
link, a regular file with one name, opened without waiting on a named pipe,
read through a limit) are one piece of code, `crates/hollowscript-hostfs`, so
the two cannot disagree. It is host-side and knows nothing of the language;
`cargo xtask graph` allows `hollowscript-cli` and `hollowscript-lsp` to depend
on it and it on nothing in the workspace. Its two third-party dependencies are
the ones the host already has: `cap-std` (below) for the handle, and, on
Unix only, [`libc`](https://crates.io/crates/libc) `0.2` (Apache-2.0 OR MIT)
for the value of `O_NONBLOCK`, which differs between operating systems and
is not in `std`. It is already in the build through `cap-std`, and nothing
here uses `unsafe`.

## Landed: `cap-std`, closing the `fs` capability's TOCTOU window

Section 16.8's sandbox check and the filesystem operation that follows it
used to be two separate syscalls on the same path string, not one atomic
operation on a resolved handle: `std::fs::canonicalize` against the granted
`ScopeRoot`s, then a plain `std::fs::read`, `std::fs::write`,
`OpenOptions::open`, and so on. Between the two, a symlink could be created
or repointed, by another process or, on a shared host, by a concurrently
running script under a different grant, to redirect the operation outside
every root the grant named: a classic check-then-use race that could make
`fs:read=./data` reach a path the grant never named.

**The decision (owner-approved).** `hollowscript-cli` now depends on
[`cap-std`](https://crates.io/crates/cap-std) `4.0.3` (Apache-2.0 OR MIT;
`default-features = false`, which this crate has no default features to
begin with, so nothing beyond the crate's core filesystem module is pulled
in). `ScopeRoot::new` (`crates/hollowscript-cli/src/capabilities.rs`) opens
each granted root once, at grant time, as a `cap_std::fs::Dir` with
`open_ambient_dir`, the one access made with the host's own ambient
authority. Every `fs` operation afterwards is a single call through that
handle (`Dir::read`, `Dir::write`, `Dir::open_with`, `Dir::read_dir`,
`Dir::remove_file`, `Dir::remove_dir`, `Dir::create_dir_all`, `Dir::is_dir`,
`Dir::is_file`, `Dir::try_exists`) with a path relative to it, so `cap_std`
resolves the whole path beneath the handle in the same step that performs
the operation: it cannot be redirected outside the root by a symlink
swapped in afterwards, on Linux (`openat2` `RESOLVE_BENEATH` where the
kernel has it), macOS and other BSDs (a manual, `O_NOFOLLOW`-checked walk),
or Windows (reopening by handle), which is what `cap-std` itself implements
and is exercised by
`capabilities::tests::a_symlink_swapped_in_after_the_grant_cannot_escape_the_root`.

Picking which of possibly several granted roots a script's path belongs to,
and what the remaining relative path into it is, is still a check the host
does itself (`check_scope`, `lexical_absolute`), but it is now purely a
textual operation, joining a relative path onto the current directory and
collapsing `.`/`..` segments without any filesystem access or symlink
resolution: there is nothing left in it for a swapped-in link to race
against, because the actual, symlink-aware resolution the specification
describes happens exactly once, inside `cap_std`, atomically with the
access. This also fixed a related correctness gap the old, fully-resolving
`canonicalize`-based check happened to paper over only by accident: a
granted root reached through a path alias (a working directory or `--allow`
flag naming a symbolic link, a platform temporary directory being the
common case) is matched consistently now because both sides of the
comparison are resolved the same, lexical way, rather than one side being
resolved through the filesystem and the other not.

`cap-std`'s only further dependencies pulled in by its filesystem module,
with no default features enabled, are small, platform-specific, permissively
licensed crates (`rustix`, `io-lifetimes`, `io-extras`, `cap-primitives`,
`fs-set-times`, `ambient-authority`, and, on the relevant platforms,
`errno`, `libc`, `bitflags`, `windows-sys`/`winx`, `linux-raw-sys`,
`rustix-linux-procfs`); every one of them is Apache-2.0 OR MIT (several also
offer BSD-2-Clause), so the workspace's own licence choice is unaffected.
`cargo-deny` (below) checks this mechanically rather than leaving it to this
paragraph's word.
