# RUMPS Persistence TODOs

**NOTE**: For storage layer architecture and implementation details, see `docs/storage.org`.

---

## Future Considerations (Post-Goal 1)

These are the current next steps (post persistence and basic DB):

- **Networking**: Client-server protocol for remote access
- **Parallel Transactions**: Enable concurrent transaction execution to improve throughput (see [persistence/parallel-transactions.md](persistence/parallel-transactions.md))

These are the potential next steps (post networking):

- **Compression**: Single-page LZ4 compression is implemented; multi-page or alternative codecs are not yet planned
- **Encryption**: Optional encryption at rest (see [persistence/encryption.md](persistence/encryption.md))

These are future considerations not yet planned:
- **Advanced Concurrency**: Lock-free data structures, optimistic concurrency control (current plan uses Mutex for commit serialization)
- **Multi-Version Concurrency Control (MVCC)**: Full MVCC for better read concurrency (current plan uses snapshot isolation)
- **Savepoints**: Nested transactions with partial rollback
- **Replication**: Multi-node deployment with data replication
- **Distributed Transactions**: Two-phase commit for multi-node transactions
- **Configuration Changes**: Runtime overrides, persistent reconfiguration, and data rebuilds (see [persistence/reconfigure.md](persistence/reconfigure.md))
- **`COLLECT` Enhancements**:
  - Parallel processing with `buffer_unordered` for concurrent record processing:
    ```rust
    let results: Vec<_> = btree.collects(...)
        .map(|r| async move { process_record_async(r).await })
        .buffer_unordered(10)
        .try_collect()
        .await?;
    ```
  - Bidirectional iteration via `get_prev_internal` for reverse traversal
  - Batch node reads and read-ahead buffering for performance
  - Range queries with end bounds (inclusive/exclusive)

## Fixes + Notes

### 6. Two-layer cache redundancy

There are two caches: `DashMap<NodeId, Arc<Node>>` (`NodeCache` in btree.rs, used for locals) and `PageCache` (LRU with `RwLock`, used by `FileStorageEngine` for globals). They serve different populations (locals vs globals), but the `BTree` also calls into the engine's `read()` which hits `PageCache`. The `BTree` then separately caches the result in `NodeCache`. This means globals get double-cached. Unify or clearly separate the responsibilities.

**Failed attempt (reverted)**: Added `modify_node`/`try_modify_node` wrappers that synced every mutation to both caches via a `sync_dirty` step (read node back from `DashMap`, then `put` into `PageCache` as dirty). This fixed the correctness bug (`modify`/`try_modify` only updating `NodeCache` without marking pages dirty) but caused 50%+ regressions across the board. The problem: every B-tree mutation (splits, merges, borrows, inserts, deletes) acquired three locks instead of one; the `DashMap` write for the modification itself, a `DashMap` read to retrieve the node, then a global `PageCache` `RwLock` write to sync it. The `PageCache` write lock is unsharded, so it serialized all mutations.

**Failed attempt 2 (reverted)**: Introduced a `NodeStore` enum routing by tree kind. In-memory trees used `DashMap` directly (as before). Persistent trees used `PageCache` exclusively, skipping `DashMap` entirely. Also split `Database` into two BTrees (one per namespace) so locals never touched `PageCache`. This fixed the `modify`/`try_modify` correctness bug (no second cache to fall out of sync) but caused +30-200% regressions across all persistent benchmarks. The problem: `PageCache.get()` requires a *write* lock (LRU promotion), and the `PageCache` `RwLock` is unsharded. The old `DashMap` served as a fast sharded read cache in front of `PageCache`; removing it forced every `load_node` through the single `PageCache` write lock, serializing all reads. `get_existing` regressed ~89%, `collects` ~200%, `set_batch_large/100000` ~49%, `set_multi_global/1000` ~48%.

**Failed attempt 3 (reverted)**: Added a `DashSet<NodeId>` (`pending_dirty`) inside `NodeCache`. `modify`/`try_modify` inserted the node ID into the set after each mutation (sharded, no new lock contention on the hot path). Before each flush, `BTree::sync_pending_dirty()` drained the set, read each latest `Arc<Node>` from `DashMap`, and pushed it into `PageCache` via `engine.mark_dirty()`, so the existing flush logic wrote correct data. This deferred all `PageCache` write-lock acquisition to flush boundaries rather than per-mutation. Despite no new locks on the mutation hot path, set operations regressed ~20% across the board. The problem: even a sharded `DashSet::insert` per `modify`/`try_modify` call adds measurable overhead; B-tree mutations call `modify` many times per operation (splits, merges, borrows, rebalances), and the cumulative cost of the extra sharded-set bookkeeping on every one was significant. The batched `sync_pending_dirty` flush step itself was not the bottleneck; the per-mutation `DashSet::insert` was.

**Failed attempt 4 (reverted)**: Replaced `DashMap<NodeId, Arc<Node>>` with `DashMap<NodeId, CachedNode>` where `CachedNode` is a struct wrapping `Arc<Node>` + `AtomicBool` (dirty flag). `modify`/`try_modify` set the `AtomicBool` with a relaxed store while already holding the shard write guard (effectively zero per-mutation CPU cost). At flush boundaries, `take_dirty()` iterated the entire `DashMap` to collect entries with the flag set, then synced them to `PageCache`. Caused regressions across the board. Two problems: (1) `CachedNode` is 16 bytes (8-byte `Arc` + 1-byte `AtomicBool` + 7 bytes alignment padding) vs the original 8-byte `Arc<Node>`, doubling DashMap entry size and degrading cache-line utilization on every operation (reads included); (2) `take_dirty()` performed an O(n) scan of the entire `DashMap` on every flush, locking each shard in turn, even when only 1-2 entries were dirty.

**Failed attempt 5 (reverted)**: Kept `DashMap<NodeId, Arc<Node>>` (8-byte entries, unchanged) and tracked dirty IDs with a `parking_lot::Mutex<Vec<NodeId>>` alongside the `DashMap`. `modify`/`try_modify` locked the mutex and pushed the `NodeId` after each mutation. At flush boundaries, drained the vec, deduplicated, and point-looked-up each dirty ID in `DashMap` to sync to `PageCache`. Also made `sync_pending_dirty` fully synchronous (no `futures::stream` overhead) via a new `mark_dirty_sync` on the engine. Caused 60%+ regressions on writes. The problem: even an uncontended `parking_lot::Mutex` lock/unlock per `modify`/`try_modify` call (~5-10ns per CAS+push) compounds across the thousands of mutations per B-tree operation. This is worse than the `DashSet` (attempt 3), likely because `DashSet`'s sharded locking had lower effective contention than a single global `Mutex`, and/or because the mutex acquisition stalls the CPU pipeline more than a sharded `DashSet` probe on the same shard the `DashMap` write guard already warmed.

**Correct approach**: The `DashMap` read path is load-bearing for performance; the fix must keep it. The real problem is that `modify`/`try_modify` mutate nodes in `DashMap` without syncing dirty state to `PageCache`. A correct fix must either (a) make `DashMap` the single authority for persistent nodes too (promoting it to a dirty-tracking cache and demoting `PageCache` to a decode cache / disk-read buffer), or (b) keep both caches but ensure `modify`/`try_modify` write-through to `PageCache` without acquiring the `PageCache` write lock on every mutation (e.g. batched dirty-sync at `save_node` / flush boundaries). Option (a) avoids the double-cache entirely; option (b) preserves the architecture but must be careful not to reintroduce the three-lock pattern from failed attempt 1. Attempts 3-5 showed that ANY per-mutation bookkeeping (`DashSet::insert`, `AtomicBool::store` in a larger struct, or `Mutex<Vec>::push`) causes measurable regressions, even when the individual operation cost is very low. A viable approach likely needs truly zero per-mutation overhead, which probably means option (a): promoting `DashMap` to the sole authority for persistent nodes and having the flush path read dirty state directly from it (e.g. via an inline `AtomicBool` that does NOT enlarge the entry, or by making the flush path compare `DashMap` entries against on-disk state). Alternatively, eliminating `modify`/`try_modify` entirely and routing all mutations through `save_node` (which already syncs both caches) could work if the `PageCache` write lock can be made cheaper (e.g. sharded).

### 9. ~~File I/O: single `RwLock<File>` for all reads and writes~~ DONE

Replaced `Arc<RwLock<tokio::fs::File>>` with `Arc<std::fs::File>` using synchronous positional I/O (`read_exact_at`/`write_all_at` from `std::os::unix::fs::FileExt`). No lock, no seek, no `spawn_blocking`.

### Approach + bench results
`pread`/`pwrite` on a 4K page takes ~1us, well under the threshold where blocking the tokio runtime matters (tokio's guidance is roughly 10-100us+). The `PageCache` already uses synchronous `parking_lot::RwLock` for the same reason. Calling these directly on the runtime thread avoids the `spawn_blocking` overhead (~2-5us per call) that dominated the syscall cost in the previous attempt.

Positional I/O is also inherently thread-safe on the same fd (`pread`/`pwrite` are atomic at the page level on Linux), so this naturally supports parallel transactions if they are ever added, with no code changes needed to unlock concurrency.

**Benchmark results** (vs the `RwLock<tokio::fs::File>` + `seek` + async read/write baseline):

| Benchmark                | Change        |
|--------------------------|---------------|
| `transaction_empty`      | -54%          |
| `set_single`             | -46%          |
| `set_batch/100`          | -39%          |
| `set_batch/1000`         | -38%          |
| `set_batch/10000`        | -22%          |
| `set_batch_large/100000` | -16%          |
| `set_multi_global/1000`  | -60%          |
| `get_existing`           | -53%          |
| `query_next_100`         | -25%          |
| `order_next_100`         | -21%          |
| `collects_1000`          | +4.5% (noise) |
| `collects_10000`         | +1.9% (noise) |
| `kill_subtree_100`       | no change     |

**Failed attempt (reverted, preserved for reference)**: `pread`/`pwrite` via `spawn_blocking` + `std::os::unix::fs::FileExt`. The `spawn_blocking` overhead per call (~2-5us for thread pool scheduling + context switch) dominated the actual `pwrite` syscall time (~1us). For batch flushes with hundreds of dirty pages, this overhead compounded: `set_batch/10000` regressed +63%, `set_batch_large/100000` regressed +58%.

### 13. ~~Flush writes dirty pages sequentially then syncs~~ DONE

Flush now encodes all dirty pages, sorts them by page number, groups contiguous runs with `slice::chunk_by`, and writes each group via `pwritev` (falling back to individual `write_all_at` for singletons or partial writes). The `IOV_MAX` cap is `1024` per `pwritev` call.

This replaced the `FuturesUnordered` approach, which provided no real concurrency since all writes contended on the same `RwLock<File>` (now removed; see #9).

