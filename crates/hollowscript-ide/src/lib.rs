//! `hollowscript-ide`: editor services for HollowScript, over the syntax and
//! check crates and nothing else.
//!
//! This crate is the part of a language server that is about the language
//! and not about a transport: it checks a document against the modules and
//! capabilities a caller supplies and answers questions about it. It reads
//! no file, speaks no protocol and keeps no state between calls, so the same
//! code serves the language server (`hollowscript-lsp`, which adds the
//! protocol and a disk) and a browser editor (through the WebAssembly build,
//! which adds a JavaScript boundary and counts positions in UTF-16).
//!
//! - [`highlight`]: lexical classes of every token and comment, from the
//!   lexer alone;
//! - [`workspace`]: [`Workspace`], one document checked with its imports
//!   loaded, from a caller's loader or an in-memory map of modules;
//! - [`symbols`]: what a name means at a position (binders, occurrences,
//!   types);
//! - [`outline`]: an outline and the foldable spans, from the syntax tree
//!   alone;
//! - [`semantic`]: classified identifiers;
//! - [`types`]: the type under a position, from the checker;
//! - [`complete`]: the names in scope at a position;
//! - [`vocabulary`]: the keywords and the prelude names;
//! - [`text`]: conversion between byte offsets and UTF-16 code units.
//!
//! **Positions are byte spans.** Everything here counts in UTF-8 bytes into
//! the text of one module, the unit of `hollowscript_core::Span`. A caller
//! that counts lines and characters, or UTF-16 code units, converts at its
//! own edge, with [`text::Utf16Map`] for the second.

#![forbid(unsafe_code)]

pub mod complete;
pub mod highlight;
pub mod outline;
pub mod semantic;
pub mod symbols;
pub mod text;
pub mod types;
pub mod vocabulary;
pub mod workspace;

pub use types::Hover;
pub use workspace::{DeclId, Export, ImportTarget, ImportedFile, ModuleLoader, Workspace};

/// Helpers the tests of every module share.
#[cfg(test)]
pub(crate) mod testing {
    use crate::Workspace;
    use hollowscript_check::CapabilityInterface;
    use std::collections::BTreeMap;

    /// A single-file workspace named `main.hws`, with no capabilities.
    pub(crate) fn workspace(text: &str) -> Workspace {
        Workspace::from_modules(
            "main.hws",
            text,
            &BTreeMap::new(),
            &CapabilityInterface::default(),
        )
        .unwrap()
    }

    /// The byte offset of the first occurrence of `needle` in `text`.
    pub(crate) fn offset(text: &str, needle: &str) -> u32 {
        u32::try_from(text.find(needle).unwrap()).unwrap()
    }
}
