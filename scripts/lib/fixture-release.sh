#!/usr/bin/env bash
# Shared by scripts/test-install and scripts/test-release: builds a small
# fake release, signed with throwaway keys, so the installers and the upload
# script can be exercised end to end with no network and no real key.
#
# Sourced, never run. Needs bash, python3, tar, gzip, base64, shasum or
# sha256sum, and ssh-keygen (OpenSSH 8.0 or newer); minisign is used too when
# it is present.
#
# One throwaway key, as the real release has: it signs SHA256SUMS in two
# formats, minisign's (made by `minisign` itself, in the legacy "Ed" format
# the real release uses, `-l`) and the SSH signature format (made by
# `ssh-keygen -Y sign`). It is made as an OpenSSH key, and written out as a
# minisign secret key and public key file too, so both tools sign with the
# same 32 bytes. It is not the pinned release key, so a test passes the
# public key file to the installer as the extra trusted key
# (HOLLOWSCRIPT_PUBLIC_KEY_FILE).

# fixture_tools: sets have_minisign, and dies if what every test needs is
# missing.
fixture_tools() {
    command -v ssh-keygen >/dev/null 2>&1 || {
        printf 'ssh-keygen is required to build the test fixtures\n' >&2
        return 1
    }
    command -v tar >/dev/null 2>&1 || return 1
    have_minisign=0
    command -v minisign >/dev/null 2>&1 && have_minisign=1
    return 0
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# fixture_keys DIR: writes DIR/ssh.key (the OpenSSH key), DIR/one.pub (its
# public half in minisign's file format) and, with minisign, DIR/one.key (its
# secret half in minisign's unencrypted secret key format).
fixture_keys() {
    local dir="$1"
    mkdir -p "$dir"
    ssh-keygen -q -t ed25519 -N '' -C fixture -f "$dir/ssh.key"
    python3 - "$dir" <<'PY'
import base64, hashlib, struct, sys

d = sys.argv[1]
text = open(d + "/ssh.key").read().split("-----")[2]
blob = base64.b64decode("".join(text.split()))
assert blob.startswith(b"openssh-key-v1\0")
pos = len(b"openssh-key-v1\0")

current = blob

def string():
    global pos
    (n,) = struct.unpack(">I", current[pos:pos + 4])
    value = current[pos + 4:pos + 4 + n]
    pos += 4 + n
    return value

cipher, kdf, kdfopts = string(), string(), string()
assert cipher == b"none" and kdf == b"none"
pos += 4  # the number of keys, always 1 here
public_blob = string()
private_blob = string()
current = private_blob
# The private section: two check integers, the key type, the public key, the
# private key (seed then public key) and a comment.
pos = 8
assert string() == b"ssh-ed25519"
public = string()
secret = string()
assert len(public) == 32 and len(secret) == 64 and secret[32:] == public
assert public_blob.endswith(public)

key_id = bytes(range(1, 9))
checksum = hashlib.blake2b(b"Ed" + key_id + secret, digest_size=32).digest()
secret_file = b"Ed" + b"\0\0" + b"B2" + bytes(32) + bytes(8) + bytes(8) + key_id + secret + checksum
b64 = lambda b: base64.b64encode(b).decode()
open(d + "/one.pub", "w").write("untrusted comment: fixture key\n" + b64(b"Ed" + key_id + public) + "\n")
open(d + "/one.key", "w").write("untrusted comment: minisign encrypted secret key\n" + b64(secret_file) + "\n")
PY
}

# fixture_binary VERSION: a stand-in for the hollowscript binary. It reports
# its version, except that when HS_FIXTURE_FAIL_IN_PLACE is set it fails once
# it runs from an installed location, which is what a binary that passes the
# installer's check before the swap and fails after it looks like.
fixture_binary() {
    cat <<EOF
#!/bin/sh
if [ -n "\${HS_FIXTURE_FAIL_IN_PLACE:-}" ]; then
    case "\$0" in */.hollowscript/bin/*) exit 3 ;; esac
fi
echo "hollowscript $1"
EOF
}

# fixture_release SITE VERSION TARGET KEYDIR: SITE/VERSION/ holds the CLI
# archive for TARGET, SHA256SUMS and every signature the tools present can
# make: SHA256SUMS.minisig (minisign) and SHA256SUMS.sshsig (ssh-keygen),
# each for exactly VERSION.
fixture_release() {
    local version="$2" target="$3" keys="$4"
    local bare="${version#v}" rel="$1/$2" stage
    mkdir -p "$rel"
    stage="$(mktemp -d)"
    mkdir -p "$stage/hollowscript-$bare-$target"
    fixture_binary "$bare" >"$stage/hollowscript-$bare-$target/hollowscript"
    fixture_binary "$bare" >"$stage/hollowscript-$bare-$target/hollowscript-lsp"
    chmod 755 "$stage/hollowscript-$bare-$target/hollowscript" "$stage/hollowscript-$bare-$target/hollowscript-lsp"
    (cd "$stage" && tar -czf "$rel/hollowscript-$bare-$target.tar.gz" "hollowscript-$bare-$target")
    rm -rf "$stage"
    printf '%s  %s\n' "$(sha256_file "$rel/hollowscript-$bare-$target.tar.gz")" "hollowscript-$bare-$target.tar.gz" >"$rel/SHA256SUMS"
    if [ "$have_minisign" = 1 ]; then
        minisign -S -l -s "$keys/one.key" -x "$rel/SHA256SUMS.minisig" \
            -t "hollowscript SHA256SUMS, version $version" -m "$rel/SHA256SUMS" </dev/null >/dev/null
    fi
    ssh-keygen -q -Y sign -f "$keys/ssh.key" -n "hollowscript-release-$version" "$rel/SHA256SUMS"
    mv "$rel/SHA256SUMS.sig" "$rel/SHA256SUMS.sshsig"
}
