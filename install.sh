#!/bin/bash
# Installs (or updates) Drawlycool from the latest GitHub release:
#   curl -fsSL https://raw.githubusercontent.com/rafa-rrayes/Drawlycool/main/install.sh | bash
set -euo pipefail

REPO="rafa-rrayes/Drawlycool"
APP="Drawlycool.app"
BUNDLE_ID="br.com.rayes.drawlycool"
URL="https://github.com/$REPO/releases/latest/download/Drawlycool.dmg"
DEST="${INSTALL_DIR:-/Applications}"

fail() { echo "error: $*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || fail "Drawlycool is a macOS app."
[ "$(uname -m)" = arm64 ] || fail "Drawlycool needs a Mac with Apple silicon."
# Not an admin, or /Applications is locked down: install just for this user.
if [ ! -w "$DEST" ]; then
  DEST="$HOME/Applications"
  mkdir -p "$DEST"
fi

tmp="$(mktemp -d)"
mnt="$tmp/mnt"
cleanup() {
  hdiutil detach -quiet "$mnt" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

echo "Downloading Drawlycool…"
curl -fL --progress-bar -o "$tmp/Drawlycool.dmg" "$URL" || fail "download failed ($URL)"
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mnt" "$tmp/Drawlycool.dmg" || fail "couldn't open the disk image"
[ -d "$mnt/$APP" ] || fail "the disk image has no $APP"

# Quit a running copy so it saves its canvases before being replaced.
if pgrep -qf "$DEST/$APP/Contents/MacOS/"; then
  echo "Quitting the running Drawlycool…"
  osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do
    pgrep -qf "$DEST/$APP/Contents/MacOS/" || break
    sleep 0.5
  done
fi

echo "Installing to $DEST/$APP…"
rm -rf "${DEST:?}/$APP"
ditto "$mnt/$APP" "$DEST/$APP"
# The app is ad-hoc signed, not notarized: skip Gatekeeper's "unidentified developer" block.
xattr -dr com.apple.quarantine "$DEST/$APP" 2>/dev/null || true

echo "Done. Opening Drawlycool."
open "$DEST/$APP"
