# Security

## Reporting a vulnerability

Please report a suspected vulnerability privately, through GitHub's
"Report a vulnerability" on this repository's Security tab, rather than by
opening a public issue.

Include what you did, what happened, and what you expected. A program that
reproduces it is worth more than a description: the whole implementation is
deterministic, so a program and the limits it ran under reproduce a run
exactly.

## What counts

HollowScript is made to run code its host did not write, so anything that
breaks one of the two promises counts:

- **A program reaching outside itself** without a capability the host
  granted and the program declared: reading a file, the clock, the
  environment or the network; reaching another instance's values; observing
  or changing the host beyond what a granted capability allows.
- **A run escaping its bounds**: a program that does not stop, that uses
  work or memory far beyond the limits it was given, that exhausts the
  host's own stack, or that crashes the process.

A panic anywhere in the implementation counts, whatever caused it, because
section 19.1 of the specification says it never happens: "never crashes,
hangs or exhausts its host on any input, including arbitrary bytes".

One thing is known and written up rather than reported: nothing here is
constant time. This is in [docs/design/security.md](docs/design/security.md),
which also says what the implementation promises and where the boundaries
are.

## Supported versions

This is pre-1.0. The current preview is v0.1.0; report issues against that
release. The command line, Rust API and bytecode format may change before 1.0.

The installer verifies signed checksums. The `latest.txt` version pointer is
not signed and has no expiry, so a compromised download service could keep a
default install on an older signed version at or above the installer's release
floor. An explicit `--version` avoids relying on that pointer. Freshness
protection is planned before a stable release.
