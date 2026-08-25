# 缓存双模式与适配加固实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 spec runtime 增加「全兼容 / DeepSeek 兼容」两种上游缓存模式（provider 级配置），并修复调研与实测发现的全部适配缺陷（count_tokens 400、usage 双计、`[1M]` 后缀、工具排序确定性），并入 CC Switch 已验证的高价值功能（thinking 整流入口、错误分类豁免）。

**Architecture:** ProviderProfile 增加 `cache_mode` 字段（`auto` 默认 / `compat` 全兼容 / `deepseek` 特化）。`compat` = OpenAI 风格宽松前缀缓存，保持标准编码；`deepseek` = DeepSeek 前缀单元完整匹配特化：tools 数组按 name 稳定排序、system 固定前置、usage 归一化（`prompt_cache_hit_tokens`→`cached_tokens`）且输入不双计。修复集中在 bridge 请求/响应编码与 runtime 端点，全部带回归测试；最后用 zen 真实对照测量验证两种模式。

**Tech Stack:** Rust、serde_json、spec runtime bridge、Claude Code 2.1.228（真实客户端验证）、zen。

## Global Constraints

- 不覆盖未提交改动；配置改动先备份；密钥不写入报告/测试/git。
- zen 免费层冷却与偶发 502：真实测量单点失败重试一次后记为瞬态，不把偶发当常见。
- 不新增依赖；不重构无关代码；每个修复先写失败测试。
- 缓存模式默认 `auto`（按 provider 识别），不改变现有 zen 用户行为。
- 调研来源：`~/.config/opencode/claude-codex-large-project-cache/research/*.md`（4 份并行调研报告）。

---

## 问题清单（调研 + 实测证据）

| # | 问题 | 证据 | 严重度 |
|---|---|---|---|
| P1 | `/v1/messages/count_tokens` 返回 400，Claude Code 调用（`{model,messages}` → 期望 `{"input_tokens":N}`，只读 input_tokens） | 实测 req24/25 400；research/anthropic-cc-caching.md | 高 |
| P2 | usage 双计：chat→anthropic 时若 `input_tokens` 含缓存前缀又另报 `cache_read_input_tokens`，下游 2x（压缩提前触发） | research/chat-provider-caching.md（CCR #1655） | 高 |
| P3 | `[1M]` 模型后缀未剥离（Claude Code 模型名可带 `[1m]`/`[1M]`，转发前需剥离） | research/switch-tools-adaptation.md（高优先遗漏） | 中 |
| P4 | DeepSeek 前缀单元完整匹配下，tools 数组顺序不稳定会掉命中（BTreeMap 排序的是 JSON 键，不是 tools 数组元素） | research/zen-caching.md + chat-provider-caching.md | 中 |
| P5 | 无缓存模式概念：compat/deepseek 行为不可配置 | 用户需求 | 中 |
| P6 | 工具定义/usage 字段在转换方向的存活无「双计」回归锁定 | research/chat-provider-caching.md | 低 |
| P7 | CC Switch 的 thinking 整流（错误驱动）未并入——7 类错误→删 thinking/redacted_thinking/signature；zen 场景先做错误分类豁免，整流留 hook | research/switch-tools-adaptation.md | 低（观察） |
| P8 | 模型槽位映射已做；`-thinking` 后缀 / Fable 槽位 / Opus 分支 仅对 Anthropic 官方模型有意义，zen 场景不实现，记录为观察项 | research/switch-tools-adaptation.md | 观察 |

---

### Task 1: 缓存模式基础设施

**Files:**
- Modify: `src/domain/provider.rs`（ProviderProfile 增加 `cache_mode`）
- Modify: `src/providers/store.rs`（解析 `cache_mode`，缺省 auto）
- Modify: `src/agents/codex.rs` / `src/agents/claude.rs`（若表单/预览需显示，先不做 UI，仅存储透传）
- Test: `tests/provider_store.rs`

**Interfaces:**
- Consumes: provider JSON 根对象。
- Produces: `ProviderProfile.cache_mode: CacheMode`（`Auto`/`Compat`/`DeepSeek`），解析缺省 `Auto`。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn cache_mode_parses_with_default_auto() {
    let v: Value = serde_json::from_str(r#"{"provider":{"zen":{"apiKind":"chat","name":"zen","models":{"m":{}}}}}"#).unwrap();
    let p = read_profiles_impl(&v).unwrap();
    assert_eq!(p[0].cache_mode, CacheMode::Auto);
    let v2: Value = serde_json::from_str(r#"{"provider":{"zen":{"apiKind":"chat","name":"zen","cacheMode":"deepseek","models":{"m":{}}}}}"#).unwrap();
    let p2 = read_profiles_impl(&v2).unwrap();
    assert_eq!(p2[0].cache_mode, CacheMode::DeepSeek);
    // 非法值回退 Auto
}
```

- [ ] **Step 2: 实现解析**

`CacheMode` enum + `ProviderProfile.cache_mode`，store 解析 `cacheMode` 字段（`auto|compat|deepseek`），未知值回退 `Auto`。所有现有 `ProviderProfile` 构造点（测试、adapter）补默认值。

- [ ] **Step 3: 全量验证**

```bash
cargo test --test provider_store && cargo test
```

- [ ] **Step 4: 提交**

`git commit -m "feat: provider cache_mode (auto/compat/deepseek)"`

---

### Task 2: DeepSeek 模式编码（tools 排序 + system 前置）

**Files:**
- Modify: `src/runtime/bridge/request.rs` 或 `src/runtime/mod.rs`（编码入口按 cache_mode 分支）
- Modify: `src/runtime/bridge/chat.rs`（`encode_request` 增加稳定排序选项）
- Test: `src/runtime/bridge/chat.rs` / `tests/runtime_bridge_integration.rs`

**Interfaces:**
- Consumes: `ProviderProfile.cache_mode`。
- Produces: `encode_upstream_request` 在 DeepSeek 模式下输出 tools 按 name 排序、system 消息恒定位首的请求。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn deepseek_mode_sorts_tools_and_keeps_system_first() {
    // 输入 tools [{"name":"z_tool"...},{"name":"a_tool"...}]，system 消息在中间
    // DeepSeek 模式编码后：tools[0].name == "a_tool"；第一条消息 role == "system"
}
```

- [ ] **Step 2: 实现**

`encode_upstream_request(ir, target, model, cache_mode)`（新增参数，调用点更新）：
- `CacheMode::DeepSeek` + target chat：tools 数组按 `name` 排序后输出；system/developer 消息保持最前（若 IR 中 system 不在首位，重排）。
- `Compat`/`Auto`：保持现有行为不变。

- [ ] **Step 3: 兼容性回归**

所有既有方向测试（`text_routes_cover_all_protocol_directions` 等）用 `Auto` 走原路径必须全绿；新增 DeepSeek 模式单测验证排序。

- [ ] **Step 4: 提交**

`git commit -m "feat: deepseek cache mode sorts tools and pins system prefix"`

---

### Task 3: usage 双计修复（chat→anthropic 输入不重复计缓存）

**Files:**
- Modify: `src/runtime/bridge/chat.rs`（`decode_usage` 或 encode 映射）
- Modify: `src/runtime/bridge/anthropic.rs`（`encode_usage`）
- Test: 单测 + `tests/runtime_bridge_integration.rs`

**Interfaces:**
- Consumes: chat usage（`prompt_tokens`、`cached_tokens` / `prompt_cache_hit_tokens`）。
- Produces: anthropic 方向 `input_tokens = prompt_tokens - cached_tokens`（cached 为 0 时等于 prompt_tokens），`cache_read_input_tokens = cached_tokens`。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn chat_to_anthropic_usage_does_not_double_count_cache() {
    // chat usage: prompt_tokens=100, cached_tokens=40
    // anthropic 编码后: input_tokens == 60, cache_read_input_tokens == 40
}
```

- [ ] **Step 2: 实现**

chat decode 已得 `cached_tokens`；anthropic `encode_usage` 的 input_tokens 改为 `prompt_tokens.saturating_sub(cached)`；`cache_read_input_tokens = cached`。检查现有实现是否已如此（若已正确则测试锁定并跳过修改）。

- [ ] **Step 3: 回归**

全量测试 + 既有 cache 相关测试（`decoder_accepts_provider_reasoning_and_cost_metadata` 等）不破。

- [ ] **Step 4: 提交**

`git commit -m "fix: chat->anthropic usage excludes cached prefix from input_tokens"`

---

### Task 4: `[1M]` 模型后缀剥离

**Files:**
- Modify: `src/runtime/mod.rs`（`resolve_model` 或模型解析入口）
- Test: `src/runtime/mod.rs` 单测 / `tests/runtime_bridge_integration.rs`

**Interfaces:**
- Consumes: 客户端模型名（可能带 `[1m]`/`[1M]` 后缀）。
- Produces: 上游模型名剥离 `[1m]` 后缀；客户端侧展示保留。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn model_resolution_strips_1m_suffix_for_upstream() {
    // 请求模型 "deepseek-v4-flash-free_zen[1m]" -> 上游模型 "deepseek-v4-flash-free"
}
```

- [ ] **Step 2: 实现**

`resolve_model` 匹配时先剥离 `[1m]`/`[1M]` 后缀再解析 provider 后缀；`upstream_model` 输出不带 `[1m]`。

- [ ] **Step 3: 回归 + 提交**

全量测试；`git commit -m "fix: strip [1m] model suffix before upstream dispatch"`

---

### Task 5: `/v1/messages/count_tokens` 端点

**Files:**
- Modify: `src/runtime/mod.rs`（路由 + handler）
- Modify: `src/runtime/bridge/mod.rs`（若需协议路径映射）
- Test: `tests/runtime_bridge_integration.rs` / runtime 单测

**Interfaces:**
- Consumes: `POST /v1/messages/count_tokens`，body `{model, messages}`（Claude Code 实测格式）。
- Produces: `{"input_tokens": N}`（估算值；CC 只读 `input_tokens`）。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn count_tokens_returns_input_tokens_estimate() {
    // POST /v1/messages/count_tokens {model, messages:[...]}
    // -> 200 {"input_tokens": N>0}，响应只含 input_tokens 键
}
```

- [ ] **Step 2: 实现**

路由 `/v1/messages/count_tokens`（不含 ?beta=true 也匹配）：解析 body（要求 `messages` 数组；模型可选）；估算 token 数（消息文本长度/4 的近似，或复用现有 token 计数逻辑如有）；返回 `{"input_tokens": N}`。不做上游请求。

- [ ] **Step 3: 回归 + 真实验证**

全量测试；用真实 Claude Code 请求（captured body）curl 验证 200 且 CC 可接受。

- [ ] **Step 4: 提交**

`git commit -m "feat: implement /v1/messages/count_tokens for Claude Code"`

---

### Task 6: 错误分类豁免 + thinking 整流 hook（CC Switch 功能并入）

**Files:**
- Modify: `src/runtime/bridge/anthropic.rs`（错误分类）
- Modify: `src/runtime/bridge/ir.rs`（若需要新错误类别）
- Test: 单测

**Interfaces:**
- Consumes: 上游错误 body（`cache_control_field` / `mid_conv_system` / 空 text / TTL 错误等 400 类别）。
- Produces: 对已知良性 400（CC 自带分类器豁免的类别）返回可识别的明确错误；为 thinking 整流预留 hook（`RequestRectifier` trait 或等价扩展点，本任务仅加空实现 + 测试占位）。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn known_benign_400_categories_surface_clear_errors() {
    // 上游返回 cache_control 相关 400：错误信息含字段名而非笼统 invalid_upstream
}
```

- [ ] **Step 2: 实现**

错误分类：`map_upstream_error` 对 `Unsupported{field}` 已在 public_message 输出笼统信息——改为携带 field 名（`Unsupported { field }` → message 含 `field`）；新增 `RequestRectifier` 空 trait + `noop_rectifier`（整流逻辑本任务不实现，标注 P7 观察）。

- [ ] **Step 3: 回归 + 提交**

全量测试；`git commit -m "feat: surface unsupported field names and add rectifier hook"`

---

### Task 7: 双模式真实对照测量

**Files:**
- Create: `~/.config/opencode/claude-codex-large-project-cache/cache-mode-measure.md`

**Interfaces:**
- Consumes: 部署后的 runtime（compat 与 deepseek 两种配置）。
- Produces: 两种模式在同一请求序列下的 cached_tokens/字节稳定对比。

- [ ] **Step 1: 对照组设计**

同一 Claude Code 真实请求序列（captured bodies：`092ce013`、`7bd7e166`、`255e7efef`），分别以 `cacheMode=compat` 与 `cacheMode=deepseek` 配置运行 runtime，各发 3 轮（冷→热→热），记录 `cache_read_input_tokens` 与请求字节 sha。

- [ ] **Step 2: 测量**

串行执行（间隔 ≥30s 防冷却；失败重试一次记为瞬态）。对比：① 两模式是否都命中（cached 值）② DeepSeek 模式 tools 排序后前缀是否仍稳定 ③ 首轮冷缓存差异。

- [ ] **Step 3: 报告**

写 cache-mode-measure.md：两种模式对比表、结论（DeepSeek 模式是否提升/等价）、对默认 `auto` 的建议。

---

### Task 8: 收尾固化

**Files:**
- Update: `docs/superpowers/plans/2026-08-12-cache-modes-and-adapter-harden.md`（勾选完成项）
- Update: `~/.config/opencode/claude-codex-large-project-cache/report.md`

**Interfaces:**
- Consumes: Task 1-7 结果。
- Produces: 最终报告（问题清单逐项状态、双模式结论、遗留观察项）。

- [ ] **Step 1: 汇总**

逐项核对问题清单 P1-P8 状态（修复/观察/不修+原因）。

- [ ] **Step 2: 固化建议**

建议提交的改动清单 + 建议保留的用户配置（cacheMode 写入 xu-chat-providers.json 的 zen 条目）示例；未授权不提交。
