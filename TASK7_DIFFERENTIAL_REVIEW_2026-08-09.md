# Task 7 Differential Review

## Scope and Method

This is a snapshot review because the repository has no Git commits or baseline;
all project files are untracked. The current tree was reviewed against:

- `.superpowers/sdd/2026-08-08-c-core-protocol-bridge/task-7-brief.md`
- `.superpowers/sdd/2026-08-08-c-core-protocol-bridge/task-7-report.md`
- `.superpowers/sdd/2026-08-08-c-core-protocol-bridge/task-7-review-package.md`
- `docs/superpowers/plans/2026-08-08-c-core-protocol-bridge.md`

The review covered the listed bridge modules, their focused tests, and the
one-hop shared IR, response, and stream state paths. No implementation files,
staged files, or commits were changed.

## Spec Compliance

| Area | Result | Evidence |
|------|--------|----------|
| Versioned opaque reasoning envelope | Pass | URL-safe no-pad encoding, prefix checking, JSON decoding, and `type == "reasoning"` validation are implemented in `reasoning.rs`. |
| Reasoning round trips | Pass for covered non-streaming paths | Summary plus encrypted, encrypted-only, and visible-only cases are tested. Arbitrary signatures are preserved as opaque data rather than decoded as envelopes. |
| Tool identity and argument validation | Partial | Duplicate IDs, item mismatches, index changes, fragmented arguments, object validation, and result ordering are covered, but a valid later Responses tool turn is rejected. |
| Multimodal handling | Partial | Data URLs, remote URLs, local paths, malformed data, and unsupported blocks are covered, but the remote URL policy does not reject all local-target aliases. |
| Fail-closed conversion | Pass in reviewed paths | Unknown fields, unsupported target fields, opaque reasoning to Chat, and unrepresentable Responses metadata are rejected. |
| Resource bounds | Pass for reviewed counters | Tool arguments, media strings, reasoning envelopes on decode, SSE frames, stream items, and cumulative stream output have explicit limits. |
| Verification scope | Partial | Focused bridge suites pass, but no HTTP-level integration or provider-fetch test exists in this task. |

## Strengths

- The opaque reasoning envelope is versioned, URL-safe, padding-free, and type
  checked before recovery.
- Encrypted reasoning is not silently converted to Chat text; unsupported
  conversions return `Unsupported`.
- Tool state keeps call IDs, optional Responses item IDs, indexes, argument
  bounds, and completion state separate.
- Request and response validators reject duplicate IDs, malformed argument
  objects, orphan results, reordered results, and incompatible target fields.
- Media conversion does not read local files or decode base64 into an
  additional unbounded buffer.
- Error debug/display/public envelopes avoid serializing sensitive field values.

## Issues

### Critical

None identified.

### Important

#### 1. Responses multi-turn tool calls are rejected after the first result

**Files:** `src/runtime/bridge/responses.rs:104-109`,
`src/runtime/bridge/responses.rs:487-500`,
`src/runtime/bridge/request.rs:991-1009`

`parse_request` assigns every Responses `function_call` its index from
`tool_index(&messages)`. That helper searches for the most recent assistant
message anywhere in the input and counts all of its tool calls. After a
`function_call_output`, a new valid assistant tool turn therefore receives
index `1` instead of index `0`. The shared validator resets its expected index
to `0` when the pending result queue becomes empty, so the request fails with
`ToolState`.

Example sequence:

```json
[
  {"type":"function_call","call_id":"call-1","name":"one","arguments":"{}"},
  {"type":"function_call_output","call_id":"call-1","output":"ok"},
  {"type":"function_call","call_id":"call-2","name":"two","arguments":"{}"}
]
```

The third item is a normal next tool turn, but it is given the old turn's
index and rejected. Existing tests cover Chat next-turn behavior and Responses
single-turn tool IDs, but not this valid Responses sequence.

**Impact:** Responses clients cannot perform more than one tool-call turn in a
single request history.

**Recommendation:** Compute the index from the current assistant group only,
or reset the Responses tool index after a `function_call_output`. Add a
regression test with two function-call/result turns and assert both calls are
accepted with index `0` within their respective turns.

#### 2. Remote media URL validation can pass local-target aliases

**File:** `src/runtime/bridge/reasoning.rs:285-330`

The URL validator rejects a small set of literal IPv4/IPv6 local ranges and
the exact `localhost` name, but IPv4-mapped IPv6 addresses are treated as
ordinary IPv6 addresses. For example, `[::ffff:127.0.0.1]` is not rejected by
the IPv6 predicates. Hostname checks also do not cover trailing-dot or
DNS-alias forms such as `localhost.` or `127.0.0.1.nip.io`.

The bridge passes accepted URLs to the provider, which may fetch them. These
forms undermine the stated safe-remote-URL boundary and can expose a provider
or fetcher to loopback/private targets.

**Impact:** Local or private resources can bypass the literal-host checks when
the downstream provider resolves or fetches the media URL.

**Recommendation:** Normalize and reject IPv4-mapped IPv6 addresses using the
same IPv4 private/loopback/link-local policy. Canonicalize hostname trailing
dots and apply private-address checks at the actual URL-fetch boundary with
DNS-rebinding protection. Add regression cases for mapped IPv6, trailing-dot
localhost, and a hostname resolving to a private address.

### Minor

#### 3. Same-turn text after a tool block is rejected without a stated protocol rule

**File:** `src/runtime/bridge/request.rs:1011-1014`

`validate_tool_state` sets `saw_tool_call` after the first `ToolUse` and rejects
every later non-tool content block in that same assistant message. The brief
requires rejecting ordinary assistant text inserted while tool results are
unresolved, but it does not require tool-use blocks to be the final block in
an assistant turn. Anthropic content blocks are otherwise parsed and emitted
in order.

**Impact:** A valid assistant content sequence containing a tool block followed
by text may be rejected. This is lower confidence because the product may
intentionally impose a tool-last policy, but that policy is not documented in
the Task 7 requirements.

**Recommendation:** Either document and test the tool-last invariant, or track
message boundaries separately so only text in a subsequent unresolved
assistant turn is rejected.

## Assessment

**Overall risk:** MEDIUM

**Recommendation:** CONDITIONAL

The reviewed implementation has good defensive structure and focused nominal
coverage, but Task 7 should not be marked complete until the Responses
multi-turn tool-index defect is fixed. The media URL issue also needs a clear
fetch-boundary policy before the implementation is described as safe for
remote URLs. No Critical issue was found.

## Verification

The following commands were rerun serially on the current tree:

```text
cargo test --lib runtime::bridge::reasoning -- --nocapture  # 4 passed
cargo test --lib runtime::bridge::tools -- --nocapture       # 3 passed
cargo test --lib runtime::bridge::request -- --nocapture    # 18 passed
cargo test --lib runtime::bridge::responses -- --nocapture  # 4 passed
```

The first attempted parallel run encountered Cargo build-cache lock races;
those failed commands were rerun serially and passed. The implementer's
report also records passing format, Clippy, bridge, and full-suite checks, but
those results were not independently rerun in this review. No HTTP integration
or provider network fetches were performed.
