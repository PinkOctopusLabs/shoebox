#!/usr/bin/env bash
# Release Shoebox: bump the version, build a signed and notarized app, and
# publish it on GitHub Releases with a Sparkle appcast, which installed copies
# pick up at their next update check.
#
#   scripts/release-macos.sh 0.2.0
#
# Environment:
#   SIGN_IDENTITY   required: "Developer ID Application: Pink Octopus Labs (TEAMID)"
#   NOTARY_PROFILE  required: a `xcrun notarytool store-credentials` profile
#   SPARKLE_ACCOUNT Keychain account of the EdDSA update key (default "shoebox",
#                   made with: "$(scripts/fetch-sparkle.sh)/bin/generate_keys" --account shoebox)
#   SKIP_TESTS=1    skip the local fmt/clippy/test run (CI runs them anyway)
#
# Nothing is pushed until the app is built, notarized and its appcast signed.
set -euo pipefail
cd "$(dirname "$0")/.."

REPO="PinkOctopusLabs/shoebox"
NEW="${1:-}"
ACCOUNT="${SPARKLE_ACCOUNT:-shoebox}"
die() { echo "release: $*" >&2; exit 1; }

[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "usage: $0 <major.minor.patch>"
TAG="v$NEW"
case "${SIGN_IDENTITY:-}" in
  "Developer ID Application:"*) ;;
  *) die "set SIGN_IDENTITY to your Developer ID Application identity" ;;
esac
[ -n "${NOTARY_PROFILE:-}" ] || die "set NOTARY_PROFILE (xcrun notarytool store-credentials)"

# The tree is clean and is origin's main.
[ "$(git branch --show-current)" = "main" ] || die "release from main"
[ -z "$(git status --porcelain)" ] || die "the working tree has changes"
git fetch --quiet origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "main isn't origin/main"
! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || die "$TAG already exists"
gh auth status >/dev/null 2>&1 || die "gh isn't signed in"

# The update key in the Keychain is the one the app checks against.
SPARKLE="$(scripts/fetch-sparkle.sh)"
PUBLIC_KEY="$(cat assets/sparkle-public-key.txt 2>/dev/null || true)"
[ -n "$PUBLIC_KEY" ] || die "no assets/sparkle-public-key.txt"
KEYCHAIN_KEY="$("$SPARKLE/bin/generate_keys" --account "$ACCOUNT" -p 2>/dev/null || true)"
[ "$KEYCHAIN_KEY" = "$PUBLIC_KEY" ] ||
  die "the Keychain's update key (account $ACCOUNT) doesn't match assets/sparkle-public-key.txt"

OLD="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
echo "Releasing Shoebox $OLD → $NEW"

# Bump the workspace version (the first `version =` in Cargo.toml).
sed -i '' "1,/^version = \".*\"/s/^version = \".*\"/version = \"$NEW\"/" Cargo.toml
cargo update --workspace --quiet
if [ -z "${SKIP_TESTS:-}" ]; then
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test
fi
git commit --quiet -am "Release $NEW"

OUT="target/release/dist/$NEW"
mkdir -p "$OUT"
OUT_DIR="$OUT" scripts/bundle-macos.sh
FEED="$OUT/feed"
mkdir -p "$FEED"
cp "$OUT/Shoebox-$NEW.zip" "$FEED/"
"$SPARKLE/bin/generate_appcast" --account "$ACCOUNT" \
  --download-url-prefix "https://github.com/$REPO/releases/download/$TAG/" \
  --link "https://github.com/$REPO" \
  -o "$FEED/appcast.xml" "$FEED"
grep -q "sparkle:edSignature" "$FEED/appcast.xml" || die "the appcast isn't signed"

git tag -a "$TAG" -m "Shoebox $NEW"
git push --quiet origin main "$TAG"
gh release create "$TAG" --repo "$REPO" --verify-tag --title "Shoebox $NEW" \
  --generate-notes "$FEED/Shoebox-$NEW.zip" "$FEED/appcast.xml"
echo "Released $TAG: https://github.com/$REPO/releases/tag/$TAG"
