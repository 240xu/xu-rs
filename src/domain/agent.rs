use std::path::PathBuf;

use super::protocol::ProtocolKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentTarget {
    OpenCode,
    ClaudeCode,
    Codex,
    Other,
}

impl AgentTarget {
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenCode => "OpenCode",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Other => "Other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentConfigSpec {
    pub target: AgentTarget,
    pub config_files: Vec<PathBuf>,
    pub native_protocol: ProtocolKind,
    pub supports_direct_file: bool,
    pub supports_local_routing: bool,
}
