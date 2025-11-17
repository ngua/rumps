use thiserror::Error;
use rumps_types::NodeId;

/// Errors that can occur during B-tree operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Node not found in storage
    #[error("Node {0:?} not found")]
    NodeNotFound(NodeId),

    /// Key not found in tree
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    /// Node overflow (too many keys)
    #[error("Node overflow: {current} keys, max {max}")]
    NodeOverflow { current: usize, max: usize },

    /// Memory limit exceeded
    #[error("Memory limit exceeded: {used} bytes, limit {limit}")]
    MemoryLimitExceeded { used: usize, limit: usize },

    /// I/O error (for future disk operations)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Transaction error (for future)
    #[error("Transaction error: {0}")]
    Transaction(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
