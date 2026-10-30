# HollowScript

HollowScript™ is a small, statically typed, embeddable programming language with a capability-based security model, in the class of Lua. This repository holds its native implementation: a compiler and bytecode virtual machine, an embedding API, a language server and a WebAssembly build of the VM.

**Status: preview (v0.1.1).** The language runs end to end, from source to a verified bytecode container, on macOS, Linux and Windows, and in a browser through the WebAssembly build. Nothing is stable yet: the command line, the Rust API and the bytecode format may change in any release. It implements version 1.1.1 of the language specification, which is in `docs/spec/`; the shared conformance corpus is in `corpus/`, and the design of this implementation is recorded in `docs/design/`.

## Install

The v0.1.1 public preview supports **macOS on Apple Silicon**, **Linux on
x86_64** (static musl build) and **Windows on x86_64**. On macOS and Linux,
install the command line tool and language server with:

```sh
curl -fsSL https://get.hollowscript.org/install.sh | sh -s -- --version v0.1.1
```

On Windows, in PowerShell:

```powershell
$env:HOLLOWSCRIPT_VERSION = 'v0.1.1'; irm https://get.hollowscript.org/install.ps1 | iex
```

The installers verify the release signature with minisign or OpenSSH and
check the archive's SHA-256 before installing. Restart your shell, then run
`hollowscript version`. To uninstall, run the same installer with `--uninstall`
(`-Uninstall` on Windows). The Windows executables are not Authenticode
signed, so Smart App Control, where it is turned on, may refuse to run them.
See the [install guide](https://hollowscript.org/get-started/) for verification,
preview limitations and alternative installation directories.

Intel macOS, Linux on ARM64, downloadable WebAssembly builds and registry
packages are outside this preview's supported distribution. The source
contains those build targets, but their presence is not a support claim.
Build from source with the pinned Rust toolchain using [Building](#building).
The release process is in [docs/design/release.md](docs/design/release.md).

## What runs today

A program is lexed, parsed, checked, compiled, verified and interpreted, end to end. This program is in the canonical form `hollowscript format` produces, and `hollowscript run` prints three lines and returns 2:

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

Imports are resolved from the entry module's own directory. The standard library of chapters 13, 14 and 15 is implemented, closures and `Result` included. `print`, `clock` and `random` are granted to every run; `--allow` grants `env` and `args`, and `fs`, `stdin` and `http` with the scoped grants of section 9.5. `hollowscript build` compiles a program to a verified bytecode container, which a host loads from Rust, from C or, through the WebAssembly build, from JavaScript.

The `hollowscript` command has `check`, `format`, `run`, `build`, `lsp`, `version` and `help`; `hollowscript help COMMAND`, `COMMAND --help` and `COMMAND -h` give one command's own usage, and with no arguments the usage is printed to standard error with exit status 2. `run` also takes a container `build` wrote, verified before it runs and refused (HS0404) unless it is run with the grants it was built with. What a program prints reaches standard output as it happens, so a question printed before `stdin.readLine()` is seen before the program waits for an answer; with `--json`, output is collected into the one document written when the run ends instead. A runtime fault is reported on standard error as `FILE:LINE:COLUMN: error[code]: message`, and a reader that closes standard output early (`hollowscript run app.hws | head -1`) stops the run quietly. Exit status is 0 on success, 1 when the program has an error or its run faults, and 2 when the command itself could not be carried out; with `--json`, that last kind of problem is reported as `{"error": "..."}` on standard output. An interactive REPL is planned but not part of this release.

**How much of the language that is** is measured, not asserted. `hollowscript-conformance` compares the parse diagnostics, the check diagnostics, the formatter's output and the run's output, result and fault of every corpus case against its expected file. Every case passes: `crates/hollowscript-conformance/known-failures.txt`, where a case that does not pass would be listed by name, is empty, and `scripts/check` fails both when a case outside that list fails and when a case inside it passes, so the list can only shrink. This implementation's own limits (for example, 65,535 values held at once by one function) are reported as HS0901 and HS1015 and listed in `docs/design/bytecode.md`. `docs/design/release-and-milestones.md` says what remains.

## Editor support

`hollowscript lsp` starts the language server (`hollowscript-lsp`) on
standard input and output. It resolves names declared in imported files
(from an open document first, then disk), and implements diagnostics,
formatting, hover, go to definition, find references, rename, signature
help, an outline, folding, semantic tokens and quick fixes for a few
diagnostics, all against the real checker.

The server accepts message bodies up to 32 MiB and header blocks up to 8 KiB;
larger frames are rejected before their declared body is allocated.

The analysis behind the language server is its own library crate,
`hollowscript-ide`, which reads no file and speaks no protocol: it checks a
document against the modules and capabilities a caller supplies and answers
hover, completion, definition, the outline and the rest. The WebAssembly
`tooling` build links it too, so a browser editor gets the same answers with
no server (`crates/hollowscript-wasm/web/README.md`).

The VS Code extension in `editors/vscode/` (version 0.1.0, preview) is a
thin client over it: it highlights HollowScript, and finds `hollowscript`
through its `hollowscript.executablePath` setting, on `PATH` or in the
installer's directory, offering the install command when it is missing. It
is prepared for the Visual Studio Marketplace and Open VSX but not
published yet; its `README.md` covers installing and using it, and its
`PUBLISHING.md` building, testing and publishing it.

## Building

This is a Cargo workspace, edition 2024. `rust-toolchain.toml` pins the toolchain to Rust 1.98.1, which rustup installs on first use; release builds are made with exactly that version.

```
cargo build --workspace
cargo test --workspace
scripts/check
```

`scripts/check` runs formatting, the build, Clippy with warnings denied, the documentation build with warnings denied, the tests, the conformance gate, the crate graph check and the repository hygiene scan: everything a change should pass before it lands.

## Project

Development is closed and the roadmap is decided by Soulstack; see [GOVERNANCE.md](GOVERNANCE.md). [CONTRIBUTING.md](CONTRIBUTING.md) says what that means for an issue or a pull request, and [CHANGELOG.md](CHANGELOG.md) tracks what has landed.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option, except that the documentation under `docs/`, including the language specification, is licensed under the Creative Commons Attribution 4.0 International licence (CC BY 4.0, full text in [`docs/LICENSE-CC-BY-4.0.txt`](docs/LICENSE-CC-BY-4.0.txt)). The exception is [`docs/brand/`](docs/brand/README.md), the HollowScript logo and wordmark files, which are not licensed under CC BY 4.0, Apache-2.0 or MIT; their use is governed by [TRADEMARKS.md](TRADEMARKS.md). See [NOTICE](NOTICE) for the copyright notice. The project uses HollowScript as a trademark; the licences above do not grant rights to it, and a fork must use a different name (see [TRADEMARKS.md](TRADEMARKS.md)). The owner has retained the name after a self-search; this does not claim registration or legal clearance.
