//! A small, stable C ABI over opaque handles.
//!
//! This is one of the two crates in the workspace allowed `unsafe`, because a
//! C caller can only be met at a raw pointer boundary; every other crate is
//! ordinary safe Rust. The rule this crate holds itself to: every `unsafe`
//! block does exactly one thing (dereference a pointer the caller gave us, or
//! hand one back), is preceded by a `// SAFETY:` comment that states the
//! precondition, and the precondition is checked in safe code immediately
//! before the block wherever that is possible (a null check, a UTF-8 check).
//!
//! **Surface today.** [`hs_version`], JSON diagnostics from
//! [`hs_check_json`], JSON formatting from [`hs_format_json`], and the
//! instance lifecycle of chapter 12: [`hs_load`], [`hs_call`], [`hs_read`]
//! and [`hs_dispose`]. An instance is an opaque handle a host holds; every
//! function that acts on one takes it and reports through the same status
//! codes. [`hs_load_ex`] and [`hs_call_ex`] add, additively, a host
//! capability interface (chapter 9, via [`HsCapabilityFn`], including a
//! namespace capability such as `clock` and a capability declared
//! `Result<T, E>` such as `fs.read`, see [`HsCapabilityFn`]'s own
//! documentation) and a cancellation token (section 11.7, via
//! [`HsCancelToken`] and [`hs_cancel_token_new`]); [`hs_load`] and
//! [`hs_call`] remain exactly as they were, as the case of no capabilities
//! and no cancellation.
//!
//! **Values across the boundary.** A C caller has no way to hold a
//! `Value`, which is a handle into one instance's heap, so every value
//! chapter 12 transfers across this ABI, in either direction, crosses as
//! JSON: [`hs_call`]'s own arguments (a JSON array) and its rendered result
//! (`corpus/FORMAT.md`'s own text, `Int(3)`, `String("a")`, not JSON, since
//! that is [`hs_read`]'s and [`hs_call`]'s existing, unchanged contract), and
//! a host capability function's arguments and answer, bound through
//! [`hs_load_ex`], which are JSON in both directions. This is the same
//! encoding `hollowscript-wasm`'s own JS boundary uses, so a value crosses
//! either ABI the same way:
//!
//! | HollowScript | JSON |
//! |---|---|
//! | `Int`, `Float`, `Bool` | a JSON number or `true`/`false` (a `Float` always keeps a decimal point, `3.0` not `3`, so it is never read back as an `Int`) |
//! | `String` | a JSON string |
//! | `none` (an absent optional) | `null` |
//! | `[T]` | `{"list": [...]}` |
//! | `Set<T>` | `{"set": [...]}` |
//! | `Map<K, V>` | `{"map": [[k, v], ...]}`, an array of two-element `[key, value]` arrays, not a JSON object, because a key is not always a JSON-object-safe string (it may be `Int` or `Bool`) |
//! | a record | `{"record": {"field": value, ...}}` |
//! | an enum value | `{"enum": {"name": "E", "variant": "V", "payload": [...]}}` |
//! | `Result<T, E>` | sugar for the `Result` enum specifically: `{"Ok": value}` or `{"Err": value}` |
//! | `Filter` | `{"filter": "<source text>"}` (Appendix A.1), one direction only: a program builds a `Filter` only from its own literal, never from a value a host hands back, so this tag is never read inbound, only ever written outbound to a capability argument |
//!
//! A record or enum value crosses only when its shape or variant is one the
//! module already has, either because a capability's own declared type
//! mentions it or because the program's own source does (including only in
//! an exported function's signature); `hollowscript`'s own internal
//! `hosttypes` module registers both, and this crate relies on it doing so.
//! A host capability's *declared* parameter and result types are the same
//! grammar, one level up: [`HsCapabilityFn::params`]/`result`/`result_err`
//! remain the four [`HsValueType`] scalars plus
//! [`Filter`](HsValueType::Filter), unchanged and additive, but
//! [`HsCapabilityFn::params_json`] and `result_json` (see their own
//! documentation) declare a parameter or result of any transferable type at
//! all, a record, a list, a map, a set, an enum or `Result<T, E>` with a
//! non-scalar `ok`/`err`, exactly the grammar `hollowscript-wasm`'s own
//! `tooling::declared_type` uses for the WASM boundary's own capability
//! declarations, so one capability interface, written once as JSON, reads
//! the same at either boundary.
//!
//! **A bare JSON number is decoded against the declared type, not its own
//! spelling.** JSON cannot tell `3` from `3.0` (a number with no `.` and no
//! exponent is `Int`, whichever the sender meant), and a JavaScript host
//! cannot write one that can: `JSON.stringify(3.0)` is already `"3"` by the
//! time this ABI ever sees it. So a JSON number crossing inbound, an
//! `hs_call` argument, a capability's own argument (rendered outbound, so
//! this does not apply there) or its own answer, decodes to the `Float`
//! its position's declared type calls for even when it looks integral,
//! walking into a list element, a map's own value, a record field, an
//! enum's or `Result`'s own payload and an optional's own inner type the
//! same way; a number at a declared `Int` position, an out-of-range one, or
//! one that is not integral where `Int` is declared, is still refused, by
//! the same acceptance rule as before (section 9.6). This is the
//! encoding's own definition of which language value a JSON number
//! denotes, given the declared type; it is a different question from
//! section 9.6's "nothing is converted inside a collection, a record or an
//! enum payload", which is about a language *value* the boundary has
//! already decided on, not about deciding what one JSON number means in
//! the first place.
//!
//! **Cancellation and re-entrancy.** A [`HsCancelToken`] may be set from any
//! thread while [`hs_load_ex`] or [`hs_call_ex`] is running; that is the one
//! concurrent use this crate allows (section 12.5). A capability callback,
//! by contrast, must never call back into the instance it was invoked for,
//! whether directly or by calling [`hs_call`]/[`hs_call_ex`] on the same
//! handle from inside the callback: the instance registry's `busy` flag
//! refuses that as [`HsStatus::Refused`] rather than aliasing the instance,
//! but a host should not rely on the refusal and
//! should treat re-entering the same instance from a capability as a
//! programming error. See `docs/design/security.md` for the fuller threat
//! model this boundary is reviewed against.
//!
//! **Unwinding across a capability callback.** `guard` and `guard_with`
//! stop a *Rust* panic at every extern "C" entry point of this crate, which
//! is what makes [`HsStatus::Panicked`] possible at all; they cannot do
//! anything about the reverse direction, a host's own capability callback
//! (`HsCapabilityFn::call`) unwinding out through this crate's own Rust
//! frames back into the interpreter that called it. A callback that
//! `longjmp`s past its own return, or (in a C++ host) lets an exception
//! escape it, does not run this crate's destructors on the way out: `InUse`'s
//! own `Drop`, which is what clears the instance registry's `busy` flag (and
//! what `hs_dispose`'s own `dispose_pending` relies on to actually free the
//! instance once a call currently using it finishes), never runs. The
//! instance is left busy and every later call on it is refused, permanently:
//! it can be neither called again nor actually disposed, only leaked for
//! the rest of the process. This is inherent to crossing an `extern "C"`
//! boundary in either language, not a defect this crate can catch the way
//! it catches its own panics, so a host must not let a callback do either:
//! it should return a normal failure (a non-zero result) instead, exactly
//! as its own contract already asks.
//!
//! **Stability.** Every exported function's name, signature and behaviour is
//! part of the C ABI and, once this reaches 1.0, is stable within a major
//! version (the Rust API of `hollowscript` is not, and may change in a minor
//! version). No function here may be removed or have its signature changed
//! without a major version; a replacement is added alongside the old one instead.
//!
//! **Ownership.** A string this crate hands back (`char*`) is owned by the
//! caller and must be freed with [`hs_string_free`], never with the host's
//! own `free`: it was allocated by Rust's allocator, which is not guaranteed
//! to be the same allocator as the host's C runtime.

#![allow(
    unsafe_code,
    reason = "a C ABI can only be met at a raw pointer boundary"
)]

use hollowscript::{
    CallOutcome, CancellationToken, CapabilityDecl, CapabilityInterface, CapabilityShape, Control,
    ERR, Fault, FaultCode, HostCall, HostFn, Instance, LoadOutcome, NoLoader, OK, Options, RESULT,
    Value, check, load,
};
use hollowscript_bytecode::module::ExportKind;
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable, VariantDef};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt::Write as _;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Status codes every function below returns.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HsStatus {
    /// The call succeeded.
    Ok = 0,
    /// A pointer argument that must not be null was null.
    NullArgument = 1,
    /// A `char*` argument was not valid UTF-8.
    InvalidUtf8 = 2,
    /// The program has an error, so no instance was created. The
    /// diagnostics are written to the caller's JSON output.
    CheckFailed = 3,
    /// The instance is not in a state that allows this call, or the name is
    /// not an export of it. The reason is written to the caller's output.
    Refused = 4,
    /// The call ran and faulted. The fault is written to the caller's output.
    Faulted = 5,
    /// The implementation panicked. This is a defect of the library, never
    /// of the program: nothing a script can do should reach it, and the
    /// fuzz harness exists to keep it that way. It is a status rather than
    /// an abort because a panic unwinding out of an `extern "C"` function
    /// is undefined behaviour, so the boundary stops it. The instance, if
    /// there was one, is disposed before this is returned, so every later
    /// call on that handle is refused rather than reaching state a panic
    /// left half-changed.
    Panicked = 6,
    /// The instance handle is not one [`hs_load`] gave out, or
    /// [`hs_dispose`] has already released it. Nothing was read through it:
    /// a handle is an identifier the library looks up, never a pointer it
    /// follows, so a stale or made-up one is refused rather than reaching
    /// freed memory.
    UnknownHandle = 7,
    /// An [`HsCapabilityFn`]'s `params_json` or `result_json` was non-null
    /// and non-empty but was not valid JSON, or not one of the type
    /// descriptions this crate's own module documentation defines (a
    /// malformed capability declaration, at the point [`hs_load_ex`] reads
    /// it, distinct from [`HsStatus::CheckFailed`], which is a malformed
    /// *program*). The reason is written to the caller's output.
    InvalidCapability = 8,
    /// An [`HsLimits`] passed to [`hs_load_ex`] held a value outside 1 to
    /// 2^63 - 1 (section 11.1: a limit cannot be switched off, and 0 is not
    /// a value). The reason is written to the caller's output.
    InvalidLimits = 9,
}

/// Runs `body` and stops a panic at the boundary.
///
/// `panic = "unwind"` is set for every profile precisely so that this can
/// exist: a panic unwinding out of an `extern "C"` function is undefined
/// behaviour, so the C ABI catches one and answers
/// [`HsStatus::Panicked`]. Nothing a script can do should reach it; the
/// pipeline fuzz harness in `hollowscript` is what keeps that true.
fn guard(body: impl FnOnce() -> c_int) -> c_int {
    // The closure holds raw pointers and `&mut`, neither of which is
    // `UnwindSafe`, and cannot be: a panic part way through a call could
    // leave an instance half-changed. That is exactly why every caller of
    // this that holds an instance disposes it before returning, so nothing
    // can be observed afterwards.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or(HsStatus::Panicked as c_int)
}

/// The same, for an entry point that acts on an instance. A panic part way
/// through a call could leave that instance half-changed, so `on_panic`
/// ends it before the status goes back and every later call on the handle
/// is refused. `on_panic` is itself guarded, so a second panic cannot
/// escape either.
fn guard_with(body: impl FnOnce() -> c_int, on_panic: impl FnOnce()) -> c_int {
    let Ok(status) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) else {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(on_panic));
        return HsStatus::Panicked as c_int;
    };
    status
}

/// An opaque instance handle (chapter 12). A host holds this between calls
/// and releases it with [`hs_dispose`].
///
/// The type is deliberately not `#[repr(C)]` and has no accessible fields: a
/// C caller only ever holds a pointer to one. That pointer is an identifier,
/// not an address: the library looks it up in its own table on every call and
/// never dereferences it, so a handle used after [`hs_dispose`], disposed
/// twice or never given out at all is [`HsStatus::UnknownHandle`] rather
/// than undefined behaviour.
///
/// **Identifier width.** `NEXT_HANDLE` is a [`usize`]-wide counter, so it
/// is 64 bits wide on every target this library ships for today, and on
/// such a target it cannot wrap within a process's real lifetime: a stale
/// handle can never coincide with a later instance's. On a hypothetical
/// 32-bit target it is only 32 bits wide, and `register`'s own loop over
/// `LIVE` only rules out colliding with a handle that is *live right
/// now*, not with one a host still holds stale (freed, or never disposed
/// but long forgotten): once the counter has wrapped past 2^32 loads in one
/// process's life, a stale handle from early in that process could then
/// coincide with a handle a much later `hs_load`/`hs_load_ex` hands out.
/// This is a real, narrow gap on such a target (this crate does not widen
/// the handle to carry a separate generation the way the VM's own internal
/// heap handles do, `hollowscript_vm`'s `Handle { index, generation }`,
/// because doing so changes what a handle *is* across this stable ABI); a
/// host embedding on a 32-bit target and expecting to load more than
/// roughly four billion times over one process's life should restart the
/// process well before that, the same hygiene a host already needs for any
/// other `usize`-counted resource on such a target.
pub struct HsInstance {
    instance: Box<Instance>,
}

/// The instances [`hs_load`] has given out and [`hs_dispose`] has not yet
/// released, by handle identifier.
static LIVE: Mutex<BTreeMap<usize, Live>> = Mutex::new(BTreeMap::new());

/// The next handle identifier. Zero is never used, so a null handle is
/// never a live one.
static NEXT_HANDLE: AtomicUsize = AtomicUsize::new(1);

/// One live instance.
struct Live {
    /// The instance, from `Box::leak`, freed only by [`release`].
    instance: Owned,
    /// Whether a call is using the instance now. A second call on the same
    /// handle while this is set is refused rather than aliasing it.
    busy: bool,
    /// Whether [`hs_dispose`] was called while a call was using the
    /// instance, in which case that call frees it when it finishes.
    dispose_pending: bool,
}

/// An instance pointer [`LIVE`] owns.
struct Owned(NonNull<HsInstance>);

// SAFETY: `LIVE` only stores and hands back the pointer; it never reads
// through it. The instance is only ever reached by the one call that holds
// its `busy` flag, which is what the caller's contract of using an
// instance from one thread at a time already requires, so storing the
// pointer in a shared table adds no access an unshared one would not have.
unsafe impl Send for Owned {}

fn live() -> MutexGuard<'static, BTreeMap<usize, Live>> {
    // Nothing below runs the interpreter while holding this lock, so a
    // panic cannot leave the table half-changed and a poisoned lock is
    // still correct to use.
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Takes ownership of `instance` and returns the handle a host holds.
fn register(instance: Box<Instance>) -> *mut HsInstance {
    let pointer = NonNull::from(Box::leak(Box::new(HsInstance { instance })));
    let mut table = live();
    let mut id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    // Only reachable once the counter wraps, which on a 64-bit host it
    // never does: skip zero and anything still live.
    while id == 0 || table.contains_key(&id) {
        id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    }
    table.insert(
        id,
        Live {
            instance: Owned(pointer),
            busy: false,
            dispose_pending: false,
        },
    );
    std::ptr::without_provenance_mut(id)
}

/// Frees an instance [`LIVE`] no longer holds.
fn release(owned: &Owned) {
    // SAFETY: the pointer came from `Box::leak` in `register`, and the
    // caller has just removed it from `LIVE`, which is the only place it
    // was held, so nothing can reach it again and this is its one release.
    let mut handle = unsafe { Box::from_raw(owned.0.as_ptr()) };
    let _ = handle.instance.dispose();
}

/// The exclusive use of one live instance for the length of a call.
/// Dropping it, including while a panic unwinds, gives the instance back,
/// or frees it if [`hs_dispose`] was called in the meantime.
struct InUse {
    id: usize,
    pointer: NonNull<HsInstance>,
}

impl InUse {
    /// Marks the instance named by `handle` busy and returns it, or the
    /// status that says why it cannot be used.
    fn acquire(handle: *mut HsInstance) -> Result<InUse, HsStatus> {
        let id = handle.addr();
        let mut table = live();
        let Some(entry) = table.get_mut(&id) else {
            return Err(HsStatus::UnknownHandle);
        };
        if entry.busy {
            return Err(HsStatus::Refused);
        }
        entry.busy = true;
        Ok(InUse {
            id,
            pointer: entry.instance.0,
        })
    }

    fn instance(&mut self) -> &mut Instance {
        // SAFETY: the pointer came from `Box::leak` in `register` and is
        // still in `LIVE`, which frees it only once no call holds it busy.
        // This call holds it busy until `self` is dropped, so no other
        // reference to it exists and it is not freed while this one lives.
        let handle = unsafe { &mut *self.pointer.as_ptr() };
        &mut handle.instance
    }
}

impl Drop for InUse {
    fn drop(&mut self) {
        let mut table = live();
        let pending = match table.get_mut(&self.id) {
            Some(entry) if entry.dispose_pending => true,
            Some(entry) => {
                entry.busy = false;
                false
            }
            None => false,
        };
        if pending {
            if let Some(entry) = table.remove(&self.id) {
                drop(table);
                release(&entry.instance);
            }
        }
    }
}

/// The message written for a handle [`InUse::acquire`] refused.
fn refusal(status: HsStatus) -> &'static str {
    if status == HsStatus::UnknownHandle {
        "the instance handle is not live: it was never given out, or hs_dispose has released it"
    } else {
        "the instance is already in use by another call"
    }
}

impl std::fmt::Debug for HsInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HsInstance")
            .field("state", &self.instance.state())
            .finish()
    }
}

/// The library's own version, as a static, null-terminated string with no
/// allocation: this crate's own version (the workspace version, for
/// example `"0.1.0"`). A host compares this against the version it was built
/// against to detect a mismatched library.
#[unsafe(no_mangle)]
pub extern "C" fn hs_version() -> *const c_char {
    /// The version with the terminating nul a C string needs, so the
    /// pointer below names a complete, `'static` C string.
    const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");
    VERSION.as_ptr().cast::<c_char>()
}

/// Checks `source` (the entry module) and writes a JSON array of diagnostics
/// to `*out_json`, in the shape `corpus/FORMAT.md` and the CLI's `--json`
/// mode use: `[{"severity":"error","line":1,"code":"HS1099","message":"..."}, ...]`.
///
/// `entry_name` is the path reported with each diagnostic; a null pointer
/// means `"main.hws"`. On success, `*out_json` is a string the caller must
/// free with [`hs_string_free`]; on any other status, `*out_json` is set to
/// null and nothing needs freeing.
///
/// # Safety
///
/// `source` must be a valid, null-terminated, UTF-8 C string. `out_json` must
/// be a valid pointer to a `*mut c_char` the caller owns. `entry_name`, if not
/// null, must also be a valid, null-terminated, UTF-8 C string. Both input
/// strings must outlive the call, which they trivially do since this function
/// does not retain them past its own return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_check_json(
    source: *const c_char,
    entry_name: *const c_char,
    out_json: *mut *mut c_char,
) -> c_int {
    guard(|| {
        // SAFETY: `out_json`, if non-null, is a valid pointer to a
        // `*mut c_char` by the caller's contract; this runs before any
        // other argument is checked, so `*out_json` is never left
        // untouched on an error path (an invalid `source`, in particular).
        unsafe { clear_text_out(out_json) };
        if source.is_null() || out_json.is_null() {
            return HsStatus::NullArgument as c_int;
        }
        // SAFETY: `source` is non-null and, by the caller's contract, a valid
        // null-terminated C string that outlives this call.
        let Ok(source) = unsafe { CStr::from_ptr(source) }.to_str() else {
            return HsStatus::InvalidUtf8 as c_int;
        };
        let entry_name = if entry_name.is_null() {
            Ok("main.hws")
        } else {
            // SAFETY: the caller's contract makes this the same as `source` above.
            unsafe { CStr::from_ptr(entry_name) }.to_str()
        };
        let Ok(entry_name) = entry_name else {
            return HsStatus::InvalidUtf8 as c_int;
        };
        // The C ABI takes source text, not a file system root, so an import can
        // have nothing to resolve against: every one is reported as not found.
        let diagnostics = check(
            source,
            entry_name,
            &CapabilityInterface::default(),
            None,
            &mut NoLoader,
            &mut Control::none(),
        );
        let json = diagnostics_to_json(&diagnostics, entry_name, source);
        // `json` came from our own formatting of ASCII and escaped text only, so
        // it cannot contain an interior NUL; `expect` here reports a defect in
        // `diagnostics_to_json`, never a property of `source`.
        let Ok(c_string) = CString::new(json) else {
            // SAFETY: `out_json` is non-null by the check above, and a valid
            // pointer to a `*mut c_char` by the caller's contract.
            unsafe { *out_json = std::ptr::null_mut() };
            return HsStatus::InvalidUtf8 as c_int;
        };
        // SAFETY: `out_json` is non-null by the check above, and a valid pointer
        // to a `*mut c_char` by the caller's contract; `into_raw` hands over an
        // allocation the caller now owns and must return through `hs_string_free`.
        unsafe { *out_json = c_string.into_raw() };
        HsStatus::Ok as c_int
    })
}

/// Formats `source` (the entry module) to its canonical layout and writes a
/// JSON document to `*out_json`, in the shape `corpus/FORMAT.md` and the
/// CLI's `--json` mode use: `{"output":"...text..."|null,"diagnostics":[...]}`.
/// `output` is `null` when the source has a syntax error, or a comment the
/// layout has no place to keep; `diagnostics` then says why. Unlike
/// [`hs_check_json`], this needs no checker and already works end to end.
///
/// `entry_name` is the path reported with each diagnostic; a null pointer
/// means `"main.hws"`. On success, `*out_json` is a string the caller must
/// free with [`hs_string_free`]; on any other status, `*out_json` is set to
/// null and nothing needs freeing.
///
/// # Safety
///
/// `source` must be a valid, null-terminated, UTF-8 C string. `out_json` must
/// be a valid pointer to a `*mut c_char` the caller owns. `entry_name`, if not
/// null, must also be a valid, null-terminated, UTF-8 C string. Both input
/// strings must outlive the call, which they trivially do since this function
/// does not retain them past its own return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_format_json(
    source: *const c_char,
    entry_name: *const c_char,
    out_json: *mut *mut c_char,
) -> c_int {
    guard(|| {
        // SAFETY: as in `hs_check_json`.
        unsafe { clear_text_out(out_json) };
        if source.is_null() || out_json.is_null() {
            return HsStatus::NullArgument as c_int;
        }
        // SAFETY: `source` is non-null and, by the caller's contract, a valid
        // null-terminated C string that outlives this call.
        let Ok(source) = unsafe { CStr::from_ptr(source) }.to_str() else {
            return HsStatus::InvalidUtf8 as c_int;
        };
        let entry_name = if entry_name.is_null() {
            Ok("main.hws")
        } else {
            // SAFETY: the caller's contract makes this the same as `source` above.
            unsafe { CStr::from_ptr(entry_name) }.to_str()
        };
        let Ok(entry_name) = entry_name else {
            return HsStatus::InvalidUtf8 as c_int;
        };
        let formatted = hollowscript::format(source, entry_name, &mut Control::none());
        let json = format_to_json(
            formatted.output.as_deref(),
            &formatted.diagnostics,
            entry_name,
            source,
        );
        // `json` came from our own formatting of ASCII and escaped text only, so
        // it cannot contain an interior NUL; `expect` here reports a defect in
        // `format_to_json`, never a property of `source`.
        let Ok(c_string) = CString::new(json) else {
            // SAFETY: `out_json` is non-null by the check above, and a valid
            // pointer to a `*mut c_char` by the caller's contract.
            unsafe { *out_json = std::ptr::null_mut() };
            return HsStatus::InvalidUtf8 as c_int;
        };
        // SAFETY: `out_json` is non-null by the check above, and a valid pointer
        // to a `*mut c_char` by the caller's contract; `into_raw` hands over an
        // allocation the caller now owns and must return through `hs_string_free`.
        unsafe { *out_json = c_string.into_raw() };
        HsStatus::Ok as c_int
    })
}

/// Frees a string this crate returned. Freeing null is a no-op. Freeing a
/// pointer this crate did not return, or freeing one twice, is undefined
/// behaviour, exactly as for any other owned allocation.
///
/// The string's content is the caller's to read, never to modify: this
/// finds the string's own length again by scanning for its NUL terminator
/// ([`CString::from_raw`]'s own contract), so a caller that wrote an
/// earlier `'\0'` into it, to use it as a shorter string in place, makes
/// this free fewer bytes than were allocated, corrupting the heap rather
/// than merely leaking. A caller that wants a shorter string copies the
/// prefix it needs and frees the original, unmodified, exactly as it was
/// returned.
///
/// # Safety
///
/// `ptr` must be null, or a pointer this crate returned that has not already
/// been freed, and whose content has not been modified since.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: by the caller's contract, `ptr` is a pointer `CString::into_raw`
    // produced in this crate and not yet freed; reclaiming it with
    // `from_raw` and dropping it is exactly how such a pointer is released.
    drop(unsafe { CString::from_raw(ptr) });
}

/// Loads `source` as the entry module named `entry_name` and writes an
/// opaque instance handle to `*out_instance` (section 12.3).
///
/// A program with a checker error creates no instance: the return is
/// [`HsStatus::CheckFailed`], `*out_instance` is left null and `*out_json`
/// holds the diagnostics in the shape [`hs_check_json`] uses.
///
/// A program whose *initialisation* faults (a top-level `1 / 0`, for one)
/// still produces a live instance, in [`hollowscript::instance::State::Faulted`]:
/// the return is [`HsStatus::Faulted`], `*out_instance` is the caller's
/// handle exactly as on success, and `*out_json` holds the fault as a
/// one-element diagnostics array, the same shape [`hs_check_json`] itself
/// uses (section 12.1's own promise that a load's fault is answered "as
/// often as asked"; see `finish_instance_load`'s own documentation). The
/// instance still needs [`hs_dispose`]; a call on it is
/// [`HsStatus::Refused`], as on any other Faulted instance.
///
/// A program with no error, [`HsStatus::Ok`] or [`HsStatus::Faulted`]
/// either way, may still have its own warnings (HS0801 "unused constant or
/// variable", for one): `*out_json` then holds those too, in the same
/// diagnostics array, after the fault if there is one. `*out_json` is the
/// empty array `"[]"` only when there is neither a fault nor a warning.
///
/// Nothing is granted: this ABI has no way for a C host to supply a
/// capability yet, so a program that says `uses print` is HS0404 here. That
/// is a real limit of this surface, not of the language.
///
/// # Safety
///
/// `source` and `entry_name` must be non-null, null-terminated, valid UTF-8
/// and readable for their length. `out_instance` and `out_json` must be
/// non-null and point at writable storage of their types. On
/// [`HsStatus::Ok`] or [`HsStatus::Faulted`], `*out_instance` is an
/// instance the caller owns and must release with [`hs_dispose`]; on
/// [`HsStatus::CheckFailed`] or [`HsStatus::Faulted`], `*out_json` is a
/// string the caller owns and must release with [`hs_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_load(
    source: *const c_char,
    entry_name: *const c_char,
    out_instance: *mut *mut HsInstance,
    out_json: *mut *mut c_char,
) -> c_int {
    guard(|| {
        // SAFETY: `out_instance` and `out_json`, if non-null, are valid
        // pointers to their respective types by the caller's contract; each
        // is cleared, in its own single-operation block, before any other
        // argument is checked, so neither is ever left untouched on an
        // error path.
        unsafe { clear_instance_out(out_instance) };
        // SAFETY: as immediately above, for `out_json`.
        unsafe { clear_text_out(out_json) };
        if source.is_null() || entry_name.is_null() || out_instance.is_null() || out_json.is_null()
        {
            return HsStatus::NullArgument as c_int;
        }
        // SAFETY: both are non-null by the check above, and null-terminated and
        // readable by the caller's contract.
        let (Ok(source), Ok(entry_name)) = (
            unsafe { CStr::from_ptr(source) }.to_str(),
            unsafe { CStr::from_ptr(entry_name) }.to_str(),
        ) else {
            return HsStatus::InvalidUtf8 as c_int;
        };
        // `load` runs its own check internally and would answer the same
        // `CheckFailed` diagnostics if there were an error; this crate runs
        // it once more, first, only so that a *clean* program's warnings
        // (section 12.3 step 1 can report a diagnostic with no error in it)
        // are not simply discarded the way they used to be, since
        // `LoadOutcome::Instance` itself carries no way to return them. See
        // `precheck`'s own documentation.
        let warnings = match precheck(source, entry_name, &CapabilityInterface::default()) {
            Err(diagnostics) => {
                write_string(
                    out_json,
                    &diagnostics_to_json(&diagnostics, entry_name, source),
                );
                return HsStatus::CheckFailed as c_int;
            }
            Ok(warnings) => warnings,
        };
        let outcome = load(
            source,
            entry_name,
            &Options::default(),
            &mut NoLoader,
            &mut |_, _| None,
            &mut Control::none(),
        );
        match outcome {
            // Unreachable in practice (`precheck` already found no error in
            // the same source against the same capabilities), kept only so
            // this match is exhaustive and correct if the two ever somehow
            // disagreed, rather than assuming they cannot.
            LoadOutcome::CheckFailed(diagnostics) => {
                write_string(
                    out_json,
                    &diagnostics_to_json(&diagnostics, entry_name, source),
                );
                HsStatus::CheckFailed as c_int
            }
            LoadOutcome::Instance(instance) => finish_instance_load(
                instance,
                entry_name,
                source,
                warnings,
                out_instance,
                out_json,
            ),
        }
    })
}

/// Runs the checker once for `source`/`entry_name` against `capabilities`,
/// separately from what `load` itself runs internally: `load` only ever
/// hands back its diagnostics on the `CheckFailed` path (section 12.3 step
/// 1), so a *clean* program's own warnings, a diagnostic with no error in
/// it, had no way to reach a host through this ABI at all, silently
/// dropped rather than merely unread (security.md and section 12.3 both
/// describe reporting them). `filter_schema` is always `None`: this ABI has
/// no way for a host to supply one yet, the same limit [`hs_check_json`]'s
/// own documentation already states.
///
/// # Errors
///
/// The diagnostics, unchanged, when any of them is an error: the same
/// outcome `load` would separately answer as `LoadOutcome::CheckFailed`,
/// computed once here instead of a second time inside it.
fn precheck(
    source: &str,
    entry_name: &str,
    capabilities: &CapabilityInterface,
) -> Result<Vec<hollowscript_core::Diagnostic>, Vec<hollowscript_core::Diagnostic>> {
    precheck_with(source, entry_name, capabilities, &mut Control::none())
}

/// As [`precheck`], but with an explicit [`Control`], so [`hs_load_ex`] can
/// give this its own clone of the host's cancellation token: a token set
/// while this extra check runs then cancels it too, not only `load`'s own
/// internal one afterwards.
fn precheck_with(
    source: &str,
    entry_name: &str,
    capabilities: &CapabilityInterface,
    control: &mut Control<'_>,
) -> Result<Vec<hollowscript_core::Diagnostic>, Vec<hollowscript_core::Diagnostic>> {
    let diagnostics = check(
        source,
        entry_name,
        capabilities,
        None,
        &mut NoLoader,
        control,
    );
    if diagnostics
        .iter()
        .any(hollowscript_core::Diagnostic::is_error)
    {
        Err(diagnostics)
    } else {
        Ok(diagnostics)
    }
}

/// Finishes the `LoadOutcome::Instance` arm [`hs_load`] and [`hs_load_ex`]
/// share: registers the instance and writes its handle to `*out_instance`
/// (a Faulted instance is still a live handle a host must dispose of, the
/// same as a Ready or Finished one), then reports whether initialisation
/// reached Ready or Finished ([`HsStatus::Ok`]) or Faulted
/// ([`HsStatus::Faulted`]). `*out_json` holds a diagnostics array in the
/// shape [`hs_check_json`] itself uses: the fault, if there is one, then
/// `warnings` (from [`precheck`]), so a host reads both a load's own fault
/// and its warnings the one way this ABI already reports diagnostics,
/// rather than either being swallowed.
///
/// Before this, a load that faulted (a top-level `constant x = 1 / 0`, for
/// one) answered [`HsStatus::Ok`] with `*out_json` unconditionally `"[]"`,
/// so the only way to learn a freshly loaded instance was already Faulted
/// was to attempt a call on it and read the refusal reason from that;
/// section 12.1's own promise that a load's fault is answered "as often as
/// asked" had no way to reach this ABI's own load result at all.
fn finish_instance_load(
    instance: Box<Instance>,
    entry_name: &str,
    source: &str,
    warnings: Vec<hollowscript_core::Diagnostic>,
    out_instance: *mut *mut HsInstance,
    out_json: *mut *mut c_char,
) -> c_int {
    let fault_diagnostic = instance.fault().map(Fault::to_diagnostic);
    let handle = register(instance);
    // SAFETY: `out_instance` is non-null and writable by every caller's own
    // contract (checked, and cleared to null, before this is reached). The
    // caller now holds the handle.
    unsafe { *out_instance = handle };
    let status = if fault_diagnostic.is_some() {
        HsStatus::Faulted as c_int
    } else {
        HsStatus::Ok as c_int
    };
    let mut reported = Vec::with_capacity(warnings.len() + 1);
    reported.extend(fault_diagnostic);
    reported.extend(warnings);
    write_string(
        out_json,
        &diagnostics_to_json(&reported, entry_name, source),
    );
    status
}

/// Calls the exported function `name` of `instance` with `arguments_json`, a
/// JSON array of scalars, and writes the rendered result to `*out_text`
/// (section 12.4).
///
/// A function that returns nothing writes the empty string. A call the
/// instance refuses (an unknown export, the wrong number of arguments, an
/// instance that is not Ready) returns [`HsStatus::Refused`] with the reason
/// in `*out_text`; one that runs and faults returns [`HsStatus::Faulted`]
/// with the fault's message.
///
/// # Safety
///
/// `instance` may be any value: one that is not a live handle is
/// [`HsStatus::UnknownHandle`], and one another thread is using is
/// [`HsStatus::Refused`]. `name` and `arguments_json` must be non-null,
/// null-terminated, valid UTF-8 and readable. `out_text` must be non-null
/// and point at writable storage; on return it is a string the caller owns
/// and must release with [`hs_string_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_call(
    instance: *mut HsInstance,
    name: *const c_char,
    arguments_json: *const c_char,
    out_text: *mut *mut c_char,
) -> c_int {
    guard_with(
        || {
            // SAFETY: `out_text`, if non-null, is a valid pointer to a
            // `*mut c_char` by the caller's contract; this runs before any
            // other argument is checked, so `*out_text` is never left
            // untouched on an error path.
            unsafe { clear_text_out(out_text) };
            if instance.is_null()
                || name.is_null()
                || arguments_json.is_null()
                || out_text.is_null()
            {
                return HsStatus::NullArgument as c_int;
            }
            let mut in_use = match InUse::acquire(instance) {
                Ok(in_use) => in_use,
                Err(status) => {
                    write_string(out_text, refusal(status));
                    return status as c_int;
                }
            };
            let instance = in_use.instance();
            // SAFETY: both are non-null by the check above, and null-terminated and
            // readable by the contract.
            let (Ok(name), Ok(arguments)) = (
                unsafe { CStr::from_ptr(name) }.to_str(),
                unsafe { CStr::from_ptr(arguments_json) }.to_str(),
            ) else {
                return HsStatus::InvalidUtf8 as c_int;
            };
            // `parse_arguments` already releases whatever it rooted itself if
            // it fails partway through (see its own documentation); nothing
            // is rooted at all on the arity-checked and structural error
            // paths, so nothing needs releasing here on `Err`.
            let arguments = match parse_arguments(instance, name, arguments) {
                Ok(arguments) => arguments,
                Err(reason) => {
                    write_string(out_text, &reason);
                    return HsStatus::Refused as c_int;
                }
            };
            // From here on `arguments` is rooted against the heap
            // (`parse_arguments` remembers each one), so every remaining
            // exit path, including a refusal or a fault, releases it: a
            // refused or faulted call must not leak its arguments any more
            // than a successful one does.
            match instance.call(name, &arguments, &mut Control::none()) {
                Err(error) => {
                    instance.release_values();
                    write_string(out_text, &error.to_diagnostic().message);
                    HsStatus::Refused as c_int
                }
                Ok(CallOutcome::Faulted(fault)) => {
                    instance.release_values();
                    write_string(out_text, &fault.to_diagnostic().message);
                    HsStatus::Faulted as c_int
                }
                Ok(CallOutcome::Returned(None)) => {
                    // A C caller never holds a `Value`: everything crosses the
                    // boundary as text, so nothing here has to stay alive past this
                    // call and the next collection may free all of it.
                    instance.release_values();
                    write_string(out_text, "");
                    HsStatus::Ok as c_int
                }
                Ok(CallOutcome::Returned(Some(value))) => {
                    let rendered = instance.render(value);
                    instance.release_values();
                    write_string(out_text, &rendered);
                    HsStatus::Ok as c_int
                }
                // The C ABI binds capabilities synchronously, so nothing it loads
                // can pause; reported rather than unwrapped all the same.
                Ok(CallOutcome::Suspended) => {
                    instance.release_values();
                    write_string(out_text, "the run paused at a capability call");
                    HsStatus::Refused as c_int
                }
            }
        },
        || end_after_panic(instance),
    )
}

/// Reads the exported constant `name` of `instance` and writes its rendering
/// to `*out_text` (section 12.1's `read`).
///
/// # Safety
///
/// As [`hs_call`], for `instance`, `name` and `out_text`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_read(
    instance: *mut HsInstance,
    name: *const c_char,
    out_text: *mut *mut c_char,
) -> c_int {
    guard_with(
        || {
            // SAFETY: as in `hs_call`.
            unsafe { clear_text_out(out_text) };
            if instance.is_null() || name.is_null() || out_text.is_null() {
                return HsStatus::NullArgument as c_int;
            }
            let mut in_use = match InUse::acquire(instance) {
                Ok(in_use) => in_use,
                Err(status) => {
                    write_string(out_text, refusal(status));
                    return status as c_int;
                }
            };
            let instance = in_use.instance();
            // SAFETY: non-null by the check above, null-terminated and readable by
            // the contract.
            let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
                return HsStatus::InvalidUtf8 as c_int;
            };
            match instance.read(name) {
                Err(error) => {
                    write_string(out_text, &error.to_diagnostic().message);
                    HsStatus::Refused as c_int
                }
                Ok(value) => {
                    let rendered = instance.render(value);
                    write_string(out_text, &rendered);
                    HsStatus::Ok as c_int
                }
            }
        },
        || end_after_panic(instance),
    )
}

/// `result(instance)` (section 12.1): the load result, the value a
/// top-level `return` in the entry module produced, rendered the same way
/// [`hs_call`]'s own result is. A Ready instance (initialisation ran to the
/// end with no top-level `return`) answers the empty string, every time
/// this is called; a Finished instance (initialisation ended through a
/// top-level `return`, or the `Err` a top-level `?` met) answers the
/// rendered value the first time this is called and [`HsStatus::Refused`]
/// after, since section 12.1 gives a Finished instance's load result out
/// once. Refused for any other state (in particular Faulted: read the
/// fault from [`hs_load_ex`]'s own diagnostics or [`hs_call`]'s answer
/// instead, which is what section 12.1 means by the fault being answered
/// "as often as asked" while the load result is not).
///
/// # Safety
///
/// As [`hs_call`], for `instance` and `out_text`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_load_result(
    instance: *mut HsInstance,
    out_text: *mut *mut c_char,
) -> c_int {
    guard_with(
        || {
            // SAFETY: as in `hs_call`.
            unsafe { clear_text_out(out_text) };
            if instance.is_null() || out_text.is_null() {
                return HsStatus::NullArgument as c_int;
            }
            let mut in_use = match InUse::acquire(instance) {
                Ok(in_use) => in_use,
                Err(status) => {
                    write_string(out_text, refusal(status));
                    return status as c_int;
                }
            };
            let instance = in_use.instance();
            match instance.result() {
                Some(hollowscript::LoadResult::NoValue) => {
                    write_string(out_text, "");
                    HsStatus::Ok as c_int
                }
                Some(hollowscript::LoadResult::Value(value)) => {
                    let rendered = instance.render(value);
                    write_string(out_text, &rendered);
                    HsStatus::Ok as c_int
                }
                None => {
                    write_string(
                        out_text,
                        "the instance has no load result: it is not Ready or Finished, \
                         or a Finished instance's load result was already read once",
                    );
                    HsStatus::Refused as c_int
                }
            }
        },
        || end_after_panic(instance),
    )
}

/// Ends `instance` and releases everything it holds (section 12.5).
///
/// A handle that is not live (null, never given out, or already released)
/// is ignored, so a host can call this unconditionally and more than once.
/// Called while another thread is part way through a call on the same
/// instance, it takes effect when that call finishes.
///
/// # Safety
///
/// None beyond calling it through the C ABI: the handle is looked up, never
/// followed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_dispose(instance: *mut HsInstance) {
    let _ = guard(|| {
        let id = instance.addr();
        let mut table = live();
        let released = match table.get_mut(&id) {
            Some(entry) if entry.busy => {
                entry.dispose_pending = true;
                None
            }
            Some(_) => table.remove(&id),
            None => None,
        };
        drop(table);
        if let Some(entry) = released {
            release(&entry.instance);
        }
        HsStatus::Ok as c_int
    });
}

/// Ends an instance after a panic part way through a call on it, so every
/// later call on the handle is refused rather than reaching state the panic
/// left half-changed. The handle stays the caller's to release with
/// [`hs_dispose`]. By the time this runs the call's [`InUse`] has been
/// dropped by the unwind, so the instance can be taken again.
fn end_after_panic(instance: *mut HsInstance) {
    if let Ok(mut in_use) = InUse::acquire(instance) {
        let _ = in_use.instance().dispose();
    }
}

/// Writes `text` to `*out` as a freshly allocated C string the caller owns.
/// A string that cannot be made (one holding an interior NUL, which nothing
/// here produces) is written as null.
fn write_string(out: *mut *mut c_char, text: &str) {
    let pointer = CString::new(text).map_or(std::ptr::null_mut(), CString::into_raw);
    // SAFETY: every caller checks `out` for null first, and the caller's
    // contract makes it a valid pointer to a `*mut c_char`.
    unsafe { *out = pointer };
}

/// Writes null to `*out`, if `out` itself is non-null. Every function below
/// that has a `char **out_*` parameter calls this on it before doing
/// anything else, including before checking whether its *other* arguments
/// are null or valid UTF-8: an error path that returns before this crate's
/// own code would otherwise write a real value must still leave `*out`
/// null, never untouched, so a host that does not check the status first
/// (or checks it correctly but still frees whatever garbage was on the
/// stack at `*out`) reads a null pointer rather than uninitialised or stale
/// memory. `hs_load`'s `out_instance` has the analogous
/// [`clear_instance_out`].
///
/// # Safety
///
/// `out`, if non-null, must be a valid pointer to a `*mut c_char` the
/// caller owns.
unsafe fn clear_text_out(out: *mut *mut c_char) {
    if !out.is_null() {
        // SAFETY: non-null by the check above, and a valid pointer to a
        // `*mut c_char` by the caller's contract.
        unsafe { *out = std::ptr::null_mut() };
    }
}

/// As [`clear_text_out`], for [`hs_load`] and [`hs_load_ex`]'s own
/// `out_instance`.
///
/// # Safety
///
/// `out`, if non-null, must be a valid pointer to a `*mut HsInstance` the
/// caller owns.
unsafe fn clear_instance_out(out: *mut *mut HsInstance) {
    if !out.is_null() {
        // SAFETY: non-null by the check above, and a valid pointer to a
        // `*mut HsInstance` by the caller's contract.
        unsafe { *out = std::ptr::null_mut() };
    }
}

/// The arguments of [`hs_call`]: a JSON array, each element in the
/// transferable-value encoding this crate's module documentation defines,
/// built against the instance's own heap because anything beyond a scalar
/// is a handle into it.
///
/// The argument count is checked against `name`'s own declared arity, when
/// `name` names an exported function, before any argument is built against
/// the heap: a caller that gets the count wrong (the common shape of a
/// refused call) never pays for rooting arguments `instance.call` would
/// reject anyway, and the loop below only ever roots as many values as the
/// export itself can accept. If building an argument fails partway through
/// (an earlier item in the same call has already been rooted), every root
/// this call took is released before the error is returned, so a caller
/// that retries after a bad argument never accumulates them (see
/// `Instance::release_values`'s own documentation: this ABI never holds a
/// `Value` across a call, so releasing here is always safe).
fn parse_arguments(instance: &mut Instance, name: &str, text: &str) -> Result<Vec<Value>, String> {
    let trimmed = text.trim();
    let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return Err("the arguments must be a JSON array".to_owned());
    };
    let inner = inner.trim();
    if inner.is_empty() {
        if let Some(arity) = export_arity(instance.module(), name) {
            if arity != 0 {
                return Err(arity_message(instance.module(), name, 0));
            }
        }
        return Ok(Vec::new());
    }
    // The array is re-read through the language's own JSON reader, which is
    // strict and whose errors are already defined, rather than a second
    // parser written here.
    let parsed =
        hollowscript::json::parse(trimmed).map_err(|e| format!("{} at {}", e.message, e.offset))?;
    let hollowscript::json::Json::Array(items) = parsed else {
        return Err("the arguments must be a JSON array".to_owned());
    };
    // Section 12.4's own arity check, done here rather than left entirely to
    // `Instance::call`, so a mismatched count is refused before this
    // function roots a single argument against the heap.
    if let Some(arity) = export_arity(instance.module(), name) {
        if arity != items.len() {
            return Err(arity_message(instance.module(), name, items.len()));
        }
    }
    // `name`'s own declared parameter types, so a bare JSON number decodes
    // to the `Float` a signature calls for even when it looks integral
    // (`coerce_json_numbers`'s own documentation explains why this is
    // needed at all): each borrow of `instance` ends with the statement
    // that takes it, before `heap_string`'s own `&mut` borrow below.
    let param_types = export_param_types(instance.module(), name).unwrap_or_default();
    let types = instance.module().types.clone();
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let item = match param_types.get(index) {
            Some(&ty) => match coerce_json_numbers(item, ty, &types) {
                Ok(item) => item,
                Err(reason) => {
                    // Nothing has been rooted yet this call when this is the
                    // first argument; anything built by an earlier one has,
                    // and must not be left rooted for a caller that never
                    // reaches a successful call.
                    instance.release_values();
                    return Err(format!("argument {} of '{name}': {reason}", index + 1));
                }
            },
            None => item,
        };
        let value = match instance.heap_string(|call| json_to_value(call, item)) {
            Ok(value) => value,
            Err(fault) => {
                // Every argument built so far (`out`) is rooted; a caller
                // that never reaches a successful call must not accumulate
                // them.
                instance.release_values();
                return Err(fault.to_diagnostic().message);
            }
        };
        out.push(value);
    }
    Ok(out)
}

/// The declared parameter types of the exported function `name`, or `None`
/// if `name` names no exported function (which `instance.call` itself
/// refuses with its own, more specific message; this only needs to
/// distinguish "no such export" from "an export with no parameters" so
/// [`export_arity`] can tell the two apart).
fn export_param_types(
    module: &hollowscript_bytecode::module::Module,
    name: &str,
) -> Option<Vec<TypeRef>> {
    for export in &module.exports {
        if module.string(export.name) == name {
            if let ExportKind::Function { signature, .. } = &export.kind {
                return Some(signature.params.clone());
            }
        }
    }
    None
}

/// `name`'s own declared arity, or `None` if `name` names no exported
/// function.
fn export_arity(module: &hollowscript_bytecode::module::Module, name: &str) -> Option<usize> {
    export_param_types(module, name).map(|params| params.len())
}

/// The message [`Instance::call`] itself would report for `name` called
/// with `found` arguments, built the same way here so a caller sees the
/// same wording whether the mismatch is caught before or during the call.
fn arity_message(
    module: &hollowscript_bytecode::module::Module,
    name: &str,
    found: usize,
) -> String {
    let arity = export_arity(module, name).unwrap_or(0);
    format!("expected {arity} argument(s), found {found}")
}

/// Section 3.4's `Float` carries 53 bits of integer precision, exactly as
/// `hollowscript-vm`'s own `accept::int_as_float` limits a host-returned
/// `Int` at a declared `Float` position (that check lives one crate down
/// and is not exported, so this is the same range, not the same function);
/// an `Int` outside it is not one the conversion can preserve.
const MAX_EXACT_FLOAT_INT: i64 = 1 << 53;

/// Recursively coerces a JSON number that arrived looking like an integer
/// (no `.` and no exponent, [`hollowscript::json::parse`] cannot otherwise
/// tell `3` from `3.0`, and `JSON.stringify(3.0)` on a JS host is already
/// just `"3"`) to a JSON float wherever `expected` says the position is
/// `Float`, walking into a list, a map's own value, a record field, an
/// enum's or `Result`'s own payload and an optional's own inner type the
/// same way. Every other shape is left exactly as parsed, for the ordinary
/// decode and section 9.6's own acceptance rule to accept or refuse
/// afterwards, unchanged.
///
/// This is the encoding's own definition of which language value a bare
/// JSON number denotes, given the declared type; it is not the conversion
/// section 9.6 says never happens "inside a collection, a record or an
/// enum payload", which is about a language *value* the boundary has
/// already decided on, not about deciding what one JSON number means in
/// the first place.
///
/// # Errors
///
/// A wire integer at a `Float` position that is not exactly representable
/// (outside `-2^53..=2^53`) is refused with a reason, not rounded to the
/// nearest `Float`: rounding here would happen before the value even
/// becomes a `Value::Float`, so it could never be caught downstream the way
/// [`hollowscript_vm`]'s own acceptance rule catches a host-returned `Int`
/// in the same range, including inside a list, map, record or enum
/// payload, which is exactly where an unbounded `n as f64` used to round
/// silently.
fn coerce_json_numbers(
    json: hollowscript::json::Json,
    expected: TypeRef,
    types: &TypeTable,
) -> Result<hollowscript::json::Json, String> {
    use hollowscript::json::Json;
    let Some(entry) = types.get(expected) else {
        return Ok(json);
    };
    match entry {
        TransferType::Float => match json {
            Json::Int(n) => {
                if !(-MAX_EXACT_FLOAT_INT..=MAX_EXACT_FLOAT_INT).contains(&n) {
                    return Err(format!(
                        "{n} is not exactly representable as Float (outside \
                         -2^53 to 2^53) and is refused rather than rounded"
                    ));
                }
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "the range check above makes the conversion exact"
                )]
                let f = n as f64;
                Ok(Json::Float(f))
            }
            other => Ok(other),
        },
        TransferType::List(element) => coerce_tagged_array(json, "list", *element, types),
        TransferType::Set(_) => Ok(json), // a Set's element is Int, String or Bool: never Float.
        TransferType::Map(_, value_ty) => coerce_map(json, *value_ty, types),
        TransferType::Optional(inner) => match json {
            Json::Null => Ok(Json::Null),
            other => coerce_json_numbers(other, *inner, types),
        },
        TransferType::Record(fields) => coerce_record(json, fields, types),
        TransferType::Enum { name, variants, .. } if name == RESULT => {
            coerce_result_sugar(json, variants, types)
        }
        TransferType::Enum { variants, .. } => coerce_enum(json, variants, types),
        TransferType::Int | TransferType::Bool | TransferType::String | TransferType::Filter => {
            Ok(json)
        }
    }
}

/// `json`, with `{"list": [...]}`'s own array elements coerced against
/// `element`; any other shape (a mismatch the ordinary decode will refuse)
/// is left untouched.
fn coerce_tagged_array(
    json: hollowscript::json::Json,
    tag: &str,
    element: TypeRef,
    types: &TypeTable,
) -> Result<hollowscript::json::Json, String> {
    use hollowscript::json::Json;
    let Json::Object(fields) = json else {
        return Ok(json);
    };
    let mut coerced = Vec::with_capacity(fields.len());
    for (key, value) in fields {
        if key == tag {
            if let Json::Array(items) = value {
                let mut coerced_items = Vec::with_capacity(items.len());
                for item in items {
                    coerced_items.push(coerce_json_numbers(item, element, types)?);
                }
                coerced.push((key, Json::Array(coerced_items)));
            } else {
                coerced.push((key, value));
            }
        } else {
            coerced.push((key, value));
        }
    }
    Ok(Json::Object(coerced))
}

/// `json`, with `{"map": [[k, v], ...]}`'s own `v`s coerced against
/// `value_ty` (a key is always `Int`, `String` or `Bool`, never `Float`).
fn coerce_map(
    json: hollowscript::json::Json,
    value_ty: TypeRef,
    types: &TypeTable,
) -> Result<hollowscript::json::Json, String> {
    use hollowscript::json::Json;
    let Json::Object(fields) = json else {
        return Ok(json);
    };
    let mut coerced = Vec::with_capacity(fields.len());
    for (key, value) in fields {
        if key != "map" {
            coerced.push((key, value));
            continue;
        }
        let Json::Array(entries) = value else {
            coerced.push((key, value));
            continue;
        };
        let mut coerced_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            let Json::Array(mut pair) = entry else {
                coerced_entries.push(entry);
                continue;
            };
            if let Some(v) = pair.get_mut(1) {
                *v = coerce_json_numbers(std::mem::replace(v, Json::Null), value_ty, types)?;
            }
            coerced_entries.push(Json::Array(pair));
        }
        coerced.push((key, Json::Array(coerced_entries)));
    }
    Ok(Json::Object(coerced))
}

/// `json`, with `{"record": {"field": value, ...}}`'s own field values
/// coerced against `fields`' own declared field types, matched by name.
fn coerce_record(
    json: hollowscript::json::Json,
    fields: &[(String, TypeRef)],
    types: &TypeTable,
) -> Result<hollowscript::json::Json, String> {
    use hollowscript::json::Json;
    let Json::Object(tagged) = json else {
        return Ok(json);
    };
    let mut coerced = Vec::with_capacity(tagged.len());
    for (key, value) in tagged {
        if key != "record" {
            coerced.push((key, value));
            continue;
        }
        let Json::Object(record_fields) = value else {
            coerced.push((key, value));
            continue;
        };
        let mut coerced_fields = Vec::with_capacity(record_fields.len());
        for (field_name, field_value) in record_fields {
            let ty = fields
                .iter()
                .find(|(name, _)| *name == field_name)
                .map(|(_, ty)| *ty);
            let field_value = match ty {
                Some(ty) => coerce_json_numbers(field_value, ty, types)?,
                None => field_value,
            };
            coerced_fields.push((field_name, field_value));
        }
        coerced.push((key, Json::Object(coerced_fields)));
    }
    Ok(Json::Object(coerced))
}

/// `json`, with `{"enum": {"name", "variant", "payload": [...]}}`'s own
/// payload items coerced against the matching variant's own declared
/// payload types, by position.
fn coerce_enum(
    json: hollowscript::json::Json,
    variants: &[VariantDef],
    types: &TypeTable,
) -> Result<hollowscript::json::Json, String> {
    use hollowscript::json::Json;
    let Json::Object(tagged) = json else {
        return Ok(json);
    };
    let mut coerced = Vec::with_capacity(tagged.len());
    for (key, value) in tagged {
        if key != "enum" {
            coerced.push((key, value));
            continue;
        }
        let Json::Object(enum_fields) = value else {
            coerced.push((key, value));
            continue;
        };
        let variant_name = enum_fields
            .iter()
            .find(|(k, _)| k == "variant")
            .and_then(|(_, v)| match v {
                Json::String(s) => Some(s.clone()),
                _ => None,
            });
        let payload_types = variant_name
            .and_then(|name| variants.iter().find(|v| v.name == name))
            .map(|v| v.payload.clone());
        let mut coerced_fields = Vec::with_capacity(enum_fields.len());
        for (k, v) in enum_fields {
            if k != "payload" {
                coerced_fields.push((k, v));
                continue;
            }
            let Json::Array(items) = v else {
                coerced_fields.push((k, v));
                continue;
            };
            let items = match &payload_types {
                Some(payload_types) => {
                    let mut coerced_items = Vec::with_capacity(items.len());
                    for (i, item) in items.into_iter().enumerate() {
                        coerced_items.push(match payload_types.get(i) {
                            Some(&ty) => coerce_json_numbers(item, ty, types)?,
                            None => item,
                        });
                    }
                    coerced_items
                }
                None => items,
            };
            coerced_fields.push((k, Json::Array(items)));
        }
        coerced.push((key, Json::Object(coerced_fields)));
    }
    Ok(Json::Object(coerced))
}

/// `json`, as [`coerce_result_sugar`] handles `Result`'s own `{"Ok": v}`/
/// `{"Err": v}` sugar (not a `{"enum": {...}}`-tagged value at all): `v`
/// coerced against the matching variant's one payload type.
fn coerce_result_sugar(
    json: hollowscript::json::Json,
    variants: &[VariantDef],
    types: &TypeTable,
) -> Result<hollowscript::json::Json, String> {
    use hollowscript::json::Json;
    let Json::Object(fields) = json else {
        return Ok(json);
    };
    let mut coerced = Vec::with_capacity(fields.len());
    for (key, value) in fields {
        let variant_name = if key == OK {
            Some(OK)
        } else if key == ERR {
            Some(ERR)
        } else {
            None
        };
        let payload_ty = variant_name
            .and_then(|name| variants.iter().find(|v| v.name == name))
            .and_then(|v| v.payload.first().copied());
        let value = match payload_ty {
            Some(ty) => coerce_json_numbers(value, ty, types)?,
            None => value,
        };
        coerced.push((key, value));
    }
    Ok(Json::Object(coerced))
}

/// A cancellation token (section 11.7), created with [`hs_cancel_token_new`]
/// and shared between the thread that calls [`hs_load_ex`] or [`hs_call_ex`]
/// and the thread that cancels the operation with
/// [`hs_cancel_token_cancel`]: cancelling is the one call the specification
/// allows to run concurrently with the operation it stops.
///
/// A token is looked up by dereferencing the pointer the host still holds,
/// unlike an instance handle, because a token has no state the library owns
/// beyond the flag itself and the host's contract already keeps the pointer
/// alive for as long as an operation might read it.
#[derive(Debug)]
pub struct HsCancelToken(CancellationToken);

/// Creates a cancellation token, not yet set. The caller owns it and must
/// release it with [`hs_cancel_token_free`] once no operation can still be
/// reading it (after the [`hs_load_ex`] or [`hs_call_ex`] call it was passed
/// to has returned).
#[unsafe(no_mangle)]
pub extern "C" fn hs_cancel_token_new() -> *mut HsCancelToken {
    Box::into_raw(Box::new(HsCancelToken(CancellationToken::new())))
}

/// Sets `token`, so that the operation it was passed to stops at its next
/// safepoint (section 11.7) with the fault HS1006. Idempotent: cancelling an
/// already cancelled token, or one no operation is using any more, does
/// nothing harmful. This is the one function in this crate that is safe to
/// call from a thread other than the one running [`hs_load_ex`] or
/// [`hs_call_ex`], which is the whole point of a token: a host cancels a
/// long-running or runaway call from another thread while it is in flight.
///
/// # Safety
///
/// `token` must be null (a no-op) or a pointer [`hs_cancel_token_new`]
/// returned that has not yet been passed to [`hs_cancel_token_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_cancel_token_cancel(token: *const HsCancelToken) {
    if token.is_null() {
        return;
    }
    // SAFETY: `token` is non-null and, by the caller's contract, a pointer
    // `hs_cancel_token_new` returned that has not been freed. `cancel` takes
    // `&self` and only stores to an `AtomicBool` inside an `Arc`, which is
    // exactly what makes calling this from another thread, concurrently
    // with the operation reading the same token, race-free.
    unsafe { &*token }.0.cancel();
}

/// Releases a cancellation token. Freeing null is a no-op. The token must
/// not be freed while [`hs_load_ex`] or [`hs_call_ex`] might still read it;
/// freeing it while that call runs on another thread is undefined behaviour,
/// exactly as for any other owned allocation used across threads.
///
/// # Safety
///
/// `token` must be null, or a pointer [`hs_cancel_token_new`] returned that
/// has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_cancel_token_free(token: *mut HsCancelToken) {
    if token.is_null() {
        return;
    }
    // SAFETY: by the caller's contract, `token` is a pointer `Box::into_raw`
    // produced in `hs_cancel_token_new` and not yet freed.
    drop(unsafe { Box::from_raw(token) });
}

/// The token `cancel_token` names, cloned, or `None` for a null pointer.
///
/// This clones [`CancellationToken`]'s own inner `Arc` right here, at entry,
/// rather than handing back a reference that borrows the host's memory for
/// as long as the caller cares to hold it: [`hs_cancel_token_cancel`]'s own
/// contract makes freeing the token with [`hs_cancel_token_free`] from
/// another thread, while [`hs_load_ex`] or [`hs_call_ex`] is still running,
/// a real case a host can reach (the whole point of a token is that a host
/// capability can end the run and free the token it was given, from
/// whatever thread it runs the callback on, before the call that is still
/// reading it returns). A clone keeps the shared flag alive for exactly as
/// long as this one call needs it, once, at the start, regardless of what
/// the host does to its own pointer afterwards; only that clone, never the
/// host's own pointer, is read again after this returns.
///
/// # Safety
///
/// `cancel_token` must be null, or a pointer [`hs_cancel_token_new`]
/// returned that has not been freed, valid for this one dereference.
unsafe fn token_clone(cancel_token: *const HsCancelToken) -> Option<CancellationToken> {
    if cancel_token.is_null() {
        return None;
    }
    // SAFETY: non-null by the check above, and a live token by the caller's
    // contract; the clone taken here is the only part of `*cancel_token`
    // this function, or anything it hands the clone to, ever reads again.
    Some(unsafe { &*cancel_token }.0.clone())
}

/// One of the scalar types a host capability's parameters and result may be
/// (section 9.6's transferable types, restricted the same way [`hs_call`]'s
/// arguments already are: no list, map, record or enum, because a C caller
/// has no way to name one of those through this enum, only through the
/// module's own compiled type table (see this crate's own module
/// documentation, "Values across the boundary")).
///
/// [`Filter`](HsValueType::Filter) is the one exception, and is a
/// *parameter*-only type (Appendix A.1): a program's own filter literal is
/// the only way a `Filter` value is ever built, so it is never a
/// capability's `result` or `result_err`, only ever one of its `params`.
/// Declaring it there anyway is not rejected here; the checker itself
/// reports it (Chapter 9), the same as any other declared type it does not
/// accept in that position.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HsValueType {
    /// `Int`.
    Int = 0,
    /// `Float`.
    Float = 1,
    /// `Bool`.
    Bool = 2,
    /// `String`.
    String = 3,
    /// `Filter` (Appendix A), parameter-only. A capability receives it as
    /// `{"filter": "<source text>"}` in `args_json`; see this crate's own
    /// module documentation, "Values across the boundary", for the full
    /// encoding.
    Filter = 4,
}

/// `v`, as an [`HsValueType`], or `Err(())` when it names none of the five
/// (a C host wrote an out-of-range value, whether by mistake or by `memset`
/// leaving a field at whatever a stale buffer already held). Every field of
/// [`HsCapabilityFn`] this crate reads as an [`HsValueType`] is declared
/// `int32_t` in the header precisely so that Rust never transmutes an
/// arbitrary caller-supplied integer into an enum: reading an out-of-range
/// discriminant as a Rust enum is undefined behaviour, so
/// `read_capabilities` converts through this instead and answers
/// [`HsStatus::InvalidCapability`] on failure.
impl TryFrom<i32> for HsValueType {
    type Error = ();

    fn try_from(v: i32) -> Result<HsValueType, ()> {
        match v {
            0 => Ok(HsValueType::Int),
            1 => Ok(HsValueType::Float),
            2 => Ok(HsValueType::Bool),
            3 => Ok(HsValueType::String),
            4 => Ok(HsValueType::Filter),
            _ => Err(()),
        }
    }
}

/// What an [`HsCapabilityFn`] answers (section 8.2 and 9.6).
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HsResultShape {
    /// The function returns nothing; `result` and `result_err` are unread.
    None = 0,
    /// A single scalar of `result`.
    Value = 1,
    /// `Result<result, result_err>` (section 8.2): the callback answers a
    /// JSON object with one field, `"Ok"` holding a scalar of `result` or
    /// `"Err"` holding a scalar of `result_err`, instead of a bare scalar;
    /// see [`HsCapabilityFn::call`].
    Result = 2,
}

/// `v`, as an [`HsResultShape`], or `Err(())` when it names none of the
/// three, the same reason and the same use as [`TryFrom<i32> for
/// HsValueType`](HsValueType#impl-TryFrom<i32>-for-HsValueType).
impl TryFrom<i32> for HsResultShape {
    type Error = ();

    fn try_from(v: i32) -> Result<HsResultShape, ()> {
        match v {
            0 => Ok(HsResultShape::None),
            1 => Ok(HsResultShape::Value),
            2 => Ok(HsResultShape::Result),
            _ => Err(()),
        }
    }
}

/// A capability function a C host supplies to [`hs_load_ex`] (chapter 9): the
/// `uses` name it is bound to (and, for one function of a namespace
/// capability such as `clock`, that function's own name), its declared
/// parameter and result types, and the function pointer and user data the
/// VM calls through when the program calls it.
///
/// **A plain function capability** (`print`) has one [`HsCapabilityFn`] with
/// `function` null. **A namespace capability** (`clock`, with `now` and
/// `today`) has one entry per function, all sharing `name` and each naming
/// itself with `function`; [`hs_load_ex`] groups entries by `name` and
/// requires every entry of a group with more than one member to name a
/// `function`, since a namespace with an unnamed member has nothing to call
/// it by.
///
/// **Ownership.** `name`, `function` and `params` are read once, while
/// [`hs_load_ex`] builds the capability interface, and are not retained:
/// they may be freed or reused by the host as soon as that call returns.
/// `call` and `user_data` are retained for the life of the instance
/// [`hs_load_ex`] creates and must stay valid until it is disposed.
#[repr(C)]
#[derive(Debug)]
pub struct HsCapabilityFn {
    /// The capability's `uses` name, a non-null, null-terminated, valid
    /// UTF-8 C string.
    pub name: *const c_char,
    /// The function's own name, within a namespace capability's several
    /// (`"now"` of `clock`); null for a capability that is a single
    /// function, not a namespace.
    pub function: *const c_char,
    /// The parameter types, by position, each one of [`HsValueType`]'s own
    /// discriminants (`0` to `4`). Declared `int32_t`, not [`HsValueType`]
    /// itself, so that an out-of-range value a host writes (by mistake, or
    /// left over in an uninitialised buffer) is a value `read_capabilities`
    /// can refuse with [`HsStatus::InvalidCapability`] rather than a Rust
    /// enum read that is undefined behaviour when the discriminant is not
    /// one of the five. May be null only when `params_len` is zero. Ignored
    /// (may be left however the host likes, including null with
    /// `params_len` zero) when `params_json` is non-null and non-empty; see
    /// `params_json`.
    pub params: *const i32,
    /// The number of parameters `params` names. Ignored, the same way
    /// `params` is, when `params_json` is non-null and non-empty.
    pub params_len: usize,
    /// What the function answers, one of [`HsResultShape`]'s own
    /// discriminants (`0` to `2`); declared `int32_t` for the reason
    /// `params` is: see [`HsValueType`]'s own field above.
    pub result_shape: i32,
    /// The result type when `result_shape` is [`HsResultShape::Value`], or
    /// `Result`'s success type when it is [`HsResultShape::Result`], one of
    /// [`HsValueType`]'s own discriminants; declared `int32_t` for the same
    /// reason. Unread when `result_shape` is [`HsResultShape::None`], or when
    /// `result_json` is non-null and non-empty; see `result_json`.
    pub result: i32,
    /// `Result`'s failure type, the same encoding as `result`; read only
    /// when `result_shape` is [`HsResultShape::Result`] and `result_json` is
    /// null or empty.
    pub result_err: i32,
    /// The parameter types, by position, as a JSON array of type
    /// descriptions (see `declared_type`'s own documentation for the
    /// grammar: a scalar name, or a tagged object for a list, map, set,
    /// optional, record or enum), for a parameter `params`'s four scalars
    /// cannot express. A null pointer, or an empty string, means "use
    /// `params`/`params_len` instead" (the scalar-only path this field is
    /// additive to); a non-null, non-empty string takes over entirely, and
    /// `params`/`params_len` are then not read at all. Read once, while
    /// [`hs_load_ex`] builds the capability interface, and not retained
    /// past that call returning, exactly as `params` itself is not.
    pub params_json: *const c_char,
    /// The result type, as one `declared_type` description, for a result
    /// `result` cannot express (including `Result<T, E>` with a non-scalar
    /// `ok`/`err`, written `{"ok": T, "err": T}`, the same shape
    /// `declared_type` gives that case generally). A null pointer, or an
    /// empty string, means "use `result_shape`/`result`/`result_err`
    /// instead"; a non-null, non-empty string takes over entirely for the
    /// return type, and `result`/`result_err` are then not read (but
    /// `result_shape` must still not be [`HsResultShape::None`], or this
    /// function has no return type and `result_json` is ignored). Read
    /// once, the same as `params_json`.
    pub result_json: *const c_char,
    /// Called when the program calls this capability. `args_json` is a JSON
    /// array of the arguments, rendered the same way [`hs_call`]'s own
    /// arguments are read: `["a string", 3, true, null]`. The text of all the
    /// arguments of one call is bounded by the instance's output limit (not
    /// less than 64 KiB): an argument can share its sub-values and so be
    /// written out far larger than it is held, and a call whose arguments
    /// would pass the bound is not made, the run faulting with HS1004.
    ///
    /// On success the callback returns `0` and, unless `result_shape` is
    /// [`HsResultShape::None`], writes its answer to `*out_json`: a single
    /// JSON scalar for [`HsResultShape::Value`], or, for
    /// [`HsResultShape::Result`], a JSON object with exactly one field,
    /// either `{"Ok": <scalar>}` or `{"Err": <scalar>}`, exactly as a
    /// capability that is declared `Result<T, E>` answers success or an
    /// expected failure as a plain value the program handles (section 9.5's
    /// own example: `fs.read` answers `Err(IoError.NotFound(message))`
    /// rather than failing the call). On failure the callback returns a
    /// non-zero value and may write a message to `*out_error`; this is a
    /// different thing from a `Result`'s `Err`, and reaches the program as
    /// the fault HS1012 rather than a value it can pattern-match. Neither
    /// output pointer is read unless the corresponding condition holds.
    ///
    /// **Ownership of `*out_json` and `*out_error`.** The callback keeps
    /// ownership of whatever it points them at: this library reads the
    /// string during the call and copies it, and never frees it. The
    /// pointer only needs to stay valid until the callback itself returns;
    /// the host may free it, reuse a static buffer for it, or leave it
    /// pointing at a string literal, exactly as it prefers.
    pub call: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            args_json: *const c_char,
            out_json: *mut *mut c_char,
            out_error: *mut *mut c_char,
        ) -> c_int,
    >,
    /// Passed back to `call` unchanged; this library never reads through it.
    pub user_data: *mut c_void,
}

/// The four resource limits of one run (chapter 11), as [`hs_load_ex`]'s own
/// optional `limits` parameter: each governs the whole load, including the
/// initialisation run, and every call made afterwards on the instance it
/// produces. Read once, while `hs_load_ex` builds the instance, and not
/// retained past that call returning.
///
/// Passing null for `hs_load_ex`'s own `limits` parameter uses
/// [`hollowscript_core::Limits::default`], the specification's own
/// defaults (100,000,000 steps, 64 MiB memory, 16 MiB output, call depth
/// 1,000): before this type existed, that default was the *only* limit set
/// this ABI could produce, which left a host with no way to grant a program
/// fewer resources, or more, than that one fixed budget (chapter 12 lists
/// limits among a load's options; the C ABI is now at parity with the Rust
/// embedding API and `hollowscript-wasm`'s own `loadOptions` here).
///
/// Each field must be from 1 to 2^63 - 1 (section 11.1: a limit cannot be
/// switched off, and 0 is not a value); one outside that range is refused
/// as [`HsStatus::InvalidLimits`] before the load is attempted, with the
/// reason in `hs_load_ex`'s own `out_json`.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HsLimits {
    /// Units of work executed by one run (section 11.2).
    pub max_steps: u64,
    /// Bytes allocated by one run, and bytes retained by the instance
    /// afterwards (section 11.4).
    pub max_memory_bytes: u64,
    /// Bytes written by the `print` capability in one run (section 11.5).
    pub max_output_bytes: u64,
    /// Active function and closure calls (section 11.6).
    pub max_call_depth: u64,
}

/// `raw`, as a validated [`hollowscript_core::Limits`], or the reason it is
/// out of range.
///
/// # Errors
///
/// The reason, as plain text, when a field is 0 or above 2^63 - 1.
fn limits_of(raw: HsLimits) -> Result<hollowscript_core::Limits, String> {
    let limits = hollowscript_core::Limits {
        max_steps: raw.max_steps,
        max_memory_bytes: raw.max_memory_bytes,
        max_output_bytes: raw.max_output_bytes,
        max_call_depth: raw.max_call_depth,
    };
    limits.validate().map_err(|e| e.to_string())?;
    Ok(limits)
}

/// The parts of an [`HsCapabilityFn`] this crate keeps past the call that
/// declared it: everything [`HsCapabilityFn`] itself holds except `name` and
/// `function`, which become the key [`make_binder`]'s closure is looked up
/// by, and `params`, which becomes the declared shape rather than being
/// needed again.
struct CapabilityBinding {
    call: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *mut *mut c_char,
        *mut *mut c_char,
    ) -> c_int,
    user_data: usize,
    result_shape: HsResultShape,
    result: HsValueType,
    result_err: HsValueType,
    /// This capability's own declared result type, exactly as
    /// `read_capabilities` resolved it (from `result_json` or the scalar
    /// `result`/`result_err`/`result_shape` triple), together with the type
    /// table it indexes into: [`invoke_capability`] uses this to decode the
    /// callback's own JSON answer with the declared type directing a bare
    /// number to `Int` or `Float` (see `coerce_json_numbers`'s own
    /// documentation), the same as `parse_arguments` already does for
    /// `hs_call`'s own arguments.
    result_type: Option<TypeRef>,
    result_types: TypeTable,
}

// SAFETY: a `CapabilityBinding` is only ever called from inside the one
// `hs_load_ex`/`hs_call_ex` invocation using the instance it was bound into,
// which the instance registry's `busy` flag already serialises to one
// thread at a time; storing the host's function pointer and user data as
// plain integers/fn items here adds no access beyond what that call already has.
unsafe impl Send for CapabilityBinding {}

/// A type description, as [`HsCapabilityFn`]'s own `params_json` and
/// `result_json` write it: a bare string for one of the five scalars
/// (`"Int"`/`"Float"`/`"Bool"`/`"String"`/`"Filter"`, `Filter` valid only in
/// `params_json`, per [`TransferType::Filter`]'s own restriction, which the
/// checker enforces, not this parser), or a JSON object with exactly one
/// field naming a container kind and describing its own element type(s) the
/// same way, recursively:
///
/// - `{"list": T}`, `{"set": T}`, `{"optional": T}`
/// - `{"map": {"key": T, "value": T}}`
/// - `{"record": {"field": T, ...}}`, at least one field
/// - `{"enum": {"name": "E", "variants": {"V": [T, ...], ...}}}`, at least
///   one variant; a payload-less variant still needs its array, empty
/// - `{"ok": T, "err": T}`, `Result<T, E>` (section 8.2), kept as a bare
///   two-field object rather than a single-field tag
///
/// This is exactly `hollowscript-wasm`'s own `tooling::declared_type`
/// grammar, reproduced here rather than shared (that crate is not a
/// dependency of this one, and pulling it in for one function would be a
/// strange reason to), and it is the same tagged shape [`value_to_json`]'s
/// own value encoding uses, one level up: a capability whose declared
/// result type is `{"record": {"x": "Int", "y": "Int"}}` answers a value
/// tagged the same way, `{"record": {"x": 3, "y": 4}}`.
///
/// # Errors
///
/// The reason, as plain text, if `json` does not have one of the shapes
/// above.
fn declared_type(
    json: &hollowscript::json::Json,
    types: &mut TypeTable,
) -> Result<TypeRef, String> {
    use hollowscript::json::Json;
    match json {
        Json::String(name) => Ok(match name.as_str() {
            "Int" => types.add(TransferType::Int),
            "Float" => types.add(TransferType::Float),
            "Bool" => types.add(TransferType::Bool),
            "String" => types.add(TransferType::String),
            "Filter" => types.add(TransferType::Filter),
            other => return Err(format!("'{other}' is not a type this boundary can declare")),
        }),
        Json::Object(fields) => {
            let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            if let (Some(ok), Some(err)) = (field("ok"), field("err")) {
                if fields.len() != 2 {
                    return Err(
                        "a Result type's object must have exactly \"ok\" and \"err\"".to_owned(),
                    );
                }
                let ok_ty = declared_type(ok, types)?;
                let err_ty = declared_type(err, types)?;
                return Ok(result_type_of(types, ok_ty, err_ty));
            }
            let [(tag, body)] = <[(String, Json); 1]>::try_from(fields.clone()).map_err(|_| {
                "a type object must have exactly one field naming its kind \
                 (list, set, map, optional, record, enum), or be \"ok\"/\"err\""
                    .to_owned()
            })?;
            match tag.as_str() {
                "list" => {
                    let element = declared_type(&body, types)?;
                    Ok(types.add(TransferType::List(element)))
                }
                "set" => {
                    let element = declared_type(&body, types)?;
                    Ok(types.add(TransferType::Set(element)))
                }
                "optional" => {
                    let inner = declared_type(&body, types)?;
                    Ok(types.add(TransferType::Optional(inner)))
                }
                "map" => {
                    let Json::Object(map_fields) = &body else {
                        return Err("\"map\" needs an object with \"key\" and \"value\"".to_owned());
                    };
                    let get = |key: &str| map_fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
                    let (Some(key), Some(value)) = (get("key"), get("value")) else {
                        return Err("\"map\" needs \"key\" and \"value\"".to_owned());
                    };
                    let key_ty = declared_type(key, types)?;
                    let value_ty = declared_type(value, types)?;
                    Ok(types.add(TransferType::Map(key_ty, value_ty)))
                }
                "record" => {
                    let Json::Object(record_fields) = &body else {
                        return Err("\"record\" needs an object".to_owned());
                    };
                    if record_fields.is_empty() {
                        return Err("\"record\" needs at least one field".to_owned());
                    }
                    let mut built = Vec::with_capacity(record_fields.len());
                    for (name, field_ty) in record_fields {
                        built.push((name.clone(), declared_type(field_ty, types)?));
                    }
                    Ok(types.add(TransferType::Record(built)))
                }
                "enum" => {
                    let Json::Object(enum_fields) = &body else {
                        return Err("\"enum\" needs an object".to_owned());
                    };
                    let get =
                        |key: &str| enum_fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
                    let Some(Json::String(name)) = get("name") else {
                        return Err("\"enum\" needs a string \"name\"".to_owned());
                    };
                    let Some(Json::Object(variant_fields)) = get("variants") else {
                        return Err("\"enum\" needs an object \"variants\"".to_owned());
                    };
                    if variant_fields.is_empty() {
                        return Err("\"enum\" needs at least one variant".to_owned());
                    }
                    let mut variants = Vec::with_capacity(variant_fields.len());
                    for (variant_name, payload_json) in variant_fields {
                        let Json::Array(payload_items) = payload_json else {
                            return Err(format!(
                                "variant '{variant_name}' needs an array of payload types"
                            ));
                        };
                        let mut payload = Vec::with_capacity(payload_items.len());
                        for item in payload_items {
                            payload.push(declared_type(item, types)?);
                        }
                        variants.push(VariantDef {
                            name: variant_name.clone(),
                            payload,
                        });
                    }
                    // A fresh identity per C-declared enum (section 10.4): each
                    // `{"enum": ...}` in a host's JSON
                    // interface names a distinct declaration even if two
                    // happen to share a spelled name.
                    Ok(types.add_enum(name.clone(), variants))
                }
                other => Err(format!(
                    "'{other}' is not a type kind this boundary can declare \
                     (list, set, map, optional, record, enum)"
                )),
            }
        }
        _ => Err("a type must be a string or an object naming its kind".to_owned()),
    }
}

/// `Result<ok, err>` (section 8.2), built once per distinct call, the same
/// way [`declared_type`]'s own `{"ok": T, "err": T}` case and
/// `read_capabilities`'s scalar-only `result_type_ref` both do.
fn result_type_of(types: &mut TypeTable, ok: TypeRef, err: TypeRef) -> TypeRef {
    types.add(TransferType::Enum {
        // The language's own `Result` (section 8.2), the same declaration
        // wherever it is built: the reserved id, not a freshly minted one.
        id: hollowscript_core::transfer::EnumId {
            module: hollowscript_core::transfer::HOST_MODULE,
            index: hollowscript_core::transfer::RESULT_INDEX,
        },
        name: RESULT.to_owned(),
        variants: vec![
            VariantDef {
                name: OK.to_owned(),
                payload: vec![ok],
            },
            VariantDef {
                name: ERR.to_owned(),
                payload: vec![err],
            },
        ],
    })
}

/// One [`HsCapabilityFn`] entry, read out of the raw array (see
/// [`read_capabilities`]'s safety contract for what makes this valid).
struct RawEntry {
    name: String,
    function: Option<String>,
    params: Vec<HsValueType>,
    /// `HsCapabilityFn::params_json`, already read and UTF-8 checked;
    /// `None` for null or empty (use `params` instead).
    params_json: Option<String>,
    /// `HsCapabilityFn::result_json`, the same way.
    result_json: Option<String>,
    binding: CapabilityBinding,
}

/// `ptr` as `Some(text)`, or `None` for a null pointer or an empty string
/// (the two are treated alike everywhere this is used: "nothing declared
/// here", the scalar-only path is additive to, not replaced by).
///
/// # Safety
///
/// `ptr` must be null, or a valid, null-terminated C string for the
/// duration of this call.
unsafe fn optional_text(ptr: *const c_char) -> Result<Option<String>, HsStatus> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: non-null by the check above, and null-terminated, valid UTF-8
    // by the caller's contract.
    let Ok(text) = unsafe { CStr::from_ptr(ptr) }.to_str() else {
        return Err(HsStatus::InvalidUtf8);
    };
    if text.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(text.to_owned()))
    }
}

/// `t`, as a fresh entry of `types` (a scalar is small enough that a
/// capability interface having a duplicate entry per use, rather than one
/// shared entry reused, costs nothing the checker or the verifier cares
/// about; [`declared_type`]'s own container cases do not deduplicate
/// either, for the same reason).
fn scalar_type_ref(types: &mut TypeTable, t: HsValueType) -> TypeRef {
    match t {
        HsValueType::Int => types.add(TransferType::Int),
        HsValueType::Float => types.add(TransferType::Float),
        HsValueType::Bool => types.add(TransferType::Bool),
        HsValueType::String => types.add(TransferType::String),
        HsValueType::Filter => types.add(TransferType::Filter),
    }
}

/// `entry`'s own parameter types: [`declared_type`] over `entry.params_json`
/// if it is set, or `entry.params`'s scalars otherwise.
///
/// # Errors
///
/// The reason, as plain text, if `params_json` is set but is not valid
/// JSON, is not a JSON array, or one of its entries is not a
/// [`declared_type`] description.
fn params_of(entry: &RawEntry, types: &mut TypeTable) -> Result<Vec<TypeRef>, String> {
    let Some(json) = &entry.params_json else {
        return Ok(entry
            .params
            .iter()
            .map(|&t| scalar_type_ref(types, t))
            .collect());
    };
    let parsed = hollowscript::json::parse(json.trim())
        .map_err(|e| format!("params_json is not JSON: {}", e.message))?;
    let hollowscript::json::Json::Array(items) = parsed else {
        return Err("params_json must be a JSON array of type descriptions".to_owned());
    };
    items
        .iter()
        .map(|item| declared_type(item, types))
        .collect()
}

/// `entry`'s own return type: `None` for [`HsResultShape::None`]; otherwise
/// [`declared_type`] over `entry.result_json` if it is set (which may
/// itself describe `Result<T, E>`, via `{"ok": T, "err": T}`, taking over
/// entirely for `result_shape`'s own Value/Result distinction), or the
/// scalar `result`/`result_err`/`result_shape` combination otherwise.
///
/// # Errors
///
/// The reason, as plain text, as [`params_of`]'s.
fn ret_of(entry: &RawEntry, types: &mut TypeTable) -> Result<Option<TypeRef>, String> {
    if entry.binding.result_shape == HsResultShape::None {
        return Ok(None);
    }
    if let Some(json) = &entry.result_json {
        let parsed = hollowscript::json::parse(json.trim())
            .map_err(|e| format!("result_json is not JSON: {}", e.message))?;
        return Ok(Some(declared_type(&parsed, types)?));
    }
    Ok(Some(
        if entry.binding.result_shape == HsResultShape::Value {
            scalar_type_ref(types, entry.binding.result)
        } else {
            let ok = scalar_type_ref(types, entry.binding.result);
            let err = scalar_type_ref(types, entry.binding.result_err);
            result_type_of(types, ok, err)
        },
    ))
}

/// One member of a namespace capability's declared shape: its own name,
/// parameter types and result type.
///
/// # Errors
///
/// The reason, as plain text, as [`params_of`]'s or [`ret_of`]'s.
fn namespace_member(
    entry: &RawEntry,
    types: &mut TypeTable,
) -> Result<(String, Vec<TypeRef>, Option<TypeRef>), String> {
    let params = params_of(entry, types)?;
    let ret = ret_of(entry, types)?;
    Ok((entry.function.clone().unwrap_or_default(), params, ret))
}

/// Reads `capabilities[0..capabilities_len]` and builds the declared
/// [`CapabilityInterface`] together with the bindings [`make_binder`] turns
/// into [`HostFn`]s. Returns the status to report, with an explanatory
/// message for [`HsStatus::InvalidCapability`] (a malformed `params_json`
/// or `result_json`) and the empty string for every other status (a `name`
/// that is null or not UTF-8, a required pointer that is null, or a
/// namespace with an unnamed member; see [`HsCapabilityFn`]'s own
/// documentation for when a `function` name is required).
///
/// # Safety
///
/// `capabilities` must be null when `capabilities_len` is zero, or valid for
/// reads of `capabilities_len` consecutive [`HsCapabilityFn`] values
/// otherwise; every non-null `name`, `function`, `params`, `params_json` and
/// `result_json` field of each must be valid for the reads their own fields
/// describe.
unsafe fn read_capabilities(
    capabilities: *const HsCapabilityFn,
    capabilities_len: usize,
) -> Result<(CapabilityInterface, Vec<RawEntry>), (HsStatus, String)> {
    let mut types = TypeTable::new();

    // Never pre-size from `capabilities_len`: it is a caller-supplied value
    // this function has not yet validated (in particular has not yet
    // checked against a null `capabilities`), so `Vec::with_capacity` on it
    // directly could ask the allocator for an enormous or overflowing
    // request before the null check below ever runs.
    let mut raw = Vec::new();
    if capabilities_len > 0 {
        if capabilities.is_null() {
            return Err((HsStatus::NullArgument, String::new()));
        }
        // SAFETY: non-null by the check above, and valid for `capabilities_len`
        // reads by the caller's contract.
        let entries = unsafe { std::slice::from_raw_parts(capabilities, capabilities_len) };
        for entry in entries {
            if entry.name.is_null() {
                return Err((HsStatus::NullArgument, String::new()));
            }
            // SAFETY: non-null by the check above, and null-terminated, valid
            // UTF-8 by the caller's contract.
            let Ok(name) = unsafe { CStr::from_ptr(entry.name) }.to_str() else {
                return Err((HsStatus::InvalidUtf8, String::new()));
            };
            let function = if entry.function.is_null() {
                None
            } else {
                // SAFETY: non-null by the check above, and null-terminated,
                // valid UTF-8 by the caller's contract.
                let Ok(function) = unsafe { CStr::from_ptr(entry.function) }.to_str() else {
                    return Err((HsStatus::InvalidUtf8, String::new()));
                };
                Some(function.to_owned())
            };
            let params: Vec<HsValueType> = if entry.params_len == 0 {
                Vec::new()
            } else if entry.params.is_null() {
                return Err((HsStatus::NullArgument, String::new()));
            } else {
                // SAFETY: non-null by the check above, and valid for
                // `params_len` reads by the caller's contract.
                let raw_params =
                    unsafe { std::slice::from_raw_parts(entry.params, entry.params_len) };
                let mut converted = Vec::with_capacity(raw_params.len());
                for (position, &raw_value) in raw_params.iter().enumerate() {
                    // `params` is declared `int32_t` in the header exactly so
                    // that this is a checked conversion, never a transmute of
                    // whatever the host wrote into an `HsValueType`: an
                    // out-of-range discriminant is a malformed declaration,
                    // reported the same way an invalid params_json is, not
                    // undefined behaviour.
                    let Ok(value_type) = HsValueType::try_from(raw_value) else {
                        return Err((
                            HsStatus::InvalidCapability,
                            format!(
                                "params[{position}] of '{name}' ({raw_value}) is not one \
                                 of HsValueType's five values"
                            ),
                        ));
                    };
                    converted.push(value_type);
                }
                converted
            };
            // SAFETY: as this function's own contract requires.
            let params_json = match unsafe { optional_text(entry.params_json) } {
                Ok(text) => text,
                Err(status) => return Err((status, String::new())),
            };
            // SAFETY: as above.
            let result_json = match unsafe { optional_text(entry.result_json) } {
                Ok(text) => text,
                Err(status) => return Err((status, String::new())),
            };
            let Some(call) = entry.call else {
                return Err((HsStatus::NullArgument, String::new()));
            };
            // The same checked conversion as `params`, for the three scalar
            // fields the header declares `int32_t` for the same reason.
            let Ok(result_shape) = HsResultShape::try_from(entry.result_shape) else {
                return Err((
                    HsStatus::InvalidCapability,
                    format!(
                        "result_shape of '{name}' ({}) is not one of HsResultShape's \
                         three values",
                        entry.result_shape
                    ),
                ));
            };
            let Ok(result) = HsValueType::try_from(entry.result) else {
                return Err((
                    HsStatus::InvalidCapability,
                    format!(
                        "result of '{name}' ({}) is not one of HsValueType's five values",
                        entry.result
                    ),
                ));
            };
            let Ok(result_err) = HsValueType::try_from(entry.result_err) else {
                return Err((
                    HsStatus::InvalidCapability,
                    format!(
                        "result_err of '{name}' ({}) is not one of HsValueType's five values",
                        entry.result_err
                    ),
                ));
            };
            raw.push(RawEntry {
                name: name.to_owned(),
                function,
                params,
                params_json,
                result_json,
                binding: CapabilityBinding {
                    call,
                    user_data: entry.user_data as usize,
                    result_shape,
                    result,
                    result_err,
                    // Resolved just below, once this entry exists to read
                    // `ret_of` back from: a placeholder that is never read
                    // this way, since it is always overwritten before
                    // `raw` is returned.
                    result_type: None,
                    result_types: TypeTable::new(),
                },
            });
            // `ret_of` needs the entry to exist (it reads `result_json` and
            // the scalar `result`/`result_err`/`result_shape` back off it),
            // so this runs immediately after pushing it, and patches the
            // placeholder above with what every later capability call
            // through this same entry needs to decode its own answer with
            // the declared type directing it (`coerce_json_numbers`'s own
            // documentation): the result type, and a snapshot of `types` at
            // this point, which already holds every entry that type can
            // reference (nothing later in the loop can be one of them).
            let resolved = raw
                .last()
                .map(|just_pushed| ret_of(just_pushed, &mut types));
            if let Some(resolved) = resolved {
                let ret = resolved.map_err(|message| (HsStatus::InvalidCapability, message))?;
                let types_snapshot = types.clone();
                if let Some(last) = raw.last_mut() {
                    last.binding.result_type = ret;
                    last.binding.result_types = types_snapshot;
                }
            }
        }
    }

    // Group by `name`, preserving the order entries first appear in: a
    // linear scan rather than a map, since a host declares at most a
    // handful of capabilities and this keeps the checker's capability
    // order (which is otherwise arbitrary, but stable within one call)
    // matching the order the host wrote them in.
    let mut decls = Vec::new();
    let mut order: Vec<String> = Vec::new();
    for entry in &raw {
        if !order.contains(&entry.name) {
            order.push(entry.name.clone());
        }
    }
    for name in &order {
        let members: Vec<&RawEntry> = raw.iter().filter(|e| &e.name == name).collect();
        let shape = if let [only] = members.as_slice() {
            if only.function.is_none() {
                let params = params_of(only, &mut types)
                    .map_err(|message| (HsStatus::InvalidCapability, message))?;
                let ret = ret_of(only, &mut types)
                    .map_err(|message| (HsStatus::InvalidCapability, message))?;
                CapabilityShape::Function { params, ret }
            } else {
                let member = namespace_member(only, &mut types)
                    .map_err(|message| (HsStatus::InvalidCapability, message))?;
                CapabilityShape::Namespace(vec![member])
            }
        } else {
            let mut functions = Vec::with_capacity(members.len());
            for member in &members {
                if member.function.is_none() {
                    // A namespace with more than one member has nothing to
                    // call an unnamed one by.
                    return Err((HsStatus::NullArgument, String::new()));
                }
                functions.push(
                    namespace_member(member, &mut types)
                        .map_err(|message| (HsStatus::InvalidCapability, message))?,
                );
            }
            CapabilityShape::Namespace(functions)
        };
        decls.push(CapabilityDecl {
            name: name.clone(),
            shape,
        });
    }

    Ok((
        CapabilityInterface {
            capabilities: decls,
            types,
        },
        raw,
    ))
}

/// Builds the [`hollowscript::instance::CapabilityBinder`] `hs_load_ex`
/// passes to [`load`]: looks `(name, function)` up among `bindings` and,
/// for a match, boxes an [`HostFn`] that marshals arguments to JSON, calls
/// the host's function pointer, and marshals the result back.
fn make_binder(
    bindings: Vec<RawEntry>,
    output_limit: u64,
) -> impl FnMut(&str, Option<&str>) -> Option<Box<dyn HostFn>> {
    move |name, function| {
        let entry = bindings
            .iter()
            .find(|e| e.name == name && e.function.as_deref() == function)?;
        let call = entry.binding.call;
        let user_data = entry.binding.user_data as *mut c_void;
        let result_shape = entry.binding.result_shape;
        let result = entry.binding.result;
        let result_err = entry.binding.result_err;
        let result_type = entry.binding.result_type;
        let result_types = entry.binding.result_types.clone();
        let display_name = match &entry.function {
            Some(f) => format!("{name}.{f}"),
            None => name.to_owned(),
        };
        let bound: Box<dyn HostFn> = Box::new(
            move |args: &[Value], call_ctx: &mut HostCall<'_>| -> Result<Value, Fault> {
                invoke_capability(
                    call,
                    user_data,
                    result_shape,
                    result,
                    result_err,
                    result_type,
                    &result_types,
                    &display_name,
                    args,
                    call_ctx,
                    output_limit,
                )
            },
        );
        Some(bound)
    }
}

/// Calls a host capability function through its C function pointer, once
/// its arguments have been rendered to JSON, and turns its answer back into
/// a [`Value`]. The VM checks the result against the capability's declared
/// type after this returns (section 9.6), faulting HS1013 itself if it does
/// not fit, so this only needs to build a value of roughly the right shape.
#[allow(
    clippy::too_many_arguments,
    reason = "one per HsCapabilityFn field the callback needs"
)]
fn invoke_capability(
    call: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *mut *mut c_char,
        *mut *mut c_char,
    ) -> c_int,
    user_data: *mut c_void,
    result_shape: HsResultShape,
    result_ty: HsValueType,
    result_err_ty: HsValueType,
    declared_result: Option<TypeRef>,
    declared_types: &TypeTable,
    capability_name: &str,
    args: &[Value],
    call_ctx: &mut HostCall<'_>,
    output_limit: u64,
) -> Result<Value, Fault> {
    let args_json = render_capability_arguments(call_ctx, args, output_limit)?;
    let Ok(args_c) = CString::new(args_json) else {
        return Err(Fault::internal(
            "a capability argument contained an interior NUL byte",
        ));
    };
    let mut out_json: *mut c_char = std::ptr::null_mut();
    let mut out_error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `call` is the function pointer the host supplied to
    // `hs_load_ex`, which its own contract keeps valid for the life of the
    // instance; `user_data` is the host's own pointer, passed back
    // unchanged; `args_c` is a valid, null-terminated C string owned by this
    // stack frame for the length of the call; `out_json` and `out_error`
    // point at writable local storage.
    let status = unsafe {
        call(
            user_data,
            args_c.as_ptr(),
            &raw mut out_json,
            &raw mut out_error,
        )
    };
    let failed = |message: &str| {
        Fault::new(
            FaultCode::CapabilityFailed,
            &[("name", &capability_name), ("message", &message)],
        )
    };
    if status != 0 {
        // SAFETY: by `HsCapabilityFn::call`'s documented contract, a
        // non-null `out_error` the callback wrote points at a valid,
        // null-terminated string the host still owns and that stays valid
        // until the callback returns, which was just now.
        let message = unsafe { borrow_c_string(out_error) }
            .unwrap_or_else(|| "the capability failed".to_owned());
        return Err(failed(&message));
    }
    if result_shape == HsResultShape::None {
        return Ok(Value::None);
    }
    // SAFETY: as for `out_error` above, for `out_json`, which the callback's
    // contract requires to be set to a valid string whenever it returns `0`
    // and `result_shape` is not `None`.
    let Some(text) = (unsafe { borrow_c_string(out_json) }) else {
        return Err(failed("reported success but wrote no result"));
    };
    let Ok(parsed) = hollowscript::json::parse(text.trim()) else {
        return Err(failed("wrote a result that is not valid JSON"));
    };
    // `result_ty`/`result_err_ty` name nothing further here: `json_to_value`
    // already dispatches `{"Ok": ...}`/`{"Err": ...}` to `call_ctx.ok`/`err`
    // by the tag alone (see the module documentation's transferable-value
    // encoding), the same way it would for any other declared result type,
    // and the VM's own acceptance rule is what actually checks the answer
    // against `result_ty`/`result_err_ty` once this returns. `declared_result`
    // is different: it directs a bare JSON number to `Int` or `Float` before
    // that happens at all, which the acceptance rule cannot do after the
    // fact (see `coerce_json_numbers`'s own documentation).
    let _ = (result_shape, result_ty, result_err_ty);
    let parsed = match declared_result {
        Some(ty) => {
            coerce_json_numbers(parsed, ty, declared_types).map_err(|reason| failed(&reason))?
        }
        None => parsed,
    };
    json_to_value(call_ctx, parsed).map_err(|e| failed(&e.to_diagnostic().message))
}

/// Reads a C string a capability callback wrote and copies it, without
/// taking ownership of it (see [`HsCapabilityFn::call`]'s ownership note).
/// `None` for a null pointer or one that is not valid UTF-8.
///
/// # Safety
///
/// `ptr` must be null, or a valid, null-terminated string readable for the
/// length of this call.
unsafe fn borrow_c_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null by the check above, and a valid, null-terminated
    // string readable for the length of this call by the caller's contract.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// Renders a capability call's arguments the same way [`hs_call`]'s own
/// arguments are read: a JSON array, each element in the transferable-value
/// encoding this crate's module documentation defines.
///
/// The text of all the arguments together is bounded by `output_limit`, the
/// run's output limit (not less than 64 KiB), because an argument can share its sub-values and so
/// be written out far larger than the memory it occupies
/// (`t = Tree.Node(t, t)`, repeated, is fifty objects and 2^50 paths), and
/// the charge for a capability call's arguments (section 11.3) counts each
/// shared object once, so it does not bound the writing. The walk also stops
/// when the run's cancellation token is set.
///
/// # Errors
///
/// HS1004 when the arguments would be written as more than `output_limit`
/// bytes, and HS1006 when the run is cancelled meanwhile.
fn render_capability_arguments(
    call: &HostCall<'_>,
    args: &[Value],
    output_limit: u64,
) -> Result<String, Fault> {
    let mut poll = || call.is_cancelled();
    let mut budget = JsonBudget::new(output_limit, &mut poll);
    let mut out = String::from("[");
    for (i, &value) in args.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // Each argument is written to a string of its own, so the budget
        // counts it once, whole, and the commas are not charged.
        let mut text = String::new();
        value_to_json(call, value, &mut text, &mut budget)?;
        out.push_str(&text);
    }
    out.push(']');
    Ok(out)
}

/// The least the JSON for the arguments of one capability call is allowed to
/// be, whatever the output limit: 64 KiB. The output limit says how much
/// `print` may write and a host may set it very low; the arguments a host
/// legitimately receives should not fault because of that.
const MIN_ARGUMENT_BYTES: u64 = 64 * 1024;

/// What bounds the writing of a capability call's arguments: a limit on the
/// bytes produced, shared by every argument of the call, and a poll that is
/// asked whether to stop every 1,024 steps of the walk. Every piece the walk
/// writes is at least one byte, so a bound on the bytes is a bound on the
/// work, however much of the structure is shared. (`hollowscript-wasm` has
/// its own copy of this for the same reason it has its own copy of the
/// walk: the two crates do not depend on each other.)
struct JsonBudget<'a> {
    limit: u64,
    spent: u64,
    poll: &'a mut dyn FnMut() -> bool,
    frames: u32,
}

/// How many pieces of a rendering are written between two asks of the
/// budget's `poll`: the 1,024 that section 11.3 allows between safepoints.
const POLL_EVERY: u32 = 1_024;

impl<'a> JsonBudget<'a> {
    fn new(limit: u64, poll: &'a mut dyn FnMut() -> bool) -> JsonBudget<'a> {
        JsonBudget {
            limit,
            spent: 0,
            poll,
            frames: 0,
        }
    }

    /// Whether `produced` bytes written so far plus `extra` about to be
    /// written would pass the limit.
    fn exceeded(&self, produced: usize, extra: usize) -> bool {
        self.spent
            .saturating_add(produced as u64)
            .saturating_add(extra as u64)
            > self.limit
    }

    /// The fault for a rendering that has produced `produced` bytes and
    /// would pass the limit with its next piece.
    fn over(&self, produced: usize) -> Fault {
        Fault::output_limit(
            self.limit,
            self.spent.saturating_add(produced as u64).min(self.limit),
        )
    }

    /// Counts one piece of work, asking `poll` once in every [`POLL_EVERY`].
    fn tick(&mut self) -> Result<(), Fault> {
        self.frames = self.frames.wrapping_add(1);
        if self.frames % POLL_EVERY == 0 && (self.poll)() {
            return Err(Fault::cancelled());
        }
        Ok(())
    }
}

/// Appends `value` to `out` in the transferable-value JSON encoding this
/// crate's module documentation defines: a bare scalar for `Int`, `Float`,
/// `Bool`, `String` or `None`; a tagged object, `{"list": [...]}` and so
/// on, for a list, map, set, record or enum.
///
/// A container this call cannot read the contents of (a `Filter`, which is
/// never a capability result and so never reaches here as an argument
/// either, since it may only be a parameter; or a record or enum whose
/// shape or variant [`HostTypes`](hollowscript::HostTypes) has no name
/// for, which does not happen for a value a program built from its own
/// declared types, only one built to match a capability's declared type
/// that was never registered) renders as `null` rather than panicking or
/// silently dropping the whole call; this is a defect in the host's own
/// capability declaration, not something a script can trigger.
/// One step of [`value_to_json`]'s walk: either a value still to be
/// written, or a literal to append once its surrounding value's children
/// are done. See [`value_to_json`]'s own documentation for why this is a
/// stack, not recursion.
enum JsonFrame<'a> {
    Value(Value),
    Literal(&'static str),
    Owned(String),
    Field(&'a str),
}

/// Appends `value` to `out` as JSON, in the encoding this module's own
/// documentation defines, within `budget`.
///
/// **Iterative, not recursive.** A capability
/// argument can be a value of a declared recursive type (a program's own
/// `enum Tree { Leaf, Node(Tree) }`, passed as a capability parameter),
/// nested as deep as the heap allows and still a perfectly well-typed
/// value the checker and this crate's own argument acceptance both
/// accept: `hollowscript-vm`'s `check_capability_arguments` bounds a
/// corrupted argument's *shape*, not a legitimate one's *depth*, so a
/// walk that recurses once per level here would still overflow the host's
/// stack on an honestly-typed, merely deep, value. The walk keeps an
/// explicit stack of pending [`JsonFrame`]s instead, the same discipline
/// `hollowscript-wasm`'s own copy of this function uses for the same
/// reason (the two are not shared code: this crate and `hollowscript-wasm`
/// are kept free of a dependency on each other, per
/// `docs/design/dependencies.md`).
///
/// # Errors
///
/// HS1004 when the text would pass the budget, HS1006 when the budget's
/// poll says to stop. `out` holds what was written until then.
fn value_to_json(
    call: &HostCall<'_>,
    value: Value,
    out: &mut String,
    budget: &mut JsonBudget<'_>,
) -> Result<(), Fault> {
    let mut stack = vec![JsonFrame::Value(value)];
    while let Some(frame) = stack.pop() {
        budget.tick()?;
        match frame {
            JsonFrame::Literal(text) => out.push_str(text),
            JsonFrame::Owned(text) => out.push_str(&text),
            JsonFrame::Field(name) => {
                out.push('"');
                escape_json_string(name, out);
                out.push_str("\":");
            }
            JsonFrame::Value(value) => write_value_frame(call, value, out, &mut stack, budget)?,
        }
        if budget.exceeded(out.len(), 0) {
            return Err(budget.over(out.len()));
        }
    }
    budget.spent = budget.spent.saturating_add(out.len() as u64);
    Ok(())
}

/// Writes the leaf text of one value directly to `out`, or, for a value
/// with children, writes its opening punctuation and pushes its children
/// (and their own closing punctuation) onto `stack` for a later iteration
/// of [`value_to_json`]'s loop to expand: never calls itself, and never
/// calls [`value_to_json`].
fn write_value_frame<'a>(
    call: &'a HostCall<'_>,
    value: Value,
    out: &mut String,
    stack: &mut Vec<JsonFrame<'a>>,
    budget: &JsonBudget<'_>,
) -> Result<(), Fault> {
    match value {
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        // `{f:?}` always writes a decimal point (`3.0`, not `3`), which
        // keeps a `Float` argument distinguishable from an `Int` one in
        // the JSON a host callback reads, matching the language's own
        // rule that a `Float` is never confused with an `Int`.
        Value::Float(f) => {
            let _ = write!(out, "{f:?}");
        }
        Value::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Value::None => out.push_str("null"),
        Value::Heap(_) => {
            if let Some(text) = call.str_of(value) {
                // Checked before it is escaped, not after: one string can be
                // as long as the memory limit allows, and escaping can write
                // six bytes for each of its own.
                if budget.exceeded(out.len(), text.len().saturating_add(2)) {
                    return Err(budget.over(out.len()));
                }
                out.push('"');
                escape_json_string(text, out);
                out.push('"');
            } else if let Some(items) = call.list_of(value) {
                out.push_str(r#"{"list":["#);
                let frames = comma_separated(items.iter().copied().map(JsonFrame::Value));
                push_frames(stack, frames, "]}");
            } else if let Some(elements) = call.set_of(value) {
                out.push_str(r#"{"set":["#);
                // A set's elements are keys, not values (`Int`, `String`
                // or `Bool`, never a container), so `map_key_to_json`
                // cannot recurse into another value: safe to write
                // straight to `out`, the same as it always has been.
                for (i, &key) in elements.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if budget.exceeded(out.len(), 0) {
                        return Err(budget.over(out.len()));
                    }
                    map_key_to_json(call, key, out, 0, budget)?;
                }
                out.push_str("]}");
            } else if let Some(entries) = call.map_of(value) {
                out.push_str(r#"{"map":["#);
                let mut frames = Vec::with_capacity(entries.len() * 5);
                for (idx, entry) in entries.iter().enumerate() {
                    if idx > 0 {
                        frames.push(JsonFrame::Literal(","));
                    }
                    frames.push(JsonFrame::Owned({
                        let mut key = String::from("[");
                        map_key_to_json(call, entry.key, &mut key, out.len(), budget)?;
                        key
                    }));
                    frames.push(JsonFrame::Literal(","));
                    frames.push(JsonFrame::Value(entry.value));
                    frames.push(JsonFrame::Literal("]"));
                }
                push_frames(stack, frames, "]}");
            } else if let Some(fields) = call.record_of(value) {
                out.push_str(r#"{"record":{"#);
                let mut frames = Vec::with_capacity(fields.len() * 3);
                for (idx, (name, field_value)) in fields.into_iter().enumerate() {
                    if idx > 0 {
                        frames.push(JsonFrame::Literal(","));
                    }
                    frames.push(JsonFrame::Field(name));
                    frames.push(JsonFrame::Value(field_value));
                }
                push_frames(stack, frames, "}}");
            } else if let Some((enum_name, variant_name, payload)) = call.enum_of(value) {
                out.push_str(r#"{"enum":{"name":""#);
                escape_json_string(enum_name, out);
                out.push_str(r#"","variant":""#);
                escape_json_string(variant_name, out);
                out.push_str(r#"","payload":["#);
                let frames = comma_separated(payload.iter().copied().map(JsonFrame::Value));
                push_frames(stack, frames, "]}}");
            } else if let Some(source) = call.filter_of(value) {
                // Appendix A.1: a `Filter` is opaque to the program itself
                // and cannot be built at run time, taken apart or compared,
                // so this direction is the only one the encoding needs. A
                // host receives it as its filter source text (Appendix A.6
                // onward: parsing it against the host's own schema, and
                // evaluating it against the host's own records, is entirely
                // the host's responsibility), tagged so it is never
                // confused with an ordinary `String` argument.
                out.push_str(r#"{"filter":""#);
                escape_json_string(source, out);
                out.push_str("\"}");
            } else {
                // An unregistered record or enum shape: a value of a type
                // that reached this call without ever being registered in
                // `HostTypes`, which should not happen for a well-formed
                // module.
                out.push_str("null");
            }
        }
    }
    Ok(())
}

/// Interleaves `items` with a literal comma frame between each consecutive
/// pair (none before the first or after the last), in the forward
/// (left-to-right) order the finished JSON text reads in: a `Vec` built
/// this way is meant for [`push_frames`], not for pushing onto the stack
/// directly (a `Vec` is not a LIFO; `push_frames` reverses it once).
fn comma_separated<'a>(items: impl Iterator<Item = JsonFrame<'a>>) -> Vec<JsonFrame<'a>> {
    let mut frames = Vec::new();
    for item in items {
        if !frames.is_empty() {
            frames.push(JsonFrame::Literal(","));
        }
        frames.push(item);
    }
    frames
}

/// Pushes `closing` (a container's own closing punctuation), then `frames`
/// in reverse, onto `stack`. Popping the stack afterwards therefore writes
/// `frames` in their own forward order first, and `closing` last.
fn push_frames<'a>(
    stack: &mut Vec<JsonFrame<'a>>,
    frames: Vec<JsonFrame<'a>>,
    closing: &'static str,
) {
    stack.push(JsonFrame::Literal(closing));
    for frame in frames.into_iter().rev() {
        stack.push(frame);
    }
}

/// Appends `key` to `out`, as [`value_to_json`] would render the
/// equivalent `Value` (a map key is always `Int`, `String` or `Bool`).
/// `already` is the number of bytes of the same rendering written elsewhere
/// (`out` may be a scratch string appended to the rendering later), so that
/// a long key is refused against the budget before it is escaped.
///
/// # Errors
///
/// HS1004 when the key would pass `budget`.
fn map_key_to_json(
    call: &HostCall<'_>,
    key: hollowscript::MapKey,
    out: &mut String,
    already: usize,
    budget: &JsonBudget<'_>,
) -> Result<(), Fault> {
    match key {
        hollowscript::MapKey::Int(n) => {
            let _ = write!(out, "{n}");
        }
        hollowscript::MapKey::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        hollowscript::MapKey::Str(_) => {
            let text = call.key_str_of(key).unwrap_or("");
            let used = already.saturating_add(out.len());
            if budget.exceeded(used, text.len().saturating_add(2)) {
                return Err(budget.over(used));
            }
            out.push('"');
            escape_json_string(text, out);
            out.push('"');
        }
    }
    Ok(())
}

/// Builds a [`Value`] from `json`, in the transferable-value encoding this
/// crate's module documentation defines: the reverse of [`value_to_json`],
/// for the two directions values cross this boundary flowing in (an
/// [`hs_call`] argument, or a capability's own result). The VM's own
/// acceptance rule (section 9.6) checks the answer against the declared
/// type once this returns, faulting HS1013 (for a capability result) or
/// answering [`HsStatus::Refused`] (for an [`hs_call`] argument) if it does
/// not fit, so this only needs to build a value of roughly the right shape.
///
/// # Errors
///
/// The reason, as plain text, if `json` is not valid in this encoding (an
/// unknown tag, a tagged object with more than one field, a `map` entry
/// that is not a two-element `[key, value]` array, and so on), or the heap
/// is exhausted.
fn json_to_value(call: &mut HostCall<'_>, json: hollowscript::json::Json) -> Result<Value, Fault> {
    use hollowscript::json::Json;
    match json {
        Json::Null => Ok(Value::None),
        Json::Bool(b) => Ok(Value::Bool(b)),
        Json::Int(n) => Ok(Value::Int(n)),
        Json::Float(f) => Ok(Value::Float(f)),
        Json::String(text) => call.string(text),
        Json::Array(_) => Err(Fault::internal(
            "a bare array is not a value here; use {\"list\": [...]} or {\"set\": [...]}",
        )),
        Json::Object(fields) => json_tagged_to_value(call, fields),
    }
}

/// The tagged-object half of [`json_to_value`], split out because every
/// arm needs the same "exactly one field" check first.
///
/// Every error returned here, and by [`json_to_value`], is a [`Fault`]
/// built exactly once (by this function, or by the `HostCall` method an arm
/// calls), never a message that was already rendered through
/// [`Fault::to_diagnostic`] and then wrapped a second time: doing so would
/// double the "Internal error: " a template like [`Fault::internal`]'s adds.
/// A caller renders the `Fault` to text with `to_diagnostic().message` once,
/// at the boundary where it is reported.
fn json_tagged_to_value(
    call: &mut HostCall<'_>,
    fields: Vec<(String, hollowscript::json::Json)>,
) -> Result<Value, Fault> {
    use hollowscript::json::Json;
    let [(tag, body)] = <[(String, Json); 1]>::try_from(fields).map_err(|_| {
        Fault::internal(
            "a value that is a JSON object must have exactly one field, naming its kind \
             (list, set, map, record, enum, Ok or Err)",
        )
    })?;
    match tag.as_str() {
        "list" => {
            let Json::Array(items) = body else {
                return Err(Fault::internal("\"list\" needs an array"));
            };
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(json_to_value(call, item)?);
            }
            call.list(values)
        }
        "set" => {
            let Json::Array(items) = body else {
                return Err(Fault::internal("\"set\" needs an array"));
            };
            let mut keys = Vec::with_capacity(items.len());
            for item in items {
                let value = json_to_value(call, item)?;
                keys.push(
                    value_to_map_key(value).ok_or_else(|| {
                        Fault::internal("a set element must be Int, String or Bool")
                    })?,
                );
            }
            call.set(keys)
        }
        "map" => {
            let Json::Array(items) = body else {
                return Err(Fault::internal(
                    "\"map\" needs an array of [key, value] pairs",
                ));
            };
            let mut entries = Vec::with_capacity(items.len());
            for item in items {
                let Json::Array(pair) = item else {
                    return Err(Fault::internal(
                        "each map entry must be a [key, value] pair",
                    ));
                };
                let [key_json, value_json] = <[Json; 2]>::try_from(pair).map_err(|_| {
                    Fault::internal("each map entry must have exactly two elements")
                })?;
                let key_value = json_to_value(call, key_json)?;
                let key = value_to_map_key(key_value)
                    .ok_or_else(|| Fault::internal("a map key must be Int, String or Bool"))?;
                let value = json_to_value(call, value_json)?;
                entries.push(hollowscript::MapEntry { key, value });
            }
            call.map(entries)
        }
        "record" => {
            let Json::Object(field_entries) = body else {
                return Err(Fault::internal("\"record\" needs an object"));
            };
            let mut owned: Vec<(String, Value)> = Vec::with_capacity(field_entries.len());
            for (name, value_json) in field_entries {
                owned.push((name, json_to_value(call, value_json)?));
            }
            let fields: Vec<(&str, Value)> =
                owned.iter().map(|(name, v)| (name.as_str(), *v)).collect();
            call.record(fields)
        }
        "enum" => {
            let Json::Object(enum_fields) = body else {
                return Err(Fault::internal("\"enum\" needs an object"));
            };
            let field = |key: &str| enum_fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            let Some(Json::String(name)) = field("name") else {
                return Err(Fault::internal("\"enum\" needs a string \"name\""));
            };
            let Some(Json::String(variant)) = field("variant") else {
                return Err(Fault::internal("\"enum\" needs a string \"variant\""));
            };
            let payload_json = match field("payload") {
                None => Vec::new(),
                Some(Json::Array(items)) => items.clone(),
                Some(_) => return Err(Fault::internal("\"payload\" must be an array")),
            };
            let mut payload = Vec::with_capacity(payload_json.len());
            for item in payload_json {
                payload.push(json_to_value(call, item)?);
            }
            call.enum_value(name, variant, payload)
        }
        OK => {
            let inner = json_to_value(call, body)?;
            call.ok(inner)
        }
        ERR => {
            let inner = json_to_value(call, body)?;
            call.err(inner)
        }
        other => Err(Fault::internal(&format!(
            "'{other}' is not a value kind this ABI understands \
             (list, set, map, record, enum, Ok, Err)"
        ))),
    }
}

/// `value`, as a map/set key, or `None` if it is not `Int`, `String` or
/// `Bool` (section 2.8's key types).
fn value_to_map_key(value: Value) -> Option<hollowscript::MapKey> {
    match value {
        Value::Bool(b) => Some(hollowscript::MapKey::Bool(b)),
        Value::Int(n) => Some(hollowscript::MapKey::Int(n)),
        Value::Heap(h) => Some(hollowscript::MapKey::Str(h)),
        Value::None | Value::Float(_) => None,
    }
}

/// As [`hs_load`], but a C host may also declare and bind capabilities
/// (chapter 9) and supply a cancellation token that governs the whole load,
/// including the initialisation run of section 12.3's step 3.
///
/// A pair the checker accepts because it was declared here, but that the
/// program never actually calls, is simply never looked up: only the
/// capability, function pairs the compiled program calls are bound, exactly
/// as [`hs_load`]'s own binder does for the empty case.
///
/// # Safety
///
/// As [`hs_load`], for `source`, `entry_name`, `out_instance` and
/// `out_json`. `capabilities` must be null when `capabilities_len` is zero,
/// or valid for reads of `capabilities_len` consecutive [`HsCapabilityFn`]
/// values otherwise, each valid per that type's own documentation.
/// `cancel_token` must be null (no cancellation) or a pointer
/// [`hs_cancel_token_new`] returned that has not been freed. `limits` must
/// be null (the specification's own defaults) or a valid pointer to one
/// [`HsLimits`] value, read once, before this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_load_ex(
    source: *const c_char,
    entry_name: *const c_char,
    capabilities: *const HsCapabilityFn,
    capabilities_len: usize,
    cancel_token: *const HsCancelToken,
    limits: *const HsLimits,
    out_instance: *mut *mut HsInstance,
    out_json: *mut *mut c_char,
) -> c_int {
    guard(|| {
        // SAFETY: `out_instance` and `out_json`, if non-null, are valid
        // pointers to their respective types by the caller's contract; each
        // is cleared, in its own single-operation block, before any other
        // argument is checked, so neither is ever left untouched on an
        // error path (in particular, a malformed `capabilities` entry that
        // answers `InvalidCapability` below, which used to leave
        // `*out_instance` whatever the caller's own stack already held
        // there).
        unsafe { clear_instance_out(out_instance) };
        // SAFETY: as immediately above, for `out_json`.
        unsafe { clear_text_out(out_json) };
        if source.is_null() || entry_name.is_null() || out_instance.is_null() || out_json.is_null()
        {
            return HsStatus::NullArgument as c_int;
        }
        // SAFETY: both are non-null by the check above, and null-terminated and
        // readable by the caller's contract.
        let (Ok(source), Ok(entry_name)) = (
            unsafe { CStr::from_ptr(source) }.to_str(),
            unsafe { CStr::from_ptr(entry_name) }.to_str(),
        ) else {
            return HsStatus::InvalidUtf8 as c_int;
        };
        // SAFETY: `limits` is as this function's own contract requires:
        // null, or valid for one read of `HsLimits`, a `Copy` type this
        // function reads once and does not retain past this statement.
        let resolved_limits = match unsafe { limits.as_ref() } {
            None => hollowscript_core::Limits::default(),
            Some(&raw) => match limits_of(raw) {
                Ok(limits) => limits,
                Err(reason) => {
                    write_string(out_json, &reason);
                    return HsStatus::InvalidLimits as c_int;
                }
            },
        };
        // SAFETY: `capabilities`/`capabilities_len` are as this function's own
        // contract requires, which is exactly `read_capabilities`'s contract.
        let (capability_interface, bindings) =
            match unsafe { read_capabilities(capabilities, capabilities_len) } {
                Ok(built) => built,
                Err((status, message)) => {
                    if !message.is_empty() {
                        write_string(out_json, &message);
                    }
                    return status as c_int;
                }
            };
        // SAFETY: `cancel_token` is as this function's own contract requires,
        // which is exactly `token_clone`'s contract. Cloned here, at entry,
        // so a host freeing `cancel_token` from another thread while this
        // call runs (a case `hs_cancel_token_free`'s own contract allows: it
        // forbids freeing while the call *might* still read the host's
        // pointer, not while the flag is still needed) drops only the box
        // and one reference, never the flag this clone keeps alive. Shared,
        // by reference, between `precheck`'s own `Control` below and
        // `load`'s: both are read-only uses of the same clone, so a token
        // set while this call is in flight cancels whichever of the two is
        // running, not only `load`'s own internal check.
        let token = unsafe { token_clone(cancel_token) };
        // `load` runs its own check internally and would answer the same
        // `CheckFailed` diagnostics if there were an error; this crate runs
        // it once more, first, only so that a *clean* program's warnings
        // are not simply discarded, since `LoadOutcome::Instance` itself
        // carries no way to return them (see `precheck`'s own documentation).
        let mut precheck_control = Control::new(token.as_ref(), None);
        let warnings = match precheck_with(
            source,
            entry_name,
            &capability_interface,
            &mut precheck_control,
        ) {
            Err(diagnostics) => {
                write_string(
                    out_json,
                    &diagnostics_to_json(&diagnostics, entry_name, source),
                );
                return HsStatus::CheckFailed as c_int;
            }
            Ok(warnings) => warnings,
        };
        let options = Options {
            capabilities: capability_interface,
            limits: resolved_limits,
            ..Options::default()
        };
        let mut binder = make_binder(
            bindings,
            resolved_limits.max_output_bytes.max(MIN_ARGUMENT_BYTES),
        );
        let mut control = Control::new(token.as_ref(), None);
        let outcome = load(
            source,
            entry_name,
            &options,
            &mut NoLoader,
            &mut binder,
            &mut control,
        );
        match outcome {
            // Unreachable in practice; see `hs_load`'s own identical arm.
            LoadOutcome::CheckFailed(diagnostics) => {
                write_string(
                    out_json,
                    &diagnostics_to_json(&diagnostics, entry_name, source),
                );
                HsStatus::CheckFailed as c_int
            }
            LoadOutcome::Instance(instance) => finish_instance_load(
                instance,
                entry_name,
                source,
                warnings,
                out_instance,
                out_json,
            ),
        }
    })
}

/// As [`hs_call`], but a C host may also supply a cancellation token that
/// governs this one call: set it from another thread while the call is in
/// flight and the run stops at its next safepoint (section 11.7) with the
/// fault HS1006, reported here as [`HsStatus::Faulted`].
///
/// # Safety
///
/// As [`hs_call`], for `instance`, `name`, `arguments_json` and `out_text`.
/// `cancel_token` must be null (no cancellation) or a pointer
/// [`hs_cancel_token_new`] returned that has not been freed; it may safely
/// be cancelled, from another thread, at any point up to and including
/// while this call is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_call_ex(
    instance: *mut HsInstance,
    name: *const c_char,
    arguments_json: *const c_char,
    cancel_token: *const HsCancelToken,
    out_text: *mut *mut c_char,
) -> c_int {
    guard_with(
        || {
            // SAFETY: as in `hs_call`.
            unsafe { clear_text_out(out_text) };
            if instance.is_null()
                || name.is_null()
                || arguments_json.is_null()
                || out_text.is_null()
            {
                return HsStatus::NullArgument as c_int;
            }
            let mut in_use = match InUse::acquire(instance) {
                Ok(in_use) => in_use,
                Err(status) => {
                    write_string(out_text, refusal(status));
                    return status as c_int;
                }
            };
            let instance = in_use.instance();
            // SAFETY: both are non-null by the check above, and null-terminated and
            // readable by the contract.
            let (Ok(name), Ok(arguments)) = (
                unsafe { CStr::from_ptr(name) }.to_str(),
                unsafe { CStr::from_ptr(arguments_json) }.to_str(),
            ) else {
                return HsStatus::InvalidUtf8 as c_int;
            };
            // As `hs_call`: `parse_arguments` releases whatever it rooted
            // itself on its own error paths, and every path below releases
            // `arguments` once it has succeeded.
            let arguments = match parse_arguments(instance, name, arguments) {
                Ok(arguments) => arguments,
                Err(reason) => {
                    write_string(out_text, &reason);
                    return HsStatus::Refused as c_int;
                }
            };
            // SAFETY: `cancel_token` is as this function's own contract
            // requires, which is exactly `token_clone`'s contract; see its
            // own documentation for why this clones rather than borrows.
            let token = unsafe { token_clone(cancel_token) };
            let mut control = Control::new(token.as_ref(), None);
            match instance.call(name, &arguments, &mut control) {
                Err(error) => {
                    instance.release_values();
                    write_string(out_text, &error.to_diagnostic().message);
                    HsStatus::Refused as c_int
                }
                Ok(CallOutcome::Faulted(fault)) => {
                    instance.release_values();
                    write_string(out_text, &fault.to_diagnostic().message);
                    HsStatus::Faulted as c_int
                }
                Ok(CallOutcome::Returned(None)) => {
                    instance.release_values();
                    write_string(out_text, "");
                    HsStatus::Ok as c_int
                }
                Ok(CallOutcome::Returned(Some(value))) => {
                    let rendered = instance.render(value);
                    instance.release_values();
                    write_string(out_text, &rendered);
                    HsStatus::Ok as c_int
                }
                Ok(CallOutcome::Suspended) => {
                    instance.release_values();
                    write_string(out_text, "the run paused at a capability call");
                    HsStatus::Refused as c_int
                }
            }
        },
        || end_after_panic(instance),
    )
}

/// A minimal, dependency-free JSON array of diagnostics, carrying what the
/// facade's own [`hollowscript_core::Diagnostic`] has: `severity`, `code`,
/// `message`, `file`, `line`, 1-based `column` in code points (as the
/// specification itself defines it), `spanStartUtf16`/`spanEndUtf16` (the
/// half-open byte span [`hollowscript_core::source::Span`] itself uses,
/// converted to UTF-16 code units, which is what a web text editor such as
/// Monaco counts an offset or a column in, stated explicitly by these two
/// field names so a caller never mistakes them for `column`'s own
/// code-point count) and `related` (further locations,
/// [`hollowscript_core::Diagnostic::related`], the same shape one level
/// down, without its own `related`). `source`/`entry_name` are the ones the
/// call this answers for was given: this ABI takes source text, not a file
/// system root, so every diagnostic's own `location.file` is `entry_name`
/// (an import is always "not found", HS0601, with no other file to be in);
/// a location field is `null` when there is no location at all (the host
/// errors HS1201 to HS1205, which answer a host operation, not a position
/// in a program) or, defensively, when `location.file` is not `entry_name`
/// after all. Every field is either a fixed word, a decimal integer, or
/// text escaped by [`escape_json_string`], so the result is always valid
/// JSON without a general-purpose encoder.
fn diagnostics_to_json(
    diagnostics: &[hollowscript_core::Diagnostic],
    entry_name: &str,
    source: &str,
) -> String {
    let mut out = String::from("[");
    for (i, d) in diagnostics.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_diagnostic(&mut out, d, entry_name, source);
    }
    out.push(']');
    out
}

/// Appends one diagnostic object to `out`; see [`diagnostics_to_json`]'s
/// own documentation for the shape.
fn push_diagnostic(
    out: &mut String,
    d: &hollowscript_core::Diagnostic,
    entry_name: &str,
    source: &str,
) {
    out.push_str(r#"{"severity":""#);
    out.push_str(d.severity.as_str());
    out.push_str(r#"","code":""#);
    out.push_str(&d.code.to_text());
    out.push_str(r#"","message":""#);
    escape_json_string(&d.message, out);
    out.push_str("\",");
    push_location_fields(out, d.location.as_ref(), entry_name, source);
    out.push_str(r#","related":["#);
    for (i, r) in d.related.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        push_location_fields(out, Some(&r.location), entry_name, source);
        out.push_str(r#","message":""#);
        escape_json_string(&r.message, out);
        out.push('"');
        out.push('}');
    }
    out.push_str("]}");
}

/// Appends `"file":...,"line":...,"column":...,"spanStartUtf16":...,"spanEndUtf16":...`
/// (no leading comma, `{` or trailing `}`, so [`push_diagnostic`] can use this for
/// both a diagnostic's own location and each of its `related` ones) for
/// `location`, or the same keys with `null` values when it is `None` or
/// names a file other than `entry_name` (never happens through this ABI
/// today, since an import is always refused before it could add one, but
/// this must not panic or convert against the wrong text either way).
fn push_location_fields(
    out: &mut String,
    location: Option<&hollowscript_core::Location>,
    entry_name: &str,
    source: &str,
) {
    use std::fmt::Write as _;
    let Some(location) = location else {
        out.push_str(
            r#""file":null,"line":null,"column":null,"spanStartUtf16":null,"spanEndUtf16":null"#,
        );
        return;
    };
    out.push_str(r#""file":""#);
    escape_json_string(&location.file, out);
    out.push_str(r#"","line":"#);
    let _ = write!(out, "{}", location.line);
    out.push_str(r#","column":"#);
    let _ = write!(out, "{}", location.column);
    if &*location.file == entry_name {
        let start = utf16_offset(source, location.span.start);
        let end = utf16_offset(source, location.span.end);
        let _ = write!(out, r#","spanStartUtf16":{start},"spanEndUtf16":{end}"#);
    } else {
        out.push_str(r#","spanStartUtf16":null,"spanEndUtf16":null"#);
    }
}

/// The number of UTF-16 code units the first `byte_offset` bytes of `text`
/// encode as. A web text editor (Monaco among them) counts a position in
/// UTF-16 code units, not bytes (`hollowscript_core::source::Span`'s own
/// unit) or code points (`Location::column`'s own unit), so a diagnostic's
/// byte span needs this conversion before a page can use it to place a
/// squiggle. `byte_offset` past `text`'s own length, or one that does not
/// fall on a `char` boundary (should not happen for a diagnostic's own
/// span, but this must not panic either way), is walked back to the
/// nearest boundary at or before it first.
fn utf16_offset(text: &str, byte_offset: u32) -> u32 {
    let mut boundary = (byte_offset as usize).min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let Some(prefix) = text.get(..boundary) else {
        return 0;
    };
    let units: usize = prefix.chars().map(char::len_utf16).sum();
    u32::try_from(units).unwrap_or(u32::MAX)
}

/// A minimal, dependency-free JSON document for a formatted file:
/// `{"output":[...lines...]|null,"diagnostics":[...]}`, matching
/// `corpus/FORMAT.md` and the CLI's `format --json` mode: `output` is an
/// array of the formatted text's lines split on `\n` (so a trailing newline
/// in the text is a trailing empty string in the array), or `null` when the
/// formatter refused. `diagnostics_to_json` encodes the array.
fn format_to_json(
    output: Option<&str>,
    diagnostics: &[hollowscript_core::Diagnostic],
    entry_name: &str,
    source: &str,
) -> String {
    let mut out = String::from(r#"{"output":"#);
    match output {
        None => out.push_str("null"),
        Some(text) => {
            out.push('[');
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                escape_json_string(line, &mut out);
                out.push('"');
            }
            out.push(']');
        }
    }
    out.push_str(r#","diagnostics":"#);
    out.push_str(&diagnostics_to_json(diagnostics, entry_name, source));
    out.push('}');
    out
}

/// Appends the JSON string-literal encoding of `text` (without the
/// surrounding quotes) to `out`.
fn escape_json_string(text: &str, out: &mut String) {
    use std::fmt::Write as _;
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HsCancelToken, HsCapabilityFn, HsInstance, HsLimits, HsResultShape, HsStatus, HsValueType,
        escape_json_string, hs_call, hs_call_ex, hs_cancel_token_cancel, hs_cancel_token_free,
        hs_cancel_token_new, hs_check_json, hs_dispose, hs_format_json, hs_load, hs_load_ex,
        hs_read, hs_string_free, hs_version, live,
    };
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Loads `source` through the C ABI, as a C caller would.
    fn load_instance(source: &str) -> (*mut HsInstance, String) {
        let source = CString::new(source).unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null, null-terminated and valid for
        // the length of this call, which is the contract of `hs_load`.
        let status = unsafe {
            hs_load(
                source.as_ptr(),
                name.as_ptr(),
                &raw mut instance,
                &raw mut json,
            )
        };
        let text = take_string(json);
        assert!(
            status == HsStatus::Ok as c_int
                || status == HsStatus::Faulted as c_int
                || instance.is_null(),
            // A load that faults (a top-level `1 / 0`, for one) still
            // produces a live, Faulted instance (`finish_instance_load`'s
            // own documentation): only `HsStatus::CheckFailed` leaves no
            // instance at all.
            "a check-failed load must leave no instance"
        );
        (instance, text)
    }

    /// As [`load_instance`], but through `hs_load_ex` with `capabilities`
    /// bound.
    fn load_instance_with_capabilities(
        source: &str,
        capabilities: &[HsCapabilityFn],
    ) -> (*mut HsInstance, String) {
        let source = CString::new(source).unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null and valid for the length of this
        // call, which is `hs_load_ex`'s own contract.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                capabilities.as_ptr(),
                capabilities.len(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut instance,
                &raw mut json,
            )
        };
        let text = take_string(json);
        assert!(
            status == HsStatus::Ok as c_int
                || status == HsStatus::Faulted as c_int
                || instance.is_null(),
            "a check-failed load must leave no instance"
        );
        (instance, text)
    }

    /// Reads a string the ABI handed back and releases it, as a C caller
    /// would.
    fn take_string(pointer: *mut c_char) -> String {
        if pointer.is_null() {
            return String::new();
        }
        // SAFETY: the pointer came from this crate and is null-terminated.
        let text = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: the same pointer, not yet freed.
        unsafe { hs_string_free(pointer) };
        text
    }

    /// The number of objects `instance`'s heap holds, read the same way
    /// [`InUse::acquire`] resolves the opaque handle to the real pointer
    /// `register` boxed: a handle is an identifier this crate looks up in
    /// [`LIVE`], never a pointer it dereferences directly (see
    /// [`HsInstance`]'s own documentation), so a test that wants to look
    /// inside the instance for something the public ABI does not expose
    /// must go through the same table every call does.
    fn heap_len(instance: *mut HsInstance) -> usize {
        let id = instance.addr();
        let table = live();
        let entry = table.get(&id).expect("the test's own handle is live");
        // SAFETY: `entry.instance.0` is the pointer `register` boxed for
        // this still-live handle; this test holds `LIVE`'s own lock for the
        // whole read, the same lock every ordinary call acquires the
        // instance through, and makes no concurrent call of its own.
        unsafe { entry.instance.0.as_ref() }.instance.heap_len()
    }

    fn call(instance: *mut HsInstance, name: &str, args: &str) -> (c_int, String) {
        let name = CString::new(name).unwrap();
        let args = CString::new(args).unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` is live, the two strings are null-terminated,
        // and `out` points at writable storage.
        let status = unsafe { hs_call(instance, name.as_ptr(), args.as_ptr(), &raw mut out) };
        (status, take_string(out))
    }

    /// The whole instance lifecycle of chapter 12, through the C ABI: load a
    /// program, call its exported functions, read its exported constants,
    /// and dispose of it.
    #[test]
    fn a_c_host_loads_calls_reads_and_disposes() {
        let (instance, json) = load_instance(concat!(
            "export fn twice(n: Int): Int {\n",
            "    return n * 2\n",
            "}\n",
            "export fn greet(who: String): String {\n",
            "    return \"hello, ${who}\"\n",
            "}\n",
            "export fn nothing() {\n",
            "    return\n",
            "}\n",
            "export fn boom(): Int {\n",
            "    return div(1, 0)\n",
            "}\n",
            "export constant answer = 42\n",
        ));
        assert!(!instance.is_null(), "the program loads: {json}");
        assert_eq!(json, "[]");

        assert_eq!(call(instance, "twice", "[21]"), (0, "Int(42)".to_owned()));
        assert_eq!(
            call(instance, "greet", r#"["world"]"#),
            (0, r#"String("hello, world")"#.to_owned())
        );
        assert_eq!(call(instance, "nothing", "[]"), (0, String::new()));

        let mut out: *mut c_char = std::ptr::null_mut();
        let name = CString::new("answer").unwrap();
        // SAFETY: `instance` is live and the pointers are valid.
        let status = unsafe { hs_read(instance, name.as_ptr(), &raw mut out) };
        assert_eq!(status, 0);
        assert_eq!(take_string(out), "Int(42)");

        // A fault is reported as a fault, with its message, and does not
        // take the process with it. Section 12.2 then makes the instance
        // Faulted, so everything after this is refused, which is what the
        // second read checks.
        let (status, message) = call(instance, "boom", "[]");
        assert_eq!(status, HsStatus::Faulted as c_int);
        assert!(message.contains("Division by zero"), "{message}");
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: the handle is still live; only its state has changed.
        let status = unsafe { hs_read(instance, name.as_ptr(), &raw mut out) };
        assert_eq!(status, HsStatus::Refused as c_int);
        let _ = take_string(out);

        // SAFETY: `instance` is the handle `hs_load` produced, not yet
        // released.
        unsafe { hs_dispose(instance) };
    }

    /// A handle is looked up, never followed, so every way of holding a
    /// handle that is not live is refused: after `hs_dispose`, a second
    /// `hs_dispose`, and a value `hs_load` never gave out.
    #[test]
    fn a_handle_that_is_not_live_is_refused_not_followed() {
        let (instance, _) = load_instance("export fn one(): Int { return 1 }\n");
        assert_eq!(call(instance, "one", "[]"), (0, "Int(1)".to_owned()));
        // SAFETY: the handle is live.
        unsafe { hs_dispose(instance) };

        let (status, message) = call(instance, "one", "[]");
        assert_eq!(status, HsStatus::UnknownHandle as c_int);
        assert!(message.contains("not live"), "{message}");
        let mut out: *mut c_char = std::ptr::null_mut();
        let name = CString::new("one").unwrap();
        // SAFETY: the pointers are valid; the handle is deliberately stale.
        let status = unsafe { hs_read(instance, name.as_ptr(), &raw mut out) };
        assert_eq!(status, HsStatus::UnknownHandle as c_int);
        let _ = take_string(out);
        // SAFETY: disposing a stale handle is documented as a no-op.
        unsafe { hs_dispose(instance) };

        let made_up: *mut HsInstance = std::ptr::without_provenance_mut(0xdead_beef);
        assert_eq!(
            call(made_up, "one", "[]").0,
            HsStatus::UnknownHandle as c_int
        );
        // SAFETY: as above, for a handle that was never given out.
        unsafe { hs_dispose(made_up) };
    }

    /// A handle is never reused, so a stale one cannot reach a later
    /// instance that happens to have been allocated in the same place.
    #[test]
    fn a_stale_handle_never_names_a_later_instance() {
        let (first, _) = load_instance("export fn which(): Int { return 1 }\n");
        // SAFETY: the handle is live.
        unsafe { hs_dispose(first) };
        let (second, _) = load_instance("export fn which(): Int { return 2 }\n");
        assert_ne!(first, second);
        assert_eq!(
            call(first, "which", "[]").0,
            HsStatus::UnknownHandle as c_int
        );
        assert_eq!(call(second, "which", "[]"), (0, "Int(2)".to_owned()));
        // SAFETY: the handle is live.
        unsafe { hs_dispose(second) };
    }

    /// While a call holds an instance, a second call on it is refused
    /// rather than aliasing it, and `hs_dispose` waits for the call to
    /// finish rather than freeing what it is using.
    #[test]
    fn an_instance_in_use_is_neither_shared_nor_freed() {
        let (instance, _) = load_instance("export fn one(): Int { return 1 }\n");
        let held = super::InUse::acquire(instance).unwrap();
        assert_eq!(call(instance, "one", "[]").0, HsStatus::Refused as c_int);
        // SAFETY: the handle is live; the dispose is deferred.
        unsafe { hs_dispose(instance) };
        assert!(live().contains_key(&instance.addr()));
        drop(held);
        assert!(!live().contains_key(&instance.addr()));
        assert_eq!(
            call(instance, "one", "[]").0,
            HsStatus::UnknownHandle as c_int
        );
    }

    #[test]
    fn a_program_with_an_error_creates_no_instance() {
        let (instance, json) = load_instance("return missing\n");
        assert!(instance.is_null());
        assert!(json.contains("HS0205"), "{json}");
    }

    /// A program that faults during initialisation (a top-level `1 / 0`)
    /// still produces a live, Faulted instance, and `hs_load`'s own status
    /// and `*out_json` report that fault, rather than `HS_OK` with `"[]"`
    /// regardless (section 12.1's own promise that a load's fault is
    /// answered "as often as asked", which this ABI had no way to honour
    /// for a load's own fault before `finish_instance_load` existed). The
    /// handle is still the caller's to dispose of.
    #[test]
    fn a_program_that_faults_during_load_reports_the_fault() {
        let (instance, json) = load_instance("return 1 / 0\n");
        assert!(!instance.is_null(), "a Faulted instance is still live");
        assert!(json.contains("HS1001") || json.contains("error"), "{json}");

        // hs_load's own return status, not only the instance's later
        // behaviour, must say Faulted: re-run through the raw entry point
        // to check the status code directly.
        let source = CString::new("return 1 / 0\n").unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut raw_instance: *mut HsInstance = std::ptr::null_mut();
        let mut raw_json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null, null-terminated and valid for
        // the length of this call.
        let status = unsafe {
            hs_load(
                source.as_ptr(),
                name.as_ptr(),
                &raw mut raw_instance,
                &raw mut raw_json,
            )
        };
        let message = take_string(raw_json);
        assert_eq!(status, HsStatus::Faulted as c_int, "{message}");
        assert!(!raw_instance.is_null());

        // A Faulted instance is refused for a call, since it can run no
        // further code, but the handle is still valid to dispose of.
        assert_eq!(
            call(instance, "anything", "[]").0,
            HsStatus::Refused as c_int
        );
        // SAFETY: both handles are live.
        unsafe { hs_dispose(instance) };
        // SAFETY: as immediately above.
        unsafe { hs_dispose(raw_instance) };
    }

    /// A clean program's own warnings (HS0801, an unused `constant`) reach
    /// the host through `*out_json` on a successful load, rather than being
    /// silently dropped the way `LoadOutcome::CheckFailed`'s own diagnostics
    /// used to be the only ones this ABI ever reported.
    #[test]
    fn a_clean_load_still_reports_its_own_warnings() {
        let (instance, json) = load_instance("constant unused = 1\nreturn 1\n");
        assert!(!instance.is_null(), "{json}");
        assert!(json.contains("HS0801"), "{json}");
        assert!(json.contains(r#""severity":"warning""#), "{json}");
        // SAFETY: the handle is live.
        unsafe { hs_dispose(instance) };
    }

    #[test]
    fn an_unknown_export_and_a_bad_argument_count_are_refused() {
        let (instance, _) = load_instance("export fn twice(n: Int): Int { return n * 2 }\n");
        assert!(!instance.is_null());
        let refused = HsStatus::Refused as c_int;
        assert_eq!(call(instance, "nope", "[]").0, refused);
        assert_eq!(call(instance, "twice", "[]").0, refused);
        assert_eq!(call(instance, "twice", "not json").0, refused);
        assert_eq!(call(instance, "twice", "[[1]]").0, refused);
        // SAFETY: the handle is live and not yet released.
        unsafe { hs_dispose(instance) };
    }

    /// Every entry point refuses a null pointer rather than dereferencing
    /// it, and `hs_dispose` accepts one so a host can call it blindly.
    #[test]
    fn null_pointers_are_refused_not_dereferenced() {
        let null = HsStatus::NullArgument as c_int;
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut out: *mut c_char = std::ptr::null_mut();
        let name = CString::new("x").unwrap();
        // SAFETY: passing null is exactly what this call must refuse.
        let status = unsafe {
            hs_load(
                std::ptr::null(),
                name.as_ptr(),
                &raw mut instance,
                &raw mut out,
            )
        };
        assert_eq!(status, null);
        // SAFETY: as above, for a null instance handle.
        let status = unsafe {
            hs_call(
                std::ptr::null_mut(),
                name.as_ptr(),
                name.as_ptr(),
                &raw mut out,
            )
        };
        assert_eq!(status, null);
        // SAFETY: as above.
        let status = unsafe { hs_read(std::ptr::null_mut(), name.as_ptr(), &raw mut out) };
        assert_eq!(status, null);
        // SAFETY: `hs_dispose` documents that a null handle is a no-op.
        unsafe { hs_dispose(std::ptr::null_mut()) };
    }

    #[test]
    fn version_is_a_stable_static_string() {
        // SAFETY: `hs_version` returns a pointer into a `'static` C string
        // literal compiled into the binary, valid for the whole process.
        let text = unsafe { CStr::from_ptr(hs_version()) }.to_str().unwrap();
        // The crate's own version, so a release's C library can never report
        // a version other than the one its archive is named for.
        assert_eq!(text, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn check_json_reports_no_diagnostics_for_a_clean_program() {
        let source = CString::new("return 1").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `source` is a valid null-terminated C string owned by this
        // scope, and `out` is a valid pointer to a local `*mut c_char`.
        let status = unsafe { hs_check_json(source.as_ptr(), std::ptr::null(), &raw mut out) };
        assert_eq!(status, HsStatus::Ok as i32);
        assert!(!out.is_null());
        // SAFETY: `out` was just set by `hs_check_json` to a pointer it
        // returned, which is exactly what `CStr::from_ptr` here requires.
        let json = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        assert_eq!(json, "[]", "a clean program reports no diagnostics");
        // SAFETY: `out` is exactly the pointer `hs_check_json` returned,
        // freed here for the first and only time.
        unsafe { hs_string_free(out) };
    }

    #[test]
    fn check_json_reports_a_real_diagnostic_for_an_unresolved_name() {
        let source = CString::new("return missing").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `source` is a valid null-terminated C string owned by this
        // scope, and `out` is a valid pointer to a local `*mut c_char`.
        let status = unsafe { hs_check_json(source.as_ptr(), std::ptr::null(), &raw mut out) };
        assert_eq!(status, HsStatus::Ok as i32);
        assert!(!out.is_null());
        // SAFETY: `out` was just set by `hs_check_json` to a pointer it
        // returned, which is exactly what `CStr::from_ptr` here requires.
        let json = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        assert!(json.contains(r#""code":"HS0205""#), "{json}");
        assert!(json.contains(r#""severity":"error""#), "{json}");
        // SAFETY: `out` is exactly the pointer `hs_check_json` returned,
        // freed here for the first and only time.
        unsafe { hs_string_free(out) };
    }

    /// A diagnostic with a related location (a repeated declaration points
    /// back at the first one) must still be well-formed JSON: each `related`
    /// object once began with a stray comma, `{,"file"`, which no JSON
    /// parser accepts.
    #[test]
    fn check_json_writes_well_formed_json_for_a_related_location() {
        let source =
            CString::new("fn twice(): Int { return 1 }\nfn twice(): Int { return 2 }\n").unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: both strings are valid, null-terminated C strings owned by
        // this scope, and `out` is a valid pointer to a local `*mut c_char`.
        let status = unsafe { hs_check_json(source.as_ptr(), name.as_ptr(), &raw mut out) };
        assert_eq!(status, HsStatus::Ok as i32);
        let json = take_string(out);
        assert!(json.contains(r#""code":"HS0201""#), "{json}");
        assert!(json.contains(r#""related":[{"file":"main.hws""#), "{json}");
        assert!(!json.contains("{,"), "{json}");
        assert!(!json.contains(",,"), "{json}");
    }

    /// A diagnostic's own JSON carries what the facade's `Diagnostic` itself
    /// has: file, line, column, the span converted to UTF-16 code units
    /// (stated explicitly, `spanStartUtf16`/`spanEndUtf16`, since Monaco and
    /// every other web text editor counts a position that way, not in bytes
    /// or in code points), severity, code, message and `related` (empty
    /// here; HS0205 names none). The span converts through a multi-byte
    /// character (an emoji, four UTF-8 bytes and two UTF-16 code units, a
    /// surrogate pair) earlier on the line, so a byte-for-UTF-16 mixup would
    /// show up as a wrong offset here, not merely a missing field.
    #[test]
    fn check_json_reports_the_full_location_of_a_diagnostic() {
        let source = CString::new("return \"\u{1f600}\" == missing").unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: both strings are valid, null-terminated C strings owned by
        // this scope, and `out` is a valid pointer to a local `*mut c_char`.
        let status = unsafe { hs_check_json(source.as_ptr(), name.as_ptr(), &raw mut out) };
        assert_eq!(status, HsStatus::Ok as i32);
        let json = take_string(out);
        assert!(json.contains(r#""code":"HS0205""#), "{json}");
        assert!(json.contains(r#""file":"main.hws""#), "{json}");
        assert!(json.contains(r#""line":1"#), "{json}");
        assert!(json.contains(r#""column":"#), "{json}");
        // `return "` is 8 bytes/UTF-16 units, the emoji is 4 bytes but 2
        // UTF-16 units, then `" == ` is 5 more: `missing` starts at UTF-16
        // offset 8 + 2 + 5 = 15, not the byte offset 17 a byte-for-UTF-16
        // mixup would report instead.
        assert!(json.contains(r#""spanStartUtf16":15"#), "{json}");
        assert!(json.contains(r#""related":[]"#), "{json}");
    }

    #[test]
    fn null_arguments_are_refused_without_touching_memory() {
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: a null `source` is exactly the case this call is testing;
        // `hs_check_json` checks for it before dereferencing anything.
        let status = unsafe { hs_check_json(std::ptr::null(), std::ptr::null(), &raw mut out) };
        assert_eq!(status, HsStatus::NullArgument as i32);
        assert!(out.is_null());
        // SAFETY: freeing a null pointer is defined to do nothing.
        unsafe { hs_string_free(std::ptr::null_mut()) };
    }

    #[test]
    fn control_characters_are_escaped() {
        let mut out = String::new();
        escape_json_string("a\"b\\c\nd\u{1}", &mut out);
        assert_eq!(out, "a\\\"b\\\\c\\nd\\u0001");
    }

    #[test]
    fn format_json_formats_a_clean_source_with_no_checker_needed() {
        let source = CString::new("constant x=1").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `source` is a valid null-terminated C string owned by this
        // scope, and `out` is a valid pointer to a local `*mut c_char`.
        let status = unsafe { hs_format_json(source.as_ptr(), std::ptr::null(), &raw mut out) };
        assert_eq!(status, HsStatus::Ok as i32);
        assert!(!out.is_null());
        // SAFETY: `out` was just set by `hs_format_json` to a pointer it
        // returned, which is exactly what `CStr::from_ptr` here requires.
        let json = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        assert!(json.contains(r#""output":["constant x = 1",""]"#), "{json}");
        assert!(json.contains(r#""diagnostics":[]"#), "{json}");
        // SAFETY: `out` is exactly the pointer `hs_format_json` returned,
        // freed here for the first and only time.
        unsafe { hs_string_free(out) };
    }

    #[test]
    fn format_json_reports_null_output_on_a_syntax_error() {
        let source = CString::new("constant x =").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: as above.
        let status = unsafe { hs_format_json(source.as_ptr(), std::ptr::null(), &raw mut out) };
        assert_eq!(status, HsStatus::Ok as i32);
        // SAFETY: as above.
        let json = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
        assert!(json.contains(r#""output":null"#), "{json}");
        assert!(!json.contains(r#""diagnostics":[]"#), "{json}");
        // SAFETY: as above.
        unsafe { hs_string_free(out) };
    }

    /// A panic does not cross the boundary. `panic = "unwind"` is set for
    /// every profile precisely so that this can be true; a panic unwinding
    /// out of an `extern "C"` function is undefined behaviour.
    #[test]
    fn a_panic_is_stopped_at_the_boundary() {
        // The panic hook is silenced for the length of the test, so a
        // deliberate panic does not print a backtrace that reads like a
        // failure.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let status = super::guard(|| panic!("a defect, deliberately"));
        let with_instance = super::guard_with(|| panic!("another"), || {});
        std::panic::set_hook(previous);
        assert_eq!(status, HsStatus::Panicked as c_int);
        assert_eq!(with_instance, HsStatus::Panicked as c_int);
    }

    /// A capability callback that captures its argument, for a program that
    /// `uses print`. `user_data` is the address of a `Mutex<String>` the
    /// test owns; the callback locks it and appends the raw JSON argument
    /// array it was given.
    unsafe extern "C" fn capture_print(
        user_data: *mut c_void,
        args_json: *const c_char,
        _out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: `user_data` is the `Mutex<String>` pointer the test passed
        // to `hs_load_ex`, which outlives the call this callback runs
        // inside of.
        let buffer = unsafe { &*user_data.cast::<Mutex<String>>() };
        // SAFETY: `args_json` is a valid, null-terminated C string for the
        // length of this call, per `HsCapabilityFn::call`'s contract.
        let text = unsafe { CStr::from_ptr(args_json) }.to_string_lossy();
        buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_str(&text);
        0
    }

    /// A capability callback with a result: squares its one `Int` argument.
    /// Its answer is a leaked, static-lifetime C string, which is within
    /// [`HsCapabilityFn::call`]'s ownership contract (a host may point
    /// `*out_json` at a buffer that outlives the call by as much as it likes).
    unsafe extern "C" fn square(
        _user_data: *mut c_void,
        args_json: *const c_char,
        out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: as `capture_print`'s `args_json`.
        let text = unsafe { CStr::from_ptr(args_json) }.to_string_lossy();
        let n: i64 = text
            .trim_matches(|c: char| c == '[' || c == ']')
            .trim()
            .parse()
            .unwrap_or(0);
        let Ok(result) = CString::new((n * n).to_string()) else {
            return 1;
        };
        let leaked: &'static CStr = Box::leak(result.into_boxed_c_str());
        // SAFETY: `out_json` points at writable storage the caller
        // provided, per `HsCapabilityFn::call`'s contract.
        unsafe { *out_json = leaked.as_ptr().cast_mut() };
        0
    }

    /// A failing capability callback, to check that its message reaches the
    /// program as the fault HS1012.
    unsafe extern "C" fn always_fails(
        _user_data: *mut c_void,
        _args_json: *const c_char,
        _out_json: *mut *mut c_char,
        out_error: *mut *mut c_char,
    ) -> c_int {
        let message = c"the host refused, deliberately";
        // SAFETY: `out_error` points at writable storage the caller
        // provided; `message` is a `'static` string literal, well within
        // the contract that it need only stay valid until this callback
        // returns.
        unsafe { *out_error = message.as_ptr().cast_mut() };
        1
    }

    /// A C host can declare and bind a capability interface (chapter 9):
    /// `uses print` and a host-defined function both work, arguments and
    /// results cross as JSON the same way [`hs_call`]'s already do, and a
    /// callback that fails reaches the program as HS1012.
    #[test]
    fn a_c_host_binds_and_calls_capabilities() {
        let captured = Box::new(Mutex::new(String::new()));
        let captured_ptr: *mut c_void = Box::into_raw(captured).cast();

        let print_params = [HsValueType::String as i32];
        let square_params = [HsValueType::Int as i32];
        let capabilities = [
            HsCapabilityFn {
                name: c"print".as_ptr(),
                function: std::ptr::null(),
                params: print_params.as_ptr(),
                params_len: print_params.len(),
                result_shape: HsResultShape::None as i32,
                result: HsValueType::Int as i32,
                result_err: HsValueType::Int as i32,
                call: Some(capture_print),
                params_json: std::ptr::null(),
                result_json: std::ptr::null(),
                user_data: captured_ptr,
            },
            HsCapabilityFn {
                name: c"square".as_ptr(),
                function: std::ptr::null(),
                params: square_params.as_ptr(),
                params_len: square_params.len(),
                result_shape: HsResultShape::Value as i32,
                result: HsValueType::Int as i32,
                result_err: HsValueType::Int as i32,
                call: Some(square),
                params_json: std::ptr::null(),
                result_json: std::ptr::null(),
                user_data: std::ptr::null_mut(),
            },
            HsCapabilityFn {
                name: c"refuse".as_ptr(),
                function: std::ptr::null(),
                params: std::ptr::null(),
                params_len: 0,
                result_shape: HsResultShape::None as i32,
                result: HsValueType::Int as i32,
                result_err: HsValueType::Int as i32,
                call: Some(always_fails),
                params_json: std::ptr::null(),
                result_json: std::ptr::null(),
                user_data: std::ptr::null_mut(),
            },
        ];

        let source = CString::new(concat!(
            "uses print\n",
            "uses square\n",
            "uses refuse\n",
            "export fn run(): Int {\n",
            "    effect print(\"hello\")\n",
            "    return effect square(21)\n",
            "}\n",
            "export fn boom() {\n",
            "    effect refuse()\n",
            "}\n",
        ))
        .unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null (or, for `params` with a zero
        // length, may be non-null and simply unread) and valid for the
        // length of this call.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                capabilities.as_ptr(),
                capabilities.len(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut instance,
                &raw mut json,
            )
        };
        let diagnostics = take_string(json);
        assert_eq!(status, HsStatus::Ok as c_int, "{diagnostics}");
        assert!(!instance.is_null());

        let run = CString::new("run").unwrap();
        let args = CString::new("[]").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` is live; the strings are valid for this call;
        // a null token means no cancellation.
        let status = unsafe {
            hs_call_ex(
                instance,
                run.as_ptr(),
                args.as_ptr(),
                std::ptr::null(),
                &raw mut out,
            )
        };
        let rendered = take_string(out);
        assert_eq!(status, HsStatus::Ok as c_int, "{rendered}");
        assert_eq!(rendered, "Int(441)", "square(21) should answer 441");

        // SAFETY: the pointer is the one this test leaked into `user_data`
        // above, reclaimed here for the first and only time.
        let captured = unsafe { Box::from_raw(captured_ptr.cast::<Mutex<String>>()) };
        let printed = captured
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(printed.contains("hello"), "{printed}");

        let boom = CString::new("boom").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: as the call to `run` above.
        let status = unsafe {
            hs_call_ex(
                instance,
                boom.as_ptr(),
                args.as_ptr(),
                std::ptr::null(),
                &raw mut out,
            )
        };
        let message = take_string(out);
        assert_eq!(status, HsStatus::Faulted as c_int, "{message}");
        assert!(
            message.contains("the host refused, deliberately"),
            "{message}"
        );

        // SAFETY: `instance` is the handle `hs_load_ex` produced.
        unsafe { hs_dispose(instance) };
    }

    unsafe extern "C" fn clock_now(
        _user_data: *mut c_void,
        _args_json: *const c_char,
        out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: `out_json` points at writable storage the caller
        // provided, per `HsCapabilityFn::call`'s contract.
        unsafe { *out_json = c"\"2026-09-23T12:00:00Z\"".as_ptr().cast_mut() };
        0
    }

    unsafe extern "C" fn clock_today(
        _user_data: *mut c_void,
        _args_json: *const c_char,
        out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: as `clock_now`'s.
        unsafe { *out_json = c"\"2026-09-23\"".as_ptr().cast_mut() };
        0
    }

    /// A capability declared `Result<String, String>`: answers `{"Ok": ...}`
    /// when the argument contains "found", `{"Err": ...}` otherwise. Its own
    /// call always succeeds (returns `0`); which branch the program's `match`
    /// takes depends only on the `Result` value, not on a fault.
    unsafe extern "C" fn cache_get(
        _user_data: *mut c_void,
        args_json: *const c_char,
        out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: `args_json` is a valid, null-terminated C string for the
        // length of this call, per `HsCapabilityFn::call`'s contract.
        let text = unsafe { CStr::from_ptr(args_json) }.to_string_lossy();
        let found = text.contains("found");
        let message = if found {
            c"{\"Ok\": \"cached value\"}"
        } else {
            c"{\"Err\": \"no such key\"}"
        };
        // SAFETY: as `clock_now`'s.
        unsafe { *out_json = message.as_ptr().cast_mut() };
        0
    }

    /// A C host can declare a namespace capability (several functions
    /// sharing one `uses` name, like `clock`) and a capability declared
    /// `Result<T, E>` (section 8.2), whose answer the program pattern-matches
    /// as an ordinary value rather than the call faulting.
    #[test]
    fn a_c_host_binds_a_namespace_capability_and_a_result_typed_one() {
        let clock_functions = [c"now", c"today"];
        let cache_params = [HsValueType::String as i32];
        let capabilities = [
            HsCapabilityFn {
                name: c"clock".as_ptr(),
                function: clock_functions[0].as_ptr(),
                params: std::ptr::null(),
                params_len: 0,
                result_shape: HsResultShape::Value as i32,
                result: HsValueType::String as i32,
                result_err: HsValueType::Int as i32,
                call: Some(clock_now),
                params_json: std::ptr::null(),
                result_json: std::ptr::null(),
                user_data: std::ptr::null_mut(),
            },
            HsCapabilityFn {
                name: c"clock".as_ptr(),
                function: clock_functions[1].as_ptr(),
                params: std::ptr::null(),
                params_len: 0,
                result_shape: HsResultShape::Value as i32,
                result: HsValueType::String as i32,
                result_err: HsValueType::Int as i32,
                call: Some(clock_today),
                params_json: std::ptr::null(),
                result_json: std::ptr::null(),
                user_data: std::ptr::null_mut(),
            },
            HsCapabilityFn {
                name: c"cache".as_ptr(),
                function: std::ptr::null(),
                params: cache_params.as_ptr(),
                params_len: cache_params.len(),
                result_shape: HsResultShape::Result as i32,
                result: HsValueType::String as i32,
                result_err: HsValueType::String as i32,
                call: Some(cache_get),
                params_json: std::ptr::null(),
                result_json: std::ptr::null(),
                user_data: std::ptr::null_mut(),
            },
        ];

        let source = CString::new(concat!(
            "uses clock\n",
            "uses cache\n",
            "export fn today(): String {\n",
            "    return effect clock.today()\n",
            "}\n",
            "export fn timestamp(): String {\n",
            "    return effect clock.now()\n",
            "}\n",
            "export fn lookup(key: String): String {\n",
            "    match effect cache(key) {\n",
            "        Ok(value) { return value }\n",
            "        Err(reason) { return \"missing: ${reason}\" }\n",
            "    }\n",
            "}\n",
        ))
        .unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null (or, for `params`, null only
        // where `params_len` is 0) and valid for the length of this call.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                capabilities.as_ptr(),
                capabilities.len(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut instance,
                &raw mut json,
            )
        };
        let diagnostics = take_string(json);
        assert_eq!(status, HsStatus::Ok as c_int, "{diagnostics}");
        assert!(!instance.is_null());

        let call = |name: &str, args: &str| -> (c_int, String) {
            let name = CString::new(name).unwrap();
            let args = CString::new(args).unwrap();
            let mut out: *mut c_char = std::ptr::null_mut();
            // SAFETY: `instance` is live; the strings are valid for this
            // call; a null token means no cancellation.
            let status = unsafe {
                hs_call_ex(
                    instance,
                    name.as_ptr(),
                    args.as_ptr(),
                    std::ptr::null(),
                    &raw mut out,
                )
            };
            (status, take_string(out))
        };

        assert_eq!(
            call("today", "[]"),
            (HsStatus::Ok as c_int, "String(\"2026-09-23\")".to_owned())
        );
        assert_eq!(
            call("timestamp", "[]"),
            (
                HsStatus::Ok as c_int,
                "String(\"2026-09-23T12:00:00Z\")".to_owned()
            )
        );
        assert_eq!(
            call("lookup", "[\"found-key\"]"),
            (HsStatus::Ok as c_int, "String(\"cached value\")".to_owned())
        );
        assert_eq!(
            call("lookup", "[\"missing-key\"]"),
            (
                HsStatus::Ok as c_int,
                "String(\"missing: no such key\")".to_owned()
            )
        );

        // SAFETY: `instance` is the handle `hs_load_ex` produced.
        unsafe { hs_dispose(instance) };
    }

    /// `hs_call`'s arguments carry a full list, set and map, not only
    /// scalars, in the tagged JSON encoding this crate's module
    /// documentation defines. A list needs no host-declared shape at all;
    /// this is the part of the encoding that works for any program.
    #[test]
    fn hs_call_arguments_carry_a_list_a_set_and_a_map() {
        let (instance, diagnostics) = load_instance(concat!(
            "export fn sumList(xs: [Int]): Int {\n",
            "    variable total = 0\n",
            "    for x in xs {\n",
            "        total = total + x\n",
            "    }\n",
            "    return total\n",
            "}\n",
            "export fn hasEach(names: Set<String>): Bool {\n",
            "    return names.has(\"a\") and names.has(\"b\")\n",
            "}\n",
            "export fn lookup(scores: Map<String, Int>, key: String): Int {\n",
            "    constant found = scores.get(key)\n",
            "    if found == none {\n",
            "        return -1\n",
            "    }\n",
            "    return found\n",
            "}\n",
        ));
        assert!(!instance.is_null(), "{diagnostics}");

        assert_eq!(
            call(instance, "sumList", r#"[{"list": [1, 2, 3]}]"#),
            (HsStatus::Ok as c_int, "Int(6)".to_owned())
        );
        assert_eq!(
            call(instance, "hasEach", r#"[{"set": ["a", "b"]}]"#),
            (HsStatus::Ok as c_int, "Bool(true)".to_owned())
        );
        assert_eq!(
            call(
                instance,
                "lookup",
                r#"[{"map": [["x", 10], ["y", 20]]}, "y"]"#
            ),
            (HsStatus::Ok as c_int, "Int(20)".to_owned())
        );

        // SAFETY: the handle is live.
        unsafe { hs_dispose(instance) };
    }

    /// `hs_call`'s arguments carry a record and an enum too: `Point` and
    /// `Quadrant` are used only in `area`/`whichQuadrant`'s own exported
    /// signatures, never constructed anywhere else in the source, so this
    /// only works because `hollowscript::hosttypes::register_module_types`
    /// names every shape and variant the compiled module already has, not
    /// only ones a capability's declared type happens to mention.
    #[test]
    fn hs_call_arguments_carry_a_record_and_an_enum() {
        let (instance, diagnostics) = load_instance(concat!(
            "record Point = { x: Int, y: Int }\n",
            "enum Quadrant { Origin, Elsewhere }\n",
            "export fn area(p: Point): Int {\n",
            "    return p.x * p.y\n",
            "}\n",
            "export fn whichQuadrant(q: Quadrant): String {\n",
            "    match q {\n",
            "        Origin { return \"origin\" }\n",
            "        Elsewhere { return \"elsewhere\" }\n",
            "    }\n",
            "}\n",
        ));
        assert!(!instance.is_null(), "{diagnostics}");

        assert_eq!(
            call(instance, "area", r#"[{"record": {"x": 3, "y": 4}}]"#),
            (HsStatus::Ok as c_int, "Int(12)".to_owned())
        );
        assert_eq!(
            call(
                instance,
                "whichQuadrant",
                r#"[{"enum": {"name": "Quadrant", "variant": "Origin", "payload": []}}]"#
            ),
            (HsStatus::Ok as c_int, "String(\"origin\")".to_owned())
        );

        // SAFETY: the handle is live.
        unsafe { hs_dispose(instance) };
    }

    /// A C host declares a capability parameter of [`HsValueType::Filter`]
    /// (Cascade's `tasks`, cited by name in section 16.11, is exactly this
    /// shape) and receives the program's filter literal as its source text,
    /// tagged so a host cannot mistake it for an ordinary `String`
    /// argument.
    #[test]
    fn a_filter_argument_crosses_as_its_source_text() {
        let captured = Box::new(Mutex::new(String::new()));
        let captured_ptr: *mut c_void = Box::into_raw(captured).cast();

        let tasks_params = [HsValueType::Filter as i32];
        let capabilities = [HsCapabilityFn {
            name: c"tasks".as_ptr(),
            function: std::ptr::null(),
            params: tasks_params.as_ptr(),
            params_len: tasks_params.len(),
            result_shape: HsResultShape::None as i32,
            result: HsValueType::Int as i32,
            result_err: HsValueType::Int as i32,
            call: Some(capture_print),
            params_json: std::ptr::null(),
            result_json: std::ptr::null(),
            user_data: captured_ptr,
        }];

        let source = CString::new(concat!(
            "uses tasks\n",
            "export fn run() {\n",
            "    effect tasks(`priority:high`)\n",
            "}\n",
        ))
        .unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null and valid for the length of
        // this call.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                capabilities.as_ptr(),
                capabilities.len(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut instance,
                &raw mut json,
            )
        };
        let diagnostics = take_string(json);
        assert_eq!(status, HsStatus::Ok as c_int, "{diagnostics}");
        assert!(!instance.is_null());

        let run = CString::new("run").unwrap();
        let args = CString::new("[]").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` is live; the strings are valid for this call;
        // a null token means no cancellation.
        let status = unsafe {
            hs_call_ex(
                instance,
                run.as_ptr(),
                args.as_ptr(),
                std::ptr::null(),
                &raw mut out,
            )
        };
        let rendered = take_string(out);
        assert_eq!(status, HsStatus::Ok as c_int, "{rendered}");

        // SAFETY: the pointer is the one this test leaked into `user_data`
        // above, reclaimed here for the first and only time.
        let captured = unsafe { Box::from_raw(captured_ptr.cast::<Mutex<String>>()) };
        let args_json = captured
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(args_json, r#"[{"filter":"priority:high"}]"#);

        // SAFETY: `instance` is the handle `hs_load_ex` produced.
        unsafe { hs_dispose(instance) };
    }

    /// A capability whose parameter and result types are declared through
    /// `params_json`/`result_json`, not `params`/`result`: `entities.get`
    /// takes a `String` (still declared through the scalar `params`, which
    /// keeps working unchanged alongside `params_json`) and answers a
    /// record `{"id": String, "x": Int, "y": Int}`, which `HsValueType`
    /// alone cannot express. This is the C ABI reaching parity with
    /// `hollowscript-wasm`'s own `tooling` boundary: the same recursive
    /// type-description grammar, documented once in `declared_type`'s own
    /// module documentation and reproduced in the header.
    unsafe extern "C" fn entities_get(
        _user_data: *mut c_void,
        _args_json: *const c_char,
        out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        let answer = c"{\"record\":{\"id\":\"player-1\",\"x\":3,\"y\":4}}";
        // SAFETY: `out_json` points at writable storage the caller
        // provided, per `HsCapabilityFn::call`'s contract; `answer` is a
        // `'static` string literal, well within the contract that it need
        // only stay valid until this callback returns.
        unsafe { *out_json = answer.as_ptr().cast_mut() };
        0
    }

    #[test]
    fn a_capability_declares_a_record_result_through_result_json() {
        let entities_params = [HsValueType::String as i32];
        let result_json = c"{\"record\":{\"id\":\"String\",\"x\":\"Int\",\"y\":\"Int\"}}";
        let capabilities = [HsCapabilityFn {
            name: c"entities".as_ptr(),
            function: c"get".as_ptr(),
            params: entities_params.as_ptr(),
            params_len: entities_params.len(),
            result_shape: HsResultShape::Value as i32,
            // Unread: `result_json` takes over entirely for the return type.
            result: HsValueType::Int as i32,
            result_err: HsValueType::Int as i32,
            call: Some(entities_get),
            params_json: std::ptr::null(),
            result_json: result_json.as_ptr(),
            user_data: std::ptr::null_mut(),
        }];

        let source = CString::new(concat!(
            "uses entities\n",
            "export fn run(): Int {\n",
            "    constant e = effect entities.get(\"player-1\")\n",
            "    return e.x + e.y\n",
            "}\n",
        ))
        .unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null and valid for the length of
        // this call.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                capabilities.as_ptr(),
                capabilities.len(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut instance,
                &raw mut json,
            )
        };
        let diagnostics = take_string(json);
        assert_eq!(status, HsStatus::Ok as c_int, "{diagnostics}");
        assert!(!instance.is_null());

        let run = CString::new("run").unwrap();
        let args = CString::new("[]").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` is live; the strings are valid for this call;
        // a null token means no cancellation.
        let status = unsafe {
            hs_call_ex(
                instance,
                run.as_ptr(),
                args.as_ptr(),
                std::ptr::null(),
                &raw mut out,
            )
        };
        let rendered = take_string(out);
        assert_eq!(status, HsStatus::Ok as c_int, "{rendered}");
        assert_eq!(rendered, "Int(7)");

        // SAFETY: `instance` is the handle `hs_load_ex` produced.
        unsafe { hs_dispose(instance) };
    }

    /// A capability answering a record `{x: Float, y: Float}` as
    /// `{"x": 1, "y": 0}` (both look integral in JSON, since JSON and a JS
    /// host cannot write `1.0` and have it read back distinguishably from
    /// `1`) decodes each field to a genuine `Float`, not an `Int`: the
    /// declared type, not the JSON's own spelling, is what says which one a
    /// bare number means. Section 9.6's "nothing is converted inside a
    /// collection, a record or an enum payload" is unaffected: this is the
    /// encoding's own definition of the value in the first place, before
    /// section 9.6 ever sees it.
    unsafe extern "C" fn points_get(
        _user_data: *mut c_void,
        _args_json: *const c_char,
        out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        let answer = c"{\"record\":{\"x\":1,\"y\":0}}";
        // SAFETY: as `entities_get`'s own identical case.
        unsafe { *out_json = answer.as_ptr().cast_mut() };
        0
    }

    #[test]
    fn a_bare_json_number_at_a_float_position_decodes_as_float() {
        let points_params = [HsValueType::String as i32];
        let result_json = c"{\"record\":{\"x\":\"Float\",\"y\":\"Float\"}}";
        let capabilities = [HsCapabilityFn {
            name: c"points".as_ptr(),
            function: c"get".as_ptr(),
            params: points_params.as_ptr(),
            params_len: points_params.len(),
            result_shape: HsResultShape::Value as i32,
            result: HsValueType::Int as i32,
            result_err: HsValueType::Int as i32,
            call: Some(points_get),
            params_json: std::ptr::null(),
            result_json: result_json.as_ptr(),
            user_data: std::ptr::null_mut(),
        }];

        let (instance, diagnostics) = load_instance_with_capabilities(
            concat!(
                "uses points\n",
                "export fn run(): Int {\n",
                "    return 0\n",
                "}\n",
                "export fn point(): { x: Float, y: Float } {\n",
                "    return effect points.get(\"origin\")\n",
                "}\n",
            ),
            &capabilities,
        );
        assert!(!instance.is_null(), "{diagnostics}");

        assert_eq!(
            call(instance, "point", "[]"),
            (
                HsStatus::Ok as c_int,
                "{x: Float(1.0), y: Float(0.0)}".to_owned()
            ),
        );

        // SAFETY: `instance` is the handle `load_instance_with_capabilities`
        // produced.
        unsafe { hs_dispose(instance) };
    }

    /// The same disambiguation applies to `hs_call`'s own arguments: an
    /// exported function's declared `Float` parameter accepts a bare,
    /// integral-looking JSON number.
    #[test]
    fn a_bare_json_number_call_argument_at_a_float_position_decodes_as_float() {
        let (instance, diagnostics) = load_instance(concat!(
            "export fn describe(x: Float): String {\n",
            "    return \"${x}\"\n",
            "}\n",
        ));
        assert!(!instance.is_null(), "{diagnostics}");
        assert_eq!(
            call(instance, "describe", "[3]"),
            (HsStatus::Ok as c_int, "String(\"3.0\")".to_owned()),
        );
        // SAFETY: the handle is live.
        unsafe { hs_dispose(instance) };
    }

    /// A wire integer at a `Float` position that is not exactly
    /// representable is refused, not rounded to the nearest `Float`:
    /// `9007199254740993` (`2^53 + 1`) must not silently become
    /// `9007199254740992.0`. `2^53` itself, the boundary, is still exact
    /// and accepted.
    #[test]
    fn an_inexact_integer_at_a_float_position_is_refused_not_rounded() {
        let (instance, diagnostics) = load_instance(concat!(
            "export fn describe(x: Float): String {\n",
            "    return \"${x}\"\n",
            "}\n",
        ));
        assert!(!instance.is_null(), "{diagnostics}");

        let (status, message) = call(instance, "describe", "[9007199254740993]");
        assert_eq!(status, HsStatus::Refused as c_int, "{message}");
        assert!(message.contains("9007199254740993"), "{message}");

        assert_eq!(
            call(instance, "describe", "[9007199254740992]"),
            (
                HsStatus::Ok as c_int,
                "String(\"9007199254740992.0\")".to_owned()
            ),
            "2^53 itself is exactly representable and must still be accepted"
        );
        // SAFETY: the handle is live.
        unsafe { hs_dispose(instance) };
    }

    /// A malformed `params_json` is refused as `HS_INVALID_CAPABILITY`
    /// (distinct from `HS_CHECK_FAILED`, which is a malformed program),
    /// with the reason written to `*out_json`, rather than panicking or
    /// silently falling back to the scalar `params`.
    #[test]
    fn a_malformed_params_json_is_refused_with_a_reason() {
        let malformed = c"not json";
        let capabilities = [HsCapabilityFn {
            name: c"tasks".as_ptr(),
            function: std::ptr::null(),
            params: std::ptr::null(),
            params_len: 0,
            result_shape: HsResultShape::None as i32,
            result: HsValueType::Int as i32,
            result_err: HsValueType::Int as i32,
            call: Some(entities_get),
            params_json: malformed.as_ptr(),
            result_json: std::ptr::null(),
            user_data: std::ptr::null_mut(),
        }];

        let source = CString::new("uses tasks\n").unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null and valid for the length of
        // this call.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                capabilities.as_ptr(),
                capabilities.len(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut instance,
                &raw mut json,
            )
        };
        let message = take_string(json);
        assert_eq!(status, HsStatus::InvalidCapability as c_int, "{message}");
        assert!(instance.is_null());
        assert!(!message.is_empty());
    }

    /// A running call is cancelled from another thread (section 11.7): a
    /// `while true {}` never finishes on its own, but setting the token
    /// from a second thread stops it at its next safepoint with HS1006.
    #[test]
    fn a_running_call_is_cancelled_from_another_thread() {
        let (instance, _) = load_instance("export fn spin() {\n    while true {\n    }\n}\n");
        assert!(!instance.is_null());

        let token = hs_cancel_token_new();
        // The handle and the token are raw pointers, sent across the thread
        // boundary as addresses: the instance is used from one thread at a
        // time as its own contract requires (this thread, inside
        // `hs_call_ex`, until it returns), and the token is exactly the one
        // pointer this crate documents as safe to reach from another thread
        // while a call using it is in flight.
        let instance_addr = instance as usize;
        let token_addr = token as usize;
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let token = std::ptr::without_provenance::<HsCancelToken>(token_addr);
            // SAFETY: `token` is live for as long as this thread runs,
            // which the test's join below guarantees ends before it is freed.
            unsafe { hs_cancel_token_cancel(token) };
        });

        let name = CString::new("spin").unwrap();
        let args = CString::new("[]").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        let instance_ptr = std::ptr::without_provenance_mut::<HsInstance>(instance_addr);
        let token_ptr = std::ptr::without_provenance::<HsCancelToken>(token_addr);
        // SAFETY: `instance_ptr` is the live handle `load_instance` returned,
        // reconstructed from its own address; `token_ptr` likewise for the
        // live token; the strings are valid for this call.
        let status = unsafe {
            hs_call_ex(
                instance_ptr,
                name.as_ptr(),
                args.as_ptr(),
                token_ptr,
                &raw mut out,
            )
        };
        canceller.join().unwrap();
        let message = take_string(out);
        assert_eq!(status, HsStatus::Faulted as c_int, "{message}");
        assert!(message.contains("cancelled"), "{message}");

        // SAFETY: the token is live and no thread can still be using it,
        // since `canceller` has already been joined.
        unsafe { hs_cancel_token_free(token) };
        // SAFETY: `instance` is the handle `load_instance` produced.
        unsafe { hs_dispose(instance) };
    }

    /// A capability argument of a declared recursive
    /// type, nested as deep as the heap allows, is exactly what
    /// `check_capability_arguments` (`hollowscript-vm`) accepts as
    /// well-typed and what `value_to_json` still has to turn into text.
    /// Before this, `value_to_json` recursed once per level and would
    /// have overflowed the host's stack; built directly against a
    /// [`HostCall`] rather than through the full C ABI, since the point is
    /// this function's own stack use, not the marshalling around it.
    #[test]
    fn a_deeply_nested_capability_argument_converts_to_json_without_exhausting_the_stack() {
        use hollowscript::{Heap, HostCall, HostTypes, Object, RunCounters};

        let mut heap = Heap::new();
        let mut value = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..200_000 {
            value = heap.alloc(Object::List(vec![value])).unwrap();
        }
        let types = HostTypes::new();
        let mut counters = RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let mut out = String::new();
        let mut never = || false;
        let mut budget = super::JsonBudget::new(u64::MAX, &mut never);
        super::value_to_json(&call, value, &mut out, &mut budget).unwrap();
        assert_eq!(out.len(), 200_000 * 11 + r#"{"list":[]}"#.len());
    }

    /// A capability argument built by repeated doubling (`t = [t, t]`, fifty
    /// times: fifty objects and 2^50 paths) must be refused with the output
    /// limit once its text would pass it, having stopped within one piece of
    /// the limit, instead of being written out until memory runs out.
    #[test]
    fn a_shared_structure_capability_argument_is_bounded_by_the_output_limit() {
        use hollowscript::{Heap, HostCall, HostTypes, Object, RunCounters};

        let mut heap = Heap::new();
        let mut value = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..50 {
            value = heap.alloc(Object::List(vec![value, value])).unwrap();
        }
        let types = HostTypes::new();
        let mut counters = RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let started = std::time::Instant::now();
        let fault = super::render_capability_arguments(&call, &[value], 1_000_000).unwrap_err();
        let diagnostic = fault.to_diagnostic();
        assert_eq!(diagnostic.code.to_text(), "HS1004");
        let data = diagnostic.data.unwrap();
        assert_eq!(data.limit, 1_000_000);
        assert!(data.used <= 1_000_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
    }

    /// The bound is shared by every argument of one call, a single long
    /// string is refused before it is escaped, and a rendering that fits is
    /// untouched.
    #[test]
    fn capability_arguments_share_one_budget_and_a_fitting_call_is_untouched() {
        use hollowscript::{Heap, HostCall, HostTypes, Object, RunCounters};

        let mut heap = Heap::new();
        let long = heap.alloc(Object::Str("x".repeat(600))).unwrap();
        let types = HostTypes::new();
        let mut counters = RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let one = super::render_capability_arguments(&call, &[long], 1_000).unwrap();
        assert_eq!(one.len(), 602 + 2);
        // Two of them: each fits alone, together they do not.
        let two = super::render_capability_arguments(&call, &[long, long], 1_000);
        assert!(two.is_err());
        // A single string over the limit is refused without being written.
        assert!(super::render_capability_arguments(&call, &[long], 100).is_err());
    }

    /// A cancellation token set while a capability argument is written stops
    /// the writing with HS1006.
    #[test]
    fn writing_a_capability_argument_stops_when_the_run_is_cancelled() {
        use hollowscript::{Heap, HostCall, HostTypes, Object, RunCounters};

        let mut heap = Heap::new();
        let mut value = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..50 {
            value = heap.alloc(Object::List(vec![value, value])).unwrap();
        }
        let types = HostTypes::new();
        let mut counters = RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let mut polls = 0u32;
        let mut stop_after_three = || {
            polls += 1;
            polls > 3
        };
        let mut budget = super::JsonBudget::new(u64::MAX, &mut stop_after_three);
        let mut out = String::new();
        let fault = super::value_to_json(&call, value, &mut out, &mut budget).unwrap_err();
        assert_eq!(fault.to_diagnostic().code.to_text(), "HS1006");
        assert!(out.len() < 1_000_000, "{}", out.len());
    }

    /// `hs_check_json` must run its check on a dedicated large-stack thread
    /// (`hollowscript::instance::check`, which it calls directly), not
    /// whatever thread the C host happened to call it from: a C host is
    /// free to call it from a thread as small as it likes, and a
    /// deeply nested but legal program used to abort the process on one
    /// small enough (512 KiB or 1024 KiB, reported against exactly this
    /// entry point) instead of answering with diagnostics. This calls it
    /// from a thread that small, standing in for such a host.
    #[test]
    fn check_json_survives_a_deeply_nested_program_from_a_small_stack_caller() {
        const SMALL_STACK: usize = 512 * 1024;
        // A chain of 100 immediately invoked closures, well within the
        // 250-level nesting limit of section 1.10 but heavier per level for
        // the checker than for the parser (see
        // `crates/hollowscript/tests/stack_safety.rs` for the same
        // construction and a fuller explanation).
        let mut inner = "1".to_owned();
        for _ in 0..100 {
            inner = format!("(fn(x: Int): Int {{ return x }})({inner})");
        }
        let source = CString::new(format!("return {inner}\n")).unwrap();
        let handle = std::thread::Builder::new()
            .stack_size(SMALL_STACK)
            .spawn(move || {
                let mut out: *mut c_char = std::ptr::null_mut();
                // SAFETY: `source` is a valid null-terminated C string owned
                // by this closure, and `out` is a valid pointer to a local
                // `*mut c_char`.
                let status =
                    unsafe { hs_check_json(source.as_ptr(), std::ptr::null(), &raw mut out) };
                assert_eq!(status, HsStatus::Ok as c_int);
                assert!(!out.is_null());
                // SAFETY: `out` was just set by `hs_check_json` to a pointer
                // it returned.
                let json = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_owned();
                assert_eq!(
                    json, "[]",
                    "a clean, deeply nested program still checks cleanly"
                );
                // SAFETY: `out` is exactly the pointer `hs_check_json`
                // returned, freed here for the first and only time.
                unsafe { hs_string_free(out) };
            })
            .expect("could not start the test's small-stack thread");
        handle
            .join()
            .expect("hs_check_json aborted (or panicked) on a small-stack caller");
    }

    /// The address of the token [`freeing_the_cancellation_token_during_the_call_that_uses_it_is_safe`]
    /// frees from inside its own capability callback, carried across the
    /// callback boundary as an address the same way
    /// [`a_running_call_is_cancelled_from_another_thread`] carries one
    /// across a thread boundary.
    static TOKEN_TO_FREE_MID_CALL: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn free_token_mid_call(
        _user_data: *mut c_void,
        _args_json: *const c_char,
        out_json: *mut *mut c_char,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        let token_addr = TOKEN_TO_FREE_MID_CALL.load(Ordering::SeqCst);
        let token = std::ptr::without_provenance_mut::<HsCancelToken>(token_addr);
        // SAFETY: `token_addr` names the live token this test's own
        // `hs_cancel_token_new` returned; freeing it here, synchronously,
        // from inside the very capability call that is using it, is the
        // reproduction this crate's cancellation-token fix (cloning the
        // flag at `hs_call_ex`'s own entry rather than borrowing the
        // host's memory for the call's whole length) must survive.
        unsafe { hs_cancel_token_free(token) };
        // SAFETY: `out_json` is non-null, by `hs_call_ex`'s own contract on
        // a capability callback whose `result_shape` is not `None`; this
        // crate reads the string during the call and never frees it, so a
        // pointer at a string literal, kept alive for the callback's own
        // duration, is exactly what the callback's ownership contract asks for.
        unsafe { *out_json = c"1".as_ptr().cast_mut() };
        0
    }

    /// Freeing a cancellation token from inside the very capability call
    /// that is using it (not from another thread: synchronously, on the
    /// same thread `hs_call_ex` is running on) must not read freed memory
    /// afterwards. This is the C ABI review's own reproduction of the use
    /// after free `token_clone` (formerly `token_ref`) fixes: cloning the
    /// token's shared flag at entry means the rest of this call reads its
    /// own clone, never the host's freed allocation.
    #[test]
    fn freeing_the_cancellation_token_during_the_call_that_uses_it_is_safe() {
        let capabilities = [HsCapabilityFn {
            name: c"host".as_ptr(),
            function: std::ptr::null(),
            params: std::ptr::null(),
            params_len: 0,
            result_shape: HsResultShape::Value as i32,
            result: HsValueType::Int as i32,
            result_err: HsValueType::Int as i32,
            call: Some(free_token_mid_call),
            params_json: std::ptr::null(),
            result_json: std::ptr::null(),
            user_data: std::ptr::null_mut(),
        }];
        let (instance, diagnostics) = load_instance_with_capabilities(
            "uses host\nexport fn f(): Int {\n    return effect host() + 1\n}\n",
            &capabilities,
        );
        assert!(!instance.is_null(), "{diagnostics}");

        let token = hs_cancel_token_new();
        TOKEN_TO_FREE_MID_CALL.store(token as usize, Ordering::SeqCst);

        let name = CString::new("f").unwrap();
        let args = CString::new("[]").unwrap();
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` and `token` are both live; the two strings are
        // null-terminated and outlive this call.
        let status =
            unsafe { hs_call_ex(instance, name.as_ptr(), args.as_ptr(), token, &raw mut out) };
        let text = take_string(out);
        assert_eq!(status, HsStatus::Ok as c_int, "{text}");
        assert_eq!(text, "Int(2)");

        // SAFETY: `instance` is the handle `load_instance_with_capabilities`
        // produced; the token itself was already freed, by the capability
        // callback, during the call above.
        unsafe { hs_dispose(instance) };
    }

    /// A NULL `capabilities` array with an enormous `capabilities_len` must
    /// be [`HsStatus::NullArgument`], not an allocator abort or panic:
    /// `read_capabilities` never sizes anything from a caller-supplied
    /// length before it has checked the pointer that length describes.
    #[test]
    fn a_null_capabilities_array_with_a_huge_length_is_refused() {
        let source = CString::new("return 1\n").unwrap();
        let name = CString::new("main.hws").unwrap();
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: `capabilities` is deliberately null with a huge, invalid
        // `capabilities_len`, which this crate's own contract allows a
        // caller to get wrong (it is checked, not trusted); every other
        // pointer is non-null and valid for this call.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                std::ptr::null(),
                1_usize << 40,
                std::ptr::null(),
                std::ptr::null(),
                &raw mut instance,
                &raw mut json,
            )
        };
        assert_eq!(status, HsStatus::NullArgument as c_int);
        assert!(instance.is_null());
        take_string(json);
    }

    /// Out-of-range `HsValueType`/`HsResultShape` values, the raw integers
    /// a C host could write by mistake (or leave from an uninitialised
    /// buffer), are refused as [`HsStatus::InvalidCapability`] rather than
    /// read as a Rust enum whose discriminant they may not name.
    #[test]
    fn out_of_range_capability_enum_values_are_refused() {
        let bad_params = [99_i32];
        let mut capabilities = [HsCapabilityFn {
            name: c"host".as_ptr(),
            function: std::ptr::null(),
            params: bad_params.as_ptr(),
            params_len: 1,
            result_shape: HsResultShape::None as i32,
            result: HsValueType::Int as i32,
            result_err: HsValueType::Int as i32,
            call: Some(entities_get),
            params_json: std::ptr::null(),
            result_json: std::ptr::null(),
            user_data: std::ptr::null_mut(),
        }];
        let (instance, diagnostics) = load_instance_with_capabilities("return 1\n", &capabilities);
        assert!(
            instance.is_null(),
            "an out-of-range params[0] value must create no instance"
        );
        assert!(!diagnostics.is_empty());

        capabilities[0].params = std::ptr::null();
        capabilities[0].params_len = 0;
        capabilities[0].result_shape = 7;
        let (instance, diagnostics) = load_instance_with_capabilities("return 1\n", &capabilities);
        assert!(
            instance.is_null(),
            "an out-of-range result_shape value must create no instance"
        );
        assert!(!diagnostics.is_empty());
    }

    /// A call refused for the wrong argument count must not root any
    /// argument against the heap in the first place: `parse_arguments`
    /// checks the export's own arity before building a single one, so a
    /// caller that repeatedly gets the count wrong (or retries after a
    /// refusal) never accumulates roots the way it used to.
    #[test]
    fn a_bad_arity_call_never_roots_its_arguments() {
        let (instance, diagnostics) =
            load_instance("export fn one(x: Int): Int {\n    return x\n}\n");
        assert!(!instance.is_null(), "{diagnostics}");
        let big = format!("[\"{}\", \"{}\"]", "x".repeat(10_000), "y".repeat(10_000));
        let before = heap_len(instance);
        for _ in 0..50 {
            let (status, message) = call(instance, "one", &big);
            assert_eq!(status, HsStatus::Refused as c_int, "{message}");
        }
        let after = heap_len(instance);
        assert_eq!(
            before, after,
            "a call refused for the wrong argument count must not grow the heap"
        );
        // SAFETY: `instance` is the handle `load_instance` produced.
        unsafe { hs_dispose(instance) };
    }

    /// Every `out_*` pointer is written null before any check that could
    /// return early runs, so a caller that does not inspect the status (or
    /// does, but still frees whatever was already at `*out`) never reads
    /// uninitialised or stale memory rather than a null pointer.
    #[test]
    fn output_pointers_are_written_null_before_any_error_return() {
        // Not valid UTF-8 past the first byte: every function below refuses
        // this the same way, before it does anything else with `source`.
        let invalid_utf8 = [b'x', 0xFF, 0];

        let mut json: *mut c_char = std::ptr::without_provenance_mut(1);
        // SAFETY: `invalid_utf8` is a null-terminated byte buffer, valid to
        // read as a `*const c_char` for `hs_check_json`'s own contract
        // (it does not have to be valid UTF-8: that is exactly what is
        // being refused); `json` points at writable storage poisoned with
        // a non-null, never-dereferenced value, to prove it is overwritten.
        let status = unsafe {
            hs_check_json(
                invalid_utf8.as_ptr().cast::<c_char>(),
                std::ptr::null(),
                &raw mut json,
            )
        };
        assert_eq!(status, HsStatus::InvalidUtf8 as c_int);
        assert!(json.is_null(), "hs_check_json must clear *out_json");

        let mut instance: *mut HsInstance = std::ptr::without_provenance_mut(1);
        let mut json: *mut c_char = std::ptr::without_provenance_mut(1);
        let name = CString::new("main.hws").unwrap();
        // SAFETY: as above, for `hs_load`'s own contract; `instance` and
        // `json` are poisoned the same way.
        let status = unsafe {
            hs_load(
                invalid_utf8.as_ptr().cast::<c_char>(),
                name.as_ptr(),
                &raw mut instance,
                &raw mut json,
            )
        };
        assert_eq!(status, HsStatus::InvalidUtf8 as c_int);
        assert!(instance.is_null(), "hs_load must clear *out_instance");
        assert!(json.is_null(), "hs_load must clear *out_json");

        let mut out: *mut c_char = std::ptr::without_provenance_mut(1);
        let name = CString::new("f").unwrap();
        let args = CString::new("[]").unwrap();
        // SAFETY: `instance` is deliberately null, the documented
        // `HsStatus::NullArgument` case; `out` is poisoned the same way.
        let status = unsafe {
            hs_call(
                std::ptr::null_mut(),
                name.as_ptr(),
                args.as_ptr(),
                &raw mut out,
            )
        };
        assert_eq!(status, HsStatus::NullArgument as c_int);
        assert!(
            out.is_null(),
            "hs_call must clear *out_text even for a NULL instance"
        );
    }

    /// A zero `max_call_depth` is refused as `HS_INVALID_LIMITS`, with the
    /// reason in `*out_json`, before the load is attempted, rather than
    /// being silently clamped or crashing later.
    #[test]
    fn an_out_of_range_limit_is_refused() {
        let source = CString::new("return 1\n").unwrap();
        let name = CString::new("main.hws").unwrap();
        let limits = HsLimits {
            max_steps: 1_000,
            max_memory_bytes: 1_000_000,
            max_output_bytes: 1_000,
            max_call_depth: 0,
        };
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: every pointer is non-null and valid for the length of
        // this call; `limits` is a valid `HsLimits` value.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                &raw const limits,
                &raw mut instance,
                &raw mut json,
            )
        };
        let message = take_string(json);
        assert_eq!(status, HsStatus::InvalidLimits as c_int, "{message}");
        assert!(instance.is_null());
        assert!(message.contains("maxCallDepth"), "{message}");
    }

    /// A custom `max_steps` set through `HsLimits` actually governs the
    /// run: a program that would spin forever under the specification's own
    /// default budget faults quickly under a deliberately tiny one, proving
    /// the C ABI's own limits are not merely accepted but applied.
    #[test]
    fn a_custom_max_steps_limit_is_applied() {
        let source = CString::new("export fn spin() {\n    while true {\n    }\n}\n").unwrap();
        let name = CString::new("main.hws").unwrap();
        let limits = HsLimits {
            max_steps: 100,
            max_memory_bytes: 1_000_000,
            max_output_bytes: 1_000,
            max_call_depth: 16,
        };
        let mut instance: *mut HsInstance = std::ptr::null_mut();
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: as `an_out_of_range_limit_is_refused`.
        let status = unsafe {
            hs_load_ex(
                source.as_ptr(),
                name.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                &raw const limits,
                &raw mut instance,
                &raw mut json,
            )
        };
        let diagnostics = take_string(json);
        assert_eq!(status, HsStatus::Ok as c_int, "{diagnostics}");
        assert!(!instance.is_null());

        let (status, message) = call(instance, "spin", "[]");
        assert_eq!(status, HsStatus::Faulted as c_int, "{message}");
        assert!(message.contains("step"), "{message}");

        // SAFETY: `instance` is the handle this test produced.
        unsafe { hs_dispose(instance) };
    }
}
