//! Applying a `didOpen` or `didChange` notification to a document's stored
//! text.
//!
//! `initialize` advertises `TextDocumentSyncKind.Incremental` (2), so a
//! `didChange` from a real client carries each edit as a `range` and the
//! text that replaces it rather than the whole document; this is what makes
//! that affordable for a large file, since a keystroke no longer resends
//! it. A `contentChanges` entry with no `range` is still accepted as a full
//! replacement (the protocol allows a client to send one even under
//! incremental sync, and this server's own tests use it as the simplest
//! way to open a document), and several entries are applied in order, which
//! is how the protocol defines them.

use crate::json::Value;

/// The document's new text after a `textDocument/didOpen` or
/// `textDocument/didChange` notification's `params`. `previous` is the text
/// this server has for the document already (`None` for `didOpen`, where
/// there is nothing to patch).
pub(crate) fn apply_change(previous: Option<&String>, params: Option<&Value>) -> Option<String> {
    let doc = params?.get("textDocument")?;
    if let Some(text) = doc.get("text").and_then(Value::as_str) {
        // `didOpen`: the document's whole text, the only place it appears.
        return Some(text.to_owned());
    }
    let changes = params?.get("contentChanges")?;
    let Value::Array(items) = changes else {
        return None;
    };
    let mut text = previous.cloned().unwrap_or_default();
    for item in items {
        text = apply_one_change(&text, item)?;
    }
    Some(text)
}

/// One `TextDocumentContentChangeEvent` applied to `text`.
fn apply_one_change(text: &str, item: &Value) -> Option<String> {
    let new_text = item.get("text")?.as_str()?;
    let Some(range) = item.get("range") else {
        // No `range`: a full replacement, valid under either sync kind.
        return Some(new_text.to_owned());
    };
    let start = range.get("start")?;
    let end = range.get("end")?;
    let start_offset = crate::server::offset_of(
        text,
        start.get("line")?.as_i64()?,
        start.get("character")?.as_i64()?,
    )? as usize;
    let end_offset = crate::server::offset_of(
        text,
        end.get("line")?.as_i64()?,
        end.get("character")?.as_i64()?,
    )? as usize;
    let (start_offset, end_offset) = if start_offset <= end_offset {
        (start_offset, end_offset)
    } else {
        (end_offset, start_offset)
    };
    let mut out = String::with_capacity(text.len() + new_text.len());
    out.push_str(text.get(..start_offset)?);
    out.push_str(new_text);
    out.push_str(text.get(end_offset..)?);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::apply_change;
    use crate::json::parse;

    fn params(text: &str) -> crate::json::Value {
        parse(text).unwrap()
    }

    #[test]
    fn did_open_is_the_documents_whole_text() {
        let p = params(r#"{"textDocument":{"uri":"file:///a.hws","text":"return 1"}}"#);
        assert_eq!(apply_change(None, Some(&p)), Some("return 1".to_owned()));
    }

    #[test]
    fn a_full_replacement_change_has_no_range() {
        let p = params(
            r#"{"textDocument":{"uri":"file:///a.hws"},"contentChanges":[{"text":"return 2"}]}"#,
        );
        let previous = "return 1".to_owned();
        assert_eq!(
            apply_change(Some(&previous), Some(&p)),
            Some("return 2".to_owned())
        );
    }

    #[test]
    fn an_incremental_change_replaces_only_its_range() {
        // "constant x = 1" -> "constant x = 12": insert "2" right after the
        // trailing digit, at line 0 character 14 (the end of the line).
        let p = params(
            r#"{"textDocument":{"uri":"file:///a.hws"},"contentChanges":[{"range":{"start":{"line":0,"character":14},"end":{"line":0,"character":14}},"text":"2"}]}"#,
        );
        let previous = "constant x = 1".to_owned();
        assert_eq!(
            apply_change(Some(&previous), Some(&p)),
            Some("constant x = 12".to_owned())
        );
    }

    #[test]
    fn several_incremental_changes_apply_in_order() {
        let p = params(concat!(
            r#"{"textDocument":{"uri":"file:///a.hws"},"contentChanges":["#,
            r#"{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"text":"a"},"#,
            r#"{"range":{"start":{"line":0,"character":1},"end":{"line":0,"character":1}},"text":"b"}"#,
            r#"]}"#,
        ));
        let previous = String::new();
        assert_eq!(
            apply_change(Some(&previous), Some(&p)),
            Some("ab".to_owned())
        );
    }

    #[test]
    fn a_range_that_deletes_text_shrinks_the_document() {
        // "constant xxxxxx = 1": the six `x`s are characters 9 to 15.
        let p = params(
            r#"{"textDocument":{"uri":"file:///a.hws"},"contentChanges":[{"range":{"start":{"line":0,"character":9},"end":{"line":0,"character":15}},"text":""}]}"#,
        );
        let previous = "constant xxxxxx = 1".to_owned();
        assert_eq!(
            apply_change(Some(&previous), Some(&p)),
            Some("constant  = 1".to_owned())
        );
    }
}
