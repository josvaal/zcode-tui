//! Error types for the zcode RPC client.

use thiserror::Error;

/// Error returned by a remote channel call (mirrors the JS `Error` passthrough
/// payload: name, message, stack and optional structured keys).
#[derive(Debug, Clone, Error)]
#[error("{name}: {message}")]
pub struct RpcError {
    pub name: String,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("transport closed")]
    Closed,
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("malformed frame: {0}")]
    Malformed(String),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}
