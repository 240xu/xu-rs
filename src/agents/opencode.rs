use std::path::Path;

use serde_json::{json, Value};

use super::{ApplyPlan, FileAdapter};
use crate::domain::{AgentConfigSpec, AgentTarget, ProtocolKind, ProviderProfile, RoutingMode};
use crate::patch::{read_before_checked, ConfigPatch};

pub(super) struct OpenCodeAdapter;

fn ensure_opencode_provider(provider: &ProviderProfile) -> Result<(), String> {
    match provider.protocol {
        ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses => Ok(()),
        ProtocolKind::AnthropicMessages => Err(format!(
            "provider {} uses the Anthropic Messages API, which OpenCode does not commonly support. \
             Convert it to an OpenAI-compatible chat or Responses endpoint to use it with OpenCode",
            provider.id
        )),
    }
}

fn opencode_npm(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::OpenAiChat => "@ai-sdk/openai-compatible",
        ProtocolKind::OpenAiResponses => "@ai-sdk/openai",
        ProtocolKind::AnthropicMessages => unreachable!("checked by ensure_opencode_provider"),
    }
}

impl FileAdapter for OpenCodeAdapter {
    fn spec(&self, home: &Path) -> AgentConfigSpec {
        AgentConfigSpec {
            target: AgentTarget::OpenCode,
            config_files: vec![home.join(".config/opencode/opencode.json")],
            native_protocol: ProtocolKind::OpenAiChat,
            supports_direct_file: true,
            supports_local_routing: false,
        }
    }

    fn build_plan(&self, home: &Path, providers: &[&ProviderProfile]) -> Result<ApplyPlan, String> {
        for provider in providers {
            ensure_opencode_provider(provider)?;
        }

        let path = home.join(".config/opencode/opencode.json");
        let before = read_before_checked(&path)?;
        let mut doc: serde_json::Value = if before.trim().is_empty() {
            json!({ "$schema": "https://opencode.ai/config.json", "provider": {} })
        } else {
            serde_json::from_str(&before)
                .map_err(|error| format!("parse {}: {error}", path.display()))?
        };
        let root = doc
            .as_object_mut()
            .ok_or_else(|| format!("{} root must be an object", path.display()))?;
        root.entry("$schema".to_string())
            .or_insert_with(|| json!("https://opencode.ai/config.json"));
        let provider_map = root
            .entry("provider".to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| format!("{} provider must be an object", path.display()))?;

        for provider in providers {
            let mut options = serde_json::Map::new();
            options.insert("baseURL".to_string(), json!(provider.base_url));
            if !provider.api_key.trim().is_empty() {
                options.insert("apiKey".to_string(), json!(provider.api_key));
            }
            options.insert("timeout".to_string(), json!(provider.timeout_ms));
            options.insert("chunkTimeout".to_string(), json!(60000));
            options.insert("maxRetries".to_string(), json!(provider.max_retries));
            if !provider.extra_headers.is_empty() {
                options.insert("headers".to_string(), json!(provider.extra_headers));
            }

            let provider_models: serde_json::Map<String, serde_json::Value> = provider
                .models
                .iter()
                .map(|model| {
                    let mut model_doc = provider.model_metadata(model).cloned().unwrap_or_default();
                    model_doc
                        .entry("name".to_string())
                        .or_insert_with(|| json!(model));
                    model_doc.entry("limit".to_string()).or_insert_with(|| {
                        json!({
                            "context": provider.context_window,
                            "output": provider.max_output_tokens
                        })
                    });
                    if let Some(reasoning_effort) = &provider.reasoning_effort {
                        let options = model_doc
                            .entry("options".to_string())
                            .or_insert_with(|| json!({}));
                        if let Some(options) = options.as_object_mut() {
                            options
                                .entry("reasoningEffort".to_string())
                                .or_insert_with(|| json!(reasoning_effort));
                        }
                    }
                    (model.clone(), serde_json::Value::Object(model_doc))
                })
                .collect();

            let built = json!({
                "npm": opencode_npm(provider.protocol),
                "name": provider.name,
                "options": options,
                "models": provider_models
            });
            match provider_map.get_mut(&provider.id) {
                Some(existing) => merge_opencode_provider(existing, &built),
                None => {
                    provider_map.insert(provider.id.clone(), built);
                }
            }
        }

        let first = providers[0];
        let first_model = first.first_model()?;
        root.insert(
            "model".to_string(),
            json!(format!("{}/{}", first.id, first_model)),
        );
        let after = serde_json::to_string_pretty(&doc).map_err(|error| error.to_string())? + "\n";

        let summary = vec![
            "OpenCode: merges the selected provider into ~/.config/opencode/opencode.json"
                .to_string(),
            "OpenCode: preserves other providers and unknown top-level settings".to_string(),
            "OpenCode: no proxy, no request routing, no protocol bridge".to_string(),
            "OpenCode: uses the Chat Completions API via @ai-sdk/openai-compatible for chat providers \
             and the Responses API via @ai-sdk/openai for responses providers"
                .to_string(),
        ];

        Ok(ApplyPlan {
            target: AgentTarget::OpenCode,
            routing_mode: RoutingMode::DirectFile,
            protocol_adapter: None,
            patches: vec![ConfigPatch::new(path, before, after)],
            summary,
            warnings: Vec::new(),
        })
    }
}

/// Merge a profile-built provider entry into an existing one so opencode
/// runtime-managed settings survive a re-select. Adapter-managed fields
/// (`npm`, `name`, `options` keys, per-model `name`/`limit`) are overwritten;
/// anything the profile does not define is preserved.
fn merge_opencode_provider(existing: &mut Value, built: &Value) {
    let Some(existing) = existing.as_object_mut() else {
        return;
    };
    let Some(built) = built.as_object() else {
        return;
    };
    for key in ["npm", "name"] {
        if let Some(value) = built.get(key) {
            existing.insert(key.to_string(), value.clone());
        }
    }
    merge_json_object(
        existing
            .entry("options".to_string())
            .or_insert_with(|| json!({})),
        built.get("options").unwrap_or(&json!({})),
    );
    if let Some(built_models) = built.get("models").and_then(Value::as_object) {
        let models = existing
            .entry("models".to_string())
            .or_insert_with(|| json!({}));
        if let Some(models) = models.as_object_mut() {
            for (model, built_doc) in built_models {
                match models.get_mut(model) {
                    Some(existing_doc) => merge_opencode_model(existing_doc, built_doc),
                    None => {
                        models.insert(model.clone(), built_doc.clone());
                    }
                }
            }
        }
    }
}

fn merge_opencode_model(existing: &mut Value, built: &Value) {
    let Some(existing) = existing.as_object_mut() else {
        return;
    };
    let Some(built) = built.as_object() else {
        return;
    };
    for key in ["name", "limit"] {
        if let Some(value) = built.get(key) {
            existing.insert(key.to_string(), value.clone());
        }
    }
    merge_json_object(
        existing
            .entry("options".to_string())
            .or_insert_with(|| json!({})),
        built.get("options").unwrap_or(&json!({})),
    );
}

/// Merge `built` keys into `target`, keeping keys `built` does not define.
fn merge_json_object(target: &mut Value, built: &Value) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    let Some(built) = built.as_object() else {
        return;
    };
    for (key, value) in built {
        target.insert(key.clone(), value.clone());
    }
}
