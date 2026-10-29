//! Cross-checks the keyword table against the specification.
//!
//! Section 1.3 lists the 27 reserved words in a code block, and section 18.2
//! lists them again. Both lists must be the table of [`Keyword`]. Set
//! `HOLLOWSCRIPT_REQUIRE_SPEC` to make a missing specification an error.

#![allow(clippy::print_stderr, reason = "reports a skipped test")]

use hollowscript_syntax::Keyword;
use std::path::Path;

fn read(chapter: &str) -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/spec")
        .join(chapter);
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) => {
            assert!(
                std::env::var_os("HOLLOWSCRIPT_REQUIRE_SPEC").is_none(),
                "the specification is required but {} cannot be read: {e}",
                path.display()
            );
            eprintln!("skipping: {} cannot be read: {e}", path.display());
            None
        }
    }
}

fn table() -> Vec<&'static str> {
    Keyword::ALL.iter().map(|k| k.as_str()).collect()
}

#[test]
fn chapter_1_lists_the_keywords() {
    let Some(text) = read("01-lexical-structure.md") else {
        return;
    };
    let after = text
        .split("These 27 words are reserved")
        .nth(1)
        .expect("the keyword sentence");
    let block = after.split("```").nth(1).expect("the code block after it");
    let mut words: Vec<&str> = block.split_whitespace().collect();
    words.sort_unstable();
    assert_eq!(words, table());
}

#[test]
fn chapter_18_lists_the_keywords() {
    let Some(text) = read("18-grammar.md") else {
        return;
    };
    let line = text
        .lines()
        .find(|l| l.starts_with("**Keywords** (27)"))
        .expect("the keyword line");
    // The sentence that follows the list names `identifier` in backticks too.
    let list = line
        .split("`while`")
        .next()
        .expect("text before the last keyword");
    let mut words: Vec<&str> = list.split('`').skip(1).step_by(2).collect();
    words.push("while");
    words.sort_unstable();
    assert_eq!(words, table());
}
