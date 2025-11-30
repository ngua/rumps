use thiserror::Error;

/// Errors that can occur during B-tree operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Node not found in storage.
    ///
    /// The u64 represents a NodeId (which is a private internal type).
    #[error("Node({0}) not found")]
    NodeNotFound(u64),

    /// Key not found in tree
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    /// Node overflow (too many keys)
    #[error("Node overflow: {current} keys, max {max}")]
    NodeOverflow {
        /// Current number of keys in the node
        current: usize,
        /// Maximum allowed keys
        max: usize,
    },

    /// Memory limit exceeded
    #[error("Memory limit exceeded: {used} bytes, limit {limit}")]
    MemoryLimitExceeded {
        /// Current memory usage in bytes
        used: usize,
        /// Memory limit in bytes
        limit: usize,
    },

    /// Invalid operation
    #[error("Invalid operation: {0}")]
    InvalidOperation(String),

    /// I/O error (for future disk operations)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Transaction error (for future)
    #[error("Transaction error: {0}")]
    Transaction(String),

    /// WAL corruption detected (e.g., checksum mismatch).
    #[error("WAL corruption at seq {seq}: {reason}")]
    WalCorruption {
        /// Sequence number of the corrupted record.
        seq: u64,
        /// Description of the corruption.
        reason: String,
    },

    /// WAL file has invalid magic bytes.
    #[error("WAL invalid magic")]
    WalInvalidMagic,

    /// WAL file has unsupported version.
    #[error("WAL unsupported version: {0}")]
    WalUnsupportedVersion(u16),
}

/// A specialized Result type for storage operations.
///
/// This is a convenience type alias that fixes the error type to [`StorageError`].
pub type Result<T> = std::result::Result<T, StorageError>;
