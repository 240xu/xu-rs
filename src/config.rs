use std::path::PathBuf;

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/data/data/com.termux/files/home"))
}

pub fn codex_home() -> PathBuf {
    std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".codex"))
}

pub fn providers_path() -> PathBuf {
    codex_home().join("xu-chat-providers.json")
}

/// 单个会话档案（三端统一视图：来源 + 标识 + 文件路径 + 标题 + 时间）。
#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub source: String,
    pub id: String,
    pub file: String,
    pub title: String,
    pub time: String,
}
