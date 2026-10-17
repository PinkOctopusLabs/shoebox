#!/usr/bin/env bash
# Download the pinned Sparkle release (the macOS updater framework and its
# tools) into target/sparkle/<version>, checking its SHA-256, and print that
# directory. Re-runs reuse the download.
#
#   SPARKLE="$(scripts/fetch-sparkle.sh)"
#   "$SPARKLE/bin/generate_keys" --account shoebox
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="2.10.0"
# From the release's asset digest on GitHub.
SHA256="c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c"
DIR="target/sparkle/$VERSION"

if [ ! -d "$DIR/Sparkle.framework" ]; then
  mkdir -p "$DIR"
  TARBALL="$DIR.tar.xz"
  curl -fsSL -o "$TARBALL" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$VERSION/Sparkle-$VERSION.tar.xz"
  echo "$SHA256  $TARBALL" | shasum -a 256 -c - >/dev/null
  tar -xJf "$TARBALL" -C "$DIR"
fi
echo "$PWD/$DIR"
