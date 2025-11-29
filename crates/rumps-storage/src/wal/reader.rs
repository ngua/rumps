//! WAL reader for sequential record iteration and recovery.
//!
//! # Architecture: Reader → Writer Lifecycle
//!
//! The WAL system enforces a single path for initialization:
//!
//! ```text
//! WalReader::open(dir)  →  iterate for recovery  →  reader.into_writer(cfg)
//! ```
//!
//! This design ensures:
//!
//! 1. **Single source of truth**: The reader is the authority on file state.
//!    There's no separate "open for writing" path that might disagree.
//!
//! 2. **No double-scanning**: The reader tracks position and sequence numbers
//!    as it iterates. Converting to a writer reuses this state.
//!
//! 3. **Forced acknowledgment**: Callers must explicitly handle existing WAL
//!    records (even if just iterating to EOF) before writing new ones.
//!
//! 4. **Clear lifecycle**: Read phase (recovery) → Write phase (runtime).
//!    No ambiguity about which operations are valid when.
//!
//! # Usage
//!
//! ```ignore
//! // Open WAL (creates if missing)
//! let mut reader = WalReader::open(dir).await?;
//!
//! // Recovery: process existing records
//! while let Some(entry) = reader.next().await? {
//!     recover_record(entry)?;
//! }
//!
//! // Convert to writer for runtime use
//! let writer = reader.into_writer(WalWriterConfig::default()).await?;
//! ```
//!
//! For a fresh database with no existing WAL, `reader.next()` immediately
//! returns `None`, and `into_writer` creates the writer at sequence 0.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::BoxFuture;
use futures::Stream;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use super::format::{try_read_record_at, FileHeader, FILE_HEADER_SIZE};
use super::{WalRecord, WalWriter, WalWriterConfig};
use crate::error::{Result, StorageError};

/// A parsed WAL record with its metadata.
#[derive(Debug, Clone)]
pub(crate) struct WalEntry {
    /// The sequence number of this record.
    pub(crate) seq: u64,
    /// The deserialized record.
    pub(crate) record: WalRecord,
}

/// WAL reader that iterates over records sequentially.
///
/// This is the **only** way to initialize WAL access. After iterating
/// through existing records (for recovery), call [`into_writer`] to
/// convert to a [`WalWriter`] for appending new records.
///
/// [`into_writer`]: WalReader::into_writer
pub(crate) struct WalReader {
    /// Directory containing the WAL file.
    pub(super) dir: PathBuf,
    /// Path to the WAL file.
    pub(super) path: PathBuf,
    /// File handle (read-only).
    pub(super) file: File,
    /// File size in bytes (cached at open).
    pub(super) file_size: u64,
    /// Current read position.
    pos: u64,
    /// First sequence number in this file.
    pub(super) first_seq: u64,
    /// Next sequence number (updated as we read).
    pub(super) next_seq: u64,
}

impl WalReader {
    /// Open a WAL directory for reading, creating the file if it doesn't exist.
    ///
    /// This is the single entry point for WAL initialization. After reading
    /// existing records, call [`into_writer`] to begin writing.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory containing (or to contain) the WAL file
    ///
    /// # Errors
    ///
    /// Returns `Err` if:
    /// - The directory cannot be created
    /// - The file exists but has an invalid header
    ///
    /// [`into_writer`]: WalReader::into_writer
    pub(crate) async fn open(dir: &Path) -> Result<Self> {
        // Ensure directory exists
        fs::create_dir_all(dir).await?;

        let path = dir.join("wal.log");
        let exists = fs::try_exists(&path).await.unwrap_or(false);

        if exists {
            Self::open_existing(dir, &path).await
        } else {
            Self::create_new(dir, &path).await
        }
    }

    /// Open an existing WAL file.
    async fn open_existing(dir: &Path, path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).open(path).await?;

        let file_size = file.metadata().await?.len();

        // Validate file has at least a header
        if file_size < FILE_HEADER_SIZE as u64 {
            Err(StorageError::InvalidOperation(
                "WAL file too small for header".into(),
            ))
        } else {
            // Read and validate file header
            let mut hdr_buf = [0u8; FILE_HEADER_SIZE];
            file.read_exact(&mut hdr_buf).await?;

            let hdr = FileHeader::from_bytes(&hdr_buf).ok_or_else(|| {
                StorageError::InvalidOperation("Invalid WAL file header".into())
            })?;

            Ok(Self {
                dir: dir.to_path_buf(),
                path: path.to_path_buf(),
                file,
                file_size,
                pos: FILE_HEADER_SIZE as u64,
                first_seq: hdr.first_seq,
                next_seq: hdr.first_seq,
            })
        }
    }

    /// Create a new WAL file.
    async fn create_new(dir: &Path, path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await?;

        // Write file header
        let hdr = FileHeader::new(0);
        file.write_all(&hdr.to_bytes()).await?;
        file.sync_all().await?;

        // Seek back to after header for reading
        file.seek(SeekFrom::Start(FILE_HEADER_SIZE as u64)).await?;

        Ok(Self {
            dir: dir.to_path_buf(),
            path: path.to_path_buf(),
            file,
            file_size: FILE_HEADER_SIZE as u64,
            pos: FILE_HEADER_SIZE as u64,
            first_seq: 0,
            next_seq: 0,
        })
    }

    /// Get the first sequence number in this WAL file.
    pub(crate) fn first_seq(&self) -> u64 {
        self.first_seq
    }

    /// Get the next sequence number (one past the last record read).
    ///
    /// This is updated as records are read via [`next`].
    ///
    /// [`next`]: WalReader::next
    pub(crate) fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Get the file size in bytes.
    pub(crate) fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Read the next record from the WAL.
    ///
    /// Returns:
    /// - `Ok(Some(entry))` on success
    /// - `Ok(None)` at EOF (no more complete records)
    /// - `Err(WalCorruption)` on checksum mismatch
    /// - `Err(...)` on I/O or deserialization error
    pub(crate) async fn next(&mut self) -> Result<Option<WalEntry>> {
        match try_read_record_at(&mut self.file, self.pos, self.file_size)
            .await?
        {
            None => Ok(None),
            Some(raw) => {
                let record: WalRecord = bincode::deserialize(&raw.payload)
                    .map_err(|e| {
                        StorageError::Serialization(format!(
                            "WAL record at seq {}: {e}",
                            raw.header.seq
                        ))
                    })?;

                self.pos = raw.end_pos;
                self.next_seq = raw.header.seq + 1;

                Ok(Some(WalEntry {
                    seq: raw.header.seq,
                    record,
                }))
            }
        }
    }

    /// Convert this reader into a writer for appending new records.
    ///
    /// This consumes the reader and returns a [`WalWriter`] positioned
    /// at the end of valid data, ready to append new records.
    ///
    /// # Important
    ///
    /// You should iterate through all existing records before calling
    /// this method. The writer's sequence numbers continue from the
    /// last record read (or 0 for an empty/new WAL).
    ///
    /// # Errors
    ///
    /// Returns `Err` if the file cannot be reopened for writing.
    pub(crate) async fn into_writer(
        self,
        cfg: WalWriterConfig,
    ) -> Result<WalWriter> {
        WalWriter::from_reader(self, cfg).await
    }

    /// Convert this reader into an async stream of entries.
    ///
    /// The stream yields `Result<WalEntry>` items until EOF or error.
    ///
    /// **Note**: After using the stream, you cannot call `into_writer`
    /// because the stream consumes ownership. If you need to convert
    /// to a writer, use the `next()` method directly instead.
    pub(crate) fn into_stream(self) -> WalRecordStream {
        WalRecordStream {
            reader: Some(self),
            pending: None,
        }
    }

    /// Seek to a specific position in the file.
    ///
    /// This is useful for resuming from a known offset. The position
    /// should be the start of a record (after the file header).
    pub(crate) async fn seek(&mut self, pos: u64) -> Result<()> {
        self.pos = pos;
        self.file.seek(SeekFrom::Start(pos)).await?;
        Ok(())
    }

    /// Get the current read position.
    pub(crate) fn position(&self) -> u64 {
        self.pos
    }
}

/// Async stream over WAL records.
///
/// Created by [`WalReader::into_stream`].
pub(crate) struct WalRecordStream {
    reader: Option<WalReader>,
    pending: Option<BoxFuture<'static, (WalReader, Result<Option<WalEntry>>)>>,
}

impl Stream for WalRecordStream {
    type Item = Result<WalEntry>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        // If we have a pending future, poll it
        if let Some(fut) = self.pending.as_mut() {
            match Pin::new(fut).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready((reader, result)) => {
                    self.pending = None;
                    self.reader = Some(reader);
                    match result {
                        Ok(Some(entry)) => Poll::Ready(Some(Ok(entry))),
                        Ok(None) => Poll::Ready(None), // EOF
                        Err(e) => Poll::Ready(Some(Err(e))),
                    }
                }
            }
        } else if let Some(mut reader) = self.reader.take() {
            // Start a new read
            let fut = Box::pin(async move {
                let result = reader.next().await;
                (reader, result)
            });
            self.pending = Some(fut);
            // Poll immediately
            Pin::new(&mut *self).poll_next(cx)
        } else {
            // No reader, stream exhausted
            Poll::Ready(None)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use futures::StreamExt;
    use rumps_types::{global, key};
    use tempfile::TempDir;

    use super::*;
    use crate::node::NodeData;
    use crate::transaction::TransactionId;

    /// Helper to create a reader and immediately convert to writer for setup.
    async fn setup_writer(dir: &TempDir) -> WalWriter {
        WalReader::open(dir.path())
            .await
            .expect("open")
            .into_writer(WalWriterConfig::default())
            .await
            .expect("into_writer")
    }

    /// Helper to write records using reader→writer flow.
    async fn write_records(dir: &TempDir, recs: &[WalRecord]) {
        let writer = setup_writer(dir).await;
        recs.iter()
            .try_for_each(|rec| {
                futures::executor::block_on(async {
                    writer.append(rec).await.map(|_| ())
                })
            })
            .expect("append all");
        writer.sync().await.expect("sync");
    }

    #[tokio::test]
    async fn creates_wal_on_open() {
        let dir = TempDir::new().expect("temp dir");
        let reader = WalReader::open(dir.path()).await.expect("open");

        let wal_path = dir.path().join("wal.log");
        assert!(wal_path.exists());
        assert_eq!(reader.first_seq(), 0);
        assert_eq!(reader.next_seq(), 0);
    }

    #[tokio::test]
    async fn empty_wal_returns_none() {
        let dir = TempDir::new().expect("temp dir");
        let mut reader = WalReader::open(dir.path()).await.expect("open");

        assert!(reader.next().await.expect("next").is_none());
        assert_eq!(reader.next_seq(), 0);
    }

    #[tokio::test]
    async fn reads_single_record() {
        let dir = TempDir::new().expect("temp dir");
        let rec = WalRecord::TxnBegin {
            txn_id: TransactionId::from(42),
        };
        write_records(&dir, &[rec.clone()]).await;

        let mut reader = WalReader::open(dir.path()).await.expect("open");

        let entry = reader.next().await.expect("next").expect("entry");
        assert_eq!(entry.seq, 0);
        assert_eq!(entry.record, rec);
        assert_eq!(reader.next_seq(), 1);

        assert!(reader.next().await.expect("next").is_none());
    }

    #[tokio::test]
    async fn reads_multiple_records() {
        let dir = TempDir::new().expect("temp dir");
        let recs = vec![
            WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            },
            WalRecord::Set {
                txn_id: TransactionId::from(1),
                name: global!("TEST"),
                key: key!["abc"],
                old: None,
                new: NodeData::new(Some("value".into()), false),
            },
            WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            },
        ];
        write_records(&dir, &recs).await;

        let mut reader = WalReader::open(dir.path()).await.expect("open");

        recs.iter()
            .enumerate()
            .try_for_each(|(i, expected)| {
                futures::executor::block_on(async {
                    let entry = reader.next().await?.expect("entry");
                    assert_eq!(entry.seq, i as u64);
                    assert_eq!(&entry.record, expected);
                    Ok::<_, StorageError>(())
                })
            })
            .expect("read all");

        assert_eq!(reader.next_seq(), 3);
        assert!(reader.next().await.expect("next").is_none());
    }

    #[tokio::test]
    async fn into_writer_continues_seq() {
        let dir = TempDir::new().expect("temp dir");

        // Write initial records
        {
            let writer = setup_writer(&dir).await;
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

        // Reopen, iterate, then convert to writer
        {
            let mut reader = WalReader::open(dir.path()).await.expect("open");

            // Read all records
            while reader.next().await.expect("next").is_some() {}
            assert_eq!(reader.next_seq(), 2);

            // Convert to writer and append more
            let writer = reader
                .into_writer(WalWriterConfig::default())
                .await
                .expect("into_writer");

            let seq = writer
                .append(&WalRecord::TxnBegin {
                    txn_id: TransactionId::from(2),
                })
                .await
                .expect("append");
            assert_eq!(seq, 2);
            assert_eq!(writer.next_seq().await, 3);
        }
    }

    #[tokio::test]
    async fn stream_interface() {
        let dir = TempDir::new().expect("temp dir");
        let recs = vec![
            WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            },
            WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            },
        ];
        write_records(&dir, &recs).await;

        let reader = WalReader::open(dir.path()).await.expect("open");
        let mut stream = reader.into_stream();

        let entries: Vec<_> = futures::executor::block_on(async {
            let mut v = vec![];
            while let Some(entry) = stream.next().await {
                v.push(entry.expect("entry"));
            }
            v
        });

        assert_eq!(entries.len(), 2);
        assert_eq!(entries.get(0).unwrap().seq, 0);
        assert_eq!(entries.get(1).unwrap().seq, 1);
    }

    #[tokio::test]
    async fn all_record_types() {
        let dir = TempDir::new().expect("temp dir");
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
            WalRecord::TxnAbort {
                txn_id: TransactionId::from(2),
            },
            WalRecord::Checkpoint { seq: 100 },
        ];
        write_records(&dir, &recs).await;

        let mut reader = WalReader::open(dir.path()).await.expect("open");

        recs.iter()
            .try_for_each(|expected| {
                futures::executor::block_on(async {
                    let entry = reader.next().await?.expect("entry");
                    assert_eq!(&entry.record, expected);
                    Ok::<_, StorageError>(())
                })
            })
            .expect("read all");
    }

    #[tokio::test]
    async fn seek_and_resume() {
        let dir = TempDir::new().expect("temp dir");
        let recs = vec![
            WalRecord::TxnBegin {
                txn_id: TransactionId::from(1),
            },
            WalRecord::TxnCommit {
                txn_id: TransactionId::from(1),
            },
            WalRecord::TxnBegin {
                txn_id: TransactionId::from(2),
            },
        ];
        write_records(&dir, &recs).await;

        let mut reader = WalReader::open(dir.path()).await.expect("open");

        // Read first record and save position
        let _ = reader.next().await.expect("next").expect("entry");
        let pos_after_first = reader.position();

        // Read remaining
        let _ = reader.next().await.expect("next").expect("entry");
        let _ = reader.next().await.expect("next").expect("entry");
        assert!(reader.next().await.expect("next").is_none());

        // Seek back and re-read
        reader.seek(pos_after_first).await.expect("seek");
        let entry = reader.next().await.expect("next").expect("entry");
        assert_eq!(entry.seq, 1);
    }

    #[tokio::test]
    async fn rejects_invalid_file() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("wal.log");

        // Write garbage
        tokio::fs::write(&path, b"not a wal file")
            .await
            .expect("write");

        let result = WalReader::open(dir.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_truncated_header() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("wal.log");

        // Write partial header
        tokio::fs::write(&path, b"RWAL").await.expect("write");

        let result = WalReader::open(dir.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn handles_partial_record_at_eof() {
        let dir = TempDir::new().expect("temp dir");

        // Write valid WAL then append garbage
        let rec = WalRecord::TxnBegin {
            txn_id: TransactionId::from(1),
        };
        write_records(&dir, &[rec.clone()]).await;

        // Append partial record header (incomplete)
        let path = dir.path().join("wal.log");
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open");
        tokio::io::AsyncWriteExt::write_all(&mut file, &[0u8; 10])
            .await
            .expect("write garbage");
        drop(file);

        let mut reader = WalReader::open(dir.path()).await.expect("open");

        // First record should be readable
        let entry = reader.next().await.expect("next").expect("entry");
        assert_eq!(entry.record, rec);

        // Second "record" is incomplete, should return None
        assert!(reader.next().await.expect("next").is_none());
    }

    #[tokio::test]
    async fn detects_checksum_corruption() {
        let dir = TempDir::new().expect("temp dir");
        let rec = WalRecord::TxnBegin {
            txn_id: TransactionId::from(1),
        };
        write_records(&dir, &[rec]).await;

        // Corrupt the payload
        let path = dir.path().join("wal.log");
        let mut data = tokio::fs::read(&path).await.expect("read");

        // Corrupt last byte of payload (after header + record header)
        let corrupt_idx = data.len() - 1;
        data[corrupt_idx] ^= 0xFF;
        tokio::fs::write(&path, &data).await.expect("write");

        let mut reader = WalReader::open(dir.path()).await.expect("open");

        let result = reader.next().await;
        assert!(matches!(result, Err(StorageError::WalCorruption { .. })));
    }
}
