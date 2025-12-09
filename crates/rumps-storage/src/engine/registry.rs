//! Global registry page mapping global names to their B-tree root pages.

use std::str;

use crate::error::{Result, StorageError};
use crate::page::PageId;

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

    /// Calculate the serialized size of an entry with the given name.
    ///
    /// Entry format: 2 bytes (name length) + N bytes (name) + 8 bytes (PageId).
    pub(crate) const fn entry_size(name_len: usize) -> usize {
        2 + name_len + 8
    }

    /// Total bytes currently used by entries in this page.
    pub(crate) fn used_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|e| Self::entry_size(e.name.len()))
            .sum()
    }

    /// Check if this page has room for a new entry with the given name.
    ///
    /// Returns `true` if the entry would fit, `false` if this page is full.
    pub(crate) fn can_insert(&self, name: &str) -> bool {
        // Check if entry already exists (update doesn't need extra space)
        self.entries.iter().any(|e| e.name == name)
            || self.used_bytes() + Self::entry_size(name.len())
                <= Self::MAX_ENTRIES_BYTES
    }

    /// Check if this page is empty (has no entries).
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entries in this page.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Look up a global's root page by name.
    pub(crate) fn get(&self, name: &str) -> Option<PageId> {
        self.entries.iter().find(|e| e.name == name).map(|e| e.root)
    }

    /// Insert or update a global's root page in this page only.
    ///
    /// Returns `Err(StorageError::RegistryPageFull)` if the entry would exceed
    /// page capacity. The caller should handle chaining in that case.
    pub(crate) fn insert(&mut self, name: String, root: PageId) -> Result<()> {
        // Check if exists → update
        let existing = self.entries.iter_mut().find(|e| e.name == name);

        match existing {
            Some(e) => {
                e.root = root;
                Ok(())
            }
            None => {
                if self.can_insert(&name) {
                    self.entries.push(RegistryEntry { name, root });
                    Ok(())
                } else {
                    Err(StorageError::RegistryPageFull)
                }
            }
        }
    }

    /// Insert unconditionally (for internal use when we know there's space).
    /// Caller must ensure `can_insert(name)` is `true`.
    pub(crate) fn insert_unchecked(&mut self, name: String, root: PageId) {
        self.entries.push(RegistryEntry { name, root });
    }

    /// Remove a global from the registry.
    pub(crate) fn remove(&mut self, name: &str) {
        self.entries.retain(|e| e.name != name);
    }

    /// Iterate over all entries in this single registry page.
    ///
    /// For iterating across a chain of registry pages, use
    /// `FileStorageEngine::registry_entries()` instead.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, PageId)> {
        self.entries.iter().map(|e| (e.name.as_str(), e.root))
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
                    .and_then(|s| str::from_utf8(s).ok())
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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

    // ========== Tests for chaining helper methods ==========

    #[test]
    fn entry_size_calculation() {
        // Entry: 2 bytes (len) + name + 8 bytes (PageId)
        assert_eq!(GlobalRegistry::entry_size(0), 10);
        assert_eq!(GlobalRegistry::entry_size(7), 17); // "PATIENT" = 7 chars
        assert_eq!(GlobalRegistry::entry_size(100), 110);
    }

    #[test]
    fn used_bytes_empty() {
        let reg = GlobalRegistry::new();
        assert_eq!(reg.used_bytes(), 0);
    }

    #[test]
    fn used_bytes_with_entries() {
        let mut reg = GlobalRegistry::new();
        reg.insert("ABC".into(), PageId::from(1)).unwrap(); // 2 + 3 + 8 = 13
        reg.insert("DEFGH".into(), PageId::from(2)).unwrap(); // 2 + 5 + 8 = 15

        assert_eq!(reg.used_bytes(), 28);
    }

    #[test]
    fn can_insert_empty() {
        let reg = GlobalRegistry::new();
        assert!(reg.can_insert("PATIENT"));
        assert!(reg.can_insert("A")); // 2 + 1 + 8 = 11 bytes, fits easily
    }

    #[test]
    fn can_insert_existing_always_true() {
        let mut reg = GlobalRegistry::new();
        reg.insert("PATIENT".into(), PageId::from(100)).unwrap();

        // Existing entry update doesn't need extra space
        assert!(reg.can_insert("PATIENT"));
    }

    #[test]
    fn can_insert_full_page() {
        let mut reg = GlobalRegistry::new();

        // Fill the page with maximum entries
        // MAX_ENTRIES_BYTES = 4070
        // Each entry with "G_NNN" name (5 chars) = 2 + 5 + 8 = 15 bytes
        // 4070 / 15 ≈ 271 entries
        let mut i = 0u64;
        while reg.can_insert(&format!("G_{i:03}")) {
            reg.insert_unchecked(format!("G_{i:03}"), PageId::from(i));
            i += 1;
        }

        // Page should be full now
        assert!(!reg.can_insert(&format!("G_{i:03}")));
        // But existing entry should still work
        assert!(reg.can_insert("G_000"));

        // Verify we got ~270 entries
        assert!(i >= 250, "expected at least 250 entries, got {i}");
    }

    #[test]
    fn insert_returns_page_full_error() {
        let mut reg = GlobalRegistry::new();

        // Fill the page
        let mut i = 0u64;
        while reg.can_insert(&format!("G_{i:03}")) {
            reg.insert_unchecked(format!("G_{i:03}"), PageId::from(i));
            i += 1;
        }

        // Next insert should fail
        let result = reg.insert("NEW_GLOBAL".into(), PageId::from(999));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::StorageError::RegistryPageFull
        ));
    }

    #[test]
    fn is_empty_and_len() {
        let mut reg = GlobalRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);

        reg.insert("A".into(), PageId::from(1)).unwrap();
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 1);

        reg.insert("B".into(), PageId::from(2)).unwrap();
        assert_eq!(reg.len(), 2);

        reg.remove("A");
        assert_eq!(reg.len(), 1);

        reg.remove("B");
        assert!(reg.is_empty());
    }

    #[test]
    fn iter_entries() {
        let mut reg = GlobalRegistry::new();
        reg.insert("PATIENT".into(), PageId::from(100)).unwrap();
        reg.insert("ORDER".into(), PageId::from(200)).unwrap();

        let entries: Vec<_> = reg.iter().collect();
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&("PATIENT", PageId::from(100))));
        assert!(entries.contains(&("ORDER", PageId::from(200))));
    }

    #[test]
    fn next_page_serialization() {
        let mut reg = GlobalRegistry::new();
        reg.insert("TEST".into(), PageId::from(50)).unwrap();
        reg.next_page = Some(PageId::from(999));

        let buf = reg.serialize();
        let restored = GlobalRegistry::deserialize(&buf).unwrap();

        assert_eq!(restored.next_page, Some(PageId::from(999)));
        assert_eq!(restored.get("TEST"), Some(PageId::from(50)));
    }
}
