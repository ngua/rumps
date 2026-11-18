# RUMPS Persistence TODOs

**NOTE**: For storage layer architecture and implementation details, see `docs/storage.org`.

---

## Future Considerations (Post-Goal 1)

These are the current next steps (post persistence and basic DB):

- **Networking**: Client-server protocol for remote access
- **Parallel Transactions**: Enable concurrent transaction execution to improve throughput (see [persistence/parallel-transactions.md](persistence/parallel-transactions.md))

These are the potential next steps (post networking):

- **Compression**: Compress nodes/pages to save disk space (see [persistence/compression.md](persistence/compression.md))
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

## Fixes

### 6. Two-layer cache redundancy

There are two caches: `DashMap<NodeId, Arc<Node>>` (`NodeCache` in btree.rs, used for locals) and `PageCache` (LRU with `RwLock`, used by `FileStorageEngine` for globals). They serve different populations (locals vs globals), but the `BTree` also calls into the engine's `read()` which hits `PageCache`. The `BTree` then separately caches the result in `NodeCache`. This means globals get double-cached. Unify or clearly separate the responsibilities.

**Failed attempt (reverted)**: Added `modify_node`/`try_modify_node` wrappers that synced every mutation to both caches via a `sync_dirty` step (read node back from `DashMap`, then `put` into `PageCache` as dirty). This fixed the correctness bug (`modify`/`try_modify` only updating `NodeCache` without marking pages dirty) but caused 50%+ regressions across the board. The problem: every B-tree mutation (splits, merges, borrows, inserts, deletes) acquired three locks instead of one; the `DashMap` write for the modification itself, a `DashMap` read to retrieve the node, then a global `PageCache` `RwLock` write to sync it. The `PageCache` write lock is unsharded, so it serialized all mutations.

**Correct approach**: Introduce a `NodeStore` enum that routes by tree kind. In-memory trees use `DashMap` directly (as today). Persistent trees use `PageCache` exclusively and skip `DashMap` entirely. This eliminates the sync overhead instead of doubling the write cost; reads, writes, and dirty tracking all go through one cache per tree kind. The `modify`/`try_modify` correctness bug goes away because there is no second cache to fall out of sync with.

### 9. ~~File I/O: single `RwLock<File>` for all reads and writes~~ NEED RECONSIDERATION

`FileStorageEngine` wraps the data file in `Arc<RwLock<File>>`. Every page read acquires a write lock (because `seek` + `read_exact` must be atomic). This serializes all disk I/O.

Fix: use `pread`/`pwrite` (positional I/O) which don't need seeking and can run concurrently. On Linux, `pread64`/`pwrite64` are atomic at the page level.

**Note**: `pread`/`pwrite` via `spawn_blocking` + `std::os::unix::fs::FileExt` was tried and reverted. The `spawn_blocking` overhead per call (~2-5 us for thread pool scheduling + context switch) dominates the actual `pwrite` syscall time (~1 us). For batch flushes with hundreds of dirty pages, this overhead compounds: `set_batch/10000` regressed +63%, `set_batch_large/100000` regressed +58%. Single-op benchmarks improved (e.g. `set_single` -35%) because the overhead is amortized. The `RwLock` has zero contention in the current single-threaded model, so removing it provides no benefit. Revisit when parallel transactions are implemented; the contention-free `pread`/`pwrite` path will pay off once multiple transactions do I/O concurrently.

### 10. Full-page reads and writes (always `PAGE_SIZE` bytes; high priority)

`read()` always reads `PAGE_SIZE` bytes and deserializes the whole buffer. `write_page_to_disk()` serializes then pads to `PAGE_SIZE`. A small leaf node that serializes to 200 bytes still reads/writes 4096 bytes.

When compression is added (per TODO), this becomes critical: store the compressed length in a small header, read only the compressed bytes, decompress on demand. Without compression, at least consider variable-length records with a page directory for multi-record pages, or prefix compression for keys that share common prefixes (which is the norm in RUMPS's hierarchical key model).

### 13. ~~Flush writes dirty pages sequentially then syncs~~ NEED RECONSIDERATION

**NOTE**: See #9 above

`flush()` uses `FuturesUnordered` to write dirty pages, but the writes still contend on the single `RwLock<File>` (see #9), so concurrency is illusory. Once #9 is resolved with lock-free positional I/O, the `FuturesUnordered` path becomes truly concurrent. Remaining opportunity: vectored I/O (`writev`) to batch multiple page writes into fewer syscalls.

