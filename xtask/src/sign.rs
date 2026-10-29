//! Signing a release's `SHA256SUMS` file, and writing the result in a format
//! `minisign` itself can verify.
//!
//! The owner's decision (see the workspace's own `PREAMBLE`, not part of
//! this repository) is a non-exportable Cloud KMS Ed25519 key: the private
//! key material never leaves Google's HSM or software backend, so nothing
//! here ever holds it, reads it from disk, or could leak it. What this
//! module holds instead is [`Signer`], an abstraction over "sign these
//! bytes", with two implementations: [`KmsSigner`], which shells out to
//! `gcloud kms asymmetric-sign`, and `LocalSigner` (test-only, so not a doc
//! link: it only exists under `#[cfg(test)]`), an in-memory Ed25519 key
//! used only by this module's own tests, to prove the minisign encoding is
//! correct without needing KMS credentials at all. Both go through the same
//! [`sign_release`] function, so the encoding is exercised identically
//! whichever one produced the signature.
//!
//! **The format.** `minisign`'s legacy `Ed` algorithm signs the file's raw
//! bytes directly (no prehashing, which matters for a `SHA256SUMS` file:
//! it is small, so there is no reason to pay for the `ED` prehashed variant
//! minisign reserves for large files). A signature file is four lines: an
//! untrusted comment, the base64 of `"Ed" || key_id || signature`, a trusted
//! comment, and the base64 of a second Ed25519 signature over
//! `signature || trusted_comment_bytes`, the "global signature", which is
//! what stops an attacker who cannot forge a signature from still editing
//! the trusted comment. A public key file is one line of the same shape,
//! `"Ed" || key_id || public_key`.

use std::io::Write as _;
use std::path::Path;
use std::process::Command;

/// Eight bytes that a public key file and every signature it verifies must
/// share, so minisign can tell the reader which key to check against
/// without trying every key it has. Not a secret; it is written in both
/// files in the clear.
pub(crate) type KeyId = [u8; 8];

/// An Ed25519 public key, exactly as it appears in a minisign key file.
pub(crate) type PublicKey = [u8; 32];

/// An Ed25519 signature, exactly as it appears in a minisign signature
/// file.
pub(crate) type Signature = [u8; 64];

/// An abstraction over "sign these bytes with the release key", so the
/// minisign encoding below can be exercised with a key this module holds
/// in memory, in a test, and trusted in production only with a Cloud KMS
/// key that never leaves KMS.
pub(crate) trait Signer {
    /// Signs `message` and returns the raw 64-byte Ed25519 signature.
    fn sign(&self, message: &[u8]) -> Result<Signature, String>;

    /// The signer's public key, for the public key file.
    fn public_key(&self) -> PublicKey;

    /// The key id recorded in both the public key file and every signature
    /// this signer produces.
    fn key_id(&self) -> KeyId;
}

/// Signs `message` twice, the way minisign's own `Ed` format requires: once
/// over the message itself, and once over that signature followed by the
/// trusted comment, so the comment cannot be edited after the fact without
/// the global signature failing to verify. Returns the two encoded files'
/// contents, ready to write.
pub(crate) fn sign_release(
    signer: &dyn Signer,
    message: &[u8],
    untrusted_comment: &str,
    trusted_comment: &str,
) -> Result<(String, String), String> {
    let signature = signer.sign(message)?;
    let key_id = signer.key_id();

    let mut sig_blob = Vec::with_capacity(2 + 8 + 64);
    sig_blob.extend_from_slice(b"Ed");
    sig_blob.extend_from_slice(&key_id);
    sig_blob.extend_from_slice(&signature);

    let mut global_message = Vec::with_capacity(64 + trusted_comment.len());
    global_message.extend_from_slice(&signature);
    global_message.extend_from_slice(trusted_comment.as_bytes());
    let global_signature = signer.sign(&global_message)?;

    let signature_file = format!(
        "untrusted comment: {untrusted_comment}\n{}\ntrusted comment: {trusted_comment}\n{}\n",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &sig_blob),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, global_signature),
    );

    let mut key_blob = Vec::with_capacity(2 + 8 + 32);
    key_blob.extend_from_slice(b"Ed");
    key_blob.extend_from_slice(&key_id);
    key_blob.extend_from_slice(&signer.public_key());
    let public_key_file = format!(
        "untrusted comment: minisign public key {}\n{}\n",
        hex(&key_id),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &key_blob),
    );

    Ok((signature_file, public_key_file))
}

/// The namespace an SSH signature of a release's `SHA256SUMS` is made in:
/// `hollowscript-release-` followed by the version. The verifier
/// (`ssh-keygen -Y verify -n NAMESPACE`) refuses a signature made in any
/// other namespace, so this is what ties the SSH signature to one version,
/// the way the signed trusted comment ties the minisign one.
pub(crate) fn ssh_namespace(version: &str) -> String {
    format!("hollowscript-release-{version}")
}

/// Signs `message` in the SSH signature format (`SSHSIG`, the format
/// `ssh-keygen -Y sign` writes and `ssh-keygen -Y verify` reads) with the
/// same key [`sign_release`] uses, and returns the armoured signature file.
///
/// Why a second format: minisign is not installed on a stock macOS or
/// Windows machine, but OpenSSH is, and `ssh-keygen -Y verify` can verify an
/// Ed25519 signature only in this format. The key is the same; nothing about
/// who can sign changes. What is signed is not the file but a fixed
/// structure: the literal `SSHSIG`, the namespace, a reserved empty string,
/// the hash algorithm (`sha512`) and the SHA-512 of the message. A minisign
/// signature is over the raw file, so a signature in one format can never
/// be replayed as one in the other: the one starts with `SSHSIG`, which a
/// `SHA256SUMS` file does not, and the other covers 64 signature bytes
/// before any text.
pub(crate) fn sign_ssh(
    signer: &dyn Signer,
    message: &[u8],
    namespace: &str,
) -> Result<String, String> {
    use sha2::Digest as _;

    const KEY_TYPE: &[u8] = b"ssh-ed25519";
    const HASH: &[u8] = b"sha512";

    let digest = sha2::Sha512::digest(message);
    let mut signed = Vec::new();
    signed.extend_from_slice(b"SSHSIG");
    put_string(&mut signed, namespace.as_bytes())?;
    put_string(&mut signed, b"")?;
    put_string(&mut signed, HASH)?;
    put_string(&mut signed, &digest)?;
    let signature = signer.sign(&signed)?;

    let mut key_blob = Vec::new();
    put_string(&mut key_blob, KEY_TYPE)?;
    put_string(&mut key_blob, &signer.public_key())?;
    let mut signature_blob = Vec::new();
    put_string(&mut signature_blob, KEY_TYPE)?;
    put_string(&mut signature_blob, &signature)?;

    let mut blob = Vec::new();
    blob.extend_from_slice(b"SSHSIG");
    blob.extend_from_slice(&1u32.to_be_bytes());
    put_string(&mut blob, &key_blob)?;
    put_string(&mut blob, namespace.as_bytes())?;
    put_string(&mut blob, b"")?;
    put_string(&mut blob, HASH)?;
    put_string(&mut blob, &signature_blob)?;

    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &blob);
    let mut armoured = String::from("-----BEGIN SSH SIGNATURE-----\n");
    for line in encoded.as_bytes().chunks(70) {
        armoured.push_str(&String::from_utf8_lossy(line));
        armoured.push('\n');
    }
    armoured.push_str("-----END SSH SIGNATURE-----\n");
    Ok(armoured)
}

/// Appends an SSH wire `string`: a big-endian `u32` length, then the bytes.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    let len = u32::try_from(bytes.len()).map_err(|_| "a field is too long to encode".to_owned())?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02X}");
    }
    out
}

/// Signs with a Cloud KMS `EC_SIGN_ED25519` key version, through the
/// `gcloud` CLI (`gcloud kms asymmetric-sign`). The key never leaves KMS:
/// this only ever sends the message to be signed and receives a signature
/// back, over an authenticated `gcloud` invocation, and holds no key
/// material itself. `key_id` and `public_key` are supplied at construction
/// (read once, with `gcloud kms keys versions get-public-key`, when the key
/// is provisioned) rather than fetched on every call, since KMS asymmetric
/// public keys do not change under a fixed key version.
#[derive(Debug, Clone)]
pub(crate) struct KmsSigner {
    /// A KMS key version resource name, for example
    /// `projects/P/locations/L/keyRings/R/cryptoKeys/K/cryptoKeyVersions/1`.
    pub key_version: String,
    key_id: KeyId,
    public_key: PublicKey,
}

impl KmsSigner {
    /// Builds a signer for an already-provisioned KMS key version. `key_id`
    /// is the minisign key id to record in every signature this signer
    /// produces (chosen once, when the key is provisioned, and recorded
    /// alongside `key_version` in the release process's own configuration,
    /// not derived from anything KMS returns); `public_key` is the KMS
    /// key's own Ed25519 public key, in its raw 32-byte form.
    pub(crate) fn new(key_version: String, key_id: KeyId, public_key: PublicKey) -> Self {
        Self {
            key_version,
            key_id,
            public_key,
        }
    }
}

impl Signer for KmsSigner {
    fn sign(&self, message: &[u8]) -> Result<Signature, String> {
        let mut input = tempfile("hollowscript-kms-sign-in")?;
        input
            .write_all(message)
            .map_err(|e| format!("writing the message to sign: {e}"))?;
        let input_path = input.into_temp_path();

        // The signature goes to a file of its own: `gcloud kms
        // asymmetric-sign` writes the raw signature bytes there, 64 of them
        // for Ed25519, with nothing to decode. `--version` takes the full
        // key version resource name, and an Ed25519 key signs the message
        // itself, so no `--digest-algorithm` is given.
        let signature_path = tempfile("hollowscript-kms-sign-out")?.into_temp_path();
        let output = Command::new("gcloud")
            .args(["kms", "asymmetric-sign", "--version", &self.key_version])
            .arg("--input-file")
            .arg(input_path.as_ref())
            .arg("--signature-file")
            .arg(signature_path.as_ref())
            .output()
            .map_err(|e| format!("running gcloud kms asymmetric-sign: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "gcloud kms asymmetric-sign failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let raw = std::fs::read(signature_path.as_ref())
            .map_err(|e| format!("reading the signature gcloud wrote: {e}"))?;
        Signature::try_from(raw.as_slice()).map_err(|_| {
            format!(
                "expected a 64-byte Ed25519 signature, got {} bytes",
                raw.len()
            )
        })
    }

    fn public_key(&self) -> PublicKey {
        self.public_key
    }

    fn key_id(&self) -> KeyId {
        self.key_id
    }
}

fn tempfile(prefix: &str) -> Result<tempfile_shim::NamedTempFile, String> {
    tempfile_shim::NamedTempFile::new(prefix)
}

/// A minimal stand-in for the `tempfile` crate: this workspace adds a
/// dependency only when it has weighed the alternative (see
/// `docs/design/dependencies.md`), and a single "create a private file next
/// to the others `std::env::temp_dir()` already gives every process" is not
/// worth one. `NamedTempFile::into_temp_path` returns a `PathBuf` alone
/// (the file is removed on drop of the returned guard).
mod tempfile_shim {
    use std::fs::File;
    use std::path::{Path, PathBuf};

    pub(super) struct NamedTempFile {
        path: PathBuf,
        file: File,
    }

    impl NamedTempFile {
        pub(super) fn new(prefix: &str) -> Result<Self, String> {
            let path = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default(),
            ));
            let file =
                File::create(&path).map_err(|e| format!("creating {}: {e}", path.display()))?;
            Ok(Self { path, file })
        }

        pub(super) fn into_temp_path(self) -> TempPath {
            TempPath { path: self.path }
        }
    }

    impl std::io::Write for NamedTempFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.file.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }

    pub(super) struct TempPath {
        path: PathBuf,
    }

    impl AsRef<Path> for TempPath {
        fn as_ref(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// An in-memory Ed25519 key, used only by this module's tests, to prove the
/// minisign encoding [`sign_release`] produces is byte-compatible with the
/// real `minisign` tool and with `openssl`, without needing Cloud KMS
/// credentials to do it. Never used outside `#[cfg(test)]`: production
/// signing goes through [`KmsSigner`], so the release key is never held in
/// process memory as raw bytes.
#[cfg(test)]
pub(crate) struct LocalSigner {
    signing_key: ed25519_dalek::SigningKey,
    key_id: KeyId,
}

#[cfg(test)]
impl LocalSigner {
    pub(crate) fn generate() -> Self {
        use rand::RngCore as _;
        let mut rng = rand::rngs::OsRng;
        let signing_key = ed25519_dalek::SigningKey::generate(&mut rng);
        let mut key_id = [0u8; 8];
        rng.fill_bytes(&mut key_id);
        Self {
            signing_key,
            key_id,
        }
    }
}

#[cfg(test)]
impl Signer for LocalSigner {
    fn sign(&self, message: &[u8]) -> Result<Signature, String> {
        use ed25519_dalek::Signer as _;
        Ok(self.signing_key.sign(message).to_bytes())
    }

    fn public_key(&self) -> PublicKey {
        self.signing_key.verifying_key().to_bytes()
    }

    fn key_id(&self) -> KeyId {
        self.key_id
    }
}

/// Writes `contents` to `path`, refusing to silently overwrite a signature
/// or key file that already exists with different bytes: a release script
/// re-run over the same output directory should fail loudly rather than
/// quietly replace a signature that was already published.
pub(crate) fn write_new(path: &Path, contents: &str) -> Result<(), String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == contents {
            return Ok(());
        }
        return Err(format!(
            "{} already exists with different content; remove it first if that is intended",
            path.display()
        ));
    }
    std::fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// The end-to-end proof the release process's design asks for: sign
    /// with a local key through exactly the code path production uses
    /// (`sign_release`, the same function `KmsSigner` goes through), then
    /// verify the result with the real `minisign` binary and, separately,
    /// with `openssl`. If either external tool is not installed the test
    /// explains that rather than failing, since this workspace does not
    /// control the machine's tool inventory.
    #[test]
    fn minisign_and_openssl_verify_a_locally_signed_release() {
        let signer = LocalSigner::generate();
        let message = b"hollowscript-0.0.0-testcommit  hollowscript-aarch64-apple-darwin.tar.gz\n";

        let (sig_file, pub_file) = sign_release(
            &signer,
            message,
            "hollowscript release signature",
            "hollowscript SHA256SUMS, commit testcommit",
        )
        .expect("signing must not fail for a valid local key");

        let dir =
            std::env::temp_dir().join(format!("hollowscript-sign-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let msg_path = dir.join("SHA256SUMS");
        let sig_path = dir.join("SHA256SUMS.minisig");
        let pub_path = dir.join("minisign.pub");
        std::fs::write(&msg_path, message).unwrap();
        std::fs::write(&sig_path, &sig_file).unwrap();
        std::fs::write(&pub_path, &pub_file).unwrap();

        match Command::new("minisign")
            .args(["-V", "-p"])
            .arg(&pub_path)
            .arg("-m")
            .arg(&msg_path)
            .arg("-x")
            .arg(&sig_path)
            .output()
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => panic!(
                "minisign -V rejected our signature: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping the minisign half of this test: minisign is not installed");
            }
            Err(e) => panic!("running minisign: {e}"),
        }

        // openssl verifies the raw Ed25519 signature (not minisign's
        // framing): build a PEM SubjectPublicKeyInfo by hand around the
        // raw 32-byte key (the fixed 12-byte DER prefix for an Ed25519
        // SPKI, RFC 8410) and a detached signature file holding the 64 raw
        // signature bytes, both decoded back out of what `sign_release`
        // just encoded.
        let public_key = signer.public_key();
        let spki_der = ed25519_spki_der(&public_key);
        let spki_pem = pem("PUBLIC KEY", &spki_der);
        let spki_path = dir.join("key.pem");
        std::fs::write(&spki_path, spki_pem).unwrap();

        let signature = signer
            .sign(message)
            .expect("re-signing for the openssl half");
        let raw_sig_path = dir.join("SHA256SUMS.raw.sig");
        std::fs::write(&raw_sig_path, signature).unwrap();

        let openssl_version = Command::new("openssl").arg("version").output();
        match openssl_version {
            Ok(version) if String::from_utf8_lossy(&version.stdout).starts_with("OpenSSL ") => {
                let output = Command::new("openssl")
                    .args(["pkeyutl", "-verify", "-pubin", "-inkey"])
                    .arg(&spki_path)
                    .args(["-rawin", "-in"])
                    .arg(&msg_path)
                    .arg("-sigfile")
                    .arg(&raw_sig_path)
                    .stdout(Stdio::piped())
                    .output()
                    .expect("OpenSSL is installed");
                assert!(
                    output.status.success(),
                    "openssl pkeyutl rejected our signature: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    String::from_utf8_lossy(&output.stdout)
                        .contains("Signature Verified Successfully"),
                    "openssl did not report success: {output:?}",
                );
            }
            Ok(version) => eprintln!(
                "skipping the openssl half of this test: OpenSSL with Ed25519 support is not installed (found {})",
                String::from_utf8_lossy(&version.stdout).trim()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping the openssl half of this test: openssl is not installed");
            }
            Err(e) => panic!("checking openssl: {e}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The SSH signature `sign_ssh` writes is verified by the real
    /// `ssh-keygen -Y verify` against a public key line built from the same
    /// raw key, accepted only in its own namespace, and refused once the
    /// message changes. Skipped, with a note, where `ssh-keygen` is absent.
    #[test]
    fn ssh_keygen_verifies_an_ssh_signed_release() {
        let signer = LocalSigner::generate();
        let message = b"0123456789abcdef  hollowscript-0.1.0-aarch64-apple-darwin.tar.gz\n";
        let namespace = ssh_namespace("v0.1.0");
        assert_eq!(namespace, "hollowscript-release-v0.1.0");
        let signature = sign_ssh(&signer, message, &namespace).expect("signing with a local key");
        assert!(signature.starts_with("-----BEGIN SSH SIGNATURE-----\n"));
        assert!(signature.ends_with("-----END SSH SIGNATURE-----\n"));
        assert!(
            signature
                .lines()
                .all(|l| l.len() <= 70 || l.starts_with("-----"))
        );

        let dir =
            std::env::temp_dir().join(format!("hollowscript-sshsig-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sig_path = dir.join("SHA256SUMS.sshsig");
        std::fs::write(&sig_path, &signature).unwrap();
        // The public key in `ssh-keygen`'s allowed signers form: the wire
        // blob is string "ssh-ed25519" then string key.
        let mut blob = Vec::new();
        put_string(&mut blob, b"ssh-ed25519").unwrap();
        put_string(&mut blob, &signer.public_key()).unwrap();
        let allowed = format!(
            "releases@example.test ssh-ed25519 {}\n",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &blob)
        );
        let allowed_path = dir.join("allowed_signers");
        std::fs::write(&allowed_path, allowed).unwrap();

        let verify = |namespace: &str, message: &[u8]| -> Option<bool> {
            let mut child = match Command::new("ssh-keygen")
                .args([
                    "-Y",
                    "verify",
                    "-I",
                    "releases@example.test",
                    "-n",
                    namespace,
                ])
                .arg("-f")
                .arg(&allowed_path)
                .arg("-s")
                .arg(&sig_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
                Err(e) => panic!("running ssh-keygen: {e}"),
            };
            child.stdin.take().unwrap().write_all(message).unwrap();
            Some(child.wait().unwrap().success())
        };
        match verify(&namespace, message) {
            None => {
                eprintln!("skipping the ssh-keygen half of this test: ssh-keygen is not installed");
            }
            Some(ok) => {
                assert!(ok, "ssh-keygen -Y verify rejected our signature");
                assert_eq!(verify("hollowscript-release-v0.1.1", message), Some(false));
                assert_eq!(verify(&namespace, b"another file\n"), Some(false));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fixed 12-byte prefix RFC 8410 gives an Ed25519
    /// `SubjectPublicKeyInfo`, in front of the raw 32-byte public key.
    fn ed25519_spki_der(public_key: &PublicKey) -> Vec<u8> {
        let mut der = vec![
            0x30, 0x2a, // SEQUENCE, 42 bytes
            0x30, 0x05, // SEQUENCE, 5 bytes (AlgorithmIdentifier)
            0x06, 0x03, 0x2b, 0x65, 0x70, // OID 1.3.101.112 (Ed25519)
            0x03, 0x21, 0x00, // BIT STRING, 33 bytes, 0 unused bits
        ];
        der.extend_from_slice(public_key);
        der
    }

    fn pem(label: &str, der: &[u8]) -> String {
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, der);
        let mut out = format!("-----BEGIN {label}-----\n");
        for chunk in encoded.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(chunk).unwrap());
            out.push('\n');
        }
        out.push_str("-----END ");
        out.push_str(label);
        out.push_str("-----\n");
        out
    }

    #[test]
    fn write_new_refuses_to_silently_replace_different_content() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-write-new-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SHA256SUMS.minisig");

        write_new(&path, "first").unwrap();
        write_new(&path, "first").unwrap(); // identical content: fine
        let err = write_new(&path, "second").unwrap_err();
        assert!(err.contains("already exists"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
