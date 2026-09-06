# @240xu/xcc

`xcc-switch` (alias `spec`) — three-protocol LLM router with dual-surface console (TUI + Web).

- Routes OpenAI Chat Completions / Anthropic Messages / Responses protocols
- Manages providers for OpenCode, Claude Code, and Codex
- Installs and patches agent CLIs for Termux/Android and Linux

## Install

```sh
npm i -g @240xu/xcc
xcc --version   # or: spec --version
```

Currently ships a prebuilt binary for **Termux/Android aarch64**.
Other platforms: build from source — https://github.com/240xu/xu-rs (`cargo build --release`,
binary is `target/release/xcc`).

## License

MIT — https://github.com/240xu/xu-rs
