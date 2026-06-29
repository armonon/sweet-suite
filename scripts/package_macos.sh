#!/usr/bin/env bash
#
# package_macos.sh — build SWEET Visual as a double-clickable macOS .app bundle.
#
# The release binary is self-contained (links only system frameworks), so the bundle is
# just Info.plist + the binary + an ad-hoc code signature. Output:
#
#   dist/SweetVisual.app          — the app bundle (double-click to run)
#   dist/SweetVisual-macos.zip    — zipped, for handing off / "download"
#
# Usage:  ./scripts/package_macos.sh [--no-build]
#
set -euo pipefail

cd "$(dirname "$0")/.."          # repo root
ROOT="$(pwd)"
APP_NAME="SweetVisual"
BIN_NAME="suite-visual"
BUNDLE_ID="com.sweet.visual"
VERSION="0.1.0"

DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"
CONTENTS="$APP/Contents"

if [[ "${1:-}" != "--no-build" ]]; then
  echo "==> Building release binary…"
  cargo build --release -p suite-visual
fi

BIN="$ROOT/target/release/$BIN_NAME"
[[ -f "$BIN" ]] || { echo "error: $BIN not found — build first"; exit 1; }

echo "==> Assembling $APP_NAME.app…"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BIN" "$CONTENTS/MacOS/$APP_NAME"
chmod +x "$CONTENTS/MacOS/$APP_NAME"

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>SWEET Visual</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>$APP_NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

echo "==> Ad-hoc code-signing (so Gatekeeper allows a first-run 'Open')…"
codesign --force --deep --sign - "$APP" 2>/dev/null || \
  echo "   (codesign unavailable — the app still runs via right-click → Open)"

echo "==> Zipping…"
( cd "$DIST" && rm -f "$APP_NAME-macos.zip" && zip -q -r --symlinks "$APP_NAME-macos.zip" "$APP_NAME.app" )

echo ""
echo "Done."
echo "  App:  $APP"
echo "  Zip:  $DIST/$APP_NAME-macos.zip"
echo ""
echo "First launch: right-click the app → Open (unsigned app; one-time Gatekeeper bypass)."
