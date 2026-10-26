#!/bin/sh
# Cargo runs binaries through this on macOS (see .cargo/config.toml).
#
# The backfill binary is wrapped in an app bundle next to it and opened as an app, because
# macOS only lets apps ask for calendar access. Output still appears in this terminal, and
# Ctrl-C quits it. Anything else, such as test binaries, runs unchanged.
#
# The bundle is signed with your "Apple Development" identity if you have one, so macOS
# remembers the calendar permission across rebuilds; otherwise it is signed ad hoc and
# macOS may ask again after a rebuild. BACKFILL_SIGN_IDENTITY overrides the choice ("-"
# means ad hoc).
set -eu

bin="$1"
shift
if [ "$(basename "$bin")" != backfill ]; then
    exec "$bin" "$@"
fi

repo="$(cd "$(dirname "$0")/.." && pwd)"
app="$(dirname "$bin")/backfill.app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/backfill"
cp "$repo/crates/app/macos/Info.plist" "$app/Contents/Info.plist"

identity="${BACKFILL_SIGN_IDENTITY:-$(security find-identity -v -p codesigning 2>/dev/null \
    | awk '/"Apple Development/ { print $2; exit }')}"
codesign --force --sign "${identity:--}" "$app" 2>/dev/null \
    || codesign --force --sign - "$app"

# Pass BACKFILL_* settings through, since apps opened this way don't inherit the shell's.
set --
for var in $(env | sed -n 's/^\(BACKFILL_[A-Z_]*\)=.*/\1/p'); do
    eval "value=\${$var}"
    set -- "$@" --env "$var=$value"
done

out="$(tty 2>/dev/null || true)"
case "$out" in /dev/*) ;; *) out=/dev/null ;; esac

open -W -n "$app" --stdout "$out" --stderr "$out" "$@" &
waiting=$!
trap 'pkill -f "$app/Contents/MacOS/backfill" || true; exit 130' INT TERM
wait "$waiting"
