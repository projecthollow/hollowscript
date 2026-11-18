<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/lockup-dark.svg">
    <img src="docs/brand/lockup-light.svg" alt="HollowScript" height="48">
  </picture>
</h1>

HollowScript™ is a small, statically typed programming language for embedding in other software, with a capability-based security model. This repository contains its compiler, bytecode virtual machine, embedding API, language server and WebAssembly build.

**Preview release (v0.1.2).** The command line, Rust API and bytecode format may change in any release.

## Install

Supported platforms: macOS on Apple Silicon, Linux on x86_64 and Windows on x86_64.

macOS and Linux:

```sh
curl -fsSL https://get.hollowscript.org/install.sh | sh -s -- --version v0.1.2
```

Windows, in PowerShell:

```powershell
$env:HOLLOWSCRIPT_VERSION = 'v0.1.2'; irm https://get.hollowscript.org/install.ps1 | iex
```

The installer checks the release signature and checksum before installing. Restart your shell, then run `hollowscript version`.

The Windows executables are not code signed, so Smart App Control, where it is turned on, may block them. The [install guide](https://hollowscript.org/get-started/) covers verification, other install locations and uninstalling. On other platforms, [build from source](#build-from-source).

## Example

```hollow
uses print

record Task = { title: String, owner: String? }

fn ownerOf(task: Task): String {
    match task.owner {
        none { return "unassigned" }
        else { return task.owner }
    }
}

constant tasks: [Task] = [
    { title: "Fix build", owner: "sam" },
    { title: "Write docs", owner: none }
]

for task in tasks {
    effect print("${ownerOf(task)}: ${task.title}")
}

effect print(tasks.map(fn(t: Task): String {
    return t.title
}).sort().join(", "))

return tasks.length()
```

`hollowscript run tasks.hws` prints:

```text
sam: Fix build
unassigned: Write docs
Fix build, Write docs
```

## Usage

| Command | Purpose |
| --- | --- |
| `hollowscript check FILE` | Check a program without running it |
| `hollowscript format FILE` | Print the program in standard formatting |
| `hollowscript run FILE` | Run a program or a compiled file |
| `hollowscript build FILE` | Compile to a bytecode file |
| `hollowscript lsp` | Start the language server |

`print`, `clock` and `random` are always available. Access to files, the network, standard input, environment variables and program arguments must be granted with `--allow`, for example `--allow fs:read=./data`. Run `hollowscript help` for all options.

A compiled program can be loaded from Rust, from C, or from JavaScript through the WebAssembly build.

## Editor support

The language server provides diagnostics, formatting, hover, go to definition, find references, rename and an outline. A VS Code extension is in [`editors/vscode/`](editors/vscode/README.md); it is not yet published.

## Documentation

- [Language specification](docs/spec/README.md), version 1.2.0
- [Implementation design](docs/design/README.md)
- [Conformance tests](corpus/)
- [Changelog](CHANGELOG.md)

## Build from source

Requires [rustup](https://rustup.rs). The pinned toolchain, Rust 1.98.1, installs on first use.

```sh
cargo build --workspace
scripts/check
```

`scripts/check` runs every check, including the tests.

## Contributing

Development is closed and Soulstack decides the roadmap. Bug reports and specification questions are welcome as issues; pull requests are not accepted. See [CONTRIBUTING.md](CONTRIBUTING.md) and [GOVERNANCE.md](GOVERNANCE.md). Report security issues as described in [SECURITY.md](SECURITY.md).

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option, except that the documentation under `docs/`, including the language specification, is licensed under the Creative Commons Attribution 4.0 International licence (CC BY 4.0, full text in [`docs/LICENSE-CC-BY-4.0.txt`](docs/LICENSE-CC-BY-4.0.txt)). The exception is [`docs/brand/`](docs/brand/README.md), the HollowScript logo and wordmark files, which are not licensed under CC BY 4.0, Apache-2.0 or MIT; their use is governed by [TRADEMARKS.md](TRADEMARKS.md). See [NOTICE](NOTICE) for the copyright notice. The project uses HollowScript as a trademark; the licences above do not grant rights to it, and a fork must use a different name (see [TRADEMARKS.md](TRADEMARKS.md)). The owner has retained the name after a self-search; this does not claim registration or legal clearance.
