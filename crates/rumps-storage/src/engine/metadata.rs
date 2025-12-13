//! Database metadata page stored separately from the superblock.

use std::time::Duration;

use crate::engine::config::StorageConfig;
use crate::error::{Result, StorageError};
use crate::page;
use crate::wal::{SyncMode, WalWriterConfig};

/// Database metadata page stored separately from the superblock.
///
/// This page contains configuration and state that may evolve over time.
/// Storing it separately from the superblock allows the metadata format
/// to change without breaking superblock compatibility.
///
/// # Layout
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────┐
/// │ Offset   Size     Field                                     │
/// ├─────────────────────────────────────────────────────────────┤
/// │ 0        4        Magic ("RMTD")                            │
/// │ 4        4        Format version (1)                        │
/// │ 8        8        Created timestamp (Unix seconds)          │
/// │ 16       4        Page size (must match runtime)            │
/// │ 20       2        B-tree min degree                         │
/// │ 22       2        Reserved (alignment)                      │
/// │ 24       8        Last checkpoint sequence number           │
/// │ 32       8        Page cache size (pages)                   │
/// │ 40       8        Max pages (`0` = unlimited)               │
/// │ 48       8        Max memory bytes (`0` = unlimited)        │
/// │ 56       1        Sync mode (see below for mapping)         │
/// │ 57       7        Reserved (alignment)                      │
/// │ 64       8        Sync interval ms (for Periodic mode)      │
/// │ 72       8        WAL max file size                         │
/// │ 80       4000     Reserved (future fields)                  │
/// │ 4088     8        Checksum (CRC32 of bytes `0..4088`)       │
/// └─────────────────────────────────────────────────────────────┘
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
    /// Page cache size in pages.
    pub(crate) cache_size: u64,
    /// Max pages (`0` = unlimited).
    pub(crate) max_pages: u64,
    /// Max memory bytes for B-tree (`0` = unlimited).
    pub(crate) max_memory_bytes: u64,
    /// Sync mode: `0`=Immediate, `1`=OnCommit, `2`=Periodic.
    pub(crate) sync_mode: u8,
    /// Sync interval in milliseconds (only for `Periodic` mode).
    pub(crate) sync_interval_ms: u64,
    /// WAL max file size before rotation.
    pub(crate) wal_max_file_size: u64,
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
    const OFF_CACHE_SIZE: usize = 32;
    const OFF_MAX_PAGES: usize = 40;
    const OFF_MAX_MEMORY_BYTES: usize = 48;
    const OFF_SYNC_MODE: usize = 56;
    const OFF_SYNC_INTERVAL_MS: usize = 64;
    const OFF_WAL_MAX_FILE_SIZE: usize = 72;
    const OFF_CHECKSUM: usize = 4088;
    const SIZE: usize = 4096;

    /// Create a new metadata page with current timestamp and default config.
    pub(crate) fn new(min_degree: u16) -> Self {
        let cfg = StorageConfig::default();
        Self::from_config(min_degree, &cfg, None)
    }

    /// Create a metadata page from full configuration.
    pub(crate) fn from_config(
        min_degree: u16,
        cfg: &StorageConfig,
        max_memory_bytes: Option<usize>,
    ) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let (sync_mode, sync_interval_ms) = cfg.wal_config.sync_mode.to_raw();

        Self {
            version: Self::VERSION,
            created_at,
            page_size: page::PAGE_SIZE as u32,
            min_degree,
            last_checkpoint: 0,
            cache_size: cfg.cache_size as u64,
            max_pages: cfg.max_pages.unwrap_or(0),
            max_memory_bytes: max_memory_bytes.map_or(0, |b| b as u64),
            sync_mode,
            sync_interval_ms,
            wal_max_file_size: cfg.wal_config.max_file_size,
        }
    }

    /// Convert stored config back to a [`StorageConfig`].
    pub(crate) fn to_storage_config(&self) -> StorageConfig {
        let sync_mode = match self.sync_mode {
            0 => SyncMode::Immediate,
            2 => {
                SyncMode::Periodic(Duration::from_millis(self.sync_interval_ms))
            }
            3 => SyncMode::Relaxed,
            _ => SyncMode::OnCommit,
        };
        StorageConfig {
            cache_size: self.cache_size as usize,
            max_pages: (self.max_pages != 0).then_some(self.max_pages),
            wal_config: WalWriterConfig {
                sync_mode,
                max_file_size: self.wal_max_file_size,
            },
        }
    }

    /// Returns `max_memory_bytes` as `Option` (`0` means unlimited/`None`).
    pub(crate) fn max_memory_bytes_opt(&self) -> Option<usize> {
        (self.max_memory_bytes != 0).then_some(self.max_memory_bytes as usize)
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

        buf.get_mut(Self::OFF_CACHE_SIZE..Self::OFF_CACHE_SIZE + 8)
            .map(|s| s.copy_from_slice(&self.cache_size.to_le_bytes()));

        buf.get_mut(Self::OFF_MAX_PAGES..Self::OFF_MAX_PAGES + 8)
            .map(|s| s.copy_from_slice(&self.max_pages.to_le_bytes()));

        buf.get_mut(Self::OFF_MAX_MEMORY_BYTES..Self::OFF_MAX_MEMORY_BYTES + 8)
            .map(|s| s.copy_from_slice(&self.max_memory_bytes.to_le_bytes()));

        buf.get_mut(Self::OFF_SYNC_MODE..Self::OFF_SYNC_MODE + 1)
            .map(|s| s.copy_from_slice(&[self.sync_mode]));

        buf.get_mut(Self::OFF_SYNC_INTERVAL_MS..Self::OFF_SYNC_INTERVAL_MS + 8)
            .map(|s| s.copy_from_slice(&self.sync_interval_ms.to_le_bytes()));

        buf.get_mut(
            Self::OFF_WAL_MAX_FILE_SIZE..Self::OFF_WAL_MAX_FILE_SIZE + 8,
        )
        .map(|s| s.copy_from_slice(&self.wal_max_file_size.to_le_bytes()));

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
        let read_u8 = |off: usize| -> Result<u8> {
            buf.get(off).copied().ok_or_else(|| {
                StorageError::InvalidOperation(format!(
                    "failed to read u8 at {off}"
                ))
            })
        };

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
            cache_size: read_u64(Self::OFF_CACHE_SIZE)?,
            max_pages: read_u64(Self::OFF_MAX_PAGES)?,
            max_memory_bytes: read_u64(Self::OFF_MAX_MEMORY_BYTES)?,
            sync_mode: read_u8(Self::OFF_SYNC_MODE)?,
            sync_interval_ms: read_u64(Self::OFF_SYNC_INTERVAL_MS)?,
            wal_max_file_size: read_u64(Self::OFF_WAL_MAX_FILE_SIZE)?,
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
        // Check default config values
        assert_eq!(restored.cache_size, 1024); // Default
        assert_eq!(restored.max_pages, 0); // 0 = unlimited
        assert_eq!(restored.max_memory_bytes, 0);
        assert_eq!(restored.sync_mode, 1); // OnCommit
        assert_eq!(restored.sync_interval_ms, 0);
        assert_eq!(restored.wal_max_file_size, 64 * 1024 * 1024);
    }

    #[test]
    fn metadata_page_from_config_roundtrip() {
        let cfg = StorageConfig {
            cache_size: 2048,
            max_pages: Some(10000),
            wal_config: WalWriterConfig {
                sync_mode: SyncMode::Periodic(Duration::from_millis(500)),
                max_file_size: 128 * 1024 * 1024,
            },
        };
        let meta = MetadataPage::from_config(7, &cfg, Some(1024 * 1024));
        let buf = meta.serialize();
        let restored = MetadataPage::deserialize(&buf).expect("deserialize");

        assert_eq!(restored.min_degree, 7);
        assert_eq!(restored.cache_size, 2048);
        assert_eq!(restored.max_pages, 10000);
        assert_eq!(restored.max_memory_bytes, 1024 * 1024);
        assert_eq!(restored.sync_mode, 2); // Periodic
        assert_eq!(restored.sync_interval_ms, 500);
        assert_eq!(restored.wal_max_file_size, 128 * 1024 * 1024);
    }

    #[test]
    fn metadata_page_to_storage_config() {
        let cfg = StorageConfig {
            cache_size: 4096,
            max_pages: Some(50000),
            wal_config: WalWriterConfig {
                sync_mode: SyncMode::Immediate,
                max_file_size: 32 * 1024 * 1024,
            },
        };
        let meta = MetadataPage::from_config(5, &cfg, None);
        let restored_cfg = meta.to_storage_config();

        assert_eq!(restored_cfg.cache_size, 4096);
        assert_eq!(restored_cfg.max_pages, Some(50000));
        assert_eq!(restored_cfg.wal_config.sync_mode, SyncMode::Immediate);
        assert_eq!(restored_cfg.wal_config.max_file_size, 32 * 1024 * 1024);
    }

    #[test]
    fn metadata_page_max_memory_bytes_opt() {
        let meta = MetadataPage::new(3);
        assert_eq!(meta.max_memory_bytes_opt(), None);

        let cfg = StorageConfig::default();
        let meta = MetadataPage::from_config(3, &cfg, Some(999));
        assert_eq!(meta.max_memory_bytes_opt(), Some(999));
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
}
