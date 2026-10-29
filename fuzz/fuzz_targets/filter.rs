//! Appendix A's filter language over arbitrary bytes.
//!
//! Section 19.1's "never crashes, hangs or exhausts its host on any input"
//! applies to `Filter` exactly as it does to the rest of the front end, and
//! `hollowscript-core::filter` is its own small lexer, parser, evaluator and
//! printer with no test suite reaching it through libFuzzer's coverage
//! feedback otherwise. The four operations are called directly, without a
//! program or the checker around them, since that is how a host actually
//! reaches them (section A.1: "a host or tool that implements filters").
//!
//! Beyond "never panics", this target checks the correctness property
//! cheap enough to assert on every input: `format`'s own text always parses
//! back cleanly, and reformatting what it reparses to is idempotent
//! (`format(parse(format(t))) == format(t)`), the same property
//! `format_roundtrip` checks for a whole program. This stops short of
//! asserting the *reparsed tree* equals the original one node for node,
//! because section A.9 itself does not promise that at the byte level: it
//! lower-cases the keyword parts of a relative date's value
//! (`due<TODAY+7` formats as `due<today+7`), so a tree built from `TODAY+7`
//! and the tree reparsed from its own canonical text differ in that one
//! value's original text even though they mean the same thing and format
//! to the same string from there on; idempotence is the invariant that
//! actually holds unconditionally.
//!
//! `validate` and `matches` are exercised too, against a small fixed schema
//! and adapter that between them touch every field type Appendix A defines,
//! so a panic specific to schema validation or evaluation (a bad index, an
//! unwrap on a value shape the parser can produce but the schema code did
//! not expect) is reachable from arbitrary text as well as from the
//! conformance vectors' own hand-written schema.
//!
//! **Canonical text can be longer than the source it came from**: quoting
//! an unsafe term or value, and parenthesising a nested `and`/`or`, only
//! ever adds bytes, never removes them, so a source right at
//! [`MAX_FILTER_BYTES`] can format to text past it. Section A.10 caps
//! *parsing*, not what `format` may produce, so `HS0704` on the reparse is
//! an expected outcome of that combination, not a defect; every other
//! reparse error is not.

#![no_main]

use hollowscript_core::Code;
use hollowscript_core::filter::{
    Adapter, FieldDesc, FieldType, FilterSchema, Resolved, format, matches, parse, validate,
};
use hollowscript_core::limits::structural::MAX_FILTER_BYTES;
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;

/// A record whose fields exercise every `FieldType`, and whose values are
/// varied enough (an empty list, an absent date, a zero) that emptiness and
/// the none/any/date/number rules of section A.8 all get real branches to
/// evaluate against, not only the "field not in schema" fallback.
struct FuzzRecord;

impl Adapter for FuzzRecord {
    fn resolve(&self, field: &str) -> Resolved {
        match field {
            "status" => Resolved::Text("open".to_owned()),
            "tag" => Resolved::List(vec!["a".to_owned(), "b".to_owned()]),
            "empty_tag" => Resolved::List(Vec::new()),
            "title" => Resolved::Text("hello world".to_owned()),
            "due" => Resolved::Text("2026-07-26".to_owned()),
            "points" => Resolved::Int(5),
            "score" => Resolved::Float(1.5),
            "starred" => Resolved::Bool(true),
            _ => Resolved::Absent,
        }
    }

    fn free_text(&self) -> Vec<String> {
        vec!["hello world".to_owned(), "quarterly report".to_owned()]
    }

    fn today(&self) -> String {
        "2026-07-26".to_owned()
    }
}

fn schema() -> FilterSchema {
    let mut fields = BTreeMap::new();
    fields.insert(
        "status".to_owned(),
        FieldDesc {
            ty: FieldType::Enum,
            aliases: BTreeMap::from([("opened".to_owned(), "open".to_owned())]),
            values: Some(vec!["open".to_owned(), "closed".to_owned()]),
        },
    );
    fields.insert(
        "tag".to_owned(),
        FieldDesc {
            ty: FieldType::TextMulti,
            aliases: BTreeMap::new(),
            values: None,
        },
    );
    fields.insert(
        "empty_tag".to_owned(),
        FieldDesc {
            ty: FieldType::TextMulti,
            aliases: BTreeMap::new(),
            values: None,
        },
    );
    fields.insert(
        "title".to_owned(),
        FieldDesc {
            ty: FieldType::Text,
            aliases: BTreeMap::new(),
            values: None,
        },
    );
    fields.insert(
        "due".to_owned(),
        FieldDesc {
            ty: FieldType::Date,
            aliases: BTreeMap::new(),
            values: None,
        },
    );
    fields.insert(
        "points".to_owned(),
        FieldDesc {
            ty: FieldType::Number,
            aliases: BTreeMap::new(),
            values: None,
        },
    );
    fields.insert(
        "score".to_owned(),
        FieldDesc {
            ty: FieldType::Number,
            aliases: BTreeMap::new(),
            values: None,
        },
    );
    fields.insert(
        "starred".to_owned(),
        FieldDesc {
            ty: FieldType::Boolean,
            aliases: BTreeMap::new(),
            values: None,
        },
    );
    FilterSchema { fields }
}

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data).into_owned();
    let Ok(tree) = parse(&source) else {
        return;
    };
    let schema = schema();
    let _ = validate(&tree, &schema);
    let _ = matches(&tree, &FuzzRecord, &schema);

    let canonical = format(&tree);
    match parse(&canonical) {
        Ok(reparsed) => {
            // format is idempotent on its own output: reformatting the
            // reparsed tree must give the same text back (see this file's
            // module doc comment for why this, not tree equality, is the
            // invariant that holds unconditionally).
            let again = format(&reparsed);
            assert_eq!(
                again, canonical,
                "format was not idempotent:\n\
                 source:    {source:?}\n\
                 canonical: {canonical:?}\n\
                 again:     {again:?}"
            );
        }
        Err(e) if e.code == Code::HS0704 && canonical.len() > MAX_FILTER_BYTES => {
            // Expected: quoting and parenthesising only ever add bytes, so
            // canonical text can grow past the length a source of exactly
            // MAX_FILTER_BYTES was allowed to have (see the module doc
            // comment). Anything else about the error would still be a bug.
        }
        Err(e) => panic!(
            "format's own output failed to reparse: {e:?}\n\
             source:    {source:?}\n\
             canonical: {canonical:?}"
        ),
    }
});
