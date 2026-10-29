//! The syntax tree.
//!
//! The tree is owned and immutable once built. Every node has a [`NodeId`],
//! unique within its file and assigned in creation order, so later passes keep
//! their results in tables keyed by id and never need to mutate or clone the
//! tree. Every node has a span. Names are interned [`Symbol`]s.
//!
//! The parser bounds the depth of the tree by the structural limits of section
//! 1.10 (250 levels of nesting and a weight of 1,000), so a pass may recurse
//! on it, and every pass keeps a depth counter and stops at 1,000 levels.
//! Nodes that stand for a construct with an error have an `Error` kind, so a
//! tree is produced for every input and later passes report nothing about them.

use crate::token::Comment;
use hollowscript_core::{Span, Symbol};

/// The identity of a node within one file.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Hands out node ids in order.
#[derive(Debug, Default)]
pub struct NodeIds {
    next: u32,
}

impl NodeIds {
    /// A generator that starts at 0.
    #[must_use]
    pub fn new() -> NodeIds {
        NodeIds::default()
    }

    /// The next id.
    pub fn next_id(&mut self) -> NodeId {
        let id = NodeId(self.next);
        self.next = self.next.saturating_add(1);
        id
    }

    /// How many ids were handed out.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.next
    }
}

/// A name and where it was written.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    /// The interned name.
    pub name: Symbol,
    /// The span of the name.
    pub span: Span,
}

/// A source file after parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    /// The `uses` lines, which come first.
    pub uses: Vec<UsesLine>,
    /// The `import` lines, which come next.
    pub imports: Vec<ImportDecl>,
    /// Everything else, in source order.
    pub items: Vec<Item>,
    /// Every comment, in source order.
    pub comments: Vec<Comment>,
    /// The number of node ids used.
    pub node_count: u32,
    /// The span of the whole file.
    pub span: Span,
}

/// `uses print, clock`.
#[derive(Clone, Debug, PartialEq)]
pub struct UsesLine {
    /// The node id.
    pub id: NodeId,
    /// The span of the line without its terminator.
    pub span: Span,
    /// The capability names.
    pub names: Vec<Ident>,
}

/// `import a, b from "./m"`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportDecl {
    /// The node id.
    pub id: NodeId,
    /// The span of the line without its terminator.
    pub span: Span,
    /// The imported names.
    pub names: Vec<Ident>,
    /// The path literal. The checker requires that it has no interpolation.
    pub path: StringLiteral,
}

/// A top-level item.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The node id.
    pub id: NodeId,
    /// The span of the item, `export` included.
    pub span: Span,
    /// The span of the `export` keyword when there is one.
    pub export: Option<Span>,
    /// What the item is.
    pub kind: ItemKind,
}

/// The kinds of top-level item.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    /// `fn name(...) { ... }`.
    Fn(FnDecl),
    /// `record Name = { ... }`.
    Record(RecordDecl),
    /// `enum Name { ... }`.
    Enum(EnumDecl),
    /// `constant x = e`.
    Constant(BindingDecl),
    /// `variable x = e`.
    Variable(BindingDecl),
    /// Any other statement.
    Stmt(Stmt),
}

/// A function declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    /// The name.
    pub name: Ident,
    /// The type parameters.
    pub type_params: Vec<Ident>,
    /// The parameters.
    pub params: Vec<Param>,
    /// The declared return type, if the function returns a value.
    pub ret: Option<TypeExpr>,
    /// The body.
    pub body: Block,
}

/// A parameter of a function declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// The name.
    pub name: Ident,
    /// The declared type.
    pub ty: TypeExpr,
}

/// A record declaration. A record is a name for a structural type.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordDecl {
    /// The name.
    pub name: Ident,
    /// The type parameters.
    pub type_params: Vec<Ident>,
    /// The fields.
    pub fields: Vec<FieldType>,
    /// The span of the braces.
    pub braces: Span,
}

/// An enum declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumDecl {
    /// The name.
    pub name: Ident,
    /// The type parameters.
    pub type_params: Vec<Ident>,
    /// The variants.
    pub variants: Vec<VariantDecl>,
    /// The span of the braces.
    pub braces: Span,
}

/// A variant of an enum declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct VariantDecl {
    /// The name, which starts with an upper case letter (checked after parsing).
    pub name: Ident,
    /// The payload types.
    pub payload: Vec<TypeExpr>,
}

/// A `constant` or `variable` declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct BindingDecl {
    /// What is bound.
    pub target: BindingTarget,
    /// The annotation.
    pub ty: Option<TypeExpr>,
    /// The initialiser.
    pub init: Expr,
}

/// What a declaration or a `for` binds.
#[derive(Clone, Debug, PartialEq)]
pub enum BindingTarget {
    /// One name, or the discard name `_`.
    Name(Ident),
    /// A record or list pattern, which the checker restricts to declaration patterns.
    Pattern(Pattern),
}

/// A field of a record type.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldType {
    /// The field name.
    pub name: Ident,
    /// Its type.
    pub ty: TypeExpr,
}

/// A type as written.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    /// The node id.
    pub id: NodeId,
    /// The span.
    pub span: Span,
    /// What kind of type.
    pub kind: TypeKind,
}

/// The kinds of type expression.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// A name with optional type arguments, such as `Int` or `Map<String, Int>`.
    Named {
        /// The type name.
        name: Ident,
        /// The type arguments.
        args: Vec<TypeExpr>,
    },
    /// `[T]`.
    List(Box<TypeExpr>),
    /// `T?`. Written at most once (HS0111).
    Optional(Box<TypeExpr>),
    /// `(T)`, kept so that `(Int?)?` can be diagnosed and formatted.
    Paren(Box<TypeExpr>),
    /// `{ a: A, b: B }`.
    Record(Vec<FieldType>),
    /// `fn(A, B): C`, and `effect fn(A)`.
    Fn {
        /// Whether the type is effectful.
        effect: bool,
        /// The parameter types.
        params: Vec<TypeExpr>,
        /// The return type, if any.
        ret: Option<Box<TypeExpr>>,
    },
    /// A malformed type.
    Error,
}

/// A block of statements.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// The span from `{` to `}`.
    pub span: Span,
    /// The statements.
    pub stmts: Vec<Stmt>,
}

/// A statement.
#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    /// The node id.
    pub id: NodeId,
    /// The span.
    pub span: Span,
    /// What kind of statement.
    pub kind: StmtKind,
}

/// The kinds of statement.
#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    /// `constant`.
    Constant(BindingDecl),
    /// `variable`.
    Variable(BindingDecl),
    /// `name = expression`.
    Assign {
        /// The variable assigned.
        target: Ident,
        /// The value.
        value: Expr,
    },
    /// `if`, with its `else if` run and `else`.
    If(IfStmt),
    /// `match`.
    Match(MatchStmt),
    /// `for target in iterable { body }`.
    For {
        /// The loop binding.
        target: BindingTarget,
        /// The collection.
        iterable: Expr,
        /// The body.
        body: Block,
    },
    /// `while condition { body }`.
    While {
        /// The condition.
        condition: Expr,
        /// The body.
        body: Block,
    },
    /// `break`.
    Break,
    /// `continue`.
    Continue,
    /// `return` with an optional value.
    Return(Option<Expr>),
    /// A call standing alone. The parser reports HS0109 for anything else.
    Call(Expr),
    /// A declaration that is only allowed at the top level, kept for recovery (HS0117).
    Misplaced,
    /// A statement that could not be parsed. Its tokens were skipped.
    Error,
}

/// An `if` statement. The `else if` run is a chain, so a long run is not deep.
#[derive(Clone, Debug, PartialEq)]
pub struct IfStmt {
    /// The condition.
    pub condition: Expr,
    /// The block that runs when the condition is true.
    pub then: Block,
    /// What follows `else`, if anything.
    pub otherwise: Option<Box<Else>>,
}

/// What follows the `else` of an `if`.
#[derive(Clone, Debug, PartialEq)]
pub enum Else {
    /// `else if`.
    If(Box<Stmt>),
    /// `else { ... }`.
    Block(Block),
}

/// A `match` statement.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchStmt {
    /// The scrutinee.
    pub scrutinee: Expr,
    /// The arms in order.
    pub arms: Vec<MatchArm>,
    /// The `else` arm, which must be last.
    pub else_arm: Option<Block>,
}

/// One arm of a `match`: its alternatives and its block.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    /// The comma separated alternatives. An arm matches if any of them does.
    pub patterns: Vec<Pattern>,
    /// The block that runs.
    pub body: Block,
}

/// A pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    /// The node id.
    pub id: NodeId,
    /// The span.
    pub span: Span,
    /// What kind of pattern.
    pub kind: PatternKind,
}

/// The kinds of pattern (section 7.1).
#[derive(Clone, Debug, PartialEq)]
pub enum PatternKind {
    /// A literal, with a minus sign for a negative number.
    Literal {
        /// Whether a `-` precedes the number.
        negative: bool,
        /// The literal.
        literal: Literal,
    },
    /// `none`.
    None,
    /// `_`.
    Wildcard,
    /// A name that starts with a lower case letter or `_`, which binds.
    Binding(Ident),
    /// A variant such as `Circle(r)`, or `Empty`.
    Variant {
        /// The variant name.
        name: Ident,
        /// The sub-patterns.
        args: Vec<Pattern>,
        /// Whether parentheses were written.
        parens: bool,
    },
    /// `{ x, y: 0 }`.
    Record(Vec<FieldPattern>),
    /// `[a, ..rest]`.
    List(Vec<ListElement>),
    /// A malformed pattern.
    Error,
}

/// A field of a record pattern. `x` alone is `x: x`.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldPattern {
    /// The field name.
    pub name: Ident,
    /// The pattern, or `None` for the short form.
    pub pattern: Option<Pattern>,
}

/// An element of a list pattern.
#[derive(Clone, Debug, PartialEq)]
pub enum ListElement {
    /// A pattern for one element.
    Pattern(Pattern),
    /// `..` or `..name`, which may only be last (HS0121).
    Rest {
        /// The span of the rest pattern.
        span: Span,
        /// The name that binds the rest, if any.
        binding: Option<Ident>,
    },
}

/// An expression.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    /// The node id.
    pub id: NodeId,
    /// The span.
    pub span: Span,
    /// What kind of expression.
    pub kind: ExprKind,
}

/// A prefix operator.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-`.
    Neg,
    /// `not`.
    Not,
}

/// A binary operator.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    /// `or`.
    Or,
    /// `and`.
    And,
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// `/`.
    Div,
}

/// The kinds of expression.
#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// A literal.
    Literal(Literal),
    /// A name.
    Name(Ident),
    /// `[a, b]`.
    List(Vec<Expr>),
    /// `[k: v]`, or `[:]` when empty.
    Map(Vec<MapEntry>),
    /// `{ x: 1 }`.
    Record(Vec<FieldInit>),
    /// `fn(x) { ... }`.
    Closure(Closure),
    /// `(e)`, kept for the formatter and for the rule about statements.
    Paren(Box<Expr>),
    /// A prefix operator.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
    },
    /// A binary operator. Operators are left associative, so a chain leans left.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left operand.
        lhs: Box<Expr>,
        /// The right operand.
        rhs: Box<Expr>,
    },
    /// `effect call`.
    Effect(Box<Expr>),
    /// `callee(args)`.
    Call {
        /// The callee.
        callee: Box<Expr>,
        /// The arguments.
        args: Vec<Expr>,
    },
    /// `target[index]`.
    Index {
        /// The indexed expression.
        target: Box<Expr>,
        /// The index.
        index: Box<Expr>,
    },
    /// `target.name`.
    Field {
        /// The record, namespace or method receiver.
        target: Box<Expr>,
        /// The field or method name.
        name: Ident,
    },
    /// `operand?`.
    Try(Box<Expr>),
    /// A malformed expression.
    Error,
}

/// An entry of a map literal.
#[derive(Clone, Debug, PartialEq)]
pub struct MapEntry {
    /// The key.
    pub key: Expr,
    /// The value.
    pub value: Expr,
}

/// A field of a record literal.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldInit {
    /// The field name.
    pub name: Ident,
    /// The value.
    pub value: Expr,
}

/// A closure literal.
#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    /// The parameters, whose types may be left out.
    pub params: Vec<ClosureParam>,
    /// The declared return type.
    pub ret: Option<TypeExpr>,
    /// The body.
    pub body: Block,
}

/// A parameter of a closure.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosureParam {
    /// The name.
    pub name: Ident,
    /// The annotation, when written.
    pub ty: Option<TypeExpr>,
}

/// A literal.
#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    /// An `Int` literal. The value is the magnitude, which is at most 2^63; the
    /// checker accepts 2^63 only under a minus sign (HS0014).
    Int {
        /// The magnitude.
        value: u64,
        /// The literal was malformed and reported.
        malformed: bool,
    },
    /// A `Float` literal, the binary64 value nearest to the text.
    Float {
        /// The value, finite.
        value: f64,
        /// The literal was malformed and reported.
        malformed: bool,
    },
    /// `true` or `false`.
    Bool(bool),
    /// `none`.
    None,
    /// A string.
    String(StringLiteral),
    /// A filter literal.
    Filter {
        /// The filter source with `\`` and `\\` unescaped.
        source: String,
        /// The literal was malformed and reported.
        malformed: bool,
    },
}

/// A string literal: text parts and interpolations.
#[derive(Clone, Debug, PartialEq)]
pub struct StringLiteral {
    /// The span of the literal, quotes included.
    pub span: Span,
    /// The parts in order.
    pub parts: Vec<StringPart>,
    /// The literal was malformed and reported.
    pub malformed: bool,
}

/// A part of a string.
#[derive(Clone, Debug, PartialEq)]
pub enum StringPart {
    /// Literal text with its escapes replaced.
    Text(String),
    /// `${expression}`.
    Interpolation(Box<Expr>),
}

#[cfg(test)]
mod tests {
    use super::NodeIds;

    #[test]
    fn node_ids_count_up() {
        let mut ids = NodeIds::new();
        assert_eq!(ids.next_id().0, 0);
        assert_eq!(ids.next_id().0, 1);
        assert_eq!(ids.count(), 2);
    }
}
