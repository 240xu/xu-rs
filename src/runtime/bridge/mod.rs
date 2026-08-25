mod anthropic;
mod chat;
mod ir;
mod reasoning;
mod rectifier;
mod request;
mod response;
mod responses;
mod stream;
mod tools;

pub use ir::*;
pub use rectifier::{thinking_rejection, DefaultRectifier, NoopRectifier, RequestRectifier};
pub(crate) use request::EXT_PROVIDER_CACHE_MODE;
pub use request::{encode_upstream_request, parse_request};
pub use response::{decode_response, encode_response};
pub use stream::{decode_stream_frame, encode_stream_events, SseDecoder, StreamState};
pub use tools::{ToolCallState, ToolCallTracker, DEFAULT_MAX_TOOL_ARGUMENT_BYTES};

pub fn protocol_for_path(path: &str) -> Option<WireProtocol> {
    match path {
        "/v1/messages" => Some(WireProtocol::AnthropicMessages),
        "/v1/chat/completions" => Some(WireProtocol::OpenAiChat),
        "/v1/responses" => Some(WireProtocol::OpenAiResponses),
        _ => None,
    }
}

pub fn upstream_path(protocol: WireProtocol) -> &'static str {
    match protocol {
        WireProtocol::AnthropicMessages => "messages",
        WireProtocol::OpenAiChat => "chat/completions",
        WireProtocol::OpenAiResponses => "responses",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ir::{
        BridgeError, CompletionIr, ContentIr, GenerationIr, MessageIr, RequestIr, ResponseIr,
        ResponseMetaIr, RoleIr, StreamEventIr, ToolCallIr, ToolChoiceIr, ToolDefinitionIr,
        ToolResultIr, UsageIr, WireProtocol,
    };
    use super::tools::{ToolCallState, ToolCallTracker};
    use crate::domain::ProtocolKind;

    #[test]
    fn request_ir_round_trip_preserves_structured_content_and_order() {
        let original = RequestIr {
            protocol: WireProtocol::OpenAiResponses,
            model: "reasoning-model".to_string(),
            messages: vec![
                MessageIr {
                    role: RoleIr::Developer,
                    content: vec![ContentIr::Text("Follow the policy".to_string())],
                    name: None,
                    item_id: None,
                },
                MessageIr {
                    role: RoleIr::Assistant,
                    content: vec![
                        ContentIr::Thinking {
                            text: "plan".to_string(),
                            signature: Some("sig".to_string()),
                        },
                        ContentIr::ToolUse(ToolCallIr {
                            call_id: "call-1".to_string(),
                            item_id: Some("item-1".to_string()),
                            index: 0,
                            name: "lookup".to_string(),
                            arguments: json!({"q": "rust"}),
                        }),
                    ],
                    name: None,
                    item_id: None,
                },
                MessageIr {
                    role: RoleIr::Tool,
                    content: vec![ContentIr::ToolResult(ToolResultIr {
                        call_id: "call-1".to_string(),
                        content: vec![ContentIr::Text("result".to_string())],
                        is_error: false,
                    })],
                    name: None,
                    item_id: None,
                },
            ],
            tools: vec![ToolDefinitionIr {
                name: "lookup".to_string(),
                description: Some("Look up a value".to_string()),
                input_schema: json!({"type": "object"}),
                strict: None,
            }],
            tool_choice: Some(ToolChoiceIr::Tool {
                name: "lookup".to_string(),
            }),
            generation: GenerationIr {
                max_tokens: Some(512),
                temperature: Some(0.2),
                top_p: None,
                stop_sequences: vec!["END".to_string()],
                reasoning_effort: Some("high".to_string()),
            },
            stream: true,
            metadata: Some(json!({"trace": "test"})),
            extensions: std::collections::BTreeMap::new(),
        };

        let encoded = serde_json::to_value(&original).expect("IR serializes");
        let round_trip: RequestIr = serde_json::from_value(encoded).expect("IR deserializes");

        assert_eq!(round_trip.protocol, WireProtocol::OpenAiResponses);
        assert_eq!(round_trip.messages.len(), 3);
        assert_eq!(round_trip.messages[0].role, RoleIr::Developer);
        assert!(matches!(
            &round_trip.messages[1].content[0],
            ContentIr::Thinking { text, signature: Some(signature) }
                if text == "plan" && signature == "sig"
        ));
        assert!(matches!(
            &round_trip.messages[1].content[1],
            ContentIr::ToolUse(call) if call.call_id == "call-1" && call.arguments["q"] == "rust"
        ));
        assert!(matches!(
            &round_trip.messages[2].content[0],
            ContentIr::ToolResult(result) if result.call_id == "call-1" && !result.is_error
        ));
        assert_eq!(round_trip.tools[0].name, "lookup");
        assert_eq!(round_trip.generation.max_tokens, Some(512));
        assert!(round_trip.stream);
        assert_eq!(round_trip.metadata, Some(json!({"trace": "test"})));
    }

    #[test]
    fn response_and_stream_round_trip_preserves_terminal_usage_and_reasoning_fields() {
        let completion = CompletionIr {
            finish_reason: Some("stop".to_string()),
            stop_reason: Some("end_turn".to_string()),
            status: Some("completed".to_string()),
            error: None,
        };
        let usage = UsageIr {
            input_tokens: Some(10),
            output_tokens: Some(7),
            total_tokens: Some(17),
            cache_read_tokens: Some(2),
            cache_write_tokens: Some(1),
            reasoning_tokens: Some(3),
        };
        let response = ResponseIr {
            meta: ResponseMetaIr {
                id: Some("resp-1".to_string()),
                model: Some("reasoning-model".to_string()),
            },
            content: vec![
                ContentIr::RedactedThinking {
                    data: "opaque-reasoning-envelope".to_string(),
                },
                ContentIr::Text("answer".to_string()),
                ContentIr::ToolUse(ToolCallIr {
                    call_id: "call-1".to_string(),
                    item_id: Some("item-1".to_string()),
                    index: 0,
                    name: "lookup".to_string(),
                    arguments: json!({"q": "rust"}),
                }),
            ],
            usage: Some(usage.clone()),
            completion: completion.clone(),
            extensions: std::collections::BTreeMap::new(),
        };

        let encoded = serde_json::to_value(&response).expect("response IR serializes");
        let round_trip: ResponseIr =
            serde_json::from_value(encoded).expect("response IR deserializes");

        assert_eq!(round_trip.meta.id.as_deref(), Some("resp-1"));
        assert!(matches!(
            &round_trip.content[0],
            ContentIr::RedactedThinking { data } if data == "opaque-reasoning-envelope"
        ));
        assert!(matches!(
            &round_trip.content[2],
            ContentIr::ToolUse(call) if call.item_id.as_deref() == Some("item-1")
        ));
        assert_eq!(round_trip.usage, Some(usage.clone()));
        assert_eq!(round_trip.completion, completion);

        let failed_completion = CompletionIr {
            status: Some("failed".to_string()),
            error: Some("upstream semantic failure".to_string()),
            ..CompletionIr::default()
        };
        let encoded = serde_json::to_value(&failed_completion).expect("failure serializes");
        let failed_round_trip: CompletionIr =
            serde_json::from_value(encoded).expect("failure deserializes");
        assert_eq!(failed_round_trip.status.as_deref(), Some("failed"));
        assert_eq!(
            failed_round_trip.error.as_deref(),
            Some("upstream semantic failure")
        );

        let events = vec![
            StreamEventIr::Started(ResponseMetaIr {
                id: Some("resp-1".to_string()),
                model: Some("reasoning-model".to_string()),
            }),
            StreamEventIr::ReasoningDelta {
                text: "plan".to_string(),
            },
            StreamEventIr::ToolCallStarted(ToolCallIr {
                call_id: "call-1".to_string(),
                item_id: Some("item-1".to_string()),
                index: 0,
                name: "lookup".to_string(),
                arguments: json!({}),
            }),
            StreamEventIr::ToolCallArgumentsDelta {
                call_id: "call-1".to_string(),
                delta: "{\"q\":\"rust\"}".to_string(),
            },
            StreamEventIr::Usage(usage),
            StreamEventIr::ToolCallFinished {
                call_id: "call-1".to_string(),
            },
            StreamEventIr::Completed(completion),
        ];
        let encoded = serde_json::to_value(&events).expect("stream IR serializes");
        let round_trip: Vec<StreamEventIr> =
            serde_json::from_value(encoded).expect("stream IR deserializes");

        assert!(matches!(
            &round_trip[0],
            StreamEventIr::Started(ResponseMetaIr { id: Some(id), .. }) if id == "resp-1"
        ));
        assert!(matches!(
            &round_trip[3],
            StreamEventIr::ToolCallArgumentsDelta { call_id, delta }
                if call_id == "call-1" && delta == "{\"q\":\"rust\"}"
        ));
        assert!(matches!(
            &round_trip[6],
            StreamEventIr::Completed(CompletionIr { status: Some(status), .. })
                if status == "completed"
        ));
    }

    #[test]
    fn wire_protocol_maps_to_existing_protocol_kind() {
        assert_eq!(
            WireProtocol::AnthropicMessages.protocol_kind(),
            ProtocolKind::AnthropicMessages
        );
        assert_eq!(
            WireProtocol::OpenAiChat.protocol_kind(),
            ProtocolKind::OpenAiChat
        );
        assert_eq!(
            WireProtocol::OpenAiResponses.protocol_kind(),
            ProtocolKind::OpenAiResponses
        );
        assert_eq!(
            WireProtocol::from(ProtocolKind::OpenAiChat),
            WireProtocol::OpenAiChat
        );
    }

    #[test]
    fn public_paths_map_to_wire_protocols_and_upstream_paths() {
        let cases = [
            ("/v1/messages", WireProtocol::AnthropicMessages, "messages"),
            (
                "/v1/chat/completions",
                WireProtocol::OpenAiChat,
                "chat/completions",
            ),
            ("/v1/responses", WireProtocol::OpenAiResponses, "responses"),
        ];

        for (path, protocol, upstream) in cases {
            assert_eq!(super::protocol_for_path(path), Some(protocol));
            assert_eq!(super::upstream_path(protocol), upstream);
        }
        assert_eq!(super::protocol_for_path("/v1/unknown"), None);
    }

    #[test]
    fn bridge_error_has_safe_http_and_log_classification() {
        let cases = [
            (BridgeError::InvalidRequest, 400, "invalid_request"),
            (
                BridgeError::Unsupported {
                    field: "thinking".to_string(),
                },
                400,
                "unsupported",
            ),
            (BridgeError::InvalidUpstream, 502, "invalid_upstream"),
            (BridgeError::ToolState, 400, "tool_state"),
            (BridgeError::ResourceLimit, 413, "resource_limit"),
            (BridgeError::Timeout, 504, "timeout"),
            (BridgeError::Internal, 500, "internal"),
        ];

        for (error, status, category) in cases {
            assert_eq!(error.http_status(), status);
            assert_eq!(error.log_category(), category);
            assert!(!error.public_message().contains("thinking"));
        }

        let sensitive = BridgeError::Unsupported {
            field: "prompt=secret tool_args={\"token\":\"secret\"}".to_string(),
        };
        let envelope = sensitive.public_envelope();
        assert_eq!(envelope.error.code, "unsupported");
        assert_eq!(envelope.error.message, "unsupported request field");
        assert!(!serde_json::to_string(&envelope)
            .expect("error envelope serializes")
            .contains("secret"));
        assert!(!serde_json::to_string(&sensitive)
            .expect("bridge error serializes")
            .contains("secret"));
        assert!(!format!("{sensitive:?}").contains("secret"));
    }

    #[test]
    fn tool_call_state_bounds_argument_fragments_and_tracks_completion() {
        let mut state = ToolCallState::new("call-1", Some("item-1".to_string()), 2, "lookup", 5)
            .expect("valid tool identity");

        state.push_arguments("ab").expect("first fragment fits");
        state.push_arguments("cd").expect("second fragment fits");
        assert_eq!(state.arguments(), "abcd");
        assert!(!state.is_completed());
        assert_eq!(state.call_id(), "call-1");
        assert_eq!(state.item_id(), Some("item-1"));
        assert_eq!(state.index(), 2);
        assert_eq!(state.name(), "lookup");

        assert_eq!(state.push_arguments("ef"), Err(BridgeError::ResourceLimit));
        assert_eq!(state.arguments(), "abcd");
        state.finish().expect("tool call can finish once");
        assert!(state.is_completed());
        assert_eq!(state.finish(), Err(BridgeError::ToolState));
        assert_eq!(state.push_arguments("!"), Err(BridgeError::ToolState));
        assert!(!format!("{state:?}").contains("abcd"));
    }

    #[test]
    fn tool_call_state_rejects_empty_identity_fields() {
        assert_eq!(
            ToolCallState::new("", Some("item-1".to_string()), 0, "lookup", 5),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            ToolCallState::new("call-1", Some("".to_string()), 0, "lookup", 5),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            ToolCallState::new("call-1", None, 0, " ", 5),
            Err(BridgeError::ToolState)
        );
    }

    #[test]
    fn tool_call_tracker_rejects_duplicate_ids_mismatches_and_out_of_order_indexes() {
        let mut tracker = ToolCallTracker::new(8);
        tracker
            .start("call-0", Some("item-0"), 0, "first")
            .expect("first parallel call starts");
        tracker
            .start("call-1", Some("item-1"), 1, "second")
            .expect("second parallel call starts in order");

        assert_eq!(
            tracker.start("call-0", Some("item-2"), 2, "duplicate-call"),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            tracker.start("call-2", Some("item-1"), 2, "duplicate-item"),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            tracker.start("call-2", Some("item-2"), 4, "out-of-order"),
            Err(BridgeError::ToolState)
        );

        assert_eq!(
            tracker.push_arguments("call-0", Some("wrong-item"), "{}"),
            Err(BridgeError::ToolState)
        );
        assert_eq!(
            tracker.push_arguments("wrong-call", Some("item-0"), "{}"),
            Err(BridgeError::ToolState)
        );
        tracker
            .push_arguments("call-0", Some("item-0"), "{}")
            .expect("matching event identity is accepted");
        tracker
            .finish("call-0", Some("item-0"))
            .expect("matching finish identity is accepted");

        tracker
            .start("call-2", Some("item-2"), 2, "third")
            .expect("next index is accepted");
        let indexes: Vec<_> = tracker.states().map(ToolCallState::index).collect();
        assert_eq!(indexes, vec![0, 1, 2]);
    }
}
