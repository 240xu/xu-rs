//! Agents 页（v3 前端层 · 自 menu/mod.rs 物理迁入）
//! 渲染与命中同文件同源。

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use spec::agent_tools::AgentToolStatus;

use crate::ui::screens::chrome::list_window_rects;
use crate::ui::theme;
use crate::ui::widgets::chips::{
    point_in, toolbar_rects, toolbar_rows, IconButton, ICON_BUTTON_HEIGHT, ICON_BUTTON_WIDTH,
    TOOL_CHIP_GAP, TOOL_CHIP_HEIGHT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMouseAction {
    Refresh,
    Latest,
    Doctor,
    Setup,
    Install(usize),
    OpenCodeSettings,
}

pub const AGENT_HEIGHT: usize = 8;

/// 运行时页：/health 状态 + 端点清单 + 启停。
/// Agent 是否有可更新版本（已安装且最新版更高）。
pub fn agent_has_update(status: &AgentToolStatus) -> bool {
    match (
        status.current_version.as_deref(),
        status.latest_version.as_deref(),
    ) {
        (Some(current), Some(latest)) => spec::agent_tools::compare_versions(current, latest) < 0,
        _ => false,
    }
}

/// Agent 页 Frame 级绘制（与 TestBackend 测试共用）。
pub fn draw_agent_targets_frame(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    selected: usize,
    statuses: &[AgentToolStatus],
    message: &str,
    pending_update: Option<usize>,
    pending_setup: bool,
) {
    let has_update = statuses.iter().any(agent_has_update);
    let mut chips: Vec<(&str, Color, bool)> = vec![
        ("ref", Color::Cyan, false),
        ("ver", Color::DarkGray, false),
        ("doc", Color::DarkGray, false),
    ];
    if has_update {
        chips.push(("all", Color::Cyan, pending_setup));
    }
    chips.push(("set", Color::DarkGray, false));
    let header = agent_header_height(area.width, chips.len());
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(Span::styled(
            "客户端",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let toolbar_rects = agent_toolbar_rects(area, chips.len());
    for (index, (icon, color, active)) in chips.into_iter().enumerate() {
        f.render_widget(
            IconButton {
                label: icon,
                color: if active { Color::Yellow } else { color },
                active,
            },
            toolbar_rects[index],
        );
    }
    let updates = statuses.iter().filter(|row| agent_has_update(row)).count();
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("{} 客户端 · {} 可更新", statuses.len(), updates),
            Style::default().fg(Color::DarkGray),
        )),
        Rect::new(area.x, area.y + 5, area.width, 1),
    );

    // 矮终端防压缩：只显示放得下的整卡（与扩展卡同策略）。
    let visible_agents = (chunks[1].height as usize / AGENT_HEIGHT).max(1);
    let card_rects = list_window_rects(
        chunks[1],
        statuses.len(),
        selected,
        AGENT_HEIGHT as u16,
        visible_agents,
    );

    // 空态/加载中占位：不再留 19 行空白让用户以为应用坏了（P5）。
    if statuses.is_empty() {
        let placeholder = if message.is_empty() {
            "正在检测客户端状态…"
        } else {
            message
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                placeholder,
                Style::default().fg(Color::DarkGray),
            ))
            .alignment(Alignment::Center),
            chunks[1],
        );
    }

    for (idx, rect) in card_rects {
        let row = &statuses[idx];
        let active = idx == selected;
        let pending = pending_update == Some(idx);
        // 克制派：无边框，选中/待确认用 ▍竖条 + 面板明度表达。
        let bar_color = if pending {
            Color::Yellow
        } else if active {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        let current = row.current_version.as_deref().unwrap_or("未安装");
        let version_color = agent_version_line_color(
            row.current_version.as_deref(),
            row.latest_version.as_deref(),
        );
        let bar = if active || pending {
            Span::styled("▍", Style::default().fg(bar_color))
        } else {
            Span::raw(" ")
        };
        let widget = Paragraph::new(vec![
            Line::from(vec![
                bar,
                Span::styled(
                    format!(" {} ", idx + 1),
                    Style::default().fg(if active { Color::Cyan } else { Color::DarkGray }),
                ),
                Span::raw(" "),
                Span::styled(
                    if pending {
                        format!("{} · 再点确认", row.tool.label)
                    } else {
                        row.tool.label.to_string()
                    },
                    Style::default()
                        .fg(if pending {
                            Color::Yellow
                        } else if active {
                            Color::Cyan
                        } else {
                            Color::White
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(format!("当前 {current}"), Style::default().fg(Color::Gray)),
            ]),
            Line::from(Span::styled(
                agent_version_line_text(
                    row.current_version.as_deref(),
                    row.latest_version.as_deref(),
                ),
                Style::default().fg(version_color),
            )),
            Line::from(Span::styled(
                format!(
                    "命令：{} · 状态：{}",
                    row.command_path.as_deref().unwrap_or(row.tool.command),
                    row.note
                ),
                Style::default().fg(Color::Gray),
            )),
        ])
        .style(if active || pending {
            Style::default().bg(theme::PANEL_BG)
        } else {
            Style::default()
        })
        .wrap(Wrap { trim: true });
        f.render_widget(widget, rect);
        let (button_label, button_color) = agent_card_button_spec(
            row.ready,
            row.current_version.as_deref(),
            row.latest_version.as_deref(),
        );
        f.render_widget(
            IconButton {
                label: button_label,
                color: button_color,
                active,
            },
            Rect::new(
                rect.right().saturating_sub(ICON_BUTTON_WIDTH + 1),
                rect.y + 1,
                ICON_BUTTON_WIDTH,
                ICON_BUTTON_HEIGHT,
            ),
        );
    }

    let hint_text = if message.is_empty() {
        if has_update {
            "进入已自动查最新版 · 点一次选中 · 再点确认更新 · 顶栏：刷新 / 检查更新 / 诊断 / 全部更新 / 设置"
        } else {
            "进入已自动查最新版 · 点一次选中 · 再点确认更新 · 顶栏：刷新 / 检查更新 / 诊断 / 设置"
        }
    } else {
        message
    };
    let hint = Paragraph::new(Line::from(Span::styled(
        hint_text,
        Style::default().fg(Color::DarkGray),
    )))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
    .alignment(Alignment::Center);
    f.render_widget(hint, chunks[2]);
}

pub fn agent_mouse_action(
    area: Rect,
    statuses: &[AgentToolStatus],
    m: &MouseEvent,
) -> Option<AgentMouseAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let len = statuses.len();
    let has_update = statuses.iter().any(agent_has_update);
    let mut actions = vec![
        AgentMouseAction::Refresh,
        AgentMouseAction::Latest,
        AgentMouseAction::Doctor,
    ];
    if has_update {
        actions.push(AgentMouseAction::Setup);
    }
    actions.push(AgentMouseAction::OpenCodeSettings);
    let chip_count = actions.len();
    let rects = agent_toolbar_rects(area, chip_count);
    let row = m.row as usize;
    for (index, action) in actions.into_iter().enumerate() {
        if point_in(rects[index], m.column, m.row) {
            return Some(action);
        }
    }
    if len == 0 || row < agent_header_height(area.width, chip_count) as usize {
        return None;
    }
    let header = agent_header_height(area.width, chip_count);
    let content = Rect::new(
        area.x,
        area.y + header,
        area.width,
        area.height.saturating_sub(header + 3),
    );
    let visible = (content.height as usize / AGENT_HEIGHT).max(1);
    list_window_rects(content, len, 0, AGENT_HEIGHT as u16, visible)
        .into_iter()
        .find_map(|(idx, rect)| {
            let hit = point_in(rect, m.column, m.row);
            if hit && m.row < rect.y + 6 {
                Some(AgentMouseAction::Install(idx))
            } else {
                None
            }
        })
}

pub fn agent_header_height(width: u16, chip_count: usize) -> u16 {
    2 + toolbar_rows(width, chip_count) as u16 * (TOOL_CHIP_HEIGHT + TOOL_CHIP_GAP)
}

/// 行内动作按钮三态：未安装 →「安装」；有更新 →「更新」；已最新 →「最新」；
/// 已安装但最新版本未知 →「检查更新」。返回 (按钮标签, 颜色)。
pub fn agent_card_button_spec(
    ready: bool,
    current: Option<&str>,
    latest: Option<&str>,
) -> (&'static str, Color) {
    if !ready {
        return ("+", Color::Cyan);
    }
    match (current, latest) {
        (Some(current), Some(latest))
            if spec::agent_tools::compare_versions(current, latest) < 0 =>
        {
            ("upd", Color::Green)
        }
        (Some(_), Some(_)) => ("new", Color::DarkGray),
        (Some(_), None) => ("ver", Color::DarkGray),
        (None, _) => ("+", Color::Cyan),
    }
}

/// 变更行配色：有更新=黄；已最新=绿；未知=灰。
pub fn agent_version_line_color(current: Option<&str>, latest: Option<&str>) -> Color {
    match (current, latest) {
        (Some(current), Some(latest))
            if spec::agent_tools::compare_versions(current, latest) < 0 =>
        {
            Color::Yellow
        }
        (Some(_), Some(_)) => Color::Green,
        _ => Color::Gray,
    }
}

/// 变更行文字：未安装 / 有更新（旧 → 新）/ 已最新 / 已装但未查最新。
pub fn agent_version_line_text(current: Option<&str>, latest: Option<&str>) -> String {
    match (current, latest) {
        (None, Some(latest)) => format!("状态：未安装 → 将安装 {latest}"),
        (None, None) => "状态：未安装 → 将查询并安装最新版".to_string(),
        (Some(current), Some(latest))
            if spec::agent_tools::compare_versions(current, latest) < 0 =>
        {
            format!("更新：{current} → {latest}")
        }
        (Some(current), Some(_)) => format!("已是最新：{current}"),
        (Some(current), None) => format!("已安装：{current} · 点「检查更新」"),
    }
}

pub fn agent_toolbar_rects(area: Rect, chip_count: usize) -> Vec<Rect> {
    toolbar_rects(area.x, area.y + 1, area.width, chip_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::agent_tools::AgentToolStatus;

    fn status(current: Option<&str>, latest: Option<&str>) -> AgentToolStatus {
        AgentToolStatus {
            tool: spec::agent_tools::AGENT_TOOLS[0],
            command_path: None,
            current_version: current.map(|s| s.to_string()),
            latest_version: latest.map(|s| s.to_string()),
            ready: true,
            note: String::new(),
        }
    }

    #[test]
    fn agent_card_button_spec_follows_update_state() {
        // 未安装=+；有更新=upd；已最新=new；未知版本=ver（方向 C 紧凑词）
        assert_eq!(agent_card_button_spec(false, None, None).0, "+");
        let (label, color) = agent_card_button_spec(true, Some("1.0"), Some("2.0"));
        assert_eq!(label, "upd");
        assert_eq!(color, Color::Green);
        assert_eq!(
            agent_card_button_spec(true, Some("2.0"), Some("2.0")).0,
            "new"
        );
        assert_eq!(agent_card_button_spec(true, Some("1.0"), None).0, "ver");
        assert_eq!(agent_card_button_spec(true, None, Some("9.9")).0, "+");
    }

    #[test]
    fn agent_version_line_color_follows_state() {
        assert_eq!(
            agent_version_line_color(Some("1.0"), Some("2.0")),
            Color::Yellow
        );
        assert_eq!(
            agent_version_line_color(Some("2.0"), Some("2.0")),
            Color::Green
        );
        assert_eq!(agent_version_line_color(Some("1.0"), None), Color::Gray);
        assert_eq!(agent_version_line_color(None, None), Color::Gray);
    }

    #[test]
    fn agent_version_line_text_follows_state() {
        assert!(agent_version_line_text(None, None).contains("未安装"));
        let upgrade = agent_version_line_text(Some("1.0"), Some("2.0"));
        assert!(upgrade.contains("1.0") && upgrade.contains("2.0"));
        assert!(agent_version_line_text(Some("2.0"), Some("2.0")).contains("已是最新"));
        assert!(agent_version_line_text(Some("1.0"), None).contains("检查更新"));
    }

    #[test]
    fn agent_has_update_matches_status_pairs() {
        assert!(agent_has_update(&status(Some("1.0"), Some("2.0"))));
        assert!(!agent_has_update(&status(Some("2.0"), Some("2.0"))));
        assert!(!agent_has_update(&status(Some("2.0"), None)));
    }
}
