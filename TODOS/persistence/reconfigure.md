# Database Configuration Changes

Four APIs for changing database configuration at different levels of persistence and scope.

## Overview

| API                     | Scope              | Persists? | Safe Only? | Use Case                        |
|-------------------------|--------------------|-----------|-----------:|---------------------------------|
| `db.local()`            | Single callback    | No        |        Yes | Scoped config for one operation |
| `Database::override`    | Entire session     | No        |        Yes | Session-wide config override    |
| `Database::reconfigure` | Permanent          | Yes       |        Yes | Persistent safe config updates  |
| `Database::rebuild`     | Permanent (new DB) | Yes       |         No | Layout-affecting migrations     |

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

### `db.local()`

Temporarily applies config overrides for a single callback, then restores original config.
Analogous to `local` in Haskell's `ReaderT`.

```rust
let db = Database::open("./data").await?;

// Temporarily use Relaxed mode for a batch import
db.local()
    .sync_mode(SyncMode::Relaxed)
    .run(|db| async move {
        // All operations here use Relaxed mode
        db.transaction(|txn| async move {
            txn.set(&name, &k1, v1).await?;
            txn.set(&name, &k2, v2).await?;
            Ok(())
        }).await?;

        db.transaction(|txn| async move {
            // ... more batch inserts ...
            Ok(())
        }).await?;

        Ok(())
    })
    .await?;

// Back to original config here (e.g. OnCommit mode)
```

### `Database::override`

Opens a database with config overrides for the entire session. Does not modify stored metadata.

```rust
let db = Database::override("./data")
    .cache_size(8192)
    .sync_mode(SyncMode::Relaxed)
    .open()
    .await?;

// Uses overridden config for this session only.
// Next `Database::open("./data")` uses original stored config.
```

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
pub struct SafeConfig {
    pub cache_size: Option<usize>,
    pub sync_mode: Option<SyncMode>,
    pub wal_max_file_size: Option<u64>,
}

/// Builder for `db.local()`.
pub struct DatabaseLocal<'a> {
    db: &'a Database,
    config: SafeConfig,
}

/// Builder for `Database::override`.
pub struct DatabaseOverride {
    path: PathBuf,
    config: SafeConfig,
}

/// Builder for `Database::reconfigure`.
pub struct DatabaseReconfigure {
    path: PathBuf,
    config: SafeConfig,
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

### `local()` Implementation Notes

The `local()` method needs to:
1. Snapshot current config
2. Apply overrides
3. Run callback
4. Restore original config (even on error/panic)

For `sync_mode`, this likely means swapping the WAL writer's config. Need to ensure
thread-safety if other operations are concurrent. Options:
- Use `RwLock` on config and swap atomically
- Create a scoped "view" that intercepts calls
- For simplicity, require exclusive access during `local()` (no concurrent ops)

## Checklist

### Phase 1: `SafeConfig` Infrastructure

- [ ] Create `SafeConfig` struct in `database.rs` (or new `config.rs`)
- [ ] Implement `SafeConfig::apply_to(&self, StorageConfig) -> StorageConfig`
- [ ] Add internal method to swap/restore config on `Database` or `FileStorage`

### Phase 2: `Database::local`

- [ ] Create `DatabaseLocal<'a>` builder struct
- [ ] Implement `Database::local(&self) -> DatabaseLocal<'_>`
- [ ] Implement `DatabaseLocal::cache_size`, `sync_mode`, `wal_max_file_size` setters
- [ ] Implement `DatabaseLocal::run<F, Fut, T>()`:
  - [ ] Snapshot current config
  - [ ] Apply `SafeConfig` overrides to database internals
  - [ ] Run callback `f(&self.db)`
  - [ ] Restore original config (use `scopeguard` or manual drop guard)
  - [ ] Return callback result
- [ ] Add tests for `local`:
  - [ ] `local` with `SyncMode::Relaxed`, verify no fsync during callback
  - [ ] Verify config restored after callback completes
  - [ ] Verify config restored even if callback returns `Err`
  - [ ] Verify config restored even if callback panics

### Phase 3: `Database::override`

- [ ] Create `DatabaseOverride` builder struct
- [ ] Implement `Database::override(path) -> DatabaseOverride`
- [ ] Implement `DatabaseOverride::cache_size`, `sync_mode`, `wal_max_file_size` setters
- [ ] Implement `DatabaseOverride::open()`:
  - [ ] Read existing `MetadataPage` from disk
  - [ ] Convert to `StorageConfig` via `to_storage_config()`
  - [ ] Apply `SafeConfig` overrides
  - [ ] Open database with merged config (do NOT write back to metadata)
- [ ] Add tests for `override`:
  - [ ] Override `cache_size` and verify runtime behavior
  - [ ] Override `sync_mode` and verify WAL behavior
  - [ ] Verify original metadata unchanged after close

### Phase 4: `Database::reconfigure`

- [ ] Create `DatabaseReconfigure` builder struct
- [ ] Implement `Database::reconfigure(path) -> DatabaseReconfigure`
- [ ] Implement `DatabaseReconfigure::cache_size`, `sync_mode`, `wal_max_file_size` setters
- [ ] Implement `DatabaseReconfigure::apply()`:
  - [ ] Verify database is not currently open (check lock file or similar)
  - [ ] Read existing `MetadataPage`
  - [ ] Apply `SafeConfig` changes
  - [ ] Write updated `MetadataPage` back to disk
  - [ ] Sync to ensure durability
- [ ] Add tests for `reconfigure`:
  - [ ] Reconfigure `sync_mode`, reopen, verify new mode active
  - [ ] Reconfigure `cache_size`, reopen, verify new size
  - [ ] Verify error if database is open (if lock detection implemented)

### Phase 5: `Database::rebuild`

- [ ] Create `DatabaseRebuild` builder struct
- [ ] Implement `Database::rebuild(path) -> DatabaseRebuild`
- [ ] Implement setters for ALL config fields (safe and unsafe)
- [ ] Implement `DatabaseRebuild::output(path)` to set destination
- [ ] Implement `DatabaseRebuild::apply()`:
  - [ ] Open source database read-only
  - [ ] Create destination database with new config via `Database::builder().create()`
  - [ ] Iterate all globals in source (via registry)
  - [ ] For each global, iterate all keys via `collects`
  - [ ] Insert each key-value into destination within transaction(s)
  - [ ] Handle batching for large datasets (commit every N keys?)
  - [ ] Close both databases
- [ ] Add tests for `rebuild`:
  - [ ] Rebuild with different `min_degree`, verify data intact
  - [ ] Rebuild with different `max_pages`, verify constraint applied
  - [ ] Rebuild large dataset, verify no data loss
  - [ ] Error if `output` not specified
  - [ ] Error if `output` already exists (or add `force` flag?)

### Phase 6: Documentation and Cleanup

- [ ] Add doc comments to all public APIs
- [ ] Add examples in doc comments
- [ ] Update `CLAUDE.md` if needed
