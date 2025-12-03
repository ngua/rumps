//! Global registry page mapping global names to their B-tree root pages.

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
}
