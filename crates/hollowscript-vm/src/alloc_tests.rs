//! The allocation budget (`crate::alloc::with_budget`) standing in for a small
//! machine: every site that allocates in proportion to a size the program
//! controls must answer HS1015 (an implementation limit) and leave the
//! interpreter usable, rather than abort. Each test names the site.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::alloc::with_budget;
use crate::capability::CapabilityTable;
use crate::interp::Interpreter;
use crate::limits::RunCounters;
use crate::natives::NativeOutcome;
use crate::value::{MapEntry, MapKey, Object, Value};
use hollowscript_bytecode::module::{Function, FunctionKind, Module, ModuleEntry};
use hollowscript_bytecode::opcode::{Instruction, Op};
use hollowscript_bytecode::verify::verify;
use hollowscript_core::fault::{Fault, FaultCode};
use hollowscript_core::native::ROSTER;
use hollowscript_core::{Control, Limits};

fn vm() -> Interpreter {
    let mut m = Module::new();
    let path = m.add_string("main.hws").unwrap();
    let name = m.add_string("main").unwrap();
    m.functions.push(Function {
        name,
        kind: FunctionKind::ModuleInit,
        arity: 0,
        register_count: 1,
        returns_value: false,
        captures: vec![],
        constants: vec![],
        code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
        spans: vec![],
    });
    m.modules.push(ModuleEntry { path, init: 0 });
    Interpreter::new(verify(m).unwrap(), CapabilityTable::new(vec![]))
}

fn call(vm: &mut Interpreter, name: &str, args: &[Value]) -> Result<NativeOutcome, Fault> {
    call_with(vm, name, args, &Limits::default())
}

fn call_with(
    vm: &mut Interpreter,
    name: &str,
    args: &[Value],
    limits: &Limits,
) -> Result<NativeOutcome, Fault> {
    let position = ROSTER
        .iter()
        .position(|info| info.name == name)
        .unwrap_or_else(|| panic!("no native {name}"));
    vm.call_native(
        u32::try_from(position).unwrap(),
        args,
        false,
        limits,
        &mut Control::none(),
        &mut RunCounters::new(),
    )
}

fn value_of(outcome: Result<NativeOutcome, Fault>) -> Value {
    match outcome.expect("no fault") {
        NativeOutcome::Value(value) => value,
        NativeOutcome::Job(_) => panic!("a job, not a value"),
    }
}

#[track_caller]
fn limit_fault(outcome: Result<NativeOutcome, Fault>) {
    match outcome {
        Err(fault) => assert_eq!(fault.code, FaultCode::ImplementationLimit, "{fault:?}"),
        Ok(_) => panic!("expected HS1015"),
    }
}

fn list_of(vm: &mut Interpreter, n: i64) -> Value {
    let items = (0..n).map(Value::Int).collect();
    vm.alloc_object(Object::List(items)).unwrap()
}

fn text(vm: &mut Interpreter, text: &str) -> Value {
    vm.alloc_object(Object::Str(text.to_owned())).unwrap()
}

fn length_of(vm: &Interpreter, value: Value) -> usize {
    let Value::Heap(h) = value else {
        panic!("not a heap value")
    };
    match vm.heap.get(h) {
        Some(Object::List(items)) => items.len(),
        Some(Object::Str(s)) => s.len(),
        other => panic!("{other:?}"),
    }
}

/// After a refusal the interpreter answers the same call with room.
fn still_works(vm: &mut Interpreter) {
    let value = value_of(call(vm, "range", &[Value::Int(0), Value::Int(10)]));
    assert_eq!(length_of(vm, value), 10);
}

const VALUE: usize = size_of::<Value>();

#[test]
fn range_that_the_machine_cannot_hold_is_hs1015_not_an_abort() {
    let mut vm = vm();
    let big = [Value::Int(0), Value::Int(1_000_000)];
    with_budget(1 << 20, || limit_fault(call(&mut vm, "range", &big)));
    still_works(&mut vm);
    // The same call with room is fine, so it is the budget that refused it.
    let value = with_budget(1_000_000 * VALUE, || value_of(call(&mut vm, "range", &big)));
    assert_eq!(length_of(&vm, value), 1_000_000);
}

#[test]
fn list_concat_and_append_are_hs1015_when_the_copy_does_not_fit() {
    let mut vm = vm();
    let list = list_of(&mut vm, 10_000);
    with_budget(10_000 * VALUE, || {
        limit_fault(call(&mut vm, "list.concat", &[list, list]));
        limit_fault(call(&mut vm, "list.append", &[list, Value::Int(1)]));
    });
    still_works(&mut vm);
    let joined = with_budget(20_000 * VALUE, || {
        value_of(call(&mut vm, "list.concat", &[list, list]))
    });
    assert_eq!(length_of(&vm, joined), 20_000);
}

#[test]
fn list_slice_reverse_and_sort_are_hs1015_when_the_copy_does_not_fit() {
    let mut vm = vm();
    let list = list_of(&mut vm, 10_000);
    with_budget(1_000, || {
        limit_fault(call(&mut vm, "list.reverse", &[list]));
        limit_fault(call(&mut vm, "list.sort", &[list]));
        limit_fault(call(
            &mut vm,
            "list.slice",
            &[list, Value::Int(0), Value::Int(10_000)],
        ));
    });
    // The copy fits and the sort's own buffer does not.
    with_budget(10_000 * VALUE + 10, || {
        limit_fault(call(&mut vm, "list.sort", &[list]));
    });
    still_works(&mut vm);
}

/// `map` keeps no copy of its receiver: the job reads it through the heap, so
/// the only thing it reserves is the answer, one `Value` for each element.
#[test]
fn map_reserves_its_answer_only_and_does_not_copy_the_receiver() {
    let mut vm = vm();
    let list = list_of(&mut vm, 50_000);
    let callback = vm
        .alloc_object(Object::Closure {
            function: 0,
            captures: vec![],
        })
        .unwrap();
    with_budget(50_000 * VALUE - 1, || {
        limit_fault(call(&mut vm, "list.map", &[list, callback]));
    });
    // Exactly the answer's buffer is enough, and the job holds the receiver
    // by handle with nothing of it copied.
    let outcome = with_budget(50_000 * VALUE, || {
        call(&mut vm, "list.map", &[list, callback])
    });
    let Ok(NativeOutcome::Job(job)) = outcome else {
        panic!("expected a job")
    };
    let (source, copied) = job.storage();
    assert!(
        matches!(source, Some(Value::Heap(_)))
            && format!("{source:?}") == format!("{:?}", Some(list))
    );
    assert_eq!(copied, 0);
    // A filter reserves nothing up front and a flat map needs none.
    let outcome = with_budget(0, || call(&mut vm, "list.filter", &[list, callback]));
    assert!(matches!(outcome, Ok(NativeOutcome::Job(_))));
    still_works(&mut vm);
}

/// Hostile bytecode can change a map in place (`Op::MapPut` on a map a
/// closure captured) while a `map.*` job walks it. The job walks what the
/// map held when it started, so it never revisits an entry or drops one,
/// and its answer can never hold one key twice.
#[test]
fn a_map_job_does_not_see_its_receiver_change_under_it() {
    let mut vm = vm();
    let entry = |k: i64| MapEntry {
        key: MapKey::Int(k),
        value: Value::Int(k * 10),
    };
    let map = vm
        .alloc_object(Object::Map(vec![entry(2), entry(4), entry(6)]))
        .unwrap();
    let set = vm
        .alloc_object(Object::Set(vec![MapKey::Int(2), MapKey::Int(4)]))
        .unwrap();
    let callback = vm
        .alloc_object(Object::Closure {
            function: 0,
            captures: vec![],
        })
        .unwrap();
    let job = |vm: &mut Interpreter, name: &str, receiver: Value| match call(
        vm,
        name,
        &[receiver, callback],
    ) {
        Ok(NativeOutcome::Job(job)) => job,
        _ => panic!("expected a job"),
    };
    let jobs = [
        job(&mut vm, "map.filter", map),
        job(&mut vm, "map.mapValues", map),
        job(&mut vm, "map.each", map),
    ];
    let before = format!("{:?}", jobs[0].elements(&vm.heap).unwrap());
    // What `Interpreter::map_put` does: take the entries out and put a
    // longer list back, with a key in front of everything.
    let Value::Heap(h) = map else { panic!() };
    let mut entries = vm.heap.take_map_entries(h).unwrap();
    entries.insert(0, entry(1));
    assert!(vm.heap.put_map_entries(h, entries, 0));
    for job in &jobs {
        assert_eq!(format!("{:?}", job.elements(&vm.heap).unwrap()), before);
    }
    let set_job = job(&mut vm, "set.filter", set);
    assert_eq!(set_job.elements(&vm.heap).unwrap().len(), 2);
}

#[test]
fn sort_by_and_map_values_are_hs1015_when_their_buffers_do_not_fit() {
    let mut vm = vm();
    let list = list_of(&mut vm, 10_000);
    let map = map_of(&mut vm, 0, 10_000);
    let callback = closure(&mut vm);
    with_budget(100, || {
        limit_fault(call(&mut vm, "list.sortBy", &[list, callback]));
    });
    // The snapshot of the map fits and the answer's values do not.
    let snapshot = 10_000 * (VALUE + size_of::<MapKey>());
    with_budget(snapshot + 10_000 * VALUE - 1, || {
        limit_fault(call(&mut vm, "map.mapValues", &[map, callback]));
    });
    still_works(&mut vm);
}

/// `sortBy` asks for everything its merge sort needs when it starts: the
/// snapshot, the merge plan (one entry per merge, `len - 1` of them) and the
/// buffer a merge takes into, so none of it is allocated without asking
/// part way through the sort.
#[test]
fn sort_by_asks_for_its_plan_and_merge_buffer_at_the_start() {
    let mut vm = vm();
    let n: usize = 10_000;
    let list = list_of(&mut vm, 10_000);
    let callback = closure(&mut vm);
    let snapshot = n * VALUE;
    let plan = (n - 1) * size_of::<(usize, usize, usize)>();
    with_budget(snapshot, || {
        limit_fault(call(&mut vm, "list.sortBy", &[list, callback]));
    });
    with_budget(snapshot + plan + snapshot - 1, || {
        limit_fault(call(&mut vm, "list.sortBy", &[list, callback]));
    });
    let outcome = with_budget(snapshot + plan + snapshot, || {
        call(&mut vm, "list.sortBy", &[list, callback])
    });
    assert!(matches!(outcome, Ok(NativeOutcome::Job(_))));
}

fn closure(vm: &mut Interpreter) -> Value {
    vm.alloc_object(Object::Closure {
        function: 0,
        captures: vec![],
    })
    .unwrap()
}

/// A map of the `Int` keys `from .. from + n`.
fn map_of(vm: &mut Interpreter, from: i64, n: i64) -> Value {
    let entries = (from..from + n)
        .map(|k| MapEntry {
            key: MapKey::Int(k),
            value: Value::Int(k),
        })
        .collect();
    vm.alloc_object(Object::Map(entries)).unwrap()
}

/// A set of the `Int` elements `from .. from + n`.
fn set_of(vm: &mut Interpreter, from: i64, n: i64) -> Value {
    let items = (from..from + n).map(MapKey::Int).collect();
    vm.alloc_object(Object::Set(items)).unwrap()
}

#[track_caller]
fn fault_code(outcome: Result<NativeOutcome, Fault>, code: FaultCode, call: &str) {
    match outcome {
        Err(fault) => assert_eq!(fault.code, code, "{call}: {fault:?}"),
        Ok(_) => panic!("{call}: expected {code:?}"),
    }
}

/// Section 11.3's order of faults and 11.4's "charged before it is built":
/// the step charge, then the memory of a result whose size is known, then
/// the work. With a host that has no memory at all (a budget of 0) the
/// language's own limits must still be what a program sees, so the fault is
/// the same on every host.
#[test]
fn the_language_limits_fault_before_the_host_is_asked_for_memory() {
    let mut vm = vm();
    let n = 10_000;
    let list = list_of(&mut vm, n);
    let map = map_of(&mut vm, 0, n);
    let other_map = map_of(&mut vm, n, n);
    let set = set_of(&mut vm, 0, n);
    let other_set = set_of(&mut vm, n, n);
    let callback = closure(&mut vm);
    let long = text(&mut vm, &"x".repeat(100_000));
    let padded = text(&mut vm, &format!(" {} ", "x".repeat(100_000)));
    let words = {
        let word = text(&mut vm, "word");
        let items: Vec<Value> = (0..n).map(|_| word).collect();
        vm.alloc_object(Object::List(items)).unwrap()
    };
    let sep = text(&mut vm, ",");
    let entries = {
        let items: Vec<Value> = (0..n)
            .map(|k| {
                vm.alloc_object(Object::Record {
                    shape: vec![],
                    fields: vec![Value::Int(k), Value::Int(k)],
                })
                .unwrap()
            })
            .collect();
        vm.alloc_object(Object::List(items)).unwrap()
    };
    let (zero, all) = (Value::Int(0), Value::Int(n));
    let wide = Value::Int(200_000);
    let key = Value::Int(5);

    // Room for the steps, none for the result.
    let memory = Limits {
        max_memory_bytes: 1_000,
        ..Limits::default()
    };
    let known_size: &[(&str, Vec<Value>)] = &[
        ("list.sort", vec![list]),
        ("list.reverse", vec![list]),
        ("list.slice", vec![list, zero, all]),
        ("list.take", vec![list, all]),
        ("list.drop", vec![list, zero]),
        ("list.join", vec![words, sep]),
        ("list.map", vec![list, callback]),
        ("list.sortBy", vec![list, callback]),
        ("map.mapValues", vec![map, callback]),
        ("map.keys", vec![map]),
        ("map.values", vec![map]),
        ("map.entries", vec![map]),
        ("map.without", vec![map, key]),
        ("map.merge", vec![map, other_map]),
        ("set.toList", vec![set]),
        ("set.without", vec![set, key]),
        ("set.union", vec![set, other_set]),
        ("set.intersect", vec![set, set]),
        ("set.difference", vec![set, other_set]),
        ("string.substring", vec![long, zero, wide]),
        ("string.trim", vec![padded]),
    ];
    for (name, args) in known_size {
        let outcome = with_budget(0, || call_with(&mut vm, name, args, &memory));
        fault_code(outcome, FaultCode::MemoryLimit, name);
    }

    // No room for the steps either: HS1001 comes first.
    let steps = Limits {
        max_steps: 1,
        ..Limits::default()
    };
    let needed_steps: &[(&str, Vec<Value>)] = &[
        ("list.contains", vec![list, key]),
        ("list.indexOf", vec![list, key]),
        ("list.sort", vec![list]),
        ("list.sortBy", vec![list, callback]),
        ("list.join", vec![words, sep]),
        ("map.mapValues", vec![map, callback]),
        ("map.each", vec![map, callback]),
        ("set.filter", vec![set, callback]),
        ("map.merge", vec![map, other_map]),
        ("map.entries", vec![map]),
        ("mapFrom", vec![entries]),
        ("set", vec![list]),
        ("set.union", vec![set, other_set]),
        ("set.isSubsetOf", vec![set, other_set]),
    ];
    for (name, args) in needed_steps {
        let outcome = with_budget(0, || call_with(&mut vm, name, args, &steps));
        fault_code(outcome, FaultCode::StepLimit, name);
    }
    still_works(&mut vm);
}

/// The answers built in one exact buffer are right, including keys the two
/// sides share.
#[test]
fn merge_and_the_set_operations_answer_what_they_did() {
    let mut vm = vm();
    let left = map_of(&mut vm, 0, 4);
    let right = {
        let entries = (2..6)
            .map(|k| MapEntry {
                key: MapKey::Int(k),
                value: Value::Int(k * 100),
            })
            .collect();
        vm.alloc_object(Object::Map(entries)).unwrap()
    };
    let merged = value_of(call(&mut vm, "map.merge", &[left, right]));
    let Value::Heap(h) = merged else { panic!() };
    let Some(Object::Map(entries)) = vm.heap.get(h) else {
        panic!()
    };
    let pairs: Vec<String> = entries
        .iter()
        .map(|e| format!("{:?}={:?}", e.key, e.value))
        .collect();
    assert_eq!(
        pairs.join(" "),
        "Int(0)=Int(0) Int(1)=Int(1) Int(2)=Int(200) Int(3)=Int(300) Int(4)=Int(400) Int(5)=Int(500)"
    );
    let a = set_of(&mut vm, 0, 4);
    let b = set_of(&mut vm, 2, 4);
    for (name, expected) in [
        (
            "set.union",
            "[Int(0), Int(1), Int(2), Int(3), Int(4), Int(5)]",
        ),
        ("set.intersect", "[Int(2), Int(3)]"),
        ("set.difference", "[Int(0), Int(1)]"),
    ] {
        let Value::Heap(h) = value_of(call(&mut vm, name, &[a, b])) else {
            panic!()
        };
        let Some(Object::Set(items)) = vm.heap.get(h) else {
            panic!()
        };
        assert_eq!(format!("{items:?}"), expected, "{name}");
    }
}

/// The set operations and `merge` with an empty side, `isSubsetOf` both
/// ways, and `String` keys whose equal contents are different objects:
/// `merge` keeps the right side's entry, `union` the left side's key.
#[test]
fn merge_and_the_set_operations_with_empty_sides_and_string_keys() {
    let mut vm = vm();
    let empty_set = vm.alloc_object(Object::Set(vec![])).unwrap();
    let empty_map = vm.alloc_object(Object::Map(vec![])).unwrap();
    let small = set_of(&mut vm, 0, 3);
    let large = set_of(&mut vm, 0, 5);
    let subset = |vm: &mut Interpreter, x, y| {
        let Value::Bool(answer) = value_of(call(vm, "set.isSubsetOf", &[x, y])) else {
            panic!()
        };
        answer
    };
    assert!(subset(&mut vm, small, large));
    assert!(!subset(&mut vm, large, small));
    assert!(subset(&mut vm, empty_set, small));
    assert!(!subset(&mut vm, small, empty_set));
    assert!(subset(&mut vm, empty_set, empty_set));
    let length = |vm: &Interpreter, value: Value| {
        let Value::Heap(h) = value else { panic!() };
        match vm.heap.get(h) {
            Some(Object::Set(items)) => items.len(),
            Some(Object::Map(entries)) => entries.len(),
            other => panic!("{other:?}"),
        }
    };
    for (name, x, y, expected) in [
        ("set.union", small, empty_set, 3),
        ("set.union", empty_set, small, 3),
        ("set.intersect", small, empty_set, 0),
        ("set.difference", small, empty_set, 3),
        ("set.difference", empty_set, small, 0),
    ] {
        let answer = value_of(call(&mut vm, name, &[x, y]));
        assert_eq!(length(&vm, answer), expected, "{name}");
    }
    let three = map_of(&mut vm, 0, 3);
    for (x, y) in [(three, empty_map), (empty_map, three)] {
        let answer = value_of(call(&mut vm, "map.merge", &[x, y]));
        assert_eq!(length(&vm, answer), 3);
    }

    // Equal text in two different objects on each side.
    let (left_key, right_key) = (text(&mut vm, "k"), text(&mut vm, "k"));
    let (Value::Heap(lh), Value::Heap(rh)) = (left_key, right_key) else {
        panic!()
    };
    let map_with = |vm: &mut Interpreter, key, value| {
        vm.alloc_object(Object::Map(vec![MapEntry {
            key: MapKey::Str(key),
            value: Value::Int(value),
        }]))
        .unwrap()
    };
    let (left, right) = (map_with(&mut vm, lh, 1), map_with(&mut vm, rh, 2));
    let Value::Heap(h) = value_of(call(&mut vm, "map.merge", &[left, right])) else {
        panic!()
    };
    let Some(Object::Map(entries)) = vm.heap.get(h) else {
        panic!()
    };
    assert_eq!(entries.len(), 1);
    assert!(
        matches!(entries[0], MapEntry { key: MapKey::Str(k), value: Value::Int(2) } if k == rh)
    );
    let left = vm.alloc_object(Object::Set(vec![MapKey::Str(lh)])).unwrap();
    let right = vm.alloc_object(Object::Set(vec![MapKey::Str(rh)])).unwrap();
    let Value::Heap(h) = value_of(call(&mut vm, "set.union", &[left, right])) else {
        panic!()
    };
    let Some(Object::Set(items)) = vm.heap.get(h) else {
        panic!()
    };
    assert!(matches!(items[..], [MapKey::Str(k)] if k == lh));
}

/// `mapFrom` and `set` from a list in descending order with repeats: the
/// answer is sorted with each key once, `mapFrom` keeping the later entry's
/// value (section 14.2) and the earlier entry's key object, `set` the
/// earlier element.
#[test]
fn map_from_and_set_keep_the_right_one_of_each_key() {
    let mut vm = vm();
    let n = 1_000;
    let mut records = Vec::new();
    for i in (0..n).rev() {
        for copy in 0..2 {
            records.push(
                vm.alloc_object(Object::Record {
                    shape: vec![],
                    fields: vec![Value::Int(i), Value::Int(i * 10 + copy)],
                })
                .unwrap(),
            );
        }
    }
    let list = vm.alloc_object(Object::List(records)).unwrap();
    let Value::Heap(h) = value_of(call(&mut vm, "mapFrom", &[list])) else {
        panic!()
    };
    let Some(Object::Map(entries)) = vm.heap.get(h) else {
        panic!()
    };
    assert_eq!(entries.len(), 1_000);
    for (i, entry) in (0..n).zip(entries) {
        assert!(
            matches!(entry, MapEntry { key: MapKey::Int(k), value: Value::Int(v) } if *k == i && *v == i * 10 + 1),
            "{entry:?}"
        );
    }
    // Text keys: the first object of equal text is kept.
    let (first, second) = (text(&mut vm, "k"), text(&mut vm, "k"));
    let (Value::Heap(fh), Value::Heap(sh)) = (first, second) else {
        panic!()
    };
    let pair = |vm: &mut Interpreter, key, value| {
        vm.alloc_object(Object::Record {
            shape: vec![],
            fields: vec![key, Value::Int(value)],
        })
        .unwrap()
    };
    let items = vec![pair(&mut vm, first, 1), pair(&mut vm, second, 2)];
    let list = vm.alloc_object(Object::List(items)).unwrap();
    let Value::Heap(h) = value_of(call(&mut vm, "mapFrom", &[list])) else {
        panic!()
    };
    let Some(Object::Map(entries)) = vm.heap.get(h) else {
        panic!()
    };
    assert!(
        matches!(entries[..], [MapEntry { key: MapKey::Str(k), value: Value::Int(2) }] if k == fh)
    );

    let values: Vec<Value> = (0..n)
        .rev()
        .flat_map(|i| [Value::Int(i), Value::Int(i)])
        .collect();
    let list = vm.alloc_object(Object::List(values)).unwrap();
    let Value::Heap(h) = value_of(call(&mut vm, "set", &[list])) else {
        panic!()
    };
    let Some(Object::Set(items)) = vm.heap.get(h) else {
        panic!()
    };
    assert_eq!(
        format!("{items:?}"),
        format!("{:?}", (0..n).map(MapKey::Int).collect::<Vec<_>>())
    );
    let list = vm.alloc_object(Object::List(vec![second, first])).unwrap();
    let Value::Heap(h) = value_of(call(&mut vm, "set", &[list])) else {
        panic!()
    };
    let Some(Object::Set(items)) = vm.heap.get(h) else {
        panic!()
    };
    assert!(matches!(items[..], [MapKey::Str(k)] if k == sh));
}

#[test]
fn string_builders_are_hs1015_when_the_text_does_not_fit() {
    let mut vm = vm();
    let abc = text(&mut vm, "abc");
    let long = text(&mut vm, &"x".repeat(100_000));
    let fill = text(&mut vm, "-");
    let sep = text(&mut vm, ",");
    let empty = text(&mut vm, "");
    let ex = text(&mut vm, "x");
    let words = {
        let items: Vec<Value> = (0..1_000).map(|_| abc).collect();
        vm.alloc_object(Object::List(items)).unwrap()
    };
    with_budget(1_000, || {
        limit_fault(call(&mut vm, "string.repeat", &[abc, Value::Int(100_000)]));
        limit_fault(call(
            &mut vm,
            "string.padStart",
            &[abc, Value::Int(100_000), fill],
        ));
        limit_fault(call(
            &mut vm,
            "string.padEnd",
            &[abc, Value::Int(100_000), fill],
        ));
        limit_fault(call(&mut vm, "string.replace", &[long, ex, abc]));
        limit_fault(call(&mut vm, "string.toUpper", &[long]));
        limit_fault(call(&mut vm, "string.toLower", &[long]));
        limit_fault(call(&mut vm, "string.split", &[long, empty]));
        limit_fault(call(&mut vm, "string.chars", &[long]));
        limit_fault(call(&mut vm, "list.join", &[words, sep]));
    });
    still_works(&mut vm);
    // With room they all work.
    let joined = value_of(call(&mut vm, "list.join", &[words, sep]));
    assert_eq!(length_of(&vm, joined), 1_000 * 3 + 999);
    let repeated = value_of(call(&mut vm, "string.repeat", &[abc, Value::Int(1_000)]));
    assert_eq!(length_of(&vm, repeated), 3_000);
    let padded = value_of(call(
        &mut vm,
        "string.padStart",
        &[abc, Value::Int(10), fill],
    ));
    assert_eq!(length_of(&vm, padded), 10);
}

/// Case conversion works out the exact size first, charges it, and builds
/// the answer in one buffer of that size: `U+0390` upper-cases to three code
/// points (2 bytes to 6), so a budget of exactly the answer is enough and
/// one byte less is not, and a memory limit one byte short of the answer
/// faults HS1002 before the host is asked for anything.
#[test]
fn case_conversion_reserves_exactly_its_answer_after_charging_it() {
    let mut vm = vm();
    let greek = "\u{390}".repeat(10_000);
    let upper = greek.to_uppercase();
    let lower_source = "ΑΣ ΣΑΣ İ".repeat(1_000);
    let lower = lower_source.to_lowercase();
    for (name, source, answer) in [
        ("string.toUpper", greek.as_str(), upper.as_str()),
        ("string.toLower", lower_source.as_str(), lower.as_str()),
    ] {
        let receiver = text(&mut vm, source);
        with_budget(answer.len() - 1, || {
            limit_fault(call(&mut vm, name, &[receiver]));
        });
        let tight = Limits {
            max_memory_bytes: 16 + answer.len() as u64 - 1,
            ..Limits::default()
        };
        let outcome = with_budget(0, || call_with(&mut vm, name, &[receiver], &tight));
        fault_code(outcome, FaultCode::MemoryLimit, name);
        // The arena may need a slot, so allow for it alongside the answer.
        let value = with_budget(answer.len() + 1024, || {
            value_of(call(&mut vm, name, &[receiver]))
        });
        let Value::Heap(h) = value else { panic!() };
        let Some(Object::Str(built)) = vm.heap.get(h) else {
            panic!()
        };
        assert_eq!(built, answer, "{name}");
        assert_eq!(built.capacity(), answer.len(), "{name}");
    }
}

/// A list job reads its receiver through the heap, so the job itself must
/// root it: a collection with nothing else holding the list leaves the job
/// able to read every element. `reduce` is the job whose accumulator is not
/// the receiver, and a compiled program always holds the receiver in a
/// caller register too, so only a test like this one can see the job's own
/// root.
#[test]
fn a_job_roots_the_receiver_it_reads_through_the_heap() {
    let mut vm = vm();
    let list = list_of(&mut vm, 10);
    let callback = closure(&mut vm);
    let args = [list, Value::Int(0), callback];
    let Ok(NativeOutcome::Job(job)) = call(&mut vm, "list.reduce", &args) else {
        panic!("expected a job")
    };
    let mut roots = Vec::new();
    job.push_roots(&mut roots);
    vm.heap.collect(&roots);
    assert_eq!(job.elements(&vm.heap).unwrap().len(), 10);
}

/// A collection the host has no memory for (its mark bitmap and work list
/// grow with the heap) is skipped, freeing nothing, rather than aborting;
/// with room it frees what nothing holds.
#[test]
fn a_collection_with_no_memory_is_skipped() {
    let mut vm = vm();
    let garbage = list_of(&mut vm, 1_000);
    let Value::Heap(h) = garbage else { panic!() };
    assert_eq!(with_budget(0, || vm.collect(&[])), 0);
    assert!(vm.heap.get(h).is_some());
    assert!(vm.collect(&[]) >= 1);
    assert!(vm.heap.get(h).is_none());
}

/// A collection the host has no memory for backs off: the trigger waits for
/// another threshold of allocation rather than retrying before every
/// instruction, which would make each step cost a whole heap walk.
#[test]
fn a_skipped_collection_is_not_retried_at_once() {
    let mut vm = vm();
    // Enough allocation to make a collection due.
    let _big = list_of(&mut vm, 1 << 20);
    assert!(vm.heap.due_for_collection());
    assert_eq!(with_budget(0, || vm.collect(&[])), 0);
    assert!(!vm.heap.due_for_collection());
}

/// A wide list's children are marked a chunk at a time, so a collection
/// needs room for the mark bitmap and a few chunks, not for every child of
/// the widest object at once. One that fails part way through the mark
/// frees nothing, and every object live before it is still there.
#[test]
fn a_wide_list_is_marked_in_chunks_and_a_failed_mark_frees_nothing() {
    let mut vm = vm();
    let n = 100_000;
    let pieces: Vec<Value> = (0..n).map(|i| text(&mut vm, &i.to_string())).collect();
    let live = vm.alloc_object(Object::List(pieces.clone())).unwrap();
    let garbage = list_of(&mut vm, 10);
    let slots = vm.heap.len() + 16;
    let alive = |vm: &Interpreter, value: Value| {
        let Value::Heap(h) = value else { panic!() };
        vm.heap.get(h).is_some()
    };
    // Room for the bitmap and the root, not for one chunk of children.
    assert_eq!(with_budget(slots + 64, || vm.collect(&[live])), 0);
    assert!(alive(&vm, garbage) && alive(&vm, live));
    assert!(pieces.iter().all(|&p| alive(&vm, p)));
    // Room for a few chunks, far less than the 100,000 children at once.
    let freed = with_budget(slots + (1 << 20), || vm.collect(&[live]));
    assert!(freed >= 1, "{freed}");
    assert!(!alive(&vm, garbage) && alive(&vm, live));
    assert!(pieces.iter().all(|&p| alive(&vm, p)));
}

#[test]
fn the_heap_refuses_an_object_it_has_no_memory_for() {
    let mut vm = vm();
    // A fresh arena has no slot room, so the first allocation reserves one.
    let fault = with_budget(0, || vm.alloc_object(Object::Str(String::new()))).unwrap_err();
    assert_eq!(fault.code, FaultCode::ImplementationLimit);
    assert!(vm.alloc_object(Object::Str(String::new())).is_ok());
}

#[test]
fn json_text_is_hs1015_when_the_host_has_no_room_for_it() {
    use crate::json::{Json, try_pretty, try_stringify};
    let value = Json::Array((0..1_000).map(Json::Int).collect());
    with_budget(100, || {
        assert!(try_stringify(&value).is_err());
        assert!(try_pretty(&value, 2).is_err());
    });
    let compact = try_stringify(&value).unwrap();
    assert!(compact.starts_with("[0,1,2"));
    assert_eq!(try_pretty(&value, 2).unwrap().lines().count(), 1_002);
}
