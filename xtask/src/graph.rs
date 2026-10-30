//! Checks the crate graph against the rules of `docs/design/architecture.md`.
//!
//! The rules keep the browser VM free of the parser and the checker, keep the
//! front end free of the VM, and keep external dependencies out of the core
//! crates. The graph is read from `cargo tree`, one level deep for each crate.

use std::process::Command;

/// Each workspace crate and the workspace crates it may depend on.
const ALLOWED: &[(&str, &[&str])] = &[
    ("hollowscript-core", &[]),
    ("hollowscript-bytecode", &["hollowscript-core"]),
    ("hollowscript-syntax", &["hollowscript-core"]),
    (
        "hollowscript-check",
        &["hollowscript-core", "hollowscript-syntax"],
    ),
    (
        "hollowscript-compiler",
        &[
            "hollowscript-core",
            "hollowscript-check",
            "hollowscript-bytecode",
        ],
    ),
    (
        "hollowscript-vm",
        &["hollowscript-core", "hollowscript-bytecode"],
    ),
    (
        "hollowscript",
        &[
            "hollowscript-core",
            "hollowscript-syntax",
            "hollowscript-check",
            "hollowscript-compiler",
            "hollowscript-bytecode",
            "hollowscript-vm",
        ],
    ),
    (
        "hollowscript-capi",
        &["hollowscript", "hollowscript-core", "hollowscript-bytecode"],
    ),
    (
        "hollowscript-cli",
        &[
            "hollowscript",
            "hollowscript-core",
            "hollowscript-hostfs",
            "hollowscript-syntax",
        ],
    ),
    // How a host opens a file it has only a name for (relative to a
    // directory handle, no symbolic link, regular files only, bounded). It
    // is host-side code shared by the command line host and the language
    // server, and it knows nothing of the language.
    ("hollowscript-hostfs", &[]),
    // Editor services: the analysis a language server and a browser editor
    // share, over the front end and the checker only. It reads no file and
    // speaks no protocol, so it needs nothing else, and above all it must
    // never reach the facade, the compiler or the VM: an editor never runs
    // a program.
    (
        "hollowscript-ide",
        &[
            "hollowscript-core",
            "hollowscript-syntax",
            "hollowscript-check",
        ],
    ),
    (
        "hollowscript-lsp",
        &[
            "hollowscript-core",
            "hollowscript-syntax",
            "hollowscript-check",
            "hollowscript-hostfs",
            "hollowscript-ide",
        ],
    ),
    (
        "hollowscript-wasm",
        &[
            "hollowscript-core",
            "hollowscript-bytecode",
            "hollowscript-vm",
        ],
    ),
    (
        "hollowscript-conformance",
        &["hollowscript", "hollowscript-core"],
    ),
    // Its `bench` subcommand runs a program through the same facade a real
    // embedder calls, exactly as `hollowscript-conformance` runs the corpus
    // through it below, rather than a private fast path into the VM.
    ("xtask", &["hollowscript"]),
];

/// Crates outside the workspace that a crate may use, and which workspace
/// crate may use each one. Every entry must also be recorded in
/// `docs/design/dependencies.md`, with its licence.
///
/// `xtask` is not shipped (`publish = false`; it is a repository helper, not
/// a released crate), so it is held to a looser bar than the crates below:
/// four dependencies, all used by `xtask/src/sign.rs` for the release signing
/// code's encodings (minisign's and the SSH signature format) and the tests
/// that prove them correct without needing Cloud KMS credentials (see that
/// module's own doc comment).
const EXTERNAL: &[(&str, &[&str])] = &[
    ("cap-std", &["hollowscript-cli", "hollowscript-hostfs"]),
    ("libc", &["hollowscript-hostfs"]),
    ("cap-primitives", &["hollowscript-hostfs"]),
    ("ureq", &["hollowscript-cli"]),
    ("base64", &["xtask"]),
    ("ed25519-dalek", &["xtask"]),
    ("rand", &["xtask"]),
    ("sha2", &["xtask"]),
];

/// The crates the language itself is built from: the parser, checker,
/// compiler, bytecode, VM, the embedding facade, the two raw-pointer
/// boundaries (`hollowscript-capi`, `hollowscript-wasm`) and the editor
/// services a browser build links (`hollowscript-ide`). None of these may
/// ever gain a third-party dependency, so that the surface a reviewer must
/// audit for the interpreter's own safety is exactly this workspace's own
/// code (see `docs/design/dependencies.md`, "Today"). This list exists so
/// that rule is checked and named explicitly, in addition to the general
/// rule below (that every non-workspace dependency of any crate must have
/// an `EXTERNAL` entry) already ruling it out structurally, because
/// `EXTERNAL` simply having no entry for one of these crates is easy to
/// satisfy by omission without anyone having decided it on purpose.
const ZERO_DEPENDENCY: &[&str] = &[
    "hollowscript-core",
    "hollowscript-bytecode",
    "hollowscript-syntax",
    "hollowscript-check",
    "hollowscript-compiler",
    "hollowscript-vm",
    "hollowscript",
    "hollowscript-capi",
    "hollowscript-wasm",
    "hollowscript-ide",
];

/// Verifies every crate that exists. Returns the problems found.
pub(crate) fn check() -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    for (name, allowed) in ALLOWED {
        let Some(direct) = direct_dependencies(name)? else {
            continue;
        };
        for dep in direct {
            if let Some((_, _)) = ALLOWED.iter().find(|(n, _)| *n == dep) {
                if !allowed.contains(&dep.as_str()) {
                    problems.push(format!("{name} must not depend on {dep}"));
                }
            } else if ZERO_DEPENDENCY.contains(name) {
                problems.push(format!(
                    "{name} must have no third-party dependency, but depends on {dep}"
                ));
            } else if !EXTERNAL
                .iter()
                .any(|(n, users)| *n == dep && users.contains(name))
            {
                problems.push(format!(
                    "{name} depends on {dep}, which is not an approved external crate"
                ));
            }
        }
    }
    Ok(problems)
}

/// The names of the normal dependencies of `package`, or `None` when the
/// workspace has no such package.
fn direct_dependencies(package: &str) -> Result<Option<Vec<String>>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args([
            "tree",
            "--offline",
            "--edges",
            "normal",
            "--depth",
            "1",
            "--prefix",
            "none",
            "-p",
            package,
        ])
        .output()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("did not match any packages")
            || stderr.contains("package ID specification")
        {
            return Ok(None);
        }
        return Err(format!("cargo tree failed for {package}: {stderr}"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let names = text
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    Ok(Some(names))
}

#[cfg(test)]
mod tests {
    use super::{ALLOWED, EXTERNAL, ZERO_DEPENDENCY};
    use std::path::PathBuf;

    /// `docs/design/architecture.md`'s own diagram and prose never mention
    /// `hollowscript-lsp` alongside `hollowscript-cli` (its own section
    /// says plainly that the CLI depends on the facade and, for `format`,
    /// `hollowscript-syntax`, nothing else, and `hollowscript-lsp`'s section
    /// says it is "independent of the facade"). `ALLOWED` granting that edge
    /// anyway, with nothing in the document to justify it, is exactly the
    /// drift the document's own "checked by machine, not left to review
    /// discipline" claim is supposed to catch; this pins the regression
    /// directly since a general parse of the diagram's ASCII art is its own
    /// project.
    #[test]
    fn hollowscript_cli_is_not_allowed_to_depend_on_the_language_server() {
        let (_, deps) = ALLOWED
            .iter()
            .find(|(name, _)| *name == "hollowscript-cli")
            .expect("hollowscript-cli is a workspace member");
        assert!(
            !deps.contains(&"hollowscript-lsp"),
            "architecture.md documents hollowscript-lsp as independent of the facade; \
             ALLOWED must not grant hollowscript-cli that dependency without a matching \
             update to the document"
        );
    }

    /// Every crate `ALLOWED` names has its own heading in
    /// `docs/design/architecture.md` (a combined heading, such as the one
    /// `hollowscript-conformance` shares with `xtask`, counts): a crate
    /// added to the table with no matching section is exactly the kind of
    /// undocumented entry the document's "machine, not review discipline"
    /// claim should not let through silently.
    #[test]
    fn every_allowed_crate_has_a_section_in_the_architecture_document() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a parent directory")
            .join("docs/design/architecture.md");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for (name, _) in ALLOWED {
            let marker = format!("`{name}`");
            let documented = text
                .lines()
                .any(|line| line.starts_with("### ") && line.contains(&marker));
            assert!(
                documented,
                "{name} is in xtask's ALLOWED table but architecture.md has no `### ...{marker}...` heading for it"
            );
        }
    }

    /// Every crate in `ZERO_DEPENDENCY` is a real workspace crate `check`
    /// actually walks, and no `EXTERNAL` entry grants one of them a
    /// third-party dependency: that would make the deliberate,
    /// error-message-backed rule above silently unreachable for that crate,
    /// which is exactly the kind of drift this file exists to catch by
    /// machine rather than by review discipline.
    #[test]
    fn zero_dependency_crates_are_real_and_granted_no_external_crate() {
        for name in ZERO_DEPENDENCY {
            assert!(
                ALLOWED.iter().any(|(n, _)| n == name),
                "{name} is in ZERO_DEPENDENCY but not in ALLOWED"
            );
            for (external, users) in EXTERNAL {
                assert!(
                    !users.contains(name),
                    "{name} must have no third-party dependency, but EXTERNAL grants it {external}"
                );
            }
        }
    }
}
