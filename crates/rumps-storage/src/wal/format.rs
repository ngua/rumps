//! WAL file format definitions.
//!
//! # File Structure
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ File Header (16 bytes)                                      │
//! │   magic: [u8; 4]     = b"RWAL"                              │
//! │   version: u16       = 1                                    │
//! │   flags: u16         = 0 (reserved)                         │
//! │   first_seq: u64     = sequence number of first record      │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Record 0                                                    │
//! │   header: RecordHeader (20 bytes)                           │
//! │   payload: [u8; header.len]                                 │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Record 1                                                    │
//! │   header: RecordHeader (20 bytes)                           │
//! │   payload: [u8; header.len]                                 │
//! ├─────────────────────────────────────────────────────────────┤
//! │ ...                                                         │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Record Header
//!
//! Each record is prefixed with a fixed-size header:
//!
//! ```text
//! ┌────────────────────────────────────────┐
//! │ RecordHeader (20 bytes)                │
//! │   checksum: u32   - CRC32 of payload   │
//! │   len: u32        - payload length     │
//! │   seq: u64        - sequence number    │
//! │   flags: u32      - reserved           │
//! └────────────────────────────────────────┘
//! ```
//!
//! The payload is the bincode-serialized `WalRecord`.

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

use super::WalSequence;
use crate::error::{Result, StorageError};

/// WAL file header, written once at the start of each WAL file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileHeader {
    /// Reserved flags.
    pub(crate) flags: u16,
    /// Sequence number of the first record in this file.
    pub(crate) first_seq: WalSequence,
}

impl FileHeader {
    /// Magic bytes identifying a RUMPS WAL file.
    pub(crate) const MAGIC: [u8; 4] = *b"RWAL";

    /// Current WAL format version.
    pub(crate) const VERSION: u16 = 1;

    /// Size of the file header in bytes.
    pub(crate) const SIZE: usize = 16;
}

impl FileHeader {
    /// Create a new file header with the given first sequence number.
    pub(crate) fn new(first_seq: WalSequence) -> Self {
        Self {
            flags: 0,
            first_seq,
        }
    }

    /// Read a file header from a WAL file path.
    pub(crate) async fn read(path: &std::path::Path) -> Result<Self> {
        let mut file = File::open(path).await?;
        let mut buf = [0u8; Self::SIZE];

        AsyncReadExt::read_exact(&mut file, &mut buf)
            .await
            .map_err(|_| StorageError::WalCorruption {
                seq: 0,
                reason: format!("truncated header in {}", path.display()),
            })?;

        Self::from_bytes(&buf)
    }

    /// Serialize the header to bytes.
    pub(crate) fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&Self::MAGIC);
        buf[4..6].copy_from_slice(&Self::VERSION.to_le_bytes());
        buf[6..8].copy_from_slice(&self.flags.to_le_bytes());
        buf[8..16].copy_from_slice(&(*self.first_seq).to_le_bytes());
        buf
    }

    /// Deserialize from bytes.
    ///
    /// # Errors
    ///
    /// Returns `WalInvalidMagic` or `WalUnsupportedVersion` on mismatch.
    pub(crate) fn from_bytes(buf: &[u8; Self::SIZE]) -> Result<Self> {
        // SAFETY: slice lengths match array sizes exactly
        #[allow(clippy::unwrap_used)]
        let magic: [u8; 4] = buf[0..4].try_into().unwrap();
        (magic == Self::MAGIC)
            .then_some(())
            .ok_or(StorageError::WalInvalidMagic)?;

        // SAFETY: slice lengths match array sizes exactly
        #[allow(clippy::unwrap_used)]
        let version = u16::from_le_bytes(buf[4..6].try_into().unwrap());
        (version == Self::VERSION)
            .then_some(())
            .ok_or(StorageError::WalUnsupportedVersion(version))?;

        // SAFETY: slice lengths match array sizes exactly
        #[allow(clippy::unwrap_used)]
        let flags = u16::from_le_bytes(buf[6..8].try_into().unwrap());
        #[allow(clippy::unwrap_used)]
        let first_seq = WalSequence::from(u64::from_le_bytes(
            buf[8..16].try_into().unwrap(),
        ));

        Ok(Self { flags, first_seq })
    }
}

/// Header preceding each WAL record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordHeader {
    /// CRC32 checksum of the payload bytes.
    pub(crate) checksum: u32,
    /// Length of the payload in bytes.
    pub(crate) len: u32,
    /// Monotonically increasing sequence number.
    pub(crate) seq: WalSequence,
    /// Reserved flags (currently unused).
    pub(crate) flags: u32,
}

impl RecordHeader {
    /// Size of a record header in bytes.
    pub(crate) const SIZE: usize = 20;

    /// Create a new record header for the given payload.
    pub(crate) fn new(seq: WalSequence, payload: &[u8]) -> Self {
        Self {
            checksum: crc32fast::hash(payload),
            len: payload.len() as u32,
            seq,
            flags: 0,
        }
    }

    /// Serialize the header to bytes.
    pub(crate) fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.checksum.to_le_bytes());
        buf[4..8].copy_from_slice(&self.len.to_le_bytes());
        buf[8..16].copy_from_slice(&(*self.seq).to_le_bytes());
        buf[16..20].copy_from_slice(&self.flags.to_le_bytes());
        buf
    }

    /// Deserialize from bytes.
    ///
    /// # Safety note
    ///
    /// The `unwrap()` calls below are infallible because `buf` is a fixed-size
    /// array of exactly `Self::SIZE` (`20`) bytes, and each slice is exactly
    /// the size needed for the corresponding integer type.
    #[allow(clippy::unwrap_used)]
    pub(crate) fn from_bytes(buf: &[u8; Self::SIZE]) -> Self {
        Self {
            checksum: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            len: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            seq: WalSequence::from(u64::from_le_bytes(
                buf[8..16].try_into().unwrap(),
            )),
            flags: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        }
    }

    /// Verify the checksum matches the given payload.
    pub(crate) fn verify(&self, payload: &[u8]) -> bool {
        payload.len() == self.len as usize
            && crc32fast::hash(payload) == self.checksum
    }
}

/// Raw record data before deserialization.
#[derive(Debug, Clone)]
pub(crate) struct RawRecord {
    /// The record header.
    pub(crate) header: RecordHeader,
    /// The raw payload bytes (not yet deserialized).
    pub(crate) payload: Vec<u8>,
    /// Position immediately after this record.
    pub(crate) end_pos: u64,
}

/// Try to read a raw WAL record at the given position.
///
/// This is the shared low-level record reading logic used by both
/// `WalWriter` (for scanning) and `WalReader` (for iteration).
///
/// # Returns
///
/// - `Ok(Some(raw))` on success
/// - `Ok(None)` if record is incomplete (EOF)
/// - `Err(WalCorruption)` on checksum mismatch
pub(crate) async fn try_read_record_at(
    file: &mut File,
    pos: u64,
    file_size: u64,
) -> Result<Option<RawRecord>> {
    let hdr_end = pos + RecordHeader::SIZE as u64;

    // Check if header fits
    if hdr_end > file_size {
        Ok(None) // EOF: header doesn't fit
    } else {
        file.seek(SeekFrom::Start(pos)).await?;

        let mut hdr_buf = [0u8; RecordHeader::SIZE];
        AsyncReadExt::read_exact(file, &mut hdr_buf).await?;

        let hdr = RecordHeader::from_bytes(&hdr_buf);
        let end_pos = hdr_end + hdr.len as u64;

        // Check if payload fits
        if end_pos > file_size {
            Ok(None) // EOF: payload doesn't fit
        } else {
            let mut payload = vec![0u8; hdr.len as usize];
            AsyncReadExt::read_exact(file, &mut payload).await?;

            // Verify checksum (hard error on mismatch)
            if hdr.verify(&payload) {
                Ok(Some(RawRecord {
                    header: hdr,
                    payload,
                    end_pos,
                }))
            } else {
                Err(StorageError::WalCorruption {
                    seq: *hdr.seq,
                    reason: "checksum mismatch".into(),
                })
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn file_header_roundtrip() {
        let hdr = FileHeader::new(WalSequence::from(12345));
        let bytes = hdr.to_bytes();
        let decoded = FileHeader::from_bytes(&bytes).expect("valid header");
        assert_eq!(decoded, hdr);
    }

    #[test]
    fn file_header_rejects_bad_magic() {
        let mut bytes = FileHeader::new(WalSequence::ZERO).to_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            FileHeader::from_bytes(&bytes),
            Err(StorageError::WalInvalidMagic)
        ));
    }

    #[test]
    fn file_header_rejects_bad_version() {
        let mut bytes = FileHeader::new(WalSequence::ZERO).to_bytes();
        bytes[4] = 99; // wrong version
        assert!(matches!(
            FileHeader::from_bytes(&bytes),
            Err(StorageError::WalUnsupportedVersion(99))
        ));
    }

    #[test]
    fn record_header_roundtrip() {
        let payload = b"hello world";
        let hdr = RecordHeader::new(WalSequence::from(42), payload);
        let bytes = hdr.to_bytes();
        let decoded = RecordHeader::from_bytes(&bytes);
        assert_eq!(decoded, hdr);
        assert!(decoded.verify(payload));
    }

    #[test]
    fn record_header_detects_corruption() {
        let payload = b"hello world";
        let hdr = RecordHeader::new(WalSequence::from(42), payload);
        let corrupted = b"hello worLd"; // one byte changed
        assert!(!hdr.verify(corrupted));
    }

    #[test]
    fn record_header_detects_truncation() {
        let payload = b"hello world";
        let hdr = RecordHeader::new(WalSequence::from(42), payload);
        let truncated = b"hello";
        assert!(!hdr.verify(truncated));
    }

    #[test]
    fn crc32_known_values() {
        // Empty string
        assert_eq!(crc32fast::hash(b""), 0x0000_0000);
        // "123456789" has well-known CRC32 = 0xCBF43926
        assert_eq!(crc32fast::hash(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn header_sizes_correct() {
        assert_eq!(FileHeader::SIZE, 16);
        assert_eq!(RecordHeader::SIZE, 20);
        assert_eq!(
            FileHeader::new(WalSequence::ZERO).to_bytes().len(),
            FileHeader::SIZE
        );
        assert_eq!(
            RecordHeader::new(WalSequence::ZERO, b"").to_bytes().len(),
            RecordHeader::SIZE
        );
    }
}
