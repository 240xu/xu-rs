use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::ir::{BridgeError, ContentIr, ResponseIr, WireProtocol};

pub(super) const EXT_RESPONSE_SOURCE_PROTOCOL: &str = "runtime.response.v1.source_protocol";
pub(super) const EXT_RESPONSES_MESSAGE_ITEM_ID: &str =
    "openai_responses.v1.response_message_item_id";
pub(super) const EXT_RESPONSES_REASONING_ITEM_IDS: &str =
    "openai_responses.v1.response_reasoning_item_ids";
pub(super) const EXT_RESPONSES_COMPLETED_AT: &str = "openai_responses.v1.response_completed_at";
pub(super) const EXT_RESPONSES_INSTRUCTIONS: &str = "openai_responses.v1.response_instructions";
pub(super) const EXT_RESPONSES_REASONING: &str = "openai_responses.v1.response_reasoning";
pub(super) const EXT_RESPONSES_INCOMPLETE_DETAILS: &str =
    "openai_responses.v1.response_incomplete_details";
pub(super) const EXT_RESPONSES_OUTPUT_ITEM_STATUSES: &str =
    "openai_responses.v1.response_output_item_statuses";
pub(super) const EXT_RESPONSES_REASONING_ITEM_CONTENT: &str =
    "openai_responses.v1.response_reasoning_item_content";
pub(super) const EXT_RESPONSES_CONVERSATION: &str = "openai_responses.v1.response_conversation";
pub(super) const EXT_RESPONSES_MODERATION: &str = "openai_responses.v1.response_moderation";
pub(super) const EXT_RESPONSES_PROMPT_CACHE_OPTIONS: &str =
    "openai_responses.v1.response_prompt_cache_options";

static NEXT_GENERATED_ID: AtomicU64 = AtomicU64::new(0);

pub fn decode_response(protocol: WireProtocol, body: Value) -> Result<ResponseIr, BridgeError> {
    let mut ir = match protocol {
        WireProtocol::AnthropicMessages => super::anthropic::decode_response(body),
        WireProtocol::OpenAiChat => super::chat::decode_response(body),
        WireProtocol::OpenAiResponses => super::responses::decode_response(body),
    }?;
    ir.extensions.insert(
        EXT_RESPONSE_SOURCE_PROTOCOL.to_string(),
        Value::String(protocol_tag(protocol).to_string()),
    );
    validate_response_ir(&ir)?;
    Ok(ir)
}

pub fn encode_response(ir: ResponseIr, target: WireProtocol) -> Result<Value, BridgeError> {
    if ir.completion.error.is_some() || ir.completion.status.as_deref() == Some("failed") {
        return match target {
            WireProtocol::AnthropicMessages => super::anthropic::encode_error_response(&ir),
            WireProtocol::OpenAiChat => super::chat::encode_error_response(&ir),
            WireProtocol::OpenAiResponses => super::responses::encode_error_response(&ir),
        };
    }

    validate_response_ir(&ir)?;
    reject_unrepresentable_responses_ids(&ir, target)?;
    match target {
        WireProtocol::AnthropicMessages => super::anthropic::encode_response(&ir),
        WireProtocol::OpenAiChat => super::chat::encode_response(&ir),
        WireProtocol::OpenAiResponses => super::responses::encode_response(&ir),
    }
}

pub(super) fn reject_unrepresentable_responses_ids(
    ir: &ResponseIr,
    target: WireProtocol,
) -> Result<(), BridgeError> {
    if target == WireProtocol::OpenAiResponses
        || !is_source_protocol(ir, WireProtocol::OpenAiResponses)
    {
        return Ok(());
    }
    for (extension, field) in [
        (EXT_RESPONSES_MESSAGE_ITEM_ID, "responses.item_id"),
        (EXT_RESPONSES_COMPLETED_AT, "responses.completed_at"),
        (EXT_RESPONSES_INSTRUCTIONS, "responses.instructions"),
        (EXT_RESPONSES_REASONING, "responses.reasoning"),
        (
            EXT_RESPONSES_OUTPUT_ITEM_STATUSES,
            "responses.output_item_status",
        ),
        (
            EXT_RESPONSES_REASONING_ITEM_CONTENT,
            "responses.reasoning_item_content",
        ),
        (EXT_RESPONSES_CONVERSATION, "responses.conversation"),
        (EXT_RESPONSES_MODERATION, "responses.moderation"),
        (
            EXT_RESPONSES_PROMPT_CACHE_OPTIONS,
            "responses.prompt_cache_options",
        ),
    ] {
        if ir.extensions.contains_key(extension)
            && !matches!(
                extension,
                EXT_RESPONSES_OUTPUT_ITEM_STATUSES
                    if ir.extensions[extension]
                        .as_object()
                        .is_some_and(|statuses| {
                            statuses.values().all(|status| status == "completed")
                        })
            )
        {
            return Err(BridgeError::Unsupported {
                field: field.to_string(),
            });
        }
    }
    if ir
        .extensions
        .get(EXT_RESPONSES_REASONING_ITEM_IDS)
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|id| id.as_str().is_some()))
    {
        return Err(BridgeError::Unsupported {
            field: "responses.reasoning_item_id".to_string(),
        });
    }
    Ok(())
}

pub(super) fn validate_response_ir(ir: &ResponseIr) -> Result<(), BridgeError> {
    if ir
        .meta
        .id
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        || ir
            .meta
            .model
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(BridgeError::InvalidUpstream);
    }
    if ir.completion.error.is_some() || ir.completion.status.as_deref() == Some("failed") {
        return Err(BridgeError::InvalidUpstream);
    }
    if let Some(status) = ir.completion.status.as_deref() {
        if !matches!(status, "completed" | "incomplete") {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    if ir.completion.finish_reason.is_none()
        && ir.completion.stop_reason.is_none()
        && ir.completion.status.is_none()
    {
        return Err(BridgeError::InvalidUpstream);
    }

    let mut call_ids = BTreeSet::new();
    let mut item_ids = BTreeSet::new();
    let mut expected_index = 0;
    for content in &ir.content {
        match content {
            ContentIr::ToolUse(call) => {
                if call.call_id.trim().is_empty()
                    || call.name.trim().is_empty()
                    || !call.arguments.is_object()
                    || call.index != expected_index
                    || !call_ids.insert(call.call_id.clone())
                    || call.item_id.as_ref().is_some_and(|item_id| {
                        item_id.trim().is_empty() || !item_ids.insert(item_id.clone())
                    })
                {
                    return Err(BridgeError::ToolState);
                }
                expected_index += 1;
            }
            ContentIr::ToolResult(_) => {
                return Err(BridgeError::Unsupported {
                    field: "response.tool_result".to_string(),
                })
            }
            _ => {}
        }
    }
    validate_completion_semantics(&ir.completion, &ir.content)?;
    if ir.extensions.contains_key(EXT_RESPONSES_INCOMPLETE_DETAILS)
        && ir.completion.status.as_deref() != Some("incomplete")
    {
        return Err(BridgeError::InvalidUpstream);
    }
    Ok(())
}

fn validate_completion_semantics(
    completion: &super::ir::CompletionIr,
    content: &[ContentIr],
) -> Result<(), BridgeError> {
    let finish_reason = completion.finish_reason.as_deref();
    let stop_reason = completion.stop_reason.as_deref();
    let has_tool_call = content
        .iter()
        .any(|item| matches!(item, ContentIr::ToolUse(_)));

    if let (Some(finish_reason), Some(stop_reason)) = (finish_reason, stop_reason) {
        let consistent = match stop_reason {
            "end_turn" | "stop_sequence" => finish_reason == "stop",
            "max_tokens" => finish_reason == "length",
            "tool_use" => finish_reason == "tool_calls",
            "refusal" => finish_reason == "content_filter",
            _ => true,
        };
        if !consistent {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    if let Some(status) = completion.status.as_deref() {
        match status {
            "completed" => {
                if finish_reason == Some("length") || stop_reason == Some("max_tokens") {
                    return Err(BridgeError::InvalidUpstream);
                }
            }
            "incomplete" => {
                let max_tokens =
                    finish_reason == Some("length") && stop_reason == Some("max_tokens");
                let content_filter =
                    finish_reason == Some("content_filter") && stop_reason == Some("refusal");
                if !max_tokens && !content_filter {
                    return Err(BridgeError::InvalidUpstream);
                }
            }
            _ => return Err(BridgeError::InvalidUpstream),
        }
    }
    if !has_tool_call {
        if finish_reason == Some("tool_calls") || stop_reason == Some("tool_use") {
            return Err(BridgeError::InvalidUpstream);
        }
    } else if finish_reason.is_some_and(|reason| reason != "tool_calls")
        || stop_reason.is_some_and(|reason| reason != "tool_use")
    {
        return Err(BridgeError::InvalidUpstream);
    }
    Ok(())
}

pub(super) fn protocol_tag(protocol: WireProtocol) -> &'static str {
    match protocol {
        WireProtocol::AnthropicMessages => "anthropic_messages",
        WireProtocol::OpenAiChat => "openai_chat",
        WireProtocol::OpenAiResponses => "openai_responses",
    }
}

pub(super) fn is_source_protocol(ir: &ResponseIr, protocol: WireProtocol) -> bool {
    ir.extensions
        .get(EXT_RESPONSE_SOURCE_PROTOCOL)
        .and_then(Value::as_str)
        == Some(protocol_tag(protocol))
}

pub(super) fn target_response_id(ir: &ResponseIr, target: WireProtocol) -> String {
    if is_source_protocol(ir, target) {
        if let Some(id) = ir.meta.id.as_deref().filter(|id| !id.trim().is_empty()) {
            return id.to_string();
        }
    }
    generated_id(match target {
        WireProtocol::AnthropicMessages => "msg",
        WireProtocol::OpenAiChat => "chatcmpl",
        WireProtocol::OpenAiResponses => "resp",
    })
}

pub(super) fn generated_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sequence = NEXT_GENERATED_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_bridge_{now}_{sequence}")
}

pub(super) fn generated_created_at() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn response_model(ir: &ResponseIr) -> Result<String, BridgeError> {
    ir.meta
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .ok_or(BridgeError::InvalidUpstream)
}

pub(super) fn error_model(ir: &ResponseIr) -> String {
    ir.meta
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn safe_error_message() -> &'static str {
    BridgeError::InvalidUpstream.public_message()
}

pub(super) fn required_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Map<String, Value>, BridgeError> {
    object
        .get(field)
        .and_then(Value::as_object)
        .ok_or(BridgeError::InvalidUpstream)
}

pub(super) fn required_array<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Vec<Value>, BridgeError> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or(BridgeError::InvalidUpstream)
}

pub(super) fn required_string(
    object: &Map<String, Value>,
    field: &str,
) -> Result<String, BridgeError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or(BridgeError::InvalidUpstream)
}

pub(super) fn required_u64(object: &Map<String, Value>, field: &str) -> Result<u64, BridgeError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(BridgeError::InvalidUpstream)
}

pub(super) fn optional_u64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u64>, BridgeError> {
    object
        .get(field)
        .map(|value| value.as_u64().ok_or(BridgeError::InvalidUpstream))
        .transpose()
}

pub(super) fn normalize_total_tokens(
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
) -> Result<Option<u64>, BridgeError> {
    if let (Some(input), Some(output), Some(total)) = (input_tokens, output_tokens, total_tokens) {
        if input.checked_add(output) != Some(total) {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    match (input_tokens, output_tokens, total_tokens) {
        (Some(input), Some(output), None) => input
            .checked_add(output)
            .map(Some)
            .ok_or(BridgeError::InvalidUpstream),
        _ => Ok(total_tokens),
    }
}

pub(super) fn map_upstream_error(error: BridgeError) -> BridgeError {
    match error {
        BridgeError::Unsupported { field } => BridgeError::Unsupported { field },
        BridgeError::ResourceLimit => BridgeError::ResourceLimit,
        _ => BridgeError::InvalidUpstream,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{json, Value};

    use super::super::{
        decode_response, encode_response, BridgeError, CompletionIr, ContentIr, ResponseIr,
        ResponseMetaIr, UsageIr, WireProtocol,
    };

    fn fixture(name: &str) -> Value {
        serde_json::from_str(match name {
            "anthropic" => {
                include_str!("../../../tests/fixtures/runtime_bridge/anthropic_text_response.json")
            }
            "chat" => {
                include_str!("../../../tests/fixtures/runtime_bridge/chat_text_response.json")
            }
            "responses" => {
                include_str!("../../../tests/fixtures/runtime_bridge/responses_text_response.json")
            }
            _ => panic!("unknown response fixture"),
        })
        .expect("response fixture is valid JSON")
    }

    fn responses_text_without_item_id() -> Value {
        let mut response = fixture("responses");
        response["output"][0]
            .as_object_mut()
            .expect("message item is an object")
            .remove("id");
        response["usage"]["input_tokens_details"]
            .as_object_mut()
            .expect("usage details are an object")
            .remove("cache_write_tokens");
        response
    }

    fn anthropic_text_without_cache_write() -> Value {
        let mut response = fixture("anthropic");
        response["usage"]
            .as_object_mut()
            .expect("usage is an object")
            .remove("cache_creation_input_tokens");
        response
    }

    fn assert_text_response(ir: &ResponseIr, text: &str, expected_input: u64) {
        assert!(matches!(&ir.content[..], [ContentIr::Text(value)] if value == text));
        assert_eq!(
            ir.usage.as_ref().and_then(|usage| usage.input_tokens),
            Some(expected_input)
        );
        assert_eq!(
            ir.usage.as_ref().and_then(|usage| usage.output_tokens),
            Some(4)
        );
        assert_eq!(
            ir.usage.as_ref().and_then(|usage| usage.total_tokens),
            Some(expected_input + 4)
        );
    }

    #[test]
    fn six_cross_protocol_text_directions_decode_and_encode() {
        let cases = [
            (
                WireProtocol::AnthropicMessages,
                WireProtocol::OpenAiChat,
                anthropic_text_without_cache_write(),
            ),
            (
                WireProtocol::AnthropicMessages,
                WireProtocol::OpenAiResponses,
                fixture("anthropic"),
            ),
            (
                WireProtocol::OpenAiChat,
                WireProtocol::AnthropicMessages,
                fixture("chat"),
            ),
            (
                WireProtocol::OpenAiChat,
                WireProtocol::OpenAiResponses,
                fixture("chat"),
            ),
            (
                WireProtocol::OpenAiResponses,
                WireProtocol::AnthropicMessages,
                responses_text_without_item_id(),
            ),
            (
                WireProtocol::OpenAiResponses,
                WireProtocol::OpenAiChat,
                responses_text_without_item_id(),
            ),
        ];

        for (source, target, body) in cases {
            let ir = decode_response(source, body).expect("source response decodes");
            // The anthropic fixture bills 12 (cache excluded) so its normalized
            // full input is 14 (12 + 2 cache_read); chat/responses fixtures
            // report a full input of 12 directly.
            let expected_input = if source == WireProtocol::AnthropicMessages {
                14
            } else {
                12
            };
            assert_text_response(&ir, "fixture response text", expected_input);
            let encoded = encode_response(ir, target).expect("target response encodes");
            match target {
                WireProtocol::AnthropicMessages => {
                    assert_eq!(encoded["type"], json!("message"));
                    assert_eq!(encoded["role"], json!("assistant"));
                    assert_eq!(
                        encoded["content"][0],
                        json!({
                            "type": "text",
                            "text": "fixture response text"
                        })
                    );
                    assert_eq!(encoded["stop_reason"], json!("end_turn"));
                    assert!(encoded["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("msg_")));
                }
                WireProtocol::OpenAiChat => {
                    assert_eq!(encoded["object"], json!("chat.completion"));
                    assert_eq!(encoded["choices"].as_array().map(Vec::len), Some(1));
                    assert_eq!(encoded["choices"][0]["finish_reason"], json!("stop"));
                    assert_eq!(
                        encoded["choices"][0]["message"]["content"],
                        json!("fixture response text")
                    );
                    assert!(encoded["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("chatcmpl_")));
                }
                WireProtocol::OpenAiResponses => {
                    assert_eq!(encoded["object"], json!("response"));
                    assert_eq!(encoded["status"], json!("completed"));
                    assert_eq!(encoded["output"][0]["type"], json!("message"));
                    assert_eq!(
                        encoded["output"][0]["content"][0]["text"],
                        json!("fixture response text")
                    );
                    assert_eq!(encoded["output_text"], json!("fixture response text"));
                    assert!(encoded["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("resp_")));
                }
            }
        }

        let responses_with_item_id =
            decode_response(WireProtocol::OpenAiResponses, fixture("responses"))
                .expect("Responses response with item ID decodes");
        // item_id 对 chat 无表达：拒绝（responses.item_id）。
        assert!(matches!(
            encode_response(responses_with_item_id, WireProtocol::OpenAiChat),
            Err(BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn response_tool_calls_preserve_call_ids_and_responses_item_ids() {
        let cases = [
            (
                WireProtocol::AnthropicMessages,
                json!({
                    "id": "msg_tool",
                    "type": "message",
                    "role": "assistant",
                    "model": "anthropic-model",
                    "content": [{
                        "type": "tool_use",
                        "id": "call-tool",
                        "name": "lookup",
                        "input": {"q": "rust"}
                    }],
                    "stop_reason": "tool_use",
                    "usage": {"input_tokens": 10, "output_tokens": 3}
                }),
            ),
            (
                WireProtocol::OpenAiChat,
                json!({
                    "id": "chat_tool",
                    "object": "chat.completion",
                    "model": "chat-model",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call-tool",
                                "type": "function",
                                "function": {"name": "lookup", "arguments": "{\"q\":\"rust\"}"}
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
                }),
            ),
            (
                WireProtocol::OpenAiResponses,
                json!({
                    "id": "resp_tool",
                    "object": "response",
                    "model": "responses-model",
                    "status": "completed",
                    "output": [{
                        "id": "fc_item",
                        "type": "function_call",
                        "call_id": "call-tool",
                        "name": "lookup",
                        "arguments": "{\"q\":\"rust\"}",
                        "status": "completed"
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 3, "total_tokens": 13}
                }),
            ),
        ];

        for (source, body) in &cases {
            let ir = decode_response(*source, body.clone()).expect("tool response decodes");
            let ContentIr::ToolUse(call) = &ir.content[0] else {
                panic!("expected one tool call")
            };
            assert_eq!(call.call_id, "call-tool");
            assert_eq!(call.name, "lookup");
            assert_eq!(call.arguments, json!({"q": "rust"}));

            let same_protocol =
                encode_response(ir.clone(), *source).expect("same response encodes");
            match source {
                WireProtocol::AnthropicMessages => {
                    assert_eq!(same_protocol["content"][0]["id"], json!("call-tool"));
                    assert_eq!(same_protocol["stop_reason"], json!("tool_use"));
                }
                WireProtocol::OpenAiChat => {
                    assert_eq!(
                        same_protocol["choices"][0]["message"]["tool_calls"][0]["id"],
                        json!("call-tool")
                    );
                    assert_eq!(
                        same_protocol["choices"][0]["finish_reason"],
                        json!("tool_calls")
                    );
                }
                WireProtocol::OpenAiResponses => {
                    assert_eq!(same_protocol["output"][0]["id"], json!("fc_item"));
                    assert_eq!(same_protocol["output"][0]["call_id"], json!("call-tool"));
                }
            }
        }

        let anthropic_ir = decode_response(WireProtocol::AnthropicMessages, cases[0].1.clone())
            .expect("Anthropic tool response decodes");
        let responses = encode_response(anthropic_ir, WireProtocol::OpenAiResponses)
            .expect("Anthropic tool response encodes to Responses");
        assert_eq!(responses["output"][0]["call_id"], json!("call-tool"));
        assert!(responses["output"][0]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("fc_")));

        let mixed = json!({
            "id": "msg_mixed",
            "type": "message",
            "role": "assistant",
            "model": "anthropic-model",
            "content": [
                {"type": "text", "text": "before"},
                {"type": "tool_use", "id": "call-mixed", "name": "lookup", "input": {}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let mixed_ir = decode_response(WireProtocol::AnthropicMessages, mixed)
            .expect("mixed Anthropic response decodes");
        assert!(matches!(
            mixed_ir.content[1],
            ContentIr::ToolUse(ref call) if call.index == 0
        ));

        let responses_ir = decode_response(WireProtocol::OpenAiResponses, cases[2].1.clone())
            .expect("Responses tool response decodes");
        assert!(matches!(
            encode_response(responses_ir, WireProtocol::OpenAiChat),
            Err(BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn reasoning_usage_and_next_tool_turn_share_normalized_ir() {
        let body = json!({
            "id": "resp_reasoning",
            "object": "response",
            "model": "reasoning-model",
            "status": "completed",
            "output": [
                {
                    "id": "rs_item",
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "plan"}],
                    "encrypted_content": "opaque-plan"
                },
                {
                    "id": "msg_item",
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "answer"}]
                }
            ],
            "output_text": "answer",
            "usage": {
                "input_tokens": 20,
                "output_tokens": 9,
                "total_tokens": 29,
                "input_tokens_details": {"cached_tokens": 4, "cache_write_tokens": 2},
                "output_tokens_details": {"reasoning_tokens": 5}
            }
        });

        let ir = decode_response(WireProtocol::OpenAiResponses, body)
            .expect("reasoning response decodes");
        assert!(matches!(
            &ir.content[0],
            ContentIr::Thinking { text, signature: Some(signature) }
                if text == "plan"
                    && signature.starts_with("ccswitch-openai-reasoning-v1:")
        ));
        assert!(matches!(&ir.content[1], ContentIr::Text(text) if text == "answer"));
        assert_eq!(
            ir.usage.as_ref().and_then(|usage| usage.cache_read_tokens),
            Some(4)
        );
        assert_eq!(
            ir.usage.as_ref().and_then(|usage| usage.cache_write_tokens),
            Some(2)
        );
        assert_eq!(
            ir.usage.as_ref().and_then(|usage| usage.reasoning_tokens),
            Some(5)
        );

        let responses = encode_response(ir.clone(), WireProtocol::OpenAiResponses)
            .expect("Responses reasoning preserves Responses usage");
        assert_eq!(
            responses["usage"]["input_tokens_details"]["cache_write_tokens"],
            json!(2)
        );
        assert_eq!(
            responses["usage"]["output_tokens_details"]["reasoning_tokens"],
            json!(5)
        );

        let mut anthropic_ir = ir.clone();
        anthropic_ir.usage = Some(UsageIr {
            input_tokens: Some(20),
            output_tokens: Some(9),
            total_tokens: Some(29),
            cache_read_tokens: Some(4),
            cache_write_tokens: None,
            reasoning_tokens: None,
        });
        assert!(matches!(
            encode_response(anthropic_ir, WireProtocol::AnthropicMessages),
            Err(BridgeError::Unsupported { .. })
        ));

        let anthropic_source = json!({
            "id": "msg_reasoning",
            "type": "message",
            "role": "assistant",
            "model": "reasoning-model",
            "content": [{"type": "thinking", "thinking": "plan", "signature": "opaque-plan"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 20, "output_tokens": 9}
        });
        let anthropic = decode_response(WireProtocol::AnthropicMessages, anthropic_source)
            .expect("Anthropic reasoning response decodes");
        let responses = encode_response(anthropic, WireProtocol::OpenAiResponses)
            .expect("Anthropic reasoning encodes to Responses");
        assert_eq!(responses["output"][0]["type"], json!("reasoning"));
        assert_eq!(
            responses["output"][0]["encrypted_content"],
            json!("opaque-plan")
        );

        assert!(matches!(
            encode_response(ir.clone(), WireProtocol::OpenAiChat),
            Err(BridgeError::Unsupported { .. })
        ));

        let tool_turn = json!({
            "model": "chat-model",
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-tool", "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"q\":\"rust\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call-tool", "content": "tool result"}
            ]
        });
        let next_turn = super::super::parse_request(WireProtocol::OpenAiChat, &tool_turn)
            .expect("next tool turn parses after response call");
        assert!(matches!(
            &next_turn.messages[1].content[0],
            ContentIr::ToolResult(result) if result.call_id == "call-tool" && !result.is_error
        ));
    }

    #[test]
    fn response_decoders_fail_closed_on_semantic_and_terminal_errors() {
        let cases = [
            (
                WireProtocol::AnthropicMessages,
                json!({"type": "error", "error": {"type": "api_error", "message": "secret"}}),
            ),
            (
                WireProtocol::AnthropicMessages,
                json!({"id": "msg", "type": "message", "role": "assistant", "model": "m", "content": [], "usage": {"input_tokens": 1, "output_tokens": 1}}),
            ),
            (
                WireProtocol::OpenAiChat,
                json!({"id": "chat", "object": "chat.completion", "model": "m", "choices": []}),
            ),
            (
                WireProtocol::OpenAiChat,
                json!({"id": "chat", "object": "chat.completion", "model": "m", "choices": [{"index": 0, "message": {"role": "assistant", "content": "x"}}]}),
            ),
            (
                WireProtocol::OpenAiResponses,
                json!({"id": "resp", "object": "response", "model": "m", "status": "failed", "output": [], "error": {"code": "secret", "message": "secret"}}),
            ),
            (
                WireProtocol::OpenAiResponses,
                json!({"id": "resp", "object": "response", "model": "m", "status": "in_progress", "output": []}),
            ),
        ];

        for (protocol, body) in cases {
            assert_eq!(
                decode_response(protocol, body),
                Err(BridgeError::InvalidUpstream)
            );
        }
    }

    #[test]
    fn failed_response_ir_uses_safe_target_error_envelopes() {
        let ir = ResponseIr {
            meta: ResponseMetaIr {
                id: Some("source-id".to_string()),
                model: Some("model".to_string()),
            },
            content: Vec::new(),
            usage: None,
            completion: CompletionIr {
                status: Some("failed".to_string()),
                error: Some("raw upstream secret".to_string()),
                ..CompletionIr::default()
            },
            extensions: BTreeMap::new(),
        };

        let anthropic = encode_response(ir.clone(), WireProtocol::AnthropicMessages)
            .expect("Anthropic error encodes");
        assert_eq!(anthropic["type"], json!("error"));
        assert_eq!(anthropic["error"]["type"], json!("api_error"));
        assert_eq!(
            anthropic["error"]["message"],
            json!("invalid upstream response")
        );
        assert!(!anthropic.to_string().contains("raw upstream secret"));

        let chat =
            encode_response(ir.clone(), WireProtocol::OpenAiChat).expect("Chat error encodes");
        assert_eq!(chat["error"]["type"], json!("invalid_upstream"));
        assert_eq!(chat["error"]["message"], json!("invalid upstream response"));
        assert!(!chat.to_string().contains("raw upstream secret"));

        let responses =
            encode_response(ir, WireProtocol::OpenAiResponses).expect("Responses error encodes");
        assert_eq!(responses["status"], json!("failed"));
        assert_eq!(responses["error"]["code"], json!("invalid_upstream"));
        assert_eq!(
            responses["error"]["message"],
            json!("invalid upstream response")
        );
        assert!(!responses.to_string().contains("raw upstream secret"));
    }

    #[test]
    fn malformed_tool_arguments_and_unrepresentable_fields_are_rejected() {
        let malformed = json!({
            "id": "chat",
            "object": "chat.completion",
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "not-json"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        assert_eq!(
            decode_response(WireProtocol::OpenAiChat, malformed),
            Err(BridgeError::InvalidUpstream)
        );

        let anthropic = json!({
            "id": "msg",
            "type": "message",
            "role": "assistant",
            "model": "m",
            "content": [{"type": "tool_use", "id": "call", "name": "lookup", "input": {}}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let mut ir = decode_response(WireProtocol::AnthropicMessages, anthropic)
            .expect("Anthropic tool response decodes");
        if let ContentIr::ToolUse(call) = &mut ir.content[0] {
            call.item_id = Some("responses-only-item".to_string());
        }
        assert!(matches!(
            encode_response(ir, WireProtocol::OpenAiChat),
            Err(BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn usage_encoding_rejects_unrepresentable_cache_and_reasoning_values() {
        let ir = ResponseIr {
            meta: ResponseMetaIr {
                id: Some("id".to_string()),
                model: Some("model".to_string()),
            },
            content: vec![ContentIr::Text("text".to_string())],
            usage: Some(UsageIr {
                input_tokens: Some(1),
                output_tokens: Some(2),
                total_tokens: Some(3),
                cache_read_tokens: Some(1),
                cache_write_tokens: Some(1),
                reasoning_tokens: Some(2),
            }),
            completion: CompletionIr {
                finish_reason: Some("stop".to_string()),
                stop_reason: Some("end_turn".to_string()),
                status: Some("completed".to_string()),
                error: None,
            },
            extensions: BTreeMap::new(),
        };
        // cache_write 对 chat 无表达：丢弃而非拒绝（与流式一致）。
        let encoded = encode_response(ir, WireProtocol::OpenAiChat)
            .expect("nonzero cache-write dropped, not rejected");
        assert_eq!(encoded["usage"]["prompt_tokens"], 1);
        assert_eq!(encoded["usage"]["total_tokens"], 3);
    }

    #[test]
    fn standard_responses_success_fields_are_accepted_and_round_trip() {
        let mut body = fixture("responses");
        body["completed_at"] = json!(1700000001);
        body["instructions"] = json!("Follow the response contract.");
        body["reasoning"] = json!({"effort": "medium"});
        body["parallel_tool_calls"] = json!(true);
        body["store"] = json!(true);
        body["temperature"] = json!(1.0);
        body["text"] = json!({"format": {"type": "text"}});
        body["tool_choice"] = json!("auto");
        body["tools"] = json!([]);
        body["top_p"] = json!(1.0);
        body["truncation"] = json!("disabled");
        body["user"] = Value::Null;
        body["metadata"] = json!({});

        let ir = decode_response(WireProtocol::OpenAiResponses, body)
            .expect("standard Responses success response decodes");
        let encoded = encode_response(ir, WireProtocol::OpenAiResponses)
            .expect("standard Responses success response encodes");
        assert_eq!(encoded["completed_at"], json!(1700000001));
        assert_eq!(
            encoded["instructions"],
            json!("Follow the response contract.")
        );
        assert_eq!(encoded["reasoning"]["effort"], json!("medium"));
    }

    #[test]
    fn responses_standard_metadata_shapes_round_trip_and_reject_malformed_values() {
        let mut body = fixture("responses");
        body["conversation"] = json!({"id": "conv_fixture"});
        body["prompt_cache_options"] = json!({"ttl": "30m", "mode": "implicit"});
        body["moderation"] = json!({
            "input": {
                "type": "moderation_result",
                "model": "omni-moderation-latest",
                "flagged": false,
                "categories": {"violence": false},
                "category_scores": {"violence": 0.01},
                "category_applied_input_types": {"violence": ["text"]}
            },
            "output": {
                "type": "error",
                "code": "moderation_unavailable",
                "message": "fixture moderation error"
            }
        });

        let ir = decode_response(WireProtocol::OpenAiResponses, body.clone())
            .expect("documented Responses metadata decodes");
        let encoded = encode_response(ir, WireProtocol::OpenAiResponses)
            .expect("documented Responses metadata encodes");
        assert_eq!(encoded["conversation"], body["conversation"]);
        assert_eq!(
            encoded["prompt_cache_options"],
            body["prompt_cache_options"]
        );
        assert_eq!(encoded["moderation"], body["moderation"]);

        for (field, value) in [
            (
                "conversation",
                json!({"id": "conv_fixture", "unsafe": true}),
            ),
            (
                "prompt_cache_options",
                json!({"ttl": "1h", "mode": "implicit"}),
            ),
            ("prompt_cache_options", json!({"mode": "implicit"})),
            ("prompt_cache_options", Value::Null),
            (
                "moderation",
                json!({
                    "input": {
                        "type": "moderation_result",
                        "model": "omni-moderation-latest",
                        "flagged": false,
                        "categories": {},
                        "category_scores": {},
                        "category_applied_input_types": {},
                        "unsafe": true
                    },
                    "output": null
                }),
            ),
        ] {
            let mut malformed = fixture("responses");
            malformed[field] = value;
            assert!(matches!(
                decode_response(WireProtocol::OpenAiResponses, malformed),
                Err(BridgeError::InvalidUpstream | BridgeError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn responses_array_instructions_and_richer_reasoning_round_trip() {
        let mut body = fixture("responses");
        body["instructions"] = json!([
            {
                "type": "message",
                "role": "developer",
                "content": [{"type": "input_text", "text": "Use concise answers."}]
            }
        ]);
        body["reasoning"] = json!({
            "mode": "all_turns",
            "effort": "high",
            "summary": "detailed",
            "context": "current_turn",
            "generate_summary": "auto"
        });

        let ir = decode_response(WireProtocol::OpenAiResponses, body.clone())
            .expect("array instructions and richer reasoning decode");
        let encoded = encode_response(ir, WireProtocol::OpenAiResponses)
            .expect("array instructions and richer reasoning encode");
        assert_eq!(encoded["instructions"], body["instructions"]);
        assert_eq!(encoded["reasoning"], body["reasoning"]);
    }

    #[test]
    fn incomplete_content_filter_preserves_reason_and_maps_completion_semantics() {
        let mut body = fixture("responses");
        body["status"] = json!("incomplete");
        body["incomplete_details"] = json!({"reason": "content_filter"});
        body["output"][0]
            .as_object_mut()
            .expect("message item is an object")
            .remove("id");
        body["usage"]["input_tokens_details"]
            .as_object_mut()
            .expect("usage details are an object")
            .remove("cache_write_tokens");

        let ir = decode_response(WireProtocol::OpenAiResponses, body)
            .expect("content-filter incomplete Responses response decodes");
        assert_eq!(
            ir.completion.finish_reason.as_deref(),
            Some("content_filter")
        );
        assert_eq!(ir.completion.stop_reason.as_deref(), Some("refusal"));

        let chat = encode_response(ir.clone(), WireProtocol::OpenAiChat)
            .expect("content-filter response encodes to Chat");
        assert_eq!(chat["choices"][0]["finish_reason"], json!("content_filter"));

        let responses = encode_response(ir, WireProtocol::OpenAiResponses)
            .expect("content-filter response encodes to Responses");
        assert_eq!(
            responses["incomplete_details"],
            json!({"reason": "content_filter"})
        );
    }

    #[test]
    fn chat_and_anthropic_content_filter_responses_preserve_incomplete_reason() {
        let mut chat = fixture("chat");
        chat["choices"][0]["finish_reason"] = json!("content_filter");
        let chat_ir = decode_response(WireProtocol::OpenAiChat, chat)
            .expect("Chat content-filter response decodes");
        assert_eq!(chat_ir.completion.status.as_deref(), Some("incomplete"));
        assert_eq!(chat_ir.completion.stop_reason.as_deref(), Some("refusal"));
        let chat_responses = encode_response(chat_ir, WireProtocol::OpenAiResponses)
            .expect("Chat content-filter response encodes to Responses");
        assert_eq!(chat_responses["status"], json!("incomplete"));
        assert_eq!(
            chat_responses["incomplete_details"],
            json!({"reason": "content_filter"})
        );

        let mut anthropic = fixture("anthropic");
        anthropic["stop_reason"] = json!("refusal");
        let anthropic_ir = decode_response(WireProtocol::AnthropicMessages, anthropic)
            .expect("Anthropic refusal response decodes");
        assert_eq!(
            anthropic_ir.completion.status.as_deref(),
            Some("incomplete")
        );
        assert_eq!(
            anthropic_ir.completion.finish_reason.as_deref(),
            Some("content_filter")
        );
        let anthropic_responses = encode_response(anthropic_ir, WireProtocol::OpenAiResponses)
            .expect("Anthropic refusal response encodes to Responses");
        assert_eq!(anthropic_responses["status"], json!("incomplete"));
        assert_eq!(
            anthropic_responses["incomplete_details"],
            json!({"reason": "content_filter"})
        );

        let ordinary = encode_response(
            decode_response(WireProtocol::OpenAiChat, fixture("chat"))
                .expect("ordinary Chat response decodes"),
            WireProtocol::OpenAiResponses,
        )
        .expect("ordinary Chat response encodes to Responses");
        assert_eq!(ordinary["status"], json!("completed"));
        assert_eq!(ordinary["incomplete_details"], Value::Null);
    }

    #[test]
    fn reasoning_item_status_and_content_are_preserved() {
        let body = json!({
            "id": "resp_reasoning_content",
            "object": "response",
            "model": "reasoning-model",
            "status": "completed",
            "output": [{
                "id": "rs_content",
                "type": "reasoning",
                "status": "completed",
                "summary": [],
                "content": [{"type": "reasoning_text", "text": "internal plan"}]
            }],
            "usage": {"input_tokens": 2, "output_tokens": 1, "total_tokens": 3}
        });

        let ir = decode_response(WireProtocol::OpenAiResponses, body.clone())
            .expect("reasoning item content decodes");
        assert!(matches!(
            ir.content.as_slice(),
            [ContentIr::Thinking { text, signature: None }] if text == "internal plan"
        ));
        let encoded = encode_response(ir, WireProtocol::OpenAiResponses)
            .expect("reasoning item content encodes");
        assert_eq!(encoded["output"][0]["status"], json!("completed"));
        assert_eq!(
            encoded["output"][0]["content"],
            body["output"][0]["content"]
        );
    }

    #[test]
    fn unknown_responses_metadata_still_fails_closed() {
        let mut body = fixture("responses");
        body["unsafe_extension"] = json!({"secret": true});
        assert!(matches!(
            decode_response(WireProtocol::OpenAiResponses, body),
            Err(BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn responses_standard_scalar_fields_are_strictly_validated() {
        for (field, value) in [
            ("background", json!("yes")),
            ("max_output_tokens", json!("many")),
            ("max_tool_calls", json!(-1)),
            ("parallel_tool_calls", json!("yes")),
            ("temperature", json!(3.0)),
            ("top_logprobs", json!(21)),
            ("top_p", json!(2.0)),
            ("service_tier", json!("unsupported")),
            ("truncation", json!("unsupported")),
        ] {
            let mut body = fixture("responses");
            body[field] = value;
            assert_eq!(
                decode_response(WireProtocol::OpenAiResponses, body),
                Err(BridgeError::InvalidUpstream)
            );
        }
    }

    #[test]
    fn incomplete_responses_status_details_and_items_round_trip() {
        let mut body = fixture("responses");
        body["status"] = json!("incomplete");
        body["incomplete_details"] = json!({"reason": "max_output_tokens"});
        body["output"][0]["status"] = json!("incomplete");

        let ir = decode_response(WireProtocol::OpenAiResponses, body)
            .expect("incomplete Responses response decodes");
        assert_eq!(ir.completion.status.as_deref(), Some("incomplete"));
        let encoded = encode_response(ir, WireProtocol::OpenAiResponses)
            .expect("incomplete Responses response encodes");
        assert_eq!(encoded["status"], json!("incomplete"));
        assert_eq!(
            encoded["incomplete_details"],
            json!({"reason": "max_output_tokens"})
        );
        assert_eq!(encoded["output"][0]["status"], json!("incomplete"));
    }

    #[test]
    fn chat_rejects_unsigned_thinking_response() {
        let body = json!({
            "id": "chat-thinking",
            "object": "chat.completion",
            "model": "reasoning-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "plan"},
                "finish_reason": "stop"
            }]
        });
        let mut ir =
            decode_response(WireProtocol::OpenAiChat, body).expect("Chat response decodes");
        ir.content = vec![ContentIr::Thinking {
            text: "plan".to_string(),
            signature: None,
        }];
        // 无签名 thinking 对 chat 上游可表达（折叠为文本），不再拒绝。
        let encoded = encode_response(ir, WireProtocol::OpenAiChat).expect("encodes");
        assert_eq!(encoded["choices"][0]["message"]["content"], "plan");
    }

    #[test]
    fn anthropic_requires_thinking_signature_and_maps_thinking_usage() {
        let mut missing_signature = fixture("anthropic");
        missing_signature["content"] = json!([{"type": "thinking", "thinking": "plan"}]);
        assert_eq!(
            decode_response(WireProtocol::AnthropicMessages, missing_signature),
            Err(BridgeError::InvalidUpstream)
        );

        let mut body = fixture("anthropic");
        body["content"] = json!([{
            "type": "thinking",
            "thinking": "plan",
            "signature": "opaque-plan"
        }]);
        body["usage"]["output_tokens_details"] = json!({"thinking_tokens": 3});
        let ir = decode_response(WireProtocol::AnthropicMessages, body)
            .expect("Anthropic reasoning response decodes");
        assert_eq!(
            ir.usage.as_ref().and_then(|usage| usage.reasoning_tokens),
            Some(3)
        );
        let encoded = encode_response(ir, WireProtocol::AnthropicMessages)
            .expect("Anthropic reasoning response encodes");
        assert_eq!(
            encoded["usage"]["output_tokens_details"]["thinking_tokens"],
            json!(3)
        );
    }

    #[test]
    fn chat_rejects_non_null_logprobs_and_nonzero_cache_writes_but_accepts_zero() {
        let mut body = fixture("chat");
        body["choices"][0]["logprobs"] = json!({"content": []});
        assert!(matches!(
            decode_response(WireProtocol::OpenAiChat, body),
            Err(BridgeError::Unsupported { .. })
        ));

        let ir = decode_response(WireProtocol::AnthropicMessages, fixture("anthropic"))
            .expect("Anthropic response decodes");
        let mut usage = ir.usage.clone().expect("fixture has usage");
        usage.cache_write_tokens = Some(0);
        let encoded = encode_response(
            ResponseIr {
                usage: Some(usage.clone()),
                ..ir.clone()
            },
            WireProtocol::OpenAiChat,
        )
        .expect("zero cache-write usage is a lossless no-op");
        assert_eq!(encoded["usage"]["prompt_tokens"], 14);
        assert_eq!(encoded["usage"]["completion_tokens"], 4);
        assert_eq!(encoded["usage"]["total_tokens"], 18);
        assert_eq!(
            encoded["usage"]["prompt_tokens_details"]["cached_tokens"],
            2
        );

        // cache_write 对 OpenAI chat 无对应字段：与流式路径一致，静默丢弃。
        usage.cache_write_tokens = Some(5);
        let encoded = encode_response(
            ResponseIr {
                usage: Some(usage),
                ..ir
            },
            WireProtocol::OpenAiChat,
        )
        .expect("nonzero cache-write is dropped, not rejected");
        assert_eq!(encoded["usage"]["prompt_tokens"], 14);
        assert_eq!(encoded["usage"]["total_tokens"], 18);
    }

    #[test]
    fn contradictory_completion_semantics_fail_closed() {
        let ir = ResponseIr {
            meta: ResponseMetaIr {
                id: Some("id".to_string()),
                model: Some("model".to_string()),
            },
            content: vec![ContentIr::Text("text".to_string())],
            usage: None,
            completion: CompletionIr {
                finish_reason: Some("stop".to_string()),
                stop_reason: Some("max_tokens".to_string()),
                status: Some("incomplete".to_string()),
                error: None,
            },
            extensions: BTreeMap::new(),
        };
        assert_eq!(
            encode_response(ir, WireProtocol::OpenAiChat),
            Err(BridgeError::InvalidUpstream)
        );
    }
}
