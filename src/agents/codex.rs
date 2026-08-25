use std::path::Path;

use serde_json::json;
use toml_edit::{value, DocumentMut, Item, Table};

use super::{sanitize_toml_key, serve_port, ApplyPlan, FileAdapter};
use crate::domain::{
    AgentConfigSpec, AgentTarget, ProtocolAdapter, ProtocolKind, ProviderProfile, RoutingMode,
};
use crate::patch::{read_before_checked, ConfigPatch};

pub(super) struct CodexAdapter;

impl FileAdapter for CodexAdapter {
    fn spec(&self, home: &Path) -> AgentConfigSpec {
        AgentConfigSpec {
            target: AgentTarget::Codex,
            config_files: vec![
                home.join(".codex/config.toml"),
                home.join(".codex/model-catalogs/xucodex-catalog.json"),
            ],
            native_protocol: ProtocolKind::OpenAiResponses,
            supports_direct_file: true,
            supports_local_routing: true,
        }
    }

    fn build_plan(&self, home: &Path, providers: &[&ProviderProfile]) -> Result<ApplyPlan, String> {
        let first = providers[0];
        let use_direct = providers.len() == 1 && first.protocol == ProtocolKind::OpenAiResponses;
        let routing_mode = if use_direct {
            RoutingMode::DirectFile
        } else {
            RoutingMode::LocalOpenAiResponses
        };
        let adapter = if use_direct {
            None
        } else {
            Some(ProtocolAdapter {
                from: first.protocol,
                to: ProtocolKind::OpenAiResponses,
                routing_mode,
                endpoint: Some(format!("http://127.0.0.1:{}/v1/responses", serve_port())),
            })
        };

        let selected_model = if use_direct {
            let client_model = first.first_model()?;
            first
                .configured_request_name_for(client_model)
                .unwrap_or(client_model)
                .to_string()
        } else {
            first.model_slug(first.first_model()?)
        };
        let (provider_name, base_url, bearer, wire_api) = if use_direct {
            (
                sanitize_toml_key(&format!("xu_{}", first.id)),
                first.base_url.trim_end_matches('/').to_string(),
                first.api_key.clone(),
                "responses",
            )
        } else {
            (
                "xucodex_multi".to_string(),
                format!("http://127.0.0.1:{}/v1", serve_port()),
                "local-proxy".to_string(),
                "responses",
            )
        };

        let config_path = home.join(".codex/config.toml");
        let catalog_path = home.join(".codex/model-catalogs/xucodex-catalog.json");
        let before = read_before_checked(&config_path)?;
        let config = merge_codex_config(
            &before,
            first,
            &selected_model,
            &provider_name,
            &base_url,
            &bearer,
            wire_api,
        )?;
        let catalog = codex_model_catalog(providers);

        let mut summary = vec![match routing_mode {
            RoutingMode::DirectFile => {
                "Codex: direct OpenAI Responses-compatible config".to_string()
            }
            _ => {
                "Codex: OpenAI Responses-compatible /v1/responses through local adapter".to_string()
            }
        }];
        if providers
            .iter()
            .any(|p| p.protocol == ProtocolKind::AnthropicMessages)
        {
            summary.push(
                "Codex: Anthropic providers require local Anthropic->Responses adaptation"
                    .to_string(),
            );
        }
        if routing_mode.needs_local_proxy() {
            summary.push(
                "Codex: local routing requires running `spec serve` while using Codex".to_string(),
            );
        }

        Ok(ApplyPlan {
            target: AgentTarget::Codex,
            routing_mode,
            protocol_adapter: adapter,
            patches: vec![
                ConfigPatch::new(config_path.clone(), before, config),
                super::json_patch(catalog_path, catalog)?,
            ],
            summary,
            warnings: Vec::new(),
        })
    }
}

fn merge_codex_config(
    before: &str,
    provider: &ProviderProfile,
    model: &str,
    provider_name: &str,
    base_url: &str,
    bearer: &str,
    wire_api: &str,
) -> Result<String, String> {
    let mut doc = if before.trim().is_empty() {
        DocumentMut::new()
    } else {
        before
            .parse::<DocumentMut>()
            .map_err(|error| format!("parse Codex config: {error}"))?
    };
    doc["model"] = value(model);
    doc["model_provider"] = value(provider_name);
    doc["model_context_window"] = value(i64::from(provider.context_window.max(128000)));
    doc["max_output_tokens"] = value(i64::from(provider.max_output_tokens.max(32768)));
    doc["model_reasoning_effort"] = value(provider.reasoning_effort.as_deref().unwrap_or("medium"));
    doc["model_catalog_json"] = value("model-catalogs/xucodex-catalog.json");

    if !doc.contains_key("model_providers") {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let providers = doc["model_providers"]
        .as_table_mut()
        .ok_or_else(|| "Codex model_providers must be a table".to_string())?;
    providers.retain(|key, _| !key.starts_with("xu_") && key != "xucodex_multi");
    let mut managed = Table::new();
    managed["name"] = value(if provider_name == "xucodex_multi" {
        "spec Multi Provider"
    } else {
        &provider.name
    });
    managed["base_url"] = value(base_url);
    if !bearer.trim().is_empty() {
        managed["experimental_bearer_token"] = value(bearer);
    }
    managed["wire_api"] = value(wire_api);
    managed["request_max_retries"] = value(i64::from(provider.max_retries));
    managed["stream_max_retries"] = value(i64::from(provider.max_retries));
    managed["stream_idle_timeout_ms"] =
        value(i64::try_from(provider.timeout_ms).unwrap_or(i64::MAX));
    managed["websocket_connect_timeout_ms"] =
        value(i64::try_from(provider.timeout_ms).unwrap_or(i64::MAX));
    providers.insert(provider_name, Item::Table(managed));
    Ok(doc.to_string())
}

fn codex_model_catalog(providers: &[&ProviderProfile]) -> serde_json::Value {
    let mut models = Vec::new();
    for provider in providers {
        for source_model in &provider.models {
            let slug = provider.model_slug(source_model);
            let reasoning_level = provider.reasoning_effort.as_deref().unwrap_or("medium");
            models.push(json!({
                "slug": slug,
                "display_name": format!("{} · {}", source_model, provider.id),
                "description": format!("{} mapped from {} ({})", source_model, provider.id, provider.protocol.as_str()),
                "visibility": "list",
                "supported_in_api": true,
                "context_window": provider.context_window.max(128000),
                "max_context_window": provider.context_window.max(128000),
                "output_limit": provider.max_output_tokens.max(32768),
                "supports_parallel_tool_calls": false,
                "default_reasoning_level": reasoning_level,
                "supported_reasoning_levels": [],
                "default_reasoning_summary": "none",
                "supports_reasoning_summaries": false,
                "shell_type": "shell_command",
                "priority": 0,
                "additional_speed_tiers": [],
                "service_tiers": [],
                "availability_nux": null,
                "upgrade": null,
                "support_verbosity": false,
                "default_verbosity": "low",
                "apply_patch_tool_type": "freeform",
                "web_search_tool_type": "text_and_image",
                "truncation_policy": { "mode": "tokens", "limit": 10000 },
                "supports_image_detail_original": true,
                "effective_context_window_percent": 95,
                "experimental_supported_tools": [],
                "input_modalities": ["text", "image"],
                "supports_search_tool": false,
                "use_responses_lite": false,
                "base_instructions": format!(
                    "You are Codex, a coding agent based on {}. You and the user share one workspace, and your job is to collaborate with them until their goal is genuinely handled.\n\n# Personality\nYou are an intelligent, playful, curious, and deeply present collaborator.\n\n# General\nYou bring a senior engineer's judgment to the work, ask good questions when the problem space is blurry, and become decisive once you have enough context to act. Follow the user's instructions carefully and write clean, well-tested code.",
                    source_model
                ),
                "model_messages": {
                    "instructions_template": format!(
                        "You are Codex, a coding agent based on {}. You and the user share one workspace, and your job is to collaborate with them until their goal is genuinely handled."
                    , source_model)
                }
            }));
        }
    }
    json!({ "models": models })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CacheMode, ProtocolKind, ProviderProfile, ProviderVendor};

    fn provider(id: &str) -> ProviderProfile {
        ProviderProfile {
            id: id.to_string(),
            name: id.to_string(),
            notes: None,
            website: None,
            vendor: ProviderVendor::Unknown,
            protocol: ProtocolKind::OpenAiChat,
            base_url: "http://localhost/v1".to_string(),
            api_key: "key".to_string(),
            models: vec!["deepseek-v4-flash-free".to_string()],
            model_entries: Default::default(),
            model_metadata: Default::default(),
            claude_slots: Default::default(),
            default_model: "deepseek-v4-flash-free".to_string(),
            extra_headers: Default::default(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: 60_000,
            max_retries: 3,
            context_window: 128_000,
            max_output_tokens: 32_768,
            reasoning_effort: None,
            cache_mode: CacheMode::Auto,
        }
    }

    #[test]
    fn direct_codex_mapping_uses_request_name() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".codex")).unwrap();
        std::fs::write(root.path().join(".codex/config.toml"), "").unwrap();
        let mut provider = provider("zen");
        provider.protocol = ProtocolKind::OpenAiResponses;
        provider.models = vec!["client-key".into()];
        provider.default_model = "client-key".into();
        provider.model_entries.insert(
            "client-key".into(),
            crate::domain::ModelEntry {
                client_name: "client-key".into(),
                display_name: "Client key".into(),
                request_name: "upstream-name".into(),
            },
        );
        let plan = CodexAdapter.build_plan(root.path(), &[&provider]).unwrap();
        assert!(plan.patches[0].after.contains("model = \"upstream-name\""));
    }

    #[test]
    fn catalog_models_include_all_codex_required_fields() {
        let catalog = codex_model_catalog(&[&provider("zen")]);
        let model = &catalog["models"][0];

        assert_eq!(model["slug"], json!("deepseek-v4-flash-free_zen"));
        assert_eq!(model["shell_type"], json!("shell_command"));
        assert!(model.get("priority").is_some());
        assert!(model.get("base_instructions").is_some());
        assert!(model.get("model_messages").is_some());
        for required in [
            "visibility",
            "supported_in_api",
            "context_window",
            "max_context_window",
            "output_limit",
            "default_reasoning_level",
            "supported_reasoning_levels",
            "default_reasoning_summary",
            "supports_reasoning_summaries",
            "additional_speed_tiers",
            "service_tiers",
            "availability_nux",
            "upgrade",
            "support_verbosity",
            "default_verbosity",
            "apply_patch_tool_type",
            "web_search_tool_type",
            "truncation_policy",
            "supports_parallel_tool_calls",
            "supports_image_detail_original",
            "effective_context_window_percent",
            "experimental_supported_tools",
            "input_modalities",
            "supports_search_tool",
            "use_responses_lite",
        ] {
            assert!(
                model.get(required).is_some(),
                "catalog model missing required field: {required}"
            );
        }
    }

    #[test]
    fn catalog_values_track_provider_profile_like_codex_config() {
        let mut base = provider("zen");
        base.context_window = 96_000;
        base.max_output_tokens = 16_384;

        let catalog = codex_model_catalog(&[&base]);
        let model = &catalog["models"][0];
        assert_eq!(model["slug"], json!("deepseek-v4-flash-free_zen"));
        assert_eq!(
            model["context_window"],
            json!(base.context_window.max(128_000)),
            "catalog context window must match the provider profile floor used by config.toml"
        );
        assert_eq!(
            model["max_context_window"],
            json!(base.context_window.max(128_000))
        );
        assert_eq!(
            model["output_limit"],
            json!(base.max_output_tokens.max(32_768)),
            "catalog output limit must match the provider profile floor used by config.toml"
        );

        let mut effort = provider("zen");
        effort.reasoning_effort = Some("high".to_string());
        let catalog = codex_model_catalog(&[&effort]);
        assert_eq!(
            catalog["models"][0]["default_reasoning_level"],
            json!("high"),
            "catalog reasoning level must mirror the config model_reasoning_effort fallback"
        );

        let catalog = codex_model_catalog(&[&provider("zen")]);
        assert_eq!(
            catalog["models"][0]["default_reasoning_level"],
            json!("medium"),
            "unset provider effort defaults to medium in both config and catalog"
        );
    }

    #[test]
    fn provider_config_uses_codex_0_147_timeout_and_retry_keys() {
        let config = merge_codex_config(
            "",
            &provider("zen"),
            "deepseek-v4-flash-free_zen",
            "xu_zen",
            "http://localhost/v1",
            "key",
            "responses",
        )
        .expect("merge codex config");
        let doc = config.parse::<DocumentMut>().expect("parse codex config");
        let managed = &doc["model_providers"]["xu_zen"];

        assert_eq!(
            managed["request_max_retries"].as_integer(),
            Some(3),
            "Codex 0.147 expects request_max_retries"
        );
        assert_eq!(
            managed["stream_max_retries"].as_integer(),
            Some(3),
            "Codex 0.147 expects stream_max_retries"
        );
        assert_eq!(
            managed["stream_idle_timeout_ms"].as_integer(),
            Some(60_000),
            "Codex 0.147 expects stream_idle_timeout_ms"
        );
        assert_eq!(
            managed["websocket_connect_timeout_ms"].as_integer(),
            Some(60_000),
            "Codex 0.147 expects websocket_connect_timeout_ms"
        );
        assert!(
            !config.contains("request_timeout_ms"),
            "request_timeout_ms is a dead key since Codex 0.146"
        );
        assert!(
            !config
                .lines()
                .any(|line| line.trim_start().starts_with("max_retries")),
            "bare max_retries is a dead key since Codex 0.146"
        );
    }
}
