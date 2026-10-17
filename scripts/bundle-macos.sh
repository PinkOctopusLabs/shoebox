#!/usr/bin/env bash
# Build Shoebox.app for macOS: release build, Info.plist, icon, signing, and
# (optionally) notarization.
#
#   scripts/bundle-macos.sh
#
# Environment:
#   SIGN_IDENTITY   codesign identity. Default "-" (ad-hoc: runs on this Mac).
#                   For other Macs use a Developer ID, e.g.
#                   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)"
#   NOTARY_PROFILE  a `xcrun notarytool store-credentials` profile name; when
#                   set (with a Developer ID), the app is notarized and stapled.
#   OUT_DIR         where Shoebox.app goes (default target/release/bundle).
#
# The app is not sandboxed: it reads and moves files anywhere in the library
# folders you add. Hardened runtime is on.
set -euo pipefail
cd "$(dirname "$0")/.."

IDENTITY="${SIGN_IDENTITY:--}"
OUT_DIR="${OUT_DIR:-target/release/bundle}"
BUNDLE_ID="com.crankyunicorn.shoebox"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
BUILD="$(git rev-list --count HEAD 2>/dev/null || echo 1)"
APP="$OUT_DIR/Shoebox.app"

echo "Building Shoebox $VERSION ($BUILD)…"
cargo build --release -p shoebox-app -p shoebox-cli

# Assemble a fresh bundle. Only our own build output is replaced here.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
cp target/release/shoebox-app "$APP/Contents/MacOS/Shoebox"
# The command-line tool travels with the app, in Helpers (not MacOS: the
# file system is case-insensitive, so `shoebox` would replace `Shoebox`):
#   ln -s /Applications/Shoebox.app/Contents/Helpers/shoebox ~/bin/shoebox
cp target/release/shoebox "$APP/Contents/Helpers/shoebox"

# Icon: every size macOS asks for, from the 1024 px master.
ICONSET="$(mktemp -d)/Shoebox.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" assets/icon-1024.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z "$d" "$d" assets/icon-1024.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Shoebox.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Shoebox</string>
  <key>CFBundleDisplayName</key><string>Shoebox</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>Shoebox</string>
  <key>CFBundleIconFile</key><string>Shoebox</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$BUILD</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>27.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSHumanReadableCopyright</key><string>© $(date +%Y) Pink Octopus Labs</string>
  <key>NSDesktopFolderUsageDescription</key><string>Shoebox shows and organises the photos in library folders you add.</string>
  <key>NSDocumentsFolderUsageDescription</key><string>Shoebox shows and organises the photos in library folders you add.</string>
  <key>NSDownloadsFolderUsageDescription</key><string>Shoebox shows and organises the photos in library folders you add.</string>
  <key>NSRemovableVolumesUsageDescription</key><string>Shoebox imports photos from memory cards and shows library folders on external drives.</string>
  <key>NSNetworkVolumesUsageDescription</key><string>Shoebox shows library folders you add.</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Sign: nested executables first, then the app, with the hardened runtime.
# A secure timestamp needs a real identity (and the network).
SIGN_ARGS=(--force --options runtime --sign "$IDENTITY")
if [ "$IDENTITY" != "-" ]; then
  SIGN_ARGS+=(--timestamp)
fi
codesign "${SIGN_ARGS[@]}" "$APP/Contents/Helpers/shoebox"
codesign "${SIGN_ARGS[@]}" "$APP"
codesign --verify --strict --verbose=1 "$APP"

if [ -n "${NOTARY_PROFILE:-}" ]; then
  if [ "$IDENTITY" = "-" ]; then
    echo "Notarization needs a Developer ID (set SIGN_IDENTITY)." >&2
    exit 1
  fi
  ZIP="$OUT_DIR/Shoebox-$VERSION.zip"
  ditto -c -k --keepParent "$APP" "$ZIP"
  echo "Notarizing (this can take a few minutes)…"
  xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
  rm -f "$ZIP"
  spctl --assess --type execute --verbose "$APP"
fi

# A zip to copy to another Mac (made after stapling, so it carries the ticket).
ditto -c -k --keepParent "$APP" "$OUT_DIR/Shoebox-$VERSION.zip"
echo "Built $APP"
echo "Zip:  $OUT_DIR/Shoebox-$VERSION.zip"
