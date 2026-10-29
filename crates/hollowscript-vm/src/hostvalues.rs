//! What a capability function is given so that it can read its arguments and
//! build its result (section 9.6).
//!
//! A scalar crosses the boundary as a plain [`Value`], but a `String`, a
//! list, a map, a set, a record or an enum value is a handle into the
//! instance's heap, so a host cannot read or build one without the heap
//! itself. [`HostCall`] is that access, plus the one thing the heap alone
//! cannot supply: the *names* of a record's fields and of an enum's
//! variants.
//!
//! **Why names need a table.** A record on the heap stores its field names
//! as `StringId`s into the module's string table, and an enum value names
//! its variant by an index into the module's variant table. Neither is
//! something a host could know, and neither can be invented at the point of
//! use, because the tables belong to the compiled module and are fixed by
//! the time it runs. [`HostTypes`] is built once, when an instance is
//! created, from the capability interface the host declared: every record
//! and enum a capability's own declared type mentions is registered in the
//! module's tables then, whether or not the program itself ever writes one.
//! That is what makes `Ok(text)` and `Err(IoError.NotFound(message))`
//! something a capability can return at all.

use crate::heap::Heap;
use crate::limits::RunCounters;
use crate::value::{MapEntry, MapKey, Object, Value};
use hollowscript_core::Limits;
use hollowscript_core::cancel::CancellationToken;
use hollowscript_core::fault::{Fault, FaultCode};
use std::collections::BTreeMap;

/// The name of the prelude enum every fallible capability returns.
pub const RESULT: &str = "Result";
/// `Result`'s success variant.
pub const OK: &str = "Ok";
/// `Result`'s failure variant.
pub const ERR: &str = "Err";

/// The module positions of the records and enums a host may build.
///
/// Built by the embedder when an instance is created; see the module
/// documentation for why it cannot be derived later.
#[derive(Clone, Debug, Default)]
pub struct HostTypes {
    /// `(enum name, variant name)` to the module's variant table position.
    variants: BTreeMap<(String, String), u32>,
    /// The module's variant table position back to `(enum name, variant
    /// name)`, the other direction: what a host needs to name a variant it
    /// reads off the heap (an `Object::Enum` names its variant by this
    /// position, never by the names themselves), not to build one.
    variant_names: BTreeMap<u32, (String, String)>,
    /// A record's field names, sorted, to the module's shape position, with
    /// the field name ids in the same order.
    shapes: BTreeMap<Vec<String>, (u16, Vec<u32>)>,
    /// A field name's `StringId` (as a plain `u32`, the same one
    /// `Object::Record`'s own `shape` stores) back to the name itself: what
    /// a host needs to name a record's fields it reads off the heap.
    string_names: BTreeMap<u32, String>,
}

impl HostTypes {
    /// An empty set of host types: a host that builds only scalars and
    /// strings needs nothing here.
    #[must_use]
    pub fn new() -> HostTypes {
        HostTypes::default()
    }

    /// Records that `enum_name.variant_name` is at `position` in the
    /// module's variant table.
    pub fn add_variant(&mut self, enum_name: &str, variant_name: &str, position: u32) {
        self.variants
            .insert((enum_name.to_owned(), variant_name.to_owned()), position);
        self.variant_names
            .insert(position, (enum_name.to_owned(), variant_name.to_owned()));
    }

    /// Records a record shape: `fields` sorted as the module stores them,
    /// `shape` its position in the module's shape table, and `field_ids` the
    /// string ids of the field names in that same order.
    pub fn add_shape(&mut self, fields: Vec<String>, shape: u16, field_ids: Vec<u32>) {
        for (id, name) in field_ids.iter().zip(fields.iter()) {
            self.string_names.insert(*id, name.clone());
        }
        self.shapes.insert(fields, (shape, field_ids));
    }

    /// The variant position of `enum_name.variant_name`, if it was registered.
    #[must_use]
    pub fn variant(&self, enum_name: &str, variant_name: &str) -> Option<u32> {
        self.variants
            .get(&(enum_name.to_owned(), variant_name.to_owned()))
            .copied()
    }

    /// `(enum name, variant name)` for the variant at `position` in the
    /// module's variant table, if it was registered: the other direction
    /// of [`HostTypes::variant`], for a host reading an `Object::Enum`
    /// off the heap rather than building one.
    #[must_use]
    pub fn variant_name(&self, position: u32) -> Option<(&str, &str)> {
        self.variant_names
            .get(&position)
            .map(|(e, v)| (e.as_str(), v.as_str()))
    }

    /// The field name that `id` (a `StringId`'s own `u32`, as
    /// `Object::Record`'s own `shape` stores it) names, if it was
    /// registered: the other direction of the names `field_ids` carries
    /// into [`HostTypes::add_shape`], for a host reading an
    /// `Object::Record` off the heap rather than building one.
    #[must_use]
    pub fn field_name(&self, id: u32) -> Option<&str> {
        self.string_names.get(&id).map(String::as_str)
    }

    /// Whether anything is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.variants.is_empty() && self.shapes.is_empty()
    }
}

/// One capability call in progress: the heap, the host's own type table, and
/// the run's counters, so that a capability can see how much of the memory
/// limit is left and charge what it writes against the output limit.
pub struct HostCall<'a> {
    heap: &'a mut Heap,
    types: &'a HostTypes,
    counters: &'a mut RunCounters,
    limits: &'a Limits,
    /// The run's own cancellation token, if the embedder gave the run one
    /// (an owned clone: a `CancellationToken` is a cheaply cloned `Arc`, so
    /// this needs no borrow of the run's `Control`). A capability whose own
    /// work can take a while, `fs.read` chief among them (section 16.8),
    /// polls this between chunks of that work rather than only at the
    /// interpreter's own safepoints, which never run until the capability
    /// itself returns.
    cancel: Option<CancellationToken>,
}

impl std::fmt::Debug for HostCall<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCall")
            .field("memory_remaining", &self.memory_remaining())
            .finish_non_exhaustive()
    }
}

impl<'a> HostCall<'a> {
    /// A call context over `heap`, with `types` as the host's registered
    /// records and enums, and `cancel` the run's own cancellation token if
    /// it has one (an owned clone of [`Control`](hollowscript_core::cancel::Control)'s
    /// own token, since a capability call outlives any borrow of `Control`
    /// itself). Pass `None` when there is no run to cancel: building or
    /// reading a value between runs, or a test.
    pub fn new(
        heap: &'a mut Heap,
        types: &'a HostTypes,
        counters: &'a mut RunCounters,
        limits: &'a Limits,
        cancel: Option<CancellationToken>,
    ) -> HostCall<'a> {
        HostCall {
            heap,
            types,
            counters,
            limits,
            cancel,
        }
    }

    /// Whether the run's cancellation token is set (section 11.7). A
    /// capability whose own work is not a single cheap step, `fs.read`
    /// reading a large file in chunks (section 16.8), calls this between
    /// chunks so a cancelled run stops promptly instead of waiting for the
    /// whole read to finish and only then reaching the interpreter's own
    /// safepoint. `false` when the run was given no token at all.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    /// `maxMemoryBytes` less the allocation counter (section 9.6), so that a
    /// host can refuse to build a result that would not fit before it does
    /// the work of building it.
    #[must_use]
    pub fn memory_remaining(&self) -> u64 {
        self.limits
            .max_memory_bytes
            .saturating_sub(self.counters.allocated())
    }

    /// Refuses `bytes` (raising the same HS1002 the allocation counter
    /// itself would once a value of that size existed) when it is more than
    /// [`HostCall::memory_remaining`], *without* charging the counter: the
    /// counter's own charge still happens the normal way, once, when the
    /// capability goes on to build the value (the caller of this function
    /// is expected to build something no larger than `bytes` only after
    /// this returns `Ok`). This is for a capability that already knows its
    /// result's exact size before building it and whose specification
    /// requires the fault to happen before that (`fs.read`, section 16.8:
    /// "a file larger than the remaining memory allowance faults with
    /// HS1002 before its contents are read"): it lets that
    /// capability avoid reading, or otherwise constructing, something that
    /// would only be discarded by the ordinary post-hoc charge.
    ///
    /// # Errors
    ///
    /// HS1002 if `bytes` is more than [`HostCall::memory_remaining`].
    pub fn refuse_if_too_large(&self, bytes: u64) -> Result<(), Fault> {
        if bytes > self.memory_remaining() {
            return Err(Fault::memory_limit(
                self.limits.max_memory_bytes,
                self.counters.allocated(),
                bytes,
            ));
        }
        Ok(())
    }

    /// Charges `bytes` against `maxOutputBytes` (section 11.6) *before* the
    /// capability writes them. Section 11.6 is explicit that a call which
    /// would take the total past the limit writes nothing at all, so a
    /// capability that produces output calls this first and writes only if
    /// it answers `Ok`.
    ///
    /// # Errors
    ///
    /// HS1004 when the call would bring the run's total above the limit.
    pub fn write_output(&mut self, bytes: u64) -> Result<(), Fault> {
        self.counters.charge_output(bytes, self.limits)
    }

    /// The heap, for a host that needs to read or build something this type
    /// has no shorthand for.
    pub fn heap(&mut self) -> &mut Heap {
        self.heap
    }

    // -- Reading arguments. --

    /// `value` as a string slice, or `None` if it is not a `String`.
    #[must_use]
    pub fn str_of(&self, value: Value) -> Option<&str> {
        match value {
            Value::Heap(h) => match self.heap.get(h) {
                Some(Object::Str(s)) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }

    /// `value` as its filter source text (Appendix A), or `None` if it is
    /// not a `Filter`. A host-specific capability that declares a `Filter`
    /// parameter ([section 16.11](../../docs/spec/16-standard-capabilities.md))
    /// receives it as this text alone, exactly as it was written between
    /// backticks; evaluating it against the host's own records, following
    /// Appendix A's schema and adapter interface, is entirely the host's
    /// own responsibility, since the language itself has no way to
    /// evaluate a filter and deliberately does not check one against
    /// anything here.
    #[must_use]
    pub fn filter_of(&self, value: Value) -> Option<&str> {
        match value {
            Value::Heap(h) => match self.heap.get(h) {
                Some(Object::Filter(s)) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }

    /// `value`'s elements, if it is a list.
    #[must_use]
    pub fn list_of(&self, value: Value) -> Option<&[Value]> {
        match value {
            Value::Heap(h) => match self.heap.get(h) {
                Some(Object::List(items)) => Some(items),
                _ => None,
            },
            _ => None,
        }
    }

    /// `value`'s entries, sorted ascending by key, if it is a map.
    #[must_use]
    pub fn map_of(&self, value: Value) -> Option<&[MapEntry]> {
        match value {
            Value::Heap(h) => match self.heap.get(h) {
                Some(Object::Map(entries)) => Some(entries),
                _ => None,
            },
            _ => None,
        }
    }

    /// `value`'s elements, sorted ascending, if it is a set.
    #[must_use]
    pub fn set_of(&self, value: Value) -> Option<&[MapKey]> {
        match value {
            Value::Heap(h) => match self.heap.get(h) {
                Some(Object::Set(elements)) => Some(elements),
                _ => None,
            },
            _ => None,
        }
    }

    /// `key` as a string slice, if it is a `String` key (as opposed to an
    /// `Int` or a `Bool` one), the same way [`HostCall::str_of`] reads a
    /// `String` value.
    #[must_use]
    pub fn key_str_of(&self, key: MapKey) -> Option<&str> {
        match key {
            MapKey::Str(h) => match self.heap.get(h) {
                Some(Object::Str(s)) => Some(s),
                _ => None,
            },
            MapKey::Int(_) | MapKey::Bool(_) => None,
        }
    }

    /// `value`'s fields, named and in the order the record's own shape
    /// declares (sorted by name), if it is a record whose shape this
    /// call's [`HostTypes`] has a name for: a record a program built from
    /// its own declared type always does, since the compiler puts every
    /// record shape a program writes into the module's own tables; a
    /// record built to match a *capability's* declared type only does if
    /// that shape was also registered here (which
    /// `hollowscript::hosttypes::register` does for every record type a
    /// capability interface mentions, whether or not the program itself
    /// ever writes one, exactly so this is never surprised by one it does
    /// not have a name for).
    #[must_use]
    pub fn record_of(&self, value: Value) -> Option<Vec<(&str, Value)>> {
        let Value::Heap(h) = value else {
            return None;
        };
        let Some(Object::Record { shape, fields }) = self.heap.get(h) else {
            return None;
        };
        let mut named = Vec::with_capacity(shape.len());
        for (id, &field) in shape.iter().zip(fields.iter()) {
            named.push((self.types.field_name(id.0)?, field));
        }
        Some(named)
    }

    /// `value`'s enum name, variant name and payload, if it is an enum
    /// value whose variant this call's [`HostTypes`] has a name for (see
    /// [`HostCall::record_of`]'s own note on when that holds).
    #[must_use]
    pub fn enum_of(&self, value: Value) -> Option<(&str, &str, &[Value])> {
        let Value::Heap(h) = value else {
            return None;
        };
        let Some(Object::Enum { variant, payload }) = self.heap.get(h) else {
            return None;
        };
        let (enum_name, variant_name) = self.types.variant_name(*variant)?;
        Some((enum_name, variant_name, payload))
    }

    // -- Building results. --

    /// A `String`.
    ///
    /// # Errors
    ///
    /// Returns HS1099 if the heap's object count is exhausted.
    pub fn string(&mut self, text: impl Into<String>) -> Result<Value, Fault> {
        self.alloc(Object::Str(text.into()))
    }

    /// A list.
    ///
    /// # Errors
    ///
    /// Returns HS1099 if the heap's object count is exhausted.
    pub fn list(&mut self, items: Vec<Value>) -> Result<Value, Fault> {
        self.alloc(Object::List(items))
    }

    /// A map. Section 9.6: "a map or set given in any order is put in key
    /// order by the binding", so a host may write its entries however its
    /// own data came out and this sorts them.
    ///
    /// # Errors
    ///
    /// Returns HS1013 if two entries have the same key, which is not a
    /// question of order and so cannot be repaired here: the host has
    /// described one map with two values for one key. Returns HS1099 if the
    /// heap's object count is exhausted.
    pub fn map(&mut self, mut entries: Vec<MapEntry>) -> Result<Value, Fault> {
        entries.sort_by(|a, b| self.heap.compare_keys(a.key, b.key));
        if entries
            .windows(2)
            .filter_map(|pair| Some((pair.first()?, pair.get(1)?)))
            .any(|(a, b)| self.heap.keys_equal(a.key, b.key))
        {
            return Err(repeated_key());
        }
        self.alloc(Object::Map(entries))
    }

    /// A set, sorted the same way [`HostCall::map`] sorts a map's entries.
    ///
    /// # Errors
    ///
    /// Returns HS1013 for a repeated element and HS1099 if the heap's object
    /// count is exhausted.
    pub fn set(&mut self, mut elements: Vec<MapKey>) -> Result<Value, Fault> {
        elements.sort_by(|&a, &b| self.heap.compare_keys(a, b));
        if elements
            .windows(2)
            .filter_map(|pair| Some((*pair.first()?, *pair.get(1)?)))
            .any(|(a, b)| self.heap.keys_equal(a, b))
        {
            return Err(repeated_key());
        }
        self.alloc(Object::Set(elements))
    }

    /// A record. `fields` may be in any order; the value is built in the
    /// sorted order the language uses, so that a field read by position
    /// finds the right one.
    ///
    /// # Errors
    ///
    /// Returns HS1099 if this record's field set was not registered when the
    /// instance was created, which is a defect in the host's declared
    /// capability interface, or if the heap is exhausted.
    pub fn record(&mut self, fields: Vec<(&str, Value)>) -> Result<Value, Fault> {
        let mut sorted: Vec<(String, Value)> = fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<String> = sorted.iter().map(|(name, _)| name.clone()).collect();
        let Some((_, field_ids)) = self.types.shapes.get(&names) else {
            return Err(Fault::internal(&format!(
                "a capability returned a record {names:?} that its declared type does not mention"
            )));
        };
        let shape = field_ids
            .iter()
            .map(|&id| hollowscript_bytecode::module::StringId(id))
            .collect();
        let values = sorted.into_iter().map(|(_, value)| value).collect();
        self.alloc(Object::Record {
            shape,
            fields: values,
        })
    }

    /// An enum value of `enum_name`'s variant `variant_name`.
    ///
    /// # Errors
    ///
    /// Returns HS1099 if that variant was not registered when the instance
    /// was created, which is a defect in the host's declared capability
    /// interface, or if the heap is exhausted.
    pub fn enum_value(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        payload: Vec<Value>,
    ) -> Result<Value, Fault> {
        let Some(variant) = self.types.variant(enum_name, variant_name) else {
            return Err(Fault::internal(&format!(
                "a capability returned {enum_name}.{variant_name}, \
                 which its declared type does not mention"
            )));
        };
        self.alloc(Object::Enum { variant, payload })
    }

    /// `Ok(value)` (section 8.2), the success of every fallible capability.
    ///
    /// # Errors
    ///
    /// As [`HostCall::enum_value`].
    pub fn ok(&mut self, value: Value) -> Result<Value, Fault> {
        self.enum_value(RESULT, OK, vec![value])
    }

    /// `Err(value)` (section 8.2).
    ///
    /// # Errors
    ///
    /// As [`HostCall::enum_value`].
    pub fn err(&mut self, value: Value) -> Result<Value, Fault> {
        self.enum_value(RESULT, ERR, vec![value])
    }

    fn alloc(&mut self, object: Object) -> Result<Value, Fault> {
        self.heap
            .alloc(object)
            .map_err(|_| Fault::internal("the heap's object count is exhausted"))
    }
}

/// HS1013 for a map or a set a host described with the same key twice.
/// Unlike an order, which the binding fixes, this is a host defect: the two
/// values it gave for one key cannot both be in the map.
fn repeated_key() -> Fault {
    Fault::new(
        FaultCode::CapabilityType,
        &[
            ("name", &"a host value"),
            ("expected", &"a map or set with no repeated key"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::{HostCall, HostTypes};
    use crate::heap::Heap;
    use crate::limits::RunCounters;
    use crate::value::{Object, Value};
    use hollowscript_core::Limits;

    fn types() -> HostTypes {
        let mut types = HostTypes::new();
        types.add_variant("Result", "Ok", 0);
        types.add_variant("Result", "Err", 1);
        types.add_variant("IoError", "NotFound", 2);
        types.add_shape(vec!["status".to_owned(), "text".to_owned()], 0, vec![7, 8]);
        types
    }

    #[test]
    fn a_host_can_build_a_result() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let text = call.string("hello").expect("a string");
        let ok = call.ok(text).expect("Ok");
        let Value::Heap(h) = ok else {
            panic!("an enum is a heap value")
        };
        match heap.get(h) {
            Some(Object::Enum { variant, payload }) => {
                assert_eq!(*variant, 0);
                assert_eq!(payload.len(), 1);
            }
            other => panic!("expected an enum, got {other:?}"),
        }
    }

    /// A host reads a `Filter` capability parameter as its source text
    /// alone (Appendix A): a `Filter` is built only by the interpreter,
    /// from a literal, never by a host, so this allocates one directly to
    /// stand in for that.
    #[test]
    fn a_host_reads_a_filter_as_its_source_text() {
        let mut heap = Heap::new();
        let value = heap
            .alloc(Object::Filter("status:open priority>=high".to_owned()))
            .expect("the heap has room");
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        assert_eq!(call.filter_of(value), Some("status:open priority>=high"));
        assert_eq!(call.str_of(value), None, "a Filter is not a String");
    }

    #[test]
    fn a_host_can_build_an_error_of_a_named_enum() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let message = call.string("no such file").expect("a string");
        let io = call
            .enum_value("IoError", "NotFound", vec![message])
            .expect("IoError.NotFound");
        let wrapped = call.err(io).expect("Err");
        let Value::Heap(h) = wrapped else {
            panic!("an enum is a heap value")
        };
        match heap.get(h) {
            Some(Object::Enum { variant, .. }) => assert_eq!(*variant, 1),
            other => panic!("expected an enum, got {other:?}"),
        }
    }

    /// A variant the host never declared is a defect in the host's own
    /// capability interface, reported rather than guessed at.
    #[test]
    fn an_unregistered_variant_is_refused_by_name() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let fault = call
            .enum_value("IoError", "Denied", vec![])
            .expect_err("Denied was not registered");
        assert!(
            fault.message.contains("IoError.Denied"),
            "{}",
            fault.message
        );
    }

    /// A record is built in sorted field order however the host wrote it,
    /// because that is the order a field read by position expects.
    #[test]
    fn a_record_is_built_in_sorted_field_order() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let text = call.string("body").expect("a string");
        let record = call
            .record(vec![("text", text), ("status", Value::Int(200))])
            .expect("the shape is registered");
        let Value::Heap(h) = record else {
            panic!("a record is a heap value")
        };
        match heap.get(h) {
            Some(Object::Record { shape, fields }) => {
                assert_eq!(shape.iter().map(|s| s.0).collect::<Vec<_>>(), [7, 8]);
                // "status" sorts before "text", so the Int comes first.
                assert!(matches!(fields.first(), Some(Value::Int(200))));
            }
            other => panic!("expected a record, got {other:?}"),
        }
    }

    #[test]
    fn an_unregistered_record_is_refused() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        assert!(call.record(vec![("nope", Value::Int(1))]).is_err());
    }

    /// A record built through `HostCall::record` reads back through
    /// `HostCall::record_of` with the same fields, named: the round trip a
    /// capability argument's own marshalling depends on.
    #[test]
    fn a_built_record_reads_back_named_through_record_of() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let text = call.string("body").expect("a string");
        let record = call
            .record(vec![("text", text), ("status", Value::Int(200))])
            .expect("the shape is registered");
        let fields = call.record_of(record).expect("a named record");
        let mut names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        assert_eq!(names, ["status", "text"]);
        let status = fields
            .iter()
            .find(|(name, _)| *name == "status")
            .map(|(_, value)| *value);
        assert!(matches!(status, Some(Value::Int(200))));
    }

    /// An `Ok(...)` value built through `HostCall::ok` reads back through
    /// `HostCall::enum_of` as `("Result", "Ok", [value])`.
    #[test]
    fn a_built_enum_value_reads_back_named_through_enum_of() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let text = call.string("hello").expect("a string");
        let ok = call.ok(text).expect("Ok");
        let (enum_name, variant_name, payload) = call.enum_of(ok).expect("a named enum value");
        assert_eq!(enum_name, "Result");
        assert_eq!(variant_name, "Ok");
        assert_eq!(payload.len(), 1);
    }

    /// A map and a set built through `HostCall` read back through
    /// `map_of`/`set_of`/`key_str_of`, sorted the same way `render` fixes
    /// (section 9.6: "a map or set given in any order is put in key order").
    #[test]
    fn a_built_map_and_set_read_back_through_map_of_and_set_of() {
        let mut heap = Heap::new();
        let types = types();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let key = call.string("b").expect("a string");
        let value = Value::Int(2);
        let map = call
            .map(vec![super::MapEntry {
                key: crate::value::MapKey::Str(match key {
                    Value::Heap(h) => h,
                    _ => unreachable!("call.string always answers a heap value"),
                }),
                value,
            }])
            .expect("a map");
        let entries = call.map_of(map).expect("a map");
        assert_eq!(entries.len(), 1);
        assert_eq!(call.key_str_of(entries[0].key), Some("b"));
        assert!(matches!(entries[0].value, Value::Int(2)));

        let set = call
            .set(vec![
                crate::value::MapKey::Int(1),
                crate::value::MapKey::Int(2),
            ])
            .expect("a set");
        let elements = call.set_of(set).expect("a set");
        assert_eq!(elements.len(), 2);
    }

    #[test]
    fn arguments_can_be_read_back() {
        let mut heap = Heap::new();
        let types = HostTypes::new();
        let mut counters = RunCounters::new();
        let limits = Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        assert_eq!(call.memory_remaining(), Limits::default().max_memory_bytes);
        let text = call.string("abc").expect("a string");
        assert_eq!(call.str_of(text), Some("abc"));
        let list = call.list(vec![Value::Int(1)]).expect("a list");
        assert_eq!(call.list_of(list).map(<[Value]>::len), Some(1));
        assert_eq!(call.str_of(Value::Int(1)), None);
    }

    /// Section 11.6: a capability charges what it is about to write, and a
    /// call that would take the run past `maxOutputBytes` is HS1004 before
    /// anything is written.
    #[test]
    fn output_is_charged_before_it_is_written() {
        let mut heap = Heap::new();
        let types = HostTypes::new();
        let mut counters = RunCounters::new();
        let limits = Limits {
            max_output_bytes: 10,
            ..Limits::default()
        };
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        assert!(call.write_output(6).is_ok());
        assert!(call.write_output(4).is_ok());
        let fault = call.write_output(1).expect_err("the eleventh byte is over");
        assert_eq!(fault.code.code().number(), 1004);
    }

    /// `refuse_if_too_large` raises HS1002 for a size that would not
    /// fit, without moving the allocation counter (so the caller's own,
    /// separate charge for whatever it goes on to build still counts once,
    /// not twice).
    #[test]
    fn refuse_if_too_large_raises_hs1002_without_charging() {
        let mut heap = Heap::new();
        let types = HostTypes::new();
        let mut counters = RunCounters::new();
        let limits = Limits {
            max_memory_bytes: 100,
            ..Limits::default()
        };
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        assert!(call.refuse_if_too_large(50).is_ok());
        let fault = call
            .refuse_if_too_large(101)
            .expect_err("more than the limit");
        assert_eq!(fault.code.code().number(), 1002);
        assert_eq!(
            call.memory_remaining(),
            100,
            "a refusal must not itself charge the counter"
        );
    }
}
