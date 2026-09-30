// Lifecycle events reported by a running server or client. The CLI prints
// them; the GUI forwards them as Tauri events.

use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Server: accepting clients on this address.
    Listening(String),
    /// Client: trying to reach this address.
    Connecting(String),
    /// Client: could not reach the server; retrying after a backoff.
    Retrying(String),
    /// Handshake done with this peer.
    Connected(String),
    /// Control is on the client machine (both roles report it).
    Remote,
    /// Control is back on the server machine (both roles report it).
    Local,
    /// A session ended, with the reason.
    Disconnected(String),
    /// `stop()` finished: no threads left, port and hooks released.
    Stopped,
}

/// Called from network threads, never from a hook callback.
pub type OnStatus = Arc<dyn Fn(Status) + Send + Sync>;
