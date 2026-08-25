# 三协议统一路由设计规范

日期：2026-08-11  
仓库：`/data/data/com.termux/files/home/xu-rs`  
范围：Codex Responses、OpenAI Chat Completions、Claude Code Anthropic Messages

## 1. 决策摘要

Rust `spec` Runtime 是协议转换的唯一事实源。它已经具备统一 IR、六个请求转换方向、响应语义校验、SSE 状态机、tool call 状态追踪和公开错误分类；Node `codex-chat2responses-proxy.mjs` 不再作为第二套日常转换实现。

默认入口保持 `127.0.0.1:9316`，由 `XU_SERVE_PORT` 控制。9317 不作为独立转换核心；如外部兼容仍要求 9317，后续只增加无状态转发入口，转发到同一 Rust Runtime，不复制转换逻辑。当前阶段不启动任何新监听器，也不修改客户端配置。

OpenCode 不强制经过 Runtime：它的原生 Chat/Responses provider 已经由 `@ai-sdk/openai-compatible` 或 `@ai-sdk/openai` 直接支持。Codex 和 Claude 只有在需要多 provider 或跨协议转换时走 9316。这样保留已验证的 OpenCode 直连行为，同时让所有需要桥接的流量共用同一实现。

## 2. 当前实现事实

### 2.1 入口与路由

`src/runtime/mod.rs::process_protocol_request` 的当前流程是：

1. 根据请求路径识别来源协议。
2. 解析 JSON 为 `RequestIr`。
3. 用模型后缀 `_provider_id` 解析 provider；只有一个 provider 时允许无后缀模型。
4. 使用 provider 的 `ProtocolKind` 选择上游目标协议。
5. 调用 `encode_upstream_request`，注入 provider-specific 请求修正。
6. 非流式请求读取 JSON，调用 `decode_response` 后再按来源协议调用 `encode_response`。
7. 流式请求使用独立的 upstream/output `StreamState`，先解码上游 SSE，再编码客户端 SSE。

公开路径必须保持以下映射：

| 客户端路径 | `WireProtocol` | 上游路径 |
|---|---|---|
| `/v1/messages` | `AnthropicMessages` | `messages` |
| `/v1/chat/completions` | `OpenAiChat` | `chat/completions` |
| `/v1/responses` | `OpenAiResponses` | `responses` |

### 2.2 IR 边界

`src/runtime/bridge/ir.rs` 中的 `RequestIr`、`ResponseIr`、`StreamEventIr` 是跨协议唯一中间层。它必须继续承载：

- system/developer/user/assistant/tool 角色和有序内容。
- text、image、document、thinking、redacted thinking、tool use、tool result。
- function tool 名称、描述、JSON Schema、call id、item id、顺序索引和参数对象。
- tool choice、max tokens、temperature、top-p、stop sequences、reasoning effort、stream 和 metadata。
- usage、finish/stop/status、错误和 Responses 特有扩展。

不能表示的字段必须在解析或目标编码阶段返回 `BridgeError::Unsupported`，不能悄悄删除。

### 2.3 运行时约束

以下约束属于协议入口契约，不因端口兼容而改变：

- `MAX_BODY_BYTES = 2 * 1024 * 1024`。
- SSE 单帧上限 `256 KiB`，流输出上限 `2 MiB`，流 item 上限 `256`。
- socket 读写超时 `60s`；provider 请求超时来自 `ProviderProfile.timeout_ms`，至少为 `1ms`。
- 最大并发连接 `8`。
- `x-spec-runtime-hop` 达到 `1` 时拒绝请求，防止本地代理环路。
- `/health` 必须报告 `service = spec-runtime`、监听地址、连接统计和三类入口能力。

## 3. 转换契约

### 3.1 六个方向

### 3.1 六个方向与同协议直通

| 来源 | 目标 | 默认结论 |
|---|---|---|
| Anthropic Messages | OpenAI Chat | 支持可表示的文本、工具和生成参数 |
| Anthropic Messages | OpenAI Responses | 支持 system 到 `instructions`、消息到 input、工具和生成参数 |
| OpenAI Chat | Anthropic Messages | 支持文本和 function tools；无法表达的并行工具语义拒绝 |
| OpenAI Chat | OpenAI Responses | 支持消息、function tools、tool result、reasoning effort |
| OpenAI Responses | Anthropic Messages | 仅支持没有不可逆 Responses 扩展的请求/响应 |
| OpenAI Responses | OpenAI Chat | 仅支持没有 opaque reasoning、item id 等不可逆扩展的请求/响应 |

**同协议直通（2026-08-11 修订）**：source 与 target 相同的请求不再经过 IR decode/encode。Runtime 只做模型 slug 改写、provider 归一化（如 Zen developer→system）、认证/hop 头注入，然后按字节转发上游响应（含流式与错误响应）。原生客户端（如 OpenCode→Zen）不再经历任何转换损耗；跨协议方向继续走完整 IR 校验。

### 3.2 明确支持

- Anthropic `system` 合并为 Chat 的 system message，或 Responses 的 `instructions`。
- Chat 的 developer message 在 Zen Chat 目标中按现有规则规范为 system；该 provider-specific 规则只允许位于 `normalize_provider_request`，不污染 IR。
- function/custom 工具按名称、描述和参数 schema 转换；Codex custom tool 的 `format` 必须被纳入 schema 提取规则。
- function call 的 `call_id`、Responses `item_id` 和流式参数增量由 `ToolCallTracker` / `StreamState` 维护。
- Anthropic adaptive thinking 的 effort 可映射为 IR `reasoning_effort`，再映射到有对应字段的目标。
- provider 能提供完整 Responses reasoning 内容时，保留在 `ResponseIr.extensions`；目标无法表示时拒绝。
- Chat `reasoning_content`（DeepSeek/GLM 等 provider 的思考过程）对 Anthropic 客户端呈现为**普通文本**：非流式解码为 `ContentIr::Text`、流式解码为 `StreamEventIr::TextDelta`，保持观测顺序插入。这保证推理内容对客户端可见，同时绝不产生未签名 thinking 块（Anthropic 客户端会把无签名的 thinking 视为非法）。同理，Chat 流式编码器对 `ReasoningDelta` 事件已 fail closed（`chat.stream.reasoning`）。
- `cache_control.type = ephemeral` 作为已知客户端提示接受；未知 cache 类型拒绝。

### 3.3 明确拒绝

- Chat 到 Anthropic 的 `parallel_tool_calls`，因为 Anthropic 目标不能保持相同的并行语义。
- 签名 thinking（`ContentIr::Thinking { signature: Some(_) }`，字段 `thinking.signature`）或密文 thinking（`ContentIr::RedactedThinking`，字段 `redacted_thinking`）编码到 Chat：请求与响应编码器一律返回 `BridgeError::Unsupported`，**绝不静默剥离**（此前为剥离后 200 回放，属丢失性行为）。请求侧拒绝发生在 upstream 派发前（400 `unsupported` 信封）；响应侧按运行时既有契约把响应编解码阶段的 `Unsupported` 折叠为 502 `invalid_upstream`（`map_upstream_bridge_error`），客户端不会收到被裁剪的 200。
- Responses opaque/encrypted reasoning 到 Chat 或 Anthropic，除非已有完整可逆内容。
- Responses item id、completed_at、conversation、moderation、prompt cache options 等在目标协议无法表示且会影响客户端状态的扩展。
- Anthropic document、未知 `cache_control.type`、未知 thinking 结构或未知 Responses reasoning 内容类型。
- Codex `namespace`、`web_search` 等客户端自有工具类型，除非后续设计出明确的等价目标语义；当前返回 `unsupported` 或按现有 client-owned 过滤规则记录可见诊断。
- `response.tool_result` 出现在上游 response 内容中，返回 `unsupported`。

### 3.4 响应和流终止

- 非流式响应必须先 `decode_response` 验证 id、model、status、finish/stop reason、工具索引和 usage，再编码给客户端。
- 流式响应必须经过 `SseDecoder`、目标协议 decoder、IR event 校验和目标 encoder。
- 一个流只能有一个 terminal state；terminal 后的新事件返回 `tool_state`，除非是上游已明确结束的传输尾帧。
- 上游 EOF 前必须出现合法完成或失败事件；缺少 terminal 返回 `invalid_upstream`。
- 已经向客户端写出 terminal 后，不再追加错误帧；terminal 前的转换错误使用目标协议可表达的失败事件。
- Responses 客户端必须最终收到对应的 `response.completed` 或失败事件；Chat/Anthropic 必须收到各自终止事件，不能只发送 `[DONE]` 而丢失协议终态。

## 4. 认证、provider 和模型选择

`src/providers/store.rs` 继续接受当前 JSON 的 `provider` 根对象，以及历史数组格式；协议字段按 `protocol`、`apiKind`、options 内同名字段顺序解析。`apiKey` 缺省值为空字符串是合法配置读取结果，但发送认证头的规则必须显式定义：

- OpenAI Chat/Responses：仅当 key 非空时发送 Bearer；空 key 不发送 `Authorization`。
- Anthropic Messages：仅当 key 非空时发送 `x-api-key`；`anthropic-version` 仍发送。
- `extra_headers` 原样附加，但报告和日志必须脱敏。
- provider 的 `base_url` 必须是 `http://` 或 `https://`；Runtime upstream URL 指向自身监听地址时拒绝，避免环路。

多 provider 请求必须使用 `model_provider` 生成的 `<model>_<provider_id>`；未知后缀或多 provider 下无后缀模型返回 `invalid_request`。`/v1/models` 返回同样的稳定 slug。

## 5. 客户端接入策略

### Codex

- 原生协议保持 Responses。
- 单一 Responses provider 可直连；多 provider、Chat provider 或 Anthropic provider 使用 `http://127.0.0.1:9316/v1`，并保持 `wire_api = "responses"`。
- Codex catalog 的 model slug 必须和 Runtime `resolve_model` 使用相同的 `_provider_id` 规则。

### Claude Code

- 原生协议保持 Anthropic Messages。
- 单一 Anthropic provider 可直连；需要 Chat/Responses 转换或多 provider 时使用 Runtime `/v1/messages`。
- 只读写 `.claude/settings.json`；`.claude.json` 是 Claude 启动/项目状态文件，禁止作为 provider 环境配置写入目标。现有 settings patch 仍必须保留用户未知字段。
- Responses-only provider 当前不能直接作为 Claude provider；必须经过已测试的 Responses 到 Anthropic 可逆子集。

### OpenCode

- 原生 Chat 使用 `@ai-sdk/openai-compatible`；原生 Responses 使用 `@ai-sdk/openai`。
- 默认保持 direct-file，不强制通过 Runtime。
- 若以后需要 OpenCode 也进入统一入口，必须新增显式 routing mode 和独立回归，不在本次恢复中隐式改变其直连行为。

## 6. 9317 兼容策略

Node 转换器当前有 provider 空 key 校验问题，并且与 Rust bridge 形成两套协议语义。它不应继续作为日常三协议实现。

若 9317 是外部兼容要求，唯一允许的后续实现是：

1. 9317 只负责接收 HTTP 并转发到 9316。
2. 不解析 JSON、不生成 provider auth、不维护独立 SSE 状态机。
3. 9316 不得把转发目标重新指向 9317；hop header 继续阻止环路。
4. 9317 health 必须明确显示 `compat-forwarder`，不能伪装成独立转换核心。

在确认确实存在依赖前，不启动 9317，也不保留 Node converter 作为后台 fallback。

## 7. 验收标准

### 协议与语义

- 六个方向的 clean fixtures 全部通过；每个方向都断言上游 path、model、system/instructions、tools、tool choice、生成参数和 stream 标志。
- tool call 的 id、参数增量、索引、结束事件和 tool result 全部保持一致。
- Responses reasoning 的可逆子集通过；opaque reasoning、未知 cache/document、并行工具等拒绝用例返回稳定的 `unsupported`。
- Chat reasoning 对 Anthropic 客户端呈现为有序文本块（无 thinking 块）；签名/密文 thinking 转 Chat 在请求侧 400 `unsupported`、响应侧 502 `invalid_upstream`，均无成功回放。
- 非法 JSON、缺少 model/messages、坏工具参数、坏 upstream response 分别映射到既有 `invalid_request`、`tool_state`、`invalid_upstream`。

### 流式与 Runtime

- 文本流、reasoning 流、工具流、usage、terminal 和上游尾帧均有测试。
- terminal 只出现一次；EOF 缺少 terminal、terminal 后额外协议事件和超过限制的帧均有确定错误。
- 9316 health、models、并发上限、socket timeout、body/frame/output 限制和 hop 防环路保持全绿。

### 客户端

- Codex 能看到最终文本，且 Responses 流包含合法终态。
- Claude 文本和 `pwd` 工具调用均能完成，且实际写入路径与读取路径一致。
- OpenCode direct-file 回归保持通过；若未启用 Runtime，不得声称它已经过协议桥。

## 8. 非目标

- 本设计不实现 tool execution；Runtime 只转换工具描述和模型返回的 tool call。
- 本设计不引入 async runtime、连接池、hot reload、failover 或持久化 service manager。
- 本设计不修改凭据值、不把真实 key 写入 fixture、不提交 git。
- 本设计不把 Node converter 的历史行为当作 Rust 兼容性标准。
