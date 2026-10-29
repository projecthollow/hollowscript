//! Just enough JSON to read `cargo metadata`'s output, with no dependency:
//! a value big enough to represent it, and a parser for it.
//!
//! This is not a general-purpose JSON library: it only needs to round-trip
//! what `cargo metadata --format-version 1` actually writes (objects,
//! arrays, strings, numbers, booleans and null, all UTF-8, no comments), so
//! it does not aim at every corner of the grammar (surrogate pairs in a
//! `\uXXXX` escape, for example, are decoded as the one code point they
//! name, which is all `cargo`'s own output ever needs).

use std::collections::BTreeMap;

/// A JSON value, kept exactly as parsed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    /// Kept as an ordered map so a caller that walks every entry (there is
    /// none here today) would see them in the order the input gave them.
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// The value at `key` if this is an object and has it.
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// The elements if this is an array.
    pub(crate) fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The text if this is a string.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

/// Parses `text` as one JSON value.
///
/// # Errors
///
/// Returns a message naming roughly where the input stopped making sense.
pub(crate) fn parse(text: &str) -> Result<Value, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut pos = 0;
    let value = parse_value(&chars, &mut pos)?;
    skip_whitespace(&chars, &mut pos);
    if pos != chars.len() {
        return Err(format!("trailing input at character {pos}"));
    }
    Ok(value)
}

fn skip_whitespace(chars: &[char], pos: &mut usize) {
    while matches!(chars.get(*pos), Some(c) if c.is_whitespace()) {
        *pos += 1;
    }
}

fn parse_value(chars: &[char], pos: &mut usize) -> Result<Value, String> {
    skip_whitespace(chars, pos);
    match chars.get(*pos) {
        Some('{') => parse_object(chars, pos),
        Some('[') => parse_array(chars, pos),
        Some('"') => parse_string(chars, pos).map(Value::String),
        Some('t') => parse_literal(chars, pos, "true", Value::Bool(true)),
        Some('f') => parse_literal(chars, pos, "false", Value::Bool(false)),
        Some('n') => parse_literal(chars, pos, "null", Value::Null),
        Some(c) if c.is_ascii_digit() || *c == '-' => parse_number(chars, pos),
        Some(c) => Err(format!("unexpected character '{c}' at {pos}")),
        None => Err("unexpected end of input".to_owned()),
    }
}

fn parse_literal(
    chars: &[char],
    pos: &mut usize,
    word: &str,
    value: Value,
) -> Result<Value, String> {
    for expected in word.chars() {
        if chars.get(*pos) != Some(&expected) {
            return Err(format!("expected '{word}' at {pos}"));
        }
        *pos += 1;
    }
    Ok(value)
}

fn parse_number(chars: &[char], pos: &mut usize) -> Result<Value, String> {
    let start = *pos;
    if chars.get(*pos) == Some(&'-') {
        *pos += 1;
    }
    while matches!(chars.get(*pos), Some(c) if c.is_ascii_digit() || *c == '.' || *c == 'e' || *c == 'E' || *c == '+' || *c == '-')
    {
        *pos += 1;
    }
    let text: String = chars.get(start..*pos).unwrap_or_default().iter().collect();
    text.parse::<f64>()
        .map(Value::Number)
        .map_err(|_| format!("invalid number '{text}' at {start}"))
}

fn parse_string(chars: &[char], pos: &mut usize) -> Result<String, String> {
    if chars.get(*pos) != Some(&'"') {
        return Err(format!("expected string at {pos}"));
    }
    *pos += 1;
    let mut out = String::new();
    loop {
        match chars.get(*pos) {
            None => return Err("unterminated string".to_owned()),
            Some('"') => {
                *pos += 1;
                return Ok(out);
            }
            Some('\\') => {
                *pos += 1;
                match chars.get(*pos) {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('u') => {
                        let hex: String = chars
                            .get(*pos + 1..*pos + 5)
                            .map_or_else(String::new, |cs| cs.iter().collect());
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|_| format!("invalid \\u escape at {pos}"))?;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        *pos += 4;
                    }
                    _ => return Err(format!("invalid escape at {pos}")),
                }
                *pos += 1;
            }
            Some(c) => {
                out.push(*c);
                *pos += 1;
            }
        }
    }
}

fn parse_array(chars: &[char], pos: &mut usize) -> Result<Value, String> {
    *pos += 1; // '['
    let mut items = Vec::new();
    skip_whitespace(chars, pos);
    if chars.get(*pos) == Some(&']') {
        *pos += 1;
        return Ok(Value::Array(items));
    }
    loop {
        items.push(parse_value(chars, pos)?);
        skip_whitespace(chars, pos);
        match chars.get(*pos) {
            Some(',') => {
                *pos += 1;
            }
            Some(']') => {
                *pos += 1;
                return Ok(Value::Array(items));
            }
            _ => return Err(format!("expected ',' or ']' at {pos}")),
        }
    }
}

fn parse_object(chars: &[char], pos: &mut usize) -> Result<Value, String> {
    *pos += 1; // '{'
    let mut map = BTreeMap::new();
    skip_whitespace(chars, pos);
    if chars.get(*pos) == Some(&'}') {
        *pos += 1;
        return Ok(Value::Object(map));
    }
    loop {
        skip_whitespace(chars, pos);
        let key = parse_string(chars, pos)?;
        skip_whitespace(chars, pos);
        if chars.get(*pos) != Some(&':') {
            return Err(format!("expected ':' at {pos}"));
        }
        *pos += 1;
        let value = parse_value(chars, pos)?;
        map.insert(key, value);
        skip_whitespace(chars, pos);
        match chars.get(*pos) {
            Some(',') => {
                *pos += 1;
            }
            Some('}') => {
                *pos += 1;
                return Ok(Value::Object(map));
            }
            _ => return Err(format!("expected ',' or '}}' at {pos}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, parse};

    #[test]
    fn parses_the_shapes_cargo_metadata_actually_uses() {
        let value = parse(
            r#"{"packages":[{"name":"a","version":"1.0.0","license":"MIT","license_file":null,"source":null}],"n":1,"b":true,"z":null}"#,
        )
        .unwrap();
        let packages = value.get("packages").unwrap().as_array().unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].get("name").unwrap().as_str(), Some("a"));
        assert_eq!(packages[0].get("license_file").unwrap(), &Value::Null);
        assert_eq!(value.get("b").unwrap(), &Value::Bool(true));
    }

    #[test]
    fn parses_escapes_and_unicode() {
        let value = parse(r#""a\"b\\cé""#).unwrap();
        assert_eq!(value.as_str(), Some("a\"b\\c\u{e9}"));
    }

    #[test]
    fn refuses_trailing_and_truncated_input() {
        assert!(parse("{} extra").is_err());
        assert!(parse("{\"a\":").is_err());
    }
}
