use trivium::provider_store::profiles_from_xu_chat_json;

#[test]
fn reads_existing_xu_chat_provider_shape() {
    let text = r#"
    {
      "provider": {
        "zen": {
          "apiKind": "chat",
          "name": "OpenCode Zen (free)",
          "options": {
            "baseURL": "https://opencode.ai/zen/v1",
            "apiKey": "dummy",
            "timeout": 60000,
            "maxRetries": 10,
            "maxOutputTokens": 32768
          },
          "models": {
            "deepseek-v4-flash-free": { "name": "DeepSeek", "limit": "free" }
          }
        }
      }
    }
    "#;

    let profiles = profiles_from_xu_chat_json(text).unwrap();

    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].id, "zen");
    assert_eq!(profiles[0].protocol.as_str(), "openai-chat");
    assert_eq!(profiles[0].base_url, "https://opencode.ai/zen/v1");
    assert_eq!(profiles[0].api_key, "dummy");
    assert_eq!(profiles[0].default_model, "deepseek-v4-flash-free");
    assert_eq!(profiles[0].models, vec!["deepseek-v4-flash-free"]);
    assert_eq!(
        profiles[0].model_metadata["deepseek-v4-flash-free"]["limit"],
        "free"
    );
}

#[test]
fn preserves_rich_model_metadata() {
    let text = r#"{
      "provider": {
        "rich": {
          "apiKind": "chat",
          "options": { "baseURL": "https://example.com/v1", "apiKey": "key" },
          "models": {
            "reasoner": {
              "limit": { "context": 200000, "output": 64000 },
              "modalities": { "input": ["text", "image"], "output": ["text"] },
              "variants": { "high": { "reasoningEffort": "high" } }
            }
          }
        }
      }
    }"#;

    let profiles = profiles_from_xu_chat_json(text).unwrap();
    let metadata = &profiles[0].model_metadata["reasoner"];

    assert_eq!(metadata["limit"]["context"], 200000);
    assert_eq!(metadata["modalities"]["input"][1], "image");
    assert_eq!(metadata["variants"]["high"]["reasoningEffort"], "high");
}

#[test]
fn reads_legacy_array_provider_shape() {
    let text = r#"
    [{
      "providerName": "Anthropic",
      "providerId": "anth",
      "apiKey": "key",
      "baseUrl": "https://api.anthropic.com",
      "apiKind": "anthropic",
      "models": ["claude-3-5-sonnet-latest"],
      "context": 200000,
      "output": 8192
    }]
    "#;

    let profiles = profiles_from_xu_chat_json(text).unwrap();

    assert_eq!(profiles[0].id, "anth");
    assert_eq!(profiles[0].protocol.as_str(), "anthropic-messages");
    assert_eq!(profiles[0].context_window, 200000);
    assert_eq!(profiles[0].max_output_tokens, 8192);
}

#[test]
fn rejects_unknown_api_kind_in_current_provider_shape() {
    let text = r#"
    {
      "provider": {
        "bad": {
          "apiKind": "openai-response-typo",
          "options": { "baseURL": "https://example.com/v1", "apiKey": "key" },
          "models": { "m": {} }
        }
      }
    }
    "#;

    let err = profiles_from_xu_chat_json(text).unwrap_err();

    assert!(err.contains("apiKind") || err.contains("protocol"));
}

#[test]
fn rejects_unknown_api_kind_in_legacy_array_shape() {
    let text = r#"
    [{
      "providerName": "Bad",
      "providerId": "bad",
      "apiKey": "key",
      "baseUrl": "https://example.com/v1",
      "apiKind": "anthropic-message-typo",
      "models": ["m"]
    }]
    "#;

    let err = profiles_from_xu_chat_json(text).unwrap_err();

    assert!(err.contains("apiKind") || err.contains("protocol"));
}
