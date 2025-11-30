//! WAL file discovery and metadata.
//!
//! Provides utilities for finding and ordering WAL files in a directory.

use std::path::{Path, PathBuf};

use futures::stream::{self, StreamExt};
use tokio::fs;

use super::format::{FileHeader, FILE_HEADER_SIZE};
use super::WalSequence;
use crate::error::{Result, StorageError};

/// Metadata for a single WAL file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WalFileInfo {
    /// Path to the WAL file.
    pub(crate) path: PathBuf,
    /// First sequence number in this file.
    pub(crate) first_seq: WalSequence,
    /// Last sequence number (known for archives only).
    pub(crate) last_seq: Option<WalSequence>,
    /// Whether this is the active `wal.log` file.
    pub(crate) is_active: bool,
}

/// Parse an archived WAL filename.
///
/// Archives are named `wal.{first_seq:016x}-{last_seq:016x}.log`.
/// Returns `Some((first, last))` if valid, `None` otherwise.
pub(crate) fn parse_archive_name(
    name: &str,
) -> Option<(WalSequence, WalSequence)> {
    let name = name.strip_prefix("wal.")?;
    let name = name.strip_suffix(".log")?;

    let mut parts = name.split('-');
    let first = parts.next()?;
    let last = parts.next()?;

    // Ensure no extra parts
    if parts.next().is_some() {
        None
    } else {
        let first_seq = u64::from_str_radix(first, 16).ok()?;
        let last_seq = u64::from_str_radix(last, 16).ok()?;
        Some((WalSequence::new(first_seq), WalSequence::new(last_seq)))
    }
}

/// Discover all WAL files in a directory, sorted by `first_seq`.
///
/// Returns an empty `Vec` if no WAL files exist.
///
/// # Errors
///
/// Returns `WalCorruption` if sequence gaps are detected between files.
pub(crate) async fn discover_wal_files(dir: &Path) -> Result<Vec<WalFileInfo>> {
    match fs::read_dir(dir).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
        Ok(entries) => {
            // Unfold ReadDir into a stream of DirEntry
            let entry_stream = stream::unfold(entries, |mut rd| async {
                rd.next_entry().await.transpose().map(|res| (res, rd))
            });

            // Process entries into WalFileInfo, filtering out non-WAL files
            let mut files: Vec<WalFileInfo> = entry_stream
                .filter_map(|res| async {
                    let entry = res.ok()?;
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    let path = entry.path();

                    if name_str == "wal.log" {
                        // Active file - read header to get `first_seq`
                        read_file_header(&path).await.ok().map(|hdr| {
                            WalFileInfo {
                                path,
                                first_seq: hdr.first_seq,
                                last_seq: None,
                                is_active: true,
                            }
                        })
                    } else {
                        // Try parsing as archive
                        parse_archive_name(&name_str).map(|(first, last)| {
                            WalFileInfo {
                                path,
                                first_seq: first,
                                last_seq: Some(last),
                                is_active: false,
                            }
                        })
                    }
                })
                .collect()
                .await;

            // Sort by `first_seq`
            files.sort_by_key(|f| f.first_seq);

            // Validate sequence continuity
            validate_sequence_continuity(&files)?;

            Ok(files)
        }
    }
}

/// Read the file header from a WAL file.
async fn read_file_header(path: &Path) -> Result<FileHeader> {
    use tokio::io::AsyncReadExt;

    let mut file = fs::File::open(path).await?;
    let mut buf = [0u8; FILE_HEADER_SIZE];

    // `read_exact` either reads exactly `FILE_HEADER_SIZE` bytes or errors
    file.read_exact(&mut buf)
        .await
        .map_err(|_| StorageError::WalCorruption {
            seq: 0,
            reason: format!("truncated header in {}", path.display()),
        })
        .and_then(|_| {
            FileHeader::from_bytes(&buf).ok_or_else(|| {
                StorageError::WalCorruption {
                    seq: 0,
                    reason: format!("invalid header in {}", path.display()),
                }
            })
        })
}

/// Validate that WAL files have continuous sequences (no gaps).
fn validate_sequence_continuity(files: &[WalFileInfo]) -> Result<()> {
    files.windows(2).try_for_each(|pair| {
        // `windows(2)` guarantees exactly 2 elements
        let (prev, curr) = match pair {
            [p, c] => (p, c),
            _ => unreachable!("windows(2) yields slices of length 2"),
        };

        // For archives, `last_seq + 1` should equal next file's `first_seq`
        // For active file (no `last_seq`), we can't validate
        match prev.last_seq {
            Some(last) if last.next() != curr.first_seq => {
                Err(StorageError::WalCorruption {
                    seq: curr.first_seq.get(),
                    reason: format!(
                        "sequence gap: expected {}, found {}",
                        last.next(),
                        curr.first_seq
                    ),
                })
            }
            _ => Ok(()),
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::wal::format::FileHeader;

    #[test]
    fn parse_archive_name_valid() {
        let cases = [
            (
                "wal.0000000000000000-0000000000000005.log",
                (WalSequence::ZERO, WalSequence::new(5)),
            ),
            (
                "wal.0000000000000010-00000000000000ff.log",
                (WalSequence::new(16), WalSequence::new(255)),
            ),
            (
                "wal.ffffffffffffffff-ffffffffffffffff.log",
                (WalSequence::new(u64::MAX), WalSequence::new(u64::MAX)),
            ),
        ];

        cases.iter().for_each(|(name, expected)| {
            assert_eq!(parse_archive_name(name), Some(*expected));
        });
    }

    #[test]
    fn parse_archive_name_invalid() {
        let cases = [
            "wal.log",                                     // Active file
            "wal.0000000000000000.log",                    // Missing last
            "wal.0000000000000000-0000000000000005.txt",   // Wrong ext
            "wal.0000000000000000-0000000000000005-x.log", // Extra part
            "data.db",                                     // Not WAL
            "wal.ghij-0000000000000005.log",               // Invalid hex
        ];

        cases.iter().for_each(|name| {
            assert_eq!(parse_archive_name(name), None);
        });
    }

    #[tokio::test]
    async fn discover_empty_dir() {
        let dir = TempDir::new().expect("temp dir");
        let files = discover_wal_files(dir.path()).await.expect("discover");
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn discover_single_active() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("wal.log");

        // Write valid header
        let mut f = fs::File::create(&path).await.expect("create");
        let hdr = FileHeader::new(WalSequence::new(10));
        f.write_all(&hdr.to_bytes()).await.expect("write");

        let files = discover_wal_files(dir.path()).await.expect("discover");
        assert_eq!(files.len(), 1);
        assert!(files.first().unwrap().is_active);
        assert_eq!(files.first().unwrap().first_seq, WalSequence::new(10));
    }

    #[tokio::test]
    async fn discover_archives_sorted() {
        let dir = TempDir::new().expect("temp dir");

        // Create archives out of order
        let archives = [
            ("wal.0000000000000005-0000000000000009.log", 5, 9),
            ("wal.0000000000000000-0000000000000004.log", 0, 4),
        ];

        futures::future::join_all(archives.iter().map(|(name, first, _)| {
            let path = dir.path().join(name);
            async move {
                let mut f = fs::File::create(&path).await.expect("create");
                let hdr = FileHeader::new(WalSequence::new(*first));
                f.write_all(&hdr.to_bytes()).await.expect("write");
            }
        }))
        .await;

        let files = discover_wal_files(dir.path()).await.expect("discover");
        assert_eq!(files.len(), 2);

        // Should be sorted by first_seq
        assert_eq!(files.first().unwrap().first_seq, WalSequence::ZERO);
        assert_eq!(files.get(1).unwrap().first_seq, WalSequence::new(5));
    }

    #[tokio::test]
    async fn discover_detects_gap() {
        let dir = TempDir::new().expect("temp dir");

        // Create archives with gap (0-4, then 10-14, missing 5-9)
        let archives = [
            ("wal.0000000000000000-0000000000000004.log", 0, 4),
            ("wal.000000000000000a-000000000000000e.log", 10, 14),
        ];

        futures::future::join_all(archives.iter().map(|(name, first, _)| {
            let path = dir.path().join(name);
            async move {
                let mut f = fs::File::create(&path).await.expect("create");
                let hdr = FileHeader::new(WalSequence::new(*first));
                f.write_all(&hdr.to_bytes()).await.expect("write");
            }
        }))
        .await;

        let result = discover_wal_files(dir.path()).await;
        assert!(matches!(result, Err(StorageError::WalCorruption { .. })));
    }

    #[tokio::test]
    async fn discover_archives_and_active() {
        let dir = TempDir::new().expect("temp dir");

        // Archive first, then active
        let archive =
            dir.path().join("wal.0000000000000000-0000000000000004.log");
        let active = dir.path().join("wal.log");

        let mut f = fs::File::create(&archive).await.expect("create archive");
        f.write_all(&FileHeader::new(WalSequence::ZERO).to_bytes())
            .await
            .expect("write");

        let mut f = fs::File::create(&active).await.expect("create active");
        f.write_all(&FileHeader::new(WalSequence::new(5)).to_bytes())
            .await
            .expect("write");

        let files = discover_wal_files(dir.path()).await.expect("discover");
        assert_eq!(files.len(), 2);
        assert!(!files.first().unwrap().is_active);
        assert!(files.get(1).unwrap().is_active);
    }
}
