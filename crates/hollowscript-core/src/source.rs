//! Source text, spans and the mapping from byte offsets to lines and columns.
//!
//! The rules are those of section 1.1 of the specification. A span is a
//! half-open range of byte offsets that counts a leading byte order mark. A
//! line starts after each LF (a CR LF pair is one terminator, and a lone CR is
//! not a terminator). A column is 1 plus the number of code points between the
//! start of the line and the position, and the byte order mark is not counted.

use crate::diagnostic::{Code, Diagnostic, Location};
use std::fmt;
use std::sync::Arc;

/// The largest source file, in bytes (section 1.1).
pub const MAX_SOURCE_BYTES: usize = 4_194_304;

/// The canonical path of a module, or the name a host gave a source.
pub type FileName = Arc<str>;

/// A half-open range `[start, end)` of byte offsets into one source.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    /// The offset of the first byte.
    pub start: u32,
    /// The offset one past the last byte. Never less than `start`.
    pub end: u32,
}

impl Span {
    /// A span from `start` to `end`. An `end` before `start` is raised to `start`.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Span {
        Span {
            start,
            end: if end < start { start } else { end },
        }
    }

    /// The empty span at `offset`.
    #[must_use]
    pub const fn empty(offset: u32) -> Span {
        Span {
            start: offset,
            end: offset,
        }
    }

    /// A span from byte offsets held as `usize`, saturating at `u32::MAX`.
    #[must_use]
    pub fn from_usize(start: usize, end: usize) -> Span {
        let clamp = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Span::new(clamp(start), clamp(end))
    }

    /// The number of bytes covered.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Whether the span covers no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The smallest span that covers both spans.
    #[must_use]
    pub fn cover(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// The text this span covers in `text`, or `None` when it is out of range
    /// or does not fall on character boundaries.
    #[must_use]
    pub fn slice(self, text: &str) -> Option<&str> {
        text.get(self.start as usize..self.end as usize)
    }
}

/// A 1-based line and column.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineCol {
    /// The line, 1 for the first.
    pub line: u32,
    /// The column in code points, 1 for the first.
    pub column: u32,
}

/// The identity of a source within one program: its index in load order.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub u32);

/// The start offsets of the lines of a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineIndex {
    /// The offset at which each line starts. Line 1 starts at 0 and every
    /// following entry is the offset just after an LF.
    starts: Vec<u32>,
    /// The length of a leading byte order mark, which is not counted in columns.
    bom_len: u32,
}

const BOM: &str = "\u{feff}";

impl LineIndex {
    /// Builds the index of `text`.
    #[must_use]
    pub fn new(text: &str) -> LineIndex {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(u32::try_from(i + 1).unwrap_or(u32::MAX));
            }
        }
        let bom_len = if text.starts_with(BOM) { 3 } else { 0 };
        LineIndex { starts, bom_len }
    }

    /// The number of lines. A text that ends with a line terminator has an
    /// empty last line, so `"a\n"` has two lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The offset at which `line` (1-based) starts.
    #[must_use]
    pub fn line_start(&self, line: u32) -> Option<u32> {
        let index = usize::try_from(line.checked_sub(1)?).ok()?;
        self.starts.get(index).copied()
    }

    /// The line and column of `offset` in `text`, which must be the text the
    /// index was built from. An offset past the end maps to the end, and an
    /// offset inside a character counts that character.
    ///
    /// This counts code points from the start of the line on every call, so
    /// resolving many positions spread across one long line costs time
    /// proportional to the square of how many there are. A caller that
    /// resolves many positions in non-decreasing offset order, such as a
    /// lexer or parser locating each diagnostic as it finds it, should use
    /// [`LineIndex::resolve`] with one [`PositionCursor`] instead.
    #[must_use]
    pub fn position(&self, text: &str, offset: u32) -> LineCol {
        let offset = offset.min(u32::try_from(text.len()).unwrap_or(u32::MAX));
        let line_index = self.line_index_of(offset);
        let line = u32::try_from(line_index + 1).unwrap_or(u32::MAX);
        let origin = self.line_origin(line_index);
        let column = if offset <= origin {
            1
        } else {
            count_chars(text, origin, offset).saturating_add(1)
        };
        LineCol { line, column }
    }

    /// The 0-based index into `starts` of the line `offset` falls on.
    fn line_index_of(&self, offset: u32) -> usize {
        self.starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1)
    }

    /// The start of line `line_index` (see [`LineIndex::line_index_of`]),
    /// with the byte order mark excluded from the first line as columns do.
    fn line_origin(&self, line_index: usize) -> u32 {
        let start = self.starts.get(line_index).copied().unwrap_or(0);
        if line_index == 0 {
            start.max(self.bom_len)
        } else {
            start
        }
    }

    /// The line and column of `offset` in `text` (the text the index was
    /// built from), like [`LineIndex::position`], but resuming the column
    /// count from `cursor`'s last position instead of the start of the line
    /// when `offset` is on the same line and at or after it.
    ///
    /// The result is identical to [`LineIndex::position`] for every `offset`
    /// and every history of prior calls: an offset that is not at or after
    /// the cursor's last one, or that falls on a different line, is resolved
    /// exactly as `position` would, from the start of its line, just without
    /// reusing the cursor's count. Only a caller that resolves offsets in
    /// non-decreasing order gets the benefit, and every call, whatever order
    /// it comes in, leaves `cursor` updated for the next one.
    ///
    /// This is what keeps locating every diagnostic of a lex or parse pass
    /// to time proportional to the size of the file plus the number of
    /// diagnostics, rather than quadratic in a file with many diagnostics on
    /// one long line (section 19.1 forbids exhausting the host on any input
    /// within the limits of section 1.10, and a source at the size limit
    /// with one lexical error every few bytes is such an input).
    #[must_use]
    pub fn resolve(&self, text: &str, offset: u32, cursor: &mut PositionCursor) -> LineCol {
        let offset = offset.min(u32::try_from(text.len()).unwrap_or(u32::MAX));
        let line_index = self.line_index_of(offset);
        let origin = self.line_origin(line_index);
        let column = if offset <= origin {
            1
        } else if let Some((from, from_column)) = cursor.resume_point(line_index, offset, text) {
            from_column.saturating_add(count_chars(text, from, offset))
        } else {
            count_chars(text, origin, offset).saturating_add(1)
        };
        cursor.line_index = line_index;
        cursor.last = Some((offset, column));
        LineCol {
            line: u32::try_from(line_index + 1).unwrap_or(u32::MAX),
            column,
        }
    }

    /// The offset of a 1-based line and column, or `None` when the line does
    /// not exist or the column is past the end of the line.
    #[must_use]
    pub fn offset_of(&self, text: &str, at: LineCol) -> Option<u32> {
        let start = self.line_start(at.line)?;
        let origin = if at.line == 1 {
            start.max(self.bom_len)
        } else {
            start
        };
        let end = self
            .line_start(at.line.checked_add(1)?)
            .map_or(text.len(), |e| e as usize);
        let line_text = text.get(origin as usize..end)?;
        let skip = usize::try_from(at.column.checked_sub(1)?).ok()?;
        let mut chars = line_text.char_indices();
        let offset = match chars.nth(skip) {
            Some((i, _)) => (origin as usize) + i,
            None if skip == line_text.chars().count() => end,
            None => return None,
        };
        u32::try_from(offset).ok()
    }
}

/// The number of code points of `text` in the half-open range `[from, to)`.
/// `from` must be a char boundary (a line origin always is; so is any
/// `PositionCursor`'s last offset, since it is only ever set from one). `to`
/// need not be: an offset inside a character counts that character, matching
/// [`LineIndex::position`]'s own tolerance of an offset that lands inside one.
fn count_chars(text: &str, from: u32, to: u32) -> u32 {
    let slice = text.get(from as usize..).unwrap_or("");
    let chars = slice
        .char_indices()
        .take_while(|(i, _)| (from as usize) + i < to as usize)
        .count();
    u32::try_from(chars).unwrap_or(u32::MAX)
}

/// A cursor for [`LineIndex::resolve`], which lets it resume counting a
/// position's column from the last position resolved instead of from the
/// start of the line. See `resolve`'s own documentation for what this buys a
/// caller and what it costs one that does not fit its assumptions.
///
/// A cursor starts before any position has been resolved. It belongs to one
/// [`LineIndex`] and one `text`; using it with a different one gives a
/// meaningless, though never incorrect in a way that panics, result.
#[derive(Copy, Clone, Debug, Default)]
pub struct PositionCursor {
    line_index: usize,
    last: Option<(u32, u32)>,
}

impl PositionCursor {
    /// A fresh cursor, equivalent to [`PositionCursor::default`].
    #[must_use]
    pub fn new() -> PositionCursor {
        PositionCursor::default()
    }

    /// The `(offset, column)` to resume counting from for a query at
    /// `line_index`/`offset`, or `None` when the last position resolved does
    /// not let `resolve` reuse it: there is not one yet, it was on a
    /// different line, it was after `offset`, or (defensively; not reachable
    /// for an offset `resolve` itself produced) it is not a char boundary of
    /// `text`.
    fn resume_point(&self, line_index: usize, offset: u32, text: &str) -> Option<(u32, u32)> {
        let (from, from_column) = self.last?;
        if line_index == self.line_index && from <= offset && text.is_char_boundary(from as usize) {
            Some((from, from_column))
        } else {
            None
        }
    }
}

/// A problem with the bytes of a source. It is turned into the diagnostic
/// HS0001 or HS0002 by [`SourceError::to_diagnostic`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// The source is longer than [`MAX_SOURCE_BYTES`]. Checked before anything else.
    TooLarge {
        /// The length in bytes.
        size: usize,
    },
    /// The bytes are not valid UTF-8.
    InvalidUtf8 {
        /// The offset of the first offending byte.
        offset: usize,
        /// The position of that byte, computed over the valid prefix.
        at: LineCol,
    },
}

impl SourceError {
    /// The diagnostic that reports this problem for the file called `file`.
    #[must_use]
    pub fn to_diagnostic(&self, file: &FileName) -> Diagnostic {
        match self {
            SourceError::TooLarge { size } => Diagnostic::new(
                Code::HS0002,
                &[("size", &size), ("limit", &MAX_SOURCE_BYTES)],
            )
            .with_location(Location {
                file: file.clone(),
                span: Span::empty(0),
                line: 1,
                column: 1,
            }),
            SourceError::InvalidUtf8 { offset, at } => {
                Diagnostic::new(Code::HS0001, &[("offset", offset)]).with_location(Location {
                    file: file.clone(),
                    span: Span::from_usize(*offset, offset + 1),
                    line: at.line,
                    column: at.column,
                })
            }
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::TooLarge { size } => {
                write!(
                    f,
                    "the source is {size} bytes; the limit is {MAX_SOURCE_BYTES}"
                )
            }
            SourceError::InvalidUtf8 { offset, .. } => {
                write!(
                    f,
                    "the source is not valid UTF-8 (invalid byte at offset {offset})"
                )
            }
        }
    }
}

impl std::error::Error for SourceError {}

/// One source file: its name, its text and its line index.
#[derive(Clone, Debug)]
pub struct Source {
    id: SourceId,
    name: FileName,
    text: Arc<str>,
    index: LineIndex,
}

impl Source {
    /// A source from text that is already decoded. The size limit applies to
    /// the UTF-8 length.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::TooLarge`] when the text is longer than [`MAX_SOURCE_BYTES`].
    pub fn new(
        id: SourceId,
        name: impl Into<FileName>,
        text: impl Into<Arc<str>>,
    ) -> Result<Source, SourceError> {
        let text = text.into();
        if text.len() > MAX_SOURCE_BYTES {
            return Err(SourceError::TooLarge { size: text.len() });
        }
        let index = LineIndex::new(&text);
        Ok(Source {
            id,
            name: name.into(),
            text,
            index,
        })
    }

    /// A source from bytes. The size is checked first, then the UTF-8 encoding.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::TooLarge`] or [`SourceError::InvalidUtf8`].
    pub fn from_bytes(
        id: SourceId,
        name: impl Into<FileName>,
        bytes: &[u8],
    ) -> Result<Source, SourceError> {
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(SourceError::TooLarge { size: bytes.len() });
        }
        match std::str::from_utf8(bytes) {
            Ok(text) => Source::new(id, name, text),
            Err(e) => {
                let offset = e.valid_up_to();
                let prefix = bytes
                    .get(..offset)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .unwrap_or("");
                let at = LineIndex::new(prefix)
                    .position(prefix, u32::try_from(offset).unwrap_or(u32::MAX));
                Err(SourceError::InvalidUtf8 { offset, at })
            }
        }
    }

    /// The identity of this source within its program.
    #[must_use]
    pub fn id(&self) -> SourceId {
        self.id
    }

    /// The canonical path or host-given name.
    #[must_use]
    pub fn name(&self) -> &FileName {
        &self.name
    }

    /// The whole text, including any byte order mark.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// A shared handle to the text.
    #[must_use]
    pub fn shared_text(&self) -> Arc<str> {
        self.text.clone()
    }

    /// The length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether the source is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The line index.
    #[must_use]
    pub fn line_index(&self) -> &LineIndex {
        &self.index
    }

    /// The line and column of a byte offset.
    #[must_use]
    pub fn position(&self, offset: u32) -> LineCol {
        self.index.position(&self.text, offset)
    }

    /// The text of a span, if it is in range.
    #[must_use]
    pub fn slice(&self, span: Span) -> Option<&str> {
        span.slice(&self.text)
    }

    /// The location record for a span of this source.
    #[must_use]
    pub fn locate(&self, span: Span) -> Location {
        let at = self.position(span.start);
        Location {
            file: self.name.clone(),
            span,
            line: at.line,
            column: at.column,
        }
    }

    /// The location record for a span of this source, like
    /// [`Source::locate`], but resuming the column count of `cursor`. A
    /// caller that locates many spans in non-decreasing offset order, such
    /// as a lexer or parser locating each diagnostic as it is found, should
    /// keep one [`PositionCursor`] across every call instead of calling
    /// `locate`, so that doing so costs time proportional to the size of
    /// this source in total rather than quadratic in how many it locates.
    #[must_use]
    pub fn locate_with(&self, span: Span, cursor: &mut PositionCursor) -> Location {
        let at = self.index.resolve(&self.text, span.start, cursor);
        Location {
            file: self.name.clone(),
            span,
            line: at.line,
            column: at.column,
        }
    }
}

/// The sources of one program, in load order.
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    sources: Vec<Arc<Source>>,
}

impl SourceMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    /// The id the next source added will have.
    #[must_use]
    pub fn next_id(&self) -> SourceId {
        SourceId(u32::try_from(self.sources.len()).unwrap_or(u32::MAX))
    }

    /// Adds a source, which must have been created with [`SourceMap::next_id`].
    pub fn push(&mut self, source: Source) -> Arc<Source> {
        let source = Arc::new(source);
        self.sources.push(source.clone());
        source
    }

    /// The source with `id`.
    #[must_use]
    pub fn get(&self, id: SourceId) -> Option<&Arc<Source>> {
        self.sources.get(id.0 as usize)
    }

    /// The number of sources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// The names of the sources in load order, the order in which diagnostics
    /// are reported (section 17.1).
    #[must_use]
    pub fn file_order(&self) -> Vec<FileName> {
        self.sources.iter().map(|s| s.name.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LineCol, LineIndex, MAX_SOURCE_BYTES, PositionCursor, Source, SourceError, SourceId, Span,
    };

    fn at(text: &str, offset: u32) -> (u32, u32) {
        let p = LineIndex::new(text).position(text, offset);
        (p.line, p.column)
    }

    #[test]
    fn lines_and_columns_count_code_points() {
        let text = "ab\nc\u{e9}d\n\u{1f600}x";
        assert_eq!(at(text, 0), (1, 1));
        assert_eq!(at(text, 2), (1, 3));
        assert_eq!(at(text, 3), (2, 1));
        assert_eq!(at(text, 6), (2, 3));
        assert_eq!(at(text, 7), (2, 4));
        assert_eq!(at(text, 8), (3, 1));
        assert_eq!(at(text, 12), (3, 2));
    }

    #[test]
    fn crlf_is_one_terminator_and_a_lone_cr_is_not() {
        let text = "a\r\nb\rc\nd";
        assert_eq!(LineIndex::new(text).line_count(), 3);
        assert_eq!(at(text, 1), (1, 2));
        assert_eq!(at(text, 3), (2, 1));
        assert_eq!(at(text, 5), (2, 3));
        assert_eq!(at(text, 7), (3, 1));
    }

    #[test]
    fn a_final_terminator_puts_the_end_on_the_next_line() {
        let text = "a\n";
        assert_eq!(at(text, 2), (2, 1));
        assert_eq!(LineIndex::new(text).line_count(), 2);
        assert_eq!(at("", 0), (1, 1));
    }

    #[test]
    fn the_byte_order_mark_counts_in_offsets_but_not_in_columns() {
        let text = "\u{feff}ab\ncd";
        assert_eq!(at(text, 3), (1, 1));
        assert_eq!(at(text, 4), (1, 2));
        assert_eq!(at(text, 0), (1, 1));
        assert_eq!(at(text, 6), (2, 1));
        assert_eq!(at(text, 7), (2, 2));
    }

    #[test]
    fn positions_past_the_end_and_inside_characters_are_clamped() {
        assert_eq!(at("ab", 99), (1, 3));
        assert_eq!(at("\u{e9}x", 1), (1, 2));
    }

    #[test]
    fn offset_of_inverts_position() {
        let text = "\u{feff}a\u{e9}\nbc\n";
        let index = LineIndex::new(text);
        for offset in [3, 4, 6, 7, 8, 9, 10] {
            let p = index.position(text, offset);
            assert_eq!(index.offset_of(text, p), Some(offset), "offset {offset}");
        }
        assert_eq!(index.offset_of(text, LineCol { line: 9, column: 1 }), None);
        assert_eq!(index.offset_of(text, LineCol { line: 1, column: 9 }), None);
        assert_eq!(index.offset_of(text, LineCol { line: 0, column: 1 }), None);
    }

    #[test]
    fn spans_are_ordered_ranges() {
        let s = Span::new(5, 2);
        assert_eq!((s.start, s.end), (5, 5));
        assert!(s.is_empty());
        assert_eq!(Span::new(1, 3).cover(Span::new(6, 9)), Span::new(1, 9));
        assert_eq!(Span::new(1, 3).slice("abcd"), Some("bc"));
        assert_eq!(Span::new(0, 9).slice("abcd"), None);
        assert_eq!(Span::from_usize(1, usize::MAX).end, u32::MAX);
    }

    #[test]
    fn size_is_checked_before_the_encoding() {
        let mut bytes = vec![0xff; MAX_SOURCE_BYTES + 1];
        let err = Source::from_bytes(SourceId(0), "main.hws", &bytes).unwrap_err();
        assert!(matches!(err, SourceError::TooLarge { size } if size == MAX_SOURCE_BYTES + 1));
        bytes.truncate(MAX_SOURCE_BYTES);
        assert!(matches!(
            Source::from_bytes(SourceId(0), "main.hws", &bytes),
            Err(SourceError::InvalidUtf8 { offset: 0, .. })
        ));
    }

    #[test]
    fn invalid_utf8_is_located_in_the_valid_prefix() {
        let err = Source::from_bytes(SourceId(0), "a.hws", b"ab\ncd\xffef").unwrap_err();
        assert_eq!(
            err,
            SourceError::InvalidUtf8 {
                offset: 5,
                at: LineCol { line: 2, column: 3 }
            }
        );
    }

    #[test]
    fn a_source_locates_spans() {
        let source = Source::new(SourceId(3), "m/x.hws", "one\ntwo").unwrap();
        let loc = source.locate(Span::new(4, 7));
        assert_eq!((loc.line, loc.column), (2, 1));
        assert_eq!(&*loc.file, "m/x.hws");
        assert_eq!(source.slice(loc.span), Some("two"));
        assert_eq!(source.id(), SourceId(3));
    }

    #[test]
    fn resolve_with_a_cursor_agrees_with_position_in_increasing_order() {
        let text = "\u{feff}ab\u{e9}\nc\u{1f600}d\n\nlast";
        let index = LineIndex::new(text);
        let mut cursor = PositionCursor::new();
        for offset in 0..=u32::try_from(text.len()).unwrap() {
            let expected = index.position(text, offset);
            let got = index.resolve(text, offset, &mut cursor);
            assert_eq!(got, expected, "offset {offset}");
        }
    }

    #[test]
    fn resolve_with_a_cursor_agrees_with_position_out_of_order() {
        // A cursor is only an optimisation: every offset must resolve to the
        // same place as `position` however the offsets are visited, so a
        // caller that (unlike the lexer and parser) does not scan strictly
        // left to right is still correct, just without the speed-up.
        let text = "one two\nthree four\nfive";
        let index = LineIndex::new(text);
        let mut cursor = PositionCursor::new();
        let offsets = [10u32, 3, 20, 20, 5, 0, 23, 8, 8, 1];
        for offset in offsets {
            let expected = index.position(text, offset);
            let got = index.resolve(text, offset, &mut cursor);
            assert_eq!(got, expected, "offset {offset}");
        }
    }

    #[test]
    fn resolve_past_the_end_and_the_empty_text_match_position() {
        let index = LineIndex::new("ab");
        let mut cursor = PositionCursor::new();
        assert_eq!(
            index.resolve("ab", 99, &mut cursor),
            index.position("ab", 99)
        );
        let empty = LineIndex::new("");
        let mut cursor = PositionCursor::new();
        assert_eq!(empty.resolve("", 0, &mut cursor), empty.position("", 0));
    }

    #[test]
    fn resolving_many_positions_on_one_long_line_is_not_quadratic() {
        // The specific regression this cursor exists for: many diagnostics
        // spread across one long line, resolved in the order a left-to-right
        // scan finds them. `position` alone would cost time proportional to
        // the square of the count here; `resolve` with one cursor must not.
        let text = "x".repeat(400_000);
        let index = LineIndex::new(&text);
        let mut cursor = PositionCursor::new();
        let started = std::time::Instant::now();
        let len = u32::try_from(text.len()).unwrap();
        for offset in (0..len).step_by(4) {
            let _ = index.resolve(&text, offset, &mut cursor);
        }
        // Generous even for a slow or loaded machine: the point is ruling
        // out quadratic behaviour, not pinning down an exact budget. The
        // quadratic version of this loop takes tens of seconds at this size.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "resolving 100,000 positions on one long line took {:?}",
            started.elapsed()
        );
    }
}
