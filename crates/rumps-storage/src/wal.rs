//! Write-Ahead Log (WAL) for crash recovery and durability.
//!
//! The WAL ensures durability by logging all modifications before they are
//! applied to the main data file. On crash recovery, uncommitted transactions
//! can be rolled back and committed transactions can be replayed.
//!
//! # Components
//!
//! - [`WalRecord`]: The different record types that can be written
//! - [`WalWriter`]: Appends records to WAL files with configurable sync
//! - [`WalReader`]: Reads records sequentially with checksum verification
//! - [`SyncMode`]: When to sync writes to disk
//!
//! # Design: Incremental Kill Records
//!
//! When a KILL operation removes a subtree (e.g., `KILL ^PATIENT(123)` which
//! has children `NAME`, `DOB`, `ADDR`), we emit one `KillEntry` record per
//! deleted key rather than storing the entire subtree in a single record.
//!
//! This keeps each WAL record bounded in size, at the cost of a longer WAL
//! for large subtree deletions.
//!
//! ## Alternative approaches (not implemented):
//!
//! 1. **Inline subtree**: Store `Vec<(Key, NodeData)>` in a single record.
//!    Simpler but unbounded record size for large subtrees.
//!
//! 2. **Reference-based undo**: Store references to data file locations
//!    instead of actual data. Bounded size but complicates recovery (must
//!    read both WAL and data file).
//!
//! 3. **Hybrid**: Small subtrees inline, large ones split or use references.
//!    More complex, may be worth revisiting if performance requires it.

mod files;
mod format;
mod reader;
mod recovery;
mod writer;

use std::fmt::{self, Display};
use std::ops::Deref;

pub(crate) use reader::WalReader;
// Used by tests in submodules
#[allow(unused_imports)]
pub(crate) use recovery::{recover_from_dir, WalOp};
use rumps_types::{Key, Name};
use serde::{Deserialize, Serialize};
pub(crate) use writer::{WalWriter, WalWriterConfig};

use crate::node::NodeData;
use crate::transaction::TransactionId;

/// A WAL sequence number - monotonically increasing across all WAL files.
///
/// Sequence numbers uniquely identify each record in the WAL and are used to:
/// - Order records for replay during recovery
/// - Detect gaps in the WAL (indicating corruption or missing files)
/// - Determine which archived files can be safely deleted after checkpointing
///
/// The sequence is formatted as 16-digit hex for consistency with archive filenames.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize
)]
#[repr(transparent)]
pub(crate) struct WalSequence(u64);

impl WalSequence {
    /// The zero sequence number (start of a fresh WAL).
    pub const ZERO: Self = Self(0);

    /// Get the next sequence number.
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// Saturating subtraction - returns `ZERO` if result would underflow.
    pub fn saturating_sub(self, n: u64) -> Self {
        Self(self.0.saturating_sub(n))
    }
}

impl Deref for WalSequence {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<u64> for WalSequence {
    fn from(n: u64) -> Self {
        Self(n)
    }
}

impl From<WalSequence> for u64 {
    fn from(seq: WalSequence) -> Self {
        seq.0
    }
}

impl Display for WalSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// A record in the Write-Ahead Log.
///
/// Each record represents either a transaction lifecycle event (begin, commit,
/// abort) or a single data modification operation (set, kill entry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum WalRecord {
    /// Transaction has started.
    TxnBegin {
        /// The transaction's unique identifier.
        txn_id: TransactionId,
    },

    /// Transaction committed successfully.
    ///
    /// All operations from this transaction should be made durable.
    TxnCommit {
        /// The transaction's unique identifier.
        txn_id: TransactionId,
    },

    /// Transaction was aborted.
    ///
    /// All operations from this transaction should be discarded.
    TxnAbort {
        /// The transaction's unique identifier.
        txn_id: TransactionId,
    },

    /// A SET operation within a transaction.
    Set {
        /// The transaction performing this operation.
        txn_id: TransactionId,
        /// The variable name (global only; locals aren't persisted).
        name: Name,
        /// The key path.
        key: Key,
        /// The previous value (for undo on abort).
        old: Option<NodeData>,
        /// The new value being set.
        new: NodeData,
    },

    /// A single entry deletion within a KILL operation.
    ///
    /// When killing a subtree, one `KillEntry` is emitted per deleted key.
    /// This keeps each record bounded in size. To undo a KILL, replay all
    /// `KillEntry` records for that transaction by re-inserting their data.
    KillEntry {
        /// The transaction performing this operation.
        txn_id: TransactionId,
        /// The variable name (global only; locals aren't persisted).
        name: Name,
        /// The key path of the deleted entry.
        key: Key,
        /// The deleted data (for undo on abort).
        data: NodeData,
    },

    /// A checkpoint marker.
    ///
    /// Indicates that all data up to this point has been flushed to the main
    /// data file. WAL entries before this checkpoint can be discarded.
    Checkpoint {
        /// Monotonically increasing checkpoint sequence number.
        seq: WalSequence,
    },
}

// NOTE on tests: We obviously never test `Name::Local`s here because they are
// ephemeral. I.e. we can't (intentionally) round-trip a local, so everything
// below is a global (which is what will really be written)
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use rumps_types::{global, key, Value};

    use super::*;

    #[test]
    fn txn_begin_roundtrip() {
        let rec = WalRecord::TxnBegin { txn_id: 42.into() };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn txn_commit_roundtrip() {
        let rec = WalRecord::TxnCommit { txn_id: 123.into() };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn txn_abort_roundtrip() {
        let rec = WalRecord::TxnAbort { txn_id: 999.into() };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn set_roundtrip() {
        let rec = WalRecord::Set {
            txn_id: 1.into(),
            name: global!("PATIENT"),
            key: key![123, "NAME"],
            old: None,
            new: NodeData::new(Some("John".into()), false),
        };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn set_with_old_value_roundtrip() {
        let rec = WalRecord::Set {
            txn_id: 2.into(),
            name: global!("PATIENT"),
            key: key![123, "NAME"],
            old: Some(NodeData::new(Some("Jane".into()), false)),
            new: NodeData::new(Some("John".into()), false),
        };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn kill_entry_roundtrip() {
        let rec = WalRecord::KillEntry {
            txn_id: 3.into(),
            name: global!("PATIENT"),
            key: key![123, "NAME"],
            data: NodeData::new(Some("John".into()), false),
        };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn checkpoint_roundtrip() {
        let rec = WalRecord::Checkpoint {
            seq: WalSequence::from(12345),
        };
        let bytes = bincode::serialize(&rec).expect("serialize");
        let decoded: WalRecord =
            bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(rec, decoded);
    }

    #[test]
    fn all_value_types_roundtrip() {
        let values: [Value; 4] =
            ["hello".into(), 42i64.into(), 3.14f64.into(), true.into()];

        values
            .into_iter()
            .enumerate()
            .try_for_each(|(i, val)| {
                let rec = WalRecord::Set {
                    txn_id: (i as u64).into(),
                    name: global!("TEST"),
                    key: key![i as i64],
                    old: None,
                    new: NodeData::new(Some(val), false),
                };
                let bytes = bincode::serialize(&rec)?;
                let decoded: WalRecord = bincode::deserialize(&bytes)?;
                assert_eq!(rec, decoded);
                Ok::<_, Box<bincode::ErrorKind>>(())
            })
            .expect("roundtrip");
    }

    #[test]
    fn encoding_sizes() {
        // Check bincode encoding overhead for each variant
        let txn_begin = WalRecord::TxnBegin { txn_id: 1.into() };
        let txn_commit = WalRecord::TxnCommit { txn_id: 1.into() };
        let checkpoint = WalRecord::Checkpoint {
            seq: WalSequence::from(1),
        };
        let set_minimal = WalRecord::Set {
            txn_id: 1.into(),
            name: global!("X"),
            key: key![1],
            old: None,
            new: NodeData::new(Some(1i64.into()), false),
        };
        let kill_minimal = WalRecord::KillEntry {
            txn_id: 1.into(),
            name: global!("X"),
            key: key![1],
            data: NodeData::new(Some(1i64.into()), false),
        };

        let sizes = [
            ("TxnBegin", bincode::serialize(&txn_begin).unwrap().len()),
            ("TxnCommit", bincode::serialize(&txn_commit).unwrap().len()),
            ("Checkpoint", bincode::serialize(&checkpoint).unwrap().len()),
            (
                "Set (minimal)",
                bincode::serialize(&set_minimal).unwrap().len(),
            ),
            (
                "KillEntry (minimal)",
                bincode::serialize(&kill_minimal).unwrap().len(),
            ),
        ];

        // Print sizes for inspection (visible with --nocapture)
        sizes.iter().for_each(|(name, size)| {
            eprintln!("{name}: {size} bytes");
        });

        // Sanity checks - these should be reasonably small
        assert!(sizes[0].1 <= 16, "TxnBegin too large: {}", sizes[0].1);
        assert!(sizes[1].1 <= 16, "TxnCommit too large: {}", sizes[1].1);
        assert!(sizes[2].1 <= 16, "Checkpoint too large: {}", sizes[2].1);
        // Set/KillEntry have more fields, but minimal versions should be bounded
        assert!(sizes[3].1 <= 64, "Set too large: {}", sizes[3].1);
        assert!(sizes[4].1 <= 64, "KillEntry too large: {}", sizes[4].1);
    }

    // WalSequence tests

    #[test]
    fn seq_zero_constant() {
        assert_eq!(*WalSequence::ZERO, 0);
    }

    #[test]
    fn seq_from_and_deref() {
        let seq: WalSequence = 42u64.into();
        assert_eq!(*seq, 42);
    }

    #[test]
    fn seq_next_increments() {
        let seq: WalSequence = 10u64.into();
        assert_eq!(*seq.next(), 11);
    }

    #[test]
    fn seq_saturating_sub() {
        let seq: WalSequence = 10u64.into();
        assert_eq!(*seq.saturating_sub(3), 7);
        assert_eq!(
            WalSequence::from(5u64).saturating_sub(10),
            WalSequence::ZERO
        );
    }

    #[test]
    fn seq_display_hex() {
        assert_eq!(format!("{}", WalSequence::ZERO), "0000000000000000");
        assert_eq!(
            format!("{}", WalSequence::from(255u64)),
            "00000000000000ff"
        );
        assert_eq!(
            format!("{}", WalSequence::from(u64::MAX)),
            "ffffffffffffffff"
        );
    }

    #[test]
    fn seq_ordering() {
        let a: WalSequence = 1u64.into();
        let b: WalSequence = 2u64.into();
        assert!(a < b);
        assert!(b > a);
        assert_eq!(b, 2u64.into());
    }
}

/// Benchmark suite for WAL operations.
#[cfg(feature = "bench")]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod benches {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
    use futures::stream::StreamExt;
    use rumps_types::{global, key};
    use tempfile::TempDir;
    use tokio::runtime::Runtime;

    use super::*;
    use crate::node::NodeData;

    /// Create a SET record with a payload of approximately `size` bytes.
    fn make_set_record(txn_id: u64, size: usize) -> WalRecord {
        let val = "x".repeat(size);
        WalRecord::Set {
            txn_id: txn_id.into(),
            name: global!("BENCH"),
            key: key![txn_id as i64],
            old: None,
            new: NodeData::new(Some(val.into()), false),
        }
    }

    /// Benchmark steady-state append throughput (no fsync).
    ///
    /// Measures raw append performance with setup amortized out.
    fn bench_append_throughput(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();
        let mut group = c.benchmark_group("wal_append");
        group.throughput(Throughput::Elements(1));

        // Small record (TxnBegin ~12 bytes)
        group.bench_function("small", |b| {
            let dir = TempDir::new().unwrap();
            let writer = rt.block_on(async {
                let reader = WalReader::open(dir.path()).await.unwrap();
                reader
                    .into_writer(WalWriterConfig {
                        sync_mode: writer::SyncMode::OnCommit,
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            });
            let counter = AtomicU64::new(0);

            b.iter(|| {
                let txn_id = counter.fetch_add(1, Ordering::Relaxed);
                rt.block_on(async {
                    let rec = WalRecord::TxnBegin {
                        txn_id: txn_id.into(),
                    };
                    writer.append(&rec).await.unwrap();
                })
            });
        });

        // Medium record (~100 bytes payload)
        group.bench_function("medium_100B", |b| {
            let dir = TempDir::new().unwrap();
            let writer = rt.block_on(async {
                let reader = WalReader::open(dir.path()).await.unwrap();
                reader
                    .into_writer(WalWriterConfig {
                        sync_mode: writer::SyncMode::OnCommit,
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            });
            let counter = AtomicU64::new(0);

            b.iter(|| {
                let txn_id = counter.fetch_add(1, Ordering::Relaxed);
                rt.block_on(async {
                    let rec = make_set_record(txn_id, 100);
                    writer.append(&rec).await.unwrap();
                })
            });
        });

        // Large record (~1KB payload)
        group.bench_function("large_1KB", |b| {
            let dir = TempDir::new().unwrap();
            let writer = rt.block_on(async {
                let reader = WalReader::open(dir.path()).await.unwrap();
                reader
                    .into_writer(WalWriterConfig {
                        sync_mode: writer::SyncMode::OnCommit,
                        ..Default::default()
                    })
                    .await
                    .unwrap()
            });
            let counter = AtomicU64::new(0);

            b.iter(|| {
                let txn_id = counter.fetch_add(1, Ordering::Relaxed);
                rt.block_on(async {
                    let rec = make_set_record(txn_id, 1024);
                    writer.append(&rec).await.unwrap();
                })
            });
        });

        group.finish();
    }

    /// Benchmark transaction commit with fsync.
    ///
    /// This is the realistic "durable write" scenario: append + sync.
    fn bench_commit_with_sync(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();
        let mut group = c.benchmark_group("wal_commit_sync");
        group.sample_size(50); // Fewer samples, fsync is slow
        group.measurement_time(Duration::from_secs(10));

        group.bench_function("single_op_txn", |b| {
            let dir = TempDir::new().unwrap();
            let writer = rt.block_on(async {
                let reader = WalReader::open(dir.path()).await.unwrap();
                reader
                    .into_writer(WalWriterConfig::default())
                    .await
                    .unwrap()
            });
            let counter = AtomicU64::new(0);

            b.iter(|| {
                let txn_id = counter.fetch_add(1, Ordering::Relaxed);
                rt.block_on(async {
                    writer
                        .append(&WalRecord::TxnBegin {
                            txn_id: txn_id.into(),
                        })
                        .await
                        .unwrap();
                    writer.append(&make_set_record(txn_id, 64)).await.unwrap();
                    writer
                        .append(&WalRecord::TxnCommit {
                            txn_id: txn_id.into(),
                        })
                        .await
                        .unwrap();
                    writer.sync().await.unwrap();
                })
            });
        });

        group.finish();
    }

    /// Benchmark batched transactions (amortize sync cost).
    ///
    /// Shows benefit of batching multiple operations before sync.
    fn bench_batched_commits(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();
        let mut group = c.benchmark_group("wal_batch_commit");
        group.sample_size(30);
        group.measurement_time(Duration::from_secs(10));

        [5, 10, 50].iter().copied().for_each(|ops_per_txn| {
            group.throughput(Throughput::Elements(ops_per_txn as u64));
            group.bench_with_input(
                BenchmarkId::new("ops", ops_per_txn),
                &ops_per_txn,
                |b, &ops| {
                    let dir = TempDir::new().unwrap();
                    let writer = rt.block_on(async {
                        let reader = WalReader::open(dir.path()).await.unwrap();
                        reader
                            .into_writer(WalWriterConfig::default())
                            .await
                            .unwrap()
                    });
                    let counter = AtomicU64::new(0);

                    b.iter(|| {
                        let txn_id = counter.fetch_add(1, Ordering::Relaxed);
                        rt.block_on(async {
                            writer
                                .append(&WalRecord::TxnBegin {
                                    txn_id: txn_id.into(),
                                })
                                .await
                                .unwrap();

                            // Sequential appends within the transaction
                            futures::stream::iter(0..ops as u64)
                                .for_each(|i| {
                                    let w = &writer;
                                    async move {
                                        let rec = make_set_record(
                                            txn_id * 1000 + i,
                                            64,
                                        );
                                        w.append(&rec).await.unwrap();
                                    }
                                })
                                .await;

                            writer
                                .append(&WalRecord::TxnCommit {
                                    txn_id: txn_id.into(),
                                })
                                .await
                                .unwrap();
                            writer.sync().await.unwrap();
                        })
                    });
                },
            );
        });

        group.finish();
    }

    /// Benchmark WAL recovery (reading all records).
    fn bench_recovery(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();
        let mut group = c.benchmark_group("wal_recovery");

        [100, 1000, 10000].iter().copied().for_each(|count| {
            group.throughput(Throughput::Elements(count as u64));
            group.bench_with_input(
                BenchmarkId::from_parameter(count),
                &count,
                |b, &count| {
                    // Setup: create WAL with records
                    let dir = TempDir::new().unwrap();
                    rt.block_on(async {
                        let reader = WalReader::open(dir.path()).await.unwrap();
                        let writer = reader
                            .into_writer(WalWriterConfig::default())
                            .await
                            .unwrap();

                        futures::stream::iter(0..count as u64)
                            .for_each(|i| {
                                let w = &writer;
                                async move {
                                    let rec = make_set_record(i, 64);
                                    w.append(&rec).await.unwrap();
                                }
                            })
                            .await;
                        writer.sync().await.unwrap();
                    });

                    // Benchmark: read all records
                    b.iter(|| {
                        rt.block_on(async {
                            let mut reader =
                                WalReader::open(dir.path()).await.unwrap();
                            let mut n = 0u64;
                            while reader.next().await.unwrap().is_some() {
                                n += 1;
                            }
                            n
                        })
                    });
                },
            );
        });

        group.finish();
    }

    /// Benchmark file rotation overhead.
    ///
    /// Uses small file size to force frequent rotations.
    fn bench_rotation(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();
        let mut group = c.benchmark_group("wal_rotation");
        group.throughput(Throughput::Elements(100));

        group.bench_function("100_records_small_files", |b| {
            b.iter_batched(
                || TempDir::new().unwrap(),
                |dir| {
                    rt.block_on(async {
                        let reader = WalReader::open(dir.path()).await.unwrap();
                        let writer = reader
                            .into_writer(WalWriterConfig {
                                max_file_size: 512, // Force rotation every ~4 records
                                sync_mode: writer::SyncMode::OnCommit,
                            })
                            .await
                            .unwrap();

                        futures::stream::iter(0..100u64)
                            .for_each(|i| {
                                let w = &writer;
                                async move {
                                    let rec = make_set_record(i, 64);
                                    w.append(&rec).await.unwrap();
                                }
                            })
                            .await;
                    })
                },
                BatchSize::SmallInput,
            );
        });

        group.finish();
    }

    /// Benchmark serialization overhead (bincode).
    fn bench_serialization(c: &mut Criterion) {
        let mut group = c.benchmark_group("wal_serialize");

        let small = WalRecord::TxnBegin { txn_id: 1.into() };
        let medium = make_set_record(1, 100);
        let large = make_set_record(1, 1024);

        group.bench_function("small", |b| {
            b.iter(|| bincode::serialize(&small).unwrap())
        });

        group.bench_function("medium_100B", |b| {
            b.iter(|| bincode::serialize(&medium).unwrap())
        });

        group.bench_function("large_1KB", |b| {
            b.iter(|| bincode::serialize(&large).unwrap())
        });

        group.finish();
    }

    /// Benchmark concurrent sync (group commit effectiveness).
    ///
    /// Spawns N tasks that each append a record and call sync().
    /// With group commit, these should complete faster than N sequential syncs.
    fn bench_concurrent_sync(c: &mut Criterion) {
        use std::sync::Arc;

        let rt = Runtime::new().unwrap();
        let mut group = c.benchmark_group("wal_concurrent_sync");
        group.sample_size(20);
        group.measurement_time(Duration::from_secs(15));

        [1, 4, 8, 16, 32].iter().copied().for_each(|concurrency| {
            group.throughput(Throughput::Elements(concurrency as u64));
            group.bench_with_input(
                BenchmarkId::new("tasks", concurrency),
                &concurrency,
                |b, &n| {
                    let dir = TempDir::new().unwrap();
                    let writer = Arc::new(rt.block_on(async {
                        let reader = WalReader::open(dir.path()).await.unwrap();
                        reader
                            .into_writer(WalWriterConfig::default())
                            .await
                            .unwrap()
                    }));
                    let counter = AtomicU64::new(0);

                    b.iter(|| {
                        rt.block_on(async {
                            // Spawn n concurrent tasks, each doing append + sync
                            let handles: Vec<_> = (0..n)
                                .map(|_| {
                                    let w = Arc::clone(&writer);
                                    let id =
                                        counter.fetch_add(1, Ordering::Relaxed);
                                    tokio::spawn(async move {
                                        w.append(&WalRecord::TxnBegin {
                                            txn_id: id.into(),
                                        })
                                        .await
                                        .unwrap();
                                        w.sync().await.unwrap();
                                    })
                                })
                                .collect();

                            futures::future::join_all(handles).await;
                        })
                    });
                },
            );
        });

        group.finish();
    }

    /// Main entry point for all WAL benchmarks.
    pub fn run_benchmarks(c: &mut Criterion) {
        bench_append_throughput(c);
        bench_commit_with_sync(c);
        bench_batched_commits(c);
        bench_recovery(c);
        bench_rotation(c);
        bench_serialization(c);
        bench_concurrent_sync(c);
    }
}
