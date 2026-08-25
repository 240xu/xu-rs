# Task 6 Differential Review

## Executive Summary

| Severity | Count |
|----------|-------|
| CRITICAL | 0 |
| HIGH | 0 |
| MEDIUM | 0 |
| LOW | 0 |

**Overall Risk:** MEDIUM
**Recommendation:** CONDITIONAL

The current Task 6 stream bridge has good nominal coverage and passes the focused
stream suite, formatting, Clippy checks, and full all-target verification. The review
identified five correctness and resource-boundary gaps in malformed lifecycle handling
and input accounting; follow-up fixes cover all five. A second review pass closed four
related lifecycle/cardinality gaps, a third pass closed three ID/finalization gaps, and
a fourth pass closed three terminal-status/post-finish payload gaps. HTTP-level
integration and production-sized mock upstream coverage remain deferred to the later
integration task.

## Scope

Reviewed the Task 6 plan/spec, `src/runtime/bridge/stream.rs`, `tools.rs`,
`anthropic.rs`, `chat.rs`, `responses.rs`, `response.rs`, `ir.rs`, `mod.rs`,
`src/runtime/http.rs`, fixtures, and existing tests. No git baseline or commits
exist, so this is a snapshot review rather than a commit-range differential.

## Findings

### [MEDIUM] SSE frame limits are checked after oversized newline-containing chunks are buffered

**File:** `src/runtime/bridge/stream.rs:L40-L70`

`SseDecoder::feed` performs its preflight limit check only when the input chunk
contains no newline. For a chunk containing a newline, it first executes
`pending.extend_from_slice(bytes)` and only then checks the size while draining
lines. A caller can therefore force allocation of an arbitrarily large chunk
before the per-frame limit is reported.

**Impact:** A large upstream read buffer or direct caller can bypass the intended
early frame-size memory bound and create avoidable allocation pressure.

**Recommendation:** Consume the input incrementally or reject based on the current
line/frame budget before extending `pending`. Keep the existing per-frame reset
behavior for batches containing multiple valid frames.

### [MEDIUM] Anthropic non-tool content blocks have no close-state tracking

**Files:** `src/runtime/bridge/stream.rs:L355-L377`,
`src/runtime/bridge/anthropic.rs:L649-L744`

`wire_block_kinds` records only the block kind and whether it was started. For
text and thinking blocks, `content_block_stop` performs only a kind lookup and
does not record that the block was closed. Duplicate stops are accepted, and a
text or thinking block may reach `message_stop` without any stop event. The shared
completion path checks unfinished tools but not unfinished non-tool blocks.

**Impact:** Malformed Anthropic streams can be accepted as successful responses,
violating the required one-start/one-stop lifecycle and fail-closed terminal
validation.

**Recommendation:** Track open/closed state per wire block, reject duplicate or
unknown stops, and require all started content blocks to be closed before
`message_stop` completes the stream.

### [MEDIUM] Responses non-tool output item IDs and lifecycle states are not bound

**Files:** `src/runtime/bridge/responses.rs:L1138-L1199`,
`src/runtime/bridge/responses.rs:L1201-L1310`,
`src/runtime/bridge/stream.rs:L355-L377`

Responses message and reasoning output items register only their `output_index`
and kind. Their `id`, role, and status are not stored. Later text/reasoning deltas
do not verify `item_id`, and `response.output_item.done` for these item types only
checks the kind at the index. Duplicate done events and mismatched item IDs are
therefore accepted for non-tool items. Function-call items have stricter identity
checks, but that protection is not shared by all output item types.

**Impact:** Inconsistent upstream events can be associated with the wrong output
item or can close an item multiple times while still producing a plausible stream.

**Recommendation:** Store `(output_index, item_id, kind, status)` for every output
item and require subsequent deltas and done events to match the stored identity and
legal lifecycle transition.

### [MEDIUM] Initial and final tool argument payloads bypass the cumulative output limit

**Files:** `src/runtime/bridge/stream.rs:L232-L269`,
`src/runtime/bridge/stream.rs:L417-L425`,
`src/runtime/bridge/tools.rs:L97-L119`

`StreamState::output_bytes` is charged for text, reasoning, and argument deltas,
but not for non-empty arguments supplied in `ToolCallStarted`. Responses
`function_call_arguments.done` also replaces the buffered argument string through
`set_arguments_final` without charging the stream output budget. The per-tool
argument cap still applies, but multiple initial or final payloads can accumulate
well beyond `max_output_bytes`.

**Impact:** The required cumulative output bound is incomplete for tool-heavy
streams, allowing memory growth beyond the configured stream budget.

**Recommendation:** Account initial arguments and authoritative final arguments in
the cumulative budget, avoiding double-counting when a final event repeats already
buffered deltas. Also consider a bounded maximum number of tool calls/content
blocks.

### [MEDIUM] Responses terminal event names are not checked against response status

**File:** `src/runtime/bridge/responses.rs:L1309-L1337`

The `response.completed` and `response.incomplete` branches prefer the embedded
`response.status` over the SSE event name. A `response.incomplete` event carrying
`status: "completed"` is accepted as a completed stream, and the inverse is also
accepted when the embedded status is valid for the other branch. The decoder checks
the resulting completion semantics but not the event/status agreement.

**Impact:** A malformed terminal sequence can be normalized into a different
terminal state than the wire event declared, weakening fail-closed status handling.

**Recommendation:** Require `response.completed` to carry `status: "completed"`
and `response.incomplete` to carry `status: "incomplete"`; validate the required
response fields and status for `response.failed` as well when the provider supplies
the response envelope.

## Final Follow-up Findings

The final follow-up review identified three additional lifecycle/encoding gaps. All
three were fixed and covered by regression tests in the current tree.

### [MEDIUM] Responses incomplete encoder emitted completed status

The encoder used `response.completed` and `completed` output-item status for every
successful `Completed` IR event. It now emits `response.incomplete`, preserves
`status: "incomplete"`, includes `incomplete_details`, and closes text/reasoning items
with `status: "incomplete"` when appropriate.

### [MEDIUM] Responses start events accepted invalid embedded status

`response.created` and `response.in_progress` now require the embedded response status
to be `in_progress` before binding stream identity.

### [MEDIUM] Chat/Anthropic payload was accepted after finish metadata

Chat now rejects non-empty choice payloads after `finish_reason`, while still allowing
usage-only trailing chunks. Anthropic now rejects content payloads after
`stop_reason`, while allowing required block/message close events and ping metadata.

## Verified Fixes

The following issues recorded by the earlier review are fixed in the current tree:

- Multiple complete SSE frames in one `feed` call are handled per frame; the
  preflight no longer rejects aggregate chunk size when the chunk contains newlines.
- Responses `function_call_arguments.done` accepts a complete final argument string
  after multiple deltas through `set_arguments_final`.
- Responses function-call events bind `item_id` to `output_index`.
- Stream usage parsing rejects wrong types and unknown fields.
- Chat response identity and Responses `response_id`/response identity are checked
  after stream start for the covered event families.
- `SseDecoder::feed` consumes input incrementally, so a newline-containing read cannot
  allocate an unbounded pending frame before the per-frame limit is enforced.
- Anthropic text, thinking, and tool blocks now reject duplicate stops and require all
  started blocks to close before `message_stop`.
- Responses message and reasoning items retain item IDs and lifecycle status; deltas
  and done events must match the registered identity and close exactly once.
- Initial tool arguments and authoritative final arguments are charged against the
  cumulative output budget without double-counting a repeated buffered prefix.
- Responses terminal event names must agree with embedded response status, and a
  supplied failed response envelope is identity/status validated.
- Responses terminal events now require every registered output item to be closed, and
  all text, reasoning, tool-argument, and content-part events reject closed items.
- Stream metadata cardinality is capped at `DEFAULT_MAX_STREAM_ITEMS` (`256`) for
  tool calls, wire blocks, and accumulated text/reasoning deltas.
- Encoder block keys and generated message item IDs use isolated namespaces, avoiding
  collisions with arbitrary tool call IDs.
- Responses item IDs are globally unique within a stream, message items require the
  assistant role, and `function_call_arguments.done` is one-shot.
- Responses incomplete terminal events preserve incomplete status and item lifecycle
  status, including `incomplete_details`.
- Responses `response.created` and `response.in_progress` require embedded
  `status: "in_progress"`.
- Chat and Anthropic reject payload deltas after finish metadata while permitting
  protocol-required close/usage metadata.

## Test Coverage

The current focused verification passed:

```text
cargo test --lib runtime::bridge::stream -- --nocapture
38 passed; 0 failed

cargo test --all-targets
263 passed; 0 failed

cargo clippy --all-targets -- -D warnings
passed

cargo fmt --check
passed
```

Existing tests cover CRLF and multiline SSE, malformed JSON, frame limits across
multiple frames, duplicate tool starts, tool argument completion, tool item identity,
usage type validation, response identity, EOF without terminal, output ordering,
nominal encoder lifecycles, closed-item rejection, cardinality limits, and namespace
isolation. HTTP integration coverage remains deferred to Task 9.

## Recommendations

- [x] Add bounded incremental SSE input handling.
- [x] Add Anthropic text/reasoning block close-state validation.
- [x] Add Responses non-tool item identity and lifecycle tracking.
- [x] Include initial/final tool argument payloads in cumulative output accounting.
- [x] Enforce Responses terminal event/status agreement.
- [x] Require Responses output items to close before terminal completion.
- [x] Reject deltas and content events after wire item/block closure.
- [x] Bound stream metadata cardinality.
- [x] Isolate encoder namespaces from arbitrary user IDs.
- [x] Enforce global Responses item IDs and assistant message roles.
- [x] Reject duplicate final tool argument events.
- [x] Preserve Responses incomplete terminal status and item lifecycle status.
- [x] Validate Responses start-event embedded status.
- [x] Reject Chat/Anthropic payload after finish metadata while allowing required
  close/usage metadata.
- [ ] Add HTTP integration tests for malformed SSE, EOF, semantic errors, and limits.
- [ ] Complete the later runtime integration task before treating this bridge as a
  production streaming path.

## Limitations

- No git baseline or commit history is available.
- No production HTTP integration test currently exercises the new bridge API.
- No provider network calls were made; findings are source-level and fixture-level.
- The missing Task 6 brief was reviewed through the available plan and design spec.

## Confidence

HIGH for the source-level findings and follow-up lifecycle checks. Production impact
remains MEDIUM until the later runtime integration and local mock upstream tests
exercise the public decoder with production-sized read buffers.
