#!/bin/sh
# Builds backfill.app for a release and zips it as target/dist/backfill-macos.zip.
#
#   scripts/package-macos.sh 0.2.0    sets the version in the source, then builds, signs and zips
#   scripts/package-macos.sh --check  only checks that the signing certificate is there
#
# semantic-release runs it with the version it's about to release (see .releaserc.json), so
# the release commit, the tag and the app all say the same version.
#
# The app is signed with backfill's own self-signed certificate, and it only installs an
# update signed with that same certificate, so every release has to use it. Losing it means
# everyone has to download the next version by hand. BACKFILL_SIGN_IDENTITY names it
# ("backfill self-signed" by default) and BACKFILL_KEYCHAIN says which keychain it's in,
# when that isn't one macOS searches anyway, as on CI.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: scripts/package-macos.sh VERSION | --check" >&2
    exit 2
fi
identity="${BACKFILL_SIGN_IDENTITY:-backfill self-signed}"
cd "$(dirname "$0")/.."

if [ -n "${BACKFILL_KEYCHAIN:-}" ]; then
    set -- "$1" --keychain "$BACKFILL_KEYCHAIN"
fi
version="$1"
shift

# Untrusted is fine: macOS never checks this certificate against a root, only that an
# update was signed with it.
if ! security find-identity -p codesigning ${BACKFILL_KEYCHAIN:+"$BACKFILL_KEYCHAIN"} | grep -q "\"$identity\""; then
    echo "package-macos: no signing identity called \"$identity\"" >&2
    exit 1
fi
if [ "$version" = --check ]; then
    # Listed isn't always usable, so sign something small, before a release spends minutes
    # building only to find codesign can't use it.
    probe="$(mktemp -t backfill-sign)"
    trap 'rm -f "$probe"' EXIT
    codesign --force --sign "$identity" "$@" "$probe"
    exit 0
fi

# The version, in the app's crate and its Info.plist.
perl -pi -e 'if (!$done && s/^version = "[^"]*"/version = "'"$version"'"/) { $done = 1 }' crates/app/Cargo.toml
perl -0pi -e 's#(<key>CFBundle(?:ShortVersionString|Version)</key>\s*<string>)[^<]*#${1}'"$version"'#g' \
    crates/app/macos/Info.plist

cargo build --release -p backfill --features updates

dist=target/dist
app="$dist/backfill.app"
rm -rf "$dist"
mkdir -p "$app/Contents/MacOS"
cp target/release/backfill "$app/Contents/MacOS/backfill"
cp crates/app/macos/Info.plist "$app/Contents/Info.plist"
codesign --force --sign "$identity" "$@" "$app"
codesign --verify --deep --strict "$app"
# Without extended attributes, so nothing but the signed files goes in the zip.
ditto -c -k --keepParent --norsrc --noextattr --noqtn --noacl "$app" "$dist/backfill-macos.zip"
echo "package-macos: $dist/backfill-macos.zip is backfill $version"
