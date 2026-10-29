# Publishing the VS Code extension

How to build, test and publish `hollowscript.hollowscript` to the Visual
Studio Marketplace and to Open VSX. Nothing here is automated: every step
that creates an account, a token or a published version is done by the
project owner, by hand.

This file is not shipped in the `.vsix` (see `.vscodeignore`).

## Before the first release

These must be true before 0.1.0 is published:

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
   128 pixels, the Marketplace's minimum. If the logo changes, copy both
   from `docs/brand/` again (`png/icon-tile-128.png` for the PNG). The
   gallery banner is `#1c1c1c` with the dark theme, to match the tile.
4. **`https://hollowscript.org` works**, since the extension's "Open
   Install Guide" button opens it.

## Build and check

From `editors/vscode`, with Node 22 or later:

```sh
npm ci                  # exact versions from package-lock.json
npm audit               # expect: found 0 vulnerabilities
npm test                # type check and unit tests
npm run package         # production build, then hollowscript-<version>.vsix
npm run check:vsix      # contents, Open VSX manifest and licence, size
```

`npm ci` may warn that install scripts were skipped (npm 11 blocks them by
default). None are needed: esbuild's binary comes from its platform
package, and `keytar` (used by `vsce` only to remember a token) is not
used by these steps.

`npm run package` runs `vsce package --no-dependencies`: esbuild has
already bundled the one runtime dependency into `dist/extension.js`, so
`node_modules` is not shipped. `npx vsce ls --no-dependencies` shows the
file list; `.vscodeignore` is an allow-list, and `check:vsix` fails on any
file outside it, including a source map.

When a dependency changes, run `npm run notices` and commit the updated
`THIRD-PARTY-NOTICES.md` (a unit test fails until you do).

## Test

1. **Language server, end to end, no VS Code needed.** Build the CLI and
   server from the repository root, then run the same command the
   extension runs and check each feature:

   ```sh
   cargo build --release -p hollowscript-cli -p hollowscript-lsp
   cd editors/vscode
   npm run smoke:lsp
   ```

2. **In VS Code.** `npm run test:integration` opens `test/fixtures` in a
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
     Outline view: each works.
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
   - Repeat the first three on Windows, with the PowerShell installer.

4. `npm run clean` removes `dist/`, `.vscode-test/` and any `.vsix`. Do
   this before `scripts/check` at the repository root: its hygiene scan
   walks the working tree, and the bundled `dist/extension.js` contains
   third-party text it flags.

## Visual Studio Marketplace

### One-time setup: the `hollowscript` publisher

1. Sign in at https://marketplace.visualstudio.com/manage with the
   Microsoft account, or Microsoft Entra ID work account, that should own
   the publisher. Prefer an account the organisation controls over a
   personal one: the publisher cannot be moved to another owner later
   without adding that owner as a member first.
2. Choose **Create publisher** and enter:
   - **ID**: `hollowscript` (must match `publisher` in `package.json`; it
     cannot be changed later)
   - **Name**: `HollowScript`
   - optionally the website `https://hollowscript.org`, the source
     repository, and a support contact.
3. Under the publisher's **Members**, add at least one other owner so
   access is never held by one person.
4. Optional, later: **Verified domain**. In the publisher's details, add
   `hollowscript.org` and create the DNS TXT record the page shows.
   Microsoft reviews the request, and applies its own eligibility rules
   (such as how long the publisher has been publishing) before the
   verified badge appears.

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
  the `hollowscript` publisher's **Members** with the Contributor role, as
  Microsoft's publishing guide describes
  (https://code.visualstudio.com/api/working-with-extensions/publishing-extension).
  In the workflow, sign in with `azure/login` using OIDC (no client
  secret), then run `vsce publish --azure-credential`. Check access first
  with `npx vsce verify-pat --azure-credential hollowscript`.

**Alternative: an Azure DevOps personal access token.**

1. Sign in at https://dev.azure.com (create an organisation if the account
   has none; it is only used to issue the token).
2. **User settings > Personal access tokens > New token**:
   - Organization: **All accessible organizations** (a Marketplace token
     must be global)
   - Scopes: **Custom defined > Marketplace > Manage**, nothing else
   - Expiration: as short as practical, such as 7 days for one release.
3. Keep it out of shell history and out of the repository: put it in
   `VSCE_PAT` for the one command, or run `npx vsce login hollowscript`,
   which stores it in the operating system's credential store.
4. Check it: `npx vsce verify-pat hollowscript`. Revoke it after the
   release.

Microsoft has announced that global Azure DevOps personal access tokens
are being retired in favour of Entra ID, so plan on the Entra route for
anything beyond the first release.

### Publish

Publish the exact `.vsix` that was checked, rather than letting `vsce`
build a new one:

```sh
cd editors/vscode
npx vsce publish --packagePath hollowscript-0.1.0.vsix --azure-credential
# or, with a token in VSCE_PAT:
npx vsce publish --packagePath hollowscript-0.1.0.vsix
```

`"preview": true` in `package.json` shows the Preview badge. Do not also
pass `--pre-release`: that is a separate channel for users who opt in to
pre-release builds of an extension that also has regular ones.

The listing is usually live within minutes at
https://marketplace.visualstudio.com/items?itemName=hollowscript.hollowscript.

## Open VSX

Open VSX (https://open-vsx.org) serves VSCodium, Gitpod, Eclipse Theia
and other editors that cannot use the Microsoft Marketplace.

### One-time setup: the `hollowscript` namespace

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
   npx ovsx create-namespace hollowscript -p "$OVSX_PAT"
   ```

5. Claim ownership so the namespace shows as verified and nobody else can
   publish into it: open an issue at
   https://github.com/EclipseFdn/open-vsx.org/issues using the **Claim
   namespace ownership** template, naming the `hollowscript` namespace and
   showing that you control the project (for example, from the project's
   GitHub organisation or its website). Until that is granted, the
   extension is listed with an "unverified" warning.
6. Optional, later: trusted publishing. `ovsx publish` can exchange a CI
   system's OIDC token for a short-lived publishing token
   (`--trusted-publishing`), once the namespace is configured to trust this
   repository's release workflow on open-vsx.org. That removes the stored
   token entirely.

### Publish

```sh
cd editors/vscode
npx ovsx verify-pat hollowscript -p "$OVSX_PAT"
npx ovsx publish hollowscript-0.1.0.vsix -p "$OVSX_PAT"
```

Or set `OVSX_PAT` in the environment and leave out `-p`. The listing is at
https://open-vsx.org/extension/hollowscript/hollowscript.

## After publishing

1. Install from each store into a clean VS Code (a new profile, or
   `--user-data-dir` and `--extensions-dir` pointing at empty directories)
   and repeat the first manual checks above.
2. Tag the commit the `.vsix` was built from, for example
   `vscode-v0.1.0`, so the published build can always be rebuilt.
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
