//! The formatter over arbitrary bytes: never panics, and for a source that
//! parses cleanly, is idempotent (`format(format(x)) == format(x)`) and its
//! own output parses cleanly again.
//!
//! `crates/hollowscript-syntax/tests/format_fuzz.rs` and `format_roundtrip.rs`
//! already check this deterministically over the corpus and a seeded
//! mutation engine; this target complements them with libFuzzer's own
//! coverage-guided search, which explores byte shapes neither hand-written
//! mutation nor a fixed seed reaches.

#![no_main]

use hollowscript_core::{Control, Diagnostic, Interner, Source, SourceId};
use hollowscript_syntax::{format, parse};
use libfuzzer_sys::fuzz_target;

fn has_error(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data).into_owned();
    let Ok(source) = Source::new(SourceId(0), "fuzz.hws", text) else {
        return;
    };
    let mut interner = Interner::new();
    let parsed = parse(&source, &mut interner, &mut Control::none());
    if has_error(&parsed.diagnostics) {
        // The round-trip contract only binds a source that parses cleanly;
        // format's unconditional never-panics property over any input,
        // including one that does not parse, is `lex_parse`'s and
        // `check`'s job (both format arbitrary bytes too).
        let _ = format(&source, &mut interner, &mut Control::none());
        return;
    }

    let first = format(&source, &mut interner, &mut Control::none());
    let Some(once) = first.output else {
        // A clean parse can still be refused, for a comment the canonical
        // layout has no place for; that is a legitimate refusal, not a
        // property violation. `first.diagnostics` must say why.
        assert!(
            first.diagnostics.iter().any(Diagnostic::is_error),
            "format refused a cleanly parsed source without an error diagnostic"
        );
        return;
    };

    let Ok(reparsed_source) = Source::new(SourceId(0), "fuzz.hws", once.clone()) else {
        // The formatted text grew past the source size limit, which the
        // formatter itself cannot have anticipated; not a defect to fuzz.
        return;
    };
    let reparsed = parse(&reparsed_source, &mut interner, &mut Control::none());
    assert!(
        !has_error(&reparsed.diagnostics),
        "formatted output does not parse cleanly: {once:?}\ndiagnostics: {:?}",
        reparsed.diagnostics
    );

    let second = format(&reparsed_source, &mut interner, &mut Control::none());
    assert_eq!(
        second.output.as_deref(),
        Some(once.as_str()),
        "format is not idempotent"
    );
});
