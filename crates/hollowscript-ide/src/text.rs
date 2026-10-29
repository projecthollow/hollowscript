//! Converting between the byte offsets this crate works in and the UTF-16
//! code unit offsets a web text editor counts in.
//!
//! A span is a half-open range of UTF-8 byte offsets, the unit of
//! `hollowscript_core::Span`. A browser editor (and every other host whose
//! strings are UTF-16) counts a position in UTF-16 code units, which agrees
//! with a byte offset for ASCII and for nothing else: a character in
//! U+0080 to U+07FF is two bytes and one unit, one in U+0800 to U+FFFF is
//! three bytes and one unit, and one outside the basic multilingual plane
//! is four bytes and two units (a surrogate pair).
//!
//! A [`Utf16Map`] is built once per text and then converts in either
//! direction in logarithmic time. It records only the characters that are
//! not ASCII, so for source that is mostly ASCII, which is nearly all of it,
//! it is tiny.

/// One character that is not one byte long, and how far its position has
/// drifted from a plain byte offset.
#[derive(Copy, Clone, Debug)]
struct Wide {
    /// The byte offset the character starts at.
    start: u32,
    /// Its length in UTF-8 bytes (2, 3 or 4).
    bytes: u32,
    /// Its length in UTF-16 code units (1 or 2).
    units: u32,
    /// How many more bytes than UTF-16 code units the text holds from the
    /// start up to and including this character.
    drift_after: u32,
}

impl Wide {
    /// The drift before this character.
    fn drift_before(self) -> u32 {
        self.drift_after - (self.bytes - self.units)
    }

    /// The UTF-16 offset this character starts at.
    fn unit_start(self) -> u32 {
        self.start - self.drift_before()
    }
}

/// The mapping between byte offsets and UTF-16 code unit offsets of one text.
///
/// Both directions clamp: an offset past the end of the text answers the end
/// of the text, so a stale position from an editor never panics. An offset
/// that falls inside a character (the second byte of a two-byte character,
/// the second unit of a surrogate pair) answers the start of that character.
#[derive(Clone, Debug)]
pub struct Utf16Map {
    wides: Vec<Wide>,
    /// The length of the text in bytes.
    byte_len: u32,
    /// The length of the text in UTF-16 code units.
    unit_len: u32,
}

impl Utf16Map {
    /// The map of `text`. A text longer than `u32::MAX` bytes (far past the
    /// source size limit of section 1.1) is mapped as far as `u32` reaches.
    #[must_use]
    pub fn new(text: &str) -> Utf16Map {
        let mut wides = Vec::new();
        let mut drift = 0u32;
        for (start, c) in text.char_indices() {
            let bytes = c.len_utf8();
            let units = c.len_utf16();
            if bytes == units {
                continue;
            }
            let (Ok(start), Ok(bytes), Ok(units)) = (
                u32::try_from(start),
                u32::try_from(bytes),
                u32::try_from(units),
            ) else {
                break;
            };
            drift = drift.saturating_add(bytes - units);
            wides.push(Wide {
                start,
                bytes,
                units,
                drift_after: drift,
            });
        }
        let byte_len = u32::try_from(text.len()).unwrap_or(u32::MAX);
        Utf16Map {
            wides,
            byte_len,
            unit_len: byte_len.saturating_sub(drift),
        }
    }

    /// The length of the text in UTF-16 code units.
    #[must_use]
    pub fn utf16_len(&self) -> u32 {
        self.unit_len
    }

    /// The UTF-16 offset of the byte offset `byte`.
    #[must_use]
    pub fn to_utf16(&self, byte: u32) -> u32 {
        if byte >= self.byte_len {
            return self.unit_len;
        }
        let after = self.wides.partition_point(|w| w.start <= byte);
        let Some(last) = after.checked_sub(1).and_then(|i| self.wides.get(i)) else {
            return byte;
        };
        if byte >= last.start + last.bytes {
            byte - last.drift_after
        } else {
            // Inside the character: its own start.
            last.unit_start()
        }
    }

    /// The byte offset of the UTF-16 offset `unit`.
    #[must_use]
    pub fn to_byte(&self, unit: u32) -> u32 {
        if unit >= self.unit_len {
            return self.byte_len;
        }
        let after = self.wides.partition_point(|w| w.unit_start() <= unit);
        let Some(last) = after.checked_sub(1).and_then(|i| self.wides.get(i)) else {
            return unit;
        };
        if unit >= last.unit_start() + last.units {
            unit + last.drift_after
        } else {
            // Inside a surrogate pair: the start of the character.
            last.start
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Utf16Map;

    #[test]
    fn ascii_is_the_identity() {
        let map = Utf16Map::new("constant x = 1\n");
        for offset in 0..=15 {
            assert_eq!(map.to_utf16(offset), offset.min(15));
            assert_eq!(map.to_byte(offset), offset.min(15));
        }
        assert_eq!(map.utf16_len(), 15);
    }

    #[test]
    fn an_empty_text_maps_everything_to_zero() {
        let map = Utf16Map::new("");
        assert_eq!(map.to_utf16(0), 0);
        assert_eq!(map.to_utf16(9), 0);
        assert_eq!(map.to_byte(0), 0);
        assert_eq!(map.to_byte(9), 0);
    }

    #[test]
    fn two_three_and_four_byte_characters_count_one_one_and_two_units() {
        // a, e with acute (2 bytes, 1 unit), euro sign (3, 1), grinning
        // face (4, 2), b.
        let text = "a\u{e9}\u{20ac}\u{1f600}b";
        assert_eq!(text.len(), 1 + 2 + 3 + 4 + 1);
        let map = Utf16Map::new(text);
        // Byte offsets of each character's start and the end.
        let bytes = [0u32, 1, 3, 6, 10, 11];
        let units = [0u32, 1, 2, 3, 5, 6];
        for (byte, unit) in bytes.into_iter().zip(units) {
            assert_eq!(map.to_utf16(byte), unit, "byte {byte}");
            assert_eq!(map.to_byte(unit), byte, "unit {unit}");
        }
        assert_eq!(map.utf16_len(), 6);
        assert_eq!(map.utf16_len() as usize, text.encode_utf16().count());
    }

    #[test]
    fn an_offset_inside_a_character_answers_the_start_of_that_character() {
        let text = "a\u{e9}\u{1f600}b";
        let map = Utf16Map::new(text);
        // Inside the two-byte character (byte 2) and inside the four-byte
        // one (bytes 4 to 6).
        assert_eq!(map.to_utf16(2), 1);
        for byte in 4..7 {
            assert_eq!(map.to_utf16(byte), 2, "byte {byte}");
        }
        // The second unit of the surrogate pair.
        assert_eq!(map.to_byte(3), 3);
    }

    #[test]
    fn an_offset_past_the_end_clamps() {
        let map = Utf16Map::new("\u{1f600}x");
        assert_eq!(map.to_utf16(5), 3);
        assert_eq!(map.to_utf16(500), 3);
        assert_eq!(map.to_byte(3), 5);
        assert_eq!(map.to_byte(500), 5);
    }

    #[test]
    fn every_boundary_round_trips() {
        let text = "x \u{1f600} y \u{20ac}\u{20ac} z \u{e9}\u{1f680}\u{1f680}!";
        let map = Utf16Map::new(text);
        let mut unit = 0u32;
        for (byte, c) in text.char_indices() {
            let byte = u32::try_from(byte).unwrap();
            assert_eq!(map.to_utf16(byte), unit);
            assert_eq!(map.to_byte(unit), byte);
            unit += u32::try_from(c.len_utf16()).unwrap();
        }
        assert_eq!(map.to_utf16(u32::try_from(text.len()).unwrap()), unit);
    }
}
