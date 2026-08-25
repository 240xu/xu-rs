use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::ir::{
    BridgeError, CompletionIr, ContentIr, MessageIr, RequestIr, ResponseIr, ResponseMetaIr, RoleIr,
    SseFrame, StreamEventIr, ToolCallIr, ToolChoiceIr, ToolDefinitionIr, UsageIr, WireProtocol,
};
use super::reasoning::{media_from_url, media_to_url};
use super::request::{
    add_extension, object, optional_bool, optional_string, parse_arguments, reject_unknown_fields,
    required_string, text_content, text_from_content, tool_call, tool_definition, tool_result,
    validate_target_representability, validate_tool_state, EXT_CHAT_PARALLEL_TOOL_CALLS,
    EXT_PROVIDER_CACHE_MODE, EXT_RESPONSES_PARALLEL_TOOL_CALLS,
};
use super::response::{
    generated_created_at, map_upstream_error, normalize_total_tokens, optional_u64, required_array,
    required_object, required_string as response_required_string, required_u64, response_model,
    safe_error_message, target_response_id,
};
use super::stream::StreamState;

pub(super) fn parse_request(body: &Value) -> Result<RequestIr, BridgeError> {
    let object = object(body)?;
    reject_unknown_fields(
        object,
        &[
            "model",
            "stream",
            "messages",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "max_tokens",
            "max_completion_tokens",
            "temperature",
            "top_p",
            "stop",
            "reasoning_effort",
            "metadata",
        ],
    )?;
    let model = required_string(object, "model")?;
    let stream = optional_bool(object, "stream", false)?;
    let mut extensions = BTreeMap::new();
    add_extension(
        &mut extensions,
        EXT_CHAT_PARALLEL_TOOL_CALLS,
        object.get("parallel_tool_calls"),
    );
    optional_bool(object, "parallel_tool_calls", true)?;

    let mut messages = Vec::new();
    for value in object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(BridgeError::InvalidRequest)?
    {
        append_message(&mut messages, parse_message(value)?)?;
    }

    let mut generation =
        super::request::parse_generation(object, &["max_tokens", "max_completion_tokens"], "stop")?;
    generation.reasoning_effort = optional_string(object, "reasoning_effort")?;

    let ir = RequestIr {
        protocol: WireProtocol::OpenAiChat,
        model,
        messages,
        tools: parse_tools(object.get("tools"))?,
        tool_choice: parse_tool_choice(object.get("tool_choice"))?,
        generation,
        stream,
        metadata: object.get("metadata").cloned(),
        extensions,
    };
    validate_tool_state(&ir.messages)?;
    Ok(ir)
}

fn parse_message(value: &Value) -> Result<MessageIr, BridgeError> {
    let object = object(value)?;
    let role = required_string(object, "role")?;
    let name = optional_string(object, "name")?;
    match role.as_str() {
        "system" => {
            reject_unknown_fields(object, &["role", "content", "name"])?;
            Ok(MessageIr {
                role: RoleIr::System,
                content: parse_chat_content(object.get("content"), "messages.system.content")?,
                name,
                item_id: None,
            })
        }
        "developer" => {
            reject_unknown_fields(object, &["role", "content", "name"])?;
            Ok(MessageIr {
                role: RoleIr::Developer,
                content: parse_chat_content(object.get("content"), "messages.developer.content")?,
                name,
                item_id: None,
            })
        }
        "user" => {
            reject_unknown_fields(object, &["role", "content", "name"])?;
            Ok(MessageIr {
                role: RoleIr::User,
                content: parse_chat_content(object.get("content"), "messages.user.content")?,
                name,
                item_id: None,
            })
        }
        "assistant" => parse_assistant_message(object, name),
        "tool" => {
            reject_unknown_fields(object, &["role", "tool_call_id", "content"])?;
            let call_id = required_string(object, "tool_call_id")?;
            let content = parse_chat_content(object.get("content"), "messages.tool.content")?;
            Ok(MessageIr {
                role: RoleIr::Tool,
                content: vec![ContentIr::ToolResult(tool_result(call_id, content, false))],
                name,
                item_id: None,
            })
        }
        other => Err(super::request::invalid_role(other)),
    }
}

fn parse_assistant_message(
    object: &Map<String, Value>,
    name: Option<String>,
) -> Result<MessageIr, BridgeError> {
    reject_unknown_fields(object, &["role", "content", "name", "tool_calls"])?;
    let mut content = match object.get("content") {
        Some(Value::Null) | None => Vec::new(),
        Some(value) => parse_chat_content(Some(value), "messages.assistant.content")?,
    };
    if let Some(tool_calls) = object.get("tool_calls") {
        for (index, value) in tool_calls
            .as_array()
            .ok_or(BridgeError::InvalidRequest)?
            .iter()
            .enumerate()
        {
            let call_object = super::request::object(value)?;
            reject_unknown_fields(call_object, &["id", "type", "function"])?;
            if call_object.get("type").and_then(Value::as_str) != Some("function") {
                return Err(BridgeError::Unsupported {
                    field: "messages.assistant.tool_calls.type".to_string(),
                });
            }
            let function = call_object
                .get("function")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidRequest)?;
            reject_unknown_fields(function, &["name", "arguments"])?;
            content.push(ContentIr::ToolUse(tool_call(
                required_string(call_object, "id")?,
                None,
                index,
                required_string(function, "name")?,
                parse_arguments(
                    function
                        .get("arguments")
                        .ok_or(BridgeError::InvalidRequest)?,
                )?,
            )));
        }
    }
    Ok(MessageIr {
        role: RoleIr::Assistant,
        content,
        name,
        item_id: None,
    })
}

fn parse_chat_content(value: Option<&Value>, field: &str) -> Result<Vec<ContentIr>, BridgeError> {
    let value = value.ok_or(BridgeError::InvalidRequest)?;
    if value.is_string() {
        return text_content(value, field);
    }
    value
        .as_array()
        .ok_or(BridgeError::InvalidRequest)?
        .iter()
        .map(|part| {
            let object = object(part)?;
            match object.get("type").and_then(Value::as_str) {
                Some("text") | Some("input_text") => {
                    reject_unknown_fields(object, &["type", "text"])?;
                    Ok(ContentIr::Text(required_string(object, "text")?))
                }
                Some("image_url") => {
                    reject_unknown_fields(object, &["type", "image_url"])?;
                    Ok(ContentIr::Image(parse_image_url(
                        object.get("image_url").ok_or(BridgeError::InvalidRequest)?,
                    )?))
                }
                Some(other) => Err(BridgeError::Unsupported {
                    field: format!("{field}.type:{other}"),
                }),
                None => Err(BridgeError::InvalidRequest),
            }
        })
        .collect()
}

fn parse_image_url(value: &Value) -> Result<super::ir::MediaIr, BridgeError> {
    if let Some(url) = value.as_str() {
        return media_from_url(url);
    }
    let object = object(value)?;
    reject_unknown_fields(object, &["url"])?;
    media_from_url(&required_string(object, "url")?)
}

fn append_message(messages: &mut Vec<MessageIr>, message: MessageIr) -> Result<(), BridgeError> {
    if message.role == RoleIr::Assistant
        && messages
            .last()
            .is_some_and(|last| last.role == RoleIr::Assistant)
        && (message.content.iter().any(is_tool_use)
            || messages
                .last()
                .is_some_and(|last| last.content.iter().any(is_tool_use)))
    {
        let previous = messages.last_mut().ok_or(BridgeError::InvalidRequest)?;
        for content in message.content {
            if let ContentIr::ToolUse(mut call) = content {
                call.index = previous
                    .content
                    .iter()
                    .filter(|item| matches!(item, ContentIr::ToolUse(_)))
                    .count();
                previous.content.push(ContentIr::ToolUse(call));
            } else {
                previous.content.push(content);
            }
        }
        return Ok(());
    }
    messages.push(message);
    Ok(())
}

fn is_tool_use(content: &ContentIr) -> bool {
    matches!(content, ContentIr::ToolUse(_))
}

fn parse_tools(value: Option<&Value>) -> Result<Vec<ToolDefinitionIr>, BridgeError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or(BridgeError::InvalidRequest)?
        .iter()
        .map(|value| {
            let object = object(value)?;
            reject_unknown_fields(object, &["type", "function"])?;
            if object.get("type").and_then(Value::as_str) != Some("function") {
                return Err(BridgeError::Unsupported {
                    field: "tools.type".to_string(),
                });
            }
            let function = object
                .get("function")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidRequest)?;
            reject_unknown_fields(function, &["name", "description", "parameters"])?;
            let parameters = function
                .get("parameters")
                .cloned()
                .ok_or(BridgeError::InvalidRequest)?;
            if !parameters.is_object() {
                return Err(BridgeError::InvalidRequest);
            }
            Ok(tool_definition(
                required_string(function, "name")?,
                optional_string(function, "description")?,
                parameters,
            ))
        })
        .collect()
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
    reject_unknown_fields(object, &["type", "function"])?;
    if object.get("type").and_then(Value::as_str) != Some("function") {
        return Err(BridgeError::Unsupported {
            field: "tool_choice.type".to_string(),
        });
    }
    let function = object
        .get("function")
        .and_then(Value::as_object)
        .ok_or(BridgeError::InvalidRequest)?;
    reject_unknown_fields(function, &["name"])?;
    Ok(Some(ToolChoiceIr::Tool {
        name: required_string(function, "name")?,
    }))
}

pub(super) fn encode_request(ir: &RequestIr, model: &str) -> Result<Value, BridgeError> {
    validate_target_representability(ir, WireProtocol::OpenAiChat)?;
    let deepseek_cache = ir
        .extensions
        .get(EXT_PROVIDER_CACHE_MODE)
        .and_then(Value::as_str)
        == Some("deepseek");
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert("stream".to_string(), Value::Bool(ir.stream));
    let mut messages = Vec::new();
    if deepseek_cache {
        // DeepSeek prefix cache requires a byte-exact stable prefix. We only
        // proceed when system messages are already leading (guard above).
        let system_count = ir
            .messages
            .iter()
            .filter(|message| message.role == RoleIr::System)
            .count();
        let already_leading = ir
            .messages
            .iter()
            .take(system_count)
            .all(|message| message.role == RoleIr::System);
        if !already_leading {
            // 中部 System 被悄悄前移会改变指令锚定语义——fail-closed。
            return Err(BridgeError::Unsupported {
                field: "deepseek_cache.mid_conversation_system".to_string(),
            });
        }
        for message in &ir.messages {
            encode_message(message, &mut messages)?;
        }
    } else {
        for message in &ir.messages {
            encode_message(message, &mut messages)?;
        }
    }
    body.insert("messages".to_string(), Value::Array(messages));
    if !ir.tools.is_empty() {
        // strict 回填必须按“原始声明索引”对位：deepseek 分支会按名排序，
        // 位置 zip 会把 strict 挂到错误工具上（P2 地雷）。
        let strict_by_name: std::collections::BTreeMap<&str, bool> = ir
            .extensions
            .get(super::request::EXT_CHAT_TOOL_STRICT_LIST)
            .and_then(Value::as_array)
            .map(|list| {
                ir.tools
                    .iter()
                    .zip(list)
                    .filter_map(|(tool, strict)| {
                        strict.as_bool().map(|value| (tool.name.as_str(), value))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut tools: Vec<Value> = if deepseek_cache {
            // DeepSeek prefix cache matches the full tools block byte-for-byte;
            // a stable name sort keeps the encoded block deterministic across
            // requests regardless of declaration order.
            let mut ordered = ir.tools.clone();
            ordered.sort_by(|left, right| left.name.cmp(&right.name));
            ordered.iter().map(encode_tool).collect::<Result<_, _>>()?
        } else {
            ir.tools.iter().map(encode_tool).collect::<Result<_, _>>()?
        };
        if !strict_by_name.is_empty() {
            for tool in tools.iter_mut() {
                if let Some(name) = tool
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                {
                    if let Some(strict) = strict_by_name.get(name) {
                        if let Some(function) =
                            tool.get_mut("function").and_then(Value::as_object_mut)
                        {
                            function.insert("strict".to_string(), Value::Bool(*strict));
                        }
                    }
                }
            }
        }
        body.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(choice) = &ir.tool_choice {
        body.insert("tool_choice".to_string(), encode_tool_choice(choice)?);
    }
    if let Some(value) = ir.generation.max_tokens {
        body.insert("max_tokens".to_string(), json!(value));
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
        body.insert("reasoning_effort".to_string(), json!(value));
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
            "created",
            "model",
            "choices",
            "usage",
            "system_fingerprint",
            "service_tier",
            "cost",
            "request_id",
        ],
    )
    .map_err(map_upstream_error)?;
    if object.get("object").and_then(Value::as_str) != Some("chat.completion") {
        return Err(BridgeError::InvalidUpstream);
    }
    let id = response_required_string(object, "id")?;
    let model = response_required_string(object, "model")?;
    let choices = required_array(object, "choices")?;
    if choices.len() != 1 {
        return if choices.is_empty() {
            Err(BridgeError::InvalidUpstream)
        } else {
            Err(BridgeError::Unsupported {
                field: "choices".to_string(),
            })
        };
    }
    let choice = choices[0].as_object().ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(choice, &["index", "message", "finish_reason", "logprobs"])
        .map_err(map_upstream_error)?;
    if choice.get("logprobs").is_some_and(|value| !value.is_null()) {
        return Err(BridgeError::Unsupported {
            field: "choices.logprobs".to_string(),
        });
    }
    if required_u64(choice, "index")? != 0 {
        return Err(BridgeError::Unsupported {
            field: "choices.index".to_string(),
        });
    }
    let message = required_object(choice, "message")?;
    reject_unknown_fields(
        message,
        &[
            "role",
            "content",
            "tool_calls",
            "refusal",
            "reasoning_content",
            "function_call",
        ],
    )
    .map_err(map_upstream_error)?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(BridgeError::InvalidUpstream);
    }

    let mut content = match message.get("content") {
        Some(Value::String(text)) => vec![ContentIr::Text(text.clone())],
        Some(Value::Null) | None => Vec::new(),
        Some(value) => parse_response_content(value)?,
    };
    // reasoning_content（DeepSeek/GLM 思考过程）：转为普通文本块，
    // 让 anthropic 客户端能看到思考内容；绝不产生未签名 thinking。
    if let Some(text) = message.get("reasoning_content").and_then(Value::as_str) {
        if !text.is_empty() {
            content.insert(0, ContentIr::Text(text.to_string()));
        }
    }
    if message.get("refusal").is_some_and(|value| !value.is_null()) {
        return Err(BridgeError::Unsupported {
            field: "choices.message.refusal".to_string(),
        });
    }
    if message
        .get("tool_calls")
        .is_some_and(|value| !value.is_null())
    {
        let tool_calls = message["tool_calls"]
            .as_array()
            .ok_or(BridgeError::InvalidUpstream)?;
        for (index, value) in tool_calls.iter().enumerate() {
            let call_object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(call_object, &["id", "type", "function", "index"])
                .map_err(map_upstream_error)?;
            if call_object.get("type").and_then(Value::as_str) != Some("function") {
                return Err(BridgeError::Unsupported {
                    field: "choices.message.tool_calls.type".to_string(),
                });
            }
            let function = required_object(call_object, "function")?;
            reject_unknown_fields(function, &["name", "arguments"]).map_err(map_upstream_error)?;
            let arguments = super::request::parse_arguments(
                function
                    .get("arguments")
                    .ok_or(BridgeError::InvalidUpstream)?,
            )
            .map_err(map_upstream_error)?;
            content.push(ContentIr::ToolUse(super::request::tool_call(
                response_required_string(call_object, "id")?,
                None,
                index,
                response_required_string(function, "name")?,
                arguments,
            )));
        }
    }
    if content.is_empty() {
        return Err(BridgeError::InvalidUpstream);
    }
    let finish_reason = response_required_string(choice, "finish_reason")?;
    let finish_reason = tolerated_finish_reason(&finish_reason)?;
    let has_tool_call = content
        .iter()
        .any(|item| matches!(item, ContentIr::ToolUse(_)));
    if (finish_reason == "tool_calls") != has_tool_call {
        return Err(BridgeError::InvalidUpstream);
    }
    let usage = decode_usage(object.get("usage"))?;
    Ok(ResponseIr {
        meta: ResponseMetaIr {
            id: Some(id),
            model: Some(model),
        },
        content,
        usage,
        completion: CompletionIr {
            finish_reason: Some(finish_reason.to_string()),
            stop_reason: Some(chat_stop_reason(finish_reason).to_string()),
            status: Some(if matches!(finish_reason, "length" | "content_filter") {
                "incomplete".to_string()
            } else {
                "completed".to_string()
            }),
            error: None,
        },
        extensions: BTreeMap::new(),
    })
}

pub(super) fn encode_response(ir: &ResponseIr) -> Result<Value, BridgeError> {
    let model = response_model(ir)?;
    if ir.content.is_empty() {
        return Err(BridgeError::InvalidUpstream);
    }
    let finish_reason = chat_finish_reason(ir)?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for content in &ir.content {
        match content {
            ContentIr::Text(value) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(value);
            }
            ContentIr::Thinking {
                text: value,
                signature: None,
            } => {
                // 无签名 thinking 对 chat 上游可表达：折叠为文本（内容不丢）。
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(value);
            }
            ContentIr::Thinking {
                signature: Some(_), ..
            } => {
                // 签名 thinking 对 chat 上游无法表达：fail closed，不剥离。
                return Err(BridgeError::Unsupported {
                    field: "thinking.signature".to_string(),
                });
            }
            ContentIr::RedactedThinking { .. } => {
                // 密文 thinking 对 chat 上游无法表达：fail closed，不剥离。
                return Err(BridgeError::Unsupported {
                    field: "redacted_thinking".to_string(),
                });
            }
            ContentIr::ToolUse(call) => {
                if call.item_id.is_some() {
                    return Err(BridgeError::Unsupported {
                        field: "responses.item_id".to_string(),
                    });
                }
                tool_calls.push(encode_tool_call(call)?);
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
    let message_content = if text.is_empty() && !tool_calls.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    };
    let mut message = json!({
        "role": "assistant",
        "content": message_content
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    let mut result = json!({
        "id": target_response_id(ir, WireProtocol::OpenAiChat),
        "object": "chat.completion",
        "created": generated_created_at(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
            "logprobs": null
        }]
    });
    if let Some(usage) = encode_usage(ir.usage.as_ref())? {
        result["usage"] = usage;
    }
    Ok(result)
}

pub(super) fn encode_error_response(ir: &ResponseIr) -> Result<Value, BridgeError> {
    let _ = ir;
    Ok(json!({
        "error": {
            "message": safe_error_message(),
            "type": "invalid_upstream",
            "param": null,
            "code": "invalid_upstream"
        }
    }))
}

pub(super) fn decode_stream_frame(
    frame: &SseFrame,
    state: &mut StreamState,
) -> Result<Vec<StreamEventIr>, BridgeError> {
    if state.is_terminal() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_str(&frame.data).map_err(|_| BridgeError::InvalidUpstream)?;
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(
        object,
        &[
            "id",
            "object",
            "created",
            "model",
            "system_fingerprint",
            "choices",
            "usage",
            "error",
            "cost",
        ],
    )
    .map_err(map_upstream_error)?;
    if object.get("error").is_some_and(|error| !error.is_null()) {
        let error = object
            .get("error")
            .and_then(Value::as_object)
            .ok_or(BridgeError::InvalidUpstream)?;
        reject_unknown_fields(error, &["message", "type", "param", "code"])
            .map_err(map_upstream_error)?;
        let event = StreamEventIr::Failed(BridgeError::InvalidUpstream);
        state.apply_event(&event)?;
        return Ok(vec![event]);
    }
    let id = stream_required_string(object, "id")?;
    let model = stream_required_string(object, "model")?;
    if !state.is_started() {
        let event = StreamEventIr::Started(ResponseMetaIr {
            id: Some(id),
            model: Some(model),
        });
        state.apply_event(&event)?;
        let mut events = vec![event];
        return decode_chat_payload(object, state, &mut events);
    }
    state.require_response_identity(&id, &model)?;
    let mut events = Vec::new();
    decode_chat_payload(object, state, &mut events)
}

fn decode_chat_payload(
    object: &Map<String, Value>,
    state: &mut StreamState,
    events: &mut Vec<StreamEventIr>,
) -> Result<Vec<StreamEventIr>, BridgeError> {
    if let Some(usage) = object.get("usage") {
        if !usage.is_null() {
            let event = StreamEventIr::Usage(decode_stream_usage(usage)?);
            state.apply_event(&event)?;
            events.push(event);
        }
    }
    let choices = object
        .get("choices")
        .and_then(Value::as_array)
        .ok_or(BridgeError::InvalidUpstream)?;
    if !choices.is_empty() {
        state.require_no_pending_completion()?;
    }
    if choices.len() > 1 {
        return Err(BridgeError::Unsupported {
            field: "stream.choices".to_string(),
        });
    }
    let Some(choice) = choices.first().and_then(Value::as_object) else {
        return Ok(events.clone());
    };
    reject_unknown_fields(choice, &["index", "delta", "finish_reason", "logprobs"])
        .map_err(map_upstream_error)?;
    if choice.get("index").and_then(Value::as_u64) != Some(0) {
        return Err(BridgeError::Unsupported {
            field: "stream.choices.index".to_string(),
        });
    }
    let delta = choice
        .get("delta")
        .and_then(Value::as_object)
        .ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(
        delta,
        &[
            "role",
            "content",
            "tool_calls",
            "reasoning_content",
            "function_call",
        ],
    )
    .map_err(map_upstream_error)?;
    if let Some(role) = delta.get("role") {
        if !role.is_null() && role.as_str() != Some("assistant") {
            return Err(BridgeError::InvalidUpstream);
        }
    }
    if let Some(text) = delta.get("content") {
        if !text.is_null() {
            let text = text.as_str().ok_or(BridgeError::InvalidUpstream)?;
            if !text.is_empty() {
                let event = StreamEventIr::TextDelta {
                    text: text.to_string(),
                };
                state.apply_event(&event)?;
                events.push(event);
            }
        }
    }
    // reasoning_content 流式增量：转普通文本 delta，让 anthropic
    // 客户端能看到思考过程；保持观测到的顺序转发。
    if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
        if !text.is_empty() {
            let event = StreamEventIr::TextDelta {
                text: text.to_string(),
            };
            state.apply_event(&event)?;
            events.push(event);
        }
    }
    if delta
        .get("tool_calls")
        .is_some_and(|value| !value.is_null())
    {
        for call in delta["tool_calls"]
            .as_array()
            .ok_or(BridgeError::InvalidUpstream)?
        {
            let call = call.as_object().ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(call, &["index", "id", "type", "function"])
                .map_err(map_upstream_error)?;
            if call
                .get("type")
                .is_some_and(|value| value.as_str() != Some("function"))
            {
                return Err(BridgeError::Unsupported {
                    field: "stream.tool_calls.type".to_string(),
                });
            }
            let index = stream_required_u64(call, "index")? as usize;
            let function = call
                .get("function")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(function, &["name", "arguments"]).map_err(map_upstream_error)?;
            let call_id = call.get("id").and_then(Value::as_str);
            let name = function.get("name").and_then(Value::as_str);
            if let (Some(call_id), Some(name)) = (call_id, name) {
                state.register_wire_tool(WireProtocol::OpenAiChat, index, call_id)?;
                let logical_index = state.next_tool_index();
                let event = StreamEventIr::ToolCallStarted(ToolCallIr {
                    call_id: call_id.to_string(),
                    item_id: None,
                    index: logical_index,
                    name: name.to_string(),
                    arguments: json!({}),
                });
                state.apply_event(&event)?;
                events.push(event);
            }
            if let Some(arguments) = function.get("arguments") {
                let arguments = arguments.as_str().ok_or(BridgeError::InvalidUpstream)?;
                if !arguments.is_empty() {
                    let call_id = match call_id {
                        Some(call_id) => call_id.to_string(),
                        None => state.wire_tool_call_id(WireProtocol::OpenAiChat, index)?,
                    };
                    let event = StreamEventIr::ToolCallArgumentsDelta {
                        call_id,
                        delta: arguments.to_string(),
                    };
                    state.apply_event(&event)?;
                    events.push(event);
                }
            }
        }
    }
    if let Some(reason) = choice.get("finish_reason") {
        if !reason.is_null() {
            let reason = reason.as_str().ok_or(BridgeError::InvalidUpstream)?;
            state.set_pending_completion(chat_completion(reason)?)?;
        }
    }
    Ok(events.clone())
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
            frames.push(chat_frame(
                state,
                json!({ "role": "assistant" }),
                None,
                None,
            ));
        }
        StreamEventIr::TextDelta { text } => {
            frames.push(chat_frame(state, json!({ "content": text }), None, None));
        }
        StreamEventIr::ReasoningDelta { text } => {
            // 推理增量折叠为正文（对齐非流式 decode 的 reasoning_content→正文
            // 降级）：headers 已写出，此处硬拒会让推理流必断。
            frames.push(chat_frame(state, json!({ "content": text }), None, None));
        }
        StreamEventIr::ToolCallStarted(call) => {
            state.register_wire_tool(WireProtocol::OpenAiChat, call.index, &call.call_id)?;
            let arguments = state.tool_state(&call.call_id)?.arguments().to_string();
            frames.push(chat_frame(
                state,
                json!({
                    "tool_calls": [{
                        "index": call.index,
                        "id": call.call_id,
                        "type": "function",
                         "function": { "name": call.name, "arguments": arguments }
                    }]
                }),
                None,
                None,
            ));
        }
        StreamEventIr::ToolCallArgumentsDelta { call_id, delta } => {
            let index = state.tool_state(call_id)?.index();
            frames.push(chat_frame(
                state,
                json!({ "tool_calls": [{ "index": index, "function": { "arguments": delta } }] }),
                None,
                None,
            ));
        }
        StreamEventIr::ToolCallFinished { .. } => {}
        StreamEventIr::Usage(usage) => {
            frames.push(chat_frame(
                state,
                json!({}),
                None,
                Some(encode_stream_usage(usage)),
            ));
        }
        StreamEventIr::Completed(completion) => {
            if state.encoded_terminal() {
                return Err(BridgeError::ToolState);
            }
            frames.push(chat_frame(
                state,
                json!({}),
                Some(chat_stream_finish_reason(completion)?),
                None,
            ));
            frames.push(SseFrame {
                event: None,
                data: "[DONE]".to_string(),
            });
            state.mark_encoded_terminal();
        }
        StreamEventIr::Failed(_) => {
            frames.push(SseFrame {
                event: None,
                data: serde_json::to_string(&json!({
                    "error": { "message": safe_error_message(), "type": "invalid_upstream", "code": "invalid_upstream" }
                }))
                .expect("stream error is serializable"),
            });
            frames.push(SseFrame {
                event: None,
                data: "[DONE]".to_string(),
            });
            state.mark_encoded_terminal();
        }
    }
    Ok(frames)
}

fn chat_frame(
    state: &StreamState,
    delta: Value,
    finish_reason: Option<&str>,
    usage: Option<Value>,
) -> SseFrame {
    let mut value = json!({
        "id": state.meta().id.clone().unwrap_or_else(|| "chatcmpl-stream".to_string()),
        "object": "chat.completion.chunk",
        "created": generated_created_at(),
        "model": state.meta().model.clone().unwrap_or_else(|| "unknown".to_string()),
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
    });
    if let Some(usage) = usage {
        value["choices"] = json!([]);
        value["usage"] = usage;
    }
    SseFrame {
        event: None,
        data: serde_json::to_string(&value).expect("Chat stream frame is serializable"),
    }
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

fn decode_stream_usage(value: &Value) -> Result<UsageIr, BridgeError> {
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(
        object,
        &[
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
            "prompt_cache_hit_tokens",
            "prompt_cache_miss_tokens",
            "cached_tokens",
            "cache_write_tokens",
            "timing",
        ],
    )
    .map_err(map_upstream_error)?;
    Ok(UsageIr {
        input_tokens: stream_optional_u64(object, "prompt_tokens")?,
        output_tokens: stream_optional_u64(object, "completion_tokens")?,
        total_tokens: stream_optional_u64(object, "total_tokens")?,
        cache_read_tokens: stream_detail_u64(object, "prompt_tokens_details", "cached_tokens")?
            .or(stream_optional_u64(object, "prompt_cache_hit_tokens")?)
            .or(stream_optional_u64(object, "cached_tokens")?),
        cache_write_tokens: stream_optional_u64(object, "cache_write_tokens")?,
        reasoning_tokens: stream_detail_u64(
            object,
            "completion_tokens_details",
            "reasoning_tokens",
        )?,
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
    reject_unknown_fields(details, &[detail]).map_err(map_upstream_error)?;
    stream_optional_u64(details, detail)
}

fn encode_stream_usage(usage: &UsageIr) -> Value {
    let mut value = json!({
        "prompt_tokens": usage.input_tokens.unwrap_or(0),
        "completion_tokens": usage.output_tokens.unwrap_or(0),
        "total_tokens": usage.total_tokens.unwrap_or_else(|| {
            usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0)
        })
    });
    if let Some(tokens) = usage.cache_read_tokens {
        value["prompt_tokens_details"] = json!({ "cached_tokens": tokens });
    }
    if let Some(tokens) = usage.reasoning_tokens {
        value["completion_tokens_details"] = json!({ "reasoning_tokens": tokens });
    }
    value
}

fn chat_completion(reason: &str) -> Result<CompletionIr, BridgeError> {
    let reason = tolerated_finish_reason(reason)?;
    let status = if matches!(reason, "length" | "content_filter") {
        "incomplete"
    } else {
        "completed"
    };
    Ok(CompletionIr {
        finish_reason: Some(reason.to_string()),
        stop_reason: Some(chat_stop_reason(reason).to_string()),
        status: Some(status.to_string()),
        error: None,
    })
}

/// Accepts the OpenAI-compatible finish reasons plus provider-specific
/// extensions. Resource exhaustion and safety intercepts normalize to the
/// closest incomplete semantics (length / content_filter) so clients can tell
/// them apart from a clean stop; network errors and legacy function calls keep
/// stop semantics. Genuinely unknown reasons still reject.
fn tolerated_finish_reason(reason: &str) -> Result<&str, BridgeError> {
    match reason {
        "stop" | "length" | "tool_calls" | "content_filter" => Ok(reason),
        // 有损归一必须保留“未正常收尾”信号：
        // - sensitive：内容风控拦截 → content_filter（incomplete）
        // - insufficient_system_resource：上游资源截断 → length（incomplete）
        // 仅网络错误与遗留 function_call 维持 stop 旧语义。
        "sensitive" => Ok("content_filter"),
        "insufficient_system_resource" => Ok("length"),
        "network_error" | "function_call" => Ok("stop"),
        _ => Err(BridgeError::InvalidUpstream),
    }
}

fn chat_stream_finish_reason(completion: &CompletionIr) -> Result<&'static str, BridgeError> {
    chat_finish_reason(&ResponseIr {
        meta: ResponseMetaIr::default(),
        content: vec![ContentIr::Text(String::new())],
        usage: None,
        completion: completion.clone(),
        extensions: BTreeMap::new(),
    })
}

fn parse_response_content(value: &Value) -> Result<Vec<ContentIr>, BridgeError> {
    let parts = value.as_array().ok_or(BridgeError::InvalidUpstream)?;
    parts
        .iter()
        .map(|part| {
            let object = part.as_object().ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(object, &["type", "text"]).map_err(map_upstream_error)?;
            if object.get("type").and_then(Value::as_str) != Some("text") {
                return Err(BridgeError::Unsupported {
                    field: "choices.message.content.type".to_string(),
                });
            }
            Ok(ContentIr::Text(response_required_string(object, "text")?))
        })
        .collect()
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
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
            "prompt_cache_hit_tokens",
            "prompt_cache_miss_tokens",
            "cached_tokens",
            "cache_write_tokens",
            "timing",
        ],
    )
    .map_err(map_upstream_error)?;
    let input_tokens = Some(required_u64(object, "prompt_tokens")?);
    let output_tokens = Some(required_u64(object, "completion_tokens")?);
    let total_tokens = normalize_total_tokens(
        input_tokens,
        output_tokens,
        optional_u64(object, "total_tokens")?,
    )?;
    let cache_read_tokens = if object.contains_key("prompt_tokens_details") {
        let details = required_object(object, "prompt_tokens_details")?;
        reject_unknown_fields(details, &["cached_tokens"]).map_err(map_upstream_error)?;
        optional_u64(details, "cached_tokens")?
    } else {
        None
    }
    .or(optional_u64(object, "prompt_cache_hit_tokens")?)
    .or(optional_u64(object, "cached_tokens")?);
    let reasoning_tokens = if object.contains_key("completion_tokens_details") {
        let details = required_object(object, "completion_tokens_details")?;
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
        cache_write_tokens: optional_u64(object, "cache_write_tokens")?,
        reasoning_tokens,
    }))
}

fn encode_usage(usage: Option<&UsageIr>) -> Result<Option<Value>, BridgeError> {
    let Some(usage) = usage else {
        return Ok(None);
    };
    let input_tokens = usage.input_tokens.ok_or(BridgeError::InvalidUpstream)?;
    let output_tokens = usage.output_tokens.ok_or(BridgeError::InvalidUpstream)?;
    let total_tokens =
        normalize_total_tokens(Some(input_tokens), Some(output_tokens), usage.total_tokens)?
            .ok_or(BridgeError::InvalidUpstream)?;
    // cache_write 在 OpenAI chat 无对应字段：与流式路径一致，静默丢弃。
    let _ = usage.cache_write_tokens;
    let mut result = json!({
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
        "total_tokens": total_tokens
    });
    if let Some(value) = usage.cache_read_tokens {
        result["prompt_tokens_details"] = json!({ "cached_tokens": value });
    }
    if let Some(value) = usage.reasoning_tokens {
        result["completion_tokens_details"] = json!({ "reasoning_tokens": value });
    }
    Ok(Some(result))
}

fn chat_stop_reason(finish_reason: &str) -> &'static str {
    match finish_reason {
        "tool_calls" => "tool_use",
        "length" => "max_tokens",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

fn chat_finish_reason(ir: &ResponseIr) -> Result<&'static str, BridgeError> {
    if let Some(reason) = ir.completion.finish_reason.as_deref() {
        return match reason {
            "stop" | "length" | "tool_calls" | "content_filter" => Ok(match reason {
                "stop" => "stop",
                "length" => "length",
                "tool_calls" => "tool_calls",
                "content_filter" => "content_filter",
                _ => unreachable!(),
            }),
            _ => Err(BridgeError::InvalidUpstream),
        };
    }
    match ir.completion.stop_reason.as_deref() {
        Some("end_turn") | Some("stop_sequence") => Ok("stop"),
        Some("max_tokens") => Ok("length"),
        Some("tool_use") => Ok("tool_calls"),
        Some("refusal") => Ok("content_filter"),
        _ if ir.completion.status.as_deref() == Some("incomplete") => Ok("length"),
        _ if ir
            .content
            .iter()
            .any(|item| matches!(item, ContentIr::ToolUse(_))) =>
        {
            Ok("tool_calls")
        }
        _ if ir.completion.status.as_deref() == Some("completed") => Ok("stop"),
        _ => Err(BridgeError::InvalidUpstream),
    }
}

fn encode_message(message: &MessageIr, messages: &mut Vec<Value>) -> Result<(), BridgeError> {
    match message.role {
        RoleIr::System => {
            let mut value = json!({
                "role": "system",
                "content": encode_chat_content(&message.content)?,
            });
            if let Some(name) = &message.name {
                value["name"] = json!(name);
            }
            messages.push(value);
        }
        RoleIr::Developer => {
            let mut value = json!({
                "role": "developer",
                "content": encode_chat_content(&message.content)?,
            });
            if let Some(name) = &message.name {
                value["name"] = json!(name);
            }
            messages.push(value);
        }
        RoleIr::User => encode_user_message(message, messages)?,
        RoleIr::Tool => {
            for content in &message.content {
                let ContentIr::ToolResult(result) = content else {
                    return Err(BridgeError::Unsupported {
                        field: "messages.tool.content".to_string(),
                    });
                };
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": result.call_id,
                    "content": text_from_content(&result.content)?,
                }));
            }
        }
        RoleIr::Assistant => {
            let (content, tool_calls) = encode_assistant_content(&message.content)?;
            let mut value = json!({ "role": "assistant", "content": content });
            if !tool_calls.is_empty() {
                value["tool_calls"] = Value::Array(tool_calls);
            }
            if let Some(name) = &message.name {
                value["name"] = json!(name);
            }
            messages.push(value);
        }
    }
    Ok(())
}

fn encode_user_message(message: &MessageIr, messages: &mut Vec<Value>) -> Result<(), BridgeError> {
    let mut content = Vec::new();
    for item in &message.content {
        if let ContentIr::ToolResult(result) = item {
            if !content.is_empty() {
                let mut value = json!({
                    "role": "user",
                    "content": take_chat_content(&content)?
                });
                if let Some(name) = &message.name {
                    value["name"] = json!(name);
                }
                messages.push(value);
                content.clear();
            }
            messages.push(json!({
                "role": "tool",
                "tool_call_id": result.call_id,
                "content": text_from_content(&result.content)?,
            }));
        } else {
            content.push(item.clone());
        }
    }
    if !content.is_empty() || message.content.is_empty() {
        let mut value = json!({
            "role": "user",
            "content": take_chat_content(&content)?
        });
        if let Some(name) = &message.name {
            value["name"] = json!(name);
        }
        messages.push(value);
    }
    Ok(())
}

fn take_chat_content(content: &[ContentIr]) -> Result<Value, BridgeError> {
    encode_chat_content(content)
}

fn encode_chat_content(content: &[ContentIr]) -> Result<Value, BridgeError> {
    if content
        .iter()
        .all(|item| matches!(item, ContentIr::Text(_)))
    {
        return Ok(Value::String(text_from_content(content)?));
    }
    let mut parts = Vec::new();
    for item in content {
        match item {
            ContentIr::Text(text)
            | ContentIr::Thinking {
                text,
                signature: None,
            } => {
                parts.push(json!({ "type": "text", "text": text }));
            }
            ContentIr::Thinking {
                signature: Some(_), ..
            } => {
                // 签名 thinking 对 chat 上游无法表达：fail closed，不剥离。
                return Err(BridgeError::Unsupported {
                    field: "thinking.signature".to_string(),
                });
            }
            ContentIr::RedactedThinking { .. } => {
                // 密文 thinking 对 chat 上游无法表达：fail closed，不剥离。
                return Err(BridgeError::Unsupported {
                    field: "redacted_thinking".to_string(),
                });
            }
            ContentIr::Image(media) => parts.push(json!({
                "type": "image_url",
                "image_url": { "url": image_url(media)? },
            })),
            _ => {
                return Err(BridgeError::Unsupported {
                    field: "messages.content".to_string(),
                })
            }
        }
    }
    Ok(Value::Array(parts))
}

fn encode_assistant_content(content: &[ContentIr]) -> Result<(Value, Vec<Value>), BridgeError> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for item in content {
        match item {
            ContentIr::Text(value)
            | ContentIr::Thinking {
                text: value,
                signature: None,
            } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(value);
            }
            ContentIr::Thinking {
                signature: Some(_), ..
            } => {
                // 签名 thinking 对 chat 上游无法表达：fail closed，不剥离。
                return Err(BridgeError::Unsupported {
                    field: "thinking.signature".to_string(),
                });
            }
            ContentIr::RedactedThinking { .. } => {
                // 密文 thinking 对 chat 上游无法表达：fail closed，不剥离。
                return Err(BridgeError::Unsupported {
                    field: "redacted_thinking".to_string(),
                });
            }
            ContentIr::ToolUse(call) => tool_calls.push(encode_tool_call(call)?),
            _ => {
                return Err(BridgeError::Unsupported {
                    field: "messages.assistant.content".to_string(),
                })
            }
        }
    }
    let content = if text.is_empty() && !tool_calls.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    };
    Ok((content, tool_calls))
}

fn encode_tool_call(call: &ToolCallIr) -> Result<Value, BridgeError> {
    if call.call_id.trim().is_empty() || call.name.trim().is_empty() || !call.arguments.is_object()
    {
        return Err(BridgeError::InvalidRequest);
    }
    Ok(json!({
        "id": call.call_id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": serde_json::to_string(&call.arguments).map_err(|_| BridgeError::InvalidRequest)?
        }
    }))
}

fn image_url(media: &super::ir::MediaIr) -> Result<String, BridgeError> {
    media_to_url(media)
}

fn encode_tool(tool: &ToolDefinitionIr) -> Result<Value, BridgeError> {
    if tool.name.trim().is_empty() || !tool.input_schema.is_object() {
        return Err(BridgeError::InvalidRequest);
    }
    let mut function = json!({
        "name": tool.name,
        "parameters": tool.input_schema,
    });
    if let Some(description) = &tool.description {
        function["description"] = json!(description);
    }
    if let Some(strict) = tool.strict {
        function["strict"] = json!(strict);
    }
    Ok(json!({ "type": "function", "function": function }))
}

fn encode_tool_choice(choice: &ToolChoiceIr) -> Result<Value, BridgeError> {
    Ok(match choice {
        ToolChoiceIr::Auto => json!("auto"),
        ToolChoiceIr::Any => json!("required"),
        ToolChoiceIr::None => json!("none"),
        ToolChoiceIr::Tool { name } if !name.trim().is_empty() => {
            json!({ "type": "function", "function": { "name": name } })
        }
        ToolChoiceIr::Tool { .. } => return Err(BridgeError::InvalidRequest),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::super::{
        BridgeError, CompletionIr, ContentIr, ResponseMetaIr, RoleIr, SseFrame, StreamEventIr,
        StreamState, WireProtocol,
    };

    #[test]
    fn parser_merges_contiguous_assistant_tool_calls_and_binds_tool_results() {
        let body = json!({
            "model": "chat-fixture-model",
            "messages": [
                { "role": "system", "content": "system fixture" },
                { "role": "user", "content": "request fixture" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-one",
                        "type": "function",
                        "function": { "name": "one", "arguments": "{}" }
                    }]
                },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-two",
                        "type": "function",
                        "function": { "name": "two", "arguments": "{\"v\":1}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call-one", "content": "first result fixture" },
                { "role": "tool", "tool_call_id": "call-two", "content": "result fixture" }
            ],
            "tool_choice": "required",
            "max_tokens": 77
        });

        let ir = super::parse_request(&body).expect("Chat request parses");
        assert_eq!(ir.messages.len(), 5);
        assert_eq!(ir.messages[2].role, RoleIr::Assistant);
        assert_eq!(
            ir.messages[2]
                .content
                .iter()
                .filter_map(|content| match content {
                    ContentIr::ToolUse(call) => Some((call.call_id.as_str(), call.index)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![("call-one", 0), ("call-two", 1)]
        );
        assert!(matches!(
            ir.messages[3].content[0],
            ContentIr::ToolResult(ref result) if result.call_id == "call-one"
        ));
        assert!(matches!(
            ir.messages[4].content[0],
            ContentIr::ToolResult(ref result) if result.call_id == "call-two"
        ));
        assert_eq!(ir.generation.max_tokens, Some(77));
        assert_eq!(ir.tool_choice, Some(super::super::ToolChoiceIr::Any));
    }

    #[test]
    fn deepseek_cache_mode_rejects_mid_conversation_system() {
        let body = json!({
            "model": "chat-fixture-model",
            "messages": [
                {"role": "user", "content": "first user fixture"},
                {"role": "system", "content": "system fixture"},
                {"role": "user", "content": "follow-up fixture"}
            ],
            "tools": [
                {"type": "function", "function": {"name": "z_tool", "description": "last", "parameters": {"type": "object"}}},
                {"type": "function", "function": {"name": "a_tool", "description": "first", "parameters": {"type": "object"}}}
            ]
        });
        let mut ir = super::parse_request(&body).expect("Chat request parses");
        ir.extensions.insert(
            super::EXT_PROVIDER_CACHE_MODE.to_string(),
            json!("deepseek"),
        );

        let encoded = super::encode_request(&ir, "chat-upstream");
        // 中部 System 前移会改变指令锚定语义：必须 fail-closed。
        let error = encoded.expect_err("mid-conversation system must be rejected");
        assert!(
            matches!(error, super::BridgeError::Unsupported { .. }),
            "got: {error:?}"
        );
    }

    #[test]
    fn deepseek_cache_mode_sorts_tools_and_keeps_leading_system() {
        let body = json!({
            "model": "chat-fixture-model",
            "messages": [
                {"role": "system", "content": "system fixture"},
                {"role": "user", "content": "follow-up fixture"}
            ],
            "tools": [
                {"type": "function", "function": {"name": "z_tool", "description": "last", "parameters": {"type": "object"}}},
                {"type": "function", "function": {"name": "a_tool", "description": "first", "parameters": {"type": "object"}}}
            ]
        });
        let mut ir = super::parse_request(&body).expect("Chat request parses");
        ir.extensions.insert(
            super::EXT_PROVIDER_CACHE_MODE.to_string(),
            json!("deepseek"),
        );

        let encoded = super::encode_request(&ir, "chat-upstream").expect("Chat request encodes");
        let tools = encoded["tools"].as_array().expect("tools array");
        assert_eq!(tools[0]["function"]["name"], json!("a_tool"));
        assert_eq!(tools[1]["function"]["name"], json!("z_tool"));
        assert_eq!(
            encoded["messages"].as_array().expect("messages array")[0]["role"],
            json!("system")
        );
    }

    #[test]
    fn auto_cache_mode_preserves_original_tool_and_message_order() {
        let body = json!({
            "model": "chat-fixture-model",
            "messages": [
                {"role": "user", "content": "first user fixture"},
                {"role": "system", "content": "system fixture"},
                {"role": "user", "content": "follow-up fixture"}
            ],
            "tools": [
                {"type": "function", "function": {"name": "z_tool", "description": "last", "parameters": {"type": "object"}}},
                {"type": "function", "function": {"name": "a_tool", "description": "first", "parameters": {"type": "object"}}}
            ]
        });
        let ir = super::parse_request(&body).expect("Chat request parses");
        assert!(!ir.extensions.contains_key(super::EXT_PROVIDER_CACHE_MODE));

        let encoded = super::encode_request(&ir, "chat-upstream").expect("Chat request encodes");
        let tools = encoded["tools"].as_array().expect("tools array");
        assert_eq!(tools[0]["function"]["name"], json!("z_tool"));
        assert_eq!(tools[1]["function"]["name"], json!("a_tool"));
        let roles = encoded["messages"]
            .as_array()
            .expect("messages array")
            .iter()
            .map(|message| message["role"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            ["user", "system", "user"]
                .iter()
                .map(|role| json!(role))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn encoder_strips_opaque_reasoning_in_chat_messages() {
        // 签名/密文 thinking 块对 chat 上游无法表达：fail closed 返回
        // Unsupported，绝不静默剥离。
        let body = json!({
            "model": "chat-fixture-model",
            "messages": [{
                "role": "assistant",
                "content": [{ "type": "input_text", "text": "visible fixture" }]
            }]
        });
        let mut ir = super::parse_request(&body).expect("Chat request parses");
        ir.messages[0].content.push(ContentIr::RedactedThinking {
            data: "opaque-fixture".to_string(),
        });
        assert!(matches!(
            super::encode_request(&ir, "chat-upstream"),
            Err(BridgeError::Unsupported { field }) if field == "redacted_thinking"
        ));
    }

    #[test]
    fn encoder_rejects_signed_thinking_in_chat_messages() {
        let body = json!({
            "model": "chat-fixture-model",
            "messages": [{
                "role": "assistant",
                "content": [{ "type": "input_text", "text": "visible fixture" }]
            }]
        });
        let mut ir = super::parse_request(&body).expect("Chat request parses");
        ir.messages[0].content.push(ContentIr::Thinking {
            text: "signed fixture".to_string(),
            signature: Some("fixture-signature".to_string()),
        });
        assert!(matches!(
            super::encode_request(&ir, "chat-upstream"),
            Err(BridgeError::Unsupported { field }) if field == "thinking.signature"
        ));
    }

    #[test]
    fn encoder_rejects_opaque_reasoning_in_user_chat_content() {
        let body = json!({
            "model": "chat-fixture-model",
            "messages": [{
                "role": "user",
                "content": [{ "type": "input_text", "text": "visible fixture" }]
            }]
        });
        let mut ir = super::parse_request(&body).expect("Chat request parses");
        ir.messages[0].content.push(ContentIr::RedactedThinking {
            data: "opaque-fixture".to_string(),
        });
        assert!(matches!(
            super::encode_request(&ir, "chat-upstream"),
            Err(BridgeError::Unsupported { field }) if field == "redacted_thinking"
        ));
    }

    #[test]
    fn decoder_accepts_provider_reasoning_and_cost_metadata() {
        let body = json!({
            "id": "chat-response-fixture",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "visible fixture",
                    "reasoning_content": "provider reasoning fixture"
                },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "total_tokens": 3,
                "prompt_cache_hit_tokens": 0,
                "prompt_cache_miss_tokens": 1
            },
            "cost": "0"
        });

        let response = super::decode_response(body).expect("provider metadata should be accepted");
        // reasoning_content 对 anthropic 客户端以普通文本呈现（绝不作未签名 thinking）。
        assert_eq!(
            response.content,
            vec![
                ContentIr::Text("provider reasoning fixture".to_string()),
                ContentIr::Text("visible fixture".to_string()),
            ]
        );
    }

    #[test]
    fn decoder_treats_null_tool_calls_and_content_as_absent() {
        let body = json!({
            "id": "chat-response-null-tools",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "plain reply",
                    "tool_calls": null
                },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "total_tokens": 3
            }
        });

        let response =
            super::decode_response(body).expect("null tool_calls must decode like absent");
        assert_eq!(
            response.content,
            vec![ContentIr::Text("plain reply".to_string())]
        );
        assert!(
            !response
                .content
                .iter()
                .any(|item| { matches!(item, ContentIr::ToolUse { .. }) }),
            "null tool_calls must not create tool items"
        );
    }

    #[test]
    fn decoder_tolerates_glm_request_id_field() {
        let mut body = json!({
            "id": "chat-response-glm",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "glm" },
                "finish_reason": "sensitive",
                "logprobs": null
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
        });
        body["request_id"] = json!("req_glm_001");
        let response =
            super::decode_response(body).expect("GLM request_id and sensitive finish must decode");
        assert_eq!(response.content, vec![ContentIr::Text("glm".to_string())]);
    }

    #[test]
    fn stream_decoder_accepts_provider_reasoning_content() {
        // reasoning 增量与普通 content 分帧提供：源 JSON 对象不定义两者
        // 之间的顺序契约，一帧只放一个字段。
        let reasoning_frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-fixture",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "reasoning_content": "provider reasoning fixture"
                    },
                    "finish_reason": null,
                    "logprobs": null
                }]
            })
            .to_string(),
        };
        let content_frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-fixture",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "content": "visible fixture"
                    },
                    "finish_reason": null,
                    "logprobs": null
                }]
            })
            .to_string(),
        };
        let mut state = StreamState::new();

        let mut events = super::decode_stream_frame(&reasoning_frame, &mut state)
            .expect("provider reasoning frame should be accepted");
        events.extend(
            super::decode_stream_frame(&content_frame, &mut state)
                .expect("provider content frame should be accepted"),
        );
        let deltas: Vec<&StreamEventIr> = events
            .iter()
            .filter(|event| matches!(event, StreamEventIr::TextDelta { .. }))
            .collect();
        assert_eq!(
            deltas,
            vec![
                &StreamEventIr::TextDelta {
                    text: "provider reasoning fixture".to_string(),
                },
                &StreamEventIr::TextDelta {
                    text: "visible fixture".to_string(),
                },
            ],
            "Chat reasoning deltas must forward as ordered TextDelta events"
        );
    }

    #[test]
    fn stream_decoder_ignores_post_done_tail_frame() {
        let mut state = StreamState::new();
        state
            .apply_event(&StreamEventIr::Started(ResponseMetaIr {
                id: Some("chat-stream-fixture".to_string()),
                model: Some("chat-fixture-model".to_string()),
            }))
            .expect("Chat stream starts");
        state
            .apply_event(&StreamEventIr::TextDelta {
                text: "visible fixture".to_string(),
            })
            .expect("Chat content decodes");
        state
            .apply_event(&StreamEventIr::Completed(CompletionIr {
                finish_reason: Some("stop".to_string()),
                stop_reason: Some("end_turn".to_string()),
                status: Some("completed".to_string()),
                error: None,
            }))
            .expect("Chat stream completes at DONE");

        let frame = SseFrame {
            event: None,
            data: json!({ "choices": [], "cost": "0" }).to_string(),
        };
        let events = super::decode_stream_frame(&frame, &mut state)
            .expect("post-DONE Chat tail frame is ignored");

        assert!(events.is_empty());
        assert!(state.is_terminal());
    }

    #[test]
    fn stream_decoder_tolerates_null_role_delta() {
        let mut state = StreamState::new();
        state
            .apply_event(&StreamEventIr::Started(ResponseMetaIr {
                id: Some("chat-stream-fixture".to_string()),
                model: Some("chat-fixture-model".to_string()),
            }))
            .expect("Chat stream starts");

        let frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-fixture",
                "object": "chat.completion.chunk",
                "created": 1786423382,
                "model": "chat-fixture-model",
                "choices": [{
                    "index": 0,
                    "finish_reason": null,
                    "logprobs": null,
                    "delta": {"role": null, "content": "visible after null role"}
                }]
            })
            .to_string(),
        };
        let events =
            super::decode_stream_frame(&frame, &mut state).expect("null-role delta is tolerated");

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            StreamEventIr::TextDelta { ref text } if text == "visible after null role"
        ));
        assert!(!state.is_terminal());
    }

    #[test]
    fn decoder_maps_zen_cache_hit_tokens_to_cache_read_usage() {
        let body = json!({
            "id": "chat-response-cache",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "reply fixture" },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "prompt_cache_hit_tokens": 42
            }
        });

        let response = super::decode_response(body).expect("zen cache usage decodes");
        let usage = response.usage.expect("usage present");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.cache_read_tokens, Some(42));
    }

    #[test]
    fn decoder_prefers_nested_cached_tokens_over_top_level_hit_tokens() {
        let body = json!({
            "id": "chat-response-cache-both",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "reply fixture" },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "prompt_cache_hit_tokens": 99,
                "prompt_tokens_details": { "cached_tokens": 42 }
            }
        });

        let response = super::decode_response(body).expect("both cache shapes decode");
        let usage = response.usage.expect("usage present");
        assert_eq!(usage.cache_read_tokens, Some(42));
    }

    #[test]
    fn stream_decoder_maps_zen_cache_hit_tokens_to_cache_read_usage() {
        let frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-cache",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "prompt_cache_hit_tokens": 42
                }
            })
            .to_string(),
        };
        let mut state = StreamState::new();

        let events =
            super::decode_stream_frame(&frame, &mut state).expect("zen stream cache usage decodes");
        assert!(events.iter().any(|event| {
            matches!(event, StreamEventIr::Usage(usage) if usage.cache_read_tokens == Some(42))
        }));
    }

    fn chat_completion_response(usage: Value) -> Value {
        json!({
            "id": "chatcmpl-usage",
            "object": "chat.completion",
            "created": 1,
            "model": "zen-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "answer" },
                "finish_reason": "stop"
            }],
            "usage": usage
        })
    }

    #[test]
    fn usage_encoded_to_anthropic_excludes_cached_prefix_from_input_tokens() {
        let body = chat_completion_response(json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }));
        let ir = super::decode_response(body).expect("zen chat response decodes");
        let encoded =
            super::super::anthropic::encode_response(&ir).expect("anthropic response encodes");
        let usage = &encoded["usage"];
        assert_eq!(usage["input_tokens"], json!(60));
        assert_eq!(usage["cache_read_input_tokens"], json!(40));
        assert_eq!(usage["output_tokens"], json!(20));
    }

    #[test]
    fn usage_encoded_to_anthropic_without_cache_keeps_input_tokens_unchanged() {
        for usage in [
            json!({ "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120 }),
            json!({
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "total_tokens": 120,
                "prompt_tokens_details": { "cached_tokens": 0 }
            }),
        ] {
            let body = chat_completion_response(usage);
            let ir = super::decode_response(body).expect("zen chat response decodes");
            let encoded =
                super::super::anthropic::encode_response(&ir).expect("anthropic response encodes");
            let usage_value = &encoded["usage"];
            assert_eq!(usage_value["input_tokens"], json!(100));
            assert!(
                usage_value.get("cache_read_input_tokens").is_none(),
                "cache_read_input_tokens must be omitted when nothing is cached"
            );
        }
    }

    #[test]
    fn decoder_accepts_kimi_top_level_cached_tokens_usage() {
        let body = json!({
            "id": "chat-response-kimi",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "reply fixture" },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 19,
                "completion_tokens": 21,
                "total_tokens": 40,
                "cached_tokens": 10
            }
        });

        let response =
            super::decode_response(body).expect("Kimi top-level cached_tokens usage must decode");
        let usage = response.usage.expect("usage present");
        assert_eq!(usage.cache_read_tokens, Some(10));
    }

    #[test]
    fn decoder_prefers_nested_cached_tokens_over_top_level_cached_tokens() {
        let body = json!({
            "id": "chat-response-kimi-both",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "reply fixture" },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 19,
                "completion_tokens": 21,
                "total_tokens": 40,
                "cached_tokens": 10,
                "prompt_tokens_details": { "cached_tokens": 42 }
            }
        });

        let response = super::decode_response(body).expect("both cache shapes decode");
        let usage = response.usage.expect("usage present");
        assert_eq!(usage.cache_read_tokens, Some(42));
    }

    #[test]
    fn stream_decoder_accepts_kimi_top_level_cached_tokens_usage() {
        let frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-kimi",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [],
                "usage": {
                    "prompt_tokens": 19,
                    "completion_tokens": 21,
                    "total_tokens": 40,
                    "cached_tokens": 10
                }
            })
            .to_string(),
        };
        let mut state = StreamState::new();

        let events =
            super::decode_stream_frame(&frame, &mut state).expect("Kimi stream usage must decode");
        assert!(events.iter().any(|event| {
            matches!(event, StreamEventIr::Usage(usage) if usage.cache_read_tokens == Some(10))
        }));
    }

    #[test]
    fn decoder_tolerates_groq_timing_usage_metadata() {
        let body = json!({
            "id": "chat-response-groq",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "reply fixture" },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "total_tokens": 3,
                "timing": { "total_time_ms": 123 }
            }
        });
        super::decode_response(body).expect("Groq timing usage metadata must be ignored");

        let frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-groq",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 2,
                    "total_tokens": 3,
                    "timing": { "total_time_ms": 123 }
                }
            })
            .to_string(),
        };
        let mut state = StreamState::new();
        super::decode_stream_frame(&frame, &mut state)
            .expect("Groq stream timing usage metadata must be ignored");
    }

    #[test]
    fn decoder_accepts_openai_cache_write_tokens_usage() {
        for tokens in [0, 5] {
            let body = json!({
                "id": "chat-response-cache-write",
                "object": "chat.completion",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "reply fixture" },
                    "finish_reason": "stop",
                    "logprobs": null
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "cache_write_tokens": tokens
                }
            });
            let response =
                super::decode_response(body).expect("OpenAI cache_write_tokens usage must decode");
            let usage = response.usage.expect("usage present");
            assert_eq!(usage.cache_write_tokens, Some(tokens));
        }

        let frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-cache-write",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "cache_write_tokens": 5
                }
            })
            .to_string(),
        };
        let mut state = StreamState::new();
        let events = super::decode_stream_frame(&frame, &mut state)
            .expect("OpenAI stream cache_write_tokens usage must decode");
        assert!(events.iter().any(|event| {
            matches!(event, StreamEventIr::Usage(usage) if usage.cache_write_tokens == Some(5))
        }));
    }

    #[test]
    fn decoder_tolerates_provider_specific_finish_reasons() {
        for reason in [
            "insufficient_system_resource",
            "sensitive",
            "network_error",
            "function_call",
        ] {
            let body = json!({
                "id": "chat-response-finish",
                "object": "chat.completion",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "reply fixture" },
                    "finish_reason": reason,
                    "logprobs": null
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 2,
                    "total_tokens": 3
                }
            });
            let response = super::decode_response(body)
                .expect("provider-specific finish reason must be tolerated");
            let (expected_finish, expected_status) = match reason {
                "sensitive" => ("content_filter", "incomplete"),
                "insufficient_system_resource" => ("length", "incomplete"),
                _ => ("stop", "completed"),
            };
            assert_eq!(
                response.completion.finish_reason.as_deref(),
                Some(expected_finish),
                "finish reason {reason} must map to {expected_finish}"
            );
            assert_eq!(response.completion.status.as_deref(), Some(expected_status));
        }
    }

    #[test]
    fn stream_decoder_tolerates_provider_specific_finish_reasons() {
        for reason in [
            "insufficient_system_resource",
            "sensitive",
            "network_error",
            "function_call",
        ] {
            let mut state = StreamState::new();
            let frame = SseFrame {
                event: None,
                data: json!({
                    "id": "chat-stream-finish",
                    "object": "chat.completion.chunk",
                    "created": 1,
                    "model": "chat-fixture-model",
                    "choices": [{
                        "index": 0,
                        "delta": { "role": "assistant", "content": "reply fixture" },
                        "finish_reason": reason,
                        "logprobs": null
                    }]
                })
                .to_string(),
            };
            let (expected_finish, _expected_stop) = match reason {
                "sensitive" => ("content_filter", "refusal"),
                "insufficient_system_resource" => ("length", "max_tokens"),
                _ => ("stop", "end_turn"),
            };
            super::decode_stream_frame(&frame, &mut state)
                .expect("provider-specific stream finish reason must be tolerated");

            let done = SseFrame {
                event: None,
                data: "[DONE]".to_string(),
            };
            let events =
                super::super::decode_stream_frame(WireProtocol::OpenAiChat, &done, &mut state)
                    .unwrap_or_else(|error| {
                        panic!("reason {reason}: DONE completes the stream: {error}")
                    });
            assert!(events.iter().any(|event| {
                matches!(
                    event,
                    StreamEventIr::Completed(completion)
                        if completion.finish_reason.as_deref() == Some(expected_finish)
                )
            }));
        }
    }

    #[test]
    fn decoder_treats_null_message_function_call_as_absent() {
        let body = json!({
            "id": "chat-response-qwen",
            "object": "chat.completion",
            "created": 1,
            "model": "chat-fixture-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "plain reply",
                    "function_call": null,
                    "tool_calls": null
                },
                "finish_reason": "stop",
                "logprobs": null
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "total_tokens": 3
            }
        });

        let response =
            super::decode_response(body).expect("null function_call must decode like absent");
        assert_eq!(
            response.content,
            vec![ContentIr::Text("plain reply".to_string())]
        );
    }

    #[test]
    fn stream_decoder_treats_null_delta_tool_calls_and_function_call_as_absent() {
        let mut state = StreamState::new();
        state
            .apply_event(&StreamEventIr::Started(ResponseMetaIr {
                id: Some("chat-stream-qwen".to_string()),
                model: Some("chat-fixture-model".to_string()),
            }))
            .expect("Chat stream starts");

        let frame = SseFrame {
            event: None,
            data: json!({
                "id": "chat-stream-qwen",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "chat-fixture-model",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "content": "plain reply",
                        "function_call": null,
                        "tool_calls": null
                    },
                    "finish_reason": null,
                    "logprobs": null
                }]
            })
            .to_string(),
        };
        let events = super::decode_stream_frame(&frame, &mut state)
            .expect("null delta tool_calls must decode like absent");
        assert!(events.iter().any(|event| {
            matches!(event, StreamEventIr::TextDelta { text } if text == "plain reply")
        }));
    }
}
