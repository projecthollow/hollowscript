//! Hand-built HIR fixtures, one per construct the HIR defines, each compiled
//! and then run through `hollowscript_bytecode`'s own verifier.
//!
//! `hollowscript_check::check` does not exist yet (its name resolution and
//! inference passes are unwritten), so there is no way to get a checked
//! `Program` from source text. Every fixture here builds the typed tree by
//! hand instead, exactly the shape a real checker would hand the compiler.
//! `Expr::ty`/`Pattern::ty` are never read by the compiler (only checked
//! programs are compiled, and typing is the checker's job), so every fixture
//! uses one placeholder `TypeId` throughout rather than building a real type
//! for each expression.

use hollowscript_bytecode::opcode::{Instruction, Op, Wide};
use hollowscript_bytecode::verify;
use hollowscript_check::hir::{
    Arm, Block, Capture, Constant, Export as HirExport, Expr, ExprKind, Function, Global, GlobalId,
    LocalBinding, LocalId, Match, Module as HirModule, Path, PathRoot, Pattern, PatternKind,
    Program, Stmt, StmtKind,
};
use hollowscript_check::types::{DeclId, DeclShape, TypeInterner, Variant};
use hollowscript_core::Interner;
use hollowscript_core::native::ROSTER;
use hollowscript_syntax::ast::NodeId;

/// Assigns increasing `NodeId`s in construction order, standing in for the
/// parser's own monotonic counter.
struct Nodes(u32);
impl Nodes {
    fn new() -> Nodes {
        Nodes(0)
    }
    fn next(&mut self) -> NodeId {
        self.0 += 1;
        NodeId(self.0)
    }
}

fn expr(ty: hollowscript_check::types::TypeId, node: NodeId, kind: ExprKind) -> Expr {
    Expr { node, ty, kind }
}

fn stmt(node: NodeId, kind: StmtKind) -> Stmt {
    Stmt { node, kind }
}

fn pattern(ty: hollowscript_check::types::TypeId, node: NodeId, kind: PatternKind) -> Pattern {
    Pattern { node, ty, kind }
}

/// A one-module, zero-function program whose entry module's `init` block is
/// `stmts`. Good enough for every construct that does not itself need a
/// separate `fn` or closure (most of them: the compiler never treats module
/// init specially except for its `FunctionKind`).
fn program_with_init(types: TypeInterner, stmts: Vec<Stmt>) -> Program {
    Program {
        capability_types: Vec::new(),
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: Vec::new(),
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block { stmts },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions: Vec::new(),
    }
}

// Test-only helpers: `panic!`/`expect` here fail the calling `#[test]`, which
// is what they are for, but clippy's test allowances only look at the
// immediately enclosing function's own attribute, not its caller's.
#[allow(
    clippy::panic,
    reason = "a test helper failing the test that called it"
)]
fn compiles_and_verifies(program: &Program, symbols: &Interner) {
    let compiled =
        hollowscript_compiler::compile(program, symbols, hollowscript_compiler::Options::default())
            .unwrap_or_else(|e| panic!("compile failed: {}", e.into_diagnostic().message));
    verify(compiled).unwrap_or_else(|e| panic!("verification failed: {e}"));
}

#[allow(
    clippy::panic,
    clippy::expect_used,
    reason = "a test helper failing the test that called it"
)]
fn native_index(name: &str) -> u32 {
    u32::try_from(
        ROSTER
            .iter()
            .position(|n| n.name == name)
            .unwrap_or_else(|| panic!("no native named {name}")),
    )
    .expect("small index")
}

#[test]
fn literals_arithmetic_and_comparisons() {
    let types = TypeInterner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let int_lit = |v: i64, n: &mut Nodes| expr(t, n.next(), ExprKind::Literal(Constant::Int(v)));
    let a = int_lit(2, &mut n);
    let b = int_lit(3, &mut n);
    let sum = expr(
        t,
        n.next(),
        ExprKind::Binary {
            op: hollowscript_check::hir::BinaryOp::AddInt,
            lhs: Box::new(a),
            rhs: Box::new(b),
        },
    );
    let neg = expr(
        t,
        n.next(),
        ExprKind::Unary {
            op: hollowscript_check::hir::UnaryOp::NegInt,
            operand: Box::new(sum),
        },
    );
    let flt = expr(t, n.next(), ExprKind::Literal(Constant::Float(1.5)));
    let coerced = expr(
        t,
        n.next(),
        ExprKind::IntToFloat(Box::new(int_lit(4, &mut n))),
    );
    let add_float = expr(
        t,
        n.next(),
        ExprKind::Binary {
            op: hollowscript_check::hir::BinaryOp::AddFloat,
            lhs: Box::new(flt),
            rhs: Box::new(coerced),
        },
    );
    let cmp = expr(
        t,
        n.next(),
        ExprKind::Binary {
            op: hollowscript_check::hir::BinaryOp::Lt,
            lhs: Box::new(neg),
            rhs: Box::new(add_float),
        },
    );
    let not = expr(t, n.next(), ExprKind::Not(Box::new(cmp)));
    let text = expr(
        t,
        n.next(),
        ExprKind::Interpolate {
            text: vec!["value: ".to_owned(), String::new()],
            exprs: vec![expr(
                t,
                n.next(),
                ExprKind::Literal(Constant::String("x".to_owned())),
            )],
        },
    );
    let and_or = expr(
        t,
        n.next(),
        ExprKind::Logical {
            op: hollowscript_check::hir::LogicalOp::And,
            lhs: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Bool(true)))),
            rhs_facts: Vec::new(),
            rhs: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Bool(false)))),
        },
    );
    let program = program_with_init(
        types,
        vec![
            stmt(n.next(), StmtKind::Call(not)),
            stmt(n.next(), StmtKind::Call(text)),
            stmt(n.next(), StmtKind::Call(and_or)),
        ],
    );
    compiles_and_verifies(&program, &Interner::new());
}

#[test]
fn records_and_field_reads() {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    // Interned (and therefore built) in alphabetical order, which is what
    // both this fixture's field indices and the compiler's own re-sort rely
    // on; see gap 2 of `hollowscript_compiler`'s module documentation.
    let age = names.intern("age");
    let name = names.intern("name");
    let record = expr(
        t,
        n.next(),
        ExprKind::Record(vec![
            (
                name,
                expr(
                    t,
                    n.next(),
                    ExprKind::Literal(Constant::String("Ada".to_owned())),
                ),
            ),
            (age, expr(t, n.next(), ExprKind::Literal(Constant::Int(36)))),
        ]),
    );
    let local0 = LocalId(0);
    let let_stmt = stmt(
        n.next(),
        StmtKind::Let {
            bindings: vec![LocalBinding {
                node: n.next(),
                local: local0,
                name,
                ty: t,
            }],
            pattern: None,
            init: record,
        },
    );
    // `age` is alphabetical rank 0, `name` is rank 1.
    let read_age = expr(
        t,
        n.next(),
        ExprKind::Field {
            target: Box::new(expr(t, n.next(), ExprKind::Local(local0))),
            index: 0,
        },
    );
    let read_name = expr(
        t,
        n.next(),
        ExprKind::Field {
            target: Box::new(expr(t, n.next(), ExprKind::Local(local0))),
            index: 1,
        },
    );
    let program = program_with_init(
        types,
        vec![
            let_stmt,
            stmt(n.next(), StmtKind::Call(read_age)),
            stmt(n.next(), StmtKind::Call(read_name)),
        ],
    );
    compiles_and_verifies(&program, &names);
}

#[test]
fn enums_construct_and_match() {
    let mut types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let decl = DeclId {
        module: 0,
        index: 0,
    };
    let none_name = names.intern("None");
    let some_name = names.intern("Some");
    let option_name = names.intern("Option");
    types.declare(
        decl,
        option_name,
        DeclShape::Enum {
            params: Vec::new(),
            variants: vec![
                Variant {
                    name: none_name,
                    payload: Vec::new(),
                },
                Variant {
                    name: some_name,
                    payload: vec![t],
                },
            ],
        },
    );
    let constructed = expr(
        t,
        n.next(),
        ExprKind::VariantConstruct {
            decl,
            index: 1,
            args: vec![expr(t, n.next(), ExprKind::Literal(Constant::Int(7)))],
        },
    );
    let local0 = LocalId(0);
    let scrut_name = names.intern("scrutinee");
    let let_stmt = stmt(
        n.next(),
        StmtKind::Let {
            bindings: vec![LocalBinding {
                node: n.next(),
                local: local0,
                name: scrut_name,
                ty: t,
            }],
            pattern: None,
            init: constructed,
        },
    );
    let payload_local = LocalId(1);
    let match_stmt = stmt(
        n.next(),
        StmtKind::Match(Match {
            scrutinee: expr(t, n.next(), ExprKind::Local(local0)),
            scrutinee_path: None,
            arms: vec![
                Arm {
                    patterns: vec![pattern(
                        t,
                        n.next(),
                        PatternKind::Variant {
                            index: 1,
                            args: vec![pattern(
                                t,
                                n.next(),
                                PatternKind::Binding(Some(payload_local)),
                            )],
                        },
                    )],
                    bindings: vec![LocalBinding {
                        node: n.next(),
                        local: payload_local,
                        name: names.intern("value"),
                        ty: t,
                    }],
                    body: Block {
                        stmts: vec![stmt(
                            n.next(),
                            StmtKind::Call(expr(t, n.next(), ExprKind::Local(payload_local))),
                        )],
                    },
                },
                Arm {
                    patterns: vec![pattern(
                        t,
                        n.next(),
                        PatternKind::Variant {
                            index: 0,
                            args: Vec::new(),
                        },
                    )],
                    bindings: Vec::new(),
                    body: Block { stmts: Vec::new() },
                },
            ],
            exhaustive_without_else: true,
        }),
    );
    // A bare, zero-arity variant reference used as a value (`VariantFn`).
    let bare = expr(t, n.next(), ExprKind::VariantValue { decl, index: 0 });
    let program = program_with_init(
        types,
        vec![let_stmt, match_stmt, stmt(n.next(), StmtKind::Call(bare))],
    );
    compiles_and_verifies(&program, &names);
}

/// Defence in depth for section 7.7: even a `match` statement the checker
/// (wrongly) called exhaustive must still trap if a value reaches it that
/// matches none of its arms, rather than falling through to whatever
/// bytecode happens to follow. This fixture builds exactly that: one
/// variant of a two-variant enum, marked `exhaustive_without_else: true`
/// though only the first variant has an arm, standing in for a checker
/// defect the compiler cannot see through. `compile` must still emit a
/// `Trap` (HS1099) the second variant's failed pattern test can reach.
#[test]
fn a_match_statement_the_checker_wrongly_called_exhaustive_still_traps() {
    let mut types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let decl = DeclId {
        module: 0,
        index: 0,
    };
    let red_name = names.intern("Red");
    let green_name = names.intern("Green");
    let light_name = names.intern("Light");
    types.declare(
        decl,
        light_name,
        DeclShape::Enum {
            params: Vec::new(),
            variants: vec![
                Variant {
                    name: red_name,
                    payload: Vec::new(),
                },
                Variant {
                    name: green_name,
                    payload: Vec::new(),
                },
            ],
        },
    );
    let constructed = expr(
        t,
        n.next(),
        ExprKind::VariantConstruct {
            decl,
            index: 0,
            args: Vec::new(),
        },
    );
    let local0 = LocalId(0);
    let let_stmt = stmt(
        n.next(),
        StmtKind::Let {
            bindings: vec![LocalBinding {
                node: n.next(),
                local: local0,
                name: names.intern("scrutinee"),
                ty: t,
            }],
            pattern: None,
            init: constructed,
        },
    );
    let match_stmt = stmt(
        n.next(),
        StmtKind::Match(Match {
            scrutinee: expr(t, n.next(), ExprKind::Local(local0)),
            scrutinee_path: None,
            // Only `Red` has an arm; `Green` has none. A correct checker
            // would never set `exhaustive_without_else` here, which is
            // exactly why the compiler must not trust it blindly.
            arms: vec![Arm {
                patterns: vec![pattern(
                    t,
                    n.next(),
                    PatternKind::Variant {
                        index: 0,
                        args: Vec::new(),
                    },
                )],
                bindings: Vec::new(),
                body: Block { stmts: Vec::new() },
            }],
            exhaustive_without_else: true,
        }),
    );
    let program = program_with_init(types, vec![let_stmt, match_stmt]);
    let compiled =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default())
            .unwrap_or_else(|e| panic!("compile failed: {}", e.into_diagnostic().message));
    let entry = &compiled.modules[compiled.modules.len() - 1];
    let function = &compiled.functions[entry.init as usize];
    let has_trap = function
        .code
        .iter()
        .any(|&word| Instruction::decode(word).is_ok_and(|instr| instr.op == Op::Trap));
    assert!(
        has_trap,
        "a match statement with a refutable last arm must compile a trap for the case nothing matched"
    );
    // The module as a whole must still verify: a reachable `Trap` is not a
    // defect the verifier rejects, only ordinary bytecode a correct program
    // never executes (see `Op::Trap`'s own documentation).
    verify(compiled).unwrap_or_else(|e| panic!("verification failed: {e}"));
}

#[test]
fn maps_sets_lists_and_index() {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let list = expr(
        t,
        n.next(),
        ExprKind::List(vec![
            expr(t, n.next(), ExprKind::Literal(Constant::Int(1))),
            expr(t, n.next(), ExprKind::Literal(Constant::Int(2))),
            expr(t, n.next(), ExprKind::Literal(Constant::Int(3))),
        ]),
    );
    let map = expr(
        t,
        n.next(),
        ExprKind::Map(vec![(
            expr(
                t,
                n.next(),
                ExprKind::Literal(Constant::String("k".to_owned())),
            ),
            expr(t, n.next(), ExprKind::Literal(Constant::Int(9))),
        )]),
    );
    // A set literal is a native call to `set` over a list, as the roster
    // documents (`success`/`failure`/`pi` are the only surface forms the
    // compiler itself builds; every other library form, including this one,
    // is `ExprKind::NativeCall`).
    let set = expr(
        t,
        n.next(),
        ExprKind::NativeCall {
            native: native_index("set"),
            args: vec![expr(
                t,
                n.next(),
                ExprKind::List(vec![expr(t, n.next(), ExprKind::Literal(Constant::Int(1)))]),
            )],
        },
    );
    let local_list = LocalId(0);
    let index_expr = expr(
        t,
        n.next(),
        ExprKind::Index {
            target: Box::new(expr(t, n.next(), ExprKind::Local(local_list))),
            index: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Int(0)))),
        },
    );
    let program = program_with_init(
        types,
        vec![
            stmt(
                n.next(),
                StmtKind::Let {
                    bindings: vec![LocalBinding {
                        node: n.next(),
                        local: local_list,
                        name: names.intern("xs"),
                        ty: t,
                    }],
                    pattern: None,
                    init: list,
                },
            ),
            stmt(n.next(), StmtKind::Call(map)),
            stmt(n.next(), StmtKind::Call(set)),
            stmt(n.next(), StmtKind::Call(index_expr)),
        ],
    );
    compiles_and_verifies(&program, &names);
}

#[test]
fn list_patterns_with_and_without_rest() {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let head = LocalId(0);
    let tail = LocalId(1);
    let list = expr(
        t,
        n.next(),
        ExprKind::List(vec![
            expr(t, n.next(), ExprKind::Literal(Constant::Int(1))),
            expr(t, n.next(), ExprKind::Literal(Constant::Int(2))),
        ]),
    );
    let match_stmt = stmt(
        n.next(),
        StmtKind::Match(Match {
            scrutinee: list,
            scrutinee_path: None,
            arms: vec![
                Arm {
                    patterns: vec![pattern(
                        t,
                        n.next(),
                        PatternKind::List {
                            elements: vec![pattern(t, n.next(), PatternKind::Binding(Some(head)))],
                            rest: Some(Some(tail)),
                        },
                    )],
                    bindings: vec![
                        LocalBinding {
                            node: n.next(),
                            local: head,
                            name: names.intern("head"),
                            ty: t,
                        },
                        LocalBinding {
                            node: n.next(),
                            local: tail,
                            name: names.intern("tail"),
                            ty: t,
                        },
                    ],
                    body: Block {
                        stmts: vec![stmt(
                            n.next(),
                            StmtKind::Call(expr(t, n.next(), ExprKind::Local(tail))),
                        )],
                    },
                },
                Arm {
                    patterns: vec![pattern(
                        t,
                        n.next(),
                        PatternKind::List {
                            elements: Vec::new(),
                            rest: None,
                        },
                    )],
                    bindings: Vec::new(),
                    body: Block { stmts: Vec::new() },
                },
            ],
            exhaustive_without_else: true,
        }),
    );
    let program = program_with_init(types, vec![match_stmt]);
    compiles_and_verifies(&program, &names);
}

#[test]
fn closures_capture_locals_and_are_called() {
    // fn main() { let x = 1; let f = || x; x = 2; f(); }
    // The closure captures `x` by value (`Capture::Local`), so `x` must live
    // in a cell from its declaration onward.
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let x = LocalId(0);
    let f = LocalId(1);

    let closure_body = Block {
        stmts: vec![stmt(
            n.next(),
            StmtKind::Return(Some(expr(t, n.next(), ExprKind::Upvalue(0)))),
        )],
    };
    let closure_fn = Function {
        node: n.next(),
        name: names.intern("<closure>"),
        params: Vec::new(),
        ret: Some(t),
        local_count: 0,
        captures: vec![Capture::Local(x)],
        body: closure_body,
    };
    let functions = vec![closure_fn];
    let closure_fid = hollowscript_check::hir::FunctionId(0);

    let stmts = vec![
        stmt(
            n.next(),
            StmtKind::Let {
                bindings: vec![LocalBinding {
                    node: n.next(),
                    local: x,
                    name: names.intern("x"),
                    ty: t,
                }],
                pattern: None,
                init: expr(t, n.next(), ExprKind::Literal(Constant::Int(1))),
            },
        ),
        stmt(
            n.next(),
            StmtKind::Let {
                bindings: vec![LocalBinding {
                    node: n.next(),
                    local: f,
                    name: names.intern("f"),
                    ty: t,
                }],
                pattern: None,
                init: expr(t, n.next(), ExprKind::Closure(closure_fid)),
            },
        ),
        stmt(
            n.next(),
            StmtKind::Assign {
                local: x,
                value: expr(t, n.next(), ExprKind::Literal(Constant::Int(2))),
            },
        ),
        stmt(
            n.next(),
            StmtKind::Call(expr(
                t,
                n.next(),
                ExprKind::Call {
                    callee: Box::new(expr(t, n.next(), ExprKind::Local(f))),
                    args: Vec::new(),
                },
            )),
        ),
    ];
    let program = Program {
        capability_types: Vec::new(),
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: Vec::new(),
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block { stmts },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions,
    };
    compiles_and_verifies(&program, &names);
}

#[test]
fn nested_closures_forward_upvalues() {
    // fn outer() { let x = 1; return || || x; }
    // The inner closure captures `x` as an upvalue of the outer closure
    // (`Capture::Upvalue(0)`), forwarded rather than reaching past it.
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let x = LocalId(0);

    let innermost = Function {
        node: n.next(),
        name: names.intern("<closure>"),
        params: Vec::new(),
        ret: Some(t),
        local_count: 0,
        captures: vec![Capture::Upvalue(0)],
        body: Block {
            stmts: vec![stmt(
                n.next(),
                StmtKind::Return(Some(expr(t, n.next(), ExprKind::Upvalue(0)))),
            )],
        },
    };
    let innermost_fid = hollowscript_check::hir::FunctionId(0);
    let middle = Function {
        node: n.next(),
        name: names.intern("<closure>"),
        params: Vec::new(),
        ret: Some(t),
        local_count: 0,
        captures: vec![Capture::Local(x)],
        body: Block {
            stmts: vec![stmt(
                n.next(),
                StmtKind::Return(Some(expr(t, n.next(), ExprKind::Closure(innermost_fid)))),
            )],
        },
    };
    let middle_fid = hollowscript_check::hir::FunctionId(1);
    let outer = Function {
        node: n.next(),
        name: names.intern("outer"),
        params: Vec::new(),
        ret: Some(t),
        local_count: 1,
        captures: Vec::new(),
        body: Block {
            stmts: vec![
                stmt(
                    n.next(),
                    StmtKind::Let {
                        bindings: vec![LocalBinding {
                            node: n.next(),
                            local: x,
                            name: names.intern("x"),
                            ty: t,
                        }],
                        pattern: None,
                        init: expr(t, n.next(), ExprKind::Literal(Constant::Int(1))),
                    },
                ),
                stmt(
                    n.next(),
                    StmtKind::Return(Some(expr(t, n.next(), ExprKind::Closure(middle_fid)))),
                ),
            ],
        },
    };
    let outer_fid = hollowscript_check::hir::FunctionId(2);
    let program = Program {
        capability_types: Vec::new(),
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: Vec::new(),
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block {
                stmts: vec![stmt(
                    n.next(),
                    StmtKind::Call(expr(
                        t,
                        n.next(),
                        ExprKind::Call {
                            callee: Box::new(expr(t, n.next(), ExprKind::FunctionValue(outer_fid))),
                            args: Vec::new(),
                        },
                    )),
                )],
            },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions: vec![
            innermost,
            middle,
            Function {
                local_count: 1,
                ..outer
            },
        ],
    };
    compiles_and_verifies(&program, &names);
}

#[test]
fn the_try_operator_returns_early() {
    // fn safe_div(a: Int): Result<Int, String> { let v = risky()?; return v; }
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let risky_fid = hollowscript_check::hir::FunctionId(0);
    let risky = Function {
        node: n.next(),
        name: names.intern("risky"),
        params: Vec::new(),
        ret: Some(t),
        local_count: 0,
        captures: Vec::new(),
        body: Block {
            stmts: vec![stmt(
                n.next(),
                StmtKind::Return(Some(expr(t, n.next(), ExprKind::Literal(Constant::Int(1))))),
            )],
        },
    };
    let value_local = LocalId(0);
    let caller = Function {
        node: n.next(),
        name: names.intern("safe"),
        params: Vec::new(),
        ret: Some(t),
        local_count: 1,
        captures: Vec::new(),
        body: Block {
            stmts: vec![
                stmt(
                    n.next(),
                    StmtKind::Let {
                        bindings: vec![LocalBinding {
                            node: n.next(),
                            local: value_local,
                            name: names.intern("v"),
                            ty: t,
                        }],
                        pattern: None,
                        init: expr(
                            t,
                            n.next(),
                            ExprKind::Try {
                                operand: Box::new(expr(
                                    t,
                                    n.next(),
                                    ExprKind::Call {
                                        callee: Box::new(expr(
                                            t,
                                            n.next(),
                                            ExprKind::FunctionValue(risky_fid),
                                        )),
                                        args: Vec::new(),
                                    },
                                )),
                                err_ty: t,
                            },
                        ),
                    },
                ),
                stmt(
                    n.next(),
                    StmtKind::Return(Some(expr(t, n.next(), ExprKind::Local(value_local)))),
                ),
            ],
        },
    };
    let caller_fid = hollowscript_check::hir::FunctionId(1);
    let program = Program {
        capability_types: Vec::new(),
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: Vec::new(),
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block {
                stmts: vec![stmt(
                    n.next(),
                    StmtKind::Call(expr(
                        t,
                        n.next(),
                        ExprKind::Call {
                            callee: Box::new(expr(
                                t,
                                n.next(),
                                ExprKind::FunctionValue(caller_fid),
                            )),
                            args: Vec::new(),
                        },
                    )),
                )],
            },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions: vec![risky, caller],
    };
    compiles_and_verifies(&program, &names);
}

#[test]
fn effects_and_capability_calls() {
    let mut types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let print_name = names.intern("print");
    let cap_id = hollowscript_check::hir::CapabilityId(0);
    let effect_call = expr(
        t,
        n.next(),
        ExprKind::EffectCall {
            callee: Box::new(expr(t, n.next(), ExprKind::Capability(cap_id))),
            args: vec![expr(
                t,
                n.next(),
                ExprKind::Literal(Constant::String("hi".to_owned())),
            )],
        },
    );
    // `print(text: String)`, the declared interface the module carries.
    let print_type = types.function(true, vec![types.string()], None);
    let program = Program {
        capability_types: vec![print_type],
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: vec![(print_name, cap_id)],
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block {
                stmts: vec![stmt(n.next(), StmtKind::Call(effect_call))],
            },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions: Vec::new(),
    };
    compiles_and_verifies(&program, &names);
}

/// A capability used as a value rather than called compiles to `Op::CapFn`,
/// whose operand is a *capability call* index. A value has no call site to
/// take an argument count from, so the entry is built from the declared
/// interface the module carries, which is what makes the index mean the
/// same thing as it does for an immediate `effect` call.
#[test]
fn a_capability_used_as_a_value_compiles_to_a_capability_function() {
    let mut types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let print_name = names.intern("print");
    let cap_id = hollowscript_check::hir::CapabilityId(0);
    let as_value = expr(t, n.next(), ExprKind::Capability(cap_id));
    let print_type = types.function(true, vec![types.string()], None);
    let program = Program {
        capability_types: vec![print_type],
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: vec![(print_name, cap_id)],
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block {
                stmts: vec![stmt(n.next(), StmtKind::Call(as_value))],
            },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions: Vec::new(),
    };
    let module =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default())
            .expect("a capability as a value compiles");
    assert_eq!(module.cap_calls.len(), 1);
    let init = module.functions.first().expect("the module initialiser");
    let ops: Vec<Op> = init
        .code
        .iter()
        .filter_map(|word| Instruction::decode(*word).ok())
        .map(|i| i.op)
        .collect();
    assert!(ops.contains(&Op::CapFn), "{ops:?}");
    verify(module).expect("the module verifies");
}

/// A member of a namespace capability (`clock.now`) compiles to the same
/// `Op::CallCap` a single-function capability does, with the member's own
/// position in the module's namespace table.
#[test]
fn a_namespace_capability_member_compiles_to_a_capability_call() {
    let mut types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let clock = names.intern("clock");
    let now = names.intern("now");
    let today = names.intern("today");
    let cap_id = hollowscript_check::hir::CapabilityId(0);
    let mut member_call = |name, index| {
        let member = expr(
            t,
            n.next(),
            ExprKind::CapabilityMember {
                capability: cap_id,
                name,
                index,
            },
        );
        expr(
            t,
            n.next(),
            ExprKind::EffectCall {
                callee: Box::new(member),
                args: Vec::new(),
            },
        )
    };
    let now_call = member_call(now, 0);
    let today_call = member_call(today, 1);
    let now_again = member_call(now, 0);
    // `clock` is a namespace: a record of two effectful functions, each
    // taking nothing and answering a `String`.
    let answers_text = types.function(true, Vec::new(), Some(types.string()));
    let clock_type = types.record(
        &names,
        vec![
            hollowscript_check::types::Field {
                name: now,
                ty: answers_text,
            },
            hollowscript_check::types::Field {
                name: today,
                ty: answers_text,
            },
        ],
    );
    let program = Program {
        capability_types: vec![clock_type],
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: vec![(clock, cap_id)],
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block {
                stmts: vec![
                    stmt(n.next(), StmtKind::Call(now_call)),
                    stmt(n.next(), StmtKind::Call(today_call)),
                    stmt(n.next(), StmtKind::Call(now_again)),
                ],
            },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions: Vec::new(),
    };
    let module =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default())
            .expect("a namespace capability call compiles");
    // One capability, two members, two call entries: calling `now` twice
    // shares one entry.
    assert_eq!(module.capabilities.len(), 1);
    let hollowscript_bytecode::module::CapabilityKind::Namespace(functions) =
        &module.capabilities.first().expect("one capability").kind
    else {
        panic!("a capability with members is a namespace")
    };
    let member_names: Vec<&str> = functions.iter().map(|(n, _)| module.string(*n)).collect();
    assert_eq!(member_names, ["now", "today"]);
    assert_eq!(module.cap_calls.len(), 2);
    assert_eq!(
        module
            .cap_calls
            .iter()
            .map(|c| c.function)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    verify(module).expect("and it verifies");
}

#[test]
fn for_loops_over_lists_with_break_and_continue() {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let element = LocalId(0);
    let list = expr(
        t,
        n.next(),
        ExprKind::List(vec![
            expr(t, n.next(), ExprKind::Literal(Constant::Int(1))),
            expr(t, n.next(), ExprKind::Literal(Constant::Int(2))),
            expr(t, n.next(), ExprKind::Literal(Constant::Int(3))),
        ]),
    );
    let body = Block {
        stmts: vec![
            stmt(
                n.next(),
                StmtKind::If {
                    condition: expr(
                        t,
                        n.next(),
                        ExprKind::Binary {
                            op: hollowscript_check::hir::BinaryOp::Eq {
                                scalar_operands: true,
                            },
                            lhs: Box::new(expr(t, n.next(), ExprKind::Local(element))),
                            rhs: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Int(2)))),
                        },
                    ),
                    then_facts: Vec::new(),
                    then: Block {
                        stmts: vec![stmt(n.next(), StmtKind::Continue { depth: 0 })],
                    },
                    else_facts: Vec::new(),
                    otherwise: None,
                },
            ),
            stmt(
                n.next(),
                StmtKind::If {
                    condition: expr(
                        t,
                        n.next(),
                        ExprKind::Binary {
                            op: hollowscript_check::hir::BinaryOp::Eq {
                                scalar_operands: true,
                            },
                            lhs: Box::new(expr(t, n.next(), ExprKind::Local(element))),
                            rhs: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Int(3)))),
                        },
                    ),
                    then_facts: Vec::new(),
                    then: Block {
                        stmts: vec![stmt(n.next(), StmtKind::Break { depth: 0 })],
                    },
                    else_facts: Vec::new(),
                    otherwise: Some(Block { stmts: Vec::new() }),
                },
            ),
        ],
    };
    let for_stmt = stmt(
        n.next(),
        StmtKind::For {
            bindings: vec![LocalBinding {
                node: n.next(),
                local: element,
                name: names.intern("e"),
                ty: t,
            }],
            pattern: None,
            element_ty: t,
            iterable: list,
            body,
        },
    );

    let counter = LocalId(1);
    let while_stmt = stmt(
        n.next(),
        StmtKind::While {
            condition: expr(
                t,
                n.next(),
                ExprKind::Binary {
                    op: hollowscript_check::hir::BinaryOp::Lt,
                    lhs: Box::new(expr(t, n.next(), ExprKind::Local(counter))),
                    rhs: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Int(3)))),
                },
            ),
            loop_facts: Vec::new(),
            body: Block {
                stmts: vec![stmt(
                    n.next(),
                    StmtKind::Assign {
                        local: counter,
                        value: expr(
                            t,
                            n.next(),
                            ExprKind::Binary {
                                op: hollowscript_check::hir::BinaryOp::AddInt,
                                lhs: Box::new(expr(t, n.next(), ExprKind::Local(counter))),
                                rhs: Box::new(expr(
                                    t,
                                    n.next(),
                                    ExprKind::Literal(Constant::Int(1)),
                                )),
                            },
                        ),
                    },
                )],
            },
        },
    );
    let program = program_with_init(
        types,
        vec![
            stmt(
                n.next(),
                StmtKind::Let {
                    bindings: vec![LocalBinding {
                        node: n.next(),
                        local: counter,
                        name: names.intern("i"),
                        ty: t,
                    }],
                    pattern: None,
                    init: expr(t, n.next(), ExprKind::Literal(Constant::Int(0))),
                },
            ),
            for_stmt,
            while_stmt,
        ],
    );
    compiles_and_verifies(&program, &names);
}

#[test]
fn modules_with_globals_in_initialisation_order() {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let base_name = names.intern("base");
    let base_id = GlobalId {
        module: 0,
        index: 0,
    };
    let base_module = HirModule {
        path: "base.hws".to_owned(),
        capabilities: Vec::new(),
        globals: vec![Global {
            id: base_id,
            node: n.next(),
            name: base_name,
            mutable: false,
            shared_mutable: false,
            ty: t,
            init: Some(expr(t, n.next(), ExprKind::Literal(Constant::Int(10)))),
        }],
        function_ids: Vec::new(),
        init: Block { stmts: Vec::new() },
        exports: vec![(base_name, HirExport::Constant(base_id))],
        node_spans: std::collections::BTreeMap::new(),
    };

    let derived_id = GlobalId {
        module: 1,
        index: 0,
    };
    let derived_name = names.intern("derived");
    let entry_module = HirModule {
        path: "main.hws".to_owned(),
        capabilities: Vec::new(),
        globals: vec![Global {
            id: derived_id,
            node: n.next(),
            name: derived_name,
            mutable: true,
            shared_mutable: false,
            ty: t,
            init: Some(expr(
                t,
                n.next(),
                ExprKind::Binary {
                    op: hollowscript_check::hir::BinaryOp::AddInt,
                    lhs: Box::new(expr(t, n.next(), ExprKind::Global(base_id))),
                    rhs: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Int(1)))),
                },
            )),
        }],
        function_ids: Vec::new(),
        init: Block {
            stmts: vec![stmt(
                n.next(),
                StmtKind::Call(expr(t, n.next(), ExprKind::Global(derived_id))),
            )],
        },
        exports: Vec::new(),
        node_spans: std::collections::BTreeMap::new(),
    };

    let program = Program {
        capability_types: Vec::new(),
        modules: vec![base_module, entry_module],
        types,
        functions: Vec::new(),
    };
    compiles_and_verifies(&program, &names);
}

#[test]
fn a_narrowed_path_with_no_fields_compiles() {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let x = LocalId(0);
    let program = program_with_init(
        types,
        vec![
            stmt(
                n.next(),
                StmtKind::Let {
                    bindings: vec![LocalBinding {
                        node: n.next(),
                        local: x,
                        name: names.intern("x"),
                        ty: t,
                    }],
                    pattern: None,
                    init: expr(t, n.next(), ExprKind::Literal(Constant::Int(5))),
                },
            ),
            stmt(
                n.next(),
                StmtKind::Call(expr(
                    t,
                    n.next(),
                    ExprKind::NarrowedPath(Path {
                        root: PathRoot::Local(x),
                        fields: Vec::new(),
                    }),
                )),
            ),
        ],
    );
    compiles_and_verifies(&program, &names);
}

/// A narrowed path with field accesses compiles to the root read followed by
/// one `GetField` per field, at the position `hir::PathField` carries. It was
/// once a documented gap in this compiler, because `hir::Path` named its
/// fields without their positions.
#[test]
fn a_narrowed_path_with_fields_compiles_to_field_reads() {
    let types = TypeInterner::new();
    let names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let x = LocalId(0);
    let field_name = {
        let mut names = names;
        let s = names.intern("owner");
        (names, s)
    };
    let (names, owner) = field_name;
    let program = program_with_init(
        types,
        vec![stmt(
            n.next(),
            StmtKind::Call(expr(
                t,
                n.next(),
                ExprKind::NarrowedPath(Path {
                    root: PathRoot::Local(x),
                    fields: vec![hollowscript_check::hir::PathField {
                        name: owner,
                        index: 0,
                    }],
                }),
            )),
        )],
    );
    let module =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default())
            .expect("a narrowed path with fields compiles");
    let init = module
        .functions
        .first()
        .expect("the initialiser is the first function");
    assert!(
        init.code
            .iter()
            .filter_map(|&w| Instruction::decode(w).ok())
            .any(|i| i.op == Op::GetField),
        "the field access lowers to a GetField"
    );
    verify(module).expect("and the result verifies");
}

#[test]
fn never_panics_on_a_battery_of_small_well_typed_programs() {
    // A stress pass over a grab-bag of the fixtures above, compiled in a
    // fresh interner each time, checking only that compilation and
    // verification never panic and always agree with each other.
    for _ in 0..3 {
        literals_arithmetic_and_comparisons();
        records_and_field_reads();
        enums_construct_and_match();
        maps_sets_lists_and_index();
        list_patterns_with_and_without_rest();
        closures_capture_locals_and_are_called();
        nested_closures_forward_upvalues();
        the_try_operator_returns_early();
        effects_and_capability_calls();
        a_namespace_capability_member_compiles_to_a_capability_call();
        for_loops_over_lists_with_break_and_continue();
        modules_with_globals_in_initialisation_order();
    }
}

#[test]
#[allow(
    clippy::mem_forget,
    reason = "deliberately leaked: see the comment beside the forget() call below"
)]
fn a_deeply_nested_expression_errors_instead_of_overflowing_the_stack() {
    // 1 + (1 + (1 + ... )), 200,000 levels deep: the exact adversarial input
    // that used to abort the whole process with a stack overflow instead of
    // returning a `CompileError` (spec section 19.1: "never crashes, hangs
    // or exhausts its host on any input").
    let types = TypeInterner::new();
    let names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let mut chain = expr(t, n.next(), ExprKind::Literal(Constant::Int(1)));
    for _ in 0..200_000 {
        chain = expr(
            t,
            n.next(),
            ExprKind::Binary {
                op: hollowscript_check::hir::BinaryOp::AddInt,
                lhs: Box::new(expr(t, n.next(), ExprKind::Literal(Constant::Int(1)))),
                rhs: Box::new(chain),
            },
        );
    }
    let program = program_with_init(types, vec![stmt(n.next(), StmtKind::Call(chain))]);
    let result =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default());
    assert!(
        result.is_err(),
        "200,000 levels of nested Binary must be rejected with a CompileError, not crash the process"
    );
    // `Expr`'s nested `Box`es are ordinary owned Rust values, so dropping a
    // 200,000-deep chain of them recurses one stack frame per level the same
    // way compiling one used to; that is a property of this test's own
    // fixture, not of the compiler, so the fixture is leaked rather than
    // dropped (the process exits shortly after this test regardless).
    std::mem::forget(program);
}

/// A program with one function that declares `local_count` locals and whose
/// body only returns.
fn program_declaring_locals(local_count: u32) -> (Program, Interner) {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let node = n.next();
    let function = Function {
        node,
        name: names.intern("many_locals"),
        params: Vec::new(),
        ret: None,
        // Declared, but never referenced by any instruction: only the
        // upfront check catches a count too large, since no register
        // allocation for an unused local would ever fire on its own.
        local_count,
        captures: Vec::new(),
        body: Block {
            stmts: vec![stmt(n.next(), StmtKind::Return(None))],
        },
    };
    let mut node_spans = std::collections::BTreeMap::new();
    node_spans.insert(
        node,
        hollowscript_check::spans::NodeSpan {
            start: 0,
            end: 2,
            line: 1,
            column: 1,
        },
    );
    let program = Program {
        capability_types: Vec::new(),
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: Vec::new(),
            globals: Vec::new(),
            function_ids: vec![hollowscript_check::hir::FunctionId(0)],
            init: Block { stmts: Vec::new() },
            exports: Vec::new(),
            node_spans,
        }],
        types,
        functions: vec![function],
    };
    (program, names)
}

#[test]
fn a_function_declaring_more_than_256_locals_compiles_to_a_wider_frame() {
    for locals in [256, 257, 300, 65_535] {
        let (program, names) = program_declaring_locals(locals);
        let module = hollowscript_compiler::compile(
            &program,
            &names,
            hollowscript_compiler::Options::default(),
        )
        .expect("a function of up to 65,535 locals compiles");
        assert_eq!(u32::from(module.functions[0].register_count), locals);
        verify(module).expect("and verifies");
    }
}

#[test]
fn a_function_declaring_more_locals_than_a_frame_holds_is_a_located_error() {
    let (program, names) = program_declaring_locals(65_536);
    let diagnostic =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default())
            .expect_err("a declared local count above 65,535 must be rejected")
            .into_diagnostic();
    assert_eq!(diagnostic.code, hollowscript_core::Code::HS0901);
    let location = diagnostic
        .location
        .expect("the error is located at the function");
    assert_eq!((&*location.file, location.line), ("main.hws", 1));
}

#[test]
fn a_capability_called_with_two_different_argument_counts_is_rejected() {
    let types = TypeInterner::new();
    let mut names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    let cap_name = names.intern("print");
    let cap_id = hollowscript_check::hir::CapabilityId(0);
    let one_arg_call = expr(
        t,
        n.next(),
        ExprKind::EffectCall {
            callee: Box::new(expr(t, n.next(), ExprKind::Capability(cap_id))),
            args: vec![expr(t, n.next(), ExprKind::Literal(Constant::Int(1)))],
        },
    );
    let three_arg_call = expr(
        t,
        n.next(),
        ExprKind::EffectCall {
            callee: Box::new(expr(t, n.next(), ExprKind::Capability(cap_id))),
            args: vec![
                expr(t, n.next(), ExprKind::Literal(Constant::Int(1))),
                expr(t, n.next(), ExprKind::Literal(Constant::Int(2))),
                expr(t, n.next(), ExprKind::Literal(Constant::Int(3))),
            ],
        },
    );
    let program = Program {
        capability_types: Vec::new(),
        modules: vec![HirModule {
            path: "main.hws".to_owned(),
            capabilities: vec![(cap_name, cap_id)],
            globals: Vec::new(),
            function_ids: Vec::new(),
            init: Block {
                stmts: vec![
                    stmt(n.next(), StmtKind::Call(one_arg_call)),
                    stmt(n.next(), StmtKind::Call(three_arg_call)),
                ],
            },
            exports: Vec::new(),
            node_spans: std::collections::BTreeMap::new(),
        }],
        types,
        functions: Vec::new(),
    };
    let result =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default());
    assert!(
        result.is_err(),
        "two call sites of the same capability with different argument counts must be \
         rejected, not silently widened into one shared signature that leaves the smaller \
         call's extra argument registers unpopulated"
    );
}

#[test]
fn more_than_65536_distinct_int_constants_in_one_function_are_each_their_own_slot() {
    let types = TypeInterner::new();
    let names = Interner::new();
    let mut n = Nodes::new();
    let t = types.int();
    // Two more than a 16 bit operand indexes (0..=65535), all outside i16
    // range so every one goes through the constant pool rather than
    // `LoadInt`'s inline immediate. The last two need a prefix to name their
    // slot, and must not alias an earlier value's.
    let stmts: Vec<Stmt> = (0..65_537i64)
        .map(|i| {
            stmt(
                n.next(),
                StmtKind::Call(expr(
                    t,
                    n.next(),
                    ExprKind::Literal(Constant::Int(100_000 + i)),
                )),
            )
        })
        .collect();
    let program = program_with_init(types, stmts);
    let module =
        hollowscript_compiler::compile(&program, &names, hollowscript_compiler::Options::default())
            .expect("a function may hold more constants than a 16 bit operand indexes");
    let init = &module.functions[0];
    assert_eq!(init.constants.len(), 65_537);
    let last_load = init
        .code
        .windows(2)
        .filter_map(|pair| {
            let ext = Instruction::decode(pair[0]).ok()?;
            let main = Instruction::decode(pair[1]).ok()?;
            (ext.op == Op::Ext && main.op == Op::LoadK).then(|| Wide::new(Some(ext), main).bx())
        })
        .max();
    assert_eq!(last_load, Some(65_536));
    assert_eq!(
        init.constants[65_536],
        hollowscript_bytecode::module::Constant::Int(100_000 + 65_536)
    );
    verify(module).expect("and verifies");
}
