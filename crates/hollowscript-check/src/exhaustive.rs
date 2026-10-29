//! Exhaustiveness and unreachable arms (section 7.7).
//!
//! The algorithm works over a *matrix* of pattern rows reduced to
//! [`Constructor`]s: literals, `none`, a variant, a record's single
//! constructor, a list's length classes, or a wildcard. The specification's
//! `useful(P, q)` decides both exhaustiveness (`useful(P, [_])`) and whether
//! row *i* is reachable (`useful(P_i, row_i)`). The work is bounded
//! (section 7.7's budget): a [`Budget`] is threaded through and the algorithm
//! stops with HS0511 rather than run unbounded on hostile input.
//!
//! ## The contract a caller must follow
//!
//! [`Row::columns`] is a *pre-order flattening* of one alternative's whole
//! pattern tree, not one entry per top-level pattern: a fixed-arity
//! constructor (a variant, a record, `some`) is followed immediately by the
//! flattened columns of its own sub-patterns, in order, and a wildcard or
//! binding contributes exactly one [`Constructor::Wildcard`] column with
//! nothing after it. This is what lets this module's own specialize step
//! turn a matched row into its continuation by simply dropping the head
//! column: the sub-patterns are already sitting right after it. A caller
//! that builds `rows` (not yet written; see the checker's own gap) must
//! flatten every arm's alternatives this way before calling [`check_match`].

use std::collections::BTreeSet;

/// One constructor of a scrutinee's type, or a wildcard, in the reduced
/// pattern matrix (section 7.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Constructor {
    /// `_` or a binding: matches anything, no sub-patterns.
    Wildcard,
    /// A literal value, compared as the specification's value equality: `0.0`,
    /// `-0.0` and `0.00` are one constructor, and so are `1` and `01`.
    Literal(crate::hir::Constant),
    /// `none`, arity 0.
    None,
    /// A present value of an optional, arity 1, standing for any constructor
    /// pattern of the inner type other than `none`.
    Some,
    /// `true` or `false`.
    Bool(bool),
    /// One variant of an enum, by position, with its declared arity.
    Variant {
        /// The position of the variant within its enum's declared order.
        index: u32,
        /// The number of payload values.
        arity: u32,
    },
    /// The record type's single constructor, one argument per field in
    /// ascending field name order.
    Record {
        /// The number of fields.
        arity: u32,
    },
    /// A list of exactly `length` elements, or, for the last constructor of a
    /// column, `length` or more (used for a rest pattern's wildcard extension).
    ListLength {
        /// The number of elements, or the number before the rest.
        length: u32,
        /// Whether this stands for `length` or more (the last constructor of a column).
        or_more: bool,
    },
}

impl Constructor {
    /// The number of sub-patterns this constructor takes.
    #[must_use]
    pub fn arity(&self) -> u32 {
        match self {
            Constructor::Wildcard
            | Constructor::None
            | Constructor::Bool(_)
            | Constructor::Literal(_) => 0,
            Constructor::Some => 1,
            Constructor::Variant { arity, .. } | Constructor::Record { arity } => *arity,
            Constructor::ListLength { length, .. } => *length,
        }
    }
}

/// One row of the reduced pattern matrix: a sequence of constructors, one per
/// column, with a wildcard already expanded to its own row per alternative of
/// a `match` arm (`useful`'s precondition in section 7.7).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// One constructor per column.
    pub columns: Vec<Constructor>,
}

/// A bound on the work `useful` may do, per section 7.7: a `match` of more
/// than 1,000 rows, a list pattern of more than 250 elements, or a query count
/// above 100,000 units is HS0511, and then neither HS0501, HS0502 nor HS0503
/// is reported for it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Units of `useful` spent so far.
    pub units_used: u64,
    /// The limit, `hollowscript_core::limits::structural::MAX_EXHAUSTIVENESS_WORK`.
    pub unit_limit: u64,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget::new()
    }
}

impl Budget {
    /// A fresh budget at the limits of section 1.10.
    #[must_use]
    pub fn new() -> Budget {
        Budget {
            units_used: 0,
            unit_limit: hollowscript_core::limits::structural::MAX_EXHAUSTIVENESS_WORK,
        }
    }

    /// Charges one unit for a call of `useful`.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] once the limit is spent; the caller reports HS0511.
    pub fn charge(&mut self) -> Result<(), BudgetExceeded> {
        self.units_used += 1;
        if self.units_used > self.unit_limit {
            Err(BudgetExceeded)
        } else {
            Ok(())
        }
    }
}

/// The work budget of section 7.7 was spent.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BudgetExceeded;

/// The outcome of checking one `match`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MatchReport {
    /// Whether the `match` is exhaustive.
    pub exhaustive: bool,
    /// The rows, by index, that are unreachable (HS0503), or repeat an
    /// earlier row's top-level literal alternative (HS0502).
    pub unreachable: Vec<(usize, bool)>,
    /// Whether the algorithm gave up under the budget of section 7.7
    /// (HS0511), in which case `exhaustive` and `unreachable` are meaningless.
    pub too_complex: bool,
}

/// Checks a `match`'s rows for exhaustiveness and unreachable arms, by the
/// `useful(P, q)` algorithm of section 7.7.
///
/// `scrutinee_constructors` is the complete constructor set of the
/// scrutinee's own type (empty for `Int`, `Float`, `String`, `Map` or `Set`,
/// which have none); it is consulted only for the outermost column, the one
/// column the scrutinee itself fills, because that is the only column this
/// function is given a type for. Every other column comes from specializing
/// a constructor's own sub-patterns, whose type this function is not told:
/// for `Bool`, `none`/`some` and a record, section 7.7 already fixes what a
/// complete signature looks like regardless of type (two booleans, the two
/// optionality cases, or the type's one constructor), so nesting does not
/// matter and completeness at any depth is decided from the rows alone. A
/// nested column of variants is the one case this cannot cover: without the
/// enum's own variant count this function cannot tell "every variant I see"
/// from "every variant that exists", so it conservatively treats a nested
/// variant column as never complete, which can only ever cost a spurious
/// `else`, never accept a program that skips a case. Closing that gap needs
/// this function, or its caller, to be given a type for every column, which
/// is a real interface gap, not something to guess at here.
#[must_use]
pub fn check_match(rows: &[Row], scrutinee_constructors: &[Constructor]) -> MatchReport {
    use hollowscript_core::limits::structural::{MAX_LIST_PATTERN_ELEMENTS, MAX_MATCH_ROWS};
    let list_pattern_too_long = |c: &Constructor| matches!(c, Constructor::ListLength { length, .. } if *length as usize > MAX_LIST_PATTERN_ELEMENTS);
    let too_complex = rows.len() > MAX_MATCH_ROWS
        || rows
            .iter()
            .any(|r| r.columns.iter().any(list_pattern_too_long))
        || scrutinee_constructors.iter().any(list_pattern_too_long);
    if too_complex {
        return too_complex_report();
    }

    let mut budget = Budget::new();
    let wildcard_query = [Constructor::Wildcard];
    let not_exhaustive = match useful(
        rows,
        &wildcard_query,
        Some(scrutinee_constructors),
        &mut budget,
    ) {
        Ok(is_useful) => is_useful,
        Err(BudgetExceeded) => return too_complex_report(),
    };

    let mut unreachable = Vec::new();
    for (i, current) in rows.iter().enumerate() {
        let prior = rows.get(..i).unwrap_or(&[]);
        let reachable = match useful(
            prior,
            &current.columns,
            Some(scrutinee_constructors),
            &mut budget,
        ) {
            Ok(is_useful) => is_useful,
            Err(BudgetExceeded) => return too_complex_report(),
        };
        if !reachable {
            unreachable.push((i, is_literal_repeat(rows, i)));
        }
    }

    MatchReport {
        exhaustive: !not_exhaustive,
        unreachable,
        too_complex: false,
    }
}

fn too_complex_report() -> MatchReport {
    MatchReport {
        exhaustive: false,
        unreachable: Vec::new(),
        too_complex: true,
    }
}

/// Whether row `i`'s own top-level pattern is a literal that some earlier row
/// already tested (HS0502), as opposed to being unreachable for any other
/// reason (HS0503): section 7.7 reports these two differently, and the
/// distinction is which earlier row did the shadowing, not merely that this
/// row is a literal.
fn is_literal_repeat(rows: &[Row], i: usize) -> bool {
    let Some(Constructor::Literal(value)) = rows.get(i).and_then(|row| row.columns.first()) else {
        return false;
    };
    rows.get(..i).unwrap_or(&[]).iter().any(
        |row| matches!(row.columns.first(), Some(Constructor::Literal(earlier)) if earlier == value),
    )
}

/// `useful(matrix, query)`: whether some value the flattened `query` matches
/// is matched by no row of `matrix`. `top` is `scrutinee_constructors`,
/// consulted only while `query` still holds the scrutinee's own, outermost
/// column (see [`check_match`]'s own documentation): every recursive call
/// below passes `None`, because by then `query`'s head names a column this
/// function was never given a type for.
fn useful(
    matrix: &[Row],
    query: &[Constructor],
    top: Option<&[Constructor]>,
    budget: &mut Budget,
) -> Result<bool, BudgetExceeded> {
    budget.charge()?;
    let Some((head, tail)) = query.split_first() else {
        // Rule 1: an empty query matches everything; it is useful exactly
        // when nothing in the matrix already does.
        return Ok(matrix.is_empty());
    };
    match head {
        Constructor::Wildcard => {
            // Rule 3.
            let sigma = column_constructors(matrix);
            if is_complete(&sigma, top) {
                for c in expand_signature(&sigma, top) {
                    let mut sub_query = vec![Constructor::Wildcard; c.arity() as usize];
                    sub_query.extend_from_slice(tail);
                    if useful(&specialize(matrix, &c), &sub_query, None, budget)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            } else {
                useful(&default_matrix(matrix), tail, None, budget)
            }
        }
        c => {
            // Rule 2: `query`'s own sub-patterns are already `tail`'s
            // leading columns (the flattening `check_match` documents), so
            // specializing the query is just using `tail` as it stands.
            useful(&specialize(matrix, c), tail, None, budget)
        }
    }
}

/// `S(c, P)`: the rows of `matrix` whose head is `c` or a wildcard, with the
/// head replaced by its sub-patterns (already `tail`'s leading columns for a
/// row that already names `c`) or, for a wildcard head or a list head that
/// stands for a longer or unbounded `c` (section 7.7's "wildcard-extended
/// constructor"), padded out to `c`'s own arity with fresh wildcards.
fn specialize(matrix: &[Row], c: &Constructor) -> Vec<Row> {
    let mut out = Vec::with_capacity(matrix.len());
    for row in matrix {
        let Some((head, tail)) = row.columns.split_first() else {
            continue;
        };
        let covers = matches!(head, Constructor::Wildcard) || constructor_covers(head, c);
        if !covers {
            continue;
        }
        let padding = c.arity().saturating_sub(head.arity());
        let mut columns = vec![Constructor::Wildcard; padding as usize];
        columns.extend_from_slice(tail);
        out.push(Row { columns });
    }
    out
}

/// `D(P)`: the rows of `matrix` whose head is a wildcard, with that head dropped.
fn default_matrix(matrix: &[Row]) -> Vec<Row> {
    matrix
        .iter()
        .filter_map(|row| {
            let (head, tail) = row.columns.split_first()?;
            matches!(head, Constructor::Wildcard).then(|| Row {
                columns: tail.to_vec(),
            })
        })
        .collect()
}

/// `Σ`: the distinct constructors occurring as the head of some row of
/// `matrix`, in first-seen order. Never includes [`Constructor::Wildcard`],
/// which is not itself a constructor of any type.
fn column_constructors(matrix: &[Row]) -> Vec<Constructor> {
    let mut seen = Vec::new();
    for row in matrix {
        if let Some(head) = row.columns.first() {
            if !matches!(head, Constructor::Wildcard) && !seen.contains(head) {
                seen.push(head.clone());
            }
        }
    }
    seen
}

/// Whether a row's own head `head` is exactly `target`, or (for a list's
/// "at least k" head) stands for it: section 7.7's constructor family for
/// `[T]` is defined so that specializing against a longer, or unbounded,
/// target still matches a shorter rest-pattern head.
fn constructor_covers(head: &Constructor, target: &Constructor) -> bool {
    match (head, target) {
        (Constructor::Literal(a), Constructor::Literal(b)) => a == b,
        (Constructor::None, Constructor::None)
        | (Constructor::Some, Constructor::Some)
        | (Constructor::Record { .. }, Constructor::Record { .. }) => true,
        (Constructor::Bool(a), Constructor::Bool(b)) => a == b,
        (Constructor::Variant { index: i, .. }, Constructor::Variant { index: j, .. }) => i == j,
        (
            Constructor::ListLength {
                length: k,
                or_more: head_or_more,
            },
            Constructor::ListLength {
                length: n,
                or_more: target_or_more,
            },
        ) => list_covers(*k, *head_or_more, *n, *target_or_more),
        _ => false,
    }
}

/// Whether a list head of `head_length` elements (`head_or_more`: "or more")
/// covers a target of `target_length` elements (`target_or_more`: "or more").
fn list_covers(
    head_length: u32,
    head_or_more: bool,
    target_length: u32,
    target_or_more: bool,
) -> bool {
    if head_or_more {
        head_length <= target_length
    } else {
        !target_or_more && head_length == target_length
    }
}

/// Whether `sigma` (a column's own observed constructors) is a *complete
/// signature* of its type (section 7.7): every constructor the type has
/// appears in `sigma`. See [`check_match`]'s own documentation for why this
/// needs no external type information except for a variant at the outermost
/// column (`top`).
fn is_complete(sigma: &[Constructor], top: Option<&[Constructor]>) -> bool {
    match sigma.first() {
        None | Some(Constructor::Literal(_) | Constructor::Wildcard) => false,
        Some(Constructor::Bool(_)) => {
            sigma.contains(&Constructor::Bool(true)) && sigma.contains(&Constructor::Bool(false))
        }
        Some(Constructor::None | Constructor::Some) => {
            sigma.contains(&Constructor::None) && sigma.contains(&Constructor::Some)
        }
        Some(Constructor::Record { .. }) => true,
        Some(Constructor::ListLength { .. }) => list_signature_is_complete(sigma),
        Some(Constructor::Variant { .. }) => top.is_some_and(|all| {
            let declared: BTreeSet<u32> = all.iter().filter_map(variant_index).collect();
            let seen: BTreeSet<u32> = sigma.iter().filter_map(variant_index).collect();
            !declared.is_empty() && declared == seen
        }),
    }
}

fn variant_index(c: &Constructor) -> Option<u32> {
    if let Constructor::Variant { index, .. } = c {
        Some(*index)
    } else {
        None
    }
}

/// Section 7.7's list constructor family is defined relative to the column's
/// own largest observed length `M`: complete exactly when every length from
/// zero to `M`, and "`M + 1` or more", is covered by some row of `sigma`.
fn list_signature_is_complete(sigma: &[Constructor]) -> bool {
    let m = sigma
        .iter()
        .filter_map(|c| match c {
            Constructor::ListLength { length, .. } => Some(*length),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let covered = |length: u32, or_more: bool| {
        sigma.iter().any(|c| match c {
            Constructor::ListLength {
                length: k,
                or_more: k_or_more,
            } => list_covers(*k, *k_or_more, length, or_more),
            _ => false,
        })
    };
    (0..=m).all(|n| covered(n, false)) && covered(m + 1, true)
}

/// Given a complete `sigma`, the definitive constructors to case-split on:
/// for most types this is a fixed or `sigma`-derived set, but for a variant
/// it is read from `top` (see [`check_match`]'s documentation), since
/// `sigma` alone might list a variant's declared arity inconsistently with
/// another row naming the same variant.
fn expand_signature(sigma: &[Constructor], top: Option<&[Constructor]>) -> Vec<Constructor> {
    match sigma.first() {
        Some(Constructor::Bool(_)) => vec![Constructor::Bool(true), Constructor::Bool(false)],
        Some(Constructor::None | Constructor::Some) => vec![Constructor::None, Constructor::Some],
        Some(record @ Constructor::Record { .. }) => vec![record.clone()],
        Some(Constructor::ListLength { .. }) => {
            let m = sigma
                .iter()
                .filter_map(|c| match c {
                    Constructor::ListLength { length, .. } => Some(*length),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            let mut all: Vec<Constructor> = (0..=m)
                .map(|length| Constructor::ListLength {
                    length,
                    or_more: false,
                })
                .collect();
            all.push(Constructor::ListLength {
                length: m + 1,
                or_more: true,
            });
            all
        }
        Some(Constructor::Variant { .. }) => top.map_or_else(Vec::new, |all| {
            all.iter()
                .filter(|c| matches!(c, Constructor::Variant { .. }))
                .cloned()
                .collect()
        }),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Budget, Constructor, Row, check_match};
    use crate::hir::Constant;

    fn row(columns: Vec<Constructor>) -> Row {
        Row { columns }
    }

    #[test]
    fn arities_match_the_specification() {
        assert_eq!(Constructor::None.arity(), 0);
        assert_eq!(Constructor::Some.arity(), 1);
        assert_eq!(Constructor::Variant { index: 2, arity: 3 }.arity(), 3);
        assert_eq!(
            Constructor::ListLength {
                length: 4,
                or_more: true
            }
            .arity(),
            4
        );
    }

    #[test]
    fn the_budget_stops_at_its_limit() {
        let mut b = Budget {
            units_used: 0,
            unit_limit: 3,
        };
        assert_eq!(b.charge(), Ok(()));
        assert_eq!(b.charge(), Ok(()));
        assert_eq!(b.charge(), Ok(()));
        assert_eq!(b.charge(), Err(super::BudgetExceeded));
    }

    fn bools() -> Vec<Constructor> {
        vec![Constructor::Bool(true), Constructor::Bool(false)]
    }

    #[test]
    fn a_bool_match_with_one_arm_and_no_wildcard_is_not_exhaustive() {
        let rows = vec![row(vec![Constructor::Bool(true)])];
        let report = check_match(&rows, &bools());
        assert!(
            !report.exhaustive,
            "true alone does not cover false, and there is no wildcard"
        );
        assert!(!report.too_complex);
    }

    #[test]
    fn a_bool_match_with_both_values_is_exhaustive() {
        let rows = vec![
            row(vec![Constructor::Bool(true)]),
            row(vec![Constructor::Bool(false)]),
        ];
        let report = check_match(&rows, &bools());
        assert!(report.exhaustive);
        assert!(report.unreachable.is_empty());
    }

    #[test]
    fn an_else_arm_alone_is_exhaustive() {
        let rows = vec![row(vec![Constructor::Wildcard])];
        let report = check_match(&rows, &bools());
        assert!(report.exhaustive);
    }

    #[test]
    fn a_repeated_literal_is_hs0502() {
        // constant n = 1; match n { 1 {...} 1 {...} else {...} }
        let one = Constructor::Literal(Constant::Int(1));
        let rows = vec![
            row(vec![one.clone()]),
            row(vec![one]),
            row(vec![Constructor::Wildcard]),
        ];
        let report = check_match(&rows, &[]);
        assert!(report.exhaustive);
        assert_eq!(report.unreachable, vec![(1, true)], "HS0502, not HS0503");
    }

    #[test]
    fn a_literal_shadowed_by_an_earlier_binding_is_hs0503() {
        // constant n = 1; match n { x {...} 1 {...} }
        let rows = vec![
            row(vec![Constructor::Wildcard]),
            row(vec![Constructor::Literal(Constant::Int(1))]),
        ];
        let report = check_match(&rows, &[]);
        assert_eq!(
            report.unreachable,
            vec![(1, false)],
            "shadowed by a binding, not a repeated literal: HS0503"
        );
    }

    #[test]
    fn an_enum_missing_a_variant_is_not_exhaustive() {
        let all = vec![
            Constructor::Variant { index: 0, arity: 0 },
            Constructor::Variant { index: 1, arity: 0 },
        ];
        let rows = vec![row(vec![Constructor::Variant { index: 0, arity: 0 }])];
        let report = check_match(&rows, &all);
        assert!(!report.exhaustive);
    }

    #[test]
    fn an_enum_with_every_variant_is_exhaustive_without_an_else() {
        let all = vec![
            Constructor::Variant { index: 0, arity: 0 },
            Constructor::Variant { index: 1, arity: 0 },
        ];
        let rows = vec![
            row(vec![Constructor::Variant { index: 0, arity: 0 }]),
            row(vec![Constructor::Variant { index: 1, arity: 0 }]),
        ];
        let report = check_match(&rows, &all);
        assert!(report.exhaustive);
    }

    #[test]
    fn empty_and_first_rest_together_cover_every_list() {
        // match xs { [] {...} [first, ..rest] {...} }
        let rows = vec![
            row(vec![Constructor::ListLength {
                length: 0,
                or_more: false,
            }]),
            row(vec![
                Constructor::ListLength {
                    length: 1,
                    or_more: true,
                },
                Constructor::Wildcard, // `first`
            ]),
        ];
        let report = check_match(&rows, &[]);
        assert!(report.exhaustive, "[] and [first, ..rest] cover every list");
    }

    #[test]
    fn a_fixed_length_list_pattern_alone_is_not_exhaustive() {
        let rows = vec![row(vec![Constructor::ListLength {
            length: 2,
            or_more: false,
        }])];
        let report = check_match(&rows, &[]);
        assert!(!report.exhaustive);
    }

    #[test]
    fn more_than_the_row_limit_is_too_complex_not_a_false_exhaustive() {
        let rows: Vec<Row> = (0..1_001)
            .map(|i| row(vec![Constructor::Literal(Constant::Int(i))]))
            .collect();
        let report = check_match(&rows, &[]);
        assert!(report.too_complex);
        assert!(
            !report.exhaustive,
            "too-complex must not be reported as exhaustive"
        );
        assert!(report.unreachable.is_empty());
    }

    #[test]
    fn a_list_pattern_longer_than_the_limit_is_too_complex() {
        let rows = vec![row(vec![Constructor::ListLength {
            length: 251,
            or_more: false,
        }])];
        let report = check_match(&rows, &[]);
        assert!(report.too_complex);
    }

    #[test]
    fn a_variant_payload_pattern_is_checked_against_the_flattened_continuation() {
        // enum E { A(Bool) }: match e { A(true) {...} A(false) {...} } is
        // exhaustive; A(true) alone is not. This exercises specialize()
        // dropping a matched head to reveal the payload's own column.
        let a = |payload: bool| {
            row(vec![
                Constructor::Variant { index: 0, arity: 1 },
                Constructor::Bool(payload),
            ])
        };
        let one_arm = vec![a(true)];
        let scrutinee = vec![Constructor::Variant { index: 0, arity: 1 }];
        assert!(!check_match(&one_arm, &scrutinee).exhaustive);

        let both_arms = vec![a(true), a(false)];
        assert!(check_match(&both_arms, &scrutinee).exhaustive);
    }
}
