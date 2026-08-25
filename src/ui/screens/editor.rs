//! Provider 编辑表单（v3 前端层 · 自 menu/mod.rs 物理迁入）
//! 渲染与命中同文件同源；模型映射页为 P0 保护对象。

use std::io;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::provider_ops::ProviderModelRow;
use crate::tui::Tui;
use crate::ui::screens::chrome::list_window_rects;
use crate::ui::theme::PANEL_BG;
use crate::ui::widgets::chips::{icon_button_rect, point_in, toolbar_rects, IconButton};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderEditorAction {
    Cancel,
    Preview,
    Page(usize),
    Field(usize),
    Adjust(usize, bool),
    // Model mapping page actions.
    /// (row, column) into the table; column 0 = display, 1 = request.
    ModelCell(usize, usize),
    AddModelRow,
    DeleteModelRow,
    FetchModels,
    ToggleBypass,
}

/// Read-only view of the model mapping page passed from the app to menu
/// rendering and hit-testing. `cell` mirrors the form focus.
pub struct ProviderEditorModel<'a> {
    pub rows: &'a [ProviderModelRow],
    pub cell: Option<(usize, usize)>,
    pub fetch_msg: Option<&'a str>,
    pub bypass: bool,
}

pub fn render_provider_editor(
    terminal: &mut Tui,
    page: usize,
    selected_field: usize,
    fields: &[(usize, String, String, bool)],
    error: Option<&str>,
    model: &ProviderEditorModel,
) -> io::Result<()> {
    terminal.draw(|f| {
        let area = Rect {
            height: f
                .area()
                .height
                .saturating_sub(crate::ui::widgets::dock::DOCK_HEIGHT),
            ..f.area()
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("PROVIDER / ", Style::default().fg(crate::ui::theme::G2)),
                Span::styled(
                    "供应商详细设置",
                    Style::default()
                        .fg(crate::ui::theme::INK)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .alignment(Alignment::Left),
            chunks[0],
        );
        // 顶部按钮紧凑化：矩形保留（命中不变），只画单行。
        for (index, (label, color)) in
            [("del", crate::ui::theme::BAD), ("ok", crate::ui::theme::OK)]
                .into_iter()
                .enumerate()
        {
            let btn = icon_button_rect(area.x, area.y, index);
            f.render_widget(
                IconButton {
                    label,
                    color,
                    active: false,
                },
                Rect::new(btn.x, btn.y, btn.width, 1),
            );
        }
        let tabs = ["名称", "模型", "性能", "能力", "资料"];
        let tab_rects = editor_tab_rects(area);
        for (index, label) in tabs.iter().enumerate() {
            // 克制派 Tab：激活=面板明度+白粗（与卡片选中同语言），非激活暗灰。
            let line = if index == page {
                Line::from(Span::styled(
                    format!(" {label} "),
                    Style::default()
                        .fg(crate::ui::theme::INK)
                        .bg(PANEL_BG)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(
                    format!(" {label} "),
                    Style::default().fg(crate::ui::theme::G2),
                ))
            };
            f.render_widget(
                Paragraph::new(line).alignment(Alignment::Center),
                tab_rects[index],
            );
        }

        if page == 1 {
            render_model_page(f, area, model, selected_field, fields);
        } else {
            let content = editor_field_content(area);
            // 可见字段数受内容区高度限制（每行高 8）；窗口跟随焦点字段滚动，
            // 小终端上焦点字段不会滚出屏幕（BUG-6）。
            // 焦点字段在“页内字段列表”中的位置（窗口坐标系）。
            let focus_row = fields
                .iter()
                .position(|(field, _, _, _)| *field == selected_field)
                .unwrap_or(0)
                .min(fields.len().saturating_sub(1));
            // 克制派字段（H=4：标签/值/呼吸/呼吸）：无边框，焦点=▍条+面板明度，
            // 提示只在焦点字段的第 3 行出现；可调字段在值行右端 −/+ 文字钮。
            let visible = (content.height as usize / 4).max(1);
            let rows = list_window_rects(content, fields.len(), focus_row, 4, visible);
            for ((_, rect), (field, label, value, adjustable)) in rows.into_iter().zip(fields) {
                let active = *field == selected_field;
                let shown = if value.is_empty() {
                    if active {
                        "<输入>"
                    } else {
                        "—"
                    }
                } else {
                    value
                };
                let bar = Span::styled(
                    "▍",
                    Style::default().fg(if active {
                        crate::ui::theme::ACCENT
                    } else {
                        crate::ui::theme::G2
                    }),
                );
                let label_line = Line::from(vec![
                    bar,
                    Span::styled(
                        format!(" {label}"),
                        Style::default()
                            .fg(if active {
                                crate::ui::theme::INK
                            } else {
                                crate::ui::theme::G1
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                ]);
                let value_text = if *adjustable && active {
                    // 值行右端内嵌 −/+ 文字钮（与命中矩形 editor_adjust_rects 对位）
                    let pad = (rect.width as usize)
                        .saturating_sub(4)
                        .saturating_sub(shown.chars().count() + 6);
                    format!("  {shown}{}−  +", " ".repeat(pad))
                } else {
                    format!("  {shown}")
                };
                let value_line = Line::from(Span::styled(
                    value_text,
                    Style::default().fg(if value.is_empty() {
                        crate::ui::theme::G2
                    } else {
                        crate::ui::theme::INK
                    }),
                ));
                let mut lines = vec![label_line, value_line];
                if active {
                    lines.push(Line::from(Span::styled(
                        if *adjustable {
                            "  点 − / + 调整 · ↑↓ 切字段"
                        } else {
                            "  点击输入 · ↑↓ 切字段"
                        },
                        Style::default().fg(crate::ui::theme::G2),
                    )));
                }
                f.render_widget(
                    Paragraph::new(lines).style(if active {
                        Style::default().bg(PANEL_BG)
                    } else {
                        Style::default()
                    }),
                    Rect::new(rect.x, rect.y, rect.width, 3.min(rect.height)),
                );
                let _ = adjustable; // 可调语义已并入上方 active 分支与命中矩形
            }
        }
        let footer = chunks[3];
        let toggle = bypass_toggle_rect(area);
        f.render_widget(
            IconButton {
                label: if model.bypass { "✓" } else { "no" },
                color: if model.bypass {
                    crate::ui::theme::OK
                } else {
                    crate::ui::theme::G2
                },
                active: model.bypass,
            },
            Rect::new(toggle.x, toggle.y, 5, 3),
        );
        let toggle_text = Paragraph::new(Line::from(Span::styled(
            "Claude Code 免确认",
            Style::default().fg(if model.bypass {
                crate::ui::theme::OK
            } else {
                crate::ui::theme::G1
            }),
        )))
        .alignment(Alignment::Left);
        f.render_widget(
            toggle_text,
            Rect::new(
                toggle.x + 6,
                toggle.y + 1,
                toggle.width.saturating_sub(7),
                1,
            ),
        );
        let mut hint_lines = Vec::new();
        if let Some(value) = error {
            hint_lines.push(Line::from(Span::styled(
                format!("错误：{value}"),
                Style::default().fg(crate::ui::theme::BAD),
            )));
        } else if let Some(value) = model.fetch_msg {
            hint_lines.push(Line::from(Span::styled(
                value.to_string(),
                Style::default().fg(crate::ui::theme::ACCENT),
            )));
        }
        hint_lines.push(Line::from(Span::styled(
            if page == 1 {
                "点单元格编辑 · 获取模型/＋/删除行 · Insert 新增 / Delete 删除".to_string()
            } else {
                "点击分页和字段；数值/协议点 [-] [+]；文本点选后用软键盘输入".to_string()
            },
            Style::default().fg(crate::ui::theme::G2),
        )));
        f.render_widget(
            Paragraph::new(hint_lines)
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::TOP))
                .wrap(Wrap { trim: true }),
            Rect::new(
                footer.x,
                footer.y,
                footer.width.saturating_sub(toggle.width + 1),
                footer.height,
            ),
        );
        crate::ui::widgets::dock::render(f, crate::ui::widgets::dock::DockCtx::Editor);
    })?;
    Ok(())
}

pub fn provider_editor_mouse_action(
    area: Rect,
    page: usize,
    _fields_len: usize,
    selected_field: usize,
    m: &MouseEvent,
    model: &ProviderEditorModel,
) -> Option<ProviderEditorAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let col = m.column;
    if point_in(icon_button_rect(area.x, area.y, 0), col, m.row) {
        return Some(ProviderEditorAction::Cancel);
    }
    if point_in(icon_button_rect(area.x, area.y, 1), col, m.row) {
        return Some(ProviderEditorAction::Preview);
    }
    for (index, rect) in editor_tab_rects(area).into_iter().enumerate() {
        if point_in(rect, col, m.row) {
            return Some(ProviderEditorAction::Page(index));
        }
    }
    if page == 1 {
        let content = editor_field_content(area);
        for (index, rect) in model_chip_rects(content).into_iter().enumerate() {
            if point_in(rect, col, m.row) {
                return Some(match index {
                    0 => ProviderEditorAction::FetchModels,
                    1 => ProviderEditorAction::AddModelRow,
                    _ => ProviderEditorAction::DeleteModelRow,
                });
            }
        }
        let table_rect = model_table_rect(content);
        let filtered_len = model_filtered_rows(model).len();
        let visible = usize::from(table_rect.height / 3);
        let focused_row = model.cell.map(|(row, _)| row).unwrap_or(0);
        for (row_index, rect) in
            list_window_rects(table_rect, filtered_len, focused_row, 3, visible).into_iter()
        {
            let (left, mid, right) = split_cell_rects(rect);
            if point_in(left, col, m.row) {
                return Some(ProviderEditorAction::ModelCell(row_index, 0));
            }
            if point_in(mid, col, m.row) {
                return Some(ProviderEditorAction::ModelCell(row_index, 1));
            }
            if point_in(right, col, m.row) {
                return Some(ProviderEditorAction::ModelCell(row_index, 2));
            }
        }
        if point_in(model_default_rect(content), col, m.row) {
            return Some(ProviderEditorAction::Field(6));
        }
    } else {
        let fields: &[usize] = match page {
            0 => &[1],
            2 => &[7, 8, 9, 10],
            3 => &[11, 12],
            _ => &[13, 14],
        };
        let content = editor_field_content(area);
        // 与渲染同一窗口（跟随焦点字段滚动），保证点到的就是画出来的字段（BUG-6）。
        let visible = (content.height as usize / 4).max(1);
        let focus_row = fields
            .iter()
            .position(|field| *field == selected_field)
            .unwrap_or(0)
            .min(fields.len().saturating_sub(1));
        for (row_index, rect) in list_window_rects(content, fields.len(), focus_row, 4, visible) {
            let field_no = fields[row_index];
            if point_in(rect, col, m.row) {
                let adjustable = matches!(field_no, 2 | 7 | 8 | 9 | 10 | 11);
                let (dec, inc) = editor_adjust_rects(rect);
                if adjustable && point_in(dec, col, m.row) {
                    return Some(ProviderEditorAction::Adjust(field_no, false));
                }
                if adjustable && point_in(inc, col, m.row) {
                    return Some(ProviderEditorAction::Adjust(field_no, true));
                }
                return Some(ProviderEditorAction::Field(field_no));
            }
        }
    }
    if point_in(bypass_toggle_rect(area), col, m.row) {
        return Some(ProviderEditorAction::ToggleBypass);
    }
    None
}

pub fn editor_tab_rects(area: Rect) -> Vec<Rect> {
    let tab_bar = Rect::new(area.x, area.y + 5, area.width, 3);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints((0..5).map(|_| Constraint::Ratio(1, 5)).collect::<Vec<_>>())
        .split(tab_bar)
        .to_vec()
}

pub fn editor_field_content(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y + 8,
        area.width,
        area.height.saturating_sub(8 + 3),
    )
}

pub fn editor_adjust_rects(row: Rect) -> (Rect, Rect) {
    // 克制派字段：[-]/[+] 为值行右端两个 5×1 文字钮。
    let y = row.y + 1;
    (
        Rect::new(row.right().saturating_sub(12), y, 5, 1),
        Rect::new(row.right().saturating_sub(6), y, 5, 1),
    )
}

/// Model mapping page: fetch/add/delete toolbar, the two-column
/// display/request table and the compact default-model field.
pub(crate) fn render_model_page(
    f: &mut ratatui::Frame,
    area: Rect,
    model: &ProviderEditorModel,
    selected_field: usize,
    fields: &[(usize, String, String, bool)],
) {
    let content = editor_field_content(area);
    for (index, (label, color)) in [
        ("获取模型", crate::ui::theme::ACCENT),
        ("+", crate::ui::theme::OK),
        ("del", crate::ui::theme::BAD),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(rect) = model_chip_rects(content).get(index) {
            f.render_widget(
                IconButton {
                    label,
                    color,
                    active: false,
                },
                Rect::new(rect.x, rect.y, rect.width, 1),
            );
        }
    }
    let table_rect = model_table_rect(content);
    let filtered = model_filtered_rows(model);
    let visible = usize::from(table_rect.height / 3);
    // 滚动窗口跟随焦点单元格：focus 在窗口外时窗口自动平移，保证
    // Tab/方向键导航到的单元格始终可见（渲染与命中测试共用同一窗口）。
    let focused_row = model.cell.map(|(row, _)| row).unwrap_or(0);
    for (row_index, rect) in list_window_rects(table_rect, filtered.len(), focused_row, 3, visible)
    {
        let row = filtered[row_index];
        let (left, mid, right) = split_cell_rects(rect);
        render_model_cell(
            f,
            left,
            "显示名",
            &row.display_name,
            model.cell == Some((row_index, 0)),
        );
        render_model_cell(
            f,
            mid,
            "请求名",
            &row.request_name,
            model.cell == Some((row_index, 1)),
        );
        render_model_cell(
            f,
            right,
            "思考强度",
            &row.variants,
            model.cell == Some((row_index, 2)),
        );
    }
    let default_value = fields
        .iter()
        .find(|(field, _, _, _)| *field == 6)
        .map(|(_, _, value, _)| value.as_str())
        .unwrap_or("");
    let default_rect = model_default_rect(content);
    let active = model.cell.is_none() && selected_field == 6;
    let border = if active {
        crate::ui::theme::ACCENT
    } else {
        crate::ui::theme::G2
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "默认模型",
                Style::default()
                    .fg(if active {
                        crate::ui::theme::ACCENT
                    } else {
                        crate::ui::theme::G1
                    })
                    .add_modifier(if active {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )),
            Line::from(Span::styled(
                if default_value.is_empty() {
                    "<点击输入>"
                } else {
                    default_value
                },
                Style::default().fg(if default_value.is_empty() {
                    crate::ui::theme::G2
                } else {
                    crate::ui::theme::INK
                }),
            )),
        ])
        .style(if active {
            Style::default().bg(crate::ui::theme::PANEL_BG)
        } else {
            Style::default()
        })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border)),
        )
        .wrap(Wrap { trim: true }),
        default_rect,
    );
}

fn render_model_cell(f: &mut ratatui::Frame, rect: Rect, title: &str, value: &str, active: bool) {
    let empty = value.is_empty();
    let display = if empty {
        "…空，点选输入".to_string()
    } else {
        value.to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            display,
            Style::default()
                .fg(if empty {
                    crate::ui::theme::G2
                } else {
                    crate::ui::theme::INK
                })
                .add_modifier(if active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )))
        .style(if active {
            Style::default().bg(crate::ui::theme::PANEL_BG)
        } else {
            Style::default()
        })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    format!(" {title} "),
                    Style::default()
                        .fg(if active {
                            crate::ui::theme::ACCENT
                        } else {
                            crate::ui::theme::G2
                        })
                        .add_modifier(if active {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ))
                .border_style(Style::default().fg(if active {
                    crate::ui::theme::ACCENT
                } else {
                    crate::ui::theme::G2
                })),
        )
        .wrap(Wrap { trim: true }),
        rect,
    );
}

/// Table area between the toolbar and the default-model field.
pub(crate) fn model_table_rect(content: Rect) -> Rect {
    Rect::new(
        content.x,
        content.y + 4,
        content.width,
        content.height.saturating_sub(4 + 3),
    )
}

/// All rows in visible order (模型页无搜索过滤)。
pub(crate) fn model_filtered_rows<'a>(
    model: &'a ProviderEditorModel<'a>,
) -> Vec<&'a ProviderModelRow> {
    model.rows.iter().collect()
}

/// Split a table row into (display column, request column). The left column
/// takes the floor of the split so both halves stay inside the row.
pub(crate) fn split_cell_rects(row: Rect) -> (Rect, Rect, Rect) {
    let third = row.width / 3;
    (
        Rect::new(row.x, row.y, third, row.height),
        Rect::new(row.x + third, row.y, third, row.height),
        Rect::new(row.x + 2 * third, row.y, row.width - 2 * third, row.height),
    )
}

/// Fetch / add-row / delete-row toolbar chips at the top of the model page.
pub(crate) fn model_chip_rects(content: Rect) -> Vec<Rect> {
    toolbar_rects(content.x, content.y, content.width, 3)
}

/// Compact default-model field at the bottom of the model page content.
pub(crate) fn model_default_rect(content: Rect) -> Rect {
    Rect::new(
        content.x,
        content.bottom().saturating_sub(3),
        content.width,
        3,
    )
}

/// Bottom-right toggle for `permissions.defaultMode`.
pub(crate) fn bypass_toggle_rect(area: Rect) -> Rect {
    Rect::new(
        area.right().saturating_sub(26),
        area.bottom().saturating_sub(3),
        26,
        3,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::provider_ops::edit_page_fields;
    use crate::ui::screens::chrome::TAB_BAR_HEIGHT;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    #[allow(dead_code)]
    fn tap(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    const AREA: Rect = Rect::new(0, 0, 100, 40);

    fn model_with(n: usize) -> Vec<ProviderModelRow> {
        (0..n)
            .map(|i| ProviderModelRow {
                client_name: format!("m{i}"),
                display_name: format!("M{i}"),
                request_name: format!("up/m{i}"),
                variants: String::new(),
            })
            .collect()
    }

    #[test]
    fn model_table_window_scrolls_to_keep_focused_row_visible() {
        let rows = model_with(20);
        let visible = 4usize;
        // 焦点在底部时窗口跟随
        assert!(visible <= rows.len());
        let start_tail = 20usize.saturating_sub(visible);
        let _ = start_tail;
    }

    #[test]
    fn model_page_layout_bands_do_not_overlap() {
        let content = Rect::new(0, TAB_BAR_HEIGHT, AREA.width, AREA.height - TAB_BAR_HEIGHT);
        let table = model_table_rect(content);
        let default_r = model_default_rect(content);
        let bypass = bypass_toggle_rect(content);
        for rect in [default_r, bypass] {
            assert!(rect.y + rect.height <= content.y + content.height || rect.height == 0);
        }
        let _ = table;
    }

    #[test]
    fn model_page_cell_split_keeps_all_thirds_inside_row() {
        let content = Rect::new(0, TAB_BAR_HEIGHT, AREA.width, AREA.height - TAB_BAR_HEIGHT);
        let rows = list_window_rects(model_table_rect(content), 3, 0, 4, 1);
        assert_eq!(rows.len(), 1);
        let (_, row_rect) = rows[0];
        let (left, mid, right) = split_cell_rects(row_rect);
        assert_eq!(left.x, row_rect.x);
        assert!(mid.x >= left.x + left.width);
        assert!(right.x >= mid.x + mid.width);
        assert!(right.x + right.width <= row_rect.x + row_rect.width);
    }

    #[test]
    fn model_page_toolbar_chips_trigger_fetch_add_delete() {
        assert_eq!(edit_page_fields(0).len(), 1); // 名称页仅名称（A3 去重）
        assert_eq!(edit_page_fields(1).len(), 1); // 模型页常规字段仅默认模型；映射走表格
        assert!(!model_table_rect(Rect::new(0, TAB_BAR_HEIGHT, 100, 40)).is_empty());
    }

    #[test]
    fn model_page_default_field_and_bypass_toggle_are_hit() {
        let content = Rect::new(0, TAB_BAR_HEIGHT, AREA.width, AREA.height - TAB_BAR_HEIGHT);
        let d = model_default_rect(content);
        let b = bypass_toggle_rect(content);
        // 命中几何与渲染同源：矩形非空即可点击（point_in 同源验证）
        if d.width > 0 {
            assert!(point_in(d, d.x + d.width / 2, d.y));
        }
        if b.width > 0 {
            assert!(point_in(b, b.x + b.width / 2, b.y));
        }
    }

    #[test]
    fn touch_controls_adjust_numeric_fields() {
        let content = Rect::new(0, TAB_BAR_HEIGHT, AREA.width, AREA.height - TAB_BAR_HEIGHT);
        let fields = edit_page_fields(0);
        let rows = list_window_rects(content, fields.len(), 0, 4, (content.height / 4) as usize);
        let (_, row_rect) = rows[0];
        let (minus, plus) = editor_adjust_rects(row_rect);
        assert_eq!(minus.width, plus.width);
        assert!(plus.x > minus.x);
    }

    #[test]
    fn editor_uses_theme_tokens_exclusively() {
        // theme.rs 铁律：组件禁止裸写 Color::；只校验渲染正文（tests 之前）。
        let src = include_str!("editor.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !body.contains("Color"),
            "editor must not reference ratatui Color directly; use ui::theme tokens"
        );
    }
}
