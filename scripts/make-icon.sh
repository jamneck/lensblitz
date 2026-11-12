#!/bin/sh
# Regenerates assets/icon/LensBlitz.icns and assets/icon/icon-256.png from assets/icon/icon.svg.
# Only needed after editing the SVG; the generated files are checked in.
set -eu
cd "$(dirname "$0")/.."
iconset="$(mktemp -d)/LensBlitz.iconset"
cargo run --quiet --release --example mkicon -- assets/icon/icon.svg "$iconset"
iconutil --convert icns --output assets/icon/LensBlitz.icns "$iconset"
cp "$iconset/icon_256x256.png" assets/icon/icon-256.png
rm -rf "$(dirname "$iconset")"
echo "wrote assets/icon/LensBlitz.icns and assets/icon/icon-256.png"
