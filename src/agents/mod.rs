use std::path::{Path, PathBuf};

use crate::domain::{AgentConfigSpec, ProtocolAdapter, ProtocolKind, ProviderProfile};
pub use crate::domain::{AgentTarget, RoutingMode};
use crate::patch::{read_before_checked, ConfigPatch};

mod claude;
mod codex;
mod opencode;

pub fn serve_port() -> u16 {
    std::env::var("XU_SERVE_PORT")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(9316)
}

#[derive(Clone, Debug)]
pub struct ApplyPlan {
    pub target: AgentTarget,
    pub routing_mode: RoutingMode,
    pub protocol_adapter: Option<ProtocolAdapter>,
    pub patches: Vec<ConfigPatch>,
    pub summary: Vec<String>,
    pub warnings: Vec<String>,
}

pub trait FileAdapter {
    fn spec(&self, home: &Path) -> AgentConfigSpec;
    fn build_plan(&self, home: &Path, providers: &[&ProviderProfile]) -> Result<ApplyPlan, String>;
}

pub fn apply_agent(
    home: &Path,
    target: AgentTarget,
    providers: &[ProviderProfile],
    provider_ids: &[String],
) -> Result<ApplyPlan, String> {
    let selected = select_providers(providers, provider_ids)?;
    match target {
        AgentTarget::OpenCode => opencode::OpenCodeAdapter.build_plan(home, &selected),
        AgentTarget::ClaudeCode => claude::ClaudeCodeAdapter.build_plan(home, &selected),
        AgentTarget::Codex => codex::CodexAdapter.build_plan(home, &selected),
        AgentTarget::Other => Err("unsupported target: other".to_string()),
    }
}

pub fn agent_spec(home: &Path, target: AgentTarget) -> AgentConfigSpec {
    match target {
        AgentTarget::OpenCode => opencode::OpenCodeAdapter.spec(home),
        AgentTarget::ClaudeCode => claude::ClaudeCodeAdapter.spec(home),
        AgentTarget::Codex => codex::CodexAdapter.spec(home),
        AgentTarget::Other => AgentConfigSpec {
            target,
            config_files: Vec::new(),
            native_protocol: ProtocolKind::OpenAiChat,
            supports_direct_file: false,
            supports_local_routing: false,
        },
    }
}

fn select_providers<'a>(
    providers: &'a [ProviderProfile],
    provider_ids: &[String],
) -> Result<Vec<&'a ProviderProfile>, String> {
    if provider_ids.is_empty() {
        return Err("select at least one provider".to_string());
    }

    let mut selected = Vec::new();
    for id in provider_ids {
        let provider = providers
            .iter()
            .find(|p| &p.id == id)
            .ok_or_else(|| format!("unknown provider: {id}"))?;
        provider.validate_for_write()?;
        selected.push(provider);
    }
    Ok(selected)
}

pub(super) fn json_patch(path: PathBuf, value: serde_json::Value) -> Result<ConfigPatch, String> {
    let before = read_before_checked(&path)?;
    let after = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())? + "\n";
    Ok(ConfigPatch::new(path, before, after))
}

pub(super) fn sanitize_toml_key(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "xu_provider".to_string()
    } else {
        out
    }
}
