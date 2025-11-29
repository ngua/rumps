//! WAL reader for sequential record iteration.
//!
//! The `WalReader` reads records from a WAL file, verifying checksums
//! and deserializing payloads. It provides an async stream interface
//! for efficient iteration.
//!
//! # Thread Safety
//!
//! `WalReader` owns its file handle. For concurrent access, use separate
//! readers or coordinate externally.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::BoxFuture;
use futures::Stream;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

use super::format::{try_read_record_at, FileHeader, FILE_HEADER_SIZE};
use super::WalRecord;
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
/// # Example
///
/// ```ignore
/// use rumps_storage::wal::WalReader;
/// use futures::StreamExt;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let reader = WalReader::open(Path::new("./data/wal.log")).await?;
///
///     // Convert to stream and iterate
///     let mut stream = reader.into_stream();
///     while let Some(entry) = stream.next().await {
///         let entry = entry?;
///         println!("seq={}, record={:?}", entry.seq, entry.record);
///     }
///     Ok(())
/// }
/// ```
pub(crate) struct WalReader {
    /// File handle.
    file: File,
    /// File size in bytes (cached at open).
    file_size: u64,
    /// Current read position.
    pos: u64,
    /// First sequence number in this file.
    first_seq: u64,
}

impl WalReader {
    /// Open an existing WAL file for reading.
    ///
    /// # Errors
    ///
    /// Returns `Err` if:
    /// - The file doesn't exist or can't be opened
    /// - The file header is invalid or corrupted
    pub(crate) async fn open(path: &Path) -> Result<Self> {
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
                file,
                file_size,
                pos: FILE_HEADER_SIZE as u64,
                first_seq: hdr.first_seq,
            })
        }
    }

    /// Get the first sequence number in this WAL file.
    pub(crate) fn first_seq(&self) -> u64 {
        self.first_seq
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

                Ok(Some(WalEntry {
                    seq: raw.header.seq,
                    record,
                }))
            }
        }
    }

    /// Convert this reader into an async stream of entries.
    ///
    /// The stream yields `Result<WalEntry>` items until EOF or error.
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
    use crate::wal::{WalWriter, WalWriterConfig};

    async fn write_records(dir: &TempDir, recs: &[WalRecord]) {
        let writer = WalWriter::open(dir.path(), WalWriterConfig::default())
            .await
            .expect("open writer");
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
    async fn reads_empty_wal() {
        let dir = TempDir::new().expect("temp dir");
        let writer = WalWriter::open(dir.path(), WalWriterConfig::default())
            .await
            .expect("open writer");
        writer.sync().await.expect("sync");
        drop(writer);

        let mut reader = WalReader::open(&dir.path().join("wal.log"))
            .await
            .expect("open reader");

        assert_eq!(reader.first_seq(), 0);
        assert!(reader.next().await.expect("next").is_none());
    }

    #[tokio::test]
    async fn reads_single_record() {
        let dir = TempDir::new().expect("temp dir");
        let rec = WalRecord::TxnBegin {
            txn_id: TransactionId::from(42),
        };
        write_records(&dir, &[rec.clone()]).await;

        let mut reader = WalReader::open(&dir.path().join("wal.log"))
            .await
            .expect("open reader");

        let entry = reader.next().await.expect("next").expect("entry");
        assert_eq!(entry.seq, 0);
        assert_eq!(entry.record, rec);

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

        let mut reader = WalReader::open(&dir.path().join("wal.log"))
            .await
            .expect("open reader");

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

        assert!(reader.next().await.expect("next").is_none());
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

        let reader = WalReader::open(&dir.path().join("wal.log"))
            .await
            .expect("open reader");
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

        let mut reader = WalReader::open(&dir.path().join("wal.log"))
            .await
            .expect("open reader");

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

        let mut reader = WalReader::open(&dir.path().join("wal.log"))
            .await
            .expect("open reader");

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
        let path = dir.path().join("bad.log");

        // Write garbage
        tokio::fs::write(&path, b"not a wal file")
            .await
            .expect("write");

        let result = WalReader::open(&path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_truncated_header() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("truncated.log");

        // Write partial header
        tokio::fs::write(&path, b"RWAL").await.expect("write");

        let result = WalReader::open(&path).await;
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

        let mut reader = WalReader::open(&path).await.expect("open reader");

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

        let mut reader = WalReader::open(&path).await.expect("open reader");

        let result = reader.next().await;
        assert!(matches!(result, Err(StorageError::WalCorruption { .. })));
    }
}
