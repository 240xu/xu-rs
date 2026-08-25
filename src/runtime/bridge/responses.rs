use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::ir::{
    BridgeError, CompletionIr, ContentIr, MessageIr, RequestIr, ResponseIr, ResponseMetaIr, RoleIr,
    SseFrame, StreamEventIr, ToolCallIr, ToolChoiceIr, ToolDefinitionIr, UsageIr, WireProtocol,
};
use super::reasoning::{
    encode_openai_reasoning_item, media_from_base64, media_from_url, media_to_responses_file,
    media_to_url, openai_reasoning_item_from_anthropic_block, reasoning_summary_text,
};
use super::request::{
    add_extension, object, optional_bool, optional_string, parse_arguments, parse_generation,
    reject_unknown_fields, required_string, text_content, text_from_content, tool_call,
    tool_result, validate_target_representability, validate_tool_state,
    EXT_CHAT_PARALLEL_TOOL_CALLS, EXT_RESPONSES_CLIENT_METADATA, EXT_RESPONSES_CONTEXT_MANAGEMENT,
    EXT_RESPONSES_INCLUDE, EXT_RESPONSES_PARALLEL_TOOL_CALLS, EXT_RESPONSES_PREVIOUS_RESPONSE_ID,
    EXT_RESPONSES_PROMPT_CACHE_KEY, EXT_RESPONSES_REASONING_IDS,
    EXT_RESPONSES_REASONING_ITEM_METADATA, EXT_RESPONSES_REQUEST_CONVERSATION,
    EXT_RESPONSES_SERVICE_TIER, EXT_RESPONSES_STORE, EXT_RESPONSES_STREAM_OPTIONS,
    EXT_RESPONSES_TEXT,
};
use super::response::{
    error_model, generated_created_at, generated_id, map_upstream_error, normalize_total_tokens,
    optional_u64, required_array, required_object, required_string as response_required_string,
    required_u64, response_model, safe_error_message, target_response_id,
    EXT_RESPONSES_COMPLETED_AT, EXT_RESPONSES_CONVERSATION, EXT_RESPONSES_INCOMPLETE_DETAILS,
    EXT_RESPONSES_INSTRUCTIONS, EXT_RESPONSES_MESSAGE_ITEM_ID, EXT_RESPONSES_MODERATION,
    EXT_RESPONSES_OUTPUT_ITEM_STATUSES, EXT_RESPONSES_PROMPT_CACHE_OPTIONS,
    EXT_RESPONSES_REASONING, EXT_RESPONSES_REASONING_ITEM_CONTENT,
    EXT_RESPONSES_REASONING_ITEM_IDS,
};
use super::stream::StreamState;

const EXT_INSTRUCTIONS: &str = "openai_responses.v1.instructions";
const EXT_INSTRUCTION_COUNT: &str = "openai_responses.v1.instruction_message_count";

pub(super) fn parse_request(body: &Value) -> Result<RequestIr, BridgeError> {
    let request_object = object(body)?;
    reject_unknown_fields(
        request_object,
        &[
            "model",
            "stream",
            "instructions",
            "input",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "max_output_tokens",
            "temperature",
            "top_p",
            "stop",
            "reasoning",
            "metadata",
            "previous_response_id",
            "store",
            "include",
            "prompt_cache_key",
            "client_metadata",
            "service_tier",
            "text",
            "stream_options",
            "conversation",
            "context_management",
        ],
    )?;
    let model = required_string(request_object, "model")?;
    let stream = optional_bool(request_object, "stream", false)?;
    let mut extensions = BTreeMap::new();
    add_extension(
        &mut extensions,
        EXT_INSTRUCTIONS,
        request_object.get("instructions"),
    );
    add_extension(
        &mut extensions,
        EXT_RESPONSES_PARALLEL_TOOL_CALLS,
        request_object.get("parallel_tool_calls"),
    );
    add_extension(
        &mut extensions,
        EXT_RESPONSES_PREVIOUS_RESPONSE_ID,
        request_object.get("previous_response_id"),
    );
    if let Some(store) = request_object.get("store") {
        if store.as_bool() != Some(false) {
            return Err(BridgeError::Unsupported {
                field: "store".to_string(),
            });
        }
        add_extension(&mut extensions, EXT_RESPONSES_STORE, Some(store));
    }
    if let Some(include) = request_object.get("include") {
        let include = include.as_array().ok_or(BridgeError::InvalidRequest)?;
        if include
            .iter()
            .any(|value| value.as_str() != Some("reasoning.encrypted_content"))
        {
            return Err(BridgeError::Unsupported {
                field: "include".to_string(),
            });
        }
        add_extension(
            &mut extensions,
            EXT_RESPONSES_INCLUDE,
            request_object.get("include"),
        );
    }
    if let Some(prompt_cache_key) = request_object.get("prompt_cache_key") {
        if !prompt_cache_key.is_string() {
            return Err(BridgeError::InvalidRequest);
        }
        add_extension(
            &mut extensions,
            EXT_RESPONSES_PROMPT_CACHE_KEY,
            Some(prompt_cache_key),
        );
    }
    if let Some(client_metadata) = request_object.get("client_metadata") {
        if !client_metadata.is_object() {
            return Err(BridgeError::InvalidRequest);
        }
        add_extension(
            &mut extensions,
            EXT_RESPONSES_CLIENT_METADATA,
            Some(client_metadata),
        );
    }
    if let Some(service_tier) = request_object.get("service_tier") {
        if !service_tier.is_string() {
            return Err(BridgeError::InvalidRequest);
        }
        add_extension(
            &mut extensions,
            EXT_RESPONSES_SERVICE_TIER,
            Some(service_tier),
        );
    }
    if let Some(text) = request_object.get("text") {
        if !text.is_object() {
            return Err(BridgeError::InvalidRequest);
        }
        add_extension(&mut extensions, EXT_RESPONSES_TEXT, Some(text));
    }
    if let Some(stream_options) = request_object.get("stream_options") {
        if !stream_options.is_object() {
            return Err(BridgeError::InvalidRequest);
        }
        add_extension(
            &mut extensions,
            EXT_RESPONSES_STREAM_OPTIONS,
            Some(stream_options),
        );
    }
    if let Some(conversation) = request_object.get("conversation") {
        if !conversation.is_object() && !conversation.is_string() {
            return Err(BridgeError::InvalidRequest);
        }
        add_extension(
            &mut extensions,
            EXT_RESPONSES_REQUEST_CONVERSATION,
            Some(conversation),
        );
    }
    if let Some(context_management) = request_object.get("context_management") {
        if !context_management.is_object() {
            return Err(BridgeError::InvalidRequest);
        }
        add_extension(
            &mut extensions,
            EXT_RESPONSES_CONTEXT_MANAGEMENT,
            Some(context_management),
        );
    }
    optional_bool(request_object, "parallel_tool_calls", true)?;

    let mut messages = Vec::new();
    let mut reasoning_ids = Vec::new();
    let mut reasoning_metadata = Vec::new();
    if let Some(instructions) = request_object.get("instructions") {
        let content = parse_instruction_content(instructions)?;
        if !content.is_empty() {
            messages.push(MessageIr {
                role: RoleIr::System,
                content,
                name: None,
                item_id: None,
            });
            extensions.insert(EXT_INSTRUCTION_COUNT.to_string(), json!(1));
        }
    }

    let input = request_object
        .get("input")
        .ok_or(BridgeError::InvalidRequest)?;
    if let Some(text) = input.as_str() {
        messages.push(MessageIr {
            role: RoleIr::User,
            content: vec![ContentIr::Text(text.to_string())],
            name: None,
            item_id: None,
        });
    } else {
        for item in input.as_array().ok_or(BridgeError::InvalidRequest)? {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    let call = parse_function_call(item, tool_index(&messages))?;
                    append_assistant_content(&mut messages, ContentIr::ToolUse(call))?;
                }
                Some("function_call_output") => {
                    let item_object = object(item)?;
                    reject_unknown_fields(
                        item_object,
                        &["type", "id", "call_id", "output", "status"],
                    )?;
                    let call_id = required_string(item_object, "call_id")?;
                    let content = parse_output_content(
                        item_object
                            .get("output")
                            .ok_or(BridgeError::InvalidRequest)?,
                    )?;
                    let is_error = item_object
                        .get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|status| status == "failed");
                    messages.push(MessageIr {
                        role: RoleIr::Tool,
                        content: vec![ContentIr::ToolResult(tool_result(
                            call_id, content, is_error,
                        ))],
                        name: None,
                        item_id: None,
                    });
                }
                Some("reasoning") => {
                    let item_object = object(item)?;
                    let content = parse_reasoning(item_object)?;
                    reasoning_metadata.push(parse_reasoning_item_metadata(item_object)?);
                    if let Some(id) = item_object.get("id") {
                        reasoning_ids.push(id.clone());
                    } else {
                        reasoning_ids.push(Value::Null);
                    }
                    append_assistant_content(&mut messages, content)?;
                }
                Some("message") | None => {
                    let message = parse_message_item(item)?;
                    append_message(&mut messages, message)?;
                }
                Some(other) => {
                    return Err(BridgeError::Unsupported {
                        field: format!("input.type:{other}"),
                    })
                }
            }
        }
    }
    if !reasoning_ids.is_empty() {
        extensions.insert(
            EXT_RESPONSES_REASONING_IDS.to_string(),
            Value::Array(reasoning_ids),
        );
    }
    if reasoning_metadata.iter().any(|metadata| {
        metadata
            .as_object()
            .is_some_and(|metadata| !metadata.is_empty())
    }) {
        extensions.insert(
            EXT_RESPONSES_REASONING_ITEM_METADATA.to_string(),
            Value::Array(reasoning_metadata),
        );
    }

    let mut generation = parse_generation(request_object, &["max_output_tokens"], "stop")?;
    if let Some(reasoning) = request_object.get("reasoning") {
        let reasoning = object(reasoning)?;
        reject_unknown_fields(reasoning, &["effort"])?;
        generation.reasoning_effort = optional_string(reasoning, "effort")?;
    }

    let ir = RequestIr {
        protocol: WireProtocol::OpenAiResponses,
        model,
        messages,
        tools: parse_tools(request_object.get("tools"))?,
        tool_choice: parse_tool_choice(request_object.get("tool_choice"))?,
        generation,
        stream,
        metadata: request_object.get("metadata").cloned(),
        extensions,
    };
    validate_tool_state(&ir.messages)?;
    Ok(ir)
}

fn parse_instruction_content(value: &Value) -> Result<Vec<ContentIr>, BridgeError> {
    if value.is_string() {
        return text_content(value, "instructions");
    }
    let blocks = value.as_array().ok_or(BridgeError::InvalidRequest)?;
    blocks
        .iter()
        .map(|block| {
            let object = object(block)?;
            reject_unknown_fields(object, &["type", "text"])?;
            match object.get("type").and_then(Value::as_str) {
                Some("input_text") | Some("text") => {
                    Ok(ContentIr::Text(required_string(object, "text")?))
                }
                Some(other) => Err(BridgeError::Unsupported {
                    field: format!("instructions.type:{other}"),
                }),
                None => Err(BridgeError::InvalidRequest),
            }
        })
        .collect()
}

fn parse_message_item(value: &Value) -> Result<MessageIr, BridgeError> {
    let object = object(value)?;
    reject_unknown_fields(object, &["id", "type", "role", "content", "name"])?;
    let role = required_string(object, "role")?;
    let content = parse_input_content(object.get("content").ok_or(BridgeError::InvalidRequest)?)?;
    let role = match role.as_str() {
        "system" => RoleIr::System,
        "developer" => RoleIr::Developer,
        "user" => RoleIr::User,
        "assistant" => RoleIr::Assistant,
        other => return Err(super::request::invalid_role(other)),
    };
    Ok(MessageIr {
        role,
        content,
        name: optional_string(object, "name")?,
        item_id: optional_string(object, "id")?,
    })
}

fn parse_input_content(value: &Value) -> Result<Vec<ContentIr>, BridgeError> {
    if value.is_string() {
        return text_content(value, "input.message.content");
    }
    value
        .as_array()
        .ok_or(BridgeError::InvalidRequest)?
        .iter()
        .map(|part| {
            let object = object(part)?;
            match object.get("type").and_then(Value::as_str) {
                Some("input_text") | Some("output_text") | Some("text") => {
                    reject_unknown_fields(object, &["type", "text"])?;
                    Ok(ContentIr::Text(required_string(object, "text")?))
                }
                Some("input_image") => {
                    reject_unknown_fields(object, &["type", "image_url", "url"])?;
                    Ok(ContentIr::Image(parse_input_image(object)?))
                }
                Some("input_file") | Some("document") => {
                    reject_unknown_fields(
                        object,
                        &["type", "file_url", "url", "file_data", "mime_type"],
                    )?;
                    Ok(ContentIr::Document(parse_input_file(object)?))
                }
                Some(other) => Err(BridgeError::Unsupported {
                    field: format!("input.message.content.type:{other}"),
                }),
                None => Err(BridgeError::InvalidRequest),
            }
        })
        .collect()
}

fn parse_input_image(object: &Map<String, Value>) -> Result<super::ir::MediaIr, BridgeError> {
    if object.contains_key("image_url") && object.contains_key("url") {
        return Err(BridgeError::InvalidRequest);
    }
    let url = object
        .get("image_url")
        .or_else(|| object.get("url"))
        .and_then(Value::as_str)
        .ok_or(BridgeError::InvalidRequest)?;
    media_from_url(url)
}

fn parse_input_file(object: &Map<String, Value>) -> Result<super::ir::MediaIr, BridgeError> {
    if (object.contains_key("file_url") || object.contains_key("url"))
        && object.contains_key("file_data")
    {
        return Err(BridgeError::InvalidRequest);
    }
    if object.contains_key("file_url") && object.contains_key("url") {
        return Err(BridgeError::InvalidRequest);
    }
    if let Some(url) = object
        .get("file_url")
        .or_else(|| object.get("url"))
        .and_then(Value::as_str)
    {
        return media_from_url(url);
    }
    if let Some(data) = object.get("file_data").and_then(Value::as_str) {
        if data.starts_with("data:") {
            return media_from_url(data);
        }
        return media_from_base64(object.get("mime_type").and_then(Value::as_str), data);
    }
    Err(BridgeError::InvalidRequest)
}

fn parse_function_call(value: &Value, index: usize) -> Result<ToolCallIr, BridgeError> {
    let object = object(value)?;
    reject_unknown_fields(
        object,
        &["id", "type", "call_id", "name", "arguments", "status"],
    )?;
    if let Some(status) = object.get("status") {
        if status.as_str() != Some("completed") {
            return Err(BridgeError::Unsupported {
                field: "function_call.status".to_string(),
            });
        }
    }
    Ok(tool_call(
        required_string(object, "call_id")?,
        optional_string(object, "id")?,
        index,
        required_string(object, "name")?,
        parse_arguments(object.get("arguments").ok_or(BridgeError::InvalidRequest)?)?,
    ))
}

fn parse_reasoning(reasoning_object: &Map<String, Value>) -> Result<ContentIr, BridgeError> {
    reject_unknown_fields(
        reasoning_object,
        &[
            "id",
            "type",
            "status",
            "summary",
            "content",
            "encrypted_content",
        ],
    )?;
    if let Some(summary) = reasoning_object.get("summary") {
        let summary = summary.as_array().ok_or(BridgeError::InvalidRequest)?;
        for item in summary {
            let item = super::request::object(item)?;
            reject_unknown_fields(item, &["type", "text"])?;
            if item.get("type").and_then(Value::as_str) != Some("summary_text") {
                return Err(BridgeError::Unsupported {
                    field: "reasoning.summary.type".to_string(),
                });
            }
            required_string(item, "text")?;
        }
    }
    let reasoning_item = Value::Object(reasoning_object.clone());
    let summary_text = reasoning_summary_text(&reasoning_item);
    let content_text = reasoning_content_text(reasoning_object.get("content"))?;
    let encrypted = reasoning_object
        .get("encrypted_content")
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .map(Some)
                .ok_or(BridgeError::InvalidRequest)
        })
        .transpose()?;
    let encrypted = encrypted.flatten();
    let text = if summary_text.is_empty() && encrypted.is_none() {
        content_text
    } else {
        summary_text
    };
    let encrypted = encrypted
        .map(|_| encode_openai_reasoning_item(&reasoning_item).ok_or(BridgeError::InvalidRequest))
        .transpose()?;
    match (text.is_empty(), encrypted) {
        (false, signature) => Ok(ContentIr::Thinking { text, signature }),
        (true, Some(data)) => Ok(ContentIr::RedactedThinking { data }),
        (true, None) => Ok(ContentIr::Thinking {
            text,
            signature: None,
        }),
    }
}

fn parse_reasoning_item_metadata(object: &Map<String, Value>) -> Result<Value, BridgeError> {
    let mut metadata = Map::new();
    if let Some(status) = object.get("status") {
        let status = status.as_str().ok_or(BridgeError::InvalidRequest)?;
        if !matches!(status, "in_progress" | "completed" | "incomplete") {
            return Err(BridgeError::Unsupported {
                field: "reasoning.status".to_string(),
            });
        }
        metadata.insert("status".to_string(), Value::String(status.to_string()));
    }
    if let Some(content) = object.get("content") {
        metadata.insert("content".to_string(), content.clone());
    }
    Ok(Value::Object(metadata))
}

fn validate_reasoning_content(value: &Value) -> Result<(), BridgeError> {
    let content = value.as_array().ok_or(BridgeError::InvalidUpstream)?;
    for item in content {
        let item = item.as_object().ok_or(BridgeError::InvalidUpstream)?;
        reject_unknown_fields(item, &["type", "text"]).map_err(map_upstream_error)?;
        if item.get("type").and_then(Value::as_str) != Some("reasoning_text") {
            return Err(BridgeError::Unsupported {
                field: "responses.output.reasoning.content.type".to_string(),
            });
        }
        response_required_string(item, "text")?;
    }
    Ok(())
}

fn reasoning_content_text(value: Option<&Value>) -> Result<String, BridgeError> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    validate_reasoning_content(value)?;
    let mut text = String::new();
    for item in value.as_array().expect("validated reasoning content") {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(
            item.get("text")
                .and_then(Value::as_str)
                .expect("validated reasoning text"),
        );
    }
    Ok(text)
}

fn parse_output_content(value: &Value) -> Result<Vec<ContentIr>, BridgeError> {
    if value.is_string() {
        return text_content(value, "function_call_output.output");
    }
    parse_input_content(value)
}

fn append_assistant_content(
    messages: &mut Vec<MessageIr>,
    content: ContentIr,
) -> Result<(), BridgeError> {
    if let Some(last) = messages.last_mut() {
        if last.role == RoleIr::Assistant {
            if let ContentIr::ToolUse(call) = &content {
                if last
                    .content
                    .iter()
                    .filter_map(|item| match item {
                        ContentIr::ToolUse(call) => Some(call.index),
                        _ => None,
                    })
                    .any(|index| index == call.index)
                {
                    return Err(BridgeError::ToolState);
                }
            }
            last.content.push(content);
            return Ok(());
        }
    }
    messages.push(MessageIr {
        role: RoleIr::Assistant,
        content: vec![content],
        name: None,
        item_id: None,
    });
    Ok(())
}

fn append_message(messages: &mut Vec<MessageIr>, message: MessageIr) -> Result<(), BridgeError> {
    messages.push(message);
    Ok(())
}

fn tool_index(messages: &[MessageIr]) -> usize {
    messages
        .last()
        .and_then(|message| {
            (message.role == RoleIr::Assistant).then(|| {
                message
                    .content
                    .iter()
                    .filter(|item| matches!(item, ContentIr::ToolUse(_)))
                    .count()
            })
        })
        .unwrap_or(0)
}

fn parse_tools(value: Option<&Value>) -> Result<Vec<ToolDefinitionIr>, BridgeError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let tools: Vec<Option<ToolDefinitionIr>> = value
        .as_array()
        .ok_or(BridgeError::InvalidRequest)?
        .iter()
        .map(|value| {
            let object = object(value)?;
            match object.get("type").and_then(Value::as_str) {
                // namespace/web_search are Codex client-owned capabilities with
                // no Chat representation; provider conversion only forwards
                // function-shaped tools (function/custom).
                Some("namespace" | "web_search") => return Ok(None),
                Some("custom") | Some("function") => {}
                _ => {
                    return Err(BridgeError::Unsupported {
                        field: "tools.type".to_string(),
                    })
                }
            }
            let is_custom = object.get("type").and_then(Value::as_str) == Some("custom");
            if is_custom {
                // Codex custom tools carry a client-owned `format` field and no
                // `parameters` schema; neither is forwarded upstream. Function
                // tools keep the strict whitelist with required `parameters`.
                reject_unknown_fields(
                    object,
                    &["type", "name", "description", "format", "parameters"],
                )?;
            } else {
                reject_unknown_fields(
                    object,
                    &["type", "name", "description", "strict", "parameters"],
                )?;
            }
            let parameters = match object.get("parameters").cloned() {
                Some(parameters) if parameters.is_object() => parameters,
                Some(_) => return Err(BridgeError::InvalidRequest),
                None if is_custom => json!({"type": "object"}),
                None => return Err(BridgeError::InvalidRequest),
            };
            let strict = if is_custom {
                None
            } else if let Some(value) = object.get("strict") {
                Some(value.as_bool().ok_or(BridgeError::InvalidRequest)?)
            } else {
                None
            };
            Ok(Some(ToolDefinitionIr {
                name: required_string(object, "name")?,
                description: optional_string(object, "description")?,
                input_schema: parameters,
                strict,
            }))
        })
        .collect::<Result<_, _>>()?;
    Ok(tools.into_iter().flatten().collect())
}

fn parse_tool_choice(value: Option<&Value>) -> Result<Option<ToolChoiceIr>, BridgeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some(value) = value.as_str() {
        return Ok(Some(match value {
            "auto" => ToolChoiceIr::Auto,
            "required" => ToolChoiceIr::Any,
            "none" => ToolChoiceIr::None,
            _ => {
                return Err(BridgeError::Unsupported {
                    field: "tool_choice".to_string(),
                })
            }
        }));
    }
    let object = object(value)?;
    reject_unknown_fields(object, &["type", "name"])?;
    if object.get("type").and_then(Value::as_str) != Some("function") {
        return Err(BridgeError::Unsupported {
            field: "tool_choice.type".to_string(),
        });
    }
    Ok(Some(ToolChoiceIr::Tool {
        name: required_string(object, "name")?,
    }))
}

pub(super) fn encode_request(ir: &RequestIr, model: &str) -> Result<Value, BridgeError> {
    validate_target_representability(ir, WireProtocol::OpenAiResponses)?;
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert("stream".to_string(), Value::Bool(ir.stream));
    let instructions = instruction_value(ir)?;
    let instruction_count = ir
        .extensions
        .get(EXT_INSTRUCTION_COUNT)
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if let Some(instructions) = instructions {
        body.insert("instructions".to_string(), instructions);
    }
    let mut input = Vec::new();
    let mut skipped_instructions = 0;
    let mut reasoning_index = 0;
    let reasoning_ids = ir
        .extensions
        .get(EXT_RESPONSES_REASONING_IDS)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let reasoning_metadata =
        if let Some(value) = ir.extensions.get(EXT_RESPONSES_REASONING_ITEM_METADATA) {
            Some(value.as_array().ok_or(BridgeError::InvalidRequest)?)
        } else {
            None
        };
    for message in &ir.messages {
        if message.role == RoleIr::System
            && (instruction_count == 0 || skipped_instructions < instruction_count)
        {
            skipped_instructions += 1;
            continue;
        }
        encode_message(
            message,
            &mut input,
            &mut reasoning_index,
            &reasoning_ids,
            reasoning_metadata.map(Vec::as_slice),
        )?;
    }
    if let Some(reasoning_metadata) = reasoning_metadata {
        if reasoning_metadata.len() != reasoning_index {
            return Err(BridgeError::InvalidRequest);
        }
    }
    body.insert("input".to_string(), Value::Array(input));
    if !ir.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            Value::Array(ir.tools.iter().map(encode_tool).collect::<Result<_, _>>()?),
        );
    }
    if let Some(choice) = &ir.tool_choice {
        body.insert("tool_choice".to_string(), encode_tool_choice(choice)?);
    }
    if let Some(value) = ir.generation.max_tokens {
        body.insert("max_output_tokens".to_string(), json!(value));
    }
    if let Some(value) = ir.generation.temperature {
        body.insert("temperature".to_string(), json!(value));
    }
    if let Some(value) = ir.generation.top_p {
        body.insert("top_p".to_string(), json!(value));
    }
    if !ir.generation.stop_sequences.is_empty() {
        body.insert("stop".to_string(), json!(ir.generation.stop_sequences));
    }
    if let Some(value) = &ir.generation.reasoning_effort {
        body.insert("reasoning".to_string(), json!({ "effort": value }));
    }
    let parallel_tool_calls = match (
        ir.extensions.get(EXT_CHAT_PARALLEL_TOOL_CALLS),
        ir.extensions.get(EXT_RESPONSES_PARALLEL_TOOL_CALLS),
    ) {
        (Some(chat), Some(responses)) if chat != responses => {
            return Err(BridgeError::InvalidRequest)
        }
        (Some(value), _) | (_, Some(value)) => Some(value),
        (None, None) => None,
    };
    if let Some(value) = parallel_tool_calls {
        body.insert("parallel_tool_calls".to_string(), value.clone());
    }
    if let Some(value) = &ir.metadata {
        body.insert("metadata".to_string(), value.clone());
    }
    if let Some(value) = ir.extensions.get(EXT_RESPONSES_PREVIOUS_RESPONSE_ID) {
        body.insert("previous_response_id".to_string(), value.clone());
    }
    // 同协议（Responses→Responses）必须回传已接受的扩展字段：
    // accept-then-drop 会翻转 store 语义、断掉 include 加密推理、丢 text.format 契约。
    if ir.protocol == WireProtocol::OpenAiResponses {
        for (key, field) in [
            (EXT_RESPONSES_STORE, "store"),
            (EXT_RESPONSES_INCLUDE, "include"),
            (EXT_RESPONSES_TEXT, "text"),
            (EXT_RESPONSES_STREAM_OPTIONS, "stream_options"),
            (EXT_RESPONSES_SERVICE_TIER, "service_tier"),
            (EXT_RESPONSES_REQUEST_CONVERSATION, "conversation"),
            (EXT_RESPONSES_CONTEXT_MANAGEMENT, "context_management"),
            (EXT_RESPONSES_PROMPT_CACHE_KEY, "prompt_cache_key"),
            (EXT_RESPONSES_CLIENT_METADATA, "client_metadata"),
        ] {
            if let Some(value) = ir.extensions.get(key) {
                body.insert(field.to_string(), value.clone());
            }
        }
    }
    Ok(Value::Object(body))
}

pub(super) fn decode_response(body: Value) -> Result<ResponseIr, BridgeError> {
    let object = body.as_object().ok_or(BridgeError::InvalidUpstream)?;
    if object.get("error").is_some_and(|value| !value.is_null()) {
        return Err(BridgeError::InvalidUpstream);
    }
    reject_unknown_fields(
        object,
        &[
            "id",
            "object",
            "background",
            "created_at",
            "completed_at",
            "conversation",
            "model",
            "moderation",
            "status",
            "output",
            "output_text",
            "usage",
            "error",
            "incomplete_details",
            "instructions",
            "max_output_tokens",
            "max_tool_calls",
            "metadata",
            "parallel_tool_calls",
            "previous_response_id",
            "prompt",
            "prompt_cache_key",
            "prompt_cache_options",
            "prompt_cache_retention",
            "reasoning",
            "safety_identifier",
            "service_tier",
            "store",
            "temperature",
            "text",
            "tool_choice",
            "tools",
            "top_logprobs",
            "top_p",
            "truncation",
            "user",
        ],
    )
    .map_err(map_upstream_error)?;
    if object.get("object").and_then(Value::as_str) != Some("response") {
        return Err(BridgeError::InvalidUpstream);
    }
    if object
        .get("created_at")
        .is_some_and(|value| value.as_u64().is_none())
    {
        return Err(BridgeError::InvalidUpstream);
    }
    validate_standard_response_fields(object)?;
    let id = response_required_string(object, "id")?;
    let model = response_required_string(object, "model")?;
    let status = response_required_string(object, "status")?;
    if !matches!(status.as_str(), "completed" | "incomplete") {
        return Err(BridgeError::InvalidUpstream);
    }
    let mut extensions = BTreeMap::new();
    decode_response_metadata(object, &status, &mut extensions)?;
    let output = required_array(object, "output")?;
    let mut content = Vec::new();
    let mut message_item_id = None;
    let mut reasoning_item_ids = Vec::new();
    let mut reasoning_item_contents = Map::new();
    let mut output_item_statuses = Map::new();
    let mut message_items = 0;
    let mut seen_output_ids = std::collections::BTreeSet::new();
    for item in output {
        let item_object = item.as_object().ok_or(BridgeError::InvalidUpstream)?;
        let item_type = response_required_string(item_object, "type")?;
        match item_type.as_str() {
            "message" => {
                message_items += 1;
                if message_items > 1 {
                    return Err(BridgeError::Unsupported {
                        field: "responses.output.message_item_id".to_string(),
                    });
                }
                reject_unknown_fields(item_object, &["id", "type", "role", "status", "content"])
                    .map_err(map_upstream_error)?;
                if item_object.get("role").and_then(Value::as_str) != Some("assistant") {
                    return Err(BridgeError::InvalidUpstream);
                }
                if let Some(item_id) = item_object.get("id") {
                    let item_id = item_id
                        .as_str()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or(BridgeError::InvalidUpstream)?;
                    if !seen_output_ids.insert(item_id.to_string()) {
                        return Err(BridgeError::ToolState);
                    }
                    message_item_id = Some(item_id.to_string());
                }
                capture_output_item_status(
                    item_object,
                    &status,
                    message_item_id.as_deref(),
                    &mut output_item_statuses,
                )?;
                let parts = required_array(item_object, "content")?;
                if parts.is_empty() {
                    return Err(BridgeError::InvalidUpstream);
                }
                for part in parts {
                    let part_object = part.as_object().ok_or(BridgeError::InvalidUpstream)?;
                    reject_unknown_fields(part_object, &["type", "text", "annotations"])
                        .map_err(map_upstream_error)?;
                    if part_object.get("type").and_then(Value::as_str) != Some("output_text") {
                        return Err(BridgeError::Unsupported {
                            field: "responses.output.message.content.type".to_string(),
                        });
                    }
                    if let Some(annotations) = part_object.get("annotations") {
                        let annotations =
                            annotations.as_array().ok_or(BridgeError::InvalidUpstream)?;
                        if !annotations.is_empty() {
                            return Err(BridgeError::Unsupported {
                                field: "responses.output.message.content.annotations".to_string(),
                            });
                        }
                    }
                    content.push(ContentIr::Text(response_required_string(
                        part_object,
                        "text",
                    )?));
                }
            }
            "function_call" => {
                reject_unknown_fields(
                    item_object,
                    &["id", "type", "call_id", "name", "arguments", "status"],
                )
                .map_err(map_upstream_error)?;
                let item_id = response_required_string(item_object, "id")?;
                if !seen_output_ids.insert(item_id.clone()) {
                    return Err(BridgeError::ToolState);
                }
                capture_output_item_status(
                    item_object,
                    &status,
                    Some(item_id.as_str()),
                    &mut output_item_statuses,
                )?;
                let arguments = super::request::parse_arguments(
                    item_object
                        .get("arguments")
                        .ok_or(BridgeError::InvalidUpstream)?,
                )
                .map_err(map_upstream_error)?;
                content.push(ContentIr::ToolUse(super::request::tool_call(
                    response_required_string(item_object, "call_id")?,
                    Some(item_id),
                    content
                        .iter()
                        .filter(|item| matches!(item, ContentIr::ToolUse(_)))
                        .count(),
                    response_required_string(item_object, "name")?,
                    arguments,
                )));
            }
            "reasoning" => {
                reject_unknown_fields(
                    item_object,
                    &[
                        "id",
                        "type",
                        "status",
                        "summary",
                        "content",
                        "encrypted_content",
                    ],
                )
                .map_err(map_upstream_error)?;
                let item_id = response_required_string(item_object, "id")?;
                if !seen_output_ids.insert(item_id.clone()) {
                    return Err(BridgeError::ToolState);
                }
                capture_output_item_status(
                    item_object,
                    &status,
                    Some(item_id.as_str()),
                    &mut output_item_statuses,
                )?;
                if let Some(item_content) = item_object.get("content") {
                    validate_reasoning_content(item_content)?;
                    reasoning_item_contents.insert(item_id.clone(), item_content.clone());
                }
                reasoning_item_ids.push(Value::String(item_id));
                content.push(parse_reasoning(item_object).map_err(map_upstream_error)?);
            }
            other => {
                return Err(BridgeError::Unsupported {
                    field: format!("responses.output.type:{other}"),
                })
            }
        }
    }
    let output_text = content
        .iter()
        .filter_map(|item| match item {
            ContentIr::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(value) = object.get("output_text") {
        if !value.is_null() && value.as_str() != Some(output_text.as_str()) {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    let usage = decode_usage(object.get("usage"))?;
    if let Some(item_id) = message_item_id {
        extensions.insert(EXT_RESPONSES_MESSAGE_ITEM_ID.to_string(), json!(item_id));
    }
    if !reasoning_item_ids.is_empty() {
        extensions.insert(
            EXT_RESPONSES_REASONING_ITEM_IDS.to_string(),
            Value::Array(reasoning_item_ids),
        );
    }
    if !output_item_statuses.is_empty() {
        extensions.insert(
            EXT_RESPONSES_OUTPUT_ITEM_STATUSES.to_string(),
            Value::Object(output_item_statuses),
        );
    }
    if !reasoning_item_contents.is_empty() {
        extensions.insert(
            EXT_RESPONSES_REASONING_ITEM_CONTENT.to_string(),
            Value::Object(reasoning_item_contents),
        );
    }
    let has_tool_call = content
        .iter()
        .any(|item| matches!(item, ContentIr::ToolUse(_)));
    let incomplete_reason = extensions
        .get(EXT_RESPONSES_INCOMPLETE_DETAILS)
        .and_then(Value::as_object)
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str);
    let (finish_reason, stop_reason) = if has_tool_call {
        ("tool_calls", "tool_use")
    } else if incomplete_reason == Some("content_filter") {
        ("content_filter", "refusal")
    } else if status == "incomplete" {
        ("length", "max_tokens")
    } else {
        ("stop", "end_turn")
    };
    Ok(ResponseIr {
        meta: ResponseMetaIr {
            id: Some(id),
            model: Some(model),
        },
        content,
        usage,
        completion: CompletionIr {
            finish_reason: Some(finish_reason.to_string()),
            stop_reason: Some(stop_reason.to_string()),
            status: Some(status),
            error: None,
        },
        extensions,
    })
}

pub(super) fn encode_response(ir: &ResponseIr) -> Result<Value, BridgeError> {
    let model = response_model(ir)?;
    let response_id = target_response_id(ir, WireProtocol::OpenAiResponses);
    let reasoning_ids = ir
        .extensions
        .get(EXT_RESPONSES_REASONING_ITEM_IDS)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let item_statuses = ir
        .extensions
        .get(EXT_RESPONSES_OUTPUT_ITEM_STATUSES)
        .and_then(Value::as_object);
    let reasoning_item_contents = ir
        .extensions
        .get(EXT_RESPONSES_REASONING_ITEM_CONTENT)
        .and_then(Value::as_object);
    let mut reasoning_index = 0;
    let mut message_item_index = 0;
    let mut message_text = Vec::new();
    let mut output = Vec::new();
    for content in &ir.content {
        match content {
            ContentIr::Text(text) => message_text.push(json!({
                "type": "output_text",
                "text": text,
                "annotations": []
            })),
            ContentIr::Thinking { text, signature } => {
                if flush_message_output(
                    &mut output,
                    &mut message_text,
                    ir,
                    message_item_index,
                    item_statuses,
                )? {
                    message_item_index += 1;
                }
                let mut reasoning = encode_reasoning(
                    text,
                    signature.as_deref(),
                    reasoning_index,
                    &reasoning_ids,
                    None,
                )?;
                apply_output_item_status(&mut reasoning, item_statuses);
                apply_reasoning_item_content(&mut reasoning, reasoning_item_contents);
                output.push(reasoning);
                reasoning_index += 1;
            }
            ContentIr::RedactedThinking { data } => {
                if flush_message_output(
                    &mut output,
                    &mut message_text,
                    ir,
                    message_item_index,
                    item_statuses,
                )? {
                    message_item_index += 1;
                }
                let mut reasoning =
                    encode_reasoning("", Some(data), reasoning_index, &reasoning_ids, None)?;
                apply_output_item_status(&mut reasoning, item_statuses);
                apply_reasoning_item_content(&mut reasoning, reasoning_item_contents);
                output.push(reasoning);
                reasoning_index += 1;
            }
            ContentIr::ToolUse(call) => {
                if flush_message_output(
                    &mut output,
                    &mut message_text,
                    ir,
                    message_item_index,
                    item_statuses,
                )? {
                    message_item_index += 1;
                }
                let mut function_call = encode_function_call(call)?;
                apply_output_item_status(&mut function_call, item_statuses);
                output.push(function_call);
            }
            ContentIr::Image(_) | ContentIr::Document(_) => {
                return Err(BridgeError::Unsupported {
                    field: "response.content.media".to_string(),
                })
            }
            ContentIr::ToolResult(_) => {
                return Err(BridgeError::Unsupported {
                    field: "response.tool_result".to_string(),
                })
            }
        }
    }
    flush_message_output(
        &mut output,
        &mut message_text,
        ir,
        message_item_index,
        item_statuses,
    )?;
    let status = response_status(ir)?;
    let output_text = ir
        .content
        .iter()
        .filter_map(|item| match item {
            ContentIr::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let content_filter = content_filter_semantics(ir);
    let incomplete_details = ir
        .extensions
        .get(EXT_RESPONSES_INCOMPLETE_DETAILS)
        .cloned()
        .or_else(|| content_filter.then(|| json!({"reason": "content_filter"})))
        .unwrap_or(Value::Null);
    let mut result = json!({
        "id": response_id,
        "object": "response",
        "created_at": generated_created_at(),
        "model": model,
        "status": status,
        "output": output,
        "output_text": output_text,
        "usage": encode_usage(ir.usage.as_ref())?,
        "error": null,
        "incomplete_details": incomplete_details
    });
    for (field, extension) in [
        ("completed_at", EXT_RESPONSES_COMPLETED_AT),
        ("instructions", EXT_RESPONSES_INSTRUCTIONS),
        ("reasoning", EXT_RESPONSES_REASONING),
        ("conversation", EXT_RESPONSES_CONVERSATION),
        ("moderation", EXT_RESPONSES_MODERATION),
        ("prompt_cache_options", EXT_RESPONSES_PROMPT_CACHE_OPTIONS),
    ] {
        if let Some(value) = ir.extensions.get(extension) {
            result[field] = value.clone();
        }
    }
    Ok(result)
}

pub(super) fn encode_error_response(ir: &ResponseIr) -> Result<Value, BridgeError> {
    let model = error_model(ir);
    Ok(json!({
        "id": target_response_id(ir, WireProtocol::OpenAiResponses),
        "object": "response",
        "created_at": generated_created_at(),
        "model": model,
        "status": "failed",
        "output": [],
        "output_text": "",
        "usage": null,
        "error": {
            "code": "invalid_upstream",
            "message": safe_error_message()
        },
        "incomplete_details": null
    }))
}

pub(super) fn decode_stream_frame(
    frame: &SseFrame,
    state: &mut StreamState,
) -> Result<Vec<StreamEventIr>, BridgeError> {
    let value: Value =
        serde_json::from_str(&frame.data).map_err(|_| BridgeError::InvalidUpstream)?;
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    let kind = frame
        .event
        .as_deref()
        .or_else(|| object.get("type").and_then(Value::as_str))
        .ok_or(BridgeError::InvalidUpstream)?;
    if !matches!(kind, "response.created" | "response.in_progress")
        && object.contains_key("response_id")
    {
        validate_optional_response_id(object, state)?;
    }
    let mut events = Vec::new();
    match kind {
        "response.created" | "response.in_progress" => {
            reject_unknown_fields(object, &["type", "response"]).map_err(map_upstream_error)?;
            let response = object
                .get("response")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(
                response,
                &[
                    "id",
                    "object",
                    "created_at",
                    "model",
                    "status",
                    "output",
                    "usage",
                    "error",
                    "incomplete_details",
                ],
            )
            .map_err(map_upstream_error)?;
            let id = stream_required_string(response, "id")?;
            let model = stream_required_string(response, "model")?;
            if stream_required_string(response, "status")? != "in_progress" {
                return Err(BridgeError::InvalidUpstream);
            }
            if !state.is_started() {
                let event = StreamEventIr::Started(ResponseMetaIr {
                    id: Some(id),
                    model: Some(model),
                });
                state.apply_event(&event)?;
                events.push(event);
            } else {
                state.require_response_identity(&id, &model)?;
            }
        }
        "response.output_item.added" => {
            reject_unknown_fields(object, &["type", "response_id", "output_index", "item"])
                .map_err(map_upstream_error)?;
            state
                .is_started()
                .then_some(())
                .ok_or(BridgeError::ToolState)?;
            validate_optional_response_id(object, state)?;
            let item = object
                .get("item")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            let wire_index = stream_required_usize(object, "output_index")?;
            reject_stream_item_fields(item).map_err(map_upstream_error)?;
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    state.require_next_output_index(wire_index)?;
                    state.register_wire_block(WireProtocol::OpenAiResponses, wire_index, 2)?;
                    let call_id = stream_required_string(item, "call_id")?;
                    let item_id = stream_required_string(item, "id")?;
                    let status = item
                        .get("status")
                        .map(|value| value.as_str().ok_or(BridgeError::InvalidUpstream))
                        .transpose()?
                        .unwrap_or("in_progress");
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(|value| {
                            let parsed: Value = serde_json::from_str(value)
                                .map_err(|_| BridgeError::InvalidUpstream)?;
                            if !parsed.is_object() {
                                return Err(BridgeError::InvalidUpstream);
                            }
                            Ok(parsed)
                        })
                        .transpose()?
                        .unwrap_or_else(|| json!({}));
                    let event = StreamEventIr::ToolCallStarted(ToolCallIr {
                        call_id: call_id.clone(),
                        item_id: Some(item_id.clone()),
                        index: state.next_tool_index(),
                        name: stream_required_string(item, "name")?,
                        arguments,
                    });
                    state.register_wire_tool_with_item(
                        WireProtocol::OpenAiResponses,
                        wire_index,
                        &call_id,
                        &item_id,
                    )?;
                    state.register_wire_item(
                        WireProtocol::OpenAiResponses,
                        wire_index,
                        &item_id,
                        status,
                    )?;
                    state.apply_event(&event)?;
                    events.push(event);
                }
                Some("message") => {
                    state.require_next_output_index(wire_index)?;
                    state.register_wire_block(WireProtocol::OpenAiResponses, wire_index, 0)?;
                    if item.get("role").and_then(Value::as_str) != Some("assistant") {
                        return Err(BridgeError::InvalidUpstream);
                    }
                    let item_id = stream_required_string(item, "id")?;
                    let status = item
                        .get("status")
                        .map(|value| value.as_str().ok_or(BridgeError::InvalidUpstream))
                        .transpose()?
                        .unwrap_or("in_progress");
                    state.register_wire_item(
                        WireProtocol::OpenAiResponses,
                        wire_index,
                        &item_id,
                        status,
                    )?;
                }
                Some("reasoning") => {
                    state.require_next_output_index(wire_index)?;
                    state.register_wire_block(WireProtocol::OpenAiResponses, wire_index, 1)?;
                    let item_id = stream_required_string(item, "id")?;
                    let status = item
                        .get("status")
                        .map(|value| value.as_str().ok_or(BridgeError::InvalidUpstream))
                        .transpose()?
                        .unwrap_or("in_progress");
                    state.register_wire_item(
                        WireProtocol::OpenAiResponses,
                        wire_index,
                        &item_id,
                        status,
                    )?;
                }
                Some(other) => {
                    return Err(BridgeError::Unsupported {
                        field: format!("responses.stream.output_item.type:{other}"),
                    })
                }
                None => return Err(BridgeError::InvalidUpstream),
            }
        }
        "response.output_text.delta" => {
            reject_unknown_fields(
                object,
                &["type", "item_id", "output_index", "content_index", "delta"],
            )
            .map_err(map_upstream_error)?;
            let output_index = stream_required_usize(object, "output_index")?;
            state.require_wire_block_kind(WireProtocol::OpenAiResponses, output_index, 0)?;
            state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
            let item_id = stream_required_string(object, "item_id")?;
            state.require_wire_item(WireProtocol::OpenAiResponses, output_index, &item_id)?;
            let event = StreamEventIr::TextDelta {
                text: stream_content_string(object, "delta")?,
            };
            state.apply_event(&event)?;
            events.push(event);
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_content.delta" => {
            reject_unknown_fields(
                object,
                &["type", "item_id", "output_index", "summary_index", "delta"],
            )
            .map_err(map_upstream_error)?;
            let output_index = stream_required_usize(object, "output_index")?;
            state.require_wire_block_kind(WireProtocol::OpenAiResponses, output_index, 1)?;
            state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
            let item_id = stream_required_string(object, "item_id")?;
            state.require_wire_item(WireProtocol::OpenAiResponses, output_index, &item_id)?;
            let event = StreamEventIr::ReasoningDelta {
                text: stream_content_string(object, "delta")?,
            };
            state.apply_event(&event)?;
            events.push(event);
        }
        "response.function_call_arguments.delta" => {
            reject_unknown_fields(object, &["type", "item_id", "output_index", "delta"])
                .map_err(map_upstream_error)?;
            let output_index = stream_required_usize(object, "output_index")?;
            state.require_wire_block_kind(WireProtocol::OpenAiResponses, output_index, 2)?;
            state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
            let item_id = stream_required_string(object, "item_id")?;
            state.require_wire_tool_item(WireProtocol::OpenAiResponses, output_index, &item_id)?;
            let event = StreamEventIr::ToolCallArgumentsDelta {
                call_id: state.tool_call_id_for_item(&item_id)?,
                delta: stream_required_string(object, "delta")?,
            };
            state.apply_event(&event)?;
            events.push(event);
        }
        "response.output_item.done" => {
            reject_unknown_fields(object, &["type", "response_id", "output_index", "item"])
                .map_err(map_upstream_error)?;
            let output_index = stream_required_usize(object, "output_index")?;
            validate_optional_response_id(object, state)?;
            let item = object
                .get("item")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            reject_stream_item_fields(item).map_err(map_upstream_error)?;
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    state.require_wire_block_kind(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        2,
                    )?;
                    let item_id = stream_required_string(item, "id")?;
                    state.require_wire_tool_item(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        &item_id,
                    )?;
                    if stream_required_string(item, "status")? != "completed" {
                        return Err(BridgeError::ToolState);
                    }
                    state.transition_wire_item(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        "in_progress",
                        "completed",
                    )?;
                    let call_id = state.tool_call_id_for_item(&item_id)?;
                    if item.contains_key("call_id")
                        && stream_required_string(item, "call_id")? != call_id
                    {
                        return Err(BridgeError::ToolState);
                    }
                    let event = StreamEventIr::ToolCallFinished { call_id };
                    state.apply_event(&event)?;
                    state.close_wire_block(WireProtocol::OpenAiResponses, output_index)?;
                    events.push(event);
                }
                Some("message") => {
                    state.require_wire_block_kind(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        0,
                    )?;
                    state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
                    let item_id = stream_required_string(item, "id")?;
                    state.require_wire_item(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        &item_id,
                    )?;
                    if item
                        .get("role")
                        .is_some_and(|role| role.as_str() != Some("assistant"))
                    {
                        return Err(BridgeError::InvalidUpstream);
                    }
                    if stream_required_string(item, "status")? != "completed" {
                        return Err(BridgeError::ToolState);
                    }
                    state.transition_wire_item(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        "in_progress",
                        "completed",
                    )?;
                    state.close_wire_block(WireProtocol::OpenAiResponses, output_index)?;
                }
                Some("reasoning") => {
                    state.require_wire_block_kind(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        1,
                    )?;
                    state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
                    let item_id = stream_required_string(item, "id")?;
                    state.require_wire_item(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        &item_id,
                    )?;
                    if stream_required_string(item, "status")? != "completed" {
                        return Err(BridgeError::ToolState);
                    }
                    state.transition_wire_item(
                        WireProtocol::OpenAiResponses,
                        output_index,
                        "in_progress",
                        "completed",
                    )?;
                    state.close_wire_block(WireProtocol::OpenAiResponses, output_index)?;
                }
                Some(other) => {
                    return Err(BridgeError::Unsupported {
                        field: format!("responses.stream.output_item.type:{other}"),
                    })
                }
                None => return Err(BridgeError::InvalidUpstream),
            }
        }
        "response.function_call_arguments.done" => {
            reject_unknown_fields(object, &["type", "item_id", "output_index", "arguments"])
                .map_err(map_upstream_error)?;
            let output_index = stream_required_usize(object, "output_index")?;
            state.require_wire_block_kind(WireProtocol::OpenAiResponses, output_index, 2)?;
            state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
            let item_id = stream_required_string(object, "item_id")?;
            state.require_wire_tool_item(WireProtocol::OpenAiResponses, output_index, &item_id)?;
            let arguments = stream_required_string(object, "arguments")?;
            let parsed: Value =
                serde_json::from_str(&arguments).map_err(|_| BridgeError::InvalidUpstream)?;
            if !parsed.is_object() {
                return Err(BridgeError::InvalidUpstream);
            }
            let call_id = state.tool_call_id_for_item(&item_id)?;
            state.set_tool_arguments_final(&call_id, &arguments)?;
        }
        "response.output_text.done"
        | "response.content_part.added"
        | "response.content_part.done" => {
            reject_unknown_fields(
                object,
                &[
                    "type",
                    "item_id",
                    "output_index",
                    "content_index",
                    "text",
                    "part",
                ],
            )
            .map_err(map_upstream_error)?;
            if let Some(part) = object.get("part") {
                let part = part.as_object().ok_or(BridgeError::InvalidUpstream)?;
                reject_unknown_fields(part, &["type", "text", "annotations"])
                    .map_err(map_upstream_error)?;
            }
            let output_index = stream_required_usize(object, "output_index")?;
            state.require_wire_block_kind(WireProtocol::OpenAiResponses, output_index, 0)?;
            state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
            let item_id = stream_required_string(object, "item_id")?;
            state.require_wire_item(WireProtocol::OpenAiResponses, output_index, &item_id)?;
        }
        "response.reasoning_summary_part.added"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_content.delta.done" => {
            reject_unknown_fields(
                object,
                &["type", "item_id", "output_index", "summary_index", "part"],
            )
            .map_err(map_upstream_error)?;
            let output_index = stream_required_usize(object, "output_index")?;
            state.require_wire_block_kind(WireProtocol::OpenAiResponses, output_index, 1)?;
            state.require_wire_block_open(WireProtocol::OpenAiResponses, output_index)?;
            let item_id = stream_required_string(object, "item_id")?;
            state.require_wire_item(WireProtocol::OpenAiResponses, output_index, &item_id)?;
        }
        "response.completed" | "response.incomplete" => {
            reject_unknown_fields(object, &["type", "response"]).map_err(map_upstream_error)?;
            let response = object
                .get("response")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(
                response,
                &[
                    "id",
                    "object",
                    "created_at",
                    "completed_at",
                    "model",
                    "status",
                    "output",
                    "output_text",
                    "usage",
                    "error",
                    "incomplete_details",
                ],
            )
            .map_err(map_upstream_error)?;
            let response_id = stream_required_string(response, "id")?;
            let model = stream_required_string(response, "model")?;
            state.require_response_identity(&response_id, &model)?;
            state.require_wire_blocks_closed(WireProtocol::OpenAiResponses)?;
            if let Some(usage) = response.get("usage") {
                if !usage.is_null() {
                    let event = StreamEventIr::Usage(decode_stream_usage(usage)?);
                    state.apply_event(&event)?;
                    events.push(event);
                }
            }
            let status = response
                .get("status")
                .and_then(Value::as_str)
                .filter(|status| {
                    matches!(
                        (kind, *status),
                        ("response.completed", "completed") | ("response.incomplete", "incomplete")
                    )
                })
                .ok_or(BridgeError::InvalidUpstream)?;
            let completion = responses_completion(
                status,
                response.get("incomplete_details"),
                state.next_tool_index() > 0,
            )?;
            let event = StreamEventIr::Completed(completion);
            state.apply_event(&event)?;
            events.push(event);
        }
        "response.failed" => {
            reject_unknown_fields(object, &["type", "response", "error"])
                .map_err(map_upstream_error)?;
            if let Some(response) = object.get("response") {
                let response = response.as_object().ok_or(BridgeError::InvalidUpstream)?;
                reject_unknown_fields(
                    response,
                    &[
                        "id",
                        "object",
                        "created_at",
                        "model",
                        "status",
                        "output",
                        "output_text",
                        "usage",
                        "error",
                        "incomplete_details",
                    ],
                )
                .map_err(map_upstream_error)?;
                let response_id = stream_required_string(response, "id")?;
                let model = stream_required_string(response, "model")?;
                state.require_response_identity(&response_id, &model)?;
                if stream_required_string(response, "status")? != "failed" {
                    return Err(BridgeError::InvalidUpstream);
                }
            }
            let event = StreamEventIr::Failed(BridgeError::InvalidUpstream);
            state.apply_event(&event)?;
            events.push(event);
        }
        "response.refusal.delta" => {
            reject_unknown_fields(
                object,
                &["type", "item_id", "output_index", "content_index", "delta"],
            )
            .map_err(map_upstream_error)?;
            return Err(BridgeError::Unsupported {
                field: "responses.stream.refusal".to_string(),
            });
        }
        other => {
            return Err(BridgeError::Unsupported {
                field: format!("responses.stream.event:{other}"),
            })
        }
    }
    Ok(events)
}

fn reject_stream_item_fields(item: &Map<String, Value>) -> Result<(), BridgeError> {
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .ok_or(BridgeError::InvalidUpstream)?;
    let fields = match item_type {
        "function_call" => &["id", "type", "call_id", "name", "arguments", "status"][..],
        "message" => &["id", "type", "role", "status", "content"][..],
        "reasoning" => &[
            "id",
            "type",
            "status",
            "summary",
            "encrypted_content",
            "content",
        ][..],
        _ => return Ok(()),
    };
    reject_unknown_fields(item, fields)
}

pub(super) fn encode_stream_event(
    event: &StreamEventIr,
    state: &mut StreamState,
) -> Result<Vec<SseFrame>, BridgeError> {
    let mut frames = Vec::new();
    match event {
        StreamEventIr::Started(_) => {
            if state.encoded_started() {
                return Err(BridgeError::ToolState);
            }
            state.mark_encoded_started();
            let id = state.encoded_message_id();
            let model = state
                .meta()
                .model
                .clone()
                .ok_or(BridgeError::InvalidUpstream)?;
            frames.push(responses_frame(
                "response.created",
                json!({ "type": "response.created", "response": { "id": id, "object": "response", "status": "in_progress", "model": model, "output": [] } }),
            ));
            frames.push(responses_frame(
                "response.in_progress",
                json!({ "type": "response.in_progress", "response": { "id": state.encoded_message_id(), "object": "response", "status": "in_progress", "model": model, "output": [] } }),
            ));
        }
        StreamEventIr::TextDelta { text } => {
            let item_id = state.encoded_message_item_id();
            let output_index = state.encoded_block_index("responses:message");
            if !state.encoded_text() {
                frames.push(responses_frame(
                    "response.output_item.added",
                    json!({ "type": "response.output_item.added", "output_index": output_index, "item": { "id": item_id, "type": "message", "role": "assistant", "status": "in_progress", "content": [] } }),
                ));
                frames.push(responses_frame(
                    "response.content_part.added",
                    json!({ "type": "response.content_part.added", "item_id": item_id, "output_index": output_index, "content_index": 0, "part": { "type": "output_text", "text": "", "annotations": [] } }),
                ));
                state.mark_encoded_text();
            }
            state.append_encoded_text(text);
            frames.push(responses_frame(
                "response.output_text.delta",
                json!({ "type": "response.output_text.delta", "item_id": item_id, "output_index": output_index, "content_index": 0, "delta": text }),
            ));
        }
        StreamEventIr::ReasoningDelta { text } => {
            let item_id = state.encoded_reasoning_id();
            let output_index = state.encoded_block_index("responses:reasoning");
            if !state.encoded_reasoning() {
                frames.push(responses_frame(
                    "response.output_item.added",
                    json!({ "type": "response.output_item.added", "output_index": output_index, "item": { "id": item_id, "type": "reasoning", "status": "in_progress", "summary": [] } }),
                ));
                state.mark_encoded_reasoning();
            }
            state.append_encoded_reasoning_summary(text);
            frames.push(responses_frame(
                "response.reasoning_summary_text.delta",
                json!({ "type": "response.reasoning_summary_text.delta", "item_id": item_id, "output_index": output_index, "summary_index": 0, "delta": text }),
            ));
        }
        StreamEventIr::ToolCallStarted(call) => {
            if state.encoded_tool(&call.call_id) {
                return Err(BridgeError::ToolState);
            }
            let item_id = state.encoded_item_id(&call.call_id, call.item_id.as_deref());
            let output_index =
                state.encoded_block_index(&format!("responses:tool:{}", call.call_id));
            let arguments = state.tool_state(&call.call_id)?.arguments().to_string();
            state.register_wire_tool(WireProtocol::OpenAiResponses, call.index, &call.call_id)?;
            frames.push(responses_frame(
                "response.output_item.added",
                json!({ "type": "response.output_item.added", "output_index": output_index, "item": { "id": item_id, "type": "function_call", "status": "in_progress", "call_id": call.call_id, "name": call.name, "arguments": arguments } }),
            ));
            state.mark_encoded_tool(&call.call_id);
        }
        StreamEventIr::ToolCallArgumentsDelta { call_id, delta } => {
            let item_id = state.encoded_item_id(call_id, None);
            let index = state.encoded_block_index(&format!("responses:tool:{call_id}"));
            frames.push(responses_frame(
                "response.function_call_arguments.delta",
                json!({ "type": "response.function_call_arguments.delta", "item_id": item_id, "output_index": index, "delta": delta }),
            ));
        }
        StreamEventIr::ToolCallFinished { call_id } => {
            if state.encoded_tool_stop(call_id) {
                return Err(BridgeError::ToolState);
            }
            let item_id = state.encoded_item_id(call_id, None);
            let index = state.encoded_block_index(&format!("responses:tool:{call_id}"));
            let arguments = state.tool_state(call_id)?.arguments().to_string();
            let name = state.tool_state(call_id)?.name().to_string();
            frames.push(responses_frame(
                "response.function_call_arguments.done",
                json!({ "type": "response.function_call_arguments.done", "item_id": item_id, "output_index": index, "arguments": arguments }),
            ));
            frames.push(responses_frame(
                "response.output_item.done",
                json!({ "type": "response.output_item.done", "output_index": index, "item": { "id": item_id, "type": "function_call", "status": "completed", "call_id": call_id, "name": name, "arguments": arguments } }),
            ));
            state.mark_encoded_tool_stop(call_id);
        }
        StreamEventIr::Usage(_) => {}
        StreamEventIr::Completed(completion) => {
            if state.encoded_terminal() {
                return Err(BridgeError::ToolState);
            }
            let status = response_stream_status(completion)?;
            let item_status = if status == "incomplete" {
                "incomplete"
            } else {
                "completed"
            };
            if state.encoded_text() {
                let item_id = state.encoded_message_item_id();
                let output_index = state.encoded_block_index("responses:message");
                let text = state.encoded_text_content();
                frames.push(responses_frame(
                    "response.output_text.done",
                    json!({ "type": "response.output_text.done", "item_id": item_id, "output_index": output_index, "content_index": 0, "text": text }),
                ));
                frames.push(responses_frame(
                    "response.content_part.done",
                    json!({ "type": "response.content_part.done", "item_id": item_id, "output_index": output_index, "content_index": 0, "part": { "type": "output_text", "text": text, "annotations": [] } }),
                ));
                frames.push(responses_frame(
                    "response.output_item.done",
                    json!({ "type": "response.output_item.done", "output_index": output_index, "item": { "id": item_id, "type": "message", "role": "assistant", "status": item_status, "content": [{ "type": "output_text", "text": text, "annotations": [] }] } }),
                ));
            }
            if state.encoded_reasoning() {
                let item_id = state.encoded_reasoning_id();
                let output_index = state.encoded_block_index("responses:reasoning");
                let summary = state.encoded_reasoning_summary();
                let summary_value = if summary.is_empty() {
                    Value::Array(Vec::new())
                } else {
                    json!([{ "type": "summary_text", "text": summary }])
                };
                frames.push(responses_frame(
                    "response.output_item.done",
                    json!({ "type": "response.output_item.done", "output_index": output_index, "item": { "id": item_id, "type": "reasoning", "status": item_status, "summary": summary_value } }),
                ));
            }
            let response_id = state.encoded_message_id();
            let model = state
                .meta()
                .model
                .clone()
                .ok_or(BridgeError::InvalidUpstream)?;
            let usage = state.usage().map(encode_stream_usage);
            let event_name = match status {
                "completed" => "response.completed",
                "incomplete" => "response.incomplete",
                "failed" => "response.failed",
                _ => return Err(BridgeError::InvalidUpstream),
            };
            let incomplete_details = if status == "incomplete" {
                Some(json!({
                    "reason": if completion.stop_reason.as_deref() == Some("refusal") {
                        "content_filter"
                    } else {
                        "max_output_tokens"
                    }
                }))
            } else {
                None
            };
            frames.push(responses_frame(
                event_name,
                json!({
                    "type": event_name,
                    "response": {
                        "id": response_id,
                        "object": "response",
                        "status": status,
                        "model": model,
                        "output": [],
                        "usage": usage,
                        "incomplete_details": incomplete_details
                    }
                }),
            ));
            state.mark_encoded_terminal();
        }
        StreamEventIr::Failed(_) => {
            let response_id = state.encoded_message_id();
            let model = state
                .meta()
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            frames.push(responses_frame(
                "response.failed",
                json!({ "type": "response.failed", "response": { "id": response_id, "object": "response", "status": "failed", "model": model, "output": [], "error": { "code": "invalid_upstream", "message": safe_error_message() } } }),
            ));
            state.mark_encoded_terminal();
        }
    }
    Ok(frames)
}

fn responses_frame(event: &str, data: Value) -> SseFrame {
    SseFrame {
        event: Some(event.to_string()),
        data: serde_json::to_string(&data).expect("Responses stream frame is serializable"),
    }
}

/// 内容型流字段（text/delta/arguments）：允许空白甚至空串——正文换行是合法增量，
/// 不能用 trim 判空（否则 "\n" 增量会把整条流打成 502）。
fn stream_content_string(object: &Map<String, Value>, field: &str) -> Result<String, BridgeError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(BridgeError::InvalidUpstream)
}

fn stream_required_string(object: &Map<String, Value>, field: &str) -> Result<String, BridgeError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or(BridgeError::InvalidUpstream)
}

fn stream_required_u64(object: &Map<String, Value>, field: &str) -> Result<u64, BridgeError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(BridgeError::InvalidUpstream)
}

fn validate_optional_response_id(
    object: &Map<String, Value>,
    state: &StreamState,
) -> Result<(), BridgeError> {
    if let Some(response_id) = object.get("response_id") {
        state.require_response_id(
            response_id
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or(BridgeError::InvalidUpstream)?,
        )?;
    }
    Ok(())
}

fn stream_required_usize(object: &Map<String, Value>, field: &str) -> Result<usize, BridgeError> {
    usize::try_from(stream_required_u64(object, field)?).map_err(|_| BridgeError::ResourceLimit)
}

fn decode_stream_usage(value: &Value) -> Result<UsageIr, BridgeError> {
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    super::request::reject_unknown_fields(
        object,
        &[
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "input_tokens_details",
            "output_tokens_details",
        ],
    )
    .map_err(super::response::map_upstream_error)?;
    let input_tokens = stream_optional_u64(object, "input_tokens")?;
    let output_tokens = stream_optional_u64(object, "output_tokens")?;
    let total_tokens = stream_optional_u64(object, "total_tokens")?;
    if let (Some(input), Some(output), Some(total)) = (input_tokens, output_tokens, total_tokens) {
        if input.checked_add(output) != Some(total) {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    let (cache_read_tokens, cache_write_tokens) = if let Some(value) =
        object.get("input_tokens_details")
    {
        let details = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
        super::request::reject_unknown_fields(details, &["cached_tokens", "cache_write_tokens"])
            .map_err(super::response::map_upstream_error)?;
        (
            stream_optional_u64(details, "cached_tokens")?,
            stream_optional_u64(details, "cache_write_tokens")?,
        )
    } else {
        (None, None)
    };
    Ok(UsageIr {
        input_tokens,
        output_tokens,
        total_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens: stream_detail_u64(object, "output_tokens_details", "reasoning_tokens")?,
    })
}

fn stream_optional_u64(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<u64>, BridgeError> {
    object
        .get(field)
        .map(|value| value.as_u64().ok_or(BridgeError::InvalidUpstream))
        .transpose()
}

fn stream_detail_u64(
    object: &Map<String, Value>,
    field: &str,
    detail: &str,
) -> Result<Option<u64>, BridgeError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let details = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    super::request::reject_unknown_fields(details, &[detail])
        .map_err(super::response::map_upstream_error)?;
    stream_optional_u64(details, detail)
}

fn responses_completion(
    status: &str,
    incomplete_details: Option<&Value>,
    has_tool_calls: bool,
) -> Result<CompletionIr, BridgeError> {
    let (finish_reason, stop_reason) = if status == "completed" && has_tool_calls {
        ("tool_calls", "tool_use")
    } else if status == "completed" {
        ("stop", "end_turn")
    } else if status == "incomplete" {
        let reason = incomplete_details
            .and_then(Value::as_object)
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("max_output_tokens");
        match reason {
            "content_filter" => ("content_filter", "refusal"),
            "max_output_tokens" => ("length", "max_tokens"),
            _ => return Err(BridgeError::InvalidUpstream),
        }
    } else {
        return Err(BridgeError::InvalidUpstream);
    };
    Ok(CompletionIr {
        finish_reason: Some(finish_reason.to_string()),
        stop_reason: Some(stop_reason.to_string()),
        status: Some(status.to_string()),
        error: None,
    })
}

fn encode_stream_usage(usage: &UsageIr) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens,
        "input_tokens_details": if usage.cache_read_tokens.is_some() || usage.cache_write_tokens.is_some() {
            json!({
                "cached_tokens": usage.cache_read_tokens.unwrap_or(0),
                "cache_write_tokens": usage.cache_write_tokens.unwrap_or(0)
            })
        } else {
            Value::Null
        },
        "output_tokens_details": usage.reasoning_tokens.map(|value| json!({ "reasoning_tokens": value }))
    })
}

fn response_stream_status(completion: &CompletionIr) -> Result<&'static str, BridgeError> {
    match completion.status.as_deref() {
        Some("completed") => Ok("completed"),
        Some("incomplete") => Ok("incomplete"),
        Some("failed") => Ok("failed"),
        _ => Err(BridgeError::InvalidUpstream),
    }
}

fn flush_message_output(
    output: &mut Vec<Value>,
    message_text: &mut Vec<Value>,
    ir: &ResponseIr,
    message_index: usize,
    item_statuses: Option<&Map<String, Value>>,
) -> Result<bool, BridgeError> {
    if message_text.is_empty() {
        return Ok(false);
    }
    let id = if message_index == 0
        && super::response::is_source_protocol(ir, WireProtocol::OpenAiResponses)
    {
        ir.extensions
            .get(EXT_RESPONSES_MESSAGE_ITEM_ID)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| generated_id("msg"))
    } else {
        generated_id("msg")
    };
    let status = item_statuses
        .and_then(|statuses| statuses.get(&id))
        .cloned()
        .unwrap_or_else(|| json!("completed"));
    output.push(json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "status": status,
        "content": std::mem::take(message_text)
    }));
    Ok(true)
}

fn apply_output_item_status(item: &mut Value, item_statuses: Option<&Map<String, Value>>) {
    let Some(id) = item.get("id").and_then(Value::as_str) else {
        return;
    };
    if let Some(status) = item_statuses.and_then(|statuses| statuses.get(id)) {
        item["status"] = status.clone();
    }
}

fn apply_reasoning_item_content(item: &mut Value, item_contents: Option<&Map<String, Value>>) {
    let Some(id) = item.get("id").and_then(Value::as_str) else {
        return;
    };
    if let Some(content) = item_contents.and_then(|contents| contents.get(id)) {
        item["content"] = content.clone();
    }
}

fn capture_output_item_status(
    object: &Map<String, Value>,
    response_status: &str,
    item_id: Option<&str>,
    statuses: &mut Map<String, Value>,
) -> Result<(), BridgeError> {
    let Some(status) = object.get("status") else {
        return Ok(());
    };
    let status = status.as_str().ok_or(BridgeError::InvalidUpstream)?;
    if !matches!(status, "completed" | "incomplete") {
        return Err(BridgeError::Unsupported {
            field: "responses.output.status".to_string(),
        });
    }
    if response_status == "completed" && status != "completed" {
        return Err(BridgeError::InvalidUpstream);
    }
    if let Some(item_id) = item_id {
        statuses.insert(item_id.to_string(), Value::String(status.to_string()));
    } else if status == "incomplete" {
        return Err(BridgeError::Unsupported {
            field: "responses.output.status_without_id".to_string(),
        });
    }
    Ok(())
}

fn validate_standard_response_fields(object: &Map<String, Value>) -> Result<(), BridgeError> {
    validate_optional_bool(object.get("background"))?;
    validate_optional_u64(object.get("max_output_tokens"))?;
    validate_optional_u64(object.get("max_tool_calls"))?;
    validate_optional_bool(object.get("parallel_tool_calls"))?;
    validate_optional_f64_range(object.get("temperature"), 0.0, 2.0)?;
    validate_optional_u64_max(object.get("top_logprobs"), 20)?;
    validate_optional_f64_range(object.get("top_p"), 0.0, 1.0)?;
    validate_optional_enum(
        object.get("service_tier"),
        &["auto", "default", "flex", "scale", "priority", "fast"],
    )?;
    validate_optional_enum(object.get("prompt_cache_retention"), &["in_memory", "24h"])?;
    validate_optional_string_or_null(object.get("previous_response_id"))?;
    validate_optional_string_or_null(object.get("prompt_cache_key"))?;
    validate_optional_string_or_null(object.get("user"))?;
    validate_optional_string_or_null(object.get("safety_identifier"))?;
    validate_optional_object(object.get("metadata"))?;
    validate_optional_object(object.get("text"))?;
    validate_optional_object(object.get("prompt"))?;
    validate_optional_enum(object.get("truncation"), &["auto", "disabled"])
}

fn decode_response_metadata(
    object: &Map<String, Value>,
    status: &str,
    extensions: &mut BTreeMap<String, Value>,
) -> Result<(), BridgeError> {
    if let Some(value) = object.get("completed_at") {
        if !value.is_null() && value.as_u64().is_none() {
            return Err(BridgeError::InvalidUpstream);
        }
        if !value.is_null() {
            if status == "incomplete" {
                return Err(BridgeError::InvalidUpstream);
            }
            extensions.insert(EXT_RESPONSES_COMPLETED_AT.to_string(), value.clone());
        }
    }
    if let Some(value) = object.get("instructions") {
        validate_response_instructions(value)?;
        if !value.is_null() {
            extensions.insert(EXT_RESPONSES_INSTRUCTIONS.to_string(), value.clone());
        }
    }
    if let Some(value) = object.get("reasoning") {
        if !value.is_null() {
            let reasoning = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
            validate_response_reasoning(reasoning)?;
            extensions.insert(EXT_RESPONSES_REASONING.to_string(), value.clone());
        }
    }
    if let Some(value) = object.get("conversation") {
        if !value.is_null() {
            let conversation = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(conversation, &["id"]).map_err(map_upstream_error)?;
            response_required_string(conversation, "id")?;
            extensions.insert(EXT_RESPONSES_CONVERSATION.to_string(), value.clone());
        }
    }
    if let Some(value) = object.get("prompt_cache_options") {
        validate_prompt_cache_options(value)?;
        extensions.insert(
            EXT_RESPONSES_PROMPT_CACHE_OPTIONS.to_string(),
            value.clone(),
        );
    }
    if let Some(value) = object.get("moderation") {
        if !value.is_null() {
            validate_moderation(value)?;
            extensions.insert(EXT_RESPONSES_MODERATION.to_string(), value.clone());
        }
    }
    if let Some(value) = object.get("incomplete_details") {
        if !value.is_null() {
            if status != "incomplete" {
                return Err(BridgeError::InvalidUpstream);
            }
            let details = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(details, &["reason"]).map_err(map_upstream_error)?;
            let reason = response_required_string(details, "reason")?;
            if !matches!(reason.as_str(), "max_output_tokens" | "content_filter") {
                return Err(BridgeError::InvalidUpstream);
            }
            extensions.insert(EXT_RESPONSES_INCOMPLETE_DETAILS.to_string(), value.clone());
        }
    }
    Ok(())
}

fn validate_response_instructions(value: &Value) -> Result<(), BridgeError> {
    if value.is_null() || value.is_string() {
        return Ok(());
    }
    let items = value.as_array().ok_or(BridgeError::InvalidUpstream)?;
    for item in items {
        let object = item.as_object().ok_or(BridgeError::InvalidUpstream)?;
        match object.get("type").and_then(Value::as_str) {
            None | Some("message") => validate_instruction_message(object)?,
            Some("function_call") => {
                reject_unknown_fields(
                    object,
                    &["id", "type", "call_id", "name", "arguments", "status"],
                )
                .map_err(map_upstream_error)?;
                response_required_string(object, "call_id")?;
                response_required_string(object, "name")?;
                response_required_string(object, "arguments")?;
                validate_instruction_status(object.get("status"))?;
            }
            Some("function_call_output") => {
                reject_unknown_fields(object, &["id", "type", "call_id", "output", "status"])
                    .map_err(map_upstream_error)?;
                response_required_string(object, "call_id")?;
                if !object
                    .get("output")
                    .is_some_and(|output| output.is_string() || output.is_array())
                {
                    return Err(BridgeError::InvalidUpstream);
                }
                validate_instruction_status(object.get("status"))?;
            }
            Some("reasoning") => {
                validate_reasoning_item(object)?;
            }
            Some(other) => {
                return Err(BridgeError::Unsupported {
                    field: format!("responses.instructions.type:{other}"),
                })
            }
        }
    }
    Ok(())
}

fn validate_instruction_message(object: &Map<String, Value>) -> Result<(), BridgeError> {
    reject_unknown_fields(
        object,
        &["id", "type", "role", "status", "content", "phase"],
    )
    .map_err(map_upstream_error)?;
    let role = response_required_string(object, "role")?;
    if !matches!(role.as_str(), "user" | "assistant" | "system" | "developer") {
        return Err(BridgeError::InvalidUpstream);
    }
    validate_instruction_status(object.get("status"))?;
    if let Some(phase) = object.get("phase") {
        if !phase.is_null() && !matches!(phase.as_str(), Some("commentary") | Some("final_answer"))
        {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    let content = object.get("content").ok_or(BridgeError::InvalidUpstream)?;
    if content.is_string() {
        return Ok(());
    }
    let parts = content.as_array().ok_or(BridgeError::InvalidUpstream)?;
    for part in parts {
        let part = part.as_object().ok_or(BridgeError::InvalidUpstream)?;
        match part.get("type").and_then(Value::as_str) {
            Some("input_text")
            | Some("output_text")
            | Some("text")
            | Some("summary_text")
            | Some("reasoning_text") => {
                reject_unknown_fields(part, &["type", "text"]).map_err(map_upstream_error)?;
                response_required_string(part, "text")?;
            }
            Some("refusal") => {
                reject_unknown_fields(part, &["type", "refusal"]).map_err(map_upstream_error)?;
                response_required_string(part, "refusal")?;
            }
            Some("input_image") => {
                reject_unknown_fields(
                    part,
                    &[
                        "type",
                        "image_url",
                        "file_id",
                        "detail",
                        "prompt_cache_breakpoint",
                    ],
                )
                .map_err(map_upstream_error)?;
                validate_optional_string_or_null(part.get("image_url"))?;
                validate_optional_string_or_null(part.get("file_id"))?;
                validate_required_enum(part.get("detail"), &["low", "high", "auto", "original"])?;
                validate_optional_object_or_null(part.get("prompt_cache_breakpoint"))?;
            }
            Some("input_file") => {
                reject_unknown_fields(
                    part,
                    &[
                        "type",
                        "file_id",
                        "filename",
                        "file_data",
                        "prompt_cache_breakpoint",
                        "file_url",
                        "detail",
                    ],
                )
                .map_err(map_upstream_error)?;
                validate_optional_string_or_null(part.get("file_id"))?;
                validate_optional_string(part.get("filename"))?;
                validate_optional_string(part.get("file_data"))?;
                validate_optional_string(part.get("file_url"))?;
                validate_optional_object_or_null(part.get("prompt_cache_breakpoint"))?;
                if let Some(detail) = part.get("detail") {
                    if !matches!(detail.as_str(), Some("auto") | Some("low") | Some("high")) {
                        return Err(BridgeError::InvalidUpstream);
                    }
                }
            }
            Some(other) => {
                return Err(BridgeError::Unsupported {
                    field: format!("responses.instructions.content.type:{other}"),
                })
            }
            None => return Err(BridgeError::InvalidUpstream),
        }
    }
    Ok(())
}

fn validate_instruction_status(value: Option<&Value>) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !matches!(
            value.as_str(),
            Some("in_progress") | Some("completed") | Some("incomplete")
        ) {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_reasoning_item(object: &Map<String, Value>) -> Result<(), BridgeError> {
    reject_unknown_fields(
        object,
        &[
            "id",
            "type",
            "status",
            "summary",
            "content",
            "encrypted_content",
        ],
    )
    .map_err(map_upstream_error)?;
    response_required_string(object, "id")?;
    let summary = object
        .get("summary")
        .and_then(Value::as_array)
        .ok_or(BridgeError::InvalidUpstream)?;
    for item in summary {
        let item = item.as_object().ok_or(BridgeError::InvalidUpstream)?;
        reject_unknown_fields(item, &["type", "text"]).map_err(map_upstream_error)?;
        if item.get("type").and_then(Value::as_str) != Some("summary_text") {
            return Err(BridgeError::InvalidUpstream);
        }
        response_required_string(item, "text")?;
    }
    if let Some(content) = object.get("content") {
        validate_reasoning_content(content)?;
    }
    validate_optional_string_or_null(object.get("encrypted_content"))?;
    validate_instruction_status(object.get("status"))
}

fn validate_response_reasoning(object: &Map<String, Value>) -> Result<(), BridgeError> {
    reject_unknown_fields(
        object,
        &["mode", "effort", "summary", "context", "generate_summary"],
    )
    .map_err(map_upstream_error)?;
    validate_optional_string(object.get("mode"))?;
    validate_optional_enum(
        object.get("effort"),
        &["none", "minimal", "low", "medium", "high", "xhigh", "max"],
    )?;
    validate_optional_enum(object.get("summary"), &["auto", "concise", "detailed"])?;
    validate_optional_enum(
        object.get("context"),
        &["auto", "current_turn", "all_turns"],
    )?;
    validate_optional_enum(
        object.get("generate_summary"),
        &["auto", "concise", "detailed"],
    )
}

fn validate_prompt_cache_options(value: &Value) -> Result<(), BridgeError> {
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(object, &["ttl", "mode"]).map_err(map_upstream_error)?;
    validate_required_enum(object.get("ttl"), &["30m"])?;
    validate_required_enum(object.get("mode"), &["implicit", "explicit"])
}

fn validate_moderation(value: &Value) -> Result<(), BridgeError> {
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(object, &["input", "output"]).map_err(map_upstream_error)?;
    validate_moderation_entry(object.get("input"))?;
    validate_moderation_entry(object.get("output"))
}

fn validate_moderation_entry(value: Option<&Value>) -> Result<(), BridgeError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(BridgeError::InvalidUpstream)?;
    match object.get("type").and_then(Value::as_str) {
        Some("error") => {
            reject_unknown_fields(object, &["type", "code", "message"])
                .map_err(map_upstream_error)?;
            response_required_string(object, "code")?;
            response_required_string(object, "message")?;
        }
        Some("moderation_result") => {
            reject_unknown_fields(
                object,
                &[
                    "type",
                    "model",
                    "flagged",
                    "categories",
                    "category_scores",
                    "category_applied_input_types",
                ],
            )
            .map_err(map_upstream_error)?;
            response_required_string(object, "model")?;
            if !object.get("flagged").is_some_and(Value::is_boolean) {
                return Err(BridgeError::InvalidUpstream);
            }
            validate_bool_map(object.get("categories"))?;
            validate_number_map(object.get("category_scores"))?;
            validate_input_type_map(object.get("category_applied_input_types"))?;
        }
        _ => return Err(BridgeError::InvalidUpstream),
    }
    Ok(())
}

fn validate_bool_map(value: Option<&Value>) -> Result<(), BridgeError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(BridgeError::InvalidUpstream)?;
    if object.values().any(|value| !value.is_boolean()) {
        return Err(BridgeError::InvalidUpstream);
    }
    Ok(())
}

fn validate_number_map(value: Option<&Value>) -> Result<(), BridgeError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(BridgeError::InvalidUpstream)?;
    if object
        .values()
        .any(|value| value.as_f64().is_none_or(|number| !number.is_finite()))
    {
        return Err(BridgeError::InvalidUpstream);
    }
    Ok(())
}

fn validate_input_type_map(value: Option<&Value>) -> Result<(), BridgeError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or(BridgeError::InvalidUpstream)?;
    for types in object.values() {
        let types = types.as_array().ok_or(BridgeError::InvalidUpstream)?;
        if types
            .iter()
            .any(|value| !matches!(value.as_str(), Some("text") | Some("image")))
        {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_bool(value: Option<&Value>) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null() && !value.is_boolean() {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_u64(value: Option<&Value>) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null() && value.as_u64().is_none() {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_u64_max(value: Option<&Value>, maximum: u64) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null() && value.as_u64().is_none_or(|value| value > maximum) {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_f64_range(
    value: Option<&Value>,
    minimum: f64,
    maximum: f64,
) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null()
            && !value
                .as_f64()
                .is_some_and(|value| value.is_finite() && value >= minimum && value <= maximum)
        {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_string(value: Option<&Value>) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if value.as_str().is_none() {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_object(value: Option<&Value>) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null() && !value.is_object() {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_string_or_null(value: Option<&Value>) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null() && value.as_str().is_none() {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_object_or_null(value: Option<&Value>) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null() && !value.is_object() {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_optional_enum(value: Option<&Value>, allowed: &[&str]) -> Result<(), BridgeError> {
    if let Some(value) = value {
        if !value.is_null()
            && !value
                .as_str()
                .is_some_and(|value| allowed.iter().any(|allowed| allowed == &value))
        {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    Ok(())
}

fn validate_required_enum(value: Option<&Value>, allowed: &[&str]) -> Result<(), BridgeError> {
    let value = value.ok_or(BridgeError::InvalidUpstream)?;
    if !value
        .as_str()
        .is_some_and(|value| allowed.iter().any(|allowed| allowed == &value))
    {
        return Err(BridgeError::InvalidUpstream);
    }
    Ok(())
}

fn response_status(ir: &ResponseIr) -> Result<&'static str, BridgeError> {
    if content_filter_semantics(ir) {
        return Ok("incomplete");
    }
    match ir.completion.status.as_deref() {
        Some("completed") => Ok("completed"),
        Some("incomplete") => Ok("incomplete"),
        Some(_) => Err(BridgeError::InvalidUpstream),
        None if ir.completion.finish_reason.as_deref() == Some("length") => Ok("incomplete"),
        None if ir.completion.stop_reason.as_deref() == Some("max_tokens") => Ok("incomplete"),
        None => Ok("completed"),
    }
}

fn content_filter_semantics(ir: &ResponseIr) -> bool {
    ir.completion.finish_reason.as_deref() == Some("content_filter")
        || ir.completion.stop_reason.as_deref() == Some("refusal")
}

fn decode_usage(value: Option<&Value>) -> Result<Option<UsageIr>, BridgeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(
        object,
        &[
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "input_tokens_details",
            "output_tokens_details",
        ],
    )
    .map_err(map_upstream_error)?;
    let input_tokens = Some(required_u64(object, "input_tokens")?);
    let output_tokens = Some(required_u64(object, "output_tokens")?);
    let total_tokens = normalize_total_tokens(
        input_tokens,
        output_tokens,
        optional_u64(object, "total_tokens")?,
    )?;
    let (cache_read_tokens, cache_write_tokens) = if object.contains_key("input_tokens_details") {
        let details = required_object(object, "input_tokens_details")?;
        reject_unknown_fields(details, &["cached_tokens", "cache_write_tokens"])
            .map_err(map_upstream_error)?;
        (
            optional_u64(details, "cached_tokens")?,
            optional_u64(details, "cache_write_tokens")?,
        )
    } else {
        (None, None)
    };
    let reasoning_tokens = if object.contains_key("output_tokens_details") {
        let details = required_object(object, "output_tokens_details")?;
        reject_unknown_fields(details, &["reasoning_tokens"]).map_err(map_upstream_error)?;
        optional_u64(details, "reasoning_tokens")?
    } else {
        None
    };
    Ok(Some(UsageIr {
        input_tokens,
        output_tokens,
        total_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    }))
}

fn encode_usage(usage: Option<&UsageIr>) -> Result<Value, BridgeError> {
    let Some(usage) = usage else {
        return Ok(Value::Null);
    };
    let input_tokens = usage.input_tokens.ok_or(BridgeError::InvalidUpstream)?;
    let output_tokens = usage.output_tokens.ok_or(BridgeError::InvalidUpstream)?;
    let total_tokens =
        normalize_total_tokens(Some(input_tokens), Some(output_tokens), usage.total_tokens)?
            .ok_or(BridgeError::InvalidUpstream)?;
    let mut result = json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": total_tokens
    });
    if usage.cache_read_tokens.is_some() || usage.cache_write_tokens.is_some() {
        result["input_tokens_details"] = json!({
            "cached_tokens": usage.cache_read_tokens.unwrap_or(0),
            "cache_write_tokens": usage.cache_write_tokens.unwrap_or(0)
        });
    }
    if let Some(value) = usage.reasoning_tokens {
        result["output_tokens_details"] = json!({ "reasoning_tokens": value });
    }
    Ok(result)
}

fn instruction_value(ir: &RequestIr) -> Result<Option<Value>, BridgeError> {
    if let Some(value) = ir.extensions.get(EXT_INSTRUCTIONS) {
        let _ = text_content(value, "instructions")?;
        return Ok(Some(value.clone()));
    }
    let mut parts = Vec::new();
    for message in &ir.messages {
        if message.role == RoleIr::System {
            parts.push(text_from_content(&message.content)?);
        }
    }
    Ok((!parts.is_empty()).then_some(Value::String(parts.join("\n\n"))))
}

fn encode_message(
    message: &MessageIr,
    input: &mut Vec<Value>,
    reasoning_index: &mut usize,
    reasoning_ids: &[Value],
    reasoning_metadata: Option<&[Value]>,
) -> Result<(), BridgeError> {
    match message.role {
        RoleIr::System | RoleIr::Developer | RoleIr::User | RoleIr::Assistant => {
            let role = match message.role {
                RoleIr::System => "system",
                RoleIr::Developer => "developer",
                RoleIr::User => "user",
                RoleIr::Assistant => "assistant",
                RoleIr::Tool => unreachable!(),
            };
            let mut text_parts = Vec::new();
            for content in &message.content {
                match content {
                    ContentIr::Text(text) => text_parts.push(if role == "assistant" {
                        json!({ "type": "output_text", "text": text })
                    } else {
                        json!({ "type": "input_text", "text": text })
                    }),
                    ContentIr::Image(media) if role != "assistant" => {
                        text_parts.push(json!({
                            "type": "input_image",
                            "image_url": media_to_url(media)?
                        }));
                    }
                    ContentIr::Document(media) if role != "assistant" => {
                        let mut file = json!({ "type": "input_file" });
                        if let Some((key, value)) = media_to_responses_file(media)?
                            .as_object()
                            .and_then(|object| object.iter().next())
                        {
                            file[key] = value.clone();
                        }
                        text_parts.push(file);
                    }
                    ContentIr::Thinking { text, signature } => {
                        if message.item_id.is_some() {
                            return Err(BridgeError::Unsupported {
                                field: "responses.message_item_id".to_string(),
                            });
                        }
                        flush_message(
                            role,
                            &mut text_parts,
                            input,
                            message.item_id.as_deref(),
                            message.name.as_deref(),
                        );
                        input.push(encode_reasoning(
                            text,
                            signature.as_deref(),
                            *reasoning_index,
                            reasoning_ids,
                            reasoning_metadata.and_then(|items| items.get(*reasoning_index)),
                        )?);
                        *reasoning_index += 1;
                    }
                    ContentIr::RedactedThinking { data } => {
                        if message.item_id.is_some() {
                            return Err(BridgeError::Unsupported {
                                field: "responses.message_item_id".to_string(),
                            });
                        }
                        flush_message(
                            role,
                            &mut text_parts,
                            input,
                            message.item_id.as_deref(),
                            message.name.as_deref(),
                        );
                        input.push(encode_reasoning(
                            "",
                            Some(data),
                            *reasoning_index,
                            reasoning_ids,
                            reasoning_metadata.and_then(|items| items.get(*reasoning_index)),
                        )?);
                        *reasoning_index += 1;
                    }
                    ContentIr::ToolUse(call) => {
                        if message.item_id.is_some() {
                            return Err(BridgeError::Unsupported {
                                field: "responses.message_item_id".to_string(),
                            });
                        }
                        flush_message(
                            role,
                            &mut text_parts,
                            input,
                            message.item_id.as_deref(),
                            message.name.as_deref(),
                        );
                        input.push(encode_function_call(call)?);
                    }
                    ContentIr::ToolResult(_) => {
                        return Err(BridgeError::ToolState);
                    }
                    ContentIr::Image(_) | ContentIr::Document(_) => {
                        return Err(BridgeError::Unsupported {
                            field: "input.message.content".to_string(),
                        })
                    }
                }
            }
            flush_message(
                role,
                &mut text_parts,
                input,
                message.item_id.as_deref(),
                message.name.as_deref(),
            );
        }
        RoleIr::Tool => {
            for content in &message.content {
                let ContentIr::ToolResult(result) = content else {
                    return Err(BridgeError::ToolState);
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": result.call_id,
                    "output": text_from_content(&result.content)?,
                    "status": if result.is_error { "failed" } else { "completed" },
                }));
            }
        }
    }
    Ok(())
}

fn flush_message(
    role: &str,
    parts: &mut Vec<Value>,
    input: &mut Vec<Value>,
    item_id: Option<&str>,
    name: Option<&str>,
) {
    if parts.is_empty() {
        return;
    }
    let parts = std::mem::take(parts);
    push_message(role, parts, input, item_id, name);
}

fn push_message(
    role: &str,
    parts: Vec<Value>,
    input: &mut Vec<Value>,
    item_id: Option<&str>,
    name: Option<&str>,
) {
    let mut message = json!({ "type": "message", "role": role, "content": parts });
    if let Some(id) = item_id {
        message["id"] = json!(id);
    }
    if let Some(name) = name {
        message["name"] = json!(name);
    }
    input.push(message);
}

fn encode_reasoning(
    text: &str,
    signature: Option<&str>,
    index: usize,
    reasoning_ids: &[Value],
    metadata: Option<&Value>,
) -> Result<Value, BridgeError> {
    let id = reasoning_ids
        .get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("rs_{index}"));
    let recovered = signature.and_then(|signature| {
        let mut block = json!({ "type": "thinking", "thinking": text });
        block["signature"] = json!(signature);
        openai_reasoning_item_from_anthropic_block(&block)
    });
    let recovered_item = recovered.is_some();
    let mut value = recovered.unwrap_or_else(|| {
        json!({
            "type": "reasoning",
            "id": id,
            "summary": if text.is_empty() { Vec::<Value>::new() } else { vec![json!({ "type": "summary_text", "text": text })] },
        })
    });
    if value.get("id").and_then(Value::as_str).is_none() {
        value["id"] = json!(id);
    }
    if !recovered_item {
        if let Some(signature) = signature {
            value["encrypted_content"] = json!(signature);
        }
    }
    if let Some(metadata) = metadata.filter(|metadata| !metadata.is_null()) {
        let metadata = metadata.as_object().ok_or(BridgeError::InvalidRequest)?;
        reject_unknown_fields(metadata, &["status", "content"])?;
        if metadata.contains_key("status") {
            let status = required_string(metadata, "status")?;
            if !matches!(status.as_str(), "in_progress" | "completed" | "incomplete") {
                return Err(BridgeError::InvalidRequest);
            }
            value["status"] = Value::String(status);
        }
        if let Some(content) = metadata.get("content") {
            validate_reasoning_content(content).map_err(|_| BridgeError::InvalidRequest)?;
            value["content"] = content.clone();
        }
    }
    Ok(value)
}

fn encode_function_call(call: &ToolCallIr) -> Result<Value, BridgeError> {
    if call.call_id.trim().is_empty() || call.name.trim().is_empty() || !call.arguments.is_object()
    {
        return Err(BridgeError::InvalidRequest);
    }
    let item_id = call
        .item_id
        .clone()
        .unwrap_or_else(|| format!("fc_{}", call.call_id));
    Ok(json!({
        "type": "function_call",
        "id": item_id,
        "call_id": call.call_id,
        "name": call.name,
        "arguments": serde_json::to_string(&call.arguments).map_err(|_| BridgeError::InvalidRequest)?,
        "status": "completed"
    }))
}

fn encode_tool(tool: &ToolDefinitionIr) -> Result<Value, BridgeError> {
    if tool.name.trim().is_empty() || !tool.input_schema.is_object() {
        return Err(BridgeError::InvalidRequest);
    }
    let mut value = json!({
        "type": "function",
        "name": tool.name,
        "parameters": tool.input_schema,
    });
    if let Some(description) = &tool.description {
        value["description"] = json!(description);
    }
    if let Some(strict) = tool.strict {
        value["strict"] = json!(strict);
    }
    Ok(value)
}

fn encode_tool_choice(choice: &ToolChoiceIr) -> Result<Value, BridgeError> {
    Ok(match choice {
        ToolChoiceIr::Auto => json!("auto"),
        ToolChoiceIr::Any => json!("required"),
        ToolChoiceIr::None => json!("none"),
        ToolChoiceIr::Tool { name } if !name.trim().is_empty() => {
            json!({ "type": "function", "name": name })
        }
        ToolChoiceIr::Tool { .. } => return Err(BridgeError::InvalidRequest),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{ContentIr, RoleIr};

    #[test]
    fn parser_preserves_instruction_reasoning_and_function_item_ids() {
        let body = json!({
            "model": "responses-fixture-model",
            "instructions": "instruction fixture",
            "input": [
                { "id": "msg-fixture", "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "request fixture" }] },
                { "id": "reasoning-fixture", "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking fixture" }], "encrypted_content": "opaque-fixture" },
                { "id": "item-fixture", "type": "function_call", "call_id": "call-fixture", "name": "lookup", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "call-fixture", "output": "result fixture" }
            ],
            "reasoning": { "effort": "high" },
            "max_output_tokens": 88
        });

        let ir = super::parse_request(&body).expect("Responses request parses");
        assert_eq!(ir.messages[0].role, RoleIr::System);
        assert_eq!(ir.messages[1].role, RoleIr::User);
        assert!(matches!(
            ir.messages[2].content[0],
            ContentIr::Thinking { signature: Some(ref signature), .. }
                if signature.starts_with(super::super::reasoning::OPENAI_REASONING_ITEM_PREFIX)
        ));
        let signature = match &ir.messages[2].content[0] {
            ContentIr::Thinking {
                signature: Some(signature),
                ..
            } => signature,
            _ => unreachable!("reasoning signature is asserted above"),
        };
        assert_eq!(
            super::super::reasoning::decode_openai_reasoning_item(signature),
            Some(body["input"][1].clone())
        );
        assert!(matches!(
            ir.messages[2].content[1],
            ContentIr::ToolUse(ref call)
                if call.item_id.as_deref() == Some("item-fixture")
                    && call.call_id == "call-fixture"
        ));
        let encoded = super::encode_request(&ir, "responses-upstream").expect("encodes");
        assert_eq!(encoded["input"][1]["id"], json!("reasoning-fixture"));
        assert_eq!(encoded["input"][2]["id"], json!("item-fixture"));
    }

    #[test]
    fn stream_usage_round_trips_cache_write_tokens() {
        use super::super::ir::{
            CompletionIr, ResponseMetaIr, SseFrame, StreamEventIr, UsageIr, WireProtocol,
        };
        use super::super::stream::{encode_stream_events, StreamState};
        use serde_json::{json, Value};

        let frame = SseFrame {
            event: Some("response.completed".to_string()),
            data: r#"{"type":"response.completed","response":{"id":"resp-1","model":"model","status":"completed","usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":40,"cache_write_tokens":60},"output_tokens":10,"output_tokens_details":{"reasoning_tokens":5},"total_tokens":110}}}"#.to_string(),
        };
        let mut state = StreamState::new();
        super::decode_stream_frame(
            &SseFrame {
                event: Some("response.created".to_string()),
                data: r#"{"type":"response.created","response":{"id":"resp-1","model":"model","status":"in_progress"}}"#.to_string(),
            },
            &mut state,
        )
        .expect("response.created starts the stream");
        let events = super::decode_stream_frame(&frame, &mut state)
            .expect("streamed usage with cache_write_tokens must decode like the non-stream path");
        assert!(events.iter().any(|event| {
            matches!(event, StreamEventIr::Usage(usage)
                if usage.cache_read_tokens == Some(40)
                    && usage.cache_write_tokens == Some(60)
                    && usage.reasoning_tokens == Some(5))
        }));

        let mut output_state = StreamState::new();
        let encoded = encode_stream_events(
            WireProtocol::OpenAiResponses,
            &[
                StreamEventIr::Started(ResponseMetaIr {
                    id: Some("resp-1".to_string()),
                    model: Some("model".to_string()),
                }),
                StreamEventIr::Usage(UsageIr {
                    input_tokens: Some(100),
                    output_tokens: Some(10),
                    total_tokens: Some(110),
                    cache_read_tokens: Some(40),
                    cache_write_tokens: Some(60),
                    reasoning_tokens: Some(5),
                }),
                StreamEventIr::Completed(CompletionIr {
                    finish_reason: Some("stop".to_string()),
                    stop_reason: Some("end_turn".to_string()),
                    status: Some("completed".to_string()),
                    error: None,
                }),
            ],
            &mut output_state,
        )
        .expect("Responses stream with cache usage encodes");
        let completed = encoded
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.completed"))
            .map(|frame| serde_json::from_str::<Value>(&frame.data).expect("frame is JSON"))
            .expect("completed frame present");
        assert_eq!(
            completed["response"]["usage"]["input_tokens_details"]["cache_write_tokens"],
            json!(60)
        );
        assert_eq!(
            completed["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
            json!(5)
        );
    }

    #[test]
    fn real_codex_request_maps_through_chat_without_field_loss() {
        use super::super::ContentIr;
        use serde_json::Value;

        let body: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/runtime_bridge/codex_responses_request.json"
        ))
        .expect("Codex fixture is valid JSON");
        let ir = super::parse_request(&body).expect("real Codex request parses");
        assert_eq!(ir.messages[0].role, RoleIr::System);
        assert!(matches!(ir.messages[0].content[0], ContentIr::Text(_)));
        assert_eq!(ir.messages[1].role, RoleIr::Developer);
        assert_eq!(
            ir.messages[1].item_id.as_deref(),
            Some("msg_fixture_developer")
        );
        assert_eq!(ir.tool_choice, Some(super::super::ToolChoiceIr::Auto));
        assert_eq!(ir.generation.reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(
            ir.extensions
                .get(super::EXT_RESPONSES_PARALLEL_TOOL_CALLS)
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            ir.extensions
                .get(super::EXT_RESPONSES_INCLUDE)
                .and_then(Value::as_array)
                .map(|items| items.len()),
            Some(1)
        );
        assert_eq!(
            ir.extensions
                .get(super::EXT_RESPONSES_PROMPT_CACHE_KEY)
                .and_then(Value::as_str),
            Some("fixture-prompt-cache-key")
        );
        assert!(ir
            .extensions
            .contains_key(super::EXT_RESPONSES_CLIENT_METADATA));
        assert_eq!(ir.tools.len(), 4, "namespace/web_search are skipped");
        assert_eq!(ir.tools[3].name, "apply_patch");
        assert_eq!(
            ir.tools[3].input_schema,
            json!({"type": "object"}),
            "custom tool without parameters gets an open schema"
        );

        let encoded = super::super::chat::encode_request(&ir, "deepseek-v4-flash-free")
            .expect("encodes to Chat");
        assert_eq!(encoded["messages"][0]["role"], json!("system"));
        assert_eq!(encoded["messages"][1]["role"], json!("developer"));
        assert_eq!(encoded["tools"].as_array().unwrap().len(), 4);
        assert!(encoded["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["type"] == json!("function")));
        assert_eq!(encoded["reasoning_effort"], json!("medium"));
        assert_eq!(encoded["parallel_tool_calls"], json!(false));
        assert_eq!(encoded["stream"], json!(true));
    }

    #[test]
    fn parser_rejects_unknown_input_items() {
        let body = json!({
            "model": "responses-fixture-model",
            "input": [{ "type": "computer_call", "id": "fixture" }]
        });

        assert!(matches!(
            super::parse_request(&body),
            Err(super::super::BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn parser_rejects_unknown_reasoning_fields() {
        let body = json!({
            "model": "responses-fixture-model",
            "reasoning": {"effort": "high", "summary": "not a request field"},
            "input": "fixture"
        });
        assert!(matches!(
            super::parse_request(&body),
            Err(super::super::BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn responses_request_accepts_modern_official_fields() {
        let body = json!({
            "model": "responses-fixture-model",
            "input": "fixture",
            "service_tier": "priority",
            "text": {"verbosity": {"level": "low"}},
            "stream_options": {"include_usage": true},
            "conversation": {"id": "conv_fixture"},
            "context_management": {"edits": []},
            "client_metadata": {"turn_id": "fixture-turn"},
            "prompt_cache_key": "fixture-key",
            "include": ["reasoning.encrypted_content"],
            "store": false,
            "stream": false
        });
        let ir = super::parse_request(&body).expect("modern Responses request parses");
        assert!(ir
            .extensions
            .contains_key(super::EXT_RESPONSES_SERVICE_TIER));
        assert!(ir.extensions.contains_key(super::EXT_RESPONSES_TEXT));
        assert!(ir
            .extensions
            .contains_key(super::EXT_RESPONSES_STREAM_OPTIONS));
        assert!(ir
            .extensions
            .contains_key(super::EXT_RESPONSES_REQUEST_CONVERSATION));
        assert!(ir
            .extensions
            .contains_key(super::EXT_RESPONSES_CONTEXT_MANAGEMENT));
    }

    #[test]
    fn parse_tools_forwards_custom_tool_schema() {
        let body = json!({
            "type": "custom",
            "name": "shell",
            "description": "run a command",
            "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }
        });
        let tools = super::parse_tools(Some(&json!([body]))).expect("custom tool parses");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "shell");
    }

    #[test]
    fn parse_tools_accepts_codex_custom_tool_with_format_field() {
        let body = json!({
            "type": "custom",
            "name": "apply_patch",
            "description": "apply a diff to files",
            "format": "diff"
        });
        let tools = super::parse_tools(Some(&json!([body])))
            .expect("Codex custom tool with format field must parse");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "apply_patch");
        assert_eq!(tools[0].input_schema, json!({"type": "object"}));
    }

    #[test]
    fn parse_tools_defaults_missing_custom_tool_parameters() {
        let body = json!({
            "type": "custom",
            "name": "apply_patch",
            "description": "apply a diff to files"
        });
        let tools = super::parse_tools(Some(&json!([body])))
            .expect("Codex custom tool without parameters must parse");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].input_schema, json!({"type": "object"}));
    }

    #[test]
    fn parse_tools_requires_function_parameters() {
        let body = json!({
            "type": "function",
            "name": "exec_command",
            "description": "run a command"
        });
        assert!(matches!(
            super::parse_tools(Some(&json!([body]))),
            Err(super::super::BridgeError::InvalidRequest)
        ));
    }

    #[test]
    fn encoder_keeps_message_ids_when_media_is_mixed() {
        let body = json!({
            "model": "responses-fixture-model",
            "input": [
                {
                    "id": "message-one",
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "fixture text"},
                        {"type": "input_image", "image_url": "https://fixture.invalid/image"}
                    ]
                },
                {
                    "id": "message-two",
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "fixture follow-up"}]
                }
            ]
        });
        let ir = super::parse_request(&body).expect("Responses request parses");
        let encoded = super::encode_request(&ir, "responses-upstream").expect("encodes");
        assert_eq!(encoded["input"][0]["id"], json!("message-one"));
        assert_eq!(encoded["input"][0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(encoded["input"][1]["id"], json!("message-two"));
    }

    #[test]
    fn usage_encoder_keeps_cached_tokens_inside_responses_input_tokens() {
        let usage = super::decode_usage(Some(&json!({
            "input_tokens": 100,
            "input_tokens_details": { "cached_tokens": 40 },
            "output_tokens": 20,
            "total_tokens": 120
        })))
        .expect("Responses usage decodes")
        .expect("usage present");
        assert_eq!(usage.cache_read_tokens, Some(40));

        let encoded = super::encode_usage(Some(&usage)).expect("Responses usage encodes");
        assert_eq!(encoded["input_tokens"], json!(100));
        assert_eq!(encoded["input_tokens_details"]["cached_tokens"], json!(40));
    }
}
