//! Lexer and parser over arbitrary bytes.
//!
//! Section 1.10 and 19.1 of the specification require that the front end
//! "never crashes, hangs or exhausts its host on any input... including
//! arbitrary bytes". This target is the direct check of that: the fuzzer's
//! bytes are decoded losslessly where possible and lossily otherwise (real
//! input need not be UTF-8, and reporting that is the lexer's own job, not
//! something this target should filter out before it gets there), then
//! lexed and parsed. Nothing here asserts a particular parse result, only
//! that neither stage panics; libFuzzer's own timeout catches a hang.

#![no_main]

use hollowscript_core::{Control, Interner, Source, SourceId};
use hollowscript_syntax::{lex, parse};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data).into_owned();
    let Ok(source) = Source::new(SourceId(0), "fuzz.hws", text) else {
        // Only `MAX_SOURCE_BYTES` refuses here, which is not a defect to fuzz.
        return;
    };
    let _ = lex(&source, &mut Control::none());
    let mut interner = Interner::new();
    let _ = parse(&source, &mut interner, &mut Control::none());
});
