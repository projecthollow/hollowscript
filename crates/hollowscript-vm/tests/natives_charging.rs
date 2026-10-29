//! Regression tests for the outside security review's findings about the
//! standard library's string and list functions: several of them built
//! their whole result before charging for it, so the charge (and any
//! resulting fault) came after the memory was already spent rather than
//! before, and a few undercharged relative to the work they actually did.
//! Each test reproduces the shape of one finding directly through bytecode
//! (`CallNative`), described here by behaviour and specification section
//! rather than by the review's own identifiers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use hollowscript_bytecode::container::encode;
use hollowscript_bytecode::module::{
    Constant, Function, FunctionKind, Global, Module, ModuleEntry, NativeImport,
};
use hollowscript_bytecode::opcode::{Instruction, Op};
use hollowscript_bytecode::verify::verify_bytes;
use hollowscript_core::{Control, Limits};
use hollowscript_vm::{CapabilityTable, Interpreter};

const RET: Instruction = Instruction::abc(Op::ReturnNone, 0, 0, 0);

/// Registers one native import and returns its `CallNative` bx (its
/// position in `module.natives`, in the order imports are added, which is
/// what [`hollowscript_vm`]'s own import resolution preserves).
fn native(m: &mut Module, name: &str, arity: u8) -> u16 {
    let position = m.natives.len();
    let name = m.add_string(name).unwrap();
    m.natives.push(NativeImport { name, arity });
    u16::try_from(position).unwrap()
}

fn str_const(m: &mut Module, text: &str) -> Constant {
    Constant::Str(m.add_string(text).unwrap())
}

/// Attaches a module-init function to `m`, which already has whatever
/// string constants and native imports the test registered on it: unlike
/// [`with_init`]'s counterpart in `review_verifier.rs`, this must not build
/// a fresh `Module`, or those registrations would be lost.
fn finish(m: &mut Module, regs: u16, consts: Vec<Constant>, code: Vec<Instruction>) {
    let path = m.add_string("main.hws").unwrap();
    let name = m.add_string("f0").unwrap();
    let init = Function {
        name,
        kind: FunctionKind::ModuleInit,
        arity: 0,
        register_count: regs,
        returns_value: false,
        captures: vec![],
        constants: consts,
        code: code.into_iter().map(Instruction::encode).collect(),
        spans: vec![],
    };
    let index = u32::try_from(m.functions.len()).unwrap();
    m.functions.push(init);
    m.modules.push(ModuleEntry { path, init: index });
}

/// Runs `m` to completion under `limits`, with no cancellation, and returns
/// the fault's code as text (or "OK" if it ran to completion).
fn run(m: &Module, limits: Limits) -> String {
    let verified = verify_bytes(&encode(m)).expect("verifies");
    let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
    match vm.initialise(&limits, &mut Control::none()) {
        Ok(_) => "OK".to_owned(),
        Err(f) => format!("{:?}", f.code),
    }
}

/// `string.replace` used to charge only the bytes read (the receiver and
/// the target), not the bytes the result would actually hold, so a
/// replacement longer than the target could multiply the receiver many
/// times over before the eventual allocation was ever charged. A receiver
/// of 20,000 bytes, every byte a match, replaced with 500 bytes each, would
/// build a 10,000,000-byte string; a memory limit well below that (but
/// comfortably above the receiver, the target and the replacement on their
/// own) must refuse the call rather than build it.
#[test]
fn string_replace_is_refused_by_its_actual_output_size_not_its_input_size() {
    let mut m = Module::new();
    let receiver = str_const(&mut m, &"x".repeat(20_000));
    let target = str_const(&mut m, "x");
    let replacement = str_const(&mut m, &"y".repeat(500));
    let replace = native(&mut m, "string.replace", 3);
    let code = vec![
        Instruction::abx(Op::LoadK, 1, 0),
        Instruction::abx(Op::LoadK, 2, 1),
        Instruction::abx(Op::LoadK, 3, 2),
        Instruction::abx(Op::CallNative, 0, replace),
        RET,
    ];
    finish(&mut m, 4, vec![receiver, target, replacement], code);
    // Comfortably more than the 20,000-byte receiver plus the small target
    // and replacement, far less than the roughly 10,000,000-byte result.
    let limits = Limits {
        max_memory_bytes: 100_000,
        ..Limits::default()
    };
    assert_eq!(run(&m, limits), "MemoryLimit");

    // The same call succeeds under the default limits, so this is really
    // about the amplified case, not a call that could never work.
    assert_eq!(run(&m, Limits::default()), "OK");
}

/// A mutable global being rebound is the one root consumed by the append.
/// The compiler clears the result temporary after `SetGlobal`, so repeated
/// `xs = xs.append(...)` does not retain stale temporary aliases either.
#[test]
fn repeated_append_assignment_reuses_the_unique_global_list() {
    let mut m = Module::new();
    let empty = str_const(&mut m, "");
    let append = native(&mut m, "list.append", 2);
    let global_name = m.add_string("xs").unwrap();
    m.globals.push(Global {
        module: 0,
        name: global_name,
        mutable: true,
    });
    let mut code = vec![
        Instruction::abx(Op::NewList, 1, 0),
        Instruction::abx(Op::SetGlobal, 1, 0),
    ];
    for _ in 0..3 {
        code.extend([
            Instruction::abx(Op::GetGlobal, 1, 0),
            Instruction::abx(Op::LoadK, 2, 0),
            Instruction::abx(Op::CallNative, 0, append),
            Instruction::abx(Op::SetGlobal, 0, 0),
            Instruction::abc(Op::LoadNone, 0, 0, 0),
        ]);
    }
    code.push(RET);
    finish(&mut m, 3, vec![empty], code);
    let verified = verify_bytes(&encode(&m)).expect("verifies");
    let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
    vm.initialise(&Limits::default(), &mut Control::none())
        .expect("runs");
    assert_eq!(vm.heap().len(), 2, "one list and one empty string constant");
}

/// The fault above proves the call is refused at the right size, but not
/// that the refusal happens before the result is built: whatever charges
/// the built object's true size (`Heap::node_cost`) would refuse it
/// eventually either way. A wall-clock assertion would prove that
/// directly, but is not reliable in a shared, parallel test run (several
/// workstreams and `cargo test`'s own thread pool contend for the same
/// CPU), so this was instead confirmed by hand rather than encoded here:
/// with the size-before-build charge in this commit reverted, refusing a
/// receiver of 2,000,000 bytes replaced with 300 bytes each (a
/// 600,000,000-byte result) took on the order of 200 to 300 milliseconds,
/// against consistently well under one millisecond with it in place. This
/// test instead re-checks the same amplified case that timing was measured
/// against, so a future change to the charge's correctness (not only its
/// timing) is still caught.
#[test]
fn string_replace_amplified_two_hundred_times_is_still_refused_by_its_true_size() {
    let mut m = Module::new();
    let receiver = str_const(&mut m, &"x".repeat(2_000_000));
    let target = str_const(&mut m, "x");
    let replacement = str_const(&mut m, &"y".repeat(300));
    let replace = native(&mut m, "string.replace", 3);
    let code = vec![
        Instruction::abx(Op::LoadK, 1, 0),
        Instruction::abx(Op::LoadK, 2, 1),
        Instruction::abx(Op::LoadK, 3, 2),
        Instruction::abx(Op::CallNative, 0, replace),
        RET,
    ];
    finish(&mut m, 4, vec![receiver, target, replacement], code);
    assert_eq!(run(&m, Limits::default()), "MemoryLimit");
}

/// `list.join`'s charge used to be the bytes of the joined result alone,
/// with no floor on the number of elements read: joining many small or
/// empty pieces cost the same one step as joining none. Splitting
/// 5,000 copies of "x" apart at every "x" produces 5,001 empty strings, so
/// the joined result is empty either way (the old charge was `1`), but
/// `join` still has to read every element. With `max_steps` set above what
/// building and splitting the receiver cost on their own but below what a
/// further 5,001-element `join` now also costs, the call must be refused
/// rather than run to completion.
#[test]
fn list_join_is_charged_for_elements_read_not_only_bytes_written() {
    let mut m = Module::new();
    let piece = str_const(&mut m, "x");
    let count = 5_000i64;
    let separator = str_const(&mut m, "x");
    let empty = str_const(&mut m, "");
    let repeat = native(&mut m, "string.repeat", 2);
    let split = native(&mut m, "string.split", 2);
    let join = native(&mut m, "list.join", 2);
    let code = vec![
        // R1 = "x" repeated `count` times (a `count`-character string).
        Instruction::abx(Op::LoadK, 1, 0),
        Instruction::asbx(Op::LoadInt, 2, i16::try_from(count).unwrap()),
        Instruction::abx(Op::CallNative, 0, repeat),
        // R4 = R0.split("x") (`count + 1` empty strings).
        Instruction::abc(Op::Move, 4, 0, 0),
        Instruction::abx(Op::LoadK, 5, 1),
        Instruction::abx(Op::CallNative, 3, split),
        // R6 = R3, R7 = "" (the empty separator).
        Instruction::abc(Op::Move, 6, 3, 0),
        Instruction::abx(Op::LoadK, 7, 2),
        Instruction::abx(Op::CallNative, 5, join),
        RET,
    ];
    finish(&mut m, 8, vec![piece, separator, empty], code);
    // Building the `count`-character string and splitting it into
    // `count + 1` empty pieces costs roughly `count / 64` steps on their
    // own; a limit above that but below what joining `count + 1` further
    // elements now also costs isolates the join itself, whether or not it
    // was ever reached.
    let limits = Limits {
        max_steps: 200,
        ..Limits::default()
    };
    assert_eq!(run(&m, limits), "StepLimit");
    assert_eq!(run(&m, Limits::default()), "OK");
}

/// `string.chars` and `string.split` used to build the whole list of
/// pieces, and charge each piece's own memory only as it was allocated
/// onto the heap one at a time, after the (already resident) source list
/// of `String`s had been built: splitting a string into many single
/// characters multiplies it several times over in per-object overhead
/// alone (a `String` costs 16 bytes plus its own, however short, and the
/// list holding them another 16 plus 8 per element), so the true cost is
/// far more than the receiver's own byte length. Splitting a plain,
/// unremarkable 8,000-byte string into single characters must be refused
/// under a memory limit comfortably above 8,000 bytes but well below what
/// 8,000 separate one-byte strings, each with their own header, actually
/// cost.
#[test]
fn string_chars_is_charged_for_the_lists_own_overhead_not_only_the_receivers_bytes() {
    let mut m = Module::new();
    let receiver = str_const(&mut m, &"x".repeat(8_000));
    let chars = native(&mut m, "string.chars", 1);
    let code = vec![
        Instruction::abx(Op::LoadK, 1, 0),
        Instruction::abx(Op::CallNative, 0, chars),
        RET,
    ];
    finish(&mut m, 2, vec![receiver], code);
    // More than the 8,000-byte receiver alone, far less than 8,000
    // one-character strings at 17 bytes each plus the list holding them.
    let limits = Limits {
        max_memory_bytes: 16_000,
        ..Limits::default()
    };
    assert_eq!(run(&m, limits), "MemoryLimit");
    assert_eq!(run(&m, Limits::default()), "OK");
}
