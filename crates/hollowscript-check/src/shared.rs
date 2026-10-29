//! Section 4.8's shared-mutable variables: the pre-pass that finds every
//! `variable` assigned inside a `fn` or closure other than the one that
//! declares it.
//!
//! The checker needs the answer before it checks the declaring function's
//! body, because a closure that assigns a variable can be written after the
//! point where that variable would otherwise be narrowed and still run
//! inside it (a closure stored in a `variable` and replaced on a later
//! loop iteration, for instance). So this walks the syntax tree once per
//! module, ahead of checking, and names each shared-mutable variable by the
//! span of the name its declaration binds, which is how the checker
//! identifies a declaration within one file.
//!
//! Only `variable` declarations are tracked. A `constant`, a parameter or a
//! pattern binding that shadows a `variable` is not modelled, so an
//! assignment to such a name from a closure marks the outer `variable`
//! instead. That can only make the answer larger, never smaller, and the
//! assignment it comes from is already HS0206, so a program without errors
//! gets exactly the set section 4.8 defines.

use std::collections::BTreeSet;

use hollowscript_core::{Span, Symbol};
use hollowscript_syntax::ast;

/// The declaration span of every shared-mutable `variable` in `file`.
#[must_use]
pub fn shared_mutable_variables(file: &ast::File) -> BTreeSet<Span> {
    let mut walk = Walk {
        scopes: vec![Vec::new()],
        depth: 0,
        shared: BTreeSet::new(),
    };
    for item in &file.items {
        walk.item(item);
    }
    walk.shared
}

/// A `variable` in scope: its name, its declaration span and the depth of
/// the function that declares it.
struct Variable {
    name: Symbol,
    span: Span,
    depth: u32,
}

struct Walk {
    scopes: Vec<Vec<Variable>>,
    /// How many functions deep the walk is: 0 for the module's own
    /// top-level statements, one more for each `fn` body or closure.
    depth: u32,
    shared: BTreeSet<Span>,
}

impl Walk {
    fn item(&mut self, item: &ast::Item) {
        match &item.kind {
            ast::ItemKind::Fn(decl) => self.function_body(&decl.body),
            ast::ItemKind::Constant(binding) => self.expr(&binding.init),
            ast::ItemKind::Variable(binding) => self.variable(binding),
            ast::ItemKind::Stmt(stmt) => self.stmt(stmt),
            ast::ItemKind::Record(_) | ast::ItemKind::Enum(_) => {}
        }
    }

    fn function_body(&mut self, body: &ast::Block) {
        self.depth += 1;
        self.block(body);
        self.depth -= 1;
    }

    fn block(&mut self, block: &ast::Block) {
        self.scopes.push(Vec::new());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.scopes.pop();
    }

    /// A `variable` declaration: its initialiser is walked first, because
    /// the name it binds is not in scope there.
    fn variable(&mut self, binding: &ast::BindingDecl) {
        self.expr(&binding.init);
        let mut names = Vec::new();
        match &binding.target {
            ast::BindingTarget::Name(ident) => names.push(*ident),
            ast::BindingTarget::Pattern(pattern) => pattern_names(pattern, &mut names),
        }
        let depth = self.depth;
        if let Some(scope) = self.scopes.last_mut() {
            scope.extend(names.into_iter().map(|ident| Variable {
                name: ident.name,
                span: ident.span,
                depth,
            }));
        }
    }

    fn assign(&mut self, target: &ast::Ident) {
        let found = self
            .scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|v| v.name == target.name);
        if let Some(variable) = found {
            if variable.depth < self.depth {
                self.shared.insert(variable.span);
            }
        }
    }

    fn stmt(&mut self, stmt: &ast::Stmt) {
        match &stmt.kind {
            ast::StmtKind::Constant(binding) => self.expr(&binding.init),
            ast::StmtKind::Variable(binding) => self.variable(binding),
            ast::StmtKind::Assign { target, value } => {
                self.expr(value);
                self.assign(target);
            }
            ast::StmtKind::If(if_stmt) => self.if_stmt(if_stmt),
            ast::StmtKind::Match(m) => {
                self.expr(&m.scrutinee);
                for arm in &m.arms {
                    self.block(&arm.body);
                }
                if let Some(else_arm) = &m.else_arm {
                    self.block(else_arm);
                }
            }
            ast::StmtKind::For { iterable, body, .. } => {
                self.expr(iterable);
                self.block(body);
            }
            ast::StmtKind::While { condition, body } => {
                self.expr(condition);
                self.block(body);
            }
            ast::StmtKind::Return(Some(expr)) | ast::StmtKind::Call(expr) => self.expr(expr),
            ast::StmtKind::Return(None)
            | ast::StmtKind::Break
            | ast::StmtKind::Continue
            | ast::StmtKind::Misplaced
            | ast::StmtKind::Error => {}
        }
    }

    fn if_stmt(&mut self, if_stmt: &ast::IfStmt) {
        self.expr(&if_stmt.condition);
        self.block(&if_stmt.then);
        match if_stmt.otherwise.as_deref() {
            Some(ast::Else::If(stmt)) => self.stmt(stmt),
            Some(ast::Else::Block(block)) => self.block(block),
            None => {}
        }
    }

    fn expr(&mut self, expr: &ast::Expr) {
        match &expr.kind {
            ast::ExprKind::Closure(closure) => self.function_body(&closure.body),
            ast::ExprKind::List(items) => {
                for item in items {
                    self.expr(item);
                }
            }
            ast::ExprKind::Map(entries) => {
                for entry in entries {
                    self.expr(&entry.key);
                    self.expr(&entry.value);
                }
            }
            ast::ExprKind::Record(fields) => {
                for field in fields {
                    self.expr(&field.value);
                }
            }
            ast::ExprKind::Paren(inner)
            | ast::ExprKind::Effect(inner)
            | ast::ExprKind::Try(inner)
            | ast::ExprKind::Unary { operand: inner, .. }
            | ast::ExprKind::Field { target: inner, .. } => self.expr(inner),
            ast::ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ast::ExprKind::Call { callee, args } => {
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
            }
            ast::ExprKind::Index { target, index } => {
                self.expr(target);
                self.expr(index);
            }
            ast::ExprKind::Literal(ast::Literal::String(literal)) => {
                for part in &literal.parts {
                    if let ast::StringPart::Interpolation(inner) = part {
                        self.expr(inner);
                    }
                }
            }
            ast::ExprKind::Literal(_) | ast::ExprKind::Name(_) | ast::ExprKind::Error => {}
        }
    }
}

/// The names a declaration pattern binds, in any order.
pub(crate) fn pattern_names(pattern: &ast::Pattern, out: &mut Vec<ast::Ident>) {
    match &pattern.kind {
        ast::PatternKind::Binding(ident) => out.push(*ident),
        ast::PatternKind::Variant { args, .. } => {
            for arg in args {
                pattern_names(arg, out);
            }
        }
        ast::PatternKind::Record(fields) => {
            for field in fields {
                match &field.pattern {
                    Some(inner) => pattern_names(inner, out),
                    None => out.push(field.name),
                }
            }
        }
        ast::PatternKind::List(elements) => {
            for element in elements {
                match element {
                    ast::ListElement::Pattern(inner) => pattern_names(inner, out),
                    ast::ListElement::Rest {
                        binding: Some(ident),
                        ..
                    } => out.push(*ident),
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

#[cfg(test)]
mod tests {
    use super::shared_mutable_variables;
    use hollowscript_core::{Control, Interner, Source, SourceId};

    /// The names of the shared-mutable variables of `source`, in source
    /// order.
    fn shared(source: &str) -> Vec<String> {
        let mut names = Interner::new();
        let src = Source::new(SourceId(0), "main.hws", source).unwrap();
        let mut control = Control::none();
        let parsed = hollowscript_syntax::parse(&src, &mut names, &mut control);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        shared_mutable_variables(&parsed.file)
            .into_iter()
            .map(|span| source[span.start as usize..span.end as usize].to_owned())
            .collect()
    }

    #[test]
    fn a_variable_a_closure_assigns_is_shared() {
        let source = "fn f(): Int {\n    variable n = 0\n    variable m = 0\n    constant g = fn() {\n        n = n + 1\n    }\n    m = 1\n    return n + m\n}\n";
        assert_eq!(shared(source), ["n"]);
    }

    #[test]
    fn a_closure_assigning_its_own_variable_shares_nothing() {
        let source = "constant g = fn(): Int {\n    variable n = 0\n    n = 1\n    return n\n}\n";
        assert!(shared(source).is_empty());
    }

    #[test]
    fn an_inner_variable_of_the_same_name_is_the_one_assigned() {
        let source = "fn f(): Int {\n    variable n = 0\n    constant g = fn() {\n        variable n = 1\n        n = 2\n    }\n    return n\n}\n";
        assert!(shared(source).is_empty());
    }

    #[test]
    fn an_assignment_two_closures_deep_is_found() {
        let source = "fn f() {\n    variable a = 0\n    constant g = fn() {\n        constant h = fn() {\n            a = 1\n        }\n    }\n}\n";
        assert_eq!(shared(source), ["a"]);
    }
}
