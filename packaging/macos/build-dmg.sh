#!/usr/bin/env bash
# Builds Leungo.dmg from target/release/leungo. Run on macOS.
# Usage: packaging/macos/build-dmg.sh <version> [output-dir]
set -euo pipefail

VERSION="${1:?version required}"
OUT="${2:-dist}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$ROOT/target/release/leungo"
[ -x "$BIN" ] || { echo "build first: cargo build --release -p leungo-cli" >&2; exit 1; }

STAGE="$(mktemp -d)"
APP="$STAGE/Leungo.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT"

cp "$BIN" "$APP/Contents/MacOS/leungo-bin"

# The bundle's entry point starts the daemon with the dashboard.
cat > "$APP/Contents/MacOS/Leungo" <<'LAUNCH'
#!/bin/bash
exec "$(dirname "$0")/leungo-bin" gui
LAUNCH
chmod +x "$APP/Contents/MacOS/Leungo"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Leungo</string>
  <key>CFBundleDisplayName</key><string>Leungo</string>
  <key>CFBundleIdentifier</key><string>dev.leungo.app</string>
  <key>CFBundleExecutable</key><string>Leungo</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>Leungo listens to the BlackHole loopback device to analyse and equalise your music.</string>
  <key>NSAppleEventsUsageDescription</key>
  <string>Leungo asks Spotify and Music which track is playing so it can re-analyse on song changes.</string>
</dict>
</plist>
PLIST

if [ -f "$ROOT/assets/brand/leungo.icns" ]; then
  cp "$ROOT/assets/brand/leungo.icns" "$APP/Contents/Resources/"
fi

[ "$(wc -c < "$APP/Contents/MacOS/leungo-bin")" -eq "$(wc -c < "$BIN")" ] || { echo "bundled binary was overwritten" >&2; exit 1; }

# Ad-hoc signature (no Developer ID): Apple silicon refuses to run unsigned code.
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

cp "$ROOT/LICENSE" "$STAGE/LICENSE.txt"
cat > "$STAGE/READ ME FIRST.txt" <<'TXT'
Leungo is not signed with an Apple Developer ID, so macOS blocks the first launch.

1. Drag Leungo.app to Applications.
2. Right-click Leungo.app, choose Open, then Open again in the dialog.
   (If it says the app is damaged, run in Terminal:
    xattr -dr com.apple.quarantine /Applications/Leungo.app )
3. Open Leungo. If BlackHole 2ch is missing, click "Install BlackHole 2ch" on the
   setup screen, enter your password, and restart if the installer asks.
4. Set BlackHole 2ch as the system output, and put your real speakers or
   headphones in ~/Library/Application Support/leungo/config.toml under
   [standalone] output_device.
5. Allow the Microphone and Automation prompts.

Full guide: https://github.com/rizqyep/leungo#macos
TXT
ln -s /Applications "$STAGE/Applications"

DMG="$OUT/Leungo-${VERSION}-macos-arm64.dmg"
rm -f "$DMG"
hdiutil create -volname "Leungo" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
shasum -a 256 "$DMG" | tee "$DMG.sha256"
rm -rf "$STAGE"
