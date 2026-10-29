//! One document, checked with its imports loaded for real: the front end
//! every position-based query in this crate runs against.
//!
//! A query needs two different views of the program. The checker's own HIR
//! resolves a name used through an import to the exporting module's
//! declaration, which is what makes hover, completion and diagnostics
//! already correct for an imported name once real modules are loaded. But a
//! source-level feature (go to definition landing on the declaration's own
//! span, an outline, a rename) needs the *syntax* of the file the name is
//! declared in, and the checker does not hand back the trees of the modules
//! it loaded, only their typed HIR. So [`Workspace::build`] parses the entry
//! document's own direct imports a second time, the same way the checker
//! resolved them, and keeps their trees and text alongside the checked
//! program.
//!
//! Only direct imports are parsed this way (an import of an import is not
//! followed): every language feature a name can be used through is a direct
//! import (section 10.2 gives a module no way to re-export a name it did not
//! declare itself), so this is complete for what a program can actually
//! write, not a simplification of it.
//!
//! Nothing here touches a disk or a transport. The modules an import names
//! come from a caller-supplied loader ([`Workspace::build`]) or from an
//! in-memory map ([`Workspace::from_modules`]), and the capabilities a
//! program may `uses` come from a caller-supplied
//! [`CapabilityInterface`]. Every span in this crate is a half-open range of
//! UTF-8 byte offsets into the text of one module, as everywhere else in the
//! workspace; a caller that counts another way converts at its own edge.

use crate::symbols::{Binder, Kind, definition_of, fn_signature, render_type, word_at};
use crate::types::{Hover, TypeIndex};
use hollowscript_check::CapabilityInterface;
use hollowscript_core::{Control, Diagnostic, Interner, LoadModuleError, Source, SourceId, Span};
use hollowscript_syntax::ast;
use hollowscript_syntax::{Token, TokenKind};
use std::collections::BTreeMap;

/// A function a caller supplies to answer an `import`: given a canonical
/// module path (`util.hws`, `lib/helpers.hws`, relative to the directory of
/// the entry module's own root, section 10.3), the module's source text, or
/// the reason there is none.
pub type ModuleLoader<'a> = dyn FnMut(&str) -> Result<String, LoadModuleError> + 'a;

/// Where a name is really declared: a module's canonical name and the span
/// of the name in that declaration. Two occurrences that resolve to the same
/// [`DeclId`] are the same name for find references, rename and go to
/// definition, whether or not either occurrence is the one written through
/// an `import`.
///
/// `module` is [`Workspace::entry_name`] for a declaration in the entry
/// module itself and the module's canonical path (as an `import` resolves
/// it) otherwise. It is only meaningful next to the workspace that produced
/// it: two workspaces rooted differently may name the same file differently.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeclId {
    /// The canonical name of the declaring module.
    pub module: String,
    /// The span of the declared name in that module's text.
    pub span: Span,
}

/// What an `import` line brings in for one name: the declaration it
/// resolves to, what kind of thing that is, and its type or signature when
/// there is one to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportTarget {
    /// Where the name is really declared.
    pub decl: DeclId,
    /// What the declaration declares.
    pub kind: Kind,
    /// The declaration's type or signature as a program writes it, or empty.
    pub detail: String,
}

/// One module the entry document imports directly: its canonical path, its
/// text and its parsed tree.
#[derive(Debug)]
pub struct ImportedFile {
    /// The canonical path, as the entry document's `import` resolves it.
    pub canonical_path: String,
    /// The module's text.
    pub text: String,
    /// The module's parsed tree.
    pub file: ast::File,
}

/// A document, checked against its real imports, with each direct import's
/// own tree kept alongside for source-level lookups.
#[derive(Debug)]
pub struct Workspace {
    /// The canonical name of the entry module, which is also the name its
    /// diagnostics carry.
    pub entry_name: String,
    /// The entry module's text.
    pub text: String,
    /// The interner every symbol in the trees and the HIR refers to.
    pub names: Interner,
    /// The entry module's parsed tree.
    pub file: ast::File,
    /// The checked program.
    pub hir: hollowscript_check::hir::Program,
    /// The parse and check diagnostics of the whole program, in the
    /// checker's canonical order.
    pub diagnostics: Vec<Diagnostic>,
    /// The direct imports that resolved, in the order the `import` lines
    /// first name them.
    pub imports: Vec<ImportedFile>,
    /// Where each name this file's `import` lines bring in is really
    /// declared: the exporting module and the span of its name in that
    /// declaration, with what it declares. Absent for a name that did not
    /// resolve (the module was not found, or it does not export that name),
    /// which already has its own diagnostic.
    pub import_targets: BTreeMap<String, ImportTarget>,
    /// Every binder of the entry module, in declaration order.
    binders: Vec<Binder>,
    /// The type of every expression of the entry module, by position.
    types: TypeIndex,
    /// The tokens of the entry module, for what the cursor is in.
    tokens: Vec<Token>,
}

impl Workspace {
    /// Checks the module `text`, named `entry_name`, against
    /// `capabilities`, resolving every import through `load`.
    ///
    /// `load` is called at most once per canonical path: the answer is kept
    /// for the second, syntax-level read of each direct import.
    ///
    /// Answers `None` only when `text` is longer than the source size limit
    /// of section 1.1; any other problem is a diagnostic in the result.
    #[must_use]
    pub fn build(
        entry_name: &str,
        text: &str,
        capabilities: &CapabilityInterface,
        load: &mut ModuleLoader<'_>,
    ) -> Option<Workspace> {
        let mut names = Interner::new();
        let src = Source::new(SourceId(0), entry_name, text).ok()?;
        let mut control = Control::none();
        let parsed = hollowscript_syntax::parse(&src, &mut names, &mut control);

        let mut loaded: BTreeMap<String, Result<String, LoadModuleError>> = BTreeMap::new();
        let mut fetch = |path: &str| -> Result<String, LoadModuleError> {
            loaded
                .entry(path.to_owned())
                .or_insert_with(|| load(path))
                .clone()
        };
        let checked = hollowscript_check::check(
            &parsed.file,
            &src,
            &mut names,
            capabilities,
            None,
            &mut |path| fetch(path),
        );
        let mut diagnostics = parsed.diagnostics.clone();
        diagnostics.extend(checked.diagnostics.clone());

        let dir = dir_of(entry_name);
        let mut imports: Vec<ImportedFile> = Vec::new();
        let mut import_targets = BTreeMap::new();
        for import in &parsed.file.imports {
            let Some(raw) = import_path_text(&import.path) else {
                continue;
            };
            let Ok(Ok(segments)) = hollowscript_check::resolve::resolve_import_path(&dir, &raw)
            else {
                continue;
            };
            let canonical_path = segments.join("/");
            // Already loaded (an earlier import line named the same
            // module): do not parse it twice.
            if !imports.iter().any(|m| m.canonical_path == canonical_path) {
                let Ok(itext) = fetch(&canonical_path) else {
                    continue;
                };
                let Ok(isrc) = Source::new(SourceId(1), canonical_path.as_str(), itext.as_str())
                else {
                    continue;
                };
                let mut icontrol = Control::none();
                let iparsed = hollowscript_syntax::parse(&isrc, &mut names, &mut icontrol);
                imports.push(ImportedFile {
                    canonical_path: canonical_path.clone(),
                    text: itext,
                    file: iparsed.file,
                });
            }
            let Some(module) = imports.iter().find(|m| m.canonical_path == canonical_path) else {
                continue;
            };
            for ident in &import.names {
                let name = names.resolve(ident.name).to_owned();
                let Some(export) = find_export(&module.file, &names, &name) else {
                    continue;
                };
                let detail = if export.detail.is_empty() && export.kind.is_value() {
                    exported_type(&checked.hir, &names, &canonical_path, &name)
                } else {
                    export.detail
                };
                import_targets.insert(
                    name,
                    ImportTarget {
                        decl: DeclId {
                            module: canonical_path.clone(),
                            span: export.span,
                        },
                        kind: export.kind,
                        detail,
                    },
                );
            }
        }

        let whole = Span::new(0, u32::try_from(text.len()).unwrap_or(u32::MAX));
        let mut binders =
            crate::symbols::binders(&parsed.file, &names, whole, entry_name, &import_targets);
        let bound = crate::types::binding_types(&checked.hir, &names);
        for binder in &mut binders {
            if binder.detail.is_empty()
                && matches!(
                    binder.kind,
                    Kind::Constant | Kind::Variable | Kind::Parameter | Kind::Binding
                )
            {
                if let Some(Some(ty)) = bound.get(&binder.name) {
                    binder.detail.clone_from(ty);
                }
            }
        }
        let types = TypeIndex::build(&checked.hir);
        let tokens = hollowscript_syntax::lex(&src, &mut Control::none()).tokens;

        Some(Workspace {
            entry_name: entry_name.to_owned(),
            text: text.to_owned(),
            names,
            file: parsed.file,
            hir: checked.hir,
            diagnostics,
            imports,
            import_targets,
            binders,
            types,
            tokens,
        })
    }

    /// As [`Workspace::build`], resolving every import against `modules`,
    /// an in-memory map from canonical path to source text. A path the map
    /// does not hold is a module that is not found (HS0601).
    #[must_use]
    pub fn from_modules(
        entry_name: &str,
        text: &str,
        modules: &BTreeMap<String, String>,
        capabilities: &CapabilityInterface,
    ) -> Option<Workspace> {
        Workspace::build(entry_name, text, capabilities, &mut |path| {
            modules.get(path).cloned().ok_or(LoadModuleError::NotFound)
        })
    }

    /// The direct import whose canonical path is `canonical_path`.
    #[must_use]
    pub fn imported(&self, canonical_path: &str) -> Option<&ImportedFile> {
        self.imports
            .iter()
            .find(|m| m.canonical_path == canonical_path)
    }

    /// The text of the module named `module`: the entry module, or one of
    /// its direct imports.
    #[must_use]
    pub fn text_of(&self, module: &str) -> Option<&str> {
        if module == self.entry_name {
            Some(&self.text)
        } else {
            self.imported(module).map(|m| m.text.as_str())
        }
    }

    /// The tree of the module named `module`: the entry module, or one of
    /// its direct imports.
    #[must_use]
    pub fn file_of(&self, module: &str) -> Option<&ast::File> {
        if module == self.entry_name {
            Some(&self.file)
        } else {
            self.imported(module).map(|m| &m.file)
        }
    }

    /// The binders of the entry module's own document, in declaration order,
    /// redirecting an `import`'s declaration to the file it was resolved
    /// from.
    #[must_use]
    pub fn binders(&self) -> &[Binder] {
        &self.binders
    }

    /// The type of the smallest checked expression whose span holds
    /// `offset`, as the checker renders it. `None` where the offset is not
    /// inside an expression, which is what a client expects rather than an
    /// empty hover, and where the checker could not work the type out.
    #[must_use]
    pub fn hover(&self, offset: u32) -> Option<Hover> {
        let (span, ty) = self.types.at(offset)?;
        let ty = crate::types::display(&self.hir, &self.names, ty)?;
        Some(Hover { span, ty })
    }

    /// Where the name at `offset` was declared: in this module or, for a
    /// name brought in by `import`, in the module it was imported from.
    /// `None` where the offset is not on a name that resolves to a
    /// declaration (a keyword, a prelude name, a field or method name after
    /// a `.`, or text inside a comment or a string).
    #[must_use]
    pub fn definition(&self, offset: u32) -> Option<DeclId> {
        if self.place_at(offset) != Place::Code {
            return None;
        }
        let (word, _) = word_at(&self.text, offset as usize)?;
        definition_of(&self.binders, &word, offset).map(|binder| binder.decl.clone())
    }

    /// What `offset` is in, lexically: ordinary code, a comment, the text of
    /// a string or a filter literal, or the position after a `.` where a
    /// field or method name goes.
    pub(crate) fn place_at(&self, offset: u32) -> Place {
        let in_comment = self
            .file
            .comments
            .iter()
            .any(|c| offset > c.span.start && offset <= c.span.end);
        if in_comment {
            return Place::Comment;
        }
        for token in &self.tokens {
            if token.span.start >= offset {
                break;
            }
            let end = token.span.end;
            match token.kind {
                // The text of the literal is between its delimiters. A token
                // that was malformed, such as a string with no closing
                // quote, runs to the end of its line and is still being
                // typed at its end.
                TokenKind::String | TokenKind::StringTail | TokenKind::Filter => {
                    let last = if token.malformed {
                        end
                    } else {
                        end.saturating_sub(1)
                    };
                    if offset <= last {
                        return if token.kind == TokenKind::Filter {
                            Place::FilterText
                        } else {
                            Place::StringText
                        };
                    }
                }
                // Up to the `${` that ends the piece, whose two bytes begin
                // the interpolation, not the text.
                TokenKind::StringHead | TokenKind::StringMiddle
                    if offset <= end.saturating_sub(2) =>
                {
                    return Place::StringText;
                }
                _ => {}
            }
        }
        if self.follows_dot(offset) {
            return Place::Member;
        }
        Place::Code
    }

    /// Whether the name being written at `offset` comes right after a `.`:
    /// the start of the word the cursor is on (or at, if there is none) is
    /// preceded by a dot token.
    fn follows_dot(&self, offset: u32) -> bool {
        let start = word_at(&self.text, offset as usize).map_or(offset, |(_, span)| span.start);
        self.tokens
            .iter()
            .rev()
            .find(|t| t.span.end <= start && t.kind != TokenKind::Term)
            .is_some_and(|t| t.kind == TokenKind::Dot)
    }
}

/// What the cursor is in, lexically.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Ordinary code, where a name or a keyword may be written.
    Code,
    /// A `//` comment.
    Comment,
    /// The text of a string literal, outside any interpolation.
    StringText,
    /// A filter literal.
    FilterText,
    /// After a `.`, where a field or a method name goes.
    Member,
}

/// The directory segments of a canonical module path: everything before the
/// last `/`.
fn dir_of(canonical_path: &str) -> Vec<String> {
    let mut segments: Vec<String> = canonical_path.split('/').map(str::to_owned).collect();
    segments.pop();
    segments
}

/// An exported declaration: where its name is, what it declares, and its
/// signature or annotated type when the source gives one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// The span of the declared name.
    pub span: Span,
    /// What the declaration declares.
    pub kind: Kind,
    /// A function's signature or a type annotation as a program writes it,
    /// or empty.
    pub detail: String,
}

impl Kind {
    /// Whether this is a value with a type to infer: a `constant` or
    /// `variable`.
    pub(crate) const fn is_value(self) -> bool {
        matches!(self, Kind::Constant | Kind::Variable)
    }
}

/// The exported declaration `name` of `file`, if it exports one: only a
/// plain name target (`export constant x = ...`, never a destructuring
/// pattern, since a pattern's own names cannot be individually exported) so
/// this always lands on a real identifier.
#[must_use]
pub fn find_export(file: &ast::File, names: &Interner, name: &str) -> Option<Export> {
    for item in &file.items {
        if item.export.is_none() {
            continue;
        }
        let candidate = match &item.kind {
            ast::ItemKind::Fn(decl) => {
                Some((&decl.name, Kind::Function, fn_signature(decl, names)))
            }
            ast::ItemKind::Record(decl) => Some((&decl.name, Kind::Record, String::new())),
            ast::ItemKind::Enum(decl) => Some((&decl.name, Kind::Enum, String::new())),
            ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
                let kind = if matches!(item.kind, ast::ItemKind::Constant(_)) {
                    Kind::Constant
                } else {
                    Kind::Variable
                };
                match &decl.target {
                    ast::BindingTarget::Name(ident) => Some((
                        ident,
                        kind,
                        decl.ty
                            .as_ref()
                            .map_or_else(String::new, |ty| render_type(ty, names)),
                    )),
                    ast::BindingTarget::Pattern(_) => None,
                }
            }
            ast::ItemKind::Stmt(_) => None,
        };
        if let Some((ident, kind, detail)) = candidate {
            if names.resolve(ident.name) == name {
                return Some(Export {
                    span: ident.span,
                    kind,
                    detail,
                });
            }
        }
    }
    None
}

/// The checker's type of the top-level binding `name` of the module at
/// `canonical_path`, as a program writes it.
fn exported_type(
    program: &hollowscript_check::hir::Program,
    names: &Interner,
    canonical_path: &str,
    name: &str,
) -> String {
    program
        .modules
        .iter()
        .find(|m| m.path == canonical_path)
        .and_then(|module| {
            module
                .globals
                .iter()
                .find(|g| names.resolve(g.name) == name)
        })
        .and_then(|global| crate::types::display(program, names, global.ty))
        .unwrap_or_default()
}

/// The text of an import path literal, if it has no interpolation (the same
/// restriction HS0602 enforces): a plain path string is all this crate
/// resolves further.
#[must_use]
pub fn import_path_text(path: &ast::StringLiteral) -> Option<String> {
    if path.malformed {
        return None;
    }
    let mut out = String::new();
    for part in &path.parts {
        match part {
            ast::StringPart::Text(t) => out.push_str(t),
            ast::StringPart::Interpolation(_) => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{DeclId, Workspace};
    use hollowscript_check::CapabilityInterface;
    use hollowscript_core::LoadModuleError;
    use std::collections::BTreeMap;

    fn modules(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect()
    }

    fn build(text: &str, map: &BTreeMap<String, String>) -> Workspace {
        Workspace::from_modules("main.hws", text, map, &CapabilityInterface::default()).unwrap()
    }

    #[test]
    fn an_import_is_loaded_from_the_map_and_parsed() {
        let map = modules(&[("util.hws", "export constant two = 2\n")]);
        let workspace = build("import two from \"./util\"\nreturn two\n", &map);
        assert!(
            workspace.diagnostics.is_empty(),
            "{:?}",
            workspace.diagnostics
        );
        assert_eq!(workspace.imports.len(), 1);
        assert_eq!(workspace.imports[0].canonical_path, "util.hws");
        assert_eq!(workspace.imports[0].text, "export constant two = 2\n");
        let target = workspace.import_targets.get("two").unwrap();
        assert_eq!(target.decl.module, "util.hws");
        assert_eq!(target.kind, crate::symbols::Kind::Constant);
        assert_eq!(target.detail, "Int");
        assert_eq!(
            workspace.text_of("util.hws"),
            Some("export constant two = 2\n")
        );
        assert!(workspace.file_of("util.hws").is_some());
    }

    #[test]
    fn a_missing_import_is_a_real_diagnostic_not_a_panic() {
        let workspace = build("import two from \"./nope\"\nreturn 1\n", &BTreeMap::new());
        assert!(
            workspace
                .diagnostics
                .iter()
                .any(|d| d.code.to_text() == "HS0601"),
            "{:?}",
            workspace.diagnostics
        );
        assert!(workspace.imports.is_empty());
        assert!(workspace.import_targets.is_empty());
    }

    #[test]
    fn a_path_that_leaves_the_root_is_not_found() {
        let workspace = build("import x from \"../secret\"\nreturn 1\n", &BTreeMap::new());
        assert!(
            workspace
                .diagnostics
                .iter()
                .any(|d| d.code.to_text() == "HS0605"),
            "{:?}",
            workspace.diagnostics
        );
    }

    #[test]
    fn the_loader_is_asked_for_each_path_once() {
        let mut asked: Vec<String> = Vec::new();
        let workspace = Workspace::build(
            "main.hws",
            "import two from \"./util\"\nimport three from \"./util\"\nreturn two\n",
            &CapabilityInterface::default(),
            &mut |path| {
                asked.push(path.to_owned());
                Ok("export constant two = 2\nexport constant three = 3\n".to_owned())
            },
        )
        .unwrap();
        assert_eq!(asked, vec!["util.hws".to_owned()]);
        assert_eq!(workspace.imports.len(), 1);
        assert_eq!(workspace.import_targets.len(), 2);
    }

    #[test]
    fn a_failing_loader_is_a_diagnostic() {
        let workspace = Workspace::build(
            "main.hws",
            "import two from \"./util\"\nreturn 1\n",
            &CapabilityInterface::default(),
            &mut |_| Err(LoadModuleError::Failed("the disk is full".to_owned())),
        )
        .unwrap();
        assert!(
            workspace
                .diagnostics
                .iter()
                .any(|d| d.code.to_text() == "HS0607"),
            "{:?}",
            workspace.diagnostics
        );
        assert!(workspace.imports.is_empty());
    }

    #[test]
    fn imports_resolve_relative_to_a_nested_entry_name() {
        let map = modules(&[("src/util.hws", "export constant two = 2\n")]);
        let workspace = Workspace::from_modules(
            "src/main.hws",
            "import two from \"./util\"\nreturn two\n",
            &map,
            &CapabilityInterface::default(),
        )
        .unwrap();
        assert!(
            workspace.diagnostics.is_empty(),
            "{:?}",
            workspace.diagnostics
        );
        assert_eq!(workspace.imports[0].canonical_path, "src/util.hws");
        assert_eq!(
            workspace
                .import_targets
                .get("two")
                .map(|t| t.decl.module.as_str()),
            Some("src/util.hws")
        );
    }

    #[test]
    fn a_source_over_the_size_limit_builds_nothing() {
        let big = "a".repeat(hollowscript_core::source::MAX_SOURCE_BYTES + 1);
        assert!(
            Workspace::from_modules(
                "main.hws",
                &big,
                &BTreeMap::new(),
                &CapabilityInterface::default()
            )
            .is_none()
        );
    }

    #[test]
    fn binders_redirect_an_import_to_the_declaring_module() {
        let map = modules(&[("util.hws", "export constant two = 2\n")]);
        let workspace = build("import two from \"./util\"\nreturn two\n", &map);
        let binders = workspace.binders();
        let import = binders.iter().find(|b| b.name == "two").unwrap();
        assert_eq!(
            import.decl,
            DeclId {
                module: "util.hws".to_owned(),
                span: workspace.import_targets["two"].decl.span
            }
        );
    }

    // ---- hover, definition and what the cursor is in ----

    use crate::testing::{offset, workspace};
    use crate::workspace::Place;

    #[test]
    fn hover_is_the_type_of_the_smallest_expression_under_the_offset() {
        let text = "constant total = 1 + 2.5\nreturn total\n";
        let w = workspace(text);
        let hover = |needle: &str| w.hover(offset(text, needle));
        // At the `+`, only the whole sum holds the offset.
        let sum = hover("+ 2.5").unwrap();
        assert_eq!(sum.ty, "Float");
        assert_eq!(
            &text[sum.span.start as usize..sum.span.end as usize],
            "1 + 2.5"
        );
        // At the start of the sum, the `1` is the smaller expression, and
        // carries the type the program converts it to.
        let one = hover("1 + 2.5").unwrap();
        assert_eq!(&text[one.span.start as usize..one.span.end as usize], "1");
        assert_eq!(one.ty, "Float");
        let literal = hover("2.5").unwrap();
        assert_eq!(literal.ty, "Float");
        assert_eq!(
            &text[literal.span.start as usize..literal.span.end as usize],
            "2.5"
        );
        assert_eq!(hover("total\n").unwrap().ty, "Float");
        // Not inside an expression: no hover rather than an empty one.
        assert_eq!(hover("constant"), None);
    }

    #[test]
    fn hover_sees_inside_not_and_inside_a_function_body() {
        let text = concat!(
            "fn check(n: Int): Bool {\n",
            "    return not (n == 1)\n",
            "}\n",
        );
        let w = workspace(text);
        assert_eq!(w.hover(offset(text, "n == 1")).unwrap().ty, "Int");
        assert_eq!(w.hover(offset(text, "(n == 1)")).unwrap().ty, "Bool");
    }

    #[test]
    fn hover_in_the_entry_module_ignores_the_functions_of_an_import() {
        // Node ids restart at zero in every file, so the body of an imported
        // function must not be matched against the entry module's spans.
        let map = modules(&[(
            "util.hws",
            concat!(
                "export fn shout(): String {\n",
                "    constant a = \"a\"\n",
                "    constant b = a + a\n",
                "    return b + a\n",
                "}\n",
            ),
        )]);
        let text = concat!(
            "import shout from \"./util\"\n",
            "constant n = 12345\n",
            "constant m = n + 1\n",
            "constant s = shout()\n",
        );
        let w = build(text, &map);
        assert_eq!(w.hover(offset(text, "12345")).unwrap().ty, "Int");
        assert_eq!(w.hover(offset(text, "n + 1")).unwrap().ty, "Int");
        assert_eq!(w.hover(offset(text, "()")).unwrap().ty, "String");
        assert_eq!(w.hover(offset(text, "shout()")).unwrap().ty, "fn(): String");
    }

    #[test]
    fn definition_finds_a_local_and_an_import() {
        let map = modules(&[("util.hws", "export constant two = 2\n")]);
        let text = concat!(
            "import two from \"./util\"\n",
            "constant n = 1\n",
            "fn f(n2: Int): Int {\n",
            "    return n2 + two\n",
            "}\n",
            "return n\n",
        );
        let w = build(text, &map);
        let local = w.definition(offset(text, "n2 + two")).unwrap();
        assert_eq!(local.module, "main.hws");
        assert_eq!(
            &text[local.span.start as usize..local.span.end as usize],
            "n2"
        );
        let imported = w.definition(offset(text, "two\n")).unwrap();
        assert_eq!(imported.module, "util.hws");
        let util = w.text_of("util.hws").unwrap();
        assert_eq!(
            &util[imported.span.start as usize..imported.span.end as usize],
            "two"
        );
        // The same through the import line itself.
        assert_eq!(w.definition(offset(text, "two from")).unwrap(), imported);
        // At the end of the name, as a caret sits after typing it.
        assert!(w.definition(offset(text, "n2 + two") + 2).is_some());
    }

    #[test]
    fn definition_is_none_where_there_is_nothing_to_go_to() {
        let text = concat!(
            "constant total = 1\n",
            "// total in a comment\n",
            "constant s = \"total in a string\"\n",
            "constant r = { total: 1 }\n",
            "constant f = r.total\n",
            "return sqrt(2.0)\n",
        );
        let w = build(text, &BTreeMap::new());
        assert_eq!(w.definition(offset(text, "total in a comment")), None);
        assert_eq!(w.definition(offset(text, "total in a string")), None);
        // A field name after a dot is not the variable of the same name.
        assert_eq!(w.definition(offset(text, "total\nreturn")), None);
        // A keyword and a prelude name have no declaration.
        assert_eq!(w.definition(offset(text, "return")), None);
        assert_eq!(w.definition(offset(text, "sqrt")), None);
        // A name that is plainly code does resolve.
        assert!(w.definition(offset(text, "total: 1")).is_some());
    }

    #[test]
    fn the_place_of_an_offset_says_what_the_cursor_is_in() {
        let text = concat!(
            "constant s = \"a ${1 + 2} b\" // note\n",
            "constant f = `status:open`\n",
            "constant x = s.length()\n",
        );
        let w = workspace(text);
        let place = |needle: &str, plus: u32| w.place_at(offset(text, needle) + plus);
        assert_eq!(place("a ${", 1), Place::StringText);
        assert_eq!(place("a ${", 2), Place::StringText);
        assert_eq!(place("1 + 2", 0), Place::Code);
        assert_eq!(place("1 + 2", 3), Place::Code);
        assert_eq!(place(" b\"", 1), Place::StringText);
        assert_eq!(place("note", 1), Place::Comment);
        assert_eq!(place("status", 2), Place::FilterText);
        assert_eq!(place("length", 0), Place::Member);
        assert_eq!(place("length", 3), Place::Member);
        assert_eq!(place("constant x", 0), Place::Code);
        // Just after the closing quote is code again.
        assert_eq!(place(" // note", 0), Place::Code);
    }

    #[test]
    fn an_unterminated_string_is_still_being_typed_at_its_end() {
        let text = "constant s = \"unfinished";
        let w = workspace(text);
        assert_eq!(
            w.place_at(u32::try_from(text.len()).unwrap()),
            Place::StringText
        );
    }
}
