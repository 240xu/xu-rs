//! 页面共享件（v3 前端层）：MCP/Skills 页底定向同步 chips。

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::widgets::chips::Glyph;
use spec::domain::AgentTarget;

use crate::ui::theme;
use crate::ui::widgets::chips::point_in;

/// 绘制页脚定向同步 chips（MCP / Skills 页共用）。
pub fn render_sync_target_chips(f: &mut ratatui::Frame<'_>, footer: Rect) {
    // 克制派：单行文字按钮（身份色亮字），不再用三个大圆角盒子。
    let mut left = vec![Span::styled(
        " 同步 » ",
        Style::default().fg(Color::DarkGray),
    )];
    for index in 0..3 {
        if index > 0 {
            left.push(Span::styled("  ", Style::default().fg(Color::DarkGray)));
        }
        let (_, label, color) = sync_target_chip_spec(index);
        left.push(Span::styled(label, Style::default().fg(color)));
    }
    let line = right_align_line(
        left,
        vec![Span::styled(
            "源=OpenCode · 点按定向同步",
            Style::default().fg(Color::DarkGray),
        )],
        footer.width,
    );
    f.render_widget(
        Paragraph::new(line),
        Rect::new(footer.x, footer.y + 1, footer.width, 1),
    );
}

/// 页脚定向同步 chips 命中（左键 Up）：三段 → 对应目标（OpenCode/Claude/Codex）。
pub fn sync_target_chips_hit_test(area: Rect, m: &MouseEvent) -> Option<AgentTarget> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let footer = Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 3);
    sync_target_chip_rects(footer)
        .into_iter()
        .zip([
            AgentTarget::OpenCode,
            AgentTarget::ClaudeCode,
            AgentTarget::Codex,
        ])
        .find_map(|(rect, target)| point_in(rect, m.column, m.row).then_some(target))
}

pub fn sync_target_chip_rects(footer: Rect) -> Vec<Rect> {
    let mut x = footer.x + 8; // " 同步 » " 前缀宽
    (0..3)
        .map(|index| {
            let (_, label, _) = sync_target_chip_spec(index);
            let w = str_width(label) as u16;
            let rect = Rect::new(x, footer.y, w, footer.height);
            x += w + 2;
            rect
        })
        .collect()
}

/// 每端同步 chip 的身份规格：glyph + 全名 + 身份色（全应用唯一的每端配色）。
/// OpenCode=青 / Claude=品红 / Codex=黄。
pub fn sync_target_chip_spec(index: usize) -> (Glyph, &'static str, Color) {
    match index {
        0 => (Glyph::OpenCode, "OpenCode", Color::Cyan),
        1 => (Glyph::Claude, "Claude", Color::Magenta),
        _ => (Glyph::Codex, "Codex", Color::Yellow),
    }
}

// ─── 自 menu 迁入：文本/列表工具 ───

/// 显示宽度：CJK 全角字符按 2 列，其余按 1 列。
pub fn str_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            1 + usize::from(matches!(
                c as u32,
                0x1100..=0x115F
                    | 0x2E80..=0xA4CF
                    | 0xAC00..=0xD7A3
                    | 0xF900..=0xFAFF
                    | 0xFE30..=0xFE4F
                    | 0xFF00..=0xFF60
                    | 0xFFE0..=0xFFE6
            ))
        })
        .sum()
}

/// 左右两段拼成一行：右段贴右边缘，中间自动补空格。
pub fn right_align_line<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: u16) -> Line<'a> {
    let lw: usize = left.iter().map(|s| str_width(&s.content)).sum();
    let rw: usize = right.iter().map(|s| str_width(&s.content)).sum();
    let gap = (width as usize).saturating_sub(lw + rw);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

/// 在过滤列表上移动选择；返回新的真实下标。当前选中项不在过滤窗口内时，
/// 向上回退到列表首项、向下前进到列表尾项。
pub fn move_filtered_selection(filtered: &[usize], selected: usize, delta: isize) -> usize {
    if filtered.is_empty() {
        return selected;
    }
    let position = filtered
        .iter()
        .position(|index| *index == selected)
        .unwrap_or_else(|| if delta < 0 { 0 } else { filtered.len() - 1 });
    let next = (position as isize + delta).clamp(0, filtered.len() as isize - 1) as usize;
    filtered[next]
}

/// 把选中下标钳制到过滤后可见项里：仍在窗口中则不动；否则跳到最近的可见项
/// （距离相等时取序号较小者）。用于过滤条件变化后避免操作落在隐藏项（BUG-2）。
pub fn clamp_selection_to_filter(filtered: &[usize], selected: usize) -> usize {
    if filtered.is_empty() {
        return 0;
    }
    if let Some(pos) = filtered.iter().position(|index| *index == selected) {
        return filtered[pos];
    }
    let mut best = filtered[0];
    let mut best_gap = usize::MAX;
    for index in filtered {
        let gap = index.abs_diff(selected);
        if gap < best_gap {
            best_gap = gap;
            best = *index;
        }
    }
    best
}

pub fn list_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if len <= visible || selected < visible {
        0
    } else {
        (selected + 1).saturating_sub(visible)
    }
}

/// Computes the visible window of a vertical list using the exact same
/// ratatui `Layout` that rendering uses, returning `(index, rect)` pairs.
/// Rendering and touch hit-testing must call this same function so taps hit
/// the cards that were actually drawn, even when rows are compressed to fit a
/// short screen (where fixed-pitch arithmetic would drift out of sync).
pub fn list_window_rects(
    content: Rect,
    len: usize,
    selected: usize,
    row_height: u16,
    visible: usize,
) -> Vec<(usize, Rect)> {
    if len == 0 || content.height == 0 {
        return Vec::new();
    }
    let start = list_window_start(selected, len, visible);
    let shown = visible.min(len - start);
    let constraints = vec![Constraint::Length(row_height); shown];
    Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(content)
        .iter()
        .enumerate()
        .map(|(row, rect)| (start + row, *rect))
        .collect()
}

// ─── Tab 导航（自 menu 迁入 · 按现行为重建）───

/// 顶部标签条占用的行数（文字行 + 下划线行）。
pub const TAB_BAR_HEIGHT: u16 = 2;

/// 主导航五标签（顺序即渲染顺序）。
pub const TABS: [(Tab, &str); 5] = [
    (Tab::Providers, "供应商"),
    (Tab::Agent, "Agent"),
    (Tab::Mcp, "MCP"),
    (Tab::Skills, "Skills"),
    (Tab::Sessions, "会话"),
];

/// 主导航页签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Providers,
    Agent,
    Mcp,
    Skills,
    Sessions,
}

impl Tab {
    pub fn index(self) -> usize {
        TABS.iter().position(|t| t.0 == self).unwrap_or(0)
    }
}

/// 兼容旧调用点。
#[allow(dead_code)] // 桥接期保留；M6 由 Tab::index 取代
pub fn tab_index(tab: Tab) -> usize {
    tab.index()
}

/// 左右循环切页。
pub fn move_tab(tab: Tab, key: KeyCode) -> Tab {
    let len = TABS.len();
    let index = tab.index();
    match key {
        KeyCode::Left | KeyCode::BackTab => TABS[(index + len - 1) % len].0,
        KeyCode::Right | KeyCode::Tab => TABS[(index + 1) % len].0,
        _ => tab,
    }
}

/// 五等分列矩形（渲染与命中共用）。
pub fn tab_bar_rects(area: Rect) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, TABS.len() as u32); TABS.len()])
        .split(Rect {
            height: TAB_BAR_HEIGHT,
            ..area
        })
        .to_vec()
}

/// 命中测试：返回点击到的页签。
pub fn tab_hit_test(area: Rect, m: &MouseEvent) -> Option<Tab> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    tab_bar_rects(area)
        .iter()
        .zip(TABS)
        .find(|(rect, _)| point_in(**rect, m.column, m.row))
        .map(|(_, (tab, _))| tab)
}

/// 绘制标签条：活动项白字加粗 + 青色 ━ 下划线；其余暗灰。
pub fn render_tab_bar(f: &mut ratatui::Frame<'_>, area: Rect, active: Tab) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for (rect, (tab, label)) in tab_bar_rects(area).iter().zip(TABS.iter()) {
        let w = str_width(label) as u16;
        if rect.width < w || rect.height == 0 {
            continue;
        }
        let is_active = *tab == active;
        let fg = if is_active { theme::INK } else { theme::G2 };
        let mut dx = 0u16;
        for ch in label.chars() {
            if dx >= rect.width {
                break;
            }
            f.buffer_mut()[(rect.x + dx, rect.y)]
                .set_char(ch)
                .set_fg(fg)
                .set_style(if is_active {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                });
            dx += str_width(&ch.to_string()) as u16;
        }
        if is_active && rect.height > 1 {
            for dx in 0..w.min(rect.width) {
                f.buffer_mut()[(rect.x + dx, rect.y + 1)]
                    .set_char(theme::UNDERLINE.chars().next().unwrap())
                    .set_fg(theme::ACCENT);
            }
        }
    }
}

/// 页面框架：标签条 + 内容区。
pub fn render_page(
    terminal: &mut crate::tui::Tui,
    tab: Tab,
    body: impl FnOnce(&mut ratatui::Frame<'_>, Rect),
) -> std::io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        if area.width == 0 || area.height == 0 {
            return;
        }
        render_tab_bar(f, area, tab);
        // 内容区让位底部拇指坞（2 行）
        let content = Rect::new(
            area.x,
            area.y + TAB_BAR_HEIGHT,
            area.width,
            area.height
                .saturating_sub(TAB_BAR_HEIGHT + crate::ui::widgets::dock::DOCK_HEIGHT),
        );
        body(f, content);
        if tab == Tab::Providers {
            crate::ui::widgets::dock::render(f, crate::ui::widgets::dock::DockCtx::List);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::screens::detail::{active_targets, protocol_label};
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn tap(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }
    fn press(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn tab_navigation_cycles_wraps_and_hit_tests() {
        assert_eq!(move_tab(Tab::Providers, KeyCode::Right), Tab::Agent);
        assert_eq!(move_tab(Tab::Sessions, KeyCode::Right), Tab::Providers);
        assert_eq!(move_tab(Tab::Providers, KeyCode::Left), Tab::Sessions);
        assert_eq!(move_tab(Tab::Agent, KeyCode::Tab), Tab::Mcp);
        assert_eq!(move_tab(Tab::Mcp, KeyCode::BackTab), Tab::Agent);
        assert_eq!(move_tab(Tab::Providers, KeyCode::Enter), Tab::Providers);

        let bar = Rect::new(0, 0, 80, 3);
        assert_eq!(tab_hit_test(bar, &tap(5, 1)), Some(Tab::Providers));
        assert_eq!(tab_hit_test(bar, &tap(25, 1)), Some(Tab::Agent));
        assert_eq!(tab_hit_test(bar, &tap(40, 1)), Some(Tab::Mcp));
        assert_eq!(tab_hit_test(bar, &tap(56, 1)), Some(Tab::Skills));
        assert_eq!(tab_hit_test(bar, &tap(79, 1)), Some(Tab::Sessions));
        assert_eq!(tab_hit_test(bar, &tap(5, 0)), Some(Tab::Providers));
        assert_eq!(tab_hit_test(bar, &press(5, 1)), None);
    }

    #[test]
    fn tab_bar_splits_five_equal_buttons_covering_the_width() {
        let area = Rect::new(0, 0, 100, TAB_BAR_HEIGHT);
        let rects = tab_bar_rects(area);
        assert_eq!(rects.len(), TABS.len());
        assert_eq!(rects[0].x, area.x);
        let last = rects.last().unwrap();
        assert_eq!(last.x + last.width, area.x + area.width);
        for pair in rects.windows(2) {
            assert_eq!(pair[0].x + pair[0].width, pair[1].x);
        }
    }

    #[test]
    fn sync_target_chip_spec_carries_agent_identity() {
        let (g0, n0, c0) = sync_target_chip_spec(0);
        assert_eq!(n0, "OpenCode");
        assert_eq!(c0, Color::Cyan);
        let (_, _, _) = (g0, n0, c0);
        let (_, n1, c1) = sync_target_chip_spec(1);
        assert_eq!(n1, "Claude");
        assert_eq!(c1, Color::Magenta);
        let (_, n2, c2) = sync_target_chip_spec(2);
        assert_eq!(n2, "Codex");
        assert_eq!(c2, Color::Yellow);
    }

    #[test]
    fn sync_target_chip_words_hit_full_footer_columns() {
        let footer = Rect::new(0, 30, 120, 1);
        let rects = sync_target_chip_rects(footer);
        assert_eq!(rects.len(), 3);
        assert_eq!(
            sync_target_chips_hit_test(footer, &tap(rects[0].x, 30)),
            Some(AgentTarget::OpenCode)
        );
        assert_eq!(
            sync_target_chips_hit_test(footer, &tap(rects[1].x + rects[1].width - 1, 30)),
            Some(AgentTarget::ClaudeCode)
        );
        assert_eq!(
            sync_target_chips_hit_test(footer, &tap(rects[2].x + rects[2].width - 1, 30)),
            Some(AgentTarget::Codex)
        );
        assert_eq!(sync_target_chips_hit_test(footer, &press(0, 30)), None);
    }

    #[test]
    fn filtered_selection_moves_within_window_and_clamps() {
        let filtered = vec![2usize, 5, 9];
        assert_eq!(move_filtered_selection(&filtered, 2, 1), 5);
        assert_eq!(move_filtered_selection(&filtered, 5, -1), 2);
        assert_eq!(move_filtered_selection(&filtered, 9, 1), 9);
        assert_eq!(move_filtered_selection(&filtered, 2, -1), 2);
        // 选中不在过滤集内：向上回退首项、向下前进尾项
        assert_eq!(move_filtered_selection(&filtered, 7, -1), 2);
        assert_eq!(move_filtered_selection(&filtered, 7, 1), 9);
    }

    #[test]
    fn clamp_selection_to_filter_picks_nearest_visible() {
        let filtered = vec![2usize, 5, 9];
        assert_eq!(clamp_selection_to_filter(&filtered, 5), 5);
        assert_eq!(clamp_selection_to_filter(&filtered, 4), 5);
        assert_eq!(clamp_selection_to_filter(&filtered, 8), 9);
        assert_eq!(clamp_selection_to_filter(&filtered, 0), 2);
        assert_eq!(clamp_selection_to_filter(&[], 3), 0);
    }

    #[test]
    fn active_targets_joins_matching_provider_ids() {
        use std::collections::BTreeMap;
        let mut current = BTreeMap::new();
        current.insert("opencode".to_string(), "zen".to_string());
        current.insert("claude".to_string(), "zen,other".to_string());
        current.insert("codex".to_string(), "go".to_string());
        assert_eq!(active_targets(&current, "zen"), "claude,opencode"); // BTreeMap 序
        assert_eq!(active_targets(&current, "none"), "");
    }

    #[test]
    fn protocol_label_maps_each_kind() {
        use spec::domain::ProtocolKind;
        assert!(!protocol_label(ProtocolKind::OpenAiChat).is_empty());
        assert!(!protocol_label(ProtocolKind::OpenAiResponses).is_empty());
        assert!(!protocol_label(ProtocolKind::AnthropicMessages).is_empty());
    }
}
