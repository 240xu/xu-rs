# spec

`spec` 是一个面向 Termux 的单二进制工具：TUI 供应商管理器 + 本地协议路由器 + 三端（OpenCode / Claude Code / Codex）配置管理器。

通过统一供应商（provider）管理，把上游 API（如 OpenCode Zen 免费层、zen-go 付费层）接入 OpenCode / Claude Code / Codex 三端，并在本地做协议转换（Anthropic Messages / OpenAI Chat / OpenAI Responses 互通）。所有写操作都带 dry-run 预览、脱敏 diff、时间戳备份与回滚。

> ⚠️ 成本提醒：上游按模型独立计费。默认请只用便宜模型（如 zen 免费层 `deepseek-v4-flash-free`、zen-go 付费层 `deepseek-v4-flash`）。`claude-opus-5` / `claude-sonnet-5` / `gpt-5.6-sol` 等模型很贵，不要配进 `defaultModel`。

## 功能清单

- **供应商管理**：预设添加、测试连通、编辑、复制、删除、模型拉取、导入导出、应用到三端
- **协议路由**：`spec serve` 本地服务，Anthropic Messages / OpenAI Chat / OpenAI Responses 三协议互通（同协议直通透传，跨协议 IR 转换）
- **缓存双模式**：供应商级 `cacheMode` 配置（`auto` 按厂商自动识别 / `compat` 通用前缀缓存 / `deepseek` 特化：tools 稳定排序 + system 固定前置 + usage 归一化不双计）
- **MCP / Skills / Projects / Prompts**：统一 store + 三端非破坏式投影
- **Agent 安装更新**：OpenCode / Claude Code / Codex 的 Termux 环境诊断、安装、升级
- **会话浏览**：归一化浏览三端本地会话记录
- **runit 自启**：`spec serve` 由 termux-services 托管，开机自动运行

## 快速开始（Termux）

```sh
# 1. 构建
cd xu-rs
cargo build --release
cp target/release/spec $PREFIX/bin/spec

# 2. 环境诊断 + 安装三端 Agent（可选）
spec agent doctor
spec agent setup --yes        # 安装/更新 opencode、claude、codex

# 3. 自启本地协议服务（termux-services）
# 已提供 runit 服务：/var/service/spec-serve
sv up spec-serve              # 启动；sv status spec-serve 查看；sv down 停止
```

### 直接使用脚本安装

```sh
sh scripts/install-termux.sh
spec agent doctor
spec agent setup --yes
# 或一步完成：./install-termux.sh --setup-agents
```

## 供应商接入示例（zen 免费层 / zen-go 付费层）

```sh
# zen 免费层（零成本）
spec provider add zen \
  --kind chat --name "OpenCode Zen (free)" \
  --base-url https://opencode.ai/zen/v1 \
  --model deepseek-v4-flash-free --model mimo-v2.5-free
# 免费层无需 apiKey；也可以直接 `--preset opencode-zen`

# zen-go 付费层（用便宜模型，默认 deepseek-v4-flash，并开 deepseek 缓存模式）
spec provider add zen-go \
  --kind chat --name "zen-go" \
  --base-url https://opencode.ai/zen/go/v1 \
  --model deepseek-v4-flash
spec provider update zen-go --default-model deepseek-v4-flash
# 缓存模式在配置文件 ~/.codex/xu-chat-providers.json 的 options.cacheMode 设置：
#   "options": { "baseURL": "https://opencode.ai/zen/go/v1", "apiKey": "...", "cacheMode": "deepseek" }

# 测试连通并应用到三端
spec test zen
spec use zen --target opencode
spec use zen --target claude      # 需要 spec serve 运行
spec use zen --target codex       # 需要 spec serve 运行
```

应用方式：OpenCode 直写 `opencode.json`；Claude / Codex 按需直连或走本地 `spec serve` 路由（多供应商或跨协议时必走）。

## CLI 速查

```sh
spec                          # 打开 TUI
spec list                     # 列出供应商
spec current opencode         # 查看目标当前供应商
spec use zen --target codex --dry-run   # 预览切换（不写文件）
spec test zen                 # 测试连通（显式请求才发起网络请求）
spec doctor                   # 路径与环境诊断

spec provider preset list                     # 内置模板（19 个：openai/deepseek/gemini/glm/qwen/…）
spec provider add myid --preset openrouter --api-key sk-...
spec provider models zen                      # 拉取 /models 列表
spec provider models zen --apply --yes        # 写回供应商配置
spec provider update zen --default-model model-a
spec provider show zen        # 查看（密钥脱敏）
spec provider export          # 导出（默认脱敏，--include-secrets 才含密钥）
spec provider import ./providers.json --replace
spec provider delete zen --yes
spec provider sync-opencode   # 从 opencode.json 导入供应商

spec agent doctor             # 诊断三端环境
spec agent status --latest    # 版本与更新状态
spec agent install all --yes  # 安装/更新三端
spec opencode permission allow|ask|deny   # OpenCode 全局权限

spec mcp list / mcp add / mcp enable|disable / mcp import / mcp apply / mcp delete
spec prompt list / add / edit / import / activate / deactivate / delete
spec skill list / import / enable|disable / verify / install-zip / install-github / update / uninstall
spec project list / create / capture / plan / switch / rename / delete
spec session list / delete
spec backup list / show / restore / prune --keep 5 --yes

spec runtime status|start|stop   # 本地协议服务守护进程管理
spec serve                       # 前台运行本地协议服务（127.0.0.1:9316）
spec config path                 # 显示所有配置文件路径
```

> 多数写入类命令默认 dry-run 预览，需显式 `--yes` 才真正写盘；写前自动备份（`*.xu.<时间戳>.bak`，0600 权限），失败自动回滚。

## 常见问题（FAQ）

**问：Claude Code 一直要求登录？**
答：应用 Claude 时 spec 写的是 `ANTHROPIC_AUTH_TOKEN`（空 key 供应商用占位 `local-proxy` 跳过登录门禁，有 key 则写真实 key），不写 `ANTHROPIC_API_KEY`。若仍要求登录，检查 `~/.claude/settings.json` 的 env 是否被外部覆盖，或运行 `spec agent doctor` 看是否有 `ANTHROPIC_*` 环境变量残留。

**问：`/compact` 报 400？**
答：`/compact` 是 Claude Code 命令，报错通常来自上游接口（限流/参数不支持）。spec 会把上游返回的 `error.message` 原样透传——看报错正文是哪个上游、什么原因，而非 spec 本身故障。

**问：模型不被识别（"cannot map model … to a configured provider"）？**
答：路由按 `<模型名>_<供应商id>` 格式映射（如 `deepseek-v4-flash-free_zen`），或单供应商时直传模型名。模型名必须是供应商 `models` 里配置过的；`[1m]`/`[1M]` 上下文后缀会被自动剥离。三端里改模型请用 spec 应用过的模型名，或运行 `spec provider models <id> --apply --yes` 拉取最新列表。

**问：runit 服务怎么管理？**
```sh
sv status spec-serve
sv up spec-serve      # 启动
sv down spec-serve    # 停止
sv restart spec-serve # 重启（改配置后）
```
服务日志：`/data/data/com.termux/files/usr/var/log/spec-serve.err`。也可以不用 runit，直接 `spec runtime start`（写 pidfile 的后台守护）。

**问：密钥安全？**
答：dry-run diff、`spec provider show`、`spec provider export` 均默认脱敏密钥；导出需显式 `--include-secrets`。TUI 导出永远脱敏。

## 开发

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
sh scripts/package.sh     # 产物在 dist/
```

详细使用指南见 [docs/USER_GUIDE.md](docs/USER_GUIDE.md)。

## v2 新增能力（2026-08-13）

### Token 统计与缓存命中率
- 每次请求自动记录 input/output/cached tokens（`~/.codex/stats/tokens-YYYY-MM-DD.jsonl`）
- 统计页展示 **24h / 48h / 7d / 30d** 四档位 + 缓存命中率
- 右下角标记「⚠ passthrough 直通流量为近似统计」（同协议直通流式的 usage 为尽力扫描）
- 查询：`GET /api/stats/tokens?period=24h|48h|7d|30d`（本地）

### 跨端同步
- `spec sync skills <源> <目标...> [--dry-run]`：Skill 任意源→目标镜像（opencode/claude/codex，零转换）
- `spec sync mcp <源> <目标...> [--dry-run]`：MCP 全部 server 跨端同步（command/env/headers/type 自动转换）
- TUI「同步」页：选源 → 多选目标 → 预览 diff → 执行

### 快照保存（原「项目」）
- `spec snapshot save|list|switch|delete|overwrite`（`project` 为兼容别名）
- 捕获「供应商配置 + MCP + Skill + 提示词」整套状态，一键切换；旧 `xu-projects.json` 自动迁移到 `xu-snapshots.json`

### 新 UI（Web 思想 × 终端）
- 顶部 Tab 导航：供应商 / 统计 / 同步 / 快照 / 运行时 / 工具（按钮式，鼠标/触摸/键盘均可用）
- 搜索框（圆角线框 + ⌕ 实时过滤）、卡片列表、单·多供应商分开选择
- 更新入口：仅在检测到新版本时显示「更新 xxx」
