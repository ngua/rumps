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
use futures::stream::TryStreamExt;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::RwLock;

use crate::error::{Result, StorageError};
use crate::node::{Node, NodeId};
use crate::page::{self, PageAllocator, PageCache, PageId};
use crate::wal::{WalReader, WalWriter, WalWriterConfig};

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
    async fn read(&self, id: NodeId) -> Result<Node>;

    /// Write a node to storage.
    ///
    /// The node is written to the WAL and marked dirty in the cache.
    /// Actual disk write happens on flush/checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL write fails or the page cannot be allocated.
    async fn write(&self, id: NodeId, node: &Node) -> Result<()>;

    /// Allocate a new page for a node.
    ///
    /// Returns a fresh [`NodeId`] that can be used for [`write`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::MemoryLimitExceeded`] if page limit is reached.
    ///
    /// [`write`]: Self::write
    async fn allocate(&self) -> Result<NodeId>;

    /// Deallocate a page, marking it as free for reuse.
    ///
    /// The page should not be accessed after deallocation.
    ///
    /// # Errors
    ///
    /// Returns an error if the page is not currently allocated.
    async fn deallocate(&self, id: NodeId) -> Result<()>;

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

impl FileStorageEngine {
    /// Magic bytes for identifying RUMPS data files.
    const MAGIC: [u8; 4] = *b"RUMP";

    /// Data file name within the data directory.
    const DATA_FILE_NAME: &'static str = "data.db";

    /// WAL directory name within the data directory.
    const WAL_DIR_NAME: &'static str = "wal";

    // Header layout: [magic:4][version:4][bitmap_len:8][bitmap:...]
    const HDR_MAGIC_OFFSET: usize = 0;
    const HDR_VERSION_OFFSET: usize = 4;
    const HDR_BITMAP_LEN_OFFSET: usize = 8;
    const HDR_BITMAP_DATA_OFFSET: usize = 16;

    /// Open an existing database.
    ///
    /// Opens the data file and WAL, runs WAL recovery, and initializes
    /// the page cache. The database directory must already exist and
    /// contain a valid data file.
    ///
    /// # WAL Recovery
    ///
    /// On open, all WAL records are read and committed transactions are
    /// replayed to bring the database to a consistent state. Uncommitted
    /// transactions are discarded.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The directory doesn't exist
    /// - The data file is missing or corrupted
    /// - WAL recovery fails
    pub(crate) async fn open(dir: &Path, cfg: StorageConfig) -> Result<Self> {
        let data_path = dir.join(Self::DATA_FILE_NAME);
        let wal_dir = dir.join(Self::WAL_DIR_NAME);

        // Open data file (must exist)
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&data_path)
            .await
            .map_err(|e| StorageError::Io {
                op: "open data file".into(),
                path: data_path.clone(),
                source: e,
            })?;

        // Read file size to determine allocated pages
        let file_len = file
            .metadata()
            .await
            .map_err(|e| StorageError::Io {
                op: "get file metadata".into(),
                path: data_path.clone(),
                source: e,
            })?
            .len();

        // File must be at least one page (header)
        if file_len < page::PAGE_SIZE as u64 {
            Err(StorageError::InvalidOperation(
                "data file too small for header page".into(),
            ))
        } else {
            // Read page allocator bitmap from header page
            // Header layout: [magic:4][version:4][bitmap_len:8][bitmap:...]
            let mut hdr = vec![0u8; page::PAGE_SIZE];

            file.seek(SeekFrom::Start(0)).await.map_err(|e| {
                StorageError::Io {
                    op: "seek to header".into(),
                    path: data_path.clone(),
                    source: e,
                }
            })?;
            file.read_exact(&mut hdr)
                .await
                .map_err(|e| StorageError::Io {
                    op: "read header".into(),
                    path: data_path.clone(),
                    source: e,
                })?;

            // Validate magic
            let magic: [u8; 4] = hdr
                .get(Self::HDR_MAGIC_OFFSET..Self::HDR_VERSION_OFFSET)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(|| {
                    StorageError::InvalidOperation("header too short".into())
                })?;
            if magic != Self::MAGIC {
                Err(StorageError::InvalidOperation(format!(
                    "invalid magic: expected {:?}, got {:?}",
                    Self::MAGIC,
                    magic
                )))
            } else {
                // Read bitmap length and data
                let bm_len_bytes = hdr
                    .get(
                        Self::HDR_BITMAP_LEN_OFFSET
                            ..Self::HDR_BITMAP_DATA_OFFSET,
                    )
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(
                            "header missing bitmap length".into(),
                        )
                    })?;
                let bm_len = u64::from_le_bytes(
                    bm_len_bytes.try_into().map_err(|_| {
                        StorageError::InvalidOperation(
                            "invalid bitmap length bytes".into(),
                        )
                    })?,
                );

                let bm_end = Self::HDR_BITMAP_DATA_OFFSET + bm_len as usize;
                let bm_data = hdr
                    .get(Self::HDR_BITMAP_DATA_OFFSET..bm_end)
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(
                            "header missing bitmap data".into(),
                        )
                    })?;

                // Reconstruct page allocator
                let max_pages = cfg.max_pages;
                let page_alloc = PageAllocator::from_bytes(bm_data, max_pages)?;

                // Run WAL recovery
                let (recovery, reader) =
                    WalReader::open(&wal_dir).await?.recover().await?;

                // TODO(Phase 4.4): Apply recovery.committed_ops to page cache/data file
                // This should be implemented when adding `AsyncStorageEngine` trait impl.
                // Uncommitted transactions are automatically discarded.
                let _ = recovery.uncommitted_txns;

                // Convert reader to writer
                let wal = reader.into_writer(cfg.wal_config.clone()).await?;

                // Create page cache
                let cache = PageCache::new(cfg.cache_size);

                Ok(Self {
                    data_file: Arc::new(RwLock::new(file)),
                    wal: Arc::new(wal),
                    cache: Arc::new(cache),
                    page_alloc: Arc::new(page_alloc),
                    cfg,
                    data_dir: dir.to_path_buf(),
                })
            }
        }
    }

    /// Create a new database.
    ///
    /// Creates the data directory, initializes a fresh data file with header,
    /// and sets up a new WAL. Fails if the directory already contains a
    /// database.
    ///
    /// # Header Format
    ///
    /// The header page (page 0) contains:
    /// - Magic bytes (`RUMP`)
    /// - Version number
    /// - Bitmap length
    /// - Page allocator bitmap
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The directory already exists with a data file
    /// - Directory creation fails
    /// - File creation fails
    pub(crate) async fn create(dir: &Path, cfg: StorageConfig) -> Result<Self> {
        use tokio::io::AsyncWriteExt;

        let data_path = dir.join(Self::DATA_FILE_NAME);
        let wal_dir = dir.join(Self::WAL_DIR_NAME);

        // Create directories
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| StorageError::Io {
                op: "create data directory".into(),
                path: dir.to_path_buf(),
                source: e,
            })?;

        tokio::fs::create_dir_all(&wal_dir).await.map_err(|e| {
            StorageError::Io {
                op: "create WAL directory".into(),
                path: wal_dir.clone(),
                source: e,
            }
        })?;

        // Fail if data file already exists
        if data_path.exists() {
            Err(StorageError::InvalidOperation(format!(
                "database already exists at {}",
                data_path.display()
            )))
        } else {
            // Create fresh page allocator (page 0 reserved for header)
            let page_alloc = PageAllocator::with_limit(64, cfg.max_pages);

            // Serialize bitmap
            let bm = page_alloc.to_bytes().await;
            let bm_len = bm.len() as u64;

            // Build header page
            let mut hdr = vec![0u8; page::PAGE_SIZE];
            hdr.get_mut(Self::HDR_MAGIC_OFFSET..Self::HDR_VERSION_OFFSET)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "header buffer too small".into(),
                    )
                })?
                .copy_from_slice(&Self::MAGIC);

            // Version = 1
            hdr.get_mut(Self::HDR_VERSION_OFFSET..Self::HDR_BITMAP_LEN_OFFSET)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "header buffer too small".into(),
                    )
                })?
                .copy_from_slice(&1u32.to_le_bytes());

            // Bitmap length
            hdr.get_mut(
                Self::HDR_BITMAP_LEN_OFFSET..Self::HDR_BITMAP_DATA_OFFSET,
            )
            .ok_or_else(|| {
                StorageError::InvalidOperation("header buffer too small".into())
            })?
            .copy_from_slice(&bm_len.to_le_bytes());

            // Bitmap data
            let bm_end = Self::HDR_BITMAP_DATA_OFFSET + bm.len();
            hdr.get_mut(Self::HDR_BITMAP_DATA_OFFSET..bm_end)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "bitmap too large for header page".into(),
                    )
                })?
                .copy_from_slice(&bm);

            // Write header to data file
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&data_path)
                .await
                .map_err(|e| StorageError::Io {
                    op: "create data file".into(),
                    path: data_path.clone(),
                    source: e,
                })?;

            file.write_all(&hdr).await.map_err(|e| StorageError::Io {
                op: "write header".into(),
                path: data_path.clone(),
                source: e,
            })?;

            file.sync_all().await.map_err(|e| StorageError::Io {
                op: "sync data file".into(),
                path: data_path,
                source: e,
            })?;

            // Create fresh WAL (open handles empty dir)
            let reader = WalReader::open(&wal_dir).await?;
            let wal = reader.into_writer(cfg.wal_config.clone()).await?;

            // Create page cache
            let cache = PageCache::new(cfg.cache_size);

            Ok(Self {
                data_file: Arc::new(RwLock::new(file)),
                wal: Arc::new(wal),
                cache: Arc::new(cache),
                page_alloc: Arc::new(page_alloc),
                cfg,
                data_dir: dir.to_path_buf(),
            })
        }
    }
}

#[async_trait]
impl AsyncStorageEngine for FileStorageEngine {
    async fn read(&self, id: NodeId) -> Result<Node> {
        let page_id = PageId::from(id);

        // Check cache first
        if let Some(cached) = self.cache.get(page_id).await {
            Ok((*cached).clone())
        } else {
            // Cache miss - read from disk
            let offset = page_id.byte_offset();
            let mut buf = vec![0u8; page::PAGE_SIZE];
            let mut file = self.data_file.write().await;

            file.seek(SeekFrom::Start(offset)).await.map_err(|e| {
                StorageError::Io {
                    op: "seek for read".into(),
                    path: self.data_dir.join(Self::DATA_FILE_NAME),
                    source: e,
                }
            })?;
            file.read_exact(&mut buf)
                .await
                .map_err(|e| StorageError::Io {
                    op: "read page".into(),
                    path: self.data_dir.join(Self::DATA_FILE_NAME),
                    source: e,
                })?;
            drop(file);

            // Deserialize
            let node: Node = bincode::deserialize(&buf).map_err(|e| {
                StorageError::InvalidOperation(format!("deserialize node: {e}"))
            })?;

            // Add to cache (clean, since it came from disk)
            let evicted = self.cache.put(page_id, node.clone(), false).await;

            // Handle evicted dirty page
            if let Some(ev) = evicted {
                if ev.dirty {
                    self.write_page_to_disk(ev.id, &ev.node).await?;
                }
            }

            Ok(node)
        }
    }

    async fn write(&self, id: NodeId, node: &Node) -> Result<()> {
        let page_id = PageId::from(id);

        // Put in cache as dirty
        let evicted = self.cache.put(page_id, node.clone(), true).await;

        // Handle evicted dirty page
        if let Some(ev) = evicted {
            if ev.dirty {
                self.write_page_to_disk(ev.id, &ev.node).await?;
            }
        }

        Ok(())
    }

    async fn allocate(&self) -> Result<NodeId> {
        Ok(NodeId::from(self.page_alloc.allocate().await?))
    }

    async fn deallocate(&self, id: NodeId) -> Result<()> {
        let page_id = PageId::from(id);

        // Remove from cache if present
        self.cache.remove(page_id).await;

        // Free in allocator
        self.page_alloc.free(page_id).await?;

        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        // Get all dirty pages
        let dirty = self.cache.dirty_pages().await;

        // Write each dirty page to disk
        // (sequential for now; could parallelize with FuturesUnordered)
        let ids: Vec<_> = dirty
            .iter()
            .map(|(id, node)| async move {
                self.write_page_to_disk(*id, node).await.map(|_| *id)
            })
            .collect::<futures::stream::FuturesUnordered<_>>()
            .try_collect()
            .await?;

        // Mark all as flushed
        self.cache.mark_flushed(&ids).await;

        // Sync data file
        let file = self.data_file.read().await;

        file.sync_all().await.map_err(|e| StorageError::Io {
            op: "sync data file".into(),
            path: self.data_dir.join(Self::DATA_FILE_NAME),
            source: e,
        })?;

        Ok(())
    }

    async fn metadata(&self) -> StorageMetadata {
        StorageMetadata {
            allocated_pages: self.page_alloc.allocated_count().await,
            free_pages: self.page_alloc.free_count().await,
            cached_pages: self.cache.len().await,
            dirty_pages: self.cache.dirty_count().await,
            cache_hit_rate: self.cache.hit_rate().await,
            data_dir: self.data_dir.clone(),
        }
    }
}

impl FileStorageEngine {
    /// Write a page to disk at its designated offset.
    async fn write_page_to_disk(&self, id: PageId, node: &Node) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let offset = id.byte_offset();

        // Serialize node
        let data = bincode::serialize(node).map_err(|e| {
            StorageError::InvalidOperation(format!("serialize node: {e}"))
        })?;

        // Pad to page size
        let mut buf = vec![0u8; page::PAGE_SIZE];

        buf.get_mut(..data.len())
            .ok_or_else(|| {
                StorageError::InvalidOperation("node too large for page".into())
            })?
            .copy_from_slice(&data);

        // Write to file
        let mut file = self.data_file.write().await;

        file.seek(SeekFrom::Start(offset)).await.map_err(|e| {
            StorageError::Io {
                op: "seek for write".into(),
                path: self.data_dir.join(Self::DATA_FILE_NAME),
                source: e,
            }
        })?;
        file.write_all(&buf).await.map_err(|e| StorageError::Io {
            op: "write page".into(),
            path: self.data_dir.join(Self::DATA_FILE_NAME),
            source: e,
        })?;

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tempfile::TempDir;

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

    #[tokio::test]
    async fn open_nonexistent_dir_fails() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nonexistent");

        let result =
            FileStorageEngine::open(&path, StorageConfig::default()).await;

        // Should fail because data file doesn't exist
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn open_missing_data_file_fails() {
        let dir = TempDir::new().expect("temp dir");

        // Directory exists but no data.db file
        let result =
            FileStorageEngine::open(dir.path(), StorageConfig::default()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_new_database() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine =
            FileStorageEngine::create(&db_path, StorageConfig::default())
                .await
                .expect("create should succeed");

        // Verify files exist
        assert!(db_path.join("data.db").exists());
        assert!(db_path.join("wal").exists());

        // Verify engine state
        assert_eq!(engine.data_dir, db_path);
    }

    #[tokio::test]
    async fn create_then_open() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create
        let engine =
            FileStorageEngine::create(&db_path, StorageConfig::default())
                .await
                .expect("create should succeed");
        drop(engine);

        // Reopen
        let engine =
            FileStorageEngine::open(&db_path, StorageConfig::default())
                .await
                .expect("open should succeed");

        assert_eq!(engine.data_dir, db_path);
    }

    #[tokio::test]
    async fn create_existing_fails() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create first time
        FileStorageEngine::create(&db_path, StorageConfig::default())
            .await
            .expect("first create should succeed");

        // Create again should fail
        let result =
            FileStorageEngine::create(&db_path, StorageConfig::default()).await;

        assert!(result.is_err());
    }
}
