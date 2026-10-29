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
//! them are ordinary tokens.

use hollowscript_core::{Control, Source, SourceId, Span};
use hollowscript_syntax::{TokenKind, lex};

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
/// answers nothing. A byte the lexer rejects (it reports a diagnostic and
/// produces no token) is simply not covered, so the ranges returned never
/// include text the language does not recognise.
#[must_use]
pub fn highlight(text: &str) -> Vec<Highlight> {
    let Ok(source) = Source::new(SourceId(0), "highlight", text) else {
        return Vec::new();
    };
    let lexed = lex(&source, &mut Control::none());
    let mut out: Vec<Highlight> = Vec::with_capacity(lexed.tokens.len() + lexed.comments.len());
    for token in &lexed.tokens {
        if token.span.start == token.span.end {
            continue;
        }
        if let Some(class) = class_of(token.kind) {
            out.push(Highlight {
                span: token.span,
                class,
            });
        }
    }
    for comment in &lexed.comments {
        if comment.span.start != comment.span.end {
            out.push(Highlight {
                span: comment.span,
                class: TokenClass::Comment,
            });
        }
    }
    // Tokens and comments are each in order already; one stable sort
    // interleaves them.
    out.sort_by_key(|h| h.span.start);
    out
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
    fn text_the_lexer_rejects_is_not_covered_and_nothing_panics() {
        // A stray character the language has no token for.
        let found = classes("a # b");
        assert!(found.contains(&("a", TokenClass::Identifier)));
        assert!(found.contains(&("b", TokenClass::Identifier)));
        assert!(found.iter().all(|(text, _)| !text.contains('#')));
        // Unterminated string and filter literals still lex.
        assert!(!highlight("\"never closed").is_empty());
        assert!(!highlight("`never closed").is_empty());
        assert!(highlight("").is_empty());
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
                "comment"
            ]
        );
    }
}
