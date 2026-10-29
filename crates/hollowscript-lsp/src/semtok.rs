//! `textDocument/semanticTokens/full`: the protocol face of
//! [`hollowscript_ide`]'s semantic tokens, every identifier it resolves to
//! a declaration, delta-encoded against the legend `initialize` advertises.
//! A keyword, a literal and a comment are left to the `TextMate` grammar in
//! `editors/vscode/`, which tokenises them from the lexical grammar alone.

use crate::json::{Value, n, obj};
use crate::server::Server;
use hollowscript_core::Span;
use hollowscript_ide::semantic::SemanticKind;

/// The legend `initialize` advertises, indexed by [`token_type`].
pub(crate) const TOKEN_TYPES: &[&str] = &["function", "variable", "parameter", "type", "namespace"];

const fn token_type(kind: SemanticKind) -> i64 {
    match kind {
        SemanticKind::Function => 0,
        SemanticKind::Variable => 1,
        SemanticKind::Parameter => 2,
        SemanticKind::Type => 3,
        SemanticKind::Namespace => 4,
    }
}

pub(crate) fn semantic_tokens(server: &Server, params: Option<&Value>) -> Value {
    let Some(workspace) = server.workspace_of(params) else {
        return Value::Null;
    };
    let tokens: Vec<(Span, i64)> = workspace
        .semantic_tokens()
        .into_iter()
        .map(|token| (token.span, token_type(token.kind)))
        .collect();
    let data = encode(&workspace.text, &tokens);
    obj([("data", Value::Array(data.into_iter().map(n).collect()))])
}

/// Delta-encodes `tokens` (already sorted by start offset) into the
/// protocol's flat `[deltaLine, deltaStartChar, length, tokenType,
/// tokenModifiers]` quintuples. Every token here is on one line: an
/// identifier never spans a line break (section 1.2).
fn encode(text: &str, tokens: &[(Span, i64)]) -> Vec<i64> {
    let mut out = Vec::with_capacity(tokens.len() * 5);
    let mut previous_line = 0i64;
    let mut previous_char = 0i64;
    for (span, ty) in tokens {
        let start = position(text, span.start);
        let length = i64::from(span.end.saturating_sub(span.start));
        let delta_line = start.0 - previous_line;
        let delta_char = if delta_line == 0 {
            start.1 - previous_char
        } else {
            start.1
        };
        out.extend([delta_line, delta_char, length, *ty, 0]);
        previous_line = start.0;
        previous_char = start.1;
    }
    out
}

/// The zero-based line and UTF-16-ish character (code points, see
/// `crate::server::offset_of`'s own note) of a byte offset.
fn position(text: &str, offset: u32) -> (i64, i64) {
    let mut line = 0i64;
    let mut start = 0usize;
    for content in text.split_inclusive('\n') {
        let end = start + content.len();
        if (offset as usize) < end || end == text.len() {
            let character = text
                .get(start..(offset as usize).min(text.len()))
                .map_or(0, |s| i64::try_from(s.chars().count()).unwrap_or(0));
            return (line, character);
        }
        start = end;
        line += 1;
    }
    (line, 0)
}

#[cfg(test)]
mod tests {
    use super::encode;
    use hollowscript_core::Span;

    #[test]
    fn two_tokens_on_one_line_delta_encode_from_each_other() {
        let text = "constant x = 1\n";
        let tokens = vec![(Span::new(9, 10), 1i64)];
        assert_eq!(encode(text, &tokens), vec![0, 9, 1, 1, 0]);
    }

    #[test]
    fn a_token_on_a_later_line_resets_the_character_delta() {
        let text = "constant x = 1\nreturn x\n";
        let tokens = vec![(Span::new(9, 10), 1i64), (Span::new(22, 23), 1i64)];
        let data = encode(text, &tokens);
        // second token: line 1, character 7 (the `x` of "return x"), length 1, type 1
        assert_eq!(data, vec![0, 9, 1, 1, 0, 1, 7, 1, 1, 0]);
    }
}
