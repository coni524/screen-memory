#!/bin/zsh
# Release build -> .app bundle -> codesign.
# Usage: packaging/build-app.sh [identity]  (default: screen-memory)
#
# Three signing options:
# - adhoc: ad-hoc signature, no certificate needed. Fine if you build once and
#   keep using that build. But TCC ties the permission to the binary's hash, so
#   you have to re-grant Screen Recording access after every rebuild.
# - Self-signed certificate (default: screen-memory): the permission survives
#   any number of rebuilds. Create one in Keychain Access as a "Self Signed
#   Root" with certificate type "Code Signing".
# - Developer ID Application: Name (Team ID): for distribution. Signs with
#   hardened runtime and a secure timestamp, both required for notarization.
#   Use make-dmg.sh if you also want to notarize.
set -euo pipefail

IDENTITY="${1:-screen-memory}"
[[ "$IDENTITY" == "adhoc" ]] && IDENTITY="-"
SCRIPT_DIR="${0:A:h}"
CLIENT_DIR="${SCRIPT_DIR:h}"
APP_DIR="$CLIENT_DIR/dist/Screen Memory.app"

if [[ "$IDENTITY" != "-" ]] && ! security find-identity -p codesigning -v | grep -qF "\"$IDENTITY\""; then
    echo "Error: code signing certificate \"$IDENTITY\" is not in the keychain" >&2
    echo "Create one via Keychain Access > Certificate Assistant > Create a Certificate," >&2
    echo "with name $IDENTITY, identity type \"Self Signed Root\", certificate type \"Code Signing\"" >&2
    exit 1
fi

cd "$CLIENT_DIR"
# Dependency crates bake their own source paths into panic messages and log
# events, which would ship the build machine's home directory
# (/Users/<name>/.cargo/registry/...) inside the binary. Rewrite that prefix to
# a plain "~" so the distributed binary carries no user name.
export RUSTFLAGS="--remap-path-prefix=$HOME=~ ${RUSTFLAGS:-}"

# --remap-path-prefix only reaches rustc. aws-lc-sys and ring compile their C
# through cc-rs, and clang bakes each __FILE__ into the assert and error strings
# it emits, which put the home directory back into the binary by a second route.
# -ffile-prefix-map rewrites it the same way, so both halves agree on "~".
export CFLAGS="-ffile-prefix-map=$HOME=~ ${CFLAGS:-}"
export CXXFLAGS="-ffile-prefix-map=$HOME=~ ${CXXFLAGS:-}"
cargo build --release

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$SCRIPT_DIR/Info.plist" "$APP_DIR/Contents/Info.plist"
cp "$SCRIPT_DIR/AppIcon.icns" "$APP_DIR/Contents/Resources/AppIcon.icns"
cp target/release/screen-memory "$APP_DIR/Contents/MacOS/screen-memory"

SIGN_FLAGS=(--force --sign "$IDENTITY")
if [[ "$IDENTITY" == "Developer ID Application"* ]]; then
    SIGN_FLAGS+=(--options runtime --timestamp)
fi
codesign "${SIGN_FLAGS[@]}" "$APP_DIR"
codesign --verify --verbose=2 "$APP_DIR"

echo "Built: $APP_DIR"
echo "Next steps:"
echo "  1. launchctl unload ~/Library/LaunchAgents/com.screen-memory.agent.plist (if an older version is running)"
echo "  2. rm -rf '/Applications/Screen Memory.app' && cp -R '$APP_DIR' /Applications/"
echo "  3. Open the app by hand once, then grant it access under System Settings > Privacy & Security > Screen Recording"
echo "  4. cp '$SCRIPT_DIR/com.screen-memory.agent.plist' ~/Library/LaunchAgents/ && launchctl load ~/Library/LaunchAgents/com.screen-memory.agent.plist"
if [[ "$IDENTITY" == "-" ]]; then
    echo "Note: with an ad-hoc signature you must redo the step 3 permission after every rebuild"
fi
