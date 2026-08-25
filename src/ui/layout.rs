//! 几何纯函数：渲染与命中共享的唯一布局来源。
#![allow(dead_code)] // 桥接期：M2+ 逐屏消费后移除

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// 窗口起始行（跟随选中项，贴底夹紧）。
pub fn list_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if len == 0 || visible == 0 {
        return 0;
    }
    selected
        .saturating_sub(visible.saturating_sub(1))
        .min(len.saturating_sub(1))
}

/// 可见窗口内各行的 (全局下标, 矩形)。
pub fn list_window_rects(
    area: Rect,
    len: usize,
    selected: usize,
    row_height: u16,
    visible: usize,
) -> Vec<(usize, Rect)> {
    let start = list_window_start(selected, len, visible.max(1));
    let rows = visible.min(len.saturating_sub(start));
    let constraints: Vec<Constraint> = (0..visible)
        .map(|_| Constraint::Length(row_height))
        .collect();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    (start..start + rows)
        .enumerate()
        .map(|(index, global)| (global, chunks[index]))
        .collect()
}

/// 过滤窗口内的相邻移动（delta=±1；越窗先钳制）。
pub fn move_in_window(filtered: &[usize], selected: usize, delta: isize) -> usize {
    if filtered.is_empty() {
        return selected;
    }
    let position = filtered
        .iter()
        .position(|index| *index == selected)
        .unwrap_or(if delta < 0 { 0 } else { filtered.len() - 1 });
    let next = (position as isize + delta).clamp(0, filtered.len() as isize - 1) as usize;
    filtered[next]
}
