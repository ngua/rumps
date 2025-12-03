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
// ┌─────────────────────────────────────────────────────────────┐
// │ Offset   Size     Field                                     │
// ├─────────────────────────────────────────────────────────────┤
// │ 0        4        Magic ("RUMP")                            │
// │ 4        4        Version (2)                               │
// │ 8        8        Flags (reserved, must be 0)               │
// │ 16       8        Total allocated page count (cached)       │
// │ 24       8        Bitmap page count (N)                     │
// │ 32       8×500    Bitmap page IDs [PageId; 500]             │
// │ 4032     8        Metadata page ID (0 = none)               │
// │ 4040     8        Registry page ID (0 = none)               │
// │ 4048     40       Reserved (future use)                     │
// │ 4088     8        Checksum (CRC32 of bytes 0..4088)         │
// └─────────────────────────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone)]
pub(crate) struct Superblock {
    /// Format version (currently 2).
    pub(crate) version: u32,
    /// Flags (reserved, must be 0).
    pub(crate) flags: u64,
    /// Cached count of total allocated pages.
    pub(crate) total_pages: u64,
    /// Number of bitmap pages in use.
    pub(crate) bitmap_page_count: u64,
    /// Page IDs of bitmap pages (up to 500).
    pub(crate) bitmap_page_ids: Vec<PageId>,
    /// Page ID of the metadata page (`None` = not yet allocated).
    pub(crate) metadata_root: Option<PageId>,
    /// Page ID of the global registry page (`None` = not yet allocated).
    pub(crate) registry_root: Option<PageId>,
}

impl Superblock {
    /// Magic bytes for RUMPS data files.
    const MAGIC: [u8; 4] = *b"RUMP";

    /// Current superblock version.
    const VERSION: u32 = 2;

    /// Maximum number of bitmap pages.
    pub(crate) const MAX_BITMAP_PAGES: usize = 500;

    // Layout offsets
    const OFF_MAGIC: usize = 0;
    const OFF_VERSION: usize = 4;
    const OFF_FLAGS: usize = 8;
    const OFF_TOTAL_PAGES: usize = 16;
    const OFF_BITMAP_COUNT: usize = 24;
    const OFF_BITMAP_IDS: usize = 32;
    const OFF_METADATA_ROOT: usize = 32 + 8 * Self::MAX_BITMAP_PAGES; // 4032
    const OFF_REGISTRY_ROOT: usize = Self::OFF_METADATA_ROOT + 8; // 4040
    const OFF_RESERVED: usize = Self::OFF_REGISTRY_ROOT + 8; // 4048
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
            metadata_root: None,
            registry_root: None,
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

        // Metadata root (0 = None)
        let meta_val = self.metadata_root.map_or(0u64, u64::from);
        buf.get_mut(Self::OFF_METADATA_ROOT..Self::OFF_REGISTRY_ROOT)
            .map(|s| s.copy_from_slice(&meta_val.to_le_bytes()));

        // Registry root (0 = None)
        let reg_val = self.registry_root.map_or(0u64, u64::from);
        buf.get_mut(Self::OFF_REGISTRY_ROOT..Self::OFF_RESERVED)
            .map(|s| s.copy_from_slice(&reg_val.to_le_bytes()));

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

            // Read metadata and registry roots (0 = None)
            let meta_val = read_u64(Self::OFF_METADATA_ROOT)?;
            let reg_val = read_u64(Self::OFF_REGISTRY_ROOT)?;

            let metadata_root = (meta_val != 0).then(|| PageId::from(meta_val));
            let registry_root = (reg_val != 0).then(|| PageId::from(reg_val));

            Ok(Self {
                version,
                flags,
                total_pages,
                bitmap_page_count,
                bitmap_page_ids,
                metadata_root,
                registry_root,
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

/// Database metadata page stored separately from the superblock.
///
/// This page contains configuration and state that may evolve over time.
/// Storing it separately from the superblock allows the metadata format
/// to change without breaking superblock compatibility.
///
/// # Layout
///
/// ```text
// ┌─────────────────────────────────────────────────────────────┐
// │ Offset   Size     Field                                     │
// ├─────────────────────────────────────────────────────────────┤
// │ 0        4        Magic ("RMTD")                            │
// │ 4        4        Format version (1)                        │
// │ 8        8        Created timestamp (Unix seconds)          │
// │ 16       4        Page size (must match runtime)            │
// │ 20       2        B-tree min degree                         │
// │ 22       2        Reserved (alignment)                      │
// │ 24       8        Last checkpoint sequence number           │
// │ 32       4056     Reserved (future fields)                  │
// │ 4088     8        Checksum (CRC32 of bytes 0..4088)         │
// └─────────────────────────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone)]
pub(crate) struct MetadataPage {
    /// Metadata format version.
    pub(crate) version: u32,
    /// Unix timestamp (seconds) when this database was created.
    pub(crate) created_at: u64,
    /// Page size this database was created with (must match runtime).
    pub(crate) page_size: u32,
    /// B-tree minimum degree parameter.
    pub(crate) min_degree: u16,
    /// Last successful checkpoint sequence number.
    pub(crate) last_checkpoint: u64,
}

impl MetadataPage {
    const MAGIC: [u8; 4] = *b"RMTD";
    const VERSION: u32 = 1;

    const OFF_MAGIC: usize = 0;
    const OFF_VERSION: usize = 4;
    const OFF_CREATED: usize = 8;
    const OFF_PAGE_SIZE: usize = 16;
    const OFF_MIN_DEGREE: usize = 20;
    const OFF_LAST_CHECKPOINT: usize = 24;
    const OFF_CHECKSUM: usize = 4088;
    const SIZE: usize = 4096;

    /// Create a new metadata page with current timestamp.
    pub(crate) fn new(min_degree: u16) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Self {
            version: Self::VERSION,
            created_at,
            page_size: page::PAGE_SIZE as u32,
            min_degree,
            last_checkpoint: 0,
        }
    }

    /// Serialize to a page-sized buffer with checksum.
    pub(crate) fn serialize(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];

        buf.get_mut(Self::OFF_MAGIC..Self::OFF_VERSION)
            .map(|s| s.copy_from_slice(&Self::MAGIC));

        buf.get_mut(Self::OFF_VERSION..Self::OFF_CREATED)
            .map(|s| s.copy_from_slice(&self.version.to_le_bytes()));

        buf.get_mut(Self::OFF_CREATED..Self::OFF_PAGE_SIZE)
            .map(|s| s.copy_from_slice(&self.created_at.to_le_bytes()));

        buf.get_mut(Self::OFF_PAGE_SIZE..Self::OFF_MIN_DEGREE)
            .map(|s| s.copy_from_slice(&self.page_size.to_le_bytes()));

        buf.get_mut(Self::OFF_MIN_DEGREE..Self::OFF_MIN_DEGREE + 2)
            .map(|s| s.copy_from_slice(&self.min_degree.to_le_bytes()));

        buf.get_mut(Self::OFF_LAST_CHECKPOINT..Self::OFF_LAST_CHECKPOINT + 8)
            .map(|s| s.copy_from_slice(&self.last_checkpoint.to_le_bytes()));

        let crc = crc32fast::hash(buf.get(..Self::OFF_CHECKSUM).unwrap_or(&[]));
        buf.get_mut(Self::OFF_CHECKSUM..Self::OFF_CHECKSUM + 4)
            .map(|s| s.copy_from_slice(&crc.to_le_bytes()));

        buf
    }

    /// Deserialize from a page-sized buffer, validating checksum.
    pub(crate) fn deserialize(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::SIZE {
            Err(StorageError::InvalidOperation(
                "metadata page too small".into(),
            ))
        } else {
            let magic =
                buf.get(Self::OFF_MAGIC..Self::OFF_VERSION).ok_or_else(
                    || StorageError::InvalidOperation("missing magic".into()),
                )?;

            if magic != Self::MAGIC {
                Err(StorageError::InvalidOperation(format!(
                    "invalid metadata magic: expected {:?}, got {:?}",
                    Self::MAGIC,
                    magic
                )))
            } else {
                let stored_crc = buf
                    .get(Self::OFF_CHECKSUM..Self::OFF_CHECKSUM + 4)
                    .and_then(|s| s.try_into().ok())
                    .map(u32::from_le_bytes)
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(
                            "missing checksum".into(),
                        )
                    })?;

                let computed = crc32fast::hash(
                    buf.get(..Self::OFF_CHECKSUM).unwrap_or(&[]),
                );

                if stored_crc != computed {
                    Err(StorageError::InvalidOperation(format!(
                        "metadata checksum mismatch: stored {stored_crc:#x}, computed {computed:#x}"
                    )))
                } else {
                    Self::deserialize_unchecked(buf)
                }
            }
        }
    }

    /// Deserialize without checksum validation (for internal use after validation).
    fn deserialize_unchecked(buf: &[u8]) -> Result<Self> {
        let read_u16 = |off: usize| -> Result<u16> {
            buf.get(off..off + 2)
                .and_then(|s| s.try_into().ok())
                .map(u16::from_le_bytes)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(format!(
                        "failed to read u16 at {off}"
                    ))
                })
        };

        let read_u32 = |off: usize| -> Result<u32> {
            buf.get(off..off + 4)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(format!(
                        "failed to read u32 at {off}"
                    ))
                })
        };

        let read_u64 = |off: usize| -> Result<u64> {
            buf.get(off..off + 8)
                .and_then(|s| s.try_into().ok())
                .map(u64::from_le_bytes)
                .ok_or_else(|| {
                    StorageError::InvalidOperation(format!(
                        "failed to read u64 at {off}"
                    ))
                })
        };

        Ok(Self {
            version: read_u32(Self::OFF_VERSION)?,
            created_at: read_u64(Self::OFF_CREATED)?,
            page_size: read_u32(Self::OFF_PAGE_SIZE)?,
            min_degree: read_u16(Self::OFF_MIN_DEGREE)?,
            last_checkpoint: read_u64(Self::OFF_LAST_CHECKPOINT)?,
        })
    }

    /// Validate that this metadata matches runtime configuration.
    pub(crate) fn validate_runtime(&self) -> Result<()> {
        if self.page_size != page::PAGE_SIZE as u32 {
            Err(StorageError::InvalidOperation(format!(
                "page size mismatch: file has {}, runtime has {}",
                self.page_size,
                page::PAGE_SIZE
            )))
        } else {
            Ok(())
        }
    }
}

/// Global registry page mapping global names to their B-tree root pages.
///
/// This page stores the name → `PageId` mapping for all persistent globals.
/// Entries are variable-length (name length varies), packed sequentially.
/// Multiple registry pages can be chained via `next_page` for large databases.
///
/// # Layout
///
/// ```text
// ┌─────────────────────────────────────────────────────────────┐
// │ Offset   Size     Field                                     │
// ├─────────────────────────────────────────────────────────────┤
// │ 0        4        Magic ("RREG")                            │
// │ 4        4        Format version (1)                        │
// │ 8        2        Entry count in this page                  │
// │ 10       8        Next registry page ID (0 = none)          │
// │ 18       4070     Entry data (variable-length)              │
// │ 4088     8        Checksum (CRC32 of bytes 0..4088)         │
// └─────────────────────────────────────────────────────────────┘
//
// Each entry:
// │ 2        Name length (u16)                                  │
// │ N        Name bytes (UTF-8, no caret prefix)                │
// │ 8        Root PageId                                        │
/// ```
#[derive(Debug, Clone)]
pub(crate) struct GlobalRegistry {
    /// Entries in this registry page.
    pub(crate) entries: Vec<RegistryEntry>,
    /// Next registry page for overflow (if any).
    pub(crate) next_page: Option<PageId>,
}

/// A single entry in the global registry.
#[derive(Debug, Clone)]
pub(crate) struct RegistryEntry {
    /// Global name (without the `^` prefix).
    pub(crate) name: String,
    /// Root page ID of this global's B-tree.
    pub(crate) root: PageId,
}

impl GlobalRegistry {
    const MAGIC: [u8; 4] = *b"RREG";
    const VERSION: u32 = 1;

    const OFF_MAGIC: usize = 0;
    const OFF_VERSION: usize = 4;
    const OFF_COUNT: usize = 8;
    const OFF_NEXT_PAGE: usize = 10;
    const OFF_ENTRIES: usize = 18;
    const OFF_CHECKSUM: usize = 4088;
    const SIZE: usize = 4096;

    /// Maximum bytes available for entry data.
    const MAX_ENTRIES_BYTES: usize = Self::OFF_CHECKSUM - Self::OFF_ENTRIES; // 4070

    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_page: None,
        }
    }

    /// Look up a global's root page by name.
    pub(crate) fn get(&self, name: &str) -> Option<PageId> {
        self.entries.iter().find(|e| e.name == name).map(|e| e.root)
    }

    /// Insert or update a global's root page.
    ///
    /// Returns `Err` if the entry would exceed page capacity.
    pub(crate) fn insert(&mut self, name: String, root: PageId) -> Result<()> {
        // Check if exists → update
        let existing = self.entries.iter_mut().find(|e| e.name == name);

        match existing {
            Some(e) => {
                e.root = root;
                Ok(())
            }
            None => {
                // Check capacity (entry size = 2 + name.len() + 8)
                let entry_size = 2 + name.len() + 8;
                let current_size: usize =
                    self.entries.iter().map(|e| 2 + e.name.len() + 8).sum();

                if current_size + entry_size > Self::MAX_ENTRIES_BYTES {
                    Err(StorageError::InvalidOperation(
                        "registry page full, chaining not yet implemented"
                            .into(),
                    ))
                } else {
                    self.entries.push(RegistryEntry { name, root });
                    Ok(())
                }
            }
        }
    }

    /// Remove a global from the registry.
    pub(crate) fn remove(&mut self, name: &str) {
        self.entries.retain(|e| e.name != name);
    }

    /// Serialize to a page-sized buffer with checksum.
    pub(crate) fn serialize(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];

        buf.get_mut(Self::OFF_MAGIC..Self::OFF_VERSION)
            .map(|s| s.copy_from_slice(&Self::MAGIC));

        buf.get_mut(Self::OFF_VERSION..Self::OFF_COUNT)
            .map(|s| s.copy_from_slice(&Self::VERSION.to_le_bytes()));

        let count = self.entries.len() as u16;
        buf.get_mut(Self::OFF_COUNT..Self::OFF_NEXT_PAGE)
            .map(|s| s.copy_from_slice(&count.to_le_bytes()));

        let next_val = self.next_page.map_or(0u64, u64::from);
        buf.get_mut(Self::OFF_NEXT_PAGE..Self::OFF_ENTRIES)
            .map(|s| s.copy_from_slice(&next_val.to_le_bytes()));

        // Serialize entries
        let mut off = Self::OFF_ENTRIES;
        self.entries.iter().for_each(|e| {
            let name_bytes = e.name.as_bytes();
            let name_len = name_bytes.len() as u16;

            buf.get_mut(off..off + 2)
                .map(|s| s.copy_from_slice(&name_len.to_le_bytes()));
            off += 2;

            buf.get_mut(off..off + name_bytes.len())
                .map(|s| s.copy_from_slice(name_bytes));
            off += name_bytes.len();

            buf.get_mut(off..off + 8)
                .map(|s| s.copy_from_slice(&u64::from(e.root).to_le_bytes()));
            off += 8;
        });

        let crc = crc32fast::hash(buf.get(..Self::OFF_CHECKSUM).unwrap_or(&[]));
        buf.get_mut(Self::OFF_CHECKSUM..Self::OFF_CHECKSUM + 4)
            .map(|s| s.copy_from_slice(&crc.to_le_bytes()));

        buf
    }

    /// Deserialize from a page-sized buffer, validating checksum.
    pub(crate) fn deserialize(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::SIZE {
            Err(StorageError::InvalidOperation(
                "registry page too small".into(),
            ))
        } else {
            let magic =
                buf.get(Self::OFF_MAGIC..Self::OFF_VERSION).ok_or_else(
                    || StorageError::InvalidOperation("missing magic".into()),
                )?;

            if magic != Self::MAGIC {
                Err(StorageError::InvalidOperation(format!(
                    "invalid registry magic: expected {:?}, got {:?}",
                    Self::MAGIC,
                    magic
                )))
            } else {
                let stored_crc = buf
                    .get(Self::OFF_CHECKSUM..Self::OFF_CHECKSUM + 4)
                    .and_then(|s| s.try_into().ok())
                    .map(u32::from_le_bytes)
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(
                            "missing checksum".into(),
                        )
                    })?;

                let computed = crc32fast::hash(
                    buf.get(..Self::OFF_CHECKSUM).unwrap_or(&[]),
                );

                if stored_crc != computed {
                    Err(StorageError::InvalidOperation(format!(
                        "registry checksum mismatch: stored {stored_crc:#x}, computed {computed:#x}"
                    )))
                } else {
                    Self::deserialize_unchecked(buf)
                }
            }
        }
    }

    fn deserialize_unchecked(buf: &[u8]) -> Result<Self> {
        let count = buf
            .get(Self::OFF_COUNT..Self::OFF_NEXT_PAGE)
            .and_then(|s| s.try_into().ok())
            .map(u16::from_le_bytes)
            .ok_or_else(|| {
                StorageError::InvalidOperation("missing count".into())
            })?;

        let next_val = buf
            .get(Self::OFF_NEXT_PAGE..Self::OFF_ENTRIES)
            .and_then(|s| s.try_into().ok())
            .map(u64::from_le_bytes)
            .ok_or_else(|| {
                StorageError::InvalidOperation("missing next_page".into())
            })?;

        let next_page = (next_val != 0).then(|| PageId::from(next_val));

        // Parse entries
        let mut off = Self::OFF_ENTRIES;
        let entries = (0..count)
            .map(|_| {
                let name_len = buf
                    .get(off..off + 2)
                    .and_then(|s| s.try_into().ok())
                    .map(u16::from_le_bytes)
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "missing name length at {off}"
                        ))
                    })?;
                off += 2;

                let name = buf
                    .get(off..off + name_len as usize)
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .map(String::from)
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "invalid name at {off}"
                        ))
                    })?;
                off += name_len as usize;

                let root_val = buf
                    .get(off..off + 8)
                    .and_then(|s| s.try_into().ok())
                    .map(u64::from_le_bytes)
                    .ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "missing root at {off}"
                        ))
                    })?;
                off += 8;

                Ok(RegistryEntry {
                    name,
                    root: PageId::from(root_val),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self { entries, next_page })
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

            // Parse superblock and load bitmap pages
            // (Superblock::deserialize validates magic and checksum)
            let (page_alloc, superblock) =
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
                metadata: RwLock::new(metadata),
                registry: RwLock::new(registry),
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
        assert_eq!(sb.version, 2);
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
            assert_eq!(sb.version, 2);
            assert_eq!(sb.bitmap_page_count, 1);
        }

        // Reopen
        {
            let engine =
                FileStorageEngine::open(&db_path, StorageConfig::default())
                    .await
                    .expect("open should succeed");

            let sb = engine.superblock.read().await;
            assert_eq!(sb.version, 2);
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

        // Pages 0-3 should be reserved (superblock, bitmap, metadata, registry)
        assert!(engine.page_alloc.is_reserved(0).await);
        assert!(engine.page_alloc.is_reserved(1).await);
        assert!(engine.page_alloc.is_reserved(2).await);
        assert!(engine.page_alloc.is_reserved(3).await);

        // First allocation should be page 4
        let node_id = engine.allocate().await.unwrap();
        assert_eq!(u64::from(node_id), 4);
    }

    // ---- MetadataPage tests ----

    #[test]
    fn metadata_page_serialize_deserialize_roundtrip() {
        let meta = MetadataPage::new(5);
        let buf = meta.serialize();
        let restored = MetadataPage::deserialize(&buf).expect("deserialize");

        assert_eq!(restored.version, 1);
        assert_eq!(restored.min_degree, 5);
        assert_eq!(restored.page_size, page::PAGE_SIZE as u32);
        assert!(restored.created_at > 0);
        assert_eq!(restored.last_checkpoint, 0);
    }

    #[test]
    fn metadata_page_invalid_magic_fails() {
        let mut buf = [0u8; 4096];
        buf[0..4].copy_from_slice(b"XXXX");

        let result = MetadataPage::deserialize(&buf);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid metadata magic"));
    }

    #[test]
    fn metadata_page_checksum_mismatch_fails() {
        let meta = MetadataPage::new(3);
        let mut buf = meta.serialize();
        // Corrupt a byte
        buf[10] ^= 0xFF;

        let result = MetadataPage::deserialize(&buf);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
    }

    #[test]
    fn metadata_page_validate_runtime_correct() {
        let meta = MetadataPage::new(3);
        assert!(meta.validate_runtime().is_ok());
    }

    #[test]
    fn metadata_page_validate_runtime_wrong_page_size() {
        let mut meta = MetadataPage::new(3);
        meta.page_size = 8192; // Different from runtime PAGE_SIZE

        let result = meta.validate_runtime();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("page size mismatch"));
    }

    // ---- GlobalRegistry tests ----

    #[test]
    fn registry_serialize_deserialize_empty() {
        let reg = GlobalRegistry::new();
        let buf = reg.serialize();
        let restored = GlobalRegistry::deserialize(&buf).expect("deserialize");

        assert!(restored.entries.is_empty());
        assert!(restored.next_page.is_none());
    }

    #[test]
    fn registry_serialize_deserialize_with_entries() {
        let mut reg = GlobalRegistry::new();
        reg.insert("PATIENT".into(), PageId::from(100)).unwrap();
        reg.insert("ORDER".into(), PageId::from(200)).unwrap();
        reg.insert("USER".into(), PageId::from(300)).unwrap();

        let buf = reg.serialize();
        let restored = GlobalRegistry::deserialize(&buf).expect("deserialize");

        assert_eq!(restored.entries.len(), 3);
        assert_eq!(restored.get("PATIENT"), Some(PageId::from(100)));
        assert_eq!(restored.get("ORDER"), Some(PageId::from(200)));
        assert_eq!(restored.get("USER"), Some(PageId::from(300)));
        assert!(restored.next_page.is_none());
    }

    #[test]
    fn registry_insert_update_existing() {
        let mut reg = GlobalRegistry::new();
        reg.insert("PATIENT".into(), PageId::from(100)).unwrap();
        reg.insert("PATIENT".into(), PageId::from(999)).unwrap();

        assert_eq!(reg.entries.len(), 1);
        assert_eq!(reg.get("PATIENT"), Some(PageId::from(999)));
    }

    #[test]
    fn registry_remove() {
        let mut reg = GlobalRegistry::new();
        reg.insert("A".into(), PageId::from(1)).unwrap();
        reg.insert("B".into(), PageId::from(2)).unwrap();
        reg.remove("A");

        assert_eq!(reg.entries.len(), 1);
        assert!(reg.get("A").is_none());
        assert_eq!(reg.get("B"), Some(PageId::from(2)));
    }

    #[test]
    fn registry_invalid_magic_fails() {
        let mut buf = [0u8; 4096];
        buf[0..4].copy_from_slice(b"XXXX");

        let result = GlobalRegistry::deserialize(&buf);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid registry magic"));
    }

    #[test]
    fn registry_checksum_mismatch_fails() {
        let reg = GlobalRegistry::new();
        let mut buf = reg.serialize();
        buf[10] ^= 0xFF;

        let result = GlobalRegistry::deserialize(&buf);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
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
}
