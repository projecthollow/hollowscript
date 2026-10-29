//! What a name means at a position, for completion, go to definition,
//! references and rename.
//!
//! These need the same two things: which declarations are visible at an
//! offset in the source, and which declaration the name under the cursor
//! refers to. Both come from one walk of the parsed file that records every
//! binder with the span of the region it is visible in, which is section
//! 4.6's own rule in the form an editor needs it.
//!
//! **Scope.** A name is visible from the end of its declaration to the end
//! of the innermost block that holds it; the parameters of a function or
//! closure are visible in its body; a `for` variable and the bindings of a
//! `match` arm are visible in that loop body or arm; top-level `fn`,
//! `record` and `enum` declarations, imports and capability names are
//! visible throughout their file. A top-level `constant` or `variable` is
//! visible in top-level code only after its declaration, but in every
//! function body of its file wherever it is declared (the "hoisted" names of
//! section 4.6). A [`Binder`] carries all of this: [`Binder::scope`] is the
//! region, [`Binder::visible_from`] the offset it comes into view at, and
//! [`Binder::hoisted`] whether a function body sees it regardless.
//! [`definition_of`] resolves a use by region alone (a program that parses
//! has no shadowing, section 4.6, so that is exact), while
//! [`visible_in_order`] applies the order rule too, which is what a
//! completion list needs: a name declared further down is not yet in scope.

use crate::workspace::{DeclId, ImportTarget};
use hollowscript_core::{Interner, Span};
use hollowscript_syntax::ast;
use std::collections::BTreeMap;

/// What kind of thing a binder declares, which a client turns into an icon
/// beside a completion or a colour for a token.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A `constant`.
    Constant,
    /// A `variable`.
    Variable,
    /// A `fn`.
    Function,
    /// A parameter of a function or a closure.
    Parameter,
    /// A `for` variable or a name a pattern bound.
    Binding,
    /// A `record`.
    Record,
    /// An `enum`.
    Enum,
    /// A capability named by a `uses` line.
    Capability,
}

/// One declared name, with where it is and where it can be seen.
#[derive(Clone, Debug)]
pub struct Binder {
    /// The name as written.
    pub name: String,
    /// The span of the name in its declaration, which is where go to
    /// definition lands when this binder itself is the real declaration
    /// (every binder except one for an `import` name that resolved to
    /// another file, where `decl` points there instead).
    pub at: Span,
    /// The region the name is visible in.
    pub scope: Span,
    /// The offset at which the name comes into view in code that runs as it
    /// is read: the end of its declaration for a `constant` or `variable`,
    /// the start of the body for a parameter, and 0 for a name visible
    /// throughout its file.
    pub visible_from: u32,
    /// Whether every function body of the file sees the name wherever it is
    /// declared, which is true of a top-level `constant` or `variable`
    /// alone.
    pub hoisted: bool,
    /// What the name declares.
    pub kind: Kind,
    /// Where this name is really declared: itself, for everything but an
    /// `import`, whose `decl` is the exporting file's declaration when
    /// [`crate::workspace::Workspace::import_targets`] resolved it.
    pub decl: DeclId,
    /// The name's type as a program writes it, or a signature for a
    /// function, or empty when there is none to show. Set from what the
    /// declaration itself says (an annotation, a parameter list); the
    /// workspace fills the rest in from the checker's inferred types.
    pub detail: String,
}

/// Where a binder can be seen.
#[derive(Copy, Clone)]
struct Seen {
    scope: Span,
    from: u32,
    hoisted: bool,
}

impl Seen {
    /// Throughout the file.
    fn file(whole: Span) -> Seen {
        Seen {
            scope: whole,
            from: 0,
            hoisted: false,
        }
    }

    /// Throughout `scope`, from its start.
    fn within(scope: Span) -> Seen {
        Seen {
            scope,
            from: scope.start,
            hoisted: false,
        }
    }

    /// From the end of the declaration `decl` to the end of `scope`.
    fn after(scope: Span, decl: Span) -> Seen {
        Seen {
            scope,
            from: decl.end,
            hoisted: false,
        }
    }
}

/// What a walk threads through: the names to resolve and the module the
/// binders belong to.
struct Walk<'a> {
    names: &'a Interner,
    module: &'a str,
    out: Vec<Binder>,
}

/// Every binder of one file, in declaration order. `module` is this file's
/// own canonical name, the identity every local binder's [`Binder::decl`]
/// carries; `import_targets` is
/// [`crate::workspace::Workspace::import_targets`], which redirects an
/// `import` binder's `decl` to the name's real declaration in the file it
/// was imported from, and gives it that declaration's kind.
#[must_use]
pub fn binders(
    file: &ast::File,
    names: &Interner,
    whole: Span,
    module: &str,
    import_targets: &BTreeMap<String, ImportTarget>,
) -> Vec<Binder> {
    let mut walk = Walk {
        names,
        module,
        out: Vec::new(),
    };
    for entry in &file.uses {
        for name in &entry.names {
            walk.push(name, Seen::file(whole), Kind::Capability, "capability");
        }
    }
    for import in &file.imports {
        for name in &import.names {
            walk.push(name, Seen::file(whole), Kind::Variable, "");
            if let Some(last) = walk.out.last_mut() {
                if let Some(target) = import_targets.get(&last.name) {
                    last.decl = target.decl.clone();
                    last.kind = target.kind;
                    last.detail.clone_from(&target.detail);
                }
            }
        }
    }
    for item in &file.items {
        match &item.kind {
            ast::ItemKind::Fn(decl) => {
                let signature = fn_signature(decl, names);
                walk.push(&decl.name, Seen::file(whole), Kind::Function, &signature);
                // A parameter is visible in the body and nowhere else.
                for param in &decl.params {
                    let detail = render_type(&param.ty, names);
                    walk.push(
                        &param.name,
                        Seen::within(decl.body.span),
                        Kind::Parameter,
                        &detail,
                    );
                }
                walk.block(&decl.body);
            }
            ast::ItemKind::Record(decl) => {
                walk.push(&decl.name, Seen::file(whole), Kind::Record, "");
            }
            ast::ItemKind::Enum(decl) => {
                walk.push(&decl.name, Seen::file(whole), Kind::Enum, "");
            }
            ast::ItemKind::Constant(decl) => {
                let seen = Seen {
                    hoisted: true,
                    ..Seen::after(whole, item.span)
                };
                walk.target(&decl.target, decl.ty.as_ref(), seen, Kind::Constant);
                walk.expr(&decl.init);
            }
            ast::ItemKind::Variable(decl) => {
                let seen = Seen {
                    hoisted: true,
                    ..Seen::after(whole, item.span)
                };
                walk.target(&decl.target, decl.ty.as_ref(), seen, Kind::Variable);
                walk.expr(&decl.init);
            }
            ast::ItemKind::Stmt(stmt) => walk.stmt(stmt, whole),
        }
    }
    walk.out
}

/// The signature of a function as a program writes its type, with the
/// parameter names kept: `fn(a: Int, b: Int): Int`.
#[must_use]
pub fn fn_signature(decl: &ast::FnDecl, names: &Interner) -> String {
    let params: Vec<String> = decl
        .params
        .iter()
        .map(|p| {
            format!(
                "{}: {}",
                names.resolve(p.name.name),
                render_type(&p.ty, names)
            )
        })
        .collect();
    let ret = decl
        .ret
        .as_ref()
        .map_or_else(String::new, |ty| format!(": {}", render_type(ty, names)));
    format!("fn({}){ret}", params.join(", "))
}

/// The binders visible at `offset` by region alone, innermost last, with no
/// name repeated: an inner binder hides an outer one of the same name for
/// the purposes of a completion list, which should offer each name once.
///
/// This ignores the order of declarations; [`visible_in_order`] applies it.
#[must_use]
pub fn visible_at(binders: &[Binder], offset: u32) -> Vec<&Binder> {
    let mut out: Vec<&Binder> = Vec::new();
    for binder in binders {
        if !contains(binder.scope, offset) {
            continue;
        }
        if let Some(existing) = out.iter().position(|b| b.name == binder.name) {
            out.remove(existing);
        }
        out.push(binder);
    }
    out
}

/// The binders in scope at `offset`, in declaration order, with no name
/// repeated: those whose region holds the offset and that have come into
/// view there. A name declared later in the same block is not yet in scope;
/// a top-level `constant` or `variable` is, inside any function body, wherever
/// it is declared (`in_function_body` says whether `offset` is in one).
#[must_use]
pub fn visible_in_order(binders: &[Binder], offset: u32, in_function_body: bool) -> Vec<&Binder> {
    let mut out: Vec<&Binder> = Vec::new();
    for binder in binders {
        if !contains(binder.scope, offset) {
            continue;
        }
        let in_view = offset >= binder.visible_from || (binder.hoisted && in_function_body);
        if !in_view {
            continue;
        }
        if let Some(existing) = out.iter().position(|b| b.name == binder.name) {
            out.remove(existing);
        }
        out.push(binder);
    }
    out
}

/// The declaration the name at `offset` refers to: the innermost binder of
/// that name whose scope contains the cursor.
#[must_use]
pub fn definition_of<'a>(binders: &'a [Binder], name: &str, offset: u32) -> Option<&'a Binder> {
    binders
        .iter()
        .rev()
        .find(|b| b.name == name && contains(b.scope, offset))
}

/// The identifier the cursor is on or immediately after, which is what an
/// editor means by "the word under the cursor" when the caret sits at the
/// end of a name.
pub fn word_at(text: &str, offset: usize) -> Option<(String, Span)> {
    let bytes = text.as_bytes();
    let is_part = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if offset > bytes.len() {
        return None;
    }
    let mut start = offset.min(bytes.len());
    while start > 0 && is_part(*bytes.get(start - 1)?) {
        start -= 1;
    }
    let mut end = offset.min(bytes.len());
    while end < bytes.len() && is_part(*bytes.get(end)?) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let word = text.get(start..end)?.to_owned();
    if word.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some((
        word,
        Span::new(
            u32::try_from(start).unwrap_or(0),
            u32::try_from(end).unwrap_or(0),
        ),
    ))
}

// ---- the AST walk -------------------------------------------------------

fn contains(span: Span, offset: u32) -> bool {
    offset >= span.start && offset <= span.end
}

impl Walk<'_> {
    fn push(&mut self, ident: &ast::Ident, seen: Seen, kind: Kind, detail: &str) {
        let name = self.names.resolve(ident.name).to_owned();
        if name.is_empty() || name == "_" {
            return;
        }
        self.out.push(Binder {
            name,
            at: ident.span,
            scope: seen.scope,
            visible_from: seen.from,
            hoisted: seen.hoisted,
            kind,
            decl: DeclId {
                module: self.module.to_owned(),
                span: ident.span,
            },
            detail: detail.to_owned(),
        });
    }

    /// The binders of a `constant`, a `variable` or a `for` target: one
    /// name of the given `kind`, or the names of a pattern.
    fn target(
        &mut self,
        target: &ast::BindingTarget,
        annotation: Option<&ast::TypeExpr>,
        seen: Seen,
        kind: Kind,
    ) {
        match target {
            ast::BindingTarget::Name(ident) => {
                let detail = annotation.map_or_else(String::new, |ty| render_type(ty, self.names));
                self.push(ident, seen, kind, &detail);
            }
            ast::BindingTarget::Pattern(pattern) => self.pattern(pattern, seen),
        }
    }

    fn pattern(&mut self, pattern: &ast::Pattern, seen: Seen) {
        match &pattern.kind {
            ast::PatternKind::Binding(ident) => self.push(ident, seen, Kind::Binding, ""),
            ast::PatternKind::Variant { args, .. } => {
                for arg in args {
                    self.pattern(arg, seen);
                }
            }
            ast::PatternKind::Record(fields) => {
                for field in fields {
                    match &field.pattern {
                        Some(inner) => self.pattern(inner, seen),
                        None => self.push(&field.name, seen, Kind::Binding, ""),
                    }
                }
            }
            ast::PatternKind::List(elements) => {
                for element in elements {
                    match element {
                        ast::ListElement::Pattern(inner) => self.pattern(inner, seen),
                        ast::ListElement::Rest {
                            binding: Some(ident),
                            ..
                        } => self.push(ident, seen, Kind::Binding, ""),
                        ast::ListElement::Rest { binding: None, .. } => {}
                    }
                }
            }
            ast::PatternKind::Literal { .. }
            | ast::PatternKind::None
            | ast::PatternKind::Wildcard
            | ast::PatternKind::Error => {}
        }
    }

    fn block(&mut self, block: &ast::Block) {
        for stmt in &block.stmts {
            self.stmt(stmt, block.span);
        }
    }

    fn stmt(&mut self, stmt: &ast::Stmt, scope: Span) {
        match &stmt.kind {
            ast::StmtKind::Constant(decl) => {
                self.target(
                    &decl.target,
                    decl.ty.as_ref(),
                    Seen::after(scope, stmt.span),
                    Kind::Constant,
                );
                self.expr(&decl.init);
            }
            ast::StmtKind::Variable(decl) => {
                self.target(
                    &decl.target,
                    decl.ty.as_ref(),
                    Seen::after(scope, stmt.span),
                    Kind::Variable,
                );
                self.expr(&decl.init);
            }
            ast::StmtKind::If(if_stmt) => {
                self.expr(&if_stmt.condition);
                self.block(&if_stmt.then);
                let mut next = if_stmt.otherwise.as_deref();
                while let Some(else_part) = next {
                    match else_part {
                        ast::Else::Block(block) => {
                            self.block(block);
                            next = None;
                        }
                        // `else if` is another `if` statement, walked whole
                        // so its own `else` chain is reached too.
                        ast::Else::If(inner) => {
                            self.stmt(inner, scope);
                            next = None;
                        }
                    }
                }
            }
            ast::StmtKind::Match(m) => {
                self.expr(&m.scrutinee);
                for arm in &m.arms {
                    for pattern in &arm.patterns {
                        self.pattern(pattern, Seen::within(arm.body.span));
                    }
                    self.block(&arm.body);
                }
                if let Some(block) = &m.else_arm {
                    self.block(block);
                }
            }
            ast::StmtKind::For {
                target,
                iterable,
                body,
            } => {
                self.expr(iterable);
                self.target(target, None, Seen::within(body.span), Kind::Binding);
                self.block(body);
            }
            ast::StmtKind::While { condition, body } => {
                self.expr(condition);
                self.block(body);
            }
            ast::StmtKind::Assign { value, .. }
            | ast::StmtKind::Return(Some(value))
            | ast::StmtKind::Call(value) => self.expr(value),
            ast::StmtKind::Break
            | ast::StmtKind::Continue
            | ast::StmtKind::Return(None)
            | ast::StmtKind::Misplaced
            | ast::StmtKind::Error => {}
        }
    }

    /// Finds the closures inside `expr`: each one's parameters are visible
    /// in its body, and its body is walked like any block.
    fn expr(&mut self, expr: &ast::Expr) {
        if let ast::ExprKind::Closure(closure) = &expr.kind {
            for param in &closure.params {
                let detail = param
                    .ty
                    .as_ref()
                    .map_or_else(String::new, |ty| render_type(ty, self.names));
                self.push(
                    &param.name,
                    Seen::within(closure.body.span),
                    Kind::Parameter,
                    &detail,
                );
            }
            self.block(&closure.body);
            return;
        }
        for child in child_exprs(expr) {
            self.expr(child);
        }
    }
}

/// The expressions directly inside `expr`, other than the body of a closure
/// (a closure has no child expression; its body is a block).
fn child_exprs(expr: &ast::Expr) -> Vec<&ast::Expr> {
    match &expr.kind {
        ast::ExprKind::List(items) => items.iter().collect(),
        ast::ExprKind::Map(entries) => entries
            .iter()
            .flat_map(|entry| [&entry.key, &entry.value])
            .collect(),
        ast::ExprKind::Record(fields) => fields.iter().map(|f| &f.value).collect(),
        ast::ExprKind::Literal(ast::Literal::String(lit)) => lit
            .parts
            .iter()
            .filter_map(|part| match part {
                ast::StringPart::Interpolation(inner) => Some(&**inner),
                ast::StringPart::Text(_) => None,
            })
            .collect(),
        ast::ExprKind::Paren(inner)
        | ast::ExprKind::Unary { operand: inner, .. }
        | ast::ExprKind::Effect(inner)
        | ast::ExprKind::Try(inner)
        | ast::ExprKind::Field { target: inner, .. } => vec![inner],
        ast::ExprKind::Binary { lhs, rhs, .. } => vec![lhs, rhs],
        ast::ExprKind::Call { callee, args } => {
            let mut out: Vec<&ast::Expr> = vec![callee];
            out.extend(args.iter());
            out
        }
        ast::ExprKind::Index { target, index } => vec![target, index],
        ast::ExprKind::Literal(_)
        | ast::ExprKind::Name(_)
        | ast::ExprKind::Closure(_)
        | ast::ExprKind::Error => Vec::new(),
    }
}

// ---- occurrences: every use of a name, for references and rename --------

/// Every use of a name in `file`: an expression `Name`, an assignment
/// target, or a type's `Named` name. Declarations are not included here;
/// [`binders`] already has each declaration's own span as `Binder::at`, and
/// the caller (`crate::refactor`) merges the two.
pub fn occurrences(file: &ast::File) -> Vec<ast::Ident> {
    let mut out = Vec::new();
    for item in &file.items {
        match &item.kind {
            ast::ItemKind::Fn(decl) => {
                for param in &decl.params {
                    walk_type_occurrences(&param.ty, &mut out);
                }
                if let Some(ret) = &decl.ret {
                    walk_type_occurrences(ret, &mut out);
                }
                walk_block_occurrences(&decl.body, &mut out);
            }
            ast::ItemKind::Record(decl) => {
                for field in &decl.fields {
                    walk_type_occurrences(&field.ty, &mut out);
                }
            }
            ast::ItemKind::Enum(decl) => {
                for variant in &decl.variants {
                    for payload in &variant.payload {
                        walk_type_occurrences(payload, &mut out);
                    }
                }
            }
            ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
                if let Some(ty) = &decl.ty {
                    walk_type_occurrences(ty, &mut out);
                }
                walk_expr_occurrences(&decl.init, &mut out);
            }
            ast::ItemKind::Stmt(stmt) => walk_stmt_occurrences(stmt, &mut out),
        }
    }
    out
}

fn walk_block_occurrences(block: &ast::Block, out: &mut Vec<ast::Ident>) {
    for stmt in &block.stmts {
        walk_stmt_occurrences(stmt, out);
    }
}

#[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
fn walk_stmt_occurrences(stmt: &ast::Stmt, out: &mut Vec<ast::Ident>) {
    match &stmt.kind {
        ast::StmtKind::Constant(decl) | ast::StmtKind::Variable(decl) => {
            if let Some(ty) = &decl.ty {
                walk_type_occurrences(ty, out);
            }
            walk_expr_occurrences(&decl.init, out);
        }
        ast::StmtKind::Assign { target, value } => {
            out.push(*target);
            walk_expr_occurrences(value, out);
        }
        ast::StmtKind::If(if_stmt) => {
            walk_expr_occurrences(&if_stmt.condition, out);
            walk_block_occurrences(&if_stmt.then, out);
            let mut next = if_stmt.otherwise.as_deref();
            while let Some(else_part) = next {
                match else_part {
                    ast::Else::Block(block) => {
                        walk_block_occurrences(block, out);
                        next = None;
                    }
                    ast::Else::If(inner) => {
                        walk_stmt_occurrences(inner, out);
                        next = None;
                    }
                }
            }
        }
        ast::StmtKind::Match(m) => {
            walk_expr_occurrences(&m.scrutinee, out);
            for arm in &m.arms {
                walk_block_occurrences(&arm.body, out);
            }
            if let Some(block) = &m.else_arm {
                walk_block_occurrences(block, out);
            }
        }
        ast::StmtKind::For { iterable, body, .. } => {
            walk_expr_occurrences(iterable, out);
            walk_block_occurrences(body, out);
        }
        ast::StmtKind::While { condition, body } => {
            walk_expr_occurrences(condition, out);
            walk_block_occurrences(body, out);
        }
        ast::StmtKind::Return(Some(value)) | ast::StmtKind::Call(value) => {
            walk_expr_occurrences(value, out);
        }
        ast::StmtKind::Return(None)
        | ast::StmtKind::Break
        | ast::StmtKind::Continue
        | ast::StmtKind::Misplaced
        | ast::StmtKind::Error => {}
    }
}

#[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
fn walk_expr_occurrences(expr: &ast::Expr, out: &mut Vec<ast::Ident>) {
    match &expr.kind {
        ast::ExprKind::Name(ident) => out.push(*ident),
        ast::ExprKind::Literal(ast::Literal::String(lit)) => walk_string_occurrences(lit, out),
        ast::ExprKind::List(items) => {
            for item in items {
                walk_expr_occurrences(item, out);
            }
        }
        ast::ExprKind::Map(entries) => {
            for entry in entries {
                walk_expr_occurrences(&entry.key, out);
                walk_expr_occurrences(&entry.value, out);
            }
        }
        ast::ExprKind::Record(fields) => {
            for field in fields {
                walk_expr_occurrences(&field.value, out);
            }
        }
        ast::ExprKind::Closure(closure) => {
            for param in &closure.params {
                if let Some(ty) = &param.ty {
                    walk_type_occurrences(ty, out);
                }
            }
            if let Some(ret) = &closure.ret {
                walk_type_occurrences(ret, out);
            }
            walk_block_occurrences(&closure.body, out);
        }
        ast::ExprKind::Paren(inner)
        | ast::ExprKind::Unary { operand: inner, .. }
        | ast::ExprKind::Effect(inner)
        | ast::ExprKind::Try(inner) => walk_expr_occurrences(inner, out),
        ast::ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr_occurrences(lhs, out);
            walk_expr_occurrences(rhs, out);
        }
        ast::ExprKind::Call { callee, args } => {
            walk_expr_occurrences(callee, out);
            for arg in args {
                walk_expr_occurrences(arg, out);
            }
        }
        ast::ExprKind::Index { target, index } => {
            walk_expr_occurrences(target, out);
            walk_expr_occurrences(index, out);
        }
        // `Field.name` is a field or method name, a different namespace
        // from a declared value or type, so it is not one of this pass's
        // occurrences; the receiver still is.
        ast::ExprKind::Field { target, .. } => walk_expr_occurrences(target, out),
        ast::ExprKind::Literal(_) | ast::ExprKind::Error => {}
    }
}

fn walk_string_occurrences(lit: &ast::StringLiteral, out: &mut Vec<ast::Ident>) {
    for part in &lit.parts {
        if let ast::StringPart::Interpolation(expr) = part {
            walk_expr_occurrences(expr, out);
        }
    }
}

fn walk_type_occurrences(ty: &ast::TypeExpr, out: &mut Vec<ast::Ident>) {
    match &ty.kind {
        ast::TypeKind::Named { name, args } => {
            out.push(*name);
            for arg in args {
                walk_type_occurrences(arg, out);
            }
        }
        ast::TypeKind::List(inner)
        | ast::TypeKind::Optional(inner)
        | ast::TypeKind::Paren(inner) => {
            walk_type_occurrences(inner, out);
        }
        ast::TypeKind::Record(fields) => {
            for field in fields {
                walk_type_occurrences(&field.ty, out);
            }
        }
        ast::TypeKind::Fn { params, ret, .. } => {
            for param in params {
                walk_type_occurrences(param, out);
            }
            if let Some(ret) = ret {
                walk_type_occurrences(ret, out);
            }
        }
        ast::TypeKind::Error => {}
    }
}

/// A type expression as a program writes it: `Map<String, Int>`, `[Int]`,
/// `String?`, `{ x: Int }`, `effect fn(Int): Int`.
#[must_use]
pub fn render_type(ty: &ast::TypeExpr, names: &Interner) -> String {
    match &ty.kind {
        ast::TypeKind::Named { name, args } => {
            let base = names.resolve(name.name).to_owned();
            if args.is_empty() {
                base
            } else {
                let inner: Vec<String> = args.iter().map(|a| render_type(a, names)).collect();
                format!("{base}<{}>", inner.join(", "))
            }
        }
        ast::TypeKind::List(inner) => format!("[{}]", render_type(inner, names)),
        ast::TypeKind::Optional(inner) => format!("{}?", render_type(inner, names)),
        ast::TypeKind::Paren(inner) => format!("({})", render_type(inner, names)),
        ast::TypeKind::Record(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {}",
                        names.resolve(f.name.name),
                        render_type(&f.ty, names)
                    )
                })
                .collect();
            format!("{{ {} }}", inner.join(", "))
        }
        ast::TypeKind::Fn {
            effect,
            params,
            ret,
        } => {
            let inner: Vec<String> = params.iter().map(|p| render_type(p, names)).collect();
            let ret = ret
                .as_ref()
                .map_or_else(String::new, |r| format!(": {}", render_type(r, names)));
            format!(
                "{}fn({}){ret}",
                if *effect { "effect " } else { "" },
                inner.join(", ")
            )
        }
        ast::TypeKind::Error => "?".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Kind, definition_of, fn_signature, occurrences, visible_at, visible_in_order, word_at,
    };
    use crate::testing::{offset, workspace};
    use hollowscript_syntax::ast;

    #[test]
    fn the_word_under_the_cursor_includes_a_caret_at_its_end() {
        let text = "return total + 1";
        assert_eq!(word_at(text, 8).unwrap().0, "total");
        assert_eq!(word_at(text, 12).unwrap().0, "total");
        assert_eq!(word_at(text, 7).unwrap().0, "total");
        assert_eq!(word_at(text, 7).unwrap().1.start, 7);
        assert!(word_at(text, 13).is_none());
        assert!(word_at(text, 500).is_none());
        // A number is not a name.
        assert!(word_at("return 42", 8).is_none());
    }

    #[test]
    fn an_inner_binder_is_found_before_an_outer_one_and_scope_limits_visibility() {
        let text = concat!(
            "constant outer = 1\n",
            "fn f(param: Int): Int {\n",
            "    constant inner = 2\n",
            "    return param\n",
            "}\n",
            "return f(outer)\n",
        );
        let w = workspace(text);
        let binders = w.binders();
        let inside = offset(text, "return param") + 8;
        let names: Vec<&str> = visible_at(binders, inside)
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        for expected in ["outer", "f", "param", "inner"] {
            assert!(names.contains(&expected), "{names:?}");
        }
        let outside = offset(text, "return f") + 7;
        let names: Vec<&str> = visible_at(binders, outside)
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert!(names.contains(&"outer"));
        assert!(!names.contains(&"param"));
        assert!(!names.contains(&"inner"));
        let found = definition_of(binders, "param", inside).unwrap();
        assert_eq!(
            &text[found.at.start as usize..found.at.end as usize],
            "param"
        );
        assert!(definition_of(binders, "param", outside).is_none());
    }

    #[test]
    fn declaration_order_is_applied_by_visible_in_order_and_not_by_visible_at() {
        let text = "constant a = 1\nconstant b = a\nconstant c = 3\n";
        let w = workspace(text);
        let at = offset(text, "a\nconstant c");
        let by_region: Vec<&str> = visible_at(w.binders(), at)
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(by_region, vec!["a", "b", "c"]);
        let in_order: Vec<&str> = visible_in_order(w.binders(), at, false)
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(in_order, vec!["a"]);
        // A function body sees them all, wherever it is.
        let in_body: Vec<&str> = visible_in_order(w.binders(), at, true)
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(in_body, vec!["a", "b", "c"]);
    }

    #[test]
    fn each_declaration_has_the_kind_it_declares() {
        let text = concat!(
            "uses print\n",
            "record R = { x: Int }\n",
            "enum E { A }\n",
            "constant k = 1\n",
            "variable v = 2\n",
            "fn f(p: Int): Int {\n",
            "    for i in [1] {\n",
            "    }\n",
            "    return p\n",
            "}\n",
        );
        let w = workspace(text);
        let kind = |name: &str| {
            w.binders()
                .iter()
                .find(|b| b.name == name)
                .map(|b| b.kind)
                .unwrap()
        };
        assert_eq!(kind("print"), Kind::Capability);
        assert_eq!(kind("R"), Kind::Record);
        assert_eq!(kind("E"), Kind::Enum);
        assert_eq!(kind("k"), Kind::Constant);
        assert_eq!(kind("v"), Kind::Variable);
        assert_eq!(kind("f"), Kind::Function);
        assert_eq!(kind("p"), Kind::Parameter);
        assert_eq!(kind("i"), Kind::Binding);
    }

    #[test]
    fn a_closure_parameter_and_its_locals_are_binders() {
        let text = concat!(
            "constant doubled = [1, 2].map(fn(n: Int): Int {\n",
            "    constant twice = n * 2\n",
            "    return twice\n",
            "})\n",
        );
        let w = workspace(text);
        let n = w.binders().iter().find(|b| b.name == "n").unwrap();
        assert_eq!(n.kind, Kind::Parameter);
        assert_eq!(n.detail, "Int");
        let use_of_n = offset(text, "n * 2");
        assert_eq!(definition_of(w.binders(), "n", use_of_n).unwrap().at, n.at);
        assert!(w.binders().iter().any(|b| b.name == "twice"));
    }

    #[test]
    fn a_function_signature_keeps_the_parameter_names() {
        let w = workspace(
            "fn add(a: Int, b: [String]?): Int { return a }\nfn one(): Int { return 1 }\nfn unit() {\n}\n",
        );
        let sigs: Vec<String> = w
            .file
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ast::ItemKind::Fn(decl) => Some(fn_signature(decl, &w.names)),
                _ => None,
            })
            .collect();
        assert_eq!(
            sigs,
            vec![
                "fn(a: Int, b: [String]?): Int".to_owned(),
                "fn(): Int".to_owned(),
                "fn()".to_owned()
            ]
        );
    }

    #[test]
    fn occurrences_are_uses_and_not_declarations_or_field_names() {
        let w = workspace(concat!(
            "record P = { x: Int }\n",
            "constant p: P = { x: 1 }\n",
            "return p.x + p.x\n",
        ));
        let uses: Vec<&str> = occurrences(&w.file)
            .iter()
            .map(|i| w.names.resolve(i.name))
            .collect();
        // `Int` and `P` in annotations, then the two `p` receivers; the
        // field name `x` is another namespace and is not listed.
        assert_eq!(uses, vec!["Int", "P", "p", "p"]);
    }
}
