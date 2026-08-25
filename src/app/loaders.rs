use std::collections::BTreeMap;

use crate::config;
use crate::ProviderState;
use spec::agents::AgentTarget;

pub fn load_mcp_servers() -> (Vec<spec::mcp::McpServer>, Option<String>) {
    match spec::mcp::read_store(&config::home()) {
        Ok(store) => (store.servers.into_values().collect(), None),
        Err(error) => (Vec::new(), Some(error)),
    }
}

pub fn load_skills() -> (Vec<spec::skills::SkillRecord>, Option<String>) {
    match spec::skills::read_store(&config::home()) {
        Ok(store) => (store.skills.into_values().collect(), None),
        Err(error) => (Vec::new(), Some(error)),
    }
}

pub fn load_providers() -> ProviderState {
    load_providers_with(None)
}

/// 重新加载供应商列表。`prefer_selected` 为 Some(old) 时尽量保留选中项
/// （id 消失则退回旧 index 的 clamp）；None 时选中回 0（初始加载/切换）。
pub fn load_providers_with(prefer_selected: Option<ProviderState>) -> ProviderState {
    let path = config::providers_path();
    let sync_notice = Some("OpenCode 自动导入已关闭；使用同步操作预览后显式执行".to_string());
    let (identity, fallback) = prefer_selected
        .as_ref()
        .map(|state| {
            (
                state.providers.get(state.selected).map(|p| p.id.clone()),
                state.selected,
            )
        })
        .unwrap_or((None, 0));
    let mut base = match spec::providers::read_profiles(&path) {
        Ok(providers) => {
            let state = spec::state::read_state(&config::home()).unwrap_or_default();
            ProviderState {
                providers,
                error: None,
                sync_notice,
                selected: 0,
                current: state.current,
                health: state.provider_health,
            }
        }
        Err(error) => ProviderState {
            providers: Vec::new(),
            error: Some(format!("{}: {error}", path.display())),
            sync_notice,
            selected: 0,
            current: BTreeMap::new(),
            health: BTreeMap::new(),
        },
    };
    // 依 id 重定位保留的选中项；id 消失→退回旧 index 的 clamp。
    base.selected = match &identity {
        Some(id) => base
            .providers
            .iter()
            .position(|p| &p.id == id)
            .unwrap_or_else(|| fallback.min(base.providers.len().saturating_sub(1))),
        None => 0,
    };
    base
}

/// 依当前 state 重载并保留选中项（保存/刷新后不跳回第一个）。
pub fn reload_providers_keep(state: &ProviderState) -> ProviderState {
    load_providers_with(Some(state.clone()))
}

pub fn run_skill_command(home: &std::path::Path, args: &[String]) -> String {
    match spec::cli::run_command(home, args) {
        Some(Ok(output)) => output,
        Some(Err(error)) => format!("Skills 操作失败：{error}"),
        None => "Skills 命令不可用".to_string(),
    }
}

pub fn run_mcp_command(home: &std::path::Path, args: &[String]) -> String {
    match spec::cli::run_command(home, args) {
        Some(Ok(output)) => output,
        Some(Err(error)) => format!("MCP 操作失败：{error}"),
        None => "MCP 命令不可用".to_string(),
    }
}

pub fn skill_toggle_args(skill: &spec::skills::SkillRecord, target: AgentTarget) -> Vec<String> {
    vec![
        "skill".to_string(),
        if skill.targets.get(target) {
            "disable".to_string()
        } else {
            "enable".to_string()
        },
        skill.id.clone(),
        "--target".to_string(),
        target.id().to_string(),
    ]
}

pub fn mcp_toggle_args(server: &spec::mcp::McpServer, target: AgentTarget) -> Vec<String> {
    vec![
        "mcp".to_string(),
        if server.targets.get(target) {
            "disable".to_string()
        } else {
            "enable".to_string()
        },
        server.id.clone(),
        "--target".to_string(),
        target.id().to_string(),
    ]
}

pub fn mcp_preset_args(index: usize) -> Vec<String> {
    let Some(preset) = spec::mcp::MCP_PRESETS.get(index) else {
        return Vec::new();
    };
    vec![
        "mcp".to_string(),
        "preset".to_string(),
        preset.id.to_string(),
    ]
}
