//! The diagnostic record (section 17.1), message templates, and the ordering
//! and capping of the diagnostics of one run.

pub use crate::codes::{Code, CodeInfo, Family, Severity, Stage, all};
use crate::limits::{LimitData, structural::MAX_DIAGNOSTICS_PER_FILE};
use crate::source::{FileName, Span};
use std::collections::BTreeMap;
use std::fmt::{self, Display, Write as _};

/// A place in a source: the file, the span and the line and column of its start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The canonical path of the module, or the name the host gave the source.
    pub file: FileName,
    /// The half-open byte range.
    pub span: Span,
    /// The 1-based line of `span.start`.
    pub line: u32,
    /// The 1-based column of `span.start`, in code points.
    pub column: u32,
}

/// A further location that explains a diagnostic, such as the first
/// declaration of a name that is declared twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Related {
    /// Where.
    pub location: Location,
    /// A short message.
    pub message: String,
}

/// One frame of the call stack of a fault (section 8.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The function name, or `<closure>`.
    pub function: String,
    /// The call site, or for a closure the span of the closure literal.
    pub location: Location,
}

/// A diagnostic: a problem found in a program, or a fault while it ran, or an
/// error in a host request to an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The stable code.
    pub code: Code,
    /// The severity, from the registry.
    pub severity: Severity,
    /// The message, from the template of the code with the placeholders filled in.
    pub message: String,
    /// Where the diagnostic is. `None` for the host errors HS1201 to HS1205,
    /// which answer a host operation and have no file, span, line or column.
    pub location: Option<Location>,
    /// Further locations.
    pub related: Vec<Related>,
    /// For faults only: the innermost frames, at most 32.
    pub frames: Vec<Frame>,
    /// For the limit faults HS1001 to HS1005 only.
    pub data: Option<LimitData>,
}

impl Diagnostic {
    /// A diagnostic for `code` with its template filled in from `args`. A
    /// placeholder that has no argument is left as it is in the message.
    #[must_use]
    pub fn new(code: Code, args: &[(&str, &dyn Display)]) -> Diagnostic {
        let info = code.info();
        Diagnostic {
            code,
            severity: info.severity,
            message: render_template(info.template, args),
            location: None,
            related: Vec::new(),
            frames: Vec::new(),
            data: None,
        }
    }

    /// The diagnostic used where a feature is not built yet. It is the
    /// internal error HS1099, so no code outside the specification exists.
    #[must_use]
    pub fn not_implemented(what: &str) -> Diagnostic {
        Diagnostic::new(
            Code::HS1099,
            &[("message", &format_args!("{what} is not implemented yet"))],
        )
    }

    /// Sets the location.
    #[must_use]
    pub fn with_location(mut self, location: Location) -> Diagnostic {
        self.location = Some(location);
        self
    }

    /// Adds a related location.
    #[must_use]
    pub fn with_related(mut self, location: Location, message: impl Into<String>) -> Diagnostic {
        self.related.push(Related {
            location,
            message: message.into(),
        });
        self
    }

    /// Sets the frames of a fault, keeping the innermost 32.
    #[must_use]
    pub fn with_frames(mut self, mut frames: Vec<Frame>) -> Diagnostic {
        frames.truncate(crate::limits::structural::MAX_FRAMES);
        self.frames = frames;
        self
    }

    /// Sets the limit data of a limit fault.
    #[must_use]
    pub fn with_data(mut self, data: LimitData) -> Diagnostic {
        self.data = Some(data);
        self
    }

    /// Whether this diagnostic has severity error.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(l) = &self.location {
            write!(f, "{}:{}:{}: ", l.file, l.line, l.column)?;
        }
        write!(
            f,
            "{}[{}]: {}",
            self.severity.as_str(),
            self.code,
            self.message
        )
    }
}

/// Fills the `{name}` placeholders of a template. Text in braces that is not
/// a plain name, such as the `{...}` of a unicode escape in HS0008, is kept.
#[must_use]
pub fn render_template(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).unwrap_or(""));
        let after = rest.get(open + 1..).unwrap_or("");
        let Some(close) = after.find('}') else {
            out.push('{');
            rest = after;
            continue;
        };
        let name = after.get(..close).unwrap_or("");
        let is_name = name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && name.bytes().all(|b| b.is_ascii_alphanumeric());
        if let Some((_, value)) = args.iter().find(|(k, _)| is_name && *k == name) {
            let _ = write!(out, "{value}");
        } else {
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = after.get(close + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// The placeholders of a template, in order of appearance.
#[must_use]
pub fn placeholders(template: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = rest.get(open + 1..).unwrap_or("");
        let Some(close) = after.find('}') else { break };
        let name = after.get(..close).unwrap_or("");
        if name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && name.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            found.push(name);
        }
        rest = after.get(close + 1..).unwrap_or("");
    }
    found
}

/// How many diagnostics of one file a collector retains: the 100 that section
/// 17.1 keeps, and the one after them, whose position the notice that says
/// more were found is given.
const KEPT_PER_FILE: usize = MAX_DIAGNOSTICS_PER_FILE + 1;

/// The order diagnostics of one file are kept in: by start ascending, then end
/// ascending, then errors before warnings, then code ascending.
fn in_file_order(a: &Diagnostic, b: &Diagnostic) -> std::cmp::Ordering {
    let key = |d: &Diagnostic| {
        (
            d.location.as_ref().map(|l| (l.span.start, l.span.end)),
            d.severity,
            d.code,
        )
    };
    key(a).cmp(&key(b))
}

/// Sorts one file's diagnostics into the canonical order, keeping the first
/// [`KEPT_PER_FILE`]. The sort is stable, so of two diagnostics that are equal
/// in every respect the one found first is kept first.
fn keep_first_of(bucket: &mut Vec<Diagnostic>) {
    bucket.sort_by(in_file_order);
    bucket.truncate(KEPT_PER_FILE);
}

/// A speculative stretch of a [`BoundedDiagnostics`]: what was reported since
/// [`BoundedDiagnostics::begin_speculation`].
#[derive(Clone, Debug, Default)]
struct Speculation {
    items: Vec<Diagnostic>,
    errors: bool,
}

/// Diagnostics collected under the per-file cap of section 17.1 as they arrive,
/// so that a source that provokes millions of them holds a few hundred, not
/// millions.
///
/// What the cap keeps is the first 100 of a file in the canonical order (see
/// [`DiagnosticSet::finish`]), and one more, the first that is dropped, which
/// is where the notice goes. A diagnostic is never needed again once 101 that
/// come before it in that order have been found, so this holds the smallest
/// 101 of each file: whenever a file's pile reaches twice that it is sorted
/// and cut back, which keeps the work for each diagnostic constant on average
/// and the memory for each file under 202 diagnostics. The result is exactly
/// what collecting everything and capping at the end would give, whatever the
/// order they were found in.
///
/// A diagnostic with no location (a host error) is not capped.
#[derive(Clone, Debug, Default)]
pub struct BoundedDiagnostics {
    files: BTreeMap<FileName, Vec<Diagnostic>>,
    unlocated: Vec<Diagnostic>,
    speculative: Vec<Speculation>,
    total: usize,
    errors: bool,
}

impl BoundedDiagnostics {
    /// An empty collector.
    #[must_use]
    pub fn new() -> BoundedDiagnostics {
        BoundedDiagnostics::default()
    }

    /// Adds a diagnostic. Inside a speculation (see
    /// [`BoundedDiagnostics::begin_speculation`]) it is held apart, uncapped,
    /// until the speculation ends.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        if let Some(top) = self.speculative.last_mut() {
            top.errors |= diagnostic.is_error();
            top.items.push(diagnostic);
            return;
        }
        self.total += 1;
        self.errors |= diagnostic.is_error();
        let Some(file) = diagnostic.location.as_ref().map(|l| l.file.clone()) else {
            self.unlocated.push(diagnostic);
            return;
        };
        let bucket = self.files.entry(file).or_default();
        bucket.push(diagnostic);
        if bucket.len() >= 2 * KEPT_PER_FILE {
            keep_first_of(bucket);
        }
    }

    /// Adds every diagnostic of `diagnostics`.
    pub fn extend(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        for diagnostic in diagnostics {
            self.push(diagnostic);
        }
    }

    /// Starts a speculation: what is reported until the matching
    /// [`BoundedDiagnostics::discard_speculation`] is held apart and then
    /// thrown away, for a caller that checks something only to learn a fact
    /// about it. Speculations nest.
    pub fn begin_speculation(&mut self) {
        self.speculative.push(Speculation::default());
    }

    /// Ends the innermost speculation, dropping what it collected.
    pub fn discard_speculation(&mut self) {
        self.speculative.pop();
    }

    /// The number of diagnostics reported, before capping. A speculation's are
    /// not counted.
    #[must_use]
    pub fn total(&self) -> usize {
        self.total
    }

    /// The number of diagnostics held now, which the cap keeps under 202 for
    /// each file, plus those without a location.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.values().map(Vec::len).sum::<usize>() + self.unlocated.len()
    }

    /// Whether nothing has been reported.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Whether any diagnostic reported so far, held or dropped by the cap, or
    /// held by a speculation that is still open, is an error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.errors || self.speculative.iter().any(|s| s.errors)
    }

    /// The diagnostics held, file by file in the order of the files' names and
    /// without a location last. Within a file they are in the order found
    /// until the cap has had to cut the file back, and canonical after.
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.files.values().flatten().chain(self.unlocated.iter())
    }

    /// The diagnostics held, at most 101 for each file, in the order
    /// [`BoundedDiagnostics::iter`] gives.
    #[must_use]
    pub fn into_vec(mut self) -> Vec<Diagnostic> {
        for bucket in self.files.values_mut() {
            if bucket.len() > KEPT_PER_FILE {
                keep_first_of(bucket);
            }
        }
        let mut out = Vec::with_capacity(self.len());
        for (_, bucket) in self.files {
            out.extend(bucket);
        }
        out.extend(self.unlocated);
        out
    }
}

impl IntoIterator for BoundedDiagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

/// Caps `diagnostics`, which may be in any order, as section 17.1 caps a run:
/// at most 100 for the file, in the canonical order, and one HS0004 after them
/// if more were found. A list of 100 or fewer is returned as it is.
#[must_use]
pub fn cap_diagnostics(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    if diagnostics.len() <= MAX_DIAGNOSTICS_PER_FILE {
        return diagnostics;
    }
    let mut set = DiagnosticSet::new();
    for diagnostic in diagnostics {
        set.push(diagnostic);
    }
    set.finish(&[])
}

/// Collects the diagnostics of one run and puts them in the canonical order
/// with the per-file cap of section 17.1. The cap is applied as diagnostics
/// are added (see [`BoundedDiagnostics`]), so a run that provokes millions of
/// them holds a few hundred per file.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticSet {
    collected: BoundedDiagnostics,
}

impl DiagnosticSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> DiagnosticSet {
        DiagnosticSet::default()
    }

    /// Adds a diagnostic.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.collected.push(diagnostic);
    }

    /// The number of diagnostics added, before capping.
    #[must_use]
    pub fn len(&self) -> usize {
        self.collected.total()
    }

    /// Whether nothing has been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.collected.is_empty()
    }

    /// Whether any added diagnostic is an error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.collected.has_errors()
    }

    /// The diagnostics in the canonical order, capped.
    ///
    /// Order: by file, in load order (`file_order`); within a file by start
    /// ascending, then end ascending, then errors before warnings, then code
    /// ascending. A file that is not in `file_order` follows the ones that are,
    /// in the order of the names. Diagnostics without a location, which are host
    /// errors, come last. At most 100 diagnostics are kept for one file, followed
    /// by exactly one HS0004 whose empty span is at the start of the first one
    /// dropped.
    #[must_use]
    pub fn finish(self, file_order: &[FileName]) -> Vec<Diagnostic> {
        let BoundedDiagnostics {
            mut files,
            mut unlocated,
            ..
        } = self.collected;
        let mut out: Vec<Diagnostic> = Vec::new();
        let mut emit = |mut bucket: Vec<Diagnostic>| {
            bucket.sort_by(in_file_order);
            let dropped = bucket.get(MAX_DIAGNOSTICS_PER_FILE).cloned();
            bucket.truncate(MAX_DIAGNOSTICS_PER_FILE);
            out.extend(bucket);
            if let Some(first) = dropped.and_then(|d| d.location) {
                let at = Location {
                    file: first.file.clone(),
                    span: Span::empty(first.span.start),
                    line: first.line,
                    column: first.column,
                };
                let limit = MAX_DIAGNOSTICS_PER_FILE;
                out.push(Diagnostic::new(Code::HS0004, &[("limit", &limit)]).with_location(at));
            }
        };
        for name in file_order {
            if let Some(bucket) = files.remove(name) {
                emit(bucket);
            }
        }
        for (_, bucket) in files {
            emit(bucket);
        }
        unlocated.sort_by_key(|d| (d.severity, d.code));
        out.extend(unlocated);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoundedDiagnostics, Code, Diagnostic, DiagnosticSet, Location, Severity, placeholders,
        render_template,
    };
    use crate::source::{FileName, Span};
    use std::sync::Arc;

    fn at(file: &str, start: u32, end: u32) -> Location {
        Location {
            file: Arc::from(file),
            span: Span::new(start, end),
            line: start + 1,
            column: 1,
        }
    }

    fn diag(code: Code, file: &str, start: u32, end: u32) -> Diagnostic {
        Diagnostic::new(code, &[]).with_location(at(file, start, end))
    }

    #[test]
    fn templates_fill_named_placeholders_only() {
        let message = render_template(
            "Expected {expected}, found {found} {...} {Upper}.",
            &[("expected", &"Int"), ("found", &3)],
        );
        assert_eq!(message, "Expected Int, found 3 {...} {Upper}.");
        assert_eq!(render_template("{a", &[]), "{a");
        assert_eq!(render_template("x {missing} y", &[]), "x {missing} y");
    }

    #[test]
    fn placeholders_are_listed_in_order() {
        assert_eq!(placeholders(Code::HS0201.info().template), vec!["name"]);
        assert_eq!(placeholders("{a} {...} {b}"), vec!["a", "b"]);
    }

    #[test]
    fn new_takes_severity_from_the_registry() {
        assert_eq!(
            Diagnostic::new(Code::HS0801, &[("name", &"x")]).severity,
            Severity::Warning
        );
        assert!(Diagnostic::new(Code::HS0301, &[]).is_error());
        let d = Diagnostic::not_implemented("the parser");
        assert_eq!(d.code, Code::HS1099);
        assert_eq!(
            d.message,
            "Internal error: the parser is not implemented yet."
        );
    }

    #[test]
    fn order_is_file_then_span_then_severity_then_code() {
        let files: Vec<FileName> = vec![Arc::from("main.hws"), Arc::from("a.hws")];
        let mut set = DiagnosticSet::new();
        set.push(diag(Code::HS0801, "a.hws", 0, 1));
        set.push(diag(Code::HS0301, "a.hws", 5, 9));
        set.push(diag(Code::HS0801, "main.hws", 4, 5));
        set.push(diag(Code::HS0205, "main.hws", 4, 5));
        set.push(diag(Code::HS0201, "main.hws", 4, 5));
        set.push(diag(Code::HS0301, "main.hws", 4, 8));
        set.push(Diagnostic::new(Code::HS1201, &[("name", &"f")]));
        let codes: Vec<(String, u32)> = set
            .finish(&files)
            .iter()
            .map(|d| {
                (
                    d.code.to_text(),
                    d.location.as_ref().map_or(0, |l| l.span.start),
                )
            })
            .collect();
        let expected = [
            "HS0201", "HS0205", "HS0801", "HS0301", "HS0801", "HS0301", "HS1201",
        ];
        assert_eq!(
            codes.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn at_most_one_hundred_per_file_then_one_notice() {
        let files: Vec<FileName> = vec![Arc::from("main.hws"), Arc::from("m.hws")];
        let mut set = DiagnosticSet::new();
        for i in 0..130 {
            set.push(diag(Code::HS0301, "main.hws", i, i + 1));
        }
        set.push(diag(Code::HS0301, "m.hws", 0, 1));
        let out = set.finish(&files);
        assert_eq!(out.len(), 100 + 1 + 1);
        let notice = &out[100];
        assert_eq!(notice.code, Code::HS0004);
        let l = notice.location.as_ref().unwrap();
        assert_eq!((&*l.file, l.span.start, l.span.end), ("main.hws", 100, 100));
        assert!(notice.message.contains("100"));
        assert_eq!(out[101].location.as_ref().map(|l| &*l.file), Some("m.hws"));
    }

    #[test]
    fn exactly_one_hundred_has_no_notice() {
        let files: Vec<FileName> = vec![Arc::from("main.hws")];
        let mut set = DiagnosticSet::new();
        for i in 0..100 {
            set.push(diag(Code::HS0801, "main.hws", i, i));
        }
        assert_eq!(set.finish(&files).len(), 100);
    }

    #[test]
    fn the_cap_is_exact_whatever_order_the_diagnostics_are_found_in() {
        // Found in descending order, a million of them for one file: the
        // collector holds a few hundred, and what it answers is still the
        // first hundred of the canonical order and the notice at the 101st.
        let files: Vec<FileName> = vec![Arc::from("main.hws")];
        let mut set = DiagnosticSet::new();
        for i in (0..200_000).rev() {
            set.push(diag(Code::HS0301, "main.hws", i, i + 1));
        }
        assert_eq!(set.len(), 200_000);
        assert!(set.has_errors());
        let out = set.finish(&files);
        assert_eq!(out.len(), 101);
        let starts: Vec<u32> = out[..100]
            .iter()
            .map(|d| d.location.as_ref().unwrap().span.start)
            .collect();
        assert_eq!(starts, (0..100).collect::<Vec<u32>>());
        assert_eq!(out[100].code, Code::HS0004);
        assert_eq!(out[100].location.as_ref().unwrap().span.start, 100);
    }

    #[test]
    fn the_collector_holds_a_few_hundred_diagnostics_however_many_are_found() {
        let mut collected = BoundedDiagnostics::new();
        for i in 0..1_000_000_u32 {
            collected.push(diag(Code::HS0006, "q.hws", i, i + 1));
            assert!(collected.len() < 202, "{} held after {i}", collected.len());
        }
        assert_eq!(collected.total(), 1_000_000);
        assert_eq!(collected.into_vec().len(), 101);
    }

    #[test]
    fn an_error_dropped_by_the_cap_still_counts_as_an_error() {
        let mut collected = BoundedDiagnostics::new();
        for i in 0..500 {
            collected.push(diag(Code::HS0801, "main.hws", i, i));
        }
        assert!(!collected.has_errors());
        // After every warning in the order, so the cap drops it.
        collected.push(diag(Code::HS0301, "main.hws", 10_000, 10_001));
        assert!(collected.has_errors());
    }

    #[test]
    fn each_file_is_capped_on_its_own() {
        let files: Vec<FileName> = vec![Arc::from("a.hws"), Arc::from("b.hws")];
        let mut set = DiagnosticSet::new();
        for i in 0..300 {
            set.push(diag(Code::HS0301, "b.hws", i, i + 1));
            set.push(diag(Code::HS0301, "a.hws", i, i + 1));
        }
        let out = set.finish(&files);
        assert_eq!(out.len(), 202);
        assert_eq!(out[0].location.as_ref().map(|l| &*l.file), Some("a.hws"));
        assert_eq!(out[100].code, Code::HS0004);
        assert_eq!(out[101].location.as_ref().map(|l| &*l.file), Some("b.hws"));
        assert_eq!(out[201].code, Code::HS0004);
    }

    #[test]
    fn a_speculation_is_dropped_and_its_errors_count_while_it_is_open() {
        let mut collected = BoundedDiagnostics::new();
        collected.push(diag(Code::HS0801, "main.hws", 0, 1));
        collected.begin_speculation();
        assert!(!collected.has_errors());
        collected.push(diag(Code::HS0301, "main.hws", 2, 3));
        assert!(collected.has_errors());
        collected.begin_speculation();
        collected.push(diag(Code::HS0301, "main.hws", 4, 5));
        collected.discard_speculation();
        collected.discard_speculation();
        assert!(!collected.has_errors());
        assert_eq!(collected.total(), 1);
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn a_list_of_a_hundred_or_fewer_is_capped_as_it_is() {
        let few: Vec<Diagnostic> = (0..100)
            .rev()
            .map(|i| diag(Code::HS0301, "main.hws", i, i + 1))
            .collect();
        assert_eq!(super::cap_diagnostics(few.clone()), few);
        let many: Vec<Diagnostic> = (0..150)
            .rev()
            .map(|i| diag(Code::HS0301, "main.hws", i, i + 1))
            .collect();
        let capped = super::cap_diagnostics(many);
        assert_eq!(capped.len(), 101);
        assert_eq!(capped[100].code, Code::HS0004);
    }
}
