# XU Rust Differential Review 2026-08-14

## Executive Summary

| Severity | Count |
|----------|-------|
| Critical | 0 |
| High | 0 |
| Medium | 6 |
| Low | 1 |

**Overall risk:** Medium

**Recommendation:** Conditional. Resolve the six medium findings before treating the provider detail/model-management and runtime-bridge work as complete.

**Scope metrics:** 18 tracked files changed in the worktree, plus 5 untracked top-level artifacts (including the review output, plan files, a backup source file, and `.rust-review-results/`). The tracked diff is +2,846/-851 lines. The review focused on provider persistence, model alias routing, provider-detail selection, OpenCode state handling, model-fetch execution paths, the three-protocol runtime bridge, and the newly added DSH installer flow.

No security vulnerability was identified in the reviewed paths. The findings are functional and state-integrity issues that can silently produce invalid provider configuration.

## What Changed

The relevant history is a sequence of provider-management and model-routing changes ending at `6b18fc9`, plus uncommitted worktree changes. Relevant commits include:

| Commit | Change |
|--------|--------|
| `fd38bea` | Added display/request model names and provider model fetching |
| `7b4b2a4` | Added Claude slot overrides and `--claude-slot` persistence |
| `7165adf` | Added provider-detail model display and selection |
| `8de7189` | Added model multi-select and save |
| `0586d51` | Added preservation of mapped model entries during multi-select |
| `6b18fc9` | Current committed head; worktree also contains further UI changes |

## Findings

### MEDIUM: Claude slot overrides are discarded when the provider store is reloaded

**Files:** `src/cli.rs:2262-2270`, `src/providers/store.rs:38-65,91-104`, `src/app/provider_ops.rs:1236-1252`, `src/agents/claude.rs:123-147`

**Introduced by:** `7b4b2a4` (`feat: claude slot mapping overrides`)

The CLI writes slot overrides at the provider root:

```rust
let slots = provider
    .entry("claudeSlots".to_string())
    .or_insert_with(|| serde_json::json!({}));
```

The store parser only copies model-entry metadata from `provider.models` into `model_metadata`. It does not import the provider-level `claudeSlots` object. The UI and Claude adapter then read `model_metadata["claudeSlots"]`, so a value written by the CLI is invisible after the next reload and falls back to `default_model`.

**Reproduction:**

1. Run `provider update zen --claude-slot opus=claude-opus-5`.
2. Reload the provider through `profiles_from_xu_chat_json`.
3. `claude_slot_value(&provider, "opus")` returns the default/first model instead of `claude-opus-5`.

The existing unit test only inserts `claudeSlots` directly into `model_metadata`, so it does not cover the persistence boundary.

**Impact:** Every UI-selected Claude slot appears to save successfully but is lost from the effective configuration after reload or a later apply operation.

**Recommendation:** Use one canonical representation. Either parse the provider-level object into a dedicated `claude_slots` field and use it everywhere, or deliberately place it under model metadata during serialization and round-trip it through the store parser. Add a JSON round-trip test that starts with the CLI-shaped provider JSON.

### MEDIUM: Direct Claude and Codex adapters send client aliases instead of upstream request names

**Files:** `src/agents/claude.rs:210-218`, `src/agents/codex.rs:47-50`, `src/domain/provider.rs:131-139`, `src/runtime/mod.rs:1574-1588`

**Related change:** `fd38bea` added `ModelEntry::request_name` and runtime resolution, but direct adapters still use `first_model()` unchanged.

For a provider entry such as:

```json
{
  "models": {
    "ds-v4": {
      "name": "DeepSeek V4",
      "requestName": "deepseek-v4-flash-free"
    }
  }
}
```

`first_model()` returns `ds-v4`. The local runtime path correctly calls `request_name_for()`, but direct Claude maps `ANTHROPIC_MODEL` to `ds-v4`, and direct Codex sets its selected model to `ds-v4`.

**Impact:** Single-provider direct configurations can send an alias that the upstream API does not recognize, while the equivalent local-routing configuration works. This makes behavior depend on provider count/routing mode.

**Recommendation:** Resolve `request_name_for(first_model()?)` before constructing direct Claude/Codex mappings. Add direct-path tests with a distinct client key and request name.

### MEDIUM: Provider-detail model row actions can modify the wrong row for array-backed model entries

**Files:** `src/app/provider_ops.rs:1316-1330`, current worktree `src/app/provider_ops.rs:1371-1409`, current worktree `src/main.rs:270-280`

`provider_model_rows()` displays `model_entries.values()`. `model_entries` is a `BTreeMap`, so this sorts entries by client name. The row-edit code and selection initialization index into `provider.models`, which preserves the original JSON array order.

**Reproduction:** An array-backed provider with models `["Zebra", "Alpha", "Mid"]` is parsed with `provider.models` in that order, but the detail table displays `Alpha`, `Mid`, `Zebra`. Selecting visible row 0 and saving calls `detail_row_pick_args(..., row: 0)`, which replaces `provider.models[0]`, namely `Zebra`.

The existing store test at `src/app/provider_ops.rs:2305-2327` verifies that array order survives parsing, but no test verifies that the detail table and row mutation use the same order.

**Impact:** Editing a visible model row can silently replace a different model and corrupt the provider model list.

**Recommendation:** Make `provider_model_rows()` iterate `provider.models` and look up each entry by client name. Use the same ordered row model for display, selection initialization, and serialization. Add a non-alphabetical array-order regression test.

### MEDIUM: Multi-select saving drops display/request alias metadata

**Files:** `src/main.rs:215-229`, `src/app/provider_ops.rs:1333-1360`

`detail_multi_sel_init()` compares fetched upstream request names against the current rows. However, `detail_multi_models_args()` then looks up each selected fetched name directly in `provider.model_entries`, whose keys are client/display names.

For the alias above, the selected value is `deepseek-v4-flash-free`, but the map key is `ds-v4` or `DeepSeek V4`. The lookup misses and emits a plain JSON string:

```rust
_ => serde_json::Value::String(name),
```

Saving the selection therefore converts the mapped object entry into a plain identity string and loses its display/request-name relationship.

**Impact:** Opening model multi-select, keeping an existing aliased model selected, and saving can permanently remove its alias metadata.

**Recommendation:** Carry a stable client-name/request-name pair through the fetched-list selection model, or resolve selected request names back to `ModelEntry` by `entry.request_name` before serializing.

### MEDIUM: Chat reasoning content is emitted as an unsigned Anthropic thinking block

**Files:** current worktree `src/runtime/bridge/chat.rs:465-484,747-779`, `src/runtime/bridge/anthropic.rs:256-289,1045-1057,1250-1262`, `src/runtime/bridge/response.rs:1241-1290`, `tests/runtime_bridge_integration.rs:3249-3323`

The uncommitted Chat decoder now preserves provider `reasoning_content` by inserting an IR thinking block with no signature:

```rust
ContentIr::Thinking {
    text: text.to_string(),
    signature: None,
}
```

When the source client is Anthropic and the upstream provider speaks OpenAI Chat, the Anthropic response encoder emits that IR block as:

```json
{"type":"thinking","thinking":"..."}
```

The `signature` field is only added when the IR contains one. The local Anthropic decoder and regression test explicitly reject an unsigned thinking response as `invalid_upstream` (`src/runtime/bridge/anthropic.rs:282-285`, `src/runtime/bridge/response.rs:1264-1270`). The existing Responses-to-Anthropic path avoids this by carrying a versioned opaque signature, so this is specific to Chat provider reasoning.

**Reproduction:**

1. Route an Anthropic Messages request to an OpenAI Chat provider that returns `choices[0].message.reasoning_content`.
2. Let the Chat decoder build the response IR.
3. Observe the Anthropic response containing a `thinking` block without `signature`.
4. An Anthropic-compatible client validates the response against the thinking-block contract and rejects it or treats the stream as invalid.

The streaming integration test does not cover this direction: `reasoning_streaming_routes_cover_representable_directions()` skips every direction whose target is OpenAI Chat, and no test asserts Chat-provider reasoning converted back to an Anthropic response. The current implementation therefore turns a provider field that was previously discarded into a protocol-invalid downstream response.

**Impact:** Anthropic clients routed to DeepSeek/GLM-style Chat providers can receive an invalid reasoning response whenever the provider returns non-empty `reasoning_content`. Ordinary text-only responses and OpenAI Responses reasoning with an opaque signature are unaffected.

**Recommendation:** Either reject Chat `reasoning_content` when the target is Anthropic because Chat cannot supply a valid Anthropic signature, or define an explicitly supported unsigned-reasoning representation that the Anthropic client contract accepts. Add non-streaming and streaming regression tests for Anthropic-source/OpenAI-Chat-target routing before retaining the preservation behavior.

### MEDIUM: Signed and redacted thinking is silently stripped when converting to OpenAI Chat

**Files:** current worktree `src/runtime/bridge/chat.rs:556-588,1328-1365,1367-1402,1585-1603`, `docs/superpowers/specs/2026-08-11-three-protocol-router-design.md:87-94`, `TASK10_DIFFERENTIAL_REVIEW_2026-08-10.md:10-14`, `CC_SWITCH_GAP_AUDIT.md:178-184`

The Chat encoder previously rejected signed `ContentIr::Thinking` and `ContentIr::RedactedThinking` because OpenAI Chat has no equivalent field. The current worktree instead skips those blocks in both request and response conversion:

```rust
ContentIr::Thinking {
    signature: Some(_), ..
}
| ContentIr::RedactedThinking { .. } => {
    continue;
}
```

This changes a fail-closed protocol boundary into silent data loss. It applies when an Anthropic Messages request with prior signed thinking is sent to a Chat provider, and when a response containing signed or redacted thinking is returned to a Chat client. The replacement unit test, `encoder_strips_opaque_reasoning_in_chat_messages`, now asserts that the opaque block is removed instead of asserting `BridgeError::Unsupported`.

The repository's protocol specification says Responses opaque/encrypted reasoning to Chat must be rejected unless fully reversible. The Task 10 differential review and gap audit repeat the same contract: signed/redacted reasoning to Chat is intentionally unsupported so Xu does not silently drop client state. No design update accompanies this behavioral reversal.

**Reproduction:**

1. Send an Anthropic Messages request containing an assistant `thinking` block with a signature, or a `redacted_thinking` block, through an OpenAI Chat provider.
2. Observe that the Chat upstream request retains visible text but omits the reasoning block without an error.
3. Alternatively, route a response containing signed or redacted thinking to an OpenAI Chat client and observe that the bridge returns a successful response with that content removed.

**Impact:** The bridge reports success after discarding opaque model state that cannot be reconstructed. This can break subsequent tool/reasoning turns and violates the stated safety property that unrepresentable protocol extensions fail closed rather than silently degrade.

**Recommendation:** Restore `unsupported` errors for signed/redacted thinking in the Chat request and response encoders, or update the protocol specification and introduce an explicit, end-to-end loss-tolerant mode with a client-visible diagnostic. Keep the original fail-closed regression tests and add integration coverage for signed Anthropic reasoning and redacted reasoning routed to Chat.

### LOW: Synchronous model-fetch paths remain reachable despite the new asynchronous fetch path

**Files:** current worktree `src/app/provider_flow.rs:29-41`, `src/main.rs:3133-3148`, `src/main.rs:2395-2397`, `src/app/provider_ops.rs:1192-1221,1546-1559`

The new button/action paths call `start_models_fetch_cached()` and run the blocking fetch on a worker thread. Two existing paths still call the synchronous implementation directly:

```rust
KeyCode::Char('m') => {
    match detail_fetch_models_result(&provider_state) {
        // synchronous fetch and UI update
    }
}
```

The provider edit form also calls `fetch_models_into_form()` directly. Both paths reach `spec::cli::run_command()`, then `fetch_models_from_provider()`, which uses blocking network I/O.

**Impact:** Pressing the keyboard shortcut or fetching from the edit form can block the TUI for the provider request timeout, despite the UI presenting an asynchronous model-fetch workflow elsewhere.

**Recommendation:** Route every model-fetch entry point through the same worker/receiver path, or remove the synchronous handlers. Add a testable action-level contract that fetch actions never perform network I/O on the event-loop thread.

## OpenCode State Review

`read_opencode_provider_active()` treats a provider as active when it is present under `opencode.json.provider`. The enable/disable implementation uses the same presence-based contract: enable applies the provider and disable removes it. The reviewed code does not contain a contradictory provider-level `disabled` field, so this was not recorded as a finding.

## Additional Review Notes

- The provider-detail write helper at `src/app/provider_ops.rs:2090-2104` now applies mode `0600` to its temporary file before `rename`; the reviewed provider/settings/cache write paths therefore do not reproduce the earlier permission-downgrade concern. The shared `src/patch.rs:376-419` helper additionally uses `create_new`, `sync_all`, cleanup, and private permissions.
- The DSH installer is reachable through `spec agent setup --yes` and installs `@deepseek-ai/dsh` with `--ignore-scripts`, then applies explicit Termux patches and runs headless bootstrap/ping checks. The no-`--yes` path remains preview-only. The installer was reviewed at flow level and no standalone security finding was recorded; the package supply-chain trust and intentionally unsandboxed Termux executor remain deployment assumptions rather than regressions proven by this diff.
- The DSH install lock is a local coordination mechanism, not a security boundary: it checks for an existing PID and then writes the lock path non-atomically. Concurrent local `spec agent install` processes could still race, but this was not escalated because the impact is installation corruption/duplication rather than provider credential exposure.

## Test Coverage Analysis

Fresh verification passed all existing test binaries: 350, 113, 20, 43, 5, and 291 tests (`822` total). `git diff --check` and `cargo fmt --all -- --check` also passed.

Coverage gaps remain for:

| Area | Coverage |
|------|----------|
| Claude slot JSON round trip | Missing |
| Direct Claude alias mapping | Missing |
| Direct Codex alias mapping | Missing |
| Provider-detail row order and mutation | Missing |
| Multi-select alias preservation | Missing |
| Event-loop non-blocking fetch behavior | Missing |
| Chat reasoning to Anthropic response contract | Missing |
| Signed/redacted reasoning to Chat contract | Regressed from fail-closed to lossy success |

## Blast Radius Analysis

| Surface | Impact |
|---------|--------|
| Claude slot persistence | All providers using `--claude-slot` or the slot UI |
| Direct model alias mapping | Single-provider direct Claude/Codex applications with distinct request names |
| Detail row indexing | Array-backed providers with non-sorted model order |
| Multi-select alias preservation | Any aliased model retained through the multi-select save flow |
| Synchronous fetch paths | Users using the `m` shortcut or edit-form fetch action |
| Chat reasoning compatibility | Anthropic clients using Chat providers that return `reasoning_content` |
| Signed/redacted reasoning | Any cross-protocol request or response containing opaque reasoning routed to Chat |

## Recommendations

### Immediate

- [ ] Fix the `claudeSlots` representation and add a store round-trip test.
- [ ] Resolve request names in direct Claude and Codex adapters.
- [ ] Use one ordered model-row representation for display, selection, and writes.
- [ ] Preserve aliases when multi-select serializes selected models.
- [ ] Reject or correctly represent Chat `reasoning_content` when encoding an Anthropic response.
- [ ] Restore fail-closed handling for signed/redacted reasoning routed to Chat.

### Before Production

- [ ] Move all remaining model-fetch actions off the TUI event loop.
- [ ] Add regression tests for the seven scenarios above.
- [ ] Re-run the full Cargo test suite and `git diff --check` after fixes.

## Analysis Methodology

**Strategy:** Focused review of the provider-management, runtime-routing, and three-protocol bridge surfaces.

**Techniques:** Targeted source reads, call-site searches, current diff inspection, `git blame`, commit history review, data-flow tracing, concrete alias/order scenarios, and full test execution.

**Limitations:** The broad worktree contains unrelated agent-tooling and stats changes that were not reviewed line-by-line. External OpenCode implementation details were not independently audited; the OpenCode conclusion is limited to the repository's presence-based contract.

**Confidence:** High for the seven findings above; medium for the overall worktree because unrelated changed files were out of scope.

## Verification

- `cargo test --quiet`: passed, 822 tests across six non-empty test binaries; 0 failed.
- `cargo test --test runtime_bridge_integration reasoning_streaming_routes_cover_representable_directions -- --nocapture`: passed.
- `cargo test chat_rejects_unsigned_thinking_response -- --nocapture`: passed.
- `git diff --check`: passed.
- `cargo fmt --all -- --check`: passed.
