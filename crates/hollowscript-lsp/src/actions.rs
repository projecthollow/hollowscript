//! `textDocument/codeAction`: a quick fix for the three diagnostics with one
//! unambiguous repair.
//!
//! * HS0401 (a call through an effectful callee with no `effect`): insert
//!   `effect ` right before the call, which is exactly where the
//!   diagnostic's own location points (`codes.rs`: "the call").
//! * HS0801/HS0802 (an unused `constant`/`variable`/import): delete the
//!   whole line, offered only when the declaration binds exactly one name
//!   (a single `constant`/`variable` target, or an `import` line with one
//!   name) so the fix is never a guess about which of several names to
//!   drop or how to reformat what is left.
//! * HS0205 (an undefined name) where the name is one of section 9.1's
//!   eight standard capabilities: add a `uses` line for it. Several `uses`
//!   lines are the same as one combined line (section 9.2), so a new line
//!   is always correct rather than an edit to an existing one, which would
//!   have to guess comma placement.

use crate::json::{Value, obj, s};
use crate::server::{Server, document_uri, span_range};
use hollowscript_core::Span;
use hollowscript_ide::symbols::word_at;
use hollowscript_syntax::ast;

/// The standard capabilities (section 9.1), the only names HS0205 gets a
/// "add uses" quick fix for: any other undefined name is at least as likely
/// a typo as a missing capability, which is not an unambiguous fix.
const STANDARD_CAPABILITIES: &[&str] = &[
    "print", "clock", "random", "stdin", "args", "env", "fs", "http",
];

pub(crate) fn code_actions(server: &Server, params: Option<&Value>) -> Value {
    let Some(uri) = document_uri(params) else {
        return Value::Array(Vec::new());
    };
    let Some(text) = server.documents().get(&uri).cloned() else {
        return Value::Array(Vec::new());
    };
    let Some(Value::Array(diagnostics)) = params
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("diagnostics"))
    else {
        return Value::Array(Vec::new());
    };
    let Some(workspace) = crate::workspace::build(&uri, &text, server.documents()) else {
        return Value::Array(Vec::new());
    };

    let mut out = Vec::new();
    for diagnostic in diagnostics {
        let Some(code) = diagnostic.get("code").and_then(Value::as_str) else {
            continue;
        };
        let Some(start) = diagnostic.get("range").and_then(|r| r.get("start")) else {
            continue;
        };
        let (Some(line), Some(character)) = (
            start.get("line").and_then(Value::as_i64),
            start.get("character").and_then(Value::as_i64),
        ) else {
            continue;
        };
        let Some(offset) = crate::server::offset_of(&text, line, character) else {
            continue;
        };
        let action = match code {
            "HS0401" => Some(add_effect(&uri, &text, offset)),
            "HS0801" | "HS0802" => remove_unused(&uri, &workspace.file, &text, offset),
            "HS0205" => add_missing_uses(&uri, &workspace.file, &text, offset),
            _ => None,
        };
        if let Some(action) = action {
            out.push(action);
        }
    }
    Value::Array(out)
}

fn one_change(uri: &str, edits: Vec<Value>) -> Value {
    let changes: std::collections::BTreeMap<String, Value> =
        [(uri.to_owned(), Value::Array(edits))]
            .into_iter()
            .collect();
    obj([("changes", Value::Object(changes))])
}

fn add_effect(uri: &str, text: &str, offset: u32) -> Value {
    let edit = obj([
        ("range", span_range(text, Span::new(offset, offset))),
        ("newText", s("effect ")),
    ]);
    obj([
        ("title", s("Add 'effect'")),
        ("kind", s("quickfix")),
        ("edit", one_change(uri, vec![edit])),
    ])
}

/// Deletes the whole line of the `constant`/`variable`/`import` declared at
/// `offset`, when it binds exactly one name.
fn remove_unused(uri: &str, file: &ast::File, text: &str, offset: u32) -> Option<Value> {
    let unambiguous = file.items.iter().any(|item| match &item.kind {
        ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
            matches!(&decl.target, ast::BindingTarget::Name(ident) if contains(ident.span, offset))
        }
        _ => false,
    }) || file.imports.iter().any(|import| {
        import.names.len() == 1
            && import
                .names
                .first()
                .is_some_and(|name| contains(name.span, offset))
    });
    if !unambiguous {
        return None;
    }
    let (word, _) = word_at(text, offset as usize)?;
    let line = whole_line_span(text, offset);
    let edit = obj([("range", span_range(text, line)), ("newText", s(""))]);
    Some(obj([
        ("title", s(format!("Remove unused '{word}'"))),
        ("kind", s("quickfix")),
        ("edit", one_change(uri, vec![edit])),
    ]))
}

/// Adds `uses <name>` as a new line at the top of the file (after any
/// existing `uses` lines, or at the very start when there are none), for
/// an undefined name that is one of the standard capabilities.
fn add_missing_uses(uri: &str, file: &ast::File, text: &str, offset: u32) -> Option<Value> {
    let (word, _) = word_at(text, offset as usize)?;
    if !STANDARD_CAPABILITIES.contains(&word.as_str()) {
        return None;
    }
    let insert_at = file
        .uses
        .last()
        .map_or(0, |last| line_start_after(text, last.span.end));
    let edit = obj([
        ("range", span_range(text, Span::new(insert_at, insert_at))),
        ("newText", s(format!("uses {word}\n"))),
    ]);
    Some(obj([
        ("title", s(format!("Add 'uses {word}'"))),
        ("kind", s("quickfix")),
        ("edit", one_change(uri, vec![edit])),
    ]))
}

fn contains(span: Span, offset: u32) -> bool {
    offset >= span.start && offset <= span.end
}

/// The offset just past the end of the line `after` falls on: where a new
/// line inserted right after it should start.
fn line_start_after(text: &str, after: u32) -> u32 {
    let bytes = text.as_bytes();
    let mut end = after as usize;
    while bytes.get(end).is_some_and(|b| *b != b'\n') {
        end += 1;
    }
    if end < bytes.len() {
        end += 1;
    }
    u32::try_from(end).unwrap_or(after)
}

/// The span of the whole line `offset` is on, newline included when there
/// is one, so deleting it leaves no blank line behind.
fn whole_line_span(text: &str, offset: u32) -> Span {
    let bytes = text.as_bytes();
    let mut start = offset as usize;
    while start > 0 && bytes.get(start - 1).is_some_and(|b| *b != b'\n') {
        start -= 1;
    }
    let mut end = offset as usize;
    while bytes.get(end).is_some_and(|b| *b != b'\n') {
        end += 1;
    }
    if end < bytes.len() {
        end += 1;
    }
    Span::new(
        u32::try_from(start).unwrap_or(0),
        u32::try_from(end).unwrap_or(0),
    )
}
