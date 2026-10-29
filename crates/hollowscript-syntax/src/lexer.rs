//! The lexer (chapter 1).
//!
//! The lexer never stops at the first error. It reads left to right by maximal
//! munch, inserts the virtual `TERM` token by the rule of section 1.8,
//! reports every lexical error of section 1.9 once, and produces a malformed
//! token of the right kind wherever a literal is malformed, so the parser and
//! the checker report nothing further about it. It checks the token budget of
//! section 12.1 with [`hollowscript_core::Control`] every 4,096 tokens.
//!
//! String interpolation is lexed without recursion: entering `${` pushes a
//! frame on an explicit stack and control returns to the main scan loop,
//! which lexes the interpolation's expression as ordinary tokens (counting
//! braces so that a record literal inside it does not close the
//! interpolation early); the matching `}` pops the frame and resumes
//! scanning the string's text. This keeps the lexer's own native stack usage
//! independent of how deeply strings and interpolations nest.

use crate::token::{Comment, CommentKind, Keyword, Token, TokenKind};
use hollowscript_core::limits::structural::{FRONT_END_TOKEN_INTERVAL, MAX_INTERPOLATION_DEPTH};
use hollowscript_core::{
    BoundedDiagnostics, Code, Control, Diagnostic, PositionCursor, Source, Span,
};
use std::fmt::Display;

/// The result of lexing one source.
#[derive(Clone, Debug, PartialEq)]
pub struct LexOutput {
    /// The tokens, ending with `Eof`.
    pub tokens: Vec<Token>,
    /// The comments, in source order.
    pub comments: Vec<Comment>,
    /// The lexical diagnostics, in the order found, at most 101 of them: the
    /// first 100 of the canonical order for the file and the one after, as
    /// [`hollowscript_core::BoundedDiagnostics`] keeps them, because a source
    /// can provoke one for nearly every byte and nothing past the cap is ever
    /// reported. Callers put them in the canonical order, and add the notice
    /// that more were found, with a [`hollowscript_core::DiagnosticSet`].
    pub diagnostics: Vec<Diagnostic>,
}

/// One open string interpolation: the source is inside a `${` that has not
/// yet met its matching `}`.
struct InterpFrame {
    /// The combined nesting of `(`, `[` and `{` opened since this
    /// interpolation started that have not yet been matched by a closer
    /// (the same single counter section 1.10 uses for structural depth): the
    /// interpolation's own closing `}` is the first `}` found while this is
    /// zero. A raw line terminator is fatal to the interpolation only while
    /// this is zero too, so a call whose arguments are spread over several
    /// lines works inside `${ }` exactly as it does anywhere else, and only
    /// a line terminator with nothing open ends it abnormally (section 1.5).
    bracket_depth: u32,
    /// The span of the `${` that opened this frame, for HS0007 and HS0115.
    dollar_span: Span,
    /// Whether this frame is nested past the interpolation depth limit of
    /// section 1.10 and HS0115 has already been reported for it: a `${`
    /// nested inside such a frame, however much deeper, inherits this and is
    /// not reported again, so one violation produces one diagnostic. A fresh
    /// excursion past the limit, once the stack has unwound below it again,
    /// starts this at `false` and is reported once in its own right.
    over_limit: bool,
}

struct Lexer<'a> {
    text: &'a str,
    pos: u32,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    diagnostics: BoundedDiagnostics,
    interp_stack: Vec<InterpFrame>,
    /// The kind of the last real (non-`Term`) token emitted since the last
    /// `Term`, or since the start of the file. `None` once a line terminator
    /// has already produced a `Term` (or found nothing to trigger one), so
    /// that further blank or comment-only lines insert no more (section 1.8).
    pending_term: Option<TokenKind>,
    tokens_since_safepoint: u32,
    cancelled: bool,
    source: &'a Source,
    /// Resumes each diagnostic's column count from the previous one instead
    /// of the start of its line: diagnostics are found in non-decreasing
    /// offset order during this left-to-right scan, so this keeps locating
    /// all of them to time proportional to the source's length in total,
    /// rather than quadratic in a source with many of them on one long line
    /// (section 19.1).
    position_cursor: PositionCursor,
}

/// Turns a source into tokens.
#[must_use]
pub fn lex(source: &Source, control: &mut Control<'_>) -> LexOutput {
    let mut lexer = Lexer {
        text: source.text(),
        pos: 0,
        tokens: Vec::new(),
        comments: Vec::new(),
        diagnostics: BoundedDiagnostics::new(),
        interp_stack: Vec::new(),
        pending_term: None,
        tokens_since_safepoint: 0,
        cancelled: false,
        source,
        position_cursor: PositionCursor::new(),
    };
    lexer.run(control);
    LexOutput {
        tokens: lexer.tokens,
        comments: lexer.comments,
        diagnostics: lexer.diagnostics.into_vec(),
    }
}

/// Whether `c` is one of the code points section 1.1 rejects everywhere
/// (a byte order mark that is not the first character, the bidirectional
/// controls, a C0 control other than TAB, DEL, or any other code point with
/// the `White_Space` property besides SPACE, TAB and the line terminators).
fn is_rejected(c: char) -> bool {
    matches!(c,
        '\u{0}'..='\u{8}'
            | '\u{b}'
            | '\u{c}'
            | '\u{e}'..='\u{1f}'
            | '\u{7f}'
            | '\u{feff}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}')
        || (c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n' | '\r'))
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl Lexer<'_> {
    fn push_diag(&mut self, code: Code, args: &[(&str, &dyn Display)], span: Span) {
        let location = self.source.locate_with(span, &mut self.position_cursor);
        self.diagnostics
            .push(Diagnostic::new(code, args).with_location(location));
    }

    fn peek_char(&self) -> Option<char> {
        self.text.get(self.pos as usize..)?.chars().next()
    }

    fn peek_char_at(&self, n: usize) -> Option<char> {
        self.text.get(self.pos as usize..)?.chars().nth(n)
    }

    fn push_token(&mut self, token: Token) {
        if let Some(frame) = self.interp_stack.last_mut() {
            match token.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    frame.bracket_depth += 1;
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    frame.bracket_depth = frame.bracket_depth.saturating_sub(1);
                }
                _ => {}
            }
        }
        self.pending_term = Some(token.kind);
        self.tokens_since_safepoint = self.tokens_since_safepoint.saturating_add(1);
        self.tokens.push(token);
    }

    fn emit_punct(&mut self, kind: TokenKind, len: u32) {
        let span = Span::new(self.pos, self.pos + len);
        self.pos += len;
        self.push_token(Token {
            kind,
            span,
            malformed: false,
        });
    }

    fn run(&mut self, control: &mut Control<'_>) {
        if self.text.starts_with('\u{feff}') {
            self.pos = 3;
        }
        loop {
            if self.tokens_since_safepoint >= FRONT_END_TOKEN_INTERVAL {
                self.tokens_since_safepoint = 0;
                if control.safepoint().is_err() {
                    self.cancelled = true;
                    break;
                }
            }
            let Some(c) = self.peek_char() else { break };
            self.step(c);
        }
        self.finish();
    }

    fn step(&mut self, c: char) {
        if c == '\n' {
            self.handle_line_terminator(1);
            return;
        }
        if c == '\r' {
            if self.peek_char_at(1) == Some('\n') {
                self.handle_line_terminator(2);
            } else {
                self.push_diag(
                    Code::HS0005,
                    &[("char", &'\r')],
                    Span::new(self.pos, self.pos + 1),
                );
                self.pos += 1;
            }
            return;
        }
        if c == ' ' || c == '\t' {
            self.pos += 1;
            return;
        }
        if is_rejected(c) {
            let width = u32::try_from(c.len_utf8()).unwrap_or(1);
            self.push_diag(
                Code::HS0005,
                &[("char", &c)],
                Span::new(self.pos, self.pos + width),
            );
            self.pos += width;
            return;
        }
        if c == '/' && self.peek_char_at(1) == Some('/') {
            self.scan_comment();
            return;
        }
        if c == '"' {
            self.pos += 1;
            let start = self.pos - 1;
            self.scan_string_text(start, false);
            return;
        }
        if c == '`' {
            self.scan_filter();
            return;
        }
        if c == '{' {
            self.emit_punct(TokenKind::LBrace, 1);
            return;
        }
        if c == '}' {
            let closes_interpolation = self
                .interp_stack
                .last()
                .is_some_and(|frame| frame.bracket_depth == 0);
            if closes_interpolation {
                self.interp_stack.pop();
                self.pos += 1;
                let start = self.tokens.last().map_or(self.pos, |t| t.span.end);
                self.scan_string_text(start, true);
            } else {
                self.emit_punct(TokenKind::RBrace, 1);
            }
            return;
        }
        if is_ident_start(c) {
            self.scan_ident_or_keyword();
            return;
        }
        if c.is_ascii_digit() {
            self.scan_number();
            return;
        }
        if self.scan_operator(c) {
            return;
        }
        let width = u32::try_from(c.len_utf8()).unwrap_or(1);
        self.push_diag(
            Code::HS0005,
            &[("char", &c)],
            Span::new(self.pos, self.pos + width),
        );
        self.pos += width;
    }

    fn handle_line_terminator(&mut self, width: u32) {
        let span = Span::new(self.pos, self.pos + width);
        let abandons_interpolation = self
            .interp_stack
            .last()
            .is_some_and(|frame| frame.bracket_depth == 0);
        if abandons_interpolation {
            if let Some(frame) = self.interp_stack.pop() {
                self.abandon_interpolation(&frame, span.start);
            }
            return;
        }
        self.pos += width;
        if let Some(kind) = self.pending_term.take() {
            if kind.ends_statement() {
                self.push_token(Token {
                    kind: TokenKind::Term,
                    span,
                    malformed: false,
                });
            }
        }
    }

    fn abandon_interpolation(&mut self, frame: &InterpFrame, end: u32) {
        self.push_diag(Code::HS0007, &[], frame.dollar_span);
        let start = self.tokens.last().map_or(self.pos, |t| t.span.end);
        let start = start.min(end);
        self.push_token(Token {
            kind: TokenKind::StringTail,
            span: Span::new(start, end),
            malformed: true,
        });
    }

    fn scan_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        while let Some(c) = self.peek_char() {
            if c == '\n' || c == '\r' {
                break;
            }
            if is_rejected(c) {
                let width = u32::try_from(c.len_utf8()).unwrap_or(1);
                self.push_diag(
                    Code::HS0005,
                    &[("char", &c)],
                    Span::new(self.pos, self.pos + width),
                );
            }
            self.pos += u32::try_from(c.len_utf8()).unwrap_or(1);
        }
        let span = Span::new(start, self.pos);
        let text = self
            .text
            .get(start as usize..self.pos as usize)
            .unwrap_or("");
        self.comments.push(Comment {
            span,
            kind: CommentKind::classify(text),
        });
    }

    /// Scans string text (section 1.5) from `start` (the opening quote, or
    /// the position right after a `}` that closed an interpolation) up to a
    /// closing quote, the start of a new interpolation, or an abnormal end.
    /// `is_continuation` selects `StringTail`/`StringMiddle` over
    /// `String`/`StringHead`.
    fn scan_string_text(&mut self, start: u32, is_continuation: bool) {
        loop {
            match self.peek_char() {
                None | Some('\n') => {
                    self.push_diag(Code::HS0006, &[], Span::new(start, self.pos));
                    self.emit_string_part(start, is_continuation, true);
                    return;
                }
                Some('\r') => {
                    if self.peek_char_at(1) == Some('\n') {
                        // A raw CR LF line terminator (section 1.1): the
                        // pair is left unconsumed, exactly as a lone LF is
                        // above, so the caller's normal line-terminator
                        // handling applies to it once this literal has ended.
                        self.push_diag(Code::HS0006, &[], Span::new(start, self.pos));
                        self.emit_string_part(start, is_continuation, true);
                        return;
                    }
                    // A CR not immediately followed by LF is an unrecognised
                    // character wherever it occurs (section 1.1), including
                    // here: one HS0005 for the CR alone, and the literal
                    // keeps being scanned, exactly as for any other rejected
                    // code point mid-literal.
                    self.push_diag(
                        Code::HS0005,
                        &[("char", &'\r')],
                        Span::new(self.pos, self.pos + 1),
                    );
                    self.pos += 1;
                }
                Some('"') => {
                    self.pos += 1;
                    self.emit_string_part(start, is_continuation, false);
                    return;
                }
                Some('$') if self.peek_char_at(1) == Some('{') => {
                    let dollar_span = Span::new(self.pos, self.pos + 2);
                    self.pos += 2;
                    self.emit_string_head_or_middle(start, is_continuation, dollar_span);
                    return;
                }
                Some('\\') => self.scan_string_escape(),
                Some(c) => {
                    if is_rejected(c) {
                        let width = u32::try_from(c.len_utf8()).unwrap_or(1);
                        self.push_diag(
                            Code::HS0005,
                            &[("char", &c)],
                            Span::new(self.pos, self.pos + width),
                        );
                    }
                    self.pos += u32::try_from(c.len_utf8()).unwrap_or(1);
                }
            }
        }
    }

    fn emit_string_part(&mut self, start: u32, is_continuation: bool, malformed: bool) {
        let kind = if is_continuation {
            TokenKind::StringTail
        } else {
            TokenKind::String
        };
        self.push_token(Token {
            kind,
            span: Span::new(start, self.pos),
            malformed,
        });
    }

    fn emit_string_head_or_middle(&mut self, start: u32, is_continuation: bool, dollar_span: Span) {
        let kind = if is_continuation {
            TokenKind::StringMiddle
        } else {
            TokenKind::StringHead
        };
        self.push_token(Token {
            kind,
            span: Span::new(start, self.pos),
            malformed: false,
        });
        // Section 1.10: "each violation is one diagnostic and the offending
        // construct is parsed no further". A frame inherited from an
        // enclosing one already past the limit is past it too, but does not
        // get a second HS0115: only the `${` that first crosses the limit is
        // reported, however much deeper the nesting then goes.
        let already_over_limit = self.interp_stack.last().is_some_and(|f| f.over_limit);
        let over_limit =
            already_over_limit || self.interp_stack.len() >= MAX_INTERPOLATION_DEPTH as usize;
        if over_limit && !already_over_limit {
            self.push_diag(
                Code::HS0115,
                &[("limit", &MAX_INTERPOLATION_DEPTH)],
                dollar_span,
            );
        }
        self.interp_stack.push(InterpFrame {
            bracket_depth: 0,
            dollar_span,
            over_limit,
        });
    }

    fn scan_string_escape(&mut self) {
        let backslash_start = self.pos;
        self.pos += 1;
        match self.peek_char() {
            Some(c @ ('"' | '\\' | 'n' | 't' | 'r' | '$')) => {
                self.pos += u32::try_from(c.len_utf8()).unwrap_or(1);
            }
            Some('u') => {
                if self.peek_char_at(1) == Some('{') {
                    self.scan_unicode_escape(backslash_start);
                } else {
                    self.push_diag(Code::HS0008, &[], Span::new(backslash_start, self.pos + 1));
                    self.pos += 1;
                }
            }
            Some(other) if other != '\n' && other != '\r' => {
                let width = u32::try_from(other.len_utf8()).unwrap_or(1);
                self.push_diag(
                    Code::HS0008,
                    &[],
                    Span::new(backslash_start, self.pos + width),
                );
                self.pos += width;
            }
            _ => {
                // A backslash right before the end of the string's text (end
                // of file or a raw line terminator) is left as one ordinary
                // byte; the caller's next iteration reports HS0006.
            }
        }
    }

    fn scan_unicode_escape(&mut self, backslash_start: u32) {
        self.pos += 2; // 'u' '{'
        let digits_start = self.pos;
        let mut count = 0u32;
        while let Some(c) = self.peek_char() {
            if c.is_ascii_hexdigit() && count < 6 {
                self.pos += 1;
                count += 1;
            } else {
                break;
            }
        }
        let digits_end = self.pos;
        let closed = self.peek_char() == Some('}');
        let digits = self
            .text
            .get(digits_start as usize..digits_end as usize)
            .unwrap_or("");
        let value = u32::from_str_radix(digits, 16).ok();
        let valid = closed
            && (1..=6).contains(&count)
            && value.is_some_and(|v| char::from_u32(v).is_some());
        if valid {
            self.pos += 1; // '}'
        } else {
            let end = if closed { self.pos + 1 } else { self.pos };
            self.push_diag(Code::HS0009, &[], Span::new(backslash_start, end));
            self.pos = end;
        }
    }

    fn scan_filter(&mut self) {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek_char() {
                None | Some('\n') => {
                    self.push_diag(Code::HS0010, &[], Span::new(start, self.pos));
                    self.push_token(Token {
                        kind: TokenKind::Filter,
                        span: Span::new(start, self.pos),
                        malformed: true,
                    });
                    return;
                }
                Some('\r') => {
                    if self.peek_char_at(1) == Some('\n') {
                        // A raw CR LF terminator: left unconsumed, as above.
                        self.push_diag(Code::HS0010, &[], Span::new(start, self.pos));
                        self.push_token(Token {
                            kind: TokenKind::Filter,
                            span: Span::new(start, self.pos),
                            malformed: true,
                        });
                        return;
                    }
                    // A lone CR is an unrecognised character (section 1.1),
                    // not a terminator: one HS0005, and the filter keeps
                    // being scanned.
                    self.push_diag(
                        Code::HS0005,
                        &[("char", &'\r')],
                        Span::new(self.pos, self.pos + 1),
                    );
                    self.pos += 1;
                }
                Some('`') => {
                    self.pos += 1;
                    self.push_token(Token {
                        kind: TokenKind::Filter,
                        span: Span::new(start, self.pos),
                        malformed: false,
                    });
                    return;
                }
                Some('\\') => {
                    self.pos += 1;
                    if let Some(c2) = self.peek_char() {
                        if c2 != '\n' && c2 != '\r' {
                            self.pos += u32::try_from(c2.len_utf8()).unwrap_or(1);
                        }
                    }
                }
                Some(c) => {
                    if is_rejected(c) {
                        let width = u32::try_from(c.len_utf8()).unwrap_or(1);
                        self.push_diag(
                            Code::HS0005,
                            &[("char", &c)],
                            Span::new(self.pos, self.pos + width),
                        );
                    }
                    self.pos += u32::try_from(c.len_utf8()).unwrap_or(1);
                }
            }
        }
    }

    fn scan_ident_or_keyword(&mut self) {
        let start = self.pos;
        self.pos += 1;
        let mut malformed = false;
        while let Some(c) = self.peek_char() {
            if is_ident_continue(c) {
                self.pos += 1;
                continue;
            }
            // Section 1.2: an identifier is ASCII. A letter or digit that is
            // not is part of the name the author wrote, not the start of the
            // next token, so the whole word is taken as one malformed
            // identifier and reported once. Splitting it instead would leave
            // the ASCII prefix behind as a name nobody wrote: `café` would
            // declare `caf`.
            if c.is_alphanumeric() {
                if !malformed {
                    let width = u32::try_from(c.len_utf8()).unwrap_or(1);
                    self.push_diag(
                        Code::HS0005,
                        &[("char", &c)],
                        Span::new(self.pos, self.pos + width),
                    );
                    malformed = true;
                }
                self.pos += u32::try_from(c.len_utf8()).unwrap_or(1);
                continue;
            }
            break;
        }
        let span = Span::new(start, self.pos);
        let text = self
            .text
            .get(start as usize..self.pos as usize)
            .unwrap_or("");
        let kind = match Keyword::from_word(text) {
            Some(k) if !malformed => TokenKind::Keyword(k),
            _ => TokenKind::Ident,
        };
        self.push_token(Token {
            kind,
            span,
            malformed,
        });
    }

    /// Consumes `digit { [ "_" ] digit }`, only taking an underscore when it
    /// is directly followed by a digit.
    fn consume_digits(&mut self) {
        if self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        } else {
            return;
        }
        loop {
            match self.peek_char() {
                Some(c) if c.is_ascii_digit() => self.pos += 1,
                Some('_') if self.peek_char_at(1).is_some_and(|c| c.is_ascii_digit()) => {
                    self.pos += 2;
                }
                _ => break,
            }
        }
    }

    fn scan_number(&mut self) {
        let start = self.pos;
        self.consume_digits();
        let mut is_float = false;

        if self.peek_char() == Some('.') {
            if self.peek_char_at(1).is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                self.pos += 1;
                self.consume_digits();
            } else {
                self.pos += 1;
                let span = Span::new(start, self.pos);
                self.push_diag(Code::HS0011, &[], span);
                self.push_token(Token {
                    kind: TokenKind::Float,
                    span,
                    malformed: true,
                });
                return;
            }
        }

        if matches!(self.peek_char(), Some('e' | 'E')) {
            let mut p = self.pos + 1;
            if matches!(self.text.as_bytes().get(p as usize), Some(b'+' | b'-')) {
                p += 1;
            }
            let digits_start = p;
            let mut q = p;
            while self
                .text
                .as_bytes()
                .get(q as usize)
                .is_some_and(u8::is_ascii_digit)
            {
                q += 1;
            }
            if q > digits_start {
                is_float = true;
                self.pos = q;
            } else {
                self.pos = p;
                let span = Span::new(start, self.pos);
                self.push_diag(Code::HS0016, &[], span);
                self.push_token(Token {
                    kind: TokenKind::Float,
                    span,
                    malformed: true,
                });
                return;
            }
        }

        if matches!(self.peek_char(), Some(c) if c.is_ascii_alphabetic() || c == '_') {
            while matches!(self.peek_char(), Some(c) if c.is_ascii_alphanumeric() || c == '_') {
                self.pos += 1;
            }
            let span = Span::new(start, self.pos);
            self.push_diag(Code::HS0013, &[], span);
            self.push_token(Token {
                kind: if is_float {
                    TokenKind::Float
                } else {
                    TokenKind::Int
                },
                span,
                malformed: true,
            });
            return;
        }

        let span = Span::new(start, self.pos);
        let text: String = self.text[start as usize..self.pos as usize]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        if is_float {
            let value = text.parse::<f64>().unwrap_or(f64::INFINITY);
            if value.is_infinite() {
                self.push_diag(Code::HS0015, &[], span);
                self.push_token(Token {
                    kind: TokenKind::Float,
                    span,
                    malformed: true,
                });
            } else {
                self.push_token(Token {
                    kind: TokenKind::Float,
                    span,
                    malformed: false,
                });
            }
        } else {
            let mut value: u64 = 0;
            let mut overflowed = false;
            for b in text.bytes() {
                let d = u64::from(b - b'0');
                match value.checked_mul(10).and_then(|v| v.checked_add(d)) {
                    Some(v) => value = v,
                    None => overflowed = true,
                }
            }
            let too_large = overflowed || value > 9_223_372_036_854_775_808u64;
            if too_large {
                self.push_diag(Code::HS0014, &[], span);
            }
            self.push_token(Token {
                kind: TokenKind::Int,
                span,
                malformed: too_large,
            });
        }
    }

    /// Tries to scan a punctuation or operator token starting with `c`.
    /// Returns `false` for `!` not followed by `=`, which is HS0005.
    fn scan_operator(&mut self, c: char) -> bool {
        let start = self.pos;
        let next = self.peek_char_at(1);
        let (kind, len) = match c {
            '(' => (TokenKind::LParen, 1),
            ')' => (TokenKind::RParen, 1),
            '[' => (TokenKind::LBracket, 1),
            ']' => (TokenKind::RBracket, 1),
            ',' => (TokenKind::Comma, 1),
            ':' => (TokenKind::Colon, 1),
            '.' => {
                if next == Some('.') {
                    (TokenKind::DotDot, 2)
                } else if next.is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                    let digits_start = self.pos;
                    self.consume_digits();
                    let _ = digits_start;
                    let span = Span::new(start, self.pos);
                    self.push_diag(Code::HS0012, &[], span);
                    self.push_token(Token {
                        kind: TokenKind::Float,
                        span,
                        malformed: true,
                    });
                    return true;
                } else {
                    (TokenKind::Dot, 1)
                }
            }
            '?' => (TokenKind::Question, 1),
            '+' => (TokenKind::Plus, 1),
            '-' => (TokenKind::Minus, 1),
            '*' => (TokenKind::Star, 1),
            '/' => (TokenKind::Slash, 1),
            '=' => {
                if next == Some('=') {
                    (TokenKind::EqEq, 2)
                } else {
                    (TokenKind::Assign, 1)
                }
            }
            '!' => {
                if next == Some('=') {
                    (TokenKind::NotEq, 2)
                } else {
                    return false;
                }
            }
            '<' => {
                if next == Some('=') {
                    (TokenKind::LtEq, 2)
                } else {
                    (TokenKind::Lt, 1)
                }
            }
            '>' => {
                if next == Some('=') {
                    (TokenKind::GtEq, 2)
                } else {
                    (TokenKind::Gt, 1)
                }
            }
            _ => return false,
        };
        self.emit_punct(kind, len);
        true
    }

    fn finish(&mut self) {
        let end = u32::try_from(self.text.len()).unwrap_or(u32::MAX);
        while let Some(frame) = self.interp_stack.pop() {
            self.abandon_interpolation(&frame, end);
        }
        if !self.cancelled {
            if let Some(kind) = self.pending_term.take() {
                if kind.ends_statement() {
                    self.tokens.push(Token {
                        kind: TokenKind::Term,
                        span: Span::new(end, end),
                        malformed: false,
                    });
                }
            }
        }
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(end, end),
            malformed: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::lex;
    use crate::token::{CommentKind, Keyword, TokenKind};
    use hollowscript_core::{Code, Control, Source, SourceId};

    fn kinds(text: &str) -> Vec<TokenKind> {
        let source = Source::new(SourceId(0), "main.hws", text).unwrap();
        lex(&source, &mut Control::none())
            .tokens
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    fn codes(text: &str) -> Vec<Code> {
        let source = Source::new(SourceId(0), "main.hws", text).unwrap();
        lex(&source, &mut Control::none())
            .diagnostics
            .into_iter()
            .map(|d| d.code)
            .collect()
    }

    #[test]
    fn empty_source_is_just_eof() {
        assert_eq!(kinds(""), vec![TokenKind::Eof]);
    }

    #[test]
    fn keywords_and_identifiers() {
        use TokenKind::{Ident, Keyword as Kw};
        assert_eq!(
            kinds("if x"),
            vec![Kw(Keyword::If), Ident, TokenKind::Term, TokenKind::Eof]
        );
        assert_eq!(kinds("_"), vec![Ident, TokenKind::Term, TokenKind::Eof]);
        // `café` lexes as the identifier `caf` (ASCII only) followed by the
        // unrecognised character `é`; `caf` still ends the (missing) line
        // with a Term, since an identifier is a Term-triggering kind.
        assert_eq!(
            kinds("caf\u{e9}"),
            vec![Ident, TokenKind::Term, TokenKind::Eof]
        );
        assert_eq!(codes("caf\u{e9}"), vec![Code::HS0005]);
    }

    #[test]
    fn future_keywords_are_still_tokens() {
        // `test`/`assert` are not in the Term-triggering set, so none is
        // inserted at EOF here.
        assert_eq!(
            kinds("test"),
            vec![TokenKind::Keyword(Keyword::Test), TokenKind::Eof]
        );
        assert_eq!(
            kinds("test\n"),
            vec![TokenKind::Keyword(Keyword::Test), TokenKind::Eof]
        );
    }

    #[test]
    fn term_after_statement_ending_kinds_only() {
        assert_eq!(
            kinds("a\nb"),
            vec![
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
        assert_eq!(
            kinds("1 +\n2"),
            vec![
                TokenKind::Int,
                TokenKind::Plus,
                TokenKind::Int,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn blank_and_comment_only_lines_collapse_term() {
        assert_eq!(
            kinds("a\n\n\nb"),
            vec![
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
        let toks = kinds("a\n// hi\nb");
        assert_eq!(
            toks,
            vec![
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn term_span_is_the_first_line_terminator() {
        let source = Source::new(SourceId(0), "main.hws", "a\n\nb").unwrap();
        let out = lex(&source, &mut Control::none());
        let term = out
            .tokens
            .iter()
            .find(|t| t.kind == TokenKind::Term)
            .unwrap();
        assert_eq!((term.span.start, term.span.end), (1, 2));
    }

    #[test]
    fn no_term_before_eof_when_last_token_does_not_end_a_statement() {
        assert_eq!(
            kinds("1 +"),
            vec![TokenKind::Int, TokenKind::Plus, TokenKind::Eof]
        );
    }

    #[test]
    fn term_at_eof_when_no_trailing_newline() {
        let toks = kinds("return 1");
        assert_eq!(*toks.last().unwrap(), TokenKind::Eof);
        assert_eq!(toks[toks.len() - 2], TokenKind::Term);
    }

    #[test]
    fn crlf_is_one_terminator() {
        assert_eq!(
            kinds("a\r\nb"),
            vec![
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lone_cr_is_unrecognised() {
        assert_eq!(codes("a\rb"), vec![Code::HS0005]);
    }

    #[test]
    fn comments_are_collected_and_classified() {
        let source = Source::new(SourceId(0), "main.hws", "// todo: x\na // note: y\n").unwrap();
        let out = lex(&source, &mut Control::none());
        assert_eq!(out.comments.len(), 2);
        assert_eq!(out.comments[0].kind, CommentKind::Todo);
        assert_eq!(out.comments[1].kind, CommentKind::Note);
    }

    #[test]
    fn operators_and_punctuation() {
        use TokenKind::{Assign, EqEq, GtEq, LtEq, NotEq};
        // None of these punctuation kinds end a statement, so EOF adds no Term.
        assert_eq!(
            kinds("== != <= >= = .. ."),
            vec![
                EqEq,
                NotEq,
                LtEq,
                GtEq,
                Assign,
                TokenKind::DotDot,
                TokenKind::Dot,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn bang_without_eq_is_unrecognised() {
        assert_eq!(codes("a ! b"), vec![Code::HS0005]);
        assert_eq!(
            kinds("a ! b"),
            vec![
                TokenKind::Ident,
                TokenKind::Ident,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn simple_numbers() {
        assert_eq!(
            kinds("1_000_000"),
            vec![TokenKind::Int, TokenKind::Term, TokenKind::Eof]
        );
        assert_eq!(
            kinds("2.5e-3"),
            vec![TokenKind::Float, TokenKind::Term, TokenKind::Eof]
        );
        assert_eq!(
            kinds("1E3"),
            vec![TokenKind::Float, TokenKind::Term, TokenKind::Eof]
        );
        assert_eq!(
            kinds("007"),
            vec![TokenKind::Int, TokenKind::Term, TokenKind::Eof]
        );
    }

    #[test]
    fn malformed_numbers_are_one_diagnostic_each() {
        assert_eq!(codes("2."), vec![Code::HS0011]);
        assert_eq!(codes(".5"), vec![Code::HS0012]);
        assert_eq!(codes("1e"), vec![Code::HS0016]);
        assert_eq!(codes("1e+"), vec![Code::HS0016]);
        assert_eq!(codes("2.0E-"), vec![Code::HS0016]);
        assert_eq!(codes("12abc"), vec![Code::HS0013]);
        assert_eq!(codes("0x10"), vec![Code::HS0013]);
        assert_eq!(codes("1_"), vec![Code::HS0013]);
        assert_eq!(codes("1__0"), vec![Code::HS0013]);
        for text in ["2.", ".5", "1e", "12abc", "1_"] {
            let source = Source::new(SourceId(0), "main.hws", text).unwrap();
            let out = lex(&source, &mut Control::none());
            assert!(out.tokens[0].malformed, "{text}");
        }
    }

    #[test]
    fn dot_digit_is_one_malformed_float_not_dot_then_int() {
        assert_eq!(
            kinds(".5"),
            vec![TokenKind::Float, TokenKind::Term, TokenKind::Eof]
        );
    }

    #[test]
    fn int_above_2_63_is_malformed() {
        assert_eq!(codes("9223372036854775808"), vec![]);
        assert_eq!(codes("9223372036854775809"), vec![Code::HS0014]);
        assert_eq!(
            codes("99999999999999999999999999999999999999999999999999999999999999"),
            vec![Code::HS0014]
        );
    }

    #[test]
    fn float_that_rounds_to_infinity_is_malformed() {
        assert_eq!(codes("1e400"), vec![Code::HS0015]);
        assert_eq!(codes("1e-400"), vec![]);
    }

    #[test]
    fn plain_strings() {
        assert_eq!(
            kinds(r#""hello""#),
            vec![TokenKind::String, TokenKind::Term, TokenKind::Eof]
        );
        assert_eq!(codes(r#""hello""#), vec![]);
    }

    #[test]
    fn unterminated_string_by_newline_or_eof() {
        assert_eq!(codes("\"abc"), vec![Code::HS0006]);
        assert_eq!(codes("\"abc\nreturn 1"), vec![Code::HS0006]);
        let source = Source::new(SourceId(0), "main.hws", "\"abc").unwrap();
        let out = lex(&source, &mut Control::none());
        assert!(out.tokens[0].malformed);
        assert_eq!(out.tokens[0].kind, TokenKind::String);
    }

    #[test]
    fn a_lone_cr_inside_a_string_is_one_unrecognised_character_not_an_unterminated_string() {
        // Section 1.1: a CR not immediately followed by LF is an
        // unrecognised character wherever it occurs, including inside a
        // string; it is not itself a line terminator, so it must not end the
        // literal the way a genuine one does.
        assert_eq!(codes("\"a\rb\""), vec![Code::HS0005]);
        let toks = kinds("\"a\rb\"");
        assert_eq!(
            toks,
            vec![TokenKind::String, TokenKind::Term, TokenKind::Eof]
        );
        let source = Source::new(SourceId(0), "main.hws", "\"a\rb\"").unwrap();
        let out = lex(&source, &mut Control::none());
        assert!(!out.tokens[0].malformed, "the string closes cleanly");
        assert_eq!((out.tokens[0].span.start, out.tokens[0].span.end), (0, 5));
    }

    #[test]
    fn a_genuine_crlf_still_ends_a_string_abnormally() {
        assert_eq!(codes("\"a\r\nb\""), vec![Code::HS0006, Code::HS0006]);
    }

    #[test]
    fn string_escapes() {
        assert_eq!(codes(r#""a\"b\\c\n\t\r\$""#), vec![]);
        assert_eq!(codes("\"\\u{1F600}\""), vec![]);
        assert_eq!(codes("\"\\q\""), vec![Code::HS0008]);
        assert_eq!(codes("\"\\u{D800}\""), vec![Code::HS0009]);
        assert_eq!(codes("\"\\u{110000}\""), vec![Code::HS0009]);
        assert_eq!(codes("\"\\u{}\""), vec![Code::HS0009]);
        assert_eq!(codes("\"\\u{1234567}\""), vec![Code::HS0009]);
        assert_eq!(codes("\"end\\"), vec![Code::HS0006]);
    }

    #[test]
    fn simple_interpolation_splits_into_head_and_tail() {
        let toks = kinds(r#""a ${1} b""#);
        assert_eq!(
            toks,
            vec![
                TokenKind::StringHead,
                TokenKind::Int,
                TokenKind::StringTail,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn interpolation_with_two_parts_uses_middle() {
        let toks = kinds(r#""${a}-${b}""#);
        assert_eq!(
            toks,
            vec![
                TokenKind::StringHead,
                TokenKind::Ident,
                TokenKind::StringMiddle,
                TokenKind::Ident,
                TokenKind::StringTail,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn record_literal_braces_inside_interpolation_do_not_close_it() {
        let toks = kinds(r#""${ { x: 1 } }""#);
        assert_eq!(
            toks,
            vec![
                TokenKind::StringHead,
                TokenKind::LBrace,
                TokenKind::Ident,
                TokenKind::Colon,
                TokenKind::Int,
                TokenKind::RBrace,
                TokenKind::StringTail,
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn nested_string_inside_interpolation() {
        let toks = kinds(r#""a${"b${1}c"}d""#);
        assert_eq!(
            toks,
            vec![
                TokenKind::StringHead, // "a${
                TokenKind::StringHead, // "b${
                TokenKind::Int,        // 1
                TokenKind::StringTail, // }c"
                TokenKind::StringTail, // }d"
                TokenKind::Term,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn unterminated_interpolation_is_hs0007() {
        assert_eq!(codes("\"a${1\nreturn 1"), vec![Code::HS0007]);
        assert_eq!(codes("\"a${1"), vec![Code::HS0007]);
    }

    #[test]
    fn a_line_terminator_nested_in_brackets_inside_an_interpolation_is_not_fatal() {
        // A call whose arguments are spread over several lines is ordinary
        // code once inside `${ }`, exactly as it would be outside a string:
        // only a line terminator with nothing open inside the interpolation
        // ends it abnormally. The lexer still inserts `TERM` by its normal,
        // context-free rule after the `2` (an `Int` ends a statement); the
        // parser is what discards a `TERM` inside an argument list (section
        // 1.8), the same as it does outside any string.
        let text = "\"${f(\n    1,\n    2\n)}\"";
        assert_eq!(codes(text), Vec::<Code>::new());
        assert_eq!(
            kinds(text),
            vec![
                TokenKind::StringHead,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::Int,
                TokenKind::Comma,
                TokenKind::Int,
                TokenKind::Term,
                TokenKind::RParen,
                TokenKind::StringTail,
                TokenKind::Term,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn a_line_terminator_with_nothing_open_inside_an_interpolation_is_still_fatal() {
        // Once `f(1)` has closed, the interpolation is back to bracket depth
        // zero, so the following line terminator abandons it (HS0007). What
        // is left on the rest of the line (here `}"`, the punctuation that
        // would have closed the interpolation and the string) is relexed as
        // ordinary text from a clean slate, exactly as section 1.9 describes
        // for any other malformed token; here that happens to include a
        // second, unrelated diagnostic (an unterminated string opened by the
        // stray closing quote), which is why this asserts on the first
        // diagnostic rather than the whole list.
        let out = codes("\"${f(1)\n}\"");
        assert_eq!(out.first(), Some(&Code::HS0007));
    }

    #[test]
    fn deep_interpolation_nesting_reports_hs0115_and_does_not_panic() {
        let mut src = String::new();
        for _ in 0..150 {
            src.push_str("\"${");
        }
        src.push('1');
        for _ in 0..150 {
            src.push_str("}\"");
        }
        let source = Source::new(SourceId(0), "main.hws", src.as_str()).unwrap();
        let out = lex(&source, &mut Control::none());
        // Exactly one diagnostic for the one violation (section 1.10: "each
        // violation is one diagnostic"), not one per `${` past the limit.
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|d| d.code == Code::HS0115)
                .count(),
            1
        );
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.tokens.last().map(|t| t.kind), Some(TokenKind::Eof));
    }

    #[test]
    fn two_separate_over_limit_excursions_are_two_diagnostics() {
        // Once nesting unwinds back below the limit, a later, unrelated
        // excursion past it is a fresh violation and gets its own HS0115.
        let excursion = || {
            let mut s = String::new();
            for _ in 0..105 {
                s.push_str("\"${");
            }
            s.push('1');
            for _ in 0..105 {
                s.push_str("}\"");
            }
            s
        };
        let src = format!(
            "constant a = {}\nconstant b = {}\n",
            excursion(),
            excursion()
        );
        let source = Source::new(SourceId(0), "main.hws", src.as_str()).unwrap();
        let out = lex(&source, &mut Control::none());
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|d| d.code == Code::HS0115)
                .count(),
            2
        );
    }

    #[test]
    fn filters() {
        assert_eq!(
            kinds("`status:open`"),
            vec![TokenKind::Filter, TokenKind::Term, TokenKind::Eof]
        );
        assert_eq!(codes(r"`a\`b\\c`"), vec![]);
        assert_eq!(codes("`unterminated"), vec![Code::HS0010]);
        assert_eq!(codes("`unterminated\nx"), vec![Code::HS0010]);
    }

    #[test]
    fn a_lone_cr_inside_a_filter_is_one_unrecognised_character_not_an_unterminated_filter() {
        assert_eq!(codes("`a\rb`"), vec![Code::HS0005]);
        let toks = kinds("`a\rb`");
        assert_eq!(
            toks,
            vec![TokenKind::Filter, TokenKind::Term, TokenKind::Eof]
        );
        let source = Source::new(SourceId(0), "main.hws", "`a\rb`").unwrap();
        let out = lex(&source, &mut Control::none());
        assert!(!out.tokens[0].malformed, "the filter closes cleanly");
    }

    #[test]
    fn byte_order_mark_is_skipped_once() {
        let source = Source::new(SourceId(0), "main.hws", "\u{feff}return 1").unwrap();
        let out = lex(&source, &mut Control::none());
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.tokens[0].kind, TokenKind::Keyword(Keyword::Return));
    }

    #[test]
    fn a_second_byte_order_mark_is_unrecognised() {
        assert_eq!(codes("a\u{feff}b"), vec![Code::HS0005]);
    }

    #[test]
    fn bidi_controls_are_rejected_everywhere() {
        assert_eq!(codes("a\u{202e}b"), vec![Code::HS0005]);
        assert_eq!(codes("\"\u{202e}\""), vec![Code::HS0005]);
    }

    #[test]
    fn else_branch_kept_intact_by_terminator_rule() {
        let toks = kinds("if a {\n1\n} else {\n2\n}");
        // No TERM directly before `else`, since `}` on the same line as `else` does not insert one before it.
        let idx = toks
            .iter()
            .position(|k| *k == TokenKind::Keyword(Keyword::Else))
            .unwrap();
        assert_eq!(toks[idx - 1], TokenKind::RBrace);
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        // A broad scan of single and paired bytes/characters that previously
        // tripped up ad hoc lexers: unmatched surrogate-adjacent sequences
        // are impossible in a `&str`, but we still hammer on backslashes,
        // quotes, braces and control characters in isolation.
        let fragments = [
            "\\", "\"", "`", "{", "}", "$", "..", "...", "-", "_", ".", "0x", "\u{0}", "\u{7f}",
            "\r", "\u{2028}",
        ];
        for a in fragments {
            for b in fragments {
                let text = format!("{a}{b}");
                let source = Source::new(SourceId(0), "main.hws", text.as_str()).unwrap();
                let out = lex(&source, &mut Control::none());
                assert_eq!(
                    out.tokens.last().map(|t| t.kind),
                    Some(TokenKind::Eof),
                    "{text:?}"
                );
            }
        }
    }

    #[test]
    fn many_diagnostics_on_one_long_line_lex_in_near_linear_time() {
        // Section 19.1: never hangs on any input within the limits of
        // section 1.10. A source that is one long line of rejected control
        // characters is within every structural limit (well under the
        // 4,194,304-byte source size cap) and lexes to one HS0005 per
        // character; locating each diagnostic from the start of its line
        // (as `LineIndex::position` alone does) would make this quadratic in
        // the character count. A generous budget here, not a tight one: the
        // point is ruling out quadratic behaviour, which is tens of seconds
        // slower than this even at a fraction of this size, not pinning down
        // an exact time.
        let text = "\u{1}".repeat(200_000);
        let source = Source::new(SourceId(0), "main.hws", text.as_str()).unwrap();
        let started = std::time::Instant::now();
        let out = lex(&source, &mut Control::none());
        let elapsed = started.elapsed();
        // The cap of section 17.1 keeps the first 100 and the one after,
        // however many were found; every one of them is HS0005.
        assert_eq!(out.diagnostics.len(), 101);
        assert!(
            out.diagnostics.iter().all(|d| d.code == Code::HS0005),
            "every diagnostic should be HS0005"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "lexing 200,000 single-character diagnostics on one line took {elapsed:?}"
        );
    }
}
