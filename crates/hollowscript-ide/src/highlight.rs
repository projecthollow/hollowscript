//! Lexical highlighting: every token and comment of a text, classified, from
//! the lexer alone.
//!
//! This is stateless and needs neither a parse nor a check, so it is as
//! cheap as lexing and works on any text, including one that does not parse
//! (a browser editor highlights on every keystroke, long before the program
//! is valid). It is the lexical counterpart of [`crate::semantic`], which
//! classifies only the identifiers that name something declared.
//!
//! Whitespace and the virtual statement terminator of section 1.8 are not
//! tokens a reader sees and are left out. A string with interpolation is cut
//! by the lexer into a head, any number of middles and a tail, each of which
//! is a [`TokenClass::String`] covering its own text including the `${` and
//! `}` that bound it; the tokens of the interpolated expressions between
//! them are ordinary tokens. A character the language rejects (HS0005) is a
//! [`TokenClass::Invalid`] range, matching what `check` reports (up to the
//! diagnostic cap of section 17.1).

use hollowscript_core::{Control, Source, SourceId, Span};
use hollowscript_syntax::{Comment, TokenKind, lex};

/// What a highlighted range is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TokenClass {
    /// A reserved word, including `true`, `false` and `none`, the word
    /// operators `and`, `or`, `not` and `in`, and the words reserved for a
    /// later version.
    Keyword,
    /// An identifier.
    Identifier,
    /// An `Int` or a `Float` literal.
    Number,
    /// A string literal, or a piece of one that has interpolation.
    String,
    /// A filter literal, backticks included.
    Filter,
    /// An operator: `+ - * / = == != < > <= >= .. ?`.
    Operator,
    /// Punctuation: brackets, `,`, `:` and `.`.
    Punctuation,
    /// A `//` comment, up to but not including the line terminator.
    Comment,
    /// A character the language rejects (HS0005) outside a string, a
    /// comment and a filter: any character with no token, for example a
    /// non-ASCII letter or digit, a stray `#`, a control character, an emoji
    /// or a lone `!`. In an identifier the ASCII part stays an
    /// [`TokenClass::Identifier`] and each rejected code point is its own
    /// range.
    Invalid,
}

impl TokenClass {
    /// The class as a lower case word, the spelling a host sees.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TokenClass::Keyword => "keyword",
            TokenClass::Identifier => "identifier",
            TokenClass::Number => "number",
            TokenClass::String => "string",
            TokenClass::Filter => "filter",
            TokenClass::Operator => "operator",
            TokenClass::Punctuation => "punctuation",
            TokenClass::Comment => "comment",
            TokenClass::Invalid => "invalid",
        }
    }
}

/// One classified range of a text.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    /// The bytes it covers.
    pub span: Span,
    /// What it is.
    pub class: TokenClass,
}

/// The class of a token kind, or `None` for one a reader does not see: the
/// virtual statement terminator and the end of the file.
const fn class_of(kind: TokenKind) -> Option<TokenClass> {
    match kind {
        TokenKind::Ident => Some(TokenClass::Identifier),
        TokenKind::Int | TokenKind::Float => Some(TokenClass::Number),
        TokenKind::String
        | TokenKind::StringHead
        | TokenKind::StringMiddle
        | TokenKind::StringTail => Some(TokenClass::String),
        TokenKind::Filter => Some(TokenClass::Filter),
        TokenKind::Keyword(_) => Some(TokenClass::Keyword),
        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Assign
        | TokenKind::EqEq
        | TokenKind::NotEq
        | TokenKind::Lt
        | TokenKind::Gt
        | TokenKind::LtEq
        | TokenKind::GtEq
        | TokenKind::DotDot
        | TokenKind::Question => Some(TokenClass::Operator),
        TokenKind::LParen
        | TokenKind::RParen
        | TokenKind::LBracket
        | TokenKind::RBracket
        | TokenKind::LBrace
        | TokenKind::RBrace
        | TokenKind::Comma
        | TokenKind::Colon
        | TokenKind::Dot => Some(TokenClass::Punctuation),
        TokenKind::Term | TokenKind::Eof => None,
    }
}

/// Every token and comment of `text`, classified, in order of where they
/// start, with none overlapping.
///
/// A text longer than the source size limit of section 1.1 is not lexed and
/// answers nothing. A character the lexer rejects (HS0005) outside a string,
/// a comment and a filter is an [`TokenClass::Invalid`] range of one code
/// point, so the ranges returned cover all text except whitespace, line
/// terminators and a leading byte order mark.
#[must_use]
pub fn highlight(text: &str) -> Vec<Highlight> {
    let Ok(source) = Source::new(SourceId(0), "highlight", text) else {
        return Vec::new();
    };
    let lexed = lex(&source, &mut Control::none());
    let mut out: Vec<Highlight> = Vec::with_capacity(lexed.tokens.len() + lexed.comments.len());
    // Every byte range the lexer took, whatever it was, so that what is left
    // over is exactly what it skipped or rejected.
    let mut taken: Vec<(u32, u32)> = Vec::with_capacity(lexed.tokens.len() + lexed.comments.len());
    for token in &lexed.tokens {
        if token.span.start == token.span.end {
            continue;
        }
        let Some(class) = class_of(token.kind) else {
            taken.push((token.span.start, token.span.end));
            continue;
        };
        let (start, end) = (token.span.start, token.span.end);
        match token.kind {
            TokenKind::Ident if token.malformed => {
                taken.push((start, end));
                split_identifier(text, start, end, &mut out);
            }
            TokenKind::StringMiddle | TokenKind::StringTail => {
                // The piece begins where the previous token ended, so it can
                // hold spaces, comments and rejected characters before the
                // `}` that closed the interpolation. That text is a gap,
                // handled below, and the piece starts at the brace. A piece
                // with no brace is an interpolation abandoned at a line
                // terminator or the end of the file (HS0007): all of it is
                // gap.
                let Some(brace) = closing_brace(text, start, end, &lexed.comments) else {
                    continue;
                };
                taken.push((brace, end));
                out.push(Highlight {
                    span: Span::new(brace, end),
                    class,
                });
            }
            _ => {
                taken.push((start, end));
                out.push(Highlight {
                    span: token.span,
                    class,
                });
            }
        }
    }
    for comment in &lexed.comments {
        if comment.span.start != comment.span.end {
            taken.push((comment.span.start, comment.span.end));
            out.push(Highlight {
                span: comment.span,
                class: TokenClass::Comment,
            });
        }
    }
    taken.sort_unstable();
    mark_rejected(text, &taken, &mut out);
    // Tokens and comments are each in order already; one stable sort
    // interleaves them.
    out.sort_by_key(|h| h.span.start);
    out
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The `}` that closed the interpolation before the string piece
/// `start..end`: the first one outside a comment (the tail of an abandoned
/// interpolation can hold a `}` only inside a comment), or `None` for a piece
/// that never reached its brace. `comments` are in source order.
fn closing_brace(text: &str, start: u32, end: u32, comments: &[Comment]) -> Option<u32> {
    let piece = &text[start as usize..end as usize];
    piece
        .match_indices('}')
        .map(|(i, _)| start + to_u32(i))
        .find(|&at| {
            let started = comments.partition_point(|c| c.span.start <= at);
            started
                .checked_sub(1)
                .and_then(|last| comments.get(last))
                .is_none_or(|c| c.span.end <= at)
        })
}

/// A malformed identifier is a run of ASCII name characters with non-ASCII
/// letters or digits glued in (section 1.2). Each ASCII run stays an
/// identifier; each other code point is its own invalid range.
fn split_identifier(text: &str, start: u32, end: u32, out: &mut Vec<Highlight>) {
    let word = &text[start as usize..end as usize];
    let mut run_start: Option<u32> = None;
    for (i, c) in word.char_indices() {
        let at = start + to_u32(i);
        if c.is_ascii() {
            run_start.get_or_insert(at);
            continue;
        }
        if let Some(from) = run_start.take() {
            out.push(Highlight {
                span: Span::new(from, at),
                class: TokenClass::Identifier,
            });
        }
        out.push(Highlight {
            span: Span::new(at, at + to_u32(c.len_utf8())),
            class: TokenClass::Invalid,
        });
    }
    if let Some(from) = run_start {
        out.push(Highlight {
            span: Span::new(from, end),
            class: TokenClass::Identifier,
        });
    }
}

/// Marks every character outside `taken` (sorted) that the lexer did not
/// simply skip as invalid. The lexer skips SPACE, TAB, a line terminator
/// (LF, or CR immediately before LF) and a leading byte order mark; it
/// rejects (HS0005) everything else it has no token for.
fn mark_rejected(text: &str, taken: &[(u32, u32)], out: &mut Vec<Highlight>) {
    let bytes = text.as_bytes();
    let mut cursor = 0u32;
    let gap = |from: u32, to: u32, out: &mut Vec<Highlight>| {
        let mut at = from;
        if at == 0 && text.starts_with('\u{feff}') {
            at = 3.min(to);
        }
        while at < to {
            let Some(c) = text[at as usize..].chars().next() else {
                break;
            };
            let width = to_u32(c.len_utf8());
            let skipped = matches!(c, ' ' | '\t' | '\n')
                || (c == '\r' && bytes.get(at as usize + 1) == Some(&b'\n'));
            if !skipped {
                out.push(Highlight {
                    span: Span::new(at, at + width),
                    class: TokenClass::Invalid,
                });
            }
            at += width;
        }
    };
    for &(start, end) in taken {
        if start > cursor {
            gap(cursor, start, out);
        }
        cursor = cursor.max(end);
    }
    gap(cursor, to_u32(text.len()), out);
}

#[cfg(test)]
mod tests {
    use super::{TokenClass, highlight};

    fn classes(text: &str) -> Vec<(&str, TokenClass)> {
        highlight(text)
            .into_iter()
            .map(|h| (&text[h.span.start as usize..h.span.end as usize], h.class))
            .collect()
    }

    #[test]
    fn a_declaration_is_classified_token_by_token() {
        use TokenClass::{Identifier, Keyword, Number, Operator, Punctuation};
        assert_eq!(
            classes("constant total: Int = 1 + 2.5\n"),
            vec![
                ("constant", Keyword),
                ("total", Identifier),
                (":", Punctuation),
                ("Int", Identifier),
                ("=", Operator),
                ("1", Number),
                ("+", Operator),
                ("2.5", Number),
            ]
        );
    }

    #[test]
    fn every_operator_and_punctuation_mark_has_a_class() {
        use TokenClass::{Operator, Punctuation};
        let operators = [
            "+", "-", "*", "/", "=", "==", "!=", "<", ">", "<=", ">=", "..", "?",
        ];
        for op in operators {
            let text = format!("a {op} b");
            let found = classes(&text);
            assert_eq!(
                found.get(1).map(|(t, c)| (*t, *c)),
                Some((op, Operator)),
                "{op}"
            );
        }
        let marks = ["(", ")", "[", "]", "{", "}", ",", ":", "."];
        for mark in marks {
            let found = classes(mark);
            assert_eq!(found, vec![(mark, Punctuation)], "{mark}");
        }
    }

    #[test]
    fn every_keyword_is_a_keyword_including_the_reserved_ones() {
        for keyword in hollowscript_syntax::Keyword::ALL {
            let word = keyword.as_str();
            assert_eq!(
                classes(word).first(),
                Some(&(word, TokenClass::Keyword)),
                "{word}"
            );
        }
    }

    #[test]
    fn a_string_with_interpolation_is_cut_around_its_expression() {
        use TokenClass::{Identifier, String};
        assert_eq!(
            classes("\"hi ${name}!\""),
            vec![("\"hi ${", String), ("name", Identifier), ("}!\"", String)]
        );
        assert_eq!(classes("\"plain\""), vec![("\"plain\"", String)]);
    }

    #[test]
    fn a_filter_literal_is_one_token() {
        assert_eq!(
            classes("tasks.where(`status:open`)\n").get(4),
            Some(&("`status:open`", TokenClass::Filter))
        );
    }

    #[test]
    fn a_comment_is_a_range_that_stops_before_the_line_end() {
        use TokenClass::{Comment, Identifier, Number, Operator};
        assert_eq!(
            classes("x = 1 // note: one\ny\n"),
            vec![
                ("x", Identifier),
                ("=", Operator),
                ("1", Number),
                ("// note: one", Comment),
                ("y", Identifier),
            ]
        );
    }

    #[test]
    fn the_ranges_are_ordered_and_do_not_overlap() {
        let text = concat!(
            "uses print // a comment\n",
            "fn f(n: Int): String {\n",
            "    return \"n is ${n}\" // tail\n",
            "}\n",
        );
        let found = highlight(text);
        assert!(!found.is_empty());
        for pair in found.windows(2) {
            assert!(pair[0].span.end <= pair[1].span.start, "{pair:?}");
        }
    }

    #[test]
    fn text_the_lexer_rejects_is_invalid_and_nothing_panics() {
        use TokenClass::{Identifier, Invalid};
        // A stray character the language has no token for.
        assert_eq!(
            classes("a # b"),
            vec![("a", Identifier), ("#", Invalid), ("b", Identifier)]
        );
        // Unterminated string and filter literals still lex.
        assert!(!highlight("\"never closed").is_empty());
        assert!(!highlight("`never closed").is_empty());
        assert!(highlight("").is_empty());
    }

    #[test]
    fn a_non_ascii_letter_in_an_identifier_is_invalid_and_the_rest_is_not() {
        use TokenClass::{Identifier, Invalid, Keyword, Number, Operator};
        assert_eq!(
            classes("constant caf\u{e9} = 1"),
            vec![
                ("constant", Keyword),
                ("caf", Identifier),
                ("\u{e9}", Invalid),
                ("=", Operator),
                ("1", Number),
            ]
        );
        assert_eq!(
            classes("na\u{ef}ve_name"),
            vec![
                ("na", Identifier),
                ("\u{ef}", Invalid),
                ("ve_name", Identifier)
            ]
        );
        assert_eq!(
            classes("a\u{e9}"),
            vec![("a", Identifier), ("\u{e9}", Invalid)]
        );
        // One range per code point.
        assert_eq!(
            classes("a\u{e9}\u{e8}"),
            vec![("a", Identifier), ("\u{e9}", Invalid), ("\u{e8}", Invalid)]
        );
    }

    #[test]
    fn a_word_that_starts_with_a_non_ascii_letter_or_follows_a_number_is_invalid() {
        use TokenClass::{Invalid, Number};
        assert_eq!(
            classes("\u{540d}\u{524d}"),
            vec![("\u{540d}", Invalid), ("\u{524d}", Invalid)]
        );
        assert_eq!(classes("1\u{e9}"), vec![("1", Number), ("\u{e9}", Invalid)]);
        // An astral character is one range.
        assert_eq!(
            classes("a \u{1f600} b").get(1),
            Some(&("\u{1f600}", Invalid))
        );
    }

    #[test]
    fn rejected_control_characters_and_a_lone_carriage_return_are_invalid() {
        use TokenClass::{Identifier, Invalid};
        assert_eq!(
            classes("a\u{1}b"),
            vec![("a", Identifier), ("\u{1}", Invalid), ("b", Identifier)]
        );
        assert_eq!(
            classes("a\rb"),
            vec![("a", Identifier), ("\r", Invalid), ("b", Identifier)]
        );
        // A CRLF line ending and a leading byte order mark are fine.
        assert_eq!(
            classes("\u{feff}a\r\nb"),
            vec![("a", Identifier), ("b", Identifier)]
        );
        // A byte order mark anywhere else is rejected.
        assert_eq!(classes("a\u{feff}").get(1), Some(&("\u{feff}", Invalid)));
    }

    #[test]
    fn non_ascii_text_in_a_string_a_comment_or_a_filter_keeps_its_class() {
        use TokenClass::{Comment, Filter, String};
        assert_eq!(
            classes("\"\u{e9}\" // \u{e9}\n`\u{e9}`"),
            vec![
                ("\"\u{e9}\"", String),
                ("// \u{e9}", Comment),
                ("`\u{e9}`", Filter)
            ]
        );
    }

    #[test]
    fn a_rejected_character_inside_an_interpolation_does_not_leak_into_the_string_pieces() {
        use TokenClass::{Invalid, String};
        assert_eq!(
            classes("\"a ${ \u{e9} } b\""),
            vec![("\"a ${", String), ("\u{e9}", Invalid), ("} b\"", String)]
        );
    }

    #[test]
    fn the_string_piece_after_an_interpolation_starts_at_the_closing_brace() {
        use TokenClass::{Identifier, String};
        assert_eq!(
            classes("\"${ x }\""),
            vec![("\"${", String), ("x", Identifier), ("}\"", String)]
        );
        assert_eq!(
            classes("\"${x} ${  y  }!\""),
            vec![
                ("\"${", String),
                ("x", Identifier),
                ("} ${", String),
                ("y", Identifier),
                ("}!\"", String)
            ]
        );
        // A multi-line interpolation inside open brackets.
        let found = classes("\"${f(\n  1\n)  }\"");
        assert_eq!(found.last(), Some(&("}\"", String)));
    }

    #[test]
    fn a_rejected_control_character_in_a_string_or_comment_is_not_invalid() {
        let found = highlight("\"\u{1}\" // \u{1}");
        assert!(found.iter().all(|h| h.class != TokenClass::Invalid));
    }

    #[test]
    fn malformed_string_pieces_start_at_the_brace_and_leave_the_code_before_it_alone() {
        use TokenClass::{Comment, Identifier, Invalid, String};
        // HS0006 after a closed interpolation.
        assert_eq!(
            classes("\"${ \u{e9} } abc"),
            vec![("\"${", String), ("\u{e9}", Invalid), ("} abc", String)]
        );
        // HS0007: the tail has no closing brace, so it is not a string range.
        assert_eq!(
            classes("\"${ x \u{e9}"),
            vec![("\"${", String), ("x", Identifier), ("\u{e9}", Invalid)]
        );
        assert_eq!(
            classes("\"${ x // note\ny"),
            vec![
                ("\"${", String),
                ("x", Identifier),
                ("// note", Comment),
                ("y", Identifier)
            ]
        );
        // A `}` inside a comment is not the closer: nothing is left of the
        // abandoned tail to colour as a string.
        assert_eq!(
            classes("\"${ x // }\ny"),
            vec![
                ("\"${", String),
                ("x", Identifier),
                ("// }", Comment),
                ("y", Identifier)
            ]
        );
        // A lone CR does not end a comment: it and the `}` after it are comment.
        assert_eq!(
            classes("\"${ x // \r}\ny"),
            vec![
                ("\"${", String),
                ("x", Identifier),
                ("// \r}", Comment),
                ("y", Identifier)
            ]
        );
    }

    #[test]
    fn every_invalid_range_is_a_check_error_and_every_check_error_outside_text_is_invalid() {
        use hollowscript_core::{Code, Control, Source, SourceId};
        use hollowscript_syntax::{TokenKind, lex};
        let samples = [
            "constant caf\u{e9} = 1",
            "constant a\u{e9}\u{e8} = 1",
            "constant \u{540d}\u{524d} = 1",
            "return 1\u{e9}",
            "x = a # b",
            "\"a ${ \u{e9} } b\"",
            "\"${ \u{e9} } abc",
            "\"${ x \u{e9}",
            "\"${ x // note\nreturn 1\u{e9}",
            "\"${ x // }\ny",
            "\"${ x // \r}\ny",
            "x // a\rb\ny",
            "a\rb",
            "a\u{1}b",
            "a \u{1f600} b",
        ];
        for text in samples {
            let source = Source::new(SourceId(0), "t", text).unwrap();
            let lexed = lex(&source, &mut Control::none());
            let found = highlight(text);
            for pair in found.windows(2) {
                assert!(pair[0].span.end <= pair[1].span.start, "{text:?}: {pair:?}");
            }
            let invalid: Vec<_> = found
                .iter()
                .filter(|h| h.class == TokenClass::Invalid)
                .collect();
            // The HS0005 sites outside a string, a comment and a filter,
            // found from the lexer's own tokens and comments.
            let mut text_ranges: Vec<(u32, u32)> = lexed
                .comments
                .iter()
                .map(|c| (c.span.start, c.span.end))
                .collect();
            for token in &lexed.tokens {
                let (start, end) = (token.span.start, token.span.end);
                match token.kind {
                    TokenKind::String | TokenKind::StringHead | TokenKind::Filter => {
                        text_ranges.push((start, end));
                    }
                    TokenKind::StringMiddle | TokenKind::StringTail => {
                        if let Some(brace) = super::closing_brace(text, start, end, &lexed.comments)
                        {
                            text_ranges.push((brace, end));
                        }
                    }
                    _ => {}
                }
            }
            let sites: Vec<u32> = lexed
                .diagnostics
                .iter()
                .filter(|d| d.code == Code::HS0005)
                .filter_map(|d| d.location.as_ref().map(|loc| loc.span.start))
                .filter(|&at| !text_ranges.iter().any(|&(s, e)| s <= at && at < e))
                .collect();
            // Both directions, with equal counts: one invalid range per site.
            assert_eq!(invalid.len(), sites.len(), "{text:?}");
            for at in sites {
                assert!(
                    invalid
                        .iter()
                        .any(|h| h.span.start <= at && at < h.span.end),
                    "{text:?}: HS0005 at {at} not invalid"
                );
            }
        }
    }

    #[test]
    fn a_source_over_the_size_limit_answers_nothing() {
        let big = "a ".repeat(hollowscript_core::source::MAX_SOURCE_BYTES);
        assert!(highlight(&big).is_empty());
    }

    #[test]
    fn the_class_names_are_the_lower_case_words() {
        let names: Vec<&str> = [
            TokenClass::Keyword,
            TokenClass::Identifier,
            TokenClass::Number,
            TokenClass::String,
            TokenClass::Filter,
            TokenClass::Operator,
            TokenClass::Punctuation,
            TokenClass::Comment,
            TokenClass::Invalid,
        ]
        .into_iter()
        .map(TokenClass::as_str)
        .collect();
        assert_eq!(
            names,
            vec![
                "keyword",
                "identifier",
                "number",
                "string",
                "filter",
                "operator",
                "punctuation",
                "comment",
                "invalid"
            ]
        );
    }
}
