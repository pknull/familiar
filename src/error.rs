//! Error types for Familiar.

use thallus_core::CoreError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FamiliarError {
    #[error("Configuration error: {reason}")]
    Config { reason: String },

    #[error("Identity not found at {path}")]
    IdentityNotFound { path: String },

    #[error("Invalid keypair: {reason}")]
    InvalidKeypair { reason: String },

    #[error("MCP error: {reason}")]
    Mcp { reason: String },

    #[error("MCP server '{name}' not found")]
    McpServerNotFound { name: String },

    #[error("Invalid arguments for MCP tool '{tool}': {reason}")]
    McpValidation { tool: String, reason: String },

    #[error("LLM provider error: {reason}")]
    Provider { reason: String },

    #[error("Egregore API error: {reason}")]
    Egregore { reason: String },

    #[error("Timeout after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("Store error: {reason}")]
    Store { reason: String },

    #[error("Internal error: {reason}")]
    Internal { reason: String },

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, FamiliarError>;

impl From<CoreError> for FamiliarError {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::IdentityNotFound { path } => FamiliarError::IdentityNotFound { path },
            CoreError::InvalidKeypair { reason } => FamiliarError::InvalidKeypair { reason },
            CoreError::Mcp { reason } => FamiliarError::Mcp { reason },
            CoreError::McpServerNotFound { name } => FamiliarError::McpServerNotFound { name },
            CoreError::McpValidation { tool, reason } => {
                FamiliarError::McpValidation { tool, reason }
            }
            CoreError::Provider { reason } => FamiliarError::Provider { reason },
            CoreError::Config { reason } => FamiliarError::Config { reason },
            CoreError::Io(e) => FamiliarError::Io(e),
            CoreError::Json(e) => FamiliarError::Json(e),
            CoreError::Http(e) => FamiliarError::Http(e),
        }
    }
}
