use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use futures::StreamExt;
use rumps_storage::BTree;
use rumps_types::{Key, Name, NodeData, Value};
use tokio::runtime::Runtime;

/// Helper function to create a key with the specified depth.
///
/// For depth=3, creates Key([1, 2, 3])
/// This will result in (depth - 1) ancestors being created.
fn create_key_at_depth(depth: usize) -> Key {
    Key::from((1..=depth).map(|i| (i as i64).into()).collect::<Vec<_>>())
}

/// Benchmark INSERT operations at various depths to show hierarchical semantics performance characteristics.
///
/// This benchmarks the `set()` operation which internally calls `ensure_ancestors()` before insertion,
/// demonstrating the time complexity as depth increases.
fn bench_insert_depths(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_by_depth");
    let rt = Runtime::new().unwrap();

    [2, 3, 4, 5, 10].iter().copied().for_each(|depth| {
        group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &depth,
            |b, &depth| {
                b.iter(|| {
                    rt.block_on(async {
                        let btree = BTree::new(3).unwrap();
                        let name = Name::Global("VAR".into());
                        let key = create_key_at_depth(depth);
                        let value = Value::Integer(42);

                        // This call internally invokes ensure_ancestors() before insertion
                        btree
                            .set_internal(
                                &name,
                                &key,
                                NodeData::with_value(value),
                            )
                            .await
                            .unwrap();
                    })
                });
            },
        );
    });

    group.finish();
}

/// Benchmark INSERT with existing ancestors to show amortization benefits.
///
/// This tests the scenario where ancestors already exist, which should be faster
/// than creating them from scratch.
fn bench_insert_with_existing_ancestors(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("depth_5_with_existing_ancestors", |b| {
        b.iter(|| {
            rt.block_on(async {
                let btree = BTree::new(3).unwrap();
                let name = Name::Global("VAR".into());

                // First insert creates all ancestors
                let key1 = Key::from(vec![
                    1.into(),
                    2.into(),
                    3.into(),
                    4.into(),
                    100.into(),
                ]);
                btree
                    .set_internal(
                        &name,
                        &key1,
                        NodeData::with_value(Value::Integer(42)),
                    )
                    .await
                    .unwrap();

                // Second insert at same depth should be faster (ancestors exist)
                let key2 = Key::from(vec![
                    1.into(),
                    2.into(),
                    3.into(),
                    4.into(),
                    200.into(),
                ]);
                btree
                    .set_internal(
                        &name,
                        &key2,
                        NodeData::with_value(Value::Integer(43)),
                    )
                    .await
                    .unwrap();
            })
        });
    });
}

/// Benchmark ancestor creation in isolation.
///
/// This directly measures ancestor creation performance by repeatedly
/// creating ancestors without the final key insertion.
fn bench_ensure_ancestors(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("ensure_ancestors_depth_5", |b| {
        b.iter(|| {
            rt.block_on(async {
                let btree = BTree::new(3).unwrap();
                let name = Name::Global("VAR".into());
                let key = create_key_at_depth(5);

                // Create all ancestors
                let ancestors = key.ancestors();
                futures::stream::iter(ancestors)
                    .for_each(|ancestor| {
                        let btree = &btree;
                        let name = name.clone();
                        async move {
                            btree
                                .set_internal(
                                    &name,
                                    &ancestor,
                                    NodeData::with_value(Value::String(
                                        "ancestor".into(),
                                    )),
                                )
                                .await
                                .unwrap();
                        }
                    })
                    .await;
            })
        });
    });
}

/// Benchmark worst case: deep nesting with completely fresh tree.
fn bench_worst_case(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("worst_case_depth_10_fresh_tree", |b| {
        b.iter(|| {
            rt.block_on(async {
                // Fresh tree for each iteration
                let btree = BTree::new(3).unwrap();
                let name = Name::Global("DEEP".into());
                let key = create_key_at_depth(10);

                btree
                    .set_internal(
                        &name,
                        &key,
                        NodeData::with_value(Value::String("value".into())),
                    )
                    .await
                    .unwrap();
            })
        });
    });
}

/// Benchmark best case: shallow nesting (depth 2).
fn bench_best_case(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("best_case_depth_2_fresh_tree", |b| {
        b.iter(|| {
            rt.block_on(async {
                let btree = BTree::new(3).unwrap();
                let name = Name::Global("SHALLOW".into());
                let key = create_key_at_depth(2);

                btree
                    .set_internal(
                        &name,
                        &key,
                        NodeData::with_value(Value::String("value".into())),
                    )
                    .await
                    .unwrap();
            })
        });
    });
}

criterion_group!(
    benches,
    bench_insert_depths,
    bench_insert_with_existing_ancestors,
    bench_ensure_ancestors,
    bench_worst_case,
    bench_best_case
);
criterion_main!(benches);
