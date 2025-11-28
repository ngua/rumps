//! Persistent storage layer for RUMPS database.
//!
//! This crate implements the B-tree-backed persistent storage system
//! for MUMPS-style globals with disk persistence.
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
#![deny(clippy::use_self)]

mod btree;
mod error;
pub(crate) mod node;
pub(crate) mod page;
mod transaction;

#[cfg(feature = "bench")]
pub use btree::benches;
pub use error::{Result, StorageError};
pub use rumps_types::DataStatus;
pub use transaction::TransactionContext;
