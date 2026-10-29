//! The canonical formatter (the `format` command of the specification's
//! tooling chapter).
//!
//! `format` parses a source and, when it parses cleanly, prints it back in
//! one canonical layout: four spaces per level of nesting, a single space
//! around every operator, one blank line between top-level declarations, and
//! a fixed line width past which a list, record, map or a call's arguments
//! move one to a line. A source with a lexical or syntax error is not
//! formatted; the diagnostics explain why, exactly as `parse` reports them.
//!
//! Every comment in the source, tagged or plain, is kept. A comment is
//! printed wherever the layout has a line for it: on its own line before a
//! declaration, a statement or a match arm, trailing the line a statement
//! ends on, or before a block's closing brace. A comment in a place with no
//! such line (inside a list that spans several lines, between a condition
//! and the block that follows it, and so on) cannot be kept without moving
//! or dropping it, so the source is refused instead, with a diagnostic that
//! points at the comment.
//!
//! The syntax tree keeps a parenthesised expression as its own node
//! ([`ExprKind::Paren`]) rather than folding it away, precisely so this
//! module never has to work out from precedence alone which parentheses a
//! re-parse would need: every parenthesis printed is one the source already
//! had, and every one the source had is printed back. The same is true of a
//! variant pattern's own parentheses ([`PatternKind::Variant`]'s `parens`
//! field). Printing a node's children exactly as the grammar produced them,
//! with no attempt to add or remove grouping, is therefore always correct.
//!
//! The specification does not define a formatter's output; the conformance
//! corpus under `corpus/formatting/` pins the choices this implementation
//! has made, and `format(format(source)) == format(source)` for any source
//! that formats at all (checked by `tests/format_roundtrip.rs`, alongside
//! the whole conformance corpus and a large set of generated and mutated
//! programs).

use crate::ast::{
    BinaryOp, BindingDecl, BindingTarget, Block, Closure, Else, EnumDecl, Expr, ExprKind,
    FieldInit, FieldType, File, FnDecl, Ident, IfStmt, Item, ItemKind, ListElement, Literal,
    MapEntry, MatchArm, MatchStmt, Param, Pattern, PatternKind, RecordDecl, Stmt, StmtKind,
    StringLiteral, StringPart, TypeExpr, TypeKind, UnaryOp, VariantDecl,
};
use crate::parser::{ParseOutput, parse};
use crate::token::Comment;
use hollowscript_core::{Code, Control, Diagnostic, Interner, Source, Span};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Four spaces, the indent unit every example in the specification uses.
const INDENT_UNIT: &str = "    ";

/// The line width past which a list, record, map or call's arguments move
/// one to a line. Fixed centrally rather than left tunable: one canonical
/// form has no options.
const WIDTH: usize = 100;

/// The result of formatting one source.
#[derive(Clone, Debug, PartialEq)]
pub struct FormatOutput {
    /// The formatted text, or `None` when the source has an error, or has a
    /// comment the layout has no place to keep.
    pub output: Option<String>,
    /// The diagnostics that made the formatter refuse: the source's own
    /// lexical and syntax diagnostics, or, for an otherwise clean source, one
    /// diagnostic naming the comment that has no place.
    pub diagnostics: Vec<Diagnostic>,
}

/// Formats a source.
///
/// Printing walks the parsed tree with plain, unbounded recursion (the
/// printer's `print_expr` and its kin), exactly the kind of recursion
/// [`crate::parse`] already runs on a dedicated, generously sized stack
/// rather than the caller's own: a legal program at the weight limit of
/// section 1.10 is as deep to print as it was to parse, so printing it, and
/// the round-trip re-parse that checks it, run on such a thread too. Section
/// 1.10 requires that a construct within the limits "MUST NOT exhaust the
/// host's stack", and a stack overflow is a process abort that no caller can
/// recover from, which the ordinary diagnostic this function otherwise
/// returns can be.
#[must_use]
pub fn format(source: &Source, interner: &mut Interner, control: &mut Control<'_>) -> FormatOutput {
    let parsed = parse(source, interner, control);
    if parsed.diagnostics.iter().any(Diagnostic::is_error) {
        return FormatOutput {
            output: None,
            diagnostics: hollowscript_core::cap_diagnostics(parsed.diagnostics),
        };
    }
    let interner: &Interner = interner;
    let run = move || print_and_verify(source, interner, parsed, control);
    match hollowscript_core::run_on_dedicated_stack(run) {
        Ok(output) => output,
        // Printing must never panic; a `Panicked` here only guards against
        // a defect elsewhere (or in a future change) actually doing so.
        // `SpawnFailed` (the host could not create the thread) is, as in
        // `parse`, reported the same way as any other internal failure
        // rather than risking a stack this function cannot bound on the
        // calling thread. See `run_on_dedicated_stack`'s own documentation
        // for what `wasm32`, which has no thread backend, does instead.
        Err(hollowscript_core::DedicatedStackError::Panicked) => {
            internal_error_output(source, "the formatter did not complete")
        }
        Err(hollowscript_core::DedicatedStackError::SpawnFailed) => {
            internal_error_output(source, "could not start the formatter")
        }
    }
}

/// The printer over the parsed tree of a source, and the round-trip check.
/// Split out of [`format`] so that both can run on the dedicated stack it
/// spawns for exactly this recursion; see `format`'s own documentation.
fn print_and_verify(
    source: &Source,
    interner: &Interner,
    parsed: ParseOutput,
    control: &mut Control<'_>,
) -> FormatOutput {
    let mut comments = parsed.file.comments.clone();
    comments.sort_by_key(|c| c.span.start);
    let mut printer = Printer {
        text: source.text(),
        interner,
        comments: &comments,
        next: 0,
        stray: None,
        one_line: BTreeMap::new(),
    };
    let text = printer.print_file(&parsed.file);

    if let Some(span) = printer.stray {
        let diagnostic = refusal_diagnostic(
            "a comment here has no line of its own in the canonical layout (for example, \
             inside a list that spans more than one line, or between a condition and the \
             block that follows it); move it onto its own line, or to the end of the line \
             it already shares, then format again",
        )
        .with_location(source.locate(span));
        return FormatOutput {
            output: None,
            diagnostics: vec![diagnostic],
        };
    }

    // Defence in depth, not a substitute for testing this module directly:
    // the text just produced must itself lex and parse without error and
    // must carry exactly the comments the source had. Either failing here is
    // a bug in this module, and refusing leaves the caller's own source
    // untouched rather than handing back damaged text.
    let Ok(check_source) = Source::new(source.id(), source.name().clone(), text.clone()) else {
        return FormatOutput {
            output: None,
            diagnostics: vec![refusal_diagnostic(
                "formatting this source would not fit the source size limit; nothing was changed",
            )],
        };
    };
    let mut check_interner = Interner::new();
    let reparsed = parse(&check_source, &mut check_interner, control);
    if reparsed.diagnostics.iter().any(Diagnostic::is_error) {
        return FormatOutput {
            output: None,
            diagnostics: vec![refusal_diagnostic(
                "the formatter produced text that does not itself parse; nothing was changed",
            )],
        };
    }
    let mut before: Vec<String> = parsed
        .file
        .comments
        .iter()
        .map(|c| comment_key(source.text(), c))
        .collect();
    let mut after: Vec<String> = reparsed
        .file
        .comments
        .iter()
        .map(|c| comment_key(&text, c))
        .collect();
    before.sort();
    after.sort();
    if before != after {
        return FormatOutput {
            output: None,
            diagnostics: vec![refusal_diagnostic(
                "the formatter would have changed which comments the source has; nothing was changed",
            )],
        };
    }

    FormatOutput {
        output: Some(text),
        diagnostics: parsed.diagnostics,
    }
}

/// The [`FormatOutput`] for a situation this function's own machinery
/// (rather than the source being formatted) could not get past: the printing
/// thread panicked or could not be started. Mirrors the internal-error output
/// `parse` builds for the same two situations, with `HS1099` and no location
/// more precise than the start of the file.
fn internal_error_output(source: &Source, message: &str) -> FormatOutput {
    FormatOutput {
        output: None,
        diagnostics: vec![refusal_diagnostic(message).with_location(source.locate(Span::empty(0)))],
    }
}

/// A diagnostic for a situation the specification's own registry has no code
/// for, because the registry (chapter 17) covers the lexer, the parser, the
/// checker and the runtime, and this is none of those: a deliberate,
/// case-by-case refusal by the formatter itself. `HS1099` is this crate's
/// existing, documented escape hatch for exactly that gap (see
/// [`Diagnostic::not_implemented`]); this reuses the same code with its own
/// message rather than inventing a code the specification does not define.
fn refusal_diagnostic(message: &str) -> Diagnostic {
    Diagnostic::new(Code::HS1099, &[("message", &message)])
}

/// The text of a comment (from `//` to end of line, trailing spaces and tabs
/// removed) used as a key for comparing two comment lists as multisets.
fn comment_key(text: &str, comment: &Comment) -> String {
    text.get(comment.span.start as usize..comment.span.end as usize)
        .unwrap_or("")
        .trim_end()
        .to_string()
}

/// The stateful printer: the source text and interner (read-only), and the
/// comment cursor, which every leading or trailing comment is taken from in
/// source order as printing reaches the place for it. A comment left
/// pending when [`Printer::guard_before`] is next called has no such place.
#[derive(Debug)]
struct Printer<'a> {
    text: &'a str,
    interner: &'a Interner,
    comments: &'a [Comment],
    next: usize,
    stray: Option<Span>,
    /// What [`Printer::print_case_body`] learned about a statement when it
    /// tried it on one line, by the statement's span: the line, or `None` if it
    /// does not fit on one. The attempt prints the statement at indent 0 and
    /// throws the text away if it comes to more than one line, and a statement
    /// that holds a `match` whose arms are single statements holds that attempt
    /// again at every level: without this the attempt and the real print each
    /// make the one inside, so every level of nesting doubles the work, and 24
    /// nested arms took 12 seconds.
    one_line: BTreeMap<(u32, u32), Option<String>>,
}

impl Printer<'_> {
    fn ident(&self, id: &Ident) -> &str {
        self.interner.resolve(id.name)
    }

    fn join_idents(&self, idents: &[Ident]) -> String {
        idents
            .iter()
            .map(|id| self.ident(id).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    // ---- the comment cursor --------------------------------------------

    /// Every not-yet-taken comment that starts before `offset`, in source
    /// order. Advances past them, so each comment is returned at most once.
    fn take_before(&mut self, offset: u32) -> Vec<Comment> {
        let mut out = Vec::new();
        while let Some(comment) = self.comments.get(self.next) {
            if comment.span.start >= offset {
                break;
            }
            out.push(*comment);
            self.next += 1;
        }
        out
    }

    /// Notes the next comment as stray when it is due before `offset`:
    /// everything this module can place has already been taken by the time
    /// this is called at the right points, so what remains is in a spot with
    /// no place for it. Only the first stray comment found is kept, since
    /// printing continues (the output is discarded either way).
    fn guard_before(&mut self, offset: u32) {
        if self.stray.is_some() {
            return;
        }
        if let Some(comment) = self.comments.get(self.next) {
            if comment.span.start < offset {
                self.stray = Some(comment.span);
            }
        }
    }

    /// The next comment, when it shares its line with a token ending exactly
    /// at `end` (a statement's or a branch's last token): `x = 1 // why`.
    fn take_trailing(&mut self, end: u32) -> Option<Comment> {
        let comment = *self.comments.get(self.next)?;
        if self.is_trailing(&comment, end) {
            self.next += 1;
            Some(comment)
        } else {
            None
        }
    }

    /// A trailing comment must sit directly after `end` with nothing but
    /// whitespace between: not only no newline, but no other token either.
    /// Checking for a newline alone let a comment that actually opens the
    /// *next* construct on the same line (`1 { a }   2 { // note` written on
    /// one line, where the comment is the first thing inside arm `2`'s
    /// block) be misread as trailing the previous one, which made printing
    /// non-idempotent: the first pass moved the comment onto the wrong arm's
    /// line, and the second pass, now with a newline back in its usual
    /// place, read it correctly and printed something different.
    fn is_trailing(&self, comment: &Comment, end: u32) -> bool {
        comment.span.start >= end
            && self
                .text
                .get(end as usize..comment.span.start as usize)
                .is_some_and(|between| !between.contains(['\n', '\r']) && between.trim().is_empty())
    }

    /// Whatever is left once every node has been printed: a comment after
    /// the last statement of a block, or after the last item of the file.
    fn take_remaining(&mut self) -> Vec<Comment> {
        let rest = self.comments.get(self.next..).unwrap_or(&[]).to_vec();
        self.next = self.comments.len();
        rest
    }

    fn comment_text(&self, comment: &Comment) -> String {
        comment_key(self.text, comment)
    }

    /// Whether at least one whole blank line separates `comment_end` (a
    /// comment's own end) from `next_start` (the next comment or real code).
    fn blank_after(&self, comment_end: u32, next_start: u32) -> bool {
        self.text
            .get(comment_end as usize..next_start as usize)
            .is_some_and(|between| between.matches('\n').count() >= 2)
    }

    /// Own-line comments, each at `pad`. A blank line the source kept after
    /// one stays (once, however many there were), so a detached header
    /// comment does not glue itself to the code below; `before_code` says
    /// whether anything follows this whole run, since a blank line right
    /// before a closing brace or the end of the file would only be noise.
    fn render_leading(
        &self,
        comments: &[Comment],
        pad: &str,
        before_code: bool,
        next_offset: u32,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        for (index, comment) in comments.iter().enumerate() {
            lines.push(format!("{pad}{}", self.comment_text(comment)));
            let boundary = comments
                .get(index + 1)
                .map_or(next_offset, |next| next.span.start);
            let is_last = index + 1 == comments.len();
            if self.blank_after(comment.span.end, boundary) && (before_code || !is_last) {
                lines.push(String::new());
            }
        }
        lines
    }

    /// Appends a trailing comment to the last of `lines`.
    fn with_trailing(&self, mut lines: Vec<String>, trailing: Option<Comment>) -> Vec<String> {
        if let Some(comment) = trailing {
            if let Some(last) = lines.last_mut() {
                last.push(' ');
                last.push_str(&self.comment_text(&comment));
            }
        }
        lines
    }

    // ---- the file and top-level items -----------------------------------

    fn print_file(&mut self, file: &File) -> String {
        let mut sections: Vec<Vec<String>> = Vec::new();

        if !file.uses.is_empty() {
            let mut lines = Vec::new();
            for uses_line in &file.uses {
                let due = self.take_before(uses_line.span.start);
                lines.extend(self.render_leading(&due, "", true, uses_line.span.start));
                let names = self.join_idents(&uses_line.names);
                self.guard_before(uses_line.span.end);
                let trailing = self.take_trailing(uses_line.span.end);
                lines.extend(self.with_trailing(vec![format!("uses {names}")], trailing));
            }
            sections.push(lines);
        }

        if !file.imports.is_empty() {
            let mut lines = Vec::new();
            for import in &file.imports {
                let due = self.take_before(import.span.start);
                lines.extend(self.render_leading(&due, "", true, import.span.start));
                let names = self.join_idents(&import.names);
                let path = self.print_string_literal(&import.path, 0);
                let line = format!("import {names} from {path}");
                self.guard_before(import.span.end);
                let trailing = self.take_trailing(import.span.end);
                lines.extend(self.with_trailing(vec![line], trailing));
            }
            sections.push(lines);
        }

        for item in &file.items {
            let due = self.take_before(item.span.start);
            let mut lines = self.render_leading(&due, "", true, item.span.start);
            let printed = self.print_top_level_item(item);
            self.guard_before(item.span.end);
            let trailing = self.take_trailing(item.span.end);
            lines.extend(self.with_trailing(printed, trailing));
            sections.push(lines);
        }

        // Whatever comments follow the last item stay one block, however the
        // source spaced them; one blank line only where it already had one.
        let remaining = self.take_remaining();
        if !remaining.is_empty() {
            sections.push(self.render_leading(&remaining, "", false, file.span.end));
        }

        let mut lines: Vec<String> = Vec::new();
        for (index, section) in sections.into_iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            lines.extend(section);
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        }
    }

    fn print_top_level_item(&mut self, item: &Item) -> Vec<String> {
        let inner = match &item.kind {
            ItemKind::Fn(decl) => self.print_fn_decl(decl),
            ItemKind::Record(decl) => self.print_record_decl(decl),
            ItemKind::Enum(decl) => self.print_enum_decl(decl),
            ItemKind::Constant(decl) => self.print_binding(decl, "constant", 0),
            ItemKind::Variable(decl) => self.print_binding(decl, "variable", 0),
            ItemKind::Stmt(stmt) => self.print_statement(stmt, 0),
        };
        if item.export.is_none() {
            return inner;
        }
        let mut lines = inner;
        if let Some(first) = lines.first_mut() {
            *first = format!("export {first}");
        }
        lines
    }

    fn print_fn_decl(&mut self, decl: &FnDecl) -> Vec<String> {
        let head = format!(
            "fn {}{}({}){} {{",
            self.ident(&decl.name),
            self.print_type_params(&decl.type_params),
            self.print_params(&decl.params),
            self.print_return_type(decl.ret.as_ref()),
        );
        let mut lines = vec![head];
        lines.extend(self.print_block_lines(&decl.body, 0));
        lines.push("}".to_string());
        lines
    }

    fn print_record_decl(&self, decl: &RecordDecl) -> Vec<String> {
        let text = format!(
            "record {}{} = {}",
            self.ident(&decl.name),
            self.print_type_params(&decl.type_params),
            self.print_record_type(&decl.fields, 0),
        );
        one_or_more(&text, "")
    }

    fn print_enum_decl(&self, decl: &EnumDecl) -> Vec<String> {
        let head = format!(
            "enum {}{}",
            self.ident(&decl.name),
            self.print_type_params(&decl.type_params)
        );
        if decl.variants.is_empty() {
            return vec![format!("{head} {{ }}")];
        }
        let variants: Vec<String> = decl
            .variants
            .iter()
            .map(|v| self.render_variant(v))
            .collect();
        let flat = format!("{head} {{ {} }}", variants.join(", "));
        if fits(flat.len(), 0, 0) {
            return vec![flat];
        }
        let pad = indent(1);
        let count = variants.len();
        let mut lines = vec![format!("{head} {{")];
        for (index, variant) in variants.iter().enumerate() {
            let suffix = if index + 1 == count { "" } else { "," };
            lines.push(format!("{pad}{variant}{suffix}"));
        }
        lines.push("}".to_string());
        lines
    }

    fn render_variant(&self, variant: &VariantDecl) -> String {
        if variant.payload.is_empty() {
            return self.ident(&variant.name).to_string();
        }
        let payload = variant
            .payload
            .iter()
            .map(|t| self.print_type(t))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({payload})", self.ident(&variant.name))
    }

    // ---- types ------------------------------------------------------
    // Type annotations carry no comments of their own in the grammar, so
    // nothing below this point in the type printer needs the cursor.

    fn print_type_params(&self, params: &[Ident]) -> String {
        if params.is_empty() {
            String::new()
        } else {
            format!("<{}>", self.join_idents(params))
        }
    }

    fn print_params(&self, params: &[Param]) -> String {
        params
            .iter()
            .map(|param| {
                format!(
                    "{}: {}",
                    self.ident(&param.name),
                    self.print_type(&param.ty)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn print_return_type(&self, ret: Option<&TypeExpr>) -> String {
        match ret {
            Some(t) => format!(": {}", self.print_type(t)),
            None => String::new(),
        }
    }

    fn print_type(&self, ty: &TypeExpr) -> String {
        match &ty.kind {
            TypeKind::Named { name, args } => {
                let args_text = if args.is_empty() {
                    String::new()
                } else {
                    let joined = args
                        .iter()
                        .map(|a| self.print_type(a))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("<{joined}>")
                };
                format!("{}{args_text}", self.ident(name))
            }
            TypeKind::List(element) => format!("[{}]", self.print_type(element)),
            TypeKind::Optional(inner) => format!("{}?", self.print_type(inner)),
            TypeKind::Paren(inner) => format!("({})", self.print_type(inner)),
            TypeKind::Record(fields) => self.print_record_type(fields, 0),
            TypeKind::Fn {
                effect,
                params,
                ret,
            } => {
                let prefix = if *effect { "effect " } else { "" };
                let params_text = params
                    .iter()
                    .map(|p| self.print_type(p))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{prefix}fn({params_text}){}",
                    self.print_return_type(ret.as_deref())
                )
            }
            // Only ever built alongside a syntax error; kept total rather
            // than assuming a clean parse never reaches it.
            TypeKind::Error => String::new(),
        }
    }

    fn print_record_type(&self, fields: &[FieldType], level: u32) -> String {
        if fields.is_empty() {
            return "{ }".to_string();
        }
        let flat_fields: Vec<String> = fields
            .iter()
            .map(|f| format!("{}: {}", self.ident(&f.name), self.print_type(&f.ty)))
            .collect();
        let flat = format!("{{ {} }}", flat_fields.join(", "));
        if fits(flat.len(), level, 0) {
            return flat;
        }
        let pad = indent(level + 1);
        let body = flat_fields
            .iter()
            .map(|f| format!("{pad}{f}"))
            .collect::<Vec<_>>()
            .join(",\n");
        format!("{{\n{body}\n{}}}", indent(level))
    }

    // ---- patterns -----------------------------------------------------
    // Patterns, like types, carry no comments of their own.

    fn print_binding_target(&self, target: &BindingTarget) -> String {
        match target {
            BindingTarget::Name(id) => self.ident(id).to_string(),
            BindingTarget::Pattern(pattern) => self.print_pattern(pattern),
        }
    }

    fn print_pattern(&self, pattern: &Pattern) -> String {
        match &pattern.kind {
            PatternKind::Literal { negative, literal } => {
                let text = match literal {
                    Literal::Int { value, .. } => value.to_string(),
                    Literal::Float { value, .. } => format_float(*value),
                    Literal::Bool(b) => b.to_string(),
                    // A string pattern (with no interpolation; one with an
                    // interpolation is already a syntax error) is built
                    // directly by the parser's own string-literal branch,
                    // never through `-`, so `negative` is always false here.
                    Literal::String(sl) => render_flat_string(sl, self.interner),
                    // `None` has its own `PatternKind`, and a filter literal
                    // is not accepted as a pattern at all; kept total rather
                    // than assuming a clean parse never reaches this.
                    Literal::None | Literal::Filter { .. } => String::new(),
                };
                if *negative { format!("-{text}") } else { text }
            }
            PatternKind::None => "none".to_string(),
            PatternKind::Wildcard => "_".to_string(),
            PatternKind::Binding(id) => self.ident(id).to_string(),
            PatternKind::Variant { name, args, parens } => {
                if *parens {
                    let joined = args
                        .iter()
                        .map(|a| self.print_pattern(a))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}({joined})", self.ident(name))
                } else {
                    self.ident(name).to_string()
                }
            }
            PatternKind::Record(fields) => {
                if fields.is_empty() {
                    return "{ }".to_string();
                }
                let body = fields
                    .iter()
                    .map(|f| match &f.pattern {
                        Some(p) => format!("{}: {}", self.ident(&f.name), self.print_pattern(p)),
                        None => self.ident(&f.name).to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {body} }}")
            }
            PatternKind::List(elements) => {
                let body = elements
                    .iter()
                    .map(|element| match element {
                        ListElement::Pattern(p) => self.print_pattern(p),
                        ListElement::Rest { binding, .. } => match binding {
                            Some(id) => format!("..{}", self.ident(id)),
                            None => "..".to_string(),
                        },
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{body}]")
            }
            // Only ever built alongside a syntax error.
            PatternKind::Error => String::new(),
        }
    }

    // ---- blocks and statements -----------------------------------------

    fn print_block_lines(&mut self, block: &Block, level: u32) -> Vec<String> {
        let mut lines = Vec::new();
        let pad = indent(level + 1);
        // A comment between a header and its `{` has no line of its own.
        self.guard_before(block.span.start);
        for stmt in &block.stmts {
            let due = self.take_before(stmt.span.start);
            lines.extend(self.render_leading(&due, &pad, true, stmt.span.start));
            let printed = self.print_statement(stmt, level + 1);
            self.guard_before(stmt.span.end);
            let trailing = self.take_trailing(stmt.span.end);
            lines.extend(self.with_trailing(printed, trailing));
        }
        let due = self.take_before(block.span.end);
        lines.extend(self.render_leading(&due, &pad, false, block.span.end));
        lines
    }

    fn print_statement(&mut self, stmt: &Stmt, level: u32) -> Vec<String> {
        let pad = indent(level);
        match &stmt.kind {
            StmtKind::Constant(decl) => self.print_binding(decl, "constant", level),
            StmtKind::Variable(decl) => self.print_binding(decl, "variable", level),
            StmtKind::Assign { target, value } => {
                let head = format!("{} = ", self.ident(target));
                let column = pad.len() + head.len();
                let value_text = self.print_expr(value, level, column);
                one_or_more(&format!("{head}{value_text}"), &pad)
            }
            StmtKind::If(inner) => self.print_if(inner, level),
            StmtKind::Match(inner) => self.print_match(inner, level, stmt.span.end),
            StmtKind::For {
                target,
                iterable,
                body,
            } => {
                let target_text = self.print_binding_target(target);
                let iterable_text = self.print_expr(iterable, level, 0);
                let mut lines = vec![format!("{pad}for {target_text} in {iterable_text} {{")];
                lines.extend(self.print_block_lines(body, level));
                lines.push(format!("{pad}}}"));
                lines
            }
            StmtKind::While { condition, body } => {
                let condition_text = self.print_expr(condition, level, 0);
                let mut lines = vec![format!("{pad}while {condition_text} {{")];
                lines.extend(self.print_block_lines(body, level));
                lines.push(format!("{pad}}}"));
                lines
            }
            StmtKind::Break => vec![format!("{pad}break")],
            StmtKind::Continue => vec![format!("{pad}continue")],
            StmtKind::Return(value) => match value {
                None => vec![format!("{pad}return")],
                Some(inner) => {
                    let column = pad.len() + "return ".len();
                    let text = self.print_expr(inner, level, column);
                    one_or_more(&format!("return {text}"), &pad)
                }
            },
            StmtKind::Call(inner) => {
                let text = self.print_expr(inner, level, pad.len());
                one_or_more(&text, &pad)
            }
            // `Misplaced` and `Error` are only ever built alongside an error
            // diagnostic (HS0117, or a syntax error), so a clean parse never
            // reaches either arm; kept total, and deliberately not a panic,
            // rather than assuming it.
            StmtKind::Misplaced | StmtKind::Error => vec![pad],
        }
    }

    fn print_binding(&mut self, decl: &BindingDecl, keyword: &str, level: u32) -> Vec<String> {
        let pad = indent(level);
        let target = self.print_binding_target(&decl.target);
        let ty = match &decl.ty {
            Some(t) => format!(": {}", self.print_type(t)),
            None => String::new(),
        };
        let head = format!("{keyword} {target}{ty} = ");
        let column = pad.len() + head.len();
        let value = self.print_expr(&decl.init, level, column);
        one_or_more(&format!("{head}{value}"), &pad)
    }

    fn print_if(&mut self, stmt: &IfStmt, level: u32) -> Vec<String> {
        let pad = indent(level);
        let condition = self.print_expr(&stmt.condition, level, 0);
        let mut lines = vec![format!("{pad}if {condition} {{")];
        lines.extend(self.print_block_lines(&stmt.then, level));
        let mut current = stmt.otherwise.as_deref();
        while let Some(branch) = current {
            match branch {
                Else::If(boxed) => {
                    if let StmtKind::If(inner) = &boxed.kind {
                        let condition = self.print_expr(&inner.condition, level, 0);
                        lines.push(format!("{pad}}} else if {condition} {{"));
                        lines.extend(self.print_block_lines(&inner.then, level));
                        current = inner.otherwise.as_deref();
                    } else {
                        // The parser only ever builds `Else::If` around an
                        // `If` statement; kept total rather than assuming it.
                        current = None;
                    }
                }
                Else::Block(block) => {
                    lines.push(format!("{pad}}} else {{"));
                    lines.extend(self.print_block_lines(block, level));
                    current = None;
                }
            }
        }
        lines.push(format!("{pad}}}"));
        lines
    }

    fn print_match(&mut self, stmt: &MatchStmt, level: u32, overall_end: u32) -> Vec<String> {
        let pad = indent(level);
        let scrutinee = self.print_expr(&stmt.scrutinee, level, 0);
        let mut lines = vec![format!("{pad}match {scrutinee} {{")];
        let inner_pad = indent(level + 1);
        for arm in &stmt.arms {
            let start = match_arm_start(arm);
            let due = self.take_before(start);
            lines.extend(self.render_leading(&due, &inner_pad, true, start));
            let label = arm
                .patterns
                .iter()
                .map(|p| self.print_pattern(p))
                .collect::<Vec<_>>()
                .join(", ");
            let printed = self.print_case_body(&label, &arm.body, level + 1);
            self.guard_before(arm.body.span.end);
            let trailing = self.take_trailing(arm.body.span.end);
            lines.extend(self.with_trailing(printed, trailing));
        }
        if let Some(else_block) = &stmt.else_arm {
            let due = self.take_before(else_block.span.start);
            lines.extend(self.render_leading(&due, &inner_pad, true, else_block.span.start));
            let printed = self.print_case_body("else", else_block, level + 1);
            self.guard_before(else_block.span.end);
            let trailing = self.take_trailing(else_block.span.end);
            lines.extend(self.with_trailing(printed, trailing));
        }
        let due = self.take_before(overall_end);
        lines.extend(self.render_leading(&due, &inner_pad, false, overall_end));
        lines.push(format!("{pad}}}"));
        lines
    }

    /// A match arm's or the mandatory else arm's body, `label { ... }`.
    ///
    /// An arm whose body is one statement with no comment anywhere in it is
    /// written on one line, `none { return "unowned" }`, which is how the
    /// language's own guide and examples write them: a `match` is usually a
    /// table of short answers, and giving each three lines buries it.
    /// Anything else, a longer body or a body with a comment, is a block.
    fn print_case_body(&mut self, label: &str, block: &Block, level: u32) -> Vec<String> {
        let pad = indent(level);
        if let [only] = block.stmts.as_slice() {
            if !self.has_comment_in(block.span) {
                let key = (only.span.start, only.span.end);
                let attempt = if let Some(known) = self.one_line.get(&key) {
                    known.clone()
                } else {
                    let printed = self.print_statement(only, 0);
                    let attempt = match printed.as_slice() {
                        [line] => Some(line.clone()),
                        _ => None,
                    };
                    self.one_line.insert(key, attempt.clone());
                    attempt
                };
                if let Some(line) = attempt {
                    self.guard_before(block.span.end);
                    return vec![format!("{pad}{label} {{ {line} }}")];
                }
            }
        }
        let mut lines = vec![format!("{pad}{label} {{")];
        lines.extend(self.print_block_lines(block, level));
        lines.push(format!("{pad}}}"));
        lines
    }

    /// Whether any comment falls inside `span`, which is what keeps a
    /// one-line arm from swallowing one.
    fn has_comment_in(&self, span: Span) -> bool {
        self.comments
            .iter()
            .any(|c| c.span.start >= span.start && c.span.end <= span.end)
    }

    // ---- expressions ----------------------------------------------------

    fn print_string_literal(&mut self, literal: &StringLiteral, level: u32) -> String {
        let mut out = String::from("\"");
        for part in &literal.parts {
            match part {
                StringPart::Text(text) => out.push_str(&escape_string_text(text)),
                StringPart::Interpolation(expr) => {
                    out.push_str("${");
                    out.push_str(&self.print_expr(expr, level, 0));
                    out.push('}');
                }
            }
        }
        out.push('"');
        out
    }

    /// `level` is the statement's own indent level, used only if `expr`
    /// breaks onto further lines (a closure); `column` is how far into that
    /// first line printing has already reached (for example, `constant x = `
    /// before the value), used only to decide whether a list, record, map or
    /// call breaks.
    fn print_expr(&mut self, expr: &Expr, level: u32, column: usize) -> String {
        match &expr.kind {
            ExprKind::Literal(lit) => match lit {
                Literal::Int { value, .. } => value.to_string(),
                Literal::Float { value, .. } => format_float(*value),
                Literal::Bool(b) => b.to_string(),
                Literal::None => "none".to_string(),
                Literal::String(sl) => self.print_string_literal(sl, level),
                Literal::Filter { source, .. } => format!("`{}`", escape_filter_source(source)),
            },
            ExprKind::Name(id) => self.ident(id).to_string(),
            ExprKind::List(items) => self.print_list(items, level, column),
            ExprKind::Map(entries) => self.print_map(entries, level, column),
            ExprKind::Record(fields) => self.print_record_literal(fields, level, column),
            ExprKind::Closure(closure) => self.print_closure(closure, level),
            ExprKind::Paren(inner) => format!("({})", self.print_expr(inner, level, column + 1)),
            ExprKind::Unary { op, operand } => {
                let op_text = unary_op_str(*op);
                format!(
                    "{op_text}{}",
                    self.print_expr(operand, level, column + op_text.len())
                )
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let left = self.print_expr(lhs, level, column);
                let op_text = binary_op_str(*op);
                let prefix = format!("{left} {op_text} ");
                let right = self.print_expr(rhs, level, column + prefix.len());
                format!("{prefix}{right}")
            }
            ExprKind::Effect(inner) => {
                format!("effect {}", self.print_expr(inner, level, column + 7))
            }
            ExprKind::Call { callee, args } => {
                let callee_text = self.print_expr(callee, level, column);
                let args_column = column + callee_text.len();
                let flat: Vec<String> = args
                    .iter()
                    .map(|a| render_flat_expr(a, self.interner))
                    .collect();
                let brackets = Brackets {
                    open_flat: "(",
                    close_flat: ")",
                    open_broken: "(",
                    close_broken: ")",
                };
                let args_text =
                    lay_out_bracketed(brackets, args, &flat, (level, args_column), |a, lvl| {
                        self.print_expr(a, lvl, 0)
                    });
                format!("{callee_text}{args_text}")
            }
            ExprKind::Index { target, index } => {
                let target_text = self.print_expr(target, level, column);
                let index_text = self.print_expr(index, level, 0);
                format!("{target_text}[{index_text}]")
            }
            ExprKind::Field { target, name } => {
                let target_text = self.print_expr(target, level, column);
                format!("{target_text}.{}", self.ident(name))
            }
            ExprKind::Try(inner) => format!("{}?", self.print_expr(inner, level, column)),
            // Only ever built alongside a syntax error.
            ExprKind::Error => String::new(),
        }
    }

    fn print_list(&mut self, items: &[Expr], level: u32, column: usize) -> String {
        let flat: Vec<String> = items
            .iter()
            .map(|e| render_flat_expr(e, self.interner))
            .collect();
        let brackets = Brackets {
            open_flat: "[",
            close_flat: "]",
            open_broken: "[",
            close_broken: "]",
        };
        lay_out_bracketed(brackets, items, &flat, (level, column), |item, lvl| {
            self.print_expr(item, lvl, 0)
        })
    }

    fn print_map(&mut self, entries: &[MapEntry], level: u32, column: usize) -> String {
        if entries.is_empty() {
            return "[:]".to_string();
        }
        let flat: Vec<String> = entries
            .iter()
            .map(|e| {
                format!(
                    "{}: {}",
                    render_flat_expr(&e.key, self.interner),
                    render_flat_expr(&e.value, self.interner)
                )
            })
            .collect();
        let brackets = Brackets {
            open_flat: "[",
            close_flat: "]",
            open_broken: "[",
            close_broken: "]",
        };
        lay_out_bracketed(brackets, entries, &flat, (level, column), |entry, lvl| {
            let key = self.print_expr(&entry.key, lvl, 0);
            let value = self.print_expr(&entry.value, lvl, 0);
            format!("{key}: {value}")
        })
    }

    fn print_record_literal(&mut self, fields: &[FieldInit], level: u32, column: usize) -> String {
        if fields.is_empty() {
            return "{ }".to_string();
        }
        let flat: Vec<String> = fields
            .iter()
            .map(|f| {
                format!(
                    "{}: {}",
                    self.interner.resolve(f.name.name),
                    render_flat_expr(&f.value, self.interner)
                )
            })
            .collect();
        let brackets = Brackets {
            open_flat: "{ ",
            close_flat: " }",
            open_broken: "{",
            close_broken: "}",
        };
        lay_out_bracketed(brackets, fields, &flat, (level, column), |field, lvl| {
            let value = self.print_expr(&field.value, lvl, 0);
            format!("{}: {value}", self.interner.resolve(field.name.name))
        })
    }

    fn print_closure(&mut self, closure: &Closure, level: u32) -> String {
        let params = closure
            .params
            .iter()
            .map(|p| match &p.ty {
                Some(t) => format!("{}: {}", self.ident(&p.name), self.print_type(t)),
                None => self.ident(&p.name).to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let ret = self.print_return_type(closure.ret.as_ref());
        let head = format!("fn({params}){ret} {{");
        let mut lines = vec![head];
        lines.extend(self.print_block_lines(&closure.body, level));
        lines.push(format!("{}}}", indent(level)));
        lines.join("\n")
    }
}

/// The start of a match arm, which (unlike a statement or a top-level item)
/// has no span of its own: its first pattern's start stands in for it. Every
/// arm has at least one pattern (the grammar requires it).
fn match_arm_start(arm: &MatchArm) -> u32 {
    arm.patterns
        .first()
        .map_or(arm.body.span.start, |p| p.span.start)
}

fn indent(level: u32) -> String {
    INDENT_UNIT.repeat(level as usize)
}

/// Whether a flat rendering of length `first_line_len`, starting at
/// `column`, and further indented to `level`, still fits within [`WIDTH`].
fn fits(first_line_len: usize, level: u32, column: usize) -> bool {
    column + first_line_len + indent(level).len() <= WIDTH
}

/// A statement printed on its own line always starts at column 0, then gets
/// `pad` prefixed; a multi-line statement (a declaration whose value is a
/// closure, in particular) keeps its own later lines exactly as printed,
/// already indented for this same level.
fn one_or_more(text: &str, pad: &str) -> Vec<String> {
    format!("{pad}{text}")
        .split('\n')
        .map(String::from)
        .collect()
}

fn unary_op_str(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "not ",
    }
}

fn binary_op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "or",
        BinaryOp::And => "and",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Gt => ">",
        BinaryOp::Le => "<=",
        BinaryOp::Ge => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
    }
}

/// Decides whether a comma-separated bracketed construct (a list, a record,
/// a map or a call's arguments) prints flat, on one line, or broken, one
/// item to a line.
///
/// `flat_items` is every item's own flat rendering, computed once by
/// [`render_flat_expr`] rather than by calling `real`: `real` threads the
/// real comment cursor and would consume a comment twice over if it were
/// also used to decide the layout, and, since a closure is the only node
/// here that can hold a comment and a closure's own rendering is always
/// multi-line regardless, deciding from the flat rendering alone never
/// reaches a different answer than deciding from the real one would.
/// Measuring this way, rather than by calling the real, layout-deciding
/// printer twice at every level of nesting, is also what keeps this
/// quadratic rather than exponential in how deeply lists, records, maps and
/// calls nest inside one another.
/// The four delimiter strings of a bracketed construct: how it opens and
/// closes printed flat, and how it opens and closes printed broken (a record
/// literal's `{ ` and `{` differ this way; every other caller repeats the
/// same string in both positions).
#[derive(Copy, Clone)]
struct Brackets<'a> {
    open_flat: &'a str,
    close_flat: &'a str,
    open_broken: &'a str,
    close_broken: &'a str,
}

fn lay_out_bracketed<T>(
    brackets: Brackets<'_>,
    items: &[T],
    flat_items: &[String],
    at: (u32, usize),
    mut real: impl FnMut(&T, u32) -> String,
) -> String {
    let (level, column) = at;
    let Brackets {
        open_flat,
        close_flat,
        open_broken,
        close_broken,
    } = brackets;
    if items.is_empty() {
        return format!("{open_flat}{close_flat}");
    }
    if let ([only_item], [only_flat]) = (items, flat_items) {
        if only_flat.contains('\n') {
            // A single item whose own printed form already spans several
            // lines (overwhelmingly, a closure argument) stays snug around
            // the brackets rather than moving to its own indented line.
            return format!("{open_flat}{}{close_flat}", real(only_item, level));
        }
    }
    if flat_items.iter().all(|s| !s.contains('\n')) {
        let flat = format!("{open_flat}{}{close_flat}", flat_items.join(", "));
        if fits(flat.len(), level, column) {
            return flat;
        }
    }
    let pad = indent(level + 1);
    let count = items.len();
    let mut lines = Vec::with_capacity(count);
    for item in items {
        lines.push(format!("{pad}{}", real(item, level + 1)));
    }
    format!(
        "{open_broken}\n{}\n{}{close_broken}",
        lines.join(",\n"),
        indent(level)
    )
}

/// The flat, context-free rendering of an expression: no line breaks other
/// than those a nested closure's own block unconditionally has, no comments
/// (there are none to place in this position; see [`lay_out_bracketed`]).
/// Used only to measure whether a list, record, map or call fits on one
/// line; kept entirely separate from [`Printer::print_expr`] so that
/// measuring a node never also does the real, comment-consuming and
/// layout-deciding work of printing it.
fn render_flat_expr(expr: &Expr, interner: &Interner) -> String {
    match &expr.kind {
        ExprKind::Literal(lit) => match lit {
            Literal::Int { value, .. } => value.to_string(),
            Literal::Float { value, .. } => format_float(*value),
            Literal::Bool(b) => b.to_string(),
            Literal::None => "none".to_string(),
            Literal::String(sl) => render_flat_string(sl, interner),
            Literal::Filter { source, .. } => format!("`{}`", escape_filter_source(source)),
        },
        ExprKind::Name(id) => interner.resolve(id.name).to_string(),
        ExprKind::List(items) => {
            let joined = items
                .iter()
                .map(|e| render_flat_expr(e, interner))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{joined}]")
        }
        ExprKind::Map(entries) => {
            if entries.is_empty() {
                return "[:]".to_string();
            }
            let joined = entries
                .iter()
                .map(|e| {
                    format!(
                        "{}: {}",
                        render_flat_expr(&e.key, interner),
                        render_flat_expr(&e.value, interner)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{joined}]")
        }
        ExprKind::Record(fields) => {
            if fields.is_empty() {
                return "{ }".to_string();
            }
            let joined = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {}",
                        interner.resolve(f.name.name),
                        render_flat_expr(&f.value, interner)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {joined} }}")
        }
        // A closure's body is a block, which always breaks onto several
        // lines on its own account; the exact text is never read back, only
        // whether it contains a newline (see `lay_out_bracketed`), so a
        // fixed placeholder stands in for the real, multi-line rendering.
        ExprKind::Closure(_) => "fn() {\n}".to_string(),
        ExprKind::Paren(inner) => format!("({})", render_flat_expr(inner, interner)),
        ExprKind::Unary { op, operand } => format!(
            "{}{}",
            unary_op_str(*op),
            render_flat_expr(operand, interner)
        ),
        ExprKind::Binary { op, lhs, rhs } => {
            format!(
                "{} {} {}",
                render_flat_expr(lhs, interner),
                binary_op_str(*op),
                render_flat_expr(rhs, interner)
            )
        }
        ExprKind::Effect(inner) => format!("effect {}", render_flat_expr(inner, interner)),
        ExprKind::Call { callee, args } => {
            let joined = args
                .iter()
                .map(|a| render_flat_expr(a, interner))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({joined})", render_flat_expr(callee, interner))
        }
        ExprKind::Index { target, index } => format!(
            "{}[{}]",
            render_flat_expr(target, interner),
            render_flat_expr(index, interner)
        ),
        ExprKind::Field { target, name } => format!(
            "{}.{}",
            render_flat_expr(target, interner),
            interner.resolve(name.name)
        ),
        ExprKind::Try(inner) => format!("{}?", render_flat_expr(inner, interner)),
        ExprKind::Error => String::new(),
    }
}

fn render_flat_string(literal: &StringLiteral, interner: &Interner) -> String {
    let mut out = String::from("\"");
    for part in &literal.parts {
        match part {
            StringPart::Text(text) => out.push_str(&escape_string_text(text)),
            StringPart::Interpolation(expr) => {
                out.push_str("${");
                out.push_str(&render_flat_expr(expr, interner));
                out.push('}');
            }
        }
    }
    out.push('"');
    out
}

/// Reverses the parser's own unescaping of a string's text parts (section
/// 1.5), so a re-printed string parses back to the same value. `\"`, `\\`,
/// `\n`, `\t` and `\r` are always written as escapes; `$` is escaped only
/// when it precedes `{` in the output, the one case that would otherwise be
/// misread as opening an interpolation. Any other code point that section
/// 1.1 does not allow raw in source text (a C0 control, `U+007F`, a
/// bidirectional control, `U+FEFF`, or another code point outside space and
/// tab that Unicode treats as whitespace) is written as a `\u{...}` escape
/// instead of being copied through raw, which a `\u{...}` escape in the
/// original source can otherwise smuggle into the decoded text.
fn escape_string_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            c if needs_unicode_escape(c) => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Whether `c`, found in a string's already-decoded text, is not safe to
/// write back raw between the quotes (section 1.1's restrictions on source
/// text, which a string's own content is not exempt from).
fn needs_unicode_escape(c: char) -> bool {
    let code = u32::from(c);
    code <= 0x1F
        || code == 0x7F
        || c == '\u{feff}'
        || (0x202A..=0x202E).contains(&code)
        || (0x2066..=0x2069).contains(&code)
        || matches!(
            c,
            '\u{85}'
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
        )
        || ('\u{2000}'..='\u{200a}').contains(&c)
}

/// Reverses the parser's own unescaping of a filter literal's source
/// (section 1.6): a backtick and a backslash are the only two characters a
/// filter literal escapes, and a filter's decoded source (unlike a string's)
/// has no escape that could have smuggled in a character not already legal
/// to write raw, so nothing beyond these two needs re-escaping.
fn escape_filter_source(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for c in source.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            c => out.push(c),
        }
    }
    out
}

/// The text of a `Float` value, exactly as section 3.9 ("Float text")
/// specifies it for string interpolation: the shortest decimal digits and
/// exponent that read back as the same value (ties broken to an even final
/// digit), then written positionally when the exponent is from -5 to 15 and
/// with an explicit exponent otherwise. The syntax tree keeps only the
/// value of a `Float` literal, not the text it was written with, so this is
/// also the only choice available for printing one back as source: two
/// different spellings of the same value (`2.50`, `2.5`, `0002.5`) already
/// collapse to one node during parsing, and section 3.9's algorithm is the
/// specification's own definition of what that one node's canonical text is.
///
/// Rust's own shortest round-trip digit generation, used by the `{:e}`
/// format, already produces exactly the digits and exponent this needs; the
/// work here is only choosing between the two layouts section 3.9 defines
/// and placing the digits and the decimal point accordingly.
fn format_float(value: f64) -> String {
    if value == 0.0 {
        // "A zero is `0.0`"; a negative zero has no separate spelling.
        return "0.0".to_string();
    }
    if !value.is_finite() {
        // A `Float` literal whose value would round to an infinity is
        // already a lexical error (HS0015), so a clean parse never reaches
        // this; kept total, and away from `{:e}`'s own "inf"/"NaN" text,
        // rather than assuming it.
        return "0.0".to_string();
    }
    let negative = value.is_sign_negative();
    let rendered = format!("{:e}", value.abs());
    let Some((mantissa, exponent_text)) = rendered.split_once('e') else {
        return rendered;
    };
    let Ok(exponent) = exponent_text.parse::<i32>() else {
        return rendered;
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digit_count = i32::try_from(digits.len()).unwrap_or(i32::MAX);

    let mut body = String::new();
    if (-5..=15).contains(&exponent) {
        if exponent >= 0 {
            let integer_len = exponent + 1;
            if digit_count <= integer_len {
                body.push_str(&digits);
                body.push_str(&"0".repeat(usize::try_from(integer_len - digit_count).unwrap_or(0)));
                body.push_str(".0");
            } else {
                let split = usize::try_from(integer_len).unwrap_or(0);
                body.push_str(digits.get(..split).unwrap_or(&digits));
                body.push('.');
                body.push_str(digits.get(split..).unwrap_or(""));
            }
        } else {
            body.push_str("0.");
            body.push_str(&"0".repeat(usize::try_from(-exponent - 1).unwrap_or(0)));
            body.push_str(&digits);
        }
    } else {
        let mut chars = digits.chars();
        if let Some(first) = chars.next() {
            body.push(first);
        }
        let rest: String = chars.collect();
        if !rest.is_empty() {
            body.push('.');
            body.push_str(&rest);
        }
        body.push('e');
        body.push_str(&exponent.to_string());
    }
    if negative { format!("-{body}") } else { body }
}

#[cfg(test)]
mod tests {
    use super::format;
    use hollowscript_core::{Control, Diagnostic, Interner, Source, SourceId};

    fn run(source: &str) -> super::FormatOutput {
        let src = Source::new(SourceId(0), "main.hws", source).unwrap();
        format(&src, &mut Interner::new(), &mut Control::none())
    }

    fn expect_same(source: &str) {
        let out = run(source);
        assert_eq!(
            out.output.as_deref(),
            Some(source),
            "diagnostics: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_source_already_in_canonical_form_is_unchanged() {
        expect_same(
            "constant a = (1 + 2) * 3\n\n\
             constant b = 10 - (4 - 3)\n\n\
             constant c = not (a > 1 and b > 1)\n\n\
             if c {\n    return [0]\n}\n\n\
             return [a, b]\n",
        );
    }

    #[test]
    fn spacing_is_normalised() {
        let out = run(
            "uses   print\nconstant   a=1+2*3\nif a>5{effect   print(\"big ${a}\")}\nfn   add(x:Int,y:Int):Int{return x+y}\nreturn add( a , 1 )\n",
        );
        assert_eq!(
            out.output.as_deref(),
            Some(
                "uses print\n\nconstant a = 1 + 2 * 3\n\nif a > 5 {\n    effect print(\"big ${a}\")\n}\n\nfn add(x: Int, y: Int): Int {\n    return x + y\n}\n\nreturn add(a, 1)\n"
            ),
            "diagnostics: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn crlf_and_repeated_blank_lines_become_one_canonical_lf_form() {
        let out = run("constant a = 1\r\n\r\n\r\n\r\nconstant b = 2\r\nreturn a + b\r\n");
        assert_eq!(
            out.output.as_deref(),
            Some("constant a = 1\n\nconstant b = 2\n\nreturn a + b\n"),
            "diagnostics: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_source_with_a_syntax_error_is_refused() {
        let out = run("constant a = \n");
        assert!(out.output.is_none());
        assert!(out.diagnostics.iter().any(Diagnostic::is_error));
    }

    #[test]
    fn a_leading_comment_is_kept_and_a_blank_line_between_declarations_is_added() {
        let out = run("// just a plain comment\nconstant x = 1\nreturn x\n");
        assert_eq!(
            out.output.as_deref(),
            Some("// just a plain comment\nconstant x = 1\n\nreturn x\n"),
            "diagnostics: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_trailing_comment_stays_on_its_line() {
        let out = run("constant x = 1 // why one\nconstant y = 2\n");
        assert_eq!(
            out.output.as_deref(),
            Some("constant x = 1 // why one\n\nconstant y = 2\n"),
            "diagnostics: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn a_comment_that_opens_the_next_arm_is_not_read_as_trailing_the_previous_one() {
        // Found by the coverage-guided fuzzer of fuzz/fuzz_targets/format_roundtrip.rs
        // (see docs/design/fuzzing.md): two match arms on one line, the
        // second's block opening with a comment right after its `{`. The
        // comment belongs to arm `2`, not arm `1`, even though it shares
        // arm `1`'s line with nothing but whitespace and `2 {` between them.
        let source = "constant n = 1\n\nmatch n {\n    1 { return 1 }   2 { // note\n        return 2\n    }\n    else { return 0 }\n}\n";
        let once = run(source)
            .output
            .expect("this source has a place for its one comment");
        assert_eq!(
            once,
            "constant n = 1\n\nmatch n {\n    1 { return 1 }\n    2 {\n        // note\n        return 2\n    }\n    else { return 0 }\n}\n",
            "the comment must stay with arm `2`, not become trailing text on arm `1`'s line"
        );
        let src = Source::new(SourceId(0), "main.hws", once.clone()).unwrap();
        let twice = format(&src, &mut Interner::new(), &mut Control::none()).output;
        assert_eq!(
            twice.as_deref(),
            Some(once.as_str()),
            "format must be idempotent"
        );
    }

    #[test]
    fn a_comment_with_no_place_is_refused_and_names_its_own_location() {
        let source = "constant xs = [\n    1, // one\n    2\n]\n";
        let out = run(source);
        assert_eq!(out.output, None);
        assert_eq!(out.diagnostics.len(), 1);
        let diagnostic = out.diagnostics.first().expect("checked above");
        assert!(diagnostic.is_error());
        assert!(diagnostic.message.to_lowercase().contains("comment"));
        let location = diagnostic
            .location
            .as_ref()
            .expect("a stray comment has a location");
        assert_eq!(
            &source[location.span.start as usize..location.span.end as usize],
            "// one"
        );
    }

    #[test]
    fn formatting_a_file_with_comments_is_idempotent() {
        let source =
            "// a\nconstant x = 1 // b\n\n// c\nfn f() {\n    // d\n    return // e\n}\n// f\n";
        let once = run(source)
            .output
            .expect("this source has a place for every comment");
        let src = Source::new(SourceId(0), "main.hws", once.clone()).unwrap();
        let twice = format(&src, &mut Interner::new(), &mut Control::none()).output;
        assert_eq!(twice.as_deref(), Some(once.as_str()));
    }

    #[test]
    fn load_bearing_parentheses_the_record_literal_condition_survive() {
        expect_same("if ({ a: 1 }).a == 1 {\n    return 9\n}\n\nreturn [9, 9]\n");
    }

    #[test]
    fn a_call_argument_list_breaks_when_it_does_not_fit_on_one_line() {
        let out = run(
            "return f(aVeryLongArgumentNameIndeed, anotherVeryLongArgumentNameThatPushesThisOverTheLimit, yetAnotherOne)\n",
        );
        let formatted = out.output.expect("this source has no comment");
        assert!(formatted.contains("return f(\n    aVeryLongArgumentNameIndeed,\n"));
    }

    #[test]
    fn float_text_follows_section_3_9() {
        assert_eq!(super::format_float(1.0), "1.0");
        assert_eq!(super::format_float(100.0), "100.0");
        assert_eq!(super::format_float(1.5), "1.5");
        assert_eq!(super::format_float(0.1), "0.1");
        assert_eq!(super::format_float(0.000_01), "0.00001");
        assert_eq!(super::format_float(0.000_001), "1e-6");
        assert_eq!(
            super::format_float(1_000_000_000_000_000.0),
            "1000000000000000.0"
        );
        assert_eq!(super::format_float(10_000_000_000_000_000.0), "1e16");
        assert_eq!(super::format_float(1.5e20), "1.5e20");
        assert_eq!(super::format_float(-2.5e-7), "-2.5e-7");
        assert_eq!(super::format_float(-0.0), "0.0");
        assert_eq!(super::format_float(2.0 / 3.0), "0.6666666666666666");
    }

    /// A `match` whose arms are single statements, nested inside such an arm,
    /// to `depth` levels: every arm but the innermost holds the next `match`.
    fn nested_matches(depth: usize) -> String {
        use std::fmt::Write as _;
        let mut text = String::from("constant v = 1\n");
        for level in 0..depth {
            let pad = "    ".repeat(level * 2);
            let _ = write!(text, "{pad}match v {{\n{pad}    1 {{\n");
        }
        let _ = writeln!(text, "{}return 1", "    ".repeat(depth * 2));
        for level in (0..depth).rev() {
            let pad = "    ".repeat(level * 2);
            let _ = write!(
                text,
                "{pad}    }}\n{pad}    else {{\n{pad}        return 2\n{pad}    }}\n{pad}}}\n"
            );
        }
        text
    }

    /// Trying an arm's one statement on one line printed it twice at every
    /// level when it did not fit, so each level of nesting doubled the work:
    /// 24 nested arms took 12 seconds and 40 would not finish. The answer to
    /// the attempt is kept, so the work is quadratic at worst.
    #[test]
    fn nested_single_statement_arms_format_in_reasonable_time() {
        for depth in [3, 40, 120] {
            let source = nested_matches(depth);
            let (sender, receiver) = std::sync::mpsc::channel();
            let text = source.clone();
            std::thread::spawn(move || {
                let _ = sender.send(run(&text));
            });
            let out = receiver
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("formatting nested single-statement match arms did not finish");
            assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
            // Formatting its own output changes nothing.
            let formatted = out.output.expect("a clean source formats");
            assert_eq!(run(&formatted).output.as_deref(), Some(formatted.as_str()));
            if depth == 3 {
                // An arm that holds one statement is one line, and one that
                // holds a `match` is a block.
                assert_eq!(
                    formatted,
                    concat!(
                        "constant v = 1\n\nmatch v {\n    1 {\n        match v {\n            1 {\n",
                        "                match v {\n                    1 { return 1 }\n",
                        "                    else { return 2 }\n                }\n            }\n",
                        "            else { return 2 }\n        }\n    }\n    else { return 2 }\n}\n"
                    )
                );
            }
        }
    }

    /// The arms that do fit on one line are still written on one, at every
    /// level, so that keeping the attempt changes nothing about the output.
    #[test]
    fn a_one_statement_arm_still_goes_on_one_line() {
        expect_same("match 1 {\n    1 { return \"one\" }\n    else { return \"other\" }\n}\n");
        let out = run(
            "match 1 {\n    1 {\n        return 1\n    }\n    else {\n        match 2 {\n            2 {\n                return 3\n            }\n            else {\n                return 4\n            }\n        }\n    }\n}\n",
        );
        assert_eq!(
            out.output.as_deref(),
            Some(
                "match 1 {\n    1 { return 1 }\n    else {\n        match 2 {\n            2 { return 3 }\n            else { return 4 }\n        }\n    }\n}\n"
            )
        );
    }
}
