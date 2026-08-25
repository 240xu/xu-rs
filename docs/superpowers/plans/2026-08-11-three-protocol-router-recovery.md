# 三协议统一路由恢复实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 以 Rust `spec` Runtime 为唯一协议转换实现，稳定 Codex Responses、OpenAI Chat Completions 和 Claude Code Anthropic Messages 的跨协议本地路由，并保留已验证的 OpenCode 直连行为。

**Architecture:** `spec serve` 默认监听 `127.0.0.1:9316`，通过 `/v1/messages`、`/v1/chat/completions` 和 `/v1/responses` 接收三种 wire protocol。请求先进入 `RequestIr`，按 model slug 选择 provider，再转换到 provider 的目标协议；响应和 SSE 事件沿相反方向经过同一 IR 和状态校验。Node converter 不再作为独立日常链路；只有确认存在 9317 外部依赖时，才实现不解析协议的 9317 到 9316 转发器。

**Tech Stack:** Rust、`serde_json`、`reqwest::blocking`、TCP HTTP、SSE、Codex TOML、Claude JSON settings、OpenCode JSON provider 配置。

## Global Constraints

- 不覆盖或回滚 `/data/data/com.termux/files/home/xu-rs` 现有未提交改动。
- 任何服务启动、停止、端口切换或客户端配置写入前，先使用已创建的备份并取得用户确认。
- 所有真实请求使用硬超时；协议转换先用仓库 mock upstream 和集成测试验证，再做真实 smoke test。
- 不把 API key、session cookie 或其他凭据写入报告、测试 fixture 或 git。
- 不修改 `/data/data/com.termux/files/home/.claude.json`；Claude provider 环境配置只允许写入 `.claude/settings.json`。
- 未经用户单独授权不提交 git commit。
- 不新增依赖；不引入 async runtime、hot reload、failover 或持久化 service manager。

---

### Task 1: 固定当前状态和设计基线

**Files:**
- Read: `/data/data/com.termux/files/home/xu-rs/docs/superpowers/specs/2026-08-11-three-protocol-router-design.md`
- Read: `/data/data/com.termux/files/home/.codex/config.toml`
- Read: `/data/data/com.termux/files/home/.codex/xu-chat-providers.json`
- Read: `/data/data/com.termux/files/home/.codex/xu-active-providers.json`
- Read: `/data/data/com.termux/files/home/.codex/xu-client-routes.json`
- Read: `/data/data/com.termux/files/home/.claude/settings.json`
- Read: `/data/data/com.termux/files/home/.claude.json`
- Update: `/data/data/com.termux/files/home/xu-rs/.superpowers/sdd/2026-08-11-three-protocol-router-recovery/progress.md`

**Interfaces:**
- Consumes: current provider files, agent config files and listener state.
- Produces: confirmed Rust-first architecture, listener ownership, config paths and rollback directory.

- [x] **Step 1: Record repository and listener state**

Run:

```bash
git status --short --branch
ps -ef | awk '$0 ~ /spec serve|codex-chat2responses-proxy|zen-capture-proxy/ && $0 !~ /awk/ {print}'
curl -sS --max-time 5 http://127.0.0.1:9316/health
curl -sS --max-time 5 http://127.0.0.1:9317/health
```

Expected: existing Rust/fixture changes remain untouched; no process is assumed healthy without a successful health response.

- [x] **Step 2: Verify configuration ownership**

Expected paths: Codex `~/.codex/config.toml`, OpenCode `~/.config/opencode/opencode.json`, Claude settings `~/.claude/settings.json`. Treat `~/.claude.json` as state data and never include it in an agent patch.

- [x] **Step 3: Verify the rollback copies**

Use `/data/data/com.termux/files/usr/tmp/opencode/three-protocol-router-backup-20260811_151208` and compare each copied Codex file with `cmp` before any configuration edit.

- [x] **Step 4: Record the design decision**

Update the SDD ledger with the Rust-first decision and state that no live process or client configuration has been changed.

---

### Task 2: Lock the six-direction protocol contract with tests

**Files:**
- Modify: `/data/data/com.termux/files/home/xu-rs/tests/runtime_bridge_integration.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/src/runtime/bridge/request.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/src/runtime/bridge/response.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/src/runtime/bridge/stream.rs`
- Test fixtures: `/data/data/com.termux/files/home/xu-rs/tests/fixtures/runtime_bridge/`

**Interfaces:**
- Consumes: `protocol_for_path`, `parse_request`, `encode_upstream_request`, `decode_response`, `encode_response`, `decode_stream_frame` and `encode_stream_events`.
- Produces: deterministic coverage for all six source/target directions and their refusal boundaries.

- [ ] **Step 1: Add a request-direction test that names every route**

Use the existing `DIRECTIONS` table and assert that each case has the expected source path and target protocol. The test must call the real parser and encoder, not a hand-built expected-only fixture:

```rust
for direction in DIRECTIONS {
    let input = fixture_for_direction(direction.name);
    let ir = runtime::bridge::parse_request(direction.source.into(), &input)
        .expect("fixture request parses");
    let body = runtime::bridge::encode_upstream_request(
        &ir,
        direction.target.into(),
        "fixture-target-model",
    )
    .expect("representable direction encodes");
    assert_eq!(body["model"], "fixture-target-model");
    assert_eq!(body["stream"], ir.stream);
}
```

Add `fn fixture_for_direction(name: &str) -> Value` beside the existing fixture helper. Map `anthropic_to_chat` and `anthropic_to_responses` to `anthropic_clean_request.json`, `chat_to_anthropic` and `chat_to_responses` to `chat_clean_request.json`, and `responses_to_anthropic` and `responses_to_chat` to `responses_compatible_request.json`. The helper must panic on any name outside the six `DIRECTIONS` entries.

- [ ] **Step 2: Run the new test and capture the first failure**

Run:

```bash
cargo test --test runtime_bridge_integration six_direction -- --nocapture
```

Expected initial result: either PASS if the existing coverage already satisfies the contract, or a specific conversion assertion identifying the smallest missing behavior. Do not broaden the IR to make an unsupported field silently pass.

- [ ] **Step 3: Add refusal tests for irreversible fields**

Add tests named `responses_to_chat_rejects_opaque_reasoning`, `chat_to_anthropic_rejects_parallel_tool_calls`, and `unknown_anthropic_cache_control_is_rejected`. Assert `BridgeError::Unsupported` and the exact field names `responses.reasoning`, `parallel_tool_calls`, and `cache_control.type`.

- [ ] **Step 4: Run the bridge unit and integration subset**

```bash
cargo test runtime::bridge -- --nocapture
cargo test --test runtime_bridge_integration -- --nocapture
```

Expected: all six directions and all refusal tests pass without changing the existing custom tool or Zen tail fixture semantics.

---

### Task 3: Make provider authentication and model resolution explicit

**Files:**
- Modify: `/data/data/com.termux/files/home/xu-rs/src/runtime/mod.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/src/providers/store.rs` only if a parser regression is demonstrated
- Modify: `/data/data/com.termux/files/home/xu-rs/tests/runtime_bridge_integration.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/tests/provider_store.rs` only if parser coverage is missing

**Interfaces:**
- Consumes: `ProviderProfile.api_key`, `ProviderProfile.extra_headers`, `ProviderProfile.protocol` and `resolve_model`.
- Produces: no Authorization header for intentionally empty-key providers; correct header for keyed providers; stable `_provider_id` selection.

- [ ] **Step 1: Add empty-key auth regression tests**

Use the existing `MockUpstream` capture. Add one OpenAI Chat provider and one Anthropic provider with `api_key = ""`; send each through `handle_request_for_test` and assert the headers separately:

```rust
let openai = captured_openai;
assert!(!openai.headers.contains_key("authorization"));
assert!(!openai.headers.contains_key("x-api-key"));

let anthropic = captured_anthropic;
assert!(!anthropic.headers.contains_key("authorization"));
assert!(!anthropic.headers.contains_key("x-api-key"));
assert_eq!(anthropic.headers["anthropic-version"], "2023-06-01");
```

Add keyed variants and assert `authorization = "Bearer test-key"` for OpenAI and `x-api-key = "test-key"` for Anthropic.

- [ ] **Step 2: Run the auth tests before implementation**

```bash
cargo test --test runtime_bridge_integration empty_key -- --nocapture
```

Expected initial failure: the current request builder emits an auth header even when the key is empty. Preserve this failure as the regression target.

- [ ] **Step 3: Change only the request-builder auth branch**

In `send_upstream_request`, keep the existing protocol match and attach credentials conditionally:

```rust
match protocol {
    WireProtocol::AnthropicMessages if !provider.api_key.trim().is_empty() => {
        request = request.header("x-api-key", &provider.api_key);
    }
    WireProtocol::AnthropicMessages => {}
    WireProtocol::OpenAiChat | WireProtocol::OpenAiResponses
        if !provider.api_key.trim().is_empty() => {
        request = request.bearer_auth(&provider.api_key);
    }
    WireProtocol::OpenAiChat | WireProtocol::OpenAiResponses => {}
}
```

Keep `anthropic-version`, `extra_headers`, hop protection, timeout and retry behavior unchanged.

- [ ] **Step 4: Verify model selection and auth together**

```bash
cargo test --test runtime_bridge_integration -- --nocapture
cargo test --test provider_store -- --nocapture
```

Expected: multi-provider slugs resolve only with `_provider_id`; single-provider un-suffixed models continue to work; no credentials appear in assertion messages or captured artifacts.

---

### Task 4: Prove streaming terminal and tail behavior

**Files:**
- Modify: `/data/data/com.termux/files/home/xu-rs/src/runtime/bridge/stream.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/src/runtime/bridge/response.rs` only if a failing terminal test identifies a response validation bug
- Modify: `/data/data/com.termux/files/home/xu-rs/tests/runtime_bridge_integration.rs`
- Read: `/data/data/com.termux/files/home/xu-rs/tests/fixtures/runtime_bridge/zen_tool_stream_with_tail.sse`

**Interfaces:**
- Consumes: `SseDecoder`, `StreamState`, `decode_stream_frame` and `encode_stream_events`.
- Produces: one valid terminal event for each client protocol, deterministic handling of `[DONE]` and provider tail frames, and no post-terminal error sent to a client.

- [ ] **Step 1: Add text and tool stream assertions**

For each source protocol, feed chunked SSE bytes through the real decoder with a 7-byte chunk size. Assert the resulting stream contains `Started`, expected text/tool events, `Completed` or `Failed`, and exactly one terminal event.

- [ ] **Step 2: Add the Zen tail regression**

Load `zen_tool_stream_with_tail.sse`, feed it through `SseDecoder`, and assert that the tool call finishes with JSON arguments `{"q":"zen fixture"}`. The tail after `data: [DONE]` must not create a second client terminal event or overwrite the completed tool state.

- [ ] **Step 3: Add malformed terminal tests**

Assert these exact outcomes:

```rust
use runtime::bridge::{BridgeError, CompletionIr, SseDecoder, StreamEventIr, StreamState};

let mut state = StreamState::new();
assert_eq!(state.finish_eof(), Err(BridgeError::InvalidUpstream));
let completed = StreamEventIr::Completed(CompletionIr {
    status: Some("completed".to_string()),
    ..CompletionIr::default()
});
state.apply_event(&completed).expect("first terminal event");
assert_eq!(state.apply_event(&completed), Err(BridgeError::ToolState));
let mut limited_decoder = SseDecoder::new(4);
assert_eq!(limited_decoder.feed(b"12345"), Err(BridgeError::ResourceLimit));
```

- [ ] **Step 4: Run streaming verification**

```bash
cargo test runtime::bridge::stream -- --nocapture
cargo test --test runtime_bridge_integration stream -- --nocapture
```

Expected: Responses emits its completed event, Chat/Anthropic emit their protocol terminal events, and no stream path relies on a bare `[DONE]` as the only semantic completion.

---

### Task 5: Keep client adapters aligned with the Runtime contract

**Files:**
- Modify: `/data/data/com.termux/files/home/xu-rs/src/agents/codex.rs` only if model slug or endpoint assertions fail
- Modify: `/data/data/com.termux/files/home/xu-rs/src/agents/claude.rs` only if settings-path or local endpoint assertions fail
- Read: `/data/data/com.termux/files/home/xu-rs/src/agents/opencode.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/tests/agent_mapping.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/tests/cli_plan.rs` only if plan output lacks the safety assertion

**Interfaces:**
- Consumes: `serve_port`, `ProtocolAdapter`, `RoutingMode`, `ProviderProfile` and existing patch preservation.
- Produces: Codex local Responses and Claude local Messages endpoints using 9316 by default; OpenCode remains direct-file; `.claude.json` is never patched.

- [ ] **Step 1: Add adapter path safety coverage**

Create a temporary home containing both `.claude.json` state data and `.claude/settings.json` settings. Apply the Claude plan and assert every patch path ends in `.claude/settings.json`, no patch path ends in `.claude.json`, and the settings JSON keeps an unknown `theme` field.

- [ ] **Step 2: Add routing-mode assertions**

Keep these exact expectations in `tests/agent_mapping.rs`:

```rust
assert_eq!(codex_plan.routing_mode, RoutingMode::LocalOpenAiResponses);
assert_eq!(claude_plan.routing_mode, RoutingMode::LocalAnthropicMessages);
assert_eq!(opencode_plan.routing_mode, RoutingMode::DirectFile);
```

Use a Chat provider for the first two local cases and a Responses provider to assert Claude's explicit refusal.

- [ ] **Step 3: Run adapter and CLI plan tests**

```bash
cargo test --test agent_mapping -- --nocapture
cargo test --test cli_plan -- --nocapture
```

Expected: endpoint values are derived from `serve_port()` and default to `http://127.0.0.1:9316`; no test mutates the process-global `XU_SERVE_PORT`, so adapter tests remain parallel-safe.

---

### Task 6: Verify Runtime end to end with a local mock upstream

**Files:**
- Modify: `/data/data/com.termux/files/home/xu-rs/tests/runtime_bridge_integration.rs`
- Modify: `/data/data/com.termux/files/home/xu-rs/src/runtime/mod.rs` only for failures covered by Tasks 2-5
- Read: `/data/data/com.termux/files/home/xu-rs/docs/superpowers/specs/2026-08-11-three-protocol-router-design.md`

**Interfaces:**
- Consumes: all three public routes, the existing `MockUpstream`, provider model suffixes and bounded Runtime I/O.
- Produces: deterministic non-streaming, streaming, tool, timeout, body-limit and hop-limit evidence without contacting Zen, Anthropic or any other real upstream.

- [ ] **Step 1: Add one mock provider per target protocol**

Use loopback ephemeral ports and provider ids `mock-chat`, `mock-responses`, and `mock-anthropic`. Each mock records path, lower-cased headers without secret values, and JSON body; each reply includes a valid id, model and terminal status.

- [ ] **Step 2: Exercise all public paths through `handle_request_for_test`**

For each of `/v1/messages`, `/v1/chat/completions`, and `/v1/responses`, assert the mock receives the provider target path and the client receives the source protocol response shape. Run both `stream = false` and `stream = true` for text and tool fixtures.

- [ ] **Step 3: Exercise limits and loop protection**

Assert a request with `hop = 1` returns HTTP 400, a body above `2 * 1024 * 1024` returns HTTP 413, a slow mock returns HTTP 504, and an upstream non-JSON response returns HTTP 502. Use test timeouts no longer than 3 seconds.

- [ ] **Step 4: Run the complete local verification**

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

Expected: all existing tests plus the new protocol, auth, adapter and Runtime tests pass; no listener is started by the test suite outside loopback ephemeral ports.

---

### Task 7: Decide 9317 compatibility and close the recovery loop

**Files:**
- Read: `/data/data/com.termux/files/usr/bin/codex-chat2responses-proxy.mjs`
- Read: `/data/data/com.termux/files/home/.codex/xu-client-routes.json`
- Update: `/data/data/com.termux/files/home/xu-rs/.superpowers/sdd/2026-08-11-three-protocol-router-recovery/progress.md`
- Update: `/data/data/com.termux/files/home/xu-rs/.superpowers/sdd/2026-08-08-c-core-protocol-bridge/task-7-report.md`

**Interfaces:**
- Consumes: completed local test results, actual client config ownership and any confirmed external dependency on 9317.
- Produces: final route map, explicit Node disposition and a clean handoff for live smoke testing.

- [ ] **Step 1: Confirm whether any current client or service requires 9317**

Search only local config and process/service definitions:

```bash
rg -n --hidden --glob '!**/.git/**' '127\.0\.0\.1:(9317|9316)|CODEX_CHAT2RESP_PORT|codex-chat2responses-proxy' \
  /data/data/com.termux/files/home/.codex \
  /data/data/com.termux/files/home/.config/opencode \
  /data/data/com.termux/files/home/.claude \
  /data/data/com.termux/files/usr/tmp/opencode
```

If no dependency is found, leave 9317 stopped and document Node as retired for this route. If a dependency is found, write a separate forwarding task before touching 9317; do not revive the converter in place.

- [ ] **Step 2: Record live-smoke prerequisites without executing them**

Document the exact commands that would be run only after confirmation: start `spec serve`, check `/health` and `/v1/models`, then run one bounded Codex Responses request and one bounded Claude Messages request. Do not write credentials into the report.

- [ ] **Step 3: Update Task 7 evidence**

Record the Rust test result, fixture names, current listener ownership, the 9317 decision, and the remaining user-confirmation gate. Never record API key values or session cookies.

- [ ] **Step 4: Stop before persistent service or client edits**

The plan is implementation-complete only after the local suite is green and the user explicitly approves live process/configuration changes. Persistence, daemon installation and git commit remain separate actions.
