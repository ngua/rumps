//! WAL writer for appending records and managing file rotation.
//!
//! The `WalWriter` handles:
//! - Appending serialized `WalRecord`s to the WAL file
//! - Flushing/syncing based on configurable `SyncMode`
//! - Rotating WAL files when size exceeds a threshold
//!
//! # Thread Safety
//!
//! `WalWriter` uses `tokio::sync::Mutex` internally and is safe to share
//! via `Arc<WalWriter>` across tasks.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex;

use super::format::{
    FileHeader, RecordHeader, FILE_HEADER_SIZE, RECORD_HEADER_SIZE,
};
use super::WalRecord;
use crate::error::{Result, StorageError};

/// When to sync WAL data to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SyncMode {
    /// Sync on every write (slowest but safest).
    ///
    /// Each `append` call is immediately flushed and synced to disk.
    /// Maximum durability, minimum performance.
    Immediate,

    /// Sync only on transaction commit.
    ///
    /// Writes are buffered until `sync()` is explicitly called.
    /// Good balance of durability and performance.
    #[default]
    OnCommit,

    /// Sync periodically at a specified interval.
    ///
    /// A background task (managed externally) should call `sync()`
    /// at the specified interval. Until then, writes are buffered.
    Periodic(Duration),
}

/// Configuration for `WalWriter`.
#[derive(Debug, Clone)]
pub(crate) struct WalWriterConfig {
    /// When to sync writes to disk.
    pub(crate) sync_mode: SyncMode,

    /// Maximum WAL file size in bytes before rotation.
    ///
    /// When the current file exceeds this threshold, a new file
    /// is created. Default: 64 MiB.
    pub(crate) max_file_size: u64,
}

impl Default for WalWriterConfig {
    fn default() -> Self {
        Self {
            sync_mode: SyncMode::default(),
            max_file_size: 64 * 1024 * 1024, // 64 MiB
        }
    }
}

/// Result of opening/creating a WAL file.
struct OpenedWal {
    file: File,
    file_size: u64,
    first_seq: u64,
    next_seq: u64,
}

/// Internal mutable state of the WAL writer.
struct WalWriterState {
    /// Current WAL file handle.
    file: File,
    /// Current file path (for rotation).
    path: PathBuf,
    /// Current file size in bytes.
    file_size: u64,
    /// Next sequence number to assign.
    next_seq: u64,
    /// First sequence number in the current file.
    first_seq_in_file: u64,
}

/// WAL writer that appends records and manages file rotation.
///
/// # Example
///
/// ```ignore
/// use rumps_storage::wal::{WalWriter, WalWriterConfig, SyncMode, WalRecord};
/// use std::path::Path;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let cfg = WalWriterConfig {
///         sync_mode: SyncMode::OnCommit,
///         max_file_size: 16 * 1024 * 1024, // 16 MiB
///     };
///     let writer = WalWriter::open(Path::new("./data/wal.log"), cfg).await?;
///
///     // Append a record
///     let rec = WalRecord::TxnBegin { txn_id: 1.into() };
///     let seq = writer.append(&rec).await?;
///
///     // Explicitly sync on commit
///     writer.sync().await?;
///     Ok(())
/// }
/// ```
pub(crate) struct WalWriter {
    /// Directory containing WAL files.
    dir: PathBuf,
    /// Configuration.
    cfg: WalWriterConfig,
    /// Internally mutable state. Callers wrap `WalWriter` in `Arc` for sharing.
    state: Mutex<WalWriterState>,
}

impl WalWriter {
    /// Open or create a WAL writer at the given directory.
    ///
    /// If a WAL file already exists, it will be opened for appending.
    /// The `next_seq` continues from where it left off.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the directory cannot be created or the file
    /// cannot be opened.
    pub(crate) async fn open(dir: &Path, cfg: WalWriterConfig) -> Result<Self> {
        // Ensure directory exists
        fs::create_dir_all(dir).await?;

        let path = dir.join("wal.log");
        let opened = Self::open_or_create_file(&path).await?;

        let state = WalWriterState {
            file: opened.file,
            path,
            file_size: opened.file_size,
            next_seq: opened.next_seq,
            first_seq_in_file: opened.first_seq,
        };

        Ok(Self {
            dir: dir.to_path_buf(),
            cfg,
            state: Mutex::new(state),
        })
    }

    /// Open an existing WAL file or create a new one.
    async fn open_or_create_file(path: &Path) -> Result<OpenedWal> {
        let exists = fs::try_exists(path).await.unwrap_or(false);

        if exists {
            Self::open_existing_file(path).await
        } else {
            Self::create_new_file(path, 0).await
        }
    }

    /// Open an existing WAL file and determine `next_seq`.
    async fn open_existing_file(path: &Path) -> Result<OpenedWal> {
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(path)
            .await?;

        let file_size = file.metadata().await?.len();

        // Need at least the file header
        if file_size < FILE_HEADER_SIZE as u64 {
            // Corrupt or empty file; recreate
            drop(file);
            Self::create_new_file(path, 0).await
        } else {
            // Read file header
            file.seek(SeekFrom::Start(0)).await?;
            let mut hdr_buf = [0u8; FILE_HEADER_SIZE];
            AsyncReadExt::read_exact(&mut file, &mut hdr_buf).await?;

            let hdr = FileHeader::from_bytes(&hdr_buf).ok_or_else(|| {
                StorageError::InvalidOperation("Invalid WAL file header".into())
            })?;

            // Scan to find the last sequence number
            let next_seq =
                Self::scan_for_last_seq(&mut file, hdr.first_seq, file_size)
                    .await?;

            // Seek to end for appending
            file.seek(SeekFrom::End(0)).await?;

            Ok(OpenedWal {
                file,
                file_size,
                first_seq: hdr.first_seq,
                next_seq,
            })
        }
    }

    /// Scan WAL to find `next_seq` (one past last valid record).
    async fn scan_for_last_seq(
        file: &mut File,
        first_seq: u64,
        file_size: u64,
    ) -> Result<u64> {
        let mut pos = FILE_HEADER_SIZE as u64;
        let mut next_seq = first_seq;

        while let Some((end, seq)) =
            Self::try_read_record(file, pos, file_size).await?
        {
            next_seq = seq + 1;
            pos = end;
        }

        Ok(next_seq)
    }

    /// Try to read one WAL record at `pos`.
    ///
    /// Returns `Ok(Some((payload_end, seq)))` on success, `Ok(None)` on soft
    /// failure (incomplete, I/O error, checksum mismatch).
    async fn try_read_record(
        file: &mut File,
        pos: u64,
        file_size: u64,
    ) -> Result<Option<(u64, u64)>> {
        let header_end = pos + RECORD_HEADER_SIZE as u64;

        if header_end > file_size {
            Ok(None)
        } else {
            file.seek(SeekFrom::Start(pos)).await?;

            let mut hdr_buf = [0u8; RECORD_HEADER_SIZE];
            let hdr_read = AsyncReadExt::read_exact(file, &mut hdr_buf).await;

            let parsed = hdr_read
                .ok()
                .map(|_| {
                    let hdr = RecordHeader::from_bytes(&hdr_buf);
                    let end = pos + RECORD_HEADER_SIZE as u64 + hdr.len as u64;
                    (hdr, end)
                })
                .filter(|(_, end)| *end <= file_size);

            match parsed {
                None => Ok(None),
                Some((hdr, end)) => {
                    let mut payload = vec![0u8; hdr.len as usize];
                    let verified = AsyncReadExt::read_exact(file, &mut payload)
                        .await
                        .ok()
                        .filter(|_| hdr.verify(&payload))
                        .map(|_| (end, hdr.seq));

                    Ok(verified)
                }
            }
        }
    }

    /// Create a new WAL file with the given first sequence number.
    async fn create_new_file(path: &Path, first_seq: u64) -> Result<OpenedWal> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await?;

        // Write file header
        let hdr = FileHeader::new(first_seq);
        file.write_all(&hdr.to_bytes()).await?;
        file.sync_all().await?;

        Ok(OpenedWal {
            file,
            file_size: FILE_HEADER_SIZE as u64,
            first_seq,
            next_seq: first_seq,
        })
    }

    /// Append a WAL record and return its sequence number.
    ///
    /// The record is serialized and written to the current WAL file.
    /// If `SyncMode::Immediate`, the write is synced before returning.
    ///
    /// # Rotation
    ///
    /// If appending would exceed `max_file_size`, the current file
    /// is rotated first.
    pub(crate) async fn append(&self, rec: &WalRecord) -> Result<u64> {
        let payload = bincode::serialize(rec).map_err(|e| {
            StorageError::Serialization(format!("WAL record: {e}"))
        })?;

        let mut state = self.state.lock().await;

        // Check if rotation is needed
        let rec_size = RECORD_HEADER_SIZE as u64 + payload.len() as u64;
        if state.file_size + rec_size > self.cfg.max_file_size {
            self.rotate_locked(&mut state).await?;
        }

        let seq = state.next_seq;
        let hdr = RecordHeader::new(seq, &payload);

        // Write header + payload
        state.file.write_all(&hdr.to_bytes()).await?;
        state.file.write_all(&payload).await?;

        state.file_size += rec_size;
        state.next_seq += 1;

        // Sync if immediate mode
        if self.cfg.sync_mode == SyncMode::Immediate {
            state.file.sync_all().await?;
        }

        Ok(seq)
    }

    /// Explicitly flush and sync the WAL to disk.
    ///
    /// Call this after committing a transaction when using
    /// `SyncMode::OnCommit` or `SyncMode::Periodic`.
    pub(crate) async fn sync(&self) -> Result<()> {
        let state = self.state.lock().await;
        state.file.sync_all().await?;
        Ok(())
    }

    /// Rotate to a new WAL file.
    ///
    /// The current file is renamed with a timestamp suffix,
    /// and a new file is created.
    async fn rotate_locked(&self, state: &mut WalWriterState) -> Result<()> {
        // Sync current file before rotation
        state.file.sync_all().await?;

        // Rename old file with sequence range suffix
        let old_path = state.path.clone();
        let new_name = format!(
            "wal.{:016x}-{:016x}.log",
            state.first_seq_in_file,
            state.next_seq.saturating_sub(1)
        );
        let archive_path = self.dir.join(new_name);
        fs::rename(&old_path, &archive_path).await?;

        // Create new file
        let opened = Self::create_new_file(&old_path, state.next_seq).await?;

        state.file = opened.file;
        state.path = old_path;
        state.file_size = opened.file_size;
        state.first_seq_in_file = opened.first_seq;
        state.next_seq = opened.next_seq;

        Ok(())
    }

    /// Force rotation to a new WAL file.
    ///
    /// Useful for checkpointing when you want to start fresh.
    pub(crate) async fn rotate(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        self.rotate_locked(&mut state).await
    }

    /// Get the current file size in bytes.
    pub(crate) async fn file_size(&self) -> u64 {
        self.state.lock().await.file_size
    }

    /// Get the next sequence number that will be assigned.
    pub(crate) async fn next_seq(&self) -> u64 {
        self.state.lock().await.next_seq
    }

    /// Get the sync mode.
    pub(crate) fn sync_mode(&self) -> SyncMode {
        self.cfg.sync_mode
    }

    /// Get the maximum file size before rotation.
    pub(crate) fn max_file_size(&self) -> u64 {
        self.cfg.max_file_size
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use rumps_types::{global, key};
    use tempfile::TempDir;

    use super::*;
    use crate::node::NodeData;
    use crate::transaction::TransactionId;

    async fn temp_writer(cfg: WalWriterConfig) -> (WalWriter, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let writer = WalWriter::open(dir.path(), cfg).await.expect("open");
        (writer, dir)
    }

    #[tokio::test]
    async fn creates_wal_file() {
        let (writer, dir) = temp_writer(WalWriterConfig::default()).await;

        let wal_path = dir.path().join("wal.log");
        assert!(wal_path.exists());
        assert_eq!(writer.file_size().await, FILE_HEADER_SIZE as u64);
        assert_eq!(writer.next_seq().await, 0);
    }

    #[tokio::test]
    async fn append_increments_seq() {
        let (writer, _dir) = temp_writer(WalWriterConfig::default()).await;

        let rec1 = WalRecord::TxnBegin {
            txn_id: TransactionId::from(1),
        };
        let rec2 = WalRecord::TxnCommit {
            txn_id: TransactionId::from(1),
        };

        let seq1 = writer.append(&rec1).await.expect("append");
        let seq2 = writer.append(&rec2).await.expect("append");

        assert_eq!(seq1, 0);
        assert_eq!(seq2, 1);
        assert_eq!(writer.next_seq().await, 2);
    }

    #[tokio::test]
    async fn sync_mode_immediate_syncs() {
        let cfg = WalWriterConfig {
            sync_mode: SyncMode::Immediate,
            ..Default::default()
        };
        let (writer, _dir) = temp_writer(cfg).await;

        let rec = WalRecord::TxnBegin {
            txn_id: TransactionId::from(1),
        };
        // Should not panic; sync happens internally
        writer.append(&rec).await.expect("append");
    }

    #[tokio::test]
    async fn reopen_continues_seq() {
        let dir = TempDir::new().expect("temp dir");

        // Write some records
        {
            let writer =
                WalWriter::open(dir.path(), WalWriterConfig::default())
                    .await
                    .expect("open");
            writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append");
            writer
                .append(&WalRecord::TxnCommit {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append");
            writer.sync().await.expect("sync");
        }

        // Reopen and verify seq continues
        {
            let writer =
                WalWriter::open(dir.path(), WalWriterConfig::default())
                    .await
                    .expect("reopen");
            assert_eq!(writer.next_seq().await, 2);

            let seq = writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(2),
                })
                .await
                .expect("append");
            assert_eq!(seq, 2);
        }
    }

    #[tokio::test]
    async fn rotation_on_size_limit() {
        let cfg = WalWriterConfig {
            sync_mode: SyncMode::OnCommit,
            max_file_size: 100, // Very small to trigger rotation
        };
        let (writer, dir) = temp_writer(cfg).await;

        // Append records until rotation
        let rec = WalRecord::Set {
            txn_id: TransactionId::from(1),
            name: global!("TEST"),
            key: key!["abc"],
            old: None,
            new: NodeData::new(Some("value".into()), false),
        };

        // Each record is ~50+ bytes, so 2-3 should trigger rotation
        writer.append(&rec).await.expect("append 1");
        writer.append(&rec).await.expect("append 2");
        writer.append(&rec).await.expect("append 3");
        writer.sync().await.expect("sync");

        // Check that archive file was created
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("-"))
            .collect();

        assert!(!entries.is_empty(), "Expected archived WAL file");
    }

    #[tokio::test]
    async fn explicit_rotate() {
        let (writer, dir) = temp_writer(WalWriterConfig::default()).await;

        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append");
        writer.rotate().await.expect("rotate");

        // Check archive exists
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("-"))
            .collect();

        assert_eq!(entries.len(), 1);

        // New file should have seq = 1
        assert_eq!(writer.next_seq().await, 1);
        assert_eq!(writer.file_size().await, FILE_HEADER_SIZE as u64);
    }

    #[tokio::test]
    async fn all_record_types() {
        let (writer, _dir) = temp_writer(WalWriterConfig::default()).await;

        let recs = vec![
            WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            },
            WalRecord::Set {
                txn_id: TransactionId::from(1),
                name: global!("X"),
                key: key![1],
                old: None,
                new: NodeData::new(Some(42i64.into()), false),
            },
            WalRecord::KillEntry {
                txn_id: TransactionId::from(1),
                name: global!("X"),
                key: key![2],
                data: NodeData::new(Some("deleted".into()), true),
            },
            WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            },
            WalRecord::Checkpoint { seq: 100 },
        ];

        recs.iter()
            .enumerate()
            .try_for_each(|(i, rec)| {
                futures::executor::block_on(async {
                    let seq = writer.append(rec).await?;
                    assert_eq!(seq, i as u64);
                    Ok::<_, StorageError>(())
                })
            })
            .expect("all appends succeed");

        writer.sync().await.expect("sync");
    }

    #[tokio::test]
    async fn file_grows_with_appends() {
        let (writer, _dir) = temp_writer(WalWriterConfig::default()).await;

        let initial = writer.file_size().await;

        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append");

        let after_one = writer.file_size().await;
        assert!(after_one > initial);

        writer
            .append(&WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append");

        let after_two = writer.file_size().await;
        assert!(after_two > after_one);
    }

    #[tokio::test]
    async fn concurrent_appends() {
        use std::sync::Arc;

        let (writer, _dir) = temp_writer(WalWriterConfig::default()).await;
        let writer = Arc::new(writer);

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let w = Arc::clone(&writer);
                tokio::spawn(async move {
                    let rec = WalRecord::TxnBegin {
                        txn_id: TransactionId::from(i),
                    };
                    w.append(&rec).await.expect("append")
                })
            })
            .collect();

        let seqs: Vec<u64> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.expect("join"))
            .collect();

        // All seqs should be unique and in range [0, 10)
        let mut sorted = seqs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 10);
        assert_eq!(*sorted.first().unwrap(), 0);
        assert_eq!(*sorted.last().unwrap(), 9);
    }
}
