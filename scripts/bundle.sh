#!/bin/sh
# Builds LensBlitz.app.
#
#   scripts/bundle.sh                 build for this Mac        -> dist/LensBlitz.app
#   scripts/bundle.sh --universal     Apple Silicon + Intel     -> dist/LensBlitz.app
#   scripts/bundle.sh --binary PATH   wrap an already built binary (used by the Homebrew formula)
#   scripts/bundle.sh --out DIR       put LensBlitz.app in DIR instead of dist/
set -eu
cd "$(dirname "$0")/.."

universal=0
binary=""
out="dist"
while [ $# -gt 0 ]; do
	case "$1" in
		--universal) universal=1 ;;
		--binary) binary="$2"; shift ;;
		--out) out="$2"; shift ;;
		-h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
		*) echo "bundle.sh: unknown option $1" >&2; exit 2 ;;
	esac
	shift
done

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

if [ -z "$binary" ]; then
	if [ "$universal" -eq 1 ]; then
		for target in aarch64-apple-darwin x86_64-apple-darwin; do
			if ! rustup target list --installed 2>/dev/null | grep -q "^$target\$"; then
				echo "bundle.sh: missing Rust target; run: rustup target add $target" >&2
				exit 1
			fi
			cargo build --release --locked --target "$target"
		done
		mkdir -p target/universal
		binary="target/universal/lensblitz"
		lipo -create -output "$binary" \
			target/aarch64-apple-darwin/release/lensblitz \
			target/x86_64-apple-darwin/release/lensblitz
	else
		cargo build --release --locked
		binary="target/release/lensblitz"
	fi
fi

app="$out/LensBlitz.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/lensblitz"
chmod 755 "$app/Contents/MacOS/lensblitz"
cp assets/icon/LensBlitz.icns "$app/Contents/Resources/LensBlitz.icns"
sed "s/@VERSION@/$version/g" macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# Ad-hoc signature: enough to run locally (Apple Silicon refuses unsigned code).
# Distribution to other Macs needs a Developer ID signature and notarisation.
codesign --force --sign - "$app" >/dev/null 2>&1 || echo "bundle.sh: warning: codesign failed" >&2

echo "built $app ($(lipo -archs "$app/Contents/MacOS/lensblitz" 2>/dev/null || echo unknown), v$version)"
