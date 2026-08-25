use crate::domain::ProtocolKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: ProtocolKind,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
}

pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "openai-chat",
        name: "OpenAI Chat",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.openai.com/v1",
        models: &["gpt-4.1", "gpt-4.1-mini", "gpt-4o-mini"],
    },
    ProviderPreset {
        id: "openai-responses",
        name: "OpenAI Responses",
        protocol: ProtocolKind::OpenAiResponses,
        base_url: "https://api.openai.com/v1",
        models: &["gpt-4.1", "gpt-4.1-mini", "gpt-4o-mini"],
    },
    ProviderPreset {
        id: "openrouter",
        name: "OpenRouter",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://openrouter.ai/api/v1",
        models: &[
            "openai/gpt-4o-mini",
            "anthropic/claude-3.5-sonnet",
            "deepseek/deepseek-chat",
        ],
    },
    ProviderPreset {
        id: "deepseek",
        name: "DeepSeek",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.deepseek.com/v1",
        models: &["deepseek-chat", "deepseek-reasoner"],
    },
    ProviderPreset {
        id: "moonshot",
        name: "Moonshot / Kimi",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.moonshot.cn/v1",
        models: &["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"],
    },
    ProviderPreset {
        id: "anthropic",
        name: "Anthropic",
        protocol: ProtocolKind::AnthropicMessages,
        base_url: "https://api.anthropic.com/v1",
        models: &["claude-sonnet-4-20250514", "claude-3-5-sonnet-latest"],
    },
    ProviderPreset {
        id: "gemini",
        name: "Google Gemini (OpenAI compatible)",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        models: &["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.0-flash"],
    },
    ProviderPreset {
        id: "zhipu-glm",
        name: "Zhipu GLM",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: &["glm-4.7", "glm-4.5", "glm-4.7-flash", "glm-4.5-flash"],
    },
    ProviderPreset {
        id: "qwen",
        name: "Alibaba Qwen (DashScope)",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        models: &["qwen-max-latest", "qwen-plus-latest", "qwen-turbo-latest"],
    },
    ProviderPreset {
        id: "minimax",
        name: "MiniMax",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.minimax.chat/v1",
        models: &["MiniMax-Text-01", "abab6.5s-chat"],
    },
    ProviderPreset {
        id: "siliconflow",
        name: "SiliconFlow",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.siliconflow.cn/v1",
        models: &[
            "Qwen/Qwen2.5-7B-Instruct",
            "deepseek-ai/DeepSeek-V3",
            "THUDM/glm-4-9b-chat",
        ],
    },
    ProviderPreset {
        id: "mistral",
        name: "Mistral",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.mistral.ai/v1",
        models: &[
            "mistral-large-latest",
            "codestral-latest",
            "open-mistral-nemo",
        ],
    },
    ProviderPreset {
        id: "groq",
        name: "Groq",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.groq.com/openai/v1",
        models: &[
            "llama-3.3-70b-versatile",
            "qwen-2.5-coder-32b",
            "deepseek-r1-distill-llama-70b",
        ],
    },
    ProviderPreset {
        id: "xai",
        name: "xAI Grok",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.x.ai/v1",
        models: &["grok-4", "grok-3", "grok-3-mini"],
    },
    ProviderPreset {
        id: "together",
        name: "Together AI",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://api.together.xyz/v1",
        models: &[
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "Qwen/Qwen2.5-72B-Instruct-Turbo",
        ],
    },
    ProviderPreset {
        id: "nvidia-nim",
        name: "NVIDIA NIM",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://integrate.api.nvidia.com/v1",
        models: &["meta/llama-3.3-70b-instruct", "deepseek-ai/deepseek-r1"],
    },
    ProviderPreset {
        id: "volcengine-ark",
        name: "Volcengine Ark",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        models: &["deepseek-v3.1", "doubao-pro-32k"],
    },
    ProviderPreset {
        id: "ollama",
        name: "Ollama (local)",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "http://127.0.0.1:11434/v1",
        models: &["llama3.2", "qwen2.5"],
    },
    ProviderPreset {
        id: "opencode-zen",
        name: "OpenCode Zen (free)",
        protocol: ProtocolKind::OpenAiChat,
        base_url: "https://opencode.ai/zen/v1",
        models: &["deepseek-v4-flash-free", "mimo-v2.5-free"],
    },
];

pub fn all() -> &'static [ProviderPreset] {
    PRESETS
}

pub fn find(id: &str) -> Option<&'static ProviderPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

pub fn protocol_key(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::OpenAiChat => "chat",
        ProtocolKind::OpenAiResponses => "responses",
        ProtocolKind::AnthropicMessages => "anthropic",
    }
}
