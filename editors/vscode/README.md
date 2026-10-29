# HollowScript for Visual Studio Code

Language support for HollowScript™, a small, statically typed, embeddable
scripting language with a capability-based security model.

> **Preview.** This extension and HollowScript itself (v0.1.0) are a
> preview. Things will change between versions, and some features are still
> missing. Please report problems at
> https://github.com/projecthollow/hollowscript/issues.

## Features

Syntax highlighting works as soon as the extension is installed. Everything
else comes from the HollowScript language server, which is part of the
`hollowscript` command-line tool (see [Requirements](#requirements)):

- **Diagnostics** as you type, from the real checker, with the same `HS`
  codes the command-line tool reports
- **Formatting** (Format Document) with the canonical formatter
- **Hover** showing the type of a name
- **Go to Definition** and **Find All References**, across imported files
- **Rename Symbol**
- **Signature help** while typing a call's arguments
- **Completion** of the names in scope
- **Outline**, breadcrumbs and **folding**
- **Semantic highlighting**, colouring each name by what it really refers to
- **Quick fixes** for some diagnostics, such as adding a missing `uses` line

It also has snippets for common declarations and statements, bracket
matching, and comment toggling.

Files ending in `.hws` are HollowScript source. Compiled bytecode (`.hsb`,
written by `hollowscript build`) is binary and is not opened as text.

## Requirements

Install the `hollowscript` command-line tool first. On macOS and Linux:

```sh
curl -fsSL https://get.hollowscript.org/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://get.hollowscript.org/install.ps1 | iex
```

The extension looks for `hollowscript` in this order:

1. the `hollowscript.executablePath` setting, if you set one;
2. your `PATH`;
3. the installer's directory: `~/.hollowscript/bin` on macOS and Linux
   (`$HOLLOWSCRIPT_HOME/bin` if you set `HOLLOWSCRIPT_HOME` when
   installing), `%LOCALAPPDATA%\hollowscript\bin` on Windows
   (`HOLLOWSCRIPT_INSTALL_DIR` if you set it when installing).

If it cannot find the tool, it tells you, offers to copy the install
command, and keeps syntax highlighting working. After installing, run
**HollowScript: Restart Language Server** from the Command Palette, or
reload the window.

The extension starts the server by running `hollowscript lsp`. That
command runs the `hollowscript-lsp` program installed next to
`hollowscript`; set the `HOLLOWSCRIPT_LSP_PATH` environment variable to use
one somewhere else.

## Settings

| Setting | Default | Description |
|---|---|---|
| `hollowscript.executablePath` | empty | Full path to the `hollowscript` command. Empty means search as described above. This can only be set in your user settings, never by a workspace, so opening a folder can never change which program the extension runs. |
| `hollowscript.trace.server` | `off` | `messages` or `verbose` logs the traffic between VS Code and the language server, for troubleshooting. |

## Commands

| Command | What it does |
|---|---|
| HollowScript: Restart Language Server | Stops the server, finds `hollowscript` again and starts it. |
| HollowScript: Show Language Server Output | Opens the server's log. |

## Troubleshooting

- **No diagnostics, hover or formatting.** Open the log with
  **HollowScript: Show Language Server Output**. The first lines say which
  `hollowscript` was found, its version, and every place that was searched
  if none was.
- **"The language server stopped while starting."** Usually the
  `hollowscript-lsp` program is missing next to `hollowscript`. Reinstall
  with the command above.
- **Reporting a bug.** Set `hollowscript.trace.server` to `verbose`,
  reproduce the problem, and include the **HollowScript Language Server
  Trace** output with your report.

## Workspace trust

In an untrusted (Restricted Mode) window the extension still runs: the
language server only reads and checks `.hws` files and never runs them.
Virtual workspaces (such as a remote repository opened without a clone)
are not supported, because the server resolves imports from the local file
system.

## Licence

Licensed under either of the Apache License, Version 2.0 or the MIT
license, at your option. The notices of the open source packages bundled
into the extension are in `THIRD-PARTY-NOTICES.md`, included in the
extension package.

HollowScript™ is a trade mark of Soulstack; see the project's trade mark
policy at https://github.com/projecthollow/hollowscript.
