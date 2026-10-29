//! One open document, checked with its imports loaded for real: the front
//! end every position-based request in [`crate::server`] runs against.
//!
//! The analysis itself is `hollowscript_ide::Workspace`, which knows nothing
//! about files or URIs. This adapter supplies what it leaves to the caller:
//! the entry module's name and directory (from the document's URI, the same
//! split section 10.3 makes for a command-line entry module), a loader that
//! prefers open documents over disk ([`crate::loader::DocLoader`]), and the
//! mapping from a module's canonical name back to the URI a client knows it
//! by. The analysis is reachable through `Deref`, so a request reads
//! `workspace.text`, `workspace.hir` and the like directly.

use crate::loader::DocLoader;
use hollowscript_check::CapabilityInterface;
use hollowscript_core::Span;
use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::PathBuf;

/// Where a name is really declared: a file's URI and the span of its name in
/// that declaration. Unlike [`hollowscript_ide::DeclId`], whose module name
/// is only meaningful next to the workspace that produced it, this is
/// comparable across workspaces rooted in different directories, which find
/// references and rename rely on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DeclId {
    pub(crate) uri: String,
    pub(crate) span: Span,
}

/// A document, checked against its real imports, with each direct import's
/// own tree kept alongside for source-level lookups.
pub(crate) struct Workspace {
    pub(crate) uri: String,
    /// The URI each direct import has as an open document, by canonical path.
    import_uris: BTreeMap<String, String>,
    /// Where each name this file's `import` lines bring in is really
    /// declared, in URI terms; see
    /// [`hollowscript_ide::Workspace::import_targets`].
    pub(crate) import_targets: BTreeMap<String, DeclId>,
    analysis: hollowscript_ide::Workspace,
}

impl Deref for Workspace {
    type Target = hollowscript_ide::Workspace;

    fn deref(&self) -> &hollowscript_ide::Workspace {
        &self.analysis
    }
}

impl Workspace {
    /// The URI of the module the analysis names `module`: this document
    /// for the entry module, an import's own URI otherwise.
    fn uri_of_module(&self, module: &str) -> Option<&str> {
        if module == self.analysis.entry_name {
            Some(&self.uri)
        } else {
            self.import_uris.get(module).map(String::as_str)
        }
    }

    /// `decl`, in URI terms.
    pub(crate) fn decl_id(&self, decl: &hollowscript_ide::DeclId) -> DeclId {
        DeclId {
            uri: self
                .uri_of_module(&decl.module)
                .unwrap_or(&self.uri)
                .to_owned(),
            span: decl.span,
        }
    }

    /// The diagnostics of the whole program, consuming the workspace.
    pub(crate) fn into_diagnostics(self) -> Vec<hollowscript_core::Diagnostic> {
        self.analysis.diagnostics
    }

    /// The text of the module at `uri`: this document, or one of its direct
    /// imports.
    pub(crate) fn text_at(&self, uri: &str) -> Option<&str> {
        if uri == self.uri {
            return Some(&self.analysis.text);
        }
        self.import_at(uri).map(|m| m.text.as_str())
    }

    /// The imported file at `uri`, if this document imports it directly.
    pub(crate) fn import_at(&self, uri: &str) -> Option<&hollowscript_ide::ImportedFile> {
        self.analysis
            .imports
            .iter()
            .find(|m| self.import_uris.get(&m.canonical_path).map(String::as_str) == Some(uri))
    }
}

/// Builds a [`Workspace`] for the document at `uri` with contents `text`,
/// resolving its imports against `documents` (open documents first, then
/// disk, via [`DocLoader`]) the same way a command-line run rooted at this
/// document would.
///
/// The checker is given the default, empty capability interface: this server
/// knows nothing of the host a program will run in.
pub(crate) fn build(
    uri: &str,
    text: &str,
    documents: &BTreeMap<String, String>,
) -> Option<Workspace> {
    let (root, entry_name) = crate::paths::root_and_entry(uri)
        .unwrap_or_else(|| (PathBuf::from("."), "document".to_owned()));
    let mut loader = DocLoader::new(root, documents);
    let analysis = hollowscript_ide::Workspace::build(
        &entry_name,
        text,
        &CapabilityInterface::default(),
        &mut |path| loader.load(path),
    )?;
    let import_uris: BTreeMap<String, String> = analysis
        .imports
        .iter()
        .filter_map(|m| {
            loader
                .uri_of(&m.canonical_path)
                .map(|u| (m.canonical_path.clone(), u))
        })
        .collect();
    let mut workspace = Workspace {
        uri: uri.to_owned(),
        import_uris,
        import_targets: BTreeMap::new(),
        analysis,
    };
    workspace.import_targets = workspace
        .analysis
        .import_targets
        .iter()
        .map(|(name, target)| (name.clone(), workspace.decl_id(&target.decl)))
        .collect();
    Some(workspace)
}

#[cfg(test)]
mod tests {
    use super::build;
    use std::collections::BTreeMap;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-lsp-workspace-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_import_is_loaded_from_disk_and_parsed() {
        let dir = temp_dir("basic");
        std::fs::write(dir.join("util.hws"), "export constant two = 2\n").unwrap();
        let uri = crate::paths::from_path(&dir.join("main.hws"));
        let text = "import two from \"./util\"\nreturn two\n";
        let documents = BTreeMap::new();
        let workspace = build(&uri, text, &documents).unwrap();
        assert!(
            workspace.diagnostics.is_empty(),
            "{:?}",
            workspace.diagnostics
        );
        assert_eq!(workspace.imports.len(), 1);
        assert_eq!(workspace.imports[0].canonical_path, "util.hws");
        let util_uri = crate::paths::from_path(&dir.join("util.hws"));
        assert!(workspace.import_at(&util_uri).is_some());
        assert_eq!(workspace.import_targets["two"].uri, util_uri);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_open_document_overrides_disk_for_an_import() {
        let dir = temp_dir("override");
        std::fs::write(dir.join("util.hws"), "export constant two = 2\n").unwrap();
        let util_uri = crate::paths::from_path(&dir.join("util.hws"));
        let mut documents = BTreeMap::new();
        documents.insert(util_uri, "export constant two = 5\n".to_owned());
        let main_uri = crate::paths::from_path(&dir.join("main.hws"));
        let text = "import two from \"./util\"\nreturn two\n";
        let workspace = build(&main_uri, text, &documents).unwrap();
        assert!(
            workspace.diagnostics.is_empty(),
            "{:?}",
            workspace.diagnostics
        );
        assert_eq!(workspace.imports[0].text, "export constant two = 5\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_import_is_a_real_diagnostic_not_a_panic() {
        let dir = temp_dir("missing");
        let uri = crate::paths::from_path(&dir.join("main.hws"));
        let text = "import two from \"./nope\"\nreturn 1\n";
        let documents = BTreeMap::new();
        let workspace = build(&uri, text, &documents).unwrap();
        assert!(
            workspace
                .diagnostics
                .iter()
                .any(|d| d.code.to_text() == "HS0601"),
            "{:?}",
            workspace.diagnostics
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_declaration_in_this_document_has_this_documents_uri() {
        let dir = temp_dir("own");
        let uri = crate::paths::from_path(&dir.join("main.hws"));
        let workspace =
            build(&uri, "constant total = 1\nreturn total\n", &BTreeMap::new()).unwrap();
        let binders = workspace.binders();
        let total = binders.iter().find(|b| b.name == "total").unwrap();
        let decl = workspace.decl_id(&total.decl);
        assert_eq!(decl.uri, uri);
        assert_eq!(decl.span, total.at);
        assert_eq!(workspace.text_at(&uri), Some(workspace.text.as_str()));
        assert_eq!(workspace.text_at("file:///elsewhere.hws"), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
