//! Request and response types exchanged between the HTTP worker and the app.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// A request kind accepted by the application loop.
#[derive(Debug, Clone)]
pub enum ReplRequestKind {
    /// Evaluate one Fennel form.
    Eval(String),
    /// Start playback at an optional pattern and row.
    Play {
        /// Optional pattern ID.
        pattern: Option<u8>,
        /// Optional row.
        row: Option<usize>,
    },
    /// Stop playback and release notes.
    Stop,
    /// Clear session-defined globals.
    Reset,
    /// Report playback and project state without stopping playback.
    Status,
    /// Report the routes and namespaces the REPL exposes.
    Help,
}

/// A request waiting for the application loop.
pub struct ReplRequest {
    /// The request to service.
    pub kind: ReplRequestKind,
    /// The channel used to return the response.
    pub response: Sender<ReplResponse>,
    /// Set when the HTTP worker gave up waiting, so the application loop skips
    /// a request whose client has already seen a timeout.
    pub cancel: Arc<AtomicBool>,
}

/// A JSON response with an HTTP status.
#[derive(Debug, Clone)]
pub struct ReplResponse {
    /// HTTP status code.
    pub status: u16,
    /// JSON body.
    pub body: serde_json::Value,
}
