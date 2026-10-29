//! The JavaScript boundary: a hand-written `extern "C"` surface over this
//! module's own linear memory.
//!
//! There is no `wasm-bindgen` and no other dependency, which is what keeps
//! this crate's dependency list the three crates it declares. A host page
//! reaches everything here through the WebAssembly instance's exports and
//! its `memory`, the way `web/hollowscript.js` does.
//!
//! **How a call works.** The host asks for a buffer with
//! [`hs_wasm_alloc`], writes bytes into the instance's memory at the offset
//! it gets back, and passes that offset and its length. Everything this
//! side hands back is a buffer too, owned by the caller and released with
//! [`hs_wasm_free`], so nothing here keeps a pointer the host might still
//! be reading.
//!
//! **Values cross as text**, for the same reason they do at the C ABI: a
//! `Value` of a container kind is a handle into one instance's heap, which
//! a caller outside the VM cannot hold. A result comes back in the
//! rendering `corpus/FORMAT.md` fixes (`Int(3)`, `String("a")`), and
//! arguments go in as a JSON array in the encoding this crate's own root
//! module documentation defines (see "the shared value <-> JSON encoding"),
//! which the instance converts: every transferable type of chapter 12, not
//! only a scalar.
//!
//! **Instances live in a registry** rather than being handed out as
//! pointers, so a host that passes back a number this module never gave it
//! gets a refusal instead of reaching memory. WebAssembly is
//! single-threaded, so the registry is a thread local and needs no lock.
//!
//! **A panic does not cross the boundary.** A panic unwinding out of a
//! WebAssembly export is not something a host page can be handed, so the
//! two entry points that run script code, [`hs_wasm_load`] and every call
//! that reaches an instance, catch one and answer [`Status::Panicked`].
//! An instance that panicked is dropped rather than put back, because a
//! panic part way through a call could have left it half changed; every
//! later call on that handle is then refused. Nothing a program can do
//! should reach this, which is what the pipeline fuzz harness exists to
//! keep true.
//!
//! Every `unsafe` block does one thing, behind a `SAFETY` comment, and the
//! precondition is checked in safe code immediately before it.

use crate::WasmModule;
use hollowscript_core::fault::{Fault, FaultCode};
use hollowscript_core::{Control, Limits, YieldDecision};
use hollowscript_vm::{CapabilityTable, CommandResult, HostCall, HostFn, Value};
use std::cell::{Cell, RefCell};
use std::fmt::Write as _;

// The functions a JavaScript host imports into this module (module `env`),
// so that a capability declared `uses` in the program, and cancellation,
// can reach back out of WebAssembly. Real imports only on the `wasm32`
// target this crate is actually built for as a browser VM; every native
// build (this crate's own tests, `cargo doc`, a host tool that links it
// for some other reason) instead uses `host_stub`, a thread-local a test
// installs, so the marshalling code below is exercised without a
// WebAssembly runtime to host it. (A plain comment, not a doc comment:
// rustdoc does not generate documentation for an `extern` block at all, and
// warns on a doc comment attached to one.)
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    /// Calls capability `index` (positional into the loaded module's own
    /// `cap_calls` table, exactly as [`crate::capability_names`] lists
    /// them) with `args_json`, a JSON array of scalars the same shape
    /// [`hs_wasm_call`]'s own arguments already are. Answers `0` for
    /// success or `1` for failure; either way the host holds the pending
    /// result (the JSON success value, or the failure message) until
    /// [`hs_capability_result_len`] and [`hs_capability_take_result`] pull
    /// it, the same two-step [`hs_wasm_text_len`]/[`hs_wasm_take_text`]
    /// already use, in the other direction.
    fn hs_capability_call(index: u32, args_ptr: *const u8, args_len: usize) -> i32;
    /// The length, in bytes, of the pending result [`hs_capability_call`]
    /// just produced.
    fn hs_capability_result_len() -> usize;
    /// Copies the pending result into `dest_ptr`, which must have room for
    /// exactly [`hs_capability_result_len`]'s last answer; consumes it.
    fn hs_capability_take_result(dest_ptr: *mut u8);
    /// Polled at every safepoint (section 11.7) of every run: non-zero
    /// stops it with the fault HS1006. A host typically backs this with an
    /// `Int32Array` over a `SharedArrayBuffer`, read with `Atomics.load`,
    /// set from another thread with `Atomics.store`, which is the one way
    /// a page can cancel a call genuinely concurrently while this module
    /// (in a Worker) is synchronously running it; `web/hollowscript.js`
    /// documents the pattern.
    fn hs_wasm_poll_cancel() -> i32;
}

/// The native stand-in for the `env` imports above, so this crate's tests
/// exercise the same call/marshal/take-result code paths on the host that a
/// WebAssembly build reaches through real imports. A test installs a
/// handler with [`host_stub::set_capability`] and a cancellation flag with
/// [`host_stub::set_cancelled`]; nothing here is reachable from a `wasm32`
/// build, which uses the real imports instead.
#[cfg(not(target_arch = "wasm32"))]
mod host_stub {
    use std::cell::{Cell, RefCell};

    type CapabilityFn = Box<dyn FnMut(u32, &[u8]) -> (bool, Vec<u8>)>;

    thread_local! {
        static CAPABILITY: RefCell<Option<CapabilityFn>> = const { RefCell::new(None) };
        static PENDING: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static CANCELLED: Cell<bool> = const { Cell::new(false) };
    }

    /// Installs (or, with `None`, removes) the handler [`super::call_capability`]
    /// calls in place of the `env.hs_capability_call` import.
    #[cfg(test)]
    pub(super) fn set_capability(handler: Option<CapabilityFn>) {
        CAPABILITY.with_borrow_mut(|slot| *slot = handler);
    }

    /// Sets or clears the flag [`super::poll_cancel`] reads in place of the
    /// `env.hs_wasm_poll_cancel` import.
    #[cfg(test)]
    pub(super) fn set_cancelled(cancelled: bool) {
        CANCELLED.with(|flag| flag.set(cancelled));
    }

    pub(super) fn capability_call(index: u32, args_json: &[u8]) -> bool {
        let (ok, text) = CAPABILITY.with_borrow_mut(|slot| match slot.as_mut() {
            Some(handler) => handler(index, args_json),
            None => (false, b"no capability handler is installed".to_vec()),
        });
        PENDING.with_borrow_mut(|pending| *pending = text);
        ok
    }

    pub(super) fn result_len() -> usize {
        PENDING.with_borrow(Vec::len)
    }

    pub(super) fn take_result(buffer: &mut [u8]) {
        PENDING.with_borrow(|pending| buffer.copy_from_slice(pending));
    }

    pub(super) fn poll_cancel() -> bool {
        CANCELLED.with(Cell::get)
    }
}

/// Calls capability `index` through the host, marshalling `args_json` out
/// and the answer back exactly as the `env` imports above document, and
/// answers the pending result: `Ok` with the JSON success value, `Err` with
/// the failure message.
fn call_capability(index: u32, args_json: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: `args_json` is a valid slice for the length of this call, so
    // `args_json.as_ptr()`/`args_json.len()` describe exactly the readable
    // bytes the import's own contract requires; nothing here keeps the
    // pointer past this call.
    let ok = unsafe { hs_capability_call(index, args_json.as_ptr(), args_json.len()) == 0 };
    #[cfg(not(target_arch = "wasm32"))]
    let ok = host_stub::capability_call(index, args_json);

    #[cfg(target_arch = "wasm32")]
    // SAFETY: no precondition beyond being called on this module's own
    // thread, which a WebAssembly instance always is.
    let len = unsafe { hs_capability_result_len() };
    #[cfg(not(target_arch = "wasm32"))]
    let len = host_stub::result_len();

    let mut buffer = vec![0u8; len];
    #[cfg(target_arch = "wasm32")]
    // SAFETY: `buffer` is exactly `len` bytes, matching what
    // `hs_capability_result_len` just answered, which is exactly what
    // `hs_capability_take_result`'s own contract requires it to write.
    unsafe {
        hs_capability_take_result(buffer.as_mut_ptr());
    }
    #[cfg(not(target_arch = "wasm32"))]
    host_stub::take_result(&mut buffer);

    if ok { Ok(buffer) } else { Err(buffer) }
}

/// Polls for cancellation through the host, as the `env.hs_wasm_poll_cancel`
/// import documents.
fn poll_cancel() -> bool {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: no precondition.
    let cancelled = unsafe { hs_wasm_poll_cancel() != 0 };
    #[cfg(not(target_arch = "wasm32"))]
    let cancelled = host_stub::poll_cancel();
    cancelled
}

/// The [`hollowscript_core::Yield`] every run of every instance uses: at
/// each safepoint, asks the host whether to cancel. Section 11.7's rule
/// (token first, then this) has no token here, since a WebAssembly instance
/// is single-threaded and cannot itself hold one another thread could set;
/// the host's own thread, if it wants cancellation, backs this callback
/// with a flag another thread of its own sets, which is what makes
/// cancellation from another thread possible at all for a page that runs
/// this module in a Worker.
struct PollCancel;

impl hollowscript_core::Yield for PollCancel {
    fn on_yield(&mut self) -> YieldDecision {
        if poll_cancel() {
            YieldDecision::Cancel
        } else {
            YieldDecision::Continue
        }
    }
}

/// Runs `body` with a [`crate::JsonBudget`] of `limit` bytes whose
/// cancellation poll is the host's own (`env.hs_wasm_poll_cancel`), so that
/// every rendering of a value for the host is bounded and can be stopped the
/// way a run can.
fn with_json_budget<R>(limit: u64, body: impl FnOnce(&mut crate::JsonBudget<'_>) -> R) -> R {
    let mut poll = poll_cancel;
    let mut budget = crate::JsonBudget::new(limit, &mut poll);
    body(&mut budget)
}

/// The least a rendering of a value for the host is allowed to produce,
/// whatever the output limit is: 64 KiB. The output limit says how much
/// `print` may write, and a host may set it very low (a few bytes, to stop a
/// program printing at all); the JSON for a `print` argument is also longer
/// than the text it carries (it is quoted), and an argument or a result a
/// host legitimately exchanges should not fault because of that. The floor
/// is small enough to leave the bound on a shared-structure value as tight
/// as it needs to be.
const MIN_RENDERING_BYTES: u64 = 64 * 1024;

/// How many bytes one rendering of a value for the host (a result, the
/// arguments of one capability call, or the commands one drain answers) may
/// produce: the run's output limit, but not less than
/// [`MIN_RENDERING_BYTES`]. See [`crate::JsonBudget`].
fn output_limit() -> u64 {
    LIMITS.with_borrow(|limits| limits.max_output_bytes.max(MIN_RENDERING_BYTES))
}

/// Renders a capability call's arguments as JSON, the same shape
/// [`hs_wasm_call`]'s own arguments already are, in the encoding
/// `crate`'s own root module documentation defines: every transferable
/// type of chapter 12, not only a scalar, and an `Int` above 2^53 - 1 as the
/// tagged `{"bigint": "<decimal>"}` that results use, so that no `Int` is
/// handed to the host as a different one.
///
/// The text of all the arguments together is bounded by the output limit,
/// because an argument can share its sub-values and so be written out far
/// larger than the memory it occupies. The charge for a capability call's
/// arguments (section 11.3) counts each shared object once, so it does not
/// bound this.
///
/// # Errors
///
/// HS1004 when the arguments would be written as more than the output limit
/// allows, and HS1006 when the host asks the run to stop meanwhile.
fn render_capability_arguments(
    call: &HostCall<'_>,
    args: &[Value],
    limit: u64,
) -> Result<Vec<u8>, Fault> {
    with_json_budget(limit, |budget| {
        let mut out = String::from("[");
        for (i, &value) in args.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            // Each argument is written to a string of its own, so the
            // budget counts it once, whole, and the commas are not charged.
            let mut text = String::new();
            crate::value_to_json(call, value, &mut text, budget)?;
            out.push_str(&text);
        }
        out.push(']');
        Ok(out.into_bytes())
    })
}

/// Builds a [`CapabilityTable`] whose every entry, positionally, calls back
/// into the host through [`call_capability`]: one per `(capability,
/// function)` pair the compiled bytecode calls, named as
/// [`crate::capability_names`] lists them (`"print"`, or `"clock.now"` for
/// a member of a namespace capability; a namespace needs no different
/// handling here, since binding is entirely positional by index, and the
/// qualified name exists only for a fault's message and for the host to
/// know which JS function an index means), for the message of a fault this
/// binding itself reports. The VM checks each answer against the
/// capability's declared type after this returns (section 9.6), faulting
/// HS1013 itself if it does not fit, so this only has to build a value of
/// roughly the right shape from whatever JSON the host sent back. `types`
/// (from [`crate::host_types`]) is what lets a capability answer
/// `Ok`/`Err` (section 8.2) or a record: it is the module's own already
/// compiled record shapes and enum variants, carried alongside the
/// functions so [`HostFn::call`] can look a name up by it. `print_calls` (from
/// [`crate::print_calls`]) are the positions of every capability named `print`, whose
/// output the interpreter charges against `maxOutputBytes` (section 11.6)
/// before the host's function is called, since this build cannot look inside
/// that function to do it there.
fn capability_table(
    names: &[String],
    types: hollowscript_vm::HostTypes,
    result_types: &[Option<hollowscript_core::transfer::TypeRef>],
    module_types: &hollowscript_core::transfer::TypeTable,
    print_calls: Vec<u32>,
) -> CapabilityTable {
    let mut functions: Vec<Box<dyn HostFn>> = Vec::with_capacity(names.len());
    for (raw_index, name) in names.iter().enumerate() {
        // `names` came from a `u32`-indexed table (`Module::cap_calls`), so
        // it always fits back into one; the conversion here narrows a
        // `usize` that is already known to be in range rather than
        // truncating one that might not be.
        let index = u32::try_from(raw_index).unwrap_or(u32::MAX);
        let name = name.clone();
        let declared_result = result_types.get(raw_index).copied().flatten();
        let module_types = module_types.clone();
        functions.push(Box::new(
            move |args: &[Value], call: &mut HostCall<'_>| -> Result<Value, Fault> {
                let args_json = render_capability_arguments(call, args, output_limit())?;
                let failed = |message: &str| {
                    Fault::new(
                        FaultCode::CapabilityFailed,
                        &[("name", &name), ("message", &message)],
                    )
                };
                match call_capability(index, &args_json) {
                    Err(message) => Err(failed(&String::from_utf8_lossy(&message))),
                    Ok(text) => {
                        let Ok(text) = String::from_utf8(text) else {
                            return Err(failed("the result is not UTF-8"));
                        };
                        let Ok(parsed) = hollowscript_vm::json::parse(text.trim()) else {
                            return Err(failed("the result is not valid JSON"));
                        };
                        // `crate::json_to_value` understands every
                        // transferable type of chapter 12, including the
                        // `{"Ok": ...}`/`{"Err": ...}` sugar for a
                        // capability declared `Result<T, E>` (section 8.2);
                        // its own `Fault` is rendered to text once here, so
                        // a host's own capability-failure message is not
                        // doubled with a second "Internal error: " prefix.
                        // `declared_result`, when this capability answers
                        // anything, directs a bare JSON number to `Int` or
                        // `Float` before that, which the acceptance rule
                        // cannot do after the fact (see
                        // `crate::coerce_json_numbers`'s own documentation).
                        let parsed = match declared_result {
                            Some(ty) => crate::coerce_json_numbers(parsed, ty, &module_types),
                            None => parsed,
                        };
                        crate::json_to_value(call, parsed)
                            .map_err(|e| failed(&e.to_diagnostic().message))
                    }
                }
            },
        ));
    }
    // Every capability named `print` is charged against `maxOutputBytes` by
    // the interpreter, before the host's own function is called (section
    // 11.6): the host binds it by name and this build cannot look inside it,
    // and a call over the limit must write nothing.
    CapabilityTable::with_types(functions, types).with_output_charged(print_calls)
}

/// What a call answered. A host reads the number and, for anything but
/// [`Status::Ok`], the message from [`hs_wasm_take_text`].
#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The call succeeded.
    Ok = 0,
    /// The handle names no live instance, or an argument was not usable.
    Refused = 1,
    /// The program faulted; the message says how.
    Faulted = 2,
    /// The implementation panicked. A defect of the library, never of the
    /// program. The instance the call was acting on is dropped before this
    /// is answered, so every later call on that handle is refused rather
    /// than reaching state a panic left half-changed.
    Panicked = 3,
    /// The run paused at a capability call, on the batched path
    /// ([`hs_wasm_load_batched`], [`hs_wasm_call_batched`],
    /// [`hs_wasm_resume_json`]). [`hs_wasm_commands_json`] answers what it
    /// is waiting for.
    Suspended = 4,
    /// The handle names a real instance, but a call on it is already in
    /// progress: a synchronous capability handler (a JS `print`
    /// implementation, say) called back into the same instance it is
    /// itself being called from. Distinct from [`Status::Refused`], which
    /// also covers a handle that never named an instance, or has been
    /// disposed, so a host can tell "try again once the outer call
    /// returns" apart from "this handle is gone".
    Busy = 5,
}

/// One position of [`INSTANCES`]. A plain `Option<WasmModule>` cannot tell
/// "no instance was ever here, or it was disposed" apart from "a call on
/// this instance is in progress right now": [`with_instance`] needs that
/// distinction to answer [`Status::Busy`] for the second and, if
/// [`hs_wasm_dispose`] runs during a re-entrant call and empties the slot,
/// to honour that instead of putting the instance back once the call that
/// was in progress returns (see `with_instance`'s own documentation).
#[allow(
    clippy::large_enum_variant,
    reason = "boxing WasmModule would touch every match on Occupied across this file; \
              Empty and Busy are the two states most instances spend nearly all their \
              time in, and INSTANCES holds a modest number of slots"
)]
enum Slot {
    /// No instance here, or it was disposed.
    Empty,
    /// A call on this instance is in progress.
    Busy,
    /// A live instance, not currently in a call.
    Occupied(WasmModule),
}

/// A position of [`INSTANCES`] and how many instances have lived in it.
///
/// A disposed instance's position is reused by the next one loaded, which is
/// what keeps a page that loads and disposes in a loop from growing the table
/// for ever (it grew by about 1.5 KiB for each load, 144 MiB in 100,000
/// cycles). Reuse must not bring a stale handle back to life, so a handle
/// carries the position's generation as well as the position, and the
/// generation moves on every time the instance in a position is disposed: a
/// handle from before is refused, as a handle to a disposed instance always
/// was. A position whose generation could not move on (it has been used
/// [`MAX_GENERATION`] times) is retired, left empty and never reused, rather
/// than wrapped, so no handle ever names a second instance.
struct Entry {
    /// 1 for a position that has held no instance yet; never 0, so that a
    /// handle is never 0, which means "none".
    generation: u16,
    slot: Slot,
}

/// The most positions the table holds, and so the most live instances: a
/// handle keeps the position, plus one, in its low 16 bits.
const MAX_INSTANCES: usize = 0xFFFF;

/// The last generation a position is used in; see [`Entry`].
const MAX_GENERATION: u16 = u16::MAX;

/// The table of instances, with the positions free for reuse. A pair rather
/// than two thread locals so that the two always change together.
struct Instances {
    entries: Vec<Entry>,
    /// Positions that hold no instance and may be reused, most recently freed
    /// last.
    free: Vec<usize>,
}

impl Instances {
    const fn new() -> Instances {
        Instances {
            entries: Vec::new(),
            free: Vec::new(),
        }
    }

    /// The handle for position `index` in `generation`.
    fn handle(index: usize, generation: u16) -> u32 {
        // The position fits 16 bits (the table never has more than
        // `MAX_INSTANCES`), and a position is stored plus one.
        let position = u32::try_from(index + 1).unwrap_or(0);
        (u32::from(generation) << 16) | position
    }

    /// The position and generation a handle names, if it could name one.
    fn split(handle: u32) -> Option<(usize, u16)> {
        let position = usize::try_from(handle & 0xFFFF).ok()?.checked_sub(1)?;
        let generation = u16::try_from(handle >> 16).ok()?;
        (generation != 0).then_some((position, generation))
    }

    /// The entry a handle names, only if it names it in its own generation: a
    /// handle to a position that has been disposed and reused is refused.
    fn entry_mut(&mut self, handle: u32) -> Option<&mut Entry> {
        let (position, generation) = Instances::split(handle)?;
        self.entries
            .get_mut(position)
            .filter(|entry| entry.generation == generation)
    }

    /// Stores `module` in a free position, or a new one, and answers its
    /// handle; `None` when every position is in use.
    fn install(&mut self, module: WasmModule) -> Option<u32> {
        if let Some(index) = self.free.pop() {
            let entry = self.entries.get_mut(index)?;
            entry.slot = Slot::Occupied(module);
            return Some(Instances::handle(index, entry.generation));
        }
        if self.entries.len() >= MAX_INSTANCES {
            return None;
        }
        self.entries.push(Entry {
            generation: 1,
            slot: Slot::Occupied(module),
        });
        Some(Instances::handle(self.entries.len() - 1, 1))
    }

    /// Empties the position `handle` names, if it names a live one, moving
    /// its generation on so that the handle is never good again; the position
    /// is free for reuse unless its generations are used up.
    fn release(&mut self, handle: u32) {
        let Some((position, _)) = Instances::split(handle) else {
            return;
        };
        let Some(entry) = self.entry_mut(handle) else {
            return;
        };
        if matches!(entry.slot, Slot::Empty) {
            return;
        }
        entry.slot = Slot::Empty;
        if entry.generation < MAX_GENERATION {
            entry.generation += 1;
            self.free.push(position);
        }
    }
}

thread_local! {
    /// Every live instance, by handle. A handle is a position (plus one) and
    /// that position's generation, so 0 is never a live instance and can
    /// mean "none", and a handle to a disposed instance is refused even once
    /// its position has been reused (see [`Entry`]).
    static INSTANCES: RefCell<Instances> = const { RefCell::new(Instances::new()) };
    /// The text the last call produced: a result, or the reason it failed.
    /// Held here rather than returned, because a WebAssembly function
    /// answers one number.
    static TEXT: RefCell<String> = const { RefCell::new(String::new()) };
    /// The limits every run of every instance uses, which a host may set
    /// once with [`hs_wasm_set_limits`].
    static LIMITS: RefCell<Limits> = RefCell::new(Limits::default());
    /// How a value a call answers is written to the text channel, which a
    /// host may set with [`hs_wasm_set_result_format`].
    static RESULT_FORMAT: Cell<ResultFormat> = const { Cell::new(ResultFormat::Text) };
    /// The structured form of the fault the last call ended with, as JSON,
    /// for [`hs_wasm_fault_json`] to hand over. `None` when that call did
    /// not end in a fault of the program.
    static FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// How a value a call answers is written to the text channel.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ResultFormat {
    /// The rendering `corpus/FORMAT.md` fixes (`Int(3)`, `String("a")`).
    Text,
    /// JSON, in the encoding of this crate's root module documentation, with
    /// an `Int` above 2^53 - 1 written exactly as `{"bigint": "<decimal>"}`
    /// ([`WasmModule::render_json`]).
    Json,
}

/// `value` in the format a host asked for with [`hs_wasm_set_result_format`].
///
/// The text format is bounded by the renderer itself (16 MiB). JSON is
/// bounded by the output limit, with the host's cancellation poll asked as it
/// is written: a value that shares its sub-values is far larger written out
/// than it is held, and neither the memory limit nor a safepoint reaches the
/// writing, which happens after the run has finished.
///
/// # Errors
///
/// For JSON, HS1004 when the text would be longer than the output limit, and
/// HS1006 when the host asks to stop meanwhile.
fn render_result(module: &mut WasmModule, value: Value) -> Result<String, Fault> {
    match RESULT_FORMAT.get() {
        ResultFormat::Text => Ok(module.render(value)),
        ResultFormat::Json => {
            with_json_budget(output_limit(), |budget| module.render_json(value, budget))
        }
    }
}

/// Chooses how every value that a later call answers is written to the text
/// channel: `0` for the rendering `corpus/FORMAT.md` fixes (the default, what
/// every export answered before this one existed), `1` for JSON. This covers
/// the result of [`hs_wasm_call`], [`hs_wasm_call_batched`] and
/// [`hs_wasm_resume_json`], the value of [`hs_wasm_read`] and the load result
/// [`hs_wasm_load`] and [`hs_wasm_load_batched`] answer; a message (the text
/// of a fault, the reason a call was refused) is never affected. Any other
/// value is refused and changes nothing.
///
/// JSON is the value encoding this crate's root module documents (a list is
/// `{"list": [...]}`, a record `{"record": {...}}`, and so on) with one
/// change that only matters for an `Int` above 2^53 - 1 in magnitude: JSON
/// has one number type and a host that reads it as a double cannot tell
/// such `Int`s apart, so each is written as `{"bigint": "<decimal digits>"}`
/// instead, and an `Int` that is safe stays a plain number. A function that
/// returns nothing answers the empty string in either format, and `none`
/// answers `null`.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_set_result_format(format: u32) -> i32 {
    match format {
        0 => RESULT_FORMAT.set(ResultFormat::Text),
        1 => RESULT_FORMAT.set(ResultFormat::Json),
        _ => {
            set_text("the result format must be 0 (text) or 1 (JSON)".to_owned());
            return Status::Refused as i32;
        }
    }
    Status::Ok as i32
}

/// Writes the message of `fault` to the text channel, as every export that
/// can fault always has, and keeps its structured form for
/// [`hs_wasm_fault_json`].
fn report_fault(fault: &Fault) {
    let diagnostic = fault.to_diagnostic();
    FAULT.with_borrow_mut(|held| *held = Some(fault_json(fault)));
    set_text(diagnostic.message);
}

/// Forgets the fault of the previous call, so that [`hs_wasm_fault_json`]
/// answers nothing after a call that did not fault.
fn clear_fault() {
    FAULT.with_borrow_mut(|held| *held = None);
}

/// Hands over the structured form of the fault that the last call ended
/// with, as JSON on the text channel, and forgets it; the text channel is
/// left empty when that call did not end in a fault of the program (it
/// succeeded, was refused, was busy or panicked). Read it right after the
/// call that answered [`Status::Faulted`], or after a load that answered 0,
/// before the next call on any instance: every call that can fault forgets
/// the previous fault when it starts.
///
/// The object has the fields of section 17.1's diagnostic record that a
/// fault has: `severity` (always `"error"`), `code` (`"HS1008"`), `message`,
/// `class` (section 8.1's `"resource"`, `"domain"`, `"host"` or
/// `"internal"`), `file`, `line`, `column` (1-based; the column counts code
/// points), `spanStartByte` and `spanEndByte` (the half-open span in bytes of
/// the file's UTF-8 text, as the specification defines it), `frames` (the
/// innermost call frames, at most 32, each `function`, `file`, `line`,
/// `column`, `spanStartByte` and `spanEndByte`) and `data` (for the limit
/// faults HS1001 to HS1005 an object with `option`, `limit` and `used`, and
/// `null` for every other fault). `file`, `line`, `column` and the two spans
/// are `null` when the fault has no position, which is the case for bytecode
/// built with its source positions stripped. A number above 2^53 - 1 in
/// `data` is written as `{"bigint": "<decimal>"}`, as in
/// [`hs_wasm_set_result_format`]'s JSON.
///
/// The VM holds no source text, so it cannot answer a UTF-16 span; `line` and
/// `column` are always available when there is a position, and a host that
/// keeps the source can convert `spanStartByte` itself.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_fault_json() -> i32 {
    let fault = FAULT.with_borrow_mut(Option::take);
    set_text(fault.unwrap_or_default());
    Status::Ok as i32
}

/// The structured form of `fault`, for [`hs_wasm_fault_json`].
fn fault_json(fault: &Fault) -> String {
    let diagnostic = fault.to_diagnostic();
    let mut out = String::from(r#"{"severity":"error","code":""#);
    out.push_str(&diagnostic.code.to_text());
    out.push_str(r#"","message":""#);
    crate::escape_json_string(&diagnostic.message, &mut out);
    out.push_str(r#"","class":""#);
    out.push_str(match fault.class() {
        hollowscript_core::fault::FaultClass::Resource => "resource",
        hollowscript_core::fault::FaultClass::Domain => "domain",
        hollowscript_core::fault::FaultClass::Host => "host",
        hollowscript_core::fault::FaultClass::Internal => "internal",
    });
    out.push('"');
    push_fault_location(&mut out, diagnostic.location.as_ref());
    out.push_str(r#","frames":["#);
    for (i, frame) in diagnostic.frames.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"function":""#);
        crate::escape_json_string(&frame.function, &mut out);
        out.push('"');
        push_fault_location(&mut out, Some(&frame.location));
        out.push('}');
    }
    out.push_str(r#"],"data":"#);
    match diagnostic.data {
        Some(data) => {
            let _ = write!(out, r#"{{"option":"{}","limit":"#, data.option.name());
            crate::write_int(&mut out, i64::try_from(data.limit).unwrap_or(i64::MAX));
            out.push_str(r#","used":"#);
            crate::write_int(&mut out, i64::try_from(data.used).unwrap_or(i64::MAX));
            out.push('}');
        }
        None => out.push_str("null"),
    }
    out.push('}');
    out
}

/// Appends `,"file":...,"line":...,"column":...,"spanStartByte":...,"spanEndByte":...`
/// for `location`, or the same keys with `null` values when there is none.
fn push_fault_location(out: &mut String, location: Option<&hollowscript_core::Location>) {
    let Some(location) = location else {
        out.push_str(
            r#","file":null,"line":null,"column":null,"spanStartByte":null,"spanEndByte":null"#,
        );
        return;
    };
    out.push_str(r#","file":""#);
    crate::escape_json_string(&location.file, out);
    let _ = write!(
        out,
        r#"","line":{},"column":{},"spanStartByte":{},"spanEndByte":{}"#,
        location.line, location.column, location.span.start, location.span.end
    );
}

/// The library's own version, written into `TEXT` for the host to read.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_version() -> i32 {
    set_text(env!("CARGO_PKG_VERSION").to_owned());
    Status::Ok as i32
}

/// Reserves `len` bytes and answers the offset of the first one. The host
/// writes into the instance's memory there. A length of 0 answers 0.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    // Zeroed, because the host is about to overwrite it and a buffer that
    // starts as whatever was in memory is one a host could read by mistake.
    release(vec![0u8; len].into_boxed_slice())
}

/// Releases a buffer [`hs_wasm_alloc`] or [`hs_wasm_take_text`] handed out.
/// `len` must be the length that call used.
///
/// # Safety
///
/// `pointer` must be one this module handed out with that same `len`, and
/// must not have been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_free(pointer: *mut u8, len: usize) {
    if pointer.is_null() || len == 0 {
        return;
    }
    // SAFETY: the caller states these are the pointer and length of one
    // buffer this module allocated and has not yet freed, which is exactly
    // what `Box<[u8]>` was made from in `hs_wasm_alloc`.
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(pointer, len)) });
}

/// The length of the text the last call produced, so the host can reserve
/// for it before taking it.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_text_len() -> usize {
    TEXT.with_borrow(String::len)
}

/// Moves that text into a fresh buffer and answers its offset. The buffer is
/// the caller's, to release with [`hs_wasm_free`] and the length
/// [`hs_wasm_text_len`] gave before this call.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_take_text() -> *mut u8 {
    let text = TEXT.with_borrow_mut(std::mem::take);
    if text.is_empty() {
        return std::ptr::null_mut();
    }
    release(text.into_bytes().into_boxed_slice())
}

/// Hands a buffer to the host: answers the offset of its first byte and
/// gives up ownership, so nothing on this side frees it. The host releases
/// it with [`hs_wasm_free`], which rebuilds the same `Box` from the same
/// pointer and length.
pub(crate) fn release(boxed: Box<[u8]>) -> *mut u8 {
    // `into_raw` gives up ownership without `mem::forget`, which is the
    // same thing said in the way that cannot be mistaken for a leak.
    Box::into_raw(boxed).cast::<u8>()
}

/// Sets the limits of chapter 11 for every run that follows. A value of 0
/// leaves that limit as it was, so a host can set one without stating the
/// rest. `output` bounds what the standard `print` writes: the module charges
/// each call to `print`, before the host's function is called, so a host
/// cannot print past it.
///
/// Every limit must be from 1 to 2^63 - 1 (`Limits::validate`, which a limit
/// of the language is held to everywhere). A value outside that range, which is
/// what a negative number becomes when a host passes it as an unsigned 64-bit
/// integer, refuses the whole call with the reason in the text and
/// [`Status::Refused`], and changes none of the limits: a limit near 2^64 never
/// stops anything and, added to a counter, overflows it.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_set_limits(steps: u64, memory: u64, output: u64, depth: u64) -> i32 {
    let mut candidate = LIMITS.with_borrow(Clone::clone);
    for (value, option) in [
        (steps, hollowscript_core::LimitOption::MaxSteps),
        (memory, hollowscript_core::LimitOption::MaxMemoryBytes),
        (output, hollowscript_core::LimitOption::MaxOutputBytes),
        (depth, hollowscript_core::LimitOption::MaxCallDepth),
    ] {
        if value > 0 {
            candidate.set(option, value);
        }
    }
    if let Err(error) = candidate.validate() {
        set_text(error.to_string());
        return Status::Refused as i32;
    }
    LIMITS.with_borrow_mut(|limits| *limits = candidate);
    Status::Ok as i32
}

/// `names` as a JSON array of strings. The verifier admits only identifiers
/// as capability names, so escaping changes nothing for a module that verified;
/// it is here because a name written into JSON unescaped, with a quote in it,
/// shifts every handler a host binds after it, and one with a backslash or a
/// line feed breaks the host's `JSON.parse`.
fn names_json(names: &[String]) -> String {
    let mut json = String::from("[");
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push('"');
        crate::escape_json_string(name, &mut json);
        json.push('"');
    }
    json.push(']');
    json
}

/// Answers the capability names the bytecode at `pointer` calls, in the
/// order [`hs_wasm_load`] binds them in, as a JSON array of strings such as
/// `["print","clock.now"]`: `crate::capability_names` rendered to text. A
/// host reads this before loading, so it knows which JS function to answer
/// each `env.hs_capability_call(index, ...)` with; loading itself never
/// needs a name, only the count, since binding is positional.
///
/// # Safety
///
/// As [`hs_wasm_load`], for `pointer` and `len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_capability_names(pointer: *const u8, len: usize) -> i32 {
    if pointer.is_null() {
        set_text("no bytecode was given".to_owned());
        return Status::Refused as i32;
    }
    // SAFETY: the caller states this is a buffer of `len` readable bytes,
    // and the null case is refused above. The slice is read and never kept.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, len) };
    match crate::capability_names(bytes) {
        Ok(names) => {
            set_text(names_json(&names));
            Status::Ok as i32
        }
        Err(error) => {
            set_text(format!("the bytecode was refused: {error}"));
            Status::Refused as i32
        }
    }
}

/// Verifies the bytecode at `pointer` and initialises it, answering a
/// handle for the instance, or 0 with the reason in the text. On success,
/// the text instead holds section 12.3's load result: the value a
/// top-level `return` in the entry module produced, rendered the same way
/// [`hs_wasm_call`]'s own result is, or the empty string for a module that
/// falls off the end without one. Capabilities bind automatically,
/// positionally: every `(capability, function)` pair the compiled code
/// calls reaches back out through `env.hs_capability_call` (see
/// [`hs_wasm_capability_names`] for what index means what), and the
/// initialisation run this performs can itself be cancelled through
/// `env.hs_wasm_poll_cancel`, exactly as a later [`hs_wasm_call`] can.
///
/// # Safety
///
/// `pointer` and `len` must describe a buffer of `len` readable bytes in
/// this module's memory, which [`hs_wasm_alloc`] is how a host obtains one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_load(pointer: *const u8, len: usize) -> u32 {
    clear_fault();
    if pointer.is_null() {
        set_text("no bytecode was given".to_owned());
        return 0;
    }
    // SAFETY: the caller states this is a buffer of `len` readable bytes,
    // and the null case is refused above. The slice is read and never kept.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, len) };
    let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let names = crate::capability_names(bytes)?;
        let types = crate::host_types(bytes)?;
        let (result_types, module_types) = crate::capability_result_types(bytes)?;
        let print_calls = crate::print_calls(bytes)?;
        let table = capability_table(&names, types, &result_types, &module_types, print_calls);
        WasmModule::load(bytes, table)
    }));
    let Ok(loaded) = loaded else {
        set_text("the implementation panicked".to_owned());
        return 0;
    };
    let mut module = match loaded {
        Ok(module) => module,
        Err(error) => {
            set_text(format!("the bytecode was refused: {error}"));
            return 0;
        }
    };
    let limits = LIMITS.with_borrow(Clone::clone);
    let initialised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut yielder = PollCancel;
        let mut control = Control::new(None, Some(&mut yielder));
        module.initialise(&limits, &mut control)
    }));
    let result_text = match initialised {
        Ok(Ok(None)) => String::new(),
        // Section 12.3's load result: the value a top-level `return` in the
        // entry module produced, rendered the same way `hs_wasm_call`'s own
        // result already is. A host reads it with `hs_wasm_text_len`/
        // `hs_wasm_take_text` right after a successful load, before it does
        // anything else with the handle.
        Ok(Ok(Some(value))) => match render_result(&mut module, value) {
            Ok(text) => text,
            Err(fault) => {
                report_fault(&fault);
                return 0;
            }
        },
        Ok(Err(fault)) => {
            report_fault(&fault);
            return 0;
        }
        Err(_) => {
            set_text("the implementation panicked".to_owned());
            return 0;
        }
    };
    set_text(result_text);
    install(module)
}

/// Calls the exported function named by the first buffer, with the
/// arguments in the second, a JSON array of scalars. The result is the text
/// of the returned value, or empty for a function that returns nothing.
///
/// # Safety
///
/// Each pointer and length must describe a readable buffer of that length
/// in this module's memory, or be null with a length of 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_call(
    handle: u32,
    name: *const u8,
    name_len: usize,
    args: *const u8,
    args_len: usize,
) -> i32 {
    // SAFETY: the caller states both describe readable buffers; `text_of`
    // refuses a null pointer without reading it.
    let (Some(name), Some(args)) = (unsafe { text_of(name, name_len) }, unsafe {
        text_of(args, args_len)
    }) else {
        set_text("the function name and the arguments must be UTF-8".to_owned());
        return Status::Refused as i32;
    };
    let limits = LIMITS.with_borrow(Clone::clone);
    clear_fault();
    with_instance(handle, |module| {
        let Some(function) = module.export_index(&name) else {
            set_text(format!("the program exports no function named '{name}'"));
            return Status::Refused as i32;
        };
        let arguments: Vec<Value> = match module.arguments(function, &args) {
            Ok(values) => values,
            Err(reason) => {
                set_text(reason);
                return Status::Refused as i32;
            }
        };
        let mut yielder = PollCancel;
        let mut control = Control::new(None, Some(&mut yielder));
        match module.call_export(function, &arguments, &limits, &mut control) {
            Ok(None) => {
                set_text(String::new());
                Status::Ok as i32
            }
            Ok(Some(value)) => finish_with_value(module, value),
            Err(fault) => {
                report_fault(&fault);
                Status::Faulted as i32
            }
        }
    })
}

// ---- the batched (suspend/resume) capability path --------------------
//
// A page whose host must answer a capability call asynchronously (an
// `await fetch(...)`, a message round trip to a main thread the page is
// not cross-origin isolated enough to share a `SharedArrayBuffer` with)
// cannot use `hs_wasm_load`/`hs_wasm_call` at all: nothing can make the
// synchronous `env.hs_capability_call` import wait for a JS `Promise`.
// These four exports are the batched path instead (see
// `WasmModule`'s own module documentation for the mechanism): a run pauses
// with `Status::Suspended` rather than blocking, the host reads what it is
// waiting for with `hs_wasm_commands_json`, answers however long that
// takes, and continues the same run with `hs_wasm_resume_json`, as many
// times as the program calls another capability, until it finishes or
// faults. No `SharedArrayBuffer` and no cross-origin isolation is needed
// anywhere in this path.
//
// Cancellation across a suspension needs nothing new either: the existing
// `env.hs_wasm_poll_cancel` import still governs every synchronous slice
// (the run between one call/resume and the `Suspended` it answers, or its
// completion), and between slices nothing here is running at all, so a
// host that wants to cancel a suspended instance simply does not resume it
// and disposes it instead (`hs_wasm_dispose`, which does not care whether
// the instance it releases is suspended).

/// As [`hs_wasm_load`], but on the batched path: every capability call
/// queues a command and the run pauses instead of blocking for an answer.
/// The handle this answers may already be `Status::Suspended` if the
/// module's own top-level statements call a capability; read
/// [`hs_wasm_commands_json`] to find out.
///
/// # Safety
///
/// As [`hs_wasm_load`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_load_batched(pointer: *const u8, len: usize) -> u32 {
    clear_fault();
    if pointer.is_null() {
        set_text("no bytecode was given".to_owned());
        return 0;
    }
    // SAFETY: as `hs_wasm_load`'s.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, len) };
    let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let names = crate::capability_names(bytes)?;
        let types = crate::host_types(bytes)?;
        let (result_types, module_types) = crate::capability_result_types(bytes)?;
        let print_calls = crate::print_calls(bytes)?;
        let table = capability_table(&names, types, &result_types, &module_types, print_calls);
        WasmModule::load(bytes, table)
    }));
    let Ok(loaded) = loaded else {
        set_text("the implementation panicked".to_owned());
        return 0;
    };
    let mut module = match loaded {
        Ok(module) => module,
        Err(error) => {
            set_text(format!("the bytecode was refused: {error}"));
            return 0;
        }
    };
    module.use_command_buffer();
    let limits = LIMITS.with_borrow(Clone::clone);
    let initialised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut yielder = PollCancel;
        let mut control = Control::new(None, Some(&mut yielder));
        module.initialise_batched(&limits, &mut control)
    }));
    // As `hs_wasm_load`, the text holds section 12.3's load result on
    // success; on `Run::Suspended` it is left empty, since the module's own
    // top-level statements have not finished yet and there is no result to
    // render until a `hs_wasm_resume_json` completes them.
    let result_text = match initialised {
        Ok(Ok(hollowscript_vm::Run::Done(None) | hollowscript_vm::Run::Suspended)) => String::new(),
        Ok(Ok(hollowscript_vm::Run::Done(Some(value)))) => {
            match render_result(&mut module, value) {
                Ok(text) => text,
                Err(fault) => {
                    report_fault(&fault);
                    return 0;
                }
            }
        }
        Ok(Err(fault)) => {
            report_fault(&fault);
            return 0;
        }
        Err(_) => {
            set_text("the implementation panicked".to_owned());
            return 0;
        }
    };
    set_text(result_text);
    install(module)
}

/// As [`hs_wasm_call`], but on the batched path: answers `Status::Suspended`
/// instead of blocking when the program calls a capability, with nothing
/// written to the text channel; drain what it is waiting for with
/// [`hs_wasm_commands_json`] and continue with [`hs_wasm_resume_json`].
///
/// # Safety
///
/// As [`hs_wasm_call`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_call_batched(
    handle: u32,
    name: *const u8,
    name_len: usize,
    args: *const u8,
    args_len: usize,
) -> i32 {
    // SAFETY: as `hs_wasm_call`'s.
    let (Some(name), Some(args)) = (unsafe { text_of(name, name_len) }, unsafe {
        text_of(args, args_len)
    }) else {
        set_text("the function name and the arguments must be UTF-8".to_owned());
        return Status::Refused as i32;
    };
    let limits = LIMITS.with_borrow(Clone::clone);
    clear_fault();
    with_instance(handle, |module| {
        let Some(function) = module.export_index(&name) else {
            set_text(format!("the program exports no function named '{name}'"));
            return Status::Refused as i32;
        };
        let arguments: Vec<Value> = match module.arguments(function, &args) {
            Ok(values) => values,
            Err(reason) => {
                set_text(reason);
                return Status::Refused as i32;
            }
        };
        let mut yielder = PollCancel;
        let mut control = Control::new(None, Some(&mut yielder));
        let run = module.call_export_batched(function, &arguments, &limits, &mut control);
        run_status(module, run)
    })
}

/// The pending capability calls the last [`hs_wasm_call_batched`] or
/// [`hs_wasm_resume_json`] on `handle` paused at, as a JSON array to the
/// text channel: `[{"function":0,"args":[...]}, ...]`. `function` is the
/// same positional index [`hs_wasm_capability_names`] lists (the second
/// element of that call's own array is the name this one's `function`
/// means). Draining: a second call with nothing queued since the last one
/// answers `[]`.
///
/// # Safety
///
/// None beyond the handle being looked up, never followed.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_commands_json(handle: u32) -> i32 {
    with_instance(handle, |module| {
        clear_fault();
        let commands = module.commands();
        // One budget for the whole answer, so the text of every argument of
        // every command together is bounded by the output limit.
        let rendered = with_json_budget(output_limit(), |budget| {
            let mut out = String::from("[");
            for (i, command) in commands.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _ = write!(out, r#"{{"function":{},"args":["#, command.function);
                for (j, &arg) in command.args.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    out.push_str(&module.command_argument_json(arg, budget)?);
                }
                out.push_str("]}");
            }
            out.push(']');
            Ok::<String, Fault>(out)
        });
        match rendered {
            Ok(out) => {
                set_text(out);
                Status::Ok as i32
            }
            Err(fault) => {
                // The commands were drained to be written, so the run they
                // paused cannot be resumed with answers: the host disposes
                // of the instance, as it does after any fault.
                report_fault(&fault);
                Status::Faulted as i32
            }
        }
    })
}

/// Continues the run [`hs_wasm_call_batched`] (or a previous
/// [`hs_wasm_resume_json`]) paused, with `results_json`, a JSON array with
/// exactly one entry per command [`hs_wasm_commands_json`] last answered,
/// in the same order. Each entry is either a plain JSON scalar (or, for a
/// capability declared `Result<T, E>`, a one-field object `{"Ok": ...}` or
/// `{"Err": ...}`, exactly as [`hs_wasm_call`]'s own capability results
/// already work), meaning that call succeeded, or `{"error": "message"}`,
/// meaning the call itself failed (reaching the program as HS1012, not as
/// a value it can match). May itself answer `Status::Suspended` again, if
/// the program calls another capability before it finishes.
///
/// # Safety
///
/// `results_ptr`/`results_len` must describe a buffer of `results_len`
/// readable bytes in this module's memory, or be null with a length of 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_resume_json(
    handle: u32,
    results_ptr: *const u8,
    results_len: usize,
) -> i32 {
    // SAFETY: the caller states this describes a readable buffer, or is
    // null with a length of 0; `text_of` refuses a null pointer without
    // reading it.
    let Some(results_json) = (unsafe { text_of(results_ptr, results_len) }) else {
        set_text("the results must be UTF-8".to_owned());
        return Status::Refused as i32;
    };
    let limits = LIMITS.with_borrow(Clone::clone);
    clear_fault();
    with_instance(handle, |module| {
        let Ok(hollowscript_vm::json::Json::Array(items)) =
            hollowscript_vm::json::parse(results_json.trim())
        else {
            set_text("the results must be a JSON array".to_owned());
            return Status::Refused as i32;
        };
        let mut results = Vec::with_capacity(items.len());
        for (index, item) in items.into_iter().enumerate() {
            let result = match <[(String, hollowscript_vm::json::Json); 1]>::try_from(match &item {
                hollowscript_vm::json::Json::Object(fields) => fields.clone(),
                _ => Vec::new(),
            }) {
                Ok([(key, value)]) if key == "error" => {
                    let message = match value {
                        hollowscript_vm::json::Json::String(s) => s,
                        _ => "the capability failed".to_owned(),
                    };
                    CommandResult::Err(Fault::new(
                        FaultCode::CapabilityFailed,
                        &[("name", &"capability"), ("message", &message)],
                    ))
                }
                _ => match module.value_of_json_result(index, item) {
                    Ok(value) => CommandResult::Ok(value),
                    Err(fault) => CommandResult::Err(fault),
                },
            };
            results.push(result);
        }
        let mut yielder = PollCancel;
        let mut control = Control::new(None, Some(&mut yielder));
        let run = module.resume(results, &limits, &mut control);
        run_status(module, run)
    })
}

/// Writes `value` to the text channel in the host's chosen format and answers
/// [`Status::Ok`], or, when the value cannot be written within the output
/// limit (or the host asks to stop while it is), reports that fault and
/// answers [`Status::Faulted`]. The run itself has finished by now, so the
/// instance is exactly as it is after any successful call: only the value
/// could not be delivered.
fn finish_with_value(module: &mut WasmModule, value: Value) -> i32 {
    match render_result(module, value) {
        Ok(text) => {
            set_text(text);
            Status::Ok as i32
        }
        Err(fault) => {
            report_fault(&fault);
            Status::Faulted as i32
        }
    }
}

/// Turns a [`Run`] answer (or the fault it faulted with) into the status
/// [`hs_wasm_call_batched`] and [`hs_wasm_resume_json`] both report,
/// writing the text channel the same way the synchronous path does.
fn run_status(module: &mut WasmModule, run: Result<hollowscript_vm::Run, Fault>) -> i32 {
    match run {
        Ok(hollowscript_vm::Run::Done(None)) => {
            set_text(String::new());
            Status::Ok as i32
        }
        Ok(hollowscript_vm::Run::Done(Some(value))) => finish_with_value(module, value),
        Ok(hollowscript_vm::Run::Suspended) => {
            set_text(String::new());
            Status::Suspended as i32
        }
        Err(fault) => {
            report_fault(&fault);
            Status::Faulted as i32
        }
    }
}

/// Reads an exported `constant`, as text.
///
/// # Safety
///
/// As [`hs_wasm_call`], for the one buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_read(handle: u32, name: *const u8, name_len: usize) -> i32 {
    // SAFETY: the caller states this describes a readable buffer.
    let Some(name) = (unsafe { text_of(name, name_len) }) else {
        set_text("the constant's name must be UTF-8".to_owned());
        return Status::Refused as i32;
    };
    with_instance(handle, |module| {
        let Some(slot) = module.constant_slot(&name) else {
            set_text(format!("the program exports no constant named '{name}'"));
            return Status::Refused as i32;
        };
        let value = module.global(slot).unwrap_or(Value::None);
        finish_with_value(module, value)
    })
}

/// Ends an instance and releases everything it holds. A handle that names
/// no live instance is ignored, so a host may call this twice.
///
/// Calling this from inside a capability handler for the same instance
/// (`handle`'s slot is `Slot::Busy`, since `with_instance` is running
/// its call) is honoured, not silently undone: the slot is emptied here,
/// the same as any other dispose, and `with_instance` checks the slot is
/// still `Busy`, not already emptied, before it puts the instance back
/// once that call returns (a self-dispose during a re-entrant call).
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_dispose(handle: u32) -> i32 {
    INSTANCES.with_borrow_mut(|instances| instances.release(handle));
    Status::Ok as i32
}

// ---- helpers ------------------------------------------------------------

/// Stores a loaded instance and answers its handle, or, when every one of the
/// table's positions is in use, 0 with the reason in the text.
fn install(module: WasmModule) -> u32 {
    let handle = INSTANCES.with_borrow_mut(|instances| instances.install(module));
    handle.unwrap_or_else(|| {
        set_text(format!(
            "too many live instances: at most {MAX_INSTANCES} at once, dispose one first"
        ));
        0
    })
}

pub(crate) fn set_text(text: String) {
    TEXT.with_borrow_mut(|held| *held = text);
}

/// Runs `body` against the instance `handle` names, or refuses.
///
/// **Re-entrancy.** A synchronous capability handler can call
/// back into the very instance it is itself being called from (a JS
/// `print` implementation calling `inst.call` on its own instance is
/// exactly this). Before this, that read the handle's slot as `None`,
/// indistinguishable from a handle that had never named an instance or
/// had been disposed, and answered the same generic `Status::Refused`. The
/// slot is now left as [`Slot::Busy`], not emptied, for the length of the
/// call, so a re-entrant call on the same handle answers
/// [`Status::Busy`], which tells the host what actually happened and that
/// the handle is still good once the outer call returns.
fn with_instance(handle: u32, body: impl FnOnce(&mut WasmModule) -> i32) -> i32 {
    let taken = INSTANCES.with_borrow_mut(|instances| {
        let slot = &mut instances.entry_mut(handle)?.slot;
        match std::mem::replace(slot, Slot::Busy) {
            Slot::Occupied(module) => Some(Ok(module)),
            Slot::Busy => {
                // Leave it `Busy`: this call did not take it, so it is not
                // this call's place to change what the other, outer call
                // put there.
                *slot = Slot::Busy;
                Some(Err(()))
            }
            Slot::Empty => {
                *slot = Slot::Empty;
                None
            }
        }
    });
    let mut module = match taken {
        Some(Ok(module)) => module,
        Some(Err(())) => {
            set_text("a call on this instance is already in progress".to_owned());
            return Status::Busy as i32;
        }
        None => {
            set_text("that handle names no live instance".to_owned());
            return Status::Refused as i32;
        }
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut module)));
    let Ok(status) = outcome else {
        // The instance is not put back: a panic could have left it half
        // changed, so the slot is left `Busy` (never `Empty`, which
        // `hs_wasm_dispose` alone should mean) and every later call on the
        // handle is refused the same way a real re-entrant call would be.
        // Section 19's own promise ("a panic never crosses a boundary")
        // holds for the C ABI's own thread, where `catch_unwind` genuinely
        // stops the unwind; on a `wasm32-unknown-unknown` build there is no
        // unwinding support to catch at all (this `catch_unwind` compiles
        // but never triggers, per `std`'s own panic-abort default for that
        // target), so a trap there abandons whatever the interpreter's own
        // Rust call frames were doing mid-operation and this arm is not
        // reached; `docs/design/security.md` and `hollowscript.js` say so.
        set_text("the implementation panicked".to_owned());
        return Status::Panicked as i32;
    };
    // Put it back where it was, but only if the slot is still `Busy`:
    // `hs_wasm_dispose`, called from inside `body` on this same handle
    // (the self-dispose case), already set it `Empty`, and that must win,
    // not be silently overwritten by putting the disposed instance back.
    // The generation is part of the handle, so a position that was disposed
    // and then reused for another instance during the call is not this one.
    INSTANCES.with_borrow_mut(|instances| {
        if let Some(entry) = instances.entry_mut(handle) {
            if matches!(entry.slot, Slot::Busy) {
                entry.slot = Slot::Occupied(module);
            }
        }
    });
    status
}

/// A buffer as a `String`, or `None` for a null pointer or bytes that are
/// not UTF-8. A length of 0 is the empty string, whatever the pointer.
///
/// # Safety
///
/// `pointer` must be null, or describe `len` readable bytes.
pub(crate) unsafe fn text_of(pointer: *const u8, len: usize) -> Option<String> {
    if len == 0 {
        return Some(String::new());
    }
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller states this describes `len` readable bytes, and
    // the null and empty cases are handled above. The slice is copied out
    // and never kept.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, len) };
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        Status, call_capability, host_stub, hs_wasm_alloc, hs_wasm_call, hs_wasm_call_batched,
        hs_wasm_capability_names, hs_wasm_commands_json, hs_wasm_dispose, hs_wasm_fault_json,
        hs_wasm_free, hs_wasm_load, hs_wasm_load_batched, hs_wasm_read, hs_wasm_resume_json,
        hs_wasm_set_result_format, hs_wasm_take_text, hs_wasm_text_len, hs_wasm_version,
        poll_cancel,
    };
    use hollowscript_bytecode::module::{
        CapCall, Capability, CapabilityKind, Export, ExportKind, Function, FunctionKind, Global,
        Module, ModuleEntry, Signature,
    };
    use hollowscript_bytecode::opcode::{Instruction, Op};
    use hollowscript_core::transfer::TransferType;

    /// Writes `bytes` into a buffer this module owns, the way a host page
    /// does, and answers the pointer and length to pass on.
    fn upload(bytes: &[u8]) -> (*mut u8, usize) {
        let pointer = hs_wasm_alloc(bytes.len());
        assert!(!pointer.is_null());
        // SAFETY: the buffer was just allocated with exactly this length.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len()) };
        (pointer, bytes.len())
    }

    /// The text the last call produced, taken and released as a host does.
    fn text() -> String {
        let len = hs_wasm_text_len();
        let pointer = hs_wasm_take_text();
        if pointer.is_null() || len == 0 {
            return String::new();
        }
        // SAFETY: the pointer and length came from this module, together.
        let bytes = unsafe { std::slice::from_raw_parts(pointer, len) }.to_vec();
        // SAFETY: the same pointer and length, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        String::from_utf8(bytes).unwrap_or_default()
    }

    /// A module exporting `twice(n)` and the constant `answer`.
    fn sample() -> Vec<u8> {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let int = m.types.add(TransferType::Int);
        let twice = m.add_string("twice").unwrap();
        m.functions.push(Function {
            name: twice,
            kind: FunctionKind::Function,
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
        let init = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::asbx(Op::LoadInt, 0, 42).encode(),
                Instruction::abx(Op::SetGlobal, 0, 0).encode(),
                Instruction::abc(Op::ReturnNone, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let answer = m.add_string("answer").unwrap();
        m.globals.push(Global {
            module: 0,
            name: answer,
            mutable: false,
        });
        m.exports.push(Export {
            name: twice,
            kind: ExportKind::Function {
                function: 0,
                signature: Signature {
                    params: vec![int],
                    ret: Some(int),
                },
            },
        });
        m.exports.push(Export {
            name: answer,
            kind: ExportKind::Constant { global: 0, ty: int },
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        hollowscript_bytecode::encode(&m)
    }

    /// The whole boundary as a host page walks it: upload, load, call,
    /// read, dispose.
    #[test]
    fn a_page_loads_calls_reads_and_disposes() {
        assert_eq!(hs_wasm_version(), Status::Ok as i32);
        assert!(!text().is_empty(), "the version is not empty");

        let bytes = sample();
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer was just written with exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed; the VM copied what it needs.
        unsafe { hs_wasm_free(pointer, len) };
        assert!(handle != 0, "the module loads");

        let name = upload(b"twice");
        let args = upload(b"[21]");
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, args.0, args.1) };
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(text(), "Int(42)");

        let constant = upload(b"answer");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_read(handle, constant.0, constant.1) };
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(text(), "Int(42)");

        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(constant.0, constant.1);
        }
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);

        // Everything after a dispose is refused rather than reaching memory.
        let name = upload(b"twice");
        let args = upload(b"[1]");
        // SAFETY: both buffers are live.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, args.0, args.1) };
        assert_eq!(status, Status::Refused as i32);
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
    }

    /// A module exporting `go()`, which calls the one capability `print`
    /// with a fixed string and returns nothing.
    fn sample_with_print() -> Vec<u8> {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let string_ty = m.types.add(TransferType::String);
        let print_name = m.add_string("print").unwrap();
        m.capabilities.push(Capability {
            name: print_name,
            kind: CapabilityKind::Function(Signature {
                params: vec![string_ty],
                ret: None,
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let text = m.add_string("hi").unwrap();
        let go = m.add_string("go").unwrap();
        m.functions.push(Function {
            name: go,
            kind: FunctionKind::Function,
            arity: 0,
            register_count: 2,
            returns_value: false,
            captures: vec![],
            constants: vec![hollowscript_bytecode::module::Constant::Str(text)],
            code: vec![
                // `CallCap`'s destination is register 0 and its arguments
                // follow at 1, 2, ...: the one `String` argument goes in
                // register 1, not 0.
                Instruction::abx(Op::LoadK, 1, 0).encode(),
                Instruction::abx(Op::CallCap, 0, 0).encode(),
                Instruction::abc(Op::ReturnNone, 0, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.exports.push(Export {
            name: go,
            kind: ExportKind::Function {
                function: 0,
                signature: Signature {
                    params: vec![],
                    ret: None,
                },
            },
        });
        m.modules.push(ModuleEntry { path, init: 1 });
        hollowscript_bytecode::encode(&m)
    }

    /// A synchronous capability handler (a JS `print` implementation,
    /// say) that calls back into the very instance it is itself being
    /// called from must be told `Busy`, not the same generic "no live
    /// instance" a disposed or never-allocated handle also answers, and the
    /// handle must still work normally once the outer call returns.
    #[test]
    fn a_capability_handler_re_entering_its_own_instance_gets_busy() {
        let bytes = sample_with_print();
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer was just written with exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert!(handle != 0, "the module loads");

        let reentrant_status = std::rc::Rc::new(std::cell::Cell::new(-1));
        let recorded = reentrant_status.clone();
        host_stub::set_capability(Some(Box::new(move |_index, _args_json| {
            let name = upload(b"go");
            // SAFETY: the buffer is live.
            let empty_args = upload(b"[]");
            // SAFETY: the buffer is live.
            let status =
                unsafe { hs_wasm_call(handle, name.0, name.1, empty_args.0, empty_args.1) };
            // SAFETY: as above.
            unsafe { hs_wasm_free(empty_args.0, empty_args.1) };
            recorded.set(status);
            // SAFETY: as above.
            unsafe { hs_wasm_free(name.0, name.1) };
            (true, b"null".to_vec())
        })));

        let name = upload(b"go");
        // SAFETY: the buffer is live.
        let empty_args = upload(b"[]");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, empty_args.0, empty_args.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(empty_args.0, empty_args.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(name.0, name.1) };

        assert_eq!(
            reentrant_status.get(),
            Status::Busy as i32,
            "the re-entrant call answers Busy, not a bare refusal"
        );
        assert_eq!(
            status,
            Status::Ok as i32,
            "the outer call still completes normally"
        );

        // The handle is not left `Busy` forever: a later, ordinary call
        // (still with a capability handler installed, just not a
        // re-entrant one, so this checks the handle's own state rather
        // than repeating the unrelated "no handler installed" fault)
        // works normally.
        host_stub::set_capability(Some(Box::new(|_index, _args_json| {
            (true, b"null".to_vec())
        })));
        let name = upload(b"go");
        let empty_args = upload(b"[]");
        // SAFETY: both buffers are live.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, empty_args.0, empty_args.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(name.0, name.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(empty_args.0, empty_args.1) };
        host_stub::set_capability(None);
        assert_eq!(status, Status::Ok as i32);

        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// The self-dispose case: a capability handler that disposes its
    /// own instance mid-call must have that dispose take effect once the
    /// outer call returns, not be silently undone by the outer call
    /// putting the (supposedly disposed) instance back.
    #[test]
    fn disposing_an_instance_from_its_own_capability_handler_takes_effect() {
        let bytes = sample_with_print();
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer was just written with exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert!(handle != 0, "the module loads");

        host_stub::set_capability(Some(Box::new(move |_index, _args_json| {
            assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
            (true, b"null".to_vec())
        })));

        let name = upload(b"go");
        // SAFETY: the buffer is live.
        let empty_args = upload(b"[]");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, empty_args.0, empty_args.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(empty_args.0, empty_args.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(name.0, name.1) };
        host_stub::set_capability(None);
        assert_eq!(
            status,
            Status::Ok as i32,
            "the call that disposed itself still completes"
        );

        // The dispose from inside the call must have taken: a later call
        // on the same handle is refused, not silently handed the instance
        // the outer call would otherwise have put back.
        let name = upload(b"go");
        // SAFETY: the buffer is live.
        let empty_args = upload(b"[]");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, empty_args.0, empty_args.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(empty_args.0, empty_args.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(name.0, name.1) };
        assert_eq!(
            status,
            Status::Refused as i32,
            "the instance was disposed from inside its own call and must stay disposed"
        );
    }

    /// Bytes that are not a container are refused with a reason, and no
    /// handle is created.
    #[test]
    fn bytes_that_are_not_bytecode_are_refused_with_a_reason() {
        let (pointer, len) = upload(b"not bytecode at all");
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(handle, 0);
        assert!(text().contains("refused"));
    }

    /// A handle this module never gave out is refused, not dereferenced.
    #[test]
    fn an_unknown_handle_is_refused() {
        let name = upload(b"twice");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_call(9_999, name.0, name.1, std::ptr::null(), 0) };
        assert_eq!(status, Status::Refused as i32);
        // SAFETY: the same buffer, not yet freed.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
    }

    /// A module declaring a plain-function capability and a namespace
    /// member, both actually called, so `cap_calls` names both.
    fn module_with_capabilities() -> Vec<u8> {
        let mut m = Module::new();
        let string_ty = m.types.add(TransferType::String);
        let print_name = m.add_string("print").unwrap();
        let clock_name = m.add_string("clock").unwrap();
        let now_name = m.add_string("now").unwrap();
        m.capabilities.push(Capability {
            name: print_name,
            kind: CapabilityKind::Function(Signature {
                params: vec![string_ty],
                ret: None,
            }),
        });
        m.capabilities.push(Capability {
            name: clock_name,
            kind: CapabilityKind::Namespace(vec![(
                now_name,
                Signature {
                    params: vec![],
                    ret: Some(string_ty),
                },
            )]),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        m.cap_calls.push(CapCall {
            capability: 1,
            function: 0,
        });
        let path = m.add_string("main.hws").unwrap();
        let init = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init,
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
        hollowscript_bytecode::encode(&m)
    }

    /// [`crate::capability_names`] names a plain function by its own name
    /// and a namespace member as `capability.function`, in the order
    /// `cap_calls` lists them, which is exactly the order a host must bind
    /// its JS functions in.
    #[test]
    fn capability_names_lists_a_function_and_a_namespace_member() {
        let names = crate::capability_names(&module_with_capabilities()).unwrap();
        assert_eq!(names, vec!["print".to_owned(), "clock.now".to_owned()]);
    }

    /// The same list, through the wasm boundary itself.
    #[test]
    fn hs_wasm_capability_names_answers_the_same_list_as_json() {
        let (pointer, len) = upload(&module_with_capabilities());
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe { hs_wasm_capability_names(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(text(), r#"["print","clock.now"]"#);
    }

    /// [`hs_wasm_capability_names`] refuses bytes that do not verify, the
    /// same way [`hs_wasm_load`] does, rather than reporting an empty list.
    #[test]
    fn hs_wasm_capability_names_refuses_bytes_that_are_not_bytecode() {
        let (pointer, len) = upload(b"not bytecode at all");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_capability_names(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, Status::Refused as i32);
        assert!(text().contains("refused"));
    }

    /// [`call_capability`] marshals through the host stub exactly the way
    /// the real `env.hs_capability_call`/`hs_capability_result_len`/
    /// `hs_capability_take_result` imports would on `wasm32`: the index and
    /// the JSON arguments reach the handler, and its answer, success or
    /// failure, comes back as the JSON text or the message.
    #[test]
    fn call_capability_round_trips_through_the_host_stub() {
        host_stub::set_capability(Some(Box::new(|index, args_json| {
            assert_eq!(index, 3);
            assert_eq!(args_json, br#"["hi"]"#);
            (true, b"42".to_vec())
        })));
        let result = call_capability(3, br#"["hi"]"#);
        host_stub::set_capability(None);
        assert_eq!(result, Ok(b"42".to_vec()));
    }

    /// A failing capability's message comes back as `Err`, not folded into
    /// a success value.
    #[test]
    fn call_capability_reports_a_failure_message() {
        host_stub::set_capability(Some(Box::new(|_, _| (false, b"nope".to_vec()))));
        let result = call_capability(0, b"[]");
        host_stub::set_capability(None);
        assert_eq!(result, Err(b"nope".to_vec()));
    }

    /// The full synchronous path, end to end: a capability declaring a
    /// record parameter and a record result (Studio's `entities.get`,
    /// cited by the Studio integration) carries one across as
    /// `{"record": {...}}`, the same encoding this crate's own root module
    /// documentation defines and `hollowscript-capi`'s C ABI uses, not only
    /// the four scalars this boundary used to be limited to.
    #[test]
    #[cfg(feature = "tooling")]
    fn a_capability_carries_a_record_argument_and_result_end_to_end() {
        let source = concat!(
            "uses entities\n",
            "export fn run(): Int {\n",
            "    constant e = effect entities.get(\"player-1\")\n",
            "    return e.x + e.y\n",
            "}\n",
        );
        let capabilities = concat!(
            r#"[{"name":"entities","function":"get","params":["String"],"#,
            r#""result":{"record":{"id":"String","x":"Int","y":"Int"}}}]"#,
        );
        let bytes = crate::tooling::compile(source, "main.hws", capabilities, "", "")
            .unwrap_or_else(|diagnostics| panic!("{diagnostics}"));

        host_stub::set_capability(Some(Box::new(|_index, args_json| {
            assert_eq!(args_json, br#"["player-1"]"#);
            (
                true,
                br#"{"record":{"id":"player-1","x":3,"y":4}}"#.to_vec(),
            )
        })));
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer was just written with exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert!(handle != 0, "{}", text());

        let name = upload(b"run");
        let args = upload(b"[]");
        // SAFETY: both buffers are live.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        host_stub::set_capability(None);
        assert_eq!(status, Status::Ok as i32, "{}", text());
        assert_eq!(text(), "Int(7)");

        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// A capability answering a record `{x: Float, y: Float}` as
    /// `{"x": 1, "y": 0}` (both look integral in JSON) decodes each field
    /// to a genuine `Float`, not an `Int`: the declared type, not the
    /// JSON's own spelling, says which one a bare number means (see
    /// `crate::coerce_json_numbers`'s own documentation).
    #[test]
    #[cfg(feature = "tooling")]
    fn a_synchronous_capability_result_directs_a_bare_number_to_float() {
        let source = concat!(
            "uses points\n",
            "export fn point(): { x: Float, y: Float } {\n",
            "    return effect points.get(\"origin\")\n",
            "}\n",
        );
        let capabilities = concat!(
            r#"[{"name":"points","function":"get","params":["String"],"#,
            r#""result":{"record":{"x":"Float","y":"Float"}}}]"#,
        );
        let bytes = crate::tooling::compile(source, "main.hws", capabilities, "", "")
            .unwrap_or_else(|diagnostics| panic!("{diagnostics}"));

        host_stub::set_capability(Some(Box::new(|_index, _args_json| {
            (true, br#"{"record":{"x":1,"y":0}}"#.to_vec())
        })));
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer was just written with exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert!(handle != 0, "{}", text());

        let name = upload(b"point");
        let args = upload(b"[]");
        // SAFETY: both buffers are live.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        host_stub::set_capability(None);
        assert_eq!(status, Status::Ok as i32, "{}", text());
        assert_eq!(text(), "{x: Float(1.0), y: Float(0.0)}");

        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// The same disambiguation for `hs_call`'s own arguments: an exported
    /// function's declared `Float` parameter accepts a bare, integral-
    /// looking JSON number.
    #[test]
    fn hs_wasm_call_directs_a_bare_number_argument_to_float() {
        let mut m = Module::new();
        let float_ty = m.types.add(TransferType::Float);
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("describe").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::Function,
            arity: 1,
            register_count: 1,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::Return, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.exports.push(Export {
            name,
            kind: ExportKind::Function {
                function: 0,
                signature: Signature {
                    params: vec![float_ty],
                    ret: Some(float_ty),
                },
            },
        });
        let init = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init,
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
        let bytes = hollowscript_bytecode::encode(&m);

        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer was just written with exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert!(handle != 0, "{}", text());

        let call_name = upload(b"describe");
        let args = upload(b"[3]");
        // SAFETY: both buffers are live.
        let status = unsafe { hs_wasm_call(handle, call_name.0, call_name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(call_name.0, call_name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        assert_eq!(status, Status::Ok as i32, "{}", text());
        assert_eq!(text(), "Float(3.0)");

        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// With no handler installed, a capability call fails with a message
    /// that says so, rather than panicking or hanging.
    #[test]
    fn call_capability_with_no_handler_installed_fails_cleanly() {
        let result = call_capability(0, b"[]");
        assert!(result.is_err());
    }

    /// [`poll_cancel`] reads the same flag [`host_stub::set_cancelled`]
    /// sets, which is what backs the [`super::PollCancel`] yield callback
    /// every run of every instance uses in place of `env.hs_wasm_poll_cancel`
    /// on a native build.
    #[test]
    fn poll_cancel_reads_the_host_stub_flag() {
        host_stub::set_cancelled(true);
        assert!(poll_cancel());
        host_stub::set_cancelled(false);
        assert!(!poll_cancel());
    }

    /// The batched suspend/resume path, end to end, against bytecode a real
    /// compile produced (a hand-assembled `CallCap` instruction would be too
    /// easy to get subtly wrong to trust as the only coverage of this):
    /// `hs_wasm_call_batched` pauses at a capability call, `hs_wasm_commands_json`
    /// answers what it is waiting for, and `hs_wasm_resume_json` finishes
    /// the run with the host's own answer, exactly as an async host that
    /// awaited something between the two would.
    #[test]
    fn a_capability_call_suspends_and_resumes_with_the_hosts_async_answer() {
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let int_ty = types.add(TransferType::Int);
        let capabilities = hollowscript::CapabilityInterface {
            capabilities: vec![hollowscript::CapabilityDecl {
                name: "double".to_owned(),
                shape: hollowscript::CapabilityShape::Function {
                    params: vec![int_ty],
                    ret: Some(int_ty),
                },
            }],
            types,
        };
        let options = hollowscript::Options {
            capabilities,
            ..hollowscript::Options::default()
        };
        let source = "uses double\nexport fn run(): Int {\n    return effect double(21)\n}\n";
        let outcome = hollowscript::build(
            source,
            "main.hws",
            &options,
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        );
        let hollowscript::BuildOutcome::Bytecode(bytes) = outcome else {
            panic!("the fixture program should compile cleanly");
        };

        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load_batched(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());

        let name = upload(b"run");
        let args = upload(b"[]");
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call_batched(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        assert_eq!(status, Status::Suspended as i32, "{}", text());

        let status = hs_wasm_commands_json(handle);
        assert_eq!(status, Status::Ok as i32);
        let commands = text();
        assert!(commands.contains(r#""function":0"#), "{commands}");
        assert!(commands.contains("21"), "{commands}");

        let results = upload(b"[42]");
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe { hs_wasm_resume_json(handle, results.0, results.1) };
        // SAFETY: the same buffer, not yet freed.
        unsafe {
            hs_wasm_free(results.0, results.1);
        }
        assert_eq!(status, Status::Ok as i32, "{}", text());
        assert_eq!(text(), "Int(42)");

        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// The batched path's own resume answer is directed by the declared
    /// type too: a capability declared to answer `Float` accepts a bare,
    /// integral-looking JSON number the same way the synchronous path does
    /// (see `crate::coerce_json_numbers`'s own documentation), since the
    /// `Command` itself is long gone by the time the host's asynchronous
    /// answer comes back, so [`WasmModule::value_of_json_result`]'s own
    /// `index` argument is what finds the capability that declared it.
    #[test]
    fn a_batched_resume_answer_directs_a_bare_number_to_float() {
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let float_ty = types.add(TransferType::Float);
        let capabilities = hollowscript::CapabilityInterface {
            capabilities: vec![hollowscript::CapabilityDecl {
                name: "half".to_owned(),
                shape: hollowscript::CapabilityShape::Function {
                    params: vec![float_ty],
                    ret: Some(float_ty),
                },
            }],
            types,
        };
        let options = hollowscript::Options {
            capabilities,
            ..hollowscript::Options::default()
        };
        let source = "uses half\nexport fn run(): Float {\n    return effect half(6.0)\n}\n";
        let outcome = hollowscript::build(
            source,
            "main.hws",
            &options,
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        );
        let hollowscript::BuildOutcome::Bytecode(bytes) = outcome else {
            panic!("the fixture program should compile cleanly");
        };

        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load_batched(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());

        let name = upload(b"run");
        let args = upload(b"[]");
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call_batched(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        assert_eq!(status, Status::Suspended as i32, "{}", text());

        let status = hs_wasm_commands_json(handle);
        assert_eq!(status, Status::Ok as i32);
        let _ = text();

        // `3`, not `3.0`: exactly the shape a JS host cannot avoid writing
        // for a value that started life as `Number(3)`.
        let results = upload(b"[3]");
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe { hs_wasm_resume_json(handle, results.0, results.1) };
        // SAFETY: the same buffer, not yet freed.
        unsafe {
            hs_wasm_free(results.0, results.1);
        }
        assert_eq!(status, Status::Ok as i32, "{}", text());
        assert_eq!(text(), "Float(3.0)");

        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// A malformed answer to `hs_wasm_resume_json` is refused, not
    /// panicked on, and does not leave the instance unusable.
    #[test]
    fn resume_with_malformed_results_is_refused() {
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let int_ty = types.add(TransferType::Int);
        let capabilities = hollowscript::CapabilityInterface {
            capabilities: vec![hollowscript::CapabilityDecl {
                name: "double".to_owned(),
                shape: hollowscript::CapabilityShape::Function {
                    params: vec![int_ty],
                    ret: Some(int_ty),
                },
            }],
            types,
        };
        let options = hollowscript::Options {
            capabilities,
            ..hollowscript::Options::default()
        };
        let source = "uses double\nexport fn run(): Int {\n    return effect double(21)\n}\n";
        let hollowscript::BuildOutcome::Bytecode(bytes) = hollowscript::build(
            source,
            "main.hws",
            &options,
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) else {
            panic!("the fixture program should compile cleanly");
        };
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load_batched(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        let name = upload(b"run");
        let args = upload(b"[]");
        // SAFETY: both buffers are live.
        unsafe { hs_wasm_call_batched(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        let _ = hs_wasm_commands_json(handle);

        let results = upload(b"not json");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_resume_json(handle, results.0, results.1) };
        // SAFETY: the same buffer, not yet freed.
        unsafe {
            hs_wasm_free(results.0, results.1);
        }
        assert_eq!(status, Status::Refused as i32);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// [`hs_wasm_commands_json`] and [`hs_wasm_resume_json`] on a handle
    /// this module never gave out are refused, like every other entry point.
    #[test]
    fn batched_entry_points_refuse_an_unknown_handle() {
        assert_eq!(hs_wasm_commands_json(9_999), Status::Refused as i32);
        let results = upload(b"[]");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_resume_json(9_999, results.0, results.1) };
        // SAFETY: the same buffer, not yet freed.
        unsafe {
            hs_wasm_free(results.0, results.1);
        }
        assert_eq!(status, Status::Refused as i32);
    }

    /// `hs_wasm_load`'s text channel, on success, holds section 12.3's load
    /// result (the value a top-level `return` in the entry module produced),
    /// not the empty string it used to be left as regardless: a page cannot
    /// read a program's top-level result at all otherwise.
    #[test]
    fn hs_wasm_load_answers_the_top_level_return_value_as_the_load_result() {
        let hollowscript::BuildOutcome::Bytecode(bytes) = hollowscript::build(
            "return 42",
            "main.hws",
            &hollowscript::Options::default(),
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) else {
            panic!("the fixture program should compile cleanly");
        };
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0);
        assert_eq!(text(), "Int(42)");
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// A module with no top-level `return` (falls off the end) answers the
    /// empty string as its load result, the same as a function that returns
    /// nothing does for `hs_wasm_call`.
    #[test]
    fn hs_wasm_load_answers_an_empty_load_result_when_there_is_no_top_level_return() {
        let hollowscript::BuildOutcome::Bytecode(bytes) = hollowscript::build(
            "constant x = 1",
            "main.hws",
            &hollowscript::Options::default(),
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) else {
            panic!("the fixture program should compile cleanly");
        };
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0);
        assert_eq!(text(), "");
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    // ---- structured results and faults ------------------------------------

    /// Compiles `source` with only the standard capabilities and loads it,
    /// answering the handle (and leaving the load result on the text channel).
    fn load_source(source: &str) -> u32 {
        let bytes = match hollowscript::build(
            source,
            "main.hws",
            &hollowscript::Options::default(),
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) {
            hollowscript::BuildOutcome::Bytecode(bytes) => bytes,
            hollowscript::BuildOutcome::CheckFailed(diagnostics) => {
                panic!("the fixture program should compile cleanly: {diagnostics:?}")
            }
        };
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        handle
    }

    /// Calls the export `name` with the JSON arguments `args`, answering the
    /// status and the text channel.
    fn call_export(handle: u32, name: &str, args: &str) -> (i32, String) {
        let name = upload(name.as_bytes());
        let args = upload(args.as_bytes());
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        (status, text())
    }

    /// The fault of the last call, as the JSON a host reads.
    fn fault() -> String {
        assert_eq!(hs_wasm_fault_json(), Status::Ok as i32);
        text()
    }

    /// Restores the default result format when dropped, so that a test that
    /// asks for JSON cannot leak the choice into another test on the same thread.
    struct JsonResults;

    impl JsonResults {
        fn on() -> JsonResults {
            assert_eq!(hs_wasm_set_result_format(1), Status::Ok as i32);
            JsonResults
        }
    }

    impl Drop for JsonResults {
        fn drop(&mut self) {
            hs_wasm_set_result_format(0);
        }
    }

    const SHAPES: &str = concat!(
        "record Point = { x: Int, y: Int }\n",
        "enum Shape { Dot, Circle(Int) }\n",
        "export fn nothing() {\n}\n",
        "export fn small(): Int {\n    return 9007199254740991\n}\n",
        "export fn big(): Int {\n    return 9007199254740993\n}\n",
        "export fn negative(): Int {\n    return -9007199254740993\n}\n",
        "export fn floating(): Float {\n    return 3.0\n}\n",
        "export fn word(): String {\n    return \"a\\\"b\"\n}\n",
        "export fn absent(): Int? {\n    return none\n}\n",
        "export fn numbers(): [Int] {\n    return [1, 9007199254740993]\n}\n",
        "export fn point(): Point {\n    return { x: 1, y: 9007199254740993 }\n}\n",
        "export fn names(): Map<Int, String> {\n    return [9007199254740993: \"big\", 2: \"two\"]\n}\n",
        "export fn tags(): Set<Int> {\n    return set([9007199254740993, 2])\n}\n",
        "export fn circle(): Shape {\n    return Shape.Circle(9007199254740993)\n}\n",
        "export fn dot(): Shape {\n    return Shape.Dot\n}\n",
        "export constant answer: Int = 9007199254740993\n",
    );

    /// With the JSON format, a result is the shared value encoding, an `Int`
    /// that a double cannot hold exactly is the tagged decimal string, and
    /// one it can is a plain number.
    #[test]
    fn a_json_result_keeps_every_int_exact() {
        let handle = load_source(SHAPES);
        assert_ne!(handle, 0, "{}", text());
        let _json = JsonResults::on();
        let expect = |name: &str, json: &str| {
            let (status, answered) = call_export(handle, name, "[]");
            assert_eq!(status, Status::Ok as i32, "{name}: {answered}");
            assert_eq!(answered, json, "{name}");
        };
        expect("nothing", "");
        expect("small", "9007199254740991");
        expect("big", r#"{"bigint":"9007199254740993"}"#);
        expect("negative", r#"{"bigint":"-9007199254740993"}"#);
        expect("floating", "3.0");
        expect("word", r#""a\"b""#);
        expect("absent", "null");
        expect("numbers", r#"{"list":[1,{"bigint":"9007199254740993"}]}"#);
        expect(
            "point",
            r#"{"record":{"x":1,"y":{"bigint":"9007199254740993"}}}"#,
        );
        expect(
            "names",
            r#"{"map":[[2,"two"],[{"bigint":"9007199254740993"},"big"]]}"#,
        );
        expect("tags", r#"{"set":[2,{"bigint":"9007199254740993"}]}"#);
        expect(
            "circle",
            r#"{"enum":{"name":"Shape","variant":"Circle","payload":[{"bigint":"9007199254740993"}]}}"#,
        );
        expect(
            "dot",
            r#"{"enum":{"name":"Shape","variant":"Dot","payload":[]}}"#,
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// The text format is what it always was, whatever the JSON format does.
    #[test]
    fn the_text_format_is_unchanged_and_the_default() {
        let handle = load_source(SHAPES);
        assert_ne!(handle, 0, "{}", text());
        let (status, answered) = call_export(handle, "big", "[]");
        assert_eq!(
            (status, answered.as_str()),
            (Status::Ok as i32, "Int(9007199254740993)")
        );
        let (_, answered) = call_export(handle, "numbers", "[]");
        assert_eq!(answered, "[Int(1), Int(9007199254740993)]");
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// `hs_wasm_read`, and the load result, follow the format too, and a
    /// format other than 0 and 1 is refused without changing the choice.
    #[test]
    fn constants_and_the_load_result_follow_the_format() {
        let json = JsonResults::on();
        let handle = load_source(
            "export constant answer: Int = 9007199254740993\nreturn 9007199254740993\n",
        );
        assert_ne!(handle, 0);
        assert_eq!(
            text(),
            r#"{"bigint":"9007199254740993"}"#,
            "the load result"
        );
        let name = upload(b"answer");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_read(handle, name.0, name.1) };
        // SAFETY: the same buffer, not yet freed.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(text(), r#"{"bigint":"9007199254740993"}"#);

        assert_eq!(hs_wasm_set_result_format(2), Status::Refused as i32);
        assert!(text().contains("must be 0"));
        let (_, answered) = call_export(handle, "missing", "[]");
        assert!(answered.contains("exports no function"), "{answered}");
        drop(json);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// A division by zero is a fault with a code, a class, a position and a
    /// span in bytes.
    #[test]
    fn a_fault_carries_its_code_and_position() {
        let handle = load_source(concat!(
            "fn inner(n: Int): Int {\n",
            "    return div(10, n)\n",
            "}\n",
            "export fn boom(n: Int): Int {\n",
            "    return inner(n)\n",
            "}\n",
        ));
        assert_ne!(handle, 0, "{}", text());
        let (status, message) = call_export(handle, "boom", "[0]");
        assert_eq!(status, Status::Faulted as i32);
        let json = fault();
        let parsed = hollowscript_vm::json::parse(&json).expect("the fault is JSON");
        let field = |name: &str| match &parsed {
            hollowscript_vm::json::Json::Object(fields) => fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone()),
            _ => None,
        };
        let json_str = |value: Option<hollowscript_vm::json::Json>| match value {
            Some(hollowscript_vm::json::Json::String(s)) => s,
            other => panic!("expected a string, got {other:?}"),
        };
        assert_eq!(json_str(field("code")), "HS1008", "{json}");
        assert_eq!(json_str(field("message")), message);
        assert_eq!(json_str(field("severity")), "error");
        assert_eq!(json_str(field("class")), "domain");
        assert_eq!(json_str(field("file")), "main.hws");
        assert!(
            matches!(field("line"), Some(hollowscript_vm::json::Json::Int(2))),
            "{json}"
        );
        assert!(
            matches!(field("column"), Some(hollowscript_vm::json::Json::Int(c)) if c > 0),
            "{json}"
        );
        assert!(matches!(
            field("data"),
            Some(hollowscript_vm::json::Json::Null)
        ));
        // The span is in bytes and covers the division on line 2.
        let start = match field("spanStartByte") {
            Some(hollowscript_vm::json::Json::Int(n)) => n,
            other => panic!("expected a byte offset, got {other:?}"),
        };
        let end = match field("spanEndByte") {
            Some(hollowscript_vm::json::Json::Int(n)) => n,
            other => panic!("expected a byte offset, got {other:?}"),
        };
        let source_line_two = i64::try_from("fn inner(n: Int): Int {\n".len()).unwrap();
        assert!(start >= source_line_two && end > start, "{json}");
        // The VM does not report call frames yet, so the array is there, as
        // the specification's record has it, and empty.
        assert!(
            matches!(field("frames"), Some(hollowscript_vm::json::Json::Array(frames)) if frames.is_empty()),
            "{json}"
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// Bytecode built without its source positions faults without a position,
    /// and says so with nulls rather than leaving the fields out.
    #[test]
    fn a_fault_in_stripped_bytecode_has_no_position() {
        let hollowscript::BuildOutcome::Bytecode(bytes) = hollowscript::build(
            "export fn boom(n: Int): Int {\n    return div(10, n)\n}\n",
            "main.hws",
            &hollowscript::Options::default(),
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) else {
            panic!("the fixture program should compile cleanly");
        };
        let stripped = hollowscript_bytecode::encode(
            &hollowscript_bytecode::verify::verify_bytes(&bytes)
                .unwrap()
                .module()
                .stripped(),
        );
        let (pointer, len) = upload(&stripped);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        let (status, _) = call_export(handle, "boom", "[0]");
        assert_eq!(status, Status::Faulted as i32);
        let json = fault();
        assert!(json.contains(r#""code":"HS1008""#), "{json}");
        assert!(
            json.contains(
                r#""file":null,"line":null,"column":null,"spanStartByte":null,"spanEndByte":null"#
            ),
            "{json}"
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// A limit fault carries its `data`, and a call that does not fault
    /// leaves nothing to hand over, so a stale fault is never read.
    #[test]
    fn a_limit_fault_has_data_and_a_later_call_forgets_it() {
        let handle = load_source(concat!(
            "export fn spin() {\n",
            "    while true {\n",
            "    }\n",
            "}\n",
            "export fn quick(): Int {\n",
            "    return 1\n",
            "}\n",
        ));
        assert_ne!(handle, 0, "{}", text());
        super::hs_wasm_set_limits(5_000, 0, 0, 0);
        let (status, _) = call_export(handle, "spin", "[]");
        assert_eq!(status, Status::Faulted as i32);
        let json = fault();
        assert!(json.contains(r#""code":"HS1001""#), "{json}");
        assert!(json.contains(r#""class":"resource""#), "{json}");
        assert!(
            json.contains(r#""data":{"option":"maxSteps","limit":5000,"used":"#),
            "{json}"
        );
        // Taking the fault consumes it.
        assert_eq!(fault(), "");

        let (status, _) = call_export(handle, "spin", "[]");
        assert_eq!(status, Status::Faulted as i32);
        let (status, answered) = call_export(handle, "quick", "[]");
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(answered, "Int(1)");
        assert_eq!(fault(), "", "a call that succeeded leaves no fault behind");

        let (status, _) = call_export(handle, "spin", "[]");
        assert_eq!(status, Status::Faulted as i32);
        let (status, _) = call_export(handle, "nothing_here", "[]");
        assert_eq!(status, Status::Refused as i32);
        assert_eq!(fault(), "", "a refused call is not a fault of the program");
        super::hs_wasm_set_limits(hollowscript_core::Limits::default().max_steps, 0, 0, 0);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// A fault while loading is structured the same way.
    #[test]
    fn a_fault_while_loading_is_structured_too() {
        let handle = load_source("return 1 / 0\n");
        assert_eq!(handle, 0);
        let message = text();
        assert!(message.contains("zero"), "{message}");
        let json = fault();
        assert!(json.contains(r#""code":"HS1008""#), "{json}");
        assert!(json.contains(r#""line":1"#), "{json}");
    }

    // ---- rendering a value for the host is bounded -------------------------

    /// A program that builds a value of about 2^50 paths out of fifty
    /// objects, by sharing, and hands it out three ways.
    const DOUBLING: &str = concat!(
        "enum Tree { Leaf, Node(Tree, Tree) }\n",
        "fn build(): Tree {\n",
        "    variable t = Tree.Leaf\n",
        "    for i in range(0, 50) { t = Tree.Node(t, t) }\n",
        "    return t\n",
        "}\n",
        "export fn tree(): Tree {\n    return build()\n}\n",
        "export fn small(): Int {\n    return 7\n}\n",
        "export constant held: Tree = build()\n",
    );

    /// Sets the limits a host would, restoring the defaults when dropped.
    struct Limited;

    impl Limited {
        fn with(steps: u64, memory: u64, output: u64) -> Limited {
            super::hs_wasm_set_limits(steps, memory, output, 0);
            Limited
        }
    }

    impl Drop for Limited {
        fn drop(&mut self) {
            let defaults = hollowscript_core::Limits::default();
            super::hs_wasm_set_limits(
                defaults.max_steps,
                defaults.max_memory_bytes,
                defaults.max_output_bytes,
                0,
            );
        }
    }

    /// The reported fault of a call that answered [`Status::Faulted`], parsed
    /// into what these tests check.
    fn assert_output_limit_fault(limit: u64) {
        let json = fault();
        assert!(json.contains(r#""code":"HS1004""#), "{json}");
        assert!(json.contains(r#""class":"resource""#), "{json}");
        assert!(
            json.contains(&format!(
                r#""data":{{"option":"maxOutputBytes","limit":{limit},"used":"#
            )),
            "{json}"
        );
    }

    /// The finding: with the JSON format a returned value was written out with
    /// no charge and no safepoint, so a value of 2^50 paths allocated until
    /// the module ran out of memory. Now the `call` result, the `read` of a
    /// constant and the load result each fault with HS1004 (a normal fault,
    /// not a trap), quickly, and the instance carries on.
    #[test]
    fn a_shared_structure_json_result_faults_with_the_output_limit() {
        let _limits = Limited::with(1_000_000_000, 32 * 1024 * 1024, 256 * 1024);
        let _json = JsonResults::on();
        let started = std::time::Instant::now();

        // `call`
        let handle = load_source(DOUBLING);
        assert_ne!(handle, 0, "{}", text());
        let (status, message) = call_export(handle, "tree", "[]");
        assert_eq!(status, Status::Faulted as i32, "{message}");
        assert!(message.contains("output limit"), "{message}");
        assert_output_limit_fault(256 * 1024);

        // The run finished before the value was written, so the instance is
        // exactly as after any call that returned.
        let (status, answered) = call_export(handle, "small", "[]");
        assert_eq!(status, Status::Ok as i32, "{answered}");
        assert_eq!(answered, "7");
        assert_eq!(fault(), "");

        // `read`
        let name = upload(b"held");
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe { hs_wasm_read(handle, name.0, name.1) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(name.0, name.1) };
        assert_eq!(status, Status::Faulted as i32, "{}", text());
        text();
        assert_output_limit_fault(256 * 1024);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);

        // The load result, the repro as it was reported.
        let handle = load_source(concat!(
            "enum Tree { Leaf, Node(Tree, Tree) }\n",
            "variable t = Tree.Leaf\n",
            "for i in range(0, 50) { t = Tree.Node(t, t) }\n",
            "return { t: t }\n",
        ));
        assert_eq!(handle, 0);
        assert!(text().contains("output limit"));
        assert_output_limit_fault(256 * 1024);

        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
    }

    /// The batched path writes its result the same bounded way.
    #[test]
    fn a_shared_structure_batched_result_faults_with_the_output_limit() {
        let _limits = Limited::with(1_000_000_000, 32 * 1024 * 1024, 256 * 1024);
        let _json = JsonResults::on();
        let hollowscript::BuildOutcome::Bytecode(bytes) = hollowscript::build(
            DOUBLING,
            "main.hws",
            &hollowscript::Options::default(),
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) else {
            panic!("the fixture program should compile cleanly");
        };
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load_batched(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        let name = upload(b"tree");
        let args = upload(b"[]");
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call_batched(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe { hs_wasm_free(name.0, name.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(args.0, args.1) };
        assert_eq!(status, Status::Faulted as i32, "{}", text());
        text();
        assert_output_limit_fault(256 * 1024);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// The text format has its own, older bound (16 MiB, with a marker) and is
    /// unchanged: the same value is cut off, not faulted.
    #[test]
    fn a_shared_structure_text_result_is_still_cut_off_by_the_renderer() {
        let handle = load_source(DOUBLING);
        assert_ne!(handle, 0, "{}", text());
        let (status, answered) = call_export(handle, "tree", "[]");
        assert_eq!(status, Status::Ok as i32);
        assert!(answered.ends_with("(truncated: rendered output exceeds the limit)"));
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// The host's cancellation reaches the writing of a result as it reaches
    /// a run: with the flag set the value is not written, HS1006.
    #[test]
    fn writing_a_json_result_stops_when_the_host_cancels() {
        // A large limit, so only the cancellation can stop the walk early.
        let _limits = Limited::with(1_000_000_000, 32 * 1024 * 1024, 1 << 40);
        let _json = JsonResults::on();
        let handle = load_source(DOUBLING);
        assert_ne!(handle, 0, "{}", text());
        // The run is a few hundred steps, so it reaches no safepoint (the
        // first is at 1,024 steps) and never asks the host; the writing of
        // its result asks every 1,024 pieces, so it is what sees the flag.
        let name = upload(b"tree");
        let args = upload(b"[]");
        host_stub::set_cancelled(true);
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call(handle, name.0, name.1, args.0, args.1) };
        host_stub::set_cancelled(false);
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe { hs_wasm_free(name.0, name.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(args.0, args.1) };
        assert_eq!(status, Status::Faulted as i32, "{}", text());
        text();
        assert!(fault().contains(r#""code":"HS1006""#));
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// Builds bytecode for a program that calls `sink` with one argument of
    /// the type `ty` builds, answering nothing.
    fn sink_bytes(
        source: &str,
        ty: impl FnOnce(
            &mut hollowscript_core::transfer::TypeTable,
        ) -> hollowscript_core::transfer::TypeRef,
    ) -> Vec<u8> {
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let param = ty(&mut types);
        let capabilities = hollowscript::CapabilityInterface {
            capabilities: vec![hollowscript::CapabilityDecl {
                name: "sink".to_owned(),
                shape: hollowscript::CapabilityShape::Function {
                    params: vec![param],
                    ret: None,
                },
            }],
            types,
        };
        let options = hollowscript::Options {
            capabilities,
            ..hollowscript::Options::default()
        };
        match hollowscript::build(
            source,
            "main.hws",
            &options,
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) {
            hollowscript::BuildOutcome::Bytecode(bytes) => bytes,
            hollowscript::BuildOutcome::CheckFailed(diagnostics) => {
                panic!("the fixture program should compile cleanly: {diagnostics:?}")
            }
        }
    }

    /// A capability argument of 125,000 copies of one three-element list, held
    /// as 52 objects. The charge for a capability call's arguments counts each
    /// shared object once, so it is tiny; the JSON handed to the host was
    /// hundreds of kilobytes before this was bounded, and unbounded for a
    /// deeper value.
    const SHARED_ARGUMENT: &str = concat!(
        "uses sink\n",
        "export fn run() {\n",
        "    constant a = [1, 2, 3]\n",
        "    constant row = range(0, 50).map(fn(i: Int): [Int] { return a })\n",
        "    constant grid = range(0, 50).map(fn(i: Int): [[Int]] { return row })\n",
        "    constant cube = range(0, 50).map(fn(i: Int): [[[Int]]] { return grid })\n",
        "    effect sink(cube)\n",
        "}\n",
    );

    fn cube_of_ints(
        types: &mut hollowscript_core::transfer::TypeTable,
    ) -> hollowscript_core::transfer::TypeRef {
        let int = types.add(TransferType::Int);
        let row = types.add(TransferType::List(int));
        let grid = types.add(TransferType::List(row));
        let cube = types.add(TransferType::List(grid));
        types.add(TransferType::List(cube))
    }

    /// The same finding through a capability argument: the call faults with
    /// HS1004, the host's function is never called with the oversized text,
    /// and the instance carries on.
    #[test]
    fn a_shared_structure_capability_argument_faults_with_the_output_limit() {
        let _limits = Limited::with(1_000_000_000, 32 * 1024 * 1024, 100_000);
        let bytes = sink_bytes(SHARED_ARGUMENT, cube_of_ints);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let seen = std::rc::Rc::clone(&calls);
        host_stub::set_capability(Some(Box::new(move |_index, _args| {
            seen.set(seen.get() + 1);
            (true, b"null".to_vec())
        })));
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        let started = std::time::Instant::now();
        let (status, message) = call_export(handle, "run", "[]");
        assert_eq!(status, Status::Faulted as i32, "{message}");
        assert_output_limit_fault(100_000);
        assert_eq!(calls.get(), 0, "the host was never handed the text");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));

        // Within the limit it is delivered, whole.
        super::hs_wasm_set_limits(0, 0, 1 << 30, 0);
        let (status, message) = call_export(handle, "run", "[]");
        assert_eq!(status, Status::Ok as i32, "{message}");
        assert_eq!(calls.get(), 1);
        host_stub::set_capability(None);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// A capability argument crosses with every `Int` exact: one above 2^53 - 1
    /// is the tagged `{"bigint": "<decimal>"}` that results use, so two
    /// distinct 64-bit `Int`s never collide for the host.
    #[test]
    fn a_capability_argument_keeps_every_int_exact() {
        let bytes = sink_bytes(
            concat!(
                "uses sink\n",
                "export fn run() {\n",
                "    effect sink([9007199254740993, 9223372036854775807, 9007199254740991])\n",
                "}\n",
            ),
            |types| {
                let int = types.add(TransferType::Int);
                types.add(TransferType::List(int))
            },
        );
        let sent = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let store = std::rc::Rc::clone(&sent);
        host_stub::set_capability(Some(Box::new(move |_index, args| {
            *store.borrow_mut() = String::from_utf8_lossy(args).into_owned();
            (true, b"null".to_vec())
        })));
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        let (status, message) = call_export(handle, "run", "[]");
        assert_eq!(status, Status::Ok as i32, "{message}");
        host_stub::set_capability(None);
        assert_eq!(
            sent.borrow().as_str(),
            concat!(
                r#"[{"list":[{"bigint":"9007199254740993"},"#,
                r#"{"bigint":"9223372036854775807"},9007199254740991]}]"#
            )
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// Runs `run` on the batched path to its first capability call and
    /// drains the commands, answering the status and the text.
    fn first_commands(
        source: &str,
        ty: impl FnOnce(
            &mut hollowscript_core::transfer::TypeTable,
        ) -> hollowscript_core::transfer::TypeRef,
    ) -> (i32, String) {
        let bytes = sink_bytes(source, ty);
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load_batched(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        let name = upload(b"run");
        let args = upload(b"[]");
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call_batched(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe { hs_wasm_free(name.0, name.1) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(args.0, args.1) };
        assert_eq!(status, Status::Suspended as i32, "{}", text());
        let status = hs_wasm_commands_json(handle);
        let commands = text();
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
        (status, commands)
    }

    /// The batched path bounds what it hands out as a command the same way.
    /// After the fault the run cannot be resumed (its commands were drained
    /// to be written), so the host disposes of the instance.
    #[test]
    fn batched_commands_are_bounded() {
        let _limits = Limited::with(1_000_000_000, 32 * 1024 * 1024, 100_000);
        let (status, commands) = first_commands(SHARED_ARGUMENT, cube_of_ints);
        assert_eq!(status, Status::Faulted as i32, "{commands}");
        assert_output_limit_fault(100_000);
    }

    /// A queued command writes an `Int` above 2^53 - 1 exactly.
    #[test]
    fn batched_commands_keep_every_int_exact() {
        let (status, commands) = first_commands(
            concat!(
                "uses sink\n",
                "export fn run() {\n",
                "    effect sink([9007199254740993, 9223372036854775807])\n",
                "}\n",
            ),
            |types| {
                let int = types.add(TransferType::Int);
                types.add(TransferType::List(int))
            },
        );
        assert_eq!(status, Status::Ok as i32, "{commands}");
        assert!(
            commands.contains(
                r#"{"list":[{"bigint":"9007199254740993"},{"bigint":"9223372036854775807"}]}"#
            ),
            "{commands}"
        );
    }

    // ---- the output limit and a host-bound `print` ------------------------

    /// A program with the standard `print`, which prints "hello" (6 bytes with
    /// its LF) `n` times, and the same with the one-byte-wide `"e"` and the
    /// two-byte `"\u{e9}"`.
    const PRINTER: &str = concat!(
        "uses print\n",
        "export fn shout(n: Int) {\n",
        "    for i in range(0, n) {\n",
        "        effect print(\"hello\")\n",
        "    }\n",
        "}\n",
        "export fn accent() {\n",
        "    effect print(\"\u{e9}\")\n",
        "}\n",
    );

    /// `PRINTER` compiled against the standard `print` (a `String` parameter,
    /// no result), the way a host that grants it does.
    fn printer_bytes() -> Vec<u8> {
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let string_ty = types.add(TransferType::String);
        let options = hollowscript::Options {
            capabilities: hollowscript::CapabilityInterface {
                capabilities: vec![hollowscript::CapabilityDecl {
                    name: "print".to_owned(),
                    shape: hollowscript::CapabilityShape::Function {
                        params: vec![string_ty],
                        ret: None,
                    },
                }],
                types,
            },
            ..hollowscript::Options::default()
        };
        match hollowscript::build(
            PRINTER,
            "main.hws",
            &options,
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) {
            hollowscript::BuildOutcome::Bytecode(bytes) => bytes,
            hollowscript::BuildOutcome::CheckFailed(diagnostics) => {
                panic!("the fixture program should compile cleanly: {diagnostics:?}")
            }
        }
    }

    /// Loads `PRINTER`, answering the handle.
    fn load_printer() -> u32 {
        let bytes = printer_bytes();
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        handle
    }

    /// Installs a `print` handler that counts the calls that reach it.
    fn count_print_calls() -> std::rc::Rc<std::cell::Cell<u32>> {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = std::rc::Rc::clone(&calls);
        host_stub::set_capability(Some(Box::new(move |_index, _args_json| {
            counted.set(counted.get() + 1);
            (true, b"null".to_vec())
        })));
        calls
    }

    /// Section 11.6 and 16.2: what a host-bound `print` prints counts against
    /// `maxOutputBytes`, which is checked before the host's function is called,
    /// so a call over the limit writes nothing. The run faults with HS1004.
    #[test]
    fn a_host_bound_print_charges_the_output_limit() {
        let handle = load_printer();
        assert_ne!(handle, 0, "{}", text());
        let calls = count_print_calls();

        // Two lines of 6 bytes fit in 12 exactly.
        super::hs_wasm_set_limits(0, 0, 12, 0);
        let (status, answered) = call_export(handle, "shout", "[2]");
        assert_eq!(status, Status::Ok as i32, "{answered}");
        assert_eq!(calls.get(), 2);

        // A third would be 18: the host is called twice, never a third time.
        calls.set(0);
        let (status, message) = call_export(handle, "shout", "[3]");
        assert_eq!(status, Status::Faulted as i32);
        assert!(message.contains("output limit"), "{message}");
        assert_eq!(calls.get(), 2, "the call over the limit writes nothing");
        let json = fault();
        assert!(json.contains(r#""code":"HS1004""#), "{json}");
        assert!(
            json.contains(r#""data":{"option":"maxOutputBytes","limit":12,"used":12}"#),
            "{json}"
        );

        // The counter is per run (section 11.2): the next call starts at 0.
        calls.set(0);
        let (status, _) = call_export(handle, "shout", "[2]");
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(calls.get(), 2);

        // It counts the bytes of the UTF-8 text, not its characters: "e" with
        // an acute accent is two bytes, and a line feed is one more.
        super::hs_wasm_set_limits(0, 0, 3, 0);
        calls.set(0);
        let (status, _) = call_export(handle, "accent", "[]");
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(calls.get(), 1);
        super::hs_wasm_set_limits(0, 0, 2, 0);
        calls.set(0);
        let (status, _) = call_export(handle, "accent", "[]");
        assert_eq!(status, Status::Faulted as i32);
        assert_eq!(calls.get(), 0);

        host_stub::set_capability(None);
        super::hs_wasm_set_limits(
            0,
            0,
            hollowscript_core::Limits::default().max_output_bytes,
            0,
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// With the default limit of 16 MiB a host-bound `print` is never in the
    /// way of an ordinary program.
    #[test]
    fn a_host_bound_print_is_unaffected_within_the_limit() {
        let handle = load_printer();
        assert_ne!(handle, 0, "{}", text());
        let calls = count_print_calls();
        let (status, _) = call_export(handle, "shout", "[100]");
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(calls.get(), 100);
        host_stub::set_capability(None);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// The batched path charges at the same point: the call over the limit is
    /// never queued, so the host is never told to write it.
    #[test]
    fn a_batched_print_charges_the_output_limit_before_it_is_queued() {
        let bytes = printer_bytes();
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load_batched(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        super::hs_wasm_set_limits(0, 0, 12, 0);

        let name = upload(b"shout");
        let args = upload(b"[3]");
        // SAFETY: both buffers are live and hold exactly these bytes.
        let mut status = unsafe { hs_wasm_call_batched(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        let mut queued = 0;
        while status == Status::Suspended as i32 {
            assert_eq!(hs_wasm_commands_json(handle), Status::Ok as i32);
            let commands = text();
            assert_eq!(commands.matches(r#""function":0"#).count(), 1, "{commands}");
            queued += 1;
            let answer = upload(b"[null]");
            // SAFETY: the buffer is live and holds exactly these bytes.
            status = unsafe { hs_wasm_resume_json(handle, answer.0, answer.1) };
            // SAFETY: the same buffer, not yet freed.
            unsafe {
                hs_wasm_free(answer.0, answer.1);
            }
        }
        assert_eq!(status, Status::Faulted as i32, "{}", text());
        assert_eq!(queued, 2, "two lines were queued, the third was refused");
        assert!(fault().contains(r#""code":"HS1004""#));
        super::hs_wasm_set_limits(
            0,
            0,
            hollowscript_core::Limits::default().max_output_bytes,
            0,
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// Every call of a capability named `print` is marked for charging, whatever
    /// shape it declares: the host binds handlers by name alone, so a `print`
    /// declared as taking an `Int`, or a list of strings, writes what it likes.
    /// A function that is not called `print` is not marked, and a member of a
    /// namespace that is called `print` is.
    #[test]
    fn every_call_of_a_capability_named_print_is_marked_for_charging() {
        assert_eq!(
            crate::print_calls(&module_with_capabilities()).unwrap(),
            vec![0],
            "print is the first of the calls, clock.now the second"
        );
        assert_eq!(crate::print_calls(&sample_with_print()).unwrap(), vec![0]);
        assert!(crate::print_calls(b"not bytecode").is_err());

        let signature = |m: &mut Module, ty: TransferType| Signature {
            params: vec![m.types.add(ty)],
            ret: None,
        };
        let finish = |mut m: Module| {
            let path = m.add_string("main.hws").unwrap();
            let init = m.add_string("main").unwrap();
            m.functions.push(Function {
                name: init,
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
            hollowscript_bytecode::encode(&m)
        };
        for shape in [
            TransferType::Int,
            TransferType::Float,
            TransferType::String,
            TransferType::Bool,
        ] {
            let mut m = Module::new();
            let name = m.add_string("print").unwrap();
            let sig = signature(&mut m, shape);
            m.capabilities.push(Capability {
                name,
                kind: CapabilityKind::Function(sig),
            });
            m.cap_calls.push(CapCall {
                capability: 0,
                function: 0,
            });
            assert_eq!(crate::print_calls(&finish(m)).unwrap(), vec![0]);
        }
        // A list of strings, which is the shape that got past the charge.
        let mut m = Module::new();
        let string_ty = m.types.add(TransferType::String);
        let list_ty = m.types.add(TransferType::List(string_ty));
        let name = m.add_string("print").unwrap();
        m.capabilities.push(Capability {
            name,
            kind: CapabilityKind::Function(Signature {
                params: vec![list_ty],
                ret: None,
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        assert_eq!(crate::print_calls(&finish(m)).unwrap(), vec![0]);
        // A namespace called `print`, and a function that is not.
        let mut m = Module::new();
        let print = m.add_string("print").unwrap();
        let line = m.add_string("line").unwrap();
        let sig = signature(&mut m, TransferType::String);
        m.capabilities.push(Capability {
            name: print,
            kind: CapabilityKind::Namespace(vec![(line, sig)]),
        });
        let other = m.add_string("log").unwrap();
        let sig = signature(&mut m, TransferType::String);
        m.capabilities.push(Capability {
            name: other,
            kind: CapabilityKind::Function(sig),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        m.cap_calls.push(CapCall {
            capability: 1,
            function: 0,
        });
        assert_eq!(crate::print_calls(&finish(m)).unwrap(), vec![0]);
    }

    /// A program that prints a list of strings `times` times, each line `count`
    /// elements of ten bytes, through a capability *declared* as taking
    /// `[String]`.
    const LIST_PRINTER: &str = concat!(
        "uses print\n",
        "export fn flood(times: Int, count: Int) {\n",
        "    constant row = range(0, count).map(fn(i: Int): String { return \"abcdefghij\" })\n",
        "    for i in range(0, times) {\n",
        "        effect print(row)\n",
        "    }\n",
        "}\n",
    );

    fn list_printer_bytes() -> Vec<u8> {
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let string_ty = types.add(TransferType::String);
        let list_ty = types.add(TransferType::List(string_ty));
        let options = hollowscript::Options {
            capabilities: hollowscript::CapabilityInterface {
                capabilities: vec![hollowscript::CapabilityDecl {
                    name: "print".to_owned(),
                    shape: hollowscript::CapabilityShape::Function {
                        params: vec![list_ty],
                        ret: None,
                    },
                }],
                types,
            },
            ..hollowscript::Options::default()
        };
        match hollowscript::build(
            LIST_PRINTER,
            "main.hws",
            &options,
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) {
            hollowscript::BuildOutcome::Bytecode(bytes) => bytes,
            hollowscript::BuildOutcome::CheckFailed(diagnostics) => {
                panic!("the fixture program should compile cleanly: {diagnostics:?}")
            }
        }
    }

    /// A `print` declared as taking a list of strings was not charged at all:
    /// 20,000 elements sent 200 times delivered 256 MiB through the host's
    /// handler under an output limit of 1,000. It is charged by the text its
    /// arguments carry now, before the host is called.
    #[test]
    fn a_print_declared_as_a_list_of_strings_is_charged_by_what_it_carries() {
        let bytes = list_printer_bytes();
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        let calls = count_print_calls();

        // The reproduction: 20,000 elements, 200 times, under a limit of 1,000.
        super::hs_wasm_set_limits(0, 0, 1_000, 0);
        let (status, message) = call_export(handle, "flood", "[200, 20000]");
        assert_eq!(status, Status::Faulted as i32, "{message}");
        assert_eq!(calls.get(), 0, "the first line is over the limit already");
        assert!(fault().contains(r#""code":"HS1004""#));

        // Three elements of ten bytes and a separator each, and a line feed:
        // 34 bytes a line, so two lines fit in 100 and the third does not.
        calls.set(0);
        super::hs_wasm_set_limits(0, 0, 100, 0);
        let (status, _) = call_export(handle, "flood", "[3, 3]");
        assert_eq!(status, Status::Faulted as i32);
        assert_eq!(calls.get(), 2);
        let json = fault();
        assert!(json.contains(r#""limit":100,"used":68"#), "{json}");

        // Within the limit it is never in the way.
        calls.set(0);
        super::hs_wasm_set_limits(0, 0, 1_000, 0);
        let (status, answered) = call_export(handle, "flood", "[5, 3]");
        assert_eq!(status, Status::Ok as i32, "{answered}");
        assert_eq!(calls.get(), 5);

        host_stub::set_capability(None);
        super::hs_wasm_set_limits(
            0,
            0,
            hollowscript_core::Limits::default().max_output_bytes,
            0,
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    // ---- arguments are accepted by section 9.6's rule --------------------

    const TYPED_EXPORTS: &str = concat!(
        "export fn takes_int(n: Int): Int {\n    return n\n}\n",
        "export fn takes_float(x: Float): Float {\n    return x\n}\n",
        "export fn takes_text(s: String): String {\n    return s\n}\n",
        "export fn takes_list(xs: [Int]): Int {\n    return xs.length()\n}\n",
        "export fn takes_map(m: Map<String, Int>): Int {\n    return m.length()\n}\n",
        "export fn takes_maybe(m: Int?): Int {\n    return 7\n}\n",
        "export fn takes_none() {\n}\n",
    );

    /// The same call as [`call_export`], on the batched path.
    fn call_export_batched(handle: u32, name: &str, args: &str) -> (i32, String) {
        let name = upload(name.as_bytes());
        let args = upload(args.as_bytes());
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe { hs_wasm_call_batched(handle, name.0, name.1, args.0, args.1) };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(name.0, name.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(args.0, args.1);
        }
        (status, text())
    }

    /// A string for an `Int`, a fractional or an enormous number for an `Int`,
    /// a set for a list, a map of strings for a `Map<String, Int>` and the rest
    /// of the wrong shapes were decoded by shape alone and reached the program,
    /// which answered wrongly or faulted as an internal error. Each is now the
    /// host's refused call, with the reason, and nothing has run.
    #[test]
    fn a_wrong_typed_argument_is_a_refused_call_and_not_a_fault() {
        let handle = load_source(TYPED_EXPORTS);
        assert_ne!(handle, 0, "{}", text());
        let refused = [
            (
                "takes_int",
                r#"["abc"]"#,
                "argument 1 is not a value of Int",
            ),
            ("takes_int", "[1.5]", "argument 1 is not a value of Int"),
            ("takes_int", "[1e300]", "argument 1 is not a value of Int"),
            ("takes_int", "[true]", "argument 1 is not a value of Int"),
            ("takes_int", "[null]", "argument 1 is not a value of Int"),
            (
                "takes_float",
                r#"["1.0"]"#,
                "argument 1 is not a value of Float",
            ),
            ("takes_text", "[1]", "argument 1 is not a value of String"),
            (
                "takes_list",
                r#"[{"set":[1,2]}]"#,
                "argument 1 is not a value of [Int]",
            ),
            (
                "takes_list",
                r#"[{"list":["a"]}]"#,
                "argument 1 is not a value of [Int]",
            ),
            ("takes_list", "[3]", "argument 1 is not a value of [Int]"),
            (
                "takes_map",
                r#"[{"map":[["a","x"]]}]"#,
                "argument 1 is not a value of Map<String, Int>",
            ),
            (
                "takes_map",
                r#"[{"map":[[1,2]]}]"#,
                "argument 1 is not a value of Map<String, Int>",
            ),
            (
                "takes_maybe",
                r#"["x"]"#,
                "argument 1 is not a value of Int?",
            ),
            ("takes_int", "[]", "expected 1 argument(s), found 0"),
            ("takes_int", "[1,2]", "expected 1 argument(s), found 2"),
            ("takes_none", "[1]", "expected 0 argument(s), found 1"),
        ];
        for (function, args, reason) in refused {
            for path in ["synchronous", "batched"] {
                let (status, answered) = if path == "synchronous" {
                    call_export(handle, function, args)
                } else {
                    call_export_batched(handle, function, args)
                };
                assert_eq!(status, Status::Refused as i32, "{path} {function} {args}");
                assert!(
                    answered.contains(reason),
                    "{path} {function} {args}: {answered}"
                );
                assert_eq!(fault(), "", "a refused call is not a fault of the program");
            }
        }
        // And what is accepted still is: `Int` for `Float` converts, an
        // absent optional is `none`, and the instance was never disturbed.
        assert_eq!(
            call_export(handle, "takes_int", "[41]"),
            (Status::Ok as i32, "Int(41)".to_owned())
        );
        assert_eq!(
            call_export(handle, "takes_float", "[2]"),
            (Status::Ok as i32, "Float(2.0)".to_owned())
        );
        assert_eq!(
            call_export(handle, "takes_float", "[2.5]"),
            (Status::Ok as i32, "Float(2.5)".to_owned())
        );
        assert_eq!(
            call_export(handle, "takes_list", r#"[{"list":[1,2,3]}]"#),
            (Status::Ok as i32, "Int(3)".to_owned())
        );
        assert_eq!(
            call_export(handle, "takes_map", r#"[{"map":[["a",1]]}]"#),
            (Status::Ok as i32, "Int(1)".to_owned())
        );
        assert_eq!(
            call_export(handle, "takes_maybe", "[null]"),
            (Status::Ok as i32, "Int(7)".to_owned())
        );
        assert_eq!(
            call_export(handle, "takes_maybe", "[5]"),
            (Status::Ok as i32, "Int(7)".to_owned())
        );
        assert_eq!(
            call_export(handle, "takes_none", "[]"),
            (Status::Ok as i32, String::new())
        );
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }

    /// What a host answers a suspended capability call with is held to the
    /// type the capability declared too (section 9.6, HS1013), so a resume
    /// cannot put a value of another type into the program either.
    #[test]
    fn a_batched_resume_answer_of_the_wrong_type_is_hs1013() {
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let int_ty = types.add(TransferType::Int);
        let capabilities = hollowscript::CapabilityInterface {
            capabilities: vec![hollowscript::CapabilityDecl {
                name: "double".to_owned(),
                shape: hollowscript::CapabilityShape::Function {
                    params: vec![int_ty],
                    ret: Some(int_ty),
                },
            }],
            types,
        };
        let options = hollowscript::Options {
            capabilities,
            ..hollowscript::Options::default()
        };
        let source = "uses double\nexport fn run(): Int {\n    return effect double(21)\n}\n";
        let hollowscript::BuildOutcome::Bytecode(bytes) = hollowscript::build(
            source,
            "main.hws",
            &options,
            &mut hollowscript::NoLoader,
            &mut hollowscript::Control::none(),
        ) else {
            panic!("the fixture program should compile cleanly");
        };
        for answer in [r#"["forty-two"]"#, "[1.5]", "[true]", r#"[{"list":[1]}]"#] {
            let (pointer, len) = upload(&bytes);
            // SAFETY: the buffer is live and holds exactly these bytes.
            let handle = unsafe { hs_wasm_load_batched(pointer, len) };
            // SAFETY: the same buffer, not yet freed.
            unsafe { hs_wasm_free(pointer, len) };
            assert_ne!(handle, 0, "{}", text());
            assert_eq!(
                call_export_batched(handle, "run", "[]").0,
                Status::Suspended as i32
            );
            assert_eq!(hs_wasm_commands_json(handle), Status::Ok as i32);
            let _ = text();
            let results = upload(answer.as_bytes());
            // SAFETY: the buffer is live and holds exactly these bytes.
            let status = unsafe { hs_wasm_resume_json(handle, results.0, results.1) };
            // SAFETY: the same buffer, not yet freed.
            unsafe {
                hs_wasm_free(results.0, results.1);
            }
            assert_eq!(status, Status::Faulted as i32, "{answer}: {}", text());
            assert!(fault().contains(r#""code":"HS1013""#), "{answer}");
            assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
        }
    }

    // ---- capability names are identifiers and are escaped -----------------

    /// A module whose one capability, `print`, is called by name `name`.
    fn module_with_capability_named(name: &str) -> Vec<u8> {
        let mut m = Module::new();
        let string_ty = m.types.add(TransferType::String);
        let capability = m.add_string(name).unwrap();
        m.capabilities.push(Capability {
            name: capability,
            kind: CapabilityKind::Function(Signature {
                params: vec![string_ty],
                ret: None,
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let path = m.add_string("main.hws").unwrap();
        let init = m.add_string("main").unwrap();
        m.functions.push(Function {
            name: init,
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
        hollowscript_bytecode::encode(&m)
    }

    /// A name such as `print","clock.now` used to be written into the JSON
    /// array unescaped, which turned one name into two and shifted the
    /// handler of every capability after it; one with a backslash or a line
    /// feed made `JSON.parse` throw. The verifier now refuses such a name, so
    /// neither the names nor the load ever see one.
    #[test]
    fn a_capability_name_that_is_not_an_identifier_is_refused() {
        for name in [
            "print\",\"clock.now",
            "back\\slash",
            "line\nfeed",
            "with space",
            "dotted.name",
            "9starts_with_a_digit",
            "",
            "caf\u{e9}",
        ] {
            let bytes = module_with_capability_named(name);
            let (pointer, len) = upload(&bytes);
            // SAFETY: the buffer is live and holds exactly these bytes.
            let status = unsafe { hs_wasm_capability_names(pointer, len) };
            assert_eq!(status, Status::Refused as i32, "{name:?}");
            assert!(text().contains("capability names"), "{name:?}");
            // SAFETY: the same buffer, still live.
            let handle = unsafe { hs_wasm_load(pointer, len) };
            // SAFETY: the same buffer, not yet freed.
            unsafe { hs_wasm_free(pointer, len) };
            assert_eq!(handle, 0, "{name:?}");
            let _ = text();
        }
        // And an identifier is not.
        let bytes = module_with_capability_named("_ok_Name_9");
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe { hs_wasm_capability_names(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(text(), r#"["_ok_Name_9"]"#);
    }

    /// Whatever reaches the JSON writer is escaped, so a host's parse never
    /// sees more entries than there are names.
    #[test]
    fn the_names_are_escaped_into_json() {
        let json = super::names_json(&[
            "print\",\"clock.now".to_owned(),
            "a\\b".to_owned(),
            "line\nfeed".to_owned(),
            "bell\u{7}".to_owned(),
        ]);
        assert_eq!(
            json,
            r#"["print\",\"clock.now","a\\b","line\nfeed","bell\u0007"]"#
        );
        assert_eq!(super::names_json(&[]), "[]");
    }

    // ---- the table of instances reuses positions and refuses stale handles --

    fn loaded_sample() -> u32 {
        let bytes = sample();
        let (pointer, len) = upload(&bytes);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let handle = unsafe { hs_wasm_load(pointer, len) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_ne!(handle, 0, "{}", text());
        handle
    }

    fn table_size() -> (usize, usize) {
        super::INSTANCES.with_borrow(|instances| (instances.entries.len(), instances.free.len()))
    }

    /// A page that loads and disposes in a loop used to add a position to the
    /// table for every load (about 1.5 KiB each, 144 MiB in 100,000 cycles).
    /// A disposed position is reused, so the table stays as large as the most
    /// instances that were ever live at once.
    #[test]
    fn a_load_and_dispose_loop_does_not_grow_the_table() {
        let (before, _) = table_size();
        let first = loaded_sample();
        assert_eq!(hs_wasm_dispose(first), Status::Ok as i32);
        let (after_one, free_after_one) = table_size();
        assert!(after_one <= before + 1);
        for _ in 0..20_000 {
            let handle = loaded_sample();
            assert_eq!(call_export(handle, "twice", "[2]").1, "Int(4)");
            assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
        }
        assert_eq!(table_size(), (after_one, free_after_one));
    }

    /// The position is reused, and the handle to what used to be in it is
    /// still refused, still cannot dispose what is in it now, and the new
    /// handle is a different number.
    #[test]
    fn a_stale_handle_is_refused_after_its_position_is_reused() {
        let old = loaded_sample();
        assert_eq!(call_export(old, "twice", "[1]").1, "Int(2)");
        assert_eq!(hs_wasm_dispose(old), Status::Ok as i32);
        let new = loaded_sample();
        assert_ne!(old, new, "a reused position has a new generation");
        assert_eq!(old & 0xFFFF, new & 0xFFFF, "the position is the same");
        assert_eq!(call_export(new, "twice", "[3]").1, "Int(6)");
        // The old handle is refused by every export that takes one...
        let (status, _) = call_export(old, "twice", "[1]");
        assert_eq!(status, Status::Refused as i32);
        let name = upload(b"answer");
        // SAFETY: the buffer is live.
        let status = unsafe { hs_wasm_read(old, name.0, name.1) };
        assert_eq!(status, Status::Refused as i32);
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(name.0, name.1) };
        // ...and disposing it again does not dispose the new instance.
        assert_eq!(hs_wasm_dispose(old), Status::Ok as i32);
        assert_eq!(call_export(new, "twice", "[4]").1, "Int(8)");
        assert_eq!(hs_wasm_dispose(new), Status::Ok as i32);
        // A handle that was never given out is refused too: no generation, a
        // position past the table, and zero.
        for handle in [0, 1 << 16, 0xFFFF, 0xFFFF_FFFF, 9_999] {
            let (status, _) = call_export(handle, "twice", "[1]");
            assert_eq!(status, Status::Refused as i32, "{handle:#x}");
        }
    }

    /// A handle that disposes its own instance from inside a call on it
    /// (a capability handler doing so) is honoured, and the position is free
    /// for the next load without the finished call putting anything back.
    #[test]
    fn a_position_disposed_during_its_own_call_is_not_resurrected() {
        use super::{Slot, with_instance};
        let handle = loaded_sample();
        let status = with_instance(handle, |_| hs_wasm_dispose(handle));
        assert_eq!(status, Status::Ok as i32);
        let (status, _) = call_export(handle, "twice", "[1]");
        assert_eq!(status, Status::Refused as i32);
        let next = loaded_sample();
        assert_ne!(next, handle);
        super::INSTANCES.with_borrow(|instances| {
            assert!(
                instances
                    .entries
                    .iter()
                    .any(|e| matches!(e.slot, Slot::Occupied(_)))
            );
        });
        assert_eq!(call_export(next, "twice", "[5]").1, "Int(10)");
        assert_eq!(hs_wasm_dispose(next), Status::Ok as i32);
    }

    /// A position whose generation cannot move on is retired, not wrapped, so
    /// no handle ever names a second instance.
    #[test]
    fn a_position_that_has_used_every_generation_is_retired() {
        use super::{Entry, Instances, MAX_GENERATION, Slot};
        let mut table = Instances {
            entries: vec![Entry {
                generation: MAX_GENERATION - 1,
                slot: Slot::Empty,
            }],
            free: vec![0],
        };
        let module = |bytes: &[u8]| {
            crate::WasmModule::load(bytes, hollowscript_vm::CapabilityTable::new(Vec::new()))
                .unwrap()
        };
        let bytes = sample();
        let second_last = table.install(module(&bytes)).unwrap();
        assert_eq!(second_last >> 16, u32::from(MAX_GENERATION - 1));
        table.release(second_last);
        let last = table.install(module(&bytes)).unwrap();
        assert_eq!(last >> 16, u32::from(MAX_GENERATION));
        table.release(last);
        assert!(table.free.is_empty(), "the position is retired");
        assert!(table.entry_mut(last).is_some(), "an empty entry remains");
        assert!(matches!(table.entries[0].slot, Slot::Empty));
        // Neither handle is good any more, and a new instance takes a new position.
        let fresh = table.install(module(&bytes)).unwrap();
        assert_eq!(fresh & 0xFFFF, 2);
        assert!(table.entry_mut(second_last).is_none());
        assert!(matches!(
            table.entry_mut(last).map(|e| &e.slot),
            Some(Slot::Empty)
        ));
    }

    /// There is a limit to how many are live at once, and past it a load is
    /// refused with the reason rather than a handle that names nothing.
    #[test]
    fn a_full_table_refuses_a_load() {
        use super::{Entry, Instances, MAX_INSTANCES, Slot};
        let mut table = Instances {
            entries: (0..MAX_INSTANCES)
                .map(|_| Entry {
                    generation: 1,
                    slot: Slot::Busy,
                })
                .collect(),
            free: Vec::new(),
        };
        let bytes = sample();
        let module =
            crate::WasmModule::load(&bytes, hollowscript_vm::CapabilityTable::new(Vec::new()))
                .unwrap();
        assert_eq!(table.install(module), None);
        // The last position's handle is the largest a handle can be.
        assert_eq!(Instances::handle(MAX_INSTANCES - 1, 1), (1 << 16) | 0xFFFF);
        assert_eq!(
            Instances::split((1 << 16) | 0xFFFF),
            Some((MAX_INSTANCES - 1, 1))
        );
        assert_eq!(Instances::split(0), None);
        assert_eq!(Instances::split(1 << 16), None, "position 0 is no position");
        assert_eq!(Instances::split(1), None, "generation 0 is no generation");
    }

    // ---- limits are validated -------------------------------------------

    /// What a negative number becomes when a host passes it as an unsigned
    /// 64-bit integer, and every value above 2^63 - 1, is refused with the
    /// reason, changes none of the limits (even one that was valid in the same
    /// call), and the run that follows is held to the limits it had.
    #[test]
    fn a_limit_outside_the_range_is_refused_and_changes_nothing() {
        let before = super::LIMITS.with_borrow(Clone::clone);
        for bad in [u64::MAX, 1 << 63, i64::MAX as u64 + 1] {
            for position in 0..4 {
                let mut args = [0u64; 4];
                args[position] = bad;
                // A valid value alongside the bad one: all or nothing.
                args[(position + 1) % 4] = 7;
                let status = super::hs_wasm_set_limits(args[0], args[1], args[2], args[3]);
                assert_eq!(status, Status::Refused as i32, "{args:?}");
                let reason = text();
                assert!(
                    reason.contains("must be from 1 to 9223372036854775807"),
                    "{reason}"
                );
                assert_eq!(super::LIMITS.with_borrow(Clone::clone), before);
            }
        }
        // The largest valid value is accepted, and zero leaves a limit alone.
        let status = super::hs_wasm_set_limits(i64::MAX as u64, 0, 0, 0);
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(
            super::LIMITS.with_borrow(|limits| limits.max_steps),
            i64::MAX as u64
        );
        assert_eq!(
            super::LIMITS.with_borrow(|limits| limits.max_memory_bytes),
            before.max_memory_bytes
        );
        super::LIMITS.with_borrow_mut(|limits| *limits = before);
    }

    // ---- Int and Float on the wire ---------------------------------------

    /// In the JSON result format a `Float` is always written with a decimal
    /// point or an exponent and an `Int` never is, so the two are different on
    /// the wire, and the value of a `Float` is exactly the shortest text that
    /// reads back as the same double. (A JavaScript `JSON.parse` reads both as
    /// a `Number`, which `web/README.md` says; this is about the text.)
    #[test]
    fn a_float_is_written_with_a_point_or_an_exponent_and_an_int_never_is() {
        let handle = load_source(concat!(
            "export fn floats(): [Float] {\n",
            "    return [3.0, 9007199254740993.0, 1.5, 1e21, 0.1, 5e-324, 1e16, 123456789012345680000.0]\n",
            "}\n",
            "export fn ints(): [Int] {\n",
            "    return [3, 0, -7, 9007199254740991, 9007199254740993]\n",
            "}\n",
            "export fn one(): { a: Int, b: Float, c: Float? } {\n",
            "    return { a: 3, b: 3.0, c: 2.0 }\n",
            "}\n",
        ));
        assert_ne!(handle, 0, "{}", text());
        let _json = JsonResults::on();
        let (status, floats) = call_export(handle, "floats", "[]");
        assert_eq!(status, Status::Ok as i32);
        assert_eq!(
            floats,
            r#"{"list":[3.0,9007199254740992.0,1.5,1e21,0.1,5e-324,1e16,1.2345678901234568e20]}"#
        );
        let (_, ints) = call_export(handle, "ints", "[]");
        assert_eq!(
            ints,
            r#"{"list":[3,0,-7,9007199254740991,{"bigint":"9007199254740993"}]}"#
        );
        let (_, record) = call_export(handle, "one", "[]");
        assert_eq!(record, r#"{"record":{"a":3,"b":3.0,"c":2.0}}"#);
        assert_eq!(hs_wasm_dispose(handle), Status::Ok as i32);
    }
}
