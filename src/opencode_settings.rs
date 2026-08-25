use std::path::Path;

use serde_json::Value;

use crate::patch::{apply_patch, read_before_checked, ConfigPatch, PatchOptions, PatchResult};

pub const PERMISSION_MODES: &[&str] = &["allow", "ask", "deny"];

pub fn read_permission(home: &Path) -> Result<String, String> {
    let path = home.join(".config/opencode/opencode.json");
    let text = read_before_checked(&path)?;
    if text.trim().is_empty() {
        return Ok("ask".to_string());
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    Ok(value
        .get("permission")
        .and_then(Value::as_str)
        .unwrap_or("ask")
        .to_string())
}

pub fn set_permission(home: &Path, mode: &str, dry_run: bool) -> Result<PatchResult, String> {
    if !PERMISSION_MODES.contains(&mode) {
        return Err(format!(
            "invalid OpenCode permission: {mode}; expected allow|ask|deny"
        ));
    }
    let path = home.join(".config/opencode/opencode.json");
    let before = read_before_checked(&path)?;
    let mut value: Value = if before.trim().is_empty() {
        serde_json::json!({ "$schema": "https://opencode.ai/config.json" })
    } else {
        serde_json::from_str(&before)
            .map_err(|error| format!("parse {}: {error}", path.display()))?
    };
    let root = value
        .as_object_mut()
        .ok_or_else(|| format!("{} root must be an object", path.display()))?;
    root.insert("permission".to_string(), Value::String(mode.to_string()));
    let after = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())? + "\n";
    apply_patch(
        &ConfigPatch::new(path, before, after),
        PatchOptions {
            dry_run,
            backup: true,
        },
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn permission_update_preserves_unrelated_config() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".config/opencode/opencode.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"permission":"ask","theme":"custom","provider":{"p":{"npm":"other"}}}"#,
        )
        .unwrap();

        set_permission(home.path(), "allow", false).unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        assert_eq!(value["permission"], "allow");
        assert_eq!(value["theme"], "custom");
        assert_eq!(value["provider"]["p"]["npm"], "other");
    }

    #[test]
    fn permission_dry_run_does_not_write() {
        let home = tempfile::tempdir().unwrap();

        let result = set_permission(home.path(), "deny", true).unwrap();

        assert!(result.changed);
        assert!(!result.path.exists());
    }
}
