# Phase 1 Correctness Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 2026-08-14 差异审查确认的 provider 持久化、模型路由、协议桥、HTTP query、异步 fetch 和 token 统计正确性问题，并在进入阶段 2 UI 工作前建立可验证的后端基线。

**Architecture:** 按 provider、runtime/HTTP、protocol bridge、UI fetch、stats 五个边界拆成顺序任务。每个任务先写一个能复现问题的失败测试，再做最小实现；跨任务只通过明确的类型/函数契约连接。旧的 `2026-08-12-spec-v2-web-ui-and-stats.md` 仍保留为历史记录，但其中“后端零改动”约束不适用于本计划，本计划是阶段 1 的唯一执行依据。设计规范中的 `HashMap` 是抽象 map 语义；实现沿用当前领域层的 `BTreeMap`，以保持现有结构体和确定性测试风格一致。

**Tech Stack:** Rust, Cargo, `serde_json`, `reqwest::blocking`, ratatui event loop, JSONL token statistics, Termux.

## Global Constraints

- 当前工作树已有 18 个 tracked 文件修改和多个未跟踪审查产物；禁止 `git reset --hard`、`git checkout --`、批量清理或覆盖其他 agent 的修改。
- 若用户要求提交，每个任务只 `git add` 自己列出的文件；提交前运行 `git diff --check`，确认没有把密钥、私有配置、备份文件或 `.rust-review-results/` 加入提交。
- 不新增 crate 或 Cargo feature；沿用现有 `BTreeMap`、`std::sync::mpsc`、`serde_json` 和测试结构。
- 阶段 1 不修改 UI 布局、焦点可见性、Help 返回逻辑、Tab/卡片/searchbox 视觉设计；这些属于阶段 2。
- 空 API key 的有效语义是“不发送认证头”，不是用占位 key；`baseURL` 仍必须是合法 `http://` 或 `https://` URL。
- 任何不可逆协议字段必须 fail-closed；唯一批准的例外是 Chat `reasoning_content` 转 Anthropic 时降级为普通文本块。
- 未配置模型必须在读取 provider 凭据前拒绝，错误响应不得包含 provider key、Authorization 或完整配置对象。
- 每个任务必须先验证失败测试，再实现，再运行该任务的定向测试；任务完成后才能进入审阅。
- 本计划不授权提交；每个任务只运行 `git diff --check` 并保留可审阅的未提交 diff。只有用户明确要求时才按任务文件列表创建提交。
- 阶段 1 最终门禁：`cargo test --quiet`、`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo build --release`、`git diff --check` 全部通过。

## File Map and Ownership

| 文件 | 本计划中的唯一责任 |
|---|---|
| `src/domain/provider.rs` | `ModelEntry`、provider 模型身份查询、`claude_slots` 领域字段 |
| `src/providers/store.rs` | provider JSON 解析、root/legacy `claudeSlots` 兼容读取 |
| `src/cli.rs` | provider 原始 JSON 更新与 root `claudeSlots` 写回；不改变既有 patch/backup 机制 |
| `src/agents/claude.rs` | Claude 直连模型映射和槽位读取 |
| `src/agents/codex.rs` | Codex 直连模型映射 |
| `src/app/provider_ops.rs` | 有序模型行、alias 保留、多选和编辑表单 fetch 接口 |
| `src/app/provider_flow.rs` | 后台 fetch worker/receiver 生命周期 |
| `src/main.rs` | 详情页和编辑表单 action 消费 receiver；不做布局重绘 |
| `src/menu/mod.rs` | `ProviderModelRow` 的渲染和编辑调用方；不改变布局 |
| `src/runtime/http.rs` | HTTP request line 的纯 path/query 解析 |
| `src/runtime/mod.rs` | 模型授权、stats query 读取、上游请求校验和运行时错误 envelope |
| `src/runtime/bridge/ir.rs` | `proxy_error` 错误分类的公共状态/消息映射 |
| `src/runtime/bridge/chat.rs` | Chat reasoning/text 和 opaque thinking 转换 |
| `src/runtime/bridge/anthropic.rs` | Anthropic 响应/流编码契约测试配合 |
| `src/stats.rs` | per-model semantics 归一化和 denominator 计算 |
| `tests/runtime_bridge_integration.rs` | 跨协议非流式/流式回归测试 |
| `docs/superpowers/specs/2026-08-11-three-protocol-router-design.md` | 阶段 1 协议契约更新 |

---

### Task 0: Freeze the Baseline and Establish Ownership

**Files:**
- Read only: `docs/XU_RUST_DIFFERENTIAL_REVIEW_2026-08-14.md`
- Read only: `docs/superpowers/specs/2026-08-14-phase1-correctness-hardening-design.md`
- Read only: current `git diff`

**Interfaces:**
- Consumes: current dirty worktree and the committed design specification.
- Produces: a verified baseline record; no source changes and no commit.

- [ ] **Step 1: Capture the worktree boundary**

Run:

```bash
git status --short
git diff --name-only
git diff --check
```

Expected: the command lists the already-dirty files; `git diff --check` exits zero. Do not clean or reset any listed file.

- [ ] **Step 2: Run the baseline test suite**

Run:

```bash
cargo test --quiet
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Expected: baseline is recorded before new edits. If a command fails, record the exact pre-existing failure and stop before Task 1.

- [ ] **Step 3: Confirm the implementation branch is cleanly attributable**

For every later task, capture a pre-task diff for only that task's files:

```bash
git diff -- src/domain/provider.rs src/providers/store.rs
```

Do not stage any file outside the current task's file list.

---

### Task 1: Canonical Claude Slots and Direct Request Names

**Files:**
- Modify: `src/domain/provider.rs:45-79,126-163`
- Modify: `src/providers/store.rs:14-118,124-173`
- Modify: `src/agents/claude.rs:206-245,247-296`
- Modify: `src/agents/codex.rs:47-80`
- Modify: `src/app/provider_ops.rs:1236-1249`
- Modify: `src/cli.rs:2262-2270` only if root-field normalization is needed
- Test: existing module tests in `src/providers/store.rs`, `src/agents/claude.rs`, `src/agents/codex.rs`

**Interfaces:**
- Consumes: provider JSON with root `claudeSlots`, legacy metadata slots, and model objects with optional `clientName`, `name`, and `requestName`.
- Produces:
  ```rust
  pub claude_slots: BTreeMap<String, String>
  pub fn claude_slot(&self, slot: &str) -> Option<&str>
  pub fn configured_request_name_for(&self, model: &str) -> Option<&str>
  ```
  Parse legacy `model_metadata["claudeSlots"]` first, then overlay every root `claudeSlots` value. A root value, including an empty string, wins for its slot; a missing root slot retains the legacy value. For ordered model arrays, `clientName` is the optional stable local key; old objects without it retain the current `client_name == name` behavior. The raw JSON writer remains `cli.rs::provider_update_command`; `ProviderProfile` is a parsed domain view, not a new serializer.

- [ ] **Step 1: Write the failing provider round-trip and precedence tests**

Add tests to `src/providers/store.rs` using a CLI-shaped store:

```rust
#[test]
fn root_claude_slots_are_loaded_and_win_over_legacy_metadata() {
    let text = r#"{
      "provider": {"zen": {
        "options": {"baseURL":"https://zen.example/v1"},
        "models": {"m": {"claudeSlots":{"opus":"legacy"}}},
        "claudeSlots": {"opus":"root"}
      }}
    }"#;
    let profiles = profiles_from_xu_chat_json(text).unwrap();
    assert_eq!(profiles[0].claude_slots["opus"], "root");
}
```

Add this helper inside `src/agents/claude.rs::tests_slots`, copying the existing fixture fields and adding the new `claude_slots: Default::default()` field:

```rust
fn direct_provider() -> crate::domain::ProviderProfile {
    crate::domain::ProviderProfile {
        id: "zen".into(), name: "zen".into(), notes: None, website: None,
        vendor: crate::domain::ProviderVendor::CustomOpenAiCompatible,
        protocol: crate::domain::ProtocolKind::AnthropicMessages,
        base_url: "https://zen/v1".into(), api_key: String::new(),
        models: vec!["client-key".into()],
        model_metadata: Default::default(),
        model_entries: [(
            "client-key".into(),
            crate::domain::ModelEntry {
                client_name: "client-key".into(), display_name: "Client key".into(),
                request_name: "upstream-name".into(),
            },
        )].into_iter().collect(),
        claude_slots: Default::default(), default_model: "client-key".into(),
        extra_headers: Default::default(), request_url_mode: None, header_mode: None,
        timeout_ms: 90000, max_retries: 1, context_window: 128000,
        max_output_tokens: 32768, reasoning_effort: None,
        cache_mode: crate::domain::CacheMode::Auto,
    }
}
```

Then call the private mapper directly:

```rust
#[test]
fn direct_claude_mapping_uses_request_name() {
    let provider = direct_provider();
    let mapping = super::claude_mapping(&provider, 1).unwrap();
    assert_eq!(mapping.routing_mode, super::RoutingMode::DirectFile);
    assert_eq!(mapping.model, "upstream-name");
}
```

Add the Codex test in `src/agents/codex.rs::tests`. Reuse its existing `provider("zen")` fixture, change `protocol` to `OpenAiResponses`, set its only model to `client-key`, set `default_model` to `client-key`, and insert a `crate::domain::ModelEntry` with `request_name: "upstream-name"`. Add `claude_slots: Default::default()` to that fixture once Task 1 adds the field. Construct a temporary home containing an empty `.codex/config.toml`, and call the existing adapter:

```rust
#[test]
fn direct_codex_mapping_uses_request_name() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".codex")).unwrap();
    std::fs::write(root.path().join(".codex/config.toml"), "").unwrap();
    let mut provider = provider("zen");
    provider.protocol = ProtocolKind::OpenAiResponses;
    provider.models = vec!["client-key".into()];
    provider.default_model = "client-key".into();
    provider.model_entries.insert("client-key".into(), crate::domain::ModelEntry {
        client_name: "client-key".into(), display_name: "Client key".into(),
        request_name: "upstream-name".into(),
    });
    let plan = CodexAdapter.build_plan(root.path(), &[&provider]).unwrap();
    assert!(plan.patches[0].after.contains("model = \"upstream-name\""));
}
```

Add a CLI writer regression test that calls the existing `provider_update_command` with `--claude-slot opus=root`, reloads the resulting store with `profiles_from_xu_chat_json()`, and asserts `profiles[0].claude_slot("opus") == Some("root")`.

- [ ] **Step 2: Run the targeted tests and verify they fail**

Run:

```bash
cargo test --quiet root_claude_slots_are_loaded_and_win_over_legacy_metadata
cargo test --quiet direct_claude_mapping_uses_request_name
cargo test --quiet direct_codex_mapping_uses_request_name
```

Expected: failures show missing `claude_slots` or the current direct mappings returning the client alias.

- [ ] **Step 3: Add the canonical domain field and parser precedence**

Add a deterministic map to `ProviderProfile`:

```rust
pub claude_slots: BTreeMap<String, String>,
```

In `store.rs`:

1. Read `model_metadata["claudeSlots"]` as an object into a `BTreeMap<String, String>`.
2. Read `obj.get("claudeSlots")` as an object and insert every string value into the same map after legacy values.
3. If both exist, the root value overwrites the legacy value for that slot; root omissions do not erase unrelated legacy slots.
4. In `model_entries_from_array()`, use `clientName` when it is a non-empty string; otherwise retain the current `name` then `requestName` fallback. Preserve the complete object in `model_metadata`.
5. Populate the field in both current provider-object and legacy-array parser paths.
6. Update every `ProviderProfile` struct literal in tests and source to initialize an empty map.

Do not add a second serializer. `cli.rs` already mutates the raw root JSON and `write_provider_store()` validates the parsed result before applying the atomic patch.

- [ ] **Step 4: Move consumers and direct adapters to the canonical lookup**

Add the domain accessor and change `provider_ops::claude_slot_value()` and Claude settings generation to use it:

```rust
pub fn claude_slot(&self, slot: &str) -> Option<&str> {
    self.claude_slots
        .get(slot)
        .map(String::as_str)
        .filter(|model| !model.trim().is_empty())
}
```

In Claude and Codex direct paths, resolve before writing config:

```rust
let client_model = provider.first_model()?;
let request_model = provider.request_name_for(client_model).to_string();
```

Use `request_model` only for the direct upstream config. Keep local runtime slugs based on the client model so provider routing remains addressable. Define `responses_provider_with_alias()` in the Codex test module as a small fixture builder derived from its existing `provider()` helper; it must set `protocol`, `models`, `default_model`, and the `model_entries` key consistently.

- [ ] **Step 5: Run targeted tests and verify compatibility**

Run:

```bash
cargo test --quiet root_claude_slots_are_loaded_and_win_over_legacy_metadata
cargo test --quiet claude_mapping
cargo test --quiet codex
cargo test --quiet providers::store
```

Expected: all targeted tests pass and legacy metadata-only slots still work.

- [ ] **Step 6: Review the provider contract diff**

```bash
git diff --check
git diff -- src/domain/provider.rs src/providers/store.rs src/agents/claude.rs src/agents/codex.rs src/app/provider_ops.rs src/cli.rs
```

---

### Task 2: Stable Provider Detail Rows and Alias-Preserving Multi-Select

**Files:**
- Modify: `src/app/provider_ops.rs:12-18,1163-1221,1316-1410`
- Modify: `src/main.rs` callers of `provider_model_rows`, detail selection initialization, and fetch actions only
- Modify: `src/menu/mod.rs` tuple destructuring and `ProviderModelRow` test fixtures only
- Test: `src/app/provider_ops.rs` existing provider detail tests

**Interfaces:**
- Consumes: `ProviderProfile.models` as the normalized storage order and `ProviderProfile.model_entries` for metadata.
- Produces:
  ```rust
  pub struct ProviderModelRow {
      pub client_name: String,
      pub display_name: String,
      pub request_name: String,
  }
  pub fn provider_model_rows(provider: &ProviderProfile) -> Vec<ProviderModelRow>
  ```
  The stable lookup key for fetched multi-select values is `request_name`; no second UI selection type is introduced.

- [ ] **Step 1: Write the failing order and alias tests**

```rust
#[test]
fn provider_model_rows_preserve_normalized_model_order() {
    let mut provider = detail_profile();
    provider.models = vec!["Zebra".into(), "Alpha".into(), "Mid".into()];
    provider.model_entries.clear();
    provider.model_entries.insert("Zebra".into(), spec::domain::ModelEntry {
        client_name: "Zebra".into(), display_name: "Zebra".into(), request_name: "zebra-upstream".into(),
    });
    provider.model_entries.insert("Alpha".into(), spec::domain::ModelEntry {
        client_name: "Alpha".into(), display_name: "Alpha".into(), request_name: "alpha-upstream".into(),
    });
    provider.model_entries.insert("Mid".into(), spec::domain::ModelEntry {
        client_name: "Mid".into(), display_name: "Mid".into(), request_name: "Mid".into(),
    });
    let rows = provider_model_rows(&provider);
    assert_eq!(rows[0].client_name, "Zebra");
}

#[test]
fn detail_multi_models_preserves_existing_request_alias() {
    let mut provider = detail_profile();
    provider.models = vec!["client-key".into()];
    provider.model_entries.insert("client-key".into(), crate::domain::ModelEntry {
        client_name: "client-key".into(), display_name: "Client key".into(), request_name: "upstream-name".into(),
    });
    let args = detail_multi_models_args(&provider, &["upstream-name".into()], &BTreeSet::from([0])).unwrap();
    assert_eq!(args.last().unwrap(), r#"[{"clientName":"client-key","name":"Client key","requestName":"upstream-name"}]"#);
}
```

- [ ] **Step 2: Run the targeted tests and verify they fail**

```bash
cargo test --quiet provider_model_rows_preserve_normalized_model_order
cargo test --quiet detail_multi_models_preserves_existing_request_alias
```

Expected: row 0 is currently alphabetically sorted and the alias lookup currently misses request-name selections.

- [ ] **Step 3: Make the row model carry its stable client key**

Change `ProviderModelRow` and `provider_model_rows()` to iterate `provider.models` and look up each client name:

```rust
provider.models.iter().map(|client_name| {
    match provider.model_entries.get(client_name) {
        Some(entry) => ProviderModelRow {
            client_name: client_name.clone(),
            display_name: entry.display_name.clone(),
            request_name: entry.request_name.clone(),
        },
        None => ProviderModelRow {
            client_name: client_name.clone(),
            display_name: client_name.clone(),
            request_name: client_name.clone(),
        },
    }
}).collect()
```

Update every tuple destructure and `ProviderModelRow` fixture in `src/main.rs` and `src/menu/mod.rs`. Use `row.client_name` for model-row mutation; use `display_name` and `request_name` only for rendering or upstream-list matching.

- [ ] **Step 4: Resolve selected names by request name before serialization**

In `detail_multi_models_args()`, replace direct `model_entries.get(&name)` lookup with:

```rust
let entry = provider.model_entries.get(name).or_else(|| {
    provider.models.iter().find_map(|client_name| {
        provider.model_entries.get(client_name).filter(|entry| entry.request_name == *name)
    })
});
```

If an entry is found, serialize its existing `{clientName, name, requestName}` relationship. Omit `clientName` only when it equals the display name to preserve the old JSON shape; include it whenever the local client key differs. If no entry is found, serialize the selected name as a plain string. Keep the selected list order and reject an empty selection as today.

- [ ] **Step 5: Run detail tests and compile all callers**

```bash
cargo test --quiet provider_model_rows
cargo test --quiet detail_multi_models
cargo test --quiet provider_ops
```

Expected: the order and alias tests pass; no caller indexes a `BTreeMap` row against `provider.models`.

- [ ] **Step 6: Review the detail model contract diff**

```bash
git diff --check
git diff -- src/app/provider_ops.rs src/main.rs src/menu/mod.rs
```

---

### Task 3: Strict Configured-Model Resolution Before Credential Lookup

**Files:**
- Modify: `src/domain/provider.rs:126-163`
- Modify: `src/runtime/mod.rs:760-830,1574-1594` and the existing error response mapping
- Test: runtime unit tests in `src/runtime/mod.rs`, `tests/runtime_bridge_integration.rs` if route-level coverage is required

**Interfaces:**
- Consumes: provider `models`, `model_entries`, and request model slug. `default_model` is not part of resolution; `ProviderProfile::first_model()` remains the only default-model fallback used by agent configuration.
- Produces:
  ```rust
  pub fn configured_request_name_for(&self, model: &str) -> Option<&str>
  #[derive(Debug, PartialEq, Eq)]
  enum ModelResolutionError {
      NotConfigured(String),
      Ambiguous(String),
      InvalidSuffix(String),
  }
  fn resolve_model(providers: &[ProviderProfile], requested_model: &str)
      -> Result<(&ProviderProfile, String), ModelResolutionError>
  ```
  Resolution is case-sensitive and deterministic after the existing case-insensitive `[1m]` normalization. Exact client name wins; an exact request name is accepted only when no client-name match exists; a provider suffix is stripped before lookup; an empty model after suffix stripping becomes `InvalidSuffix`. `NotConfigured` becomes the existing runtime error envelope with `code: "invalid_request"`, `type: "invalid_request_error"`, and a sanitized `model not configured: <slug>` message. `Ambiguous` becomes the same envelope with `model provider is ambiguous: <slug>`.

- [ ] **Step 1: Write the failing resolver tests**

```rust
#[test]
fn resolve_model_rejects_unknown_model_before_credentials() {
    let mut provider = provider("zen", ProtocolKind::OpenAiChat);
    provider.api_key = "provider-secret".into();
    provider.models = vec!["configured".into()];
    let error = resolve_model(&[provider], "not-configured_fixture").unwrap_err();
    assert_eq!(error, ModelResolutionError::NotConfigured("not-configured_fixture".into()));
}

#[test]
fn resolve_model_accepts_client_and_request_names() {
    let mut provider = provider("zen", ProtocolKind::OpenAiChat);
    provider.models = vec!["client-key".into()];
    provider.model_entries.insert("client-key".into(), spec::domain::ModelEntry {
        client_name: "client-key".into(), display_name: "Client key".into(), request_name: "upstream-name".into(),
    });
    let providers = vec![provider];
    assert_eq!(resolve_model(&providers, "client-key_zen").unwrap().1, "upstream-name");
    assert_eq!(resolve_model(&providers, "upstream-name_zen").unwrap().1, "upstream-name");
}

#[test]
fn resolve_model_requires_provider_suffix_when_multiple_providers_exist() {
    let providers = vec![
        provider("zen", ProtocolKind::OpenAiChat),
        provider("other", ProtocolKind::OpenAiChat),
    ];
    assert!(matches!(
        resolve_model(&providers, "deepseek-chat"),
        Err(ModelResolutionError::Ambiguous(slug)) if slug == "deepseek-chat"
    ));
}
```

Add the route-level test in `tests/runtime_bridge_integration.rs` using the existing `MockUpstream::start`, `runtime::http_request_for_test`, `runtime::handle_request_for_test`, and `MockUpstream::finish_result`: configure a provider whose API key is `provider-secret`, request `not-configured_fixture_zen`, assert HTTP 400 with `"code":"invalid_request"`, `"type":"invalid_request_error"`, and `model not configured`, assert the body does not contain `provider-secret`, then assert `finish_result()` reports no upstream request.

- [ ] **Step 2: Run the tests and verify the current unknown-model leak**

```bash
cargo test --quiet resolve_model_rejects_unknown_model_before_credentials
cargo test --quiet resolve_model_accepts_client_and_request_names
```

Expected: the unknown model currently becomes an upstream model name instead of a local 400.

- [ ] **Step 3: Implement explicit membership and typed resolution errors**

Implement membership without changing the permissive legacy `request_name_for()` API:

```rust
pub fn configured_request_name_for(&self, model: &str) -> Option<&str> {
    if self.models.iter().any(|name| name == model) {
        return self.model_entries.get(model)
            .map(|entry| entry.request_name.as_str())
            .or(Some(model));
    }
    self.model_entries
        .values()
        .find(|entry| entry.request_name == model)
        .map(|entry| entry.request_name.as_str())
}
```

Add `ModelResolutionError` beside `resolve_model()` and map it in `process_protocol_request()` instead of the current `.map_err(|_| BridgeError::InvalidRequest)`. Extend `UpstreamFailure` with `error_type: Option<&'static str>`, initialize it to `None` in `From<BridgeError>` and every existing struct literal, and make `write_upstream_error()` insert `error.type` only when it is `Some`. The local resolution mapping sets `kind: BridgeError::InvalidRequest`, a sanitized relay message, `status: Some(400)`, and `error_type: Some("invalid_request_error")`; ordinary upstream errors retain their current envelope.

In `resolve_model()`:

1. Strip `[1m]` first.
2. Try an exact provider suffix `_provider_id`; reject an empty remaining model.
3. Require `configured_request_name_for()` to return `Some`.
4. If there is one provider, allow an unqualified configured client/request name only.
5. If there are multiple providers and no suffix, return `ModelResolutionError::Ambiguous`.
6. If a suffix matches a provider but the remaining client/request name is unknown, return `ModelResolutionError::NotConfigured`.
7. If the suffix is present but has no model portion, return `ModelResolutionError::InvalidSuffix` and the same sanitized 400 envelope.

Map an unknown configured-model error to HTTP 400 with:

```json
{"error":{"message":"model not configured: <slug>","type":"invalid_request_error"}}
```

Perform this before `is_runtime_upstream()`, `build_upstream_request()`, or any credential-bearing provider request.

- [ ] **Step 4: Run resolver and runtime integration tests**

```bash
cargo test --quiet resolve_model
cargo test --quiet --test runtime_bridge_integration
```

Expected: unknown models never reach an upstream request; client and request aliases resolve to the same provider/request model.

- [ ] **Step 5: Review the routing boundary diff**

```bash
git diff --check
git diff -- src/domain/provider.rs src/runtime/mod.rs tests/runtime_bridge_integration.rs
```

---

### Task 4: Preserve HTTP Path and Query as Separate Fields

**Files:**
- Modify: `src/runtime/http.rs:8-22,29-93`
- Modify: `src/runtime/mod.rs` request dispatch and stats route
- Test: `src/runtime/http.rs` unit tests and runtime stats tests

**Interfaces:**
- Consumes: HTTP request lines such as `GET /api/stats/tokens?period=24h HTTP/1.1`.
- Produces:
  ```rust
  pub(super) struct HttpRequest {
      pub(super) method: String,
      pub(super) path: String,                 // /api/stats/tokens
      pub(super) query: BTreeMap<String, String>,
      pub(super) body: Vec<u8>,
      pub(super) hop: u8,
  }
  ```
  `path` remains suitable for exact route matching; `query` contains decoded first values for unique keys. `http_request_for_test()` continues to accept a raw request target and constructs these two fields through the same parser helper.

- [ ] **Step 1: Write failing parser and period-filter tests**

```rust
#[test]
fn request_line_preserves_query_separately() {
    let request = read_request_from_bytes(
        b"GET /api/stats/tokens?period=24h HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec(),
    ).unwrap();
    assert_eq!(request.path, "/api/stats/tokens");
    assert_eq!(request.query["period"], "24h");
}

#[test]
fn stats_period_query_limits_response() {
    let request = http_request_for_test("GET", "/api/stats/tokens?period=24h", Vec::new(), 0);
    let (_, _, body) = handle_request_for_test(request, home.path(), &providers).unwrap();
    assert!(body.contains("24h"));
    assert!(!body.contains("7d"));
}
```

- [ ] **Step 2: Run the tests and verify query loss**

```bash
cargo test --quiet request_line_preserves_query_separately
cargo test --quiet stats_period_query_limits_response
```

Expected: the parser currently returns an empty query and the stats endpoint cannot distinguish periods.

- [ ] **Step 3: Add the query map without adding dependencies**

Parse the request target through one shared helper used by both socket parsing and `http_request_for_test()`:

```rust
pub(super) fn split_request_target(raw_target: &str) -> Result<(String, BTreeMap<String, String>), String> {
    let (path, query_text) = raw_target.split_once('?').unwrap_or((raw_target, ""));
    Ok((path.to_string(), parse_query(query_text)?))
}
```

`parse_query()` must:

- split `&` pairs and accept `key` as an empty-value parameter;
- percent-decode `%XX` and translate `+` to space using a local helper;
- return the existing sanitized local `invalid_request` 400 envelope for malformed percent escapes; `handle_connection()` currently maps parser errors to `BridgeError::InvalidRequest`, so do not invent a second parser-error response type;
- keep the first value for duplicate keys;
- return an empty map for no query.

Add `query: BTreeMap<String, String>` to both current `HttpRequest` declarations, update every struct literal, and make `http_request_for_test()` call `http::split_request_target()` for valid targets. Change `path_matches_count_tokens()`, `path_matches_stats_tokens()`, and `path_models_retrieve()` to receive an already-normalized `&request.path`; change `stats_period_filter()` and `handle_stats_tokens()` to receive `&request.query`. Add a raw socket regression using the existing `raw_runtime_request()` helper so the test covers actual request-line parsing, not only the test constructor.

- [ ] **Step 4: Run parser, stats, and runtime tests**

```bash
cargo test --quiet request_line
cargo test --quiet stats_period
cargo test --quiet --test runtime_bridge_integration
```

Expected: exact path routing remains unchanged and `period=24h` selects only the 24h aggregate.

- [ ] **Step 5: Review the HTTP contract diff**

```bash
git diff --check
git diff -- src/runtime/http.rs src/runtime/mod.rs
```

---

### Task 5: Make Cross-Protocol Reasoning Behavior Explicit

**Files:**
- Modify: `src/runtime/bridge/chat.rs:465-484,556-588,757-780,1328-1388`
- Modify: `src/runtime/bridge/anthropic.rs` only for focused regression assertions if required
- Modify: `tests/runtime_bridge_integration.rs`
- Modify: `docs/superpowers/specs/2026-08-11-three-protocol-router-design.md`
- Test: `src/runtime/bridge/chat.rs` unit tests and integration tests

**Interfaces:**
- Consumes: Chat response `reasoning_content`, Chat stream reasoning deltas, IR `ContentIr::Thinking` and `ContentIr::RedactedThinking`.
- Produces:
  - Chat reasoning → Anthropic: `ContentIr::Text` / `StreamEventIr::TextDelta`, never unsigned thinking.
  - Signed or redacted thinking → Chat: `BridgeError::Unsupported` in request and response encoders.

- [ ] **Step 1: Turn the existing codec tests into failing contract tests**

```rust
// Keep the existing JSON fixture in `decoder_accepts_provider_reasoning_and_cost_metadata`.
// Replace its assertion with this exact expected IR:
assert_eq!(
    response.content,
    vec![
        ContentIr::Text("provider reasoning fixture".to_string()),
        ContentIr::Text("visible fixture".to_string()),
    ]
);

// Keep the existing fixture in `encoder_strips_opaque_reasoning_in_chat_messages`.
// Replace the successful encoding assertion with:
assert!(matches!(
    super::encode_request(&ir, "chat-upstream"),
    Err(BridgeError::Unsupported { field }) if field == "redacted_thinking"
));
```

Modify the existing `stream_decoder_accepts_provider_reasoning_content` fixture into two frames. The first frame has only `delta.reasoning_content = "provider reasoning fixture"`; the second has only `delta.content = "visible fixture"`. Decode both with the same `StreamState`, concatenate the returned events, and assert that both events are `StreamEventIr::TextDelta` in that order. Do not put both fields in one frame because the source JSON object does not define an ordering contract between them.

Add signed-thinking and redacted-thinking request variants to the existing encoder test. Each must return `Err(BridgeError::Unsupported { field })`, with fields `thinking.signature` and `redacted_thinking` respectively. Add the user-content variant by changing the message role from `assistant` to `user` and using the request encoder path that handles non-assistant content.

- [ ] **Step 2: Run the tests and verify the current invalid/lossy behavior**

```bash
cargo test --quiet decoder_accepts_provider_reasoning_and_cost_metadata
cargo test --quiet encoder_strips_opaque_reasoning_in_chat_messages
cargo test --quiet stream_decoder_accepts_provider_reasoning_content
```

Expected: the first test observes `ContentIr::Thinking { signature: None }`; the second currently succeeds after silent stripping; and the stream test does not yet prove that the reasoning event is a `TextDelta`.

- [ ] **Step 3: Convert Chat reasoning to ordinary text events**

In the non-stream decoder, replace the current insertion with:

```rust
if let Some(text) = message.get("reasoning_content").and_then(Value::as_str) {
    if !text.is_empty() {
        content.insert(0, ContentIr::Text(text.to_string()));
    }
}
```

In the stream decoder, change `StreamEventIr::ReasoningDelta` to `StreamEventIr::TextDelta` for Chat `reasoning_content`. Keep provider ordering: reasoning deltas arrive before or alongside normal content and are forwarded in the same order observed.

- [ ] **Step 4: Restore fail-closed opaque reasoning behavior**

Replace both Chat encoder skip arms with an error:

```rust
ContentIr::Thinking { signature: Some(_), .. } => {
    return Err(BridgeError::Unsupported { field: "thinking.signature".to_string() });
}
ContentIr::RedactedThinking { .. } => {
    return Err(BridgeError::Unsupported { field: "redacted_thinking".to_string() });
}
```

Apply the same behavior to request and response paths; do not silently remove content.

- [ ] **Step 5: Update the protocol spec and run bridge tests**

Document both rules in `2026-08-11-three-protocol-router-design.md`. Add focused integration cases to `tests/runtime_bridge_integration.rs` using existing `MockUpstream::json`, `request_for_source(AnthropicMessages)`, and `parse_sse_frames`: for `anthropic_to_chat`, add Chat `choices[0].message.reasoning_content` and assert the downstream Anthropic response contains two ordered text blocks and no thinking block; for signed request and redacted response fixtures, assert the runtime returns its documented error envelope and `MockUpstream::finish_result()` shows no successful relay. Then run:

```bash
cargo test --quiet chat_reasoning
cargo test --quiet reasoning_streaming
cargo test --quiet --test runtime_bridge_integration
```

Expected: reasoning remains visible as valid text for Anthropic clients; opaque reasoning routed to Chat returns `Unsupported`.

- [ ] **Step 6: Review the bridge contract diff**

```bash
git diff --check
git diff -- src/runtime/bridge/chat.rs src/runtime/bridge/anthropic.rs tests/runtime_bridge_integration.rs docs/superpowers/specs/2026-08-11-three-protocol-router-design.md
```

---

### Task 6: Remove Synchronous Model Fetches from the Event Loop

**Files:**
- Modify: `src/app/provider_flow.rs:18-42`
- Modify: `src/app/provider_ops.rs:1163-1221`
- Modify: `src/main.rs` model-fetch action handlers and edit-form event handling only
- Test: provider flow/action tests; no layout changes

**Interfaces:**
- Consumes: the existing detail-cache worker `start_models_fetch_cached(state, force) -> Option<Receiver<Result<Vec<String>, String>>>` and `parse_fetch_models_output(&str) -> Result<Vec<ProviderModelRow>, String>`.
- Produces:
  ```rust
  pub struct FormModelsFetchJob {
      pub generation: u64,
      pub provider_id: String,
      pub receiver: Receiver<Result<Vec<ProviderModelRow>, String>>,
  }
  pub fn start_form_models_fetch(provider_id: String, generation: u64)
      -> Option<FormModelsFetchJob>;
  pub fn apply_models_fetch_result(
      form: &mut ProviderAddForm,
      result: Result<Vec<ProviderModelRow>, String>,
  );
  ```
  The detail page retains its existing `Vec<String>` cache receiver. The edit form uses the separate job because its editable `form.id` can differ from `ProviderState.selected`; the job carries both generation and provider id so stale results can be discarded. The event loop starts workers and polls receivers; only worker closures call `fetch_models_cached()` or `spec::cli::run_command()`.

- [ ] **Step 1: Write failing pure form-result tests**

```rust
#[test]
fn apply_models_fetch_result_updates_rows_and_feedback() {
    let mut form = form_with_rows(vec![("old", "old")]);
    form.model_cell = Some((0, 1));
    apply_models_fetch_result(
        &mut form,
        Ok(vec![ProviderModelRow {
            client_name: "new".into(),
            display_name: "new".into(),
            request_name: "new-upstream".into(),
        }]),
    );
    assert_eq!(form.model_rows[0].request_name, "new-upstream");
    assert_eq!(form.model_cell, None);
    assert!(form.model_fetch_msg.as_deref().unwrap().contains("拉取成功"));
}

#[test]
fn apply_models_fetch_error_preserves_rows_and_cell() {
    let mut form = form_with_rows(vec![("old", "old")]);
    form.model_cell = Some((0, 1));
    apply_models_fetch_result(&mut form, Err("network down".into()));
    assert_eq!(form.model_rows[0].request_name, "old");
    assert_eq!(form.model_cell, Some((0, 1)));
    assert_eq!(form.model_fetch_msg.as_deref(), Some("拉取失败：network down"));
}
```

- [ ] **Step 2: Run targeted tests and verify the synchronous path**

```bash
cargo test --quiet apply_models_fetch_result_updates_rows_and_feedback
cargo test --quiet apply_models_fetch_error_preserves_rows_and_cell
```

Expected: current `fetch_models_into_form()` reaches `spec::cli::run_command()` directly from the caller.

- [ ] **Step 3: Move form fetching behind a form-specific receiver**

Replace `fetch_models_into_form()` with this pure result-application helper:

```rust
pub fn apply_models_fetch_result(
    form: &mut ProviderAddForm,
    result: Result<Vec<ProviderModelRow>, String>,
) {
    match result {
        Ok(rows) => {
            let count = rows.len();
            form.model_rows = rows;
            form.model_cell = None;
            form.model_fetch_msg = Some(format!("拉取成功：{count} 个模型（预览，按 确认 保存）"));
        }
        Err(error) => form.model_fetch_msg = Some(format!("拉取失败：{error}")),
    }
}
```

Add `start_form_models_fetch()`: reject an empty trimmed id by returning `None`; otherwise clone the trimmed id and generation into a `std::thread::spawn` closure, call `spec::cli::run_command(&config::home(), &["provider", "fetch-models", &id])`, convert command availability/output failures to the same `Err(String)` values used today, parse successful output with `parse_fetch_models_output()`, and send the result over a new `std::sync::mpsc::channel`.

In `main.rs`, store `Option<FormModelsFetchJob>` plus a monotonically increasing form-fetch generation next to the existing edit-form state. The Fetch Models action increments generation, starts `start_form_models_fetch(form.id.clone(), generation)`, and returns immediately. Poll once per event-loop tick. Apply a received result only when the mode is still the same form, `job.generation` equals the active generation, and `job.provider_id == form.id.trim()`; otherwise discard it.

- [ ] **Step 4: Route the `m` shortcut through the same worker path**

Remove the direct `detail_fetch_models_result()` call from the `m` key handler. The handler starts the worker and returns immediately; receiver completion updates the detail state. Preserve existing messages and do not change layout rendering.

- [ ] **Step 5: Run action tests and full compile**

```bash
cargo test --quiet fetch_actions
cargo test --quiet provider_flow
cargo test --quiet provider_ops
```

Expected: form result handling is deterministic, the `m` shortcut starts the existing detail receiver, and no event-loop action invokes network I/O synchronously.

- [ ] **Step 6: Review the non-blocking fetch diff**

```bash
git diff --check
git diff -- src/app/provider_flow.rs src/app/provider_ops.rs src/main.rs
```

---

### Task 7: Lock Down Empty-Key Requests and Per-Model Cache Semantics

**Files:**
- Modify: `src/runtime/mod.rs:1066-1100` only for base URL validation if needed
- Modify: `src/stats.rs:221-297`
- Test: runtime request-builder tests and `src/stats.rs` tests

**Interfaces:**
- Consumes: provider base URL, optional API key, and `UsageRecord.semantics`.
- Produces:
  - Empty key: no `Authorization`/`x-api-key`; valid base URL still produces a request.
  - Invalid/empty base URL: local 400 before sending upstream.
  - Per-model cache rate denominator: `fresh_input + cache_creation + cached`, with `fresh_input` normalized per record semantics.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn empty_api_key_omits_authorization() {
    let mut provider = provider("fixture", ProtocolKind::OpenAiChat);
    provider.base_url = "https://example.test/v1".into();
    provider.api_key.clear();
    provider.extra_headers.clear();
    let request = build_upstream_request(&provider, bridge::WireProtocol::OpenAiChat, &json!({}))
        .unwrap()
        .build()
        .unwrap();
    assert!(request.headers().get("authorization").is_none());
    assert!(request.headers().get("x-api-key").is_none());
}

#[test]
fn aggregate_by_model_normalizes_mixed_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let mut total = record(now_minus(1), 100, 20, 20, SOURCE_CONVERTED);
    total.cache_creation = 10;
    total.semantics = SEMANTICS_TOTAL;
    let mut fresh = record(now_minus(1), 70, 30, 30, SOURCE_CONVERTED);
    fresh.cache_creation = 10;
    fresh.semantics = SEMANTICS_FRESH;
    let mut legacy = record(now_minus(1), 50, 10, 10, SOURCE_CONVERTED);
    legacy.semantics = SEMANTICS_LEGACY;
    for value in [&total, &fresh, &legacy] {
        record_usage(dir.path(), value).unwrap();
    }
    let models = aggregate_by_model(dir.path(), PERIOD_24H);
    let stat = &models[0].1;
    assert_eq!(stat.input, 220);
    assert_eq!(stat.fresh_input, 180);
    assert_eq!(stat.cached, 60);
    assert_eq!(stat.cache_creation, 20);
    assert!((stat.cache_hit_rate - 60.0 / 260.0).abs() < 1e-9);
}
```

- [ ] **Step 2: Run the tests and verify current behavior**

```bash
cargo test --quiet empty_api_key_omits_authorization
cargo test --quiet aggregate_by_model_normalizes_mixed_semantics
```

Expected: the header test remains green only after the implementation preserves the existing empty-key branch; the stats test fails because `ModelStat` currently has no `fresh_input` accumulator and derives its denominator from aggregated raw input.

- [ ] **Step 3: Add explicit base URL validation at the local boundary**

Before constructing the reqwest client, add this helper and call it from `build_upstream_request()`:

```rust
fn validate_upstream_base_url(base_url: &str) -> Result<(), bridge::BridgeError> {
    let url = reqwest::Url::parse(base_url.trim())
        .map_err(|_| bridge::BridgeError::InvalidRequest)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(bridge::BridgeError::InvalidRequest);
    }
    Ok(())
}
```

Return the existing local bridge/runtime error that maps to HTTP 400; do not include the API key or full URL in the error body. Keep the existing conditional auth headers unchanged. The empty-key test must use an empty `extra_headers` map; user-supplied extra auth headers remain an explicit override and are not silently filtered by this task.

- [ ] **Step 4: Normalize per-model statistics using the record semantics**

Add `fresh_input: u64` to `ModelStat`. While aggregating each record, call the existing `fresh_input_of(&record)` and add:

```rust
    stat.input = stat.input.saturating_add(record.input);
    stat.fresh_input = stat.fresh_input.saturating_add(fresh_input_of(&record));
    stat.cached = stat.cached.saturating_add(record.cached);
    stat.cache_creation = stat.cache_creation.saturating_add(record.cache_creation);
```

Compute:

```rust
    let cacheable = stat.fresh_input
        .saturating_add(stat.cache_creation)
        .saturating_add(stat.cached);
stat.cache_hit_rate = if cacheable == 0 {
    0.0
} else {
    stat.cached as f64 / cacheable as f64
};
```

Do not use `input - cached - cache_creation` after records with mixed `FRESH`, `TOTAL`, and `LEGACY` semantics have been aggregated.

- [ ] **Step 5: Run stats and runtime tests**

```bash
cargo test --quiet stats
cargo test --quiet empty_api_key
cargo test --quiet --test runtime_bridge_integration
```

Expected: all semantics use the same documented denominator and empty keys never create authentication headers.

- [ ] **Step 6: Review the request/statistics contract diff**

```bash
git diff --check
git diff -- src/runtime/mod.rs src/stats.rs
```

---

### Task 8: Integrate, Audit, and Close the Phase-1 Gate

**Files:**
- Modify only if verification exposes a regression: files touched by Tasks 1-7
- Read: all current phase-1 task diffs, `docs/XU_RUST_DIFFERENTIAL_REVIEW_2026-08-14.md`, and the phase-1 design spec

**Interfaces:**
- Consumes: all current task diffs and the original dirty-worktree baseline.
- Produces: a fully verified phase-1 branch and an independent review request; no phase-2 UI work.

- [ ] **Step 1: Run the complete verification suite**

```bash
cargo test --quiet
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --release
git diff --check
```

Expected: every command exits zero. If a failure is caused by an earlier dirty file outside this plan, document it separately instead of changing unrelated code.

- [ ] **Step 2: Run targeted behavior checks**

```bash
cargo test --quiet root_claude_slots
cargo test --quiet direct_.*request_name
cargo test --quiet resolve_model
cargo test --quiet request_line
cargo test --quiet chat_reasoning
cargo test --quiet signed_or_redacted
cargo test --quiet fetch_actions
cargo test --quiet stats
```

Expected: every named regression remains green after the full build.

- [ ] **Step 3: Inspect the cumulative diff for boundary regressions**

Review the current uncommitted task diff and the per-task baseline snapshots captured in Task 0:

```bash
git diff -- src/domain/provider.rs src/providers/store.rs src/runtime src/stats.rs src/app/provider_ops.rs src/app/provider_flow.rs src/main.rs src/agents/claude.rs src/agents/codex.rs
```

Confirm:

- no provider key is printed in errors or tests;
- no signed/redacted thinking is silently discarded;
- no unknown model reaches `build_upstream_request()`;
- no query is used as part of exact route matching;
- no fetch action calls blocking network I/O on the event loop;
- no UI layout file was changed for phase-2 work.

- [ ] **Step 4: Request an independent code review**

The reviewer must inspect only the phase-1 task diffs and report findings ordered by severity, with special attention to provider JSON round-trip, model alias collisions, query parsing, reasoning stream ordering, and API-key exposure.

- [ ] **Step 5: Mark the phase-1 gate complete**

Only after the independent review has no unresolved blocking finding, update the phase-1 tracking document and start a separate phase-2 UI plan. Do not deploy or modify the 9317 service as part of this plan.

## Acceptance Criteria

1. Root `claudeSlots` survives parse/reload and wins over legacy metadata when both exist.
2. Direct Claude/Codex configurations send upstream request names, while local runtime slugs retain client-facing names.
3. Provider detail rows use one normalized order for display, selection, and writes; array order regressions are covered.
4. Multi-select preserves existing `{name, requestName}` mappings.
5. Unknown models return sanitized HTTP 400 before any credential-bearing request.
6. HTTP query parameters are parsed separately from route paths; `period=24h` filters stats correctly.
7. Chat `reasoning_content` reaches Anthropic clients as valid ordinary text; signed/redacted thinking to Chat fails closed.
8. All model fetch actions use background workers and generation/provider checks before applying results.
9. Empty API keys omit auth headers; invalid base URLs fail locally.
10. Per-model cache hit rates use normalized semantics and the documented denominator.
11. Full Cargo verification is green and no secrets or unrelated files are committed.

## Execution Choice

This plan is ready for either:

1. **Subagent-driven execution (recommended):** one fresh subagent per task, with the main agent reviewing and merging each task before the next dependent task.
2. **Inline execution:** execute tasks sequentially in this session with a checkpoint after every task commit.
