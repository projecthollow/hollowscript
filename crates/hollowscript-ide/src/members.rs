//! Member completion uses checked types, including malformed member calls.
use crate::complete::{Completion, CompletionKind};
use crate::symbols::{Kind, definition_of, render_type, word_at};
use crate::workspace::Workspace;
use hollowscript_check::types::TypeView;
use hollowscript_syntax::{TokenKind, ast};

impl Workspace {
    pub(crate) fn member_completions(&self, offset: u32) -> Vec<Completion> {
        let start = word_at(&self.text, offset as usize).map_or(offset, |(_, s)| s.start);
        let Some(dot) =
            self.tokens.iter().rev().find(|t| {
                t.span.end <= start && !matches!(t.kind, TokenKind::Term | TokenKind::Eof)
            })
        else {
            return Vec::new();
        };
        if dot.kind != TokenKind::Dot {
            return Vec::new();
        }
        let Some(receiver) = self.tokens.iter().rev().find(|t| {
            t.span.end <= dot.span.start && !matches!(t.kind, TokenKind::Term | TokenKind::Eof)
        }) else {
            return Vec::new();
        };
        let receiver_end = receiver.span.end;
        // Give the unfinished member a syntactically valid call. The checker
        // still records the receiver's type when the placeholder is rejected.
        // No source is edited, compiled or executed by this query.
        let end = self
            .tokens
            .iter()
            .find(|t| t.span.start >= dot.span.end)
            .filter(|t| t.kind == TokenKind::Ident && t.span.start <= offset)
            .map_or(dot.span.end, |t| t.span.end);
        let Some(before) = self.text.get(..dot.span.end as usize) else {
            return Vec::new();
        };
        let Some(after) = self.text.get(end as usize..) else {
            return Vec::new();
        };
        let text = format!("{before}__hws_completion(){after}");
        let Some(query) = Workspace::from_modules(
            &self.entry_name,
            &text,
            &self.loaded_modules,
            &self.capabilities,
        ) else {
            return Vec::new();
        };
        let name = self
            .text
            .get(receiver.span.start as usize..receiver_end as usize)
            .unwrap_or("");
        let bare = receiver.kind == TokenKind::Ident
            && self
                .tokens
                .iter()
                .rev()
                .find(|t| {
                    t.span.end <= receiver.span.start
                        && !matches!(t.kind, TokenKind::Term | TokenKind::Eof)
                })
                .is_none_or(|t| t.kind != TokenKind::Dot);
        let binder = if bare {
            definition_of(query.binders(), name, receiver.span.start)
        } else {
            None
        };
        if let Some(binder) = binder.filter(|b| b.kind == Kind::Enum) {
            if let Some(file) = query.file_of(&binder.decl.module) {
                for item in &file.items {
                    if let ast::ItemKind::Enum(decl) = &item.kind {
                        if decl.name.span == binder.decl.span {
                            return decl
                                .variants
                                .iter()
                                .map(|v| {
                                    let label = query.names.resolve(v.name.name).to_owned();
                                    let payload = v
                                        .payload
                                        .iter()
                                        .map(|t| render_type(t, &query.names))
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    Completion {
                                        detail: if payload.is_empty() {
                                            label.clone()
                                        } else {
                                            format!("{label}({payload})")
                                        },
                                        label,
                                        kind: CompletionKind::EnumMember,
                                    }
                                })
                                .collect();
                        }
                    }
                }
            }
        }
        // A local binding takes precedence over prelude namespaces and types.
        if bare && binder.is_none() {
            match name {
                "json" | "time" => return methods(name),
                "Json" => {
                    return variants(&[
                        "Null",
                        "Bool(Bool)",
                        "Int(Int)",
                        "Float(Float)",
                        "String(String)",
                        "Array([Json])",
                        "Object(Map<String, Json>)",
                    ]);
                }
                "Result" => return variants(&["Ok(T)", "Err(E)"]),
                _ => {}
            }
        }
        let Some((_, ty)) = query.types.at(receiver_end.saturating_sub(1)) else {
            return Vec::new();
        };
        let mut types = query.hir.types.clone();
        let ty = types.expand_record(&query.names, ty).unwrap_or(ty);
        match types.view(ty) {
            TypeView::Record(fields) => fields
                .iter()
                .map(|(name, ty)| Completion {
                    label: query.names.resolve(*name).to_owned(),
                    kind: CompletionKind::Field,
                    detail: types.display(*ty, |s| query.names.resolve(s).to_owned()),
                })
                .collect(),
            TypeView::String => methods("string"),
            TypeView::List(element) => methods("list")
                .into_iter()
                .filter(|m| m.label != "join" || matches!(types.view(element), TypeView::String))
                .collect(),
            TypeView::Map(_, _) => methods("map"),
            TypeView::Set(_) => methods("set"),
            _ if types.as_result(ty).is_some() => methods("result"),
            _ => Vec::new(),
        }
    }
}

fn methods(group: &str) -> Vec<Completion> {
    crate::member_catalog::METHODS
        .iter()
        .filter(|(g, _, _)| *g == group)
        .map(|(_, name, signature)| Completion {
            label: (*name).to_owned(),
            kind: CompletionKind::Method,
            detail: (*signature).to_owned(),
        })
        .collect()
}

fn variants(signatures: &[&str]) -> Vec<Completion> {
    signatures
        .iter()
        .map(|s| Completion {
            label: s.split('(').next().unwrap_or(s).to_owned(),
            kind: CompletionKind::EnumMember,
            detail: (*s).to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::testing::workspace;

    fn labels(text: &str) -> Vec<String> {
        workspace(text)
            .completions(u32::try_from(text.len()).unwrap())
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    #[test]
    fn incomplete_members_and_calls_use_the_receiver_type() {
        for text in [
            "constant s = \"hi\"\nreturn s.",
            "constant s = \"hi\"\ns.",
            "return \"hi\".tr",
            "return \"hi\".trim().",
        ] {
            let names = labels(text);
            assert!(names.contains(&"trim".to_owned()), "{text}: {names:?}");
            assert!(!names.contains(&"append".to_owned()));
        }
        assert!(labels("constant xs = [1]\nreturn xs.").contains(&"append".to_owned()));
        assert!(labels("return [\"a\": 1].").contains(&"get".to_owned()));
        assert!(labels("return set([1]).").contains(&"union".to_owned()));
        assert!(
            labels("constant r: Result<Int, String> = success(1)\nreturn r.")
                .contains(&"unwrapOr".to_owned())
        );
    }

    #[test]
    fn fields_generics_narrowing_and_shadowing() {
        assert_eq!(
            labels("record Box<T> = { value: T }\nconstant b: Box<Int> = { value: 1 }\nreturn b."),
            ["value"]
        );
        assert_eq!(
            labels("constant p = { child: { age: 1 } }\nreturn p.child."),
            ["age"]
        );
        let names = labels("fn f(s: String?): String {\n if s != none {\n return s.");
        assert!(names.contains(&"trim".to_owned()), "{names:?}");
        assert_eq!(
            labels("constant json = { local: 1 }\nreturn json."),
            ["local"]
        );
        assert!(labels("constant x: String? = none\nreturn x.").is_empty());
        assert!(labels("return missing.").is_empty());
        assert_eq!(
            labels("constant p = { json: { age: 1 } }\nreturn p.json."),
            ["age"]
        );
        assert!(!labels("return [1].").contains(&"join".to_owned()));
        assert!(labels("return [\"a\"].").contains(&"join".to_owned()));
    }

    #[test]
    fn enums_namespaces_and_non_code() {
        assert_eq!(
            labels("enum Choice<T> { First(T), Last }\nreturn Choice."),
            ["First", "Last"]
        );
        assert!(labels("return Json.").contains(&"Object".to_owned()));
        assert!(labels("return json.").contains(&"parse".to_owned()));
        assert!(labels("return time.").contains(&"parts".to_owned()));
        for text in ["// x.", "return \"x.", "return `x.", "return 1."] {
            assert!(labels(text).is_empty(), "{text}");
        }
    }
}
