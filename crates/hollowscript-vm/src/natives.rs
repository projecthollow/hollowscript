//! The standard library (chapters 13 to 15), as the interpreter runs it.
//!
//! `hollowscript-core`'s roster (`native.rs`) is the shared list of names and
//! arities; this module is the only place their behaviour is defined. A
//! module's import table is resolved against the roster when the module is
//! loaded ([`resolve_imports`]), so a program that calls a name this build
//! does not implement is refused with a clear fault at load time rather than
//! part way through a run.
//!
//! **What is implemented.** Every entry of the roster: chapters 13, 14 and
//! 15, and the `Result` methods of section 8.4. [`is_implemented`] is kept
//! so that an entry added to the roster before its body exists is refused
//! by name when a module is loaded, rather than faulting somewhere inside
//! the interpreter.
//!
//! A function that calls a closure (`list.map` and the sixteen others) is a
//! [`NativeJob`] on the interpreter's own frame stack: it is pushed, driven
//! one callback at a time by the interpreter itself, and finished
//! when it runs out of elements. Nothing about it recurses, so a callback
//! that itself calls `map` costs a frame, not a host stack frame, and the
//! depth is bounded by `maxCallDepth` like any other call.
//!
//! **Costs.** Section 13.1 gives each function a cost in steps: `1` for a
//! constant one, and `n`, meaning `1 + floor(N / 64)`, for one that reads
//! `N` elements or bytes. Every function here charges before it does the
//! work. A result whose size is known in advance is charged to the memory
//! counter before it is built (section 11.4), which is what stops
//! `"x".repeat(1000000000)` from being built at all.

use crate::heap::Heap;
use crate::interp::{Interpreter, charge_steps_sliced};
use crate::limits::RunCounters;
use crate::value::{MapEntry, MapKey, Object, Value};
use hollowscript_bytecode::module::Module;
use hollowscript_core::Limits;
use hollowscript_core::cancel::Control;
use hollowscript_core::fault::{Fault, FaultCode};
use hollowscript_core::native::ROSTER;

/// Whether this build implements the roster entry called `name`.
///
/// The three groups that are not implemented are named here rather than
/// discovered at a call, so that a program that uses one is refused when it
/// is loaded, with the name in the message.
#[must_use]
pub fn is_implemented(name: &str) -> bool {
    // Every roster entry is implemented. The function is kept because
    // `resolve_imports` asks it, and because the next entry added to the
    // roster should be refused by name until its body exists rather than
    // faulting somewhere inside the interpreter.
    let _ = name;
    true
}

/// Resolves a module's native import table to roster positions.
///
/// # Errors
///
/// Returns a fault naming the first import whose name is not in the roster,
/// whose arity differs from the roster's, or which this build does not
/// implement.
pub fn resolve_imports(module: &Module) -> Result<Vec<u32>, Fault> {
    let mut out = Vec::with_capacity(module.natives.len());
    for import in &module.natives {
        let name = module.string(import.name);
        let position = ROSTER
            .binary_search_by_key(&name, |info| info.name)
            .map_err(|_| {
                Fault::internal(&format!("the standard library has no function '{name}'"))
            })?;
        let info = ROSTER
            .get(position)
            .ok_or_else(|| Fault::internal("a roster position is out of range"))?;
        if info.arity != import.arity {
            return Err(Fault::internal(&format!(
                "'{name}' takes {} arguments, the module asks for {}",
                info.arity, import.arity
            )));
        }
        if !is_implemented(name) {
            return Err(Fault::internal(&format!(
                "the standard library function '{name}' is not implemented yet"
            )));
        }
        out.push(u32::try_from(position).map_err(|_| Fault::internal("too many natives"))?);
    }
    Ok(out)
}

/// The cost `n` of section 13.1: `1 + floor(N / 64)` steps.
fn charge_n(
    counters: &mut RunCounters,
    limits: &Limits,
    control: &mut Control<'_>,
    n: u64,
) -> Result<(), Fault> {
    charge_steps_sliced(counters, limits, control, 1 + n / 64)
}

/// A constant cost of one step.
fn charge_one(
    counters: &mut RunCounters,
    limits: &Limits,
    control: &mut Control<'_>,
) -> Result<(), Fault> {
    charge_steps_sliced(counters, limits, control, 1)
}

/// The argument at `index`, or an internal fault: the verifier has already
/// checked the argument count against the roster's arity, so a missing one
/// is a defect in this interpreter.
fn arg(args: &[Value], index: usize) -> Result<Value, Fault> {
    args.get(index)
        .copied()
        .ok_or_else(|| Fault::internal("a library function was called with too few arguments"))
}

fn want_int(value: Value) -> Result<i64, Fault> {
    match value {
        Value::Int(i) => Ok(i),
        _ => Err(Fault::internal("a library function needed an Int")),
    }
}

/// A `Float` argument, accepting an `Int` because section 13.1 converts one
/// passed for a `Float` parameter.
fn want_float(value: Value) -> Result<f64, Fault> {
    match value {
        Value::Float(f) => Ok(f),
        // Section 2.7: an `Int` used where a `Float` is expected is
        // converted to the nearest `Float`, which is exactly this cast.
        #[allow(
            clippy::cast_precision_loss,
            reason = "section 2.7's conversion is to the nearest Float"
        )]
        Value::Int(i) => Ok(i as f64),
        _ => Err(Fault::internal("a library function needed a Float")),
    }
}

fn want_str(heap: &Heap, value: Value) -> Result<&str, Fault> {
    match value {
        Value::Heap(h) => match heap.get(h) {
            Some(Object::Str(s)) => Ok(s),
            _ => Err(Fault::internal("a library function needed a String")),
        },
        _ => Err(Fault::internal("a library function needed a String")),
    }
}

fn want_list(heap: &Heap, value: Value) -> Result<&[Value], Fault> {
    match value {
        Value::Heap(h) => match heap.get(h) {
            Some(Object::List(items)) => Ok(items),
            _ => Err(Fault::internal("a library function needed a list")),
        },
        _ => Err(Fault::internal("a library function needed a list")),
    }
}

fn want_map(heap: &Heap, value: Value) -> Result<&[MapEntry], Fault> {
    match value {
        Value::Heap(h) => match heap.get(h) {
            Some(Object::Map(entries)) => Ok(entries),
            _ => Err(Fault::internal("a library function needed a Map")),
        },
        _ => Err(Fault::internal("a library function needed a Map")),
    }
}

fn want_set(heap: &Heap, value: Value) -> Result<&[MapKey], Fault> {
    match value {
        Value::Heap(h) => match heap.get(h) {
            Some(Object::Set(items)) => Ok(items),
            _ => Err(Fault::internal("a library function needed a Set")),
        },
        _ => Err(Fault::internal("a library function needed a Set")),
    }
}

/// An `Int` result that does not fit (section 13.1: HS1007).
fn int_overflow() -> Fault {
    Fault::new(FaultCode::IntOverflow, &[])
}

/// A `Float` result that is not a number (HS1009).
fn not_a_number() -> Fault {
    Fault::new(FaultCode::NotANumber, &[])
}

/// A `Float` result that is infinite (HS1010).
fn infinite() -> Fault {
    Fault::new(FaultCode::Infinite, &[])
}

/// A `Float` result, refused if it is not finite (section 13.1).
fn finite(value: f64) -> Result<Value, Fault> {
    if value.is_nan() {
        return Err(not_a_number());
    }
    if value.is_infinite() {
        return Err(infinite());
    }
    // Section 13.2: "`Float` results are never negative zero."
    Ok(Value::Float(if value == 0.0 { 0.0 } else { value }))
}

/// The size in bytes of `needed` code points of `fill` repeated from its
/// start, which is what `padStart` and `padEnd` add, worked out without
/// building it.
fn padding_bytes(fill: &str, needed: usize) -> u64 {
    let chars = fill.chars().count();
    if chars == 0 {
        return 0;
    }
    let whole = (needed / chars) as u64;
    let rest: usize = fill.chars().take(needed % chars).map(char::len_utf8).sum();
    whole
        .saturating_mul(fill.len() as u64)
        .saturating_add(rest as u64)
}

/// A position or length counted in code points, clamped into `0 ..= length`.
fn clamp_position(position: i64, length: usize) -> usize {
    let length = i64::try_from(length).unwrap_or(i64::MAX);
    usize::try_from(position.clamp(0, length)).unwrap_or(0)
}

/// The byte offset of code point `index` of `text`, or its length.
fn byte_of_char(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(offset, _)| offset)
}

/// The memory a list of `count` strings holding `total_bytes` between them
/// would cost, matching `Heap::object_node_cost` exactly (16 + 8 per list
/// element, 16 + its length per string): worked out so `chars`, `split` and
/// `lines` can charge it before any of the list or its strings is built,
/// rather than after splitting a receiver into many small pieces has already
/// multiplied it several times over in per-object overhead alone.
fn list_of_strings_memory(count: u64, total_bytes: u64) -> u64 {
    16u64
        .saturating_add(count.saturating_mul(8 + 16))
        .saturating_add(total_bytes)
}

/// The number of lines `split_lines` would return for `text`, and the total
/// bytes they would hold between them, worked out without building any of
/// them.
fn lines_stats(text: &str) -> (u64, u64) {
    if text.is_empty() {
        return (0, 0);
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let body = body.strip_suffix('\r').unwrap_or(body);
    if body.is_empty() {
        return (1, 0);
    }
    let mut count = 0u64;
    let mut bytes = 0u64;
    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        count += 1;
        bytes += line.len() as u64;
    }
    (count, bytes)
}

impl Interpreter {
    /// Calls the roster entry at `position` with `args` (the receiver first
    /// for a method), charging its cost as section 13.1 states.
    ///
    /// # Errors
    ///
    /// The fault the function itself raises, or a limit fault from its own
    /// charges.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per library function, in roster order"
    )]
    pub(crate) fn call_native(
        &mut self,
        position: u32,
        args: &[Value],
        append_in_place: bool,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<NativeOutcome, Fault> {
        let name = ROSTER
            .get(position as usize)
            .ok_or_else(|| Fault::internal("a native index is out of range"))?
            .name;
        if takes_a_callback(name) {
            // Callback jobs retain their arguments while another script
            // frame runs, so they count as escaped aliases for append.
            self.mark_escaped_values(args);
            return self
                .start_callback_native(position, name, args, limits, control, counters)
                .map(|job| NativeOutcome::Job(Box::new(job)));
        }
        self.call_plain_native(name, args, append_in_place, limits, control, counters)
            .map(NativeOutcome::Value)
    }

    /// Every library function that does not call a closure.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per library function, in roster order"
    )]
    fn call_plain_native(
        &mut self,
        name: &str,
        args: &[Value],
        append_in_place: bool,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Value, Fault> {
        match name {
            // -- Section 13.2, numbers. All cost 1 except `range`. --
            "abs" => {
                charge_one(counters, limits, control)?;
                match arg(args, 0)? {
                    Value::Int(i) => i.checked_abs().map(Value::Int).ok_or_else(int_overflow),
                    other => finite(want_float(other)?.abs()),
                }
            }
            "sign" => {
                charge_one(counters, limits, control)?;
                match arg(args, 0)? {
                    Value::Int(i) => Ok(Value::Int(i.signum())),
                    other => {
                        let x = want_float(other)?;
                        // Section 13.2 gives `sign(x: Float): Int`.
                        Ok(Value::Int(if x > 0.0 {
                            1
                        } else if x < 0.0 {
                            -1
                        } else {
                            0
                        }))
                    }
                }
            }
            "min" | "max" => {
                charge_one(counters, limits, control)?;
                let a = arg(args, 0)?;
                let b = arg(args, 1)?;
                // Section 13.1: the `Int` signature is used when every
                // argument is an `Int`, and otherwise the `Float` one.
                if let (Value::Int(x), Value::Int(y)) = (a, b) {
                    let first = if name == "min" { x <= y } else { x >= y };
                    return Ok(if first { a } else { b });
                }
                let (x, y) = (want_float(a)?, want_float(b)?);
                let first = if name == "min" { x <= y } else { x >= y };
                finite(if first { x } else { y })
            }
            "clamp" => {
                charge_one(counters, limits, control)?;
                let (x, low, high) = (arg(args, 0)?, arg(args, 1)?, arg(args, 2)?);
                if let (Value::Int(x), Value::Int(low), Value::Int(high)) = (x, low, high) {
                    if low > high {
                        return Err(Fault::invalid_argument("clamp", "low is above high"));
                    }
                    return Ok(Value::Int(x.clamp(low, high)));
                }
                let (x, low, high) = (want_float(x)?, want_float(low)?, want_float(high)?);
                if low > high {
                    return Err(Fault::invalid_argument("clamp", "low is above high"));
                }
                finite(x.clamp(low, high))
            }
            "round" | "floor" | "ceil" | "trunc" => {
                charge_one(counters, limits, control)?;
                let x = want_float(arg(args, 0)?)?;
                let rounded = match name {
                    // Section 13.2: halves away from zero, which is what
                    // `f64::round` does and `round_ties_even` does not.
                    "round" => x.round(),
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    _ => x.trunc(),
                };
                if !rounded.is_finite() || !in_int_range(rounded) {
                    return Err(int_overflow());
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "in_int_range has just checked the range, and the value has no fractional part"
                )]
                Ok(Value::Int(rounded as i64))
            }
            "toFloat" => {
                charge_one(counters, limits, control)?;
                let n = want_int(arg(args, 0)?)?;
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "section 13.2: toFloat is the nearest Float"
                )]
                finite(n as f64)
            }
            "div" | "mod" | "remainder" => {
                charge_one(counters, limits, control)?;
                let a = want_int(arg(args, 0)?)?;
                let b = want_int(arg(args, 1)?)?;
                if b == 0 {
                    return Err(Fault::new(FaultCode::DivisionByZero, &[]));
                }
                match name {
                    "div" => a
                        .checked_div_euclid(b)
                        .map_or_else(|| Err(int_overflow()), |_| Ok(Value::Int(floor_div(a, b)))),
                    // Section 13.2: `mod(-9223372036854775808, -1)` is 0 and
                    // `remainder` of the same is 0, so neither overflows.
                    "mod" => Ok(Value::Int(floor_mod(a, b))),
                    _ => Ok(Value::Int(a.wrapping_rem(b))),
                }
            }
            "ipow" => {
                charge_one(counters, limits, control)?;
                let base = want_int(arg(args, 0)?)?;
                let exponent = want_int(arg(args, 1)?)?;
                integer_power(base, exponent).map(Value::Int)
            }
            "sqrt" => {
                charge_one(counters, limits, control)?;
                let x = want_float(arg(args, 0)?)?;
                if x < 0.0 {
                    return Err(not_a_number());
                }
                finite(x.sqrt())
            }
            "pow" => {
                charge_one(counters, limits, control)?;
                let base = want_float(arg(args, 0)?)?;
                let exponent = want_float(arg(args, 1)?)?;
                if exponent == 0.0 {
                    return Ok(Value::Float(1.0));
                }
                if base < 0.0 && exponent.fract() != 0.0 {
                    return Err(not_a_number());
                }
                finite(base.powf(exponent))
            }
            "exp" => {
                charge_one(counters, limits, control)?;
                finite(want_float(arg(args, 0)?)?.exp())
            }
            "ln" => {
                charge_one(counters, limits, control)?;
                let x = want_float(arg(args, 0)?)?;
                if x < 0.0 {
                    return Err(not_a_number());
                }
                if x == 0.0 {
                    return Err(infinite());
                }
                finite(x.ln())
            }
            "sin" | "cos" | "tan" => {
                charge_one(counters, limits, control)?;
                let x = want_float(arg(args, 0)?)?;
                finite(match name {
                    "sin" => x.sin(),
                    "cos" => x.cos(),
                    _ => x.tan(),
                })
            }
            "atan2" => {
                charge_one(counters, limits, control)?;
                let y = want_float(arg(args, 0)?)?;
                let x = want_float(arg(args, 1)?)?;
                finite(y.atan2(x))
            }
            "range" => {
                let start = want_int(arg(args, 0)?)?;
                let up_to = want_int(arg(args, 1)?)?;
                let length = (i128::from(up_to) - i128::from(start)).max(0);
                let length_u64 = u64::try_from(length).unwrap_or(u64::MAX);
                charge_n(counters, limits, control, length_u64)?;
                // Section 11.4: the size is known, so it is charged before
                // the list is built and an astronomical range never is.
                counters
                    .charge_memory(16u64.saturating_add(length_u64.saturating_mul(8)), limits)?;
                let count = usize::try_from(length)
                    .map_err(|_| Fault::internal("a range longer than this host can hold"))?;
                let items = (0..count)
                    .map(|i| Value::Int(start.wrapping_add(i64::try_from(i).unwrap_or(i64::MAX))))
                    .collect();
                self.alloc_object(Object::List(items))
            }

            // -- Section 13.4, numbers and text. --
            // Section 13.4: `Result<Int, String>` and `Result<Float, String>`,
            // with the error text the section fixes.
            "parseInt" | "parseFloat" => {
                let text = want_str(&self.heap, arg(args, 0)?)?.to_owned();
                charge_n(counters, limits, control, text.len() as u64)?;
                let parsed = if name == "parseInt" {
                    parse_int(&text).map(Value::Int)
                } else {
                    parse_float(&text).map(Value::Float)
                };
                match parsed {
                    Ok(value) => self.result_value(true, value, counters, limits),
                    Err(message) => {
                        let message =
                            self.alloc_string_charged(message.to_owned(), counters, limits)?;
                        self.result_value(false, message, counters, limits)
                    }
                }
            }
            "fixed" => {
                let x = want_float(arg(args, 0)?)?;
                let digits = want_int(arg(args, 1)?)?;
                if !(0..=100).contains(&digits) {
                    return Err(Fault::invalid_argument(
                        "fixed",
                        "digits must be from 0 to 100",
                    ));
                }
                let text = fixed_text(x, usize::try_from(digits).unwrap_or(0));
                charge_n(counters, limits, control, text.len() as u64)?;
                self.alloc_string_charged(text, counters, limits)
            }

            // -- Section 13.5, records. --
            "keys" => {
                let Value::Heap(h) = arg(args, 0)? else {
                    return Err(Fault::internal("keys needs a record"));
                };
                let names: Vec<String> = match self.heap.get(h) {
                    Some(Object::Record { shape, .. }) => shape
                        .iter()
                        .map(|id| self.module().module().string(*id).to_owned())
                        .collect(),
                    _ => return Err(Fault::internal("keys needs a record")),
                };
                charge_n(counters, limits, control, names.len() as u64)?;
                let mut items = Vec::with_capacity(names.len());
                for name in names {
                    items.push(self.alloc_string_charged(name, counters, limits)?);
                }
                self.alloc_charged(Object::List(items), counters, limits)
            }
            "has" => {
                charge_one(counters, limits, control)?;
                let receiver = arg(args, 0)?;
                let wanted = want_str(&self.heap, arg(args, 1)?)?.to_owned();
                // Section 14.2 also gives `Map` a `has`; the roster has one
                // entry, so the receiver decides which is meant.
                let Value::Heap(h) = receiver else {
                    return Err(Fault::internal("has needs a record or a Map"));
                };
                match self.heap.get(h) {
                    Some(Object::Record { shape, .. }) => {
                        let present = shape
                            .iter()
                            .any(|id| self.module().module().string(*id) == wanted);
                        Ok(Value::Bool(present))
                    }
                    Some(Object::Map(_)) => {
                        let key = self.to_map_key(arg(args, 1)?)?;
                        let entries = want_map(&self.heap, receiver)?;
                        Ok(Value::Bool(self.find_entry(entries, key).is_some()))
                    }
                    _ => Err(Fault::internal("has needs a record or a Map")),
                }
            }

            _ if name.starts_with("json.") || name.starts_with("time.") => {
                self.call_data_native(name, args, limits, control, counters)
            }
            _ => {
                self.call_collection_native(name, args, append_in_place, limits, control, counters)
            }
        }
    }

    /// Chapter 15: `json.*` and `time.*`.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per library function, in roster order"
    )]
    fn call_data_native(
        &mut self,
        name: &str,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Value, Fault> {
        match name {
            "json.parse" => {
                let text = want_str(&self.heap, arg(args, 0)?)?;
                // Charged and safepointed as the parse itself proceeds
                // (`json::parse_charged`), rather than as one lump before
                // it starts: a lump charged up front still let a slow or
                // adversarial document run for as long as it liked once
                // that one charge had passed, with nothing polling
                // cancellation again until it returned.
                match crate::json::parse_charged(text, counters, limits, control) {
                    Ok(parsed) => {
                        let value = self.json_to_value(&parsed, counters, limits)?;
                        self.result_value(true, value, counters, limits)
                    }
                    Err(crate::json::Outcome::Json(error)) => {
                        let record =
                            self.json_error_value(error.message, error.offset, counters, limits)?;
                        self.result_value(false, record, counters, limits)
                    }
                    Err(crate::json::Outcome::Fault(fault)) => Err(fault),
                }
            }
            "json.stringify" | "json.pretty" => {
                let pretty = name == "json.pretty";
                let indent = if pretty {
                    let indent = want_int(arg(args, 1)?)?;
                    if !(1..=16).contains(&indent) {
                        return Err(Fault::invalid_argument(
                            "pretty",
                            "the indent must be from 1 to 16",
                        ));
                    }
                    usize::try_from(indent).unwrap_or(1)
                } else {
                    0
                };
                let json = {
                    let types = self.data_types().clone();
                    let caller = if pretty { "pretty" } else { "stringify" };
                    crate::data::json_from_value(
                        &types,
                        &self.heap,
                        arg(args, 0)?,
                        caller,
                        counters,
                        limits,
                        control,
                    )?
                };
                // The exact output size is worked out from `json` without
                // writing any of it, and charged first, so a document large
                // enough to fault the run never gets as far as being built.
                let len = crate::json::write_len(&json, if pretty { Some(indent) } else { None });
                charge_n(counters, limits, control, len)?;
                self.alloc_sized_string(
                    len,
                    || {
                        if pretty {
                            crate::json::pretty(&json, indent)
                        } else {
                            crate::json::stringify(&json)
                        }
                    },
                    counters,
                    limits,
                )
            }
            "json.field" | "json.at" => {
                charge_one(counters, limits, control)?;
                // Read straight from the heap: neither function needs
                // `value`'s whole tree converted, only whether it is the
                // right container and, if so, the one member asked for,
                // which is already a `Json` value sitting on the heap
                // exactly as it should be returned. See `json_member`'s own
                // doc comment for why this matters: the old code converted
                // the whole value first, only to throw all but one member
                // away, both costing more and, for a value deep enough,
                // not always able to finish dropping the conversion.
                let types = self.data_types().clone();
                let member = if name == "json.field" {
                    crate::data::JsonMember::Field(want_str(&self.heap, arg(args, 1)?)?)
                } else {
                    crate::data::JsonMember::At(want_int(arg(args, 1)?)?)
                };
                match crate::data::json_member(&types, &self.heap, arg(args, 0)?, member)? {
                    None => Ok(Value::None),
                    Some(value) => Ok(value),
                }
            }
            "time.parse" | "time.parseDate" => {
                let text = want_str(&self.heap, arg(args, 0)?)?.to_owned();
                charge_n(counters, limits, control, text.len() as u64)?;
                let outcome = if name == "time.parse" {
                    crate::time::parse(&text)
                } else {
                    crate::time::parse_date(&text)
                };
                match outcome {
                    Ok(value) => self.result_value(true, Value::Int(value), counters, limits),
                    Err(error) => {
                        let message = match (name, error) {
                            ("time.parseDate", _) => "invalid date",
                            (_, crate::time::TimeError::Invalid) => "invalid timestamp",
                            (_, crate::time::TimeError::OutOfRange) => "timestamp out of range",
                        };
                        let text =
                            self.alloc_string_charged(message.to_owned(), counters, limits)?;
                        self.result_value(false, text, counters, limits)
                    }
                }
            }
            "time.format" | "time.formatDate" => {
                charge_one(counters, limits, control)?;
                let value = want_int(arg(args, 0)?)?;
                let text = if name == "time.format" {
                    crate::time::format(value)
                } else {
                    crate::time::format_date(value)
                };
                let text = text.ok_or_else(|| {
                    Fault::invalid_argument(name, "the value is outside the valid range")
                })?;
                self.alloc_string_charged(text, counters, limits)
            }
            "time.parts" => {
                charge_one(counters, limits, control)?;
                let instant = want_int(arg(args, 0)?)?;
                let parts = crate::time::parts(instant).ok_or_else(|| {
                    Fault::invalid_argument("parts", "the instant is outside the valid range")
                })?;
                self.time_parts_value(&parts, counters, limits)
            }
            "time.fromParts" => {
                charge_one(counters, limits, control)?;
                let parts = self.time_parts_of(arg(args, 0)?)?;
                if let Some(instant) = crate::time::from_parts(&parts) {
                    return self.result_value(true, Value::Int(instant), counters, limits);
                }
                let text =
                    self.alloc_string_charged("invalid time parts".to_owned(), counters, limits)?;
                self.result_value(false, text, counters, limits)
            }
            _ => Err(Fault::internal(&format!(
                "the standard library function '{name}' is not implemented yet"
            ))),
        }
    }

    /// `Ok(value)` or `Err(value)`, using the module's own `Result`
    /// variants, which the compiler puts there whenever the program calls a
    /// function that returns one.
    fn result_value(
        &mut self,
        ok: bool,
        value: Value,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        let wanted = if ok { "Ok" } else { "Err" };
        let module = self.module().module();
        let variant = module
            .variants
            .iter()
            .position(|v| module.string(v.enum_name) == "Result" && module.string(v.name) == wanted)
            .and_then(|index| u32::try_from(index).ok())
            .ok_or_else(|| {
                Fault::internal("this module has no Result type for a library function to build")
            })?;
        self.alloc_charged(
            Object::Enum {
                variant,
                payload: vec![value],
            },
            counters,
            limits,
        )
    }
}

/// Whether a `Float` that is already truncated to an integer fits an `Int`.
fn in_int_range(value: f64) -> bool {
    // 2^63 is not representable as an `i64`, so the upper bound is exclusive.
    (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value)
}

/// Floor division (section 13.2), with `i64::MIN / -1` left to the caller.
fn floor_div(a: i64, b: i64) -> i64 {
    let quotient = a.wrapping_div(b);
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        quotient.wrapping_sub(1)
    } else {
        quotient
    }
}

/// Floor modulo: `a - b * div(a, b)`, with the sign of `b`.
fn floor_mod(a: i64, b: i64) -> i64 {
    let remainder = a.wrapping_rem(b);
    if remainder != 0 && ((remainder < 0) != (b < 0)) {
        remainder.wrapping_add(b)
    } else {
        remainder
    }
}

/// `base` to the power `exponent` (section 13.2), faulting only when the
/// mathematical result is outside `Int`: no intermediate value may fault, so
/// `ipow(-2, 63)` is `i64::MIN` and `ipow(1, e)` and `ipow(-1, e)` never
/// fault however large `e` is.
fn integer_power(base: i64, exponent: i64) -> Result<i64, Fault> {
    if exponent < 0 {
        return Err(Fault::invalid_argument(
            "ipow",
            "the exponent must not be negative",
        ));
    }
    match base {
        0 => return Ok(i64::from(exponent == 0)),
        1 => return Ok(1),
        -1 => return Ok(if exponent % 2 == 0 { 1 } else { -1 }),
        _ => {}
    }
    // The magnitude of the base is at least 2 from here, so an exponent
    // above 63 is certainly out of range and needs no multiplying.
    if exponent > 63 {
        return Err(int_overflow());
    }
    let low = i128::from(i64::MIN);
    let high = i128::from(i64::MAX);
    let mut result: i128 = 1;
    let factor = i128::from(base);
    for _ in 0..exponent {
        result *= factor;
        if result < low || result > high {
            return Err(int_overflow());
        }
    }
    i64::try_from(result).map_err(|_| int_overflow())
}

/// `parseInt` (section 13.4): an optional `+` or `-` and one or more ASCII
/// digits, nothing else, leading zeros allowed.
fn parse_int(text: &str) -> Result<i64, &'static str> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid integer");
    }
    // The form is already checked, so the only way left to fail is range.
    text.strip_prefix('+')
        .unwrap_or(text)
        .parse()
        .map_err(|_| "integer out of range")
}

/// `parseFloat` (section 13.4): an optional sign, digits, an optional `.`
/// and digits, an optional exponent, nothing else. No `.5`, `5.`, `nan`,
/// `inf`, hexadecimal or underscore form.
fn parse_float(text: &str) -> Result<f64, &'static str> {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (body, None),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (mantissa, None),
    };
    let digits_only = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits_only(whole) {
        return Err("invalid number");
    }
    if let Some(fraction) = fraction {
        if !digits_only(fraction) {
            return Err("invalid number");
        }
    }
    if let Some(exponent) = exponent {
        let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if !digits_only(digits) {
            return Err("invalid number");
        }
    }
    let value: f64 = text
        .strip_prefix('+')
        .unwrap_or(text)
        .parse()
        .map_err(|_| "invalid number")?;
    if value.is_infinite() {
        // Section 13.4: a value that would be infinite is out of range.
        return Err("number out of range");
    }
    // "a parsed negative zero (`-0`, `-0.0`) is `0.0`"
    Ok(if value == 0.0 { 0.0 } else { value })
}

/// `fixed` (section 13.4): the exact value of `x` in positional notation
/// with exactly `digits` fractional digits, halves away from zero, and a
/// `-` sign only when the rounded value is not zero.
fn fixed_text(x: f64, digits: usize) -> String {
    // Rust's own `{:.*}` rounds halves to even on the exact binary value,
    // and section 13.4 rounds them away from zero, so the two differ for a
    // value that is exactly a half at the rounding position. Those are
    // exactly the values with a short binary fraction, so comparing the
    // two roundings of the exact decimal expansion settles it.
    let text = format!("{:.*}", digits, x.abs());
    let rounded_away = round_half_away(x.abs(), digits).unwrap_or(text);
    if rounded_away
        .bytes()
        .all(|b| !b.is_ascii_digit() || b == b'0')
    {
        // The rounded value is zero, so it carries no sign.
        return rounded_away;
    }
    if x.is_sign_negative() {
        return format!("-{rounded_away}");
    }
    rounded_away
}

/// The decimal expansion of the exact value of a non-negative `x`, rounded
/// to `digits` fractional digits with halves away from zero. `None` when the
/// value is too large for the exact expansion to be worth building, in which
/// case no rounding is needed anyway.
fn round_half_away(x: f64, digits: usize) -> Option<String> {
    if !x.is_finite() || x >= 1e18 {
        return None;
    }
    // Two more digits than asked for is enough to see whether the exact
    // value sits above, below or exactly at the halfway point: a `f64` whose
    // magnitude is below 1e18 has an exact decimal expansion, and
    // `{:.*}` with enough digits prints it.
    let exact = format!("{:.*}", digits + 25, x);
    let (whole, fraction) = exact.split_once('.')?;
    let kept = fraction.get(..digits)?;
    let rest = fraction.get(digits..)?;
    let round_up = match rest.as_bytes().first() {
        Some(b'5') => true,
        Some(b) if *b > b'5' => true,
        _ => false,
    };
    let mut result: Vec<u8> = format!("{whole}{kept}").into_bytes();
    if round_up {
        let mut index = result.len();
        loop {
            if index == 0 {
                result.insert(0, b'1');
                break;
            }
            index -= 1;
            let digit = result.get_mut(index)?;
            if *digit == b'9' {
                *digit = b'0';
            } else {
                *digit += 1;
                break;
            }
        }
    }
    let text = String::from_utf8(result).ok()?;
    if digits == 0 {
        return Some(text);
    }
    let split = text.len().checked_sub(digits)?;
    let (whole, fraction) = text.split_at(split);
    let whole = if whole.is_empty() { "0" } else { whole };
    Some(format!("{whole}.{fraction}"))
}

impl Interpreter {
    /// The position of `key` in a sorted entry list, if it is there.
    fn find_entry(&self, entries: &[MapEntry], key: MapKey) -> Option<usize> {
        entries
            .binary_search_by(|entry| self.heap.compare_keys(entry.key, key))
            .ok()
    }

    /// The position of `key` in a sorted element list, if it is there.
    fn find_element(&self, items: &[MapKey], key: MapKey) -> Option<usize> {
        items
            .binary_search_by(|item| self.heap.compare_keys(*item, key))
            .ok()
    }

    /// Where `key` belongs in a sorted list, present or not.
    fn place_of(&self, items: &[MapKey], key: MapKey) -> usize {
        items
            .binary_search_by(|item| self.heap.compare_keys(*item, key))
            .unwrap_or_else(|index| index)
    }

    /// Allocates a string whose size is charged before it is built, for the
    /// functions whose result can be astronomically larger than their input
    /// (`repeat`, `padStart`, `padEnd`, `join`).
    fn alloc_sized_string(
        &mut self,
        bytes: u64,
        build: impl FnOnce() -> String,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        counters.charge_memory(bytes.saturating_add(16), limits)?;
        self.alloc_object(Object::Str(build()))
    }

    /// The library functions of chapters 8 and 14: lists, maps, sets, strings
    /// and `Result`.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per library function, in roster order"
    )]
    fn call_collection_native(
        &mut self,
        name: &str,
        args: &[Value],
        append_in_place: bool,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Value, Fault> {
        let receiver = arg(args, 0)?;
        match name {
            // -- Section 14.1, lists. --
            "list.length" => {
                charge_one(counters, limits, control)?;
                let length = want_list(&self.heap, receiver)?.len();
                Ok(Value::Int(i64::try_from(length).unwrap_or(i64::MAX)))
            }
            "list.isEmpty" => {
                charge_one(counters, limits, control)?;
                Ok(Value::Bool(want_list(&self.heap, receiver)?.is_empty()))
            }
            "list.first" => {
                charge_one(counters, limits, control)?;
                Ok(want_list(&self.heap, receiver)?
                    .first()
                    .copied()
                    .unwrap_or(Value::None))
            }
            "list.last" => {
                charge_one(counters, limits, control)?;
                Ok(want_list(&self.heap, receiver)?
                    .last()
                    .copied()
                    .unwrap_or(Value::None))
            }
            "list.contains" | "list.indexOf" => {
                let items = want_list(&self.heap, receiver)?.to_vec();
                let wanted = arg(args, 1)?;
                // Section 14.1: the charge is N x (1 + t), so that it covers
                // the comparisons whatever their result. Bounded by the
                // step limit scaled back to bytes: once `wanted` alone (a
                // self-referential or heavily shared value, cycle-safe and
                // deduplicated by `tree_size_bounded`) already costs more
                // than the run can afford, the exact figure stops mattering.
                let tree = self
                    .heap
                    .tree_size_bounded(wanted, limits.max_steps.saturating_mul(64));
                let n = (items.len() as u64).saturating_mul(1u64.saturating_add(tree));
                charge_n(counters, limits, control, n)?;
                // Each element is compared with its own walk, which pays for
                // any pairs past what `tree` bytes account for (see
                // `Interpreter::values_equal`), so a doubled structure is
                // neither exponential nor out of the step limit's reach.
                let allowance = tree / 8 + 1;
                let mut found = None;
                for (index, &item) in items.iter().enumerate() {
                    if self.values_equal(item, wanted, allowance, counters, limits, control)? {
                        found = Some(index);
                        break;
                    }
                }
                if name == "list.contains" {
                    return Ok(Value::Bool(found.is_some()));
                }
                Ok(found.map_or(Value::None, |index| {
                    Value::Int(i64::try_from(index).unwrap_or(i64::MAX))
                }))
            }
            "list.append" => {
                let Value::Heap(h) = receiver else {
                    return Err(Fault::internal("append needs a list"));
                };
                let length = want_list(&self.heap, receiver)?.len();
                charge_n(counters, limits, control, length as u64)?;
                let appended = arg(args, 1)?;
                counters.charge_memory(8, limits)?;
                if append_in_place && self.heap.append_list_in_place(h, appended) {
                    return Ok(Value::Heap(h));
                }
                let items = want_list(&self.heap, receiver)?;
                // Sized to its final length up front, so the one copy of
                // `items` this needs is the only one: `items.to_vec()`
                // followed by `push` would allocate exactly `items.len()`
                // capacity for the copy and then grow again (a second copy
                // of every element) the moment `push` found no room for the
                // one it adds.
                let mut out = Vec::with_capacity(items.len() + 1);
                out.extend_from_slice(items);
                out.push(appended);
                self.alloc_object(Object::List(out))
            }
            "list.concat" => {
                let items = want_list(&self.heap, receiver)?;
                let other = want_list(&self.heap, arg(args, 1)?)?;
                let length = (items.len() + other.len()) as u64;
                charge_n(counters, limits, control, length)?;
                counters.charge_memory(16u64.saturating_add(length.saturating_mul(8)), limits)?;
                // One allocation sized for both halves, rather than cloning
                // `items` at its own length and then growing it again for
                // `other` (see `list.append`'s own comment above).
                let mut out = Vec::with_capacity(items.len() + other.len());
                out.extend_from_slice(items);
                out.extend_from_slice(other);
                self.alloc_object(Object::List(out))
            }
            "list.slice" | "list.take" | "list.drop" => {
                let items = want_list(&self.heap, receiver)?;
                let length = items.len();
                let (start, up_to) = match name {
                    "list.slice" => (
                        clamp_position(want_int(arg(args, 1)?)?, length),
                        clamp_position(want_int(arg(args, 2)?)?, length),
                    ),
                    "list.take" => (0, clamp_position(want_int(arg(args, 1)?)?, length)),
                    _ => (clamp_position(want_int(arg(args, 1)?)?, length), length),
                };
                let out: Vec<Value> = if start >= up_to {
                    Vec::new()
                } else {
                    items.get(start..up_to).unwrap_or(&[]).to_vec()
                };
                charge_n(counters, limits, control, out.len() as u64)?;
                self.alloc_charged(Object::List(out), counters, limits)
            }
            "list.reverse" => {
                let items = want_list(&self.heap, receiver)?;
                charge_n(counters, limits, control, items.len() as u64)?;
                let out: Vec<Value> = items.iter().rev().copied().collect();
                self.alloc_charged(Object::List(out), counters, limits)
            }
            "list.sort" => {
                let items = want_list(&self.heap, receiver)?.to_vec();
                let n = items.len() as u64;
                // Section 14.1: sorting is charged for N * ceil(log2(N + 1))
                // element steps.
                charge_n(
                    counters,
                    limits,
                    control,
                    n.saturating_mul(ceil_log2(n.saturating_add(1))),
                )?;
                let mut out = items;
                // A stable sort, as section 14.1 requires, over the natural
                // order of `Int`, `Float` and `String`.
                let mut failure = None;
                out.sort_by(|&a, &b| {
                    self.compare_values(a, b).unwrap_or_else(|fault| {
                        failure.get_or_insert(fault);
                        std::cmp::Ordering::Equal
                    })
                });
                if let Some(fault) = failure {
                    return Err(fault);
                }
                self.alloc_charged(Object::List(out), counters, limits)
            }
            "list.join" => {
                let items = want_list(&self.heap, receiver)?.to_vec();
                let separator = want_str(&self.heap, arg(args, 1)?)?.to_owned();
                // Sizes are read from the heap without copying any piece: a
                // call that turns out to be over budget must not pay for
                // copies it will never use.
                let mut bytes: u64 = 0;
                for item in &items {
                    bytes = bytes.saturating_add(want_str(&self.heap, *item)?.len() as u64);
                }
                bytes = bytes.saturating_add(
                    separator.len() as u64 * (items.len().saturating_sub(1)) as u64,
                );
                // Section 13.1: N is never less than the elements read,
                // which join touches one at a time even when every piece
                // and the separator are empty.
                let n = bytes.max(items.len() as u64);
                charge_n(counters, limits, control, n)?;
                let mut pieces = Vec::with_capacity(items.len());
                for item in items {
                    pieces.push(want_str(&self.heap, item)?.to_owned());
                }
                self.alloc_sized_string(bytes, || pieces.join(&separator), counters, limits)
            }

            // -- Section 14.2, maps. --
            "map.length" => {
                charge_one(counters, limits, control)?;
                let length = want_map(&self.heap, receiver)?.len();
                Ok(Value::Int(i64::try_from(length).unwrap_or(i64::MAX)))
            }
            "map.isEmpty" => {
                charge_one(counters, limits, control)?;
                Ok(Value::Bool(want_map(&self.heap, receiver)?.is_empty()))
            }
            "map.get" | "map.has" => {
                charge_one(counters, limits, control)?;
                let key = self.to_map_key(arg(args, 1)?)?;
                let entries = want_map(&self.heap, receiver)?;
                let found = self.find_entry(entries, key);
                if name == "map.has" {
                    return Ok(Value::Bool(found.is_some()));
                }
                Ok(found
                    .and_then(|index| entries.get(index))
                    .map_or(Value::None, |entry| entry.value))
            }
            "map.with" => {
                let key = self.to_map_key(arg(args, 1)?)?;
                let value = arg(args, 2)?;
                let entries = want_map(&self.heap, receiver)?;
                charge_n(counters, limits, control, entries.len() as u64)?;
                if let Some(index) = self.find_entry(entries, key) {
                    let mut out = entries.to_vec();
                    if let Some(entry) = out.get_mut(index) {
                        entry.value = value;
                    }
                    self.alloc_object(Object::Map(out))
                } else {
                    // Sized for the one new entry up front, so the copy of
                    // `entries` this needs is the only one: `to_vec()` then
                    // `insert` would grow (and so copy every entry again)
                    // the moment `insert` found no spare capacity, as
                    // `list.append`'s own comment above explains.
                    let mut out = Vec::with_capacity(entries.len() + 1);
                    out.extend_from_slice(entries);
                    let place = out
                        .binary_search_by(|entry| self.heap.compare_keys(entry.key, key))
                        .unwrap_or_else(|index| index);
                    out.insert(place, MapEntry { key, value });
                    // Section 11.4 charges `with` for what it adds.
                    counters.charge_memory(24, limits)?;
                    self.alloc_object(Object::Map(out))
                }
            }
            "map.without" => {
                let key = self.to_map_key(arg(args, 1)?)?;
                let entries = want_map(&self.heap, receiver)?;
                charge_n(counters, limits, control, entries.len() as u64)?;
                let mut out = entries.to_vec();
                if let Some(index) = self.find_entry(entries, key) {
                    out.remove(index);
                }
                self.alloc_charged(Object::Map(out), counters, limits)
            }
            "map.merge" => {
                let mine = want_map(&self.heap, receiver)?.to_vec();
                let theirs = want_map(&self.heap, arg(args, 1)?)?.to_vec();
                charge_n(
                    counters,
                    limits,
                    control,
                    (mine.len() + theirs.len()) as u64,
                )?;
                // Capacity for the worst case (no key in common) up front,
                // rather than letting the loop below grow `out` (and so
                // copy it) again each time `theirs` overflows its capacity.
                let mut out = mine;
                out.reserve_exact(theirs.len());
                for entry in theirs {
                    match out.binary_search_by(|e| self.heap.compare_keys(e.key, entry.key)) {
                        Ok(index) => {
                            if let Some(slot) = out.get_mut(index) {
                                *slot = entry;
                            }
                        }
                        Err(index) => out.insert(index, entry),
                    }
                }
                self.alloc_charged(Object::Map(out), counters, limits)
            }
            "map.keys" | "map.values" => {
                let entries = want_map(&self.heap, receiver)?;
                charge_n(counters, limits, control, entries.len() as u64)?;
                let out: Vec<Value> = if name == "map.keys" {
                    entries.iter().map(|e| key_as_value(e.key)).collect()
                } else {
                    entries.iter().map(|e| e.value).collect()
                };
                self.alloc_charged(Object::List(out), counters, limits)
            }
            "map.entries" => {
                let entries = want_map(&self.heap, receiver)?.to_vec();
                charge_n(counters, limits, control, entries.len() as u64)?;
                let mut out = Vec::with_capacity(entries.len());
                for entry in entries {
                    let record = self.alloc_charged(
                        Object::Record {
                            shape: Vec::new(),
                            fields: vec![key_as_value(entry.key), entry.value],
                        },
                        counters,
                        limits,
                    )?;
                    out.push(record);
                }
                self.alloc_charged(Object::List(out), counters, limits)
            }
            "mapFrom" => {
                let items = want_list(&self.heap, receiver)?.to_vec();
                charge_n(counters, limits, control, items.len() as u64)?;
                let mut out: Vec<MapEntry> = Vec::new();
                for item in items {
                    let Value::Heap(h) = item else {
                        return Err(Fault::internal("mapFrom needs a list of MapEntry"));
                    };
                    let (key, value) = match self.heap.get(h) {
                        Some(Object::Record { fields, .. }) if fields.len() == 2 => (
                            *fields
                                .first()
                                .ok_or_else(|| Fault::internal("a MapEntry has no key"))?,
                            *fields
                                .get(1)
                                .ok_or_else(|| Fault::internal("a MapEntry has no value"))?,
                        ),
                        _ => return Err(Fault::internal("mapFrom needs a list of MapEntry")),
                    };
                    let key = self.to_map_key(key)?;
                    match out.binary_search_by(|e| self.heap.compare_keys(e.key, key)) {
                        // "if a key occurs more than once, the later entry wins"
                        Ok(index) => {
                            if let Some(slot) = out.get_mut(index) {
                                slot.value = value;
                            }
                        }
                        Err(index) => out.insert(index, MapEntry { key, value }),
                    }
                }
                self.alloc_charged(Object::Map(out), counters, limits)
            }

            // -- Section 14.3, sets. --
            "set" => {
                let items = want_list(&self.heap, receiver)?.to_vec();
                charge_n(counters, limits, control, items.len() as u64)?;
                let mut out: Vec<MapKey> = Vec::new();
                for item in items {
                    let key = self.to_map_key(item)?;
                    if let Err(index) =
                        out.binary_search_by(|element| self.heap.compare_keys(*element, key))
                    {
                        out.insert(index, key);
                    }
                }
                self.alloc_charged(Object::Set(out), counters, limits)
            }
            "set.length" => {
                charge_one(counters, limits, control)?;
                let length = want_set(&self.heap, receiver)?.len();
                Ok(Value::Int(i64::try_from(length).unwrap_or(i64::MAX)))
            }
            "set.isEmpty" => {
                charge_one(counters, limits, control)?;
                Ok(Value::Bool(want_set(&self.heap, receiver)?.is_empty()))
            }
            "set.has" => {
                charge_one(counters, limits, control)?;
                let key = self.to_map_key(arg(args, 1)?)?;
                let items = want_set(&self.heap, receiver)?;
                Ok(Value::Bool(self.find_element(items, key).is_some()))
            }
            "set.with" => {
                let key = self.to_map_key(arg(args, 1)?)?;
                let items = want_set(&self.heap, receiver)?;
                charge_n(counters, limits, control, items.len() as u64)?;
                if self.find_element(items, key).is_some() {
                    self.alloc_object(Object::Set(items.to_vec()))
                } else {
                    // Sized for the one new element up front, so the copy
                    // of `items` this needs is the only one (see
                    // `list.append`'s own comment above).
                    let mut out = Vec::with_capacity(items.len() + 1);
                    out.extend_from_slice(items);
                    let place = self.place_of(&out, key);
                    out.insert(place, key);
                    // Section 11.4 charges `with` for what it adds.
                    counters.charge_memory(16, limits)?;
                    self.alloc_object(Object::Set(out))
                }
            }
            "set.without" => {
                let key = self.to_map_key(arg(args, 1)?)?;
                let items = want_set(&self.heap, receiver)?;
                charge_n(counters, limits, control, items.len() as u64)?;
                let mut out = items.to_vec();
                if let Some(index) = self.find_element(&out, key) {
                    out.remove(index);
                }
                self.alloc_charged(Object::Set(out), counters, limits)
            }
            "set.union" | "set.intersect" | "set.difference" | "set.isSubsetOf" => {
                let mine = want_set(&self.heap, receiver)?.to_vec();
                let theirs = want_set(&self.heap, arg(args, 1)?)?.to_vec();
                charge_n(
                    counters,
                    limits,
                    control,
                    (mine.len() + theirs.len()) as u64,
                )?;
                let in_theirs =
                    |me: &Interpreter, key: MapKey| me.find_element(&theirs, key).is_some();
                match name {
                    "set.isSubsetOf" => {
                        Ok(Value::Bool(mine.iter().all(|&key| in_theirs(self, key))))
                    }
                    "set.intersect" => {
                        let out: Vec<MapKey> = mine
                            .iter()
                            .copied()
                            .filter(|&key| in_theirs(self, key))
                            .collect();
                        self.alloc_charged(Object::Set(out), counters, limits)
                    }
                    "set.difference" => {
                        let out: Vec<MapKey> = mine
                            .iter()
                            .copied()
                            .filter(|&key| !in_theirs(self, key))
                            .collect();
                        self.alloc_charged(Object::Set(out), counters, limits)
                    }
                    _ => {
                        // Capacity for the worst case (nothing in common)
                        // up front, rather than growing (and so copying)
                        // `out` again partway through the loop below.
                        let mut out = mine;
                        out.reserve_exact(theirs.len());
                        for key in theirs {
                            if let Err(index) = out
                                .binary_search_by(|element| self.heap.compare_keys(*element, key))
                            {
                                out.insert(index, key);
                            }
                        }
                        self.alloc_charged(Object::Set(out), counters, limits)
                    }
                }
            }
            "set.toList" => {
                let items = want_set(&self.heap, receiver)?;
                charge_n(counters, limits, control, items.len() as u64)?;
                let out: Vec<Value> = items.iter().map(|&key| key_as_value(key)).collect();
                self.alloc_charged(Object::List(out), counters, limits)
            }

            // -- Section 8.4, Result. --
            "result.isOk" | "result.isErr" | "result.ok" | "result.err" | "result.unwrapOr" => {
                charge_one(counters, limits, control)?;
                let (index, payload) = self.result_parts(receiver)?;
                let is_ok = index == 0;
                match name {
                    "result.isOk" => Ok(Value::Bool(is_ok)),
                    "result.isErr" => Ok(Value::Bool(!is_ok)),
                    "result.ok" => Ok(if is_ok { payload } else { Value::None }),
                    "result.err" => Ok(if is_ok { Value::None } else { payload }),
                    _ => Ok(if is_ok { payload } else { arg(args, 1)? }),
                }
            }

            _ => self.call_string_native(name, args, limits, control, counters),
        }
    }

    /// The variant position and single payload of a `Result` value. Section
    /// 8.2 gives `Result` two variants, `Ok` first.
    fn result_parts(&self, value: Value) -> Result<(u16, Value), Fault> {
        let Value::Heap(h) = value else {
            return Err(Fault::internal("a Result method needs a Result value"));
        };
        let (variant, payload) = match self.heap.get(h) {
            Some(Object::Enum { variant, payload }) => (*variant, payload.first().copied()),
            _ => return Err(Fault::internal("a Result method needs a Result value")),
        };
        let desc = self
            .module()
            .module()
            .variants
            .get(variant as usize)
            .ok_or_else(|| Fault::internal("a variant index is out of range"))?;
        Ok((
            desc.index,
            payload.ok_or_else(|| Fault::internal("a Result variant carries no payload"))?,
        ))
    }
}

/// A map key as the value it stands for.
fn key_as_value(key: MapKey) -> Value {
    match key {
        MapKey::Bool(b) => Value::Bool(b),
        MapKey::Int(i) => Value::Int(i),
        MapKey::Str(h) => Value::Heap(h),
    }
}

/// `ceil(log2(n))` for `n >= 1`, used by the sort charge of section 14.1.
fn ceil_log2(n: u64) -> u64 {
    if n <= 1 {
        return 0;
    }
    u64::from(n.next_power_of_two().trailing_zeros())
}

impl Interpreter {
    /// The `String` methods of section 13.3. Positions and lengths count
    /// code points, never bytes.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per library function, in roster order"
    )]
    fn call_string_native(
        &mut self,
        name: &str,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Value, Fault> {
        // The receiver is borrowed, not copied: most methods only read it,
        // and charging before copying means a call that turns out to be
        // over budget never pays for a copy it will not use. Only `repeat`,
        // `padStart` and `padEnd` need an owned copy, because their result
        // is built by a closure that must be free of `self` so it can run
        // after `alloc_sized_string` has charged the memory it will take.
        let text = want_str(&self.heap, arg(args, 0)?)?;
        let bytes = text.len() as u64;
        match name {
            "string.byteLength" => {
                charge_one(counters, limits, control)?;
                Ok(Value::Int(i64::try_from(bytes).unwrap_or(i64::MAX)))
            }
            "string.isEmpty" => {
                charge_one(counters, limits, control)?;
                Ok(Value::Bool(bytes == 0))
            }
            "string.length" => {
                charge_n(counters, limits, control, bytes)?;
                Ok(Value::Int(
                    i64::try_from(text.chars().count()).unwrap_or(i64::MAX),
                ))
            }
            "string.at" => {
                charge_n(counters, limits, control, bytes)?;
                let index = want_int(arg(args, 1)?)?;
                if index < 0 {
                    return Ok(Value::None);
                }
                let index = usize::try_from(index).unwrap_or(usize::MAX);
                match text.chars().nth(index) {
                    None => Ok(Value::None),
                    Some(c) => self.alloc_string_charged(c.to_string(), counters, limits),
                }
            }
            "string.chars" => {
                charge_n(counters, limits, control, bytes)?;
                let count = text.chars().count() as u64;
                // The list, each string's own header and all of the bytes
                // are charged together before any of them is built: cutting
                // a long string into single code points multiplies it many
                // times over in per-object overhead alone.
                counters.charge_memory(list_of_strings_memory(count, bytes), limits)?;
                let pieces: Vec<String> = text.chars().map(|c| c.to_string()).collect();
                self.alloc_owned_string_list(pieces)
            }
            "string.contains" | "string.indexOf" => {
                let part = want_str(&self.heap, arg(args, 1)?)?;
                charge_n(counters, limits, control, bytes + part.len() as u64)?;
                let found = text.find(part);
                if name == "string.contains" {
                    return Ok(Value::Bool(found.is_some()));
                }
                // Positions count code points, so the byte offset is turned
                // back into a code point index.
                Ok(found.map_or(Value::None, |offset| {
                    let position = text
                        .get(..offset)
                        .map_or(0, |prefix| prefix.chars().count());
                    Value::Int(i64::try_from(position).unwrap_or(i64::MAX))
                }))
            }
            "string.startsWith" | "string.endsWith" => {
                let part = want_str(&self.heap, arg(args, 1)?)?;
                charge_n(counters, limits, control, bytes)?;
                Ok(Value::Bool(if name == "string.startsWith" {
                    text.starts_with(part)
                } else {
                    text.ends_with(part)
                }))
            }
            "string.substring" => {
                charge_n(counters, limits, control, bytes)?;
                let length = text.chars().count();
                let start = clamp_position(want_int(arg(args, 1)?)?, length);
                let up_to = clamp_position(want_int(arg(args, 2)?)?, length);
                let piece = if start >= up_to {
                    String::new()
                } else {
                    let from = byte_of_char(text, start);
                    let to = byte_of_char(text, up_to);
                    text.get(from..to).unwrap_or("").to_owned()
                };
                self.alloc_string_charged(piece, counters, limits)
            }
            "string.toUpper" | "string.toLower" => {
                charge_n(counters, limits, control, bytes)?;
                let converted = if name == "string.toUpper" {
                    text.to_uppercase()
                } else {
                    text.to_lowercase()
                };
                self.alloc_string_charged(converted, counters, limits)
            }
            "string.trim" | "string.trimStart" | "string.trimEnd" => {
                charge_n(counters, limits, control, bytes)?;
                let trimmed = match name {
                    "string.trim" => text.trim(),
                    "string.trimStart" => text.trim_start(),
                    _ => text.trim_end(),
                }
                .to_owned();
                self.alloc_string_charged(trimmed, counters, limits)
            }
            "string.split" => {
                charge_n(counters, limits, control, bytes)?;
                let separator = want_str(&self.heap, arg(args, 1)?)?;
                let (count, piece_bytes) = if separator.is_empty() {
                    (text.chars().count() as u64, bytes)
                } else {
                    let count = text.split(separator).count() as u64;
                    let removed = (separator.len() as u64).saturating_mul(count.saturating_sub(1));
                    (count, bytes.saturating_sub(removed))
                };
                counters.charge_memory(list_of_strings_memory(count, piece_bytes), limits)?;
                let pieces: Vec<String> = if separator.is_empty() {
                    // "With the empty separator it returns the code points."
                    text.chars().map(|c| c.to_string()).collect()
                } else {
                    text.split(separator).map(str::to_owned).collect()
                };
                self.alloc_owned_string_list(pieces)
            }
            "string.lines" => {
                charge_n(counters, limits, control, bytes)?;
                let (count, piece_bytes) = lines_stats(text);
                counters.charge_memory(list_of_strings_memory(count, piece_bytes), limits)?;
                self.alloc_owned_string_list(split_lines(text))
            }
            "string.replace" => {
                let target = want_str(&self.heap, arg(args, 1)?)?;
                let replacement = want_str(&self.heap, arg(args, 2)?)?;
                // "if `target` is empty, `s` is returned unchanged"
                if target.is_empty() {
                    charge_n(counters, limits, control, bytes)?;
                    return arg(args, 0);
                }
                // The result can be far larger than the receiver when
                // `replacement` is longer than `target`: the exact output
                // size is worked out from the match count before anything
                // is charged or built, rather than charging only for the
                // bytes read and then replacing without a further check.
                let matches = text.matches(target).count() as u64;
                let removed = matches.saturating_mul(target.len() as u64);
                let added = matches.saturating_mul(replacement.len() as u64);
                let output = bytes.saturating_sub(removed).saturating_add(added);
                let n = (bytes + target.len() as u64).max(output);
                charge_n(counters, limits, control, n)?;
                let target = target.to_owned();
                let replacement = replacement.to_owned();
                let text = text.to_owned();
                self.alloc_sized_string(
                    output,
                    || text.replace(target.as_str(), &replacement),
                    counters,
                    limits,
                )
            }
            "string.repeat" => {
                let count = want_int(arg(args, 1)?)?;
                if count < 0 {
                    return Err(Fault::invalid_argument(
                        "repeat",
                        "the count must not be negative",
                    ));
                }
                let count = u64::try_from(count).unwrap_or(0);
                let size = bytes.saturating_mul(count);
                charge_n(counters, limits, control, size)?;
                let times = usize::try_from(count).map_err(|_| {
                    Fault::internal("a repeat count larger than this host can hold")
                })?;
                let text = text.to_owned();
                self.alloc_sized_string(size, || text.repeat(times), counters, limits)
            }
            "string.padStart" | "string.padEnd" => {
                let width = want_int(arg(args, 1)?)?;
                let fill = want_str(&self.heap, arg(args, 2)?)?;
                // "Faults HS1011 if `fill` is empty, whether or not any
                // padding is needed."
                if fill.is_empty() {
                    return Err(Fault::invalid_argument(
                        if name == "string.padStart" {
                            "padStart"
                        } else {
                            "padEnd"
                        },
                        "the fill must not be empty",
                    ));
                }
                let length = text.chars().count();
                let wanted = clamp_position(width, usize::MAX);
                if wanted <= length {
                    charge_n(counters, limits, control, bytes)?;
                    return arg(args, 0);
                }
                let needed = wanted - length;
                // Section 11.4: the result's size is known before it is
                // built, so it is charged first. `width` comes from the
                // program, and building the padding before charging it let
                // one call allocate far past `maxMemoryBytes`.
                let size = bytes.saturating_add(padding_bytes(fill, needed));
                charge_n(counters, limits, control, size)?;
                let fill = fill.to_owned();
                let text = text.to_owned();
                self.alloc_sized_string(
                    size,
                    || {
                        let padding: String = fill.chars().cycle().take(needed).collect();
                        if name == "string.padStart" {
                            format!("{padding}{text}")
                        } else {
                            format!("{text}{padding}")
                        }
                    },
                    counters,
                    limits,
                )
            }
            _ => Err(Fault::internal(&format!(
                "the standard library function '{name}' is not implemented yet"
            ))),
        }
    }

    /// Allocates a list of already-built strings, without charging memory
    /// for any of it: for `chars`, `split` and `lines`, whose caller has
    /// already charged the list's, and every string's, exact memory with
    /// [`list_of_strings_memory`] before building `pieces`.
    fn alloc_owned_string_list(&mut self, pieces: Vec<String>) -> Result<Value, Fault> {
        let mut items = Vec::with_capacity(pieces.len());
        for piece in pieces {
            items.push(self.alloc_object(Object::Str(piece))?);
        }
        self.alloc_object(Object::List(items))
    }
}

/// `lines` (section 13.3): divides at LF or CR LF, drops the terminators,
/// and a terminator at the very end does not make a final empty line.
fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let body = body.strip_suffix('\r').unwrap_or(body);
    if body.is_empty() {
        return vec![String::new()];
    }
    body.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect()
}

/// What calling a library function produced.
pub(crate) enum NativeOutcome {
    /// The function finished; here is its value.
    Value(Value),
    /// The function calls a closure, so it becomes a frame of its own.
    Job(Box<NativeJob>),
}

/// A library function of chapter 14 or section 8.4 that calls a closure,
/// suspended between callbacks.
///
/// Every one of the sixteen is the same shape: a sequence of elements, one
/// call of the closure per element, and a rule for what to do with each
/// answer. Keeping the elements rather than a list of argument lists is what
/// keeps the extra memory to one value per element.
#[derive(Debug)]
pub struct NativeJob {
    /// The roster position, which decides how each answer is used.
    native: u32,
    /// The closure to call.
    callback: Value,
    /// The elements, in call order: a list's elements, a set's elements, or
    /// a map's values.
    items: Vec<Value>,
    /// The keys, for a map, aligned with `items`; empty otherwise.
    keys: Vec<MapKey>,
    /// The next element to call the closure on.
    index: usize,
    /// The values kept so far.
    kept: Vec<Value>,
    /// The keys kept so far, for a map job.
    kept_keys: Vec<MapKey>,
    /// The `reduce` accumulator.
    accumulator: Value,
    /// The merge sort in progress, for `list.sortBy` alone.
    sort: Option<Sort>,
    /// The value the last callback returned, waiting to be used.
    pub(crate) last: Option<Value>,
    /// The caller register the finished value goes into.
    pub(crate) dest: u16,
}

impl NativeJob {
    /// Every value this job is holding in its own private storage, for a
    /// mid-run collection: this is the "job stack of library functions that
    /// call closures" the collector's design doc talks about (a `Frame::Native`
    /// sits in the interpreter's own frame vector between callbacks, never on
    /// the Rust stack, so this is the whole of what a collection running
    /// while this job is on top of the frame stack must keep alive).
    pub(crate) fn push_roots(&self, out: &mut Vec<Value>) {
        out.push(self.callback);
        out.extend(self.items.iter().copied());
        out.extend(self.keys.iter().copied().map(key_as_value));
        out.extend(self.kept.iter().copied());
        out.extend(self.kept_keys.iter().copied().map(key_as_value));
        out.push(self.accumulator);
        out.extend(self.last);
        if let Some(sort) = &self.sort {
            sort.push_roots(out);
        }
    }
}

/// A merge sort of section 14.1, suspended between comparator calls.
///
/// The algorithm is fixed by the specification, "to make the calls to
/// `compare` the same on every implementation": a top-down merge sort whose
/// recursive calls run first on the left half, then on the right, then
/// merge. The merges therefore happen in the post-order of that recursion,
/// which is a schedule that can be worked out once, before any comparison,
/// and then followed one comparison at a time.
#[derive(Debug)]
struct Sort {
    /// The elements, rewritten in place as each merge finishes.
    values: Vec<Value>,
    /// Every merge to perform, as `(lo, mid, hi)`, in the order the
    /// recursive algorithm performs them.
    plan: Vec<(usize, usize, usize)>,
    /// The merge being performed.
    step: usize,
    /// The cursor into the left half, and into the right.
    left: usize,
    right: usize,
    /// What the merge has taken so far.
    taken: Vec<Value>,
}

impl Sort {
    /// Every value a merge in progress holds, for
    /// [`NativeJob::push_roots`]: the elements being rewritten in place and
    /// whatever a merge step has taken out of the two halves so far.
    fn push_roots(&self, out: &mut Vec<Value>) {
        out.extend(self.values.iter().copied());
        out.extend(self.taken.iter().copied());
    }

    /// The merge schedule of `len` elements, in post-order.
    fn plan_for(len: usize) -> Vec<(usize, usize, usize)> {
        let mut plan = Vec::new();
        // `(lo, hi, expanded)`: a range yet to be split, or one whose halves
        // are already scheduled and which is therefore ready to merge.
        let mut stack = vec![(0usize, len, false)];
        while let Some((lo, hi, expanded)) = stack.pop() {
            if hi - lo <= 1 {
                continue;
            }
            let mid = lo + (hi - lo) / 2;
            if expanded {
                plan.push((lo, mid, hi));
                continue;
            }
            // Popped in reverse, so the left half is sorted first, then the
            // right, and this range is merged last.
            stack.push((lo, hi, true));
            stack.push((mid, hi, false));
            stack.push((lo, mid, false));
        }
        plan
    }
}

/// Whether the roster entry called `name` calls a script closure.
fn takes_a_callback(name: &str) -> bool {
    ROSTER
        .iter()
        .find(|info| info.name == name)
        .is_some_and(|info| info.callbacks)
}

/// The closure value a callback native was given, with its function and
/// captured cells.
fn closure_parts(heap: &Heap, value: Value) -> Result<(u32, Vec<crate::arena::Handle>), Fault> {
    let Value::Heap(h) = value else {
        return Err(Fault::internal(
            "a library function needed a function value",
        ));
    };
    match heap.get(h) {
        Some(Object::Closure { function, captures }) => Ok((*function, captures.clone())),
        _ => Err(Fault::internal(
            "a library function needed a function value",
        )),
    }
}

fn want_bool(value: Value) -> Result<bool, Fault> {
    match value {
        Value::Bool(b) => Ok(b),
        _ => Err(Fault::internal(
            "a library callback needed to return a Bool",
        )),
    }
}

impl Interpreter {
    /// Builds the job for a library function that calls a closure. The whole
    /// receiver is read here, once, so that the job does not hold a heap
    /// handle whose contents could be collected under it.
    fn start_callback_native(
        &mut self,
        position: u32,
        name: &str,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<NativeJob, Fault> {
        let receiver = arg(args, 0)?;
        let mut sort = None;
        let (items, keys) = match name {
            "list.sortBy" => {
                let values = want_list(&self.heap, receiver)?.to_vec();
                let plan = Sort::plan_for(values.len());
                sort = Some(Sort {
                    values,
                    plan,
                    step: 0,
                    left: 0,
                    right: 0,
                    taken: Vec::new(),
                });
                (Vec::new(), Vec::new())
            }
            "list.all" | "list.any" | "list.each" | "list.filter" | "list.find"
            | "list.flatMap" | "list.map" => {
                (want_list(&self.heap, receiver)?.to_vec(), Vec::new())
            }
            "list.reduce" => (want_list(&self.heap, receiver)?.to_vec(), Vec::new()),
            "map.each" | "map.filter" | "map.mapValues" => {
                let entries = want_map(&self.heap, receiver)?;
                (
                    entries.iter().map(|e| e.value).collect(),
                    entries.iter().map(|e| e.key).collect(),
                )
            }
            "set.each" | "set.filter" => {
                let elements = want_set(&self.heap, receiver)?.to_vec();
                (
                    elements.iter().copied().map(key_as_value).collect(),
                    elements,
                )
            }
            // Section 8.4: the callback is called at most once, and only for
            // the variant the function acts on.
            "result.andThen" | "result.map" | "result.mapError" => {
                let (variant, payload) = self.result_parts(receiver)?;
                let acts = if name == "result.mapError" {
                    variant != 0
                } else {
                    variant == 0
                };
                (if acts { vec![payload] } else { Vec::new() }, Vec::new())
            }
            _ => {
                return Err(Fault::internal(
                    "an unknown library function with a callback",
                ));
            }
        };
        // "n + callbacks": the walk itself is charged here, and each call of
        // the closure is charged as a call when its frame is pushed.
        if name.starts_with("result.") {
            charge_one(counters, limits, control)?;
        } else if let Some(sorting) = &sort {
            // Section 14.1 charges a sort for `N * ceil(log2(N + 1))`
            // element steps; the comparator's own calls are charged as
            // calls of a closure.
            let n = sorting.values.len() as u64;
            charge_n(
                counters,
                limits,
                control,
                n.saturating_mul(ceil_log2(n.saturating_add(1))),
            )?;
        } else {
            charge_n(counters, limits, control, items.len() as u64)?;
        }
        let callback = arg(args, if name == "list.reduce" { 2 } else { 1 })?;
        // `reduce`'s accumulator starts at its first argument; every other
        // job uses this slot for the answer it has settled on.
        let accumulator = if name == "list.reduce" {
            arg(args, 1)?
        } else {
            receiver
        };
        Ok(NativeJob {
            native: position,
            callback,
            items,
            keys,
            index: 0,
            kept: Vec::new(),
            kept_keys: Vec::new(),
            accumulator,
            sort,
            last: None,
            dest: 0,
        })
    }

    /// Drives the library function on top of the frame stack by one turn:
    /// takes the value the last callback returned, if there is one, and
    /// either asks for the next callback or finishes.
    pub(crate) fn advance_native(
        &mut self,
        frames: &mut [crate::interp::Frame],
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<crate::interp::Step, Fault> {
        let Some(crate::interp::Frame::Native(job)) = frames.last_mut() else {
            return Err(Fault::internal("no library function to advance"));
        };
        let name = ROSTER
            .get(job.native as usize)
            .ok_or_else(|| Fault::internal("a native index is out of range"))?
            .name;

        if job.sort.is_some() {
            return self.advance_sort(frames, counters, limits);
        }

        // Use the answer the last callback gave, which may finish the job.
        if let Some(answer) = job.last.take() {
            let element = job.items.get(job.index).copied().unwrap_or(Value::None);
            let key = job.keys.get(job.index).copied();
            job.index += 1;
            match name {
                "list.map" => job.kept.push(answer),
                "list.flatMap" => {
                    let pieces = want_list(&self.heap, answer)?.to_vec();
                    job.kept.extend(pieces);
                }
                "list.filter" => {
                    if want_bool(answer)? {
                        job.kept.push(element);
                    }
                }
                "list.find" => {
                    if want_bool(answer)? {
                        return Ok(crate::interp::Step::Return(Some(element)));
                    }
                }
                "list.any" => {
                    if want_bool(answer)? {
                        return Ok(crate::interp::Step::Return(Some(Value::Bool(true))));
                    }
                }
                "list.all" => {
                    if !want_bool(answer)? {
                        return Ok(crate::interp::Step::Return(Some(Value::Bool(false))));
                    }
                }
                "list.each" | "map.each" | "set.each" => {}
                "list.reduce" => job.accumulator = answer,
                "map.filter" | "set.filter" => {
                    if want_bool(answer)? {
                        if let Some(key) = key {
                            job.kept_keys.push(key);
                            job.kept.push(element);
                        }
                    }
                }
                "map.mapValues" => {
                    if let Some(key) = key {
                        job.kept_keys.push(key);
                        job.kept.push(answer);
                    }
                }
                // Section 8.4: `map` and `mapError` wrap the answer in the
                // variant they were given, which is the variant the receiver
                // already carries, so no new variant is named here.
                "result.map" | "result.mapError" => {
                    let wrapped = self.rewrap_result(job.accumulator, answer, counters, limits)?;
                    return Ok(crate::interp::Step::Return(Some(wrapped)));
                }
                "result.andThen" => return Ok(crate::interp::Step::Return(Some(answer))),
                _ => {
                    return Err(Fault::internal(
                        "an unknown library function with a callback",
                    ));
                }
            }
        }

        // Ask for the next callback, if there is one.
        let Some(crate::interp::Frame::Native(job)) = frames.last_mut() else {
            return Err(Fault::internal("no library function to advance"));
        };
        if let Some(&element) = job.items.get(job.index) {
            let key = job.keys.get(job.index).copied();
            let call_args = match name {
                "list.reduce" => vec![job.accumulator, element],
                "map.each" | "map.filter" => vec![
                    key_as_value(key.ok_or_else(|| Fault::internal("a map job has no key"))?),
                    element,
                ],
                _ => vec![element],
            };
            let (function, captures) = closure_parts(&self.heap, job.callback)?;
            return Ok(crate::interp::Step::Call {
                function,
                args: call_args,
                captures,
                dest: 0,
            });
        }

        // No elements left: assemble the answer.
        let (kept, kept_keys, accumulator) = {
            let Some(crate::interp::Frame::Native(job)) = frames.last_mut() else {
                return Err(Fault::internal("no library function to advance"));
            };
            (
                std::mem::take(&mut job.kept),
                std::mem::take(&mut job.kept_keys),
                job.accumulator,
            )
        };
        let value = match name {
            "list.map" | "list.flatMap" | "list.filter" => {
                self.alloc_charged(Object::List(kept), counters, limits)?
            }
            // `find` answers `none` and `each` answers nothing; both are
            // the absent value, for different reasons.
            "list.find" | "list.each" | "map.each" | "set.each" => Value::None,
            "list.any" => Value::Bool(false),
            "list.all" => Value::Bool(true),
            // `reduce` answers its accumulator, and a `Result` the function
            // did not act on is returned unchanged.
            "list.reduce" | "result.andThen" | "result.map" | "result.mapError" => accumulator,
            "map.filter" | "map.mapValues" => {
                let entries = kept_keys
                    .into_iter()
                    .zip(kept)
                    .map(|(key, value)| MapEntry { key, value })
                    .collect();
                self.alloc_charged(Object::Map(entries), counters, limits)?
            }
            "set.filter" => self.alloc_charged(Object::Set(kept_keys), counters, limits)?,
            _ => {
                return Err(Fault::internal(
                    "an unknown library function with a callback",
                ));
            }
        };
        let _ = control;
        Ok(crate::interp::Step::Return(Some(value)))
    }

    /// Drives `list.sortBy` by one comparison.
    ///
    /// Each turn takes the answer the comparator gave for the pair it was
    /// asked about, if there is one, and then either asks about the next
    /// pair or finishes. Section 14.1 reads a negative answer as "a before
    /// b", zero as "keep the order" and positive as "b before a", and the
    /// sort is stable because a `<= 0` answer takes from the left half.
    fn advance_sort(
        &mut self,
        frames: &mut [crate::interp::Frame],
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<crate::interp::Step, Fault> {
        let Some(crate::interp::Frame::Native(job)) = frames.last_mut() else {
            return Err(Fault::internal("no sort to advance"));
        };
        let callback = job.callback;
        let Some(sort) = job.sort.as_mut() else {
            return Err(Fault::internal("no sort to advance"));
        };

        if let Some(answer) = job.last.take() {
            let Value::Int(ordering) = answer else {
                return Err(Fault::internal("a sort comparator must return an Int"));
            };
            let Some(&(lo, mid, hi)) = sort.plan.get(sort.step) else {
                return Err(Fault::internal("a sort step is out of range"));
            };
            let from_left = ordering <= 0;
            let index = if from_left {
                lo + sort.left
            } else {
                mid + sort.right
            };
            let Some(&value) = sort.values.get(index) else {
                return Err(Fault::internal("a sort index is out of range"));
            };
            sort.taken.push(value);
            if from_left {
                sort.left += 1;
            } else {
                sort.right += 1;
            }
            let _ = hi;
        }

        loop {
            let Some(&(lo, mid, hi)) = sort.plan.get(sort.step) else {
                // Every merge is done: the array is sorted.
                let values = std::mem::take(&mut sort.values);
                let list = self.alloc_charged(Object::List(values), counters, limits)?;
                return Ok(crate::interp::Step::Return(Some(list)));
            };
            if lo + sort.left < mid && mid + sort.right < hi {
                let (Some(&left), Some(&right)) = (
                    sort.values.get(lo + sort.left),
                    sort.values.get(mid + sort.right),
                ) else {
                    return Err(Fault::internal("a sort index is out of range"));
                };
                let (function, captures) = closure_parts(&self.heap, callback)?;
                return Ok(crate::interp::Step::Call {
                    function,
                    args: vec![left, right],
                    captures,
                    dest: 0,
                });
            }
            // One half is exhausted: the rest of the other follows, in
            // order, with no comparison.
            for index in (lo + sort.left)..mid {
                if let Some(&value) = sort.values.get(index) {
                    sort.taken.push(value);
                }
            }
            for index in (mid + sort.right)..hi {
                if let Some(&value) = sort.values.get(index) {
                    sort.taken.push(value);
                }
            }
            let merged = std::mem::take(&mut sort.taken);
            for (offset, value) in merged.into_iter().enumerate() {
                if let Some(slot) = sort.values.get_mut(lo + offset) {
                    *slot = value;
                }
            }
            sort.step += 1;
            sort.left = 0;
            sort.right = 0;
        }
    }

    /// The `Result` `original` with its payload replaced by `payload`, in the
    /// same variant. Section 8.4's `map` and `mapError` each wrap in the
    /// variant they were given, so this never has to name a variant the
    /// module's own table does not already hold.
    fn rewrap_result(
        &mut self,
        original: Value,
        payload: Value,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        let Value::Heap(h) = original else {
            return Err(Fault::internal("a Result method needs a Result value"));
        };
        let variant = match self.heap.get(h) {
            Some(Object::Enum { variant, .. }) => *variant,
            _ => return Err(Fault::internal("a Result method needs a Result value")),
        };
        self.alloc_charged(
            Object::Enum {
                variant,
                payload: vec![payload],
            },
            counters,
            limits,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ceil_log2, fixed_text, floor_div, floor_mod, integer_power, is_implemented, padding_bytes,
        parse_float, parse_int, split_lines,
    };
    use hollowscript_core::native::ROSTER;

    /// Every roster entry is either implemented or named in the list of
    /// those that are not, so nothing can be silently missing.
    /// A module that imports a library function this build does not have is
    /// refused when it is loaded, by name, rather than part way through a
    /// run. Every roster entry is implemented today, so the refusal is
    /// shown with a name that is not one.
    #[test]
    fn a_module_that_imports_an_unknown_function_is_refused_by_name() {
        use hollowscript_bytecode::module::{Module, NativeImport};

        let mut module = Module::new();
        let name = module.add_string("list.notAFunction").unwrap();
        module.natives.push(NativeImport { name, arity: 1 });
        let fault = super::resolve_imports(&module).expect_err("there is no such function");
        assert!(fault.message.contains("list.notAFunction"));

        let mut module = Module::new();
        let name = module.add_string("list.length").unwrap();
        module.natives.push(NativeImport { name, arity: 7 });
        let fault = super::resolve_imports(&module).expect_err("the arity is wrong");
        assert!(fault.message.contains("list.length"));

        let mut module = Module::new();
        let name = module.add_string("list.length").unwrap();
        module.natives.push(NativeImport { name, arity: 1 });
        assert_eq!(
            super::resolve_imports(&module).as_deref().map(<[u32]>::len),
            Ok(1)
        );
    }

    /// Every entry of the roster has a body: nothing is refused by
    /// `is_implemented` today, and the next entry added should be until one
    /// is written for it.
    #[test]
    fn every_roster_entry_is_implemented() {
        for info in ROSTER {
            assert!(
                is_implemented(info.name),
                "{} is not implemented",
                info.name
            );
        }
        assert_eq!(ROSTER.len(), 118);
    }

    /// The padding's size is what building it would produce, including a
    /// fill whose code points are of different widths and a count that
    /// stops part way through it.
    #[test]
    fn padding_bytes_matches_the_padding_it_describes() {
        for fill in ["y", "ab", "é€x", "🙂-"] {
            for needed in [0, 1, 2, 3, 7, 64] {
                let built: String = fill.chars().cycle().take(needed).collect();
                assert_eq!(
                    padding_bytes(fill, needed),
                    built.len() as u64,
                    "{fill} {needed}"
                );
            }
        }
        assert_eq!(padding_bytes("", 5), 0);
        assert_eq!(padding_bytes("é", usize::MAX), u64::MAX);
    }

    /// Section 13.2's own worked values for floor division and floor modulo.
    #[test]
    fn division_follows_section_13_2() {
        assert_eq!(floor_div(7, 2), 3);
        assert_eq!(floor_div(-7, 2), -4);
        assert_eq!(floor_div(7, -2), -4);
        assert_eq!(floor_mod(-7, 3), 2);
        assert_eq!(floor_mod(7, -3), -2);
        // "`mod(-9223372036854775808, -1)` is 0"
        assert_eq!(floor_mod(i64::MIN, -1), 0);
        assert_eq!(i64::MIN.wrapping_rem(-1), 0);
    }

    /// "`ipow(x, 0)` is 1, including `ipow(0, 0)`; faults HS1011 if
    /// `exponent < 0`, and HS1007 if and only if the mathematical result is
    /// outside `Int`: no intermediate value may fault, so `ipow(-2, 63)` is
    /// -9223372036854775808, and `ipow(1, e)` and `ipow(-1, e)` never fault".
    #[test]
    fn integer_power_follows_section_13_2() {
        assert_eq!(integer_power(2, 62), Ok(4_611_686_018_427_387_904));
        assert_eq!(integer_power(0, 0), Ok(1));
        assert_eq!(integer_power(0, 5), Ok(0));
        assert_eq!(integer_power(7, 0), Ok(1));
        assert_eq!(integer_power(-2, 63), Ok(i64::MIN));
        assert_eq!(integer_power(1, i64::MAX), Ok(1));
        assert_eq!(integer_power(-1, i64::MAX), Ok(-1));
        assert_eq!(integer_power(-1, i64::MAX - 1), Ok(1));
        assert!(integer_power(2, 63).is_err());
        assert!(integer_power(2, -1).is_err());
    }

    /// Section 13.4's own examples.
    #[test]
    fn fixed_rounds_halves_away_from_zero() {
        #[allow(
            clippy::approx_constant,
            reason = "section 13.4's own worked example happens to look like pi"
        )]
        {
            assert_eq!(fixed_text(3.14159, 2), "3.14");
        }
        assert_eq!(fixed_text(2.5, 0), "3");
        assert_eq!(fixed_text(-2.5, 0), "-3");
        assert_eq!(fixed_text(0.125, 2), "0.13");
        assert_eq!(fixed_text(1.0, 3), "1.000");
        // "a `-` sign is written only if the rounded value is not zero"
        assert_eq!(fixed_text(-0.001, 2), "0.00");
        assert_eq!(fixed_text(0.0, 0), "0");
    }

    #[test]
    fn parse_int_takes_the_form_section_13_4_states() {
        assert_eq!(parse_int("42"), Ok(42));
        assert_eq!(parse_int("-0"), Ok(0));
        assert_eq!(parse_int("+7"), Ok(7));
        assert_eq!(parse_int("007"), Ok(7));
        assert_eq!(parse_int("-9223372036854775808"), Ok(i64::MIN));
        for bad in ["", "4x2", " 42", "42 ", "4_2", "1.0", "+", "-"] {
            assert_eq!(
                parse_int(bad),
                Err("invalid integer"),
                "{bad} should not parse"
            );
        }
        assert_eq!(
            parse_int("9223372036854775808"),
            Err("integer out of range")
        );
        assert_eq!(
            parse_int("-99999999999999999999"),
            Err("integer out of range")
        );
    }

    #[test]
    fn parse_float_takes_the_form_section_13_4_states() {
        assert_eq!(parse_float("2.5e2"), Ok(250.0));
        assert_eq!(parse_float("-0.0"), Ok(0.0));
        assert_eq!(parse_float("1"), Ok(1.0));
        assert_eq!(parse_float("1E3"), Ok(1000.0));
        for bad in [".5", "5.", "nan", "inf", "0x1", "1_0", "", "1e"] {
            assert_eq!(
                parse_float(bad),
                Err("invalid number"),
                "{bad} should not parse"
            );
        }
        assert_eq!(parse_float("1e400"), Err("number out of range"));
        assert_eq!(parse_float("1e-400"), Ok(0.0), "underflow is zero");
        assert_eq!(parse_float("0e99999999999999999999"), Ok(0.0));
    }

    /// Section 13.3: `lines` drops the terminators, and one at the very end
    /// does not make a final empty line.
    #[test]
    fn lines_follows_section_13_3() {
        assert_eq!(split_lines("a\nb\n"), ["a", "b"]);
        assert_eq!(split_lines("a\n\nb"), ["a", "", "b"]);
        assert!(split_lines("").is_empty());
        assert_eq!(split_lines("a\r\nb"), ["a", "b"]);
        assert_eq!(split_lines("\n"), [""]);
    }

    /// Section 14.1 fixes the algorithm so that the calls to `compare` are
    /// the same on every implementation: a top-down merge sort whose
    /// recursive calls run first on the left half, then the right, then
    /// merge. The merges therefore happen in post-order.
    #[test]
    fn the_merge_schedule_is_the_recursions_post_order() {
        use super::Sort;
        assert!(Sort::plan_for(0).is_empty());
        assert!(Sort::plan_for(1).is_empty());
        assert_eq!(Sort::plan_for(2), [(0, 1, 2)]);
        // Four elements: [0,2) and [2,4) each merge before [0,4).
        assert_eq!(Sort::plan_for(4), [(0, 1, 2), (2, 3, 4), (0, 2, 4)]);
        // Five: the halves are [0,2) and [2,5), and the right half's own
        // halves are [2,3) and [3,5), so its merges come before the last.
        assert_eq!(
            Sort::plan_for(5),
            [(0, 1, 2), (3, 4, 5), (2, 3, 5), (0, 2, 5)]
        );
        // Every merge covers a real range, and the last covers all of it.
        let plan = Sort::plan_for(9);
        assert!(plan.iter().all(|&(lo, mid, hi)| lo < mid && mid < hi));
        assert_eq!(plan.last().copied(), Some((0, 4, 9)));
    }

    #[test]
    fn the_sort_charge_uses_a_ceiling_logarithm() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
    }
}
