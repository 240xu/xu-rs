//! 帮助页 / OpenCode 设置页（v3 前端层 · 自 menu/mod.rs 物理迁入）

use std::io;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::Tui;
use crate::ui::screens::common::render_placeholder;
use crate::ui::widgets::chips::{icon_button_rect, point_in, IconButton};

pub fn render_help(terminal: &mut Tui) -> io::Result<()> {
    render_placeholder(
        terminal,
        "帮助 / Help",
        "§ 使用流程\n1. 全新 Termux 先到「Agent」页做环境诊断，再更新/安装 OpenCode、Claude Code、Codex。\n2. 「供应商」页用模板或手动添加 provider，点测试/模型确认连接。\n3. 在供应商详情页确认应用，把供应商写入对应端配置（单选或路由多选）。\n\n§ 主要入口\n供应商：完整管理 —— 模板、添加、编辑、测试、模型拉取、刷新、应用到三端。\nAgent：更新 / 安装 / 状态 —— 自动查询最新版，诊断、批量更新、OpenCode 设置。\nMCP：管理 + 定向同步 —— 三端 MCP 服务投影开关；底部 O/C/X 按端定向同步。\nSkills：管理 + 定向同步 —— 本地/压缩包/仓库安装、更新、卸载、校验；底部 O/C/X 按端定向同步。\n每端配色：O=OpenCode 青 · C=Claude 品红 · X=Codex 黄。\n\n\n§ 供应商详情页快捷键\nTab/←→ 切三端 · p 完全权限 · h 会话标题 · r 路由/生效 · a 生效=应用到当前端\nm 获取模型 · s 模型多选保存 · g 进入路由多选 · e 编辑 · t 调试 · d 删除\nv 验证 · y 密钥明文 · 1/2/3 应用三端 · Esc 返回\n\n§ CLI\nspec agent doctor\nspec agent setup --yes\nspec agent status --latest\nspec agent install claude --yes\nspec provider preset list\nspec provider add <id> --preset openrouter --api-key <key>\nspec provider models <id>\nspec provider add/update/show/delete\nspec use <id> --target opencode --dry-run\nspec mcp apply [--target all] [--yes]\nspec sync mcp opencode claude\nspec sync skills opencode claude\nspec serve\n\n§ 边界\nOpenCode 只写自己的 opencode.json。\nClaude 原生 Anthropic；Codex 原生 OpenAI Responses。\n工具调用和跨协议 streaming 仍明确拒绝。\n\n按 Esc 或 q 返回主菜单。",
    )
}

pub fn render_opencode_settings(
    terminal: &mut Tui,
    permission: &str,
    message: Option<&str>,
) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Length(7),
                Constraint::Min(1),
                Constraint::Length(4),
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("OpenCode 专属设置", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Line::from("配置：~/.config/opencode/opencode.json"),
            ]),
            chunks[0],
        );
        f.render_widget(
            IconButton {
                label: "back",
                color: Color::Red,
                active: false,
            },
            Rect::new(chunks[0].x, chunks[0].y, 5, 5),
        );
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("全局工具权限", Style::default().fg(Color::Gray))),
            ])
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)))
            .wrap(Wrap { trim: true }),
            chunks[1],
        );
        for (index, (icon, color, active)) in [
            ("allow", Color::Green, permission == "allow"),
            ("ask", Color::Gray, permission == "ask"),
            ("deny", Color::Red, permission == "deny"),
        ]
        .into_iter()
        .enumerate()
        {
            f.render_widget(
                IconButton {
                    label: icon,
                    color,
                    active,
                },
                icon_button_rect(chunks[1].x + 1, chunks[1].y + 1, index),
            );
        }
        if let Some(message) = message {
            f.render_widget(Paragraph::new(message).wrap(Wrap { trim: true }), chunks[2]);
        }
        f.render_widget(
            Paragraph::new("灰色说明：allow 允许工具直接执行；ask 由 OpenCode 询问；deny 禁止工具。点击后先显示 dry-run，再确认写入。")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::TOP))
                .wrap(Wrap { trim: true }),
            chunks[3],
        );
    })?;
    Ok(())
}

pub fn opencode_settings_mouse_action(area: Rect, m: &MouseEvent) -> Option<&'static str> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Min(1),
            Constraint::Length(4),
        ])
        .split(area);
    if point_in(Rect::new(chunks[0].x, chunks[0].y, 5, 5), m.column, m.row) {
        return Some("back");
    }
    for (index, action) in ["allow", "ask", "deny"].into_iter().enumerate() {
        if point_in(
            icon_button_rect(chunks[1].x + 1, chunks[1].y + 1, index),
            m.column,
            m.row,
        ) {
            return Some(action);
        }
    }
    None
}
