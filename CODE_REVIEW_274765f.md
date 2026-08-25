# Differential Code Review: `034ffd6..274765f`

## Executive Summary

| Severity | Count |
|---|---:|
| Critical | 0 |
| High | 1 |
| Medium | 2 |
| Low | 2 |

**Overall risk:** High

**Recommendation:** Reject until the High finding is fixed and covered by a regression test. The Web console binds only to loopback, which limits exposure to users who browse untrusted pages while it is running, but its command endpoint performs state-changing CLI operations and must not rely on `Host` as an origin proof.

**Scope:** Commit `274765f` against baseline `034ffd6`; 10 changed files, 165 additions, 41 deletions. The changed runtime, bridge, Web, CLI, and sync code was reviewed, with direct callers and relevant tests inspected.

## What Changed

| File | Risk | Change summary |
|---|---|---|
| `src/web/mod.rs` | High | Adds a Host-based mutation guard for Web command and stop endpoints. |
| `src/runtime/mod.rs` | Medium | Moves connection-slot ownership to workers and changes overload/timeout handling. |
| `src/runtime/http.rs` | Medium | Adds post-response socket draining and a 503 reason phrase. |
| `src/runtime/bridge/chat.rs` | Medium | Rebinds strict tool metadata after DeepSeek tool sorting; folds stream reasoning into text. |
| `src/agent_tools.rs` | Low | Guards pid truncation in stale install-lock detection. |
| `src/cli.rs` | Low | Prevents `--arg` values from being treated as clear flags. |
| `src/main.rs` | Low | Corrects Web-to-TUI fallback wording. |
| `src/sync.rs` | Low | Excludes sync fingerprints from mirror/fingerprint traversal. |
| `src/web/static/app.js` | Low | Repairs panel ownership and expanded MCP/Skill restoration. |
| `tests/runtime_bridge_integration.rs` | Low | Updates expected Chat reasoning-stream behavior. |

## Findings

### High: Host-based CSRF guard accepts cross-origin mutations

**File:** `src/web/mod.rs:114-124,237-252`  
**Introduced by:** `274765f`  
**Blast radius:** Two state-changing Web routes: `/api/command` and `/api/web/stop`. `/api/command` is a CLI passthrough (`src/web/api.rs:79-114`).  
**Test coverage:** Partial. Tests exercise loopback requests only; they do not exercise a foreign `Origin` combined with the normal loopback `Host`.

`Host` identifies the destination server, not the page that initiated the request. A browser script hosted by an attacker can send a CORS-simple `POST` to the loopback console. The browser will correctly set `Host: 127.0.0.1:<port>`, satisfying `host_is_local()`, despite the initiating document having a foreign origin. The handler does not validate `Origin` or `Referer`, require an unguessable CSRF token, or require a custom header that would cause CORS preflight.

Concrete trigger from an arbitrary website while the console is running:

```js
fetch("http://127.0.0.1:8123/api/web/stop", { method: "POST" });

fetch("http://127.0.0.1:8123/api/command", {
  method: "POST",
  headers: { "Content-Type": "text/plain" },
  body: JSON.stringify({ args: ["agent", "setup", "--yes"] }),
});
```

`text/plain` is CORS-safelisted, and `api::command` JSON-decodes the body without validating the content type. The browser cannot read the response without CORS permission, but it does not need to read it to stop the console or trigger an allowed mutating CLI command.

**Recommendation:** Replace the Host predicate with an origin-authentication design. At minimum, generate a per-server random token, inject it only into the served UI, and require it in a custom request header for every mutating endpoint. Reject non-matching present `Origin` headers as defense in depth. Add raw-socket tests for a loopback Host plus a foreign Origin and a `text/plain` body.

### Medium: Duplicate tool names collapse per-tool `strict` semantics

**File:** `src/runtime/bridge/chat.rs:363-400`  
**Introduced by:** `274765f`  
**Blast radius:** OpenAI Chat requests routed through DeepSeek cache mode with tools.  
**Test coverage:** Missing duplicate-name coverage.

The change correctly avoids positional metadata mismatch after the DeepSeek name sort, but changes correlation to `BTreeMap<&str, bool>`. `parse_chat_request` preserves strict flags positionally (`src/runtime/bridge/request.rs:1062-1085`), while `parse_tools` accepts duplicate function names (`src/runtime/bridge/chat.rs:251-285`). A duplicate name overwrites the prior flag in the map.

For example, two `apply_patch` declarations with `strict: true` and `strict: false` become two declarations with the last flag, silently weakening or strengthening one declaration. This is protocol corruption rather than a safe rejection.

**Recommendation:** Reject duplicate tool names during request parsing, or preserve a stable original ordinal through sorting and use that ordinal for strict restoration. Add regression tests for duplicate names with conflicting strict values in DeepSeek cache mode.

### Medium: Overload rejection blocks the only accept loop on attacker-controlled idle sockets

**File:** `src/runtime/mod.rs:456-464`; `src/runtime/http.rs:197-210`  
**Introduced by:** `274765f`  
**Blast radius:** All new runtime connections while all eight slots are occupied.  
**Test coverage:** Missing production overload-path timing coverage.

Once the eight worker slots are occupied, each rejected connection is written a 503 and then synchronously passed to `drain_incoming`. An idle peer that sends no bytes causes the sole accept-loop thread to wait for the 10 ms read timeout. Repeating this connection pattern serially reduces acceptance/rejection throughput and delays queued local health checks or valid requests.

This contradicts the surrounding stated invariant that full capacity should reject immediately rather than make admission wait on connection behavior. The previous implementation closed immediately, so this is a regression introduced to avoid RST swallowing a 503.

**Recommendation:** Do not synchronously drain in the accept loop. Prefer a bounded nonblocking best-effort drain, `shutdown(Write)` followed by a short independent cleanup worker, or accept that a peer that has not issued a request cannot reliably receive a response. Add an integration test that holds all slots, creates multiple idle rejected sockets, and asserts no cumulative timeout-scale delay before accepting a later connection.

### Low: A client header timeout is counted as a successful request

**File:** `src/runtime/mod.rs:472-479,534-537`; exposed by `src/runtime/mod.rs:1619-1636`  
**Introduced by:** `274765f`  
**Blast radius:** `/health` telemetry and any automation using its success/failure counters.  
**Test coverage:** Missing worker-accounting coverage.

The timeout branch sends a correct 504 then returns `Ok(())` to avoid the prior double-write. The worker interprets every `Ok(())` as success and increments `success_count`. Slow or incomplete clients therefore lower observed failure rates rather than increase them.

**Recommendation:** Use a response outcome that distinguishes a handled client timeout from a successful request, or increment failure accounting in the timeout branch before returning. Keep the single-response invariant. Add a test that exercises the worker accounting path, not only `handle_connection_with_timeout` directly.

### Low: Rejected Web mutations produce `HTTP/1.1 403 OK`

**File:** `src/web/mod.rs:237-249,265-272`  
**Introduced by:** `274765f`  
**Blast radius:** Rejected mutation requests, logs, and strict/nonstandard HTTP consumers.  
**Test coverage:** Missing raw response assertion.

The new guard returns status 403, but `status_text` lacks a `403 => "Forbidden"` arm and falls through to `"OK"`. Standards-compliant clients use the numeric status, so this is not an authorization bypass; it is misleading wire metadata.

**Recommendation:** Add `403 => "Forbidden"`, and test the raw status line for rejected command and stop requests.

## Test Coverage Analysis

The existing suite passed before this review, but the following changed security-relevant paths are not covered adequately:

| Area | Required regression test |
|---|---|
| Web mutations | Foreign `Origin` plus normal loopback `Host`, and a CORS-simple `text/plain` JSON command body. |
| Tool strictness | Duplicate tool names with mixed strict flags under DeepSeek cache sorting. |
| Runtime overload | Eight held slots plus multiple idle rejected sockets, proving accept-loop latency is bounded. |
| Runtime telemetry | Header timeout increments failure accounting without a second HTTP response. |
| HTTP status metadata | Raw 403 response line contains `Forbidden`. |

## Historical Context

The affected code was added in `274765f` with the stated goal of fixing a full-surface review. The baseline did not have the Host guard or synchronous drain, so there is no prior security fix being removed. The issues are regressions in the new mitigation implementations rather than reintroduction of previously removed validation.

## Recommendation

1. Block release adoption of `274765f` until the Host-only mutation guard is replaced with real origin authentication.
2. Resolve duplicate tool-name ambiguity safely and restore per-declaration strictness.
3. Remove blocking cleanup from the runtime accept loop and correct timeout accounting.
4. Add the listed regression tests, run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets` after remediation.

## Review Limits

This was a differential review of `034ffd6..274765f`, not a whole-repository audit. It focused on changed paths and one-hop dependencies. Browser behavior was validated against HTTP/CORS request semantics; no live browser exploit was run because the finding follows directly from the loopback destination Host behavior and the server's lack of origin/token validation.
