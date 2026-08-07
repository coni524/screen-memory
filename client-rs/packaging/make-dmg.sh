#!/bin/zsh
# Packs the built .app into a distributable DMG (run build-app.sh first).
# Output: client-rs/dist/screen-memory-<version>.dmg
# The DMG holds only the .app and a symlink to /Applications, so whoever opens
# it can install by dragging.
# Usage: packaging/make-dmg.sh [identity] [notary profile]
#   The identity is passed straight through to build-app.sh (default: screen-memory).
#
# If the identity starts with "Developer ID Application", this is treated as a
# distribution build: the DMG is signed as well, submitted to Apple for
# notarization with notarytool, and the notarization ticket is attached to the
# DMG with stapler, so users can open it without a warning.
# Store the notarization credentials in the keychain once, beforehand:
#   xcrun notarytool store-credentials screen-memory-notary \
#     --apple-id <Apple ID email> --team-id <team ID> --password <app-specific password>
# Issue the app-specific password at https://account.apple.com > Sign-In and Security.
set -euo pipefail

SCRIPT_DIR="${0:A:h}"
CLIENT_DIR="${SCRIPT_DIR:h}"
DIST="$CLIENT_DIR/dist"
APP_DIR="$DIST/Screen Memory.app"
DMG_ROOT="$DIST/dmg-root"
# Take the version from [package] in Cargo.toml, the same source build-installer.ps1 uses
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$CLIENT_DIR/Cargo.toml" | head -1)
DMG="$DIST/screen-memory-$VERSION.dmg"
IDENTITY="${1:-screen-memory}"
NOTARY_PROFILE="${2:-screen-memory-notary}"

"$SCRIPT_DIR/build-app.sh" "$IDENTITY"

rm -rf "$DMG_ROOT" "$DMG"
mkdir -p "$DMG_ROOT"
cp -R "$APP_DIR" "$DMG_ROOT/"
ln -s /Applications "$DMG_ROOT/Applications"

hdiutil create -volname "Screen Memory" -srcfolder "$DMG_ROOT" -format UDZO -ov "$DMG" >/dev/null

if [[ "$IDENTITY" == "Developer ID Application"* ]]; then
    codesign --force --sign "$IDENTITY" --timestamp "$DMG"
    echo "Submitting for notarization (takes a few minutes)..."
    xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
fi

echo "Built: $DMG"
