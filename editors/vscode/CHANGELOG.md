# Changelog

All notable changes to the HollowScript extension for Visual Studio Code are
recorded here. The format follows Keep a Changelog 1.1.0, and versions
follow Semantic Versioning.

## [Unreleased]

## [0.1.0] - Preview

First preview release, alongside HollowScript v0.1.0.

### Added

- Syntax highlighting for `.hws` files, following the lexical grammar of
  the specification: string interpolation (including nested strings and
  record literals inside it), filter literals, number literals, tagged
  comments, declarations and all 27 keywords.
- A language client for the HollowScript language server, started as
  `hollowscript lsp`: diagnostics, formatting, hover, go to definition,
  find references, rename, signature help, completion, the outline,
  folding, semantic highlighting and quick fixes.
- Discovery of the `hollowscript` command: the
  `hollowscript.executablePath` user setting, then `PATH`, then the
  installer's default directory, with the install command offered when it
  is not found.
- Commands to restart the language server and show its output, and the
  `hollowscript.trace.server` setting.
- Snippets for common declarations and statements.
