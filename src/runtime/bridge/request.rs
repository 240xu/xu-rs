#![allow(clippy::items_after_test_module)]

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::super::{encode_upstream_request, parse_request, ContentIr, RoleIr, WireProtocol};

    fn fixture(path: &str) -> Value {
        serde_json::from_str(match path {
            "anthropic" => {
                include_str!("../../../tests/fixtures/runtime_bridge/anthropic_text_request.json")
            }
            "anthropic_clean" => {
                include_str!("../../../tests/fixtures/runtime_bridge/anthropic_clean_request.json")
            }
            "chat" => include_str!("../../../tests/fixtures/runtime_bridge/chat_tool_request.json"),
            "chat_clean" => {
                include_str!("../../../tests/fixtures/runtime_bridge/chat_clean_request.json")
            }
            "responses" => include_str!(
                "../../../tests/fixtures/runtime_bridge/responses_reasoning_request.json"
            ),
            "responses_compatible" => include_str!(
                "../../../tests/fixtures/runtime_bridge/responses_compatible_request.json"
            ),
            _ => panic!("unknown fixture"),
        })
        .expect("fixture is valid JSON")
    }

    #[test]
    fn anthropic_request_encodes_to_chat_with_head_system_message() {
        let ir = parse_request(WireProtocol::AnthropicMessages, &fixture("anthropic_clean"))
            .expect("Anthropic fixture parses");

        assert_eq!(ir.model, "fixture-anthropic-model");
        assert_eq!(ir.messages[0].role, RoleIr::System);
        assert_eq!(ir.messages[1].role, RoleIr::User);
        assert_eq!(ir.generation.max_tokens, Some(96));
        assert_eq!(ir.generation.temperature, Some(0.2));
        assert!(!ir.stream);

        let encoded = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-upstream")
            .expect("Anthropic request encodes to Chat");
        assert_eq!(encoded["model"], json!("chat-upstream"));
        assert_eq!(
            encoded["messages"][0],
            json!({
                "role": "system",
                "content": "fixture system instruction"
            })
        );
        assert_eq!(
            encoded["messages"][1]["content"],
            json!("fixture text request")
        );
        assert_eq!(encoded["max_tokens"], json!(96));
        assert_eq!(encoded["temperature"], json!(0.2));
    }

    #[test]
    fn chat_request_encodes_to_anthropic_with_tool_results_as_user_blocks() {
        let ir = parse_request(WireProtocol::OpenAiChat, &fixture("chat_clean"))
            .expect("Chat fixture parses");

        assert_eq!(ir.messages[0].role, RoleIr::System);
        assert_eq!(ir.messages[3].role, RoleIr::Assistant);
        assert!(matches!(
            ir.messages[3].content[0],
            ContentIr::ToolUse(ref call) if call.call_id == "call_fixture_weather"
        ));
        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.generation.reasoning_effort.as_deref(), Some("medium"));

        let encoded =
            encode_upstream_request(&ir, WireProtocol::AnthropicMessages, "anthropic-upstream")
                .expect("Chat request encodes to Anthropic");
        assert_eq!(encoded["model"], json!("anthropic-upstream"));
        assert_eq!(
            encoded["system"],
            json!("fixture system instruction\n\nfixture developer instruction")
        );
        assert_eq!(encoded["tools"].as_array().expect("tools array").len(), 1);
        assert_eq!(encoded["tool_choice"], json!({ "type": "auto" }));
        assert_eq!(encoded["max_tokens"], json!(128));
        assert_eq!(
            encoded["messages"][1]["content"][0]["type"],
            json!("tool_use")
        );
        assert_eq!(
            encoded["messages"][2]["content"][0]["type"],
            json!("tool_result")
        );
        assert_eq!(
            encoded["messages"][2]["content"][0]["tool_use_id"],
            json!("call_fixture_weather")
        );
    }

    #[test]
    fn anthropic_request_encodes_to_responses_with_instruction_and_generation_fields() {
        let ir = parse_request(WireProtocol::AnthropicMessages, &fixture("anthropic_clean"))
            .expect("Anthropic fixture parses");
        let encoded =
            encode_upstream_request(&ir, WireProtocol::OpenAiResponses, "responses-upstream")
                .expect("Anthropic request encodes to Responses");

        assert_eq!(encoded["model"], json!("responses-upstream"));
        assert_eq!(encoded["instructions"], json!("fixture system instruction"));
        assert_eq!(encoded["input"][0]["role"], json!("user"));
        assert_eq!(
            encoded["input"][0]["content"][0]["type"],
            json!("input_text")
        );
        assert_eq!(encoded["max_output_tokens"], json!(96));
        assert_eq!(encoded["temperature"], json!(0.2));
    }

    #[test]
    fn chat_request_encodes_to_responses_with_tool_item_ids_and_parameters() {
        let ir =
            parse_request(WireProtocol::OpenAiChat, &fixture("chat")).expect("Chat fixture parses");
        let encoded =
            encode_upstream_request(&ir, WireProtocol::OpenAiResponses, "responses-upstream")
                .expect("Chat request encodes to Responses");

        assert_eq!(encoded["model"], json!("responses-upstream"));
        assert_eq!(encoded["input"][2]["type"], json!("function_call"));
        assert_eq!(
            encoded["input"][2]["call_id"],
            json!("call_fixture_weather")
        );
        assert_eq!(encoded["input"][2]["name"], json!("get_fixture_weather"));
        assert_eq!(
            encoded["input"][2]["arguments"],
            json!(r#"{"city":"fixture-city"}"#)
        );
        assert_eq!(encoded["input"][4]["type"], json!("function_call_output"));
        assert_eq!(
            encoded["input"][4]["call_id"],
            json!("call_fixture_weather")
        );
        assert_eq!(encoded["reasoning"]["effort"], json!("medium"));
        assert_eq!(encoded["max_output_tokens"], json!(128));
    }

    #[test]
    fn responses_request_encodes_to_anthropic_with_distinct_instruction_and_tool_blocks() {
        let ir = parse_request(
            WireProtocol::OpenAiResponses,
            &fixture("responses_compatible"),
        )
        .expect("Responses fixture parses");

        assert_eq!(ir.messages[0].role, RoleIr::System);
        assert_eq!(ir.messages[1].role, RoleIr::Developer);
        assert_eq!(ir.messages[2].role, RoleIr::User);
        assert_eq!(ir.generation.max_tokens, Some(160));
        assert!(matches!(
            ir.messages[3].content[0],
            ContentIr::ToolUse(ref call) if call.call_id == "call_fixture_weather"
        ));

        let encoded =
            encode_upstream_request(&ir, WireProtocol::AnthropicMessages, "anthropic-upstream")
                .expect("Responses request encodes to Anthropic");
        assert_eq!(encoded["model"], json!("anthropic-upstream"));
        assert_eq!(
            encoded["system"],
            json!("fixture responses instruction\n\nfixture developer instruction")
        );
        assert_eq!(encoded["messages"][0]["role"], json!("user"));
        assert_eq!(encoded["messages"][1]["role"], json!("assistant"));
        assert_eq!(
            encoded["messages"][1]["content"][0]["type"],
            json!("tool_use")
        );
        assert_eq!(
            encoded["messages"][2]["content"][0]["tool_use_id"],
            json!("call_fixture_weather")
        );
        assert_eq!(encoded["max_tokens"], json!(160));
        assert_eq!(encoded["temperature"], json!(0.3));
        assert_eq!(encoded["stop_sequences"], json!(["STOP"]));
    }

    #[test]
    fn responses_request_with_opaque_reasoning_fails_when_encoding_to_chat() {
        let ir = parse_request(WireProtocol::OpenAiResponses, &fixture("responses"))
            .expect("Responses fixture parses");

        let error = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-upstream")
            .expect_err("Chat cannot preserve opaque Responses reasoning");
        assert!(matches!(
            error,
            super::super::BridgeError::Unsupported { .. }
        ));
    }

    fn assert_unsupported_field(error: super::super::BridgeError, field: &str) {
        match error {
            super::super::BridgeError::Unsupported { field: actual } => {
                assert_eq!(actual, field)
            }
            other => panic!("expected unsupported field, got {other:?}"),
        }
    }

    #[test]
    fn all_six_fixture_directions_cover_common_request_fields() {
        let anthropic = parse_request(WireProtocol::AnthropicMessages, &fixture("anthropic_clean"))
            .expect("clean Anthropic fixture parses");
        let chat = parse_request(WireProtocol::OpenAiChat, &fixture("chat_clean"))
            .expect("Chat fixture parses");
        let responses = parse_request(
            WireProtocol::OpenAiResponses,
            &fixture("responses_compatible"),
        )
        .expect("compatible Responses fixture parses");

        let directions = [
            (&anthropic, WireProtocol::OpenAiChat, "chat-upstream"),
            (
                &anthropic,
                WireProtocol::OpenAiResponses,
                "responses-upstream",
            ),
            (&chat, WireProtocol::AnthropicMessages, "anthropic-upstream"),
            (&chat, WireProtocol::OpenAiResponses, "responses-upstream"),
            (
                &responses,
                WireProtocol::AnthropicMessages,
                "anthropic-upstream",
            ),
            (&responses, WireProtocol::OpenAiChat, "chat-upstream"),
        ];
        for (ir, target, model) in directions {
            let encoded = encode_upstream_request(ir, target, model)
                .expect("compatible fixture direction encodes");
            assert_eq!(encoded["model"], json!(model));
            assert_eq!(encoded["stream"], json!(ir.stream));
            assert!(!encoded["tools"].is_null());
            assert!(!encoded["tool_choice"].is_null());
        }

        let anthropic_to_chat =
            encode_upstream_request(&anthropic, WireProtocol::OpenAiChat, "chat-upstream")
                .expect("Anthropic to Chat encodes");
        assert_eq!(anthropic_to_chat["messages"][0]["role"], json!("system"));
        assert_eq!(anthropic_to_chat["max_tokens"], json!(96));
        assert_eq!(anthropic_to_chat["temperature"], json!(0.2));
        assert_eq!(anthropic_to_chat["top_p"], json!(0.8));
        assert_eq!(anthropic_to_chat["stop"], json!(["STOP"]));

        let chat_to_responses =
            encode_upstream_request(&chat, WireProtocol::OpenAiResponses, "responses-upstream")
                .expect("Chat to Responses encodes");
        assert_eq!(
            chat_to_responses["instructions"],
            json!("fixture system instruction")
        );
        assert_eq!(chat_to_responses["input"][0]["role"], json!("developer"));
        assert_eq!(chat_to_responses["max_output_tokens"], json!(128));
        assert_eq!(chat_to_responses["reasoning"]["effort"], json!("medium"));

        let responses_to_chat =
            encode_upstream_request(&responses, WireProtocol::OpenAiChat, "chat-upstream")
                .expect("Responses to Chat encodes");
        assert_eq!(responses_to_chat["messages"][0]["role"], json!("system"));
        assert_eq!(responses_to_chat["messages"][1]["role"], json!("developer"));
        assert_eq!(responses_to_chat["max_tokens"], json!(160));
        assert_eq!(responses_to_chat["temperature"], json!(0.3));
        assert_eq!(responses_to_chat["stop"], json!(["STOP"]));
    }

    #[test]
    fn unsupported_nested_and_target_specific_fields_fail_closed() {
        let mut anthropic = fixture("anthropic");
        parse_request(WireProtocol::AnthropicMessages, &anthropic)
            .expect("ephemeral cache_control is a supported client hint");
        anthropic["system"][0]["cache_control"]["type"] = json!("unknown");
        let error = parse_request(WireProtocol::AnthropicMessages, &anthropic)
            .expect_err("unknown cache_control types remain unsupported");
        assert_unsupported_field(error, "cache_control.type");

        let chat =
            parse_request(WireProtocol::OpenAiChat, &fixture("chat")).expect("Chat fixture parses");
        let error =
            encode_upstream_request(&chat, WireProtocol::AnthropicMessages, "anthropic-upstream")
                .expect_err("parallel_tool_calls has no Anthropic mapping");
        assert_unsupported_field(error, "parallel_tool_calls");

        let thinking = json!({
            "model": "fixture",
            "thinking": {"type": "enabled", "budget_tokens": 64},
            "messages": [{"role": "user", "content": "fixture"}]
        });
        let ir = parse_request(WireProtocol::AnthropicMessages, &thinking)
            .expect("Anthropic thinking request parses");
        let error = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-upstream")
            .expect_err("Anthropic thinking extension has no Chat mapping");
        assert_unsupported_field(error, "anthropic.thinking");
    }

    #[test]
    fn anthropic_accepts_observed_claude_code_control_fields_for_conversion() {
        let body = json!({
            "model": "fixture",
            "system": [{
                "type": "text",
                "text": "system",
                "cache_control": {"type": "ephemeral"}
            }],
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "fixture",
                    "cache_control": {"type": "ephemeral"}
                }]
            }],
            "context_management": {
                "edits": [
                    {"type": "clear_thinking_20251015", "keep": "all"},
                    {"type": "compaction", "keep": "all", "turn": "12"}
                ]
            },
            "output_config": {"effort": "high"},
            "thinking": {"type": "adaptive", "display": "omitted"},
            "max_tokens": 64,
            "stream": false
        });

        let ir = parse_request(WireProtocol::AnthropicMessages, &body)
            .expect("observed Claude Code request parses");

        assert_eq!(ir.generation.reasoning_effort.as_deref(), Some("high"));
        let encoded = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-upstream")
            .expect("adaptive Claude Code thinking encodes to Chat");
        assert_eq!(encoded["reasoning_effort"], json!("high"));
    }

    #[test]
    fn responses_function_call_output_item_accepts_item_id() {
        let body = json!({
            "model": "fixture",
            "input": [
                {"type": "function_call", "call_id": "call_1", "name": "grep",
                 "arguments": "{\"q\": \"x\"}"},
                {"type": "function_call_output", "id": "fco_001",
                 "call_id": "call_1", "output": "result text"}
            ]
        });
        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("function_call_output with id must parse");
        assert!(ir.messages.iter().any(|m| matches!(m.role, RoleIr::Tool)));
    }

    #[test]
    fn responses_accepts_observed_codex_request_metadata_and_strict_tools() {
        let body = json!({
            "model": "fixture",
            "instructions": "system",
            "input": "fixture",
            "tools": [{
                "type": "function",
                "name": "shell",
                "description": "read-only shell",
                "strict": true,
                "parameters": {"type": "object"}
            }],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": {"effort": "medium"},
            "store": false,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "fixture-key",
            "client_metadata": {"turn_id": "fixture-turn"},
            "stream": false
        });

        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("observed Codex request parses");

        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.generation.reasoning_effort.as_deref(), Some("medium"));
    }

    #[test]
    fn responses_drops_known_client_owned_tool_types_for_provider_conversion() {
        let body = json!({
            "model": "fixture",
            "input": "fixture",
            "tools": [
                {
                    "type": "function",
                    "name": "shell",
                    "parameters": {"type": "object"}
                },
                {
                    "type": "custom",
                    "name": "apply_patch",
                    "parameters": {"type": "object"}
                },
                {"type": "namespace", "name": "multi_agent_v1"},
                {"type": "web_search"}
            ]
        });

        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("client-owned namespace/web_search tools should be ignored");
        assert_eq!(ir.tools.len(), 2);
        assert_eq!(ir.tools[0].name, "shell");
        assert_eq!(ir.tools[1].name, "apply_patch");

        let encoded = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-upstream")
            .expect("custom tools encode to Chat function shape");
        let tools = encoded["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().all(|tool| tool["type"] == json!("function")));
        assert!(tools.iter().all(|tool| tool["type"] != json!("custom")));
        assert_eq!(tools[1]["function"]["name"], json!("apply_patch"));
    }

    #[test]
    fn responses_rejects_unknown_tool_types() {
        let body = json!({
            "model": "fixture",
            "input": "fixture",
            "tools": [{"type": "unsupported_fixture_tool"}]
        });

        let error = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect_err("unknown tool types must remain fail-closed");
        assert_unsupported_field(error, "tools.type");
    }

    #[test]
    fn responses_drops_client_item_ids_when_encoding_to_chat() {
        let body = json!({
            "model": "fixture",
            "input": [{
                "id": "msg_fixture",
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "fixture"}]
            }]
        });
        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("Responses message with item ID parses");

        let encoded = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-upstream")
            .expect("Chat conversion drops client-owned item IDs");
        assert_eq!(
            encoded["messages"][0],
            json!({"role": "user", "content": "fixture"})
        );
    }

    #[test]
    fn request_parsers_bound_non_streaming_tool_arguments() {
        let large_value = "x".repeat(super::super::DEFAULT_MAX_TOOL_ARGUMENT_BYTES + 1);
        let arguments = serde_json::to_string(&json!({"value": large_value}))
            .expect("large arguments serialize");
        let anthropic_arguments: Value =
            serde_json::from_str(&arguments).expect("large arguments remain an object");
        let cases = [
            (
                WireProtocol::AnthropicMessages,
                json!({
                    "model": "fixture",
                    "messages": [{
                        "role": "assistant",
                        "content": [{
                            "type": "tool_use", "id": "call", "name": "tool",
                            "input": anthropic_arguments
                        }]
                    }]
                }),
            ),
            (
                WireProtocol::OpenAiChat,
                json!({
                    "model": "fixture",
                    "messages": [{
                        "role": "assistant", "content": null,
                        "tool_calls": [{
                            "id": "call", "type": "function",
                            "function": {"name": "tool", "arguments": arguments.clone()}
                        }]
                    }]
                }),
            ),
            (
                WireProtocol::OpenAiResponses,
                json!({
                    "model": "fixture",
                    "input": [{
                        "type": "function_call", "call_id": "call", "name": "tool",
                        "arguments": arguments
                    }]
                }),
            ),
        ];
        for (protocol, body) in cases {
            assert_eq!(
                parse_request(protocol, &body),
                Err(super::super::BridgeError::ResourceLimit)
            );
        }
    }

    #[test]
    fn request_parsers_reject_orphan_mismatched_and_out_of_order_tool_results() {
        let cases = [
            (
                WireProtocol::AnthropicMessages,
                json!({
                    "model": "fixture",
                    "messages": [{
                        "role": "user",
                        "content": [{"type": "tool_result", "tool_use_id": "missing", "content": "fixture"}]
                    }]
                }),
            ),
            (
                WireProtocol::OpenAiChat,
                json!({
                    "model": "fixture",
                    "messages": [
                        {"role": "assistant", "content": null, "tool_calls": [
                            {"id": "call-one", "type": "function", "function": {"name": "one", "arguments": "{}"}},
                            {"id": "call-two", "type": "function", "function": {"name": "two", "arguments": "{}"}}
                        ]},
                        {"role": "tool", "tool_call_id": "call-two", "content": "fixture"},
                        {"role": "tool", "tool_call_id": "call-two", "content": "duplicate result"}
                    ]
                }),
            ),
            (
                WireProtocol::OpenAiResponses,
                json!({
                    "model": "fixture",
                    "input": [{"type": "function_call_output", "call_id": "missing", "output": "fixture"}]
                }),
            ),
        ];
        for (protocol, body) in cases {
            assert_eq!(
                parse_request(protocol, &body),
                Err(super::super::BridgeError::ToolState)
            );
        }
    }

    #[test]
    fn responses_accept_multiple_tool_turns_with_per_turn_indexes() {
        let body = json!({
            "model": "responses-model",
            "input": [
                {
                    "id": "item-1",
                    "type": "function_call",
                    "call_id": "call-1",
                    "name": "first",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call-1",
                    "output": "first result"
                },
                {
                    "id": "item-2",
                    "type": "function_call",
                    "call_id": "call-2",
                    "name": "second",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call-2",
                    "output": "second result"
                }
            ]
        });

        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("multiple Responses tool turns parse");
        let calls = ir
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|content| match content {
                ContentIr::ToolUse(call) => Some(call),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].index, 0);
        assert_eq!(calls[0].call_id, "call-1");
        assert_eq!(calls[0].item_id.as_deref(), Some("item-1"));
        assert_eq!(calls[1].index, 0);
        assert_eq!(calls[1].call_id, "call-2");
        assert_eq!(calls[1].item_id.as_deref(), Some("item-2"));
    }

    #[test]
    fn responses_item_ids_are_rejected_when_target_cannot_represent_them() {
        let ir = parse_request(WireProtocol::OpenAiResponses, &fixture("responses"))
            .expect("Responses fixture parses");
        let error =
            encode_upstream_request(&ir, WireProtocol::AnthropicMessages, "anthropic-upstream")
                .expect_err("Responses item IDs cannot be represented by Anthropic");
        assert_unsupported_field(error, "responses.item_id");
    }

    #[test]
    fn responses_reasoning_item_status_and_content_round_trip() {
        let body = json!({
            "model": "reasoning-model",
            "input": [{
                "type": "reasoning",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "summary"}],
                "content": [{"type": "reasoning_text", "text": "full reasoning"}],
                "encrypted_content": "opaque-reasoning"
            }]
        });
        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("Responses reasoning request parses");
        let encoded =
            encode_upstream_request(&ir, WireProtocol::OpenAiResponses, "responses-model")
                .expect("Responses reasoning request round-trips");
        assert_eq!(encoded["input"][0]["status"], json!("completed"));
        assert_eq!(encoded["input"][0]["content"], body["input"][0]["content"]);

        let error = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-model")
            .expect_err("Chat cannot preserve Responses reasoning metadata");
        assert_unsupported_field(error, "responses.reasoning_item_metadata");
    }

    #[test]
    fn opaque_responses_reasoning_round_trips_through_anthropic_envelope() {
        let body = json!({
            "model": "reasoning-model",
            "input": [{
                "id": "rs_opaque",
                "type": "reasoning",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "visible summary"}],
                "content": [{"type": "reasoning_text", "text": "private trace"}],
                "encrypted_content": "opaque payload"
            }]
        });
        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("Responses reasoning parses");
        let anthropic =
            encode_upstream_request(&ir, WireProtocol::AnthropicMessages, "anthropic-model")
                .expect("opaque reasoning maps to Anthropic");
        let block = &anthropic["messages"][0]["content"][0];
        let signature = block["signature"]
            .as_str()
            .expect("opaque reasoning uses a signature");
        assert_eq!(block["type"], json!("thinking"));
        assert_eq!(block["thinking"], json!("visible summary"));
        assert!(signature.starts_with("ccswitch-openai-reasoning-v1:"));
        assert!(!signature.contains('='));
        assert_ne!(signature, "opaque payload");

        let replay = parse_request(WireProtocol::AnthropicMessages, &anthropic)
            .expect("enveloped thinking parses");
        let recovered =
            encode_upstream_request(&replay, WireProtocol::OpenAiResponses, "responses-model")
                .expect("enveloped thinking recovers");
        assert_eq!(recovered["input"][0], body["input"][0]);
    }

    #[test]
    fn encrypted_only_responses_reasoning_uses_redacted_thinking_and_round_trips() {
        let body = json!({
            "model": "reasoning-model",
            "input": [{
                "id": "rs_redacted",
                "type": "reasoning",
                "summary": [],
                "encrypted_content": "opaque payload"
            }]
        });
        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("encrypted-only reasoning parses");
        let anthropic =
            encode_upstream_request(&ir, WireProtocol::AnthropicMessages, "anthropic-model")
                .expect("encrypted-only reasoning maps to Anthropic");
        let block = &anthropic["messages"][0]["content"][0];
        assert_eq!(block["type"], json!("redacted_thinking"));
        assert!(block["data"]
            .as_str()
            .is_some_and(|value| value.starts_with("ccswitch-openai-reasoning-v1:")));

        let replay = parse_request(WireProtocol::AnthropicMessages, &anthropic)
            .expect("redacted thinking parses");
        let recovered =
            encode_upstream_request(&replay, WireProtocol::OpenAiResponses, "responses-model")
                .expect("redacted thinking recovers");
        assert_eq!(recovered["input"][0], body["input"][0]);
    }

    #[test]
    fn visible_only_responses_reasoning_stays_unsigned() {
        let body = json!({
            "model": "reasoning-model",
            "input": [{
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "visible only"}]
            }]
        });
        let ir = parse_request(WireProtocol::OpenAiResponses, &body)
            .expect("visible-only reasoning parses");
        let anthropic =
            encode_upstream_request(&ir, WireProtocol::AnthropicMessages, "anthropic-model")
                .expect("visible-only reasoning maps to Anthropic");
        let block = &anthropic["messages"][0]["content"][0];
        assert_eq!(block["type"], json!("thinking"));
        assert_eq!(block["thinking"], json!("visible only"));
        assert!(block.get("signature").is_none());
    }

    #[test]
    fn invalid_anthropic_reasoning_signature_is_preserved_as_opaque_data() {
        let body = json!({
            "model": "anthropic-model",
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "thinking",
                    "thinking": "visible summary",
                    "signature": "ccswitch-openai-reasoning-v1:not-valid"
                }]
            }]
        });
        let ir = parse_request(WireProtocol::AnthropicMessages, &body)
            .expect("arbitrary signature remains parseable");
        let responses =
            encode_upstream_request(&ir, WireProtocol::OpenAiResponses, "responses-model")
                .expect("opaque signature remains representable");
        assert_eq!(responses["input"][0]["type"], json!("reasoning"));
        assert_eq!(
            responses["input"][0]["encrypted_content"],
            json!("ccswitch-openai-reasoning-v1:not-valid")
        );
        assert_eq!(
            responses["input"][0]["summary"][0]["text"],
            json!("visible summary")
        );
    }

    #[test]
    fn media_urls_are_validated_and_data_urls_map_to_native_sources() {
        let data_url = "data:image/png;base64,aGVsbG8";
        let chat = json!({
            "model": "chat-model",
            "messages": [{
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": data_url}}]
            }]
        });
        let ir = parse_request(WireProtocol::OpenAiChat, &chat).expect("data image parses");
        let anthropic =
            encode_upstream_request(&ir, WireProtocol::AnthropicMessages, "anthropic-model")
                .expect("data image maps to Anthropic");
        assert_eq!(
            anthropic["messages"][0]["content"][0]["source"],
            json!({"type": "base64", "media_type": "image/png", "data": "aGVsbG8"})
        );

        let remote = json!({
            "model": "chat-model",
            "messages": [{
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": "https://cdn.example/image.png"}}]
            }]
        });
        parse_request(WireProtocol::OpenAiChat, &remote).expect("remote image parses");

        let local = json!({
            "model": "chat-model",
            "messages": [{
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": "/tmp/image.png"}}]
            }]
        });
        assert!(matches!(
            parse_request(WireProtocol::OpenAiChat, &local),
            Err(super::super::BridgeError::Unsupported { .. })
        ));

        let malformed = json!({
            "model": "chat-model",
            "messages": [{
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,not valid"}}]
            }]
        });
        assert!(matches!(
            parse_request(WireProtocol::OpenAiChat, &malformed),
            Err(super::super::BridgeError::Unsupported { .. })
        ));

        let document = json!({
            "model": "anthropic-model",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "document",
                    "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERg"}
                }]
            }]
        });
        let document_ir =
            parse_request(WireProtocol::AnthropicMessages, &document).expect("document parses");
        let responses = encode_upstream_request(
            &document_ir,
            WireProtocol::OpenAiResponses,
            "responses-model",
        )
        .expect("document maps to Responses");
        assert_eq!(
            responses["input"][0]["content"][0],
            json!({
                "type": "input_file",
                "file_data": "data:application/pdf;base64,JVBERg"
            })
        );

        let unknown = json!({
            "model": "chat-model",
            "messages": [{
                "role": "user",
                "content": [{"type": "audio", "data": "JVBERg"}]
            }]
        });
        assert!(matches!(
            parse_request(WireProtocol::OpenAiChat, &unknown),
            Err(super::super::BridgeError::Unsupported { .. })
        ));

        let malformed_source = json!({
            "model": "anthropic-model",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": {"type": "base64", "data": "aGVsbG8"}
                }]
            }]
        });
        assert!(matches!(
            parse_request(WireProtocol::AnthropicMessages, &malformed_source),
            Err(super::super::BridgeError::Unsupported { .. })
        ));

        for host in [
            "[::ffff:127.0.0.1]",
            "[::ffff:10.0.0.1]",
            "[::ffff:169.254.1.1]",
            "[::ffff:0.0.0.0]",
            "[::ffff:224.0.0.1]",
        ] {
            let mapped_ipv6 = json!({
                "model": "chat-model",
                "messages": [{
                    "role": "user",
                    "content": [{"type": "image_url", "image_url": {"url": format!("http://{host}/image.png")}}]
                }]
            });
            assert!(
                matches!(
                    parse_request(WireProtocol::OpenAiChat, &mapped_ipv6),
                    Err(super::super::BridgeError::Unsupported { .. })
                ),
                "mapped local IPv6 address must be rejected: {host}"
            );
        }

        for url in [
            "http://localhost./image.png",
            "http://localhost.localdomain/image.png",
            "http://127.0.0.1./image.png",
            "http://image.local/image.png",
            "http://host.docker.internal/image.png",
            "http://ip6-loopback./image.png",
        ] {
            let local_alias = json!({
                "model": "chat-model",
                "messages": [{
                    "role": "user",
                    "content": [{"type": "image_url", "image_url": {"url": url}}]
                }]
            });
            assert!(
                matches!(
                    parse_request(WireProtocol::OpenAiChat, &local_alias),
                    Err(super::super::BridgeError::Unsupported { .. })
                ),
                "local alias must be rejected: {url}"
            );
        }
    }

    #[test]
    fn chat_request_accepts_stream_options_service_tier_strict_and_thinking() {
        let body = json!({
            "model": "fixture",
            "stream": true,
            "stream_options": {"include_usage": true},
            "service_tier": "standard",
            "thinking": {"type": "enabled"},
            "messages": [{"role": "user", "content": "fixture"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "shell",
                    "description": "fixture",
                    "strict": true,
                    "parameters": {"type": "object"}
                }
            }]
        });
        let ir =
            parse_request(WireProtocol::OpenAiChat, &body).expect("modern Chat request parses");
        assert_eq!(ir.generation.reasoning_effort.as_deref(), Some("high"));
        assert!(ir.extensions.contains_key(super::EXT_CHAT_STREAM_OPTIONS));
        assert!(ir.extensions.contains_key(super::EXT_CHAT_SERVICE_TIER));
        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.tools[0].name, "shell");

        let encoded = encode_upstream_request(&ir, WireProtocol::OpenAiChat, "chat-upstream")
            .expect("modern Chat request encodes");
        assert_eq!(encoded["reasoning_effort"], json!("high"));
        assert_eq!(
            encoded["tools"][0]["function"].get("strict"),
            Some(&json!(true)),
            "tool strict must survive chat→chat round trip"
        );
    }

    #[test]
    fn chat_request_thinking_disabled_is_accepted_without_effort_mapping() {
        let body = json!({
            "model": "fixture",
            "thinking": {"type": "disabled"},
            "messages": [{"role": "user", "content": "fixture"}]
        });
        let ir = parse_request(WireProtocol::OpenAiChat, &body)
            .expect("thinking disabled Chat request parses");
        assert!(ir.generation.reasoning_effort.is_none());
        assert!(ir.extensions.contains_key(super::EXT_CHAT_THINKING));
    }

    #[test]
    fn assistant_text_cannot_interrupt_unresolved_tool_calls() {
        let body = json!({
            "model": "chat-model",
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-one", "type": "function",
                    "function": {"name": "one", "arguments": "{}"}
                }]},
                {"role": "assistant", "content": "inserted text"},
                {"role": "tool", "tool_call_id": "call-one", "content": "result"}
            ]
        });
        assert_eq!(
            parse_request(WireProtocol::OpenAiChat, &body),
            Err(super::super::BridgeError::ToolState)
        );
    }
}
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Map, Value};

use super::ir::{
    BridgeError, ContentIr, GenerationIr, RequestIr, ToolCallIr, ToolDefinitionIr, ToolResultIr,
    WireProtocol,
};

pub(super) const DEFAULT_ANTHROPIC_MAX_TOKENS: u32 = 4096;
pub(super) const EXT_PROVIDER_MAX_OUTPUT_TOKENS: &str = "runtime.provider.max_output_tokens";
pub(crate) const EXT_PROVIDER_CACHE_MODE: &str = "runtime.provider.cache_mode";
pub(super) const EXT_ANTHROPIC_THINKING: &str = "anthropic.v1.thinking";
pub(super) const EXT_CHAT_PARALLEL_TOOL_CALLS: &str = "openai_chat.v1.parallel_tool_calls";
pub(super) const EXT_CHAT_STREAM_OPTIONS: &str = "openai_chat.v1.stream_options";
pub(super) const EXT_CHAT_SERVICE_TIER: &str = "openai_chat.v1.service_tier";
pub(super) const EXT_CHAT_TOOL_STRICT_LIST: &str = "openai_chat.v1.tool_strict_list";
pub(super) const EXT_CHAT_THINKING: &str = "openai_chat.v1.thinking";
pub(super) const EXT_RESPONSES_PARALLEL_TOOL_CALLS: &str =
    "openai_responses.v1.parallel_tool_calls";
pub(super) const EXT_RESPONSES_PREVIOUS_RESPONSE_ID: &str =
    "openai_responses.v1.previous_response_id";
pub(super) const EXT_RESPONSES_REASONING_IDS: &str = "openai_responses.v1.reasoning_item_ids";
pub(super) const EXT_RESPONSES_REASONING_ITEM_METADATA: &str =
    "openai_responses.v1.reasoning_item_metadata";
pub(super) const EXT_RESPONSES_STORE: &str = "openai_responses.v1.store";
pub(super) const EXT_RESPONSES_INCLUDE: &str = "openai_responses.v1.include";
pub(super) const EXT_RESPONSES_PROMPT_CACHE_KEY: &str = "openai_responses.v1.prompt_cache_key";
pub(super) const EXT_RESPONSES_CLIENT_METADATA: &str = "openai_responses.v1.client_metadata";
pub(super) const EXT_RESPONSES_SERVICE_TIER: &str = "openai_responses.v1.service_tier";
pub(super) const EXT_RESPONSES_TEXT: &str = "openai_responses.v1.text";
pub(super) const EXT_RESPONSES_STREAM_OPTIONS: &str = "openai_responses.v1.stream_options";
pub(super) const EXT_RESPONSES_REQUEST_CONVERSATION: &str =
    "openai_responses.v1.request_conversation";
pub(super) const EXT_RESPONSES_CONTEXT_MANAGEMENT: &str = "openai_responses.v1.context_management";

pub fn parse_request(protocol: WireProtocol, body: &Value) -> Result<RequestIr, BridgeError> {
    let ir = match protocol {
        WireProtocol::AnthropicMessages => super::anthropic::parse_request(body),
        WireProtocol::OpenAiChat => parse_chat_request(body),
        WireProtocol::OpenAiResponses => super::responses::parse_request(body),
    }?;
    validate_tool_state(&ir.messages)?;
    Ok(ir)
}

/// Chat requests may carry modern OpenAI client fields that `chat.rs` does not
/// yet whitelist: `stream_options` (Roo Code always sends it), `service_tier`,
/// `strict` inside tool function schemas, and DeepSeek-style `thinking`. These
/// are sanitized out of the delegated body so the strict `chat.rs` whitelist
/// never rejects them; expressible semantics map into the IR (`thinking
/// enabled` -> `reasoning_effort: high`), the rest are preserved verbatim in
/// `ir.extensions`.
fn parse_chat_request(body: &Value) -> Result<RequestIr, BridgeError> {
    let mut sanitized = body.clone();
    let mut extensions = BTreeMap::new();
    if let Some(object) = sanitized.as_object_mut() {
        if let Some(value) = object.remove("stream_options") {
            if !value.is_object() {
                return Err(BridgeError::InvalidRequest);
            }
            extensions.insert(EXT_CHAT_STREAM_OPTIONS.to_string(), value);
        }
        if let Some(value) = object.remove("service_tier") {
            if !value.is_string() {
                return Err(BridgeError::InvalidRequest);
            }
            extensions.insert(EXT_CHAT_SERVICE_TIER.to_string(), value);
        }
        if let Some(value) = object.remove("thinking") {
            let thinking = value.as_object().ok_or(BridgeError::InvalidRequest)?;
            if thinking.get("type").and_then(Value::as_str) == Some("enabled")
                && !object.contains_key("reasoning_effort")
            {
                object.insert(
                    "reasoning_effort".to_string(),
                    Value::String("high".to_string()),
                );
            }
            extensions.insert(EXT_CHAT_THINKING.to_string(), value);
        }
        if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
            // 逐工具保留 strict（含显式 false），编码侧按位回填；
            // 单一 any-bool 会把“部分工具 strict”语义抹平。
            let mut strict_list: Vec<Value> = Vec::new();
            for tool in tools {
                let mut strict_value = Value::Null;
                if let Some(function) = tool.get_mut("function") {
                    if let Some(function) = function.as_object_mut() {
                        if let Some(strict) = function.remove("strict") {
                            if !strict.is_boolean() {
                                return Err(BridgeError::InvalidRequest);
                            }
                            strict_value = strict;
                        }
                    }
                }
                strict_list.push(strict_value);
            }
            if strict_list.iter().any(|value| !value.is_null()) {
                extensions.insert(
                    EXT_CHAT_TOOL_STRICT_LIST.to_string(),
                    Value::Array(strict_list),
                );
            }
        }
    }
    let mut ir = super::chat::parse_request(&sanitized)?;
    ir.extensions.extend(extensions);
    Ok(ir)
}

pub fn encode_upstream_request(
    ir: &RequestIr,
    target: WireProtocol,
    model: &str,
) -> Result<Value, BridgeError> {
    if model.trim().is_empty() {
        return Err(BridgeError::InvalidRequest);
    }
    validate_tool_state(&ir.messages)?;
    validate_target_representability(ir, target)?;
    match target {
        WireProtocol::AnthropicMessages => super::anthropic::encode_request(ir, model),
        WireProtocol::OpenAiChat => super::chat::encode_request(ir, model),
        WireProtocol::OpenAiResponses => super::responses::encode_request(ir, model),
    }
}

pub(super) fn object(body: &Value) -> Result<&Map<String, Value>, BridgeError> {
    body.as_object().ok_or(BridgeError::InvalidRequest)
}

pub(super) fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), BridgeError> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.iter().any(|allowed| allowed == field))
    {
        return Err(BridgeError::Unsupported {
            field: field.clone(),
        });
    }
    Ok(())
}

pub(super) fn required_string(
    object: &Map<String, Value>,
    field: &str,
) -> Result<String, BridgeError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(BridgeError::InvalidRequest)?;
    Ok(value.to_string())
}

pub(super) fn optional_bool(
    object: &Map<String, Value>,
    field: &str,
    default: bool,
) -> Result<bool, BridgeError> {
    object
        .get(field)
        .map(|value| value.as_bool().ok_or(BridgeError::InvalidRequest))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

pub(super) fn optional_string(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, BridgeError> {
    object
        .get(field)
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or(BridgeError::InvalidRequest)
        })
        .transpose()
}

pub(super) fn optional_f64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<f64>, BridgeError> {
    object
        .get(field)
        .map(|value| {
            let number = value.as_f64().ok_or(BridgeError::InvalidRequest)?;
            number
                .is_finite()
                .then_some(number)
                .ok_or(BridgeError::InvalidRequest)
        })
        .transpose()
}

pub(super) fn optional_u32(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u32>, BridgeError> {
    object
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .and_then(|number| u32::try_from(number).ok())
                .ok_or(BridgeError::InvalidRequest)
        })
        .transpose()
}

pub(super) fn parse_stop_sequences(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Vec<String>, BridgeError> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    if let Some(value) = value.as_str() {
        return Ok(vec![value.to_string()]);
    }
    value
        .as_array()
        .ok_or(BridgeError::InvalidRequest)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(BridgeError::InvalidRequest)
        })
        .collect()
}

pub(super) fn parse_generation(
    object: &Map<String, Value>,
    max_token_fields: &[&str],
    stop_field: &str,
) -> Result<GenerationIr, BridgeError> {
    let mut max_tokens = None;
    for field in max_token_fields {
        let value = optional_u32(object, field)?;
        if value.is_some() {
            if max_tokens.is_some() && max_tokens != value {
                return Err(BridgeError::InvalidRequest);
            }
            max_tokens = value;
        }
    }
    Ok(GenerationIr {
        max_tokens,
        temperature: optional_f64(object, "temperature")?,
        top_p: optional_f64(object, "top_p")?,
        stop_sequences: parse_stop_sequences(object, stop_field)?,
        reasoning_effort: None,
    })
}

pub(super) fn add_extension(
    extensions: &mut BTreeMap<String, Value>,
    key: &str,
    value: Option<&Value>,
) {
    if let Some(value) = value {
        extensions.insert(key.to_string(), value.clone());
    }
}

pub(super) fn parse_arguments(value: &Value) -> Result<Value, BridgeError> {
    let input_bytes = match value {
        Value::String(value) => value.len(),
        Value::Object(_) => serde_json::to_vec(value)
            .map_err(|_| BridgeError::InvalidRequest)?
            .len(),
        _ => return Err(BridgeError::InvalidRequest),
    };
    if input_bytes > super::tools::DEFAULT_MAX_TOOL_ARGUMENT_BYTES {
        return Err(BridgeError::ResourceLimit);
    }
    let parsed = match value {
        Value::String(value) => {
            serde_json::from_str(value).map_err(|_| BridgeError::InvalidRequest)?
        }
        Value::Object(_) => value.clone(),
        _ => return Err(BridgeError::InvalidRequest),
    };
    if parsed.is_object() {
        Ok(parsed)
    } else {
        Err(BridgeError::InvalidRequest)
    }
}

pub(super) fn text_content(value: &Value, field: &str) -> Result<Vec<ContentIr>, BridgeError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![ContentIr::Text(text.to_string())]);
    }
    let blocks = value.as_array().ok_or(BridgeError::Unsupported {
        field: field.to_string(),
    })?;
    blocks
        .iter()
        .map(|block| {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                Ok(ContentIr::Text(text.to_string()))
            } else {
                Err(BridgeError::Unsupported {
                    field: field.to_string(),
                })
            }
        })
        .collect()
}

pub(super) fn text_from_content(content: &[ContentIr]) -> Result<String, BridgeError> {
    let mut text = String::new();
    for item in content {
        match item {
            ContentIr::Text(value) | ContentIr::Thinking { text: value, .. } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(value);
            }
            _ => {
                return Err(BridgeError::Unsupported {
                    field: "content".to_string(),
                })
            }
        }
    }
    Ok(text)
}

pub(super) fn tool_definition(
    name: String,
    description: Option<String>,
    input_schema: Value,
) -> ToolDefinitionIr {
    ToolDefinitionIr {
        name,
        description,
        input_schema,
        strict: None,
    }
}

pub(super) fn tool_call(
    call_id: String,
    item_id: Option<String>,
    index: usize,
    name: String,
    arguments: Value,
) -> ToolCallIr {
    ToolCallIr {
        call_id,
        item_id,
        index,
        name,
        arguments,
    }
}

pub(super) fn tool_result(
    call_id: String,
    content: Vec<ContentIr>,
    is_error: bool,
) -> ToolResultIr {
    ToolResultIr {
        call_id,
        content,
        is_error,
    }
}

pub(super) fn invalid_role(role: &str) -> BridgeError {
    BridgeError::Unsupported {
        field: format!("message.role:{role}"),
    }
}

pub(super) fn validate_tool_state(messages: &[super::ir::MessageIr]) -> Result<(), BridgeError> {
    let mut pending = VecDeque::new();
    let mut seen_calls = BTreeSet::new();
    let mut seen_items = BTreeSet::new();
    let mut next_index = 0;

    for message in messages {
        match message.role {
            super::ir::RoleIr::Assistant => {
                let mut saw_tool_call = false;
                for content in &message.content {
                    match content {
                        ContentIr::ToolUse(call) => {
                            if (!pending.is_empty() && call.index != next_index)
                                || (pending.is_empty() && call.index != 0)
                                || !call.arguments.is_object()
                                || call.call_id.trim().is_empty()
                                || call.name.trim().is_empty()
                                || !seen_calls.insert(call.call_id.clone())
                                || call.item_id.as_ref().is_some_and(|item_id| {
                                    item_id.trim().is_empty() || !seen_items.insert(item_id.clone())
                                })
                            {
                                return Err(BridgeError::ToolState);
                            }
                            pending.push_back(call.call_id.clone());
                            next_index = call
                                .index
                                .checked_add(1)
                                .ok_or(BridgeError::ResourceLimit)?;
                            saw_tool_call = true;
                        }
                        ContentIr::ToolResult(_) => return Err(BridgeError::ToolState),
                        _ if !pending.is_empty() || saw_tool_call => {
                            return Err(BridgeError::ToolState)
                        }
                        _ => {}
                    }
                }
            }
            super::ir::RoleIr::Tool => {
                for content in &message.content {
                    let ContentIr::ToolResult(result) = content else {
                        return Err(BridgeError::ToolState);
                    };
                    // Anthropic clients may return parallel tool results in any
                    // order (Claude Code completes them asynchronously); match by
                    // call id rather than declaration order.
                    let position = pending
                        .iter()
                        .position(|id| id == &result.call_id)
                        .ok_or(BridgeError::ToolState)?;
                    pending.remove(position);
                    if pending.is_empty() {
                        next_index = 0;
                    }
                }
            }
            _ => {
                if !pending.is_empty()
                    || message.content.iter().any(|content| {
                        matches!(content, ContentIr::ToolUse(_) | ContentIr::ToolResult(_))
                    })
                {
                    return Err(BridgeError::ToolState);
                }
            }
        }
    }
    Ok(())
}

/// 工具重名唯一守卫（单一事实源）：chat 按名回填 strict、tool_choice 按
/// 名寻址——重名在任何目标协议下都是歧义，统一 Unsupported 诊断。
pub(super) fn reject_duplicate_tool_names(
    tools: &[super::ir::ToolDefinitionIr],
) -> Result<(), BridgeError> {
    let mut seen = std::collections::BTreeSet::new();
    for tool in tools {
        if !seen.insert(tool.name.as_str()) {
            return Err(BridgeError::Unsupported {
                field: "tools.duplicate_name".to_string(),
            });
        }
    }
    Ok(())
}

pub(super) fn validate_target_representability(
    ir: &RequestIr,
    target: WireProtocol,
) -> Result<(), BridgeError> {
    if target != WireProtocol::AnthropicMessages
        && ir.extensions.contains_key(EXT_ANTHROPIC_THINKING)
    {
        return Err(BridgeError::Unsupported {
            field: "anthropic.thinking".to_string(),
        });
    }
    // [C1] 重名守卫必须在共享层：chat 按名回填 strict、tool_choice 按名寻址，
    // anthropic/responses 入口放进来的重名工具转 chat 上游时会复现同一歧义。
    reject_duplicate_tool_names(&ir.tools)?;
    if target == WireProtocol::OpenAiResponses {
        return Ok(());
    }

    for message in &ir.messages {
        if message.item_id.is_some() && target == WireProtocol::AnthropicMessages {
            return Err(BridgeError::Unsupported {
                field: "responses.message_item_id".to_string(),
            });
        }
        if message.name.is_some() && target == WireProtocol::AnthropicMessages {
            return Err(BridgeError::Unsupported {
                field: "message.name".to_string(),
            });
        }
        for content in &message.content {
            if let ContentIr::ToolUse(call) = content {
                if call.item_id.is_some() && target == WireProtocol::AnthropicMessages {
                    return Err(BridgeError::Unsupported {
                        field: "responses.item_id".to_string(),
                    });
                }
            }
        }
    }

    if ir
        .extensions
        .get(EXT_RESPONSES_REASONING_IDS)
        .and_then(Value::as_array)
        .is_some_and(|ids| {
            ids.iter().enumerate().any(|(index, id)| {
                !id.is_null()
                    && (target != WireProtocol::AnthropicMessages
                        || !reasoning_item_is_recoverable(ir, index))
            })
        })
    {
        return Err(BridgeError::Unsupported {
            field: "responses.reasoning".to_string(),
        });
    }
    if ir
        .extensions
        .contains_key(EXT_RESPONSES_REASONING_ITEM_METADATA)
        && (target != WireProtocol::AnthropicMessages || !reasoning_metadata_is_recoverable(ir))
    {
        return Err(BridgeError::Unsupported {
            field: "responses.reasoning_item_metadata".to_string(),
        });
    }
    if ir
        .extensions
        .contains_key(EXT_RESPONSES_PREVIOUS_RESPONSE_ID)
    {
        return Err(BridgeError::Unsupported {
            field: "previous_response_id".to_string(),
        });
    }
    if target == WireProtocol::AnthropicMessages
        && (ir.extensions.contains_key(EXT_CHAT_PARALLEL_TOOL_CALLS)
            || ir
                .extensions
                .contains_key(EXT_RESPONSES_PARALLEL_TOOL_CALLS))
    {
        return Err(BridgeError::Unsupported {
            field: "parallel_tool_calls".to_string(),
        });
    }
    Ok(())
}

fn reasoning_item_is_recoverable(ir: &RequestIr, index: usize) -> bool {
    ir.messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            ContentIr::Thinking {
                signature: Some(signature),
                ..
            }
            | ContentIr::RedactedThinking { data: signature } => Some(signature.as_str()),
            _ => None,
        })
        .nth(index)
        .and_then(super::reasoning::decode_openai_reasoning_item)
        .is_some()
}

fn reasoning_metadata_is_recoverable(ir: &RequestIr) -> bool {
    let Some(metadata) = ir
        .extensions
        .get(EXT_RESPONSES_REASONING_ITEM_METADATA)
        .and_then(Value::as_array)
    else {
        return false;
    };
    metadata.iter().enumerate().all(|(index, metadata)| {
        metadata.as_object().is_none_or(Map::is_empty) || reasoning_item_is_recoverable(ir, index)
    })
}

pub(super) fn provider_max_output_tokens(ir: &RequestIr) -> Result<Option<u32>, BridgeError> {
    ir.extensions
        .get(EXT_PROVIDER_MAX_OUTPUT_TOKENS)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(BridgeError::InvalidRequest)
        })
        .transpose()
}

#[cfg(test)]
mod tests_reversed_tool_results {
    use serde_json::json;

    use super::super::{parse_request, WireProtocol};

    #[test]
    fn accepts_parallel_tool_results_in_any_order() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        { "type": "tool_use", "id": "call_a", "name": "lookup", "input": { "q": "a" } },
                        { "type": "tool_use", "id": "call_b", "name": "lookup", "input": { "q": "b" } }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "call_b", "content": "result b" },
                        { "type": "tool_result", "tool_use_id": "call_a", "content": "result a" }
                    ]
                }
            ]
        });
        let ir = parse_request(WireProtocol::AnthropicMessages, &body)
            .expect("reversed tool results must be accepted");
        assert_eq!(ir.messages.len(), 3);

        let mut orphan = body.clone();
        orphan["messages"][1]["content"][1]["tool_use_id"] = json!("call_unknown");
        assert!(matches!(
            parse_request(WireProtocol::AnthropicMessages, &orphan),
            Err(super::super::BridgeError::ToolState)
        ));
    }
}
