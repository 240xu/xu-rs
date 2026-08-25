pub fn run_session_restore(session: &crate::config::SessionInfo) {
    let mut command = match session.source.as_str() {
        "codex" => {
            let mut command = std::process::Command::new("codex");
            command.arg("resume").arg(&session.id);
            command
        }
        "claude" => {
            let mut command = std::process::Command::new("claude");
            command.arg("--resume").arg(&session.id);
            command
        }
        "opencode" => {
            let mut command = std::process::Command::new("opencode");
            command.arg("--session").arg(&session.id);
            command
        }
        other => {
            eprintln!("暂不支持恢复 {other} 会话");
            return;
        }
    };
    let status = command.status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!(
            "resume {} {} exited with {status}",
            session.source, session.id
        ),
        Err(error) => eprintln!("恢复会话失败 {} {}：{error}", session.source, session.id),
    }
}
