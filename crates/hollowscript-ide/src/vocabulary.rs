//! The words the language itself reserves or provides, as lists a client can
//! offer or refuse: the keywords of section 1.3 and the value names of the
//! prelude (section 13.6).

use hollowscript_syntax::Keyword;

/// Section 13.6's prelude value names, offered wherever a name can be
/// written and refused as a rename target.
pub const PRELUDE_NAMES: &[&str] = &[
    "abs",
    "atan2",
    "ceil",
    "clamp",
    "cos",
    "div",
    "exp",
    "failure",
    "fixed",
    "floor",
    "has",
    "ipow",
    "json",
    "keys",
    "ln",
    "mapFrom",
    "max",
    "min",
    "mod",
    "parseFloat",
    "parseInt",
    "pi",
    "pow",
    "range",
    "remainder",
    "round",
    "set",
    "sign",
    "sin",
    "sqrt",
    "success",
    "tan",
    "time",
    "toFloat",
    "trunc",
];

/// One reserved word of the language.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeywordInfo {
    /// The word as written.
    pub word: &'static str,
    /// Whether it is reserved for a later version, so that writing it
    /// anywhere is HS0104.
    pub future: bool,
}

/// Every reserved word of the language, as written, in alphabetical order:
/// [`Keyword::ALL`], so a word the language adds cannot be missed here.
pub fn keyword_infos() -> impl Iterator<Item = KeywordInfo> {
    Keyword::ALL.into_iter().map(|keyword| KeywordInfo {
        word: keyword.as_str(),
        future: keyword.is_future(),
    })
}

/// The reserved words a program may write today: every keyword except the
/// ones reserved for a later version, which are an error wherever they
/// appear (HS0104) and so are not worth offering.
pub fn usable_keywords() -> impl Iterator<Item = &'static str> {
    Keyword::ALL
        .into_iter()
        .filter(|keyword| !keyword.is_future())
        .map(Keyword::as_str)
}

/// Whether `word` is a keyword of any kind, usable or reserved for later.
#[must_use]
pub fn is_keyword(word: &str) -> bool {
    Keyword::from_word(word).is_some()
}

/// Whether `word` is one of the prelude's value names.
#[must_use]
pub fn is_prelude_name(word: &str) -> bool {
    PRELUDE_NAMES.contains(&word)
}

#[cfg(test)]
mod tests {
    use super::{PRELUDE_NAMES, is_keyword, is_prelude_name, keyword_infos, usable_keywords};

    #[test]
    fn every_keyword_is_known_and_the_future_ones_are_left_out_of_the_usable_list() {
        assert_eq!(keyword_infos().count(), 27);
        for keyword in keyword_infos() {
            assert!(is_keyword(keyword.word));
        }
        let future: Vec<&str> = keyword_infos()
            .filter(|k| k.future)
            .map(|k| k.word)
            .collect();
        assert_eq!(future, vec!["assert", "test"]);
        let usable: Vec<&str> = usable_keywords().collect();
        assert_eq!(usable.len(), 25);
        for word in ["and", "false", "from", "not", "or", "true"] {
            assert!(usable.contains(&word), "{word}");
        }
        assert!(!usable.contains(&"assert"));
        assert!(!usable.contains(&"test"));
        assert!(is_keyword("assert"));
        assert!(!is_keyword("print"));
    }

    #[test]
    fn the_prelude_names_are_sorted_and_recognised() {
        assert!(PRELUDE_NAMES.windows(2).all(|w| w[0] <= w[1]));
        assert!(is_prelude_name("sqrt"));
        assert!(!is_prelude_name("sqrtt"));
    }
}
