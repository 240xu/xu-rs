use std::io::BufRead;

use crate::tui::Tui;
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use rusqlite::params;
use unicode_width::UnicodeWidthStr;

const SESSION_VISIBLE: usize = 18;
const PREVIEW_MAX_RECORDS: usize = 2_000;
const PREVIEW_MAX_MESSAGES: usize = 160;
const PREVIEW_MAX_CHARS: usize = 48_000;
const PREVIEW_PAGE_LINES: u16 = 10;
const LEFT_WIDTH: usize = 38;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceFilter {
    All,
    Codex,
    Claude,
    OpenCode,
}

impl SourceFilter {
    fn matches(self, source: &str) -> bool {
        match self {
            SourceFilter::All => true,
            SourceFilter::Codex => source == "codex",
            SourceFilter::Claude => source == "claude",
            SourceFilter::OpenCode => source == "opencode",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SourceFilter::All => "全部",
            SourceFilter::Codex => "Codex",
            SourceFilter::Claude => "Claude",
            SourceFilter::OpenCode => "OpenCode",
        }
    }
}

struct SessionRenderState<'a> {
    total: usize,
    selected: usize,
    status: &'a str,
    filter: SourceFilter,
    query: &'a str,
    grouped: bool,
    search_mode: bool,
    preview_scroll: u16,
    select_mode: bool,
    confirm_delete: bool,
    multi_selected: &'a std::collections::HashSet<usize>,
}

#[derive(Clone, Debug)]
enum DeleteRequest {
    Single(usize),
    Multi(Vec<usize>),
    PurgeSource(SourceFilter),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConversationMessage {
    role: String,
    text: String,
}

/// 会话页退出结果：恢复会话 / 点击顶层 Tab 切换 / 返回。
pub enum SessionExit {
    /// 全局 q：直接退出应用。
    Quit,
    Resume(crate::config::SessionInfo),
    SwitchTab(crate::ui::screens::chrome::Tab),
    None,
}

pub fn run(terminal: &mut Tui) -> SessionExit {
    let mut selected: usize = 0;
    // 列表加载异步化：list_sessions 扫三源（含大 SQLite）冷查询可达十几秒，
    // 同步阻塞会让「按 5 进会话页」像死机。先画加载占位，后台线程回填。
    let (sessions_tx, sessions_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let all = list_sessions();
        let _ = sessions_tx.send(all);
    });
    let mut all_sessions: Vec<crate::config::SessionInfo> = Vec::new();
    let mut sessions_loading = true;
    let mut filter = SourceFilter::All;
    let mut query = String::new();
    let mut grouped = false;
    let mut search_mode = false;
    let mut sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
    let mut status = String::new();
    let mut preview_cache: Option<(usize, String)> = None;
    let mut preview_scroll: u16 = 0;
    let mut confirm_delete: Option<DeleteRequest> = None;
    let mut multi_selected: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut select_mode = false;

    loop {
        // 后台列表回填；回填当帧强制渲染（Tick 短路会跳过普通 Tick）。
        let mut force_draw = false;
        if sessions_loading {
            match sessions_rx.try_recv() {
                Ok(all) => {
                    all_sessions = all;
                    sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                    selected = selected.min(sessions.len().saturating_sub(1));
                    preview_cache = None;
                    sessions_loading = false;
                    force_draw = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // 发送端死亡（理论不可达）：兜底放行，别把用户锁在加载页。
                    sessions_loading = false;
                    force_draw = true;
                }
                Err(_) => {}
            }
        }
        let ev = match crate::tui::poll_event(std::time::Duration::from_millis(16)) {
            Some(ev) => ev,
            None => continue,
        };
        // 加载中：仅响应退出键，画占位帧。
        if sessions_loading {
            if let crate::tui::AppEvent::Key(k) = &ev {
                if matches!(
                    k.code,
                    crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Char('q')
                ) {
                    return SessionExit::None;
                }
            }
            let _ = terminal.draw(|f| {
                use ratatui::style::{Color, Style};
                use ratatui::widgets::Paragraph;
                let area = f.area();
                f.render_widget(
                    Paragraph::new(Span::styled(
                        "正在加载会话…（Esc 返回）",
                        Style::default().fg(Color::DarkGray),
                    ))
                    .alignment(ratatui::layout::Alignment::Center),
                    area,
                );
            });
            continue;
        }

        let terminal_size = terminal.size().ok();
        if preview_cache
            .as_ref()
            .map(|(idx, _)| *idx != selected)
            .unwrap_or(true)
        {
            let preview = sessions
                .get(selected)
                .map(session_preview)
                .unwrap_or_default();
            preview_cache = Some((selected, preview));
        }
        let preview = preview_cache
            .as_ref()
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        let preview_width = terminal_size
            .map(|size| size.width.saturating_sub(LEFT_WIDTH as u16 + 4).max(12) as usize)
            .unwrap_or(60);
        let preview_height = terminal_size
            .map(|size| size.height.saturating_sub(16).max(4) as usize)
            .unwrap_or(PREVIEW_PAGE_LINES as usize);
        let max_scroll = visual_line_count(preview, preview_width)
            .saturating_sub(preview_height)
            .min(u16::MAX as usize) as u16;
        preview_scroll = preview_scroll.min(max_scroll);
        'handle: {
            match ev {
                crate::tui::AppEvent::Key(k) => match k.code {
                    crossterm::event::KeyCode::Esc if search_mode => {
                        search_mode = false;
                        status.clear();
                    }
                    crossterm::event::KeyCode::Esc if confirm_delete.is_some() => {
                        confirm_delete = None;
                        status = "取消删除".to_string();
                    }
                    crossterm::event::KeyCode::Esc if select_mode => {
                        select_mode = false;
                        multi_selected.clear();
                        status.clear();
                    }
                    crossterm::event::KeyCode::Esc => return SessionExit::None,
                    // 全局 q 退出（E1）：会话子页同样直达；搜索态下 q 是字符。
                    crossterm::event::KeyCode::Char('q') if !search_mode => {
                        return SessionExit::Quit
                    }
                    crossterm::event::KeyCode::Enter if search_mode => {
                        search_mode = false;
                        status.clear();
                    }
                    // 管理模式/确认态禁止 Enter 恢复会话
                    crossterm::event::KeyCode::Enter
                        if !sessions.is_empty()
                            && !select_mode
                            && confirm_delete.is_none()
                            && !search_mode =>
                    {
                        return SessionExit::Resume(sessions[selected].clone());
                    }
                    crossterm::event::KeyCode::Up => {
                        let next = selected.saturating_sub(1);
                        if next != selected {
                            selected = next;
                            preview_scroll = 0;
                        }
                    }
                    crossterm::event::KeyCode::Down
                        if selected < sessions.len().saturating_sub(1) =>
                    {
                        selected += 1;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::PageUp => {
                        preview_scroll = preview_scroll.saturating_sub(PREVIEW_PAGE_LINES);
                    }
                    crossterm::event::KeyCode::PageDown => {
                        preview_scroll = preview_scroll.saturating_add(PREVIEW_PAGE_LINES);
                    }
                    crossterm::event::KeyCode::Home => preview_scroll = 0,
                    crossterm::event::KeyCode::End => {
                        preview_scroll = max_scroll;
                    }
                    crossterm::event::KeyCode::Char('/') => {
                        search_mode = true;
                        status = "搜索模式：输入关键词，Enter 结束，Esc 取消搜索输入。".to_string();
                    }
                    crossterm::event::KeyCode::Backspace if search_mode => {
                        query.pop();
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = selected.min(sessions.len().saturating_sub(1));
                        preview_cache = None;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::Char(c) if search_mode => {
                        query.push(c);
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = selected.min(sessions.len().saturating_sub(1));
                        preview_cache = None;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::Char('1') => {
                        filter = SourceFilter::All;
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = 0;
                        preview_cache = None;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::Char('2') => {
                        filter = SourceFilter::Codex;
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = 0;
                        preview_cache = None;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::Char('3') => {
                        filter = SourceFilter::Claude;
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = 0;
                        preview_cache = None;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::Char('4') => {
                        filter = SourceFilter::OpenCode;
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = 0;
                        preview_cache = None;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::Char('g') => {
                        grouped = !grouped;
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = selected.min(sessions.len().saturating_sub(1));
                        preview_cache = None;
                        preview_scroll = 0;
                    }
                    crossterm::event::KeyCode::Char('c') => {
                        query.clear();
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = 0;
                        preview_cache = None;
                        preview_scroll = 0;
                        status.clear();
                    }
                    // ─── 删除操作 ───
                    crossterm::event::KeyCode::Char('d')
                        if !search_mode && !sessions.is_empty() && confirm_delete.is_none() =>
                    {
                        if select_mode && !multi_selected.is_empty() {
                            let indices: Vec<usize> = multi_selected.iter().copied().collect();
                            let count = indices.len();
                            confirm_delete = Some(DeleteRequest::Multi(indices));
                            status = format!("确认删除 {count} 条对话？点「确认删除」或按 y");
                        } else {
                            confirm_delete = Some(DeleteRequest::Single(selected));
                            let title = sessions[selected].title.clone();
                            let short = compact_title(&title, 24);
                            status = format!("确认删除「{short}」？点「确认删除」或按 y");
                        }
                    }
                    crossterm::event::KeyCode::Char('D')
                        if !search_mode && confirm_delete.is_none() =>
                    {
                        confirm_delete = Some(DeleteRequest::PurgeSource(filter));
                        let label = filter.label();
                        let count = sessions.len();
                        status =
                            format!("确认删除 {label} 的 {count} 条对话？点「确认删除」或按 y");
                    }
                    crossterm::event::KeyCode::Char('y') if confirm_delete.is_some() => {
                        let Some(req) = confirm_delete.take() else {
                            unreachable!("guard guarantees confirm_delete");
                        };
                        let (ok, fail, errors) = apply_delete_request(req, &sessions);
                        multi_selected.clear();
                        select_mode = true; // 保持管理模式，避免误恢复
                        all_sessions = list_sessions();
                        sessions = filtered_sessions(&all_sessions, filter, &query, grouped);
                        selected = selected.min(sessions.len().saturating_sub(1));
                        preview_cache = None;
                        preview_scroll = 0;
                        status = format_delete_status(ok, fail, &errors);
                    }
                    crossterm::event::KeyCode::Char('n') if confirm_delete.is_some() => {
                        confirm_delete = None;
                        status = "取消删除".to_string();
                    }
                    // ─── 多选模式 ───
                    crossterm::event::KeyCode::Char('v')
                        if !search_mode && confirm_delete.is_none() =>
                    {
                        select_mode = !select_mode;
                        if !select_mode {
                            multi_selected.clear();
                            status.clear();
                        } else {
                            status = "管理模式：点列表勾选，底栏按钮操作".to_string();
                        }
                    }
                    crossterm::event::KeyCode::Char(' ')
                        if select_mode && !sessions.is_empty() && confirm_delete.is_none() =>
                    {
                        if multi_selected.contains(&selected) {
                            multi_selected.remove(&selected);
                        } else {
                            multi_selected.insert(selected);
                        }
                        status = format!("已选 {} 条", multi_selected.len());
                    }
                    _ => {}
                },
                crate::tui::AppEvent::Mouse(m) => {
                    if let Some(size) = terminal_size {
                        if let Some(tab) = crate::ui::screens::chrome::tab_hit_test(
                            ratatui::layout::Rect::new(0, 0, size.width, size.height),
                            &m,
                        ) {
                            return SessionExit::SwitchTab(tab);
                        }
                    }
                    if !matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) {
                        if m.column as usize >= LEFT_WIDTH {
                            match m.kind {
                                MouseEventKind::ScrollUp => {
                                    preview_scroll = preview_scroll.saturating_sub(3);
                                }
                                MouseEventKind::ScrollDown => {
                                    preview_scroll = preview_scroll.saturating_add(3);
                                }
                                _ => {}
                            }
                        }
                        break 'handle;
                    }
                    let height = terminal_size.map(|s| s.height as usize).unwrap_or(24);
                    let bottom_bar_start = height.saturating_sub(3);
                    let row = m.row as usize;
                    let col = m.column;

                    // ─── 底部按钮区域（最后3行）点击 ───
                    if row >= bottom_bar_start {
                        let counter = if sessions.is_empty() {
                            "0/0".to_string()
                        } else {
                            format!("{}/{}", selected + 1, sessions.len())
                        };
                        let buttons = build_bar_buttons(
                            select_mode,
                            confirm_delete.is_some(),
                            UnicodeWidthStr::width(counter.as_str()),
                        );
                        let Some(action) = hit_bar_button(col, &buttons) else {
                            break 'handle;
                        };
                        match action {
                            BarAction::Manage => {
                                select_mode = true;
                                status = "管理模式：点列表勾选，底栏按钮操作".to_string();
                            }
                            BarAction::Resume => {
                                if !sessions.is_empty() && !select_mode && confirm_delete.is_none()
                                {
                                    return SessionExit::Resume(sessions[selected].clone());
                                }
                            }
                            BarAction::Back => return SessionExit::None,
                            BarAction::SelectAll => {
                                if multi_selected.len() == sessions.len() {
                                    multi_selected.clear();
                                } else {
                                    multi_selected = (0..sessions.len()).collect();
                                }
                                status = format!("已选 {} 条", multi_selected.len());
                            }
                            BarAction::DeleteSelected => {
                                if multi_selected.is_empty() {
                                    status = "请先勾选要删除的对话".to_string();
                                } else {
                                    let indices: Vec<usize> =
                                        multi_selected.iter().copied().collect();
                                    let count = indices.len();
                                    confirm_delete = Some(DeleteRequest::Multi(indices));
                                    status = format!(
                                        "确认删除 {count} 条对话？点「确认删除」或「取消」"
                                    );
                                }
                            }
                            BarAction::ExitManage => {
                                select_mode = false;
                                multi_selected.clear();
                                confirm_delete = None;
                                status.clear();
                            }
                            BarAction::ConfirmYes => {
                                if let Some(req) = confirm_delete.take() {
                                    let (ok, fail, errors) = apply_delete_request(req, &sessions);
                                    multi_selected.clear();
                                    select_mode = true;
                                    all_sessions = list_sessions();
                                    sessions =
                                        filtered_sessions(&all_sessions, filter, &query, grouped);
                                    selected = selected.min(sessions.len().saturating_sub(1));
                                    preview_cache = None;
                                    preview_scroll = 0;
                                    status = format_delete_status(ok, fail, &errors);
                                }
                            }
                            BarAction::ConfirmNo => {
                                confirm_delete = None;
                                status = "取消删除".to_string();
                            }
                        }
                        break 'handle;
                    }

                    // 确认态时，只允许底栏按钮，防止误触恢复
                    if confirm_delete.is_some() {
                        break 'handle;
                    }

                    // ─── 列表区域点击 ───
                    if m.column as usize >= LEFT_WIDTH {
                        // 管理模式禁止右侧误恢复
                        if select_mode {
                            break 'handle;
                        }
                        // 仅点「恢复此对话」按钮区域（header 内）才恢复
                        if !sessions.is_empty() && (4..=5).contains(&row) {
                            return SessionExit::Resume(sessions[selected].clone());
                        }
                        break 'handle;
                    }
                    // 左侧列表点击
                    if let Some(idx) = session_hit(selected, sessions.len(), row) {
                        if select_mode {
                            if multi_selected.contains(&idx) {
                                multi_selected.remove(&idx);
                            } else {
                                multi_selected.insert(idx);
                            }
                            selected = idx;
                            status = format!("已选 {} 条", multi_selected.len());
                        } else {
                            selected = idx;
                            preview_scroll = 0;
                            status.clear();
                        }
                    }
                }
                crate::tui::AppEvent::Resize => {}
                _ => {}
            }
        }
        // 非交互 Tick 不需要重绘（会话页无动画），避免 16ms 空转重绘到 62Hz。
        // 但回填完成的当帧必须落屏。
        if matches!(ev, crate::tui::AppEvent::Tick) && !force_draw {
            continue;
        }
        let preview = preview_cache
            .as_ref()
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        render_split(
            terminal,
            &sessions,
            preview,
            SessionRenderState {
                total: all_sessions.len(),
                selected,
                status: &status,
                filter,
                query: &query,
                grouped,
                search_mode,
                preview_scroll,
                select_mode,
                confirm_delete: confirm_delete.is_some(),
                multi_selected: &multi_selected,
            },
        )
        .ok();
    }
}

fn apply_delete_request(
    req: DeleteRequest,
    sessions: &[crate::config::SessionInfo],
) -> (usize, usize, Vec<String>) {
    match req {
        DeleteRequest::Single(idx) => {
            if let Some(s) = sessions.get(idx) {
                match delete_session(s) {
                    Ok(()) => (1, 0, Vec::new()),
                    Err(e) => (0, 1, vec![format!("{}:{e}", s.id)]),
                }
            } else {
                (0, 0, Vec::new())
            }
        }
        DeleteRequest::Multi(indices) => {
            let targets: Vec<_> = indices
                .iter()
                .filter_map(|&i| sessions.get(i).cloned())
                .collect();
            delete_sessions_batch(&targets)
        }
        DeleteRequest::PurgeSource(sf) => {
            let targets: Vec<_> = sessions
                .iter()
                .filter(|s| sf.matches(&s.source))
                .cloned()
                .collect();
            delete_sessions_batch(&targets)
        }
    }
}

fn format_delete_status(ok: usize, fail: usize, errors: &[String]) -> String {
    if fail == 0 {
        format!("已删除 {ok} 条对话")
    } else if errors.is_empty() {
        format!("删除 {ok} 条，失败 {fail} 条")
    } else {
        format!(
            "删除 {ok} 条，失败 {fail} 条 · {}",
            errors.first().cloned().unwrap_or_default()
        )
    }
}

fn render_split(
    terminal: &mut Tui,
    sessions: &[crate::config::SessionInfo],
    preview: &str,
    state: SessionRenderState<'_>,
) -> std::io::Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        crate::ui::screens::chrome::render_tab_bar(
            f,
            area,
            crate::ui::screens::chrome::Tab::Sessions,
        );
        let body = Rect::new(
            area.x,
            area.y + crate::ui::screens::chrome::TAB_BAR_HEIGHT,
            area.width,
            area.height
                .saturating_sub(crate::ui::screens::chrome::TAB_BAR_HEIGHT),
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(body);
        let header = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(chunks[0]);
        let title = Paragraph::new(vec![
            Line::from(Span::styled(
                "对话 / Sessions",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!(
                    "来源：{} · {} / {} · 搜索：{} · 分组：{}{}",
                    state.filter.label(),
                    sessions.len(),
                    state.total,
                    if state.query.is_empty() {
                        "无"
                    } else {
                        state.query
                    },
                    if state.grouped { "开" } else { "关" },
                    if state.search_mode {
                        " · 输入中"
                    } else {
                        ""
                    }
                ),
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .alignment(Alignment::Center);
        f.render_widget(title, header[0]);

        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(LEFT_WIDTH as u16), Constraint::Min(1)])
            .split(header[1]);
        render_session_list(
            f,
            panes[0],
            sessions,
            state.selected,
            state.grouped,
            state.select_mode,
            state.multi_selected,
        );
        render_session_detail(
            f,
            panes[1],
            sessions.get(state.selected),
            preview,
            state.status,
            state.preview_scroll,
        );

        let selected_text = if sessions.is_empty() {
            "0/0".to_string()
        } else {
            format!("{}/{}", state.selected + 1, sessions.len())
        };
        let buttons = build_bar_buttons(
            state.select_mode,
            state.confirm_delete,
            UnicodeWidthStr::width(selected_text.as_str()),
        );
        let mut spans = vec![
            Span::styled(selected_text, Style::default().fg(Color::Cyan)),
            Span::raw("  "),
        ];
        for (i, btn) in buttons.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            let style = match btn.action {
                BarAction::Manage => Style::default().fg(Color::Black).bg(Color::Magenta),
                BarAction::Resume => Style::default().fg(Color::Black).bg(Color::Green),
                BarAction::Back | BarAction::ExitManage | BarAction::ConfirmNo => {
                    Style::default().fg(Color::Black).bg(Color::Gray)
                }
                BarAction::SelectAll => Style::default().fg(Color::Black).bg(Color::Yellow),
                BarAction::DeleteSelected | BarAction::ConfirmYes => {
                    Style::default().fg(Color::White).bg(Color::Red)
                }
            };
            spans.push(Span::styled(btn.label, style));
        }
        if state.select_mode && !state.confirm_delete {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("已选 {}", state.multi_selected.len()),
                Style::default().fg(Color::Red),
            ));
        }
        let hint = Paragraph::new(Line::from(spans))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .alignment(Alignment::Left);
        f.render_widget(hint, chunks[1]);
    })?;
    Ok(())
}

fn render_session_list(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    sessions: &[crate::config::SessionInfo],
    selected: usize,
    grouped: bool,
    select_mode: bool,
    multi_selected: &std::collections::HashSet<usize>,
) {
    if sessions.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(Span::styled("暂无会话", Style::default().fg(Color::Yellow))),
            Line::from(""),
            Line::from(Span::styled(
                "路径：~/.codex/sessions · ~/.claude/projects · opencode.db",
                Style::default().fg(Color::Gray),
            )),
        ])
        .block(
            Block::default()
                .title("标题")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .alignment(Alignment::Center);
        f.render_widget(empty, area);
        return;
    }
    let start =
        crate::ui::screens::chrome::list_window_start(selected, sessions.len(), SESSION_VISIBLE);
    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .skip(start)
        .take(SESSION_VISIBLE)
        .map(|(idx, session)| {
            let time = compact_time(&session.time);
            let title = compact_title(&session.title, 18);
            let check = if select_mode {
                if multi_selected.contains(&idx) {
                    "☑ "
                } else {
                    "☐ "
                }
            } else {
                ""
            };
            let text = format!(
                "{}{} {} {}",
                check,
                source_badge(&session.source),
                time,
                title
            );
            if idx == selected {
                // 克制派选中态：▍竖条 + 面板明度（与供应商/扩展卡一致）。
                ListItem::new(Line::from(Span::styled(
                    format!("▍{text}"),
                    Style::default()
                        .bg(crate::ui::theme::PANEL_BG)
                        .add_modifier(Modifier::BOLD),
                )))
            } else if select_mode && multi_selected.contains(&idx) {
                ListItem::new(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::Cyan),
                )))
            } else {
                ListItem::new(text)
            }
        })
        .collect();
    f.render_widget(
        List::new(items).block(
            Block::default()
                .title(if grouped {
                    "会话列表 · 分组"
                } else {
                    "会话列表"
                })
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn render_session_detail(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    session: Option<&crate::config::SessionInfo>,
    preview: &str,
    status: &str,
    preview_scroll: u16,
) {
    if let Some(session) = session {
        // 详情布局：
        // 1) 完整标题（主信息）
        // 2) 来源 / 时间
        // 3) 操作按钮
        // 4) 次要：会话 ID（短）
        // 5) 仅 jsonl 显示文件路径；OpenCode 不显示整库路径
        // 6) 状态提示
        // 7) 对话正文
        let mut header_lines = Vec::new();
        header_lines.push(Line::from(Span::styled(
            session.title.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        header_lines.push(Line::from(vec![
            Span::styled(
                source_label(&session.source),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  ·  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                display_time(&session.time),
                Style::default().fg(Color::Gray),
            ),
        ]));
        header_lines.push(Line::from(vec![
            Span::styled(
                " 恢复此对话 ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ),
            Span::raw("  "),
            Span::styled("点按钮或 Enter", Style::default().fg(Color::DarkGray)),
        ]));
        header_lines.push(Line::from(Span::styled(
            format!("ID  {}", short_id(&session.id)),
            Style::default().fg(Color::DarkGray),
        )));
        // 三端统一显示存储信息（含义不同：文件 vs 库）
        header_lines.push(Line::from(Span::styled(
            storage_label(session),
            Style::default().fg(Color::DarkGray),
        )));
        if !status.is_empty() {
            header_lines.push(Line::from(Span::styled(
                status,
                Style::default().fg(Color::Yellow),
            )));
        }

        let header_height = header_lines.len().saturating_add(2).min(u16::MAX as usize) as u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_height), Constraint::Min(4)])
            .split(area);
        let header = Paragraph::new(header_lines)
            .block(
                Block::default()
                    .title("会话详情")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(header, chunks[0]);

        let mut body_lines = vec![Line::from(Span::styled(
            format!(
                "对话正文 · 第 {} 行起（滚轮 / PgUp PgDn）",
                preview_scroll.saturating_add(1)
            ),
            Style::default().fg(Color::DarkGray),
        ))];
        body_lines.push(Line::from(""));
        for line in preview.lines() {
            body_lines.push(Line::from(line.to_string()));
        }
        let body = Paragraph::new(body_lines)
            .block(
                Block::default()
                    .title("对话正文")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .alignment(Alignment::Left)
            .scroll((preview_scroll, 0))
            .wrap(Wrap { trim: false });
        f.render_widget(body, chunks[1]);
    } else {
        let panel = Paragraph::new("没有选中的会话。")
            .block(
                Block::default()
                    .title("会话详情")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .alignment(Alignment::Center);
        f.render_widget(panel, area);
    }
}

fn session_hit(selected: usize, len: usize, row: usize) -> Option<usize> {
    if row < 4 || len == 0 {
        return None;
    }
    let start = crate::ui::screens::chrome::list_window_start(selected, len, SESSION_VISIBLE);
    let idx = start + row.saturating_sub(4);
    if idx < len && idx < start + SESSION_VISIBLE {
        Some(idx)
    } else {
        None
    }
}

fn session_preview(session: &crate::config::SessionInfo) -> String {
    if session.source == "opencode" {
        return preview_opencode_session(std::path::Path::new(&session.file), &session.id)
            .unwrap_or_else(|error| format!("读取失败：{error}"));
    }
    preview_file(std::path::Path::new(&session.file), PREVIEW_MAX_RECORDS)
        .unwrap_or_else(|error| format!("读取失败：{error}"))
}

fn preview_file(path: &std::path::Path, max_records: usize) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut messages = Vec::new();
    let mut record_limit_hit = false;
    for (index, line) in reader.lines().enumerate() {
        if index >= max_records {
            record_limit_hit = true;
            break;
        }
        let line = line.map_err(|e| e.to_string())?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(message) = conversation_message(&value) {
            push_message(&mut messages, message);
        }
        if messages.len() >= PREVIEW_MAX_MESSAGES {
            record_limit_hit = true;
            break;
        }
    }
    Ok(format_conversation(&messages, record_limit_hit))
}

fn conversation_message(value: &serde_json::Value) -> Option<ConversationMessage> {
    let candidate = if value.get("type").and_then(|v| v.as_str()) == Some("response_item") {
        value.get("payload")?
    } else if matches!(
        value.get("type").and_then(|v| v.as_str()),
        Some("user" | "assistant")
    ) {
        value.get("message").unwrap_or(value)
    } else if value.get("role").is_some() {
        value
    } else {
        return None;
    };
    if candidate
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|kind| kind != "message")
    {
        return None;
    }
    let role = candidate.get("role").and_then(|v| v.as_str())?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let text = message_text(candidate)?;
    let text = clean_message_text(&text);
    if text.is_empty() || is_internal_message(&text) {
        return None;
    }
    Some(ConversationMessage {
        role: role.to_string(),
        text,
    })
}

fn message_text(value: &serde_json::Value) -> Option<String> {
    if let Some(content) = value.get("content") {
        return content_text(content);
    }
    value
        .get("text")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}

fn content_text(content: &serde_json::Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let items = content.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("text");
        if !matches!(kind, "text" | "input_text" | "output_text") {
            continue;
        }
        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn clean_message_text(text: &str) -> String {
    let text = strip_hidden_reasoning(text);
    let mut out = String::new();
    let mut blank = false;
    for raw in text.replace('\r', "").lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            if !blank && !out.is_empty() {
                out.push('\n');
            }
            blank = true;
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.extend(line.chars().filter(|c| !c.is_control() || *c == '\t'));
        blank = false;
    }
    out.trim().to_string()
}

fn strip_hidden_reasoning(text: &str) -> String {
    let mut out = text.to_string();
    for (start, end) in [("<thinking>", "</thinking>"), ("<think>", "</think>")] {
        while let Some(begin) = out.find(start) {
            let remove_end = out[begin + start.len()..]
                .find(end)
                .map(|offset| begin + start.len() + offset + end.len())
                .unwrap_or(out.len());
            out.replace_range(begin..remove_end, "");
        }
        out = out.replace(end, "");
    }
    out
}

fn is_internal_message(text: &str) -> bool {
    let text = text.trim_start();
    [
        "<environment_context>",
        "<permissions instructions>",
        "<collaboration_mode>",
        "<skills_instructions>",
        "<model_switch>",
        "<turn_aborted>",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

fn push_message(messages: &mut Vec<ConversationMessage>, message: ConversationMessage) {
    if let Some(last) = messages.last_mut() {
        if last.role == message.role {
            if last.text != message.text {
                last.text.push_str("\n\n");
                last.text.push_str(&message.text);
            }
            return;
        }
    }
    messages.push(message);
}

fn format_conversation(messages: &[ConversationMessage], truncated: bool) -> String {
    if messages.is_empty() {
        return "没有找到可显示的用户/助手正文。系统事件和工具 JSON 已隐藏。".to_string();
    }
    let mut out = String::new();
    for message in messages {
        let heading = if message.role == "user" {
            "你"
        } else {
            "助手"
        };
        let block = format!("{heading}\n{}", message.text);
        if out.len() + block.len() + 2 > PREVIEW_MAX_CHARS {
            out.push_str("\n\n... 对话过长，后续正文未加载 ...");
            return out;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&block);
    }
    if truncated {
        out.push_str("\n\n... 记录较长，仅显示已加载的对话正文 ...");
    }
    out
}

fn compact_time(time: &str) -> String {
    if time.len() >= 16 {
        time[5..16].to_string()
    } else if time.is_empty() {
        "--".to_string()
    } else {
        time.to_string()
    }
}

fn compact_title(title: &str, max: usize) -> String {
    let mut out = title.replace('\n', " ");
    if out.chars().count() > max {
        out = out.chars().take(max.saturating_sub(3)).collect::<String>() + "...";
    }
    out
}

fn visual_line_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.lines()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

fn filtered_sessions(
    sessions: &[crate::config::SessionInfo],
    filter: SourceFilter,
    query: &str,
    grouped: bool,
) -> Vec<crate::config::SessionInfo> {
    let query = query.trim().to_ascii_lowercase();
    let mut out = sessions
        .iter()
        .filter(|session| filter.matches(&session.source))
        .filter(|session| {
            if query.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {} {} {}",
                session.source, session.id, session.title, session.time, session.file
            )
            .to_ascii_lowercase();
            haystack.contains(&query)
        })
        .cloned()
        .collect::<Vec<_>>();
    if grouped {
        out.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then_with(|| session_group(a).cmp(&session_group(b)))
                .then_with(|| b.time.cmp(&a.time))
        });
    }
    out
}

fn session_group(session: &crate::config::SessionInfo) -> String {
    if session.source == "opencode" {
        return "opencode.db".to_string();
    }
    let path = std::path::Path::new(&session.file);
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn list_sessions() -> Vec<crate::config::SessionInfo> {
    let mut sessions = Vec::new();
    sessions.extend(list_codex_sessions());
    sessions.extend(list_claude_sessions());
    sessions.extend(list_opencode_sessions());
    sessions.sort_by(|a, b| b.time.cmp(&a.time));
    sessions
}

fn list_codex_sessions() -> Vec<crate::config::SessionInfo> {
    let dir = crate::config::home().join(".codex").join("sessions");
    let mut sessions = Vec::new();
    walk_jsonl(&dir, &mut sessions, "codex");
    sessions.sort_by(|a, b| b.time.cmp(&a.time));
    sessions
}

fn list_claude_sessions() -> Vec<crate::config::SessionInfo> {
    let dir = crate::config::home().join(".claude").join("projects");
    let mut sessions = Vec::new();
    walk_jsonl(&dir, &mut sessions, "claude");
    sessions
}

fn list_opencode_sessions() -> Vec<crate::config::SessionInfo> {
    let db = crate::config::home()
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    read_opencode_sessions(&db).unwrap_or_default()
}

fn walk_jsonl(dir: &std::path::Path, sessions: &mut Vec<crate::config::SessionInfo>, source: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk_jsonl(&path, sessions, source);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Some(session) = parse_session_file(&path, source) {
                sessions.push(session);
            }
        }
    }
}

fn parse_session_file(path: &std::path::Path, source: &str) -> Option<crate::config::SessionInfo> {
    let fname = path.file_name()?.to_str()?;
    let id = if source == "codex" {
        let re =
            regex::Regex::new(r"rollout-\d{4}-\d{2}-\d{2}T([\d-]+)-([0-9a-f-]+)\.jsonl$").ok()?;
        re.captures(fname)?.get(2)?.as_str().to_string()
    } else {
        fname.trim_end_matches(".jsonl").to_string()
    };

    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut title = String::new();
    let mut time = String::new();

    for line in reader.lines().take(200).flatten() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if time.is_empty() {
                if let Some(ts) = val.get("timestamp").and_then(|v| v.as_str()) {
                    time = ts.to_string();
                }
            }
            if title.is_empty() {
                if let Some(message) = conversation_message(&val) {
                    if message.role == "user" {
                        title = compact_title(&message.text, 80);
                    }
                }
            }
            if !title.is_empty() && !time.is_empty() {
                break;
            }
        }
    }

    if title.is_empty() {
        title = id.clone();
    }

    Some(crate::config::SessionInfo {
        source: source.to_string(),
        id,
        file: path.to_string_lossy().to_string(),
        title,
        time,
    })
}

fn read_opencode_sessions(db: &std::path::Path) -> Result<Vec<crate::config::SessionInfo>, String> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open {}: {e}", db.display()))?;
    let mut stmt = conn
        .prepare(
            "select id, title, time_updated from session where coalesce(time_archived, 0) = 0 order by time_updated desc limit 200",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let title: String = row.get(1)?;
            let time_ms: i64 = row.get(2)?;
            Ok(crate::config::SessionInfo {
                source: "opencode".to_string(),
                id,
                file: db.to_string_lossy().to_string(),
                title,
                time: millis_to_time(time_ms),
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn preview_opencode_session(db: &std::path::Path, session_id: &str) -> Result<String, String> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open {}: {e}", db.display()))?;
    let mut stmt = conn
        .prepare(
            "select m.data, p.data from message m join part p on p.message_id = m.id where m.session_id = ?1 order by p.time_created asc limit ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id, PREVIEW_MAX_RECORDS as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut messages = Vec::new();
    let mut truncated = false;
    for (message_json, part_json) in rows.filter_map(Result::ok) {
        let Ok(message_value) = serde_json::from_str::<serde_json::Value>(&message_json) else {
            continue;
        };
        let Ok(part_value) = serde_json::from_str::<serde_json::Value>(&part_json) else {
            continue;
        };
        if part_value.get("type").and_then(|v| v.as_str()) != Some("text") {
            continue;
        }
        let Some(role) = message_value.get("role").and_then(|v| v.as_str()) else {
            continue;
        };
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let Some(text) = part_value.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        let text = clean_message_text(text);
        if text.is_empty() || is_internal_message(&text) {
            continue;
        }
        push_message(
            &mut messages,
            ConversationMessage {
                role: role.to_string(),
                text,
            },
        );
        if messages.len() >= PREVIEW_MAX_MESSAGES {
            truncated = true;
            break;
        }
    }
    Ok(format_conversation(&messages, truncated))
}

fn millis_to_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.naive_local().format("%Y-%m-%dT%H:%M:%S").to_string())
        .unwrap_or_default()
}

fn source_badge(source: &str) -> &'static str {
    match source {
        "codex" => "[Codex]",
        "claude" => "[Claude]",
        "opencode" => "[Open]",
        _ => "[Other]",
    }
}

// ─── 删除对话（委托 lib session_store，保证 CLI/TUI 一致硬删）───

pub fn delete_session(session: &crate::config::SessionInfo) -> Result<(), String> {
    let s = trivium::session_store::SessionInfo {
        source: session.source.clone(),
        id: session.id.clone(),
        file: session.file.clone(),
        title: session.title.clone(),
        time: session.time.clone(),
    };
    trivium::session_store::delete_session(&s)
}

/// 批量删除多个对话。返回 (成功数, 失败数, 错误摘要)。
pub fn delete_sessions_batch(
    sessions: &[crate::config::SessionInfo],
) -> (usize, usize, Vec<String>) {
    let mapped: Vec<_> = sessions
        .iter()
        .map(|session| trivium::session_store::SessionInfo {
            source: session.source.clone(),
            id: session.id.clone(),
            file: session.file.clone(),
            title: session.title.clone(),
            time: session.time.clone(),
        })
        .collect();
    trivium::session_store::delete_sessions_batch(&mapped)
}

/// 按来源批量删除：给定 filter 删所有匹配项。
#[allow(dead_code)]
pub fn purge_sessions(filter: Option<&str>) -> (usize, usize, Vec<String>) {
    let all = list_sessions();
    let targets: Vec<_> = all
        .into_iter()
        .filter(|s| filter.map(|f| s.source == f).unwrap_or(true))
        .collect();
    delete_sessions_batch(&targets)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarAction {
    Manage,
    Resume,
    Back,
    SelectAll,
    DeleteSelected,
    ExitManage,
    ConfirmYes,
    ConfirmNo,
}

#[derive(Clone, Copy, Debug)]
struct BarButton {
    action: BarAction,
    label: &'static str,
    start: u16,
    end: u16,
}

fn build_bar_buttons(select_mode: bool, confirm: bool, counter_width: usize) -> Vec<BarButton> {
    let mut col = counter_width.saturating_add(2) as u16; // "N/M  "
    let mut buttons = Vec::new();
    let push =
        |buttons: &mut Vec<BarButton>, col: &mut u16, action: BarAction, label: &'static str| {
            let width = UnicodeWidthStr::width(label) as u16;
            buttons.push(BarButton {
                action,
                label,
                start: *col,
                end: *col + width,
            });
            *col = col.saturating_add(width.saturating_add(1));
        };
    if confirm {
        push(&mut buttons, &mut col, BarAction::ConfirmYes, " 确认删除 ");
        push(&mut buttons, &mut col, BarAction::ConfirmNo, " 取消 ");
    } else if select_mode {
        push(&mut buttons, &mut col, BarAction::SelectAll, " 全选 ");
        push(
            &mut buttons,
            &mut col,
            BarAction::DeleteSelected,
            " 删除选中 ",
        );
        push(&mut buttons, &mut col, BarAction::ExitManage, " 退出管理 ");
    } else {
        push(&mut buttons, &mut col, BarAction::Manage, " 管理会话 ");
        push(&mut buttons, &mut col, BarAction::Resume, " 恢复 ");
        push(&mut buttons, &mut col, BarAction::Back, " 返回 ");
    }
    buttons
}

fn hit_bar_button(col: u16, buttons: &[BarButton]) -> Option<BarAction> {
    buttons
        .iter()
        .find(|b| col >= b.start && col < b.end)
        .map(|b| b.action)
}

fn source_label(source: &str) -> &'static str {
    match source {
        "codex" => "Codex",
        "claude" => "Claude Code",
        "opencode" => "OpenCode",
        _ => "Other",
    }
}

fn short_id(id: &str) -> String {
    if id.len() <= 16 {
        id.to_string()
    } else {
        // 按字符截断，防非 ASCII 边界 panic（与 crate::ui::screens::detail::masked_key 同理）。
        let chars: Vec<char> = id.chars().collect();
        let head: String = chars[..8].iter().collect();
        let tail: String = chars[chars.len().saturating_sub(6)..].iter().collect();
        format!("{head}…{tail}")
    }
}

fn storage_label(session: &crate::config::SessionInfo) -> String {
    match session.source.as_str() {
        "opencode" => format!("存储  SQLite · {}", compact_path(&session.file, 40)),
        "claude" | "codex" => format!("存储  文件 · {}", compact_path(&session.file, 40)),
        _ => format!("存储  {}", compact_path(&session.file, 40)),
    }
}

fn display_time(time: &str) -> String {
    // 支持 ISO / 本地 naive 时间，详情里完整显示，去掉毫秒尾巴
    let t = time.trim();
    if t.is_empty() {
        return "未知时间".to_string();
    }
    if let Some(dot) = t.find('.') {
        return t[..dot].replace('T', " ");
    }
    t.replace('T', " ")
}

fn compact_path(path: &str, max: usize) -> String {
    let home = crate::config::home().to_string_lossy().to_string();
    let display = if path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    };
    if display.chars().count() <= max {
        display
    } else {
        let chars: Vec<char> = display.chars().collect();
        let keep = max.saturating_sub(1);
        chars.into_iter().take(keep).collect::<String>() + "…"
    }
}

#[allow(dead_code)]
fn resume_command_label(session: &crate::config::SessionInfo) -> String {
    match session.source.as_str() {
        "codex" => format!("codex resume {}", session.id),
        "claude" => format!("claude --resume {}", session.id),
        "opencode" => format!("opencode --session {}", session.id),
        _ => "不支持恢复".to_string(),
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn preview_file_caps_large_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut text = String::new();
        for _ in 0..200 {
            text.push_str(r#"{"role":"user","content":[{"text":"abcdefghijklmnopqrstuvwxyz"}]}"#);
            text.push('\n');
        }
        std::fs::write(&path, text).unwrap();

        let preview = preview_file(&path, 10).unwrap();

        assert!(preview.contains("记录较长"));
        assert!(!preview.contains('{'));
    }

    #[test]
    fn extracts_codex_and_claude_messages_without_raw_json() {
        let codex = serde_json::json!({
            "type":"response_item",
            "payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"line one\nline two"}]}
        });
        let claude = serde_json::json!({
            "type":"user",
            "message":{"role":"user","content":"hello"}
        });
        let event = serde_json::json!({"type":"session_meta","payload":{"cwd":"/tmp"}});

        assert_eq!(
            conversation_message(&codex),
            Some(ConversationMessage {
                role: "assistant".to_string(),
                text: "line one\nline two".to_string(),
            })
        );
        assert_eq!(conversation_message(&claude).unwrap().text, "hello");
        assert_eq!(conversation_message(&event), None);
    }

    #[test]
    fn hides_internal_context_and_tool_blocks() {
        let internal = serde_json::json!({
            "type":"response_item",
            "payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>secret</environment_context>"}]}
        });
        let tool = serde_json::json!({
            "type":"assistant",
            "message":{"role":"assistant","content":[{"type":"tool_use","name":"bash","input":{"command":"pwd"}}]}
        });

        assert_eq!(conversation_message(&internal), None);
        assert_eq!(conversation_message(&tool), None);
    }

    #[test]
    fn session_hit_maps_visible_rows() {
        assert_eq!(session_hit(0, 3, 4), Some(0));
        assert_eq!(session_hit(0, 3, 6), Some(2));
        assert_eq!(session_hit(0, 3, 20), None);
    }

    #[test]
    fn counts_visually_wrapped_preview_lines() {
        assert_eq!(visual_line_count("1234567890", 5), 2);
        assert_eq!(visual_line_count("中文中文", 4), 2);
        assert_eq!(visual_line_count("a\n\nb", 10), 3);
    }

    #[test]
    fn filters_sessions_by_source_and_query() {
        let sessions = vec![
            crate::config::SessionInfo {
                source: "codex".to_string(),
                id: "a".to_string(),
                file: "/tmp/project/a.jsonl".to_string(),
                title: "hello codex".to_string(),
                time: "2026-01-02T00:00:00Z".to_string(),
            },
            crate::config::SessionInfo {
                source: "claude".to_string(),
                id: "b".to_string(),
                file: "/tmp/project/b.jsonl".to_string(),
                title: "hello claude".to_string(),
                time: "2026-01-01T00:00:00Z".to_string(),
            },
        ];

        let filtered = filtered_sessions(&sessions, SourceFilter::Claude, "hello", false);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "b");
    }

    #[test]
    fn parses_claude_nested_message_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"user","message":{"role":"user","content":"hello claude"},"timestamp":"2026-07-07T01:02:03Z","sessionId":"abc"}"#,
        )
        .unwrap();

        let session = parse_session_file(&path, "claude").unwrap();

        assert_eq!(session.source, "claude");
        assert_eq!(session.id, "abc");
        assert_eq!(session.title, "hello claude");
        assert_eq!(session.time, "2026-07-07T01:02:03Z");
    }

    #[test]
    fn reads_opencode_sessions_from_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "create table session (id text primary key, title text not null, time_updated integer not null, time_archived integer)",
            [],
        )
        .unwrap();
        conn.execute("create table message (id text primary key, session_id text not null, data text not null, time_created integer not null)", []).unwrap();
        conn.execute("create table part (id text primary key, message_id text not null, session_id text not null, data text not null, time_created integer not null)", []).unwrap();
        conn.execute(
            "insert into session (id, title, time_updated, time_archived) values ('ses_1', 'OpenCode title', 1783400000000, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            r#"insert into message (id, session_id, data, time_created) values ('msg_1', 'ses_1', '{"role":"assistant"}', 1)"#,
            [],
        )
        .unwrap();
        conn.execute(
            r#"insert into part (id, message_id, session_id, data, time_created) values ('part_1', 'msg_1', 'ses_1', '{"type":"text","text":"hello opencode"}', 1)"#,
            [],
        )
        .unwrap();
        drop(conn);

        let sessions = read_opencode_sessions(&db).unwrap();
        let preview = preview_opencode_session(&db, "ses_1").unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source, "opencode");
        assert_eq!(sessions[0].id, "ses_1");
        assert_eq!(sessions[0].title, "OpenCode title");
        assert!(preview.contains("hello opencode"));
    }

    #[test]
    fn deletes_opencode_session_hard() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "create table session (id text primary key, title text not null, time_updated integer not null, time_archived integer)",
            [],
        )
        .unwrap();
        conn.execute(
            "create table message (id text primary key, session_id text not null, data text not null, time_created integer not null, foreign key(session_id) references session(id) on delete cascade)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into session (id, title, time_updated, time_archived) values ('ses_del', 'to delete', 1, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            r#"insert into message (id, session_id, data, time_created) values ('msg_del', 'ses_del', '{"role":"user"}', 1)"#,
            [],
        )
        .unwrap();
        drop(conn);

        trivium::session_store::delete_opencode_session(&db, "ses_del").unwrap();

        let conn = rusqlite::Connection::open(&db).unwrap();
        let count: i64 = conn
            .query_row(
                "select count(*) from session where id = 'ses_del'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn deletes_jsonl_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.jsonl");
        std::fs::write(&path, r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-07-07T01:02:03Z"}"#).unwrap();
        let session = crate::config::SessionInfo {
            source: "claude".to_string(),
            id: "abc".to_string(),
            file: path.to_string_lossy().to_string(),
            title: "hi".to_string(),
            time: "2026-07-07T01:02:03Z".to_string(),
        };
        delete_session(&session).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn bar_button_hit_matches_layout() {
        let buttons = build_bar_buttons(false, false, 3);
        assert_eq!(
            hit_bar_button(buttons[0].start, &buttons),
            Some(BarAction::Manage)
        );
        assert_eq!(
            hit_bar_button(buttons[1].start, &buttons),
            Some(BarAction::Resume)
        );
        assert_eq!(
            hit_bar_button(buttons[2].start, &buttons),
            Some(BarAction::Back)
        );
        assert_eq!(hit_bar_button(0, &buttons), None);

        let manage = build_bar_buttons(true, false, 3);
        assert_eq!(
            hit_bar_button(manage[0].start, &manage),
            Some(BarAction::SelectAll)
        );
        assert_eq!(
            hit_bar_button(manage[1].start, &manage),
            Some(BarAction::DeleteSelected)
        );

        let confirm = build_bar_buttons(true, true, 3);
        assert_eq!(
            hit_bar_button(confirm[0].start, &confirm),
            Some(BarAction::ConfirmYes)
        );
        assert_eq!(
            hit_bar_button(confirm[1].start, &confirm),
            Some(BarAction::ConfirmNo)
        );
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    #[test]
    fn probe_list_sessions_duration() {
        let t0 = std::time::Instant::now();
        let all = list_sessions();
        eprintln!(
            "[probe] list_sessions → {} sessions in {:?}",
            all.len(),
            t0.elapsed()
        );
    }
}
