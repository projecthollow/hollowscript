//! The parser (chapter 18).
//!
//! A recursive descent parser over the token stream. It never fails: for every
//! input it returns a tree and diagnostics. Recovery follows section 18.9. At
//! most one syntax diagnostic is reported for each statement or top-level item,
//! and after an error the parser skips tokens to the next terminator or the
//! closing brace of the enclosing block. Chains of operators, postfix
//! operations and `else if` runs are read with loops, so the depth of its
//! recursion is bounded by the nesting limit of 250.
//!
//! Recovery is implemented with a small `Result<T, Recover>`: a failing
//! sub-parse reports at most one diagnostic (through the parser's internal
//! `fail` helper, which is a no-op after the first call for the current
//! statement or top-level item) and returns `Err(Recover)` without consuming
//! the offending token.
//! Whoever called it either plugs an `Error` node into a larger construct it
//! can still finish building (so `constant a = )` keeps the declaration of
//! `a`, matching the corpus case `diagnostics/resynchronise`), or, if there is
//! no useful larger construct, lets the failure reach the statement loop,
//! which then performs the skip described in section 18.9.

use crate::ast::{
    BindingDecl, BindingTarget, Block, Closure, ClosureParam, Else, EnumDecl, Expr, ExprKind,
    FieldInit, FieldPattern, FieldType, File, FnDecl, Ident, IfStmt, ImportDecl, Item, ItemKind,
    ListElement, Literal, MapEntry, MatchArm, MatchStmt, NodeId, NodeIds, Param, Pattern,
    PatternKind, RecordDecl, Stmt, StmtKind, StringLiteral, StringPart, TypeExpr, TypeKind,
    UnaryOp, UsesLine, VariantDecl,
};
use crate::budget::{Depth, chain_weight, check_chain, check_weight};
use crate::lexer::lex;
use crate::token::{Comment, Keyword, Token, TokenKind, decode_filter_source, decode_string_text};
use hollowscript_core::limits::structural::{MAX_CHAIN_LINKS, MAX_NESTING_DEPTH, MAX_TOKEN_WEIGHT};
use hollowscript_core::{
    BoundedDiagnostics, Code, Control, Diagnostic, Interner, PositionCursor, Source, Span,
};
use std::fmt::Display;

/// The result of parsing one source.
#[derive(Clone, Debug, PartialEq)]
pub struct ParseOutput {
    /// The tree.
    pub file: File,
    /// The lexical and syntax diagnostics, in the order found, at most 101 of
    /// them (see [`crate::lexer::LexOutput::diagnostics`]).
    pub diagnostics: Vec<Diagnostic>,
}

/// Marks that a sub-parse failed and a diagnostic was reported (or already
/// had been, for this statement). Carries no data; the parser rebuilds an
/// `Error` node itself at the point that catches it.
struct Recover;

type PResult<T> = Result<T, Recover>;

/// Parses a source.
///
/// The parser recurses up to the nesting-depth limit of section 1.10 (250
/// levels of parentheses, brackets, braces, blocks, patterns, type arguments
/// or interpolations, each several calls deep through the precedence
/// ladder), which an unoptimised build can use more native stack for than a
/// constrained thread provides. Section 1.10 requires that a construct
/// within the limits "MUST NOT exhaust the host's stack", and that must hold
/// whatever stack the caller happens to be running on, so parsing runs on
/// [`hollowscript_core::run_on_dedicated_stack`]'s dedicated thread instead
/// of trusting the caller's own.
///
/// [`crate::format::format`] runs on the same dedicated thread for the same
/// reason: the formatter walks the same tree with the same kind of
/// unbounded recursion. So, outside this crate, does checking and
/// compiling: see `hollowscript_core::stack`'s own module documentation for
/// why one size serves every pass.
#[must_use]
pub fn parse(source: &Source, interner: &mut Interner, control: &mut Control<'_>) -> ParseOutput {
    let lexed = lex(source, control);
    let run = move || {
        let mut diagnostics = BoundedDiagnostics::new();
        diagnostics.extend(lexed.diagnostics);
        let mut parser = Parser {
            tokens: &lexed.tokens,
            pos: 0,
            source,
            interner,
            diagnostics,
            comments: lexed.comments,
            ids: NodeIds::new(),
            depth: Depth::default(),
            block_depth: 0,
            error_in_unit: false,
            restrict_record: false,
            control,
            nodes_since_safepoint: 0,
            cancelled: false,
            position_cursor: PositionCursor::new(),
        };
        let file = parser.parse_file();
        ParseOutput {
            file,
            diagnostics: parser.diagnostics.into_vec(),
        }
    };
    match hollowscript_core::run_on_dedicated_stack(run) {
        Ok(output) => output,
        // The parser must never panic; a `Panicked` here only guards
        // against a defect elsewhere (or in a future change) actually doing
        // so. `SpawnFailed` means the host could not create the thread (for
        // example it is out of resources for new threads), not a case the
        // parser can usefully recover from on its own account. Both are
        // reported the same way as any other internal failure rather than
        // risking a stack this function cannot bound on the calling thread.
        Err(hollowscript_core::DedicatedStackError::Panicked) => {
            internal_error_output(source, "the parser did not complete")
        }
        Err(hollowscript_core::DedicatedStackError::SpawnFailed) => {
            internal_error_output(source, "could not start the parser")
        }
    }
}

fn internal_error_output(source: &Source, message: &str) -> ParseOutput {
    let end = u32::try_from(source.len()).unwrap_or(u32::MAX);
    ParseOutput {
        file: File {
            uses: Vec::new(),
            imports: Vec::new(),
            items: Vec::new(),
            comments: Vec::new(),
            node_count: 0,
            span: Span::new(0, end),
        },
        diagnostics: vec![
            Diagnostic::new(Code::HS1099, &[("message", &message)])
                .with_location(source.locate(Span::empty(0))),
        ],
    }
}

struct Parser<'p, 'c> {
    tokens: &'p [Token],
    pos: usize,
    source: &'p Source,
    interner: &'p mut Interner,
    diagnostics: BoundedDiagnostics,
    /// Every comment the lexer saw, in source order, carried through to the
    /// tree unattached (`File::comments`): the parser has no use for a
    /// comment itself, but the formatter, the one consumer that must not
    /// lose one, finds them there.
    comments: Vec<Comment>,
    ids: NodeIds,
    depth: Depth,
    /// Nesting of `block`s specifically (fn/closure bodies, `if`/`while`/`for`
    /// bodies, `match` arm bodies), used to decide whether recovery may stop
    /// at an enclosing `}` (section 18.9).
    block_depth: u32,
    /// Whether a syntax diagnostic has already been reported for the
    /// statement or top-level item currently being parsed.
    error_in_unit: bool,
    /// Whether the primary expression about to be parsed would be a record
    /// literal that section 3.8 forbids here (HS0110). Saved and cleared
    /// around any nested expression that is unambiguous (parentheses,
    /// brackets, argument lists, interpolations).
    restrict_record: bool,
    control: &'p mut Control<'c>,
    nodes_since_safepoint: u32,
    cancelled: bool,
    /// Resumes each diagnostic's column count from the previous one instead
    /// of the start of its line, the same way and for the same reason as the
    /// lexer's own (see `Lexer::position_cursor`).
    position_cursor: PositionCursor,
}

impl<'p> Parser<'p, '_> {
    // ---- token access -----------------------------------------------

    fn cur(&self) -> Token {
        let index = self.pos.min(self.tokens.len().saturating_sub(1));
        // The lexer always yields at least one token (`Eof`), so `tokens` is
        // never empty in practice; the fallback keeps this total without an
        // `unwrap`/`expect` if that guarantee were ever broken.
        self.tokens.get(index).copied().unwrap_or_else(|| Token {
            kind: TokenKind::Eof,
            span: Span::empty(0),
            malformed: false,
        })
    }

    fn cur_kind(&self) -> TokenKind {
        self.cur().kind
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.cur_kind() == kind
    }

    fn at_kw(&self, kw: Keyword) -> bool {
        self.cur_kind() == TokenKind::Keyword(kw)
    }

    fn bump(&mut self) -> Token {
        let t = self.cur();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn skip_terms(&mut self) {
        while self.at(TokenKind::Term) {
            self.bump();
        }
    }

    fn text(&self, span: Span) -> &'p str {
        self.source.slice(span).unwrap_or("")
    }

    fn next_id(&mut self) -> NodeId {
        self.ids.next_id()
    }

    fn safepoint(&mut self) {
        self.nodes_since_safepoint = self.nodes_since_safepoint.saturating_add(1);
        if self.nodes_since_safepoint
            >= hollowscript_core::limits::structural::FRONT_END_NODE_INTERVAL
        {
            self.nodes_since_safepoint = 0;
            if self.control.safepoint().is_err() {
                self.cancelled = true;
            }
        }
    }

    /// Whether the token stream has nothing further worth trying: either the
    /// end of the file, or the run was cancelled by the host.
    fn exhausted(&self) -> bool {
        self.cancelled || self.at(TokenKind::Eof)
    }

    // ---- diagnostics --------------------------------------------------

    fn push_diag(&mut self, code: Code, args: &[(&str, &dyn Display)], span: Span) {
        let location = self.source.locate_with(span, &mut self.position_cursor);
        self.diagnostics
            .push(Diagnostic::new(code, args).with_location(location));
    }

    /// Reports a diagnostic unless one has already been reported for the
    /// current statement or top-level item, and returns the recovery marker.
    fn fail(&mut self, code: Code, args: &[(&str, &dyn Display)], span: Span) -> Recover {
        if !self.error_in_unit {
            self.push_diag(code, args, span);
            self.error_in_unit = true;
        }
        Recover
    }

    fn describe(&self, t: Token) -> String {
        match t.kind {
            TokenKind::Eof => "the end of the file".to_string(),
            TokenKind::Term => "the end of the line".to_string(),
            TokenKind::Keyword(k) => format!("'{}'", k.as_str()),
            TokenKind::Ident | TokenKind::Int | TokenKind::Float => {
                format!("'{}'", self.text(t.span))
            }
            TokenKind::String
            | TokenKind::StringHead
            | TokenKind::StringMiddle
            | TokenKind::StringTail => "a string".to_string(),
            TokenKind::Filter => "a filter literal".to_string(),
            TokenKind::LParen => "'('".to_string(),
            TokenKind::RParen => "')'".to_string(),
            TokenKind::LBracket => "'['".to_string(),
            TokenKind::RBracket => "']'".to_string(),
            TokenKind::LBrace => "'{'".to_string(),
            TokenKind::RBrace => "'}'".to_string(),
            TokenKind::Comma => "','".to_string(),
            TokenKind::Colon => "':'".to_string(),
            TokenKind::Dot => "'.'".to_string(),
            TokenKind::DotDot => "'..'".to_string(),
            TokenKind::Question => "'?'".to_string(),
            TokenKind::Plus => "'+'".to_string(),
            TokenKind::Minus => "'-'".to_string(),
            TokenKind::Star => "'*'".to_string(),
            TokenKind::Slash => "'/'".to_string(),
            TokenKind::Assign => "'='".to_string(),
            TokenKind::EqEq => "'=='".to_string(),
            TokenKind::NotEq => "'!='".to_string(),
            TokenKind::Lt => "'<'".to_string(),
            TokenKind::Gt => "'>'".to_string(),
            TokenKind::LtEq => "'<='".to_string(),
            TokenKind::GtEq => "'>='".to_string(),
        }
    }

    fn found(&self) -> String {
        self.describe(self.cur())
    }

    fn fail_expected(&mut self, expected: &str) -> Recover {
        let span = self.cur().span;
        let found = self.found();
        self.fail(
            Code::HS0101,
            &[("expected", &expected), ("found", &found)],
            span,
        )
    }

    fn fail_expr(&mut self) -> Recover {
        let span = self.cur().span;
        let found = self.found();
        self.fail(Code::HS0102, &[("found", &found)], span)
    }

    fn fail_stmt(&mut self) -> Recover {
        let span = self.cur().span;
        let found = self.found();
        self.fail(Code::HS0103, &[("found", &found)], span)
    }

    fn expect(&mut self, kind: TokenKind, expected: &str) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            Err(self.fail_expected(expected))
        }
    }

    /// An identifier, with the future-reserved-word check of section 1.3
    /// (HS0104) applied wherever a name is expected.
    fn expect_ident(&mut self) -> PResult<Ident> {
        if self.at(TokenKind::Ident) {
            let malformed = self.cur().malformed;
            let t = self.bump();
            if malformed {
                // The lexer has already said what is wrong with the word
                // (HS0005); it is not a name, so it stands in as the
                // placeholder a missing name uses, and nothing downstream
                // sees a name the author did not write.
                return Ok(Ident {
                    name: self.interner.intern(""),
                    span: t.span,
                });
            }
            let name = self.interner.intern(self.text(t.span));
            return Ok(Ident { name, span: t.span });
        }
        if let TokenKind::Keyword(k) = self.cur_kind() {
            if k.is_future() {
                let span = self.cur().span;
                let word = k.as_str();
                return Err(self.fail(Code::HS0104, &[("word", &word)], span));
            }
        }
        Err(self.fail_expected("a name"))
    }

    fn error_expr(&mut self, span: Span) -> Expr {
        Expr {
            id: self.next_id(),
            span,
            kind: ExprKind::Error,
        }
    }

    fn error_pattern(&mut self, span: Span) -> Pattern {
        Pattern {
            id: self.next_id(),
            span,
            kind: PatternKind::Error,
        }
    }

    fn error_type(&mut self, span: Span) -> TypeExpr {
        TypeExpr {
            id: self.next_id(),
            span,
            kind: TypeKind::Error,
        }
    }

    /// Enters a nesting construct (section 1.10); on failure, reports HS0113
    /// and returns `Recover` without descending further.
    fn enter_depth(&mut self, span: Span) -> PResult<()> {
        if self.depth.enter().is_err() {
            return Err(self.fail(Code::HS0113, &[("limit", &MAX_NESTING_DEPTH)], span));
        }
        Ok(())
    }

    fn exit_depth(&mut self) {
        self.depth.exit();
    }

    fn check_weight_at(&mut self, span: Span, weight: u32) -> PResult<()> {
        if check_weight(self.depth, weight).is_err() {
            return Err(self.fail(Code::HS0113, &[("limit", &MAX_TOKEN_WEIGHT)], span));
        }
        Ok(())
    }

    // ---- recovery -------------------------------------------------------

    /// Skips tokens from the current position, following section 18.9,
    /// stopping just after a `TERM` at bracket depth zero (relative to the
    /// point recovery started), or just before a `}` at bracket depth zero
    /// that closes an enclosing block, or at `EOF`. Bare `}` at the top
    /// level of the file (no enclosing block) is treated as noise and
    /// skipped like any other token, since nothing above the file can use it
    /// as a resynchronisation point.
    fn recover_to_next_unit(&mut self) {
        let has_enclosing_block = self.block_depth > 0;
        let mut extra_depth: u32 = 0;
        loop {
            if self.exhausted() {
                return;
            }
            match self.cur_kind() {
                TokenKind::Term if extra_depth == 0 => {
                    self.bump();
                    return;
                }
                TokenKind::RBrace if extra_depth == 0 => {
                    if has_enclosing_block {
                        return;
                    }
                    self.bump();
                }
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    extra_depth += 1;
                    self.bump();
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    extra_depth = extra_depth.saturating_sub(1);
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// A last-resort guarantee that a statement/item/arm-collecting loop
    /// always makes progress, independent of whether every failure path
    /// above correctly recognised itself as one: if `before` still equals
    /// the current position, forces one token forward. This is what makes
    /// the parser total rather than merely "believed total": a bug in one
    /// recovery path becomes a slightly worse diagnostic, never a hang.
    fn force_progress(&mut self, before: usize) {
        if self.pos == before && !self.exhausted() {
            self.bump();
        }
    }

    // ---- file, uses, imports, top level items ---------------------------

    fn parse_file(&mut self) -> File {
        let mut uses = Vec::new();
        let mut imports = Vec::new();
        let mut items = Vec::new();

        self.skip_terms();
        while self.at_kw(Keyword::Uses) {
            self.error_in_unit = false;
            uses.push(self.parse_uses_line());
            self.skip_terms();
        }
        while self.at_kw(Keyword::Import) {
            self.error_in_unit = false;
            imports.push(self.parse_import_decl());
            self.skip_terms();
        }
        while !self.exhausted() {
            self.error_in_unit = false;
            if self.at_kw(Keyword::Uses) {
                let span = self.cur().span;
                self.fail(Code::HS0106, &[], span);
                self.recover_to_next_unit();
                self.skip_terms();
                continue;
            }
            if self.at_kw(Keyword::Import) {
                let span = self.cur().span;
                self.fail(Code::HS0107, &[], span);
                self.recover_to_next_unit();
                self.skip_terms();
                continue;
            }
            let before = self.pos;
            items.push(self.parse_top_level_item());
            if self.error_in_unit || self.pos == before {
                self.recover_to_next_unit();
            } else if self.at(TokenKind::Term) {
                self.bump();
            } else if !self.exhausted() {
                self.fail_expected("the end of the line");
                self.recover_to_next_unit();
            }
            self.force_progress(before);
            self.skip_terms();
            self.safepoint();
            if self.cancelled {
                break;
            }
        }

        let end = self.tokens.last().map_or(0, |t| t.span.end);
        File {
            uses,
            imports,
            items,
            comments: std::mem::take(&mut self.comments),
            node_count: self.ids.count(),
            span: Span::new(0, end),
        }
    }

    fn parse_uses_line(&mut self) -> UsesLine {
        let id = self.next_id();
        let start = self.cur().span;
        self.bump();
        let mut names = Vec::new();
        if let Ok(n) = self.expect_ident() {
            names.push(n);
        } else {
            self.recover_to_next_unit();
            let end = start.end;
            return UsesLine {
                id,
                span: Span::new(start.start, end),
                names,
            };
        }
        while self.at(TokenKind::Comma) {
            self.bump();
            match self.expect_ident() {
                Ok(n) => names.push(n),
                Err(_) => break,
            }
        }
        let end = names.last().map_or(start.end, |n| n.span.end);
        let span = Span::new(start.start, end);
        self.finish_line(span);
        span_or(id, span, names)
    }

    fn parse_import_decl(&mut self) -> ImportDecl {
        let id = self.next_id();
        let start = self.cur().span;
        self.bump();
        let mut names = Vec::new();
        let mut ok = true;
        if let Ok(n) = self.expect_ident() {
            names.push(n);
        } else {
            ok = false;
        }
        if ok {
            while self.at(TokenKind::Comma) {
                self.bump();
                if let Ok(n) = self.expect_ident() {
                    names.push(n);
                } else {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if self.at_kw(Keyword::From) {
                self.bump();
            } else {
                self.fail_expected("'from'");
                ok = false;
            }
        }
        let path = if ok {
            self.parse_plain_string_literal().ok()
        } else {
            None
        };
        let end = path.as_ref().map_or_else(
            || names.last().map_or(start.end, |n| n.span.end),
            |p| p.span.end,
        );
        let span = Span::new(start.start, end);
        if path.is_none() {
            self.recover_to_next_unit();
        } else {
            self.finish_line(span);
        }
        ImportDecl {
            id,
            span,
            names,
            path: path.unwrap_or_else(|| StringLiteral {
                span,
                parts: Vec::new(),
                malformed: true,
            }),
        }
    }

    /// A string literal for an import path: no interpolation is accepted
    /// here (an interpolated one is still consumed and marked malformed so
    /// recovery stays simple; the checker reports HS0602 for its shape).
    fn parse_plain_string_literal(&mut self) -> PResult<StringLiteral> {
        if !matches!(self.cur_kind(), TokenKind::String | TokenKind::StringHead) {
            return Err(self.fail_expected("a string"));
        }
        let (mut lit, _w) = self.parse_string_literal_raw()?;
        if lit.parts.len() > 1 {
            lit.malformed = true;
        }
        Ok(lit)
    }

    /// Ends a `uses` or `import` line: consumes a `TERM` if present (one
    /// should always be there once the lexer has run, but a truncated or
    /// fuzzed file may end right after the line with no line terminator, in
    /// which case the trailing `TERM` the lexer inserts at `EOF` covers it).
    fn finish_line(&mut self, span: Span) {
        if self.at(TokenKind::Term) {
            self.bump();
        } else if !self.exhausted() {
            self.fail_expected("the end of the line");
            self.recover_to_next_unit();
        }
        let _ = span;
    }

    fn parse_top_level_item(&mut self) -> Item {
        let export = if self.at_kw(Keyword::Export) {
            Some(self.bump().span)
        } else {
            None
        };
        let start = export.unwrap_or(self.cur().span);
        let kind = match self.cur_kind() {
            TokenKind::Keyword(Keyword::Fn) if self.peek_is_ident_after_fn() => {
                ItemKind::Fn(self.parse_fn_decl())
            }
            TokenKind::Keyword(Keyword::Record) => ItemKind::Record(self.parse_record_decl()),
            TokenKind::Keyword(Keyword::Enum) => ItemKind::Enum(self.parse_enum_decl()),
            TokenKind::Keyword(Keyword::Constant) => ItemKind::Constant(self.parse_binding_decl()),
            TokenKind::Keyword(Keyword::Variable) => ItemKind::Variable(self.parse_binding_decl()),
            _ => {
                if matches!(self.cur_kind(), TokenKind::Keyword(k) if matches!(k, Keyword::Fn | Keyword::Import | Keyword::Uses))
                {
                    // A misplaced nested-only declaration keyword reaching
                    // here (fn without a following name is a closure, handled
                    // as a statement below instead).
                }
                ItemKind::Stmt(self.parse_statement())
            }
        };
        let end = Self::item_kind_end(&kind);
        let span = Span::new(start.start, end);
        // Section 10.4: `export` may prefix `fn`, `record`, `enum` and
        // `constant`, but a `constant` it prefixes must still name a single
        // identifier; `export` on a record or list pattern's `constant` is
        // refused exactly as `export variable` is, and for the same reason
        // given below.
        let exported_destructuring_constant = matches!(
            &kind,
            ItemKind::Constant(decl) if matches!(decl.target, BindingTarget::Pattern(_))
        );
        if export.is_some()
            && (exported_destructuring_constant
                || !matches!(
                    kind,
                    ItemKind::Fn(_)
                        | ItemKind::Record(_)
                        | ItemKind::Enum(_)
                        | ItemKind::Constant(_)
                ))
        {
            let kind_word = match &kind {
                ItemKind::Variable(_) => "variable",
                ItemKind::Constant(_) => "a destructuring constant",
                _ => "this statement",
            };
            self.push_diag(Code::HS0108, &[("kind", &kind_word)], start);
            // The `export` was refused, so the item is not exported and
            // nothing downstream should treat it as though it were: section
            // 17.3 does not warn about an unused *exported* declaration,
            // and this one is not one.
            return Item {
                id: self.next_id(),
                span,
                export: None,
                kind,
            };
        }
        Item {
            id: self.next_id(),
            span,
            export,
            kind,
        }
    }

    fn item_kind_end(kind: &ItemKind) -> u32 {
        match kind {
            ItemKind::Fn(f) => f.body.span.end,
            ItemKind::Record(r) => r.braces.end,
            ItemKind::Enum(e) => e.braces.end,
            ItemKind::Constant(b) | ItemKind::Variable(b) => b.init.span.end,
            ItemKind::Stmt(s) => s.span.end,
        }
    }

    /// `fn` begins a declaration only when followed by a name; `fn (` is a
    /// closure expression (section 18.3).
    fn peek_is_ident_after_fn(&self) -> bool {
        let t = self.tokens.get(self.pos + 1).copied().unwrap_or(self.cur());
        t.kind == TokenKind::Ident
    }

    // ---- declarations -----------------------------------------------

    fn parse_fn_decl(&mut self) -> FnDecl {
        self.bump(); // 'fn'
        let name = self
            .expect_ident()
            .unwrap_or_else(|_| self.placeholder_ident());
        let type_params = self.parse_optional_type_params();
        let params = self.parse_params().unwrap_or_default();
        let ret = self.parse_optional_return_type();
        let body = self.parse_block();
        FnDecl {
            name,
            type_params,
            params,
            ret,
            body,
        }
    }

    fn placeholder_ident(&mut self) -> Ident {
        let span = self.cur().span;
        Ident {
            name: self.interner.intern(""),
            span,
        }
    }

    fn parse_optional_type_params(&mut self) -> Vec<Ident> {
        if !self.at(TokenKind::Lt) {
            return Vec::new();
        }
        let open = self.cur().span;
        if self.enter_depth(open).is_err() {
            self.recover_to_next_unit();
            return Vec::new();
        }
        self.bump();
        let mut params = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::Gt) || self.exhausted() {
                break;
            }
            match self.expect_ident() {
                Ok(id) => params.push(id),
                Err(_) => break,
            }
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        if self.at(TokenKind::Gt) {
            self.bump();
        } else {
            self.fail_expected("'>'");
        }
        self.exit_depth();
        params
    }

    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        let open = self.expect(TokenKind::LParen, "'('")?;
        self.enter_depth(open.span)?;
        let mut params = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::RParen) || self.exhausted() {
                break;
            }
            let name = self.expect_ident();
            let name = match name {
                Ok(n) => n,
                Err(e) => {
                    self.exit_depth();
                    return Err(e);
                }
            };
            if self.expect(TokenKind::Colon, "':'").is_err() {
                self.exit_depth();
                return Err(Recover);
            }
            let ty = self
                .parse_type()
                .unwrap_or_else(|_| self.error_type(self.cur().span));
            params.push(Param { name, ty });
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        let closed = self.expect(TokenKind::RParen, "')'");
        self.exit_depth();
        closed?;
        Ok(params)
    }

    fn parse_optional_return_type(&mut self) -> Option<TypeExpr> {
        if self.at(TokenKind::Colon) {
            self.bump();
            Some(
                self.parse_type()
                    .unwrap_or_else(|_| self.error_type(self.cur().span)),
            )
        } else {
            None
        }
    }

    fn parse_record_decl(&mut self) -> RecordDecl {
        self.bump(); // 'record'
        let name = self
            .expect_ident()
            .unwrap_or_else(|_| self.placeholder_ident());
        let type_params = self.parse_optional_type_params();
        if self.expect(TokenKind::Assign, "'='").is_err() {
            self.recover_to_next_unit();
            return RecordDecl {
                name,
                type_params,
                fields: Vec::new(),
                braces: name.span,
            };
        }
        match self.parse_record_type_fields() {
            Ok((fields, braces)) => RecordDecl {
                name,
                type_params,
                fields,
                braces,
            },
            Err(_) => RecordDecl {
                name,
                type_params,
                fields: Vec::new(),
                braces: name.span,
            },
        }
    }

    fn parse_record_type_fields(&mut self) -> PResult<(Vec<FieldType>, Span)> {
        let open = self.expect(TokenKind::LBrace, "'{'")?;
        self.enter_depth(open.span)?;
        let mut fields = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::RBrace) || self.exhausted() {
                break;
            }
            let name = match self.expect_ident() {
                Ok(n) => n,
                Err(e) => {
                    self.exit_depth();
                    return Err(e);
                }
            };
            if self.text(name.span) == "_" {
                self.push_diag(Code::HS0123, &[], name.span);
            }
            if self.expect(TokenKind::Colon, "':'").is_err() {
                self.exit_depth();
                return Err(Recover);
            }
            let ty = self
                .parse_type()
                .unwrap_or_else(|_| self.error_type(self.cur().span));
            fields.push(FieldType { name, ty });
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        let close = self.expect(TokenKind::RBrace, "'}'");
        self.exit_depth();
        let close = close?;
        let span = Span::new(open.span.start, close.span.end);
        if fields.is_empty() {
            self.push_diag(
                Code::HS0112,
                &[("kind", &"record"), ("member", &"field")],
                span,
            );
        }
        Ok((fields, span))
    }

    fn parse_enum_decl(&mut self) -> EnumDecl {
        self.bump(); // 'enum'
        let name = self
            .expect_ident()
            .unwrap_or_else(|_| self.placeholder_ident());
        let type_params = self.parse_optional_type_params();
        if let Ok((variants, braces)) = self.parse_enum_body() {
            EnumDecl {
                name,
                type_params,
                variants,
                braces,
            }
        } else {
            self.recover_to_next_unit();
            EnumDecl {
                name,
                type_params,
                variants: Vec::new(),
                braces: name.span,
            }
        }
    }

    fn parse_enum_body(&mut self) -> PResult<(Vec<VariantDecl>, Span)> {
        let open = self.expect(TokenKind::LBrace, "'{'")?;
        self.enter_depth(open.span)?;
        let mut variants = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::RBrace) || self.exhausted() {
                break;
            }
            let name = match self.expect_ident() {
                Ok(n) => n,
                Err(e) => {
                    self.exit_depth();
                    return Err(e);
                }
            };
            if !self
                .text(name.span)
                .starts_with(|c: char| c.is_ascii_uppercase())
            {
                self.push_diag(Code::HS0116, &[], name.span);
            }
            let mut payload = Vec::new();
            if self.at(TokenKind::LParen) {
                let popen = self.cur().span;
                if self.enter_depth(popen).is_ok() {
                    self.bump();
                    loop {
                        self.skip_terms();
                        if self.at(TokenKind::RParen) || self.exhausted() {
                            break;
                        }
                        payload.push(
                            self.parse_type()
                                .unwrap_or_else(|_| self.error_type(self.cur().span)),
                        );
                        self.skip_terms();
                        if self.at(TokenKind::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.skip_terms();
                    if self.at(TokenKind::RParen) {
                        self.bump();
                    } else {
                        self.fail_expected("')'");
                    }
                    self.exit_depth();
                }
            }
            variants.push(VariantDecl { name, payload });
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        let close = self.expect(TokenKind::RBrace, "'}'");
        self.exit_depth();
        let close = close?;
        let span = Span::new(open.span.start, close.span.end);
        if variants.is_empty() {
            self.push_diag(
                Code::HS0112,
                &[("kind", &"enum"), ("member", &"variant")],
                span,
            );
        }
        Ok((variants, span))
    }

    fn parse_binding_decl(&mut self) -> BindingDecl {
        self.bump(); // 'constant' | 'variable'
        let target = self.parse_binding_target();
        let ty = if self.at(TokenKind::Colon) {
            self.bump();
            Some(
                self.parse_type()
                    .unwrap_or_else(|_| self.error_type(self.cur().span)),
            )
        } else {
            None
        };
        let init = if self.expect(TokenKind::Assign, "'='").is_ok() {
            self.parse_expression_or_error()
        } else {
            self.error_expr(self.cur().span)
        };
        BindingDecl { target, ty, init }
    }

    fn parse_binding_target(&mut self) -> BindingTarget {
        match self.cur_kind() {
            TokenKind::LBrace => BindingTarget::Pattern(self.parse_record_pattern()),
            TokenKind::LBracket => BindingTarget::Pattern(self.parse_list_pattern()),
            _ => match self.expect_ident() {
                Ok(id) => BindingTarget::Name(id),
                Err(_) => BindingTarget::Name(self.placeholder_ident()),
            },
        }
    }

    // ---- statements -------------------------------------------------

    fn parse_statement(&mut self) -> Stmt {
        let start = self.cur().span;
        let kind = self.parse_statement_kind();
        let end = Self::stmt_kind_end(&kind, start);
        Stmt {
            id: self.next_id(),
            span: Span::new(start.start, end),
            kind,
        }
    }

    fn stmt_kind_end(kind: &StmtKind, start: Span) -> u32 {
        match kind {
            StmtKind::Constant(b) | StmtKind::Variable(b) => b.init.span.end,
            StmtKind::Assign { value, .. } => value.span.end,
            StmtKind::If(i) => i
                .otherwise
                .as_ref()
                .map_or(i.then.span.end, |e| Self::else_end(e)),
            StmtKind::Match(m) => m.else_arm.as_ref().map_or_else(
                || {
                    m.arms
                        .last()
                        .map_or(m.scrutinee.span.end, |a| a.body.span.end)
                },
                |b| b.span.end,
            ),
            StmtKind::For { body, .. } | StmtKind::While { body, .. } => body.span.end,
            StmtKind::Return(e) => e.as_ref().map_or(start.end, |e| e.span.end),
            StmtKind::Call(e) => e.span.end,
            StmtKind::Break | StmtKind::Continue | StmtKind::Misplaced | StmtKind::Error => {
                start.end
            }
        }
    }

    fn else_end(e: &Else) -> u32 {
        match e {
            Else::If(s) => s.span.end,
            Else::Block(b) => b.span.end,
        }
    }

    fn parse_statement_kind(&mut self) -> StmtKind {
        match self.cur_kind() {
            TokenKind::Keyword(Keyword::Constant) => StmtKind::Constant(self.parse_binding_decl()),
            TokenKind::Keyword(Keyword::Variable) => StmtKind::Variable(self.parse_binding_decl()),
            TokenKind::Keyword(Keyword::If) => StmtKind::If(self.parse_if()),
            TokenKind::Keyword(Keyword::Match) => StmtKind::Match(self.parse_match()),
            TokenKind::Keyword(Keyword::For) => self.parse_for(),
            TokenKind::Keyword(Keyword::While) => self.parse_while(),
            TokenKind::Keyword(Keyword::Break) => {
                self.bump();
                StmtKind::Break
            }
            TokenKind::Keyword(Keyword::Continue) => {
                self.bump();
                StmtKind::Continue
            }
            TokenKind::Keyword(Keyword::Return) => self.parse_return(),
            TokenKind::Keyword(k)
                if matches!(k, Keyword::Record | Keyword::Enum)
                    || (k == Keyword::Fn && self.peek_is_ident_after_fn()) =>
            {
                let span = self.cur().span;
                let kind_word = match k {
                    Keyword::Fn => "fn",
                    Keyword::Record => "record",
                    _ => "enum",
                };
                match k {
                    Keyword::Fn => {
                        self.parse_fn_decl();
                    }
                    Keyword::Record => {
                        self.parse_record_decl();
                    }
                    _ => {
                        self.parse_enum_decl();
                    }
                }
                self.push_diag(Code::HS0117, &[("kind", &kind_word)], span);
                StmtKind::Misplaced
            }
            TokenKind::Keyword(k) if matches!(k, Keyword::Uses | Keyword::Import) => {
                let span = self.cur().span;
                let kind_word = if k == Keyword::Uses { "uses" } else { "import" };
                if k == Keyword::Uses {
                    self.parse_uses_line();
                } else {
                    self.parse_import_decl();
                }
                self.push_diag(Code::HS0117, &[("kind", &kind_word)], span);
                StmtKind::Misplaced
            }
            TokenKind::Ident if self.next_is_assign() => self.parse_assignment(),
            _ => self.parse_call_statement(),
        }
    }

    fn next_is_assign(&self) -> bool {
        self.tokens
            .get(self.pos + 1)
            .is_some_and(|t| t.kind == TokenKind::Assign)
    }

    fn parse_assignment(&mut self) -> StmtKind {
        let target = self
            .expect_ident()
            .unwrap_or_else(|_| self.placeholder_ident());
        self.bump(); // '='
        let value = self.parse_expression_or_error();
        StmtKind::Assign { target, value }
    }

    fn parse_call_statement(&mut self) -> StmtKind {
        if self.exhausted() {
            self.fail_stmt();
            return StmtKind::Error;
        }
        if !self.starts_expression() {
            if let TokenKind::Keyword(k) = self.cur_kind() {
                if k.is_future() {
                    let span = self.cur().span;
                    let word = k.as_str();
                    self.fail(Code::HS0104, &[("word", &word)], span);
                    return StmtKind::Error;
                }
            }
            self.fail_stmt();
            return StmtKind::Error;
        }
        match self.parse_expression() {
            Ok((expr, _w)) => {
                if is_call_like(&expr) {
                    StmtKind::Call(expr)
                } else {
                    self.push_diag(Code::HS0109, &[], expr.span);
                    StmtKind::Call(expr)
                }
            }
            Err(_) => StmtKind::Error,
        }
    }

    /// Whether the current token could begin an expression, so that a
    /// statement failing to match anything else is reported as HS0103 rather
    /// than as a bad expression somewhere inside `parse_expression`.
    fn starts_expression(&self) -> bool {
        matches!(
            self.cur_kind(),
            TokenKind::Ident
                | TokenKind::Int
                | TokenKind::Float
                | TokenKind::String
                | TokenKind::StringHead
                | TokenKind::Filter
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::Minus
                | TokenKind::Keyword(
                    Keyword::True
                        | Keyword::False
                        | Keyword::None
                        | Keyword::Not
                        | Keyword::Effect
                        | Keyword::Fn
                )
        )
    }

    /// An `if` statement, with its `else if` run read as a loop rather than
    /// by recursion (section 4.4: "an implementation MUST process them
    /// iteratively, not recursively"), counting links against the 500-link
    /// chain limit (HS0114) the same as any other chain. The links are
    /// collected first and then folded from the last backwards into the
    /// nested `Else::If` shape the AST wants, which is again a loop, not
    /// recursion.
    fn parse_if(&mut self) -> IfStmt {
        self.bump(); // 'if'
        let condition = self.parse_restricted_expression_or_error();
        let then = self.parse_block();

        let mut links: Vec<(Span, Expr, Block)> = Vec::new();
        let mut final_else: Option<Block> = None;
        loop {
            let else_after_term = self.at(TokenKind::Term)
                && matches!(
                    self.tokens.get(self.pos + 1).map(|t| t.kind),
                    Some(TokenKind::Keyword(Keyword::Else))
                );
            if !self.at_kw(Keyword::Else) && !else_after_term {
                break;
            }
            if else_after_term {
                self.bump(); // the intervening TERM
            }
            let else_span = self.cur().span;
            if else_after_term {
                self.push_diag(Code::HS0122, &[], else_span);
            }
            self.bump(); // 'else'
            if !self.at_kw(Keyword::If) {
                final_else = Some(self.parse_block());
                break;
            }
            let if_span = self.cur().span;
            self.bump(); // 'if'
            let link = u32::try_from(links.len() + 1).unwrap_or(u32::MAX);
            if check_chain(link).is_err() {
                self.fail(Code::HS0114, &[("limit", &MAX_CHAIN_LINKS)], if_span);
                // Still consume this link's condition and block so the
                // token stream stays in step, but stop growing the chain.
                // Parsing that block runs its own statement loop, which
                // resets `error_in_unit` per statement it finds clean, so
                // it is restored afterwards rather than trusting it to
                // still hold the flag `fail` just set.
                let _ = self.parse_restricted_expression_or_error();
                let _ = self.parse_block();
                self.error_in_unit = true;
                break;
            }
            let cond = self.parse_restricted_expression_or_error();
            let blk = self.parse_block();
            links.push((if_span, cond, blk));
        }

        let mut otherwise: Option<Box<Else>> = final_else.map(|b| Box::new(Else::Block(b)));
        let mut end_of_otherwise = otherwise.as_deref().map(Self::else_end);
        for (if_span, cond, blk) in links.into_iter().rev() {
            let end = end_of_otherwise.unwrap_or(blk.span.end);
            let inner = IfStmt {
                condition: cond,
                then: blk,
                otherwise,
            };
            let stmt = Stmt {
                id: self.next_id(),
                span: Span::new(if_span.start, end),
                kind: StmtKind::If(inner),
            };
            end_of_otherwise = Some(stmt.span.end);
            otherwise = Some(Box::new(Else::If(Box::new(stmt))));
        }

        IfStmt {
            condition,
            then,
            otherwise,
        }
    }

    fn parse_match(&mut self) -> MatchStmt {
        self.bump(); // 'match'
        let scrutinee = self.parse_restricted_expression_or_error();
        let mut arms = Vec::new();
        let mut else_arm = None;
        let Ok(open) = self.expect(TokenKind::LBrace, "'{'") else {
            self.recover_to_next_unit();
            return MatchStmt {
                scrutinee,
                arms,
                else_arm,
            };
        };
        if self.enter_depth(open.span).is_err() {
            return MatchStmt {
                scrutinee,
                arms,
                else_arm,
            };
        }
        self.block_depth += 1;
        self.skip_terms();
        while !self.at(TokenKind::RBrace) && !self.exhausted() {
            self.error_in_unit = false;
            let before = self.pos;
            if self.at_kw(Keyword::Else) {
                self.bump();
                let block = self.parse_block();
                else_arm = Some(block);
                self.skip_terms();
                if !self.at(TokenKind::RBrace) {
                    self.fail_expected("'}'");
                    self.recover_within_match();
                }
                self.force_progress(before);
                break;
            }
            match self.parse_match_arm() {
                Ok(arm) => arms.push(arm),
                Err(_) => self.recover_within_match(),
            }
            self.force_progress(before);
            self.skip_terms();
            self.safepoint();
            if self.cancelled {
                break;
            }
        }
        self.skip_terms();
        if self.at(TokenKind::RBrace) {
            self.bump();
        } else if !self.exhausted() {
            self.fail_expected("'}'");
        }
        self.block_depth -= 1;
        self.exit_depth();
        MatchStmt {
            scrutinee,
            arms,
            else_arm,
        }
    }

    /// Recovery for a broken match arm: skip to the next `TERM` or the
    /// match's own closing `}`, treating the match body as a block for the
    /// purposes of section 18.9's stop condition.
    fn recover_within_match(&mut self) {
        let mut extra_depth: u32 = 0;
        loop {
            if self.exhausted() {
                return;
            }
            match self.cur_kind() {
                TokenKind::Term if extra_depth == 0 => {
                    self.bump();
                    return;
                }
                TokenKind::RBrace if extra_depth == 0 => return,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    extra_depth += 1;
                    self.bump();
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    extra_depth = extra_depth.saturating_sub(1);
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn parse_match_arm(&mut self) -> PResult<MatchArm> {
        let mut patterns = vec![self.parse_pattern()?];
        while self.at(TokenKind::Comma) {
            self.bump();
            patterns.push(self.parse_pattern()?);
        }
        let body = self.parse_block_result()?;
        Ok(MatchArm { patterns, body })
    }

    fn parse_for(&mut self) -> StmtKind {
        self.bump(); // 'for'
        let target = self.parse_binding_target();
        if self.at_kw(Keyword::In) {
            self.bump();
        } else {
            self.fail_expected("'in'");
        }
        let iterable = self.parse_restricted_expression_or_error();
        self.block_depth += 1;
        let body = self.parse_block();
        self.block_depth -= 1;
        StmtKind::For {
            target,
            iterable,
            body,
        }
    }

    fn parse_while(&mut self) -> StmtKind {
        self.bump(); // 'while'
        let condition = self.parse_restricted_expression_or_error();
        self.block_depth += 1;
        let body = self.parse_block();
        self.block_depth -= 1;
        StmtKind::While { condition, body }
    }

    fn parse_return(&mut self) -> StmtKind {
        let kw_span = self.cur().span;
        self.bump();
        // `return` on its own line, or immediately followed by a terminator
        // or a closing delimiter, has no value.
        if self.at(TokenKind::Term) || self.at(TokenKind::RBrace) || self.exhausted() {
            return StmtKind::Return(None);
        }
        if !self.starts_expression() {
            self.fail_expr();
            let _ = kw_span;
            return StmtKind::Return(None);
        }
        StmtKind::Return(Some(self.parse_expression_or_error()))
    }

    fn parse_expression_or_error(&mut self) -> Expr {
        match self.parse_expression() {
            Ok((e, _)) => e,
            Err(_) => self.error_expr(self.cur().span),
        }
    }

    fn parse_restricted_expression_or_error(&mut self) -> Expr {
        let saved = self.restrict_record;
        self.restrict_record = true;
        let result = self.parse_expression();
        self.restrict_record = saved;
        match result {
            Ok((e, _)) => e,
            Err(_) => self.error_expr(self.cur().span),
        }
    }

    // ---- blocks -------------------------------------------------------

    fn parse_block(&mut self) -> Block {
        self.parse_block_result().unwrap_or_else(|_| Block {
            span: self.cur().span,
            stmts: Vec::new(),
        })
    }

    fn parse_block_result(&mut self) -> PResult<Block> {
        let open = self.expect(TokenKind::LBrace, "'{'")?;
        if self.enter_depth(open.span).is_err() {
            return Ok(Block {
                span: open.span,
                stmts: Vec::new(),
            });
        }
        self.block_depth += 1;
        let mut stmts = Vec::new();
        self.skip_leading_block_terms();
        while !self.at(TokenKind::RBrace) && !self.exhausted() {
            self.error_in_unit = false;
            let before = self.pos;
            stmts.push(self.parse_statement());
            if self.error_in_unit || self.pos == before {
                self.recover_to_next_unit();
            } else if self.at(TokenKind::Term) {
                self.bump();
            } else if !self.at(TokenKind::RBrace) && !self.exhausted() {
                self.fail_expected("the end of the line");
                self.recover_to_next_unit();
            }
            self.force_progress(before);
            self.safepoint();
            if self.cancelled {
                break;
            }
        }
        let close = if self.at(TokenKind::RBrace) {
            self.bump().span
        } else {
            self.fail_expected("'}'");
            self.cur().span
        };
        self.block_depth -= 1;
        self.exit_depth();
        Ok(Block {
            span: Span::new(open.span.start, close.end),
            stmts,
        })
    }

    fn skip_leading_block_terms(&mut self) {
        self.skip_terms();
    }

    // ---- patterns -------------------------------------------------------

    fn parse_pattern(&mut self) -> PResult<Pattern> {
        self.enter_depth(self.cur().span)?;
        let result = self.parse_pattern_inner();
        self.exit_depth();
        result
    }

    fn parse_pattern_inner(&mut self) -> PResult<Pattern> {
        let start = self.cur().span;
        match self.cur_kind() {
            TokenKind::Keyword(Keyword::None) => {
                self.bump();
                Ok(Pattern {
                    id: self.next_id(),
                    span: start,
                    kind: PatternKind::None,
                })
            }
            TokenKind::Ident if self.text(self.cur().span) == "_" => {
                self.bump();
                Ok(Pattern {
                    id: self.next_id(),
                    span: start,
                    kind: PatternKind::Wildcard,
                })
            }
            TokenKind::Minus => {
                self.bump();
                let (lit, end) = self.parse_literal_for_pattern()?;
                Ok(Pattern {
                    id: self.next_id(),
                    span: Span::new(start.start, end),
                    kind: PatternKind::Literal {
                        negative: true,
                        literal: lit,
                    },
                })
            }
            TokenKind::Int
            | TokenKind::Float
            | TokenKind::Keyword(Keyword::True | Keyword::False) => {
                let (lit, end) = self.parse_literal_for_pattern()?;
                Ok(Pattern {
                    id: self.next_id(),
                    span: Span::new(start.start, end),
                    kind: PatternKind::Literal {
                        negative: false,
                        literal: lit,
                    },
                })
            }
            TokenKind::String | TokenKind::StringHead => {
                let (str_lit, end) = self.parse_string_literal_raw()?;
                let mut str_lit = str_lit;
                if str_lit.parts.len() > 1 {
                    self.push_diag(
                        Code::HS0101,
                        &[
                            ("expected", &"a string with no interpolation"),
                            ("found", &"an interpolation"),
                        ],
                        str_lit.span,
                    );
                    str_lit.malformed = true;
                }
                let _ = end;
                Ok(Pattern {
                    id: self.next_id(),
                    span: str_lit.span,
                    kind: PatternKind::Literal {
                        negative: false,
                        literal: Literal::String(str_lit),
                    },
                })
            }
            TokenKind::Ident => {
                let ident = self.expect_ident()?;
                let first = self.text(ident.span).chars().next();
                if first.is_some_and(|c| c.is_ascii_uppercase()) {
                    self.parse_variant_pattern_rest(ident)
                } else {
                    if !first.is_some_and(|c| c.is_ascii_lowercase() || c == '_') {
                        self.push_diag(Code::HS0120, &[], ident.span);
                    }
                    Ok(Pattern {
                        id: self.next_id(),
                        span: ident.span,
                        kind: PatternKind::Binding(ident),
                    })
                }
            }
            TokenKind::LBrace => Ok(self.parse_record_pattern()),
            TokenKind::LBracket => Ok(self.parse_list_pattern()),
            _ => Err(self.fail_expected("a pattern")),
        }
    }

    fn parse_literal_for_pattern(&mut self) -> PResult<(Literal, u32)> {
        match self.cur_kind() {
            TokenKind::Int => {
                let t = self.bump();
                Ok((
                    Literal::Int {
                        value: parse_int_value(self.text(t.span)),
                        malformed: t.malformed,
                    },
                    t.span.end,
                ))
            }
            TokenKind::Float => {
                let t = self.bump();
                Ok((
                    Literal::Float {
                        value: parse_float_value(self.text(t.span)),
                        malformed: t.malformed,
                    },
                    t.span.end,
                ))
            }
            TokenKind::Keyword(Keyword::True) => {
                let t = self.bump();
                Ok((Literal::Bool(true), t.span.end))
            }
            TokenKind::Keyword(Keyword::False) => {
                let t = self.bump();
                Ok((Literal::Bool(false), t.span.end))
            }
            _ => Err(self.fail_expected("a number")),
        }
    }

    fn parse_variant_pattern_rest(&mut self, name: Ident) -> PResult<Pattern> {
        let mut args = Vec::new();
        let mut parens = false;
        let mut end = name.span.end;
        if self.at(TokenKind::LParen) {
            parens = true;
            let open = self.cur().span;
            self.enter_depth(open)?;
            self.bump();
            loop {
                self.skip_terms();
                if self.at(TokenKind::RParen) || self.exhausted() {
                    break;
                }
                args.push(self.parse_pattern()?);
                self.skip_terms();
                if self.at(TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.skip_terms();
            let close = self.expect(TokenKind::RParen, "')'");
            self.exit_depth();
            end = close?.span.end;
        }
        Ok(Pattern {
            id: self.next_id(),
            span: Span::new(name.span.start, end),
            kind: PatternKind::Variant { name, args, parens },
        })
    }

    fn parse_record_pattern(&mut self) -> Pattern {
        let start = self.cur().span;
        let mut fields = Vec::new();
        let result: PResult<Span> = (|| {
            let open = self.expect(TokenKind::LBrace, "'{'")?;
            loop {
                self.skip_terms();
                if self.at(TokenKind::RBrace) || self.exhausted() {
                    break;
                }
                let name = self.expect_ident()?;
                if self.text(name.span) == "_" {
                    self.push_diag(Code::HS0123, &[], name.span);
                }
                let pattern = if self.at(TokenKind::Colon) {
                    self.bump();
                    Some(self.parse_pattern()?)
                } else {
                    None
                };
                fields.push(FieldPattern { name, pattern });
                self.skip_terms();
                if self.at(TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.skip_terms();
            let close = self.expect(TokenKind::RBrace, "'}'")?;
            let _ = open;
            Ok(Span::new(start.start, close.span.end))
        })();
        match result {
            Ok(span) => Pattern {
                id: self.next_id(),
                span,
                kind: PatternKind::Record(fields),
            },
            Err(_) => self.error_pattern(Span::new(start.start, self.cur().span.end)),
        }
    }

    fn parse_list_pattern(&mut self) -> Pattern {
        let start = self.cur().span;
        let mut elements = Vec::new();
        let result: PResult<Span> = (|| {
            self.expect(TokenKind::LBracket, "'['")?;
            let mut seen_rest = false;
            loop {
                self.skip_terms();
                if self.at(TokenKind::RBracket) || self.exhausted() {
                    break;
                }
                if self.at(TokenKind::DotDot) {
                    let rspan = self.bump().span;
                    let binding = if self.at(TokenKind::Ident) {
                        Some(self.expect_ident()?)
                    } else {
                        None
                    };
                    let end = binding.map_or(rspan.end, |b| b.span.end);
                    if seen_rest {
                        self.push_diag(Code::HS0121, &[], rspan);
                    }
                    seen_rest = true;
                    elements.push(ListElement::Rest {
                        span: Span::new(rspan.start, end),
                        binding,
                    });
                } else {
                    let pattern = self.parse_pattern()?;
                    if seen_rest {
                        self.push_diag(Code::HS0121, &[], pattern.span);
                    }
                    elements.push(ListElement::Pattern(pattern));
                }
                self.skip_terms();
                if self.at(TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.skip_terms();
            let close = self.expect(TokenKind::RBracket, "']'")?;
            Ok(Span::new(start.start, close.span.end))
        })();
        match result {
            Ok(span) => Pattern {
                id: self.next_id(),
                span,
                kind: PatternKind::List(elements),
            },
            Err(_) => self.error_pattern(Span::new(start.start, self.cur().span.end)),
        }
    }

    // ---- types ----------------------------------------------------------

    fn parse_type(&mut self) -> PResult<TypeExpr> {
        self.enter_depth(self.cur().span)?;
        let result = self.parse_type_inner();
        self.exit_depth();
        result
    }

    fn parse_type_inner(&mut self) -> PResult<TypeExpr> {
        let atom = self.parse_type_atom()?;
        if !self.at(TokenKind::Question) {
            return Ok(atom);
        }
        let first_q = self.bump();
        let mut ty = TypeExpr {
            id: self.next_id(),
            span: Span::new(atom.span.start, first_q.span.end),
            kind: TypeKind::Optional(Box::new(atom)),
        };
        // A second (or later) trailing `?` is HS0111 ("write Int?, not
        // Int??"); each is still consumed, so `Int???` is one type with
        // three diagnostics rather than leaving `??` to confuse whatever
        // follows.
        while self.at(TokenKind::Question) {
            let q = self.bump();
            self.push_diag(Code::HS0111, &[], q.span);
            ty.span = Span::new(ty.span.start, q.span.end);
        }
        Ok(ty)
    }

    fn parse_type_atom(&mut self) -> PResult<TypeExpr> {
        let start = self.cur().span;
        match self.cur_kind() {
            TokenKind::LBracket => {
                self.bump();
                self.skip_terms();
                let inner = self.parse_type()?;
                self.skip_terms();
                let close = self.expect(TokenKind::RBracket, "']'")?;
                Ok(TypeExpr {
                    id: self.next_id(),
                    span: Span::new(start.start, close.span.end),
                    kind: TypeKind::List(Box::new(inner)),
                })
            }
            TokenKind::LParen => {
                self.bump();
                self.skip_terms();
                let inner = self.parse_type()?;
                self.skip_terms();
                let close = self.expect(TokenKind::RParen, "')'")?;
                Ok(TypeExpr {
                    id: self.next_id(),
                    span: Span::new(start.start, close.span.end),
                    kind: TypeKind::Paren(Box::new(inner)),
                })
            }
            TokenKind::LBrace => {
                let (fields, span) = self.parse_record_type_fields_for_type(start)?;
                Ok(TypeExpr {
                    id: self.next_id(),
                    span,
                    kind: TypeKind::Record(fields),
                })
            }
            TokenKind::Keyword(Keyword::Effect) => {
                self.bump();
                self.expect(TokenKind::Keyword(Keyword::Fn), "'fn'")?;
                self.parse_fn_type(start, true)
            }
            TokenKind::Keyword(Keyword::Fn) => {
                self.bump();
                self.parse_fn_type(start, false)
            }
            TokenKind::Ident => {
                let name = self.expect_ident()?;
                let args = if self.at(TokenKind::Lt) {
                    self.parse_type_args()?
                } else {
                    Vec::new()
                };
                let end = args.last().map_or(name.span.end, |a: &TypeExpr| a.span.end);
                Ok(TypeExpr {
                    id: self.next_id(),
                    span: Span::new(name.span.start, end.max(name.span.end)),
                    kind: TypeKind::Named { name, args },
                })
            }
            _ => Err(self.fail_expected("a type")),
        }
    }

    fn parse_record_type_fields_for_type(
        &mut self,
        start: Span,
    ) -> PResult<(Vec<FieldType>, Span)> {
        self.expect(TokenKind::LBrace, "'{'")?;
        let mut fields = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::RBrace) || self.exhausted() {
                break;
            }
            let name = self.expect_ident()?;
            if self.text(name.span) == "_" {
                self.push_diag(Code::HS0123, &[], name.span);
            }
            self.expect(TokenKind::Colon, "':'")?;
            let ty = self.parse_type()?;
            fields.push(FieldType { name, ty });
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        let close = self.expect(TokenKind::RBrace, "'}'")?;
        Ok((fields, Span::new(start.start, close.span.end)))
    }

    fn parse_fn_type(&mut self, start: Span, effect: bool) -> PResult<TypeExpr> {
        self.expect(TokenKind::LParen, "'('")?;
        let mut params = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::RParen) || self.exhausted() {
                break;
            }
            params.push(self.parse_type()?);
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        let close = self.expect(TokenKind::RParen, "')'")?;
        let mut end = close.span.end;
        let ret = if self.at(TokenKind::Colon) {
            self.bump();
            let r = self.parse_type()?;
            end = r.span.end;
            Some(Box::new(r))
        } else {
            None
        };
        Ok(TypeExpr {
            id: self.next_id(),
            span: Span::new(start.start, end),
            kind: TypeKind::Fn {
                effect,
                params,
                ret,
            },
        })
    }

    fn parse_type_args(&mut self) -> PResult<Vec<TypeExpr>> {
        let open = self.cur().span;
        self.enter_depth(open)?;
        self.bump(); // '<'
        let mut args = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::Gt) || self.exhausted() {
                break;
            }
            match self.parse_type() {
                Ok(t) => args.push(t),
                Err(e) => {
                    self.exit_depth();
                    return Err(e);
                }
            }
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        let closed = self.expect(TokenKind::Gt, "'>'");
        self.exit_depth();
        closed?;
        Ok(args)
    }

    // ---- expressions ------------------------------------------------

    fn parse_expression(&mut self) -> PResult<(Expr, u32)> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> PResult<(Expr, u32)> {
        self.parse_binary_level(Self::parse_and, |k| {
            matches!(k, TokenKind::Keyword(Keyword::Or)).then_some(crate::ast::BinaryOp::Or)
        })
    }

    fn parse_and(&mut self) -> PResult<(Expr, u32)> {
        self.parse_binary_level(Self::parse_eq, |k| {
            matches!(k, TokenKind::Keyword(Keyword::And)).then_some(crate::ast::BinaryOp::And)
        })
    }

    fn parse_eq(&mut self) -> PResult<(Expr, u32)> {
        self.parse_binary_level(Self::parse_rel, |k| match k {
            TokenKind::EqEq => Some(crate::ast::BinaryOp::Eq),
            TokenKind::NotEq => Some(crate::ast::BinaryOp::Ne),
            _ => None,
        })
    }

    fn parse_rel(&mut self) -> PResult<(Expr, u32)> {
        self.parse_binary_level(Self::parse_add, |k| match k {
            TokenKind::Lt => Some(crate::ast::BinaryOp::Lt),
            TokenKind::Gt => Some(crate::ast::BinaryOp::Gt),
            TokenKind::LtEq => Some(crate::ast::BinaryOp::Le),
            TokenKind::GtEq => Some(crate::ast::BinaryOp::Ge),
            _ => None,
        })
    }

    fn parse_add(&mut self) -> PResult<(Expr, u32)> {
        self.parse_binary_level(Self::parse_mul, |k| match k {
            TokenKind::Plus => Some(crate::ast::BinaryOp::Add),
            TokenKind::Minus => Some(crate::ast::BinaryOp::Sub),
            _ => None,
        })
    }

    fn parse_mul(&mut self) -> PResult<(Expr, u32)> {
        self.parse_binary_level(Self::parse_unary, |k| match k {
            TokenKind::Star => Some(crate::ast::BinaryOp::Mul),
            TokenKind::Slash => Some(crate::ast::BinaryOp::Div),
            _ => None,
        })
    }

    fn parse_binary_level(
        &mut self,
        next: fn(&mut Self) -> PResult<(Expr, u32)>,
        op_of: fn(TokenKind) -> Option<crate::ast::BinaryOp>,
    ) -> PResult<(Expr, u32)> {
        let (mut lhs, mut max_w) = next(self)?;
        let mut links: u32 = 0;
        while let Some(op) = op_of(self.cur_kind()) {
            self.bump();
            let (rhs, w) = next(self)?;
            links += 1;
            max_w = max_w.max(w);
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                id: self.next_id(),
                span,
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
            if check_chain(links).is_err() {
                return Err(self.fail(Code::HS0114, &[("limit", &MAX_CHAIN_LINKS)], lhs.span));
            }
        }
        let weight = chain_weight(links, max_w);
        if links > 0 {
            self.check_weight_at(lhs.span, weight)?;
        }
        Ok((lhs, weight))
    }

    fn parse_unary(&mut self) -> PResult<(Expr, u32)> {
        if self.at_kw(Keyword::Effect) {
            let start = self.cur().span;
            self.bump();
            let (operand, w) = self.parse_postfix()?;
            if !ends_in_call(&operand) {
                self.push_diag(Code::HS0403, &[], start);
            }
            let span = Span::new(start.start, operand.span.end);
            return Ok((
                Expr {
                    id: self.next_id(),
                    span,
                    kind: ExprKind::Effect(Box::new(operand)),
                },
                w,
            ));
        }
        let mut ops: Vec<(UnaryOp, Span)> = Vec::new();
        loop {
            let op = match self.cur_kind() {
                TokenKind::Keyword(Keyword::Not) => Some(UnaryOp::Not),
                TokenKind::Minus => Some(UnaryOp::Neg),
                _ => None,
            };
            let Some(op) = op else { break };
            let span = self.cur().span;
            self.bump();
            ops.push((op, span));
            if u32::try_from(ops.len()).unwrap_or(u32::MAX) > MAX_CHAIN_LINKS {
                return Err(self.fail(Code::HS0114, &[("limit", &MAX_CHAIN_LINKS)], span));
            }
        }
        let (mut operand, w) = self.parse_postfix()?;
        let links = u32::try_from(ops.len()).unwrap_or(u32::MAX);
        let weight = chain_weight(links, w);
        if links > 0 {
            let span = ops.first().map_or(operand.span, |(_, s)| *s);
            self.check_weight_at(span, weight)?;
        }
        for (op, span) in ops.into_iter().rev() {
            let full = Span::new(span.start, operand.span.end);
            operand = Expr {
                id: self.next_id(),
                span: full,
                kind: ExprKind::Unary {
                    op,
                    operand: Box::new(operand),
                },
            };
        }
        Ok((operand, weight))
    }

    fn parse_postfix(&mut self) -> PResult<(Expr, u32)> {
        let (mut expr, base_w) = self.parse_primary()?;
        let mut links: u32 = 0;
        let mut max_w = base_w;
        loop {
            match self.cur_kind() {
                TokenKind::LParen => {
                    let open = self.cur().span;
                    self.enter_depth(open)?;
                    self.bump();
                    let (args, w, close_end) = self.parse_arg_list()?;
                    self.exit_depth();
                    max_w = max_w.max(w);
                    let span = Span::new(expr.span.start, close_end);
                    expr = Expr {
                        id: self.next_id(),
                        span,
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                    };
                }
                TokenKind::LBracket => {
                    let open = self.cur().span;
                    self.enter_depth(open)?;
                    self.bump();
                    self.skip_terms();
                    let saved = self.restrict_record;
                    self.restrict_record = false;
                    let index_result = self.parse_expression();
                    self.restrict_record = saved;
                    let (index, w) = match index_result {
                        Ok(v) => v,
                        Err(e) => {
                            self.exit_depth();
                            return Err(e);
                        }
                    };
                    self.skip_terms();
                    let close = self.expect(TokenKind::RBracket, "']'");
                    self.exit_depth();
                    let close = close?;
                    max_w = max_w.max(w);
                    let span = Span::new(expr.span.start, close.span.end);
                    expr = Expr {
                        id: self.next_id(),
                        span,
                        kind: ExprKind::Index {
                            target: Box::new(expr),
                            index: Box::new(index),
                        },
                    };
                }
                TokenKind::Dot => {
                    self.bump();
                    let name = self.expect_ident()?;
                    let span = Span::new(expr.span.start, name.span.end);
                    expr = Expr {
                        id: self.next_id(),
                        span,
                        kind: ExprKind::Field {
                            target: Box::new(expr),
                            name,
                        },
                    };
                }
                TokenKind::Question => {
                    let q = self.bump();
                    let span = Span::new(expr.span.start, q.span.end);
                    expr = Expr {
                        id: self.next_id(),
                        span,
                        kind: ExprKind::Try(Box::new(expr)),
                    };
                }
                _ => break,
            }
            links += 1;
            if check_chain(links).is_err() {
                return Err(self.fail(Code::HS0114, &[("limit", &MAX_CHAIN_LINKS)], expr.span));
            }
        }
        let weight = chain_weight(links, max_w);
        if links > 0 {
            self.check_weight_at(expr.span, weight)?;
        }
        Ok((expr, weight))
    }

    /// Parses `[argList] ")"`, having already consumed the opening `(`.
    /// Returns the arguments, their maximum weight, and the end offset of
    /// the closing `)`.
    fn parse_arg_list(&mut self) -> PResult<(Vec<Expr>, u32, u32)> {
        let mut args = Vec::new();
        let mut max_w = 0u32;
        let saved = self.restrict_record;
        self.restrict_record = false;
        loop {
            self.skip_terms();
            if self.at(TokenKind::RParen) || self.exhausted() {
                break;
            }
            match self.parse_expression() {
                Ok((e, w)) => {
                    max_w = max_w.max(w);
                    args.push(e);
                }
                Err(e) => {
                    self.restrict_record = saved;
                    return Err(e);
                }
            }
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.restrict_record = saved;
        self.skip_terms();
        let close = self.expect(TokenKind::RParen, "')'")?;
        Ok((args, max_w, close.span.end))
    }

    fn parse_primary(&mut self) -> PResult<(Expr, u32)> {
        let start = self.cur().span;
        match self.cur_kind() {
            TokenKind::Int => {
                let t = self.bump();
                let lit = Literal::Int {
                    value: parse_int_value(self.text(t.span)),
                    malformed: t.malformed,
                };
                Ok((self.lit_expr(lit, t.span), 0))
            }
            TokenKind::Float => {
                let t = self.bump();
                let lit = Literal::Float {
                    value: parse_float_value(self.text(t.span)),
                    malformed: t.malformed,
                };
                Ok((self.lit_expr(lit, t.span), 0))
            }
            TokenKind::Keyword(Keyword::True) => {
                let t = self.bump();
                Ok((self.lit_expr(Literal::Bool(true), t.span), 0))
            }
            TokenKind::Keyword(Keyword::False) => {
                let t = self.bump();
                Ok((self.lit_expr(Literal::Bool(false), t.span), 0))
            }
            TokenKind::Keyword(Keyword::None) => {
                let t = self.bump();
                Ok((self.lit_expr(Literal::None, t.span), 0))
            }
            TokenKind::Filter => {
                let t = self.bump();
                let raw = self.text(t.span);
                let inner = raw
                    .strip_prefix('`')
                    .and_then(|s| s.strip_suffix('`'))
                    .unwrap_or_else(|| raw.strip_prefix('`').unwrap_or(raw));
                let source = decode_filter_source(inner);
                Ok((
                    self.lit_expr(
                        Literal::Filter {
                            source,
                            malformed: t.malformed,
                        },
                        t.span,
                    ),
                    0,
                ))
            }
            TokenKind::String | TokenKind::StringHead => {
                let (lit, w) = self.parse_string_literal_raw()?;
                let span = lit.span;
                Ok((self.lit_expr(Literal::String(lit), span), w))
            }
            TokenKind::Ident => {
                let ident = self.expect_ident()?;
                Ok((
                    Expr {
                        id: self.next_id(),
                        span: ident.span,
                        kind: ExprKind::Name(ident),
                    },
                    0,
                ))
            }
            TokenKind::LParen => {
                self.enter_depth(start)?;
                self.bump();
                self.skip_terms();
                let saved = self.restrict_record;
                self.restrict_record = false;
                let inner = self.parse_expression();
                self.restrict_record = saved;
                let (inner, w) = match inner {
                    Ok(v) => v,
                    Err(e) => {
                        self.exit_depth();
                        return Err(e);
                    }
                };
                self.skip_terms();
                let close = self.expect(TokenKind::RParen, "')'");
                self.exit_depth();
                let close = close?;
                let span = Span::new(start.start, close.span.end);
                Ok((
                    Expr {
                        id: self.next_id(),
                        span,
                        kind: ExprKind::Paren(Box::new(inner)),
                    },
                    w,
                ))
            }
            TokenKind::LBracket => self.parse_list_or_map_literal(start),
            TokenKind::LBrace => {
                if self.restrict_record {
                    self.restrict_record = false;
                    // Report once (section 18.9's cap still applies), but
                    // still parse and consume the record literal as such:
                    // otherwise its own `{` `}` is left in the stream and
                    // gets mistaken for the block that must follow this
                    // condition, cascading into unrelated errors.
                    self.fail(Code::HS0110, &[], start);
                    self.parse_record_literal(start)
                } else {
                    self.parse_record_literal(start)
                }
            }
            TokenKind::Keyword(Keyword::Fn) => self.parse_closure(start),
            // A malformed `StringMiddle`/`StringTail` here means the lexer
            // already abandoned an unterminated interpolation (HS0007) right
            // where this expression needed an operand: that diagnostic
            // already explains the problem, so the parser recovers quietly
            // rather than adding "expected an expression" on top of it.
            TokenKind::StringMiddle | TokenKind::StringTail if self.cur().malformed => {
                self.error_in_unit = true;
                Err(Recover)
            }
            _ => Err(self.fail_expr()),
        }
    }

    fn lit_expr(&mut self, lit: Literal, span: Span) -> Expr {
        Expr {
            id: self.next_id(),
            span,
            kind: ExprKind::Literal(lit),
        }
    }

    fn parse_list_or_map_literal(&mut self, start: Span) -> PResult<(Expr, u32)> {
        self.enter_depth(start)?;
        self.bump(); // '['
        self.skip_terms();
        let saved = self.restrict_record;
        self.restrict_record = false;

        if self.at(TokenKind::Colon) {
            self.bump();
            self.skip_terms();
            let close = self.expect(TokenKind::RBracket, "']'");
            self.restrict_record = saved;
            self.exit_depth();
            let close = close?;
            let span = Span::new(start.start, close.span.end);
            return Ok((
                Expr {
                    id: self.next_id(),
                    span,
                    kind: ExprKind::Map(Vec::new()),
                },
                0,
            ));
        }
        if self.at(TokenKind::RBracket) {
            let close = self.bump();
            self.restrict_record = saved;
            self.exit_depth();
            let span = Span::new(start.start, close.span.end);
            return Ok((
                Expr {
                    id: self.next_id(),
                    span,
                    kind: ExprKind::List(Vec::new()),
                },
                0,
            ));
        }

        let first = match self.parse_expression() {
            Ok(v) => v,
            Err(e) => {
                self.restrict_record = saved;
                self.exit_depth();
                return Err(e);
            }
        };
        self.skip_terms();
        let is_map = self.at(TokenKind::Colon);
        let result = if is_map {
            self.bump();
            self.skip_terms();
            self.parse_map_literal_rest(start, first)
        } else {
            self.parse_list_literal_rest(start, first)
        };
        self.restrict_record = saved;
        self.exit_depth();
        result
    }

    fn parse_list_literal_rest(&mut self, start: Span, first: (Expr, u32)) -> PResult<(Expr, u32)> {
        let mut items = vec![first.0];
        let mut max_w = first.1;
        loop {
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
                self.skip_terms();
                if self.at(TokenKind::RBracket) {
                    break;
                }
                let (e, w) = self.parse_expression()?;
                max_w = max_w.max(w);
                items.push(e);
            } else {
                break;
            }
        }
        self.skip_terms();
        let close = self.expect(TokenKind::RBracket, "']'")?;
        let span = Span::new(start.start, close.span.end);
        Ok((
            Expr {
                id: self.next_id(),
                span,
                kind: ExprKind::List(items),
            },
            max_w,
        ))
    }

    fn parse_map_literal_rest(
        &mut self,
        start: Span,
        first_key: (Expr, u32),
    ) -> PResult<(Expr, u32)> {
        let (value, vw) = self.parse_expression()?;
        let mut max_w = first_key.1.max(vw);
        let mut entries = vec![MapEntry {
            key: first_key.0,
            value,
        }];
        loop {
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
                self.skip_terms();
                if self.at(TokenKind::RBracket) {
                    break;
                }
                let (key, kw) = self.parse_expression()?;
                self.skip_terms();
                self.expect(TokenKind::Colon, "':'")?;
                self.skip_terms();
                let (value, vw) = self.parse_expression()?;
                max_w = max_w.max(kw).max(vw);
                entries.push(MapEntry { key, value });
            } else {
                break;
            }
        }
        self.skip_terms();
        let close = self.expect(TokenKind::RBracket, "']'")?;
        let span = Span::new(start.start, close.span.end);
        Ok((
            Expr {
                id: self.next_id(),
                span,
                kind: ExprKind::Map(entries),
            },
            max_w,
        ))
    }

    fn parse_record_literal(&mut self, start: Span) -> PResult<(Expr, u32)> {
        self.enter_depth(start)?;
        self.bump(); // '{'
        let mut fields = Vec::new();
        let mut max_w = 0u32;
        let saved = self.restrict_record;
        self.restrict_record = false;
        let result: PResult<Span> = (|| {
            loop {
                self.skip_terms();
                if self.at(TokenKind::RBrace) || self.exhausted() {
                    break;
                }
                let name = self.expect_ident()?;
                self.expect(TokenKind::Colon, "':'")?;
                let (value, w) = self.parse_expression()?;
                max_w = max_w.max(w);
                fields.push(FieldInit { name, value });
                self.skip_terms();
                if self.at(TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.skip_terms();
            let close = self.expect(TokenKind::RBrace, "'}'")?;
            Ok(Span::new(start.start, close.span.end))
        })();
        self.restrict_record = saved;
        self.exit_depth();
        let span = result?;
        if fields.is_empty() {
            self.push_diag(
                Code::HS0112,
                &[("kind", &"record"), ("member", &"field")],
                span,
            );
        }
        Ok((
            Expr {
                id: self.next_id(),
                span,
                kind: ExprKind::Record(fields),
            },
            max_w,
        ))
    }

    fn parse_closure(&mut self, start: Span) -> PResult<(Expr, u32)> {
        self.bump(); // 'fn'
        self.expect(TokenKind::LParen, "'('")?;
        self.enter_depth(start)?;
        let mut params = Vec::new();
        loop {
            self.skip_terms();
            if self.at(TokenKind::RParen) || self.exhausted() {
                break;
            }
            let name = match self.expect_ident() {
                Ok(n) => n,
                Err(e) => {
                    self.exit_depth();
                    return Err(e);
                }
            };
            let ty = if self.at(TokenKind::Colon) {
                self.bump();
                match self.parse_type() {
                    Ok(t) => Some(t),
                    Err(e) => {
                        self.exit_depth();
                        return Err(e);
                    }
                }
            } else {
                None
            };
            params.push(ClosureParam { name, ty });
            self.skip_terms();
            if self.at(TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.skip_terms();
        if self.expect(TokenKind::RParen, "')'").is_err() {
            self.exit_depth();
            return Err(Recover);
        }
        self.exit_depth();
        let ret = self.parse_optional_return_type();
        let body = self.parse_block_result()?;
        let span = Span::new(start.start, body.span.end);
        Ok((
            Expr {
                id: self.next_id(),
                span,
                kind: ExprKind::Closure(Closure { params, ret, body }),
            },
            0,
        ))
    }

    // ---- string literals with interpolation -----------------------------

    /// Parses a string literal, returning it and the maximum weight of any
    /// interpolation it contains.
    fn parse_string_literal_raw(&mut self) -> PResult<(StringLiteral, u32)> {
        let head = self.cur();
        if head.kind == TokenKind::String {
            self.bump();
            let text = self.string_token_text(head);
            return Ok((
                StringLiteral {
                    span: head.span,
                    parts: vec![StringPart::Text(text)],
                    malformed: head.malformed,
                },
                0,
            ));
        }
        if head.kind != TokenKind::StringHead {
            return Err(self.fail_expected("a string"));
        }
        self.bump();
        let start = head.span.start;
        let mut parts = vec![StringPart::Text(self.string_token_text(head))];
        let mut malformed = head.malformed;
        let mut max_w = 0u32;
        loop {
            self.enter_depth(self.cur().span)?;
            let saved = self.restrict_record;
            self.restrict_record = false;
            let expr_result = self.parse_expression();
            self.restrict_record = saved;
            self.exit_depth();
            let (expr, w) = expr_result?;
            max_w = max_w.max(w);
            parts.push(StringPart::Interpolation(Box::new(expr)));
            match self.cur_kind() {
                TokenKind::StringMiddle => {
                    let t = self.bump();
                    malformed |= t.malformed;
                    parts.push(StringPart::Text(self.string_token_text(t)));
                }
                TokenKind::StringTail => {
                    let t = self.bump();
                    malformed |= t.malformed;
                    let end = t.span.end;
                    parts.push(StringPart::Text(self.string_token_text(t)));
                    return Ok((
                        StringLiteral {
                            span: Span::new(start, end),
                            parts,
                            malformed,
                        },
                        max_w,
                    ));
                }
                _ => return Err(self.fail_expected("'}'")),
            }
        }
    }

    fn string_token_text(&self, t: Token) -> String {
        let raw = self.text(t.span);
        let after_open = raw
            .strip_prefix('"')
            .or_else(|| raw.strip_prefix('}'))
            .unwrap_or(raw);
        let inner = after_open
            .strip_suffix('"')
            .or_else(|| after_open.strip_suffix("${"))
            .unwrap_or(after_open);
        decode_string_text(inner)
    }
}

fn parse_int_value(text: &str) -> u64 {
    let mut value: u64 = 0;
    for b in text.bytes() {
        if b == b'_' || !b.is_ascii_digit() {
            continue;
        }
        let d = u64::from(b - b'0');
        value = value.saturating_mul(10).saturating_add(d);
    }
    value
}

fn parse_float_value(text: &str) -> f64 {
    let cleaned: String = text.chars().filter(|&c| c != '_').collect();
    cleaned.parse::<f64>().unwrap_or(0.0)
}

/// Whether `expr`, with any trailing `?` operators stripped, is a call
/// (section 9.3's "ignoring any trailing `?` operators, the last operation of
/// the chain must be a call"). Shared by the `effect` marker check (HS0403)
/// and, through [`is_call_like`], the call-statement check (HS0109), so the
/// two agree on what "ends in a call" means.
fn ends_in_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Try(inner) => ends_in_call(inner),
        ExprKind::Call { .. } => true,
        _ => false,
    }
}

/// Whether `expr` is a postfix chain, optionally marked `effect`, whose last
/// operation, ignoring trailing `?`, is a call (section 4.9, HS0109).
fn is_call_like(expr: &Expr) -> bool {
    match &expr.kind {
        // `effect` does not change whether the chain it marks ends in a
        // call; whether it was the *right* thing to mark is HS0403's
        // question, not this one's, so this recurses the same way `?` does,
        // rather than repeating a shallower, buggy version of the check.
        ExprKind::Try(inner) | ExprKind::Effect(inner) => is_call_like(inner),
        ExprKind::Call { .. } | ExprKind::Error => true,
        _ => false,
    }
}

fn span_or(id: NodeId, span: Span, names: Vec<Ident>) -> UsesLine {
    UsesLine { id, span, names }
}

#[cfg(test)]
mod tests;
