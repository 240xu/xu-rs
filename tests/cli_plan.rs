use std::fs;

use trivium::adapters::{AgentTarget, RoutingMode};
use trivium::cli::{plan_from_home, run_command};

#[test]
fn mcp_cli_is_dry_run_by_default_and_applies_only_with_yes() {
    let home = tempfile::tempdir().unwrap();
    let args = [
        "mcp",
        "add",
        "memory",
        "--name",
        "Memory",
        "--transport",
        "stdio",
        "--command",
        "npx",
        "--arg",
        "-y",
        "--arg",
        "@modelcontextprotocol/server-memory",
        "--target",
        "opencode",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let output = run_command(home.path(), &args).unwrap().unwrap();
    assert!(output.contains("Dry-run MCP add"));
    assert!(!home.path().join(".codex/xu-mcp.json").exists());

    let mut apply = args;
    apply.push("--yes".to_string());
    let output = run_command(home.path(), &apply).unwrap().unwrap();
    assert!(output.contains("Applied MCP add"));
    let store = trivium::mcp::read_store(home.path()).unwrap();
    assert_eq!(store.servers["memory"].args[0], "-y");
    let opencode: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(home.path().join(".config/opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(opencode["mcp"]["memory"]["type"], "local");
    assert!(!home.path().join(".claude.json").exists());
}

#[test]
fn mcp_cli_disable_and_delete_remove_live_projection() {
    let home = tempfile::tempdir().unwrap();
    let add = [
        "mcp",
        "add",
        "remote",
        "--transport",
        "http",
        "--url",
        "https://mcp.example/v1",
        "--target",
        "codex",
        "--yes",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    run_command(home.path(), &add).unwrap().unwrap();
    assert!(fs::read_to_string(home.path().join(".codex/config.toml"))
        .unwrap()
        .contains("[mcp_servers.remote]"));

    let disable = ["mcp", "disable", "remote", "--target", "codex", "--yes"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    run_command(home.path(), &disable).unwrap().unwrap();
    assert!(!fs::read_to_string(home.path().join(".codex/config.toml"))
        .unwrap()
        .contains("[mcp_servers.remote]"));

    let delete = ["mcp", "delete", "remote", "--yes"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    run_command(home.path(), &delete).unwrap().unwrap();
    assert!(trivium::mcp::read_store(home.path())
        .unwrap()
        .servers
        .is_empty());
}

#[test]
fn mcp_cli_import_is_dry_run_by_default() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".claude.json");
    fs::write(
        &path,
        r#"{"mcpServers":{"memory":{"command":"npx","args":["-y","memory"]}}}"#,
    )
    .unwrap();
    let args = ["mcp", "import", "--target", "claude"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let output = run_command(home.path(), &args).unwrap().unwrap();
    assert!(output.contains("Dry-run MCP import"));
    assert!(!home.path().join(".codex/xu-mcp.json").exists());
    let mut apply = args;
    apply.push("--yes".to_string());
    run_command(home.path(), &apply).unwrap().unwrap();
    assert!(
        trivium::mcp::read_store(home.path()).unwrap().servers["memory"]
            .targets
            .claude
    );
}

#[test]
fn mcp_preset_is_dry_run_and_preserves_dash_argument() {
    let home = tempfile::tempdir().unwrap();
    let args = ["mcp", "preset", "memory", "--target", "codex"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let output = run_command(home.path(), &args).unwrap().unwrap();
    assert!(output.contains("Dry-run MCP preset"));
    assert!(!home.path().join(".codex/xu-mcp.json").exists());
    let mut apply = args;
    apply.push("--yes".to_string());
    run_command(home.path(), &apply).unwrap().unwrap();
    let store = trivium::mcp::read_store(home.path()).unwrap();
    assert_eq!(store.servers["memory"].command.as_deref(), Some("npx"));
    assert_eq!(store.servers["memory"].args[0], "-y");
    assert!(store.servers["memory"].targets.codex);
}

#[test]
fn mcp_import_all_reports_one_broken_app_and_keeps_successes() {
    let home = tempfile::tempdir().unwrap();
    let open_code = home.path().join(".config/opencode/opencode.json");
    let codex = home.path().join(".codex/config.toml");
    fs::create_dir_all(open_code.parent().unwrap()).unwrap();
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(
        open_code,
        r#"{"mcp":{"memory":{"type":"local","command":["npx","-y","memory"]}}}"#,
    )
    .unwrap();
    fs::write(codex, "[broken").unwrap();
    let args = ["mcp", "import", "--target", "all", "--yes"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let output = run_command(home.path(), &args).unwrap().unwrap();
    assert!(output.contains("OpenCode: imported 1"));
    assert!(output.contains("Codex: failed:"));
    assert!(
        trivium::mcp::read_store(home.path()).unwrap().servers["memory"]
            .targets
            .opencode
    );
}

#[test]
fn mcp_update_preserves_unspecified_fields_and_is_dry_run() {
    let home = tempfile::tempdir().unwrap();
    let add = [
        "mcp",
        "add",
        "memory",
        "--name",
        "Old",
        "--transport",
        "stdio",
        "--command",
        "npx",
        "--arg",
        "-y",
        "--arg",
        "memory",
        "--env",
        "MODE=safe",
        "--target",
        "opencode",
        "--yes",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    run_command(home.path(), &add).unwrap().unwrap();
    let update = ["mcp", "update", "memory", "--name", "New"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let output = run_command(home.path(), &update).unwrap().unwrap();
    assert!(output.contains("Dry-run MCP update"));
    assert_eq!(
        trivium::mcp::read_store(home.path()).unwrap().servers["memory"].name,
        "Old"
    );
    let mut apply = update;
    apply.push("--yes".to_string());
    run_command(home.path(), &apply).unwrap().unwrap();
    let server = &trivium::mcp::read_store(home.path()).unwrap().servers["memory"];
    assert_eq!(server.name, "New");
    assert_eq!(server.args, ["-y", "memory"]);
    assert_eq!(server.env["MODE"], "safe");
    assert!(server.targets.opencode);
}

#[test]
fn skill_cli_zip_install_update_uninstall_and_restore() {
    let home = tempfile::tempdir().unwrap();
    let package_root = home.path().join("package");
    let nested = package_root.join("nested-skill");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("SKILL.md"), "# CLI Zip\n\nZip skill").unwrap();
    fs::write(nested.join("payload.txt"), "one").unwrap();
    let zip_path = home.path().join("cli-skill.zip");
    assert!(std::process::Command::new("zip")
        .args(["-qr"])
        .arg(&zip_path)
        .arg("nested-skill")
        .current_dir(&package_root)
        .status()
        .unwrap()
        .success());

    let dry = [
        "skill",
        "install-zip",
        "cli-zip",
        "--path",
        zip_path.to_str().unwrap(),
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let output = run_command(home.path(), &dry).unwrap().unwrap();
    assert!(output.contains("Dry-run skill install"));
    assert!(!home.path().join(".codex/xu-skills.json").exists());

    let mut apply = dry;
    apply.push("--yes".to_string());
    run_command(home.path(), &apply).unwrap().unwrap();
    run_command(
        home.path(),
        &["skill", "enable", "cli-zip", "--target", "claude", "--yes"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .unwrap();
    assert!(home.path().join(".claude/skills/cli-zip").exists());

    fs::write(nested.join("payload.txt"), "two").unwrap();
    let zip_v2 = home.path().join("cli-skill-v2.zip");
    assert!(std::process::Command::new("zip")
        .args(["-qr"])
        .arg(&zip_v2)
        .arg("nested-skill")
        .current_dir(&package_root)
        .status()
        .unwrap()
        .success());
    let mut store = trivium::skills::read_store(home.path()).unwrap();
    store.skills.get_mut("cli-zip").unwrap().origin = Some(trivium::skills::SkillOrigin::Zip {
        source: zip_v2.display().to_string(),
    });
    trivium::patch::apply_patch(
        &trivium::skills::store_patch(home.path(), &store).unwrap(),
        trivium::patch::PatchOptions {
            dry_run: false,
            backup: false,
        },
    )
    .unwrap();
    run_command(
        home.path(),
        &["skill", "update", "cli-zip", "--yes"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        fs::read_to_string(home.path().join(".codex/xu-skills/cli-zip/payload.txt")).unwrap(),
        "two"
    );

    let uninstall = ["skill", "uninstall", "cli-zip"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(run_command(home.path(), &uninstall)
        .unwrap()
        .unwrap()
        .contains("Dry-run skill uninstall"));
    let mut uninstall_yes = uninstall;
    uninstall_yes.push("--yes".to_string());
    let uninstalled = run_command(home.path(), &uninstall_yes).unwrap().unwrap();
    assert!(uninstalled.contains("Uninstalled skill"));
    assert!(!home.path().join(".claude/skills/cli-zip").exists());

    let backups = run_command(
        home.path(),
        &["skill", "backups"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .unwrap();
    assert!(backups.contains("cli-zip"));
    let backup_id = trivium::skills::read_backup_index(home.path()).unwrap()[0]
        .id
        .clone();
    run_command(
        home.path(),
        &["skill", "restore", &backup_id, "--yes"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .unwrap();
    assert!(home.path().join(".codex/xu-skills/cli-zip").exists());
}

#[test]
fn skill_cli_import_enable_disable_and_verify() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("skill-source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("SKILL.md"), "# Test\n\nUseful skill").unwrap();
    let import = vec![
        "skill".to_string(),
        "import".to_string(),
        "test".to_string(),
        "--path".to_string(),
        source.display().to_string(),
    ];
    let output = run_command(home.path(), &import).unwrap().unwrap();
    assert!(output.contains("Dry-run skill import"));
    assert!(!home.path().join(".codex/xu-skills.json").exists());
    let mut apply = import;
    apply.push("--yes".to_string());
    run_command(home.path(), &apply).unwrap().unwrap();

    let enable = ["skill", "enable", "test", "--target", "opencode", "--yes"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    run_command(home.path(), &enable).unwrap().unwrap();
    let target = home.path().join(".config/opencode/skills/test");
    assert!(target.symlink_metadata().unwrap().file_type().is_symlink());
    let verify = ["skill", "verify"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(run_command(home.path(), &verify)
        .unwrap()
        .unwrap()
        .contains("test: OK"));

    let disable = ["skill", "disable", "test", "--target", "opencode", "--yes"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    run_command(home.path(), &disable).unwrap().unwrap();
    assert!(target.symlink_metadata().is_err());
}

#[test]
fn builds_plan_from_existing_provider_and_route_files() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("xu-chat-providers.json"),
        r#"{
      "provider": {
        "zen": {
          "apiKind": "chat",
          "name": "OpenCode Zen (free)",
          "options": { "baseURL": "https://opencode.ai/zen/v1", "apiKey": "dummy" },
          "models": { "deepseek-v4-flash-free": {} }
        }
      }
    }"#,
    )
    .unwrap();
    fs::write(
        codex.join("xu-client-routes.json"),
        r#"{
      "clients": { "claude": { "enabled": true, "mode": "single", "providers": ["zen"] } }
    }"#,
    )
    .unwrap();

    let plan = plan_from_home(home.path(), AgentTarget::ClaudeCode, None).unwrap();

    assert_eq!(plan.routing_mode, RoutingMode::LocalAnthropicMessages);
    assert!(plan
        .patches
        .iter()
        .any(|p| p.path.ends_with(".claude/settings.json")));
}

#[test]
fn opencode_plan_does_not_require_client_routes_file() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("xu-chat-providers.json"),
        r#"{
      "provider": {
        "zen": {
          "apiKind": "chat",
          "name": "OpenCode Zen (free)",
          "options": { "baseURL": "https://opencode.ai/zen/v1", "apiKey": "dummy" },
          "models": { "deepseek-v4-flash-free": {} }
        }
      }
    }"#,
    )
    .unwrap();

    let plan = plan_from_home(home.path(), AgentTarget::OpenCode, None).unwrap();

    assert_eq!(plan.routing_mode, RoutingMode::DirectFile);
    assert!(plan
        .patches
        .iter()
        .any(|p| p.path.ends_with(".config/opencode/opencode.json")));
}

#[test]
fn opencode_default_selection_ignores_non_chat_providers() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("xu-chat-providers.json"),
        r#"{
      "provider": {
        "zen": {
          "apiKind": "chat",
          "name": "Zen",
          "options": { "baseURL": "https://opencode.ai/zen/v1", "apiKey": "dummy" },
          "models": { "deepseek-v4-flash-free": {} }
        },
        "anth": {
          "apiKind": "anthropic",
          "name": "Anthropic",
          "options": { "baseURL": "https://api.anthropic.com", "apiKey": "dummy" },
          "models": { "claude-3-5-sonnet-latest": {} }
        }
      }
    }"#,
    )
    .unwrap();

    let plan = plan_from_home(home.path(), AgentTarget::OpenCode, None).unwrap();
    let patch = plan
        .patches
        .iter()
        .find(|p| p.path.ends_with(".config/opencode/opencode.json"))
        .unwrap();

    assert!(patch.after.contains("zen"));
    assert!(!patch.after.contains("anth"));
}

#[test]
fn cli_use_dry_run_does_not_record_current_provider() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());

    let output = run_command(
        home.path(),
        &[
            "use".to_string(),
            "zen".to_string(),
            "--target".to_string(),
            "opencode".to_string(),
            "--dry-run".to_string(),
        ],
    )
    .unwrap()
    .unwrap();

    assert!(output.contains("Dry-run"));
    assert_eq!(
        trivium::state::current_provider(home.path(), AgentTarget::OpenCode).unwrap(),
        None
    );
}

#[test]
fn cli_use_records_current_provider_after_apply() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());

    let output = run_command(
        home.path(),
        &[
            "use".to_string(),
            "zen".to_string(),
            "--target".to_string(),
            "opencode".to_string(),
        ],
    )
    .unwrap()
    .unwrap();

    assert!(output.contains("Applied"));
    assert_eq!(
        trivium::state::current_provider(home.path(), AgentTarget::OpenCode).unwrap(),
        Some("zen".to_string())
    );
}

#[test]
fn cli_use_allows_target_flag_before_provider_id() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());

    let output = run_command(
        home.path(),
        &[
            "use".to_string(),
            "--target".to_string(),
            "opencode".to_string(),
            "zen".to_string(),
            "--dry-run".to_string(),
        ],
    )
    .unwrap()
    .unwrap();

    assert!(output.contains("Provider: zen"));
}

#[test]
fn cli_current_rejects_unknown_target() {
    let home = tempfile::tempdir().unwrap();

    let err = run_command(home.path(), &["current".to_string(), "bad".to_string()])
        .unwrap()
        .unwrap_err();

    assert!(err.contains("unknown target"));
}

#[test]
fn doctor_reports_malformed_provider_file() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(codex.join("xu-chat-providers.json"), "{bad json").unwrap();

    let output = run_command(home.path(), &["doctor".to_string()])
        .unwrap()
        .unwrap();

    assert!(output.contains("provider_error"));
}

#[test]
fn cli_use_rejects_bad_state_before_writing_config() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());
    let opencode = home.path().join(".config/opencode/opencode.json");
    fs::create_dir_all(opencode.parent().unwrap()).unwrap();
    fs::write(&opencode, "old").unwrap();
    fs::write(home.path().join(".codex/xu-state.json"), "{bad json").unwrap();

    let err = run_command(
        home.path(),
        &[
            "use".to_string(),
            "zen".to_string(),
            "--target".to_string(),
            "opencode".to_string(),
        ],
    )
    .unwrap()
    .unwrap_err();

    assert!(err.contains("parse"));
    assert_eq!(fs::read_to_string(opencode).unwrap(), "old");
}

#[test]
fn provider_cli_add_show_and_delete() {
    let home = tempfile::tempdir().unwrap();

    let add = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "add".to_string(),
            "zen".to_string(),
            "--kind".to_string(),
            "chat".to_string(),
            "--name".to_string(),
            "Zen".to_string(),
            "--base-url".to_string(),
            "https://example.com/v1".to_string(),
            "--api-key".to_string(),
            "secret-key".to_string(),
            "--model".to_string(),
            "model-a".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let show = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "show".to_string(),
            "zen".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let stored = fs::read_to_string(home.path().join(".codex/xu-chat-providers.json")).unwrap();
    trivium::state::set_provider_health(home.path(), "zen", "secret-key", true, Some(42), "ok")
        .unwrap();
    let delete = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "delete".to_string(),
            "zen".to_string(),
            "--yes".to_string(),
        ],
    )
    .unwrap()
    .unwrap();

    assert!(add.contains("Added"));
    assert!(show.contains("api_key: <redacted>"));
    assert!(stored.contains(r#""apiKind": "chat""#));
    assert!(delete.contains("Deleted"));
    assert!(trivium::state::read_state(home.path())
        .unwrap()
        .provider_health
        .is_empty());
}

#[test]
fn provider_cli_lists_presets_and_adds_from_preset() {
    let home = tempfile::tempdir().unwrap();

    let presets = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "preset".to_string(),
            "list".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let add = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "add".to_string(),
            "router".to_string(),
            "--preset".to_string(),
            "openrouter".to_string(),
            "--api-key".to_string(),
            "secret-key".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let stored = fs::read_to_string(home.path().join(".codex/xu-chat-providers.json")).unwrap();

    assert!(presets.contains("openrouter"));
    assert!(add.contains("Added"));
    assert!(stored.contains("OpenRouter"));
    assert!(stored.contains("openai/gpt-4o-mini"));
}

#[test]
fn provider_cli_update_preserves_key_and_replaces_models() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());

    let dry = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "update".to_string(),
            "zen".to_string(),
            "--name".to_string(),
            "Zen Updated".to_string(),
            "--base-url".to_string(),
            "https://example.com/v1".to_string(),
            "--model".to_string(),
            "model-b".to_string(),
            "--dry-run".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let after_dry = fs::read_to_string(home.path().join(".codex/xu-chat-providers.json")).unwrap();
    let applied = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "update".to_string(),
            "zen".to_string(),
            "--name".to_string(),
            "Zen Updated".to_string(),
            "--base-url".to_string(),
            "https://example.com/v1".to_string(),
            "--model".to_string(),
            "model-b".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let stored = fs::read_to_string(home.path().join(".codex/xu-chat-providers.json")).unwrap();

    assert!(dry.contains("Dry-run update"));
    assert!(after_dry.contains("deepseek-v4-flash-free"));
    assert!(applied.contains("Updated"));
    assert!(stored.contains("Zen Updated"));
    assert!(stored.contains("model-b"));
    assert!(stored.contains("dummy"));
}

#[test]
fn provider_cli_updates_detailed_touch_editor_settings() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("xu-chat-providers.json"),
        r#"{
          "provider": {
            "zen": {
              "apiKind": "chat",
              "name": "Zen",
              "options": {
                "baseURL": "https://example.com/v1",
                "apiKey": "secret-key",
                "customHeaders": { "Old": "remove-me" }
              },
              "models": {
                "model-a": { "modalities": { "input": ["text", "image"] } },
                "model-b": {}
              },
              "defaultModel": "model-a"
            }
          }
        }"#,
    )
    .unwrap();

    let output = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "update".to_string(),
            "zen".to_string(),
            "--model".to_string(),
            "model-a".to_string(),
            "--model".to_string(),
            "model-b".to_string(),
            "--default-model".to_string(),
            "model-b".to_string(),
            "--timeout".to_string(),
            "75000".to_string(),
            "--max-retries".to_string(),
            "4".to_string(),
            "--context-window".to_string(),
            "200000".to_string(),
            "--max-output-tokens".to_string(),
            "64000".to_string(),
            "--reasoning-effort".to_string(),
            "high".to_string(),
            "--clear-headers".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let stored: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(codex.join("xu-chat-providers.json")).unwrap())
            .unwrap();
    let provider = &stored["provider"]["zen"];

    assert!(output.contains("Updated provider"));
    assert_eq!(provider["defaultModel"], "model-b");
    assert_eq!(provider["options"]["timeout"], 75000);
    assert_eq!(provider["options"]["maxRetries"], 4);
    assert_eq!(provider["options"]["contextWindow"], 200000);
    assert_eq!(provider["options"]["maxOutputTokens"], 64000);
    assert_eq!(provider["options"]["reasoningEffort"], "high");
    assert!(provider["options"].get("customHeaders").is_none());
    assert_eq!(
        provider["models"]["model-a"]["modalities"]["input"][1],
        "image"
    );
}

#[test]
fn provider_cli_duplicate_preserves_metadata_and_supports_notes() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("xu-chat-providers.json"),
        r#"{
          "provider": {
            "zen": {
              "apiKind": "chat",
              "name": "Zen",
              "options": { "baseURL": "https://example.com/v1", "apiKey": "secret-key" },
              "models": { "model-a": { "modalities": { "input": ["text", "image"] } } }
            }
          }
        }"#,
    )
    .unwrap();

    let dry = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "duplicate".to_string(),
            "zen".to_string(),
            "zen-copy".to_string(),
            "--dry-run".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let after_dry = fs::read_to_string(codex.join("xu-chat-providers.json")).unwrap();
    let applied = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "duplicate".to_string(),
            "zen".to_string(),
            "zen-copy".to_string(),
            "--name".to_string(),
            "Zen Copy".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let update = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "update".to_string(),
            "zen-copy".to_string(),
            "--website".to_string(),
            "https://example.com".to_string(),
            "--notes".to_string(),
            "fallback account".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let show = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "show".to_string(),
            "zen-copy".to_string(),
        ],
    )
    .unwrap()
    .unwrap();
    let stored = fs::read_to_string(codex.join("xu-chat-providers.json")).unwrap();

    assert!(dry.contains("Dry-run duplicate"));
    assert!(!after_dry.contains("zen-copy"));
    assert!(applied.contains("Duplicated provider: zen -> zen-copy"));
    assert!(update.contains("Updated provider"));
    assert!(stored.contains("secret-key"));
    assert!(stored.contains("modalities"));
    assert!(stored.contains("fallback account"));
    assert!(show.contains("website: https://example.com"));
    assert!(show.contains("notes: fallback account"));
    assert!(!show.contains("secret-key"));
}

#[test]
fn provider_cli_rejects_invalid_website() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());

    let err = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "update".to_string(),
            "zen".to_string(),
            "--website".to_string(),
            "not-a-url".to_string(),
        ],
    )
    .unwrap()
    .unwrap_err();

    assert!(err.contains("website must start with http:// or https://"));
}

#[test]
fn provider_cli_refuses_to_delete_active_provider() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());
    trivium::state::set_current_provider(home.path(), AgentTarget::OpenCode, "zen").unwrap();

    let err = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "delete".to_string(),
            "zen".to_string(),
            "--yes".to_string(),
        ],
    )
    .unwrap()
    .unwrap_err();

    assert!(err.contains("active provider"));
}

#[test]
fn provider_add_rejects_missing_id_and_missing_flag_values() {
    let home = tempfile::tempdir().unwrap();

    let missing_id = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "add".to_string(),
            "--kind".to_string(),
            "chat".to_string(),
            "--base-url".to_string(),
            "https://example.com/v1".to_string(),
            "--api-key".to_string(),
            "secret".to_string(),
            "--model".to_string(),
            "m".to_string(),
        ],
    )
    .unwrap()
    .unwrap_err();
    let missing_value = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "add".to_string(),
            "zen".to_string(),
            "--base-url".to_string(),
            "--api-key".to_string(),
            "secret".to_string(),
            "--model".to_string(),
            "m".to_string(),
        ],
    )
    .unwrap()
    .unwrap_err();
    let missing_model_value = run_command(
        home.path(),
        &[
            "provider".to_string(),
            "add".to_string(),
            "zen".to_string(),
            "--base-url".to_string(),
            "https://example.com/v1".to_string(),
            "--api-key".to_string(),
            "secret".to_string(),
            "--model".to_string(),
            "--dry-run".to_string(),
        ],
    )
    .unwrap()
    .unwrap_err();

    assert!(missing_id.contains("usage"));
    assert!(missing_value.contains("--base-url requires a value"));
    assert!(missing_model_value.contains("--model requires a value"));
}

#[test]
fn doctor_reports_malformed_state_without_failing() {
    let home = tempfile::tempdir().unwrap();
    write_chat_provider(home.path());
    fs::write(home.path().join(".codex/xu-state.json"), "{bad json").unwrap();

    let output = run_command(home.path(), &["doctor".to_string()])
        .unwrap()
        .unwrap();

    assert!(output.contains("opencode_state_error"));
}

#[test]
fn agent_install_without_yes_is_preview_only() {
    let home = tempfile::tempdir().unwrap();
    let out = run_command(
        home.path(),
        &[
            "agent".to_string(),
            "install".to_string(),
            "claude".to_string(),
        ],
    )
    .unwrap()
    .unwrap();

    assert!(out.contains("客户端安装/更新计划"));
    assert!(out.contains("加 --yes 才会真正执行"));
    assert!(out.contains("Claude Code") || out.contains("claude"));
    assert!(out.contains("依赖检查") || out.contains("卸载旧包"));
}

#[test]
fn agent_setup_is_preview_only_and_doctor_reports_environment() {
    let home = tempfile::tempdir().unwrap();

    let setup = run_command(home.path(), &["agent".to_string(), "setup".to_string()])
        .unwrap()
        .unwrap();
    let doctor = run_command(home.path(), &["agent".to_string(), "doctor".to_string()])
        .unwrap()
        .unwrap();

    assert!(setup.contains("客户端安装/更新计划"));
    assert!(setup.contains("Codex CLI"));
    assert!(setup.contains("Claude Code"));
    assert!(setup.contains("OpenCode"));
    assert!(doctor.contains("Termux 客户端诊断") || doctor.contains("客户端诊断"));
    assert!(doctor.contains("nodejs"));
}

#[test]
fn agent_command_rejects_unknown_tool() {
    let home = tempfile::tempdir().unwrap();
    let error = run_command(
        home.path(),
        &[
            "agent".to_string(),
            "install".to_string(),
            "bad".to_string(),
        ],
    )
    .unwrap()
    .unwrap_err();

    assert!(error.contains("unknown agent: bad"));
}

fn write_chat_provider(home: &std::path::Path) {
    let codex = home.join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("xu-chat-providers.json"),
        r#"{
      "provider": {
        "zen": {
          "apiKind": "chat",
          "name": "Zen",
          "options": { "baseURL": "https://opencode.ai/zen/v1", "apiKey": "dummy" },
          "models": { "deepseek-v4-flash-free": {} }
        }
      }
    }"#,
    )
    .unwrap();
}
