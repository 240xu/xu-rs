use std::path::Path;

use serde_json::{json, Map, Value};

use super::{json_patch, serve_port, ApplyPlan, FileAdapter};
use crate::domain::{
    AgentConfigSpec, AgentTarget, ProtocolAdapter, ProtocolKind, ProviderProfile, RoutingMode,
};

pub(super) struct ClaudeCodeAdapter;

impl FileAdapter for ClaudeCodeAdapter {
    fn spec(&self, home: &Path) -> AgentConfigSpec {
        AgentConfigSpec {
            target: AgentTarget::ClaudeCode,
            config_files: vec![home.join(".claude/settings.json")],
            native_protocol: ProtocolKind::AnthropicMessages,
            supports_direct_file: true,
            supports_local_routing: true,
        }
    }

    fn build_plan(&self, home: &Path, providers: &[&ProviderProfile]) -> Result<ApplyPlan, String> {
        if let Some(provider) = providers
            .iter()
            .find(|provider| provider.protocol == ProtocolKind::OpenAiResponses)
        {
            return Err(format!(
                "Claude Code cannot use Responses-only provider {}; Responses -> Anthropic Messages adaptation is not safely supported",
                provider.id
            ));
        }
        let provider = providers[0];
        let mut mapping = claude_mapping(provider, providers.len())?;
        let endpoint = if mapping.routing_mode.needs_local_proxy() {
            Some(format!("http://127.0.0.1:{}/v1/messages", serve_port()))
        } else {
            None
        };

        if providers.len() > 1 && !mapping.routing_mode.needs_local_proxy() {
            mapping.warnings.push(
                "Claude Code direct Anthropic config can use only the first selected provider"
                    .to_string(),
            );
        }

        let path = home.join(".claude/settings.json");
        let before = crate::patch::read_before_checked(&path)?;
        let doc = merge_claude_config(&before, &mapping, providers, endpoint)?;

        Ok(ApplyPlan {
            target: AgentTarget::ClaudeCode,
            routing_mode: mapping.routing_mode,
            protocol_adapter: mapping.adapter,
            patches: vec![json_patch(path, doc)?],
            summary: vec![match mapping.routing_mode {
                RoutingMode::DirectFile => {
                    "Claude Code: direct Anthropic-compatible settings".to_string()
                }
                RoutingMode::LocalAnthropicMessages => {
                    "Claude Code: Anthropic /v1/messages through local protocol adapter".to_string()
                }
                _ => "Claude Code: generated settings".to_string(),
            }],
            warnings: mapping.warnings,
        })
    }
}

fn merge_claude_config(
    before: &str,
    mapping: &ClaudeMapping,
    providers: &[&ProviderProfile],
    endpoint: Option<String>,
) -> Result<Value, String> {
    let mut doc = if before.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(before).map_err(|error| format!("parse Claude settings: {error}"))?
    };
    let root = doc
        .as_object_mut()
        .ok_or_else(|| "Claude settings root must be an object".to_string())?;
    {
        let env = root
            .entry("env")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| "Claude settings env must be an object".to_string())?;
        env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            Value::String(mapping.base_url.clone()),
        );
        if mapping.api_key.trim().is_empty() {
            // Keyless providers (e.g. zen): still set a placeholder auth token
            // so Claude Code skips its login gate; the runtime does not
            // forward auth headers for empty-key providers.
            env.insert(
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                Value::String("local-proxy".to_string()),
            );
            env.remove("ANTHROPIC_API_KEY");
        } else {
            // CC Switch style: AUTH_TOKEN only. Setting both makes Claude Code
            // warn that auth may not work as expected.
            env.insert(
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                Value::String(mapping.api_key.clone()),
            );
            env.remove("ANTHROPIC_API_KEY");
        }
        env.insert(
            "ANTHROPIC_MODEL".to_string(),
            Value::String(mapping.model.clone()),
        );
        env.insert(
            "ANTHROPIC_SMALL_FAST_MODEL".to_string(),
            Value::String(mapping.model.clone()),
        );
        // claudeSlots 配置优先覆盖四槽位；未配置时 Opus 带 [1m] 提供 1M 选项，
        // Sonnet/Haiku/Fable 映射基础模型。
        let slot_model = |slot: &str| -> String {
            providers
                .first()
                .and_then(|p| p.claude_slot(slot))
                .unwrap_or(mapping.model.as_str())
                .to_string()
        };
        env.insert(
            "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
            Value::String(slot_model("opus")),
        );
        env.insert(
            "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
            Value::String(slot_model("sonnet")),
        );
        env.insert(
            "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
            Value::String(slot_model("haiku")),
        );
        env.insert(
            "ANTHROPIC_DEFAULT_FABLE_MODEL".to_string(),
            Value::String(slot_model("fable")),
        );
        if let Some(provider) = providers.first() {
            env.insert(
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS".to_string(),
                Value::String(provider.context_window.to_string()),
            );
        }
        env.insert(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
            Value::String("1".to_string()),
        );
        // Not an officially documented variable: the flag was confirmed by
        // binary reverse engineering of Claude Code (it short-circuits every
        // terminal title write, so the thinking/running/done title churn is
        // suppressed for managed sessions).
        env.insert(
            "CLAUDE_CODE_DISABLE_TERMINAL_TITLE".to_string(),
            Value::String("1".to_string()),
        );
    }
    // Note: Claude Code 2.1.x modelOverrides expects string values (a model
    // alias mapping), not a window object; writing the old object shape made
    // the whole settings file fail validation and get skipped. Window
    // declaration is handled by CLAUDE_CODE_MAX_CONTEXT_TOKENS and the [1m]
    // model-name suffix instead, so no modelOverrides is emitted.
    root.insert("model".to_string(), Value::String(mapping.model.clone()));
    // /model must show only the user's models: the built-in Opus/Sonnet/Haiku
    // catalog entries are hidden unless an allowlist exists, so we always
    // emit one covering the Default and the 1M (Opus) slot.
    root.insert(
        "availableModels".to_string(),
        json!([mapping.model, format!("{}[1m]", mapping.model)]),
    );
    root.insert("enforceAvailableModels".to_string(), json!(true));
    root.insert(
        "xuCodex".to_string(),
        json!({
            "managed": true,
            "target": "claude",
            "mode": if providers.len() > 1 { "multi" } else { "single" },
            "providers": providers.iter().map(|provider| provider.id.clone()).collect::<Vec<_>>(),
            "routing": mapping.routing_mode.as_str(),
            "endpoint": endpoint
        }),
    );
    Ok(doc)
}

#[derive(Clone, Debug)]
struct ClaudeMapping {
    routing_mode: RoutingMode,
    base_url: String,
    api_key: String,
    model: String,
    adapter: Option<ProtocolAdapter>,
    warnings: Vec<String>,
}

fn claude_mapping(
    provider: &ProviderProfile,
    provider_count: usize,
) -> Result<ClaudeMapping, String> {
    let first_model = provider.first_model()?;
    let mut warnings = Vec::new();

    if provider.protocol == ProtocolKind::AnthropicMessages && provider_count == 1 {
        return Ok(ClaudeMapping {
            routing_mode: RoutingMode::DirectFile,
            base_url: provider.base_url.trim_end_matches('/').to_string(),
            api_key: provider.api_key.clone(),
            model: provider
                .configured_request_name_for(first_model)
                .unwrap_or(first_model)
                .to_string(),
            adapter: None,
            warnings,
        });
    }

    if provider.protocol == ProtocolKind::AnthropicMessages && provider_count > 1 {
        warnings.push("Claude Code multi-provider selection uses local routing even for Anthropic-compatible providers".to_string());
    }

    warnings
        .push("Local routing requires running `spec serve` while using Claude Code".to_string());

    let model = provider.model_slug(first_model);
    Ok(ClaudeMapping {
        routing_mode: RoutingMode::LocalAnthropicMessages,
        base_url: format!("http://127.0.0.1:{}", serve_port()),
        api_key: "local-proxy".to_string(),
        model,
        adapter: Some(ProtocolAdapter {
            from: provider.protocol,
            to: ProtocolKind::AnthropicMessages,
            routing_mode: RoutingMode::LocalAnthropicMessages,
            endpoint: Some(format!("http://127.0.0.1:{}/v1/messages", serve_port())),
        }),
        warnings,
    })
}

#[cfg(test)]
mod tests_slots {
    fn direct_provider() -> crate::domain::ProviderProfile {
        crate::domain::ProviderProfile {
            id: "zen".into(),
            name: "zen".into(),
            notes: None,
            website: None,
            vendor: crate::domain::ProviderVendor::CustomOpenAiCompatible,
            protocol: crate::domain::ProtocolKind::AnthropicMessages,
            base_url: "https://zen/v1".into(),
            api_key: String::new(),
            models: vec!["client-key".into()],
            model_metadata: Default::default(),
            model_entries: [(
                "client-key".into(),
                crate::domain::ModelEntry {
                    client_name: "client-key".into(),
                    display_name: "Client key".into(),
                    request_name: "upstream-name".into(),
                },
            )]
            .into_iter()
            .collect(),
            claude_slots: Default::default(),
            default_model: "client-key".into(),
            extra_headers: Default::default(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: 90000,
            max_retries: 1,
            context_window: 128000,
            max_output_tokens: 32768,
            reasoning_effort: None,
            cache_mode: crate::domain::CacheMode::Auto,
        }
    }

    #[test]
    fn direct_claude_mapping_uses_request_name() {
        let provider = direct_provider();
        let mapping = super::claude_mapping(&provider, 1).unwrap();
        assert_eq!(mapping.routing_mode, super::RoutingMode::DirectFile);
        assert_eq!(mapping.model, "upstream-name");
    }

    #[test]
    fn claude_slots_override_default_slot_envs() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        let mut provider = crate::domain::ProviderProfile {
            id: "zen".to_string(),
            name: "zen".to_string(),
            notes: None,
            website: None,
            vendor: crate::domain::ProviderVendor::CustomOpenAiCompatible,
            protocol: crate::domain::ProtocolKind::OpenAiChat,
            base_url: "https://zen/v1".to_string(),
            api_key: String::new(),
            models: vec!["deepseek-v4-flash".to_string()],
            model_metadata: std::collections::BTreeMap::new(),
            model_entries: std::collections::BTreeMap::new(),
            claude_slots: std::collections::BTreeMap::new(),
            default_model: "deepseek-v4-flash".to_string(),
            extra_headers: std::collections::BTreeMap::new(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: 90000,
            max_retries: 1,
            context_window: 128000,
            max_output_tokens: 32768,
            reasoning_effort: None,
            cache_mode: crate::domain::CacheMode::Auto,
        };
        provider.claude_slots = [
            ("fable", "kimi-k3"),
            ("opus", "deepseek-v4-pro"),
            ("sonnet", "glm-5.2"),
            ("haiku", "qwen3.5-plus"),
        ]
        .into_iter()
        .map(|(slot, model)| (slot.to_string(), model.to_string()))
        .collect();
        let plan = crate::agents::apply_agent(
            root.path(),
            crate::domain::AgentTarget::ClaudeCode,
            std::slice::from_ref(&provider),
            std::slice::from_ref(&"zen".to_string()),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&plan.patches[0].after).unwrap();
        let env = &value["env"];
        assert_eq!(env["ANTHROPIC_DEFAULT_FABLE_MODEL"], "kimi-k3");
        assert_eq!(env["ANTHROPIC_DEFAULT_OPUS_MODEL"], "deepseek-v4-pro");
        assert_eq!(env["ANTHROPIC_DEFAULT_SONNET_MODEL"], "glm-5.2");
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "qwen3.5-plus");
    }
}
