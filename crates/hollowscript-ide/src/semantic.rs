//! Semantic tokens: every identifier this crate already resolves to a
//! declaration, classified by what that declaration is.
//!
//! Scope: a function, a value, a parameter or pattern binding, a
//! `record`/`enum` type, or a capability named by `uses`. A keyword, a
//! literal and a comment are left to lexical highlighting
//! ([`crate::highlight`]), which tokenises them from the lexical grammar
//! alone and does not need the checker; layering semantic tokens only over
//! what genuinely needs name resolution is the usual division between the
//! two, and it is what keeps this a reuse of [`crate::symbols::binders`] and
//! [`crate::symbols::occurrences`] rather than a second tokeniser. A name
//! that only resolves to a prelude function (section 13.6) has no
//! declaration in the program, so it is not one of these tokens either.

use crate::symbols::{Kind, definition_of, occurrences};
use crate::workspace::Workspace;
use hollowscript_core::Span;

/// What a semantic token is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SemanticKind {
    /// A function.
    Function,
    /// A constant or a variable.
    Variable,
    /// A parameter or a name a pattern or `for` bound.
    Parameter,
    /// A record or an enum.
    Type,
    /// A capability named by a `uses` line.
    Namespace,
}

/// One classified identifier.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SemanticToken {
    /// The span of the identifier, always within one line.
    pub span: Span,
    /// What it is.
    pub kind: SemanticKind,
}

const fn semantic_kind(kind: Kind) -> SemanticKind {
    match kind {
        Kind::Function => SemanticKind::Function,
        Kind::Constant | Kind::Variable => SemanticKind::Variable,
        Kind::Parameter | Kind::Binding => SemanticKind::Parameter,
        Kind::Record | Kind::Enum => SemanticKind::Type,
        Kind::Capability => SemanticKind::Namespace,
    }
}

impl Workspace {
    /// The semantic tokens of the entry module, sorted by where they start
    /// with none repeated.
    #[must_use]
    pub fn semantic_tokens(&self) -> Vec<SemanticToken> {
        let binders = self.binders();
        let mut tokens: Vec<SemanticToken> = Vec::new();
        for binder in binders {
            tokens.push(SemanticToken {
                span: binder.at,
                kind: semantic_kind(binder.kind),
            });
        }
        for ident in occurrences(&self.file) {
            let name = self.names.resolve(ident.name);
            if let Some(binder) = definition_of(binders, name, ident.span.start) {
                tokens.push(SemanticToken {
                    span: ident.span,
                    kind: semantic_kind(binder.kind),
                });
            }
        }
        tokens.sort_by_key(|token| token.span.start);
        tokens.dedup_by_key(|token| token.span.start);
        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::{SemanticKind, SemanticToken};
    use crate::testing::workspace;

    fn tokens(text: &str) -> Vec<SemanticToken> {
        workspace(text).semantic_tokens()
    }

    #[test]
    fn a_declaration_and_its_use_are_both_tokens() {
        let found = tokens("constant total = 1\nreturn total\n");
        let spans: Vec<(u32, u32)> = found.iter().map(|t| (t.span.start, t.span.end)).collect();
        assert_eq!(spans, vec![(9, 14), (26, 31)]);
        assert!(found.iter().all(|t| t.kind == SemanticKind::Variable));
    }

    #[test]
    fn each_kind_of_declaration_gets_its_own_class() {
        let found = tokens(concat!(
            "uses print\n",
            "record Point = { x: Int }\n",
            "fn id(n: Int): Int {\n",
            "    return n\n",
            "}\n",
        ));
        let kinds: Vec<SemanticKind> = found.iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&SemanticKind::Namespace), "{kinds:?}");
        assert!(kinds.contains(&SemanticKind::Type), "{kinds:?}");
        assert!(kinds.contains(&SemanticKind::Function), "{kinds:?}");
        assert!(kinds.contains(&SemanticKind::Parameter), "{kinds:?}");
    }

    #[test]
    fn a_prelude_name_is_not_a_token() {
        assert!(tokens("return sqrt(4.0)\n").is_empty());
    }
}
