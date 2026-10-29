//! Running every discovered case and deciding whether the run passes.
//!
//! The corpus is a specification artefact, not a record of what this
//! implementation does today, so most of it fails while a part of the
//! language is unimplemented. A summary alone would be worthless as a gate:
//! it would go green whether the number of failures went up or down. So the
//! gate is a *baseline*: `known-failures.txt` lists, by case id, every case
//! this implementation is known not to pass. The run fails when
//!
//! - a case outside the baseline fails or cannot be read, which is a
//!   regression; or
//! - a case inside the baseline passes, which means the baseline is stale
//!   and has to be shortened.
//!
//! Both directions matter. A baseline that only ever grows would hide
//! regressions just as well as no gate at all.

use crate::case::{self, Outcome};
use crate::discover::Case;
use std::collections::BTreeSet;

/// The result of running every case.
pub(crate) struct Report {
    /// One entry per case, in the order given.
    pub results: Vec<(String, Outcome)>,
    /// The baseline as it was read.
    pub baseline: BTreeSet<String>,
}

impl Report {
    /// The cases that passed.
    pub(crate) fn passed(&self) -> Vec<&str> {
        self.with(|o| matches!(o, Outcome::Passed))
    }

    /// The cases that failed a comparison.
    pub(crate) fn failed(&self) -> Vec<&str> {
        self.with(|o| matches!(o, Outcome::Failed(_)))
    }

    /// The cases that could not be read at all.
    pub(crate) fn broken(&self) -> Vec<&str> {
        self.with(|o| matches!(o, Outcome::Broken(_)))
    }

    fn with(&self, f: impl Fn(&Outcome) -> bool) -> Vec<&str> {
        self.results
            .iter()
            .filter(|(_, o)| f(o))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Cases that failed although the baseline does not list them.
    pub(crate) fn regressions(&self) -> Vec<&str> {
        self.results
            .iter()
            .filter(|(id, o)| !matches!(o, Outcome::Passed) && !self.baseline.contains(id.as_str()))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Cases the baseline lists although they now pass.
    pub(crate) fn newly_passing(&self) -> Vec<&str> {
        self.results
            .iter()
            .filter(|(id, o)| matches!(o, Outcome::Passed) && self.baseline.contains(id.as_str()))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// The problems reported for one case.
    pub(crate) fn problems_of(&self, id: &str) -> Vec<String> {
        self.results
            .iter()
            .find(|(case_id, _)| case_id == id)
            .map_or_else(Vec::new, |(_, outcome)| match outcome {
                Outcome::Failed(problems) => problems.clone(),
                Outcome::Broken(message) => vec![message.clone()],
                Outcome::Passed => Vec::new(),
            })
    }
}

/// Runs every case against `baseline`.
#[must_use]
pub(crate) fn run(cases: &[Case], baseline: BTreeSet<String>) -> Report {
    let results = cases
        .iter()
        .map(|case| (case.id.clone(), case::run(case)))
        .collect();
    Report { results, baseline }
}

/// Reads a baseline file: one case id per line, `#` starting a comment, and
/// blank lines ignored.
#[must_use]
pub(crate) fn read_baseline(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Report, read_baseline};
    use crate::case::Outcome;
    use std::collections::BTreeSet;

    fn report(entries: Vec<(&str, Outcome)>, baseline: &[&str]) -> Report {
        Report {
            results: entries
                .into_iter()
                .map(|(id, o)| (id.to_owned(), o))
                .collect(),
            baseline: baseline.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn a_baseline_ignores_comments_and_blank_lines() {
        let baseline = read_baseline("# a comment\n\narea/one\narea/two # why\n");
        assert_eq!(
            baseline,
            ["area/one", "area/two"]
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn a_failure_outside_the_baseline_is_a_regression() {
        let r = report(
            vec![
                ("area/one", Outcome::Failed(vec!["no".to_owned()])),
                ("area/two", Outcome::Failed(vec!["no".to_owned()])),
            ],
            &["area/two"],
        );
        assert_eq!(r.regressions(), ["area/one"]);
        assert!(r.newly_passing().is_empty());
        assert_eq!(r.failed().len(), 2);
    }

    #[test]
    fn a_pass_inside_the_baseline_has_to_shorten_it() {
        let r = report(vec![("area/one", Outcome::Passed)], &["area/one"]);
        assert!(r.regressions().is_empty());
        assert_eq!(r.newly_passing(), ["area/one"]);
    }

    #[test]
    fn a_case_that_cannot_be_read_counts_as_a_regression() {
        let r = report(vec![("area/one", Outcome::Broken("gone".to_owned()))], &[]);
        assert_eq!(r.regressions(), ["area/one"]);
        assert_eq!(r.broken(), ["area/one"]);
        assert_eq!(r.problems_of("area/one"), ["gone"]);
    }
}
