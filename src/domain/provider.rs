use std::collections::BTreeMap;

use serde_json::Value;

use super::protocol::ProtocolKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderVendor {
    OpenAi,
    Anthropic,
    OpenRouter,
    Gemini,
    DeepSeek,
    Moonshot,
    CustomOpenAiCompatible,
    CustomAnthropicCompatible,
    Unknown,
}

impl ProviderVendor {
    pub fn infer(id: &str, base_url: &str, protocol: ProtocolKind) -> Self {
        let haystack = format!("{} {}", id, base_url).to_ascii_lowercase();
        if haystack.contains("openrouter") {
            Self::OpenRouter
        } else if haystack.contains("anthropic")
            || matches!(protocol, ProtocolKind::AnthropicMessages)
        {
            Self::Anthropic
        } else if haystack.contains("generativelanguage") || haystack.contains("gemini") {
            Self::Gemini
        } else if haystack.contains("deepseek") {
            Self::DeepSeek
        } else if haystack.contains("moonshot") || haystack.contains("kimi") {
            Self::Moonshot
        } else if haystack.contains("openai") {
            Self::OpenAi
        } else if protocol.is_openai_compatible() {
            Self::CustomOpenAiCompatible
        } else {
            Self::CustomAnthropicCompatible
        }
    }
}

/// A parsed model entry. `client_name` is the model identity used for the
/// client-facing slug (`<client_name>_<provider_id>`); `display_name` is what
/// client menus show; `request_name` is the upstream model id actually sent
/// to the provider. Plain string entries have all three equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    pub client_name: String,
    pub display_name: String,
    pub request_name: String,
}

#[derive(Clone, Debug)]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub notes: Option<String>,
    pub website: Option<String>,
    pub vendor: ProviderVendor,
    pub protocol: ProtocolKind,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub model_entries: BTreeMap<String, ModelEntry>,
    pub model_metadata: BTreeMap<String, Value>,
    pub claude_slots: BTreeMap<String, String>,
    pub default_model: String,
    pub extra_headers: BTreeMap<String, String>,
    pub request_url_mode: Option<String>,
    pub header_mode: Option<String>,
    pub timeout_ms: u64,
    pub max_retries: u32,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub cache_mode: CacheMode,
}

/// Upstream prompt-cache behavior. `Auto` selects by provider vendor
/// (deepseek vendors -> DeepSeek), `Compat` assumes a lenient OpenAI-style
/// prefix cache, `DeepSeek` applies the DeepSeek prefix-unit stability
/// optimizations (sorted tools, pinned system prefix).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheMode {
    #[default]
    Auto,
    Compat,
    DeepSeek,
}

impl CacheMode {
    pub fn parse_legacy(text: &str) -> CacheMode {
        match text.trim().to_ascii_lowercase().as_str() {
            "compat" | "openai" => CacheMode::Compat,
            "deepseek" | "ds" => CacheMode::DeepSeek,
            _ => CacheMode::Auto,
        }
    }

    /// 实际生效的缓存模式：显式指定优先；Auto 下按 vendor / 模型名识别
    /// DeepSeek（自动启用前缀稳定优化，无需手动配置）。
    pub fn effective(&self, vendor: ProviderVendor, model: &str) -> CacheMode {
        match self {
            CacheMode::DeepSeek => CacheMode::DeepSeek,
            CacheMode::Auto
                if vendor == ProviderVendor::DeepSeek
                    || model.to_ascii_lowercase().contains("deepseek") =>
            {
                CacheMode::DeepSeek
            }
            mode => *mode,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CacheMode::Auto => "auto",
            CacheMode::Compat => "compat",
            CacheMode::DeepSeek => "deepseek",
        }
    }
}

impl ProviderProfile {
    pub fn model_slug(&self, model: &str) -> String {
        format!("{model}_{}", self.id)
    }

    pub fn first_model(&self) -> Result<&str, String> {
        if !self.default_model.trim().is_empty() {
            Ok(&self.default_model)
        } else {
            self.models
                .first()
                .map(String::as_str)
                .ok_or_else(|| format!("provider {} has no models", self.id))
        }
    }

    pub fn model_metadata(&self, model: &str) -> Option<&serde_json::Map<String, Value>> {
        self.model_metadata.get(model).and_then(Value::as_object)
    }

    pub fn claude_slot(&self, slot: &str) -> Option<&str> {
        self.claude_slots
            .get(slot)
            .map(String::as_str)
            .filter(|model| !model.trim().is_empty())
    }

    /// Upstream model id for a client-facing model name. Object entries
    /// return their `requestName` (map entries default it to the entry key),
    /// string entries and unknown models return the model name itself.
    pub fn request_name_for<'a>(&'a self, model: &'a str) -> &'a str {
        self.model_entries
            .get(model)
            .map_or(model, |entry| entry.request_name.as_str())
    }

    /// Upstream model id for a configured client-facing model name. `models`
    /// membership is the single source of truth: string entries resolve to
    /// themselves, object entries to their `request_name`. A request name is
    /// also accepted via reverse lookup so client and request aliases of the
    /// same entry resolve identically. Unknown models return `None` (callers
    /// fail closed instead of leaking the raw name upstream).
    pub fn configured_request_name_for<'a>(&'a self, model: &'a str) -> Option<&'a str> {
        if self.models.iter().any(|name| name == model) {
            return self
                .model_entries
                .get(model)
                .map(|entry| entry.request_name.as_str())
                .or(Some(model));
        }
        self.model_entries
            .values()
            .find(|entry| entry.request_name == model)
            .map(|entry| entry.request_name.as_str())
    }

    /// Display name for a client-facing model name. Object entries return
    /// their `name` (defaulting to `requestName`, then to the model name),
    /// string entries and unknown models return the model name itself.
    pub fn display_name_for<'a>(&'a self, model: &'a str) -> &'a str {
        self.model_entries
            .get(model)
            .map_or(model, |entry| entry.display_name.as_str())
    }

    pub fn validate_for_write(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("provider id is empty".to_string());
        }
        if let Some(website) = self
            .website
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            if !(website.starts_with("https://") || website.starts_with("http://")) {
                return Err(format!(
                    "provider {} website must start with http:// or https://",
                    self.id
                ));
            }
        }
        if self.models.is_empty() {
            return Err(format!("provider {} has no models", self.id));
        }
        if !self.default_model.trim().is_empty() && !self.models.contains(&self.default_model) {
            return Err(format!(
                "provider {} default_model is not listed in models",
                self.id
            ));
        }
        if !(self.base_url.starts_with("https://") || self.base_url.starts_with("http://")) {
            return Err(format!(
                "provider {} base_url must start with http:// or https://",
                self.id
            ));
        }
        Ok(())
    }
}
