#!/bin/sh
# Adds what scripts/package-macos.sh built to the GitHub release tagged TAG, which
# semantic-release has just made: the disk image, the packages, and this release's part of
# the update feed, which lists only its own packages. Needs gh, jq and GITHUB_TOKEN.
#
#   scripts/upload-macos.sh v0.2.0
#
# Not `vpk upload`: it looks the release up in GitHub's list of releases, which can lag a
# moment behind one just made, and then makes a draft of its own and puts the files there.
# gh finds the release by its tag.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: scripts/upload-macos.sh TAG" >&2
    exit 2
fi
cd "$(dirname "$0")/.."

tag="$1"
releases=target/velopack
feed="$(mktemp -d)/releases.osx.json"
jq --arg version "${tag#v}" '{Assets: [.Assets[] | select(.Version == $version)]}' "$releases/releases.osx.json" > "$feed"
set --
for file in $(jq -r '.[].RelativeFileName' "$releases/assets.osx.json"); do
    set -- "$@" "$releases/$file"
done

# semantic-release reads what a publish step prints as the release it made, so this goes
# to stderr.
for attempt in 1 2 3 4 5 6; do
    if gh release upload "$tag" "$@" "$feed" --clobber --repo Kaillr/backfill >&2; then
        exit 0
    fi
    echo "upload-macos: the release isn't there yet, trying again ($attempt)" >&2
    sleep 5
done
exit 1
