use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::patch::{apply_patch, read_before_checked, ConfigPatch, PatchOptions};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenCodeSyncResult {
    pub imported: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub changed: bool,
}

pub fn sync_opencode_providers(home: &Path) -> Result<OpenCodeSyncResult, String> {
    sync_opencode_providers_with(home, false)
}

/// `dry_run` 只做解析与归一化，不写存储文件；用于 TUI/CLI 预览。
pub fn sync_opencode_providers_with(
    home: &Path,
    dry_run: bool,
) -> Result<OpenCodeSyncResult, String> {
    let opencode_path = home.join(".config/opencode/opencode.json");
    if !opencode_path.exists() {
        return Ok(OpenCodeSyncResult::default());
    }
    let opencode_text = fs::read_to_string(&opencode_path)
        .map_err(|error| format!("read {}: {error}", opencode_path.display()))?;
    let opencode: Value = serde_json::from_str(&opencode_text)
        .map_err(|error| format!("parse {}: {error}", opencode_path.display()))?;
    let live_providers = opencode
        .get("provider")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{} missing provider object", opencode_path.display()))?;

    let store_path = home.join(".codex/xu-chat-providers.json");
    let before = read_before_checked(&store_path)?;
    let mut store = if before.trim().is_empty() {
        serde_json::json!({ "provider": {} })
    } else {
        serde_json::from_str(&before)
            .map_err(|error| format!("parse {}: {error}", store_path.display()))?
    };
    let stored_providers = store
        .get_mut("provider")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("{} missing provider object", store_path.display()))?;
    let current_model = opencode.get("model").and_then(Value::as_str);
    let mut result = OpenCodeSyncResult::default();

    for (id, live) in live_providers {
        let Some(live_obj) = live.as_object() else {
            result.skipped += 1;
            continue;
        };
        let Some(kind) = api_kind_for_npm(live_obj.get("npm").and_then(Value::as_str)) else {
            result.skipped += 1;
            continue;
        };
        let normalized = normalize_provider(
            id,
            kind,
            live_obj,
            stored_providers.get(id).and_then(Value::as_object),
            current_model,
        )?;
        match stored_providers.get(id) {
            None => {
                stored_providers.insert(id.clone(), normalized);
                result.imported += 1;
            }
            Some(existing) if existing == &normalized => result.unchanged += 1,
            Some(_) => {
                stored_providers.insert(id.clone(), normalized);
                result.updated += 1;
            }
        }
    }

    result.changed = result.imported > 0 || result.updated > 0;
    if result.changed && !dry_run {
        let after = serde_json::to_string_pretty(&store).map_err(|error| error.to_string())? + "\n";
        apply_patch(
            &ConfigPatch::new(store_path, before, after),
            PatchOptions {
                dry_run: false,
                backup: true,
            },
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(result)
}

fn api_kind_for_npm(npm: Option<&str>) -> Option<&'static str> {
    match npm {
        Some("@ai-sdk/openai-compatible") => Some("chat"),
        Some("@ai-sdk/openai") => Some("responses"),
        Some("@ai-sdk/anthropic") => Some("anthropic"),
        _ => None,
    }
}

fn normalize_provider(
    id: &str,
    api_kind: &'static str,
    live: &Map<String, Value>,
    existing: Option<&Map<String, Value>>,
    current_model: Option<&str>,
) -> Result<Value, String> {
    let live_options = live
        .get("options")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("OpenCode provider {id} missing options"))?;
    let base_url = live_options
        .get("baseURL")
        .or_else(|| live_options.get("base_url"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("OpenCode provider {id} missing baseURL"))?;
    let models = live
        .get("models")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("OpenCode provider {id} missing models"))?;
    if models.is_empty() {
        return Err(format!("OpenCode provider {id} has no models"));
    }

    let mut merged = existing.cloned().unwrap_or_default();
    merged.insert("apiKind".to_string(), Value::String(api_kind.to_string()));
    merged.insert(
        "name".to_string(),
        live.get("name")
            .cloned()
            .unwrap_or_else(|| Value::String(id.to_string())),
    );
    merged.insert("models".to_string(), Value::Object(models.clone()));

    let mut options = existing
        .and_then(|value| value.get("options"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    options.insert("baseURL".to_string(), Value::String(base_url.to_string()));
    for key in ["apiKey", "timeout", "chunkTimeout", "maxRetries"] {
        if let Some(value) = live_options.get(key) {
            options.insert(key.to_string(), value.clone());
        }
    }
    if let Some(headers) = live_options
        .get("headers")
        .or_else(|| live_options.get("customHeaders"))
    {
        options.insert("customHeaders".to_string(), headers.clone());
    }
    if let Some((context, output)) = model_limits(models, selected_model(id, current_model)) {
        options.insert("contextWindow".to_string(), Value::from(context));
        options.insert("maxOutputTokens".to_string(), Value::from(output));
    }
    merged.insert("options".to_string(), Value::Object(options));

    let existing_default = existing
        .and_then(|value| value.get("defaultModel"))
        .and_then(Value::as_str)
        .filter(|model| models.contains_key(*model));
    let default_model = selected_model(id, current_model)
        .filter(|model| models.contains_key(*model))
        .or(existing_default)
        .or_else(|| models.keys().next().map(String::as_str))
        .expect("models is not empty");
    merged.insert(
        "defaultModel".to_string(),
        Value::String(default_model.to_string()),
    );
    Ok(Value::Object(merged))
}

fn selected_model<'a>(provider_id: &str, current_model: Option<&'a str>) -> Option<&'a str> {
    current_model.and_then(|value| value.strip_prefix(&format!("{provider_id}/")))
}

fn model_limits(models: &Map<String, Value>, preferred: Option<&str>) -> Option<(u64, u64)> {
    let model = preferred
        .and_then(|id| models.get(id))
        .or_else(|| models.values().next())?;
    let limit = model.get("limit")?.as_object()?;
    Some((
        limit.get("context")?.as_u64()?,
        limit.get("output")?.as_u64()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_live_provider_and_preserves_xu_metadata() {
        let home = tempfile::tempdir().unwrap();
        let opencode_path = home.path().join(".config/opencode/opencode.json");
        let store_path = home.path().join(".codex/xu-chat-providers.json");
        fs::create_dir_all(opencode_path.parent().unwrap()).unwrap();
        fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        fs::write(
            &store_path,
            r#"{"provider":{"live":{"name":"old","notes":"keep","options":{"baseURL":"https://old","apiKey":"old"},"models":{"old":{}}}}}"#,
        )
        .unwrap();
        fs::write(
            &opencode_path,
            r#"{"provider":{"live":{"npm":"@ai-sdk/openai-compatible","name":"Live","options":{"baseURL":"https://live/v1","apiKey":"new","timeout":90000},"models":{"m":{"limit":{"context":200000,"output":64000}}}},"plugin":{"npm":"other","options":{},"models":{"x":{}}}},"model":"live/m"}"#,
        )
        .unwrap();

        let result = sync_opencode_providers(home.path()).unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(store_path).unwrap()).unwrap();

        assert_eq!(result.updated, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(value["provider"]["live"]["notes"], "keep");
        assert_eq!(
            value["provider"]["live"]["options"]["baseURL"],
            "https://live/v1"
        );
        assert_eq!(value["provider"]["live"]["defaultModel"], "m");
        assert_eq!(
            value["provider"]["live"]["options"]["contextWindow"],
            200000
        );
        assert!(value["provider"].get("plugin").is_none());
    }

    #[test]
    fn second_sync_is_unchanged_and_creates_no_extra_backup() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".config/opencode/opencode.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"provider":{"p":{"npm":"@ai-sdk/openai-compatible","options":{"baseURL":"https://p/v1","apiKey":"key"},"models":{"m":{}}}}}"#).unwrap();

        assert!(sync_opencode_providers(home.path()).unwrap().changed);
        assert!(!sync_opencode_providers(home.path()).unwrap().changed);
    }

    #[test]
    fn imports_all_npm_kinds_and_skips_unknown() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".config/opencode/opencode.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"provider":{
              "chatp":{"npm":"@ai-sdk/openai-compatible","options":{"baseURL":"https://chatp/v1"},"models":{"a":{}}},
              "respp":{"npm":"@ai-sdk/openai","options":{"baseURL":"https://respp/v1"},"models":{"b":{}}},
              "anthp":{"npm":"@ai-sdk/anthropic","options":{"baseURL":"https://anthp/v1"},"models":{"c":{}}},
              "plugin":{"npm":"other","options":{"baseURL":"https://x"},"models":{"d":{}}}
            }}"#,
        )
        .unwrap();

        let result = sync_opencode_providers(home.path()).unwrap();
        assert_eq!(result.imported, 3);
        assert_eq!(result.skipped, 1);
        let store: Value = serde_json::from_str(
            &fs::read_to_string(home.path().join(".codex/xu-chat-providers.json")).unwrap(),
        )
        .unwrap();
        let provider = &store["provider"];
        assert_eq!(provider["chatp"]["apiKind"], "chat");
        assert_eq!(provider["respp"]["apiKind"], "responses");
        assert_eq!(provider["anthp"]["apiKind"], "anthropic");
        assert!(provider.get("plugin").is_none());
    }

    #[test]
    fn dry_run_preview_does_not_write_store() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".config/opencode/opencode.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"provider":{"p":{"npm":"@ai-sdk/openai","options":{"baseURL":"https://p/v1"},"models":{"m":{}}}}}"#,
        )
        .unwrap();

        let result = sync_opencode_providers_with(home.path(), true).unwrap();
        assert_eq!(result.imported, 1);
        assert!(result.changed);
        assert!(
            !home.path().join(".codex/xu-chat-providers.json").exists(),
            "dry run must not create the store"
        );
    }
}
