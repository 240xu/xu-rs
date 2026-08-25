use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

// Compact slots still used by confirm ok/no and a few legacy hit-tests.
pub const ICON_BUTTON_WIDTH: u16 = 5;
pub const ICON_BUTTON_HEIGHT: u16 = 3;
pub const ICON_BUTTON_GAP: u16 = 1;

// CC Switch-like action chips: icon row + Chinese label, large enough for thumbs.
pub const TOOL_CHIP_WIDTH: u16 = 10;
pub const TOOL_CHIP_HEIGHT: u16 = 2;
pub const TOOL_CHIP_GAP: u16 = 1;

pub fn icon_button_rect(origin_x: u16, origin_y: u16, index: usize) -> Rect {
    Rect::new(
        origin_x + index as u16 * (ICON_BUTTON_WIDTH + ICON_BUTTON_GAP),
        origin_y,
        ICON_BUTTON_WIDTH,
        ICON_BUTTON_HEIGHT,
    )
}

/// Number of toolbar rows needed for `count` chips at a given terminal width.
/// When a row cannot fit on a narrow (portrait) screen the chips wrap to a
/// second row instead of overflowing horizontally off-screen.
pub fn toolbar_rows(width: u16, count: usize) -> usize {
    let step = TOOL_CHIP_WIDTH + TOOL_CHIP_GAP;
    let per_row = usize::from(width.saturating_sub(TOOL_CHIP_GAP) / step).max(1);
    if per_row >= count {
        1
    } else {
        count.div_ceil(per_row)
    }
}

/// Lays out `count` toolbar chips into one or more rows so every button stays
/// on-screen at narrow widths. Rendering and touch hit-testing must share this
/// same list so taps land on the same chips that are drawn.
pub fn toolbar_rects(
    origin_x: u16,
    origin_y: u16,
    available_width: u16,
    count: usize,
) -> Vec<Rect> {
    let step = TOOL_CHIP_WIDTH + TOOL_CHIP_GAP;
    let per_row = usize::from(available_width.saturating_sub(TOOL_CHIP_GAP) / step).max(1);
    (0..count)
        .map(|index| {
            let column = index % per_row;
            let row = index / per_row;
            Rect::new(
                origin_x + column as u16 * step,
                origin_y + row as u16 * (TOOL_CHIP_HEIGHT + TOOL_CHIP_GAP),
                TOOL_CHIP_WIDTH,
                TOOL_CHIP_HEIGHT,
            )
        })
        .collect()
}

pub fn point_in(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Graphical monochrome glyph drawn inside a chip (3x3 core, no "mystery blob").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Plus,
    Template,
    Import,
    Route,
    Edit,
    Delete,
    Verify,
    Update,
    Refresh,
    Backup,
    Local,
    Zip,
    Github,
    Ok,
    No,
    OpenCode,
    Claude,
    Codex,
    Generic,
}

impl Glyph {
    pub fn symbol(self) -> &'static str {
        // Single-cell glyphs. Meaning comes mainly from Chinese labels below.
        match self {
            Glyph::Plus => "+",
            Glyph::Template => "#",
            Glyph::Import => "↓",
            Glyph::Route => "=",
            Glyph::Edit => "~",
            Glyph::Delete => "x",
            Glyph::Verify => "√",
            Glyph::Update => "↑",
            Glyph::Refresh => "↻",
            Glyph::Backup => "B",
            Glyph::Local => "L",
            Glyph::Zip => "Z",
            Glyph::Github => "G",
            Glyph::Ok => "√",
            Glyph::No => "x",
            Glyph::OpenCode => "O",
            Glyph::Claude => "C",
            Glyph::Codex => "X",
            Glyph::Generic => "·",
        }
    }
}

pub struct ToolChip {
    pub glyph: Glyph,
    pub label: &'static str,
    pub active: bool,
    pub danger: bool,
}

impl Widget for ToolChip {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 {
            return;
        }
        // 紧凑单行变体（工具条 H=2）：`↻ 刷新`，无框；激活=面板明度底+白粗。
        if area.height < 3 {
            let style = if self.active {
                Style::default()
                    .fg(crate::ui::theme::INK)
                    .bg(crate::ui::theme::SEL_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(if self.danger {
                    crate::ui::theme::BAD
                } else {
                    crate::ui::theme::INK
                })
            };
            let glyph = self.glyph.symbol();
            let text = if self.label.is_empty() {
                glyph.to_string()
            } else {
                format!("{glyph} {}", self.label)
            };
            Paragraph::new(Span::styled(text, style))
                .alignment(Alignment::Center)
                .render(Rect::new(area.x, area.y, area.width, 1), buf);
            return;
        }
        let border = if self.active {
            crate::ui::theme::ACCENT
        } else if self.danger {
            crate::ui::theme::BAD
        } else {
            crate::ui::theme::G2
        };
        // 激活不再整块填充青色：仅细边框指示 + 白粗内容（克制派焦点语言）。
        let ink = if self.danger {
            crate::ui::theme::BAD
        } else {
            crate::ui::theme::INK
        };
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .style(if self.active {
                Style::default().bg(crate::ui::theme::SEL_BG)
            } else {
                Style::default()
            })
            .render(area, buf);

        // Inner content: symbol on top line, Chinese label below.
        // Use Paragraph so CJK double-width is handled correctly (no buffer panic).
        let glyph = self.glyph.symbol();
        let label = if self.label.is_empty() {
            glyph.to_string()
        } else {
            format!("{glyph}\n{}", self.label)
        };
        let inner = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        Paragraph::new(label)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ink).add_modifier(Modifier::BOLD))
            .render(inner, buf);
    }
}

/// 直角滑块开关：滑块在左=灰（关），在右=蓝（开）。8×2。
/// 开关行：图标格 + 标题 + 描述 + 右侧直角滑块开关。整行可点。
pub struct ToggleRow {
    pub title: &'static str,
    pub description: &'static str,
    pub on: bool,
    /// 键盘焦点高亮（图标格边框变亮）。
    pub focused: bool,
}

impl Widget for ToggleRow {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 16 || area.height < 2 {
            return;
        }
        // 克制派：状态块（█=开 青 / ░=关 暗）+ 标题/描述 + 右侧文字态，无框无滑杆。
        let state_block = Span::styled(
            if self.on { "█" } else { "░" },
            Style::default().fg(if self.on {
                crate::ui::theme::OK
            } else {
                crate::ui::theme::G2
            }),
        );
        Paragraph::new(Line::from(vec![state_block])).render(Rect::new(area.x, area.y, 1, 1), buf);

        let title_color = if self.focused || self.on {
            crate::ui::theme::INK
        } else {
            crate::ui::theme::G1
        };
        Paragraph::new(vec![
            Line::from(Span::styled(
                self.title,
                Style::default()
                    .fg(title_color)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                self.description,
                Style::default().fg(crate::ui::theme::G2),
            )),
        ])
        .render(
            Rect::new(area.x + 3, area.y, area.width.saturating_sub(6), 2),
            buf,
        );

        // 右侧文字态（行 0 右缘 2 列）。
        let state_word = if self.on {
            Span::styled(
                "开",
                Style::default()
                    .fg(crate::ui::theme::OK)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("关", Style::default().fg(crate::ui::theme::G2))
        };
        Paragraph::new(Line::from(vec![state_word])).render(
            Rect::new(area.x + area.width.saturating_sub(2), area.y, 2, 1),
            buf,
        );
    }
}

/// 扁圆角动作 chip（单行：glyph + 标签；高 3）。编辑/应用等紧凑操作按钮。
pub struct PillChip {
    pub glyph: Option<Glyph>,
    pub label: &'static str,
    pub active: bool,
    pub danger: bool,
    /// 键盘焦点高亮（边框变亮）。
    pub focused: bool,
    /// 自定义身份色（如每端颜色）；Some 时覆盖默认边框/文字色（active 优先）。
    pub color: Option<Color>,
}

impl Widget for PillChip {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 6 {
            return;
        }
        // 紧凑单行变体（动作条 H<3）：`编辑`，激活=青底黑字，焦点=加粗白。
        if area.height < 3 {
            let style = if self.active {
                Style::default()
                    .fg(crate::ui::theme::INK)
                    .bg(crate::ui::theme::SEL_BG)
                    .add_modifier(Modifier::BOLD)
            } else if self.danger {
                Style::default().fg(crate::ui::theme::BAD)
            } else if self.focused {
                Style::default()
                    .fg(crate::ui::theme::INK)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.color.unwrap_or(crate::ui::theme::G1))
            };
            let text = match (self.glyph, self.label.is_empty()) {
                (Some(g), false) => format!("{} {}", g.symbol(), self.label),
                (Some(g), true) => g.symbol().to_string(),
                (None, _) => self.label.to_string(),
            };
            Paragraph::new(Span::styled(text, style))
                .alignment(Alignment::Center)
                .render(Rect::new(area.x, area.y, area.width, 1), buf);
            return;
        }
        let (border, ink) = if self.active {
            (crate::ui::theme::ACCENT, crate::ui::theme::INK)
        } else if let Some(color) = self.color {
            (color, color)
        } else if self.focused {
            (crate::ui::theme::INK, crate::ui::theme::INK)
        } else if self.danger {
            (crate::ui::theme::BAD, crate::ui::theme::BAD)
        } else {
            (crate::ui::theme::G2, crate::ui::theme::INK)
        };
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .render(area, buf);
        let text = match self.glyph {
            Some(glyph) if !self.label.is_empty() => format!(" {} {}", glyph.symbol(), self.label),
            Some(glyph) => glyph.symbol().to_string(),
            None => self.label.to_string(),
        };
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(ink).add_modifier(Modifier::BOLD))
            .render(
                Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1),
                buf,
            );
    }
}

/// 按钮标签 → (glyph, 中文标签) 映射。纯函数供渲染与测试共用。
pub fn icon_button_spec(label: &'static str) -> (Glyph, &'static str) {
    match label {
        "+" | "＋" | "add" => (Glyph::Plus, "新增"),
        "tpl" | "preset" => (Glyph::Template, "模板"),
        "imp" | "import" => (Glyph::Import, "导入"),
        "ref" => (Glyph::Refresh, "刷新"),
        "route" | "multi" => (Glyph::Route, "路由"),
        "edit" => (Glyph::Edit, "编辑"),
        "del" | "✕" => (Glyph::Delete, "删除"),
        "uninst" => (Glyph::Delete, "卸载"),
        "no" => (Glyph::No, "取消"),
        "ok" | "✓" => (Glyph::Ok, "确认"),
        "test" => (Glyph::Generic, "测试"),
        "mdl" => (Glyph::Generic, "模型"),
        "oc" => (Glyph::OpenCode, "OpenCode"),
        "cl" => (Glyph::Claude, "Claude"),
        "cdx" => (Glyph::Codex, "Codex"),
        "doc" => (Glyph::Verify, "诊断"),
        "ver" => (Glyph::Update, "检查更新"),
        "all" => (Glyph::Update, "全部更新"),
        "set" => (Glyph::Generic, "设置"),
        "back" => (Glyph::Generic, "返回"),
        "local" => (Glyph::Local, "本地"),
        "zip" => (Glyph::Zip, "压缩包"),
        "gh" | "github" => (Glyph::Github, "仓库"),
        "upd" | "update" => (Glyph::Update, "更新"),
        "new" => (Glyph::Ok, "最新"),
        "bak" | "backup" => (Glyph::Backup, "备份"),
        "chk" | "verify" => (Glyph::Verify, "校验"),
        other => (Glyph::Generic, other),
    }
}

/// Legacy text chip used where we still pass short ASCII labels.
pub struct IconButton {
    pub label: &'static str,
    pub color: Color,
    pub active: bool,
}

impl Widget for IconButton {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let danger = self.color == crate::ui::theme::BAD;
        let (glyph, label) = icon_button_spec(self.label);
        ToolChip {
            glyph,
            label,
            active: self.active,
            danger,
        }
        .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_button_spec_labels_refresh_check_and_update_clearly() {
        assert_eq!(icon_button_spec("ref"), (Glyph::Refresh, "刷新"));
        assert_eq!(icon_button_spec("ver"), (Glyph::Update, "检查更新"));
        assert_eq!(icon_button_spec("all"), (Glyph::Update, "全部更新"));
        assert_eq!(icon_button_spec("upd"), (Glyph::Update, "更新"));
        assert_eq!(icon_button_spec("new"), (Glyph::Ok, "最新"));
        assert_eq!(icon_button_spec("set"), (Glyph::Generic, "设置"));
    }
}

// 契约测试追加在文件尾部（原 tests mod 之上不改动）。
#[cfg(test)]
mod token_tests {
    #[test]
    fn chips_use_theme_tokens_exclusively() {
        let src = include_str!("chips.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        // 仅允许：1 处导入 + PillChip/IconButton 公开字段的类型引用。
        assert_eq!(
            body.matches("Color").count(),
            3,
            "chips may only reference Color for public field types"
        );
    }
}
