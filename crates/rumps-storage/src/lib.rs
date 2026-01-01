//! Persistent storage layer for RUMPS database.
//!
//! This crate implements the B-tree-backed persistent storage system
//! for MUMPS-style globals with disk persistence.
//!
//! # Platform Requirements
//!
//! RUMPS requires a 64-bit platform. The storage layer uses `u64` page
//! identifiers throughout, and 32-bit platforms would require pervasive
//! bounds checking and truncation handling.
//!
//! # Async-First Design
//!
//! All APIs are async from the start to support future disk I/O without
//! breaking changes. Even pure in-memory operations use async primitives
//! (e.g., `tokio::sync::RwLock`) for consistency.
//!
//! # Thread Safety
//!
//! All structures are designed to be used with `Arc` for thread-safe sharing.
//! Operations use interior mutability via `RwLock` for concurrent access.

#![warn(missing_docs)]

#[cfg(not(target_pointer_width = "64"))]
compile_error!("RUMPS requires a 64-bit platform (usize must be 64 bits)");

pub(crate) mod btree;
pub(crate) mod database;
pub(crate) mod engine;
pub(crate) mod error;
pub(crate) mod node;
pub mod orm;
pub(crate) mod page;
pub(crate) mod transaction;
pub(crate) mod wal;

pub use btree::BTreeStats;
pub use database::{
    Database, DatabaseBuilder, DatabaseOverride, DatabaseRebuild,
    DatabaseReconfigure, DatabaseStats,
};
pub use engine::{StorageConfig, StorageMetadata};
pub use error::{Error, StorageError};
pub use page::PageCacheStats;
pub use rumps_types::{DataStatus, Result};
pub use transaction::{
    ConflictStrategy, IsolationLevel, Transaction, TransactionBuilder,
    TransactionContext,
};
pub use wal::{SyncMode, WalWriterConfig};
