use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::domain::{CacheMode, ModelEntry, ProtocolKind, ProviderProfile, ProviderVendor};

pub fn read_profiles(path: &Path) -> Result<Vec<ProviderProfile>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    profiles_from_xu_chat_json(&text)
}

pub fn profiles_from_xu_chat_json(text: &str) -> Result<Vec<ProviderProfile>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;

    if let Some(array) = root.as_array() {
        return profiles_from_legacy_array(array);
    }

    let providers = root
        .get("provider")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "missing provider object".to_string())?;

    let mut out = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for (id, value) in providers {
        let obj = match value.as_object() {
            Some(o) => o,
            None => {
                warnings.push(format!("provider {id} must be an object"));
                continue;
            }
        };
        let options = match obj.get("options").and_then(|v| v.as_object()) {
            Some(o) => o,
            None => {
                warnings.push(format!("provider {id} missing options"));
                continue;
            }
        };
        let models_value = match obj.get("models") {
            Some(v) => v,
            None => {
                warnings.push(format!("provider {id} missing models"));
                continue;
            }
        };
        let (models, model_entries, model_metadata) = match models_value {
            Value::Array(items) => match model_entries_from_array(items) {
                Ok(v) => v,
                Err(e) => {
                    warnings.push(format!("provider {id}: {e}"));
                    continue;
                }
            },
            Value::Object(map) => {
                let models: Vec<String> = map.keys().cloned().collect();
                let model_metadata = map
                    .iter()
                    .map(|(id, value)| (id.clone(), value.clone()))
                    .collect();
                let model_entries = map
                    .iter()
                    .map(|(key, value)| {
                        let (name, request_name) = entry_names(value);
                        let client_name = key.clone();
                        let display_name = name
                            .or_else(|| request_name.clone())
                            .unwrap_or_else(|| client_name.clone());
                        let request_name = request_name.unwrap_or_else(|| client_name.clone());
                        (
                            client_name.clone(),
                            ModelEntry {
                                client_name,
                                display_name,
                                request_name,
                            },
                        )
                    })
                    .collect();
                (models, model_entries, model_metadata)
            }
            _ => {
                warnings.push(format!("provider {id} models must be an object or array"));
                continue;
            }
        };
        let mut claude_slots = claude_slots_from_metadata(&model_metadata);
        if let Some(slots) = obj.get("claudeSlots").and_then(Value::as_object) {
            for (slot, model) in slots {
                if let Some(model) = model.as_str() {
                    claude_slots.insert(slot.clone(), model.to_string());
                }
            }
        }
        if models.is_empty() {
            warnings.push(format!("provider {id} has no models"));
            continue;
        }

        let protocol_text = text_field(obj, "protocol")
            .or_else(|| text_field(obj, "apiKind"))
            .or_else(|| text_field_value(options, "protocol"))
            .or_else(|| text_field_value(options, "apiKind"))
            .unwrap_or_else(|| "chat".to_string());
        let protocol = match ProtocolKind::parse_legacy(&protocol_text) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("provider {id} {e}"));
                continue;
            }
        };
        let base_url = match text_field_value(options, "baseURL")
            .or_else(|| text_field_value(options, "base_url"))
        {
            Some(v) => v,
            None => {
                warnings.push(format!("provider {id} missing baseURL"));
                continue;
            }
        };
        let api_key = text_field_value(options, "apiKey")
            .or_else(|| text_field_value(options, "api_key"))
            .unwrap_or_default();
        let extra_headers = headers(options);
        let vendor = text_field(obj, "vendor")
            .map(|v| vendor_from_str(&v, id, &base_url, protocol))
            .unwrap_or_else(|| ProviderVendor::infer(id, &base_url, protocol));

        out.push(ProviderProfile {
            id: id.clone(),
            name: text_field(obj, "name").unwrap_or_else(|| id.clone()),
            notes: text_field(obj, "notes"),
            website: text_field(obj, "website"),
            vendor,
            protocol,
            base_url,
            api_key,
            default_model: text_field(obj, "defaultModel").unwrap_or_else(|| models[0].clone()),
            models,
            model_entries,
            model_metadata,
            claude_slots,
            extra_headers,
            request_url_mode: text_field_value(options, "requestUrlMode"),
            header_mode: text_field_value(options, "headerMode"),
            timeout_ms: number_field(options, "timeout").unwrap_or(60000),
            max_retries: number_field(options, "maxRetries").unwrap_or(10) as u32,
            context_window: number_field(options, "contextWindow")
                .or_else(|| number_field(options, "context"))
                .unwrap_or(128000) as u32,
            max_output_tokens: number_field(options, "maxOutputTokens").unwrap_or(32768) as u32,
            reasoning_effort: text_field_value(options, "reasoningEffort"),
            cache_mode: text_field(obj, "cacheMode")
                .or_else(|| text_field_value(options, "cacheMode"))
                .map(|v| CacheMode::parse_legacy(&v))
                .unwrap_or_default(),
        });
    }
    if out.is_empty() && !warnings.is_empty() {
        return Err(warnings.join("; "));
    }
    for w in &warnings {
        eprintln!("[warn] {w}");
    }
    Ok(out)
}

fn profiles_from_legacy_array(array: &[Value]) -> Result<Vec<ProviderProfile>, String> {
    let mut out = Vec::new();
    for value in array {
        let obj = value
            .as_object()
            .ok_or_else(|| "legacy provider entry must be an object".to_string())?;
        let id = text_field(obj, "providerId")
            .or_else(|| text_field(obj, "id"))
            .ok_or_else(|| "legacy provider missing providerId".to_string())?;
        let base_url = text_field(obj, "baseUrl")
            .or_else(|| text_field(obj, "baseURL"))
            .ok_or_else(|| format!("provider {id} missing baseUrl"))?;
        let protocol_text = text_field(obj, "apiKind").unwrap_or_else(|| "chat".to_string());
        let protocol =
            ProtocolKind::parse_legacy(&protocol_text).map_err(|e| format!("provider {id} {e}"))?;
        let (models, model_entries, model_metadata) = obj
            .get("models")
            .and_then(|v| v.as_array())
            .map(|items| model_entries_from_array(items))
            .transpose()?
            .unwrap_or_else(|| (Vec::new(), BTreeMap::new(), BTreeMap::new()));
        let mut claude_slots = claude_slots_from_metadata(&model_metadata);
        if let Some(slots) = obj.get("claudeSlots").and_then(Value::as_object) {
            for (slot, model) in slots {
                if let Some(model) = model.as_str() {
                    claude_slots.insert(slot.clone(), model.to_string());
                }
            }
        }
        if models.is_empty() {
            return Err(format!("provider {id} has no models"));
        }

        out.push(ProviderProfile {
            id: id.clone(),
            name: text_field(obj, "providerName").unwrap_or_else(|| id.clone()),
            notes: text_field(obj, "notes"),
            website: text_field(obj, "website"),
            vendor: ProviderVendor::infer(&id, &base_url, protocol),
            protocol,
            base_url,
            api_key: text_field(obj, "apiKey").unwrap_or_default(),
            models: models.clone(),
            model_entries,
            model_metadata,
            claude_slots,
            default_model: models[0].clone(),
            extra_headers: BTreeMap::new(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: number_field(obj, "timeout").unwrap_or(60000),
            max_retries: number_field(obj, "retries").unwrap_or(10) as u32,
            context_window: number_field(obj, "context").unwrap_or(128000) as u32,
            max_output_tokens: number_field(obj, "output").unwrap_or(32768) as u32,
            reasoning_effort: text_field(obj, "reasoning"),
            cache_mode: text_field(obj, "cacheMode")
                .map(|v| CacheMode::parse_legacy(&v))
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

fn entry_names(value: &Value) -> (Option<String>, Option<String>) {
    match value {
        Value::Object(map) => (
            map.get("name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            map.get("requestName")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
        ),
        _ => (None, None),
    }
}

fn claude_slots_from_metadata(metadata: &BTreeMap<String, Value>) -> BTreeMap<String, String> {
    let mut slots = BTreeMap::new();
    let mut insert_slots = |value: &Value| {
        if let Some(map) = value.as_object() {
            for (slot, model) in map {
                if let Some(model) = model.as_str() {
                    slots.insert(slot.clone(), model.to_string());
                }
            }
        }
    };
    if let Some(value) = metadata.get("claudeSlots") {
        insert_slots(value);
    }
    for value in metadata.values() {
        if let Some(value) = value.get("claudeSlots") {
            insert_slots(value);
        }
    }
    slots
}

type ParsedModels = (
    Vec<String>,
    BTreeMap<String, ModelEntry>,
    BTreeMap<String, Value>,
);

/// Parse an array of model entries: strings are identity entries
/// (display name = request name = the string); objects are
/// `{"name": .., "requestName": ..}` with `name` defaulting to
/// `requestName` and vice versa. Object metadata is preserved verbatim.
fn model_entries_from_array(items: &[Value]) -> Result<ParsedModels, String> {
    let mut models = Vec::new();
    let mut entries = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    for item in items {
        match item {
            Value::String(name) => {
                let client_name = name.clone();
                entries.insert(
                    client_name.clone(),
                    ModelEntry {
                        client_name: client_name.clone(),
                        display_name: client_name.clone(),
                        request_name: client_name.clone(),
                    },
                );
                models.push(client_name);
            }
            Value::Object(_) => {
                let (name, request_name) = entry_names(item);
                let client_name = item
                    .get("clientName")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(ToString::to_string)
                    .or_else(|| name.clone())
                    .or_else(|| request_name.clone())
                    .ok_or_else(|| "model entry object requires name or requestName".to_string())?;
                let display_name = name
                    .clone()
                    .or_else(|| request_name.clone())
                    .unwrap_or_else(|| client_name.clone());
                let request_name = request_name
                    .clone()
                    .or_else(|| name.clone())
                    .unwrap_or_else(|| client_name.clone());
                metadata.insert(client_name.clone(), item.clone());
                entries.insert(
                    client_name.clone(),
                    ModelEntry {
                        client_name: client_name.clone(),
                        display_name,
                        request_name,
                    },
                );
                models.push(client_name);
            }
            _ => return Err("model entry must be a string or object".to_string()),
        }
    }
    Ok((models, entries, metadata))
}

fn text_field(map: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}

fn text_field_value(map: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}

fn number_field(map: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    map.get(key).and_then(|v| v.as_u64())
}

fn headers(map: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    map.get("customHeaders")
        .or_else(|| map.get("headers"))
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn vendor_from_str(
    value: &str,
    id: &str,
    base_url: &str,
    protocol: ProtocolKind,
) -> ProviderVendor {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" => ProviderVendor::OpenAi,
        "anthropic" => ProviderVendor::Anthropic,
        "openrouter" => ProviderVendor::OpenRouter,
        "gemini" | "google" => ProviderVendor::Gemini,
        "deepseek" => ProviderVendor::DeepSeek,
        "moonshot" | "kimi" => ProviderVendor::Moonshot,
        "openai-compatible" | "custom-openai-compatible" => ProviderVendor::CustomOpenAiCompatible,
        "anthropic-compatible" | "custom-anthropic-compatible" => {
            ProviderVendor::CustomAnthropicCompatible
        }
        _ => ProviderVendor::infer(id, base_url, protocol),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_cache_mode_is_also_read() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","name":"zen","options":{"cacheMode":"compat","baseURL":"https://x/v1"},"models":{"m":{}}}}}"#;
        let profiles = super::profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].cache_mode, crate::domain::CacheMode::Compat);
    }

    fn new_format(cache_mode: Option<&str>) -> String {
        let extra = cache_mode
            .map(|v| format!(",\"cacheMode\":\"{v}\""))
            .unwrap_or_default();
        format!(
            r#"{{
                "provider": {{
                    "zen": {{
                        "name": "Zen",
                        "options": {{ "baseURL": "https://zen.example/v1" }},
                        "models": {{ "deepseek-v4-flash-free": {{}} }}
                        {extra}
                    }}
                }}
            }}"#
        )
    }

    fn legacy_format(cache_mode: Option<&str>) -> String {
        let extra = cache_mode
            .map(|v| format!(",\"cacheMode\":\"{v}\""))
            .unwrap_or_default();
        format!(
            r#"[
                {{
                    "providerId": "zen",
                    "baseUrl": "https://zen.example/v1",
                    "models": ["deepseek-v4-flash-free"]
                    {extra}
                }}
            ]"#
        )
    }

    fn assert_cache_mode(text: &str, expected: CacheMode) {
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].cache_mode, expected);
    }

    #[test]
    fn new_format_defaults_to_auto() {
        assert_cache_mode(&new_format(None), CacheMode::Auto);
    }

    #[test]
    fn new_format_parses_deepseek_and_compat() {
        assert_cache_mode(&new_format(Some("deepseek")), CacheMode::DeepSeek);
        assert_cache_mode(&new_format(Some("compat")), CacheMode::Compat);
    }

    #[test]
    fn new_format_falls_back_to_auto_on_unknown_value() {
        assert_cache_mode(&new_format(Some("weird")), CacheMode::Auto);
    }

    #[test]
    fn legacy_array_parses_cache_mode() {
        assert_cache_mode(&legacy_format(None), CacheMode::Auto);
        assert_cache_mode(&legacy_format(Some("deepseek")), CacheMode::DeepSeek);
        assert_cache_mode(&legacy_format(Some("compat")), CacheMode::Compat);
        assert_cache_mode(&legacy_format(Some("weird")), CacheMode::Auto);
    }

    #[test]
    fn effective_mode_auto_detects_deepseek_by_vendor_and_model() {
        use crate::domain::ProviderVendor;
        // 显式 DeepSeek 恒为 DeepSeek。
        assert_eq!(
            CacheMode::DeepSeek.effective(ProviderVendor::CustomOpenAiCompatible, "gpt-5"),
            CacheMode::DeepSeek
        );
        // Auto + deepseek vendor → DeepSeek。
        assert_eq!(
            CacheMode::Auto.effective(ProviderVendor::DeepSeek, "gpt-5"),
            CacheMode::DeepSeek
        );
        // Auto + 模型名含 deepseek（兼容层转发也识别）→ DeepSeek。
        assert_eq!(
            CacheMode::Auto.effective(ProviderVendor::CustomOpenAiCompatible, "deepseek-v4-flash"),
            CacheMode::DeepSeek
        );
        // Auto + 无关 vendor/模型 → Auto。
        assert_eq!(
            CacheMode::Auto.effective(ProviderVendor::CustomOpenAiCompatible, "claude-sonnet-5"),
            CacheMode::Auto
        );
        // Compat 显式保留，不做 DeepSeek 变换。
        assert_eq!(
            CacheMode::Compat.effective(ProviderVendor::DeepSeek, "deepseek-v4-flash"),
            CacheMode::Compat
        );
    }

    #[test]
    fn map_entry_with_name_and_request_name_parses_alias() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://x/v1"},"models":{"ds-v4":{"name":"DeepSeek V4","requestName":"deepseek-v4-flash-free","limit":"free"}}}}}"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].models, vec!["ds-v4"]);
        assert_eq!(
            profiles[0].request_name_for("ds-v4"),
            "deepseek-v4-flash-free"
        );
        assert_eq!(profiles[0].display_name_for("ds-v4"), "DeepSeek V4");
        assert_eq!(profiles[0].model_metadata["ds-v4"]["limit"], "free");
        assert_eq!(profiles[0].model_metadata["ds-v4"]["name"], "DeepSeek V4");
    }

    #[test]
    fn map_entry_name_only_keeps_request_name_on_key() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://x/v1"},"models":{"deepseek-v4-flash-free":{"name":"DeepSeek V4 Flash (free)"}}}}}"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(
            profiles[0].request_name_for("deepseek-v4-flash-free"),
            "deepseek-v4-flash-free"
        );
        assert_eq!(
            profiles[0].display_name_for("deepseek-v4-flash-free"),
            "DeepSeek V4 Flash (free)"
        );
    }

    #[test]
    fn map_entry_request_name_only_defaults_display_to_request_name() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://x/v1"},"models":{"ds-v4":{"requestName":"deepseek-v4-flash-free"}}}}}"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(
            profiles[0].request_name_for("ds-v4"),
            "deepseek-v4-flash-free"
        );
        assert_eq!(
            profiles[0].display_name_for("ds-v4"),
            "deepseek-v4-flash-free"
        );
    }

    #[test]
    fn root_claude_slots_are_loaded_and_win_over_legacy_metadata() {
        let text = r#"{
          "provider": {"zen": {
            "options": {"baseURL":"https://zen.example/v1"},
            "models": {"m": {"claudeSlots":{"opus":"legacy", "sonnet":"legacy-sonnet"}}},
            "claudeSlots": {"opus":"root", "haiku":""}
          }}
        }"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].claude_slots["opus"], "root");
        assert_eq!(profiles[0].claude_slots["sonnet"], "legacy-sonnet");
        assert_eq!(profiles[0].claude_slots["haiku"], "");
    }

    #[test]
    fn legacy_claude_slots_are_loaded_when_root_slot_is_omitted() {
        let text = r#"{
          "provider": {"zen": {
            "options": {"baseURL":"https://zen.example/v1"},
            "models": {"m": {"claudeSlots":{"opus":"legacy"}}}
          }}
        }"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].claude_slot("opus"), Some("legacy"));
    }

    #[test]
    fn array_entries_use_non_empty_client_name_as_local_key() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://x/v1"},"models":[{"clientName":"client-key","name":"Client label","requestName":"upstream-name"}]}}}"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].models, vec!["client-key"]);
        assert_eq!(profiles[0].request_name_for("client-key"), "upstream-name");
        assert_eq!(profiles[0].display_name_for("client-key"), "Client label");
        assert!(profiles[0].model_metadata["client-key"]["clientName"].is_string());
    }

    #[test]
    fn array_entries_support_strings_and_objects() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://x/v1"},"models":["plain-model",{"name":"DeepSeek V4","requestName":"deepseek-v4-flash-free"}]}}}"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].models, vec!["plain-model", "DeepSeek V4"]);
        assert_eq!(profiles[0].request_name_for("plain-model"), "plain-model");
        assert_eq!(profiles[0].display_name_for("plain-model"), "plain-model");
        assert_eq!(
            profiles[0].request_name_for("DeepSeek V4"),
            "deepseek-v4-flash-free"
        );
        assert_eq!(profiles[0].display_name_for("DeepSeek V4"), "DeepSeek V4");
    }

    #[test]
    fn array_object_entry_defaults_sibling_field() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://x/v1"},"models":[{"name":"Only Name"},{"requestName":"only-request"}]}}}"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].request_name_for("Only Name"), "Only Name");
        assert_eq!(profiles[0].display_name_for("only-request"), "only-request");
    }

    #[test]
    fn array_object_entry_without_name_or_request_name_rejected() {
        let text = r#"{"provider":{"zen":{"apiKind":"chat","options":{"baseURL":"https://x/v1"},"models":[{"limit":"free"}]}}}"#;
        let err = profiles_from_xu_chat_json(text).unwrap_err();
        assert!(err.contains("name or requestName"), "got: {err}");
    }

    #[test]
    fn legacy_array_parses_object_entries() {
        let text = r#"[{"providerId":"zen","baseUrl":"https://x/v1","models":["a",{"name":"B","requestName":"b"}]}]"#;
        let profiles = profiles_from_xu_chat_json(text).unwrap();
        assert_eq!(profiles[0].models, vec!["a", "B"]);
        assert_eq!(profiles[0].request_name_for("B"), "b");
    }
}
