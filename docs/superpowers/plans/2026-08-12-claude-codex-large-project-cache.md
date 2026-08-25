# Claude Code / Codex 大项目适配与缓存命中率系统性计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在真实大项目（xu-rs 自身，10k+ 行 Rust）上跑一轮 Claude Code 与 Codex 真实多轮会话，测量并优化 zen 上游前缀缓存命中率与两端客户端适配性（模型映射、字段适配、compaction、超时/502 韧性）。

**Architecture:** 两个并行工作流——Agent A 负责 Claude Code 端（Anthropic Messages → chat 转换路径），Agent B 负责 Codex 端（Responses → chat 转换路径）。两端共享同一 runtime（`spec serve` :9316 → zen `deepseek-v4-flash-free_zen`）与同一测量方法论：真实多轮会话 + 工具调用，逐请求记录 `usage.prompt_tokens_details.cached_tokens` / `cache_read_input_tokens` 增长与请求字节哈希稳定性。已在 2026-08-12 完成的基线：system-role 消息支持（400 修复）、`tool_calls: null` 与 `index` 字段容忍、模型槽位映射（`ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL`）、`modelOverrides`、`CLAUDE_CODE_MAX_CONTEXT_TOKENS`。

**Tech Stack:** spec runtime（Rust）、zen（opencode.ai/zen/v1）、Claude Code 2.1.228、Codex CLI 0.147.0、python3 测量脚本、curl。

## Global Constraints

- 不修改 xu-rs 源码之外的运行时配置；所有配置改动先备份。
- zen 免费层有 ~90s 冷却与偶发 502：测量脚本节奏 ≥5s，任何单点失败重试一次后记录为「瞬态」，不把偶发当常见。
- 不写真实密钥到报告/脚本/git；日志脱敏。
- 真实会话限流：每 agent 每轮 ≤20 次 zen 请求；超出改走 mock 复现。
- 不提交未经用户授权的改动；测量报告写入 `~/.config/opencode/claude-codex-large-project-cache/`（工作区）。

---

### Task 1（双端公共）：建立大项目测量基线

**Files:**
- Create: `~/.config/opencode/claude-codex-large-project-cache/measure.py`
- Create: `~/.config/opencode/claude-codex-large-project-cache/baseline.md`

**Interfaces:**
- Consumes: `spec serve` (:9316)、zen 模型、xu-rs 仓库。
- Produces: 可复用的多轮测量脚本（真实会话 + 字节哈希 + cached_tokens 记录）与基线数据。

- [ ] **Step 1: 确定真实会话脚本**

在大项目目录（xu-rs 或用户指定大 repo）构造 6 轮真实任务：读文件 → 改一个函数 → 跑测试 → 读报错 → 修 bug → 总结。每轮记录：
- 请求字节 sha256（验证前缀稳定）
- 响应 usage：`cached_tokens` / `prompt_cache_hit_tokens` / `cache_read_input_tokens`
- 状态码、时延、错误（瞬态 vs 模式）

- [ ] **Step 2: 基线运行**

对 Claude Code 和 Codex 各跑一轮（headless 模式），输出到 baseline.md：每轮 cached_tokens 增长曲线、前缀稳定哈希、瞬态错误清单。

- [ ] **Step 3: 分析基线**

找缓存断点（前缀变化原因：system 变化？工具顺序变化？compaction 触发？）与适配断点（字段被拒、超时、502 模式）。输出两类发现清单，供 Task 2/3 定位。

---

### Task 2（Agent A，Claude Code 端）：字段适配 + 缓存优化

**Files:**
- Modify: `xu-rs/src/runtime/bridge/anthropic.rs`（若字段适配发现缺口）
- Modify: `xu-rs/src/runtime/bridge/chat.rs`（若响应编码缺口）
- Modify: `xu-rs/src/agents/claude.rs`（若模型/窗口映射缺口）
- Read: `xu-rs/CC_SWITCH_GAP_AUDIT.md`（CC Switch 字段适配参考）

**Interfaces:**
- Consumes: Task 1 基线（Claude 端）、runtime 转换路径、Claude Code settings。
- Produces: Claude Code 端适配修复 + 大项目第二轮测量（缓存命中率提升证据）。

- [ ] **Step 1: 字段适配审查**

对比 CC Switch 的 request rectifier / thinking 修复 / error mapping，逐项核对 runtime 对 Claude Code 2.1.228 真实请求的适配（thinking adaptive、context_management、cache_control、system-role、metadata、output_config、tool schema）。发现缺口 → 写回归测试 → 修复。

- [ ] **Step 2: 缓存前缀优化**

分析基线中 Claude 端 cached_tokens 断点：system prompt 是否稳定（工具定义顺序、compaction 时机、session 前缀）。若工具定义顺序不稳定（HashMap 顺序泄漏），修复为稳定排序；若 compaction 导致前缀重置，评估 CLAUDE_CODE_MAX_CONTEXT_TOKENS 与实际窗口的匹配。

- [ ] **Step 3: 大项目第二轮测量**

修复后重跑大项目会话，对比基线：cached_tokens 增长曲线、请求字节稳定哈希、瞬态错误。输出 improvement 数据。

---

### Task 3（Agent B，Codex 端）：Responses 适配 + 缓存优化

**Files:**
- Modify: `xu-rs/src/runtime/bridge/responses.rs`（若适配缺口）
- Modify: `xu-rs/src/runtime/bridge/chat.rs`（若响应编码缺口）
- Modify: `xu-rs/src/agents/codex.rs`（若 catalog/模型映射缺口）
- Read: `xu-rs/TASK10_DIFFERENTIAL_REVIEW_2026-08-10.md`（Responses 差异契约）

**Interfaces:**
- Consumes: Task 1 基线（Codex 端）、runtime 转换路径、Codex config/catalog。
- Produces: Codex 端适配修复 + 大项目第二轮测量。

- [ ] **Step 1: Responses 字段适配审查**

核对 Codex CLI 0.147.0 真实 requests/responses 形状与 runtime 适配（reasoning effort、item id、tool schema `format`、usage 字段存活）。发现缺口 → 回归测试 → 修复。

- [ ] **Step 2: 缓存前缀优化**

分析 Codex 端 cached_tokens 断点：Responses → chat 转换后前缀是否与原生 chat 共享 zen 缓存（2026-08-11 已验证 256 前缀命中）；多轮会话中 instructions/tools 是否稳定；reasoning effort 注入是否改变前缀。

- [ ] **Step 3: 大项目第二轮测量**

修复后重跑，对比基线输出 improvement 数据。

---

### Task 4（收尾）：汇总与固化

**Files:**
- Create: `~/.config/opencode/claude-codex-large-project-cache/report.md`
- Update: `xu-rs/CHANGELOG.md`（若功能改动落地）

**Interfaces:**
- Consumes: Task 2/3 修复与测量。
- Produces: 双端对比报告 + 固化建议（哪些进 runtime、哪些进 adapter、哪些留配置）。

- [ ] **Step 1: 汇总双端数据**

Claude Code vs Codex 的缓存命中率提升、适配修复清单、遗留瞬态错误统计（偶发 vs 模式）。

- [ ] **Step 2: 固化建议**

列出建议提交的源码改动（带测试）与建议保留的用户配置；未授权不提交。
