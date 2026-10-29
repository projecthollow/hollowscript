//! Optional narrowing (section 4.8): the truth-set analysis over
//! [`crate::hir::Path`]s and [`crate::hir::Fact`]s.
//!
//! Facts are a set with no duplicates and no meaningful order, so they are
//! kept sorted and deduplicated, which also makes the "intersection of the
//! facts at the end of every branch" rule after `if` and `match`
//! (sections 4.8 and 7.6) a plain sorted-set intersection.

use crate::hir::{Fact, Path, PathRoot};

/// A set of facts, sorted and free of duplicates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FactSet {
    facts: Vec<Fact>,
}

impl FactSet {
    /// The empty set: what a top-level function body starts with, and a
    /// closure boundary reduces to when nothing it captures survives
    /// (section 4.8).
    #[must_use]
    pub fn empty() -> FactSet {
        FactSet::default()
    }

    /// Whether `path` is known not to be `none`.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        self.facts.iter().any(|f| &f.path == path)
    }

    /// Adds a fact, keeping the set sorted and deduplicated.
    pub fn insert(&mut self, fact: Fact) {
        if let Err(index) = self
            .facts
            .binary_search_by(|f| cmp_path(&f.path, &fact.path))
        {
            self.facts.insert(index, fact);
        }
    }

    /// The union of this set and another, used to build `T(a and b)` and
    /// `F(a or b)` (section 4.8's truth-set table).
    #[must_use]
    pub fn union(&self, other: &FactSet) -> FactSet {
        let mut out = self.clone();
        for f in &other.facts {
            out.insert(f.clone());
        }
        out
    }

    /// The facts common to both sets, used after `if` and `match` to combine
    /// the branches that can complete normally.
    #[must_use]
    pub fn intersect(&self, other: &FactSet) -> FactSet {
        FactSet {
            facts: self
                .facts
                .iter()
                .filter(|f| other.contains(&f.path))
                .cloned()
                .collect(),
        }
    }

    /// The facts of this set whose root is not in `roots`, used to drop the
    /// facts of variables assigned inside a loop or a closure (section 4.8).
    #[must_use]
    pub fn without_roots(&self, roots: &[PathRoot]) -> FactSet {
        FactSet {
            facts: self
                .facts
                .iter()
                .filter(|f| !roots.contains(&f.path.root))
                .cloned()
                .collect(),
        }
    }

    /// The facts whose root is `root`, dropped when `root` is assigned
    /// (`x = e` removes every fact rooted at `x`).
    #[must_use]
    pub fn without_root(&self, root: PathRoot) -> FactSet {
        self.without_roots(&[root])
    }

    /// The facts as a plain list, in a fixed order, for a caller that must
    /// store them (as [`crate::hir::StmtKind::If`] does).
    #[must_use]
    pub fn as_vec(&self) -> Vec<Fact> {
        self.facts.clone()
    }
}

fn cmp_path(a: &Path, b: &Path) -> std::cmp::Ordering {
    (a.root, &a.fields).cmp(&(b.root, &b.fields))
}

/// The two branches of a condition's truth-set (section 4.8's table): the
/// facts true when it is `true`, and the facts true when it is `false`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TruthSets {
    /// The facts that hold when the condition evaluates to `true`.
    pub when_true: FactSet,
    /// The facts that hold when the condition evaluates to `false`.
    pub when_false: FactSet,
}

/// The path a checked expression denotes, if it is one a fact can be about
/// (section 4.8): a local or top-level binding of `module`, or a chain of
/// field accesses rooted at one. [`crate::hir::ExprKind::Field`] names its
/// field by position, not by symbol, so recovering the name needs the record
/// type of `target`, which every [`crate::hir::Expr`] already carries as
/// `ty`; this is why [`truth_sets`] takes a [`crate::types::TypeInterner`]
/// even though it never builds a type itself. `module` is the module
/// currently being checked, needed to tell a root that is one of *its*
/// top-level bindings from one belonging to another module (see
/// [`crate::hir::PathRoot`]).
#[must_use]
pub fn path_of(
    expr: &crate::hir::Expr,
    types: &crate::types::TypeInterner,
    module: u32,
) -> Option<Path> {
    match &expr.kind {
        crate::hir::ExprKind::Local(local) => Some(Path {
            root: PathRoot::Local(*local),
            fields: Vec::new(),
        }),
        // A top-level binding of another module is never a root; see
        // `PathRoot`'s own note. `path_of` is the only place that decides
        // this, so every caller gets the restriction for free.
        crate::hir::ExprKind::Global(global) if global.module == module => Some(Path {
            root: PathRoot::Global(*global),
            fields: Vec::new(),
        }),
        crate::hir::ExprKind::Field { target, index } => {
            let mut path = path_of(target, types, module)?;
            let crate::types::TypeView::Record(fields) = types.view(target.ty) else {
                return None;
            };
            let &(name, _) = fields.get(*index as usize)?;
            path.fields.push(crate::hir::PathField {
                name,
                index: *index,
            });
            Some(path)
        }
        _ => None,
    }
}

/// Whether a checked expression is the `none` literal (section 4.8's table
/// compares a path with `none` on either side of `==`/`!=`).
fn is_none_literal(expr: &crate::hir::Expr) -> bool {
    matches!(
        expr.kind,
        crate::hir::ExprKind::Literal(crate::hir::Constant::None)
    )
}

/// Computes the truth-sets of a checked condition (section 4.8's table):
/// the facts established when it evaluates to `true`, and when it evaluates
/// to `false`. Only the condition's own structure decides this, never facts
/// already in force: a caller that wants the facts true inside a branch
/// unions these with the facts already in force itself, as section 4.8's own
/// wording for `if` and the loops does ("the incoming facts plus `T(c)`"),
/// which is also why this takes no incoming fact set: nothing in the table
/// depends on one.
#[must_use]
pub fn truth_sets(
    condition: &crate::hir::Expr,
    types: &crate::types::TypeInterner,
    module: u32,
) -> TruthSets {
    use crate::hir::{BinaryOp, ExprKind, LogicalOp};
    match &condition.kind {
        ExprKind::Binary {
            op: BinaryOp::Ne { .. },
            lhs,
            rhs,
        } => {
            if let Some(path) = none_comparison_path(lhs, rhs, types, module) {
                let mut when_true = FactSet::empty();
                when_true.insert(Fact { path });
                return TruthSets {
                    when_true,
                    when_false: FactSet::empty(),
                };
            }
            TruthSets::default()
        }
        ExprKind::Binary {
            op: BinaryOp::Eq { .. },
            lhs,
            rhs,
        } => {
            if let Some(path) = none_comparison_path(lhs, rhs, types, module) {
                let mut when_false = FactSet::empty();
                when_false.insert(Fact { path });
                return TruthSets {
                    when_true: FactSet::empty(),
                    when_false,
                };
            }
            TruthSets::default()
        }
        ExprKind::Not(inner) => {
            let inner = truth_sets(inner, types, module);
            TruthSets {
                when_true: inner.when_false,
                when_false: inner.when_true,
            }
        }
        ExprKind::Logical {
            op: LogicalOp::And,
            lhs,
            rhs,
            ..
        } => {
            let a = truth_sets(lhs, types, module);
            let b = truth_sets(rhs, types, module);
            TruthSets {
                when_true: a.when_true.union(&b.when_true),
                when_false: a.when_false.intersect(&a.when_true.union(&b.when_false)),
            }
        }
        ExprKind::Logical {
            op: LogicalOp::Or,
            lhs,
            rhs,
            ..
        } => {
            let a = truth_sets(lhs, types, module);
            let b = truth_sets(rhs, types, module);
            TruthSets {
                when_true: a.when_true.intersect(&a.when_false.union(&b.when_true)),
                when_false: a.when_false.union(&b.when_false),
            }
        }
        _ => TruthSets::default(),
    }
}

/// If exactly one of `lhs`/`rhs` is the `none` literal and the other is a
/// narrowable path, that path (section 4.8: `p != none`, `none != p`,
/// `p == none` and `none == p` are all recognised, the literal on either side).
fn none_comparison_path(
    lhs: &crate::hir::Expr,
    rhs: &crate::hir::Expr,
    types: &crate::types::TypeInterner,
    module: u32,
) -> Option<Path> {
    match (is_none_literal(lhs), is_none_literal(rhs)) {
        (true, false) => path_of(rhs, types, module),
        (false, true) => path_of(lhs, types, module),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::FactSet;
    use crate::hir::{Fact, LocalId, Path, PathRoot};

    fn path(root: u32) -> Path {
        Path {
            root: PathRoot::Local(LocalId(root)),
            fields: vec![],
        }
    }

    #[test]
    fn insert_is_idempotent_and_sorted() {
        let mut s = FactSet::empty();
        s.insert(Fact { path: path(2) });
        s.insert(Fact { path: path(1) });
        s.insert(Fact { path: path(2) });
        assert_eq!(s.as_vec().len(), 2);
        assert!(s.contains(&path(1)) && s.contains(&path(2)));
        assert!(!s.contains(&path(3)));
    }

    #[test]
    fn union_and_intersect_follow_set_semantics() {
        let mut a = FactSet::empty();
        a.insert(Fact { path: path(1) });
        a.insert(Fact { path: path(2) });
        let mut b = FactSet::empty();
        b.insert(Fact { path: path(2) });
        b.insert(Fact { path: path(3) });
        let u = a.union(&b);
        assert!(u.contains(&path(1)) && u.contains(&path(2)) && u.contains(&path(3)));
        let i = a.intersect(&b);
        assert!(i.contains(&path(2)) && !i.contains(&path(1)) && !i.contains(&path(3)));
    }

    #[test]
    fn dropping_by_root_removes_every_fact_of_that_root() {
        let mut a = FactSet::empty();
        a.insert(Fact { path: path(1) });
        a.insert(Fact { path: path(2) });
        let dropped = a.without_root(PathRoot::Local(LocalId(1)));
        assert!(!dropped.contains(&path(1)) && dropped.contains(&path(2)));
    }

    use super::{path_of, truth_sets};
    use crate::hir::{BinaryOp, Constant, Expr, ExprKind, LogicalOp};
    use crate::types::TypeInterner;
    use hollowscript_core::Interner;
    use hollowscript_syntax::ast::NodeId;

    fn local(id: u32, ty: crate::types::TypeId) -> Expr {
        Expr {
            node: NodeId(0),
            ty,
            kind: ExprKind::Local(LocalId(id)),
        }
    }

    fn none_literal(ty: crate::types::TypeId) -> Expr {
        Expr {
            node: NodeId(0),
            ty,
            kind: ExprKind::Literal(Constant::None),
        }
    }

    fn ne(lhs: Expr, rhs: Expr, ty: crate::types::TypeId) -> Expr {
        Expr {
            node: NodeId(0),
            ty,
            kind: ExprKind::Binary {
                op: BinaryOp::Ne {
                    scalar_operands: false,
                },
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        }
    }

    fn eq(lhs: Expr, rhs: Expr, ty: crate::types::TypeId) -> Expr {
        Expr {
            node: NodeId(0),
            ty,
            kind: ExprKind::Binary {
                op: BinaryOp::Eq {
                    scalar_operands: false,
                },
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        }
    }

    #[test]
    fn a_field_path_recovers_its_name_from_the_records_type() {
        let mut types = TypeInterner::new();
        let mut names = Interner::new();
        let owner = names.intern("owner");
        let opt_string = types.optional(types.string());
        let task = types.record(
            &names,
            vec![crate::types::Field {
                name: owner,
                ty: opt_string,
            }],
        );
        let target = local(0, task);
        let field = Expr {
            node: NodeId(1),
            ty: opt_string,
            kind: ExprKind::Field {
                target: Box::new(target),
                index: 0,
            },
        };
        let path = path_of(&field, &types, 0).unwrap();
        assert_eq!(path.root, PathRoot::Local(LocalId(0)));
        assert_eq!(
            path.fields.iter().map(|f| f.name).collect::<Vec<_>>(),
            vec![owner]
        );
    }

    #[test]
    fn not_none_gives_a_fact_only_when_true() {
        let mut types = TypeInterner::new();
        let opt_int = types.optional(types.int());
        let cond = ne(local(0, opt_int), none_literal(opt_int), types.bool());
        let sets = truth_sets(&cond, &types, 0);
        assert!(sets.when_true.contains(&path(0)));
        assert!(sets.when_false.as_vec().is_empty());
    }

    #[test]
    fn none_reversed_and_equal_to_none_give_a_fact_only_when_false() {
        let mut types = TypeInterner::new();
        let opt_int = types.optional(types.int());
        let cond = ne(none_literal(opt_int), local(0, opt_int), types.bool());
        assert!(truth_sets(&cond, &types, 0).when_true.contains(&path(0)));

        let cond = eq(local(1, opt_int), none_literal(opt_int), types.bool());
        let sets = truth_sets(&cond, &types, 0);
        assert!(sets.when_true.as_vec().is_empty());
        assert!(sets.when_false.contains(&path(1)));
    }

    #[test]
    fn not_flips_the_two_sets() {
        let mut types = TypeInterner::new();
        let opt_int = types.optional(types.int());
        let inner = ne(local(0, opt_int), none_literal(opt_int), types.bool());
        let cond = Expr {
            node: NodeId(2),
            ty: types.bool(),
            kind: ExprKind::Not(Box::new(inner)),
        };
        let sets = truth_sets(&cond, &types, 0);
        assert!(sets.when_true.as_vec().is_empty());
        assert!(sets.when_false.contains(&path(0)));
    }

    #[test]
    fn and_narrows_both_sides_when_true_but_not_when_false() {
        // a != none and b != none
        let mut types = TypeInterner::new();
        let opt_int = types.optional(types.int());
        let a = ne(local(0, opt_int), none_literal(opt_int), types.bool());
        let b = ne(local(1, opt_int), none_literal(opt_int), types.bool());
        let cond = Expr {
            node: NodeId(3),
            ty: types.bool(),
            kind: ExprKind::Logical {
                op: LogicalOp::And,
                lhs: Box::new(a),
                rhs: Box::new(b),
                rhs_facts: Vec::new(),
            },
        };
        let sets = truth_sets(&cond, &types, 0);
        assert!(sets.when_true.contains(&path(0)) && sets.when_true.contains(&path(1)));
        assert!(sets.when_false.as_vec().is_empty());
    }

    #[test]
    fn or_narrows_both_sides_when_false_but_not_when_true() {
        // a == none or b == none
        let mut types = TypeInterner::new();
        let opt_int = types.optional(types.int());
        let a = eq(local(0, opt_int), none_literal(opt_int), types.bool());
        let b = eq(local(1, opt_int), none_literal(opt_int), types.bool());
        let cond = Expr {
            node: NodeId(4),
            ty: types.bool(),
            kind: ExprKind::Logical {
                op: LogicalOp::Or,
                lhs: Box::new(a),
                rhs: Box::new(b),
                rhs_facts: Vec::new(),
            },
        };
        let sets = truth_sets(&cond, &types, 0);
        assert!(sets.when_true.as_vec().is_empty());
        assert!(sets.when_false.contains(&path(0)) && sets.when_false.contains(&path(1)));
    }

    #[test]
    fn an_unrecognised_condition_gives_no_facts_either_way() {
        let types = TypeInterner::new();
        let cond = local(0, types.bool());
        let sets = truth_sets(&cond, &types, 0);
        assert!(sets.when_true.as_vec().is_empty());
        assert!(sets.when_false.as_vec().is_empty());
    }
}
