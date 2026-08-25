# C-core 三协议协议桥 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Xu Runtime 升级为支持 Anthropic Messages、OpenAI Chat Completions、OpenAI Responses 三协议六方向互转的 C-core 协议桥，并可靠处理流式工具调用、reasoning、usage、错误和资源限制。

**Architecture:** 保留 Xu 当前 blocking `TcpListener`、`reqwest::blocking::Client`、provider 文件和模型后缀路由。把 `src/runtime.rs` 拆为 Runtime HTTP 层和纯协议 bridge 层；三个协议先解析为 `RequestIr`/`StreamEventIr`，再由目标协议 encoder 生成请求或 SSE。CC Switch `farion1231/cc-switch@413c09e` 只作为行为和测试基准，不复制 Tauri、桌面 forwarder、OAuth 或数据库。

**Tech Stack:** Rust 2021、`serde_json`、`reqwest 0.12` blocking + rustls + socks、现有 `TcpListener`/`TcpStream`、本地 mock upstream、Rust 单元测试和集成测试。

## Global Constraints

- 三个入口必须同时存在：`POST /v1/messages`、`POST /v1/chat/completions`、`POST /v1/responses`。
- 三个协议必须支持六个跨协议方向的非流式和 SSE 流式转换。
- 非流式和流式必须共享同一套 tool/reasoning 语义，不能维护两套不一致的转换规则。
- 保留 blocking Runtime，不在本计划内把服务改写为 Tokio/Axum。
- 不移植 Tauri、桌面配置、OAuth、桌面注入、Gemini、failover、quota 或计费。
- 不支持安全转换的字段必须 fail-closed，不能静默转成文本或丢弃。
- 当前 body 上限 `2 MiB` 必须保留；SSE frame、tool 参数和累计输出必须有独立上限。
- API key、完整 headers、prompt 正文和 tool 参数不能写入日志。
- 每个任务完成后运行该任务列出的测试；不要在当前无 Git 基线的仓库执行 `git add` 或提交。
- 修改安装脚本或打包文件前，必须先通过全部协议回归；发布包不属于早期任务。

## 文件地图

### 新建

- `src/runtime/mod.rs`：从原 `src/runtime.rs` 迁入 daemon、listener、provider 解析和 Runtime 调度。
- `src/runtime/http.rs`：HTTP 请求解析、body/header 限制、响应写入和 SSE frame 读取。
- `src/runtime/bridge/mod.rs`：bridge 公共入口、协议方向选择和 `BridgeError`。
- `src/runtime/bridge/ir.rs`：`WireProtocol`、`RequestIr`、`MessageIr`、`ContentIr`、`ResponseIr`、`StreamEventIr`。
- `src/runtime/bridge/request.rs`：三协议请求 parser 和目标请求 encoder。
- `src/runtime/bridge/response.rs`：三协议非流式响应 parser/encoder。
- `src/runtime/bridge/stream.rs`：SSE decoder、`StreamState`、事件验证和目标流 encoder。
- `src/runtime/bridge/anthropic.rs`：Anthropic Messages JSON/SSE 编解码。
- `src/runtime/bridge/chat.rs`：OpenAI Chat JSON/SSE 编解码。
- `src/runtime/bridge/responses.rs`：OpenAI Responses JSON/SSE 编解码。
- `src/runtime/bridge/tools.rs`：tool definition、ID、参数 fragment 和 result 配对。
- `src/runtime/bridge/reasoning.rs`：thinking/signature/reasoning opaque bridge。
- `tests/fixtures/runtime_bridge/`：不含凭据的三协议请求、响应和 SSE golden fixture。

### 修改

- `src/runtime.rs`：迁移为 `src/runtime/mod.rs`，原文件不得与目录模块并存。
- `src/lib.rs`：保持 `pub mod runtime;`，必要时导出测试所需的 bridge 类型。
- `src/domain/protocol.rs`：先确认现有 `ProtocolKind` 已覆盖 Anthropic/Chat/Responses；只有编译或路由测试证明缺少映射时才增加最小枚举转换，不重复定义 provider protocol。
- `Cargo.toml`：仅加入 bridge 实际需要的轻量依赖；当前 `reqwest` 的 `blocking/json/rustls-tls/socks` 必须保留。
- `README.md`：更新 Runtime 能力、三个入口、六方向转换和明确不支持边界。
- `CHANGELOG.md`：记录 C-core、流式工具和三入口变更。

### 不修改

- `src/providers/store.rs` 的 provider 配置格式。
- Claude/Codex/OpenCode 配置写回逻辑。
- TUI、MCP、Prompt、Skills、Project Profile。
- 安装包和 `dist/`，直到最后发布门禁通过。

---

### Task 1: 固定现状基线和协议 fixture

**Files:**
- Create: `tests/fixtures/runtime_bridge/anthropic_text_request.json`
- Create: `tests/fixtures/runtime_bridge/chat_tool_request.json`
- Create: `tests/fixtures/runtime_bridge/responses_reasoning_request.json`
- Create: `tests/fixtures/runtime_bridge/anthropic_text_response.json`
- Create: `tests/fixtures/runtime_bridge/chat_text_response.json`
- Create: `tests/fixtures/runtime_bridge/responses_text_response.json`
- Create: `tests/fixtures/runtime_bridge/anthropic_tool_stream.sse`
- Create: `tests/fixtures/runtime_bridge/chat_tool_stream.sse`
- Create: `tests/fixtures/runtime_bridge/responses_tool_stream.sse`
- Modify: `src/runtime.rs` tests only when a current regression needs a named fixture; after Task 2 these tests move with the file to `src/runtime/mod.rs`.

**Interfaces:**
- Consumes: 当前 `runtime.rs` 中的请求/响应转换行为和 CC Switch commit `413c09e`。
- Produces: 六方向的最小 text/tool/reasoning 输入输出样本，供后续 parser、encoder、stream 和 integration task 复用。

- [x] **Step 1: 记录当前基线命令和结果**

运行：

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

记录当前测试总数和任何已知失败；不要把真实 API key 写入 fixture。

- [x] **Step 2: 写入最小 fixture**

每种协议至少准备：纯文本请求、一个 function tool 请求、两个并行 tool 请求、tool result 回合、reasoning/thinking 请求、usage/terminal 响应和 malformed event。

- [x] **Step 3: 验证 fixture 可解析**

运行：

```sh
cargo test runtime -- --nocapture
```

Expected：基线测试通过；fixture 文件均为合法 JSON 或合法 SSE 样本。

**注意：** fixture 只描述协议数据，不包含 provider URL、token、家庭目录和真实 prompt。

---

### Task 2: 将 Runtime 拆成稳定模块边界

**Files:**
- Move: `src/runtime.rs` -> `src/runtime/mod.rs`
- Create: `src/runtime/http.rs`
- Verify: `src/lib.rs` continues to expose `pub mod runtime;`; no public API change is planned.

**Interfaces:**
- Consumes: `listen_addr`, `start_daemon`, `stop_daemon`, `health_check`, `serve`, `handle_connection`。
- Produces: `runtime::serve`, `runtime::start_daemon` 等现有公开函数保持相同签名；HTTP 层提供 `HttpRequest`、`read_http_request`、`write_json`、`write_sse`。

- [x] **Step 1: 先移动文件，不改行为**

保持原函数和测试内容不变，只调整 `mod` 引用和相对路径。

- [x] **Step 2: 抽取 HTTP 原语**

将 request line、headers、Content-Length、body 读取、`MAX_BODY_BYTES`、JSON response 和 SSE response 写入移到 `http.rs`。HTTP 原语不得解析任何协议字段。

- [x] **Step 3: 执行模块级回归**

```sh
cargo fmt --check
cargo test runtime
cargo clippy --all-targets -- -D warnings
```

Expected：与移动前测试结果相同，没有行为变化。

**注意：** Rust 不能同时存在 `src/runtime.rs` 和 `src/runtime/mod.rs`；移动完成后必须删除旧路径。

---

### Task 3: 建立 IR 和明确的 bridge 错误类型

**Files:**
- Create: `src/runtime/bridge/mod.rs`
- Create: `src/runtime/bridge/ir.rs`
- Create: `src/runtime/bridge/tools.rs`
- Modify: `src/runtime/mod.rs`

**Interfaces:**
- Consumes: `domain::ProtocolKind`、`serde_json::Value`。
- Produces:

```rust
pub enum WireProtocol { AnthropicMessages, OpenAiChat, OpenAiResponses }
pub struct RequestIr {
    pub protocol: WireProtocol,
    pub model: String,
    pub messages: Vec<MessageIr>,
    pub tools: Vec<ToolDefinitionIr>,
    pub tool_choice: Option<ToolChoiceIr>,
    pub generation: GenerationIr,
    pub stream: bool,
    pub metadata: Option<Value>,
    pub extensions: BTreeMap<String, Value>,
}
pub struct MessageIr { pub role: RoleIr, pub content: Vec<ContentIr>, pub name: Option<String> }
pub enum ContentIr { Text(String), Image(MediaIr), Document(MediaIr), Thinking { text: String, signature: Option<String> }, RedactedThinking { data: String }, ToolUse(ToolCallIr), ToolResult(ToolResultIr) }
pub struct ResponseIr { pub meta: ResponseMetaIr, pub content: Vec<ContentIr>, pub usage: Option<UsageIr>, pub completion: CompletionIr, pub extensions: BTreeMap<String, Value> }
pub enum StreamEventIr { Started(ResponseMetaIr), TextDelta { text: String }, ReasoningDelta { text: String }, ToolCallStarted(ToolCallIr), ToolCallArgumentsDelta { call_id: String, delta: String }, ToolCallFinished { call_id: String }, Usage(UsageIr), Completed(CompletionIr), Failed(BridgeError) }
pub struct SseFrame { pub event: Option<String>, pub data: String }
pub enum BridgeError { InvalidRequest, Unsupported { field: String }, InvalidUpstream, ToolState, ResourceLimit, Timeout, Internal }
```

- [x] **Step 1: 为 IR 写 round-trip 失败测试**

覆盖：消息顺序、多个 content block、工具 call/result、reasoning envelope、usage 和 terminal status。测试必须断言结构化字段，不使用整段 JSON 字符串比较作为唯一断言。

- [x] **Step 2: 实现 IR 类型和显式错误分类**

所有 `BridgeError` 都必须能够转换为：调用方 HTTP status、调用方错误 envelope、脱敏日志类别。

- [x] **Step 3: 实现工具状态模型**

`ToolCallState` 至少包含 `call_id`、`item_id`、`index`、`name`、参数 fragment、完成状态；参数 fragment 使用长度受限的 `String`，不能无限增长。

- [x] **Step 4: 运行 bridge 单测**

```sh
cargo test runtime::bridge
```

Expected：IR 和工具状态测试通过；此任务不改变线上路由。

**注意：** 不要用 `Unknown(Value)` 作为所有未知 block 的兜底类型，否则后续 encoder 会不可避免地静默丢字段。

---

### Task 4: 实现三协议请求解析和请求编码

**Files:**
- Create: `src/runtime/bridge/request.rs`
- Create: `src/runtime/bridge/anthropic.rs`
- Create: `src/runtime/bridge/chat.rs`
- Create: `src/runtime/bridge/responses.rs`
- Modify: `src/runtime/bridge/mod.rs`

**Interfaces:**
- Consumes: `RequestIr` 和三个入口的 JSON。
- Produces:

```rust
pub fn parse_request(protocol: WireProtocol, body: &Value) -> Result<RequestIr, BridgeError>;
pub fn encode_upstream_request(ir: &RequestIr, target: WireProtocol, model: &str) -> Result<Value, BridgeError>;
```

- [ ] **Step 1: 先写六方向 request fixture 测试**

每个方向至少断言：system/developer 位置、tool definitions、tool choice、model、stream、generation 参数和消息顺序。

- [ ] **Step 2: 实现 Anthropic parser/encoder**

保留有序 content block；将 `tool_use` 和 `tool_result` 映射为 IR；默认 Anthropic `max_tokens` 由调用方提供，缺失时按现有 provider 上限策略处理并测试。

- [ ] **Step 3: 实现 Chat parser/encoder**

合并同一 assistant turn 的连续 tool calls；把 `role: tool` 绑定到 `tool_call_id`；将 system/developer 明确区分。

- [ ] **Step 4: 实现 Responses parser/encoder**

区分 `message`、`function_call`、`function_call_output`、`reasoning`、instructions 和 input item；保留 response/item/call IDs。

- [ ] **Step 5: 验证请求层**

```sh
cargo test runtime::bridge::request
cargo test runtime::bridge::anthropic
cargo test runtime::bridge::chat
cargo test runtime::bridge::responses
```

**注意：** Responses 的 system/instructions 不能简单当作普通 user message；Chat 的 system messages 在编码到只允许头部 system 的目标时要按 CC Switch 规则折叠到头部。

---

### Task 5: 实现三协议非流式响应转换

**Files:**
- Create: `src/runtime/bridge/response.rs`
- Modify: `src/runtime/bridge/anthropic.rs`
- Modify: `src/runtime/bridge/chat.rs`
- Modify: `src/runtime/bridge/responses.rs`

**Interfaces:**
- Consumes: 上游 JSON response 和目标 `WireProtocol`。
- Produces:

```rust
pub fn decode_response(protocol: WireProtocol, body: Value) -> Result<ResponseIr, BridgeError>;
pub fn encode_response(ir: ResponseIr, target: WireProtocol) -> Result<Value, BridgeError>;
```

- [ ] **Step 1: 写六方向 response 测试**

覆盖纯文本、tool calls、tool result 后的下一轮、reasoning、usage、正常终止和错误终止。

- [ ] **Step 2: 实现 terminal validation**

Responses 必须检查 `status`/`error`；Anthropic 必须检查 stop reason 和 content block；Chat 必须检查 choices、finish reason 和 tool calls。HTTP 2xx 但 JSON 表示错误时归类为上游语义错误，不返回假成功。

- [ ] **Step 3: 实现 response ID 和 usage 映射**

Responses 的 response/item ID、Chat completion ID、Anthropic message ID 进入 `ResponseMetaIr`，编码到目标协议时生成目标必需字段，但不能重用不兼容的 ID 语义。

- [ ] **Step 4: 验证非流式路径**

```sh
cargo test runtime::bridge::response
```

**注意：** 非流式路径必须复用与流式路径相同的 tool/reasoning 归一化函数，不能复制一套“简化版”转换。

---

### Task 6: 实现 SSE decoder、StreamState 和三种目标 encoder

**Files:**
- Create: `src/runtime/bridge/stream.rs`
- Modify: `src/runtime/http.rs`
- Modify: `src/runtime/bridge/anthropic.rs`
- Modify: `src/runtime/bridge/chat.rs`
- Modify: `src/runtime/bridge/responses.rs`

**Interfaces:**
- Consumes: 上游 SSE bytes 和上游协议。
- Produces:

```rust
pub struct SseDecoder { /* CRLF, multi-line data, frame limit */ }
pub struct StreamState { /* blocks, calls, IDs, usage, terminal */ }
pub fn decode_stream_frame(protocol: WireProtocol, frame: &SseFrame, state: &mut StreamState) -> Result<Vec<StreamEventIr>, BridgeError>;
pub fn encode_stream_events(protocol: WireProtocol, events: &[StreamEventIr], state: &mut StreamState) -> Result<Vec<SseFrame>, BridgeError>;
```

- [ ] **Step 1: 写 decoder 失败测试**

覆盖 CRLF、多个 data 行、空事件、注释、`[DONE]`、超长 frame、malformed JSON 和上游 EOF。

- [ ] **Step 2: 写状态机失败测试**

覆盖 text delta、reasoning delta、tool start、参数分片、tool finish、usage、completed、failed 和非法顺序。

- [ ] **Step 3: 实现 decoder 和状态验证**

所有事件先变为 `StreamEventIr`；状态机拒绝 duplicate start、delta-before-start、completed 后事件和未完成 tool JSON。

- [ ] **Step 4: 实现 Anthropic encoder**

生成合法的 `message_start`、content block 生命周期、message delta、usage、message stop；tool block 必须恰好 start/stop 一次。

- [ ] **Step 5: 实现 Chat encoder**

生成 role/content/tool_calls delta 和 `[DONE]`；同一 assistant turn 的多个 tool calls 共享一致 index/id。

- [ ] **Step 6: 实现 Responses encoder**

生成 `response.created`、output item、text/reasoning/tool delta、usage、`response.completed`/`response.failed`，确保 output item 与 call ID 不交叉。

- [ ] **Step 7: 验证流式状态机**

```sh
cargo test runtime::bridge::stream
```

**注意：** upstream EOF 不能自动当作成功完成；没有合法 terminal event 时必须输出目标协议错误终止或返回 502。

---

### Task 7: 接入 reasoning、thinking、multimodal 和 tool 细节

**Files:**
- Create: `src/runtime/bridge/reasoning.rs`
- Modify: `src/runtime/bridge/tools.rs`
- Modify: `src/runtime/bridge/anthropic.rs`
- Modify: `src/runtime/bridge/chat.rs`
- Modify: `src/runtime/bridge/responses.rs`

**Interfaces:**
- Consumes: `ContentIr`、`ToolCallState`、CC Switch `reasoning_bridge.rs` 的 envelope 规则。
- Produces: 可逆 reasoning block、严格工具配对、image/document data URL 或 remote URL 的安全映射。

- [ ] **Step 1: 写 reasoning round-trip 测试**

测试有 summary + encrypted content、只有 encrypted content、只有可见 summary、无 signature 和错误 signature。

- [ ] **Step 2: 移植版本化 opaque envelope**

使用 URL-safe base64，无 padding；解码后必须检查 `type == "reasoning"`。禁止任意字符串被当成签名恢复。

- [ ] **Step 3: 写 tool pairing/property 测试**

至少验证：ID 缺失、ID 重复、index 改变、参数分片合并、非对象参数、多个并行 call、tool result 错配和 assistant turn 插入普通文本。

- [ ] **Step 4: 实现 multimodal 映射**

只支持三协议能表达的 image/document 类型；未知媒体和不安全的本地路径返回 `Unsupported`。不把二进制内容无限缓存到内存。

- [ ] **Step 5: 运行专项测试**

```sh
cargo test runtime::bridge::reasoning
cargo test runtime::bridge::tools
```

**注意：** Chat 没有 Anthropic signature 等价字段；无法保真的 encrypted reasoning 不得伪装成普通文本而丢失其语义。

---

### Task 8: 接入 Runtime 路由、provider 上游请求和错误映射

**Files:**
- Modify: `src/runtime/mod.rs`
- Modify: `src/runtime/http.rs`
- Modify: `src/runtime/bridge/mod.rs`
- Verify: `src/domain/protocol.rs` covers all three wire protocols; add only the smallest tested conversion mapping if compilation proves one is absent.

**Interfaces:**
- Consumes: `parse_request`, `encode_upstream_request`, `decode_response`, `decode_stream_frame`、现有 `resolve_model`/provider headers。
- Produces: 三个入口均能根据 provider protocol 选择透传或 bridge；错误映射为 `400/413/502/504/500`。

- [ ] **Step 1: 写 route tests**

验证 `POST /v1/messages`、`POST /v1/chat/completions`、`POST /v1/responses` 的 method/path 分发和 unsupported path 错误。

- [ ] **Step 2: 加入 Chat 入口**

解析 Chat 请求并把响应按 Chat 格式返回；不能把 Chat 当成内部私有格式而隐藏其 endpoint。

- [ ] **Step 3: 替换旧 reject 逻辑**

删除只要发现 `tools` 就拒绝的旧门禁；改由 bridge parser、tool state 和 target encoder 做能力判断。

- [ ] **Step 4: 接入非流式 bridge**

请求走 IR -> upstream JSON；响应走 upstream JSON -> IR -> caller JSON。保留 provider model suffix 选择。

- [ ] **Step 5: 接入流式 bridge**

上游 response body 按 frame 解码，事件通过目标 encoder 写回调用方；每个连接使用独立 `StreamState`。

- [ ] **Step 6: 加 hop guard 和资源限制**

当 provider base URL 指向本地 Runtime listen address 时拒绝递归；加入请求 hop header、body/frame/tool/output 上限和连接超时。

- [ ] **Step 7: 验证 Runtime**

```sh
cargo test runtime
cargo clippy --all-targets -- -D warnings
```

**注意：** HTTP 2xx 不代表上游语义成功；provider 返回错误 JSON、Responses failed status 或不完整 SSE 都必须进入 502/504 分类。

---

### Task 9: 添加本地 mock upstream 的六方向集成回归

**Files:**
- Create: `tests/runtime_bridge_integration.rs`
- Create: `tests/fixtures/runtime_bridge/` 中完整 request/response/SSE fixture。
- Modify: `src/runtime/mod.rs`：增加 `#[cfg(test)]` 的 `handle_request_for_test`，接收 `HttpRequest` 和 provider slice，返回 status、headers、body；不导出生产 API。

**Interfaces:**
- Consumes: 三入口和本地 mock upstream。
- Produces: 不依赖 Zen、Claude 或 Codex 网络的确定性端到端回归。

- [ ] **Step 1: 实现 mock upstream**

mock 必须记录收到的 path、headers、JSON body，并按测试 fixture 返回非流式 JSON 或分片 SSE。API key 只使用测试字符串，日志不得打印 body。

- [ ] **Step 2: 实现六方向非流式测试**

分别测试 Anthropic↔Chat、Anthropic↔Responses、Chat↔Responses，断言请求格式、tool IDs、消息顺序、响应格式和 usage。

- [ ] **Step 3: 实现六方向流式测试**

每条方向分别测试文本、单工具、并行工具和 reasoning；断言目标 SSE 的事件顺序和唯一终止事件。

- [ ] **Step 4: 实现错误和资源测试**

测试 malformed JSON、malformed SSE、未匹配 tool result、超长参数、超长 frame、upstream EOF、上游 2xx semantic error 和 timeout。

- [ ] **Step 5: 运行集成门禁**

```sh
cargo test --test runtime_bridge_integration -- --nocapture
```

Expected：不需要任何外部代理、API key 或运行中的 Runtime daemon。

---

### Task 10: 以 CC Switch 为行为基准做差异回归

**Files:**
- Modify: `CC_SWITCH_GAP_AUDIT.md`
- Read: `docs/superpowers/specs/2026-08-08-c-core-protocol-bridge-design.md`，确认实现没有超出已批准范围。
- Create/Modify: `tests/fixtures/runtime_bridge/ccswitch/`

**Interfaces:**
- Consumes: CC Switch `transform.rs`、`transform_responses.rs`、`transform_codex_chat.rs`、对应 streaming 模块和 reasoning bridge。
- Produces: Xu 与 CC Switch 在已声明支持字段上的行为差异表。

- [ ] **Step 1: 建立行为清单**

逐项记录 system/instructions、tool choice、parallel tools、call ID、reasoning、usage、terminal status、media 和 malformed input 的预期行为。

- [ ] **Step 2: 为每个差异写 Xu 测试**

每个差异必须有输入、目标协议、预期关键字段和拒绝原因；不得只写“与 CC Switch 一致”。

- [ ] **Step 3: 更新 gap audit**

把原来的 Runtime 缺口按 `DONE/PARTIAL/MISSING` 更新，只标记有测试和端到端证据的能力为 `DONE`。

- [ ] **Step 4: 运行差异回归**

```sh
cargo test runtime::bridge
cargo test --test runtime_bridge_integration
```

**注意：** CC Switch 的兼容行为不自动等于 Xu 的产品承诺；任何为了兼容而放宽安全校验的行为必须单独记录并经过审查。

---

### Task 11: 真实三客户端 smoke test

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`
- Do not modify: credentials or user configuration files in the repository.

**Interfaces:**
- Consumes: release binary、现有 provider store 和用户明确配置的 Zen/Claude/Codex provider。
- Produces: 三客户端真实文本、工具和流式测试结果。

- [ ] **Step 1: 构建并安装当前 release**

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
install -m 755 target/release/spec /data/data/com.termux/files/usr/bin/spec
```

- [ ] **Step 2: 启动并验证 Runtime health**

```sh
spec runtime stop || true
spec runtime start
curl -fsS http://127.0.0.1:9316/health
curl -fsS http://127.0.0.1:9316/v1/models
```

health 必须准确报告三个入口、streaming、tools、reasoning 的真实能力。

- [ ] **Step 3: 验证三入口文本请求**

分别用 Claude Code、Codex 和 OpenCode 发起最小文本请求，记录 status、模型、响应协议和耗时。

- [ ] **Step 4: 验证工具调用**

使用只读工具，例如 `pwd` 或 `printf`，验证：tool call 产生、真实工具执行、tool result 回传、下一轮 assistant 回复完成。

- [ ] **Step 5: 验证流式工具调用**

验证每个客户端都收到合法终止事件；任何上游 EOF 或 proxy TLS 错误必须报告为失败，不能记录为成功。

- [ ] **Step 6: 清理临时 proxy/采集进程**

测试结束后检查临时端口、capture 脚本和 `spec-runtime.pid`，确保没有遗留代理劫持用户后续请求。

---

### Task 12: 文档、发布门禁和回滚记录

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`
- Modify: `CC_SWITCH_GAP_AUDIT.md`
- Do not modify: `dist/` until every prior task passes.

**Interfaces:**
- Consumes: 集成测试和真实 smoke 结果。
- Produces: 用户可理解的 capability、限制、入口、错误和回滚说明。

- [ ] **Step 1: 更新 README**

说明三个入口、六方向互转、支持的 tools/reasoning/streaming，以及明确拒绝的字段和外部依赖。

- [ ] **Step 2: 更新 gap audit**

只把有 mock 集成测试和真实 smoke 证据的能力标为 `DONE`；其余保持 `PARTIAL`。

- [ ] **Step 3: 执行最终门禁**

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
git diff --check
cargo build --release
```

- [ ] **Step 4: 保存回滚信息**

记录旧 binary 的 checksum、Runtime PID、监听端口和配置备份路径。发现真实客户端回归时先停止 Runtime、恢复旧 binary，再恢复用户配置；不得用 `git reset --hard` 覆盖未跟踪工作。

**注意：** 打包不是 C-core 早期任务；只有 Task 1-11 全部通过后才允许运行 `scripts/package.sh`。

## 高风险注意点清单

### 协议语义

- Chat 连续 function call 必须在同一个 assistant turn 内。
- tool result 必须按 call ID 精确配对，不能只按数组位置。
- Responses 的 `call_id`、`item_id`、Chat 的 `tool_call.id` 和 Anthropic 的 `tool_use.id` 不可混为一个字段后丢失原始信息。
- system/developer/instructions 的位置和优先级必须显式转换。
- Responses reasoning 有 encrypted content 时不能生成假的 Anthropic signature。
- `content_block_start/stop`、Responses item lifecycle 和 Chat `[DONE]` 只能各自出现合法次数。
- upstream EOF 不得自动视为成功完成。

### Runtime

- 三入口必须有独立路由测试；不能把 Chat 仅作为内部格式。
- provider base URL 指向本地 Runtime 时必须阻止递归转发。
- upstream 2xx 的错误 JSON、Responses failed status 和 malformed SSE 必须 fail-closed。
- request、SSE frame、tool argument、累计输出和 header 必须有上限。
- blocking Runtime 的每个连接不能阻塞接受循环；如果本阶段不做并发改造，必须在计划中明确 mock/压力测试的限制，并禁止宣称生产级并发。

### 安全和运维

- 日志不得包含 key、authorization、prompt、tool argument 和完整 upstream body。
- 测试只用本地 mock 或用户明确启用的 provider；不把真实凭据写入 fixture、源码和提交。
- 真实 smoke 失败时区分网络/TLS、provider 错误、协议转换错误和客户端消费错误。
- health capability 只能反映已通过测试的功能。
- 安装新 binary 前保留旧 binary checksum；测试结束清理 one-shot capture 和临时 proxy。

## 阶段完成定义

C-core 只有在以下条件全部满足时才算完成：

1. 六方向非流式集成测试通过。
2. 六方向流式文本和工具集成测试通过。
3. reasoning/thinking opaque round-trip 测试通过。
4. malformed input、semantic error、EOF、timeout 和 resource limit 测试通过。
5. 三个真实客户端至少各完成一次文本和工具 smoke。
6. `fmt`、`test`、`clippy`、`diff --check` 和 release build 全部通过。
7. README、CHANGELOG 和 gap audit 与实际 capability 一致。
