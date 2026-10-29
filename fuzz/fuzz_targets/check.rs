//! The checker over arbitrary bytes, for a single-file program (an import
//! always fails to resolve, which is a fine, checked answer: section 10's
//! `HS0601`).
//!
//! Only asserts that checking terminates without panicking; a syntax error,
//! a check error, or a clean program are all acceptable. `pipeline.rs` goes
//! further, past a clean checker result to compilation and running.

#![no_main]

use hollowscript::{CapabilityInterface, NoLoader};
use hollowscript_core::Control;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = hollowscript::check(
        &source,
        "fuzz.hws",
        &CapabilityInterface::default(),
        None,
        &mut NoLoader,
        &mut Control::none(),
    );
});
