//! The JavaScript boundary for [`crate::tooling`] (feature `tooling` only):
//! `hs_wasm_check`, `hs_wasm_format` and `hs_wasm_compile`, hand-written
//! `extern "C"` exports over this module's own linear memory, exactly the
//! idiom [`crate::abi`] already uses. `check` and `format` write their JSON
//! answer through the same text channel [`crate::abi::hs_wasm_text_len`]
//! and [`crate::abi::hs_wasm_take_text`] already read; `compile` answers
//! bytes, not text, so it uses its own channel here
//! ([`hs_wasm_bytes_len`], [`hs_wasm_take_bytes`]), the same shape as the
//! text one.
//!
//! Every entry point catches a panic at the boundary, exactly as
//! [`crate::abi`]'s do, even though nothing in `hollowscript`'s own front
//! end and checker is expected to panic on any input (that is what its own
//! pipeline fuzz test is for): a boundary that a host reaches should never
//! propagate one regardless.

use crate::abi::{release, set_text, text_of};
use std::cell::RefCell;

thread_local! {
    /// The bytes [`hs_wasm_compile`] last produced, taken and released the
    /// same way [`crate::abi`]'s `TEXT` is.
    static BYTES: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn set_bytes(bytes: Vec<u8>) {
    BYTES.with_borrow_mut(|held| *held = bytes);
}

/// The length of the bytes [`hs_wasm_compile`] last produced.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_bytes_len() -> usize {
    BYTES.with_borrow(Vec::len)
}

/// Moves those bytes into a fresh buffer and answers its offset, exactly as
/// [`crate::abi::hs_wasm_take_text`] does for text. The buffer is the
/// caller's, released with `hs_wasm_free` and the length
/// [`hs_wasm_bytes_len`] gave before this call.
#[unsafe(no_mangle)]
pub extern "C" fn hs_wasm_take_bytes() -> *mut u8 {
    let bytes = BYTES.with_borrow_mut(std::mem::take);
    if bytes.is_empty() {
        return std::ptr::null_mut();
    }
    release(bytes.into_boxed_slice())
}

/// Reads `source` and `entry_name` (a null pointer or a zero length for
/// `entry_name` means `"main.hws"`) and writes [`crate::tooling::check`]'s
/// JSON array of diagnostics to the text channel. Checked against the
/// standard always-granted capabilities (`print`, `clock`, `random`), plus
/// whatever `capabilities_json` declares beyond them (a null pointer or a
/// zero length means none beyond the standard set); see
/// `crate::tooling::extra_capabilities` for its shape. `modules_json` (a
/// null pointer or a zero length means a single-file program: every import
/// "not found") is a JSON object of the entry module's own other files, by
/// canonical path, that `crate::tooling::module_loader` resolves every
/// `import` line against (section 10.3), for a Code workspace of more than
/// one file. `schema_ptr`/`schema_len` (a null pointer or a zero length means
/// none) is the host's filter schema, a JSON object in the shape of section
/// 19.4's `filterSchema` (see `crate::tooling::filter_schema`): with one, a
/// filter literal is also validated against it (Appendix A, section A.7), so
/// a field the host does not have is a diagnostic at check time.
///
/// # Safety
///
/// `source_ptr`/`source_len` must describe `source_len` readable bytes;
/// `name_ptr`/`name_len`, `capabilities_ptr`/`capabilities_len`,
/// `modules_ptr`/`modules_len` and `schema_ptr`/`schema_len` likewise, or any
/// of the four latter pointers may be null with its length 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_check(
    source_ptr: *const u8,
    source_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    capabilities_ptr: *const u8,
    capabilities_len: usize,
    modules_ptr: *const u8,
    modules_len: usize,
    schema_ptr: *const u8,
    schema_len: usize,
) -> i32 {
    // SAFETY: the caller states each describes a readable buffer of its
    // stated length, or, for the name, the capabilities, the modules and the
    // schema, may be absent; `text_of` handles a null pointer without
    // reading it.
    let (Some(source), Some(name), Some(capabilities), Some(modules), Some(schema)) = (
        unsafe { text_of(source_ptr, source_len) },
        unsafe { text_of(name_ptr, name_len) },
        unsafe { text_of(capabilities_ptr, capabilities_len) },
        unsafe { text_of(modules_ptr, modules_len) },
        unsafe { text_of(schema_ptr, schema_len) },
    ) else {
        set_text(
            "the source, the entry name, the capabilities, the modules and the filter schema \
             must be UTF-8"
                .to_owned(),
        );
        return 1;
    };
    let name = if name.is_empty() { "main.hws" } else { &name };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tooling::check(&source, name, &capabilities, &modules, &schema)
    }));
    if let Ok(json) = outcome {
        set_text(json);
        0
    } else {
        set_text("the implementation panicked".to_owned());
        1
    }
}

/// As [`hs_wasm_check`], for [`crate::tooling::format`].
///
/// # Safety
///
/// As [`hs_wasm_check`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_format(
    source_ptr: *const u8,
    source_len: usize,
    name_ptr: *const u8,
    name_len: usize,
) -> i32 {
    // SAFETY: as `hs_wasm_check`'s.
    let (Some(source), Some(name)) = (unsafe { text_of(source_ptr, source_len) }, unsafe {
        text_of(name_ptr, name_len)
    }) else {
        set_text("the source and the entry name must be UTF-8".to_owned());
        return 1;
    };
    let name = if name.is_empty() { "main.hws" } else { &name };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tooling::format(&source, name)
    }));
    if let Ok(json) = outcome {
        set_text(json);
        0
    } else {
        set_text("the implementation panicked".to_owned());
        1
    }
}

/// Compiles `source` to a bytecode container (chapter 12's `load` without
/// the run) and writes the bytes to the bytes channel
/// ([`hs_wasm_bytes_len`], [`hs_wasm_take_bytes`]), answering `0`. A
/// program with an error instead writes [`crate::tooling::check`]'s own
/// JSON diagnostics to the text channel and answers `1`, so a page never
/// has to read both channels to find out which happened.
/// `capabilities_ptr`/`capabilities_len`, `modules_ptr`/`modules_len` and
/// `schema_ptr`/`schema_len` are as [`hs_wasm_check`]'s, and a program that
/// [`hs_wasm_check`] refuses for a filter literal is refused here too.
///
/// # Safety
///
/// As [`hs_wasm_check`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_compile(
    source_ptr: *const u8,
    source_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    capabilities_ptr: *const u8,
    capabilities_len: usize,
    modules_ptr: *const u8,
    modules_len: usize,
    schema_ptr: *const u8,
    schema_len: usize,
) -> i32 {
    // SAFETY: as `hs_wasm_check`'s.
    let (Some(source), Some(name), Some(capabilities), Some(modules), Some(schema)) = (
        unsafe { text_of(source_ptr, source_len) },
        unsafe { text_of(name_ptr, name_len) },
        unsafe { text_of(capabilities_ptr, capabilities_len) },
        unsafe { text_of(modules_ptr, modules_len) },
        unsafe { text_of(schema_ptr, schema_len) },
    ) else {
        set_text(
            "the source, the entry name, the capabilities, the modules and the filter schema \
             must be UTF-8"
                .to_owned(),
        );
        return 1;
    };
    let name = if name.is_empty() { "main.hws" } else { &name };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tooling::compile(&source, name, &capabilities, &modules, &schema)
    }));
    match outcome {
        Ok(Ok(bytes)) => {
            set_bytes(bytes);
            set_text(String::new());
            0
        }
        Ok(Err(diagnostics_json)) => {
            set_text(diagnostics_json);
            1
        }
        Err(_) => {
            set_text("the implementation panicked".to_owned());
            1
        }
    }
}

/// Writes [`crate::tooling::filter_literals`]'s JSON array to the text
/// channel and answers `0`: every filter literal of the program and of the
/// modules it imports, each with its source text and its UTF-16 span, in
/// source order. A malformed `capabilities_json` or `modules_json` instead
/// writes the reason as plain text and answers `1`, so that an empty array
/// is always "no literals" and never "could not look". The arguments are
/// [`hs_wasm_check`]'s, without the filter schema.
///
/// # Safety
///
/// As [`hs_wasm_check`], for the four buffers it takes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hs_wasm_filter_literals(
    source_ptr: *const u8,
    source_len: usize,
    name_ptr: *const u8,
    name_len: usize,
    capabilities_ptr: *const u8,
    capabilities_len: usize,
    modules_ptr: *const u8,
    modules_len: usize,
) -> i32 {
    // SAFETY: as `hs_wasm_check`'s.
    let (Some(source), Some(name), Some(capabilities), Some(modules)) = (
        unsafe { text_of(source_ptr, source_len) },
        unsafe { text_of(name_ptr, name_len) },
        unsafe { text_of(capabilities_ptr, capabilities_len) },
        unsafe { text_of(modules_ptr, modules_len) },
    ) else {
        set_text(
            "the source, the entry name, the capabilities and the modules must be UTF-8".to_owned(),
        );
        return 1;
    };
    let name = if name.is_empty() { "main.hws" } else { &name };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tooling::filter_literals(&source, name, &capabilities, &modules)
    }));
    match outcome {
        Ok(Ok(json)) => {
            set_text(json);
            0
        }
        Ok(Err(reason)) => {
            set_text(reason);
            1
        }
        Err(_) => {
            set_text("the implementation panicked".to_owned());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        hs_wasm_bytes_len, hs_wasm_check, hs_wasm_compile, hs_wasm_filter_literals, hs_wasm_format,
    };
    use crate::abi::{hs_wasm_alloc, hs_wasm_free, hs_wasm_take_text, hs_wasm_text_len};

    fn upload(bytes: &[u8]) -> (*mut u8, usize) {
        let pointer = hs_wasm_alloc(bytes.len());
        assert!(!pointer.is_null());
        // SAFETY: the buffer was just allocated with exactly this length.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len()) };
        (pointer, bytes.len())
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

    fn bytes() -> Vec<u8> {
        let len = hs_wasm_bytes_len();
        let pointer = super::hs_wasm_take_bytes();
        if pointer.is_null() || len == 0 {
            return Vec::new();
        }
        // SAFETY: the pointer and length came from this module, together.
        let out = unsafe { std::slice::from_raw_parts(pointer, len) }.to_vec();
        // SAFETY: the same pointer and length, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        out
    }

    #[test]
    fn hs_wasm_check_reports_diagnostics_over_the_text_channel() {
        let (pointer, len) = upload(b"return missing");
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe {
            hs_wasm_check(
                pointer,
                len,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, 0);
        assert!(text().contains("HS0205"));
    }

    /// The standard always-granted capabilities (print, clock, random) work
    /// with no declaration at all, the same as a CLI run with no `--allow`:
    /// this is the defect the Varnook integration found (HS0404 on a plain
    /// `uses print`).
    #[test]
    fn hs_wasm_check_accepts_the_standard_capabilities_with_no_declaration() {
        let source = b"uses print\neffect print(\"hello\")\n";
        let (pointer, len) = upload(source);
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe {
            hs_wasm_check(
                pointer,
                len,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, 0);
        assert_eq!(text(), "[]");
    }

    /// A host-declared capability beyond the standard set checks once
    /// declared through `capabilities_json`, and is refused (not panicked
    /// on) without it.
    #[test]
    fn hs_wasm_check_accepts_a_declared_host_capability() {
        let source = b"uses tasks\nreturn effect tasks(\"x\")\n";
        let (pointer, len) = upload(source);
        // SAFETY: as above.
        let status = unsafe {
            hs_wasm_check(
                pointer,
                len,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, 0);
        assert!(text().contains("HS0404"), "{}", text());

        let (pointer, len) = upload(source);
        let capabilities = upload(br#"[{"name":"tasks","params":["String"],"result":"String"}]"#);
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe {
            hs_wasm_check(
                pointer,
                len,
                std::ptr::null(),
                0,
                capabilities.0,
                capabilities.1,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(pointer, len);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(capabilities.0, capabilities.1);
        }
        assert_eq!(status, 0);
        assert_eq!(text(), "[]");
    }

    #[test]
    fn hs_wasm_format_formats_over_the_text_channel() {
        let (pointer, len) = upload(b"constant x=1");
        // SAFETY: as above.
        let status = unsafe { hs_wasm_format(pointer, len, std::ptr::null(), 0) };
        // SAFETY: as above.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, 0);
        assert!(text().contains("constant x = 1"));
    }

    #[test]
    fn hs_wasm_compile_answers_bytes_that_the_vm_only_build_can_load() {
        let (pointer, len) = upload(b"export fn one(): Int { return 1 }");
        // SAFETY: as above.
        let status = unsafe {
            hs_wasm_compile(
                pointer,
                len,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: as above.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, 0);
        let container = bytes();
        assert!(!container.is_empty());
        assert!(hollowscript_bytecode::verify::verify_bytes(&container).is_ok());
    }

    /// The standard capabilities compile too, the same fix as `check`'s.
    #[test]
    fn hs_wasm_compile_accepts_the_standard_capabilities_with_no_declaration() {
        let source = b"uses print\neffect print(\"hello\")\n";
        let (pointer, len) = upload(source);
        // SAFETY: as above.
        let status = unsafe {
            hs_wasm_compile(
                pointer,
                len,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: as above.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, 0, "{}", text());
        assert!(!bytes().is_empty());
    }

    #[test]
    fn hs_wasm_compile_reports_diagnostics_for_a_program_with_an_error() {
        let (pointer, len) = upload(b"return missing");
        // SAFETY: as above.
        let status = unsafe {
            hs_wasm_compile(
                pointer,
                len,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: as above.
        unsafe { hs_wasm_free(pointer, len) };
        assert_eq!(status, 1);
        assert!(text().contains("HS0205"));
        assert!(bytes().is_empty());
    }

    /// The schema argument reaches the checker: a field the host does not
    /// have is a diagnostic only when a schema is given.
    #[test]
    fn hs_wasm_check_applies_a_filter_schema() {
        let source = b"uses tasks\nreturn effect tasks(`colour:red`)\n";
        let capabilities = br#"[{"name":"tasks","params":["Filter"],"result":"Int"}]"#;
        let schema = br#"{"fields": {"status": {"type": "text"}}}"#;
        let check = |schema: Option<&[u8]>| {
            let source = upload(source);
            let capabilities = upload(capabilities);
            let schema = schema.map(upload);
            let (schema_ptr, schema_len) = schema.unwrap_or((std::ptr::null_mut(), 0));
            // SAFETY: every buffer is live and holds exactly these bytes.
            let status = unsafe {
                hs_wasm_check(
                    source.0,
                    source.1,
                    std::ptr::null(),
                    0,
                    capabilities.0,
                    capabilities.1,
                    std::ptr::null(),
                    0,
                    schema_ptr,
                    schema_len,
                )
            };
            // SAFETY: each buffer is one this module handed out, with its length.
            unsafe {
                hs_wasm_free(source.0, source.1);
            }
            // SAFETY: as above.
            unsafe {
                hs_wasm_free(capabilities.0, capabilities.1);
            }
            // SAFETY: as above.
            unsafe {
                hs_wasm_free(schema_ptr, schema_len);
            }
            assert_eq!(status, 0);
            text()
        };
        assert_eq!(check(None), "[]");
        assert!(check(Some(schema)).contains("HS0708"));
    }

    /// `hs_wasm_filter_literals` answers the array over the text channel.
    #[test]
    fn hs_wasm_filter_literals_answers_the_literals_as_json() {
        let source = upload(b"constant a = `status:open`\nreturn a\n");
        // SAFETY: the buffer is live and holds exactly these bytes.
        let status = unsafe {
            hs_wasm_filter_literals(
                source.0,
                source.1,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
            )
        };
        // SAFETY: the same buffer, not yet freed.
        unsafe { hs_wasm_free(source.0, source.1) };
        assert_eq!(status, 0);
        let json = text();
        assert!(
            json.starts_with(r#"[{"source":"status:open","file":"main.hws","line":1"#),
            "{json}"
        );
    }

    /// A malformed argument answers `1` and the reason, never an empty array.
    #[test]
    fn hs_wasm_filter_literals_answers_the_reason_for_a_malformed_argument() {
        let source = upload(b"return 1\n");
        let modules = upload(b"not json");
        // SAFETY: both buffers are live and hold exactly these bytes.
        let status = unsafe {
            hs_wasm_filter_literals(
                source.0,
                source.1,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                modules.0,
                modules.1,
            )
        };
        // SAFETY: each buffer is one this module handed out, with its length.
        unsafe {
            hs_wasm_free(source.0, source.1);
        }
        // SAFETY: as above.
        unsafe {
            hs_wasm_free(modules.0, modules.1);
        }
        assert_eq!(status, 1);
        assert!(text().contains("modules"));
    }
}
