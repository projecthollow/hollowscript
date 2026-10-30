#!/usr/bin/env bash
# Builds a release: for every shipped target, a CLI archive (the
# `hollowscript` and `hollowscript-lsp` binaries) and an SDK archive (the C
# library and its header), plus an SDK archive of the WebAssembly VM, a
# SHA256SUMS over all of them and a CycloneDX SBOM, byte for byte
# reproducibly. docs/design/release.md, section 1, is the full account; this
# header is the summary.
#
#   scripts/release-build.sh --version vX.Y.Z [--targets t1,t2,...] [--out DIR]
#                            [--rehearsal] [--no-verify-reproducible]
#
# --version is required, and must agree with everything else that names the
# version, or nothing is built:
#   - the workspace version in Cargo.toml (`vX.Y.Z` is `v` + that version),
#   - the git tag on HEAD (a tag named exactly vX.Y.Z must point at HEAD),
#   - what the built `hollowscript --version` prints (checked after the build,
#     on the host target, and on x86_64-apple-darwin too under Rosetta),
#   - what the C library's hs_version() returns (checked the same way),
#   - and, later, the signed trusted comment: `cargo xtask sign --version`
#     refuses a SHA256SUMS naming any other version, and
#     scripts/release-upload.sh refuses a signature whose trusted comment is
#     not exactly "hollowscript SHA256SUMS, version vX.Y.Z".
# A dirty working tree is refused too. --rehearsal relaxes exactly two of
# these (a dirty tree, and no tag on HEAD), for a local rehearsal, and records
# that it did in DIR/release-build-info.txt, which scripts/release-upload.sh
# reads and refuses to publish to a real bucket.
#
# Every requested target must build: a target that cannot be built (a missing
# linker, a missing tool) fails the whole run, with a non-zero exit. The
# default target list is every shipped target:
#   aarch64-apple-darwin, x86_64-apple-darwin      native `cargo build`
#   x86_64-unknown-linux-musl                      `cargo zigbuild` (static)
#   aarch64-unknown-linux-musl                     `cargo zigbuild` (static)
#   x86_64-pc-windows-gnu                          `cargo zigbuild`
#   wasm32-unknown-unknown                         native `cargo build`, the
#                                                  WebAssembly VM only
# `cargo zigbuild` needs `zig` (`brew install zig`) and
# `cargo install cargo-zigbuild`.
set -euo pipefail

cd "$(dirname "$0")/.."
repo_root="$(pwd)"

step() { printf '\n==> %s\n' "$*"; }
die() {
    printf 'release-build.sh: %s\n' "$*" >&2
    exit 1
}

all_targets="aarch64-apple-darwin,x86_64-apple-darwin,x86_64-unknown-linux-musl,aarch64-unknown-linux-musl,x86_64-pc-windows-gnu,wasm32-unknown-unknown"
targets_arg="$all_targets"
out_dir="$repo_root/dist"
verify_reproducible=1
release_version=""
rehearsal=0
verification_cargo_home="different"

while [ $# -gt 0 ]; do
    case "$1" in
        --verification-cargo-home)
            [ $# -ge 2 ] || die "--verification-cargo-home needs different or fixed"
            verification_cargo_home="$2"
            case "$verification_cargo_home" in different | fixed) : ;; *) die "invalid verification Cargo home mode" ;; esac
            shift 2
            ;;
        --targets)
            [ $# -ge 2 ] || die "--targets needs a value"
            targets_arg="$2"
            shift 2
            ;;
        --out)
            [ $# -ge 2 ] || die "--out needs a value"
            out_dir="$2"
            shift 2
            ;;
        --no-verify-reproducible)
            verify_reproducible=0
            shift
            ;;
        --version)
            [ $# -ge 2 ] || die "--version needs a value"
            release_version="$2"
            shift 2
            ;;
        --rehearsal)
            rehearsal=1
            shift
            ;;
        -h | --help)
            sed -n '2,40p' "$0"
            exit 0
            ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

IFS=',' read -r -a targets <<<"$targets_arg"
[ "${#targets[@]}" -gt 0 ] || die "--targets names no target"
for target in "${targets[@]}"; do
    case ",$all_targets," in
        *",$target,"*) : ;;
        *) die "unknown target '$target'; known targets: $all_targets" ;;
    esac
done

# --- the version, and everything that must agree with it ---------------------
[ -n "$release_version" ] || die "--version vX.Y.Z is required (it must match the workspace version in Cargo.toml and the tag on HEAD)"
case "$release_version" in
    *[!A-Za-z0-9.-]*) die "'$release_version' contains a character a version string may not (only letters, digits, '.' and '-')" ;;
esac
[[ "$release_version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$ ]] \
    || die "'$release_version' does not look like a release version (expected vX.Y.Z or vX.Y.Z-suffix)"
version="${release_version#v}"

cargo_version="$(cargo metadata --no-deps --format-version 1 --locked \
    | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "hollowscript-cli"))')" \
    || die "could not read the workspace version with cargo metadata"
[ "$cargo_version" = "$version" ] \
    || die "--version $release_version disagrees with the workspace version in Cargo.toml ($cargo_version); bump Cargo.toml first, or pass --version v$cargo_version"

commit="$(git rev-parse HEAD)"
short_commit="${commit:0:12}"
dirty="$(git status --porcelain --untracked-files=normal)"
tag_commit="$(git rev-parse -q --verify "refs/tags/$release_version^{commit}" 2>/dev/null || true)"
notes=()
if [ -n "$dirty" ]; then
    [ "$rehearsal" -eq 1 ] || die "the working tree is not clean; a release is built from a committed, tagged tree only (pass --rehearsal for a local rehearsal):
$dirty"
    notes+=("working tree was dirty (rehearsal)")
fi
if [ "$tag_commit" != "$commit" ]; then
    if [ -n "$tag_commit" ]; then
        # A tag with this name on another commit is wrong even in a
        # rehearsal: the archives would claim a version whose tag is
        # somewhere else.
        die "tag $release_version points at $tag_commit, not at HEAD ($commit)"
    fi
    [ "$rehearsal" -eq 1 ] || die "HEAD is not tagged $release_version; tag the release commit first (git tag -a $release_version), or pass --rehearsal for a local rehearsal"
    notes+=("HEAD is not tagged $release_version (rehearsal)")
fi

# --- tools -------------------------------------------------------------------
host_target="$(rustc -vV | sed -n 's/^host: //p')"
needs_zig=0
needs_macos=0
for target in "${targets[@]}"; do
    case "$target" in
        *-apple-darwin) needs_macos=1 ;;
        wasm32-*) : ;;
        *) needs_zig=1 ;;
    esac
done
for tool in cargo rustup git python3 shasum tar gzip; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required and not on PATH"
done
if [ "$needs_zig" -eq 1 ]; then
    command -v zig >/dev/null 2>&1 || die "zig is required for the Linux and Windows targets (brew install zig)"
    command -v cargo-zigbuild >/dev/null 2>&1 || die "cargo-zigbuild is required for the Linux and Windows targets (cargo install cargo-zigbuild)"
fi
if [ "$needs_macos" -eq 1 ]; then
    [ "$(uname -s)" = "Darwin" ] || die "the apple-darwin targets are built on macOS only"
    command -v codesign >/dev/null 2>&1 || die "codesign is required for the apple-darwin targets"
fi
for target in "${targets[@]}"; do
    case "$target" in
        *windows*) command -v zip >/dev/null 2>&1 || die "zip is required for the Windows target" ;;
    esac
done

mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
if [ -n "$(ls -A "$out_dir")" ]; then
    die "$out_dir is not empty; remove it first, so nothing from an earlier build can end up in this release"
fi

: "${CARGO_TARGET_DIR:=$repo_root/target}"
export CARGO_TARGET_DIR
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
sysroot="$(rustc --print sysroot)"

# A release identifies itself by the commit it was built from, not by the
# build clock: SOURCE_DATE_EPOCH is the commit's own author date.
: "${SOURCE_DATE_EPOCH:=$(git log -1 --format=%at "$commit")}"
export SOURCE_DATE_EPOCH

# The repository helper, built before RUSTFLAGS below is set (it is not
# shipped, and building it with the release flags would only rebuild its
# dependencies for nothing).
step "building the release helper (cargo xtask)"
cargo build --quiet --locked -p xtask
xtask="$CARGO_TARGET_DIR/debug/xtask"
[ -x "$xtask" ] || die "the xtask build did not produce $xtask"

step "adding rustup targets"
for target in "${targets[@]}"; do
    rustup target add "$target" >/dev/null || die "rustup could not add $target"
done

# --- flags that keep local paths out of every artefact -----------------------
#
# See docs/design/release.md, section 1, "Reproducibility", for why each is
# needed. In short: rustc embeds source paths in panic locations (and, with
# debug information, elsewhere), for this checkout, for every crate under
# $CARGO_HOME's registry, and for anything under the target directory. Each
# is remapped to a fixed, machine-independent prefix, so the same commit gives
# the same bytes wherever it is built and no home directory is leaked; the C
# code a dependency compiles (ring) gets the same treatment through CFLAGS.
# `-C metadata` fixes the symbol-mangling disambiguator Cargo otherwise
# derives from absolute manifest paths.
release_rustflags() {
    local checkout="$1" target_dir="$2" home="$3"
    printf '%s' "--remap-path-prefix=$checkout=/hollowscript --remap-path-prefix=$target_dir=/target --remap-path-prefix=$home=/cargo --remap-path-prefix=$sysroot=/rust -C metadata=hollowscript-release"
}
release_cflags() {
    local checkout="$1" target_dir="$2" home="$3"
    printf '%s' "-ffile-prefix-map=$checkout=/hollowscript -ffile-prefix-map=$target_dir=/target -ffile-prefix-map=$home=/cargo"
}
# Per package: the macOS C library's install name is set at link time. ld64
# otherwise records the dylib's absolute build path, which leaks the path,
# means nothing to anyone else, and (since the header is sized for it) moves
# every section by the path's length; rewriting it afterwards cannot undo
# that last part.
package_rustflags() {
    local target="$1" package="$2"
    if is_macos "$target" && [ "$package" = hollowscript-capi ]; then
        printf ' -C link-arg=-Wl,-install_name,@rpath/libhollowscript_capi.dylib'
    fi
}

is_windows() { [[ "$1" == *windows* ]]; }
is_macos() { [[ "$1" == *-apple-darwin ]]; }
is_wasm() { [[ "$1" == wasm32-* ]]; }
exe_suffix() { if is_windows "$1"; then printf '.exe'; fi; }

# The packages built for a target.
packages_for() {
    if is_wasm "$1"; then
        printf '%s\n' hollowscript-wasm
    else
        printf '%s\n' hollowscript-cli hollowscript-lsp hollowscript-capi
    fi
}

# The build products shipped for a target, by file name in its dist directory.
products_for() {
    local target="$1" exe
    exe="$(exe_suffix "$target")"
    case "$target" in
        wasm32-*) printf '%s\n' hollowscript_wasm.wasm ;;
        *-apple-darwin) printf '%s\n' hollowscript hollowscript-lsp libhollowscript_capi.a libhollowscript_capi.dylib ;;
        # musl targets link the C runtime statically, so rustc builds no
        # cdylib for them: the SDK is the static library.
        *-linux-musl) printf '%s\n' hollowscript hollowscript-lsp libhollowscript_capi.a ;;
        *-windows-gnu) printf '%s\n' "hollowscript$exe" "hollowscript-lsp$exe" libhollowscript_capi.a hollowscript_capi.dll libhollowscript_capi.dll.a ;;
        *) die "no product list for $target" ;;
    esac
}

# build_package CHECKOUT TARGET_DIR CARGO_HOME TARGET PACKAGE [cargo args...]
#
# One package per `cargo build` invocation, never `-p a -p b`: building the C
# library's `cdylib` in the same invocation as the CLI changes the CLI's own
# bytes, because the unified build graph lets one package's codegen needs
# leak into crates the other links (docs/design/release.md, section 1).
build_package() {
    local checkout="$1" tdir="$2" home="$3" target="$4" package="$5"
    shift 5
    local -a tool=(cargo build)
    if ! is_macos "$target" && ! is_wasm "$target"; then
        tool=(cargo zigbuild)
    fi
    (cd "$checkout" && env \
        CARGO_HOME="$home" \
        CARGO_TARGET_DIR="$tdir" \
        RUSTFLAGS="$(release_rustflags "$checkout" "$tdir" "$home")$(package_rustflags "$target" "$package")" \
        CFLAGS="$(release_cflags "$checkout" "$tdir" "$home")" \
        "${tool[@]}" --locked --profile dist --target "$target" -p "$package" "$@")
}

# build_all CHECKOUT TARGET_DIR CARGO_HOME [cargo args...]
build_all() {
    local checkout="$1" tdir="$2" home="$3" target package
    shift 3
    for target in "${targets[@]}"; do
        step "building $target${*:+ ($*)}"
        for package in $(packages_for "$target"); do
            build_package "$checkout" "$tdir" "$home" "$target" "$package" "$@" \
                || die "building $package for $target failed"
        done
    done
}

build_all "$repo_root" "$CARGO_TARGET_DIR" "$cargo_home"

# --- making each product reproducible after the link ------------------------------
#
# rustc already stripped every binary (`strip = "symbols"` in the dist
# profile). Two things the linkers write still depend on the build:
#
# - Mach-O: ld64 derives LC_UUID from object file names that depend on the
#   checkout path, so it is rewritten to a hash of the binary's own bytes,
#   then the binary is signed again ad hoc (the old signature is removed
#   first, since it covers the old UUID; the new one records the file's
#   name, so a file is always finalised under its shipped name).
# - PE: the Windows linker writes the build clock into the COFF header's
#   TimeDateStamp and does not accept --no-insert-timestamp, so it is set to
#   SOURCE_DATE_EPOCH.
#
# Every step must succeed.
finalise_product() {
    local path="$1" target="$2"
    case "$target:$path" in
        *-apple-darwin:*.a) : ;;
        *-apple-darwin:*)
            codesign --remove-signature "$path" || die "codesign --remove-signature failed on $path"
            python3 "$repo_root/scripts/lib/patch-macho-uuid.py" "$path" >/dev/null || die "rewriting LC_UUID failed on $path"
            codesign -s - -f "$path" || die "ad-hoc signing failed on $path"
            if [[ "$path" == *.dylib ]]; then
                [ "$(otool -D "$path" | sed -n 2p)" = "@rpath/$(basename "$path")" ] \
                    || die "$path does not have the install name @rpath/$(basename "$path")"
            fi
            ;;
        *-windows-gnu:*.exe | *-windows-gnu:*.dll)
            python3 "$repo_root/scripts/lib/patch-pe-timestamp.py" "$path" "$SOURCE_DATE_EPOCH" >/dev/null \
                || die "setting the PE timestamp failed on $path"
            ;;
    esac
}

# --- leak check -----------------------------------------------------------------
#
# No shipped file may contain the home directory, this checkout's path, the
# target directory, Cargo's home or the toolchain's own path. Checked on every
# binary and library that is packaged.
leak_needles=("$HOME" "$repo_root" "$CARGO_TARGET_DIR" "$cargo_home" "$sysroot")
check_no_leaks() {
    python3 scripts/release-tools.py check-path "$1" "${leak_needles[@]}" \
        || die "a shipped file leaks a builder path; not packaging it"
}

# --- reproducibility check, every target ------------------------------------------
#
# Builds every target again from a copy of this tree at a different path,
# with a different target directory and a different CARGO_HOME (whose
# registry sources are unpacked afresh under the new path), finalises both
# builds' products the same way, and requires every shipped product to be
# byte-identical. A path that escaped the remapping above, or anything else a
# build takes from the machine or the clock, differs between the two and
# fails this, even where the leak check above would not know what to look for.
if [ "$verify_reproducible" -eq 1 ]; then
    step "reproducibility: separate source/target paths (Cargo home mode: $verification_cargo_home)"
    scratch="$(mktemp -d)"
    second_checkout="$scratch/a-second-checkout"
    second_target_dir="$scratch/target"
    second_home="$scratch/cargo-home"
    mkdir -p "$second_checkout" "$second_home/registry"
    # The working tree as it is (tracked and untracked files, not ignored
    # ones), so a rehearsal of uncommitted changes compares like with like.
    # The list is written to a file first, not piped: a `while` loop inside
    # a pipeline under `set -e` made bash 3.2 (macOS's /bin/bash) exit
    # silently, now and then, part way through this copy.
    file_list="$scratch/files"
    : >"$file_list"
    while IFS= read -r -d '' f; do
        if [ -e "$f" ]; then printf '%s\0' "$f" >>"$file_list"; fi
    done < <(git ls-files -z -co --exclude-standard)
    tar --null -T "$file_list" -cf - | tar -xf - -C "$second_checkout" \
        || die "copying the tree for the reproducibility build failed"
    ln -s "$cargo_home/registry/index" "$second_home/registry/index"
    ln -s "$cargo_home/registry/cache" "$second_home/registry/cache"
    if [ "$verification_cargo_home" = fixed ]; then
        # The pinned non-Apple builder treats this path as an input. The
        # second build still has a fresh target directory and source path.
        second_home="$cargo_home"
    fi
    build_all "$second_checkout" "$second_target_dir" "$second_home" --offline
    compared=0
    for target in "${targets[@]}"; do
        for product in $(products_for "$target"); do
            a="$scratch/first/$target/$product"
            b="$scratch/second/$target/$product"
            mkdir -p "$(dirname "$a")" "$(dirname "$b")"
            cp "$CARGO_TARGET_DIR/$target/dist/$product" "$a" || die "missing build product $target/$product"
            cp "$second_target_dir/$target/dist/$product" "$b" || die "the second build has no $target/$product"
            finalise_product "$a" "$target"
            finalise_product "$b" "$target"
            cmp -s "$a" "$b" \
                || die "NOT REPRODUCIBLE: $product for $target differs between two checkout paths and CARGO_HOMEs"
            compared=$((compared + 1))
        done
    done
    printf 'reproducible: all %d shipped products match separate source/target paths (Cargo home mode: %s)\n' "$compared" "$verification_cargo_home"
    rm -rf "$scratch"
fi

# --- packaging -------------------------------------------------------------------
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

zig_lib=""
if [ "$needs_zig" -eq 1 ]; then
    zig_lib="$(zig env | sed -n 's/.*[.]lib_dir = "\(.*\)",$/\1/p')"
    [ -d "$zig_lib" ] || zig_lib="$(zig env 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["lib_dir"])' 2>/dev/null || true)"
    [ -d "$zig_lib" ] || die "could not find zig's lib directory (zig env)"
fi

# The C runtime notices a target's artefacts carry, beyond the Rust
# standard library's own: musl is linked statically into every Linux binary,
# and mingw-w64's runtime into every Windows one, both from zig.
runtime_notice_args() {
    local target="$1"
    case "$target" in
        *-linux-musl)
            [ -f "$zig_lib/libc/musl/COPYRIGHT" ] || die "musl's COPYRIGHT is missing from $zig_lib"
            printf '%s\0' --notice "musl libc (statically linked)=$zig_lib/libc/musl/COPYRIGHT"
            ;;
        *-windows-gnu)
            [ -f "$zig_lib/libc/mingw/COPYING" ] || die "mingw-w64's COPYING is missing from $zig_lib"
            printf '%s\0' --notice "mingw-w64 runtime (statically linked)=$zig_lib/libc/mingw/COPYING"
            ;;
    esac
}

stamp="$(date -u -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S 2>/dev/null \
    || date -u -d "@$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)"

# Fixed permissions and timestamps on everything staged, so neither the
# builder's umask nor the build clock reaches an archive.
normalise_tree() {
    local dir="$1"
    find "$dir" -type d -exec chmod 755 {} +
    find "$dir" -type f -exec chmod 644 {} +
    find "$dir" -type f \( -name hollowscript -o -name hollowscript-lsp -o -name '*.exe' -o -name '*.dll' -o -name '*.so' -o -name '*.dylib' \) -exec chmod 755 {} +
    find "$dir" -exec env TZ=UTC touch -h -t "$stamp" {} +
}

archive_tar() {
    local name="$1"
    python3 scripts/release-tools.py archive "$stage/$name" "$out_dir/$name.tar.gz" "$SOURCE_DATE_EPOCH"
}

archive_zip() {
    local name="$1"
    (
        cd "$stage"
        find "$name" -type f | LC_ALL=C sort | TZ=UTC zip -q -X -D -@ "$out_dir/$name.zip"
    )
}

archive() {
    local target="$1" name="$2"
    normalise_tree "$stage/$name"
    if is_windows "$target"; then
        archive_zip "$name"
        archives+=("$name.zip")
    else
        archive_tar "$name"
        archives+=("$name.tar.gz")
    fi
}

legal_files() {
    local dir="$1" target="$2" artifact="$3"
    shift 3
    cp LICENSE-APACHE LICENSE-MIT NOTICE "$dir/"
    local -a packages=() notices=()
    local p
    for p in "$@"; do packages+=(--package "$p"); done
    while IFS= read -r -d '' p; do notices+=("$p"); done < <(runtime_notice_args "$target")
    "$xtask" third-party-licenses --target "$target" "${packages[@]}" ${notices[@]+"${notices[@]}"} \
        --artifact "$artifact" --out "$dir/THIRD-PARTY-LICENSES" >/dev/null \
        || die "writing THIRD-PARTY-LICENSES for $artifact failed"
}

# Copies one build product into the stage, failing if it is missing, makes
# it reproducible and checks it for leaks.
ship() {
    local from="$1" to="$2" target="$3"
    [ -f "$from" ] || die "expected build product $from is missing"
    cp "$from" "$to"
    finalise_product "$to" "$target"
    check_no_leaks "$to"
}

archives=()
sbom_args=()
for target in "${targets[@]}"; do
    dist="$CARGO_TARGET_DIR/$target/dist"

    if is_wasm "$target"; then
        name="hollowscript-sdk-$version-$target"
        step "packaging $name"
        mkdir -p "$stage/$name"
        ship "$dist/hollowscript_wasm.wasm" "$stage/$name/hollowscript_wasm.wasm" "$target"
        cp crates/hollowscript-wasm/web/hollowscript.js crates/hollowscript-wasm/web/README.md "$stage/$name/"
        legal_files "$stage/$name" "$target" "$name" hollowscript-wasm
        archive "$target" "$name"
        sbom_args+=(--artifact "${archives[${#archives[@]}-1]}=$target:hollowscript-wasm")
        continue
    fi

    exe="$(exe_suffix "$target")"
    name="hollowscript-$version-$target"
    step "packaging $name"
    mkdir -p "$stage/$name"
    ship "$dist/hollowscript$exe" "$stage/$name/hollowscript$exe" "$target"
    ship "$dist/hollowscript-lsp$exe" "$stage/$name/hollowscript-lsp$exe" "$target"
    cp README.md "$stage/$name/"
    legal_files "$stage/$name" "$target" "$name" hollowscript-cli hollowscript-lsp
    archive "$target" "$name"
    sbom_args+=(--artifact "${archives[${#archives[@]}-1]}=$target:hollowscript-cli,hollowscript-lsp")

    name="hollowscript-sdk-$version-$target"
    step "packaging $name"
    mkdir -p "$stage/$name/include" "$stage/$name/lib"
    cp crates/hollowscript-capi/include/hollowscript.h "$stage/$name/include/"
    for lib in $(products_for "$target"); do
        case "$lib" in
            *hollowscript_capi*) ship "$dist/$lib" "$stage/$name/lib/$lib" "$target" ;;
        esac
    done
    cp scripts/lib/README-sdk.md "$stage/$name/README.md"
    legal_files "$stage/$name" "$target" "$name" hollowscript-capi
    archive "$target" "$name"
    sbom_args+=(--artifact "${archives[${#archives[@]}-1]}=$target:hollowscript-capi")
done

# --- the version the artefacts report must be the version they are named for --
step "checking the built artefacts report $version"
check_cli_version() {
    local label="$1"
    shift
    local reported
    reported="$("$@" --version)" || die "$label: hollowscript --version failed"
    [ "$reported" = "hollowscript $version" ] \
        || die "$label: hollowscript --version printed '$reported', expected 'hollowscript $version'"
    printf '%s: %s\n' "$label" "$reported"
}
check_c_version() {
    local label="$1" sdk="$2"
    shift 2
    local dir probe reported
    dir="$(mktemp -d)"
    printf '#include <stdio.h>\n#include "hollowscript.h"\nint main(void) { printf("%%s\\n", hs_version()); return 0; }\n' >"$dir/probe.c"
    cc "$@" -I "$sdk/include" "$dir/probe.c" -L "$sdk/lib" -lhollowscript_capi -Wl,-rpath,"$sdk/lib" -o "$dir/probe" \
        || die "$label: could not link a C program against the SDK"
    probe="$dir/probe"
    reported="$(${run_prefix[@]+"${run_prefix[@]}"} "$probe")" || die "$label: the C probe failed to run"
    rm -rf "$dir"
    [ "$reported" = "$version" ] || die "$label: hs_version() returned '$reported', expected '$version'"
    printf '%s: hs_version() = %s\n' "$label" "$reported"
}
run_verified=()
for target in "${targets[@]}"; do
    run_prefix=()
    if [ "$target" = "$host_target" ]; then
        :
    elif [ "$host_target" = "aarch64-apple-darwin" ] && [ "$target" = "x86_64-apple-darwin" ] \
        && arch -x86_64 /usr/bin/true 2>/dev/null; then
        run_prefix=(arch -x86_64)
    else
        continue
    fi
    check_cli_version "$target" ${run_prefix[@]+"${run_prefix[@]}"} "$stage/hollowscript-$version-$target/hollowscript"
    if is_macos "$target"; then
        macho_arch="${target%%-*}"
        [ "$macho_arch" = "aarch64" ] && macho_arch="arm64"
        check_c_version "$target" "$stage/hollowscript-sdk-$version-$target" -arch "$macho_arch"
    fi
    run_verified+=("$target")
done
[ "${#run_verified[@]}" -gt 0 ] || printf 'note: no requested target runs on this machine; the version check ran on none of them\n'

# --- SBOM -------------------------------------------------------------------------
step "SBOM"
sbom="hollowscript-$version-sbom.cdx.json"
"$xtask" sbom --version "$release_version" --out "$out_dir/$sbom" "${sbom_args[@]}" \
    || die "writing the SBOM failed"

# --- SHA256SUMS -------------------------------------------------------------------
step "SHA256SUMS"
# Names are fixed patterns with no spaces, so the sorted list splits safely.
sorted_names="$(printf '%s\n' "${archives[@]}" "$sbom" | LC_ALL=C sort)"
# shellcheck disable=SC2086 # split on purpose, see above
(cd "$out_dir" && shasum -a 256 $sorted_names) >"$out_dir/SHA256SUMS" \
    || die "writing SHA256SUMS failed"
cat "$out_dir/SHA256SUMS"

# Not part of the release (not in SHA256SUMS, never uploaded): how this
# build was made, so release-upload.sh can refuse a rehearsal.
{
    printf 'version=%s\n' "$release_version"
    printf 'commit=%s\n' "$commit"
    printf 'rehearsal=%s\n' "$rehearsal"
    printf 'targets=%s\n' "${targets[*]}"
    printf 'verification_cargo_home=%s\n' "$verification_cargo_home"
    printf 'verified_reproducible=%s\n' "$verify_reproducible"
    for note in "${notes[@]+"${notes[@]}"}"; do printf 'note=%s\n' "$note"; done
} >"$out_dir/release-build-info.txt"

step "summary"
printf 'version: %s (commit %s)\n' "$release_version" "$short_commit"
printf 'built and archived: %s\n' "${targets[*]}"
printf 'version checked by running: %s\n' "${run_verified[*]:-none}"
printf 'archives:\n'
printf '  %s\n' "${archives[@]}"
if [ "$rehearsal" -eq 1 ]; then
    printf 'REHEARSAL build: not publishable (see %s)\n' "$out_dir/release-build-info.txt"
    for note in "${notes[@]+"${notes[@]}"}"; do printf '  %s\n' "$note"; done
fi
printf 'output: %s\n' "$out_dir"
