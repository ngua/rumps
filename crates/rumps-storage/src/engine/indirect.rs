//! Indirect page structures for bitmap page indirection.

use super::Superblock;
use crate::error::{Result, StorageError};
use crate::page::{self, PageId};

/// Indirect page holding `PageId` entries for bitmap pages (or further indirection).
///
/// Each indirect page holds `PAGE_SIZE / 8 = 512` entries (at 4KB page size).
/// Used for both single-indirect (points to bitmap pages) and double-indirect
/// (points to single-indirect pages).
///
/// # Layout
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────┐
/// │ 512 × 8-byte PageId entries (or fewer if not all used)      │
/// │ Entry value 0 = empty slot (no page)                        │
/// └─────────────────────────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone)]
pub(crate) struct IndirectPage {
    /// Page IDs stored in this indirect page.
    /// Length <= `Superblock::INDIRECT_ENTRIES_PER_PAGE`.
    pub(crate) entries: Vec<PageId>,
}

impl IndirectPage {
    /// Create a new empty indirect page.
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Create an indirect page from a list of page IDs.
    pub(crate) fn from_entries(entries: Vec<PageId>) -> Self {
        Self { entries }
    }

    /// Number of entries in this page.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the page is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Check if the page is full.
    pub(crate) fn is_full(&self) -> bool {
        self.entries.len() >= Superblock::INDIRECT_ENTRIES_PER_PAGE
    }

    /// Add an entry if not full. Returns `Err` if full.
    pub(crate) fn push(&mut self, pid: PageId) -> Result<()> {
        if self.is_full() {
            Err(StorageError::InvalidOperation(
                "indirect page is full".into(),
            ))
        } else {
            self.entries.push(pid);
            Ok(())
        }
    }

    /// Serialize to a page-sized buffer.
    pub(crate) fn serialize(&self) -> [u8; page::PAGE_SIZE] {
        let mut buf = [0u8; page::PAGE_SIZE];

        self.entries
            .iter()
            .take(Superblock::INDIRECT_ENTRIES_PER_PAGE)
            .enumerate()
            .for_each(|(i, &pid)| {
                let off = i * 8;
                buf.get_mut(off..off + 8)
                    .map(|s| s.copy_from_slice(&u64::from(pid).to_le_bytes()));
            });

        buf
    }

    /// Deserialize from a page-sized buffer.
    ///
    /// Reads entries until a zero (empty slot) is encountered.
    pub(crate) fn deserialize(buf: &[u8]) -> Result<Self> {
        if buf.len() < page::PAGE_SIZE {
            Err(StorageError::InvalidOperation(
                "indirect page buffer too small".into(),
            ))
        } else {
            let entries = (0..Superblock::INDIRECT_ENTRIES_PER_PAGE)
                .filter_map(|i| {
                    let off = i * 8;
                    buf.get(off..off + 8)
                        .and_then(|s| s.try_into().ok())
                        .map(u64::from_le_bytes)
                })
                .take_while(|&v| v != 0)
                .map(PageId::from)
                .collect();

            Ok(Self { entries })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::page;

    #[test]
    fn indirect_page_new_is_empty() {
        let page = IndirectPage::new();
        assert!(page.is_empty());
        assert!(!page.is_full());
        assert_eq!(page.len(), 0);
    }

    #[test]
    fn indirect_page_serialize_deserialize_roundtrip() {
        let mut page = IndirectPage::new();
        page.push(PageId::from_page_num(10).unwrap()).unwrap();
        page.push(PageId::from_page_num(20).unwrap()).unwrap();
        page.push(PageId::from_page_num(30).unwrap()).unwrap();

        let buf = page.serialize();
        let restored = IndirectPage::deserialize(&buf).expect("deserialize");

        assert_eq!(restored.len(), 3);
        assert_eq!(restored.entries[0].page_num(), 10);
        assert_eq!(restored.entries[1].page_num(), 20);
        assert_eq!(restored.entries[2].page_num(), 30);
    }

    #[test]
    fn indirect_page_deserialize_stops_at_zero() {
        // Create a buffer with 2 valid entries followed by zeros
        // Values are byte offsets, so page 100 = offset 100 * PAGE_SIZE
        let offset1 = 100u64 * page::PAGE_SIZE as u64;
        let offset2 = 200u64 * page::PAGE_SIZE as u64;

        let mut buf = [0u8; page::PAGE_SIZE];
        buf[0..8].copy_from_slice(&offset1.to_le_bytes());
        buf[8..16].copy_from_slice(&offset2.to_le_bytes());
        // bytes 16..24 are zeros (end of entries)

        let page = IndirectPage::deserialize(&buf).expect("deserialize");
        assert_eq!(page.len(), 2);
        assert_eq!(page.entries[0].page_num(), 100);
        assert_eq!(page.entries[1].page_num(), 200);
    }

    #[test]
    fn indirect_page_push_until_full() {
        let mut page = IndirectPage::new();

        // Fill the page
        (0..Superblock::INDIRECT_ENTRIES_PER_PAGE).for_each(|i| {
            page.push(PageId::from_page_num(i as u64 + 1).unwrap())
                .unwrap();
        });

        assert!(page.is_full());
        assert_eq!(page.len(), Superblock::INDIRECT_ENTRIES_PER_PAGE);

        // Next push should fail
        let result = page.push(PageId::from_page_num(9999).unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn indirect_page_from_entries() {
        let entries = vec![
            PageId::from_page_num(5).unwrap(),
            PageId::from_page_num(10).unwrap(),
            PageId::from_page_num(15).unwrap(),
        ];
        let page = IndirectPage::from_entries(entries);

        assert_eq!(page.len(), 3);
        assert!(!page.is_empty());
        assert!(!page.is_full());
    }
}
