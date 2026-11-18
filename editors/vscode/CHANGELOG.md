# Changelog

All notable changes to the HollowScript extension for Visual Studio Code are
recorded here. The format follows Keep a Changelog 1.1.0, and versions
follow Semantic Versioning.

## [Unreleased]

- Make language-server tracing follow its setting at the normal editor log level.
- Skip unsupported Windows batch wrappers when finding the native CLI.
- Pin copied installation commands to the supported v0.1.1 release.
- Clarify setup, privacy, refactoring limits and the preview status, with the
  current official HollowScript branding.

## [0.1.1] - Unpublished preview candidate

### Added

- Member completion for methods, record fields, namespaces and enum variants,
  including partial names, chains, generic and imported records, unsaved imports
  and Unicode. Uses the canonical v0.1.1 tooling in a bounded background worker,
  so the released v0.1.1 CLI does not need to change.
- Worker cancellation, memory, source and import limits, with confined file reads.
- Real editor acceptance coverage for member completion, references, rename,
  signature help, folding, semantic tokens and quick fixes.

### Fixed

- Use the existing Soulstack publisher, with extension ID `soulstack.hollowscript`.

- Adapt v0.1.1 code-point positions to UTF-16 for navigation, refactoring,
  quick fixes and semantic highlighting. Full document sync prevents Unicode
  edits corrupting the legacy server's source. A rename after an emoji is
  applied and verified in the editor.
- Quick fixes use exact canonical diagnostic spans, correcting the released
  server's whole-line ranges for missing uses, missing effect and unused names.
- References and rename load the declaring module first, so v0.1.1 includes
  an imported symbol's declaration even when that file was not already open.
- Updated the language client and packaging tools, refreshed transitive
  dependencies and pinned patched matcher/glob versions. Both dependency audit
  modes pass with no advisories.
- Documented v0.1.1 compatibility, build requirements and refactoring limits.

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
