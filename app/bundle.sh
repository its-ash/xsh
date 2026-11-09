#!/bin/sh
# Builds target/xsh.app. Usage: app/bundle.sh [debug|release] [rust-target]
set -eu

MODE="${1:-release}"
TARGET="${2:-}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/xsh.app"

rm -rf "$APP"

CARGO_FLAGS=""
SWIFT_FLAGS="-c $MODE"
RUST_DIR="$ROOT/target/$MODE"
[ "$MODE" = release ] && CARGO_FLAGS="--release"
if [ -n "$TARGET" ]; then
    CARGO_FLAGS="$CARGO_FLAGS --target $TARGET"
    SWIFT_FLAGS="$SWIFT_FLAGS --arch ${TARGET%%-*}"
    RUST_DIR="$ROOT/target/$TARGET/$MODE"
fi

# shellcheck disable=SC2086
(cd "$ROOT" && cargo build -p rushy-cli $CARGO_FLAGS)
# shellcheck disable=SC2086
(cd "$ROOT/app" && swift build $SWIFT_FLAGS)
# shellcheck disable=SC2086
SWIFT_BIN="$(cd "$ROOT/app" && swift build $SWIFT_FLAGS --show-bin-path)"

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$SWIFT_BIN/XshApp" "$APP/Contents/MacOS/XshApp"
cp "$RUST_DIR/xsh" "$APP/Contents/MacOS/xsh"

VERSION="$(awk -F'"' '/^version/ { print $2; exit }' "$ROOT/Cargo.toml")"
awk -v v="$VERSION" '{ gsub(/__VERSION__/, v); print }' "$ROOT/app/Info.plist" > "$APP/Contents/Info.plist"

# The icon is drawn from the SVG at build time, so no binary file is in the repository.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ICONSET="$WORK/xsh.iconset"
mkdir -p "$ICONSET"
qlmanage -t -s 1024 -o "$WORK" "$ROOT/assets/xsh.svg" > /dev/null 2>&1
for s in 16 32 128 256 512; do
    sips -z "$s" "$s" "$WORK/xsh.svg.png" --out "$ICONSET/icon_${s}x${s}.png" > /dev/null
    d=$((s * 2))
    sips -z "$d" "$d" "$WORK/xsh.svg.png" --out "$ICONSET/icon_${s}x${s}@2x.png" > /dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/xsh.icns"

# Ad hoc signature: enough to run on this Mac. Distribution needs a Developer ID.
codesign --force --sign - "$APP/Contents/MacOS/xsh"
codesign --force --sign - "$APP"

echo "$APP"
