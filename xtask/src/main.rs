//! Repository helper commands.
//!
//! ```text
//! cargo xtask hygiene [--root DIR]   scan the mirrored tree for forbidden content
//! cargo xtask rules                  list the rules of the scan
//! cargo xtask graph                  check the crate graph
//! cargo xtask third-party-licenses   write one artefact's third-party notices
//! cargo xtask sbom ARGS              write a release's CycloneDX SBOM
//! cargo xtask bench [--no-wasm]      run the performance benchmark harness
//! cargo xtask mem [--shape NAME]     real peak memory against section 11.4's charge (macOS)
//! cargo xtask sign ARGS              sign a release's SHA256SUMS (see 'cargo xtask sign --help')
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod bench;
mod graph;
mod hygiene;
mod json;
mod licenses;
mod mem;
mod rules;
mod sbom;
mod sign;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("hygiene") => run_hygiene(args.get(1..).unwrap_or_default()),
        Some("rules") => {
            for rule in rules::rules() {
                println!("{:<20} {}", rule.id, rule.what);
            }
            Ok(true)
        }
        Some("graph") => run_graph(),
        Some("third-party-licenses") => {
            licenses::run(&workspace_root(), args.get(1..).unwrap_or_default())
        }
        Some("sbom") => sbom::run(&workspace_root(), args.get(1..).unwrap_or_default()),
        Some("bench") => bench::run(args.get(1..).unwrap_or_default()),
        Some("mem") => mem::run(args.get(1..).unwrap_or_default()),
        Some("sign") => run_sign(args.get(1..).unwrap_or_default()),
        Some("help" | "--help" | "-h") | None => {
            println!("{}", include_str!("usage.txt"));
            Ok(true)
        }
        Some(other) => Err(format!("unknown command '{other}'; try 'cargo xtask help'")),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::from(2)
        }
    }
}

/// The root of the workspace, one level above this package.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map_or(manifest.clone(), PathBuf::from)
}

fn run_hygiene(args: &[String]) -> Result<bool, String> {
    let mut root = workspace_root();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(iter.next().ok_or("--root needs a directory")?),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    let report = hygiene::scan_tree(&root)?;
    for f in &report.findings {
        println!(
            "{}:{}:{}: {} ({})",
            f.path, f.line, f.column, f.what, f.rule
        );
    }
    if report.findings.is_empty() {
        println!(
            "hygiene: clean ({} text files scanned, {} binary files skipped)",
            report.files, report.binary
        );
        Ok(true)
    } else {
        println!(
            "hygiene: {} finding(s) in {} text files",
            report.findings.len(),
            report.files
        );
        Ok(false)
    }
}

fn run_graph() -> Result<bool, String> {
    let problems = graph::check()?;
    for p in &problems {
        println!("graph: {p}");
    }
    if problems.is_empty() {
        println!("graph: ok");
    }
    Ok(problems.is_empty())
}

const SIGN_USAGE: &str = "\
cargo xtask sign --sums FILE --version vX.Y.Z --key-version RESOURCE
                  --key-id HEX16 --public-key HEX64
                  [--untrusted-comment TEXT]

Signs FILE (a release's SHA256SUMS) with a Cloud KMS EC_SIGN_ED25519 key
version, through `gcloud kms asymmetric-sign`, and writes FILE.minisig and
minisign.pub next to it, in minisign's own format, and FILE.sshsig, the same
file signed with the same key in the SSH signature format that `ssh-keygen
-Y verify` reads, in the namespace 'hollowscript-release-vX.Y.Z' (see
xtask/src/sign.rs). Two signatures, one key: the SSH one exists so that a
machine with OpenSSH but no minisign can still verify a release.

  --sums FILE            the file to sign (usually SHA256SUMS)
  --version vX.Y.Z       the release this SHA256SUMS is for. Recorded in the
                          trusted comment, exactly
                          'hollowscript SHA256SUMS, version vX.Y.Z', which
                          the global signature covers, so install.sh
                          and install.ps1 can check the version they asked
                          for against the version that was actually signed,
                          and refuse a downgrade an unsigned latest.txt
                          alone could otherwise cause. Checked against the
                          same strict pattern the installers themselves use,
                          and every file FILE names must be named for this
                          version (hollowscript-X.Y.Z-* or
                          hollowscript-sdk-X.Y.Z-*), or nothing is signed.
  --key-version RESOURCE the KMS key version, e.g.
                          projects/P/locations/L/keyRings/R/cryptoKeys/K/cryptoKeyVersions/1
  --key-id HEX16         8 bytes, hex-encoded: the minisign key id to record
                          (chosen once, when the key is provisioned)
  --public-key HEX64     32 bytes, hex-encoded: the KMS key's own Ed25519
                          public key (read once with
                          'gcloud kms keys versions get-public-key')
  --untrusted-comment TEXT   default: 'hollowscript release signature'
";

/// Checks a release version against the same strict pattern install.sh and
/// install.ps1 apply to whatever they are about to splice into a URL: a
/// charset whitelist first (a shape check like `v[0-9]*.[0-9]*.[0-9]*` alone
/// is not a security boundary, since a glob's trailing `*` matches anything,
/// including shell metacharacters), then the vX.Y.Z(-suffix) shape.
pub(crate) fn validate_version(version: &str) -> Result<(), String> {
    if version
        .bytes()
        .any(|b| !(b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
    {
        return Err(format!(
            "'{version}' contains a character a version string may not (only letters, digits, '.' and '-' are allowed)"
        ));
    }
    let looks_right = version.strip_prefix('v').is_some_and(|rest| {
        let mut parts = rest.splitn(3, '.');
        let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let patch = patch.split('-').next().unwrap_or(patch);
        [major, minor, patch]
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    });
    if !looks_right {
        return Err(format!(
            "'{version}' does not look like a release version (expected vX.Y.Z or vX.Y.Z-suffix)"
        ));
    }
    Ok(())
}

fn run_sign(args: &[String]) -> Result<bool, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{SIGN_USAGE}");
        return Ok(true);
    }

    let mut sums_path: Option<PathBuf> = None;
    let mut version: Option<String> = None;
    let mut key_version: Option<String> = None;
    let mut key_id_hex: Option<String> = None;
    let mut public_key_hex: Option<String> = None;
    let mut untrusted_comment = "hollowscript release signature".to_string();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut next = || iter.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--sums" => sums_path = Some(PathBuf::from(next()?)),
            "--version" => version = Some(next()?),
            "--key-version" => key_version = Some(next()?),
            "--key-id" => key_id_hex = Some(next()?),
            "--public-key" => public_key_hex = Some(next()?),
            "--untrusted-comment" => untrusted_comment = next()?,
            other => {
                return Err(format!(
                    "unknown option '{other}'; see 'cargo xtask sign --help'"
                ));
            }
        }
    }

    let sums_path = sums_path.ok_or("--sums is required")?;
    let version = version.ok_or("--version is required")?;
    validate_version(&version)?;
    let key_version = key_version.ok_or("--key-version is required")?;
    let key_id = parse_hex::<8>(&key_id_hex.ok_or("--key-id is required")?)?;
    let public_key = parse_hex::<32>(&public_key_hex.ok_or("--public-key is required")?)?;
    // Not configurable: install.sh, install.ps1 and release-upload.sh all
    // require exactly this text, so a signature that says anything else is
    // one no installer would accept.
    let trusted_comment = format!("hollowscript SHA256SUMS, version {version}");

    let message =
        std::fs::read(&sums_path).map_err(|e| format!("reading {}: {e}", sums_path.display()))?;
    let sums_text = std::str::from_utf8(&message)
        .map_err(|_| format!("{} is not UTF-8 text", sums_path.display()))?;
    check_sums_names(sums_text, &version)?;
    let signer = sign::KmsSigner::new(key_version, key_id, public_key);
    let (sig_file, pub_file) =
        sign::sign_release(&signer, &message, &untrusted_comment, &trusted_comment)?;
    let ssh_file = sign::sign_ssh(&signer, &message, &sign::ssh_namespace(&version))?;

    let sig_path = PathBuf::from(format!("{}.minisig", sums_path.display()));
    let pub_path = sums_path.parent().map_or_else(
        || PathBuf::from("minisign.pub"),
        |dir| dir.join("minisign.pub"),
    );

    let ssh_path = PathBuf::from(format!("{}.sshsig", sums_path.display()));

    sign::write_new(&sig_path, &sig_file)?;
    sign::write_new(&ssh_path, &ssh_file)?;
    sign::write_new(&pub_path, &pub_file)?;
    println!("wrote {}", sig_path.display());
    println!("wrote {}", ssh_path.display());
    println!("wrote {}", pub_path.display());
    Ok(true)
}

/// The architectures a release target may start with (the first part of
/// every target triple `scripts/release-build.sh` knows).
const RELEASE_ARCHES: [&str; 3] = ["aarch64", "x86_64", "wasm32"];

/// Refuses a `SHA256SUMS` that names anything not built for `version`: every
/// entry must be `hollowscript-X.Y.Z-...` (a CLI archive, or the SBOM) or
/// `hollowscript-sdk-X.Y.Z-...` (an SDK archive), a plain file name with no
/// directory part. This is the signing half of the release consistency
/// guard (docs/design/release.md, section 1): a signature always names one
/// version, and so must everything it covers.
pub(crate) fn check_sums_names(sums: &str, version: &str) -> Result<(), String> {
    let bare = version.strip_prefix('v').unwrap_or(version);
    let cli = format!("hollowscript-{bare}-");
    let sdk = format!("hollowscript-sdk-{bare}-");
    let mut entries = 0usize;
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("malformed SHA256SUMS line: '{line}'"));
        };
        let name = name.strip_prefix('*').unwrap_or(name);
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("malformed SHA256SUMS hash for {name}"));
        }
        if name.contains('/') || name.contains('\\') {
            return Err(format!("SHA256SUMS names a path, not a file name: {name}"));
        }
        // What follows the version must be a target (its architecture
        // first) or the SBOM: a bare prefix check alone would take
        // `hollowscript-0.1.0-rc.1-...`, a different release, for 0.1.0.
        let rest = name.strip_prefix(&sdk).or_else(|| name.strip_prefix(&cli));
        let named_for_version = rest.is_some_and(|rest| {
            rest.starts_with("sbom.")
                || RELEASE_ARCHES
                    .iter()
                    .any(|arch| rest.starts_with(&format!("{arch}-")))
        });
        if !named_for_version {
            return Err(format!(
                "SHA256SUMS names {name}, which is not named for {version} \
                 (expected {cli}<target>.* or {sdk}<target>.*); refusing to sign a mixed release"
            ));
        }
        entries += 1;
    }
    if entries == 0 {
        return Err("SHA256SUMS names nothing".to_owned());
    }
    Ok(())
}

fn parse_hex<const N: usize>(text: &str) -> Result<[u8; N], String> {
    if !text.is_ascii() || text.len() != N * 2 {
        return Err(format!(
            "expected {} hex characters, got {}",
            N * 2,
            text.len()
        ));
    }
    let mut bytes = text.as_bytes().chunks_exact(2);
    let mut out = [0u8; N];
    for slot in &mut out {
        let pair = bytes.next().ok_or("not enough hex characters")?;
        let pair_text =
            std::str::from_utf8(pair).map_err(|_| format!("'{text}' is not valid hex"))?;
        *slot =
            u8::from_str_radix(pair_text, 16).map_err(|_| format!("'{text}' is not valid hex"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::check_sums_names;

    const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn sums_naming_only_this_version_are_accepted() {
        let sums = format!(
            "{H}  hollowscript-0.1.0-aarch64-apple-darwin.tar.gz\n\
             {H}  hollowscript-sdk-0.1.0-aarch64-apple-darwin.tar.gz\n\
             {H} *hollowscript-0.1.0-sbom.cdx.json\n"
        );
        assert_eq!(check_sums_names(&sums, "v0.1.0"), Ok(()));
    }

    #[test]
    fn sums_naming_another_version_or_a_stray_file_are_refused() {
        for bad in [
            format!("{H}  hollowscript-0.1.1-aarch64-apple-darwin.tar.gz\n"),
            format!("{H}  hollowscript-0.1.0-rc.1-aarch64-apple-darwin.tar.gz\n"),
            format!("{H}  install.sh\n"),
            format!("{H}  sub/hollowscript-0.1.0-x.tar.gz\n"),
            format!("{H}  hollowscript-0.1.0-a.tar.gz extra\n"),
            "nothash  hollowscript-0.1.0-a.tar.gz\n".to_owned(),
            String::new(),
        ] {
            assert!(check_sums_names(&bad, "v0.1.0").is_err(), "{bad}");
        }
    }
}
