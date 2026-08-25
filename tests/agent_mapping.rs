use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use spec::adapters::{apply_agent, AgentTarget, ApplyPlan, RoutingMode};
use spec::models::{ApiKind, CacheMode, ProviderProfile, ProviderVendor};
use spec::patch::{apply_patch, restore_backup, PatchOptions};

fn provider(id: &str, api_kind: ApiKind, model: &str) -> ProviderProfile {
    ProviderProfile {
        id: id.to_string(),
        name: id.to_string(),
        notes: None,
        website: None,
        vendor: ProviderVendor::CustomOpenAiCompatible,
        protocol: api_kind,
        base_url: format!("https://{id}.example/v1"),
        api_key: format!("{id}-key"),
        models: vec![model.to_string()],
        model_entries: Default::default(),
        model_metadata: Default::default(),
        claude_slots: Default::default(),
        default_model: model.to_string(),
        extra_headers: Default::default(),
        request_url_mode: None,
        header_mode: None,
        timeout_ms: 60000,
        max_retries: 10,
        context_window: 128000,
        max_output_tokens: 32768,
        reasoning_effort: Some("xhigh".to_string()),
        cache_mode: CacheMode::Auto,
    }
}

#[test]
fn opencode_writes_direct_config_without_proxy_route() {
    let root = tempfile::tempdir().unwrap();
    let p = provider("zen", ApiKind::Chat, "deepseek-v4-flash-free");

    let plan = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();

    assert_eq!(plan.routing_mode.as_str(), "direct-file");
    assert_eq!(plan.routing_mode, RoutingMode::DirectFile);
    assert_eq!(plan.patches.len(), 1);
    assert!(plan.patches[0]
        .path
        .ends_with(".config/opencode/opencode.json"));
    assert!(
        plan.patches[0].after.contains("opencode.ai")
            || plan.patches[0].after.contains("zen.example")
    );
    assert!(!plan.patches[0].after.contains("127.0.0.1"));
    assert!(!plan.patches[0].after.contains("/v1/chat/completions"));
    let json: serde_json::Value = serde_json::from_str(&plan.patches[0].after).unwrap();
    assert_eq!(json["model"], "zen/deepseek-v4-flash-free");
    assert_eq!(json["provider"]["zen"]["npm"], "@ai-sdk/openai-compatible");
    assert!(json["provider"]["zen"]["models"]["deepseek-v4-flash-free"].is_object());
    assert!(json.get("xuCodex").is_none());
}

#[test]
fn opencode_preserves_model_metadata_in_generated_config() {
    let root = tempfile::tempdir().unwrap();
    let mut p = provider("rich", ApiKind::Chat, "reasoner");
    p.model_metadata.insert(
        "reasoner".to_string(),
        serde_json::json!({
            "limit": { "context": 200000, "output": 64000 },
            "modalities": { "input": ["text", "image"], "output": ["text"] },
            "variants": { "high": { "reasoningEffort": "high" } }
        }),
    );

    let plan = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_str(&plan.patches[0].after).unwrap();
    let model = &json["provider"]["rich"]["models"]["reasoner"];

    assert_eq!(model["limit"]["context"], 200000);
    assert_eq!(model["modalities"]["input"][1], "image");
    assert_eq!(model["variants"]["high"]["reasoningEffort"], "high");
}

#[test]
fn opencode_merge_preserves_existing_settings_and_other_providers() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(".config/opencode/opencode.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{
          "permission": "allow",
          "theme": "custom",
          "provider": {
            "other": { "npm": "custom-plugin", "options": { "keep": true } },
            "zen": { "npm": "@ai-sdk/openai-compatible", "options": { "baseURL": "https://old" }, "models": { "old": {} } }
          },
          "agent": { "build": { "steps": 999 } }
        }"#,
    )
    .unwrap();
    let p = provider("zen", ApiKind::Chat, "new-model");

    let plan = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&plan.patches[0].after).unwrap();

    assert_eq!(value["permission"], "allow");
    assert_eq!(value["theme"], "custom");
    assert_eq!(value["agent"]["build"]["steps"], 999);
    assert_eq!(value["provider"]["other"]["npm"], "custom-plugin");
    assert!(value["provider"]["zen"]["models"]["new-model"].is_object());
    assert_eq!(value["model"], "zen/new-model");
}

#[test]
fn opencode_reselect_preserves_runtime_model_options_and_unknown_models() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(".config/opencode/opencode.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{
          "provider": {
            "zen": {
              "npm": "@ai-sdk/openai-compatible",
              "options": { "baseURL": "https://old.example/v1", "apiKey": "old-key" },
              "models": {
                "deepseek-v4-flash-free": {
                  "name": "DeepSeek V4 Flash Free",
                  "options": { "reasoningEffort": "high" },
                  "limit": { "context": 99999, "output": 9999 }
                },
                "runtime-only-model": { "name": "Runtime added" }
              }
            }
          }
        }"#,
    )
    .unwrap();
    let mut p = provider("zen", ApiKind::Chat, "deepseek-v4-flash-free");
    p.reasoning_effort = None;

    let plan = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&plan.patches[0].after).unwrap();
    let zen = &value["provider"]["zen"];

    assert_eq!(
        zen["options"]["baseURL"], p.base_url,
        "profile base URL wins on re-select"
    );
    assert!(
        zen["models"]["runtime-only-model"].is_object(),
        "runtime-added model must survive a re-select"
    );
    assert_eq!(
        zen["models"]["deepseek-v4-flash-free"]["options"]["reasoningEffort"], "high",
        "runtime model-level options must survive a re-select"
    );
}

#[test]
fn claude_plan_never_patches_dot_claude_json_state_and_preserves_unknown_settings() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join(".claude.json"),
        r#"{"numStartups":1,"hasShownWelcome":true}"#,
    )
    .unwrap();
    let settings = root.path().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        r#"{"theme":"custom-dark","permissions":{"allow":["Bash"]}}"#,
    )
    .unwrap();
    let p = provider("zen", ApiKind::Chat, "deepseek-v4-flash-free");

    let plan = apply_agent(
        root.path(),
        AgentTarget::ClaudeCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();

    assert!(!plan.patches.is_empty());
    for patch in &plan.patches {
        let path = patch.path.to_string_lossy();
        assert!(
            path.ends_with(".claude/settings.json"),
            "unexpected patch target: {path}"
        );
        assert!(
            !path.ends_with(".claude.json"),
            "Claude plan must never patch .claude.json state: {path}"
        );
    }
    let value: serde_json::Value = serde_json::from_str(&plan.patches[0].after).unwrap();
    assert_eq!(value["theme"], "custom-dark");
}

#[test]
fn claude_merge_preserves_user_settings_and_does_not_force_theme_or_auth_token() {
    let root = tempfile::tempdir().unwrap();
    let provider = provider("zen", ApiKind::Chat, "model");
    let path = root.path().join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"hooks":{"UserPromptSubmit":[]},"permissions":{"allow":["Bash"]},"theme":"light","env":{"CUSTOM":"keep"}}"#,
    )
    .unwrap();

    let plan = spec::agents::apply_agent(
        root.path(),
        AgentTarget::ClaudeCode,
        &[provider],
        &["zen".to_string()],
    )
    .unwrap();
    let patch = plan
        .patches
        .iter()
        .find(|patch| patch.path.ends_with("settings.json"))
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&patch.after).unwrap();
    assert_eq!(value["theme"], "light");
    assert!(value["hooks"].is_object());
    assert_eq!(value["env"]["CUSTOM"], "keep");
    assert_eq!(
        value["env"]["ANTHROPIC_AUTH_TOKEN"], "local-proxy",
        "auth token placeholder lets Claude Code skip its login gate"
    );
    assert!(value["env"].get("ANTHROPIC_API_KEY").is_none());
    assert_eq!(
        value["availableModels"],
        json!(["model_zen", "model_zen[1m]"]),
        "availableModels must list only the user model and its 1M variant"
    );
    assert_eq!(value["enforceAvailableModels"], true);
}

#[test]
fn codex_merge_preserves_user_tables_and_does_not_set_security_policy() {
    let root = tempfile::tempdir().unwrap();
    let provider = provider("zen", ApiKind::Responses, "model");
    let path = root.path().join(".codex/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"model = "old"

sandbox_mode = "workspace-write"

[mcp_servers.keep]
command = "keep"

[projects."/tmp/project"]
trust_level = "untrusted"
"#,
    )
    .unwrap();
    let plan = spec::agents::apply_agent(
        root.path(),
        AgentTarget::Codex,
        &[provider],
        &["zen".to_string()],
    )
    .unwrap();
    let patch = plan
        .patches
        .iter()
        .find(|patch| patch.path.ends_with("config.toml"))
        .unwrap();
    assert!(patch.after.contains("[mcp_servers.keep]"));
    assert!(patch.after.contains("sandbox_mode = \"workspace-write\""));
    assert!(patch.after.contains("trust_level = \"untrusted\""));
    assert!(!patch.after.contains("danger-full-access"));
    assert!(!patch.after.contains("approval_policy = \"never\""));
}

#[test]
fn opencode_rejects_anthropic_provider_as_uncommon_protocol() {
    let root = tempfile::tempdir().unwrap();
    let mut p = provider("anth", ApiKind::Anthropic, "claude-3-5-sonnet-latest");
    p.vendor = ProviderVendor::Anthropic;

    let err = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap_err();

    assert!(err.contains("Anthropic"));
    assert!(err.contains("OpenCode"));
}

#[test]
fn opencode_writes_responses_provider_directly_with_native_npm() {
    let root = tempfile::tempdir().unwrap();
    let p = provider("resp", ApiKind::Responses, "gpt-5.1-codex");

    let plan = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();

    assert_eq!(plan.routing_mode.as_str(), "direct-file");
    assert!(plan.warnings.is_empty());
    assert!(!plan.patches[0].after.contains("127.0.0.1"));
    let json: serde_json::Value = serde_json::from_str(&plan.patches[0].after).unwrap();
    assert_eq!(json["provider"]["resp"]["npm"], "@ai-sdk/openai");
    assert_eq!(json["model"], "resp/gpt-5.1-codex");
}

#[test]
fn claude_uses_local_anthropic_adapter_for_openai_compatible_provider() {
    let root = tempfile::tempdir().unwrap();
    let p = provider("zen", ApiKind::Chat, "deepseek-v4-flash-free");

    let plan = apply_agent(
        root.path(),
        AgentTarget::ClaudeCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();

    assert_eq!(plan.routing_mode.as_str(), "local-anthropic-messages");
    assert_eq!(plan.routing_mode, RoutingMode::LocalAnthropicMessages);
    let patch = plan
        .patches
        .iter()
        .find(|p| p.path.ends_with(".claude/settings.json"))
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&patch.after).unwrap();
    assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:9316");
    assert_eq!(json["env"]["ANTHROPIC_AUTH_TOKEN"], "local-proxy");
    assert!(json["env"].get("ANTHROPIC_API_KEY").is_none());
    assert_eq!(json["env"]["ANTHROPIC_MODEL"], "deepseek-v4-flash-free_zen");
    assert_eq!(json["env"]["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1");
    assert_eq!(json["env"]["CLAUDE_CODE_DISABLE_TERMINAL_TITLE"], "1");
    assert_eq!(
        json["xuCodex"]["endpoint"],
        "http://127.0.0.1:9316/v1/messages"
    );
}

#[test]
fn claude_rejects_responses_only_provider() {
    let root = tempfile::tempdir().unwrap();
    let p = provider("responses", ApiKind::Responses, "gpt-response");

    let error = apply_agent(
        root.path(),
        AgentTarget::ClaudeCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap_err();

    assert!(error.contains("Responses -> Anthropic Messages"));
}

#[test]
fn claude_writes_direct_anthropic_compatible_settings() {
    let root = tempfile::tempdir().unwrap();
    let mut p = provider("anth", ApiKind::Anthropic, "claude-3-5-sonnet-latest");
    p.vendor = ProviderVendor::Anthropic;
    p.base_url = "https://api.anthropic.com".to_string();

    let plan = apply_agent(
        root.path(),
        AgentTarget::ClaudeCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();

    assert_eq!(plan.routing_mode.as_str(), "direct-file");
    let patch = plan
        .patches
        .iter()
        .find(|p| p.path.ends_with(".claude/settings.json"))
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&patch.after).unwrap();
    assert_eq!(
        json["env"]["ANTHROPIC_BASE_URL"],
        "https://api.anthropic.com"
    );
    assert_eq!(json["env"]["ANTHROPIC_AUTH_TOKEN"], "anth-key");
    assert!(json["env"].get("ANTHROPIC_API_KEY").is_none());
    assert_eq!(json["env"]["ANTHROPIC_MODEL"], "claude-3-5-sonnet-latest");
    assert_eq!(json["env"]["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1");
    assert_eq!(json["env"]["CLAUDE_CODE_DISABLE_TERMINAL_TITLE"], "1");
    assert!(json["xuCodex"]["endpoint"].is_null());
}

#[test]
fn codex_writes_local_responses_config_and_catalog_for_chat_provider() {
    let root = tempfile::tempdir().unwrap();
    let p = provider("dext", ApiKind::Chat, "kimi-k2.7-code");

    let plan = apply_agent(
        root.path(),
        AgentTarget::Codex,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();

    assert_eq!(plan.routing_mode.as_str(), "local-openai-responses");
    assert_eq!(plan.routing_mode, RoutingMode::LocalOpenAiResponses);
    let config = plan
        .patches
        .iter()
        .find(|p| p.path.ends_with(".codex/config.toml"))
        .unwrap();
    assert!(config.after.contains("model = \"kimi-k2.7-code_dext\""));
    assert!(config.after.contains("wire_api = \"responses\""));
    assert!(config
        .after
        .contains("base_url = \"http://127.0.0.1:9316/v1\""));
    let catalog = plan
        .patches
        .iter()
        .find(|p| {
            p.path
                .ends_with(".codex/model-catalogs/xucodex-catalog.json")
        })
        .unwrap();
    assert!(catalog.after.contains("kimi-k2.7-code_dext"));
    assert!(catalog.after.contains("supports_parallel_tool_calls"));
}

#[test]
fn codex_can_write_direct_responses_config_for_native_provider() {
    let root = tempfile::tempdir().unwrap();
    let p = provider("openai", ApiKind::Responses, "gpt-5.1-codex");

    let plan = apply_agent(
        root.path(),
        AgentTarget::Codex,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap();

    assert_eq!(plan.routing_mode.as_str(), "direct-file");
    let config = plan
        .patches
        .iter()
        .find(|p| p.path.ends_with(".codex/config.toml"))
        .unwrap();
    assert!(config.after.contains("model = \"gpt-5.1-codex\""));
    assert!(!config.after.contains("model = \"gpt-5.1-codex_openai\""));
    assert!(config.after.contains("model_provider = \"xu_openai\""));
    assert!(config
        .after
        .contains("base_url = \"https://openai.example/v1\""));
    assert!(config
        .after
        .contains("experimental_bearer_token = \"openai-key\""));
    assert!(config.after.contains("wire_api = \"responses\""));
}

#[test]
fn invalid_provider_is_rejected_before_file_patch() {
    let root = tempfile::tempdir().unwrap();
    let mut p = provider("bad", ApiKind::Chat, "model");
    p.base_url = "bad-url".to_string();

    let err = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap_err();

    assert!(err.contains("base_url must start with http:// or https://"));
}

#[test]
fn invalid_default_model_is_rejected_before_file_patch() {
    let root = tempfile::tempdir().unwrap();
    let mut p = provider("bad", ApiKind::Chat, "listed-model");
    p.default_model = "missing-model".to_string();

    let err = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap_err();

    assert!(err.contains("default_model is not listed in models"));
}

#[test]
fn unreadable_existing_config_is_rejected_before_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(".config/opencode/opencode.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
    let p = provider("zen", ApiKind::Chat, "deepseek-v4-flash-free");

    let err = apply_agent(
        root.path(),
        AgentTarget::OpenCode,
        std::slice::from_ref(&p),
        std::slice::from_ref(&p.id),
    )
    .unwrap_err();

    assert!(err.contains("read"));
}

#[test]
fn apply_patch_supports_dry_run_backup_and_restore() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    fs::write(&path, "old").unwrap();

    let patch = spec::patch::ConfigPatch::new(path.clone(), "old".to_string(), "new".to_string());
    let dry = apply_patch(
        &patch,
        PatchOptions {
            dry_run: true,
            backup: true,
        },
    )
    .unwrap();
    assert!(dry.diff.contains("-old"));
    assert!(dry.diff.contains("+new"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    assert!(dry.backup_path.is_none());

    let applied = apply_patch(
        &patch,
        PatchOptions {
            dry_run: false,
            backup: true,
        },
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let backup = applied.backup_path.unwrap();
    assert!(backup.exists());
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(backup
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap()
        .contains(".spec."));

    restore_backup(&backup, &path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
}

#[test]
fn patch_diff_masks_secrets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    let patch = spec::patch::ConfigPatch::new(
        path,
        "{\n  \"apiKey\": \"old-secret\"\n}".to_string(),
        "{\n  \"apiKey\": \"new-secret\"\n}".to_string(),
    );

    let dry = apply_patch(
        &patch,
        PatchOptions {
            dry_run: true,
            backup: true,
        },
    )
    .unwrap();

    assert!(dry.diff.contains("<redacted>"));
    assert!(!dry.diff.contains("old-secret"));
    assert!(!dry.diff.contains("new-secret"));
}

#[test]
fn apply_plan_rolls_back_already_written_files_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.txt");
    fs::write(&first, "old").unwrap();
    let bad_target = root.path().join("bad-target");
    fs::create_dir_all(&bad_target).unwrap();

    let plan = ApplyPlan {
        target: AgentTarget::Codex,
        routing_mode: RoutingMode::DirectFile,
        protocol_adapter: None,
        patches: vec![
            spec::patch::ConfigPatch::new(first.clone(), "old".to_string(), "new".to_string()),
            spec::patch::ConfigPatch::new(bad_target, "".to_string(), "boom".to_string()),
        ],
        summary: Vec::new(),
        warnings: Vec::new(),
    };

    let err = spec::cli::apply_plan(&plan, false).unwrap_err();

    assert!(!err.is_empty());
    assert_eq!(fs::read_to_string(&first).unwrap(), "old");
}
