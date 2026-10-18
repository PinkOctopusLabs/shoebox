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
#   SPARKLE_PUBLIC_KEY  the EdDSA public key updates are checked against
#                   (default: assets/sparkle-public-key.txt). Without one the
#                   app is built with no updater.
#   VERSION, BUILD, BUNDLE_ID  override the version, build number and bundle
#                   id (for test builds).
#
# The app is not sandboxed: it reads and moves files anywhere in the library
# folders you add. Developer ID builds use the hardened runtime.
set -euo pipefail
cd "$(dirname "$0")/.."

IDENTITY="${SIGN_IDENTITY:--}"
OUT_DIR="${OUT_DIR:-target/release/bundle}"
BUNDLE_ID="${BUNDLE_ID:-com.crankyunicorn.shoebox}"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
FEED_URL="https://github.com/PinkOctopusLabs/shoebox/releases/latest/download/appcast.xml"
PUBLIC_KEY="${SPARKLE_PUBLIC_KEY:-$(cat assets/sparkle-public-key.txt 2>/dev/null || true)}"
BUILD="${BUILD:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"
APP="$OUT_DIR/Shoebox.app"

echo "Building Shoebox $VERSION ($BUILD)…"
cargo build --release -p shoebox-app -p shoebox-cli

# Assemble a fresh bundle. Only our own build output is replaced here.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources" \
  "$APP/Contents/Frameworks"
cp target/release/shoebox-app "$APP/Contents/MacOS/Shoebox"
# The command-line tool travels with the app, in Helpers (not MacOS: the
# file system is case-insensitive, so `shoebox` would replace `Shoebox`):
#   ln -s /Applications/Shoebox.app/Contents/Helpers/shoebox ~/bin/shoebox
cp target/release/shoebox "$APP/Contents/Helpers/shoebox"
# The icon font is compiled in; its licence ships beside it.
cp crates/shoebox-app/assets/Phosphor-LICENSE.txt "$APP/Contents/Resources/Phosphor-LICENSE.txt"

# Sparkle, the updater (loaded at runtime by src/updater.rs).
if [ -n "$PUBLIC_KEY" ]; then
  SPARKLE="$(scripts/fetch-sparkle.sh)"
  ditto "$SPARKLE/Sparkle.framework" "$APP/Contents/Frameworks/Sparkle.framework"
  cp "$SPARKLE/LICENSE" "$APP/Contents/Resources/Sparkle-LICENSE.txt"
  UPDATER_KEYS="  <key>SUFeedURL</key><string>$FEED_URL</string>
  <key>SUPublicEDKey</key><string>$PUBLIC_KEY</string>
  <key>SUEnableAutomaticChecks</key><true/>
  <key>SUAutomaticallyUpdate</key><true/>
  <key>SUScheduledCheckInterval</key><integer>86400</integer>"
else
  echo "No Sparkle public key (assets/sparkle-public-key.txt): building without the updater." >&2
  UPDATER_KEYS=""
fi

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
  <key>NSHumanReadableCopyright</key><string>© $(date +%Y) Pink Octopus Pty Ltd</string>
  <key>NSDesktopFolderUsageDescription</key><string>Shoebox shows and organises the photos in library folders you add.</string>
  <key>NSDocumentsFolderUsageDescription</key><string>Shoebox shows and organises the photos in library folders you add.</string>
  <key>NSDownloadsFolderUsageDescription</key><string>Shoebox shows and organises the photos in library folders you add.</string>
  <key>NSRemovableVolumesUsageDescription</key><string>Shoebox imports photos from memory cards and shows library folders on external drives.</string>
  <key>NSNetworkVolumesUsageDescription</key><string>Shoebox shows library folders you add.</string>
$UPDATER_KEYS
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Sign: nested code first, then the app. A Developer ID build gets the
# hardened runtime (notarization needs it) and a secure timestamp (needs the
# network). Ad-hoc builds go without the hardened runtime: its library
# validation won't load an ad-hoc signed Sparkle.framework.
SIGN_ARGS=(--force --sign "$IDENTITY")
if [ "$IDENTITY" != "-" ]; then
  SIGN_ARGS+=(--options runtime --timestamp)
fi
codesign "${SIGN_ARGS[@]}" "$APP/Contents/Helpers/shoebox"
SPARKLE_FW="$APP/Contents/Frameworks/Sparkle.framework"
if [ -d "$SPARKLE_FW" ]; then
  # Inside out, as Sparkle's documentation sets out.
  codesign "${SIGN_ARGS[@]}" "$SPARKLE_FW/Versions/B/XPCServices/Installer.xpc"
  codesign "${SIGN_ARGS[@]}" --preserve-metadata=entitlements \
    "$SPARKLE_FW/Versions/B/XPCServices/Downloader.xpc"
  codesign "${SIGN_ARGS[@]}" "$SPARKLE_FW/Versions/B/Autoupdate"
  codesign "${SIGN_ARGS[@]}" "$SPARKLE_FW/Versions/B/Updater.app"
  codesign "${SIGN_ARGS[@]}" "$SPARKLE_FW"
fi
codesign "${SIGN_ARGS[@]}" "$APP"
codesign --verify --deep --strict --verbose=1 "$APP"

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
