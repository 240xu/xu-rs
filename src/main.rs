mod app;
mod config;
mod session;
mod tui;
mod ui;

use app::*;
use crossterm::event::{KeyCode, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use trivium::agent_tools::{status_rows, AgentToolStatus};
use trivium::domain::{AgentTarget, ProviderProfile};
use trivium::state::{ProviderHealth, UiSurface};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Mode {
    Menu,
    Provider,
    ProviderDetail,
    ProviderAddForm,
    ProviderAddConfirm,
    ProviderEditForm,
    ProviderEditConfirm,
    ProviderDeleteConfirm,
    ProviderActionResult,
    ProviderPresetList,
    MultiProvider,
    PlanPreview,
    ApplyConfirm,
    ConnectionResult,
    Client,
    AgentDoctor,
    Session,
    OpenCodeSettings,
    Mcp,
    McpPresets,
    McpForm,
    McpFormConfirm,
    McpConfirm,
    Skills,
    SkillForm,
    SkillFormConfirm,
    SkillConfirm,
    Help,
    WebNotice,
    Quit,
}

#[derive(Default, Clone)]
pub struct ProviderState {
    pub providers: Vec<ProviderProfile>,
    pub error: Option<String>,
    pub sync_notice: Option<String>,
    pub selected: usize,
    pub current: BTreeMap<String, String>,
    pub health: BTreeMap<String, ProviderHealth>,
}

fn is_preview_mode(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::ProviderActionResult | Mode::PlanPreview | Mode::ConnectionResult | Mode::AgentDoctor
    )
}

/// 把占位/预览页的滚动事件转换为滚动偏移量；非滚动事件返回 None。
/// 支持 ↑/↓、PgUp/PgDn 与鼠标滚轮。
fn preview_scroll_delta(ev: &tui::AppEvent) -> Option<isize> {
    match ev {
        tui::AppEvent::Key(k) => match k.code {
            KeyCode::Up => Some(-1),
            KeyCode::Down => Some(1),
            KeyCode::PageUp => Some(-10),
            KeyCode::PageDown => Some(10),
            _ => None,
        },
        tui::AppEvent::Mouse(m) => match m.kind {
            MouseEventKind::ScrollUp => Some(-3),
            MouseEventKind::ScrollDown => Some(3),
            _ => None,
        },
        _ => None,
    }
}

/// 详情页字段保存：`provider update <id>` + 字段 flag → run_command。
fn detail_field_save_result(
    home: &Path,
    state: &ProviderState,
    field: usize,
    buf: &str,
) -> Result<String, String> {
    let provider = state
        .providers
        .get(state.selected)
        .ok_or_else(|| "没有选中的 provider。".to_string())?;
    let args = detail_update_args(provider, field, buf)?;
    trivium::cli::run_command(home, &args)
        .ok_or_else(|| "provider update 命令不可用。".to_string())
        .and_then(|result| result)
}

/// 同步读模型列表本地缓存（不联网；用于展开选择器秒开）。
fn detail_cached_models_result(state: &ProviderState) -> Result<Vec<String>, String> {
    let provider = state
        .providers
        .get(state.selected)
        .ok_or_else(|| "没有选中的 provider。".to_string())?;
    read_models_cache(&provider.id).ok_or_else(|| "无模型缓存".to_string())
}

/// 槽位展开选择：把拉取列表第 `sel` 个模型写入 `--claude-slot <slot>=<model>`。
fn detail_slot_pick_result(
    home: &Path,
    state: &ProviderState,
    list: &[String],
    sel: usize,
    slot: usize,
) -> Result<String, String> {
    let provider = state
        .providers
        .get(state.selected)
        .ok_or_else(|| "没有选中的 provider。".to_string())?;
    let model = list
        .get(sel)
        .ok_or_else(|| "没有选中的模型。".to_string())?;
    let args = detail_slot_args(provider, slot, model)?;
    trivium::cli::run_command(home, &args)
        .ok_or_else(|| "provider update 命令不可用。".to_string())
        .and_then(|result| result)
}

/// 获取模型多选初始勾选：当前 models 的请求名在拉取列表中的下标。
fn detail_multi_sel_init(state: &ProviderState, list: &[String]) -> BTreeSet<usize> {
    let Some(provider) = state.providers.get(state.selected) else {
        return BTreeSet::new();
    };
    let current = provider_model_rows(provider);
    list.iter()
        .enumerate()
        .filter_map(|(index, model)| {
            current
                .iter()
                .any(|row| row.request_name == *model)
                .then_some(index)
        })
        .collect()
}

/// 获取模型多选保存：勾选模型写为 models（--models-json 数组）→ run_command。
fn detail_multi_save_result(
    home: &Path,
    state: &ProviderState,
    list: &[String],
    selected: &BTreeSet<usize>,
) -> Result<String, String> {
    let provider = state
        .providers
        .get(state.selected)
        .ok_or_else(|| "没有选中的 provider。".to_string())?;
    let args = detail_multi_models_args(provider, list, selected)?;
    trivium::cli::run_command(home, &args)
        .ok_or_else(|| "provider update 命令不可用。".to_string())
        .and_then(|result| result)
}

/// 三端 Tab 顺序（Claude Code → Codex → OpenCode 循环）。
const DETAIL_TAB_ORDER: [AgentTarget; 3] = [
    AgentTarget::ClaudeCode,
    AgentTarget::Codex,
    AgentTarget::OpenCode,
];

/// 从当前 Tab 沿循环走 `step`（±1）得到目标 Tab。
fn detail_next_tab(current: AgentTarget, step: isize) -> AgentTarget {
    let index = DETAIL_TAB_ORDER
        .iter()
        .position(|target| *target == current)
        .unwrap_or(0) as isize;
    DETAIL_TAB_ORDER[((index + step).rem_euclid(3)) as usize]
}

/// 切换详情页三端 Tab；切换时清空编辑/拉取状态（各端布局不同）。
#[allow(clippy::too_many_arguments)]
fn detail_switch_tab(
    agent_tab: &mut AgentTarget,
    target: AgentTarget,
    field: &mut Option<usize>,
    buf: &mut String,
    fetch: &mut Option<Vec<String>>,
    fetch_sel: &mut usize,
    pick: &mut ui::screens::detail::DetailPickMode,
    feedback: &mut String,
    fetch_loading: &mut bool,
    fetch_rx: &mut Option<Receiver<Result<Vec<String>, String>>>,
    pending_pick: &mut ui::screens::detail::DetailPickMode,
    focus_zone: &mut u8,
    focus_index: &mut usize,
) {
    *agent_tab = target;
    *focus_zone = 0;
    *focus_index = 0;
    detail_fetch_reset(
        field,
        buf,
        fetch,
        fetch_sel,
        pick,
        feedback,
        fetch_loading,
        fetch_rx,
        pending_pick,
        focus_zone,
        focus_index,
    );
}

/// 清空详情页拉取/选择/反馈状态（切 Tab、离页、失败时统一调用）。
#[allow(clippy::too_many_arguments)]
fn detail_fetch_reset(
    field: &mut Option<usize>,
    buf: &mut String,
    fetch: &mut Option<Vec<String>>,
    fetch_sel: &mut usize,
    pick: &mut ui::screens::detail::DetailPickMode,
    feedback: &mut String,
    fetch_loading: &mut bool,
    fetch_rx: &mut Option<Receiver<Result<Vec<String>, String>>>,
    pending_pick: &mut ui::screens::detail::DetailPickMode,
    focus_zone: &mut u8,
    focus_index: &mut usize,
) {
    *focus_zone = 0;
    *focus_index = 0;
    *field = None;
    buf.clear();
    *fetch = None;
    *fetch_sel = 0;
    *pick = ui::screens::detail::DetailPickMode::None;
    feedback.clear();
    *fetch_loading = false;
    *fetch_rx = None;
    *pending_pick = ui::screens::detail::DetailPickMode::None;
}

/// 三端路由/生效开关状态：Claude/Codex 读路由表，OpenCode 读 opencode.json。
fn detail_route_status(home: &Path, target: AgentTarget, provider_id: &str) -> (bool, bool) {
    if target == AgentTarget::OpenCode {
        let active = read_opencode_provider_active(home, provider_id);
        return (active, active);
    }
    read_route_status(home, target, provider_id)
}

/// 完全权限开关（按当前端）：Claude=settings defaultMode；OpenCode=permission allow。
fn detail_toggle_permission(home: &Path, agent_tab: AgentTarget) -> Result<String, String> {
    match agent_tab {
        AgentTarget::ClaudeCode => {
            let enable = !read_bypass_permissions(home);
            set_bypass_permissions(home, enable)
        }
        AgentTarget::OpenCode => {
            let enable = !read_opencode_permission_allow(home);
            toggle_opencode_permission(home, enable)
        }
        _ => Err("该端无完全权限开关。".to_string()),
    }
}

/// 路由/生效开关（当前端 + 当前供应商）：Claude/Codex 写路由表，
/// OpenCode 写 opencode.json（生效）。
fn detail_toggle_route(
    home: &Path,
    state: &mut ProviderState,
    agent_tab: AgentTarget,
) -> Result<String, String> {
    let Some(provider_id) = state
        .providers
        .get(state.selected)
        .map(|provider| provider.id.clone())
    else {
        return Err("没有选中的 provider。".to_string());
    };
    if agent_tab == AgentTarget::OpenCode {
        let active = read_opencode_provider_active(home, &provider_id);
        return toggle_opencode_provider(state, &provider_id, !active);
    }
    toggle_route(home, agent_tab, &provider_id)
}

/// 供应商列表「长按删除」按下状态：记录按下位置的行号与起始时刻，
/// 由主循环 `Tick`（16ms）驱动进度，达到阈值后进入删除确认。
struct ProviderPress {
    index: usize,
    started: std::time::Instant,
}

/// 长按判定阈值：达到后弹出删除确认。
const PROVIDER_LONG_PRESS_MS: u128 = 500;

/// 激活一个顶层 Tab → 对应的页面 Mode。
fn activate_tab(tab: ui::screens::chrome::Tab) -> Mode {
    match tab {
        ui::screens::chrome::Tab::Providers => Mode::Provider,
        ui::screens::chrome::Tab::Agent => Mode::Client,
        ui::screens::chrome::Tab::Mcp => Mode::Mcp,
        ui::screens::chrome::Tab::Skills => Mode::Skills,
        ui::screens::chrome::Tab::Sessions => Mode::Session,
    }
}

/// W 键切换 Web 控制台：未运行 → 起服务进 WebNotice 页；运行中 → 停服务回原页。
fn toggle_web(
    home: &Path,
    connection_result: &mut String,
    web_running: &mut bool,
    web_stop: &mut Arc<AtomicBool>,
    web_notice: &mut String,
    prev_mode_for_web: &mut Mode,
    mode: &mut Mode,
) {
    if *web_running {
        web_stop.store(true, Ordering::Relaxed);
        let _ = trivium::state::set_ui_surface(home, UiSurface::Tui);
        *web_running = false;
        *mode = *prev_mode_for_web;
        web_notice.clear();
    } else {
        let persistence_warning = trivium::state::set_ui_surface(home, UiSurface::Web)
            .err()
            .map(|error| format!("\n无法保存下次启动偏好：{error}"))
            .unwrap_or_default();
        let stop = Arc::new(AtomicBool::new(false));
        let port = trivium::web::default_port();
        // 预检端口：被占（常见于残留实例）时不切换、不写偏好，直接告知。
        if std::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], port))).is_err()
        {
            let _ = trivium::state::set_ui_surface(home, UiSurface::Tui);
            *connection_result =
                format!("Web 启动失败：端口 {port} 已被占用（可能有残留实例）。\n\n已保持终端界面；如需强制回 TUI 可运行: spec tui-reset");
            *mode = Mode::ConnectionResult;
            return;
        }
        let thread_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let _ = trivium::web::serve(port, thread_stop);
        });
        *web_stop = stop;
        *web_running = true;
        *prev_mode_for_web = *mode;
        web_notice.clone_from(&format!(
            "Web 控制台已启动：http://127.0.0.1:{port}\n按 W 或 Esc 停止并返回，或在页面点「切回 TUI」{persistence_warning}"
        ));
        *mode = Mode::WebNotice;
    }
}

/// Web 服务线程退出判定：stop 标志被置位（TUI 按 W/Esc，或 Web 页「切回 TUI」）。
fn web_stop_should_return(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Relaxed)
}

/// 会话页（自包含子 TUI 循环）：恢复会话 → 退出前台执行；点 Tab 切换 → 回到对应页；
/// Esc 返回 → 主菜单。
fn run_session_page(
    terminal: &mut tui::Tui,
    mode: &mut Mode,
    tab: &mut ui::screens::chrome::Tab,
    need_redraw: &mut bool,
) {
    match session::run(terminal) {
        session::SessionExit::Resume(session) => {
            let _ = tui::restore();
            run_session_restore(&session);
            std::process::exit(0);
        }
        session::SessionExit::SwitchTab(target) => {
            *tab = target;
            *mode = activate_tab(target);
            *need_redraw = true;
        }
        session::SessionExit::Quit => {
            *mode = Mode::Quit;
            *need_redraw = true;
        }
        session::SessionExit::None => {
            *mode = Mode::Menu;
            *need_redraw = true;
        }
    }
}

/// 供应商页鼠标动作的统一分发（首页内嵌的供应商 Tab 与 Mode::Provider 共用）。
#[allow(clippy::too_many_arguments)]
fn apply_provider_mouse_action(
    action: ui::screens::providers::ProviderMouseAction,
    provider_state: &mut ProviderState,
    provider_mode: &mut ui::screens::providers::ProviderMode,
    provider_add_form: &mut ProviderAddForm,
    provider_preset_idx: &mut usize,
    multi_provider_ids: &mut Vec<String>,
    multi_provider_cursor: &mut usize,
    multi_provider_target: &mut AgentTarget,
    mode: &mut Mode,
) {
    match action {
        ui::screens::providers::ProviderMouseAction::Add => {
            *provider_add_form = ProviderAddForm::default();
            *mode = Mode::ProviderAddForm;
        }
        ui::screens::providers::ProviderMouseAction::Preset => {
            *provider_preset_idx = 0;
            *mode = Mode::ProviderPresetList;
        }
        ui::screens::providers::ProviderMouseAction::Refresh => {
            *provider_state = reload_providers_keep(provider_state);
        }
        ui::screens::providers::ProviderMouseAction::Multi => {
            multi_provider_ids.clear();
            *multi_provider_cursor = 0;
            *multi_provider_target = AgentTarget::Codex;
            *provider_mode = ui::screens::providers::ProviderMode::Multi;
            *mode = Mode::MultiProvider;
        }
        ui::screens::providers::ProviderMouseAction::Detail(idx) => {
            provider_state.selected = idx;
            *mode = Mode::ProviderDetail;
        }
    }
}

/// 把 SyncReport 渲染为多行文本。
fn format_sync_report(report: &trivium::sync::SyncReport) -> String {
    let mut out = format!(
        "新增 {} · 更新 {} · 删除 {} · 未变 {}",
        report.added, report.updated, report.removed, report.unchanged
    );
    if report.details.is_empty() {
        out.push_str("（无差异）");
    } else {
        for detail in &report.details {
            out.push_str(&format!("\n{detail}"));
        }
    }
    out
}

fn reset_ui_surface_to_tui(home: &Path) {
    if let Err(error) = trivium::state::set_ui_surface(home, UiSurface::Tui) {
        eprintln!("恢复 TUI 偏好失败：{error}（可运行 spec tui-reset 重试）");
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("trivium {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let home = config::home();
    if args.first().map(String::as_str) == Some("tui-reset") {
        match trivium::state::set_ui_surface(&home, trivium::state::UiSurface::Tui) {
            Ok(()) => println!("已重置：下次启动进入 TUI"),
            Err(error) => {
                eprintln!("重置失败：{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("serve") {
        if let Err(error) = trivium::runtime::serve(&home) {
            eprintln!("spec 服务启动失败：{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(result) = trivium::cli::run_command(&home, &args) {
        match result {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if matches!(trivium::state::preferred_ui_surface(&home), Ok(UiSurface::Web)) {
        let port = trivium::web::default_port();
        match trivium::web::serve(port, Arc::new(AtomicBool::new(false))) {
            Ok(()) => return,
            Err(error) => {
                // 端口占用等失败必须给出逃生门：回落 TUI 而不是反复锁死。
                // 文案与行为一致：这里已把偏好改回 Tui（reset 见下一行）。
                eprintln!("Web 控制台启动失败：{error}\n已回落终端界面，并把下次启动偏好改回 TUI（如需再试 Web: xcc tui-reset 后手动切回）");
                reset_ui_surface_to_tui(&home);
            }
        }
    }

    let mut terminal = match tui::init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("初始化终端失败：{e}");
            return;
        }
    };

    let mut mode = Mode::Menu;
    let mut last_mode = Mode::Menu;
    let mut agent_idx: usize = 0;
    let mut agent_pending_update: Option<usize> = None;
    let mut agent_pending_setup: bool = false;
    // 启动异步化：status_rows 涉及多轮子进程（claude/opencode 的 --version 会超时数秒），
    // 同步阻塞会让首帧 5-27 秒不可交互（卡顿①）。首帧先以空列表快出，
    // 后台线程完成后异步装填。
    let (agent_status_tx, agent_status_rx) = std::sync::mpsc::channel::<Vec<AgentToolStatus>>();
    let home_for_status = home.clone();
    std::thread::spawn(move || {
        let rows = status_rows(&home_for_status, false);
        let _ = agent_status_tx.send(rows);
    });
    let mut agent_statuses: Vec<AgentToolStatus> = Vec::new();
    let mut agent_status_rx_opt: Option<std::sync::mpsc::Receiver<Vec<AgentToolStatus>>> =
        Some(agent_status_rx);
    let agent_doctor_result = String::new();
    let mut agent_status_message = String::new();
    let mut provider_state = load_providers();
    // 首次使用引导：无供应商配置时落到帮助页（可 Esc 离开）。
    if provider_state.providers.is_empty() {
        mode = Mode::Help;
        last_mode = Mode::Help;
    }
    let mut provider_press: Option<ProviderPress> = None;
    let mut plan_preview = String::new();
    let mut pending_target: Option<AgentTarget> = None;
    let mut pending_provider_ids: Vec<String> = Vec::new();
    let mut multi_provider_ids: Vec<String> = Vec::new();
    let mut multi_provider_cursor: usize = 0;
    let mut multi_provider_target = AgentTarget::Codex;
    let mut connection_result = String::new();
    let mut provider_action_result = String::new();
    let mut provider_add_form = ProviderAddForm::default();
    let mut provider_preset_idx: usize = 0;
    let mut provider_add_preview = String::new();
    let mut provider_edit_form = ProviderAddForm::default();
    let mut provider_edit_preview = String::new();
    // 编辑表单模型拉取：后台任务 + 单调递增 generation（陈旧结果丢弃）。
    let mut provider_edit_form_fetch: Option<FormModelsFetchJob> = None;
    let mut provider_edit_form_fetch_generation: u64 = 0;
    let mut preview_scroll: usize = 0;
    let mut preview_scroll_max: usize = 0;
    // Web 控制台切换状态：web_running 期间停在 WebNotice 页（web_stop 与服务线程共享）。
    let mut web_stop = Arc::new(AtomicBool::new(false));
    let mut web_running = false;
    let mut web_notice = String::new();
    let mut prev_mode_for_web = Mode::Menu;
    let mut opencode_permission =
        trivium::opencode_settings::read_permission(&home).unwrap_or_else(|_| "ask".to_string());
    let mut pending_opencode_permission: Option<String> = None;
    let mut opencode_settings_message = String::new();
    let (mut mcp_servers, mut mcp_error) = load_mcp_servers();
    let mut mcp_selected = 0_usize;
    let mut mcp_preset_selected = 0_usize;
    let mut mcp_message = String::new();
    let mut pending_mcp_args: Vec<String> = Vec::new();
    let mut mcp_form = McpForm::default();
    let (mut skills, mut skill_error) = load_skills();
    let mut skill_selected = 0_usize;
    let mut skill_message = String::new();
    let mut pending_skill_args: Vec<String> = Vec::new();
    let mut skill_form = SkillForm::default();
    let mut connection_rx: Option<Receiver<String>> = None;
    // Agent 页最新版异步查询：事件循环保持响应，完成后再装填。
    let mut pending_agent_latest_rx: Option<Receiver<(Vec<AgentToolStatus>, Duration, String)>> =
        None;
    let mut tab = ui::screens::chrome::Tab::Providers;
    let mut provider_search = ui::widgets::searchbox::SearchBox::new();
    let mut provider_mode = ui::screens::providers::ProviderMode::Single;
    let mut detail_field: Option<usize> = None;
    let mut detail_buf = String::new();
    let mut detail_fetch: Option<Vec<String>> = None;
    let mut detail_fetch_sel: usize = 0;
    let mut detail_agent_tab = AgentTarget::ClaudeCode;
    let mut detail_pick = ui::screens::detail::DetailPickMode::None;
    let mut detail_feedback = String::new();
    // 详情页键盘焦点（区域 0=协议 1=开关 2=操作行 3=模型表格；索引=区内位置）。
    let mut detail_focus_zone: u8 = 0;
    let mut detail_focus_index: usize = 0;
    // 运行时健康缓存（避免每帧 TCP/HTTP）；启动时预热一次，避免首帧误报。
    let mut runtime_ok = trivium::runtime::is_running();
    let mut runtime_check_at = std::time::Instant::now();
    // 模型列表异步拉取：loading 标记 + 结果通道 + 完成后的目标模式。
    let mut detail_fetch_loading = false;
    let mut detail_fetch_rx: Option<Receiver<Result<Vec<String>, String>>> = None;
    let mut detail_pending_pick = ui::screens::detail::DetailPickMode::None;
    let mut detail_multi_sel: BTreeSet<usize> = BTreeSet::new();
    let mut detail_secret_visible = false;

    let mut need_redraw = true;

    loop {
        if mode == Mode::Quit {
            break;
        }

        if mode != last_mode {
            if is_preview_mode(mode) {
                preview_scroll = 0;
                preview_scroll_max = 0;
            }
            if mode == Mode::ProviderDetail {
                // 进入详情页：清掉一切残留（含在途拉取），防止旧 provider
                // 的模型列表在回页后被应用到新选中的 provider。
                detail_fetch_reset(
                    &mut detail_field,
                    &mut detail_buf,
                    &mut detail_fetch,
                    &mut detail_fetch_sel,
                    &mut detail_pick,
                    &mut detail_feedback,
                    &mut detail_fetch_loading,
                    &mut detail_fetch_rx,
                    &mut detail_pending_pick,
                    &mut detail_focus_zone,
                    &mut detail_focus_index,
                );
                detail_multi_sel.clear();
            }
            last_mode = mode;
        }

        // Agent 页最新版异步查询的完成回调。
        if let Some(rx) = pending_agent_latest_rx.take() {
            match rx.try_recv() {
                Ok((statuses, _elapsed, msg)) => {
                    agent_statuses = statuses;
                    agent_status_message = msg;
                    need_redraw = true;
                }
                Err(TryRecvError::Disconnected) => {}
                Err(TryRecvError::Empty) => {
                    pending_agent_latest_rx = Some(rx);
                }
            }
        }

        if let Some(rx) = connection_rx.take() {
            match rx.try_recv() {
                Ok(result) => {
                    connection_result = result;
                    provider_state.health = trivium::state::read_state(&home)
                        .map(|state| state.provider_health)
                        .unwrap_or_default();
                    need_redraw = true;
                }
                Err(TryRecvError::Empty) => {
                    connection_rx = Some(rx);
                }
                Err(TryRecvError::Disconnected) => {
                    connection_result = "连接测试失败：后台任务已断开。".to_string();
                    need_redraw = true;
                }
            }
        }

        // 模型列表异步拉取完成：打开对应选择模式（仅详情页在途时生效，
        // 防止拉取期间已离开详情页 → 陈旧结果写入错误 provider）。
        if let Some(rx) = detail_fetch_rx.take() {
            match rx.try_recv() {
                Ok(Ok(list)) if mode == Mode::ProviderDetail => {
                    detail_fetch_loading = false;
                    let pick = detail_pending_pick;
                    detail_pending_pick = ui::screens::detail::DetailPickMode::None;
                    if pick == ui::screens::detail::DetailPickMode::MultiSel {
                        detail_multi_sel = detail_multi_sel_init(&provider_state, &list);
                    }
                    detail_fetch = Some(list);
                    detail_fetch_sel = detail_fetch_sel
                        .min(detail_fetch.as_ref().map_or(0, Vec::len).saturating_sub(1));
                    detail_pick = pick;
                    detail_feedback.clear();
                    need_redraw = true;
                }
                Ok(Ok(_)) => {
                    // 已离开详情页：丢弃结果，不打开任何选择器。
                    detail_fetch_loading = false;
                    detail_pending_pick = ui::screens::detail::DetailPickMode::None;
                    detail_feedback.clear();
                    need_redraw = true;
                }
                Ok(Err(error)) if mode == Mode::ProviderDetail => {
                    detail_fetch_loading = false;
                    detail_pending_pick = ui::screens::detail::DetailPickMode::None;
                    detail_feedback.clear();
                    provider_action_result = error;
                    mode = Mode::ProviderActionResult;
                    need_redraw = true;
                }
                Ok(Err(_)) => {
                    // 已离开详情页：失败静默丢弃（不劫持当前页面）。
                    detail_fetch_loading = false;
                    detail_pending_pick = ui::screens::detail::DetailPickMode::None;
                    detail_feedback.clear();
                    need_redraw = true;
                }
                Err(TryRecvError::Empty) if mode == Mode::ProviderDetail => {
                    detail_fetch_rx = Some(rx);
                }
                Err(TryRecvError::Empty) => {
                    // 已离开详情页：断开通道（线程 send 失败自然退出），
                    // 彻底杜绝「回页后陈旧结果写错 provider」。
                    detail_fetch_loading = false;
                    detail_pending_pick = ui::screens::detail::DetailPickMode::None;
                    need_redraw = true;
                }
                Err(TryRecvError::Disconnected) if mode == Mode::ProviderDetail => {
                    detail_fetch_loading = false;
                    detail_pending_pick = ui::screens::detail::DetailPickMode::None;
                    detail_feedback.clear();
                    provider_action_result = "模型拉取失败：后台任务已断开。".to_string();
                    mode = Mode::ProviderActionResult;
                    need_redraw = true;
                }
                Err(TryRecvError::Disconnected) => {
                    detail_fetch_loading = false;
                    detail_pending_pick = ui::screens::detail::DetailPickMode::None;
                    detail_feedback.clear();
                    need_redraw = true;
                }
            }
        }

        // 编辑表单模型拉取（每 tick 一次）：只在仍处于编辑表单、generation
        // 仍是当前代、且 provider id 仍与表单一致时应用结果，否则丢弃。
        if let Some(job) = provider_edit_form_fetch.take() {
            match job.receiver.try_recv() {
                Ok(result) => {
                    if mode == Mode::ProviderEditForm
                        && job.generation == provider_edit_form_fetch_generation
                        && job.provider_id == provider_edit_form.id.trim()
                    {
                        apply_models_fetch_result(&mut provider_edit_form, result);
                    }
                    need_redraw = true;
                }
                Err(TryRecvError::Empty) => {
                    provider_edit_form_fetch = Some(job);
                }
                Err(TryRecvError::Disconnected) => {
                    // worker 异常退出：无结果可应用，直接丢弃任务。
                    need_redraw = true;
                }
            }
        }

        if need_redraw {
            match mode {
                Mode::Menu => match tab {
                    ui::screens::chrome::Tab::Providers => {
                        ui::screens::chrome::render_page(&mut terminal, tab, |f, area| {
                            ui::screens::providers::render_providers_page(
                                f,
                                area,
                                &provider_state.providers,
                                provider_state.selected,
                                &provider_state.current,
                                &provider_state.health,
                                provider_state.error.as_deref(),
                                provider_state.sync_notice.as_deref(),
                                &provider_search,
                                provider_press.as_ref().map(|p| {
                                    let progress = (p.started.elapsed().as_millis() * 100
                                        / PROVIDER_LONG_PRESS_MS)
                                        .min(100)
                                        as u8;
                                    ui::screens::providers::ProviderPressView {
                                        index: p.index,
                                        progress,
                                    }
                                }),
                            )
                        })
                        .ok();
                    }
                    ui::screens::chrome::Tab::Agent => {
                        ui::screens::chrome::render_page(&mut terminal, tab, |f, area| {
                            ui::screens::agents::draw_agent_targets_frame(
                                f,
                                area,
                                agent_idx,
                                &agent_statuses,
                                &agent_status_message,
                                agent_pending_update,
                                agent_pending_setup,
                            )
                        })
                        .ok();
                    }
                    ui::screens::chrome::Tab::Mcp => {
                        ui::screens::chrome::render_page(&mut terminal, tab, |f, area| {
                            ui::screens::mcp::draw_mcp_frame(
                                f,
                                area,
                                &mcp_servers,
                                mcp_selected,
                                mcp_error.as_deref(),
                                &mcp_message,
                            )
                        })
                        .ok();
                    }
                    ui::screens::chrome::Tab::Skills => {
                        ui::screens::chrome::render_page(&mut terminal, tab, |f, area| {
                            ui::screens::skills::draw_skills_frame(
                                f,
                                area,
                                &skills,
                                skill_selected,
                                skill_error.as_deref(),
                                &skill_message,
                            )
                        })
                        .ok();
                    }
                    ui::screens::chrome::Tab::Sessions => {
                        run_session_page(&mut terminal, &mut mode, &mut tab, &mut need_redraw);
                    }
                },
                Mode::Provider => {
                    ui::screens::chrome::render_page(
                        &mut terminal,
                        ui::screens::chrome::Tab::Providers,
                        |f, area| {
                            ui::screens::providers::render_providers_page(
                                f,
                                area,
                                &provider_state.providers,
                                provider_state.selected,
                                &provider_state.current,
                                &provider_state.health,
                                provider_state.error.as_deref(),
                                provider_state.sync_notice.as_deref(),
                                &provider_search,
                                provider_press.as_ref().map(|p| {
                                    let progress = (p.started.elapsed().as_millis() * 100
                                        / PROVIDER_LONG_PRESS_MS)
                                        .min(100)
                                        as u8;
                                    ui::screens::providers::ProviderPressView {
                                        index: p.index,
                                        progress,
                                    }
                                }),
                            );
                        },
                    )
                    .ok();
                }
                Mode::ProviderDetail => {
                    let (route_enabled, route_present) = provider_state
                        .providers
                        .get(provider_state.selected)
                        .map(|provider| detail_route_status(&home, detail_agent_tab, &provider.id))
                        .unwrap_or((false, false));
                    ui::screens::detail::render_provider_detail_v2(
                        &mut terminal,
                        provider_state.providers.get(provider_state.selected),
                        &provider_state.health,
                        provider_mode,
                        runtime_ok,
                        &ui::screens::detail::DetailView {
                            field: detail_field,
                            buf: &detail_buf,
                            fetch: detail_fetch.as_deref(),
                            fetch_sel: detail_fetch_sel,
                            agent_tab: detail_agent_tab,
                            pick: detail_pick,
                            multi_sel: &detail_multi_sel,
                            secret_visible: detail_secret_visible,
                            feedback: &detail_feedback,
                            fetch_loading: detail_fetch_loading,
                            focus_zone: if detail_field.is_none() && detail_fetch.is_none() {
                                Some(detail_focus_zone)
                            } else {
                                None
                            },
                            focus_index: detail_focus_index,
                        },
                        &ui::screens::detail::DetailSwitches {
                            permission: match detail_agent_tab {
                                AgentTarget::ClaudeCode => read_bypass_permissions(&home),
                                AgentTarget::OpenCode => read_opencode_permission_allow(&home),
                                _ => false,
                            },
                            terminal_title: detail_agent_tab == AgentTarget::ClaudeCode
                                && read_terminal_title_disabled(&home),
                            route: route_enabled && route_present,
                        },
                    )
                    .ok();
                }
                Mode::ProviderAddForm => {
                    ui::screens::forms::render_provider_add_form(
                        &mut terminal,
                        &provider_add_fields(&provider_add_form),
                        provider_add_form.field,
                        provider_add_form.error.as_deref(),
                    )
                    .ok();
                }
                Mode::ProviderAddConfirm => {
                    ui::screens::common::render_confirm(
                        &mut terminal,
                        "确认添加供应商",
                        &provider_add_preview,
                    )
                    .ok();
                }
                Mode::ProviderEditForm => {
                    ui::screens::editor::render_provider_editor(
                        &mut terminal,
                        provider_edit_form.page,
                        provider_edit_form.field,
                        &provider_edit_fields(&provider_edit_form),
                        provider_edit_form.error.as_deref(),
                        &ui::screens::editor::ProviderEditorModel {
                            rows: &provider_edit_form.model_rows,
                            cell: provider_edit_form.model_cell,
                            fetch_msg: provider_edit_form.model_fetch_msg.as_deref(),
                            bypass: provider_edit_form.bypass_permissions,
                        },
                    )
                    .ok();
                }
                Mode::ProviderEditConfirm => {
                    ui::screens::common::render_confirm(
                        &mut terminal,
                        "确认更新供应商",
                        &provider_edit_preview,
                    )
                    .ok();
                }
                Mode::ProviderDeleteConfirm => {
                    ui::screens::common::render_confirm(
                        &mut terminal,
                        "确认删除供应商",
                        &provider_delete_confirm_message(&provider_state),
                    )
                    .ok();
                }
                Mode::ProviderActionResult => {
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "供应商操作结果",
                        &provider_action_result,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::ProviderPresetList => {
                    ui::screens::forms::render_provider_preset_list(
                        &mut terminal,
                        provider_preset_idx,
                    )
                    .ok();
                }
                Mode::MultiProvider => {
                    ui::screens::providers::render_multi_provider(
                        &mut terminal,
                        &provider_state.providers,
                        &multi_provider_ids,
                        multi_provider_cursor,
                        multi_provider_target,
                    )
                    .ok();
                }
                Mode::PlanPreview => {
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "预览变更",
                        &plan_preview,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::ApplyConfirm => {
                    // A1：变更计划内嵌进确认屏（不再要求先看独立预览页）。
                    let message = format!(
                        "{}\n\n────────────\n{}\n回车应用 · Esc 返回预览",
                        plan_preview,
                        confirm_message(&provider_state, pending_target, &pending_provider_ids)
                    );
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "确认应用",
                        &message,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::ConnectionResult => {
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "供应商结果",
                        &connection_result,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::Client => {
                    ui::screens::chrome::render_page(
                        &mut terminal,
                        ui::screens::chrome::Tab::Agent,
                        |f, area| {
                            ui::screens::agents::draw_agent_targets_frame(
                                f,
                                area,
                                agent_idx,
                                &agent_statuses,
                                &agent_status_message,
                                agent_pending_update,
                                agent_pending_setup,
                            )
                        },
                    )
                    .ok();
                }
                Mode::AgentDoctor => {
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "Agent 环境诊断",
                        &agent_doctor_result,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::Session => {
                    run_session_page(&mut terminal, &mut mode, &mut tab, &mut need_redraw);
                }
                Mode::OpenCodeSettings => {
                    ui::screens::extra::render_opencode_settings(
                        &mut terminal,
                        &opencode_permission,
                        (!opencode_settings_message.is_empty())
                            .then_some(opencode_settings_message.as_str()),
                    )
                    .ok();
                }
                Mode::Mcp => {
                    ui::screens::chrome::render_page(
                        &mut terminal,
                        ui::screens::chrome::Tab::Mcp,
                        |f, area| {
                            ui::screens::mcp::draw_mcp_frame(
                                f,
                                area,
                                &mcp_servers,
                                mcp_selected,
                                mcp_error.as_deref(),
                                &mcp_message,
                            )
                        },
                    )
                    .ok();
                }
                Mode::McpPresets => {
                    ui::screens::mcp::render_mcp_presets(&mut terminal, mcp_preset_selected).ok();
                }
                Mode::McpForm => {
                    ui::screens::common::render_text_form(
                        &mut terminal,
                        mcp_form_title(&mcp_form),
                        &mcp_form_fields(&mcp_form),
                        mcp_form.field,
                        mcp_form.error.as_deref(),
                    )
                    .ok();
                }
                Mode::McpFormConfirm => {
                    // A1：diff/命令内嵌进确认屏，省掉独立预览一跳。
                    let message =
                        format!("{}\n\n────────────\n回车应用 · Esc 返回编辑", mcp_message);
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "确认写入 MCP",
                        &message,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::McpConfirm => {
                    // A1：变更内容内嵌确认屏。
                    let message = format!(
                        "{}\n\n────────────\n将更新 spec MCP store 与对应端配置。\n回车应用 · Esc 取消",
                        mcp_message
                    );
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "确认 MCP 修改",
                        &message,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::Skills => {
                    ui::screens::chrome::render_page(
                        &mut terminal,
                        ui::screens::chrome::Tab::Skills,
                        |f, area| {
                            ui::screens::skills::draw_skills_frame(
                                f,
                                area,
                                &skills,
                                skill_selected,
                                skill_error.as_deref(),
                                &skill_message,
                            )
                        },
                    )
                    .ok();
                }
                Mode::SkillForm => {
                    ui::screens::common::render_text_form(
                        &mut terminal,
                        skill_form_title(&skill_form),
                        &skill_form_fields(&skill_form),
                        skill_form.field,
                        skill_form.error.as_deref(),
                    )
                    .ok();
                }
                Mode::SkillFormConfirm => {
                    // A1：diff/命令内嵌进确认屏。
                    let message = format!(
                        "{}\n\n────────────\n安装会复制内容、写 store，失败时回滚。\n回车应用 · Esc 返回编辑",
                        skill_message
                    );
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "确认安装 Skills",
                        &message,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::SkillConfirm => {
                    // A1：变更内容内嵌确认屏。
                    let message = format!(
                        "{}\n\n────────────\n只创建/删除有所有权证明的投影。\n回车应用 · Esc 取消",
                        skill_message
                    );
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "确认 Skills 修改",
                        &message,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::WebNotice => {
                    preview_scroll_max = ui::screens::common::render_placeholder_scroll(
                        &mut terminal,
                        "Web 控制台",
                        &web_notice,
                        preview_scroll,
                    )
                    .unwrap_or(0);
                }
                Mode::Help => {
                    ui::screens::extra::render_help(&mut terminal).ok();
                }
                Mode::Quit => {}
            }
            need_redraw = false;
        }

        // 运行时健康检查缓存：is_running() 内部是 TCP+HTTP，只允许 1s 一次，
        // 避免每次重绘（如模型列表箭头移动）都做一次网络请求造成卡顿。
        if runtime_check_at.elapsed() >= Duration::from_millis(1000) {
            runtime_ok = trivium::runtime::is_running();
            runtime_check_at = std::time::Instant::now();
        }

        // 启动时的 status_rows 异步填充：首帧秒出，数据到达后补渲染。
        if let Some(rx) = agent_status_rx_opt.as_ref() {
            match rx.try_recv() {
                Ok(rows) => {
                    agent_statuses = rows;
                    agent_status_rx_opt = None;
                    need_redraw = true;
                }
                Err(TryRecvError::Disconnected) => {
                    agent_status_rx_opt = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }

        let mut ev = match tui::poll_event(Duration::from_millis(16)) {
            Some(ev) => ev,
            None => continue,
        };

        let screen_area = terminal
            .size()
            .map(|size| Rect::new(0, 0, size.width, size.height))
            .unwrap_or_else(|_| Rect::new(0, 0, 80, 24));

        // 拇指坞：页面底部动作条命中 → 合成对应按键（复用既有键盘分支）。
        if let tui::AppEvent::Mouse(m) = &ev {
            let ctx = match mode {
                Mode::Menu if tab == ui::screens::chrome::Tab::Providers => {
                    Some(ui::widgets::dock::DockCtx::List)
                }
                Mode::ProviderDetail => Some(ui::widgets::dock::DockCtx::Detail),
                Mode::ProviderEditForm => Some(ui::widgets::dock::DockCtx::Editor),
                _ => None,
            };
            if let Some(ctx) = ctx {
                // 文本输入态不拦截（避免字母被按钮吞用）
                let typing =
                    matches!(ctx, ui::widgets::dock::DockCtx::Detail) && detail_field.is_some();
                if !typing {
                    if let Some(key) = ui::widgets::dock::hit(ctx, screen_area, m) {
                        ev = tui::AppEvent::Key(crossterm::event::KeyEvent::from(key));
                    }
                }
            }
        }

        if let tui::AppEvent::Key(k) = &ev {
            // W：Web 控制台起/停（仅顶层页；搜索框/表单输入不劫持，会话页自有循环）。
            let top_page = matches!(
                mode,
                Mode::Menu | Mode::Provider | Mode::Client | Mode::Mcp | Mode::Skills
            );
            let search_focused = mode == Mode::Menu
                && tab == ui::screens::chrome::Tab::Providers
                && provider_search.focused;
            if matches!(k.code, KeyCode::Char('w') | KeyCode::Char('W'))
                && (top_page || mode == Mode::WebNotice)
                && !search_focused
            {
                toggle_web(
                    &home,
                    &mut connection_result,
                    &mut web_running,
                    &mut web_stop,
                    &mut web_notice,
                    &mut prev_mode_for_web,
                    &mut mode,
                );
                need_redraw = true;
                continue;
            }
        }

        if is_preview_mode(mode) {
            if let Some(delta) = preview_scroll_delta(&ev) {
                let next = (preview_scroll as isize + delta).clamp(0, preview_scroll_max as isize);
                if next as usize != preview_scroll {
                    preview_scroll = next as usize;
                    need_redraw = true;
                }
                continue;
            }
        }

        // Every top-level page renders the same tab bar. Route its clicks
        // before mode-specific handlers so tabs are always interactive.
        if matches!(
            mode,
            Mode::Menu | Mode::Provider | Mode::Client | Mode::Mcp | Mode::Skills
        ) {
            if let tui::AppEvent::Mouse(m) = &ev {
                if let Some(new_tab) = ui::screens::chrome::tab_hit_test(screen_area, m) {
                    tab = new_tab;
                    mode = activate_tab(tab);
                    need_redraw = true;
                    continue;
                }
            }
        }

        // 全局 q 退出（E1）：非文本输入焦点时，任何页面按 q 直接退出。
        // 白名单 = 正在打字的所有场景（搜索聚焦 / 各表单 / 详情输入框）。
        let text_input_focused = provider_search.focused
            || matches!(
                mode,
                Mode::ProviderAddForm
                    | Mode::ProviderEditForm
                    | Mode::McpForm
                    | Mode::SkillForm
                    | Mode::OpenCodeSettings
            )
            || (mode == Mode::ProviderDetail && detail_field.is_some());
        if let tui::AppEvent::Key(k) = &ev {
            if k.code == KeyCode::Char('q') && !text_input_focused {
                mode = Mode::Quit;
                need_redraw = true;
                continue;
            }
            if k.code == KeyCode::Char('?') && !text_input_focused {
                mode = Mode::Help;
                need_redraw = true;
                continue;
            }
        }

        match mode {
            Mode::Menu => match ev {
                tui::AppEvent::Key(k) => {
                    if tab == ui::screens::chrome::Tab::Providers
                        && provider_search.focused
                        && matches!(k.code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Esc)
                    {
                        if k.code == KeyCode::Esc {
                            // 非空=清除（与框内提示一致）；空=失焦折叠。
                            if provider_search.is_empty() {
                                provider_search.focused = false;
                            } else {
                                provider_search.clear();
                            }
                            need_redraw = true;
                        } else if provider_search.handle_key(&k) {
                            // 过滤条件变化时，把选中项钳制到可见项，避免后续操作
                            // 作用于被过滤掉的隐藏项（BUG-2）。
                            let filtered = ui::screens::providers::filtered_provider_indices(
                                &provider_state.providers,
                                &provider_search,
                            );
                            if !filtered.is_empty() && !filtered.contains(&provider_state.selected)
                            {
                                provider_state.selected =
                                    ui::screens::chrome::clamp_selection_to_filter(
                                        &filtered,
                                        provider_state.selected,
                                    );
                            }
                            need_redraw = true;
                        }
                    } else {
                        match k.code {
                            KeyCode::Char('q') => mode = Mode::Quit,
                            KeyCode::Esc => mode = Mode::Quit,
                            key @ (KeyCode::Left
                            | KeyCode::Right
                            | KeyCode::Tab
                            | KeyCode::BackTab) => {
                                tab = ui::screens::chrome::move_tab(tab, key);
                                need_redraw = true;
                            }
                            KeyCode::Enter => {
                                mode = activate_tab(tab);
                                need_redraw = true;
                            }
                            // 主菜单 Providers 页支持上/下移动选中（之前被吞，BUG-3）。
                            KeyCode::Up if tab == ui::screens::chrome::Tab::Providers => {
                                let filtered = ui::screens::providers::filtered_provider_indices(
                                    &provider_state.providers,
                                    &provider_search,
                                );
                                provider_state.selected =
                                    ui::screens::chrome::move_filtered_selection(
                                        &filtered,
                                        provider_state.selected,
                                        -1,
                                    );
                                need_redraw = true;
                            }
                            KeyCode::Down if tab == ui::screens::chrome::Tab::Providers => {
                                let filtered = ui::screens::providers::filtered_provider_indices(
                                    &provider_state.providers,
                                    &provider_search,
                                );
                                provider_state.selected =
                                    ui::screens::chrome::move_filtered_selection(
                                        &filtered,
                                        provider_state.selected,
                                        1,
                                    );
                                need_redraw = true;
                            }
                            KeyCode::Char(c @ '1'..='5') => {
                                tab = ui::screens::chrome::TABS[(c as usize) - ('1' as usize)].0;
                                mode = activate_tab(tab);
                                need_redraw = true;
                            }
                            KeyCode::Char('/') if tab == ui::screens::chrome::Tab::Providers => {
                                if provider_search.focused {
                                    provider_search.clear();
                                } else {
                                    provider_search.focused = true;
                                }
                                need_redraw = true;
                            }
                            _ => {}
                        }
                    }
                }
                tui::AppEvent::Mouse(m) => {
                    let size = terminal
                        .size()
                        .unwrap_or(ratatui::layout::Size::new(80, 24));
                    let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                    if let Some(new_tab) = ui::screens::chrome::tab_hit_test(area, &m) {
                        if new_tab == tab {
                            mode = activate_tab(tab);
                        } else {
                            tab = new_tab;
                        }
                        need_redraw = true;
                    } else if tab == ui::screens::chrome::Tab::Providers {
                        let content_area = Rect::new(
                            area.x,
                            area.y + ui::screens::chrome::TAB_BAR_HEIGHT,
                            area.width,
                            area.height
                                .saturating_sub(ui::screens::chrome::TAB_BAR_HEIGHT),
                        );
                        if ui::widgets::searchbox::search_box_click(
                            ui::widgets::searchbox::search_slot_rect(
                                content_area,
                                &provider_search,
                            ),
                            &m,
                        ) {
                            provider_search.focused = true;
                            need_redraw = true;
                        } else if let Some(action) =
                            ui::screens::providers::providers_page_mouse_action(
                                content_area,
                                provider_state.selected,
                                &provider_search,
                                &provider_state.providers,
                                &m,
                            )
                        {
                            apply_provider_mouse_action(
                                action,
                                &mut provider_state,
                                &mut provider_mode,
                                &mut provider_add_form,
                                &mut provider_preset_idx,
                                &mut multi_provider_ids,
                                &mut multi_provider_cursor,
                                &mut multi_provider_target,
                                &mut mode,
                            );
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                tui::AppEvent::Tick => {
                    if tab == ui::screens::chrome::Tab::Providers {
                        if let Some(press) = provider_press.as_ref() {
                            if press.started.elapsed().as_millis() >= PROVIDER_LONG_PRESS_MS {
                                provider_state.selected = press.index;
                                provider_press = None;
                                mode = Mode::ProviderDeleteConfirm;
                            }
                            need_redraw = true;
                        }
                    }
                }
            },
            Mode::Provider => match ev {
                tui::AppEvent::Key(k) => {
                    if provider_search.focused
                        && matches!(k.code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Esc)
                    {
                        if k.code == KeyCode::Esc {
                            // 非空=清除（与框内提示一致）；空=失焦折叠。
                            if provider_search.is_empty() {
                                provider_search.focused = false;
                            } else {
                                provider_search.clear();
                            }
                            need_redraw = true;
                        } else if provider_search.handle_key(&k) {
                            // 过滤变化后把选中项钳制到可见项（BUG-2）。
                            let filtered = ui::screens::providers::filtered_provider_indices(
                                &provider_state.providers,
                                &provider_search,
                            );
                            if !filtered.is_empty() && !filtered.contains(&provider_state.selected)
                            {
                                provider_state.selected =
                                    ui::screens::chrome::clamp_selection_to_filter(
                                        &filtered,
                                        provider_state.selected,
                                    );
                            }
                            need_redraw = true;
                        }
                    } else {
                        match k.code {
                            KeyCode::Esc => {
                                mode = Mode::Menu;
                                need_redraw = true;
                            }
                            KeyCode::Char('/') => {
                                if provider_search.focused {
                                    provider_search.clear();
                                } else {
                                    provider_search.focused = true;
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('r') => {
                                provider_state = reload_providers_keep(&provider_state);
                                need_redraw = true;
                            }
                            KeyCode::Char('a') => {
                                provider_add_form = ProviderAddForm::default();
                                mode = Mode::ProviderAddForm;
                                need_redraw = true;
                            }
                            KeyCode::Char('p') => {
                                provider_preset_idx = 0;
                                mode = Mode::ProviderPresetList;
                                need_redraw = true;
                            }
                            KeyCode::Char('g') => {
                                multi_provider_ids.clear();
                                multi_provider_cursor = 0;
                                multi_provider_target = AgentTarget::Codex;
                                provider_mode = ui::screens::providers::ProviderMode::Multi;
                                mode = Mode::MultiProvider;
                                need_redraw = true;
                            }
                            KeyCode::Up => {
                                let filtered = ui::screens::providers::filtered_provider_indices(
                                    &provider_state.providers,
                                    &provider_search,
                                );
                                provider_state.selected =
                                    ui::screens::chrome::move_filtered_selection(
                                        &filtered,
                                        provider_state.selected,
                                        -1,
                                    );
                                need_redraw = true;
                            }
                            KeyCode::Down => {
                                let filtered = ui::screens::providers::filtered_provider_indices(
                                    &provider_state.providers,
                                    &provider_search,
                                );
                                provider_state.selected =
                                    ui::screens::chrome::move_filtered_selection(
                                        &filtered,
                                        provider_state.selected,
                                        1,
                                    );
                                need_redraw = true;
                            }
                            KeyCode::Enter if !provider_state.providers.is_empty() => {
                                mode = Mode::ProviderDetail;
                                need_redraw = true;
                            }
                            KeyCode::Char('e') if !provider_state.providers.is_empty() => {
                                if let Some(form) =
                                    edit_form_from_selected_provider(&provider_state)
                                {
                                    provider_edit_form = form;
                                    mode = Mode::ProviderEditForm;
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('t') if !provider_state.providers.is_empty() => {
                                connection_result = provider_test_start_message();
                                connection_rx = start_provider_test(&provider_state);
                                mode = Mode::ConnectionResult;
                                need_redraw = true;
                            }
                            KeyCode::Char('m') if !provider_state.providers.is_empty() => {
                                connection_result = provider_models_start_message();
                                connection_rx = start_provider_models_fetch(&provider_state);
                                mode = Mode::ConnectionResult;
                                need_redraw = true;
                            }
                            KeyCode::Char('1') if !provider_state.providers.is_empty() => {
                                pending_target = Some(AgentTarget::OpenCode);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    AgentTarget::OpenCode,
                                    &pending_provider_ids,
                                );
                                mode = Mode::PlanPreview;
                                need_redraw = true;
                            }
                            KeyCode::Char('2') if !provider_state.providers.is_empty() => {
                                pending_target = Some(AgentTarget::ClaudeCode);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    AgentTarget::ClaudeCode,
                                    &pending_provider_ids,
                                );
                                mode = Mode::PlanPreview;
                                need_redraw = true;
                            }
                            KeyCode::Char('3') if !provider_state.providers.is_empty() => {
                                pending_target = Some(AgentTarget::Codex);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    AgentTarget::Codex,
                                    &pending_provider_ids,
                                );
                                mode = Mode::PlanPreview;
                                need_redraw = true;
                            }
                            _ => {}
                        }
                    }
                }
                tui::AppEvent::Mouse(m) => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let size = terminal
                            .size()
                            .unwrap_or(ratatui::layout::Size::new(80, 24));
                        let area = Rect::new(0, 0, size.width, size.height);
                        let content_area = Rect::new(
                            area.x,
                            area.y + ui::screens::chrome::TAB_BAR_HEIGHT,
                            area.width,
                            area.height
                                .saturating_sub(ui::screens::chrome::TAB_BAR_HEIGHT),
                        );
                        if ui::widgets::searchbox::search_box_click(
                            ui::widgets::searchbox::search_slot_rect(
                                content_area,
                                &provider_search,
                            ),
                            &m,
                        ) {
                            provider_search.focused = true;
                            need_redraw = true;
                        } else if let Some(index) = ui::screens::providers::providers_page_row_at(
                            content_area,
                            provider_state.selected,
                            &provider_search,
                            &provider_state.providers,
                            &m,
                        ) {
                            provider_press = Some(ProviderPress {
                                index,
                                started: std::time::Instant::now(),
                            });
                            need_redraw = true;
                        }
                    }
                    MouseEventKind::Drag(_) | MouseEventKind::Moved => {
                        if provider_press.take().is_some() {
                            need_redraw = true;
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        provider_press = None;
                        let size = terminal
                            .size()
                            .unwrap_or(ratatui::layout::Size::new(80, 24));
                        let area = Rect::new(0, 0, size.width, size.height);
                        let content_area = Rect::new(
                            area.x,
                            area.y + ui::screens::chrome::TAB_BAR_HEIGHT,
                            area.width,
                            area.height
                                .saturating_sub(ui::screens::chrome::TAB_BAR_HEIGHT),
                        );
                        if let Some(action) = ui::screens::providers::providers_page_mouse_action(
                            content_area,
                            provider_state.selected,
                            &provider_search,
                            &provider_state.providers,
                            &m,
                        ) {
                            apply_provider_mouse_action(
                                action,
                                &mut provider_state,
                                &mut provider_mode,
                                &mut provider_add_form,
                                &mut provider_preset_idx,
                                &mut multi_provider_ids,
                                &mut multi_provider_cursor,
                                &mut multi_provider_target,
                                &mut mode,
                            );
                            need_redraw = true;
                        }
                    }
                    _ => {}
                },
                tui::AppEvent::Resize => need_redraw = true,
                tui::AppEvent::Tick => {
                    if let Some(press) = provider_press.as_ref() {
                        if press.started.elapsed().as_millis() >= PROVIDER_LONG_PRESS_MS {
                            provider_state.selected = press.index;
                            provider_press = None;
                            mode = Mode::ProviderDeleteConfirm;
                            need_redraw = true;
                        } else {
                            need_redraw = true;
                        }
                    }
                }
            },
            Mode::ProviderAddForm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Provider;
                        need_redraw = true;
                    }
                    KeyCode::Enter => {
                        if provider_add_form.field < 5 {
                            provider_add_form.field += 1;
                        } else {
                            match build_provider_add_preview(&provider_add_form) {
                                Ok(preview) => {
                                    provider_add_form.error = None;
                                    provider_add_preview = preview;
                                    mode = Mode::ProviderAddConfirm;
                                }
                                Err(error) => provider_add_form.error = Some(error),
                            }
                        }
                        need_redraw = true;
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        provider_add_form.field = (provider_add_form.field + 1).min(5);
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        provider_add_form.field = provider_add_form.field.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Left if provider_add_form.field == 2 => {
                        provider_add_form.kind_index =
                            provider_add_form.kind_index.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Right if provider_add_form.field == 2 => {
                        provider_add_form.kind_index = (provider_add_form.kind_index + 1).min(2);
                        need_redraw = true;
                    }
                    KeyCode::Backspace => {
                        provider_add_form_pop(&mut provider_add_form);
                        need_redraw = true;
                    }
                    KeyCode::Char(c) => {
                        provider_add_form_push(&mut provider_add_form, c);
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(action) =
                        ui::screens::common::text_form_mouse_action(screen_area, 6, &m)
                    {
                        match action {
                            ui::screens::forms::TextFormAction::Cancel => {
                                mode = Mode::Provider;
                                need_redraw = true;
                            }
                            ui::screens::forms::TextFormAction::Preview => {
                                match build_provider_add_preview(&provider_add_form) {
                                    Ok(preview) => {
                                        provider_add_form.error = None;
                                        provider_add_preview = preview;
                                        mode = Mode::ProviderAddConfirm;
                                    }
                                    Err(error) => provider_add_form.error = Some(error),
                                }
                                need_redraw = true;
                            }
                            ui::screens::forms::TextFormAction::Field(index) => {
                                select_add_field(&mut provider_add_form, index);
                                need_redraw = true;
                            }
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ProviderAddConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        provider_action_result = add_provider_from_form(&provider_add_form);
                        provider_state = reload_providers_keep(&provider_state);
                        mode = Mode::ProviderActionResult;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        mode = Mode::ProviderAddForm;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            provider_action_result = add_provider_from_form(&provider_add_form);
                            provider_state = reload_providers_keep(&provider_state);
                            mode = Mode::ProviderActionResult;
                            need_redraw = true;
                        } else {
                            mode = Mode::ProviderAddForm;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ProviderEditForm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::ProviderDetail;
                        need_redraw = true;
                    }
                    KeyCode::Enter => {
                        match build_provider_update_preview(&provider_edit_form) {
                            Ok(preview) => {
                                provider_edit_form.error = None;
                                provider_edit_preview = preview;
                                mode = Mode::ProviderEditConfirm;
                            }
                            Err(error) => provider_edit_form.error = Some(error),
                        }
                        need_redraw = true;
                    }
                    KeyCode::Tab | KeyCode::Down if provider_edit_form.page == 1 => {
                        advance_model_focus(&mut provider_edit_form, true);
                        need_redraw = true;
                    }
                    KeyCode::Up if provider_edit_form.page == 1 => {
                        advance_model_focus(&mut provider_edit_form, false);
                        need_redraw = true;
                    }
                    KeyCode::Left if provider_edit_form.page == 1 => {
                        model_switch_column(&mut provider_edit_form, false);
                        need_redraw = true;
                    }
                    KeyCode::Right if provider_edit_form.page == 1 => {
                        model_switch_column(&mut provider_edit_form, true);
                        need_redraw = true;
                    }
                    KeyCode::Backspace if provider_edit_form.page == 1 => {
                        model_cell_pop(&mut provider_edit_form);
                        need_redraw = true;
                    }
                    KeyCode::Char(c) if provider_edit_form.page == 1 => {
                        model_cell_push(&mut provider_edit_form, c);
                        need_redraw = true;
                    }
                    KeyCode::Insert if provider_edit_form.page == 1 => {
                        model_add_row(&mut provider_edit_form);
                        need_redraw = true;
                    }
                    KeyCode::Delete if provider_edit_form.page == 1 => {
                        model_delete_focused(&mut provider_edit_form);
                        need_redraw = true;
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        select_adjacent_edit_field(&mut provider_edit_form, true);
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        select_adjacent_edit_field(&mut provider_edit_form, false);
                        need_redraw = true;
                    }
                    KeyCode::Left => {
                        adjust_provider_edit_field(&mut provider_edit_form, false);
                        need_redraw = true;
                    }
                    KeyCode::Right => {
                        adjust_provider_edit_field(&mut provider_edit_form, true);
                        need_redraw = true;
                    }
                    KeyCode::Backspace => {
                        provider_edit_form_pop(&mut provider_edit_form);
                        need_redraw = true;
                    }
                    KeyCode::Char(c) => {
                        provider_edit_form_push(&mut provider_edit_form, c);
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(action) = ui::screens::editor::provider_editor_mouse_action(
                        screen_area,
                        provider_edit_form.page,
                        provider_edit_fields(&provider_edit_form).len(),
                        provider_edit_form.field,
                        &m,
                        &ui::screens::editor::ProviderEditorModel {
                            rows: &provider_edit_form.model_rows,
                            cell: provider_edit_form.model_cell,
                            fetch_msg: provider_edit_form.model_fetch_msg.as_deref(),
                            bypass: provider_edit_form.bypass_permissions,
                        },
                    ) {
                        match action {
                            ui::screens::editor::ProviderEditorAction::Cancel => {
                                mode = Mode::ProviderDetail
                            }
                            ui::screens::editor::ProviderEditorAction::Preview => {
                                match build_provider_update_preview(&provider_edit_form) {
                                    Ok(preview) => {
                                        provider_edit_form.error = None;
                                        provider_edit_preview = preview;
                                        mode = Mode::ProviderEditConfirm;
                                    }
                                    Err(error) => provider_edit_form.error = Some(error),
                                }
                            }
                            ui::screens::editor::ProviderEditorAction::Page(page) => {
                                provider_edit_form.page = page;
                                provider_edit_form.field = edit_page_fields(page)[0];
                                provider_edit_form.model_cell = None;
                            }
                            ui::screens::editor::ProviderEditorAction::Field(field) => {
                                focus_edit_field(&mut provider_edit_form, field);
                            }
                            ui::screens::editor::ProviderEditorAction::Adjust(field, increase) => {
                                focus_edit_field(&mut provider_edit_form, field);
                                adjust_provider_edit_field(&mut provider_edit_form, increase);
                            }
                            ui::screens::editor::ProviderEditorAction::ModelCell(row, column) => {
                                focus_model_cell(&mut provider_edit_form, row, column);
                            }
                            ui::screens::editor::ProviderEditorAction::AddModelRow => {
                                model_add_row(&mut provider_edit_form);
                            }
                            ui::screens::editor::ProviderEditorAction::DeleteModelRow => {
                                model_delete_focused(&mut provider_edit_form);
                            }
                            ui::screens::editor::ProviderEditorAction::FetchModels => {
                                provider_edit_form_fetch_generation =
                                    provider_edit_form_fetch_generation.wrapping_add(1);
                                match start_form_models_fetch(
                                    provider_edit_form.id.clone(),
                                    provider_edit_form_fetch_generation,
                                ) {
                                    Some(job) => provider_edit_form_fetch = Some(job),
                                    None => {
                                        provider_edit_form.model_fetch_msg =
                                            Some("先填写 provider id 再获取模型".to_string());
                                    }
                                }
                            }
                            ui::screens::editor::ProviderEditorAction::ToggleBypass => {
                                let enable = !provider_edit_form.bypass_permissions;
                                match set_bypass_permissions(&config::home(), enable) {
                                    Ok(message) => {
                                        provider_edit_form.bypass_permissions = enable;
                                        provider_edit_form.model_fetch_msg = Some(message);
                                    }
                                    Err(error) => {
                                        provider_edit_form.model_fetch_msg = Some(error);
                                    }
                                }
                            }
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ProviderEditConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        provider_action_result = update_provider_from_form(&provider_edit_form);
                        provider_state = reload_providers_keep(&provider_state);
                        mode = Mode::ProviderActionResult;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        mode = Mode::ProviderEditForm;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            provider_action_result = update_provider_from_form(&provider_edit_form);
                            provider_state = reload_providers_keep(&provider_state);
                            mode = Mode::ProviderActionResult;
                            need_redraw = true;
                        } else {
                            mode = Mode::ProviderEditForm;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ProviderDetail => match ev {
                tui::AppEvent::Key(k) => {
                    if detail_fetch.is_some() {
                        match k.code {
                            KeyCode::Esc => {
                                detail_fetch = None;
                                detail_pick = ui::screens::detail::DetailPickMode::None;
                            }
                            KeyCode::Up => detail_fetch_sel = detail_fetch_sel.saturating_sub(1),
                            KeyCode::Down => {
                                let len = detail_fetch.as_ref().map_or(0, Vec::len);
                                detail_fetch_sel =
                                    (detail_fetch_sel + 1).min(len.saturating_sub(1));
                            }
                            KeyCode::Enter => {
                                if detail_pick == ui::screens::detail::DetailPickMode::MultiSel {
                                    // 多选模式：Enter = 勾选当前项（箭头定位后直接选中）。
                                    let len = detail_fetch.as_ref().map_or(0, Vec::len);
                                    if detail_fetch_sel < len
                                        && !detail_multi_sel.insert(detail_fetch_sel)
                                    {
                                        detail_multi_sel.remove(&detail_fetch_sel);
                                    }
                                } else {
                                    // Slot 模式：Enter 确认选中；保存走 s 键或保存按钮。
                                    let list = detail_fetch.clone();
                                    let result = match (list, detail_pick) {
                                        (
                                            Some(list),
                                            ui::screens::detail::DetailPickMode::Slot(slot),
                                        ) => detail_slot_pick_result(
                                            &home,
                                            &provider_state,
                                            &list,
                                            detail_fetch_sel,
                                            slot,
                                        ),
                                        _ => Ok(String::new()),
                                    };
                                    match result {
                                        Ok(_) => {
                                            detail_fetch = None;
                                            detail_pick = ui::screens::detail::DetailPickMode::None;
                                            detail_multi_sel.clear();
                                            provider_state = reload_providers_keep(&provider_state);
                                        }
                                        Err(error) => {
                                            detail_fetch = None;
                                            detail_pick = ui::screens::detail::DetailPickMode::None;
                                            provider_action_result = error;
                                            mode = Mode::ProviderActionResult;
                                        }
                                    }
                                }
                            }
                            KeyCode::Char(' ') => match detail_pick {
                                ui::screens::detail::DetailPickMode::MultiSel => {
                                    // Enter/空格 = 勾选当前项（箭头定位后直接选中）。
                                    let len = detail_fetch.as_ref().map_or(0, Vec::len);
                                    if detail_fetch_sel < len
                                        && !detail_multi_sel.insert(detail_fetch_sel)
                                    {
                                        detail_multi_sel.remove(&detail_fetch_sel);
                                    }
                                }
                                ui::screens::detail::DetailPickMode::Slot(slot) => {
                                    let list = detail_fetch.clone();
                                    let result = match list {
                                        Some(list) => detail_slot_pick_result(
                                            &home,
                                            &provider_state,
                                            &list,
                                            detail_fetch_sel,
                                            slot,
                                        ),
                                        None => Ok(String::new()),
                                    };
                                    match result {
                                        Ok(_) => {
                                            detail_fetch = None;
                                            detail_pick = ui::screens::detail::DetailPickMode::None;
                                            provider_state = reload_providers_keep(&provider_state);
                                        }
                                        Err(error) => {
                                            detail_fetch = None;
                                            detail_pick = ui::screens::detail::DetailPickMode::None;
                                            provider_action_result = error;
                                            mode = Mode::ProviderActionResult;
                                        }
                                    }
                                }
                                _ => {}
                            },
                            KeyCode::Char('s')
                                if detail_pick == ui::screens::detail::DetailPickMode::MultiSel =>
                            {
                                // 多选模式：s = 保存勾选（同右上角保存按钮）。
                                let list = detail_fetch.clone();
                                let saved_count = detail_multi_sel.len();
                                let result = match list {
                                    Some(list) => detail_multi_save_result(
                                        &home,
                                        &provider_state,
                                        &list,
                                        &detail_multi_sel,
                                    ),
                                    None => Ok(String::new()),
                                };
                                match result {
                                    Ok(_) => {
                                        detail_fetch = None;
                                        detail_pick = ui::screens::detail::DetailPickMode::None;
                                        detail_multi_sel.clear();
                                        detail_feedback = format!("已保存 {saved_count} 个模型");
                                        provider_state = reload_providers_keep(&provider_state);
                                    }
                                    Err(error) => {
                                        detail_fetch = None;
                                        detail_pick = ui::screens::detail::DetailPickMode::None;
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                            }
                            _ => {}
                        }
                        need_redraw = true;
                    } else if detail_field.is_some() {
                        match k.code {
                            KeyCode::Esc => {
                                detail_field = None;
                                detail_buf.clear();
                            }
                            KeyCode::Enter => {
                                let result = match detail_field {
                                    Some(field) => detail_field_save_result(
                                        &home,
                                        &provider_state,
                                        field,
                                        &detail_buf,
                                    ),
                                    None => Ok(String::new()),
                                };
                                match result {
                                    Ok(_) => {
                                        detail_field = None;
                                        detail_buf.clear();
                                        detail_feedback = "已保存".to_string();
                                        provider_state = reload_providers_keep(&provider_state);
                                    }
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                            }
                            KeyCode::Backspace => {
                                detail_buf.pop();
                            }
                            KeyCode::Char(c) => detail_buf.push(c),
                            _ => {}
                        }
                        need_redraw = true;
                    } else {
                        match k.code {
                            KeyCode::Esc => {
                                detail_fetch_reset(
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                mode = Mode::Provider;
                                need_redraw = true;
                            }
                            KeyCode::Tab => {
                                let next = detail_next_tab(detail_agent_tab, 1);
                                detail_switch_tab(
                                    &mut detail_agent_tab,
                                    next,
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                need_redraw = true;
                            }
                            KeyCode::BackTab => {
                                let next = detail_next_tab(detail_agent_tab, -1);
                                detail_switch_tab(
                                    &mut detail_agent_tab,
                                    next,
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                need_redraw = true;
                            }
                            KeyCode::Up | KeyCode::Down => {
                                // 焦点在区域间上下移动（协议→开关→操作→表格→循环）。
                                let size = terminal.size().unwrap_or_default();
                                let area =
                                    ratatui::layout::Rect::new(0, 0, size.width, size.height);
                                let models_len = provider_state
                                    .providers
                                    .get(provider_state.selected)
                                    .map_or(0, |p| p.models.len());
                                let models_visible = models_len
                                    .min(ui::screens::detail::detail_table_visible_rows(area));
                                let counts = ui::screens::detail::detail_focus_counts(
                                    detail_agent_tab,
                                    models_visible,
                                );
                                let direction: i8 = if k.code == KeyCode::Up { -1 } else { 1 };
                                let mut zone = detail_focus_zone as i8;
                                for _ in 0..4 {
                                    zone = (zone + direction).rem_euclid(4);
                                    if counts[zone as usize] > 0 {
                                        break;
                                    }
                                }
                                detail_focus_zone = zone as u8;
                                detail_focus_index =
                                    detail_focus_index.min(counts[zone as usize].saturating_sub(1));
                                need_redraw = true;
                            }
                            KeyCode::Left | KeyCode::Right => {
                                // 焦点在区内元素间左右移动。
                                let size = terminal.size().unwrap_or_default();
                                let area =
                                    ratatui::layout::Rect::new(0, 0, size.width, size.height);
                                let models_len = provider_state
                                    .providers
                                    .get(provider_state.selected)
                                    .map_or(0, |p| p.models.len());
                                let models_visible = models_len
                                    .min(ui::screens::detail::detail_table_visible_rows(area));
                                let counts = ui::screens::detail::detail_focus_counts(
                                    detail_agent_tab,
                                    models_visible,
                                );
                                let total = counts[detail_focus_zone as usize];
                                if total > 0 {
                                    let direction: i8 =
                                        if k.code == KeyCode::Left { -1 } else { 1 };
                                    detail_focus_index = ((detail_focus_index as i8 + direction)
                                        .rem_euclid(total as i8))
                                        as usize;
                                }
                                need_redraw = true;
                            }
                            KeyCode::Enter => {
                                // 激活焦点元素（复用鼠标 action 分支的语义）。
                                if let Some(action) = ui::screens::detail::detail_focus_action(
                                    detail_agent_tab,
                                    detail_focus_zone,
                                    detail_focus_index,
                                    provider_mode,
                                ) {
                                    match action {
                                        ui::screens::providers::ProviderDetailAction::SetProtocol(kind) => {
                                            match provider_state
                                                .providers
                                                .get(provider_state.selected)
                                            {
                                                Some(provider) if provider.protocol == kind => {}
                                                Some(provider) => {
                                                    let args = detail_set_kind_args(provider, kind);
                                                    match trivium::cli::run_command(&home, &args)
                                                        .ok_or_else(|| {
                                                            "provider update 命令不可用。"
                                                                .to_string()
                                                        })
                                                        .and_then(|result| result)
                                                    {
                                                        Ok(message) => {
                                                            detail_feedback =
                                                                format!("✓ {message}");
                                                            provider_state = reload_providers_keep(
                                                                &provider_state,
                                                            );
                                                        }
                                                        Err(error) => {
                                                            provider_action_result = error;
                                                            mode = Mode::ProviderActionResult;
                                                        }
                                                    }
                                                }
                                                None => {}
                                            }
                                        }
                                        ui::screens::providers::ProviderDetailAction::TogglePermission => {
                                            match detail_toggle_permission(&home, detail_agent_tab)
                                            {
                                                Ok(message) => detail_feedback = message,
                                                Err(error) => {
                                                    provider_action_result = error;
                                                    mode = Mode::ProviderActionResult;
                                                }
                                            }
                                        }
                                        ui::screens::providers::ProviderDetailAction::ToggleTerminalTitle => {
                                            let enable = !read_terminal_title_disabled(&home);
                                            match toggle_terminal_title(&home, enable) {
                                                Ok(message) => detail_feedback = message,
                                                Err(error) => {
                                                    provider_action_result = error;
                                                    mode = Mode::ProviderActionResult;
                                                }
                                            }
                                        }
                                        ui::screens::providers::ProviderDetailAction::ToggleRoute => {
                                            match detail_toggle_route(
                                                &home,
                                                &mut provider_state,
                                                detail_agent_tab,
                                            ) {
                                                Ok(message) => detail_feedback = message,
                                                Err(error) => {
                                                    provider_action_result = error;
                                                    mode = Mode::ProviderActionResult;
                                                }
                                            }
                                        }
                                        ui::screens::providers::ProviderDetailAction::Edit => {
                                            if let Some(form) =
                                                edit_form_from_selected_provider(&provider_state)
                                            {
                                                provider_edit_form = form;
                                                detail_fetch_reset(
                                                    &mut detail_field,
                                                    &mut detail_buf,
                                                    &mut detail_fetch,
                                                    &mut detail_fetch_sel,
                                                    &mut detail_pick,
                                                    &mut detail_feedback,
                                                    &mut detail_fetch_loading,
                                                    &mut detail_fetch_rx,
                                                    &mut detail_pending_pick,
                                                    &mut detail_focus_zone,
                                                    &mut detail_focus_index,
                                                );
                                                mode = Mode::ProviderEditForm;
                                            }
                                        }
                                        ui::screens::providers::ProviderDetailAction::Test => {
                                            connection_result = provider_test_start_message();
                                            connection_rx = start_provider_test(&provider_state);
                                            detail_fetch_reset(
                                                &mut detail_field,
                                                &mut detail_buf,
                                                &mut detail_fetch,
                                                &mut detail_fetch_sel,
                                                &mut detail_pick,
                                                &mut detail_feedback,
                                                &mut detail_fetch_loading,
                                                &mut detail_fetch_rx,
                                                &mut detail_pending_pick,
                                                &mut detail_focus_zone,
                                                &mut detail_focus_index,
                                            );
                                            mode = Mode::ConnectionResult;
                                        }
                                        ui::screens::providers::ProviderDetailAction::Models => {
                                            if detail_fetch_loading {
                                                detail_feedback = "正在获取模型列表…".to_string();
                                            } else if let Some(rx) =
                                                start_models_fetch_cached(&provider_state, true)
                                            {
                                                detail_fetch_loading = true;
                                                detail_fetch_rx = Some(rx);
                                                detail_pending_pick =
                                                    ui::screens::detail::DetailPickMode::MultiSel;
                                                detail_feedback = "正在获取模型列表…".to_string();
                                            } else {
                                                provider_action_result =
                                                    "拉取失败：provider fetch-models 命令不可用"
                                                        .to_string();
                                                mode = Mode::ProviderActionResult;
                                            }
                                        }
                                        ui::screens::providers::ProviderDetailAction::SelectMode(selected) => {
                                            provider_mode = selected;
                                        }
                                        ui::screens::providers::ProviderDetailAction::EnterMulti => {
                                            multi_provider_ids.clear();
                                            multi_provider_cursor = 0;
                                            multi_provider_target = AgentTarget::Codex;
                                            detail_fetch_reset(
                                                &mut detail_field,
                                                &mut detail_buf,
                                                &mut detail_fetch,
                                                &mut detail_fetch_sel,
                                                &mut detail_pick,
                                                &mut detail_feedback,
                                                &mut detail_fetch_loading,
                                                &mut detail_fetch_rx,
                                                &mut detail_pending_pick,
                                                &mut detail_focus_zone,
                                                &mut detail_focus_index,
                                            );
                                            mode = Mode::MultiProvider;
                                        }
                                        ui::screens::providers::ProviderDetailAction::Apply(target) => {
                                            pending_target = Some(target);
                                            pending_provider_ids =
                                                selected_provider_ids(&provider_state);
                                            plan_preview = build_plan_preview_for_ids(
                                                &provider_state,
                                                target,
                                                &pending_provider_ids,
                                            );
                                            detail_fetch_reset(
                                                &mut detail_field,
                                                &mut detail_buf,
                                                &mut detail_fetch,
                                                &mut detail_fetch_sel,
                                                &mut detail_pick,
                                                &mut detail_feedback,
                                                &mut detail_fetch_loading,
                                                &mut detail_fetch_rx,
                                                &mut detail_pending_pick,
                                                &mut detail_focus_zone,
                                                &mut detail_focus_index,
                                            );
                                            mode = Mode::PlanPreview;
                                        }
                                        _ => {}
                                    }
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('y') => {
                                detail_secret_visible = !detail_secret_visible;
                                need_redraw = true;
                            }
                            KeyCode::Char('g') => {
                                if provider_mode == ui::screens::providers::ProviderMode::Multi {
                                    multi_provider_ids.clear();
                                    multi_provider_cursor = 0;
                                    multi_provider_target = AgentTarget::Codex;
                                    mode = Mode::MultiProvider;
                                } else {
                                    provider_mode = ui::screens::providers::ProviderMode::Multi;
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('s') => {
                                provider_mode = ui::screens::providers::ProviderMode::Single;
                                need_redraw = true;
                            }
                            KeyCode::Char('v') => {
                                // 与 p 一致：完全权限原地反馈（不再跳结果页）。
                                match detail_toggle_permission(&home, detail_agent_tab) {
                                    Ok(message) => detail_feedback = message,
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('1') => {
                                pending_target = Some(AgentTarget::OpenCode);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    AgentTarget::OpenCode,
                                    &pending_provider_ids,
                                );
                                mode = Mode::PlanPreview;
                                need_redraw = true;
                            }
                            KeyCode::Char('2') => {
                                pending_target = Some(AgentTarget::ClaudeCode);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    AgentTarget::ClaudeCode,
                                    &pending_provider_ids,
                                );
                                mode = Mode::PlanPreview;
                                need_redraw = true;
                            }
                            KeyCode::Char('3') => {
                                pending_target = Some(AgentTarget::Codex);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    AgentTarget::Codex,
                                    &pending_provider_ids,
                                );
                                mode = Mode::PlanPreview;
                                need_redraw = true;
                            }
                            KeyCode::Char('p') => {
                                match detail_toggle_permission(&home, detail_agent_tab) {
                                    Ok(message) => detail_feedback = message,
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('h') => {
                                let enable = !read_terminal_title_disabled(&home);
                                match toggle_terminal_title(&home, enable) {
                                    Ok(message) => detail_feedback = message,
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('r') => {
                                match detail_toggle_route(
                                    &home,
                                    &mut provider_state,
                                    detail_agent_tab,
                                ) {
                                    Ok(message) => detail_feedback = message,
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('d') => {
                                mode = Mode::ProviderDeleteConfirm;
                                need_redraw = true;
                            }
                            KeyCode::Char('e') => {
                                if let Some(form) =
                                    edit_form_from_selected_provider(&provider_state)
                                {
                                    provider_edit_form = form;
                                    detail_fetch_reset(
                                        &mut detail_field,
                                        &mut detail_buf,
                                        &mut detail_fetch,
                                        &mut detail_fetch_sel,
                                        &mut detail_pick,
                                        &mut detail_feedback,
                                        &mut detail_fetch_loading,
                                        &mut detail_fetch_rx,
                                        &mut detail_pending_pick,
                                        &mut detail_focus_zone,
                                        &mut detail_focus_index,
                                    );
                                    mode = Mode::ProviderEditForm;
                                } else {
                                    provider_action_result =
                                        "编辑失败：没有选中的 provider。".to_string();
                                    mode = Mode::ProviderActionResult;
                                }
                                need_redraw = true;
                            }
                            KeyCode::Char('t') => {
                                connection_result = provider_test_start_message();
                                connection_rx = start_provider_test(&provider_state);
                                if connection_rx.is_none() {
                                    connection_result =
                                        "测试失败：没有选中的 provider。".to_string();
                                }
                                detail_fetch_reset(
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                mode = Mode::ConnectionResult;
                                need_redraw = true;
                            }
                            KeyCode::Char('a') => {
                                // 生效：应用到当前 Tab 的端（diff 预览 + 备份）。
                                pending_target = Some(detail_agent_tab);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    detail_agent_tab,
                                    &pending_provider_ids,
                                );
                                detail_fetch_reset(
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                mode = Mode::PlanPreview;
                                need_redraw = true;
                            }
                            KeyCode::Char('m') => {
                                if let Some(rx) = start_models_fetch_cached(&provider_state, true) {
                                    detail_fetch_loading = true;
                                    detail_fetch_rx = Some(rx);
                                    detail_pending_pick =
                                        ui::screens::detail::DetailPickMode::MultiSel;
                                    detail_feedback = "正在获取模型列表…".to_string();
                                    detail_fetch_sel = 0;
                                } else {
                                    provider_action_result =
                                        "拉取失败：provider fetch-models 命令不可用".to_string();
                                    mode = Mode::ProviderActionResult;
                                }
                                need_redraw = true;
                            }
                            _ => {}
                        }
                    }
                }
                tui::AppEvent::Mouse(m) => {
                    let models_len = provider_state
                        .providers
                        .get(provider_state.selected)
                        .map(|provider| {
                            if provider.model_entries.is_empty() {
                                provider.models.len()
                            } else {
                                provider.model_entries.len()
                            }
                        })
                        .unwrap_or(0);
                    if let Some(action) = ui::screens::detail::provider_detail_v2_mouse_action(
                        screen_area,
                        models_len,
                        detail_fetch
                            .as_ref()
                            .map(|list| (list.len(), detail_fetch_sel)),
                        provider_mode,
                        detail_agent_tab,
                        detail_pick,
                        &m,
                    ) {
                        match action {
                            ui::screens::providers::ProviderDetailAction::SwitchTab(target) => {
                                detail_switch_tab(
                                    &mut detail_agent_tab,
                                    target,
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                            }
                            ui::screens::providers::ProviderDetailAction::PickSlot(slot) => {
                                // 槽位展开：缓存优先秒开；未命中则后台拉取并提示。
                                if let Ok(list) = detail_cached_models_result(&provider_state) {
                                    detail_fetch = Some(list);
                                    detail_fetch_sel = 0;
                                    detail_pick = ui::screens::detail::DetailPickMode::Slot(slot);
                                } else if let Some(rx) =
                                    start_models_fetch_cached(&provider_state, true)
                                {
                                    detail_fetch_loading = true;
                                    detail_fetch_rx = Some(rx);
                                    detail_pending_pick =
                                        ui::screens::detail::DetailPickMode::Slot(slot);
                                    detail_feedback = "正在获取模型列表…".to_string();
                                } else {
                                    provider_action_result =
                                        "拉取失败：provider fetch-models 命令不可用".to_string();
                                    mode = Mode::ProviderActionResult;
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::PickSlotModel(index) => {
                                detail_fetch_sel = index;
                                let list = detail_fetch.clone();
                                let result = match (list, detail_pick) {
                                    (
                                        Some(list),
                                        ui::screens::detail::DetailPickMode::Slot(slot),
                                    ) => detail_slot_pick_result(
                                        &home,
                                        &provider_state,
                                        &list,
                                        index,
                                        slot,
                                    ),
                                    _ => Ok(String::new()),
                                };
                                match result {
                                    Ok(_) => {
                                        detail_fetch = None;
                                        detail_pick = ui::screens::detail::DetailPickMode::None;
                                        detail_feedback =
                                            "已填入槽位（保存写 claudeSlots）".to_string();
                                        provider_state = reload_providers_keep(&provider_state);
                                    }
                                    Err(error) => {
                                        detail_fetch = None;
                                        detail_pick = ui::screens::detail::DetailPickMode::None;
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::SetProtocol(kind) => {
                                match provider_state.providers.get(provider_state.selected) {
                                    Some(provider) if provider.protocol == kind => {}
                                    Some(provider) => {
                                        let args = detail_set_kind_args(provider, kind);
                                        match trivium::cli::run_command(&home, &args)
                                            .ok_or_else(|| {
                                                "provider update 命令不可用。".to_string()
                                            })
                                            .and_then(|result| result)
                                        {
                                            Ok(message) => {
                                                detail_feedback = format!("✓ {message}");
                                                provider_state =
                                                    reload_providers_keep(&provider_state);
                                            }
                                            Err(error) => {
                                                provider_action_result = error;
                                                mode = Mode::ProviderActionResult;
                                            }
                                        }
                                    }
                                    None => {}
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::Edit => {
                                if let Some(form) =
                                    edit_form_from_selected_provider(&provider_state)
                                {
                                    provider_edit_form = form;
                                    detail_fetch_reset(
                                        &mut detail_field,
                                        &mut detail_buf,
                                        &mut detail_fetch,
                                        &mut detail_fetch_sel,
                                        &mut detail_pick,
                                        &mut detail_feedback,
                                        &mut detail_fetch_loading,
                                        &mut detail_fetch_rx,
                                        &mut detail_pending_pick,
                                        &mut detail_focus_zone,
                                        &mut detail_focus_index,
                                    );
                                    mode = Mode::ProviderEditForm;
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::Test => {
                                connection_result = provider_test_start_message();
                                connection_rx = start_provider_test(&provider_state);
                                detail_fetch_reset(
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                mode = Mode::ConnectionResult;
                            }
                            ui::screens::providers::ProviderDetailAction::Apply(target) => {
                                pending_target = Some(target);
                                pending_provider_ids = selected_provider_ids(&provider_state);
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    target,
                                    &pending_provider_ids,
                                );
                                detail_fetch_reset(
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                mode = Mode::PlanPreview;
                            }
                            ui::screens::providers::ProviderDetailAction::Models => {
                                // 模型按钮：强制联网刷新并写入本地缓存（后台异步，
                                // 期间显示加载提示；在途时忽略重复点击）。
                                if detail_fetch_loading {
                                    detail_feedback = "正在获取模型列表…".to_string();
                                } else if let Some(rx) =
                                    start_models_fetch_cached(&provider_state, true)
                                {
                                    detail_fetch_loading = true;
                                    detail_fetch_rx = Some(rx);
                                    detail_pending_pick =
                                        ui::screens::detail::DetailPickMode::MultiSel;
                                    detail_feedback = "正在获取模型列表…".to_string();
                                } else {
                                    provider_action_result =
                                        "拉取失败：provider fetch-models 命令不可用".to_string();
                                    mode = Mode::ProviderActionResult;
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::TogglePermission => {
                                match detail_toggle_permission(&home, detail_agent_tab) {
                                    Ok(message) => {
                                        detail_feedback = message;
                                    }
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::ToggleTerminalTitle => {
                                let enable = !read_terminal_title_disabled(&home);
                                match toggle_terminal_title(&home, enable) {
                                    Ok(message) => {
                                        detail_feedback = message;
                                    }
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::ToggleRoute => {
                                match detail_toggle_route(
                                    &home,
                                    &mut provider_state,
                                    detail_agent_tab,
                                ) {
                                    Ok(message) => {
                                        detail_feedback = message;
                                    }
                                    Err(error) => {
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::SelectMode(selected) => {
                                provider_mode = selected;
                            }
                            ui::screens::providers::ProviderDetailAction::EnterMulti => {
                                multi_provider_ids.clear();
                                multi_provider_cursor = 0;
                                multi_provider_target = AgentTarget::Codex;
                                detail_fetch_reset(
                                    &mut detail_field,
                                    &mut detail_buf,
                                    &mut detail_fetch,
                                    &mut detail_fetch_sel,
                                    &mut detail_pick,
                                    &mut detail_feedback,
                                    &mut detail_fetch_loading,
                                    &mut detail_fetch_rx,
                                    &mut detail_pending_pick,
                                    &mut detail_focus_zone,
                                    &mut detail_focus_index,
                                );
                                mode = Mode::MultiProvider;
                            }
                            ui::screens::providers::ProviderDetailAction::DetailEdit(field) => {
                                detail_buf = provider_state
                                    .providers
                                    .get(provider_state.selected)
                                    .map(|provider| detail_field_value(provider, field))
                                    .unwrap_or_default();
                                detail_field = Some(field);
                            }
                            ui::screens::providers::ProviderDetailAction::ToggleModelSel(index) => {
                                detail_fetch_sel = index;
                                if !detail_multi_sel.insert(index) {
                                    detail_multi_sel.remove(&index);
                                }
                            }
                            ui::screens::providers::ProviderDetailAction::SaveModels => {
                                let list = detail_fetch.clone();
                                let saved_count = detail_multi_sel.len();
                                let result = match list {
                                    Some(list) => detail_multi_save_result(
                                        &home,
                                        &provider_state,
                                        &list,
                                        &detail_multi_sel,
                                    ),
                                    None => Ok(String::new()),
                                };
                                match result {
                                    Ok(_) => {
                                        detail_fetch = None;
                                        detail_pick = ui::screens::detail::DetailPickMode::None;
                                        detail_multi_sel.clear();
                                        detail_feedback = format!("已保存 {saved_count} 个模型");
                                        provider_state = reload_providers_keep(&provider_state);
                                    }
                                    Err(error) => {
                                        detail_fetch = None;
                                        detail_pick = ui::screens::detail::DetailPickMode::None;
                                        provider_action_result = error;
                                        mode = Mode::ProviderActionResult;
                                    }
                                }
                            }
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::MultiProvider => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Provider;
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        multi_provider_cursor = multi_provider_cursor.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Down => {
                        multi_provider_cursor = (multi_provider_cursor + 1)
                            .min(provider_state.providers.len().saturating_sub(1));
                        need_redraw = true;
                    }
                    KeyCode::Char(' ') => {
                        toggle_multi_provider(
                            &provider_state,
                            multi_provider_cursor,
                            &mut multi_provider_ids,
                        );
                        need_redraw = true;
                    }
                    KeyCode::Char('1') => {
                        multi_provider_target = AgentTarget::ClaudeCode;
                        need_redraw = true;
                    }
                    KeyCode::Char('2') => {
                        multi_provider_target = AgentTarget::Codex;
                        need_redraw = true;
                    }
                    KeyCode::Enter if multi_provider_ids.len() >= 2 => {
                        pending_target = Some(multi_provider_target);
                        pending_provider_ids = multi_provider_ids.clone();
                        plan_preview = build_plan_preview_for_ids(
                            &provider_state,
                            multi_provider_target,
                            &pending_provider_ids,
                        );
                        mode = Mode::PlanPreview;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(action) = ui::screens::providers::multi_provider_mouse_action(
                        screen_area,
                        provider_state.providers.len(),
                        multi_provider_cursor,
                        &m,
                    ) {
                        match action {
                            ui::screens::providers::MultiProviderAction::Cancel => {
                                mode = Mode::Provider
                            }
                            ui::screens::providers::MultiProviderAction::Target(target) => {
                                multi_provider_target = target
                            }
                            ui::screens::providers::MultiProviderAction::Toggle(index) => {
                                multi_provider_cursor = index;
                                toggle_multi_provider(
                                    &provider_state,
                                    index,
                                    &mut multi_provider_ids,
                                );
                            }
                            ui::screens::providers::MultiProviderAction::MoveUp => {
                                move_multi_provider(
                                    &provider_state,
                                    &mut multi_provider_ids,
                                    multi_provider_cursor,
                                    false,
                                )
                            }
                            ui::screens::providers::MultiProviderAction::MoveDown => {
                                move_multi_provider(
                                    &provider_state,
                                    &mut multi_provider_ids,
                                    multi_provider_cursor,
                                    true,
                                )
                            }
                            ui::screens::providers::MultiProviderAction::Preview
                                if multi_provider_ids.len() >= 2 =>
                            {
                                pending_target = Some(multi_provider_target);
                                pending_provider_ids = multi_provider_ids.clone();
                                plan_preview = build_plan_preview_for_ids(
                                    &provider_state,
                                    multi_provider_target,
                                    &pending_provider_ids,
                                );
                                mode = Mode::PlanPreview;
                            }
                            ui::screens::providers::MultiProviderAction::Preview => {}
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ProviderDeleteConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        provider_action_result = delete_selected_provider(&mut provider_state);
                        mode = Mode::ProviderActionResult;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        mode = Mode::ProviderDetail;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            provider_action_result = delete_selected_provider(&mut provider_state);
                            mode = Mode::ProviderActionResult;
                            need_redraw = true;
                        } else {
                            mode = Mode::ProviderDetail;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ProviderActionResult => match ev {
                tui::AppEvent::Key(k) => {
                    if k.code == KeyCode::Esc {
                        provider_state = reload_providers_keep(&provider_state);
                        mode = Mode::Provider;
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ProviderPresetList => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Provider;
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        provider_preset_idx = provider_preset_idx.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Down => {
                        let len = trivium::provider_presets::all().len();
                        if provider_preset_idx < len.saturating_sub(1) {
                            provider_preset_idx += 1;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(preset) = trivium::provider_presets::all().get(provider_preset_idx)
                        {
                            provider_add_form = provider_add_form_from_preset(preset);
                            mode = Mode::ProviderAddForm;
                            need_redraw = true;
                        }
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(index) = ui::screens::forms::provider_preset_list_hit_test(
                        screen_area,
                        provider_preset_idx,
                        &m,
                    ) {
                        provider_preset_idx = index;
                        if let Some(preset) = trivium::provider_presets::all().get(index) {
                            provider_add_form = provider_add_form_from_preset(preset);
                            mode = Mode::ProviderAddForm;
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::PlanPreview => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Char('a') if pending_target.is_some() => {
                        mode = Mode::ApplyConfirm;
                        need_redraw = true;
                    }
                    KeyCode::Esc => {
                        mode = Mode::ProviderDetail;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ApplyConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        detail_feedback = apply_provider_plan(
                            &mut provider_state,
                            pending_target,
                            &pending_provider_ids,
                        );
                        mode = Mode::ProviderDetail;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        mode = Mode::PlanPreview;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            detail_feedback = apply_provider_plan(
                                &mut provider_state,
                                pending_target,
                                &pending_provider_ids,
                            );
                            mode = Mode::ProviderDetail;
                            need_redraw = true;
                        } else {
                            mode = Mode::PlanPreview;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::ConnectionResult => match ev {
                tui::AppEvent::Key(k) => {
                    if k.code == KeyCode::Esc {
                        mode = Mode::ProviderDetail;
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::Client => match handle_client_event(
                ev,
                &mut ClientState {
                    agent_idx: &mut agent_idx,
                    agent_statuses: &mut agent_statuses,
                    agent_status_message: &mut agent_status_message,
                    agent_pending_update: &mut agent_pending_update,
                    agent_pending_setup: &mut agent_pending_setup,
                    opencode_permission: &mut opencode_permission,
                    pending_opencode_permission: &mut pending_opencode_permission,
                    opencode_settings_message: &mut opencode_settings_message,
                    home: &home,
                    terminal: &mut terminal,
                },
            ) {
                ClientOutcome::Cont {
                    mode: next_mode,
                    need_redraw: redraw,
                } => {
                    mode = next_mode;
                    need_redraw = redraw;
                }
                ClientOutcome::PendingLatest { rx } => {
                    pending_agent_latest_rx = Some(rx);
                    need_redraw = true;
                }
                ClientOutcome::RunInstall { statuses, index } => {
                    // 安装在 TUI 内完成：busy 屏等待，结果写入 Agent 页 hint，
                    // 不再退出整个应用（旧行为：退出 TUI 落回裸终端打印日志）。
                    let label = statuses
                        .get(index)
                        .map(|row| format!("正在安装/更新 {}…", row.tool.label))
                        .unwrap_or_else(|| "正在安装/更新…".to_string());
                    let (message, _elapsed) =
                        run_with_busy(&mut terminal, &label, || {
                            agent_install_summary(&statuses, index)
                        });
                    agent_status_message = message;
                    mode = Mode::Client;
                    need_redraw = true;
                }
                ClientOutcome::RunSetup => {
                    let (message, _elapsed) = run_with_busy(
                        &mut terminal,
                        "正在执行全部客户端安装/更新…",
                        agent_setup_summary,
                    );
                    agent_status_message = message;
                    mode = Mode::Client;
                    need_redraw = true;
                }
            },

            Mode::OpenCodeSettings => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        pending_opencode_permission = None;
                        mode = Mode::Client;
                        need_redraw = true;
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y')
                        if pending_opencode_permission.is_some() =>
                    {
                        let permission = pending_opencode_permission.take().unwrap_or_default();
                        match trivium::opencode_settings::set_permission(&home, &permission, false) {
                            Ok(result) => {
                                opencode_permission = permission;
                                opencode_settings_message = format!(
                                    "已保存。{}{}",
                                    result.path.display(),
                                    result
                                        .backup_path
                                        .map(|path| format!("\n备份：{}", path.display()))
                                        .unwrap_or_default()
                                );
                            }
                            Err(error) => opencode_settings_message = format!("保存失败：{error}"),
                        }
                        need_redraw = true;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        pending_opencode_permission = None;
                        opencode_settings_message.clear();
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(action) =
                        ui::screens::extra::opencode_settings_mouse_action(screen_area, &m)
                    {
                        if action == "back" {
                            pending_opencode_permission = None;
                            mode = Mode::Client;
                        } else {
                            match trivium::opencode_settings::set_permission(&home, action, true) {
                                Ok(result) => {
                                    pending_opencode_permission = Some(action.to_string());
                                    opencode_settings_message = format!(
                                        "预览：权限 -> {action}\n\n{}\n回车确认，Esc 取消。",
                                        result.diff
                                    );
                                }
                                Err(error) => {
                                    opencode_settings_message = format!("预览失败：{error}")
                                }
                            }
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::Mcp => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Menu;
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        mcp_selected = mcp_selected.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Down => {
                        if mcp_selected + 1 < mcp_servers.len() {
                            mcp_selected += 1;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Char('r') => {
                        (mcp_servers, mcp_error) = load_mcp_servers();
                        mcp_selected = mcp_selected.min(mcp_servers.len().saturating_sub(1));
                        need_redraw = true;
                    }
                    KeyCode::Char('a') => {
                        mcp_form = McpForm {
                            transport: "stdio".to_string(),
                            ..McpForm::default()
                        };
                        mode = Mode::McpForm;
                        need_redraw = true;
                    }
                    KeyCode::Char('p') => {
                        mcp_preset_selected = 0;
                        mode = Mode::McpPresets;
                        need_redraw = true;
                    }
                    KeyCode::Char('i') => {
                        pending_mcp_args = vec![
                            "mcp".to_string(),
                            "import".to_string(),
                            "--target".to_string(),
                            "all".to_string(),
                        ];
                        mcp_message = run_mcp_command(&home, &pending_mcp_args);
                        mode = Mode::McpConfirm;
                        need_redraw = true;
                    }
                    KeyCode::Char('e') => {
                        if let Some(server) = mcp_servers.get(mcp_selected) {
                            mcp_form = mcp_edit_form(server);
                            mode = Mode::McpForm;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Char('d') => {
                        if let Some(server) = mcp_servers.get(mcp_selected) {
                            pending_mcp_args =
                                vec!["mcp".to_string(), "delete".to_string(), server.id.clone()];
                            mcp_message = run_mcp_command(&home, &pending_mcp_args);
                            mode = Mode::McpConfirm;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Char(c @ '1'..='3') => {
                        let target = match c {
                            '1' => AgentTarget::OpenCode,
                            '2' => AgentTarget::ClaudeCode,
                            _ => AgentTarget::Codex,
                        };
                        if let Some(server) = mcp_servers.get(mcp_selected) {
                            pending_mcp_args = mcp_toggle_args(server, target);
                            mcp_message = run_mcp_command(&home, &pending_mcp_args);
                            mode = Mode::McpConfirm;
                            need_redraw = true;
                        }
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(action) = ui::screens::mcp::mcp_mouse_action(
                        screen_area,
                        mcp_selected,
                        mcp_servers.len(),
                        &m,
                    ) {
                        match action {
                            ui::screens::mcp::McpMouseAction::Select(index) => mcp_selected = index,
                            ui::screens::mcp::McpMouseAction::Add => {
                                mcp_form = McpForm {
                                    transport: "stdio".to_string(),
                                    ..McpForm::default()
                                };
                                mode = Mode::McpForm;
                            }
                            ui::screens::mcp::McpMouseAction::Presets => {
                                mcp_preset_selected = 0;
                                mode = Mode::McpPresets;
                            }
                            ui::screens::mcp::McpMouseAction::ImportAll => {
                                pending_mcp_args = vec![
                                    "mcp".to_string(),
                                    "import".to_string(),
                                    "--target".to_string(),
                                    "all".to_string(),
                                ];
                                mcp_message = run_mcp_command(&home, &pending_mcp_args);
                                mode = Mode::McpConfirm;
                            }
                            ui::screens::mcp::McpMouseAction::Edit => {
                                if let Some(server) = mcp_servers.get(mcp_selected) {
                                    mcp_form = mcp_edit_form(server);
                                    mode = Mode::McpForm;
                                }
                            }
                            ui::screens::mcp::McpMouseAction::Delete => {
                                if let Some(server) = mcp_servers.get(mcp_selected) {
                                    pending_mcp_args = vec![
                                        "mcp".to_string(),
                                        "delete".to_string(),
                                        server.id.clone(),
                                    ];
                                    mcp_message = run_mcp_command(&home, &pending_mcp_args);
                                    mode = Mode::McpConfirm;
                                }
                            }
                            ui::screens::mcp::McpMouseAction::Toggle(index, target) => {
                                mcp_selected = index;
                                if let Some(server) = mcp_servers.get(index) {
                                    pending_mcp_args = mcp_toggle_args(server, target);
                                    mcp_message = run_mcp_command(&home, &pending_mcp_args);
                                    mode = Mode::McpConfirm;
                                }
                            }
                            ui::screens::mcp::McpMouseAction::SyncTarget(target) => {
                                if mcp_servers.is_empty() {
                                    mcp_message = "暂无 MCP 服务可同步".to_string();
                                } else if target == AgentTarget::OpenCode {
                                    mcp_message =
                                        "OpenCode 为源端，无需同步（其余两段为目标）".to_string();
                                } else {
                                    let (message, elapsed) =
                                        run_with_busy(&mut terminal, "正在同步 MCP…", || {
                                            match trivium::sync_mcp::sync_mcp(
                                                &home,
                                                AgentTarget::OpenCode,
                                                &[target],
                                                false,
                                            ) {
                                                Ok(report) => format_sync_report(&report),
                                                Err(error) => format!("同步失败：{error}"),
                                            }
                                        });
                                    mcp_message = format!(
                                        "{}\n\n{}",
                                        message,
                                        elapsed_message("同步完成", elapsed)
                                    );
                                }
                            }
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::Skills => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Menu;
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        skill_selected = skill_selected.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Down => {
                        if skill_selected + 1 < skills.len() {
                            skill_selected += 1;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Char('r') => {
                        (skills, skill_error) = load_skills();
                        skill_selected = skill_selected.min(skills.len().saturating_sub(1));
                        need_redraw = true;
                    }
                    KeyCode::Char('i') => {
                        skill_form = SkillForm {
                            kind: SkillFormKind::Local,
                            branch: "main".to_string(),
                            ..SkillForm::default()
                        };
                        mode = Mode::SkillForm;
                        need_redraw = true;
                    }
                    KeyCode::Char('z') => {
                        skill_form = SkillForm {
                            kind: SkillFormKind::Zip,
                            branch: "main".to_string(),
                            ..SkillForm::default()
                        };
                        mode = Mode::SkillForm;
                        need_redraw = true;
                    }
                    KeyCode::Char('g') => {
                        skill_form = SkillForm {
                            kind: SkillFormKind::Github,
                            branch: "main".to_string(),
                            ..SkillForm::default()
                        };
                        mode = Mode::SkillForm;
                        need_redraw = true;
                    }
                    KeyCode::Char('u') => {
                        if let Some(skill) = skills.get(skill_selected) {
                            pending_skill_args =
                                vec!["skill".to_string(), "update".to_string(), skill.id.clone()];
                            let (message, elapsed) = run_with_busy(
                                &mut terminal,
                                "正在预览 Skills 更新…",
                                || run_skill_command(&home, &pending_skill_args),
                            );
                            skill_message = format!(
                                "{}\n\n{}",
                                message,
                                elapsed_message("Skills 更新预览完成", elapsed)
                            );
                            mode = Mode::SkillConfirm;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Char('d') => {
                        if let Some(skill) = skills.get(skill_selected) {
                            pending_skill_args = vec![
                                "skill".to_string(),
                                "uninstall".to_string(),
                                skill.id.clone(),
                            ];
                            skill_message = run_skill_command(&home, &pending_skill_args);
                            mode = Mode::SkillConfirm;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Char('b') => {
                        let (message, elapsed) =
                            run_with_busy(&mut terminal, "正在读取 Skills 备份…", || {
                                run_skill_command(
                                    &home,
                                    &["skill".to_string(), "backups".to_string()],
                                )
                            });
                        skill_message = format!(
                            "{}\n\n{}",
                            message,
                            elapsed_message("Skills 备份列表完成", elapsed)
                        );
                        mode = Mode::Skills;
                        need_redraw = true;
                    }
                    KeyCode::Char('v') => {
                        let (message, elapsed) =
                            run_with_busy(&mut terminal, "正在校验 Skills 内容…", || {
                                run_skill_command(
                                    &home,
                                    &["skill".to_string(), "verify".to_string()],
                                )
                            });
                        skill_message = format!(
                            "{}\n\n{}",
                            message,
                            elapsed_message("Skills 校验完成", elapsed)
                        );
                        mode = Mode::Skills;
                        need_redraw = true;
                    }
                    KeyCode::Char(c @ '1'..='3') => {
                        let target = match c {
                            '1' => AgentTarget::OpenCode,
                            '2' => AgentTarget::ClaudeCode,
                            _ => AgentTarget::Codex,
                        };
                        if let Some(skill) = skills.get(skill_selected) {
                            pending_skill_args = skill_toggle_args(skill, target);
                            skill_message = run_skill_command(&home, &pending_skill_args);
                            mode = Mode::SkillConfirm;
                            need_redraw = true;
                        }
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(action) = ui::screens::skills::skill_mouse_action(
                        screen_area,
                        skill_selected,
                        skills.len(),
                        &m,
                    ) {
                        match action {
                            ui::screens::skills::SkillMouseAction::Select(index) => {
                                skill_selected = index
                            }
                            ui::screens::skills::SkillMouseAction::ImportLocal => {
                                skill_form = SkillForm {
                                    kind: SkillFormKind::Local,
                                    branch: "main".to_string(),
                                    ..SkillForm::default()
                                };
                                mode = Mode::SkillForm;
                            }
                            ui::screens::skills::SkillMouseAction::InstallZip => {
                                skill_form = SkillForm {
                                    kind: SkillFormKind::Zip,
                                    branch: "main".to_string(),
                                    ..SkillForm::default()
                                };
                                mode = Mode::SkillForm;
                            }
                            ui::screens::skills::SkillMouseAction::InstallGithub => {
                                skill_form = SkillForm {
                                    kind: SkillFormKind::Github,
                                    branch: "main".to_string(),
                                    ..SkillForm::default()
                                };
                                mode = Mode::SkillForm;
                            }
                            ui::screens::skills::SkillMouseAction::Update => {
                                if let Some(skill) = skills.get(skill_selected) {
                                    pending_skill_args = vec![
                                        "skill".to_string(),
                                        "update".to_string(),
                                        skill.id.clone(),
                                    ];
                                    let (message, elapsed) = run_with_busy(
                                        &mut terminal,
                                        "正在预览 Skills 更新…",
                                        || run_skill_command(&home, &pending_skill_args),
                                    );
                                    skill_message = format!(
                                        "{}\n\n{}",
                                        message,
                                        elapsed_message("Skills 更新预览完成", elapsed)
                                    );
                                    mode = Mode::SkillConfirm;
                                }
                            }
                            ui::screens::skills::SkillMouseAction::Uninstall => {
                                if let Some(skill) = skills.get(skill_selected) {
                                    pending_skill_args = vec![
                                        "skill".to_string(),
                                        "uninstall".to_string(),
                                        skill.id.clone(),
                                    ];
                                    skill_message = run_skill_command(&home, &pending_skill_args);
                                    mode = Mode::SkillConfirm;
                                }
                            }
                            ui::screens::skills::SkillMouseAction::Backups => {
                                let (message, elapsed) = run_with_busy(
                                    &mut terminal,
                                    "正在读取 Skills 备份…",
                                    || {
                                        run_skill_command(
                                            &home,
                                            &["skill".to_string(), "backups".to_string()],
                                        )
                                    },
                                );
                                skill_message = format!(
                                    "{}\n\n{}",
                                    message,
                                    elapsed_message("Skills 备份列表完成", elapsed)
                                );
                                mode = Mode::Skills;
                            }
                            ui::screens::skills::SkillMouseAction::Verify => {
                                let (message, elapsed) = run_with_busy(
                                    &mut terminal,
                                    "正在校验 Skills 内容…",
                                    || {
                                        run_skill_command(
                                            &home,
                                            &["skill".to_string(), "verify".to_string()],
                                        )
                                    },
                                );
                                skill_message = format!(
                                    "{}\n\n{}",
                                    message,
                                    elapsed_message("Skills 校验完成", elapsed)
                                );
                                mode = Mode::Skills;
                            }
                            ui::screens::skills::SkillMouseAction::Toggle(index, target) => {
                                skill_selected = index;
                                if let Some(skill) = skills.get(index) {
                                    pending_skill_args = skill_toggle_args(skill, target);
                                    skill_message = run_skill_command(&home, &pending_skill_args);
                                    mode = Mode::SkillConfirm;
                                }
                            }
                            ui::screens::skills::SkillMouseAction::SyncTarget(target) => {
                                if skills.is_empty() {
                                    skill_message = "暂无 Skill 可同步".to_string();
                                } else if target == AgentTarget::OpenCode {
                                    skill_message =
                                        "OpenCode 为源端，无需同步（其余两段为目标）".to_string();
                                } else {
                                    let (message, elapsed) = run_with_busy(
                                        &mut terminal,
                                        "正在同步 Skills…",
                                        || match trivium::sync::sync_skills(
                                            &home,
                                            AgentTarget::OpenCode,
                                            &[target],
                                            false,
                                        ) {
                                            Ok(report) => format_sync_report(&report),
                                            Err(error) => format!("同步失败：{error}"),
                                        },
                                    );
                                    skill_message = format!(
                                        "{}\n\n{}",
                                        message,
                                        elapsed_message("同步完成", elapsed)
                                    );
                                }
                            }
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::SkillForm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Skills;
                        need_redraw = true;
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        let max = skill_form_fields(&skill_form).len().saturating_sub(1);
                        skill_form.field = (skill_form.field + 1).min(max);
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        skill_form.field = skill_form.field.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Enter => {
                        match skill_form_preview(&skill_form) {
                            Ok((args, preview)) => {
                                pending_skill_args = args;
                                skill_message = preview;
                                skill_form.error = None;
                                mode = Mode::SkillFormConfirm;
                            }
                            Err(error) => skill_form.error = Some(error),
                        }
                        need_redraw = true;
                    }
                    KeyCode::Backspace => {
                        skill_form_pop(&mut skill_form);
                        need_redraw = true;
                    }
                    KeyCode::Char(c) => {
                        skill_form_push(&mut skill_form, c);
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    let size = terminal
                        .size()
                        .unwrap_or(ratatui::layout::Size::new(80, 24));
                    let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                    let fields = skill_form_fields(&skill_form).len();
                    if let Some(action) =
                        ui::screens::common::text_form_mouse_action(area, fields, &m)
                    {
                        match action {
                            ui::screens::forms::TextFormAction::Cancel => mode = Mode::Skills,
                            ui::screens::forms::TextFormAction::Preview => {
                                match skill_form_preview(&skill_form) {
                                    Ok((args, preview)) => {
                                        pending_skill_args = args;
                                        skill_message = preview;
                                        skill_form.error = None;
                                        mode = Mode::SkillFormConfirm;
                                    }
                                    Err(error) => skill_form.error = Some(error),
                                }
                            }
                            ui::screens::forms::TextFormAction::Field(field) => {
                                skill_form.field = field
                            }
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::SkillFormConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        pending_skill_args.push("--yes".to_string());
                        let label = match skill_form.kind {
                            SkillFormKind::Local => "正在安装本地 Skills…",
                            SkillFormKind::Zip => "正在安装压缩包 Skills…",
                            SkillFormKind::Github => "正在从仓库安装 Skills…",
                        };
                        let (message, elapsed) = run_with_busy(&mut terminal, label, || {
                            run_skill_command(&home, &pending_skill_args)
                        });
                        skill_message = format!(
                            "{}\n\n{}",
                            message,
                            elapsed_message("Skills 安装完成", elapsed)
                        );
                        pending_skill_args.clear();
                        (skills, skill_error) = load_skills();
                        skill_selected = skill_selected.min(skills.len().saturating_sub(1));
                        mode = Mode::Skills;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        pending_skill_args.clear();
                        mode = Mode::SkillForm;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            pending_skill_args.push("--yes".to_string());
                            let label = match skill_form.kind {
                                SkillFormKind::Local => "正在安装本地 Skills…",
                                SkillFormKind::Zip => "正在安装压缩包 Skills…",
                                SkillFormKind::Github => "正在从仓库安装 Skills…",
                            };
                            let (message, elapsed) = run_with_busy(&mut terminal, label, || {
                                run_skill_command(&home, &pending_skill_args)
                            });
                            skill_message = format!(
                                "{}\n\n{}",
                                message,
                                elapsed_message("Skills 安装完成", elapsed)
                            );
                            pending_skill_args.clear();
                            (skills, skill_error) = load_skills();
                            skill_selected = skill_selected.min(skills.len().saturating_sub(1));
                            mode = Mode::Skills;
                            need_redraw = true;
                        } else {
                            pending_skill_args.clear();
                            mode = Mode::SkillForm;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::SkillConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        pending_skill_args.push("--yes".to_string());
                        let label = if pending_skill_args
                            .get(1)
                            .map(String::as_str)
                            .is_some_and(|value| value == "update")
                        {
                            "正在更新 Skills…"
                        } else if pending_skill_args
                            .get(1)
                            .map(String::as_str)
                            .is_some_and(|value| value == "uninstall")
                        {
                            "正在卸载 Skills…"
                        } else {
                            "正在应用 Skills 修改…"
                        };
                        let (message, elapsed) = run_with_busy(&mut terminal, label, || {
                            run_skill_command(&home, &pending_skill_args)
                        });
                        skill_message = format!(
                            "{}\n\n{}",
                            message,
                            elapsed_message("Skills 操作完成", elapsed)
                        );
                        pending_skill_args.clear();
                        (skills, skill_error) = load_skills();
                        skill_selected = skill_selected.min(skills.len().saturating_sub(1));
                        mode = Mode::Skills;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        pending_skill_args.clear();
                        mode = Mode::Skills;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            pending_skill_args.push("--yes".to_string());
                            let label = if pending_skill_args
                                .get(1)
                                .map(String::as_str)
                                .is_some_and(|value| value == "update")
                            {
                                "正在更新 Skills…"
                            } else if pending_skill_args
                                .get(1)
                                .map(String::as_str)
                                .is_some_and(|value| value == "uninstall")
                            {
                                "正在卸载 Skills…"
                            } else {
                                "正在应用 Skills 修改…"
                            };
                            let (message, elapsed) = run_with_busy(&mut terminal, label, || {
                                run_skill_command(&home, &pending_skill_args)
                            });
                            skill_message = format!(
                                "{}\n\n{}",
                                message,
                                elapsed_message("Skills 操作完成", elapsed)
                            );
                            pending_skill_args.clear();
                            (skills, skill_error) = load_skills();
                            skill_selected = skill_selected.min(skills.len().saturating_sub(1));
                            mode = Mode::Skills;
                            need_redraw = true;
                        } else {
                            pending_skill_args.clear();
                            mode = Mode::Skills;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::McpForm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Mcp;
                        need_redraw = true;
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        let max = mcp_form_fields(&mcp_form).len().saturating_sub(1);
                        mcp_form.field = (mcp_form.field + 1).min(max);
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        mcp_form.field = mcp_form.field.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Enter => {
                        match mcp_form_preview(&mcp_form) {
                            Ok((args, preview)) => {
                                pending_mcp_args = args;
                                mcp_message = preview;
                                mcp_form.error = None;
                                mode = Mode::McpFormConfirm;
                            }
                            Err(error) => mcp_form.error = Some(error),
                        }
                        need_redraw = true;
                    }
                    KeyCode::Backspace => {
                        mcp_form_pop(&mut mcp_form);
                        need_redraw = true;
                    }
                    KeyCode::Char(c) => {
                        mcp_form_push(&mut mcp_form, c);
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    let size = terminal
                        .size()
                        .unwrap_or(ratatui::layout::Size::new(80, 24));
                    let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                    let fields = mcp_form_fields(&mcp_form).len();
                    if let Some(action) =
                        ui::screens::common::text_form_mouse_action(area, fields, &m)
                    {
                        match action {
                            ui::screens::forms::TextFormAction::Cancel => mode = Mode::Mcp,
                            ui::screens::forms::TextFormAction::Preview => {
                                match mcp_form_preview(&mcp_form) {
                                    Ok((args, preview)) => {
                                        pending_mcp_args = args;
                                        mcp_message = preview;
                                        mcp_form.error = None;
                                        mode = Mode::McpFormConfirm;
                                    }
                                    Err(error) => mcp_form.error = Some(error),
                                }
                            }
                            ui::screens::forms::TextFormAction::Field(field) => {
                                mcp_form.field = field
                            }
                        }
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::McpFormConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        pending_mcp_args.push("--yes".to_string());
                        let label = match mcp_form.kind {
                            McpFormKind::Create => "正在创建 MCP 服务…",
                            McpFormKind::Edit => "正在更新 MCP 服务…",
                        };
                        let (message, elapsed) = run_with_busy(&mut terminal, label, || {
                            run_mcp_command(&home, &pending_mcp_args)
                        });
                        mcp_message = format!(
                            "{}\n\n{}",
                            message,
                            elapsed_message("MCP 写入完成", elapsed)
                        );
                        pending_mcp_args.clear();
                        (mcp_servers, mcp_error) = load_mcp_servers();
                        mcp_selected = mcp_selected.min(mcp_servers.len().saturating_sub(1));
                        // A1：结果作为状态消息带回列表，不再独立一屏。
                        mode = Mode::Mcp;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        pending_mcp_args.clear();
                        mode = Mode::McpForm;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            pending_mcp_args.push("--yes".to_string());
                            let label = match mcp_form.kind {
                                McpFormKind::Create => "正在创建 MCP 服务…",
                                McpFormKind::Edit => "正在更新 MCP 服务…",
                            };
                            let (message, elapsed) = run_with_busy(&mut terminal, label, || {
                                run_mcp_command(&home, &pending_mcp_args)
                            });
                            mcp_message = format!(
                                "{}\n\n{}",
                                message,
                                elapsed_message("MCP 写入完成", elapsed)
                            );
                            pending_mcp_args.clear();
                            (mcp_servers, mcp_error) = load_mcp_servers();
                            mcp_selected = mcp_selected.min(mcp_servers.len().saturating_sub(1));
                            mode = Mode::Mcp;
                            need_redraw = true;
                        } else {
                            pending_mcp_args.clear();
                            mode = Mode::McpForm;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::McpPresets => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc => {
                        mode = Mode::Mcp;
                        need_redraw = true;
                    }
                    KeyCode::Up => {
                        mcp_preset_selected = mcp_preset_selected.saturating_sub(1);
                        need_redraw = true;
                    }
                    KeyCode::Down => {
                        if mcp_preset_selected + 1 < trivium::mcp::MCP_PRESETS.len() {
                            mcp_preset_selected += 1;
                            need_redraw = true;
                        }
                    }
                    KeyCode::Enter => {
                        pending_mcp_args = mcp_preset_args(mcp_preset_selected);
                        mcp_message = run_mcp_command(&home, &pending_mcp_args);
                        mode = Mode::McpConfirm;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(index) = ui::screens::mcp::mcp_preset_hit_test(screen_area, &m) {
                        mcp_preset_selected = index;
                        pending_mcp_args = mcp_preset_args(index);
                        mcp_message = run_mcp_command(&home, &pending_mcp_args);
                        mode = Mode::McpConfirm;
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::McpConfirm => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        pending_mcp_args.push("--yes".to_string());
                        mcp_message = run_mcp_command(&home, &pending_mcp_args);
                        pending_mcp_args.clear();
                        (mcp_servers, mcp_error) = load_mcp_servers();
                        mcp_selected = mcp_selected.min(mcp_servers.len().saturating_sub(1));
                        // A1：结果作为状态消息带回列表，不再独立一屏。
                        mode = Mode::Mcp;
                        need_redraw = true;
                    }
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                        pending_mcp_args.clear();
                        mode = Mode::Mcp;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Mouse(m) => {
                    if let Some(ok) = ui::screens::common::confirm_mouse_action(&m) {
                        if ok {
                            pending_mcp_args.push("--yes".to_string());
                            mcp_message = run_mcp_command(&home, &pending_mcp_args);
                            pending_mcp_args.clear();
                            (mcp_servers, mcp_error) = load_mcp_servers();
                            mcp_selected = mcp_selected.min(mcp_servers.len().saturating_sub(1));
                            mode = Mode::Mcp;
                            need_redraw = true;
                        } else {
                            pending_mcp_args.clear();
                            mode = Mode::Mcp;
                            need_redraw = true;
                        }
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::AgentDoctor => match ev {
                tui::AppEvent::Key(k) => {
                    if k.code == KeyCode::Esc {
                        mode = Mode::Menu;
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::WebNotice => match ev {
                tui::AppEvent::Key(k) => {
                    if k.code == KeyCode::Esc {
                        toggle_web(
                            &home,
                            &mut connection_result,
                            &mut web_running,
                            &mut web_stop,
                            &mut web_notice,
                            &mut prev_mode_for_web,
                            &mut mode,
                        );
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Tick => {
                    // Web 页「切回 TUI」：服务端置位 stop 标志 → 自动回原页。
                    if web_stop_should_return(&web_stop) {
                        web_running = false;
                        mode = prev_mode_for_web;
                        web_notice.clear();
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            Mode::Help => match ev {
                tui::AppEvent::Key(k) => match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        mode = Mode::Menu;
                        need_redraw = true;
                    }
                    _ => {}
                },
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
            _ => match ev {
                tui::AppEvent::Key(k) => {
                    if k.code == KeyCode::Esc {
                        mode = Mode::Menu;
                        need_redraw = true;
                    }
                }
                tui::AppEvent::Resize => need_redraw = true,
                _ => {}
            },
        }
    }

    let _ = tui::restore();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_scroll_delta_maps_scroll_events_only() {
        let up = tui::AppEvent::Key(crossterm::event::KeyEvent::new(
            KeyCode::Up,
            crossterm::event::KeyModifiers::NONE,
        ));
        let down = tui::AppEvent::Key(crossterm::event::KeyEvent::new(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        let esc = tui::AppEvent::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        let wheel = tui::AppEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(preview_scroll_delta(&up), Some(-1));
        assert_eq!(preview_scroll_delta(&down), Some(1));
        assert_eq!(preview_scroll_delta(&esc), None);
        assert_eq!(preview_scroll_delta(&wheel), Some(3));
    }

    #[test]
    fn preview_modes_include_placeholder_screens() {
        assert!(is_preview_mode(Mode::PlanPreview));
        assert!(is_preview_mode(Mode::AgentDoctor));
        assert!(!is_preview_mode(Mode::Menu));
        assert!(!is_preview_mode(Mode::Provider));
        assert!(!is_preview_mode(Mode::Client));
    }

    #[test]
    fn activate_tab_maps_five_tabs_to_five_modes() {
        assert!(matches!(
            activate_tab(ui::screens::chrome::Tab::Providers),
            Mode::Provider
        ));
        assert!(matches!(
            activate_tab(ui::screens::chrome::Tab::Agent),
            Mode::Client
        ));
        assert!(matches!(
            activate_tab(ui::screens::chrome::Tab::Mcp),
            Mode::Mcp
        ));
        assert!(matches!(
            activate_tab(ui::screens::chrome::Tab::Skills),
            Mode::Skills
        ));
        assert!(matches!(
            activate_tab(ui::screens::chrome::Tab::Sessions),
            Mode::Session
        ));
    }

    #[test]
    fn web_toggle_helpers_set_flag_correctly() {
        let stop = Arc::new(AtomicBool::new(false));
        assert!(!web_stop_should_return(&stop));
        stop.store(true, Ordering::Relaxed);
        assert!(web_stop_should_return(&stop));
    }

    #[test]
    fn web_notice_stop_returns_to_previous_mode() {
        let mut running = true;
        let mut stop = Arc::new(AtomicBool::new(false));
        let mut notice = "Web 控制台已启动".to_string();
        let mut prev = Mode::Provider;
        let mut mode = Mode::WebNotice;
        let home = tempfile::tempdir().unwrap();
        toggle_web(
            home.path(),
            &mut String::new(),
            &mut running,
            &mut stop,
            &mut notice,
            &mut prev,
            &mut mode,
        );
        assert!(!running);
        assert!(mode == Mode::Provider);
        assert!(web_stop_should_return(&stop));
        assert!(notice.is_empty());
    }

    #[test]
    fn detail_click_enters_detail_on_first_click() {
        let mut state = ProviderState {
            providers: Vec::new(),
            error: None,
            sync_notice: None,
            selected: 0,
            current: BTreeMap::new(),
            health: BTreeMap::new(),
        };
        let mut provider_mode = ui::screens::providers::ProviderMode::Single;
        let mut add_form = ProviderAddForm::default();
        let mut preset_idx = 0usize;
        let mut multi_ids: Vec<String> = Vec::new();
        let mut multi_cursor = 0usize;
        let mut multi_target = AgentTarget::Codex;
        let mut mode = Mode::Provider;

        apply_provider_mouse_action(
            ui::screens::providers::ProviderMouseAction::Detail(1),
            &mut state,
            &mut provider_mode,
            &mut add_form,
            &mut preset_idx,
            &mut multi_ids,
            &mut multi_cursor,
            &mut multi_target,
            &mut mode,
        );

        assert_eq!(state.selected, 1);
        assert!(matches!(mode, Mode::ProviderDetail));
    }

    #[test]
    fn skill_form_builds_zip_and_github_args() {
        let zip = SkillForm {
            kind: SkillFormKind::Zip,
            id: "zip-skill".to_string(),
            name: "Zip Skill".to_string(),
            path: "/tmp/skill.zip".to_string(),
            ..SkillForm::default()
        };
        let args = skill_form_args(&zip).unwrap();
        assert_eq!(args[1], "install-zip");
        assert!(args.contains(&"--path".to_string()));

        let github = SkillForm {
            kind: SkillFormKind::Github,
            id: "gh-skill".to_string(),
            owner: "acme".to_string(),
            repo: "skills".to_string(),
            branch: "main".to_string(),
            subdir: "tools/demo".to_string(),
            ..SkillForm::default()
        };
        let args = skill_form_args(&github).unwrap();
        assert_eq!(args[1], "install-github");
        assert!(args.contains(&"--owner".to_string()));
        assert!(args.contains(&"--subdir".to_string()));
    }

    #[test]
    fn skill_form_rejects_invalid_id() {
        let form = SkillForm {
            kind: SkillFormKind::Local,
            id: "bad id".to_string(),
            path: "/tmp/skill".to_string(),
            ..SkillForm::default()
        };
        assert!(skill_form_args(&form).is_err());
    }

    #[test]
    fn mcp_form_builds_stdio_and_http_args() {
        let stdio = McpForm {
            kind: McpFormKind::Create,
            id: "memory".to_string(),
            name: "Memory".to_string(),
            transport: "stdio".to_string(),
            command: "npx".to_string(),
            args: "-y @modelcontextprotocol/server-memory".to_string(),
            env: "MODE=safe".to_string(),
            ..McpForm::default()
        };
        let args = mcp_form_args(&stdio).unwrap();
        assert_eq!(args[1], "add");
        assert!(args.contains(&"--command".to_string()));
        assert!(args.contains(&"--arg".to_string()));
        assert!(args.contains(&"--env".to_string()));

        let http = McpForm {
            kind: McpFormKind::Edit,
            id: "remote".to_string(),
            name: "Remote".to_string(),
            transport: "http".to_string(),
            url: "https://example.com/mcp".to_string(),
            headers: "Authorization=Bearer token".to_string(),
            ..McpForm::default()
        };
        let args = mcp_form_args(&http).unwrap();
        assert_eq!(args[1], "update");
        assert!(args.contains(&"--url".to_string()));
        assert!(args.contains(&"--header".to_string()));
    }

    #[test]
    fn mcp_form_rejects_invalid_transport() {
        let form = McpForm {
            kind: McpFormKind::Create,
            id: "x".to_string(),
            transport: "grpc".to_string(),
            ..McpForm::default()
        };
        assert!(mcp_form_args(&form).is_err());
    }

    #[test]
    fn provider_add_args_redacts_from_preview_fields_and_splits_models() {
        let form = ProviderAddForm {
            id: "demo".to_string(),
            name: String::new(),
            kind_index: 1,
            base_url: "https://example.com/v1".to_string(),
            api_key: "secret".to_string(),
            models: "gpt-4.1, o3".to_string(),
            ..ProviderAddForm::default()
        };
        let args = provider_add_args(&form, true).expect("valid provider add args");
        assert!(args.contains(&"--预览".to_string()));
        assert!(args.contains(&"responses".to_string()));
        assert_eq!(
            split_provider_models(&form.models).unwrap(),
            ["gpt-4.1", "o3"]
        );
    }

    #[test]
    fn provider_editor_adjusts_touch_fields_with_bounds() {
        let mut form = ProviderAddForm {
            timeout_ms: 60000,
            max_retries: 10,
            context_window: 128000,
            max_output_tokens: 32768,
            ..ProviderAddForm::default()
        };

        form.field = 7;
        adjust_provider_edit_field(&mut form, true);
        assert_eq!(form.timeout_ms, 75000);
        form.field = 8;
        adjust_provider_edit_field(&mut form, false);
        assert_eq!(form.max_retries, 9);
        form.field = 11;
        adjust_provider_edit_field(&mut form, true);
        assert_eq!(provider_reasoning(&form), "low");
    }

    #[test]
    fn provider_reasoning_cycles_five_levels_including_max() {
        let mut form = ProviderAddForm {
            reasoning_index: 0,
            ..ProviderAddForm::default()
        };
        form.field = 11;
        let expected = ["medium", "low", "high", "xhigh", "max", "medium"];
        for (step, want) in expected.iter().enumerate() {
            if step > 0 {
                adjust_provider_edit_field(&mut form, true);
            }
            assert_eq!(
                provider_reasoning(&form),
                *want,
                "step {step}: 应循环到 {want}"
            );
        }
        // 后退一格：medium → max（5 档循环）
        adjust_provider_edit_field(&mut form, false);
        assert_eq!(provider_reasoning(&form), "max");
        // 已有 max 档位配置能正确解析回索引
        assert_eq!(reasoning_index(Some("max")), 4);
        assert_eq!(reasoning_index(None), 0);
    }

    #[test]
    fn provider_editor_parses_semicolon_headers() {
        assert_eq!(
            split_provider_headers("X-App: xu; User-Agent: Xu Mobile").unwrap(),
            ["X-App: xu", "User-Agent: Xu Mobile"]
        );
        assert!(split_provider_headers("broken").is_err());
        assert_eq!(
            redact_headers_for_display("Authorization: Bearer secret; X-App: xu"),
            "Authorization: <hidden>; X-App: xu"
        );
    }
}
