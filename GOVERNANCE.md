# Governance

HollowScript is developed in the class of Lua: the source is published, the
implementation can be embedded and studied freely under its licence, and the
day to day decisions about the language and this implementation are made by
Soulstack rather than by an open contributor process.

## How development works

- **Closed development, open source.** Work happens in a private
  repository. What is published here, per release, is the source at that
  release: the language specification, this native implementation, the
  conformance corpus and their design documents. There is no separate
  "upstream" a fork is behind; each release is a complete, buildable
  snapshot.
- **The roadmap is decided by Soulstack.** What the language gains next,
  which platform or embedding gets attention, and when something ships are
  editorial decisions made by the team maintaining the specification and
  this implementation, not put to a vote or opened for proposal.
- **No outside pull requests.** This repository does not accept pull
  requests; see CONTRIBUTING.md. A change proposed elsewhere cannot be
  merged here, however small or correct.
- **Issues are welcome.** A bug report, a question about the specification,
  or a report of a security concern (see SECURITY.md) is read and valued.
  It may inform the roadmap even though it cannot itself be a patch.

## Why this shape

A language and its runtime are only as good as the coherence of the
decisions behind them. While the specification is still settling and the
native implementation is still catching up to it, reviewing and maintaining
outside changes against a moving target costs more than it returns. This is
the same trade-off Lua itself has made for decades: a small, deliberately
steered core that outside users can vendor, embed and extend, without every
embedder's own fork needing to be reconciled upstream.

This document describes the current shape of governance, not a permanent
constitution. If and when it changes, it changes here, in a later release.

## Versioning and compatibility

The language specification's own version and compatibility rules are stated
in `docs/spec/README.md`. This implementation's own compatibility promises
(what is stable across a release, what is not) are stated in
`docs/design/release-and-milestones.md` and, for the C ABI specifically, in
`crates/hollowscript-capi/src/lib.rs`'s own documentation.
