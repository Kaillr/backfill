#!/bin/sh
# Adds what scripts/package-macos.sh built to the GitHub release tagged TAG, which
# semantic-release has just made: the disk image, the packages, and this release's part of
# the update feed. Needs vpk and GITHUB_TOKEN.
#
#   scripts/upload-macos.sh v0.2.0
set -eu

if [ $# -ne 1 ]; then
    echo "usage: scripts/upload-macos.sh TAG" >&2
    exit 2
fi
cd "$(dirname "$0")/.."

# semantic-release reads what a publish step prints as the release it made, so vpk's
# progress goes to stderr.
"${VPK:-vpk}" upload github --repoUrl https://github.com/Kaillr/backfill --token "$GITHUB_TOKEN" \
    --channel osx --outputDir target/velopack --tag "$1" --merge >&2
