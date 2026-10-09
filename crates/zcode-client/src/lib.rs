//! Rust client for the ZCode agent harness RPC protocol.
//!
//! Protocol reference: `packages/rpc/src/` in zai-org/ZCode.
//!
//! Layers implemented:
//! - [`value`] — tagged binary serialization (VQL + type tags)
//! - [`framing`] — 13-byte socket message framing
//! - [`channel`] — channel request/response/event RPC
//! - [`transport`] — WebSocket adapter
//!
//! Channel names live in `packages/shared/src/channels.ts`; the agent service
//! channel is `zcode-agent`, sessions `zcode-session`, providers
//! `provider-settings` / `model-selection`.

pub mod channel;
pub mod error;
pub mod framing;
pub mod transport;
pub mod value;

pub use channel::ChannelClient;
pub use error::{ClientError, RpcError};
pub mod channels {
    /// Service channel names, mirrored from `packages/shared/src/channels.ts`.
    pub const ZCODE_AGENT: &str = "zcode-agent";
    pub const ZCODE_SESSION: &str = "zcode-session";
    pub const PROVIDER_SETTINGS: &str = "provider-settings";
    pub const MODEL_SELECTION: &str = "model-selection";
    pub const TERMINAL: &str = "terminal";
    pub const FILE: &str = "file";
    pub const GIT: &str = "git";
}
pub use value::RpcValue;
