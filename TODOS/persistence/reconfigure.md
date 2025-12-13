# Database Configuration Changes

Four APIs for changing database configuration at different levels of persistence and scope.

## Overview

| API                     | Scope               | Persists? | Safe Only? | Use Case                            |
|-------------------------|---------------------|-----------|-----------:|-------------------------------------|
| ~~`db.local()`~~        | ~~Single callback~~ | ~~No~~    |    ~~Yes~~ | ~~Scoped config for one operation~~ |
| `Database::override`    | Entire session      | No        |        Yes | Session-wide config override        |
| `Database::reconfigure` | Permanent           | Yes       |        Yes | Persistent safe config updates      |
| `Database::rebuild`     | Permanent (new DB)  | Yes       |         No | Layout-affecting migrations         |

**Note:** `db.local()` was deemed not feasible; see Phase 2 below.

## Safe vs Unsafe Configuration

| Config Field        | Safe? | Reason                              |
|---------------------|------:|-------------------------------------|
| `cache_size`        |   Yes | Runtime-only; no data impact        |
| `sync_mode`         |   Yes | Durability policy; no data impact   |
| `wal_max_file_size` |   Yes | File rotation threshold only        |
| `max_pages`         |    No | Decrease may exceed current usage   |
| `max_memory_bytes`  |    No | Decrease may cause eviction/failure |
| `min_degree`        |    No | Affects B-tree node structure       |
| `PAGE_SIZE`         |   N/A | Compile-time constant               |

## API Design

### ~~`db.local()`~~ (Not Feasible)

~~Temporarily applies config overrides for a single callback, then restores original config.~~
~~Analogous to `local` in Haskell's `ReaderT`.~~

**Status: Not feasible.** Dynamically swapping `sync_mode` at runtime requires either:
- An `RwLock` around the WAL config, adding lock contention on every commit (the hot path)
- Atomic storage of `SyncMode`, complicated by `Periodic(Duration)` variant

Neither approach is worth the performance tradeoff for a convenience API. Users who need
scoped config changes should use `Database::reconfigure` before and after, or open a
separate database handle with `Database::override`.

### `Database::reconfigure`

Updates stored metadata for safe config fields. Database must be closed.

```rust
Database::reconfigure("./data")
    .cache_size(4096)
    .sync_mode(SyncMode::Immediate)
    .wal_max_file_size(128 * 1024 * 1024)
    .apply()
    .await?;

// Subsequent opens use new config.
```

### `Database::rebuild`

Rewrites entire database with new layout-affecting config. Outputs to new path.

```rust
Database::rebuild("./data")
    .min_degree(5)
    .max_pages(10000)
    .max_memory_bytes(Some(512 * 1024 * 1024))
    .output("./data_new")
    .apply()
    .await?;

// Original "./data" unchanged.
// New database at "./data_new" with migrated data.
```

## Implementation

### Structs

```rust
/// Safe config fields shared by `local`, `override`, and `reconfigure`.
pub(crate) struct SafeReconfiguration {
    pub(crate) cache_size: Option<usize>,
    pub(crate) sync_mode: Option<SyncMode>,
    pub(crate) wal_max_file_size: Option<u64>,
}

/// Builder for `Database::override`.
pub struct DatabaseOverride {
    path: PathBuf,
    config: SafeReconfiguration,
}

/// Builder for `Database::reconfigure`.
pub struct DatabaseReconfigure {
    path: PathBuf,
    config: SafeReconfiguration,
}

/// Builder for `Database::rebuild`.
pub struct DatabaseRebuild {
    path: PathBuf,
    output: Option<PathBuf>,
    // All config fields; unsafe ones included
    cache_size: Option<usize>,
    sync_mode: Option<SyncMode>,
    wal_max_file_size: Option<u64>,
    max_pages: Option<Option<u64>>,
    max_memory_bytes: Option<Option<usize>>,
    min_degree: Option<usize>,
}
```

### ~~`local()` Implementation Notes~~ (Cancelled)

~~The `local()` method needs to:~~
~~1. Snapshot current config~~
~~2. Apply overrides~~
~~3. Run callback~~
~~4. Restore original config (even on error/panic)~~

~~For `sync_mode`, this likely means swapping the WAL writer's config. Need to ensure~~
~~thread-safety if other operations are concurrent. Options:~~
~~- Use `RwLock` on config and swap atomically~~
~~- Create a scoped "view" that intercepts calls~~
~~- For simplicity, require exclusive access during `local()` (no concurrent ops)~~

**Cancelled:** See "Not Feasible" note in API Design section above.

## Checklist

### Phase 1: `SafeReconfiguration` Infrastructure

- [x] Create `SafeReconfiguration` struct in `database.rs` (or new `config.rs`)
- [x] ~~Implement `SafeReconfiguration::apply_to(&self, StorageConfig) -> StorageConfig`~~ (applied inline)
- [x] Add internal method to swap/restore config on `Database` or `FileStorage`
- [x] Add directory lock file (`db.lock`) via `fs2` crate to prevent concurrent access
  - [x] `FileStorageEngine` holds `StdFile` handle for lock lifetime
  - [x] `StorageError::DatabaseLocked` error variant for lock failures
  - [x] Tests for concurrent open prevention and lock release on drop

### ~~Phase 2: `Database::local`~~ (Cancelled)

**Cancelled:** Runtime config swapping introduces unacceptable lock contention on the
WAL writer's hot path. See "Not Feasible" note in API Design section.

~~- [ ] Create `DatabaseLocal<'a>` builder struct~~
~~- [ ] Implement `Database::local(&self) -> DatabaseLocal<'_>` (via `BoxedFuture`?)~~
~~- [ ] Implement `DatabaseLocal::cache_size`, `sync_mode`, `wal_max_file_size` setters~~
~~- [ ] Implement `DatabaseLocal::run<F, Fut, T>()`:~~
  ~~- [ ] Snapshot current config~~
  ~~- [ ] Apply `SafeReconfiguration` overrides to database internals~~
  ~~- [ ] Run callback `f(&self.db)`~~
  ~~- [ ] Restore original config (use `scopeguard` or manual drop guard)~~
  ~~- [ ] Return callback result~~
~~- [ ] Add tests for `local`:~~
  ~~- [ ] Verify config restored after callback completes~~
  ~~- [ ] Verify config restored even if callback returns `Err`~~
  ~~- [ ] Verify config restored even if callback panics~~
  ~~- [ ] `local` with `SyncMode::Relaxed`, verify no fsync during callback (might be tricky)~~

### Phase 3: `Database::override`

- [x] Create `DatabaseOverride` builder struct
- [x] Implement `Database::open_override(path) -> DatabaseOverride`
- [x] Implement `DatabaseOverride::cache_size`, `sync_mode`, `wal_max_file_size` setters
- [x] Implement `DatabaseOverride::open()`:
  - [x] Read existing `MetadataPage` from disk
  - [x] Convert to `StorageConfig` via `to_storage_config()`
  - [x] Apply `SafeReconfiguration` overrides
  - [x] Open database with merged config (do NOT write back to metadata)
- [x] Add tests for `override`:
  - [x] Override `cache_size` and verify runtime behavior
  - [x] Override `sync_mode` and verify WAL behavior
  - [x] Verify original metadata unchanged after close

### Phase 4: `Database::reconfigure`

- [x] Create `DatabaseReconfigure` builder struct
- [x] Implement `Database::reconfigure(path) -> DatabaseReconfigure`
- [x] Implement `DatabaseReconfigure::cache_size`, `sync_mode`, `wal_max_file_size` setters
- [x] Implement `DatabaseReconfigure::apply()`:
  - [x] Verify database is not currently open (acquire `db.lock` via `fs2`)
  - [x] Read existing `MetadataPage`
  - [x] Apply `SafeReconfiguration` changes
  - [x] Write updated `MetadataPage` back to disk
  - [x] Sync to ensure durability
  - [x] Release lock
- [x] Add tests for `reconfigure`:
  - [x] Reconfigure `sync_mode`, reopen, verify new mode active
  - [x] Reconfigure `cache_size`, reopen, verify new size
  - [x] Verify `DatabaseLocked` error if database is open

### Phase 5: `Database::rebuild`

- [x] Create `DatabaseRebuild` builder struct
- [x] Implement `DatabaseReconfigure::rebuild() -> DatabaseRebuild`
- [x] Implement setters for ALL config fields (safe and unsafe)
- [x] Implement `DatabaseRebuild::output(path)` to set destination
- [x] Implement `DatabaseRebuild::apply()`:
  - [x] Open source database (with relaxed sync)
  - [x] Create destination database with new config via `Database::builder().create()`
  - [x] Iterate all globals in source (via `list_globals`)
  - [x] For each global, iterate all keys via `collects` in batches
  - [x] Insert each key-value into destination within transaction(s)
  - [x] Handle batching for large datasets (configurable `batch_size`)
  - [x] Close both databases
- [x] Add tests for `rebuild`:
  - [x] Rebuild with different `min_degree`, verify data intact
  - [x] Rebuild multiple globals, verify all copied
  - [x] Rebuild large dataset with batching, verify no data loss
  - [x] Error if `output` not specified
  - [x] Error if `output` already exists

### Phase 6: Documentation and Cleanup

- [ ] Add doc comments to all public APIs
- [ ] Add examples in doc comments
- [ ] Update `CLAUDE.md` if needed
