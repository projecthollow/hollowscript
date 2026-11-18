//! Section 16.4: `random.int(min, upTo)` faults with HS1011 when
//! `min >= upTo`. The language checks it before it calls the host, so every
//! host gets the same fault and none is asked for an answer it cannot give.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use hollowscript::{
    CallOutcome, CapabilityDecl, CapabilityInterface, CapabilityShape, Control, FaultCode,
    Instance, Limits, LoadOutcome, NoLoader, Options, Value,
};
use hollowscript_core::transfer::{TransferType, TypeTable};
use std::cell::Cell;
use std::rc::Rc;

const SOURCE: &str = "uses random\n\
    export fn roll(lo: Int, hi: Int): Int {\n\
    \x20   return effect random.int(lo, hi)\n\
    }\n";

fn instance(batched: bool, calls: &Rc<Cell<u32>>) -> Instance {
    load(SOURCE, 2, batched, Limits::default(), calls)
}

/// Loads `source` against a host whose `random.int` takes `int_params`
/// `Int`s (the standard one takes two) and counts its calls in `calls`.
fn load(
    source: &str,
    int_params: usize,
    batched: bool,
    limits: Limits,
    calls: &Rc<Cell<u32>>,
) -> Instance {
    let mut types = TypeTable::new();
    let int = types.add(TransferType::Int);
    let float = types.add(TransferType::Float);
    let options = Options {
        capabilities: CapabilityInterface {
            capabilities: vec![CapabilityDecl {
                name: "random".to_owned(),
                shape: CapabilityShape::Namespace(vec![
                    ("int".to_owned(), vec![int; int_params], Some(int)),
                    ("float".to_owned(), vec![], Some(float)),
                ]),
            }],
            types,
        },
        batch_capability_calls: batched,
        limits,
        ..Options::default()
    };
    let counted = Rc::clone(calls);
    let mut bind =
        move |name: &str, member: Option<&str>| -> Option<Box<dyn hollowscript::HostFn>> {
            let counted = Rc::clone(&counted);
            match (name, member) {
                ("random", Some("int")) => Some(Box::new(
                    move |_: &[Value], _: &mut hollowscript::HostCall<'_>| {
                        counted.set(counted.get() + 1);
                        Ok(Value::Int(3))
                    },
                )),
                _ => None,
            }
        };
    match hollowscript::load(
        source,
        "main.hws",
        &options,
        &mut NoLoader,
        &mut bind,
        &mut Control::none(),
    ) {
        LoadOutcome::Instance(instance) => *instance,
        LoadOutcome::CheckFailed(found) => panic!("{found:?}"),
    }
}

fn roll(instance: &mut Instance, lo: i64, hi: i64) -> CallOutcome {
    instance
        .call(
            "roll",
            &[Value::Int(lo), Value::Int(hi)],
            &mut Control::none(),
        )
        .unwrap()
}

#[test]
fn an_empty_range_faults_with_hs1011_and_the_host_is_not_called() {
    for (lo, hi) in [(5, 5), (9, 2), (i64::MAX, i64::MIN), (0, 0), (-3, -3)] {
        let calls = Rc::new(Cell::new(0));
        let mut instance = instance(false, &calls);
        match roll(&mut instance, lo, hi) {
            CallOutcome::Faulted(fault) => {
                assert_eq!(fault.code, FaultCode::InvalidArgument, "({lo}, {hi})");
                assert!(fault.message.contains("random.int"), "{}", fault.message);
            }
            other => panic!("({lo}, {hi}): {other:?}"),
        }
        assert_eq!(calls.get(), 0, "({lo}, {hi}) called the host");
    }
}

#[test]
fn a_valid_range_calls_the_host_once() {
    let calls = Rc::new(Cell::new(0));
    let mut instance = instance(false, &calls);
    let CallOutcome::Returned(Some(value)) = roll(&mut instance, 1, 7) else {
        panic!("a valid range should return");
    };
    assert_eq!(instance.render(value), "Int(3)");
    assert_eq!(calls.get(), 1);
    // The edge: one value in the range is a valid range.
    let CallOutcome::Returned(Some(_)) = roll(&mut instance, i64::MIN, i64::MIN + 1) else {
        panic!("a range of one value should return");
    };
    assert_eq!(calls.get(), 2);
}

#[test]
fn on_the_batched_path_an_empty_range_faults_and_queues_nothing() {
    let calls = Rc::new(Cell::new(0));
    let mut instance = instance(true, &calls);
    match roll(&mut instance, 5, 5) {
        CallOutcome::Faulted(fault) => assert_eq!(fault.code, FaultCode::InvalidArgument),
        other => panic!("{other:?}"),
    }
    assert!(instance.commands().is_empty(), "a command was queued");
    assert_eq!(calls.get(), 0);

    let mut instance = self::instance(true, &calls);
    assert!(matches!(roll(&mut instance, 1, 7), CallOutcome::Suspended));
    assert_eq!(instance.commands().len(), 1);
}

/// Section 11.3's order: the argument step charge comes before the range
/// check. The smallest step limit at which `random.int(5, 5)` reaches HS1011
/// is the smallest at which a valid call reaches the host, so the check costs
/// nothing a valid call does not, and one step less is HS1001 for both.
#[test]
fn the_argument_step_charge_comes_before_the_range_check() {
    let outcome = |lo: i64, hi: i64, max_steps: u64, calls: &Rc<Cell<u32>>| {
        let limits = Limits {
            max_steps,
            ..Limits::default()
        };
        let mut instance = load(SOURCE, 2, false, limits, calls);
        roll(&mut instance, lo, hi)
    };
    let faults_with = |result: &CallOutcome, code: FaultCode| matches!(result, CallOutcome::Faulted(fault) if fault.code == code);
    let calls = Rc::new(Cell::new(0));
    let reaches_host = (1..200)
        .find(|&steps| {
            outcome(1, 7, steps, &calls);
            calls.get() > 0
        })
        .expect("a valid call reaches the host within 200 steps");
    let unused = Rc::new(Cell::new(0));
    let invalid = (1..200)
        .find(|&steps| faults_with(&outcome(5, 5, steps, &unused), FaultCode::InvalidArgument))
        .expect("an empty range faults within 200 steps");
    assert_eq!(invalid, reaches_host);
    assert!(faults_with(
        &outcome(5, 5, invalid - 1, &unused),
        FaultCode::StepLimit
    ));
    assert_eq!(unused.get(), 0);
}

/// The check is for the standard `random.int(min, upTo): Int`. A host that
/// declares its own `random` with an `int` of another shape gets its own
/// function called, whatever the argument.
#[test]
fn a_host_random_int_of_another_shape_is_not_checked() {
    let source = "uses random\n\
        export fn roll(n: Int): Int {\n\
        \x20   return effect random.int(n)\n\
        }\n";
    let calls = Rc::new(Cell::new(0));
    let mut instance = load(source, 1, false, Limits::default(), &calls);
    let outcome = instance
        .call("roll", &[Value::Int(0)], &mut Control::none())
        .unwrap();
    assert!(
        matches!(outcome, CallOutcome::Returned(Some(_))),
        "{outcome:?}"
    );
    assert_eq!(calls.get(), 1);
}
