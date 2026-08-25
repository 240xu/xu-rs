pub mod agent_tools;
pub mod agents;
pub mod cli;
pub mod config;
pub mod domain;
pub mod mcp;
pub mod opencode_settings;
pub mod patch;
pub mod provider_check;
pub mod provider_presets;
pub mod providers;
pub mod runtime;
pub mod session_store;
pub mod skills;
pub mod state;
pub mod stats;
pub mod sync;
pub mod sync_mcp;
pub mod web;

// Compatibility re-exports (old module names).
pub use crate::agents as adapters;
pub use crate::domain as models;
pub use crate::providers::store as provider_store;
