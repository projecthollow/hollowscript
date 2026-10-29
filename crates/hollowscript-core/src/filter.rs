//! Filter expressions (Appendix A): the data types and the four operations.
//!
//! A filter literal is checked when a program is checked and is opaque
//! afterwards. A host that receives one evaluates it against its own records,
//! so the engine lives here, where both the checker and a VM without a front
//! end can reach it. The operations are `parse`, `validate`, `matches` and
//! `format` (section A.1).
//!
//! The four operations are the only public surface. Everything below them
//! (the lexer, the recursive descent parser, value classification, calendar
//! arithmetic and the canonical printer) is private: a host never sees more
//! of the grammar than the tree of [`Node`] gives it.
//!
//! **Bounding the work.** A filter source is capped at
//! [`MAX_FILTER_BYTES`] bytes and its groups and negations at
//! [`MAX_FILTER_NESTING`] deep (HS0704, HS0703); the parser checks the
//! nesting limit before it
//! recurses one level further, so it cannot be driven to exhaust its own
//! stack, and every tree it can build is bounded in both size and depth by
//! those two limits. `validate`, `matches` and `format` each do one pass
//! over a tree no bigger than that, so none of the four operations can be
//! made to do unbounded work by a filter literal, independently of anything
//! a host's schema, adapter or record supplies.

use crate::diagnostic::{Code, Diagnostic};
use crate::limits::structural::{MAX_FILTER_BYTES, MAX_FILTER_NESTING, MAX_RELATIVE_DATE_DAYS};
use crate::source::Span;
use std::collections::BTreeMap;
use std::fmt;

/// A calendar date in the proleptic Gregorian calendar.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    /// The year, from 1 to 9999.
    pub year: i32,
    /// The month, from 1 to 12.
    pub month: u8,
    /// The day of the month.
    pub day: u8,
}

/// A number in a filter: an `Int` if it has no fraction and fits, else a `Float`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Number {
    /// An integer.
    Int(i64),
    /// The nearest binary64 value.
    Float(f64),
}

/// How a value was classified, before any schema (section A.4).
#[derive(Clone, Debug, PartialEq)]
pub enum ValueClass {
    /// The reserved value `none`.
    None,
    /// The reserved value `any`.
    Any,
    /// `today`, `tomorrow`, `yesterday` and `today+N`, as a number of days from today.
    Relative(i32),
    /// `YYYY-MM-DD..YYYY-MM-DD`, inclusive at both ends.
    DateRange(Date, Date),
    /// `YYYY-MM-DD`.
    Date(Date),
    /// A number.
    Number(Number),
    /// Anything else, and every quoted value.
    Text,
}

/// The value of a predicate. It keeps its original text, which is what an
/// `enum`, `text` or `textMulti` field compares.
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    /// The text as written, without quotes.
    pub text: String,
    /// Whether it was written in quotes.
    pub quoted: bool,
    /// How it was classified.
    pub class: ValueClass,
}

/// The operator of a predicate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// `:`.
    Colon,
    /// `=`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
}

impl Op {
    /// Its canonical text (section A.9), also used in diagnostic messages.
    fn symbol(self) -> &'static str {
        match self {
            Op::Colon => ":",
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Gt => ">",
            Op::Le => "<=",
            Op::Ge => ">=",
        }
    }
}

/// A node of a parsed filter (section A.5).
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Every child holds. Two or more children.
    And(Vec<Node>),
    /// Some child holds. Two or more children.
    Or(Vec<Node>),
    /// The child does not hold.
    Not(Box<Node>),
    /// `field op value`, with the field in lower case.
    Predicate {
        /// The field name.
        field: String,
        /// The operator.
        op: Op,
        /// The value.
        value: Value,
        /// The span of the field and of the value in the filter source. The
        /// operator's own span is between them: field and operator, and
        /// operator and value, are always adjacent (section A.3), so it is
        /// `field_span.end .. value_span.start` and is never stored twice.
        spans: (Span, Span),
    },
    /// A bare word or a quoted term, searched in free text.
    Text {
        /// The text.
        text: String,
        /// Whether it was quoted, so that it is never a boolean field name.
        quoted: bool,
    },
}

/// A parsed filter.
#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    /// The root node.
    pub root: Node,
}

/// The type of a field of a host's records (section A.6).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// One of a closed set of strings, ordered by rank when `values` is given.
    Enum,
    /// A free string.
    Text,
    /// A list of strings.
    TextMulti,
    /// A calendar date.
    Date,
    /// An `Int` or a `Float`.
    Number,
    /// `true` or `false`.
    Boolean,
}

impl FieldType {
    /// Its name, as used in the `{type}` of HS0709.
    fn name(self) -> &'static str {
        match self {
            FieldType::Enum => "enum",
            FieldType::Text => "text",
            FieldType::TextMulti => "textMulti",
            FieldType::Date => "date",
            FieldType::Number => "number",
            FieldType::Boolean => "boolean",
        }
    }
}

/// The description of one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDesc {
    /// The type.
    pub ty: FieldType,
    /// Aliases from lower case input text to canonical text.
    pub aliases: BTreeMap<String, String>,
    /// For an enum, the canonical texts in rank order, lowest first.
    pub values: Option<Vec<String>>,
}

/// A host's description of its records. Field names are lower case, and two
/// names may share a description.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterSchema {
    /// The fields by name.
    pub fields: BTreeMap<String, FieldDesc>,
}

/// A value that an adapter resolves for a field of a record.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    /// The record has no value.
    Absent,
    /// A string.
    Text(String),
    /// An `Int`.
    Int(i64),
    /// A `Float`.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A list of strings.
    List(Vec<String>),
}

/// The three operations that a host provides so that filters can be evaluated
/// against its records.
pub trait Adapter {
    /// The value of the named field of the record.
    fn resolve(&self, field: &str) -> Resolved;
    /// The strings that a bare term is searched in.
    fn free_text(&self) -> Vec<String>;
    /// Today's date as `YYYY-MM-DD`, in the time zone the host wants.
    fn today(&self) -> String;
}

/// A problem in a filter: a diagnostic code and a span in the filter source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterError {
    /// The code, from HS0701 to HS0711.
    pub code: Code,
    /// The span in the filter source, after unescaping.
    pub span: Span,
    /// The message.
    pub message: String,
}

impl FilterError {
    /// The error that stands in for an operation that is not written yet.
    #[must_use]
    pub fn not_implemented(what: &str) -> FilterError {
        FilterError {
            code: Code::HS1099,
            span: Span::empty(0),
            message: Diagnostic::not_implemented(what).message,
        }
    }

    /// Builds an error from a code, a span and the template arguments of
    /// [`crate::codes`]'s registry, so every message here is rendered from
    /// the one place chapter 17's templates live.
    fn new(code: Code, span: Span, args: &[(&str, &dyn fmt::Display)]) -> FilterError {
        FilterError {
            code,
            span,
            message: Diagnostic::new(code, args).message,
        }
    }
}

// ---------------------------------------------------------------------------
// A.2 Lexical structure
// ---------------------------------------------------------------------------

/// A lexical token of a filter source (section A.2). `WORD` and `QUOTED` keep
/// their text; keyword-ness of a `Word` is decided by the parser, wherever a
/// `WORD` may appear, following section A.2's own rule.
#[derive(Clone, Debug, PartialEq)]
enum TokKind {
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// A quoted string, its content (without the quotes).
    Quoted(String),
    /// One of the seven operators.
    Op(Op),
    /// A `-` that negates the term that follows it.
    Minus,
    /// A maximal run of code points that is neither punctuation nor an operator.
    Word(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Tok {
    kind: TokKind,
    span: Span,
}

/// One of the three keywords (section A.2), recognised case-insensitively
/// wherever a `WORD` may appear.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Keyword {
    Or,
    And,
    Not,
}

fn keyword_of(word: &str) -> Option<Keyword> {
    if word.eq_ignore_ascii_case("or") {
        Some(Keyword::Or)
    } else if word.eq_ignore_ascii_case("and") {
        Some(Keyword::And)
    } else if word.eq_ignore_ascii_case("not") {
        Some(Keyword::Not)
    } else {
        None
    }
}

/// Whether `c` excludes itself from a `WORD` unconditionally: white space and
/// the six characters that start punctuation or an operator. `!` is handled
/// separately, since it only excludes itself when directly followed by `=`.
fn is_word_stop(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '"' | ':' | '=' | '<' | '>')
}

struct Lexer<'a> {
    src: &'a str,
    pos: u32,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Lexer<'a> {
        Lexer { src, pos: 0 }
    }

    /// The code point `skip` characters after the current position, without
    /// consuming anything.
    fn char_at(&self, skip: usize) -> Option<char> {
        self.src.get(self.pos as usize..)?.chars().nth(skip)
    }

    fn advance_char(&mut self) -> Option<char> {
        let c = self.char_at(0)?;
        self.pos += u32::try_from(c.len_utf8()).unwrap_or(1);
        Some(c)
    }

    /// A maximal `WORD` run starting at the current position, per section
    /// A.2: no white space, none of `( ) " : = < >`, and no `!` directly
    /// followed by `=`. The caller has already established that the current
    /// character belongs in a `WORD`.
    fn scan_word(&mut self) -> (String, Span) {
        let start = self.pos;
        loop {
            match self.char_at(0) {
                None => break,
                Some(c) if is_word_stop(c) => break,
                Some('!') if self.char_at(1) == Some('=') => break,
                Some(_) => {
                    self.advance_char();
                }
            }
        }
        let text = self.src.get(start as usize..self.pos as usize);
        (text.unwrap_or("").to_owned(), Span::new(start, self.pos))
    }
}

/// Tokenises `source` (section A.2). Stops at the first lexical error: an
/// unterminated quote (HS0702) or a `-` that starts a token but has no term
/// to negate (HS0701).
fn lex(source: &str) -> Result<Vec<Tok>, FilterError> {
    let mut lx = Lexer::new(source);
    let mut toks = Vec::new();
    // Whether the previous token was an operator, which is all the `MINUS`
    // rule of section A.2 needs to know about it.
    let mut prev_was_op = false;
    loop {
        while let Some(c) = lx.char_at(0) {
            if c.is_whitespace() {
                lx.advance_char();
            } else {
                break;
            }
        }
        let Some(c) = lx.char_at(0) else { break };
        let start = lx.pos;
        match c {
            '(' => {
                lx.advance_char();
                toks.push(Tok {
                    kind: TokKind::LParen,
                    span: Span::new(start, lx.pos),
                });
                prev_was_op = false;
            }
            ')' => {
                lx.advance_char();
                toks.push(Tok {
                    kind: TokKind::RParen,
                    span: Span::new(start, lx.pos),
                });
                prev_was_op = false;
            }
            '"' => {
                lx.advance_char();
                let content_start = lx.pos;
                loop {
                    match lx.char_at(0) {
                        None => {
                            return Err(FilterError::new(
                                Code::HS0702,
                                Span::new(start, lx.pos),
                                &[],
                            ));
                        }
                        Some('"') => break,
                        Some(_) => {
                            lx.advance_char();
                        }
                    }
                }
                let content = lx
                    .src
                    .get(content_start as usize..lx.pos as usize)
                    .unwrap_or("")
                    .to_owned();
                lx.advance_char(); // the closing quote
                toks.push(Tok {
                    kind: TokKind::Quoted(content),
                    span: Span::new(start, lx.pos),
                });
                prev_was_op = false;
            }
            '!' => {
                if lx.char_at(1) == Some('=') {
                    lx.advance_char();
                    lx.advance_char();
                    toks.push(Tok {
                        kind: TokKind::Op(Op::Ne),
                        span: Span::new(start, lx.pos),
                    });
                    prev_was_op = true;
                } else {
                    let (text, span) = lx.scan_word();
                    toks.push(Tok {
                        kind: TokKind::Word(text),
                        span,
                    });
                    prev_was_op = false;
                }
            }
            '<' | '>' => {
                let two = lx.char_at(1) == Some('=');
                lx.advance_char();
                if two {
                    lx.advance_char();
                }
                let op = match (c, two) {
                    ('<', true) => Op::Le,
                    ('<', false) => Op::Lt,
                    (_, true) => Op::Ge,
                    (_, false) => Op::Gt,
                };
                toks.push(Tok {
                    kind: TokKind::Op(op),
                    span: Span::new(start, lx.pos),
                });
                prev_was_op = true;
            }
            '=' => {
                lx.advance_char();
                toks.push(Tok {
                    kind: TokKind::Op(Op::Eq),
                    span: Span::new(start, lx.pos),
                });
                prev_was_op = true;
            }
            ':' => {
                lx.advance_char();
                toks.push(Tok {
                    kind: TokKind::Op(Op::Colon),
                    span: Span::new(start, lx.pos),
                });
                prev_was_op = true;
            }
            '-' => {
                if prev_was_op {
                    // "The first character of a WORD, whatever follows it."
                    let (text, span) = lx.scan_word();
                    toks.push(Tok {
                        kind: TokKind::Word(text),
                        span,
                    });
                    prev_was_op = false;
                } else {
                    let next = lx.char_at(1);
                    if next.is_none_or(|c| c.is_whitespace() || c == ')') {
                        return Err(FilterError::new(
                            Code::HS0701,
                            Span::new(start, start + 1),
                            &[("detail", &"a negation needs a term")],
                        ));
                    }
                    lx.advance_char();
                    toks.push(Tok {
                        kind: TokKind::Minus,
                        span: Span::new(start, lx.pos),
                    });
                    prev_was_op = false;
                }
            }
            _ => {
                let (text, span) = lx.scan_word();
                toks.push(Tok {
                    kind: TokKind::Word(text),
                    span,
                });
                prev_was_op = false;
            }
        }
    }
    Ok(toks)
}

// ---------------------------------------------------------------------------
// A.4 Values
// ---------------------------------------------------------------------------

fn is_leap_year(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i32, m: u8) -> u8 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_valid_ymd(y: i32, m: u8, d: u8) -> bool {
    (1..=9999).contains(&y) && (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)
}

/// The shape `DDDD-DD-DD` (section A.4, rule 6), without checking that it is
/// a valid date: the year, month and day as written.
fn match_date_shape(word: &str) -> Option<(i32, u8, u8)> {
    let bytes = word.as_bytes();
    if bytes.len() != 10 {
        return None;
    }
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    for i in [0usize, 1, 2, 3, 5, 6, 8, 9] {
        if !bytes.get(i).is_some_and(u8::is_ascii_digit) {
            return None;
        }
    }
    let y: i32 = word.get(0..4)?.parse().ok()?;
    let m: u8 = word.get(5..7)?.parse().ok()?;
    let d: u8 = word.get(8..10)?.parse().ok()?;
    Some((y, m, d))
}

/// The shape `DDDD-DD-DD..DDDD-DD-DD` (rule 5), as the text of each half.
fn match_date_range_shape(word: &str) -> Option<(&str, &str)> {
    let bytes = word.as_bytes();
    if bytes.len() != 22 {
        return None;
    }
    if bytes.get(10..12) != Some(b"..") {
        return None;
    }
    let left = word.get(0..10)?;
    let right = word.get(12..22)?;
    if match_date_shape(left).is_none() || match_date_shape(right).is_none() {
        return None;
    }
    Some((left, right))
}

/// The shape of rule 7: an optional `-`, one or more digits, and optionally
/// `.` and one or more digits. No exponent, no `+`, no hexadecimal.
fn match_number_shape(word: &str) -> bool {
    let mut chars = word.chars().peekable();
    if chars.peek() == Some(&'-') {
        chars.next();
    }
    let mut int_digits = 0;
    while chars.peek().is_some_and(char::is_ascii_digit) {
        chars.next();
        int_digits += 1;
    }
    if int_digits == 0 {
        return false;
    }
    if chars.peek() == Some(&'.') {
        chars.next();
        let mut frac_digits = 0;
        while chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            frac_digits += 1;
        }
        if frac_digits == 0 {
            return false;
        }
    }
    chars.next().is_none()
}

/// The shape `today[+-]N` (rule 4), for `format`'s use, where validity does
/// not matter: only whether writing the text bare would be read back as a
/// relative date at all.
fn relative_offset_shape(word: &str) -> bool {
    let Some(prefix) = word.get(0..5) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case("today") {
        return false;
    }
    let rest = word.get(5..).unwrap_or("");
    let mut chars = rest.chars();
    match chars.next() {
        Some('+' | '-') => {}
        _ => return false,
    }
    let digits = chars.as_str();
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Parses `today+N` or `today-N` (rule 4) and checks the offset against the
/// limit, given the text already matches [`relative_offset_shape`].
fn relative_offset_value(word: &str, span: Span) -> Result<i32, FilterError> {
    let rest = word.get(5..).unwrap_or("");
    let negative = rest.starts_with('-');
    let digits = rest.get(1..).unwrap_or("");
    let magnitude: Option<u32> = digits.parse().ok();
    match magnitude {
        Some(n) if n <= MAX_RELATIVE_DATE_DAYS => {
            let n = i32::try_from(n).unwrap_or(i32::MAX);
            Ok(if negative { -n } else { n })
        }
        _ => Err(FilterError::new(
            Code::HS0706,
            span,
            &[("limit", &MAX_RELATIVE_DATE_DAYS)],
        )),
    }
}

fn date_from_shape(y: i32, m: u8, d: u8, text: &str, span: Span) -> Result<Date, FilterError> {
    if is_valid_ymd(y, m, d) {
        Ok(Date {
            year: y,
            month: m,
            day: d,
        })
    } else {
        Err(FilterError::new(Code::HS0705, span, &[("text", &text)]))
    }
}

fn classify_number(word: &str) -> ValueClass {
    if !word.contains('.') {
        if let Ok(i) = word.parse::<i64>() {
            return ValueClass::Number(Number::Int(i));
        }
    }
    match word.parse::<f64>() {
        Ok(f) if f.is_finite() => {
            // "a negative zero is 0.0": overwrite rather than test the sign,
            // which folds -0.0 into 0.0 without a branch on it.
            let f = if f == 0.0 { 0.0 } else { f };
            ValueClass::Number(Number::Float(f))
        }
        _ => ValueClass::Text,
    }
}

/// Classifies the `WORD` value of a predicate (section A.4). A `QUOTED`
/// value never reaches this function: it is always [`ValueClass::Text`].
fn classify_value_word(word: &str, span: Span) -> Result<ValueClass, FilterError> {
    if word.eq_ignore_ascii_case("none") {
        return Ok(ValueClass::None);
    }
    if word.eq_ignore_ascii_case("any") {
        return Ok(ValueClass::Any);
    }
    if word.eq_ignore_ascii_case("today") {
        return Ok(ValueClass::Relative(0));
    }
    if word.eq_ignore_ascii_case("tomorrow") {
        return Ok(ValueClass::Relative(1));
    }
    if word.eq_ignore_ascii_case("yesterday") {
        return Ok(ValueClass::Relative(-1));
    }
    if relative_offset_shape(word) {
        return Ok(ValueClass::Relative(relative_offset_value(word, span)?));
    }
    if let Some((left, right)) = match_date_range_shape(word) {
        // match_date_range_shape already checked the shape of both halves.
        let (ly, lm, ld) = match_date_shape(left).unwrap_or((0, 0, 0));
        let (ry, rm, rd) = match_date_shape(right).unwrap_or((0, 0, 0));
        let from = date_from_shape(ly, lm, ld, left, span)?;
        let to = date_from_shape(ry, rm, rd, right, span)?;
        if from > to {
            return Err(FilterError::new(
                Code::HS0711,
                span,
                &[("from", &left), ("to", &right)],
            ));
        }
        return Ok(ValueClass::DateRange(from, to));
    }
    if let Some((y, m, d)) = match_date_shape(word) {
        return Ok(ValueClass::Date(date_from_shape(y, m, d, word, span)?));
    }
    if match_number_shape(word) {
        return Ok(classify_number(word));
    }
    Ok(ValueClass::Text)
}

// ---------------------------------------------------------------------------
// A.3 Grammar
// ---------------------------------------------------------------------------

/// A generic HS0701, built from what was expected and where.
fn expected(what: &str, span: Span) -> FilterError {
    FilterError::new(
        Code::HS0701,
        span,
        &[("detail", &format_args!("{what} is expected"))],
    )
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    end: u32,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_keyword(&self) -> Option<Keyword> {
        match &self.peek()?.kind {
            TokKind::Word(w) => keyword_of(w),
            _ => None,
        }
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// The span to report an error at when a token was expected and the
    /// source ended (section A.3): the end of the source.
    fn end_span(&self) -> Span {
        Span::new(self.end, self.end)
    }

    fn parse_or(&mut self, depth: u32) -> Result<Node, FilterError> {
        let first = self.parse_and(depth)?;
        let mut rest = Vec::new();
        while self.peek_keyword() == Some(Keyword::Or) {
            self.bump();
            rest.push(self.parse_and(depth)?);
        }
        Ok(if rest.is_empty() {
            first
        } else {
            let mut children = Vec::with_capacity(rest.len() + 1);
            children.push(first);
            children.extend(rest);
            Node::Or(children)
        })
    }

    /// Whether the current token can start a `notExpr`: `LPAREN`, `MINUS`,
    /// `QUOTED`, the keyword `NOT`, or a `WORD` that is not `OR` or `AND`.
    fn starts_not_expr(&self) -> bool {
        match self.peek() {
            None => false,
            Some(t) => match &t.kind {
                TokKind::LParen | TokKind::Minus | TokKind::Quoted(_) => true,
                TokKind::Word(w) => !matches!(keyword_of(w), Some(Keyword::Or | Keyword::And)),
                TokKind::RParen | TokKind::Op(_) => false,
            },
        }
    }

    fn parse_and(&mut self, depth: u32) -> Result<Node, FilterError> {
        let first = self.parse_not(depth)?;
        let mut rest = Vec::new();
        loop {
            match self.peek_keyword() {
                Some(Keyword::Or) => break,
                Some(Keyword::And) => {
                    self.bump();
                    rest.push(self.parse_not(depth)?);
                }
                Some(Keyword::Not) => rest.push(self.parse_not(depth)?),
                None => {
                    if self.starts_not_expr() {
                        rest.push(self.parse_not(depth)?);
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(if rest.is_empty() {
            first
        } else {
            let mut children = Vec::with_capacity(rest.len() + 1);
            children.push(first);
            children.extend(rest);
            Node::And(children)
        })
    }

    fn parse_not(&mut self, depth: u32) -> Result<Node, FilterError> {
        let negates = match self.peek() {
            Some(t) => {
                matches!(t.kind, TokKind::Minus) || self.peek_keyword() == Some(Keyword::Not)
            }
            None => false,
        };
        if negates {
            let Some(tok) = self.peek() else {
                return self.parse_primary(depth);
            };
            let new_depth = depth + 1;
            if new_depth > MAX_FILTER_NESTING {
                return Err(FilterError::new(
                    Code::HS0703,
                    tok.span,
                    &[("limit", &MAX_FILTER_NESTING)],
                ));
            }
            self.bump();
            let child = self.parse_not(new_depth)?;
            Ok(Node::Not(Box::new(child)))
        } else {
            self.parse_primary(depth)
        }
    }

    fn parse_primary(&mut self, depth: u32) -> Result<Node, FilterError> {
        let Some(tok) = self.peek().cloned() else {
            return Err(expected("a term", self.end_span()));
        };
        match tok.kind {
            TokKind::LParen => {
                let new_depth = depth + 1;
                if new_depth > MAX_FILTER_NESTING {
                    return Err(FilterError::new(
                        Code::HS0703,
                        tok.span,
                        &[("limit", &MAX_FILTER_NESTING)],
                    ));
                }
                self.bump();
                let inner = self.parse_or(new_depth)?;
                match self.peek().cloned() {
                    Some(t) if matches!(t.kind, TokKind::RParen) => {
                        self.bump();
                        Ok(inner)
                    }
                    Some(t) => Err(expected("')'", t.span)),
                    None => Err(expected("')'", self.end_span())),
                }
            }
            TokKind::RParen | TokKind::Op(_) | TokKind::Minus => Err(expected("a term", tok.span)),
            TokKind::Quoted(text) => {
                self.bump();
                if text.is_empty() {
                    return Err(expected("a non-empty quoted term", tok.span));
                }
                Ok(Node::Text { text, quoted: true })
            }
            TokKind::Word(word) => {
                if keyword_of(&word).is_some() {
                    return Err(expected("a term", tok.span));
                }
                let adjacent_op = self
                    .toks
                    .get(self.pos + 1)
                    .filter(|next| matches!(next.kind, TokKind::Op(_)))
                    .filter(|next| next.span.start == tok.span.end)
                    .cloned();
                let Some(op_tok) = adjacent_op else {
                    self.bump();
                    return Ok(Node::Text {
                        text: word,
                        quoted: false,
                    });
                };
                self.bump(); // the field word
                self.bump(); // the operator
                let TokKind::Op(op) = op_tok.kind else {
                    return Err(expected("a term", op_tok.span));
                };
                let value_tok = self.peek().cloned();
                let Some(vt) = value_tok
                    .clone()
                    .filter(|vt| vt.span.start == op_tok.span.end)
                else {
                    let span = value_tok.map_or(self.end_span(), |vt| vt.span);
                    return Err(expected("a value", span));
                };
                match vt.kind {
                    TokKind::Quoted(text) => {
                        self.bump();
                        let value = Value {
                            text,
                            quoted: true,
                            class: ValueClass::Text,
                        };
                        Ok(Node::Predicate {
                            field: word.to_lowercase(),
                            op,
                            value,
                            spans: (tok.span, vt.span),
                        })
                    }
                    TokKind::Word(text) => {
                        if keyword_of(&text).is_some() {
                            return Err(expected("a term", vt.span));
                        }
                        self.bump();
                        let class = classify_value_word(&text, vt.span)?;
                        let value = Value {
                            text,
                            quoted: false,
                            class,
                        };
                        Ok(Node::Predicate {
                            field: word.to_lowercase(),
                            op,
                            value,
                            spans: (tok.span, vt.span),
                        })
                    }
                    _ => Err(expected("a value", vt.span)),
                }
            }
        }
    }
}

/// Parses filter source. Parsing stops at the first error (section A.3).
///
/// # Errors
///
/// Returns the first syntax, classification or structural-limit error found.
pub fn parse(source: &str) -> Result<Filter, FilterError> {
    let byte_len = source.len();
    if byte_len > MAX_FILTER_BYTES {
        return Err(FilterError::new(
            Code::HS0704,
            Span::new(0, u32::try_from(byte_len).unwrap_or(u32::MAX)),
            &[("size", &byte_len), ("limit", &MAX_FILTER_BYTES)],
        ));
    }
    if source.trim().is_empty() {
        return Err(FilterError::new(
            Code::HS0710,
            Span::new(0, u32::try_from(byte_len).unwrap_or(u32::MAX)),
            &[],
        ));
    }
    let toks = lex(source)?;
    let end = u32::try_from(byte_len).unwrap_or(u32::MAX);
    let mut parser = Parser {
        toks: &toks,
        pos: 0,
        end,
    };
    let root = parser.parse_or(0)?;
    if parser.pos != parser.toks.len() {
        let span = parser
            .toks
            .get(parser.pos)
            .map_or(parser.end_span(), |t| t.span);
        return Err(expected("the end of the filter", span));
    }
    Ok(Filter { root })
}

// ---------------------------------------------------------------------------
// A.7 Validation against a schema
// ---------------------------------------------------------------------------

fn alias_lower(desc: &FieldDesc, text: &str) -> String {
    let key = text.to_lowercase();
    desc.aliases.get(&key).cloned().unwrap_or(key)
}

/// Whether `op` may be used with `value` against a field of `desc`'s type
/// (rule 2 of section A.7): the gate that both `validate` and `matches`
/// (its "Step 0") apply before a value is ever converted.
fn op_allowed(op: Op, desc: &FieldDesc, value: &Value) -> bool {
    if matches!(
        value.class,
        ValueClass::None | ValueClass::Any | ValueClass::DateRange(_, _)
    ) {
        return matches!(op, Op::Colon | Op::Eq | Op::Ne);
    }
    match desc.ty {
        FieldType::Text | FieldType::TextMulti | FieldType::Boolean => {
            matches!(op, Op::Colon | Op::Eq | Op::Ne)
        }
        FieldType::Number | FieldType::Date => true,
        FieldType::Enum => match op {
            Op::Colon | Op::Eq | Op::Ne => true,
            Op::Lt | Op::Gt | Op::Le | Op::Ge => desc.values.is_some(),
        },
    }
}

/// Whether `value` suits `desc`'s type (rule 4 of section A.7), for a value
/// that is neither *none* nor *any*.
fn value_suits(desc: &FieldDesc, value: &Value) -> bool {
    match desc.ty {
        FieldType::Enum => match &desc.values {
            Some(vals) => {
                let key = alias_lower(desc, &value.text);
                vals.iter().any(|v| v.to_lowercase() == key)
            }
            None => true,
        },
        FieldType::Text | FieldType::TextMulti => true,
        FieldType::Number => matches!(value.class, ValueClass::Number(_)),
        FieldType::Boolean => matches!(
            value.text.to_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "1" | "0"
        ),
        FieldType::Date => matches!(
            value.class,
            ValueClass::Date(_) | ValueClass::DateRange(_, _) | ValueClass::Relative(_)
        ),
    }
}

fn validate_node(node: &Node, schema: &FilterSchema, errors: &mut Vec<FilterError>) {
    match node {
        Node::And(children) | Node::Or(children) => {
            for child in children {
                validate_node(child, schema, errors);
            }
        }
        Node::Not(child) => validate_node(child, schema, errors),
        Node::Text { .. } => {}
        Node::Predicate {
            field,
            op,
            value,
            spans,
        } => {
            let Some(desc) = schema.fields.get(field) else {
                errors.push(FilterError::new(Code::HS0708, spans.0, &[("field", field)]));
                return;
            };
            let op_span = Span::new(spans.0.end, spans.1.start);
            if !op_allowed(*op, desc, value) {
                errors.push(FilterError::new(
                    Code::HS0707,
                    op_span,
                    &[
                        ("op", &op.symbol()),
                        ("what", &format_args!("a {} field", desc.ty.name())),
                    ],
                ));
                return;
            }
            if matches!(value.class, ValueClass::None | ValueClass::Any) {
                return;
            }
            if !value_suits(desc, value) {
                errors.push(FilterError::new(
                    Code::HS0709,
                    spans.1,
                    &[
                        ("value", &value.text),
                        ("type", &desc.ty.name()),
                        ("field", field),
                    ],
                ));
            }
        }
    }
}

/// Checks a parsed filter against a schema and returns every problem, one per
/// invalid predicate (section A.7).
#[must_use]
pub fn validate(filter: &Filter, schema: &FilterSchema) -> Vec<FilterError> {
    let mut errors = Vec::new();
    validate_node(&filter.root, schema, &mut errors);
    errors
}

// ---------------------------------------------------------------------------
// A.8 Evaluation
// ---------------------------------------------------------------------------

fn parse_date_str(s: &str) -> Option<Date> {
    let (y, m, d) = match_date_shape(s)?;
    if is_valid_ymd(y, m, d) {
        Some(Date {
            year: y,
            month: m,
            day: d,
        })
    } else {
        None
    }
}

/// Whether `resolved` is *empty* for a field of type `ty` (section A.8): a
/// resolved value whose kind does not match `ty` is treated as absent, which
/// is empty, exactly as [`crate::filter`]'s module documentation on the
/// mismatch rule of section A.6 describes.
fn resolved_is_empty(ty: FieldType, resolved: &Resolved) -> bool {
    match (ty, resolved) {
        (FieldType::Text | FieldType::Enum, Resolved::Text(s)) => s.trim().is_empty(),
        (FieldType::TextMulti, Resolved::List(v)) => v.is_empty(),
        (FieldType::Boolean, Resolved::Bool(_))
        | (FieldType::Number, Resolved::Int(_) | Resolved::Float(_)) => false,
        (FieldType::Date, Resolved::Text(s)) => parse_date_str(s).is_none(),
        // `Resolved::Absent`, and every other pairing, is a kind that does
        // not match the field's type, which section A.6's mismatch rule
        // treats as absent, hence empty.
        _ => true,
    }
}

/// The chronological order of an `Int`/`Float` pair, compared by exact
/// mathematical value (section 3.5), the same rule `==` and `<` use for a
/// mixed `Int`/`Float` comparison in the language itself.
fn cmp_int_float(i: i64, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    // 2^63: every `Int` is strictly inside [-2^63, 2^63), so a finite `f`
    // outside that range is unambiguously ordered against every `Int`.
    const BOUND: f64 = 9_223_372_036_854_775_808.0;
    if f.is_nan() {
        return None;
    }
    if f >= BOUND {
        return Some(Ordering::Less);
    }
    if f < -BOUND {
        return Some(Ordering::Greater);
    }
    let floor = f.floor();
    // `floor` is within [-2^63, 2^63) here, so the cast is exact, not
    // truncating: that range is checked above before this line runs.
    #[allow(clippy::cast_possible_truncation)]
    let floor_i128 = floor as i128;
    let i128v = i128::from(i);
    Some(match i128v.cmp(&floor_i128) {
        Ordering::Equal if f > floor => Ordering::Less,
        other => other,
    })
}

fn number_order(a: Number, b: Number) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Number::Int(x), Number::Int(y)) => Some(x.cmp(&y)),
        (Number::Float(x), Number::Float(y)) => {
            if x.is_nan() || y.is_nan() {
                None
            } else {
                x.partial_cmp(&y)
            }
        }
        (Number::Int(i), Number::Float(f)) => cmp_int_float(i, f),
        (Number::Float(f), Number::Int(i)) => cmp_int_float(i, f).map(Ordering::reverse),
    }
}

fn number_matches(resolved: Number, target: Number, op: Op) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let Some(order) = number_order(resolved, target) else {
        return false;
    };
    match op {
        Op::Colon | Op::Eq => order == Equal,
        Op::Ne => order != Equal,
        Op::Lt => order == Less,
        Op::Gt => order == Greater,
        Op::Le => order != Greater,
        Op::Ge => order != Less,
    }
}

enum DateTarget {
    Single(Date),
    Range(Date, Date),
}

/// Caches `adapter.today()`, parsed once and reused for every relative date
/// a `matches` call meets (section A.8: "reads `today()` at most once").
#[derive(Default)]
struct TodayCache {
    parsed: Option<Date>,
    read: bool,
}

impl TodayCache {
    fn get(&mut self, adapter: &dyn Adapter) -> Option<Date> {
        if !self.read {
            self.parsed = parse_date_str(&adapter.today());
            self.read = true;
        }
        self.parsed
    }
}

/// The target date of a `date`-field value (rule of section A.8): a literal
/// date or range as written, or `today()` plus the offset for a relative
/// date. `None` if the value is relative and `today()` is not a valid date.
fn date_target(
    value: &Value,
    adapter: &dyn Adapter,
    today_cache: &mut TodayCache,
) -> Option<DateTarget> {
    match &value.class {
        ValueClass::Date(d) => Some(DateTarget::Single(*d)),
        ValueClass::DateRange(from, to) => Some(DateTarget::Range(*from, *to)),
        ValueClass::Relative(offset) => {
            let today = today_cache.get(adapter);
            today.map(|t| DateTarget::Single(add_days(t, *offset)))
        }
        _ => None,
    }
}

/// `date` plus `days` (which the caller has already checked is at most
/// [`MAX_RELATIVE_DATE_DAYS`] in either direction), by the proleptic
/// Gregorian calendar, using the civil-from-days algorithm (Howard Hinnant's
/// public-domain `days_from_civil`/`civil_from_days`), which is exact for
/// every date this module can build (years 1 to 9999, shifted by at most
/// 5,000 days).
fn add_days(date: Date, days: i32) -> Date {
    let z = days_from_civil(
        i64::from(date.year),
        i64::from(date.month),
        i64::from(date.day),
    ) + i64::from(days);
    let (y, m, d) = civil_from_days(z);
    Date {
        year: i32::try_from(y).unwrap_or(if y < 0 { i32::MIN } else { i32::MAX }),
        month: u8::try_from(m).unwrap_or(0),
        day: u8::try_from(d).unwrap_or(0),
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Evaluates one predicate whose field is in the schema (section A.8's
/// numbered rules, after Step 0's gate has already been applied by the
/// caller through [`op_allowed`] and [`value_suits`]).
fn eval_predicate(
    field: &str,
    desc: &FieldDesc,
    op: Op,
    value: &Value,
    adapter: &dyn Adapter,
    today_cache: &mut TodayCache,
) -> bool {
    if !op_allowed(op, desc, value) {
        return false;
    }
    if !matches!(value.class, ValueClass::None | ValueClass::Any) && !value_suits(desc, value) {
        return false;
    }
    let resolved = adapter.resolve(field);
    let empty = resolved_is_empty(desc.ty, &resolved);
    match value.class {
        ValueClass::None => return if op == Op::Ne { !empty } else { empty },
        ValueClass::Any => return if op == Op::Ne { empty } else { !empty },
        _ => {}
    }
    if empty {
        return op == Op::Ne;
    }
    match (desc.ty, resolved) {
        (FieldType::Boolean, Resolved::Bool(b)) => {
            let target = matches!(value.text.to_lowercase().as_str(), "true" | "yes" | "1");
            if op == Op::Ne {
                b != target
            } else {
                b == target
            }
        }
        (FieldType::Number, Resolved::Int(i)) => {
            if let ValueClass::Number(n) = value.class {
                number_matches(Number::Int(i), n, op)
            } else {
                false
            }
        }
        (FieldType::Number, Resolved::Float(f)) => {
            if let ValueClass::Number(n) = value.class {
                number_matches(Number::Float(f), n, op)
            } else {
                false
            }
        }
        (FieldType::Text, Resolved::Text(s)) => {
            let a = s.to_lowercase();
            let b = value.text.to_lowercase();
            match op {
                Op::Colon => a.contains(&b),
                Op::Eq => a == b,
                _ => a != b,
            }
        }
        (FieldType::TextMulti, Resolved::List(v)) => {
            let target = alias_lower(desc, &value.text);
            let has = v.iter().any(|s| s.to_lowercase() == target);
            if op == Op::Ne { !has } else { has }
        }
        (FieldType::Enum, Resolved::Text(s)) => {
            let a = s.to_lowercase();
            let b = alias_lower(desc, &value.text);
            match op {
                Op::Colon | Op::Eq => a == b,
                Op::Ne => a != b,
                Op::Lt | Op::Gt | Op::Le | Op::Ge => {
                    let Some(vals) = &desc.values else {
                        return false;
                    };
                    let pos_a = vals.iter().position(|v| v.to_lowercase() == a);
                    let pos_b = vals.iter().position(|v| v.to_lowercase() == b);
                    let (Some(pa), Some(pb)) = (pos_a, pos_b) else {
                        return false;
                    };
                    match op {
                        Op::Lt => pa < pb,
                        Op::Gt => pa > pb,
                        Op::Le => pa <= pb,
                        Op::Ge => pa >= pb,
                        Op::Colon | Op::Eq | Op::Ne => false,
                    }
                }
            }
        }
        (FieldType::Date, Resolved::Text(s)) => {
            let Some(resolved_date) = parse_date_str(&s) else {
                return op == Op::Ne;
            };
            match date_target(value, adapter, today_cache) {
                None => false,
                Some(DateTarget::Single(target)) => match op {
                    Op::Colon | Op::Eq => resolved_date == target,
                    Op::Ne => resolved_date != target,
                    Op::Lt => resolved_date < target,
                    Op::Gt => resolved_date > target,
                    Op::Le => resolved_date <= target,
                    Op::Ge => resolved_date >= target,
                },
                Some(DateTarget::Range(from, to)) => {
                    let in_range = resolved_date >= from && resolved_date <= to;
                    if op == Op::Ne { !in_range } else { in_range }
                }
            }
        }
        _ => op == Op::Ne,
    }
}

fn eval_text_term(text: &str, quoted: bool, adapter: &dyn Adapter, schema: &FilterSchema) -> bool {
    if !quoted {
        let lower = text.to_lowercase();
        if let Some(desc) = schema.fields.get(&lower) {
            if desc.ty == FieldType::Boolean {
                return matches!(adapter.resolve(&lower), Resolved::Bool(true));
            }
        }
    }
    let needle = text.to_lowercase();
    adapter
        .free_text()
        .iter()
        .any(|s| s.to_lowercase().contains(&needle))
}

fn eval_node(
    node: &Node,
    adapter: &dyn Adapter,
    schema: &FilterSchema,
    today_cache: &mut TodayCache,
) -> bool {
    match node {
        Node::And(children) => children
            .iter()
            .all(|c| eval_node(c, adapter, schema, today_cache)),
        Node::Or(children) => children
            .iter()
            .any(|c| eval_node(c, adapter, schema, today_cache)),
        Node::Not(child) => !eval_node(child, adapter, schema, today_cache),
        Node::Predicate {
            field, op, value, ..
        } => match schema.fields.get(field) {
            Some(desc) => eval_predicate(field, desc, *op, value, adapter, today_cache),
            None => false,
        },
        Node::Text { text, quoted } => eval_text_term(text, *quoted, adapter, schema),
    }
}

/// Whether the record matches (section A.8). Never fails, and reads
/// `adapter.today()` at most once.
#[must_use]
pub fn matches(filter: &Filter, adapter: &dyn Adapter, schema: &FilterSchema) -> bool {
    let mut today_cache = TodayCache::default();
    eval_node(&filter.root, adapter, schema, &mut today_cache)
}

// ---------------------------------------------------------------------------
// A.9 Canonical text
// ---------------------------------------------------------------------------

fn term_unsafe(text: &str) -> bool {
    text.is_empty()
        || text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '"' | ':' | '=' | '<' | '>'))
        || text.contains("!=")
        || text.starts_with('-')
        || keyword_of(text).is_some()
}

/// Whether `text`, written bare (unquoted), would classify as anything other
/// than a plain string (rule 8): the second half of the re-quoting test of
/// section A.9 for a value that was quoted in the source. Only the shape
/// matters here, not validity: a quoted value's text was never checked
/// against sections A.4's date or offset limits, so it can look like a
/// malformed date, and writing it bare would still read as a date attempt.
fn bare_value_reclassifies(text: &str) -> bool {
    text.eq_ignore_ascii_case("none")
        || text.eq_ignore_ascii_case("any")
        || text.eq_ignore_ascii_case("today")
        || text.eq_ignore_ascii_case("tomorrow")
        || text.eq_ignore_ascii_case("yesterday")
        || relative_offset_shape(text)
        || match_date_range_shape(text).is_some()
        || match_date_shape(text).is_some()
        || match_number_shape(text)
}

/// The keyword parts of a relative date's original text, lower-cased, with
/// its sign and digits kept exactly as written (section A.9).
fn format_relative(text: &str) -> String {
    if text.eq_ignore_ascii_case("today") {
        return "today".to_owned();
    }
    if text.eq_ignore_ascii_case("tomorrow") {
        return "tomorrow".to_owned();
    }
    if text.eq_ignore_ascii_case("yesterday") {
        return "yesterday".to_owned();
    }
    if let Some(prefix) = text.get(0..5) {
        if prefix.eq_ignore_ascii_case("today") {
            let mut out = String::with_capacity(text.len());
            out.push_str("today");
            out.push_str(text.get(5..).unwrap_or(""));
            return out;
        }
    }
    text.to_owned()
}

fn format_value(value: &Value) -> String {
    if value.quoted {
        if term_unsafe(&value.text) || bare_value_reclassifies(&value.text) {
            format!("\"{}\"", value.text)
        } else {
            value.text.clone()
        }
    } else {
        match value.class {
            ValueClass::None => "none".to_owned(),
            ValueClass::Any => "any".to_owned(),
            ValueClass::Relative(_) => format_relative(&value.text),
            ValueClass::DateRange(..) | ValueClass::Date(_) | ValueClass::Number(_) => {
                value.text.clone()
            }
            ValueClass::Text => value.text.clone(),
        }
    }
}

fn format_term(text: &str, quoted: bool) -> String {
    if quoted || term_unsafe(text) {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    }
}

/// The parent a node is written under, which decides whether an `and` or an
/// `or` child needs parentheses (section A.9): every combination does,
/// except an `and` written directly inside an `or`.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Ctx {
    Top,
    And,
    Or,
    Not,
}

fn write_node(node: &Node, out: &mut String, ctx: Ctx) {
    match node {
        Node::And(children) => {
            let wrap = !matches!(ctx, Ctx::Or | Ctx::Top);
            if wrap {
                out.push('(');
            }
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_node(child, out, Ctx::And);
            }
            if wrap {
                out.push(')');
            }
        }
        Node::Or(children) => {
            let wrap = ctx != Ctx::Top;
            if wrap {
                out.push('(');
            }
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    out.push_str(" OR ");
                }
                write_node(child, out, Ctx::Or);
            }
            if wrap {
                out.push(')');
            }
        }
        Node::Not(child) => {
            out.push('-');
            write_node(child, out, Ctx::Not);
        }
        Node::Predicate {
            field, op, value, ..
        } => {
            out.push_str(field);
            out.push_str(op.symbol());
            out.push_str(&format_value(value));
        }
        Node::Text { text, quoted } => out.push_str(&format_term(text, *quoted)),
    }
}

/// The canonical text of a filter (section A.9).
#[must_use]
pub fn format(filter: &Filter) -> String {
    let mut out = String::new();
    write_node(&filter.root, &mut out, Ctx::Top);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(ty: FieldType) -> FieldDesc {
        FieldDesc {
            ty,
            aliases: BTreeMap::new(),
            values: None,
        }
    }

    fn vectors_schema() -> FilterSchema {
        let mut fields = BTreeMap::new();
        let mut status = field(FieldType::Enum);
        status.values = Some(vec![
            "waiting".to_owned(),
            "doing".to_owned(),
            "done".to_owned(),
        ]);
        status
            .aliases
            .insert("open".to_owned(), "waiting".to_owned());
        status
            .aliases
            .insert("completed".to_owned(), "done".to_owned());
        fields.insert("status".to_owned(), status);
        let mut priority = field(FieldType::Enum);
        priority.values = Some(vec![
            "low".to_owned(),
            "medium".to_owned(),
            "high".to_owned(),
        ]);
        fields.insert("priority".to_owned(), priority);
        fields.insert("tag".to_owned(), field(FieldType::TextMulti));
        fields.insert("title".to_owned(), field(FieldType::Text));
        fields.insert("due".to_owned(), field(FieldType::Date));
        fields.insert("points".to_owned(), field(FieldType::Number));
        fields.insert("starred".to_owned(), field(FieldType::Boolean));
        FilterSchema { fields }
    }

    struct Record {
        status: &'static str,
        priority: &'static str,
        tag: Vec<&'static str>,
        title: &'static str,
        due: Option<&'static str>,
        points: Option<i64>,
        starred: bool,
    }

    impl Adapter for Record {
        fn resolve(&self, field: &str) -> Resolved {
            match field {
                "status" => Resolved::Text(self.status.to_owned()),
                "priority" => Resolved::Text(self.priority.to_owned()),
                "tag" => Resolved::List(self.tag.iter().map(|s| (*s).to_owned()).collect()),
                "title" => Resolved::Text(self.title.to_owned()),
                "due" => self
                    .due
                    .map_or(Resolved::Absent, |d| Resolved::Text(d.to_owned())),
                "points" => self.points.map_or(Resolved::Absent, Resolved::Int),
                "starred" => Resolved::Bool(self.starred),
                _ => Resolved::Absent,
            }
        }

        fn free_text(&self) -> Vec<String> {
            vec![self.title.to_owned()]
        }

        fn today(&self) -> String {
            "2026-07-26".to_owned()
        }
    }

    fn r1() -> Record {
        Record {
            status: "doing",
            priority: "medium",
            tag: vec!["work", "q3"],
            title: "Quarterly report",
            due: Some("2026-07-30"),
            points: Some(5),
            starred: true,
        }
    }

    fn r2() -> Record {
        Record {
            status: "waiting",
            priority: "high",
            tag: vec![],
            title: "Call back",
            due: None,
            points: None,
            starred: false,
        }
    }

    #[test]
    fn parse_vectors_of_section_a11() {
        let ok = [
            ("status:waiting", "status:waiting"),
            (
                "Status:Waiting priority>=high -tag:chore",
                "status:Waiting priority>=high -tag:chore",
            ),
            ("due<today", "due<today"),
            ("due<TODAY+7", "due<today+7"),
            ("a OR b c", "a OR b c"),
            ("(a OR b) c", "(a OR b) c"),
            ("NOT a", "-a"),
            ("-(a OR b)", "-(a OR b)"),
            ("a (b c)", "a (b c)"),
            ("a OR (b OR c)", "a OR (b OR c)"),
            ("\"hello world\"", "\"hello world\""),
            ("-\"a b\"", "-\"a b\""),
            ("a\"b\"", "a \"b\""),
            ("x:-3", "x:-3"),
            ("a!=b", "a!=b"),
            ("a!b", "a!b"),
            ("a-b", "a-b"),
            ("d:2026-07-01..2026-07-31", "d:2026-07-01..2026-07-31"),
            ("n:\"7\"", "n:\"7\""),
            ("\"OR\"", "\"OR\""),
        ];
        for (src, canonical) in ok {
            let filter = parse(src).unwrap_or_else(|e| {
                panic!("expected {src:?} to parse, got {code:?}", code = e.code)
            });
            assert_eq!(format(&filter), canonical, "source: {src}");
        }

        let errs = [
            ("status:waiting OR", Code::HS0701),
            ("OR a", Code::HS0701),
            ("status:waiting NOT", Code::HS0701),
            ("a AND AND b", Code::HS0701),
            ("a )", Code::HS0701),
            ("( a", Code::HS0701),
            ("()", Code::HS0701),
            ("status: priority:high", Code::HS0701),
            ("status :open", Code::HS0701),
            ("x: -3", Code::HS0701),
            ("status:or", Code::HS0701),
            ("-", Code::HS0701),
            ("\"\"", Code::HS0701),
            ("\"unterminated", Code::HS0702),
            ("", Code::HS0710),
            ("   ", Code::HS0710),
            ("d:2026-13-45", Code::HS0705),
            ("d:2026-02-30", Code::HS0705),
            ("d:today+5001", Code::HS0706),
            ("d:2026-07-31..2026-07-01", Code::HS0711),
        ];
        for (src, code) in errs {
            let err = parse(src).unwrap_err();
            assert_eq!(err.code, code, "source: {src:?}");
        }
    }

    #[test]
    fn validate_vectors_of_section_a11() {
        let schema = vectors_schema();
        let cases = [
            ("staus:doing", Some(Code::HS0708)),
            ("status:dwon", Some(Code::HS0709)),
            ("status:open", None),
            ("status<any", Some(Code::HS0707)),
            ("points>=abc", Some(Code::HS0709)),
            ("starred:ture", Some(Code::HS0709)),
            ("starred>true", Some(Code::HS0707)),
            ("due<2026-07-01..2026-07-31", Some(Code::HS0707)),
            ("tag>work", Some(Code::HS0707)),
            ("status:doing priority:high", None),
        ];
        for (src, expected) in cases {
            let filter = parse(src).unwrap_or_else(|e| panic!("{src} did not parse: {e:?}"));
            let problems = validate(&filter, &schema);
            match expected {
                None => assert!(problems.is_empty(), "{src} should be valid: {problems:?}"),
                Some(code) => {
                    assert_eq!(problems.len(), 1, "{src}: {problems:?}");
                    assert_eq!(problems.first().map(|p| p.code), Some(code), "{src}");
                }
            }
        }
    }

    #[test]
    fn evaluation_vectors_of_section_a11() {
        let schema = vectors_schema();
        let cases: &[(&str, bool, bool)] = &[
            ("status:doing", true, false),
            ("status:open", false, true),
            ("status>=doing", true, false),
            ("priority>medium", false, true),
            ("-tag:work", false, true),
            ("tag:none", false, true),
            ("tag:any", true, false),
            ("due<today+5", true, false),
            ("due:none", false, true),
            ("due!=2026-07-30", false, true),
            ("due:2026-07-01..2026-07-31", true, false),
            ("points>=5", true, false),
            ("points!=5", false, true),
            ("points:0", false, false),
            ("starred", true, false),
            ("-starred", false, true),
            ("starred:no", false, true),
            ("report", true, false),
            ("\"call back\"", false, true),
            ("title:call", false, true),
            ("title=quarterly", false, false),
            ("title=\"quarterly report\"", true, false),
            ("status:doing OR priority:high", true, true),
            ("status:doing priority:high", false, false),
            ("starred:ture", false, false),
            ("points>=abc", false, false),
            ("title>x", false, false),
            ("due<2026-07-01..2026-07-31", false, false),
        ];
        let (rec1, rec2) = (r1(), r2());
        for (src, exp1, exp2) in cases.iter().copied() {
            let filter = parse(src).unwrap_or_else(|e| panic!("{src} did not parse: {e:?}"));
            assert_eq!(matches(&filter, &rec1, &schema), exp1, "{src} on R1");
            assert_eq!(matches(&filter, &rec2, &schema), exp2, "{src} on R2");
        }
    }

    #[test]
    fn the_parser_bounds_its_own_stack() {
        let deep = "(".repeat(200) + &")".repeat(200);
        let err = parse(&deep).unwrap_err();
        assert_eq!(err.code, Code::HS0703);
        let negations = "-".repeat(200) + "a";
        let err = parse(&negations).unwrap_err();
        assert_eq!(err.code, Code::HS0703);
    }

    #[test]
    fn source_length_is_capped() {
        let long = "a".repeat(MAX_FILTER_BYTES + 1);
        let err = parse(&long).unwrap_err();
        assert_eq!(err.code, Code::HS0704);
    }
}
