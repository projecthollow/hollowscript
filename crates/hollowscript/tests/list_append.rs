//! `list.append` may grow a uniquely held list in place, but must keep
//! immutable value semantics whenever another language value or the host
//! can still observe the receiver.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use hollowscript::{CallOutcome, Control, Instance, LoadOutcome, LoadResult, NoLoader, Options};

fn instance(source: &str) -> Instance {
    match hollowscript::load(
        source,
        "main.hws",
        &Options::default(),
        &mut NoLoader,
        &mut |_, _| None,
        &mut Control::none(),
    ) {
        LoadOutcome::Instance(instance) => *instance,
        LoadOutcome::CheckFailed(found) => panic!("{found:?}"),
    }
}

fn returned_bool(source: &str) -> bool {
    let mut instance = instance(source);
    let Some(LoadResult::Value(value)) = instance.result() else {
        panic!("the program did not return a value: {:?}", instance.fault());
    };
    instance.render(value) == "Bool(true)"
}

#[test]
fn a_second_register_keeps_the_old_list_value() {
    assert!(returned_bool(
        "variable original = [1]\n\
         variable alias = original\n\
         variable added = original.append(2)\n\
         return original == [1] and alias == [1] and added == [1, 2]\n"
    ));
}

#[test]
fn a_list_stored_inside_another_heap_object_keeps_its_old_value() {
    assert!(returned_bool(
        "variable original = [1]\n\
         variable nested = [original]\n\
         variable added = original.append(2)\n\
         return nested[0] == [1] and added == [1, 2]\n"
    ));
}

#[test]
fn a_script_callee_keeps_the_callers_list_value() {
    assert!(returned_bool(
        "fn append(xs: [Int]): [Int] { return xs.append(2) }\n\
         variable original = [1]\n\
         variable added = append(original)\n\
         return original == [1] and added == [1, 2]\n"
    ));
}

#[test]
fn an_exported_argument_keeps_the_value_the_host_passed() {
    let mut instance = instance(
        "export fn seed(): [Int] { return [1] }\n\
         export fn append(xs: [Int]): [Int] {\n\
         \x20 return xs.append(2)\n\
         }\n",
    );
    let original = match instance.call("seed", &[], &mut Control::none()) {
        Ok(CallOutcome::Returned(Some(value))) => value,
        other => panic!("seed export failed: {other:?}"),
    };
    match instance.call("append", &[original], &mut Control::none()) {
        Ok(CallOutcome::Returned(Some(added))) => {
            assert_eq!(instance.render(original), "[Int(1)]");
            assert_eq!(instance.render(added), "[Int(1), Int(2)]");
        }
        other => panic!("append export failed: {other:?}"),
    }
}
