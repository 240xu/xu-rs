use std::path::Path;

use serde_json::{Map, Value};
use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table};

use crate::domain::AgentTarget;
use crate::mcp::{import_live, McpServer, McpStore, McpTransport};
use crate::patch::{apply_patch, read_before_checked, ConfigPatch, PatchOptions};
use crate::sync::SyncReport;

pub fn sync_mcp(
    home: &Path,
    source: AgentTarget,
    targets: &[AgentTarget],
    dry_run: bool,
) -> Result<SyncReport, String> {
    if source == AgentTarget::Other {
        return Err("unsupported MCP sync source".to_string());
    }
    let mut store = McpStore::default();
    let count = import_live(home, source, &mut store)?;
    let mut report = SyncReport::default();
    if count == 0 {
        report
            .details
            .push(format!("no MCP servers found in {}", source.label()));
        return Ok(report);
    }
    if source == AgentTarget::ClaudeCode {
        apply_claude_settings_enabled(home, &mut store);
    }
    let servers: Vec<McpServer> = store.servers.into_values().collect();
    for target in targets {
        if *target == AgentTarget::Other {
            return Err("unsupported MCP sync target".to_string());
        }
        sync_target(home, source, *target, &servers, &mut report, dry_run)?;
    }
    Ok(report)
}

fn apply_claude_settings_enabled(home: &Path, store: &mut McpStore) {
    let path = home.join(".claude/settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(root) = text.parse::<Value>() else {
        return;
    };
    let disabled = root
        .get("disabledMcpjsonServers")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let enabled = root
        .get("enabledMcpjsonServers")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    for server in store.servers.values_mut() {
        if disabled.iter().any(|name| name == &server.id) {
            server.targets.claude = false;
        } else if enabled.iter().any(|name| name == &server.id) {
            server.targets.claude = true;
        }
    }
}

fn sync_target(
    home: &Path,
    source: AgentTarget,
    target: AgentTarget,
    servers: &[McpServer],
    report: &mut SyncReport,
    dry_run: bool,
) -> Result<(), String> {
    match target {
        AgentTarget::OpenCode => sync_json_target(
            home.join(".config/opencode/opencode.json"),
            "mcp",
            source,
            target,
            servers,
            report,
            dry_run,
        ),
        AgentTarget::ClaudeCode => sync_json_target(
            home.join(".claude.json"),
            "mcpServers",
            source,
            target,
            servers,
            report,
            dry_run,
        ),
        AgentTarget::Codex => sync_codex_target(home, source, servers, report, dry_run),
        AgentTarget::Other => Err("unsupported MCP sync target".to_string()),
    }
}

fn sync_json_target(
    path: std::path::PathBuf,
    key: &str,
    source: AgentTarget,
    target: AgentTarget,
    servers: &[McpServer],
    report: &mut SyncReport,
    dry_run: bool,
) -> Result<(), String> {
    let before = read_before_checked(&path)?;
    let mut doc: Value = if before.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(&before)
            .map_err(|error| format!("parse {}: {error}", path.display()))?
    };
    let root = doc
        .as_object_mut()
        .ok_or_else(|| format!("{} root must be an object", path.display()))?;
    let entries = root
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{}.{} must be an object", path.display(), key))?;
    let mut changed = false;
    for server in servers {
        let enabled = server.targets.get(source);
        let entry = json_server_entry(server, target, enabled);
        match entries.get(&server.id) {
            None => report.added += 1,
            Some(existing) if *existing == entry => {
                report.unchanged += 1;
                continue;
            }
            Some(_) => {
                report.updated += 1;
                report.details.push(format!(
                    "MCP server {} already exists in {}; overwriting with source definition",
                    server.id,
                    target.label()
                ));
            }
        }
        if target == AgentTarget::ClaudeCode && !enabled {
            report.details.push(format!(
                "MCP server {} is disabled in source; claude user-level mcpServers are always enabled",
                server.id
            ));
        }
        entries.insert(server.id.clone(), entry);
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    let after = serde_json::to_string_pretty(&doc).map_err(|error| error.to_string())? + "\n";
    apply_patch(
        &ConfigPatch::new(path, before, after),
        PatchOptions {
            dry_run,
            backup: true,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn json_server_entry(server: &McpServer, target: AgentTarget, enabled: bool) -> Value {
    match (target, server.transport) {
        (AgentTarget::OpenCode, McpTransport::Stdio) => serde_json::json!({
            "type": "local",
            "command": std::iter::once(server.command.clone().unwrap_or_default())
                .chain(server.args.clone())
                .collect::<Vec<_>>(),
            "environment": server.env,
            "enabled": enabled
        }),
        (AgentTarget::OpenCode, McpTransport::Http | McpTransport::Sse) => serde_json::json!({
            "type": "remote",
            "url": server.url,
            "headers": server.headers,
            "enabled": enabled
        }),
        (_, McpTransport::Stdio) => serde_json::json!({
            "command": server.command,
            "args": server.args,
            "env": server.env
        }),
        (_, McpTransport::Http) => serde_json::json!({
            "type": "http",
            "url": server.url,
            "headers": server.headers
        }),
        (_, McpTransport::Sse) => serde_json::json!({
            "type": "sse",
            "url": server.url,
            "headers": server.headers
        }),
    }
}

fn sync_codex_target(
    home: &Path,
    source: AgentTarget,
    servers: &[McpServer],
    report: &mut SyncReport,
    dry_run: bool,
) -> Result<(), String> {
    let path = home.join(".codex/config.toml");
    let before = read_before_checked(&path)?;
    let mut doc = if before.trim().is_empty() {
        DocumentMut::new()
    } else {
        before
            .parse::<DocumentMut>()
            .map_err(|error| format!("parse {}: {error}", path.display()))?
    };
    if !doc.contains_key("mcp_servers") {
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    let existing = doc["mcp_servers"]
        .as_table()
        .ok_or_else(|| "Codex mcp_servers must be a table".to_string())?;
    let existing_entries: Vec<(String, String)> = existing
        .iter()
        .map(|(name, item)| (name.to_string(), item.to_string()))
        .collect();
    let mut table = existing.clone();
    let mut changed = false;
    for server in servers {
        let enabled = server.targets.get(source);
        let entry = codex_entry(server, enabled);
        let existing_text = existing_entries
            .iter()
            .find(|(name, _)| name == &server.id)
            .map(|(_, text)| text.clone());
        match existing_text {
            None => report.added += 1,
            Some(text) if toml_equivalent(&text, &entry.to_string()) => {
                report.unchanged += 1;
                continue;
            }
            Some(_) => {
                report.updated += 1;
                report.details.push(format!(
                    "MCP server {} already exists in {}; overwriting with source definition",
                    server.id,
                    AgentTarget::Codex.label()
                ));
            }
        }
        table.insert(&server.id, Item::Table(entry));
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    doc["mcp_servers"] = Item::Table(table);
    let after = doc.to_string();
    apply_patch(
        &ConfigPatch::new(path, before, after),
        PatchOptions {
            dry_run,
            backup: true,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn toml_equivalent(a: &str, b: &str) -> bool {
    match (a.parse::<toml::Value>(), b.parse::<toml::Value>()) {
        (Ok(left), Ok(right)) => left == right,
        _ => a == b,
    }
}

fn codex_entry(server: &McpServer, enabled: bool) -> Table {
    let mut table = Table::new();
    match server.transport {
        McpTransport::Stdio => {
            table["command"] = value(server.command.as_deref().unwrap_or_default());
            let mut args = Array::new();
            for arg in &server.args {
                args.push(arg.as_str());
            }
            if !args.is_empty() {
                table["args"] = value(args);
            }
            if !server.env.is_empty() {
                table["env"] = Item::Value(inline_table(&server.env).into());
            }
        }
        McpTransport::Http | McpTransport::Sse => {
            table["url"] = value(server.url.as_deref().unwrap_or_default());
            if !server.headers.is_empty() {
                table["http_headers"] = Item::Value(inline_table(&server.headers).into());
            }
        }
    }
    if !enabled {
        table["enabled"] = value(false);
    }
    table
}

fn inline_table(values: &std::collections::BTreeMap<String, String>) -> InlineTable {
    let mut table = InlineTable::new();
    for (key, value) in values {
        table.insert(key, value.as_str().into());
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_opencode_mcp(home: &Path, mcp: &str) {
        let path = home.join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{{\"mcp\":{mcp}}}")).unwrap();
    }

    fn read_json(home: &Path, path: &str) -> Value {
        let text = std::fs::read_to_string(home.join(path)).unwrap();
        text.parse().unwrap()
    }

    fn sample_opencode_mcp() -> String {
        r#"{
          "memory": {
            "type": "local",
            "command": ["npx", "-y", "@modelcontextprotocol/server-memory"],
            "environment": {"MODE": "safe"}
          },
          "cf-memory": {
            "type": "remote",
            "url": "https://mcp.example.com/mcp",
            "headers": {"Authorization": "Bearer token"},
            "enabled": false
          }
        }"#
        .to_string()
    }

    #[test]
    fn roundtrip_opencode_to_claude_to_codex() {
        let home = tempfile::tempdir().unwrap();
        write_opencode_mcp(home.path(), &sample_opencode_mcp());
        let report = sync_mcp(
            home.path(),
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();
        assert_eq!(report.added, 2);
        assert_eq!(report.updated, 0);
        assert!(report.details.iter().any(|d| d.contains("cf-memory")));
        let claude = read_json(home.path(), ".claude.json");
        let servers = claude["mcpServers"].as_object().unwrap();
        assert_eq!(servers["memory"]["command"].as_str(), Some("npx"));
        assert_eq!(servers["memory"]["args"][0].as_str(), Some("-y"));
        assert_eq!(servers["memory"]["env"]["MODE"].as_str(), Some("safe"));
        assert!(servers["memory"].get("type").is_none());
        assert_eq!(servers["cf-memory"]["type"].as_str(), Some("http"));
        assert_eq!(servers["cf-memory"]["url"], "https://mcp.example.com/mcp");
        assert_eq!(
            servers["cf-memory"]["headers"]["Authorization"],
            "Bearer token"
        );
        let codex_report = sync_mcp(
            home.path(),
            AgentTarget::ClaudeCode,
            &[AgentTarget::Codex],
            false,
        )
        .unwrap();
        assert_eq!(codex_report.added, 2);
        let toml_text = std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
        let toml: toml::Value = toml_text.parse().unwrap();
        let servers = toml["mcp_servers"].as_table().unwrap();
        assert_eq!(servers["memory"]["command"].as_str(), Some("npx"));
        assert_eq!(servers["memory"]["args"][0].as_str(), Some("-y"));
        assert_eq!(servers["memory"]["env"]["MODE"].as_str(), Some("safe"));
        assert_eq!(
            servers["cf-memory"]["url"].as_str(),
            Some("https://mcp.example.com/mcp")
        );
        assert_eq!(
            servers["cf-memory"]["http_headers"]["Authorization"].as_str(),
            Some("Bearer token")
        );
    }

    #[test]
    fn syncs_all_servers_including_disabled() {
        let home = tempfile::tempdir().unwrap();
        write_opencode_mcp(home.path(), &sample_opencode_mcp());
        let report = sync_mcp(
            home.path(),
            AgentTarget::OpenCode,
            &[AgentTarget::Codex],
            false,
        )
        .unwrap();
        assert_eq!(report.added, 2);
        let toml_text = std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
        let toml: toml::Value = toml_text.parse().unwrap();
        let codex = toml["mcp_servers"].as_table().unwrap();
        assert!(codex.contains_key("memory"));
        assert!(codex.contains_key("cf-memory"));
        assert_eq!(codex["cf-memory"]["enabled"].as_bool(), Some(false));
        assert!(codex["memory"].get("enabled").is_none());
        let opencode = read_json(home.path(), ".config/opencode/opencode.json");
        assert_eq!(opencode["mcp"]["cf-memory"]["enabled"], false);
        assert_eq!(opencode["mcp"]["cf-memory"]["type"], "remote");
    }

    #[test]
    fn overwrite_warns_and_merges_existing_target_servers() {
        let home = tempfile::tempdir().unwrap();
        write_opencode_mcp(home.path(), &sample_opencode_mcp());
        let claude_path = home.path().join(".claude.json");
        std::fs::create_dir_all(claude_path.parent().unwrap()).unwrap();
        std::fs::write(
            &claude_path,
            r#"{"theme":"dark","mcpServers":{"memory":{"command":"other"},"kept":{"command":"local-keep"}}}"#,
        )
        .unwrap();
        let report = sync_mcp(
            home.path(),
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(report.updated, 1);
        assert!(report
            .details
            .iter()
            .any(|d| d.contains("memory") && d.contains("overwriting")));
        let claude = read_json(home.path(), ".claude.json");
        assert_eq!(claude["theme"], "dark");
        let servers = claude["mcpServers"].as_object().unwrap();
        assert_eq!(servers["memory"]["command"].as_str(), Some("npx"));
        assert_eq!(servers["kept"]["command"], "local-keep");
    }

    #[test]
    fn second_run_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        write_opencode_mcp(home.path(), &sample_opencode_mcp());
        let first = sync_mcp(
            home.path(),
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode, AgentTarget::Codex],
            false,
        )
        .unwrap();
        assert_eq!(first.added, 4);
        let claude_before = std::fs::read_to_string(home.path().join(".claude.json")).unwrap();
        let codex_before = std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
        let second = sync_mcp(
            home.path(),
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode, AgentTarget::Codex],
            false,
        )
        .unwrap();
        assert_eq!(second.added, 0);
        assert_eq!(second.updated, 0);
        assert_eq!(second.unchanged, 4);
        assert_eq!(
            std::fs::read_to_string(home.path().join(".claude.json")).unwrap(),
            claude_before
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap(),
            codex_before
        );
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let home = tempfile::tempdir().unwrap();
        write_opencode_mcp(home.path(), &sample_opencode_mcp());
        let report = sync_mcp(
            home.path(),
            AgentTarget::OpenCode,
            &[AgentTarget::Codex],
            true,
        )
        .unwrap();
        assert_eq!(report.added, 2);
        assert!(!home.path().join(".codex/config.toml").exists());
    }

    #[test]
    fn empty_source_returns_empty_report() {
        let home = tempfile::tempdir().unwrap();
        write_opencode_mcp(home.path(), "{}");
        let report = sync_mcp(
            home.path(),
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(report.unchanged, 0);
        assert!(!home.path().join(".claude.json").exists());
    }

    #[test]
    fn codex_source_syncs_to_opencode() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"[mcp_servers.search]
command = "npx"
args = ["-y", "server-search"]
env = { KEY = "value" }

[mcp_servers.offline]
url = "https://example.com/mcp"
http_headers = { Authorization = "Bearer x" }
enabled = false
"#,
        )
        .unwrap();
        let report = sync_mcp(
            home.path(),
            AgentTarget::Codex,
            &[AgentTarget::OpenCode],
            false,
        )
        .unwrap();
        assert_eq!(report.added, 2);
        let opencode = read_json(home.path(), ".config/opencode/opencode.json");
        let server = &opencode["mcp"]["search"];
        assert_eq!(server["type"], "local");
        assert_eq!(server["command"][0], "npx");
        assert_eq!(server["environment"]["KEY"], "value");
        assert_eq!(server["enabled"], true);
        let offline = &opencode["mcp"]["offline"];
        assert_eq!(offline["type"], "remote");
        assert_eq!(offline["url"], "https://example.com/mcp");
        assert_eq!(offline["headers"]["Authorization"], "Bearer x");
        assert_eq!(offline["enabled"], false);
    }

    #[test]
    fn claude_settings_enabled_state_applies_to_source() {
        let home = tempfile::tempdir().unwrap();
        let claude_path = home.path().join(".claude.json");
        std::fs::create_dir_all(claude_path.parent().unwrap()).unwrap();
        std::fs::write(
            &claude_path,
            r#"{"mcpServers":{"memory":{"command":"npx"},"fetch":{"command":"npx"}}}"#,
        )
        .unwrap();
        let settings_path = home.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        std::fs::write(
            &settings_path,
            r#"{"disabledMcpjsonServers":["fetch"],"enabledMcpjsonServers":["memory"]}"#,
        )
        .unwrap();
        let report = sync_mcp(
            home.path(),
            AgentTarget::ClaudeCode,
            &[AgentTarget::Codex],
            false,
        )
        .unwrap();
        assert_eq!(report.added, 2);
        let toml_text = std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
        let toml: toml::Value = toml_text.parse().unwrap();
        let servers = toml["mcp_servers"].as_table().unwrap();
        assert!(servers["memory"].get("enabled").is_none());
        assert_eq!(servers["fetch"]["enabled"].as_bool(), Some(false));
    }
}
