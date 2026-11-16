//! Async storage engine for persistent B-tree nodes.
//!
//! This module defines the [`AsyncStorageEngine`] trait that abstracts disk
//! operations, and [`FileStorageEngine`] which implements persistent storage
//! backed by data files, a page cache, and write-ahead logging.
//!
//! # Design
//!
//! The storage engine provides a clean abstraction between the B-tree logic
//! and physical storage details. This separation allows:
//!
//! - Different storage backends (file-based, memory-only for testing)
//! - Transparent caching and write buffering
//! - WAL-based crash recovery
//!
//! # Thread Safety
//!
//! All implementations are `Send + Sync` and designed for concurrent access.
//! Internal synchronization uses `RwLock` for read-heavy workloads.

mod config;
mod file;
mod indirect;
mod metadata;
mod registry;
mod superblock;

pub(crate) use config::SafeReconfiguration;
pub use config::{StorageConfig, StorageMetadata};
pub(crate) use file::FileStorageEngine;
pub(crate) use indirect::IndirectPage;
pub(crate) use metadata::MetadataPage;
pub(crate) use registry::GlobalRegistry;
pub(crate) use superblock::Superblock;
