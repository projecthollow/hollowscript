//! Chapter 15's library functions, as the interpreter runs them: `json.*`
//! and `time.*`.
//!
//! The logic is in [`crate::json`] and [`crate::time`], which work on plain
//! Rust values and know nothing of a heap. This module is the boundary: it
//! turns a heap `Json` value into one of those and back, and a `TimeParts`
//! record into [`crate::time::Parts`] and back.
//!
//! **Why the positions are resolved once.** A `Json` value on the heap names
//! its variant by an index into the module's own variant table, and a
//! `JsonError` or `TimeParts` names its fields by ids into the module's
//! string table. The compiler puts an entry there for every `Json` variant
//! and both record shapes whenever the program calls one of these
//! functions, whether or not the program itself writes one. [`DataTypes`]
//! reads those entries once, when the module is loaded, so that a call does
//! not search the tables.

use crate::heap::Heap;
use crate::interp::charge_steps_sliced;
use crate::json::Json;
use crate::limits::RunCounters;
use crate::time::Parts;
use crate::value::{MapEntry, MapKey, Object, Value};
use hollowscript_bytecode::module::{Module, StringId};
use hollowscript_core::Limits;
use hollowscript_core::cancel::Control;
use hollowscript_core::fault::Fault;
use std::mem::size_of;

/// The variants of `Json`, in the order section 15.1 declares them, which is
/// the order their positions within the enum follow.
const JSON_VARIANTS: [&str; 7] = ["Null", "Bool", "Int", "Float", "String", "Array", "Object"];

/// The fields of `JsonError` (section 15.1) and of `TimeParts` (section
/// 15.2), each in the sorted order a record value stores them in.
const JSON_ERROR_FIELDS: [&str; 2] = ["message", "offset"];
const TIME_PARTS_FIELDS: [&str; 8] = [
    "day",
    "hour",
    "millisecond",
    "minute",
    "month",
    "second",
    "weekday",
    "year",
];

/// Where a module keeps the types chapter 15 builds.
#[derive(Clone, Debug, Default)]
pub struct DataTypes {
    /// The module variant table position of each `Json` variant, in
    /// [`JSON_VARIANTS`] order.
    json: Option<[u32; 7]>,
    /// The field name ids of `JsonError`, sorted.
    json_error: Option<Vec<StringId>>,
    /// The field name ids of `TimeParts`, sorted.
    time_parts: Option<Vec<StringId>>,
}

impl DataTypes {
    /// Reads the positions out of `module`. Anything the module does not
    /// have stays absent, and a call that needs it faults rather than
    /// guessing, which can only happen if the compiler did not register what
    /// the program's own calls need.
    #[must_use]
    pub fn of(module: &Module) -> DataTypes {
        let variant = |name: &str| {
            module
                .variants
                .iter()
                .position(|v| module.string(v.enum_name) == "Json" && module.string(v.name) == name)
                .and_then(|index| u32::try_from(index).ok())
        };
        let mut json = [0u32; 7];
        let mut complete = true;
        for (slot, name) in json.iter_mut().zip(JSON_VARIANTS) {
            match variant(name) {
                Some(position) => *slot = position,
                None => complete = false,
            }
        }
        let shape = |wanted: &[&str]| {
            module
                .shapes
                .iter()
                .find(|s| {
                    s.fields.len() == wanted.len()
                        && s.fields
                            .iter()
                            .zip(wanted)
                            .all(|(id, name)| module.string(*id) == *name)
                })
                .map(|s| s.fields.clone())
        };
        DataTypes {
            json: complete.then_some(json),
            json_error: shape(&JSON_ERROR_FIELDS),
            time_parts: shape(&TIME_PARTS_FIELDS),
        }
    }

    fn json_variant(&self, index: usize) -> Result<u32, Fault> {
        self.json
            .and_then(|table| table.get(index).copied())
            .ok_or_else(|| Fault::internal("this module has no Json type to build"))
    }

    /// Which variant of `Json` a module variant position is, if it is one.
    fn json_index_of(&self, position: u32) -> Option<usize> {
        self.json?.iter().position(|&p| p == position)
    }
}

/// The deepest an array or object may nest (section 15.1, matching
/// [`crate::json::MAX_DEPTH`], which is private to that module; kept as its
/// own constant here rather than exposed across the boundary, since this is
/// the only other place that needs it).
const MAX_DEPTH: u32 = 128;

/// The Rust-side memory one converted `Json` node costs by itself, beyond
/// whatever bytes its own content (a string's, or a container's element
/// buffer) adds on top: `Json`'s own size, so that the charge matches what
/// this function actually allocates rather than an arbitrary estimate.
const JSON_NODE_BYTES: u64 = size_of::<Json>() as u64;

/// Turns a heap value into the `Json` the pure functions work on.
///
/// `caller` names the library function this conversion is for, used only to
/// word a depth-limit fault the way that function's own argument checks do
/// (section 15.1, HS1011: "faults if the value nests more than 128 deep").
///
/// Charged and safepointed one node at a time as the walk proceeds
/// (section 11.4), not only once after the whole tree is built: `Json`,
/// unlike the interpreter's own heap, has no way to represent a value
/// shared through more than one reference, so converting one can produce
/// far more nodes than `value` itself has distinct heap objects (an array
/// doubled through a shared reference at every level costs the source
/// heap a handful of objects and this conversion two to the power of its
/// depth), and that cost is first paid right here.
///
/// # Errors
///
/// HS1099 if the value is not a `Json` value of this module, which the
/// checker makes unreachable from a program. HS1011 if it nests more than
/// 128 deep, checked as the value is walked rather than after the whole
/// (possibly far deeper) tree has already been built: a heap value can nest
/// as deeply as the memory limit allows, far past what this function would
/// ever need to finish building before refusing it, and building the whole
/// thing anyway before checking used to cost the conversion's own time and,
/// far worse, cost dropping the result later a stack frame per level. HS1001
/// or HS1002 if the walk itself goes over the run's step or memory budget,
/// or the run is cancelled, before the conversion finishes.
pub(crate) fn json_from_value(
    types: &DataTypes,
    heap: &Heap,
    value: Value,
    caller: &str,
    counters: &mut RunCounters,
    limits: &Limits,
    control: &mut Control<'_>,
) -> Result<Json, Fault> {
    // An explicit stack: even bounded to 128 levels, a `Json` array or
    // object can hold arbitrarily many elements side by side, so this may
    // not recurse.
    enum Step {
        Read(Value, u32),
        /// Close an array of `len` elements.
        Array(usize),
        /// Close an object of `len` members, whose keys are in `keys`.
        Object(Vec<String>),
    }
    let too_deep = || Fault::invalid_argument(caller, "the value nests more than 128 deep");
    let mut work = vec![Step::Read(value, 1)];
    let mut done: Vec<Json> = Vec::new();
    while let Some(step) = work.pop() {
        // One node's own memory, and one step, charged as it is produced.
        // A shared or doubling source value cannot be charged in advance by
        // its own size (section 11.4's "deep values"): the number of `Json`
        // nodes this walk will produce is exactly what is not known until
        // it is done, so the only place left to charge it is here, before
        // the next node is built rather than after every node has been.
        charge_steps_sliced(counters, limits, control, 1)?;
        counters.charge_memory(JSON_NODE_BYTES, limits)?;
        match step {
            Step::Array(len) => {
                let at = done.len().checked_sub(len).ok_or_else(under)?;
                let items = done.split_off(at);
                // The `Vec<Json>` buffer holding them, beyond the nodes
                // themselves, already charged one at a time above.
                counters.charge_memory(u64_len(len).saturating_mul(JSON_NODE_BYTES), limits)?;
                done.push(Json::Array(items));
            }
            Step::Object(keys) => {
                let at = done.len().checked_sub(keys.len()).ok_or_else(under)?;
                let values = done.split_off(at);
                // The `Vec<(String, Json)>` buffer, and every key's bytes:
                // a key is a fresh `String` this conversion itself builds,
                // never one already charged for elsewhere.
                let key_bytes: u64 = keys.iter().map(|k| k.len() as u64).sum();
                let slot = JSON_NODE_BYTES.saturating_add(size_of::<String>() as u64);
                counters.charge_memory(
                    u64_len(keys.len())
                        .saturating_mul(slot)
                        .saturating_add(key_bytes),
                    limits,
                )?;
                done.push(Json::Object(keys.into_iter().zip(values).collect()));
            }
            Step::Read(value, level) => {
                let Value::Heap(handle) = value else {
                    return Err(not_json());
                };
                let Some(Object::Enum { variant, payload }) = heap.get(handle) else {
                    return Err(not_json());
                };
                let index = types.json_index_of(*variant).ok_or_else(not_json)?;
                let first = payload.first().copied();
                match index {
                    0 => done.push(Json::Null),
                    1 => match first {
                        Some(Value::Bool(b)) => done.push(Json::Bool(b)),
                        _ => return Err(not_json()),
                    },
                    2 => match first {
                        Some(Value::Int(n)) => done.push(Json::Int(n)),
                        _ => return Err(not_json()),
                    },
                    3 => match first {
                        Some(Value::Float(f)) => done.push(Json::Float(f)),
                        _ => return Err(not_json()),
                    },
                    4 => {
                        let text = string_of(heap, first.ok_or_else(not_json)?)?;
                        counters.charge_memory(text.len() as u64, limits)?;
                        done.push(Json::String(text));
                    }
                    5 => {
                        if level > MAX_DEPTH {
                            return Err(too_deep());
                        }
                        let items = match first {
                            Some(Value::Heap(h)) => match heap.get(h) {
                                Some(Object::List(items)) => items.clone(),
                                _ => return Err(not_json()),
                            },
                            _ => return Err(not_json()),
                        };
                        work.push(Step::Array(items.len()));
                        work.extend(
                            items
                                .into_iter()
                                .rev()
                                .map(|item| Step::Read(item, level + 1)),
                        );
                    }
                    _ => {
                        if level > MAX_DEPTH {
                            return Err(too_deep());
                        }
                        let entries = match first {
                            Some(Value::Heap(h)) => match heap.get(h) {
                                Some(Object::Map(entries)) => entries.clone(),
                                _ => return Err(not_json()),
                            },
                            _ => return Err(not_json()),
                        };
                        let mut keys = Vec::with_capacity(entries.len());
                        for entry in &entries {
                            let MapKey::Str(handle) = entry.key else {
                                return Err(not_json());
                            };
                            keys.push(string_of(heap, Value::Heap(handle))?);
                        }
                        work.push(Step::Object(keys));
                        work.extend(
                            entries
                                .into_iter()
                                .rev()
                                .map(|e| Step::Read(e.value, level + 1)),
                        );
                    }
                }
            }
        }
    }
    done.pop().ok_or_else(under)
}

/// `json.field` and `json.at` (section 15.1): unlike `json_from_value`, this
/// never converts `value`'s whole tree into a [`Json`]. It only needs to
/// know which one of the seven `Json` variants `value` is (`json_from_value`
/// checks the same thing, one level at a time, which is all a member lookup
/// ever needs) and, if it is the right container kind, to look inside the
/// one heap `List` or `Map` that container's payload already is: the member
/// found there is returned exactly as it sits on the heap, still a `Json`
/// value of this module, with nothing rebuilt and nothing more of it
/// visited than the one member asked for. Before this, both functions ran
/// the whole value through `json_from_value` first and converted the
/// member found back into a fresh heap value with `Interpreter::json_to_value`,
/// which did far more work than reading one member needs and, worse, built
/// (and then had to drop) a `Json` copy of everything else `value`
/// contained too, however deep.
///
/// # Errors
///
/// HS1099 if `value` is not a `Json` value of this module.
pub(crate) fn json_member(
    types: &DataTypes,
    heap: &Heap,
    value: Value,
    member: JsonMember<'_>,
) -> Result<Option<Value>, Fault> {
    let Value::Heap(handle) = value else {
        return Err(not_json());
    };
    let Some(Object::Enum { variant, payload }) = heap.get(handle) else {
        return Err(not_json());
    };
    let index = types.json_index_of(*variant).ok_or_else(not_json)?;
    let first = payload.first().copied();
    match (member, index, first) {
        (JsonMember::Field(name), 6, Some(Value::Heap(h))) => match heap.get(h) {
            Some(Object::Map(entries)) => Ok(entries.iter().find_map(|entry| {
                let MapKey::Str(key) = entry.key else {
                    return None;
                };
                (string_of(heap, Value::Heap(key)).ok().as_deref() == Some(name))
                    .then_some(entry.value)
            })),
            _ => Err(not_json()),
        },
        (JsonMember::At(i), 5, Some(Value::Heap(h))) => match heap.get(h) {
            Some(Object::List(items)) => {
                Ok(usize::try_from(i).ok().and_then(|i| items.get(i)).copied())
            }
            _ => Err(not_json()),
        },
        // The right container kind but a malformed payload (not the heap
        // `List` or `Map` a `Json` value's own constructors always give
        // it) cannot arise from a well-formed `Json` value; the wrong
        // container kind (`json.field` on an `Array`, `json.at` on an
        // `Object`, either on a scalar) is not an error, only `none`.
        _ => Ok(None),
    }
}

/// Which member `json_member` looks for.
pub(crate) enum JsonMember<'a> {
    /// `json.field`'s key.
    Field(&'a str),
    /// `json.at`'s index.
    At(i64),
}

fn not_json() -> Fault {
    Fault::internal("a json function needed a Json value")
}

fn under() -> Fault {
    Fault::internal("a Json walk lost its place")
}

/// `n` as a `u64`, for a charge; a `usize` this large could not have come
/// from a real allocation on any host this runs on.
fn u64_len(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn string_of(heap: &Heap, value: Value) -> Result<String, Fault> {
    match value {
        Value::Heap(h) => match heap.get(h) {
            Some(Object::Str(s)) => Ok(s.clone()),
            _ => Err(not_json()),
        },
        _ => Err(not_json()),
    }
}

impl crate::interp::Interpreter {
    /// A `Json` value on the heap. Charged as it is built, one allocation
    /// per node, which is what section 11.4 charges a library function for.
    pub(crate) fn json_to_value(
        &mut self,
        json: &Json,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        // Post-order, over an explicit stack, for the same reason the read
        // uses one.
        enum Step<'a> {
            Visit(&'a Json),
            Array(usize),
            Object(Vec<String>),
        }
        let mut work = vec![Step::Visit(json)];
        let mut done: Vec<Value> = Vec::new();
        while let Some(step) = work.pop() {
            match step {
                Step::Visit(json) => match json {
                    Json::Null => {
                        let value = self.json_variant_value(0, Vec::new(), counters, limits)?;
                        done.push(value);
                    }
                    Json::Bool(b) => {
                        let value =
                            self.json_variant_value(1, vec![Value::Bool(*b)], counters, limits)?;
                        done.push(value);
                    }
                    Json::Int(n) => {
                        let value =
                            self.json_variant_value(2, vec![Value::Int(*n)], counters, limits)?;
                        done.push(value);
                    }
                    Json::Float(f) => {
                        let value =
                            self.json_variant_value(3, vec![Value::Float(*f)], counters, limits)?;
                        done.push(value);
                    }
                    Json::String(s) => {
                        let text = self.alloc_string_charged(s.clone(), counters, limits)?;
                        let value = self.json_variant_value(4, vec![text], counters, limits)?;
                        done.push(value);
                    }
                    Json::Array(items) => {
                        work.push(Step::Array(items.len()));
                        work.extend(items.iter().rev().map(Step::Visit));
                    }
                    Json::Object(members) => {
                        work.push(Step::Object(
                            members.iter().map(|(key, _)| key.clone()).collect(),
                        ));
                        work.extend(members.iter().rev().map(|(_, value)| Step::Visit(value)));
                    }
                },
                Step::Array(len) => {
                    let at = done.len().checked_sub(len).ok_or_else(under)?;
                    let items = done.split_off(at);
                    let list = self.alloc_charged(Object::List(items), counters, limits)?;
                    let value = self.json_variant_value(5, vec![list], counters, limits)?;
                    done.push(value);
                }
                Step::Object(keys) => {
                    let at = done.len().checked_sub(keys.len()).ok_or_else(under)?;
                    let values = done.split_off(at);
                    let mut entries = Vec::with_capacity(keys.len());
                    for (key, value) in keys.into_iter().zip(values) {
                        let key = self.alloc_string_charged(key, counters, limits)?;
                        let Value::Heap(handle) = key else {
                            return Err(not_json());
                        };
                        entries.push(MapEntry {
                            key: MapKey::Str(handle),
                            value,
                        });
                    }
                    let map = self.alloc_charged(Object::Map(entries), counters, limits)?;
                    let value = self.json_variant_value(6, vec![map], counters, limits)?;
                    done.push(value);
                }
            }
        }
        done.pop().ok_or_else(under)
    }

    fn json_variant_value(
        &mut self,
        index: usize,
        payload: Vec<Value>,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        let variant = self.data_types().json_variant(index)?;
        self.alloc_charged(Object::Enum { variant, payload }, counters, limits)
    }

    /// A `JsonError` record (section 15.1).
    pub(crate) fn json_error_value(
        &mut self,
        message: &str,
        offset: usize,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        let shape = self
            .data_types()
            .json_error
            .clone()
            .ok_or_else(|| Fault::internal("this module has no JsonError type to build"))?;
        let text = self.alloc_string_charged(message.to_owned(), counters, limits)?;
        // The fields are sorted: `message` then `offset`.
        let fields = vec![text, Value::Int(i64::try_from(offset).unwrap_or(i64::MAX))];
        self.alloc_charged(Object::Record { shape, fields }, counters, limits)
    }

    /// A `TimeParts` record (section 15.2).
    pub(crate) fn time_parts_value(
        &mut self,
        parts: &Parts,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        let shape = self
            .data_types()
            .time_parts
            .clone()
            .ok_or_else(|| Fault::internal("this module has no TimeParts type to build"))?;
        // Sorted field order, which is what `TIME_PARTS_FIELDS` lists.
        let fields = vec![
            Value::Int(parts.day),
            Value::Int(parts.hour),
            Value::Int(parts.millisecond),
            Value::Int(parts.minute),
            Value::Int(parts.month),
            Value::Int(parts.second),
            Value::Int(parts.weekday),
            Value::Int(parts.year),
        ];
        self.alloc_charged(Object::Record { shape, fields }, counters, limits)
    }

    /// Reads a `TimeParts` record back.
    pub(crate) fn time_parts_of(&self, value: Value) -> Result<Parts, Fault> {
        let Value::Heap(handle) = value else {
            return Err(Fault::internal("fromParts needs a TimeParts record"));
        };
        let Some(Object::Record { fields, .. }) = self.heap.get(handle) else {
            return Err(Fault::internal("fromParts needs a TimeParts record"));
        };
        let at = |index: usize| match fields.get(index) {
            Some(Value::Int(n)) => Ok(*n),
            _ => Err(Fault::internal("a TimeParts field is not an Int")),
        };
        Ok(Parts {
            day: at(0)?,
            hour: at(1)?,
            millisecond: at(2)?,
            minute: at(3)?,
            month: at(4)?,
            second: at(5)?,
            weekday: at(6)?,
            year: at(7)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{DataTypes, JsonMember, json_from_value, json_member};
    use crate::heap::Heap;
    use crate::limits::RunCounters;
    use crate::value::Object;
    use crate::value::Value;
    use hollowscript_bytecode::module::{Module, VariantDesc};
    use hollowscript_core::Limits;
    use hollowscript_core::cancel::Control;
    use hollowscript_core::fault::FaultCode;

    /// A module whose variant table has the seven `Json` variants, in
    /// `JSON_VARIANTS` order, so [`DataTypes::of`] finds all of them.
    fn json_module() -> Module {
        let mut m = Module::new();
        let enum_name = m.add_string("Json").unwrap();
        for name in super::JSON_VARIANTS {
            let name_id = m.add_string(name).unwrap();
            let index = u16::try_from(m.variants.len()).unwrap();
            m.variants.push(VariantDesc {
                enum_name,
                name: name_id,
                index,
                arity: u16::from(name != "Null"),
                decl_module: 0,
                decl_index: 0,
            });
        }
        m
    }

    /// A heap value `Json.Array([Json.Array([...])])`, `levels` deep, with a
    /// `Json.Null` at the bottom.
    fn deep_array(heap: &mut Heap, array_variant: u32, null_variant: u32, levels: u32) -> Value {
        let mut value = heap
            .alloc(Object::Enum {
                variant: null_variant,
                payload: vec![],
            })
            .unwrap();
        for _ in 0..levels {
            let list = heap.alloc(Object::List(vec![value])).unwrap();
            value = heap
                .alloc(Object::Enum {
                    variant: array_variant,
                    payload: vec![list],
                })
                .unwrap();
        }
        value
    }

    /// `json_from_value` used to build the whole tree, however deep
    /// the heap value actually was, before anything checked it against
    /// section 15.1's 128-level limit; converting the review's own
    /// reproduction (a heap value nested a million deep) took real, if
    /// linear, time and then, worse, could not always be dropped again (see
    /// `json.rs`'s own reasoning about recursive drop glue). The depth check
    /// now runs as the value is walked, so a heap value nested far past 128
    /// levels is refused, HS1011, well before the whole of it is ever built.
    #[test]
    fn json_from_value_refuses_a_heap_value_nested_past_128_levels() {
        let module = json_module();
        let types = DataTypes::of(&module);
        let mut heap = Heap::new();
        let value = deep_array(&mut heap, 5, 0, 100_000);
        let limits = Limits::default();
        let mut counters = RunCounters::new();
        let err = json_from_value(
            &types,
            &heap,
            value,
            "stringify",
            &mut counters,
            &limits,
            &mut Control::none(),
        )
        .unwrap_err();
        assert_eq!(err.code, FaultCode::InvalidArgument);
        assert!(err.message.contains("nests more than 128 deep"), "{err}");

        // Exactly 128 levels is still accepted (the outermost counts as 1,
        // matching `crate::json::depth`'s own convention).
        let ok = deep_array(&mut heap, 5, 0, 128);
        let mut counters = RunCounters::new();
        assert!(
            json_from_value(
                &types,
                &heap,
                ok,
                "stringify",
                &mut counters,
                &limits,
                &mut Control::none()
            )
            .is_ok()
        );
    }

    /// The walk that converts a heap value into `Json` charges memory one
    /// node at a time as it goes, so a value that would convert into far
    /// more `Json` nodes than the run's memory budget allows is refused
    /// before the whole of it is built: this used to only be charged, all
    /// at once, after the (possibly far larger) converted tree already
    /// existed. A doubling array eight levels deep, well inside the 128
    /// level nesting limit, already has more distinct converted nodes than
    /// a handful of bytes of memory can pay for.
    #[test]
    fn json_from_value_charges_memory_as_it_converts_a_doubling_value() {
        let module = json_module();
        let types = DataTypes::of(&module);
        let mut heap = Heap::new();
        let mut value = heap
            .alloc(Object::Enum {
                variant: 0,
                payload: vec![],
            })
            .unwrap();
        for _ in 0..8 {
            let list = heap.alloc(Object::List(vec![value, value])).unwrap();
            value = heap
                .alloc(Object::Enum {
                    variant: 5,
                    payload: vec![list],
                })
                .unwrap();
        }
        let limits = Limits {
            max_memory_bytes: 64,
            ..Limits::default()
        };
        let mut counters = RunCounters::new();
        let err = json_from_value(
            &types,
            &heap,
            value,
            "stringify",
            &mut counters,
            &limits,
            &mut Control::none(),
        )
        .unwrap_err();
        assert_eq!(err.code, FaultCode::MemoryLimit, "{err}");
    }

    /// `json.field` and `json.at`: reading one member must not need
    /// the whole value converted, so it must not be bounded by the 128-level
    /// limit `json_from_value` enforces for `stringify` and `pretty` (the
    /// spec states that limit only for those two functions). A heap value
    /// nested past 128 levels, indexed once at its outermost level, must
    /// still succeed.
    #[test]
    fn json_member_is_not_bounded_by_the_stringify_depth_limit() {
        let module = json_module();
        let types = DataTypes::of(&module);
        let mut heap = Heap::new();
        let value = deep_array(&mut heap, 5, 0, 100_000);
        let found = json_member(&types, &heap, value, JsonMember::At(0))
            .expect("json.at does not check depth")
            .expect("index 0 exists");
        // What was found is itself a `Json.Array`, one level shallower.
        let Value::Heap(h) = found else {
            panic!("expected a heap value")
        };
        assert!(matches!(heap.get(h), Some(Object::Enum { variant: 5, .. })));
    }
}
