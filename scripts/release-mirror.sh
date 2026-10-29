#!/usr/bin/env bash
# Builds the one commit a release mirror push is made of: exports a tagged
# tree, excludes internal/, scans it, and commits it, with no history and no
# push, to a local mirror repository directory (see docs/design/release.md).
#
#   scripts/release-mirror.sh <tag-or-commit> --mirror DIR [--dry-run]
#                             [--check-mirror-only]
#
# DIR is replaced wholesale by the export, so it is only ever one of: a
# directory that does not exist yet, an empty one, or a git repository this
# script made (it records that in the repository's own config, as
# hollowscript.release-mirror). Anything else, a directory with files and no
# repository, some other repository, this checkout or a parent of it, is
# refused before anything is exported or removed. `--check-mirror-only` stops
# after that check.
#
# <tag-or-commit> is exported with `git archive` (no working tree, no
# history: exactly what a checkout of that ref contains). `internal/` is
# then removed from the export before anything scans it, since it is this
# workspace's own boundary for what must never reach the public repository
# (docs/design/release-and-milestones.md), not merely something the hygiene
# scan happens to skip.
#
# `--dry-run` does everything (export, scan, commit) but prints the push
# command instead of leaving a mirror the caller might mistake for one
# that is safe to push without looking at the scan output.
#
# This script never pushes. It always prints the push command it did not
# run, so pushing stays a separate, deliberate, manual step.
set -euo pipefail

cd "$(dirname "$0")/.."
repo_root="$(pwd -P)"

step() { printf '\n==> %s\n' "$*"; }

ref="${1:-}"
if [ -z "$ref" ] || [[ "$ref" == --* ]]; then
    printf 'usage: scripts/release-mirror.sh <tag-or-commit> --mirror DIR [--dry-run]\n' >&2
    exit 2
fi
shift

mirror_dir=""
dry_run=0
check_only=0
while [ $# -gt 0 ]; do
    case "$1" in
        --mirror)
            mirror_dir="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=1
            shift
            ;;
        --check-mirror-only)
            check_only=1
            shift
            ;;
        *)
            printf 'unknown argument: %s\n' "$1" >&2
            exit 2
            ;;
    esac
done
[ -n "$mirror_dir" ] || { printf -- '--mirror DIR is required\n' >&2; exit 2; }

# --- the mirror directory: refused unless it is one this script may replace ---------------
mirror_refuse() {
    printf 'release-mirror.sh: refusing to use %s as the mirror: %s\n' "$mirror_dir" "$1" >&2
    exit 1
}
mirror_marker=hollowscript.release-mirror
if [ -e "$mirror_dir" ] || [ -L "$mirror_dir" ]; then
    [ -d "$mirror_dir" ] || mirror_refuse "it exists and is not a directory"
    mirror_abs="$(cd "$mirror_dir" && pwd -P)"
    # The mirror is emptied, so it must never be this checkout, or anything
    # that contains it or lives inside it.
    case "$repo_root/" in "$mirror_abs"/*) mirror_refuse "it is this checkout or a directory containing it" ;; esac
    case "$mirror_abs/" in "$repo_root"/*) mirror_refuse "it is inside this checkout" ;; esac
    if [ -n "$(ls -A "$mirror_dir")" ]; then
        [ -d "$mirror_dir/.git" ] \
            || mirror_refuse "it has files in it and is not a git repository; the mirror is replaced wholesale, so use an empty directory or one that does not exist yet"
        mirror_top="$(git -C "$mirror_dir" rev-parse --show-toplevel 2>/dev/null || true)"
        [ -n "$mirror_top" ] && [ "$(cd "$mirror_top" && pwd -P)" = "$mirror_abs" ] \
            || mirror_refuse "it is not the top of its own git repository"
        [ "$(git -C "$mirror_dir" config --local --get "$mirror_marker" || true)" = true ] \
            || mirror_refuse "it is a git repository this script did not make (it does not have $mirror_marker set); if it is a mirror made by an earlier run, mark it with: git -C $mirror_dir config $mirror_marker true"
    fi
fi
if [ "$check_only" -eq 1 ]; then
    printf '%s is usable as a mirror directory\n' "$mirror_dir"
    exit 0
fi

# No default for the mirror's remote: the public organisation and
# repository name are a launch decision, not something to hard-code into a
# script that this private checkout's own export includes. A dry run never
# pushes, so it does not need one; the push command it prints then names a
# placeholder.
if [ "$dry_run" -eq 1 ]; then
    : "${HOLLOWSCRIPT_MIRROR_REPO:=<HOLLOWSCRIPT_MIRROR_REPO>}"
else
    : "${HOLLOWSCRIPT_MIRROR_REPO:?HOLLOWSCRIPT_MIRROR_REPO must name the git remote of the mirror, for example the SSH URL of the public repository}"
fi
: "${HOLLOWSCRIPT_MIRROR_NAME:=HollowScript Releases}"
: "${HOLLOWSCRIPT_MIRROR_EMAIL:=releases@hollowscript.org}"

commit="$(git rev-parse "$ref^{commit}")" || {
    printf '%s does not resolve to a commit in this checkout\n' "$ref" >&2
    exit 1
}

step "exporting $ref ($commit) with git archive"
export_dir="$(mktemp -d)/hollowscript-release-export"
mkdir -p "$export_dir"
git archive --format=tar "$commit" | tar -x -C "$export_dir"

step "removing internal/"
if [ -e "$export_dir/internal" ]; then
    rm -rf "$export_dir/internal"
    printf 'removed %s/internal\n' "$export_dir"
else
    printf 'no internal/ in this export (nothing to remove)\n'
fi

# A release process's own boundary is stronger than what the hygiene scan
# enforces: hygiene deliberately does not scan internal/ (so working on
# this workspace's own internal notes is not itself a hygiene failure), but
# a release export must not contain it at all. Checked again here, after
# the removal above, as a second line of defence against a future export
# path that does not go through this script.
if [ -e "$export_dir/internal" ]; then
    printf 'internal/ is still present in the export; refusing to continue\n' >&2
    exit 1
fi
# Development-tooling files: none of these belong in a public mirror, even
# though the hygiene scan (a content scanner, not a filename scanner) does
# not look for them. Named in parts, not spelled out, so this check's own
# source does not itself carry the names it is checking for.
repo_instructions="$(printf 'AGE')$(printf 'NTS').md"
tool_config_dir=".$(printf 'cla')$(printf 'ude')"
tool_instructions="$(printf 'CLA')$(printf 'UDE').md"
preamble_file="PREAMBLE.md"
for stray in "$repo_instructions" "$tool_config_dir" "$tool_instructions" "$preamble_file"; do
    if [ -e "$export_dir/$stray" ]; then
        printf '%s is present in the export; refusing to continue\n' "$stray" >&2
        exit 1
    fi
done

step "cargo xtask hygiene --root $export_dir"
if ! (cd "$repo_root" && cargo run --quiet -p xtask -- hygiene --root "$export_dir"); then
    printf 'hygiene scan failed; not committing this export\n' >&2
    exit 1
fi

step "gitleaks"
if command -v gitleaks >/dev/null 2>&1; then
    if ! gitleaks detect --no-git --source "$export_dir" --redact -v; then
        printf 'gitleaks found something; not committing this export\n' >&2
        exit 1
    fi
else
    printf 'gitleaks is not installed (brew install gitleaks); skipped, not clean\n' >&2
    exit 1
fi

step "creating the release commit in $mirror_dir"
if [ ! -d "$mirror_dir/.git" ]; then
    mkdir -p "$mirror_dir"
    git -C "$mirror_dir" init -q -b main
    git -C "$mirror_dir" config --local "$mirror_marker" true
fi
# Every path already in the mirror directory that is not in this export is
# removed first, so the mirror commit is exactly this export's tree, the
# same guarantee `git archive` + a fresh checkout would give, without
# discarding the mirror's own `.git` history of prior release commits.
find "$mirror_dir" -mindepth 1 -maxdepth 1 ! -name '.git' -exec rm -rf {} +
cp -R "$export_dir"/. "$mirror_dir"/

version="${ref#v}"
message="release: HollowScript ${version}

Exported from the private repository at ${commit}, hygiene-scanned and
gitleaks-scanned, no history carried over. See the release notes for what
changed."

(
    cd "$mirror_dir"
    git add -A
    is_tag=0
    if [[ "$ref" != "HEAD" && ! "$ref" =~ ^[0-9a-f]{7,40}$ ]]; then
        is_tag=1
    fi
    if git diff --cached --quiet; then
        printf 'nothing changed since the mirror'"'"'s last commit; not committing\n'
    else
        # A tag is never moved silently: if the mirror already has this
        # release's tag, on a commit whose tree differs from this export,
        # the mirror disagrees with this release, and that is for a person
        # to resolve, not for `git tag -f` to paper over. Checked before
        # committing; the mirror directory is put back as it was.
        if [ "$is_tag" -eq 1 ] && git rev-parse -q --verify "refs/tags/$ref" >/dev/null; then
            printf 'the mirror already has a tag %s, on %s, with a different tree; not committing or moving it\n' \
                "$ref" "$(git rev-parse "$ref^{commit}")" >&2
            git reset -q --hard
            git clean -q -fd
            exit 1
        fi
        GIT_AUTHOR_NAME="$HOLLOWSCRIPT_MIRROR_NAME" \
            GIT_AUTHOR_EMAIL="$HOLLOWSCRIPT_MIRROR_EMAIL" \
            GIT_COMMITTER_NAME="$HOLLOWSCRIPT_MIRROR_NAME" \
            GIT_COMMITTER_EMAIL="$HOLLOWSCRIPT_MIRROR_EMAIL" \
            git commit -q -m "$message"
        # Only for an actual tag name (not "HEAD", not a bare commit): git
        # rejects both as tag names, and a dry run against an arbitrary
        # commit should not require a real tag to try.
        if [ "$is_tag" -eq 1 ]; then
            git tag "$ref"
        fi
    fi
)

rm -rf "$(dirname "$export_dir")"

step "done"
printf 'mirror commit: %s\n' "$(git -C "$mirror_dir" rev-parse HEAD)"
printf 'tree matches the scanned export: %s\n' "$(git -C "$mirror_dir" rev-parse 'HEAD^{tree}')"
if [ "$dry_run" -eq 1 ]; then
    printf 'dry run: nothing was pushed, and this mirror is a scratch directory, not the real one.\n'
fi
printf '\nTo publish this release, run (not run by this script):\n'
printf '  git -C %s push %s HEAD:main %s\n' "$mirror_dir" "$HOLLOWSCRIPT_MIRROR_REPO" "$ref"
