//! File-based storage engine with WAL and page cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::TryStreamExt;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::RwLock;

use super::{
    AsyncStorageEngine, GlobalRegistry, IndirectPage, MetadataPage,
    StorageConfig, StorageMetadata, Superblock,
};
use crate::error::{Result, StorageError};
use crate::node::{Node, NodeId};
use crate::page::{self, PageAllocator, PageCache, PageId};
use crate::wal::{SyncMode, WalReader, WalRecord, WalSequence, WalWriter};

/// Indirect page data loaded from disk during superblock loading.
struct LoadedIndirectPages {
    single: Option<IndirectPage>,
    double: Option<(IndirectPage, Vec<IndirectPage>)>,
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
/// // Open existing (config restored from metadata)
/// let engine = FileStorageEngine::open(Path::new("./data")).await?;
///
/// // Create new (config persisted to metadata)
/// let engine = FileStorageEngine::create(Path::new("./data"), config, 3, None).await?;
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

    /// Single-indirect page contents (bitmap page IDs).
    /// `None` if no single-indirect page is allocated.
    single_indirect: RwLock<Option<IndirectPage>>,

    /// Double-indirect page contents (indirect page IDs + their contents).
    /// Outer Option is None if no double-indirect page is allocated.
    /// Inner Vec contains the indirect pages pointed to by double-indirect.
    double_indirect: RwLock<Option<(IndirectPage, Vec<IndirectPage>)>>,

    /// Database metadata (page size, creation time, etc.).
    metadata: RwLock<MetadataPage>,

    /// Global name to page registry chain.
    ///
    /// Stores all registry pages in order: `[(page_id, registry_page), ...]`.
    /// The first element corresponds to `superblock.registry_root`.
    /// Each registry page's `next_page` field points to the next in the chain.
    registry_chain: RwLock<Vec<(PageId, GlobalRegistry)>>,

    /// Configuration settings.
    cfg: StorageConfig,

    /// Path to the data directory.
    data_dir: PathBuf,

    /// Abort handle for the periodic sync task (if `SyncMode::Periodic`).
    ///
    /// When set, a background task is running that syncs the WAL at the
    /// configured interval. Call `shutdown()` to abort it.
    sync_task_abort: Option<tokio::task::AbortHandle>,
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
    pub(crate) async fn open(dir: &Path) -> Result<Self> {
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

            // Load metadata page first so we can use stored config
            let superblock_raw = Superblock::deserialize(&hdr)?;

            let metadata = match superblock_raw.metadata_root {
                Some(pid) => {
                    let buf = Self::read_page_at(
                        &mut file,
                        pid.byte_offset(),
                        &data_path,
                    )
                    .await?;
                    let meta = MetadataPage::deserialize(&buf)?;
                    meta.validate_runtime()?;
                    meta
                }
                None => {
                    // DB without metadata page - create default
                    MetadataPage::new(3)
                }
            };

            // Use stored config from metadata
            let cfg = metadata.to_storage_config();

            // Parse superblock and load bitmap pages (including indirect)
            let (page_alloc, superblock, indirect) =
                Self::load_superblock(&mut file, &hdr, &cfg, &data_path)
                    .await?;

            // Load registry chain (follows next_page links)
            let registry_chain = match superblock.registry_root {
                Some(pid) => {
                    Self::load_registry_chain(&mut file, pid, &data_path)
                        .await?
                }
                None => {
                    // DB without registry - create empty chain
                    Vec::new()
                }
            };

            // Run WAL recovery
            let (recovery, reader) =
                WalReader::open(&wal_dir).await?.recover().await?;

            // TODO(Phase 4.4): Apply recovery.committed_ops to page cache/data file
            let _ = recovery.uncommitted_txns;

            // Convert reader to writer using stored WAL config
            let wal = reader.into_writer(cfg.wal_config.clone()).await?;

            // Create page cache using stored cache size
            let cache = PageCache::new(cfg.cache_size);

            // Wrap WAL in Arc for potential sharing with sync task
            let wal = Arc::new(wal);

            // Spawn periodic sync task if configured
            let sync_task_abort =
                Self::maybe_spawn_sync_task(&cfg, Arc::clone(&wal));

            Ok(Self {
                data_file: Arc::new(RwLock::new(file)),
                wal,
                cache: Arc::new(cache),
                page_alloc: Arc::new(page_alloc),
                superblock: RwLock::new(superblock),
                single_indirect: RwLock::new(indirect.single),
                double_indirect: RwLock::new(indirect.double),
                metadata: RwLock::new(metadata),
                registry_chain: RwLock::new(registry_chain),
                cfg,
                data_dir: dir.to_path_buf(),
                sync_task_abort,
            })
        }
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
    pub(crate) async fn create(
        dir: &Path,
        cfg: StorageConfig,
        min_degree: u16,
        max_memory_bytes: Option<usize>,
    ) -> Result<Self> {
        use tokio::io::AsyncWriteExt;

        let data_path = dir.join(Self::DATA_FILE_NAME);
        let wal_dir = dir.join(Self::WAL_DIR_NAME);

        // Create directories
        fs::create_dir_all(dir)
            .await
            .map_err(|e| StorageError::Io {
                op: "create data directory".into(),
                path: dir.to_path_buf(),
                source: e,
            })?;

        fs::create_dir_all(&wal_dir)
            .await
            .map_err(|e| StorageError::Io {
                op: "create WAL directory".into(),
                path: wal_dir.clone(),
                source: e,
            })?;

        // Fail if data file already exists
        if data_path.exists() {
            Err(StorageError::InvalidOperation(format!(
                "database already exists at {}",
                data_path.display()
            )))
        } else {
            // Reserve pages: 0=superblock, 1=bitmap, 2=metadata, 3=registry
            let first_bm_page = PageId::from_page_num(1)?;
            let metadata_page_id = PageId::from_page_num(2)?;
            let registry_page_id = PageId::from_page_num(3)?;

            let page_alloc = PageAllocator::new(64, cfg.max_pages);

            // Serialize bitmap to page 1
            let bm_data = page_alloc.to_bytes().await;
            let copy_len = bm_data.len().min(page::PAGE_SIZE);
            let mut bm_page = vec![0u8; page::PAGE_SIZE];
            bm_page
                .get_mut(..copy_len)
                .map(|s| s.copy_from_slice(&bm_data[..copy_len]));

            // Create metadata page (page 2) with full config
            let metadata =
                MetadataPage::from_config(min_degree, &cfg, max_memory_bytes);
            let meta_data = metadata.serialize();

            // Create registry page (page 3)
            let registry = GlobalRegistry::new();
            let reg_data = registry.serialize();
            let registry_chain = vec![(registry_page_id, registry)];

            // Create superblock (page 0) with pointers to metadata & registry
            let mut superblock = Superblock::new(first_bm_page, 4); // 4 pages allocated
            superblock.metadata_root = Some(metadata_page_id);
            superblock.registry_root = Some(registry_page_id);
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

            // Page 2: metadata page
            file.write_all(&meta_data)
                .await
                .map_err(|e| StorageError::Io {
                    op: "write metadata page".into(),
                    path: data_path.clone(),
                    source: e,
                })?;

            // Page 3: registry page
            file.write_all(&reg_data)
                .await
                .map_err(|e| StorageError::Io {
                    op: "write registry page".into(),
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

            // Wrap WAL in Arc for potential sharing with sync task
            let wal = Arc::new(wal);

            // Spawn periodic sync task if configured
            let sync_task_abort =
                Self::maybe_spawn_sync_task(&cfg, Arc::clone(&wal));

            Ok(Self {
                data_file: Arc::new(RwLock::new(file)),
                wal,
                cache: Arc::new(cache),
                page_alloc: Arc::new(page_alloc),
                superblock: RwLock::new(superblock),
                single_indirect: RwLock::new(None),
                double_indirect: RwLock::new(None),
                metadata: RwLock::new(metadata),
                registry_chain: RwLock::new(registry_chain),
                cfg,
                data_dir: dir.to_path_buf(),
                sync_task_abort,
            })
        }
    }

    /// Spawns a background task for periodic WAL sync if configured.
    ///
    /// Returns `Some(AbortHandle)` if a task was spawned, `None` otherwise.
    fn maybe_spawn_sync_task(
        cfg: &StorageConfig,
        wal: Arc<WalWriter>,
    ) -> Option<tokio::task::AbortHandle> {
        match cfg.wal_config.sync_mode {
            SyncMode::Periodic(interval) => {
                let task = tokio::spawn(async move {
                    let mut tick = tokio::time::interval(interval);
                    // First tick completes immediately, skip it
                    tick.tick().await;

                    loop {
                        tick.tick().await;
                        // Ignore sync errors in background task
                        // (errors will surface on next explicit sync)
                        let _ = wal.sync().await;
                    }
                });
                Some(task.abort_handle())
            }
            SyncMode::Immediate | SyncMode::OnCommit => None,
        }
    }

    /// Shuts down the periodic sync task if running.
    ///
    /// Call this before dropping the engine to ensure clean shutdown.
    pub(crate) fn shutdown_sync_task(&self) {
        if let Some(handle) = &self.sync_task_abort {
            handle.abort();
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

    async fn mark_dirty(&self, id: NodeId, node: &Node) -> Result<()> {
        let page_id = PageId::from(id);

        // Put in cache as dirty
        let evicted = self.cache.put(page_id, node.clone(), true).await;

        // Handle evicted dirty page - write to disk
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

        // Write registry chain pages
        self.flush_registry_chain().await?;

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
    /// Returns page cache statistics.
    pub(crate) async fn cache_stats(&self) -> page::PageCacheStats {
        self.cache.stats().await
    }

    /// Returns the stored B-tree min_degree from the metadata page.
    pub(crate) async fn min_degree(&self) -> u16 {
        self.metadata.read().await.min_degree
    }

    /// Returns the stored max_memory_bytes as `Option` (`0` means unlimited).
    pub(crate) async fn max_memory_bytes(&self) -> Option<usize> {
        self.metadata.read().await.max_memory_bytes_opt()
    }
}

impl FileStorageEngine {
    // ========== Private Helpers for open() ==========

    /// Load superblock and bitmap pages (including indirect pages).
    ///
    /// Returns `(allocator, superblock, single_indirect, double_indirect)`.
    async fn load_superblock(
        file: &mut File,
        hdr: &[u8],
        cfg: &StorageConfig,
        data_path: &Path,
    ) -> Result<(PageAllocator, Superblock, LoadedIndirectPages)> {
        let superblock = Superblock::deserialize(hdr)?;

        // Resolve all bitmap page IDs (direct + indirect)
        let (bm_page_ids, reserved, indirect_pages) =
            Self::resolve_bitmap_pages_full(file, &superblock, data_path)
                .await?;

        // Read all bitmap pages and concatenate
        let bm_data = Self::read_pages(file, &bm_page_ids, data_path).await?;

        let page_alloc =
            PageAllocator::from_bytes(&bm_data, cfg.max_pages, &reserved)?;

        Ok((page_alloc, superblock, indirect_pages))
    }

    /// Resolve all bitmap page IDs from direct slots and indirect pages.
    ///
    /// Returns `(bitmap_page_ids, reserved_page_nums, indirect_pages)` where
    /// reserved includes superblock, all bitmap pages, and all indirect pages.
    async fn resolve_bitmap_pages_full(
        file: &mut File,
        sb: &Superblock,
        path: &Path,
    ) -> Result<(Vec<PageId>, Vec<u64>, LoadedIndirectPages)> {
        // Start with direct bitmap pages
        let mut bm_ids: Vec<PageId> = sb.direct_bitmap_ids.clone();
        let mut reserved: Vec<u64> = sb.reserved_page_nums();

        // Single-indirect: read indirect page, extract bitmap page IDs
        let single_indirect = match sb.single_indirect {
            Some(single_pid) => {
                let buf =
                    Self::read_page_at(file, single_pid.byte_offset(), path)
                        .await?;
                let indirect = IndirectPage::deserialize(&buf)?;

                // Add single-indirect bitmap pages to reserved and bm_ids
                indirect.entries.iter().for_each(|&pid| {
                    reserved.push(pid.page_num());
                    bm_ids.push(pid);
                });

                Some(indirect)
            }
            None => None,
        };

        // Double-indirect: read indirect page, then each sub-indirect page
        let double_indirect = match sb.double_indirect {
            Some(double_pid) => {
                let buf =
                    Self::read_page_at(file, double_pid.byte_offset(), path)
                        .await?;
                let double_page = IndirectPage::deserialize(&buf)?;

                // Read each sub-indirect page
                let sub_pages = Self::load_double_indirect_entries(
                    file,
                    &double_page.entries,
                    path,
                    &mut bm_ids,
                    &mut reserved,
                )
                .await?;

                Some((double_page, sub_pages))
            }
            None => None,
        };

        let indirect = LoadedIndirectPages {
            single: single_indirect,
            double: double_indirect,
        };

        Ok((bm_ids, reserved, indirect))
    }

    /// Load double-indirect entries recursively.
    fn load_double_indirect_entries<'a>(
        file: &'a mut File,
        entries: &'a [PageId],
        path: &'a Path,
        bm_ids: &'a mut Vec<PageId>,
        reserved: &'a mut Vec<u64>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<IndirectPage>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            match entries.split_first() {
                None => Ok(Vec::new()),
                Some((&sub_pid, rest)) => {
                    // This entry is a sub-indirect page within double-indirect
                    reserved.push(sub_pid.page_num());

                    let buf =
                        Self::read_page_at(file, sub_pid.byte_offset(), path)
                            .await?;
                    let sub_page = IndirectPage::deserialize(&buf)?;

                    // Add all bitmap pages from this indirect page
                    sub_page.entries.iter().for_each(|&pid| {
                        reserved.push(pid.page_num());
                        bm_ids.push(pid);
                    });

                    let mut rest_pages = Self::load_double_indirect_entries(
                        file, rest, path, bm_ids, reserved,
                    )
                    .await?;

                    // Prepend this page
                    let mut result = vec![sub_page];
                    result.append(&mut rest_pages);
                    Ok(result)
                }
            }
        })
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

    /// Load the entire registry chain by following `next_page` links.
    ///
    /// Returns a vector of `(PageId, GlobalRegistry)` pairs in chain order.
    fn load_registry_chain<'a>(
        file: &'a mut File,
        start_pid: PageId,
        path: &'a Path,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Vec<(PageId, GlobalRegistry)>>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let buf =
                Self::read_page_at(file, start_pid.byte_offset(), path).await?;
            let reg = GlobalRegistry::deserialize(&buf)?;

            match reg.next_page {
                None => Ok(vec![(start_pid, reg)]),
                Some(next_pid) => {
                    let mut rest =
                        Self::load_registry_chain(file, next_pid, path).await?;
                    // Prepend this page at the front
                    let mut result = vec![(start_pid, reg)];
                    result.append(&mut rest);
                    Ok(result)
                }
            }
        })
    }

    // ========== Private Helpers for write operations ==========

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

    /// Flush metadata (bitmap pages + indirect pages + superblock) to disk.
    ///
    /// This writes the page allocator bitmap to all designated bitmap pages
    /// (direct + indirect) and updates the superblock with current total pages.
    async fn flush_metadata(&self) -> Result<()> {
        let superblock = self.superblock.read().await;
        let single_indirect = self.single_indirect.read().await;
        let double_indirect = self.double_indirect.read().await;

        // Collect all bitmap page IDs in order
        let bm_page_ids = Self::collect_all_bitmap_ids(
            &superblock,
            single_indirect.as_ref(),
            double_indirect.as_ref(),
        );
        drop(single_indirect);
        drop(double_indirect);
        drop(superblock);

        let bm_data = self.page_alloc.to_bytes().await;
        let data_path = self.data_dir.join(Self::DATA_FILE_NAME);

        // Prepare bitmap page buffers: (PageId, page-sized buffer)
        let bm_pages: Vec<(PageId, Vec<u8>)> = bm_page_ids
            .iter()
            .zip((0usize..).map(|i| i * page::PAGE_SIZE))
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

        // Write indirect pages if present
        self.flush_indirect_pages(&data_path).await?;

        // Update and write superblock
        let mut superblock = self.superblock.write().await;
        superblock.total_pages = self.page_alloc.allocated_count().await;
        let sb_data = superblock.serialize();
        drop(superblock);

        Self::write_page_at(&self.data_file, 0, &sb_data, &data_path).await
    }

    /// Collect all bitmap page IDs from direct slots and indirect pages.
    fn collect_all_bitmap_ids(
        sb: &Superblock,
        si: Option<&IndirectPage>,
        di: Option<&(IndirectPage, Vec<IndirectPage>)>,
    ) -> Vec<PageId> {
        let mut ids = sb.direct_bitmap_ids.clone();

        // Add single-indirect entries
        si.iter()
            .for_each(|p| ids.extend(p.entries.iter().copied()));

        // Add double-indirect entries (each sub-indirect page's entries)
        di.iter().for_each(|(_, subs)| {
            subs.iter().for_each(|sub| {
                ids.extend(sub.entries.iter().copied());
            });
        });

        ids
    }

    /// Flush indirect pages to disk.
    async fn flush_indirect_pages(&self, data_path: &Path) -> Result<()> {
        let superblock = self.superblock.read().await;
        let single_indirect = self.single_indirect.read().await;
        let double_indirect = self.double_indirect.read().await;

        // Write single-indirect page if present
        if let (Some(single_pid), Some(single_page)) =
            (superblock.single_indirect, single_indirect.as_ref())
        {
            let buf = single_page.serialize();
            Self::write_page_at(
                &self.data_file,
                single_pid.byte_offset(),
                &buf,
                data_path,
            )
            .await?;
        }

        // Write double-indirect page and sub-pages if present
        if let (Some(double_pid), Some((double_page, sub_pages))) =
            (superblock.double_indirect, double_indirect.as_ref())
        {
            // Write the double-indirect page itself
            let double_buf = double_page.serialize();
            Self::write_page_at(
                &self.data_file,
                double_pid.byte_offset(),
                &double_buf,
                data_path,
            )
            .await?;

            // Write each sub-indirect page
            Self::write_sub_indirect_pages(
                &self.data_file,
                &double_page.entries,
                sub_pages,
                data_path,
            )
            .await?;
        }

        Ok(())
    }

    /// Write sub-indirect pages recursively.
    fn write_sub_indirect_pages<'a>(
        file: &'a RwLock<File>,
        pids: &'a [PageId],
        pages: &'a [IndirectPage],
        path: &'a Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>,
    > {
        Box::pin(async move {
            match (pids.split_first(), pages.split_first()) {
                (Some((&pid, rest_pids)), Some((page, rest_pages))) => {
                    let buf = page.serialize();
                    Self::write_page_at(file, pid.byte_offset(), &buf, path)
                        .await?;
                    Self::write_sub_indirect_pages(
                        file, rest_pids, rest_pages, path,
                    )
                    .await
                }
                _ => Ok(()),
            }
        })
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
    /// page and adds it to the superblock (or indirect pages). This prevents
    /// the allocator from running out of capacity to track new pages.
    async fn ensure_bitmap_capacity(&self) -> Result<()> {
        let superblock = self.superblock.read().await;
        let single_indirect = self.single_indirect.read().await;
        let double_indirect = self.double_indirect.read().await;
        let allocated = self.page_alloc.allocated_count().await;

        // Count total bitmap pages across direct + indirect
        let direct_count = superblock.direct_bitmap_count as usize;
        let single_count = single_indirect.as_ref().map_or(0, |p| p.len());
        let double_count = double_indirect
            .as_ref()
            .map_or(0, |(_, subs)| subs.iter().map(|s| s.len()).sum());
        let total_bm_pages = direct_count + single_count + double_count;

        drop(single_indirect);
        drop(double_indirect);
        drop(superblock);

        // Each bitmap page can track PAGE_SIZE * 8 pages
        let bits_per_page = page::PAGE_SIZE * 8;
        let max_capacity = total_bm_pages * bits_per_page;

        // If we're near capacity (within 10 pages), grow the bitmap
        if allocated + 10 >= max_capacity as u64 {
            self.grow_bitmap().await
        } else {
            Ok(())
        }
    }

    /// Allocate a new bitmap page and add it to the appropriate location.
    ///
    /// Allocation priority:
    /// 1. Direct slots in superblock (up to 400)
    /// 2. Single-indirect page (up to 512)
    /// 3. Double-indirect pages (up to 512 × 512)
    async fn grow_bitmap(&self) -> Result<()> {
        let superblock = self.superblock.read().await;
        let can_use_direct = superblock.direct_bitmap_ids.len()
            < Superblock::MAX_DIRECT_BITMAP_PAGES;
        drop(superblock);

        if can_use_direct {
            // Add to direct slots
            self.grow_bitmap_direct().await
        } else {
            // Need to use indirect pages
            self.grow_bitmap_indirect().await
        }
    }

    /// Add a bitmap page to direct slots.
    async fn grow_bitmap_direct(&self) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        // Allocate a new page for the bitmap
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
        superblock.add_direct_bitmap_page(new_bm_page)?;
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

    /// Add a bitmap page to indirect slots.
    async fn grow_bitmap_indirect(&self) -> Result<()> {
        let superblock = self.superblock.read().await;
        let mut single_indirect = self.single_indirect.write().await;
        let single_not_exists = superblock.single_indirect.is_none();
        drop(superblock);

        let bits_per_page = page::PAGE_SIZE * 8;

        match single_indirect.as_mut() {
            None if single_not_exists => {
                // Create single-indirect page
                let single_page_id = self.page_alloc.allocate().await?;
                self.page_alloc
                    .add_reserved(single_page_id.page_num())
                    .await;

                // Allocate the actual bitmap page
                let new_bm_page = self.page_alloc.allocate().await?;
                self.page_alloc.add_reserved(new_bm_page.page_num()).await;

                // Extend capacity
                let new_capacity =
                    self.page_alloc.capacity().await + bits_per_page as u64;
                self.page_alloc.extend_capacity(new_capacity).await;

                // Create indirect page with the new bitmap page
                let mut new_single = IndirectPage::new();
                new_single.push(new_bm_page)?;
                *single_indirect = Some(new_single);

                // Update superblock
                let mut superblock = self.superblock.write().await;
                superblock.set_single_indirect(single_page_id);
                drop(superblock);
                drop(single_indirect);

                // Initialize pages with zeros
                self.initialize_page(new_bm_page).await?;
                self.initialize_page(single_page_id).await
            }
            Some(page) if !page.is_full() => {
                // Add to existing single-indirect
                let new_bm_page = self.page_alloc.allocate().await?;
                self.page_alloc.add_reserved(new_bm_page.page_num()).await;

                // Extend capacity
                let new_capacity =
                    self.page_alloc.capacity().await + bits_per_page as u64;
                self.page_alloc.extend_capacity(new_capacity).await;

                // Add to indirect page
                page.push(new_bm_page)?;
                drop(single_indirect);

                // Initialize the new bitmap page
                self.initialize_page(new_bm_page).await
            }
            _ => {
                // Single-indirect is full (or inconsistent state), need double-indirect
                drop(single_indirect);
                self.grow_bitmap_double_indirect().await
            }
        }
    }

    /// Add a bitmap page via double-indirect.
    async fn grow_bitmap_double_indirect(&self) -> Result<()> {
        let superblock = self.superblock.read().await;
        let mut double_indirect = self.double_indirect.write().await;

        let double_exists = superblock.double_indirect.is_some();
        drop(superblock);

        let bits_per_page = page::PAGE_SIZE * 8;

        if !double_exists {
            // Create double-indirect structure from scratch
            let double_page_id = self.page_alloc.allocate().await?;
            self.page_alloc
                .add_reserved(double_page_id.page_num())
                .await;

            let sub_page_id = self.page_alloc.allocate().await?;
            self.page_alloc.add_reserved(sub_page_id.page_num()).await;

            let new_bm_page = self.page_alloc.allocate().await?;
            self.page_alloc.add_reserved(new_bm_page.page_num()).await;

            // Extend capacity
            let new_cap =
                self.page_alloc.capacity().await + bits_per_page as u64;
            self.page_alloc.extend_capacity(new_cap).await;

            // Create sub-indirect with the bitmap page
            let mut sub_indirect = IndirectPage::new();
            sub_indirect.push(new_bm_page)?;

            // Create double-indirect with the sub-indirect
            let mut double_page = IndirectPage::new();
            double_page.push(sub_page_id)?;

            *double_indirect = Some((double_page, vec![sub_indirect]));

            // Update superblock
            let mut superblock = self.superblock.write().await;
            superblock.set_double_indirect(double_page_id);
            drop(superblock);
            drop(double_indirect);

            // Initialize all pages
            self.initialize_page(double_page_id).await?;
            self.initialize_page(sub_page_id).await?;
            self.initialize_page(new_bm_page).await
        } else {
            // Double-indirect exists, check sub-indirects
            let (double_page, sub_pages): &mut (
                IndirectPage,
                Vec<IndirectPage>,
            ) = double_indirect.as_mut().ok_or_else(|| {
                StorageError::InvalidOperation(
                    "double_indirect mismatch".into(),
                )
            })?;

            let last_sub_full =
                sub_pages.last().is_none_or(IndirectPage::is_full);

            if !last_sub_full {
                // Add bitmap to last sub-indirect
                let new_bm_page = self.page_alloc.allocate().await?;
                self.page_alloc.add_reserved(new_bm_page.page_num()).await;

                let new_cap =
                    self.page_alloc.capacity().await + bits_per_page as u64;
                self.page_alloc.extend_capacity(new_cap).await;

                sub_pages
                    .last_mut()
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(
                            "no sub-indirect pages".into(),
                        )
                    })?
                    .push(new_bm_page)?;

                drop(double_indirect);
                self.initialize_page(new_bm_page).await
            } else if !double_page.is_full() {
                // Create new sub-indirect
                let sub_page_id = self.page_alloc.allocate().await?;
                self.page_alloc.add_reserved(sub_page_id.page_num()).await;

                let new_bm_page = self.page_alloc.allocate().await?;
                self.page_alloc.add_reserved(new_bm_page.page_num()).await;

                let new_cap =
                    self.page_alloc.capacity().await + bits_per_page as u64;
                self.page_alloc.extend_capacity(new_cap).await;

                // Create new sub-indirect with bitmap
                let mut new_sub = IndirectPage::new();
                new_sub.push(new_bm_page)?;

                double_page.push(sub_page_id)?;
                sub_pages.push(new_sub);

                drop(double_indirect);

                self.initialize_page(sub_page_id).await?;
                self.initialize_page(new_bm_page).await
            } else {
                // Both double-indirect and all sub-indirects are full
                let max = Superblock::MAX_DIRECT_BITMAP_PAGES
                    + Superblock::INDIRECT_ENTRIES_PER_PAGE
                    + Superblock::INDIRECT_ENTRIES_PER_PAGE
                        * Superblock::INDIRECT_ENTRIES_PER_PAGE;
                Err(StorageError::MemoryLimitExceeded {
                    used: max,
                    limit: max,
                })
            }
        }
    }

    /// Initialize a page with zeros on disk.
    async fn initialize_page(&self, pid: PageId) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let data_path = self.data_dir.join(Self::DATA_FILE_NAME);
        let mut file = self.data_file.write().await;
        let zeros = vec![0u8; page::PAGE_SIZE];

        file.seek(SeekFrom::Start(pid.byte_offset()))
            .await
            .map_err(|e| StorageError::Io {
                op: "seek to initialize page".into(),
                path: data_path.clone(),
                source: e,
            })?;

        file.write_all(&zeros).await.map_err(|e| StorageError::Io {
            op: "initialize page".into(),
            path: data_path,
            source: e,
        })?;

        Ok(())
    }

    // ========== Registry Chain Operations ==========

    /// Look up a global's root page by name across the entire registry chain.
    pub(crate) async fn registry_get(&self, name: &str) -> Option<PageId> {
        let chain = self.registry_chain.read().await;
        chain.iter().find_map(|(_, reg)| reg.get(name))
    }

    /// Insert or update a global's root page in the registry chain.
    ///
    /// If the entry exists in any page, it is updated. Otherwise, the entry
    /// is inserted into the first page with available space. If all pages are
    /// full, a new page is allocated and appended to the chain.
    pub(crate) async fn registry_insert(
        &self,
        name: String,
        root: PageId,
    ) -> Result<()> {
        let mut chain = self.registry_chain.write().await;

        // First, check if the name exists in any page (update case)
        let existing_idx =
            chain.iter().position(|(_, reg)| reg.get(&name).is_some());

        match existing_idx {
            Some(idx) => {
                // Update existing entry
                chain
                    .get_mut(idx)
                    .map(|(_, reg)| reg.insert(name.clone(), root))
                    .transpose()?
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(
                            "registry chain index out of bounds".into(),
                        )
                    })
            }
            None => {
                // Find first page with space for a new entry
                let has_space_idx =
                    chain.iter().position(|(_, reg)| reg.can_insert(&name));

                match has_space_idx {
                    Some(idx) => chain
                        .get_mut(idx)
                        .map(|(_, reg)| {
                            reg.insert_unchecked(name.clone(), root)
                        })
                        .ok_or_else(|| {
                            StorageError::InvalidOperation(
                                "registry chain index out of bounds".into(),
                            )
                        }),
                    None => {
                        // All pages full - need to allocate a new one
                        drop(chain);
                        self.registry_chain_extend(name, root).await
                    }
                }
            }
        }
    }

    /// Allocate a new registry page and append it to the chain.
    ///
    /// Note: Unlike the initial registry page (page 3), dynamically allocated
    /// registry pages are NOT marked as reserved. This allows them to be freed
    /// when the chain is compacted.
    ///
    /// This function handles the race condition where another thread may have
    /// already inserted the entry or extended the chain while we were allocating.
    async fn registry_chain_extend(
        &self,
        name: String,
        root: PageId,
    ) -> Result<()> {
        // Allocate a new page for the registry (before acquiring lock)
        // Note: We do NOT mark this as reserved so it can be freed on compaction
        let new_pid = self.page_alloc.allocate().await?;

        // Initialize the page on disk
        self.initialize_page(new_pid).await?;

        // Re-acquire the lock and check if another thread beat us
        let mut chain = self.registry_chain.write().await;

        // Race condition check #1: Entry may now exist (another thread inserted it)
        let existing_idx =
            chain.iter().position(|(_, reg)| reg.get(&name).is_some());

        if let Some(idx) = existing_idx {
            // Another thread inserted this entry - just update it and free our page
            drop(chain);
            self.page_alloc.free(new_pid).await?;
            let mut chain = self.registry_chain.write().await;
            chain
                .get_mut(idx)
                .map(|(_, reg)| reg.insert(name.clone(), root))
                .transpose()?
                .ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "registry chain index out of bounds".into(),
                    )
                })
        } else {
            // Race condition check #2: Space may now exist (another thread extended)
            let has_space_idx =
                chain.iter().position(|(_, reg)| reg.can_insert(&name));

            match has_space_idx {
                Some(idx) => {
                    // Space found in existing page - use it and free our page
                    chain.get_mut(idx).map(|(_, reg)| {
                        reg.insert_unchecked(name.clone(), root)
                    });
                    drop(chain);
                    self.page_alloc
                        .free(new_pid)
                        .await
                        .map_err(StorageError::from)
                }
                None => {
                    // No existing space - use our newly allocated page
                    let mut new_reg = GlobalRegistry::new();
                    new_reg.insert_unchecked(name, root);

                    // Update the previous last page's next_page pointer
                    chain
                        .last_mut()
                        .map(|(_, reg)| reg.next_page = Some(new_pid));

                    // Append the new page
                    chain.push((new_pid, new_reg));

                    Ok(())
                }
            }
        }
    }

    /// Remove a global from the registry chain.
    ///
    /// Searches all pages for the entry and removes it. If a page becomes
    /// empty after removal (and it's not the first page), it is unlinked
    /// from the chain and deallocated.
    pub(crate) async fn registry_remove(&self, name: &str) -> Result<()> {
        let mut chain = self.registry_chain.write().await;

        // Find which page contains the entry
        let containing_idx =
            chain.iter().position(|(_, reg)| reg.get(name).is_some());

        match containing_idx {
            None => Ok(()), // Entry doesn't exist, nothing to do
            Some(idx) => {
                // Remove from the page
                chain.get_mut(idx).map(|(_, reg)| reg.remove(name));

                // Check if page is now empty and can be compacted
                // (Don't compact the first page, even if empty)
                let should_compact = idx > 0
                    && chain.get(idx).is_some_and(|(_, reg)| reg.is_empty());

                if should_compact {
                    // Get the page to remove and its ID
                    let (removed_pid, removed_reg) =
                        chain.get(idx).cloned().ok_or_else(|| {
                            StorageError::InvalidOperation(
                                "registry chain index out of bounds".into(),
                            )
                        })?;

                    // Update previous page's next_page pointer to skip this one
                    chain.get_mut(idx - 1).map(|(_, prev_reg)| {
                        prev_reg.next_page = removed_reg.next_page;
                    });

                    // Remove from chain
                    chain.remove(idx);

                    // Deallocate the page
                    drop(chain);
                    self.page_alloc.free(removed_pid).await?;
                }

                Ok(())
            }
        }
    }

    /// Iterate over all entries in the registry chain.
    ///
    /// Returns an iterator over `(name, root_page_id)` pairs.
    pub(crate) async fn registry_entries(&self) -> Vec<(String, PageId)> {
        let chain = self.registry_chain.read().await;
        chain
            .iter()
            .flat_map(|(_, reg)| {
                reg.entries.iter().map(|e| (e.name.clone(), e.root))
            })
            .collect()
    }

    /// Append a record to the Write-Ahead Log.
    ///
    /// This is called by the Database layer to log logical operations (SET,
    /// KILL) before they are applied to the in-memory B-tree. The WAL ensures
    /// crash recovery and durability.
    ///
    /// Returns the WAL sequence number for the appended record.
    pub(crate) async fn wal_append(
        &self,
        rec: &WalRecord,
    ) -> Result<WalSequence> {
        self.wal.append(rec).await
    }

    /// Flush and sync the Write-Ahead Log to disk.
    ///
    /// This is called during transaction commit to ensure all WAL records
    /// are durably written to disk before the commit is acknowledged.
    pub(crate) async fn wal_sync(&self) -> Result<()> {
        self.wal.sync().await
    }

    /// Flush the entire registry chain to disk.
    async fn flush_registry_chain(&self) -> Result<()> {
        let chain = self.registry_chain.read().await;
        let data_path = self.data_dir.join(Self::DATA_FILE_NAME);

        // Write each registry page
        Self::write_registry_pages(&self.data_file, &chain, &data_path).await
    }

    /// Write registry pages recursively.
    fn write_registry_pages<'a>(
        file: &'a RwLock<File>,
        pages: &'a [(PageId, GlobalRegistry)],
        path: &'a Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>,
    > {
        Box::pin(async move {
            match pages.split_first() {
                None => Ok(()),
                Some(((pid, reg), rest)) => {
                    let buf = reg.serialize();
                    Self::write_page_at(file, pid.byte_offset(), &buf, path)
                        .await?;
                    Self::write_registry_pages(file, rest, path).await
                }
            }
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use futures::stream::{self, StreamExt};
    use tempfile::TempDir;

    use super::*;

    // ========== Test Helpers ==========

    /// Fills the first registry page with entries until full.
    ///
    /// Inserts entries named `"G{i:03}"` with root `PageId::from(i)`.
    /// Returns the number of entries inserted.
    async fn fill_first_registry_page(engine: &FileStorageEngine) -> usize {
        let mut count = 0usize;
        while let Some(()) = try_insert_registry_entry(engine, count).await {
            count += 1;
        }
        count
    }

    /// Tries to insert a single registry entry if the first page has space.
    ///
    /// Returns `Some(())` if inserted, `None` if page is full.
    async fn try_insert_registry_entry(
        engine: &FileStorageEngine,
        i: usize,
    ) -> Option<()> {
        let name = format!("G{i:03}");
        let can_fit = {
            let chain = engine.registry_chain.read().await;
            chain
                .first()
                .map_or(false, |(_, reg)| reg.can_insert(&name))
        };
        if can_fit {
            engine
                .registry_insert(name, PageId::from(i as u64))
                .await
                .unwrap();
            Some(())
        } else {
            None
        }
    }

    #[tokio::test]
    async fn open_nonexistent_dir_fails() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nonexistent");

        let result = FileStorageEngine::open(&path).await;

        // Should fail because data file doesn't exist
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn open_missing_data_file_fails() {
        let dir = TempDir::new().expect("temp dir");

        // Directory exists but no data.db file
        let result = FileStorageEngine::open(dir.path()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_new_database() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
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
        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create should succeed");
        drop(engine);

        // Reopen
        let engine = FileStorageEngine::open(&db_path)
            .await
            .expect("open should succeed");

        assert_eq!(engine.data_dir, db_path);
    }

    #[tokio::test]
    async fn create_existing_fails() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create first time
        FileStorageEngine::create(&db_path, StorageConfig::default(), 3, None)
            .await
            .expect("first create should succeed");

        // Create again should fail
        let result = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_uses_superblock() {
        use tokio::io::AsyncReadExt;

        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        FileStorageEngine::create(&db_path, StorageConfig::default(), 3, None)
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
        assert_eq!(sb.version, Superblock::VERSION);
        assert_eq!(sb.direct_bitmap_count, 1);
        assert_eq!(sb.direct_bitmap_ids[0].page_num(), 1);
    }

    #[tokio::test]
    async fn create_then_open_preserves_superblock() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create
        {
            let engine = FileStorageEngine::create(
                &db_path,
                StorageConfig::default(),
                3,
                None,
            )
            .await
            .expect("create should succeed");

            let sb = engine.superblock.read().await;
            assert_eq!(sb.version, Superblock::VERSION);
            assert_eq!(sb.direct_bitmap_count, 1);
        }

        // Reopen
        {
            let engine = FileStorageEngine::open(&db_path)
                .await
                .expect("open should succeed");

            let sb = engine.superblock.read().await;
            assert_eq!(sb.version, Superblock::VERSION);
            assert_eq!(sb.direct_bitmap_count, 1);
        }
    }

    #[tokio::test]
    async fn allocate_reserves_superblock_and_bitmap() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create should succeed");

        // Pages 0-3 should be reserved (superblock, bitmap, metadata, registry)
        assert!(engine.page_alloc.is_reserved(0).await);
        assert!(engine.page_alloc.is_reserved(1).await);
        assert!(engine.page_alloc.is_reserved(2).await);
        assert!(engine.page_alloc.is_reserved(3).await);

        // First allocation should be page 4
        let node_id = engine.allocate().await.unwrap();
        assert_eq!(u64::from(node_id), 4);
    }

    #[tokio::test]
    async fn create_then_open_preserves_metadata_and_registry() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create and drop engine
        {
            let engine = FileStorageEngine::create(
                &db_path,
                StorageConfig::default(),
                3,
                None,
            )
            .await
            .expect("create");

            // Verify metadata was created
            let meta = engine.metadata.read().await;
            assert_eq!(meta.version, 1);
            assert_eq!(meta.min_degree, 3);
            assert!(meta.created_at > 0);
        }

        // Reopen and verify
        let engine = FileStorageEngine::open(&db_path).await.expect("open");

        let meta = engine.metadata.read().await;
        assert_eq!(meta.version, 1);
        assert_eq!(meta.min_degree, 3);

        let chain = engine.registry_chain.read().await;
        assert_eq!(chain.len(), 1);
        assert!(chain.first().map_or(true, |(_, r)| r.entries.is_empty()));
    }

    #[test]
    fn collect_all_bitmap_ids_direct_only() {
        let mut sb = Superblock::new(PageId::from_page_num(1).unwrap(), 100);
        sb.add_direct_bitmap_page(PageId::from_page_num(2).unwrap())
            .unwrap();
        sb.add_direct_bitmap_page(PageId::from_page_num(3).unwrap())
            .unwrap();

        let ids = FileStorageEngine::collect_all_bitmap_ids(&sb, None, None);

        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0].page_num(), 1);
        assert_eq!(ids[1].page_num(), 2);
        assert_eq!(ids[2].page_num(), 3);
    }

    #[test]
    fn collect_all_bitmap_ids_with_single_indirect() {
        let mut sb = Superblock::new(PageId::from_page_num(1).unwrap(), 100);
        sb.add_direct_bitmap_page(PageId::from_page_num(2).unwrap())
            .unwrap();

        let mut single = IndirectPage::new();
        single.push(PageId::from_page_num(100).unwrap()).unwrap();
        single.push(PageId::from_page_num(101).unwrap()).unwrap();

        let ids =
            FileStorageEngine::collect_all_bitmap_ids(&sb, Some(&single), None);

        assert_eq!(ids.len(), 4);
        assert_eq!(ids[0].page_num(), 1);
        assert_eq!(ids[1].page_num(), 2);
        assert_eq!(ids[2].page_num(), 100);
        assert_eq!(ids[3].page_num(), 101);
    }

    #[test]
    fn collect_all_bitmap_ids_with_double_indirect() {
        let sb = Superblock::new(PageId::from_page_num(1).unwrap(), 100);

        let mut single = IndirectPage::new();
        single.push(PageId::from_page_num(100).unwrap()).unwrap();

        // Double-indirect: main page points to sub-indirect pages
        let mut sub1 = IndirectPage::new();
        sub1.push(PageId::from_page_num(200).unwrap()).unwrap();
        sub1.push(PageId::from_page_num(201).unwrap()).unwrap();

        let mut sub2 = IndirectPage::new();
        sub2.push(PageId::from_page_num(300).unwrap()).unwrap();

        // The main double-indirect page (entries are sub-indirect page IDs)
        let mut double_main = IndirectPage::new();
        double_main
            .push(PageId::from_page_num(50).unwrap())
            .unwrap(); // sub1's page
        double_main
            .push(PageId::from_page_num(51).unwrap())
            .unwrap(); // sub2's page

        let double = (double_main, vec![sub1, sub2]);

        let ids = FileStorageEngine::collect_all_bitmap_ids(
            &sb,
            Some(&single),
            Some(&double),
        );

        // direct (1) + single (100) + double sub1 (200, 201) + double sub2 (300)
        assert_eq!(ids.len(), 5);
        assert_eq!(ids[0].page_num(), 1);
        assert_eq!(ids[1].page_num(), 100);
        assert_eq!(ids[2].page_num(), 200);
        assert_eq!(ids[3].page_num(), 201);
        assert_eq!(ids[4].page_num(), 300);
    }

    // ---- Expensive integration test for indirect bitmap allocation ----
    //
    // Run with: cargo test -p rumps-storage single_indirect_allocation -- --ignored --nocapture
    //
    // Requirements:
    // - ~50 GB free disk space (sparse file, actual usage depends on filesystem)
    // - Less than one minute to minutes to run (allocates ~13M pages)
    //   - NOTE: On 1TB ZFS SSD, run time is 19.13s

    #[tokio::test]
    #[ignore = "expensive: ~50GB sparse file, potentially several minutes to run"]
    async fn single_indirect_allocation_and_roundtrip() {
        use std::time::Instant;

        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("large_db");

        // Each bitmap page tracks PAGE_SIZE * 8 = 32,768 pages
        // We have 400 direct bitmap slots
        // To trigger single-indirect, we need to exceed 400 bitmap pages
        // That means allocating > 400 * 32,768 = 13,107,200 pages
        let bits_per_bm_page = page::PAGE_SIZE * 8;
        let target_pages =
            Superblock::MAX_DIRECT_BITMAP_PAGES * bits_per_bm_page + 1000;

        println!(
            "Target: {} pages (~{} GB sparse file)",
            target_pages,
            (target_pages * page::PAGE_SIZE) / (1024 * 1024 * 1024)
        );

        // Create database
        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        let start = Instant::now();
        let mut last_report = 0usize;

        // Allocate pages until we exceed direct bitmap capacity
        // Note: pages 0-3 are reserved (superblock, bitmap, metadata, registry)
        // and first bitmap page is already allocated
        let mut allocated = 0usize;
        while allocated < target_pages {
            engine.allocate().await.expect("allocate");
            allocated += 1;

            // Progress report every 1M pages
            if allocated / 1_000_000 > last_report {
                last_report = allocated / 1_000_000;
                let elapsed = start.elapsed().as_secs();
                println!(
                    "  Allocated {}M pages in {}s ({} pages/sec)",
                    last_report,
                    elapsed,
                    if elapsed > 0 {
                        allocated as u64 / elapsed
                    } else {
                        0
                    }
                );
            }
        }

        println!(
            "Allocation complete: {} pages in {:?}",
            allocated,
            start.elapsed()
        );

        // Verify single-indirect is now set
        {
            let sb = engine.superblock.read().await;
            assert!(
                sb.single_indirect.is_some(),
                "single_indirect should be set after allocating {} pages",
                allocated
            );
            assert_eq!(
                sb.direct_bitmap_ids.len(),
                Superblock::MAX_DIRECT_BITMAP_PAGES,
                "should have exactly {} direct bitmap pages",
                Superblock::MAX_DIRECT_BITMAP_PAGES
            );
            println!(
                "Superblock: {} direct bitmap pages, single_indirect = {:?}",
                sb.direct_bitmap_ids.len(),
                sb.single_indirect
            );
        }

        // Verify single-indirect page has entries
        {
            let si = engine.single_indirect.read().await;
            assert!(si.is_some(), "single_indirect page should exist");
            let si_len = si.as_ref().unwrap().len();
            assert!(si_len > 0, "single_indirect should have entries");
            println!("Single-indirect page has {} entries", si_len);
        }

        // Flush to disk
        engine.flush().await.expect("flush");
        drop(engine);

        println!("Reopening database...");

        // Reopen and verify
        let engine = FileStorageEngine::open(&db_path).await.expect("reopen");

        {
            let sb = engine.superblock.read().await;
            assert!(
                sb.single_indirect.is_some(),
                "single_indirect should persist after reopen"
            );
            assert_eq!(
                sb.direct_bitmap_ids.len(),
                Superblock::MAX_DIRECT_BITMAP_PAGES
            );
        }

        {
            let si = engine.single_indirect.read().await;
            assert!(si.is_some());
            println!(
                "After reopen: single_indirect has {} entries",
                si.as_ref().unwrap().len()
            );
        }

        // Verify allocated count matches
        let reopened_count = engine.page_alloc.allocated_count().await;
        // +4 for reserved pages (superblock, first bitmap, metadata, registry)
        // plus additional bitmap pages allocated during growth
        assert!(
            reopened_count >= allocated as u64,
            "allocated count {} should be >= {}",
            reopened_count,
            allocated
        );

        println!(
            "Test passed! Allocated {} pages, single-indirect working.",
            reopened_count
        );
    }

    // ========== Registry Chaining Integration Tests ==========

    #[tokio::test]
    async fn registry_insert_and_get_single_page() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        // Insert a few globals
        engine
            .registry_insert("PATIENT".into(), PageId::from(100))
            .await
            .expect("insert PATIENT");
        engine
            .registry_insert("ORDER".into(), PageId::from(200))
            .await
            .expect("insert ORDER");
        engine
            .registry_insert("USER".into(), PageId::from(300))
            .await
            .expect("insert USER");

        // Verify we can get them back
        assert_eq!(
            engine.registry_get("PATIENT").await,
            Some(PageId::from(100))
        );
        assert_eq!(engine.registry_get("ORDER").await, Some(PageId::from(200)));
        assert_eq!(engine.registry_get("USER").await, Some(PageId::from(300)));
        assert_eq!(engine.registry_get("NONEXISTENT").await, None);
    }

    #[tokio::test]
    async fn registry_insert_update_existing() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        engine
            .registry_insert("PATIENT".into(), PageId::from(100))
            .await
            .expect("insert");
        engine
            .registry_insert("PATIENT".into(), PageId::from(999))
            .await
            .expect("update");

        assert_eq!(
            engine.registry_get("PATIENT").await,
            Some(PageId::from(999))
        );

        // Should still be only one entry
        let entries = engine.registry_entries().await;
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn registry_remove_entry() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        engine
            .registry_insert("A".into(), PageId::from(1))
            .await
            .unwrap();
        engine
            .registry_insert("B".into(), PageId::from(2))
            .await
            .unwrap();
        engine
            .registry_insert("C".into(), PageId::from(3))
            .await
            .unwrap();

        engine.registry_remove("B").await.unwrap();

        assert_eq!(engine.registry_get("A").await, Some(PageId::from(1)));
        assert_eq!(engine.registry_get("B").await, None);
        assert_eq!(engine.registry_get("C").await, Some(PageId::from(3)));
    }

    #[tokio::test]
    async fn registry_entries_iteration() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        engine
            .registry_insert("PATIENT".into(), PageId::from(100))
            .await
            .unwrap();
        engine
            .registry_insert("ORDER".into(), PageId::from(200))
            .await
            .unwrap();

        let entries = engine.registry_entries().await;
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&("PATIENT".into(), PageId::from(100))));
        assert!(entries.contains(&("ORDER".into(), PageId::from(200))));
    }

    #[tokio::test]
    async fn registry_persist_and_reopen() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create and populate
        {
            let engine = FileStorageEngine::create(
                &db_path,
                StorageConfig::default(),
                3,
                None,
            )
            .await
            .expect("create");

            engine
                .registry_insert("GLOBAL1".into(), PageId::from(100))
                .await
                .unwrap();
            engine
                .registry_insert("GLOBAL2".into(), PageId::from(200))
                .await
                .unwrap();

            engine.flush().await.expect("flush");
        }

        // Reopen and verify
        {
            let engine = FileStorageEngine::open(&db_path).await.expect("open");

            assert_eq!(
                engine.registry_get("GLOBAL1").await,
                Some(PageId::from(100))
            );
            assert_eq!(
                engine.registry_get("GLOBAL2").await,
                Some(PageId::from(200))
            );
        }
    }

    #[tokio::test]
    async fn registry_chain_overflow_to_second_page() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        // Fill the first page until it can't accept any more entries.
        // Each entry "G{i:03}" = 5 chars = 2 + 5 + 8 = 15 bytes.
        // MAX_ENTRIES_BYTES = 4070, so ~270 entries fit.
        let num_first_page = fill_first_registry_page(&engine).await;

        // Verify chain has only 1 page so far
        {
            let chain = engine.registry_chain.read().await;
            assert_eq!(
                chain.len(),
                1,
                "expected 1 page with {num_first_page} entries"
            );
        }

        // Add one more entry - should trigger chain extension
        engine
            .registry_insert("OVERFLOW".into(), PageId::from(9999))
            .await
            .expect("insert overflow");

        // Verify chain now has 2 pages
        {
            let chain = engine.registry_chain.read().await;
            assert_eq!(chain.len(), 2, "expected 2 pages after overflow");

            // Check first page has next_page set
            assert!(chain[0].1.next_page.is_some());
            // Check second page's ID matches first page's next_page
            assert_eq!(chain[0].1.next_page, Some(chain[1].0));
        }

        // Verify we can get entries from both pages
        assert_eq!(engine.registry_get("G000").await, Some(PageId::from(0)));
        assert_eq!(
            engine.registry_get("OVERFLOW").await,
            Some(PageId::from(9999))
        );
    }

    #[tokio::test]
    async fn registry_chain_persist_multiple_pages() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        // Create, fill, and persist
        let first_page_count = {
            let engine = FileStorageEngine::create(
                &db_path,
                StorageConfig::default(),
                3,
                None,
            )
            .await
            .expect("create");

            // Fill first page until it's full
            let count = fill_first_registry_page(&engine).await;

            // Add overflow entry
            engine
                .registry_insert("OVERFLOW".into(), PageId::from(9999))
                .await
                .unwrap();

            engine.flush().await.expect("flush");
            count
        };

        // Reopen and verify chain is loaded correctly
        {
            let engine = FileStorageEngine::open(&db_path).await.expect("open");

            // Check chain length
            {
                let chain = engine.registry_chain.read().await;
                assert_eq!(chain.len(), 2, "expected 2 pages after reopen");
            }

            // Check entries from both pages
            assert_eq!(
                engine.registry_get("G000").await,
                Some(PageId::from(0))
            );
            // Check last entry that fit in first page
            let last_name = format!("G{:03}", first_page_count - 1);
            assert_eq!(
                engine.registry_get(&last_name).await,
                Some(PageId::from(first_page_count as u64 - 1))
            );
            assert_eq!(
                engine.registry_get("OVERFLOW").await,
                Some(PageId::from(9999))
            );
        }
    }

    #[tokio::test]
    async fn registry_remove_from_second_page() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        // Fill first page until it's full
        fill_first_registry_page(&engine).await;

        // Add entries to second page
        engine
            .registry_insert("OVERFLOW1".into(), PageId::from(1001))
            .await
            .unwrap();
        engine
            .registry_insert("OVERFLOW2".into(), PageId::from(1002))
            .await
            .unwrap();

        // Remove entry from second page
        engine.registry_remove("OVERFLOW1").await.unwrap();

        assert_eq!(engine.registry_get("OVERFLOW1").await, None);
        assert_eq!(
            engine.registry_get("OVERFLOW2").await,
            Some(PageId::from(1002))
        );
    }

    #[tokio::test]
    async fn registry_remove_compacts_empty_page() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        // Fill first page until it's full
        fill_first_registry_page(&engine).await;

        // Add single entry to second page
        engine
            .registry_insert("ONLY_ENTRY".into(), PageId::from(9999))
            .await
            .unwrap();

        // Verify chain has 2 pages
        {
            let chain = engine.registry_chain.read().await;
            assert_eq!(chain.len(), 2);
        }

        // Remove the only entry from second page
        engine.registry_remove("ONLY_ENTRY").await.unwrap();

        // Page should be compacted (removed from chain)
        {
            let chain = engine.registry_chain.read().await;
            assert_eq!(chain.len(), 1, "empty page should be compacted");
            assert!(
                chain[0].1.next_page.is_none(),
                "first page next_page should be cleared"
            );
        }

        // Entry should be gone
        assert_eq!(engine.registry_get("ONLY_ENTRY").await, None);

        // First page entries should still work
        assert_eq!(engine.registry_get("G000").await, Some(PageId::from(0)));
    }

    #[tokio::test]
    async fn registry_stress_many_globals() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let engine = FileStorageEngine::create(
            &db_path,
            StorageConfig::default(),
            3,
            None,
        )
        .await
        .expect("create");

        // Insert 1000 globals (will need ~4 pages)
        let eng = &engine;
        stream::iter(0usize..1000)
            .for_each(|i| async move {
                let name = format!("GLOBAL_{i:04}");
                eng.registry_insert(name, PageId::from(i as u64))
                    .await
                    .unwrap();
            })
            .await;

        // Verify chain has multiple pages
        let chain_len = {
            let chain = engine.registry_chain.read().await;
            chain.len()
        };
        assert!(chain_len >= 3, "expected at least 3 pages, got {chain_len}");

        // Verify we can get all entries
        let eng = &engine;
        stream::iter(0usize..1000)
            .for_each(|j| async move {
                let name = format!("GLOBAL_{j:04}");
                let result = eng.registry_get(&name).await;
                assert_eq!(
                    result,
                    Some(PageId::from(j as u64)),
                    "failed to get {name}"
                );
            })
            .await;

        // Verify total entries count
        let entries = engine.registry_entries().await;
        assert_eq!(entries.len(), 1000);
    }

    #[tokio::test]
    async fn registry_stress_persist_and_reopen() {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("testdb");

        let num_globals = 500usize;

        // Create and populate
        {
            let engine = FileStorageEngine::create(
                &db_path,
                StorageConfig::default(),
                3,
                None,
            )
            .await
            .expect("create");

            let eng = &engine;
            stream::iter(0..num_globals)
                .for_each(|i| async move {
                    let name = format!("G_{i:04}");
                    eng.registry_insert(name, PageId::from(i as u64))
                        .await
                        .unwrap();
                })
                .await;

            engine.flush().await.expect("flush");
        }

        // Reopen and verify
        {
            let engine = FileStorageEngine::open(&db_path).await.expect("open");

            let entries = engine.registry_entries().await;
            assert_eq!(entries.len(), num_globals);

            // Spot check a few entries
            assert_eq!(
                engine.registry_get("G_0000").await,
                Some(PageId::from(0))
            );
            assert_eq!(
                engine.registry_get("G_0250").await,
                Some(PageId::from(250))
            );
            assert_eq!(
                engine.registry_get("G_0499").await,
                Some(PageId::from(499))
            );
        }
    }
}
