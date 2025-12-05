//! Page identifier type.

use std::ops::Deref;

use serde::{Deserialize, Serialize};

use crate::error::{Result, StorageError};

/// Identifier for a page in the data file.
///
/// `PageId` represents the byte offset into the data file where a page begins.
/// Each page is `PAGE_SIZE` bytes. Page 0 is reserved for metadata/header.
///
/// # Relationship to `NodeId`
///
/// - `PageId` is specific to disk storage (byte offset in data file)
/// - `NodeId` is a logical identifier used by the B-tree
/// - For persistent globals: `NodeId` maps to `PageId`
/// - For locals: `NodeId` maps to in-memory index
#[repr(transparent)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize
)]
pub(crate) struct PageId(u64);

impl PageId {
    /// Reserved page ID for the file header/metadata.
    pub(crate) const HEADER: Self = Self(0);

    /// Create a `PageId` from a page number (not byte offset).
    ///
    /// Page `n` starts at byte offset `n * PAGE_SIZE`.
    /// Returns an error if `n * PAGE_SIZE` would overflow.
    pub(crate) fn from_page_num(n: u64) -> Result<Self> {
        n.checked_mul(super::PAGE_SIZE as u64)
            .map(Self)
            .ok_or_else(|| {
                StorageError::InvalidOperation(format!(
                    "page number {n} overflows byte offset calculation"
                ))
            })
    }

    /// Get the page number (0-indexed).
    pub(crate) fn page_num(self) -> u64 {
        self.0 / super::PAGE_SIZE as u64
    }

    /// Get the byte offset in the data file.
    pub(crate) fn offset(self) -> u64 {
        self.0
    }

    /// Check if this is the header page.
    pub(crate) fn is_header(self) -> bool {
        self.0 == 0
    }

    /// Get the byte offset for I/O operations.
    pub(crate) fn byte_offset(self) -> u64 {
        self.0
    }
}

impl From<crate::node::NodeId> for PageId {
    fn from(id: crate::node::NodeId) -> Self {
        Self(u64::from(id) * super::PAGE_SIZE as u64)
    }
}

impl From<PageId> for crate::node::NodeId {
    fn from(id: PageId) -> Self {
        Self::from(id.page_num())
    }
}

impl Deref for PageId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<u64> for PageId {
    /// Create a `PageId` from a byte offset.
    fn from(offset: u64) -> Self {
        Self(offset)
    }
}

impl From<PageId> for u64 {
    fn from(id: PageId) -> Self {
        id.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::page::PAGE_SIZE;

    #[test]
    fn page_id_from_page_num() {
        let id = PageId::from_page_num(0).unwrap();
        assert_eq!(id.offset(), 0);
        assert_eq!(id.page_num(), 0);

        let id = PageId::from_page_num(1).unwrap();
        assert_eq!(id.offset(), PAGE_SIZE as u64);
        assert_eq!(id.page_num(), 1);

        let id = PageId::from_page_num(10).unwrap();
        assert_eq!(id.offset(), 10 * PAGE_SIZE as u64);
        assert_eq!(id.page_num(), 10);
    }

    #[test]
    fn page_id_header() {
        assert!(PageId::HEADER.is_header());
        assert_eq!(PageId::HEADER.offset(), 0);
        assert!(!PageId::from_page_num(1).unwrap().is_header());
    }

    #[test]
    fn page_id_conversions() {
        let offset = 8192u64;
        let id: PageId = offset.into();
        assert_eq!(u64::from(id), offset);
    }

    #[test]
    fn page_size_is_power_of_two() {
        assert!(PAGE_SIZE.is_power_of_two());
    }

    #[test]
    fn page_size_reasonable() {
        assert!(PAGE_SIZE >= 512, "page size too small");
        assert!(PAGE_SIZE <= 65536, "page size too large");
    }

    #[test]
    fn page_id_from_page_num_overflow() {
        // Page number that would overflow when multiplied by PAGE_SIZE
        let huge = u64::MAX / PAGE_SIZE as u64 + 1;
        let err = PageId::from_page_num(huge).unwrap_err();
        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }
}
