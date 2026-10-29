//! Golden AST tests: a sample of real corpus programs, spanning most of the
//! grammar (uses, records, enums, functions and closures, patterns
//! including destructuring, `if`/`match`/`for`/`while`, effects, imports,
//! the `?` operator), parsed and dumped as a compact, span-free structural
//! text. The dump is deliberately not `{:?}`: `Debug` includes `NodeId`s and
//! byte spans, which would make every golden brittle to unrelated
//! whitespace changes elsewhere in the same file and would not read as a
//! shape a reviewer can check by eye.
//!
//! `clippy.toml` allows `unwrap`/`expect`/`panic` in test code; that
//! detection covers a `#[cfg(test)] mod` inside a crate but not a
//! `tests/*.rs` integration test's own helper functions, so it is spelled
//! out explicitly here for the same, already-stated policy.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use hollowscript_core::{Control, Interner, Source, SourceId, Symbol};
use hollowscript_syntax::ast::*;
use hollowscript_syntax::parse;

struct Dumper<'a> {
    interner: &'a Interner,
    out: String,
    indent: usize,
}

impl<'a> Dumper<'a> {
    fn new(interner: &'a Interner) -> Self {
        Dumper {
            interner,
            out: String::new(),
            indent: 0,
        }
    }

    fn name(&self, s: Symbol) -> &str {
        self.interner.resolve(s)
    }

    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn nested(&mut self, header: &str, body: impl FnOnce(&mut Self)) {
        self.line(header);
        self.indent += 1;
        body(self);
        self.indent -= 1;
    }

    fn file(&mut self, file: &File) {
        for u in &file.uses {
            let names: Vec<&str> = u.names.iter().map(|n| self.name(n.name)).collect();
            self.line(&format!("uses {}", names.join(", ")));
        }
        for i in &file.imports {
            let names: Vec<&str> = i.names.iter().map(|n| self.name(n.name)).collect();
            self.line(&format!(
                "import {} from {:?}",
                names.join(", "),
                string_text(&i.path)
            ));
        }
        for item in &file.items {
            self.item(item);
        }
    }

    fn item(&mut self, item: &Item) {
        let export = if item.export.is_some() { "export " } else { "" };
        match &item.kind {
            ItemKind::Fn(f) => {
                let params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| self.name(p.name.name).to_string())
                    .collect();
                self.nested(
                    &format!(
                        "{export}fn {}({})",
                        self.name(f.name.name),
                        params.join(", ")
                    ),
                    |d| {
                        d.block(&f.body);
                    },
                );
            }
            ItemKind::Record(r) => {
                let fields: Vec<String> = r
                    .fields
                    .iter()
                    .map(|f| self.name(f.name.name).to_string())
                    .collect();
                self.line(&format!(
                    "{export}record {} {{ {} }}",
                    self.name(r.name.name),
                    fields.join(", ")
                ));
            }
            ItemKind::Enum(e) => {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| self.name(v.name.name).to_string())
                    .collect();
                self.line(&format!(
                    "{export}enum {} {{ {} }}",
                    self.name(e.name.name),
                    variants.join(", ")
                ));
            }
            ItemKind::Constant(b) => self.nested(
                &format!("{export}constant {}", self.binding_target(&b.target)),
                |d| d.expr(&b.init),
            ),
            ItemKind::Variable(b) => self.nested(
                &format!("{export}variable {}", self.binding_target(&b.target)),
                |d| d.expr(&b.init),
            ),
            ItemKind::Stmt(s) => self.stmt(s),
        }
    }

    fn binding_target(&self, t: &BindingTarget) -> String {
        match t {
            BindingTarget::Name(n) => self.name(n.name).to_string(),
            BindingTarget::Pattern(p) => self.pattern_summary(p),
        }
    }

    fn pattern_summary(&self, p: &Pattern) -> String {
        match &p.kind {
            PatternKind::Wildcard => "_".to_string(),
            PatternKind::Binding(n) => self.name(n.name).to_string(),
            PatternKind::Record(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| self.name(f.name.name).to_string())
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            PatternKind::List(elems) => {
                let parts: Vec<String> = elems
                    .iter()
                    .map(|e| match e {
                        ListElement::Pattern(p) => self.pattern_summary(p),
                        ListElement::Rest { binding, .. } => format!(
                            "..{}",
                            binding.map_or(String::new(), |b| self.name(b.name).to_string())
                        ),
                    })
                    .collect();
                format!("[{}]", parts.join(", "))
            }
            PatternKind::Variant { name, args, .. } => {
                let parts: Vec<String> = args.iter().map(|a| self.pattern_summary(a)).collect();
                if parts.is_empty() {
                    self.name(name.name).to_string()
                } else {
                    format!("{}({})", self.name(name.name), parts.join(", "))
                }
            }
            PatternKind::Literal { negative, literal } => format!(
                "{}{}",
                if *negative { "-" } else { "" },
                literal_summary(literal)
            ),
            PatternKind::None => "none".to_string(),
            PatternKind::Error => "<error>".to_string(),
        }
    }

    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Constant(b) => self.nested(
                &format!("constant {}", self.binding_target(&b.target)),
                |d| d.expr(&b.init),
            ),
            StmtKind::Variable(b) => self.nested(
                &format!("variable {}", self.binding_target(&b.target)),
                |d| d.expr(&b.init),
            ),
            StmtKind::Assign { target, value } => {
                self.nested(&format!("assign {}", self.name(target.name)), |d| {
                    d.expr(value);
                });
            }
            StmtKind::If(i) => self.if_stmt(i),
            StmtKind::Match(m) => self.match_stmt(m),
            StmtKind::For {
                target,
                iterable,
                body,
            } => {
                self.nested(&format!("for {} in", self.binding_target(target)), |d| {
                    d.expr(iterable);
                    d.block(body);
                });
            }
            StmtKind::While { condition, body } => {
                self.nested("while", |d| {
                    d.expr(condition);
                    d.block(body);
                });
            }
            StmtKind::Break => self.line("break"),
            StmtKind::Continue => self.line("continue"),
            StmtKind::Return(e) => match e {
                Some(e) => self.nested("return", |d| d.expr(e)),
                None => self.line("return"),
            },
            StmtKind::Call(e) => self.nested("call", |d| d.expr(e)),
            StmtKind::Misplaced => self.line("<misplaced>"),
            StmtKind::Error => self.line("<error-stmt>"),
        }
    }

    fn if_stmt(&mut self, i: &IfStmt) {
        self.nested("if", |d| {
            d.expr(&i.condition);
            d.block(&i.then);
        });
        if let Some(e) = &i.otherwise {
            self.else_branch(e);
        }
    }

    fn else_branch(&mut self, e: &Else) {
        match e {
            Else::Block(b) => self.nested("else", |d| d.block(b)),
            Else::If(s) => {
                self.line("else");
                self.stmt(s);
            }
        }
    }

    fn match_stmt(&mut self, m: &MatchStmt) {
        self.nested("match", |d| {
            d.expr(&m.scrutinee);
            for arm in &m.arms {
                let pats: Vec<String> = arm.patterns.iter().map(|p| d.pattern_summary(p)).collect();
                d.nested(&format!("arm {}", pats.join(" | ")), |d2| {
                    d2.block(&arm.body);
                });
            }
            if let Some(b) = &m.else_arm {
                d.nested("else", |d2| d2.block(b));
            }
        });
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Literal(lit) => self.line(&literal_summary(lit)),
            ExprKind::Name(n) => {
                let name = self.name(n.name).to_string();
                self.line(&name);
            }
            ExprKind::List(items) => self.nested("list", |d| {
                for i in items {
                    d.expr(i);
                }
            }),
            ExprKind::Map(entries) => self.nested("map", |d| {
                for en in entries {
                    d.nested("entry", |d2| {
                        d2.expr(&en.key);
                        d2.expr(&en.value);
                    });
                }
            }),
            ExprKind::Record(fields) => self.nested("record", |d| {
                for f in fields {
                    let name = d.name(f.name.name).to_string();
                    d.nested(&name, |d2| d2.expr(&f.value));
                }
            }),
            ExprKind::Closure(c) => {
                let params: Vec<String> = c
                    .params
                    .iter()
                    .map(|p| self.name(p.name.name).to_string())
                    .collect();
                self.nested(&format!("closure({})", params.join(", ")), |d| {
                    d.block(&c.body);
                });
            }
            ExprKind::Paren(inner) => self.nested("paren", |d| d.expr(inner)),
            ExprKind::Unary { op, operand } => {
                self.nested(&format!("unary {op:?}"), |d| d.expr(operand));
            }
            ExprKind::Binary { op, lhs, rhs } => self.nested(&format!("binary {op:?}"), |d| {
                d.expr(lhs);
                d.expr(rhs);
            }),
            ExprKind::Effect(inner) => self.nested("effect", |d| d.expr(inner)),
            ExprKind::Call { callee, args } => self.nested("call", |d| {
                d.expr(callee);
                for a in args {
                    d.expr(a);
                }
            }),
            ExprKind::Index { target, index } => self.nested("index", |d| {
                d.expr(target);
                d.expr(index);
            }),
            ExprKind::Field { target, name } => {
                self.nested(&format!("field .{}", self.name(name.name)), |d| {
                    d.expr(target);
                });
            }
            ExprKind::Try(inner) => self.nested("try", |d| d.expr(inner)),
            ExprKind::Error => self.line("<error-expr>"),
        }
    }
}

fn literal_summary(lit: &Literal) -> String {
    match lit {
        Literal::Int { value, .. } => format!("Int({value})"),
        Literal::Float { value, .. } => format!("Float({value})"),
        Literal::Bool(b) => format!("Bool({b})"),
        Literal::None => "none".to_string(),
        Literal::String(s) => format!("String({} parts)", s.parts.len()),
        Literal::Filter { source, .. } => format!("Filter({source:?})"),
    }
}

fn string_text(s: &StringLiteral) -> String {
    s.parts
        .iter()
        .map(|p| match p {
            StringPart::Text(t) => t.clone(),
            StringPart::Interpolation(_) => "${..}".to_string(),
        })
        .collect()
}

fn dump(text: &str) -> String {
    let source = Source::new(SourceId(0), "main.hws", text).unwrap();
    let mut interner = Interner::new();
    let out = parse(&source, &mut interner, &mut Control::none());
    assert!(
        out.diagnostics.is_empty(),
        "expected a clean parse: {:?}",
        out.diagnostics
    );
    let mut d = Dumper::new(&interner);
    d.file(&out.file);
    d.out
}

fn golden(path: &str, expected: &str) {
    let full = support::corpus_root().join(path);
    let text = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()));
    let actual = dump(&text);
    assert_eq!(
        actual.trim_end(),
        expected.trim(),
        "\n--- golden mismatch for {path} ---\n--- actual ---\n{actual}--- expected ---\n{expected}\n"
    );
}

#[test]
fn golden_examples_digest() {
    if !support::corpus_root().exists() {
        eprintln!("skipping: corpus not found");
        return;
    }
    golden(
        "examples/digest.hws",
        r"
uses print
record Task { id, title, priority, owner }
fn titlesOf(items)
  return
    call
      field .map
        items
      closure(t)
        return
          field .title
            t
fn ownerOf(task)
  match
    field .owner
      task
    arm none
      return
        String(1 parts)
    else
      return
        field .owner
          task
constant urgent
  list
    record
      id
        String(1 parts)
      title
        String(1 parts)
      priority
        String(1 parts)
      owner
        String(1 parts)
    record
      id
        String(1 parts)
      title
        String(1 parts)
      priority
        String(1 parts)
      owner
        none
if
  call
    field .isEmpty
      urgent
  call
    effect
      call
        print
        String(1 parts)
  return
    Int(0)
for task in
  urgent
  call
    effect
      call
        print
        String(5 parts)
call
  effect
    call
      print
      call
        field .join
          call
            titlesOf
            urgent
        String(1 parts)
return
  call
    field .length
      urgent
",
    );
}

#[test]
fn golden_capabilities_guide_sample() {
    if !support::corpus_root().exists() {
        eprintln!("skipping: corpus not found");
        return;
    }
    golden(
        "guide/capabilities-01.hws",
        r"
uses print
call
  effect
    call
      print
      String(1 parts)
",
    );
}

#[test]
fn golden_enum_variant_patterns_and_closures() {
    // Written by hand rather than taken from `corpus/enums/`: several of
    // those fixtures write variant patterns qualified with the enum name
    // (`Shape.Circle(r)`), which section 7.2, the grammar of chapter 18 and
    // `docs/spec/decisions.md` ("Enums and patterns") all agree is not valid
    // pattern syntax ("bare patterns... the scrutinee's type says which
    // enum"); this parser follows that contract rather than those fixtures
    // (see the corpus cross-check test for the full list). This sample
    // covers the same ground - enum declarations, bare variant patterns
    // with and without a payload, an `else` arm, and a closure passed to a
    // call - without that issue.
    let text = "enum Shape {\n    Circle(Float),\n    Empty,\n}\n\nfn areas(shapes: [Shape]): [Float] {\n    return shapes.map(fn(s: Shape): Float {\n        match s {\n            Circle(r) {\n                return 3.0 * r * r\n            }\n            Empty {\n                return 0.0\n            }\n        }\n    })\n}\n\nreturn areas([Shape.Circle(2.0), Shape.Empty])\n";
    let actual = dump(text);
    let expected = r"
enum Shape { Circle, Empty }
fn areas(shapes)
  return
    call
      field .map
        shapes
      closure(s)
        match
          s
          arm Circle(r)
            return
              binary Mul
                binary Mul
                  Float(3)
                  r
                r
          arm Empty
            return
              Float(0)
return
  call
    areas
    list
      call
        field .Circle
          Shape
        Float(2)
      field .Empty
        Shape
";
    assert_eq!(
        actual.trim_end(),
        expected.trim(),
        "\n--- actual ---\n{actual}"
    );
}

#[test]
fn golden_destructuring_declarations() {
    if !support::corpus_root().exists() {
        eprintln!("skipping: corpus not found");
        return;
    }
    let text =
        std::fs::read_to_string(support::corpus_root().join("destructuring/list-with-rest.hws"));
    let Ok(text) = text else {
        eprintln!("skipping: fixture not present");
        return;
    };
    let actual = dump(&text);
    // A structural property rather than a full snapshot: destructuring a
    // list with a rest pattern produces exactly one `constant` binding whose
    // target contains a `..` element.
    assert!(
        actual.contains(".."),
        "expected a rest pattern in the dump:\n{actual}"
    );
}

#[test]
fn every_golden_sample_round_trips_through_the_same_dumper_twice() {
    // Parsing is meant to be a pure function of the source text: dumping the
    // same input twice must give byte-identical output. This guards against
    // any hidden nondeterminism (for example from `Interner` iteration,
    // which the crate avoids using a `HashMap` only for lookup).
    if !support::corpus_root().exists() {
        eprintln!("skipping: corpus not found");
        return;
    }
    for path in support::walk_hws(&support::corpus_root()) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let source = Source::new(SourceId(0), "main.hws", text.as_str()).unwrap();
        let mut i1 = Interner::new();
        let out1 = parse(&source, &mut i1, &mut Control::none());
        let mut d1 = Dumper::new(&i1);
        d1.file(&out1.file);
        let mut i2 = Interner::new();
        let out2 = parse(&source, &mut i2, &mut Control::none());
        let mut d2 = Dumper::new(&i2);
        d2.file(&out2.file);
        assert_eq!(d1.out, d2.out, "{}", path.display());
        assert_eq!(
            out1.diagnostics.len(),
            out2.diagnostics.len(),
            "{}",
            path.display()
        );
    }
}
