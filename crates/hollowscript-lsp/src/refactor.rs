//! `textDocument/references`, `textDocument/prepareRename` and
//! `textDocument/rename`.
//!
//! All three start the same way: resolve the word under the cursor to a
//! [`DeclId`], the file and span it is really declared at (an `import`
//! resolves straight through to the exporting file, see
//! [`Workspace::import_targets`]). From there, [`occurrences_across`]:
//!
//! * finds every occurrence in the declaring file with this crate's usual
//!   textual-plus-scope resolution ([`hollowscript_ide::symbols::definition_of`]);
//! * does the same in every *other open document* whose own
//!   `import_targets` points at the same [`DeclId`], so a rename reaches
//!   every file it needs to and a reference list is not silently short of
//!   the files that import the name. A file that imports the name but is
//!   not open is not reached: this server has no workspace index of files
//!   it has not been asked to load, only the ones a request or an import
//!   pulled in, which is the same limit [`Workspace`] already has for
//!   cross-file checking.

use crate::json::{Value, obj, s};
use crate::server::{Server, span_range};
use crate::workspace::{DeclId, Workspace};
use hollowscript_core::Span;
use hollowscript_ide::symbols::{Binder, definition_of, occurrences, word_at};

/// One occurrence of a resolved name, in the file it appears in, with that
/// file's own text alongside (an imported file may not be an open document
/// at all, so its text has to travel with the occurrence rather than be
/// looked up again by URI).
struct Occurrence {
    uri: String,
    text: String,
    span: Span,
}

/// `textDocument/references`: a `Location` for every [`Occurrence`]
/// [`occurrences_across`] finds for the name under the cursor, `null` where
/// the cursor is not on one.
pub(crate) fn references(server: &Server, params: Option<&Value>) -> Value {
    let Some((workspace, offset)) = server.workspace_position(params) else {
        return Value::Null;
    };
    let include_declaration = params
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("includeDeclaration"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let Some(target) = target_at(&workspace, offset) else {
        return Value::Null;
    };
    let occurrences = occurrences_across(server, &workspace, &target, include_declaration);
    let locations = occurrences.iter().map(location_value).collect();
    Value::Array(locations)
}

/// `textDocument/prepareRename`: the range and current text of the name
/// under the cursor, or `null` to refuse (the cursor is not on a name, the
/// name is a keyword, or it is one of section 13.6's prelude names, which
/// nothing declares and so nothing can rename).
pub(crate) fn prepare_rename(server: &Server, params: Option<&Value>) -> Value {
    let Some((workspace, offset)) = server.workspace_position(params) else {
        return Value::Null;
    };
    let Some((word, word_span)) = word_at(&workspace.text, offset as usize) else {
        return Value::Null;
    };
    if is_refused(&word) || target_at(&workspace, offset).is_none() {
        return Value::Null;
    }
    obj([
        ("range", span_range(&workspace.text, word_span)),
        ("placeholder", s(word)),
    ])
}

/// `textDocument/rename`: a `WorkspaceEdit` renaming every occurrence
/// [`occurrences_across`] finds, or a JSON-RPC error (`InvalidParams`,
/// -32602) refusing a keyword, a prelude name, or a name the cursor is not
/// actually on: the same refusals `prepareRename` already signals to a
/// client that asks first.
pub(crate) fn rename(server: &Server, params: Option<&Value>) -> Result<Value, (i64, String)> {
    const INVALID_PARAMS: i64 = -32602;
    let Some((workspace, offset)) = server.workspace_position(params) else {
        return Err((INVALID_PARAMS, "no document at that position".to_owned()));
    };
    let Some(new_name) = params
        .and_then(|p| p.get("newName"))
        .and_then(Value::as_str)
    else {
        return Err((INVALID_PARAMS, "missing newName".to_owned()));
    };
    let Some((word, _)) = word_at(&workspace.text, offset as usize) else {
        return Err((INVALID_PARAMS, "not a name".to_owned()));
    };
    if is_refused(&word) {
        return Err((
            INVALID_PARAMS,
            format!("'{word}' is a keyword or a prelude name and cannot be renamed"),
        ));
    }
    let Some(target) = target_at(&workspace, offset) else {
        return Err((INVALID_PARAMS, "not a declared name".to_owned()));
    };
    let occurrences = occurrences_across(server, &workspace, &target, true);

    let mut uris: Vec<String> = Vec::new();
    for occurrence in &occurrences {
        if !uris.contains(&occurrence.uri) {
            uris.push(occurrence.uri.clone());
        }
    }
    let mut changes = Vec::new();
    for uri in uris {
        let in_file: Vec<&Occurrence> = occurrences.iter().filter(|o| o.uri == uri).collect();
        let Some(text) = in_file.first().map(|o| o.text.clone()) else {
            continue;
        };
        let mut spans: Vec<Span> = in_file.iter().map(|o| o.span).collect();
        spans.sort_by_key(|s| s.start);
        let edits: Vec<Value> = spans
            .iter()
            .map(|span| {
                obj([
                    ("range", span_range(&text, *span)),
                    ("newText", s(new_name)),
                ])
            })
            .collect();
        changes.push((uri, Value::Array(edits)));
    }
    Ok(obj([(
        "changes",
        Value::Object(changes.into_iter().collect()),
    )]))
}

/// The name text is refused as a rename target: a keyword, which is not a
/// name at all, or a prelude name (section 13.6), which nothing in the
/// program declares.
fn is_refused(word: &str) -> bool {
    hollowscript_ide::vocabulary::is_keyword(word)
        || hollowscript_ide::vocabulary::is_prelude_name(word)
}

/// Where the name at `offset` in `workspace` is really declared.
fn target_at(workspace: &Workspace, offset: u32) -> Option<DeclId> {
    let (word, _) = word_at(&workspace.text, offset as usize)?;
    let binders = workspace.binders();
    definition_of(binders, &word, offset).map(|b| workspace.decl_id(&b.decl))
}

/// Every occurrence of `target`: in the file it is declared in, and in
/// every other currently open document whose imports resolve to it.
/// `include_declaration` drops the declaration's own span when `false`, the
/// option `textDocument/references` takes; a rename always wants it.
fn occurrences_across(
    server: &Server,
    workspace: &Workspace,
    target: &DeclId,
    include_declaration: bool,
) -> Vec<Occurrence> {
    let mut out = Vec::new();
    // The file that declares the name: the current document if it is its
    // own declaration, or the imported file this workspace already parsed
    // for source-level lookups otherwise. `Workspace` does not implement
    // `Clone` (it carries the checker's own HIR), so the current document's
    // case borrows `workspace` directly instead of rebuilding it.
    let declaring_uri = if target.uri == workspace.uri {
        collect(workspace, target, include_declaration, &mut out);
        Some(workspace.uri.clone())
    } else if let Some(declaring) = server.workspace_for_uri(&target.uri) {
        collect(&declaring, target, include_declaration, &mut out);
        Some(declaring.uri.clone())
    } else {
        None
    };
    for uri in server.documents().keys() {
        if Some(uri) == declaring_uri.as_ref() {
            continue; // already covered above
        }
        if *uri == workspace.uri {
            if workspace.import_targets.values().any(|d| d == target) {
                collect(workspace, target, include_declaration, &mut out);
            }
            continue;
        }
        let Some(other) = server.workspace_for_uri(uri) else {
            continue;
        };
        if other.import_targets.values().any(|d| d == target) {
            collect(&other, target, include_declaration, &mut out);
        }
    }
    out
}

fn collect(
    workspace: &Workspace,
    target: &DeclId,
    include_declaration: bool,
    out: &mut Vec<Occurrence>,
) {
    let binders = workspace.binders();
    for span in occurrence_spans(workspace, binders, target, include_declaration) {
        out.push(Occurrence {
            uri: workspace.uri.clone(),
            text: workspace.text.clone(),
            span,
        });
    }
}

/// Every span in `workspace` that names `target`: each binder whose own
/// `decl` is `target` (unless `include_declaration` is `false`), plus every
/// use [`occurrences`] finds that resolves, at its own position, to the
/// same declaration.
fn occurrence_spans(
    workspace: &Workspace,
    binders: &[Binder],
    target: &DeclId,
    include_declaration: bool,
) -> Vec<Span> {
    let mut out = Vec::new();
    if include_declaration {
        for binder in binders {
            if workspace.decl_id(&binder.decl) == *target {
                out.push(binder.at);
            }
        }
    }
    for ident in occurrences(&workspace.file) {
        let name = workspace.names.resolve(ident.name);
        let Some(binder) = definition_of(binders, name, ident.span.start) else {
            continue;
        };
        if workspace.decl_id(&binder.decl) == *target {
            out.push(ident.span);
        }
    }
    out.sort_by_key(|s| s.start);
    out.dedup();
    out
}

fn location_value(occurrence: &Occurrence) -> Value {
    obj([
        ("uri", s(occurrence.uri.clone())),
        ("range", span_range(&occurrence.text, occurrence.span)),
    ])
}
