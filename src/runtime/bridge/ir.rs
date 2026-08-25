use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::ProtocolKind;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    AnthropicMessages,
    OpenAiChat,
    OpenAiResponses,
}

impl WireProtocol {
    pub fn protocol_kind(self) -> ProtocolKind {
        match self {
            Self::AnthropicMessages => ProtocolKind::AnthropicMessages,
            Self::OpenAiChat => ProtocolKind::OpenAiChat,
            Self::OpenAiResponses => ProtocolKind::OpenAiResponses,
        }
    }
}

impl From<ProtocolKind> for WireProtocol {
    fn from(value: ProtocolKind) -> Self {
        match value {
            ProtocolKind::AnthropicMessages => Self::AnthropicMessages,
            ProtocolKind::OpenAiChat => Self::OpenAiChat,
            ProtocolKind::OpenAiResponses => Self::OpenAiResponses,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RequestIr {
    pub protocol: WireProtocol,
    pub model: String,
    pub messages: Vec<MessageIr>,
    pub tools: Vec<ToolDefinitionIr>,
    pub tool_choice: Option<ToolChoiceIr>,
    pub generation: GenerationIr,
    pub stream: bool,
    pub metadata: Option<Value>,
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MessageIr {
    pub role: RoleIr,
    pub content: Vec<ContentIr>,
    pub name: Option<String>,
    pub item_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum RoleIr {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum ContentIr {
    Text(String),
    Image(MediaIr),
    Document(MediaIr),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse(ToolCallIr),
    ToolResult(ToolResultIr),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MediaIr {
    pub media_type: Option<String>,
    pub url: Option<String>,
    pub data: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolCallIr {
    pub call_id: String,
    pub item_id: Option<String>,
    pub index: usize,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolResultIr {
    pub call_id: String,
    pub content: Vec<ContentIr>,
    pub is_error: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinitionIr {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    /// OpenAI 结构化输出保证；无法表示该字段的协议必须 fail-closed 拒绝。
    #[serde(default)]
    pub strict: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum ToolChoiceIr {
    Auto,
    Any,
    None,
    Tool { name: String },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct GenerationIr {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop_sequences: Vec<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ResponseMetaIr {
    pub id: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct UsageIr {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CompletionIr {
    pub finish_reason: Option<String>,
    pub stop_reason: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ResponseIr {
    pub meta: ResponseMetaIr,
    pub content: Vec<ContentIr>,
    pub usage: Option<UsageIr>,
    pub completion: CompletionIr,
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum StreamEventIr {
    Started(ResponseMetaIr),
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallStarted(ToolCallIr),
    ToolCallArgumentsDelta { call_id: String, delta: String },
    ToolCallFinished { call_id: String },
    Usage(UsageIr),
    Completed(CompletionIr),
    Failed(BridgeError),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum BridgeError {
    InvalidRequest,
    Unsupported {
        #[serde(skip)]
        field: String,
    },
    InvalidUpstream,
    ToolState,
    ResourceLimit,
    Timeout,
    Internal,
}

impl fmt::Debug for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeError")
            .field("category", &self.log_category())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BridgeErrorBody {
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BridgeErrorEnvelope {
    pub error: BridgeErrorBody,
}

impl BridgeError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidRequest | Self::Unsupported { .. } | Self::ToolState => 400,
            Self::InvalidUpstream => 502,
            Self::ResourceLimit => 413,
            Self::Timeout => 504,
            Self::Internal => 500,
        }
    }

    pub fn log_category(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unsupported { .. } => "unsupported",
            Self::InvalidUpstream => "invalid_upstream",
            Self::ToolState => "tool_state",
            Self::ResourceLimit => "resource_limit",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
        }
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid request",
            Self::Unsupported { .. } => "unsupported request field",
            Self::InvalidUpstream => "invalid upstream response",
            Self::ToolState => "invalid tool state",
            Self::ResourceLimit => "request exceeds resource limit",
            Self::Timeout => "upstream request timed out",
            Self::Internal => "internal bridge error",
        }
    }

    pub fn public_envelope(&self) -> BridgeErrorEnvelope {
        BridgeErrorEnvelope {
            error: BridgeErrorBody {
                code: self.log_category(),
                message: self.public_message(),
            },
        }
    }
}

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message())
    }
}

impl std::error::Error for BridgeError {}
