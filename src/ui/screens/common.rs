use std::io;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::Tui;
use crate::ui::screens::forms::{text_form_rects, TextFormAction};
use crate::ui::widgets::chips::{icon_button_rect, point_in, IconButton};

pub fn render_text_form(
    terminal: &mut Tui,
    title: &str,
    fields: &[(String, String, bool)],
    selected: usize,
    error: Option<&str>,
) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "字段纵向排列；点击字段后直接输入。只读字段不会被修改。",
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
                icon_button_rect(area.x, area.y, index),
            );
        }
        let rects = text_form_rects(area, fields.len());
        for (index, ((label, value, readonly), rect)) in fields.iter().zip(rects).enumerate() {
            let active = index == selected;
            let display = if value.is_empty() { "<empty>" } else { value };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("{}{}", label, if *readonly { "  [read-only]" } else { "" }),
                        Style::default().fg(if active { Color::Cyan } else { Color::Gray }),
                    )),
                    Line::from(Span::styled(
                        display,
                        Style::default().fg(if *readonly {
                            Color::DarkGray
                        } else {
                            Color::White
                        }),
                    )),
                ])
                .block(Block::default().borders(Borders::ALL).border_style(
                    Style::default().fg(if active { Color::Cyan } else { Color::DarkGray }),
                ))
                .wrap(Wrap { trim: false }),
                rect,
            );
        }
        let message = error.unwrap_or("Tab/上下切换字段 · 回车预览 · 退格删除 · Esc 取消");
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

pub fn text_form_mouse_action(area: Rect, fields: usize, m: &MouseEvent) -> Option<TextFormAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    if point_in(icon_button_rect(area.x, area.y, 0), m.column, m.row) {
        return Some(TextFormAction::Cancel);
    }
    if point_in(icon_button_rect(area.x, area.y, 1), m.column, m.row) {
        return Some(TextFormAction::Preview);
    }
    text_form_rects(area, fields)
        .into_iter()
        .enumerate()
        .find_map(|(index, rect)| {
            point_in(rect, m.column, m.row).then_some(TextFormAction::Field(index))
        })
}

pub fn confirm_mouse_action(m: &MouseEvent) -> Option<bool> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    // bottom-left ok, next cancel
    if point_in(icon_button_rect(0, 0, 0), m.column, m.row) {
        return Some(true);
    }
    if point_in(icon_button_rect(0, 0, 1), m.column, m.row) {
        return Some(false);
    }
    None
}

pub fn render_confirm(terminal: &mut Tui, title: &str, message: &str) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    title,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ]),
            Rect::new(
                area.x.saturating_add(1),
                area.y.saturating_add(1),
                area.width.saturating_sub(2),
                2,
            ),
        );
        f.render_widget(
            IconButton {
                label: "ok",
                color: Color::Green,
                active: false,
            },
            icon_button_rect(area.x, area.y, 0),
        );
        f.render_widget(
            IconButton {
                label: "no",
                color: Color::Red,
                active: false,
            },
            icon_button_rect(area.x, area.y, 1),
        );
        f.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::Gray))
                .wrap(Wrap { trim: false }),
            Rect::new(
                area.x.saturating_add(1),
                area.y.saturating_add(4),
                area.width.saturating_sub(2),
                area.height.saturating_sub(7),
            ),
        );
        f.render_widget(
            Paragraph::new("回车确认 · Esc 取消 · 可点「确认/取消」")
                .style(Style::default().fg(Color::DarkGray)),
            Rect::new(area.x, area.bottom().saturating_sub(2), area.width, 1),
        );
    })?;
    Ok(())
}

pub fn render_placeholder(terminal: &mut Tui, title: &str, message: &str) -> io::Result<()> {
    render_placeholder_scroll(terminal, title, message, 0).map(|_| ())
}

/// 渲染占位/预览页，支持垂直滚动。返回最大可滚动行数（0 = 内容不溢出）。
/// `scroll` 是当前滚动偏移，会按内容高度自动夹紧。
pub fn render_placeholder_scroll(
    terminal: &mut Tui,
    title: &str,
    message: &str,
    scroll: usize,
) -> io::Result<usize> {
    let mut lines = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    lines.extend(message.lines().map(|line| {
        // 以 § 开头的行 = 分节标题（青色加粗），供长文占位页建立层级。
        if let Some(header) = line.strip_prefix('§') {
            Line::from(Span::styled(
                header.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::Gray),
            ))
        }
    }));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Esc 返回 · ↑/↓ 滚动",
        Style::default().fg(Color::DarkGray),
    )));

    let mut max_scroll = 0usize;
    terminal.draw(|f| {
        let area = f.area();
        let body = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        let width = body.width.max(1) as usize;
        let total: usize = lines
            .iter()
            .map(|line| placeholder_wrapped_rows(line, width))
            .sum();
        max_scroll = total.saturating_sub(body.height as usize);
        let panel = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        f.render_widget(panel.scroll((scroll.min(max_scroll) as u16, 0)), body);
    })?;
    Ok(max_scroll)
}

fn placeholder_wrapped_rows(line: &Line, width: usize) -> usize {
    let chars = line.width();
    if chars == 0 {
        return 1;
    }
    chars.div_ceil(width).max(1)
}

pub fn render_busy(terminal: &mut Tui, label: &str) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let panel = Paragraph::new(vec![
            Line::from(Span::styled(
                "…",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(label, Style::default().fg(Color::White))),
            Line::from(Span::styled(
                "处理中…",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Left);
        let body = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(area.height / 3),
            area.width.saturating_sub(4),
            4,
        );
        f.render_widget(panel, body);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_wrapped_rows_counts_wrap_lines() {
        let short = Line::from("你好");
        assert_eq!(placeholder_wrapped_rows(&short, 20), 1);

        let long = Line::from("abcdefghij");
        assert_eq!(placeholder_wrapped_rows(&long, 5), 2);
        assert_eq!(placeholder_wrapped_rows(&long, 10), 1);
        assert_eq!(placeholder_wrapped_rows(&long, 3), 4);

        let empty = Line::from("");
        assert_eq!(placeholder_wrapped_rows(&empty, 10), 1);

        let narrow = Line::from("abc");
        assert_eq!(placeholder_wrapped_rows(&narrow, 1), 3);
    }
}
