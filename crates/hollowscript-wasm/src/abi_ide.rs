//! The JavaScript boundary for `crate::tooling::ide` (feature `tooling`
//! only): hand-written `extern "C"` exports over this module's own linear
//! memory, exactly the idiom [`crate::abi_tooling`] already uses. Every
//! answer is JSON written to the same text channel
//! ([`crate::abi::hs_wasm_text_len`], [`crate::abi::hs_wasm_take_text`]).
//!
//! Every entry point catches a panic at the boundary, as the others do.

use crate::abi::{Status, set_text, text_of};

/// Runs `body`, which answers JSON text, and writes it to the text channel.
/// Answers [`Status::Ok`], or [`Status::Panicked`] with the usual message
/// if `body` panicked.
fn answer(body: impl FnOnce() -> String) -> i32 {
    if let Ok(json) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        set_text(json);
        Status::Ok as i32
    } else {
        set_text("the implementation panicked".to_owned());
        Status::Panicked as i32
    }
}

/// Writes `crate::tooling::ide::highlight_json` of the source to the text
/// channel. Stateless: needs no handle and keeps nothing.
///
/// # Safety
///
/// `source_ptr`/`source_len` must describe `source_len` readable bytes, or
/// the pointer may be null with a length of 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_highlight(source_ptr: *const u8, source_len: usize) -> i32 {
    // SAFETY: the caller states this describes `source_len` readable bytes
    // or is null with length 0; `text_of` handles both without reading past
    // them.
    let Some(source) = (unsafe { text_of(source_ptr, source_len) }) else {
        set_text("the source must be UTF-8".to_owned());
        return Status::Refused as i32;
    };
    answer(|| crate::tooling::ide::highlight_json(&source))
}

/// Writes `crate::tooling::ide::keywords_json` to the text channel.
/// Stateless.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_keywords() -> i32 {
    answer(crate::tooling::ide::keywords_json)
}

/// Writes `crate::tooling::ide::standard_capabilities_json` to the text
/// channel. Stateless.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_standard_capabilities() -> i32 {
    answer(crate::tooling::ide::standard_capabilities_json)
}

/// Checks the source and holds the result as an analysis, answering its
/// handle (never 0), or answering 0 with the reason in the text channel. The
/// name (a null pointer or a zero length means `main.hws`), the capabilities
/// and the modules are read exactly as [`crate::abi_tooling::hs_wasm_check`]
/// reads them. The host releases the analysis with
/// [`hs_wasm_analysis_dispose`].
///
/// # Safety
///
/// `source_ptr`/`source_len` must describe `source_len` readable bytes;
/// `name_ptr`/`name_len`, `capabilities_ptr`/`capabilities_len` and
/// `modules_ptr`/`modules_len` likewise, or any of the latter three pointers
/// may be null with its length 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_analyse(
    source_ptr: *const u8,
    source_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    capabilities_ptr: *const u8,
    capabilities_len: usize,
    modules_ptr: *const u8,
    modules_len: usize,
) -> u32 {
    // SAFETY: the caller states each describes a readable buffer of its
    // stated length, or, for the name, the capabilities and the modules, may
    // be absent; `text_of` handles a null pointer without reading it.
    let (Some(source), Some(name), Some(capabilities), Some(modules)) = (
        unsafe { text_of(source_ptr, source_len) },
        unsafe { text_of(name_ptr, name_len) },
        unsafe { text_of(capabilities_ptr, capabilities_len) },
        unsafe { text_of(modules_ptr, modules_len) },
    ) else {
        set_text(
            "the source, the entry name, the capabilities and the modules must be UTF-8".to_owned(),
        );
        return 0;
    };
    let name = if name.is_empty() { "main.hws" } else { &name };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tooling::ide::analyse(&source, name, &capabilities, &modules)
    }));
    match outcome {
        Ok(Ok(handle)) => {
            set_text(String::new());
            handle
        }
        Ok(Err(reason)) => {
            set_text(reason);
            0
        }
        Err(_) => {
            set_text("the implementation panicked".to_owned());
            0
        }
    }
}

/// Runs `query` against the analysis `handle` names and writes its JSON to
/// the text channel, or refuses a handle that names none.
fn query(
    handle: u32,
    query: impl FnOnce(&crate::tooling::ide::Analysis) -> String + std::panic::UnwindSafe,
) -> i32 {
    let outcome = std::panic::catch_unwind(|| crate::tooling::ide::with_analysis(handle, query));
    match outcome {
        Ok(Some(json)) => {
            set_text(json);
            Status::Ok as i32
        }
        Ok(None) => {
            set_text("that handle names no live analysis".to_owned());
            Status::Refused as i32
        }
        Err(_) => {
            set_text("the implementation panicked".to_owned());
            Status::Panicked as i32
        }
    }
}

/// Writes the analysis's hover at the UTF-16 `offset` to the text channel:
/// `{"start","end","type"}` or `null`.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_analysis_hover(handle: u32, offset: u32) -> i32 {
    query(handle, move |analysis| analysis.hover_json(offset))
}

/// Writes the analysis's completions at the UTF-16 `offset` to the text
/// channel: `[{"label","kind","detail"}]`.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_analysis_completions(handle: u32, offset: u32) -> i32 {
    query(handle, move |analysis| analysis.completions_json(offset))
}

/// Writes the analysis's definition of the name at the UTF-16 `offset` to the
/// text channel: `{"file","start","end"}` or `null`.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_analysis_definition(handle: u32, offset: u32) -> i32 {
    query(handle, move |analysis| analysis.definition_json(offset))
}

/// Writes the analysis's outline to the text channel, a tree of symbols.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_analysis_symbols(handle: u32) -> i32 {
    query(handle, crate::tooling::ide::Analysis::symbols_json)
}

/// Ends an analysis and releases what it holds. A handle that names none is
/// ignored, so a host may call this twice.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_analysis_dispose(handle: u32) -> i32 {
    crate::tooling::ide::dispose(handle);
    Status::Ok as i32
}

#[cfg(test)]
mod tests {
    use super::{
        hs_wasm_analyse, hs_wasm_analysis_completions, hs_wasm_analysis_definition,
        hs_wasm_analysis_dispose, hs_wasm_analysis_hover, hs_wasm_analysis_symbols,
        hs_wasm_highlight, hs_wasm_keywords, hs_wasm_standard_capabilities,
    };
    use crate::abi::{hs_wasm_alloc, hs_wasm_free, hs_wasm_take_text, hs_wasm_text_len};

    fn upload(bytes: &[u8]) -> (*mut u8, usize) {
        if bytes.is_empty() {
            return (std::ptr::null_mut(), 0);
        }
        let pointer = hs_wasm_alloc(bytes.len());
        assert!(!pointer.is_null());
        // SAFETY: the buffer was just allocated with exactly this length.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len()) };
        (pointer, bytes.len())
    }

    /// Calls `hs_wasm_analyse` with the given source, capabilities and
    /// modules (the entry name left to its default), answering the handle.
    fn analyse(source: &str, capabilities: &str, modules: &str) -> u32 {
        let source = upload(source.as_bytes());
        let capabilities = upload(capabilities.as_bytes());
        let modules = upload(modules.as_bytes());
        // SAFETY: each buffer is live and holds exactly these bytes, and the
        // name pointer is null with a length of 0.
        let handle = unsafe {
            hs_wasm_analyse(
                source.0,
                source.1,
                std::ptr::null(),
                0,
                capabilities.0,
                capabilities.1,
                modules.0,
                modules.1,
            )
        };
        for buffer in [source, capabilities, modules] {
            // SAFETY: each buffer is one this module handed out, with its length.
            unsafe { hs_wasm_free(buffer.0, buffer.1) };
        }
        handle
    }

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

    #[test]
    fn hs_wasm_highlight_answers_classes_over_the_text_channel() {
        let source = b"return 1";
        let pointer = hs_wasm_alloc(source.len());
        // SAFETY: the buffer was just allocated with exactly this length.
        unsafe { std::ptr::copy_nonoverlapping(source.as_ptr(), pointer, source.len()) };
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe { hs_wasm_highlight(pointer, source.len()) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, source.len()) };
        assert_eq!(status, 0);
        assert_eq!(
            text(),
            r#"[{"start":0,"end":6,"class":"keyword"},{"start":7,"end":8,"class":"number"}]"#
        );
    }

    #[test]
    fn hs_wasm_highlight_of_an_empty_source_is_an_empty_array() {
        // SAFETY: a null pointer with a length of 0 is allowed.
        let status = unsafe { hs_wasm_highlight(std::ptr::null(), 0) };
        assert_eq!(status, 0);
        assert_eq!(text(), "[]");
    }

    #[test]
    fn hs_wasm_highlight_refuses_text_that_is_not_utf8() {
        let bytes = [0xffu8, 0xfe];
        let pointer = hs_wasm_alloc(bytes.len());
        // SAFETY: the buffer was just allocated with exactly this length.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len()) };
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe { hs_wasm_highlight(pointer, bytes.len()) };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, bytes.len()) };
        assert_eq!(status, 1);
        assert!(text().contains("UTF-8"));
    }

    #[test]
    fn hs_wasm_keywords_answers_the_keyword_table() {
        assert_eq!(hs_wasm_keywords(), 0);
        let json = text();
        assert!(
            json.starts_with(r#"[{"word":"and","future":false}"#),
            "{json}"
        );
        assert!(
            json.contains(r#"{"word":"assert","future":true}"#),
            "{json}"
        );
    }

    #[test]
    fn hs_wasm_analyse_then_every_query_then_dispose() {
        let source = "constant face = \"\u{1f600}\"\nconstant total = 1 + 2\nreturn total\n";
        let handle = analyse(source, "", "");
        assert_ne!(handle, 0, "{}", text());
        assert_eq!(text(), "");
        let plus =
            u32::try_from(source[..source.find("+ 2").unwrap()].encode_utf16().count()).unwrap();

        assert_eq!(hs_wasm_analysis_hover(handle, plus), 0);
        assert_eq!(text(), r#"{"start":38,"end":43,"type":"Int"}"#);

        assert_eq!(hs_wasm_analysis_completions(handle, plus), 0);
        let completions = text();
        assert!(
            completions.starts_with(r#"[{"label":"face","kind":"constant","detail":"String"}"#),
            "{completions}"
        );

        assert_eq!(hs_wasm_analysis_definition(handle, plus), 0);
        assert_eq!(text(), "null");

        assert_eq!(hs_wasm_analysis_symbols(handle), 0);
        let symbols = text();
        assert!(
            symbols.contains(r#""name":"total","kind":"constant","detail":"Int""#),
            "{symbols}"
        );

        assert_eq!(hs_wasm_analysis_dispose(handle), 0);
        // Disposed: every query is refused, and disposing again is fine.
        assert_eq!(hs_wasm_analysis_hover(handle, 0), 1);
        assert!(text().contains("no live analysis"));
        assert_eq!(hs_wasm_analysis_completions(handle, 0), 1);
        assert_eq!(hs_wasm_analysis_definition(handle, 0), 1);
        assert_eq!(hs_wasm_analysis_symbols(handle), 1);
        assert_eq!(hs_wasm_analysis_dispose(handle), 0);
    }

    #[test]
    fn hs_wasm_analyse_refuses_a_malformed_declaration_with_the_reason() {
        assert_eq!(analyse("return 1", "{", ""), 0);
        assert!(text().contains("capabilit"));
        assert_eq!(analyse("return 1", "", "["), 0);
        assert!(text().contains("modules"));
    }

    #[test]
    fn hs_wasm_analyse_refuses_bytes_that_are_not_utf8() {
        let bytes = upload(&[0xff, 0xfe]);
        // SAFETY: the buffer is live and holds exactly these bytes, and the
        // other pointers are null with a length of 0.
        let handle = unsafe {
            hs_wasm_analyse(
                bytes.0,
                bytes.1,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: the buffer is one this module handed out, with its length.
        unsafe { hs_wasm_free(bytes.0, bytes.1) };
        assert_eq!(handle, 0);
        assert!(text().contains("UTF-8"));
    }

    #[test]
    fn hs_wasm_analyse_resolves_imports_against_the_modules() {
        let modules = r#"{"util.hws":"export constant two = 2\n"}"#;
        let source = "import two from \"./util\"\nreturn two\n";
        let handle = analyse(source, "", modules);
        assert_ne!(handle, 0, "{}", text());
        let at = u32::try_from(source.find("two\n").unwrap()).unwrap();
        assert_eq!(hs_wasm_analysis_definition(handle, at), 0);
        assert_eq!(text(), r#"{"file":"util.hws","start":16,"end":19}"#);
        assert_eq!(hs_wasm_analysis_dispose(handle), 0);
    }

    #[test]
    fn hs_wasm_standard_capabilities_answers_the_declarations() {
        assert_eq!(hs_wasm_standard_capabilities(), 0);
        let json = text();
        assert!(
            json.starts_with(r#"[{"name":"print","params":["String"]},{"name":"clock","function":"now","params":[],"result":"String"}"#),
            "{json}"
        );
    }
}
