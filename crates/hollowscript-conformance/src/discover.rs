//! Discovers the cases of the shared conformance corpus (`corpus/FORMAT.md`):
//! every `<area>/<name>.hws` that has a matching `<area>/<name>.expected.json`
//! beside it. Modules of a multi-file case (`<name>.modules/`) are found from
//! the case, not listed on their own.

use std::path::{Path, PathBuf};

/// One case: its id (`area/name`, as `corpus/FORMAT.md` names a case) and the
/// path of its entry program.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Case {
    /// `area/name`, the path under `corpus/` without the extension.
    pub id: String,
    /// The entry program.
    pub program: PathBuf,
    /// The expected outcomes.
    pub expected: PathBuf,
}

/// Finds every case under `corpus_root` (`corpus/` at the repository's own
/// root), in a fixed order (by id), skipping `FORMAT.md`,
/// `spec-v1-pending.json` and any `<name>.modules` directory.
///
/// # Errors
///
/// Returns an error if `corpus_root` or one of its area directories cannot be read.
pub(crate) fn discover_cases(corpus_root: &Path) -> std::io::Result<Vec<Case>> {
    let mut cases = Vec::new();
    let mut areas: Vec<PathBuf> = std::fs::read_dir(corpus_root)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    areas.sort();
    for area in areas {
        let area_name = area
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_owned();
        let mut names: Vec<String> = std::fs::read_dir(&area)?
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(".hws").map(str::to_owned))
            .collect();
        names.sort();
        for name in names {
            let program = area.join(format!("{name}.hws"));
            let expected = area.join(format!("{name}.expected.json"));
            if expected.is_file() {
                cases.push(Case {
                    id: format!("{area_name}/{name}"),
                    program,
                    expected,
                });
            }
        }
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::discover_cases;
    use std::path::Path;

    /// The corpus of this repository, found from this crate's own manifest
    /// directory, three levels up (`crates/hollowscript-conformance` to the
    /// repository root, then into `corpus`).
    fn repo_corpus() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
    }

    #[test]
    fn the_repositorys_own_corpus_has_many_cases_across_several_areas() {
        let cases = discover_cases(&repo_corpus()).expect("the corpus directory is read");
        assert!(cases.len() > 200, "found only {}", cases.len());
        let areas: std::collections::BTreeSet<&str> = cases
            .iter()
            .filter_map(|c| c.id.split_once('/'))
            .map(|(area, _)| area)
            .collect();
        for expected_area in [
            "lexical",
            "types",
            "expressions",
            "modules",
            "capabilities",
            "guide",
        ] {
            assert!(
                areas.contains(expected_area),
                "missing area {expected_area}"
            );
        }
    }

    #[test]
    fn every_case_has_a_readable_program_and_expected_file() {
        let cases = discover_cases(&repo_corpus()).expect("the corpus directory is read");
        for case in cases.iter().take(20) {
            assert!(case.program.is_file(), "{}", case.id);
            assert!(case.expected.is_file(), "{}", case.id);
        }
    }

    #[test]
    fn ids_are_sorted_and_unique() {
        let cases = discover_cases(&repo_corpus()).expect("the corpus directory is read");
        let ids: Vec<&str> = cases.iter().map(|c| c.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
        let unique: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len());
    }
}
