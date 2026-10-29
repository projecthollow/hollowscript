//! The WebAssembly ABI (`hollowscript-wasm::abi`) called with arbitrary
//! bytecode bytes and arguments, the way a browser host's JavaScript, which
//! this crate can never trust to have supplied a well-formed buffer, would
//! call it.
//!
//! This target runs the exported `extern "C"` functions directly on the
//! host rather than through an actual WebAssembly instance, which is fine:
//! the functions themselves are the whole boundary (`abi.rs`'s own doc
//! comment), and nothing about their behaviour depends on running inside a
//! `wasm32` target. `hs_wasm_load` is given the fuzzer's bytes as the
//! bytecode buffer directly; on success, `hs_wasm_call` and `hs_wasm_read`
//! are tried against a fixed export name with the tail of the input as the
//! JSON arguments, and `hs_wasm_dispose` is called twice, matching its
//! documented "a host may call this twice" contract.

#![no_main]

use hollowscript_wasm::abi::{
    hs_wasm_call, hs_wasm_dispose, hs_wasm_load, hs_wasm_read, hs_wasm_set_limits,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Keeps every fuzzed module's run bounded, same as `pipeline.rs` and
    // `bytecode_verify.rs`: a module that loops forever must reach the step
    // limit, not libFuzzer's own timeout.
    hs_wasm_set_limits(200_000, 4_000_000, 100_000, 64);

    let split = data.len() / 2;
    let (bytecode, args) = data.split_at(split.min(data.len()));
    let name = b"main";

    // SAFETY: `bytecode.as_ptr()` and `bytecode.len()` describe a buffer of
    // that many readable bytes, since it is a slice this process owns.
    let handle = unsafe { hs_wasm_load(bytecode.as_ptr(), bytecode.len()) };
    if handle == 0 {
        return;
    }

    // SAFETY: `name` and `args` each describe a buffer of their own length
    // that this process owns and is readable for the call's duration.
    let _ = unsafe { hs_wasm_call(handle, name.as_ptr(), name.len(), args.as_ptr(), args.len()) };
    // SAFETY: as above, for the one buffer.
    let _ = unsafe { hs_wasm_read(handle, name.as_ptr(), name.len()) };

    // No preconditions: a handle naming no live instance is documented as
    // ignored, which is exactly what a second dispose exercises.
    hs_wasm_dispose(handle);
    hs_wasm_dispose(handle);
});
