//! `textDocument/documentSymbol` and `textDocument/foldingRange`: the
//! protocol face of [`hollowscript_ide`]'s outline and fold spans, which are
//! built straight from the syntax tree with no need for the checker (an
//! outline has to work for a file that does not check yet, which is most of
//! the time someone is looking at one).

use crate::json::{Value, n, obj, s};
use crate::server::{Server, span_range};
use hollowscript_ide::outline::{OutlineItem, SymbolKind};

/// `textDocument/documentSymbol`: one hierarchical `DocumentSymbol` per
/// name a `uses` line declares, per `import` line, and per top-level `fn`,
/// `record`, `enum`, `constant` or `variable`, in source order, with its
/// signature or type as the `detail`; a record's fields and an enum's
/// variants are its children.
pub(crate) fn document_symbols(server: &Server, params: Option<&Value>) -> Value {
    let Some(workspace) = server.workspace_of(params) else {
        return Value::Null;
    };
    let text = &workspace.text;
    Value::Array(
        workspace
            .document_symbols()
            .iter()
            .map(|item| document_symbol(item, text))
            .collect(),
    )
}

/// The protocol's own `SymbolKind` numbering.
const fn symbol_kind(kind: SymbolKind) -> i64 {
    match kind {
        SymbolKind::Module => 2,
        SymbolKind::Capability => 3,
        SymbolKind::Function => 12,
        SymbolKind::Record => 23,
        SymbolKind::Enum => 10,
        SymbolKind::Constant => 14,
        SymbolKind::Variable => 13,
        SymbolKind::Field => 8,
        SymbolKind::EnumMember => 22,
    }
}

fn document_symbol(item: &OutlineItem, text: &str) -> Value {
    let mut fields = vec![
        ("name", s(item.name.as_str())),
        ("kind", n(symbol_kind(item.kind))),
        ("range", span_range(text, item.range)),
        ("selectionRange", span_range(text, item.selection)),
    ];
    if !item.detail.is_empty() {
        fields.push(("detail", s(item.detail.as_str())));
    }
    if !item.children.is_empty() {
        fields.push((
            "children",
            Value::Array(
                item.children
                    .iter()
                    .map(|child| document_symbol(child, text))
                    .collect(),
            ),
        ));
    }
    obj(fields)
}

/// `textDocument/foldingRange`: one range per block that spans more than
/// one line (a `fn` body, an `if`/`for`/`while`/`match` block, a `record`
/// or `enum`'s braces).
pub(crate) fn folding_ranges(server: &Server, params: Option<&Value>) -> Value {
    let Some(workspace) = server.workspace_of(params) else {
        return Value::Null;
    };
    let text = &workspace.text;
    Value::Array(
        workspace
            .folding_spans()
            .into_iter()
            .map(|span| {
                obj([
                    ("startLine", n(line_of(text, span.start))),
                    ("endLine", n(line_of(text, span.end))),
                ])
            })
            .collect(),
    )
}

fn line_of(text: &str, offset: u32) -> i64 {
    let end = (offset as usize).min(text.len());
    i64::try_from(text.get(..end).map_or(0, |s| s.matches('\n').count())).unwrap_or(0)
}
