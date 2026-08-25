//! SearchBox 组件：圆角线框 + ⌕ 图标 + 实时过滤 + Esc 清除。
//! 风格参考 /plugins Installed 列表的搜索框。

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

/// 搜索框（含边框）占用的行数。
pub const SEARCHBOX_HEIGHT: u16 = 3;

/// 实时过滤输入框。`query` 为空时不过滤任何条目。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchBox {
    pub query: String,
    pub focused: bool,
}

impl SearchBox {
    pub fn new() -> Self {
        Self::default()
    }

    /// 处理一个按键事件；返回 true 表示事件被搜索框消费。
    /// 字符追加、退格删除、Esc 清空（仅当已有内容时）。
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) => {
                self.query.push(c);
                true
            }
            KeyCode::Backspace => {
                self.query.pop();
                true
            }
            KeyCode::Esc => {
                if self.query.is_empty() {
                    return false;
                }
                self.query.clear();
                true
            }
            _ => false,
        }
    }

    /// 大小写不敏感的子串匹配；空查询匹配一切。
    pub fn matches(&self, haystack: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        haystack.to_lowercase().contains(&self.query.to_lowercase())
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    pub fn clear(&mut self) {
        self.query.clear();
    }

    /// 槽位高度：未聚焦且无查询时折叠为 1 行提示，否则展开为完整框。
    pub fn slot_height(&self) -> u16 {
        if self.focused || !self.is_empty() {
            SEARCHBOX_HEIGHT
        } else {
            1
        }
    }
}

/// 渲染搜索框：圆角边框 + ⌕ 图标 + 查询文本 + 提示。
/// 聚焦时青色边框并显示光标，非空查询时右侧提示「Esc 清除」。
pub fn render_search_box(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    sb: &SearchBox,
    placeholder: &str,
) {
    // 折叠态：单行暗灰提示（P11——不占黄金位），点击或按 / 展开。
    if sb.slot_height() == 1 {
        let line = Line::from(vec![
            Span::styled("⌕", Style::default().fg(Color::Cyan)),
            Span::styled(
                format!(" / {placeholder}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    let border = if sb.focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let hint = if sb.focused {
        if sb.query.is_empty() {
            "输入过滤 · Esc 失焦".to_string()
        } else {
            "Esc 清除".to_string()
        }
    } else {
        "/ 聚焦搜索".to_string()
    };
    let text = if sb.query.is_empty() {
        Line::from(vec![
            Span::styled(" ⌕ ", Style::default().fg(Color::Cyan)),
            Span::styled(placeholder, Style::default().fg(Color::DarkGray)),
            Span::styled(format!("  {hint}"), Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ⌕ ", Style::default().fg(Color::Cyan)),
            Span::styled(&sb.query, Style::default().fg(Color::White)),
            if sb.focused {
                Span::styled("▏", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            },
            Span::styled(format!("  {hint}"), Style::default().fg(Color::DarkGray)),
        ])
    };
    let widget = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border)),
        )
        .style(Style::default().add_modifier(Modifier::ITALIC));
    f.render_widget(widget, area);
}

/// 搜索框点击命中：点击圆角框内部区域即聚焦（增大命中区）。
pub fn search_box_click(area: Rect, m: &MouseEvent) -> bool {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return false;
    }
    point_in_rect(area, m.column, m.row)
}

fn point_in_rect(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// 搜索槽位矩形（渲染与命中共用；折叠态 1 行）。
pub fn search_slot_rect(content_area: Rect, search: &SearchBox) -> Rect {
    Rect::new(
        content_area.x,
        content_area.y,
        content_area.width,
        search.slot_height(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn tap(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn key_handling_appends_backspaces_and_clears() {
        let mut sb = SearchBox::new();
        assert!(sb.handle_key(&key(KeyCode::Char('d'))));
        assert!(sb.handle_key(&key(KeyCode::Char('e'))));
        assert_eq!(sb.query, "de");
        assert!(sb.handle_key(&key(KeyCode::Backspace)));
        assert_eq!(sb.query, "d");
        // Esc 清空并消费
        assert!(sb.handle_key(&key(KeyCode::Esc)));
        assert!(sb.query.is_empty());
        // 空查询时 Esc 不消费（留给外层导航）
        assert!(!sb.handle_key(&key(KeyCode::Esc)));
        // 非输入键不消费
        assert!(!sb.handle_key(&key(KeyCode::Enter)));
        assert!(!sb.handle_key(&key(KeyCode::Up)));
    }

    #[test]
    fn matching_is_case_insensitive_substring() {
        let sb = SearchBox {
            query: "DEEP".to_string(),
            focused: false,
        };
        assert!(sb.matches("DeepSeek V4"));
        assert!(sb.matches("deepseek"));
        assert!(!sb.matches("glm"));
        let empty = SearchBox::new();
        assert!(empty.matches("anything"));
        assert!(empty.matches(""));
    }

    #[test]
    fn click_hits_inside_rounded_box_only() {
        let area = Rect::new(0, 3, 40, 3);
        assert!(search_box_click(area, &tap(2, 4)));
        assert!(search_box_click(area, &tap(39, 3)));
        // 边框行也算命中（增大触摸命中区）
        assert!(search_box_click(area, &tap(0, 3)));
        assert!(!search_box_click(area, &tap(5, 2))); // 上方
        assert!(!search_box_click(area, &tap(5, 6))); // 下方
        assert!(!search_box_click(area, &tap(40, 4))); // 右侧外
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert!(!search_box_click(area, &moved));
    }

    #[test]
    fn clear_resets_query() {
        let mut sb = SearchBox {
            query: "abc".to_string(),
            focused: true,
        };
        sb.clear();
        assert!(sb.is_empty());
    }
}
