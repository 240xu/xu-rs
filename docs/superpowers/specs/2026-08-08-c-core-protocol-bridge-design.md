# C-core 三协议协议桥设计

日期：2026-08-08

## 目标

将 Xu Runtime 从当前的保守型双入口适配器升级为三协议全互转核心：

- Anthropic Messages
- OpenAI Chat Completions
- OpenAI Responses

三个协议都作为本地 Runtime 的一等入口和一等上游格式，支持非流式和 SSE 流式请求。六个跨协议方向必须共享工具调用、reasoning、usage、错误和事件生命周期实现。

行为基准采用 CC Switch GitHub 仓库 `farion1231/cc-switch` 的协议转换模块，当前参考 commit 为 `413c09e`。只移植协议语义、状态机和可复用测试，不移植 Tauri、桌面配置、OAuth、桌面注入或 CC Switch 的完整 forwarder 架构。

## 范围

### 必须实现

- `POST /v1/messages`
- `POST /v1/chat/completions`
- `POST /v1/responses`
- 三入口的非流式请求和 SSE 流式请求
- Anthropic、Chat、Responses 六个跨协议方向
- 文本、system/developer 指令和有序历史消息
- 图片和文档等可安全表示的 multimodal content
- `tools`、`tool_choice`、`parallel_tool_calls`
- assistant tool call、tool result 和多轮工具循环
- 稳定的 `id`、`call_id`、`item_id`、tool index 映射
- `finish_reason`、Responses status、Anthropic stop reason
- 输入、输出、缓存和 reasoning usage
- Responses reasoning 与 Anthropic thinking/redacted thinking 的 opaque round-trip
- malformed history、未知 block、上游语义错误和截断流的 fail-closed 行为
- body、SSE frame、工具参数和连接的资源上限

### 明确不属于本阶段

- OAuth、账号池和 provider 登录
- provider failover、circuit breaker 和多 endpoint HA
- Gemini 协议
- Tauri/WebView、桌面托盘、桌面配置迁移和桌面注入
- 音频、实时音频和无法在三协议间保持语义的私有媒体类型
- 请求日志、计费、quota 和 Web UI
- 将 CC Switch 的 provider adapter、数据库和路由器整体复制进 Xu

不支持的字段必须返回可读错误，不能静默转成普通文本或被丢弃。

## 架构

当前 `src/runtime.rs` 迁移为 `src/runtime/mod.rs`，保留现有 blocking `TcpListener` 和 `reqwest::blocking::Client` 网络边界。协议转换独立为纯函数/有限状态机，暂不把整个 Runtime 改写为异步服务。

```text
src/runtime/
  mod.rs                 # daemon、listener、路由、provider 选择
  http.rs                # HTTP 请求读取、headers、body 上限、响应写入
  bridge/
    mod.rs               # bridge 调度和公共错误
    ir.rs                # RequestIR、ResponseIR、StreamEventIR
    request.rs           # 入口解析和上游请求编码
    response.rs          # 上游响应解析和入口响应编码
    stream.rs            # SSE decoder、状态机和事件 encoder
    anthropic.rs         # Anthropic JSON/SSE 编解码
    chat.rs              # Chat JSON/SSE 编解码
    responses.rs         # Responses JSON/SSE 编解码
    tools.rs             # tool ID、参数累积、结果配对
    reasoning.rs         # thinking/signature/reasoning bridge
```

`src/lib.rs` 继续使用 `pub mod runtime;`，由 `src/runtime/mod.rs` 取代同级 `runtime.rs`。现有外部调用保持 `crate::runtime::*` 不变。

## 内部模型

内部模型是协议语义模型，不是第三方 API 的原样 JSON 类型。所有结构都使用 `serde_json::Value` 作为边界输入输出，以降低对供应商字段版本的耦合。

核心类型必须具有以下语义：

```rust
pub enum WireProtocol {
    AnthropicMessages,
    OpenAiChat,
    OpenAiResponses,
}

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

pub struct MessageIr {
    pub role: RoleIr,
    pub content: Vec<ContentIr>,
    pub name: Option<String>,
}

pub struct ResponseIr {
    pub meta: ResponseMetaIr,
    pub content: Vec<ContentIr>,
    pub usage: Option<UsageIr>,
    pub completion: CompletionIr,
    pub extensions: BTreeMap<String, Value>,
}

pub struct ResponseMetaIr {
    pub id: String,
    pub model: String,
}

pub struct UsageIr {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

pub struct CompletionIr {
    pub stop_reason: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
}

pub enum ContentIr {
    Text(String),
    Image(MediaIr),
    Document(MediaIr),
    Thinking { text: String, signature: Option<String> },
    RedactedThinking { data: String },
    ToolUse(ToolCallIr),
    ToolResult(ToolResultIr),
}

pub enum StreamEventIr {
    Started(ResponseMetaIr),
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallStarted(ToolCallIr),
    ToolCallArgumentsDelta { call_id: String, delta: String },
    ToolCallFinished { call_id: String },
    Usage(UsageIr),
    Completed(CompletionIr),
    Failed(BridgeError),
}

pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}
```

实际字段可以按当前代码风格调整，但必须保留以下不变量：消息顺序、有序 content block、工具 call 与 result 的关联、Responses item ID、reasoning opaque payload 和终止状态不能因为进入 IR 而消失。

协议特有字段仅在以下条件下进入 `extensions`：

1. 字段有明确 namespace 和版本标识。
2. 目标协议有对应的恢复路径。
3. 没有恢复路径时，解析器必须拒绝，而不是写入无语义的普通文本。

## 转换矩阵

入口和上游格式统一经过以下路径：

```text
入口 JSON/SSE
  -> protocol parser
  -> RequestIr / StreamEventIr
  -> target protocol encoder
  -> upstream request/target SSE
```

六个跨协议方向：

| 调用方 | 上游 | 请求 | 响应 |
|---|---|---|---|
| Anthropic | Chat | Anthropic -> IR -> Chat | Chat -> IR -> Anthropic |
| Chat | Anthropic | Chat -> IR -> Anthropic | Anthropic -> IR -> Chat |
| Anthropic | Responses | Anthropic -> IR -> Responses | Responses -> IR -> Anthropic |
| Responses | Anthropic | Responses -> IR -> Anthropic | Anthropic -> IR -> Responses |
| Chat | Responses | Chat -> IR -> Responses | Responses -> IR -> Chat |
| Responses | Chat | Responses -> IR -> Chat | Chat -> IR -> Responses |

同协议路径只做 model/endpoint/auth 注入和必要的安全归一化，避免无意义的转换损失。

## 流式状态机

非流式响应和流式响应必须使用相同的语义规则。非流式路径只是将所有 `StreamEventIr` 收集后一次性编码，不允许维护第二套工具或 reasoning 逻辑。

每个请求有独立的 `StreamState`，至少维护：

- 是否已发送 Started
- 当前输出 content block 和其目标协议 index
- `call_id -> ToolCallState` 映射
- 工具名称、工具 ID、参数 JSON fragment
- 已见 response/item ID
- usage 累积
- terminal 状态，防止 completed/failed 重复

状态机必须拒绝：

- 未 Started 先发 delta
- 同一个 tool block 重复 start
- 未 start 的 tool delta
- 工具参数 JSON 完成前伪造成功 tool call
- completed 后继续输出
- 上游 EOF 但没有合法 terminal event
- 同一个 tool call 同时被两个不同 ID 指向

目标编码器分别负责协议生命周期：

- Anthropic：`message_start`、`content_block_start`、delta、`content_block_stop`、`message_delta`、`message_stop`
- Chat：`chat.completion.chunk` 的 role/content/tool_calls delta 和 `[DONE]`
- Responses：`response.created`、output item、text/reasoning/tool delta、`response.completed` 或 `response.failed`

SSE decoder 要处理 CRLF、空行分帧、多行 data、注释行、`[DONE]` 和 JSON-on-stream 恢复。任何超出 frame 上限或 malformed JSON 都进入 `Failed`，不把残缺响应标记为成功。

## 工具与 reasoning

### 工具

- Chat function call 参数在多个 delta 中按 index 累积。
- Responses `function_call` 优先使用 `call_id`，并保留 `item_id`。
- Anthropic `tool_use.id` 映射为内部 call ID；回传时恢复为目标协议要求的 ID。
- 参数必须在结束时验证为合法 JSON；对象参数缺失时只使用协议允许的 `{}`，不能将任意字符串伪装成对象。
- assistant tool calls 后必须紧跟对应 tool result，不能被后续普通文本插入。
- 连续多个 assistant tool calls 必须保持同一 assistant turn。

### Reasoning

沿用 CC Switch 的 `reasoning_bridge` 思路：带 encrypted reasoning 的 Responses item 编码为版本化、URL-safe、无 padding 的 opaque envelope，放入 Anthropic `thinking.signature` 或 `redacted_thinking.data`。只有能够解码并验证 type 为 `reasoning` 的 envelope 才恢复成 Responses reasoning item。

- 不凭空生成签名。
- 无摘要但有 encrypted content 时使用 redacted thinking。
- 只有普通 summary text 时才生成普通 thinking。
- Chat 没有等价 opaque 字段时，保留可见 reasoning 文本；不可恢复的 encrypted payload 必须按策略拒绝或明确丢失告警，默认 fail-closed。

## 错误和安全边界

错误分类固定为：

- `400`：调用方 JSON、历史、工具配对或字段不合法。
- `413`：body、单个 SSE frame、工具参数或累计输出超过上限。
- `502`：上游返回 malformed JSON/SSE、协议终止状态不合法或转换失败。
- `504`：连接、首字节、读取或流空闲超时。
- `500`：Runtime 内部不可恢复错误。

必须保留当前 body 上限 `2 MiB`，并新增独立的 SSE frame、工具参数累计和输出累计上限。上游 URL 指向本地 Runtime 时拒绝转发或使用 hop guard，防止递归代理。

所有错误响应必须使用调用方协议的错误 envelope；日志只记录 provider、model、方向、阶段和错误类别，不记录 API key、完整 headers、tool 参数或 prompt 正文。

## 验收标准

### 单元测试

- 三协议请求解析和请求编码。
- 六方向非流式响应。
- 六方向文本流。
- 六方向单工具、多工具、并行工具和多轮 tool result。
- reasoning/thinking 和 opaque round-trip。
- usage、stop reason、incomplete、failed。
- malformed history、缺失 tool result、重复 event、EOF、超限。

### 集成测试

用本地 mock upstream 验证三种入口，不依赖真实 provider：

1. Anthropic client -> Chat mock
2. Chat client -> Anthropic mock
3. Anthropic client -> Responses mock
4. Responses client -> Anthropic mock
5. Chat client -> Responses mock
6. Responses client -> Chat mock

每条路径都覆盖非流式和流式工具循环。真实 Zen/Claude/Codex smoke 只作为发布前补充，不替代确定性的 mock 回归。

### 发布门禁

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
git diff --check
cargo build --release
```

## 迁移原则

1. 先拆 Runtime 文件边界，再迁移行为，避免一次性重写。
2. 每个转换器先写失败测试，再实现最小路径。
3. CC Switch 的行为用于对照，不能复制其桌面耦合。
4. 保留当前 `reqwest::blocking` 和 SOCKS 支持；异步化另立项目。
5. 新能力加入 health capability 前必须有对应测试。
6. 任意不确定的协议字段宁可 4xx/502，也不能静默改写。
7. 每个阶段都必须能独立构建、测试和回滚。
