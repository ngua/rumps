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

    /// I/O error with context.
    #[error("I/O error during {op} on {path}: {source}")]
    Io {
        /// Operation that failed.
        op: String,
        /// Path involved.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Generic I/O error (without context).
    #[error("I/O error: {0}")]
    IoGeneric(#[from] std::io::Error),

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

    /// WAL background task has shut down.
    #[error("WAL background task shut down")]
    WalShutdown,

    /// A single registry page is full and cannot accept more entries.
    ///
    /// This is an internal error used to signal that chaining is needed.
    #[error("Registry page full")]
    RegistryPageFull,
}

/// A specialized Result type for storage operations.
///
/// This is a convenience type alias that fixes the error type to [`StorageError`].
pub type Result<T> = std::result::Result<T, StorageError>;

impl From<rumps_types::Error> for StorageError {
    fn from(e: rumps_types::Error) -> Self {
        match e {
            rumps_types::Error::NotImplemented => {
                Self::InvalidOperation("not yet implemented".into())
            }
            rumps_types::Error::PageOutOfBounds(n) => {
                Self::InvalidOperation(format!("page {n} is out of bounds"))
            }
            rumps_types::Error::PageNotAllocated(n) => {
                Self::InvalidOperation(format!("page {n} is not allocated"))
            }
            rumps_types::Error::PageLimitExceeded(n) => {
                Self::MemoryLimitExceeded {
                    used: n as usize,
                    limit: n as usize,
                }
            }
            rumps_types::Error::CannotFreeHeaderPage => {
                Self::InvalidOperation("cannot free header page".into())
            }
            rumps_types::Error::InvalidBitmap => {
                Self::InvalidOperation("invalid bitmap".into())
            }
            rumps_types::Error::PageNumberOverflow(n) => {
                Self::InvalidOperation(format!("page number {n} overflows"))
            }
            rumps_types::Error::CannotFreeReservedPage(n) => {
                Self::InvalidOperation(format!("cannot free reserved page {n}"))
            }
        }
    }
}
