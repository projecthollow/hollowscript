//! A release's software bill of materials (`cargo xtask sbom`), as a
//! `CycloneDX` 1.5 JSON document.
//!
//! It lists exactly what ships: every crate linked into each of the
//! release's artefacts (the same crate sets `cargo xtask
//! third-party-licenses` writes notices for, see `licenses.rs`), each once,
//! with a property naming every artefact it is linked into, its licence
//! expression, its package URL and, for a crates.io crate, the SHA-256 of
//! the published `.crate` file as `Cargo.lock` records it. Development,
//! build and `xtask` dependencies are not linked into any artefact, so they
//! are not listed. No absolute local path is written: a crate is identified
//! by name, version and package URL alone.
//!
//! The document is deterministic for a given commit: its one timestamp is
//! `SOURCE_DATE_EPOCH` (the release commit's own date, which
//! `scripts/release-build.sh` sets) and it carries no random serial number.

use crate::licenses::{self, Crate};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// `cargo xtask sbom --version vX.Y.Z --out FILE --artifact NAME=TARGET:PKG[,PKG]...`
pub(crate) fn run(workspace_root: &Path, args: &[String]) -> Result<bool, String> {
    let mut version: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut artifacts: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut next = || iter.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--version" => version = Some(next()?),
            "--out" => out = Some(PathBuf::from(next()?)),
            "--artifact" => artifacts.push(parse_artifact(&next()?)?),
            other => {
                return Err(format!(
                    "unknown option '{other}'; usage: cargo xtask sbom --version vX.Y.Z \
                     --out FILE --artifact NAME=TARGET:PKG[,PKG]..."
                ));
            }
        }
    }
    let version = version.ok_or("--version is required")?;
    crate::validate_version(&version)?;
    let out = out.ok_or("--out FILE is required")?;
    if artifacts.is_empty() {
        return Err("at least one --artifact is required".to_owned());
    }

    // (name, version) -> (crate, the artefacts it is linked into)
    let mut components: BTreeMap<(String, String), (Crate, Vec<String>)> = BTreeMap::new();
    for (artifact, target, packages) in &artifacts {
        for krate in licenses::linked_crates(workspace_root, target, packages)? {
            let entry = components
                .entry((krate.name.clone(), krate.version.clone()))
                .or_insert_with(|| (krate.clone(), Vec::new()));
            entry.1.push(artifact.clone());
        }
    }
    let checksums = lock_checksums(workspace_root)?;
    let rustc = licenses::run_tool(workspace_root, "rustc", &["--version"])?;
    let timestamp = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(iso8601_utc);

    let doc = render(
        &version,
        timestamp.as_deref(),
        rustc.trim(),
        &components,
        &checksums,
    );
    std::fs::write(&out, doc).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    println!(
        "wrote {} ({} components across {} artefact(s))",
        out.display(),
        components.len(),
        artifacts.len()
    );
    Ok(true)
}

/// `NAME=TARGET:PKG[,PKG]`.
fn parse_artifact(text: &str) -> Result<(String, String, Vec<String>), String> {
    let (name, rest) = text.split_once('=').ok_or(format!(
        "--artifact expects NAME=TARGET:PKG[,PKG], got '{text}'"
    ))?;
    let (target, packages) = rest.split_once(':').ok_or(format!(
        "--artifact expects NAME=TARGET:PKG[,PKG], got '{text}'"
    ))?;
    let packages: Vec<String> = packages
        .split(',')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    if name.is_empty() || target.is_empty() || packages.is_empty() {
        return Err(format!(
            "--artifact expects NAME=TARGET:PKG[,PKG], got '{text}'"
        ));
    }
    Ok((name.to_owned(), target.to_owned(), packages))
}

/// `Cargo.lock`'s recorded SHA-256 of each registry package's `.crate`.
fn lock_checksums(workspace_root: &Path) -> Result<BTreeMap<(String, String), String>, String> {
    let path = workspace_root.join("Cargo.lock");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(parse_lock_checksums(&text))
}

fn parse_lock_checksums(text: &str) -> BTreeMap<(String, String), String> {
    let mut map = BTreeMap::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let unquote = |v: &str| v.trim().trim_matches('"').to_owned();
    for line in text.lines() {
        if line.trim() == "[[package]]" {
            name = None;
            version = None;
        } else if let Some(v) = line.strip_prefix("name = ") {
            name = Some(unquote(v));
        } else if let Some(v) = line.strip_prefix("version = ") {
            version = Some(unquote(v));
        } else if let Some(v) = line.strip_prefix("checksum = ") {
            if let (Some(n), Some(ver)) = (&name, &version) {
                map.insert((n.clone(), ver.clone()), unquote(v));
            }
        }
    }
    map
}

/// Days since 1970-01-01 to a civil (year, month, day), Howard Hinnant's
/// algorithm, so this needs no date crate.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Seconds since the epoch as an ISO 8601 UTC timestamp.
fn iso8601_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// A JSON string literal.
fn js(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A crate's `license` field as an SPDX expression: the old `A/B` form some
/// crates still use means `A OR B`.
fn spdx_expression(license: &str) -> String {
    license.replace('/', " OR ")
}

fn render(
    version: &str,
    timestamp: Option<&str>,
    rustc: &str,
    components: &BTreeMap<(String, String), (Crate, Vec<String>)>,
    checksums: &BTreeMap<(String, String), String>,
) -> String {
    let bare = version.trim_start_matches('v');
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"bomFormat\": \"CycloneDX\",\n");
    out.push_str("  \"specVersion\": \"1.5\",\n");
    out.push_str("  \"version\": 1,\n");
    out.push_str("  \"metadata\": {\n");
    if let Some(ts) = timestamp {
        let _ = writeln!(out, "    \"timestamp\": {},", js(ts));
    }
    out.push_str("    \"tools\": { \"components\": [ { \"type\": \"application\", \"name\": \"cargo xtask sbom\" } ] },\n");
    let _ = writeln!(
        out,
        "    \"component\": {{ \"type\": \"application\", \"bom-ref\": \"hollowscript@{bare}\", \"name\": \"hollowscript\", \"version\": {}, \"licenses\": [ {{ \"expression\": \"Apache-2.0 OR MIT\" }} ] }},",
        js(bare)
    );
    let _ = writeln!(
        out,
        "    \"properties\": [ {{ \"name\": \"hollowscript:rustc\", \"value\": {} }} ]",
        js(rustc)
    );
    out.push_str("  },\n");
    out.push_str("  \"components\": [\n");
    let count = components.len();
    for (index, ((name, ver), (krate, artifacts))) in components.iter().enumerate() {
        let purl = format!("pkg:cargo/{name}@{ver}");
        out.push_str("    {\n");
        out.push_str("      \"type\": \"library\",\n");
        let _ = writeln!(out, "      \"bom-ref\": {},", js(&purl));
        let _ = writeln!(out, "      \"name\": {},", js(name));
        let _ = writeln!(out, "      \"version\": {},", js(ver));
        let _ = writeln!(out, "      \"purl\": {},", js(&purl));
        let _ = writeln!(
            out,
            "      \"licenses\": [ {{ \"expression\": {} }} ],",
            js(&spdx_expression(&krate.license))
        );
        if let Some(sum) = checksums.get(&(name.clone(), ver.clone())) {
            let _ = writeln!(
                out,
                "      \"hashes\": [ {{ \"alg\": \"SHA-256\", \"content\": {} }} ],",
                js(sum)
            );
        }
        if let Some(repository) = &krate.repository {
            let _ = writeln!(
                out,
                "      \"externalReferences\": [ {{ \"type\": \"vcs\", \"url\": {} }} ],",
                js(repository)
            );
        }
        out.push_str("      \"properties\": [\n");
        let _ = write!(
            out,
            "        {{ \"name\": \"hollowscript:origin\", \"value\": {} }}",
            js(if krate.workspace_member {
                "workspace"
            } else {
                "crates.io"
            })
        );
        for artifact in artifacts {
            let _ = write!(
                out,
                ",\n        {{ \"name\": \"hollowscript:artifact\", \"value\": {} }}",
                js(artifact)
            );
        }
        out.push_str("\n      ]\n");
        out.push_str(if index + 1 == count {
            "    }\n"
        } else {
            "    },\n"
        });
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::{
        civil_from_days, iso8601_utc, js, parse_artifact, parse_lock_checksums, spdx_expression,
    };

    #[test]
    fn timestamps_are_iso8601_utc() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601_utc(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(js("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
    }

    #[test]
    fn old_style_licence_fields_become_spdx_expressions() {
        assert_eq!(spdx_expression("MIT/Apache-2.0"), "MIT OR Apache-2.0");
        assert_eq!(spdx_expression("Apache-2.0 AND ISC"), "Apache-2.0 AND ISC");
    }

    #[test]
    fn artifact_specs_are_parsed_and_malformed_ones_refused() {
        let (name, target, packages) =
            parse_artifact("a.tar.gz=x86_64-unknown-linux-musl:hollowscript-cli,hollowscript-lsp")
                .unwrap();
        assert_eq!(name, "a.tar.gz");
        assert_eq!(target, "x86_64-unknown-linux-musl");
        assert_eq!(packages, ["hollowscript-cli", "hollowscript-lsp"]);
        assert!(parse_artifact("a.tar.gz").is_err());
        assert!(parse_artifact("a.tar.gz=target").is_err());
        assert!(parse_artifact("a.tar.gz=target:").is_err());
    }

    #[test]
    fn lock_checksums_are_read_per_package() {
        let lock = "\
[[package]]
name = \"a\"
version = \"1.0.0\"
source = \"registry+https://github.com/rust-lang/crates.io-index\"
checksum = \"abc\"

[[package]]
name = \"local\"
version = \"0.1.0\"
";
        let map = parse_lock_checksums(lock);
        assert_eq!(map.len(), 1);
        assert_eq!(map[&("a".to_owned(), "1.0.0".to_owned())], "abc");
    }
}
