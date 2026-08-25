pub mod agent;
pub mod protocol;
pub mod provider;

pub use agent::{AgentConfigSpec, AgentTarget};
pub use protocol::{ProtocolAdapter, ProtocolKind, RoutingMode};
pub use provider::{CacheMode, ModelEntry, ProviderProfile, ProviderVendor};

// Compatibility alias for older imports that referred to ApiKind.
pub type ApiKind = ProtocolKind;
