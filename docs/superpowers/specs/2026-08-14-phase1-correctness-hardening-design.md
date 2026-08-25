# 阶段 1 正确性加固设计规范

日期：2026-08-14
仓库：`/data/data/com.termux/files/home/xu-rs`
范围：差异审查（`docs/XU_RUST_DIFFERENTIAL_REVIEW_2026-08-14.md`）6 项 Medium + 1 项 Low 修复，分两阶段发布；本文档只覆盖阶段 1（正确性、配置持久化、路由授权、协议安全、运行时边界）。

## 1. 决策摘要

阶段 1 按审查报告逐项 TDD 修复：每项先补失败测试再实现，保持提交粒度，不触碰 UI 布局/焦点/Help（阶段 2 范围）。

已批准的契约决策：

1. **Chat `reasoning_content` → Anthropic 客户端：降级为普通文本块**。不产生 unsigned thinking（Anthropic 契约要求 thinking 必须带签名），编码后自然是合法文本内容；Claude Code 仍能看到推理文本。语义上"推理标成正文"是可接受的代价。
2. **signed/redacted thinking → OpenAI Chat：恢复 fail-closed**。请求与响应两个方向均返回 `BridgeError::Unsupported`，绝不静默丢弃不可逆协议状态。
3. **未配置模型：严格拒绝**。模型 slug 必须解析到已配置条目，否则 400，绝不借用其他 provider 的 key。
4. **空 `apiKey` 契约固化**：不发送 `Authorization` 头，但 `baseURL` 必须合法存在。
5. **HTTP 请求行保留 query string**：`stats_period_filter` 按真实 period 过滤。

## 2. 修复项设计

### 2.1 Provider 持久化与模型映射（4 项 Medium）

#### F1. claudeSlots 读写路径统一

- **Canonical 存储位置 = provider JSON 根节点对象 `claudeSlots`**（与 CLI `--claude-slot` 现有写入形状一致）。
- `src/providers/store.rs` 解析时把根节点 `claudeSlots` 读入 `Provider` 结构新字段 `claude_slots: HashMap<String, String>`；若根节点缺失，回退读取旧 `model_metadata.claudeSlots`（兼容已存在数据）。
- `src/agents/claude.rs::claude_slot_value()` 与 UI 全部改读 `provider.claude_slots`，不再依赖 `model_metadata["claudeSlots"]`。
- 序列化时写回根节点 `claudeSlots`。
- 新增测试：以 CLI 形状 provider JSON（根节点含 `claudeSlots`）做 parse → serialize → parse round-trip，断言槽值不丢失。

#### F2. 直连 Claude/Codex 发送上游 request name

- `src/agents/claude.rs` 与 `src/agents/codex.rs` 的直连路径：`let m = request_name_for(first_model()?)?;` 后再构造 `ANTHROPIC_MODEL` / codex model 映射。
- 新增测试：provider 配置 `models: {"client-key": {"name": ..., "requestName": "upstream-name"}}`，断言直连映射写出的是 `upstream-name` 而非 `client-key`。

#### F3. 详情页模型行有序表示

- `provider_model_rows()` 改为按 `provider.models` 存储序迭代（数组型=数组原序；对象型=serde_json 对象 key 序，与既有 BTreeMap 排序行为一致），每行按 client name 在 `model_entries` 中查 `ModelEntry`。
- 显示、`detail_row_pick_args`（行编辑）、selection 初始化、序列化共用同一有序行模型。
- 新增回归测试：`["Zebra", "Alpha", "Mid"]` 非字母序数组，断言行 0 编辑的是 `Zebra`。

#### F4. 多选保存保留 alias 元数据

- fetch 选中列表的 selection 模型携带 `(client_name, request_name)` 对（或等价稳定键）。
- `detail_multi_models_args()` 序列化前先按 request_name 反查 `model_entries` 中的 `ModelEntry`；命中则输出完整对象（含 display/request name），未命中才退化为纯字符串。
- 新增测试：选中 `deepseek-v4-flash-free`（request_name）时，保存结果仍是 `{"client-key": {...}}` 对象而非纯字符串。

### 2.2 协议桥 reasoning/thinking 契约（2 项 Medium + 契约固化）

#### F5. Chat reasoning_content 降级为文本（→ Anthropic）

- `src/runtime/bridge/chat.rs` 解码器：provider `reasoning_content` 不再插入 `ContentIr::Thinking { signature: None }`，改为独立普通文本内容块，顺序保持推理在前、正文在后。
- 编码到 Anthropic 时自然成为 `text` 块，契约合法。
- 新增测试（非流式 + 流式）：Anthropic Messages 源 / OpenAI Chat 目标，断言响应/流中无 `thinking` 块、推理文本出现在文本内容中。

#### F6. signed/redacted thinking → Chat 恢复 fail-closed

- `src/runtime/bridge/chat.rs` 请求编码与响应编码：`ContentIr::Thinking { signature: Some(_), .. }` 与 `ContentIr::RedactedThinking { .. }` 恢复返回 `BridgeError::Unsupported`，移除 `continue` 静默丢弃。
- 恢复原 `encoder_strips_opaque_reasoning_in_chat_messages` 测试语义（断言 Unsupported），并新增：signed Anthropic thinking 请求路由到 Chat、redacted thinking 响应路由到 Chat 的集成测试。

#### F7. 协议 spec 文档更新

- 更新 `docs/superpowers/specs/2026-08-11-three-protocol-router-design.md`：
  - 写入"Chat `reasoning_content` 在 Anthropic 目标下降级为文本块"条款（含 2026-08-14 批准记录）。
  - 明确"opaque/encrypted reasoning 到 Chat 仍 fail-closed，不因上述降级而改变"。

### 2.3 Runtime 边界（3 项）

#### F8. 未配置模型严格拒绝

- 模型解析：slug 必须解析到已配置条目（client_name / request_name / `<name>_<provider>` 前缀形式均可），否则返回 **400**：
  ```json
  {"error":{"message":"model not configured: <slug>","type":"invalid_request_error"}}
  ```
- 拒绝发生在 provider 凭据查找之前，绝不借用其他 provider 的认证信息。
- 新增测试：`not-configured_fixture` 被拒且断言返回体中不含任何 provider 凭据。

#### F9. HTTP 请求行保留 query

- `src/runtime/http.rs` 解析：保留 `raw_path` 中的 query string（`split('?')` 不再截断到 path 段），query 解析为参数映射供 handler 使用。
- `src/runtime/mod.rs::stats_period_filter` 按真实 period 过滤；`/api/stats/tokens?period=24h` 只返回 24h 窗口。
- 新增测试：请求行解析单测 + stats period 集成测试。

#### F10. 同步 fetch 路径收敛（原 Low）

- `src/main.rs` 中 `m` 快捷键分支与编辑表单 fetch 全部改走既有 worker/receiver 异步路径（复用 `start_models_fetch_cached()` 的 worker 通道）。
- 新增 action 级契约测试/断言：fetch 动作不直接调用 `fetch_models_from_provider()`（不在事件循环线程做网络 I/O）。

### 2.4 空 key 契约与 stats 复核

#### F11. 空 key 契约固化

- `build_upstream_request`：`provider.api_key.trim()` 为空 → 不添加 `Authorization` 头；`baseURL` 缺失/非法 → 400 `proxy_error`。
- 新增测试：空 key 断言请求头无 `Authorization`；空 key + 空 baseURL 断言 400 错误路径。（真实验证已于 2026-08-13 完成：zen 空 key 经 9317 返回 200。）

#### F12. stats cache-hit denominator 复核

- 复核 `src/stats.rs` per-model cache-hit denominator 计算；确有问题则修复并加测试；无问题则只加说明注释，不改行为。

## 3. 错误与兼容性契约

- 失败测试先行：每个修复项先写失败测试，确认失败后实现，最后测试转绿。
- 对外错误类型沿用现有 `BridgeError` 分类与 `proxy_error` / `invalid_request_error` 命名。
- 不改变既有端口、环境变量、provider JSON 兼容读取（旧 `model_metadata.claudeSlots` 仍可读）。
- 不触碰阶段 2 范围：详情页布局/焦点/Help/异步加载 UI 状态。

## 4. 测试与验证

1. 每项修复的失败测试 → 实现 → 转绿。
2. 全量 `cargo test --quiet`（当前基线 822 passed）。
3. `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`git diff --check`、`cargo build --release`。
4. 阶段 1 全绿后请求独立 code review，再进入阶段 2。

## 5. 不做的事（YAGNI）

- UI 响应式布局、焦点只暴露可见控件、Help 返回原工作流、异步加载 UI 状态（阶段 2）。
- stats 模块整体重构。
- 未在审查报告中的行为改动（除非 TDD 过程中确证必须）。

## 6. 参考

- 审查报告：`docs/XU_RUST_DIFFERENTIAL_REVIEW_2026-08-14.md`（6 Medium + 1 Low，无 Critical/High）。
- 协议规范：`docs/superpowers/specs/2026-08-11-three-protocol-router-design.md`。
- 空 key 验证：2026-08-13 zen 空 key 经 9317 返回 200；9316 被未知服务占用，spec 已迁移 9317。

## 7. 阶段 1 状态（2026-08-14 完成）

- 计划：`docs/superpowers/plans/2026-08-14-phase1-correctness-hardening.md`（Task 0-8 全部完成）。
- 独立最终审查：1 Important（F1 别名 provider 表单保存）已修复并经两轮 scoped 复审通过；7 Minor 全部 park 并记录裁决（见 `.superpowers/sdd/2026-08-14-phase1-correctness-hardening/progress.md` 与 final-review-report）。
- 验证门禁全绿：`cargo test --quiet` 872 passed；`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo build --release`、`git diff --check` 全部通过。
- 未提交（本计划不授权提交）；全部改动保留在当前工作区待用户决定。
- 阶段 2（UI 布局/焦点/Help/异步加载 UI 状态）需另立计划。
