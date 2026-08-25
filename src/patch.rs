use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::skills::{AppliedProjection, ProjectionPatch, ProjectionState};

/// Cross-process advisory lock for SSOT writes.
/// Lock file lives beside the target as `.<name>.spec.lock`.
pub struct FileLock {
    path: PathBuf,
    file: File,
}

impl FileLock {
    pub fn lock_path_for(target: &Path) -> PathBuf {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        let name = target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("config");
        parent.join(format!(".{name}.spec.lock"))
    }

    pub fn try_acquire(target: &Path) -> Result<Self, String> {
        let path = Self::lock_path_for(target);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建锁目录失败：{e}"))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("打开锁文件失败 {}：{e}", path.display()))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(format!(
                "配置正被其他 spec 进程写入：{}。请稍后重试。",
                target.display()
            ));
        }
        {
            use std::io::Write as _;
            let mut f = &file;
            let _ = writeln!(f, "pid={} time={}", std::process::id(), chrono_like_now());
            let _ = f.sync_all();
        }
        Ok(Self { path, file })
    }

    pub fn acquire_with_timeout(target: &Path, timeout: Duration) -> Result<Self, String> {
        let start = Instant::now();
        loop {
            match Self::try_acquire(target) {
                Ok(lock) => return Ok(lock),
                Err(error) if start.elapsed() >= timeout => return Err(error),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
        let _ = fs::remove_file(&self.path);
    }
}

fn chrono_like_now() -> String {
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}

#[derive(Clone, Debug)]
pub struct ConfigPatch {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
}

impl ConfigPatch {
    pub fn new(path: PathBuf, before: String, after: String) -> Self {
        Self {
            path,
            before,
            after,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PatchOptions {
    pub dry_run: bool,
    pub backup: bool,
}

#[derive(Clone, Debug)]
pub struct PatchResult {
    pub path: PathBuf,
    pub diff: String,
    pub backup_path: Option<PathBuf>,
    pub changed: bool,
}

#[derive(Clone, Debug)]
pub enum TransactionOperation {
    File(ConfigPatch),
    Skill(ProjectionPatch),
}

#[derive(Clone, Debug, Default)]
pub struct TransactionPlan {
    pub operations: Vec<TransactionOperation>,
}

#[derive(Clone, Debug)]
pub enum TransactionResult {
    File(PatchResult),
    Skill {
        id: String,
        destination: PathBuf,
        before: ProjectionState,
        after: ProjectionState,
        changed: bool,
    },
}

enum AppliedOperation {
    File {
        path: PathBuf,
        existed: bool,
        before: String,
    },
    Skill(AppliedProjection),
}

pub fn apply_transaction(
    plan: &TransactionPlan,
    dry_run: bool,
) -> Result<Vec<TransactionResult>, String> {
    preflight_transaction(plan)?;
    let mut results = Vec::new();
    let mut applied = Vec::new();
    for operation in &plan.operations {
        let outcome = match operation {
            TransactionOperation::File(patch) => {
                let existed = patch.path.exists();
                apply_patch(
                    patch,
                    PatchOptions {
                        dry_run,
                        backup: true,
                    },
                )
                .map(|result| {
                    let applied = (!dry_run && result.changed).then(|| AppliedOperation::File {
                        path: patch.path.clone(),
                        existed,
                        before: patch.before.clone(),
                    });
                    (TransactionResult::File(result), applied)
                })
                .map_err(|error| error.to_string())
            }
            TransactionOperation::Skill(patch) => {
                if dry_run {
                    Ok((
                        TransactionResult::Skill {
                            id: patch.id.clone(),
                            destination: patch.destination.clone(),
                            before: patch.before,
                            after: patch.after,
                            changed: patch.before != patch.after,
                        },
                        None,
                    ))
                } else {
                    crate::skills::apply_projection_patch(patch).map(|applied| {
                        (
                            TransactionResult::Skill {
                                id: patch.id.clone(),
                                destination: patch.destination.clone(),
                                before: patch.before,
                                after: patch.after,
                                changed: patch.before != patch.after,
                            },
                            applied.map(AppliedOperation::Skill),
                        )
                    })
                }
            }
        };
        match outcome {
            Ok((result, applied_operation)) => {
                results.push(result);
                if let Some(applied_operation) = applied_operation {
                    applied.push(applied_operation);
                }
            }
            Err(error) => {
                let rollback = rollback_transaction(applied);
                return Err(match rollback {
                    Ok(()) => format!("transaction apply failed: {error}"),
                    Err(rollback) => {
                        format!("transaction apply failed: {error}; rollback failed: {rollback}")
                    }
                });
            }
        }
    }
    if !dry_run {
        let cleanup_errors = applied
            .iter()
            .filter_map(|operation| match operation {
                AppliedOperation::Skill(applied) => crate::skills::finish_projection(applied).err(),
                AppliedOperation::File { .. } => None,
            })
            .collect::<Vec<_>>();
        if !cleanup_errors.is_empty() {
            return Err(format!(
                "transaction committed but cleanup failed: {}",
                cleanup_errors.join("; ")
            ));
        }
    }
    Ok(results)
}

fn preflight_transaction(plan: &TransactionPlan) -> Result<(), String> {
    let mut paths = std::collections::BTreeSet::new();
    for operation in &plan.operations {
        if let TransactionOperation::File(patch) = operation {
            if !paths.insert(patch.path.clone()) {
                return Err(format!(
                    "transaction contains duplicate uncomposed file path: {}",
                    patch.path.display()
                ));
            }
            let current = read_before_checked(&patch.path)?;
            if current != patch.before {
                return Err(format!(
                    "{} changed after preview; refresh before applying",
                    patch.path.display()
                ));
            }
        }
    }
    Ok(())
}

fn rollback_transaction(applied: Vec<AppliedOperation>) -> Result<(), String> {
    let mut errors = Vec::new();
    for operation in applied.into_iter().rev() {
        let result = match operation {
            AppliedOperation::File {
                path,
                existed,
                before,
            } => {
                if existed {
                    atomic_write(&path, before.as_bytes()).map_err(|error| error.to_string())
                } else {
                    match fs::remove_file(&path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(error.to_string()),
                    }
                }
            }
            AppliedOperation::Skill(applied) => crate::skills::rollback_projection(&applied),
        };
        if let Err(error) = result {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub fn read_before(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

pub fn read_before_checked(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

pub fn apply_patch(patch: &ConfigPatch, options: PatchOptions) -> io::Result<PatchResult> {
    let diff = unified_diff(&patch.before, &patch.after);
    if options.dry_run || patch.before == patch.after {
        return Ok(PatchResult {
            path: patch.path.clone(),
            diff,
            backup_path: None,
            changed: patch.before != patch.after,
        });
    }

    // Cross-process lock for real writes only.
    let _lock = FileLock::try_acquire(&patch.path).map_err(io::Error::other)?;

    if let Some(parent) = patch.path.parent() {
        fs::create_dir_all(parent)?;
    }

    let current = match fs::read_to_string(&patch.path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    if current != patch.before {
        return Err(io::Error::other(format!(
            "{} 在预览后被其他进程修改；请刷新后重新确认",
            patch.path.display()
        )));
    }

    let backup_path = if options.backup && patch.path.exists() {
        let backup = backup_path(&patch.path);
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&patch.path, &backup)?;
        set_private_permissions(&backup)?;
        Some(backup)
    } else {
        None
    };

    atomic_write(&patch.path, patch.after.as_bytes())?;

    Ok(PatchResult {
        path: patch.path.clone(),
        diff,
        backup_path,
        changed: true,
    })
}

pub fn restore_backup(backup: &Path, target: &Path) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if target.exists() {
        let rollback = backup_path(target);
        fs::copy(target, &rollback)?;
        set_private_permissions(&rollback)?;
    }
    let contents = fs::read(backup)?;
    atomic_write(target, &contents)?;
    Ok(())
}

fn set_private_permissions(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("config");
    let mut temp_path = None;
    let mut temp_file = None;
    for attempt in 0..1000_u32 {
        let candidate = parent.join(format!(
            ".{file_name}.xu-tmp-{}-{attempt}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temp_path = Some(candidate);
                temp_file = Some(file);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let temp_path = temp_path.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("could not allocate temporary file for {}", path.display()),
        )
    })?;
    let result = (|| {
        let mut file = temp_file.expect("temporary file exists with path");
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        set_private_permissions(path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("config")
        .to_string();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    name.push_str(&format!(".spec.{timestamp}.bak"));
    let candidate = path.with_file_name(&name);
    if !candidate.exists() {
        return candidate;
    }

    for index in 1.. {
        let candidate = path.with_file_name(format!("{name}.{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn unified_diff(before: &str, after: &str) -> String {
    let mut out = String::new();
    out.push_str("--- before\n+++ after\n");
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let max = before_lines.len().max(after_lines.len());
    for i in 0..max {
        match (before_lines.get(i), after_lines.get(i)) {
            (Some(a), Some(b)) if a == b => {
                out.push(' ');
                out.push_str(&mask_secret_line(a));
                out.push('\n');
            }
            (Some(a), Some(b)) => {
                out.push('-');
                out.push_str(&mask_secret_line(a));
                out.push('\n');
                out.push('+');
                out.push_str(&mask_secret_line(b));
                out.push('\n');
            }
            (Some(a), None) => {
                out.push('-');
                out.push_str(&mask_secret_line(a));
                out.push('\n');
            }
            (None, Some(b)) => {
                out.push('+');
                out.push_str(&mask_secret_line(b));
                out.push('\n');
            }
            (None, None) => {}
        }
    }
    out
}

fn mask_secret_line(line: &str) -> String {
    // JSON 对象行：值级递归脱敏（保留原文格式与键顺序）
    if matches!(
        serde_json::from_str::<serde_json::Value>(line),
        Ok(serde_json::Value::Object(_))
    ) {
        return mask_json_values(line);
    }

    let lower = line.to_ascii_lowercase();
    let prefix_hits_key_rule = match line.split_once(':').or_else(|| line.split_once('=')) {
        Some((prefix, _)) => prefix.trim_end().to_ascii_lowercase().ends_with("key"),
        None => false,
    };
    if !(SENSITIVE_KEYWORDS.iter().any(|key| lower.contains(key)) || prefix_hits_key_rule) {
        return line.to_string();
    }

    if let Some((prefix, _)) = line.split_once(':') {
        return format!("{prefix}: \"<redacted>\"");
    }
    if let Some((prefix, _)) = line.split_once('=') {
        return format!("{}= \"<redacted>\"", prefix.trim_end());
    }
    "<redacted>".to_string()
}

const SENSITIVE_KEYWORDS: [&str; 10] = [
    "api_key",
    "apikey",
    "api-key",
    "x-api-key",
    "authorization",
    "auth",
    "token",
    "secret",
    "password",
    "credential",
];

fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEYWORDS.iter().any(|word| key.contains(word)) || key.ends_with("key")
}

/// 单行 JSON 对象的值级脱敏：按字节扫描，仅替换命中敏感规则的字符串字面量值，
/// 其余字符（键名、顺序、空白、转义）原样保留。
fn mask_json_values(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut contexts: Vec<u8> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                contexts.push(b'{');
                keys.push(String::new());
                out.push('{');
                i += 1;
            }
            b'}' => {
                contexts.pop();
                keys.pop();
                out.push('}');
                i += 1;
            }
            b'[' => {
                contexts.push(b'[');
                out.push('[');
                i += 1;
            }
            b']' => {
                contexts.pop();
                out.push(']');
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut content = String::new();
                let mut escaped = false;
                while i < bytes.len() {
                    let c = bytes[i];
                    if escaped {
                        content.push(c as char);
                        escaped = false;
                    } else if c == b'\\' {
                        escaped = true;
                    } else if c == b'"' {
                        i += 1;
                        break;
                    } else {
                        content.push(c as char);
                    }
                    i += 1;
                }
                let mut j = start;
                while j > 0 && matches!(bytes[j - 1], b' ' | b'\t' | b'\r' | b'\n') {
                    j -= 1;
                }
                let prev = if j > 0 { bytes[j - 1] } else { 0 };
                let is_key = contexts.last() == Some(&b'{') && (prev == b'{' || prev == b',');
                let is_value = prev == b':'
                    || (contexts.last() == Some(&b'[') && (prev == b'[' || prev == b','));
                if is_key {
                    if let Some(key) = keys.last_mut() {
                        *key = content.to_ascii_lowercase();
                    }
                    out.push_str(&line[start..i]);
                } else if is_value {
                    let key_sensitive = keys
                        .last()
                        .is_some_and(|key| is_sensitive_key(key.as_str()));
                    let value_sensitive = content.to_ascii_lowercase().starts_with("sk-");
                    if key_sensitive || value_sensitive {
                        out.push_str("\"<redacted>\"");
                    } else {
                        out.push_str(&line[start..i]);
                    }
                } else {
                    out.push_str(&line[start..i]);
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_contents_without_leaving_temp_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nested/config.json");
        atomic_write(&path, br#"{"ok":true}"#).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"ok":true}"#);
        let entries = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("config.json")]);
    }

    #[test]
    fn atomic_write_failure_cleans_temp_and_keeps_original() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        fs::write(&path, "original").unwrap();
        // 使父目录只读，注入 rename 后 sync 失败
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o555)).unwrap();
        }
        let result = atomic_write(&path, b"replacement");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        let leftovers = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains("xu-tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn restore_backup_uses_atomic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("config.json");
        let backup = root.path().join("config.json.bak");
        fs::write(&target, "new").unwrap();
        fs::write(&backup, "old").unwrap();
        restore_backup(&backup, &target).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "old");
    }

    #[test]
    fn mixed_transaction_dry_run_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("config.json");
        fs::write(&file, "old").unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "# Skill").unwrap();
        let destination = root.path().join("skills/test");
        let plan = TransactionPlan {
            operations: vec![
                TransactionOperation::File(ConfigPatch::new(
                    file.clone(),
                    "old".to_string(),
                    "new".to_string(),
                )),
                TransactionOperation::Skill(ProjectionPatch {
                    id: "test".to_string(),
                    source,
                    destination: destination.clone(),
                    method: crate::skills::SkillSyncMethod::Symlink,
                    before: ProjectionState::Absent,
                    after: ProjectionState::Present,
                }),
            ],
        };
        let results = apply_transaction(&plan, true).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(fs::read_to_string(file).unwrap(), "old");
        assert!(destination.symlink_metadata().is_err());
    }

    #[test]
    fn transaction_rejects_duplicate_file_paths_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        fs::write(&path, "old").unwrap();
        let plan = TransactionPlan {
            operations: vec![
                TransactionOperation::File(ConfigPatch::new(
                    path.clone(),
                    "old".to_string(),
                    "one".to_string(),
                )),
                TransactionOperation::File(ConfigPatch::new(
                    path.clone(),
                    "old".to_string(),
                    "two".to_string(),
                )),
            ],
        };
        assert!(apply_transaction(&plan, false)
            .unwrap_err()
            .contains("duplicate uncomposed"));
        assert_eq!(fs::read_to_string(path).unwrap(), "old");
    }

    #[test]
    fn transaction_rolls_back_file_when_skill_create_fails() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("config");
        fs::write(&file, "old").unwrap();
        let missing_source = root.path().join("missing-source");
        let destination = root.path().join("skills/test");
        let plan = TransactionPlan {
            operations: vec![
                TransactionOperation::File(ConfigPatch::new(
                    file.clone(),
                    "old".to_string(),
                    "new".to_string(),
                )),
                TransactionOperation::Skill(ProjectionPatch {
                    id: "test".to_string(),
                    source: missing_source,
                    destination,
                    method: crate::skills::SkillSyncMethod::Copy,
                    before: ProjectionState::Absent,
                    after: ProjectionState::Present,
                }),
            ],
        };
        assert!(apply_transaction(&plan, false)
            .unwrap_err()
            .contains("transaction apply failed"));
        assert_eq!(fs::read_to_string(file).unwrap(), "old");
    }

    #[test]
    fn transaction_restores_quarantined_skill_when_later_file_fails() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "# Skill").unwrap();
        let destination = root.path().join("skills/test");
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("SKILL.md"), "# Modified projection").unwrap();
        fs::write(destination.join(".xu-skill-owner"), "test").unwrap();
        let blocking_parent = root.path().join("not-a-directory");
        fs::write(&blocking_parent, "blocking file").unwrap();
        let impossible = blocking_parent.join("config");
        let plan = TransactionPlan {
            operations: vec![
                TransactionOperation::Skill(ProjectionPatch {
                    id: "test".to_string(),
                    source,
                    destination: destination.clone(),
                    method: crate::skills::SkillSyncMethod::Copy,
                    before: ProjectionState::Present,
                    after: ProjectionState::Absent,
                }),
                TransactionOperation::File(ConfigPatch::new(
                    impossible,
                    String::new(),
                    "new".to_string(),
                )),
            ],
        };
        assert!(apply_transaction(&plan, false).is_err());
        assert_eq!(
            fs::read_to_string(destination.join("SKILL.md")).unwrap(),
            "# Modified projection"
        );
    }

    #[test]
    fn apply_rejects_file_changed_after_preview() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        fs::write(&path, "before").unwrap();
        let patch = ConfigPatch::new(path.clone(), "before".to_string(), "after".to_string());
        fs::write(&path, "external edit").unwrap();
        let error = apply_patch(
            &patch,
            PatchOptions {
                dry_run: false,
                backup: true,
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("预览后")
                || error.to_string().contains("changed after preview")
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "external edit");
    }

    #[test]
    fn mask_secret_line_covers_nested_headers_and_arbitrary_keys() {
        assert_eq!(
            mask_secret_line(r#"{"headers":{"x-api-key":"sk-live-123","X-Custom-Key":"v"}}"#),
            r#"{"headers":{"x-api-key":"<redacted>","X-Custom-Key":"<redacted>"}}"#
        );
        assert_eq!(
            mask_secret_line(r#"{"apiKey":"sk-live-abc"}"#),
            r#"{"apiKey":"<redacted>"}"#
        );
        assert_eq!(
            mask_secret_line("X-Custom-Key: raw-value"),
            "X-Custom-Key: \"<redacted>\""
        );
    }

    #[test]
    fn file_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("xu-state.json");
        let first = FileLock::try_acquire(&target).unwrap();
        let second = FileLock::try_acquire(&target);
        assert!(second.is_err());
        drop(first);
        let third = FileLock::try_acquire(&target);
        assert!(third.is_ok());
    }
}
