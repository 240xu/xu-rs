use crossterm::event::KeyCode;
use trivium::agent_tools::{status_rows, AgentToolStatus};

use super::{
    elapsed_message, ensure_agent_versions_for_clients, ensure_agent_versions_for_install,
    run_with_busy,
};
use crate::tui::{self, AppEvent, Tui};
use crate::Mode;

fn build_latest_check_message(statuses: &[AgentToolStatus], headline: &str) -> String {
    let mut msg = format!("{headline}\n");
    for row in statuses {
        msg.push_str(&format!(
            "- {}: {}\n",
            row.tool.label,
            trivium::agent_tools::version_transition_text(
                row.current_version.as_deref(),
                row.latest_version.as_deref(),
            )
        ));
    }
    msg
}

pub enum ClientOutcome {
    Cont {
        mode: Mode,
        need_redraw: bool,
    },
    /// 异步最新版查询中：事件循环不阻塞，完成后异步装填。
    PendingLatest {
        rx: std::sync::mpsc::Receiver<(Vec<AgentToolStatus>, std::time::Duration, String)>,
    },
    /// Exit TUI and run single agent install.
    RunInstall {
        statuses: Vec<AgentToolStatus>,
        index: usize,
    },
    /// Exit TUI and run OpenCode/Claude/Codex batch update.
    RunSetup,
}

pub struct ClientState<'a> {
    pub agent_idx: &'a mut usize,
    pub agent_statuses: &'a mut Vec<AgentToolStatus>,
    pub agent_status_message: &'a mut String,
    pub agent_pending_update: &'a mut Option<usize>,
    pub agent_pending_setup: &'a mut bool,
    pub opencode_permission: &'a mut String,
    pub pending_opencode_permission: &'a mut Option<String>,
    pub opencode_settings_message: &'a mut String,
    pub home: &'a std::path::Path,
    pub terminal: &'a mut Tui,
}

pub fn handle_client_event(ev: AppEvent, s: &mut ClientState<'_>) -> ClientOutcome {
    let mut need_redraw = false;
    let mode = Mode::Client;
    match ev {
        tui::AppEvent::Key(k) => match k.code {
            KeyCode::Esc => {
                *s.agent_pending_update = None;
                *s.agent_pending_setup = false;
                return ClientOutcome::Cont {
                    mode: Mode::Menu,
                    need_redraw: true,
                };
            }
            KeyCode::Up => {
                *s.agent_idx = s.agent_idx.saturating_sub(1);
                *s.agent_pending_update = None;
                *s.agent_pending_setup = false;
                need_redraw = true;
            }
            KeyCode::Down => {
                if *s.agent_idx + 1 < s.agent_statuses.len() {
                    *s.agent_idx += 1;
                }
                *s.agent_pending_update = None;
                *s.agent_pending_setup = false;
                need_redraw = true;
            }
            KeyCode::Enter => {
                // 二次确认：第一次进入待确认，第二次执行更新
                if *s.agent_pending_update == Some(*s.agent_idx) {
                    *s.agent_statuses = ensure_agent_versions_for_install(
                        s.home,
                        std::mem::take(s.agent_statuses),
                        *s.agent_idx,
                    );
                    return ClientOutcome::RunInstall {
                        statuses: s.agent_statuses.clone(),
                        index: *s.agent_idx,
                    };
                } else {
                    *s.agent_statuses = ensure_agent_versions_for_install(
                        s.home,
                        std::mem::take(s.agent_statuses),
                        *s.agent_idx,
                    );
                    *s.agent_pending_update = Some(*s.agent_idx);
                    *s.agent_pending_setup = false;
                    if let Some(row) = s.agent_statuses.get(*s.agent_idx) {
                        *s.agent_status_message = format!(
                            "再点一次确认更新 {}：{}",
                            row.tool.label,
                            trivium::agent_tools::version_transition_text(
                                row.current_version.as_deref(),
                                row.latest_version.as_deref(),
                            )
                        );
                    }
                    need_redraw = true;
                }
            }
            KeyCode::Char(c @ '1'..='3') => {
                let idx = (c as u8 - b'1') as usize;
                if idx < s.agent_statuses.len() {
                    if *s.agent_pending_update == Some(idx) {
                        *s.agent_idx = idx;
                        *s.agent_statuses = ensure_agent_versions_for_install(
                            s.home,
                            std::mem::take(s.agent_statuses),
                            idx,
                        );
                        return ClientOutcome::RunInstall {
                            statuses: s.agent_statuses.clone(),
                            index: idx,
                        };
                    } else {
                        *s.agent_idx = idx;
                        *s.agent_statuses = ensure_agent_versions_for_install(
                            s.home,
                            std::mem::take(s.agent_statuses),
                            idx,
                        );
                        *s.agent_pending_update = Some(idx);
                        *s.agent_pending_setup = false;
                        if let Some(row) = s.agent_statuses.get(idx) {
                            *s.agent_status_message = format!(
                                "再点一次确认更新 {}：{}",
                                row.tool.label,
                                trivium::agent_tools::version_transition_text(
                                    row.current_version.as_deref(),
                                    row.latest_version.as_deref(),
                                )
                            );
                        }
                    }
                    need_redraw = true;
                }
            }
            KeyCode::Char('r') => {
                *s.agent_pending_update = None;
                *s.agent_pending_setup = false;
                let (statuses, elapsed) =
                    run_with_busy(s.terminal, "正在刷新客户端状态…", || {
                        status_rows(s.home, false)
                    });
                *s.agent_statuses = statuses;
                *s.agent_status_message = elapsed_message("客户端状态已刷新", elapsed);
                need_redraw = true;
            }
            KeyCode::Char('l') | KeyCode::Char('v') => {
                *s.agent_pending_update = None;
                *s.agent_pending_setup = false;
                *s.agent_status_message = "正在查询最新版本…".to_string();
                let home_for_worker = s.home.to_path_buf();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let started = std::time::Instant::now();
                    let statuses = status_rows(&home_for_worker, true);
                    let elapsed = started.elapsed();
                    let msg = build_latest_check_message(
                        &statuses,
                        &format!("最新版本已查询 · 耗时 {:.2}s", elapsed.as_secs_f64()),
                    );
                    let _ = tx.send((statuses, elapsed, msg));
                });
                return ClientOutcome::PendingLatest { rx };
            }
            KeyCode::Char('d') => {
                *s.agent_pending_update = None;
                *s.agent_pending_setup = false;
                let (message, elapsed) = run_with_busy(s.terminal, "正在诊断环境…", || {
                    trivium::agent_tools::diagnostics(s.home)
                });
                *s.agent_status_message =
                    format!("{}\n\n{}", message, elapsed_message("诊断完成", elapsed));
                need_redraw = true;
            }
            KeyCode::Char('s') | KeyCode::Char('a') => {
                *s.agent_pending_update = None;
                if *s.agent_pending_setup {
                    let _ =
                        ensure_agent_versions_for_clients(s.home, std::mem::take(s.agent_statuses));
                    return ClientOutcome::RunSetup;
                }
                *s.agent_statuses =
                    ensure_agent_versions_for_clients(s.home, std::mem::take(s.agent_statuses));
                *s.agent_pending_setup = true;
                *s.agent_status_message = {
                    let mut msg = String::from("再点一次确认：更新 OpenCode / Claude / Codex\n");
                    for row in s.agent_statuses.iter() {
                        msg.push_str(&format!(
                            "- {}：{}\n",
                            row.tool.label,
                            trivium::agent_tools::version_transition_text(
                                row.current_version.as_deref(),
                                row.latest_version.as_deref(),
                            )
                        ));
                    }
                    msg
                };
                need_redraw = true;
            }
            _ => {}
        },
        tui::AppEvent::Mouse(m) => {
            let area = s
                .terminal
                .size()
                .map(|size| ratatui::layout::Rect::new(0, 0, size.width, size.height))
                .unwrap_or_else(|_| ratatui::layout::Rect::new(0, 0, 80, 24));
            if let Some(action) =
                crate::ui::screens::agents::agent_mouse_action(area, s.agent_statuses, &m)
            {
                match action {
                    crate::ui::screens::agents::AgentMouseAction::Refresh => {
                        *s.agent_pending_update = None;
                        *s.agent_pending_setup = false;
                        let (statuses, elapsed) =
                            run_with_busy(s.terminal, "正在刷新客户端状态…", || {
                                status_rows(s.home, false)
                            });
                        *s.agent_statuses = statuses;
                        *s.agent_status_message = elapsed_message("客户端状态已刷新", elapsed);
                    }
                    crate::ui::screens::agents::AgentMouseAction::Latest => {
                        *s.agent_pending_update = None;
                        *s.agent_pending_setup = false;
                        *s.agent_status_message = "正在查询最新版本…".to_string();
                        let home_for_worker = s.home.to_path_buf();
                        let (tx, rx) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let started = std::time::Instant::now();
                            let statuses = status_rows(&home_for_worker, true);
                            let elapsed = started.elapsed();
                            let msg = crate::app::client_page::build_latest_check_message(
                                &statuses,
                                &format!("最新版本已查询 · 耗时 {:.2}s", elapsed.as_secs_f64()),
                            );
                            let _ = tx.send((statuses, elapsed, msg));
                        });
                        return ClientOutcome::PendingLatest { rx };
                    }
                    crate::ui::screens::agents::AgentMouseAction::Doctor => {
                        *s.agent_pending_update = None;
                        *s.agent_pending_setup = false;
                        let (message, elapsed) =
                            run_with_busy(s.terminal, "正在诊断环境…", || {
                                trivium::agent_tools::diagnostics(s.home)
                            });
                        *s.agent_status_message =
                            format!("{}\n\n{}", message, elapsed_message("诊断完成", elapsed));
                    }
                    crate::ui::screens::agents::AgentMouseAction::Setup => {
                        *s.agent_pending_update = None;
                        if *s.agent_pending_setup {
                            let _ = ensure_agent_versions_for_clients(
                                s.home,
                                std::mem::take(s.agent_statuses),
                            );
                            return ClientOutcome::RunSetup;
                        }
                        *s.agent_statuses = ensure_agent_versions_for_clients(
                            s.home,
                            std::mem::take(s.agent_statuses),
                        );
                        *s.agent_pending_setup = true;
                        *s.agent_status_message = {
                            let mut msg =
                                String::from("再点一次确认：更新 OpenCode / Claude / Codex\n");
                            for row in s.agent_statuses.iter() {
                                msg.push_str(&format!(
                                    "- {}：{}\n",
                                    row.tool.label,
                                    trivium::agent_tools::version_transition_text(
                                        row.current_version.as_deref(),
                                        row.latest_version.as_deref(),
                                    )
                                ));
                            }
                            msg
                        };
                    }
                    crate::ui::screens::agents::AgentMouseAction::OpenCodeSettings => {
                        *s.agent_pending_update = None;
                        *s.agent_pending_setup = false;
                        *s.opencode_permission = trivium::opencode_settings::read_permission(s.home)
                            .unwrap_or_else(|_| "ask".to_string());
                        *s.pending_opencode_permission = None;
                        s.opencode_settings_message.clear();
                        return ClientOutcome::Cont {
                            mode: Mode::OpenCodeSettings,
                            need_redraw: true,
                        };
                    }
                    crate::ui::screens::agents::AgentMouseAction::Install(new_idx) => {
                        *s.agent_pending_setup = false;
                        if *s.agent_pending_update == Some(new_idx) {
                            let statuses = ensure_agent_versions_for_install(
                                s.home,
                                std::mem::take(s.agent_statuses),
                                new_idx,
                            );
                            return ClientOutcome::RunInstall {
                                statuses: statuses.clone(),
                                index: new_idx,
                            };
                        } else {
                            *s.agent_idx = new_idx;
                            *s.agent_statuses = ensure_agent_versions_for_install(
                                s.home,
                                std::mem::take(s.agent_statuses),
                                new_idx,
                            );
                            *s.agent_pending_update = Some(new_idx);
                            if let Some(row) = s.agent_statuses.get(new_idx) {
                                *s.agent_status_message = format!(
                                    "再点一次确认更新 {}：{}",
                                    row.tool.label,
                                    trivium::agent_tools::version_transition_text(
                                        row.current_version.as_deref(),
                                        row.latest_version.as_deref(),
                                    )
                                );
                            }
                        }
                    }
                }
                need_redraw = true;
            }
        }
        tui::AppEvent::Resize => need_redraw = true,
        _ => {}
    }
    ClientOutcome::Cont { mode, need_redraw }
}
