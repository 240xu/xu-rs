use std::io::BufRead;
use std::path::{Path, PathBuf};

use rusqlite::params;

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub source: String,
    pub id: String,
    pub file: String,
    pub title: String,
    pub time: String,
}

pub fn list_sessions() -> Vec<SessionInfo> {
    let mut sessions = Vec::new();
    sessions.extend(list_codex_sessions());
    sessions.extend(list_claude_sessions());
    sessions.extend(list_opencode_sessions());
    sessions.sort_by(|a, b| b.time.cmp(&a.time));
    sessions
}

pub fn delete_session(session: &SessionInfo) -> Result<(), String> {
    match session.source.as_str() {
        "codex" => delete_codex_session(session),
        "claude" => {
            let path = Path::new(&session.file);
            if path.exists() {
                std::fs::remove_file(path).map_err(|e| format!("删除文件失败：{e}"))?;
            }
            Ok(())
        }
        "opencode" => delete_opencode_session(Path::new(&session.file), &session.id),
        _ => Err(format!("未知来源: {}", session.source)),
    }
}

/// Codex 官方删除：`codex delete --force <id>` 同时清理会话文件与 threads 索引
/// （直接删文件会留下索引残留，`codex resume` 仍会列出已删会话）。官方命令
/// 不可用/未识别（索引外文件）时兜底直接删文件，保证文件级删除始终生效。
fn delete_codex_session(session: &SessionInfo) -> Result<(), String> {
    let output = std::process::Command::new("codex")
        .arg("delete")
        .arg("--force")
        .arg(&session.id)
        .output();
    match output {
        Ok(out) if out.status.success() => Ok(()),
        _ => {
            let path = Path::new(&session.file);
            if path.exists() {
                std::fs::remove_file(path).map_err(|e| format!("删除文件失败：{e}"))?;
            }
            Ok(())
        }
    }
}

pub fn delete_sessions_batch(sessions: &[SessionInfo]) -> (usize, usize, Vec<String>) {
    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut errors = Vec::new();
    for s in sessions {
        match delete_session(s) {
            Ok(()) => ok += 1,
            Err(e) => {
                fail += 1;
                errors.push(format!("{}:{e}", s.id));
            }
        }
    }
    (ok, fail, errors)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/data/data/com.termux/files/home"))
}

fn list_codex_sessions() -> Vec<SessionInfo> {
    let dir = home().join(".codex").join("sessions");
    let mut sessions = Vec::new();
    walk_jsonl(&dir, &mut sessions, "codex");
    sessions.sort_by(|a, b| b.time.cmp(&a.time));
    sessions
}

fn list_claude_sessions() -> Vec<SessionInfo> {
    let dir = home().join(".claude").join("projects");
    let mut sessions = Vec::new();
    walk_jsonl(&dir, &mut sessions, "claude");
    sessions
}

fn list_opencode_sessions() -> Vec<SessionInfo> {
    let db = home()
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    read_opencode_sessions(&db).unwrap_or_default()
}

fn walk_jsonl(dir: &Path, sessions: &mut Vec<SessionInfo>, source: &str) {
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

fn parse_session_file(path: &Path, source: &str) -> Option<SessionInfo> {
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
                if let Some(text) = first_user_text(&val) {
                    title = compact_title(&text, 80);
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

    Some(SessionInfo {
        source: source.to_string(),
        id,
        file: path.to_string_lossy().to_string(),
        title,
        time,
    })
}

fn first_user_text(val: &serde_json::Value) -> Option<String> {
    // Codex: response_item.payload / event_msg.user_message
    if val.get("type").and_then(|v| v.as_str()) == Some("response_item") {
        let payload = val.get("payload")?;
        if payload.get("type").and_then(|v| v.as_str()) == Some("message")
            && payload.get("role").and_then(|v| v.as_str()) == Some("user")
        {
            if let Some(text) = content_text(payload.get("content")?) {
                if !is_internal_title(&text) {
                    return Some(text);
                }
            }
        }
    }
    if val.get("type").and_then(|v| v.as_str()) == Some("event_msg") {
        let payload = val.get("payload")?;
        if payload.get("type").and_then(|v| v.as_str()) == Some("user_message") {
            if let Some(text) = payload.get("message").and_then(|v| v.as_str()) {
                if !is_internal_title(text) {
                    return Some(text.to_string());
                }
            }
        }
    }
    // Claude nested message
    if let Some(message) = val.get("message") {
        if message.get("role").and_then(|v| v.as_str()) == Some("user") {
            if let Some(content) = message.get("content") {
                if let Some(text) = content_text(content) {
                    if !is_internal_title(&text) {
                        return Some(text);
                    }
                }
            }
        }
    }
    // plain role/content
    if val.get("role").and_then(|v| v.as_str()) == Some("user") {
        if let Some(content) = val.get("content") {
            if let Some(text) = content_text(content) {
                if !is_internal_title(&text) {
                    return Some(text);
                }
            }
        }
    }
    if val.get("type").and_then(|v| v.as_str()) == Some("user") {
        if let Some(content) = val.get("content") {
            if let Some(text) = content_text(content) {
                if !is_internal_title(&text) {
                    return Some(text);
                }
            }
        }
    }
    None
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

fn is_internal_title(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t.starts_with("<environment_context>")
        || t.starts_with("<permissions instructions>")
        || t.starts_with("<local-command-caveat>")
}

fn compact_title(text: &str, max: usize) -> String {
    let one_line = text.replace('\n', " ");
    let chars: Vec<char> = one_line.chars().collect();
    if chars.len() <= max {
        one_line
    } else {
        chars.into_iter().take(max).collect::<String>() + "…"
    }
}

fn read_opencode_sessions(db: &Path) -> Result<Vec<SessionInfo>, String> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open {}: {e}", db.display()))?;
    let mut stmt = conn
        .prepare(
            "select id, title, time_updated from session where coalesce(time_archived, 0) = 0 order by time_updated desc limit 500",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let title: String = row.get(1)?;
            let time_ms: i64 = row.get(2)?;
            Ok(SessionInfo {
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

fn millis_to_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.naive_local().format("%Y-%m-%dT%H:%M:%S").to_string())
        .unwrap_or_default()
}

pub fn delete_opencode_session(db: &Path, session_id: &str) -> Result<(), String> {
    if !db.exists() {
        return Err(format!("数据库不存在：{}", db.display()));
    }
    let conn = rusqlite::Connection::open(db).map_err(|e| format!("打开数据库失败：{e}"))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| format!("启用外键失败：{e}"))?;
    let changed = conn
        .execute("delete from session where id = ?1", params![session_id])
        .map_err(|e| format!("删除会话失败：{e}"))?;
    if changed == 0 {
        return Err(format!("会话不存在：{session_id}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_official_codex_and_claude_formats() {
        let dir = tempfile::tempdir().unwrap();
        let codex_file = dir
            .path()
            .join("rollout-2026-06-22T17-15-26-fafafafa-1234-5678-9abc-def012345678.jsonl");
        std::fs::write(&codex_file, r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"你好测试"}]},"timestamp":"2026-06-22T17:15:26.000Z"}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"你好"}]},"timestamp":"2026-06-22T17:15:27.000Z"}
"#)
        .unwrap();
        let codex = parse_session_file(&codex_file, "codex").unwrap();
        assert_eq!(codex.id, "fafafafa-1234-5678-9abc-def012345678");
        assert_eq!(codex.title, "你好测试");

        let claude_dir = dir
            .path()
            .join("projects")
            .join("-data-data-com-termux-files-home");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let claude_file = claude_dir.join("52346172-f15f-4bed-a135-6328056150d6.jsonl");
        std::fs::write(&claude_file, r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Claude 测试"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}
"#)
        .unwrap();
        let claude = parse_session_file(&claude_file, "claude").unwrap();
        assert_eq!(claude.id, "52346172-f15f-4bed-a135-6328056150d6");
        assert_eq!(claude.title, "Claude 测试");
    }

    #[test]
    fn deletes_claude_session_by_file() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join("projects").join("-p");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let claude_file = claude_dir.join("52346172-f15f-4bed-a135-6328056150d6.jsonl");
        std::fs::write(&claude_file, "{}").unwrap();
        let info = parse_session_file(&claude_file, "claude").unwrap();
        assert!(claude_file.exists());
        delete_session(&info).unwrap();
        assert!(!claude_file.exists(), "claude session file must be deleted");
    }

    #[test]
    fn deletes_codex_session_via_cli_with_file_fallback() {
        // 官方 `codex delete --force <id>` 清理文件 + threads 索引；当 id 不在官方索引
        // （或 CLI 不可用）时兜底直接删文件。测试把 CODEX_HOME 指向临时目录并用随机
        // id，确保 CLI 一定走"未找到→兜底删文件"路径，绝不触碰真实会话。
        let dir = tempfile::tempdir().unwrap();
        let fake_id = "fafafafa-1234-5678-9abc-def012345678";
        let codex_file = dir
            .path()
            .join(format!("rollout-2026-06-22T17-15-26-{fake_id}.jsonl"));
        std::fs::write(
            &codex_file,
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{fake_id}"}}}}
{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"t"}}]}}}}
"#
            ),
        )
        .unwrap();
        let info = SessionInfo {
            source: "codex".to_string(),
            id: fake_id.to_string(),
            file: codex_file.to_string_lossy().to_string(),
            title: "t".to_string(),
            time: String::new(),
        };
        let old_home = std::env::var_os("CODEX_HOME");
        std::env::set_var("CODEX_HOME", dir.path());
        let result = delete_session(&info);
        match old_home {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        result.unwrap();
        assert!(!codex_file.exists(), "codex session file must be deleted");
    }

    #[test]
    fn hard_deletes_opencode_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "create table session (id text primary key, title text not null, time_updated integer not null, time_archived integer)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into session (id, title, time_updated, time_archived) values ('ses_x', 't', 1, 0)",
            [],
        )
        .unwrap();
        drop(conn);
        delete_opencode_session(&db, "ses_x").unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        let count: i64 = conn
            .query_row("select count(*) from session", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
