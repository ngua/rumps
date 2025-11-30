/// Benchmark for WAL operations, run with
/// `cargo bench -p rumps-storage --features bench`
use criterion::{criterion_group, criterion_main, Criterion};

fn bench(c: &mut Criterion) {
    rumps_storage::benches::wal::run_benchmarks(c);
}

criterion_group!(benches, bench);
criterion_main!(benches);
