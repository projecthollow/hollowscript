//! A minimal JSON value, parser and writer, just enough for JSON-RPC
//! envelopes and LSP payloads, with no dependency.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A JSON value. Object keys are kept in a sorted map, which is fine for
/// this crate's own purposes: nothing here depends on a request's key order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    /// Kept as text so an integer id round-trips exactly.
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// A number as an `i64`, for the line and character of a position. A
    /// number that is not a whole one in range answers `None`, which a
    /// caller treats as no position rather than as zero.
    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub(crate) fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(n),
            Value::String(s) => {
                out.push('"');
                escape(s, out);
                out.push('"');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (key, value)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    escape(key, out);
                    out.push_str("\":");
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    pub(crate) fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

/// Builds a JSON object from `(key, value)` pairs, for constructing
/// responses without a macro.
pub(crate) fn obj(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

pub(crate) fn s(text: impl Into<String>) -> Value {
    Value::String(text.into())
}

pub(crate) fn n(number: i64) -> Value {
    Value::Number(number.to_string())
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// A JSON syntax error, with the byte offset of the first problem.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParseError {
    pub(crate) offset: usize,
}

/// Parses exactly one JSON value, with optional surrounding whitespace and
/// nothing else after it.
///
/// # Errors
///
/// Returns [`ParseError`] at the first byte that does not fit.
pub(crate) fn parse(text: &str) -> Result<Value, ParseError> {
    let bytes = text.as_bytes();
    let mut pos = skip_ws(bytes, 0);
    let (value, after) = parse_value(bytes, pos)?;
    pos = skip_ws(bytes, after);
    if pos == bytes.len() {
        Ok(value)
    } else {
        Err(ParseError { offset: pos })
    }
}

fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while matches!(bytes.get(pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        pos += 1;
    }
    pos
}

fn parse_value(bytes: &[u8], pos: usize) -> Result<(Value, usize), ParseError> {
    match bytes.get(pos) {
        Some(b'n') => literal(bytes, pos, "null", Value::Null),
        Some(b't') => literal(bytes, pos, "true", Value::Bool(true)),
        Some(b'f') => literal(bytes, pos, "false", Value::Bool(false)),
        Some(b'"') => parse_string(bytes, pos).map(|(s, p)| (Value::String(s), p)),
        Some(b'[') => parse_array(bytes, pos),
        Some(b'{') => parse_object(bytes, pos),
        Some(b'-' | b'0'..=b'9') => parse_number(bytes, pos),
        _ => Err(ParseError { offset: pos }),
    }
}

fn literal(
    bytes: &[u8],
    pos: usize,
    text: &str,
    value: Value,
) -> Result<(Value, usize), ParseError> {
    let end = pos + text.len();
    if bytes.get(pos..end) == Some(text.as_bytes()) {
        Ok((value, end))
    } else {
        Err(ParseError { offset: pos })
    }
}

fn parse_number(bytes: &[u8], start: usize) -> Result<(Value, usize), ParseError> {
    let mut pos = start;
    if bytes.get(pos) == Some(&b'-') {
        pos += 1;
    }
    let digits_start = pos;
    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    if pos == digits_start {
        return Err(ParseError { offset: start });
    }
    if bytes.get(pos) == Some(&b'.') {
        pos += 1;
        let frac_start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if pos == frac_start {
            return Err(ParseError { offset: start });
        }
    }
    if matches!(bytes.get(pos), Some(b'e' | b'E')) {
        pos += 1;
        if matches!(bytes.get(pos), Some(b'+' | b'-')) {
            pos += 1;
        }
        let exp_start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if pos == exp_start {
            return Err(ParseError { offset: start });
        }
    }
    let slice = bytes.get(start..pos).ok_or(ParseError { offset: start })?;
    let text = std::str::from_utf8(slice).map_err(|_| ParseError { offset: start })?;
    Ok((Value::Number(text.to_owned()), pos))
}

fn parse_string(bytes: &[u8], start: usize) -> Result<(String, usize), ParseError> {
    let mut pos = start + 1; // skip the opening quote
    let mut out = String::new();
    loop {
        match bytes.get(pos) {
            None => return Err(ParseError { offset: pos }),
            Some(b'"') => return Ok((out, pos + 1)),
            Some(b'\\') => {
                let escaped = *bytes.get(pos + 1).ok_or(ParseError { offset: pos })?;
                match escaped {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'u' => {
                        let hex = bytes
                            .get(pos + 2..pos + 6)
                            .ok_or(ParseError { offset: pos })?;
                        let text =
                            std::str::from_utf8(hex).map_err(|_| ParseError { offset: pos })?;
                        let code = u32::from_str_radix(text, 16)
                            .map_err(|_| ParseError { offset: pos })?;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        pos += 4;
                    }
                    _ => return Err(ParseError { offset: pos }),
                }
                pos += 2;
            }
            Some(&b) => {
                let ch_len = utf8_len(b);
                let end = pos + ch_len;
                let slice = bytes.get(pos..end).ok_or(ParseError { offset: pos })?;
                out.push_str(std::str::from_utf8(slice).map_err(|_| ParseError { offset: pos })?);
                pos = end;
            }
        }
    }
}

fn utf8_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn parse_array(bytes: &[u8], start: usize) -> Result<(Value, usize), ParseError> {
    let mut pos = skip_ws(bytes, start + 1);
    let mut items = Vec::new();
    if bytes.get(pos) == Some(&b']') {
        return Ok((Value::Array(items), pos + 1));
    }
    loop {
        let (value, after) = parse_value(bytes, pos)?;
        items.push(value);
        pos = skip_ws(bytes, after);
        match bytes.get(pos) {
            Some(b',') => pos = skip_ws(bytes, pos + 1),
            Some(b']') => return Ok((Value::Array(items), pos + 1)),
            _ => return Err(ParseError { offset: pos }),
        }
    }
}

fn parse_object(bytes: &[u8], start: usize) -> Result<(Value, usize), ParseError> {
    let mut pos = skip_ws(bytes, start + 1);
    let mut map = BTreeMap::new();
    if bytes.get(pos) == Some(&b'}') {
        return Ok((Value::Object(map), pos + 1));
    }
    loop {
        if bytes.get(pos) != Some(&b'"') {
            return Err(ParseError { offset: pos });
        }
        let (key, after_key) = parse_string(bytes, pos)?;
        pos = skip_ws(bytes, after_key);
        if bytes.get(pos) != Some(&b':') {
            return Err(ParseError { offset: pos });
        }
        pos = skip_ws(bytes, pos + 1);
        let (value, after_value) = parse_value(bytes, pos)?;
        map.insert(key, value);
        pos = skip_ws(bytes, after_value);
        match bytes.get(pos) {
            Some(b',') => pos = skip_ws(bytes, pos + 1),
            Some(b'}') => return Ok((Value::Object(map), pos + 1)),
            _ => return Err(ParseError { offset: pos }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, parse};

    #[test]
    fn scalars_round_trip() {
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(
            parse(" -12.5e1 ").unwrap(),
            Value::Number("-12.5e1".to_owned())
        );
        assert_eq!(
            parse(r#""a\nbA""#).unwrap(),
            Value::String("a\nbA".to_owned())
        );
    }

    #[test]
    fn nested_structures_round_trip() {
        let text = r#"{"id": 1, "method": "initialize", "params": {"a": [1, 2, "x"]}}"#;
        let value = parse(text).unwrap();
        assert_eq!(
            value.get("method").and_then(Value::as_str),
            Some("initialize")
        );
        let arr = value.get("params").and_then(|p| p.get("a")).unwrap();
        assert_eq!(arr.to_json(), "[1,2,\"x\"]");
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(parse("").is_err());
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse("\"unterminated").is_err());
        assert!(parse("truex").is_err());
        assert!(parse("1 2").is_err());
    }

    #[test]
    fn writer_escapes_control_characters() {
        assert_eq!(
            Value::String("a\"b\n".to_owned()).to_json(),
            "\"a\\\"b\\n\""
        );
    }
}
