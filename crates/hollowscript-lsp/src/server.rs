//! The server's state and method dispatch.
//!
//! Every request gets a JSON-RPC response, even a method this build does not
//! implement yet: it gets the standard `MethodNotFound` error rather than
//! being dropped, which is what a conforming server does for a method
//! outside its declared capabilities. `initialize` is real, and so is
//! `textDocument/didOpen` and `didChange`: they run the front end (parse,
//! then check) on the document's text and publish the result as LSP
//! diagnostics, now backed by a real checker. `textDocument/formatting`
//! needs no checker, so it works the same either way: it runs the real
//! formatter and returns one `TextEdit` replacing the whole document, or
//! `null` when the formatter refuses (a syntax error, or a comment with no
//! place to keep, as [`hollowscript_syntax::format()`] documents).

use crate::json::{Value, n, obj, s};
use crate::workspace::Workspace;
use hollowscript_core::{Diagnostic, Severity};
use std::collections::BTreeMap;

/// JSON-RPC: the method does not exist or is not available (LSP and
/// JSON-RPC's own reserved code).
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC: the message was not valid JSON.
const PARSE_ERROR: i64 = -32700;

/// The server's state: the text of every document the client has open, by URI.
#[derive(Debug, Default)]
pub(crate) struct Server {
    documents: BTreeMap<String, String>,
    shutting_down: bool,
}

/// One outgoing JSON-RPC message: a response to a request, or a notification
/// the server sends on its own (`textDocument/publishDiagnostics`).
#[derive(Debug)]
pub(crate) struct Outgoing {
    pub(crate) body: Value,
}

impl Server {
    #[must_use]
    pub(crate) fn new() -> Server {
        Server::default()
    }

    /// Whether `exit` has been received and the server's stdio loop should stop.
    #[must_use]
    pub(crate) fn should_exit(method: &str) -> bool {
        method == "exit"
    }

    /// Handles one incoming JSON-RPC message (already parsed) and returns
    /// zero or more outgoing messages: a response for a request (something
    /// with an `id`), diagnostics notifications it triggers, or nothing for
    /// a notification this build does not act on.
    pub(crate) fn handle(&mut self, message: &Value) -> Vec<Outgoing> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let id = message.get("id").cloned();
        let params = message.get("params");

        let response_body = match method {
            "initialize" => Some(Server::initialize()),
            // Notifications with nothing to acknowledge. `$/cancelRequest`
            // is named explicitly rather than falling into the generic
            // unknown-notification case below: this server handles one
            // message at a time on a single thread, so by the time a
            // client's cancellation is read from standard input the request
            // it names has always already been answered, and there is
            // nothing further to do; a future concurrent server would give
            // this arm a real cancellation set to fill in.
            "initialized" | "exit" | "$/cancelRequest" => None,
            "shutdown" => {
                self.shutting_down = true;
                Some(Value::Null)
            }
            "textDocument/didOpen" | "textDocument/didChange" => {
                return self.on_document_changed(params).into_iter().collect();
            }
            "textDocument/didClose" => {
                if let Some(uri) = document_uri(params) {
                    self.documents.remove(&uri);
                }
                None
            }
            "textDocument/formatting" => Some(self.on_format(params)),
            "textDocument/hover" => Some(self.on_hover(params)),
            "textDocument/definition" => Some(self.on_definition(params)),
            "textDocument/completion" => Some(self.on_completion(params)),
            "textDocument/references" => Some(crate::refactor::references(self, params)),
            "textDocument/prepareRename" => Some(crate::refactor::prepare_rename(self, params)),
            "textDocument/rename" => {
                return match crate::refactor::rename(self, params) {
                    Ok(result) => id.map_or_else(Vec::new, |id| {
                        vec![Outgoing {
                            body: response(id, result),
                        }]
                    }),
                    Err((code, message)) => id.map_or_else(Vec::new, |id| {
                        vec![Outgoing {
                            body: obj([
                                ("jsonrpc", s("2.0")),
                                ("id", id),
                                ("error", error_value(code, &message)),
                            ]),
                        }]
                    }),
                };
            }
            "textDocument/signatureHelp" => Some(crate::signature::signature_help(self, params)),
            "textDocument/documentSymbol" => Some(crate::outline::document_symbols(self, params)),
            "textDocument/foldingRange" => Some(crate::outline::folding_ranges(self, params)),
            "textDocument/semanticTokens/full" => {
                Some(crate::semtok::semantic_tokens(self, params))
            }
            "textDocument/codeAction" => Some(crate::actions::code_actions(self, params)),
            _ if id.is_some() => Some(error_value(
                METHOD_NOT_FOUND,
                &format!("method not found: {method}"),
            )),
            _ => None, // an unknown notification is silently ignored, per the protocol
        };

        match (id, response_body) {
            (Some(id), Some(result)) => vec![Outgoing {
                body: response(id, result),
            }],
            _ => vec![],
        }
    }

    fn initialize() -> Value {
        // Real capabilities for what this build actually does. Sync is
        // incremental (kind 2): `crate::sync::apply_change` patches the
        // stored text at the edited range rather than re-parsing a whole
        // document resend on every keystroke. Everything else advertised
        // as absent rather than a capability that would then be called and
        // refused.
        obj([(
            "capabilities",
            obj([
                (
                    "textDocumentSync",
                    obj([("openClose", Value::Bool(true)), ("change", n(2))]),
                ),
                ("diagnosticProvider", Value::Bool(false)),
                ("documentFormattingProvider", Value::Bool(true)),
                ("hoverProvider", Value::Bool(true)),
                ("definitionProvider", Value::Bool(true)),
                ("referencesProvider", Value::Bool(true)),
                (
                    "renameProvider",
                    obj([("prepareProvider", Value::Bool(true))]),
                ),
                (
                    "signatureHelpProvider",
                    obj([("triggerCharacters", Value::Array(vec![s("(")]))]),
                ),
                ("documentSymbolProvider", Value::Bool(true)),
                ("foldingRangeProvider", Value::Bool(true)),
                (
                    "codeActionProvider",
                    obj([("codeActionKinds", Value::Array(vec![s("quickfix")]))]),
                ),
                (
                    "semanticTokensProvider",
                    obj([
                        (
                            "legend",
                            obj([
                                (
                                    "tokenTypes",
                                    Value::Array(
                                        crate::semtok::TOKEN_TYPES.iter().map(|t| s(*t)).collect(),
                                    ),
                                ),
                                ("tokenModifiers", Value::Array(Vec::new())),
                            ]),
                        ),
                        ("full", Value::Bool(true)),
                    ]),
                ),
                (
                    "completionProvider",
                    // No trigger characters: a client asks for completions
                    // when the user asks, and every name in scope is
                    // offered whatever was typed, which the client filters.
                    obj([("triggerCharacters", Value::Array(Vec::new()))]),
                ),
            ]),
        )])
    }

    fn on_document_changed(&mut self, params: Option<&Value>) -> Option<Outgoing> {
        let uri = document_uri(params)?;
        let text = crate::sync::apply_change(self.documents.get(&uri), params)?;
        self.documents.insert(uri.clone(), text.clone());
        let workspace = crate::workspace::build(&uri, &text, &self.documents);
        let diagnostics = workspace.map_or_else(Vec::new, Workspace::into_diagnostics);
        Some(Outgoing {
            body: publish_diagnostics(&uri, &diagnostics),
        })
    }

    /// `textDocument/formatting`: one `TextEdit` replacing the whole
    /// document with the canonical layout, or `null` when the document is
    /// not open (the client sent no `didOpen` first) or the formatter
    /// refused. The request's `options` (tab size and so on) are not read:
    /// the formatter has one canonical layout, not a configurable one.
    fn on_format(&self, params: Option<&Value>) -> Value {
        let Some(uri) = document_uri(params) else {
            return Value::Null;
        };
        let Some(text) = self.documents.get(&uri) else {
            return Value::Null;
        };
        let mut names = hollowscript_core::Interner::new();
        let Ok(src) =
            hollowscript_core::Source::new(hollowscript_core::SourceId(0), uri, text.as_str())
        else {
            return Value::Null;
        };
        let mut control = hollowscript_core::Control::none();
        let formatted = hollowscript_syntax::format(&src, &mut names, &mut control);
        let Some(new_text) = formatted.output else {
            return Value::Null;
        };
        Value::Array(vec![obj([
            ("range", full_document_range(text)),
            ("newText", s(new_text)),
        ])])
    }

    /// `textDocument/hover`: the type of the smallest checked expression
    /// the cursor is inside, which is what a reader wants to know and what
    /// the checker already worked out. `null` where the cursor is not in an
    /// expression, which is what the protocol expects rather than an empty
    /// hover.
    fn on_hover(&self, params: Option<&Value>) -> Value {
        let Some((workspace, offset)) = self.workspace_position(params) else {
            return Value::Null;
        };
        let Some(hover) = workspace.hover(offset) else {
            return Value::Null;
        };
        obj([(
            "contents",
            obj([
                ("kind", s("markdown")),
                ("value", s(format!("```hollowscript\n{}\n```", hover.ty))),
            ]),
        )])
    }

    /// `textDocument/definition`: where the name under the cursor was
    /// declared, in this document or, for a name brought in by `import`,
    /// in the module it was imported from (section 10.3: a request resolves
    /// names declared in imported files, loaded from open documents first
    /// and then disk, the same way a command-line run rooted at this
    /// document would).
    fn on_definition(&self, params: Option<&Value>) -> Value {
        let Some((workspace, offset)) = self.workspace_position(params) else {
            return Value::Null;
        };
        let Some(decl) = workspace.definition(offset) else {
            return Value::Null;
        };
        location_of(&workspace, &workspace.decl_id(&decl))
    }

    /// `textDocument/completion`: every name visible where the cursor is,
    /// then the prelude (section 13.6) and the keywords. The client filters
    /// by what has been typed, so the list is the same wherever in a word
    /// the cursor sits.
    fn on_completion(&self, params: Option<&Value>) -> Value {
        let Some((workspace, offset)) = self.workspace_position(params) else {
            return Value::Array(Vec::new());
        };
        Value::Array(
            workspace
                .completions(offset)
                .iter()
                .map(completion_item)
                .collect(),
        )
    }

    /// The document's text and the byte offset of the request's position.
    pub(crate) fn position(&self, params: Option<&Value>) -> Option<(String, u32)> {
        let uri = document_uri(params)?;
        let text = self.documents.get(&uri)?.clone();
        let position = params?.get("position")?;
        let line = position.get("line")?.as_i64()?;
        let character = position.get("character")?.as_i64()?;
        let offset = offset_of(&text, line, character)?;
        Some((text, offset))
    }

    /// The checked [`Workspace`] of the request's document and the byte
    /// offset of its position, together: every position-based request in
    /// this module needs both.
    pub(crate) fn workspace_position(&self, params: Option<&Value>) -> Option<(Workspace, u32)> {
        let uri = document_uri(params)?;
        let (text, offset) = self.position(params)?;
        let workspace = crate::workspace::build(&uri, &text, &self.documents)?;
        Some((workspace, offset))
    }

    /// The checked [`Workspace`] of the request's document alone, for a
    /// request with no position (an outline, folding ranges, semantic
    /// tokens).
    pub(crate) fn workspace_of(&self, params: Option<&Value>) -> Option<Workspace> {
        let uri = document_uri(params)?;
        let text = self.documents.get(&uri)?.clone();
        crate::workspace::build(&uri, &text, &self.documents)
    }

    /// The text of every open document, for a feature that has to look
    /// across more than the one this request names (find references,
    /// rename).
    pub(crate) fn documents(&self) -> &BTreeMap<String, String> {
        &self.documents
    }

    /// The checked [`Workspace`] of the open document at `uri`, for a
    /// feature that follows a declaration into another open document (find
    /// references, rename).
    pub(crate) fn workspace_for_uri(&self, uri: &str) -> Option<Workspace> {
        let text = self.documents.get(uri)?.clone();
        crate::workspace::build(uri, &text, &self.documents)
    }
}

/// The `Location` a declaration's [`crate::workspace::DeclId`] names: the
/// current document's own text if it is this document's, or, for a name
/// resolved through an `import`, the imported file's text kept alongside it.
pub(crate) fn location_of(workspace: &Workspace, decl: &crate::workspace::DeclId) -> Value {
    match workspace.text_at(&decl.uri) {
        Some(text) => obj([
            ("uri", s(decl.uri.clone())),
            ("range", span_range(text, decl.span)),
        ]),
        None => Value::Null,
    }
}

/// One `CompletionItem`: the protocol's `CompletionItemKind` for the
/// completion's kind (`Function` 3, `Variable` 6, `Module` 9, `Keyword` 14,
/// `EnumMember` 20, `Constant` 21, `Struct` 22, `Enum` 13) and its detail
/// when it has one.
fn completion_item(completion: &hollowscript_ide::complete::Completion) -> Value {
    use hollowscript_ide::complete::CompletionKind;
    let kind = match completion.kind {
        CompletionKind::Function => 3,
        CompletionKind::Variable | CompletionKind::Parameter => 6,
        CompletionKind::Capability | CompletionKind::Namespace => 9,
        CompletionKind::Keyword => 14,
        CompletionKind::EnumMember => 20,
        CompletionKind::Constant => 21,
        CompletionKind::Type => 22,
        CompletionKind::Enum => 13,
    };
    let mut fields = vec![("label", s(completion.label.as_str())), ("kind", n(kind))];
    if !completion.detail.is_empty() {
        fields.push(("detail", s(completion.detail.as_str())));
    }
    obj(fields)
}

/// The byte offset of a zero-based line and character. The character is
/// counted in code points, which is what this server does everywhere: the
/// protocol's default is UTF-16 code units, and the two agree for every
/// program, since section 1.2 makes identifiers ASCII and a position inside
/// a string literal is not one this server resolves anything at.
pub(crate) fn offset_of(text: &str, line: i64, character: i64) -> Option<u32> {
    let line = usize::try_from(line).ok()?;
    let character = usize::try_from(character).ok()?;
    let mut offset = 0usize;
    for (index, content) in text.split_inclusive('\n').enumerate() {
        if index == line {
            let within: usize = content
                .char_indices()
                .nth(character)
                .map_or(content.len(), |(i, _)| i);
            return u32::try_from(offset + within).ok();
        }
        offset += content.len();
    }
    // A position past the last line answers the end of the document, which
    // is what a client sends when the caret is on a final empty line.
    u32::try_from(text.len()).ok()
}

/// A span as an LSP range, both ends converted back to a line and a
/// character.
pub(crate) fn span_range(text: &str, span: hollowscript_core::Span) -> Value {
    let position = |offset: u32| {
        let mut line = 0i64;
        let mut start = 0usize;
        for content in text.split_inclusive('\n') {
            let end = start + content.len();
            if (offset as usize) < end {
                let character = text
                    .get(start..offset as usize)
                    .map_or(0, |s| i64::try_from(s.chars().count()).unwrap_or(0));
                return obj([("line", n(line)), ("character", n(character))]);
            }
            start = end;
            line += 1;
        }
        obj([("line", n(line)), ("character", n(0))])
    };
    obj([("start", position(span.start)), ("end", position(span.end))])
}

/// The range from the start of the document to just past its last
/// character, for a `TextEdit` that replaces the whole thing. The end line
/// is the real line count (so an edit never misses trailing lines); the end
/// character is a large, deliberately oversized column, the same
/// whole-line approximation [`lsp_diagnostic`] already uses, since neither
/// this server nor the checker tracks UTF-16 column positions yet.
fn full_document_range(text: &str) -> Value {
    let end_line = i64::try_from(text.matches('\n').count()).unwrap_or(i64::MAX);
    let position =
        |line: i64, character: i64| obj([("line", n(line)), ("character", n(character))]);
    obj([
        ("start", position(0, 0)),
        ("end", position(end_line, 1_000_000)),
    ])
}

pub(crate) fn document_uri(params: Option<&Value>) -> Option<String> {
    params?
        .get("textDocument")?
        .get("uri")?
        .as_str()
        .map(str::to_owned)
}

fn publish_diagnostics(uri: &str, diagnostics: &[Diagnostic]) -> Value {
    let items: Vec<Value> = diagnostics.iter().map(lsp_diagnostic).collect();
    obj([
        ("jsonrpc", s("2.0")),
        ("method", s("textDocument/publishDiagnostics")),
        (
            "params",
            obj([("uri", s(uri)), ("diagnostics", Value::Array(items))]),
        ),
    ])
}

/// One diagnostic in LSP's shape: a zero-based range, a severity number (1
/// error, 2 warning), a code and a message. This build has no end column
/// yet (the checker does not produce spans), so the range covers the whole
/// line the diagnostic is reported at.
fn lsp_diagnostic(d: &Diagnostic) -> Value {
    let line = d.location.as_ref().map_or(0, |l| l.line.saturating_sub(1));
    let position =
        |character: i64| obj([("line", n(i64::from(line))), ("character", n(character))]);
    obj([
        (
            "range",
            obj([("start", position(0)), ("end", position(1000))]),
        ),
        (
            "severity",
            n(if d.severity == Severity::Error { 1 } else { 2 }),
        ),
        ("code", s(d.code.to_text())),
        ("source", s("hollowscript")),
        ("message", s(d.message.clone())),
    ])
}

fn response(id: Value, result: Value) -> Value {
    obj([("jsonrpc", s("2.0")), ("id", id), ("result", result)])
}

fn error_value(code: i64, message: &str) -> Value {
    obj([("code", n(code)), ("message", s(message))])
}

/// JSON-RPC: the message was not valid JSON at all, so its `id`, if it has
/// one, could not even be read; the response therefore carries `id: null`,
/// as the protocol requires for this one error.
pub(crate) fn parse_error_response() -> Value {
    obj([
        ("jsonrpc", s("2.0")),
        ("id", Value::Null),
        ("error", error_value(PARSE_ERROR, "parse error")),
    ])
}

#[cfg(test)]
mod tests {
    use super::Server;
    use crate::json::parse;
    use hollowscript_core::Code;

    fn call(server: &mut Server, text: &str) -> Vec<crate::json::Value> {
        let message = parse(text).unwrap();
        server
            .handle(&message)
            .into_iter()
            .map(|o| o.body)
            .collect()
    }

    #[test]
    fn initialize_returns_real_capabilities() {
        let mut server = Server::new();
        let out = call(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        );
        assert_eq!(out.len(), 1);
        let json = out[0].to_json();
        assert!(json.contains(r#""id":1"#));
        assert!(json.contains("textDocumentSync"));
        assert!(json.contains(r#""documentFormattingProvider":true"#));
    }

    #[test]
    fn formatting_an_open_document_returns_one_text_edit_with_no_checker_needed() {
        let mut server = Server::new();
        let open = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///a.hws","text":"constant x=1\n"}}}"#;
        call(&mut server, open);
        let request = r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/formatting","params":{"textDocument":{"uri":"file:///a.hws"}}}"#;
        let out = call(&mut server, request);
        assert_eq!(out.len(), 1);
        let json = out[0].to_json();
        assert!(json.contains(r#""newText":"constant x = 1\n""#), "{json}");
        assert!(json.contains(r#""line":0"#));
    }

    #[test]
    fn formatting_a_document_with_a_syntax_error_returns_null() {
        let mut server = Server::new();
        let open = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///a.hws","text":"constant x ="}}}"#;
        call(&mut server, open);
        let request = r#"{"jsonrpc":"2.0","id":4,"method":"textDocument/formatting","params":{"textDocument":{"uri":"file:///a.hws"}}}"#;
        let out = call(&mut server, request);
        assert_eq!(out.len(), 1);
        assert!(out[0].to_json().contains(r#""result":null"#));
    }

    #[test]
    fn formatting_an_unopened_document_returns_null() {
        let mut server = Server::new();
        let request = r#"{"jsonrpc":"2.0","id":5,"method":"textDocument/formatting","params":{"textDocument":{"uri":"file:///never-opened.hws"}}}"#;
        let out = call(&mut server, request);
        assert_eq!(out.len(), 1);
        assert!(out[0].to_json().contains(r#""result":null"#));
    }

    #[test]
    fn an_unknown_method_gets_method_not_found_not_silence() {
        let mut server = Server::new();
        let out = call(
            &mut server,
            r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/typeDefinition","params":{}}"#,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].to_json().contains("-32601"));
    }

    #[test]
    fn an_unknown_notification_produces_no_reply() {
        let mut server = Server::new();
        let out = call(
            &mut server,
            r#"{"jsonrpc":"2.0","method":"$/setTrace","params":{}}"#,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn opening_a_clean_document_publishes_no_diagnostics() {
        let mut server = Server::new();
        let text = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///a.hws","text":"return 1"}}}"#;
        let out = call(&mut server, text);
        assert_eq!(out.len(), 1);
        let json = out[0].to_json();
        assert!(json.contains("publishDiagnostics"));
        assert!(json.contains(r#""diagnostics":[]"#), "{json}");
        assert!(server.documents.contains_key("file:///a.hws"));
    }

    #[test]
    fn opening_a_document_with_an_unresolved_name_publishes_a_real_diagnostic() {
        let mut server = Server::new();
        let text = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///a.hws","text":"return missing"}}}"#;
        let out = call(&mut server, text);
        assert_eq!(out.len(), 1);
        let json = out[0].to_json();
        assert!(json.contains("publishDiagnostics"));
        assert!(json.contains("HS0205"), "{json}");
    }

    #[test]
    fn closing_a_document_forgets_its_text() {
        let mut server = Server::new();
        let open = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///a.hws","text":"x"}}}"#;
        call(&mut server, open);
        assert!(server.documents.contains_key("file:///a.hws"));
        let close = r#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///a.hws"}}}"#;
        call(&mut server, close);
        assert!(!server.documents.contains_key("file:///a.hws"));
    }

    #[test]
    fn a_parse_error_response_carries_a_null_id() {
        let json = super::parse_error_response().to_json();
        assert!(json.contains(r#""id":null"#));
        assert!(json.contains("-32700"));
    }

    #[test]
    fn shutdown_then_exit_is_handled() {
        let mut server = Server::new();
        let out = call(
            &mut server,
            r#"{"jsonrpc":"2.0","id":9,"method":"shutdown"}"#,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].to_json().contains(r#""result":null"#));
        assert!(Server::should_exit("exit"));
        assert_eq!(Code::HS1099.number(), 1099);
    }

    /// Opens one document and returns the server, for the three
    /// position-based requests below.
    fn with_document(text: &str) -> Server {
        let mut server = Server::new();
        let escaped = text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///a.hws","text":"{escaped}"}}}}}}"#
        );
        let _ = call(&mut server, &open);
        server
    }

    fn at(server: &mut Server, method: &str, line: i64, character: i64) -> String {
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":9,"method":"{method}","params":{{"textDocument":{{"uri":"file:///a.hws"}},"position":{{"line":{line},"character":{character}}}}}}}"#
        );
        let out = call(server, &request);
        assert_eq!(out.len(), 1);
        out[0].to_json()
    }

    /// Hover answers the type the checker worked out for the expression the
    /// cursor is inside.
    #[test]
    fn hover_answers_the_type_under_the_cursor() {
        let mut server = with_document("constant total = 1 + 2\nreturn total\n");
        // Line 0, the `1` of the initialiser.
        let json = at(&mut server, "textDocument/hover", 0, 17);
        assert!(json.contains("Int"), "{json}");
        // A position with no expression under it answers null rather than
        // an empty hover.
        let json = at(&mut server, "textDocument/hover", 0, 0);
        assert!(json.contains(r#""result":null"#), "{json}");
    }

    /// Go to definition lands on the name in its declaration, and picks the
    /// innermost binder of that name.
    #[test]
    fn definition_finds_the_declaration_of_a_name() {
        let mut server = with_document(concat!(
            "constant n = 1\n",
            "fn f(n: Int): Int {\n",
            "    return n\n",
            "}\n",
            "return f(n)\n",
        ));
        // The `n` inside the function body resolves to the parameter on
        // line 1, not to the top-level constant on line 0.
        let json = at(&mut server, "textDocument/definition", 2, 11);
        assert!(json.contains(r#""line":1"#), "{json}");
        // The `n` in the call on the last line resolves to the constant.
        let json = at(&mut server, "textDocument/definition", 4, 9);
        assert!(json.contains(r#""line":0"#), "{json}");
    }

    /// Completion offers what is in scope where the cursor is, and not what
    /// is out of scope.
    #[test]
    fn completion_offers_the_names_in_scope() {
        let mut server = with_document(concat!(
            "constant outer = 1\n",
            "fn f(param: Int): Int {\n",
            "    constant inner = 2\n",
            "    return param\n",
            "}\n",
            "return f(outer)\n",
        ));
        let inside = at(&mut server, "textDocument/completion", 3, 10);
        for name in ["outer", "param", "inner", "f", "sqrt", "return"] {
            assert!(inside.contains(&format!(r#""label":"{name}""#)), "{name}");
        }
        // `inner` and `param` belong to the function's body.
        let outside = at(&mut server, "textDocument/completion", 5, 0);
        assert!(outside.contains(r#""label":"outer""#));
        assert!(!outside.contains(r#""label":"inner""#), "{outside}");
        assert!(!outside.contains(r#""label":"param""#), "{outside}");
    }

    /// Every keyword a program can write today is offered, taken from the
    /// lexer's own table rather than a list kept by hand; the two words
    /// reserved for a later version are an error to write and are not.
    #[test]
    fn completion_offers_every_usable_keyword() {
        let mut server = with_document("return 1\n");
        let json = at(&mut server, "textDocument/completion", 0, 0);
        for word in hollowscript_syntax::Keyword::ALL {
            let present = json.contains(&format!(r#""label":"{}""#, word.as_str()));
            assert_eq!(present, !word.is_future(), "{}", word.as_str());
        }
    }

    // ---- cross-file resolution, references, rename and the rest ----

    fn open(server: &mut Server, uri: &str, text: &str) {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        let message = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","text":"{escaped}"}}}}}}"#
        );
        let _ = call(server, &message);
    }

    fn request(server: &mut Server, method: &str, body: &str) -> String {
        let message = format!(r#"{{"jsonrpc":"2.0","id":9,"method":"{method}","params":{body}}}"#);
        let out = call(server, &message);
        assert_eq!(out.len(), 1);
        out[0].to_json()
    }

    /// A fresh pair of URIs in one real temporary directory, so import
    /// resolution has a root to work from (`crate::paths::root_and_entry`)
    /// even though neither file is ever written to disk: both documents are
    /// open, and an open document always overrides disk.
    fn two_uris(tag: &str) -> (String, String) {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-lsp-server-{tag}-{}",
            std::process::id()
        ));
        (
            crate::paths::from_path(&dir.join("main.hws")),
            crate::paths::from_path(&dir.join("util.hws")),
        )
    }

    #[test]
    fn definition_follows_an_import_into_the_other_open_document() {
        let (main_uri, util_uri) = two_uris("definition");
        let mut server = Server::new();
        open(&mut server, &util_uri, "export constant answer = 42\n");
        open(
            &mut server,
            &main_uri,
            "import answer from \"./util\"\nreturn answer\n",
        );
        let json = request(
            &mut server,
            "textDocument/definition",
            &format!(
                r#"{{"textDocument":{{"uri":"{main_uri}"}},"position":{{"line":1,"character":10}}}}"#
            ),
        );
        assert!(json.contains(&util_uri), "{json}");
        assert!(json.contains(r#""line":0"#), "{json}");
    }

    #[test]
    fn references_finds_uses_in_both_the_declaring_and_the_importing_file() {
        let (main_uri, util_uri) = two_uris("references");
        let mut server = Server::new();
        open(&mut server, &util_uri, "export constant answer = 42\n");
        open(
            &mut server,
            &main_uri,
            "import answer from \"./util\"\nreturn answer + answer\n",
        );
        // The cursor is on the declaration in `util.hws`.
        let json = request(
            &mut server,
            "textDocument/references",
            &format!(
                r#"{{"textDocument":{{"uri":"{util_uri}"}},"position":{{"line":0,"character":18}},"context":{{"includeDeclaration":true}}}}"#
            ),
        );
        assert!(json.contains(&util_uri), "{json}");
        assert!(json.contains(&main_uri), "{json}");
        // Two uses in `main.hws`, plus the import line and the declaration.
        assert_eq!(json.matches(r#""range""#).count(), 4, "{json}");
    }

    #[test]
    fn rename_produces_a_workspace_edit_across_both_files() {
        let (main_uri, util_uri) = two_uris("rename");
        let mut server = Server::new();
        open(&mut server, &util_uri, "export constant answer = 42\n");
        open(
            &mut server,
            &main_uri,
            "import answer from \"./util\"\nreturn answer\n",
        );
        let json = request(
            &mut server,
            "textDocument/rename",
            &format!(
                r#"{{"textDocument":{{"uri":"{main_uri}"}},"position":{{"line":1,"character":10}},"newName":"solved"}}"#
            ),
        );
        assert!(json.contains(&util_uri), "{json}");
        assert!(json.contains(&main_uri), "{json}");
        assert!(json.contains(r#""newText":"solved""#), "{json}");
    }

    #[test]
    fn rename_refuses_a_prelude_name_and_a_keyword() {
        let mut server = with_document("return sqrt(4)\n");
        let json = request(
            &mut server,
            "textDocument/rename",
            r#"{"textDocument":{"uri":"file:///a.hws"},"position":{"line":0,"character":8},"newName":"root"}"#,
        );
        assert!(json.contains("-32602"), "{json}");

        let json = request(
            &mut server,
            "textDocument/prepareRename",
            r#"{"textDocument":{"uri":"file:///a.hws"},"position":{"line":0,"character":8}}"#,
        );
        assert!(json.contains(r#""result":null"#), "{json}");
    }

    #[test]
    fn prepare_rename_offers_a_declared_name() {
        let mut server = with_document("constant total = 1\nreturn total\n");
        let json = request(
            &mut server,
            "textDocument/prepareRename",
            r#"{"textDocument":{"uri":"file:///a.hws"},"position":{"line":0,"character":10}}"#,
        );
        assert!(json.contains(r#""placeholder":"total""#), "{json}");
    }

    #[test]
    fn signature_help_reports_the_callees_parameters() {
        let mut server = with_document(concat!(
            "fn add(a: Int, b: Int): Int {\n",
            "    return a + b\n",
            "}\n",
            "return add(1, 2)\n",
        ));
        // Cursor on the `2`, the second argument.
        let json = at(&mut server, "textDocument/signatureHelp", 3, 14);
        assert!(json.contains(r#""label":"a: Int""#), "{json}");
        assert!(json.contains(r#""label":"b: Int""#), "{json}");
        assert!(json.contains(r#""activeParameter":1"#), "{json}");
    }

    #[test]
    fn document_symbol_lists_top_level_declarations() {
        let mut server = with_document(concat!(
            "fn greet(name: String): String {\n",
            "    return name\n",
            "}\n",
            "constant total = 1\n",
        ));
        let json = request(
            &mut server,
            "textDocument/documentSymbol",
            r#"{"textDocument":{"uri":"file:///a.hws"}}"#,
        );
        assert!(json.contains(r#""name":"greet""#), "{json}");
        assert!(json.contains(r#""name":"total""#), "{json}");
    }

    #[test]
    fn document_symbol_includes_uses_and_import_lines_and_shows_details() {
        let mut server = with_document(concat!(
            "uses print\n",
            "import twice from \"./nope\"\n",
            "export fn greet(name: String): String {\n",
            "    return name\n",
            "}\n",
        ));
        let json = request(
            &mut server,
            "textDocument/documentSymbol",
            r#"{"textDocument":{"uri":"file:///a.hws"}}"#,
        );
        assert!(json.contains(r#""name":"print""#), "{json}");
        assert!(json.contains(r#""name":"./nope""#), "{json}");
        assert!(json.contains(r#""detail":"twice""#), "{json}");
        assert!(
            json.contains(r#""detail":"fn(name: String): String""#),
            "{json}"
        );
    }

    #[test]
    fn completion_items_carry_a_kind_and_a_detail() {
        let mut server = with_document(concat!(
            "fn add(a: Int): Int {\n",
            "    return a\n",
            "}\n",
            "constant limit: Float = 1\n",
            "return add(1)\n",
        ));
        let json = at(&mut server, "textDocument/completion", 4, 0);
        assert!(
            json.contains(r#"{"detail":"fn(a: Int): Int","kind":3,"label":"add"}"#),
            "{json}"
        );
        assert!(
            json.contains(r#"{"detail":"Float","kind":21,"label":"limit"}"#),
            "{json}"
        );
        // Names in a comment are not offered.
        let mut server = with_document("constant limit = 1\n// li\n");
        let json = at(&mut server, "textDocument/completion", 1, 5);
        assert!(!json.contains(r#""label":"limit""#), "{json}");
    }

    #[test]
    fn signature_help_reaches_a_function_declared_in_an_import() {
        let (main_uri, util_uri) = two_uris("signature");
        let mut server = Server::new();
        open(
            &mut server,
            &util_uri,
            "export fn twice(n: Int): Int {\n    return n * 2\n}\n",
        );
        open(
            &mut server,
            &main_uri,
            "import twice from \"./util\"\nreturn twice(3)\n",
        );
        let json = request(
            &mut server,
            "textDocument/signatureHelp",
            &format!(
                r#"{{"textDocument":{{"uri":"{main_uri}"}},"position":{{"line":1,"character":13}}}}"#
            ),
        );
        assert!(json.contains(r#""label":"n: Int""#), "{json}");
    }

    #[test]
    fn definition_inside_a_string_or_a_comment_is_null() {
        let mut server = with_document(concat!(
            "constant total = 1\n",
            "constant s = \"total\"\n",
            "// total\n",
        ));
        let json = at(&mut server, "textDocument/definition", 1, 16);
        assert!(json.contains(r#""result":null"#), "{json}");
        let json = at(&mut server, "textDocument/definition", 2, 5);
        assert!(json.contains(r#""result":null"#), "{json}");
    }

    #[test]
    fn folding_range_covers_a_function_body() {
        let mut server = with_document(concat!(
            "fn greet(name: String): String {\n",
            "    return name\n",
            "}\n",
        ));
        let json = request(
            &mut server,
            "textDocument/foldingRange",
            r#"{"textDocument":{"uri":"file:///a.hws"}}"#,
        );
        assert!(json.contains(r#""startLine":0"#), "{json}");
        assert!(json.contains(r#""endLine":2"#), "{json}");
    }

    #[test]
    fn semantic_tokens_covers_a_declaration_and_its_use() {
        let mut server = with_document("constant total = 1\nreturn total\n");
        let json = request(
            &mut server,
            "textDocument/semanticTokens/full",
            r#"{"textDocument":{"uri":"file:///a.hws"}}"#,
        );
        // Two five-integer quintuples: the declaration and the `return` use.
        let Some(data_start) = json.find('[') else {
            unreachable!("semantic tokens always answer a data array")
        };
        let count = json[data_start..].matches(',').count() + 1;
        assert_eq!(count, 10, "{json}");
    }

    #[test]
    fn code_action_adds_effect_for_an_unmarked_effectful_call() {
        let mut server = with_document("uses print\nprint(\"hi\")\n");
        let json = request(
            &mut server,
            "textDocument/codeAction",
            concat!(
                r#"{"textDocument":{"uri":"file:///a.hws"},"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":0}},"#,
                r#""context":{"diagnostics":[{"code":"HS0401","range":{"start":{"line":1,"character":0},"end":{"line":1,"character":12}}}]}}"#,
            ),
        );
        assert!(json.contains("Add 'effect'"), "{json}");
        assert!(json.contains(r#""newText":"effect ""#), "{json}");
    }

    #[test]
    fn code_action_adds_a_uses_line_for_an_undeclared_standard_capability() {
        let mut server = with_document("effect print(\"hi\")\n");
        let json = request(
            &mut server,
            "textDocument/codeAction",
            concat!(
                r#"{"textDocument":{"uri":"file:///a.hws"},"range":{"start":{"line":0,"character":7},"end":{"line":0,"character":12}},"#,
                r#""context":{"diagnostics":[{"code":"HS0205","range":{"start":{"line":0,"character":7},"end":{"line":0,"character":12}}}]}}"#,
            ),
        );
        assert!(json.contains("Add 'uses print'"), "{json}");
        assert!(json.contains(r#""newText":"uses print\n""#), "{json}");
    }

    #[test]
    fn code_action_removes_a_single_unused_import() {
        let mut server = with_document("import unused from \"./util\"\nreturn 1\n");
        let json = request(
            &mut server,
            "textDocument/codeAction",
            concat!(
                r#"{"textDocument":{"uri":"file:///a.hws"},"range":{"start":{"line":0,"character":7},"end":{"line":0,"character":13}},"#,
                r#""context":{"diagnostics":[{"code":"HS0802","range":{"start":{"line":0,"character":7},"end":{"line":0,"character":13}}}]}}"#,
            ),
        );
        assert!(json.contains("Remove unused 'unused'"), "{json}");
        assert!(json.contains(r#""newText":"""#), "{json}");
    }

    #[test]
    fn an_incremental_change_is_reflected_in_the_next_diagnostics() {
        let mut server = Server::new();
        open(&mut server, "file:///a.hws", "return missing\n");
        let change = r#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///a.hws"},"contentChanges":[{"range":{"start":{"line":0,"character":7},"end":{"line":0,"character":14}},"text":"1"}]}}"#;
        let out = call(&mut server, change);
        assert_eq!(out.len(), 1);
        let json = out[0].to_json();
        assert!(json.contains(r#""diagnostics":[]"#), "{json}");
    }
}
