//! The C ABI (`hollowscript-capi`) called with arbitrary bytes and
//! arguments, the way a host that is not Rust, and so cannot be trusted to
//! pass well-formed input, would call it.
//!
//! The fuzzer's bytes are split into a source, an entry name and a JSON
//! arguments text (see `split`), each turned into a `CString` (dropping any
//! interior NUL, since a real C caller's string cannot contain one either).
//! Every exported function is then called in the order a host's own load,
//! call, read, dispose sequence would use, with a fixed small export name so
//! that a JSON argument list generated for `main` sometimes reaches
//! `hs_call`. The property is the same as every other target here: nothing
//! panics, and `hs_dispose` may always be called, more than once, whatever
//! came before it, without the process crashing.

#![no_main]

use hollowscript_capi::{hs_call, hs_check_json, hs_dispose, hs_format_json, hs_load, hs_read};
use libfuzzer_sys::fuzz_target;
use std::ffi::CString;
use std::os::raw::c_char;

/// Turns arbitrary bytes into a `CString`, dropping embedded NUL bytes
/// rather than refusing them: a fuzzer's byte string finding the one
/// sequence that reads as NUL-free text is not the interesting part of this
/// target, so the split is done on this side instead of relying on chance.
fn to_cstring(bytes: &[u8]) -> CString {
    let clean: Vec<u8> = bytes.iter().copied().filter(|&b| b != 0).collect();
    // SAFETY-free: `clean` has no interior NUL by construction, so this
    // cannot fail.
    CString::new(clean).unwrap_or_default()
}

/// Splits `data` on the first NUL byte (before NUL-stripping) into a source
/// text and an arguments text, so unrelated fuzzer input does not always
/// collide into the same string for both.
fn split(data: &[u8]) -> (CString, CString) {
    let mid = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let (a, b) = data.split_at(mid);
    (to_cstring(a), to_cstring(b))
}

fuzz_target!(|data: &[u8]| {
    let (source, arguments) = split(data);
    let entry_name = c"fuzz.hws";
    let call_name = c"main";

    let mut out_json: *mut c_char = std::ptr::null_mut();
    // SAFETY: `source.as_ptr()` and `entry_name.as_ptr()` are valid,
    // null-terminated C strings that outlive the call; `out_json` is a
    // valid pointer to writable storage.
    let status = unsafe { hs_check_json(source.as_ptr(), entry_name.as_ptr(), &raw mut out_json) };
    free_if_set(out_json);

    let mut out_json: *mut c_char = std::ptr::null_mut();
    // SAFETY: as above.
    let _ = unsafe { hs_format_json(source.as_ptr(), entry_name.as_ptr(), &raw mut out_json) };
    free_if_set(out_json);

    if status != 3 {
        // Not `CheckFailed`: `hs_load` is worth trying too, though it will
        // reach the same answer through its own path.
    }
    let mut instance: *mut hollowscript_capi::HsInstance = std::ptr::null_mut();
    let mut out_json: *mut c_char = std::ptr::null_mut();
    // SAFETY: all four pointers are valid by construction and outlive the call.
    let load_status = unsafe {
        hs_load(
            source.as_ptr(),
            entry_name.as_ptr(),
            &raw mut instance,
            &raw mut out_json,
        )
    };
    free_if_set(out_json);

    if load_status == 0 {
        // HsStatus::Ok: an instance was created.
        let mut out_text: *mut c_char = std::ptr::null_mut();
        // SAFETY: `instance` is the live handle `hs_load` just gave out;
        // `call_name`, `arguments` and `out_text` are valid as above.
        let _ = unsafe {
            hs_call(
                instance,
                call_name.as_ptr(),
                arguments.as_ptr(),
                &raw mut out_text,
            )
        };
        free_if_set(out_text);

        let mut out_text: *mut c_char = std::ptr::null_mut();
        // SAFETY: as above.
        let _ = unsafe { hs_read(instance, call_name.as_ptr(), &raw mut out_text) };
        free_if_set(out_text);

        // SAFETY: `instance` is a handle this crate gave out, not yet freed.
        unsafe { hs_dispose(instance) };
        // Disposing twice must stay safe: it is documented as a no-op the
        // second time, which this asserts by simply not crashing.
        // SAFETY: `hs_dispose` of an already-released handle is a no-op.
        unsafe { hs_dispose(instance) };
    }
});

fn free_if_set(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: `ptr` is either null (handled above) or a string one of
        // this crate's own `*_json` functions returned and has not been
        // freed yet.
        unsafe { hollowscript_capi::hs_string_free(ptr) };
    }
}
