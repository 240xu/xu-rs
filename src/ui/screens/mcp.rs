//! MCP 页（v3 前端层 · 自 menu/mod.rs 物理迁入）
//! 渲染与命中同文件同源。

use std::io;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use spec::domain::AgentTarget;
use spec::mcp::{McpServer, McpTransport};

use crate::tui::Tui;
use crate::ui::screens::chrome::list_window_rects;
use crate::ui::screens::chrome::{render_sync_target_chips, sync_target_chips_hit_test};
#[allow(unused_imports)]
use crate::ui::screens::extension::{
    extension_card_hit_test, extension_toolbar_hit_test, ExtensionCardHit,
};
use crate::ui::screens::extension::{
    extension_visible_cards, render_extension_cards, render_toolbar, ExtensionRow,
    EXTENSION_CARD_HEIGHT,
};
use crate::ui::theme::PANEL_BG;
use crate::ui::widgets::chips::{point_in, toolbar_rows, TOOL_CHIP_GAP, TOOL_CHIP_HEIGHT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpMouseAction {
    Add,
    Presets,
    ImportAll,
    Edit,
    Delete,
    Toggle(usize, AgentTarget),
    Select(usize),
    /// 底部 →OC/→CC/→CX 定向同步 chip 命中。
    SyncTarget(AgentTarget),
}

/// MCP 页 Frame 级绘制（与 TestBackend 测试共用）。message 非空时替换页头第二行提示。
pub fn draw_mcp_frame(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    servers: &[McpServer],
    selected: usize,
    error: Option<&str>,
    message: &str,
) {
    let header = mcp_header_height(area.width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "MCP 服务",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                if message.is_empty() {
                    "连接工具 · 点卡片内 O/C/X 切换三端投影 · 底部 O/C/X 定向同步（源=OpenCode）"
                } else {
                    message
                },
                Style::default().fg(Color::DarkGray),
            )),
        ]),
        Rect::new(area.x, area.y, area.width, 2),
    );
    render_toolbar(
        f,
        area,
        &[
            ("+", Color::Green),
            ("tpl", Color::Cyan),
            ("imp", Color::Cyan),
            ("edit", Color::Cyan),
            ("del", Color::Red),
        ],
    );
    let projections: usize = servers
        .iter()
        .map(|s| s.targets.opencode as usize + s.targets.claude as usize + s.targets.codex as usize)
        .sum();
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("{} servers · {} 投影", servers.len(), projections),
            Style::default().fg(Color::DarkGray),
        )),
        Rect::new(area.x, area.y + 6, area.width, 1),
    );
    if let Some(error) = error {
        f.render_widget(
            Paragraph::new(error)
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true }),
            chunks[1],
        );
    } else if servers.is_empty() {
        f.render_widget(
            Paragraph::new("暂无 MCP 服务\n\n按 a 从模板创建 · 按 i 导入已有配置")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Gray)),
            chunks[1],
        );
    } else {
        let rows = servers
            .iter()
            .map(|server| {
                let endpoint = match server.transport {
                    McpTransport::Stdio => format!(
                        "{} {}",
                        server.command.as_deref().unwrap_or("<missing>"),
                        server.args.join(" ")
                    ),
                    McpTransport::Http | McpTransport::Sse => server
                        .url
                        .clone()
                        .unwrap_or_else(|| "<missing>".to_string()),
                };
                ExtensionRow::new(
                    &server.name,
                    vec![Span::styled(
                        format!("  ·  {:#?}", server.transport),
                        Style::default().fg(Color::DarkGray),
                    )],
                    Line::from(Span::styled(endpoint, Style::default().fg(Color::Gray))),
                    [
                        server.targets.opencode,
                        server.targets.claude,
                        server.targets.codex,
                    ],
                )
            })
            .collect::<Vec<_>>();
        let rects = list_window_rects(
            chunks[1],
            rows.len(),
            selected,
            EXTENSION_CARD_HEIGHT,
            extension_visible_cards(chunks[1]),
        );
        render_extension_cards(f, &rects, &rows, selected);
    }
    render_sync_target_chips(f, chunks[2]);
}

pub fn mcp_mouse_action(
    area: Rect,
    selected: usize,
    len: usize,
    m: &MouseEvent,
) -> Option<McpMouseAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    if let Some(button) = extension_toolbar_hit_test(area, 5, m) {
        return Some(match button {
            0 => McpMouseAction::Add,
            1 => McpMouseAction::Presets,
            2 => McpMouseAction::ImportAll,
            3 => McpMouseAction::Edit,
            4 => McpMouseAction::Delete,
            _ => unreachable!("toolbar hit index within button count"),
        });
    }
    if let Some(target) = sync_target_chips_hit_test(area, m) {
        return Some(McpMouseAction::SyncTarget(target));
    }
    match extension_card_hit_test(area, mcp_header_height(area.width), len, selected, m)? {
        ExtensionCardHit::Toggle(index, target) => Some(McpMouseAction::Toggle(index, target)),
        ExtensionCardHit::Select(index) => Some(McpMouseAction::Select(index)),
    }
}

pub fn mcp_header_height(width: u16) -> u16 {
    2 + toolbar_rows(width, 5) as u16 * (TOOL_CHIP_HEIGHT + TOOL_CHIP_GAP)
}

pub fn render_mcp_presets(terminal: &mut Tui, selected: usize) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "MCP 模板",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "创建后在 MCP 卡片上启用目标客户端。",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            chunks[0],
        );
        let rows = list_window_rects(
            chunks[1],
            spec::mcp::MCP_PRESETS.len(),
            selected,
            5,
            spec::mcp::MCP_PRESETS.len(),
        );
        for (index, rect) in rows {
            let preset = &spec::mcp::MCP_PRESETS[index];
            let active = index == selected;
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        preset.name,
                        Style::default()
                            .fg(if active { Color::Cyan } else { Color::White })
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        preset.description,
                        Style::default().fg(Color::Gray),
                    )),
                    Line::from(Span::styled(
                        preset.package,
                        Style::default().fg(Color::DarkGray),
                    )),
                ])
                .style(if active {
                    Style::default().bg(PANEL_BG)
                } else {
                    Style::default()
                }),
                rect,
            );
        }
        f.render_widget(
            Paragraph::new("点一次选中 · 再点预览 · Esc 返回")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::TOP)),
            chunks[2],
        );
    })?;
    Ok(())
}

pub fn mcp_preset_hit_test(area: Rect, m: &MouseEvent) -> Option<usize> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let content = Rect::new(
        area.x,
        area.y + 3,
        area.width,
        area.height.saturating_sub(3 + 3),
    );
    list_window_rects(
        content,
        spec::mcp::MCP_PRESETS.len(),
        0,
        5,
        spec::mcp::MCP_PRESETS.len(),
    )
    .into_iter()
    .find_map(|(index, rect)| point_in(rect, m.column, m.row).then_some(index))
}
