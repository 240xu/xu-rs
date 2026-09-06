//! Providers 列表页（v3 前端层 · 自 menu/mod.rs 物理迁入）
//! 渲染与命中同文件同源；设计令牌逐步切至 ui::theme。

use std::collections::BTreeMap;
use std::io;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::Tui;
use crate::ui::screens::chrome::list_window_rects;
use crate::ui::screens::chrome::right_align_line;
use crate::ui::screens::detail::active_targets;
use crate::ui::screens::detail::compact_url;
use crate::ui::screens::detail::protocol_label;
use crate::ui::theme;
use crate::ui::theme::{ACCENT, BAD, G1, G2, IDENTITY, INK, OK, SEL_BG, WARN};
use crate::ui::widgets::chips::{
    icon_button_rect, point_in, toolbar_rects, toolbar_rows, IconButton, ICON_BUTTON_WIDTH,
    TOOL_CHIP_GAP, TOOL_CHIP_HEIGHT,
};
use crate::ui::widgets::searchbox::render_search_box;
use crate::ui::widgets::searchbox::SearchBox;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use trivium::domain::{AgentTarget, ProtocolKind, ProviderProfile};
use trivium::state::ProviderHealth;

/// 供应商卡片高度（2 行内容 + 1 行呼吸空隙，无边框）。
pub const PROVIDER_CARD_HEIGHT: u16 = 3;

/// 按 SearchBox 查询过滤供应商：名称 / id / 协议 / 端点任一命中即保留。
/// 返回的是在 `providers` 中的真实下标列表（列表页选中项始终是真实下标，
/// 过滤只影响可见窗口，不影响详情/应用等既有流程）。
pub fn filtered_provider_indices(providers: &[ProviderProfile], search: &SearchBox) -> Vec<usize> {
    providers
        .iter()
        .enumerate()
        .filter_map(|(index, provider)| {
            let haystack = format!(
                "{} {} {} {}",
                provider.name,
                provider.id,
                protocol_label(provider.protocol),
                provider.base_url
            );
            search.matches(&haystack).then_some(index)
        })
        .collect()
}

pub fn provider_card_area(area: Rect, width: u16, search_h: u16) -> Rect {
    let header = search_h + toolbar_rows(width, 4) as u16 * (TOOL_CHIP_HEIGHT + TOOL_CHIP_GAP);
    Rect::new(
        area.x,
        area.y + header,
        area.width,
        area.height.saturating_sub(header + 3),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_providers_page(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    providers: &[ProviderProfile],
    selected: usize,
    current: &BTreeMap<String, String>,
    health: &BTreeMap<String, ProviderHealth>,
    error: Option<&str>,
    sync_notice: Option<&str>,
    search: &SearchBox,
    press: Option<ProviderPressView>,
) {
    let search_h = search.slot_height();
    let toolbar_height = toolbar_rows(area.width, 4) as u16 * (TOOL_CHIP_HEIGHT + TOOL_CHIP_GAP);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(search_h),
            Constraint::Length(toolbar_height),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);

    render_search_box(f, chunks[0], search, "搜索供应商（名称 / 协议 / 端点）");

    for (index, (icon, color)) in [
        ("+", Color::Green),
        ("tpl", Color::Green),
        ("ref", Color::Cyan),
        ("route", Color::Gray),
    ]
    .into_iter()
    .enumerate()
    {
        f.render_widget(
            IconButton {
                label: icon,
                color,
                active: false,
            },
            toolbar_rects(area.x, area.y + search_h, area.width, 4)[index],
        );
    }
    // 工具条右侧状态摘要（P2：不再留 50 列空白）。
    {
        let healthy = health.values().filter(|h| h.ok).count();
        // state 里可能残留已删供应商的健康条目，按 provider 视角统计防下溢。
        let untested = providers
            .iter()
            .filter(|p| !health.contains_key(&p.id))
            .count();
        let summary = format!(
            "{} providers · {} 健康 · {} 未测试",
            providers.len(),
            healthy,
            untested
        );
        let ty = area.y + search_h + TOOL_CHIP_HEIGHT;
        f.render_widget(
            Paragraph::new(Span::styled(summary, Style::default().fg(Color::DarkGray))),
            Rect::new(area.x, ty, area.width, 1),
        );
    }

    let filtered = filtered_provider_indices(providers, search);
    let card_area = provider_card_area(area, area.width, search_h);

    if let Some(error) = error {
        let panel = Paragraph::new(vec![
            Line::from(Span::styled("读取供应商失败", Style::default().fg(BAD))),
            Line::from(""),
            Line::from(Span::styled(error, Style::default().fg(Color::Gray))),
            Line::from(""),
            Line::from(Span::styled(
                "请检查 ~/.codex/xu-chat-providers.json，按 r 重试。",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        f.render_widget(panel, card_area);
    } else if filtered.is_empty() {
        let panel = Paragraph::new(vec![
            Line::from(Span::styled(
                if providers.is_empty() {
                    "暂无供应商".to_string()
                } else {
                    "没有匹配的供应商".to_string()
                },
                Style::default().fg(Color::Gray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                if providers.is_empty() {
                    "路径：~/.codex/xu-chat-providers.json\n\n按 a 从模板添加 · 按 p 手动新增 · 按 r 重试读取"
                } else {
                    "调整搜索关键字，或按 Esc 清除过滤。"
                },
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Center);
        f.render_widget(panel, card_area);
    } else {
        let selected_position = filtered
            .iter()
            .position(|index| *index == selected)
            .unwrap_or(0);
        // 配置级说明只占一行（不再每张卡重复）。
        let mut list_area = card_area;
        if let Some(notice) = sync_notice {
            f.render_widget(
                Paragraph::new(Span::styled(
                    notice.to_string(),
                    Style::default().fg(Color::DarkGray),
                )),
                Rect::new(card_area.x, card_area.y, card_area.width, 1),
            );
            list_area = Rect::new(
                card_area.x,
                card_area.y + 1,
                card_area.width,
                card_area.height.saturating_sub(1),
            );
        }
        let visible = usize::from((list_area.height / PROVIDER_CARD_HEIGHT).max(1));
        let rects = list_window_rects(
            list_area,
            filtered.len(),
            selected_position,
            PROVIDER_CARD_HEIGHT,
            visible,
        );
        for (position, rect) in rects {
            let provider = &providers[filtered[position]];
            let active = filtered[position] == selected;
            let pressed = Some(filtered[position]) == press.map(|p| p.index);
            let protocol = protocol_label(provider.protocol);
            let active_targets = active_targets(current, &provider.id);
            let endpoint = compact_url(&provider.base_url);

            // 行 A：▍竖条 + 序号 + 名称 + 协议 · 模型数 …… 右：健康徽章
            let health_span = match health.get(&provider.id) {
                Some(h) if h.ok => Span::styled(
                    format!("● {}ms", h.latency_ms.unwrap_or(0)),
                    Style::default().fg(OK),
                ),
                Some(_) => Span::styled("● 超时", Style::default().fg(BAD)),
                None => Span::styled("○ 未测试", Style::default().fg(G2)),
            };
            let bar = if pressed {
                Span::styled("▍", Style::default().fg(WARN))
            } else if active {
                Span::styled("▍", Style::default().fg(ACCENT))
            } else {
                Span::raw(" ")
            };
            let line_a = right_align_line(
                vec![
                    bar,
                    Span::styled(
                        format!(" {} ", position + 1),
                        Style::default().fg(if active { ACCENT } else { G2 }),
                    ),
                    Span::styled(
                        provider.name.as_str(),
                        Style::default()
                            .fg(if active { INK } else { G1 })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("   {protocol} · {} models", provider.models.len()),
                        Style::default().fg(if active { G1 } else { G2 }),
                    ),
                ],
                vec![health_span],
                rect.width,
            );
            // 行 B：端点（剥协议前缀）…… 右：生效端（三端身份色）
            let mut effect_right = vec![Span::styled(
                "生效 » ",
                Style::default().fg(Color::DarkGray),
            )];
            for (i, target) in active_targets
                .split(',')
                .filter(|t| !t.is_empty())
                .enumerate()
            {
                if i > 0 {
                    effect_right.push(Span::styled(", ", Style::default().fg(Color::DarkGray)));
                }
                let color = match target.trim() {
                    t if t.eq_ignore_ascii_case("opencode") => IDENTITY[0],
                    t if t.eq_ignore_ascii_case("claude") => IDENTITY[1],
                    t if t.eq_ignore_ascii_case("codex") => IDENTITY[2],
                    _ => G1,
                };
                effect_right.push(Span::styled(
                    target.trim().to_string(),
                    Style::default().fg(color),
                ));
            }
            if active_targets.is_empty() {
                effect_right.clear();
            }
            let line_b = right_align_line(
                vec![Span::styled(
                    format!("    {endpoint}"),
                    Style::default().fg(if active { G1 } else { G2 }),
                )],
                effect_right,
                rect.width,
            );

            let body = Rect::new(rect.x, rect.y, rect.width, 2);
            let card_style = if active || pressed {
                Style::default().bg(theme::PANEL_BG)
            } else {
                Style::default()
            };
            f.render_widget(Paragraph::new(vec![line_a, line_b]).style(card_style), body);
        }
    }

    let selected_text = if filtered.is_empty() {
        "0/0".to_string()
    } else {
        let position = filtered
            .iter()
            .position(|index| *index == selected)
            .unwrap_or(0);
        format!("{}/{}", position + 1, filtered.len())
    };
    let hint_text = if let Some(p) = press {
        let width = (p.progress as usize * 20 / 100).max(1);
        format!(
            "长按删除 {} {}/{}  {}{}",
            p.index + 1,
            p.index + 1,
            filtered.len(),
            "█".repeat(width),
            "░".repeat(20 - width),
        )
    } else if search.is_empty() {
        "点一次选中 · 再点进入 · 长按删除".to_string()
    } else {
        format!(
            "过滤「{}」{} 项 · Esc 清除 · 点一次选中 · 再点进入",
            search.query,
            filtered.len()
        )
    };
    // 克制派 footer：左提示 · 右计数，两端对齐，不再居中堆一句。
    let line = right_align_line(
        vec![Span::styled(
            hint_text,
            Style::default().fg(Color::DarkGray),
        )],
        vec![Span::styled(
            selected_text,
            Style::default().fg(Color::Cyan),
        )],
        chunks[3].width,
    );
    let hint = Paragraph::new(line)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .alignment(Alignment::Left);
    f.render_widget(hint, chunks[3]);
}

/// 供应商页工具条按钮命中（Up）。与渲染共用 `toolbar_rects`。
pub fn providers_page_toolbar_hit(
    area: Rect,
    search: &SearchBox,
    m: &MouseEvent,
) -> Option<ProviderMouseAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    for (index, action) in [
        ProviderMouseAction::Add,
        ProviderMouseAction::Preset,
        ProviderMouseAction::Refresh,
        ProviderMouseAction::Multi,
    ]
    .into_iter()
    .enumerate()
    {
        if point_in(
            toolbar_rects(area.x, area.y + search.slot_height(), area.width, 4)[index],
            m.column,
            m.row,
        ) {
            return Some(action);
        }
    }
    None
}

/// 供应商卡片命中（过滤列表 → 真实下标）。渲染与命中共用同一窗口计算。
pub fn provider_card_hit(
    area: Rect,
    selected: usize,
    search: &SearchBox,
    filtered: &[usize],
    m: &MouseEvent,
) -> Option<usize> {
    if filtered.is_empty() {
        return None;
    }
    let selected_position = filtered
        .iter()
        .position(|index| *index == selected)
        .unwrap_or(0);
    let card_area = provider_card_area(area, area.width, search.slot_height());
    let visible = usize::from((card_area.height / PROVIDER_CARD_HEIGHT).max(1));
    list_window_rects(
        card_area,
        filtered.len(),
        selected_position,
        PROVIDER_CARD_HEIGHT,
        visible,
    )
    .into_iter()
    .find_map(|(position, rect)| point_in(rect, m.column, m.row).then_some(filtered[position]))
}

/// 供应商页鼠标命中（Up）：工具条按钮 / 过滤后的卡片行。
/// 搜索框聚焦由 `search_box_click` 单独处理（Down 也触发，方便触摸）。
pub fn providers_page_mouse_action(
    area: Rect,
    selected: usize,
    search: &SearchBox,
    providers: &[ProviderProfile],
    m: &MouseEvent,
) -> Option<ProviderMouseAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    if let Some(action) = providers_page_toolbar_hit(area, search, m) {
        return Some(action);
    }
    let filtered = filtered_provider_indices(providers, search);
    provider_card_hit(area, selected, search, &filtered, m).map(ProviderMouseAction::Detail)
}

/// 供应商页长按起始命中（Down/Drag），返回真实下标；用于长按删除进度。
pub fn providers_page_row_at(
    area: Rect,
    selected: usize,
    search: &SearchBox,
    providers: &[ProviderProfile],
    m: &MouseEvent,
) -> Option<usize> {
    let filtered = filtered_provider_indices(providers, search);
    provider_card_hit(area, selected, search, &filtered, m)
}

// ─── 自 menu 迁入：动作枚举 + 多选页 ───

pub enum ProviderMouseAction {
    Add,
    Preset,
    Refresh,
    Multi,
    Detail(usize),
}

/// 供应商选择模式：单选 = 直连配置（claude=settings 直连、codex=单 provider、
/// opencode=单条目）；多选 = runtime slug + active providers 列表（需 spec serve）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderMode {
    Single,
    Multi,
}

impl ProviderMode {
    pub fn label(self) -> &'static str {
        match self {
            ProviderMode::Single => "单选",
            ProviderMode::Multi => "路由多选",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderDetailAction {
    Edit,
    Test,
    Apply(AgentTarget),
    /// 获取模型（后台拉取上游 /models）。
    Models,
    /// 单·多选择模式切换。
    SelectMode(ProviderMode),
    /// 多选模式 → 进入多供应商选择页。
    EnterMulti,
    /// 点击详情页圆角输入框 → 编辑该字段
    /// （0端点 / 1密钥 / 2默认模型 / 3备注 / 4..7 Claude 槽位 / 8超时 / 9重试）。
    DetailEdit(usize),
    /// 三端 Tab 切换（Claude Code / Codex / OpenCode）。
    SwitchTab(AgentTarget),
    /// 协议 chips：保存 provider update --kind。
    SetProtocol(ProtocolKind),
    /// Claude 槽位行右侧「▸」→ 展开上游模型列表。
    PickSlot(usize),
    /// 槽位展开列表点击 → 填入该槽位。
    PickSlotModel(usize),
    /// 完全权限开关（Claude=defaultMode / OpenCode=permission，按当前端）。
    TogglePermission,
    /// Claude 会话标题关闭开关（env CLAUDE_CODE_DISABLE_TERMINAL_TITLE）。
    ToggleTerminalTitle,
    /// 三端路由开关（xu-client-routes.json）。
    ToggleRoute,
    /// 获取模型多选：勾选/取消一个模型。
    ToggleModelSel(usize),
    /// 获取模型多选：保存写 models。
    SaveModels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultiProviderAction {
    Cancel,
    Target(AgentTarget),
    Toggle(usize),
    MoveUp,
    MoveDown,
    Preview,
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderPressView {
    pub index: usize,
    pub progress: u8,
}

pub fn chunks_footer_up_rect(area: Rect) -> Rect {
    Rect::new(area.x, area.y + area.height.saturating_sub(6), 5, 5)
}

pub fn render_multi_provider(
    terminal: &mut Tui,
    providers: &[ProviderProfile],
    selected_ids: &[String],
    cursor: usize,
    target: AgentTarget,
) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7),
                Constraint::Min(1),
                Constraint::Length(6),
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(vec![
                Line::from("                         多供应商路由"),
                Line::from(Span::styled(
                    "点选供应商；第一项为默认。需要 spec 运行时，不是自动故障转移。",
                    Style::default().fg(Color::Gray),
                )),
            ])
            .wrap(Wrap { trim: true }),
            chunks[0],
        );
        for (index, (icon, color, active)) in [
            ("back", Color::Red, false),
            ("cl", Color::Cyan, target == AgentTarget::ClaudeCode),
            ("cdx", Color::Cyan, target == AgentTarget::Codex),
            ("ok", Color::Green, false),
        ]
        .into_iter()
        .enumerate()
        {
            f.render_widget(
                IconButton {
                    label: icon,
                    color,
                    active,
                },
                Rect::new(
                    icon_button_rect(area.x, area.y, index).x,
                    icon_button_rect(area.x, area.y, index).y,
                    ICON_BUTTON_WIDTH,
                    1,
                ),
            );
        }
        let rows = list_window_rects(chunks[1], providers.len(), cursor, 4, 8);
        for (index, rect) in rows {
            let provider = &providers[index];
            let order = selected_ids.iter().position(|id| id == &provider.id);
            let active = index == cursor;
            let border = if active { Color::Cyan } else { Color::DarkGray };
            let mark = order
                .map(|position| format!("[{}]", position + 1))
                .unwrap_or_else(|| "[ ]".to_string());
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {mark} "),
                            Style::default().fg(Color::Black).bg(if order.is_some() {
                                Color::Green
                            } else {
                                Color::DarkGray
                            }),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            &provider.name,
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format!("  {}", protocol_label(provider.protocol))),
                    ]),
                    Line::from(Span::styled(
                        format!("{} · 默认 {}", provider.id, provider.default_model),
                        Style::default().fg(Color::Gray),
                    )),
                ])
                .style(if border == Color::Cyan || border == Color::Yellow {
                    Style::default().bg(SEL_BG)
                } else {
                    Style::default()
                }),
                rect,
            );
        }
        f.render_widget(
            Paragraph::new("点选供应商勾选，至少选择两个 · Esc 返回")
                .block(Block::default().borders(Borders::TOP)),
            chunks[2],
        );
        f.render_widget(
            IconButton {
                label: "up",
                color: Color::Cyan,
                active: false,
            },
            Rect::new(chunks[2].x, chunks[2].y, 5, 5),
        );
        f.render_widget(
            IconButton {
                label: "dn",
                color: Color::Cyan,
                active: false,
            },
            Rect::new(chunks[2].x + 6, chunks[2].y, 5, 5),
        );
    })?;
    Ok(())
}

pub fn multi_provider_mouse_action(
    area: Rect,
    providers_len: usize,
    cursor: usize,
    m: &MouseEvent,
) -> Option<MultiProviderAction> {
    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
        return None;
    }
    let row = m.row;
    let col = m.column;
    for (index, action) in [
        MultiProviderAction::Cancel,
        MultiProviderAction::Target(AgentTarget::ClaudeCode),
        MultiProviderAction::Target(AgentTarget::Codex),
        MultiProviderAction::Preview,
    ]
    .into_iter()
    .enumerate()
    {
        if point_in(icon_button_rect(area.x, area.y, index), col, row) {
            return Some(action);
        }
    }
    if point_in(
        Rect::new(
            chunks_footer_up_rect(area).x,
            chunks_footer_up_rect(area).y,
            5,
            5,
        ),
        col,
        row,
    ) {
        return Some(MultiProviderAction::MoveUp);
    }
    if point_in(
        Rect::new(
            chunks_footer_up_rect(area).x + 6,
            chunks_footer_up_rect(area).y,
            5,
            5,
        ),
        col,
        row,
    ) {
        return Some(MultiProviderAction::MoveDown);
    }
    let header = 7u16;
    let content = Rect::new(
        area.x,
        area.y + header,
        area.width,
        area.height.saturating_sub(header + 6),
    );
    list_window_rects(content, providers_len, cursor, 4, 8)
        .into_iter()
        .find_map(|(index, rect)| {
            point_in(rect, col, row).then_some(MultiProviderAction::Toggle(index))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::screens::chrome::TAB_BAR_HEIGHT;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn tap(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn provider(id: &str, name: &str, protocol: ProtocolKind) -> ProviderProfile {
        ProviderProfile {
            id: id.to_string(),
            name: name.to_string(),
            notes: None,
            website: None,
            vendor: trivium::domain::ProviderVendor::OpenAi,
            protocol,
            base_url: "https://api.example.com/v1".to_string(),
            api_key: format!("sk-{id}"),
            models: vec![],
            model_entries: Default::default(),
            model_metadata: Default::default(),
            claude_slots: Default::default(),
            default_model: String::new(),
            extra_headers: Default::default(),
            request_url_mode: None,
            header_mode: None,
            timeout_ms: 0,
            max_retries: 0,
            context_window: 0,
            max_output_tokens: 0,
            reasoning_effort: None,
            cache_mode: Default::default(),
        }
    }

    #[allow(dead_code)]
    fn search_with(query: &str) -> SearchBox {
        let mut s = SearchBox::new();
        s.query = query.to_string();
        s
    }

    #[test]
    fn provider_search_filters_by_name_protocol_and_endpoint() {
        let providers = vec![
            provider("deepseek", "DeepSeek", ProtocolKind::OpenAiChat),
            provider("zen", "Z.ai GLM", ProtocolKind::AnthropicMessages),
        ];
        let none = SearchBox::new();
        assert_eq!(filtered_provider_indices(&providers, &none).len(), 2);
        let mut by_name = SearchBox::new();
        by_name.query = "deep".to_string();
        assert_eq!(filtered_provider_indices(&providers, &by_name), vec![0]);
        let mut by_proto = SearchBox::new();
        by_proto.query = "anthropic".to_string();
        assert_eq!(filtered_provider_indices(&providers, &by_proto), vec![1]);
        let mut by_url = SearchBox::new();
        by_url.query = "example.com".to_string();
        assert_eq!(filtered_provider_indices(&providers, &by_url).len(), 2);
        let mut miss = SearchBox::new();
        miss.query = "不存在的词".to_string();
        assert!(filtered_provider_indices(&providers, &miss).is_empty());
    }

    #[test]
    fn provider_card_hit_resolves_filtered_rows_on_down_not_just_up() {
        let providers = vec![provider("a", "A", ProtocolKind::OpenAiChat)];
        let search = SearchBox::new();
        let area = Rect::new(0, TAB_BAR_HEIGHT, 80, 24);
        // Down（长按起点）也应命中卡片行，供选中态使用
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 6,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(
            provider_card_hit(
                area,
                0,
                &search,
                &filtered_provider_indices(&providers, &search),
                &down
            ),
            Some(0)
        );
        assert_eq!(
            provider_card_hit(
                area,
                0,
                &search,
                &filtered_provider_indices(&providers, &search),
                &tap(10, 6)
            ),
            Some(0)
        );
        // 工具条区域不算卡片
        assert_eq!(
            provider_card_hit(
                area,
                0,
                &search,
                &filtered_provider_indices(&providers, &search),
                &tap(4, TAB_BAR_HEIGHT + 1)
            ),
            None
        );
    }

    #[test]
    fn provider_card_hit_collapsed_search_shifts_cards_up() {
        let providers = vec![
            provider("a", "A", ProtocolKind::OpenAiChat),
            provider("b", "B", ProtocolKind::OpenAiChat),
        ];
        let area = Rect::new(0, TAB_BAR_HEIGHT, 80, 24);
        // 折叠搜索框（空且未聚焦）后，首卡上移：折叠头高 < 展开头高
        let collapsed = SearchBox::new();
        let mut expanded = SearchBox::new();
        expanded.focused = true;
        let h_collapsed = provider_card_area(area, area.width, collapsed.slot_height()).y;
        let h_expanded = provider_card_area(area, area.width, expanded.slot_height()).y;
        assert!(h_collapsed <= h_expanded);
        // 折叠态下点击更靠上的行仍能命中首卡
        let filtered = filtered_provider_indices(&providers, &collapsed);
        assert_eq!(
            provider_card_hit(area, 0, &collapsed, &filtered, &tap(10, h_collapsed)),
            Some(0)
        );
    }

    #[test]
    fn providers_page_toolbar_hit_routes_actions() {
        let search = SearchBox::new();
        let area = Rect::new(0, TAB_BAR_HEIGHT, 80, 24);
        // 工具条在搜索框之下；逐个 chip 命中（几何来自 toolbar_rects 同源）
        let rects = crate::ui::widgets::chips::toolbar_rects(
            area.x,
            area.y + search.slot_height(),
            area.width,
            4,
        );
        assert_eq!(rects.len(), 4);
        assert!(matches!(
            providers_page_toolbar_hit(area, &search, &tap(rects[0].x + 1, rects[0].y)),
            Some(ProviderMouseAction::Add)
        ));
        assert!(matches!(
            providers_page_toolbar_hit(area, &search, &tap(rects[3].x + 1, rects[3].y)),
            Some(ProviderMouseAction::Multi)
        ));
    }
}
