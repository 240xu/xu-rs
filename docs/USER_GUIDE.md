# spec 使用指南

## 1. 概览

`spec` 做三件事：

1. **供应商管理**：把上游 API（zen 免费层、zen-go 付费层、OpenRouter、DeepSeek 官方等）登记为统一的 provider，含连接测试、模型拉取、编辑、导入导出。
2. **三端应用**：把选定的供应商写到 OpenCode / Claude Code / Codex 的配置文件；Claude 与 Codex 在跨协议或多供应商时由本地 `spec serve` 路由。
3. **协议路由**：`spec serve` 监听 `127.0.0.1:9316`，在 Anthropic Messages / OpenAI Chat / OpenAI Responses 三种 wire 协议间转换。

### 数据文件（均在 `~/.codex/` 下）

| 文件 | 用途 |
|---|---|
| `xu-chat-providers.json` | 供应商 store（唯一事实源） |
| `xu-state.json` | 每目标当前供应商、健康缓存、权限状态 |
| `xu-projects.json` | 项目快照 store |
| `xu-mcp.json` | MCP 统一 store |
| `xu-prompts.json` | 提示词预设 store |
| `xu-skills.json` + `xu-skills/` | Skills store 与内容目录 |
| `xu-client-routes.json` | Claude/Codex 多供应商路由（OpenCode 不用） |
| `spec-runtime.pid` / `spec-runtime.log` | `spec runtime start` 守护进程 |

三端目标文件：OpenCode `~/.config/opencode/opencode.json`（+`AGENTS.md`）、Claude `~/.claude/settings.json`（+`CLAUDE.md`）、Codex `~/.codex/config.toml`（+`AGENTS.md`、`model-catalogs/xucodex-catalog.json`）。

### 供应商配置格式

```json
{
  "provider": {
    "zen": {
      "apiKind": "chat",
      "name": "OpenCode Zen (free)",
      "options": {
        "baseURL": "https://opencode.ai/zen/v1",
        "cacheMode": "deepseek",
        "timeout": 60000,
        "maxRetries": 10,
        "customHeaders": { "X-App": "xu" }
      },
      "models": {
        "ds-v4": { "name": "DeepSeek V4", "requestName": "deepseek-v4-flash-free" }
      },
      "defaultModel": "ds-v4",
      "website": "…", "notes": "…"
    }
  }
}
```

- `apiKind`：`chat`（OpenAI Chat 兼容）/ `responses`（OpenAI Responses 兼容）/ `anthropic`（Anthropic Messages 兼容）
- `options.apiKey` 可为空字符串——zen 免费层无 key 可用（Claude Code 端会用占位 AUTH_TOKEN 跳过登录门禁）
- `models` 支持对象（带元数据）/ 字符串数组 / 字符串与对象混合数组；`defaultModel` 缺省取第一个模型

## 2. 供应商管理

### 添加

```sh
spec provider add zen --kind chat --name "zen" \
  --base-url https://opencode.ai/zen/v1 \
  --model deepseek-v4-flash-free --model mimo-v2.5-free
# 或从内置模板起步（19 个：openai-chat/openai-responses/openrouter/deepseek/moonshot/anthropic/gemini/zhipu-glm/qwen/minimax/siliconflow/mistral/groq/xai/together/nvidia-nim/volcengine-ark/ollama/opencode-zen）
spec provider preset list
spec provider add myid --preset openrouter --api-key sk-...
```

TUI：主菜单 →「供应商」→ `a` 添加（或点 + 号），可先选预设再填空；触摸终端需先点文本框再调出软键盘输入。

### 测试

```sh
spec test zen
```

显式请求才发网络请求；结果（ok / latency / 脱敏 message）缓存到 `xu-state.json`，列表页展示但不主动探测。TUI 供应商列表按 `t` 测试。

### 编辑

```sh
spec provider update zen --name "Zen" --default-model model-a \
  --timeout 60000 --max-retries 10 --context-window 128000 \
  --max-output-tokens 32768 --reasoning-effort high \
  --header "X-App: xu"          # 头用 Name: Value；--clear-headers 清空
spec provider update zen --models-json '{"a":{},"b":{"name":"B","requestName":"b-up"}}'
spec provider duplicate zen zen-backup --name "Zen Backup"
```

TUI：供应商列表 `e` 进入分页编辑器（Connection / Models / Performance / Capabilities / Profile 五页），数值字段有 `[-]`/`[+]` 控件。底部固定「保存预览」按钮，保存前仍是脱敏 dry-run + 确认。

### 模型拉取

```sh
spec provider models zen                  # 拉取 /v1/models（404 时回退 /models）
spec provider models zen --apply --yes    # 写回 models（保留已有模型的元数据，defaultModel 失效时取第一个）
spec provider fetch-models zen --apply    # 同上，可加 --timeout N
```

Anthropic 兼容供应商会拒绝自动拉取（避免误触发计费接口）。TUI：`m` 拉取（编辑器内也有 Fetch 按钮）。

### 删除

```sh
spec provider delete zen --yes
```

正在被任何目标使用的供应商拒绝删除；`--dry-run` 只预览。

### 导入导出

```sh
spec provider export                    # 默认脱敏（apiKey/token/secret 等 → <redacted>）
spec provider export --include-secrets  # 显式才含密钥
spec provider import ./providers.json --dry-run   # 校验后合并
spec provider import ./providers.json --replace   # 允许覆盖同 id
spec provider sync-opencode             # 从 opencode.json 导入 @ai-sdk/openai-compatible 供应商（--预览 只预览）
```

TUI 导出永远脱敏；TUI 导入只读 `~/storage/downloads/xu-provider-import.json`，预览确认后以 `--replace` 语义应用。

## 3. 模型映射（显示名 / 请求名 / 1M 槽位）

供应商 `models` 里每条目可以是一个对象：

```json
"ds-v4": { "name": "DeepSeek V4", "requestName": "deepseek-v4-flash-free" }
```

- **显示名**（`name`）：三端菜单里展示的名字
- **请求名**（`requestName`）：真正发给上游的模型 id（缺省等于条目的 key）
- 纯字符串条目三者相同

路由时模型名格式为 `<模型>_<供应商id>`（如 `ds-v4_zen`）；仅配置一个供应商时也可直接传模型名。`[1m]` / `[1M]` 上下文后缀（Claude Code 长上下文槽位习惯写法）在转发前自动剥离，即 `deepseek-v4-flash-free_zen[1M]` 会被解析为 `deepseek-v4-flash-free` + zen。未知模型名返回 `cannot map model … to a configured provider`（400），不会静默乱发。

## 4. 缓存模式（cacheMode）

供应商级配置 `options.cacheMode`（或顶层 `cacheMode`），三种取值：

| 模式 | 含义 |
|---|---|
| `auto`（默认） | 按供应商自动识别：deepseek 系厂商走 DeepSeek 特化，其余按通用处理 |
| `compat` | OpenAI 风格宽松前缀缓存，标准编码，兼容性优先 |
| `deepseek` | DeepSeek 前缀单元完整匹配特化：tools 数组按 name 稳定排序、system 固定前置、usage 归一化（`prompt_cache_hit_tokens`→`cached_tokens`）且输入不双计，最大化缓存命中 |

未知取值回退 `auto`。缓存模式只在跨协议转换到 OpenAI Chat 目标时注入特化标记；同协议直通时原样透传。典型用法：zen-go 付费层（OpenAI Chat 兼容）配 `deepseek` 模式省钱。

## 5. 应用到三端

```sh
spec use zen --target opencode          # 直写 opencode.json
spec use zen --target claude            # 直连或走 spec serve
spec use zen --target codex             # 直连或走 spec serve
spec use zen --target opencode --dry-run
```

### OpenCode

- 直写 `~/.config/opencode/opencode.json`，不配置本地代理
- `chat` 供应商 → `@ai-sdk/openai-compatible`；`responses` → `@ai-sdk/openai`
- `anthropic` 供应商拒绝（OpenCode 不常见），不会偷偷走路由
- 只更新目标供应商与激活模型，保留无关供应商、`permission`、`agent` 等设置
- 全局权限：TUI Client 页 → 设置，或 `spec opencode permission allow|ask|deny`
- **bypassPermissions**：供应商编辑器右下角开关，写 `permissions.defaultMode = "bypassPermissions"`（跳过工具执行确认；用便宜模型的自动任务常见）

### Claude Code

- 写 `~/.claude/settings.json` env：`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`（不写 `ANTHROPIC_API_KEY`；空 key 供应商用占位 `local-proxy` 跳过登录门禁）+ `ANTHROPIC_MODEL`
- 单供应商且 anthropic 兼容 → 可直连；OpenAI 兼容或多供应商 → 走 `spec serve`（`http://127.0.0.1:9316/v1/messages`）
- 使用时需 `spec serve` 保持运行（runit 服务已托管）

### Codex

- 写 `~/.codex/config.toml` + `model-catalogs/xucodex-catalog.json`
- 单供应商 responses 兼容 → 可直连；chat/anthropic 兼容或多供应商 → 走 `spec serve`（`/v1/responses`）
- Codex 客户端自有的 `custom` / `namespace` / `web_search` 工具类型无 Chat 表示，会被过滤（当前适配契约内不保证原生工具全等）

### 多供应商模型路由

TUI 供应商页 `g`（或自绘 relay 控件）：选 ≥2 个供应商排序 → 选 Claude 或 Codex。Codex 收到含全部模型的 catalog；Claude 默认第一个供应商，其余加后缀模型经 runtime 路由。这是显式路由，不是自动故障转移。

## 6. 本地协议路由（spec serve）

```sh
spec serve                     # 前台运行（127.0.0.1:9316）
spec runtime status|start|stop # 后台守护（pidfile 方式）
sv up spec-serve               # runit 自启（termux-services 托管，推荐）
```

能力：

- 非流式与可表示路由的流式转换：Anthropic Messages ↔ OpenAI Chat ↔ OpenAI Responses
- 同协议直通（如 `/v1/messages` → anthropic 供应商）原样透传
- 转换工具调用与 `tool_choice`，但**不执行**模型请求的工具（执行是客户端责任）
- 报错透传：上游错误体的 `error.message` 原文转发给客户端（如 `/compact` 的 400 可见真实原因）
- 保守策略：无法安全表示的协议特性显式报错，绝不静默产出非法请求

## 7. MCP / Skills / Projects / Prompts

### MCP（统一 store → 三端投影）

```sh
spec mcp list
spec mcp add memory --transport stdio --command npx \
  --arg -y --arg @modelcontextprotocol/server-memory \
  --target opencode --target claude --yes
spec mcp preset list                 # 内置模板：Memory/Fetch/Time/Sequential Thinking
spec mcp enable|disable <id> --target <app> --yes
spec mcp import --target opencode --yes    # 从某端导入现有定义（含冲突检测）
spec mcp apply --target all --yes          # 重放投影
spec mcp delete <id> --yes
```

只投影 spec 管理的 server id 到三端（OpenCode `mcp`、Claude `~/.claude.json.mcpServers`、Codex `[mcp_servers]`），不碰无关配置；非法 JSON/TOML fail-closed。所有变更默认 dry-run，`--yes` 才写。

### Skills

```sh
spec skill import audit --path ./audit --method symlink --yes
spec skill enable|disable <id> --target <app> --yes
spec skill verify                      # 校验内容哈希
spec skill install-zip <id> --path ./s.zip --yes
spec skill install-github <id> --owner o --repo r --yes
spec skill update <id> / uninstall <id> --yes
spec skill backups / restore <backup-id> --yes
```

导入目录必须含根 `SKILL.md`；记录 SHA-256 哈希，`verify` 可查改动。投影到 `~/.config/opencode/skills`、`~/.claude/skills`、`~/.codex/skills`；删除只动有所有权标记的投影，非托管目录永不删。

### Prompts（全局提示词预设）

```sh
spec prompt add team --name "Team Rules" --file ./AGENTS.md --yes
spec prompt edit team --file ./new.md --yes
spec prompt activate team --target opencode --yes
spec prompt deactivate --target codex --yes
spec prompt import --target claude --id current-claude --yes   # 从 live 文件导入
spec prompt delete team --yes        # 激活中的预设禁止删除
```

live 文件：OpenCode `AGENTS.md`、Claude `CLAUDE.md`、Codex `AGENTS.md`。切换/停用前会把 live 文件的外部手工修改回填进旧预设（不丢改动）。

### Projects（整态快照）

```sh
spec project create dev --name "开发" --yes
spec project capture dev --target opencode --yes    # 捕获某端当前状态
spec project plan dev --target codex                 # 切换预览（自动保存被离开项目）
spec project switch dev --target codex --yes         # 事务切换：供应商+MCP+Prompts+Skills+current 一个事务，失败回滚
spec project rename dev --name "开发2" --yes
spec project delete dev --yes                        # 当前项目受保护
```

capture 只改 store 不改实时配置；应用 Project 至少需要一个供应商（供应商停用尚无安全的目标级行为）。

## 8. 安全边界

- **fail-closed**：配置解析遇到未知字段/非法 JSON 显式报错，绝不当作空文件继续；无法转换的协议特性返回明确错误
- **密钥不落日志**：dry-run diff、`show`、`export` 对 `apiKey` / `api_key` / `token` / `secret` / `authorization` 脱敏；TUI 导出永远脱敏
- **写前备份**：所有真实写入先备份（`*.xu.<时间戳>.bak`，0600），多文件应用失败自动回滚已写部分
- **读前校验**：目标配置文件必须可读 UTF-8，否则拒绝生成写计划
- **删除保护**：当前激活的供应商/项目、激活中的提示词预设禁止删除；备份 prune 只认 `*.xu.*.bak` 且默认 dry-run
- **下载导入限制**：TUI 导入仅读 `~/storage/downloads/xu-provider-import.json`，预览确认后才应用
- `spec doctor` 会警告可能覆盖配置的 `ANTHROPIC_*` / `OPENAI_*` / `CODEX_HOME` 环境变量
