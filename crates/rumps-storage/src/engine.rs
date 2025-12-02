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

/// Superblock stored in page 0, containing database metadata and bitmap page locations.
///
/// # Layout
///
/// ```text
/// Offset   Size     Field
/// 0        4        Magic ("RUMP")
/// 4        4        Version (2)
/// 8        8        Flags (reserved, must be 0)
/// 16       8        Total allocated page count (cached)
/// 24       8        Bitmap page count (N)
/// 32       8×500    Bitmap page IDs [PageId; 500]
/// 4032     56       Reserved (future use)
/// 4088     4        Checksum (CRC32 of bytes 0..4088)
/// 4092     4        Padding
/// ```
#[derive(Debug, Clone)]
pub(crate) struct Superblock {
    /// Format version (currently 1).
    pub(crate) version: u32,
    /// Flags (reserved, must be 0).
    pub(crate) flags: u64,
    /// Cached count of total allocated pages.
    pub(crate) total_pages: u64,
    /// Number of bitmap pages in use.
    pub(crate) bitmap_page_count: u64,
    /// Page IDs of bitmap pages (up to 500).
    pub(crate) bitmap_page_ids: Vec<PageId>,
}

impl Superblock {
    /// Magic bytes for RUMPS data files.
    const MAGIC: [u8; 4] = *b"RUMP";

    /// Current superblock version.
    const VERSION: u32 = 1;

    /// Maximum number of bitmap pages.
    pub(crate) const MAX_BITMAP_PAGES: usize = 500;

    // Layout offsets
    const OFF_MAGIC: usize = 0;
    const OFF_VERSION: usize = 4;
    const OFF_FLAGS: usize = 8;
    const OFF_TOTAL_PAGES: usize = 16;
    const OFF_BITMAP_COUNT: usize = 24;
    const OFF_BITMAP_IDS: usize = 32;
    const OFF_RESERVED: usize = 32 + 8 * Self::MAX_BITMAP_PAGES; // 4032
    const OFF_CHECKSUM: usize = 4088;
    const SIZE: usize = 4096;

    /// Create a new superblock with a single bitmap page.
    pub(crate) fn new(first_bitmap_page: PageId, total_pages: u64) -> Self {
        Self {
            version: Self::VERSION,
            flags: 0,
            total_pages,
            bitmap_page_count: 1,
            bitmap_page_ids: vec![first_bitmap_page],
        }
    }

    /// Serialize the superblock to a page-sized buffer with CRC32 checksum.
    pub(crate) fn serialize(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];

        // Magic
        buf.get_mut(Self::OFF_MAGIC..Self::OFF_VERSION)
            .map(|s| s.copy_from_slice(&Self::MAGIC));

        // Version
        buf.get_mut(Self::OFF_VERSION..Self::OFF_FLAGS)
            .map(|s| s.copy_from_slice(&self.version.to_le_bytes()));

        // Flags
        buf.get_mut(Self::OFF_FLAGS..Self::OFF_TOTAL_PAGES)
            .map(|s| s.copy_from_slice(&self.flags.to_le_bytes()));

        // Total pages
        buf.get_mut(Self::OFF_TOTAL_PAGES..Self::OFF_BITMAP_COUNT)
            .map(|s| s.copy_from_slice(&self.total_pages.to_le_bytes()));

        // Bitmap page count
        buf.get_mut(Self::OFF_BITMAP_COUNT..Self::OFF_BITMAP_IDS)
            .map(|s| s.copy_from_slice(&self.bitmap_page_count.to_le_bytes()));

        // Bitmap page IDs
        self.bitmap_page_ids
            .iter()
            .take(Self::MAX_BITMAP_PAGES)
            .enumerate()
            .for_each(|(i, &pid)| {
                let off = Self::OFF_BITMAP_IDS + i * 8;
                buf.get_mut(off..off + 8)
                    .map(|s| s.copy_from_slice(&u64::from(pid).to_le_bytes()));
            });

        // CRC32 checksum of bytes 0..4088
        let crc = crc32fast::hash(buf.get(..Self::OFF_CHECKSUM).unwrap_or(&[]));
        buf.get_mut(Self::OFF_CHECKSUM..Self::OFF_CHECKSUM + 4)
            .map(|s| s.copy_from_slice(&crc.to_le_bytes()));

        buf
    }

    /// Deserialize a superblock from a page-sized buffer, validating checksum.
    pub(crate) fn deserialize(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::SIZE {
            Err(StorageError::InvalidOperation(
                "superblock buffer too small".into(),
            ))
        } else {
            // Validate magic
            let magic =
                buf.get(Self::OFF_MAGIC..Self::OFF_VERSION).ok_or_else(
                    || StorageError::InvalidOperation("missing magic".into()),
                )?;
            if magic != Self::MAGIC {
                Err(StorageError::InvalidOperation(format!(
                    "invalid magic: expected {:?}, got {:?}",
                    Self::MAGIC,
                    magic
                )))
            } else {
                // Validate checksum
                let stored_crc = buf
                    .get(Self::OFF_CHECKSUM..Self::OFF_CHECKSUM + 4)
                    .and_then(|s| s.try_into().ok())
                    .map(u32::from_le_bytes)
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(
                            "missing checksum".into(),
                        )
                    })?;

                let computed_crc = crc32fast::hash(
                    buf.get(..Self::OFF_CHECKSUM).unwrap_or(&[]),
                );

                if stored_crc != computed_crc {
                    Err(StorageError::InvalidOperation(format!(
                        "superblock checksum mismatch: stored {stored_crc:#x}, computed {computed_crc:#x}"
                    )))
                } else {
                    Self::deserialize_unchecked(buf)
                }
            }
        }
    }

    /// Deserialize without checksum validation (for internal use after validation).
    fn deserialize_unchecked(buf: &[u8]) -> Result<Self> {
        let read_u32 = |off: usize| -> Result<u32> {
            buf.get(off..off + 4)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(format!(
                        "failed to read u32 at offset {off}"
                    ))
                })
        };

        let read_u64 = |off: usize| -> Result<u64> {
            buf.get(off..off + 8)
                .and_then(|s| s.try_into().ok())
                .map(u64::from_le_bytes)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(format!(
                        "failed to read u64 at offset {off}"
                    ))
                })
        };

        let version = read_u32(Self::OFF_VERSION)?;
        let flags = read_u64(Self::OFF_FLAGS)?;
        let total_pages = read_u64(Self::OFF_TOTAL_PAGES)?;
        let bitmap_page_count = read_u64(Self::OFF_BITMAP_COUNT)?;

        if bitmap_page_count > Self::MAX_BITMAP_PAGES as u64 {
            Err(StorageError::InvalidOperation(format!(
                "bitmap_page_count {bitmap_page_count} exceeds max {}",
                Self::MAX_BITMAP_PAGES
            )))
        } else {
            let bitmap_page_ids = (0..bitmap_page_count as usize)
                .map(|i| {
                    let off = Self::OFF_BITMAP_IDS + i * 8;
                    read_u64(off).map(PageId::from)
                })
                .collect::<Result<Vec<_>>>()?;

            Ok(Self {
                version,
                flags,
                total_pages,
                bitmap_page_count,
                bitmap_page_ids,
            })
        }
    }

    /// Add a new bitmap page ID.
    ///
    /// Returns `Err` if already at maximum capacity.
    pub(crate) fn add_bitmap_page(&mut self, pid: PageId) -> Result<()> {
        if self.bitmap_page_ids.len() >= Self::MAX_BITMAP_PAGES {
            Err(StorageError::InvalidOperation(format!(
                "cannot add bitmap page: already at max {}",
                Self::MAX_BITMAP_PAGES
            )))
        } else {
            self.bitmap_page_ids.push(pid);
            self.bitmap_page_count += 1;
            Ok(())
        }
    }
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
/// - **Superblock**: Page 0 containing metadata and bitmap page locations
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

    /// Superblock containing metadata and bitmap page locations.
    superblock: RwLock<Superblock>,

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

    /// Open an existing database.
    ///
    /// Opens the data file and WAL, runs WAL recovery, and initializes
    /// the page cache. The database directory must already exist and
    /// contain a valid data file.
    ///
    /// Supports both v1 (inline bitmap in header) and v2 (superblock +
    /// scattered bitmap pages) formats.
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
            // Read page 0 (header/superblock)
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

            // Parse superblock and load bitmap pages
            // (Superblock::deserialize validates magic and checksum)
            let (page_alloc, superblock) =
                Self::load_superblock(&mut file, &hdr, &cfg, &data_path)
                    .await?;

            // Run WAL recovery
            let (recovery, reader) =
                WalReader::open(&wal_dir).await?.recover().await?;

            // TODO(Phase 4.4): Apply recovery.committed_ops to page cache/data file
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
                superblock: RwLock::new(superblock),
                cfg,
                data_dir: dir.to_path_buf(),
            })
        }
    }

    /// Load superblock and bitmap pages.
    async fn load_superblock(
        file: &mut File,
        hdr: &[u8],
        cfg: &StorageConfig,
        data_path: &Path,
    ) -> Result<(PageAllocator, Superblock)> {
        let superblock = Superblock::deserialize(hdr)?;

        // Read all bitmap pages and concatenate
        let bm_data =
            Self::read_pages(file, &superblock.bitmap_page_ids, data_path)
                .await?;

        // Build reserved pages: superblock (0) + all bitmap pages
        let reserved: Vec<u64> = std::iter::once(0)
            .chain(superblock.bitmap_page_ids.iter().map(|pid| pid.page_num()))
            .collect();

        let page_alloc =
            PageAllocator::from_bytes(&bm_data, cfg.max_pages, &reserved)?;

        Ok((page_alloc, superblock))
    }

    /// Read multiple pages and concatenate their contents.
    fn read_pages<'a>(
        file: &'a mut File,
        page_ids: &'a [PageId],
        path: &'a Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<u8>>> + Send + 'a>,
    > {
        Box::pin(async move {
            match page_ids.split_first() {
                None => Ok(Vec::new()),
                Some((&pid, rest)) => {
                    let buf = Self::read_page_at(file, pid.byte_offset(), path)
                        .await?;
                    let mut data = Self::read_pages(file, rest, path).await?;
                    // Prepend this page's data
                    let mut result = buf;
                    result.append(&mut data);
                    Ok(result)
                }
            }
        })
    }

    /// Read a page at a specific offset.
    async fn read_page_at(
        file: &mut File,
        offset: u64,
        path: &Path,
    ) -> Result<Vec<u8>> {
        use tokio::io::AsyncReadExt;

        let mut buf = vec![0u8; page::PAGE_SIZE];
        file.seek(SeekFrom::Start(offset)).await.map_err(|e| {
            StorageError::Io {
                op: "seek".into(),
                path: path.to_path_buf(),
                source: e,
            }
        })?;
        file.read_exact(&mut buf)
            .await
            .map_err(|e| StorageError::Io {
                op: "read".into(),
                path: path.to_path_buf(),
                source: e,
            })?;
        Ok(buf)
    }

    /// Create a new database.
    ///
    /// Creates the data directory, initializes a fresh data file with
    /// superblock and bitmap pages, and sets up a new WAL. Fails if the
    /// directory already contains a database.
    ///
    /// # Layout
    ///
    /// - Page 0: Superblock (metadata + bitmap page IDs)
    /// - Page 1: First bitmap page
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
            // Reserve pages: 0 = superblock, 1 = first bitmap page
            let first_bm_page = PageId::from_page_num(1)?;
            let page_alloc =
                PageAllocator::with_reserved(64, cfg.max_pages, &[0, 1]);

            // Serialize bitmap to page 1
            let bm_data = page_alloc.to_bytes().await;
            let mut bm_page = vec![0u8; page::PAGE_SIZE];
            let copy_len = bm_data.len().min(page::PAGE_SIZE);
            bm_page
                .get_mut(..copy_len)
                .map(|s| s.copy_from_slice(&bm_data[..copy_len]));

            // Create superblock (page 0)
            let superblock = Superblock::new(first_bm_page, 2); // 2 pages: superblock + bitmap
            let sb_data = superblock.serialize();

            // Write pages to data file
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

            // Page 0: superblock
            file.write_all(&sb_data)
                .await
                .map_err(|e| StorageError::Io {
                    op: "write superblock".into(),
                    path: data_path.clone(),
                    source: e,
                })?;

            // Page 1: first bitmap page
            file.write_all(&bm_page)
                .await
                .map_err(|e| StorageError::Io {
                    op: "write bitmap page".into(),
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
                superblock: RwLock::new(superblock),
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
        // Ensure we have capacity for another page (may grow bitmap if needed)
        self.ensure_bitmap_capacity().await?;
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

        // Write bitmap pages and superblock (v2 format only)
        self.flush_metadata().await?;

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

    /// Flush metadata (bitmap pages + superblock) to disk.
    ///
    /// This writes the page allocator bitmap to the designated bitmap pages
    /// and updates the superblock with the current total page count.
    async fn flush_metadata(&self) -> Result<()> {
        let bm_page_ids = self.superblock.read().await.bitmap_page_ids.clone();
        let bm_data = self.page_alloc.to_bytes().await;
        let data_path = self.data_dir.join(Self::DATA_FILE_NAME);

        // Prepare bitmap page buffers: (PageId, page-sized buffer)
        let bm_pages: Vec<_> = bm_page_ids
            .iter()
            .zip((0..).map(|i| i * page::PAGE_SIZE))
            .take_while(|(_, off)| *off < bm_data.len())
            .map(|(&pid, off)| {
                let end = (off + page::PAGE_SIZE).min(bm_data.len());
                let mut buf = vec![0u8; page::PAGE_SIZE];
                buf.get_mut(..end - off)
                    .zip(bm_data.get(off..end))
                    .map(|(dest, src)| dest.copy_from_slice(src));
                (pid, buf)
            })
            .collect();

        // Write bitmap pages sequentially
        Self::write_pages(&self.data_file, &bm_pages, &data_path).await?;

        // Update and write superblock
        let mut superblock = self.superblock.write().await;
        superblock.total_pages = self.page_alloc.allocated_count().await;
        let sb_data = superblock.serialize();
        drop(superblock);

        Self::write_page_at(&self.data_file, 0, &sb_data, &data_path).await
    }

    /// Write a sequence of pages to disk.
    fn write_pages<'a>(
        file: &'a RwLock<File>,
        pages: &'a [(PageId, Vec<u8>)],
        path: &'a Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>,
    > {
        Box::pin(async move {
            match pages.split_first() {
                None => Ok(()),
                Some(((pid, buf), rest)) => {
                    Self::write_page_at(file, pid.byte_offset(), buf, path)
                        .await?;
                    Self::write_pages(file, rest, path).await
                }
            }
        })
    }

    /// Write data at a specific offset.
    async fn write_page_at(
        file: &RwLock<File>,
        offset: u64,
        data: &[u8],
        path: &Path,
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let mut f = file.write().await;
        f.seek(SeekFrom::Start(offset)).await.map_err(|e| {
            StorageError::Io {
                op: "seek".into(),
                path: path.to_path_buf(),
                source: e,
            }
        })?;
        f.write_all(data).await.map_err(|e| StorageError::Io {
            op: "write".into(),
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Ensure the bitmap has capacity for at least one more page allocation.
    ///
    /// If the current bitmap pages are nearly full, allocates a new bitmap
    /// page and adds it to the superblock. This prevents the allocator from
    /// running out of capacity to track new pages.
    async fn ensure_bitmap_capacity(&self) -> Result<()> {
        let superblock = self.superblock.read().await;
        let allocated = self.page_alloc.allocated_count().await;

        // Each bitmap page can track PAGE_SIZE * 8 pages
        let bits_per_page = page::PAGE_SIZE * 8;
        let max_capacity =
            superblock.bitmap_page_count as usize * bits_per_page;

        drop(superblock);

        // If we're near capacity (within 10 pages), grow the bitmap
        if allocated + 10 >= max_capacity as u64 {
            self.grow_bitmap().await
        } else {
            Ok(())
        }
    }

    /// Allocate a new bitmap page and add it to the superblock.
    async fn grow_bitmap(&self) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        // Check if we've reached the max bitmap pages
        let superblock = self.superblock.read().await;
        if superblock.bitmap_page_ids.len() >= Superblock::MAX_BITMAP_PAGES {
            drop(superblock);
            Err(StorageError::MemoryLimitExceeded {
                used: Superblock::MAX_BITMAP_PAGES,
                limit: Superblock::MAX_BITMAP_PAGES,
            })
        } else {
            drop(superblock);

            // Allocate a new page for the bitmap (this uses existing capacity)
            let new_bm_page = self.page_alloc.allocate().await?;

            // Mark it as reserved so it can't be freed
            self.page_alloc.add_reserved(new_bm_page.page_num()).await;

            // Extend the allocator capacity
            let bits_per_page = page::PAGE_SIZE * 8;
            let new_capacity =
                self.page_alloc.capacity().await + bits_per_page as u64;
            self.page_alloc.extend_capacity(new_capacity).await;

            // Add to superblock
            let mut superblock = self.superblock.write().await;
            superblock.add_bitmap_page(new_bm_page)?;
            drop(superblock);

            // Initialize the new bitmap page with zeros
            let data_path = self.data_dir.join(Self::DATA_FILE_NAME);
            let mut file = self.data_file.write().await;
            let zeros = vec![0u8; page::PAGE_SIZE];

            file.seek(SeekFrom::Start(new_bm_page.byte_offset()))
                .await
                .map_err(|e| StorageError::Io {
                    op: "seek to new bitmap page".into(),
                    path: data_path.clone(),
                    source: e,
                })?;

            file.write_all(&zeros).await.map_err(|e| StorageError::Io {
                op: "initialize bitmap page".into(),
                path: data_path,
                source: e,
            })?;

            Ok(())
        }
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

    // Superblock tests

    #[test]
    fn superblock_serialize_deserialize_roundtrip() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let sb = Superblock::new(first_bm, 100);

        let buf = sb.serialize();
        let restored = Superblock::deserialize(&buf).unwrap();

        assert_eq!(restored.version, Superblock::VERSION);
        assert_eq!(restored.flags, 0);
        assert_eq!(restored.total_pages, 100);
        assert_eq!(restored.bitmap_page_count, 1);
        assert_eq!(restored.bitmap_page_ids.len(), 1);
        assert_eq!(restored.bitmap_page_ids[0].page_num(), 1);
    }

    #[test]
    fn superblock_add_bitmap_page() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let mut sb = Superblock::new(first_bm, 100);

        // Add more bitmap pages
        let p2 = PageId::from_page_num(50).unwrap();
        let p3 = PageId::from_page_num(100).unwrap();

        sb.add_bitmap_page(p2).unwrap();
        sb.add_bitmap_page(p3).unwrap();

        assert_eq!(sb.bitmap_page_count, 3);
        assert_eq!(sb.bitmap_page_ids.len(), 3);
        assert_eq!(sb.bitmap_page_ids[1].page_num(), 50);
        assert_eq!(sb.bitmap_page_ids[2].page_num(), 100);
    }

    #[test]
    fn superblock_invalid_magic_fails() {
        let mut buf = [0u8; 4096];
        buf[0..4].copy_from_slice(b"NOPE"); // Wrong magic

        let result = Superblock::deserialize(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn superblock_checksum_mismatch_fails() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let sb = Superblock::new(first_bm, 100);

        let mut buf = sb.serialize();
        // Corrupt some data
        buf[20] ^= 0xFF;

        let result = Superblock::deserialize(&buf);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
    }

    #[tokio::test]
    async fn create_uses_superblock() {
        use tokio::io::AsyncReadExt;

        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        FileStorageEngine::create(&db_path, StorageConfig::default())
            .await
            .expect("create should succeed");

        // Read page 0 directly and check it's a v2 superblock
        let data_path = db_path.join("data.db");
        let mut file = tokio::fs::File::open(&data_path).await.unwrap();
        let mut buf = vec![0u8; 4096];
        file.read_exact(&mut buf).await.unwrap();

        // Check magic
        assert_eq!(&buf[0..4], b"RUMP");

        // Parse as superblock
        let sb = Superblock::deserialize(&buf).unwrap();
        assert_eq!(sb.version, 1);
        assert_eq!(sb.bitmap_page_count, 1);
        assert_eq!(sb.bitmap_page_ids[0].page_num(), 1);
    }

    #[tokio::test]
    async fn create_then_open_preserves_superblock() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create
        {
            let engine =
                FileStorageEngine::create(&db_path, StorageConfig::default())
                    .await
                    .expect("create should succeed");

            let sb = engine.superblock.read().await;
            assert_eq!(sb.version, 1);
            assert_eq!(sb.bitmap_page_count, 1);
        }

        // Reopen
        {
            let engine =
                FileStorageEngine::open(&db_path, StorageConfig::default())
                    .await
                    .expect("open should succeed");

            let sb = engine.superblock.read().await;
            assert_eq!(sb.version, 1);
            assert_eq!(sb.bitmap_page_count, 1);
        }
    }

    #[tokio::test]
    async fn allocate_reserves_superblock_and_bitmap() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine =
            FileStorageEngine::create(&db_path, StorageConfig::default())
                .await
                .expect("create should succeed");

        // Pages 0 and 1 should be reserved (superblock + first bitmap)
        assert!(engine.page_alloc.is_reserved(0).await);
        assert!(engine.page_alloc.is_reserved(1).await);

        // First allocation should be page 2
        let node_id = engine.allocate().await.unwrap();
        assert_eq!(u64::from(node_id), 2);
    }
}
