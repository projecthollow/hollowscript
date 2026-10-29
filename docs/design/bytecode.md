# Bytecode: instruction set, container, and the verifier

The specification deliberately says nothing about bytecode (its README lists
this among what it does not define), so every choice in this document is an
implementation decision, not a reading of the contract, and none of it is a
compatibility promise across versions.

## Register, not stack

The machine is register based, in the tradition of Lua's own bytecode. Each
function has a frame of at most 65,535 registers (`MAX_REGISTERS`); an
instruction reads its operands from registers and writes its result to one,
rather than pushing and popping a stack. This was chosen over a stack machine
for two reasons that matter for this language specifically: the interpreter
loop has fewer memory accesses per operation (a stack machine spends many
instructions just moving values into position), and it maps directly onto
the HIR's own local variables, so the compiler's register allocator is close
to the identity function for straight-line code (a local gets one register
for its whole scope) rather than a separate stack-slot assignment problem.

The compiler allocates everything above the named locals like a stack: an
expression's value lands in the first free register, and every register its
subexpressions used above that is free again as soon as the instruction that
reads them is emitted. A binary chain of any length therefore needs two
temporaries, and a frame grows with how deeply an expression nests and how
wide one run of values is, not with how long an expression or a function is.
Almost every frame stays under 256 registers.

Section 1.10 bounds how many elements, arguments, fields and statements one
construct has only by the size of a source file, so the frame is not capped
at what one operand byte names. The first 256 registers are named directly,
and the rest through the `Ext` prefix below.

## Instructions

An instruction is one 32-bit little-endian word: an 8-bit opcode and three
operand bytes, read in one of four formats:

- `Abc`: three 8-bit operands.
- `Abx`: an 8-bit operand and a 16-bit unsigned operand, for a constant index,
  a table index, or a count.
- `AsBx`: an 8-bit operand and a 16-bit signed operand, for a short
  jump, relative to the instruction that follows it, or a small `Int`.
- `Ax`: one 24-bit signed operand, used only by the long unconditional jump
  (`JumpLong`), which reaches plus or minus about 8.4 million instructions.

**Wider operands.** An operand that does not fit is widened by an `Ext` word
placed directly before the instruction. `Ext` is not an instruction of its
own: its three bytes are the high bytes of the next word's operands, bits 8
to 15 of `a`, `b` and `c` for `Abc`, bits 8 to 15 of `a` and bits 16 to 31 of
`bx` for `Abx`, and bits 8 to 15 of `a` alone for `AsBx` (a signed offset is
never widened: a jump that needs more is a `JumpLong`, and a number that
needs more is a constant). An `Ax` word takes no prefix. The pair is one
instruction: `opcode::Wide` is the instruction with its prefix applied, which
is what the verifier checks and the interpreter runs, a jump lands on the
prefix and never on the word after it, and an offset is relative to the word
after the pair. So a register, an argument count, a field position or a
payload position reaches 65,535, and a constant, function, global, shape or
variant index reaches 2^32 - 1, in the one encoding every instruction
already has, with no second form of any opcode. An instruction that needs no
prefix, which is nearly every one, costs one word and one decode as before.

**Long jumps.** The compiler records a jump by the instruction it goes to and
lays a function out only once the whole of it is built. Each jump takes its
short form when its offset fits 16 bits and a long form otherwise: `Jump`
becomes one `JumpLong`; a conditional jump becomes the opposite test jumping
over the next word, followed by a `JumpLong`; and `IterNext`, which jumps
when its collection is exhausted, becomes an `IterNext` over the next word, a
`Jump` over the word after it (taken while there are elements), and a
`JumpLong` to the loop's end. Making one jump long moves the code after it,
which can put another out of reach, so the choice is repeated until nothing
changes; a jump only ever goes from short to long, so this ends, in one pass
for almost every function.

Every opcode states, as data rather than as scattered pattern matches, what
each operand slot refers to (`Operand::Reg`, `Const`, `Func`, `Native`,
`Cap`, `Shape`, `Variant`, `Global`, `Upval`, `Jump`, `Imm` or `None`). The
verifier reads this table to check every operand of every instruction
generically, so adding an instruction never means adding a new case to the
verifier's range checks by hand; only instructions whose operands interact in
a way the generic table cannot express (a run of registers starting at one
operand and continuing for a count given by another, as `NewList` and a call
both need) get their own rule, listed explicitly in `verify_instruction`.

Steps are not counted per instruction. The specification's step count
(section 11.3) is defined over source-level constructs (statements,
iterations, calls), and the compiler is responsible for reproducing that
count exactly by emitting explicit `Charge` instructions, so that the number
of steps a program takes is a property of the specification, not of how many
bytecode instructions the compiler happened to emit for one statement.

## Literals longer than one run of registers

A list literal is built a chunk of 64 elements at a time: `NewList` of the
first chunk, then `ListAppend`s of the rest into the same list, each chunk's
values evaluated, in order, into the same registers again. A map literal is
built the same way, 128 pairs at a time, with `NewMap` and `MapPut`. So a
literal of any length needs one chunk of registers, and a literal nested as
deep as section 1.10 allows still fits a frame.

`ListAppend` and `MapPut` extend their list or map in place. They exist only
to finish a literal whose value no other register or value can see yet, so
nothing can observe the change, and in place is what keeps a long literal
linear: a copy per chunk made building one quadratic in its length. They
charge what the one literal would have: `ListAppend` 8 bytes for each element
and no steps, since a literal is not a call (section 11.3), and `MapPut` 24
bytes for each key that is new, so a literal built in chunks is charged
exactly `16 + 8n` or `32 + 24n` bytes, as section 11.4 says for one literal.
The work of `ListAppend` is the values it copies in, at most 65,535, so it
needs no step charge to stay cancellable: any loop around it charges.

A map is kept sorted by key, though, so a `MapPut` whose keys sort before
entries already in the map moves those entries to make room, work that grows
with the map and not with the chunk. It is charged one step for every 64
entries it moves, before it moves any, which bounds a loop that keeps
inserting at the front of one large map by its steps. A literal whose keys
ascend from one chunk to the next, and every literal of up to 128 pairs,
moves nothing and is charged no steps, as the specification's literal is. A
literal of more than 128 pairs whose later chunks sort before its earlier
ones is the one place a literal is charged steps: at most one step for every
64 entries each later chunk moves. That is a known difference from section
11.3, which charges a literal nothing, kept because the alternative is work
proportional to the map, unbounded by any step, on every such instruction.

An interpolation is not chunked: `Interp` reads every part at once, so the
string is charged once (section 11.4: the final string only). Its parts take
`2n + 1` registers for `n` values, plus one register to compute each value in
before it is moved between its text segments.

## This implementation's limits

Section 1.10 lets an implementation impose limits of its own, provided it
documents them, none of them affects a program of the conformance corpus,
and exceeding one is reported with HS0901 before the program runs or HS1015
while it runs, never as an internal error. These are this implementation's:

- **Registers in one function: 65,535.** A function whose frame would need
  more than 65,535 registers at once is rejected with HS0901 when it is
  compiled. This happens for an interpolation of more than 32,766 values, a
  call of that many arguments, a record of that many fields, or that many
  locals. The diagnostic is located at the construct that went over the
  limit, or at the function's declaration for its locals.
- **Code in one function: a jump of 8,388,607 instructions.** A function
  whose code is too long for a `JumpLong` to cross is rejected with HS0901,
  located at the function.
- **Registers held by the active calls of one run.** A frame's registers are
  real memory that no counter of section 11.4 charges, and a frame may hold
  65,535 of them, so the interpreter also bounds the registers every script
  call of one run holds between them. The bound is 256 for each level of
  `maxCallDepth` plus 256 for the frame the run starts in, which is exactly
  what frames could hold when a frame held at most 256, and never less than
  4,194,304 (128 MiB of values). A program whose frames are no wider than
  256 registers reaches its call depth limit as it always did. One that
  recurses through very wide frames faults sooner, with HS1015 at the call
  that went over.

## Values that do not fit in an operand

A function's constant pool holds its `Int`, `Float`, `String` and `Filter`
literals; `LoadK` loads one by index. Everything else that code refers to
(other functions, native library functions, capability calls, record shapes,
enum variants, global slots, and a closure's upvalue list) is a table on the
module, indexed the same way, so the container format and the verifier have
one pattern for "an index into a table" repeated for every kind of table
rather than a special case per kind.

## The container format

A compiled program is `hollowscript_bytecode::module::Module` in memory and a
flat byte container (`container.rs`) on disk or over the wire. The header is
fixed size and always the first thing read:

```text
offset  size  field
     0     4  magic, the bytes HSBC
     4     2  format major, little endian
     6     2  format minor
     8     2  language major (the specification version the program targets)
    10     2  language minor
    12     4  flags (bit 0: the module carries source positions)
    16     8  payload length in bytes
    24    32  SHA-256 of bytes 0 to 23 of the header, followed by the payload
    56     n  payload
```

The digest covers the version and length fields as well as the payload, so a
corrupted or truncated header is caught by the same check as a corrupted
payload, not left to a separate ad hoc comparison. A reader accepts only the
exact format version it was built for and refuses every other: bytecode is
not a stability promise, so the format is free to change in any release, and
a version mismatch is reported plainly (`ContainerError::UnsupportedVersion`)
rather than an attempt made to read a format that might have moved fields
around. Format 0.2 widened a function's parameter count, a capture's
register or upvalue index and a variant's payload count from one byte to
two, with the frame, and added `Ext`; a 0.1 container is refused.

Every count inside the payload (how many strings, how many functions, how
many instructions) is checked against both an absolute limit and the number
of bytes actually remaining in the buffer before anything is allocated for
it, so a container that claims four billion strings is rejected in constant
time rather than causing an attempted allocation of that size.

A container built with debug information carries a `SpanEntry` table per
function, mapping instruction ranges back to source positions in a named
module, for fault frames and, eventually, a debugger.
`Module::stripped` removes this table, which is how the smaller container
for a published game (no source positions needed once verified) is built:
by producing the same bytecode and then stripping it, not by compiling
twice.

## The verifier

`hollowscript_bytecode::verify::verify` is the only way to produce a
`VerifiedModule`, and only a `VerifiedModule` can be given to
`hollowscript_vm::Interpreter::new`. This is what "the browser VM accepts
only verified bytecode" means concretely: there is no code path from bytes to
a running interpreter that skips this function, whether the bytes came from
this workspace's own compiler moments earlier or were read from a file
somewhere else.

Because every crate in this workspace is safe Rust (`#![forbid(unsafe_code)]`
everywhere except the two crates that need a foreign boundary), bytecode can
never make the VM unsafe in the memory sense, whatever it contains: an
out-of-range register access in unverified bytecode would be caught by a
bounds check and turn into a logic bug or an internal fault, never undefined
behaviour. What the verifier guarantees instead is what the rest of the VM is
allowed to assume without checking again on every instruction:

1. **Every index is in range**: every register, constant, jump target, and
   table index (function, native, capability call, shape, variant, global,
   upvalue) that an instruction names actually exists, checked against the
   generic operand table plus the small number of explicit rules for runs of
   registers, calls, and closures. The code is first split into
   instructions, each an `Ext` prefix and its word or a word alone: a prefix
   must be followed by a word it has room to widen (not another prefix, not
   `JumpLong`, and for `AsBx` only its `a`), every operand is checked at
   its widened value, and a jump must land on the first word of an
   instruction, never inside a prefixed pair. The same holds for every index
   the tables themselves carry, not only for the ones instructions name:
   every string id (a function's name, a record shape's fields, a variant's,
   native's, capability's, module's, global's and export's names, and the
   string or filter a constant holds) must be inside the string table, every
   type reference inside the type table, and a `Float` constant must be finite
   and not negative zero (section 3.4). The VM sizes nothing from an index it
   has not been given the right to trust, and `Interpreter`'s cache of string
   constants checks the id against the table again before it is grown.
2. **Execution cannot fall off the end of a function**: the last instruction
   of every function is a return, a trap, or an unconditional jump; nothing
   simply runs out of instructions.
3. **Every cycle charges steps**: the control flow graph is searched (an
   explicit depth-first search over the decoded instructions, not recursion,
   so a large function cannot exhaust the verifier's own stack) for a cycle
   that passes through no instruction that charges steps. Such a cycle would
   let a script loop forever without ever reaching a safepoint, defeating
   cancellation (section 11.7); the verifier refuses the whole module rather
   than let it run. This search is scoped to one function's own jumps; a
   recursive call cannot loop forever without a safepoint only because every
   call-family opcode (`Call`, `CallFn`, `CallNative`, `CallCap`) is itself
   one of the instructions that charges, an invariant `Op::charges`'s own doc
   comment and `opcode::tests::every_call_like_opcode_charges` both state
   explicitly, so that a future call-like opcode cannot silently fall outside
   it and reopen an uncancellable loop across function boundaries.
4. **Closures capture only what exists**: a closure's capture list names
   registers of its immediately enclosing frame or upvalues of its immediately
   enclosing closure, both checked against that frame's own size.
5. **Exports match their declared signature and are transferable**: an
   exported function's declared parameter count and return presence match
   the function it names, and every type reachable from an export's signature
   (including through an enum's payloads, visited once each so a recursive
   enum terminates) contains no `Filter`, matching section 12.4's rule that a
   host could not otherwise call or read it.
6. **Names are identifiers.** The name of a capability, the name of each
   function of a namespace capability, and the name of an export are each an
   identifier of the lexical grammar (section 1.5: an ASCII letter or `_`, then
   ASCII letters, digits and `_`), because each is written as one in a program
   (`uses clock`, `clock.now()`, an `export fn`), and they are distinct (the
   capabilities from each other, the functions of one namespace from each
   other, the exports from each other). The qualified form a host sees,
   `clock.now`, is made by joining a namespace's name and a function's name
   with one dot, so neither may contain one. A name with a quote, a backslash
   or a line feed in it shifts every capability a host binds after it when the
   names are written into JSON, and breaks the host's parser when they are
   not; a name with a dot or a space cannot be reached from source at all. The
   verifier does not know the language's keywords, which belong to the front
   end, and so does not refuse a keyword as a name: none can be written where
   a name is expected, so none is reachable.
7. **What this deliberately does not guarantee.** `GetField`, `GetPayload`,
   `ListGet` and `ListRest` each take an immediate index that names a
   position in a record's field vector, an enum's payload vector, or a
   list's element vector, a bound that depends on the *value* the
   instruction meets at run time (the record's shape, the enum's variant, or
   the list's own length), not on anything the module's tables fix in
   advance. No static check of the verifier's own kind can cover this: it is
   value-dependent, not structural. A well formed module, one a correct
   compiler produced, never emits one of these four opcodes without a preceding check that the
   index fits the value it is about to meet, but that is an invariant of the
   compiler's own code generation, not something this format or this
   function can enforce. The interpreter's implementation therefore carries
   an obligation the verifier does not: it must range-check the index
   against the live value's actual length or arity every time it executes
   one of these four opcodes, and fault (HS1099) rather than index directly.

The verifier's own tests build small hand-written modules for each rule
(a loop with and without a charge, a closure that reaches past its frame, an
export whose arity does not match its own function) rather than relying on
the compiler to produce a case that exercises the rule. That was originally
because the compiler did not exist; it stays that way deliberately, because
a rule tested only through the compiler is a rule that stops being tested
the moment the compiler stops emitting that shape.
