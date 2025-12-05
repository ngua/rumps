//! Superblock and database metadata structures.

use std::iter;

use crate::error::{Result, StorageError};
use crate::page::{self, PageId};

/// Superblock stored in page 0, containing database metadata and bitmap page locations.
///
/// # Layout
///
/// The superblock uses a filesystem-style indirect block scheme to support
/// databases up to ~32 TiB. Direct slots hold up to 400 bitmap page IDs.
/// When more are needed, single-indirect and double-indirect pages provide
/// additional capacity.
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────┐
/// │ Offset   Size     Field                                     │
/// ├─────────────────────────────────────────────────────────────┤
/// │ 0        4        Magic ("RUMP")                            │
/// │ 4        4        Version (1)                               │
/// │ 8        8        Flags (reserved, must be 0)               │
/// │ 16       8        Total allocated page count (cached)       │
/// │ 24       8        Direct bitmap count (N ≤ 400)             │
/// │ 32       3200     Direct bitmap page IDs [PageId; 400]      │
/// │ 3232     8        Single-indirect bitmap page (0 = none)    │
/// │ 3240     8        Double-indirect bitmap page (0 = none)    │
/// │ 3248     8        Metadata page ID (0 = none)               │
/// │ 3256     8        Registry page ID (0 = none)               │
/// │ 3264     816      Reserved                                  │
/// │ 4080     8        Checksum (CRC32 of bytes 0..4080)         │
/// └─────────────────────────────────────────────────────────────┘
/// ```
///
/// # Capacity
///
/// | Level             | Bitmap Pages   | Data Size   |
/// |-------------------|----------------|-------------|
/// | 400 direct        | 400            | ~50 GiB     |
/// | 1 single-indirect | 512            | ~64 GiB     |
/// | 1 double-indirect | 262,144        | ~32 TiB     |
/// | **Total**         | ~263,000       | **~32 TiB** |
/// |-------------------|----------------|-------------|
///
#[derive(Debug, Clone)]
pub(crate) struct Superblock {
    /// Format version (currently 1).
    pub(crate) version: u32,
    /// Flags (reserved, must be 0).
    pub(crate) flags: u64,
    /// Cached count of total allocated pages.
    pub(crate) total_pages: u64,
    /// Number of direct bitmap pages in use (`<= 400`).
    pub(crate) direct_bitmap_count: u64,
    /// Page IDs of direct bitmap pages (up to 400).
    pub(crate) direct_bitmap_ids: Vec<PageId>,
    /// Single-indirect page (holds up to 512 bitmap page IDs).
    pub(crate) single_indirect: Option<PageId>,
    /// Double-indirect page (holds up to 512 indirect page IDs).
    pub(crate) double_indirect: Option<PageId>,
    /// Page ID of the metadata page (`None` = not yet allocated).
    pub(crate) metadata_root: Option<PageId>,
    /// Page ID of the global registry page (`None` = not yet allocated).
    pub(crate) registry_root: Option<PageId>,
}

impl Superblock {
    /// Magic bytes for RUMPS data files.
    const MAGIC: [u8; 4] = *b"RUMP";

    /// Current superblock version.
    pub(crate) const VERSION: u32 = 1;

    /// Maximum number of direct bitmap pages.
    pub(crate) const MAX_DIRECT_BITMAP_PAGES: usize = 400;

    /// Number of `PageId` entries per indirect page (`PAGE_SIZE / 8`).
    pub(crate) const INDIRECT_ENTRIES_PER_PAGE: usize = page::PAGE_SIZE / 8;

    // Layout offsets
    const OFF_MAGIC: usize = 0;
    const OFF_VERSION: usize = 4;
    const OFF_FLAGS: usize = 8;
    const OFF_TOTAL_PAGES: usize = 16;
    const OFF_BITMAP_COUNT: usize = 24;
    const OFF_BITMAP_IDS: usize = 32;
    const OFF_SINGLE_INDIRECT: usize = 32 + 8 * Self::MAX_DIRECT_BITMAP_PAGES; // 3232
    const OFF_DOUBLE_INDIRECT: usize = Self::OFF_SINGLE_INDIRECT + 8; // 3240
    const OFF_METADATA_ROOT: usize = Self::OFF_DOUBLE_INDIRECT + 8; // 3248
    const OFF_REGISTRY_ROOT: usize = Self::OFF_METADATA_ROOT + 8; // 3256
    const OFF_RESERVED: usize = Self::OFF_REGISTRY_ROOT + 8; // 3264
    const OFF_CHECKSUM: usize = 4080;
    const SIZE: usize = 4096;

    /// Create a new superblock with a single direct bitmap page.
    pub(crate) fn new(first_bitmap_page: PageId, total_pages: u64) -> Self {
        Self {
            version: Self::VERSION,
            flags: 0,
            total_pages,
            direct_bitmap_count: 1,
            direct_bitmap_ids: vec![first_bitmap_page],
            single_indirect: None,
            double_indirect: None,
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

        // Direct bitmap page count
        buf.get_mut(Self::OFF_BITMAP_COUNT..Self::OFF_BITMAP_IDS)
            .map(|s| {
                s.copy_from_slice(&self.direct_bitmap_count.to_le_bytes())
            });

        // Direct bitmap page IDs
        self.direct_bitmap_ids
            .iter()
            .take(Self::MAX_DIRECT_BITMAP_PAGES)
            .enumerate()
            .for_each(|(i, &pid)| {
                let off = Self::OFF_BITMAP_IDS + i * 8;
                buf.get_mut(off..off + 8)
                    .map(|s| s.copy_from_slice(&u64::from(pid).to_le_bytes()));
            });

        // Single-indirect (0 = None)
        let single_val = self.single_indirect.map_or(0u64, u64::from);
        buf.get_mut(Self::OFF_SINGLE_INDIRECT..Self::OFF_DOUBLE_INDIRECT)
            .map(|s| s.copy_from_slice(&single_val.to_le_bytes()));

        // Double-indirect (0 = None)
        let double_val = self.double_indirect.map_or(0u64, u64::from);
        buf.get_mut(Self::OFF_DOUBLE_INDIRECT..Self::OFF_METADATA_ROOT)
            .map(|s| s.copy_from_slice(&double_val.to_le_bytes()));

        // Metadata root (0 = None)
        let meta_val = self.metadata_root.map_or(0u64, u64::from);
        buf.get_mut(Self::OFF_METADATA_ROOT..Self::OFF_REGISTRY_ROOT)
            .map(|s| s.copy_from_slice(&meta_val.to_le_bytes()));

        // Registry root (0 = None)
        let reg_val = self.registry_root.map_or(0u64, u64::from);
        buf.get_mut(Self::OFF_REGISTRY_ROOT..Self::OFF_RESERVED)
            .map(|s| s.copy_from_slice(&reg_val.to_le_bytes()));

        // CRC32 checksum of bytes 0..4080
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
        let direct_bitmap_count = read_u64(Self::OFF_BITMAP_COUNT)?;

        if direct_bitmap_count > Self::MAX_DIRECT_BITMAP_PAGES as u64 {
            Err(StorageError::InvalidOperation(format!(
                "direct_bitmap_count {direct_bitmap_count} exceeds max {}",
                Self::MAX_DIRECT_BITMAP_PAGES
            )))
        } else {
            let direct_bitmap_ids = (0..direct_bitmap_count as usize)
                .map(|i| {
                    let off = Self::OFF_BITMAP_IDS + i * 8;
                    read_u64(off).map(PageId::from)
                })
                .collect::<Result<Vec<_>>>()?;

            // Read single/double indirect pointers (0 = None)
            let single_val = read_u64(Self::OFF_SINGLE_INDIRECT)?;
            let double_val = read_u64(Self::OFF_DOUBLE_INDIRECT)?;

            let single_indirect =
                (single_val != 0).then(|| PageId::from(single_val));
            let double_indirect =
                (double_val != 0).then(|| PageId::from(double_val));

            // Read metadata and registry roots (0 = None)
            let meta_val = read_u64(Self::OFF_METADATA_ROOT)?;
            let reg_val = read_u64(Self::OFF_REGISTRY_ROOT)?;

            let metadata_root = (meta_val != 0).then(|| PageId::from(meta_val));
            let registry_root = (reg_val != 0).then(|| PageId::from(reg_val));

            Ok(Self {
                version,
                flags,
                total_pages,
                direct_bitmap_count,
                direct_bitmap_ids,
                single_indirect,
                double_indirect,
                metadata_root,
                registry_root,
            })
        }
    }

    /// Add a new direct bitmap page ID.
    ///
    /// Returns `Err` if already at maximum direct capacity.
    /// For indirect pages, use [`Self::set_single_indirect`] or [`Self::set_double_indirect`].
    pub(crate) fn add_direct_bitmap_page(&mut self, pid: PageId) -> Result<()> {
        if self.direct_bitmap_ids.len() >= Self::MAX_DIRECT_BITMAP_PAGES {
            Err(StorageError::InvalidOperation(format!(
                "cannot add direct bitmap page: already at max {}",
                Self::MAX_DIRECT_BITMAP_PAGES
            )))
        } else {
            self.direct_bitmap_ids.push(pid);
            self.direct_bitmap_count += 1;
            Ok(())
        }
    }

    /// Set the single-indirect page pointer.
    pub(crate) fn set_single_indirect(&mut self, pid: PageId) {
        self.single_indirect = Some(pid);
    }

    /// Set the double-indirect page pointer.
    pub(crate) fn set_double_indirect(&mut self, pid: PageId) {
        self.double_indirect = Some(pid);
    }

    /// Returns true if we can add more bitmap pages via direct slots or indirect pages.
    pub(crate) fn can_add_bitmap_page(&self) -> bool {
        // Can always add if direct slots available
        self.direct_bitmap_ids.len() < Self::MAX_DIRECT_BITMAP_PAGES
            // Or if single-indirect not set yet (we can create it)
            || self.single_indirect.is_none()
            // Or if double-indirect not set yet (we can create it)
            || self.double_indirect.is_none()
        // Otherwise, indirect pages might have room (caller needs to check)
    }

    /// Get all reserved page numbers that this superblock tracks.
    ///
    /// This includes superblock itself (0), all direct bitmap pages,
    /// and indirect page pointers. Caller must also check indirect page
    /// contents for additional reserved pages.
    pub(crate) fn reserved_page_nums(&self) -> Vec<u64> {
        iter::once(0u64)
            .chain(self.direct_bitmap_ids.iter().map(|pid| pid.page_num()))
            .chain(self.single_indirect.iter().map(|pid| pid.page_num()))
            .chain(self.double_indirect.iter().map(|pid| pid.page_num()))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::page;

    #[test]
    fn superblock_serialize_deserialize_roundtrip() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let sb = Superblock::new(first_bm, 100);

        let buf = sb.serialize();
        let restored = Superblock::deserialize(&buf).unwrap();

        assert_eq!(restored.version, Superblock::VERSION);
        assert_eq!(restored.flags, 0);
        assert_eq!(restored.total_pages, 100);
        assert_eq!(restored.direct_bitmap_count, 1);
        assert_eq!(restored.direct_bitmap_ids.len(), 1);
        assert_eq!(restored.direct_bitmap_ids[0].page_num(), 1);
    }

    #[test]
    fn superblock_add_direct_bitmap_page() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let mut sb = Superblock::new(first_bm, 100);

        // Add more bitmap pages
        let p2 = PageId::from_page_num(50).unwrap();
        let p3 = PageId::from_page_num(100).unwrap();

        sb.add_direct_bitmap_page(p2).unwrap();
        sb.add_direct_bitmap_page(p3).unwrap();

        assert_eq!(sb.direct_bitmap_count, 3);
        assert_eq!(sb.direct_bitmap_ids.len(), 3);
        assert_eq!(sb.direct_bitmap_ids[1].page_num(), 50);
        assert_eq!(sb.direct_bitmap_ids[2].page_num(), 100);
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

    #[test]
    fn superblock_with_single_indirect() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let mut sb = Superblock::new(first_bm, 100);

        // Set single-indirect pointer
        let single_pid = PageId::from_page_num(500).unwrap();
        sb.set_single_indirect(single_pid);

        // Serialize and deserialize
        let buf = sb.serialize();
        let restored = Superblock::deserialize(&buf).unwrap();

        assert_eq!(restored.single_indirect, Some(single_pid));
        assert!(restored.double_indirect.is_none());
    }

    #[test]
    fn superblock_with_double_indirect() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let mut sb = Superblock::new(first_bm, 100);

        // Set both indirect pointers
        let single_pid = PageId::from_page_num(500).unwrap();
        let double_pid = PageId::from_page_num(600).unwrap();
        sb.set_single_indirect(single_pid);
        sb.set_double_indirect(double_pid);

        let buf = sb.serialize();
        let restored = Superblock::deserialize(&buf).unwrap();

        assert_eq!(restored.single_indirect, Some(single_pid));
        assert_eq!(restored.double_indirect, Some(double_pid));
    }

    #[test]
    fn superblock_reserved_page_nums_includes_indirect() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let mut sb = Superblock::new(first_bm, 100);

        // Add more direct bitmap pages
        sb.add_direct_bitmap_page(PageId::from_page_num(2).unwrap())
            .unwrap();

        // Set indirect pointers
        sb.set_single_indirect(PageId::from_page_num(500).unwrap());
        sb.set_double_indirect(PageId::from_page_num(600).unwrap());

        let reserved = sb.reserved_page_nums();

        // Should include: superblock (0), bitmap pages (1, 2), indirect pages (500, 600)
        assert!(reserved.contains(&0));
        assert!(reserved.contains(&1));
        assert!(reserved.contains(&2));
        assert!(reserved.contains(&500));
        assert!(reserved.contains(&600));
        assert_eq!(reserved.len(), 5);
    }

    #[test]
    fn superblock_can_add_bitmap_page_with_indirect_available() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let sb = Superblock::new(first_bm, 100);

        // Fresh superblock can add more
        assert!(sb.can_add_bitmap_page());
    }

    #[test]
    fn superblock_direct_limit_is_400() {
        assert_eq!(Superblock::MAX_DIRECT_BITMAP_PAGES, 400);
    }

    #[test]
    fn superblock_indirect_entries_is_512() {
        // PAGE_SIZE / 8 = 512 for 4KB pages
        assert_eq!(Superblock::INDIRECT_ENTRIES_PER_PAGE, page::PAGE_SIZE / 8);
    }

    #[test]
    fn superblock_add_direct_fails_at_400() {
        let first_bm = PageId::from_page_num(1).unwrap();
        let mut sb = Superblock::new(first_bm, 100);

        // Add 399 more to reach 400 total
        (2..=400).for_each(|i| {
            sb.add_direct_bitmap_page(PageId::from_page_num(i).unwrap())
                .unwrap();
        });

        assert_eq!(sb.direct_bitmap_ids.len(), 400);

        // Next should fail
        let result =
            sb.add_direct_bitmap_page(PageId::from_page_num(401).unwrap());
        assert!(result.is_err());
    }
}
