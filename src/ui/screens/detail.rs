//! Provider 详情页（v3 前端层）
//! 渲染与命中同文件同源；模型映射为 P0 保护对象。

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::provider_ops::{provider_model_rows, DETAIL_SLOT_NAMES};
use crate::tui::Tui;
use crate::ui::screens::common::render_placeholder;
use crate::ui::widgets::chips::ToggleRow;
use crate::ui::widgets::chips::{point_in, toolbar_rects};

use trivium::domain::{AgentTarget, ProtocolKind, ProviderProfile};
use trivium::state::ProviderHealth;

use crate::ui::screens::providers::{ProviderDetailAction, ProviderMode};
use crate::ui::theme;
use crate::ui::widgets::chips::{Glyph, PillChip, ToolChip};

/// 模型表格区引导文案：改名/思考强度收敛到编辑表单「模型」tab。
pub const DETAIL_MODEL_EDIT_HINT: &str = "改显示名/思考强度请到 编辑 → 模型；此处仅拉取与勾选模型";

/// 详情页拉取模型列表的交互语义（决定 overlay 的点击/回车行为）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailPickMode {
    /// 无拉取列表交互（fetch 未打开时）。
    None,
    /// 槽位展开选择：选中项写入 `--claude-slot <slot>=<model>`。
    Slot(usize),
    /// 获取模型多选：空格/点击勾选，Enter 保存 models。
    MultiSel,
}

/// 详情页开关区当前状态（主循环按当前端 Tab 计算后传入）。
#[derive(Clone, Copy, Debug, Default)]
pub struct DetailSwitches {
    /// Claude=settings permissions.defaultMode==bypassPermissions；
    /// OpenCode=opencode.json permission=="allow"。仅当前端相关值有效。
    pub permission: bool,
    /// Claude settings env CLAUDE_CODE_DISABLE_TERMINAL_TITLE=="1"。
    pub terminal_title: bool,
    /// xu-client-routes.json clients.<agent>.enabled 且 providers 含当前供应商。
    pub route: bool,
}

/// 详情页可编辑输入框状态（来自主循环）。
pub struct DetailView<'a> {
    /// 正在编辑的字段（0端点/1密钥/2默认模型/3备注/4..7 Claude 槽位/8超时/9重试）。
    pub field: Option<usize>,
    /// 编辑缓冲。
    pub buf: &'a str,
    /// 拉取模型列表（Some = 拉取选择模式）。
    pub fetch: Option<&'a [String]>,
    /// 拉取列表光标。
    pub fetch_sel: usize,
    /// 当前端 Tab（Claude Code / Codex / OpenCode）。
    pub agent_tab: AgentTarget,
    /// 拉取列表交互语义。
    pub pick: DetailPickMode,
    /// 获取模型多选已勾选的下标集合（初始=当前 models）。
    pub multi_sel: &'a BTreeSet<usize>,
    /// 密钥明文显示（T-E）。
    pub secret_visible: bool,
    /// 底部反馈消息（空 = 显示常规键位提示）。
    pub feedback: &'a str,
    /// 模型列表后台拉取中（显示加载提示）。
    pub fetch_loading: bool,
    /// 键盘焦点（None = 无焦点；zone 0=协议 1=开关 2=操作行 3=模型表格）。
    pub focus_zone: Option<u8>,
    /// 焦点在区内索引。
    pub focus_index: usize,
}

/// 详情页模型表格可见行数（供键盘焦点区元素数计算）。
pub fn detail_table_visible_rows(area: Rect) -> usize {
    let chunks = detail_page_layout(area);
    chunks[6].height.saturating_sub(3) as usize
}

/// 详情页键盘焦点：各区域元素数（协议/开关/操作/表格可见行）。
pub fn detail_focus_counts(tab: AgentTarget, models_visible: usize) -> [usize; 4] {
    let protocols = match tab {
        AgentTarget::ClaudeCode => 3,
        AgentTarget::Codex | AgentTarget::OpenCode => 2,
        AgentTarget::Other => 0,
    };
    let toggles = match tab {
        AgentTarget::ClaudeCode => 3,
        AgentTarget::Codex => 1,
        AgentTarget::OpenCode => 2,
        AgentTarget::Other => 0,
    };
    [protocols, toggles, 5, models_visible.max(1)]
}

pub fn detail_focus_action(
    tab: AgentTarget,
    zone: u8,
    index: usize,
    mode: ProviderMode,
) -> Option<ProviderDetailAction> {
    match zone {
        0 => protocol_chips_for(tab)
            .get(index)
            .copied()
            .map(ProviderDetailAction::SetProtocol),
        1 => match tab {
            AgentTarget::ClaudeCode => match index {
                0 => Some(ProviderDetailAction::TogglePermission),
                1 => Some(ProviderDetailAction::ToggleTerminalTitle),
                _ => Some(ProviderDetailAction::ToggleRoute),
            },
            AgentTarget::Codex => Some(ProviderDetailAction::ToggleRoute),
            AgentTarget::OpenCode => match index {
                0 => Some(ProviderDetailAction::TogglePermission),
                _ => Some(ProviderDetailAction::ToggleRoute),
            },
            AgentTarget::Other => None,
        },
        2 => match index {
            0 => Some(ProviderDetailAction::Edit),
            1 => Some(ProviderDetailAction::Test),
            2 => Some(ProviderDetailAction::Models),
            3 => Some(ProviderDetailAction::SelectMode(ProviderMode::Multi)),
            _ => Some(ProviderDetailAction::Apply(tab)),
        },
        3 => None, // 模型表格行不再进入改名：编辑入口收敛到「编辑」→「模型」。
        _ => None,
    }
    .map(|action| match action {
        ProviderDetailAction::SelectMode(ProviderMode::Multi) if mode == ProviderMode::Multi => {
            ProviderDetailAction::EnterMulti
        }
        other => other,
    })
}

/// 详情页竖向分区：头部 / 端点信息 / 三端 Tab 条 / 协议区 / 槽位·高级区 /
/// 开关区 / 模型表格 / 操作 chips / 应用行 / 底部提示。渲染与命中测试共用此布局。
pub fn detail_page_layout(area: Rect) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // 头部：名称 + 健康
            Constraint::Length(7), // 端点信息：标题 + 两对字段
            Constraint::Length(2), // 三端 Tab（文字 + 下划线）
            Constraint::Length(3), // 协议：标题 + chips
            Constraint::Length(7), // 槽位/高级：标题 + 两对字段
            Constraint::Length(3), // 开关三行
            Constraint::Min(2),    // 模型表格（弹性）
            Constraint::Length(3), // 动作条
            Constraint::Length(1), // 页脚提示
            Constraint::Length(1), // 页脚反馈
        ])
        .split(area)
        .to_vec()
}

/// 三端 Tab 按钮矩形（Claude Code / Codex / OpenCode，均分并吸收余数）。
pub fn detail_tab_rects(area: Rect) -> Vec<(AgentTarget, Rect)> {
    let band = detail_page_layout(area)[2];
    let count = 3u16;
    [
        AgentTarget::ClaudeCode,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, target)| {
        let start = band.x + band.width * index as u16 / count;
        let end = band.x + band.width * (index as u16 + 1) / count;
        (target, Rect::new(start, band.y, end - start, band.height))
    })
    .collect()
}

/// 详情页输入框（字段, 区域）：公共区 4 个（端点|密钥 / 默认模型|备注）+
/// Claude 槽位区 4 个（Fable|Opus / Sonnet|Haiku，右侧留 ▸ 按钮位）。
pub fn detail_input_rects(area: Rect) -> Vec<(usize, Rect)> {
    let chunks = detail_page_layout(area);
    let half = chunks[1].width / 2;
    let mut out = Vec::new();
    for (section, fields) in [(chunks[1], [0usize, 1, 2, 3]), (chunks[4], [4, 5, 6, 7])] {
        let right_x = section.x + half;
        let right_w = section.width - half;
        let shrink = usize::from(section.y == chunks[4].y) as u16 * 4;
        for (row, pair) in fields.chunks(2).enumerate() {
            let y = section.y + 1 + row as u16 * 3;
            out.push((pair[0], Rect::new(section.x, y, half - shrink, 3)));
            out.push((pair[1], Rect::new(right_x, y, right_w - shrink, 3)));
        }
    }
    out
}

/// 高级区输入框（timeout_ms=8 | max_retries=9），Codex/OpenCode Tab 使用。
pub fn detail_advanced_rects(area: Rect) -> Vec<(usize, Rect)> {
    let chunks = detail_page_layout(area);
    let section = chunks[4];
    let half = section.width / 2;
    vec![
        (8, Rect::new(section.x, section.y + 1, half, 3)),
        (9, Rect::new(section.x + half, section.y + 1, half, 3)),
    ]
}

/// Claude 槽位输入框右侧「▸」展开按钮（槽位, 区域）。
pub fn detail_slot_pick_rects(area: Rect) -> Vec<(usize, Rect)> {
    let chunks = detail_page_layout(area);
    let section = chunks[4];
    let half = section.width / 2;
    let mut out = Vec::new();
    for (slot, row, col) in [(0usize, 0usize, 0usize), (1, 0, 1), (2, 1, 0), (3, 1, 1)] {
        let x = section.x + col as u16 * half + half - 4;
        let y = section.y + 1 + row as u16 * 3;
        out.push((slot, Rect::new(x, y, 4, 3)));
    }
    out
}

/// 拉取模型列表 overlay 区域：协议区 + 槽位/高级区 + 开关区 + 模型表格全覆盖。
pub fn detail_overlay_rect(area: Rect) -> Rect {
    let chunks = detail_page_layout(area);
    let top = chunks[3];
    let bottom = chunks[6];
    Rect::new(top.x, top.y, top.width, bottom.y + bottom.height - top.y)
}

/// 圆角输入框（搜索框同款样式）：聚焦青色边框，失焦灰边；空值显示「点击输入」。
pub fn render_detail_input(
    f: &mut ratatui::Frame,
    rect: Rect,
    label: &str,
    value: &str,
    focused: bool,
) {
    // 方向 C：无框字段。行0=▍标签，行1=值（聚焦=面板底 + 青条 + 光标）。
    let base = if focused {
        Style::default().bg(theme::PANEL_BG)
    } else {
        Style::default()
    };
    let label_ink = if focused { theme::ACCENT } else { theme::G2 };
    let bar = if focused { "▍" } else { " " };
    let (value_text, value_ink) = if value.is_empty() {
        (
            if focused {
                "▏".to_string()
            } else {
                "点击输入".to_string()
            },
            if focused {
                crate::ui::theme::INK
            } else {
                crate::ui::theme::G2
            },
        )
    } else {
        (
            if focused {
                format!("{value}▏")
            } else {
                value.to_string()
            },
            if focused {
                crate::ui::theme::INK
            } else {
                crate::ui::theme::G1
            },
        )
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![Span::styled(
                format!("{bar}{label}"),
                Style::default().fg(label_ink),
            )])
            .style(base),
            Line::from(Span::styled(value_text, Style::default().fg(value_ink))).style(base),
        ]),
        rect,
    );
}

/// 详情页字段显示值（未编辑时）：密钥脱敏（y 可切换明文）；其余读 provider 现值。
pub fn detail_field_display(
    provider: &ProviderProfile,
    field: usize,
    secret_visible: bool,
) -> String {
    match field {
        1 => {
            if provider.api_key.is_empty() {
                String::new()
            } else if secret_visible {
                provider.api_key.clone()
            } else {
                format!("{}…（y 显示明文）", masked_key(&provider.api_key))
            }
        }
        _ => crate::app::provider_ops::detail_field_value(provider, field),
    }
}

/// 供应商详情页（v2 三端 Tab 版）：公共端点信息 / Claude Code│Codex│OpenCode
/// 三端独立布局（协议 / 槽位·高级 / 开关 / 模型表格）/ 操作 chips /
/// 应用行 + bypass / 底部提示。
#[allow(clippy::too_many_arguments)]
pub fn render_provider_detail_v2(
    terminal: &mut Tui,
    provider: Option<&ProviderProfile>,
    health: &BTreeMap<String, ProviderHealth>,
    mode: ProviderMode,
    runtime_ok: bool,
    view: &DetailView<'_>,
    switches: &DetailSwitches,
) -> io::Result<()> {
    let Some(provider) = provider else {
        return render_placeholder(terminal, "供应商详情", "没有选中的供应商。Esc 返回。");
    };
    let models = provider_model_rows(provider);
    let tab = view.agent_tab;
    terminal.draw(|f| {
        // 让位底部拇指坞（2 行）
        let area = Rect {
            height: f
                .area()
                .height
                .saturating_sub(crate::ui::widgets::dock::DOCK_HEIGHT),
            ..f.area()
        };
        // 方向 C 无框化后留白增多：整帧清屏，避免上一帧残像。
        f.render_widget(Clear, area);
        let chunks = detail_page_layout(area);
        let input_rects = detail_input_rects(area);
        let advanced_rects = detail_advanced_rects(area);
        let slot_pick_rects = detail_slot_pick_rects(area);
        let input_rect = |field: usize| {
            input_rects
                .iter()
                .find(|(candidate, _)| *candidate == field)
                .map(|(_, rect)| *rect)
        };
        let advanced_rect = |field: usize| {
            advanced_rects
                .iter()
                .find(|(candidate, _)| *candidate == field)
                .map(|(_, rect)| *rect)
        };
        let slot_pick_rect = |slot: usize| {
            slot_pick_rects
                .iter()
                .find(|(candidate, _)| *candidate == slot)
                .map(|(_, rect)| *rect)
        };

        let health_text = health
            .get(&provider.id)
            .map(|value| {
                format!("{} · {}", compact_health(value), value.message)
            })
            .unwrap_or_else(|| "未测试（不会后台联网）".to_string());
        f.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        provider.name.as_str(),
                        Style::default()
                            .fg(crate::ui::theme::INK)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        protocol_label(provider.protocol),
                        Style::default().fg(crate::ui::theme::G1),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        format!("{} 个模型", provider.models.len()),
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                ]),
                Line::from(Span::styled(
                    format!("健康：{health_text}"),
                    Style::default().fg(health
                        .get(&provider.id)
                        .map(|value| if value.ok { crate::ui::theme::OK } else { crate::ui::theme::BAD })
                        .unwrap_or(crate::ui::theme::G2)),
                )),
            ]),
            chunks[0],
        );

                // 三端 Tab 条。
        for (target, rect) in detail_tab_rects(area) {
            let active = target == tab;
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {} ", target.label()),
                    Style::default()
                        .fg(if active { crate::ui::theme::INK } else { crate::ui::theme::G2 })
                        .add_modifier(if active {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                )))
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_style(Style::default().fg(if active {
                            crate::ui::theme::ACCENT
                        } else {
                            crate::ui::theme::G2
                        })),
                ),
                rect,
            );
        }

// 端点信息区（公共）：标题 + 两行圆角输入框（端点|密钥 / 默认模型|备注）。
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                        format!("▎ {}", "端点信息"),
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                Span::styled(
                    "  点击输入框编辑 · Enter 保存 · Esc 取消 · 密钥 y 显示/隐藏",
                    Style::default().fg(crate::ui::theme::G2),
                ),
            ])),
            Rect::new(chunks[1].x, chunks[1].y, chunks[1].width, 1),
        );
        for (label, field) in [("端点", 0usize), ("密钥", 1), ("默认模型", 2), ("备注", 3)]
        {
            let value = if view.field == Some(field) {
                view.buf.to_string()
            } else {
                detail_field_display(provider, field, view.secret_visible)
            };
            if let Some(rect) = input_rect(field) {
                render_detail_input(f, rect, label, &value, view.field == Some(field));
            }
        }

        // 协议区：每端一枚紧凑协议 chip（glyph + 标签单行，3 高圆角框）。
        f.render_widget(Clear, chunks[3]);
        let protocol_title = match tab {
            AgentTarget::ClaudeCode => "协议",
            AgentTarget::Codex => "协议",
            AgentTarget::OpenCode => "协议",
            AgentTarget::Other => "",
        };
        f.render_widget(
             Paragraph::new(Line::from(vec![
                Span::styled(
                        format!("▎ {}", protocol_title),
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                Span::styled(
                    "  接入当前端的 API 类型 · 应用到某端＝写入其配置",
                    Style::default().fg(crate::ui::theme::G2),
                ),
                Span::styled(
                    format!(
                        "   ·  缓存模式 {}",
                        cache_mode_label(provider)
                    ),
                    Style::default().fg(if cache_mode_is_deepseek(provider) {
                        crate::ui::theme::OK
                    } else {
                        crate::ui::theme::G2
                    }),
                ),
            ])),
            Rect::new(chunks[3].x, chunks[3].y, chunks[3].width, 1),
        );
        let protocol_rects = protocol_chip_rects(area);
        for (index, kind) in protocol_chips_for(tab).iter().enumerate() {
            let rect = protocol_rects[index];
            let active = provider.protocol == *kind;
            let _focused = view.focus_zone == Some(0) && view.focus_index == index;
            // 克制派协议选择：文字态（激活=面板明度底+白粗），命中矩形不变。
            let text = format!(" {} ", protocol_chip_label(*kind, tab));
            let style = if active {
                Style::default()
                    .fg(crate::ui::theme::INK)
                    .bg(crate::ui::theme::SEL_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(crate::ui::theme::G2)
            };
            f.render_widget(
                Paragraph::new(Span::styled(text, style)),
                Rect::new(rect.x, rect.y, rect.width, 1),
            );
        }
        if tab == AgentTarget::OpenCode {
            let rect = protocol_rects[2];
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " Anthropic 不支持",
                    Style::default().fg(crate::ui::theme::G2),
                ))),
                Rect::new(rect.x, rect.y, rect.width, 1),
            );
        }

        // 槽位区（Claude）/ 高级区（Codex·OpenCode）。
        if tab == AgentTarget::ClaudeCode {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("▎ {}", "Claude 槽位映射"),
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                    Span::styled(
                        "  保存写 claudeSlots · 未设置时默认当前模型 · ▸ 从上游选择",
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                ])),
                Rect::new(chunks[4].x, chunks[4].y, chunks[4].width, 1),
            );
            for (index, label) in ["Fable", "Opus", "Sonnet", "Haiku"].iter().enumerate() {
                let field = 4 + index;
                let value = if view.field == Some(field) {
                    view.buf.to_string()
                } else {
                    detail_field_display(provider, field, view.secret_visible)
                };
                if let Some(rect) = input_rect(field) {
                    render_detail_input(f, rect, label, &value, view.field == Some(field));
                }
                if let Some(rect) = slot_pick_rect(index) {
                    f.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            "▍▸",
                            Style::default().fg(theme::ACCENT),
                        )))
                        .alignment(ratatui::layout::Alignment::Right),
                        rect,
                    );
                }
            }
        } else {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("▎ {}", "高级"),
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                    Span::styled(
                        "  超时与重试（provider update --timeout / --max-retries）",
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                ])),
                Rect::new(chunks[4].x, chunks[4].y, chunks[4].width, 1),
            );
            for (label, field) in [("timeout_ms", 8usize), ("max_retries", 9)] {
                let value = if view.field == Some(field) {
                    view.buf.to_string()
                } else {
                    detail_field_display(provider, field, view.secret_visible)
                };
                if let Some(rect) = advanced_rect(field) {
                    render_detail_input(f, rect, label, &value, view.field == Some(field));
                }
            }
        }

        // 开关区：每端一列开关行（图标 + 标题 + 描述 + 开/关 pill，整行可点）。
        let toggle_rows = toggle_row_rects(area);
        let switches_rows: Vec<(ProviderDetailAction, ToggleRow)> = match tab {
            AgentTarget::ClaudeCode => vec![
                (
                    ProviderDetailAction::TogglePermission,
                    ToggleRow {
                        title: "完全权限",
                        description: "绕过权限审批直接运行",
                        on: switches.permission,
                        focused: false,
                    },
                ),
                (
                    ProviderDetailAction::ToggleTerminalTitle,
                    ToggleRow {
                        title: "会话标题",
                        description: "终端标题不显示会话名",
                        on: switches.terminal_title,
                        focused: false,
                    },
                ),
                (
                    ProviderDetailAction::ToggleRoute,
                    ToggleRow {
                        title: "路由",
                        description: "作为当前端默认路由",
                        on: switches.route,
                        focused: false,
                    },
                ),
            ],
            AgentTarget::Codex => vec![(
                ProviderDetailAction::ToggleRoute,
                ToggleRow {
                    title: "路由",
                    description: "作为当前端默认路由",
                    on: switches.route,
                    focused: false,
                },
            )],
            AgentTarget::OpenCode => vec![
                (
                    ProviderDetailAction::TogglePermission,
                    ToggleRow {
                        title: "完全权限",
                        description: "允许执行，不逐项确认",
                        on: switches.permission,
                        focused: false,
                    },
                ),
                (
                    ProviderDetailAction::ToggleRoute,
                    ToggleRow {
                        title: "生效",
                        description: "把供应商写入 opencode.json",
                        on: switches.route,
                        focused: false,
                    },
                ),
            ],
            AgentTarget::Other => Vec::new(),
        };
        f.render_widget(Clear, chunks[5]);
        for (index, ((_action, mut row), rect)) in
            switches_rows.into_iter().zip(toggle_rows.iter()).enumerate()
        {
            row.focused = view.focus_zone == Some(1) && view.focus_index == index;
            f.render_widget(row, *rect);
        }

        // 模型表格 / 拉取列表 overlay（覆盖协议区到表格）。
        let overlay = detail_overlay_rect(area);
        if view.fetch_loading && view.fetch.is_none() {
            // 后台拉取中：先给明确提示（避免重复点击）。
            f.render_widget(Clear, overlay);
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        "正在获取模型列表…",
                        Style::default()
                            .fg(crate::ui::theme::INK)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        "首次拉取会联网（可能较慢）；完成后写入本地缓存，之后秒开。",
                        Style::default().fg(crate::ui::theme::G2),
                    )),
                ]),
                Rect::new(
                    overlay.x,
                    overlay.y,
                    overlay.width,
                    overlay.height.min(2),
                ),
            );
        } else if let Some(fetch) = view.fetch {
            f.render_widget(Clear, overlay);
            let (mode_hint, save_rect) = match view.pick {
                DetailPickMode::MultiSel => (
                    format!(" · 已选 {} 个", view.multi_sel.len()),
                    // 保存 chip 占 overlay 底部 3 行（不遮挡任何模型行）。
                    Some(Rect::new(
                        overlay.x + overlay.width.saturating_sub(12),
                        overlay.y + overlay.height.saturating_sub(3),
                        12,
                        3,
                    )),
                ),
                DetailPickMode::Slot(slot) => (
                    format!(" · 点击填入槽位 {}", DETAIL_SLOT_NAMES[slot].1),
                    None,
                ),
                DetailPickMode::None => (String::new(), None),
            };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        "拉取结果",
                        Style::default()
                            .fg(crate::ui::theme::INK)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" · 共 {} 个模型{mode_hint}", fetch.len()),
                        Style::default().fg(crate::ui::theme::G2),
                    ),
                ])),
                Rect::new(
                    overlay.x,
                    overlay.y,
                    overlay.width.saturating_sub(12),
                    1,
                ),
            );
            if let Some(rect) = save_rect {
                f.render_widget(
                    ToolChip {
                        glyph: Glyph::Ok,
                        label: "保存",
                        active: false,
                        danger: false,
                    },
                    rect,
                );
            }
            // 行区起点在标题下 1 行；多选模式底部 3 行留给保存 chip，不重叠（BUG-7）。
            let row_area_bottom = if save_rect.is_some() { 4 } else { 1 };
            let visible = usize::from(
                overlay
                    .height
                    .saturating_sub(row_area_bottom),
            );
            let start = view.fetch_sel.saturating_sub(visible.saturating_sub(1));
            for (offset, index) in (start..start + visible).enumerate() {
                let Some(model) = fetch.get(index) else { break };
                let y = overlay.y + 1 + offset as u16;
                let selected = index == view.fetch_sel;
                let checked = match view.pick {
                    DetailPickMode::MultiSel => view.multi_sel.contains(&index),
                    _ => false,
                };
                // 单选模式（槽位/模型行展开）不渲染 [ ] 勾选框：
                // 那会误导用户以为「点勾选中」——单选语义下点击行/回车即选中写入。
                let mark = if checked {
                    "[✓] "
                } else if matches!(
                    view.pick,
                    DetailPickMode::MultiSel
                ) {
                    "[ ] "
                } else {
                    "    "
                };
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            if selected { "▸ " } else { "  " },
                            Style::default().fg(crate::ui::theme::ACCENT),
                        ),
                        Span::styled(
                            mark,
                            Style::default()
                                .fg(if checked { crate::ui::theme::OK } else { crate::ui::theme::G2 }),
                        ),
                        Span::styled(
                            format!("{}. ", index + 1),
                            Style::default().fg(crate::ui::theme::G2),
                        ),
                        Span::styled(
                            model,
                            Style::default()
                                .fg(if selected { crate::ui::theme::INK } else { crate::ui::theme::G1 }),
                        ),
                    ])),
                    Rect::new(overlay.x, y, overlay.width, 1),
                );
            }
        } else {
            let table_area = chunks[6];
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled(
                            "模型表格",
                            Style::default()
                                .fg(crate::ui::theme::INK)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "  显示名 | 请求名",
                            Style::default().fg(crate::ui::theme::G2),
                        ),
                    ]),
                    Line::from(Span::styled(
                        DETAIL_MODEL_EDIT_HINT,
                        Style::default().fg(crate::ui::theme::G2),
                    )),
                    Line::from(Span::styled(
                        format!("{} | {}", "显示名", "请求名"),
                        Style::default().fg(crate::ui::theme::G2),
                    )),
                ]),
                Rect::new(
                    table_area.x,
                    table_area.y,
                    table_area.width,
                    table_area.height.min(3),
                ),
            );
            let visible_rows = table_area.height.saturating_sub(3) as usize;
            let shown = models.len().min(visible_rows);
            let has_more = models.len() > shown;
            for (index, row) in models.iter().take(shown).enumerate() {
                let y = table_area.y + 3 + index as u16;
                let focused = view.focus_zone == Some(3) && view.focus_index == index;
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            if focused { "▸ " } else { "   " },
                            Style::default().fg(crate::ui::theme::ACCENT),
                        ),
                        Span::styled(
                            format!("{}  ", index + 1),
                            Style::default().fg(if focused {
                                crate::ui::theme::INK
                            } else {
                                crate::ui::theme::G2
                            }),
                        ),
                        Span::styled(&row.display_name, Style::default().fg(crate::ui::theme::INK)),
                        Span::styled("  →  ", Style::default().fg(crate::ui::theme::G2)),
                        Span::styled(&row.request_name, Style::default().fg(crate::ui::theme::G1)),
                    ])),
                    Rect::new(table_area.x, y, table_area.width, 1),
                );
            }
            if has_more && shown > 0 {
                // 提示行钳制在表格最后一行内（shown==0 时表格无可见行，
                // 渲染提示只会覆盖表头，直接不显示）。
                let hint_y = (table_area.y + 3 + shown as u16)
                    .min(table_area.y + table_area.height.saturating_sub(1));
                f.render_widget(
                    Paragraph::new(Span::styled(
                        format!("… 共 {} 个模型", models.len()),
                        Style::default().fg(crate::ui::theme::G2),
                    )),
                    Rect::new(table_area.x, hint_y, table_area.width, 1),
                );
            }
        }

        f.render_widget(Clear, chunks[7]);
        f.render_widget(Clear, chunks[8]);
        let apply_row = chunks[8];
        for (index, (glyph, label, action)) in [
            (Some(Glyph::Edit), "编辑", ProviderDetailAction::Edit),
            (Some(Glyph::Verify), "调试", ProviderDetailAction::Test),
            (Some(Glyph::Import), "模型", ProviderDetailAction::Models),
            (
                Some(Glyph::Route),
                "路由多选",
                ProviderDetailAction::SelectMode(ProviderMode::Multi),
            ),
            (
                Some(Glyph::Ok),
                "生效",
                ProviderDetailAction::Apply(tab),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            // 动作条紧凑化：命中矩形不动，只画单行（与工具条同语言）。
            let chip_rect = action_chip_rects(chunks[7], 5)[index];
            f.render_widget(
                PillChip {
                    glyph,
                    label,
                    active: matches!(
                        action,
                        ProviderDetailAction::SelectMode(m) if m == mode
                    ),
                    danger: false,
                    focused: view.focus_zone == Some(2) && view.focus_index == index,
                    color: None,
                },
                Rect::new(chip_rect.x, chip_rect.y, chip_rect.width, 1),
            );
        }
        // 生效说明（当前 Tab 目标）。
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("生效 = 应用到 {}（diff 预览 + 备份）", tab.label()),
                Style::default().fg(crate::ui::theme::G2),
            ))),
            Rect::new(apply_row.x, apply_row.y, apply_row.width, 1),
        );
        let mode_hint = if mode == ProviderMode::Multi {
            if runtime_ok {
                "多选模式：spec 运行时运行中 ✓ · 再点「多选」进入选择页".to_string()
            } else {
                "多选模式：需要 spec serve 运行（当前未运行）· g 进入选择页".to_string()
            }
        } else {
            "单选模式：直连配置（claude=settings 直连 / codex=单 provider / opencode=单条目）"
                .to_string()
        };
        let tab_keys = match tab {
            AgentTarget::ClaudeCode => "Tab 切端 · ↑↓ 区域 · ←→ 元素 · Enter 激活 · p/h/r/y 快捷键 · 开关需重启 agent",
            AgentTarget::Codex => "Tab 切端 · ↑↓ 区域 · ←→ 元素 · Enter 激活 · r/y 快捷键 · 开关需重启 agent",
            AgentTarget::OpenCode => "Tab 切端 · ↑↓ 区域 · ←→ 元素 · Enter 激活 · p/r/y 快捷键 · 开关需重启 agent",
            AgentTarget::Other => "",
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                if view.feedback.is_empty() {
                    format!(
                        "{} · {mode_hint} · {tab_keys} · g 进入路由多选 · 生效=应用到{} · m 获取模型 · Esc 返回",
                        mode.label(),
                        tab.label()
                    )
                } else {
                    format!("✓ {}", view.feedback)
                },
                Style::default().fg(if view.feedback.is_empty() {
                    crate::ui::theme::G2
                } else {
                    crate::ui::theme::OK
                }),
            )]))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(crate::ui::theme::G2)),
            ),
            chunks[9],
        );
        crate::ui::widgets::dock::render(f, crate::ui::widgets::dock::DockCtx::Detail);
    })?;
    Ok(())
}

/// 详情页（v2 三端 Tab 版）鼠标命中：Tab 条 / 拉取 overlay / 输入框 / ▸ 槽位 /
/// 高级输入框 / 协议 chips / 开关 chips / 操作行 chips / 应用行 chips + bypass。
/// 渲染与命中测试共用此布局。
pub fn provider_detail_v2_mouse_action(
    area: Rect,
    _models_len: usize,
    fetch: Option<(usize, usize)>,
    mode: ProviderMode,
    agent_tab: AgentTarget,
    pick: DetailPickMode,
    m: &MouseEvent,
) -> Option<ProviderDetailAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let chunks = detail_page_layout(area);
    let col = m.column;
    let row = m.row;

    // Tab 条优先（所有模式可切换端）。
    for (target, rect) in detail_tab_rects(area) {
        if point_in(rect, col, row) {
            return Some(ProviderDetailAction::SwitchTab(target));
        }
    }

    // 拉取 overlay（覆盖协议区到模型表格）：保存 / 行点击。
    if let Some((len, sel)) = fetch {
        let overlay = detail_overlay_rect(area);
        if row >= overlay.y
            && row < overlay.y + overlay.height
            && col >= overlay.x
            && col < overlay.x + overlay.width
        {
            if pick == DetailPickMode::MultiSel {
                let save = Rect::new(
                    overlay.x + overlay.width.saturating_sub(12),
                    overlay.y + overlay.height.saturating_sub(3),
                    12,
                    3,
                );
                if point_in(save, col, row) {
                    return Some(ProviderDetailAction::SaveModels);
                }
            }
            let row_area_bottom = if pick == DetailPickMode::MultiSel {
                4
            } else {
                1
            };
            let visible = usize::from(overlay.height.saturating_sub(row_area_bottom));
            let start = sel.saturating_sub(visible.saturating_sub(1));
            for offset in 0..visible {
                let index = start + offset;
                if index >= len {
                    break;
                }
                let y = overlay.y + 1 + offset as u16;
                if row == y {
                    return Some(match pick {
                        DetailPickMode::Slot(_) => ProviderDetailAction::PickSlotModel(index),
                        DetailPickMode::MultiSel => ProviderDetailAction::ToggleModelSel(index),
                        DetailPickMode::None => return None,
                    });
                }
            }
            return None;
        }
    }

    // 公共 + Claude 槽位输入框（槽位输入框仅 Claude Tab 参与命中）。
    for (field, rect) in detail_input_rects(area) {
        if agent_tab != AgentTarget::ClaudeCode && field >= 4 {
            continue;
        }
        if point_in(rect, col, row) {
            return Some(ProviderDetailAction::DetailEdit(field));
        }
    }
    // Claude 槽位 ▸ 展开按钮。
    if agent_tab == AgentTarget::ClaudeCode {
        for (slot, rect) in detail_slot_pick_rects(area) {
            if point_in(rect, col, row) {
                return Some(ProviderDetailAction::PickSlot(slot));
            }
        }
    }
    // 高级输入框（timeout_ms / max_retries）。
    if agent_tab != AgentTarget::ClaudeCode {
        for (field, rect) in detail_advanced_rects(area) {
            if point_in(rect, col, row) {
                return Some(ProviderDetailAction::DetailEdit(field));
            }
        }
    }
    // 协议 chips。
    let protocol_rects = protocol_chip_rects(area);
    for (index, kind) in protocol_chips_for(agent_tab).iter().enumerate() {
        if point_in(protocol_rects[index], col, row) {
            return Some(ProviderDetailAction::SetProtocol(*kind));
        }
    }
    // 开关行（每端按行分布，整行可点）。
    let toggles: Vec<(Rect, ProviderDetailAction)> = {
        let rects = toggle_row_rects(area);
        match agent_tab {
            AgentTarget::ClaudeCode => rects
                .into_iter()
                .zip([
                    ProviderDetailAction::TogglePermission,
                    ProviderDetailAction::ToggleTerminalTitle,
                    ProviderDetailAction::ToggleRoute,
                ])
                .collect(),
            AgentTarget::Codex => rects
                .into_iter()
                .take(1)
                .zip([ProviderDetailAction::ToggleRoute])
                .collect(),
            AgentTarget::OpenCode => rects
                .into_iter()
                .take(2)
                .zip([
                    ProviderDetailAction::TogglePermission,
                    ProviderDetailAction::ToggleRoute,
                ])
                .collect(),
            AgentTarget::Other => Vec::new(),
        }
    };
    for (rect, action) in toggles {
        if point_in(rect, col, row) {
            return Some(action);
        }
    }
    for (index, action) in [
        ProviderDetailAction::Edit,
        ProviderDetailAction::Test,
        ProviderDetailAction::Models,
        ProviderDetailAction::SelectMode(ProviderMode::Multi),
        ProviderDetailAction::Apply(agent_tab),
    ]
    .into_iter()
    .enumerate()
    {
        if point_in(action_chip_rects(chunks[7], 5)[index], col, row) {
            if matches!(
                action,
                ProviderDetailAction::SelectMode(ProviderMode::Multi)
            ) && mode == ProviderMode::Multi
            {
                return Some(ProviderDetailAction::EnterMulti);
            }
            return Some(action);
        }
    }
    // 模型表格区不再响应行点击（改名入口收敛到「编辑」→「模型」）：
    // 该区点击无动作，直接落空。
    None
}

// ─── 自旧 menu 迁入的详情页共享助手（FR-M6b 重建）───

/// 该 provider 当前在各端生效情况（csv 命中即计入），BTreeMap 序输出。
pub fn active_targets(current: &BTreeMap<String, String>, provider_id: &str) -> String {
    current
        .iter()
        .filter_map(|(target, csv)| {
            csv.split(',')
                .any(|c| c == provider_id)
                .then_some(target.as_str())
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 动作条 chip 矩形：与工具条同源几何。
pub fn action_chip_rects(section: Rect, count: usize) -> Vec<Rect> {
    toolbar_rects(section.x, section.y + 1, section.width, count)
}

pub fn cache_mode_is_deepseek(provider: &ProviderProfile) -> bool {
    matches!(provider.cache_mode, trivium::domain::CacheMode::DeepSeek)
        || (matches!(provider.cache_mode, trivium::domain::CacheMode::Auto)
            && matches!(provider.vendor, trivium::domain::ProviderVendor::DeepSeek))
}

pub fn cache_mode_label(provider: &ProviderProfile) -> String {
    match provider.cache_mode {
        trivium::domain::CacheMode::Compat => "Compat".to_string(),
        trivium::domain::CacheMode::DeepSeek => "DeepSeek".to_string(),
        trivium::domain::CacheMode::Auto => {
            if cache_mode_is_deepseek(provider) {
                "DeepSeek（自动）".into()
            } else {
                "自动".into()
            }
        }
    }
}

/// 开关行：三行、每行高 1、整行可点（渲染/命中共用）。
pub fn toggle_row_rects(area: Rect) -> Vec<Rect> {
    let chunks = detail_page_layout(area);
    let band = chunks[5];
    (0..3u16)
        .map(|i| Rect::new(band.x, band.y + i, band.width, 1))
        .collect()
}

pub fn protocol_chips_for(tab: AgentTarget) -> Vec<ProtocolKind> {
    match tab {
        AgentTarget::ClaudeCode => vec![
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
        ],
        AgentTarget::Codex | AgentTarget::OpenCode => {
            vec![ProtocolKind::OpenAiChat, ProtocolKind::OpenAiResponses]
        }
        AgentTarget::Other => Vec::new(),
    }
}

pub fn protocol_chip_label(kind: ProtocolKind, _tab: AgentTarget) -> &'static str {
    match kind {
        ProtocolKind::OpenAiChat => "Chat",
        ProtocolKind::OpenAiResponses => "Responses",
        ProtocolKind::AnthropicMessages => "Anthropic",
    }
}

pub fn protocol_chip_rects(area: Rect) -> Vec<Rect> {
    let chunks = detail_page_layout(area);
    let band = chunks[3];
    let y = band.y + 1; // 标题行之下
    (0..3u16)
        .map(|i| {
            let w = band.width / 3;
            Rect::new(band.x + i * w, y, w.min(band.width - i * w), 1)
        })
        .collect()
}

/// 密钥打码：按字符截断；短密钥全星号。
pub fn masked_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len().min(4));
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

pub fn compact_health(health: &ProviderHealth) -> String {
    if health.ok {
        let latency = health
            .latency_ms
            .map(|ms| format!(" · {ms} ms"))
            .unwrap_or_default();
        format!("健康{}", latency)
    } else {
        format!("失败 · {}", health.message)
    }
}

pub fn protocol_label(protocol: ProtocolKind) -> &'static str {
    protocol_chip_label(protocol, AgentTarget::ClaudeCode)
}

/// 端点摘要：去 scheme，保留 host+path。
pub fn compact_url(url: &str) -> String {
    match url.split_once("://") {
        Some((_, rest)) => rest.to_string(),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn detail_uses_theme_tokens_exclusively() {
        // 与 editor 同规：渲染正文禁止裸写 Color（青色泛滥的根因）。
        let src = include_str!("detail.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !body.contains("Color"),
            "detail must not reference ratatui Color directly; use ui::theme tokens"
        );
    }
}
