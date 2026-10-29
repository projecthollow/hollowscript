//! Tokens (chapter 1).
//!
//! A token knows its kind and its span and nothing else. The text of an
//! identifier or a number is the text of its span, and the parser interns or
//! converts it. Strings that contain interpolations are cut into a head, any
//! number of middles and a tail, so that the tokens of an interpolation are
//! ordinary tokens and no pass has to recurse into a string.

use hollowscript_core::Span;

/// The 27 reserved words (section 1.3).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Keyword {
    /// `and`.
    And,
    /// `assert`, reserved for a later version (HS0104).
    Assert,
    /// `break`.
    Break,
    /// `constant`.
    Constant,
    /// `continue`.
    Continue,
    /// `effect`.
    Effect,
    /// `else`.
    Else,
    /// `enum`.
    Enum,
    /// `export`.
    Export,
    /// `false`.
    False,
    /// `fn`.
    Fn,
    /// `for`.
    For,
    /// `from`.
    From,
    /// `if`.
    If,
    /// `import`.
    Import,
    /// `in`.
    In,
    /// `match`.
    Match,
    /// `none`.
    None,
    /// `not`.
    Not,
    /// `or`.
    Or,
    /// `record`.
    Record,
    /// `return`.
    Return,
    /// `test`, reserved for a later version (HS0104).
    Test,
    /// `true`.
    True,
    /// `uses`.
    Uses,
    /// `variable`.
    Variable,
    /// `while`.
    While,
}

impl Keyword {
    /// Every keyword, in alphabetical order.
    pub const ALL: [Keyword; 27] = [
        Keyword::And,
        Keyword::Assert,
        Keyword::Break,
        Keyword::Constant,
        Keyword::Continue,
        Keyword::Effect,
        Keyword::Else,
        Keyword::Enum,
        Keyword::Export,
        Keyword::False,
        Keyword::Fn,
        Keyword::For,
        Keyword::From,
        Keyword::If,
        Keyword::Import,
        Keyword::In,
        Keyword::Match,
        Keyword::None,
        Keyword::Not,
        Keyword::Or,
        Keyword::Record,
        Keyword::Return,
        Keyword::Test,
        Keyword::True,
        Keyword::Uses,
        Keyword::Variable,
        Keyword::While,
    ];

    /// The word as written.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Keyword::And => "and",
            Keyword::Assert => "assert",
            Keyword::Break => "break",
            Keyword::Constant => "constant",
            Keyword::Continue => "continue",
            Keyword::Effect => "effect",
            Keyword::Else => "else",
            Keyword::Enum => "enum",
            Keyword::Export => "export",
            Keyword::False => "false",
            Keyword::Fn => "fn",
            Keyword::For => "for",
            Keyword::From => "from",
            Keyword::If => "if",
            Keyword::Import => "import",
            Keyword::In => "in",
            Keyword::Match => "match",
            Keyword::None => "none",
            Keyword::Not => "not",
            Keyword::Or => "or",
            Keyword::Record => "record",
            Keyword::Return => "return",
            Keyword::Test => "test",
            Keyword::True => "true",
            Keyword::Uses => "uses",
            Keyword::Variable => "variable",
            Keyword::While => "while",
        }
    }

    /// The keyword spelled `word`. Keywords are lower case and case matters.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Keyword> {
        Keyword::ALL.into_iter().find(|k| k.as_str() == word)
    }

    /// Whether the word is reserved for a later version, so that using it
    /// anywhere is HS0104.
    #[must_use]
    pub const fn is_future(self) -> bool {
        matches!(self, Keyword::Test | Keyword::Assert)
    }
}

/// The kind of a token.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// An identifier.
    Ident,
    /// An `Int` literal. The value is read from the text of the span.
    Int,
    /// A `Float` literal.
    Float,
    /// A string without interpolation, quotes included.
    String,
    /// The start of a string with interpolation: from the opening quote up to and including the first `${`.
    StringHead,
    /// The text between two interpolations: from a closing `}` up to and including the next `${`.
    StringMiddle,
    /// The end of a string with interpolation: from the last closing `}` up to and including the closing quote.
    StringTail,
    /// A filter literal, backticks included.
    Filter,
    /// A reserved word.
    Keyword(Keyword),
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `[`.
    LBracket,
    /// `]`.
    RBracket,
    /// `{`.
    LBrace,
    /// `}`.
    RBrace,
    /// `,`.
    Comma,
    /// `:`.
    Colon,
    /// `.`.
    Dot,
    /// `..`.
    DotDot,
    /// `?`.
    Question,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Star,
    /// `/`.
    Slash,
    /// `=`.
    Assign,
    /// `==`.
    EqEq,
    /// `!=`.
    NotEq,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `<=`.
    LtEq,
    /// `>=`.
    GtEq,
    /// The virtual statement terminator (section 1.8).
    Term,
    /// The end of the file.
    Eof,
}

impl TokenKind {
    /// Whether a line terminator after a token of this kind ends the
    /// statement, and so gets a `TERM` (section 1.8).
    ///
    /// The kinds are identifiers, literals, `true`, `false`, `none`,
    /// `return`, `break`, `continue` and the closers `)`, `]`, `}` and `?`.
    /// A string head or middle is inside a string and never ends a statement.
    #[must_use]
    pub const fn ends_statement(self) -> bool {
        match self {
            TokenKind::Ident
            | TokenKind::Int
            | TokenKind::Float
            | TokenKind::String
            | TokenKind::StringTail
            | TokenKind::Filter
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::Question => true,
            TokenKind::Keyword(k) => matches!(
                k,
                Keyword::True
                    | Keyword::False
                    | Keyword::None
                    | Keyword::Return
                    | Keyword::Break
                    | Keyword::Continue
            ),
            _ => false,
        }
    }
}

/// A token.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// The bytes it covers. A `Term` covers its line terminator, and the `Term`
    /// at the end of a file and the `Eof` have the empty span at the end.
    pub span: Span,
    /// The literal was malformed and its diagnostic is already reported, so the
    /// parser accepts it wherever a token of its kind is allowed and the
    /// checker reports nothing further about it (section 1.9).
    pub malformed: bool,
}

/// The kinds of comment that tools may retain and query (section 1.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CommentKind {
    /// Any comment that is not tagged.
    Plain,
    /// `// todo: ...`.
    Todo,
    /// `// explain: ...`.
    Explain,
    /// `// note: ...`.
    Note,
}

/// A comment. Comments are not tokens, and a tool that rewrites source keeps them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// From the `//` up to, not including, the line terminator.
    pub span: Span,
    /// Whether it is tagged.
    pub kind: CommentKind,
}

impl CommentKind {
    /// The kind of the comment whose text, `//` included, is `text`. A comment
    /// is tagged when, after the `//` and any spaces or tabs, it starts with
    /// `todo`, `explain` or `note` in lower case immediately followed by `:`.
    #[must_use]
    pub fn classify(text: &str) -> CommentKind {
        let body = text
            .strip_prefix("//")
            .unwrap_or(text)
            .trim_start_matches([' ', '\t']);
        for (tag, kind) in [
            ("todo:", CommentKind::Todo),
            ("explain:", CommentKind::Explain),
            ("note:", CommentKind::Note),
        ] {
            if body.starts_with(tag) {
                return kind;
            }
        }
        CommentKind::Plain
    }
}

/// The text of a string part with its escapes replaced (section 1.5). An
/// escape that is not valid is kept as written, because the lexer has already
/// reported it.
#[must_use]
pub fn decode_string_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('$') => out.push('$'),
            Some('u') => decode_unicode_escape(&mut chars, &mut out),
            // `\\` is one backslash, and so is a backslash that ends the text.
            Some('\\') | None => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

fn decode_unicode_escape(chars: &mut std::str::Chars<'_>, out: &mut String) {
    let saved = chars.clone();
    if chars.next() == Some('{') {
        let mut digits = String::new();
        for c in chars.by_ref() {
            if c == '}' {
                if let Some(ch) = u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    out.push(ch);
                    return;
                }
                break;
            }
            digits.push(c);
        }
    }
    *chars = saved;
    out.push_str("\\u");
}

/// The source of a filter literal from its text between the backticks
/// (section 1.6): `\`` is one backtick, `\\` is one backslash, and a backslash
/// before anything else is kept.
#[must_use]
pub fn decode_filter_source(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && matches!(chars.peek(), Some('`' | '\\')) {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    out
}

/// Maps a byte offset in a filter literal's *decoded* source (as
/// [`decode_filter_source`] produces it) back to a byte offset in `raw`, the
/// literal's own text between the backticks, so that a diagnostic Appendix A
/// raises against the decoded source can be reported at the right place in
/// the program file (section A.1: "an offset in it is turned into a file
/// offset by adding the widths of the escape sequences before it"). Adding
/// the literal's own start (one past its opening backtick) to the result
/// gives the file offset.
#[must_use]
pub fn filter_raw_offset(raw: &str, decoded_offset: u32) -> u32 {
    let mut chars = raw.chars().peekable();
    let mut raw_pos = 0u32;
    let mut decoded_pos = 0u32;
    while decoded_pos < decoded_offset {
        let Some(c) = chars.next() else { break };
        let c_width = u32::try_from(c.len_utf8()).unwrap_or(1);
        if c == '\\' && matches!(chars.peek(), Some('`' | '\\')) {
            let Some(escaped) = chars.next() else { break };
            let escaped_width = u32::try_from(escaped.len_utf8()).unwrap_or(1);
            raw_pos += c_width + escaped_width;
            decoded_pos += escaped_width;
        } else {
            raw_pos += c_width;
            decoded_pos += c_width;
        }
    }
    raw_pos
}

#[cfg(test)]
mod tests {
    use super::{
        CommentKind, Keyword, TokenKind, decode_filter_source, decode_string_text,
        filter_raw_offset,
    };

    #[test]
    fn filter_raw_offset_accounts_for_escapes_before_it() {
        // Decoded: `a`b (3 code points: a, `, b). Raw: a\`b (4 bytes).
        let raw = "a\\`b";
        assert_eq!(decode_filter_source(raw), "a`b");
        assert_eq!(filter_raw_offset(raw, 0), 0); // before 'a'
        assert_eq!(filter_raw_offset(raw, 1), 1); // before '`', after 'a'
        assert_eq!(filter_raw_offset(raw, 2), 3); // before 'b', after the escape
        assert_eq!(filter_raw_offset(raw, 3), 4); // the end
        // No escapes: the two offsets agree everywhere.
        assert_eq!(filter_raw_offset("status:open", 7), 7);
    }

    #[test]
    fn keywords_round_trip_and_are_sorted() {
        for k in Keyword::ALL {
            assert_eq!(Keyword::from_word(k.as_str()), Some(k));
        }
        assert!(
            Keyword::ALL
                .windows(2)
                .all(|w| w[0].as_str() < w[1].as_str())
        );
        assert_eq!(Keyword::from_word("Fn"), None);
        assert_eq!(Keyword::from_word("print"), None);
        assert!(
            Keyword::Test.is_future() && Keyword::Assert.is_future() && !Keyword::Enum.is_future()
        );
    }

    #[test]
    fn terminators_follow_section_1_8() {
        use TokenKind::{
            Comma, Ident, Int, Keyword as Kw, LBrace, Plus, RBrace, RParen, StringHead,
            StringMiddle, StringTail,
        };
        for kind in [
            Ident,
            Int,
            RParen,
            RBrace,
            StringTail,
            Kw(Keyword::True),
            Kw(Keyword::Return),
            Kw(Keyword::Continue),
        ] {
            assert!(kind.ends_statement(), "{kind:?}");
        }
        for kind in [
            Plus,
            Comma,
            LBrace,
            StringHead,
            StringMiddle,
            Kw(Keyword::Else),
            Kw(Keyword::Fn),
            Kw(Keyword::Effect),
        ] {
            assert!(!kind.ends_statement(), "{kind:?}");
        }
    }

    #[test]
    fn tagged_comments() {
        assert_eq!(CommentKind::classify("// todo: fix"), CommentKind::Todo);
        assert_eq!(CommentKind::classify("//\t note: x"), CommentKind::Note);
        assert_eq!(CommentKind::classify("//explain:x"), CommentKind::Explain);
        assert_eq!(CommentKind::classify("// TODO: x"), CommentKind::Plain);
        assert_eq!(CommentKind::classify("// todo : x"), CommentKind::Plain);
        assert_eq!(CommentKind::classify("// see note: x"), CommentKind::Plain);
    }

    #[test]
    fn string_escapes_are_decoded() {
        assert_eq!(decode_string_text(r#"a\"b\\c\n\t\r\$"#), "a\"b\\c\n\t\r$");
        assert_eq!(decode_string_text(r"\u{1F600}\u{41}"), "\u{1F600}A");
        assert_eq!(decode_string_text(r"\u{0}"), "\0");
        // Invalid escapes were reported by the lexer and are kept.
        assert_eq!(decode_string_text(r"\q"), "\\q");
        assert_eq!(decode_string_text(r"\u{D800}"), "\\u{D800}");
        assert_eq!(decode_string_text(r"\u{"), "\\u{");
        assert_eq!(decode_string_text("end\\"), "end\\");
    }

    #[test]
    fn filter_sources_are_unescaped() {
        assert_eq!(decode_filter_source(r"a\`b\\c\d"), "a`b\\c\\d");
        assert_eq!(decode_filter_source("plain"), "plain");
    }
}
