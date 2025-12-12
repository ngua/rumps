//! Error types for RUMPS.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::orm::DecodeError;
use crate::{Key, Name};

/// The main error type for RUMPS operations.
///
/// This is the top-level error type returned by all public RUMPS APIs.
#[derive(Debug, Error)]
pub enum Error {
    /// Storage layer error.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// ORM decode error (type conversion failure).
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// Result type for RUMPS operations.
///
/// This is the standard result type returned by all public RUMPS APIs.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors from the storage layer.
///
/// These errors can occur during B-tree operations, I/O, transactions,
/// and WAL operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Node not found in storage.
    ///
    /// The `u64` represents a `NodeId` (which is a private internal type).
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
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },

    /// Generic I/O error (without context).
    #[error("I/O error: {0}")]
    IoGeneric(#[from] io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Transaction error
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

    /// Too many concurrent transactions.
    #[error("Too many concurrent transactions (limit: {limit})")]
    TooManyConcurrentTransactions {
        /// Maximum allowed concurrent transactions.
        limit: usize,
    },

    /// Writes to globals require a transaction.
    #[error("Writes to global variables require a transaction")]
    GlobalRequiresTransaction,

    /// Database is locked by another process.
    #[error("Database at {path} is locked by another process")]
    DatabaseLocked {
        /// Path to the locked database.
        path: PathBuf,
    },

    /// Transaction is not active.
    #[error("Transaction {id} is not active (state: {state})")]
    TransactionNotActive {
        /// Transaction ID.
        id: u64,
        /// Current state.
        state: String,
    },

    /// Write-write conflict detected during transaction commit.
    #[error("Write conflict: transaction {txn_id} conflicts on {name}:{key}")]
    WriteConflict {
        /// Transaction ID that encountered the conflict.
        txn_id: u64,
        /// Variable name where conflict occurred.
        name: Name,
        /// Key where conflict occurred.
        key: Key,
    },
}
