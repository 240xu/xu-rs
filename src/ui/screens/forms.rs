//! Provider 新增表单 / 模板列表（v3 前端层 · 自 menu/mod.rs 物理迁入）

use std::io;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::Tui;
use crate::ui::screens::chrome::list_window_rects;
use crate::ui::theme::PANEL_BG;
use crate::ui::widgets::chips::{icon_button_rect, point_in, IconButton, ICON_BUTTON_WIDTH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFormAction {
    Cancel,
    Preview,
    Field(usize),
}

pub fn text_form_rects(area: Rect, fields: usize) -> Vec<Rect> {
    let content = Rect::new(
        area.x,
        area.y.saturating_add(5),
        area.width,
        area.height.saturating_sub(8),
    );
    Layout::default()
        .direction(Direction::Vertical)
        .constraints((0..fields).map(|_| Constraint::Length(5)))
        .split(content)
        .to_vec()
}

pub fn render_provider_add_form(
    terminal: &mut Tui,
    fields: &[(usize, String, String, bool)],
    selected: usize,
    error: Option<&str>,
) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "添加供应商",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "逐字段输入 provider；id 留空自动生成。",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            Rect::new(
                area.x.saturating_add(12),
                area.y,
                area.width.saturating_sub(12),
                2,
            ),
        );
        for (index, (icon, color)) in [("back", Color::DarkGray), ("ok", Color::Cyan)]
            .into_iter()
            .enumerate()
        {
            f.render_widget(
                IconButton {
                    label: icon,
                    color,
                    active: false,
                },
                Rect::new(
                    icon_button_rect(area.x, area.y, index).x,
                    icon_button_rect(area.x, area.y, index).y,
                    ICON_BUTTON_WIDTH,
                    1,
                ),
            );
        }
        let rects = text_form_rects(area, fields.len());
        for (index, ((_, label, value, adjustable), rect)) in fields.iter().zip(rects).enumerate() {
            let active = index == selected;
            let display = if value.is_empty() { "<empty>" } else { value };
            let hint = if *adjustable {
                "← → 或 1/2/3 切换".to_string()
            } else {
                "点击后直接输入".to_string()
            };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        label.to_string(),
                        Style::default().fg(if active { Color::Cyan } else { Color::Gray }),
                    )),
                    Line::from(Span::styled(display, Style::default().fg(Color::White))),
                    Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))),
                ])
                .block(Block::default().borders(Borders::ALL).border_style(
                    Style::default().fg(if active { Color::Cyan } else { Color::DarkGray }),
                ))
                .wrap(Wrap { trim: false }),
                rect,
            );
        }
        let message = error.unwrap_or("回车下一项/提交 · ↑↓/Tab 切换字段 · 点字段聚焦 · Esc 取消");
        f.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(if error.is_some() {
                    Color::Red
                } else {
                    Color::DarkGray
                }))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::TOP)),
            Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 3),
        );
    })?;
    Ok(())
}

pub fn provider_preset_list_hit_test(area: Rect, selected: usize, m: &MouseEvent) -> Option<usize> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let presets = trivium::provider_presets::all();
    let content = Rect::new(
        area.x,
        area.y + 3,
        area.width,
        area.height.saturating_sub(3 + 3),
    );
    list_window_rects(content, presets.len(), selected, 7, 4)
        .into_iter()
        .find_map(|(index, rect)| point_in(rect, m.column, m.row).then_some(index))
}

pub fn render_provider_preset_list(terminal: &mut Tui, selected: usize) -> io::Result<()> {
    let presets = trivium::provider_presets::all();
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
                    "供应商模板",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "点击模板预填添加表单；id 留空将按名称自动生成。",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            chunks[0],
        );
        for (index, rect) in list_window_rects(chunks[1], presets.len(), selected, 7, 4) {
            let preset = &presets[index];
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
                        preset.base_url,
                        Style::default().fg(Color::Gray),
                    )),
                    Line::from(Span::styled(
                        preset.models.join(", "),
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
        let message = if presets.len() > 4 {
            format!(
                "↑↓/点击选择 · 回车使用 · Esc 返回 · {}/{}/{}",
                selected.saturating_add(1),
                presets.len(),
                selected.saturating_div(4).saturating_add(1)
            )
        } else {
            "↑↓/点击选择 · 回车使用 · Esc 返回".to_string()
        };
        f.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            chunks[2],
        );
    })?;
    Ok(())
}
