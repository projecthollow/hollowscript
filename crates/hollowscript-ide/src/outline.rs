//! An outline and a fold list built straight from the syntax tree, with no
//! need for the checker to parse anything (an outline has to work for a file
//! that does not check yet, which is most of the time someone is looking at
//! one). The checker only fills in the type shown beside a `constant` or
//! `variable` that has no annotation.
//!
//! Both answer in byte spans of the entry module's text; a transport that
//! speaks lines and characters, or UTF-16 code units, converts at its own
//! edge.

use crate::symbols::{Binder, fn_signature, render_type};
use crate::workspace::{Workspace, import_path_text};
use hollowscript_core::{Interner, Span};
use hollowscript_syntax::ast;

/// What an outline item declares.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    /// A capability named by a `uses` line.
    Capability,
    /// An `import` line, standing for the module it names.
    Module,
    /// A `fn`.
    Function,
    /// A `record`.
    Record,
    /// An `enum`.
    Enum,
    /// A `constant`.
    Constant,
    /// A `variable`.
    Variable,
    /// A field of a record.
    Field,
    /// A variant of an enum.
    EnumMember,
}

impl SymbolKind {
    /// The kind as a word, in the spelling a host sees: `capability`,
    /// `module`, `function`, `record`, `enum`, `constant`, `variable`,
    /// `field` or `enumMember`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Capability => "capability",
            SymbolKind::Module => "module",
            SymbolKind::Function => "function",
            SymbolKind::Record => "record",
            SymbolKind::Enum => "enum",
            SymbolKind::Constant => "constant",
            SymbolKind::Variable => "variable",
            SymbolKind::Field => "field",
            SymbolKind::EnumMember => "enumMember",
        }
    }
}

/// One entry of the outline: a name, what it declares, where it is and the
/// entries nested inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    /// The declared name; for an `import`, the path as written.
    pub name: String,
    /// What it declares.
    pub kind: SymbolKind,
    /// What to show beside the name: a function's signature, the type of a
    /// `constant`, `variable` or field, a record's or enum's type
    /// parameters, a variant's payload, or the names an `import` brings in.
    /// Empty when there is nothing to show.
    pub detail: String,
    /// Whether the declaration is marked `export`.
    pub exported: bool,
    /// The span of the whole declaration, `export` included; for a `uses`
    /// name or an `import`, the whole line.
    pub range: Span,
    /// The span of the name inside it.
    pub selection: Span,
    /// A record's fields or an enum's variants, in source order.
    pub children: Vec<OutlineItem>,
}

impl Workspace {
    /// The outline of the entry module, in source order: one item per name a
    /// `uses` line declares, one per `import` line, then one per top-level
    /// `fn`, `record`, `enum`, `constant` or `variable`; a record's fields
    /// and an enum's variants are its children. A destructuring declaration
    /// has no single name to show, and `_` is not a name, so neither is
    /// listed.
    #[must_use]
    pub fn document_symbols(&self) -> Vec<OutlineItem> {
        let mut out = Vec::new();
        for line in &self.file.uses {
            for ident in &line.names {
                out.push(OutlineItem {
                    name: self.names.resolve(ident.name).to_owned(),
                    kind: SymbolKind::Capability,
                    detail: String::new(),
                    exported: false,
                    range: line.span,
                    selection: ident.span,
                    children: Vec::new(),
                });
            }
        }
        for import in &self.file.imports {
            let Some(path) = import_path_text(&import.path) else {
                continue;
            };
            let imported: Vec<&str> = import
                .names
                .iter()
                .map(|ident| self.names.resolve(ident.name))
                .collect();
            out.push(OutlineItem {
                name: path,
                kind: SymbolKind::Module,
                detail: imported.join(", "),
                exported: false,
                range: import.span,
                selection: import.path.span,
                children: Vec::new(),
            });
        }
        out.extend(
            self.file
                .items
                .iter()
                .filter_map(|item| item_symbol(item, &self.names, self.binders())),
        );
        out
    }

    /// The spans a client can fold: every block that spans more than one
    /// line (a `fn` body, an `if`/`for`/`while`/`match` block, a closure's
    /// body, a `record` or `enum`'s braces), in order of where they start.
    #[must_use]
    pub fn folding_spans(&self) -> Vec<Span> {
        let mut spans = Vec::new();
        for item in &self.file.items {
            collect_item(item, &mut spans);
        }
        let text = &self.text;
        spans.retain(|span| line_of(text, span.start) < line_of(text, span.end));
        spans.sort_by_key(|span| span.start);
        spans
    }
}

/// The type parameters of a declaration as a program writes them, `<T, U>`,
/// or empty when it has none.
fn type_params(params: &[ast::Ident], names: &Interner) -> String {
    if params.is_empty() {
        return String::new();
    }
    let written: Vec<&str> = params.iter().map(|p| names.resolve(p.name)).collect();
    format!("<{}>", written.join(", "))
}

fn item_symbol(item: &ast::Item, names: &Interner, binders: &[Binder]) -> Option<OutlineItem> {
    // The detail the workspace already worked out for the declared name,
    // an annotation or else the checker's type.
    let bound = |span: Span| {
        binders
            .iter()
            .find(|b| b.at == span)
            .map_or_else(String::new, |b| b.detail.clone())
    };
    let (name, kind, detail, selection, children) = match &item.kind {
        ast::ItemKind::Fn(decl) => (
            names.resolve(decl.name.name).to_owned(),
            SymbolKind::Function,
            fn_signature(decl, names),
            decl.name.span,
            Vec::new(),
        ),
        ast::ItemKind::Record(decl) => (
            names.resolve(decl.name.name).to_owned(),
            SymbolKind::Record,
            type_params(&decl.type_params, names),
            decl.name.span,
            decl.fields
                .iter()
                .map(|f| {
                    leaf(
                        names.resolve(f.name.name),
                        SymbolKind::Field,
                        render_type(&f.ty, names),
                        f.name.span,
                    )
                })
                .collect(),
        ),
        ast::ItemKind::Enum(decl) => (
            names.resolve(decl.name.name).to_owned(),
            SymbolKind::Enum,
            type_params(&decl.type_params, names),
            decl.name.span,
            decl.variants
                .iter()
                .map(|v| {
                    let payload: Vec<String> =
                        v.payload.iter().map(|ty| render_type(ty, names)).collect();
                    let detail = if payload.is_empty() {
                        String::new()
                    } else {
                        format!("({})", payload.join(", "))
                    };
                    leaf(
                        names.resolve(v.name.name),
                        SymbolKind::EnumMember,
                        detail,
                        v.name.span,
                    )
                })
                .collect(),
        ),
        ast::ItemKind::Constant(decl) => {
            let ident = binding_name(&decl.target)?;
            (
                names.resolve(ident.name).to_owned(),
                SymbolKind::Constant,
                bound(ident.span),
                ident.span,
                Vec::new(),
            )
        }
        ast::ItemKind::Variable(decl) => {
            let ident = binding_name(&decl.target)?;
            (
                names.resolve(ident.name).to_owned(),
                SymbolKind::Variable,
                bound(ident.span),
                ident.span,
                Vec::new(),
            )
        }
        ast::ItemKind::Stmt(_) => return None,
    };
    if name.is_empty() || name == "_" {
        return None;
    }
    Some(OutlineItem {
        name,
        kind,
        detail,
        exported: item.export.is_some(),
        range: item.span,
        selection,
        children,
    })
}

fn leaf(name: &str, kind: SymbolKind, detail: String, span: Span) -> OutlineItem {
    OutlineItem {
        name: name.to_owned(),
        kind,
        detail,
        exported: false,
        range: span,
        selection: span,
        children: Vec::new(),
    }
}

/// One name a binding declares, for an outline item's own span: only a plain
/// name (a destructuring pattern has no single name to show as the symbol,
/// so it is left out of the outline rather than shown oddly).
fn binding_name(target: &ast::BindingTarget) -> Option<&ast::Ident> {
    match target {
        ast::BindingTarget::Name(ident) => Some(ident),
        ast::BindingTarget::Pattern(_) => None,
    }
}

fn line_of(text: &str, offset: u32) -> usize {
    let end = (offset as usize).min(text.len());
    text.get(..end).map_or(0, |s| s.matches('\n').count())
}

fn collect_item(item: &ast::Item, out: &mut Vec<Span>) {
    match &item.kind {
        ast::ItemKind::Fn(decl) => {
            out.push(decl.body.span);
            collect_block(&decl.body, out);
        }
        ast::ItemKind::Record(decl) => out.push(decl.braces),
        ast::ItemKind::Enum(decl) => out.push(decl.braces),
        ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
            collect_expr(&decl.init, out);
        }
        ast::ItemKind::Stmt(stmt) => collect_stmt(stmt, out),
    }
}

fn collect_block(block: &ast::Block, out: &mut Vec<Span>) {
    for stmt in &block.stmts {
        collect_stmt(stmt, out);
    }
}

fn collect_stmt(stmt: &ast::Stmt, out: &mut Vec<Span>) {
    match &stmt.kind {
        ast::StmtKind::Constant(decl) | ast::StmtKind::Variable(decl) => {
            collect_expr(&decl.init, out);
        }
        ast::StmtKind::If(if_stmt) => {
            out.push(if_stmt.then.span);
            collect_block(&if_stmt.then, out);
            let mut next = if_stmt.otherwise.as_deref();
            while let Some(else_part) = next {
                match else_part {
                    ast::Else::Block(block) => {
                        out.push(block.span);
                        collect_block(block, out);
                        next = None;
                    }
                    ast::Else::If(inner) => {
                        collect_stmt(inner, out);
                        next = None;
                    }
                }
            }
        }
        ast::StmtKind::Match(m) => {
            for arm in &m.arms {
                out.push(arm.body.span);
                collect_block(&arm.body, out);
            }
            if let Some(block) = &m.else_arm {
                out.push(block.span);
                collect_block(block, out);
            }
        }
        ast::StmtKind::For { body, .. } | ast::StmtKind::While { body, .. } => {
            out.push(body.span);
            collect_block(body, out);
        }
        ast::StmtKind::Assign { value, .. }
        | ast::StmtKind::Return(Some(value))
        | ast::StmtKind::Call(value) => collect_expr(value, out),
        ast::StmtKind::Return(None)
        | ast::StmtKind::Break
        | ast::StmtKind::Continue
        | ast::StmtKind::Misplaced
        | ast::StmtKind::Error => {}
    }
}

fn collect_expr(expr: &ast::Expr, out: &mut Vec<Span>) {
    match &expr.kind {
        ast::ExprKind::Closure(closure) => {
            out.push(closure.body.span);
            collect_block(&closure.body, out);
        }
        ast::ExprKind::List(items) => {
            for item in items {
                collect_expr(item, out);
            }
        }
        ast::ExprKind::Map(entries) => {
            for entry in entries {
                collect_expr(&entry.key, out);
                collect_expr(&entry.value, out);
            }
        }
        ast::ExprKind::Record(fields) => {
            for field in fields {
                collect_expr(&field.value, out);
            }
        }
        ast::ExprKind::Paren(inner)
        | ast::ExprKind::Unary { operand: inner, .. }
        | ast::ExprKind::Effect(inner)
        | ast::ExprKind::Try(inner) => collect_expr(inner, out),
        ast::ExprKind::Binary { lhs, rhs, .. } => {
            collect_expr(lhs, out);
            collect_expr(rhs, out);
        }
        ast::ExprKind::Call { callee, args } => {
            collect_expr(callee, out);
            for arg in args {
                collect_expr(arg, out);
            }
        }
        ast::ExprKind::Index { target, index } => {
            collect_expr(target, out);
            collect_expr(index, out);
        }
        ast::ExprKind::Field { target, .. } => collect_expr(target, out),
        ast::ExprKind::Literal(_) | ast::ExprKind::Name(_) | ast::ExprKind::Error => {}
    }
}

#[cfg(test)]
mod tests {
    use super::SymbolKind;
    use crate::testing::workspace;

    #[test]
    fn every_top_level_declaration_is_an_item_in_source_order() {
        let w = workspace(concat!(
            "fn greet(name: String): String {\n",
            "    return name\n",
            "}\n",
            "record Point = { x: Int, y: Int }\n",
            "enum Shape { Circle(Float), Empty }\n",
            "constant total = 1\n",
            "variable count = 2\n",
        ));
        let items = w.document_symbols();
        let shape: Vec<(&str, SymbolKind)> =
            items.iter().map(|i| (i.name.as_str(), i.kind)).collect();
        assert_eq!(
            shape,
            vec![
                ("greet", SymbolKind::Function),
                ("Point", SymbolKind::Record),
                ("Shape", SymbolKind::Enum),
                ("total", SymbolKind::Constant),
                ("count", SymbolKind::Variable),
            ]
        );
        let fields: Vec<&str> = items[1].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(fields, vec!["x", "y"]);
        assert!(
            items[1]
                .children
                .iter()
                .all(|c| c.kind == SymbolKind::Field)
        );
        let variants: Vec<&str> = items[2].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(variants, vec!["Circle", "Empty"]);
        assert!(
            items[2]
                .children
                .iter()
                .all(|c| c.kind == SymbolKind::EnumMember)
        );
    }

    #[test]
    fn the_selection_is_the_name_and_the_range_is_the_whole_declaration() {
        let text = "export fn greet(): Int {\n    return 1\n}\n";
        let items = workspace(text).document_symbols();
        let greet = &items[0];
        assert_eq!(
            &text[greet.selection.start as usize..greet.selection.end as usize],
            "greet"
        );
        assert!(text[greet.range.start as usize..].starts_with("export fn greet"));
        assert!(greet.range.end >= greet.selection.end);
    }

    #[test]
    fn a_destructuring_declaration_and_a_statement_are_left_out() {
        let items = workspace(concat!(
            "constant { a, b } = { a: 1, b: 2 }\n",
            "return a + b\n",
        ))
        .document_symbols();
        assert!(items.is_empty(), "{items:?}");
    }

    #[test]
    fn an_outline_exists_for_a_file_that_does_not_check() {
        let items = workspace("constant ok = 1\nfn broken(: Int {\n").document_symbols();
        assert!(items.iter().any(|i| i.name == "ok"), "{items:?}");
    }

    #[test]
    fn only_multi_line_blocks_fold_and_they_come_in_order() {
        let text = concat!(
            "fn one(): Int { return 1 }\n",
            "fn two(n: Int): Int {\n",
            "    if n > 0 {\n",
            "        return 1\n",
            "    }\n",
            "    return 2\n",
            "}\n",
            "record R = {\n",
            "    x: Int\n",
            "}\n",
        );
        let w = workspace(text);
        let lines: Vec<usize> = w
            .folding_spans()
            .iter()
            .map(|s| text[..s.start as usize].matches('\n').count())
            .collect();
        assert_eq!(lines, vec![1, 2, 7]);
    }

    #[test]
    fn a_closure_body_folds() {
        let text = "constant f = fn(n: Int): Int {\n    return n\n}\n";
        assert_eq!(workspace(text).folding_spans().len(), 1);
    }

    #[test]
    fn uses_and_import_lines_are_items_and_come_first() {
        let map: std::collections::BTreeMap<String, String> = [(
            "util.hws".to_owned(),
            "export fn twice(n: Int): Int {\n    return n * 2\n}\nexport constant limit = 10\n"
                .to_owned(),
        )]
        .into_iter()
        .collect();
        let text = concat!(
            "uses print, clock\n",
            "import twice, limit from \"./util\"\n",
            "constant after = 1\n",
        );
        let w = crate::Workspace::from_modules(
            "main.hws",
            text,
            &map,
            &hollowscript_check::CapabilityInterface::default(),
        )
        .unwrap();
        let items = w.document_symbols();
        let shape: Vec<(&str, SymbolKind, &str)> = items
            .iter()
            .map(|i| (i.name.as_str(), i.kind, i.detail.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("print", SymbolKind::Capability, ""),
                ("clock", SymbolKind::Capability, ""),
                ("./util", SymbolKind::Module, "twice, limit"),
                ("after", SymbolKind::Constant, "Int"),
            ]
        );
        // A `uses` name selects the name and ranges over the line; an import
        // selects its path.
        assert_eq!(
            &text[items[1].selection.start as usize..items[1].selection.end as usize],
            "clock"
        );
        assert_eq!(
            &text[items[1].range.start as usize..items[1].range.end as usize],
            "uses print, clock"
        );
        assert_eq!(
            &text[items[2].selection.start as usize..items[2].selection.end as usize],
            "\"./util\""
        );
        assert!(items.iter().all(|i| !i.exported));
    }

    #[test]
    fn exported_and_detail_are_set_from_the_declaration() {
        let w = workspace(concat!(
            "export fn add(a: Int, b: Int): Int {\n    return a + b\n}\n",
            "fn hidden() {\n}\n",
            "export record Pair<T> = { first: T, second: [T] }\n",
            "enum Shape { Circle(Float), Rect(Float, Float), Empty }\n",
            "export constant limit: Float = 1\n",
            "variable count = 0\n",
        ));
        let items = w.document_symbols();
        let detail = |name: &str| items.iter().find(|i| i.name == name).unwrap();
        assert!(detail("add").exported);
        assert_eq!(detail("add").detail, "fn(a: Int, b: Int): Int");
        assert!(!detail("hidden").exported);
        assert_eq!(detail("hidden").detail, "fn()");
        assert!(detail("Pair").exported);
        assert_eq!(detail("Pair").detail, "<T>");
        let fields: Vec<(&str, &str)> = detail("Pair")
            .children
            .iter()
            .map(|c| (c.name.as_str(), c.detail.as_str()))
            .collect();
        assert_eq!(fields, vec![("first", "T"), ("second", "[T]")]);
        let members: Vec<(&str, &str)> = detail("Shape")
            .children
            .iter()
            .map(|c| (c.name.as_str(), c.detail.as_str()))
            .collect();
        assert_eq!(
            members,
            vec![
                ("Circle", "(Float)"),
                ("Rect", "(Float, Float)"),
                ("Empty", "")
            ]
        );
        assert!(!detail("Shape").exported);
        assert_eq!(detail("limit").detail, "Float");
        assert!(detail("limit").exported);
        // No annotation: the type the checker inferred.
        assert_eq!(detail("count").detail, "Int");
    }

    #[test]
    fn the_kind_names_are_the_words_a_host_sees() {
        let names: Vec<&str> = [
            SymbolKind::Capability,
            SymbolKind::Module,
            SymbolKind::Function,
            SymbolKind::Record,
            SymbolKind::Enum,
            SymbolKind::Constant,
            SymbolKind::Variable,
            SymbolKind::Field,
            SymbolKind::EnumMember,
        ]
        .into_iter()
        .map(SymbolKind::as_str)
        .collect();
        assert_eq!(
            names,
            vec![
                "capability",
                "module",
                "function",
                "record",
                "enum",
                "constant",
                "variable",
                "field",
                "enumMember"
            ]
        );
    }
}
