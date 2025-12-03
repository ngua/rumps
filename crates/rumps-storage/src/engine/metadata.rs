//! Database metadata page stored separately from the superblock.

use crate::error::{Result, StorageError};
use crate::page;

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
