use crate::config;
use trivium::agent_tools::AgentToolStatus;

pub fn ensure_agent_versions_for_install(
    home: &std::path::Path,
    mut statuses: Vec<AgentToolStatus>,
    index: usize,
) -> Vec<AgentToolStatus> {
    if let Some(row) = statuses.get(index) {
        if row.latest_version.is_none() {
            statuses[index] = trivium::agent_tools::tool_status(home, row.tool, true);
        }
    }
    statuses
}

pub fn ensure_agent_versions_for_clients(
    home: &std::path::Path,
    statuses: Vec<AgentToolStatus>,
) -> Vec<AgentToolStatus> {
    statuses
        .into_iter()
        .map(|row| {
            if row.latest_version.is_none() {
                trivium::agent_tools::tool_status(home, row.tool, true)
            } else {
                row
            }
        })
        .collect()
}

/// 单客户端安装/更新，返回一行摘要供 TUI hint 展示（不再退出 TUI）。
pub fn agent_install_summary(statuses: &[AgentToolStatus], index: usize) -> String {
    let Some(row) = statuses.get(index) else {
        return "没有选中的客户端。".to_string();
    };
    let before = row.current_version.clone();
    let target = row.latest_version.clone();
    match trivium::agent_tools::install_or_update(&config::home(), row.tool) {
        Ok(_output) => {
            let after =
                trivium::agent_tools::tool_status(&config::home(), row.tool, false).current_version;
            format!(
                "✓ {} 完成：{}（目标 {}）",
                row.tool.label,
                trivium::agent_tools::version_transition_text(
                    before.as_deref(),
                    after.as_deref()
                ),
                target.as_deref().unwrap_or("?"),
            )
        }
        Err(error) => format!(
            "✗ {} 安装/更新失败：{}",
            row.tool.label,
            last_meaningful_line(&error),
        ),
    }
}

/// 全部客户端安装/更新（批量），返回一行摘要。
pub fn agent_setup_summary() -> String {
    let tools = trivium::agent_tools::updatable_client_tools();
    match trivium::agent_tools::install_or_update_tools(&config::home(), &tools) {
        Ok(_) => "✓ 全部客户端安装/更新流程执行完成（详细日志请用 spec agent setup 查看）".to_string(),
        Err(error) => format!("✗ 批量更新未完全成功：{}", last_meaningful_line(&error)),
    }
}

/// 错误串通常内嵌完整日志，取最后一行非空内容做摘要。
fn last_meaningful_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("未知错误")
        .to_string()
}


