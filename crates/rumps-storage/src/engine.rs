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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::fs::File;
use tokio::sync::RwLock;

use crate::error::{Result, StorageError};
use crate::node::{Node, NodeId};
use crate::page::{PageAllocator, PageCache, PageId};
use crate::wal::{WalWriter, WalWriterConfig};

/// Configuration for [`FileStorageEngine`].
#[derive(Debug, Clone)]
pub(crate) struct StorageConfig {
    /// Maximum number of pages to cache in memory.
    ///
    /// Larger values improve read performance at the cost of memory.
    /// Default: `1024` pages (~4 MiB at 4KB page size).
    pub(crate) cache_size: usize,

    /// Maximum number of pages in the database (optional limit).
    ///
    /// `None` means unlimited growth. Useful for testing or
    /// resource-constrained environments.
    pub(crate) max_pages: Option<u64>,

    /// WAL configuration (sync mode, max file size, etc.).
    pub(crate) wal_config: WalWriterConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            cache_size: 1024,
            max_pages: None,
            wal_config: WalWriterConfig::default(),
        }
    }
}

/// Metadata about the storage engine state.
#[derive(Debug, Clone)]
pub(crate) struct StorageMetadata {
    /// Total number of allocated pages (including header).
    pub(crate) allocated_pages: u64,

    /// Number of free pages available for allocation.
    pub(crate) free_pages: u64,

    /// Number of pages currently in cache.
    pub(crate) cached_pages: usize,

    /// Number of dirty pages pending flush.
    pub(crate) dirty_pages: usize,

    /// Cache hit rate (0.0 to 1.0).
    pub(crate) cache_hit_rate: f64,

    /// Path to the data directory.
    pub(crate) data_dir: PathBuf,
}

/// Async storage engine trait for B-tree node persistence.
///
/// Implementations must be `Send + Sync` for use across async tasks.
/// All operations are fallible and return [`Result`].
///
/// # Node ↔ Page Mapping
///
/// For persistent globals, [`NodeId`] maps directly to [`PageId`]:
/// - `NodeId(n)` corresponds to `PageId(n * PAGE_SIZE)`
/// - Page 0 is reserved for metadata (global registry)
///
/// For locals (ephemeral), `NodeId` maps to in-memory indices only.
#[async_trait]
pub(crate) trait AsyncStorageEngine: Send + Sync {
    /// Read a node from storage.
    ///
    /// Returns the node data for the given ID. May read from cache or disk.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NodeNotFound`] if the node doesn't exist.
    async fn read_node(&self, id: NodeId) -> Result<Node>;

    /// Write a node to storage.
    ///
    /// The node is written to the WAL and marked dirty in the cache.
    /// Actual disk write happens on flush/checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL write fails or the page cannot be allocated.
    async fn write_node(&self, id: NodeId, node: &Node) -> Result<()>;

    /// Allocate a new page for a node.
    ///
    /// Returns a fresh [`NodeId`] that can be used for [`write_node`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::MemoryLimitExceeded`] if page limit is reached.
    ///
    /// [`write_node`]: Self::write_node
    async fn allocate_page(&self) -> Result<NodeId>;

    /// Deallocate a page, marking it as free for reuse.
    ///
    /// The page should not be accessed after deallocation.
    ///
    /// # Errors
    ///
    /// Returns an error if the page is not currently allocated.
    async fn deallocate_page(&self, id: NodeId) -> Result<()>;

    /// Flush all dirty pages to disk.
    ///
    /// This writes all cached dirty pages to the data file and syncs.
    /// Called during checkpoint or shutdown.
    async fn flush(&self) -> Result<()>;

    /// Get metadata about the storage engine state.
    async fn metadata(&self) -> StorageMetadata;
}

/// File-based storage engine with WAL and page cache.
///
/// `FileStorageEngine` persists B-tree nodes to disk using:
///
/// - **Data file**: Fixed-size pages containing serialized nodes
/// - **Page cache**: LRU cache for recently accessed pages
/// - **WAL**: Write-ahead log for crash recovery
/// - **Page allocator**: Bitmap tracking free/allocated pages
///
/// # Construction
///
/// Use [`open`] for existing databases or [`create`] for new ones:
///
/// ```ignore
/// // Open existing
/// let engine = FileStorageEngine::open(Path::new("./data"), config).await?;
///
/// // Create new
/// let engine = FileStorageEngine::create(Path::new("./data"), config).await?;
/// ```
///
/// # Thread Safety
///
/// This type is `Send + Sync` and can be shared via `Arc<FileStorageEngine>`.
/// Internal locks ensure safe concurrent access.
///
/// [`open`]: Self::open
/// [`create`]: Self::create
pub(crate) struct FileStorageEngine {
    /// Data file handle for page I/O.
    data_file: Arc<RwLock<File>>,

    /// Write-ahead log for durability.
    wal: Arc<WalWriter>,

    /// LRU cache of recently accessed pages.
    cache: Arc<PageCache>,

    /// Bitmap-based page allocator.
    page_alloc: Arc<PageAllocator>,

    /// Configuration settings.
    cfg: StorageConfig,

    /// Path to the data directory.
    data_dir: PathBuf,
}

impl std::fmt::Debug for FileStorageEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorageEngine")
            .field("data_dir", &self.data_dir)
            .field("cache_size", &self.cfg.cache_size)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn storage_config_default() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.cache_size, 1024);
        assert!(cfg.max_pages.is_none());
    }

    #[test]
    fn storage_metadata_fields() {
        let meta = StorageMetadata {
            allocated_pages: 100,
            free_pages: 50,
            cached_pages: 25,
            dirty_pages: 5,
            cache_hit_rate: 0.75,
            data_dir: PathBuf::from("/tmp/test"),
        };

        assert_eq!(meta.allocated_pages, 100);
        assert_eq!(meta.free_pages, 50);
        assert_eq!(meta.cached_pages, 25);
        assert_eq!(meta.dirty_pages, 5);
        assert!((meta.cache_hit_rate - 0.75).abs() < 0.001);
        assert_eq!(meta.data_dir, PathBuf::from("/tmp/test"));
    }
}
