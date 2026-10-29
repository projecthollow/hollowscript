#!/usr/bin/env bash
# Withdraws a release from the release bucket and, optionally, reinstates an
# earlier one as latest, rehearsed against a local directory standing in for
# the bucket (docs/design/release.md, section 6).
#
#   scripts/release-withdraw.sh <version> --bucket-dir DIR [--reinstate <version>]
#
# What "withdraw" means here: the release mirror (scripts/release-mirror.sh)
# keeps its history, so a withdrawn version's commit and tag stay reachable
# for anyone who already has them; withdrawal is about what a new download
# gets. The version's objects are moved aside (never deleted by this script),
# and latest.txt is moved back to the version to reinstate, or removed if the
# withdrawn version was latest and nothing is reinstated.
#
# This script acts on a local directory only. For the real bucket it prints
# the equivalent `gcloud storage` commands, with the headers
# scripts/release-upload.sh sets, for a person to run: removing a public
# artefact is a deliberate step, not one a script takes by itself.
#
# There is no CDN to purge. get.hollowscript.org redirects to the bucket on
# storage.googleapis.com, and what a client still sees after a withdrawal is
# decided by the Cache-Control each object was uploaded with: latest.txt is
# `max-age=60`, so a moved or removed latest.txt reaches new installs within
# a minute; a versioned object is `max-age=86400`, so a copy a client or a
# proxy already fetched, or one held in Google's own cache of public Cloud
# Storage objects, can outlive its deletion from the bucket by up to a day.
# Moving latest.txt is what stops new default installs at once; an install
# pinned to the withdrawn version may still be served from a cache until
# then.
set -euo pipefail

step() { printf '\n==> %s\n' "$*"; }
die() {
    printf 'release-withdraw.sh: %s\n' "$1" >&2
    exit 1
}

usage="usage: scripts/release-withdraw.sh <version> --bucket-dir DIR [--reinstate <version>]"
version="${1:-}"
[ -n "$version" ] && [[ "$version" != -* ]] || die "$usage"
shift

bucket_dir=""
reinstate=""
while [ $# -gt 0 ]; do
    case "$1" in
        --bucket-dir)
            [ $# -ge 2 ] || die "--bucket-dir needs a value"
            bucket_dir="$2"
            shift 2
            ;;
        --reinstate)
            [ $# -ge 2 ] || die "--reinstate needs a value"
            reinstate="$2"
            shift 2
            ;;
        *) die "unknown argument: $1 ($usage)" ;;
    esac
done

check_version() {
    [[ "$1" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$ ]] \
        || die "'$1' is not a release version (vX.Y.Z or vX.Y.Z-suffix)"
}
check_version "$version"
[ -z "$reinstate" ] || check_version "$reinstate"
[ "$reinstate" != "$version" ] || die "cannot reinstate the version being withdrawn"
[ -n "$bucket_dir" ] || die "--bucket-dir DIR is required"
[ -d "$bucket_dir" ] || die "$bucket_dir does not exist"
[ -d "$bucket_dir/$version" ] || die "$bucket_dir/$version does not exist; nothing to withdraw"
[ -z "$reinstate" ] || [ -d "$bucket_dir/$reinstate" ] || die "$bucket_dir/$reinstate does not exist; cannot reinstate it"

read_latest() {
    tr -d ' \t\r\n' <"$bucket_dir/latest.txt"
}

step "withdrawing $version"
withdrawn_dir="$bucket_dir/.withdrawn"
mkdir -p "$withdrawn_dir"
[ ! -e "$withdrawn_dir/$version" ] \
    || die "$withdrawn_dir/$version already exists; a previous withdrawal was not completed, resolve that first"

# latest.txt first: it is what new default installs follow, so it stops
# pointing at the withdrawn version before that version's objects move.
if [ -n "$reinstate" ]; then
    step "reinstating $reinstate as latest"
    printf '%s\n' "$reinstate" >"$bucket_dir/latest.txt"
    printf 'latest.txt now names %s\n' "$reinstate"
elif [ -e "$bucket_dir/latest.txt" ] && [ "$(read_latest)" = "$version" ]; then
    step "no --reinstate: removing latest.txt, which named $version"
    rm -f "$bucket_dir/latest.txt" "$bucket_dir/latest.txt.metadata"
    printf 'removed latest.txt; nothing installs by default until a release moves it again, and a pinned install of another version still works\n'
fi

# Moved aside, not deleted: a withdrawal can itself be a mistake.
mv "$bucket_dir/$version" "$withdrawn_dir/$version"
printf 'moved %s to %s\n' "$version" "$withdrawn_dir/$version"

step "verifying"
[ ! -e "$bucket_dir/$version" ] || die "verification failed: $version is still at $bucket_dir/$version"
if [ -n "$reinstate" ]; then
    [ "$(read_latest)" = "$reinstate" ] || die "verification failed: latest.txt does not name $reinstate"
elif [ -e "$bucket_dir/latest.txt" ]; then
    [ "$(read_latest)" != "$version" ] || die "verification failed: latest.txt still names $version"
fi
printf 'withdrawal of %s verified.\n' "$version"

step "the same against the real bucket (not run by this script)"
if [ -n "$reinstate" ]; then
    latest_cmd="printf '%s\\n' $reinstate | gcloud storage cp --content-type='text/plain; charset=utf-8' --cache-control='public, max-age=60, must-revalidate' - \"gs://\$HOLLOWSCRIPT_RELEASE_BUCKET/latest.txt\""
else
    latest_cmd="gcloud storage rm \"gs://\$HOLLOWSCRIPT_RELEASE_BUCKET/latest.txt\"   # only if it names $version"
fi
cat <<EOF
  1. $latest_cmd
  2. gcloud storage cp --recursive "gs://\$HOLLOWSCRIPT_RELEASE_BUCKET/$version" "gs://\$HOLLOWSCRIPT_RELEASE_BUCKET/.withdrawn/$version"
  3. gcloud storage rm --recursive "gs://\$HOLLOWSCRIPT_RELEASE_BUCKET/$version"

Step 1 reaches new default installs within latest.txt's max-age (60 s).
There is no CDN to purge: steps 2 and 3 stop the bucket serving $version, but
a copy a client, a proxy or Google's cache of public objects already holds may
be served until its Cache-Control (max-age=86400) runs out: within 24 hours
of step 3, nothing should serve $version any more. A security withdrawal
should say so in its advisory.

Not touched: the release mirror's history (scripts/release-mirror.sh), and
$withdrawn_dir/$version, kept until the withdrawal is confirmed correct.
EOF
