# spec v2：Token 统计 + 跨端同步 + 前端重绘 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **严格执行本计划，不偏离、不自行扩展范围。**

**Goal:** 为 spec 增加：① Token 消耗统计（24h/48h/7d/30d + 缓存命中率，含 passthrough 近似标记）② Skill/MCP 跨端手动同步 ③ 快照改名 ④ 前端全量重绘（Web 思想 × TUI：Tab 导航 + /plugins 风格搜索框 + 按钮化 + 触摸）⑤ 更新入口动态化。

**Architecture:** 数据层（`src/stats.rs` JSONL 按天埋点 + 聚合；快照改名含文件迁移）→ 同步层（Skill 零转换镜像 / MCP 格式转换，任意源→目标）→ 前端重绘（menu/main 全量重写，保留 app 业务函数与后端零改动）→ 验证。后端核心（runtime/bridge/providers/domain/cli 业务）**一律不动**。

**Tech Stack:** Rust、ratatui、JSONL、termux-services。

## Global Constraints

- **后端零改动**：src/runtime/bridge/**、providers/、domain/、cli.rs 业务命令、session.rs 一律不动；runtime/mod.rs 仅允许「埋点调用 + /api/stats 路由」两处最小修改。
- 前端备份：`/data/data/com.termux/files/usr/tmp/opencode/spec-frontend-backup-20260812_194528/`（回滚对照，禁止从中复制旧 UI 设计）。
- 密钥不进 git；UI 文案中文；不新增依赖（不启用新 crate feature 除非 T 内注明）。
- 每阶段独立提交、可编译、全量测试绿；**不做计划外功能**。

---

## 阶段 1：数据层

### Task 1: Token 统计模块（src/stats.rs）

**Files:**
- Create: `src/stats.rs`
- Modify: `src/runtime/mod.rs`（仅埋点调用 + /api/stats/tokens 路由）
- Modify: `src/lib.rs`（mod stats 导出）
- Test: `src/stats.rs` 内部 tests + runtime 埋点测试

**Interfaces:**
- Consumes: runtime 各路径的 usage（decode 结果 / 响应 JSON / passthrough SSE 帧）。
- Produces: `stats::record_usage(home, UsageRecord)`、`stats::aggregate(home) -> PeriodsSummary`、`source` 标记。

- [ ] **Step 1: 定义结构与写入**

```rust
pub struct UsageRecord { ts: i64, path: String, model: String,
    input: u64, output: u64, cached: u64, source: &'static str }
// source: "converted" | "converted_stream" | "passthrough" | "passthrough_stream"
pub fn record_usage(home: &Path, r: &UsageRecord) -> Result<(), String>
// 追加 ~/.codex/stats/tokens-YYYY-MM-DD.jsonl（目录自动创建，每行一个 JSON）
```

- [ ] **Step 2: 聚合**

```rust
pub struct PeriodStat { input: u64, output: u64, cached: u64, requests: u64,
    cache_hit_rate: f64, passthrough_approx: u64 }
pub fn aggregate(home: &Path) -> BTreeMap<String, PeriodStat>
// 四档位 key: "24h" | "48h" | "7d" | "30d"；命中率 = cached/input；
// passthrough_approx = source ∈ {passthrough_stream} 的请求数（仅流式直通为近似）
```

- [ ] **Step 3: runtime 三路径埋点**
- 转换非流式：`process_protocol_request` 非流式分支 decode 后调 `record_usage`（source=converted）
- 转换流式：`stream_protocol_request` 完成/失败后从 StreamState 拿 usage（source=converted_stream）
- passthrough 非流式：`passthrough_protocol_request` 非流式分支解析响应 JSON usage（source=passthrough）
- **passthrough 流式：`passthrough_protocol_request` 流式分支——`std::io::copy` 改为逐块复制，每块扫描 `"usage"` 关键帧（data: {...usage...} 的完整 JSON 帧），转发逻辑与字节内容不变（source=passthrough_stream）
  - **硬性要求（外部审查确认，2026-08-12）**：
    ① **字段级合并**：usage 帧必须逐字段 dict-update（非 null 字段合并），**禁止「取最后一次帧」**——Anthropic 流式把 input/cache_read/cache_creation 放 message_start、output_tokens 放 message_delta，只取最后一次会丢一半字段，命中率算错
    ② **保留 source 标记**：passthrough_stream 的近似数据必须带来源标记，禁止与精确数据无标注混同
- 埋点失败静默（不阻塞请求）

- [ ] **Step 4: 聚合 API**
- runtime 新增 `GET /api/stats/tokens`（?period= 可省→全部）→ `{"periods": {...}}`；仅本地访问（同 /health 权限）

- [ ] **Step 5: 测试**
- 写入/读取往返、按天文件、聚合四档位数值、命中率计算、passthrough 扫描（mock SSE：多帧含 usage → 取最后一次；无 usage 帧不记）、`source` 标记、埋点不阻塞请求
- 全量 `cargo test` + fmt + clippy

- [ ] **Step 6: 提交**

```bash
git commit -m "feat: token usage stats with cache hit rate (24h/48h/7d/30d)"
```

### Task 2: 快照改名（projects → snapshots）

**Files:**
- Modify: `src/projects.rs`（类型/函数改名 + 数据文件迁移）
- Modify: `src/menu/mod.rs`、`src/main.rs`、`src/app/*`（UI 文案「项目」→「快照保存」）
- Modify: `src/cli.rs`（仅文案 usage，命令本身 `project` 保留别名 + 新增 `snapshot` 别名）
- Test: 迁移测试（旧文件 xu-projects.json 存在 → 读入 → 写 xu-snapshots.json → 删旧/标记迁移）

**Interfaces:**
- Consumes: 现有 ProjectStore 结构。
- Produces: SnapshotStore 同名 API（内部改名）+ 文件 `xu-snapshots.json`（旧文件自动迁移）。

- [ ] **Step 1: 改名**
- `ProjectStore`→`SnapshotStore`、`ProjectSnapshot`→`Snapshot`、`capture_project`→`capture_snapshot`（已同名函数保留语义）、`store_path` 指向 `xu-snapshots.json`
- 迁移：`read_store` 时若 xu-snapshots.json 不存在且 xu-projects.json 存在 → 读旧写新（保留旧文件，写迁移日志）
- UI/CLI 文案全改「快照保存」（grep 全部「项目」文案）

- [ ] **Step 2: 测试与提交**

```bash
git commit -m "refactor: rename project to snapshot with data migration"
```

---

## 阶段 2：同步层

### Task 3: Skill 跨端同步引擎

**Files:**
- Create: `src/sync.rs`（或 `src/app/sync_flow.rs` 内函数）
- Modify: `src/cli.rs`（`spec sync skills <源> <目标...> [--dry-run]`）
- Modify: `src/menu/mod.rs` + `src/main.rs`（同步页 UI：源/目标选择 + 预览——并入 Task 5 前端重绘，本 Task 只做引擎+CLI）
- Test: 三端模拟目录、任意源→任意目标、幂等（重复同步无变化）、删除同步

**Interfaces:**
- Consumes: 源端 skills 目录（`~/.config/opencode/skills/`、`~/.claude/skills/`、`~/.codex/skills/`）。
- Produces: `sync_skills(home, source: AgentTarget, targets: &[AgentTarget], dry_run) -> SyncReport{added, updated, removed, unchanged}`。

- [ ] **Step 1: 实现引擎**
- 读取源端 `<name>/SKILL.md` 目录清单（**全部技能，不分来源**，含插件市场装的）
- 镜像到每个目标端目录（三端 SKILL.md 同构，零转换；沿用 sync-skills.sh 的指纹 `.sync-skill.fp` 与删除逻辑）
- dry_run 返回报告不写盘

- [ ] **Step 2: CLI + 测试 + 提交**

```bash
git commit -m "feat: cross-agent skill sync (manual source->targets)"
```

### Task 4: MCP 跨端同步引擎

**Files:**
- Create: `src/sync_mcp.rs`（或并入 src/sync.rs）
- Modify: `src/cli.rs`（`spec sync mcp <源> <目标...> [--dry-run]`）
- Modify: 同 Task 3（UI 并入 Task 5）
- Test: 三端格式转换往返（opencode mcp ↔ claude mcpServers ↔ codex mcp）、全部 server 同步（含未启用）、幂等

**Interfaces:**
- Consumes: 源端 MCP 配置（opencode.json 的 mcp / ~/.claude.json 的 mcpServers / codex config.toml）。
- Produces: `sync_mcp(home, source, targets, dry_run) -> SyncReport`；格式映射：command 数组↔字符串+args、environment↔env、headers↔http_headers、type local/remote↔stdio/sse/http。

- [ ] **Step 1: 实现引擎（复用 mcp.rs 的 import_live/projection_patch/json_server）**
- 读源端全部 MCP server 定义 → 格式转换 → 写入目标端（保留目标端已有、合并/覆盖同名——同名覆盖需预览提示）
- 全部 server 同步（含未启用的；claude 启用状态在 projects.<path>.enabledMcpjsonServers——同步时按源端启用状态写入目标端对应位置，若目标格式不支持则记录 warning）

- [ ] **Step 2: CLI + 测试 + 提交**

```bash
git commit -m "feat: cross-agent MCP sync (all servers, format conversion)"
```

---

## 阶段 3：前端全量重绘（Web 思想 × TUI）

### Task 5: 前端重绘

**Files:**
- Rewrite: `src/menu/mod.rs`（全新渲染）、`src/main.rs`（Mode 分发精简）
- Modify: `src/app/*`（仅适配新 UI 调用，业务函数保留）
- Create: `src/menu/searchbox.rs`（SearchBox 组件）
- Test: 渲染/命中/交互测试更新 + 新增

**Interfaces:**
- Consumes: app/*_flow 业务函数、stats::aggregate、sync 引擎、provider_ops。
- Produces: 新 UI（下述页面）+ 全量测试绿。

- [ ] **Step 1: 结构设计（Web 思想）**
- 顶部 Tab 导航条（按钮式，触摸/鼠标/键盘均可）：**供应商 / 统计 / 同步 / 快照 / 运行时 / 工具**
- SearchBox 组件：圆角线框 + `⌕` 图标 + 实时过滤 + Esc 清除（/plugins Installed 风格）
- 卡片列表 + 按钮化（IconButton 增强）+ 触摸事件（鼠标事件复用，增大命中区）

- [ ] **Step 2: 各页面**
- 供应商页：SearchBox + 供应商卡片（名称/协议/端点）→ 详情（端点信息/模型表格[显示名|请求名，点击编辑]/获取模型/测试/应用到三端/bypass 开关右下角）/**单·多供应商分开选择**（单选=直连配置；多选=runtime slug + active 列表，控制面按调研 supplier-modes-and-token-stats.md）
- 统计页（Task 6）
- 同步页：Skill/MCP 同步（源 agent 选择 → 目标多选 → 预览 diff → 执行）
- 快照页：快照保存列表（新建/切换/删除/覆盖，含漂移提示）
- 运行时页：health/端点清单/启停按钮
- 工具页：doctor/备份/安装更新入口（更新动态化见 Task 7）

- [ ] **Step 3: 后端零改动验证 + 测试 + 提交**

```bash
git commit -m "feat: redraw TUI with tab nav, searchbox and card UI"
```

### Task 6: Token 统计页

**Files:**
- Modify: `src/menu/mod.rs`、`src/main.rs`（统计页渲染）
- Test: 统计页数据渲染测试（mock aggregate）

- [ ] **Step 1: 页面**
- 同一页面展示：24h/48h/7d/30d 四档卡片（input/output/cached/请求数/命中率 %）
- **右下角固定标记：「⚠ passthrough 直通流量为近似统计」**（用户要求，必须存在）

- [ ] **Step 2: 测试 + 提交**

```bash
git commit -m "feat: token stats page with 4 periods and cache hit rate"
```

### Task 7: 更新入口动态化

**Files:**
- Modify: `src/menu/mod.rs`、`src/main.rs`、`src/app/agent_flow.rs`（渲染条件）
- Test: 有更新显示「更新 xxx」/无更新显示版本号

- [ ] **Step 1: 实现**
- 复用 agent_tools.rs 的 `tool_status`「可更新 X→Y」信号；进入工具页时查询（已有 query_latest 机制）
- 渲染：有更新 → 显示「更新 <agent> → <版本>」按钮；无更新 → 显示当前版本号；未安装 → 显示「安装」
- 工具条「全部更新」chip 仅在存在任一可更新 agent 时显示

- [ ] **Step 2: 测试 + 提交**

```bash
git commit -m "feat: show update entry only when a new version exists"
```

---

## 阶段 4：验证收尾

### Task 8: 集成验证

- [ ] **Step 1**: 全量 `cargo test` + fmt + clippy 全绿；部署新二进制（sv stop → cp → sv start）
- [ ] **Step 2**: 真实场景验证：Claude Code 会话（zen-go）→ 统计页出现数据（四档位 + 命中率 + 右下角标记）；Skill/MCP 同步真实执行（opencode → claude 试一次 dry-run + 实际同步）；快照保存/切换；更新入口（有更新显示）
- [ ] **Step 3**: 文档更新（README/USER_GUIDE 补充：统计、同步、快照、新 UI）
- [ ] **Step 4**: 提交

```bash
git commit -m "docs: spec v2 usage guide"
```

---

## 验收标准（全部满足才算完成）

1. `cargo test` 全量绿、fmt/clippy 干净
2. 统计：四档位数值与命中率正确；passthrough 流式有近似标记（右下角文案存在）
3. 同步：Skill/MCP 任意源→目标可执行、幂等、预览准确
4. 快照：改名完成、旧数据自动迁移、切换/漂移提示正常
5. 前端：Tab 导航/搜索框/卡片/按钮化/触摸可用；单·多供应商选择生效
6. 更新入口：仅在有新版本时显示「更新 xxx」
7. 后端核心（bridge/providers/domain/cli 业务）零改动（git diff 验证）

## v3 追加（2026-08-13）：供应商界面 CC Switch 化 + Usage 精确化

### T-A 三端 Tab（详情页内）
- 详情页内部 `agent_tab: AgentTarget`（Claude Code / Codex / OpenCode 按钮切换），各端独立布局与字段
- 布局：头部（名称/健康）+ Tab 条 + 端内容（模型区/槽位/开关/协议/重试）+ 操作 chips + 底部提示

### T-B 槽位展开选择（Claude Tab）
- 槽位行（Fable/Opus/Sonnet/Haiku）请求模型输入框右侧「▸」展开 → 显示上游拉取模型列表 → 点击选择填入；未拉取过自动拉取

### T-C 模型多选删减
- 「获取模型」→ 拉取列表多选勾选（初始=当前 models）→ 保存写 models（--models-json）；展开行 name/limit

### T-D 三端开关区
- Claude：完全权限（permissions.defaultMode toggle，只增只删）+ 会话标题关闭（env CLAUDE_CODE_DISABLE_TERMINAL_TITLE toggle）
- OpenCode：完全权限（opencode.json permission toggle）
- 三端各一：路由开关（xu-client-routes.json clients.<agent>.enabled toggle + 该供应商增删 providers）

### T-D2 协议切换器
- OpenCode Tab：Chat Completions ↔ Responses API（写 apiKind；npm 映射 @ai-sdk/openai-compatible/@ai-sdk/openai；anthropic 禁用+提示）
- Claude Tab：chat/responses/anthropic（上游目标协议）
- Codex Tab：wire_api chat/responses
- 保存走 `provider update --kind`

### T-D3 重试/超时编辑
- 每端高级区：timeout_ms + max_retries 输入框（`provider update --timeout/--max-retries`）
- apply 映射：opencode.json options.chunkTimeout/maxRetries；codex config.toml 重试；claude 经 runtime

### T-E 借鉴功能
- 获取模型流程增强（预检/计数/错误分类）、密钥输入（显示/隐藏、失焦提交）、实时校验、端点测速（可选）

### T-E2 Usage 统计精确化（抄 CC Switch，不做成本）
- UsageRecord 扩展：cache_creation + semantics（FRESH/TOTAL/LEGACY）+ latency_ms + status_code + is_streaming + error
- 埋点：成功/失败都记；cache-only（input=0 但 cached>0）不丢；流式合并 delta 优先
- 聚合：semantics 归一（TOTAL→input−cr−cc、LEGACY→input−cr，clamp）；命中率分母 = fresh_input + cache_creation + cache_read；成功率
- 统计页：cache_write 列 + 精确命中率 + 成功率；passthrough 近似标记保留
- 不做成本估算与 SQL（JSONL 保留）

### 执行与并行
- 前端包（T-A→T-E 串行分阶段提交，单 agent）
- 后端包（T-E2，独立 agent，文件不与前端冲突）
- 两路并行 → 全量验证 → 部署 → 真实场景验证
