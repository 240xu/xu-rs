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

pub fn run_agent_install_from_tui(statuses: &[AgentToolStatus], index: usize) {
    let Some(row) = statuses.get(index) else {
        eprintln!("没有选中的客户端。");
        return;
    };
    let before = row.current_version.clone();
    let target = row.latest_version.clone();
    println!(
        "开始重装 {}：{}",
        row.tool.label,
        trivium::agent_tools::version_transition_text(before.as_deref(), target.as_deref())
    );
    match trivium::agent_tools::install_or_update(&config::home(), row.tool) {
        Ok(output) => {
            println!("{output}");
            let after =
                trivium::agent_tools::tool_status(&config::home(), row.tool, false).current_version;
            println!(
                "完成 {}：{}",
                row.tool.label,
                trivium::agent_tools::version_transition_text(before.as_deref(), after.as_deref())
            );
        }
        Err(error) => eprintln!("{} 安装/更新失败：{error}", row.tool.label),
    }
}

pub fn run_all_agent_install_from_tui() {
    let tools = trivium::agent_tools::updatable_client_tools();
    println!(
        "{}",
        trivium::agent_tools::preview_update_plan(&config::home(), &tools, true)
    );
    println!("开始执行 OpenCode / Claude / Codex 重装检测…");
    match trivium::agent_tools::install_or_update_tools(&config::home(), &tools) {
        Ok(output) => {
            println!("{output}");
            println!("全部完成。");
        }
        Err(error) => eprintln!("批量更新未完全成功：\n{error}"),
    }
}
