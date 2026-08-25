use std::time::{Duration, Instant};

use crate::domain::{ProtocolKind, ProviderProfile};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCheckResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: Option<u128>,
}

pub fn test_provider_connection(provider: &ProviderProfile) -> ProviderCheckResult {
    if let Err(error) = provider.validate_for_write() {
        return ProviderCheckResult {
            ok: false,
            message: format!("本地配置无效：{error}"),
            latency_ms: None,
        };
    }

    if provider.protocol == ProtocolKind::AnthropicMessages {
        return ProviderCheckResult {
            ok: false,
            message: "Anthropic-compatible provider 暂不做自动探测，避免误调用计费接口。"
                .to_string(),
            latency_ms: None,
        };
    }

    let url = models_url(&provider.base_url);
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return ProviderCheckResult {
                ok: false,
                message: format!("创建 HTTP client 失败：{error}"),
                latency_ms: None,
            };
        }
    };

    let mut request = client.get(&url).header("accept", "application/json");
    if !provider.api_key.trim().is_empty() {
        request = request.bearer_auth(provider.api_key.trim());
    }
    for (key, value) in &provider.extra_headers {
        request = request.header(key, value);
    }

    let started = Instant::now();
    match request.send() {
        Ok(response) => {
            let status = response.status();
            let latency_ms = started.elapsed().as_millis();
            ProviderCheckResult {
                ok: status.is_success(),
                message: if status.is_success() {
                    format!("连接成功：GET /models -> {status}，{latency_ms} ms")
                } else {
                    format!("连接失败：GET /models -> {status}，{latency_ms} ms")
                },
                latency_ms: Some(latency_ms),
            }
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis();
            ProviderCheckResult {
                ok: false,
                message: format!("请求失败（{latency_ms} ms）：{error}"),
                latency_ms: Some(latency_ms),
            }
        }
    }
}

pub fn fetch_provider_models(provider: &ProviderProfile) -> Result<Vec<String>, String> {
    if let Err(error) = provider.validate_for_write() {
        return Err(format!("本地配置无效：{error}"));
    }
    if provider.protocol == ProtocolKind::AnthropicMessages {
        return Err(
            "Anthropic-compatible provider 暂不自动拉取模型，避免误调用计费接口。".to_string(),
        );
    }

    let url = models_url(&provider.base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("创建 HTTP client 失败：{e}"))?;
    let mut request = client.get(&url).header("accept", "application/json");
    if !provider.api_key.trim().is_empty() {
        request = request.bearer_auth(provider.api_key.trim());
    }
    for (key, value) in &provider.extra_headers {
        request = request.header(key, value);
    }
    let response = request.send().map_err(|e| format!("请求失败：{e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GET /models -> {status}"));
    }
    let value: serde_json::Value = response.json().map_err(|e| format!("解析响应失败：{e}"))?;
    let mut models = parse_models(&value);
    models.sort();
    models.dedup();
    if models.is_empty() {
        Err("/models 响应中没有找到模型 id".to_string())
    } else {
        Ok(models)
    }
}

fn parse_models(value: &serde_json::Value) -> Vec<String> {
    if let Some(items) = value.get("data").and_then(|v| v.as_array()) {
        return items
            .iter()
            .filter_map(model_id)
            .map(ToString::to_string)
            .collect();
    }
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .filter_map(model_id)
            .map(ToString::to_string)
            .collect();
    }
    if let Some(models) = value.get("models").and_then(|v| v.as_object()) {
        return models.keys().cloned().collect();
    }
    Vec::new()
}

fn model_id(value: &serde_json::Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("id").and_then(|v| v.as_str()))
        .or_else(|| value.get("name").and_then(|v| v.as_str()))
}

fn models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/models") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/models")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_models_url_from_v1_base() {
        assert_eq!(
            models_url("https://api.example.com/v1"),
            "https://api.example.com/v1/models"
        );
    }

    #[test]
    fn keeps_existing_models_url() {
        assert_eq!(
            models_url("https://api.example.com/v1/models"),
            "https://api.example.com/v1/models"
        );
    }

    #[test]
    fn parses_openai_models_response() {
        let value = serde_json::json!({"data":[{"id":"b"},{"id":"a"}]});

        assert_eq!(parse_models(&value), vec!["b", "a"]);
    }

    #[test]
    fn parses_model_arrays() {
        let value = serde_json::json!(["a", {"name":"b"}]);

        assert_eq!(parse_models(&value), vec!["a", "b"]);
    }
}
