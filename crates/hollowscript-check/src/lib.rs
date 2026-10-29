//! The HollowScript checker.
//!
//! Turns a parsed [`hollowscript_syntax::ast::File`] and its imports into a
//! typed HIR and the diagnostics of chapters 2 and 4 to 10: name resolution,
//! types and inference, optional narrowing, effects and capabilities, pattern
//! exhaustiveness, and module loading and initialisation order.
//!
//! - [`types`]: type representation and interning;
//! - [`hir`]: the typed tree the checker produces;
//! - [`resolve`]: name resolution, the prelude and the module path algorithm
//!   of chapter 10;
//! - [`infer`]: types and inference (chapter 2) and the expression and
//!   statement checking driver built on it;
//! - [`narrow`]: optional narrowing (section 4.8);
//! - [`effects`]: effects and capabilities (chapter 9);
//! - [`exhaustive`]: pattern exhaustiveness (section 7.7).
//!
//! [`check`] loads the entry module and every module it imports, transitively
//! (section 10.5), parsing each with [`hollowscript_syntax::parse`] and
//! resolving its import paths with [`resolve::resolve_import_path`], then
//! checks every module with [`infer::check_module`] in dependency order, so
//! that an importer always sees its imports' real exports. Diagnostics from
//! every module are collected and returned in the canonical order of section
//! 17.1.

#![forbid(unsafe_code)]

pub mod early_use;
pub mod effects;
pub mod exhaustive;
pub mod hir;
pub mod infer;
pub mod narrow;
pub mod resolve;
pub mod shared;
pub mod spans;
pub mod types;

use hollowscript_core::limits::structural::{MAX_MODULES, MAX_PROGRAM_BYTES};
use hollowscript_core::{
    Code, Control, Diagnostic, DiagnosticSet, FileName, Interner, LoadModuleError, Source,
    SourceId, Span,
};
use hollowscript_syntax::ast::File;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The result of checking a program.
#[derive(Debug)]
pub struct CheckOutput {
    /// The typed program.
    pub hir: hir::Program,
    /// Every diagnostic, already in the canonical order of section 17.1.
    pub diagnostics: Vec<Diagnostic>,
    /// Every filter literal of the program, found whether or not the program
    /// checks cleanly: grouped by module in the order the modules were
    /// checked (an imported module before the modules that import it, the
    /// entry module last) and, within a module, in source order, each literal
    /// once.
    pub filter_literals: Vec<FilterLiteral>,
}

/// A filter literal (Appendix A) the checker found in a program: what a host
/// that evaluates filters needs to know before the program runs, since a
/// `Filter` value reaches a capability as its source text alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterLiteral {
    /// The filter source: the text between the backticks with `` \` `` and
    /// `\\` unescaped, exactly what a capability that is given the filter
    /// receives (section A.1).
    pub source: String,
    /// Where the whole literal is, backticks included: the module, the byte
    /// span, and the line and column of its start.
    pub location: hollowscript_core::Location,
}

/// The capability interface of a run (section 9.7): the capabilities a host
/// has granted, by name, with their types. This is [`effects::Grants`]
/// itself; the alias is what a host and the rest of the checker are expected
/// to name it by.
pub type CapabilityInterface = effects::Grants;

/// One module discovered while loading the program (section 10.5), before it
/// is checked: its parsed tree and the source it was parsed from, kept so
/// that a diagnostic about it can be located.
struct LoadedModule {
    file: File,
    source: Source,
    dir: Vec<String>,
}

/// A checked module's index and exports, keyed by canonical path, so that a
/// later module's `import` lines can be resolved against it
/// ([`resolve_imports`]).
type CheckedExports = BTreeMap<String, (u32, Vec<(hollowscript_core::Symbol, hir::Export)>)>;

/// Loads the module graph reachable from the entry module (section 10.5):
/// parses each module the first time its canonical path is reached, in
/// depth first order following the `import` lines of each file, and reports
/// HS0601 to HS0612 for a problem with an import itself. Returns the loaded
/// modules keyed by canonical path, the order they were first reached in
/// (section 17.1's diagnostic order) and the dependency order every import
/// of a module precedes it in (section 10.5's initialisation order).
#[allow(clippy::too_many_arguments, reason = "one call site, the loader loop")]
fn load_graph(
    entry: &File,
    entry_source: &Source,
    names: &mut Interner,
    load_module: &mut dyn FnMut(&str) -> Result<String, LoadModuleError>,
    diagnostics: &mut DiagnosticSet,
) -> (BTreeMap<String, LoadedModule>, Vec<String>, Vec<String>) {
    let entry_path = entry_source.name().to_string();
    let entry_dir = dir_of(&entry_path);
    let mut modules: BTreeMap<String, LoadedModule> = BTreeMap::new();
    modules.insert(
        entry_path.clone(),
        LoadedModule {
            file: entry.clone(),
            source: entry_source.clone(),
            dir: entry_dir,
        },
    );
    let mut load_order = vec![entry_path.clone()];
    let mut dep_order = Vec::new();
    let mut visiting = vec![entry_path.clone()];
    let mut total_bytes = entry_source.len();
    let mut next_source_id = 1u32;
    visit_module(
        &entry_path,
        names,
        load_module,
        diagnostics,
        &mut modules,
        &mut load_order,
        &mut dep_order,
        &mut visiting,
        &mut total_bytes,
        &mut next_source_id,
    );
    // `visit_module` appends each module it discovers to `dep_order` once
    // its own imports are done (so every import precedes it, section 10.5);
    // the entry module is the one module whose own imports are processed
    // this way without a caller to append it afterwards, so it is appended
    // here instead, after all of its imports already are.
    dep_order.push(entry_path.clone());
    (modules, load_order, dep_order)
}

fn dir_of(canonical_path: &str) -> Vec<String> {
    let mut segments: Vec<String> = canonical_path.split('/').map(str::to_owned).collect();
    segments.pop();
    segments
}

#[allow(clippy::too_many_arguments, reason = "threaded loader state")]
fn visit_module(
    path: &str,
    names: &mut Interner,
    load_module: &mut dyn FnMut(&str) -> Result<String, LoadModuleError>,
    diagnostics: &mut DiagnosticSet,
    modules: &mut BTreeMap<String, LoadedModule>,
    load_order: &mut Vec<String>,
    dep_order: &mut Vec<String>,
    visiting: &mut Vec<String>,
    total_bytes: &mut usize,
    next_source_id: &mut u32,
) {
    let (imports, dir): (Vec<hollowscript_syntax::ast::ImportDecl>, Vec<String>) = {
        let Some(module) = modules.get(path) else {
            return;
        };
        (module.file.imports.clone(), module.dir.clone())
    };
    for import in &imports {
        let Some(raw_path) = single_text_segment(&import.path) else {
            diagnostics.push(
                Diagnostic::new(
                    Code::HS0602,
                    &[(
                        "reason",
                        &"the path must be a plain string with no interpolation",
                    )],
                )
                .with_location(location(path, modules, import.path.span)),
            );
            continue;
        };
        let resolved = resolve::resolve_import_path(&dir, &raw_path);
        let target = match resolved {
            Ok(Ok(segments)) => segments.join("/"),
            Ok(Err(())) => {
                diagnostics.push(Diagnostic::new(Code::HS0605, &[]).with_location(location(
                    path,
                    modules,
                    import.path.span,
                )));
                continue;
            }
            Err(resolve::ImportPathError::HasExtension) => {
                diagnostics.push(Diagnostic::new(Code::HS0609, &[]).with_location(location(
                    path,
                    modules,
                    import.path.span,
                )));
                continue;
            }
            Err(e) => {
                diagnostics.push(
                    Diagnostic::new(Code::HS0602, &[("reason", &e.reason())])
                        .with_location(location(path, modules, import.path.span)),
                );
                continue;
            }
        };
        if visiting.iter().any(|p| p == &target) {
            let mut chain = visiting.clone();
            chain.push(target.clone());
            let text = chain.join(", ");
            diagnostics.push(
                Diagnostic::new(Code::HS0603, &[("chain", &text)]).with_location(location(
                    path,
                    modules,
                    import.path.span,
                )),
            );
            continue;
        }
        if modules.contains_key(&target) {
            continue;
        }
        if modules.len() >= MAX_MODULES {
            diagnostics.push(
                Diagnostic::new(Code::HS0612, &[("limit", &MAX_MODULES)]).with_location(location(
                    path,
                    modules,
                    import.path.span,
                )),
            );
            continue;
        }
        let text = match load_module(&target) {
            Ok(text) => text,
            Err(e) => {
                diagnostics.push(e.to_diagnostic(&target).with_location(location(
                    path,
                    modules,
                    import.path.span,
                )));
                continue;
            }
        };
        *total_bytes += text.len();
        if *total_bytes > MAX_PROGRAM_BYTES {
            diagnostics.push(
                Diagnostic::new(Code::HS0612, &[("limit", &MAX_PROGRAM_BYTES)])
                    .with_location(location(path, modules, import.path.span)),
            );
            continue;
        }
        let source_id = SourceId(*next_source_id);
        *next_source_id += 1;
        let source = match Source::new(source_id, target.clone(), text) {
            Ok(s) => s,
            Err(e) => {
                let file_name: FileName = Arc::from(target.as_str());
                diagnostics.push(e.to_diagnostic(&file_name));
                continue;
            }
        };
        let mut control = Control::none();
        let parsed = hollowscript_syntax::parse(&source, names, &mut control);
        for d in parsed.diagnostics {
            diagnostics.push(d);
        }
        let target_dir = dir_of(&target);
        modules.insert(
            target.clone(),
            LoadedModule {
                file: parsed.file,
                source,
                dir: target_dir,
            },
        );
        load_order.push(target.clone());
        visiting.push(target.clone());
        visit_module(
            &target,
            names,
            load_module,
            diagnostics,
            modules,
            load_order,
            dep_order,
            visiting,
            total_bytes,
            next_source_id,
        );
        visiting.pop();
        dep_order.push(target);
    }
}

/// The text of an import path literal, if it has no interpolation
/// (HS0602 otherwise).
fn single_text_segment(path: &hollowscript_syntax::ast::StringLiteral) -> Option<String> {
    if path.malformed {
        return None;
    }
    let mut text = String::new();
    for part in &path.parts {
        match part {
            hollowscript_syntax::ast::StringPart::Text(t) => text.push_str(t),
            hollowscript_syntax::ast::StringPart::Interpolation(_) => return None,
        }
    }
    Some(text)
}

fn location(
    current: &str,
    modules: &BTreeMap<String, LoadedModule>,
    span: Span,
) -> hollowscript_core::Location {
    modules.get(current).map_or_else(
        || hollowscript_core::Location {
            file: Arc::from(current),
            span,
            line: 1,
            column: 1,
        },
        |m| m.source.locate(span),
    )
}

/// Checks the entry file of a program. `entry_source` is the source
/// `entry` was parsed from, needed to locate diagnostics against it; its
/// name is the entry module's own canonical path (section 10.3), which the
/// host is responsible for converting from whatever name it gave the entry
/// module. `load_module` resolves an import path to a file's text, as
/// section 10.3 describes; the checker calls it as it discovers imports.
/// Its `Err` distinguishes a module that simply is not there from one that
/// exists but could not be read, and carries the reason for the latter, so
/// that the checker can build HS0601 or HS0607 (with its `{reason}`) without
/// losing information the loader already had.
#[must_use]
#[allow(
    clippy::too_many_arguments,
    reason = "one entry point for the whole program"
)]
pub fn check(
    entry: &File,
    entry_source: &Source,
    names: &mut Interner,
    capabilities: &CapabilityInterface,
    filter_schema: Option<&hollowscript_core::filter::FilterSchema>,
    load_module: &mut dyn FnMut(&str) -> Result<String, LoadModuleError>,
) -> CheckOutput {
    let mut diagnostics = DiagnosticSet::new();
    let mut program = hir::Program::default();
    let capability_types = effects::capability_types(capabilities, &mut program.types, names);
    program
        .capability_types
        .clone_from(&capability_types.by_capability);

    let (modules, load_order, dep_order) =
        load_graph(entry, entry_source, names, load_module, &mut diagnostics);
    let entry_path = entry_source.name().to_string();

    let mut function_sigs: BTreeMap<u32, infer::FnSig> = BTreeMap::new();
    let mut global_types: BTreeMap<(u32, u32), (types::TypeId, bool)> = BTreeMap::new();
    let mut functions: BTreeMap<u32, hir::Function> = BTreeMap::new();
    let mut next_function_id = 0u32;
    let mut checked_by_path: CheckedExports = BTreeMap::new();
    let mut filter_literals = Vec::new();

    for (module_index, path) in dep_order.iter().enumerate() {
        let Some(loaded) = modules.get(path) else {
            continue;
        };
        let module_index = u32::try_from(module_index).unwrap_or(u32::MAX);
        let imports = resolve_imports(
            &loaded.file,
            &checked_by_path,
            &program,
            names,
            &loaded.source,
            &mut diagnostics,
        );
        let checked = infer::check_module(
            &loaded.file,
            module_index,
            *path == entry_path,
            path.clone(),
            &imports,
            capabilities,
            &capability_types,
            &mut program.types,
            names,
            &loaded.source,
            &mut function_sigs,
            &mut global_types,
            &mut functions,
            &mut next_function_id,
            filter_schema,
        );
        for d in checked.diagnostics {
            diagnostics.push(d);
        }
        filter_literals.extend(checked.filter_literals);
        checked_by_path.insert(path.clone(), (module_index, checked.module.exports.clone()));
        program.modules.push(checked.module);
    }
    program.functions = (0..next_function_id)
        .map(|id| {
            functions.remove(&id).unwrap_or_else(|| hir::Function {
                node: hollowscript_syntax::ast::NodeId(0),
                name: {
                    #[allow(
                        clippy::disallowed_methods,
                        reason = "a symbol is always available: interning is infallible"
                    )]
                    names.intern("<missing>")
                },
                params: Vec::new(),
                ret: None,
                local_count: 0,
                captures: Vec::new(),
                body: hir::Block::default(),
            })
        })
        .collect();

    let file_order: Vec<FileName> = load_order.iter().map(|p| Arc::from(p.as_str())).collect();
    CheckOutput {
        hir: program,
        diagnostics: diagnostics.finish(&file_order),
        filter_literals,
    }
}

/// Resolves a module's `import` lines against the already-checked modules it
/// names (section 10.2): HS0601/HS0602/etc. were already reported while
/// loading the graph, so a path that failed then simply is not found here
/// (no further diagnostic, no cascade); a name the target module does not
/// export is HS0604.
fn resolve_imports(
    file: &File,
    checked: &CheckedExports,
    program: &hir::Program,
    names: &Interner,
    source: &Source,
    diagnostics: &mut DiagnosticSet,
) -> Vec<resolve::ResolvedImport> {
    let mut out = Vec::new();
    for import in &file.imports {
        let Some(raw_path) = single_text_segment(&import.path) else {
            for name in &import.names {
                out.push(resolve::ResolvedImport {
                    name: *name,
                    value: None,
                    ty: None,
                });
            }
            continue;
        };
        let from_dir = dir_of(source.name());
        let target = match resolve::resolve_import_path(&from_dir, &raw_path) {
            Ok(Ok(segments)) => Some(segments.join("/")),
            _ => None,
        };
        let exports = target.as_ref().and_then(|t| checked.get(t));
        for name in &import.names {
            let Some((_, exports)) = exports else {
                out.push(resolve::ResolvedImport {
                    name: *name,
                    value: None,
                    ty: None,
                });
                continue;
            };
            let mut value = None;
            let mut ty = None;
            for (export_name, export) in exports {
                if *export_name != name.name {
                    continue;
                }
                match *export {
                    hir::Export::Function(id) => value = Some(resolve::ValueBinding::Function(id)),
                    hir::Export::Constant(id) => value = Some(resolve::ValueBinding::Global(id)),
                    hir::Export::Type(decl) => {
                        ty = Some(resolve::TypeBinding::Named(decl));
                        if matches!(
                            program.types.decl_shape(decl),
                            Some(types::DeclShape::Enum { .. })
                        ) {
                            value = Some(resolve::ValueBinding::EnumNamespace(decl));
                        }
                    }
                }
            }
            if value.is_none() && ty.is_none() {
                let text = names.resolve(name.name);
                let path_text = target.clone().unwrap_or_default();
                diagnostics.push(
                    Diagnostic::new(Code::HS0604, &[("path", &path_text), ("name", &text)])
                        .with_location(source.locate(name.span)),
                );
            }
            out.push(resolve::ResolvedImport {
                name: *name,
                value,
                ty,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{CapabilityInterface, check};
    use crate::effects;
    use hollowscript_core::{Code, Control, Interner, LoadModuleError, Source, SourceId};

    fn parse(source: &str) -> (File, Source, Interner) {
        let mut names = Interner::new();
        let src = Source::new(SourceId(0), "main.hws", source).unwrap();
        let mut control = Control::none();
        let parsed = hollowscript_syntax::parse(&src, &mut names, &mut control);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        (parsed.file, src, names)
    }

    use hollowscript_syntax::ast::File;

    #[test]
    fn an_empty_file_checks_with_no_diagnostics() {
        let (file, source, mut names) = parse("");
        let out = check(
            &file,
            &source,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.hir.modules.len(), 1);
    }

    #[test]
    fn a_simple_returning_program_checks_cleanly() {
        let (file, source, mut names) = parse("return 1 + 2\n");
        let out = check(
            &file,
            &source,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    }

    #[test]
    fn an_unresolved_name_is_hs0205() {
        let (file, source, mut names) = parse("return missing\n");
        let out = check(
            &file,
            &source,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.diagnostics[0].code, Code::HS0205);
    }

    /// An overloaded numeric call checks its arguments to choose between its
    /// `Int` and `Float` signatures and then checks them again against the one
    /// it chose, so a call nested in such a call was checked twice at every
    /// level: `abs(abs(...))` 22 levels deep took a second and 40 would not
    /// finish. The answer to the first check is kept, so the work is quadratic
    /// at worst and a program within the nesting limit checks at once.
    #[test]
    fn nested_overloaded_numeric_calls_check_in_reasonable_time() {
        for (call, depth) in [
            ("abs(", 22),
            ("abs(", 120),
            ("max(1, ", 120),
            ("max(1.5, ", 120),
        ] {
            let source = format!("return {}1{}\n", call.repeat(depth), ")".repeat(depth));
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let (file, source, mut names) = parse(&source);
                let out = check(
                    &file,
                    &source,
                    &mut names,
                    &CapabilityInterface::default(),
                    None,
                    &mut |_| Err(LoadModuleError::NotFound),
                );
                let _ = sender.send(out.diagnostics.len());
            });
            let diagnostics = receiver
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("checking nested numeric calls did not finish");
            assert_eq!(diagnostics, 0, "{call} {depth}");
        }
    }

    /// A checker that finds a diagnostic for every one of a quarter of a
    /// million statements holds the first hundred and one of them, not all.
    #[test]
    fn a_flood_of_checker_diagnostics_is_held_to_the_per_file_cap() {
        let (file, source, mut names) = parse(&"missing()\n".repeat(250_000));
        let out = check(
            &file,
            &source,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        // The first hundred, in order, and the notice that there were more.
        assert_eq!(out.diagnostics.len(), 101, "{:?}", out.diagnostics.last());
        assert!(
            out.diagnostics[..100]
                .iter()
                .all(|d| d.code == Code::HS0205)
        );
        assert_eq!(out.diagnostics[100].code, Code::HS0004);
        let lines: Vec<u32> = out.diagnostics[..100]
            .iter()
            .map(|d| d.location.as_ref().map_or(0, |l| l.line))
            .collect();
        assert_eq!(lines, (1..=100).collect::<Vec<u32>>());
    }

    /// Section 2.11: a value of the error type produces no further
    /// diagnostic, and the checker's internal name for that type is never
    /// shown. Each program here has one real mistake whose result is then
    /// used in ways that would each be an error for a known type.
    #[test]
    fn an_error_type_never_reaches_a_message() {
        let programs = [
            "constant r = missing\nmatch r {\n    Ok(t) { return t }\n    Err(e) { return 0 }\n}\nreturn 1\n",
            "constant r = missing\nconstant n: Int = r\nconstant s = r + 1\nreturn [r, n, s]\n",
            "constant r = missing\nmatch r {\n    [a, ..rest] { return a }\n    { size } { return size }\n    none { return 0 }\n    1 { return 1 }\n}\nreturn 2\n",
        ];
        for program in programs {
            let (file, source, mut names) = parse(program);
            let out = check(
                &file,
                &source,
                &mut names,
                &CapabilityInterface::default(),
                None,
                &mut |_| Err(LoadModuleError::NotFound),
            );
            assert!(
                out.diagnostics
                    .iter()
                    .all(|d| !d.message.contains("<error>")),
                "{program}: {:?}",
                out.diagnostics
            );
            assert_eq!(
                out.diagnostics
                    .iter()
                    .filter(|d| d.is_error())
                    .map(|d| d.code)
                    .collect::<Vec<_>>(),
                [Code::HS0205],
                "{program}: only the unresolved name is an error"
            );
        }
    }

    #[test]
    fn a_missing_import_is_hs0601() {
        let (file, source, mut names) = parse("import x from \"./util\"\nreturn x\n");
        let out = check(
            &file,
            &source,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(out.diagnostics.iter().any(|d| d.code == Code::HS0601));
    }

    #[test]
    fn an_import_of_an_exported_constant_works() {
        let (file, source, mut names) = parse("import double from \"./util\"\nreturn double\n");
        let out = check(
            &file,
            &source,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |path| {
                if path == "util.hws" {
                    Ok("export constant double = 4\n".to_owned())
                } else {
                    Err(LoadModuleError::NotFound)
                }
            },
        );
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert_eq!(out.hir.modules.len(), 2);
    }

    #[test]
    fn a_capability_not_granted_is_hs0404() {
        let (file, source, mut names) = parse("uses print\neffect print(\"hi\")\n");
        let out = check(
            &file,
            &source,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(out.diagnostics.iter().any(|d| d.code == Code::HS0404));
    }

    fn check_clean(source: &str) -> super::CheckOutput {
        let (file, src, mut names) = parse(source);
        let out = check(
            &file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        out
    }

    /// Section 2.5's record example: two differently named record types with
    /// the same fields are the same type.
    #[test]
    fn structurally_equal_records_are_the_same_type() {
        check_clean(
            "record Point = { x: Int, y: Int }\n\
             record Vector = { x: Int, y: Int }\n\
             \n\
             fn length2(p: Point): Int {\n\
             \x20   return p.x * p.x + p.y * p.y\n\
             }\n\
             \n\
             constant v: Vector = { x: 3, y: 4 }\n\
             return length2(v)\n",
        );
    }

    /// Section 2.10's generic function example: `first<T>` inferred from a
    /// `[Int]` and a `[String]` argument, and a generic record swap.
    #[test]
    fn generic_functions_and_records_check_cleanly() {
        check_clean(
            "fn first<T>(items: [T]): T? {\n\
             \x20   if items.isEmpty() {\n\
             \x20       return none\n\
             \x20   }\n\
             \x20   return items[0]\n\
             }\n\
             \n\
             record Pair<A, B> = { left: A, right: B }\n\
             \n\
             fn swap<A, B>(p: Pair<A, B>): Pair<B, A> {\n\
             \x20   return { left: p.right, right: p.left }\n\
             }\n\
             \n\
             constant a = first([10, 20])\n\
             constant b = first([\"x\"])\n\
             constant swapped = swap({ left: 1, right: \"one\" })\n\
             return [a == 10, b == \"x\", swapped.left == \"one\"]\n",
        );
    }

    /// Section 4.8's guard-clause narrowing example.
    #[test]
    fn optional_narrowing_through_a_guard_clause_checks_cleanly() {
        check_clean(
            "record Task = { title: String, owner: String? }\n\
             \n\
             fn ownerName(task: Task): String {\n\
             \x20   if task.owner == none {\n\
             \x20       return \"unassigned\"\n\
             \x20   }\n\
             \x20   return task.owner\n\
             }\n\
             \n\
             return ownerName({ title: \"Ship\", owner: \"Ada\" })\n",
        );
    }

    /// Section 7.5's enum match example, exercised for real exhaustiveness
    /// and the alternatives it lists.
    #[test]
    fn a_match_over_an_enum_and_literals_checks_cleanly() {
        check_clean(
            "fn describe(n: Int): String {\n\
             \x20   match n {\n\
             \x20       0 {\n\
             \x20           return \"zero\"\n\
             \x20       }\n\
             \x20       1, -1 {\n\
             \x20           return \"one, either sign\"\n\
             \x20       }\n\
             \x20       else {\n\
             \x20           return \"something else\"\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             \n\
             enum Light {\n\
             \x20   Red,\n\
             \x20   Yellow,\n\
             \x20   Green,\n\
             }\n\
             \n\
             fn nextLight(l: Light): Light {\n\
             \x20   match l {\n\
             \x20       Red { return Light.Green }\n\
             \x20       Yellow { return Light.Red }\n\
             \x20       Green { return Light.Yellow }\n\
             \x20   }\n\
             }\n\
             \n\
             return describe(0) == \"zero\" and nextLight(Light.Red) == Light.Green\n",
        );
    }

    /// A match missing a variant is HS0501 (section 7.5).
    #[test]
    fn a_non_exhaustive_enum_match_is_hs0501() {
        let (file, src, mut names) = parse(
            "enum Light {\n\
             \x20   Red,\n\
             \x20   Green,\n\
             }\n\
             \n\
             fn go(l: Light): Bool {\n\
             \x20   match l {\n\
             \x20       Red {\n\
             \x20           return false\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             \n\
             return go(Light.Green)\n",
        );
        let out = check(
            &file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(out.diagnostics.iter().any(|d| d.code == Code::HS0501));
    }

    /// Section 7.7's list length family: `[]`, `[a]` and `[a, b]` cover
    /// lengths zero, one and two, but nothing covers three or more, so this
    /// is not exhaustive. The exhaustiveness matrix must see the list's own
    /// `ListLength` column before its elements' columns (`exhaustive`'s
    /// pre-order contract); getting that order backwards let this match
    /// look complete because the shorter arms' missing tail columns read as
    /// wildcards instead of as a missing length.
    #[test]
    fn a_list_match_missing_a_longer_length_is_hs0501() {
        let (file, src, mut names) = parse(
            "fn sum(xs: [Int]): Int {\n\
             \x20   match xs {\n\
             \x20       [] {\n\
             \x20           return 0\n\
             \x20       }\n\
             \x20       [a] {\n\
             \x20           return a\n\
             \x20       }\n\
             \x20       [a, b] {\n\
             \x20           return a + b\n\
             \x20       }\n\
             \x20   }\n\
             \x20   return 0\n\
             }\n\
             \n\
             return sum([1, 2, 3])\n",
        );
        let out = check(
            &file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(
            out.diagnostics.iter().any(|d| d.code == Code::HS0501),
            "{:?}",
            out.diagnostics
        );
    }

    /// A `Bool?` scrutinee: `none`, `true` and `false` cover every value.
    /// Before the list column ordering was fixed, an unrelated ordering
    /// defect in the same matrix construction also misjudged some
    /// perfectly exhaustive matches as incomplete; this is one of the
    /// shapes the security review found wrongly rejected.
    #[test]
    fn none_true_and_false_together_are_exhaustive_over_a_nullable_bool() {
        check_clean(
            "fn describe(flag: Bool?): String {\n\
             \x20   match flag {\n\
             \x20       none {\n\
             \x20           return \"unset\"\n\
             \x20       }\n\
             \x20       true {\n\
             \x20           return \"yes\"\n\
             \x20       }\n\
             \x20       false {\n\
             \x20           return \"no\"\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             \n\
             return describe(none)\n",
        );
    }

    /// A `Bool` nested inside a list pattern: `[]` covers length zero, and
    /// `[true, ..rest]` together with `[false, ..rest]` cover every list of
    /// one element or more, split on its first value. This is exhaustive,
    /// but the same ordering defect the previous two tests guard against
    /// made the checker read the nested `Bool` column as if it were the
    /// list's own length column and reject it.
    #[test]
    fn a_bool_split_on_the_first_element_covers_every_list() {
        check_clean(
            "fn any(flags: [Bool]): Bool {\n\
             \x20   match flags {\n\
             \x20       [] {\n\
             \x20           return false\n\
             \x20       }\n\
             \x20       [true, ..rest] {\n\
             \x20           return true\n\
             \x20       }\n\
             \x20       [false, ..rest] {\n\
             \x20           return any(rest)\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             \n\
             return any([])\n",
        );
    }

    /// A partial record pattern, naming only one of the record's two
    /// fields: a record has exactly one constructor, so any pattern for it
    /// is complete on its own regardless of which fields it writes. Another
    /// shape the review found the same ordering defect wrongly rejecting.
    #[test]
    fn a_partial_record_pattern_alone_is_exhaustive() {
        check_clean(
            "record Point = { x: Int, y: Int }\n\
             \n\
             fn describe(pt: Point): Int {\n\
             \x20   match pt {\n\
             \x20       {x: n} {\n\
             \x20           return n\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             \n\
             constant origin: Point = { x: 1, y: 2 }\n\
             return describe(origin)\n",
        );
    }

    /// Closures nested two deep, capturing a top-level function's own
    /// parameter and a top-level constant, section 2.10's map/flatMap example.
    #[test]
    fn nested_closures_capture_across_two_levels_and_check_cleanly() {
        check_clean(
            "constant xs = [1, 2]\n\
             constant ys = [\"a\", \"b\"]\n\
             constant grid = xs.map(fn(x) {\n\
             \x20   return ys.map(fn(y) {\n\
             \x20       return \"${x}${y}\"\n\
             \x20   })\n\
             })\n\
             return grid.flatMap(fn(row) {\n\
             \x20   return row\n\
             }).join(\",\")\n",
        );
    }

    /// A closure that assigns an enclosing `variable`, and a loop using it:
    /// exercises capture-as-upvalue for a mutated local (section 5.4).
    #[test]
    fn a_while_loop_with_break_and_continue_checks_cleanly() {
        check_clean(
            "variable total = 0\n\
             for n in range(0, 10) {\n\
             \x20   if n == 7 {\n\
             \x20       break\n\
             \x20   }\n\
             \x20   if mod(n, 2) == 0 {\n\
             \x20       continue\n\
             \x20   }\n\
             \x20   total = total + n\n\
             }\n\
             return total\n",
        );
    }

    /// Chapter 9's capability example: a granted capability used with the
    /// `effect` marker.
    #[test]
    fn a_granted_capability_used_with_effect_checks_cleanly() {
        let (file, src, mut names) = parse(
            "uses print, clock\n\nconstant now = effect clock.now()\neffect print(\"started at ${now}\")\n",
        );
        let mut table = hollowscript_core::transfer::TypeTable::new();
        let string_ref = table.add(hollowscript_core::transfer::TransferType::String);
        let capabilities = CapabilityInterface {
            capabilities: vec![
                effects::CapabilityDecl {
                    name: "print".to_owned(),
                    shape: effects::CapabilityShape::Function {
                        params: vec![string_ref],
                        ret: None,
                    },
                },
                effects::CapabilityDecl {
                    name: "clock".to_owned(),
                    shape: effects::CapabilityShape::Namespace(vec![(
                        "now".to_owned(),
                        vec![],
                        Some(string_ref),
                    )]),
                },
            ],
            types: table,
        };
        let out = check(&file, &src, &mut names, &capabilities, None, &mut |_| {
            Err(LoadModuleError::NotFound)
        });
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    }

    /// Missing the `effect` marker on a capability call is HS0401.
    #[test]
    fn a_capability_call_without_effect_is_hs0401() {
        let (file, src, mut names) = parse("uses print\nprint(\"hi\")\n");
        let capabilities = CapabilityInterface {
            capabilities: vec![effects::CapabilityDecl {
                name: "print".to_owned(),
                shape: effects::CapabilityShape::Function {
                    params: vec![],
                    ret: None,
                },
            }],
            types: hollowscript_core::transfer::TypeTable::new(),
        };
        let out = check(&file, &src, &mut names, &capabilities, None, &mut |_| {
            Err(LoadModuleError::NotFound)
        });
        assert!(out.diagnostics.iter().any(|d| d.code == Code::HS0401));
    }

    /// Section 7.6 narrows the scrutinee's path "in every arm that does not
    /// have `none` among its patterns". An `else` arm is one of those arms:
    /// it runs only once the `none` arm has failed to match.
    #[test]
    fn the_else_arm_of_a_match_narrows_the_scrutinee() {
        check_clean(concat!(
            "fn f(x: String?): String {\n",
            "    match x {\n",
            "        none { return \"n\" }\n",
            "        else { return x }\n",
            "    }\n",
            "}\n",
            "return 0\n",
        ));
    }

    /// The same, for a field path rather than a bare local (section 4.8's
    /// paths are `x`, `task.owner`, `a.b.c`).
    #[test]
    fn the_else_arm_of_a_match_narrows_a_field_path() {
        check_clean(concat!(
            "record Task = { title: String, owner: String? }\n",
            "fn ownerName(task: Task): String {\n",
            "    match task.owner {\n",
            "        none { return \"unassigned\" }\n",
            "        else { return task.owner }\n",
            "    }\n",
            "}\n",
            "return 0\n",
        ));
    }

    /// Section 2.7: `String` is assignable to `String?`, and section 2.9
    /// makes a record literal's field a position with an expected type. The
    /// literal therefore has its expected type, not one rebuilt from the
    /// fields as written: records are invariant, so rebuilding would reject
    /// the literal against the very type it was checked against.
    #[test]
    fn a_record_literal_may_give_a_plain_value_for_an_optional_field() {
        check_clean(concat!(
            "record Task = { title: String, owner: String? }\n",
            // `_`-prefixed, because section 17.3 would otherwise warn that
            // nothing reads it, which is not what this test is about.
            "constant _t: Task = { title: \"a\", owner: \"b\" }\n",
            "return 0\n",
        ));
    }

    /// The same at every other position that carries an expected type: a
    /// nested record literal, a list element, a `return` operand.
    #[test]
    fn an_optional_field_is_accepted_wherever_an_expected_type_reaches() {
        check_clean(concat!(
            "record Inner = { v: Int? }\n",
            "record Outer = { i: Inner }\n",
            "record Task = { title: String, owner: String? }\n",
            "constant _o: Outer = { i: { v: 1 } }\n",
            "constant _ts: [Task] = [\n",
            "    { title: \"a\", owner: \"b\" },\n",
            "    { title: \"c\", owner: none }\n",
            "]\n",
            "fn make(): Task { return { title: \"d\", owner: \"e\" } }\n",
            "return 0\n",
        ));
    }

    /// A record literal whose field really does not fit still reports, and
    /// reports once: the field's own HS0301, not a second one for the whole
    /// record.
    #[test]
    fn a_record_literal_with_a_bad_field_reports_once() {
        let (file, src, mut names) = parse(concat!(
            "record Task = { title: String, owner: String? }\n",
            "constant _t: Task = { title: \"a\", owner: 3 }\n",
            "return 0\n",
        ));
        let out = check(
            &file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        let errors: Vec<Code> = out.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(errors, [Code::HS0301], "{:?}", out.diagnostics);
    }

    /// No diagnostics at all, for the unused-name tests below.
    const CLEAN: [Code; 0] = [];

    /// The codes one source reports, in order, for the unused-name tests.
    fn codes_of(source: &str) -> Vec<Code> {
        let (file, src, mut names) = parse(source);
        let out = check(
            &file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        out.diagnostics.iter().map(|d| d.code).collect()
    }

    /// Section 17.3's HS0801: "reported for every name that a `constant` or
    /// `variable` declaration binds, local or top-level ... when it is never
    /// read".
    #[test]
    fn a_constant_nothing_reads_warns() {
        assert_eq!(
            codes_of("constant a = 1\nreturn 0\n"),
            [Code::HS0801],
            "a top-level constant"
        );
        assert_eq!(
            codes_of("fn f() {\n    constant a = 1\n}\nreturn 0\n"),
            [Code::HS0801],
            "a local constant"
        );
        assert_eq!(
            codes_of("constant a = 1\nreturn a\n"),
            CLEAN,
            "one that is read"
        );
    }

    /// "Assigning to a `variable` is not a use."
    #[test]
    fn assigning_to_a_variable_is_not_a_use() {
        assert_eq!(
            codes_of("variable a = 1\na = 2\nreturn 0\n"),
            [Code::HS0801]
        );
        assert_eq!(codes_of("variable a = 1\na = 2\nreturn a\n"), CLEAN);
    }

    /// "including inside a closure": a name a closure reads is read, however
    /// far the read is from the declaration.
    #[test]
    fn a_read_inside_a_closure_is_a_use() {
        assert_eq!(
            codes_of(concat!(
                "fn f(): Int {\n",
                "    constant a = 1\n",
                "    constant g = fn(): Int { return a }\n",
                "    return g()\n",
                "}\n",
                "return f()\n",
            )),
            CLEAN
        );
    }

    /// The exemptions section 17.3 lists: a parameter, a `for` variable, a
    /// `match` arm binding, a `_` prefix and an exported declaration.
    #[test]
    fn the_exempt_names_never_warn() {
        assert_eq!(
            codes_of("fn f(a: Int): Int {\n    return 2\n}\nreturn f(1)\n"),
            CLEAN,
            "a parameter"
        );
        assert_eq!(
            codes_of("for i in [1, 2] {\n}\nreturn 0\n"),
            CLEAN,
            "a for variable"
        );
        assert_eq!(
            codes_of("constant _a = 1\nreturn 0\n"),
            CLEAN,
            "an underscore prefix"
        );
        assert_eq!(
            codes_of("export constant a = 1\nreturn 0\n"),
            CLEAN,
            "an exported declaration"
        );
    }

    /// Every name a destructuring pattern binds is its own name, and warns
    /// on its own.
    #[test]
    fn a_pattern_warns_for_each_name_nothing_reads() {
        assert_eq!(
            codes_of(concat!(
                "record Point = { x: Int, y: Int }\n",
                "constant p: Point = { x: 1, y: 2 }\n",
                "constant { x, y } = p\n",
                "return x\n",
            )),
            [Code::HS0801],
            "only `y` goes unread"
        );
    }

    /// The filter literals of a program, as (source, line, column, span),
    /// for a program that may have errors.
    fn literals_of(source: &str) -> Vec<(String, u32, u32, (u32, u32))> {
        let (file, src, mut names) = parse(source);
        let out = check(
            &file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        out.filter_literals
            .iter()
            .map(|l| {
                (
                    l.source.clone(),
                    l.location.line,
                    l.location.column,
                    (l.location.span.start, l.location.span.end),
                )
            })
            .collect()
    }

    /// Every literal is found in source order, with the decoded source and
    /// the span of the whole literal, backticks included, wherever it is: the
    /// top level, a function body that is checked after the top level, a
    /// closure.
    #[test]
    fn the_filter_literals_of_a_program_are_found_in_source_order() {
        let source = concat!(
            "fn later(): Filter {\n",
            "    return `due<today+7`\n",
            "}\n",
            "constant first = `status:open`\n",
            "constant escaped = `title:\"a\\`b\"`\n",
            "constant make = fn(): Filter {\n",
            "    return `starred`\n",
            "}\n",
            "return first\n",
        );
        let found = literals_of(source);
        let sources: Vec<&str> = found.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(
            sources,
            ["due<today+7", "status:open", "title:\"a`b\"", "starred"]
        );
        assert_eq!((found[0].1, found[0].2), (2, 12));
        assert_eq!((found[1].1, found[1].2), (4, 18));
        // The span is of the literal as written, with its escape and its
        // backticks: `title:"a\`b"` is 13 bytes between the backticks plus 2.
        let (start, end) = found[2].3;
        let written = &source[start as usize..end as usize];
        assert_eq!(written, "`title:\"a\\`b\"`");
    }

    /// A literal is found once even where the checker looks at it twice: the
    /// arguments of an overloaded numeric call such as `abs` are checked once
    /// to choose the overload and again against it.
    #[test]
    fn a_filter_literal_is_found_once() {
        let found = literals_of(concat!(
            "fn count(f: Filter): Int {\n",
            "    return 1\n",
            "}\n",
            "return abs(count(`a:b`))\n",
        ));
        assert_eq!(found.len(), 1, "{found:?}");
    }

    /// A literal is found although the program has errors, and although the
    /// literal itself is not valid: a host that evaluates filters can still
    /// read what the program would have asked.
    #[test]
    fn filter_literals_are_found_in_a_program_with_errors() {
        let found = literals_of("constant a = `status:open`\nconstant b = `a:`\nreturn missing\n");
        let sources: Vec<&str> = found.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(sources, ["status:open", "a:"]);
    }

    /// A literal that is not even a token (an unterminated backtick) is the
    /// lexer's problem and is not listed.
    #[test]
    fn an_unterminated_filter_literal_is_not_listed() {
        let mut names = Interner::new();
        let src = Source::new(SourceId(0), "main.hws", "constant a = `status:open\n").unwrap();
        let parsed = hollowscript_syntax::parse(&src, &mut names, &mut Control::none());
        let out = check(
            &parsed.file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |_| Err(LoadModuleError::NotFound),
        );
        assert!(out.filter_literals.is_empty(), "{:?}", out.filter_literals);
    }

    /// The literals of imported modules are listed too, each with its own
    /// file, the imported module first.
    #[test]
    fn filter_literals_of_an_imported_module_name_their_own_file() {
        let (file, src, mut names) =
            parse("import mine from \"./lib\"\nconstant own = `b:2`\nreturn mine\n");
        let out = check(
            &file,
            &src,
            &mut names,
            &CapabilityInterface::default(),
            None,
            &mut |path| {
                if path == "lib.hws" {
                    Ok("export constant mine = `a:1`\n".to_owned())
                } else {
                    Err(LoadModuleError::NotFound)
                }
            },
        );
        let found: Vec<(&str, &str)> = out
            .filter_literals
            .iter()
            .map(|l| (l.location.file.as_ref(), l.source.as_str()))
            .collect();
        assert_eq!(found, [("lib.hws", "a:1"), ("main.hws", "b:2")]);
    }
}
