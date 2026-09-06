#!/data/data/com.termux/files/usr/bin/bash
# Package xcc release tarballs for distribution.
set -euo pipefail

NAME="xcc"
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')

# Detect target triple
case "$(uname -m)" in
  aarch64|arm64) ARCH="aarch64" ;;
  x86_64|amd64)  ARCH="x86_64" ;;
  *)             ARCH="$(uname -m)" ;;
esac
case "$(uname -o 2>/dev/null || uname -s)" in
  *Android*) OS="android" ;;
  *Linux*)   OS="linux" ;;
  Darwin)    OS="darwin" ;;
  *)         OS="$(uname -s | tr '[:upper:]' '[:lower:]')" ;;
esac
TARGET="${OS}-${ARCH}"

DIST="dist"
STAGE="${DIST}/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE/${NAME}-${VERSION}-${TARGET}/bin"

cp "target/release/${NAME}" "$STAGE/${NAME}-${VERSION}-${TARGET}/bin/${NAME}"
chmod 0755 "$STAGE/${NAME}-${VERSION}-${TARGET}/bin/${NAME}"

# Include user-facing docs if present
for f in README.md LICENSE README_ZH.md; do
  [ -f "$f" ] && cp "$f" "$STAGE/${NAME}-${VERSION}-${TARGET}/" || true
done

mkdir -p "$DIST"
OUT="${NAME}-${VERSION}-${TARGET}.tar.gz"
tar -czf "${DIST}/${OUT}" -C "$STAGE" "${NAME}-${VERSION}-${TARGET}"

rm -rf "$STAGE"
sha256sum "${DIST}/${OUT}"
