//! A small JSON reader, enough for `corpus/FORMAT.md`'s expected files.
//!
//! This crate may depend on no external crate (`xtask graph`), and the JSON
//! it has to read is a fixed, hand-written shape, so a reader of a few
//! hundred lines is the whole requirement. It accepts the JSON grammar
//! (RFC 8259) with two deliberate limits that no corpus file needs: a number
//! is kept as its source text and converted on demand, and nesting is capped
//! so that a malformed file cannot exhaust the stack.

use std::collections::BTreeMap;
use std::fmt;

/// The maximum nesting depth of a document. The deepest corpus file nests
/// four levels; 64 leaves room without letting a hostile file recurse.
const MAX_DEPTH: usize = 64;

/// One JSON value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, kept as its source text.
    Number(String),
    /// A string, with its escapes already resolved.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object. Duplicate keys keep the last.
    Object(BTreeMap<String, Json>),
}

impl Json {
    /// The value at `key` of an object, or `None` for anything else.
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// This value as a string slice, if it is a string.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    /// This value as a bool, if it is one.
    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// This value as an `i64`, if it is a number that fits.
    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// This value's elements, if it is an array.
    pub(crate) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Whether this value is `null`.
    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
}

/// Why a document could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JsonError {
    /// What went wrong.
    pub message: String,
    /// The byte offset the reader had reached.
    pub offset: usize,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

/// Reads one JSON document, which must be the whole of `text`.
pub(crate) fn parse(text: &str) -> Result<Json, JsonError> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
    };
    reader.skip_space();
    let value = reader.value(0)?;
    reader.skip_space();
    if reader.at != reader.bytes.len() {
        return Err(reader.error("trailing text after the document"));
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn error(&self, message: &str) -> JsonError {
        JsonError {
            message: message.to_owned(),
            offset: self.at,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonError> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", byte as char)))
        }
    }

    fn literal(&mut self, word: &str) -> Result<(), JsonError> {
        if self
            .bytes
            .get(self.at..)
            .is_some_and(|rest| rest.starts_with(word.as_bytes()))
        {
            self.at += word.len();
            Ok(())
        } else {
            Err(self.error(&format!("expected '{word}'")))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        match self.peek() {
            Some(b'n') => self.literal("null").map(|()| Json::Null),
            Some(b't') => self.literal("true").map(|()| Json::Bool(true)),
            Some(b'f') => self.literal("false").map(|()| Json::Bool(false)),
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            None | Some(_) => Err(self.error("expected a value")),
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_space();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_space();
            items.push(self.value(depth + 1)?);
            self.skip_space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.expect(b'{')?;
        let mut map = BTreeMap::new();
        self.skip_space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_space();
            let key = self.string()?;
            self.skip_space();
            self.expect(b':')?;
            self.skip_space();
            let value = self.value(depth + 1)?;
            map.insert(key, value);
            self.skip_space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(map));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        if self.at == start {
            return Err(self.error("expected a number"));
        }
        let text = self
            .bytes
            .get(start..self.at)
            .ok_or_else(|| self.error("a number is out of range"))
            .and_then(|bytes| {
                std::str::from_utf8(bytes).map_err(|_| self.error("a number is not text"))
            })?;
        Ok(Json::Number(text.to_owned()))
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("unterminated string"));
            };
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                0x00..=0x1f => return Err(self.error("a control character in a string")),
                _ => {
                    let rest = self
                        .bytes
                        .get(self.at..)
                        .ok_or_else(|| self.error("unterminated string"))
                        .and_then(|bytes| {
                            std::str::from_utf8(bytes)
                                .map_err(|_| self.error("a string is not valid UTF-8"))
                        })?;
                    let c = rest
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("unterminated string"))?;
                    out.push(c);
                    self.at += c.len_utf8();
                }
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), JsonError> {
        let Some(byte) = self.peek() else {
            return Err(self.error("an escape at the end of the text"));
        };
        self.at += 1;
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
            out.push(c);
            return Ok(());
        }
        if byte != b'u' {
            return Err(self.error("an unknown escape"));
        }
        let high = self.hex4()?;
        // A surrogate pair is written as two \u escapes; anything else is a
        // lone code point.
        if (0xd800..0xdc00).contains(&high) {
            if !self
                .bytes
                .get(self.at..)
                .is_some_and(|rest| rest.starts_with(b"\\u"))
            {
                return Err(self.error("a high surrogate with no low surrogate"));
            }
            self.at += 2;
            let low = self.hex4()?;
            if !(0xdc00..0xe000).contains(&low) {
                return Err(self.error("a high surrogate followed by a non-surrogate"));
            }
            let combined = 0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
            let c = char::from_u32(combined).ok_or_else(|| self.error("not a code point"))?;
            out.push(c);
            return Ok(());
        }
        let c = char::from_u32(high).ok_or_else(|| self.error("not a code point"))?;
        out.push(c);
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let end = self.at.saturating_add(4);
        let digits = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| self.error("a short \\u escape"))?;
        let text =
            std::str::from_utf8(digits).map_err(|_| self.error("a \\u escape is not text"))?;
        let value =
            u32::from_str_radix(text, 16).map_err(|_| self.error("a \\u escape is not hex"))?;
        self.at = end;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{Json, parse};

    #[test]
    fn reads_the_shape_an_expected_file_uses() {
        let text = r#"{
            "title": "A case",
            "spec": "4.8",
            "parse": [],
            "check": [{ "severity": "error", "line": 3, "code": "HS0301" }],
            "run": { "ok": false, "stage": "check", "output": ["a"], "diagnostics": [] }
        }"#;
        let value = parse(text).expect("the document reads");
        assert_eq!(value.get("title").and_then(Json::as_str), Some("A case"));
        let check = value.get("check").and_then(Json::as_array).expect("check");
        assert_eq!(check.len(), 1);
        assert_eq!(check[0].get("line").and_then(Json::as_i64), Some(3));
        let run = value.get("run").expect("run");
        assert_eq!(run.get("ok").and_then(Json::as_bool), Some(false));
        assert_eq!(
            run.get("output")
                .and_then(Json::as_array)
                .and_then(|a| a.first())
                .and_then(Json::as_str),
            Some("a")
        );
    }

    #[test]
    fn reads_every_escape_including_a_surrogate_pair() {
        let value = parse(r#""a\n\t\"\\\/\u0041\ud83d\ude00""#).expect("the string reads");
        assert_eq!(value.as_str(), Some("a\n\t\"\\/A\u{1f600}"));
    }

    #[test]
    fn null_and_numbers_read() {
        assert!(parse("null").expect("null reads").is_null());
        assert_eq!(parse("-12").expect("reads").as_i64(), Some(-12));
        assert_eq!(parse("1.5e3").expect("reads"), Json::Number("1.5e3".into()));
    }

    #[test]
    fn malformed_text_is_an_error_not_a_panic() {
        for bad in [
            "",
            "{",
            "[1,",
            "\"a",
            "tru",
            "{\"a\"}",
            "[1]]",
            "\"\\q\"",
            "\"\\ud800\"",
        ] {
            assert!(parse(bad).is_err(), "{bad} should not read");
        }
    }

    #[test]
    fn deep_nesting_is_refused_rather_than_recursed() {
        let deep = "[".repeat(200);
        assert!(parse(&deep).is_err());
    }
}
