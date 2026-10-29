//! Editor services for a browser editor, behind the `tooling` feature:
//! lexical highlighting, the keyword list, and an analysis handle that holds
//! a checked workspace so hover, completion, definition and the outline do
//! not re-check on every call.
//!
//! The analysis itself is `hollowscript-ide`, which counts positions in UTF-8
//! bytes. A web editor counts in UTF-16 code units, so every offset that
//! crosses this boundary, in either direction, is converted here with
//! [`Utf16Map`], and every range this module answers is half-open and in
//! UTF-16 code units.
//!
//! Everything answers JSON text, built with the language's own JSON writer,
//! the convention [`super`] already uses.

use super::{capability_interface, module_loader, standard_capabilities};
use hollowscript::json::{Json, stringify};
use hollowscript::{CapabilityDecl, CapabilityShape};
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable};
use hollowscript_ide::highlight::highlight;
use hollowscript_ide::outline::OutlineItem;
use hollowscript_ide::text::Utf16Map;
use hollowscript_ide::vocabulary::keyword_infos;
use hollowscript_ide::{DeclId, Workspace};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

/// An `Int` JSON number from an unsigned offset.
fn number(value: u32) -> Json {
    Json::Int(i64::from(value))
}

/// A JSON object from named members, in the order given.
fn object<const N: usize>(members: [(&str, Json); N]) -> Json {
    Json::Object(
        members
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

/// The lexical classes of every token and comment of `source`, as a JSON
/// array of `{"start":0,"end":8,"class":"keyword"}`, in order of where they
/// start, with offsets in UTF-16 code units, half-open. Stateless: nothing
/// is kept between calls, and the source need not parse.
#[must_use]
pub(crate) fn highlight_json(source: &str) -> String {
    let map = Utf16Map::new(source);
    let items = highlight(source)
        .into_iter()
        .map(|h| {
            object([
                ("start", number(map.to_utf16(h.span.start))),
                ("end", number(map.to_utf16(h.span.end))),
                ("class", Json::String(h.class.as_str().to_owned())),
            ])
        })
        .collect();
    stringify(&Json::Array(items))
}

/// Every reserved word of the language, as a JSON array of
/// `{"word":"and","future":false}` in alphabetical order. `future` marks the
/// words reserved for a later version, which are an error to write today.
/// Stateless, and taken from the lexer's own keyword table.
#[must_use]
pub(crate) fn keywords_json() -> String {
    let items = keyword_infos()
        .map(|k| {
            object([
                ("word", Json::String(k.word.to_owned())),
                ("future", Json::Bool(k.future)),
            ])
        })
        .collect();
    stringify(&Json::Array(items))
}

/// The most analyses a page may hold at once. Each keeps a checked program in
/// memory, so a page that forgets to `dispose` them would otherwise grow
/// without bound; it is told, with a message that says so, when it reaches
/// this.
pub(crate) const MAX_LIVE_ANALYSES: usize = 64;

/// A checked workspace and what it needs to answer in UTF-16: the text of
/// every module its answers can point into, as a [`Utf16Map`].
pub(crate) struct Analysis {
    workspace: Workspace,
    /// The map of the entry module's text.
    entry: Utf16Map,
    /// The map of each direct import's text, by canonical path.
    imports: BTreeMap<String, Utf16Map>,
}

impl Analysis {
    /// Checks `source`, named `entry_name`, against the standard
    /// capabilities plus those `capabilities_json` declares (the same
    /// reading `check` gives it), resolving imports against `modules_json`
    /// (likewise).
    ///
    /// # Errors
    ///
    /// The reason, as plain text, if either JSON argument is malformed or
    /// the source is longer than the size limit of section 1.1.
    pub(crate) fn new(
        source: &str,
        entry_name: &str,
        capabilities_json: &str,
        modules_json: &str,
    ) -> Result<Analysis, String> {
        let capabilities = capability_interface(capabilities_json)?;
        let loader = module_loader(modules_json)?;
        let workspace = Workspace::from_modules(entry_name, source, &loader.modules, &capabilities)
            .ok_or_else(|| "the source is longer than the size limit".to_owned())?;
        let entry = Utf16Map::new(&workspace.text);
        let imports = workspace
            .imports
            .iter()
            .map(|m| (m.canonical_path.clone(), Utf16Map::new(&m.text)))
            .collect();
        Ok(Analysis {
            workspace,
            entry,
            imports,
        })
    }

    /// The byte offset of a UTF-16 offset in the entry module.
    fn byte(&self, offset: u32) -> u32 {
        self.entry.to_byte(offset)
    }

    /// `{"start":0,"end":3,"type":"Int"}` for the type of the smallest
    /// expression at the UTF-16 `offset`, or `null`.
    pub(crate) fn hover_json(&self, offset: u32) -> String {
        let json = self
            .workspace
            .hover(self.byte(offset))
            .map_or(Json::Null, |hover| {
                object([
                    ("start", number(self.entry.to_utf16(hover.span.start))),
                    ("end", number(self.entry.to_utf16(hover.span.end))),
                    ("type", Json::String(hover.ty)),
                ])
            });
        stringify(&json)
    }

    /// `[{"label":"total","kind":"constant","detail":"Int"}, ...]` for what
    /// can be written at the UTF-16 `offset`.
    pub(crate) fn completions_json(&self, offset: u32) -> String {
        let items = self
            .workspace
            .completions(self.byte(offset))
            .into_iter()
            .map(|c| {
                object([
                    ("label", Json::String(c.label)),
                    ("kind", Json::String(c.kind.as_str().to_owned())),
                    ("detail", Json::String(c.detail)),
                ])
            })
            .collect();
        stringify(&Json::Array(items))
    }

    /// `{"file":"util.hws","start":7,"end":10}` for where the name at the
    /// UTF-16 `offset` is declared (`file` is the module's canonical path,
    /// or the entry name for this module), or `null`.
    pub(crate) fn definition_json(&self, offset: u32) -> String {
        let json = self
            .workspace
            .definition(self.byte(offset))
            .and_then(|decl| self.located(&decl))
            .unwrap_or(Json::Null);
        stringify(&json)
    }

    fn located(&self, decl: &DeclId) -> Option<Json> {
        let map = if decl.module == self.workspace.entry_name {
            &self.entry
        } else {
            self.imports.get(&decl.module)?
        };
        Some(object([
            ("file", Json::String(decl.module.clone())),
            ("start", number(map.to_utf16(decl.span.start))),
            ("end", number(map.to_utf16(decl.span.end))),
        ]))
    }

    /// The outline as a tree of `{"name", "kind", "detail", "exported",
    /// "start", "end", "selStart", "selEnd", "children"}`.
    pub(crate) fn symbols_json(&self) -> String {
        let items = self
            .workspace
            .document_symbols()
            .iter()
            .map(|item| self.symbol(item))
            .collect();
        stringify(&Json::Array(items))
    }

    fn symbol(&self, item: &OutlineItem) -> Json {
        object([
            ("name", Json::String(item.name.clone())),
            ("kind", Json::String(item.kind.as_str().to_owned())),
            ("detail", Json::String(item.detail.clone())),
            ("exported", Json::Bool(item.exported)),
            ("start", number(self.entry.to_utf16(item.range.start))),
            ("end", number(self.entry.to_utf16(item.range.end))),
            (
                "selStart",
                number(self.entry.to_utf16(item.selection.start)),
            ),
            ("selEnd", number(self.entry.to_utf16(item.selection.end))),
            (
                "children",
                Json::Array(item.children.iter().map(|c| self.symbol(c)).collect()),
            ),
        ])
    }
}

thread_local! {
    /// Every live analysis, by handle. A handle is never reused, so a
    /// disposed handle stays refused rather than naming a later analysis.
    static ANALYSES: RefCell<BTreeMap<u32, Analysis>> = const { RefCell::new(BTreeMap::new()) };
    /// The handle the next analysis gets. 0 is never a handle.
    static NEXT_HANDLE: Cell<u32> = const { Cell::new(1) };
}

/// Checks the source and holds the result, answering its handle.
///
/// # Errors
///
/// The reason, as plain text, if an argument is malformed, the source is too
/// long, [`MAX_LIVE_ANALYSES`] are already held, or handles have run out.
pub(crate) fn analyse(
    source: &str,
    entry_name: &str,
    capabilities_json: &str,
    modules_json: &str,
) -> Result<u32, String> {
    if ANALYSES.with_borrow(BTreeMap::len) >= MAX_LIVE_ANALYSES {
        return Err(format!(
            "{MAX_LIVE_ANALYSES} analyses are already held; dispose the ones no longer needed"
        ));
    }
    let analysis = Analysis::new(source, entry_name, capabilities_json, modules_json)?;
    let handle = NEXT_HANDLE.get();
    let next = handle
        .checked_add(1)
        .ok_or_else(|| "no analysis handles are left".to_owned())?;
    NEXT_HANDLE.set(next);
    ANALYSES.with_borrow_mut(|held| held.insert(handle, analysis));
    Ok(handle)
}

/// Runs `query` against the analysis `handle` names, or answers `None` for a
/// handle that names none (never given, or disposed).
pub(crate) fn with_analysis(
    handle: u32,
    query: impl FnOnce(&Analysis) -> String,
) -> Option<String> {
    ANALYSES.with_borrow(|held| held.get(&handle).map(query))
}

/// Ends an analysis. A handle that names none is ignored, so disposing twice
/// is harmless.
pub(crate) fn dispose(handle: u32) {
    ANALYSES.with_borrow_mut(|held| held.remove(&handle));
}

/// The declarations of the standard capabilities (`print`, `clock`,
/// `random`), in the JSON shape the `capabilities` argument of `check` takes:
/// an array of `{"name","function","params","result"}`, one entry per
/// function of a namespace capability, with `function` left out for a
/// capability that is a single function and `result` left out for a function
/// with no result. They are what `check` and `analyse` already grant on top
/// of whatever is declared, so a host need not copy them; this lets it show
/// them, or pass them along with its own.
#[must_use]
pub(crate) fn standard_capabilities_json() -> String {
    let mut types = TypeTable::new();
    let decls = standard_capabilities(&mut types);
    stringify(&capabilities_to_json(&decls, &types))
}

fn capabilities_to_json(decls: &[CapabilityDecl], types: &TypeTable) -> Json {
    let params =
        |refs: &[TypeRef]| Json::Array(refs.iter().map(|r| type_json(types, *r, 0)).collect());
    let mut entries = Vec::new();
    for decl in decls {
        let mut push = |function: Option<&str>, args: &[TypeRef], result: Option<TypeRef>| {
            let mut members = vec![("name".to_owned(), Json::String(decl.name.clone()))];
            if let Some(function) = function {
                members.push(("function".to_owned(), Json::String(function.to_owned())));
            }
            members.push(("params".to_owned(), params(args)));
            if let Some(result) = result {
                members.push(("result".to_owned(), type_json(types, result, 0)));
            }
            entries.push(Json::Object(members));
        };
        match &decl.shape {
            CapabilityShape::Function { params, ret } => push(None, params, *ret),
            CapabilityShape::Namespace(functions) => {
                for (name, args, ret) in functions {
                    push(Some(name), args, *ret);
                }
            }
        }
    }
    Json::Array(entries)
}

/// How deep a type description nests before it is cut off with `null`,
/// which keeps a recursive enum from being followed for ever. No standard
/// capability gets near it.
const MAX_TYPE_DEPTH: u32 = 16;

/// A type description in the shape `declared_type` reads: a bare name for a
/// scalar, `{"list": T}`, `{"set": T}`, `{"optional": T}`,
/// `{"map": {"key": K, "value": V}}`, `{"record": {...}}`,
/// `{"enum": {"name": "E", "variants": {"V": [T]}}}`, and the flat
/// `{"ok": T, "err": T}` for the language's own `Result`.
fn type_json(types: &TypeTable, r: TypeRef, depth: u32) -> Json {
    let Some(ty) = types.get(r).filter(|_| depth < MAX_TYPE_DEPTH) else {
        return Json::Null;
    };
    let next = depth + 1;
    let wrap = |tag: &str, inner: Json| object([(tag, inner)]);
    match ty {
        TransferType::Int => Json::String("Int".to_owned()),
        TransferType::Float => Json::String("Float".to_owned()),
        TransferType::Bool => Json::String("Bool".to_owned()),
        TransferType::String => Json::String("String".to_owned()),
        TransferType::Filter => Json::String("Filter".to_owned()),
        TransferType::List(t) => wrap("list", type_json(types, *t, next)),
        TransferType::Set(t) => wrap("set", type_json(types, *t, next)),
        TransferType::Optional(t) => wrap("optional", type_json(types, *t, next)),
        TransferType::Map(k, v) => wrap(
            "map",
            object([
                ("key", type_json(types, *k, next)),
                ("value", type_json(types, *v, next)),
            ]),
        ),
        TransferType::Record(fields) => wrap(
            "record",
            Json::Object(
                fields
                    .iter()
                    .map(|(name, t)| (name.clone(), type_json(types, *t, next)))
                    .collect(),
            ),
        ),
        TransferType::Enum { name, variants, .. } => {
            if let ("Result", [ok, err]) = (name.as_str(), variants.as_slice()) {
                if let ([ok_ty], [err_ty]) = (ok.payload.as_slice(), err.payload.as_slice()) {
                    return object([
                        ("ok", type_json(types, *ok_ty, next)),
                        ("err", type_json(types, *err_ty, next)),
                    ]);
                }
            }
            wrap(
                "enum",
                object([
                    ("name", Json::String(name.clone())),
                    (
                        "variants",
                        Json::Object(
                            variants
                                .iter()
                                .map(|v| {
                                    (
                                        v.name.clone(),
                                        Json::Array(
                                            v.payload
                                                .iter()
                                                .map(|t| type_json(types, *t, next))
                                                .collect(),
                                        ),
                                    )
                                })
                                .collect(),
                        ),
                    ),
                ]),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{highlight_json, keywords_json};
    use hollowscript::json::{Json, parse, stringify};

    fn array(text: &str) -> Vec<Json> {
        match parse(text).unwrap() {
            Json::Array(items) => items,
            other => panic!("not an array: {other:?}"),
        }
    }

    fn field<'a>(value: &'a Json, name: &str) -> &'a Json {
        let Json::Object(members) = value else {
            panic!("not an object: {value:?}")
        };
        let Some((_, found)) = members.iter().find(|(n, _)| n == name) else {
            panic!("no member {name}")
        };
        found
    }

    #[test]
    fn highlight_answers_utf16_offsets_after_a_non_bmp_character() {
        // The grinning face is four bytes and two UTF-16 code units, so the
        // identifier after it starts at byte 9 and at unit 7.
        let source = "\"\u{1f600}\" + name";
        let items = array(&highlight_json(source));
        let last = items.last().unwrap();
        assert_eq!(field(last, "class"), &Json::String("identifier".to_owned()));
        let units: Vec<u16> = source.encode_utf16().collect();
        let start = match field(last, "start") {
            Json::Int(n) => usize::try_from(*n).unwrap(),
            other => panic!("{other:?}"),
        };
        let end = match field(last, "end") {
            Json::Int(n) => usize::try_from(*n).unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(String::from_utf16(&units[start..end]).unwrap(), "name");
        assert_eq!(start, 7);
        assert_eq!(source.find("name"), Some(9));
    }

    #[test]
    fn highlight_of_nothing_is_an_empty_array() {
        assert_eq!(highlight_json(""), "[]");
    }

    #[test]
    fn keywords_are_the_lexers_table_with_the_reserved_words_marked() {
        let items = array(&keywords_json());
        assert_eq!(items.len(), 27);
        let word = |i: &Json| match field(i, "word") {
            Json::String(s) => s.clone(),
            other => panic!("{other:?}"),
        };
        let future: Vec<String> = items
            .iter()
            .filter(|i| field(i, "future") == &Json::Bool(true))
            .map(word)
            .collect();
        assert_eq!(future, vec!["assert".to_owned(), "test".to_owned()]);
        assert_eq!(word(items.first().unwrap()), "and");
        assert_eq!(word(items.last().unwrap()), "while");
    }

    // ---- the analysis handle ------------------------------------------------

    use super::{
        Analysis, MAX_LIVE_ANALYSES, analyse, dispose, standard_capabilities_json, with_analysis,
    };

    /// The UTF-16 offset of the first `needle` in `text`.
    fn unit(text: &str, needle: &str) -> u32 {
        let at = text.find(needle).unwrap();
        u32::try_from(text[..at].encode_utf16().count()).unwrap()
    }

    fn int(value: &Json) -> u32 {
        match value {
            Json::Int(n) => u32::try_from(*n).unwrap(),
            other => panic!("not an int: {other:?}"),
        }
    }

    fn string(value: &Json) -> String {
        match value {
            Json::String(s) => s.clone(),
            other => panic!("not a string: {other:?}"),
        }
    }

    fn analysis(source: &str) -> Analysis {
        Analysis::new(source, "main.hws", "", "").unwrap()
    }

    // A grinning face before everything: four bytes and two UTF-16 code
    // units, so a byte offset and a UTF-16 offset differ for all that follows.
    const WITH_EMOJI: &str =
        "constant face = \"\u{1f600}\"\nconstant total = 1 + 2\nreturn total\n";

    #[test]
    fn hover_answers_utf16_offsets_after_a_non_bmp_character() {
        let a = analysis(WITH_EMOJI);
        let sum = unit(WITH_EMOJI, "+ 2");
        let json = parse(&a.hover_json(sum)).unwrap();
        assert_eq!(string(field(&json, "type")), "Int");
        let start = int(field(&json, "start"));
        let end = int(field(&json, "end"));
        let units: Vec<u16> = WITH_EMOJI.encode_utf16().collect();
        let hovered = String::from_utf16(&units[start as usize..end as usize]).unwrap();
        assert_eq!(hovered, "1 + 2");
        assert_eq!(start, unit(WITH_EMOJI, "1 + 2"));
        // The byte offset of the same place is two larger: the answers are
        // not bytes.
        assert_eq!(WITH_EMOJI.find("1 + 2").unwrap(), start as usize + 2);
        // Nothing to hover on a keyword.
        assert_eq!(a.hover_json(unit(WITH_EMOJI, "constant total")), "null");
    }

    #[test]
    fn completions_are_found_at_a_utf16_offset_after_a_non_bmp_character() {
        let a = analysis(WITH_EMOJI);
        let items = array(&a.completions_json(unit(WITH_EMOJI, "return total")));
        let find = |label: &str| {
            items
                .iter()
                .find(|i| string(field(i, "label")) == label)
                .unwrap_or_else(|| panic!("no completion {label}"))
        };
        assert_eq!(string(field(find("total"), "kind")), "constant");
        assert_eq!(string(field(find("total"), "detail")), "Int");
        assert_eq!(string(field(find("face"), "detail")), "String");
        assert_eq!(string(field(find("sqrt"), "kind")), "function");
        assert_eq!(string(field(find("return"), "kind")), "keyword");
        // Before the declaration of `total` it is not in scope yet.
        let early = array(&a.completions_json(unit(WITH_EMOJI, "constant total")));
        assert!(early.iter().all(|i| string(field(i, "label")) != "total"));
        // Inside the string there is nothing to complete.
        assert_eq!(a.completions_json(unit(WITH_EMOJI, "\u{1f600}") + 1), "[]");
    }

    #[test]
    fn definition_points_into_the_imported_file_in_that_files_utf16_units() {
        let util = "// \u{1f600}\nexport constant two = 2\n";
        let modules = format!(
            "{{\"util.hws\": {}}}",
            stringify(&Json::String(util.to_owned()))
        );
        let source = "import two from \"./util\"\nreturn two\n";
        let a = Analysis::new(source, "main.hws", "", &modules).unwrap();
        let found = parse(&a.definition_json(unit(source, "two\n"))).unwrap();
        assert_eq!(string(field(&found, "file")), "util.hws");
        let units: Vec<u16> = util.encode_utf16().collect();
        let start = int(field(&found, "start")) as usize;
        let end = int(field(&found, "end")) as usize;
        assert_eq!(String::from_utf16(&units[start..end]).unwrap(), "two");
        assert_eq!(start, util.encode_utf16().count() - "two = 2\n".len());
        // A name declared in this module answers this module's own name.
        let own_source = "// \u{1f600}\nconstant here = 1\nreturn here\n";
        let own = analysis(own_source);
        let found = parse(&own.definition_json(unit(own_source, "here\n"))).unwrap();
        assert_eq!(string(field(&found, "file")), "main.hws");
        assert_eq!(int(field(&found, "start")), unit(own_source, "here = 1"));
        // Nothing to go to on a prelude name.
        assert_eq!(analysis("return sqrt(4.0)\n").definition_json(7), "null");
    }

    #[test]
    fn symbols_are_a_tree_with_utf16_ranges_and_the_extra_fields() {
        let source = concat!(
            "// \u{1f600}\n",
            "uses print\n",
            "export fn add(a: Int, b: Int): Int {\n",
            "    return a + b\n",
            "}\n",
            "enum Shape { Circle(Float), Empty }\n",
        );
        let a = analysis(source);
        let items = array(&a.symbols_json());
        let names: Vec<String> = items.iter().map(|i| string(field(i, "name"))).collect();
        assert_eq!(names, vec!["print", "add", "Shape"]);
        let add = &items[1];
        assert_eq!(string(field(add, "kind")), "function");
        assert_eq!(string(field(add, "detail")), "fn(a: Int, b: Int): Int");
        assert_eq!(field(add, "exported"), &Json::Bool(true));
        assert_eq!(int(field(add, "start")), unit(source, "export fn add"));
        assert_eq!(int(field(add, "selStart")), unit(source, "add("));
        assert_eq!(int(field(add, "selEnd")), unit(source, "add(") + 3);
        assert!(int(field(add, "end")) > int(field(add, "selEnd")));
        assert_eq!(field(add, "children"), &Json::Array(Vec::new()));
        let shape = &items[2];
        assert_eq!(field(shape, "exported"), &Json::Bool(false));
        let Json::Array(children) = field(shape, "children") else {
            panic!("children is not an array")
        };
        assert_eq!(string(field(&children[0], "kind")), "enumMember");
        assert_eq!(string(field(&children[0], "detail")), "(Float)");
        assert_eq!(int(field(&children[1], "selStart")), unit(source, "Empty"));
        assert_eq!(string(field(&items[0], "kind")), "capability");
    }

    #[test]
    fn a_declared_host_capability_is_known_to_the_analysis() {
        let source = "uses tasks\nreturn effect tasks(\"x\")\n";
        let capabilities = r#"[{"name":"tasks","params":["String"],"result":"Int"}]"#;
        let a = Analysis::new(source, "main.hws", capabilities, "").unwrap();
        let json = parse(&a.hover_json(unit(source, "(\"x\")"))).unwrap();
        assert_eq!(string(field(&json, "type")), "Int");
        // Without the declaration the call has no type.
        assert_eq!(analysis(source).hover_json(unit(source, "(\"x\")")), "null");
    }

    #[test]
    fn malformed_arguments_are_refused_with_a_reason() {
        let reason = |capabilities: &str, modules: &str| {
            Analysis::new("return 1", "main.hws", capabilities, modules)
                .err()
                .unwrap()
        };
        assert!(reason("{", "").contains("capabilit"));
        assert!(reason("{}", "").contains("array"));
        assert!(reason("", "[").contains("modules"));
        assert!(reason("", "[]").contains("object"));
        let big = "a".repeat(hollowscript_core::source::MAX_SOURCE_BYTES + 1);
        assert!(
            Analysis::new(&big, "main.hws", "", "")
                .err()
                .unwrap()
                .contains("size limit")
        );
    }

    #[test]
    fn handles_are_looked_up_and_never_reused() {
        let first = analyse("return 1", "main.hws", "", "").unwrap();
        let second = analyse("return 2", "main.hws", "", "").unwrap();
        assert_ne!(first, 0);
        assert!(second > first);
        assert!(with_analysis(first, Analysis::symbols_json).is_some());
        dispose(first);
        // Disposed: refused, and disposing again is harmless.
        assert!(with_analysis(first, Analysis::symbols_json).is_none());
        dispose(first);
        // The other one is untouched, and a new analysis does not take the
        // disposed number.
        assert!(with_analysis(second, Analysis::symbols_json).is_some());
        let third = analyse("return 3", "main.hws", "", "").unwrap();
        assert!(third > second);
        assert!(with_analysis(first, Analysis::symbols_json).is_none());
        // A number never handed out names nothing.
        assert!(with_analysis(0, Analysis::symbols_json).is_none());
        assert!(with_analysis(9999, Analysis::symbols_json).is_none());
        dispose(second);
        dispose(third);
    }

    #[test]
    fn a_page_that_never_disposes_is_stopped_at_the_limit() {
        let mut held = Vec::new();
        for _ in 0..MAX_LIVE_ANALYSES {
            held.push(analyse("return 1", "main.hws", "", "").unwrap());
        }
        let refused = analyse("return 1", "main.hws", "", "").unwrap_err();
        assert!(refused.contains("dispose"), "{refused}");
        // Disposing one makes room.
        dispose(held.pop().unwrap());
        assert!(analyse("return 1", "main.hws", "", "").is_ok());
        for handle in held {
            dispose(handle);
        }
    }

    // ---- the standard capabilities ------------------------------------------

    #[test]
    fn standard_capabilities_are_listed_in_the_shape_check_accepts() {
        let entries = array(&standard_capabilities_json());
        let shape: Vec<(String, Option<String>)> = entries
            .iter()
            .map(|e| {
                let function = match e {
                    Json::Object(members) => members
                        .iter()
                        .find(|(n, _)| n == "function")
                        .map(|(_, v)| string(v)),
                    _ => None,
                };
                (string(field(e, "name")), function)
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("print".to_owned(), None),
                ("clock".to_owned(), Some("now".to_owned())),
                ("clock".to_owned(), Some("today".to_owned())),
                ("clock".to_owned(), Some("unixMillis".to_owned())),
                ("clock".to_owned(), Some("monotonicMillis".to_owned())),
                ("random".to_owned(), Some("int".to_owned())),
                ("random".to_owned(), Some("float".to_owned())),
            ]
        );
        // `print` takes a string and has no result, so no `result` member.
        let print = &entries[0];
        assert_eq!(
            field(print, "params"),
            &Json::Array(vec![Json::String("String".to_owned())])
        );
        let Json::Object(members) = print else {
            panic!()
        };
        assert!(members.iter().all(|(n, _)| n != "result"));
        // `random.int` takes two integers and answers one.
        let random_int = &entries[5];
        assert_eq!(
            field(random_int, "params"),
            &Json::Array(vec![
                Json::String("Int".to_owned()),
                Json::String("Int".to_owned())
            ])
        );
        assert_eq!(string(field(random_int, "result")), "Int");
        assert_eq!(string(field(&entries[1], "result")), "String");
        assert_eq!(string(field(&entries[3], "result")), "Int");
        assert_eq!(string(field(&entries[6], "result")), "Float");
    }

    #[test]
    fn the_standard_capabilities_read_back_to_what_they_were() {
        // What the page is given is accepted as the `capabilities` argument
        // and means the same thing: reading it and writing it again gives
        // the same text.
        let first = standard_capabilities_json();
        let mut types = hollowscript_core::transfer::TypeTable::new();
        let decls = super::super::extra_capabilities(&first, &mut types).unwrap();
        let again = stringify(&super::capabilities_to_json(&decls, &types));
        assert_eq!(first, again);
        // And handing them back changes nothing about a check that already
        // grants them.
        let source = "uses print\nuses clock\nuses random\neffect print(effect clock.now())\n";
        let with = Analysis::new(source, "main.hws", &first, "").unwrap();
        let without = analysis(source);
        let offset = unit(source, "clock.now");
        assert_eq!(with.hover_json(offset), without.hover_json(offset));
        assert!(with.hover_json(offset) != "null");
    }
}
