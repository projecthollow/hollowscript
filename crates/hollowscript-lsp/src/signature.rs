//! `textDocument/signatureHelp`: the parameter list of the function being
//! called at the cursor, resolved through an
//! import the same way `textDocument/definition` is.

use crate::json::{Value, n, obj, s};
use crate::server::Server;
use crate::workspace::{DeclId, Workspace};
use hollowscript_ide::symbols::{Kind, render_type};
use hollowscript_syntax::ast;

/// `textDocument/signatureHelp`: `null` unless the cursor is inside a call
/// to a `fn` this server can see the declaration of (a prelude or a
/// capability call has no declaration to read parameters from, so it is
/// left to the client's own knowledge, the same gap `hover` and
/// `completion` already leave for those names).
pub(crate) fn signature_help(server: &Server, params: Option<&Value>) -> Value {
    let Some((workspace, offset)) = server.workspace_position(params) else {
        return Value::Null;
    };
    let Some(call) = find_call(&workspace.file, offset) else {
        return Value::Null;
    };
    let ast::ExprKind::Call { callee, args } = &call.kind else {
        return Value::Null;
    };
    let ast::ExprKind::Name(callee_name) = &callee.kind else {
        return Value::Null;
    };
    let binders = workspace.binders();
    let Some(binder) = binders
        .iter()
        .find(|b| b.kind == Kind::Function && b.name == workspace.names.resolve(callee_name.name))
    else {
        return Value::Null;
    };
    let Some(decl) = find_fn(&workspace, &workspace.decl_id(&binder.decl)) else {
        return Value::Null;
    };
    let active_parameter = args
        .iter()
        .position(|a| offset <= a.span.end)
        .unwrap_or_else(|| decl.params.len().saturating_sub(1));
    let params: Vec<Value> = decl
        .params
        .iter()
        .map(|p| {
            obj([(
                "label",
                s(format!(
                    "{}: {}",
                    workspace.names.resolve(p.name.name),
                    render_type(&p.ty, &workspace.names)
                )),
            )])
        })
        .collect();
    let ret = decl.ret.as_ref().map_or_else(String::new, |ty| {
        format!(": {}", render_type(ty, &workspace.names))
    });
    let label = format!(
        "{}({}){}",
        workspace.names.resolve(callee_name.name),
        decl.params
            .iter()
            .map(|p| format!(
                "{}: {}",
                workspace.names.resolve(p.name.name),
                render_type(&p.ty, &workspace.names)
            ))
            .collect::<Vec<_>>()
            .join(", "),
        ret,
    );
    obj([(
        "signatures",
        Value::Array(vec![obj([
            ("label", s(label)),
            ("parameters", Value::Array(params)),
            (
                "activeParameter",
                n(i64::try_from(active_parameter).unwrap_or(0)),
            ),
        ])]),
    )])
}

/// The `fn` declaration `decl` names, in this workspace's own document or
/// in the imported file it points at.
fn find_fn<'a>(workspace: &'a Workspace, decl: &DeclId) -> Option<&'a ast::FnDecl> {
    let file = if decl.uri == workspace.uri {
        &workspace.file
    } else {
        &workspace.import_at(&decl.uri)?.file
    };
    file.items.iter().find_map(|item| match &item.kind {
        ast::ItemKind::Fn(fn_decl) if fn_decl.name.span == decl.span => Some(fn_decl),
        _ => None,
    })
}

/// The innermost `Call` expression whose span contains `offset`.
fn find_call(file: &ast::File, offset: u32) -> Option<&ast::Expr> {
    let mut calls: Vec<&ast::Expr> = Vec::new();
    for item in &file.items {
        walk_item(item, &mut calls);
    }
    calls
        .into_iter()
        .filter(|expr| offset >= expr.span.start && offset <= expr.span.end)
        .min_by_key(|expr| expr.span.end.saturating_sub(expr.span.start))
}

fn walk_item<'a>(item: &'a ast::Item, calls: &mut Vec<&'a ast::Expr>) {
    match &item.kind {
        ast::ItemKind::Fn(decl) => walk_block(&decl.body, calls),
        ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
            walk_expr(&decl.init, calls);
        }
        ast::ItemKind::Stmt(stmt) => walk_stmt(stmt, calls),
        ast::ItemKind::Record(_) | ast::ItemKind::Enum(_) => {}
    }
}

fn walk_block<'a>(block: &'a ast::Block, calls: &mut Vec<&'a ast::Expr>) {
    for stmt in &block.stmts {
        walk_stmt(stmt, calls);
    }
}

fn walk_stmt<'a>(stmt: &'a ast::Stmt, calls: &mut Vec<&'a ast::Expr>) {
    match &stmt.kind {
        ast::StmtKind::Constant(decl) | ast::StmtKind::Variable(decl) => {
            walk_expr(&decl.init, calls);
        }
        ast::StmtKind::If(if_stmt) => {
            walk_expr(&if_stmt.condition, calls);
            walk_block(&if_stmt.then, calls);
            let mut next = if_stmt.otherwise.as_deref();
            while let Some(else_part) = next {
                match else_part {
                    ast::Else::Block(block) => {
                        walk_block(block, calls);
                        next = None;
                    }
                    ast::Else::If(inner) => {
                        walk_stmt(inner, calls);
                        next = None;
                    }
                }
            }
        }
        ast::StmtKind::Match(m) => {
            walk_expr(&m.scrutinee, calls);
            for arm in &m.arms {
                walk_block(&arm.body, calls);
            }
            if let Some(block) = &m.else_arm {
                walk_block(block, calls);
            }
        }
        ast::StmtKind::For { iterable, body, .. } => {
            walk_expr(iterable, calls);
            walk_block(body, calls);
        }
        ast::StmtKind::While { condition, body } => {
            walk_expr(condition, calls);
            walk_block(body, calls);
        }
        ast::StmtKind::Assign { value, .. }
        | ast::StmtKind::Return(Some(value))
        | ast::StmtKind::Call(value) => walk_expr(value, calls),
        ast::StmtKind::Return(None)
        | ast::StmtKind::Break
        | ast::StmtKind::Continue
        | ast::StmtKind::Misplaced
        | ast::StmtKind::Error => {}
    }
}

fn walk_expr<'a>(expr: &'a ast::Expr, calls: &mut Vec<&'a ast::Expr>) {
    if matches!(expr.kind, ast::ExprKind::Call { .. }) {
        calls.push(expr);
    }
    match &expr.kind {
        ast::ExprKind::List(items) => {
            for item in items {
                walk_expr(item, calls);
            }
        }
        ast::ExprKind::Map(entries) => {
            for entry in entries {
                walk_expr(&entry.key, calls);
                walk_expr(&entry.value, calls);
            }
        }
        ast::ExprKind::Record(fields) => {
            for field in fields {
                walk_expr(&field.value, calls);
            }
        }
        ast::ExprKind::Closure(closure) => walk_block(&closure.body, calls),
        ast::ExprKind::Paren(inner)
        | ast::ExprKind::Unary { operand: inner, .. }
        | ast::ExprKind::Effect(inner)
        | ast::ExprKind::Try(inner) => walk_expr(inner, calls),
        ast::ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, calls);
            walk_expr(rhs, calls);
        }
        ast::ExprKind::Call { callee, args } => {
            walk_expr(callee, calls);
            for arg in args {
                walk_expr(arg, calls);
            }
        }
        ast::ExprKind::Index { target, index } => {
            walk_expr(target, calls);
            walk_expr(index, calls);
        }
        ast::ExprKind::Field { target, .. } => walk_expr(target, calls),
        ast::ExprKind::Literal(_) | ast::ExprKind::Name(_) | ast::ExprKind::Error => {}
    }
}
