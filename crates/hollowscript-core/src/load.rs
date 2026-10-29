//! Why a host's module loader could not produce an imported module's source
//! (section 10.3), shared by every interface that carries a loader's outcome.
//!
//! `hollowscript_check::check`'s `load_module` callback and the facade's
//! `hollowscript::Loader` trait both need to say, for one import, whether the
//! module simply is not there (HS0601) or exists but could not be read
//! (HS0607, which needs a `{reason}` to report). Defining the distinction
//! once here, rather than once per crate, is what lets a `Failed` reason
//! travel from wherever a loader is implemented all the way to the
//! diagnostic that names it, instead of being discarded by a narrower
//! interface partway through.

use crate::codes::Code;
use crate::diagnostic::Diagnostic;

/// Why a loader could not produce the source of an imported module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadModuleError {
    /// No module exists at that canonical path (HS0601).
    NotFound,
    /// The module exists but could not be read (HS0607); this text fills the
    /// diagnostic's `{reason}` placeholder.
    Failed(String),
}

impl LoadModuleError {
    /// The diagnostic that reports this for the import path `path`. The
    /// caller, which knows the span of the `import` that triggered the
    /// load, is expected to attach the location with
    /// [`Diagnostic::with_location`].
    #[must_use]
    pub fn to_diagnostic(&self, path: &str) -> Diagnostic {
        match self {
            LoadModuleError::NotFound => Diagnostic::new(Code::HS0601, &[("path", &path)]),
            LoadModuleError::Failed(reason) => {
                Diagnostic::new(Code::HS0607, &[("path", &path), ("reason", reason)])
            }
        }
    }
}

impl std::fmt::Display for LoadModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadModuleError::NotFound => f.write_str("the module was not found"),
            LoadModuleError::Failed(reason) => {
                write!(f, "the module could not be read: {reason}")
            }
        }
    }
}

impl std::error::Error for LoadModuleError {}

#[cfg(test)]
mod tests {
    use super::LoadModuleError;
    use crate::codes::Code;

    #[test]
    fn not_found_is_hs0601_with_the_path() {
        let d = LoadModuleError::NotFound.to_diagnostic("./util");
        assert_eq!(d.code, Code::HS0601);
        assert!(d.message.contains("./util"));
    }

    #[test]
    fn failed_is_hs0607_with_the_path_and_reason() {
        let d = LoadModuleError::Failed("permission denied".into()).to_diagnostic("./util");
        assert_eq!(d.code, Code::HS0607);
        assert!(d.message.contains("./util"));
        assert!(d.message.contains("permission denied"));
    }
}
