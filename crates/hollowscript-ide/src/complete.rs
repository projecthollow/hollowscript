//! Completion: the names in scope at a position, then the prelude and the
//! keywords.
//!
//! The list is the same wherever in a word the cursor sits, so a client can
//! filter it by what has been typed; what changes with the position is
//! scope. A name comes into view where section 4.6 says it does: a local
//! after its declaration and to the end of its block, a parameter in its
//! body, a top-level `fn`, `record`, `enum`, import and capability anywhere
//! in the file, and a top-level `constant` or `variable` in code that runs
//! as it is read only after its declaration, but in every function body
//! wherever it is declared.
//!
//! Nothing is offered where a name cannot be written: inside a comment, in
//! the text of a string or a filter literal (but inside a `${ }`
//! interpolation, yes), or after a `.`, where a field or method name goes.
//! **Known gap:** names after a `.` are not completed at all. That needs the
//! type of the receiver and the members of that type, which this does not
//! look up, so the list is empty there rather than a list of names that
//! would be wrong.

use crate::symbols::{Kind, visible_in_order};
use crate::vocabulary::{PRELUDE_NAMES, usable_keywords};
use crate::workspace::{Place, Workspace};
use hollowscript_syntax::ast;

/// What a completion is, which a client turns into an icon.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    /// A `variable`, a `for` variable or a name a pattern bound.
    Variable,
    /// A `constant`.
    Constant,
    /// A `fn`, or a prelude function.
    Function,
    /// A parameter of a function or a closure.
    Parameter,
    /// A `record`, or a type the prelude provides.
    Type,
    /// An `enum`.
    Enum,
    /// A variant of an enum. Not offered yet: a variant is written through
    /// its enum, which is the member completion this does not do.
    EnumMember,
    /// A capability named by a `uses` line.
    Capability,
    /// A namespace of the prelude, `json` or `time`.
    Namespace,
    /// A reserved word.
    Keyword,
}

impl CompletionKind {
    /// The kind as a word, in the spelling a host sees: `variable`,
    /// `constant`, `function`, `parameter`, `type`, `enum`, `enumMember`,
    /// `capability`, `namespace` or `keyword`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CompletionKind::Variable => "variable",
            CompletionKind::Constant => "constant",
            CompletionKind::Function => "function",
            CompletionKind::Parameter => "parameter",
            CompletionKind::Type => "type",
            CompletionKind::Enum => "enum",
            CompletionKind::EnumMember => "enumMember",
            CompletionKind::Capability => "capability",
            CompletionKind::Namespace => "namespace",
            CompletionKind::Keyword => "keyword",
        }
    }
}

/// One completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The name or word to insert.
    pub label: String,
    /// What it is.
    pub kind: CompletionKind,
    /// A signature or type to show beside it, or empty.
    pub detail: String,
}

/// The prelude's type names (section 2.1), offered where a type is written.
const PRELUDE_TYPES: &[&str] = &[
    "Bool",
    "Filter",
    "Float",
    "HttpError",
    "HttpRequest",
    "HttpResponse",
    "Int",
    "IoError",
    "Json",
    "JsonError",
    "Map",
    "MapEntry",
    "Result",
    "Set",
    "String",
    "TimeParts",
];

fn kind_of(kind: Kind) -> CompletionKind {
    match kind {
        Kind::Constant => CompletionKind::Constant,
        Kind::Variable | Kind::Binding => CompletionKind::Variable,
        Kind::Function => CompletionKind::Function,
        Kind::Parameter => CompletionKind::Parameter,
        Kind::Record => CompletionKind::Type,
        Kind::Enum => CompletionKind::Enum,
        Kind::Capability => CompletionKind::Capability,
    }
}

impl Workspace {
    /// Whether `offset` is inside the body of a top-level `fn`.
    fn in_function_body(&self, offset: u32) -> bool {
        self.file.items.iter().any(|item| match &item.kind {
            ast::ItemKind::Fn(decl) => {
                offset >= decl.body.span.start && offset <= decl.body.span.end
            }
            _ => false,
        })
    }

    /// What can be written at `offset`: the names in scope there, in the
    /// order they are declared; then the prelude's names and types, except
    /// any the program declares itself; then the keywords a program can
    /// write today (the words reserved for a later version are an error
    /// wherever they appear, so they are left out).
    ///
    /// Empty where a name cannot be written (see the module documentation).
    #[must_use]
    pub fn completions(&self, offset: u32) -> Vec<Completion> {
        if self.place_at(offset) != Place::Code {
            return Vec::new();
        }
        let mut out: Vec<Completion> =
            visible_in_order(self.binders(), offset, self.in_function_body(offset))
                .into_iter()
                .map(|binder| Completion {
                    label: binder.name.clone(),
                    kind: kind_of(binder.kind),
                    detail: binder.detail.clone(),
                })
                .collect();
        let declared: Vec<String> = self.binders().iter().map(|b| b.name.clone()).collect();
        let mut push_prelude = |label: &str, kind: CompletionKind, detail: &str| {
            if !declared.iter().any(|name| name == label) {
                out.push(Completion {
                    label: label.to_owned(),
                    kind,
                    detail: detail.to_owned(),
                });
            }
        };
        for name in PRELUDE_NAMES {
            match *name {
                "pi" => push_prelude(name, CompletionKind::Constant, "Float"),
                "json" | "time" => push_prelude(name, CompletionKind::Namespace, "prelude"),
                _ => push_prelude(name, CompletionKind::Function, "prelude"),
            }
        }
        for name in PRELUDE_TYPES {
            push_prelude(name, CompletionKind::Type, "prelude");
        }
        for word in usable_keywords() {
            out.push(Completion {
                label: word.to_owned(),
                kind: CompletionKind::Keyword,
                detail: "keyword".to_owned(),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{Completion, CompletionKind};
    use crate::testing::{offset, workspace};

    fn labels(items: &[Completion]) -> Vec<&str> {
        items.iter().map(|c| c.label.as_str()).collect()
    }

    fn find<'a>(items: &'a [Completion], label: &str) -> &'a Completion {
        items.iter().find(|c| c.label == label).unwrap()
    }

    #[test]
    fn names_in_scope_come_first_in_declaration_order_then_the_prelude_then_keywords() {
        let text = concat!(
            "uses print\n",
            "constant first = 1\n",
            "fn second(): Int {\n",
            "    return 2\n",
            "}\n",
            "constant third = 3\n",
            "return first + third\n",
        );
        let w = workspace(text);
        let items = w.completions(offset(text, "return first"));
        let names = labels(&items);
        let at = |name: &str| names.iter().position(|n| *n == name).unwrap();
        assert!(at("print") < at("first"));
        assert!(at("first") < at("second"));
        assert!(at("second") < at("third"));
        assert!(at("third") < at("sqrt"));
        assert!(at("sqrt") < at("return"));
        assert_eq!(find(&items, "print").kind, CompletionKind::Capability);
        assert_eq!(find(&items, "first").kind, CompletionKind::Constant);
        assert_eq!(find(&items, "second").kind, CompletionKind::Function);
        assert_eq!(find(&items, "sqrt").kind, CompletionKind::Function);
        assert_eq!(find(&items, "pi").kind, CompletionKind::Constant);
        assert_eq!(find(&items, "json").kind, CompletionKind::Namespace);
        assert_eq!(find(&items, "Int").kind, CompletionKind::Type);
        assert_eq!(find(&items, "return").kind, CompletionKind::Keyword);
    }

    #[test]
    fn a_name_is_not_in_scope_before_its_declaration_in_code_that_runs_in_order() {
        let text = concat!(
            "constant early = 1\n",
            "constant probe = early\n",
            "constant late = 2\n",
            "return late\n",
        );
        let w = workspace(text);
        // Inside the initialiser of `probe`: `early` is declared, `probe`
        // (still being declared) and `late` are not yet.
        let items = w.completions(offset(text, "early\nconstant late"));
        let names = labels(&items);
        assert!(names.contains(&"early"), "{names:?}");
        assert!(!names.contains(&"probe"), "{names:?}");
        assert!(!names.contains(&"late"), "{names:?}");
        // After everything, all three.
        let items = w.completions(u32::try_from(text.len()).unwrap());
        let names = labels(&items);
        for name in ["early", "probe", "late"] {
            assert!(names.contains(&name), "{names:?}");
        }
    }

    #[test]
    fn a_top_level_constant_declared_later_is_in_scope_inside_a_function_body() {
        let text = concat!(
            "fn read(): Int {\n",
            "    return base\n",
            "}\n",
            "constant base = 5\n",
        );
        let w = workspace(text);
        let items = w.completions(offset(text, "base\n}"));
        assert!(labels(&items).contains(&"base"));
        // But not in the parameter list, outside the body.
        let text = concat!("fn read(n: Int): Int {\n", "    return n\n", "}\n");
        let w = workspace(text);
        let items = w.completions(offset(text, "n: Int"));
        assert!(!labels(&items).contains(&"n"));
    }

    #[test]
    fn locals_and_parameters_are_scoped_to_their_blocks() {
        let text = concat!(
            "fn f(param: Int): Int {\n",
            "    constant before = 1\n",
            "    if param > 0 {\n",
            "        constant inner = 2\n",
            "        return inner\n",
            "    }\n",
            "    constant after = 3\n",
            "    return before + after\n",
            "}\n",
        );
        let w = workspace(text);
        let in_if = w.completions(offset(text, "inner\n    }"));
        let names = labels(&in_if);
        for name in ["param", "before", "inner", "f"] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
        assert!(!names.contains(&"after"), "{names:?}");
        assert_eq!(find(&in_if, "param").kind, CompletionKind::Parameter);
        assert_eq!(find(&in_if, "param").detail, "Int");
        let at_end = w.completions(offset(text, "after\n}"));
        let names = labels(&at_end);
        assert!(
            names.contains(&"before") && names.contains(&"after"),
            "{names:?}"
        );
        assert!(!names.contains(&"inner"), "{names:?}");
    }

    #[test]
    fn a_closure_parameter_is_in_scope_in_its_body_only() {
        let text = concat!(
            "constant doubled = [1, 2].map(fn(n: Int): Int {\n",
            "    return n * 2\n",
            "})\n",
            "return doubled\n",
        );
        let w = workspace(text);
        let inside = w.completions(offset(text, "n * 2"));
        assert_eq!(find(&inside, "n").kind, CompletionKind::Parameter);
        assert_eq!(find(&inside, "n").detail, "Int");
        let outside = w.completions(offset(text, "doubled\n") + 7);
        assert!(!labels(&outside).contains(&"n"));
    }

    #[test]
    fn details_are_signatures_and_types() {
        let text = concat!(
            "fn add(a: Int, b: Int): Int {\n",
            "    return a + b\n",
            "}\n",
            "constant annotated: Float = 1\n",
            "constant inferred = [1, 2]\n",
            "return add(1, 2)\n",
        );
        let w = workspace(text);
        let items = w.completions(offset(text, "return add"));
        assert_eq!(find(&items, "add").detail, "fn(a: Int, b: Int): Int");
        assert_eq!(find(&items, "annotated").detail, "Float");
        assert_eq!(find(&items, "inferred").detail, "[Int]");
    }

    #[test]
    fn for_variables_and_pattern_bindings_are_offered_with_their_types() {
        let text = concat!(
            "constant names = [\"a\", \"b\"]\n",
            "for name in names {\n",
            "    constant shout = name\n",
            "    return shout\n",
            "}\n",
        );
        let w = workspace(text);
        let items = w.completions(offset(text, "shout\n}"));
        assert_eq!(find(&items, "name").kind, CompletionKind::Variable);
        assert_eq!(find(&items, "name").detail, "String");
        assert_eq!(find(&items, "shout").detail, "String");
    }

    #[test]
    fn types_enums_and_imports_have_their_own_kinds() {
        let modules: std::collections::BTreeMap<String, String> = [(
            "util.hws".to_owned(),
            concat!(
                "export fn twice(n: Int): Int {\n",
                "    return n * 2\n",
                "}\n",
                "export constant limit = 10\n",
            )
            .to_owned(),
        )]
        .into_iter()
        .collect();
        let text = concat!(
            "import twice, limit from \"./util\"\n",
            "record Point = { x: Int }\n",
            "enum Shape { Circle(Float), Empty }\n",
            "return twice(limit)\n",
        );
        let w = crate::Workspace::from_modules(
            "main.hws",
            text,
            &modules,
            &hollowscript_check::CapabilityInterface::default(),
        )
        .unwrap();
        let items = w.completions(offset(text, "return twice"));
        assert_eq!(find(&items, "Point").kind, CompletionKind::Type);
        assert_eq!(find(&items, "Shape").kind, CompletionKind::Enum);
        assert_eq!(find(&items, "twice").kind, CompletionKind::Function);
        assert_eq!(find(&items, "twice").detail, "fn(n: Int): Int");
        assert_eq!(find(&items, "limit").kind, CompletionKind::Constant);
        assert_eq!(find(&items, "limit").detail, "Int");
    }

    #[test]
    fn the_prelude_name_a_program_declares_is_not_offered_twice() {
        let text = concat!(
            "fn min(a: Int, b: Int): Int {\n",
            "    return a\n",
            "}\n",
            "return min(1, 2)\n",
        );
        let w = workspace(text);
        let items = w.completions(offset(text, "return min"));
        assert_eq!(labels(&items).iter().filter(|l| **l == "min").count(), 1);
        assert_eq!(find(&items, "min").detail, "fn(a: Int, b: Int): Int");
    }

    #[test]
    fn every_usable_keyword_is_offered_and_the_reserved_ones_are_not() {
        let w = workspace("return 1\n");
        let items = w.completions(0);
        let names = labels(&items);
        for word in hollowscript_syntax::Keyword::ALL {
            assert_eq!(
                names.contains(&word.as_str()),
                !word.is_future(),
                "{}",
                word.as_str()
            );
        }
    }

    #[test]
    fn nothing_is_offered_in_a_comment_or_in_string_text() {
        let text = concat!(
            "constant total = 1\n",
            "// total here\n",
            "constant s = \"total ${total} total\"\n",
        );
        let w = workspace(text);
        assert!(w.completions(offset(text, "here")).is_empty());
        assert!(w.completions(offset(text, "total ${")).is_empty());
        assert!(w.completions(offset(text, " total\"")).is_empty());
        // Inside the interpolation, names are offered.
        assert!(!w.completions(offset(text, "total}")).is_empty());
        // After the string, too.
        let after = offset(text, "total\"\n") + 6;
        assert!(!w.completions(after).is_empty());
    }

    #[test]
    fn nothing_is_offered_after_a_dot() {
        let text = concat!("constant list = [1]\n", "return list.length()\n");
        let w = workspace(text);
        assert!(w.completions(offset(text, "length")).is_empty());
        assert!(w.completions(offset(text, "length") + 3).is_empty());
        assert!(w.completions(offset(text, "length()")).is_empty());
    }

    #[test]
    fn a_file_that_does_not_parse_still_completes() {
        let text = "constant known = 1\nconstant broken = \n";
        let w = workspace(text);
        let items = w.completions(u32::try_from(text.len()).unwrap());
        assert!(labels(&items).contains(&"known"));
    }
}
