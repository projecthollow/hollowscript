//! Walks a tree and applies the rules of [`crate::rules`] to every text file.

use crate::rules::{Rule, rules};
use std::fs;
use std::path::Path;

/// One place where a rule matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    /// The path relative to the scanned root, with `/` as the separator.
    pub(crate) path: String,
    /// The 1-based line.
    pub(crate) line: usize,
    /// The 1-based column, counted in characters.
    pub(crate) column: usize,
    /// The id of the rule.
    pub(crate) rule: &'static str,
    /// What the rule looks for.
    pub(crate) what: &'static str,
}

/// The outcome of a scan.
#[derive(Debug, Default)]
pub(crate) struct Report {
    pub(crate) findings: Vec<Finding>,
    /// Text files that were scanned.
    pub(crate) files: usize,
    /// Files with a NUL byte, which are not scanned.
    pub(crate) binary: usize,
}

/// Applies `rules` to every line of `text`.
pub(crate) fn scan_text(label: &str, text: &str, rules: &[Rule]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        for rule in rules {
            for hit in rule.find(line) {
                let column = line
                    .get(..hit.start)
                    .map_or(hit.start, |s| s.chars().count())
                    + 1;
                findings.push(Finding {
                    path: label.to_owned(),
                    line: index + 1,
                    column,
                    rule: rule.id,
                    what: rule.what,
                });
            }
        }
    }
    findings
}

/// Scans the tree under `root`, skipping the version control directory, build
/// output and the top-level `internal` directory, which is not mirrored.
pub(crate) fn scan_tree(root: &Path) -> Result<Report, String> {
    let rules = rules();
    let mut report = Report::default();
    walk(root, root, &rules, &mut report)?;
    report.findings.retain(|f| !is_tool_marker(f));
    report.findings.sort_by(|a, b| {
        (&a.path, a.line, a.column, a.rule).cmp(&(&b.path, b.line, b.column, b.rule))
    });
    Ok(report)
}

fn walk(root: &Path, dir: &Path, rules: &[Rule], report: &mut Report) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if kind.is_symlink() {
            continue;
        }
        if name == ".git" {
            continue;
        }
        if kind.is_dir() {
            let top_level_internal = dir == root && name == "internal";
            if top_level_internal || matches!(&*name, "target" | "node_modules") {
                continue;
            }
            walk(root, &path, rules, report)?;
        } else if kind.is_file() {
            if is_standard_text(&name) {
                continue;
            }
            let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if bytes.contains(&0) {
                report.binary += 1;
                continue;
            }
            let label = path.strip_prefix(root).map_or_else(
                |_| path.display().to_string(),
                |p| p.to_string_lossy().replace('\\', "/"),
            );
            report.files += 1;
            let text = String::from_utf8_lossy(&bytes);
            report.findings.extend(scan_text(&label, &text, rules));
        }
    }
    Ok(())
}

/// Licence texts are used verbatim and are not scanned.
fn is_standard_text(name: &str) -> bool {
    name.starts_with("LICENSE") || name.starts_with("COPYING")
}

/// Whether a finding is inside a file a package manager generates and owns
/// entirely, where the "authorship" rule's heuristics (a capitalised word
/// after "by", a run of initials) have nothing to do with a person:
/// `Cargo.lock`'s own header comment on line 1 (checked by the file's own
/// name, not only the workspace root's, since `fuzz/` is a separate crate
/// outside the workspace with its own `fuzz/Cargo.lock`, and any other
/// nested `Cargo.lock` would need the same exemption), and anywhere in a
/// `package-lock.json`, whose `integrity` hashes are base64 and
/// coincidentally match those same heuristics often enough that the rule
/// is not useful there at all.
fn is_tool_marker(finding: &Finding) -> bool {
    let name = finding.path.rsplit('/').next().unwrap_or(&finding.path);
    (name == "Cargo.lock" && finding.line == 1 && finding.rule == "authorship")
        || (finding.rule == "authorship" && finding.path.ends_with("package-lock.json"))
}

#[cfg(test)]
mod tests {
    use super::{Finding, is_tool_marker, scan_text, scan_tree};
    use crate::rules::rules;
    use std::fs;
    use std::path::PathBuf;

    fn cargo_lock_marker(path: &str) -> Finding {
        Finding {
            path: path.to_owned(),
            line: 1,
            column: 1,
            rule: "authorship",
            what: "",
        }
    }

    #[test]
    fn a_nested_cargo_lock_is_exempt_the_same_way_the_root_one_is() {
        assert!(is_tool_marker(&cargo_lock_marker("Cargo.lock")));
        assert!(is_tool_marker(&cargo_lock_marker("fuzz/Cargo.lock")));
        assert!(!is_tool_marker(&cargo_lock_marker("fuzz/Cargo.toml")));
    }

    #[test]
    fn only_line_one_and_the_authorship_rule_are_exempt() {
        let mut finding = cargo_lock_marker("fuzz/Cargo.lock");
        finding.line = 2;
        assert!(!is_tool_marker(&finding));

        let mut finding = cargo_lock_marker("fuzz/Cargo.lock");
        finding.rule = "local-path";
        assert!(!is_tool_marker(&finding));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hollowscript-xtask-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch directory");
        dir
    }

    #[test]
    fn reports_line_and_column_in_characters() {
        let text = "ok\n\u{e9}\u{e9} a \u{2014} b\r\nok\n";
        let findings = scan_text("f.txt", text, &rules());
        assert_eq!(findings.len(), 1);
        assert_eq!((findings[0].line, findings[0].column), (2, 6));
        assert_eq!(findings[0].rule, "dash");
    }

    #[test]
    fn skips_the_internal_directory_only_at_the_top() {
        let root = scratch("skip");
        let dash = "\u{2013}";
        fs::create_dir_all(root.join("internal")).expect("create");
        fs::create_dir_all(root.join("docs/internal")).expect("create");
        fs::create_dir_all(root.join("target")).expect("create");
        fs::write(root.join("internal/a.txt"), dash).expect("write");
        fs::write(root.join("target/b.txt"), dash).expect("write");
        fs::write(root.join("docs/internal/c.txt"), dash).expect("write");
        fs::write(root.join("d.bin"), [0u8, 1, 2]).expect("write");
        let report = scan_tree(&root).expect("scan");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].path, "docs/internal/c.txt");
        assert_eq!(report.binary, 1);
        assert_eq!(report.files, 1);
        fs::remove_dir_all(&root).expect("clean up");
    }

    /// `package-lock.json` legitimately names real npm packages whose own
    /// name happens to contain one of the "authorship" rule's whole-word
    /// triggers (a hyphen is a word boundary, so the trigger word inside a
    /// hyphenated package name stands alone): a real word in a real
    /// package name, not a person or a tool this rule is meant to flag.
    /// `is_tool_marker` drops every "authorship" finding in this one
    /// generated file rather than try to tell those apart line by line.
    #[test]
    fn a_package_name_containing_a_trigger_word_in_package_lock_json_is_not_flagged() {
        let root = scratch("package-lock");
        // Built from parts, not spelled out, so this test's own source does
        // not itself trip the rule it exercises (`rules.rs` does the same
        // for its own trigger words).
        let word: String = ["ag", "ent"].concat();
        let line = format!("\"node_modules/http-proxy-{word}\": {{ \"version\": \"7.0.2\" }}\n");
        fs::write(root.join("package-lock.json"), line).expect("write");
        let report = scan_tree(&root).expect("scan");
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        fs::remove_dir_all(&root).expect("clean up");
    }
}
