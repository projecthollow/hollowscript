#!/bin/sh
# Installs the HollowScript command line tool (hollowscript, and the
# hollowscript-lsp language server it starts) on macOS and Linux.
#
# POSIX sh: this runs as `curl ... | sh` under whatever the system's own
# `sh` is (dash, bash, busybox ash, zsh in sh mode), so nothing here assumes
# bash. Everything is inside functions, and the only statement run at the top
# level is the call to `main` on the very last line: a download cut off part
# way through defines some functions and runs nothing.
#
# Run `sh install.sh --help` (or `curl -fsSL https://get.hollowscript.org/install.sh
# | sh -s -- --help`) for usage. docs/design/release.md, section 5, is the
# design.

usage() {
    cat <<'EOF'
Installs the HollowScript command line tool.

Usage:
  curl -fsSL https://get.hollowscript.org/install.sh | sh
  curl -fsSL https://get.hollowscript.org/install.sh | sh -s -- [options]
  sh install.sh [options]

Options:
  --version vX.Y.Z   install this release instead of the latest one
  --no-modify-path   do not add HollowScript to PATH in your shell profiles
  --uninstall        remove HollowScript and the PATH line this installer added
  -h, --help         show this text

Environment variables (all optional; the options above take precedence):
  HOLLOWSCRIPT_VERSION          a release to install, for example v0.1.0.
                                Set it for the `sh` that runs the script:
                                curl ... | HOLLOWSCRIPT_VERSION=v0.1.0 sh
  HOLLOWSCRIPT_HOME             where to install. Default: ~/.hollowscript
                                (binaries in its bin directory).
  HOLLOWSCRIPT_NO_MODIFY_PATH   set to 1 to behave as --no-modify-path.
  HOLLOWSCRIPT_BASE_URL         where releases are served from. Default:
                                https://get.hollowscript.org. Must be https://
                                (plain http:// is accepted for 127.0.0.1,
                                localhost and [::1] only, for local testing).
  HOLLOWSCRIPT_PUBLIC_KEY_FILE  a minisign public key trusted in addition to
                                (never instead of) the key built into this
                                script, for a key rotation.
  HOLLOWSCRIPT_ALLOW_UNSIGNED   set to exactly 1 to install on the strength of
                                SHA256SUMS alone when this machine has no
                                signature verifier (see below). Any other
                                value but 0 is refused.
  HOLLOWSCRIPT_REQUIRE_SIGNATURE  set to 1, true or yes to refuse to install
                                without a verified signature even if
                                HOLLOWSCRIPT_ALLOW_UNSIGNED is also set.
  HOLLOWSCRIPT_ALLOW_DOWNGRADE  set to 1 to let the latest release replace a
                                newer installed one. An explicit version is
                                always installed as asked.
  HOLLOWSCRIPT_TARGET           install the build for this target instead of
                                the detected one (for example
                                x86_64-apple-darwin under Rosetta).

Verification: every download is checked against SHA256SUMS. SHA256SUMS
itself is signed, and the signature, and the version it was signed for, are
verified before anything is installed, with the first of these this machine
has: minisign, an openssl that supports Ed25519, or ssh-keygen from OpenSSH
8.0 or newer (on macOS and most Linux installs, and Windows 10 and 11 with
the OpenSSH client). If there is none, the install is refused, because a
SHA256SUMS fetched from the server the archive came from proves nothing
about that server. Install a verifier (brew install minisign, apt install
minisign or openssh-client, apk add minisign or openssh-keygen), or accept
the risk for one install with HOLLOWSCRIPT_ALLOW_UNSIGNED=1. BusyBox wget
does not validate certificates, so without curl it is used only when a
verifier is available.

PATH: the binaries go in ~/.hollowscript/bin, and one line that sources
~/.hollowscript/env is added to ~/.profile, and to ~/.bashrc,
~/.bash_profile and ~/.zshenv where those shells are in use (a file in
fish's conf.d for fish). Running the installer again adds nothing twice;
--uninstall removes those lines and ~/.hollowscript.
EOF
}

# The minisign public key releases are signed with. Kept byte-identical to
# scripts/hollowscript-release.pub (which release-upload.sh verifies against)
# and to the copy in install.ps1. A key rotation changes it here, in the same
# change; it is never fetched from the server this script downloads from,
# since whoever controls that server could then choose the key too.
pinned_public_key() {
    cat <<'EOF'
untrusted comment: minisign public key for HollowScript releases
RWTEd/MMbolYkjq8slz9NEko74kUDEwMdcjcsNLSY68z9K1MZpq0y4mK
EOF
}

say() {
    printf 'install.sh: %s\n' "$1" >&2
}

die() {
    printf 'install.sh: error: %s\n' "$1" >&2
    exit 1
}

have() {
    command -v "$1" >/dev/null 2>&1
}

# env_flag NAME VALUE: succeeds if VALUE, the setting of the environment
# variable NAME, is 1, true or yes (in any case), fails if it is empty, 0,
# false or no, and stops the install on anything else: a setting that
# changes what is trusted must not be silently ignored because of a typo.
env_flag() {
    ef_lower="$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')"
    case "$ef_lower" in
        1 | true | yes) return 0 ;;
        '' | 0 | false | no) return 1 ;;
        *) die "$1 must be 1, true or yes (or 0, false or no), not '$2'" ;;
    esac
}

# The oldest release an install that asked for "the latest" will accept.
# latest.txt is not signed, so whoever can write to the release bucket could
# otherwise point a fresh install at any older, validly signed release. This
# floor is part of this script, which is served from a different place than
# the bucket and carries the pinned key, so the bucket alone cannot lower it.
# It is raised, in install.ps1 as well, in the change that releases a security
# fix; scripts/release-upload.sh refuses to move latest.txt below it.
release_floor() { printf '%s' 'v0.1.0'; }

# --- downloading ----------------------------------------------------------------

# Succeeds if the download would go through BusyBox's wget (no curl, and the
# wget found is BusyBox's). BusyBox wget does not validate TLS certificates
# (it says "TLS certificate validation not implemented" itself), so it can
# fetch anything a network attacker serves; a signature verifier makes that
# harmless, and without one it is not acceptable.
fetch_is_busybox_wget() {
    have curl && return 1
    have wget || return 1
    wget --help 2>&1 | head -n 1 | grep -qi busybox
}

# fetch URL OUT: curl if present (https only, TLS 1.2 or newer, a failing
# HTTP status is an error), else wget.
fetch() {
    if have curl; then
        if [ "$secure_transport" = 1 ]; then
            curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$2" "$1"
        else
            curl -fsSL --retry 3 -o "$2" "$1"
        fi
    elif have wget; then
        wget -q -O "$2" "$1"
    else
        die "neither curl nor wget is available to download with"
    fi
}

# --- versions -------------------------------------------------------------------

# Succeeds if $1 is vX.Y.Z or vX.Y.Z-suffix, with only letters, digits, '.'
# and '-'. The charset is checked first: it is what makes the version safe to
# put in a URL. The shape is checked per component, not with a glob, since a
# glob's `*` would accept `v1x.2y.3z`.
version_ok() {
    case "$1" in
        *[!A-Za-z0-9.-]* | '') return 1 ;;
        v*) : ;;
        *) return 1 ;;
    esac
    vo_core="${1#v}"
    vo_suffix=""
    case "$vo_core" in
        *-*)
            vo_suffix="${vo_core#*-}"
            vo_core="${vo_core%%-*}"
            [ -n "$vo_suffix" ] || return 1
            case "$vo_suffix" in *-*) return 1 ;; esac
            ;;
    esac
    case "$vo_core" in
        *.*.*.*) return 1 ;;
        *.*.*) : ;;
        *) return 1 ;;
    esac
    vo_major="${vo_core%%.*}"
    vo_rest="${vo_core#*.}"
    vo_minor="${vo_rest%%.*}"
    vo_patch="${vo_rest#*.}"
    for vo_part in "$vo_major" "$vo_minor" "$vo_patch"; do
        case "$vo_part" in
            '' | *[!0-9]*) return 1 ;;
        esac
    done
    return 0
}

# A decimal number without leading zeros, so `[ -lt ]` and printf never read
# it as octal (08 and 09 are not valid octal).
decimal() {
    dec_n="$1"
    while :; do
        case "$dec_n" in
            0?*) dec_n="${dec_n#0}" ;;
            *) break ;;
        esac
    done
    printf '%s' "${dec_n:-0}"
}

# Succeeds if version $1 is older than version $2 (either with or without a
# leading v). X.Y.Z is compared numerically; for an equal X.Y.Z, a prerelease
# is older than the release itself.
version_older() {
    vl_a="${1#v}"
    vl_b="${2#v}"
    vl_a_pre=""
    vl_b_pre=""
    case "$vl_a" in *-*) vl_a_pre=1 ;; esac
    case "$vl_b" in *-*) vl_b_pre=1 ;; esac
    vl_a="${vl_a%%-*}"
    vl_b="${vl_b%%-*}"
    for vl_i in 1 2 3; do
        vl_x="$(decimal "${vl_a%%.*}")"
        vl_y="$(decimal "${vl_b%%.*}")"
        [ "$vl_x" -lt "$vl_y" ] && return 0
        [ "$vl_x" -gt "$vl_y" ] && return 1
        case "$vl_a" in *.*) vl_a="${vl_a#*.}" ;; *) vl_a=0 ;; esac
        case "$vl_b" in *.*) vl_b="${vl_b#*.}" ;; *) vl_b=0 ;; esac
        : "$vl_i"
    done
    [ -n "$vl_a_pre" ] && [ -z "$vl_b_pre" ] && return 0
    return 1
}

# --- hashing and signatures -------------------------------------------------------

sha256_of() {
    if have sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    elif have shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif have openssl; then
        openssl dgst -sha256 -r "$1" | awk '{print $1}'
    else
        die "no SHA-256 tool is available (sha256sum, shasum or openssl)"
    fi
}

# Writes the bytes a hex string names to standard output, one printf per
# byte: a single format string holding every byte would stop at the first
# zero byte in some shells' printf.
hex_to_bin() {
    htb_hex="$1"
    while [ -n "$htb_hex" ]; do
        htb_pair="${htb_hex%"${htb_hex#??}"}"
        htb_hex="${htb_hex#??}"
        # shellcheck disable=SC2059 # the format is one octal escape
        printf "\\$(printf '%03o' "0x$htb_pair")"
    done
}

# Bytes $2 to $2+$3 of file $1, as lower-case hex.
bytes_hex() {
    dd if="$1" bs=1 skip="$2" count="$3" 2>/dev/null | od -An -tx1 | tr -d ' \n'
}

# Bytes $2 to $2+$3 of file $1, into file $4.
bytes_to() {
    dd if="$1" bs=1 skip="$2" count="$3" of="$4" 2>/dev/null
}

# Wraps a raw 32-byte Ed25519 public key ($1) as a PEM file ($2) openssl can
# read (an RFC 8410 SubjectPublicKeyInfo). Fails if this openssl cannot.
ed25519_pem() {
    hex_to_bin 302a300506032b6570032100 >"$2.der"
    cat "$1" >>"$2.der"
    openssl pkey -pubin -inform DER -in "$2.der" -outform PEM -out "$2" >/dev/null 2>&1
}

# openssl_verify PEM MESSAGE SIGNATURE
openssl_verify() {
    openssl pkeyutl -verify -pubin -inkey "$1" -rawin -in "$2" -sigfile "$3" >/dev/null 2>&1
}

# Succeeds if this machine's openssl can verify Ed25519 at all, proved on the
# second test vector of RFC 8032 (section 7.1, the one byte message 0x72)
# rather than guessed from `openssl version`: LibreSSL, macOS's
# /usr/bin/openssl, cannot, and OpenSSL before 3.0 lacks `pkeyutl -rawin`.
openssl_can_verify_ed25519() {
    have openssl || return 1
    oc_dir="$work_dir/selftest"
    mkdir -p "$oc_dir"
    hex_to_bin 3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c >"$oc_dir/pk"
    hex_to_bin 92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00 >"$oc_dir/sig"
    hex_to_bin 72 >"$oc_dir/msg"
    ed25519_pem "$oc_dir/pk" "$oc_dir/key.pem" || return 1
    openssl_verify "$oc_dir/key.pem" "$oc_dir/msg" "$oc_dir/sig" || return 1
    # And a wrong signature must fail, or the check above proves nothing.
    printf 'x' >"$oc_dir/msg"
    if openssl_verify "$oc_dir/key.pem" "$oc_dir/msg" "$oc_dir/sig"; then
        return 1
    fi
    return 0
}

# Base64 on standard input to standard output, with whatever this machine
# has: openssl (present wherever minisign's own format is read below), else
# base64.
b64_decode() {
    if have openssl; then
        openssl base64 -d -A 2>/dev/null
    elif have base64; then
        base64 -d 2>/dev/null
    else
        return 1
    fi
}

b64_encode() {
    if have openssl; then
        openssl base64 -A 2>/dev/null
    elif have base64; then
        base64 2>/dev/null | tr -d '\n'
    else
        return 1
    fi
}

# base64 line $2 of file $1, decoded into file $3.
b64_line_to() {
    sed -n "${2}p" "$1" | tr -d '\r\n' | b64_decode >"$3"
}

# Verifies a minisign signature with openssl, the way minisign itself does:
# the key ids must match, the signature over the file must verify, and the
# global signature over signature || trusted comment must verify too, so an
# edited trusted comment is refused. Only minisign's legacy "Ed" format
# (what `cargo xtask sign` writes) is accepted here.
# verify_with_openssl KEY_FILE MESSAGE SIG_FILE
verify_with_openssl() {
    vo_dir="$work_dir/openssl"
    rm -rf "$vo_dir"
    mkdir -p "$vo_dir"
    b64_line_to "$1" 2 "$vo_dir/key.bin" || return 1
    b64_line_to "$3" 2 "$vo_dir/sig.bin" || return 1
    b64_line_to "$3" 4 "$vo_dir/global.bin" || return 1
    [ "$(wc -c <"$vo_dir/key.bin" | tr -d ' ')" = 42 ] || return 1
    [ "$(wc -c <"$vo_dir/sig.bin" | tr -d ' ')" = 74 ] || return 1
    [ "$(wc -c <"$vo_dir/global.bin" | tr -d ' ')" = 64 ] || return 1
    # "Ed" is 45 64.
    [ "$(bytes_hex "$vo_dir/key.bin" 0 2)" = 4564 ] || return 1
    [ "$(bytes_hex "$vo_dir/sig.bin" 0 2)" = 4564 ] || return 1
    [ "$(bytes_hex "$vo_dir/key.bin" 2 8)" = "$(bytes_hex "$vo_dir/sig.bin" 2 8)" ] || return 1
    bytes_to "$vo_dir/key.bin" 10 32 "$vo_dir/pk"
    bytes_to "$vo_dir/sig.bin" 10 64 "$vo_dir/sig"
    ed25519_pem "$vo_dir/pk" "$vo_dir/key.pem" || return 1
    openssl_verify "$vo_dir/key.pem" "$2" "$vo_dir/sig" || return 1
    vo_comment="$(sed -n '3p' "$3" | tr -d '\r')"
    case "$vo_comment" in
        'trusted comment: '*) vo_comment="${vo_comment#trusted comment: }" ;;
        *) return 1 ;;
    esac
    cp "$vo_dir/sig" "$vo_dir/global.msg"
    printf '%s' "$vo_comment" >>"$vo_dir/global.msg"
    openssl_verify "$vo_dir/key.pem" "$vo_dir/global.msg" "$vo_dir/global.bin" || return 1
    return 0
}

# verify_with_minisign KEY_FILE MESSAGE SIG_FILE
verify_with_minisign() {
    minisign -V -q -p "$1" -m "$2" -x "$3" >/dev/null 2>&1
}

# Succeeds if this machine has an ssh-keygen that can verify signatures
# (`-Y verify`, OpenSSH 8.0 or newer): asked with a bare -Y, which an older
# one rejects as an unknown option, and a newer one answers with a usage that
# lists the verify form.
ssh_keygen_can_verify() {
    have ssh-keygen || return 1
    ssh-keygen -Y 2>&1 | grep -q 'Y verify'
}

# Verifies an SSH signature (the armoured file `cargo xtask sign` writes next
# to the minisign one, made with the same key) with `ssh-keygen -Y verify`.
# The key is read from a minisign public key file, so there is one pinned key
# and no second copy of it to fall out of step: its 32 raw bytes become an
# OpenSSH public key line. The signature is made in the namespace
# hollowscript-release-VERSION and verified in exactly that one, which is what
# ties it to the version being installed, as the trusted comment does for the
# minisign signature.
# verify_with_sshkeygen KEY_FILE MESSAGE SIG_FILE
verify_with_sshkeygen() {
    vs_dir="$work_dir/sshkeygen"
    rm -rf "$vs_dir"
    mkdir -p "$vs_dir"
    b64_line_to "$1" 2 "$vs_dir/key.bin" || return 1
    [ "$(wc -c <"$vs_dir/key.bin" | tr -d ' ')" = 42 ] || return 1
    [ "$(bytes_hex "$vs_dir/key.bin" 0 2)" = 4564 ] || return 1
    bytes_to "$vs_dir/key.bin" 10 32 "$vs_dir/pk"
    # The SSH wire form of the key: string "ssh-ed25519", then string key.
    {
        hex_to_bin 0000000b7373682d6564323535313900000020
        cat "$vs_dir/pk"
    } >"$vs_dir/blob"
    vs_b64="$(b64_encode <"$vs_dir/blob")" || return 1
    [ -n "$vs_b64" ] || return 1
    printf 'hollowscript-release ssh-ed25519 %s\n' "$vs_b64" >"$vs_dir/allowed_signers"
    ssh-keygen -Y verify -f "$vs_dir/allowed_signers" -I hollowscript-release \
        -n "hollowscript-release-$version" -s "$3" <"$2" >/dev/null 2>&1
}

# --- PATH -------------------------------------------------------------------------

# $1 quoted for a POSIX shell: single quotes, with any single quote in it
# closed, escaped and reopened.
sh_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

# $1 quoted for fish: single quotes, with \ and ' escaped.
fish_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e "s/'/\\\\'/g")"
}

# The one line added to each sh-family profile. For the default location it
# is written with $HOME, the way it reads best; otherwise with the path.
profile_line() {
    if [ "$hs_home" = "$HOME/.hollowscript" ]; then
        # shellcheck disable=SC2016 # $HOME is for the profile to expand
        printf '%s' '. "$HOME/.hollowscript/env"'
    else
        printf '. %s' "$(sh_quote "$hs_home/env")"
    fi
}

fish_line() {
    printf 'source %s' "$(fish_quote "$hs_home/env.fish")"
}

fish_conf() {
    printf '%s/fish/conf.d/hollowscript.fish' "${XDG_CONFIG_HOME:-$HOME/.config}"
}

# The sh-family profiles this installer writes to: ~/.profile always, and
# the bash and zsh startup files where those shells are in use.
profile_files() {
    printf '%s\n' "$HOME/.profile"
    [ -f "$HOME/.bashrc" ] && printf '%s\n' "$HOME/.bashrc"
    [ -f "$HOME/.bash_profile" ] && printf '%s\n' "$HOME/.bash_profile"
    pf_zdot="${ZDOTDIR:-$HOME}"
    if [ -f "$pf_zdot/.zshenv" ] || have zsh || case "${SHELL:-}" in */zsh) true ;; *) false ;; esac; then
        printf '%s\n' "$pf_zdot/.zshenv"
    fi
    return 0
}

write_env_files() {
    we_bin_sh="$(sh_quote "$hs_bin")"
    cat >"$hs_home/env" <<EOF
# Added by the HollowScript installer: puts hollowscript on PATH.
case ":\${PATH}:" in
    *:$we_bin_sh:*) ;;
    *) export PATH=$we_bin_sh":\$PATH" ;;
esac
EOF
    we_bin_fish="$(fish_quote "$hs_bin")"
    cat >"$hs_home/env.fish" <<EOF
# Added by the HollowScript installer: puts hollowscript on PATH.
if not contains -- $we_bin_fish \$PATH
    set -gx PATH $we_bin_fish \$PATH
end
EOF
}

# Appends line $2 to file $1 unless it is already there, exactly.
add_line() {
    if [ -f "$1" ] && grep -qxF -- "$2" "$1"; then
        return 0
    fi
    mkdir -p "$(dirname "$1")"
    # Start on a line of its own, even if the file does not end in a newline.
    if [ -s "$1" ] && [ "$(tail -c 1 "$1" | od -An -tx1 | tr -d ' \n')" != 0a ]; then
        printf '\n' >>"$1"
    fi
    printf '%s\n' "$2" >>"$1"
    say "added to $1: $2"
}

# Removes every line of file $1 that is exactly $2, keeping the file itself
# (its permissions, and a symlink stays a symlink) by writing back into it.
remove_line() {
    [ -f "$1" ] || return 0
    grep -qxF -- "$2" "$1" || return 0
    rl_tmp="$1.hollowscript-tmp.$$"
    grep -vxF -- "$2" "$1" >"$rl_tmp" || true
    cat "$rl_tmp" >"$1"
    rm -f "$rl_tmp"
    say "removed from $1: $2"
}

modify_path() {
    mp_line="$(profile_line)"
    profile_files >"$work_dir/profiles"
    while IFS= read -r mp_file; do
        add_line "$mp_file" "$mp_line"
    done <"$work_dir/profiles"
    if have fish || [ -d "${XDG_CONFIG_HOME:-$HOME/.config}/fish" ]; then
        add_line "$(fish_conf)" "$(fish_line)"
    fi
}

unmodify_path() {
    um_line="$(profile_line)"
    # Every file modify_path could have written, whether or not the shell
    # is still in use, so an uninstall cleans up what an install did.
    for um_file in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.bash_profile" "${ZDOTDIR:-$HOME}/.zshenv"; do
        remove_line "$um_file" "$um_line"
    done
    um_fish="$(fish_conf)"
    if [ -f "$um_fish" ]; then
        remove_line "$um_fish" "$(fish_line)"
        [ -s "$um_fish" ] || rm -f "$um_fish"
    fi
}

# --- install state, and cleaning up after a failure or an interrupt ---------------

# What cleanup has to undo: "" (nothing on disk yet), "staged" (new binaries
# written next to the old ones as *.new), "swapping" (some binaries may
# already be replaced; the previous ones are in .*.previous).
install_state=""

cleanup() {
    if [ -n "$install_state" ]; then
        for cl_name in hollowscript hollowscript-lsp; do
            rm -f "$hs_bin/$cl_name.new"
            if [ "$install_state" = swapping ]; then
                if [ -f "$hs_bin/.$cl_name.previous" ]; then
                    mv -f "$hs_bin/.$cl_name.previous" "$hs_bin/$cl_name"
                    say "restored the previous $cl_name"
                elif [ -e "$hs_bin/$cl_name" ]; then
                    # A fresh install: there was nothing before it, so the
                    # only thing to restore is that nothing. Leaving the new
                    # binary would leave one that has just failed its check.
                    rm -f "$hs_bin/$cl_name"
                    say "removed the new $cl_name, which was not in place before"
                fi
            fi
            # Before swapping, a backup is only a copy of what is still in
            # place, so it goes too.
            rm -f "$hs_bin/.$cl_name.previous"
        done
        # Only if nothing is left in it: a first install that failed leaves
        # no empty directory behind.
        rmdir "$hs_bin" 2>/dev/null || true
        install_state=""
    fi
    if [ -n "${work_dir:-}" ] && [ -d "$work_dir" ]; then
        rm -rf "$work_dir"
    fi
}

on_signal() {
    # Cleanup first, with SIGPIPE ignored: when the signal is SIGPIPE
    # (standard error went away, say `| head`), writing a message first
    # would only raise it again, before anything was cleaned up.
    trap - EXIT INT TERM HUP
    trap '' PIPE
    cleanup
    say "interrupted; cleaned up" 2>/dev/null || true
    exit 130
}

# --- the steps --------------------------------------------------------------------

detect_target() {
    if [ -n "${HOLLOWSCRIPT_TARGET:-}" ]; then
        case "$HOLLOWSCRIPT_TARGET" in
            aarch64-apple-darwin | x86_64-apple-darwin | x86_64-unknown-linux-musl | aarch64-unknown-linux-musl)
                printf '%s' "$HOLLOWSCRIPT_TARGET"
                return 0
                ;;
            *) die "HOLLOWSCRIPT_TARGET '$HOLLOWSCRIPT_TARGET' is not a target this installer installs (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-musl, aarch64-unknown-linux-musl)" ;;
        esac
    fi
    dt_os="$(uname -s)"
    dt_machine="$(uname -m)"
    case "$dt_os" in
        Darwin) dt_os_id="apple-darwin" ;;
        # One static musl build serves every Linux distribution, glibc or
        # musl alike, so there is no libc to detect.
        Linux) dt_os_id="unknown-linux-musl" ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT) die "on Windows, use install.ps1: irm https://get.hollowscript.org/install.ps1 | iex" ;;
        *) die "unsupported operating system: $dt_os" ;;
    esac
    # Under Rosetta, uname -m says x86_64 on Apple Silicon; the native arm64
    # build is the one to install.
    # sysctl lives in /usr/sbin, which a minimal PATH (/usr/bin:/bin) lacks.
    dt_sysctl="$(command -v sysctl 2>/dev/null || echo /usr/sbin/sysctl)"
    if [ "$dt_os" = Darwin ] && [ "$dt_machine" = x86_64 ] \
        && [ "$("$dt_sysctl" -in sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
        dt_machine=arm64
        say "running under Rosetta on Apple Silicon; installing the native arm64 build (set HOLLOWSCRIPT_TARGET=x86_64-apple-darwin for the Intel one)"
    fi
    case "$dt_machine" in
        x86_64 | amd64) dt_arch=x86_64 ;;
        arm64 | aarch64) dt_arch=aarch64 ;;
        *) die "unsupported architecture: $dt_machine" ;;
    esac
    printf '%s-%s' "$dt_arch" "$dt_os_id"
}

do_uninstall() {
    unmodify_path
    for un_file in "$hs_bin/hollowscript" "$hs_bin/hollowscript-lsp" \
        "$hs_bin/.hollowscript.previous" "$hs_bin/.hollowscript-lsp.previous" \
        "$hs_bin/hollowscript.new" "$hs_bin/hollowscript-lsp.new" \
        "$hs_home/env" "$hs_home/env.fish"; do
        if [ -e "$un_file" ] || [ -L "$un_file" ]; then
            rm -f "$un_file"
            say "removed $un_file"
        fi
    done
    rmdir "$hs_bin" 2>/dev/null || true
    rmdir "$hs_home" 2>/dev/null || true
    if [ -d "$hs_home" ]; then
        say "left $hs_home in place: it holds files this installer did not put there"
    fi
    say "uninstalled. Open a new shell for the PATH change to take effect."
}

# check_base_url URL: stops the install unless URL is https://, or http://
# for exactly 127.0.0.1, localhost or [::1] (with an optional all-digit
# port), and sets secure_transport to 1 or 0. The host is the authority up to
# the first / ? or #, and an authority holding a @ is refused outright: a
# prefix glob would take http://127.0.0.1:1@example.org, whose host is
# example.org.
check_base_url() {
    cb_url="$1"
    case "$cb_url" in
        *[[:space:]\\]* | *[?#]*) die "HOLLOWSCRIPT_BASE_URL '$cb_url' is not a plain URL (no spaces, backslashes, query or fragment); refusing to download from it" ;;
        https://* | http://*) : ;;
        *) die "HOLLOWSCRIPT_BASE_URL '$cb_url' is not https:// (plain http:// is accepted only for 127.0.0.1, localhost and [::1]); refusing to download over an insecure channel" ;;
    esac
    cb_rest="${cb_url#*://}"
    cb_authority="${cb_rest%%/*}"
    case "$cb_authority" in
        '' | *@*) die "HOLLOWSCRIPT_BASE_URL '$cb_url' has no host, or has a user name in it; refusing to use it" ;;
    esac
    case "$cb_url" in
        https://*) return 0 ;;
    esac
    cb_port=""
    case "$cb_authority" in
        127.0.0.1 | localhost | '[::1]') : ;;
        127.0.0.1:* | localhost:*) cb_port="${cb_authority#*:}" ;;
        '[::1]:'*) cb_port="${cb_authority#'[::1]:'}" ;;
        *) die "HOLLOWSCRIPT_BASE_URL '$cb_url' is http:// to a host other than 127.0.0.1, localhost or [::1]; refusing to download over an insecure channel" ;;
    esac
    case "$cb_port" in
        *[!0-9]*) die "HOLLOWSCRIPT_BASE_URL '$cb_url' has a port that is not a number; refusing to use it" ;;
    esac
    secure_transport=0
}

main() {
    set -eu

    action=install
    version="${HOLLOWSCRIPT_VERSION:-}"
    modify_path_ok=1
    env_flag HOLLOWSCRIPT_NO_MODIFY_PATH "${HOLLOWSCRIPT_NO_MODIFY_PATH:-}" && modify_path_ok=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --uninstall) action=uninstall ;;
            --no-modify-path) modify_path_ok=0 ;;
            --version)
                [ $# -ge 2 ] || die "--version needs a value, for example --version v0.1.0"
                version="$2"
                shift
                ;;
            --version=*) version="${1#--version=}" ;;
            -h | --help)
                usage
                return 0
                ;;
            *) die "unknown argument: $1 (try --help)" ;;
        esac
        shift
    done

    [ -n "${HOME:-}" ] || die "HOME is not set"
    hs_home="${HOLLOWSCRIPT_HOME:-$HOME/.hollowscript}"
    case "$hs_home" in
        /*) : ;;
        *) die "HOLLOWSCRIPT_HOME must be an absolute path, not '$hs_home'" ;;
    esac
    hs_home="${hs_home%/}"
    [ -n "$hs_home" ] && [ "$hs_home" != "$HOME" ] || die "HOLLOWSCRIPT_HOME must be a directory of its own, not / or your home directory"
    hs_bin="$hs_home/bin"

    if [ "$action" = uninstall ]; then
        do_uninstall
        return 0
    fi

    # Settings that change what is trusted. Read, and any unrecognised value
    # refused, before anything is downloaded.
    allow_unsigned=0
    case "${HOLLOWSCRIPT_ALLOW_UNSIGNED:-}" in
        '' | 0) : ;;
        1) allow_unsigned=1 ;;
        *) die "HOLLOWSCRIPT_ALLOW_UNSIGNED is accepted only as exactly 1 (or 0), not '$HOLLOWSCRIPT_ALLOW_UNSIGNED'" ;;
    esac
    require_signature=0
    env_flag HOLLOWSCRIPT_REQUIRE_SIGNATURE "${HOLLOWSCRIPT_REQUIRE_SIGNATURE:-}" && require_signature=1
    allow_downgrade=0
    env_flag HOLLOWSCRIPT_ALLOW_DOWNGRADE "${HOLLOWSCRIPT_ALLOW_DOWNGRADE:-}" && allow_downgrade=1

    base_url="${HOLLOWSCRIPT_BASE_URL:-https://get.hollowscript.org}"
    base_url="${base_url%/}"
    secure_transport=1
    check_base_url "$base_url"

    target="$(detect_target)"

    trap cleanup EXIT
    trap on_signal INT TERM HUP PIPE
    work_dir="$(mktemp -d 2>/dev/null || mktemp -d -t hollowscript-install)" || die "could not create a temporary directory"

    # --- what can check a signature here? Settled before anything is fetched ---
    verifier=""
    if have minisign; then
        verifier=minisign
    elif openssl_can_verify_ed25519; then
        verifier=openssl
    elif ssh_keygen_can_verify; then
        verifier=sshkeygen
    fi
    if [ -z "$verifier" ]; then
        # Fail closed. SHA256SUMS comes from the same server as the archive,
        # so matching it proves nothing about that server.
        say "no signature verifier is available on this machine: there is no minisign, no openssl"
        say "  that supports Ed25519 (macOS's own is LibreSSL, which does not) and no ssh-keygen from"
        say "  OpenSSH 8.0 or newer. Without one, a release cannot be told from a forgery."
        say "  Install one and run this installer again: minisign (brew install minisign, apt install"
        say "  minisign, apk add minisign, scoop install minisign), or OpenSSH's ssh-keygen (apt install"
        say "  openssh-client, apk add openssh-keygen)."
        if [ "$require_signature" = 1 ]; then
            die "no signature verifier is available, and HOLLOWSCRIPT_REQUIRE_SIGNATURE is set; refusing to install"
        fi
        if [ "$allow_unsigned" != 1 ]; then
            die "no signature verifier is available; refusing to install an unverified release (to install on the strength of SHA256SUMS alone, knowing a compromised $base_url defeats it, set HOLLOWSCRIPT_ALLOW_UNSIGNED=1)"
        fi
        if fetch_is_busybox_wget; then
            die "no signature verifier is available and the only downloader is BusyBox wget, which does not validate TLS certificates; install curl or a signature verifier (see above). HOLLOWSCRIPT_ALLOW_UNSIGNED does not cover this"
        fi
        say "WARNING: HOLLOWSCRIPT_ALLOW_UNSIGNED=1 is set. Nothing downloaded will be authenticated"
        say "  beyond the TLS connection to $base_url and the SHA256SUMS it serves."
    elif fetch_is_busybox_wget; then
        say "note: downloading with BusyBox wget, which does not validate TLS certificates; the signature check ($verifier) is what protects this install"
    fi

    version_explicit=1
    if [ -z "$version" ]; then
        version_explicit=0
        fetch "$base_url/latest.txt" "$work_dir/latest.txt" || die "could not fetch $base_url/latest.txt"
        version="$(tr -d ' \t\r\n' <"$work_dir/latest.txt")"
    fi
    version_ok "$version" || die "'$version' is not a release version (expected vX.Y.Z or vX.Y.Z-suffix, letters, digits, '.' and '-' only); refusing to use it"
    bare_version="${version#v}"
    floor="$(release_floor)"
    if version_older "$version" "$floor"; then
        if [ "$version_explicit" = 0 ]; then
            die "$base_url/latest.txt names $version, older than $floor, the oldest release this installer accepts as the latest; the release channel may have been tampered with. Refusing. (A newer installer may be needed; to install $version anyway, pass --version $version)"
        fi
        say "warning: $version is older than $floor, the oldest release this installer accepts as the latest; installing it because it was asked for by name"
    fi

    say "installing hollowscript $version for $target"

    archive="hollowscript-$bare_version-$target.tar.gz"
    fetch "$base_url/$version/$archive" "$work_dir/$archive" || die "could not fetch $base_url/$version/$archive"
    fetch "$base_url/$version/SHA256SUMS" "$work_dir/SHA256SUMS" || die "could not fetch $base_url/$version/SHA256SUMS"

    # --- the signature over SHA256SUMS ---
    pinned_public_key >"$work_dir/pinned.pub"
    extra_key="${HOLLOWSCRIPT_PUBLIC_KEY_FILE:-}"
    if [ -n "$extra_key" ] && [ ! -f "$extra_key" ]; then
        die "HOLLOWSCRIPT_PUBLIC_KEY_FILE '$extra_key' does not exist"
    fi
    if [ -n "$verifier" ]; then
        if [ "$verifier" = sshkeygen ]; then sig_name=SHA256SUMS.sshsig; else sig_name=SHA256SUMS.minisig; fi
        sig="$work_dir/$sig_name"
        fetch "$base_url/$version/$sig_name" "$sig" || die "could not fetch $base_url/$version/$sig_name (a release made before SSH signatures were published has no .sshsig; install minisign to verify it)"
        verified=0
        if "verify_with_$verifier" "$work_dir/pinned.pub" "$work_dir/SHA256SUMS" "$sig"; then
            verified=1
        elif [ -n "$extra_key" ] && "verify_with_$verifier" "$extra_key" "$work_dir/SHA256SUMS" "$sig"; then
            verified=1
        fi
        [ "$verified" = 1 ] || die "the signature on SHA256SUMS does not verify ($verifier); refusing to install"
        if [ "$verifier" != sshkeygen ]; then
            # The trusted comment is covered by the signature just verified,
            # and names the one version this SHA256SUMS was signed for. It
            # must be this version, exactly. (An SSH signature is made in a
            # namespace that names the version, which verify_with_sshkeygen
            # has already required.)
            comment="$(sed -n '3p' "$sig" | tr -d '\r')"
            expected_comment="trusted comment: hollowscript SHA256SUMS, version $version"
            [ "$comment" = "$expected_comment" ] \
                || die "SHA256SUMS was not signed for $version (its signed comment is '${comment#trusted comment: }'); refusing to install"
        fi
        say "signature verified ($verifier), signed for $version"
    fi

    # --- the archive against SHA256SUMS ---
    expected_sum="$(awk -v name="$archive" '$2 == name || $2 == "*" name { print $1 }' "$work_dir/SHA256SUMS")"
    [ -n "$expected_sum" ] || die "SHA256SUMS has no entry for $archive"
    actual_sum="$(sha256_of "$work_dir/$archive")"
    [ "$actual_sum" = "$expected_sum" ] || die "checksum mismatch for $archive: expected $expected_sum, got $actual_sum; not installing it"
    say "checksum verified"
    if [ -z "$verifier" ]; then
        if [ "$secure_transport" = 1 ]; then transport="over HTTPS"; else transport="over plain http (loopback testing)"; fi
        say "NOTICE: the archive matches SHA256SUMS, fetched $transport from $base_url, but"
        say "  the signature on SHA256SUMS was NOT checked (HOLLOWSCRIPT_ALLOW_UNSIGNED=1). To verify"
        say "  this release, install minisign (brew, apk or apt: install minisign) and check it by hand:"
        say "    curl -fsSLO $base_url/$version/SHA256SUMS -O $base_url/$version/SHA256SUMS.minisig"
        say "    minisign -Vm SHA256SUMS -P $(sed -n 2p "$work_dir/pinned.pub")"
    fi

    # --- never move an unasked-for install backwards ---
    if [ "$version_explicit" = 0 ] && [ -x "$hs_bin/hollowscript" ]; then
        installed="$("$hs_bin/hollowscript" --version 2>/dev/null | awk '{print $2}')" || installed=""
        if [ -n "$installed" ] && version_older "$version" "$installed"; then
            [ "$allow_downgrade" = 1 ] \
                || die "$base_url/latest.txt names $version, older than the installed $installed; refusing to downgrade (pass --version $version, or set HOLLOWSCRIPT_ALLOW_DOWNGRADE=1, to install it anyway)"
            say "installing $version over the newer $installed: HOLLOWSCRIPT_ALLOW_DOWNGRADE=1 is set"
        fi
    fi

    # --- extract, and prove the binary runs and is the version asked for ---
    (cd "$work_dir" && tar -xzf "$archive") || die "could not extract $archive"
    extracted="$work_dir/hollowscript-$bare_version-$target"
    for name in hollowscript hollowscript-lsp; do
        [ -f "$extracted/$name" ] || die "the archive has no $name binary"
    done
    reported="$("$extracted/hollowscript" --version 2>/dev/null)" \
        || die "the downloaded hollowscript does not run on this machine; not installing it"
    [ "$reported" = "hollowscript $bare_version" ] \
        || die "the downloaded hollowscript reports '$reported', not 'hollowscript $bare_version'; not installing it"
    say "downloaded binary runs ($reported)"

    # --- install: stage both binaries, then rename each into place ---
    mkdir -p "$hs_bin"
    install_state=staged
    for name in hollowscript hollowscript-lsp; do
        cp "$extracted/$name" "$hs_bin/$name.new"
        chmod 755 "$hs_bin/$name.new"
    done
    for name in hollowscript hollowscript-lsp; do
        rm -f "$hs_bin/.$name.previous"
        if [ -e "$hs_bin/$name" ]; then
            cp -p "$hs_bin/$name" "$hs_bin/.$name.previous"
        fi
    done
    install_state=swapping
    for name in hollowscript hollowscript-lsp; do
        # A rename within one directory is atomic: anything starting
        # hollowscript meanwhile gets the old binary or the new one.
        mv -f "$hs_bin/$name.new" "$hs_bin/$name"
    done
    "$hs_bin/hollowscript" --version >/dev/null 2>&1 \
        || die "the installed hollowscript does not run; restoring the previous install (or removing the new one, on a first install)"
    install_state=""
    rm -f "$hs_bin/.hollowscript.previous" "$hs_bin/.hollowscript-lsp.previous"
    write_env_files
    say "installed hollowscript $version in $hs_bin"

    if [ "$modify_path_ok" = 1 ]; then
        modify_path
        case ":$PATH:" in
            *":$hs_bin:"*) : ;;
            *) say "to use hollowscript now, open a new shell or run: . $(sh_quote "$hs_home/env")" ;;
        esac
    else
        case ":$PATH:" in
            *":$hs_bin:"*) : ;;
            *) say "PATH was not changed (--no-modify-path). To use hollowscript, add $hs_bin to PATH, for example by adding this line to your shell profile: $(profile_line)" ;;
        esac
    fi
}

main "$@"
