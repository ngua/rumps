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

**Correct approach**: Introduce a `NodeStore` enum that routes by tree kind. In-memory trees use `DashMap` directly (as today). Persistent trees use `PageCache` exclusively and skip `DashMap` entirely. This eliminates the sync overhead instead of doubling the write cost; reads, writes, and dirty tracking all go through one cache per tree kind. The `modify`/`try_modify` correctness bug goes away because there is no second cache to fall out of sync with.

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

