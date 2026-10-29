//! Third-party notices for a release artefact
//! (`cargo xtask third-party-licenses`), and the crate set a release's SBOM
//! is written from (`cargo xtask sbom`, in `sbom.rs`).
//!
//! Both start from the same question: which crates are actually linked into
//! one shipped binary or library, for one target? That is answered with
//! `cargo tree -e normal --target <target> -p <package>`, once per package,
//! which applies the same feature resolution and platform filtering a
//! `cargo build -p <package> --target <target>` does, and follows normal
//! dependency edges only: a build dependency, a dev dependency and a
//! procedural macro (with everything below it) all run at compile time and
//! are never linked into what ships, so none of them is listed. `xtask`'s
//! own dependencies are therefore never in a release's notices either,
//! since no shipped package depends on `xtask`.
//!
//! For each third-party crate found, the notices file carries the licence
//! texts and copyright files the crate itself ships (every top-level
//! `LICENSE*`, `LICENCE*`, `COPYING*`, `COPYRIGHT*`, `NOTICE*` and
//! `UNLICENSE*` file, plus a `LICENSES/` directory and a `license-file`),
//! read from the crate's own unpacked source under Cargo's registry, verbatim,
//! and its declared authors. A crate that ships no such file is an error,
//! not a line that says "see upstream": a release must carry the text it
//! is distributed under. The Rust standard library, linked into every
//! artefact, is covered by the toolchain's own `COPYRIGHT-library.html`,
//! converted to plain text; C runtime components a target links statically
//! (musl, mingw-w64) are added by the caller with `--notice`, since only the
//! caller knows which C toolchain did the linking.

use crate::json::{self, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One crate linked into a release artefact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Crate {
    /// The package name.
    pub(crate) name: String,
    /// The exact version from `Cargo.lock`.
    pub(crate) version: String,
    /// The `license` field, or `(no licence field)`.
    pub(crate) license: String,
    /// The `authors` field, possibly empty.
    pub(crate) authors: Vec<String>,
    /// The `repository` field, if any.
    pub(crate) repository: Option<String>,
    /// The directory holding the crate's own `Cargo.toml`.
    pub(crate) dir: PathBuf,
    /// The `license-file` field, relative to `dir`, if any.
    pub(crate) license_file: Option<String>,
    /// Whether this is a member of this workspace rather than third-party.
    pub(crate) workspace_member: bool,
}

/// The packages the CLI archive ships binaries from.
pub(crate) const CLI_PACKAGES: [&str; 2] = ["hollowscript-cli", "hollowscript-lsp"];

/// `cargo xtask third-party-licenses [--target T] [--package P]... --out FILE
/// [--artifact NAME] [--notice TITLE=PATH]...`
pub(crate) fn run(workspace_root: &Path, args: &[String]) -> Result<bool, String> {
    let mut target: Option<String> = None;
    let mut packages: Vec<String> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut artifact = "hollowscript".to_owned();
    let mut notices: Vec<(String, PathBuf)> = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut next = || iter.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--target" => target = Some(next()?),
            "--package" => packages.push(next()?),
            "--out" => out = Some(PathBuf::from(next()?)),
            "--artifact" => artifact = next()?,
            "--notice" => {
                let value = next()?;
                let (title, path) = value
                    .split_once('=')
                    .ok_or(format!("--notice expects TITLE=PATH, got '{value}'"))?;
                notices.push((title.to_owned(), PathBuf::from(path)));
            }
            other => {
                return Err(format!(
                    "unknown option '{other}'; usage: cargo xtask third-party-licenses \
                     [--target T] [--package P]... --out FILE [--artifact NAME] \
                     [--notice TITLE=PATH]..."
                ));
            }
        }
    }
    let out = out.ok_or("--out FILE is required")?;
    let target = match target {
        Some(t) => t,
        None => host_target(workspace_root)?,
    };
    if packages.is_empty() {
        packages = CLI_PACKAGES.iter().map(|p| (*p).to_owned()).collect();
    }
    let crates = linked_crates(workspace_root, &target, &packages)?;
    let rust_std = rust_std_notice(workspace_root)?;
    let mut extra = Vec::new();
    for (title, path) in notices {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read notice '{title}' from {}: {e}", path.display()))?;
        extra.push((title, text));
    }
    let text = render(&artifact, &target, &packages, &crates, &rust_std, &extra)?;
    std::fs::write(&out, &text).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    let third_party = crates.iter().filter(|c| !c.workspace_member).count();
    println!(
        "wrote {} ({third_party} third-party crate(s) linked into {} for {target})",
        out.display(),
        packages.join(", ")
    );
    Ok(true)
}

/// The host target triple, from `rustc -vV`.
pub(crate) fn host_target(workspace_root: &Path) -> Result<String, String> {
    let text = run_tool(workspace_root, "rustc", &["-vV"])?;
    text.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or("rustc -vV printed no host line".to_owned())
}

/// Every crate, workspace members included, linked into any of `packages`
/// built for `target`, sorted by name and version, each listed once.
pub(crate) fn linked_crates(
    workspace_root: &Path,
    target: &str,
    packages: &[String],
) -> Result<Vec<Crate>, String> {
    let metadata = metadata(workspace_root)?;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for package in packages {
        let tree = run_tool(
            workspace_root,
            &cargo(),
            &[
                "tree", "--locked", "-e", "normal", "--target", target, "-p", package, "--prefix",
                "depth", "--format", "{p}",
            ],
        )?;
        for (name, version) in parse_tree(&tree)? {
            seen.insert((name, version));
        }
    }
    let mut result = Vec::new();
    for key in seen {
        let found = metadata
            .get(&key)
            .ok_or(format!(
                "cargo tree names {} {} but cargo metadata does not know it",
                key.0, key.1
            ))?
            .clone();
        result.push(found);
    }
    Ok(result)
}

/// Parses `cargo tree --prefix depth --format {p}` output into (name,
/// version) pairs, skipping every procedural macro and everything below
/// one, since a procedural macro runs inside the compiler and is never
/// linked into the artefact.
fn parse_tree(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut result = Vec::new();
    let mut skip_below: Option<usize> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let digits = line.bytes().take_while(u8::is_ascii_digit).count();
        let (depth_text, rest) = line.split_at(digits);
        let depth: usize = depth_text
            .parse()
            .map_err(|_| format!("cargo tree line has no depth prefix: '{line}'"))?;
        if let Some(limit) = skip_below {
            if depth > limit {
                continue;
            }
            skip_below = None;
        }
        let rest = rest.trim_end_matches(" (*)");
        let mut words = rest.split(' ');
        let name = words.next().unwrap_or_default();
        let version = words
            .next()
            .and_then(|v| v.strip_prefix('v'))
            .ok_or(format!("cargo tree line has no version: '{line}'"))?;
        if rest.contains("(proc-macro)") {
            skip_below = Some(depth);
            continue;
        }
        result.push((name.to_owned(), version.to_owned()));
    }
    Ok(result)
}

/// Every package `cargo metadata` knows, keyed by (name, version).
fn metadata(workspace_root: &Path) -> Result<BTreeMap<(String, String), Crate>, String> {
    let text = run_tool(
        workspace_root,
        &cargo(),
        &["metadata", "--format-version", "1", "--locked"],
    )?;
    let root = json::parse(&text)?;
    let packages = root
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata's output has no \"packages\" array")?;
    let mut map = BTreeMap::new();
    for package in packages {
        let text_field = |key: &str| package.get(key).and_then(Value::as_str);
        let name = text_field("name").ok_or("a package has no \"name\"")?;
        let version = text_field("version").ok_or("a package has no \"version\"")?;
        let manifest = text_field("manifest_path").ok_or("a package has no \"manifest_path\"")?;
        let dir = Path::new(manifest)
            .parent()
            .map(Path::to_path_buf)
            .ok_or(format!("{name}'s manifest path has no directory"))?;
        let authors = package
            .get("authors")
            .and_then(Value::as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        map.insert(
            (name.to_owned(), version.to_owned()),
            Crate {
                name: name.to_owned(),
                version: version.to_owned(),
                license: text_field("license")
                    .unwrap_or("(no licence field)")
                    .to_owned(),
                authors,
                repository: text_field("repository").map(str::to_owned),
                dir,
                license_file: text_field("license_file").map(str::to_owned),
                workspace_member: text_field("source").is_none(),
            },
        );
    }
    Ok(map)
}

/// Whether a top-level file name is one a crate uses for its licence or
/// copyright text.
fn is_licence_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "COPYRIGHT",
        "NOTICE",
        "UNLICENSE",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

/// The licence and copyright files `krate` ships, as (relative path, text),
/// sorted by path. An error if it ships none.
fn licence_files(krate: &Crate) -> Result<Vec<(String, String)>, String> {
    let mut paths: BTreeSet<String> = BTreeSet::new();
    let entries = std::fs::read_dir(&krate.dir)
        .map_err(|e| format!("cannot list {}: {e}", krate.dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list {}: {e}", krate.dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot stat {name}: {e}"))?;
        if kind.is_file() && is_licence_name(&name) {
            paths.insert(name);
        } else if kind.is_dir() && name.eq_ignore_ascii_case("LICENSES") {
            let inner =
                std::fs::read_dir(entry.path()).map_err(|e| format!("cannot list {name}: {e}"))?;
            for file in inner {
                let file = file.map_err(|e| format!("cannot list {name}: {e}"))?;
                if file.file_type().is_ok_and(|t| t.is_file()) {
                    paths.insert(format!("{name}/{}", file.file_name().to_string_lossy()));
                }
            }
        }
    }
    if let Some(file) = &krate.license_file {
        paths.insert(file.clone());
    }
    if paths.is_empty() {
        return Err(format!(
            "{} {} ships no licence or copyright file in {}; a release must carry the \
             text it is distributed under, so this needs resolving before it can ship",
            krate.name,
            krate.version,
            krate.dir.display()
        ));
    }
    let mut result = Vec::new();
    for path in paths {
        let full = krate.dir.join(&path);
        let bytes =
            std::fs::read(&full).map_err(|e| format!("cannot read {}: {e}", full.display()))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{} is not UTF-8 text", full.display()))?;
        result.push((path, text));
    }
    Ok(result)
}

/// The Rust standard library's own copyright notices, from the active
/// toolchain, as plain text.
fn rust_std_notice(workspace_root: &Path) -> Result<String, String> {
    let sysroot = run_tool(workspace_root, "rustc", &["--print", "sysroot"])?;
    let path = Path::new(sysroot.trim()).join("share/doc/rust/COPYRIGHT-library.html");
    let html = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read the Rust standard library's copyright notices at {}: {e}",
            path.display()
        )
    })?;
    let version = run_tool(workspace_root, "rustc", &["--version"])?;
    Ok(format!(
        "From the toolchain that built this artefact ({}), `share/doc/rust/COPYRIGHT-library.html`, \
         converted to plain text.\n\n{}",
        version.trim(),
        html_to_text(&html)
    ))
}

/// Strips tags from `html` and decodes the handful of entities the Rust
/// toolchain's copyright file uses, keeping line structure, and collapsing
/// runs of blank lines to one.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut stripped = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => stripped.push(c),
            _ => {}
        }
    }
    let decoded = decode_entities(&stripped);
    let mut out = String::with_capacity(decoded.len());
    let mut blank_run = 0usize;
    for line in decoded.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 || out.is_empty() {
                continue;
            }
            out.push('\n');
        } else {
            blank_run = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        let (before, from_amp) = rest.split_at(start);
        out.push_str(before);
        let decoded = from_amp.find(';').filter(|end| *end <= 10).and_then(|end| {
            let entity = from_amp.get(1..end)?;
            let c = match entity {
                "lt" => '<',
                "gt" => '>',
                "amp" => '&',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                _ => {
                    let code = if let Some(hex) = entity.strip_prefix("#x") {
                        u32::from_str_radix(hex, 16).ok()?
                    } else {
                        entity.strip_prefix('#')?.parse().ok()?
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, end))
        });
        if let Some((c, end)) = decoded {
            out.push(c);
            rest = from_amp.get(end + 1..).unwrap_or_default();
        } else {
            out.push('&');
            rest = from_amp.get(1..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

const RULE: &str =
    "================================================================================";

/// The notices file for one artefact.
fn render(
    artifact: &str,
    target: &str,
    packages: &[String],
    crates: &[Crate],
    rust_std: &str,
    extra: &[(String, String)],
) -> Result<String, String> {
    let third_party: Vec<&Crate> = crates.iter().filter(|c| !c.workspace_member).collect();
    let mut out = String::new();
    // Infallible throughout: writing to a `String` never fails.
    let _ = writeln!(out, "Third-party software notices and licences");
    let _ = writeln!(out, "{artifact}, built for {target}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "HollowScript itself is licensed under Apache-2.0 OR MIT (LICENSE-APACHE,\n\
         LICENSE-MIT and NOTICE, next to this file). The binaries and libraries in\n\
         this archive ({}) also contain the third-party\n\
         software listed below, each under its own licence, reproduced in full.\n\
         Where a crate offers a choice of licences, every licence text it ships is\n\
         included and the choice remains yours.\n\
         \n\
         Written by `cargo xtask third-party-licenses` from the exact crate graph\n\
         linked into this artefact (normal dependencies only, for this target).",
        packages.join(", ")
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Summary");
    let _ = writeln!(out);
    for krate in &third_party {
        let _ = writeln!(out, "  {} {}: {}", krate.name, krate.version, krate.license);
    }
    let _ = writeln!(
        out,
        "  The Rust standard library: Apache-2.0 OR MIT, with the exceptions it lists"
    );
    for (title, _) in extra {
        let _ = writeln!(out, "  {title}");
    }
    for krate in &third_party {
        let _ = writeln!(out);
        let _ = writeln!(out, "{RULE}");
        let _ = writeln!(out, "{} {}", krate.name, krate.version);
        let _ = writeln!(out, "Licence: {}", krate.license);
        if !krate.authors.is_empty() {
            let _ = writeln!(out, "Authors: {}", krate.authors.join(", "));
        }
        let _ = writeln!(
            out,
            "Source: https://crates.io/crates/{}/{}",
            krate.name, krate.version
        );
        if let Some(repository) = &krate.repository {
            let _ = writeln!(out, "Repository: {repository}");
        }
        let _ = writeln!(out, "{RULE}");
        for (path, text) in licence_files(krate)? {
            let _ = writeln!(out);
            let _ = writeln!(out, "---- {} {}: {path} ----", krate.name, krate.version);
            let _ = writeln!(out);
            out.push_str(text.trim_end());
            out.push('\n');
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "{RULE}");
    let _ = writeln!(out, "The Rust standard library");
    let _ = writeln!(out, "{RULE}");
    let _ = writeln!(out);
    out.push_str(rust_std.trim_end());
    out.push('\n');
    for (title, text) in extra {
        let _ = writeln!(out);
        let _ = writeln!(out, "{RULE}");
        let _ = writeln!(out, "{title}");
        let _ = writeln!(out, "{RULE}");
        let _ = writeln!(out);
        out.push_str(text.trim_end());
        out.push('\n');
    }
    Ok(out)
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// Runs `program args` in `dir` and returns its standard output, or an
/// error carrying its standard error.
pub(crate) fn run_tool(dir: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{program} did not print UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{decode_entities, html_to_text, is_licence_name, parse_tree};

    fn pair(name: &str, version: &str) -> (String, String) {
        (name.to_owned(), version.to_owned())
    }

    #[test]
    fn tree_parsing_keeps_normal_crates_and_drops_procedural_macros_and_their_subtrees() {
        let tree = "\
0hollowscript-cli v0.1.0 (/somewhere/crates/hollowscript-cli)
1cap-std v4.0.3
2rustix v1.1.5
3bitflags v2.13.2
1some-derive v1.0.0 (proc-macro)
2syn v2.0.0
3proc-macro2 v1.0.0
1ureq v3.4.2
2rustix v1.1.5 (*)
";
        let parsed = parse_tree(tree).unwrap();
        assert_eq!(
            parsed,
            [
                pair("hollowscript-cli", "0.1.0"),
                pair("cap-std", "4.0.3"),
                pair("rustix", "1.1.5"),
                pair("bitflags", "2.13.2"),
                pair("ureq", "3.4.2"),
                pair("rustix", "1.1.5"),
            ]
        );
    }

    #[test]
    fn tree_parsing_refuses_a_line_it_does_not_understand() {
        assert!(parse_tree("no depth here v1.0.0\n").is_err());
        assert!(parse_tree("0name-without-version\n").is_err());
    }

    #[test]
    fn licence_file_names_are_matched_case_insensitively() {
        for name in [
            "LICENSE",
            "LICENSE-MIT",
            "license-apache-2.0",
            "LICENSE.md",
            "LICENCE",
            "COPYRIGHT",
            "COPYING",
            "NOTICE",
            "UNLICENSE",
            "LICENSE-Apache-2.0_WITH_LLVM-exception",
        ] {
            assert!(is_licence_name(name), "{name}");
        }
        for name in ["README.md", "Cargo.toml", "src", "CHANGELOG.md"] {
            assert!(!is_licence_name(name), "{name}");
        }
    }

    #[test]
    fn html_is_reduced_to_its_text_with_entities_decoded() {
        let html = "<h1>Notices</h1>\n\n\n<p>Copyright &copy; 2024 A &amp; B &lt;a@b&gt;</p>\n<pre>x &#62; y &#x3c; z</pre>\n";
        assert_eq!(
            html_to_text(html),
            "Notices\n\nCopyright &copy; 2024 A & B <a@b>\nx > y < z\n"
        );
    }

    #[test]
    fn an_ampersand_that_is_not_an_entity_is_kept() {
        assert_eq!(decode_entities("a & b; c &amp"), "a & b; c &amp");
    }
}
