# Release: building, signing, uploading, mirroring and installing

This document describes the pipeline that turns a tagged commit on this
repository's default branch into a release a user can verify and install,
and the scripts under `scripts/` and `xtask/` that implement each stage. It
complements [release-and-milestones.md](release-and-milestones.md), which
covers the tree layout and the mirror's boundary (`internal/`); this document
covers the mechanics.

## v0.1.0 preview distribution scope

The first preview distributes only `aarch64-apple-darwin`, verified on
Apple Silicon macOS. Build it with
`scripts/release-build.sh --version v0.1.0 --targets aarch64-apple-darwin`.
Other target descriptions below document the build machinery, not supported
downloads for this preview. The VS Code client is available in source;
Marketplace, Open VSX and registry packages are deferred.

Each native product must match a second build from a different checkout and
Cargo home. Both builds run on one host; this is not an independent rebuild.
The release includes a fixed-field build record named
`hollowscript-0.1.0-aarch64-apple-darwin-build.json`, covered by `SHA256SUMS`
and both KMS signatures. It records the public source tag and commit, pinned
Rust toolchain, target, reproduction command and payload hashes. It records
the builder only as "macOS, Apple Silicon". It makes no claim to independent
or hosted build provenance.

The `latest.txt` pointer has no signed expiry. Publish this limitation in
the installation guide and recommend an explicit version for the preview;
freshness protection remains work for a stable release. The tagged
specification is the preview's reference snapshot, not a promise of 1.0
stability.

> **First-release status, audited 2026-10-07:** v0.1.0 has not been tagged
> or published. The 2026-10-06 checks were rehearsals against an
> uncommitted working-tree snapshot; they did not upload to the real bucket
> or push the public mirror.

The first release is **v0.1.0, a preview**. In order, a release is:

1. On the release commit, the workspace version in `Cargo.toml` is already
   `X.Y.Z`, and `CHANGELOG.md`'s `## [X.Y.Z] - TBD` heading carries the
   release date instead of `TBD`.
2. `git tag -a vX.Y.Z` on that commit, pushed to origin.
3. `scripts/release-build.sh --version vX.Y.Z` (section 1).
4. `cargo xtask sign --sums dist/SHA256SUMS --version vX.Y.Z ...` (section 2).
   Before the first real upload, verify an SSH-format signature produced by
   the KMS key with `ssh-keygen -Y verify`; local-key fixture coverage alone
   does not verify this path with the production key.
5. `scripts/release-upload.sh vX.Y.Z --source dist` (section 3).
6. For the first release, and for any release that changes an installer or
   the site's configuration, provision and deploy `get.hollowscript.org`
   from the tag (section 8). A release that fixes a vulnerability also raises
   the
   installers' release floor (section 5, "The latest release, and the
   floor"), in the same change as the release commit, and is deployed after
   step 5, so no installer ever names a latest that is not yet uploaded.
7. `scripts/release-mirror.sh vX.Y.Z --mirror DIR`, then the printed push
   (section 4).
8. Publish the VS Code extension to both the Visual Studio Marketplace and
   Open VSX for the first release; for later releases, publish when it
   changed (`editors/vscode/PUBLISHING.md`).

The current sequence does **not** publish the Rust crates to crates.io.
Whether the 12 publishable crates are part of v0.1.0 remains an owner
decision. Do not treat the crates.io names as reserved or publish them until
that decision and a dependency-ordered publish procedure are recorded. The
recommendation is to defer crates.io from v0.1.0 because the procedure and
account setup are not ready; include it only if both are completed before the
tag. npm and container-image publication are not part of this release plan.

The release produces reproducible artifacts, a CycloneDX SBOM and signed
checksums. It does not currently produce a machine-readable build-provenance
attestation tying artifacts to the source commit and build environment. The
recommendation is to require a signed attestation for v0.1.0; otherwise the
owner must record a waiver.

No release script applies Terraform or pushes the mirror commit; those are
separate, explicit actions run by a person. `scripts/release-upload.sh` writes
the verified artifacts to the release bucket. The private Hosting deployment
tool publishes the download routes to Firebase Hosting. The upload script
refuses to run unless the artifacts verify against the repository's pinned
key and were built for exactly the version being published.

## 1. Building a release: `scripts/release-build.sh`

```
scripts/release-build.sh --version vX.Y.Z [--targets t1,t2,...] [--out DIR]
                         [--rehearsal] [--no-verify-reproducible]
```

### Artifact targets in the build script

The table describes artifacts the build can produce; it is not the v0.1.0
support matrix. The default build includes these targets, but the first-release
support list is still an owner/engineering decision. Advertise only targets
whose installer behaviour has been verified on an appropriate clean host;
exclude the rest from v0.1.0 or complete those checks first (see the release
gate below).

| Target | Built with | CLI archive | SDK archive |
|---|---|---|---|
| `aarch64-apple-darwin` | `cargo build` | yes | static library and dylib |
| `x86_64-apple-darwin` | `cargo build` (Xcode's linker, from Apple Silicon) | yes | static library and dylib |
| `x86_64-unknown-linux-musl` | `cargo zigbuild`, fully static | yes | static library |
| `aarch64-unknown-linux-musl` | `cargo zigbuild`, fully static | yes | static library |
| `x86_64-pc-windows-gnu` | `cargo zigbuild` | yes (`.zip`) | static library, DLL and import library (`.zip`) |
| `wasm32-unknown-unknown` | `cargo build`, `hollowscript-wasm` only | no | the WebAssembly VM and its JavaScript loader |

There are no `-gnu` Linux builds: one static musl binary runs on every Linux
distribution, glibc or musl alike, so the installer has no libc to detect and
nothing to get wrong. Windows keeps building so it is ready, but whether it
ships in v0.1.0 depends on the supported-target decision and native Windows
testing. Cross-building the executable does not establish tested Windows
support; the remaining platform checks are listed under "Rehearsals and the
first public release" below.

Every requested target must build. A target that cannot be built (no `zig`,
no `cargo-zigbuild`, a missing linker, a failed build) fails the whole run
with a non-zero exit before anything is packaged; nothing is ever skipped
with only a line in a summary.

**Archives.** For each target:

- `hollowscript-X.Y.Z-<target>.tar.gz` (`.zip` on Windows), the **CLI
  archive**: `hollowscript` and `hollowscript-lsp` (the `lsp` subcommand
  starts the language server binary installed next to it), `README.md`,
  `LICENSE-APACHE`, `LICENSE-MIT`, `NOTICE` and `THIRD-PARTY-LICENSES`. This
  is the only archive the installers fetch.
- `hollowscript-sdk-X.Y.Z-<target>.tar.gz` (`.zip` on Windows), the **SDK
  archive**: `include/hollowscript.h` (hand-written, kept in step with
  `crates/hollowscript-capi`), the C libraries under `lib/`, a README
  (`scripts/lib/README-sdk.md`) and the same four legal files, with
  `THIRD-PARTY-LICENSES` written for the C library's own crate graph. For
  `wasm32-unknown-unknown`, `hollowscript_wasm.wasm`, `hollowscript.js` and
  the web README.

Plus `hollowscript-X.Y.Z-sbom.cdx.json`, `SHA256SUMS` over every archive and
the SBOM, and `release-build-info.txt` (how the build was made; not part of
the release and never uploaded). Only named build products are copied into an
archive, each checked to exist; the output directory must be empty to start
with, so nothing from an earlier build can end up in a release.

### The consistency guard

The version is named in five places, and a release is refused unless all of
them agree:

| Where | Checked by |
|---|---|
| `--version vX.Y.Z` | `release-build.sh` (required, `vX.Y.Z` or `vX.Y.Z-suffix`) |
| the workspace version in `Cargo.toml` | `release-build.sh`, before building |
| the git tag on HEAD | `release-build.sh`: a tag named exactly `vX.Y.Z` must point at HEAD |
| what the built binaries report | `release-build.sh`, after building: `hollowscript --version` must print `hollowscript X.Y.Z`, and a C program linked against the SDK must get `X.Y.Z` from `hs_version()` (which now returns the crate version, not a fixed string); on the host target, and on `x86_64-apple-darwin` too under Rosetta. The installers check the first again on the machine they install on. |
| the signed trusted comment | `cargo xtask sign` writes exactly `hollowscript SHA256SUMS, version vX.Y.Z` and refuses a `SHA256SUMS` naming a file built for any other version; `release-upload.sh` and both installers refuse any other comment |
| the SSH signature's namespace | the same signing run writes `SHA256SUMS.sshsig` in the namespace `hollowscript-release-vX.Y.Z`; `release-upload.sh` and the installers that use it verify in exactly that namespace |

`release-build.sh` also refuses a dirty working tree. `--rehearsal` relaxes
exactly two checks, the clean tree and the tag, for a local rehearsal; the
build records that in `release-build-info.txt`, and `release-upload.sh`
refuses to upload a rehearsal build anywhere but a local directory.

### Stripping

Every binary is stripped by rustc itself: the workspace's `dist` profile is
the release profile with `debug = false` and `strip = "symbols"`
(`-C strip=symbols`), which works the same for Mach-O, ELF, PE and
WebAssembly. The earlier approach, `zig objcopy --strip-all` after the build,
fails outright with zig 0.16 ("unimplemented"); the script hid that error and
went on to package the zero-byte `hollowscript.stripped` it had left behind.
No step in the build hides an error now. A static library keeps the symbols a
C linker needs, as it must.

### Reproducibility, and no local paths

`--verify-reproducible` (the default) builds every requested target a
second time from a copy of the tree at another path, with another target
directory and another `CARGO_HOME` (whose registry sources are unpacked
afresh under the new path), applies the same post-link steps to both, and
fails unless every shipped product (twenty, for the six targets) is
byte-identical. It used to rebuild the host's CLI only, which is why the two
Windows and dylib problems below went unnoticed until two whole releases
were compared. Separately, every packaged binary and library is searched
for the builder's home directory, the checkout path, the target directory,
`CARGO_HOME` and the toolchain's own path, and the build fails if any is
found. The second check is what caught the leak the first one could not see
on its own: panic locations in dependencies embedded the full path of every
crate under `~/.cargo/registry`, home directory included, because only the
checkout and target directory were remapped. `CARGO_HOME` and the toolchain
directory are now remapped too, and C code a dependency compiles (`ring`)
gets the same prefixes through `-ffile-prefix-map`.

What reproducibility rests on, each confirmed necessary by making two
checkouts of the same commit disagree until it was applied:

1. `--remap-path-prefix` on the checkout, the target directory,
   `CARGO_HOME` and the toolchain directory, each to a fixed prefix.
2. `-C metadata`, fixed to a constant: Cargo derives part of every crate's
   symbol-mangling disambiguator from the absolute manifest path, which no
   remap reaches.
3. One package per `cargo build` invocation, never `-p a -p b`: the C
   library's `cdylib` changes the CLI's own bytes when built in the same
   invocation.
4. On macOS, rewriting `LC_UUID` (`scripts/lib/patch-macho-uuid.py`) to a
   hash of the binary's own bytes, then signing it again ad hoc: ld64 derives
   `LC_UUID` from object file names that depend on the checkout path, and
   dyld refuses a binary with no `LC_UUID` at all. The ad-hoc signature
   records the file's name, so the two builds are compared under the same
   name.
5. On macOS, the C library's install name set at link time
   (`-install_name @rpath/libhollowscript_capi.dylib`). ld64 otherwise
   records the absolute build path, and sizes the header for it, so
   rewriting it afterwards with `install_name_tool` (the first attempt) still
   left every section offset by that path's length.
6. On Windows, the COFF header's `TimeDateStamp` set to `SOURCE_DATE_EPOCH`
   (`scripts/lib/patch-pe-timestamp.py`): the linker writes the build clock
   there, and zig's linker driver rejects `--no-insert-timestamp`.
7. The toolchain, pinned to one exact release in `rust-toolchain.toml`
   (1.98.1), not a channel, so a tag names one compiler.
8. `SOURCE_DATE_EPOCH` (the release commit's author date) for every archive
   entry's timestamp, set in UTC; entries sorted, owner and group 0, fixed
   permissions, `ustar` format with no extended attributes, and `gzip -n`.

None of the Apple builds are notarised: this project ships unsigned binaries
over HTTPS rather than paying for OS code signing unless a specific trigger
makes it necessary (a browser download button, a managed fleet, or Smart App
Control blocking an unsigned binary).

### Legal files and the SBOM

`THIRD-PARTY-LICENSES` is generated per archive by `cargo xtask
third-party-licenses` for exactly the crates linked into that archive's
binaries for that target (see [dependencies.md](dependencies.md)): the full
licence, copyright and notice files each crate ships, its authors, the Rust
standard library's own notices, and the statically linked C runtime's
(musl's on Linux, mingw-w64's on Windows, both taken from the zig
installation that linked them). `NOTICE` is the repository's own.

`hollowscript-X.Y.Z-sbom.cdx.json` is a CycloneDX 1.5 document written by
`cargo xtask sbom` from the same crate sets: every crate linked into any
shipped artefact, once, with its licence expression, package URL, the
`.crate` file's SHA-256 from `Cargo.lock`, and a property naming each
artefact it is linked into. Development, build and `xtask` dependencies are
in no artefact, so they are not listed; no local path is written. It is in
`SHA256SUMS`, so it is signed with the rest of the release.

`deny.toml` checks the dependency graph for exactly the shipped targets.

## 2. Signing

**Owner decision:** a non-exportable Cloud KMS Ed25519 key, signing
`SHA256SUMS` in minisign's own `Ed` (legacy, unhashed) format.

### The key

Provisioned in the infrastructure repository: a key ring and an
`ASYMMETRIC_SIGN` / `EC_SIGN_ED25519` key, `prevent_destroy = true`, and a
`roles/cloudkms.signerVerifier` grant scoped to the key alone, to a principal
supplied at apply time. Protection level is `SOFTWARE`: Cloud KMS's HSM level
does not support `EC_SIGN_ED25519`; the key is non-exportable regardless,
which is the property the decision asked for. Applied 2026-09-23.

### The signing code: `xtask/src/sign.rs`

A `Signer` trait ("sign these bytes") with two implementations: `KmsSigner`,
which shells out to `gcloud kms asymmetric-sign` and never holds key
material, and, test-only, `LocalSigner`, an in-memory key that exists so the
minisign encoding can be exercised without KMS credentials. Both go through
the same `sign_release`, which writes minisign's exact `Ed`-format signature
file (an untrusted comment, `base64("Ed" || key_id || signature)`, a trusted
comment, and a second, global signature over `signature || trusted_comment`)
and public key file (`base64("Ed" || key_id || public_key)`).
`cargo test -p xtask sign::tests::minisign_and_openssl_verify_a_locally_signed_release`
verifies the result with the real `minisign` and with `openssl pkeyutl`.

```
cargo xtask sign --sums dist/SHA256SUMS --version vX.Y.Z --key-version <KMS key version>
                 --key-id <hex> --public-key <hex>
```

`--version` is required. The trusted comment is always exactly
`hollowscript SHA256SUMS, version vX.Y.Z` (it is no longer configurable,
since the installers and the upload script accept nothing else), and the
command refuses a `SHA256SUMS` naming any file not built for that version:
every entry must be `hollowscript-X.Y.Z-<target>...`,
`hollowscript-sdk-X.Y.Z-<target>...` or the SBOM, where `<target>` starts
with a known architecture, so that `hollowscript-0.1.0-rc.1-...` is not taken
for a 0.1.0 file.

| Value | |
|---|---|
| `--key-version` | the key version's resource name, supplied at release time from configuration kept outside the public source tree |
| `--public-key` | `3abcb25cfd344928ef89140c4c0c75c8dcb0d2d263af33f4ad4c669ab4cb898a`, the last 32 bytes of the DER `SubjectPublicKeyInfo` that `gcloud kms keys versions get-public-key` returns |
| `--key-id` | `c477f30c6e895892`, the first 8 bytes of the SHA-256 of that public key |
| minisign public key | `RWTEd/MMbolYkjq8slz9NEko74kUDEwMdcjcsNLSY68z9K1MZpq0y4mK` (`minisign -V` prints key ids byte-reversed, as `9258896E0CF377C4`), in `scripts/hollowscript-release.pub` and embedded in both installers |

Verified end to end on 2026-09-23: a test `SHA256SUMS` signed through
`cargo xtask sign` with the real KMS key verified with `minisign -V`, and the
same file with one byte appended was refused.

### A second signature, in the SSH format

minisign is not on a stock macOS or Windows machine, so an installer that
could verify only a minisign signature either could not verify at all or had
to trust a `SHA256SUMS` fetched from the same origin as the archive, which
defeats the point of signing it. OpenSSH is on both, and `ssh-keygen -Y
verify` verifies an Ed25519 signature, but only one in the SSH signature
format. A Cloud KMS `EC_SIGN_ED25519` key signs arbitrary bytes with plain
Ed25519, so the same key can sign that format too: `cargo xtask sign` now
makes one more `gcloud kms asymmetric-sign` call and writes
`SHA256SUMS.sshsig` next to `SHA256SUMS.minisig`.

What changes, and what does not:

- **The key and who can sign do not change.** The public key is the same 32
  bytes; there is no second key to provision, protect or rotate, and the
  installers derive the OpenSSH public key from the pinned minisign key file
  at run time, so there is one pinned key and not two copies of it.
- **What is signed is a fixed structure, not the file.** The SSH signature is
  over the literal `SSHSIG`, the namespace, a reserved empty string, the hash
  algorithm (`sha512`) and the SHA-512 of `SHA256SUMS` (the format's own
  definition, as `ssh-keygen -Y sign` writes it). The minisign signature is
  over the raw file, and its global signature over a 64-byte signature
  followed by the trusted comment. Neither can be replayed as the other: a
  `SHA256SUMS` file does not begin with `SSHSIG`, and the key never signs a
  message of one kind that parses as the other.
- **The version is bound the same way.** The namespace is
  `hollowscript-release-vX.Y.Z`, and a verifier checks it, so a signature
  made for one version fails for any other, as the signed trusted comment
  makes a minisign one do.
- **No additional trust in the host that signs.** `xtask/src/sign.rs`
  (`sign_ssh`) builds the structure and asks the same `Signer` to sign it;
  `cargo test -p xtask sign` verifies the result with the real `ssh-keygen`
  (accepted in its namespace; refused in another and for another message).
  The recorded verification used only a local, in-memory key. Before the first
  real upload, verify a KMS-produced SSH-format signature with
  `ssh-keygen -Y verify` against the pinned KMS public key and the expected
  release namespace. This checks the production signing path, which the local
  fixture test cannot establish.

## 3. Uploading a release: `scripts/release-upload.sh`

```
scripts/release-upload.sh vX.Y.Z --source DIR [--bucket gs://BUCKET|DIR] [--dry-run]
                          [--public-key FILE] [--allow-latest-rollback]
```

The bucket is one setting: `--bucket`, or `HOLLOWSCRIPT_RELEASE_BUCKET` when
it is not given. Its name is not written into this public tree; it is
provided to the release operator outside the public source tree. A plain
directory instead of `gs://...` is a local stand-in for the bucket, used for
rehearsal.

Before uploading anything, it refuses:

- a `--public-key` that is not the pinned key. The default is
  `scripts/hollowscript-release.pub`, the key the installers pin, and any
  other key (compared by the key itself, the second line of the file) is
  refused unless `--allow-unpinned-public-key-for-testing` is also given,
  which is itself refused for a `gs://` bucket: it exists for a rehearsal with
  a throwaway key into a local directory, so the key a release is checked
  against can never be an accident of a command line;
- installers that do not both pin that key, or that disagree on the release
  floor, or a stable release older than the floor (section 5), so a release
  no default install would accept, or one an installer would accept from a
  key this script did not check, is never published;
- a `SHA256SUMS.minisig` that does not verify against that key;
- a missing `SHA256SUMS.sshsig`, or one that does not verify, with the same
  key, in the namespace `hollowscript-release-vX.Y.Z` (this needs
  `ssh-keygen` from OpenSSH 8.0 or newer, as well as `minisign`);
- a trusted comment other than exactly
  `hollowscript SHA256SUMS, version vX.Y.Z`;
- a `SHA256SUMS` naming any file not built for this version (the same rule
  as `cargo xtask sign`), or a file missing from `--source` or differing
  from its signed hash;
- for a real bucket, a `--source` whose `release-build-info.txt` is missing,
  names another version, or records a `--rehearsal` build.

**What is checked is what is uploaded.** `SHA256SUMS`, both signatures and
every file `SHA256SUMS` names are first copied from `--source` into a private
temporary directory (mode 0700, removed on exit), and the signatures, the
hashes and the upload all use that copy, never `--source` again. Before, a
file was hashed at one point and read again, to be uploaded, much later; a
`--source` that changed in between (another process, or a build still running
into the same directory) was uploaded as it then was, under a signature that
covered something else, and the read-back afterwards compared the bucket with
the changed file. The copy costs the disk space of one more release.

**What it uploads.** Exactly the files `SHA256SUMS` names (every CLI archive,
every SDK archive and the SBOM), then `SHA256SUMS`, then
`SHA256SUMS.sshsig`, then `SHA256SUMS.minisig` last, so a half-finished upload is one no installer
can install from. Never `install.sh` or `install.ps1` (the
`get.hollowscript.org` site serves those itself, section 8) and never any
other file that happens to be in `--source`.

**Resumable.** An object already in the bucket with identical bytes is
skipped, and its `Content-Type` and `Cache-Control` are corrected if they
differ; an object with different bytes is refused, never overwritten. Each
new object is uploaded no-clobber (`gcloud storage cp --no-clobber`, a
generation-match precondition) and read back to confirm its bytes. Whether
an object exists is decided from `gcloud`'s own answer: "not found" means
absent, and any other error stops the upload rather than being read as
absent.

**Headers.** `application/gzip` for `.tar.gz`, `application/zip` for `.zip`,
`application/json` for the SBOM, `text/plain; charset=utf-8` for
`SHA256SUMS`, `.minisig`, `.sshsig` and `latest.txt`. `Cache-Control` is
`public, max-age=86400` (one day) for every versioned object and
`public, max-age=60, must-revalidate` for `latest.txt`. With no CDN (section
8), these headers are the whole caching policy. Versioned objects never
change, but a day rather than a year of caching (an owner decision) bounds
how long a withdrawn version can still be served (section 6).

**`latest.txt`** moves last, only for a stable version (no `-suffix`), and
never backwards without `--allow-latest-rollback`. Failing to read the
bucket's current `latest.txt` is an error, never taken to mean there is none
(the previous version of this script turned a `gcloud` read error into "no
latest", which would have let any version move it).

**`--dry-run`** prints every command it would run, quoted for pasting, and
runs none of them.

## 4. Release mirror: `scripts/release-mirror.sh`

The mirror is a public repository written only by this process, one commit
per release. Given a tag or commit, the script exports the tree with
`git archive` (no history), removes `internal/`, refuses if a development
tooling file is present, runs `cargo xtask hygiene --root <export>` and
`gitleaks detect --no-git`, and only if both are clean replaces the contents
of a local mirror directory with the export and makes one commit, with a
neutral release identity (`HOLLOWSCRIPT_MIRROR_NAME`/`_EMAIL`, default
"HollowScript Releases <releases@hollowscript.org>"). It prints the push; it
never runs it.

The mirror directory is replaced wholesale, so the script only uses one it
can safely empty: a directory that does not exist yet, an empty one, or a git
repository it made itself (it records that in the repository's own config as
`hollowscript.release-mirror`, never in the tree it exports). A directory with
files and no repository, a repository it did not make, this checkout, a
directory containing it or inside it, a repository's subdirectory and a file
are all refused, before anything is exported or removed; it used to
`git init` any directory without a `.git` and then delete everything in it.
A mirror made by an earlier run is marked with `git -C DIR config
hollowscript.release-mirror true`. `--check-mirror-only` stops after this
check.

It tags the mirror commit with the release tag, and never moves a tag
silently: if the mirror already has that tag and the export differs from
it, it stops before committing and puts the mirror directory back as it was
(the earlier version ran `git tag -f`); exporting the same release again is
a no-op. `--dry-run` does not need
`HOLLOWSCRIPT_MIRROR_REPO` set, since it pushes nothing; the push it prints
then names a placeholder.

## 5. Installers

Both installers follow the same policy:

- **Downloads.** From `https://get.hollowscript.org` by default, which
  redirects `/latest.txt` and `/vX.Y.Z/*` to the release bucket on
  `storage.googleapis.com`. The base URL must be `https://`; plain `http://`
  is accepted only for `127.0.0.1`, `localhost` and `[::1]`, for local
  testing. `install.sh` passes curl `--proto '=https' --tlsv1.2`, which also
  holds for every redirect it follows.
- **Verification, and failing closed.** The archive is always checked
  against `SHA256SUMS`, and `SHA256SUMS` is always checked against its
  signature, before anything is installed. The verifier is the first of
  these the machine has: `minisign`; an `openssl` that can verify Ed25519
  (`install.sh` only); `ssh-keygen` from OpenSSH 8.0 or newer, which checks
  the SSH-format signature of section 2 (`SHA256SUMS.sshsig`) and is on
  macOS, most Linux installs and Windows 10 and 11 with the OpenSSH client.
  The signed version must be the version being installed: the exact trusted
  comment `hollowscript SHA256SUMS, version vX.Y.Z` for the first two, the
  namespace `hollowscript-release-vX.Y.Z` for the third. **With none of the
  three, the installer refuses**, naming how to install one. It used to carry
  on with the SHA-256 check alone, which proves nothing about the server the
  checksums came from: whoever could replace the archive in the bucket could
  replace `SHA256SUMS` too, and the signing that exists to stop that was
  skipped on exactly the machines that have no verifier (a stock macOS, whose
  `openssl` is LibreSSL, and Windows). The one way past it is a deliberate
  opt-out: `HOLLOWSCRIPT_ALLOW_UNSIGNED=1` (`-AllowUnsigned`, or the
  environment variable), accepted only as exactly `1` (`0` and unset mean
  off; anything else is an error), which prints a warning before the download
  and a notice after it. `HOLLOWSCRIPT_REQUIRE_SIGNATURE` (`1`, `true` or
  `yes`, in any case; `0`, `false` and `no` mean off; anything else is an
  error) wins over the opt-out: with both set, an install with no verifier is
  refused. The other settings that change behaviour
  (`HOLLOWSCRIPT_ALLOW_DOWNGRADE`, `HOLLOWSCRIPT_NO_MODIFY_PATH`) take the
  same `1`/`true`/`yes` values and refuse an unrecognised one, instead of the
  literal `1` only. When there is no `curl` and the only downloader is
  BusyBox's `wget` (Alpine's default), a verifier is required even for the
  opt-out: BusyBox's `wget` does not validate TLS certificates (it says so
  itself, "TLS certificate validation not implemented"), so it cannot be
  trusted to have reached the right server, and the signature is the only
  protection that is left. This was read from BusyBox's behaviour and
  source, and tested here only with a stand-in `wget` that answers `--help`
  as BusyBox's does; a real Alpine container was not run.
- **The version.** `vX.Y.Z` or `vX.Y.Z-suffix`, letters, digits, `.` and `-`
  only, each numeric part digits only, checked before it is used in a URL.
  The downloaded `hollowscript --version` must print exactly
  `hollowscript X.Y.Z` before anything is replaced. An explicit version is
  installed as asked (with a warning if it is below the floor, next
  subsection); a version that came from the unsigned `latest.txt` is refused
  if it is older than the installed one, unless
  `HOLLOWSCRIPT_ALLOW_DOWNGRADE=1` (or `true`, `yes`), and if it is below the
  floor.
- **The base URL** is parsed, not matched by prefix: `https://`, or `http://`
  for exactly `127.0.0.1`, `localhost` or `[::1]` with an optional all-digit
  port. The host is what comes before the first `/`; a user name in the URL, a
  space, a backslash, a query or a fragment is refused. A prefix test had
  accepted `http://127.0.0.1:1@example.org`, a request to `example.org`, over
  plain http.
- **Installing.** Both binaries are staged next to the old ones, the old ones
  are kept as backups, each new one is renamed into place, and the installed
  `hollowscript` is run once; any failure or interruption on the way removes
  what was staged and restores the backups. On a first install, where there
  are no backups, the new binaries are removed again and an empty `bin`
  directory with them: a failed check used to leave binaries that had just
  failed it in place, and nothing to restore.

### The latest release, and the floor

`latest.txt` is not signed: it is one line of text in the bucket, and
get.hollowscript.org only redirects to it, with no load balancer or other
server of ours between a client and the bucket (section 8). Whoever can write
to the bucket could therefore point a default install at any release that was
ever published, and a validly signed release is not necessarily a good one:
an old one may have a flaw the current one fixed. The installers could not
tell; the signature on the release they fetched was genuine, and steering a
fresh install from a newer signed release to an older one was demonstrated
against them.

The fix is a **release floor**: the oldest release an install that asked for
"the latest" accepts. It is a line in each installer, `release_floor` in
`install.sh` and `$releaseFloor` in `install.ps1`, so it travels with the
pinned key, in a file served by the site, not by the bucket: writing to the
bucket does not let anyone lower it, any more than choose the key. A
`latest.txt` below it is refused (the message says so and how to override it
by naming the version), and a version asked for by name below it installs with
a warning. The two floors must agree and the upload script checks that,
and that the key each installer pins is the one release verification used, and
refuses to publish a stable release older than the floor (so `latest.txt` can
never be made to name one). The value is `v0.1.0`, the first release. A change
that fixes a vulnerability in release X raises both floors to X in the same
commit as the release, and is deployed after the upload of X (section 8): the
upload first, so an installer never names a latest that is not yet there, and
the site second, so the old installers keep working until it changes.

What it does not do, so it is not mistaken for more:

- **Rollback inside the window.** `latest.txt` can still be moved to any
  release from the floor up, and replayed: an attacker with bucket write can
  hold a default install at the previous release, or any between it and the
  floor, indefinitely (a freeze). A signature over `latest.txt` would not
  change that, since an old signed `latest.txt` can be served again; only a
  signed manifest with an expiry would, and that needs a re-signing on a
  schedule with a key that is only used by hand today. So the window is as
  wide as the distance between the floor and the latest release, which is why
  the floor is raised for a security fix, and why a fix is a release and a
  floor change together.
- **Whoever can change the installers.** The site that serves
  `install.sh` and `install.ps1` is the root of this trust, as it was for the
  pinned key: compromise of it defeats the key as well as the floor.
- **A client that already has an installer.** The floor is in the installer
  it ran. An old copy keeps its old floor until it is replaced; the one-line
  `curl | sh` always fetches the current one.

**Follow-up, not done here:** `latest.txt` as a signed manifest with an expiry
(`latest.txt.sshsig` and `.minisig`, re-signed by a scheduled job), which
closes the freeze and shrinks the rollback window to the expiry. It needs a
decision about who runs the periodic signing and with what credential. The
owner must explicitly accept the current freeze limitation for v0.1.0 or make
this mechanism a release gate. Recommendation: accept and document it for the
preview while keeping bucket-write access tightly restricted, then add the
expiring manifest before a stable release.

### `install.sh` (POSIX `sh`)

Everything is in functions and the last line calls `main`, so a download cut
off part way through runs nothing. `--help` prints from a here-document, so
`curl ... | sh -s -- --help` works (the old one read its own file with
`sed $0`, which is `sh` itself when piped).

The `ssh-keygen` verifier builds an allowed-signers file from the pinned
minisign key file (its 32 raw key bytes in the SSH wire form, so there is one
key and no second copy of it to drift), then runs `ssh-keygen -Y verify -n
hollowscript-release-vX.Y.Z` over `SHA256SUMS`. It is chosen only if
`ssh-keygen -Y` lists the `verify` form, which OpenSSH before 8.0 does not.

The openssl verifier decodes minisign's `Ed` container with `openssl base64`
and `dd`, checks that the signature's key id is the key's, verifies the
signature over `SHA256SUMS`, and then verifies the global signature over
`signature || trusted comment`. The old openssl path skipped the global
signature, so a trusted comment edited after signing was accepted there (and
only there); a tampered comment is now refused on every path. Whether this
`openssl` can verify Ed25519 at all is proved on RFC 8032's own test vector
(and a deliberately wrong message must fail), not guessed from
`openssl version`: LibreSSL and OpenSSL before 3.0 fail it, and are skipped
for the next verifier. The trusted comment check is an exact match (the
old one was a substring match), version comparison no longer passes
`printf %06d` a number with a leading zero (read as octal, and an error for
`08` and `09`), and a key file path with spaces works.

**Layout and PATH.** Binaries go in `~/.hollowscript/bin`
(`HOLLOWSCRIPT_HOME` overrides `~/.hollowscript`), with `~/.hollowscript/env`
(and `env.fish`) adding that directory to `PATH` once. Like rustup, the
installer adds one line, `. "$HOME/.hollowscript/env"`, to `~/.profile`, to
`~/.bashrc` and `~/.bash_profile` where they exist, and to `.zshenv` where
zsh is in use, and a `conf.d/hollowscript.fish` for fish; running it again
adds nothing twice. `--no-modify-path` (or `HOLLOWSCRIPT_NO_MODIFY_PATH=1`)
leaves every profile alone and prints the line to add. `--uninstall` removes
exactly those lines, the fish file, the binaries and `~/.hollowscript`, and
leaves the directory in place only if it holds files the installer did not
put there. On Apple Silicon under Rosetta it installs the native arm64 build;
`HOLLOWSCRIPT_TARGET` picks another supported target. Work files are removed
on every exit, including `INT`, `TERM`, `HUP` and `PIPE` (a `| head` on the
output), and a failure after staging never leaves `.new` or `.previous`
files behind.

### `install.ps1` (PowerShell)

Everything runs inside one function; every exit is `return`, never `exit`,
so `irm ... | iex` does not close the caller's session, and
`$ErrorActionPreference` and `Set-StrictMode` stay inside that function.
Constrained Language Mode is refused before anything else. TLS is set to 1.2
(and 1.3 where this .NET knows it) and nothing older, by assignment: the
previous version added 1.2 to whatever was there, which on Windows PowerShell
5.1 can still include SSL 3 and TLS 1.0 and 1.1. Redirects are followed by
the script itself, one hop at a time (`HttpWebRequest` with automatic
redirection off, at most five), and every hop must be `https://` (or loopback
`http://` only when the base URL is): Windows PowerShell 5.1's
`Invoke-WebRequest` follows a redirect from https to plain http without
saying so. The base URL is parsed with `System.Uri`, with the same refusals as
`install.sh`. Verification is
minisign when it is installed, otherwise `ssh-keygen` (the OpenSSH client
Windows 10 and 11 include, as an optional feature on some builds; the message
is passed to it on standard input from the file itself, so nothing re-encodes
it), otherwise the refusal above: Windows PowerShell 5.1 has no Ed25519 of its
own.

**PATH.** The previous version printed a command that set the user `PATH` to
`$env:PATH;<dir>`, which is the machine and user `PATH` joined: running it
would have copied the whole machine `PATH` into the user one. The user
`PATH` is now read from the registry with its variables unexpanded, the
install directory added once, and written back with its own value kind
(so `%USERPROFILE%`-style entries survive), followed by the environment
change broadcast new terminals need. `-NoModifyPath` (or
`HOLLOWSCRIPT_NO_MODIFY_PATH=1`) leaves it alone; `-Uninstall` removes the
entry, the binaries and the install directory
(`%LOCALAPPDATA%\hollowscript\bin`, or `HOLLOWSCRIPT_INSTALL_DIR`). On
Windows on ARM it installs the x86_64 build, which Windows runs under its x64
emulation.

## 6. Withdrawal: `scripts/release-withdraw.sh`

A release withdrawn because it is dangerous should also end up below a
raised floor (section 5): withdrawal changes what the bucket serves, the
floor changes what an installer accepts from it, and a security withdrawal
does both.


```
scripts/release-withdraw.sh vX.Y.Z --bucket-dir DIR [--reinstate vA.B.C]
```

Acts on a local directory standing in for the bucket. It moves
`latest.txt` first (to the reinstated version, or removes it if it named the
withdrawn one), then moves the version's directory aside to `.withdrawn/`
(never deletes), verifies both, and prints the equivalent `gcloud storage`
commands for the real bucket, with the headers `release-upload.sh` sets, for
a person to run. The mirror's history is never rewritten: withdrawal is
about what a new download gets.

**There is no CDN to purge.** What a client still sees after a withdrawal
is decided by the objects' own `Cache-Control`: a moved or removed
`latest.txt` reaches new default installs within a minute (`max-age=60`),
and a versioned object is `max-age=86400`, so a copy already held by a
client, a proxy or Google's own cache of public Cloud Storage objects can
outlive its deletion by at most a day: within 24 hours of the objects being
removed, nothing should serve the withdrawn version. A security withdrawal
should say so in its advisory. The script prints this too.

## 7. Verifying a release by hand

1. Download the archive for your platform, `SHA256SUMS` and
   `SHA256SUMS.minisig` (or `SHA256SUMS.sshsig`) from
   `https://get.hollowscript.org/vX.Y.Z/`.
2. `minisign -Vm SHA256SUMS -P RWTEd/MMbolYkjq8slz9NEko74kUDEwMdcjcsNLSY68z9K1MZpq0y4mK`,
   and check the trusted comment it prints names this version. Without
   minisign, with OpenSSH: write one line, `hollowscript-release ssh-ed25519
   KEY`, to a file, where `KEY` is the base64 of the key in SSH wire form
   (`AAAAC3NzaC1lZDI1NTE5AAAAI` followed by the base64 of the key's 32 raw
   bytes, which `scripts/install.sh` derives from the pinned key), and run
   `ssh-keygen -Y verify -f THAT_FILE -I hollowscript-release -n
   hollowscript-release-vX.Y.Z -s SHA256SUMS.sshsig < SHA256SUMS`.
3. `shasum -a 256 -c SHA256SUMS --ignore-missing` (or `sha256sum -c`).
4. Extract, and run `hollowscript --version`.

## 8. Hosting: `get.hollowscript.org`

`get.hollowscript.org` is intended to be a Firebase Hosting site. Terraform
defines the Hosting site, custom-domain binding and public read of individual
objects from the private release bucket. The actual Terraform apply/state was
not verified in the 2026-10-07 audit, and the live hostname returned HTTP 404.
Until it is provisioned and deployed, the routes below describe the intended
service contract, not a live service. Project-specific deployment settings
and operator tooling are kept outside the public mirror.

| Request | Response |
|---|---|
| `/install.sh`, `/install.ps1` | 200, served by the site itself, `text/plain; charset=utf-8`, `Cache-Control: public, max-age=300` |
| `/latest.txt` | 302 to the same object in the release bucket on `https://storage.googleapis.com/` |
| `/vX.Y.Z/<file>`, `/vX.Y.Z-suffix/<file>` | 302 to the same path in the release bucket |
| `/` | 302 to `https://hollowscript.org/get-started/` |
| anything else | 404 |

The redirects are 302, not 301, so their target can move later without
clients holding on to the old one. The installers follow them across hosts:
`install.sh`'s `curl --proto '=https'` refuses any hop that is not https,
and `install.ps1`'s `Invoke-WebRequest` follows up to five. The release
archives, `SHA256SUMS`, its signature and `latest.txt` are never deployed
to the site; they live only in the bucket, with the headers
`scripts/release-upload.sh` sets (section 3).

The site must be deployed from a published tag only: deployment takes its
files from the tag, not the working tree, and checks that the tag is on origin
and unmoved, the installers pin the tag's release key, and `install.sh`
parses. The local Firebase emulator checks the route contract and installs a
throwaway release without credentials. After this initial deployment,
publishing a release does not need another Hosting deploy unless the routes
or site configuration change; the versioned redirect pattern covers each
release.

The release configuration is designed to allow public reads of individual
objects without allowing a bucket listing. The last recorded check found the
bucket private; the 2026-10-07 audit did not verify the live IAM policy or
apply state, so do not assume the proposed public grant is active. There is no
CDN or load balancer in front of the bucket: the objects' own headers are the
caching and content type policy. Immutability is
`--no-clobber` on every versioned object plus the upload script's refusal
to overwrite differing bytes; `latest.txt` is the one object that is ever
rewritten. `minisign.pub` is deliberately not uploaded: both installers pin
the key in their own source, so whoever can write to the bucket can never
choose the key a signature is checked against. The bucket is in
us-central1; its name is supplied to the release operator outside the public
source tree.

## 9. Rehearsal, 2026-09-26

A full local rehearsal of v0.1.0, with nothing touching a real bucket or the
KMS key: `release-build.sh --version v0.1.0 --rehearsal` on an Apple Silicon
Mac (rustc 1.98.1, zig 0.16.0), signed with a throwaway minisign key
(`minisign -S -l -t "hollowscript SHA256SUMS, version v0.1.0"`), uploaded
with `release-upload.sh` to a local directory, served with
`python3 -m http.server` on `127.0.0.1`, and installed with
`curl ... | dash` using `HOLLOWSCRIPT_BASE_URL` and
`HOLLOWSCRIPT_PUBLIC_KEY_FILE` (the throwaway key; the pinned key cannot
verify it, which also exercises the second-key path). Each test used its own
empty `HOME`.

This rehearsal was made before the installers failed closed: the rows below
that say an install goes ahead "on the SHA-256 check", with a notice, describe
the policy of the day. Today the same machines are refused unless they have a
verifier (`ssh-keygen` counts, and is on a stock macOS) or the install sets
`HOLLOWSCRIPT_ALLOW_UNSIGNED=1`; section 5 is current.

The throwaway key must sign in the legacy `Ed` format (`-l`), as the KMS
key does: `install.sh`'s openssl verifier accepts only that format, so a
default (prehashed `ED`) minisign signature installs where minisign is
present but is refused, as not verifying, on the openssl path.

**Build.** All six targets built and were packaged (eleven archives, the
SBOM and `SHA256SUMS`). The built-in check found all twenty shipped products
byte-identical across two checkout paths, target directories and
`CARGO_HOME`s. Independently, a `--rehearsal` build in the working
checkout and a build without `--rehearsal` from a fresh clone of the same
commit, tagged `v0.1.0` there (never pushed), produced identical
`SHA256SUMS`: every archive and the SBOM, byte for byte. That comparison is
what found the Windows timestamp and the dylib install name (points 5 and 6
above), which the first, host-only check could not see. No packaged file contained a local path (the check found and
failed on the `CARGO_HOME` leak and on the dylib's absolute install name
before both were fixed). `hollowscript --version` and `hs_version()` reported
0.1.0 natively and, for `x86_64-apple-darwin`, under Rosetta. Refused as they
should be: a version that disagrees with `Cargo.toml`, a dirty tree or an
untagged HEAD without `--rehearsal`, a non-empty output directory, an
unknown target, a missing `zig`, and a target whose build fails (a
`cargo-zigbuild` that exits 1): the run stops with an error and packages
nothing.

**Installer, macOS** (aarch64, and x86_64 under Rosetta):

| Case | Result |
|---|---|
| Stock PATH (`/usr/bin:/bin`: no minisign, LibreSSL openssl) | installs on the SHA-256 check, with the notice |
| Same, `HOLLOWSCRIPT_REQUIRE_SIGNATURE=1` | refused |
| minisign on PATH | signature and signed version verified, installs |
| OpenSSL 3 on PATH, no minisign | signature, global signature and signed version verified, installs |
| Tampered `SHA256SUMS`, with minisign / OpenSSL | refused (signature) |
| Tampered `SHA256SUMS`, stock PATH | refused (checksum of this archive changed) |
| Tampered signature, with minisign / OpenSSL | refused |
| Tampered trusted comment, not re-signed, with minisign / OpenSSL | refused (global signature) |
| Validly signed for another version | refused, both verifiers |
| Tampered archive, every path | refused (checksum) |
| Tampered signature or comment, stock PATH | installs on the SHA-256 check, with the notice: the documented limit of that policy |
| `latest.txt` moved back to an older signed release | refused; `HOLLOWSCRIPT_ALLOW_DOWNGRADE=1` passes the guard |
| Version pinned by `HOLLOWSCRIPT_VERSION` and by `--version` while `latest.txt` names an older one | installs the pinned version |
| An archive whose binary reports another version | refused before anything is replaced |
| `v1x.2.3`, `v0.1.0;id` | refused before any download |
| `http://` to a non-loopback host | refused |
| `--no-modify-path`, `HOLLOWSCRIPT_NO_MODIFY_PATH=1` | installs, no profile touched, prints the line to add |
| `.bashrc` (no trailing newline), `.bash_profile`, zsh, a fish config directory | one line each; a second run adds nothing |
| `HOLLOWSCRIPT_HOME` and the extra key file both in paths with spaces | installs; the profile line is quoted and works |
| `--uninstall` | removes every added line, the fish file, the binaries and the directory |
| `--help` through `curl ... \| dash -s -- --help` | prints the usage |
| `TERM` during a stalled download; `SIGPIPE` from `\| head -1` | work directory removed, nothing installed |
| Failure after staging (unreadable old binary) | staged `.new` and backup files removed, old install intact |
| Under Rosetta | installs the arm64 build; `HOLLOWSCRIPT_TARGET=x86_64-apple-darwin` installs the Intel one, which runs |
| `hollowscript lsp` from the installed location, through the profile's PATH | the language server answers `initialize` |

**Installer, Linux containers** (Docker Desktop on the same Mac; amd64 under
emulation), each serving the release on loopback inside the container with a
static busybox `httpd`:

| Image | Fetch and shell | Verifier | Result |
|---|---|---|---|
| Alpine 3, aarch64 | busybox `wget`, busybox `sh` | none | installs with the notice; `hollowscript run`, `hollowscript lsp` and `--uninstall` work |
| Alpine 3, x86_64 | busybox `wget`, busybox `sh` | none | the same |
| Alpine 3, aarch64, `apk add minisign curl` | curl, busybox `sh` | minisign | verified, installs, runs |
| Ubuntu 24.04, aarch64 | curl, dash | OpenSSL 3.0.13 | verified with openssl, installs, runs |
| Ubuntu 24.04, x86_64 | curl, dash | OpenSSL 3.0.13 | the same |

The static musl binaries ran unchanged on both musl (Alpine) and glibc
(Ubuntu).

**`install.ps1`**, under PowerShell 7.4 in a Linux container (the Windows
binary cannot run there, so the full install used a test release whose
`.exe` files are shell scripts): parses cleanly; with minisign, verifies and
installs both binaries, and a second run is clean; refuses a tampered
trusted comment, a release validly signed for another version, and a
tampered `SHA256SUMS`; without minisign, installs on the SHA-256 check with
the notice, or refuses with `HOLLOWSCRIPT_REQUIRE_SIGNATURE=1`; refuses a
plain `http://` non-loopback base URL and a malformed version; every failure
leaves the calling session alive with its `$ErrorActionPreference`
untouched. The registry `PATH` code cannot run off Windows and is untested.

**Mirror** (`--dry-run`, into scratch directories): clean hygiene and
gitleaks scans with `HOLLOWSCRIPT_MIRROR_REPO` unset; the release tag
created on the mirror commit; a mirror that already had `v0.1.0` on another
commit refused, with the mirror left as it was; the same release exported
again, a no-op.

**Upload** (to a local directory): all fourteen objects uploaded and read
back, headers recorded, `latest.txt` moved; a second run skipped all of them
as identical; with two objects deleted, resumed and uploaded only those two;
wrong headers on an identical object corrected; an existing object with
different bytes refused; an unreadable `latest.txt` stopped the run; a
tampered trusted comment, a release signed for another version and a
`SHA256SUMS` naming `install.sh` all refused before any upload; `latest.txt`
not moved backwards without `--allow-latest-rollback`; the rehearsal build
refused for a `gs://` bucket; `--dry-run` for `gs://` printed the `gcloud`
commands and ran nothing. **Withdrawal**: withdraw with reinstatement, and
withdraw of the latest with no reinstatement, both verified.

### Repeated from the integration branch

The same rehearsal was repeated on 2026-09-26 from `integration/v0.1.0`,
the six v0.1.0 pull requests merged together: a `--rehearsal` build of all
six targets (all twenty shipped products byte-identical across the two
builds, no local path in any archive, `hollowscript --version` and
`hs_version()` 0.1.0 natively and under Rosetta), signed with a throwaway
key, uploaded to a local directory and served on `127.0.0.1`. `curl | dash`
installed and uninstalled cleanly on macOS with the stock `PATH` and with
minisign, and in Alpine 3 (aarch64 and x86_64, and with minisign) and
Ubuntu 24.04 (aarch64 and x86_64, OpenSSL 3); in each, the README example
ran, a question printed before `stdin.readLine()` appeared before its
input, `| head -1` stopped the run quietly, a fault printed
`fault.hws:4:5: error[HS1008]`, a 300-element list literal ran, and
`hollowscript lsp` from the installed location answered `initialize`.

### Latest working-tree rehearsal, 2026-10-06

The latest rehearsal used a synthetic snapshot containing the in-place list
append memory fix. The release build produced all 20 shipped products byte
identically across separate build paths and `CARGO_HOME` directories. It also
produced the CycloneDX SBOM and signed checksum files. The recorded
`./scripts/check` run passed 318 corpus cases and 99 spec examples. The
installer fixture suite passed 55 cases, and the upload/withdrawal fixture
suite passed 22 cases.

This was not a tagged release candidate: the source snapshot was uncommitted,
there is no `v0.1.0` tag, and no real-bucket upload, public mirror push or
extension publication was performed. Linux integration runs used containers
under Docker Desktop on macOS; PowerShell 7.4 ran in a Linux container with
placeholder executable files. These checks do not establish native Linux or
Windows support.

## Rehearsals and the first public release

A rehearsal validates mechanics; it is not a releasable version. The first
public release must be built from a clean, frozen, tagged commit, signed with
the Cloud KMS key, and verified by the upload and installer checks. The
release operator must provision and deploy `get.hollowscript.org`, run the
real-bucket upload/withdrawal procedure, publish the curated source mirror,
and publish the extension to both planned stores. The supported target list
must reflect systems tested natively; container/emulation checks alone do not
establish native support.

The first public site update replaces the pre-release notice with the tested
install commands, target support matrix, signature verification, uninstall
instructions, changelog and reference links. Package channels beyond the
extension stores require an explicit release-scope decision before any
registry publication.
