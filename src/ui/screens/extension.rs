//! 扩展列表通用组件：MCP / Skills 两页共用的卡片渲染与鼠标命中。
//!
//! 两页共享同一套「spec store + 三端投影」视觉：名称行 + 元信息行 + oc/cl/cdx
//! 三按钮行。本模块把两份近乎相同的渲染与命中测试收敛为一份，页面只保留
//! 各自的数据 → 行模型映射与页头/页脚文案。

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::screens::chrome::{list_window_rects, right_align_line};
use crate::ui::theme;
use crate::ui::widgets::chips::{point_in, toolbar_rects, IconButton};
use spec::domain::AgentTarget;

/// 卡片高度：2 行内容 + 1 行呼吸空隙（无边框，克制派）。
pub const EXTENSION_CARD_HEIGHT: u16 = 3;
pub const EXTENSION_CARD_VISIBLE: usize = 5;

/// 三端开关的文字标签与身份色（渲染与命中共用）。
pub const TARGET_TOGGLES: [(&str, AgentTarget); 3] = [
    ("OpenCode", AgentTarget::OpenCode),
    ("Claude", AgentTarget::ClaudeCode),
    ("Codex", AgentTarget::Codex),
];

/// 右侧三端文字开关簇的总宽（词宽 + 2 列间隔）。
pub fn target_toggle_cluster_width() -> u16 {
    TARGET_TOGGLES
        .iter()
        .map(|(label, _)| crate::ui::screens::chrome::str_width(label) as u16)
        .sum::<u16>()
        + 2 * (TARGET_TOGGLES.len() as u16 - 1)
}

/// 第 index 个开关词的矩形（贴卡片右缘，纵向覆盖整卡高度——拇指触屏友好）。
pub fn target_toggle_rect(card: Rect, index: usize) -> Rect {
    let cluster = target_toggle_cluster_width();
    let mut x = card.x + card.width.saturating_sub(cluster);
    for (i, (label, _)) in TARGET_TOGGLES.iter().enumerate() {
        let w = crate::ui::screens::chrome::str_width(label) as u16;
        if i == index {
            return Rect::new(x, card.y, w, card.height);
        }
        x += w + 2;
    }
    Rect::new(x, card.y, 0, 0)
}

/// 内容区能完整放下几张卡（不压缩）。矮终端下宁可少显示几张可滚动，
/// 也不要让 ratatui 压缩卡片导致按钮压线/压字。
pub fn extension_visible_cards(content: Rect) -> usize {
    ((content.height as usize) / (EXTENSION_CARD_HEIGHT as usize)).clamp(1, EXTENSION_CARD_VISIBLE)
}

/// 一行扩展卡片：名称（可选灰色后缀）+ 元信息行 + 三端启用状态。
pub struct ExtensionRow<'a> {
    pub name: &'a str,
    pub suffix: Vec<Span<'a>>,
    pub meta: Line<'a>,
    pub targets: [bool; 3],
}

impl<'a> ExtensionRow<'a> {
    pub fn new(name: &'a str, suffix: Vec<Span<'a>>, meta: Line<'a>, targets: [bool; 3]) -> Self {
        Self {
            name,
            suffix,
            meta,
            targets,
        }
    }
}

/// 卡片区命中结果：点三端按钮 → Toggle，否则 → Select。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionCardHit {
    Toggle(usize, AgentTarget),
    Select(usize),
}

/// 绘制一排工具条按钮（chips），返回其矩形供渲染与命中共用。
pub fn render_toolbar(
    f: &mut ratatui::Frame,
    area: Rect,
    buttons: &[(&'static str, Color)],
) -> Vec<Rect> {
    let rects = toolbar_rects(area.x, area.y + 2, area.width, buttons.len());
    for ((icon, color), rect) in buttons.iter().zip(rects.iter()) {
        f.render_widget(
            IconButton {
                label: icon,
                color: *color,
                active: false,
            },
            *rect,
        );
    }
    rects
}

/// 绘制卡片列表（无框：名称行 + 元信息行 + 呼吸空隙；三端开关为文字态）。
pub fn render_extension_cards(
    f: &mut ratatui::Frame,
    rects: &[(usize, Rect)],
    rows: &[ExtensionRow],
    selected: usize,
) {
    for (index, rect) in rects {
        let row = &rows[*index];
        let active = *index == selected;
        let mut title = Line::from(Span::styled(
            row.name,
            Style::default()
                .fg(if active { Color::White } else { Color::Gray })
                .add_modifier(Modifier::BOLD),
        ));
        title.extend(row.suffix.iter().cloned());
        // 行 A：▍竖条 + 名称 + 后缀 …… 右：N/3 + 三端文字开关（on=身份色亮字）
        let mut toggles: Vec<Span> = Vec::new();
        for (i, (label, _)) in TARGET_TOGGLES.iter().enumerate() {
            if i > 0 {
                toggles.push(Span::styled("  ", Style::default().fg(Color::DarkGray)));
            }
            let on = row.targets[i];
            toggles.push(Span::styled(
                *label,
                if on {
                    Style::default().fg(match i {
                        0 => Color::Cyan,
                        1 => Color::Magenta,
                        _ => Color::Yellow,
                    })
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
        }
        let n_on = row.targets.iter().filter(|t| **t).count();
        toggles.insert(
            0,
            Span::styled(format!("{n_on}/3  "), Style::default().fg(Color::DarkGray)),
        );
        let bar = if active {
            Span::styled("▍", Style::default().fg(Color::Cyan))
        } else {
            Span::raw(" ")
        };
        let mut left = vec![bar];
        left.extend(title);
        let line_a = crate::ui::screens::chrome::right_align_line(left, toggles, rect.width);
        // 行 B：元信息（右侧留空给开关的整高命中列）
        let line_b = right_align_line(
            {
                let mut spans = vec![Span::raw("    ")];
                spans.extend(row.meta.spans.iter().cloned());
                spans
            },
            vec![],
            rect.width,
        );
        let body = Rect::new(rect.x, rect.y, rect.width, 2);
        let card_style = if active {
            Style::default().bg(theme::PANEL_BG)
        } else {
            Style::default()
        };
        f.render_widget(Paragraph::new(vec![line_a, line_b]).style(card_style), body);
    }
}

/// 页头以下、页脚以上的卡片内容区（渲染与命中共用）。
pub fn extension_content_rect(area: Rect, header_height: u16) -> Rect {
    Rect::new(
        area.x,
        area.y + header_height,
        area.width,
        area.height.saturating_sub(header_height + 3),
    )
}

/// 工具条按钮命中：返回被点中的按钮序号。
pub fn extension_toolbar_hit_test(
    area: Rect,
    button_count: usize,
    m: &MouseEvent,
) -> Option<usize> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    toolbar_rects(area.x, area.y + 2, area.width, button_count)
        .into_iter()
        .enumerate()
        .find_map(|(index, rect)| point_in(rect, m.column, m.row).then_some(index))
}

/// 卡片区命中测试：三端按钮 → Toggle，卡片内其他位置 → Select。
pub fn extension_card_hit_test(
    area: Rect,
    header_height: u16,
    len: usize,
    selected: usize,
    m: &MouseEvent,
) -> Option<ExtensionCardHit> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) || len == 0 {
        return None;
    }
    let content = extension_content_rect(area, header_height);
    for (index, rect) in list_window_rects(
        content,
        len,
        selected,
        EXTENSION_CARD_HEIGHT,
        extension_visible_cards(content),
    ) {
        if point_in(rect, m.column, m.row) {
            // 三端文字开关在行 A 右侧；点词 → Toggle，其余 → Select。
            // Toggle 首参是卡片下标（index），不是开关序号。
            for (ti, (_, target)) in TARGET_TOGGLES.iter().enumerate() {
                if point_in(target_toggle_rect(rect, ti), m.column, m.row) {
                    return Some(ExtensionCardHit::Toggle(index, *target));
                }
            }
            return Some(ExtensionCardHit::Select(index));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    fn tap(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    fn press(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn toolbar_hit_test_ignores_non_left_up_and_gaps() {
        assert_eq!(extension_toolbar_hit_test(AREA, 5, &tap(5, 3)), Some(0));
        assert_eq!(extension_toolbar_hit_test(AREA, 5, &tap(16, 3)), Some(1));
        assert_eq!(extension_toolbar_hit_test(AREA, 5, &tap(10, 3)), None); // gap
        assert_eq!(extension_toolbar_hit_test(AREA, 5, &press(5, 3)), None); // down
        assert_eq!(extension_toolbar_hit_test(AREA, 5, &tap(5, 7)), None); // below chips
    }

    #[test]
    fn card_hit_test_selects_and_toggles_within_cards() {
        // header 9: 卡片 y=9 起，行 A（y=9）右侧为三端文字开关。
        // AREA 宽 80，cluster 23 右对齐 → OpenCode x57..64 / Claude x67..72 / Codex x75..79
        assert_eq!(
            extension_card_hit_test(AREA, 9, 2, 0, &tap(58, 9)),
            Some(ExtensionCardHit::Toggle(0, AgentTarget::OpenCode))
        );
        assert_eq!(
            extension_card_hit_test(AREA, 9, 2, 0, &tap(68, 9)),
            Some(ExtensionCardHit::Toggle(0, AgentTarget::ClaudeCode))
        );
        assert_eq!(
            extension_card_hit_test(AREA, 9, 2, 0, &tap(76, 9)),
            Some(ExtensionCardHit::Toggle(0, AgentTarget::Codex))
        );
        // 行 B（meta）与名称区 → Select
        assert_eq!(
            extension_card_hit_test(AREA, 9, 2, 0, &tap(3, 10)),
            Some(ExtensionCardHit::Select(0))
        );
        assert_eq!(
            extension_card_hit_test(AREA, 9, 2, 0, &tap(30, 9)),
            Some(ExtensionCardHit::Select(0))
        );
        assert_eq!(extension_card_hit_test(AREA, 9, 0, 0, &tap(30, 9)), None);
        assert_eq!(extension_card_hit_test(AREA, 9, 2, 0, &tap(30, 22)), None);
    }

    #[test]
    fn content_rect_uses_header_and_footer_margins() {
        let content = extension_content_rect(AREA, 9);
        assert_eq!(content, Rect::new(0, 9, 80, 12));
    }
}
