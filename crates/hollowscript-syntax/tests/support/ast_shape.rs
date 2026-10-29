//! Strips every span and node id from a syntax tree in place, so that two
//! trees parsed from different (but hopefully equivalent) source texts can
//! be compared for equality with `==` and mean "the same tree, positions
//! aside" rather than "literally the same bytes at the same offsets".
//!
//! Comments are deliberately not part of this: `File::comments` is left
//! exactly as parsed, and the formatter round-trip test compares the two
//! comment lists separately, as a multiset of texts rather than as a tree,
//! since a comment is never something the tree's own shape carries.
//!
//! Every node keeps its other fields (names, operators, literal values,
//! `parens`/`negative`/`export` flags and so on) untouched, so two trees only
//! ever compare equal here when they agree on everything that is not a
//! position. A name compares equal only when both trees were parsed with the
//! same `Interner` (the caller's job, not this module's): a `Symbol` is just
//! an index into whichever interner produced it, and this module has no way
//! to resolve one back to text to compare by spelling instead.

#![allow(dead_code)] // not every helper is used by every caller of this module

use hollowscript_core::Span;
use hollowscript_syntax::ast::{
    BindingDecl, BindingTarget, Block, Closure, ClosureParam, Else, EnumDecl, Expr, ExprKind,
    FieldInit, FieldPattern, FieldType, File, FnDecl, ImportDecl, Item, ItemKind, ListElement,
    Literal, MapEntry, MatchArm, MatchStmt, NodeId, Pattern, PatternKind, RecordDecl, Stmt,
    StmtKind, StringLiteral, StringPart, TypeExpr, TypeKind, UsesLine, VariantDecl,
};

const ZERO_SPAN: Span = Span::new(0, 0);
const ZERO_ID: NodeId = NodeId(0);

/// Strips every span and node id from a whole file's tree, `comments` and
/// `node_count` aside (neither is part of the tree's shape).
pub(crate) fn normalize_file(file: &mut File) {
    file.span = ZERO_SPAN;
    for uses_line in &mut file.uses {
        normalize_uses_line(uses_line);
    }
    for import in &mut file.imports {
        normalize_import(import);
    }
    for item in &mut file.items {
        normalize_item(item);
    }
}

fn normalize_uses_line(uses_line: &mut UsesLine) {
    uses_line.id = ZERO_ID;
    uses_line.span = ZERO_SPAN;
    for name in &mut uses_line.names {
        name.span = ZERO_SPAN;
    }
}

fn normalize_import(import: &mut ImportDecl) {
    import.id = ZERO_ID;
    import.span = ZERO_SPAN;
    for name in &mut import.names {
        name.span = ZERO_SPAN;
    }
    normalize_string_literal(&mut import.path);
}

fn normalize_item(item: &mut Item) {
    item.id = ZERO_ID;
    item.span = ZERO_SPAN;
    if let Some(export_span) = &mut item.export {
        *export_span = ZERO_SPAN;
    }
    match &mut item.kind {
        ItemKind::Fn(decl) => normalize_fn_decl(decl),
        ItemKind::Record(decl) => normalize_record_decl(decl),
        ItemKind::Enum(decl) => normalize_enum_decl(decl),
        ItemKind::Constant(decl) | ItemKind::Variable(decl) => normalize_binding_decl(decl),
        ItemKind::Stmt(stmt) => normalize_stmt(stmt),
    }
}

fn normalize_fn_decl(decl: &mut FnDecl) {
    decl.name.span = ZERO_SPAN;
    for p in &mut decl.type_params {
        p.span = ZERO_SPAN;
    }
    for param in &mut decl.params {
        param.name.span = ZERO_SPAN;
        normalize_type(&mut param.ty);
    }
    if let Some(ret) = &mut decl.ret {
        normalize_type(ret);
    }
    normalize_block(&mut decl.body);
}

fn normalize_record_decl(decl: &mut RecordDecl) {
    decl.name.span = ZERO_SPAN;
    for p in &mut decl.type_params {
        p.span = ZERO_SPAN;
    }
    for field in &mut decl.fields {
        normalize_field_type(field);
    }
    decl.braces = ZERO_SPAN;
}

fn normalize_enum_decl(decl: &mut EnumDecl) {
    decl.name.span = ZERO_SPAN;
    for p in &mut decl.type_params {
        p.span = ZERO_SPAN;
    }
    for variant in &mut decl.variants {
        normalize_variant(variant);
    }
    decl.braces = ZERO_SPAN;
}

fn normalize_variant(variant: &mut VariantDecl) {
    variant.name.span = ZERO_SPAN;
    for ty in &mut variant.payload {
        normalize_type(ty);
    }
}

fn normalize_binding_decl(decl: &mut BindingDecl) {
    normalize_binding_target(&mut decl.target);
    if let Some(ty) = &mut decl.ty {
        normalize_type(ty);
    }
    normalize_expr(&mut decl.init);
}

fn normalize_binding_target(target: &mut BindingTarget) {
    match target {
        BindingTarget::Name(id) => id.span = ZERO_SPAN,
        BindingTarget::Pattern(pattern) => normalize_pattern(pattern),
    }
}

fn normalize_field_type(field: &mut FieldType) {
    field.name.span = ZERO_SPAN;
    normalize_type(&mut field.ty);
}

fn normalize_type(ty: &mut TypeExpr) {
    ty.id = ZERO_ID;
    ty.span = ZERO_SPAN;
    match &mut ty.kind {
        TypeKind::Named { name, args } => {
            name.span = ZERO_SPAN;
            for a in args {
                normalize_type(a);
            }
        }
        TypeKind::List(inner) | TypeKind::Optional(inner) | TypeKind::Paren(inner) => {
            normalize_type(inner);
        }
        TypeKind::Record(fields) => {
            for f in fields {
                normalize_field_type(f);
            }
        }
        TypeKind::Fn { params, ret, .. } => {
            for p in params {
                normalize_type(p);
            }
            if let Some(r) = ret {
                normalize_type(r);
            }
        }
        TypeKind::Error => {}
    }
}

fn normalize_block(block: &mut Block) {
    block.span = ZERO_SPAN;
    for stmt in &mut block.stmts {
        normalize_stmt(stmt);
    }
}

fn normalize_stmt(stmt: &mut Stmt) {
    stmt.id = ZERO_ID;
    stmt.span = ZERO_SPAN;
    match &mut stmt.kind {
        StmtKind::Constant(decl) | StmtKind::Variable(decl) => normalize_binding_decl(decl),
        StmtKind::Assign { target, value } => {
            target.span = ZERO_SPAN;
            normalize_expr(value);
        }
        StmtKind::If(inner) => normalize_if(inner),
        StmtKind::Match(inner) => normalize_match(inner),
        StmtKind::For {
            target,
            iterable,
            body,
        } => {
            normalize_binding_target(target);
            normalize_expr(iterable);
            normalize_block(body);
        }
        StmtKind::While { condition, body } => {
            normalize_expr(condition);
            normalize_block(body);
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Misplaced | StmtKind::Error => {}
        StmtKind::Return(value) => {
            if let Some(v) = value {
                normalize_expr(v);
            }
        }
        StmtKind::Call(inner) => normalize_expr(inner),
    }
}

fn normalize_if(stmt: &mut hollowscript_syntax::ast::IfStmt) {
    normalize_expr(&mut stmt.condition);
    normalize_block(&mut stmt.then);
    if let Some(otherwise) = &mut stmt.otherwise {
        match otherwise.as_mut() {
            Else::If(inner) => normalize_stmt(inner),
            Else::Block(block) => normalize_block(block),
        }
    }
}

fn normalize_match(stmt: &mut MatchStmt) {
    normalize_expr(&mut stmt.scrutinee);
    for arm in &mut stmt.arms {
        normalize_arm(arm);
    }
    if let Some(else_arm) = &mut stmt.else_arm {
        normalize_block(else_arm);
    }
}

fn normalize_arm(arm: &mut MatchArm) {
    for pattern in &mut arm.patterns {
        normalize_pattern(pattern);
    }
    normalize_block(&mut arm.body);
}

fn normalize_pattern(pattern: &mut Pattern) {
    pattern.id = ZERO_ID;
    pattern.span = ZERO_SPAN;
    match &mut pattern.kind {
        PatternKind::Literal { literal, .. } => normalize_literal(literal),
        PatternKind::Binding(id) => id.span = ZERO_SPAN,
        PatternKind::Variant { name, args, .. } => {
            name.span = ZERO_SPAN;
            for a in args {
                normalize_pattern(a);
            }
        }
        PatternKind::Record(fields) => {
            for f in fields {
                normalize_field_pattern(f);
            }
        }
        PatternKind::List(elements) => {
            for e in elements {
                match e {
                    ListElement::Pattern(p) => normalize_pattern(p),
                    ListElement::Rest { span, binding } => {
                        *span = ZERO_SPAN;
                        if let Some(b) = binding {
                            b.span = ZERO_SPAN;
                        }
                    }
                }
            }
        }
        PatternKind::None | PatternKind::Wildcard | PatternKind::Error => {}
    }
}

fn normalize_field_pattern(field: &mut FieldPattern) {
    field.name.span = ZERO_SPAN;
    if let Some(pattern) = &mut field.pattern {
        normalize_pattern(pattern);
    }
}

fn normalize_expr(expr: &mut Expr) {
    expr.id = ZERO_ID;
    expr.span = ZERO_SPAN;
    match &mut expr.kind {
        ExprKind::Literal(lit) => normalize_literal(lit),
        ExprKind::Name(id) => id.span = ZERO_SPAN,
        ExprKind::List(items) => {
            for e in items {
                normalize_expr(e);
            }
        }
        ExprKind::Map(entries) => {
            for e in entries {
                normalize_map_entry(e);
            }
        }
        ExprKind::Record(fields) => {
            for f in fields {
                normalize_field_init(f);
            }
        }
        ExprKind::Closure(closure) => normalize_closure(closure),
        ExprKind::Paren(inner)
        | ExprKind::Effect(inner)
        | ExprKind::Try(inner)
        | ExprKind::Unary { operand: inner, .. } => normalize_expr(inner),
        ExprKind::Binary { lhs, rhs, .. } => {
            normalize_expr(lhs);
            normalize_expr(rhs);
        }
        ExprKind::Call { callee, args } => {
            normalize_expr(callee);
            for a in args {
                normalize_expr(a);
            }
        }
        ExprKind::Index { target, index } => {
            normalize_expr(target);
            normalize_expr(index);
        }
        ExprKind::Field { target, name } => {
            normalize_expr(target);
            name.span = ZERO_SPAN;
        }
        ExprKind::Error => {}
    }
}

fn normalize_map_entry(entry: &mut MapEntry) {
    normalize_expr(&mut entry.key);
    normalize_expr(&mut entry.value);
}

fn normalize_field_init(field: &mut FieldInit) {
    field.name.span = ZERO_SPAN;
    normalize_expr(&mut field.value);
}

fn normalize_closure(closure: &mut Closure) {
    for param in &mut closure.params {
        normalize_closure_param(param);
    }
    if let Some(ret) = &mut closure.ret {
        normalize_type(ret);
    }
    normalize_block(&mut closure.body);
}

fn normalize_closure_param(param: &mut ClosureParam) {
    param.name.span = ZERO_SPAN;
    if let Some(ty) = &mut param.ty {
        normalize_type(ty);
    }
}

fn normalize_literal(literal: &mut Literal) {
    if let Literal::String(sl) = literal {
        normalize_string_literal(sl);
    }
}

fn normalize_string_literal(literal: &mut StringLiteral) {
    literal.span = ZERO_SPAN;
    for part in &mut literal.parts {
        if let StringPart::Interpolation(expr) = part {
            normalize_expr(expr);
        }
    }
}
