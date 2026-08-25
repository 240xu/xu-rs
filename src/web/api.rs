//! Web console JSON API handlers (read-only introspection + command
//! passthrough into the shared in-process CLI backend). Every handler
//! returns `(status, JSON value)`; response bodies never contain secrets
//! (API keys, Authorization headers, or full config files) — the provider
//! endpoint only exposes id/name/protocol/models/default/endpoint/health.

use std::path::Path;

use serde_json::{json, Value};

/// Ordered set of the three main agents surfaced by `/api/overview`.
const OVERVIEW_AGENT_KEYS: [&str; 3] = ["opencode", "claude", "codex"];

/// GET /api/overview — three-agent status (no network; `query_latest` is
/// off) + runtime health probe. Spec target: <50ms without network calls.
pub fn overview(home: &Path) -> (u16, Value) {
    let statuses = crate::agent_tools::status_rows(home, false);
    let mut agents = Vec::with_capacity(OVERVIEW_AGENT_KEYS.len());
    for key in OVERVIEW_AGENT_KEYS {
        match statuses.iter().find(|status| status.tool.key == key) {
            Some(status) => agents.push(json!({
                "name": key,
                "ready": status.ready,
                "current_version": status.current_version,
                "latest_version": status.latest_version,
            })),
            None => agents.push(json!({
                "name": key,
                "ready": false,
                "current_version": null,
                "latest_version": null,
            })),
        }
    }
    (
        200,
        json!({
            "ok": true,
            "data": {
                "agents": agents,
                "runtime": { "running": crate::runtime::is_running() },
            },
        }),
    )
}

/// GET /api/providers — structured provider list (read-only, no secrets).
pub fn providers(home: &Path) -> (u16, Value) {
    let path = crate::config::providers_path();
    let profiles = match crate::providers::read_profiles(&path) {
        Ok(profiles) => profiles,
        Err(error) => {
            return (500, json!({ "ok": false, "error": error }));
        }
    };
    let state = crate::state::read_state(home).unwrap_or_default();
    let items: Vec<Value> = profiles
        .iter()
        .map(|profile| {
            let health_active = state
                .provider_health
                .get(&profile.id)
                .map(|health| health.ok)
                .unwrap_or(false);
            json!({
                "id": profile.id,
                "name": profile.name,
                "protocol": profile.protocol.as_str(),
                "models": profile.models,
                "default_model": profile.default_model,
                "endpoint": profile.base_url,
                "health_active": health_active,
            })
        })
        .collect();
    (200, json!({ "ok": true, "data": items }))
}

/// POST /api/command — `{"args": ["provider", "list"]}` passthrough to the
/// shared CLI backend so every TUI page command is also available in the web
/// console with identical semantics.
pub fn command(home: &Path, body: &[u8]) -> (u16, Value) {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => {
            return (
                400,
                json!({ "ok": false, "error": format!("invalid JSON body: {error}") }),
            );
        }
    };
    let Some(args) = value.get("args").and_then(Value::as_array) else {
        return (
            400,
            json!({ "ok": false, "error": "body must be {\"args\": [\"...\", \"...\"]}" }),
        );
    };
    let mut args_as_strings = Vec::with_capacity(args.len());
    for arg in args {
        match arg.as_str() {
            Some(text) => args_as_strings.push(text.to_string()),
            None => {
                return (
                    400,
                    json!({ "ok": false, "error": "args must be an array of strings" }),
                );
            }
        }
    }
    match crate::cli::run_command(home, &args_as_strings) {
        Some(Ok(output)) => (200, json!({ "ok": true, "output": output })),
        Some(Err(error)) => (200, json!({ "ok": false, "output": error })),
        None => (400, json!({ "ok": false, "error": "unknown command" })),
    }
}

/// GET /api/mcp — unified MCP server list with per-agent enablement.
pub fn mcp_list(home: &Path) -> (u16, Value) {
    let store = match crate::mcp::read_store(home) {
        Ok(store) => store,
        Err(error) => {
            return (500, json!({ "ok": false, "error": error }));
        }
    };
    let items: Vec<Value> = store
        .servers
        .values()
        .map(|server| {
            json!({
                "id": server.id,
                "name": server.name,
                "transport": server.transport,
                "command": server.command,
                "url": server.url,
                "description": server.description,
                "enabled": {
                    "opencode": server.targets.opencode,
                    "claude": server.targets.claude,
                    "codex": server.targets.codex,
                },
            })
        })
        .collect();
    (200, json!({ "ok": true, "data": items }))
}

/// GET /api/skills — skill list with per-agent enablement.
pub fn skills_list(home: &Path) -> (u16, Value) {
    let store = match crate::skills::read_store(home) {
        Ok(store) => store,
        Err(error) => {
            return (500, json!({ "ok": false, "error": error }));
        }
    };
    let items: Vec<Value> = store
        .skills
        .values()
        .map(|skill| {
            json!({
                "id": skill.id,
                "name": skill.name,
                "path": skill.path,
                "enabled": {
                    "opencode": skill.targets.opencode,
                    "claude": skill.targets.claude,
                    "codex": skill.targets.codex,
                },
            })
        })
        .collect();
    (200, json!({ "ok": true, "data": items }))
}

/// GET /api/stats — usage aggregation (always contains the four windows).
pub fn stats(home: &Path) -> (u16, Value) {
    let periods = crate::stats::aggregate(home);
    (200, json!({ "ok": true, "data": { "periods": periods } }))
}

/// GET /api/sessions — recent sessions from all three agents (cap 200).
pub fn sessions() -> (u16, Value) {
    let items: Vec<Value> = crate::session_store::list_sessions()
        .into_iter()
        .take(200)
        .map(|session| {
            json!({
                "source": session.source,
                "id": session.id,
                "title": session.title,
                "time": session.time,
            })
        })
        .collect();
    (200, json!({ "ok": true, "data": items }))
}

/// POST /api/web/stop — persist TUI as the next launch surface, then set the
/// serve stop flag so the current Web process exits after this connection.
/// 仅持久化「下次启动回 TUI」。置 stop 标志移到路由层响应写回之后，
/// 避免服务先于 200 响应退出导致前端误报失败。
pub fn web_stop(home: &Path) -> (u16, Value) {
    match crate::state::set_ui_surface(home, crate::state::UiSurface::Tui) {
        Ok(()) => (200, json!({ "ok": true })),
        // 写盘失败不阻止停服：否则存储异常时 Web 面永远无法离开（锁死）。
        Err(error) => (
            200,
            json!({ "ok": true, "warning": format!("已停止，但保存下次启动偏好失败：{error}") }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{preferred_ui_surface, set_ui_surface, UiSurface};

    #[test]
    fn stopping_web_persists_tui_even_when_state_write_fails() {
        let home = tempfile::tempdir().unwrap();
        set_ui_surface(home.path(), UiSurface::Web).unwrap();

        // 正常路径：落盘 Tui。
        let (status, body) = web_stop(home.path());
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        assert!(body.get("warning").is_none());
        assert_eq!(preferred_ui_surface(home.path()).unwrap(), UiSurface::Tui);

        // 存储异常路径：把 home 变成普通文件，使 .codex 无法创建 →
        // 持久化必然失败，但停止仍须 ok:true + warning（不锁死 Web 面）。
        let blocked = home.path().join("not-a-dir");
        std::fs::write(&blocked, b"x").unwrap();
        let (status2, body2) = web_stop(&blocked);
        assert_eq!(status2, 200);
        assert_eq!(body2["ok"], true);
        assert!(body2.get("warning").is_some());
    }
}
