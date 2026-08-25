# 剩余项收尾计划（thinking 整流 + 模型名对齐）

**Goal:** 完成计划中标记为 P7 的 thinking 整流（错误驱动）与观察项中真实影响 zen 链路的 message_start 模型名对齐；其余观察项（Fable 槽位/Opus 分支/-thinking 后缀等仅对 Anthropic 官方模型有意义）保持观察并记录原因。

## 任务分解

### Task A: 错误驱动 thinking 整流

- rectifier.rs 增加 `DefaultRectifier`（真实实现）：上游错误体含 `thinking`/`redacted_thinking`/`signature` 相关 400 → 从请求 body 删除对应字段 → 重试一次。
- runtime 上游调用处接入：非流式 + 流式转换路径（passthrough 不接）。send_upstream 失败时读取错误体，整流器有产出则重发（限 1 次）。
- 测试：mock 上游首次 400（含 thinking 错误）→ 整流后重发 → 200；不含 thinking 的错误不整流；整流器无产出时原错误返回。

### Task B: message_start 模型名对齐

- anthropic 流式 message_start 的 `model` 用客户端请求模型（RequestIr.model，含 provider 后缀的展示名），而非上游返回的模型名。
- 测试：上游 message_start.model="deepseek-v4-flash-free" → 客户端收到 model=请求模型名。

### Task C: error_mapper 增强（观察 → 收尾记录）

- 结论：Claude Code 2.1.228 自带 400 分类器（cache_control_field/mid_conv_system/empty text/TTL 豁免），runtime 返回稳定 400 code 即可；不额外实现 mapper，记录于报告。

### Task D: 其余观察项收尾

- Fable 槽位、Opus 4.7/4.8 分支、`-thinking` 后缀、cache 断点注入：仅对 Anthropic 官方模型语义有效，zen（chat 协议自动缓存）无对应物——记录不实现原因。
- catalog 滞后：`spec use` apply 时已自动刷新，非代码缺陷——记录。

## 验证与提交

- 每任务 TDD：失败测试 → 实现 → 全量 cargo test + fmt + clippy。
- 提交分 Task A/B 两个 commit；Task C/D 写收尾报告 `~/.config/opencode/claude-codex-large-project-cache/report.md`。

## 追加：opencode.ai/go provider 配置验证（2026-08-12）

- 配置 provider（id=go，baseURL http://opencode.ai/go，API key 用户提供）
- 验证：health 加载 / /v1/models 探测 / 单次小请求确认链路（严格限制额度，不做压测/大请求）
- 若模型/协议与 zen 不同，记录差异并适配（白名单/字段按证据补）

### Task E: /model 只显示用户模型（白名单，非正则屏蔽）

- 需求：Claude Code 的 /model 选择器不显示内置官方模型（Opus 5/Sonnet 5/Haiku 4.5 等），只显示用户配置的模型；用户没配置的槽位不显示。
- 机制（Claude Code 2.1.x 原生配置，非 hack）：settings.json 顶层 `availableModels`（模型名数组）+ `enforceAvailableModels: true`（强制只显示白名单）——strings 二进制已确认字段存在。
- claude.rs 生成：availableModels = [model, model[1m]]（Default 槽位 + Opus 1M 槽位）；enforceAvailableModels = true。
- 测试：断言 settings 含 availableModels 且含两个模型名、enforceAvailableModels=true；无官方模型名。
- 风险：若某槽位（SONNET/HAIKU）映射的模型不在白名单会失效——白名单必须覆盖全部 DEFAULT_*_MODEL 值；enforce 后用户手动输其他模型可能被拒——记录为已知行为。

### Task F: 剩余 Minor 修复与界面去重（2026-08-12 追加）

| # | 项 | 方案 | 风险 |
|---|---|---|---|
| F1 | 表格行序（BTreeMap 排序破坏行序） | cli.rs/序列化用保序方案（serde_json preserve_order feature 或结构改 Vec） | 中（依赖 feature 改动需全量回归） |
| F2 | 整流重试路径上游错误未透传 | try_rectified_retry 失败后同样走 relay 透传 | 低 |
| F3 | 透传文本未清洗控制字符 | relay 前剥离 C0/C1 控制字符（ANSI 注入防护） | 低 |
| F4 | 搜索过滤下编辑单元格焦点孤立 | 过滤时失活不可见单元格焦点或禁止导航到过滤外 | 低 |
| F5 | /compact 真实复测（zen 不限流） | pty 触发 /compact 捕获请求验证 400/502 根因 | 中（zen 波动） |
| F6 | MCP/Skills/Prompts 去重（tui-menu-inventory.md 建议） | 抽通用「扩展列表」渲染组件（render_mcp/skills/prompts 三合一）；快照漂移提示；Prompts 工具条补齐 | 高（大重构，最后做） |
