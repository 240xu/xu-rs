//! 拇指坞（Thumb-Dock）：页面底部常驻动作条，触屏等价键位。
//! 命中 → 合成 KeyCode 事件，复用既有键盘分支，零新业务逻辑。

use crossterm::event::KeyCode;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::theme;
use crate::ui::widgets::chips::point_in;

#[derive(Clone, Copy, PartialEq)]
pub enum DockCtx {
    List,
    Detail,
    Editor,
}

/// The dock owns this geometry so rendering, hit-testing, and content
/// reservation cannot drift apart.
pub const DOCK_HEIGHT: u16 = 2;

/// 每上下文的按钮表：(标签, 合成按键)。
pub fn items(ctx: DockCtx) -> &'static [(&'static str, KeyCode)] {
    match ctx {
        DockCtx::List => &[
            ("↑", KeyCode::Up),
            ("↓", KeyCode::Down),
            ("+ 新增", KeyCode::Char('a')),
            ("# 模板", KeyCode::Char('p')),
            ("↻ 刷新", KeyCode::Char('r')),
            ("⇄ 多选", KeyCode::Char('g')),
            ("W Web", KeyCode::Char('w')),
        ],
        DockCtx::Detail => &[
            ("‹ 返回", KeyCode::Esc),
            ("✎ 编辑", KeyCode::Char('e')),
            ("↓ 拉模型", KeyCode::Char('m')),
            ("☑ 存选", KeyCode::Char('s')),
            ("◐ 明文", KeyCode::Char('y')),
            ("①Claude", KeyCode::Char('1')),
            ("②Codex", KeyCode::Char('2')),
            ("③OpenCode", KeyCode::Char('3')),
        ],
        DockCtx::Editor => &[("✕ 取消", KeyCode::Esc), ("✓ 保存", KeyCode::Enter)],
    }
}

/// 底部动作条区域（占 2 行）。
pub fn bar(area: Rect) -> Rect {
    Rect {
        y: area.y + area.height.saturating_sub(DOCK_HEIGHT),
        height: DOCK_HEIGHT.min(area.height),
        ..area
    }
}

/// 各按钮矩形（与渲染同源）。
pub fn rects(area: Rect, count: usize) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, count.max(1) as u32); count])
        .split(bar(area))
        .to_vec()
}

/// 绘制动作坞。
pub fn render(f: &mut Frame, ctx: DockCtx) {
    let its = items(ctx);
    let area = f.area();
    let bar = bar(area);
    if bar.width == 0 || bar.height == 0 || its.is_empty() {
        return;
    }

    let separator = "─".repeat(bar.width as usize);
    f.render_widget(
        Paragraph::new(separator).style(Style::default().fg(theme::G2)),
        Rect { height: 1, ..bar },
    );

    for (index, (rect, (label, _))) in rects(area, its.len()).iter().zip(its).enumerate() {
        let is_primary = match ctx {
            DockCtx::List => matches!(index, 2 | 3),
            DockCtx::Detail => matches!(index, 1 | 2),
            DockCtx::Editor => index == 1,
        };
        let style = Style::default()
            .bg(if is_primary {
                theme::PANEL_BG
            } else {
                theme::BASE_BG
            })
            .fg(if is_primary {
                theme::ACCENT
            } else {
                theme::INK
            })
            .add_modifier(if is_primary {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        f.render_widget(
            Paragraph::new(Span::styled(*label, style)).alignment(Alignment::Center),
            Rect {
                y: rect.y + 1,
                height: 1,
                ..*rect
            },
        );
    }
}

/// 命中测试：返回应合成的按键。未命中 None。
pub fn hit(ctx: DockCtx, area: Rect, m: &crossterm::event::MouseEvent) -> Option<KeyCode> {
    use crossterm::event::{MouseButton, MouseEventKind};
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let its = items(ctx);
    rects(area, its.len())
        .iter()
        .zip(its)
        .find(|(rect, _)| point_in(**rect, m.column, m.row))
        .map(|(_, (_, key))| *key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn tap(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn dock_geometry_reserves_exact_height() {
        let area = Rect::new(0, 0, 120, 32);
        assert_eq!(bar(area), Rect::new(0, 30, 120, DOCK_HEIGHT));
        assert_eq!(rects(area, items(DockCtx::List).len()).len(), 7);
    }

    #[test]
    fn dock_hit_uses_rendered_button_columns() {
        let area = Rect::new(0, 0, 120, 32);
        let buttons = rects(area, items(DockCtx::List).len());
        let add = buttons[2];
        assert_eq!(
            hit(DockCtx::List, area, &tap(add.x, add.y + 1)),
            Some(KeyCode::Char('a'))
        );
        assert_eq!(hit(DockCtx::List, area, &tap(0, 29)), None);
    }

    #[test]
    fn dock_ignores_non_left_release_events() {
        let area = Rect::new(0, 0, 120, 32);
        let mut event = tap(1, 31);
        event.kind = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(hit(DockCtx::List, area, &event), None);
    }
}
