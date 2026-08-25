# Xu Backlog 里程碑规划（安全+可用混合）

日期：2026-08-10
仓库：`/data/data/com.termux/files/home/xu-rs`
上游事实源：`CC_SWITCH_GAP_AUDIT.md`（P0-P4 实施顺序）、`IMPLEMENTATION_PLAN.md`（阶段 0-12）、Task 11 验证结论。

## 背景与目标

Task 11 收尾验证已完成：Zen 无 key 三端文本可用、OpenCode/Claude 工具可用、Codex 文本可用（Responses 流式 `output_text.done` 文本累积已修复），Codex 客户端自有工具类型（`custom`/`namespace`/`web_search`）仍被硬过滤。

本项目把 GAP AUDIT 的 P0-P4 与 IMPLEMENTATION_PLAN 的阶段 5-12 重排为四个可交付里程碑。排序原则经确认：**安全与可用混合**——第一里程碑同时收 P0 安全项与 P2 Runtime 可用项，之后 P1 功能闭环、P3 可观测高可用、P4 发布成熟度。

## 里程碑总览

| 里程碑 | 主题 | 来源 | 交付物性质 |
|---|---|---|---|
| M1 | 安全底座 + Runtime 可用 | P0 剩余 + P2 Runtime 项 + Codex 工具 parity | 可交付：加固的写入层、可并发/可超时的 Runtime、Codex 工具可用 |
| M2 | 功能闭环 | P1 剩余 | 可交付：Universal Provider、presets/排序/搜索、Project 收尾、导入 wizard |
| M3 | 可观测与高可用 | P3 | 可交付：Usage/Cost/Quota、Multi-endpoint、Failover/Circuit Breaker |
| M4 | 同步与发布 | P4 | 可交付：SQLite migrations、WebDAV/S3、self-update/Android CI/诊断 bundle |

每个里程碑内部按依赖排序，每个交付单元独立可验收。不做跨里程碑耦合：M2 的写操作全部建立在 M1 的原子写入/锁之上。

## M1：安全底座 + Runtime 可用

### 交付单元与任务

1. **原子写入与持久化加固**（P0，审计第 23 行）
   - 同目录临时文件、`0600`、`fsync`、原子 rename、父目录同步（`fsync` 目录）。
   - 审计现有 `patch::atomic_write` 实现，补齐缺失项；新增回归测试覆盖断电/失败中间态语义（通过注入失败点验证无半写文件）。
2. **跨进程锁与代际校验**（P0，审计第 24 行）
   - preview-to-apply 内容代际校验已有；补跨进程锁与 daemon 锁。
   - 锁粒度：配置文件写操作与 `spec serve` 状态写操作；锁冲突返回明确错误而非静默失败。
3. **Secret 强化**（P0，审计第 29 行）
   - 支持 stdin/env/file/secret reference 已有；补齐嵌套 header 与任意 credential 名称脱敏的完整覆盖，并验证 dry-run 与 diff 输出不泄漏。
4. **Runtime 并发与 timeout**（P0 MISSING，审计第 32、193、197 行）
   - 有界并发（semaphore/worker 池）、socket read/write/idle timeout、body/header 大小限制（已有部分：`runtime::http` 已有 body/header 限制测试）、chunked input、keep-alive、连接池。
   - graceful drain：活跃连接数、uptime、成功率上报（health 扩展），SIGTERM 后拒绝新连接、排空存量连接。
5. **Codex 工具 parity**（P2，审计第 185 行 `custom tools`）
   - 现状：`responses::parse_tools` 硬过滤 `custom`/`namespace`/`web_search`（responses.rs 第 564 行）。
   - 目标：`custom` 工具按 JSON Schema 提取为 function 转发（name/description/parameters 映射）；`namespace`/`web_search` 保持过滤并给出明确 unsupported 语义，不静默丢。
   - 回归：现有 `responses_drops_known_client_owned_tool_types_for_provider_conversion` 测试需按新语义更新；新增 custom 工具正向转换测试与真实 Codex 工具调用端到端验证。
6. **Runtime hot reload**（P2，审计第 192 行）
   - provider store 变更后 Runtime 在不重启进程的情况下重新加载 provider 配置（监听 store 变更信号或版本号轮询）。
   - CLI `spec serve` 增加 reload 语义：手动触发 + 自动检测；health 上报加载版本与最后 reload 时间。
7. **存量写路径迁移到原子层**（P1 前置依赖）
   - 现有 CLI/TUI 的 provider 写、state 写等全部存量写操作迁移到 M1 的原子写入与锁；这是加固动作，不是新功能。完整"首次启动与 live config 导入 wizard"功能本体在 M2 交付，届时天然复用该原子层。

### 验收标准

- 全部新增/修改代码有单元与集成测试，测试先失败后通过（TDD）。
- 断电/半写注入测试证明无残留半写文件；锁冲突有明确错误与恢复路径。
- Runtime 在并发请求、超时、过大 body、SIGTERM drain 场景下行为符合定义（对照 `runtime::http` 现有测试扩展）。
- Codex 真实 CLI 工具调用（如 `pwd`）通过且输出完整；`web_search`/`namespace` 语义明确。
- `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets -- -D warnings`、`cargo build --release` 全绿；安装版 SHA-256 与 release 一致；Runtime 真机验证。

## M2：功能闭环

来源：GAP AUDIT P1（第 393-401 行）+ Provider 管理 PARTIAL 项（第 46-49、53-55、57-59 行）。

1. **Universal Provider 实体**：独立实体、联动编辑、原子三端同步（复用 M1 原子写入与锁）。
2. **Provider presets**：从现有少量模板扩展为按客户端维护的预设库；默认模型更新机制。
3. **Provider 持久排序与搜索**：持久排序、拖动排序（TUI）、批量操作、搜索/收藏。
4. **Project Profiles 收尾**：审计剩余项（现有：创建/重命名/删除/capture/切换）；补齐审计中标记缺失的部分。
5. **首次启动与 live config 导入 wizard**：Claude/Codex/OpenCode live config 导入与双向 backfill，全部走 M1 写入层。
6. **Official Login / OAuth Auth Center**：如范围允许则纳入；否则明确标为 M3 或后续（在计划中显式决策，不悄悄跳过）。

验收标准与 M1 相同（完成定义八条），且所有写操作复用 M1 原子层。

## M3：可观测与高可用

来源：GAP AUDIT P3（第 410-416 行）与对应章节（Usage 217-240、Failover 199-215、Multi-endpoint 65/189）。

1. **Usage/Cost/Quota**：SQLite request log + daily/hourly rollup；三端 session usage 增量导入；维度（request/provider/model/session/app/status/stream）；token 明细与单价、cost multiplier、自定义价格（seed 不覆盖用户设置）；时间/Provider/模型/客户端/状态筛选与 JSON/CSV/Markdown 导出；官方 quota 与第三方 balance 模板。
2. **Multi-endpoint**：endpoint candidates、默认 endpoint、自动最快选择、Full endpoint 模式；依赖 M1 hot reload。
3. **Failover/Circuit Breaker**：每客户端 Provider queue、固定优先级/round-robin/sticky/weighted；跨 Provider retry 与 endpoint fallback；错误分类（网络/429/5xx/auth/invalid request/bridge failure）；官方账号 auth 错误永不跨账号 failover；Closed/Open/HalfOpen、consecutive failure/error rate/minimum sample、first-byte/stream idle/non-stream timeout、recovery wait/probe/threshold；circuit 状态持久化/恢复；failover event log 与 UI 状态、手动 reset。

## M4：同步与发布

来源：GAP AUDIT P4（第 417-422 行）、IMPLEMENTATION_PLAN 阶段 12。

1. **SQLite schema/migrations/full export**：版本化迁移、全量导出。
2. **WebDAV/S3 快照**：命令（push/pull/list/restore）、内容、安全（不泄漏 secret）。
3. **Self-update、signed release、Android CI、诊断 bundle**。

## 跨里程碑依赖

- M1 原子写入+锁 → M2 所有写操作、M3 circuit 状态持久化、M4 快照一致性。
- M1 Runtime 连接层（并发/timeout）→ M3 failover 的 retry/timeout 语义。
- M1 hot reload → M3 multi-endpoint 动态切换。
- M2 Provider/Project 实体稳定 → M3 Usage 维度与 quota 绑定。
- M3 完成 → M4 发布成熟度（CI 打包含 M3 产物）。

## 风险与对策

- Codex 工具 parity 上游（Zen Chat 协议）无法表示部分工具语义：以“可表示的转发、不可表示的明确报错”为边界，不静默丢；端到端验证失败时记录真实原因并回退为文档化限制。
- Runtime 连接层改造可能引入既有行为回归：所有改造先有测试；`runtime::http` 与 `runtime_bridge_integration` 全量回归。
- 里程碑范围膨胀：每个交付单元独立验收，完成即标记；范围外项进入下个里程碑或显式延期，不悄悄省略。

## 完成定义（沿用 GAP AUDIT 第 423-434 行）

功能只有同时满足：真实 domain/store/service；有 CLI；有触屏 TUI 且窄屏可用；所有写入有 preview、脱敏、确认、备份、失败回滚；保留用户未知配置；部分失败可见可重试；有单元/集成/回归测试；完成 release build、安装与真机验证——才标为 `DONE`。

## 执行纪律

- 每个交付单元 TDD：先写失败测试，再最小实现，再回归。
- 每个状态变更操作后有独立只读验证；不依据推测报告结果。
- 不为了交付速度省略验证或文档；如实记录限制与失败。
