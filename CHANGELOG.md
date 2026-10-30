# Changelog

All notable changes to this implementation are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Changes
that have landed but are not in a tagged release yet go under "Unreleased".
A release's heading carries its date; until the release is tagged the date
reads `TBD`, and the release step replaces it with the tag's date in the
commit that is tagged (see `docs/design/release.md`).

The language specification has its own version, stated in
`docs/spec/README.md`, and moves independently of this implementation's own
release history.

## [0.1.1] - 2026-10-30 (preview)

This preview adds signed command line and C SDK archives for Linux on x86_64
(static musl) and Windows on x86_64, alongside Apple Silicon macOS. Linux on
ARM64, Intel macOS and downloadable WebAssembly builds remain outside the
supported distribution.

### Security

- On Windows, `fs` and the module loader now read the hard link count from
  the open handle and refuse a hard-linked file, as on Unix (`Denied` for
  `fs`, HS0607 for an import). On every platform a file is accepted only when
  its link count is exactly one, so an unavailable or zero count is refused
  too, and the HS0607 reason now says so.

### Fixed

- The PowerShell installer parses under Windows PowerShell 5.1 and
  PowerShell 7 (a URI variable is now delimited).
- Diagnostics on Windows display paths with forward slashes, matching the
  canonical module path, for example `imports/main.hws`.
- The fuzz targets link on MSVC hosts.

### Release process

- Release archives use canonical tar metadata and are checked for path
  boundaries. A release is published only when every uploaded asset, digest
  and advertised platform matches reviewed native acceptance.
- Build records distinguish native from cross-compiling builders. The Linux
  archives are built on a native x86_64 Linux builder; the macOS and Windows
  archives on Apple Silicon macOS. Each product matches a second build from
  a different checkout and Cargo home on the same builder, which is not an
  independent reproduction.
- `latest.txt` is still unsigned and has no expiry; install an explicit
  version where that matters.

## [0.1.0] - 2026-10-29 (preview)

The first public preview distributes signed command line and C SDK archives
for Apple Silicon macOS only, with a signed build record and a source mirror.
The editor extension stores and other binary targets are deferred.

### Security

- Updated the HTTP host's TLS dependencies to rustls 0.23.46,
  rustls-webpki 0.103.16 and webpki-roots 1.0.11. The dependency advisory
  gate passes with these locked versions.
- The language server rejects message bodies above 32 MiB and header blocks
  above 8 KiB before allocation. An untrusted Content-Length can no longer
  cause a capacity-overflow panic.

### Added

- WebAssembly build: a host without a Worker can enforce a wall-clock
  deadline. `HollowScript.load` and `fromBytes` take `deadlineMs` (a budget
  for each load, call and resume, on a monotonic clock) and `shouldCancel` (a
  function asked at every safepoint), beside the existing `cancelFlag`, and
  `setDeadline` changes the budget later. A run over its deadline stops at its
  next safepoint with HS1006, and the instance stays usable.
- WebAssembly build: structured results. `loadProgram` and
  `loadProgramBatched` take `{ resultFormat: "json" }`, and every result of
  that instance (`loadResult`, `call`, `read`, and the `value` of a batched
  `call` or `resume`) is then the value itself. An `Int` above 2^53 - 1 in
  magnitude is a `BigInt` (on the wire, `{"bigint": "<decimal>"}`), so no `Int`
  is read back as a different one. New exports `hs_wasm_set_result_format`
  and `hs_wasm_fault_json`. The text results are unchanged and the default.
- WebAssembly build: structured faults. A fault of the program is thrown as a
  `HollowScriptFault`, an `Error` that also carries `code`, `class`, `file`,
  `line`, `column`, `spanStartByte`, `spanEndByte`, `frames` and, for a limit,
  `data`. `byteOffsetToUtf16` converts a span for a host that kept the source.
- Filter literals for a host that evaluates filters. The checker lists every
  filter literal it meets, with its decoded source and location
  (`hollowscript_check::CheckOutput::filter_literals`), and the facade has
  `filter_literals` to read them without running or compiling anything. In the
  WebAssembly `tooling` build, `filterLiterals(source, name, modules?,
  capabilities?)` answers each literal's source and its UTF-16 span in source
  order (new export `hs_wasm_filter_literals`), and `check` and `compile` take a
  filter schema as a fifth argument, so a program that names a field the host
  does not have is refused at check time (HS0708, HS0707, HS0709).
- `hollowscript-ide`, a library crate holding the language server's analysis
  with no transport and no disk: a checked `Workspace` built from an entry
  module, a caller-supplied capability interface and either a loader function
  or an in-memory map of modules, with binders, hover types, the outline,
  folding spans, semantic tokens and the keyword and prelude vocabularies.
  Positions are byte spans.
- Editor services in the WebAssembly `tooling` build (`HollowScriptTooling`,
  behind the `tooling` feature), all counting positions in UTF-16 code units
  with half-open ranges: `highlight(source)`, the lexical class of every
  token and comment; `keywords()`, every reserved word with a flag for the
  ones reserved for a later version; `standardCapabilities()`, the
  declarations of `print`, `clock` and `random` in the shape the
  `capabilities` argument takes; and `analyse(source, entryName,
  capabilities, modules)`, which checks once and answers `hover`,
  `completions`, `definition` and `documentSymbols` from an `Analysis` handle
  that is disposed with an idempotent `dispose()`. Completion offers names in
  scope by declaration order, then the prelude and the keywords, but not
  fields or methods after a `.` (a known gap). The outline includes `uses`
  and `import` items, an `exported` flag and a `detail`.
- `hollowscript-ide` gains `complete`, `highlight`, `text` (a `Utf16Map`
  between byte offsets and UTF-16 code units) and `types` (the type under a
  position).

- `cargo xtask sign` also writes `SHA256SUMS.sshsig`: the same file, signed by
  the same key in the SSH signature format (namespace
  `hollowscript-release-vX.Y.Z`), which `ssh-keygen -Y verify` reads.
  `scripts/release-upload.sh` requires it, verifies it and uploads it.
- `scripts/test-install` and `scripts/test-release`: offline behavioural tests
  of the installer and the upload script against a fixture release, signed
  with a throwaway key and served from a temporary directory on loopback.
  `scripts/check` runs them.

### Fixed

- WebAssembly build: the module's real memory is bounded. The logical
  `maxMemoryBytes` is not a proxy for it (a list of 600,000 pairs under a
  32 MiB limit reached 360 MiB of linear memory as a JSON result). The module
  now imports its linear memory (`env.memory`) with a declared maximum of
  256 MiB, set by `crates/hollowscript-wasm/build.rs` (and changeable for one
  build with `HOLLOWSCRIPT_WASM_MAX_MEMORY_MIB`), so a program that allocates
  for real is stopped at the ceiling, a host cannot give the module a larger
  memory, and `HollowScript.load`/`fromBytes` (and `HollowScriptTooling`) take
  `maxMemoryBytes` for a lower one. A failed allocation aborts Rust, which is a
  trap, so a module that has trapped is gone: the wrapper throws
  `HollowScriptTrap` (cause: the `RuntimeError`) and refuses every later
  call, `dispose()` being a no-op. The results writers hold a position in each
  container instead of a step for each element, so 600,000 pairs is 199 MiB as
  text and as JSON (it was 296 and 360), and the text renderer in
  `hollowscript-vm` does the same. `wasm-opt -Oz` and the `min-size` profile
  keep the maximum; the size change is under 1 KiB. The `tooling` build's
  16 MiB stack (below) lives in this memory, so that module declares a minimum of
  258 pages (16.1 MiB) beside the same 256 MiB maximum, a `maxMemoryBytes` below
  what a module needs to start is a `RangeError` that says so (never a trap), a
  ceiling with no headroom above it ends the first call that allocates in a
  `HollowScriptTrap`, and `HOLLOWSCRIPT_WASM_MAX_MEMORY_MIB` for that build
  must be at least 17.
- WebAssembly build and C ABI: writing a value out for a host is bounded. With
  `resultFormat: "json"` a returned value was written as JSON with no charge
  and no safepoint, so a six-line program that builds a value of about 2^50
  paths out of fifty shared objects made a Node process allocate about 3 GB
  and trap, whatever the memory limit and the deadline. The same walk wrote
  the arguments of every capability call (in the C ABI as well) and every
  queued command of the batched path. Every one of these writers now takes a
  budget of bytes (the output limit, and never less than 64 KiB) and stops
  with HS1004, a normal fault, when the text would pass it; every piece the
  walk writes is at least a byte, so the budget bounds the work however much
  of the value is shared. The walk also asks the host whether to stop every
  1,024 pieces, so a cancellation or a deadline (HS1006) reaches it. A fault
  while writing a result leaves the instance as it is after any call that
  returned. After a fault while draining queued commands the run cannot be
  resumed and the instance is disposed. The text format is unchanged (it keeps
  its own 16 MiB cut-off). The charge for a capability call's arguments
  (section 11.3) still counts each shared object once.
- WebAssembly build: an `Int` above 2^53 - 1 crossed to a capability handler
  and a queued command as a JSON number a double cannot hold, so
  `9007199254740993` arrived as `9007199254740992` and distinct 64-bit `Int`s
  collided. Arguments now use the same exact form as results
  (`{"bigint": "<decimal>"}`), and `hollowscript.js` hands a handler a `BigInt`
  for one. A handler and a host may also answer with a `BigInt`
  (`JSON.rawJSON`, where the platform has it).
- Equality no longer walks every path through shared values. Comparing two
  values built by the same repeated doubling (`a = Json.Array([a, a])`, thirty
  levels) took nearly twenty seconds of CPU, forty levels would not finish, and
  neither a step limit nor a cancellation could interrupt it. Equal handles are
  now equal without a walk; a comparison that outruns its charge remembers the
  pairs it has visited, charges the further pairs in steps, and polls the step
  limit and cancellation as section 11.3 requires; and `list.contains` and
  `list.indexOf` use the same walk. A comparison whose distinct pairs would
  exceed about 4 million faults with HS1015. `<`, `<=`, `>` and `>=` on strings
  are now charged as section 11.3 says (they were free), and comparing or
  sorting one string held many times no longer reads it for every comparison.
- The verifier now checks every string id inside a constant (`String` and
  `Filter`) and every function name against the string table, and that a
  `Float` constant is finite and not negative zero. It looked only that the
  constant's own index was in range, so a 316-byte container whose constants
  named string `u32::MAX` verified, and the interpreter then resized a cache to
  four billion entries when the constant was loaded: a trap of the WebAssembly
  module and, natively, an out-of-memory kill. The interpreter's constant cache
  is no longer sized from an id the string table does not reach. A seeded
  mutation test (`hollowscript/tests/mutated_bytecode.rs`) corrupts operand
  bytes of corpus containers, repairs the checksum and requires every mutant to
  be refused or to run within its limits.
- WebAssembly `tooling` build: legal nesting no longer traps the module. The
  front end ran on the module's own 1 MiB shadow stack (there are no threads),
  against documentation that claimed 16 MiB, and 233 nested closures, a 3.7 KB
  source inside the structural limit of 250, trapped `check`, `format`, `compile`
  and `analyse` with `memory access out of bounds` instead of reaching the
  HS0113 diagnostic. `crates/hollowscript-wasm/build.rs` now links the `tooling`
  build with a 16 MiB stack. The measured cost of each construct is in
  `web/README.md`, and `web/node-e2e/deep-nesting-check.mjs` runs every one to
  its limit.
- WebAssembly build: a module that traps stays dead, and now says so. A trap
  abandons the Rust frames without restoring the shadow-stack pointer, so every
  later call on the module threw. `hollowscript.js` now throws a
  `HollowScriptTrap` (with `fatal` and the trap as `cause`) from the first call
  that traps, marks the whole instance dead (`dead` on `HollowScript` and
  `HollowScriptTooling`, and every `Instance`, `AsyncInstance` and `Analysis`
  made from it), and refuses every later call with a `HollowScriptTrap` before
  it reaches the module, so a host knows to discard the instance.
- Diagnostics are capped as they are found, not after. The per-file cap of
  section 17.1 was applied only when the set was finished, so a 3.9 MiB file of
  `"` and a line feed collected four million diagnostics (3.7 GB) in
  `hollowscript check`, and the browser build kept 2.4 GiB. A new
  `BoundedDiagnostics` holds the smallest 101 of each file in the canonical
  order, so the result is the same as capping at the end whatever order they
  were found in; the lexer, the parser, the resolver, the `uses` check, the type
  checker and `DiagnosticSet` use it, and the formatter caps the syntax errors it
  refuses with. The same file now takes 2.0 GB (the tree and the checker's tables
  are still linear in the source; see `docs/design/security.md`).
- WebAssembly build: a call's arguments are now checked against the export's
  parameters, as section 12.4 requires and the facade already did. The JSON
  decoder built a value of whatever shape the text had, so a string for an `Int`
  parameter, `1.5` or `1e300` for an `Int`, a set for a list, a map of strings
  for a `Map<String, Int>` or the wrong number of arguments reached the program,
  which then answered wrongly or faulted as an internal error. `call` and the
  batched `call` now refuse them before anything runs, with the argument and the
  type it should have, and convert an `Int` for a `Float` parameter.
- The verifier now requires the names of capabilities, of the functions of a
  namespace capability and of exports to be identifiers of the lexical grammar
  (ASCII letters, digits and `_`, not starting with a digit) and distinct. It
  required only that capability names be distinct and not empty, and the
  WebAssembly build wrote them into the JSON array of `capabilityNames()`
  unescaped, so a name such as `print","clock.now` shifted every handler a host
  bound after it and one with a backslash or a line feed made `JSON.parse`
  throw. The names are also escaped now.
- WebAssembly build: every capability called `print` is charged against
  `maxOutputBytes`, whatever shape it declares. Only a plain `print` taking one
  `String` was, and a host binds handlers by name alone, so bytecode declaring
  `print` as taking `[String]` delivered 256 MiB through a handler (a 20,000
  element line sent 200 times) under an output limit of 1,000. The text the
  arguments carry is charged before the handler is called, counting a value a
  list holds many times every time. The standard shape is charged exactly as
  before.
- WebAssembly build: the table of instances no longer grows with every load.
  `hs_wasm_dispose` emptied a position that nothing reused, so each load and
  dispose added about 1.9 KiB to the module's memory for good (38 MB over 20,000
  cycles, 144 MiB over 100,000). A disposed position is now reused, and a handle
  carries a generation so that the handle of a disposed instance is refused even
  once its position has been reused and disposing it again leaves the new
  instance alone. A position that has held 65,535 instances is retired, and at
  most 65,535 instances can be live at once.
- WebAssembly build: limits are validated. `hs_wasm_set_limits` did not call
  `Limits::validate`, and `setLimits({ steps: -1 })` reached it as the unsigned
  integer 2^64 - 1, a limit nothing ever reaches that a counter added to it
  could overflow (a panic, and on `wasm32` a trap). A limit outside 1 to
  2^63 - 1 now refuses the whole call and changes none of the limits, and
  `setLimits` throws a `RangeError` or a `TypeError` for a negative number, a
  fraction, `NaN`, a string or a value above 2^63 - 1 (a limit above 2^53 - 1 is
  given as a `BigInt`). The run counters saturate instead of overflowing, whatever
  a host puts in a `Limits`.
- WebAssembly build: the comment that said a `Float` stays distinguishable from
  an `Int` in a JSON result was only true of the text. A `Float` is written with
  a decimal point or an exponent and an `Int` never is, and a test now pins that,
  but `JSON.parse` reads both as a `Number`, so a whole `Float` cannot be told
  from an `Int` in a value `hollowscript.js` returns. The comment and
  `web/README.md` now say so, and what a host that must keep them apart can do.
- WebAssembly build: `Instance.call` and `AsyncInstance.call` no longer leak a
  buffer in the module's memory when their arguments cannot be turned into JSON (a
  circular object, a value whose `toJSON` throws): the name was uploaded first
  and never freed when the conversion then threw (the `tooling` build's `check`, `compile` and
  `filterLiterals` had the same fault, with up to four buffers). An `Analysis`
  that is garbage collected without `dispose()` is now disposed by a
  `FinalizationRegistry`, so a forgotten handle no longer holds one of the
  module's 64 places for good; calling `dispose()` is still preferred.
- The formatter no longer takes time exponential in the nesting of `match`
  arms. An arm that holds one statement is tried on one line, by printing the
  statement and discarding the text if it comes to more than one line, and an
  arm that holds another `match` was printed twice at every level, so 24 nested
  arms took 12 seconds and 40 never finished (`hollowscript format`, the
  language server and the browser build all hung). The outcome of the attempt is
  kept for each statement, and the output is unchanged (every file of the
  corpus formats to the same text).
- The checker no longer takes time exponential in the nesting of overloaded
  numeric calls. `abs`, `min`, `max` and `clamp` check their arguments to choose
  the `Int` or the `Float` signature and then check them again against the
  chosen one, so a call nested in another was checked twice at every level:
  `abs(abs(...))` 22 levels deep took a second, 30 levels four minutes, and a
  program within the nesting limit never finished. The answer to the first check
  is kept.
- WebAssembly build: what a host-bound `print` writes now counts against
  `maxOutputBytes`, as sections 11.6 and 16.2 require. The module charged
  nothing for it, so a program could print without limit through a JavaScript
  host. The UTF-8 bytes of the text and one line feed are charged before the
  host's function is called (and, on the batched path, before the call is
  queued), a call that would pass the limit is never made and faults with
  HS1004, and the host's `print` does not write that line. The virtual machine
  has `CapabilityTable::with_output_charged` for a table whose `print` it cannot
  look inside.
- The diagnostics JSON written by the C ABI's `hs_check_json` and the WebAssembly
  tooling build's `check` was malformed for any diagnostic with a related
  location (a repeated declaration, for example): each `related` object began
  with a stray comma. It is valid JSON now, with a regression test for each.
- The installers (`install.sh`, `install.ps1`) failed open: on a machine with
  no `minisign` and no OpenSSL that can verify Ed25519 (a stock macOS, whose
  OpenSSL is LibreSSL, and Windows) they trusted a `SHA256SUMS` fetched from
  the same origin as the archive and never fetched the signature, so a
  compromised origin defeated the signing that exists to stop it. They now
  refuse unless a verifier is available, and say how to get one. The verifier
  is `minisign`, an OpenSSL with Ed25519, or `ssh-keygen` (OpenSSH 8.0 or
  newer, present on macOS and Windows 10 and 11) checking the release's new
  SSH-format signature. The one way past it is `HOLLOWSCRIPT_ALLOW_UNSIGNED=1`
  (exactly `1`), which prints a warning, and which does not apply when the
  only downloader is BusyBox's `wget`, which does not validate certificates.
  `HOLLOWSCRIPT_REQUIRE_SIGNATURE` now wins over it, and it, the downgrade
  flag and the no-modify-PATH flag accept `1`, `true` and `yes` alike (the
  literal `1` only, before) and refuse an unrecognised value.

- The command line host's resource and credential handling:
  - A gzip `http` response was limited by its compressed size, so 300 KB of
    gzip decoded to 300 MiB, past the 8 MiB limit. The limit is on the decoded
    body now, read a chunk at a time.
  - `stdin.readAll` and `stdin.readLine` buffered all of their input before
    charging it (700 MB piped in gave a resident size of 705 MB against a
    64 MiB `maxMemoryBytes`). They read through what is left of the limit and
    raise HS1002 once past it.
  - The module loader read a named pipe, and a 1.5 GB file, before looking at
    their kind or size, and `fs.write` and `fs.append` blocked for ever on a
    named pipe with no reader. Files are opened without waiting, judged from
    the open handle, accepted only as regular files, and the loader reads
    through the language's own source limits (HS0002, HS0612).
  - On Unix, a file with more than one hard link is refused by `fs.read`,
    `fs.write`, `fs.append` (`Denied`) and the module loader (HS0607): inside
    a granted root it can be the same file as one outside it. A legitimately
    hard-linked file is refused too, and on Windows a hard link is not
    detected (see `docs/design/security.md`).
  - On a redirect to another origin every header the program supplied is
    dropped except a short allowlist (`Accept`, `Accept-Language`,
    `Cache-Control`, `Content-Type`, `If-Modified-Since`, `If-None-Match`,
    `Pragma`, `User-Agent`); only `Authorization` was dropped before, so a
    `Cookie` or an API key was forwarded.
  - A program can no longer set `Upgrade`, `TE`, `Trailer`, `Expect`,
    `Keep-Alive`, `Proxy-Connection`, `Proxy-Authorization`, `Forwarded`,
    `X-Forwarded-*`, `X-Real-IP`, `X-Original-URL`, `X-Rewrite-URL` or the
    method override headers (`Err(Invalid)`).
  - The `http` address refusal list gained `::ffff:0:0:0/96` (and the IPv4
    address it embeds), `192.88.99.0/24`, `3fff::/20`, `2001:2::/48`,
    `2001:10::/28` and `5f00::/16`.

- The language server's module loader followed symbolic links and read
  whatever it was pointed at: `import x from "./creds"` with `creds.hws` a
  link to a file outside the repository produced diagnostics that quoted that
  file's tokens, and a link to a named pipe hung the server. It reads files
  the way the command line host does now (the same code, in
  `hollowscript-hostfs`): no link followed, regular files with one name only,
  opened without waiting, and read through the language's own source limits.

- Release scripts and installers:
  - `latest.txt` is not signed, so a fresh install could be steered to any
    older signed release. The installers have a release floor, the oldest
    release an install that asked for "the latest" will accept, in the file
    the site serves rather than the bucket (`release_floor` in `install.sh`,
    `$releaseFloor` in `install.ps1`). `scripts/release-upload.sh` checks the
    two agree and that the key they pin is the one a release is verified with,
    and refuses a stable release below it. Rollback inside the window and a
    freeze are not closed; `docs/design/release.md` says exactly what is and
    what is left, and names the follow-up.
  - `install.ps1` sets TLS 1.2 (and 1.3 where there is one) by assignment,
    not by adding to what Windows PowerShell 5.1 offers, and follows
    redirects itself so that an https to http redirect is refused.
  - `install.sh` took `http://127.0.0.1:1@example.org` for a loopback URL.
    The base URL is parsed (the host is what precedes the first `/`; a user
    name is refused); `install.ps1` uses `System.Uri` with the same rule.
  - `scripts/release-upload.sh` accepted any `--public-key`. It must be the
    pinned key, except with `--allow-unpinned-public-key-for-testing` and a
    local directory as the bucket.
  - `scripts/release-upload.sh` checked the signed files and then read them
    again, much later, to upload them. It copies them to a private directory
    and verifies and uploads the copy.
  - `scripts/release-mirror.sh` ran `git init` in any directory with no
    `.git` and then deleted everything in it. It uses only a directory that
    does not exist, an empty one, or a repository it made itself.
  - A first install whose binary failed its check after being moved into place
    left that binary behind; it is removed (and an upgrade is still rolled
    back).

### Changed

- New crate `hollowscript-hostfs`: how a host opens a file it has only a name
  for (relative to a directory handle, no symbolic link, a regular file with
  one name, opened without waiting, read through a limit), used by the
  command line host and the language server. It depends on `cap-std` and, on
  Unix, `libc`.
- `scripts/check` runs the Node end-to-end checks of the WebAssembly boundary
  (`crates/hollowscript-wasm/web/node-e2e/run-all.sh`) when `node` and the
  `wasm32-unknown-unknown` target are installed, and says so when it skips
  them. The script builds the VM-only and `tooling` modules, the command line
  and the fixture builders with a Cargo target directory of its own, compiles
  the sample programs in a temporary directory and runs every check; a check
  passes only if it exits 0 and prints `all checks passed`. It adds about
  90 seconds from cold and a little over a minute when repeated.
- WebAssembly build: the module imports `env.memory`, so a host that does not
  use `hollowscript.js` must create a `WebAssembly.Memory` with the minimum the
  module declares and a maximum no larger than the one it declares, and pass it
  in the imports (`web/README.md`, "Memory"). `HollowScript.load` now fetches
  the module and instantiates it from bytes instead of streaming it, to read
  those limits; `readMemoryLimits(bytes)` is exported.
- WebAssembly build: `HollowScript` has `memoryBytes`, the size of the module's
  linear memory, and `memoryCeilingBytes`, the most it can grow to.
- WebAssembly build: `hs_wasm_commands_json` can answer `Faulted` (HS1004) and
  `hollowscript.js` throws it as a `HollowScriptFault` from `call` or `resume`.
  `WasmModule::render_json` and `WasmModule::command_argument_json` take a
  `JsonBudget` and return a `Result`.
- WebAssembly `tooling` build: `hs_wasm_check` and `hs_wasm_compile` take two
  more arguments, the filter schema's pointer and length (null and 0 for
  none), after the modules'. `hollowscript.js` is the only caller most hosts
  have and is updated. A malformed argument is now reported as "the request is
  malformed" rather than "the capability declaration is malformed", since it
  can be the modules or the schema as well.
- `hollowscript-lsp` is now a thin adapter over `hollowscript-ide`. Its
  behaviour is unchanged except where the shared analysis is better:
  completion offers every keyword the language reserves for use today (`and`,
  `false`, `from`, `not`, `or` and `true` were missing; the list is now
  derived from the lexer's own keyword table), only the names in scope at the
  position, with a signature or type as the detail, the prelude's type names,
  and nothing inside a comment, a string or after a `.`; closure parameters
  and the locals of a closure are resolved; an imported function gets
  signature help and its own kind; go to definition no longer answers inside a
  comment or a string or for a field name; and the outline includes `uses`
  and `import` lines and shows a signature or type as the detail.

## Initial preview implementation

**Status: preview.** The first tagged release. The language and this
implementation are usable end to end, but nothing is stable yet: the Rust
API, the command line and the bytecode format may change in any release
(only source compatibility, and the C ABI within a major version, are
promised; see `docs/design/bytecode.md`). It implements version 1.1.1 of
the language specification.

### Added

- The front end: a lexer, a parser with error recovery, and the canonical
  formatter (`hollowscript-syntax`).
- The checker: name resolution, type inference, optional narrowing, effects
  and capabilities, pattern exhaustiveness, and module loading in
  specification order (`hollowscript-check`).
- The compiler and bytecode: a register-machine instruction set, a
  versioned and checksummed binary container, and a verifier that is the
  only path bytecode reaches the virtual machine through
  (`hollowscript-compiler`, `hollowscript-bytecode`).
- The virtual machine: a mark and sweep heap with the byte accounting
  chapter 11 specifies, the standard library of chapters 13 to 15, and
  capability calls with section 9.6's host-result acceptance rule applied
  to every one of them (`hollowscript-vm`).
- The embedding facade: `check`, `build`, `load`, `call`, `read`, `cancel`,
  `dispose`, exports and the batched command and resume path of chapter 12
  (`hollowscript`).
- A command line tool: `check`, `format`, `run`, `build`, `lsp`, `version`
  and `help`, with `--allow` granting `env`, `args`, and scoped `fs`,
  `stdin` and `http` (`hollowscript-cli`). `run` also runs a container
  `build` wrote, through the facade's new `load_bytecode`.
- A C ABI over opaque instance handles, with host-bound capabilities and a
  panic stopped at the boundary (`hollowscript-capi`). `hs_version()`
  reports the crate's own version.
- A WebAssembly build of the virtual machine, with a hand-written
  JavaScript boundary and no parser or checker in the browser
  (`hollowscript-wasm`).
- A language server: diagnostics, formatting, hover, go to definition,
  find references, rename, signature help, completion, an outline, folding,
  semantic tokens and quick fixes, with names declared in imported files
  resolved (`hollowscript-lsp`, started by `hollowscript lsp`).
- A VS Code extension (`editors/vscode`, version 0.1.0, preview): the
  TextMate grammar checked against the specification's lexical grammar,
  snippets, and a client for `hollowscript lsp` that finds the command in
  its setting, on `PATH` or in the installers' directories, and says how to
  install it when it is missing. Bundled with esbuild, with its own
  licences and third-party notices; not yet published.
- The shared conformance corpus (`corpus/`) and this implementation's own
  runner, comparing parse diagnostics, check diagnostics, the formatter's
  output and idempotence, and a program's run against each case's expected
  file, gated on a shrink-only baseline (`hollowscript-conformance`).
- Continuous fuzzing of the lexer and parser, the formatter, the checker,
  the whole pipeline, the bytecode container decoder and verifier, and the
  C and WebAssembly ABI surfaces (`fuzz/`; see
  `docs/design/fuzzing.md`), alongside the deterministic, seeded fuzz tests
  already run by `scripts/check`.
- A benchmark harness comparing this implementation's cold start, load
  time, throughput, peak memory and binary size against Lua and the
  TypeScript baseline (see `docs/design/performance.md`).
- Public project policy: this changelog, `GOVERNANCE.md`, `TRADEMARKS.md`,
  `NOTICE`, and `docs/security.txt`.
- The HollowScript logo: masters, renderings and a usage guide in
  `docs/brand/`. Those files are not licensed under CC BY 4.0, Apache-2.0
  or MIT; `TRADEMARKS.md` governs their use.
- A release pipeline (`docs/design/release.md`):
  `scripts/release-build.sh` builds macOS (arm64 and x86_64), static musl
  Linux (x86_64 and aarch64), Windows (x86_64) and WebAssembly, as a CLI
  archive (`hollowscript` and `hollowscript-lsp`) and an SDK archive per
  target, with per-archive `THIRD-PARTY-LICENSES`, a CycloneDX SBOM, a
  version guard, stripping by rustc, and a check that every shipped file is
  byte-identical when built from another path and contains no local path.
  `cargo xtask sign` signs `SHA256SUMS` with a Cloud KMS Ed25519 key in
  minisign's format; `scripts/release-upload.sh` uploads resumably and
  never overwrites; `scripts/release-withdraw.sh` and
  `scripts/release-mirror.sh` complete it. The toolchain is pinned to Rust
  1.98.1.
- Installers: `curl -fsSL https://get.hollowscript.org/install.sh | sh`
  (macOS and Linux) and `irm https://get.hollowscript.org/install.ps1 | iex`
  (Windows). They verify the signature when minisign (or, for `install.sh`,
  an OpenSSL with Ed25519) is available and the SHA-256 otherwise, install
  both binaries with rollback, set up `PATH`, pin a version with
  `--version` or `HOLLOWSCRIPT_VERSION`, and uninstall.
- `get.hollowscript.org`: a Firebase Hosting site that serves the
  installers and redirects release downloads to the release bucket,
  deployed only from a published tag.

### Fixed

- `hollowscript` no longer panics (exit code 101) when standard output is
  closed early, as in `hollowscript run app.hws | head -1`: the run stops
  quietly, and `format`, `check` and the rest exit without a message. No
  write to standard output or standard error in the command or the
  language server can panic any more.
- `run` writes each `print` to standard output as it happens instead of
  after the run ends, so a question printed before `stdin.readLine()` is
  seen before the program waits for an answer, and a long loop shows its progress. `run --json` still collects the
  output into its one document.
- A string literal with no interpolation, and a filter literal, is no
  longer charged against the memory limit each time it is evaluated
  (section 11.4): `print("spam")` in a long loop used to fault HS1002.
  Each constant is now one shared heap object, so real memory stays
  bounded by the size of the program.
- `run` accepts a container that `build` wrote, verified before it runs,
  instead of failing with "stream did not contain valid UTF-8".
- `hollowscript COMMAND --help` and `-h` print that command's own usage
  instead of treating `--help` as a file name; `help COMMAND` does the
  same.
- A runtime fault is reported as `FILE:LINE:COLUMN: error[code]: message`,
  and every human-readable diagnostic now includes its column.
- A source file that is not valid UTF-8 is the program's HS0001 with exit
  status 1 (section 1.1), not an input and output error. With `--json`, a
  problem with the invocation itself (a file that cannot be read, a bad
  option) is a JSON document, `{"error": "..."}`, too.
- `run --json` on a runtime fault includes `"stage":"run"`, as a check
  failure includes `"stage":"check"` (the shape of section 19.5).
- Matching on the result of a capability that was not granted, or naming
  its error type (`IoError`, `HttpError`), reports HS0404 alone, not a
  cascade of HS0208, HS0504, HS0503 and HS0205 errors, some naming the
  checker's internal `<error>` type, which is never shown any more. An
  unrelated mistake in the same program is still reported.

- Programs of any width compile: a list or map literal of any length, a
  long `+` chain, deep nesting, hundreds of locals, arguments, fields or
  interpolations, and function bodies longer than a short jump. They used
  to fail with HS1099 ("more than 256 registers is not implemented yet")
  or "a jump further than 32767 instructions". Registers, counts and
  positions reach 65,535 through a new `Ext` operand prefix, long literals
  are built in chunks, and far jumps become `JumpLong`.
- Checking a very long line is linear, not quadratic.

### Changed

- The specification is version 1.1.1. It corrects sections 2.1, 4.6, 9.7,
  13.6 and 16.1: `IoError` and `HttpError` are declared by the capabilities
  that mention them and are visible in every file of a run whose interface
  declares them, rather than being prelude names that every file sees, and
  `HttpRequest` and `HttpResponse` have no name, so a program declares a
  record with the same fields. The text now says what this implementation
  and the corpus already did, so no program it accepted is rejected, and
  three new cases in `corpus/capabilities/` pin the rule.
- The specification is version 1.1.0 (before that): section 1.10 lets an implementation
  document limits of its own, reported as HS0901 before a run and HS1015
  during one, never as an internal error. This implementation's limits
  (65,535 registers in one function, a function's code within a
  `JumpLong`, and a bound on the registers the active calls of one run
  hold) are listed in `docs/design/bytecode.md`.
- The bytecode container format is 0.2 (0.1 before): a 0.1 container is
  refused, and containers are rebuilt after an upgrade.
- The interpreter is faster: it resolves a fault's source location only
  when a fault needs it, and dispatches an operand prefix as its own
  opcode.
- `hollowscript` with no arguments prints its usage to standard error and
  exits with status 2 (it printed to standard output with status 0).

### Removed

- The `repl` command, which only said it was not implemented. An
  interactive REPL is planned for a later release.

### Known gaps

`docs/design/release-and-milestones.md` is the current account of what is
done and what remains; it is not repeated here. Before this preview becomes
a stable release, it still needs an outside security review and a test of
the Windows build on Windows.
