//! REPL error types.

/// Errors surfaced by the REPL session and protocol.
#[derive(Debug, thiserror::Error)]
pub enum ReplError {
    /// A Fennel compile error.
    #[error("compile error: {0}")]
    Compile(String),
    /// A Lua runtime error.
    #[error("runtime error: {0}")]
    Runtime(String),
    /// A request exceeded the wall-clock timeout.
    #[error("request timed out")]
    Timeout,
    /// A request exceeded the Lua instruction budget.
    #[error("instruction limit exceeded")]
    InstructionLimit,
    /// A request exceeded the Lua memory limit.
    #[error("memory limit exceeded")]
    MemoryLimit,
    /// A malformed request or response.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// An I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A JSON conversion failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl ReplError {
    /// A stable machine-readable kind for JSON responses.
    pub fn kind(&self) -> &'static str {
        match self {
            ReplError::Compile(_) => "compile",
            ReplError::Runtime(_) => "runtime",
            ReplError::Timeout => "timeout",
            ReplError::InstructionLimit => "instruction_limit",
            ReplError::MemoryLimit => "memory_limit",
            ReplError::Protocol(_) => "protocol",
            ReplError::Io(_) => "io",
            ReplError::Json(_) => "json",
        }
    }
}

/// Errors from converting a Lua value to JSON.
#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    /// The value exceeds the configured depth limit.
    #[error("value exceeds the maximum depth")]
    Depth,
    /// The value exceeds the configured node-count limit.
    #[error("value exceeds the maximum node count")]
    NodeCount,
    /// The value cannot be represented as JSON.
    #[error("value is not JSON-serializable: {0}")]
    Opaque(String),
}
