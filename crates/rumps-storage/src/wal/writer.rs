//! WAL writer for appending records and managing file rotation.
//!
//! # Construction
//!
//! `WalWriter` cannot be constructed directly. Instead, use the
//! [`WalReader::into_writer`] method:
//!
//! ```ignore
//! let mut reader = WalReader::open(dir).await?;
//! while let Some(entry) = reader.next().await? {
//!     // Handle recovery...
//! }
//! let writer = reader.into_writer(cfg).await?;
//! ```
//!
//! See the [`reader`] module documentation for the rationale behind
//! this design.
//!
//! # Thread Safety
//!
//! `WalWriter` uses channels internally and is safe to share via
//! `Arc<WalWriter>` across tasks. The background task serializes
//! all file operations.
//!
//! # Group Commit
//!
//! When multiple tasks call [`sync`] concurrently, their requests are
//! batched into a single `fsync()` call. This amortizes the cost of
//! durable writes across many transactions.
//!
//! [`WalReader::into_writer`]: super::WalReader::into_writer
//! [`reader`]: super::reader
//! [`sync`]: WalWriter::sync

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::fs::{self, File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use super::files::WalFileInfo;
use super::format::{FileHeader, RecordHeader};
use super::{WalRecord, WalSequence};
use crate::error::{Result, StorageError};

/// When to sync WAL data to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SyncMode {
    /// Sync on every write (slowest but safest).
    ///
    /// Each [`append`] call is immediately flushed and synced to disk.
    /// Maximum durability, minimum performance.
    ///
    /// [`append`]: WalWriter::append
    Immediate,

    /// Sync only on transaction commit.
    ///
    /// Writes are buffered until [`sync`] is explicitly called.
    /// Good balance of durability and performance.
    ///
    /// [`sync`]: WalWriter::sync
    #[default]
    OnCommit,

    /// Sync periodically at a specified interval.
    ///
    /// A background task (managed externally) should call [`sync`]
    /// at the specified interval. Until then, writes are buffered.
    ///
    /// [`sync`]: WalWriter::sync
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
    /// is created. Default: `64` MiB.
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

/// Commands sent to the WAL background task.
enum WalCommand {
    Append {
        rec: WalRecord,
        reply: oneshot::Sender<Result<WalSequence>>,
    },
    Sync {
        reply: oneshot::Sender<Result<()>>,
    },
    Rotate {
        reply: oneshot::Sender<Result<()>>,
    },
    Checkpoint {
        flushed_seq: WalSequence,
        reply: oneshot::Sender<Result<WalSequence>>,
    },
}

/// Background task state that owns the WAL file exclusively.
struct WalTask {
    dir: PathBuf,
    cfg: WalWriterConfig,
    file: File,
    path: PathBuf,
    file_size: u64,
    next_seq: WalSequence,
    first_seq_in_file: WalSequence,
    rx: mpsc::Receiver<WalCommand>,
}

impl WalTask {
    /// Main loop: receive commands and handle them.
    async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                WalCommand::Append { rec, reply } => {
                    let result = self.handle_append(rec).await;
                    let _ = reply.send(result);
                }
                WalCommand::Sync { reply } => {
                    // Collect all pending sync requests (group commit)
                    let mut waiters = vec![reply];
                    while let Ok(cmd) = self.rx.try_recv() {
                        match cmd {
                            WalCommand::Sync { reply } => waiters.push(reply),
                            other => self.handle_non_sync(other).await,
                        }
                    }
                    // One fsync for all waiters
                    let result = self.file.sync_all().await;
                    let err_msg = result.as_ref().err().map(|e| e.to_string());
                    waiters.into_iter().for_each(|w| {
                        let r = err_msg.as_ref().map_or(Ok(()), |msg| {
                            Err(StorageError::Io(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                msg.clone(),
                            )))
                        });
                        let _ = w.send(r);
                    });
                }
                WalCommand::Rotate { reply } => {
                    let result = self.handle_rotate().await;
                    let _ = reply.send(result);
                }
                WalCommand::Checkpoint { flushed_seq, reply } => {
                    let result = self.handle_checkpoint(flushed_seq).await;
                    let _ = reply.send(result);
                }
            }
        }
    }

    /// Handle non-sync commands that arrive during sync collection.
    async fn handle_non_sync(&mut self, cmd: WalCommand) {
        match cmd {
            WalCommand::Append { rec, reply } => {
                let result = self.handle_append(rec).await;
                let _ = reply.send(result);
            }
            WalCommand::Rotate { reply } => {
                let result = self.handle_rotate().await;
                let _ = reply.send(result);
            }
            WalCommand::Checkpoint { flushed_seq, reply } => {
                let result = self.handle_checkpoint(flushed_seq).await;
                let _ = reply.send(result);
            }
            WalCommand::Sync { .. } => {
                // Handled by caller's batch
            }
        }
    }

    /// Append a record to the WAL.
    async fn handle_append(&mut self, rec: WalRecord) -> Result<WalSequence> {
        let payload = bincode::serialize(&rec).map_err(|e| {
            StorageError::Serialization(format!("WAL record: {e}"))
        })?;

        // Check if rotation is needed
        let rec_size = RecordHeader::SIZE as u64 + payload.len() as u64;
        if self.file_size + rec_size > self.cfg.max_file_size {
            self.handle_rotate().await?;
        }

        let seq = self.next_seq;
        let hdr = RecordHeader::new(seq, &payload);

        // Write header + payload
        self.file.write_all(&hdr.to_bytes()).await?;
        self.file.write_all(&payload).await?;

        self.file_size += rec_size;
        self.next_seq = self.next_seq.next();

        // Sync if immediate mode
        if self.cfg.sync_mode == SyncMode::Immediate {
            self.file.sync_all().await?;
        }

        Ok(seq)
    }

    /// Rotate to a new WAL file.
    async fn handle_rotate(&mut self) -> Result<()> {
        // Sync current file before rotation
        self.file.sync_all().await?;

        // Rename old file with sequence range suffix
        let old_path = self.path.clone();
        let new_name = format!(
            "wal.{:016x}-{:016x}.log",
            *self.first_seq_in_file,
            *self.next_seq.saturating_sub(1)
        );
        let archive_path = self.dir.join(new_name);
        fs::rename(&old_path, &archive_path).await?;

        // Create new file
        let (file, file_size) =
            Self::create_new_file(&old_path, self.next_seq).await?;

        self.file = file;
        self.path = old_path;
        self.file_size = file_size;
        self.first_seq_in_file = self.next_seq;

        Ok(())
    }

    /// Perform a WAL checkpoint.
    async fn handle_checkpoint(
        &mut self,
        flushed_seq: WalSequence,
    ) -> Result<WalSequence> {
        // Write checkpoint record
        let checkpoint_rec = WalRecord::Checkpoint { seq: flushed_seq };
        let checkpoint_seq = self.handle_append(checkpoint_rec).await?;

        // Sync to ensure checkpoint is durable
        self.file.sync_all().await?;

        // Rotate to start fresh file
        self.handle_rotate().await?;

        // Clean up old archived files
        self.cleanup_archived_files(flushed_seq).await?;

        Ok(checkpoint_seq)
    }

    /// Create a new WAL file with the given first sequence number.
    async fn create_new_file(
        path: &Path,
        first_seq: WalSequence,
    ) -> Result<(File, u64)> {
        let mut file = OpenOptions::new()
            .write(true)
            .read(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await?;

        // Write file header
        let hdr = FileHeader::new(first_seq);
        file.write_all(&hdr.to_bytes()).await?;
        file.sync_all().await?;

        Ok((file, FileHeader::SIZE as u64))
    }

    /// Delete archived WAL files that are entirely before the checkpoint.
    async fn cleanup_archived_files(
        &self,
        checkpoint_seq: WalSequence,
    ) -> Result<()> {
        let mut entries = fs::read_dir(&self.dir).await?;

        // Collect files to delete
        let mut to_delete = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if let Some(info) =
                WalFileInfo::from_archive_name(entry.path(), &name_str)
            {
                if info.last_seq.is_some_and(|seq| seq <= checkpoint_seq) {
                    to_delete.push(info.path);
                }
            }
        }

        // Delete old files
        futures::future::try_join_all(to_delete.iter().map(fs::remove_file))
            .await?;

        Ok(())
    }
}

/// WAL writer that appends records and manages file rotation.
///
/// # Construction
///
/// This type cannot be constructed directly. Use [`WalReader::into_writer`]:
///
/// ```ignore
/// let reader = WalReader::open(dir).await?;
/// // ... iterate for recovery ...
/// let writer = reader.into_writer(cfg).await?;
/// ```
///
/// # Group Commit
///
/// Concurrent [`sync`] calls are automatically batched into a single
/// `fsync()` operation, improving throughput under concurrent workloads.
///
/// [`WalReader::into_writer`]: super::WalReader::into_writer
/// [`sync`]: Self::sync
pub(crate) struct WalWriter {
    tx: mpsc::Sender<WalCommand>,
    dir: PathBuf,
    cfg: WalWriterConfig,
}

impl WalWriter {
    /// Create a writer from a reader.
    ///
    /// This is called by `WalReader::into_writer` and should not be
    /// used directly. It consumes the reader, reopens the file for
    /// appending, and spawns the background task.
    ///
    /// If only archived files exist (no active `wal.log`), a new one
    /// is created continuing from the reader's next sequence number.
    pub(super) async fn from_reader(
        reader: super::WalReader,
        cfg: WalWriterConfig,
    ) -> Result<Self> {
        let has_active = reader.files.iter().any(WalFileInfo::is_active);

        let (file, path, file_size, next_seq, first_seq) = if has_active {
            // Close read-only handle
            drop(reader.file);

            // Reopen with append mode
            let file = OpenOptions::new()
                .read(true)
                .append(true)
                .open(&reader.path)
                .await?;

            (
                file,
                reader.path,
                reader.file_size,
                reader.next_seq,
                reader.first_seq,
            )
        } else {
            // Only archives exist - create new wal.log
            let path = reader.dir.join("wal.log");
            let first_seq = reader.next_seq;

            let (file, file_size) =
                WalTask::create_new_file(&path, first_seq).await?;

            // Reopen in append mode
            drop(file);
            let file = OpenOptions::new()
                .read(true)
                .append(true)
                .open(&path)
                .await?;

            (file, path, file_size, first_seq, first_seq)
        };

        let (tx, rx) = mpsc::channel(256);

        let task = WalTask {
            dir: reader.dir.clone(),
            cfg: cfg.clone(),
            file,
            path,
            file_size,
            next_seq,
            first_seq_in_file: first_seq,
            rx,
        };

        tokio::spawn(task.run());

        Ok(Self {
            tx,
            dir: reader.dir,
            cfg,
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
    pub(crate) async fn append(&self, rec: &WalRecord) -> Result<WalSequence> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(WalCommand::Append {
                rec: rec.clone(),
                reply: tx,
            })
            .await
            .map_err(|_| StorageError::WalShutdown)?;
        rx.await.map_err(|_| StorageError::WalShutdown)?
    }

    /// Explicitly flush and sync the WAL to disk.
    ///
    /// Call this after committing a transaction when using
    /// `SyncMode::OnCommit` or `SyncMode::Periodic`.
    ///
    /// # Group Commit
    ///
    /// Concurrent calls to this method are batched: if multiple tasks
    /// call `sync()` while an fsync is in progress, they all share
    /// the result of a single fsync operation.
    pub(crate) async fn sync(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(WalCommand::Sync { reply: tx })
            .await
            .map_err(|_| StorageError::WalShutdown)?;
        rx.await.map_err(|_| StorageError::WalShutdown)?
    }

    /// Force rotation to a new WAL file.
    ///
    /// Useful for checkpointing when you want to start fresh.
    pub(crate) async fn rotate(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(WalCommand::Rotate { reply: tx })
            .await
            .map_err(|_| StorageError::WalShutdown)?;
        rx.await.map_err(|_| StorageError::WalShutdown)?
    }

    /// Perform a WAL checkpoint.
    ///
    /// Checkpointing is a two-phase process:
    /// 1. Write a `Checkpoint` record to mark the point where all prior
    ///    operations have been flushed to the main data file
    /// 2. Delete archived WAL files that are entirely before the checkpoint
    ///
    /// # Arguments
    ///
    /// * `flushed_seq` - The sequence number up to which all data has been
    ///   flushed to the main data file. Operations with `seq <= flushed_seq`
    ///   no longer need to be replayed during recovery.
    ///
    /// # Rotation
    ///
    /// After writing the checkpoint, the current WAL file is rotated so
    /// the checkpoint begins a new file. This makes it easier to delete
    /// old files: any archived file whose `last_seq <= flushed_seq` can
    /// be safely removed.
    ///
    /// # Returns
    ///
    /// The sequence number of the checkpoint record itself.
    pub(crate) async fn checkpoint(
        &self,
        flushed_seq: WalSequence,
    ) -> Result<WalSequence> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(WalCommand::Checkpoint {
                flushed_seq,
                reply: tx,
            })
            .await
            .map_err(|_| StorageError::WalShutdown)?;
        rx.await.map_err(|_| StorageError::WalShutdown)?
    }

    /// Get the sync mode.
    pub(crate) fn sync_mode(&self) -> SyncMode {
        self.cfg.sync_mode
    }

    /// Get the maximum file size before rotation.
    pub(crate) fn max_file_size(&self) -> u64 {
        self.cfg.max_file_size
    }

    /// Get the directory containing WAL files.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
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
    use crate::wal::WalReader;

    /// Helper: open reader and convert to writer.
    async fn open_writer(dir: &Path, cfg: WalWriterConfig) -> WalWriter {
        let mut reader = WalReader::open(dir).await.expect("open reader");
        // Drain any existing records
        while reader.next().await.expect("next").is_some() {}
        reader.into_writer(cfg).await.expect("into_writer")
    }

    async fn temp_writer(cfg: WalWriterConfig) -> (WalWriter, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let writer = open_writer(dir.path(), cfg).await;
        (writer, dir)
    }

    #[tokio::test]
    async fn creates_wal_file() {
        let (_writer, dir) = temp_writer(WalWriterConfig::default()).await;

        let wal_path = dir.path().join("wal.log");
        assert!(wal_path.exists());
    }

    #[tokio::test]
    async fn append_returns_incrementing_seqs() {
        let (writer, _dir) = temp_writer(WalWriterConfig::default()).await;

        let rec1 = WalRecord::TxnBegin {
            txn_id: TransactionId::from(1),
        };
        let rec2 = WalRecord::TxnCommit {
            txn_id: TransactionId::from(1),
        };

        let seq1 = writer.append(&rec1).await.expect("append");
        let seq2 = writer.append(&rec2).await.expect("append");

        assert_eq!(seq1, WalSequence::ZERO);
        assert_eq!(seq2, WalSequence::from(1));
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
        let seq_after_first = {
            let writer =
                open_writer(dir.path(), WalWriterConfig::default()).await;
            writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append");
            let seq = writer
                .append(&WalRecord::TxnCommit {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append");
            writer.sync().await.expect("sync");
            seq
        };

        // Reopen and verify seq continues
        {
            let writer =
                open_writer(dir.path(), WalWriterConfig::default()).await;

            let seq = writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(2),
                })
                .await
                .expect("append");
            // seq_after_first was 1, so next should be 2
            assert_eq!(seq, seq_after_first.next());
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
            .filter(|e| e.file_name().to_string_lossy().contains('-'))
            .collect();

        assert!(!entries.is_empty(), "Expected archived WAL file");
    }

    #[tokio::test]
    async fn explicit_rotate() {
        let (writer, dir) = temp_writer(WalWriterConfig::default()).await;

        let seq = writer
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
            .filter(|e| e.file_name().to_string_lossy().contains('-'))
            .collect();

        assert_eq!(entries.len(), 1);

        // Next append should continue from seq + 1
        let next = writer
            .append(&WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append");
        assert_eq!(next, seq.next());
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
            WalRecord::Checkpoint {
                seq: WalSequence::from(100),
            },
        ];

        let seqs: Vec<WalSequence> = futures::future::try_join_all(
            recs.iter().map(|rec| writer.append(rec)),
        )
        .await
        .expect("all appends succeed");

        seqs.iter()
            .enumerate()
            .for_each(|(i, seq)| assert_eq!(*seq, WalSequence::from(i as u64)));

        writer.sync().await.expect("sync");
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

        let seqs: Vec<WalSequence> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.expect("join"))
            .collect();

        // All seqs should be unique and in range [0, 10)
        let mut sorted = seqs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 10);
        assert_eq!(*sorted.first().unwrap(), WalSequence::ZERO);
        assert_eq!(*sorted.last().unwrap(), WalSequence::from(9));
    }

    #[tokio::test]
    async fn concurrent_syncs_batch() {
        use std::sync::Arc;

        let (writer, _dir) = temp_writer(WalWriterConfig::default()).await;
        let writer = Arc::new(writer);

        // Append some records first
        (0..5).for_each(|_| {});
        futures::future::try_join_all((0..5).map(|i| {
            let w = Arc::clone(&writer);
            async move {
                w.append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(i),
                })
                .await
            }
        }))
        .await
        .expect("appends");

        // Now spawn many concurrent syncs - they should all complete
        let handles: Vec<_> = (0..20)
            .map(|_| {
                let w = Arc::clone(&writer);
                tokio::spawn(async move { w.sync().await })
            })
            .collect();

        let results: Vec<Result<()>> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.expect("join"))
            .collect();

        // All syncs should succeed
        assert!(results.iter().all(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn checkpoint_writes_record_and_rotates() {
        let (writer, dir) = temp_writer(WalWriterConfig::default()).await;

        // Write some records
        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append"); // seq 0
        writer
            .append(&WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append"); // seq 1

        // Checkpoint at seq 1 (covering the commit)
        let cp_seq = writer
            .checkpoint(WalSequence::from(1))
            .await
            .expect("checkpoint");
        assert_eq!(cp_seq, WalSequence::from(2)); // Checkpoint record is seq 2

        // Verify rotation happened (archived file should exist)
        let archives: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains('-'))
            .collect();
        assert_eq!(archives.len(), 1);
    }

    #[tokio::test]
    async fn checkpoint_deletes_old_archives() {
        let (writer, dir) = temp_writer(WalWriterConfig::default()).await;

        // Create first batch of records and rotate
        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append"); // seq 0
        writer
            .append(&WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append"); // seq 1
        writer.rotate().await.expect("rotate");
        // Archive 1: seqs 0-1

        // Create second batch and rotate
        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(2),
            })
            .await
            .expect("append"); // seq 2
        writer
            .append(&WalRecord::TxnCommit {
                txn_id: TransactionId::from(2),
            })
            .await
            .expect("append"); // seq 3
        writer.rotate().await.expect("rotate");
        // Archive 2: seqs 2-3

        // Create third batch
        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(3),
            })
            .await
            .expect("append"); // seq 4
        writer
            .append(&WalRecord::TxnCommit {
                txn_id: TransactionId::from(3),
            })
            .await
            .expect("append"); // seq 5

        // Should have 2 archives before checkpoint
        let archives_before: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains('-'))
            .collect();
        assert_eq!(archives_before.len(), 2);

        // Checkpoint at seq 3 (should delete first archive with seqs 0-1)
        writer
            .checkpoint(WalSequence::from(3))
            .await
            .expect("checkpoint");

        // Now: archive with seqs 0-1 should be deleted
        //      archive with seqs 2-3 should be deleted (last_seq 3 <= 3)
        //      archive with checkpoint record should exist
        let archives_after: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains('-'))
            .collect();
        // Only the newly rotated archive (from checkpoint) should remain
        assert_eq!(archives_after.len(), 1);

        // Verify the remaining archive contains the checkpoint
        let remaining = archives_after.first().unwrap();
        let name = remaining.file_name().to_string_lossy().to_string();
        // The checkpoint record is at seq 6, so archive should end there
        assert!(
            name.contains("-0000000000000006"),
            "Expected archive to end at seq 6, got: {name}"
        );
    }

    #[tokio::test]
    async fn checkpoint_keeps_newer_archives() {
        let (writer, dir) = temp_writer(WalWriterConfig::default()).await;

        // Create records and rotate multiple times
        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            })
            .await
            .expect("append"); // seq 0
        writer.rotate().await.expect("rotate");
        // Archive 1: seq 0

        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(2),
            })
            .await
            .expect("append"); // seq 1
        writer.rotate().await.expect("rotate");
        // Archive 2: seq 1

        writer
            .append(&WalRecord::TxnBegin {
                txn_id: TransactionId::from(3),
            })
            .await
            .expect("append"); // seq 2

        // Checkpoint at seq 0 (only delete first archive)
        writer
            .checkpoint(WalSequence::ZERO)
            .await
            .expect("checkpoint");

        // Count archives (should be 2: seq 1 archive + checkpoint archive)
        let archives: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains('-'))
            .collect();
        assert_eq!(archives.len(), 2);
    }

    #[tokio::test]
    async fn checkpoint_recovery_integration() {
        use crate::wal::{recover_from_dir, WalOp};

        let dir = TempDir::new().expect("temp dir");

        // First session: write and checkpoint
        {
            let writer =
                open_writer(dir.path(), WalWriterConfig::default()).await;

            // Transaction 1
            writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append"); // seq 0
            writer
                .append(&WalRecord::Set {
                    txn_id: TransactionId::from(1),
                    name: global!("OLD"),
                    key: key![1],
                    old: None,
                    new: NodeData::new(Some(1i64.into()), false),
                })
                .await
                .expect("append"); // seq 1
            writer
                .append(&WalRecord::TxnCommit {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append"); // seq 2

            // Checkpoint at seq 2
            writer
                .checkpoint(WalSequence::from(2))
                .await
                .expect("checkpoint"); // seq 3

            // Transaction 2 (after checkpoint)
            writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(2),
                })
                .await
                .expect("append"); // seq 4
            writer
                .append(&WalRecord::Set {
                    txn_id: TransactionId::from(2),
                    name: global!("NEW"),
                    key: key![2],
                    old: None,
                    new: NodeData::new(Some(2i64.into()), false),
                })
                .await
                .expect("append"); // seq 5
            writer
                .append(&WalRecord::TxnCommit {
                    txn_id: TransactionId::from(2),
                })
                .await
                .expect("append"); // seq 6

            writer.sync().await.expect("sync");
        }

        // Multi-file recovery reads both the archived file (containing the
        // checkpoint and txn1) and the active wal.log (containing txn2).
        let (result, _) = recover_from_dir(dir.path()).await.expect("recover");

        // Should see txn2's operation (txn1 is filtered by the checkpoint)
        assert_eq!(result.committed_ops.len(), 1);

        let op = result.committed_ops.first().unwrap();
        assert_eq!(op.txn_id, TransactionId::from(2));
        assert!(
            matches!(op.op, WalOp::Set { ref name, .. } if name == &global!("NEW"))
        );

        // Checkpoint is found in the archived file
        assert_eq!(result.last_checkpoint_seq, Some(WalSequence::from(2)));
    }

    #[tokio::test]
    async fn checkpoint_in_same_file() {
        // Test checkpoint record visible when not followed by rotation
        use crate::wal::recover_from_dir;

        let dir = TempDir::new().expect("temp dir");

        {
            let writer =
                open_writer(dir.path(), WalWriterConfig::default()).await;

            // Transaction 1 (before checkpoint seq)
            writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append"); // seq 0
            writer
                .append(&WalRecord::TxnCommit {
                    txn_id: TransactionId::from(1),
                })
                .await
                .expect("append"); // seq 1

            // Manually write checkpoint without using the checkpoint() method
            // (which rotates). This tests the recovery checkpoint filtering.
            writer
                .append(&WalRecord::Checkpoint {
                    seq: WalSequence::from(1),
                })
                .await
                .expect("append"); // seq 2

            // Transaction 2 (after checkpoint seq)
            writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(2),
                })
                .await
                .expect("append"); // seq 3
            writer
                .append(&WalRecord::TxnCommit {
                    txn_id: TransactionId::from(2),
                })
                .await
                .expect("append"); // seq 4

            writer.sync().await.expect("sync");
        }

        let (result, _) = recover_from_dir(dir.path()).await.expect("recover");

        // Checkpoint seq=1 filters out txn1's ops (but txn1 had no Set ops)
        // Both txns have no Set ops, so committed_ops should be empty
        assert!(result.committed_ops.is_empty());
        assert_eq!(result.last_checkpoint_seq, Some(WalSequence::from(1)));
    }
}
