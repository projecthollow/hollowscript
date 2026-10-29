//! The typed tree the checker produces.
//!
//! Where the syntax tree ([`hollowscript_syntax::ast`]) is a direct
//! transcription of the source, the HIR is the same program after names are
//! resolved to bindings, every expression carries its [`TypeId`], generic
//! calls are resolved to a fixed instantiation, and narrowing and
//! exhaustiveness have been decided. The compiler reads only the HIR: it never
//! looks at the syntax tree or does name lookup of its own.
//!
//! Every node keeps the [`hollowscript_syntax::ast::NodeId`] of the syntax it
//! came from, so a diagnostic or a fault can point at a span without the HIR
//! repeating spans itself.

use crate::types::TypeId;
use hollowscript_core::Symbol;
/// The identity a node keeps from the syntax tree, which is how the HIR
/// names a position without carrying one.
pub use hollowscript_syntax::ast::NodeId;
use std::collections::BTreeMap;

/// The identity of a local binding: a `constant`, a `variable`, a parameter, a
/// loop variable or a pattern binding, unique within its function.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// The identity of a top-level binding (a `constant`, a `variable` or a
/// function), unique within its module. A closure that captures a top-level
/// binding refers to it this way rather than as an upvalue.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GlobalId {
    /// The declaring module.
    pub module: u32,
    /// A per-module counter.
    pub index: u32,
}

/// The identity of a top-level `fn` declaration.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(pub u32);

/// The identity of a capability declared by a `uses` line, resolved to one
/// entry of the run's [`crate::CapabilityInterface`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapabilityId(pub u32);

/// A whole checked program: every module, in initialisation order (section
/// 10.5), and the type table they share.
#[derive(Debug, Default)]
pub struct Program {
    /// The modules, in the order in which they must be initialised: each
    /// module's imports appear before it. The last is the entry module.
    pub modules: Vec<Module>,
    /// The interner every [`TypeId`] of this program refers to.
    pub types: crate::types::TypeInterner,
    /// Every function of the program, indexed by [`FunctionId`].
    pub functions: Vec<Function>,
    /// The checker type of every capability the host granted, indexed by
    /// [`CapabilityId`]: an effectful function type for a capability that is
    /// one function, a record of effectful function types for a namespace.
    /// This is the declared interface of section 9.6, which the compiler
    /// writes into the module so that the VM can hold a host to it.
    pub capability_types: Vec<TypeId>,
}

/// One checked module.
#[derive(Debug)]
pub struct Module {
    /// The canonical path.
    pub path: String,
    /// The capabilities this module declared with `uses`.
    pub capabilities: Vec<(Symbol, CapabilityId)>,
    /// The top-level `constant` and `variable` bindings, in source order,
    /// which is also their initialisation order (section 4.10).
    pub globals: Vec<Global>,
    /// Every function and closure declared anywhere in this module, top
    /// level or nested, in the order they were checked. The compiler uses
    /// this to find which module's source a function's instructions came
    /// from, for the position a fault raised inside it is reported at
    /// (section 19.3): a function this list leaves out is located nowhere.
    pub function_ids: Vec<FunctionId>,
    /// The top-level statements that are not a declaration, run in order
    /// during initialisation, interleaved with the globals by source position.
    pub init: Block,
    /// The names, and their [`GlobalId`] or [`FunctionId`], that this module
    /// exports (section 10.4).
    pub exports: Vec<(Symbol, Export)>,
    /// Where each of this module's statements and expressions is in its
    /// source, so that a fault can be located at the operation that raised
    /// it (section 19.3). Empty for a module built by hand rather than
    /// parsed.
    pub node_spans: BTreeMap<NodeId, crate::spans::NodeSpan>,
}

/// What a module exports.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Export {
    /// An exported function.
    Function(FunctionId),
    /// An exported constant.
    Constant(GlobalId),
    /// An exported record or enum type, exported for its name alone: nothing
    /// runs for it, so it needs no run-time identity here.
    Type(crate::types::DeclId),
}

/// A top-level `constant` or `variable`.
#[derive(Debug)]
pub struct Global {
    /// Its identity.
    pub id: GlobalId,
    /// The syntax it came from.
    pub node: NodeId,
    /// The name.
    pub name: Symbol,
    /// Whether it is a `variable`.
    pub mutable: bool,
    /// Whether any function or closure other than the one that declares it
    /// assigns it, which makes it *shared-mutable* and exempt from narrowing
    /// (section 4.8). Always `false` for a `constant`.
    pub shared_mutable: bool,
    /// Its type.
    pub ty: TypeId,
    /// The initialiser, or `None` for a binding a destructuring declaration
    /// introduces: one initialiser gives several globals their values, so
    /// it is evaluated once by the [`StmtKind::Let`] that destructures it
    /// and each name is then written with [`StmtKind::AssignGlobal`].
    pub init: Option<Expr>,
}

/// A checked function or closure body.
#[derive(Debug)]
pub struct Function {
    /// The syntax it came from.
    pub node: NodeId,
    /// The name, or `<closure>`.
    pub name: Symbol,
    /// The parameters, in order; also `LocalId`s 0 to `params.len() - 1`.
    pub params: Vec<Param>,
    /// The return type, if the function returns a value.
    pub ret: Option<TypeId>,
    /// The number of `LocalId`s the body uses, so a caller can size a frame
    /// without walking the body again.
    pub local_count: u32,
    /// The bindings a closure captures from its enclosing scopes, empty for a
    /// top-level function. Ordered, and referred to by position from the body
    /// as an upvalue.
    pub captures: Vec<Capture>,
    /// The body.
    pub body: Block,
}

/// A parameter of a function or closure.
#[derive(Debug)]
pub struct Param {
    /// The syntax it came from.
    pub node: NodeId,
    /// Its local identity within the function.
    pub local: LocalId,
    /// The name, or `_`.
    pub name: Symbol,
    /// Its type.
    pub ty: TypeId,
}

/// One binding a closure captures, by where it lives in the enclosing scope.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    /// A local of the immediately enclosing function.
    Local(LocalId),
    /// An upvalue of the immediately enclosing closure, forwarded.
    Upvalue(u32),
}

// A top-level binding is never captured. It is reachable from every
// function of its module by its global slot, so a closure reads and writes
// it directly through [`ExprKind::Global`] and [`StmtKind::AssignGlobal`],
// which is also what makes a closure see a later assignment to it
// (section 5.4). Capturing one instead would snapshot its value and break
// that, and section 11.4's retained-size walk counts it once as a top-level
// binding either way.

/// A sequence of statements.
#[derive(Debug, Default)]
pub struct Block {
    /// The statements, in order.
    pub stmts: Vec<Stmt>,
}

/// A checked statement.
#[derive(Debug)]
pub struct Stmt {
    /// The syntax it came from.
    pub node: NodeId,
    /// What it does.
    pub kind: StmtKind,
}

/// The kinds of checked statement. Top-level `fn`, `record` and `enum` are not
/// among them: they are lifted into [`Program::functions`] and
/// [`crate::types::TypeInterner`] and are not part of any block's statement list.
#[derive(Debug)]
pub enum StmtKind {
    /// A local `constant` or `variable`, or a destructuring of one.
    Let {
        /// The bindings this declaration introduces, in the order a nested
        /// pattern binds them.
        bindings: Vec<LocalBinding>,
        /// The pattern the value is destructured by, or `None` for a plain name.
        pattern: Option<Pattern>,
        /// The initialiser.
        init: Expr,
    },
    /// `name = value`, naming the `variable` assigned.
    Assign {
        /// The assigned local.
        local: LocalId,
        /// The value.
        value: Expr,
    },
    /// `name = value` where `name` is a `variable` of an enclosing function,
    /// captured by the current closure as its upvalue `index` (section 5.4).
    AssignUpvalue {
        /// The current closure's upvalue that holds the variable's cell.
        index: u32,
        /// The value.
        value: Expr,
    },
    /// `name = value` where `name` is a top-level `variable` (section 4.3):
    /// kept apart from [`StmtKind::Assign`], whose `local` is scoped to the
    /// current function, because a top-level binding has no such frame and
    /// is identified the same way everywhere it is reached, by
    /// [`GlobalId`], including from the top-level code that declares it.
    AssignGlobal {
        /// The assigned global.
        global: GlobalId,
        /// The value.
        value: Expr,
    },
    /// `if`, with its `else if` run and `else`.
    If {
        /// The condition.
        condition: Expr,
        /// The narrowing facts true inside `then` beyond those already in force.
        then_facts: Vec<Fact>,
        /// The block that runs when the condition is true.
        then: Block,
        /// The facts true inside `otherwise`.
        else_facts: Vec<Fact>,
        /// The `else` block or `else if`, absent for a bare `if`.
        otherwise: Option<Block>,
    },
    /// `match`.
    Match(Match),
    /// `for target in iterable { body }`.
    For {
        /// The loop variable's bindings (a record or list pattern may bind several).
        bindings: Vec<LocalBinding>,
        /// The pattern that destructures each element, or `None` for a plain name.
        pattern: Option<Pattern>,
        /// The element type, per the kind of `iterable`.
        element_ty: TypeId,
        /// The collection, evaluated once before the first iteration.
        iterable: Expr,
        /// The loop body.
        body: Block,
    },
    /// `while condition { body }`.
    While {
        /// The condition, re-evaluated before each iteration.
        condition: Expr,
        /// The facts in force at the top of the loop, after those rooted at an
        /// assigned variable are dropped (section 4.8).
        loop_facts: Vec<Fact>,
        /// The loop body.
        body: Block,
    },
    /// `break`, naming the loop it leaves so that the compiler need not search for it.
    Break {
        /// How many enclosing loops of the same function to leave: 0 for the innermost.
        depth: u32,
    },
    /// `continue`, named the same way as `break`.
    Continue {
        /// How many enclosing loops of the same function to skip to: 0 for the innermost.
        depth: u32,
    },
    /// `return`, with a value for a function that declares a return type.
    Return(Option<Expr>),
    /// A call standing alone as a statement.
    Call(Expr),
}

/// One name a `let` or a `for` binds, and where.
#[derive(Debug)]
pub struct LocalBinding {
    /// The syntax it came from.
    pub node: NodeId,
    /// Its identity.
    pub local: LocalId,
    /// The name, or `_` for a binding that is never read.
    pub name: Symbol,
    /// Its type.
    pub ty: TypeId,
}

/// A checked `match`.
#[derive(Debug)]
pub struct Match {
    /// The scrutinee.
    pub scrutinee: Expr,
    /// If the scrutinee is a narrowable path, its facts (section 7.6).
    pub scrutinee_path: Option<Path>,
    /// The arms, in the order they are tried.
    pub arms: Vec<Arm>,
    /// Whether an `else` arm was needed for exhaustiveness, or the listed
    /// arms already covered the type (section 7.5).
    pub exhaustive_without_else: bool,
}

/// One arm of a checked `match`.
#[derive(Debug)]
pub struct Arm {
    /// The alternatives, which may not bind names when there is more than one
    /// (HS0508), so the bindings are common to the arm.
    pub patterns: Vec<Pattern>,
    /// The bindings the arm's pattern makes, empty for `else`.
    pub bindings: Vec<LocalBinding>,
    /// The block that runs when this arm matches.
    pub body: Block,
}

/// A checked pattern (chapter 7), typed against its scrutinee.
#[derive(Debug)]
pub struct Pattern {
    /// The syntax it came from.
    pub node: NodeId,
    /// The type of the value it matches.
    pub ty: TypeId,
    /// What kind of pattern.
    pub kind: PatternKind,
}

/// The kinds of checked pattern.
#[derive(Debug)]
pub enum PatternKind {
    /// A literal pattern, holding the value it compares equal to.
    Literal(Constant),
    /// `none`.
    None,
    /// `_`, or a binding, with the local it binds (absent for `_`).
    Binding(Option<LocalId>),
    /// A variant pattern, naming which variant of the scrutinee's enum by position.
    Variant {
        /// The position of the variant within its enum's declared order.
        index: u32,
        /// The sub-patterns, matched against the payload position by position.
        args: Vec<Pattern>,
    },
    /// A record pattern. The list holds only the fields written; others are
    /// implicitly `_` and are not part of the tree.
    Record(Vec<RecordFieldPattern>),
    /// A list pattern with an optional rest.
    List {
        /// The patterns before the rest, or all of them if there is none.
        elements: Vec<Pattern>,
        /// The local the rest binds, `None` for `..` alone, and `None` when
        /// there is no rest pattern at all (use `elements.len()` to tell the two apart).
        rest: Option<Option<LocalId>>,
    },
}

/// A checked expression.
#[derive(Debug)]
pub struct Expr {
    /// The syntax it came from.
    pub node: NodeId,
    /// Its type once all coercions of section 2.7 are applied.
    pub ty: TypeId,
    /// What kind of expression.
    pub kind: ExprKind,
}

/// A constant value that a literal or a constant-folded expression denotes.
/// Used in patterns, where a scrutinee is compared for equality, and as the
/// payload of [`ExprKind::Literal`].
///
/// `Eq` compares a `Float` by bit pattern, not by numeric equality: patterns
/// need to tell `0.0` from a value that only compares equal to it under the
/// language's own `==`, and section 7.7 treats `0.0` and `-0.0` as one
/// constructor by value, which the checker normalises to `0.0` before this
/// is built (the language itself never produces a negative zero, section 3.4).
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    /// An `Int` value.
    Int(i64),
    /// Finite; never negative zero (section 3.4).
    Float(f64),
    /// `true` or `false`.
    Bool(bool),
    /// A `String` value.
    String(String),
    /// The `none` literal (section 3.1 lists it among the literals), typed
    /// to the optional type the checker gave it (section 2.9). Distinct from
    /// [`PatternKind::None`], which a pattern uses directly rather than
    /// wrapping in a `Constant`, since a pattern is never a literal value to
    /// compare, only a shape to match.
    None,
}

impl Eq for Constant {}

/// One field of a record pattern, with the position that reads it at run
/// time.
#[derive(Debug)]
pub struct RecordFieldPattern {
    /// The field's name, as the pattern wrote it.
    pub name: Symbol,
    /// The field's position in its *record type's* field order, which is
    /// what reads it at run time. It is not the field's position within the
    /// pattern: a pattern may write a subset of the record's fields, in any
    /// order, and `{ y }` on a `{ x: Int, y: Int }` reads position 1.
    pub position: u32,
    /// The pattern the field is matched against.
    pub pattern: Pattern,
}

/// One field access of a [`Path`].
///
/// The name is what identifies the field to a reader and to fact equality;
/// the position is the same one [`ExprKind::Field`] carries, the field's rank
/// in its record type's own field order. Both are kept because the checker
/// knows the position while it builds the path and nothing downstream can
/// recover it: a `TypeId`'s field list is not reachable from outside
/// `crate::types`, so a compiler given only the name could not emit the field
/// read. Two paths with the same root and the same field names always have
/// the same positions, since the root's type fixes the record type at every
/// step, so comparing a path by name alone and comparing it by both give the
/// same answer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PathField {
    /// The field's name.
    pub name: Symbol,
    /// The field's position in its record type's field order.
    pub index: u32,
}

/// The root of a [`Path`]: a local of the current frame, or a top-level
/// binding of the module currently being checked, read directly (section
/// 4.8's facts are rooted alike at either kind; nothing in the truth-set
/// table or the statement rules distinguishes them). A top-level binding of
/// *another* module is never a root: this checker has no way to know,
/// looking only at this file, whether it is shared-mutable (section 4.8's
/// rule is per declaring module), so [`crate::narrow::path_of`] simply does
/// not build a path for one, which is conservative, never unsound.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathRoot {
    /// A local of the frame currently being checked.
    Local(LocalId),
    /// A top-level `constant` or `variable` of the module currently being
    /// checked.
    Global(GlobalId),
}

/// A path (section 4.8): a local or top-level binding, optionally followed by
/// field accesses, that optional narrowing can track. `x`, `task.owner`,
/// `a.b.c`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    /// The root.
    pub root: PathRoot,
    /// The field accesses, in order.
    pub fields: Vec<PathField>,
}

/// A narrowing fact (section 4.8): a path known not to be `none` at a point
/// of the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    /// The path known not to be `none`.
    pub path: Path,
}

/// The kinds of checked expression. A coercion of section 2.7 that the source
/// does not write explicitly (an `Int` used as a `Float`) is represented by
/// wrapping the coerced expression in [`ExprKind::IntToFloat`], so the
/// compiler never has to consult the original syntax to find one.
#[derive(Debug)]
pub enum ExprKind {
    /// A literal value.
    Literal(Constant),
    /// A filter literal, already validated; opaque from here on.
    Filter(String),
    /// A read of a local.
    Local(LocalId),
    /// A read of an upvalue of the enclosing closure.
    Upvalue(u32),
    /// A read of a top-level binding.
    Global(GlobalId),
    /// A read of a narrowed path, `ty` already the inner type of the optional.
    NarrowedPath(Path),
    /// A capability name used as a value (only well typed as the operand of an
    /// immediate `effect` call; the checker rejects every other use).
    Capability(CapabilityId),
    /// One function of a namespace capability, as the callee of an immediate
    /// `effect` call: `clock.now`, `fs.read`.
    ///
    /// This is not [`ExprKind::Field`] over a [`ExprKind::Capability`],
    /// although that is what the source looks like, because the compiler has
    /// to write the member's own name into the module's capability table and
    /// a field access names its field by position alone. The checker knows
    /// the name; nothing downstream could recover it.
    CapabilityMember {
        /// The capability the member belongs to.
        capability: CapabilityId,
        /// The member's name, as the host declared it.
        name: Symbol,
        /// Its position in the namespace, which is the sorted field order of
        /// the record the capability's type is.
        index: u32,
    },
    /// A reference to a top-level function, not called.
    FunctionValue(FunctionId),
    /// A library function used as a value rather than called (`sqrt` on its
    /// own), by its index into `hollowscript_core::native::ROSTER`. Section
    /// 13.6 lists these as values, so a program may pass one where a
    /// function type is expected.
    NativeValue(u32),
    /// A pure reference to an enum variant constructor, not called.
    VariantValue {
        /// The enum the variant belongs to.
        decl: crate::types::DeclId,
        /// The position of the variant within its enum's declared order.
        index: u32,
    },
    /// A list literal.
    List(Vec<Expr>),
    /// A map literal, key and value pairs in source order.
    Map(Vec<(Expr, Expr)>),
    /// A record literal.
    Record(Vec<(Symbol, Expr)>),
    /// A closure literal.
    Closure(FunctionId),
    /// A prefix operator.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
    },
    /// `not e` (section 3.3). Kept apart from [`ExprKind::Unary`], whose
    /// [`UnaryOp`] is only ever a numeric negation, and apart from
    /// [`ExprKind::Logical`], whose shape is fixed at two operands plus
    /// short-circuit narrowing facts for the right one, a shape that only
    /// makes sense for `and`/`or`: `not` takes exactly one operand and never
    /// short-circuits, so it needs a node of its own rather than a synthetic
    /// second operand on either of those.
    Not(Box<Expr>),
    /// A binary arithmetic or comparison operator.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left operand.
        lhs: Box<Expr>,
        /// The right operand.
        rhs: Box<Expr>,
    },
    /// `a and b` / `a or b`, kept apart from [`ExprKind::Binary`] because they
    /// short-circuit and carry narrowing facts for their right operand.
    Logical {
        /// The operator.
        op: LogicalOp,
        /// The left operand.
        lhs: Box<Expr>,
        /// The right operand.
        rhs: Box<Expr>,
        /// The facts under which `rhs` is checked, beyond those already in force.
        rhs_facts: Vec<Fact>,
    },
    /// An implicit `Int` to `Float` conversion inserted at a coercion site.
    IntToFloat(Box<Expr>),
    /// A call of a value of function type.
    Call {
        /// The callee.
        callee: Box<Expr>,
        /// The arguments.
        args: Vec<Expr>,
    },
    /// A call whose callee has an effectful type, so the source wrote `effect`.
    EffectCall {
        /// The callee.
        callee: Box<Expr>,
        /// The arguments.
        args: Vec<Expr>,
    },
    /// A call of a native library function or method, resolved to the roster
    /// (`hollowscript_core::native`) rather than left as a general call.
    NativeCall {
        /// The index into `hollowscript_core::native::ROSTER`.
        native: u32,
        /// The arguments, receiver first for a method.
        args: Vec<Expr>,
    },
    /// `enum.Variant(args)`.
    VariantConstruct {
        /// The enum the variant belongs to.
        decl: crate::types::DeclId,
        /// The position of the variant within its enum's declared order.
        index: u32,
        /// The payload values.
        args: Vec<Expr>,
    },
    /// `target[index]`.
    Index {
        /// The list or map indexed.
        target: Box<Expr>,
        /// The index or key.
        index: Box<Expr>,
    },
    /// `target.name`.
    Field {
        /// The record read.
        target: Box<Expr>,
        /// The position of the field within the record's sorted field list,
        /// which is the order [`crate::types::TypeInterner::record`] gives it.
        index: u32,
    },
    /// `e?` (section 8.3): the payload type is `ty`; `err_ty` is the error
    /// type of the enclosing function's `Result` return type, needed to build
    /// the early `Result.Err` return.
    Try {
        /// The `Result` operand.
        operand: Box<Expr>,
        /// The error type of the enclosing function's return type.
        err_ty: TypeId,
    },
    /// String interpolation, holding the already-converted-to-text operands
    /// between the literal text segments; `text` alternates with `exprs`,
    /// starting and ending with a text segment (an empty string for none).
    Interpolate {
        /// The literal text segments, `exprs.len() + 1` of them.
        text: Vec<String>,
        /// The interpolated expressions, already converted to `String`.
        exprs: Vec<Expr>,
    },
    /// An expression the checker could not give a valid type; assignable to
    /// and from everything, so that using it produces no further diagnostic
    /// (section 2.11).
    Error,
}

/// A numeric prefix operator (section 3.3): `not` is [`ExprKind::Not`], not
/// this, because it is boolean rather than numeric and never appears as a
/// [`LogicalOp`] either, for the same reason [`ExprKind::Not`] documents.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-` on an `Int`.
    NegInt,
    /// `-` on a `Float`.
    NegFloat,
}

/// A binary arithmetic or comparison operator, resolved to the operand kind
/// it was checked against (so `+` on two `Int`s and `+` on two `Float`s are
/// different variants, and the compiler need not re-derive that from types).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+` on two `Int`s.
    AddInt,
    /// `-` on two `Int`s.
    SubInt,
    /// `*` on two `Int`s.
    MulInt,
    /// `+` where an operand is a `Float`.
    AddFloat,
    /// `-` where an operand is a `Float`.
    SubFloat,
    /// `*` where an operand is a `Float`.
    MulFloat,
    /// `/`, which always produces a `Float`.
    Div,
    /// Equality and ordering compare by tree size at run time (section 11.3);
    /// the checker only needs to know whether both operands are scalars, for
    /// the step charge, which `scalar_operands` records.
    Eq {
        /// Whether both operands are `Int`, `Float`, `Bool` or `none`.
        scalar_operands: bool,
    },
    /// `!=`, with the same charge as `Eq`.
    Ne {
        /// Whether both operands are `Int`, `Float`, `Bool` or `none`.
        scalar_operands: bool,
    },
    /// `<` on numbers or strings.
    Lt,
    /// `>` on numbers or strings.
    Gt,
    /// `<=` on numbers or strings.
    Le,
    /// `>=` on numbers or strings.
    Ge,
}

/// `and` and `or`: the two short-circuiting operators, each with a left and
/// a right operand and narrowing facts for the right (see
/// [`ExprKind::Logical`]). `not` is not one of these: it takes a single
/// operand, never short-circuits and carries no narrowing facts of its own,
/// so it is [`ExprKind::Not`] instead.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    /// `and`.
    And,
    /// `or`.
    Or,
}

#[cfg(test)]
mod tests {
    use super::{Constant, Expr, ExprKind, FunctionId, GlobalId, LocalId, NodeId, Program};
    use crate::types::TypeInterner;

    #[test]
    fn ids_are_small_and_distinct() {
        assert_ne!(LocalId(0), LocalId(1));
        assert_ne!(
            GlobalId {
                module: 0,
                index: 0
            },
            GlobalId {
                module: 1,
                index: 0
            }
        );
        assert_ne!(FunctionId(0), FunctionId(1));
        assert!(Program::default().modules.is_empty());
    }

    #[test]
    fn not_takes_a_single_operand_with_no_synthetic_second_one() {
        let interner = TypeInterner::new();
        let operand = Box::new(Expr {
            node: NodeId(0),
            ty: interner.bool(),
            kind: ExprKind::Literal(Constant::Bool(true)),
        });
        let not_expr = Expr {
            node: NodeId(1),
            ty: interner.bool(),
            kind: ExprKind::Not(operand),
        };
        match not_expr.kind {
            ExprKind::Not(inner) => match inner.kind {
                ExprKind::Literal(Constant::Bool(true)) => {}
                _ => panic!("the operand should round trip untouched"),
            },
            _ => panic!("expected ExprKind::Not"),
        }
    }
}
