#!/usr/bin/env bash
# Uploads a signed release (scripts/release-build.sh's output, signed with
# `cargo xtask sign`) to the release bucket get.hollowscript.org redirects to,
# per docs/design/release.md, sections 3 and 8.
#
#   scripts/release-upload.sh <version> --source DIR [--bucket TARGET] [--dry-run]
#                             [--allow-latest-rollback]
#                             [--public-key FILE --allow-unpinned-public-key-for-testing]
#
# TARGET is `gs://<bucket>` (the real bucket, through `gcloud storage`) or a
# plain local directory standing in for it (rehearsal). The real bucket's name
# is one setting, HOLLOWSCRIPT_RELEASE_BUCKET, used when --bucket is not
# given; it is not written into this public script (see
# docs/design/release.md, section 8, for where it is recorded).
#
# What it refuses, before uploading anything:
#   - a SHA256SUMS.minisig that does not verify against the pinned key
#     (scripts/hollowscript-release.pub, the key the installers pin; another
#     key is accepted only with --allow-unpinned-public-key-for-testing, for
#     a rehearsal with a throwaway key);
#   - a SHA256SUMS.sshsig (the same file, signed with the same key in the SSH
#     signature format `ssh-keygen -Y verify` reads, which is what lets a
#     machine without minisign verify the release) that is missing or does
#     not verify in the namespace hollowscript-release-<version>;
#   - a signed trusted comment other than exactly
#     "hollowscript SHA256SUMS, version <version>";
#   - installers that do not pin that same key, or do not agree with each
#     other on the oldest release they accept as the latest, or a stable
#     release older than that floor (latest.txt is not signed; the floor is
#     what stops it being used to steer a fresh install to an old release);
#   - a SHA256SUMS naming any file not built for <version> (every entry must be
#     hollowscript-X.Y.Z-<target>.*, hollowscript-sdk-X.Y.Z-<target>.* or the
#     SBOM), or a file missing from DIR or differing from its signed hash;
#   - for a real bucket, a DIR whose release-build-info.txt is missing, names
#     another version, or records a rehearsal build.
# Only what SHA256SUMS names, plus SHA256SUMS and its signatures, is uploaded:
# never install.sh or install.ps1, which the get.hollowscript.org site serves
# itself, and never a stray file that happens to be in DIR.
#
# Everything is copied from DIR into a private directory first, and verified
# and uploaded from the copy: what is checked is what is sent, even if DIR
# changes while this runs.
#
# Resumable: an object already in the bucket with identical bytes is skipped
# (its Cache-Control and Content-Type are corrected if they differ); one with
# different bytes is refused, never overwritten. Every new object is uploaded
# no-clobber and read back. latest.txt moves last, for a stable version only,
# never backwards without --allow-latest-rollback, and a failure to read the
# current latest.txt is an error, not "there is none".
#
# --dry-run prints every command it would run and runs none of them.
set -euo pipefail

cd "$(dirname "$0")/.."
repo_root="$(pwd)"

step() { printf '\n==> %s\n' "$*"; }
die() {
    printf 'release-upload.sh: %s\n' "$*" >&2
    exit 1
}

usage="usage: scripts/release-upload.sh <version> --source DIR [--bucket gs://BUCKET|DIR] [--dry-run] [--public-key FILE] [--allow-latest-rollback]"
version="${1:-}"
[ -n "$version" ] && [[ "$version" != -* ]] || die "$usage"
shift

source_dir=""
bucket="${HOLLOWSCRIPT_RELEASE_BUCKET:-}"
dry_run=0
pinned_key_file="$repo_root/scripts/hollowscript-release.pub"
public_key="$pinned_key_file"
allow_unpinned_key=0
allow_latest_rollback=0
while [ $# -gt 0 ]; do
    case "$1" in
        --source)
            [ $# -ge 2 ] || die "--source needs a value"
            source_dir="$2"
            shift 2
            ;;
        --bucket)
            [ $# -ge 2 ] || die "--bucket needs a value"
            bucket="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=1
            shift
            ;;
        --public-key)
            [ $# -ge 2 ] || die "--public-key needs a value"
            public_key="$2"
            shift 2
            ;;
        --allow-unpinned-public-key-for-testing)
            allow_unpinned_key=1
            shift
            ;;
        --allow-latest-rollback)
            allow_latest_rollback=1
            shift
            ;;
        *) die "unknown argument: $1 ($usage)" ;;
    esac
done
[ -n "$source_dir" ] || die "--source DIR is required"
[ -d "$source_dir" ] || die "$source_dir does not exist"
[ -n "$bucket" ] || die "no bucket: pass --bucket gs://BUCKET (or a local directory to rehearse against), or set HOLLOWSCRIPT_RELEASE_BUCKET"
[ -f "$public_key" ] || die "$public_key does not exist"
bucket="${bucket%/}"

# The key a release is verified against is the one the installers pin, or the
# upload could publish a release no installer would accept (or, worse, one
# signed by a key somebody else chose). The second line of a minisign public
# key file is the key itself; the comment above it may differ.
key_line() { sed -n '2p' "$1" | tr -d '\r\n'; }
if [ "$(key_line "$public_key")" != "$(key_line "$pinned_key_file")" ]; then
    [ "$allow_unpinned_key" -eq 1 ] \
        || die "$public_key is not the key the installers pin ($pinned_key_file); refusing to verify a release against it (--allow-unpinned-public-key-for-testing is for a rehearsal with a throwaway key, and only with a local directory as the bucket)"
    case "$bucket" in
        gs://*) die "--allow-unpinned-public-key-for-testing cannot be used with a real bucket ($bucket)" ;;
    esac
    printf 'note: verifying against %s, which is NOT the pinned key; for a rehearsal only\n' "$public_key"
fi

is_local_bucket=1
[[ "$bucket" == gs://* ]] && is_local_bucket=0
if [ "$is_local_bucket" -eq 1 ] && [ "$dry_run" -eq 0 ]; then
    [ -d "$bucket" ] || die "$bucket is neither gs://... nor an existing local directory"
fi

# --- the version -----------------------------------------------------------------
case "$version" in
    *[!A-Za-z0-9.-]*) die "'$version' contains a character a version string may not" ;;
esac
[[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$ ]] \
    || die "'$version' does not look like a release version (expected vX.Y.Z or vX.Y.Z-suffix)"
is_stable=1
[[ "$version" == *-* ]] && is_stable=0
bare="${version#v}"

# Numeric sort key for vX.Y.Z, each part forced to base 10 (a leading zero
# must not make it octal).
version_sort_key() {
    local v="${1#v}"
    v="${v%%-*}"
    local IFS=.
    local -a parts
    read -r -a parts <<<"$v"
    printf '%06d%06d%06d' "$((10#${parts[0]:-0}))" "$((10#${parts[1]:-0}))" "$((10#${parts[2]:-0}))"
}

# --- the installers: same key, same floor, and this release is not below it -------------
sh_floor="$(sed -n "s/^release_floor() { printf '%s' '\(v[0-9.]*\)'; }\$/\1/p" "$repo_root/scripts/install.sh")"
ps_floor="$(sed -n "s/^    \$releaseFloor = '\(v[0-9.]*\)'\$/\1/p" "$repo_root/scripts/install.ps1")"
[[ "$sh_floor" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "could not read the release floor from scripts/install.sh"
[ "$sh_floor" = "$ps_floor" ] || die "scripts/install.sh accepts the latest release from '$sh_floor' but scripts/install.ps1 from '$ps_floor'; they must agree"
pinned_line="$(key_line "$pinned_key_file")"
grep -qxF "$pinned_line" "$repo_root/scripts/install.sh" || die "scripts/install.sh does not pin the key in $pinned_key_file"
grep -qF "$pinned_line" "$repo_root/scripts/install.ps1" || die "scripts/install.ps1 does not pin the key in $pinned_key_file"
if [ "$is_stable" -eq 1 ] && [ "$(version_sort_key "$version")" -lt "$(version_sort_key "$sh_floor")" ]; then
    die "$version is older than $sh_floor, the oldest release the installers accept as the latest; a stable release below it would be refused by every default install (raise the floor only forwards, never lower it to publish this)"
fi

# --- where it was built ------------------------------------------------------------
info="$source_dir/release-build-info.txt"
if [ -f "$info" ]; then
    info_version="$(sed -n 's/^version=//p' "$info")"
    [ "$info_version" = "$version" ] || die "$info says this build is $info_version, not $version"
    if grep -qx 'rehearsal=1' "$info"; then
        [ "$is_local_bucket" -eq 1 ] || die "$source_dir is a rehearsal build (release-build.sh --rehearsal); it can be uploaded to a local directory only, never to $bucket"
        printf 'note: %s is a rehearsal build; uploading to a local directory only\n' "$source_dir"
    fi
elif [ "$is_local_bucket" -eq 0 ]; then
    die "$info is missing; a real upload needs the build's own record that it is not a rehearsal"
fi

# --- a private copy: what is verified is what is uploaded ----------------------------
# Everything below reads and uploads from this copy, never from --source
# again, so the source cannot be changed between a check and the upload.
stage="$(mktemp -d)"
chmod 700 "$stage"
trap 'rm -rf "$stage"' EXIT
sums_file="$stage/SHA256SUMS"
sig_file="$stage/SHA256SUMS.minisig"
ssh_sig_file="$stage/SHA256SUMS.sshsig"
for f in SHA256SUMS SHA256SUMS.minisig SHA256SUMS.sshsig; do
    [ -f "$source_dir/$f" ] || die "$source_dir/$f does not exist; refusing to upload a release without it (SHA256SUMS and both signatures are needed)"
    cp "$source_dir/$f" "$stage/$f"
done

# --- the signatures, and the version they were signed for ----------------------------------
command -v minisign >/dev/null 2>&1 || die "minisign is not installed (brew install minisign); it is needed to verify the signature before anything is uploaded"
command -v ssh-keygen >/dev/null 2>&1 || die "ssh-keygen is not installed (OpenSSH 8.0 or newer); it is needed to verify the SSH signature before anything is uploaded"
minisign -V -q -p "$public_key" -m "$sums_file" -x "$sig_file" >/dev/null \
    || die "SHA256SUMS does not verify against $public_key; refusing to upload it"
comment="$(sed -n '3p' "$sig_file")"
[ "$comment" = "trusted comment: hollowscript SHA256SUMS, version $version" ] \
    || die "the signature's trusted comment is '${comment#trusted comment: }', not 'hollowscript SHA256SUMS, version $version'; refusing to upload (sign with cargo xtask sign --version $version)"

# The SSH signature, checked with the same key: its 32 raw bytes, from the
# minisign public key file, become an OpenSSH public key line.
key_bin="$stage/public-key.bin"
key_line "$public_key" | base64 -d >"$key_bin" 2>/dev/null || die "$public_key is not a minisign public key file"
[ "$(wc -c <"$key_bin" | tr -d ' ')" -eq 42 ] && [ "$(head -c 2 "$key_bin")" = Ed ] \
    || die "$public_key is not a minisign Ed25519 public key file"
# The SSH wire form: string "ssh-ed25519" (length 11), then string key (length 32).
blob_b64="$({ printf '\000\000\000\013ssh-ed25519\000\000\000\040'; tail -c 32 "$key_bin"; } | base64 | tr -d '\n')"
printf 'hollowscript-release ssh-ed25519 %s\n' "$blob_b64" >"$stage/allowed_signers"
ssh-keygen -Y verify -f "$stage/allowed_signers" -I hollowscript-release \
    -n "hollowscript-release-$version" -s "$ssh_sig_file" <"$sums_file" >/dev/null 2>&1 \
    || die "SHA256SUMS.sshsig does not verify against $public_key in the namespace hollowscript-release-$version; refusing to upload (sign with cargo xtask sign --version $version)"
step "SHA256SUMS verified against $public_key (minisign and SSH signatures), signed for $version"

# --- every entry: named for this version, present, and byte for byte right ------
names_for_version() {
    case "$1" in
        */* | *\\*) return 1 ;;
        "hollowscript-$bare-sbom."*) return 0 ;;
        "hollowscript-$bare-"aarch64-* | "hollowscript-$bare-"x86_64-* | "hollowscript-$bare-"wasm32-*) return 0 ;;
        "hollowscript-sdk-$bare-"aarch64-* | "hollowscript-sdk-$bare-"x86_64-* | "hollowscript-sdk-$bare-"wasm32-*) return 0 ;;
    esac
    return 1
}
step "checking every SHA256SUMS entry against $source_dir"
objects=()
archives=0
while IFS= read -r line || [ -n "$line" ]; do
    [ -n "$line" ] || continue
    read -r expected_sum name extra <<<"$line"
    [ -z "${extra:-}" ] || die "malformed SHA256SUMS line: $line"
    name="${name#\*}"
    names_for_version "$name" \
        || die "SHA256SUMS names '$name', which is not a file built for $version; refusing to upload"
    [ -f "$source_dir/$name" ] || die "SHA256SUMS names $name but it is not in $source_dir"
    # Copied first, and the copy is what is hashed (and later uploaded).
    cp "$source_dir/$name" "$stage/$name"
    actual_sum="$(shasum -a 256 "$stage/$name" | cut -d' ' -f1)"
    [ "$actual_sum" = "$expected_sum" ] \
        || die "$source_dir/$name does not match its signed sha256 ($expected_sum, got $actual_sum)"
    objects+=("$name")
    case "$name" in *.tar.gz | *.zip) archives=$((archives + 1)) ;; esac
done <"$sums_file"
[ "$archives" -gt 0 ] || die "SHA256SUMS names no archive for $version; refusing to upload a release with no payload"
printf '%d entries (%d archives), all named for %s and matching their signed hashes\n' "${#objects[@]}" "$archives" "$version"

# --- headers -------------------------------------------------------------------------
content_type_for() {
    case "$1" in
        *.tar.gz) printf 'application/gzip' ;;
        *.zip) printf 'application/zip' ;;
        *.json) printf 'application/json' ;;
        SHA256SUMS | *.minisig | *.sshsig | latest.txt) printf 'text/plain; charset=utf-8' ;;
        *) die "no Content-Type for $1" ;;
    esac
}
# A versioned object never changes once published, but it may be withdrawn:
# a day's caching (an owner decision) bounds how long a withdrawn version can
# still be served from a cache, with no CDN in front of the bucket to purge.
versioned_cache_control="public, max-age=86400"
# latest.txt is the one object that changes; keep caches short.
latest_cache_control="public, max-age=60, must-revalidate"

sha_local() { shasum -a 256 "$1" | cut -d' ' -f1; }

# --- the bucket, real or local ----------------------------------------------------------
#
# remote_state PATH prints "absent" or the object's sha256, and fails (never
# prints "absent") if the object's existence cannot be determined.
remote_state() {
    local path="$1"
    if [ "$is_local_bucket" -eq 1 ]; then
        local dest="$bucket/$path"
        if [ -e "$dest" ]; then
            [ -f "$dest" ] && [ -r "$dest" ] || die "$dest exists but cannot be read"
            sha_local "$dest"
        else
            printf 'absent'
        fi
        return 0
    fi
    local err
    err="$(mktemp)"
    if gcloud storage objects describe "$bucket/$path" --format='value(name)' >/dev/null 2>"$err"; then
        rm -f "$err"
        gcloud storage cat "$bucket/$path" | shasum -a 256 | cut -d' ' -f1
        return 0
    fi
    if grep -qiE 'not found|404|No URLs matched' "$err"; then
        rm -f "$err"
        printf 'absent'
        return 0
    fi
    cat "$err" >&2
    rm -f "$err"
    die "could not tell whether $bucket/$path exists (see the error above); stopping rather than guessing"
}

run() {
    if [ "$dry_run" -eq 1 ]; then
        printf '[dry run]'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

# put SRC PATH CACHE_CONTROL NO_CLOBBER
put() {
    local src="$1" path="$2" cache="$3" no_clobber="$4" type
    type="$(content_type_for "$(basename "$path")")"
    if [ "$is_local_bucket" -eq 1 ]; then
        local dest="$bucket/$path"
        if [ "$no_clobber" -eq 1 ] && [ -e "$dest" ]; then
            die "$dest already exists; refusing to overwrite a published object"
        fi
        run mkdir -p "$(dirname "$dest")"
        run cp "$src" "$dest"
        if [ "$dry_run" -eq 0 ]; then
            printf 'Content-Type: %s\nCache-Control: %s\n' "$type" "$cache" >"$dest.metadata"
        fi
    else
        local -a flags=(--content-type="$type" --cache-control="$cache")
        [ "$no_clobber" -eq 1 ] && flags+=(--no-clobber)
        run gcloud storage cp "${flags[@]}" "$src" "$bucket/$path"
    fi
}

# Brings an existing, identical object's headers in line, without touching
# its bytes.
fix_headers() {
    local path="$1" cache="$2" type current
    type="$(content_type_for "$(basename "$path")")"
    if [ "$is_local_bucket" -eq 1 ]; then
        current="$(cat "$bucket/$path.metadata" 2>/dev/null || true)"
        if [ "$current" != "$(printf 'Content-Type: %s\nCache-Control: %s' "$type" "$cache")" ]; then
            run sh -c 'printf "Content-Type: %s\nCache-Control: %s\n" "$1" "$2" >"$3"' _ "$type" "$cache" "$bucket/$path.metadata"
            printf 'corrected the headers of %s\n' "$path"
        fi
    else
        current="$(gcloud storage objects describe "$bucket/$path" --format='value(cache_control,content_type)')" \
            || die "could not read the headers of $bucket/$path"
        if [ "$current" != "$(printf '%s\t%s' "$cache" "$type")" ]; then
            run gcloud storage objects update "$bucket/$path" --cache-control="$cache" --content-type="$type"
            printf 'corrected the headers of %s\n' "$path"
        fi
    fi
}

# --- upload: the payload first, then SHA256SUMS, its signature last --------------------
# An installer needs SHA256SUMS and the signature; publishing them last means
# a half-finished upload is one no installer can install from.
step "uploading $version to $bucket"
uploaded=0
skipped=0
for name in "${objects[@]}" SHA256SUMS SHA256SUMS.sshsig SHA256SUMS.minisig; do
    src="$stage/$name"
    path="$version/$name"
    want="$(sha_local "$src")"
    if [ "$dry_run" -eq 1 ]; then
        put "$src" "$path" "$versioned_cache_control" 1
        continue
    fi
    have="$(remote_state "$path")"
    if [ "$have" = "$want" ]; then
        printf 'already uploaded, identical: %s\n' "$path"
        fix_headers "$path" "$versioned_cache_control"
        skipped=$((skipped + 1))
        continue
    fi
    [ "$have" = absent ] || die "$bucket/$path already exists with different contents (sha256 $have, this release has $want); refusing to overwrite a published object"
    put "$src" "$path" "$versioned_cache_control" 1
    back="$(remote_state "$path")"
    [ "$back" = "$want" ] || die "read $bucket/$path back after uploading it and it does not match (sha256 $back, expected $want)"
    printf 'uploaded and read back: %s\n' "$path"
    uploaded=$((uploaded + 1))
done
[ "$dry_run" -eq 1 ] || printf '%d uploaded, %d already there and identical\n' "$uploaded" "$skipped"

# --- latest.txt: stable only, last, never backwards -----------------------------------
if [ "$is_stable" -eq 0 ]; then
    step "prerelease: latest.txt is not touched"
else
    step "latest.txt"
    current_latest=""
    if [ "$dry_run" -eq 0 ]; then
        state="$(remote_state latest.txt)"
        if [ "$state" != absent ]; then
            if [ "$is_local_bucket" -eq 1 ]; then
                current_latest="$(tr -d ' \t\r\n' <"$bucket/latest.txt")" || die "could not read $bucket/latest.txt"
            else
                current_latest="$(gcloud storage cat "$bucket/latest.txt" | tr -d ' \t\r\n')" || die "could not read $bucket/latest.txt"
            fi
            [[ "$current_latest" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "the bucket's latest.txt holds '$current_latest', not a stable version; fix it by hand before moving it"
        fi
    fi
    if [ "$current_latest" = "$version" ]; then
        printf 'latest.txt already names %s\n' "$version"
        fix_headers latest.txt "$latest_cache_control"
    else
        if [ -n "$current_latest" ] && [ "$(version_sort_key "$version")" -lt "$(version_sort_key "$current_latest")" ]; then
            [ "$allow_latest_rollback" -eq 1 ] \
                || die "latest.txt names $current_latest, newer than $version; refusing to move it backwards without --allow-latest-rollback"
            printf 'moving latest.txt back from %s: --allow-latest-rollback was given\n' "$current_latest"
        fi
        latest_src="$(mktemp)"
        printf '%s\n' "$version" >"$latest_src"
        put "$latest_src" latest.txt "$latest_cache_control" 0
        rm -f "$latest_src"
        if [ "$dry_run" -eq 0 ]; then
            printf 'latest.txt now names %s\n' "$version"
        fi
    fi
fi

if [ "$dry_run" -eq 1 ]; then
    printf '\ndry run: nothing was read from or written to %s.\n' "$bucket"
fi
