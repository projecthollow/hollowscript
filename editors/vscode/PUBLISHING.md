# Publishing the VS Code extension

How to build, test and publish `soulstack.hollowscript` to the Visual
Studio Marketplace and to Open VSX. Publish only with explicit owner
authorisation. The owner reviews binding agreements and controls account
credentials; publication uses the exact validated package.

This file is not shipped in the `.vsix` (see `.vscodeignore`).

## Release status on 2 November 2026

The owner authorised publication after validation. HollowScript 0.1.1 is
[published on Marketplace as Preview](https://marketplace.visualstudio.com/items?itemName=soulstack.hollowscript),
using the existing `soulstack` publisher. A fresh installation by Marketplace
ID and version passed. The published VSIX SHA-256 is
`e0171a6ac40ec74a22623ae373b9606c58cb40621a4033357841b05acff59007`.

Owner access was verified through Microsoft browser sign-in. No publisher
creation, membership change or broader federation grant was needed. Cached
Azure CLI credentials remain separate from browser authentication.

Open VSX publication remains pending Eclipse account linking, owner acceptance
of the Publisher Agreement and verified Soulstack namespace approval. The
approved [ownership request](https://github.com/EclipseFdn/open-vsx.org/issues/15148)
has been submitted. Internal ticket PRO-315 tracks the remaining work for both
extensions. Reuse the exact reviewed VSIX and verify the public listing and a
fresh installation after publication. Public copy: "Open VSX coming soon".

## Release prerequisites

These checks passed for 0.1.1 and must remain true for future releases:

1. **The release archives contain `hollowscript-lsp`.** The extension runs
   `hollowscript lsp`, which starts the `hollowscript-lsp` binary installed
   next to `hollowscript`. If the installer only installs `hollowscript`,
   every user sees "the language server stopped while starting". Check
   with a fresh install:

   ```sh
   curl -fsSL https://get.hollowscript.org/install.sh | sh
   ls ~/.hollowscript/bin/            # must list hollowscript-lsp too
   node scripts/lsp-smoke.mjs ~/.hollowscript/bin/hollowscript
   ```

2. **The repository URL is public.** `package.json`'s `repository` and
   `bugs`, and the README, point at the public repository. The Marketplace
   and Open VSX link to it from the extension's page.
3. **The icon is final.** `images/icon.svg` is the HollowScript icon
   tile, a copy of `docs/brand/icon-tile.svg` (the mark on a `#1c1c1c`
   rounded square), and `images/icon.png` is that tile rendered at 128 by
   128 pixels, the Marketplace's minimum. If the logo changes, run
   `python3 scripts/render-brand.py` from the repository root. It rebuilds
   both editor files from the canonical vector and palette; verify them
   with `python3 scripts/render-brand.py --check`. The
   gallery banner is `#1c1c1c` with the dark theme, to match the tile.
4. **`https://hollowscript.org` works**, since the extension's "Open
   Install Guide" button opens it.

## Build and check

From `editors/vscode`, with Node 22 or later and the repository's pinned Rust toolchain:

```sh
rustup toolchain install 1.98.1 --profile minimal
rustup target add wasm32-unknown-unknown --toolchain 1.98.1
```

`cargo` must be on PATH. Builds compile the canonical tooling to
`dist/members.wasm` and bundle the worker beside the language client.
Nothing is downloaded or installed by the extension at run time.
Check the generated method signatures with
`python3 scripts/member-catalog.py --check` from the repository root.

Then, from `editors/vscode`:

```sh
npm ci                  # exact versions from package-lock.json
npm audit               # expect: found 0 vulnerabilities
npm test                # type check and unit tests
npm run package         # production build, then hollowscript-<version>.vsix
npm run check:vsix      # contents, Open VSX manifest and licence, size
```

`npm ci --ignore-scripts` is sufficient: no dependency install scripts are
needed by these checks. esbuild's binary comes from its platform package.

`npm run package` runs `vsce package --no-dependencies`: esbuild has
already bundled the runtime dependency into `dist/extension.js`, so
`node_modules` is not shipped. The member worker bundles the project's own
WebAssembly wrapper; the WASM binary is built from the same checkout. `npx vsce ls --no-dependencies` shows the
file list; `.vscodeignore` is an allow-list, and `check:vsix` fails on any
file outside it, including a source map.

When a dependency changes, run `npm run notices` and commit the updated
`THIRD-PARTY-NOTICES.md` (a unit test fails until you do).

## Test

Use `HOLLOWSCRIPT_BIN` to select the released v0.1.1 CLI for acceptance.
For example, after packaging:

```sh
HOLLOWSCRIPT_BIN="$HOME/.hollowscript/bin/hollowscript" VSIX_PATH=hollowscript-0.1.1.vsix node test/integration/run.mjs
```

The profile and extension directory are temporary and separate from your own.
`TEST_KEEP_PROFILE=1` retains the profile and language-server trace for review.

1. **Language server, end to end, no VS Code needed.** Build the CLI and
   server from the repository root, then run the same command the
   extension runs and check the baseline features:

   ```sh
   cargo build --release -p hollowscript-cli -p hollowscript-lsp
   cd editors/vscode
   npm run smoke:lsp
   ```

2. **In VS Code.** Set `HOLLOWSCRIPT_BIN` to an installed, signature-verified
   released v0.1.1 binary, rather than the modified repository build. Set
   `VSIX_PATH` to the production package to test the installed package in an
   isolated profile (`node test/integration/run.mjs`). The runner also accepts
   `TEST_RESTRICTED=1` for Restricted Mode and `TEST_MISSING_SERVER=1` to check
   activation and member completion with an intentionally invalid CLI path.

   `npm run test:integration` opens `test/fixtures` in a
   real VS Code with the extension loaded from source, and checks
   activation, diagnostics, formatting, hover, go to definition, the
   outline and restarting the server. Point it at an installed VS Code
   with `VSCODE_EXECUTABLE` (on macOS, for example,
   `".../Visual Studio Code.app/Contents/MacOS/Code"`); without it,
   `@vscode/test-electron` downloads VS Code 1.91.0 (the oldest supported
   version) into `.vscode-test/`.

3. **By hand, once per release,** install the packaged `.vsix` with
   `code --install-extension hollowscript-<version>.vsix` and check:

   - Open a `.hws` file: it is highlighted, and the status bar says
     HollowScript.
   - Introduce an error (`return missing`): a red squiggle with `HS0205`
     appears. Fix it: it goes.
   - Format Document, hover a name, F12 on a call, F2 to rename, the
     Outline view: each works. Type a dot after a String, list, Map, Set,
     Result, record, enum and `json`: check the suggestions and signatures.
     Check an imported record and enum too.
   - **HollowScript: Show Language Server Output** shows which
     `hollowscript` was found and its version.
   - Set `hollowscript.trace.server` to `verbose`: JSON-RPC traffic appears
     in **HollowScript Language Server Trace**.
   - Temporarily move `hollowscript` off `PATH` and out of
     `~/.hollowscript/bin`, then run **HollowScript: Restart Language
     Server**: an error offers to copy the install command, and
     highlighting still works. Set `hollowscript.executablePath` to a wrong
     path: the error names the setting.
   - Open a `.hsb` file built with `hollowscript build`: VS Code says it is
     binary instead of opening it as text.
   - Open the folder in Restricted Mode (untrusted): the server still runs.
   - Repeat the editor checks on Windows and Linux with the released v0.1.1
     installer for that platform. Record native platform results separately
     from container or emulation results.

4. `npm run clean` removes `dist/`, `.vscode-test/` and any `.vsix`. Do
   this before `scripts/check` at the repository root: its hygiene scan
   walks the working tree, and the bundled `dist/extension.js` contains
   third-party text it flags.

## Visual Studio Marketplace

### Existing publisher: `soulstack`

The owner has selected the existing Soulstack publisher used by Hollow Code.
Sign in at https://marketplace.visualstudio.com/manage/publishers/soulstack
with its owner account. The Members page was checked on 2 November 2026 and
shows Lee Fuller as Owner. Do not create another Marketplace publisher.
`publisher` in `package.json` must remain `soulstack`.

### Authentication: choose one

**Recommended: Microsoft Entra ID, no stored token.**

- By hand: install the Azure CLI, run `az login` as a member of the
  publisher, then publish with `--azure-credential` (below). `vsce` gets a
  short-lived token from the signed-in identity.
- From CI (for later releases): create a user-assigned managed identity,
  or an app registration, in the organisation's Entra ID tenant. Give it a
  federated credential that trusts only this repository's release workflow
  (for GitHub Actions: issuer `https://token.actions.githubusercontent.com`,
  subject limited to the release environment or tag). Add that identity to
  the `soulstack` publisher's **Members** with the Contributor role, as
  Microsoft's publishing guide describes
  (https://code.visualstudio.com/api/working-with-extensions/publishing-extension).
  In the workflow, sign in with `azure/login` using OIDC (no client
  secret), then run `vsce publish --azure-credential`. Check access first
  with `npx vsce verify-pat --azure-credential soulstack`.

**Alternative: an Azure DevOps personal access token.**

1. Sign in at https://dev.azure.com (create an organisation if the account
   has none; it is only used to issue the token).
2. **User settings > Personal access tokens > New token**:
   - Organization: **All accessible organizations** (a Marketplace token
     must be global)
   - Scopes: **Custom defined > Marketplace > Manage**, nothing else
   - Expiration: as short as practical, such as 7 days for one release.
3. Keep it out of shell history and out of the repository: put it in
   `VSCE_PAT` for the one command, or run `npx vsce login soulstack`,
   which stores it in the operating system's credential store.
4. Check it: `npx vsce verify-pat soulstack`. Revoke it after the
   release.

Microsoft has announced that global Azure DevOps personal access tokens
are being retired in favour of Entra ID, so plan on the Entra route for
anything beyond the first release.

### Publish

Publish the exact `.vsix` that was checked, rather than letting `vsce`
build a new one:

```sh
cd editors/vscode
npx vsce publish --packagePath hollowscript-0.1.1.vsix --azure-credential
# or, with a token in VSCE_PAT:
npx vsce publish --packagePath hollowscript-0.1.1.vsix
```

`"preview": true` in `package.json` shows the Preview badge. Do not also
pass `--pre-release`: that is a separate channel for users who opt in to
pre-release builds of an extension that also has regular ones.

The listing is usually live within minutes at
https://marketplace.visualstudio.com/items?itemName=soulstack.hollowscript.

## Open VSX

Open VSX (https://open-vsx.org) serves VSCodium, Gitpod, Eclipse Theia
and other editors that cannot use the Microsoft Marketplace.

### One-time setup: the `soulstack` namespace

1. Create an Eclipse Foundation account at https://accounts.eclipse.org,
   with the same email address as the GitHub account you will use. Put your
   GitHub username in its profile.
2. Sign in at https://open-vsx.org with GitHub, open your profile
   settings, log in to the Eclipse account from there, and sign the Eclipse
   Foundation Open VSX Publisher Agreement.
3. In **Settings > Access Tokens**, generate a token (describe it as
   "hollowscript publish"). It is shown once; keep it in a password
   manager.
4. Create the namespace, which must match `publisher` in `package.json`:

   ```sh
   npx ovsx create-namespace soulstack -p "$OVSX_PAT"
   ```

5. Claim ownership of `soulstack` before the first public release. Creating
   a namespace grants Contributor membership, not verified Owner status.
   Submit the namespace ownership template at
   https://github.com/EclipseFdn/open-vsx.org/issues with proof of control of
   the Marketplace publisher. This is a public action and needs owner approval.
   Since these extension IDs match the Marketplace, an upload can succeed
   while the version is subsequently quarantined pending ownership review.
   Check the registry UI/API after publishing; a successful CLI exit alone
   does not prove public availability. Do not publish just to test access.
6. Optional, later: trusted publishing. `ovsx publish` can exchange a CI
   system's OIDC token for a short-lived publishing token
   (`--trusted-publishing`), after a first version exists, the namespace is verified, the agreement is
   signed and the namespace trusts this repository's exact release workflow.
   This replaces a long-lived pipeline token with a short-lived credential.

### Publish

```sh
cd editors/vscode
npx ovsx verify-pat soulstack -p "$OVSX_PAT"
npx ovsx publish hollowscript-0.1.1.vsix -p "$OVSX_PAT"
```

Or set `OVSX_PAT` in the environment and leave out `-p`. The listing is at
https://open-vsx.org/extension/soulstack/hollowscript.

## After publishing

1. Install from each store into a clean VS Code (a new profile, or
   `--user-data-dir` and `--extensions-dir` pointing at empty directories)
   and repeat the first manual checks above.
2. Tag the commit the `.vsix` was built from, for example
   `vscode-v0.1.1`, so the published build can always be rebuilt.
3. Move the `[Unreleased]` notes in `CHANGELOG.md` under the new version
   before the next release, and bump `version` in `package.json`: neither
   store accepts the same version twice.

## If something goes wrong

- A broken release is fixed by publishing a higher version. Unpublishing
  (`vsce unpublish`, `ovsx unpublish`) removes the extension for everyone
  and is a last resort.
- A leaked token: revoke it at once (Azure DevOps **Personal access
  tokens**, or open-vsx.org **Settings > Access Tokens**), then check the
  publisher's and namespace's version history for anything you did not
  publish.


## Dependency overrides

The exact `minimatch` and `glob` overrides in `package.json` replace versions
with published denial-of-service advisories, including the matcher bundled
by `vscode-languageclient`. Do not use `npm audit fix --force`: it suggests
an incompatible language-client downgrade. Re-run both audit modes, the
minimum-version editor tests and packaging checks after changing overrides.

## Publication hold

This candidate is prepared for review only. No store publish, account setup,
namespace claim or release tag is authorised by the implementation work.
Consult `RELEASE-READINESS.md` for the exact acceptance evidence and remaining
external gates before the owner decides to publish.

## Local analysis limits

The bundled analyser runs in a worker, with at most two concurrent requests
per service, a three-second deadline, 1 MiB per source file, 8 MiB total source,
128 modules and 128 MiB of WebAssembly memory per analysis. Imports must remain
below the entry file's directory. File reads reject symbolic and hard links.
Open documents may override the contents of an existing accepted import.
Cancellation, a missing CLI or an analysis limit must leave the editor usable.

Tracing is controlled by `hollowscript.trace.server`, including at VS Code's
normal log level. `off` suppresses protocol logging even if the editor itself
uses Trace logging. Verbose logs can include source text and file paths.
