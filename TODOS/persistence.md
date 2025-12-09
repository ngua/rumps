# RUMPS Persistence TODOs

**NOTE**: For storage layer architecture and implementation details, see `docs/storage.org`.

---

## Future Considerations (Post-Goal 1)

These are the current next steps (post persistence and basic DB):

- **Query Language**: Parser and evaluator for MUMPS commands (rewrite old parser; see ./dsl.md for sketch in progress)
- **Networking**: Client-server protocol for remote access

These are the potential next steps (post query layer and networking):

- **Compression**: Compress nodes/pages to save disk space (see [persistence/compression.md](persistence/compression.md))
- **Encryption**: Optional encryption at rest (see [persistence/encryption.md](persistence/encryption.md))

These are future considerations not yet planned:
- **Advanced Concurrency**: Lock-free data structures, optimistic concurrency control (current plan uses Mutex for commit serialization)
- **Multi-Version Concurrency Control (MVCC)**: Full MVCC for better read concurrency (current plan uses snapshot isolation)
- **Savepoints**: Nested transactions with partial rollback
- **Replication**: Multi-node deployment with data replication
- **Distributed Transactions**: Two-phase commit for multi-node transactions
- **Configuration Migration**: Update stored config on existing databases (see below)
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

### Configuration Migration (`Database::migrate`)

**Background**: All database configuration is persisted to the `MetadataPage` at creation
time. When reopening via `Database::open()`, the stored config is restored automatically.
The `DatabaseBuilder` intentionally has no `open()` method—there's no need to specify
config when opening since it's already stored.

However, users may want to **change** configuration on an existing database. This requires
a `migrate` API that updates stored config while validating safety constraints.

**Proposed API**:

```rust
// Update safe-to-change config on an existing database
Database::migrate("./data")
    .cache_size(4096)              // Safe: just update metadata
    .sync_mode(SyncMode::Immediate) // Safe: affects durability, not data layout
    .wal_max_file_size(128 * 1024 * 1024)
    .apply()
    .await?;

// Attempting to change layout-affecting config should error
Database::migrate("./data")
    .min_degree(10)  // ERROR: affects B-tree structure
    .apply()
    .await?;
// => Err(StorageError::InvalidOperation("min_degree cannot be changed..."))
```

**Safe vs Unsafe Configuration Changes**:

| Config Field        | Safe to Change? | Reason                                    | Notes                                              |
|---------------------|-----------------|-------------------------------------------|----------------------------------------------------|
| `cache_size`        | Yes             | Runtime-only, no data impact              |                                                    |
| `sync_mode`         | Yes             | Durability policy, no data impact         |                                                    |
| `wal_max_file_size` | Yes             | File rotation threshold only              |                                                    |
| `max_pages`         | Conditional     | Can increase; decrease requires data fits |                                                    |
| `max_memory_bytes`  | Conditional     | Can increase; decrease may cause eviction |                                                    |
| `min_degree`        | No              | Affects B-tree node structure             |                                                    |
| `PAGE_SIZE`         | N/A             | Would require full data rewrite           | This is a compile-time constant, not config option |

**Implementation Sketch**:

```rust
pub struct DatabaseMigration {
    path: PathBuf,
    cache_size: Option<usize>,
    sync_mode: Option<SyncMode>,
    wal_max_file_size: Option<u64>,
    max_pages: Option<Option<u64>>,
    max_memory_bytes: Option<Option<usize>>,
    // For future `Database::rebuild` support, to avoid duplicating
    // types
    min_degree: Option<usize>,
}

impl Database {
    pub fn migrate(path: impl AsRef<Path>) -> DatabaseMigration {
        DatabaseMigration {
            path: path.as_ref().to_path_buf(),
            cache_size: None,
            sync_mode: None,
            wal_max_file_size: None,
            max_pages: None,
            max_memory_bytes: None,
            min_degree: None,
        }
    }
}

impl DatabaseMigration {
    pub fn cache_size(mut self, sz: usize) -> Self {
        self.cache_size = Some(sz);
        self
    }

    // ... other setters ...

    pub async fn apply(self) -> Result<()> {
        // 1. Open storage engine (no DB, just raw access)
        // 2. Read current MetadataPage
        // 3. Validate changes are safe
        //  - E.g. `min_degree` is never safe for `migrate`,
        //    this requires `Database::rebuild`
        // 4. Update MetadataPage fields
        // 5. Write updated MetadataPage to disk
        // 6. Close storage
    }
}
```

**Future Extension**: For layout-affecting changes (`min_degree`, `page_size`), a separate
`Database::rebuild()` API could perform a full data migration:

```rust
// Full rebuild with new layout (expensive, rewrites all data)
Database::rebuild("./data")
    .min_degree(5)
    .page_size(8192)
    .output("./data_new")  // Write to new location
    .apply()
    .await?;
```

This would iterate all data, write to a new database file with the new config, then
optionally swap paths. This is a major operation and should be rare.
