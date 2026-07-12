#!/usr/bin/env bash
# Build the SwiftUI app and assemble a minimal Token9.app bundle (SPM-only,
# no Xcode required). Not code-signed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT/token9-apps/macos"
BUNDLE="$APP_DIR/Token9.app"

cd "$APP_DIR"
swift build -c release
BIN="$(swift build -c release --show-bin-path)/Token9"

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS"
cp "$BIN" "$BUNDLE/Contents/MacOS/Token9"

cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>Token9</string>
    <key>CFBundleIdentifier</key>
    <string>ai.oraculo.token9</string>
    <key>CFBundleName</key>
    <string>token9</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
PLIST

echo "built: $BUNDLE"
echo "run: open '$BUNDLE'  (with 'token9 serve' running)"
