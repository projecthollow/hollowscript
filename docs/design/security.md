# Security: what this implementation promises, and what it does not

HollowScript exists to run code you did not write. That is the whole point
of the language: a host embeds it so that a user, a plugin author or a
generated program can compute something without being able to reach anything
the host did not hand it. This document says what the implementation
guarantees, what it deliberately does not, and where the boundaries worth an
outside review are.

## The two promises

**1. A program cannot reach outside itself.** There is no ambient authority:
no file system, no clock, no network, no environment, no randomness, unless
the host grants a capability and the program declares it with `uses`
([Chapter 9](../spec/09-effects-and-capabilities.md)). A program with no
`uses` line can only compute a value from its own literals and its imports.
The capability interface is fixed before the program is checked, so a program
that names a capability the host did not grant is refused before any
statement runs.

**2. A run is bounded.** Every run has a step limit, an allocation limit, an
output limit and a call depth limit ([Chapter 11](../spec/11-resource-limits.md)),
all enforced by the interpreter rather than by the host's patience. Reaching
one is a fault the host sees, not a hang. The limits are part of the language,
so the same program reaches the same limit at the same operation on every
conforming implementation, and a host that lowers one is stricter than the
language rather than differently behaved.

Everything below is in service of those two.

## What the implementation does to keep them

- **No dependencies on the path from source to running code.**
  `hollowscript-core`, `hollowscript-syntax`, `hollowscript-check`,
  `hollowscript-compiler` and `hollowscript-vm` depend on the standard
  library and on each other, and on nothing else
  ([dependencies.md](dependencies.md)). The code a reviewer must audit for
  the interpreter's own safety is exactly this workspace's own.
- **No `unsafe` where untrusted input is handled.** Every crate carries
  `#![forbid(unsafe_code)]` except `hollowscript-capi` and
  `hollowscript-wasm`, which are the two boundaries a non-Rust caller
  reaches, where a raw pointer is unavoidable. In those two, each `unsafe`
  block does one thing, is preceded by a `SAFETY` comment stating its
  precondition, and the precondition is checked in safe code immediately
  before it. `cargo xtask graph` enforces the crate graph and the hygiene
  scan runs over the whole tree.
- **No panics on any input.** `unwrap`, `expect`, `panic`, `todo`,
  `unimplemented`, `dbg!` and process exit are denied by lint across the
  workspace, and indexing that can panic is warned on. Section 19.1's
  promise is that the implementation "never crashes, hangs or exhausts its
  host on any input", so a panic is a defect, not a way of reporting one.
- **A panic that happens anyway does not cross a boundary, except on
  `wasm32-unknown-unknown`.** Unwinding is kept on in every
  profile so that the C ABI and a native build of the WebAssembly ABI can
  each catch one with `catch_unwind` and answer a status; the instance a
  panic happened inside is not put back into the registry, so every later
  call on that handle is refused (`hollowscript-wasm::abi::Slot::Busy`,
  never restored to `Occupied`) rather than reaching state a panic left
  half changed. `wasm32-unknown-unknown`, the target a browser actually
  loads, has no unwinding support at all: `catch_unwind` compiles there but
  never has anything to catch, because a panic on that target is a `trap`
  instruction, which does not unwind Rust frames and does not return to
  `with_instance`'s caller; it stops the whole WebAssembly instance where
  it stands, mid-operation, with no promise about what any Rust value it
  was working on (a `RefCell`'s borrow flag, the interpreter's own call
  stack) is left holding. This is a property of the target, not something
  this crate's own code can catch or work around: a host embedding the
  actual `.wasm` build must treat a trap (an exception thrown by calling
  any exported function after instantiation) as fatal to the whole module
  instance, not only to the `HollowScript` handle that was mid-call, and
  re-instantiate rather than continue calling it; `web/hollowscript.js`
  enforces it: a trap is thrown as `HollowScriptTrap` and every later call
  on that module is refused without entering it. A failed allocation at the
  memory ceiling (below) is the same trap. This does not affect
  `hollowscript-capi`, or a native (non-`wasm32`) build of
  `hollowscript-wasm` such as the one this workspace's own tests run
  against, where `catch_unwind` behaves as the paragraph above describes.
- **The host's stack is not the program's.** Script calls run on an explicit
  frame stack, so recursion reaches `maxCallDepth` as a fault long before it
  could exhaust the host's own stack, which section 11.5 requires in so many
  words. Every deep walk over a value (equality, comparison, tree size, the
  retained-size walk, rendering, the acceptance check) is iterative for the
  same reason. Iterative alone is not enough over a value that shares or
  refers back to its own structure: a captured `variable` can hold a cell
  that (through a list) holds itself, and a value built by repeated doubling
  (`t = Tree.Node(t, t)`) shares almost everything between its two halves,
  so `Heap::tree_size` and every walk built on it (`check_retained`,
  comparison charges, capability sizing, `list.contains`/`indexOf`)
  deduplicate by handle: an already-counted object contributes nothing a
  second time, which is what makes the walk terminate over the first case
  and stay linear in the number of distinct objects, not the number of
  paths to them, over the second. Each also stops as soon as its running
  total passes the caller's remaining budget, rather than finish counting a
  value already known to be over it.
- **Equality does not walk every path through shared values either.**
  Counting each object once is not enough for the comparison itself: two
  values built by the same repeated doubling (`a = Json.Array([a, a])`) are
  different objects at every level, and a walk that follows every path
  through them visits 2^levels pairs while the comparison's charge, which
  counts each object once, is a few dozen steps. `Interpreter::values_equal`
  therefore treats two handles to the same object as equal without a walk,
  and, once a comparison has visited more pairs than its charge paid for (one
  pair for each 8 bytes of the charged size), remembers every pair of objects
  it has scheduled so each distinct pair is visited once, charges a further
  step for every 8 pairs it visits past that point, before visiting them, and
  polls the step limit and the cancellation token in the slices of section
  11.3. The remembered pairs are themselves capped (about 4 million); a
  comparison that needs more faults with HS1015 rather than let its own
  bookkeeping grow without a bound. `list.contains` and `list.indexOf` use
  the same walk for each element. A comparison that stays within its charge,
  which is every comparison of values that do not share structure, takes
  none of these paths and costs what it always did. `<`, `<=`, `>` and `>=`
  on strings are charged by section 11.3's comparison rule like `==` is, and
  comparing or sorting by one string handle used many times does not read the
  string once for each comparison. The charge itself still counts each object
  once, where section 11.4 defines a tree size that counts a shared value
  once for every path to it; the walk above is what keeps that difference
  from being a way to make a run cost more than its limits allow.
- **Nor are frames free.** A frame's registers are real memory that no
  counter of section 11.4 charges, and a frame holds up to 65,535 of them
  (`MAX_REGISTERS`, see [bytecode.md](bytecode.md)), so call depth alone no
  longer bounds what recursion can hold. The interpreter bounds the
  registers every script call of one run holds between them: 256 for each
  level of `maxCallDepth` and for the frame the run starts in, and never
  less than 4,194,304 (128 MiB of values). Frames no wider than 256
  registers, which is nearly all of them, reach the call depth limit
  exactly as before; recursion through very wide frames faults with HS1015
  (an implementation limit, section 1.10) at the call that would go over. `ListAppend` and `MapPut`, which finish
  a long literal in place, charge memory as the one literal would, and
  `MapPut` charges a step for every 64 entries it moves, so neither can do
  unbounded work between safepoints.
- **Nor is it the front end's, while getting from source to bytecode.**
  Parsing, checking and compiling each recurse over the syntax tree or the
  HIR up to section 1.10's own structural limits (250 levels of nesting,
  or, through a chain's own weight, deeper still), which "MUST NOT exhaust
  the host's stack" in that section's own words, whatever stack the host's
  calling thread happens to have. All three run on one dedicated,
  generously sized thread (`hollowscript_core::stack`), not the caller's
  own, from every entry point that reaches them: `hollowscript::check`,
  `build` and `load`, and therefore `hs_check_json`, `hs_load` and
  `hs_load_ex` in the C API. Before checking and compiling moved onto that
  thread, a legal, 100 to 200-level nested program could abort the process
  on a caller thread as small as 512 KiB or 1024 KiB, which no host could
  catch or recover from.
  `wasm32-unknown-unknown` has no threads, so there the same passes run on
  the module's own shadow stack, and that is what bounds them: the `tooling`
  build of `hollowscript-wasm` (the only one that holds a front end) is linked
  with a 16 MiB stack by its `build.rs`, the size of the thread everywhere else.
  The linker's default, 1 MiB, was not enough: a legal program of 233 nested
  closures (3.7 KB of source) trapped the module, where every other construct
  reached its limit, because a level of nesting costs between 0.7 KiB (a chain
  link) and 4.5 KiB (a closure) of stack and 250 closures need a little over
  1.1 MiB. The measured costs are in `crates/hollowscript-wasm/web/README.md`,
  and `web/node-e2e/deep-nesting-check.mjs` runs every construct to its
  deepest legal nesting through every entry point. The stack is address space
  that is not touched until a deep program uses it. If the module traps all the
  same (a defect, or a stack no legal program reaches), the JavaScript wrapper
  marks the whole instance dead at the first trap and refuses every later call
  with a `HollowScriptTrap`, because a trap leaves the shadow-stack pointer
  where the abandoned frames left it.
- **Whatever is written out for a host is bounded too.** A value that
  shares its sub-values (`t = Tree.Node(t, t)`, fifty times) is fifty objects
  and 2^50 paths, and the JSON encoding of a value writes a sub-value once for
  every path to it, so the text of a small value can be astronomically long.
  Section 11.4 counts such a value along every path, which is why this
  implementation's own deduplicating size walks (above) are what let it exist
  at all, and the text results had their own 16 MiB cut-off
  (`hollowscript-vm::render`). The JSON writers in `hollowscript-wasm` (a
  call's, read's and load's result, a capability call's arguments and the
  commands the batched path drains) and the argument writer in
  `hollowscript-capi` take a budget instead: the output limit, and never less
  than 64 KiB so that a host that sets a tiny output limit to stop `print`
  still exchanges ordinary values. Every piece the walk writes is at least a
  byte, so a bound on the bytes is a bound on the work however much of the
  value is shared, a long string is checked against the budget before it is
  escaped (escaping can write six bytes for each), the walk stops with HS1004
  within one piece of the budget, and it asks the host whether to cancel every
  1,024 pieces, so a deadline reaches it as it reaches a run. A capability
  call's argument charge (section 11.3) counts each shared object once, so it
  does not bound the writing, which is why the writing carries its own bound.
  The budget is its own allowance for each writing and is not shared with what
  `print` wrote. Before this, a six-line program made a Node process allocate
  about 3 GB in about 2.5 seconds and trap, under a 32 MiB memory limit and a
  5 second deadline, through `resultFormat: "json"`.
- **Arithmetic is checked.** Overflow checks are on in every profile the
  tests and the fuzzers use, and the language's own integer arithmetic is
  written with checked and saturating operations, so an overflow is HS1007
  rather than a wrap.
- **Bytecode is verified, not trusted.** A VM runs only a `VerifiedModule`,
  and the only way to make one is `verify`. That is what lets the browser
  build contain the VM and no compiler: it accepts bytecode from wherever
  and refuses what does not hold to the rules before an instruction of it
  runs. Capability, namespace function and export names are checked to be
  distinct identifiers (a name with a quote in it once shifted the handlers a
  host bound, through the JSON the WebAssembly build wrote them into). Every index the module carries is range-checked, the ones inside
  its tables and constants as well as the ones instructions name (a string
  constant naming a string the table lacks once made the interpreter size a
  cache to four billion entries before any instruction ran), and a seeded
  mutation test refuses or runs every container it corrupts
  (`hollowscript/tests/mutated_bytecode.rs`). Rule 3 (every cycle of the control flow charges steps) is checked
  per edge, not per instruction: `IterNext` charges on only one of its two
  outgoing edges (there is another element to visit), never on the other
  (the collection is exhausted), so a loop built entirely from its uncharged
  exit edge is refused rather than accepted and left to run uncharged and
  uncancellable. `SetCell a, a`, which would make a cell hold a handle to
  itself directly, is refused outright. Whether an exported or a capability
  type is transferable (rule 4) is computed once for a module's whole type
  table, not walked afresh from every export's own root, and is correct
  over a cycle in the type table whether or not it passes through an
  `Enum`.
- **The module loader cannot be steered outside the program's root.** The
  CLI host resolves every import through a `cap_std` handle opened once on
  the root directory (`crates/hollowscript-cli/src/loader.rs`), so a path
  the resolver hands it is read, or refused, in one step: no symbolic link
  is ever followed, at any level of the path, whether it would have stayed
  inside the root or left it, and a segment is accepted only when it matches
  a real directory entry byte for byte, so a case-insensitive file system
  cannot hand back a module under a name that differs only in case
  ([section 10.3](../spec/10-modules.md#103-import-paths-and-canonical-paths)).
  The walk and the read are in `hollowscript-hostfs`, which the language
  server uses too, so the two cannot disagree. The module is opened without
  waiting (`O_NONBLOCK`), so a named pipe is refused at once (HS0607) and not
  waited on for ever; what was opened must be a regular file, judged from the
  open handle and not from a second look at the name; a file with more than
  one hard link, or whose link count is unavailable, is refused (HS0607),
  since a name inside the root can be
  the same file as one outside it; and the file is read through a limit,
  never in full: at most the language's own limit for one module (4,194,304
  bytes, HS0002) and for a whole program (67,108,864, HS0612), plus one
  byte, so the front end sees it is too large and says so without the host
  having held it in memory.
- **`fs.read` cannot be made to buffer more than its allowance, or to hang on
  a FIFO, and nor can `fs.write` or `fs.append`, or `stdin`.** The CLI host `fstat`s a granted file before reading it
  (`crates/hollowscript-cli/src/capabilities.rs`): a directory, a FIFO, a
  device or a socket is refused outright rather than read from, and a file
  larger than what is left of `maxMemoryBytes` faults with HS1002 before any
  of its content is read, not after it has already been buffered
  ([section 16.8](../spec/16-standard-capabilities.md#168-fs)). That check
  is repeated on the handle the open returns, because the name may have
  changed kind in between, and every `fs` open is made without waiting on a
  named pipe: `fs.write` and `fs.append` used to block for ever in the open of
  a FIFO with no reader, and now refuse it (`Invalid` once it has been opened
  and found not to be a regular file, `Failed` if the open itself fails
  because there is no reader). A file is truncated by `fs.write` only after the
  open has been checked. `stdin.readAll` and `stdin.readLine` read through
  what is left of `maxMemoryBytes` and raise HS1002 once more than that has
  been read, instead of buffering all of the input first and charging
  afterwards (700 MB piped in used to give a resident size of 705 MB against a
  64 MiB limit).
- **A file with more than one hard link is refused by `fs` and the module
  loader.** A hard link inside a granted root and a file outside it
  can be one file, and a handle opened inside the root cannot tell; reading or
  writing the one reads or writes the other. `fs.read`, `fs.write` and
  `fs.append` answer `Denied` for a regular file whose link count (read from
  the open handle) is above one, and the loader refuses it. This is a
  deliberate trade, not a precise test: a file that is legitimately hard
  linked (a deduplicating store, say) is refused as well, and a program cannot
  create a link, so only a file someone else linked is affected. On Unix the
  count is `st_nlink` from `fstat`; on Windows it is the handle's
  `nNumberOfLinks` (`GetFileInformationByHandle`, through `cap-primitives`),
  and a count the platform does not report is refused rather than assumed
  to be one. The Windows check is implemented and unit tested but has not
  yet passed native Windows acceptance.
- **`http`'s address safety covers the full IANA special-purpose registries,
  not only loopback, private, link-local and multicast.** The CLI host
  refuses the unspecified address, CGNAT, cloud metadata addresses,
  benchmarking and documentation ranges, and the rest of the reserved block,
  decoding an IPv6 address that embeds one of these in IPv4-mapped,
  IPv4-compatible, NAT64, 6to4 or Teredo form before it checks
  (`crates/hollowscript-cli/src/http.rs`). A program cannot set its own
  `Host`, `Content-Length`, `Transfer-Encoding` or `Connection` header; the
  literal-address exception applies only to the exact scope and port that
  authorised the request, never to an unrelated grant naming the same
  address on a different port or to whatever a granted name happens to
  resolve to; one deadline covers a request's DNS lookup and every redirect
  hop, rather than each restarting the same `total_time`; and a credential
  in a request's own headers is dropped before a redirect to a different
  origin is followed ([section 16.9](../spec/16-standard-capabilities.md#169-http)).
  More precisely: on a redirect to another origin every header the program
  supplied is dropped except `Accept`, `Accept-Language`, `Cache-Control`,
  `Content-Type`, `If-Modified-Since`, `If-None-Match`, `Pragma` and
  `User-Agent` (an allowlist, since a list of the names that carry secrets,
  `Cookie` and `X-Api-Key` and the rest, is always one name short; only
  `Authorization` was dropped before). Also refused in a program's headers
  are the hop-by-hop and proxy ones (`Upgrade`, `TE`, `Trailer`, `Expect`,
  `Keep-Alive`, `Proxy-Connection`, `Proxy-Authorization`), the headers a
  reverse proxy sets to say who asked (`Forwarded`, `X-Forwarded-*`,
  `X-Real-IP`), the ones that rewrite what was asked (`X-Original-URL`,
  `X-Rewrite-URL`, the method overrides), and the framing ones above. The
  body limit counts the *decoded* body: a gzip response used to be limited
  only by its compressed size, so a few hundred kilobytes decoded to hundreds
  of megabytes; it is read through the decoder, a chunk at a time, and refused
  one byte past the limit. The address refusal list also covers
  `::ffff:0:0:0/96` (IPv4-translated, with its embedded address),
  `192.88.99.0/24`, `3fff::/20`, `2001:2::/48`, `2001:10::/28` and
  `5f00::/16`.
- **A host is held to what it declared.** A capability's result is checked
  against the type its interface declared (HS1013), and a host's arguments
  to an exported call against the parameter types (HS1202), so a host defect
  cannot become type confusion inside a script.
- **The front end takes time polynomial in the source, and two places that did
  not are fixed.** Section 19.1's "never hangs" covers checking and formatting
  as much as running. Probing every construct for the growth of its time with
  its nesting found two that doubled at each level: the formatter tried an arm
  that holds one statement on one line by printing it, and printed it again for
  real if it did not fit, so nested `match` arms took 12 seconds at 24 levels;
  and the checker checked the arguments of an overloaded numeric call (`abs`,
  `min`, `max`, `clamp`) once to choose a signature and once to use it, so
  `abs(abs(abs(...)))` took a second at 22 levels. Each now keeps the answer to
  the first visit. A regression test for each runs the construct at a depth the
  language allows, on a thread with a deadline.
- **A source cannot make the diagnostics themselves a way to exhaust the
  host.** Section 17.1 reports at most 100 diagnostics for a file, and the cap
  is applied as each is found, in the lexer, the parser, the resolver and the
  type checker as well as in the final set, which holds the smallest 101 of
  each file in the canonical order (`BoundedDiagnostics`, see
  [diagnostics.md](diagnostics.md)). A 3.9 MiB file of `"` and a line feed used
  to collect four million diagnostics, 3.7 GB, before capping them; it now
  holds a few hundred. What remains is not the diagnostics: the syntax tree is
  about 0.7 KiB for each statement, so a file of 2,000,000 two-byte statements
  still takes about 2 GB to check (and the browser build keeps the memory it
  grew to, 740 MiB for a million lines). That is linear in the source, and the
  structural limits bound a source at 4 MiB for a file and 64 MiB in all; a host
  that checks source it does not trust should bound it tighter than that.

- **Fuzzed end to end.** The lexer, the parser and the formatter have their
  own seeded fuzz tests, and `hollowscript`'s pipeline fuzz test feeds
  mutated corpus programs through check, compile, verify, load and run, and
  then through an instance's own surface. It checks that nothing panics and
  that a program the checker accepts compiles, verifies and loads. Seeded,
  so a failure is reproducible without the fuzzer.

## What it deliberately does not promise

- **Timing.** Nothing here is constant time. A program can measure how long
  its own work takes, and a host that runs untrusted programs beside secret
  data should assume they can be distinguished by timing.
- **Memory as the operating system counts it.** The limits count what the
  language's own size formulas say, not the host's real allocation
  ([section 11.4](../spec/11-resource-limits.md)). An implementation may use
  more or less real memory than the counters state; the counters exist so
  that programs behave identically everywhere, not to bound the process.
  **In the WebAssembly build this is bounded by something that is not the
  counter.** The module imports its linear memory with a declared maximum of
  256 MiB (`crates/hollowscript-wasm/build.rs`, a linker argument for the
  `cdylib`; `HOLLOWSCRIPT_WASM_MAX_MEMORY_MIB` changes it for one build). A
  host cannot supply a memory with a larger maximum, and the wrapper makes
  one with a lower maximum when its `maxMemoryBytes` option asks for it, so a
  program that allocates for real stops there whatever the logical limit
  says: a loop that doubles a list under a logical limit of 2^40 bytes ended at
  130 MiB of a 256 MiB ceiling (the allocation that would pass it failed) and at
  34 MiB of a 64 MiB one, in 20 ms and 5 ms. A failed allocation aborts Rust and
  an abort is a trap, so the module is gone afterwards, which is defined and
  enforced (`HollowScriptTrap`, `web/README.md` "Memory"), not a fault of
  the program. The same build script links the `tooling` build with its 16 MiB
  stack (above), and that stack is in this memory, so that module declares a
  minimum of 258 pages (16.1 MiB) beside the same maximum: a lower
  `maxMemoryBytes` than it needs to start is refused by the wrapper with a
  `RangeError` that says what the module needs, and a ceiling with no
  headroom above that ends the first call that allocates in the same trap.
  The ceiling is not a substitute for the limits: at the default
  logical limit of 64 MiB the best case (a list of scalars) is 245 MiB of the
  256, so a program can reach the ceiling without reaching a limit, and a host
  that sizes for untrusted programs sets a logical limit near a quarter of its
  ceiling (the figures are in `web/README.md`). The writers that hand a value
  to a host were also making a step for every element of a container before
  writing any of them, which put 600,000 pairs at 360 MiB as JSON and 296 MiB
  as text against 199 MiB for the value itself; they now keep a position in each
  container, and both are 199 MiB. A host's own conversion of the text it is
  given (`JSON.parse`) is outside the module.

  A host that nonetheless wants `maxMemoryBytes` as a rough sizing proxy
  (deciding how much real memory to reserve per instance, say) should not
  plan for a single floor: the real factor measured here runs from about
  4x, for the best case, to 12.7x for a loop appending empty strings, and
  grows with how the value was built as well as with what it is made of,
  not a fixed multiple of the logical count either way. `cargo xtask mem`
  (`xtask/src/mem.rs`) reproduces every figure below on demand, reading
  `/usr/bin/time -l`'s "peak memory footprint" on macOS against a charged
  figure it computes independently from section 11.4's own formula; three
  shapes, all measured this way, give the range:
  - **A list of scalars, built by one native call.** `range(0, 8000000)`,
    an 8,000,000-element list of `Int`s, measured at 257 MB of real resident
    memory against a 64.0 MB charge: 4.0x. This is close to a best case:
    `Value` is 24 bytes on a 64-bit host against the 8 bytes per list, map,
    set or record element section 11.4's own formula charges, and building
    the list with a single call leaves nothing else to account for.
  - **A list of strings, built by a loop of appends.** A function that
    appends an interpolated empty string to a list 100,000 times (forcing
    a fresh `String` value each time rather than reusing one constant)
    measures 12.7x against a 2.4 MB logical charge. A second shape,
    appending an interpolated 1,000-character string 100,000 times,
    measures 1.3x against 102.4 MB logical. These 2026-10-06 results follow
    the VM's alias-checked in-place append change; before it the same shapes
    measured 99x and 15x in `docs/design/performance.md`. The empty-string
    ratio that remains is largely the VM's per-object, arena-slot and
    allocator overhead, not quadratic copies of prior list generations.
  - **A list of scalars, built the same way, once fixed.** The same
    loop-of-appends shape, but for `Int`s rather than `String`s
    (`docs/design/performance.md`'s `list-sort` benchmark, 5,000 elements):
    30 MB resident for a few-hundred-byte retained value, which is this
    section's own roughly-3x-to-4x representation overhead on a tiny
    logical size, not the unbounded case the next paragraph describes.

  **How `list.append` preserves immutable values.** When the receiver has
  no other live register or global root, no heap object or host boundary
  refers to it, and the result immediately replaces its binding, the VM
  grows the existing vector geometrically. Otherwise it copies the list to
  keep the old value observable, as section 6 requires. The direct alias,
  nested-container and host-argument regressions are covered by
  `crates/hollowscript/tests/list_append.rs`; a bytecode regression checks
  repeated global rebinding reuses one list object. The 100,000-element
  measurement in `docs/design/performance.md` confirms that the common
  unaliased loop no longer stores or copies every prior list generation.
  The section 11.4 charge remains unchanged at 8 bytes per added element.
  The measured 12.7x factor for empty strings is still a reason for hosts
  running untrusted programs to impose an OS or container memory limit.

  Historically, before the copy-on-write change, a script that built an
  n-element collection through n sequential calls
  to an immutable collection's own mutating-looking operation (`list.append`
  and its kin; section 6 and 14.1) inside one call, with no host call in
  between to create a safe point for collection, used to retain every
  discarded intermediate generation until that call returned, which grew
  with n, not as a fixed multiple of it: measured before the fix below,
  building and discarding 4,999 intermediate lists this way before reaching
  a final 5,000-element one (the same `list-sort` benchmark) held 348 MB
  resident for a value whose own retained size is a few hundred bytes,
  where building the same final list with one native call (`range(0,
  5000)`) held 3 MB.

  The collector now runs mid-call as well as between calls
  (`docs/design/performance.md`, "mid-run collection"): every frame's
  registers, a running closure's captures, a library function's own job
  state, a suspended run's frames and every value the host still holds are
  all rooted, so a collection at a safepoint inside a `while` or `for` loop
  can no longer free something still reachable, and `list-sort`'s 348 MB
  is now the 30 MB above (still well above its few-hundred-byte retained
  size, for the representation-overhead reason the list above gives, not
  for this one's reason any more). This does not remove the general advice
  below, only the specific unbounded-with-n case for a value whose own
  logical size is small: the two measured String shapes above show that a
  loop which keeps what it builds can still carry a real factor an order of
  magnitude past the best case, because a collection this often still lets
  each iteration's short-lived intermediate values (the interpolated
  `String` itself, among others) sit uncollected until the next safepoint
  finds them garbage, and a host sizing memory for a script it does not
  control should budget headroom on that basis rather than assume the
  logical limit is close to the real one, but "grows with how many times
  the loop ran, not with what it kept" is no longer a
  separate, larger gap on top of that.
- **Legitimate hard links.** See the hard-link bullet above: a harmless file
  with two names is refused as well, on every platform. The Windows check
  still needs native acceptance before Windows support is claimed.
- **A named pipe between the check and the open, on Windows.** The
  non-blocking open and the check on the open handle are Unix measures; on
  Windows the host `stat`s a name before opening it and refuses a non-regular
  file, which leaves the gap between the two that Unix no longer has.
- **Wall-clock time.** Not a language limit. A host that needs one uses
  cancellation ([section 11.7](../spec/11-resource-limits.md)), which every
  entry point takes a token for.
- **`http`'s internationalised host names and numeric host detection.**
  Section 16.9 asks for a scope value and a request host to be compared in
  their `xn--` (Punycode) form; this implementation compares lower-cased
  ASCII text only, with no IDNA normalisation. Its check for a disguised
  numeric host in a non-canonical form covers the specification's own named
  examples but not every historical `inet_aton` acceptance rule. Both are
  recorded, with what they do and do not catch, in
  `crates/hollowscript-cli/src/http.rs`'s own module documentation.

## The release channel

The installers and the release scripts are part of what a user trusts, and
`docs/design/release.md` is their design. What it promises, and what it does
not, in one place:

- **An archive is installed only if it was signed for that version by the
  pinned key.** `SHA256SUMS` is signed (minisign, and in the SSH signature
  format for machines with OpenSSH but no minisign, both by the same key) and
  the installers verify it before anything else is trusted. They refuse to
  continue on a machine that has no verifier, unless the user sets
  `HOLLOWSCRIPT_ALLOW_UNSIGNED=1`, and never when the only downloader is one
  that does not validate certificates.
- **The key and the floor come from the site that serves the installers, not
  from the bucket the releases are in.** Write access to the bucket lets
  someone publish bytes, but not a signature the installers accept, and not
  lower the oldest release a default install will take (the release floor).
- **What is not promised.** `latest.txt` is not signed. Anyone who can write
  to the bucket can still hold a default install at an older release that is
  at or above the floor, and keep it there (a freeze), because an old
  `latest.txt` can be served again; the floor limits that to releases the
  project has not marked as dangerous by raising it. Whoever controls the site
  that serves the installers controls the key and the floor with it. Closing
  the freeze needs a signed `latest.txt` with an expiry, which needs a
  scheduled signing: recorded as a follow-up in `release.md`, not built.
  Windows PowerShell could not be exercised here: see the notes in
  `release.md` on what was and was not run.

## Review status and boundaries

An independent security review of an earlier preview revision was completed
on 2026-09-23. It found issues across the verifier, resource accounting,
capability boundary, filesystem and HTTP hosts, and the C and WebAssembly
interfaces. The findings register records fixes and regression coverage for
the reported critical and high severity issues. The review was not a
certification, and the post-fix source has not had a second external review.
The current source is therefore a preview, not a security-certified runtime.

The list below orders the areas on which the most depends when reviewing a
change or assessing a host integration. The measured real-memory amplification
from repeated immutable list appends described above remains a known runtime
limitation. A host that runs untrusted programs should set conservative
language limits and use process or container limits where it needs a hard
operating-system memory bound.

1. **The verifier** (`hollowscript-bytecode::verify`). Everything the browser
   VM trusts rests on it: that every index, register, jump target and run of
   registers is in range, that execution cannot fall off the end of a
   function, and that every cycle of the control flow passes through an
   instruction that charges steps. A gap here turns malformed bytecode into
   an interpreter reading what it should not. The verifier's own stated limit
   is that it does not re-check the type system, which values carry at run
   time instead; a reviewer should confirm that division of work holds for
   every opcode.
2. **The two raw-pointer boundaries** (`hollowscript-capi`,
   `hollowscript-wasm::abi`). The only `unsafe` in the workspace, and the
   only places a caller's mistake can become memory unsafety rather than a
   refusal. Both take handles from a caller and refuse one they never gave
   out, or one already released: an instance handle is an identifier the
   library looks up in its own table, never a pointer it follows, so a
   double dispose or a call after dispose is a status, not a use after free.
   What remains a caller's obligation is the ordinary one of passing valid
   string and output pointers.

   Both now also let a host bind a capability function to a running program
   (`hollowscript-capi`'s `hs_load_ex`, `hollowscript-wasm`'s equivalent) and
   supply a cancellation token a second thread may set. This adds two more
   ways for a host's own mistake to reach the boundary, neither of them
   memory unsafety: a capability callback that calls back into the instance
   it was invoked for (directly, or by calling `hs_call`/`hs_call_ex` on the
   same handle from inside the callback) is refused as HS1204/`Busy`, by the
   same "identifier, not a pointer" instance registry that already refuses a
   stale handle, rather than aliasing the running call; and cancelling a
   token from another thread is the one operation the specification allows
   to run concurrently with the call it was passed to (section 12.5), so it
   is the only place in either boundary where two threads are expected to
   touch the same state at once, and is reviewed as such (an `AtomicBool`
   behind an `Arc`, set with `Release` and read with `Acquire`, in
   `hollowscript-core::cancel`).
3. **The resource accounting** (`hollowscript-vm::limits` and every charge
   site). A charge that is missed, or applied after the work rather than
   before it, is a way to make a run cost more than its limits allow. The
   slice rule of section 11.3 (no single charge above 1,024, with a safepoint
   between slices) is what bounds the work between two cancellation checks.
4. **The capability boundary** (`hollowscript-vm::capability`,
   `hollowscript-vm::accept`, `hollowscript::hosttypes`). What a host may
   hand in and what it must be held to, including the acceptance rule that
   stops a host defect becoming type confusion. `hollowscript-capi` and
   `hollowscript-wasm` each add a thin marshalling layer on top of this (a C
   function pointer plus user data; a JS function reference), which turns a
   `HostFn`'s arguments and result into JSON on the way across the
   language boundary and back, the same way `hs_call`'s own arguments and
   results already do; the acceptance rule itself still runs, unchanged,
   inside the VM on whatever value that layer builds, so a malformed
   capability answer is HS1013 there, not a new failure mode this layer has
   to reproduce.
5. **The `fs` capability's scope check** (`hollowscript-cli::capabilities`):
   whether the `cap_std` handle for a granted root can, in fact, be walked
   outside it, on every platform this is shipped for.
6. **The `http` capability's address safety** (`hollowscript-cli::http`,
   `SafeResolver`): whether a name really is resolved once and connected to
   only that resolved address, on every platform, and whether the loopback,
   private, link-local, multicast and IPv4-mapped classification is
   complete. This uses `ureq`'s own `unversioned` API, which is worth an
   extra look on its own terms (see `docs/design/dependencies.md`).
7. **The checker**, last, and for a different reason: a defect there is a
   program accepted that should not have been, which is a correctness
   problem and only a safety one where the VM relies on the checker rather
   than checking for itself. The VM is written not to.

## Reporting

See [SECURITY.md](../../SECURITY.md) in the repository root.
