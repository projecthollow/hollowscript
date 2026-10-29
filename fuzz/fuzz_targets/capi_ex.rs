//! The extended C ABI (`hs_load_ex`, `hs_call_ex`, cancellation tokens and a
//! host capability's own callback) called with arbitrary bytes, complementing
//! `capi.rs`, which never reaches any of these: `hs_load` and `hs_call` alone
//! bind no capability at all (an empty binder that always answers "not
//! found"), so a program that calls `uses cap` never actually invokes a
//! callback through that path, and neither `hs_load` nor `hs_call` takes a
//! cancellation token to begin with.
//!
//! This target declares one capability (`cap`, a plain function returning
//! `Int`) whose callback is driven by the fuzzer's own bytes, so the
//! callback boundary itself (reading `args_json`, writing `*out_json` or
//! `*out_error`, the non-zero-return refusal path) is exercised under
//! coverage guidance, not just the parser and checker in front of it.
//! Cancellation is exercised the same way: a token is always created, is
//! sometimes cancelled before the call it governs even starts, and is freed
//! only after every call that could still read it has returned, since
//! freeing one mid-call is exactly the use-after-free the C ABI's own
//! contract (see `hs_cancel_token_new`'s documentation) warns a host away
//! from, not something this target means to reproduce.
//!
//! The property is the same as every other target here: nothing panics, and
//! every handle this crate gives out (`HsInstance`, `HsCancelToken`) may be
//! disposed, more than once, without the process crashing.

#![no_main]

use hollowscript_capi::{
    HsCapabilityFn, HsResultShape, HsValueType, hs_call_ex, hs_cancel_token_cancel,
    hs_cancel_token_free, hs_cancel_token_new, hs_dispose, hs_load_ex, hs_read, hs_string_free,
};
use libfuzzer_sys::fuzz_target;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

/// As `capi.rs`'s own `to_cstring`: arbitrary bytes with embedded NULs
/// dropped, since a real C string cannot hold one either.
fn to_cstring(bytes: &[u8]) -> CString {
    let clean: Vec<u8> = bytes.iter().copied().filter(|&b| b != 0).collect();
    CString::new(clean).unwrap_or_default()
}

/// Splits `data` on the first NUL byte into a source text and a call
/// arguments text, the same split `capi.rs` uses, so unrelated fuzzer input
/// does not always collide into the same string for both.
fn split(data: &[u8]) -> (CString, CString) {
    let mid = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let (a, b) = data.split_at(mid);
    (to_cstring(a), to_cstring(b))
}

/// The `cap` capability's callback: a fixed, static menu of answers chosen
/// by a byte of the program's own call arguments, wide enough to exercise
/// both `Ok` and the refusal path (a non-zero return) without allocating,
/// so this target can run for a long time without leaking the strings a
/// heap-allocated answer would need freeing.
unsafe extern "C" fn cap_call(
    _user_data: *mut c_void,
    args_json: *const c_char,
    out_json: *mut *mut c_char,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: `args_json` is a valid, null-terminated C string for the
    // duration of this call, per `HsCapabilityFn::call`'s own contract.
    let choice = unsafe { CStr::from_ptr(args_json) }
        .to_bytes()
        .first()
        .copied()
        .unwrap_or(0);
    match choice % 4 {
        0 => {
            // SAFETY: `out_json` is a valid pointer to writable storage, and
            // the string pointed to is `'static`, so it stays valid for as
            // long as the library keeps reading it, well past this call.
            unsafe { *out_json = c"0".as_ptr().cast_mut() };
            0
        }
        1 => {
            unsafe { *out_json = c"9223372036854775807".as_ptr().cast_mut() };
            0
        }
        2 => {
            // SAFETY: as above, for `out_error`.
            unsafe { *out_error = c"fuzz refusal".as_ptr().cast_mut() };
            1
        }
        _ => {
            unsafe { *out_json = c"-1".as_ptr().cast_mut() };
            0
        }
    }
}

fn free_if_set(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: `ptr` is either null (handled above) or a string this
        // crate's own `*_json`/`*_ex` functions returned and has not been
        // freed yet.
        unsafe { hs_string_free(ptr) };
    }
}

fuzz_target!(|data: &[u8]| {
    let (source, arguments) = split(data);
    let entry_name = c"fuzz.hws";
    let call_name = c"main";
    let cap_name = c"cap";

    let capability = HsCapabilityFn {
        name: cap_name.as_ptr(),
        function: std::ptr::null(),
        params: std::ptr::null(),
        params_len: 0,
        result_shape: HsResultShape::Value as i32,
        result: HsValueType::Int as i32,
        result_err: HsValueType::Int as i32,
        params_json: std::ptr::null(),
        result_json: std::ptr::null(),
        call: Some(cap_call),
        user_data: std::ptr::null_mut(),
    };

    let load_token = hs_cancel_token_new();
    // A byte of the source itself (rather than the arguments, already used
    // for the call below) decides whether the load's own token is
    // cancelled before `hs_load_ex` ever runs, covering section 12.3 step
    // 3's "already cancelled" path through this ABI specifically.
    if source.as_bytes().first().is_some_and(|b| b % 5 == 0) {
        // SAFETY: `load_token` was just returned by `hs_cancel_token_new`
        // and is not yet freed.
        unsafe { hs_cancel_token_cancel(load_token) };
    }

    let mut instance: *mut hollowscript_capi::HsInstance = std::ptr::null_mut();
    let mut out_json: *mut c_char = std::ptr::null_mut();
    // SAFETY: `source.as_ptr()` and `entry_name.as_ptr()` are valid,
    // null-terminated C strings that outlive the call; `capability` is a
    // single valid `HsCapabilityFn`; `load_token` is a live token this
    // process holds; `instance` and `out_json` are valid pointers to
    // writable storage.
    let load_status = unsafe {
        hs_load_ex(
            source.as_ptr(),
            entry_name.as_ptr(),
            &raw const capability,
            1,
            load_token,
            std::ptr::null(),
            &raw mut instance,
            &raw mut out_json,
        )
    };
    free_if_set(out_json);
    // SAFETY: `load_token` is not read again after this point until it is
    // freed, so nothing still needs it valid.
    unsafe { hs_cancel_token_free(load_token) };

    if load_status == 0 {
        let call_token = hs_cancel_token_new();
        if arguments.as_bytes().first().is_some_and(|b| b % 5 == 0) {
            // SAFETY: as above, for the call's own token.
            unsafe { hs_cancel_token_cancel(call_token) };
        }

        let mut out_text: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` is the live handle `hs_load_ex` just gave out;
        // `call_name`, `arguments`, `call_token` and `out_text` are valid as
        // above.
        let _ = unsafe {
            hs_call_ex(
                instance,
                call_name.as_ptr(),
                arguments.as_ptr(),
                call_token,
                &raw mut out_text,
            )
        };
        free_if_set(out_text);
        // SAFETY: not read again once freed.
        unsafe { hs_cancel_token_free(call_token) };

        let mut out_text: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` is still live; `call_name` and `out_text` are
        // valid.
        let _ = unsafe { hs_read(instance, call_name.as_ptr(), &raw mut out_text) };
        free_if_set(out_text);

        // SAFETY: `instance` is a handle this crate gave out, not yet freed.
        unsafe { hs_dispose(instance) };
        // Disposing twice must stay safe, the same contract `capi.rs` checks.
        // SAFETY: `hs_dispose` of an already-released handle is a no-op.
        unsafe { hs_dispose(instance) };
    }
});
