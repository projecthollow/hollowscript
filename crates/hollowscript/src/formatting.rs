//! The canonical formatter, exposed through the facade.
//!
//! `hollowscript-syntax` does the real work; this module only builds a
//! [`hollowscript_core::Source`] from plain text the way [`crate::instance::check`]
//! does, so a host that only wants to format a file, such as the C ABI, can
//! reach it without depending on `hollowscript-syntax` directly. It cannot:
//! `hollowscript-capi`'s allowed dependencies are `hollowscript` and
//! `hollowscript-core` only (`docs/design/architecture.md`, enforced by
//! `xtask graph`), on purpose, to keep the C ABI's surface deliberately small
//! and everything it can reach going through one crate.

pub use hollowscript_syntax::FormatOutput;

use hollowscript_core::cancel::Control;
use hollowscript_core::{Interner, Source, SourceId};
use std::sync::Arc;

/// Formats `source` (the entry module named `entry_name`), canonicalising
/// its layout. Never panics: a syntax error, or a comment the layout has no
/// place to keep, is reported in the result's diagnostics rather than
/// stopping the call. This formats one file only; a formatter has no need of
/// imports, unlike [`crate::instance::check`].
pub fn format(source: &str, entry_name: &str, control: &mut Control<'_>) -> FormatOutput {
    let mut interner = Interner::new();
    let src = match Source::new(SourceId(0), entry_name, source) {
        Ok(src) => src,
        Err(e) => {
            return FormatOutput {
                output: None,
                diagnostics: vec![e.to_diagnostic(&Arc::from(entry_name))],
            };
        }
    };
    hollowscript_syntax::format(&src, &mut interner, control)
}

#[cfg(test)]
mod tests {
    use super::format;
    use hollowscript_core::cancel::Control;

    #[test]
    fn formats_a_clean_source() {
        let out = format("constant x=1\n", "main.hws", &mut Control::none());
        assert_eq!(out.output.as_deref(), Some("constant x = 1\n"));
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn refuses_a_source_with_a_syntax_error_and_says_why() {
        let out = format("constant x =\n", "main.hws", &mut Control::none());
        assert!(out.output.is_none());
        assert!(!out.diagnostics.is_empty());
    }

    #[test]
    fn a_source_over_the_size_limit_is_reported_not_panicked() {
        let huge = "a".repeat(hollowscript_core::source::MAX_SOURCE_BYTES + 1);
        let out = format(&huge, "main.hws", &mut Control::none());
        assert!(out.output.is_none());
        assert_eq!(out.diagnostics.len(), 1);
    }
}
