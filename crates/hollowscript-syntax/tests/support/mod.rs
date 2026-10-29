//! Test-only support code shared by the integration tests: corpus discovery,
//! a tiny hand-rolled JSON reader for `.expected.json`, a deterministic
//! mutation engine shared by every fuzz-style test, and the AST-shape
//! comparison the formatter's round-trip test uses (no external crate is
//! used anywhere in this repository).
//!
//! Each `tests/*.rs` file is its own crate, and not every one of them uses
//! every helper here (`corpus.rs` and `golden.rs` have no use for the
//! mutation engine, for one); rather than duplicate this module once per
//! crate that needs a subset of it, or split it into several files each
//! included by only some of them, it stays one module and disables the two
//! lints that a helper unused by a particular caller would otherwise trip in
//! that caller's own build.
#![allow(dead_code, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};

mod ast_shape;
#[allow(unused_imports)] // only `format_roundtrip.rs` compares two trees
pub(crate) use ast_shape::normalize_file;

pub(crate) fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

/// Every `.hws` file under `dir`, sorted for a stable order.
pub(crate) fn walk_hws(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_into(dir, &mut out);
    out.sort();
    out
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(&path, out);
        } else if path.extension().is_some_and(|e| e == "hws") {
            out.push(path);
        }
    }
}

/// A minimal JSON value, enough to read `corpus/**/*.expected.json`
/// (`FORMAT.md`): objects, arrays, strings, numbers, booleans and null.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }
}

/// Parses a complete JSON document. Returns `None` on any malformed input
/// rather than panicking: this is a test helper, and a malformed fixture
/// should be reported as a clear test failure, not a crash.
pub(crate) fn parse_json(text: &str) -> Option<Json> {
    let mut p = JsonParser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.parse_value()?;
    p.skip_ws();
    Some(value)
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl JsonParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self) -> Option<Json> {
        self.skip_ws();
        match self.peek()? {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(Json::String),
            b't' => self.parse_lit("true", Json::Bool(true)),
            b'f' => self.parse_lit("false", Json::Bool(false)),
            b'n' => self.parse_lit("null", Json::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_lit(&mut self, lit: &str, value: Json) -> Option<Json> {
        let end = self.pos + lit.len();
        if self.bytes.get(self.pos..end) == Some(lit.as_bytes()) {
            self.pos = end;
            Some(value)
        } else {
            None
        }
    }

    fn parse_number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(self.bytes.get(start..self.pos)?).ok()?;
        text.parse::<f64>().ok().map(Json::Number)
    }

    fn parse_string(&mut self) -> Option<String> {
        if self.peek() != Some(b'"') {
            return None;
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.pos += 1;
                    match self.peek()? {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            let hex =
                                std::str::from_utf8(self.bytes.get(self.pos + 1..self.pos + 5)?)
                                    .ok()?;
                            let code = u32::from_str_radix(hex, 16).ok()?;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            self.pos += 4;
                        }
                        _ => return None,
                    }
                    self.pos += 1;
                }
                _ => {
                    // Fall back to scanning a run of plain bytes for speed and
                    // to stay valid UTF-8: re-decode as a str slice.
                    let rest = std::str::from_utf8(self.bytes.get(self.pos..)?).ok()?;
                    let ch = rest.chars().next()?;
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn parse_array(&mut self) -> Option<Json> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Some(Json::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek()? {
                b',' => {
                    self.pos += 1;
                    self.skip_ws();
                }
                b']' => {
                    self.pos += 1;
                    return Some(Json::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn parse_object(&mut self) -> Option<Json> {
        self.pos += 1; // '{'
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Some(Json::Object(fields));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return None;
            }
            self.pos += 1;
            let value = self.parse_value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.peek()? {
                b',' => {
                    self.pos += 1;
                }
                b'}' => {
                    self.pos += 1;
                    return Some(Json::Object(fields));
                }
                _ => return None,
            }
        }
    }
}

// ---- a small, deterministic mutation engine, shared by every fuzz-style
// integration test in this crate (the parser's own `fuzz.rs` and the
// formatter's `format_fuzz.rs` and `format_roundtrip.rs`) ----------------
//
// The only source of "randomness" is the tiny linear congruential generator
// below, seeded explicitly by every caller, so a failure is always
// reproducible: the same seed produces the same sequence of mutated
// programs on every run, on every machine. No external crate is used (the
// workspace allows none in a crate that touches untrusted input, and these
// tests exercise exactly that crate).

/// A minimal linear congruential generator (the constants are Knuth's, from
/// "The Art of Computer Programming" volume 2, also used by `glibc`'s
/// `drand48`-family generators). It is not cryptographically meaningful; it
/// only needs to be fast, deterministic and reasonably well mixed.
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) const fn new(seed: u64) -> Lcg {
        Lcg(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// A value in `0..n`, or 0 if `n` is 0.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        // The high bits of an LCG are much better distributed than the low
        // bits, which is why this reads from the top of the word.
        let r = self.next_u64() >> 32;
        (r % n as u64) as usize
    }

    pub(crate) fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    /// A `usize` weighted so that small counts are common: `1 + below(bound)`.
    pub(crate) fn count(&mut self, bound: usize) -> usize {
        1 + self.below(bound)
    }
}

/// Code points that are individually interesting to insert or substitute:
/// delimiters, escape-adjacent characters, whitespace variants section 1.1
/// treats specially, and a few plain letters and digits so not every
/// mutation immediately produces an unrecognised character.
pub(crate) const INTERESTING_CHARS: &[char] = &[
    '"',
    '\'',
    '`',
    '{',
    '}',
    '(',
    ')',
    '[',
    ']',
    '\\',
    '$',
    '\n',
    '\r',
    '\t',
    '.',
    ',',
    ':',
    '?',
    '+',
    '-',
    '*',
    '/',
    '=',
    '<',
    '>',
    '!',
    '_',
    'a',
    'A',
    'z',
    '0',
    '9',
    ' ',
    '\u{feff}',
    '\u{202e}',
    '\u{2028}',
    '\u{0}',
    '\u{7f}',
    '\u{1f600}',
];

/// Snippets that are interesting to splice in whole, because on their own
/// they are meaningful tokens (a keyword, an operator, a delimiter run) more
/// likely to land the mutation on a grammar boundary than a single
/// character would.
pub(crate) const INTERESTING_SNIPPETS: &[&str] = &[
    "fn", "if", "else", "match", "return", "constant", "variable", "for", "while", "uses",
    "import", "effect", "not", "and", "or", "none", "true", "false", "record", "enum", "export",
    "..", "?", "${", "}", "\"", "`", "((((", "))))", "[[[[", "]]]]",
];

pub(crate) fn mutate_char_level(chars: &mut Vec<char>, rng: &mut Lcg) {
    if chars.is_empty() {
        match rng.below(2) {
            0 => chars.push('1'),
            _ => chars.push(INTERESTING_CHARS[rng.below(INTERESTING_CHARS.len())]),
        }
        return;
    }
    match rng.below(5) {
        0 => {
            let i = rng.below(chars.len());
            chars.remove(i);
        }
        1 => {
            let i = rng.below(chars.len() + 1);
            chars.insert(i, INTERESTING_CHARS[rng.below(INTERESTING_CHARS.len())]);
        }
        2 => {
            let i = rng.below(chars.len());
            chars[i] = INTERESTING_CHARS[rng.below(INTERESTING_CHARS.len())];
        }
        3 => {
            let i = rng.below(chars.len());
            let c = chars[i];
            chars.insert(i, c);
        }
        _ => {
            if chars.len() >= 2 {
                let i = rng.below(chars.len() - 1);
                chars.swap(i, i + 1);
            }
        }
    }
}

pub(crate) fn mutate_splice_snippet(chars: &mut Vec<char>, rng: &mut Lcg) {
    let snippet = INTERESTING_SNIPPETS[rng.below(INTERESTING_SNIPPETS.len())];
    let i = rng.below(chars.len() + 1);
    for (offset, c) in snippet.chars().enumerate() {
        chars.insert(i + offset, c);
    }
}

/// Deletes, duplicates, swaps or replaces one whole token's text, found by
/// actually lexing the (possibly already mutated) source, so the edit lands
/// on a real token boundary rather than the middle of one.
pub(crate) fn mutate_token_level(text: &str, rng: &mut Lcg) -> String {
    let Ok(source) =
        hollowscript_core::Source::new(hollowscript_core::SourceId(0), "fuzz.hws", text)
    else {
        return text.to_string();
    };
    let lexed = hollowscript_syntax::lex(&source, &mut hollowscript_core::Control::none());
    // Exclude the trailing `Eof`/`Term` so there is always real text at the
    // chosen span.
    let usable = lexed.tokens.len().saturating_sub(1).min(lexed.tokens.len());
    if usable == 0 {
        return text.to_string();
    }
    let text_end = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let slice = |a: u32, b: u32| text.get(a as usize..b as usize).unwrap_or("");
    match rng.below(4) {
        0 => {
            let t = lexed.tokens[rng.below(usable)];
            format!("{}{}", slice(0, t.span.start), slice(t.span.end, text_end))
        }
        1 => {
            let t = lexed.tokens[rng.below(usable)];
            let piece = slice(t.span.start, t.span.end);
            format!(
                "{}{piece}{piece}{}",
                slice(0, t.span.end),
                slice(t.span.end, text_end)
            )
        }
        2 if usable >= 2 => {
            let i = rng.below(usable);
            let j = rng.below(usable);
            let (a, b) = if lexed.tokens[i].span.start <= lexed.tokens[j].span.start {
                (lexed.tokens[i], lexed.tokens[j])
            } else {
                (lexed.tokens[j], lexed.tokens[i])
            };
            format!(
                "{}{}{}{}{}",
                slice(0, a.span.start),
                slice(b.span.start, b.span.end),
                slice(a.span.end, b.span.start),
                slice(a.span.start, a.span.end),
                slice(b.span.end, text_end)
            )
        }
        _ => {
            let t = lexed.tokens[rng.below(usable)];
            let replacement = INTERESTING_SNIPPETS[rng.below(INTERESTING_SNIPPETS.len())];
            format!(
                "{}{replacement}{}",
                slice(0, t.span.start),
                slice(t.span.end, text_end)
            )
        }
    }
}

pub(crate) fn mutate_truncate(text: &str, rng: &mut Lcg) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let mut cut = rng.below(text.len() + 1);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    // Occasionally truncate from the front instead of the back, so an
    // opening `uses`/uses-then-import block can be knocked out too.
    if rng.one_in(4) {
        let mut start = rng.below(text.len() + 1);
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        let start = start.min(cut);
        text.get(start..cut).unwrap_or("").to_string()
    } else {
        text.get(..cut).unwrap_or("").to_string()
    }
}

/// Builds one mutated program from `base`, applying between one and four
/// mutations so later iterations can compound edits.
pub(crate) fn mutate(base: &str, rng: &mut Lcg) -> String {
    let mut text = base.to_string();
    let rounds = rng.count(4);
    for _ in 0..rounds {
        match rng.below(4) {
            0 => {
                let mut chars: Vec<char> = text.chars().collect();
                mutate_char_level(&mut chars, rng);
                text = chars.into_iter().collect();
            }
            1 => {
                let mut chars: Vec<char> = text.chars().collect();
                mutate_splice_snippet(&mut chars, rng);
                text = chars.into_iter().collect();
            }
            2 => text = mutate_token_level(&text, rng),
            _ => text = mutate_truncate(&text, rng),
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{Json, parse_json};

    #[test]
    fn reads_objects_arrays_and_scalars() {
        let v = parse_json(r#"{"a": 1, "b": [true, false, null, "x\ny"], "c": -2.5e3}"#).unwrap();
        assert_eq!(v.get("a").unwrap().as_f64(), Some(1.0));
        let arr = v.get("b").unwrap().as_array().unwrap();
        assert_eq!(arr[0], Json::Bool(true));
        assert_eq!(arr[3].as_str(), Some("x\ny"));
        assert_eq!(v.get("c").unwrap().as_f64(), Some(-2500.0));
    }

    #[test]
    fn malformed_json_is_none_not_a_panic() {
        assert_eq!(parse_json("{"), None);
        assert_eq!(parse_json("[1,"), None);
        assert_eq!(parse_json("\"unterminated"), None);
        assert_eq!(parse_json(""), None);
    }
}
