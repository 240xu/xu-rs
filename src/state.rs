use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::domain::AgentTarget;
use crate::patch::{atomic_write, read_before_checked, ConfigPatch, FileLock};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct XuState {
    #[serde(default)]
    pub current: BTreeMap<String, String>,
    #[serde(default)]
    pub provider_health: BTreeMap<String, ProviderHealth>,
    #[serde(default)]
    pub ui_surface: UiSurface,
}

/// 首选入口界面。写入状态文件，下一次执行 `spec` 时延续选择。
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiSurface {
    #[default]
    Tui,
    Web,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ProviderHealth {
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub checked_at: String,
    pub message: String,
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join(".codex/xu-state.json")
}

pub fn read_state(home: &Path) -> Result<XuState, String> {
    let path = state_path(home);
    if !path.exists() {
        return Ok(XuState::default());
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// 假定调用方已持有 FileLock；供锁内读改写复用（避免同进程二次 flock 自锁）。
fn write_state_unlocked(home: &Path, state: &XuState) -> Result<(), String> {
    let path = state_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // 调用方已持锁（mutate_state / write_state）；此处绝不能再 acquire，
    // 否则同进程第二个 fd 的 flock 会自冲突并超时。
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())? + "\n";
    atomic_write(&path, text.as_bytes()).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

pub fn write_state(home: &Path, state: &XuState) -> Result<(), String> {
    let path = state_path(home);
    let _lock = FileLock::acquire_with_timeout(&path, Duration::from_secs(5))
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    write_state_unlocked(home, state)
}

/// 锁内「读-改-写」模板：所有 set_* 必须走这里，防止并发整文件覆盖丢更新。
fn mutate_state<R>(home: &Path, mutate: impl FnOnce(&mut XuState) -> R) -> Result<R, String> {
    let path = state_path(home);
    let _lock = FileLock::acquire_with_timeout(&path, Duration::from_secs(5))
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    let mut state = read_state(home)?;
    let result = mutate(&mut state);
    write_state_unlocked(home, &state)?;
    Ok(result)
}

pub fn current_provider(home: &Path, target: AgentTarget) -> Result<Option<String>, String> {
    Ok(read_state(home)?.current.get(target.id()).cloned())
}

pub fn set_current_provider(
    home: &Path,
    target: AgentTarget,
    provider_id: &str,
) -> Result<(), String> {
    mutate_state(home, |state| {
        state
            .current
            .insert(target.id().to_string(), provider_id.to_string());
    })
}

pub fn preferred_ui_surface(home: &Path) -> Result<UiSurface, String> {
    Ok(read_state(home)?.ui_surface)
}

pub fn set_ui_surface(home: &Path, surface: UiSurface) -> Result<(), String> {
    mutate_state(home, |state| {
        state.ui_surface = surface;
    })
}

pub fn current_provider_patch(
    home: &Path,
    target: AgentTarget,
    provider_id: &str,
) -> Result<ConfigPatch, String> {
    let path = state_path(home);
    let before = read_before_checked(&path)?;
    let mut state = if before.trim().is_empty() {
        XuState::default()
    } else {
        serde_json::from_str(&before)
            .map_err(|error| format!("parse {}: {error}", path.display()))?
    };
    state
        .current
        .insert(target.id().to_string(), provider_id.to_string());
    let after = serde_json::to_string_pretty(&state).map_err(|error| error.to_string())? + "\n";
    Ok(ConfigPatch::new(path, before, after))
}

pub fn set_provider_health(
    home: &Path,
    provider_id: &str,
    api_key: &str,
    ok: bool,
    latency_ms: Option<u128>,
    message: &str,
) -> Result<(), String> {
    let safe_message = if api_key.is_empty() {
        message.to_string()
    } else {
        message.replace(api_key, "<redacted>")
    };
    mutate_state(home, |state| {
        state.provider_health.insert(
            provider_id.to_string(),
            ProviderHealth {
                ok,
                latency_ms: latency_ms.and_then(|value| u64::try_from(value).ok()),
                checked_at: chrono::Utc::now().to_rfc3339(),
                message: safe_message.clone(),
            },
        );
    })
}

pub fn remove_provider_health(home: &Path, provider_id: &str) -> Result<(), String> {
    mutate_state(home, |state| {
        let _removed = state.provider_health.remove(provider_id);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_current_provider_per_target() {
        let root = tempfile::tempdir().unwrap();

        set_current_provider(root.path(), AgentTarget::OpenCode, "zen").unwrap();
        set_current_provider(root.path(), AgentTarget::Codex, "deepseek").unwrap();

        assert_eq!(
            current_provider(root.path(), AgentTarget::OpenCode).unwrap(),
            Some("zen".to_string())
        );
        assert_eq!(
            current_provider(root.path(), AgentTarget::Codex).unwrap(),
            Some("deepseek".to_string())
        );
    }

    #[test]
    fn stores_preferred_ui_surface_across_processes() {
        let root = tempfile::tempdir().unwrap();

        set_ui_surface(root.path(), UiSurface::Web).unwrap();

        assert_eq!(preferred_ui_surface(root.path()).unwrap(), UiSurface::Web);
        assert!(fs::read_to_string(state_path(root.path()))
            .unwrap()
            .contains("\"ui_surface\": \"web\""));
    }

    #[test]
    fn stores_provider_health_without_api_key() {
        let root = tempfile::tempdir().unwrap();

        set_provider_health(
            root.path(),
            "zen",
            "secret-key",
            false,
            Some(123),
            "request using secret-key failed",
        )
        .unwrap();
        let state = read_state(root.path()).unwrap();
        let health = &state.provider_health["zen"];

        assert!(!health.ok);
        assert_eq!(health.latency_ms, Some(123));
        assert_eq!(health.message, "request using <redacted> failed");
        assert!(!fs::read_to_string(state_path(root.path()))
            .unwrap()
            .contains("secret-key"));
    }

    #[test]
    fn write_state_conflict_returns_clear_error() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let path = home.join(".codex/xu-state.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"current":{"opencode":"zen"}}"#).unwrap();
        let first = FileLock::try_acquire(&path).unwrap();
        let error = write_state(home, &XuState::default()).unwrap_err();
        drop(first);
        assert!(error.contains("请稍后重试"), "got: {error}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"current":{"opencode":"zen"}}"#,
            "conflicted write must not modify the file"
        );
    }

    #[test]
    fn reads_state_written_before_health_cache_existed() {
        let root = tempfile::tempdir().unwrap();
        let path = state_path(root.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"current":{"opencode":"zen"}}"#).unwrap();

        let state = read_state(root.path()).unwrap();

        assert_eq!(state.current["opencode"], "zen");
        assert!(state.provider_health.is_empty());
        assert_eq!(state.ui_surface, UiSurface::Tui);
    }
}
