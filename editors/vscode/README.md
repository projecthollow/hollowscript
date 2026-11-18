# HollowScript for your editor

![HollowScript: a small, statically typed language with explicit capabilities](https://hollowscript.org/og.png?v=b9ad2b52dacb)

Write HollowScript with syntax highlighting, completion, diagnostics and
refactoring in VS Code and compatible editors. Published by **Soulstack** as `soulstack.hollowscript`.

**Preview:** this extension supports HollowScript v0.1.1 and VS Code 1.91 or
later on macOS, Windows and Linux. HollowScript and its tooling are still
previews. No account is required. Install from Visual Studio Marketplace or
use a VSIX package in VSCodium and Theia IDE. Open VSX coming soon.

## Get started

1. Install the HollowScript v0.1.1 command-line tool using the command below.
2. Open a `.hws` file in your editor.
3. If the file was already open, run **HollowScript: Restart Language Server**
   from the Command Palette.

On macOS or Linux:

```sh
curl -fsSL https://get.hollowscript.org/install.sh | sh -s -- --version v0.1.1
```

On Windows, in PowerShell:

```powershell
& ([scriptblock]::Create((irm https://get.hollowscript.org/install.ps1))) -Version v0.1.1
```

The installer includes both the CLI and its language server. See the
[HollowScript installation guide](https://hollowscript.org) for other options.

## What you can do

- **See errors as you type**, with the same diagnostic codes as the CLI.
- **Complete your code** with names, methods, record fields and enum variants,
  including imported types, partial names and chained calls.
- **Explore code** with hover information, Go to Definition, the Outline view,
  folding and semantic highlighting.
- **Refactor names** with Find All References and Rename Symbol.
- **Format files** with Format Document and apply quick fixes for supported
  diagnostics, such as a missing `uses` declaration.
- **Start common declarations** with snippets, bracket matching and comment
  toggling.

References and rename search the declaration and open importing files. Open
other affected importing files first; the server does not index the whole
project. Completion covers String, list, Map, Set, Result, records, enums and
`json`/`time`. Optional values need to be narrowed before their members appear.

Syntax highlighting and member completion work without the CLI. The remaining
language features need the separately installed CLI. Compiled `.hsb` bytecode
files are binary and are not opened as source.

## Settings and commands

The extension finds `hollowscript` on your `PATH`, then in its default install
folder. It also honours the installer's `HOLLOWSCRIPT_HOME` on macOS/Linux and
`HOLLOWSCRIPT_INSTALL_DIR` on Windows.

| Setting | Default | Purpose |
|---|---|---|
| `hollowscript.executablePath` | Empty | Choose an installed CLI executable. Set this in your user settings; a workspace cannot choose which program runs. |
| `hollowscript.trace.server` | `off` | Use `messages` or `verbose` to record language-server traffic for troubleshooting. |

Run **HollowScript: Restart Language Server** after installing or changing the
CLI, or **HollowScript: Show Language Server Output** to inspect a connection
problem.

## Privacy and workspace trust

Code analysis runs locally. This extension has no account sign-in, telemetry
or hosted code-analysis service, and it does not run HollowScript programs.
It reads imported files to check the current source. Analysis limits keep
large or incomplete files from blocking the editor.

Language features also work in VS Code's Restricted Mode. Virtual workspaces
without local files are not supported. In a remote development window, the
CLI must be installed where the workspace files are available.

## Help and feedback

- **No diagnostics or formatting:** open the language-server output and check
  the selected CLI path. Run **HollowScript: Restart Language Server** after
  correcting it.
- **Server stops while starting:** reinstall the CLI and its matching language
  server with the command above. A custom `HOLLOWSCRIPT_LSP_PATH` must point to
  a working language-server executable.
- **Report a problem:** enable tracing in Settings, reproduce the problem, then
  select **HollowScript Language Server Trace** in the Output panel. Logs can
  contain source text and file paths; review them before sharing.

[HollowScript documentation](https://hollowscript.org) ·
[Report an issue](https://github.com/projecthollow/hollowscript/issues)

## Licence

The extension code is available under Apache-2.0 or MIT, at your option.
Bundled dependency notices and specification attribution are included in the
extension package. HollowScript™ and its artwork are Soulstack trade marks;
see the trade mark policy, `TRADEMARKS.md`, in the
[HollowScript repository](https://github.com/projecthollow/hollowscript).
