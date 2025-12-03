//! File-based storage engine with WAL and page cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::TryStreamExt;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::RwLock;

use super::{
    AsyncStorageEngine, GlobalRegistry, IndirectPage, MetadataPage,
    StorageConfig, StorageMetadata, Superblock,
};
use crate::error::{Result, StorageError};
use crate::node::{Node, NodeId};
use crate::page::{self, PageAllocator, PageCache, PageId};
use crate::wal::{WalReader, WalWriter};

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

    /// Single-indirect page contents (bitmap page IDs).
    /// `None` if no single-indirect page is allocated.
    single_indirect: RwLock<Option<IndirectPage>>,

    /// Double-indirect page contents (indirect page IDs + their contents).
    /// Outer Option is None if no double-indirect page is allocated.
    /// Inner Vec contains the indirect pages pointed to by double-indirect.
    double_indirect: RwLock<Option<(IndirectPage, Vec<IndirectPage>)>>,

    /// Database metadata (page size, creation time, etc.).
    metadata: RwLock<MetadataPage>,

    /// Global name → root page registry.
    registry: RwLock<GlobalRegistry>,

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

            // Parse superblock and load bitmap pages (including indirect)
            // (Superblock::deserialize validates magic and checksum)
            let (page_alloc, superblock, indirect) =
                Self::load_superblock(&mut file, &hdr, &cfg, &data_path)
                    .await?;

            // Load metadata page
            let metadata = match superblock.metadata_root {
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
                    // Legacy DB without metadata page - create default
                    MetadataPage::new(3)
                }
            };

            // Load registry page
            let registry = match superblock.registry_root {
                Some(pid) => {
                    let buf = Self::read_page_at(
                        &mut file,
                        pid.byte_offset(),
                        &data_path,
                    )
                    .await?;
                    GlobalRegistry::deserialize(&buf)?
                }
                None => {
                    // Legacy DB without registry page - create empty
                    GlobalRegistry::new()
                }
            };

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
                single_indirect: RwLock::new(indirect.single),
                double_indirect: RwLock::new(indirect.double),
                metadata: RwLock::new(metadata),
                registry: RwLock::new(registry),
                cfg,
                data_dir: dir.to_path_buf(),
            })
        }
    }

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

            // Create metadata page (page 2)
            let metadata = MetadataPage::new(3); // default min_degree = 3
            let meta_data = metadata.serialize();

            // Create registry page (page 3)
            let registry = GlobalRegistry::new();
            let reg_data = registry.serialize();

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

            Ok(Self {
                data_file: Arc::new(RwLock::new(file)),
                wal: Arc::new(wal),
                cache: Arc::new(cache),
                page_alloc: Arc::new(page_alloc),
                superblock: RwLock::new(superblock),
                single_indirect: RwLock::new(None),
                double_indirect: RwLock::new(None),
                metadata: RwLock::new(metadata),
                registry: RwLock::new(registry),
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tempfile::TempDir;

    use super::*;

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
            let engine =
                FileStorageEngine::create(&db_path, StorageConfig::default())
                    .await
                    .expect("create should succeed");

            let sb = engine.superblock.read().await;
            assert_eq!(sb.version, Superblock::VERSION);
            assert_eq!(sb.direct_bitmap_count, 1);
        }

        // Reopen
        {
            let engine =
                FileStorageEngine::open(&db_path, StorageConfig::default())
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

        let engine =
            FileStorageEngine::create(&db_path, StorageConfig::default())
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
            let engine =
                FileStorageEngine::create(&db_path, StorageConfig::default())
                    .await
                    .expect("create");

            // Verify metadata was created
            let meta = engine.metadata.read().await;
            assert_eq!(meta.version, 1);
            assert_eq!(meta.min_degree, 3);
            assert!(meta.created_at > 0);
        }

        // Reopen and verify
        let engine =
            FileStorageEngine::open(&db_path, StorageConfig::default())
                .await
                .expect("open");

        let meta = engine.metadata.read().await;
        assert_eq!(meta.version, 1);
        assert_eq!(meta.min_degree, 3);

        let reg = engine.registry.read().await;
        assert!(reg.entries.is_empty());
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
        let engine =
            FileStorageEngine::create(&db_path, StorageConfig::default())
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
        let engine =
            FileStorageEngine::open(&db_path, StorageConfig::default())
                .await
                .expect("reopen");

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
}
