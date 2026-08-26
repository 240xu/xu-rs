use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::ir::{
    BridgeError, CompletionIr, ContentIr, MessageIr, RequestIr, ResponseIr, ResponseMetaIr, RoleIr,
    SseFrame, StreamEventIr, ToolCallIr, ToolChoiceIr, ToolDefinitionIr, UsageIr, WireProtocol,
};
use super::reasoning::{
    anthropic_block_from_openai_reasoning_item, decode_openai_reasoning_item, media_from_source,
    media_to_anthropic_source,
};
use super::request::{
    add_extension, object, optional_bool, optional_string, optional_u32, parse_arguments,
    parse_generation, provider_max_output_tokens, reject_unknown_fields, required_string,
    text_content, text_from_content, tool_call, tool_definition, tool_result,
    validate_target_representability, validate_tool_state, DEFAULT_ANTHROPIC_MAX_TOKENS,
    EXT_ANTHROPIC_THINKING,
};
use super::response::{
    map_upstream_error, normalize_total_tokens, optional_u64, required_array, required_object,
    required_string as response_required_string, required_u64, response_model, safe_error_message,
    target_response_id,
};
use super::stream::StreamState;

const EXT_SYSTEM: &str = "anthropic.v1.system";
const EXT_CONTEXT_MANAGEMENT: &str = "anthropic.v1.context_management";
const EXT_OUTPUT_CONFIG: &str = "anthropic.v1.output_config";
const EXT_FALLBACKS: &str = "anthropic.v1.fallbacks";

pub(super) fn parse_request(body: &Value) -> Result<RequestIr, BridgeError> {
    let object = object(body)?;
    reject_unknown_fields(
        object,
        &[
            "model",
            "stream",
            "system",
            "messages",
            "tools",
            "tool_choice",
            "max_tokens",
            "temperature",
            "top_p",
            "stop_sequences",
            "metadata",
            "thinking",
            "context_management",
            "output_config",
            "fallbacks",
            "cache_control",
        ],
    )?;
    // 顶层 cache_control（Claude Code 自动缓存断点）：复用块级校验，
    // 断点位置对上游转换无意义。
    if object.contains_key("cache_control") {
        validate_cache_control(object)?;
    }
    let model = required_string(object, "model")?;
    let stream = optional_bool(object, "stream", false)?;
    let mut extensions = BTreeMap::new();

    let mut messages = Vec::new();
    if let Some(system) = object.get("system") {
        let content = parse_system_content(system)?;
        if !content.is_empty() {
            messages.push(MessageIr {
                role: RoleIr::System,
                content,
                name: None,
                item_id: None,
            });
        }
        add_extension(&mut extensions, EXT_SYSTEM, Some(system));
    }

    let message_values = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(BridgeError::InvalidRequest)?;
    for message in message_values {
        messages.extend(parse_message(message)?);
    }

    let mut generation = parse_generation(object, &["max_tokens"], "stop_sequences")?;
    if let Some(context_management) = object.get("context_management") {
        let context_management = super::request::object(context_management)?;
        let edits = context_management
            .get("edits")
            .and_then(Value::as_array)
            .ok_or(BridgeError::InvalidRequest)?;
        for edit in edits {
            let edit = super::request::object(edit)?;
            // `turn`（压缩/回填所在轮次）由 Claude Code 压缩请求携带，
            // 对上游无意义但必须接受，否则压缩请求 400 导致无法自动压缩。
            reject_unknown_fields(edit, &["type", "keep", "turn"])?;
            required_string(edit, "type")?;
            if let Some(keep) = edit.get("keep") {
                if keep.as_str() != Some("all") {
                    return Err(BridgeError::Unsupported {
                        field: "context_management.edits.keep".to_string(),
                    });
                }
            }
        }
        add_extension(
            &mut extensions,
            EXT_CONTEXT_MANAGEMENT,
            object.get("context_management"),
        );
    }
    if let Some(output_config) = object.get("output_config") {
        let output_config = super::request::object(output_config)?;
        // format (structured outputs / --json-schema) has no IR equivalent;
        // accept and ignore it — the whole object is preserved in the extension.
        reject_unknown_fields(output_config, &["effort", "format"])?;
        generation.reasoning_effort = optional_string(output_config, "effort")?;
        add_extension(
            &mut extensions,
            EXT_OUTPUT_CONFIG,
            object.get("output_config"),
        );
    }
    if let Some(fallbacks) = object.get("fallbacks") {
        // --fallback-model: no IR equivalent; preserve for anthropic upstreams.
        add_extension(&mut extensions, EXT_FALLBACKS, Some(fallbacks));
    }
    if object.get("thinking").is_some() {
        let thinking = object
            .get("thinking")
            .and_then(Value::as_object)
            .ok_or(BridgeError::InvalidRequest)?;
        reject_unknown_fields(thinking, &["type", "budget_tokens", "display"])?;
        let thinking_type = thinking
            .get("type")
            .and_then(Value::as_str)
            .ok_or(BridgeError::InvalidRequest)?;
        if !matches!(thinking_type, "enabled" | "disabled" | "adaptive") {
            return Err(BridgeError::Unsupported {
                field: "thinking.type".to_string(),
            });
        }
        if let Some(display) = optional_string(thinking, "display")? {
            if !matches!(display.as_str(), "omitted" | "summarized") {
                return Err(BridgeError::Unsupported {
                    field: "thinking.display".to_string(),
                });
            }
        }
        if thinking_type == "enabled" {
            // OpenAI 合法枚举是 low/medium/high（"enabled" 非法会被上游 400）；
            // 与 chat 侧 DeepSeek 风格映射一致，用 high 表达「开启思考」。
            generation.reasoning_effort = Some("high".to_string());
        }
        optional_u32(thinking, "budget_tokens")?;
        if thinking_type != "adaptive" {
            add_extension(
                &mut extensions,
                EXT_ANTHROPIC_THINKING,
                object.get("thinking"),
            );
        }
    }

    let tools = parse_tools(object.get("tools"))?;
    let tool_choice = parse_tool_choice(object.get("tool_choice"))?;
    let metadata = object.get("metadata").cloned();

    let ir = RequestIr {
        protocol: WireProtocol::AnthropicMessages,
        model,
        messages,
        tools,
        tool_choice,
        generation,
        stream,
        metadata,
        extensions,
    };
    validate_tool_state(&ir.messages)?;
    Ok(ir)
}

fn parse_system_content(value: &Value) -> Result<Vec<ContentIr>, BridgeError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![ContentIr::Text(text.to_string())]);
    }
    let blocks = value.as_array().ok_or(BridgeError::InvalidRequest)?;
    blocks
        .iter()
        .map(|block| {
            let object = object(block)?;
            reject_unknown_fields(object, &["type", "text", "cache_control", "citations"])?;
            validate_cache_control(object)?;
            if object.get("type").and_then(Value::as_str) != Some("text") {
                return Err(BridgeError::Unsupported {
                    field: "system.content.type".to_string(),
                });
            }
            Ok(ContentIr::Text(required_string(object, "text")?))
        })
        .collect()
}

fn parse_message(value: &Value) -> Result<Vec<MessageIr>, BridgeError> {
    let object = object(value)?;
    reject_unknown_fields(object, &["role", "content"])?;
    let role = required_string(object, "role")?;
    let content = object.get("content").ok_or(BridgeError::InvalidRequest)?;
    match role.as_str() {
        "assistant" => Ok(vec![MessageIr {
            role: RoleIr::Assistant,
            content: parse_assistant_content(content)?,
            name: None,
            item_id: None,
        }]),
        "user" => parse_user_content(content),
        "system" => {
            let content = parse_system_content(content)?;
            if content.is_empty() {
                return Err(BridgeError::InvalidRequest);
            }
            Ok(vec![MessageIr {
                role: RoleIr::System,
                content,
                name: None,
                item_id: None,
            }])
        }
        other => Err(super::request::invalid_role(other)),
    }
}

fn parse_assistant_content(value: &Value) -> Result<Vec<ContentIr>, BridgeError> {
    if value.is_string() {
        return text_content(value, "messages.assistant.content");
    }
    let blocks = value.as_array().ok_or(BridgeError::InvalidRequest)?;
    let mut tool_index = 0;
    let mut content = Vec::new();
    for block in blocks {
        let parsed = parse_assistant_block(block, tool_index, false)?;
        if matches!(parsed, ContentIr::ToolUse(_)) {
            tool_index += 1;
        }
        if matches!(&parsed, ContentIr::Text(text) if text.trim().is_empty()) {
            // Claude Code emits an empty text block before tool calls; skip it.
            continue;
        }
        content.push(parsed);
    }
    Ok(content)
}

fn parse_assistant_block(
    value: &Value,
    index: usize,
    require_thinking_signature: bool,
) -> Result<ContentIr, BridgeError> {
    let object = object(value)?;
    match object.get("type").and_then(Value::as_str) {
        Some("text") => {
            reject_unknown_fields(object, &["type", "text", "cache_control", "citations"])?;
            validate_cache_control(object)?;
            // Claude Code emits empty text blocks before tool calls; the
            // caller filters them out, so missing/empty text is not an error.
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(ContentIr::Text(text))
        }
        Some("thinking") => {
            // thinking 块也是合法缓存断点位置（Anthropic 文档）；
            // 接受 cache_control 避免未来 Claude Code 版本被拒。
            reject_unknown_fields(object, &["type", "thinking", "signature", "cache_control"])?;
            if object.contains_key("cache_control") {
                validate_cache_control(object)?;
            }
            let signature = optional_string(object, "signature")?;
            if require_thinking_signature && signature.is_none() {
                return Err(BridgeError::InvalidUpstream);
            }
            Ok(ContentIr::Thinking {
                text: required_string(object, "thinking")?,
                signature,
            })
        }
        Some("redacted_thinking") => {
            reject_unknown_fields(object, &["type", "data"])?;
            Ok(ContentIr::RedactedThinking {
                data: required_string(object, "data")?,
            })
        }
        Some("tool_use") => {
            reject_unknown_fields(object, &["type", "id", "name", "input"])?;
            Ok(ContentIr::ToolUse(parse_tool_use(object, None, index)?))
        }
        Some(other) => Err(BridgeError::Unsupported {
            field: format!("messages.assistant.content.type:{other}"),
        }),
        None => Err(BridgeError::InvalidRequest),
    }
}

fn parse_user_content(value: &Value) -> Result<Vec<MessageIr>, BridgeError> {
    if value.is_string() {
        return Ok(vec![MessageIr {
            role: RoleIr::User,
            content: text_content(value, "messages.user.content")?,
            name: None,
            item_id: None,
        }]);
    }
    let blocks = value.as_array().ok_or(BridgeError::InvalidRequest)?;
    let mut messages = Vec::new();
    let mut normal = Vec::new();
    for block in blocks {
        let parsed = parse_user_block(block)?;
        if matches!(parsed, ContentIr::ToolResult(_)) {
            if !normal.is_empty() {
                messages.push(MessageIr {
                    role: RoleIr::User,
                    content: std::mem::take(&mut normal),
                    name: None,
                    item_id: None,
                });
            }
            messages.push(MessageIr {
                role: RoleIr::Tool,
                content: vec![parsed],
                name: None,
                item_id: None,
            });
        } else {
            normal.push(parsed);
        }
    }
    if !normal.is_empty() || messages.is_empty() {
        messages.push(MessageIr {
            role: RoleIr::User,
            content: normal,
            name: None,
            item_id: None,
        });
    }
    Ok(messages)
}

fn parse_user_block(value: &Value) -> Result<ContentIr, BridgeError> {
    let object = object(value)?;
    match object.get("type").and_then(Value::as_str) {
        Some("text") => {
            reject_unknown_fields(object, &["type", "text", "cache_control", "citations"])?;
            validate_cache_control(object)?;
            Ok(ContentIr::Text(required_string(object, "text")?))
        }
        Some("image") => {
            reject_unknown_fields(object, &["type", "source"])?;
            Ok(ContentIr::Image(media_from_source(
                object.get("source").ok_or(BridgeError::InvalidRequest)?,
            )?))
        }
        Some("document") => {
            reject_unknown_fields(object, &["type", "source"])?;
            Ok(ContentIr::Document(media_from_source(
                object.get("source").ok_or(BridgeError::InvalidRequest)?,
            )?))
        }
        Some("tool_result") => {
            reject_unknown_fields(
                object,
                &[
                    "type",
                    "tool_use_id",
                    "content",
                    "is_error",
                    "cache_control",
                ],
            )?;
            if object.contains_key("cache_control") {
                validate_cache_control(object)?;
            }
            let call_id = required_string(object, "tool_use_id")?;
            let content = object
                .get("content")
                .map(|value| parse_tool_result_content(value, "tool_result.content"))
                .transpose()?
                .unwrap_or_else(|| vec![ContentIr::Text(String::new())]);
            let is_error = optional_bool(object, "is_error", false)?;
            Ok(ContentIr::ToolResult(tool_result(
                call_id, content, is_error,
            )))
        }
        Some(other) => Err(BridgeError::Unsupported {
            field: format!("messages.user.content.type:{other}"),
        }),
        None => Err(BridgeError::InvalidRequest),
    }
}

fn parse_tool_result_content(value: &Value, field: &str) -> Result<Vec<ContentIr>, BridgeError> {
    if value.is_string() {
        return text_content(value, field);
    }
    let blocks = value.as_array().ok_or(BridgeError::InvalidRequest)?;
    blocks
        .iter()
        .map(|block| {
            let object = object(block)?;
            match object.get("type").and_then(Value::as_str) {
                Some("text") => {
                    reject_unknown_fields(object, &["type", "text", "cache_control", "citations"])?;
                    validate_cache_control(object)?;
                    // Claude Code emits empty text blocks before tool calls; the
                    // caller filters them out, so missing/empty text is not an error.
                    let text = object
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Ok(ContentIr::Text(text))
                }
                Some("image") => {
                    reject_unknown_fields(object, &["type", "source"])?;
                    Ok(ContentIr::Image(media_from_source(
                        object.get("source").ok_or(BridgeError::InvalidRequest)?,
                    )?))
                }
                Some("document") => {
                    reject_unknown_fields(object, &["type", "source"])?;
                    Ok(ContentIr::Document(media_from_source(
                        object.get("source").ok_or(BridgeError::InvalidRequest)?,
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

fn validate_cache_control(object: &Map<String, Value>) -> Result<(), BridgeError> {
    let Some(value) = object.get("cache_control") else {
        return Ok(());
    };
    let cache_control = super::request::object(value)?;
    // Official values are string enums ("5m" | "1h"); accept any string so
    // future values never break, but reject malformed non-string types.
    reject_unknown_fields(cache_control, &["type", "ttl"])?;
    if required_string(cache_control, "type")? != "ephemeral" {
        return Err(BridgeError::Unsupported {
            field: "cache_control.type".to_string(),
        });
    }
    if cache_control.get("ttl").is_some_and(|ttl| !ttl.is_string()) {
        return Err(BridgeError::InvalidRequest);
    }
    Ok(())
}

fn parse_tool_use(
    object: &Map<String, Value>,
    item_id: Option<String>,
    index: usize,
) -> Result<ToolCallIr, BridgeError> {
    reject_unknown_fields(object, &["type", "id", "name", "input"])?;
    Ok(tool_call(
        required_string(object, "id")?,
        item_id,
        index,
        required_string(object, "name")?,
        parse_arguments(object.get("input").ok_or(BridgeError::InvalidRequest)?)?,
    ))
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
            reject_unknown_fields(
                object,
                &[
                    "type",
                    "name",
                    "description",
                    "input_schema",
                    "cache_control",
                ],
            )?;
            validate_cache_control(object)?;
            if let Some(tool_type) = object.get("type").and_then(Value::as_str) {
                if tool_type != "custom" {
                    return Err(BridgeError::Unsupported {
                        field: "tools.type".to_string(),
                    });
                }
            }
            let input_schema = object
                .get("input_schema")
                .cloned()
                .ok_or(BridgeError::InvalidRequest)?;
            if !input_schema.is_object() {
                return Err(BridgeError::InvalidRequest);
            }
            Ok(tool_definition(
                required_string(object, "name")?,
                optional_string(object, "description")?,
                input_schema,
            ))
        })
        .collect()
}

fn parse_tool_choice(value: Option<&Value>) -> Result<Option<ToolChoiceIr>, BridgeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = object(value)?;
    reject_unknown_fields(object, &["type", "name"])?;
    match required_string(object, "type")?.as_str() {
        "auto" => Ok(Some(ToolChoiceIr::Auto)),
        "any" => Ok(Some(ToolChoiceIr::Any)),
        "none" => Ok(Some(ToolChoiceIr::None)),
        "tool" => Ok(Some(ToolChoiceIr::Tool {
            name: required_string(object, "name")?,
        })),
        _ => Err(BridgeError::Unsupported {
            field: "tool_choice.type".to_string(),
        }),
    }
}

pub(super) fn encode_request(ir: &RequestIr, model: &str) -> Result<Value, BridgeError> {
    validate_target_representability(ir, WireProtocol::AnthropicMessages)?;
    let provider_max_tokens = provider_max_output_tokens(ir)?;
    let max_tokens = ir
        .generation
        .max_tokens
        .or_else(|| provider_max_tokens.map(|value| value.max(DEFAULT_ANTHROPIC_MAX_TOKENS)))
        .unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS);
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert("max_tokens".to_string(), json!(max_tokens));
    body.insert("stream".to_string(), Value::Bool(ir.stream));

    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &ir.messages {
        match message.role {
            RoleIr::System | RoleIr::Developer => {
                system.extend(anthropic_system_text(&message.content)?);
            }
            RoleIr::User | RoleIr::Tool => {
                messages.push(json!({
                    "role": "user",
                    "content": encode_user_content(&message.content)?,
                }));
            }
            RoleIr::Assistant => {
                messages.push(json!({
                    "role": "assistant",
                    "content": encode_assistant_content(&message.content)?,
                }));
            }
        }
    }
    if !system.is_empty() {
        let original = ir.extensions.get(EXT_SYSTEM);
        body.insert(
            "system".to_string(),
            if ir.protocol == WireProtocol::AnthropicMessages {
                original.cloned().unwrap_or_else(|| system_value(&system))
            } else {
                system_value(&system)
            },
        );
    }
    body.insert("messages".to_string(), Value::Array(messages));

    if !ir.tools.is_empty() {
        if ir.tools.iter().any(|tool| tool.strict.is_some()) {
            return Err(BridgeError::Unsupported {
                field: "anthropic.tool.strict".to_string(),
            });
        }
        body.insert(
            "tools".to_string(),
            Value::Array(ir.tools.iter().map(encode_tool).collect::<Result<_, _>>()?),
        );
    }
    if let Some(tool_choice) = &ir.tool_choice {
        body.insert("tool_choice".to_string(), encode_tool_choice(tool_choice)?);
    }
    if ir.protocol == WireProtocol::AnthropicMessages {
        if let Some(value) = ir.extensions.get(EXT_FALLBACKS) {
            body.insert("fallbacks".to_string(), value.clone());
        }
    }
    if let Some(value) = ir.generation.temperature {
        body.insert("temperature".to_string(), json!(value));
    }
    if let Some(value) = ir.generation.top_p {
        body.insert("top_p".to_string(), json!(value));
    }
    if !ir.generation.stop_sequences.is_empty() {
        body.insert(
            "stop_sequences".to_string(),
            json!(ir.generation.stop_sequences),
        );
    }
    if let Some(value) = ir.metadata.clone() {
        body.insert("metadata".to_string(), value);
    }
    if let Some(value) = ir.extensions.get(EXT_ANTHROPIC_THINKING) {
        body.insert("thinking".to_string(), value.clone());
    } else if ir.generation.reasoning_effort.is_some() {
        // Anthropic 约束: budget_tokens >= 1024 且严格 < max_tokens。
        // 始终注入（rectifier 兜底：上游因预算不足拒绝时剥 thinking 重试），
        // 但确保 budget < max_tokens 以避免最常见违规。
        body.insert(
            "thinking".to_string(),
            json!({
                "type": "enabled",
                "budget_tokens": std::cmp::max(max_tokens.saturating_sub(1), 1)
            }),
        );
    }
    Ok(Value::Object(body))
}

pub(super) fn decode_response(body: Value) -> Result<ResponseIr, BridgeError> {
    let object = body.as_object().ok_or(BridgeError::InvalidUpstream)?;
    if object.get("type").and_then(Value::as_str) == Some("error") {
        return Err(BridgeError::InvalidUpstream);
    }
    reject_unknown_fields(
        object,
        &[
            "id",
            "type",
            "role",
            "model",
            "content",
            "stop_reason",
            "stop_sequence",
            "usage",
        ],
    )
    .map_err(map_upstream_error)?;
    if object.get("type").and_then(Value::as_str) != Some("message")
        || object.get("role").and_then(Value::as_str) != Some("assistant")
    {
        return Err(BridgeError::InvalidUpstream);
    }
    let id = response_required_string(object, "id")?;
    let model = response_required_string(object, "model")?;
    let blocks = required_array(object, "content")?;
    if blocks.is_empty() {
        return Err(BridgeError::InvalidUpstream);
    }
    let mut tool_index = 0;
    let mut content = Vec::with_capacity(blocks.len());
    for block in blocks {
        let parsed = parse_assistant_block(block, tool_index, true).map_err(map_upstream_error)?;
        if matches!(parsed, ContentIr::ToolUse(_)) {
            tool_index += 1;
        }
        content.push(parsed);
    }
    let stop_reason = response_required_string(object, "stop_reason")?;
    if !matches!(
        stop_reason.as_str(),
        "end_turn" | "max_tokens" | "stop_sequence" | "tool_use" | "refusal"
    ) {
        return Err(BridgeError::InvalidUpstream);
    }
    // stop_sequence（命中的 stop 序列文本）按规范返回；对 chat/其他协议
    // 无表达，读取并忽略即可（不该 502）。
    if object.contains_key("stop_sequence") {
        let _ = &object["stop_sequence"];
    }
    let usage_object = required_object(object, "usage")?;
    reject_unknown_fields(
        usage_object,
        &[
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "output_tokens_details",
        ],
    )
    .map_err(map_upstream_error)?;
    let input_tokens = required_u64(usage_object, "input_tokens")?;
    let output_tokens = required_u64(usage_object, "output_tokens")?;
    let cache_read_tokens = optional_u64(usage_object, "cache_read_input_tokens")?;
    let cache_write_tokens = optional_u64(usage_object, "cache_creation_input_tokens")?;
    let reasoning_tokens = if usage_object.contains_key("output_tokens_details") {
        let details = required_object(usage_object, "output_tokens_details")?;
        reject_unknown_fields(details, &["thinking_tokens"]).map_err(map_upstream_error)?;
        optional_u64(details, "thinking_tokens")?
    } else {
        None
    };
    // Anthropic input_tokens is the billed figure (cached prefixes excluded);
    // normalize to the full input so the encoder bills exactly once and the
    // anthropic -> anthropic direct path round-trips without double subtraction
    // (mirrors decode_stream_usage).
    let full_input_tokens = input_tokens
        .saturating_add(cache_read_tokens.unwrap_or(0))
        .saturating_add(cache_write_tokens.unwrap_or(0));
    let total_tokens = normalize_total_tokens(Some(full_input_tokens), Some(output_tokens), None)?;
    let has_tool_call = content
        .iter()
        .any(|item| matches!(item, ContentIr::ToolUse(_)));
    if (stop_reason == "tool_use") != has_tool_call {
        return Err(BridgeError::InvalidUpstream);
    }
    Ok(ResponseIr {
        meta: ResponseMetaIr {
            id: Some(id),
            model: Some(model),
        },
        content,
        usage: Some(UsageIr {
            input_tokens: Some(full_input_tokens),
            output_tokens: Some(output_tokens),
            total_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        }),
        completion: CompletionIr {
            finish_reason: Some(anthropic_finish_reason(&stop_reason).to_string()),
            stop_reason: Some(stop_reason.clone()),
            status: Some(
                if matches!(stop_reason.as_str(), "max_tokens" | "refusal") {
                    "incomplete".to_string()
                } else {
                    "completed".to_string()
                },
            ),
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
    let stop_reason = anthropic_stop_reason(ir)?;
    let content = ir
        .content
        .iter()
        .map(encode_response_block)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "id": target_response_id(ir, WireProtocol::AnthropicMessages),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": encode_usage(ir.usage.as_ref())?
    }))
}

pub(super) fn encode_error_response(ir: &ResponseIr) -> Result<Value, BridgeError> {
    let _ = ir;
    Ok(json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": safe_error_message()
        }
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
    if !matches!(kind, "message_stop" | "content_block_stop" | "ping") {
        state.require_no_pending_completion()?;
    }
    let mut events = Vec::new();
    match kind {
        "message_start" => {
            reject_unknown_fields(object, &["type", "message"]).map_err(map_upstream_error)?;
            let message = object
                .get("message")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(
                message,
                &[
                    "id",
                    "type",
                    "role",
                    "model",
                    "content",
                    "stop_reason",
                    "stop_sequence",
                    "usage",
                ],
            )
            .map_err(map_upstream_error)?;
            let event = StreamEventIr::Started(ResponseMetaIr {
                id: Some(stream_required_string(message, "id")?),
                model: Some(stream_required_string(message, "model")?),
            });
            state.apply_event(&event)?;
            events.push(event);
            if let Some(usage) = message.get("usage") {
                let usage = decode_stream_usage(usage)?;
                let event = StreamEventIr::Usage(usage);
                state.apply_event(&event)?;
                events.push(event);
            }
        }
        "content_block_start" => {
            reject_unknown_fields(object, &["type", "index", "content_block"])
                .map_err(map_upstream_error)?;
            state
                .is_started()
                .then_some(())
                .ok_or(BridgeError::ToolState)?;
            let index = stream_required_u64(object, "index")? as usize;
            let block = object
                .get("content_block")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    reject_unknown_fields(block, &["type", "text"]).map_err(map_upstream_error)?;
                    state.register_wire_block(WireProtocol::AnthropicMessages, index, 0)?
                }
                Some("thinking") => {
                    reject_unknown_fields(block, &["type", "thinking", "signature"])
                        .map_err(map_upstream_error)?;
                    state.register_wire_block(WireProtocol::AnthropicMessages, index, 1)?
                }
                Some("tool_use") => {
                    reject_unknown_fields(block, &["type", "id", "name", "input"])
                        .map_err(map_upstream_error)?;
                    state.register_wire_block(WireProtocol::AnthropicMessages, index, 2)?;
                    let call_id = stream_required_string(block, "id")?;
                    let logical_index = state.next_tool_index();
                    state.register_wire_tool(WireProtocol::AnthropicMessages, index, &call_id)?;
                    let event = StreamEventIr::ToolCallStarted(ToolCallIr {
                        call_id,
                        item_id: None,
                        index: logical_index,
                        name: stream_required_string(block, "name")?,
                        arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                    });
                    state.apply_event(&event)?;
                    events.push(event);
                }
                Some(other) => {
                    return Err(BridgeError::Unsupported {
                        field: format!("anthropic.stream.content_block.type:{other}"),
                    })
                }
                None => return Err(BridgeError::InvalidUpstream),
            }
        }
        "content_block_delta" => {
            reject_unknown_fields(object, &["type", "index", "delta"])
                .map_err(map_upstream_error)?;
            state
                .is_started()
                .then_some(())
                .ok_or(BridgeError::ToolState)?;
            let index = stream_required_u64(object, "index")? as usize;
            state.require_wire_block_open(WireProtocol::AnthropicMessages, index)?;
            let delta = object
                .get("delta")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            let block_kind = state.wire_block_kind(WireProtocol::AnthropicMessages, index)?;
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") if block_kind == 0 => {
                    reject_unknown_fields(delta, &["type", "text"]).map_err(map_upstream_error)?;
                    let text = stream_required_string(delta, "text")?;
                    let event = StreamEventIr::TextDelta { text };
                    state.apply_event(&event)?;
                    events.push(event);
                }
                Some("thinking_delta") if block_kind == 1 => {
                    reject_unknown_fields(delta, &["type", "thinking"])
                        .map_err(map_upstream_error)?;
                    let text = stream_required_string(delta, "thinking")?;
                    let event = StreamEventIr::ReasoningDelta { text };
                    state.apply_event(&event)?;
                    events.push(event);
                }
                Some("input_json_delta") if block_kind == 2 => {
                    reject_unknown_fields(delta, &["type", "partial_json"])
                        .map_err(map_upstream_error)?;
                    let call_id =
                        state.wire_tool_call_id(WireProtocol::AnthropicMessages, index)?;
                    let event = StreamEventIr::ToolCallArgumentsDelta {
                        call_id,
                        delta: stream_required_string(delta, "partial_json")?,
                    };
                    state.apply_event(&event)?;
                    events.push(event);
                }
                // Extended-thinking 收尾签名增量：转换路径中 reasoning 会被
                // rectifier 剥离，签名本身不可跨协议表示——接受并丢弃，
                // 避免整条 thinking 流在收尾帧上 502。
                Some("signature_delta") if block_kind == 1 => {
                    reject_unknown_fields(delta, &["type", "signature"])
                        .map_err(map_upstream_error)?;
                    stream_content_string(delta, "signature")?;
                }
                Some(other) => {
                    return Err(BridgeError::Unsupported {
                        field: format!("anthropic.stream.delta.type:{other}"),
                    })
                }
                None => return Err(BridgeError::InvalidUpstream),
            }
        }
        "content_block_stop" => {
            reject_unknown_fields(object, &["type", "index"]).map_err(map_upstream_error)?;
            state
                .is_started()
                .then_some(())
                .ok_or(BridgeError::ToolState)?;
            let index = stream_required_u64(object, "index")? as usize;
            let block_kind = state.wire_block_kind(WireProtocol::AnthropicMessages, index)?;
            if block_kind == 2 {
                let call_id = state.wire_tool_call_id(WireProtocol::AnthropicMessages, index)?;
                let event = StreamEventIr::ToolCallFinished { call_id };
                state.apply_event(&event)?;
                events.push(event);
            }
            state.close_wire_block(WireProtocol::AnthropicMessages, index)?;
        }
        "message_delta" => {
            reject_unknown_fields(object, &["type", "delta", "usage"])
                .map_err(map_upstream_error)?;
            state
                .is_started()
                .then_some(())
                .ok_or(BridgeError::ToolState)?;
            if let Some(usage) = object.get("usage") {
                let event = StreamEventIr::Usage(decode_stream_usage(usage)?);
                state.apply_event(&event)?;
                events.push(event);
            }
            let delta = object
                .get("delta")
                .and_then(Value::as_object)
                .ok_or(BridgeError::InvalidUpstream)?;
            reject_unknown_fields(delta, &["stop_reason"]).map_err(map_upstream_error)?;
            if let Some(stop_reason) = delta.get("stop_reason") {
                if !stop_reason.is_null() {
                    let stop_reason = stop_reason.as_str().ok_or(BridgeError::InvalidUpstream)?;
                    state.set_pending_completion(anthropic_completion(stop_reason)?)?;
                }
            }
        }
        "message_stop" => {
            reject_unknown_fields(object, &["type"]).map_err(map_upstream_error)?;
            state.require_wire_blocks_closed(WireProtocol::AnthropicMessages)?;
            let completion = state.take_pending_completion()?;
            let event = StreamEventIr::Completed(completion);
            state.apply_event(&event)?;
            events.push(event);
        }
        "ping" => {
            reject_unknown_fields(object, &["type"]).map_err(map_upstream_error)?;
        }
        "error" => {
            reject_unknown_fields(object, &["type", "error"]).map_err(map_upstream_error)?;
            if let Some(error) = object.get("error") {
                let error = error.as_object().ok_or(BridgeError::InvalidUpstream)?;
                reject_unknown_fields(error, &["type", "message"]).map_err(map_upstream_error)?;
            }
            let event = StreamEventIr::Failed(BridgeError::InvalidUpstream);
            state.apply_event(&event)?;
            events.push(event);
        }
        other => {
            return Err(BridgeError::Unsupported {
                field: format!("anthropic.stream.event:{other}"),
            })
        }
    }
    Ok(events)
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
            frames.push(stream_frame(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": state.encoded_message_id(),
                        "type": "message",
                        "role": "assistant",
                        "model": state.meta().model.clone().ok_or(BridgeError::InvalidUpstream)?,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 }
                    }
                }),
            ));
        }
        StreamEventIr::TextDelta { text } => {
            if !state.encoded_text() {
                let index = state.encoded_block_index("anthropic:text");
                frames.push(stream_frame(
                    "content_block_start",
                    json!({ "type": "content_block_start", "index": index, "content_block": { "type": "text", "text": "" } }),
                ));
                state.mark_encoded_text();
            }
            frames.push(stream_frame(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": state.encoded_block_index("anthropic:text"), "delta": { "type": "text_delta", "text": text } }),
            ));
        }
        StreamEventIr::ReasoningDelta { text } => {
            if !state.encoded_reasoning() {
                let index = state.encoded_block_index("anthropic:reasoning");
                frames.push(stream_frame(
                    "content_block_start",
                    json!({ "type": "content_block_start", "index": index, "content_block": { "type": "thinking", "thinking": "" } }),
                ));
                state.mark_encoded_reasoning();
            }
            frames.push(stream_frame(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": state.encoded_block_index("anthropic:reasoning"), "delta": { "type": "thinking_delta", "thinking": text } }),
            ));
        }
        StreamEventIr::ToolCallStarted(call) => {
            if state.encoded_tool(&call.call_id) {
                return Err(BridgeError::ToolState);
            }
            let index = state.encoded_block_index(&format!("anthropic:tool:{}", call.call_id));
            let input = if call
                .arguments
                .as_object()
                .is_some_and(|object| !object.is_empty())
            {
                call.arguments.clone()
            } else {
                json!({})
            };
            frames.push(stream_frame(
                "content_block_start",
                json!({ "type": "content_block_start", "index": index, "content_block": { "type": "tool_use", "id": call.call_id, "name": call.name, "input": input } }),
            ));
            state.mark_encoded_tool(&call.call_id);
        }
        StreamEventIr::ToolCallArgumentsDelta { call_id, delta } => {
            state.tool_state(call_id)?;
            frames.push(stream_frame(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": state.encoded_block_index(&format!("anthropic:tool:{call_id}")), "delta": { "type": "input_json_delta", "partial_json": delta } }),
            ));
        }
        StreamEventIr::ToolCallFinished { call_id } => {
            if state.encoded_tool_stop(call_id) {
                return Err(BridgeError::ToolState);
            }
            let index = state.encoded_block_index(&format!("anthropic:tool:{call_id}"));
            frames.push(stream_frame(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ));
            state.mark_encoded_tool_stop(call_id);
        }
        StreamEventIr::Usage(usage) => {
            frames.push(stream_frame(
                "message_delta",
                json!({ "type": "message_delta", "delta": {}, "usage": stream_usage_value(Some(usage))? }),
            ));
        }
        StreamEventIr::Completed(completion) => {
            if state.encoded_terminal() {
                return Err(BridgeError::ToolState);
            }
            if state.encoded_text() {
                frames.push(stream_frame(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": state.encoded_block_index("anthropic:text") }),
                ));
            }
            if state.encoded_reasoning() {
                frames.push(stream_frame(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": state.encoded_block_index("anthropic:reasoning") }),
                ));
            }
            frames.push(stream_frame(
                "message_delta",
                json!({ "type": "message_delta", "delta": { "stop_reason": anthropic_stream_stop_reason(completion)? }, "usage": stream_usage_value(state.usage())? }),
            ));
            frames.push(stream_frame(
                "message_stop",
                json!({ "type": "message_stop" }),
            ));
            state.mark_encoded_terminal();
        }
        StreamEventIr::Failed(_) => {
            frames.push(stream_frame(
                "error",
                json!({ "type": "error", "error": { "type": "api_error", "message": safe_error_message() } }),
            ));
            frames.push(stream_frame(
                "message_stop",
                json!({ "type": "message_stop" }),
            ));
            state.mark_encoded_terminal();
        }
    }
    Ok(frames)
}

fn stream_frame(event: &str, data: Value) -> SseFrame {
    SseFrame {
        event: Some(event.to_string()),
        data: serde_json::to_string(&data).expect("stream frame JSON is serializable"),
    }
}

/// 内容型流字段：允许空白/空串（换行增量合法），见 responses 同名注释。
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

fn decode_stream_usage(value: &Value) -> Result<UsageIr, BridgeError> {
    let object = value.as_object().ok_or(BridgeError::InvalidUpstream)?;
    reject_unknown_fields(
        object,
        &[
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "output_tokens_details",
        ],
    )
    .map_err(map_upstream_error)?;
    let input_tokens = stream_optional_u64(object, "input_tokens")?;
    let cache_read_tokens = stream_optional_u64(object, "cache_read_input_tokens")?;
    let cache_write_tokens = stream_optional_u64(object, "cache_creation_input_tokens")?;
    // Anthropic input_tokens is the billed figure (cache tokens excluded);
    // normalize to the full input so the encoder bills exactly once and the
    // anthropic -> anthropic direct path round-trips without double subtraction.
    let full_input_tokens = input_tokens.map(|tokens| {
        tokens
            .saturating_add(cache_read_tokens.unwrap_or(0))
            .saturating_add(cache_write_tokens.unwrap_or(0))
    });
    Ok(UsageIr {
        input_tokens: full_input_tokens,
        output_tokens: stream_optional_u64(object, "output_tokens")?,
        total_tokens: None,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens: stream_detail_u64(object, "output_tokens_details", "thinking_tokens")?,
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

fn anthropic_completion(stop_reason: &str) -> Result<CompletionIr, BridgeError> {
    let (finish_reason, status) = match stop_reason {
        "end_turn" | "stop_sequence" => ("stop", "completed"),
        "max_tokens" => ("length", "incomplete"),
        "tool_use" => ("tool_calls", "completed"),
        "refusal" => ("content_filter", "incomplete"),
        _ => return Err(BridgeError::InvalidUpstream),
    };
    Ok(CompletionIr {
        finish_reason: Some(finish_reason.to_string()),
        stop_reason: Some(stop_reason.to_string()),
        status: Some(status.to_string()),
        error: None,
    })
}

fn anthropic_stream_stop_reason(completion: &CompletionIr) -> Result<&'static str, BridgeError> {
    anthropic_stop_reason(&ResponseIr {
        meta: ResponseMetaIr::default(),
        content: vec![ContentIr::Text(String::new())],
        usage: None,
        completion: completion.clone(),
        extensions: BTreeMap::new(),
    })
}

fn encode_response_block(content: &ContentIr) -> Result<Value, BridgeError> {
    match content {
        ContentIr::Text(text) => Ok(json!({ "type": "text", "text": text })),
        ContentIr::Thinking { text, signature } => {
            if let Some(item) = signature.as_deref().and_then(decode_openai_reasoning_item) {
                return anthropic_block_from_openai_reasoning_item(&item)
                    .ok_or(BridgeError::InvalidUpstream);
            }
            let mut block = json!({ "type": "thinking", "thinking": text });
            if let Some(signature) = signature {
                block["signature"] = json!(signature);
            }
            Ok(block)
        }
        ContentIr::RedactedThinking { data } => {
            if let Some(item) = decode_openai_reasoning_item(data) {
                return anthropic_block_from_openai_reasoning_item(&item)
                    .ok_or(BridgeError::InvalidUpstream);
            }
            Ok(json!({ "type": "redacted_thinking", "data": data }))
        }
        ContentIr::ToolUse(call) => {
            if call.item_id.is_some() {
                return Err(BridgeError::Unsupported {
                    field: "responses.item_id".to_string(),
                });
            }
            Ok(json!({
                "type": "tool_use",
                "id": call.call_id,
                "name": call.name,
                "input": call.arguments
            }))
        }
        ContentIr::Image(_) | ContentIr::Document(_) => Err(BridgeError::Unsupported {
            field: "response.content.media".to_string(),
        }),
        ContentIr::ToolResult(_) => Err(BridgeError::Unsupported {
            field: "response.tool_result".to_string(),
        }),
    }
}

fn encode_usage(usage: Option<&UsageIr>) -> Result<Value, BridgeError> {
    // OpenAI 规范中 usage 可选（vLLM/部分中转不返回）；与流式路径一致，
    // 缺省时输出占位，避免合法上游被误杀成 502。
    let usage = match usage {
        Some(usage) => usage,
        None => {
            return Ok(json!({
                "input_tokens": 0,
                "output_tokens": 0,
            }));
        }
    };
    let input_tokens = usage.input_tokens.ok_or(BridgeError::InvalidUpstream)?;
    let output_tokens = usage.output_tokens.ok_or(BridgeError::InvalidUpstream)?;
    let _ = normalize_total_tokens(Some(input_tokens), Some(output_tokens), usage.total_tokens)?;
    let cache_read_tokens = usage.cache_read_tokens.filter(|tokens| *tokens > 0);
    let cache_write_tokens = usage.cache_write_tokens.filter(|tokens| *tokens > 0);
    // IR input is the full count (includes cached prefixes); bill exactly once
    // by subtracting both cache components back out (mirrors stream_usage_value).
    let billed_input_tokens = input_tokens
        .checked_sub(cache_read_tokens.unwrap_or(0))
        .and_then(|rest| rest.checked_sub(cache_write_tokens.unwrap_or(0)))
        .ok_or(BridgeError::InvalidUpstream)?;
    let mut result = json!({
        "input_tokens": billed_input_tokens,
        "output_tokens": output_tokens
    });
    if let Some(value) = cache_read_tokens {
        result["cache_read_input_tokens"] = value.into();
    }
    if let Some(value) = usage.cache_write_tokens {
        result["cache_creation_input_tokens"] = value.into();
    }
    if let Some(value) = usage.reasoning_tokens {
        result["output_tokens_details"] = json!({ "thinking_tokens": value });
    }
    Ok(result)
}

fn stream_usage_value(usage: Option<&UsageIr>) -> Result<Value, BridgeError> {
    let cache_read_tokens = usage
        .and_then(|usage| usage.cache_read_tokens)
        .filter(|tokens| *tokens > 0);
    let cache_write_tokens = usage.and_then(|usage| usage.cache_write_tokens);
    let input_tokens = usage.and_then(|usage| usage.input_tokens).unwrap_or(0);
    // IR input is the full count (includes cached prefixes); bill exactly once.
    // 上游自相矛盾账单（缓存分量 > 全量输入）fail-closed。
    let billed_input_tokens = input_tokens
        .checked_sub(cache_read_tokens.unwrap_or(0))
        .and_then(|rest| rest.checked_sub(cache_write_tokens.unwrap_or(0)))
        .ok_or(BridgeError::InvalidUpstream)?;
    let mut value = json!({
        "input_tokens": billed_input_tokens,
        "output_tokens": usage.and_then(|usage| usage.output_tokens).unwrap_or(0)
    });
    if let Some(tokens) = cache_read_tokens {
        value["cache_read_input_tokens"] = json!(tokens);
    }
    if let Some(tokens) = cache_write_tokens {
        value["cache_creation_input_tokens"] = json!(tokens);
    }
    if let Some(tokens) = usage.and_then(|usage| usage.reasoning_tokens) {
        value["output_tokens_details"] = json!({ "thinking_tokens": tokens });
    }
    Ok(value)
}

fn anthropic_finish_reason(stop_reason: &str) -> &'static str {
    match stop_reason {
        "end_turn" | "stop_sequence" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

fn anthropic_stop_reason(ir: &ResponseIr) -> Result<&'static str, BridgeError> {
    if let Some(reason) = ir.completion.stop_reason.as_deref() {
        return match reason {
            "end_turn" | "stop_sequence" | "max_tokens" | "tool_use" | "refusal" => {
                Ok(match reason {
                    "end_turn" => "end_turn",
                    "stop_sequence" => "stop_sequence",
                    "max_tokens" => "max_tokens",
                    "tool_use" => "tool_use",
                    "refusal" => "refusal",
                    _ => unreachable!(),
                })
            }
            _ => Err(BridgeError::InvalidUpstream),
        };
    }
    match ir.completion.finish_reason.as_deref() {
        Some("stop") => Ok("end_turn"),
        Some("length") => Ok("max_tokens"),
        Some("tool_calls") => Ok("tool_use"),
        Some("content_filter") => Ok("refusal"),
        Some("stop_sequence") => Ok("stop_sequence"),
        _ if ir.completion.status.as_deref() == Some("incomplete") => Ok("max_tokens"),
        _ => Err(BridgeError::InvalidUpstream),
    }
}

fn anthropic_system_text(content: &[ContentIr]) -> Result<Vec<String>, BridgeError> {
    content
        .iter()
        .map(|item| match item {
            ContentIr::Text(text) => Ok(text.clone()),
            _ => Err(BridgeError::Unsupported {
                field: "system.content".to_string(),
            }),
        })
        .collect()
}

fn system_value(system: &[String]) -> Value {
    Value::String(system.join("\n\n"))
}

fn encode_user_content(content: &[ContentIr]) -> Result<Value, BridgeError> {
    if content
        .iter()
        .all(|item| matches!(item, ContentIr::Text(_)))
    {
        return Ok(Value::String(text_from_content(content)?));
    }
    content
        .iter()
        .map(encode_user_block)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn encode_user_block(content: &ContentIr) -> Result<Value, BridgeError> {
    match content {
        ContentIr::Text(text) => Ok(json!({ "type": "text", "text": text })),
        ContentIr::Image(media) => Ok(json!({
            "type": "image",
            "source": media_to_anthropic_source(media)?,
        })),
        ContentIr::Document(media) => Ok(json!({
            "type": "document",
            "source": media_to_anthropic_source(media)?,
        })),
        ContentIr::ToolResult(result) => Ok(json!({
            "type": "tool_result",
            "tool_use_id": result.call_id,
            "content": encode_tool_result_content(&result.content)?,
            "is_error": result.is_error,
        })),
        _ => Err(BridgeError::Unsupported {
            field: "messages.user.content".to_string(),
        }),
    }
}

fn encode_tool_result_content(content: &[ContentIr]) -> Result<Value, BridgeError> {
    if content
        .iter()
        .all(|item| matches!(item, ContentIr::Text(_)))
    {
        return Ok(Value::String(text_from_content(content)?));
    }
    content
        .iter()
        .map(encode_user_block)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn encode_assistant_content(content: &[ContentIr]) -> Result<Value, BridgeError> {
    if content
        .iter()
        .all(|item| matches!(item, ContentIr::Text(_)))
    {
        return Ok(Value::String(text_from_content(content)?));
    }
    content
        .iter()
        .map(|content| match content {
            ContentIr::Text(text) => Ok(json!({ "type": "text", "text": text })),
            ContentIr::Thinking { text, signature } => {
                if let Some(item) = signature.as_deref().and_then(decode_openai_reasoning_item) {
                    return anthropic_block_from_openai_reasoning_item(&item)
                        .ok_or(BridgeError::InvalidRequest);
                }
                let mut block = json!({ "type": "thinking", "thinking": text });
                if let Some(signature) = signature {
                    block["signature"] = json!(signature);
                }
                Ok(block)
            }
            ContentIr::RedactedThinking { data } => {
                if let Some(item) = decode_openai_reasoning_item(data) {
                    return anthropic_block_from_openai_reasoning_item(&item)
                        .ok_or(BridgeError::InvalidRequest);
                }
                Ok(json!({ "type": "redacted_thinking", "data": data }))
            }
            ContentIr::ToolUse(call) => Ok(json!({
                "type": "tool_use",
                "id": call.call_id,
                "name": call.name,
                "input": call.arguments,
            })),
            _ => Err(BridgeError::Unsupported {
                field: "messages.assistant.content".to_string(),
            }),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn encode_tool(tool: &ToolDefinitionIr) -> Result<Value, BridgeError> {
    if tool.name.trim().is_empty() || !tool.input_schema.is_object() {
        return Err(BridgeError::InvalidRequest);
    }
    let mut value = json!({
        "name": tool.name,
        "input_schema": tool.input_schema,
    });
    if let Some(description) = &tool.description {
        value["description"] = json!(description);
    }
    Ok(value)
}

fn encode_tool_choice(choice: &ToolChoiceIr) -> Result<Value, BridgeError> {
    Ok(match choice {
        ToolChoiceIr::Auto => json!({ "type": "auto" }),
        ToolChoiceIr::Any => json!({ "type": "any" }),
        ToolChoiceIr::None => json!({ "type": "none" }),
        ToolChoiceIr::Tool { name } if !name.trim().is_empty() => {
            json!({ "type": "tool", "name": name })
        }
        ToolChoiceIr::Tool { .. } => return Err(BridgeError::InvalidRequest),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::super::{ContentIr, RoleIr, WireProtocol};

    #[test]
    fn accepts_tool_cache_control_and_rejects_unknown_types() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "messages": [{ "role": "user", "content": "fixture" }],
            "tools": [
                {
                    "name": "read_file",
                    "description": "read",
                    "input_schema": { "type": "object" },
                    "cache_control": { "type": "ephemeral" }
                }
            ]
        });
        let ir = super::parse_request(&body).expect("tool cache_control accepted");
        assert_eq!(ir.tools.len(), 1);

        let mut bad = body.clone();
        bad["tools"][0]["cache_control"]["type"] = json!("persistent");
        assert!(matches!(
            super::parse_request(&bad),
            Err(super::super::BridgeError::Unsupported { .. })
        ));
    }

    #[test]
    fn parser_keeps_ordered_blocks_and_encoder_applies_default_max_tokens() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "system": [{ "type": "text", "text": "system fixture" }],
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "before" },
                        { "type": "tool_use", "id": "call-fixture", "name": "lookup", "input": { "q": "fixture" } }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "call-fixture", "content": "result fixture" },
                        { "type": "text", "text": "after" }
                    ]
                }
            ],
            "tools": [{ "name": "lookup", "input_schema": { "type": "object" } }]
        });

        let ir = super::parse_request(&body).expect("Anthropic request parses");
        assert_eq!(ir.messages[0].role, RoleIr::System);
        assert_eq!(ir.messages[1].role, RoleIr::Assistant);
        assert!(matches!(
            ir.messages[1].content[1],
            ContentIr::ToolUse(ref call) if call.index == 0
        ));
        assert_eq!(ir.messages[2].role, RoleIr::Tool);
        assert_eq!(ir.messages[3].role, RoleIr::User);

        let encoded = super::encode_request(&ir, "anthropic-upstream").expect("encodes");
        assert_eq!(encoded["max_tokens"], json!(4096));
        assert_eq!(encoded["messages"][0]["role"], json!("assistant"));
        assert_eq!(
            encoded["messages"][1]["content"][0]["type"],
            json!("tool_result")
        );
    }

    #[test]
    fn parser_rejects_unknown_content_blocks() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "messages": [{ "role": "user", "content": [{ "type": "audio", "data": "fixture" }] }]
        });

        assert!(matches!(
            super::parse_request(&body),
            Err(super::super::BridgeError::Unsupported { .. })
        ));
        let _ = WireProtocol::AnthropicMessages;
    }

    #[test]
    fn parser_accepts_cached_tool_result_text_blocks() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call-fixture",
                        "name": "lookup",
                        "input": { "q": "fixture" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-fixture",
                        "content": [{
                            "type": "text",
                            "text": "result fixture"
                        }],
                        "cache_control": { "type": "ephemeral" }
                    }]
                }
            ]
        });

        let ir = super::parse_request(&body).expect("cached tool result parses");
        assert_eq!(ir.messages[1].role, RoleIr::Tool);
    }

    #[test]
    fn encoder_accepts_anthropic_none_tool_choice() {
        // Anthropic 官方支持 tool_choice none：应编码为 {"type":"none"} 而非拒绝。
        let body = json!({
            "model": "anthropic-fixture-model",
            "messages": [{ "role": "user", "content": "fixture" }],
            "tool_choice": { "type": "none" }
        });
        let ir = super::parse_request(&body).expect("Anthropic request parses");
        let encoded =
            super::encode_request(&ir, "anthropic-upstream").expect("encodes to anthropic");
        assert_eq!(encoded["tool_choice"]["type"], "none");
    }

    #[test]
    fn provider_max_output_extension_controls_missing_max_tokens_floor() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "messages": [{ "role": "user", "content": "fixture" }]
        });
        let mut ir = super::parse_request(&body).expect("Anthropic request parses");
        ir.extensions.insert(
            "runtime.provider.max_output_tokens".to_string(),
            json!(8192),
        );
        let encoded = super::encode_request(&ir, "anthropic-upstream").expect("encodes");
        assert_eq!(encoded["max_tokens"], json!(8192));
    }

    #[test]
    fn accepts_system_role_messages_like_claude_code() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "messages": [
                { "role": "system", "content": [
                    { "type": "text", "text": "system prompt", "cache_control": { "type": "ephemeral" } }
                ]},
                { "role": "user", "content": "fixture" }
            ]
        });
        let ir = super::parse_request(&body).expect("system-role message parses");
        assert_eq!(ir.messages[0].role, RoleIr::System);
        assert_eq!(ir.messages[0].content.len(), 1);
        let encoded = super::encode_request(&ir, "anthropic-upstream").expect("encodes");
        assert_eq!(encoded["messages"][0]["role"], "user");
        let system_contains_prompt = encoded["system"] == json!("system prompt")
            || encoded["system"].as_array().is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("text") == Some(&json!("system prompt")))
            });
        assert!(
            system_contains_prompt,
            "system-role message content must be emitted as top-level system: {:?}",
            encoded["system"]
        );
    }

    #[test]
    fn stream_encoder_preserves_cache_and_reasoning_tokens_in_usage_delta() {
        let mut state = super::super::StreamState::new();
        let events = vec![
            super::super::StreamEventIr::Started(super::super::ResponseMetaIr {
                id: Some("msg-stream-usage".to_string()),
                model: Some("anthropic-upstream".to_string()),
            }),
            super::super::StreamEventIr::Usage(super::super::UsageIr {
                input_tokens: Some(10),
                output_tokens: Some(7),
                total_tokens: Some(17),
                cache_read_tokens: Some(2),
                cache_write_tokens: Some(1),
                reasoning_tokens: Some(3),
            }),
        ];
        let frames = super::super::encode_stream_events(
            super::super::WireProtocol::AnthropicMessages,
            &events,
            &mut state,
        )
        .expect("usage event encodes");
        let usage_frame = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("message_delta"))
            .expect("usage emits a message_delta frame");
        let value: Value = serde_json::from_str(&usage_frame.data).expect("frame JSON parses");
        assert_eq!(value["usage"]["input_tokens"], json!(7));
        assert_eq!(value["usage"]["output_tokens"], json!(7));
        assert_eq!(value["usage"]["cache_read_input_tokens"], json!(2));
        assert_eq!(value["usage"]["cache_creation_input_tokens"], json!(1));
        assert_eq!(
            value["usage"]["output_tokens_details"]["thinking_tokens"],
            json!(3)
        );
    }

    #[test]
    fn parser_accepts_citations_on_text_blocks() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "messages": [
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "question",
                        "citations": null
                    }]
                },
                {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "text",
                            "text": "answer with citation",
                            "citations": [{
                                "type": "char_location",
                                "cited_text": "fixture source",
                                "document_index": 0,
                                "document_title": "fixture",
                                "start_char_index": 0,
                                "end_char_index": 14
                            }]
                        },
                        {
                            "type": "tool_use",
                            "id": "call_cited",
                            "name": "lookup",
                            "input": { "q": "fixture" }
                        }
                    ]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call_cited",
                        "content": [{
                            "type": "text",
                            "text": "tool output",
                            "citations": [{ "type": "char_location", "cited_text": "x" }]
                        }]
                    }]
                }
            ]
        });

        let ir = super::parse_request(&body).expect("citations on text blocks parse");
        assert!(matches!(
            ir.messages[0].content[0],
            super::super::ContentIr::Text(ref text) if text == "question"
        ));
        assert!(matches!(
            ir.messages[1].content[0],
            super::super::ContentIr::Text(ref text) if text == "answer with citation"
        ));
        assert!(matches!(
            ir.messages[2].content[0],
            super::super::ContentIr::ToolResult(ref result)
                if matches!(&result.content[0], super::super::ContentIr::Text(ref text) if text == "tool output")
        ));
    }

    #[test]
    fn parser_accepts_citations_on_system_text_blocks() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "system": [{
                "type": "text",
                "text": "system prompt",
                "citations": [{ "type": "char_location", "cited_text": "src" }]
            }],
            "messages": [{ "role": "user", "content": "fixture" }]
        });

        let ir = super::parse_request(&body).expect("citations on system blocks parse");
        assert_eq!(ir.messages[0].role, super::super::RoleIr::System);
        assert_eq!(ir.messages[0].content.len(), 1);
    }

    #[test]
    fn parser_accepts_fallbacks_and_preserves_on_anthropic_encode() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "fallbacks": ["fixture-fallback-model"],
            "messages": [{ "role": "user", "content": "fixture" }]
        });

        let ir = super::parse_request(&body).expect("fallbacks parse");
        assert_eq!(
            ir.extensions.get(super::EXT_FALLBACKS),
            Some(&json!(["fixture-fallback-model"]))
        );
        let encoded = super::encode_request(&ir, "anthropic-upstream").expect("anthropic encode");
        assert_eq!(encoded["fallbacks"], json!(["fixture-fallback-model"]));

        let mut chat = ir;
        chat.protocol = super::super::WireProtocol::OpenAiChat;
        let encoded = super::encode_request(&chat, "anthropic-upstream").expect("encodes");
        assert!(
            encoded.get("fallbacks").is_none(),
            "fallbacks must not leak into non-anthropic upstream requests"
        );
    }

    #[test]
    fn parser_accepts_output_config_format() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "output_config": {
                "effort": "high",
                "format": {
                    "type": "json_schema",
                    "name": "fixture-schema",
                    "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } }
                }
            },
            "messages": [{ "role": "user", "content": "fixture" }]
        });

        let ir = super::parse_request(&body).expect("output_config.format parses");
        assert_eq!(ir.generation.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn parser_accepts_cache_control_ttl_and_rejects_non_string() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "system": [{
                "type": "text",
                "text": "system prompt",
                "cache_control": { "type": "ephemeral", "ttl": "1h" }
            }],
            "tools": [{
                "name": "lookup",
                "description": "lookup",
                "input_schema": { "type": "object" },
                "cache_control": { "type": "ephemeral", "ttl": "5m" }
            }],
            "messages": [{ "role": "user", "content": "fixture" }]
        });

        let ir = super::parse_request(&body).expect("cache_control.ttl parses");
        assert_eq!(ir.tools.len(), 1);

        let mut bad = body.clone();
        bad["system"][0]["cache_control"]["ttl"] = json!(3600);
        assert!(matches!(
            super::parse_request(&bad),
            Err(super::super::BridgeError::InvalidRequest)
        ));
    }

    #[test]
    fn stream_decoder_normalizes_usage_input_to_full() {
        let frame = super::super::SseFrame {
            event: Some("message_start".to_string()),
            data: json!({
                "type": "message_start",
                "message": {
                    "id": "msg_stream_usage_full",
                    "type": "message",
                    "role": "assistant",
                    "model": "anthropic-upstream",
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 4,
                        "cache_read_input_tokens": 2,
                        "cache_creation_input_tokens": 5
                    }
                }
            })
            .to_string(),
        };
        let mut state = super::super::StreamState::new();
        let events = super::decode_stream_frame(&frame, &mut state).expect("stream usage decodes");
        let usage = events
            .iter()
            .find_map(|event| match event {
                super::super::StreamEventIr::Usage(usage) => Some(usage),
                _ => None,
            })
            .expect("usage event present");
        assert_eq!(usage.input_tokens, Some(19));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.cache_read_tokens, Some(2));
        assert_eq!(usage.cache_write_tokens, Some(5));
    }

    #[test]
    fn stream_encoder_bills_direct_connect_usage_once() {
        let mut state = super::super::StreamState::new();
        let events = vec![
            super::super::StreamEventIr::Started(super::super::ResponseMetaIr {
                id: Some("msg_stream_direct".to_string()),
                model: Some("anthropic-upstream".to_string()),
            }),
            super::super::StreamEventIr::Usage(super::super::UsageIr {
                input_tokens: Some(19),
                output_tokens: Some(4),
                total_tokens: None,
                cache_read_tokens: Some(2),
                cache_write_tokens: Some(5),
                reasoning_tokens: None,
            }),
        ];
        let frames = super::super::encode_stream_events(
            super::super::WireProtocol::AnthropicMessages,
            &events,
            &mut state,
        )
        .expect("usage event encodes");
        let usage_frame = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("message_delta"))
            .expect("usage emits a message_delta frame");
        let value: Value = serde_json::from_str(&usage_frame.data).expect("frame JSON parses");
        assert_eq!(
            value["usage"]["input_tokens"],
            json!(12),
            "full input (19) must be billed exactly once: 19 - 2 cache_read - 5 cache_creation"
        );
        assert_eq!(value["usage"]["cache_read_input_tokens"], json!(2));
        assert_eq!(value["usage"]["cache_creation_input_tokens"], json!(5));
    }

    #[test]
    fn non_stream_decoder_normalizes_usage_input_to_full() {
        let body = json!({
            "id": "msg_usage_full",
            "type": "message",
            "role": "assistant",
            "model": "anthropic-upstream",
            "content": [{"type": "text", "text": "fixture response text"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 12,
                "output_tokens": 4,
                "cache_read_input_tokens": 2,
                "cache_creation_input_tokens": 5
            }
        });
        let ir = super::decode_response(body).expect("anthropic response decodes");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, Some(19));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.total_tokens, Some(23));
        assert_eq!(usage.cache_read_tokens, Some(2));
        assert_eq!(usage.cache_write_tokens, Some(5));
    }

    #[test]
    fn non_stream_encoder_bills_direct_connect_usage_once() {
        for (usage, expected_input, expected_creation) in [
            (
                json!({
                    "input_tokens": 12,
                    "output_tokens": 4,
                    "cache_read_input_tokens": 2
                }),
                12,
                None,
            ),
            (
                json!({
                    "input_tokens": 12,
                    "output_tokens": 4,
                    "cache_read_input_tokens": 2,
                    "cache_creation_input_tokens": 5
                }),
                12,
                Some(5),
            ),
        ] {
            let body = json!({
                "id": "msg_usage_direct",
                "type": "message",
                "role": "assistant",
                "model": "anthropic-upstream",
                "content": [{"type": "text", "text": "fixture response text"}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": usage
            });
            let ir = super::decode_response(body).expect("anthropic response decodes");
            let encoded = super::encode_response(&ir).expect("anthropic response encodes");
            assert_eq!(
                encoded["usage"]["input_tokens"],
                json!(expected_input),
                "full input must be billed exactly once: no double subtraction on the direct path"
            );
            assert_eq!(encoded["usage"]["cache_read_input_tokens"], json!(2));
            match expected_creation {
                Some(value) => assert_eq!(
                    encoded["usage"]["cache_creation_input_tokens"],
                    json!(value)
                ),
                None => assert!(
                    encoded["usage"]
                        .get("cache_creation_input_tokens")
                        .is_none(),
                    "cache_creation_input_tokens must be omitted when absent upstream"
                ),
            }
        }
    }

    #[test]
    fn stream_encoder_emits_full_usage_in_terminal_message_delta() {
        let mut state = super::super::StreamState::new();
        let events = vec![
            super::super::StreamEventIr::Started(super::super::ResponseMetaIr {
                id: Some("msg-stream-terminal".to_string()),
                model: Some("anthropic-upstream".to_string()),
            }),
            super::super::StreamEventIr::TextDelta {
                text: "hi".to_string(),
            },
            super::super::StreamEventIr::Usage(super::super::UsageIr {
                input_tokens: Some(10),
                output_tokens: Some(7),
                total_tokens: Some(17),
                cache_read_tokens: Some(2),
                cache_write_tokens: None,
                reasoning_tokens: Some(3),
            }),
            super::super::StreamEventIr::Completed(super::super::CompletionIr {
                finish_reason: Some("stop".to_string()),
                stop_reason: Some("end_turn".to_string()),
                status: Some("completed".to_string()),
                error: None,
            }),
        ];
        let frames = super::super::encode_stream_events(
            super::super::WireProtocol::AnthropicMessages,
            &events,
            &mut state,
        )
        .expect("terminal stream encodes");
        let deltas: Vec<_> = frames
            .iter()
            .filter(|frame| frame.event.as_deref() == Some("message_delta"))
            .collect();
        assert_eq!(deltas.len(), 2, "usage delta then terminal delta");
        let terminal = deltas[1];
        let value: Value = serde_json::from_str(&terminal.data).expect("frame JSON parses");
        assert_eq!(value["delta"]["stop_reason"], json!("end_turn"));
        assert_eq!(value["usage"]["input_tokens"], json!(8));
        assert_eq!(value["usage"]["cache_read_input_tokens"], json!(2));
        assert_eq!(
            value["usage"]["output_tokens_details"]["thinking_tokens"],
            json!(3)
        );
    }
}

#[cfg(test)]
mod tests_empty_text_block {
    use serde_json::json;

    use super::super::parse_request;
    use super::super::WireProtocol;

    #[test]
    fn assistant_empty_text_block_before_tool_use_is_skipped() {
        let body = json!({
            "model": "anthropic-fixture-model",
            "max_tokens": 64,
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "" },
                        { "type": "tool_use", "id": "call_1", "name": "Bash", "input": { "command": "ls" } },
                        { "type": "tool_use", "id": "call_2", "name": "Read", "input": { "file_path": "/tmp/x" } }
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        { "type": "tool_result", "tool_use_id": "call_2", "content": "b" },
                        { "type": "tool_result", "tool_use_id": "call_1", "content": "a" }
                    ]
                }
            ]
        });
        let mut only_empty = body.clone();
        only_empty["messages"] =
            json!([{"role": "assistant", "content": [{"type": "text", "text": ""}]}]);
        let _ = parse_request(WireProtocol::AnthropicMessages, &only_empty)
            .expect("assistant with only empty text parses");
        let mut stripped = body.clone();
        stripped["messages"] = json!([body["messages"][0].clone()]);
        let _ = parse_request(WireProtocol::AnthropicMessages, &stripped)
            .expect("assistant with empty text block parses");
        let full = parse_request(WireProtocol::AnthropicMessages, &body)
            .expect("empty text block and reversed results accepted");
        let assistant = full
            .messages
            .iter()
            .find(|m| m.role == super::super::RoleIr::Assistant)
            .expect("assistant message present");
        assert_eq!(
            assistant
                .content
                .iter()
                .filter(|c| matches!(c, super::super::ContentIr::Text(_)))
                .count(),
            0,
            "empty text block must be skipped"
        );
    }
}
