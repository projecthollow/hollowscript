//! Section 4.10's early-use rule: a top-level statement that names a
//! top-level `fn` before every top-level `constant`/`variable` that
//! function (transitively) reads has been declared is HS0210.
//!
//! The rule is deliberately syntactic ("the rule looks only at names. It
//! does not follow calls made through values"), so this pass never needs a
//! type: it tracks only which names are shadowed by a local of the same
//! name, exactly the ordinary scoping rule of section 4.6, and matches
//! everything else by text against the file's own top-level names. That is
//! also what keeps it independent of the checker's own phases, which need
//! this answer before any top-level statement or function body is checked
//! (`Checker::check_top_level` reports HS0210 as it goes).

use std::collections::{BTreeMap, BTreeSet};

use hollowscript_core::{Span, Symbol};
use hollowscript_syntax::ast;

/// A fault the checker turns into HS0210: `span` is the first place, in
/// source order among the functions section 4.10's `Refs` reaches, that
/// mentions the binding directly; `related` is the top-level statement that
/// named the function too early.
#[derive(Clone, Copy, Debug)]
pub struct EarlyUseFault {
    /// The top-level `fn` the offending statement names.
    pub function: Symbol,
    /// The offending binding's name.
    pub binding: Symbol,
    /// Where the binding is read.
    pub span: Span,
    /// The top-level statement whose early reference to the function is
    /// what makes the read too early.
    pub related: Span,
}

/// One top-level `fn`'s direct references (section 4.10's `Refs`, before the
/// least-fixed-point closure over calls): every top-level `constant`/
/// `variable` name its body mentions unshadowed, each with the span of its
/// first mention, and every other top-level `fn` name its body mentions,
/// called or not.
#[derive(Default, Clone)]
struct DirectRefs {
    bindings: BTreeMap<Symbol, Span>,
    functions: BTreeSet<Symbol>,
}

/// Every HS0210 fault of `file`, in the order the offending top-level
/// statements appear.
#[must_use]
pub fn early_use_faults(file: &ast::File) -> Vec<EarlyUseFault> {
    let fn_names: BTreeSet<Symbol> = file
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Fn(decl) => Some(decl.name.name),
            _ => None,
        })
        .collect();
    let binding_names: BTreeSet<Symbol> = file
        .items
        .iter()
        .flat_map(|item| match &item.kind {
            ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
                binding_target_names(&decl.target)
            }
            _ => Vec::new(),
        })
        .collect();
    let direct: BTreeMap<Symbol, DirectRefs> = file
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Fn(decl) => Some((
                decl.name.name,
                direct_refs_of_body(&decl.body, &decl.params, &fn_names, &binding_names),
            )),
            _ => None,
        })
        .collect();
    let refs = close_refs(&direct);

    let mut faults = Vec::new();
    let mut declared: BTreeSet<Symbol> = BTreeSet::new();
    for item in &file.items {
        match &item.kind {
            ast::ItemKind::Constant(decl) | ast::ItemKind::Variable(decl) => {
                check_statement(&decl.init, &fn_names, &refs, &declared, &mut faults);
                declared.extend(binding_target_names(&decl.target));
            }
            ast::ItemKind::Stmt(stmt) => {
                check_statement_kind(stmt, &fn_names, &refs, &declared, &mut faults);
            }
            ast::ItemKind::Fn(_) | ast::ItemKind::Record(_) | ast::ItemKind::Enum(_) => {}
        }
    }
    faults
}

/// Whichever top-level `fn` names `expr` mentions (unshadowed, no local
/// scope in force at the top level itself), checked against `declared`.
fn check_statement(
    expr: &ast::Expr,
    fn_names: &BTreeSet<Symbol>,
    refs: &BTreeMap<Symbol, DirectRefs>,
    declared: &BTreeSet<Symbol>,
    faults: &mut Vec<EarlyUseFault>,
) {
    let mut walk = RefWalk {
        scopes: vec![Vec::new()],
        mentions: BTreeMap::new(),
    };
    walk.expr(expr);
    check_mentions(&walk.mentions, expr.span, fn_names, refs, declared, faults);
}

fn check_statement_kind(
    stmt: &ast::Stmt,
    fn_names: &BTreeSet<Symbol>,
    refs: &BTreeMap<Symbol, DirectRefs>,
    declared: &BTreeSet<Symbol>,
    faults: &mut Vec<EarlyUseFault>,
) {
    let mut walk = RefWalk {
        scopes: vec![Vec::new()],
        mentions: BTreeMap::new(),
    };
    walk.stmt_kind(&stmt.kind);
    check_mentions(&walk.mentions, stmt.span, fn_names, refs, declared, faults);
}

fn check_mentions(
    mentions: &BTreeMap<Symbol, Span>,
    related: Span,
    fn_names: &BTreeSet<Symbol>,
    refs: &BTreeMap<Symbol, DirectRefs>,
    declared: &BTreeSet<Symbol>,
    faults: &mut Vec<EarlyUseFault>,
) {
    for name in mentions.keys() {
        if !fn_names.contains(name) {
            continue;
        }
        let Some(direct_refs) = refs.get(name) else {
            continue;
        };
        for (&binding, &span) in &direct_refs.bindings {
            if !declared.contains(&binding) {
                faults.push(EarlyUseFault {
                    function: *name,
                    binding,
                    span,
                    related,
                });
            }
        }
    }
}

/// The direct references of one top-level `fn`'s body: its parameters start
/// the outermost scope, since a name they bind is never a top-level
/// reference inside the body.
fn direct_refs_of_body(
    body: &ast::Block,
    params: &[ast::Param],
    fn_names: &BTreeSet<Symbol>,
    binding_names: &BTreeSet<Symbol>,
) -> DirectRefs {
    let mut walk = RefWalk {
        scopes: vec![params.iter().map(|p| p.name.name).collect()],
        mentions: BTreeMap::new(),
    };
    walk.block(body);
    let mut refs = DirectRefs::default();
    for (name, span) in walk.mentions {
        // A mention is only relevant here if it is one of the two kinds
        // section 4.10 tracks: a top-level `fn` (it feeds the call graph
        // `close_refs` closes over) or a top-level `constant`/`variable`
        // (it is what a caller must have declared already). Anything else
        // a body happens to mention by the same name (an import, a
        // capability, a prelude name) is neither and is dropped here.
        if fn_names.contains(&name) {
            refs.functions.insert(name);
        }
        if binding_names.contains(&name) {
            refs.bindings.insert(name, span);
        }
    }
    refs
}

/// The names a `constant`/`variable` target binds, for the top-level set
/// `direct_refs_of_body` filters mentions against.
fn binding_target_names(target: &ast::BindingTarget) -> Vec<Symbol> {
    let mut names = Vec::new();
    match target {
        ast::BindingTarget::Name(ident) => names.push(ident.name),
        ast::BindingTarget::Pattern(pattern) => {
            let mut idents = Vec::new();
            crate::shared::pattern_names(pattern, &mut idents);
            names.extend(idents.into_iter().map(|i| i.name));
        }
    }
    names
}

/// Section 4.10's least fixed point of `Refs` over the call graph among
/// top-level functions, computed by condensing the graph into its strongly
/// connected components (Tarjan) and propagating along the resulting DAG in
/// reverse topological order once per component, which is what keeps this
/// near-linear in the size of the source rather than the naive fixed-point
/// iteration's possible quadratic blow-up: the specification allows any
/// method that gives the same result and requires this cost bound.
fn close_refs(direct: &BTreeMap<Symbol, DirectRefs>) -> BTreeMap<Symbol, DirectRefs> {
    let names: Vec<Symbol> = direct.keys().copied().collect();
    let index_of: BTreeMap<Symbol, usize> =
        names.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    let edges: Vec<Vec<usize>> = names
        .iter()
        .map(|n| {
            direct
                .get(n)
                .map(|r| {
                    r.functions
                        .iter()
                        .filter_map(|f| index_of.get(f).copied())
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    let components = tarjan_scc(&edges);

    // `components` lists every callee's component before every caller's
    // (Tarjan finishes a component only once everything it can reach has
    // already been finished), so a single forward pass over `components`
    // can close each one by pulling in its callees' already-closed sets,
    // never revisiting a component once its own turn is done: each edge is
    // read once, which is the near-linear cost the specification requires.
    let mut closed: Vec<DirectRefs> = names
        .iter()
        .map(|n| direct.get(n).cloned().unwrap_or_default())
        .collect();
    for component in &components {
        // Every function of a mutually recursive component shares one
        // closed set: the union of what any of them directly reads, plus
        // what every function any of them calls has already closed to.
        let mut shared = DirectRefs::default();
        for &i in component {
            let Some(own) = closed.get(i) else { continue };
            for (&b, &s) in &own.bindings {
                shared.bindings.entry(b).or_insert(s);
            }
            shared.functions.extend(own.functions.iter().copied());
            for &callee in edges.get(i).map_or(&[][..], Vec::as_slice) {
                if component.contains(&callee) {
                    // Already folded in above, from this same component.
                    continue;
                }
                let Some(callee_refs) = closed.get(callee) else {
                    continue;
                };
                for (&b, &s) in &callee_refs.bindings {
                    shared.bindings.entry(b).or_insert(s);
                }
                shared
                    .functions
                    .extend(callee_refs.functions.iter().copied());
            }
        }
        for &i in component {
            if let Some(slot) = closed.get_mut(i) {
                *slot = shared.clone();
            }
        }
    }
    names.into_iter().zip(closed).collect()
}

/// Tarjan's strongly connected components, iterative (an explicit stack, no
/// recursion, matching this crate's rule for a walk a program controls the
/// depth of). Returns the components in reverse topological order: a
/// component that only earlier ones can reach comes first, so folding
/// forward through the result never has to revisit one already folded.
// One call frame of `tarjan_scc`'s recursive algorithm, held explicitly: the
// node and how far its own edge list has been walked.
struct Frame {
    node: usize,
    edge: usize,
}

/// `edges[v]`, or an empty slice for a `v` outside it, which `tarjan_scc`
/// never actually passes (every node it names is one `edges` itself already
/// named, a caller's own index or the far end of one of its edges): the
/// fallback is only there so a defect elsewhere is a wrong answer, not a
/// panic (the workspace's own lint policy: no indexing that can panic).
fn edges_of(edges: &[Vec<usize>], v: usize) -> &[usize] {
    edges.get(v).map_or(&[], Vec::as_slice)
}

fn tarjan_scc(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = edges.len();
    let mut index: Vec<Option<usize>> = vec![None; n];
    let mut low: Vec<usize> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut components: Vec<Vec<usize>> = Vec::new();

    for start in 0..n {
        if index.get(start).copied().flatten().is_some() {
            continue;
        }
        let mut call_stack = vec![Frame {
            node: start,
            edge: 0,
        }];
        if let (Some(i), Some(l)) = (index.get_mut(start), low.get_mut(start)) {
            *i = Some(next_index);
            *l = next_index;
        }
        next_index += 1;
        stack.push(start);
        if let Some(o) = on_stack.get_mut(start) {
            *o = true;
        }

        while let Some(frame) = call_stack.last_mut() {
            let v = frame.node;
            let v_edges = edges_of(edges, v);
            if let Some(&w) = v_edges.get(frame.edge) {
                frame.edge += 1;
                if index.get(w).copied().flatten().is_none() {
                    if let (Some(i), Some(l)) = (index.get_mut(w), low.get_mut(w)) {
                        *i = Some(next_index);
                        *l = next_index;
                    }
                    next_index += 1;
                    stack.push(w);
                    if let Some(o) = on_stack.get_mut(w) {
                        *o = true;
                    }
                    call_stack.push(Frame { node: w, edge: 0 });
                } else if on_stack.get(w).copied().unwrap_or(false) {
                    let w_index = index.get(w).copied().flatten();
                    let w_low = low.get(w).copied().unwrap_or(0);
                    if let (Some(v_low), Some(bound)) =
                        (low.get_mut(v), Some(w_index.unwrap_or(w_low)))
                    {
                        *v_low = (*v_low).min(bound);
                    }
                }
            } else {
                call_stack.pop();
                let v_low = low.get(v).copied().unwrap_or(0);
                if let Some(&Frame { node: parent, .. }) = call_stack.last() {
                    if let Some(parent_low) = low.get_mut(parent) {
                        *parent_low = (*parent_low).min(v_low);
                    }
                }
                let v_index = index.get(v).copied().flatten();
                if v_index.unwrap_or(v_low) == v_low {
                    let mut component = Vec::new();
                    while let Some(w) = stack.pop() {
                        if let Some(o) = on_stack.get_mut(w) {
                            *o = false;
                        }
                        component.push(w);
                        if w == v {
                            break;
                        }
                    }
                    components.push(component);
                }
            }
        }
    }
    components
}

/// Collects every unshadowed name mentioned in a `fn`'s body (or a
/// top-level statement's own expression), tracking local scope the same way
/// section 4.6 does: a parameter, a closure parameter, a `for` binding, a
/// pattern binding, and a local `constant`/`variable` all shadow an outer
/// name of the same text. Only the *first* span of each mentioned name is
/// kept, which is enough: any direct mention proves the reference exists,
/// and section 4.10 only ever needs one place to point the diagnostic at.
struct RefWalk {
    scopes: Vec<Vec<Symbol>>,
    mentions: BTreeMap<Symbol, Span>,
}

impl RefWalk {
    fn is_shadowed(&self, name: Symbol) -> bool {
        self.scopes.iter().any(|scope| scope.contains(&name))
    }

    fn mention(&mut self, ident: &ast::Ident) {
        if self.is_shadowed(ident.name) {
            return;
        }
        self.mentions.entry(ident.name).or_insert(ident.span);
    }

    fn block(&mut self, block: &ast::Block) {
        self.scopes.push(Vec::new());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.scopes.pop();
    }

    fn declare_target(&mut self, target: &ast::BindingTarget) {
        let mut names = Vec::new();
        match target {
            ast::BindingTarget::Name(ident) => names.push(*ident),
            ast::BindingTarget::Pattern(pattern) => {
                crate::shared::pattern_names(pattern, &mut names);
            }
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.extend(names.into_iter().map(|i| i.name));
        }
    }

    fn stmt(&mut self, stmt: &ast::Stmt) {
        self.stmt_kind(&stmt.kind);
    }

    fn stmt_kind(&mut self, kind: &ast::StmtKind) {
        match kind {
            ast::StmtKind::Constant(decl) | ast::StmtKind::Variable(decl) => {
                self.expr(&decl.init);
                self.declare_target(&decl.target);
            }
            ast::StmtKind::Assign { target, value } => {
                self.expr(value);
                self.mention(target);
            }
            ast::StmtKind::If(if_stmt) => {
                self.expr(&if_stmt.condition);
                self.block(&if_stmt.then);
                match if_stmt.otherwise.as_deref() {
                    Some(ast::Else::If(inner)) => self.stmt(inner),
                    Some(ast::Else::Block(block)) => self.block(block),
                    None => {}
                }
            }
            ast::StmtKind::Match(m) => {
                self.expr(&m.scrutinee);
                for arm in &m.arms {
                    self.scopes.push(Vec::new());
                    for pattern in &arm.patterns {
                        self.pattern(pattern);
                    }
                    for stmt in &arm.body.stmts {
                        self.stmt(stmt);
                    }
                    self.scopes.pop();
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
                self.scopes.push(Vec::new());
                self.declare_target(target);
                for stmt in &body.stmts {
                    self.stmt(stmt);
                }
                self.scopes.pop();
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

    fn pattern(&mut self, pattern: &ast::Pattern) {
        let mut names = Vec::new();
        crate::shared::pattern_names(pattern, &mut names);
        if let Some(scope) = self.scopes.last_mut() {
            scope.extend(names.into_iter().map(|i| i.name));
        }
    }

    fn expr(&mut self, expr: &ast::Expr) {
        match &expr.kind {
            ast::ExprKind::Name(ident) => self.mention(ident),
            ast::ExprKind::Closure(closure) => {
                self.scopes
                    .push(closure.params.iter().map(|p| p.name.name).collect());
                for stmt in &closure.body.stmts {
                    self.stmt(stmt);
                }
                self.scopes.pop();
            }
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
            ast::ExprKind::Literal(_) | ast::ExprKind::Error => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::early_use_faults;
    use hollowscript_core::{Control, Interner, Source, SourceId};

    /// The names section 4.10's own faults report, in file order.
    fn faults(source: &str) -> Vec<String> {
        let mut names = Interner::new();
        let src = Source::new(SourceId(0), "main.hws", source).unwrap();
        let mut control = Control::none();
        let parsed = hollowscript_syntax::parse(&src, &mut names, &mut control);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        early_use_faults(&parsed.file)
            .into_iter()
            .map(|f| names.resolve(f.binding).to_owned())
            .collect()
    }

    #[test]
    fn a_function_called_after_its_dependency_is_declared_is_fine() {
        let source = "uses print\n\neffect print(\"${double(4)}\")\n\nfn double(n: Int): Int {\n    return n * 2\n}\n";
        assert!(faults(source).is_empty());
    }

    #[test]
    fn reading_a_binding_declared_after_the_call_is_hs0210() {
        let source = "fn read(): Int {\n    return base\n}\n\nconstant early = read()\n\nconstant base = 5\n\nreturn early + base\n";
        assert_eq!(faults(source), ["base"]);
    }

    #[test]
    fn a_binding_declared_before_the_call_is_fine() {
        let source = "constant base = 5\n\nfn read(): Int {\n    return base\n}\n\nconstant early = read()\n\nreturn early + base\n";
        assert!(faults(source).is_empty());
    }

    #[test]
    fn the_dependency_reaches_through_a_called_function() {
        let source = "fn outer(): Int {\n    return inner()\n}\n\nfn inner(): Int {\n    return base\n}\n\nconstant early = outer()\n\nconstant base = 5\n\nreturn early + base\n";
        assert_eq!(faults(source), ["base"]);
    }

    #[test]
    fn mutual_recursion_still_finds_the_dependency() {
        let source = "fn a(n: Int): Int {\n    if n <= 0 {\n        return base\n    }\n    return b(n - 1)\n}\n\nfn b(n: Int): Int {\n    return a(n)\n}\n\nconstant early = a(1)\n\nconstant base = 5\n\nreturn early + base\n";
        assert_eq!(faults(source), ["base"]);
    }

    #[test]
    fn a_local_of_the_same_name_is_not_the_top_level_binding() {
        let source = "fn read(): Int {\n    constant base = 1\n    return base\n}\n\nconstant early = read()\n\nconstant base = 5\n\nreturn early + base\n";
        assert!(faults(source).is_empty());
    }

    #[test]
    fn a_binding_declared_by_the_same_statement_does_not_count() {
        let source =
            "fn read(): Int {\n    return base\n}\n\nconstant base = read()\n\nreturn base\n";
        assert_eq!(faults(source), ["base"]);
    }
}
