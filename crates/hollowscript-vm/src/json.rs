//! JSON, as section 15.1 defines it: a strict RFC 8259 reader whose errors
//! are part of the language's own contract, and a writer whose output is
//! fixed to the byte.
//!
//! The parser works on bytes and reports an offset in bytes, which is what
//! `JsonError` carries. It uses an explicit depth counter rather than
//! recursion for arrays and objects, and refuses at 129 levels, so a
//! hostile document cannot exhaust the host's stack before the language's
//! own limit stops it. [`parse`] is the plain entry point, used wherever
//! there is no run to charge or cancel against (the C and WebAssembly
//! boundaries decoding a capability answer or a host call's arguments);
//! [`parse_charged`] is [`json.parse`](crate::natives)'s own entry point,
//! which charges and safepoints as it goes rather than in one lump before
//! parsing starts.

use crate::interp::charge_steps_sliced;
use crate::limits::RunCounters;
use hollowscript_core::Limits;
use hollowscript_core::cancel::Control;
use hollowscript_core::fault::Fault;

/// A parsed JSON document, before it becomes a heap value.
///
/// This mirrors the prelude enum `Json` of section 15.1 one variant at a
/// time. It exists so that parsing, writing and sizing can be written and
/// tested without a heap or a module's variant table.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number with no fraction and no exponent that fits an `Int`.
    Int(i64),
    /// Every other number.
    Float(f64),
    /// A string, with its escapes resolved.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object, in ascending key order with no duplicate: "if an object
    /// has the same key twice, the later value wins".
    Object(Vec<(String, Json)>),
}

/// Why a document could not be parsed (section 15.1's own table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// One of the messages section 15.1 lists, verbatim.
    pub message: &'static str,
    /// The byte offset at which the problem was detected.
    pub offset: usize,
}

/// The deepest an array or object may nest (section 15.1).
const MAX_DEPTH: u32 = 128;

/// Parses `text` as one JSON value, optionally surrounded by whitespace.
///
/// # Errors
///
/// One of section 15.1's messages, with the offset that section gives for it.
pub fn parse(text: &str) -> Result<Json, JsonError> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        text,
        at: 0,
        charge: None,
    };
    match reader.run() {
        Ok(value) => Ok(value),
        Err(Outcome::Json(error)) => Err(error),
        // No charge state was given, so `checkpoint` and `finish` below
        // never had a step budget to run out of and could not have
        // returned this. Kept as a plain `JsonError` rather than a panic,
        // matching the lint policy, in case that ever stops being true.
        Err(Outcome::Fault(_)) => Err(JsonError {
            message: "unexpected end of input",
            offset: text.len(),
        }),
    }
}

/// [`parse`], but for a call made from a running program: charges and
/// safepoints the parse in slices as it consumes `text` (section 11.7's
/// safepoint rule, section 13.1's `1 + floor(N / 64)` cost), rather than
/// charging the whole cost in one lump before any of the work is done. That
/// used to leave a large or slow parse unable to be cancelled: the charge
/// happened, and passed, before `parse` was ever called, and nothing polled
/// the cancellation token again until it returned.
///
/// # Errors
///
/// `Outcome::Json` for a document `text` does not parse as JSON, or
/// `Outcome::Fault` if the run is cancelled or goes over budget partway
/// through.
pub fn parse_charged(
    text: &str,
    counters: &mut RunCounters,
    limits: &Limits,
    control: &mut Control<'_>,
) -> Result<Json, Outcome> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        text,
        at: 0,
        charge: Some(ChargeState {
            counters,
            limits,
            control,
            charged_through: 0,
        }),
    };
    let value = reader.run()?;
    if let Some(charge) = reader.charge.as_mut() {
        charge.finish(reader.at)?;
    }
    Ok(value)
}

/// Either half of what a parse can fail with: a document that does not
/// parse, or a run that was cancelled or went over budget while parsing it.
#[derive(Debug)]
pub enum Outcome {
    /// The document is not valid JSON.
    Json(JsonError),
    /// The run's own budget or cancellation stopped the parse.
    Fault(Fault),
}

impl From<JsonError> for Outcome {
    fn from(error: JsonError) -> Self {
        Outcome::Json(error)
    }
}

impl From<Fault> for Outcome {
    fn from(fault: Fault) -> Self {
        Outcome::Fault(fault)
    }
}

/// How often, in bytes consumed, a charged parse charges and safepoints:
/// often enough that a cancellation token is polled well within a moment of
/// being set, rarely enough that the bookkeeping does not dominate the cost
/// of an ordinary document.
const CHARGE_CHUNK_BYTES: u64 = 4096;

/// The running total charged so far for a [`parse_charged`] call, and the
/// counters, limits and cancellation token it charges and polls against.
struct ChargeState<'a, 'b> {
    counters: &'a mut RunCounters,
    limits: &'a Limits,
    control: &'a mut Control<'b>,
    /// The byte position the charge is paid up to.
    charged_through: u64,
}

impl ChargeState<'_, '_> {
    /// Charges and safepoints once at least [`CHARGE_CHUNK_BYTES`] have been
    /// consumed since the last charge; otherwise does nothing, so this can
    /// be called as often as convenient without dominating the parse.
    fn checkpoint(&mut self, at: usize) -> Result<(), Fault> {
        let at = at as u64;
        if at < self.charged_through.saturating_add(CHARGE_CHUNK_BYTES) {
            return Ok(());
        }
        self.flush(at)
    }

    /// Charges whatever is owed up to `at`, however little, so a document
    /// shorter than one chunk, or the last partial chunk of a longer one,
    /// is still paid for. Section 13.1's `1 + floor(N / 64)` is charged as
    /// the difference between what `at` bytes are worth and what was
    /// already charged, so the total across every call is exactly that,
    /// regardless of how it was sliced.
    fn finish(&mut self, at: usize) -> Result<(), Fault> {
        self.flush(at as u64)
    }

    fn flush(&mut self, at: u64) -> Result<(), Fault> {
        let due = 1 + at / 64;
        let already = if self.charged_through == 0 {
            0
        } else {
            1 + self.charged_through / 64
        };
        if due > already {
            charge_steps_sliced(self.counters, self.limits, self.control, due - already)?;
        }
        self.charged_through = at;
        Ok(())
    }
}

struct Reader<'a, 'b, 'c> {
    bytes: &'a [u8],
    /// The same bytes, already known valid UTF-8 (`parse`'s caller is
    /// `&str`), so a character can be read at any position the reader
    /// itself has advanced to (always a char boundary) in constant time:
    /// `str::get` only checks the boundary, unlike re-validating the whole
    /// rest of the input as `std::str::from_utf8` would (this used to make
    /// a string of N bytes take time quadratic in N to parse).
    text: &'a str,
    at: usize,
    charge: Option<ChargeState<'b, 'c>>,
}

impl Reader<'_, '_, '_> {
    /// Charges and safepoints for the bytes consumed so far, if this parse
    /// has a budget to charge against.
    fn checkpoint(&mut self) -> Result<(), Fault> {
        let at = self.at;
        match self.charge.as_mut() {
            Some(charge) => charge.checkpoint(at),
            None => Ok(()),
        }
    }

    fn run(&mut self) -> Result<Json, Outcome> {
        // "A byte order mark at the start is an error", at offset 0.
        if self.bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            return Err(Outcome::Json(JsonError {
                message: "unexpected character",
                offset: 0,
            }));
        }
        self.skip_whitespace();
        let value = self.value(1)?;
        self.skip_whitespace();
        if self.at != self.bytes.len() {
            return Err(Outcome::Json(JsonError {
                message: "unexpected trailing characters",
                offset: self.at,
            }));
        }
        Ok(value)
    }
}

impl Reader<'_, '_, '_> {
    fn fail(message: &'static str, offset: usize) -> Outcome {
        Outcome::Json(JsonError { message, offset })
    }

    fn end(&self) -> Outcome {
        Outcome::Json(JsonError {
            message: "unexpected end of input",
            offset: self.bytes.len(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    /// "Whitespace between tokens is space, tab, LF and CR only."
    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn value(&mut self, depth: u32) -> Result<Json, Outcome> {
        self.checkpoint()?;
        match self.peek() {
            None => Err(self.end()),
            Some(b'n') => self.literal("null").map(|()| Json::Null),
            Some(b't') => self.literal("true").map(|()| Json::Bool(true)),
            Some(b'f') => self.literal("false").map(|()| Json::Bool(false)),
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            // "a value cannot start with `.` or `+`"
            Some(_) => Err(Self::fail("unexpected character", self.at)),
        }
    }

    /// `true`, `false` or `null`, matched byte by byte: the first byte that
    /// differs is `unexpected character`, and running out is an end.
    fn literal(&mut self, word: &str) -> Result<(), Outcome> {
        for (offset, wanted) in word.bytes().enumerate() {
            match self.bytes.get(self.at + offset) {
                None => return Err(self.end()),
                Some(&found) if found == wanted => {}
                Some(_) => return Err(Self::fail("unexpected character", self.at + offset)),
            }
        }
        self.at += word.len();
        Ok(())
    }

    fn array(&mut self, depth: u32) -> Result<Json, Outcome> {
        if depth > MAX_DEPTH {
            return Err(Self::fail("nesting too deep", self.at));
        }
        self.at += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                None => return Err(self.end()),
                Some(b',') => {
                    self.at += 1;
                    self.skip_whitespace();
                    // "a `,` before `]` or `}`" is unexpected.
                    if self.peek() == Some(b']') {
                        return Err(Self::fail("unexpected character", self.at));
                    }
                }
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                Some(_) => return Err(Self::fail("unexpected character", self.at)),
            }
        }
    }

    fn object(&mut self, depth: u32) -> Result<Json, Outcome> {
        if depth > MAX_DEPTH {
            return Err(Self::fail("nesting too deep", self.at));
        }
        self.at += 1;
        let mut members: Vec<(String, Json)> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Err(self.end()),
                // "an unquoted key" is unexpected.
                Some(b'"') => {}
                Some(_) => return Err(Self::fail("unexpected character", self.at)),
            }
            let key = self.string()?;
            self.skip_whitespace();
            match self.peek() {
                None => return Err(self.end()),
                Some(b':') => self.at += 1,
                Some(_) => return Err(Self::fail("unexpected character", self.at)),
            }
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            // "If an object has the same key twice, the later value wins."
            match members.binary_search_by(|(k, _)| k.as_str().cmp(key.as_str())) {
                Ok(index) => {
                    if let Some(slot) = members.get_mut(index) {
                        slot.1 = value;
                    }
                }
                Err(index) => members.insert(index, (key, value)),
            }
            self.skip_whitespace();
            match self.peek() {
                None => return Err(self.end()),
                Some(b',') => {
                    self.at += 1;
                    self.skip_whitespace();
                    if self.peek() == Some(b'}') {
                        return Err(Self::fail("unexpected character", self.at));
                    }
                }
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(members));
                }
                Some(_) => return Err(Self::fail("unexpected character", self.at)),
            }
        }
    }

    fn string(&mut self) -> Result<String, Outcome> {
        self.at += 1;
        let mut out = String::new();
        loop {
            // Charged and safepointed here, not only once per value: a
            // single long string is the case a lump sum charged before
            // parsing started could never make cancellable.
            self.checkpoint()?;
            let Some(byte) = self.peek() else {
                return Err(self.end());
            };
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => self.escape(&mut out)?,
                // "A raw control character (below U+0020) inside a string is
                // an error."
                0x00..=0x1f => return Err(Self::fail("invalid string", self.at)),
                _ => {
                    // `self.text` is already known valid UTF-8 (it is the
                    // whole `&str` `parse` was given), so this only checks
                    // that `self.at` sits on a char boundary, in constant
                    // time; it used to re-validate every byte from `self.at`
                    // to the end of the input for every character read,
                    // which made a string of N bytes take time quadratic in
                    // N to parse.
                    let rest = self
                        .text
                        .get(self.at..)
                        .ok_or_else(|| Self::fail("invalid string", self.at))?;
                    let c = rest.chars().next().ok_or_else(|| self.end())?;
                    out.push(c);
                    self.at += c.len_utf8();
                }
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), Outcome> {
        let backslash = self.at;
        self.at += 1;
        let Some(byte) = self.peek() else {
            return Err(self.end());
        };
        let simple = match byte {
            b'"' => Some('"'),
            b'\\' => Some('\\'),
            b'/' => Some('/'),
            b'b' => Some('\u{8}'),
            b'f' => Some('\u{c}'),
            b'n' => Some('\n'),
            b'r' => Some('\r'),
            b't' => Some('\t'),
            _ => None,
        };
        if let Some(c) = simple {
            self.at += 1;
            out.push(c);
            return Ok(());
        }
        if byte != b'u' {
            return Err(Self::fail("invalid escape sequence", backslash));
        }
        self.at += 1;
        let high = self.hex4(backslash)?;
        if (0xd800..0xdc00).contains(&high) {
            // "A `\u` escape that is a high surrogate must be followed by a
            // `\u` escape that is a low surrogate", and the offset is the
            // backslash of the first escape of the pair.
            if self.bytes.get(self.at..self.at + 2) != Some(b"\\u") {
                return Err(Self::fail("invalid unicode escape", backslash));
            }
            self.at += 2;
            let low = self.hex4(backslash)?;
            if !(0xdc00..0xe000).contains(&low) {
                return Err(Self::fail("invalid unicode escape", backslash));
            }
            let combined = 0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
            let c = char::from_u32(combined)
                .ok_or_else(|| Self::fail("invalid unicode escape", backslash))?;
            out.push(c);
            return Ok(());
        }
        // "a low surrogate escape on its own" is an error too.
        let c =
            char::from_u32(high).ok_or_else(|| Self::fail("invalid unicode escape", backslash))?;
        out.push(c);
        Ok(())
    }

    fn hex4(&mut self, backslash: usize) -> Result<u32, Outcome> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(byte) = self.peek() else {
                return Err(self.end());
            };
            let digit = match byte {
                b'0'..=b'9' => u32::from(byte - b'0'),
                b'a'..=b'f' => u32::from(byte - b'a') + 10,
                b'A'..=b'F' => u32::from(byte - b'A') + 10,
                _ => return Err(Self::fail("invalid unicode escape", backslash)),
            };
            value = value * 16 + digit;
            self.at += 1;
        }
        Ok(value)
    }

    /// A number: `-`, then `0` or a non-zero digit and digits, then
    /// optionally a fraction, then optionally an exponent.
    fn number(&mut self) -> Result<Json, Outcome> {
        let start = self.at;
        let bad = |at: usize| {
            Outcome::Json(JsonError {
                message: "invalid number",
                offset: at,
            })
        };
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            // A number is scanned as one token, so a truncated one is
            // "a `0` followed by a digit (`01`)" is invalid.
            Some(b'0') => {
                self.at += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(bad(start));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.at += 1;
                }
            }
            // "`-` with no digit after it", and a number cut short by the
            // end of the input: a number is scanned as one token, so a
            // truncated one is `invalid number` and never `unexpected end
            // of input`, which section 15.1's own table says in its
            // end-of-input row.
            None | Some(_) => return Err(bad(start)),
        }
        let mut integral = true;
        if self.peek() == Some(b'.') {
            integral = false;
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(bad(start));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            integral = false;
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(bad(start));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        let text = self
            .bytes
            .get(start..self.at)
            .and_then(|b| std::str::from_utf8(b).ok())
            .ok_or_else(|| bad(start))?;
        if integral {
            // "A number is `Int` if it has no fraction and no exponent and
            // fits in an `Int` (`-0` is `Int(0)`), and `Float` otherwise,
            // including an integer too large for an `Int`."
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Json::Int(value));
            }
        }
        let value: f64 = text.parse().map_err(|_| bad(start))?;
        if value.is_infinite() {
            return Err(Outcome::Json(JsonError {
                message: "number out of range",
                offset: start,
            }));
        }
        Ok(Json::Float(if value == 0.0 { 0.0 } else { value }))
    }
}

/// `json.stringify` (section 15.1): compact text, no white space.
#[must_use]
pub fn stringify(value: &Json) -> String {
    let mut out = String::new();
    write(value, None, 0, &mut out);
    out
}

/// `json.pretty` (section 15.1): the same text with a line feed and
/// `indent` spaces per level before each element and member and before the
/// closing bracket.
#[must_use]
pub fn pretty(value: &Json, indent: usize) -> String {
    let mut out = String::new();
    write(value, Some(indent), 0, &mut out);
    out
}

/// The exact number of UTF-8 bytes [`stringify()`] or [`pretty`] would produce
/// for `value`, worked out without building the text, so a caller can charge
/// for it before doing the work (section 13.1: a result whose size is known
/// in advance is charged before it is built). Kept in step with `write`:
/// every arm here counts exactly what the matching arm there pushes.
#[must_use]
pub fn write_len(value: &Json, indent: Option<usize>) -> u64 {
    let mut total = 0u64;
    // An explicit stack, like `write` and `depth`: this must not recurse
    // without bound either.
    let mut stack = vec![(value, 0usize)];
    while let Some((value, level)) = stack.pop() {
        match value {
            Json::Null | Json::Bool(true) => total = total.saturating_add(4),
            Json::Bool(false) => total = total.saturating_add(5),
            Json::Int(n) => total = total.saturating_add(n.to_string().len() as u64),
            Json::Float(f) => {
                total = total.saturating_add(crate::interp::format_float(*f).len() as u64);
            }
            Json::String(s) => total = total.saturating_add(json_string_len(s)),
            Json::Array(items) => {
                if items.is_empty() {
                    total = total.saturating_add(2);
                    continue;
                }
                total = total.saturating_add(2); // '[' and ']'
                total = total.saturating_add(items.len().saturating_sub(1) as u64); // ','
                if let Some(width) = indent {
                    total = total.saturating_add(
                        (items.len() as u64).saturating_mul(newline_len(width, level + 1)),
                    );
                    total = total.saturating_add(newline_len(width, level));
                }
                stack.extend(items.iter().map(|item| (item, level + 1)));
            }
            Json::Object(members) => {
                if members.is_empty() {
                    total = total.saturating_add(2);
                    continue;
                }
                total = total.saturating_add(2); // '{' and '}'
                total = total.saturating_add(members.len().saturating_sub(1) as u64); // ','
                if let Some(width) = indent {
                    total = total.saturating_add(
                        (members.len() as u64).saturating_mul(newline_len(width, level + 1)),
                    );
                    total = total.saturating_add(newline_len(width, level));
                }
                for (key, item) in members {
                    total = total.saturating_add(json_string_len(key));
                    total = total.saturating_add(1); // ':'
                    if indent.is_some() {
                        total = total.saturating_add(1); // ' '
                    }
                    stack.push((item, level + 1));
                }
            }
        }
    }
    total
}

/// The length in bytes of `newline`'s output for this `width` and `level`.
fn newline_len(width: usize, level: usize) -> u64 {
    1u64.saturating_add((width as u64).saturating_mul(level as u64))
}

/// The length in bytes of [`push_json_string`]'s output for `text`.
fn json_string_len(text: &str) -> u64 {
    let mut total = 2u64; // the quotes
    for c in text.chars() {
        total = total.saturating_add(match c {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            c if (c as u32) < 0x20 || c == '\u{7f}' => 6,
            c => c.len_utf8() as u64,
        });
    }
    total
}

/// The depth of the deepest array or object in `value`, counting the
/// outermost as 1, saturating at `MAX_DEPTH + 1` so that a value too deep to
/// write is reported without walking all of it.
#[must_use]
pub fn depth(value: &Json) -> u32 {
    let mut deepest = 0;
    // An explicit stack: the encoder "MUST NOT recurse without bound", and
    // neither may the check that decides whether it may run.
    let mut stack = vec![(value, 1u32)];
    while let Some((value, level)) = stack.pop() {
        match value {
            Json::Array(items) => {
                deepest = deepest.max(level);
                if level > MAX_DEPTH {
                    return level;
                }
                stack.extend(items.iter().map(|item| (item, level + 1)));
            }
            Json::Object(members) => {
                deepest = deepest.max(level);
                if level > MAX_DEPTH {
                    return level;
                }
                stack.extend(members.iter().map(|(_, item)| (item, level + 1)));
            }
            _ => {}
        }
    }
    deepest
}

/// Writes `value`, with `indent` spaces per level when pretty-printing.
fn write(value: &Json, indent: Option<usize>, level: usize, out: &mut String) {
    /// What the writer has still to do.
    enum Step<'a> {
        Value(&'a Json, usize),
        Text(&'static str),
        Owned(String),
    }
    let mut stack = vec![Step::Value(value, level)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Text(text) => out.push_str(text),
            Step::Owned(text) => out.push_str(&text),
            Step::Value(value, level) => match value {
                Json::Null => out.push_str("null"),
                Json::Bool(true) => out.push_str("true"),
                Json::Bool(false) => out.push_str("false"),
                Json::Int(n) => out.push_str(&n.to_string()),
                Json::Float(f) => out.push_str(&crate::interp::format_float(*f)),
                Json::String(s) => push_json_string(s, out),
                Json::Array(items) => {
                    if items.is_empty() {
                        out.push_str("[]");
                        continue;
                    }
                    out.push('[');
                    let mut steps = Vec::with_capacity(items.len() * 3 + 2);
                    for (index, item) in items.iter().enumerate() {
                        if index > 0 {
                            steps.push(Step::Text(","));
                        }
                        if let Some(width) = indent {
                            steps.push(Step::Owned(newline(width, level + 1)));
                        }
                        steps.push(Step::Value(item, level + 1));
                    }
                    if let Some(width) = indent {
                        steps.push(Step::Owned(newline(width, level)));
                    }
                    steps.push(Step::Text("]"));
                    stack.extend(steps.into_iter().rev());
                }
                Json::Object(members) => {
                    if members.is_empty() {
                        out.push_str("{}");
                        continue;
                    }
                    out.push('{');
                    let mut steps = Vec::with_capacity(members.len() * 4 + 2);
                    for (index, (key, item)) in members.iter().enumerate() {
                        if index > 0 {
                            steps.push(Step::Text(","));
                        }
                        if let Some(width) = indent {
                            steps.push(Step::Owned(newline(width, level + 1)));
                        }
                        let mut written = String::new();
                        push_json_string(key, &mut written);
                        written.push(':');
                        if indent.is_some() {
                            written.push(' ');
                        }
                        steps.push(Step::Owned(written));
                        steps.push(Step::Value(item, level + 1));
                    }
                    if let Some(width) = indent {
                        steps.push(Step::Owned(newline(width, level)));
                    }
                    steps.push(Step::Text("}"));
                    stack.extend(steps.into_iter().rev());
                }
            },
        }
    }
}

fn newline(width: usize, level: usize) -> String {
    let mut out = String::with_capacity(1 + width * level);
    out.push('\n');
    for _ in 0..(width * level) {
        out.push(' ');
    }
    out
}

/// A JSON string literal, quotes included, escaped as section 15.1 states.
fn push_json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // "every other code point below U+0020, and U+007F, as `\u00xx`
            // with four lower-case hexadecimal digits"
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            // "`/` is not escaped"
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::{
        Json, JsonError, Outcome, depth, parse, parse_charged, pretty, stringify, write_len,
    };
    use crate::limits::RunCounters;
    use hollowscript_core::{CancellationToken, Control, Limits};

    fn err(text: &str) -> JsonError {
        parse(text).expect_err("should not parse")
    }

    #[test]
    fn the_worked_example_of_section_15_1_round_trips() {
        let parsed = parse(r#"{"b": [1, 2.5, true], "a": null}"#).expect("parses");
        // An object is written in ascending key order.
        assert_eq!(stringify(&parsed), r#"{"a":null,"b":[1,2.5,true]}"#);
    }

    #[test]
    fn pretty_matches_the_specifications_own_output() {
        let doc = Json::Array(vec![
            Json::Int(1),
            Json::Object(vec![("a".to_owned(), Json::Null)]),
        ]);
        assert_eq!(pretty(&doc, 2), "[\n  1,\n  {\n    \"a\": null\n  }\n]");
        // "an empty array or object as `[]` or `{}`"
        assert_eq!(pretty(&Json::Array(vec![]), 2), "[]");
        assert_eq!(pretty(&Json::Object(vec![]), 2), "{}");
    }

    /// "A number is `Int` if it has no fraction and no exponent and fits in
    /// an `Int` (`-0` is `Int(0)`), and `Float` otherwise, including an
    /// integer too large for an `Int`."
    #[test]
    fn numbers_are_int_or_float_by_the_rule_of_section_15_1() {
        assert_eq!(parse("1"), Ok(Json::Int(1)));
        assert_eq!(parse("-0"), Ok(Json::Int(0)));
        assert_eq!(parse("1.0"), Ok(Json::Float(1.0)));
        assert_eq!(parse("1e2"), Ok(Json::Float(100.0)));
        assert_eq!(parse("-0.0"), Ok(Json::Float(0.0)), "no negative zero");
        assert_eq!(
            parse("9223372036854775808"),
            Ok(Json::Float(9_223_372_036_854_775_808.0)),
            "too large for an Int"
        );
        assert_eq!(err("1e999").message, "number out of range");
    }

    /// Every message of section 15.1's own table, with the offset it gives.
    #[test]
    fn each_error_has_the_message_and_offset_section_15_1_gives() {
        assert_eq!(err("").message, "unexpected end of input");
        assert_eq!(err("[1,").message, "unexpected end of input");
        assert_eq!(err("{\"a\":").message, "unexpected end of input");
        assert_eq!(err("\"ab").message, "unexpected end of input");

        assert_eq!(err("$").message, "unexpected character");
        assert_eq!(err(".5").message, "unexpected character");
        assert_eq!(err("+1").message, "unexpected character");
        assert_eq!(err("{a: 1}").message, "unexpected character");
        assert_eq!(err("[1,]").message, "unexpected character");
        assert_eq!(err("{\"a\" 1}").message, "unexpected character");
        assert_eq!(
            err("\u{feff}1"),
            JsonError {
                message: "unexpected character",
                offset: 0,
            }
        );
        // "the first byte that differs from `true` ... when a value starts
        // with `t`, `f` or `n`"
        assert_eq!(
            err("trve"),
            JsonError {
                message: "unexpected character",
                offset: 2,
            }
        );

        assert_eq!(
            err(r#""a\qb""#),
            JsonError {
                message: "invalid escape sequence",
                offset: 2,
            }
        );
        assert_eq!(
            err(r#""\uZZZZ""#),
            JsonError {
                message: "invalid unicode escape",
                offset: 1,
            }
        );
        assert_eq!(
            err(r#""\ud800""#),
            JsonError {
                message: "invalid unicode escape",
                offset: 1,
            },
            "a high surrogate with no low surrogate"
        );
        assert_eq!(
            err(r#""\udc00""#).message,
            "invalid unicode escape",
            "a low surrogate on its own"
        );
        assert_eq!(
            err("\"a\u{1}b\""),
            JsonError {
                message: "invalid string",
                offset: 2,
            }
        );

        for (text, offset) in [("-", 0), ("01", 0), ("1.", 0), ("1e", 0), ("[01]", 1)] {
            let e = err(text);
            assert_eq!(e.message, "invalid number", "{text}");
            assert_eq!(e.offset, offset, "{text}");
        }

        assert_eq!(
            err("truex"),
            JsonError {
                message: "unexpected trailing characters",
                offset: 4,
            }
        );
        assert_eq!(err("1 2").message, "unexpected trailing characters");
    }

    /// "Arrays and objects may be nested at most 128 deep", and the bracket
    /// that makes it 129 is where the error is.
    #[test]
    fn nesting_is_refused_at_the_bracket_that_makes_it_too_deep() {
        let deep = |levels: usize| format!("{}{}", "[".repeat(levels), "]".repeat(levels));
        assert!(parse(&deep(128)).is_ok());
        let e = err(&deep(129));
        assert_eq!(e.message, "nesting too deep");
        assert_eq!(e.offset, 128, "the 129th bracket");
        // A document far past the limit is refused, not recursed into.
        assert_eq!(err(&"[".repeat(100_000)).message, "nesting too deep");
    }

    #[test]
    fn a_repeated_key_keeps_the_later_value() {
        assert_eq!(
            parse(r#"{"a": 1, "a": 2}"#),
            Ok(Json::Object(vec![("a".to_owned(), Json::Int(2))]))
        );
    }

    #[test]
    fn escapes_are_read_and_written_as_section_15_1_states() {
        let parsed = parse(r#""a\"b\\c\/d\b\f\n\r\teA😀""#).expect("parses");
        assert_eq!(
            parsed,
            Json::String("a\"b\\c/d\u{8}\u{c}\n\r\teA\u{1f600}".to_owned())
        );
        // `/` is not escaped on the way out, and U+007F is.
        assert_eq!(
            stringify(&Json::String("/\u{7f}\u{1}".to_owned())),
            r#""/\u007f\u0001""#
        );
    }

    #[test]
    fn whitespace_is_only_the_four_bytes_section_15_1_allows() {
        assert!(parse(" \t\r\n1 \t\r\n").is_ok());
        assert_eq!(err("\u{b}1").message, "unexpected character");
    }

    #[test]
    fn depth_counts_the_outermost_as_one_and_saturates() {
        assert_eq!(depth(&Json::Int(1)), 0);
        assert_eq!(depth(&Json::Array(vec![])), 1);
        assert_eq!(depth(&Json::Array(vec![Json::Array(vec![])])), 2);
        let mut value = Json::Array(vec![]);
        for _ in 0..200 {
            value = Json::Array(vec![value]);
        }
        assert!(depth(&value) > 128);
    }

    /// [`write_len`] must predict exactly what [`stringify()`] and [`pretty`]
    /// go on to write, for every kind of node, every indent, and the empty
    /// containers, escapes and non-ASCII text that change a character's
    /// encoded length: a caller charges by this number before building the
    /// text, so if it ever fell short the charge would let through exactly
    /// the unbounded allocation it exists to stop.
    #[test]
    fn write_len_matches_what_stringify_and_pretty_actually_write() {
        let values = [
            Json::Null,
            Json::Bool(true),
            Json::Bool(false),
            Json::Int(-9_223_372_036_854_775_808),
            Json::Float(1.5),
            Json::String(String::new()),
            Json::String("a\"b\\c\u{8}\u{c}\n\r\teA😀\u{7f}".to_owned()),
            Json::Array(vec![]),
            Json::Object(vec![]),
            Json::Array(vec![Json::Int(1), Json::Int(2), Json::Int(3)]),
            Json::Object(vec![
                ("a".to_owned(), Json::Null),
                ("b b".to_owned(), Json::Array(vec![Json::Bool(false)])),
            ]),
            Json::Array(vec![
                Json::Object(vec![("k".to_owned(), Json::String("v".to_owned()))]),
                Json::Array(vec![Json::Array(vec![Json::Null])]),
            ]),
        ];
        for value in &values {
            assert_eq!(
                write_len(value, None),
                stringify(value).len() as u64,
                "{value:?} compact"
            );
            for indent in [1, 2, 4, 16] {
                assert_eq!(
                    write_len(value, Some(indent)),
                    pretty(value, indent).len() as u64,
                    "{value:?} indent {indent}"
                );
            }
        }
    }

    /// [`parse_charged`] with no budget at all behaves exactly like
    /// [`parse`]: charging is threaded through the same reader, not a
    /// second implementation that could drift from it.
    #[test]
    fn parse_charged_agrees_with_parse() {
        for text in [
            r#"{"b": [1, 2.5, true], "a": null}"#,
            "not json",
            "\"a\\ud83d\\ude00b\"",
            "",
        ] {
            let mut counters = RunCounters::new();
            let limits = Limits::default();
            let charged = parse_charged(text, &mut counters, &limits, &mut Control::none());
            let plain = parse(text);
            match (charged, plain) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{text}"),
                (Err(Outcome::Json(a)), Err(b)) => assert_eq!(a, b, "{text}"),
                (charged, plain) => panic!("{text}: {charged:?} vs {plain:?}"),
            }
        }
    }

    /// The parser used to call `std::str::from_utf8` on the whole rest of
    /// the input for every character it read inside a string, making a
    /// document of N bytes take time quadratic in N (roughly four minutes
    /// for a 3.2 MB string over the C ABI, by the outside review's own
    /// measurement). This is a weaker, fast-running version of the same
    /// shape: a string over a million bytes must still parse well inside a
    /// couple of seconds, which the quadratic behaviour could not manage.
    #[test]
    fn a_long_string_parses_in_roughly_linear_time() {
        let text = format!("\"{}\"", "a".repeat(1_500_000));
        let start = std::time::Instant::now();
        let parsed = parse(&text).expect("parses");
        let elapsed = start.elapsed();
        assert_eq!(parsed, Json::String("a".repeat(1_500_000)));
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "a 1.5 MB string took {elapsed:?} to parse; this used to be quadratic"
        );
    }

    /// A charged parse must still be cancellable while it is under way, not
    /// only before it starts or after it finishes: this used to charge the
    /// whole call's steps as one lump before `parse` ran at all, so nothing
    /// polled the cancellation token again until the (possibly very slow)
    /// parse itself returned. A large string is parsed on another thread,
    /// its token is cancelled shortly after the thread starts, and the call
    /// must return quickly with a fault rather than run to completion.
    #[test]
    fn a_charged_parse_can_be_cancelled_while_it_is_under_way() {
        let text = format!("\"{}\"", "a".repeat(20_000_000));
        let token = CancellationToken::new();
        let cancel_from = token.clone();
        let limits = Limits::default();
        let start = std::time::Instant::now();
        let handle = std::thread::spawn(move || {
            let mut counters = RunCounters::new();
            let mut control = Control::new(Some(&cancel_from), None);
            parse_charged(&text, &mut counters, &limits, &mut control)
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        token.cancel();
        let result = handle.join().expect("the parse thread did not panic");
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(Outcome::Fault(_))),
            "expected a cancellation fault, got {result:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "cancellation must be timely, not only checked before or after the parse: {elapsed:?}"
        );
    }
}
