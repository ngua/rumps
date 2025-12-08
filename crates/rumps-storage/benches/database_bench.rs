//! Database-level benchmarks for RUMPS storage.
//!
//! These benchmarks measure the performance of the public `Database` API,
//! including transactions, reads, writes, and iteration.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, Criterion,
    Throughput,
};
use futures::{StreamExt, TryStreamExt};
use rumps_storage::{Database, Transaction};
use rumps_types::{global, key, value, Name, Value};
use tempfile::TempDir;
use tokio::runtime::Runtime;

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
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();

    c.bench_function("transaction_empty", |b| {
        b.iter(|| {
            rt.block_on(async {
                db.transaction(|_txn| async move { Ok(()) }).await.unwrap();
            })
        })
    });
}

/// Benchmark single `set` operation within a transaction.
fn bench_set_single(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();
    let name = global!("BENCH");
    let counter = std::sync::RwLock::new(0i64);

    c.bench_function("set_single", |b| {
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
}

/// Benchmark batch `set` operations (multiple sets per transaction).
fn bench_set_batch(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("set_batch");

    [10i64, 100].into_iter().for_each(|batch_size| {
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_function(format!("{batch_size}_ops"), |b| {
            b.iter_batched(
                || {
                    let dir = TempDir::new().unwrap();
                    let db = rt.block_on(Database::create(dir.path())).unwrap();
                    (dir, db)
                },
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
                BatchSize::SmallInput,
            )
        });
    });

    group.finish();
}

/// Benchmark `get` operations on existing keys.
fn bench_get(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();
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

    c.bench_function("get_existing", |b| {
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
}

/// Benchmark `order` iteration.
fn bench_order(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();
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

    c.bench_function("order_next_100", |b| {
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
}

/// Benchmark `collects` stream iteration.
fn bench_collects(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();
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

    c.bench_function("collects_1000", |b| {
        b.iter(|| {
            rt.block_on(async {
                db.transaction(|txn| {
                    let n = name.clone();
                    async move {
                        let vals: Vec<Value> = txn
                            .collects(&n, None, |_, _| true, |_, v| v.clone())
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
}

/// Benchmark `collects` stream iteration with 10,000 entries.
fn bench_collects_10k(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();
    let db = rt.block_on(Database::create(dir.path())).unwrap();
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

    c.bench_function("collects_10000", |b| {
        b.iter(|| {
            rt.block_on(async {
                db.transaction(|txn| {
                    let n = name.clone();
                    async move {
                        let vals: Vec<Value> = txn
                            .collects(&n, None, |_, _| true, |_, v| v.clone())
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
}

/// Benchmark `kill` operation.
fn bench_kill(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("kill_subtree_100", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().unwrap();
                let db = rt.block_on(Database::create(dir.path())).unwrap();
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
                                        t.set(n, &key!["prefix", i], value!(i))
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
}

criterion_group!(
    benches,
    bench_transaction_overhead,
    bench_set_single,
    bench_set_batch,
    bench_get,
    bench_order,
    bench_collects,
    bench_collects_10k,
    bench_kill,
);

criterion_main!(benches);
