#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

VERSION=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
TARGET=${TARGET:-"${OS}-${ARCH}"}
if [ -n "${ANDROID_ROOT:-}" ] || [ -n "${TERMUX_VERSION:-}" ]; then
  TARGET="android-${ARCH}"
fi
NAME="xcc-${VERSION}-${TARGET}"
DIST="$ROOT/dist"
STAGE="$DIST/$NAME"

cargo build --release

rm -f "$DIST"/xcc-*.tar.gz "$DIST"/SHA256SUMS
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$ROOT/target/release/xcc" "$STAGE/xcc"
cp "$ROOT/README.md" "$STAGE/README.md"
cp "$ROOT/LICENSE" "$STAGE/LICENSE"
cp "$ROOT/CHANGELOG.md" "$STAGE/CHANGELOG.md"
cp "$ROOT/IMPLEMENTATION_PLAN.md" "$STAGE/IMPLEMENTATION_PLAN.md"
cp "$ROOT/scripts/install-termux.sh" "$STAGE/install-termux.sh"
chmod 0755 "$STAGE/install-termux.sh"

tar -C "$DIST" -czf "$DIST/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$DIST" && sha256sum "$NAME.tar.gz" > SHA256SUMS)
elif command -v shasum >/dev/null 2>&1; then
  (cd "$DIST" && shasum -a 256 "$NAME.tar.gz" > SHA256SUMS)
else
  printf 'error: no sha256 tool found; checksum not written\n' >&2
  exit 1
fi

printf 'created %s\n' "$DIST/$NAME.tar.gz"
