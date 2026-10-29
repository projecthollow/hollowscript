//! Frames wider than one operand byte reaches, and the in-place
//! `ListAppend` and `MapPut` that build a literal too long for one
//! instruction: built by hand, as the compiler would emit them, so that each
//! property is pinned by the interpreter itself and not only through the
//! compiler.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use hollowscript_bytecode::container::encode;
use hollowscript_bytecode::module::{Constant, Function, FunctionKind, Module, ModuleEntry};
use hollowscript_bytecode::opcode::{Instruction, Op};
use hollowscript_bytecode::verify::verify_bytes;
use hollowscript_core::{Control, Limits};
use hollowscript_vm::{CapabilityTable, Interpreter};

/// A module whose initialiser charges one step and then runs `code`, with a
/// frame of `regs` registers. The one step makes every module need at least
/// one, so [`steps_needed`] measures what `code` adds to it.
fn with_init(
    regs: u16,
    consts: Vec<Constant>,
    code: Vec<(Option<Instruction>, Instruction)>,
) -> Module {
    let code: Vec<_> = std::iter::once(one(Instruction::abx(Op::Charge, 0, 1)))
        .chain(code)
        .collect();
    let mut m = Module::new();
    let path = m.add_string("main.hws").unwrap();
    let name = m.add_string("<module init>").unwrap();
    m.functions.push(Function {
        name,
        kind: FunctionKind::ModuleInit,
        arity: 0,
        register_count: regs,
        returns_value: false,
        captures: vec![],
        constants: consts,
        code: code
            .into_iter()
            .flat_map(|(ext, main)| ext.into_iter().chain([main]))
            .map(Instruction::encode)
            .collect(),
        spans: vec![],
    });
    m.modules.push(ModuleEntry { path, init: 0 });
    m
}

fn one(i: Instruction) -> (Option<Instruction>, Instruction) {
    (None, i)
}

/// Runs `m` and renders what its initialiser returned, or the fault's code.
fn run(m: &Module, limits: Limits) -> String {
    let verified = verify_bytes(&encode(m)).expect("verifies");
    let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
    match vm.initialise(&limits, &mut Control::none()) {
        Ok(Some(value)) => vm.render(value),
        Ok(None) => "none".to_owned(),
        Err(f) => format!("{:?}", f.code),
    }
}

fn limits(max_steps: u64, max_memory_bytes: u64) -> Limits {
    Limits {
        max_steps,
        max_memory_bytes,
        ..Limits::default()
    }
}

/// The smallest `maxSteps` under which `m` runs to the end.
fn steps_needed(m: &Module) -> u64 {
    (1..1_000)
        .find(|&s| !run(m, limits(s, 1 << 30)).contains("StepLimit"))
        .expect("finishes in under 1,000 steps")
}

/// The smallest `maxMemoryBytes` under which `m` runs to the end.
fn bytes_needed(m: &Module) -> u64 {
    let (mut low, mut high) = (0u64, 1u64 << 24);
    while low + 1 < high {
        let mid = low + (high - low) / 2;
        if run(m, limits(1_000_000, mid)).contains("MemoryLimit") {
            low = mid;
        } else {
            high = mid;
        }
    }
    high
}

#[test]
fn a_prefixed_instruction_reads_and_writes_registers_past_255() {
    let code = vec![
        Instruction::wide_asbx(Op::LoadInt, 300, 5),
        Instruction::wide_asbx(Op::LoadInt, 256, 7),
        Instruction::wide_abc(Op::AddInt, 399, 300, 256),
        Instruction::wide_abc(Op::Move, 1, 399, 0),
        Instruction::wide_abc(Op::Return, 1, 0, 0),
    ];
    assert_eq!(
        run(&with_init(400, vec![], code), Limits::default()),
        "Int(12)"
    );
}

/// A list literal of `n` elements, built as the compiler builds one: the
/// first `chunk` elements by `NewList`, the rest by `ListAppend`s of up to
/// `chunk` into the same list, every chunk's values in the same registers.
fn list_literal(n: u16, chunk: u16) -> Module {
    let mut code = Vec::new();
    let mut done = 0;
    while done < n {
        let count = chunk.min(n - done);
        for i in 0..count {
            let value = i16::try_from(done + i).unwrap();
            code.push(Instruction::wide_asbx(Op::LoadInt, 1 + i, value));
        }
        code.push(if done == 0 {
            Instruction::wide_abx(Op::NewList, 0, u32::from(count))
        } else {
            Instruction::wide_abc(Op::ListAppend, 0, 1, count)
        });
        done += count;
    }
    code.push(one(Instruction::abc(Op::Return, 0, 0, 0)));
    with_init(chunk + 1, vec![], code)
}

#[test]
fn list_append_extends_one_list_in_order() {
    let rendered = run(&list_literal(10, 4), Limits::default());
    let expected: Vec<String> = (0..10).map(|i| format!("Int({i})")).collect();
    assert_eq!(rendered, format!("[{}]", expected.join(", ")));
}

/// Section 11.4: a literal of `n` elements is charged `16 + 8n` bytes and,
/// being no call, no steps, however many instructions build it.
#[test]
fn a_literal_built_in_chunks_is_charged_as_one_literal() {
    for n in [1u16, 64, 65, 300] {
        let whole = list_literal(n, n);
        let chunked = list_literal(n, 64);
        assert_eq!(bytes_needed(&whole), 16 + 8 * u64::from(n), "n = {n}");
        assert_eq!(bytes_needed(&chunked), 16 + 8 * u64::from(n), "n = {n}");
        assert_eq!(steps_needed(&chunked), steps_needed(&whole), "n = {n}");
    }
}

/// A map literal of `keys`, built as the compiler builds one: `NewMap` of
/// the first `chunk` pairs, `MapPut`s of the rest. Each value is its key.
fn map_literal(keys: &[i16], chunk: usize) -> Module {
    let mut code = Vec::new();
    for (index, part) in keys.chunks(chunk).enumerate() {
        for (i, &key) in part.iter().enumerate() {
            let at = u16::try_from(1 + 2 * i).unwrap();
            code.push(Instruction::wide_asbx(Op::LoadInt, at, key));
            code.push(Instruction::wide_asbx(Op::LoadInt, at + 1, key));
        }
        let count = u16::try_from(part.len()).unwrap();
        code.push(if index == 0 {
            Instruction::wide_abx(Op::NewMap, 0, u32::from(count))
        } else {
            Instruction::wide_abc(Op::MapPut, 0, 1, count)
        });
    }
    code.push(one(Instruction::abc(Op::Return, 0, 0, 0)));
    with_init(u16::try_from(2 * chunk + 1).unwrap(), vec![], code)
}

#[test]
fn map_put_merges_in_key_order_and_a_later_pair_wins() {
    // Keys out of order and repeated, across and within chunks: 3 appears
    // in the first chunk and twice in the second, where its last pair wins.
    let m = with_init(
        9,
        vec![],
        vec![
            Instruction::wide_asbx(Op::LoadInt, 1, 3),
            Instruction::wide_asbx(Op::LoadInt, 2, 30),
            Instruction::wide_asbx(Op::LoadInt, 3, 9),
            Instruction::wide_asbx(Op::LoadInt, 4, 90),
            Instruction::wide_abx(Op::NewMap, 0, 2),
            Instruction::wide_asbx(Op::LoadInt, 1, 3),
            Instruction::wide_asbx(Op::LoadInt, 2, 31),
            Instruction::wide_asbx(Op::LoadInt, 3, 1),
            Instruction::wide_asbx(Op::LoadInt, 4, 10),
            Instruction::wide_asbx(Op::LoadInt, 5, 3),
            Instruction::wide_asbx(Op::LoadInt, 6, 32),
            Instruction::wide_asbx(Op::LoadInt, 7, 5),
            Instruction::wide_asbx(Op::LoadInt, 8, 50),
            Instruction::wide_abc(Op::MapPut, 0, 1, 4),
            one(Instruction::abc(Op::Return, 0, 0, 0)),
        ],
    );
    assert_eq!(
        run(&m, Limits::default()),
        "Map{Int(1): Int(10), Int(3): Int(32), Int(5): Int(50), Int(9): Int(90)}"
    );
    // 24 bytes for each of the two keys that were new, as `NewMap` of all
    // four distinct keys would have charged: 32 + 24 * 4.
    assert_eq!(bytes_needed(&m), 32 + 24 * 4);
}

/// A map literal whose keys ascend from one chunk to the next moves nothing
/// when a later chunk is merged, and is charged exactly as one literal.
#[test]
fn an_ascending_map_literal_in_chunks_is_charged_as_one_literal() {
    let keys: Vec<i16> = (0..300).collect();
    let whole = map_literal(&keys, 300);
    let chunked = map_literal(&keys, 128);
    assert_eq!(bytes_needed(&chunked), 32 + 24 * 300);
    assert_eq!(bytes_needed(&whole), bytes_needed(&chunked));
    assert_eq!(steps_needed(&chunked), steps_needed(&whole));
}

/// A later chunk whose keys sort before existing entries moves those
/// entries, and is charged one step per 64 of them, before it moves any.
#[test]
fn prepending_to_a_large_map_is_charged_for_what_it_moves() {
    let keys: Vec<i16> = (0..300).rev().collect();
    let whole = map_literal(&keys, 300);
    let chunked = map_literal(&keys, 128);
    assert_eq!(bytes_needed(&chunked), bytes_needed(&whole));
    // The second chunk moves the first chunk's 128 entries (2 steps), the
    // third moves all 256 (4 steps).
    assert_eq!(steps_needed(&chunked), steps_needed(&whole) + 2 + 4);
    let rendered = run(&chunked, Limits::default());
    assert!(
        rendered.starts_with("Map{Int(0): Int(0), Int(1): Int(1),"),
        "{rendered}"
    );
    assert!(rendered.ends_with("Int(299): Int(299)}"), "{rendered}");
}

/// The shape the step charge exists for: a loop that inserts, every
/// iteration, a key that sorts before every entry of a map that keeps
/// growing, which would move the whole map each time. Each iteration is
/// charged for what it moves, so this reaches its step limit after moving
/// about 64 entries per step, not after an unbounded amount of work.
#[test]
fn a_loop_prepending_to_one_map_is_bounded_by_its_steps() {
    // r1 = the next key, counting down; r3 = the step -1. The jump is
    // relative to the word after it, and `with_init` puts one word first.
    let code = vec![
        Instruction::wide_abx(Op::NewMap, 0, 0),
        Instruction::wide_asbx(Op::LoadInt, 1, 0),
        Instruction::wide_asbx(Op::LoadInt, 3, -1),
        // loop:
        Instruction::wide_abx(Op::Charge, 0, 1),
        Instruction::wide_abc(Op::Move, 2, 1, 0),
        Instruction::wide_abc(Op::MapPut, 0, 1, 1),
        Instruction::wide_abc(Op::AddInt, 1, 1, 3),
        Instruction::wide_asbx(Op::Jump, 0, -5),
    ];
    let m = with_init(4, vec![], code);
    let steps = 20_000;
    let outcome = run(&m, limits(steps, 1 << 30));
    assert_eq!(outcome, "StepLimit");
    // Without the charge the loop would have run `steps` iterations; with
    // it, the map reaches only about sqrt(2 * 64 * steps) entries before
    // the limit, far fewer.
    let with_memory = run(&m, limits(steps, 24 * 3_000));
    assert_eq!(with_memory, "StepLimit", "fewer than 3,000 keys were added");
}

/// A module whose initialiser calls a closure that calls itself forever,
/// with a frame of `regs` registers.
fn endless_recursion(regs: u16) -> Module {
    let mut m = with_init(
        2,
        vec![],
        vec![
            Instruction::wide_abx(Op::Closure, 0, 1),
            Instruction::wide_abc(Op::Call, 0, 0, 0),
            one(Instruction::abc(Op::ReturnNone, 0, 0, 0)),
        ],
    );
    let name = m.add_string("down").unwrap();
    m.functions.push(Function {
        name,
        kind: FunctionKind::Closure,
        arity: 0,
        register_count: regs,
        returns_value: false,
        captures: vec![],
        constants: vec![],
        code: [
            Instruction::abx(Op::Closure, 0, 1),
            Instruction::abc(Op::Call, 0, 0, 0),
            Instruction::abc(Op::ReturnNone, 0, 0, 0),
        ]
        .map(Instruction::encode)
        .to_vec(),
        spans: vec![],
    });
    m
}

fn fault_of(m: &Module, limits: Limits) -> hollowscript_core::Fault {
    let verified = verify_bytes(&encode(m)).expect("verifies");
    let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
    vm.initialise(&limits, &mut Control::none())
        .expect_err("endless recursion faults")
}

/// Frames no wider than 256 registers reach `maxCallDepth`, as they always
/// did; frames as wide as a frame can be are stopped much sooner, when
/// the registers every call in progress holds reach the implementation's
/// own budget, since those are real memory no counter of section 11.4
/// charges. That is HS1015, a limit of the implementation (section 1.10),
/// not HS1005: the language's own limit was not reached.
#[test]
fn recursion_through_wide_frames_is_bounded_by_the_registers_it_holds() {
    let narrow = fault_of(&endless_recursion(256), Limits::default());
    assert_eq!(narrow.code, hollowscript_core::FaultCode::CallDepth);
    let wide = fault_of(&endless_recursion(65_535), Limits::default());
    assert_eq!(wide.code, hollowscript_core::FaultCode::ImplementationLimit);
    assert!(wide.message.contains("4194304 values"), "{}", wide.message);
    // A host that raises the call depth limit raises the budget with it,
    // 256 registers a level, so narrow frames still reach that limit.
    let deeper = Limits {
        max_call_depth: 20_000,
        ..Limits::default()
    };
    assert_eq!(
        fault_of(&endless_recursion(256), deeper).code,
        hollowscript_core::FaultCode::CallDepth
    );
}
