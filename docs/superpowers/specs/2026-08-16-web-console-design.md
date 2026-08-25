# Web 控制台（与 TUI 共享后端）设计说明

日期：2026-08-16
状态：已批准（用户）→ 实施中

## 目标

为 spec 提供 Web 管理面板：TUI 按 `W` 起/停 Web 面板并显示 URL；Web 面板「切回 TUI」按钮停 server 回终端；`spec web` 独立运行。两者共用同一进程内的命令后端（`cli::run_command`），协议转换层（src/runtime/**、providers/**、domain/**、agents/**）**零改动**。

## 架构

```
同一进程（spec 二进制）
├── TUI（默认界面，ratatui）
├── WebServer（src/web/，手写 HTTP，复用 runtime 的 HttpRequest/响应模式）
│    ├── REST JSON API → cli::run_command(结构化封装)
│    └── 内嵌静态前端（include_str! 嵌入 HTML/CSS/JS）
└── 切换：TUI `W` 起/停 · Web `POST /api/web/stop`（仅 TUI 起模式）· Esc 或再按 W 返回
```

- 端口：默认 `127.0.0.1:8123`（`XU_WEB_PORT` 可配；runtime serve 默认 9316 已避开）
- 协议转换层不新增任何依赖、不修改任何文件

## API 路由（JSON）

| 路由 | 方法 | 行为 |
|---|---|---|
| `/` | GET | 静态 index.html |
| `/static/*` | GET | 内嵌 CSS/JS |
| `/api/overview` | GET | 三端 agent 状态（doctor 摘要）+ runtime 健康（is_running） |
| `/api/providers` | GET | provider 列表（结构化 JSON：id/name/protocol/models/defaultModel/health） |
| `/api/command` | POST | `{args: [...]}` → 调 `cli::run_command` → `{ok: bool, output: string}` |
| `/api/mcp` | GET | MCP servers 列表 |
| `/api/skills` | GET | skills 列表 |
| `/api/stats` | GET | 用量统计（stats::aggregate → 含缓存命中率） |
| `/api/sessions` | GET | 会话列表（list_sessions，封顶 200 条） |
| `/api/web/stop` | POST | 设置 stop 标志（仅 TUI 拉起模式；独立 `spec web` 返回 400） |

错误约定：非 2xx 返回 `{"ok": false, "error": "..."}`；任何响应体**禁止**包含 API key / Authorization / 完整配置文件。

## 前端

单页深色管理面板（无构建工具，原生 HTML/JS/CSS + fetch）：
- 顶部分段导航：供应商 / MCP / Skills / Agent / 用量
- 供应商：卡片列表（名称/协议/模型数/状态），`→ 详情` 走 `/api/command`（provider show）文本视图；操作按钮复用 command 透传（test/models/use）
- MCP / Skills：列表 + 状态 + command 操作
- Agent：三端状态卡片 + 通用命令
- 用量：`/api/stats` 表格（24h/48h/7d/30d × 输入/输出/缓存读/缓存写/命中率/成功率/请求数）
- 右上角「切回 TUI」按钮 → `/api/web/stop`
- 空态/错误态友好文案；加载失败提示

## 切换与状态机

- `W` 键（顶层页：Menu/Provider/Client/Mcp/Skills/Mode::WebNotice）：
  - 未运行 → 起 server（线程）→ `Mode::WebNotice` 显示 `http://127.0.0.1:<port>` + 提示（再按 W 或 Esc 停止返回）
  - 运行中（WebNotice 页）→ 停 server → 回原模式
- Web「切回 TUI」：`POST /api/web/stop` → server 记录 stopped → server 线程退出 → TUI 事件循环检测到 server 退出 → 自动回原模式（need_redraw）
- `spec web [--port]`：独立运行（阻塞），无 TUI；`/api/web/stop` 返回 400
- run_command 的 CLI 语义不变；TUI 状态与 web 操作**不共享可变状态**（web 操作直接落盘，TUI 下次读取自动刷新——provider_state 等为进入页面时读取，无并发写冲突）

## 测试

- `src/web/mod.rs` 单元测试：路由解析、错误 JSON、端口绑定
- 集成测试（复用 runtime 测试模式）：bind 127.0.0.1:0 起 server 线程 → reqwest 请求 `/api/overview` `/api/stats` → 断言 JSON 结构
- `/api/command` 测试：agent status / provider list 透传正确
- 全量回归：`cargo test`（现有 849 + 新增）、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check`

## 实施分工（并行子代理）

- Agent A：`src/web/mod.rs` + `src/web/api.rs`（server + 路由 + API + 内嵌静态）
- Agent B：`src/web/static/index.html` + `app.js` + `style.css`（先创建占位文件再填充，供 A include_str!）
- Agent C：`src/main.rs` 切换集成（W 键 + Mode::WebNotice + 起停线程）+ `src/cli.rs` `web` 子命令 + `src/lib.rs`/`src/app/mod.rs` 模块声明
- 主 agent：集成编译修复 + 全量回归 + release 构建安装

## 禁止触碰

- `src/runtime/**`、`src/providers/**`、`src/domain/**`、`src/agents/**`（除 `agents::serve_port` 只读调用）
- 现有 TUI 页面逻辑（Provider/Client/Mcp/Skills/Sessions 行为不变）
- 不新增任何 Cargo 依赖（手写 HTTP + 标准库 + 已有 serde_json）