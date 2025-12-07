# RUMPS Persistence TODOs

**NOTE**: For storage layer architecture and implementation details, see `docs/storage.org`.

---

## Future Considerations (Post-Goal 1)

These are the current next steps (post persistence and basic DB):

- **Query Language**: Parser and evaluator for MUMPS commands (rewrite old parser; see ./dsl.md for sketch in progress)
- **Compression**: Compress nodes/pages to save disk space
- **Encryption**: Optional encryption at rest
- **Networking**: Client-server protocol for remote access

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

| Config Field        | Safe to Change? | Reason                                    |
|---------------------|-----------------|-------------------------------------------|
| `cache_size`        | Yes             | Runtime-only, no data impact              |
| `sync_mode`         | Yes             | Durability policy, no data impact         |
| `wal_max_file_size` | Yes             | File rotation threshold only              |
| `max_pages`         | Conditional     | Can increase; decrease requires data fits |
| `max_memory_bytes`  | Conditional     | Can increase; decrease may cause eviction |
| `min_degree`        | No              | Affects B-tree node structure             |
| `page_size`         | No              | Would require full data rewrite           |

**Implementation Sketch**:

```rust
pub struct DatabaseMigration {
    path: PathBuf,
    cache_size: Option<usize>,
    sync_mode: Option<SyncMode>,
    wal_max_file_size: Option<u64>,
    max_pages: Option<Option<u64>>,
    max_memory_bytes: Option<Option<usize>>,
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

---

## Notes for README

### In-Memory Databases: ACI without D

When using `Database::in_memory()`, writes to globals still require transactions and provide full **ACI** guarantees:

- **Atomicity**: Writes are buffered until commit; rollback discards all changes
- **Consistency**: Conflict detection prevents write-write conflicts between concurrent transactions
- **Isolation**: Snapshot isolation ensures transactions see consistent state

The only difference from persistent databases is **no Durability** - data is lost when the database is dropped. Specifically:

- No WAL logging (the `if let (Name::Global(_), Some(storage))` guards skip WAL ops)
- `flush_with_txn()` is a no-op
- No disk I/O whatsoever

This is intentional and useful for:
- Testing (fast, isolated tests without disk cleanup)
- Caching (transactional semantics for in-memory data)
- Temporary workspaces

The "ceremony" of transactions for in-memory globals is still enforced and meaningful for concurrent access - you get proper multi-transaction coordination, just without persistence.

### Database Configuration is Immutable After Creation

When you create a persistent database with `Database::builder()...create(path)`, all configuration (cache size, sync mode, min degree, etc.) is stored in the database's metadata page. On subsequent opens, you **must** use `Database::open(path)` - the stored config is restored automatically.

```rust
// Create with custom config - stored in metadata page
let db = Database::builder()
    .min_degree(5)
    .cache_size(4096)
    .create("./data")
    .await?;
db.close().await?;

// Later: just open - config restored automatically
let db = Database::open("./data").await?;  // Correct

// DON'T try to re-create with different options!
let db = Database::builder()
    .min_degree(10)  // Different from stored config
    .create("./data")  // This would overwrite/corrupt!
    .await?;
```

This is why `DatabaseBuilder` has no `open()` method - there's no need to specify config when opening since it's already stored.

**Changing config on existing databases**: A `Database::migrate()` API is planned (see "Configuration Migration" above) but not yet implemented. For now, if you need different config, you must create a new database and manually migrate the data.
