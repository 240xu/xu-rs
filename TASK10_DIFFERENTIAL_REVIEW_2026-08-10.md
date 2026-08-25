# Task 10：CC Switch 协议桥差异回归

## 基准

- 基准：`farion1231/cc-switch@413c09e0790c304506888ae24b9be72820aca126`
- 来源文件：`src-tauri/src/proxy/providers/transform.rs`、`transform_responses.rs`、`transform_codex_chat.rs`、`streaming.rs`、`streaming_responses.rs`、`streaming_codex_chat.rs`、`reasoning_bridge.rs`、`claude.rs`、`src-tauri/src/proxy/handlers.rs`。
- 比较范围：只比较协议桥行为语义，不复制 CC Switch 产品架构，也不把 CC Switch 兼容行为自动变成 Xu 产品承诺。
- 目标：验证 CC Switch 已覆盖、Xu 已覆盖以及 Xu 按 fail-closed 策略明确拒绝的协议桥行为。

## 结论

Task 10 的可执行差异 corpus 已加入 `tests/fixtures/runtime_bridge/ccswitch/`，覆盖 8 个请求/响应场景，包括 malformed input。manifest 现在记录 CC Switch 的机器可断言字段 oracle、Xu 合同和每个 case 的 `match`/`mismatch` outcome。Xu 当前可以稳定转换文本、系统指令、命名 tool choice、parallel tool calls、稳定 call id、部分 reasoning、cache-read usage 和 incomplete terminal 状态；对无法无损表示的 document、cache-write usage 等字段会返回明确的 `unsupported`，不静默丢数据。

这些能力应标为 `DONE`、`PARTIAL` 或明确的 intentional divergence，不能宣称为 CC Switch 的完整协议兼容。CC Switch 的 Chat 转换路径对错误类型的 `messages` 和 `document` block 存在静默丢失风险；Xu 选择在这些边界 fail closed。图片 URL/base64 已有 Chat 表示，但 document/PDF、音频、带签名 reasoning 到 Chat 等仍按目标协议能力拒绝。

## 差异矩阵

| 场景 | CC Switch 基线 | Xu 行为 | 结论 |
|---|---|---|---|
| Anthropic system + named tool choice -> Chat | 接受并转换 system、工具和命名选择 | 接受，字段级断言通过 | DONE |
| Chat parallel tool calls -> Responses | 保留 parallel 标志、顺序和 call id | 接受，两个 function call 与两个 result 顺序保持 | DONE |
| Responses reasoning -> Anthropic | 保留 summary 和 opaque encrypted reasoning | 接受，字段级断言 `thinking`、`signature`、system 位置 | PARTIAL：仅限可恢复目标 |
| Anthropic malformed Messages -> Chat | JSON 解析后进入转换；错误类型的 `messages` 变成空 Chat `messages` 数组 | `/messages` 错误类型返回 `invalid_request`/400 | intentional divergence：Xu fail closed |
| Anthropic document -> Chat | Chat 转换忽略 `document` block；同一消息中的 text 仍保留。Responses 路径另行转换为 `input_file` | `unsupported`/400，结构化字段记录为 `messages.content` | PARTIAL：Xu 避免静默丢 document |
| Anthropic cache-write usage -> Chat | 转换 usage | `unsupported`/400，结构化字段记录为 `usage.cache_write_tokens` | PARTIAL，计费安全优先 |
| Anthropic cache-read usage -> Chat | 转换 usage | prompt/completion/total/cache-read 保持 | DONE |
| Chat `finish_reason=length` -> Responses | 转为 incomplete | `status=incomplete`，usage 和 output_text 保持 | DONE |

## 实现证据

- Corpus：`tests/fixtures/runtime_bridge/ccswitch/task10_differential_manifest.json`
- CC Switch malformed-input evidence：`handlers.rs` 先将 JSON 解析为通用 `Value`，没有 `/messages` 数组 schema 校验；`transform.rs:174` 只在 `messages.as_array()` 成功时转换，`transform.rs:465` 对未知 content block 直接忽略。
- CC Switch document evidence：`transform.rs:393-465` 的 Chat block 分支没有 `document`；`transform_responses.rs:52-78` 将支持的 document 转为 Responses `input_file`。
- 新增请求 fixture：system/tool choice、malformed messages、document media、Responses reasoning。
- 新增响应 fixture：Anthropic cache-read usage、Chat length terminal。
- 执行入口：`tests/runtime_bridge_integration.rs` 中的 `task10_cc_switch_differential_manifest_executes_xu_contract`。
- 每个 case 都实际调用 `parse_request`/`encode_upstream_request` 或 `decode_response`/`encode_response`，并按 JSON Pointer 校验 Xu 字段；`match` case 同时校验 CC Switch 字段 oracle；`mismatch` case 校验 CC Switch/Xu 结果差异。
- 拒绝 case 校验错误分类、HTTP 状态、公开 reason code、非空 reason detail 和实际 `BridgeError::Unsupported { field }`；malformed case 校验错误字段对应输入问题路径。

## 验证

```text
cargo fmt --check
通过

cargo test --test runtime_bridge_integration task10_cc_switch_differential_manifest_executes_xu_contract -- --exact --nocapture
1 passed; 0 failed
```

## Fix Round 1

- 新增 malformed Messages request case：`/messages` 为 string 而非 array，真实走 Xu parser/encoder pipeline，断言 `invalid_request`、HTTP 400 和 `invalid request`。
- Responses reasoning -> Anthropic 现在按 JSON Pointer 断言 `system`、thinking block 的 `type`、`thinking` 文本和 opaque `signature` 前缀。
- manifest 每个 case 新增非空 `reason_code`/`reason_detail`、`error_field`、机器可断言的 CC Switch fields 和 `match`/`mismatch` outcome；拒绝测试验证实际结构化 unsupported field，未扩大生产错误 envelope。
- `CC_SWITCH_GAP_AUDIT.md` 已增加覆盖 8 个 case 的 DONE/PARTIAL 状态表和边界说明。

TDD 记录：先改测试合同时因缺少 `anthropic_malformed_messages_to_chat` 触发 RED；补齐 fixture/manifest 后专项测试 GREEN。

最终回归：

```text
cargo fmt --check
通过

cargo test runtime::bridge
103 passed; 0 failed

cargo test --test runtime_bridge_integration
160 passed; 0 failed
```

Task 10 完成后，仍需在完整回归中继续监控跨协议 streaming、provider-specific metadata、全量多媒体和 usage/cache 语义覆盖。
