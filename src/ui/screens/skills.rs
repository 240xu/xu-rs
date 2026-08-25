//! Skills 页（v3 前端层 · 自 menu/mod.rs 物理迁入）
//! 渲染与命中同文件同源。

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::screens::extension::{
    extension_visible_cards, render_extension_cards, render_toolbar, ExtensionRow,
    EXTENSION_CARD_HEIGHT,
};
use spec::domain::AgentTarget;

use crate::ui::screens::chrome::list_window_rects;
use crate::ui::screens::chrome::{render_sync_target_chips, sync_target_chips_hit_test};
#[allow(unused_imports)]
use crate::ui::screens::extension::{
    extension_card_hit_test, extension_toolbar_hit_test, ExtensionCardHit,
};
use crate::ui::widgets::chips::{toolbar_rows, TOOL_CHIP_GAP, TOOL_CHIP_HEIGHT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillMouseAction {
    ImportLocal,
    InstallZip,
    InstallGithub,
    Update,
    Uninstall,
    Backups,
    Verify,
    Toggle(usize, AgentTarget),
    Select(usize),
    /// 底部 →OC/→CC/→CX 定向同步 chip 命中。
    SyncTarget(AgentTarget),
}

/// Skills 页 Frame 级绘制（与 TestBackend 测试共用）。message 非空时替换页头第二行提示。
pub fn draw_skills_frame(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    skills: &[spec::skills::SkillRecord],
    selected: usize,
    error: Option<&str>,
    message: &str,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(skill_header_height(area.width)),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
    f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Skills 技能",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    if message.is_empty() {
                        "指令包 · 本地/压缩包/仓库安装 · 更新 · 卸载 · 校验 · 底部 O/C/X 定向同步（源=OpenCode）"
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
            ("local", Color::Cyan),
            ("zip", Color::Cyan),
            ("gh", Color::Cyan),
            ("upd", Color::Green),
            ("uninst", Color::Red),
            ("bak", Color::Gray),
            ("chk", Color::DarkGray),
        ],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("{} skills", skills.len()),
            Style::default().fg(Color::DarkGray),
        )),
        Rect::new(area.x, area.y + 6, area.width, 1),
    );
    if let Some(error) = error {
        f.render_widget(
            Paragraph::new(error).style(Style::default().fg(Color::Red)),
            chunks[1],
        );
    } else if skills.is_empty() {
        f.render_widget(
            Paragraph::new(
                "暂无 Skill\n\n按 i 本地导入 · 按 z 压缩包 · 按 g 从仓库安装；目录需含根级 SKILL.md。",
            )
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Gray)),
            chunks[1],
        );
    } else {
        let rows = skills
            .iter()
            .map(|skill| {
                let origin = match &skill.origin {
                    Some(spec::skills::SkillOrigin::Local { source }) => {
                        format!("本地 · {source}")
                    }
                    Some(spec::skills::SkillOrigin::Zip { source }) => {
                        format!("压缩包 · {source}")
                    }
                    Some(spec::skills::SkillOrigin::GitHub {
                        owner,
                        repo,
                        branch,
                        subdir,
                    }) => format!(
                        "仓库 · {owner}/{repo}@{branch}{}",
                        subdir
                            .as_ref()
                            .map(|value| format!("/{value}"))
                            .unwrap_or_default()
                    ),
                    None => "来源 · 未知".to_string(),
                };
                ExtensionRow::new(
                    &skill.name,
                    vec![Span::styled(
                        format!("  ·  {:#?}", skill.sync_method),
                        Style::default().fg(Color::DarkGray),
                    )],
                    Line::from(Span::styled(
                        format!("校验 {} · {origin}", &skill.sha256[..12]),
                        Style::default().fg(Color::Gray),
                    )),
                    [
                        skill.targets.opencode,
                        skill.targets.claude,
                        skill.targets.codex,
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

pub fn skill_mouse_action(
    area: Rect,
    selected: usize,
    len: usize,
    m: &MouseEvent,
) -> Option<SkillMouseAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    if let Some(button) = extension_toolbar_hit_test(area, 7, m) {
        return Some(match button {
            0 => SkillMouseAction::ImportLocal,
            1 => SkillMouseAction::InstallZip,
            2 => SkillMouseAction::InstallGithub,
            3 => SkillMouseAction::Update,
            4 => SkillMouseAction::Uninstall,
            5 => SkillMouseAction::Backups,
            6 => SkillMouseAction::Verify,
            _ => unreachable!("toolbar hit index within button count"),
        });
    }
    if let Some(target) = sync_target_chips_hit_test(area, m) {
        return Some(SkillMouseAction::SyncTarget(target));
    }
    match extension_card_hit_test(area, skill_header_height(area.width), len, selected, m)? {
        ExtensionCardHit::Toggle(index, target) => Some(SkillMouseAction::Toggle(index, target)),
        ExtensionCardHit::Select(index) => Some(SkillMouseAction::Select(index)),
    }
}

pub fn skill_header_height(width: u16) -> u16 {
    2 + toolbar_rows(width, 7) as u16 * (TOOL_CHIP_HEIGHT + TOOL_CHIP_GAP)
}
