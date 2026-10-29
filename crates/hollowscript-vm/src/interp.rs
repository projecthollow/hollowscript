//! The interpreter.
//!
//! Owns one instance's state: the heap, the run counters, the loaded module
//! and the top-level bindings. [`Interpreter::initialise`] and
//! [`Interpreter::call_export`] each run a fresh [`RunCounters`]-scoped
//! *run* (section 11.2) over the register machine of `hollowscript-bytecode`,
//! threading step, memory, output and call-depth charges and the safepoint
//! rule of section 11.7 through every instruction.
//!
//! **Scope of this pass.** Everything the checked, verified instruction set
//! can express is implemented here except three things that need a
//! representation this crate's frozen primitives do not yet have, and that no
//! fixture or test anywhere in the workspace yet exercises: a first-class
//! function value built from a capability, a native import or an enum
//! variant constructor ([`Op::CapFn`], [`Op::NativeFn`], [`Op::VariantFn`]).
//! Each faults with a precisely worded internal error rather than guess at an
//! encoding. A direct call of a closure ([`Op::Call`], [`Op::CallFn`]), a
//! synchronous capability call ([`Op::CallCap`], through
//! [`CapabilityTable::call`]) and a library function ([`Op::CallNative`],
//! through [`crate::natives`]) are all implemented.
//!
//! **The call stack is the interpreter's own.** A script call pushes a
//! frame onto a vector rather than recursing into the interpreter's
//! function-running method, so the host's real stack stays one frame deep
//! however deeply a program calls. Section 11.5 requires exactly this: "an
//! implementation MUST make the default `maxCallDepth` of 1,000 safe:
//! recursion must always reach the language's counter before it can exhaust
//! the implementation's own stack". Recursing in Rust did not satisfy that.
//! It happened to reach HS1005 in an optimised build, but in an unoptimised
//! one an interpreter frame is large enough that a hundred script calls
//! aborted the process with a stack overflow, which is not a fault a host
//! can catch.

use crate::arena::Handle;
use crate::capability::{CapabilityTable, Command, CommandBuffer, CommandResult};
use crate::heap::Heap;
use crate::limits::{RunCounters, check_retained_size};
use crate::value::{Builtin, MapEntry, MapKey, Object, Value};
use hollowscript_bytecode::module::{
    CapabilityKind, CaptureSource, Constant, StringId, VariantDesc,
};
use hollowscript_bytecode::opcode::{Instruction, Op, Wide};
use hollowscript_bytecode::verify::VerifiedModule;
use hollowscript_core::cancel::Control;
use hollowscript_core::fault::{Fault, FaultCode};
use hollowscript_core::limits::structural::{MAX_CHARGE, SAFEPOINT_INTERVAL};
use hollowscript_core::{Interner, Limits};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// The wording used when a part of a string interpolation is not one of the
/// four types section 3.9 allows. The checker rejects anything else with
/// HS0317, so reaching this means the bytecode did not come from the checker.
const INTERP_OPERAND: &str = "Interp needs an Int, a Float, a Bool or a String";

/// One active script call: everything that belongs to the call rather than
/// to the instance. [`Interpreter::run_function`] keeps these in a vector, so
/// the depth a program can reach is bounded by `maxCallDepth` (section 11.5)
/// and by the heap, never by the host's own stack.
pub(crate) struct ScriptFrame {
    /// The function being run.
    function: u32,
    /// The closure's captured cells, empty for a function with no upvalues.
    captures: Vec<Handle>,
    /// The registers of this call.
    pub(crate) registers: Vec<Value>,
    /// The next instruction to run.
    pc: usize,
    /// The register of the *caller* that this call's result goes into.
    dest: u16,
    /// The registers of this frame and of every script frame below it, which
    /// [`stacked_register_budget`] bounds.
    stacked: u64,
}

/// The fewest registers the script frames of one run may hold between them
/// before a call is refused, whatever `maxCallDepth` is; see
/// [`stacked_register_budget`].
const STACKED_REGISTER_FLOOR: u64 = 1 << 22;

/// Alias checks for `list.append` scan only compact frames. Wider frames use
/// the immutable copy path so an uncharged scan cannot scale with attacker-
/// controlled register or global counts.
const MAX_APPEND_ALIAS_SCAN_SLOTS: usize = 32;

/// How many registers the script frames of one run may hold between them.
///
/// A frame's registers are real memory that none of section 11.4's counters
/// charge, and a frame may hold up to 65,535 of them
/// (`hollowscript_bytecode::module::MAX_REGISTERS`), so `maxCallDepth` alone
/// no longer bounds what recursion can hold: a hundred-thousand-element
/// literal is built a chunk at a time and never needs such a frame, but a
/// call of tens of thousands of arguments does, and a function with one
/// that calls itself a thousand times deep would hold two gibibytes. The
/// budget is 256 registers for each level of `maxCallDepth` and for the
/// frame the run starts in, which is exactly what frames could hold when a
/// frame held at most 256, so a program whose frames are no wider than that
/// reaches its call depth limit as it always did; and never less than [`STACKED_REGISTER_FLOOR`] (128 MiB of values),
/// so that a moderately wide function, a few hundred or thousand
/// registers, still recurses as deep as the default `maxCallDepth` allows.
/// Beyond it the call faults with HS1015, section 1.10's fault for a limit
/// of the implementation rather than of the language, at the call that
/// reached it.
fn stacked_register_budget(limits: &Limits) -> u64 {
    limits
        .max_call_depth
        .saturating_add(1)
        .saturating_mul(256)
        .max(STACKED_REGISTER_FLOOR)
}

/// One entry of the interpreter's call stack: a script call, or a library
/// function of chapter 14 suspended at a call of the closure it was given.
pub(crate) enum Frame {
    /// A script function or closure.
    Script(ScriptFrame),
    /// A library function that calls a closure (`list.map` and the rest),
    /// which is driven one callback at a time. Boxed because a job carries
    /// the whole collection it is walking, which is much larger than a
    /// script frame and far rarer.
    Native(Box<crate::natives::NativeJob>),
}

/// Every value a frame stack is currently holding, pushed onto `out`: each
/// script frame's registers (the interpreter's whole register set for that
/// call, section 11.5) and the cells its closure captured (read directly by
/// `Op::GetUpval`/`Op::SetUpval`, not necessarily copied into any register),
/// and each library-function job's own private storage. Called both by the
/// safepoint check in [`Interpreter::run_frames`], with the frames a run is
/// actively on, and by [`Interpreter::live_roots`], with a suspended run's
/// frames; both call it at a point where every frame's registers have
/// already been written back from the loop's local variables, so this alone
/// is precise, with no extra "currently mid-instruction" state to add.
fn push_frame_roots(frames: &[Frame], out: &mut Vec<Value>) {
    for frame in frames {
        match frame {
            Frame::Script(f) => {
                out.extend(f.registers.iter().copied());
                out.extend(f.captures.iter().copied().map(Value::Heap));
            }
            Frame::Native(job) => job.push_roots(out),
        }
    }
}

/// Whether the whole call is over after one [`Step`].
enum Outcome {
    /// There are frames left to run.
    Running,
    /// The frame the call started in has returned, with this value.
    Finished(Option<Value>),
    /// The run has queued a capability call in the command buffer and is
    /// waiting for the host to perform it.
    Suspended {
        /// The `cap_calls` entry the run is waiting on, so that its result
        /// can be checked against what it declared.
        call: u32,
        /// The register of the frame on top that the result goes into.
        dest: u16,
    },
}

/// What running one instruction, or one turn of a library function, asks the
/// frame loop to do next.
pub(crate) enum Step {
    /// Carry on with the next instruction, or the next callback.
    Next,
    /// Push a frame for a script call.
    Call {
        /// The function to call.
        function: u32,
        /// The arguments, already read out of the caller's registers.
        args: Vec<Value>,
        /// The closure's captured cells.
        captures: Vec<Handle>,
        /// The caller register the result goes into. Ignored when the
        /// caller is a library function, which catches the value itself.
        dest: u16,
    },
    /// Push a frame for a library function that calls a closure.
    StartNative {
        /// The job, with its first callback not yet called.
        job: Box<crate::natives::NativeJob>,
        /// The caller register the finished result goes into.
        dest: u16,
    },
    /// Pop this frame, with the value it returns.
    Return(Option<Value>),
    /// Queue a capability call for the host and pause the run
    /// (see [`Interpreter::use_command_buffer`]).
    Suspend {
        /// The `cap_calls` entry being called.
        call: u32,
        /// The arguments, already read out of the caller's registers and
        /// already charged.
        args: Vec<Value>,
        /// The caller register the result will go into on resume.
        dest: u16,
    },
}

/// What `Op::Call` found in the register it was pointed at, read out of the
/// heap before anything else borrows it.
enum Callee {
    /// A script function or closure, with the cells it captured.
    Script(u32, Vec<Handle>),
    /// A library function, a variant constructor or a capability function.
    Builtin(Builtin),
}

/// What one entry point did: finished, or paused at a capability call.
///
/// Only a host on the batched path (see
/// [`Interpreter::use_command_buffer`]) can ever see [`Run::Suspended`]; a
/// synchronous host's capability calls never pause.
#[derive(Debug)]
pub enum Run {
    /// The run is over, with the value it produced: `Some` for an early
    /// `return`, `None` for running to the end.
    Done(Option<Value>),
    /// The run paused at a capability call. The host drains
    /// [`Interpreter::commands`], performs them in order, and continues the
    /// run with [`Interpreter::resume`].
    Suspended,
}

/// Which entry point a suspended run started in, so that
/// [`Interpreter::resume`] finishes the same way that entry point would
/// have: the remaining module initialisers for a load, the call depth and
/// the retained-memory check for an exported call.
enum Entry {
    /// [`Interpreter::initialise_batched`], with the module initialisers
    /// that have not run yet.
    Initialise(Vec<u32>),
    /// [`Interpreter::call_export_batched`].
    Export,
}

/// The instruction being run, as its function and pc, for a fault it raises
/// to be located at (section 19.3). Kept as the two numbers rather than
/// resolved to a `Location` on every instruction, which allocated the
/// module's path each time; [`Interpreter::location_at`] resolves it only
/// once a fault needs it.
type FaultAt = Option<(u32, usize)>;

/// Everything a paused run has to put down and pick up again. Section 11.2
/// says what a run is and section 11.3 says what it is charged; a pause is
/// not a new run, so the counters and the frames cross it untouched.
struct Suspension {
    frames: Vec<Frame>,
    counters: RunCounters,
    fault_at: FaultAt,
    /// The `cap_calls` entry the run is waiting on.
    call: u32,
    /// The register of the frame on top that the result goes into.
    dest: u16,
    /// How many commands the buffer held when the run paused, which is how
    /// many results the host must supply.
    pending: usize,
    entry: Entry,
}

/// One loaded program, ready to run its exported functions (chapter 12).
pub struct Interpreter {
    module: VerifiedModule,
    pub(crate) heap: Heap,
    /// The roster position of each of the module's native imports, resolved
    /// when the module was loaded.
    natives: Vec<u32>,
    /// Why the module's native imports could not be resolved, if they could
    /// not: the module names a library function this build does not have.
    native_error: Option<Fault>,
    /// Where this module keeps the prelude types of chapter 15.
    data_types: crate::data::DataTypes,
    capabilities: CapabilityTable,
    /// The current value of every global slot, indexed as the module's
    /// `globals` table is, filled in as initialisation runs.
    globals: Vec<Value>,
    /// Present when this interpreter is on the batched path: a capability
    /// call is queued here and the run pauses, rather than calling a
    /// [`crate::capability::HostFn`] synchronously.
    buffer: Option<CommandBuffer>,
    /// The paused run, if there is one.
    suspended: Option<Suspension>,
    /// Values the host is holding onto from a previous call, kept alive
    /// across every collection, mid-run ones included, until the host says
    /// otherwise (`crates/hollowscript`'s `Instance::remember` and
    /// `Instance::release_values`, which mirror every change here so that a
    /// collection running in the middle of the *next* call still sees them).
    /// [`Interpreter::collect`]'s `also` parameter is for one call's own
    /// extra roots on top of these, not a replacement for them.
    host_roots: Vec<Value>,
    /// The one heap object for each `String` or `Filter` constant of the
    /// module that has been loaded so far, indexed by the constant's
    /// `StringId` (`None` until the first `LoadK` of it). Section 11.4 does
    /// not charge a literal with no interpolation, so `LoadK` must not
    /// allocate a new, uncharged copy on every execution either: it
    /// allocates each distinct constant once, the first time it runs, and
    /// hands out that same immutable object every time after. Real memory
    /// spent on constants is therefore bounded by the size of the module,
    /// not by how often a loop loads one. Every entry is a root of every
    /// collection, since a later `LoadK` may hand it out again.
    constant_strings: Vec<Option<Value>>,
    /// As `constant_strings`, for `Filter` constants.
    constant_filters: Vec<Option<Value>>,
}

impl std::fmt::Debug for Interpreter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interpreter")
            .field("functions", &self.module.module().functions.len())
            .field("heap_len", &self.heap.len())
            .field("capabilities", &self.capabilities)
            .field("globals", &self.globals.len())
            .field("natives", &self.natives.len())
            .field("native_error", &self.native_error)
            .field("data_types", &self.data_types)
            .field("batched", &self.buffer.is_some())
            .field("suspended", &self.suspended.is_some())
            .field("host_roots", &self.host_roots.len())
            .field(
                "constant_strings",
                &self.constant_strings.iter().flatten().count(),
            )
            .field(
                "constant_filters",
                &self.constant_filters.iter().flatten().count(),
            )
            .finish()
    }
}

// Register access. The verifier guarantees every register operand of a
// verified module is within its function's `register_count`, so a failure
// here is a defect of the verifier or the module builder, not a property of
// any script; these still fault rather than index directly, matching this
// crate's own rule that nothing may panic on any input (section 19.1).

fn reg(registers: &[Value], index: u16) -> Result<Value, Fault> {
    registers
        .get(index as usize)
        .copied()
        .ok_or_else(|| Fault::internal("a register index is out of range"))
}

fn set_reg(registers: &mut [Value], index: u16, value: Value) -> Result<(), Fault> {
    match registers.get_mut(index as usize) {
        Some(slot) => {
            *slot = value;
            Ok(())
        }
        None => Err(Fault::internal("a register index is out of range")),
    }
}

/// A run of `count` consecutive registers starting at `start`, copied out so
/// the borrow does not outlive a later write to `registers`.
fn reg_run(registers: &[Value], start: usize, count: usize) -> Result<Vec<Value>, Fault> {
    let end = start.saturating_add(count);
    registers
        .get(start..end)
        .map(<[Value]>::to_vec)
        .ok_or_else(|| Fault::internal("a register range is out of bounds"))
}

/// `count` key and value pairs from `2 * count` consecutive registers.
fn read_pairs(
    registers: &[Value],
    start: usize,
    count: usize,
) -> Result<Vec<(Value, Value)>, Fault> {
    let flat = reg_run(registers, start, count.saturating_mul(2))?;
    let mut pairs = Vec::with_capacity(count);
    let mut values = flat.into_iter();
    while let (Some(key), Some(value)) = (values.next(), values.next()) {
        pairs.push((key, value));
    }
    Ok(pairs)
}

/// A jump target relative to the instruction after the jump, per the
/// encoding's own rule (`hollowscript_bytecode::opcode`'s module doc).
/// Section 19.3 locates a run-stage diagnostic at the operation that raised
/// it. A fault that already carries a location keeps it, since an inner
/// operation knows better than the instruction around it.
fn locate(fault: Fault, at: Option<hollowscript_core::Location>) -> Fault {
    match at {
        Some(location) if fault.location.is_none() => fault.at(location),
        _ => fault,
    }
}

/// Compares an `Int` with a `Float` by exact mathematical value, which is
/// what sections 3.5 and 2.7 require: "Comparisons are not coercion sites:
/// an `Int` and a `Float` are compared by their exact values, optional or
/// not, without converting either." Converting either way would answer
/// wrongly at the ends of the ranges, which is why the section states
/// `9007199254740993 == 9007199254740992.0` is `false`.
///
/// `f` is finite, which every [`Value::Float`] is (section 3.4).
fn compare_int_float(i: i64, f: f64) -> Ordering {
    // 2^63, the first Float above every Int, and its negation, which is the
    // smallest Int exactly. Both are exact in binary64.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if f >= LIMIT {
        return Ordering::Less;
    }
    if f < -LIMIT {
        return Ordering::Greater;
    }
    // Within the range, `trunc` is a whole number the cast converts exactly,
    // and `f - trunc` is exact too: below 2^52 the subtraction is, and at or
    // above it `f` is already whole and the difference is zero.
    let trunc = f.trunc();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the range check above makes the cast exact"
    )]
    let whole = trunc as i64;
    match i.cmp(&whole) {
        Ordering::Equal => 0.0_f64.partial_cmp(&(f - trunc)).unwrap_or(Ordering::Equal),
        other => other,
    }
}

fn jump(pc: usize, offset: i32) -> Result<usize, Fault> {
    let base = i64::try_from(pc).unwrap_or(i64::MAX);
    let target = base.saturating_add(i64::from(offset));
    usize::try_from(target).map_err(|_| Fault::internal("a jump target is out of range"))
}

/// Charges `amount` steps (1 to [`MAX_CHARGE`]) and, if the charge takes the
/// counter across a multiple of [`SAFEPOINT_INTERVAL`], applies the
/// safepoint rule of section 11.7, before the work the charge pays for.
pub(crate) fn charge_and_safepoint(
    counters: &mut RunCounters,
    limits: &Limits,
    control: &mut Control<'_>,
    amount: u64,
) -> Result<(), Fault> {
    let before = counters.steps();
    counters.charge_steps(amount, limits)?;
    if before / SAFEPOINT_INTERVAL != counters.steps() / SAFEPOINT_INTERVAL {
        control.safepoint().map_err(|_| Fault::cancelled())?;
    }
    Ok(())
}

/// Charges a total that may exceed [`MAX_CHARGE`], applied as consecutive
/// slices of at most `MAX_CHARGE`, each with its own safepoint check
/// (section 11.3, "slices").
pub(crate) fn charge_steps_sliced(
    counters: &mut RunCounters,
    limits: &Limits,
    control: &mut Control<'_>,
    mut amount: u64,
) -> Result<(), Fault> {
    while amount > 0 {
        let slice = amount.min(MAX_CHARGE);
        charge_and_safepoint(counters, limits, control, slice)?;
        amount -= slice;
    }
    Ok(())
}

/// The largest number of distinct pairs of objects an equality walk will
/// remember once it has outrun its charge (see [`Interpreter::values_equal`]):
/// about 4 million, which is tens of megabytes of bookkeeping. A comparison
/// that needs more faults with HS1015 rather than let the account grow
/// without a bound of its own.
const EQUALITY_MEMO_LIMIT: usize = 1 << 22;

/// How many pairs an equality walk visits past its allowance for each
/// charge of [`EQUALITY_BLOCK_STEPS`] steps: one step for every 8 pairs, the
/// rate at which section 11.3's comparison charge pays for 8-byte elements.
const EQUALITY_BLOCK_PAIRS: u64 = 512;

/// The charge for each [`EQUALITY_BLOCK_PAIRS`] pairs past the allowance.
const EQUALITY_BLOCK_STEPS: u64 = EQUALITY_BLOCK_PAIRS / 8;

/// Whether a program can compare objects of this kind, which is what makes
/// one equal to itself.
fn is_comparable_object(object: &Object) -> bool {
    matches!(
        object,
        Object::Str(_)
            | Object::Filter(_)
            | Object::List(_)
            | Object::Map(_)
            | Object::Set(_)
            | Object::Record { .. }
            | Object::Enum { .. }
    )
}

/// The account an equality walk keeps of its own work: how many pairs it has
/// visited against what the comparison's charge paid for, and, once it has
/// outrun that, which pairs of objects it has already scheduled.
///
/// Slot indices identify the objects, not full handles: sound for the reason
/// the collector's mark bitmap is, because a walk allocates and frees nothing,
/// so no handle it meets can be a stale alias of a reused slot.
struct PairMemo {
    allowance: u64,
    limit: usize,
    visited: u64,
    #[allow(
        clippy::disallowed_types,
        reason = "only inserted into and measured, never iterated, so its order cannot reach a result"
    )]
    seen: Option<std::collections::HashSet<(u32, u32)>>,
}

impl PairMemo {
    fn new(allowance: u64, limit: usize) -> Self {
        Self {
            allowance,
            limit,
            visited: 0,
            seen: None,
        }
    }

    /// Counts one pair about to be visited. Past the allowance this starts
    /// the memo and pays for the next block of pairs before they are visited,
    /// each payment a safepoint (section 11.3, "slices").
    fn visit(
        &mut self,
        counters: &mut RunCounters,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<(), Fault> {
        self.visited = self.visited.saturating_add(1);
        if self.visited > self.allowance {
            if self.seen.is_none() {
                #[allow(
                    clippy::disallowed_types,
                    reason = "only inserted into and measured, never iterated"
                )]
                let fresh = std::collections::HashSet::new();
                self.seen = Some(fresh);
            }
            if (self.visited - self.allowance) % EQUALITY_BLOCK_PAIRS == 1 {
                charge_steps_sliced(counters, limits, control, EQUALITY_BLOCK_STEPS)?;
            }
        }
        Ok(())
    }

    /// Schedules the child pairs of a pair just visited. Once the memo is
    /// running, a pair of objects already scheduled is dropped.
    fn schedule(
        &mut self,
        stack: &mut Vec<(Value, Value)>,
        children: impl Iterator<Item = (Value, Value)>,
    ) -> Result<(), Fault> {
        let limit = self.limit;
        let Some(seen) = self.seen.as_mut() else {
            stack.extend(children);
            return Ok(());
        };
        for (x, y) in children {
            if let (Value::Heap(hx), Value::Heap(hy)) = (x, y) {
                if !seen.insert((hx.index(), hy.index())) {
                    continue;
                }
                if seen.len() > limit {
                    return Err(Fault::implementation_limit(
                        "the number of distinct pairs of values one comparison may walk",
                    ));
                }
            }
            stack.push((x, y));
        }
        Ok(())
    }
}

fn is_scalar(value: Value) -> bool {
    !matches!(value, Value::Heap(_))
}

/// A finite, non-negative-zero `Float` (section 3.4), or the fault an
/// out-of-range `Float` operation carries: HS1009 for a result that is not a
/// number, HS1010 for one that is infinite.
fn finite_float(x: f64) -> Result<Value, Fault> {
    if x.is_nan() {
        return Err(Fault::new(FaultCode::NotANumber, &[]));
    }
    if x.is_infinite() {
        return Err(Fault::new(FaultCode::Infinite, &[]));
    }
    Ok(Value::Float(if x == 0.0 { 0.0 } else { x }))
}

fn as_int(value: Value) -> Result<i64, Fault> {
    match value {
        Value::Int(i) => Ok(i),
        _ => Err(Fault::internal("this operation needs an Int")),
    }
}

fn as_float(value: Value) -> Result<f64, Fault> {
    match value {
        Value::Float(f) => Ok(f),
        _ => Err(Fault::internal("this operation needs a Float")),
    }
}

fn as_bool(value: Value) -> Result<bool, Fault> {
    match value {
        Value::Bool(b) => Ok(b),
        _ => Err(Fault::internal("this operation needs a Bool")),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "IntToFloat and Div's Float conversion are themselves the lossy \
              conversion the language defines; there is no lossless alternative"
)]
fn int_to_float(i: i64) -> f64 {
    i as f64
}

fn to_float_for_div(value: Value) -> Result<f64, Fault> {
    match value {
        Value::Int(i) => Ok(int_to_float(i)),
        Value::Float(f) => Ok(f),
        _ => Err(Fault::internal("Div needs an Int or a Float")),
    }
}

fn map_key_to_value(key: MapKey) -> Value {
    match key {
        MapKey::Bool(b) => Value::Bool(b),
        MapKey::Int(i) => Value::Int(i),
        MapKey::Str(handle) => Value::Heap(handle),
    }
}

/// The text of a `Float` (section 3.9): the shortest decimal that reads back
/// as the same value, positional for an exponent from -5 to 15 and
/// scientific otherwise.
#[allow(
    clippy::cast_sign_loss,
    reason = "each cast here is guarded by the branch above it already \
              having established the value is not negative"
)]
pub(crate) fn format_float(value: f64) -> String {
    if value == 0.0 {
        return "0.0".to_string();
    }
    let negative = value.is_sign_negative();
    let magnitude = value.abs();
    let scientific = format!("{magnitude:e}");
    let Some((mantissa, exponent_text)) = scientific.split_once('e') else {
        return scientific;
    };
    let exponent: i32 = exponent_text.parse().unwrap_or(0);
    let digits: Vec<u8> = mantissa.bytes().filter(|&b| b != b'.').collect();
    let digit_count = digits.len();
    let slice_text = |range: std::ops::Range<usize>| -> String {
        digits
            .get(range)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default()
    };
    let body = if (-5..=15).contains(&exponent) {
        if exponent >= 0 {
            let int_len = (exponent as usize).saturating_add(1);
            let mut integer_part = slice_text(0..int_len.min(digit_count));
            if int_len > digit_count {
                integer_part.push_str(&"0".repeat(int_len - digit_count));
            }
            let fractional = if int_len < digit_count {
                slice_text(int_len..digit_count)
            } else {
                "0".to_string()
            };
            format!("{integer_part}.{fractional}")
        } else {
            let zeros = "0".repeat((-exponent - 1) as usize);
            format!("0.{zeros}{}", slice_text(0..digit_count))
        }
    } else {
        let mut body = slice_text(0..1);
        if digit_count > 1 {
            body.push('.');
            body.push_str(&slice_text(1..digit_count));
        }
        body.push('e');
        body.push_str(&exponent.to_string());
        body
    };
    if negative { format!("-{body}") } else { body }
}

impl Interpreter {
    /// Prepares an interpreter for a verified module. Does not run the
    /// module's initialisers; call [`Interpreter::initialise`] first.
    #[must_use]
    pub fn new(module: VerifiedModule, capabilities: CapabilityTable) -> Interpreter {
        let globals = vec![Value::None; module.module().globals.len()];
        // Section 13.1's roster is resolved here, when the module is loaded,
        // so a program that names a library function this build does not
        // have is refused before it runs rather than part way through.
        let natives = crate::natives::resolve_imports(module.module());
        let data_types = crate::data::DataTypes::of(module.module());
        Interpreter {
            module,
            heap: Heap::new(),
            capabilities,
            globals,
            data_types,
            natives: natives.as_ref().map_or_else(|_| Vec::new(), Clone::clone),
            native_error: natives.err(),
            buffer: None,
            suspended: None,
            host_roots: Vec::new(),
            constant_strings: Vec::new(),
            constant_filters: Vec::new(),
        }
    }

    /// Remembers `value` as one the host is holding, so a collection never
    /// frees it out from under the host, mid-run or between runs. Does
    /// nothing for a scalar, which no collection could free anyway. Mirrors
    /// `crates/hollowscript`'s `Instance::remember`, which calls this
    /// whenever it adds to its own `host_roots`.
    pub fn remember_host_root(&mut self, value: Value) {
        if matches!(value, Value::Heap(_)) {
            self.heap.mark_escaped(value);
            self.host_roots.push(value);
        }
    }

    /// Forgets every value [`Interpreter::remember_host_root`] was given,
    /// mirroring `Instance::release_values`.
    pub fn forget_host_roots(&mut self) {
        self.host_roots.clear();
    }

    /// Forces a collection at every safepoint this interpreter reaches from
    /// now on, in place of the geometric-threshold policy `Heap` normally
    /// applies. The stress-test mode of `docs/design/performance.md`, used
    /// to prove that collecting as often as possible never changes what a
    /// program's run observes. An ordinary host has no reason to call this.
    pub fn enable_gc_stress_mode(&mut self) {
        self.heap.enable_collect_stress_mode();
    }

    /// The module.
    #[must_use]
    pub fn module(&self) -> &VerifiedModule {
        &self.module
    }

    /// Where this module keeps the prelude types of chapter 15.
    pub(crate) fn data_types(&self) -> &crate::data::DataTypes {
        &self.data_types
    }

    /// The heap.
    #[must_use]
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    /// Runs `build` against this interpreter's heap and host types, so that
    /// an embedder can make an argument value for
    /// [`Interpreter::call_export`]. A `String`, list, map, set, record or
    /// enum value is a handle into this heap and cannot be made anywhere
    /// else.
    ///
    /// The counters are this call's own, not a run's: building an argument
    /// happens between runs, where there is no run to charge. `limits` is
    /// still the instance's, so that `memory_remaining` answers against the
    /// budget the argument will actually be used under.
    ///
    /// # Errors
    ///
    /// Whatever `build` returns.
    pub fn with_host_call(
        &mut self,
        limits: &Limits,
        build: impl FnOnce(&mut crate::hostvalues::HostCall<'_>) -> Result<Value, Fault>,
    ) -> Result<Value, Fault> {
        let types = self.capabilities.host_types().clone();
        let mut counters = RunCounters::new();
        let mut call =
            crate::hostvalues::HostCall::new(&mut self.heap, &types, &mut counters, limits, None);
        build(&mut call)
    }

    /// As [`Interpreter::with_host_call`], but for reading rather than
    /// building: `read` answers any `R`, not only a fresh [`Value`], so an
    /// embedder can render an already-built value (a queued
    /// [`crate::Command`]'s own argument, say) back out through
    /// [`crate::hostvalues::HostCall`]'s read accessors (`list_of`,
    /// `record_of`, `enum_of` and so on) without needing to produce a new
    /// heap value of its own.
    pub fn with_host_call_read<R>(
        &mut self,
        limits: &Limits,
        read: impl FnOnce(&mut crate::hostvalues::HostCall<'_>) -> R,
    ) -> R {
        let types = self.capabilities.host_types().clone();
        let mut counters = RunCounters::new();
        let mut call =
            crate::hostvalues::HostCall::new(&mut self.heap, &types, &mut counters, limits, None);
        read(&mut call)
    }

    /// Where the instruction at `pc` of `function` came from, from the
    /// module's span table. `None` for a stripped module, which carries no
    /// spans, and for a function this compiler could not locate.
    fn location_of(&self, function: u32, pc: usize) -> Option<hollowscript_core::Location> {
        let module = self.module.module();
        let spans = &module.functions.get(function as usize)?.spans;
        let pc = u32::try_from(pc).ok()?;
        // The entry covering `pc` is the last one whose own pc is not above
        // it, which a binary search over the sorted table finds.
        let index = match spans.binary_search_by_key(&pc, |entry| entry.pc) {
            Ok(index) => index,
            Err(0) => return None,
            Err(index) => index - 1,
        };
        let entry = spans.get(index)?;
        let path = module
            .modules
            .get(entry.module as usize)
            .map(|m| module.string(m.path))
            .unwrap_or_default();
        Some(hollowscript_core::Location {
            file: std::sync::Arc::from(path),
            span: hollowscript_core::Span {
                start: entry.start,
                end: entry.end,
            },
            line: entry.line,
            column: entry.column,
        })
    }

    /// Where `at` is, if it is anywhere; see [`FaultAt`].
    fn location_at(&self, at: FaultAt) -> Option<hollowscript_core::Location> {
        at.and_then(|(function, pc)| self.location_of(function, pc))
    }

    /// `value` as text, in the form `corpus/FORMAT.md` fixes for a result
    /// ([`crate::render::render`]).
    #[must_use]
    pub fn render(&self, value: Value) -> String {
        crate::render::render(self.module.module(), &self.heap, value)
    }

    /// The current value of a global slot.
    #[must_use]
    pub fn global(&self, index: u32) -> Option<Value> {
        self.globals.get(index as usize).copied()
    }

    /// Runs the top-level statements of every module in initialisation order
    /// (section 10.5), in a fresh run. The result is the entry module's own
    /// outcome: `Some(value)` for an early `return`, `None` for running to
    /// the end.
    ///
    /// # Errors
    ///
    /// Any fault a module's top-level statements raise, or HS1003 if the
    /// program retains more than `limits.max_memory_bytes` once loaded.
    pub fn initialise(
        &mut self,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Option<Value>, Fault> {
        self.synchronous(self.buffer.is_some(), |vm| {
            vm.initialise_batched(limits, control)
        })
    }

    /// Puts this interpreter on the batched path of section 9.6's
    /// "deliberate simplifications" note and
    /// `docs/design/capabilities.md`: every capability call is queued in a
    /// [`CommandBuffer`] and the run pauses, instead of calling a
    /// [`crate::capability::HostFn`] from the middle of execution. A
    /// frame-based host (a game engine that owns the thread between `update`
    /// and `draw`) can then perform a whole slice's capability calls at its
    /// own frame boundary.
    ///
    /// Call this before the first run. Switching part way through leaves the
    /// calls already made synchronous, which is exactly what they were.
    pub fn use_command_buffer(&mut self) {
        self.buffer.get_or_insert_with(CommandBuffer::new);
    }

    /// The capability calls queued since the last drain, in order, removing
    /// them from the buffer. Empty when this interpreter is not on the
    /// batched path.
    pub fn commands(&mut self) -> Vec<Command> {
        self.buffer
            .as_mut()
            .map_or_else(Vec::new, CommandBuffer::drain)
    }

    /// Frees every heap object that nothing reachable from the globals, from
    /// [`Interpreter::remember_host_root`]'s roots or from `also` still
    /// holds, and answers how many were freed.
    ///
    /// Section 11.4 is explicit that "a garbage collector never changes when
    /// a limit is reached": the counters are computed from the specification's
    /// size formulas and know nothing about this, so a collection is purely a
    /// question of the host's real memory. Safe both between runs and, since
    /// mid-run collection was made safe (see `run_frames`'s safepoint check
    /// and `docs/design/performance.md`), while a run's frames are on the
    /// interpreter's own stack: `Interpreter::live_roots` roots them the
    /// same way the safepoint check does, so calling this directly, for
    /// example right after a run pauses at a suspended capability call, is
    /// exactly as safe as the automatic trigger.
    ///
    /// `also` is every extra value this one call wants kept, on top of the
    /// host's standing roots: a `Value` of a container kind is a handle into
    /// this heap, so a caller that wants one to survive but has not called
    /// [`Interpreter::remember_host_root`] for it must pass it here or see
    /// it freed underneath it.
    pub fn collect(&mut self, also: &[Value]) -> usize {
        let mut roots = self.live_roots();
        roots.extend_from_slice(also);
        self.heap.collect(&roots)
    }

    /// Every value presently live, for a collection run either automatically
    /// at a safepoint or explicitly through [`Interpreter::collect`]: the
    /// globals, the host's standing roots, every running frame's registers
    /// and closure captures, every library-function job's private storage,
    /// and, if a run is paused at a capability call, the suspended run's own
    /// frames (its registers were written back into the frame before the
    /// pause, exactly as they are between two instructions of a run that is
    /// not suspended, so rooting them is the same operation).
    fn live_roots(&self) -> Vec<Value> {
        let mut roots = self.globals.clone();
        roots.extend_from_slice(&self.host_roots);
        self.push_constant_roots(&mut roots);
        if let Some(suspended) = &self.suspended {
            push_frame_roots(&suspended.frames, &mut roots);
        }
        roots
    }

    /// Collects at the mid-run safepoint of `run_frames`: everything
    /// [`Interpreter::live_roots`] would find, plus `frames`, the run
    /// currently in progress (not yet in `self.suspended`, since it is still
    /// running). Never touches a [`RunCounters`] and cannot itself fault:
    /// collection is purely a question of the host's real memory (section
    /// 11.4), never of what a program's run observes.
    fn collect_at_safepoint(&mut self, frames: &[Frame]) {
        let mut roots = self.globals.clone();
        roots.extend_from_slice(&self.host_roots);
        self.push_constant_roots(&mut roots);
        push_frame_roots(frames, &mut roots);
        self.heap.collect(&roots);
    }

    /// Every constant `LoadK` has put on the heap so far (see
    /// `constant_strings`), which a later `LoadK` hands out again.
    fn push_constant_roots(&self, out: &mut Vec<Value>) {
        out.extend(self.constant_strings.iter().flatten().copied());
        out.extend(self.constant_filters.iter().flatten().copied());
    }

    /// The heap object for the `String` (or, with `filter`, the `Filter`)
    /// constant `id`, allocated the first time it is asked for and shared
    /// from then on. Not charged against the allocation counter: section
    /// 11.4 does not charge a literal with no interpolation, and because the
    /// object is made once per constant rather than once per execution, the
    /// real memory it takes is bounded by the module's own size.
    fn constant_object(&mut self, id: StringId, filter: bool) -> Result<Value, Fault> {
        let index = id.0 as usize;
        // The verifier refuses a constant whose id the string table does not
        // reach, so this never fails for a verified module. The check is here
        // as well because the size of the cache below follows the id: nothing
        // that is not in the table may ever be allowed to size it.
        if index >= self.module.module().strings.len() {
            return Err(Fault::internal(
                "a string constant names a string that does not exist",
            ));
        }
        let cached = if filter {
            &self.constant_filters
        } else {
            &self.constant_strings
        };
        if let Some(Some(value)) = cached.get(index) {
            return Ok(*value);
        }
        let text = self.module.module().string(id).to_string();
        let value = self.alloc_object(if filter {
            Object::Filter(text)
        } else {
            Object::Str(text)
        })?;
        let cache = if filter {
            &mut self.constant_filters
        } else {
            &mut self.constant_strings
        };
        if cache.len() <= index {
            cache.resize(index.saturating_add(1), None);
        }
        if let Some(slot) = cache.get_mut(index) {
            *slot = Some(value);
        }
        Ok(value)
    }

    /// Whether a run is paused waiting for the host to perform the queued
    /// calls.
    #[must_use]
    pub fn is_suspended(&self) -> bool {
        self.suspended.is_some()
    }

    /// [`Interpreter::initialise`] on the batched path, which may answer
    /// [`Run::Suspended`].
    ///
    /// # Errors
    ///
    /// Any fault a module's top-level statements raise, or HS1003 if the
    /// program retains more than `limits.max_memory_bytes` once loaded.
    pub fn initialise_batched(
        &mut self,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Run, Fault> {
        if control.is_cancelled() {
            return Err(Fault::cancelled());
        }
        if let Some(fault) = &self.native_error {
            return Err(fault.clone());
        }
        if self.suspended.is_some() {
            return Err(Fault::internal(
                "a run was started while another is suspended",
            ));
        }
        let mut counters = RunCounters::new();
        let mut inits: Vec<u32> = self
            .module
            .module()
            .modules
            .iter()
            .map(|entry| entry.init)
            .collect();
        inits.reverse();
        self.run_initialisers(inits, None, limits, control, &mut counters)
    }

    /// Runs the module initialisers left in `inits` (innermost last, so that
    /// popping takes them in initialisation order), carrying `result` from
    /// the ones already run. Shared by the first call and by a resume.
    fn run_initialisers(
        &mut self,
        mut inits: Vec<u32>,
        mut result: Option<Value>,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Run, Fault> {
        while let Some(init) = inits.pop() {
            match self.run_function(init, &[], &[], limits, control, counters)? {
                Run::Done(value) => result = value,
                Run::Suspended => {
                    if let Some(suspended) = self.suspended.as_mut() {
                        suspended.entry = Entry::Initialise(inits);
                    }
                    return Ok(Run::Suspended);
                }
            }
        }
        self.check_retained(limits)?;
        Ok(Run::Done(result))
    }

    /// Runs `body` and refuses a suspension, for the two synchronous entry
    /// points: a host that has not asked for the command buffer cannot be
    /// handed one, and a host that has must use the batched entry points.
    fn synchronous(
        &mut self,
        batched: bool,
        body: impl FnOnce(&mut Interpreter) -> Result<Run, Fault>,
    ) -> Result<Option<Value>, Fault> {
        if batched {
            return Err(Fault::internal(
                "a synchronous entry point was called on the batched path",
            ));
        }
        match body(self)? {
            Run::Done(value) => Ok(value),
            // Unreachable while `batched` is false, since nothing queues a
            // command; reported rather than unwrapped all the same.
            Run::Suspended => Err(Fault::internal("a synchronous run suspended")),
        }
    }

    /// Calls an exported function of the entry module (section 12.4), at
    /// call depth 1, in a fresh run.
    ///
    /// # Errors
    ///
    /// Any fault the function raises, or HS1003 if the instance retains more
    /// than `limits.max_memory_bytes` after the call.
    pub fn call_export(
        &mut self,
        function: u32,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Option<Value>, Fault> {
        self.synchronous(self.buffer.is_some(), |vm| {
            vm.call_export_batched(function, args, limits, control)
        })
    }

    /// [`Interpreter::call_export`] on the batched path, which may answer
    /// [`Run::Suspended`].
    ///
    /// # Errors
    ///
    /// Any fault the function raises, or HS1003 if the instance retains more
    /// than `limits.max_memory_bytes` after the call.
    pub fn call_export_batched(
        &mut self,
        function: u32,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Run, Fault> {
        if control.is_cancelled() {
            return Err(Fault::cancelled());
        }
        if let Some(fault) = &self.native_error {
            return Err(fault.clone());
        }
        if self.suspended.is_some() {
            return Err(Fault::internal(
                "a run was started while another is suspended",
            ));
        }
        // Export arguments are values the host may still hold after this
        // call. Preserve their old contents if the program appends to them.
        self.mark_escaped_values(args);
        let mut counters = RunCounters::new();
        charge_and_safepoint(&mut counters, limits, control, 1)?;
        counters.enter_call(limits)?;
        let run = self.run_function(function, args, &[], limits, control, &mut counters);
        self.finish_export(run, &mut counters, limits)
    }

    /// The tail of an exported call: the depth the entry point took is given
    /// back, and section 11.4's retained-memory check runs, unless the call
    /// paused, in which case both wait for the resume that finishes it.
    fn finish_export(
        &mut self,
        run: Result<Run, Fault>,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Run, Fault> {
        if matches!(run, Ok(Run::Suspended)) {
            // The depth this entry point took is still held while the run
            // is paused, and the retained check belongs to the resume that
            // finishes it.
            if let Some(suspended) = self.suspended.as_mut() {
                suspended.entry = Entry::Export;
            }
            return run;
        }
        counters.leave_call();
        let value = match run? {
            Run::Done(value) => value,
            Run::Suspended => return Ok(Run::Suspended),
        };
        if let Some(value) = value {
            self.heap.mark_escaped(value);
        }
        self.check_retained(limits)?;
        Ok(Run::Done(value))
    }

    /// Continues a run that [`Run::Suspended`] paused, with one
    /// [`CommandResult`] per [`Command`] the host drained, in the order the
    /// commands were queued.
    ///
    /// # Errors
    ///
    /// HS1012 if the host supplies a number of results that is not the
    /// number of queued calls, HS1013 if a result does not fit the type its
    /// capability declared, or whatever the run faults with once it
    /// continues.
    pub fn resume(
        &mut self,
        results: Vec<CommandResult>,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Run, Fault> {
        if control.is_cancelled() {
            return Err(Fault::cancelled());
        }
        let Some(suspended) = self.suspended.take() else {
            return Err(Fault::internal(
                "an interpreter that is not suspended was resumed",
            ));
        };
        let Suspension {
            mut frames,
            mut counters,
            mut fault_at,
            call,
            dest,
            pending,
            entry,
        } = suspended;
        if results.len() != pending {
            let name = self.cap_call_name(call);
            let message = format!(
                "the host supplied {} result(s) for {pending} queued call(s)",
                results.len()
            );
            return Err(locate(
                Fault::new(
                    FaultCode::CapabilityFailed,
                    &[("name", &name), ("message", &message)],
                ),
                self.location_at(fault_at),
            ));
        }
        // The last queued command is the one the run paused at; every
        // earlier one was performed for its effect alone.
        let Some(last) = results.into_iter().next_back() else {
            return Err(Fault::internal(
                "a resume with no result for the paused call",
            ));
        };
        let value = match last {
            CommandResult::Ok(value) => value,
            CommandResult::Err(fault) => return Err(locate(fault, self.location_at(fault_at))),
        };
        let value = self
            .finish_capability(call, value, limits, control, &mut counters)
            .map_err(|fault| locate(fault, self.location_at(fault_at)))?;
        let Some(Frame::Script(frame)) = frames.last_mut() else {
            return Err(Fault::internal("a suspended run has no frame to resume"));
        };
        set_reg(&mut frame.registers, dest, value)?;
        let run = self.drive(frames, limits, control, &mut counters, &mut fault_at);
        match entry {
            Entry::Export => self.finish_export(run, &mut counters, limits),
            Entry::Initialise(inits) => match run? {
                Run::Suspended => {
                    if let Some(suspended) = self.suspended.as_mut() {
                        suspended.entry = Entry::Initialise(inits);
                    }
                    Ok(Run::Suspended)
                }
                Run::Done(value) => {
                    self.run_initialisers(inits, value, limits, control, &mut counters)
                }
            },
        }
    }

    // -- Module table lookups, all range-checked rather than indexed directly. --

    fn function_arity_and_registers(&self, index: u32) -> Result<(u16, u16), Fault> {
        self.module
            .module()
            .functions
            .get(index as usize)
            .map(|f| (f.arity, f.register_count))
            .ok_or_else(|| Fault::internal("a function index is out of range"))
    }

    fn function_code_word(&self, index: u32, pc: usize) -> Result<u32, Fault> {
        self.module
            .module()
            .functions
            .get(index as usize)
            .and_then(|f| f.code.get(pc))
            .copied()
            .ok_or_else(|| Fault::internal("the program counter ran past the end of a function"))
    }

    /// The instruction at `*pc`, widened by `prefix`, the `Op::Ext` word
    /// just before it, advancing `*pc` past it. The verifier has already
    /// checked that a prefix is always followed by an instruction it fits,
    /// never by another prefix, so the result is never a prefix itself.
    #[cold]
    fn widen(&self, function: u32, pc: &mut usize, prefix: Instruction) -> Result<Wide, Fault> {
        let main = Instruction::decode(self.function_code_word(function, *pc)?)
            .map_err(|_| Fault::internal("a verified module carried an unknown opcode"))?;
        *pc += 1;
        if main.op == Op::Ext {
            return Err(Fault::internal("an operand prefix was followed by another"));
        }
        Ok(Wide::new(Some(prefix), main))
    }

    fn function_constant(&self, index: u32, slot: u32) -> Result<Constant, Fault> {
        self.module
            .module()
            .functions
            .get(index as usize)
            .and_then(|f| f.constants.get(slot as usize))
            .cloned()
            .ok_or_else(|| Fault::internal("a constant index is out of range"))
    }

    fn function_captures_of(&self, index: u32) -> Result<Vec<CaptureSource>, Fault> {
        self.module
            .module()
            .functions
            .get(index as usize)
            .map(|f| f.captures.clone())
            .ok_or_else(|| Fault::internal("a function index is out of range"))
    }

    fn shape_fields(&self, index: u32) -> Result<Vec<StringId>, Fault> {
        self.module
            .module()
            .shapes
            .get(index as usize)
            .map(|s| s.fields.clone())
            .ok_or_else(|| Fault::internal("a shape index is out of range"))
    }

    pub(crate) fn variant_desc(&self, index: u32) -> Result<VariantDesc, Fault> {
        self.module
            .module()
            .variants
            .get(index as usize)
            .copied()
            .ok_or_else(|| Fault::internal("a variant index is out of range"))
    }

    /// The roster position of the module's native import at `index`.
    fn native_position(&self, index: u32) -> Result<u32, Fault> {
        self.natives
            .get(index as usize)
            .copied()
            .ok_or_else(|| Fault::internal("a native import index is out of range"))
    }

    /// Whether `list.append` can grow its receiver without changing an old
    /// value still visible to the program or host. The argument register is
    /// excluded because it is the receiver being consumed by this call; any
    /// other live register containing the same handle is an observable alias.
    #[allow(
        clippy::too_many_arguments,
        reason = "the decision needs the native call, overwrite, and live-root context"
    )]
    fn can_append_list_in_place(
        &self,
        function: u32,
        next_pc: usize,
        position: u32,
        args: &[Value],
        registers: &[Value],
        receiver_register: usize,
        result_register: usize,
    ) -> bool {
        let Some(import) = hollowscript_core::native::ROSTER.get(position as usize) else {
            return false;
        };
        if import.name != "list.append" {
            return false;
        }
        let Some(Value::Heap(handle)) = args.first().copied() else {
            return false;
        };
        if registers.len() > MAX_APPEND_ALIAS_SCAN_SLOTS
            || self.globals.len() > MAX_APPEND_ALIAS_SCAN_SLOTS
        {
            return false;
        }
        if matches!(args.get(1), Some(Value::Heap(item)) if *item == handle) {
            return false;
        }
        if !self.heap.can_append_in_place(handle) {
            return false;
        }
        // The compiler emits the assignment immediately after the native
        // call. Its old binding is the one root being consumed by this
        // append, so it need not force a copy; every other root still does.
        let mut overwritten_global = None;
        let mut overwritten_register = None;
        if let Some(next) = self.peek_wide_instruction(function, next_pc) {
            match next.op {
                Op::SetGlobal if usize::from(next.a) == result_register => {
                    overwritten_global = usize::try_from(next.bx()).ok();
                }
                Op::Move if usize::from(next.b) == result_register => {
                    overwritten_register = Some(usize::from(next.a));
                }
                _ => {}
            }
        }
        let is_receiver = |value: &&Value| matches!(**value, Value::Heap(found) if found == handle);
        let mut references = self
            .globals
            .iter()
            .enumerate()
            .filter(|(index, value)| Some(*index) != overwritten_global && is_receiver(value))
            .count();
        references += registers
            .iter()
            .enumerate()
            .filter(|(index, value)| {
                *index != receiver_register
                    && Some(*index) != overwritten_register
                    && matches!(**value, Value::Heap(found) if found == handle)
            })
            .count();
        references == 0
    }

    fn peek_wide_instruction(&self, function: u32, pc: usize) -> Option<Wide> {
        let first = Instruction::decode(self.function_code_word(function, pc).ok()?).ok()?;
        if first.op == Op::Ext {
            let second =
                Instruction::decode(self.function_code_word(function, pc + 1).ok()?).ok()?;
            (second.op != Op::Ext).then(|| Wide::new(Some(first), second))
        } else {
            Some(Wide::new(None, first))
        }
    }

    /// A roster entry's declared arity, receiver included.
    fn native_arity(position: u32) -> Result<usize, Fault> {
        hollowscript_core::native::ROSTER
            .get(position as usize)
            .map(|info| usize::from(info.arity))
            .ok_or_else(|| Fault::internal("a native index is out of range"))
    }

    fn cap_call_arity(&self, index: u32) -> Result<usize, Fault> {
        let module = self.module.module();
        let call = module
            .cap_calls
            .get(index as usize)
            .ok_or_else(|| Fault::internal("a capability call index is out of range"))?;
        let capability = module
            .capabilities
            .get(call.capability as usize)
            .ok_or_else(|| Fault::internal("a capability index is out of range"))?;
        let params = match &capability.kind {
            CapabilityKind::Function(signature) => signature.params.len(),
            CapabilityKind::Namespace(functions) => functions
                .get(call.function as usize)
                .ok_or_else(|| Fault::internal("a capability function index is out of range"))?
                .1
                .params
                .len(),
        };
        Ok(params)
    }

    // -- Allocation. --

    pub(crate) fn alloc_object(&mut self, object: Object) -> Result<Value, Fault> {
        self.heap
            .alloc(object)
            .map_err(|_| Fault::internal("the heap's object count is exhausted"))
    }

    /// Allocates `object` and charges its node cost against the allocation
    /// counter (section 11.4). Used at every allocation site the
    /// specification lists. `LoadK` of a `String` or `Filter` constant is
    /// not one of them: it shares one object per constant instead (see
    /// `constant_object`).
    pub(crate) fn alloc_charged(
        &mut self,
        object: Object,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        let value = self.alloc_object(object)?;
        let cost = self.heap.node_cost(value);
        counters.charge_memory(cost, limits)?;
        Ok(value)
    }

    pub(crate) fn alloc_string_charged(
        &mut self,
        text: String,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        self.alloc_charged(Object::Str(text), counters, limits)
    }

    // -- Structural comparison. --

    /// Structural equality (section 6), computed iteratively so that a value
    /// nested as deep as the memory limit allows cannot exhaust the host's
    /// stack (section 11.4, "deep values"), and bounded in work however the
    /// two values share structure.
    ///
    /// Two handles that name the same object are equal without being walked,
    /// so a value compared with itself, or two values that share a large
    /// part, costs nothing for that part. That alone does not bound the walk:
    /// two values built by the same repeated doubling are different objects
    /// at every level, and walking every path through them visits a number of
    /// pairs exponential in the number of levels while the comparison's own
    /// charge, which counts each object once ([`Heap::tree_size_bounded`]),
    /// stays tiny. So the walk keeps its own account. `allowance` is the
    /// number of pairs the caller's up-front charge already paid for. Within
    /// it the walk is a plain traversal, as cheap as it has ever been. Past
    /// it, the walk starts to remember every pair of objects it has scheduled
    /// (a pair already scheduled has nothing new to find: a difference in it
    /// would already have ended the walk, or will), so each distinct pair is
    /// visited once whatever the number of paths to it, and it charges one
    /// step for every [`EQUALITY_BLOCK_PAIRS`] further pairs, before visiting
    /// them, with the safepoint rule of section 11.3, so the step limit
    /// and a cancellation both stop it. The memory that account may hold is
    /// itself bounded: a comparison that needs more than
    /// [`EQUALITY_MEMO_LIMIT`] distinct pairs faults with HS1015.
    pub(crate) fn values_equal(
        &self,
        a: Value,
        b: Value,
        allowance: u64,
        counters: &mut RunCounters,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<bool, Fault> {
        self.walk_equal(
            a,
            b,
            PairMemo::new(allowance, EQUALITY_MEMO_LIMIT),
            counters,
            limits,
            control,
        )
    }

    /// [`Interpreter::values_equal`] with its account supplied, so a test can
    /// give it a smaller memo limit than the real one.
    fn walk_equal(
        &self,
        a: Value,
        b: Value,
        mut pairs: PairMemo,
        counters: &mut RunCounters,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<bool, Fault> {
        let mut stack = vec![(a, b)];
        while let Some((x, y)) = stack.pop() {
            pairs.visit(counters, limits, control)?;
            match (x, y) {
                (Value::Int(x), Value::Int(y)) if x == y => {}
                (Value::Float(x), Value::Float(y)) if x.to_bits() == y.to_bits() => {}
                // Section 3.5: "Comparing an `Int` with a `Float` does not
                // convert the `Int` to `Float`; it compares the exact
                // values, so `9007199254740993 == 9007199254740992.0` is
                // `false`."
                (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i))
                    if compare_int_float(i, f) == Ordering::Equal => {}
                (Value::Bool(x), Value::Bool(y)) if x == y => {}
                (Value::None, Value::None) => {}
                (Value::Heap(ha), Value::Heap(hb)) => {
                    let (Some(oa), Some(ob)) = (self.heap.get(ha), self.heap.get(hb)) else {
                        return Ok(false);
                    };
                    // One object is equal to itself, whatever it holds. Only
                    // the kinds a program can compare qualify: a closure or
                    // a binding's cell is never equal to anything, and that
                    // stays so.
                    if ha == hb && is_comparable_object(oa) {
                        continue;
                    }
                    match (oa, ob) {
                        (Object::Str(x), Object::Str(y))
                        | (Object::Filter(x), Object::Filter(y)) => {
                            if x != y {
                                return Ok(false);
                            }
                        }
                        (Object::List(x), Object::List(y)) => {
                            if x.len() != y.len() {
                                return Ok(false);
                            }
                            pairs.schedule(&mut stack, x.iter().copied().zip(y.iter().copied()))?;
                        }
                        (Object::Set(x), Object::Set(y)) => {
                            if x.len() != y.len()
                                || x.iter()
                                    .zip(y.iter())
                                    .any(|(&kx, &ky)| !self.heap.keys_equal(kx, ky))
                            {
                                return Ok(false);
                            }
                        }
                        (Object::Map(x), Object::Map(y)) => {
                            if x.len() != y.len() {
                                return Ok(false);
                            }
                            for (ex, ey) in x.iter().zip(y.iter()) {
                                if !self.heap.keys_equal(ex.key, ey.key) {
                                    return Ok(false);
                                }
                            }
                            pairs.schedule(
                                &mut stack,
                                x.iter().zip(y.iter()).map(|(ex, ey)| (ex.value, ey.value)),
                            )?;
                        }
                        (Object::Record { fields: fx, .. }, Object::Record { fields: fy, .. }) => {
                            if fx.len() != fy.len() {
                                return Ok(false);
                            }
                            pairs
                                .schedule(&mut stack, fx.iter().copied().zip(fy.iter().copied()))?;
                        }
                        (
                            Object::Enum {
                                variant: vx,
                                payload: px,
                            },
                            Object::Enum {
                                variant: vy,
                                payload: py,
                            },
                        ) => {
                            if vx != vy || px.len() != py.len() {
                                return Ok(false);
                            }
                            pairs
                                .schedule(&mut stack, px.iter().copied().zip(py.iter().copied()))?;
                        }
                        _ => return Ok(false),
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Charges a comparison (section 11.3) before it is made: nothing for two
    /// scalars, otherwise `1 + floor(m / 64)` steps. Returns the number of
    /// operand pairs that charge paid for, which [`Interpreter::values_equal`]
    /// takes as its allowance (one pair for each 8 bytes of `m`, the size of
    /// the smallest thing a pair can stand for, and one more for the
    /// operands themselves).
    pub(crate) fn charge_comparison(
        &self,
        x: Value,
        y: Value,
        counters: &mut RunCounters,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<u64, Fault> {
        if is_scalar(x) && is_scalar(y) {
            return Ok(1);
        }
        // Bounded by the run's own step limit (scaled back up from steps to
        // the bytes a step charge divides by 64): once either side's tree
        // size alone would already charge past what the run has left, the
        // exact figure stops mattering, since the charge below is about to
        // fault either way, so the walk need not keep counting a value that
        // is, for instance, an enormous self-referential structure built
        // specifically to make this comparison expensive.
        //
        // `m` is the smaller of the two sizes, so the second walk need not
        // count past the first one's answer, and a scalar operand (size 0,
        // as in `found == none`) needs no walk of the other at all.
        let budget = limits.max_steps.saturating_mul(64);
        let size_y = if is_scalar(y) {
            0
        } else {
            self.heap.tree_size_bounded(y, budget)
        };
        let size_x = if is_scalar(x) {
            0
        } else {
            self.heap.tree_size_bounded(x, budget.min(size_y))
        };
        let m = size_x.min(size_y);
        charge_steps_sliced(counters, limits, control, 1 + m / 64)?;
        Ok(m / 8 + 1)
    }

    /// The ordering of two numbers or two strings (`Lt`, `Le`, `Gt`, `Ge`).
    pub(crate) fn compare_values(&self, a: Value, b: Value) -> Result<Ordering, Fault> {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => Ok(x.cmp(&y)),
            (Value::Float(x), Value::Float(y)) => x
                .partial_cmp(&y)
                .ok_or_else(|| Fault::internal("a Float comparison operand is not a number")),
            // Section 3.5 orders two numbers "in any combination, compared
            // by exact mathematical value".
            (Value::Int(i), Value::Float(f)) => Ok(compare_int_float(i, f)),
            (Value::Float(f), Value::Int(i)) => Ok(compare_int_float(i, f).reverse()),
            (Value::Heap(hx), Value::Heap(hy)) => match (self.heap.get(hx), self.heap.get(hy)) {
                // The same string is equal to itself: sorting or comparing a
                // list that holds one long string many times over must not
                // read that string once for every comparison.
                (Some(Object::Str(_)), Some(Object::Str(_))) if hx == hy => Ok(Ordering::Equal),
                (Some(Object::Str(x)), Some(Object::Str(y))) => Ok(x.cmp(y)),
                _ => Err(Fault::internal(
                    "Lt, Le, Gt and Ge need two numbers or two strings",
                )),
            },
            _ => Err(Fault::internal(
                "Lt, Le, Gt and Ge need two numbers or two strings",
            )),
        }
    }

    // -- Keys, maps. --

    pub(crate) fn to_map_key(&self, value: Value) -> Result<MapKey, Fault> {
        match value {
            Value::Bool(b) => Ok(MapKey::Bool(b)),
            Value::Int(i) => Ok(MapKey::Int(i)),
            Value::Heap(h) => match self.heap.get(h) {
                Some(Object::Str(_)) => Ok(MapKey::Str(h)),
                _ => Err(Fault::internal(
                    "a map key or set element must be a Bool, Int or String",
                )),
            },
            Value::Float(_) | Value::None => Err(Fault::internal(
                "a map key or set element must be a Bool, Int or String",
            )),
        }
    }

    /// Merges `pairs` into `entries`, kept sorted with no duplicate key (a
    /// later pair replaces an earlier one with the same key). Returns the
    /// merged entries and the number of genuinely new keys, for the caller
    /// to charge as section 11.4 says for `NewMap` and `MapPut` respectively.
    fn merge_map_entries(
        &self,
        mut entries: Vec<MapEntry>,
        pairs: &[(Value, Value)],
    ) -> Result<(Vec<MapEntry>, u64), Fault> {
        let mut new_keys: u64 = 0;
        for &(key_value, value) in pairs {
            let key = self.to_map_key(key_value)?;
            match entries.binary_search_by(|e| self.heap.compare_keys(e.key, key)) {
                Ok(index) => {
                    if let Some(slot) = entries.get_mut(index) {
                        slot.value = value;
                    }
                }
                Err(index) => {
                    entries.insert(index, MapEntry { key, value });
                    new_keys += 1;
                }
            }
        }
        Ok((entries, new_keys))
    }

    /// `Op::MapPut`: merges `pairs` into the map at `h` in place, a later
    /// pair replacing an earlier one with the same key.
    ///
    /// Like `Op::ListAppend`, this builds one long map literal a chunk at a
    /// time on a map nothing else can see yet, and charges what one `NewMap`
    /// of every pair would have: 24 bytes for each new key (section 11.4).
    /// A map is kept sorted, though, so a chunk whose keys fall before
    /// existing ones has to move those entries up to make room, work that
    /// grows with the map rather than with the chunk. That is charged one
    /// step for every 64 entries moved (the rate of section 11.3's `1 +
    /// floor(n / 64)`), before any of it is done, so a loop that keeps
    /// inserting at the front of a large map stays bounded by its steps. A
    /// literal whose keys ascend from one chunk to the next moves nothing and
    /// is charged nothing, as the specification's literal is; see
    /// `docs/design/bytecode.md` for the one case where they differ.
    fn map_put(
        &mut self,
        h: Handle,
        pairs: &[(Value, Value)],
        counters: &mut RunCounters,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<(), Fault> {
        // Every key is converted before anything is changed, so a key that
        // is not a key faults with the map untouched.
        let mut chunk: Vec<(MapKey, Value)> = Vec::with_capacity(pairs.len());
        for &(key, value) in pairs {
            chunk.push((self.to_map_key(key)?, value));
        }
        // Sorted by key, stably, and then only the last of each run of equal
        // keys kept: within one chunk as across chunks, the later pair wins.
        chunk.sort_by(|x, y| self.heap.compare_keys(x.0, y.0));
        let mut fresh: Vec<MapEntry> = Vec::with_capacity(chunk.len());
        let mut replaced: Vec<(usize, Value)> = Vec::new();
        let (moved, taken_capacity) = {
            let Some(Object::Map(entries)) = self.heap.get(h) else {
                return Err(Fault::internal("MapPut needs a map"));
            };
            let mut iter = chunk.into_iter().peekable();
            while let Some((key, value)) = iter.next() {
                if iter
                    .peek()
                    .is_some_and(|next| self.heap.keys_equal(next.0, key))
                {
                    continue;
                }
                match entries.binary_search_by(|e| self.heap.compare_keys(e.key, key)) {
                    Ok(index) => replaced.push((index, value)),
                    Err(_) => fresh.push(MapEntry { key, value }),
                }
            }
            let moved = fresh.first().map_or(0, |first| {
                let keep =
                    entries.partition_point(|e| self.heap.compare_keys(e.key, first.key).is_lt());
                entries.len() - keep
            });
            (moved, entries.capacity())
        };
        let steps = (moved / 64) as u64;
        if steps > 0 {
            charge_steps_sliced(counters, limits, control, steps)?;
        }
        counters.charge_memory(24 * fresh.len() as u64, limits)?;
        let Some(mut entries) = self.heap.take_map_entries(h) else {
            return Err(Fault::internal("MapPut needs a map"));
        };
        for (index, value) in replaced {
            if let Some(slot) = entries.get_mut(index) {
                slot.value = value;
            }
        }
        if let Some(first) = fresh.first() {
            let keep =
                entries.partition_point(|e| self.heap.compare_keys(e.key, first.key).is_lt());
            let tail = entries.split_off(keep);
            entries.reserve(tail.len() + fresh.len());
            let mut tail = tail.into_iter().peekable();
            for entry in fresh {
                while let Some(older) =
                    tail.next_if(|t| self.heap.compare_keys(t.key, entry.key).is_lt())
                {
                    entries.push(older);
                }
                entries.push(entry);
            }
            entries.extend(tail);
        }
        if !self.heap.put_map_entries(h, entries, taken_capacity) {
            return Err(Fault::internal("MapPut needs a map"));
        }
        Ok(())
    }

    fn index_value(&self, target: Value, key: Value) -> Result<Value, Fault> {
        let Value::Heap(h) = target else {
            return Err(Fault::internal("Index needs a list or a map"));
        };
        match self.heap.get(h) {
            Some(Object::List(items)) => {
                let i = as_int(key)?;
                Ok(usize::try_from(i)
                    .ok()
                    .and_then(|i| items.get(i))
                    .copied()
                    .unwrap_or(Value::None))
            }
            Some(Object::Map(entries)) => {
                let key = self.to_map_key(key)?;
                match entries.binary_search_by(|e| self.heap.compare_keys(e.key, key)) {
                    Ok(idx) => Ok(entries.get(idx).map_or(Value::None, |e| e.value)),
                    Err(_) => Ok(Value::None),
                }
            }
            _ => Err(Fault::internal("Index needs a list or a map")),
        }
    }

    // -- Iteration. --

    fn collection_len(&self, value: Value) -> Result<usize, Fault> {
        let Value::Heap(h) = value else {
            return Err(Fault::internal("iteration needs a list, a set or a map"));
        };
        match self.heap.get(h) {
            Some(Object::List(items)) => Ok(items.len()),
            Some(Object::Set(items)) => Ok(items.len()),
            Some(Object::Map(entries)) => Ok(entries.len()),
            _ => Err(Fault::internal("iteration needs a list, a set or a map")),
        }
    }

    /// The element at `index` of the list, set or map `value`: a list or set
    /// element as a plain value, or, for a map, a freshly allocated two field
    /// record of its key and its value (`IterNext`'s "a `MapEntry` record
    /// for a map"), charged as a new allocation.
    ///
    /// Nothing in the workspace yet compiles a `for` over a map, so this
    /// record's field order (key, then value) is a convention of this
    /// interpreter rather than a shape any compiler has agreed to; its
    /// `shape` is therefore empty rather than naming fields nothing has
    /// interned a name for.
    fn collection_element(
        &mut self,
        value: Value,
        index: usize,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        let Value::Heap(h) = value else {
            return Err(Fault::internal("iteration needs a list, a set or a map"));
        };
        match self.heap.get(h) {
            Some(Object::List(items)) => items
                .get(index)
                .copied()
                .ok_or_else(|| Fault::internal("an iterator cursor ran past the end of its list")),
            Some(Object::Set(items)) => items
                .get(index)
                .copied()
                .map(map_key_to_value)
                .ok_or_else(|| Fault::internal("an iterator cursor ran past the end of its set")),
            Some(Object::Map(entries)) => {
                let entry = entries.get(index).copied().ok_or_else(|| {
                    Fault::internal("an iterator cursor ran past the end of its map")
                })?;
                let fields = vec![map_key_to_value(entry.key), entry.value];
                self.alloc_charged(
                    Object::Record {
                        shape: Vec::new(),
                        fields,
                    },
                    counters,
                    limits,
                )
            }
            _ => Err(Fault::internal("iteration needs a list, a set or a map")),
        }
    }

    // -- Text. --

    fn text_of(
        &mut self,
        value: Value,
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        match value {
            Value::Bool(b) => self.alloc_string_charged(
                (if b { "true" } else { "false" }).into(),
                counters,
                limits,
            ),
            Value::Int(i) => self.alloc_string_charged(i.to_string(), counters, limits),
            Value::Float(f) => self.alloc_string_charged(format_float(f), counters, limits),
            Value::Heap(h) => match self.heap.get(h) {
                Some(Object::Str(_)) => Ok(value),
                _ => Err(Fault::internal(
                    "ToText needs an Int, a Float, a Bool or a String",
                )),
            },
            Value::None => Err(Fault::internal(
                "ToText needs an Int, a Float, a Bool or a String",
            )),
        }
    }

    /// Joins the parts of a string interpolation (section 3.9): each part is
    /// rendered as its text, and the finished string is charged once, before
    /// it is built, at 16 plus its total bytes. Section 11.4 is explicit that
    /// the text of an individual `${}` is not charged on its own, so the
    /// parts are measured rather than allocated on the heap one at a time.
    fn interpolate(
        &mut self,
        parts: &[Value],
        counters: &mut RunCounters,
        limits: &Limits,
    ) -> Result<Value, Fault> {
        // One `String` part and nothing else: the result is that string,
        // unchanged, so nothing is built and nothing is charged. This is
        // how the compiler lowers a string literal with no interpolation
        // (one text segment, no expressions), which section 11.4 does not
        // charge; the segment itself came from a shared, uncharged constant
        // (see `constant_object`). Any string with a `${}` has at least
        // three parts, so it still takes the charged path below.
        if let [part @ Value::Heap(h)] = parts {
            if matches!(self.heap.get(*h), Some(Object::Str(_))) {
                return Ok(*part);
            }
        }
        let mut rendered: Vec<Option<String>> = Vec::with_capacity(parts.len());
        let mut bytes: u64 = 0;
        for part in parts {
            let text = match *part {
                Value::Bool(b) => Some(if b { "true" } else { "false" }.to_string()),
                Value::Int(i) => Some(i.to_string()),
                Value::Float(f) => Some(format_float(f)),
                Value::Heap(h) => match self.heap.get(h) {
                    Some(Object::Str(_)) => None,
                    _ => return Err(Fault::internal(INTERP_OPERAND)),
                },
                Value::None => return Err(Fault::internal(INTERP_OPERAND)),
            };
            let len = match (&text, *part) {
                (Some(text), _) => text.len(),
                (None, Value::Heap(h)) => match self.heap.get(h) {
                    Some(Object::Str(s)) => s.len(),
                    _ => return Err(Fault::internal(INTERP_OPERAND)),
                },
                (None, _) => return Err(Fault::internal(INTERP_OPERAND)),
            };
            bytes = bytes.saturating_add(len as u64);
            rendered.push(text);
        }
        counters.charge_memory(bytes.saturating_add(16), limits)?;

        let mut text = String::with_capacity(usize::try_from(bytes).unwrap_or(0));
        for (part, own) in parts.iter().zip(rendered.iter()) {
            match own {
                Some(own) => text.push_str(own),
                None => match *part {
                    Value::Heap(h) => match self.heap.get(h) {
                        Some(Object::Str(s)) => text.push_str(s),
                        _ => return Err(Fault::internal(INTERP_OPERAND)),
                    },
                    _ => return Err(Fault::internal(INTERP_OPERAND)),
                },
            }
        }
        self.alloc_object(Object::Str(text))
    }

    // -- Calls. --

    /// Calls a capability function synchronously (section 9.6): the argument
    /// size charge before the call, the result size charge and the result's
    /// node cost after it, and the unconditional safepoint of section 11.7
    /// that follows every capability call regardless of the counter.
    fn call_capability(
        &mut self,
        index: u32,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Value, Fault> {
        // A capability implementation receives raw VM handles and may keep
        // a copy after returning. Values passed to it therefore cannot be
        // mutated by a subsequent append.
        self.mark_escaped_values(args);
        self.charge_capability_arguments(index, args, limits, control, counters)?;
        let result = self.capabilities.call(
            index,
            args,
            &mut self.heap,
            counters,
            limits,
            control.cancellation_token(),
        )?;
        self.finish_capability(index, result, limits, control, counters)
    }

    /// Section 9.6's acceptance rule applied to `args`, then the
    /// charge section 11.3 makes for what a capability is given, before the
    /// call. Split out because the batched path makes the same check and
    /// charge and then pauses instead of calling.
    fn charge_capability_arguments(
        &mut self,
        index: u32,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<(), Fault> {
        self.check_capability_arguments(index, args)?;
        let budget = limits.max_steps.saturating_mul(64);
        let arg_size: u64 = args
            .iter()
            .map(|&v| self.capability_size(v, budget))
            .fold(0u64, u64::saturating_add);
        charge_steps_sliced(counters, limits, control, 1 + arg_size / 64)?;
        // Section 11.6: the text's UTF-8 bytes and one LF, charged after the
        // step charge and before the host sees the call, because a call that
        // would pass the limit writes nothing at all.
        if self.capabilities.charges_output(index) {
            // The standard `print` has one `String` parameter and this is
            // that string's bytes. A capability that is merely *called*
            // `print` can declare any shape (a list of strings, say), and the
            // host's function behind it writes whatever it likes with what
            // it is given, so what is charged is the text the arguments
            // carry: see [`Interpreter::rendered_size`].
            let written = args
                .iter()
                .map(|&value| self.rendered_size(value))
                .fold(0u64, u64::saturating_add);
            counters.charge_output(written.saturating_add(1), limits)?;
        }
        Ok(())
    }

    /// The bytes of text a value carries, for a capability whose output is
    /// charged (section 11.6): a `String`'s UTF-8 bytes, the digits of an
    /// `Int` and the longest a `Float`, a `Bool` or `none` can be written in,
    /// and for a list, a set, a map, a record or an enum value the sum of what
    /// it holds, once for every path to it (a value a list holds twenty
    /// thousand times is written twenty thousand times, whatever it costs to
    /// hold), with one byte for each element, entry, field or payload as its
    /// separator and a map's key and value counted separately. For a single
    /// `String` this is exactly its length, which is what the standard `print`
    /// writes.
    ///
    /// The walk visits each distinct object once, however many paths lead to
    /// it (a post-order over the shared structure with the answer remembered
    /// for each object), so a value built by repeated doubling costs what it
    /// takes to hold and answers an astronomical size, saturating at
    /// `u64::MAX`, which no output limit allows. It is iterative for the same
    /// reason every walk over a value is, and a cell, which only a closure's
    /// captured binding makes, is counted as nothing.
    fn rendered_size(&self, root: Value) -> u64 {
        enum Step {
            Enter(Handle),
            Leave(Handle),
        }
        fn scalar(value: Value) -> u64 {
            match value {
                Value::Int(n) => {
                    u64::from(n.unsigned_abs().checked_ilog10().unwrap_or(0)) + 1 + u64::from(n < 0)
                }
                // The longest `{:?}` of an `f64` is 24 bytes (`-1.7976931348623157e308`).
                Value::Float(_) => 24,
                Value::Bool(_) => 5,
                Value::None => 4,
                Value::Heap(_) => 0,
            }
        }
        let Value::Heap(start) = root else {
            return scalar(root);
        };
        let mut sizes: BTreeMap<u32, u64> = BTreeMap::new();
        let mut visiting: BTreeSet<u32> = BTreeSet::new();
        let mut stack = vec![Step::Enter(start)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(handle) => {
                    let key = handle.index();
                    if sizes.contains_key(&key) || !visiting.insert(key) {
                        continue;
                    }
                    stack.push(Step::Leave(handle));
                    let mut push_heap = |value: Value| {
                        if let Value::Heap(child) = value {
                            if !sizes.contains_key(&child.index()) {
                                stack.push(Step::Enter(child));
                            }
                        }
                    };
                    match self.heap.get(handle) {
                        Some(Object::List(items)) => items.iter().copied().for_each(&mut push_heap),
                        Some(Object::Set(keys)) => {
                            for key in keys {
                                if let MapKey::Str(child) = *key {
                                    push_heap(Value::Heap(child));
                                }
                            }
                        }
                        Some(Object::Map(entries)) => {
                            for entry in entries {
                                if let MapKey::Str(child) = entry.key {
                                    push_heap(Value::Heap(child));
                                }
                                push_heap(entry.value);
                            }
                        }
                        Some(Object::Record { fields, .. }) => {
                            fields.iter().copied().for_each(&mut push_heap);
                        }
                        Some(Object::Enum { payload, .. }) => {
                            payload.iter().copied().for_each(&mut push_heap);
                        }
                        _ => {}
                    }
                }
                Step::Leave(handle) => {
                    let known = |value: Value| match value {
                        Value::Heap(child) => sizes.get(&child.index()).copied().unwrap_or(0),
                        other => scalar(other),
                    };
                    let key_size = |key: MapKey| match key {
                        MapKey::Str(child) => sizes.get(&child.index()).copied().unwrap_or(0),
                        MapKey::Int(n) => scalar(Value::Int(n)),
                        MapKey::Bool(b) => scalar(Value::Bool(b)),
                    };
                    let size = match self.heap.get(handle) {
                        Some(Object::Str(text) | Object::Filter(text)) => text.len() as u64,
                        Some(Object::List(items)) => items
                            .iter()
                            .map(|&v| known(v).saturating_add(1))
                            .fold(0u64, u64::saturating_add),
                        Some(Object::Set(keys)) => keys
                            .iter()
                            .map(|&k| key_size(k).saturating_add(1))
                            .fold(0u64, u64::saturating_add),
                        Some(Object::Map(entries)) => entries
                            .iter()
                            .map(|e| {
                                key_size(e.key)
                                    .saturating_add(known(e.value))
                                    .saturating_add(2)
                            })
                            .fold(0u64, u64::saturating_add),
                        Some(Object::Record { fields, .. }) => fields
                            .iter()
                            .map(|&v| known(v).saturating_add(1))
                            .fold(0u64, u64::saturating_add),
                        Some(Object::Enum { payload, .. }) => payload
                            .iter()
                            .map(|&v| known(v).saturating_add(1))
                            .fold(0u64, u64::saturating_add),
                        _ => 0,
                    };
                    visiting.remove(&handle.index());
                    sizes.insert(handle.index(), size);
                }
            }
        }
        sizes.get(&start.index()).copied().unwrap_or(0)
    }

    /// Everything that happens to a capability's result, whether it came
    /// back from a [`crate::capability::HostFn`] or from a
    /// [`CommandResult`] a resume supplied: section 9.6's acceptance check,
    /// section 11.3's result charges, and section 11.7's unconditional
    /// safepoint after a capability call.
    fn finish_capability(
        &mut self,
        index: u32,
        result: Value,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Value, Fault> {
        // Section 9.6: what the host handed back is not a language value
        // until it has been compared against the type the capability
        // declared. Charged after the check, since a value that is refused
        // never reaches the program.
        let result = self.accept_result(index, result)?;
        // The value came from the host, which may retain its original handle.
        self.heap.mark_escaped(result);
        let result_size = self.capability_size(result, limits.max_steps.saturating_mul(64));
        charge_steps_sliced(counters, limits, control, result_size / 64)?;
        let retained = self.heap.tree_size_bounded(result, limits.max_memory_bytes);
        counters.charge_memory(retained, limits)?;
        control.safepoint().map_err(|_| Fault::cancelled())?;
        Ok(result)
    }

    pub(crate) fn mark_escaped_values(&mut self, values: &[Value]) {
        for &value in values {
            self.heap.mark_escaped(value);
        }
    }

    /// The size of a value for a capability charge (section 11.3): element
    /// and entry counts plus the sizes of what they hold, not the node
    /// overhead [`Heap::tree_size`] counts; distinct from that function on
    /// purpose, since the two are defined by different sections for
    /// different charges. Cycle-safe and sharing-aware in the same way as
    /// [`Heap::tree_size_bounded`] (a handle already counted contributes
    /// nothing again, so a self-referential or heavily shared argument
    /// cannot loop forever or cost more than its distinct objects), and
    /// bounded the same way: once `total` already exceeds `budget`, the
    /// walk stops rather than keep counting a value only ever compared
    /// against a remaining charge budget.
    fn capability_size(&self, root: Value, budget: u64) -> u64 {
        // Only a `Value::Heap` element is worth pushing back onto `stack`: a
        // scalar (`Int`, `Float`, `Bool`, `none`) adds nothing to `total`
        // and has no children of its own, so pushing it only to immediately
        // discard it at the top of the loop cost nothing in what this
        // function computes, but a lot in what it had to allocate to get
        // there for a large list or record of scalars. See
        // `Heap::children`'s own doc comment for the fuller account; this
        // walk cannot share that code because it sums sizes as it goes
        // rather than only visiting, but it shares the same fix.
        let is_heap = |v: &Value| matches!(v, Value::Heap(_));
        let mut total: u64 = 0;
        let mut stack = vec![root];
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        while let Some(value) = stack.pop() {
            let Value::Heap(h) = value else { continue };
            if !seen.insert(h.index()) {
                continue;
            }
            let Some(object) = self.heap.get(h) else {
                continue;
            };
            match object {
                Object::Str(s) | Object::Filter(s) => {
                    total = total.saturating_add(s.len() as u64);
                }
                Object::List(items) => {
                    total = total.saturating_add(items.len() as u64);
                    stack.extend(items.iter().copied().filter(is_heap));
                }
                Object::Set(items) => {
                    total = total.saturating_add(items.len() as u64);
                    for key in items {
                        if let MapKey::Str(h) = *key {
                            stack.push(Value::Heap(h));
                        }
                    }
                }
                Object::Map(entries) => {
                    total = total.saturating_add(entries.len() as u64);
                    for entry in entries {
                        if let MapKey::Str(h) = entry.key {
                            stack.push(Value::Heap(h));
                        }
                        if is_heap(&entry.value) {
                            stack.push(entry.value);
                        }
                    }
                }
                Object::Record { fields, .. } => {
                    stack.extend(fields.iter().copied().filter(is_heap));
                }
                Object::Enum { payload, .. } => {
                    stack.extend(payload.iter().copied().filter(is_heap));
                }
                Object::Closure { .. } | Object::Builtin(_) | Object::Cell(_) => {}
            }
            if total > budget {
                return total;
            }
        }
        total
    }

    // -- Retained size (section 11.4), run after a load and after each call. --

    fn check_retained(&self, limits: &Limits) -> Result<(), Fault> {
        // Each binding's tree size is bounded by the run's own memory limit:
        // once one binding alone already retains more than the run is
        // allowed, `check_retained_size` below faults regardless of what
        // the rest add up to, so a walk need not keep counting past that
        // point (a value doubled 30 times "retains" more bytes than fit in
        // a `u64`, let alone the default 64 MiB).
        let budget = limits.max_memory_bytes;
        let top_level_sizes: Vec<u64> = self
            .globals
            .iter()
            .map(|&v| self.heap.tree_size_bounded(v, budget))
            .collect();
        let mut interner = Interner::new();
        let mut visited: BTreeSet<Handle> = BTreeSet::new();
        let mut captured: Vec<(hollowscript_core::Symbol, u64)> = Vec::new();
        let mut stack: Vec<Value> = self.globals.clone();
        // As in `Heap::children` (and `capability_size` above): only a
        // `Value::Heap` element can lead this walk to a captured cell, which
        // is the only thing it is looking for, so a scalar element is
        // filtered out before it is ever pushed, not after it is popped
        // again. A top-level list of a million plain `Int`s used to make
        // this walk (which section 11.4 requires after every load and every
        // call) copy all one million of them into `stack` for nothing.
        let is_heap = |v: &Value| matches!(v, Value::Heap(_));
        while let Some(value) = stack.pop() {
            let Value::Heap(h) = value else { continue };
            if !visited.insert(h) {
                continue;
            }
            let Some(object) = self.heap.get(h) else {
                continue;
            };
            match object {
                Object::Closure { captures, .. } => {
                    for &cell in captures {
                        let symbol = interner.intern(&cell.index().to_string());
                        let size = match self.heap.get(cell) {
                            Some(Object::Cell(inner)) => {
                                self.heap.tree_size_bounded(*inner, budget)
                            }
                            _ => 0,
                        };
                        captured.push((symbol, size));
                        stack.push(Value::Heap(cell));
                    }
                }
                Object::List(items) => stack.extend(items.iter().copied().filter(is_heap)),
                Object::Map(entries) => {
                    for entry in entries {
                        if let MapKey::Str(h) = entry.key {
                            stack.push(Value::Heap(h));
                        }
                        if is_heap(&entry.value) {
                            stack.push(entry.value);
                        }
                    }
                }
                Object::Set(items) => {
                    for key in items {
                        if let MapKey::Str(h) = *key {
                            stack.push(Value::Heap(h));
                        }
                    }
                }
                Object::Record { fields, .. } => {
                    stack.extend(fields.iter().copied().filter(is_heap));
                }
                Object::Enum { payload, .. } => {
                    stack.extend(payload.iter().copied().filter(is_heap));
                }
                Object::Cell(inner) => {
                    if is_heap(inner) {
                        stack.push(*inner);
                    }
                }
                Object::Str(_) | Object::Filter(_) | Object::Builtin(_) => {}
            }
        }
        check_retained_size(top_level_sizes.into_iter(), captured.into_iter(), limits)
    }

    /// Runs `function_index` to its `Return` or `ReturnNone`, over an
    /// explicit stack of [`Frame`]s: a script call pushes a frame and a
    /// return pops one, and the call is finished when the frame this call
    /// started in returns. `captures` is the closure's captured cells, empty
    /// for a function with no upvalues (every export and every module
    /// initialiser).
    #[allow(
        clippy::too_many_lines,
        reason = "one dispatch per opcode, in opcode order"
    )]
    fn run_function(
        &mut self,
        function_index: u32,
        args: &[Value],
        captures: &[Handle],
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Run, Fault> {
        let frames = vec![Frame::Script(self.new_frame(
            function_index,
            args,
            captures.to_vec(),
            0,
            0,
        )?)];
        let mut fault_at = None;
        self.drive(frames, limits, control, counters, &mut fault_at)
    }

    /// Runs `frames` to completion or to a pause, locating a fault at the
    /// instruction that raised it (section 19.3), which is the instruction
    /// the loop was on. Both the first call of a function and a resume go
    /// through here, so both locate a fault the same way.
    fn drive(
        &mut self,
        frames: Vec<Frame>,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
        fault_at: &mut FaultAt,
    ) -> Result<Run, Fault> {
        match self.run_frames(frames, limits, control, counters, fault_at) {
            Err(fault) => Err(locate(fault, self.location_at(*fault_at))),
            other => other,
        }
    }

    /// The frame loop itself. `fault_at` is where the instruction being run
    /// came from, kept current so that a fault can be located.
    fn run_frames(
        &mut self,
        mut frames: Vec<Frame>,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
        fault_at: &mut FaultAt,
    ) -> Result<Run, Fault> {
        loop {
            // The mid-run collection safepoint (section 11.7, "collection
            // policy" in docs/design/performance.md). This is the one place
            // in the whole frame loop that calls `Heap::collect`, and it is
            // reached only here, at the very top of the loop: every earlier
            // iteration finished by writing its frame's `registers` and
            // `captures` back before looping (see the end of the instruction
            // match below, and `advance_native`, which never takes a job's
            // fields out of `frames` the way a script frame's registers are
            // taken out for one instruction), so at this exact point every
            // value presently reachable is either in `self.globals`,
            // `self.host_roots`, or somewhere in `frames` itself. Nothing is
            // ever transiently only on the Rust stack across this check, so
            // `collect_at_safepoint` needs no extra bookkeeping to be
            // precise. `due_for_collection` is a single integer comparison,
            // so paying it on every instruction, rather than only at the
            // wider 1,024-step rhythm of `charge_and_safepoint`, costs
            // nothing measurable; only an actual collection, gated by that
            // comparison, is not free.
            if self.heap.due_for_collection() {
                self.collect_at_safepoint(&frames);
            }
            // A library function that calls a closure is driven a callback
            // at a time, without an instruction of its own.
            if matches!(frames.last(), Some(Frame::Native(_))) {
                let step = self.advance_native(&mut frames, limits, control, counters)?;
                match self.apply_step(&mut frames, step, limits, control, counters)? {
                    Outcome::Running => continue,
                    Outcome::Finished(value) => return Ok(Run::Done(value)),
                    Outcome::Suspended { call, dest } => {
                        return Ok(self.suspend(frames, *counters, *fault_at, call, dest));
                    }
                }
            }
            let Some(Frame::Script(frame)) = frames.last_mut() else {
                return Err(Fault::internal("the interpreter has no frame to run"));
            };
            let function_index = frame.function;
            let mut pc = frame.pc;
            // Moved out for the length of one instruction so that the body
            // below can hold them as plain locals while it also calls
            // methods of `self`; both go back into the frame directly after.
            let mut registers = std::mem::take(&mut frame.registers);
            let captures = std::mem::take(&mut frame.captures);
            let mut step = Step::Next;

            // Where this instruction came from, for a fault raised by it
            // (section 19.3). Recorded before it runs, because a fault
            // unwinds straight out of the loop.
            *fault_at = Some((function_index, pc));
            let word = self.function_code_word(function_index, pc)?;
            let mut instr = Wide::new(
                None,
                Instruction::decode(word)
                    .map_err(|_| Fault::internal("a verified module carried an unknown opcode"))?,
            );
            pc += 1;
            // An `Op::Ext` prefix is dispatched like any other opcode: its own
            // arm below widens the word after it and comes back here to run
            // that, so an instruction without a prefix, which is nearly
            // every one, pays nothing for prefixes existing.
            loop {
                match instr.op {
                    Op::Ext => {
                        let prefix = Instruction::abc(
                            Op::Ext,
                            instr.a.to_le_bytes()[0],
                            instr.b.to_le_bytes()[0],
                            instr.c.to_le_bytes()[0],
                        );
                        instr = self.widen(function_index, &mut pc, prefix)?;
                        continue;
                    }
                    Op::Move => {
                        let value = reg(&registers, instr.b)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::LoadK => {
                        let constant = self.function_constant(function_index, instr.bx())?;
                        let value = match constant {
                            Constant::Int(i) => Value::Int(i),
                            Constant::Float(f) => Value::Float(f),
                            // Shared, not copied: one heap object per constant,
                            // made on first use and uncharged, as section 11.4
                            // says of a literal with no interpolation (see
                            // `constant_object`). An earlier version allocated
                            // a fresh uncharged copy on every execution, which
                            // let a loop around a large constant grow real
                            // memory without bound; charging each copy instead
                            // made `print("spam")` in a long loop fault HS1002,
                            // which the specification does not allow.
                            Constant::Str(id) => self.constant_object(id, false)?,
                            Constant::Filter(id) => self.constant_object(id, true)?,
                        };
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::LoadInt => {
                        set_reg(&mut registers, instr.a, Value::Int(i64::from(instr.sbx())))?;
                    }
                    Op::LoadTrue => set_reg(&mut registers, instr.a, Value::Bool(true))?,
                    Op::LoadFalse => set_reg(&mut registers, instr.a, Value::Bool(false))?,
                    Op::LoadNone => set_reg(&mut registers, instr.a, Value::None)?,
                    Op::GetGlobal => {
                        let value = self
                            .globals
                            .get(instr.bx() as usize)
                            .copied()
                            .ok_or_else(|| Fault::internal("a global index is out of range"))?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::SetGlobal => {
                        let value = reg(&registers, instr.a)?;
                        let slot = self
                            .globals
                            .get_mut(instr.bx() as usize)
                            .ok_or_else(|| Fault::internal("a global index is out of range"))?;
                        *slot = value;
                    }
                    Op::NewCell => {
                        let value = reg(&registers, instr.b)?;
                        let cell = self.alloc_object(Object::Cell(value))?;
                        set_reg(&mut registers, instr.a, cell)?;
                    }
                    Op::GetCell => {
                        let Value::Heap(handle) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("GetCell needs a cell"));
                        };
                        let value = match self.heap.get(handle) {
                            Some(Object::Cell(v)) => *v,
                            _ => return Err(Fault::internal("GetCell needs a live cell")),
                        };
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::SetCell => {
                        let Value::Heap(handle) = reg(&registers, instr.a)? else {
                            return Err(Fault::internal("SetCell needs a cell"));
                        };
                        let value = reg(&registers, instr.b)?;
                        if !self.heap.set_cell(handle, value) {
                            return Err(Fault::internal("SetCell needs a live cell"));
                        }
                    }
                    Op::GetUpval => {
                        let handle = *captures
                            .get(instr.b as usize)
                            .ok_or_else(|| Fault::internal("an upvalue index is out of range"))?;
                        let value = match self.heap.get(handle) {
                            Some(Object::Cell(v)) => *v,
                            _ => return Err(Fault::internal("an upvalue cell is missing")),
                        };
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::SetUpval => {
                        let handle = *captures
                            .get(instr.b as usize)
                            .ok_or_else(|| Fault::internal("an upvalue index is out of range"))?;
                        let value = reg(&registers, instr.a)?;
                        if !self.heap.set_cell(handle, value) {
                            return Err(Fault::internal("an upvalue cell is missing"));
                        }
                    }

                    Op::IntToFloat => {
                        let i = as_int(reg(&registers, instr.b)?)?;
                        let value = finite_float(int_to_float(i))?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::AddInt => {
                        let x = as_int(reg(&registers, instr.b)?)?;
                        let y = as_int(reg(&registers, instr.c)?)?;
                        let result = x
                            .checked_add(y)
                            .ok_or_else(|| Fault::new(FaultCode::IntOverflow, &[]))?;
                        set_reg(&mut registers, instr.a, Value::Int(result))?;
                    }
                    Op::SubInt => {
                        let x = as_int(reg(&registers, instr.b)?)?;
                        let y = as_int(reg(&registers, instr.c)?)?;
                        let result = x
                            .checked_sub(y)
                            .ok_or_else(|| Fault::new(FaultCode::IntOverflow, &[]))?;
                        set_reg(&mut registers, instr.a, Value::Int(result))?;
                    }
                    Op::MulInt => {
                        let x = as_int(reg(&registers, instr.b)?)?;
                        let y = as_int(reg(&registers, instr.c)?)?;
                        let result = x
                            .checked_mul(y)
                            .ok_or_else(|| Fault::new(FaultCode::IntOverflow, &[]))?;
                        set_reg(&mut registers, instr.a, Value::Int(result))?;
                    }
                    Op::NegInt => {
                        let x = as_int(reg(&registers, instr.b)?)?;
                        let result = x
                            .checked_neg()
                            .ok_or_else(|| Fault::new(FaultCode::IntOverflow, &[]))?;
                        set_reg(&mut registers, instr.a, Value::Int(result))?;
                    }
                    Op::AddFloat => {
                        let x = as_float(reg(&registers, instr.b)?)?;
                        let y = as_float(reg(&registers, instr.c)?)?;
                        let value = finite_float(x + y)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::SubFloat => {
                        let x = as_float(reg(&registers, instr.b)?)?;
                        let y = as_float(reg(&registers, instr.c)?)?;
                        let value = finite_float(x - y)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::MulFloat => {
                        let x = as_float(reg(&registers, instr.b)?)?;
                        let y = as_float(reg(&registers, instr.c)?)?;
                        let value = finite_float(x * y)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::NegFloat => {
                        let x = as_float(reg(&registers, instr.b)?)?;
                        let value = finite_float(-x)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::Div => {
                        let x = to_float_for_div(reg(&registers, instr.b)?)?;
                        let y = to_float_for_div(reg(&registers, instr.c)?)?;
                        if y == 0.0 {
                            return Err(Fault::new(FaultCode::DivisionByZero, &[]));
                        }
                        let value = finite_float(x / y)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::Eq => {
                        let x = reg(&registers, instr.b)?;
                        let y = reg(&registers, instr.c)?;
                        let allowance = self.charge_comparison(x, y, counters, limits, control)?;
                        let result =
                            self.values_equal(x, y, allowance, counters, limits, control)?;
                        set_reg(&mut registers, instr.a, Value::Bool(result))?;
                    }
                    Op::Ne => {
                        let x = reg(&registers, instr.b)?;
                        let y = reg(&registers, instr.c)?;
                        let allowance = self.charge_comparison(x, y, counters, limits, control)?;
                        let result =
                            !self.values_equal(x, y, allowance, counters, limits, control)?;
                        set_reg(&mut registers, instr.a, Value::Bool(result))?;
                    }
                    Op::Lt => {
                        let x = reg(&registers, instr.b)?;
                        let y = reg(&registers, instr.c)?;
                        self.charge_comparison(x, y, counters, limits, control)?;
                        let result = self.compare_values(x, y)?.is_lt();
                        set_reg(&mut registers, instr.a, Value::Bool(result))?;
                    }
                    Op::Le => {
                        let x = reg(&registers, instr.b)?;
                        let y = reg(&registers, instr.c)?;
                        self.charge_comparison(x, y, counters, limits, control)?;
                        let result = self.compare_values(x, y)?.is_le();
                        set_reg(&mut registers, instr.a, Value::Bool(result))?;
                    }
                    Op::Gt => {
                        let x = reg(&registers, instr.b)?;
                        let y = reg(&registers, instr.c)?;
                        self.charge_comparison(x, y, counters, limits, control)?;
                        let result = self.compare_values(x, y)?.is_gt();
                        set_reg(&mut registers, instr.a, Value::Bool(result))?;
                    }
                    Op::Ge => {
                        let x = reg(&registers, instr.b)?;
                        let y = reg(&registers, instr.c)?;
                        self.charge_comparison(x, y, counters, limits, control)?;
                        let result = self.compare_values(x, y)?.is_ge();
                        set_reg(&mut registers, instr.a, Value::Bool(result))?;
                    }
                    Op::Not => {
                        let x = as_bool(reg(&registers, instr.b)?)?;
                        set_reg(&mut registers, instr.a, Value::Bool(!x))?;
                    }
                    Op::ToText => {
                        let x = reg(&registers, instr.b)?;
                        let value = self.text_of(x, counters, limits)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::Interp => {
                        let parts =
                            reg_run(&registers, usize::from(instr.b), usize::from(instr.c))?;
                        let value = self.interpolate(&parts, counters, limits)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }

                    Op::Jump => pc = jump(pc, i32::from(instr.sbx()))?,
                    Op::JumpIf => {
                        if as_bool(reg(&registers, instr.a)?)? {
                            pc = jump(pc, i32::from(instr.sbx()))?;
                        }
                    }
                    Op::JumpIfNot => {
                        if !as_bool(reg(&registers, instr.a)?)? {
                            pc = jump(pc, i32::from(instr.sbx()))?;
                        }
                    }
                    Op::JumpIfNone => {
                        if matches!(reg(&registers, instr.a)?, Value::None) {
                            pc = jump(pc, i32::from(instr.sbx()))?;
                        }
                    }
                    Op::JumpIfSome => {
                        if !matches!(reg(&registers, instr.a)?, Value::None) {
                            pc = jump(pc, i32::from(instr.sbx()))?;
                        }
                    }
                    Op::Charge => {
                        charge_and_safepoint(counters, limits, control, u64::from(instr.bx()))?;
                    }
                    Op::IterInit => {
                        let collection = reg(&registers, instr.b)?;
                        set_reg(&mut registers, instr.a, collection)?;
                        set_reg(&mut registers, instr.a.wrapping_add(1), Value::Int(0))?;
                    }
                    Op::IterNext => {
                        let a = instr.a;
                        let collection = reg(&registers, a)?;
                        let cursor = as_int(reg(&registers, a.wrapping_add(1))?)?;
                        let index = usize::try_from(cursor).unwrap_or(usize::MAX);
                        let len = self.collection_len(collection)?;
                        if index >= len {
                            // The collection is exhausted: there is no next
                            // iteration to charge for (section 11.3 charges
                            // "each iteration of a for or while loop", and
                            // finding that there is not one more is not one).
                            pc = jump(pc, i32::from(instr.sbx()))?;
                        } else {
                            charge_and_safepoint(counters, limits, control, 1)?;
                            let element =
                                self.collection_element(collection, index, counters, limits)?;
                            set_reg(&mut registers, a.wrapping_add(2), element)?;
                            let next = i64::try_from(index + 1).unwrap_or(i64::MAX);
                            set_reg(&mut registers, a.wrapping_add(1), Value::Int(next))?;
                        }
                    }
                    Op::JumpLong => pc = jump(pc, instr.ax())?,

                    Op::NewList => {
                        let count = instr.bx() as usize;
                        let items = reg_run(&registers, usize::from(instr.a) + 1, count)?;
                        let value = self.alloc_charged(Object::List(items), counters, limits)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::ListAppend => {
                        let Value::Heap(h) = reg(&registers, instr.a)? else {
                            return Err(Fault::internal("ListAppend needs a list"));
                        };
                        // In place: this builds one long list literal a chunk at
                        // a time after its first `NewList`, on a list nothing
                        // else can see yet, so the literal is charged exactly
                        // what one `NewList` of all its elements would be (8
                        // bytes for each, section 11.4, and no steps, section
                        // 11.3: a literal is not a call). The work is the `c`
                        // values copied in, at most 65,535, not the list's own
                        // length, which is why nothing here needs a step charge
                        // to keep a loop around it cancellable: the loop itself
                        // charges. A copy of the whole list per chunk made a
                        // long literal quadratic, and was charged steps to
                        // bound that, which a literal must not be.
                        let added =
                            reg_run(&registers, usize::from(instr.b), usize::from(instr.c))?;
                        counters.charge_memory(8 * added.len() as u64, limits)?;
                        if !self.heap.extend_list(h, &added) {
                            return Err(Fault::internal("ListAppend needs a list"));
                        }
                    }
                    Op::NewMap => {
                        let count = instr.bx() as usize;
                        let pairs = read_pairs(&registers, usize::from(instr.a) + 1, count)?;
                        let (entries, _) = self.merge_map_entries(Vec::new(), &pairs)?;
                        let value = self.alloc_charged(Object::Map(entries), counters, limits)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::MapPut => {
                        let Value::Heap(h) = reg(&registers, instr.a)? else {
                            return Err(Fault::internal("MapPut needs a map"));
                        };
                        let pairs =
                            read_pairs(&registers, usize::from(instr.b), usize::from(instr.c))?;
                        self.map_put(h, &pairs, counters, limits, control)?;
                    }
                    Op::NewRecord => {
                        let shape = self.shape_fields(instr.bx())?;
                        let fields = reg_run(&registers, usize::from(instr.a) + 1, shape.len())?;
                        let value =
                            self.alloc_charged(Object::Record { shape, fields }, counters, limits)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::NewEnum => {
                        let variant_index = instr.bx();
                        let desc = self.variant_desc(variant_index)?;
                        let payload =
                            reg_run(&registers, usize::from(instr.a) + 1, desc.arity as usize)?;
                        let value = self.alloc_charged(
                            Object::Enum {
                                variant: variant_index,
                                payload,
                            },
                            counters,
                            limits,
                        )?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::GetField => {
                        let Value::Heap(h) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("GetField needs a record"));
                        };
                        let value = match self.heap.get(h) {
                            Some(Object::Record { fields, .. }) => {
                                fields.get(instr.c as usize).copied().ok_or_else(|| {
                                    Fault::internal("a field index is out of range for its record")
                                })?
                            }
                            _ => return Err(Fault::internal("GetField needs a record")),
                        };
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::Index => {
                        let target = reg(&registers, instr.b)?;
                        let key = reg(&registers, instr.c)?;
                        let value = self.index_value(target, key)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::VariantIs => {
                        let Value::Heap(h) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("VariantIs needs an enum value"));
                        };
                        let is_match = match self.heap.get(h) {
                            Some(Object::Enum { variant, .. }) => {
                                let desc = self.variant_desc(*variant)?;
                                instr.c == desc.index
                            }
                            _ => return Err(Fault::internal("VariantIs needs an enum value")),
                        };
                        set_reg(&mut registers, instr.a, Value::Bool(is_match))?;
                    }
                    Op::GetPayload => {
                        let Value::Heap(h) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("GetPayload needs an enum value"));
                        };
                        let value = match self.heap.get(h) {
                            Some(Object::Enum { payload, .. }) => {
                                payload.get(instr.c as usize).copied().ok_or_else(|| {
                                    Fault::internal(
                                        "a payload index is out of range for its variant",
                                    )
                                })?
                            }
                            _ => return Err(Fault::internal("GetPayload needs an enum value")),
                        };
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::ListLen => {
                        let Value::Heap(h) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("ListLen needs a list"));
                        };
                        let len = match self.heap.get(h) {
                            Some(Object::List(items)) => items.len(),
                            _ => return Err(Fault::internal("ListLen needs a list")),
                        };
                        let value = Value::Int(i64::try_from(len).unwrap_or(i64::MAX));
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::ListGet => {
                        let Value::Heap(h) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("ListGet needs a list"));
                        };
                        let value = match self.heap.get(h) {
                            Some(Object::List(items)) => items
                                .get(instr.c as usize)
                                .copied()
                                .ok_or_else(|| Fault::internal("a list index is out of range"))?,
                            _ => return Err(Fault::internal("ListGet needs a list")),
                        };
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::ListRest => {
                        let Value::Heap(h) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("ListRest needs a list"));
                        };
                        let rest = match self.heap.get(h) {
                            Some(Object::List(items)) => items
                                .get(instr.c as usize..)
                                .map(<[Value]>::to_vec)
                                .ok_or_else(|| {
                                    Fault::internal("ListRest's count is greater than the list")
                                })?,
                            _ => return Err(Fault::internal("ListRest needs a list")),
                        };
                        let value = self.alloc_charged(Object::List(rest), counters, limits)?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::ListCheck => {
                        let Value::Heap(h) = reg(&registers, instr.a)? else {
                            return Err(Fault::internal("ListCheck needs a list"));
                        };
                        let len = match self.heap.get(h) {
                            Some(Object::List(items)) => items.len(),
                            _ => return Err(Fault::internal("ListCheck needs a list")),
                        };
                        let expected = instr.b as usize;
                        let at_least = instr.c == 1;
                        let ok = if at_least {
                            len >= expected
                        } else {
                            len == expected
                        };
                        if !ok {
                            return Err(Fault::new(
                                FaultCode::ListPattern,
                                &[("found", &len), ("expected", &expected)],
                            ));
                        }
                    }
                    Op::IsNone => {
                        let value = reg(&registers, instr.b)?;
                        set_reg(
                            &mut registers,
                            instr.a,
                            Value::Bool(matches!(value, Value::None)),
                        )?;
                    }

                    Op::Closure => {
                        let target = instr.bx();
                        let sources = self.function_captures_of(target)?;
                        let mut new_captures = Vec::with_capacity(sources.len());
                        for source in sources {
                            let handle = match source {
                                CaptureSource::Register(r) => {
                                    let Value::Heap(h) = reg(&registers, r)? else {
                                        return Err(Fault::internal(
                                            "a captured register does not hold a cell",
                                        ));
                                    };
                                    h
                                }
                                CaptureSource::Upvalue(u) => {
                                    *captures.get(u as usize).ok_or_else(|| {
                                        Fault::internal("an upvalue index is out of range")
                                    })?
                                }
                            };
                            new_captures.push(handle);
                        }
                        let value = self.alloc_charged(
                            Object::Closure {
                                function: target,
                                captures: new_captures,
                            },
                            counters,
                            limits,
                        )?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    // A function value that is not a script closure. None of
                    // the three captures anything, so each is the one index
                    // that names it, and `Op::Call` dispatches on the kind.
                    Op::VariantFn | Op::NativeFn | Op::CapFn => {
                        let which = match instr.op {
                            Op::VariantFn => Builtin::Variant(instr.bx()),
                            Op::NativeFn => Builtin::Native(instr.bx()),
                            _ => Builtin::Capability(instr.bx()),
                        };
                        let value = self.alloc_object(Object::Builtin(which))?;
                        set_reg(&mut registers, instr.a, value)?;
                    }
                    Op::Call => {
                        let Value::Heap(h) = reg(&registers, instr.a)? else {
                            return Err(Fault::internal("Call needs a function value"));
                        };
                        let callee = match self.heap.get(h) {
                            Some(Object::Closure { function, captures }) => {
                                Callee::Script(*function, captures.clone())
                            }
                            Some(&Object::Builtin(which)) => Callee::Builtin(which),
                            _ => return Err(Fault::internal("Call needs a function value")),
                        };
                        match callee {
                            Callee::Script(target, closure_captures) => {
                                let args = reg_run(
                                    &registers,
                                    usize::from(instr.a) + 1,
                                    instr.b as usize,
                                )?;
                                step = Step::Call {
                                    function: target,
                                    args,
                                    captures: closure_captures,
                                    dest: instr.a,
                                };
                            }
                            // The three built-in shapes are called here with the
                            // same arguments a direct `CallNative`, `NewEnum` or
                            // `CallCap` would have read, and charged the same.
                            Callee::Builtin(Builtin::Native(import)) => {
                                let position = self.native_position(import)?;
                                let arity = Self::native_arity(position)?;
                                let args = reg_run(&registers, usize::from(instr.a) + 1, arity)?;
                                let append_in_place = self.can_append_list_in_place(
                                    function_index,
                                    pc,
                                    position,
                                    &args,
                                    &registers,
                                    usize::from(instr.a) + 1,
                                    usize::from(instr.a),
                                );
                                match self.call_native(
                                    position,
                                    &args,
                                    append_in_place,
                                    limits,
                                    control,
                                    counters,
                                )? {
                                    crate::natives::NativeOutcome::Value(value) => {
                                        set_reg(&mut registers, instr.a, value)?;
                                    }
                                    crate::natives::NativeOutcome::Job(job) => {
                                        step = Step::StartNative { job, dest: instr.a };
                                    }
                                }
                            }
                            Callee::Builtin(Builtin::Variant(variant)) => {
                                let desc = self.variant_desc(variant)?;
                                let payload = reg_run(
                                    &registers,
                                    usize::from(instr.a) + 1,
                                    usize::from(desc.arity),
                                )?;
                                charge_and_safepoint(counters, limits, control, 1)?;
                                let value = self.alloc_charged(
                                    Object::Enum { variant, payload },
                                    counters,
                                    limits,
                                )?;
                                set_reg(&mut registers, instr.a, value)?;
                            }
                            Callee::Builtin(Builtin::Capability(call)) => {
                                let arity = self.cap_call_arity(call)?;
                                let args = reg_run(&registers, usize::from(instr.a) + 1, arity)?;
                                if self.buffer.is_some() {
                                    self.charge_capability_arguments(
                                        call, &args, limits, control, counters,
                                    )?;
                                    step = Step::Suspend {
                                        call,
                                        args,
                                        dest: instr.a,
                                    };
                                } else {
                                    let result = self
                                        .call_capability(call, &args, limits, control, counters)?;
                                    set_reg(&mut registers, instr.a, result)?;
                                }
                            }
                        }
                    }
                    Op::CallFn => {
                        let target = instr.bx();
                        let (arity, _) = self.function_arity_and_registers(target)?;
                        let args = reg_run(&registers, usize::from(instr.a) + 1, arity as usize)?;
                        step = Step::Call {
                            function: target,
                            args,
                            captures: Vec::new(),
                            dest: instr.a,
                        };
                    }
                    Op::CallNative => {
                        let import = instr.bx();
                        let position = *self.natives.get(import as usize).ok_or_else(|| {
                            Fault::internal("a native import index is out of range")
                        })?;
                        let arity = usize::from(
                            hollowscript_core::native::ROSTER
                                .get(position as usize)
                                .ok_or_else(|| Fault::internal("a native index is out of range"))?
                                .arity,
                        );
                        let call_args = reg_run(&registers, usize::from(instr.a) + 1, arity)?;
                        let append_in_place = self.can_append_list_in_place(
                            function_index,
                            pc,
                            position,
                            &call_args,
                            &registers,
                            usize::from(instr.a) + 1,
                            usize::from(instr.a),
                        );
                        match self.call_native(
                            position,
                            &call_args,
                            append_in_place,
                            limits,
                            control,
                            counters,
                        )? {
                            crate::natives::NativeOutcome::Value(value) => {
                                set_reg(&mut registers, instr.a, value)?;
                            }
                            crate::natives::NativeOutcome::Job(job) => {
                                step = Step::StartNative { job, dest: instr.a };
                            }
                        }
                    }
                    Op::CallCap => {
                        let target = instr.bx();
                        let arity = self.cap_call_arity(target)?;
                        let args = reg_run(&registers, usize::from(instr.a) + 1, arity)?;
                        if self.buffer.is_some() {
                            // The batched path: charge the arguments as the
                            // synchronous path does, queue the call, and pause.
                            // The result lands in `instr.a` on resume, which is
                            // where the synchronous path puts it now.
                            self.mark_escaped_values(&args);
                            self.charge_capability_arguments(
                                target, &args, limits, control, counters,
                            )?;
                            step = Step::Suspend {
                                call: target,
                                args,
                                dest: instr.a,
                            };
                        } else {
                            let result =
                                self.call_capability(target, &args, limits, control, counters)?;
                            set_reg(&mut registers, instr.a, result)?;
                        }
                    }
                    Op::Return => {
                        let value = reg(&registers, instr.a)?;
                        step = Step::Return(Some(value));
                    }
                    Op::ReturnNone => step = Step::Return(None),
                    Op::Try => {
                        let Value::Heap(h) = reg(&registers, instr.b)? else {
                            return Err(Fault::internal("? needs a Result value"));
                        };
                        match self.heap.get(h) {
                            Some(Object::Enum { variant, payload }) => {
                                let desc = self.variant_desc(*variant)?;
                                if desc.index == 0 {
                                    let ok_value = payload.first().copied().ok_or_else(|| {
                                        Fault::internal("Result.Ok carries no payload")
                                    })?;
                                    set_reg(&mut registers, instr.a, ok_value)?;
                                } else {
                                    step = Step::Return(Some(Value::Heap(h)));
                                }
                            }
                            _ => return Err(Fault::internal("? needs a Result value")),
                        }
                    }
                    Op::Trap => {
                        return Err(Fault::internal("reached code a correct program never runs"));
                    }
                }
                break;
            }

            let Some(Frame::Script(frame)) = frames.last_mut() else {
                return Err(Fault::internal("the interpreter lost its frame"));
            };
            frame.registers = registers;
            frame.captures = captures;
            frame.pc = pc;

            match self.apply_step(&mut frames, step, limits, control, counters)? {
                Outcome::Running => {}
                Outcome::Finished(value) => return Ok(Run::Done(value)),
                Outcome::Suspended { call, dest } => {
                    return Ok(self.suspend(frames, *counters, *fault_at, call, dest));
                }
            }
        }
    }

    /// Puts a paused run down: the frames, the counters and where the loop
    /// was, so that [`Interpreter::resume`] can pick all three up again.
    /// `entry` is filled in by the entry point the run started in, which is
    /// the only place that knows what still has to happen after it.
    fn suspend(
        &mut self,
        frames: Vec<Frame>,
        counters: RunCounters,
        fault_at: FaultAt,
        call: u32,
        dest: u16,
    ) -> Run {
        let pending = self.buffer.as_ref().map_or(0, CommandBuffer::len);
        self.suspended = Some(Suspension {
            frames,
            counters,
            fault_at,
            call,
            dest,
            pending,
            entry: Entry::Export,
        });
        Run::Suspended
    }

    /// Acts on one [`Step`]. Returns [`Outcome::Finished`] once the frame
    /// the call started in has returned, with the value of the whole call.
    fn apply_step(
        &mut self,
        frames: &mut Vec<Frame>,
        step: Step,
        limits: &Limits,
        control: &mut Control<'_>,
        counters: &mut RunCounters,
    ) -> Result<Outcome, Fault> {
        match step {
            Step::Next => {}
            Step::Call {
                function,
                args,
                captures,
                dest,
            } => {
                // Section 11.3 charges the call and section 11.5 bounds the
                // depth, both before the frame is built.
                charge_and_safepoint(counters, limits, control, 1)?;
                counters.enter_call(limits)?;
                // The caller's frame is not part of the bounded alias scan
                // while this callee runs. Mark direct list arguments shared
                // so an append in the callee cannot mutate a caller-visible
                // value.
                self.mark_escaped_values(&args);
                // The nearest script frame below: at most one library
                // function's job sits between two script frames.
                let below = frames
                    .iter()
                    .rev()
                    .find_map(|f| match f {
                        Frame::Script(s) => Some(s.stacked),
                        Frame::Native(_) => None,
                    })
                    .unwrap_or(0);
                let frame = self.new_frame(function, &args, captures, dest, below)?;
                if frame.stacked > stacked_register_budget(limits) {
                    return Err(Fault::implementation_limit(&format!(
                        "the calls in progress hold at most {} values between them",
                        stacked_register_budget(limits)
                    )));
                }
                frames.push(Frame::Script(frame));
            }
            Step::StartNative { mut job, dest } => {
                job.dest = dest;
                frames.push(Frame::Native(job));
            }
            Step::Suspend { call, args, dest } => {
                let Some(buffer) = self.buffer.as_mut() else {
                    return Err(Fault::internal(
                        "a capability call was queued with no command buffer",
                    ));
                };
                buffer.push(Command {
                    function: call,
                    args,
                });
                return Ok(Outcome::Suspended { call, dest });
            }
            Step::Return(value) => {
                let finished = frames
                    .pop()
                    .ok_or_else(|| Fault::internal("a return from no frame"))?;
                let dest = match &finished {
                    Frame::Script(frame) => frame.dest,
                    Frame::Native(job) => job.dest,
                };
                let from_script = matches!(finished, Frame::Script(_));
                match frames.last_mut() {
                    // The frame this call started in: the run is over.
                    None => return Ok(Outcome::Finished(value)),
                    Some(Frame::Script(caller)) => {
                        if from_script {
                            counters.leave_call();
                        }
                        set_reg(&mut caller.registers, dest, value.unwrap_or(Value::None))?;
                    }
                    // A library function was waiting for this callback's
                    // value; it takes it rather than a register.
                    Some(Frame::Native(job)) => {
                        if from_script {
                            counters.leave_call();
                        }
                        job.last = Some(value.unwrap_or(Value::None));
                    }
                }
            }
        }
        Ok(Outcome::Running)
    }

    /// A fresh frame for `function`, with `args` copied into its first
    /// registers and the rest set to `none`, stacked on `below` registers of
    /// the script frames under it.
    fn new_frame(
        &self,
        function: u32,
        args: &[Value],
        captures: Vec<Handle>,
        dest: u16,
        below: u64,
    ) -> Result<ScriptFrame, Fault> {
        let (arity, register_count) = self.function_arity_and_registers(function)?;
        let mut registers = vec![Value::None; register_count as usize];
        for (slot, value) in registers.iter_mut().zip(args.iter()).take(arity as usize) {
            *slot = *value;
        }
        Ok(ScriptFrame {
            function,
            captures,
            registers,
            pc: 0,
            dest,
            stacked: below.saturating_add(u64::from(register_count)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Interpreter, PairMemo, format_float};
    use crate::capability::CapabilityTable;
    use crate::limits::RunCounters;
    use crate::value::{Object, Value};
    use hollowscript_bytecode::module::{
        CapCall, CaptureSource, Constant, Function, FunctionKind, Global, Module, ModuleEntry,
        Shape,
    };
    use hollowscript_bytecode::opcode::{Instruction, Op};
    use hollowscript_bytecode::verify::{VerifiedModule, verify};
    use hollowscript_core::fault::FaultCode;
    use hollowscript_core::{Control, Limits};

    fn sample() -> VerifiedModule {
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
        verify(m).unwrap()
    }

    /// A value built by doubling `levels` times: each level is a list of the
    /// previous level twice, so it has `levels + 1` distinct objects and
    /// 2^levels paths through it.
    fn doubled(vm: &mut Interpreter, levels: u32) -> Value {
        let mut value = Value::Int(1);
        for _ in 0..levels {
            value = vm.alloc_object(Object::List(vec![value, value])).unwrap();
        }
        value
    }

    /// A value whose levels are `width` objects wide, each one a list of
    /// three entries of the level below, so that the number of distinct
    /// pairs of objects a comparison against another such value can reach
    /// grows with the cube of the number of levels. `flip` changes which
    /// entry of a node is the neighbour, so two of them are equal as trees
    /// (every leaf is the same `Int`) while sharing nothing and having
    /// different shapes underneath.
    fn lattice(vm: &mut Interpreter, levels: usize, width: usize, flip: bool) -> Value {
        let mut row = vec![Value::Int(0); width];
        for _ in 0..levels {
            let below = row.clone();
            for (j, slot) in row.iter_mut().enumerate() {
                let here = below[j];
                let next = below[(j + 1) % width];
                let items = if flip {
                    vec![here, here, next]
                } else {
                    vec![here, next, here]
                };
                *slot = vm.alloc_object(Object::List(items)).unwrap();
            }
        }
        row[0]
    }

    fn equality_vm() -> Interpreter {
        Interpreter::new(sample(), CapabilityTable::new(vec![]))
    }

    #[test]
    fn equality_of_two_doubled_values_visits_each_pair_once() {
        let mut vm = equality_vm();
        let a = doubled(&mut vm, 60);
        let b = doubled(&mut vm, 60);
        let mut counters = RunCounters::new();
        let equal = vm
            .values_equal(
                a,
                b,
                1,
                &mut counters,
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(equal);
        // 2^60 paths, but 61 pairs: one block of charge, not billions.
        assert!(counters.steps() <= 64, "{} steps", counters.steps());
    }

    #[test]
    fn equality_of_two_doubled_values_that_differ_at_the_bottom_is_false() {
        let mut vm = equality_vm();
        let a = doubled(&mut vm, 40);
        let mut b = Value::Int(2);
        for _ in 0..40 {
            b = vm.alloc_object(Object::List(vec![b, b])).unwrap();
        }
        let equal = vm
            .values_equal(
                a,
                b,
                1,
                &mut RunCounters::new(),
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(!equal);
    }

    #[test]
    fn equality_of_an_object_with_itself_is_true_without_a_walk() {
        let mut vm = equality_vm();
        let a = doubled(&mut vm, 60);
        let mut counters = RunCounters::new();
        let equal = vm
            .values_equal(
                a,
                a,
                1,
                &mut counters,
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(equal);
        assert_eq!(counters.steps(), 0);
    }

    #[test]
    fn a_closure_is_still_not_equal_to_itself() {
        let mut vm = equality_vm();
        let closure = vm
            .alloc_object(Object::Closure {
                function: 0,
                captures: vec![],
            })
            .unwrap();
        let equal = vm
            .values_equal(
                closure,
                closure,
                1,
                &mut RunCounters::new(),
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(!equal);
    }

    #[test]
    fn the_pairs_past_the_allowance_are_charged_in_steps_and_the_step_limit_stops_them() {
        let mut vm = equality_vm();
        let a = lattice(&mut vm, 60, 60, false);
        let b = lattice(&mut vm, 60, 60, true);
        // With room to finish the walk completes, and what it cost is
        // visible in the counter.
        let mut counters = RunCounters::new();
        let equal = vm
            .values_equal(
                a,
                b,
                1,
                &mut counters,
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(equal);
        assert!(counters.steps() > 1_000, "{} steps", counters.steps());
        // A step limit below that cost faults the comparison instead.
        let limits = Limits {
            max_steps: counters.steps() / 2,
            ..Limits::default()
        };
        let fault = vm
            .values_equal(
                a,
                b,
                1,
                &mut RunCounters::new(),
                &limits,
                &mut Control::none(),
            )
            .unwrap_err();
        assert_eq!(fault.code, FaultCode::StepLimit);
    }

    #[test]
    fn a_walk_within_its_allowance_is_not_charged() {
        let mut vm = equality_vm();
        // Two separately built lists of 1,000 integers: a thousand and one
        // pairs, which an allowance of that size has already paid for.
        let items: Vec<Value> = (0..1_000).map(Value::Int).collect();
        let a = vm.alloc_object(Object::List(items.clone())).unwrap();
        let b = vm.alloc_object(Object::List(items)).unwrap();
        let mut counters = RunCounters::new();
        let equal = vm
            .values_equal(
                a,
                b,
                1_001,
                &mut counters,
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(equal);
        assert_eq!(counters.steps(), 0);
    }

    #[test]
    fn a_cancelled_token_stops_a_long_equality_walk_at_a_safepoint() {
        let mut vm = equality_vm();
        let a = lattice(&mut vm, 80, 80, false);
        let b = lattice(&mut vm, 80, 80, true);
        let token = hollowscript_core::cancel::CancellationToken::new();
        token.cancel();
        let fault = vm
            .values_equal(
                a,
                b,
                1,
                &mut RunCounters::new(),
                &Limits::default(),
                &mut Control::new(Some(&token), None),
            )
            .unwrap_err();
        assert_eq!(fault.code, FaultCode::Cancelled);
    }

    #[test]
    fn a_walk_that_needs_more_pairs_than_the_memo_may_hold_faults_with_the_implementation_limit() {
        let mut vm = equality_vm();
        let a = lattice(&mut vm, 60, 60, false);
        let b = lattice(&mut vm, 60, 60, true);
        let fault = vm
            .walk_equal(
                a,
                b,
                PairMemo::new(1, 1_000),
                &mut RunCounters::new(),
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap_err();
        assert_eq!(fault.code, FaultCode::ImplementationLimit);
    }

    // ---- the text a capability whose output is charged is given ----------

    fn rendered(vm: &Interpreter, value: Value) -> u64 {
        vm.rendered_size(value)
    }

    #[test]
    fn the_rendered_size_of_a_string_is_its_bytes_and_of_scalars_their_text() {
        let mut vm = equality_vm();
        let text = vm
            .alloc_object(Object::Str("h\u{e9}llo".to_owned()))
            .unwrap();
        assert_eq!(rendered(&vm, text), 6, "bytes, not characters");
        assert_eq!(rendered(&vm, Value::Int(0)), 1);
        assert_eq!(rendered(&vm, Value::Int(-5)), 2);
        assert_eq!(rendered(&vm, Value::Int(1_234_567)), 7);
        assert_eq!(rendered(&vm, Value::Int(i64::MIN)), 20);
        assert_eq!(rendered(&vm, Value::Int(i64::MAX)), 19);
        assert_eq!(rendered(&vm, Value::None), 4);
        assert_eq!(rendered(&vm, Value::Bool(false)), 5);
    }

    #[test]
    fn a_value_a_list_holds_many_times_is_counted_every_time() {
        let mut vm = equality_vm();
        let text = vm.alloc_object(Object::Str("x".repeat(100))).unwrap();
        let list = vm.alloc_object(Object::List(vec![text; 20_000])).unwrap();
        // 100 bytes and a separator, for each of 20,000 elements, though the
        // string is held once.
        assert_eq!(rendered(&vm, list), 20_000 * 101);
    }

    #[test]
    fn a_doubled_value_answers_an_astronomical_size_without_walking_every_path() {
        let mut vm = equality_vm();
        let mut value = vm.alloc_object(Object::Str("ab".to_owned())).unwrap();
        for _ in 0..100 {
            value = vm.alloc_object(Object::List(vec![value, value])).unwrap();
        }
        // Two to the hundredth paths: the answer saturates, and no output
        // limit allows it.
        assert_eq!(rendered(&vm, value), u64::MAX);
    }

    #[test]
    fn the_rendered_size_of_containers_counts_what_they_hold_and_a_separator() {
        let mut vm = equality_vm();
        let key = vm.alloc_object(Object::Str("key".to_owned())).unwrap();
        let word = vm.alloc_object(Object::Str("word".to_owned())).unwrap();
        let Value::Heap(key_handle) = key else {
            unreachable!()
        };
        let map = vm
            .alloc_object(Object::Map(vec![crate::value::MapEntry {
                key: crate::value::MapKey::Str(key_handle),
                value: word,
            }]))
            .unwrap();
        // key 3 + value 4 + 2 separators.
        assert_eq!(rendered(&vm, map), 9);
        let record = vm
            .alloc_object(Object::Record {
                shape: vec![],
                fields: vec![Value::Int(12), word],
            })
            .unwrap();
        assert_eq!(rendered(&vm, record), (2 + 1) + (4 + 1));
        let variant = vm
            .alloc_object(Object::Enum {
                variant: 0,
                payload: vec![word, word],
            })
            .unwrap();
        assert_eq!(rendered(&vm, variant), 2 * (4 + 1));
    }

    #[test]
    fn initialise_runs_a_trivial_module_to_completion() {
        let mut vm = Interpreter::new(sample(), CapabilityTable::new(vec![]));
        let result = vm
            .initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        assert!(result.is_none());
        assert!(
            vm.global(0).is_none(),
            "the sample module declares no globals"
        );
    }

    /// A module whose entry function is `double(n): Int { return n + n }`,
    /// exported at function index 0, mirroring `hollowscript::instance`'s own
    /// `sample_instance` fixture.
    fn double_module() -> VerifiedModule {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("double").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Closure,
            arity: 1,
            register_count: 2,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::abc(Op::AddInt, 1, 0, 0).encode(),
                Instruction::abc(Op::Return, 1, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        verify(m).unwrap()
    }

    #[test]
    fn call_export_runs_a_function_and_returns_its_value() {
        let mut vm = Interpreter::new(double_module(), CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm
            .call_export(
                0,
                &[Value::Int(21)],
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(matches!(result, Some(Value::Int(42))));
    }

    /// A module whose initialiser calls a function that calls itself for
    /// ever, through `CallFn`, with nothing between the call and the return
    /// to consume steps.
    fn endless_recursion_module() -> VerifiedModule {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("down").unwrap();
        m.functions.push(Function {
            name,
            // `CallFn` names a function directly, which the verifier allows
            // only for `FunctionKind::Function`.
            kind: FunctionKind::Function,
            arity: 0,
            register_count: 2,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::abx(Op::CallFn, 0, 0).encode(),
                Instruction::abc(Op::Return, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 2,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::abx(Op::CallFn, 0, 0).encode(),
                Instruction::abc(Op::ReturnNone, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        verify(m).unwrap()
    }

    /// Section 11.5: "recursion must always reach the language's counter
    /// before it can exhaust the implementation's own stack". The frames are
    /// the interpreter's own, so this is a fault a host can catch, in every
    /// build, rather than an abort. Before the frame stack existed this test
    /// aborted the process in an unoptimised build.
    #[test]
    fn runaway_recursion_is_hs1005_and_not_a_stack_overflow() {
        let mut vm = Interpreter::new(endless_recursion_module(), CapabilityTable::new(vec![]));
        let fault = vm
            .initialise(&Limits::default(), &mut Control::none())
            .expect_err("endless recursion must fault");
        assert_eq!(fault.code, FaultCode::CallDepth);
    }

    /// The counter, not the host's stack, is what bounds the depth: raising
    /// `maxCallDepth` well past its default raises how deep a program can
    /// go, and it still faults rather than aborting.
    #[test]
    fn a_raised_call_depth_limit_is_still_reached_as_a_fault() {
        let limits = Limits {
            max_call_depth: 20_000,
            max_steps: u64::MAX,
            ..Limits::default()
        };
        let mut vm = Interpreter::new(endless_recursion_module(), CapabilityTable::new(vec![]));
        let fault = vm
            .initialise(&limits, &mut Control::none())
            .expect_err("endless recursion must fault");
        assert_eq!(fault.code, FaultCode::CallDepth);
    }

    #[test]
    fn int_overflow_faults_hs1007() {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("overflow").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 3,
            returns_value: true,
            captures: vec![],
            constants: vec![Constant::Int(i64::MAX), Constant::Int(1)],
            code: vec![
                Instruction::abx(Op::LoadK, 0, 0).encode(),
                Instruction::abx(Op::LoadK, 1, 1).encode(),
                Instruction::abc(Op::AddInt, 2, 0, 1).encode(),
                Instruction::abc(Op::Return, 2, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let err = vm
            .call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap_err();
        assert_eq!(err.code, FaultCode::IntOverflow);
    }

    #[test]
    fn division_by_zero_faults_hs1008() {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("bad_div").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 3,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::asbx(Op::LoadInt, 0, 1).encode(),
                Instruction::asbx(Op::LoadInt, 1, 0).encode(),
                Instruction::abc(Op::Div, 2, 0, 1).encode(),
                Instruction::abc(Op::Return, 2, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let err = vm
            .call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap_err();
        assert_eq!(err.code, FaultCode::DivisionByZero);
    }

    #[test]
    fn the_step_limit_is_reported_as_hs1001() {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("costly").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::abx(Op::Charge, 0, 1_000).encode(),
                Instruction::abc(Op::ReturnNone, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let limits = Limits {
            max_steps: 999,
            ..Limits::default()
        };
        let err = vm
            .call_export(0, &[], &limits, &mut Control::none())
            .unwrap_err();
        assert_eq!(err.code, FaultCode::StepLimit);
    }

    /// `main.hws`'s entry function builds a two-field record `{x, y}` with
    /// shape 0 and returns field 1 (`y`), exercising `NewRecord`/`GetField`.
    #[test]
    fn records_are_built_and_read_by_field_position() {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let x = m.add_string("x").unwrap();
        let y = m.add_string("y").unwrap();
        m.shapes.push(Shape { fields: vec![x, y] });
        let name = m.add_string("point").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 4,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::asbx(Op::LoadInt, 1, 10).encode(),
                Instruction::asbx(Op::LoadInt, 2, 20).encode(),
                Instruction::abx(Op::NewRecord, 0, 0).encode(),
                Instruction::abc(Op::GetField, 3, 0, 1).encode(),
                Instruction::abc(Op::Return, 3, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm
            .call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap();
        assert!(matches!(result, Some(Value::Int(20))));
    }

    #[test]
    fn an_out_of_range_field_index_faults_hs1099() {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let x = m.add_string("x").unwrap();
        m.shapes.push(Shape { fields: vec![x] });
        let name = m.add_string("bad_field").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 3,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::asbx(Op::LoadInt, 1, 10).encode(),
                Instruction::abx(Op::NewRecord, 1, 0).encode(),
                Instruction::abc(Op::GetField, 2, 1, 5).encode(),
                Instruction::abc(Op::Return, 2, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let err = vm
            .call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap_err();
        assert_eq!(err.code, FaultCode::Internal);
    }

    /// `counter()` returns a closure over a `variable count`, starting at 0,
    /// that adds 1 to it and returns the new value each time it is called:
    /// exercises `NewCell`, `Closure`, `GetUpval`, `SetUpval` and a
    /// dynamically dispatched `Call`.
    #[test]
    fn a_closure_reads_and_mutates_its_captured_cell() {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let inc_name = m.add_string("increment").unwrap();
        m.functions.push(Function {
            name: inc_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 2,
            returns_value: true,
            captures: vec![CaptureSource::Register(0)],
            constants: vec![Constant::Int(1)],
            code: vec![
                Instruction::abc(Op::GetUpval, 0, 0, 0).encode(),
                Instruction::abx(Op::LoadK, 1, 0).encode(),
                Instruction::abc(Op::AddInt, 0, 0, 1).encode(),
                Instruction::abc(Op::SetUpval, 0, 0, 0).encode(),
                Instruction::abc(Op::Return, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let counter_name = m.add_string("counter").unwrap();
        m.functions.push(Function {
            name: counter_name,
            kind: FunctionKind::Function,
            arity: 0,
            register_count: 2,
            returns_value: true,
            captures: vec![],
            constants: vec![Constant::Int(0)],
            code: vec![
                Instruction::abx(Op::LoadK, 0, 0).encode(),
                Instruction::abc(Op::NewCell, 0, 0, 0).encode(),
                Instruction::abx(Op::Closure, 1, 0).encode(),
                Instruction::abc(Op::Return, 1, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let call_twice_name = m.add_string("call_twice").unwrap();
        m.functions.push(Function {
            name: call_twice_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 2,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::abx(Op::CallFn, 1, 1).encode(),
                Instruction::abc(Op::Move, 0, 1, 0).encode(),
                Instruction::abc(Op::Call, 0, 0, 0).encode(),
                Instruction::abc(Op::Move, 0, 1, 0).encode(),
                Instruction::abc(Op::Call, 0, 0, 0).encode(),
                Instruction::abc(Op::Return, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 3 });
        let verified = verify(m).unwrap();
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm
            .call_export(2, &[], &Limits::default(), &mut Control::none())
            .unwrap();
        assert!(matches!(result, Some(Value::Int(2))));
    }

    /// `main.hws` calls a capability that doubles its argument, exercising
    /// `CallCap` end to end against a real `CapabilityTable`.
    /// A capability that reads a `String` argument's real text off the heap
    /// and allocates a new `String` result, proving [`HostFn`] genuinely has
    /// heap access and is not just handed an opaque handle it cannot read.
    #[test]
    fn a_capability_call_reads_and_returns_a_heap_string() {
        use crate::value::Object;
        use hollowscript_bytecode::module::{Capability, CapabilityKind, Constant, Signature};
        use hollowscript_core::transfer::TransferType;

        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let string_type = m.types.add(TransferType::String);
        let cap_name = m.add_string("shout").unwrap();
        m.capabilities.push(Capability {
            name: cap_name,
            kind: CapabilityKind::Function(Signature {
                params: vec![string_type],
                ret: Some(string_type),
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let fn_name = m.add_string("run").unwrap();
        let greeting = m.add_string("hello").unwrap();
        m.functions.push(Function {
            name: fn_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 3,
            returns_value: true,
            captures: vec![],
            constants: vec![Constant::Str(greeting)],
            code: vec![
                Instruction::abx(Op::LoadK, 2, 0).encode(),
                Instruction::abx(Op::CallCap, 1, 0).encode(),
                Instruction::abc(Op::Return, 1, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let shout: Box<dyn crate::capability::HostFn> = Box::new(
            |args: &[Value], call: &mut crate::hostvalues::HostCall<'_>| match args {
                [value] => {
                    let Some(text) = call.str_of(*value) else {
                        return Err(hollowscript_core::fault::Fault::internal("not a string"));
                    };
                    let shouted = format!("{}!", text.to_uppercase());
                    call.string(shouted)
                }
                _ => Err(hollowscript_core::fault::Fault::internal("wrong arity")),
            },
        );
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![shout]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm
            .call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap();
        let Some(Value::Heap(h)) = result else {
            panic!("expected a heap string, got {result:?}");
        };
        assert!(matches!(vm.heap().get(h), Some(Object::Str(s)) if s == "HELLO!"));
    }

    #[test]
    fn a_capability_call_reaches_the_host_function() {
        use hollowscript_bytecode::module::{Capability, CapabilityKind, Signature};
        use hollowscript_core::transfer::TransferType;

        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let int_type = m.types.add(TransferType::Int);
        let cap_name = m.add_string("double").unwrap();
        m.capabilities.push(Capability {
            name: cap_name,
            kind: CapabilityKind::Function(Signature {
                params: vec![int_type],
                ret: Some(int_type),
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let fn_name = m.add_string("run").unwrap();
        m.functions.push(Function {
            name: fn_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 3,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::asbx(Op::LoadInt, 2, 21).encode(),
                Instruction::abx(Op::CallCap, 1, 0).encode(),
                Instruction::abc(Op::Return, 1, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let doubling: Box<dyn crate::capability::HostFn> = Box::new(
            |args: &[Value], _: &mut crate::hostvalues::HostCall<'_>| match args {
                [Value::Int(n)] => Ok(Value::Int(n * 2)),
                _ => Err(hollowscript_core::fault::Fault::internal("wrong arity")),
            },
        );
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![doubling]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm
            .call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap();
        assert!(matches!(result, Some(Value::Int(42))));
    }

    /// A module whose export `run` calls the capability `print` (a `String`
    /// parameter, no result) twice with "hello", which is 6 bytes with the
    /// LF it ends in, around a table whose one function counts its calls.
    fn printing_twice(
        table: impl FnOnce(Box<dyn crate::capability::HostFn>) -> CapabilityTable,
    ) -> (Interpreter, std::rc::Rc<std::cell::Cell<u32>>) {
        use hollowscript_bytecode::module::{Capability, CapabilityKind, Signature};
        use hollowscript_core::transfer::TransferType;

        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let string_type = m.types.add(TransferType::String);
        let cap_name = m.add_string("print").unwrap();
        m.capabilities.push(Capability {
            name: cap_name,
            kind: CapabilityKind::Function(Signature {
                params: vec![string_type],
                ret: None,
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let fn_name = m.add_string("run").unwrap();
        let hello = m.add_string("hello").unwrap();
        m.functions.push(Function {
            name: fn_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 2,
            returns_value: false,
            captures: vec![],
            constants: vec![Constant::Str(hello)],
            code: vec![
                Instruction::abx(Op::LoadK, 1, 0).encode(),
                Instruction::abx(Op::CallCap, 0, 0).encode(),
                Instruction::abx(Op::LoadK, 1, 0).encode(),
                Instruction::abx(Op::CallCap, 0, 0).encode(),
                Instruction::abc(Op::ReturnNone, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = std::rc::Rc::clone(&calls);
        let print: Box<dyn crate::capability::HostFn> = Box::new(
            move |_args: &[Value], _call: &mut crate::hostvalues::HostCall<'_>| {
                counted.set(counted.get() + 1);
                Ok(Value::None)
            },
        );
        let vm = Interpreter::new(verify(m).unwrap(), table(print));
        (vm, calls)
    }

    fn output_limit(bytes: u64) -> Limits {
        Limits {
            max_output_bytes: bytes,
            ..Limits::default()
        }
    }

    /// Section 11.6, for a table whose `print` the interpreter cannot look
    /// inside: the bytes of the text and its LF are charged before the call,
    /// a call that would pass the limit never reaches the host, and the
    /// fault is HS1004 with the limit and the amount used so far.
    #[test]
    fn a_marked_print_is_charged_against_the_output_limit_before_the_host_sees_it() {
        // Two calls of 6 bytes each fit in 12 and are both made.
        let (mut vm, calls) =
            printing_twice(|print| CapabilityTable::new(vec![print]).with_output_charged([0]));
        let limits = output_limit(12);
        vm.initialise(&limits, &mut Control::none()).unwrap();
        vm.call_export(0, &[], &limits, &mut Control::none())
            .unwrap();
        assert_eq!(calls.get(), 2);

        // In 11 the second would make 12: the host is called once, not twice.
        let (mut vm, calls) =
            printing_twice(|print| CapabilityTable::new(vec![print]).with_output_charged([0]));
        let limits = output_limit(11);
        vm.initialise(&limits, &mut Control::none()).unwrap();
        let fault = vm
            .call_export(0, &[], &limits, &mut Control::none())
            .unwrap_err();
        assert_eq!(fault.code, FaultCode::OutputLimit);
        let data = fault.data.expect("a limit fault carries its data");
        assert_eq!((data.limit, data.used), (11, 6));
        assert_eq!(calls.get(), 1, "the call over the limit writes nothing");

        // Not even the first fits in 5.
        let (mut vm, calls) =
            printing_twice(|print| CapabilityTable::new(vec![print]).with_output_charged([0]));
        let limits = output_limit(5);
        vm.initialise(&limits, &mut Control::none()).unwrap();
        let fault = vm
            .call_export(0, &[], &limits, &mut Control::none())
            .unwrap_err();
        assert_eq!(fault.code, FaultCode::OutputLimit);
        assert_eq!(calls.get(), 0);
    }

    /// A table that does not mark its function charges nothing here, since
    /// a host that writes the output itself charges it itself (and would be
    /// charged twice otherwise).
    #[test]
    fn an_unmarked_capability_is_not_charged_against_the_output_limit() {
        let (mut vm, calls) = printing_twice(|print| CapabilityTable::new(vec![print]));
        let limits = output_limit(1);
        vm.initialise(&limits, &mut Control::none()).unwrap();
        vm.call_export(0, &[], &limits, &mut Control::none())
            .unwrap();
        assert_eq!(calls.get(), 2);
    }

    /// The batched path charges at the same point: before the host is told
    /// of the call, so the command is never queued for one over the limit.
    #[test]
    fn a_marked_print_is_charged_on_the_batched_path_before_the_call_is_queued() {
        let (mut vm, _) =
            printing_twice(|print| CapabilityTable::new(vec![print]).with_output_charged([0]));
        vm.use_command_buffer();
        let limits = output_limit(11);
        vm.initialise_batched(&limits, &mut Control::none())
            .unwrap();
        let paused = vm
            .call_export_batched(0, &[], &limits, &mut Control::none())
            .unwrap();
        assert!(matches!(paused, super::Run::Suspended));
        assert_eq!(vm.commands().len(), 1);
        // The host answers the first call, and the second is over the limit.
        let fault = vm
            .resume(
                vec![crate::capability::CommandResult::Ok(Value::None)],
                &limits,
                &mut Control::none(),
            )
            .unwrap_err();
        assert_eq!(fault.code, FaultCode::OutputLimit);
        assert!(vm.commands().is_empty(), "the second call was never queued");
    }

    /// A capability call whose argument register does
    /// not hold a value of the declared parameter type must be refused
    /// before the host function is ever called, not passed through. This
    /// never happens for a program the checker and compiler produced (both
    /// agree on the declared type before a call is emitted), so it is
    /// built the way the review did: bytecode that names a value the
    /// declared `Int` parameter never allows in the argument register a
    /// verified, otherwise ordinary module's `CallCap` reads.
    #[test]
    fn a_capability_argument_of_the_wrong_type_is_refused_before_the_host_is_called() {
        use hollowscript_bytecode::module::{Capability, CapabilityKind, Signature};
        use hollowscript_core::transfer::TransferType;

        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let int_type = m.types.add(TransferType::Int);
        let cap_name = m.add_string("double").unwrap();
        m.capabilities.push(Capability {
            name: cap_name,
            kind: CapabilityKind::Function(Signature {
                params: vec![int_type],
                ret: Some(int_type),
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let fn_name = m.add_string("run").unwrap();
        let text = m.add_string("not an int").unwrap();
        m.functions.push(Function {
            name: fn_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 3,
            returns_value: true,
            captures: vec![],
            constants: vec![Constant::Str(text)],
            code: vec![
                // Loads a String into the argument register a verified,
                // honestly-compiled call to `double(Int)` would only ever
                // put an `Int` into: the verifier checks the register is
                // initialised and of *some* value kind, not that it
                // matches this particular capability's declared parameter
                // type (that is a `TypeRef` into the module's own type
                // table, not part of the bytecode's own value-kind lattice
                // `verify.rs` tracks).
                Instruction::abx(Op::LoadK, 2, 0).encode(),
                Instruction::abx(Op::CallCap, 1, 0).encode(),
                Instruction::abc(Op::Return, 1, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let called = std::rc::Rc::new(std::cell::Cell::new(false));
        let called_from_host = called.clone();
        let doubling: Box<dyn crate::capability::HostFn> = Box::new(
            move |args: &[Value], _: &mut crate::hostvalues::HostCall<'_>| {
                called_from_host.set(true);
                match args {
                    [Value::Int(n)] => Ok(Value::Int(n * 2)),
                    _ => Err(hollowscript_core::fault::Fault::internal("wrong arity")),
                }
            },
        );
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![doubling]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm.call_export(0, &[], &Limits::default(), &mut Control::none());
        assert!(
            matches!(&result, Err(f) if f.code == FaultCode::Internal),
            "expected an internal fault, got {result:?}"
        );
        assert!(
            !called.get(),
            "the host function must never be called with an argument of the wrong type"
        );
    }

    #[test]
    fn a_global_survives_between_calls() {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let global_name = m.add_string("total").unwrap();
        m.globals.push(Global {
            module: 0,
            name: global_name,
            mutable: true,
        });
        let bump_name = m.add_string("bump").unwrap();
        m.functions.push(Function {
            name: bump_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 2,
            returns_value: true,
            captures: vec![],
            constants: vec![Constant::Int(1)],
            code: vec![
                Instruction::abx(Op::GetGlobal, 0, 0).encode(),
                Instruction::abx(Op::LoadK, 1, 0).encode(),
                Instruction::abc(Op::AddInt, 0, 0, 1).encode(),
                Instruction::abx(Op::SetGlobal, 0, 0).encode(),
                Instruction::abc(Op::Return, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 2,
            returns_value: false,
            captures: vec![],
            constants: vec![Constant::Int(0)],
            code: vec![
                Instruction::abx(Op::LoadK, 0, 0).encode(),
                Instruction::abx(Op::SetGlobal, 0, 0).encode(),
                Instruction::abc(Op::ReturnNone, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        let verified = verify(m).unwrap();
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        assert!(matches!(vm.global(0), Some(Value::Int(0))));
        vm.call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap();
        vm.call_export(0, &[], &Limits::default(), &mut Control::none())
            .unwrap();
        assert!(matches!(vm.global(0), Some(Value::Int(2))));
    }

    #[test]
    fn float_text_matches_the_specification_table() {
        let cases: [(f64, &str); 10] = [
            (1.0, "1.0"),
            (100.0, "100.0"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (0.000_01, "0.00001"),
            (0.000_001, "1e-6"),
            (1_000_000_000_000_000.0, "1000000000000000.0"),
            (10_000_000_000_000_000.0, "1e16"),
            (1.5e20, "1.5e20"),
            (-2.5e-7, "-2.5e-7"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_float(value), expected, "for {value}");
        }
        assert_eq!(format_float(0.0), "0.0");
        assert_eq!(format_float(-0.0), "0.0");
    }

    /// A module whose exported function `loopy(n)` is exactly the pattern
    /// `docs/design/performance.md` names as the amplification case: a
    /// `for`-shaped loop (here written by hand as `Lt`/`JumpIfNot`/`Jump`)
    /// that builds a small list every iteration and immediately discards it
    /// by overwriting the register that held it, `n` times, then returns
    /// the iteration count. Register layout: `r0` the argument, `r1` the
    /// counter, `r2` the constant 1, `r3` the loop condition, `r7` the list
    /// each iteration builds and discards, `r8..r11` its three elements.
    fn discarding_loop_module() -> VerifiedModule {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("loopy").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Closure,
            arity: 1,
            register_count: 11,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::asbx(Op::LoadInt, 1, 0).encode(), // r1 = 0
                Instruction::asbx(Op::LoadInt, 2, 1).encode(), // r2 = 1
                // loop_start (index 2): the verifier requires every cycle of
                // control flow to charge at least one step, section 11.3's
                // "each iteration of a for or while loop" made explicit.
                Instruction::abx(Op::Charge, 0, 1).encode(),
                Instruction::abc(Op::Lt, 3, 1, 0).encode(), // r3 = r1 < r0
                Instruction::asbx(Op::JumpIfNot, 3, 6).encode(), // -> end (index 11)
                Instruction::asbx(Op::LoadInt, 8, 1).encode(),
                Instruction::asbx(Op::LoadInt, 9, 2).encode(),
                Instruction::asbx(Op::LoadInt, 10, 3).encode(),
                Instruction::abx(Op::NewList, 7, 3).encode(), // r7 = [r8, r9, r10]
                Instruction::abc(Op::AddInt, 1, 1, 2).encode(), // r1 += 1
                Instruction::asbx(Op::Jump, 0, -9).encode(),  // -> loop_start (index 2)
                // end (index 11):
                Instruction::abc(Op::Return, 1, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        verify(m).unwrap()
    }

    /// The safepoint check in `run_frames` must never change what a run
    /// observes: the same program, run once under the collector's ordinary
    /// threshold policy and once with every safepoint forced to collect
    /// (`Heap::enable_collect_stress_mode`), must return the same value.
    /// `n` is kept well under the 256 KiB default threshold, so the
    /// ordinary run performs no mid-run collection at all; the stress run
    /// performs one at (conceptually) every instruction, which is the
    /// harder case for correctness precisely because it is not the case the
    /// threshold policy would ordinarily choose.
    #[test]
    fn a_forced_collection_at_every_safepoint_does_not_change_the_result() {
        let n = 500;
        let mut ordinary = Interpreter::new(discarding_loop_module(), CapabilityTable::new(vec![]));
        ordinary
            .initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let ordinary_result = ordinary
            .call_export(
                0,
                &[Value::Int(n)],
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(matches!(ordinary_result, Some(Value::Int(v)) if v == n));

        let mut stressed = Interpreter::new(discarding_loop_module(), CapabilityTable::new(vec![]));
        stressed.enable_gc_stress_mode();
        stressed
            .initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let stressed_result = stressed
            .call_export(
                0,
                &[Value::Int(n)],
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(matches!(stressed_result, Some(Value::Int(v)) if v == n));
    }

    /// The point of a mid-run collection: under stress mode, each
    /// iteration's discarded list is freed at the very next safepoint
    /// (`Heap::alloc`'s new object made `due_for_collection` true, and the
    /// next loop-top check acts on it), rather than staying resident until
    /// the whole call returns. After 2,000 iterations the heap holds at
    /// most the one list the last iteration built (or none, if that last
    /// one was itself collected once the call's own frame popped), not
    /// 2,000 of them.
    #[test]
    fn mid_run_collection_frees_each_iterations_discarded_list_as_it_goes() {
        let n = 2_000;
        let mut vm = Interpreter::new(discarding_loop_module(), CapabilityTable::new(vec![]));
        vm.enable_gc_stress_mode();
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm
            .call_export(
                0,
                &[Value::Int(n)],
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(matches!(result, Some(Value::Int(v)) if v == n));
        assert!(
            vm.heap().len() <= 1,
            "a mid-run collection should have freed every discarded list but the last, \
             found {} objects still live",
            vm.heap().len()
        );
    }

    /// Without stress mode, and with `n` too small to cross the collector's
    /// own threshold, nothing mid-run ever collects: every one of `n`
    /// discarded lists is still in the heap when the call returns, exactly
    /// the amplification `docs/design/performance.md` describes. This is
    /// the baseline the two tests above are a contrast to, and it also
    /// guards `discarding_loop_module` itself: if this failed, the two
    /// stress tests above would not be exercising what they claim to.
    #[test]
    fn without_a_collection_every_discarded_list_stays_resident() {
        let n = 500;
        let mut vm = Interpreter::new(discarding_loop_module(), CapabilityTable::new(vec![]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm
            .call_export(
                0,
                &[Value::Int(n)],
                &Limits::default(),
                &mut Control::none(),
            )
            .unwrap();
        assert!(matches!(result, Some(Value::Int(v)) if v == n));
        assert_eq!(i64::try_from(vm.heap().len()).unwrap(), n);
    }

    /// The batched path of `docs/design/capabilities.md`, end to end: a
    /// frame-based host that cannot be re-entered from the middle of a
    /// script drains the queued calls at its own boundary and hands the
    /// results back.
    mod batched {
        use super::{
            CapCall, Control, Function, FunctionKind, Instruction, Interpreter, Limits, Module,
            ModuleEntry, Op, Value, verify,
        };
        use crate::capability::{CapabilityTable, CommandResult};
        use crate::interp::Run;
        use hollowscript_bytecode::module::{Capability, CapabilityKind, Signature};
        use hollowscript_core::transfer::TransferType;

        /// A program whose one export doubles what the capability answers,
        /// so that a wrong result or a lost frame shows up in the value.
        fn twice_what_the_host_says() -> Interpreter {
            let mut m = Module::new();
            let path = m.add_string("main.hws").unwrap();
            let int = m.types.add(TransferType::Int);
            let cap_name = m.add_string("ask").unwrap();
            m.capabilities.push(Capability {
                name: cap_name,
                kind: CapabilityKind::Function(Signature {
                    params: vec![int],
                    ret: Some(int),
                }),
            });
            m.cap_calls.push(CapCall {
                capability: 0,
                function: 0,
            });
            let fn_name = m.add_string("run").unwrap();
            m.functions.push(Function {
                name: fn_name,
                kind: FunctionKind::Closure,
                arity: 1,
                register_count: 4,
                returns_value: true,
                captures: vec![],
                constants: vec![],
                code: vec![
                    // r2 = ask(r0); r3 = r2 + r2; return r3
                    Instruction::abc(Op::Move, 3, 0, 0).encode(),
                    Instruction::abx(Op::CallCap, 2, 0).encode(),
                    Instruction::abc(Op::AddInt, 3, 2, 2).encode(),
                    Instruction::abc(Op::Return, 3, 0, 0).encode(),
                ],
                spans: vec![],
            });
            let init_name = m.add_string("main").unwrap();
            m.functions.push(Function {
                name: init_name,
                kind: FunctionKind::ModuleInit,
                arity: 0,
                register_count: 1,
                returns_value: false,
                captures: vec![],
                constants: vec![],
                code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
                spans: vec![],
            });
            m.modules.push(ModuleEntry { path, init: 1 });
            let mut vm = Interpreter::new(verify(m).unwrap(), CapabilityTable::new(vec![]));
            vm.use_command_buffer();
            vm
        }

        #[test]
        fn a_run_pauses_at_a_capability_call_and_the_host_resumes_it() {
            let mut vm = twice_what_the_host_says();
            let limits = Limits::default();
            assert!(matches!(
                vm.initialise_batched(&limits, &mut Control::none()),
                Ok(Run::Done(None))
            ));
            let paused = vm
                .call_export_batched(0, &[Value::Int(20)], &limits, &mut Control::none())
                .unwrap();
            assert!(matches!(paused, Run::Suspended));
            assert!(vm.is_suspended());

            // The host sees the call it has to perform, with the argument
            // the script passed, already converted.
            let commands = vm.commands();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].function, 0);
            assert!(matches!(commands[0].args.as_slice(), [Value::Int(20)]));

            let finished = vm
                .resume(
                    vec![CommandResult::Ok(Value::Int(21))],
                    &limits,
                    &mut Control::none(),
                )
                .unwrap();
            let Run::Done(Some(Value::Int(n))) = finished else {
                panic!("expected a finished run, got {finished:?}");
            };
            assert_eq!(n, 42);
            assert!(!vm.is_suspended());
        }

        /// The instance goes on working after a pause: a second call pauses
        /// and resumes the same way, with no state left over from the first.
        #[test]
        fn a_second_call_pauses_and_resumes_the_same_way() {
            let mut vm = twice_what_the_host_says();
            let limits = Limits::default();
            vm.initialise_batched(&limits, &mut Control::none())
                .unwrap();
            for answer in [1, 2] {
                vm.call_export_batched(0, &[Value::Int(0)], &limits, &mut Control::none())
                    .unwrap();
                let _ = vm.commands();
                let finished = vm
                    .resume(
                        vec![CommandResult::Ok(Value::Int(answer))],
                        &limits,
                        &mut Control::none(),
                    )
                    .unwrap();
                assert!(matches!(finished, Run::Done(Some(Value::Int(n))) if n == answer * 2));
            }
        }

        /// A host that fails reports it the way a synchronous `HostFn` does.
        #[test]
        fn a_failed_command_faults_the_run() {
            let mut vm = twice_what_the_host_says();
            let limits = Limits::default();
            vm.initialise_batched(&limits, &mut Control::none())
                .unwrap();
            vm.call_export_batched(0, &[Value::Int(0)], &limits, &mut Control::none())
                .unwrap();
            let _ = vm.commands();
            let fault = vm
                .resume(
                    vec![CommandResult::Err(hollowscript_core::fault::Fault::new(
                        hollowscript_core::fault::FaultCode::CapabilityFailed,
                        &[("name", &"ask"), ("message", &"the disk is on fire")],
                    ))],
                    &limits,
                    &mut Control::none(),
                )
                .expect_err("the host failed");
            assert_eq!(fault.code.code().number(), 1012);
        }

        /// A result of the wrong type is the same HS1013 the synchronous
        /// path raises, not a value of the wrong type in a register.
        #[test]
        fn a_result_of_the_wrong_type_is_refused() {
            let mut vm = twice_what_the_host_says();
            let limits = Limits::default();
            vm.initialise_batched(&limits, &mut Control::none())
                .unwrap();
            vm.call_export_batched(0, &[Value::Int(0)], &limits, &mut Control::none())
                .unwrap();
            let _ = vm.commands();
            let fault = vm
                .resume(
                    vec![CommandResult::Ok(Value::Bool(true))],
                    &limits,
                    &mut Control::none(),
                )
                .expect_err("a Bool is not an Int");
            assert_eq!(fault.code.code().number(), 1013);
        }

        /// The wrong number of results faults rather than leaving the run in
        /// a state nothing can describe.
        #[test]
        fn the_wrong_number_of_results_is_reported() {
            let mut vm = twice_what_the_host_says();
            let limits = Limits::default();
            vm.initialise_batched(&limits, &mut Control::none())
                .unwrap();
            vm.call_export_batched(0, &[Value::Int(0)], &limits, &mut Control::none())
                .unwrap();
            let _ = vm.commands();
            let fault = vm
                .resume(Vec::new(), &limits, &mut Control::none())
                .expect_err("no results for one queued call");
            assert_eq!(fault.code.code().number(), 1012);
            assert!(
                fault.message.contains("0 result(s) for 1 queued call(s)"),
                "{}",
                fault.message
            );
        }

        /// Resuming an interpreter that is not paused is a host defect, and
        /// is reported as one.
        #[test]
        fn resuming_a_running_interpreter_is_refused() {
            let mut vm = twice_what_the_host_says();
            let limits = Limits::default();
            let fault = vm
                .resume(Vec::new(), &limits, &mut Control::none())
                .expect_err("nothing is suspended");
            assert_eq!(fault.code.code().number(), 1099);
        }

        /// A synchronous entry point on the batched path is refused rather
        /// than quietly losing the pause.
        #[test]
        fn a_synchronous_entry_point_is_refused_on_the_batched_path() {
            let mut vm = twice_what_the_host_says();
            let fault = vm
                .initialise(&Limits::default(), &mut Control::none())
                .expect_err("the batched path needs the batched entry points");
            assert_eq!(fault.code.code().number(), 1099);
        }
    }
}
