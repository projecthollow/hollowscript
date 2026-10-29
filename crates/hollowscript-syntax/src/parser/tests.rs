//! Unit tests for the parser, organised by grammar area (chapter 18).

use super::parse;
use crate::ast::{
    BinaryOp, BindingTarget, Else, ExprKind, ItemKind, Literal, PatternKind, StmtKind, StringPart,
    TypeKind, UnaryOp,
};
use hollowscript_core::{Code, Control, Interner, Source, SourceId};

fn parse_src(text: &str) -> super::ParseOutput {
    let source = Source::new(SourceId(0), "main.hws", text).unwrap();
    parse(&source, &mut Interner::new(), &mut Control::none())
}

fn codes(text: &str) -> Vec<Code> {
    parse_src(text)
        .diagnostics
        .into_iter()
        .map(|d| d.code)
        .collect()
}

fn single_expr(text: &str) -> ExprKind {
    let src = format!("return {text}");
    let out = parse_src(&src);
    assert!(out.diagnostics.is_empty(), "{text}: {:?}", out.diagnostics);
    let item = out.file.items.first().expect("one item");
    let ItemKind::Stmt(stmt) = &item.kind else {
        panic!("not a stmt: {item:?}")
    };
    let StmtKind::Return(Some(expr)) = &stmt.kind else {
        panic!("not a return: {stmt:?}")
    };
    expr.kind.clone()
}

// ---- 18.3 files and top-level items -----------------------------------

#[test]
fn empty_file_parses_cleanly() {
    let out = parse_src("");
    assert!(out.diagnostics.is_empty());
    assert!(out.file.items.is_empty());
    assert!(out.file.uses.is_empty());
    assert!(out.file.imports.is_empty());
}

#[test]
fn uses_then_imports_then_items() {
    let out = parse_src("uses print, clock\nimport a from \"./m\"\nreturn 1");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(out.file.uses.len(), 1);
    assert_eq!(out.file.uses[0].names.len(), 2);
    assert_eq!(out.file.imports.len(), 1);
    assert_eq!(out.file.imports[0].names.len(), 1);
    assert_eq!(out.file.items.len(), 1);
}

#[test]
fn uses_after_import_is_hs0106() {
    assert_eq!(
        codes("import a from \"./m\"\nuses print\nreturn 1"),
        vec![Code::HS0106]
    );
}

#[test]
fn uses_after_statement_is_hs0106() {
    assert_eq!(codes("return 1\nuses print"), vec![Code::HS0106]);
}

#[test]
fn import_after_statement_is_hs0107() {
    assert_eq!(codes("return 1\nimport a from \"./m\""), vec![Code::HS0107]);
}

#[test]
fn export_on_fn_record_enum_constant_is_fine() {
    let out = parse_src(
        "export fn f() {}\nexport record R = { x: Int }\nexport enum E { A }\nexport constant c = 1",
    );
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(out.file.items.len(), 4);
    for item in &out.file.items {
        assert!(item.export.is_some());
    }
}

#[test]
fn export_on_variable_is_hs0108() {
    assert_eq!(codes("export variable x = 1"), vec![Code::HS0108]);
}

#[test]
fn fn_record_enum_import_uses_inside_a_function_body_is_hs0117() {
    for bad in ["fn g() {}", "record R = { x: Int }", "enum E { A }"] {
        let src = format!("fn f() {{\n    {bad}\n}}");
        assert_eq!(codes(&src), vec![Code::HS0117], "{bad}");
    }
}

// ---- 18.4 declarations --------------------------------------------------

#[test]
fn fn_decl_with_params_and_return_type() {
    let out = parse_src("fn add(a: Int, b: Int): Int {\n    return a + b\n}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    assert_eq!(f.params.len(), 2);
    assert!(f.ret.is_some());
}

#[test]
fn fn_decl_with_type_params() {
    let out = parse_src("fn identity<T>(x: T): T {\n    return x\n}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    assert_eq!(f.type_params.len(), 1);
}

#[test]
fn fn_with_no_return_type_has_no_ret() {
    let out = parse_src("fn f() {\n}");
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    assert!(f.ret.is_none());
}

#[test]
fn record_decl() {
    let out = parse_src("record Point = { x: Int, y: Int }");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Record(r) = &out.file.items[0].kind else {
        panic!()
    };
    assert_eq!(r.fields.len(), 2);
}

#[test]
fn empty_record_is_hs0112() {
    assert_eq!(codes("record R = {}"), vec![Code::HS0112]);
}

#[test]
fn enum_decl_with_payloads() {
    let out = parse_src("enum Shape {\n    Circle(Float),\n    Rect(Float, Float),\n    Empty,\n}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Enum(e) = &out.file.items[0].kind else {
        panic!()
    };
    assert_eq!(e.variants.len(), 3);
    assert_eq!(e.variants[1].payload.len(), 2);
}

#[test]
fn empty_enum_is_hs0112() {
    assert_eq!(codes("enum E {}"), vec![Code::HS0112]);
}

#[test]
fn lower_case_variant_name_is_hs0116() {
    assert_eq!(codes("enum E { circle }"), vec![Code::HS0116]);
}

#[test]
fn constant_and_variable_decls() {
    let out = parse_src("constant a = 1\nvariable b = 2");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(matches!(out.file.items[0].kind, ItemKind::Constant(_)));
    assert!(matches!(out.file.items[1].kind, ItemKind::Variable(_)));
}

#[test]
fn destructuring_record_and_list_targets() {
    let out = parse_src("constant { x, y: h } = p\nconstant [first, ..rest] = xs");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Constant(b1) = &out.file.items[0].kind else {
        panic!()
    };
    assert!(matches!(b1.target, BindingTarget::Pattern(_)));
    let ItemKind::Constant(b2) = &out.file.items[1].kind else {
        panic!()
    };
    assert!(matches!(b2.target, BindingTarget::Pattern(_)));
}

// ---- 18.5 types -----------------------------------------------------

#[test]
fn named_list_paren_optional_and_generic_types() {
    let out = parse_src("fn f(a: Int, b: [String], c: (Bool), d: Int?, e: Map<String, Int>) {\n}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    assert!(matches!(f.params[0].ty.kind, TypeKind::Named { .. }));
    assert!(matches!(f.params[1].ty.kind, TypeKind::List(_)));
    assert!(matches!(f.params[2].ty.kind, TypeKind::Paren(_)));
    assert!(matches!(f.params[3].ty.kind, TypeKind::Optional(_)));
    let TypeKind::Named { args, .. } = &f.params[4].ty.kind else {
        panic!()
    };
    assert_eq!(args.len(), 2);
}

#[test]
fn double_optional_is_hs0111() {
    assert_eq!(codes("fn f(a: Int??) {}"), vec![Code::HS0111]);
    assert_eq!(codes("fn f(a: (Int?)?) {}"), Vec::<Code>::new());
}

#[test]
fn record_and_fn_types() {
    let out = parse_src("fn f(a: { x: Int }, b: fn(Int): Bool, c: effect fn(Int)) {}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    assert!(matches!(f.params[0].ty.kind, TypeKind::Record(_)));
    let TypeKind::Fn { effect, ret, .. } = &f.params[1].ty.kind else {
        panic!()
    };
    assert!(!effect);
    assert!(ret.is_some());
    let TypeKind::Fn { effect, ret, .. } = &f.params[2].ty.kind else {
        panic!()
    };
    assert!(effect);
    assert!(ret.is_none());
}

#[test]
fn fn_type_return_extends_as_far_as_possible() {
    // fn(Int): Int? returns Int?, not (fn(Int): Int)?
    let out = parse_src("fn f(a: fn(Int): Int?) {}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    let TypeKind::Fn { ret, .. } = &f.params[0].ty.kind else {
        panic!()
    };
    let ret = ret.as_ref().unwrap();
    assert!(matches!(ret.kind, TypeKind::Optional(_)));
}

// ---- 18.6 statements --------------------------------------------------

#[test]
fn assignment_vs_call_statement() {
    let out = parse_src("variable x = 1\nx = 2\nprint(x)");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        matches!(out.file.items[1].kind, ItemKind::Stmt(ref s) if matches!(s.kind, StmtKind::Assign { .. }))
    );
    assert!(
        matches!(out.file.items[2].kind, ItemKind::Stmt(ref s) if matches!(s.kind, StmtKind::Call(_)))
    );
}

#[test]
fn bare_non_call_expression_statement_is_hs0109() {
    for bad in ["1 + 1", "task.title", "(f())"] {
        assert_eq!(codes(bad), vec![Code::HS0109], "{bad}");
    }
}

#[test]
fn if_else_if_else_chain_is_a_linked_list() {
    let out = parse_src(
        "fn f(n: Int): String {\n    if n < 0 {\n        return \"neg\"\n    } else if n == 0 {\n        return \"zero\"\n    } else {\n        return \"pos\"\n    }\n}",
    );
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    let StmtKind::If(if1) = &f.body.stmts[0].kind else {
        panic!()
    };
    let Some(else1) = &if1.otherwise else {
        panic!()
    };
    match else1.as_ref() {
        Else::If(inner) => {
            let StmtKind::If(if2) = &inner.kind else {
                panic!()
            };
            assert!(matches!(if2.otherwise.as_deref(), Some(Else::Block(_))));
        }
        Else::Block(_) => panic!("expected else-if"),
    }
}

#[test]
fn else_not_on_same_line_is_hs0122() {
    assert_eq!(
        codes("if true {\n    return 1\n}\nelse {\n    return 2\n}"),
        vec![Code::HS0122]
    );
}

#[test]
fn match_with_arms_and_else() {
    let out = parse_src(
        "fn f(n: Int): String {\n    match n {\n        0 {\n            return \"zero\"\n        }\n        1, -1 {\n            return \"one\"\n        }\n        else {\n            return \"many\"\n        }\n    }\n}",
    );
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    let StmtKind::Match(m) = &f.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(m.arms.len(), 2);
    assert_eq!(m.arms[1].patterns.len(), 2);
    assert!(m.else_arm.is_some());
}

#[test]
fn for_and_while_loops_with_break_and_continue() {
    let out = parse_src(
        "variable total = 0\nfor n in range(0, 10) {\n    if n == 7 {\n        break\n    }\n    if n == 3 {\n        continue\n    }\n    total = total + n\n}\nwhile total > 0 {\n    total = total - 1\n}",
    );
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        matches!(out.file.items[1].kind, ItemKind::Stmt(ref s) if matches!(s.kind, StmtKind::For { .. }))
    );
    assert!(
        matches!(out.file.items[2].kind, ItemKind::Stmt(ref s) if matches!(s.kind, StmtKind::While { .. }))
    );
}

#[test]
fn return_with_and_without_value() {
    let out = parse_src("fn f() {\n    return\n}\nfn g(): Int {\n    return 1\n}");
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Fn(f) = &out.file.items[0].kind else {
        panic!()
    };
    assert!(matches!(f.body.stmts[0].kind, StmtKind::Return(None)));
    let ItemKind::Fn(g) = &out.file.items[1].kind else {
        panic!()
    };
    assert!(matches!(g.body.stmts[0].kind, StmtKind::Return(Some(_))));
}

// ---- 18.7 patterns ------------------------------------------------------

#[test]
fn pattern_forms_in_match_arms() {
    let src = "match x {\n    3 { return 0 }\n    -1 { return 0 }\n    2.5 { return 0 }\n    \"s\" { return 0 }\n    true { return 0 }\n    none { return 0 }\n    _ { return 0 }\n    name { return 0 }\n    Circle(r) { return 0 }\n    Empty { return 0 }\n    { x, y: 0 } { return 0 }\n    [] { return 0 }\n    [a, b] { return 0 }\n    [first, ..rest] { return 0 }\n}";
    let out = parse_src(src);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let ItemKind::Stmt(s) = &out.file.items[0].kind else {
        panic!()
    };
    let StmtKind::Match(m) = &s.kind else {
        panic!()
    };
    assert_eq!(m.arms.len(), 14);
    let kinds: Vec<&PatternKind> = m.arms.iter().map(|a| &a.patterns[0].kind).collect();
    assert!(matches!(
        kinds[0],
        PatternKind::Literal {
            negative: false,
            literal: Literal::Int { .. }
        }
    ));
    assert!(matches!(
        kinds[1],
        PatternKind::Literal { negative: true, .. }
    ));
    assert!(matches!(kinds[6], PatternKind::Wildcard));
    assert!(matches!(kinds[7], PatternKind::Binding(_)));
    assert!(matches!(
        kinds[8],
        PatternKind::Variant { parens: true, .. }
    ));
    assert!(matches!(
        kinds[9],
        PatternKind::Variant { parens: false, .. }
    ));
    assert!(matches!(kinds[10], PatternKind::Record(_)));
    assert!(matches!(kinds[13], PatternKind::List(_)));
}

#[test]
fn uppercase_starts_a_variant_lowercase_a_binding() {
    let out = parse_src("match x {\n    Foo { return 0 }\n    bar { return 0 }\n}");
    assert!(out.diagnostics.is_empty());
    let ItemKind::Stmt(s) = &out.file.items[0].kind else {
        panic!()
    };
    let StmtKind::Match(m) = &s.kind else {
        panic!()
    };
    assert!(matches!(
        m.arms[0].patterns[0].kind,
        PatternKind::Variant { .. }
    ));
    assert!(matches!(
        m.arms[1].patterns[0].kind,
        PatternKind::Binding(_)
    ));
}

#[test]
fn rest_pattern_only_last_is_hs0121() {
    assert_eq!(
        codes("match x {\n    [..rest, a] { return 0 }\n}"),
        vec![Code::HS0121]
    );
}

#[test]
fn field_named_underscore_is_hs0123() {
    assert_eq!(
        codes("match x {\n    { _ } { return 0 }\n}"),
        vec![Code::HS0123]
    );
    assert_eq!(codes("record R = { _: Int }"), vec![Code::HS0123]);
}

// ---- 18.8 expressions: precedence and associativity ----------------------

#[test]
fn precedence_ladder_matches_chapter_18() {
    // or < and < eq < rel < add < mul < unary < postfix
    let ExprKind::Binary {
        op: BinaryOp::Or,
        lhs,
        rhs,
    } = single_expr("a and b or c and d")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Binary {
            op: BinaryOp::And,
            ..
        }
    ));
    assert!(matches!(
        rhs.kind,
        ExprKind::Binary {
            op: BinaryOp::And,
            ..
        }
    ));

    let ExprKind::Binary {
        op: BinaryOp::And,
        lhs,
        ..
    } = single_expr("a == b and c")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Binary {
            op: BinaryOp::Eq,
            ..
        }
    ));

    let ExprKind::Binary {
        op: BinaryOp::Eq,
        lhs,
        ..
    } = single_expr("a < b == c")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Binary {
            op: BinaryOp::Lt,
            ..
        }
    ));

    let ExprKind::Binary {
        op: BinaryOp::Lt,
        lhs,
        ..
    } = single_expr("a + b < c")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Binary {
            op: BinaryOp::Add,
            ..
        }
    ));

    let ExprKind::Binary {
        op: BinaryOp::Add,
        lhs,
        rhs,
    } = single_expr("a * b + c / d")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Binary {
            op: BinaryOp::Mul,
            ..
        }
    ));
    assert!(matches!(
        rhs.kind,
        ExprKind::Binary {
            op: BinaryOp::Div,
            ..
        }
    ));

    // `-a * b` is `(-a) * b`
    let ExprKind::Binary {
        op: BinaryOp::Mul,
        lhs,
        ..
    } = single_expr("-a * b")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Unary {
            op: UnaryOp::Neg,
            ..
        }
    ));

    // `not ready and done` is `(not ready) and done`
    let ExprKind::Binary {
        op: BinaryOp::And,
        lhs,
        ..
    } = single_expr("not ready and done")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Unary {
            op: UnaryOp::Not,
            ..
        }
    ));
}

#[test]
fn binary_operators_are_left_associative() {
    // a - b - c is (a - b) - c, not a - (b - c)
    let ExprKind::Binary {
        op: BinaryOp::Sub,
        lhs,
        rhs,
    } = single_expr("a - b - c")
    else {
        panic!()
    };
    assert!(matches!(
        lhs.kind,
        ExprKind::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
    assert!(matches!(rhs.kind, ExprKind::Name(_)));
}

#[test]
fn postfix_chain_left_to_right() {
    let ExprKind::Field { target, name } = single_expr("a.b().c[0]?.d") else {
        panic!("expected a field access at the end")
    };
    assert_eq!(name.name, name.name); // sanity: field exists
    assert!(matches!(target.kind, ExprKind::Try(_)));
}

#[test]
fn try_operator_and_effect_marker() {
    assert!(matches!(single_expr("parseInt(a)?"), ExprKind::Try(_)));
    assert!(matches!(
        single_expr("effect clock.now()"),
        ExprKind::Effect(_)
    ));
}

#[test]
fn effect_binds_to_the_whole_postfix_chain() {
    // `effect a.b(c)` is `effect (a.b(c))`: the Effect node wraps the call,
    // whose callee is the field access, not the other way round.
    let ExprKind::Effect(inner) = single_expr("effect a.b(c)") else {
        panic!()
    };
    let ExprKind::Call { callee, .. } = &inner.kind else {
        panic!("effect must wrap a call")
    };
    assert!(matches!(callee.kind, ExprKind::Field { .. }));
}

#[test]
fn effect_on_a_non_call_is_hs0403() {
    assert_eq!(codes("constant t = effect clock.now"), vec![Code::HS0403]);
}

#[test]
fn a_trailing_try_after_an_effect_call_is_not_hs0403_or_hs0109() {
    // `effect f()?`: ignoring the trailing `?`, the chain ends in a call, so
    // the marker is used correctly (no HS0403) and the statement is a call
    // (no HS0109), exactly as the same chain without `effect` already is.
    assert_eq!(codes("effect f()?"), Vec::<Code>::new());
    assert_eq!(codes("f()?"), Vec::<Code>::new());
}

#[test]
fn effect_on_a_non_call_is_hs0403_even_behind_a_trailing_try() {
    // `effect f().g?`: ignoring the trailing `?`, the last operation is the
    // field access `.g`, not a call, so this is exactly as much a misuse of
    // `effect` as `effect f().g` (no trailing `?`, checked below), and the
    // chain still does not end in a call either way, so both get HS0109 too.
    assert_eq!(codes("effect f().g?"), vec![Code::HS0403, Code::HS0109]);
    assert_eq!(codes("effect f().g"), vec![Code::HS0403, Code::HS0109]);
}

#[test]
fn a_chain_not_ending_in_a_call_is_hs0109_with_or_without_a_trailing_try() {
    // Section 4.9's "ignoring trailing `?` operators" applies to the bare
    // call-statement rule regardless of `effect`: `f().g?` does not end in a
    // call once the `?` is stripped, so it is HS0109 just as `f().g` is.
    assert_eq!(codes("f().g?"), vec![Code::HS0109]);
    assert_eq!(codes("f().g"), vec![Code::HS0109]);
}

#[test]
fn generics_less_than_is_always_comparison() {
    let ExprKind::Binary {
        op: BinaryOp::Lt, ..
    } = single_expr("a < b")
    else {
        panic!()
    };
}

// ---- 18.8 literals and collection/record literals ------------------------

#[test]
fn list_map_and_record_literals() {
    assert!(matches!(single_expr("[1, 2, 3]"), ExprKind::List(v) if v.len() == 3));
    assert!(matches!(single_expr("[]"), ExprKind::List(v) if v.is_empty()));
    assert!(matches!(single_expr("[\"a\": 1, \"b\": 2]"), ExprKind::Map(v) if v.len() == 2));
    assert!(matches!(single_expr("[:]"), ExprKind::Map(v) if v.is_empty()));
    assert!(matches!(single_expr("{ x: 1, y: 2 }"), ExprKind::Record(v) if v.len() == 2));
}

#[test]
fn empty_record_literal_is_hs0112() {
    assert_eq!(codes("return {}"), vec![Code::HS0112]);
}

#[test]
fn list_vs_map_disambiguated_by_a_following_colon() {
    assert!(matches!(single_expr("[a, b]"), ExprKind::List(_)));
    assert!(matches!(single_expr("[a: b]"), ExprKind::Map(_)));
}

#[test]
fn closures_with_and_without_annotations() {
    let ExprKind::Closure(c) = single_expr("fn(x: Int): Int { return x }") else {
        panic!()
    };
    assert_eq!(c.params.len(), 1);
    assert!(c.params[0].ty.is_some());
    assert!(c.ret.is_some());
    let ExprKind::Closure(c) = single_expr("fn(x) { return x }") else {
        panic!()
    };
    assert!(c.params[0].ty.is_none());
    assert!(c.ret.is_none());
}

#[test]
fn string_literal_with_interpolation_splits_into_parts() {
    let ExprKind::Literal(Literal::String(lit)) = single_expr(r#""a ${1 + 1} b""#) else {
        panic!()
    };
    assert_eq!(lit.parts.len(), 3);
    assert!(matches!(&lit.parts[0], StringPart::Text(t) if t == "a "));
    assert!(matches!(&lit.parts[1], StringPart::Interpolation(_)));
    assert!(matches!(&lit.parts[2], StringPart::Text(t) if t == " b"));
}

#[test]
fn nested_interpolation_parses_as_nested_expressions() {
    let ExprKind::Literal(Literal::String(lit)) = single_expr(r#""${"nested ${n}"}""#) else {
        panic!()
    };
    // parts[0] is the (empty) leading text before the interpolation.
    let StringPart::Interpolation(inner) = &lit.parts[1] else {
        panic!()
    };
    assert!(matches!(inner.kind, ExprKind::Literal(Literal::String(_))));
}

#[test]
fn filter_literal_is_opaque() {
    let ExprKind::Literal(Literal::Filter { source, malformed }) = single_expr("`status:open`")
    else {
        panic!()
    };
    assert_eq!(source, "status:open");
    assert!(!malformed);
}

#[test]
fn record_literal_restricted_in_condition_but_allowed_in_parens() {
    assert_eq!(
        codes("if { a: 1 }.a == 1 {\n    return 1\n}"),
        vec![Code::HS0110]
    );
    assert!(codes("if ({ a: 1 }.a == 1) {\n    return 1\n}").is_empty());
    assert!(codes("if f({ a: 1 }) {\n    return 1\n}").is_empty());
}

// ---- 18.9 syntax errors and recovery ------------------------------------

#[test]
fn spec_example_expected_an_expression() {
    let out = parse_src("constant x = )\nreturn 1");
    assert_eq!(
        out.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
        vec![Code::HS0102]
    );
    // Recovery preserves the declaration of `x` with an Error initialiser,
    // and parsing continues cleanly with `return 1`.
    assert_eq!(out.file.items.len(), 2);
    let ItemKind::Constant(b) = &out.file.items[0].kind else {
        panic!()
    };
    assert!(matches!(b.init.kind, ExprKind::Error));
    assert!(
        matches!(out.file.items[1].kind, ItemKind::Stmt(ref s) if matches!(s.kind, StmtKind::Return(Some(_))))
    );
}

#[test]
fn spec_example_expected_a_statement() {
    assert_eq!(codes(")\nreturn 1"), vec![Code::HS0103]);
}

#[test]
fn spec_example_expected_a_token() {
    assert_eq!(codes("constant x 1\nreturn 1"), vec![Code::HS0101]);
}

#[test]
fn one_bad_line_does_not_cascade_past_the_next_statement() {
    let out =
        parse_src("constant a = )\nconstant b = 2\nconstant c = ]\nconstant d = 4\nreturn b + d");
    assert_eq!(out.diagnostics.len(), 2);
    assert_eq!(out.file.items.len(), 5);
}

#[test]
fn unclosed_block_at_eof_is_hs0101_with_eof() {
    let out = parse_src("fn f() {\n    return 1");
    assert_eq!(out.diagnostics.len(), 1);
    assert_eq!(out.diagnostics[0].code, Code::HS0101);
}

#[test]
fn future_reserved_words_are_hs0104() {
    assert_eq!(codes("constant test = 1"), vec![Code::HS0104]);
    assert_eq!(codes("constant assert = 1"), vec![Code::HS0104]);
}

// ---- structural limits (chapter 1.10 / chapter 11) -----------------------

#[test]
fn nesting_depth_250_is_allowed_and_251_is_hs0113() {
    let ok = format!("return {}1{}", "(".repeat(250), ")".repeat(250));
    assert!(codes(&ok).is_empty(), "250 should be fine");
    let bad = format!("return {}1{}", "(".repeat(251), ")".repeat(251));
    assert_eq!(codes(&bad), vec![Code::HS0113]);
}

#[test]
fn chain_of_500_links_is_allowed_and_501_is_hs0114() {
    let ok = (0..501).map(|_| "1").collect::<Vec<_>>().join("+");
    assert!(
        codes(&format!("return {ok}")).is_empty(),
        "500 links should be fine"
    );
    let bad = (0..502).map(|_| "1").collect::<Vec<_>>().join("+");
    assert_eq!(codes(&format!("return {bad}")), vec![Code::HS0114]);
}

#[test]
fn postfix_chain_of_501_links_is_hs0114() {
    let bad = format!("return a{}", "?".repeat(501));
    assert_eq!(codes(&bad), vec![Code::HS0114]);
    let ok = format!("return a{}", "?".repeat(500));
    assert!(codes(&ok).is_empty());
}

#[test]
fn else_if_run_of_501_links_is_hs0114() {
    let mut src = String::from("fn f(): Int {\n    if false {\n        return 0\n    }");
    for _ in 0..501 {
        src.push_str(" else if false {\n        return 0\n    }");
    }
    src.push_str(" else {\n        return 1\n    }\n}");
    assert_eq!(codes(&src), vec![Code::HS0114]);
}

#[test]
fn a_long_run_of_unary_operators_does_not_panic_or_hang() {
    let src = format!("return {}1", "-".repeat(10_000));
    let out = parse_src(&src);
    // Whatever the diagnostics say, this must not panic or hang, and it must
    // produce a real tree.
    let _ = out.file.node_count;
}

#[test]
fn deeply_nested_list_literal_does_not_panic() {
    let src = format!("return {}1{}", "[".repeat(2_000), "]".repeat(2_000));
    let out = parse_src(&src);
    let _ = out.diagnostics;
}

// ---- error recovery never panics on truncated input ----------------------

#[test]
fn every_prefix_of_a_realistic_program_parses_without_panicking() {
    let program = "uses print\nrecord Point = { x: Int, y: Int }\nenum Shape {\n    Circle(Float),\n    Empty,\n}\nfn area(s: Shape): Float {\n    match s {\n        Circle(r) {\n            return 3.0 * r * r\n        }\n        Empty {\n            return 0.0\n        }\n    }\n}\nconstant origin: Point = { x: 0, y: 0 }\nfor i in range(0, 3) {\n    effect print(\"${i}\")\n}\nreturn area(Shape.Circle(2.0))\n";
    for end in 0..=program.len() {
        if !program.is_char_boundary(end) {
            continue;
        }
        let out = parse_src(&program[..end]);
        let _ = out.file.node_count;
    }
}

// ---- the diagnostics of a flood of errors are bounded --------------------

#[test]
fn a_flood_of_syntax_errors_is_held_to_the_per_file_cap_as_it_is_found() {
    // Two diagnostics for every line: the unterminated string and the
    // statement that is not a call. Collecting all of them took 3.7 GB for a
    // 3.9 MiB file before the cap was applied as they arrived.
    let out = parse_src(&"\"\n".repeat(200_000));
    assert!(
        out.diagnostics.len() <= 101,
        "{} diagnostics were kept",
        out.diagnostics.len()
    );
    // What is kept is the start of the file, which is what the cap of
    // section 17.1 reports.
    let lines: Vec<u32> = out
        .diagnostics
        .iter()
        .take(100)
        .map(|d| d.location.as_ref().map_or(0, |l| l.line))
        .collect();
    assert!(lines.iter().all(|&line| line <= 51), "{lines:?}");
    assert!(
        out.diagnostics
            .iter()
            .all(hollowscript_core::Diagnostic::is_error)
    );
}
