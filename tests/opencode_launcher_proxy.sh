#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
WRAPPER="$HOME/bin/opencode"
CAPTURE="$ROOT/tests/fixtures/opencode-launcher-capture.sh"
STALE="$ROOT/tests/fixtures/opencode-launcher-stale-proxy.sh"
INSTALLER="$ROOT/scripts/install-termux.sh"
TMP_HOME=$(mktemp -d)
NO_PROXY_HOME=$(mktemp -d)
trap 'rm -rf "$TMP_HOME" "$NO_PROXY_HOME"' EXIT

mkdir -p "$TMP_HOME/.codex"
cp "$STALE" "$TMP_HOME/.codex/xu-upstream-proxy.sh"
chmod 755 "$CAPTURE"

output=$(env -i \
  HOME="$TMP_HOME" \
  PATH="$ROOT/tests/fixtures:$HOME/bin:/data/data/com.termux/files/usr/bin" \
  TMPDIR="/data/data/com.termux/files/usr/tmp" \
  XU_REAL_OPENCODE_BIN="$CAPTURE" \
  HTTP_PROXY="http://127.0.0.1:7890" \
  HTTPS_PROXY="socks5h://127.0.0.1:7890" \
  ALL_PROXY="socks5h://127.0.0.1:7890" \
  "$WRAPPER" --version)

case "$output" in
  *'HTTPS_PROXY=socks5h://'*|*'ALL_PROXY=socks5h://'*|*'XU_UPSTREAM_PROXY_MODE=proxy'*)
    printf '%s\n' "$output" >&2
    printf '%s\n' 'opencode launcher leaked stale proxy state' >&2
    exit 1
    ;;
esac

proxy_output=$(env -i \
  HOME="$TMP_HOME" \
  PATH="$ROOT/tests/fixtures:$HOME/bin:/data/data/com.termux/files/usr/bin" \
  TMPDIR="/data/data/com.termux/files/usr/tmp" \
  XU_REAL_OPENCODE_BIN="$CAPTURE" \
  XU_OPENCODE_USE_UPSTREAM_PROXY=1 \
  "$WRAPPER" --version)

case "$proxy_output" in
  *'HTTPS_PROXY=socks5://'*'ALL_PROXY=socks5://'*'1.17.19'*) ;;
  *)
    printf '%s\n' "$proxy_output" >&2
    printf '%s\n' 'opencode launcher did not normalize the opt-in proxy' >&2
    exit 1
    ;;
esac

if unsupported_output=$(env -i \
  HOME="$NO_PROXY_HOME" \
  PATH="$ROOT/tests/fixtures:$HOME/bin:/data/data/com.termux/files/usr/bin" \
  TMPDIR="/data/data/com.termux/files/usr/tmp" \
  XU_REAL_OPENCODE_BIN="$CAPTURE" \
  XU_OPENCODE_USE_UPSTREAM_PROXY=1 \
  HTTPS_PROXY='ftp://127.0.0.1:7890' \
  "$WRAPPER" --version 2>&1); then
  printf '%s\n' "$unsupported_output" >&2
  printf '%s\n' 'opencode launcher accepted an unsupported proxy protocol' >&2
  exit 1
fi

case "$unsupported_output" in
  *'unsupported proxy protocol in HTTPS_PROXY'*) ;;
  *)
    printf '%s\n' "$unsupported_output" >&2
    printf '%s\n' 'opencode launcher did not explain the unsupported proxy protocol' >&2
    exit 1
    ;;
esac

case "$output" in
  *'HTTPS_PROXY='*'ALL_PROXY='*'XU_UPSTREAM_PROXY_MODE='*) ;;
  *)
    printf '%s\n' "$output" >&2
    printf '%s\n' 'opencode launcher did not produce a clean proxy environment' >&2
    exit 1
    ;;
esac

if grep -Fq 'added proxy source line' "$INSTALLER" || \
   grep -Fq '>> "$HOME/.bashrc"' "$INSTALLER" || \
   grep -Fq 'sed -i' "$INSTALLER"; then
  printf '%s\n' 'installer still injects proxy configuration into shell startup files' >&2
  exit 1
fi

if grep -Fq 'socks5h://' "$INSTALLER"; then
  printf '%s\n' 'installer still writes the unsupported socks5h protocol' >&2
  exit 1
fi

printf '%s\n' 'opencode launcher proxy isolation: PASS'
