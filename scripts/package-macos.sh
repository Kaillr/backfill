#!/bin/sh
# Builds a backfill release with Velopack, in target/velopack: the disk image people install
# from (backfill.dmg), the full package, a delta from the release before, and the feed the
# app reads its updates from.
#
#   scripts/package-macos.sh 0.2.0    sets the version in the source, then builds and packs
#   scripts/package-macos.sh --check  only checks that codesign can use the certificate
#
# semantic-release runs it with the version it's about to release (see .releaserc.json), so
# the release commit, the tag and the app all say the same version. Packing needs vpk, the
# Velopack CLI (`dotnet tool install -g vpk`), and scripts/upload-macos.sh publishes the result.
#
# The app is signed with backfill's own self-signed certificate. It isn't what updates are
# checked with, but it keeps the app the same app to macOS from one version to the next, so
# its calendar access and Keychain items carry over. BACKFILL_SIGN_IDENTITY names it
# ("backfill self-signed" by default) and BACKFILL_KEYCHAIN says which keychain it's in,
# when that isn't one macOS searches anyway, as on CI.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: scripts/package-macos.sh VERSION | --check" >&2
    exit 2
fi
identity="${BACKFILL_SIGN_IDENTITY:-backfill self-signed}"
vpk="${VPK:-vpk}"
repo=https://github.com/Kaillr/backfill
cd "$(dirname "$0")/.."

if [ -n "${BACKFILL_KEYCHAIN:-}" ]; then
    set -- "$1" --keychain "$BACKFILL_KEYCHAIN"
fi
version="$1"
shift

# Untrusted is fine: nothing checks this certificate against a root.
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

cargo build --release -p backfill

app=target/dist/backfill.app
releases=target/velopack
rm -rf target/dist "$releases"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/backfill "$app/Contents/MacOS/backfill"
cp crates/app/macos/Info.plist "$app/Contents/Info.plist"

# The newest release so far, so the new one can carry a delta from it. Before the first
# release there's none, and vpk says so and carries on.
"$vpk" download github --repoUrl "$repo" --channel osx --outputDir "$releases" \
    ${GITHUB_TOKEN:+--token "$GITHUB_TOKEN"}

# No .pkg: backfill installs by dragging it to Applications, so it belongs to whoever did, and
# Velopack can update it without asking for an admin password.
"$vpk" pack --packId backfill --packVersion "$version" --packTitle backfill --packAuthors backfill \
    --packDir "$app" --mainExe backfill --channel osx --noInst --outputDir "$releases" \
    --signAppIdentity "$identity" --signEntitlements crates/app/macos/backfill.entitlements "$@"

# The disk image: the app vpk signed, next to a shortcut to Applications to drag it onto.
# It's uploaded in place of vpk's zip of the same app.
staging="$(mktemp -d)/backfill"
mkdir -p "$staging"
ditto -x -k "$releases/backfill-osx-Portable.zip" "$staging"
ln -s /Applications "$staging/Applications"
hdiutil create -volname backfill -srcfolder "$staging" -fs HFS+ -format UDZO -ov "$releases/backfill.dmg"
rm -rf "$(dirname "$staging")" "$releases/backfill-osx-Portable.zip"
perl -pi -e 's/backfill-osx-Portable\.zip/backfill.dmg/' "$releases/assets.osx.json"

echo "package-macos: backfill $version is in $releases"
