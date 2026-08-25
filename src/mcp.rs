use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table};

use crate::domain::AgentTarget;
use crate::patch::{read_before_checked, ConfigPatch};

pub const MCP_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub struct McpPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub package: &'static str,
    pub description: &'static str,
}

pub const MCP_PRESETS: &[McpPreset] = &[
    McpPreset {
        id: "memory",
        name: "Memory",
        package: "@modelcontextprotocol/server-memory",
        description: "Persistent knowledge graph memory",
    },
    McpPreset {
        id: "fetch",
        name: "Fetch",
        package: "@modelcontextprotocol/server-fetch",
        description: "Fetch and convert web content",
    },
    McpPreset {
        id: "time",
        name: "Time",
        package: "@modelcontextprotocol/server-time",
        description: "Time and timezone conversion",
    },
    McpPreset {
        id: "sequential-thinking",
        name: "Sequential Thinking",
        package: "@modelcontextprotocol/server-sequential-thinking",
        description: "Structured step-by-step reasoning",
    },
];

pub fn server_from_preset(id: &str, targets: McpTargets) -> Result<McpServer, String> {
    let preset = MCP_PRESETS
        .iter()
        .find(|preset| preset.id == id)
        .ok_or_else(|| format!("unknown MCP preset: {id}"))?;
    Ok(McpServer {
        id: preset.id.to_string(),
        name: preset.name.to_string(),
        transport: McpTransport::Stdio,
        command: Some("npx".to_string()),
        args: vec!["-y".to_string(), preset.package.to_string()],
        url: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        targets,
        description: Some(preset.description.to_string()),
        homepage: None,
    })
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
    Http,
    Sse,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpTargets {
    #[serde(default)]
    pub opencode: bool,
    #[serde(default)]
    pub claude: bool,
    #[serde(default)]
    pub codex: bool,
}

impl McpTargets {
    pub fn get(&self, target: AgentTarget) -> bool {
        match target {
            AgentTarget::OpenCode => self.opencode,
            AgentTarget::ClaudeCode => self.claude,
            AgentTarget::Codex => self.codex,
            AgentTarget::Other => false,
        }
    }

    pub fn set(&mut self, target: AgentTarget, enabled: bool) {
        match target {
            AgentTarget::OpenCode => self.opencode = enabled,
            AgentTarget::ClaudeCode => self.claude = enabled,
            AgentTarget::Codex => self.codex = enabled,
            AgentTarget::Other => {}
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub targets: McpTargets,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

impl McpServer {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err("MCP id must use ASCII letters, digits, '-' or '_'".to_string());
        }
        if self.name.trim().is_empty() {
            return Err("MCP name cannot be empty".to_string());
        }
        match self.transport {
            McpTransport::Stdio => {
                if self.command.as_deref().is_none_or(str::is_empty) {
                    return Err("stdio MCP requires command".to_string());
                }
                if self.url.is_some() {
                    return Err("stdio MCP cannot define url".to_string());
                }
            }
            McpTransport::Http | McpTransport::Sse => {
                let url = self
                    .url
                    .as_deref()
                    .ok_or_else(|| "remote MCP requires url".to_string())?;
                if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1")) {
                    return Err("remote MCP URL must use https or loopback http".to_string());
                }
                if self.command.is_some() || !self.args.is_empty() {
                    return Err("remote MCP cannot define command or args".to_string());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpStore {
    pub schema_version: u32,
    #[serde(default)]
    pub servers: BTreeMap<String, McpServer>,
}

impl Default for McpStore {
    fn default() -> Self {
        Self {
            schema_version: MCP_SCHEMA_VERSION,
            servers: BTreeMap::new(),
        }
    }
}

pub fn store_path(home: &Path) -> PathBuf {
    home.join(".codex/xu-mcp.json")
}

pub fn read_store(home: &Path) -> Result<McpStore, String> {
    let path = store_path(home);
    let before = read_before_checked(&path)?;
    if before.trim().is_empty() {
        return Ok(McpStore::default());
    }
    let store: McpStore = serde_json::from_str(&before)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if store.schema_version != MCP_SCHEMA_VERSION {
        return Err(format!(
            "unsupported MCP schema {}, expected {}",
            store.schema_version, MCP_SCHEMA_VERSION
        ));
    }
    for (id, server) in &store.servers {
        if id != &server.id {
            return Err(format!(
                "MCP map id {id} does not match server id {}",
                server.id
            ));
        }
        server.validate()?;
    }
    Ok(store)
}

pub fn store_patch(home: &Path, store: &McpStore) -> Result<ConfigPatch, String> {
    for server in store.servers.values() {
        server.validate()?;
    }
    let path = store_path(home);
    let before = read_before_checked(&path)?;
    let after = serde_json::to_string_pretty(store).map_err(|error| error.to_string())? + "\n";
    Ok(ConfigPatch::new(path, before, after))
}

pub fn import_live(
    home: &Path,
    target: AgentTarget,
    store: &mut McpStore,
) -> Result<usize, String> {
    let imported = match target {
        AgentTarget::OpenCode => {
            import_json(&home.join(".config/opencode/opencode.json"), "mcp", target)?
        }
        AgentTarget::ClaudeCode => import_json(&home.join(".claude.json"), "mcpServers", target)?,
        AgentTarget::Codex => import_codex(&home.join(".codex/config.toml"))?,
        AgentTarget::Other => return Err("unsupported MCP import target".to_string()),
    };
    let mut count = 0;
    for server in imported {
        merge_imported(store, server, target)?;
        count += 1;
    }
    Ok(count)
}

fn import_json(path: &Path, key: &str, target: AgentTarget) -> Result<Vec<McpServer>, String> {
    let text = read_before_checked(path)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: Value = serde_json::from_str(&text)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let Some(servers) = root.get(key) else {
        return Ok(Vec::new());
    };
    let servers = servers
        .as_object()
        .ok_or_else(|| format!("{}.{} must be an object", path.display(), key))?;
    servers
        .iter()
        .map(|(id, value)| parse_json_server(id, value, target))
        .collect()
}

fn parse_json_server(id: &str, value: &Value, target: AgentTarget) -> Result<McpServer, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("MCP server {id} must be an object"))?;
    let mut targets = McpTargets::default();
    targets.set(
        target,
        object.get("enabled").and_then(Value::as_bool) != Some(false),
    );
    let open_code_command = object.get("command").and_then(Value::as_array);
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string);
    if command.is_some()
        || open_code_command.is_some()
        || object.get("type").and_then(Value::as_str) == Some("local")
    {
        let (command, args) = if let Some(parts) = open_code_command {
            let parts = parts
                .iter()
                .map(|part| {
                    part.as_str().map(str::to_string).ok_or_else(|| {
                        format!("MCP server {id} command array must contain strings")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut parts = parts.into_iter();
            (
                parts
                    .next()
                    .ok_or_else(|| format!("MCP server {id} command cannot be empty"))?,
                parts.collect(),
            )
        } else {
            (
                command.ok_or_else(|| format!("MCP server {id} missing command"))?,
                string_array(object.get("args"), id, "args")?,
            )
        };
        return Ok(McpServer {
            id: id.to_string(),
            name: id.to_string(),
            transport: McpTransport::Stdio,
            command: Some(command),
            args,
            url: None,
            env: string_map(
                object.get("environment").or_else(|| object.get("env")),
                id,
                "env",
            )?,
            headers: BTreeMap::new(),
            targets,
            description: None,
            homepage: None,
        });
    }
    let url = object
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("MCP server {id} missing command or url"))?;
    Ok(McpServer {
        id: id.to_string(),
        name: id.to_string(),
        transport: if object.get("type").and_then(Value::as_str) == Some("sse") {
            McpTransport::Sse
        } else {
            McpTransport::Http
        },
        command: None,
        args: Vec::new(),
        url: Some(url.to_string()),
        env: BTreeMap::new(),
        headers: string_map(object.get("headers"), id, "headers")?,
        targets,
        description: None,
        homepage: None,
    })
}

fn import_codex(path: &Path) -> Result<Vec<McpServer>, String> {
    let text = read_before_checked(path)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: toml::Value =
        toml::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))?;
    let Some(servers) = root.get("mcp_servers") else {
        return Ok(Vec::new());
    };
    let servers = servers
        .as_table()
        .ok_or_else(|| "Codex mcp_servers must be a table".to_string())?;
    servers
        .iter()
        .map(|(id, value)| {
            let table = value
                .as_table()
                .ok_or_else(|| format!("Codex MCP server {id} must be a table"))?;
            let targets = McpTargets {
                opencode: false,
                claude: false,
                codex: table.get("enabled").and_then(toml::Value::as_bool) != Some(false),
            };
            if let Some(command) = table.get("command").and_then(toml::Value::as_str) {
                Ok(McpServer {
                    id: id.clone(),
                    name: id.clone(),
                    transport: McpTransport::Stdio,
                    command: Some(command.to_string()),
                    args: toml_string_array(table.get("args"), id, "args")?,
                    url: None,
                    env: toml_string_map(table.get("env"), id, "env")?,
                    headers: BTreeMap::new(),
                    targets,
                    description: None,
                    homepage: None,
                })
            } else {
                let url = table
                    .get("url")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| format!("Codex MCP server {id} missing command or url"))?;
                Ok(McpServer {
                    id: id.clone(),
                    name: id.clone(),
                    transport: McpTransport::Http,
                    command: None,
                    args: Vec::new(),
                    url: Some(url.to_string()),
                    env: BTreeMap::new(),
                    headers: toml_string_map(table.get("http_headers"), id, "http_headers")?,
                    targets,
                    description: None,
                    homepage: None,
                })
            }
        })
        .collect()
}

fn merge_imported(
    store: &mut McpStore,
    imported: McpServer,
    target: AgentTarget,
) -> Result<(), String> {
    imported.validate()?;
    if let Some(existing) = store.servers.get_mut(&imported.id) {
        let mut expected = existing.clone();
        expected.name = imported.name.clone();
        expected.targets = imported.targets.clone();
        expected.description = None;
        expected.homepage = None;
        let mut actual = imported.clone();
        actual.description = None;
        actual.homepage = None;
        if expected != actual {
            return Err(format!(
                "MCP server {} conflicts with existing definition; rename or reconcile it before import",
                imported.id
            ));
        }
        existing.targets.set(target, imported.targets.get(target));
    } else {
        store.servers.insert(imported.id.clone(), imported);
    }
    Ok(())
}

fn string_array(value: Option<&Value>, id: &str, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| format!("MCP server {id} {field} must be an array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("MCP server {id} {field} must contain strings"))
        })
        .collect()
}

fn string_map(
    value: Option<&Value>,
    id: &str,
    field: &str,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .ok_or_else(|| format!("MCP server {id} {field} must be an object"))?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| format!("MCP server {id} {field}.{key} must be a string"))
        })
        .collect()
}

fn toml_string_array(
    value: Option<&toml::Value>,
    id: &str,
    field: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| format!("MCP server {id} {field} must be an array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("MCP server {id} {field} must contain strings"))
        })
        .collect()
}

fn toml_string_map(
    value: Option<&toml::Value>,
    id: &str,
    field: &str,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    value
        .as_table()
        .ok_or_else(|| format!("MCP server {id} {field} must be a table"))?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| format!("MCP server {id} {field}.{key} must be a string"))
        })
        .collect()
}

pub fn projection_patch(
    home: &Path,
    target: AgentTarget,
    store: &McpStore,
    remove_ids: &[String],
) -> Result<ConfigPatch, String> {
    match target {
        AgentTarget::OpenCode => json_projection(
            home.join(".config/opencode/opencode.json"),
            "mcp",
            target,
            store,
            remove_ids,
        ),
        AgentTarget::ClaudeCode => json_projection(
            home.join(".claude.json"),
            "mcpServers",
            target,
            store,
            remove_ids,
        ),
        AgentTarget::Codex => codex_projection(home, store, remove_ids),
        AgentTarget::Other => Err("unsupported MCP target".to_string()),
    }
}

pub fn projection_patch_from(
    home: &Path,
    target: AgentTarget,
    store: &McpStore,
    remove_ids: &[String],
    before: String,
) -> Result<ConfigPatch, String> {
    match target {
        AgentTarget::OpenCode => json_projection_from(
            home.join(".config/opencode/opencode.json"),
            "mcp",
            target,
            store,
            remove_ids,
            before,
        ),
        AgentTarget::ClaudeCode => json_projection_from(
            home.join(".claude.json"),
            "mcpServers",
            target,
            store,
            remove_ids,
            before,
        ),
        AgentTarget::Codex => codex_projection_from(home, store, remove_ids, before),
        AgentTarget::Other => Err("unsupported MCP target".to_string()),
    }
}

fn json_projection(
    path: PathBuf,
    key: &str,
    target: AgentTarget,
    store: &McpStore,
    remove_ids: &[String],
) -> Result<ConfigPatch, String> {
    let before = read_before_checked(&path)?;
    json_projection_from(path, key, target, store, remove_ids, before)
}

fn json_projection_from(
    path: PathBuf,
    key: &str,
    target: AgentTarget,
    store: &McpStore,
    remove_ids: &[String],
    before: String,
) -> Result<ConfigPatch, String> {
    if before.trim().is_empty()
        && !store
            .servers
            .values()
            .any(|server| server.targets.get(target))
    {
        return Ok(ConfigPatch::new(path, before, String::new()));
    }
    let mut doc = if before.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(&before)
            .map_err(|error| format!("parse {}: {error}", path.display()))?
    };
    let root = doc
        .as_object_mut()
        .ok_or_else(|| format!("{} root must be an object", path.display()))?;
    let servers = root
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{}.{} must be an object", path.display(), key))?;
    for id in remove_ids {
        servers.remove(id);
    }
    for server in store.servers.values() {
        if server.targets.get(target) {
            servers.insert(server.id.clone(), json_server(server, target));
        } else {
            servers.remove(&server.id);
        }
    }
    let after = serde_json::to_string_pretty(&doc).map_err(|error| error.to_string())? + "\n";
    Ok(ConfigPatch::new(path, before, after))
}

fn json_server(server: &McpServer, target: AgentTarget) -> Value {
    match (target, server.transport) {
        (AgentTarget::OpenCode, McpTransport::Stdio) => serde_json::json!({
            "type": "local",
            "command": std::iter::once(server.command.clone().unwrap_or_default()).chain(server.args.clone()).collect::<Vec<_>>(),
            "environment": server.env,
            "enabled": true
        }),
        (AgentTarget::OpenCode, McpTransport::Http | McpTransport::Sse) => serde_json::json!({
            "type": "remote",
            "url": server.url,
            "headers": server.headers,
            "enabled": true
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

fn codex_projection(
    home: &Path,
    store: &McpStore,
    remove_ids: &[String],
) -> Result<ConfigPatch, String> {
    let path = home.join(".codex/config.toml");
    let before = read_before_checked(&path)?;
    codex_projection_from(home, store, remove_ids, before)
}

fn codex_projection_from(
    home: &Path,
    store: &McpStore,
    remove_ids: &[String],
    before: String,
) -> Result<ConfigPatch, String> {
    let path = home.join(".codex/config.toml");
    if before.trim().is_empty() && !store.servers.values().any(|server| server.targets.codex) {
        return Ok(ConfigPatch::new(path, before, String::new()));
    }
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
    let servers = doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| "Codex mcp_servers must be a table".to_string())?;
    for id in remove_ids {
        servers.remove(id);
    }
    for server in store.servers.values() {
        if !server.targets.codex {
            servers.remove(&server.id);
            continue;
        }
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
        servers.insert(&server.id, Item::Table(table));
    }
    Ok(ConfigPatch::new(path, before, doc.to_string()))
}

fn inline_table(values: &BTreeMap<String, String>) -> InlineTable {
    let mut table = InlineTable::new();
    for (key, value) in values {
        table.insert(key, value.as_str().into());
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> McpServer {
        McpServer {
            id: "memory".to_string(),
            name: "Memory".to_string(),
            transport: McpTransport::Stdio,
            command: Some("npx".to_string()),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-memory".to_string(),
            ],
            url: None,
            env: BTreeMap::from([("MODE".to_string(), "safe".to_string())]),
            headers: BTreeMap::new(),
            targets: McpTargets {
                opencode: true,
                claude: true,
                codex: true,
            },
            description: None,
            homepage: None,
        }
    }

    #[test]
    fn projections_preserve_unmanaged_settings() {
        let home = tempfile::tempdir().unwrap();
        let opencode = home.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(opencode.parent().unwrap()).unwrap();
        std::fs::write(
            &opencode,
            r#"{"theme":"custom","mcp":{"other":{"type":"remote","url":"https://example.com"}}}"#,
        )
        .unwrap();
        let mut store = McpStore::default();
        store.servers.insert("memory".to_string(), server());
        let patch = projection_patch(home.path(), AgentTarget::OpenCode, &store, &[]).unwrap();
        let value: Value = serde_json::from_str(&patch.after).unwrap();
        assert_eq!(value["theme"], "custom");
        assert!(value["mcp"]["other"].is_object());
        assert_eq!(value["mcp"]["memory"]["type"], "local");
    }

    #[test]
    fn codex_projection_preserves_provider_and_unmanaged_mcp() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "model = \"keep\"\n[mcp_servers.other]\ncommand = \"other\"\n",
        )
        .unwrap();
        let mut store = McpStore::default();
        store.servers.insert("memory".to_string(), server());
        let patch = projection_patch(home.path(), AgentTarget::Codex, &store, &[]).unwrap();
        assert!(patch.after.contains("model = \"keep\""));
        assert!(patch.after.contains("[mcp_servers.other]"));
        assert!(patch.after.contains("[mcp_servers.memory]"));
    }

    #[test]
    fn malformed_codex_config_fails_closed() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[broken").unwrap();
        assert!(
            projection_patch(home.path(), AgentTarget::Codex, &McpStore::default(), &[]).is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "[broken");
    }

    #[test]
    fn imports_matching_server_from_multiple_apps_and_merges_targets() {
        let home = tempfile::tempdir().unwrap();
        let open_code = home.path().join(".config/opencode/opencode.json");
        let claude = home.path().join(".claude.json");
        std::fs::create_dir_all(open_code.parent().unwrap()).unwrap();
        std::fs::write(
            &open_code,
            r#"{"mcp":{"memory":{"type":"local","command":["npx","-y","memory"],"enabled":true}}}"#,
        )
        .unwrap();
        std::fs::write(
            &claude,
            r#"{"mcpServers":{"memory":{"command":"npx","args":["-y","memory"]}}}"#,
        )
        .unwrap();
        let mut store = McpStore::default();
        assert_eq!(
            import_live(home.path(), AgentTarget::OpenCode, &mut store).unwrap(),
            1
        );
        assert_eq!(
            import_live(home.path(), AgentTarget::ClaudeCode, &mut store).unwrap(),
            1
        );
        assert!(store.servers["memory"].targets.opencode);
        assert!(store.servers["memory"].targets.claude);
    }

    #[test]
    fn import_rejects_conflicting_definition() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"memory":{"command":"different"}}}"#,
        )
        .unwrap();
        let mut store = McpStore::default();
        store.servers.insert("memory".to_string(), server());
        let error = import_live(home.path(), AgentTarget::ClaudeCode, &mut store).unwrap_err();
        assert!(error.contains("conflicts"));
        assert_eq!(store.servers["memory"].command.as_deref(), Some("npx"));
    }
}
