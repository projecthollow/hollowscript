//! Where each node of a module came from in its source.
//!
//! The HIR names a node by [`NodeId`] alone, because nothing about checking
//! a program needs its position. Running one does: section 19.3 requires a
//! fault to be located at the line where the failing operation starts, and
//! by then the syntax tree is gone. This walks one file once, after it
//! parses, and records the position of every statement and expression, which
//! the compiler then writes into the module's span table.
//!
//! The walk uses an explicit stack, like every other walk over a tree a
//! program controls the depth of.

use hollowscript_core::{PositionCursor, Source, Span};
use hollowscript_syntax::ast::{self, NodeId};
use std::collections::BTreeMap;

/// Where one node is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NodeSpan {
    /// The byte offset where the node starts.
    pub start: u32,
    /// The byte offset where it ends.
    pub end: u32,
    /// The 1-based line of `start`.
    pub line: u32,
    /// The 1-based column of `start`, in code points.
    pub column: u32,
}

/// The position of every statement and expression of `file`.
///
/// The walk finds nodes in no particular order of offset, so their spans are
/// gathered first and located afterwards in order of where they start, with
/// one [`PositionCursor`]: each column is then counted on from the last one
/// rather than from the start of its line, which keeps a long line (a list
/// literal of 70,000 elements written on one) linear in its length rather
/// than quadratic (section 1.10: every pass takes time near-linear in a
/// width).
#[must_use]
pub fn collect(file: &ast::File, source: &Source) -> BTreeMap<NodeId, NodeSpan> {
    let mut found: Vec<(NodeId, Span)> = Vec::new();
    let mut exprs: Vec<&ast::Expr> = Vec::new();
    let mut stmts: Vec<&ast::Stmt> = Vec::new();
    let mut blocks: Vec<&ast::Block> = Vec::new();

    for item in &file.items {
        match &item.kind {
            ast::ItemKind::Fn(decl) => blocks.push(&decl.body),
            ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
                // The item's own node id, not just its initialiser's: the
                // compiler positions a top-level declaration's whole
                // `Global`/destructuring `Let` at it (`hir::Global::node`,
                // and the `node` a destructuring declaration's own `Let`
                // and `AssignGlobal` statements carry), so a fault raised
                // before any more specific position is recorded, or one a
                // narrower `at` call never reaches (compiling an
                // `ExprKind::Global` read, for instance, never calls `at`
                // at all), still lands on the declaration rather than
                // nowhere.
                found.push((item.id, item.span));
                exprs.push(&decl.init);
            }
            ast::ItemKind::Stmt(stmt) => stmts.push(stmt),
            ast::ItemKind::Record(_) | ast::ItemKind::Enum(_) => {}
        }
    }

    loop {
        if let Some(block) = blocks.pop() {
            stmts.extend(block.stmts.iter());
            continue;
        }
        if let Some(stmt) = stmts.pop() {
            found.push((stmt.id, stmt.span));
            push_stmt(stmt, &mut exprs, &mut stmts, &mut blocks);
            continue;
        }
        let Some(expr) = exprs.pop() else {
            break;
        };
        found.push((expr.id, expr.span));
        push_expr(expr, &mut exprs, &mut blocks);
    }
    found.sort_by_key(|(_, span)| span.start);
    let mut cursor = PositionCursor::new();
    let mut out = BTreeMap::new();
    for (id, span) in found {
        let location = source.locate_with(span, &mut cursor);
        out.insert(
            id,
            NodeSpan {
                start: span.start,
                end: span.end,
                line: location.line,
                column: location.column,
            },
        );
    }
    out
}

fn push_stmt<'a>(
    stmt: &'a ast::Stmt,
    exprs: &mut Vec<&'a ast::Expr>,
    stmts: &mut Vec<&'a ast::Stmt>,
    blocks: &mut Vec<&'a ast::Block>,
) {
    match &stmt.kind {
        ast::StmtKind::Constant(decl) | ast::StmtKind::Variable(decl) => exprs.push(&decl.init),
        ast::StmtKind::If(if_stmt) => {
            exprs.push(&if_stmt.condition);
            blocks.push(&if_stmt.then);
            match if_stmt.otherwise.as_deref() {
                Some(ast::Else::Block(block)) => blocks.push(block),
                Some(ast::Else::If(inner)) => stmts.push(inner),
                None => {}
            }
        }
        ast::StmtKind::Match(m) => {
            exprs.push(&m.scrutinee);
            for arm in &m.arms {
                blocks.push(&arm.body);
            }
            if let Some(block) = &m.else_arm {
                blocks.push(block);
            }
        }
        ast::StmtKind::For { iterable, body, .. } => {
            exprs.push(iterable);
            blocks.push(body);
        }
        ast::StmtKind::While {
            condition, body, ..
        } => {
            exprs.push(condition);
            blocks.push(body);
        }
        ast::StmtKind::Return(Some(value))
        | ast::StmtKind::Call(value)
        | ast::StmtKind::Assign { value, .. } => exprs.push(value),
        ast::StmtKind::Return(None)
        | ast::StmtKind::Break
        | ast::StmtKind::Continue
        | ast::StmtKind::Misplaced
        | ast::StmtKind::Error => {}
    }
}

fn push_expr<'a>(
    expr: &'a ast::Expr,
    exprs: &mut Vec<&'a ast::Expr>,
    blocks: &mut Vec<&'a ast::Block>,
) {
    match &expr.kind {
        ast::ExprKind::List(items) => exprs.extend(items.iter()),
        ast::ExprKind::Map(entries) => {
            for entry in entries {
                exprs.push(&entry.key);
                exprs.push(&entry.value);
            }
        }
        ast::ExprKind::Record(fields) => exprs.extend(fields.iter().map(|f| &f.value)),
        ast::ExprKind::Closure(closure) => blocks.push(&closure.body),
        ast::ExprKind::Paren(inner)
        | ast::ExprKind::Effect(inner)
        | ast::ExprKind::Try(inner)
        | ast::ExprKind::Unary { operand: inner, .. }
        | ast::ExprKind::Field { target: inner, .. } => exprs.push(inner),
        ast::ExprKind::Binary { lhs, rhs, .. } => {
            exprs.push(lhs);
            exprs.push(rhs);
        }
        ast::ExprKind::Call { callee, args } => {
            exprs.push(callee);
            exprs.extend(args.iter());
        }
        ast::ExprKind::Index { target, index } => {
            exprs.push(target);
            exprs.push(index);
        }
        ast::ExprKind::Literal(ast::Literal::String(s)) => {
            for part in &s.parts {
                if let ast::StringPart::Interpolation(inner) = part {
                    exprs.push(inner);
                }
            }
        }
        ast::ExprKind::Literal(_) | ast::ExprKind::Name(_) | ast::ExprKind::Error => {}
    }
}

#[cfg(test)]
mod tests {
    use super::collect;
    use hollowscript_core::{Interner, Source, SourceId};

    fn spans_of(text: &str) -> std::collections::BTreeMap<u32, u32> {
        let source = Source::new(SourceId(0), "main.hws", text).unwrap();
        let mut names = Interner::new();
        let mut control = hollowscript_core::Control::none();
        let parsed = hollowscript_syntax::parse(&source, &mut names, &mut control);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        collect(&parsed.file, &source)
            .into_iter()
            .map(|(id, span)| (id.0, span.line))
            .collect()
    }

    /// Every statement and expression is recorded, at the line it starts on.
    #[test]
    fn a_nodes_line_is_where_it_starts() {
        let lines: Vec<u32> = spans_of(concat!(
            "constant a = 1\n",
            "fn f(n: Int): Int {\n",
            "    return n + 1\n",
            "}\n",
            "for x in [1] {\n",
            "    constant y = x\n",
            "}\n",
        ))
        .into_values()
        .collect();
        // Lines 1, 3, 5 and 6 all carry nodes; lines 2, 4 and 7 carry none
        // of their own.
        for wanted in [1, 3, 5, 6] {
            assert!(lines.contains(&wanted), "no node on line {wanted}");
        }
    }

    /// An interpolation is an expression of its own, so a fault inside one
    /// is located at the interpolation and not at the string.
    #[test]
    fn an_interpolation_is_recorded() {
        let spans = spans_of("uses print\neffect print(\"${1 + 2}\")\n");
        assert!(spans.values().any(|&line| line == 2));
    }

    /// Every node of one long line gets the column it starts at, however the
    /// walk happened to reach it: the elements of a list are popped last
    /// first, and the columns are still those of the source. A line of
    /// 50,000 elements is also what used to take time quadratic in its
    /// length, counting every column from the start of the line.
    #[test]
    fn nodes_on_one_long_line_get_their_own_columns() {
        let n = 50_000;
        let items: Vec<String> = (0..n).map(|i| (i % 10).to_string()).collect();
        let text = format!("constant a = [{}]\n", items.join(", "));
        let source = Source::new(SourceId(0), "main.hws", &*text).unwrap();
        let mut names = Interner::new();
        let mut control = hollowscript_core::Control::none();
        let parsed = hollowscript_syntax::parse(&source, &mut names, &mut control);
        let spans = collect(&parsed.file, &source);
        assert_eq!(
            spans.len(),
            n + 2,
            "the declaration, the list and each element"
        );
        // The text is ASCII, so a column is its byte offset plus one.
        for span in spans.values() {
            assert_eq!((span.line, span.column), (1, span.start + 1));
        }
    }

    /// A deeply nested expression does not exhaust the host's stack.
    #[test]
    fn a_deeply_nested_expression_is_walked_iteratively() {
        let text = format!("constant a = {}1{}\n", "(".repeat(200), ")".repeat(200));
        assert!(!spans_of(&text).is_empty());
    }
}
