//! Regression tests for the outside security review's promise-2 findings
//! (a run is bounded): each test reproduces one finding's bytecode exactly
//! as the review gave it and asserts the fault or timing bound the fix
//! restores, described here by behaviour and specification section rather
//! than by the review's own identifiers.
//!
//! Run with `cargo test --release -p hollowscript-vm --test review_verifier -- --nocapture`
//! to see the timing line each test prints; the assertions do not depend on
//! the release profile, but a hang is much easier to see time out quickly
//! against it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use hollowscript_bytecode::container::encode;
use hollowscript_bytecode::module::{
    CaptureSource, Constant, Export, ExportKind, Function, FunctionKind, Global, Module,
    ModuleEntry,
};
use hollowscript_bytecode::opcode::{Instruction, Op};
use hollowscript_bytecode::verify::verify_bytes;
use hollowscript_core::transfer::{TransferType, TypeRef};
use hollowscript_core::{CancellationToken, Control, Limits};
use hollowscript_vm::{CapabilityTable, Interpreter};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn func(
    m: &mut Module,
    kind: FunctionKind,
    regs: u16,
    consts: Vec<Constant>,
    code: Vec<Instruction>,
) -> u32 {
    let name = m.add_string(format!("f{}", m.functions.len())).unwrap();
    m.functions.push(Function {
        name,
        kind,
        arity: 0,
        register_count: regs,
        returns_value: false,
        captures: vec![],
        constants: consts,
        code: code.into_iter().map(Instruction::encode).collect(),
        spans: vec![],
    });
    u32::try_from(m.functions.len() - 1).unwrap()
}

fn with_init(regs: u16, consts: Vec<Constant>, code: Vec<Instruction>) -> Module {
    let mut m = Module::new();
    let path = m.add_string("main.hws").unwrap();
    let init = func(&mut m, FunctionKind::ModuleInit, regs, consts, code);
    m.modules.push(ModuleEntry { path, init });
    m
}

/// Runs `m` on a background thread with a one-second cancellation deadline,
/// so that a fix regressing to an uncancellable loop shows up as `HANG`
/// rather than blocking the test suite forever.
fn run(m: &Module, limits: Limits, secs: u64) -> String {
    let verified = match verify_bytes(&encode(m)) {
        Ok(v) => v,
        Err(e) => return format!("REFUSED: {e}"),
    };
    let token = CancellationToken::new();
    let t2 = token.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
        let r = vm.initialise(&limits, &mut Control::new(Some(&t2), None));
        let _ = tx.send(match r {
            Ok(v) => format!("OK {v:?}"),
            Err(f) => format!("FAULT {:?}: {}", f.code, f),
        });
    });
    std::thread::sleep(Duration::from_millis(1000));
    token.cancel();
    rx.recv_timeout(Duration::from_secs(secs))
        .unwrap_or_else(|_| "HANG (not finished, cancellation token set and ignored)".to_string())
}

fn small() -> Limits {
    Limits {
        max_steps: 10_000,
        max_memory_bytes: 1_000_000,
        max_output_bytes: 1000,
        max_call_depth: 16,
    }
}

const RET: Instruction = Instruction::abc(Op::ReturnNone, 0, 0, 0);

/// A cell that holds itself (through a captured closure) used to
/// send the retained-size walk that runs after every load into an endless
/// loop. `tree_size` is now cycle-safe, so this must finish (with a fault
/// or a value, never a hang) well inside the one-second cancellation
/// deadline above.
#[test]
fn self_cell_retained_walk_does_not_hang() {
    let mut m = Module::new();
    let path = m.add_string("main.hws").unwrap();
    let body = func(
        &mut m,
        FunctionKind::Closure,
        1,
        vec![],
        vec![Instruction::abx(Op::Charge, 0, 1), RET],
    );
    m.functions[body as usize].captures = vec![CaptureSource::Register(0)];
    let init = func(
        &mut m,
        FunctionKind::ModuleInit,
        2,
        vec![],
        vec![
            Instruction::asbx(Op::LoadInt, 1, 10),
            Instruction::abc(Op::NewCell, 0, 1, 0),
            Instruction::abc(Op::SetCell, 0, 0, 0),
            Instruction::abx(Op::Closure, 1, u16::try_from(body).unwrap()),
            Instruction::abx(Op::SetGlobal, 1, 0),
            RET,
        ],
    );
    m.modules.push(ModuleEntry { path, init });
    let g = m.add_string("g").unwrap();
    m.globals.push(Global {
        module: 0,
        name: g,
        mutable: false,
    });
    let outcome = run(&m, small(), 10);
    assert!(!outcome.starts_with("HANG"), "{outcome}");
}

/// A cell that holds itself *directly*, `SetCell a, a`, with no list or
/// closure indirection at all, is refused by the verifier outright: it is
/// the one case a purely syntactic check can catch for certain, ahead of
/// ever running a size walk over it.
#[test]
fn directly_self_referential_setcell_is_refused() {
    let m = with_init(
        2,
        vec![],
        vec![
            Instruction::abc(Op::NewCell, 0, 0, 0),
            Instruction::abc(Op::SetCell, 0, 0, 0),
            RET,
        ],
    );
    let err = verify_bytes(&encode(&m)).unwrap_err();
    assert!(
        err.to_string().contains("a cell cannot hold itself"),
        "{err}"
    );
}

/// A loop through `IterNext`'s exit jump (the branch taken once a
/// collection is exhausted, which the interpreter does not charge for)
/// verified and then ran forever, because the verifier's cycle check
/// treated arriving at any `IterNext` as proof a charge had already
/// happened, without noticing that only the fall-through edge actually
/// charges. The exit edge alone, looped back on itself, must now be refused
/// by the verifier rather than accepted and then hang.
#[test]
fn iternext_exit_edge_self_loop_is_refused() {
    let m = with_init(
        4,
        vec![],
        vec![
            Instruction::abx(Op::NewList, 0, 0),
            Instruction::abc(Op::IterInit, 1, 0, 0),
            Instruction::asbx(Op::IterNext, 1, -1),
            RET,
        ],
    );
    let err = verify_bytes(&encode(&m)).unwrap_err();
    assert!(err.to_string().contains("charges no steps"), "{err}");
}

/// A loop through `IterNext`'s fall-through edge (the ordinary case: keep
/// iterating while there is another element) still verifies, because that
/// edge genuinely does charge every time it is taken. The exit edge fix
/// above must not reject correct programs like this one.
#[test]
fn iternext_fallthrough_loop_still_verifies() {
    let m = with_init(
        4,
        vec![],
        vec![
            Instruction::abx(Op::NewList, 0, 0),
            Instruction::abc(Op::IterInit, 1, 0, 0),
            Instruction::asbx(Op::IterNext, 1, 1),
            Instruction::asbx(Op::Jump, 0, -2),
            RET,
        ],
    );
    assert!(verify_bytes(&encode(&m)).is_ok());
}

/// `LoadK` of a `String` or `Filter` constant is not charged (section 11.4:
/// "string literals with no interpolation and filter literals that appear
/// in the source are not charged"), and must not escape into unbounded real
/// memory either. It once allocated a fresh uncharged copy on every
/// execution (a 1 MiB constant in a loop reached 1.62 GB), and a later fix
/// charged each copy instead, which made a plain `print("spam")` loop fault
/// HS1002. Both are wrong: one object per constant is made on first use and
/// shared from then on, so a loop that loads a large constant a hundred
/// thousand times ends on its step limit, never its memory limit, with one
/// copy on the heap.
#[test]
fn loadk_shares_one_uncharged_object_per_constant() {
    let mut m = Module::new();
    let path = m.add_string("main.hws").unwrap();
    let big = m.add_string("x".repeat(200_000)).unwrap();
    let init = func(
        &mut m,
        FunctionKind::ModuleInit,
        2,
        vec![Constant::Str(big), Constant::Filter(big)],
        vec![
            Instruction::abx(Op::Charge, 0, 1),
            Instruction::abx(Op::LoadK, 0, 0),
            Instruction::abx(Op::LoadK, 1, 1),
            Instruction::asbx(Op::Jump, 0, -4),
        ],
    );
    m.modules.push(ModuleEntry { path, init });
    let limits = Limits {
        max_steps: 100_000,
        max_memory_bytes: 1_000_000,
        max_output_bytes: 1000,
        max_call_depth: 16,
    };
    let verified = verify_bytes(&encode(&m)).unwrap();
    let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
    let fault = vm.initialise(&limits, &mut Control::none()).unwrap_err();
    assert_eq!(fault.code, hollowscript_core::fault::FaultCode::StepLimit);
    assert_eq!(
        vm.heap().len(),
        2,
        "one String and one Filter object, however often they were loaded"
    );
    // A collection keeps both: a later `LoadK` hands them out again.
    vm.collect(&[]);
    assert_eq!(vm.heap().len(), 2);
}

/// `ListAppend` used to copy the whole list on every execution
/// without charging for the copy, so a straight-line run of a great many
/// appends did a huge amount of uncancellable work between safepoints. It
/// now extends the list in place, so each execution costs the values it
/// appends and nothing more, and 100,000 of them in a row are a linear
/// amount of work that ends well before the cancellation deadline, rather
/// than 100,000 O(n) copies.
#[test]
fn straight_line_list_append_is_charged_and_cancellable() {
    let n: usize = 100_000;
    let mut code = vec![
        Instruction::asbx(Op::LoadInt, 1, 7),
        Instruction::abx(Op::NewList, 0, 0),
    ];
    code.extend(std::iter::repeat_n(
        Instruction::abc(Op::ListAppend, 0, 1, 1),
        n,
    ));
    code.push(RET);
    let verified = verify_bytes(&encode(&with_init(4, vec![], code))).expect("verifies");
    let token = CancellationToken::new();
    let t2 = token.clone();
    let start = Instant::now();
    let h = std::thread::spawn(move || {
        let limits = Limits {
            max_steps: 1,
            ..Limits::default()
        };
        let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
        format!(
            "{:?}",
            vm.initialise(&limits, &mut Control::new(Some(&t2), None))
                .map_err(|f| f.to_string())
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    token.cancel();
    let outcome = h.join().unwrap();
    let elapsed = start.elapsed();
    println!("N={n}, max_steps=1, cancelled at 0.2 s: {outcome} after {elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "the run must end promptly, not after copying the list {n} times: {elapsed:?}"
    );
}

/// Suspected item from the outside review: `MapPut` was found to have the
/// same shape of bug as `ListAppend`, cloning the whole existing
/// map on every call without charging a step for the copy. It now merges in
/// place, so replacing the one key this repeats costs a search and nothing
/// more, and the work a merge can do beyond its own pairs (moving entries
/// that sort after them) is charged steps; see
/// `wide_frames::prepending_to_a_large_map_is_charged_for_what_it_moves`.
#[test]
fn straight_line_map_put_is_charged_and_cancellable() {
    let n: usize = 100_000;
    let mut code = vec![
        Instruction::asbx(Op::LoadInt, 1, 7),
        Instruction::asbx(Op::LoadInt, 2, 7),
        Instruction::abx(Op::NewMap, 0, 0),
    ];
    code.extend(std::iter::repeat_n(
        Instruction::abc(Op::MapPut, 0, 1, 1),
        n,
    ));
    code.push(RET);
    let verified = verify_bytes(&encode(&with_init(4, vec![], code))).expect("verifies");
    let token = CancellationToken::new();
    let t2 = token.clone();
    let start = Instant::now();
    let h = std::thread::spawn(move || {
        let limits = Limits {
            max_steps: 1,
            ..Limits::default()
        };
        let mut vm = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
        format!(
            "{:?}",
            vm.initialise(&limits, &mut Control::new(Some(&t2), None))
                .map_err(|f| f.to_string())
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    token.cancel();
    let outcome = h.join().unwrap();
    let elapsed = start.elapsed();
    println!("N={n}, max_steps=1, cancelled at 0.2 s: {outcome} after {elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "the run must end promptly, not after copying the map {n} times: {elapsed:?}"
    );
}

/// Verifying an export list against a long chain of
/// `List(List(List(...)))` types used to redo
/// the whole transferability walk from scratch for every export, making
/// verification quadratic in the export count. It must now stay fast.
#[test]
fn export_transferability_is_not_quadratic() {
    let mut m = with_init(1, vec![], vec![RET]);
    let chain: u32 = 1 << 14;
    for i in 0..chain {
        m.types.add(TransferType::List(TypeRef(i + 1)));
    }
    m.types.add(TransferType::Int);
    let g = m.add_string("g").unwrap();
    m.globals.push(Global {
        module: 0,
        name: g,
        mutable: false,
    });
    for i in 0..500 {
        let name = m.add_string(format!("e{i}")).unwrap();
        m.exports.push(Export {
            name,
            kind: ExportKind::Constant {
                global: 0,
                ty: TypeRef(0),
            },
        });
    }
    let bytes = encode(&m);
    let start = Instant::now();
    let r = verify_bytes(&bytes);
    let elapsed = start.elapsed();
    println!(
        "{} bytes, verify took {elapsed:?}, ok={}",
        bytes.len(),
        r.is_ok()
    );
    assert!(r.is_ok());
    assert!(
        elapsed < Duration::from_secs(2),
        "transferability must be memoised across the whole type table, not walked once per export: {elapsed:?}"
    );
}
