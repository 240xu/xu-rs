use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtocolKind {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

impl ProtocolKind {
    #[allow(non_upper_case_globals)]
    pub const Chat: Self = Self::OpenAiChat;
    #[allow(non_upper_case_globals)]
    pub const Responses: Self = Self::OpenAiResponses;
    #[allow(non_upper_case_globals)]
    pub const Anthropic: Self = Self::AnthropicMessages;

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai-chat",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    pub fn parse_legacy(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "chat" | "openai-chat" | "openai_chat" => Ok(Self::OpenAiChat),
            "responses" | "openai-responses" | "openai_responses" => Ok(Self::OpenAiResponses),
            "anthropic" | "messages" | "anthropic-messages" | "anthropic_messages" => {
                Ok(Self::AnthropicMessages)
            }
            other => Err(format!("unsupported protocol/apiKind: {other}")),
        }
    }

    pub fn is_openai_compatible(self) -> bool {
        matches!(self, Self::OpenAiChat | Self::OpenAiResponses)
    }
}

impl fmt::Display for ProtocolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutingMode {
    DirectFile,
    LocalAnthropicMessages,
    LocalOpenAiChat,
    LocalOpenAiResponses,
    Unsupported,
}

impl RoutingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectFile => "direct-file",
            Self::LocalAnthropicMessages => "local-anthropic-messages",
            Self::LocalOpenAiChat => "local-openai-chat",
            Self::LocalOpenAiResponses => "local-openai-responses",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn needs_local_proxy(self) -> bool {
        !matches!(self, Self::DirectFile | Self::Unsupported)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolAdapter {
    pub from: ProtocolKind,
    pub to: ProtocolKind,
    pub routing_mode: RoutingMode,
    pub endpoint: Option<String>,
}
