//! The checker's types, found by position and by name.
//!
//! The checker gives every expression a type and every node a span, so the
//! type under the cursor is the type of the smallest expression whose span
//! holds it. `TypeIndex` collects those spans and types once, when a
//! [`crate::Workspace`] is built, so a hover is a scan of a list rather than
//! a walk of the program.
//!
//! The same walk also gathers, by name, the type of every binding the
//! checker introduced (a global, a parameter, a local, a loop variable, a
//! pattern binding), which is what fills in the type of a name whose
//! declaration carries no annotation.

use hollowscript_check::hir;
use hollowscript_check::types::TypeId;
use hollowscript_core::{Interner, Span};
use hollowscript_syntax::ast::NodeId;
use std::collections::BTreeMap;

/// The type of the expression at a position: the span of the smallest
/// expression that holds it, and its type as the checker renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    /// The span of the expression.
    pub span: Span,
    /// Its type, as a program would write it.
    pub ty: String,
}

/// Every expression of the entry module with its span and type, in the
/// order the checker's tree is walked.
#[derive(Debug, Default)]
pub(crate) struct TypeIndex {
    entries: Vec<(Span, TypeId)>,
}

/// The text a type that contains an error displays as, which says nothing
/// useful to a reader and so is never offered.
const ERROR_TEXT: &str = "<error>";

/// The type `ty` as a program writes it, or `None` for one the checker could
/// not work out.
pub(crate) fn display(program: &hir::Program, names: &Interner, ty: TypeId) -> Option<String> {
    if program.types.is_error(ty) {
        return None;
    }
    let text = program.types.display(ty, |s| names.resolve(s).to_owned());
    (!text.contains(ERROR_TEXT)).then_some(text)
}

impl TypeIndex {
    /// The index of the entry module of `program`: the last module, which is
    /// the one every other is initialised before. Only the entry module's
    /// own functions are walked, since the nodes of another module are
    /// numbered from the start of its own file and would collide.
    pub(crate) fn build(program: &hir::Program) -> TypeIndex {
        let Some(module) = program.modules.last() else {
            return TypeIndex::default();
        };
        let mut entries = Vec::new();
        let mut consider = |node: NodeId, ty: TypeId| {
            if let Some(span) = module.node_spans.get(&node) {
                entries.push((Span::new(span.start, span.end), ty));
            }
        };
        for global in &module.globals {
            if let Some(init) = &global.init {
                walk_expr(init, &mut consider);
            }
        }
        walk_block(&module.init, &mut consider);
        for id in &module.function_ids {
            if let Some(function) = program.functions.get(id.0 as usize) {
                walk_block(&function.body, &mut consider);
            }
        }
        TypeIndex { entries }
    }

    /// The smallest expression whose span holds `offset`, with the earliest
    /// in the walk winning a tie (an outer coercion wraps the expression it
    /// converts and shares its span, and what the reader wants is the type
    /// the program ends up with).
    pub(crate) fn at(&self, offset: u32) -> Option<(Span, TypeId)> {
        let mut best: Option<(Span, TypeId)> = None;
        for (span, ty) in &self.entries {
            if offset < span.start || offset >= span.end {
                continue;
            }
            if best.is_none_or(|(current, _)| span.len() < current.len()) {
                best = Some((*span, *ty));
            }
        }
        best
    }
}

/// The type of every binding of the entry module, by name: `Some(text)` when
/// every binding of that name has the same type, `None` when the name is
/// bound to different types in different places (a name may be reused in
/// sibling blocks) or to one the checker could not work out.
pub(crate) fn binding_types(
    program: &hir::Program,
    names: &Interner,
) -> BTreeMap<String, Option<String>> {
    let mut found: BTreeMap<String, Option<String>> = BTreeMap::new();
    let Some(module) = program.modules.last() else {
        return found;
    };
    let mut record = |name: hollowscript_core::Symbol, ty: TypeId| {
        let text = display(program, names, ty);
        found
            .entry(names.resolve(name).to_owned())
            .and_modify(|existing| {
                if *existing != text {
                    *existing = None;
                }
            })
            .or_insert(text);
    };
    for global in &module.globals {
        record(global.name, global.ty);
    }
    for id in &module.function_ids {
        let Some(function) = program.functions.get(id.0 as usize) else {
            continue;
        };
        for param in &function.params {
            record(param.name, param.ty);
        }
        bindings_in_block(&function.body, &mut record);
    }
    bindings_in_block(&module.init, &mut record);
    found
}

fn bindings_in_block(
    block: &hir::Block,
    record: &mut impl FnMut(hollowscript_core::Symbol, TypeId),
) {
    for stmt in &block.stmts {
        match &stmt.kind {
            hir::StmtKind::Let { bindings, .. } => {
                for binding in bindings {
                    record(binding.name, binding.ty);
                }
            }
            hir::StmtKind::If {
                then, otherwise, ..
            } => {
                bindings_in_block(then, record);
                if let Some(block) = otherwise {
                    bindings_in_block(block, record);
                }
            }
            hir::StmtKind::Match(m) => {
                for arm in &m.arms {
                    for binding in &arm.bindings {
                        record(binding.name, binding.ty);
                    }
                    bindings_in_block(&arm.body, record);
                }
            }
            hir::StmtKind::For { bindings, body, .. } => {
                for binding in bindings {
                    record(binding.name, binding.ty);
                }
                bindings_in_block(body, record);
            }
            hir::StmtKind::While { body, .. } => bindings_in_block(body, record),
            hir::StmtKind::Assign { .. }
            | hir::StmtKind::AssignUpvalue { .. }
            | hir::StmtKind::AssignGlobal { .. }
            | hir::StmtKind::Break { .. }
            | hir::StmtKind::Continue { .. }
            | hir::StmtKind::Return(_)
            | hir::StmtKind::Call(_) => {}
        }
    }
}

fn walk_block(block: &hir::Block, consider: &mut impl FnMut(NodeId, TypeId)) {
    for stmt in &block.stmts {
        walk_stmt(stmt, consider);
    }
}

fn walk_stmt(stmt: &hir::Stmt, consider: &mut impl FnMut(NodeId, TypeId)) {
    match &stmt.kind {
        hir::StmtKind::Let { init, .. } => walk_expr(init, consider),
        hir::StmtKind::Assign { value, .. }
        | hir::StmtKind::AssignUpvalue { value, .. }
        | hir::StmtKind::AssignGlobal { value, .. }
        | hir::StmtKind::Return(Some(value))
        | hir::StmtKind::Call(value) => walk_expr(value, consider),
        hir::StmtKind::If {
            condition,
            then,
            otherwise,
            ..
        } => {
            walk_expr(condition, consider);
            walk_block(then, consider);
            if let Some(block) = otherwise {
                walk_block(block, consider);
            }
        }
        hir::StmtKind::Match(m) => {
            walk_expr(&m.scrutinee, consider);
            for arm in &m.arms {
                walk_block(&arm.body, consider);
            }
        }
        hir::StmtKind::For { iterable, body, .. } => {
            walk_expr(iterable, consider);
            walk_block(body, consider);
        }
        hir::StmtKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, consider);
            walk_block(body, consider);
        }
        hir::StmtKind::Return(None)
        | hir::StmtKind::Break { .. }
        | hir::StmtKind::Continue { .. } => {}
    }
}

fn walk_expr(expr: &hir::Expr, consider: &mut impl FnMut(NodeId, TypeId)) {
    consider(expr.node, expr.ty);
    let mut each = |e: &hir::Expr| walk_expr(e, consider);
    match &expr.kind {
        hir::ExprKind::Unary { operand, .. }
        | hir::ExprKind::Not(operand)
        | hir::ExprKind::IntToFloat(operand)
        | hir::ExprKind::Try { operand, .. }
        | hir::ExprKind::Field {
            target: operand, ..
        } => each(operand),
        hir::ExprKind::Binary { lhs, rhs, .. } | hir::ExprKind::Logical { lhs, rhs, .. } => {
            each(lhs);
            each(rhs);
        }
        hir::ExprKind::Index { target, index } => {
            each(target);
            each(index);
        }
        hir::ExprKind::List(items)
        | hir::ExprKind::NativeCall { args: items, .. }
        | hir::ExprKind::VariantConstruct { args: items, .. }
        | hir::ExprKind::Interpolate { exprs: items, .. } => {
            for item in items {
                each(item);
            }
        }
        hir::ExprKind::Map(pairs) => {
            for (key, value) in pairs {
                each(key);
                each(value);
            }
        }
        hir::ExprKind::Record(fields) => {
            for (_, value) in fields {
                each(value);
            }
        }
        hir::ExprKind::Call { callee, args } | hir::ExprKind::EffectCall { callee, args } => {
            each(callee);
            for arg in args {
                each(arg);
            }
        }
        _ => {}
    }
}
