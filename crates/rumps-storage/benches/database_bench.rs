//! Database-level benchmarks for RUMPS storage.
//!
//! These benchmarks measure the performance of the public `Database` API,
//! including transactions, reads, writes, and iteration.
//!
//! Benchmarks run against both in-memory and on-disk storage modes for
//! comparison. Filter with `cargo bench -- InMemory` or `OnDisk` to run
//! only one mode.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt;
use std::path::Path;

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, Criterion,
    Throughput,
};
use futures::{StreamExt, TryStreamExt};
use rumps_storage::{Database, SyncMode, Transaction};
use rumps_types::{global, key, value, Name, Value};
use tempfile::TempDir;
use tokio::runtime::Runtime;

/// Storage mode for benchmarks.
#[derive(Clone, Copy)]
enum Mode {
    InMemory,
    OnDisk,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::InMemory => write!(f, "InMemory"),
            Mode::OnDisk => write!(f, "OnDisk"),
        }
    }
}

const MODES: [Mode; 2] = [Mode::InMemory, Mode::OnDisk];

/// Creates a database for the given mode. Returns `(Option<TempDir>, Database)`.
/// The `TempDir` must be kept alive for on-disk DBs.
///
/// We use `TempDir::new_in(CARGO_MANIFEST_DIR)` instead of `TempDir::new()` to
/// ensure the temp directory is on a real filesystem, not a RAM disk. On many
/// systems, `/tmp` is a `tmpfs` mount, which would make I/O benchmarks
/// meaningless since there's no actual disk I/O.
fn create_db(mode: Mode, rt: &Runtime) -> (Option<TempDir>, Database) {
    match mode {
        Mode::InMemory => (None, Database::in_memory().unwrap()),
        Mode::OnDisk => {
            let dir =
                TempDir::new_in(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
            let db = rt.block_on(Database::create(dir.path())).unwrap();
            (Some(dir), db)
        }
    }
}

/// Helper to insert `n` sequential keys in a transaction.
async fn insert_keys(
    txn: &Transaction,
    name: &Name,
    n: i64,
) -> rumps_storage::Result<()> {
    futures::stream::iter(0..n)
        .then(|i| {
            let t = txn;
            let n = name;
            async move { t.set(n, &key![i], value!(i)).await }
        })
        .try_collect::<Vec<_>>()
        .await?;
    Ok(())
}

/// Benchmark transaction overhead (begin + commit with no operations).
fn bench_transaction_overhead(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("transaction_empty");

    MODES.iter().for_each(|&mode| {
        let (_dir, db) = create_db(mode, &rt);

        group.bench_function(format!("{mode}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    db.transaction(|_txn| async move { Ok(()) }).await.unwrap();
                })
            })
        });
    });

    group.finish();
}

/// Benchmark single `set` operation within a transaction.
fn bench_set_single(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("set_single");
    group.measurement_time(std::time::Duration::from_secs(15));

    MODES.iter().for_each(|&mode| {
        let (_dir, db) = create_db(mode, &rt);
        let name = global!("BENCH");
        let counter = std::sync::RwLock::new(0i64);

        group.bench_function(format!("{mode}"), |b| {
            b.iter(|| {
                let i = {
                    let mut c = counter.write().unwrap();
                    let v = *c;
                    *c += 1;
                    v
                };
                rt.block_on(async {
                    db.transaction(|txn| {
                        let n = name.clone();
                        async move {
                            txn.set(&n, &key![i], value!(i)).await?;
                            Ok(())
                        }
                    })
                    .await
                    .unwrap();
                })
            })
        });
    });

    group.finish();
}

/// Benchmark batch `set` operations (multiple sets per transaction).
fn bench_set_batch(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("set_batch");
    group.measurement_time(std::time::Duration::from_secs(15));

    MODES.iter().for_each(|&mode| {
        [100i64, 1_000, 10_000].iter().for_each(|&batch_size| {
            group.throughput(Throughput::Elements(batch_size as u64));
            group.bench_function(format!("{mode}/{batch_size}_ops"), |b| {
                b.iter_batched(
                    || create_db(mode, &rt),
                    |(_dir, db)| {
                        rt.block_on(async {
                            db.transaction(|txn| async move {
                                insert_keys(
                                    &txn,
                                    &global!("BENCH"),
                                    batch_size,
                                )
                                .await?;
                                Ok(())
                            })
                            .await
                            .unwrap();
                        })
                    },
                    BatchSize::SmallInput,
                )
            });
        });
    });

    group.finish();
}

/// Benchmark large batch `set` operations (100k elements).
fn bench_set_batch_100k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("set_batch_large");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(30));

    let batch_size = 100_000i64;
    group.throughput(Throughput::Elements(batch_size as u64));

    MODES.iter().for_each(|&mode| {
        group.bench_function(format!("{mode}/100000_ops"), |b| {
            b.iter_batched(
                || create_db(mode, &rt),
                |(_dir, db)| {
                    rt.block_on(async {
                        db.transaction(|txn| async move {
                            insert_keys(&txn, &global!("BENCH"), batch_size)
                                .await?;
                            Ok(())
                        })
                        .await
                        .unwrap();
                    })
                },
                BatchSize::LargeInput,
            )
        });
    });

    group.finish();
}

/// Benchmark multi-global writes (1000 values across 100 globals).
///
/// Tests sharded node cache performance with concurrent global access.
fn bench_set_multi_global(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("set_multi_global");
    group.measurement_time(std::time::Duration::from_secs(15));

    let n_globals = 100usize;
    let vals_per_global = 10i64;
    let total = (n_globals as i64) * vals_per_global;

    group.throughput(Throughput::Elements(total as u64));

    MODES.iter().for_each(|&mode| {
        group.bench_function(
            format!("{mode}/{total}_across_{n_globals}_globals"),
            |b| {
                b.iter_batched(
                    || {
                        let (dir, db) = create_db(mode, &rt);
                        // Pre-generate global names
                        let globals: Vec<Name> = (0..n_globals)
                            .map(|i| Name::global(&format!("G{i}")))
                            .collect();
                        (dir, db, globals)
                    },
                    |(_dir, db, globals)| {
                        rt.block_on(async {
                            db.transaction(|txn| {
                                let gs = globals.clone();
                                async move {
                                    // Write vals_per_global values to each global
                                    futures::future::try_join_all(
                                        gs.iter().map(|g| {
                                            let t = &txn;
                                            async move {
                                                insert_keys(
                                                    t,
                                                    g,
                                                    vals_per_global,
                                                )
                                                .await
                                            }
                                        }),
                                    )
                                    .await?;
                                    Ok(())
                                }
                            })
                            .await
                            .unwrap();
                        })
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    });

    group.finish();
}

/// Benchmark `get` operations on existing keys.
fn bench_get(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("get_existing");

    MODES.iter().for_each(|&mode| {
        let (_dir, db) = create_db(mode, &rt);
        let name = global!("BENCH");

        // Setup: insert 1000 keys
        rt.block_on(async {
            db.transaction(|txn| {
                let n = name.clone();
                async move { insert_keys(&txn, &n, 1000).await }
            })
            .await
            .unwrap();
        });

        group.bench_function(format!("{mode}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    db.transaction(|txn| {
                        let n = name.clone();
                        async move {
                            let val = txn.get(&n, &key![500i64]).await?;
                            black_box(val);
                            Ok(())
                        }
                    })
                    .await
                    .unwrap();
                })
            })
        });
    });

    group.finish();
}

/// Benchmark `order` iteration.
fn bench_order(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("order_next_100");

    MODES.iter().for_each(|&mode| {
        let (_dir, db) = create_db(mode, &rt);
        let name = global!("BENCH");

        // Setup: insert 1000 keys
        rt.block_on(async {
            db.transaction(|txn| {
                let n = name.clone();
                async move { insert_keys(&txn, &n, 1000).await }
            })
            .await
            .unwrap();
        });

        group.bench_function(format!("{mode}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    db.transaction(|txn| {
                        let n = name.clone();
                        async move {
                            let mut count = 0;
                            let mut current: Option<rumps_types::Key> = None;
                            // Iterate through first 100 keys
                            loop {
                                match txn.order(&n, current.as_ref()).await? {
                                    Some(next) if count < 100 => {
                                        current = Some(next);
                                        count += 1;
                                    }
                                    _ => break,
                                }
                            }
                            black_box(count);
                            Ok(())
                        }
                    })
                    .await
                    .unwrap();
                })
            })
        });
    });

    group.finish();
}

/// Benchmark `collects` stream iteration.
fn bench_collects(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("collects_1000");

    MODES.iter().for_each(|&mode| {
        let (_dir, db) = create_db(mode, &rt);
        let name = global!("BENCH");

        // Setup: insert 1000 keys
        rt.block_on(async {
            db.transaction(|txn| {
                let n = name.clone();
                async move { insert_keys(&txn, &n, 1000).await }
            })
            .await
            .unwrap();
        });

        group.bench_function(format!("{mode}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    db.transaction(|txn| {
                        let n = name.clone();
                        async move {
                            let vals: Vec<Value> = txn
                                .collects(
                                    &n,
                                    None,
                                    |_, _| true,
                                    |_, v| v.clone(),
                                )
                                .await?
                                .try_collect()
                                .await?;
                            black_box(vals.len());
                            Ok(())
                        }
                    })
                    .await
                    .unwrap();
                })
            })
        });
    });

    group.finish();
}

/// Benchmark `collects` stream iteration with 10,000 entries.
fn bench_collects_10k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("collects_10000");

    MODES.iter().for_each(|&mode| {
        let (_dir, db) = create_db(mode, &rt);
        let name = global!("BENCH");

        // Setup: insert 10,000 keys
        rt.block_on(async {
            db.transaction(|txn| {
                let n = name.clone();
                async move { insert_keys(&txn, &n, 10_000).await }
            })
            .await
            .unwrap();
        });

        group.bench_function(format!("{mode}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    db.transaction(|txn| {
                        let n = name.clone();
                        async move {
                            let vals: Vec<Value> = txn
                                .collects(
                                    &n,
                                    None,
                                    |_, _| true,
                                    |_, v| v.clone(),
                                )
                                .await?
                                .try_collect()
                                .await?;
                            black_box(vals.len());
                            Ok(())
                        }
                    })
                    .await
                    .unwrap();
                })
            })
        });
    });

    group.finish();
}

/// Benchmark `kill` operation.
fn bench_kill(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("kill_subtree_100");

    MODES.iter().for_each(|&mode| {
        group.bench_function(format!("{mode}"), |b| {
            b.iter_batched(
                || {
                    let (dir, db) = create_db(mode, &rt);
                    let name = global!("BENCH");

                    // Setup: insert 100 keys under a common prefix
                    rt.block_on(async {
                        db.transaction(|txn| {
                            let n = name.clone();
                            async move {
                                futures::stream::iter(0..100i64)
                                    .then(|i| {
                                        let t = &txn;
                                        let n = &n;
                                        async move {
                                            t.set(
                                                n,
                                                &key!["prefix", i],
                                                value!(i),
                                            )
                                            .await
                                        }
                                    })
                                    .try_collect::<Vec<_>>()
                                    .await?;
                                Ok(())
                            }
                        })
                        .await
                        .unwrap();
                    });

                    (dir, db, name)
                },
                |(_dir, db, name)| {
                    rt.block_on(async {
                        db.transaction(|txn| {
                            let n = name.clone();
                            async move {
                                txn.kill(&n, &key!["prefix"]).await?;
                                Ok(())
                            }
                        })
                        .await
                        .unwrap();
                    })
                },
                BatchSize::SmallInput,
            )
        });
    });

    group.finish();
}

// Sync Mode Comparison Benchmarks
//
// These benchmarks compare throughput between `OnCommit` (default) and `Relaxed`
// sync modes for on-disk databases. `Relaxed` skips fsync on commit, relying on
// OS page cache, which trades durability for throughput.

/// Creates an on-disk database with the specified sync mode.
///
/// See [`create_db`] for why we use `CARGO_MANIFEST_DIR` instead of default `/tmp`.
fn create_db_with_sync(sync: SyncMode, rt: &Runtime) -> (TempDir, Database) {
    let dir = TempDir::new_in(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let db = rt
        .block_on(Database::builder().sync_mode(sync).create(dir.path()))
        .unwrap();
    (dir, db)
}

/// Benchmark batch sets comparing `OnCommit` vs `Relaxed` sync modes.
fn bench_sync_mode_set_batch(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("sync_mode/set_batch");
    group.measurement_time(std::time::Duration::from_secs(15));

    let modes = [
        ("OnCommit", SyncMode::OnCommit),
        ("Relaxed", SyncMode::Relaxed),
    ];

    modes.iter().for_each(|(label, sync)| {
        [100i64, 1_000, 10_000].iter().for_each(|&batch_size| {
            group.throughput(Throughput::Elements(batch_size as u64));
            group.bench_function(format!("{label}/{batch_size}_ops"), |b| {
                b.iter_batched(
                    || create_db_with_sync(*sync, &rt),
                    |(_dir, db)| {
                        rt.block_on(async {
                            db.transaction(|txn| async move {
                                insert_keys(
                                    &txn,
                                    &global!("BENCH"),
                                    batch_size,
                                )
                                .await?;
                                Ok(())
                            })
                            .await
                            .unwrap();
                        })
                    },
                    BatchSize::SmallInput,
                )
            });
        });
    });

    group.finish();
}

/// Benchmark large batch sets (100k) comparing sync modes.
fn bench_sync_mode_set_batch_100k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("sync_mode/set_batch_large");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(30));

    let batch_size = 100_000i64;
    group.throughput(Throughput::Elements(batch_size as u64));

    let modes = [
        ("OnCommit", SyncMode::OnCommit),
        ("Relaxed", SyncMode::Relaxed),
    ];

    modes.iter().for_each(|(label, sync)| {
        group.bench_function(format!("{label}/100000_ops"), |b| {
            b.iter_batched(
                || create_db_with_sync(*sync, &rt),
                |(_dir, db)| {
                    rt.block_on(async {
                        db.transaction(|txn| async move {
                            insert_keys(&txn, &global!("BENCH"), batch_size)
                                .await?;
                            Ok(())
                        })
                        .await
                        .unwrap();
                    })
                },
                BatchSize::LargeInput,
            )
        });
    });

    group.finish();
}

/// Benchmark multi-global writes comparing sync modes.
fn bench_sync_mode_multi_global(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("sync_mode/set_multi_global");
    group.measurement_time(std::time::Duration::from_secs(15));

    let n_globals = 100usize;
    let vals_per_global = 10i64;
    let total = (n_globals as i64) * vals_per_global;

    group.throughput(Throughput::Elements(total as u64));

    let modes = [
        ("OnCommit", SyncMode::OnCommit),
        ("Relaxed", SyncMode::Relaxed),
    ];

    modes.iter().for_each(|(label, sync)| {
        group.bench_function(
            format!("{label}/{total}_across_{n_globals}_globals"),
            |b| {
                b.iter_batched(
                    || {
                        let (dir, db) = create_db_with_sync(*sync, &rt);
                        let globals: Vec<Name> = (0..n_globals)
                            .map(|i| Name::global(&format!("G{i}")))
                            .collect();
                        (dir, db, globals)
                    },
                    |(_dir, db, globals)| {
                        rt.block_on(async {
                            db.transaction(|txn| {
                                let gs = globals.clone();
                                async move {
                                    futures::future::try_join_all(
                                        gs.iter().map(|g| {
                                            let t = &txn;
                                            async move {
                                                insert_keys(
                                                    t,
                                                    g,
                                                    vals_per_global,
                                                )
                                                .await
                                            }
                                        }),
                                    )
                                    .await?;
                                    Ok(())
                                }
                            })
                            .await
                            .unwrap();
                        })
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_transaction_overhead,
    bench_set_single,
    bench_set_batch,
    bench_set_batch_100k,
    bench_set_multi_global,
    bench_get,
    bench_order,
    bench_collects,
    bench_collects_10k,
    bench_kill,
);

criterion_group!(
    sync_mode_benches,
    bench_sync_mode_set_batch,
    bench_sync_mode_set_batch_100k,
    bench_sync_mode_multi_global,
);

criterion_main!(benches, sync_mode_benches);
