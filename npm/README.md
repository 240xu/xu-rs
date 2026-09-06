# @240xu/trivium

**trivium** — 三协议 LLM 路由中枢，带双界面控制台（TUI + Web）。

*trivium*：拉丁语 *tri-*（三）+ *via*（路），三条路交汇之处。

- **协议路由**：Anthropic Messages / OpenAI Chat Completions / OpenAI Responses 三协议互通（同协议直通透传，跨协议 IR 转换）
- **双界面**：TUI（ratatui）与 Web 控制台
- **三端管理**：OpenCode / Claude Code / Codex 的供应商配置、安装与升级
- **MCP / Skills / Projects / Prompts**：统一 store + 非破坏式投影
- **安全默认**：所有写操作带 dry-run 预览、脱敏 diff、时间戳备份与回滚

## 安装

```sh
npm i -g @240xu/trivium
trivium --version
trivium              # 打开 TUI
```

`xcc` 与 `spec` 作为兼容别名保留，既有脚本与 runit 服务无需改动。

当前提供 **Termux / Android aarch64** 预编译二进制。其他平台请从源码构建：
<https://github.com/240xu/xu-rs>（`cargo build --release`，产物 `target/release/trivium`）。

## 成本提醒

上游按模型独立计费，默认请只用便宜模型。不要用昂贵的 Opus / GPT 级模型作为 `defaultModel`。

## License

MIT — <https://github.com/240xu/xu-rs>
