#!/data/data/com.termux/files/usr/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PREFIX_DIR=${PREFIX:-/data/data/com.termux/files/usr}
SPEC="$PREFIX_DIR/bin/spec"

if [ ! -x "$ROOT/spec" ]; then
  printf 'error: spec binary not found next to install-termux.sh\n' >&2
  exit 1
fi

mkdir -p "$PREFIX_DIR/bin"
if [ -e "$PREFIX_DIR/bin/spec" ]; then
  cp "$PREFIX_DIR/bin/spec" "$PREFIX_DIR/bin/spec.before-release-install.bak"
fi
install -m 0755 "$ROOT/spec" "$PREFIX_DIR/bin/spec"

printf 'installed spec to %s\n' "$PREFIX_DIR/bin/spec"

if [ "${1:-}" = "--setup-agents" ]; then
  exec "$SPEC" agent setup --yes
fi

if [ "${1:-}" = "--setup-zen" ] || [ "${1:-}" = "--setup-zen-only" ]; then
  PROXY_ACTIVE="0"
  if curl -fsS -m 3 -x http://127.0.0.1:7890 https://opencode.ai/zen/v1/models >/dev/null 2>&1; then
    PROXY_ACTIVE="1"
  fi
  if [ "$PROXY_ACTIVE" = "0" ]; then
    printf 'error: cannot reach opencode.ai/zen through proxy 127.0.0.1:7890\n' >&2
    printf 'hint: start your local proxy on 7890 first (mihomo/clash etc.), then re-run.\n' >&2
    exit 1
  fi

  if [ ! -f "$HOME/.codex/xu-chat-providers.json" ]; then
    mkdir -p "$HOME/.codex"
    printf '{\n  "provider": {}\n}\n' > "$HOME/.codex/xu-chat-providers.json"
  fi
  if ! "$SPEC" provider show zen >/dev/null 2>&1; then
    printf 'adding zen provider...\n'
    "$SPEC" provider add zen --preset opencode-zen
  else
    printf 'zen provider already exists\n'
  fi

  mkdir -p "$HOME/.codex"
  cat > "$HOME/.codex/xu-upstream-proxy.sh" <<'PROXYEOF'
export XU_UPSTREAM_PROXY_MODE="proxy"
export HTTP_PROXY="http://127.0.0.1:7890"
export HTTPS_PROXY="http://127.0.0.1:7890"
export ALL_PROXY="socks5://127.0.0.1:7890"
export http_proxy="$HTTP_PROXY"
export https_proxy="$HTTPS_PROXY"
export all_proxy="$ALL_PROXY"
PROXYEOF
  printf 'wrote proxy config to ~/.codex/xu-upstream-proxy.sh\n'

  printf 'proxy config is saved for explicit Xu runtime use; OpenCode stays direct by default\n'

  printf 'applying zen to opencode/codex/claude...\n'
  "$SPEC" use zen --target opencode || true
  "$SPEC" use zen --target codex || true
  "$SPEC" use zen --target claude || true

  printf 'starting runtime...\n'
  "$SPEC" runtime start || true
  sleep 1

  printf '\n=== verification ===\n'
  "$SPEC" test zen || true
  curl -s -m 5 "http://127.0.0.1:9316/health" | head -c 200 || true
  printf '\n'
  printf '\ndone. default model on all three clients: deepseek-v4-flash-free (opencode zen)\n'
  exit 0
fi

printf 'to install/update all three agents, run:\n  spec agent setup --yes\n'
printf 'to install spec + configure zen on all three clients, run:\n  ./install-termux.sh --setup-zen\n'
