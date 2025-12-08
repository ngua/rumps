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

### What is a "variable"?

In most languages, a "variable" is a name bound to a single value. In RUMPS (as with MUMPS), a variable is a *name bound to an entire tree*. The name (`^PATIENT` or `TEMP`) identifies the tree root; subscripts (`(123, "NAME")`) navigate to *nodes* within it:

```
SET ^P = "root"              ; value at root node (empty path)
SET ^P(1) = "child"          ; value at node [1]
SET ^P(1,"A") = "deeper"     ; value at node [1, "A"]
```

All three are "the variable `^P`" — nodes in the same tree. A node can hold a value *and* have children simultaneously (`^P(1)` stores `"child"` while `^P(1,"A")` exists beneath it).

#### Globals vs. locals

Globals (`^NAME`) persist to disk via WAL + page cache; Locals (`NAME`) are memory-only. Both use the *same* B-tree implementation.

### Core Properties of RUMPS data model

(See `docs/storage.org` for full architectural details.)

1. **Data is Always Sorted**: Keys are stored in *collation order* within the B-tree. Iteration (e.g. `ORDER`, `COLLECTS`) yields keys in sorted sequence without explicit `ORDER BY`, i.e. no post-hoc sorting is needed or desired.
2. **Sparse by Nature**: No predefined schema means absent keys simply don't exist—no null-padding or wasted space. (See above.)
3. **Hierarchical Keys**: Multi-dimensional subscripts (`^VAR(a,b,c)`) form a natural tree. Parent-child relationships are implicit in key structure.
4. **Schema-less/Schema-flexible**: Structure emerges from usage. Add new "fields" (subscripts) at any time without migrations.
5. **Unified Storage Model**: One data structure (the global) serves all purposes—no separate table definitions, index tables, or join tables.
6. **Nodes Have Dual Nature**: A single node can store a scalar value *and* have child subscripts. `^PATIENT(123)` can hold `"ACTIVE"` while `^PATIENT(123,"NAME")` holds `"John"`. This is tracked via a `has_descendants` flag on each node.
7. **Extended Collation Order**:
   - Booleans: `false` < `true`
   - Numbers: in numeric order (`-10` < `0` < `10` < `100`)
   - Strings: in lexicographic order
   - JSON: string representation in lexicographic order
   - Cross-type: `booleans` < `numbers` < `chars` < `strings` < `json`
9. **Persistent B-tree Backing**: Sequential access and prefix scans are I/O-efficient because keys are physically co-located. RUMPS uses a B-tree (not B+-tree) so internal nodes can hold data—matching MUMPS semantics where intermediate nodes have values.

### RUMPS vs SQL: Conceptual Differences

| Aspect             | RUMPS                                                     | SQL                                           |
|--------------------|-----------------------------------------------------------|-----------------------------------------------|
| **Data Model**     | Hierarchical sparse trees (globals)                       | Relational tables with rows/columns           |
| **Schema**         | Schema-less; structure emerges from keys                  | Strict schema defined upfront                 |
| **Query Model**    | Tree-navigation primitives (`ORDER`, `QUERY`, `COLLECT`)  | Declarative set operations (`SELECT`, `JOIN`) |
| **Keys**           | Compound subscript paths (e.g., `^PATIENT(123,"NAME")`)   | Primary/foreign key relationships             |
| **Joins**          | Hierarchical nesting eliminates most joins                | Explicit `JOIN` operations required           |
| **Node Structure** | Nodes can have *both* a value AND children simultaneously | Cells hold single values only                 |
| **Storage**        | Unified key-value B-tree; one structure for everything    | Separate tables, each with own storage        |
| **Iteration**      | Stream-based (`COLLECT`) or tree traversal (`ORDER`)      | Set-based `WHERE` clauses                     |
| **Collation**      | Extended: `bool < number < string` (numeric `10` < `100`) | Type-specific; string `"10"` > `"100"`        |
| **Namespaces**     | Globals (`^NAME`) vs Locals (`NAME`)                      | Tables within schemas/databases               |

### RUMPS vs SQL: Similarities

- **ACID Transactions**: Both provide atomicity, consistency, isolation, durability
- **Aggregation**: Both support `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`
- **Filtering**: RUMPS `WHERE`/`FILTER` ≈ SQL `WHERE`
- **Projection**: RUMPS `SELECT` ≈ SQL `SELECT` (transformation)
- **Grouping**: Both support `GROUP BY` operations
- **Sorted Access**: Both can iterate data in sorted order (RUMPS by default; SQL via `ORDER BY`)
- **Concurrent Access**: Both handle multiple simultaneous readers/writers
- **Indexing**: RUMPS's sorted keys act as implicit indices; SQL has explicit index creation

### Sparse Storage: No Schema, No NULLs

The "sparsity" difference between RUMPS and SQL comes down to how absent data is represented.

**SQL (dense)**

When you define a table, every row has every column:

```sql
CREATE TABLE patients (
    id INT,
    name VARCHAR(100),
    dob DATE,
    phone VARCHAR(20),
    fax VARCHAR(20),
    emergency_contact VARCHAR(100),
    -- ... 50 more columns
);
```

If patient 123 has no fax number, that row still has a `fax` column—it just holds `NULL`. The storage engine must track that NULL somehow (typically a null bitmap or sentinel value). If you have 50 columns but most patients only use 10, you're still paying storage overhead for 40 NULLs per row.

**RUMPS (sparse)**

There's no schema. A "record" is just keys that share a prefix:

```
^PATIENT(123,"NAME") = "John"
^PATIENT(123,"DOB") = "1980-01-15"
^PATIENT(123,"PHONE") = "555-1234"
```

If patient 123 has no fax, there's simply *no key* `^PATIENT(123,"FAX")`. Nothing is stored. Zero bytes. The key doesn't exist in the B-tree.

Patient 456 might have completely different fields:

```
^PATIENT(456,"NAME") = "Jane"
^PATIENT(456,"FAX") = "555-9999"
^PATIENT(456,"EMERGENCY","NAME") = "Bob"
^PATIENT(456,"EMERGENCY","PHONE") = "555-0000"
```

**The sparse matrix analogy**

Think of it like sparse vs dense matrices:

- *Dense matrix*: Store every cell, including zeros → SQL with NULLs
- *Sparse matrix*: Only store non-zero entries → RUMPS

This is why MUMPS was historically popular for healthcare—patient records are notoriously sparse and irregular. One patient has 3 fields, another has 300, another has nested structures 5 levels deep. No schema migrations needed.

### When RUMPS/MUMPS-Style DBs Are a Good Fit

**Hierarchical / Nested Data**
- Medical records: `^PATIENT(id,"VISITS",date,"DIAGNOSIS")`
- Org charts: `^ORG(dept,team,employee)`
- File system metadata: `^FS(path,component,...)`
- Configuration trees: `^CONFIG(section,subsection,key)`

**Sparse / Irregular Data**
- Entities with many optional fields (only present keys consume space)
- Survey responses where most questions are skipped
- Feature flags per user/tenant

**Evolving / Unknown Schema**
- Rapid prototyping where structure isn't finalized
- Multi-tenant systems with tenant-specific fields
- Log/event storage with varying payloads

**Key-Range Workloads**
- Time-series: `^LOG(2025,01,15,timestamp)` — efficient date-range scans
- Audit trails: `^AUDIT(entity,action,time)`
- Leaderboards / sorted sets

**Document-Like Access Patterns**
- Retrieve entire subtrees in one traversal
- Nested JSON-like structures stored directly
- No ORM impedance mismatch

**Low-Ceremony Applications**
- Embedded-in-application databases (à la SQLite) for CLI tools or services
- Prototypes that may evolve into larger systems
- Situations where SQL DDL overhead isn't justified

*Note*: RUMPS requires `std` and `tokio`—not suitable for `no_std` / embedded device targets.

### When Something Simpler Is Better

RUMPS has overhead from its B-tree structure, async runtime, and transaction machinery. For some use cases, simpler tools win:

**Flat Key-Value Storage**
- If you just need `get(key) -> Option<Value>` with no hierarchy or range queries
- A `DashMap`, `HashMap` + `RwLock`, or even `sled` may be faster and simpler
- RUMPS shines when your keys have *structure* you want to query

**Simple Memoization / Function Caching**
- Caching pure function results by input hash
- No need for transactions, range queries, or hierarchical access
- Use `moka`, `cached`, or a simple LRU cache crate

**TTL-Based Expiration**
- RUMPS has no built-in key expiration or eviction policies
- Redis, `moka`, or similar caches handle TTL natively
- You *can* implement time-based cleanup via `$ORDER` iteration, but it's manual

**Nanosecond-Scale Hot Paths**
- B-tree traversal and async machinery add overhead vs. lock-free hash maps
- For millions of tiny ops/sec on flat data, raw `DashMap` wins
- RUMPS is better suited for microsecond-scale structured operations

**Relational / Tabular Data with Complex Joins**
- MUMPS-style DBs excel at hierarchical traversal, not relational algebra
- If you need multi-table joins, aggregations, or SQL semantics, use SQLite/Postgres
- RUMPS *can* model relations, but you're hand-rolling the join logic

**Embedded / `no_std` Targets**
- RUMPS requires `std` and `tokio`
- For microcontrollers or `no_std` environments, look elsewhere

**When You Need a Battle-Tested Production DB**
- RUMPS is experimental and under active development
- For production workloads requiring proven reliability, use established alternatives

### ACID Guarantees

RUMPS provides full ACID semantics for all writes to globals. (See `docs/storage.org` for implementation details.)

**Atomicity**
- All operations within a transaction succeed together or fail together
- Writes are buffered in the WAL with undo information (`old_data`)
- On rollback, changes are reverted using the stored undo data
- No partial writes are ever visible to other transactions

**Consistency**
- Conflict detection prevents write-write conflicts between concurrent transactions
- The database is never left in a partial or corrupted state
- B-tree invariants (sorted keys, balanced nodes) are maintained through all operations
- Ancestor nodes automatically updated when descendants change (`has_descendants` flag)

**Isolation**
- Snapshot isolation by default: each transaction sees a consistent snapshot of data as of transaction start
- Readers never block writers; writers never block readers
- Concurrent transactions operate on independent snapshots
- Conflicts detected at commit time, not during execution

**Durability**
- Write-Ahead Logging (WAL) ensures committed transactions survive crashes
- Sequence: WAL write → `fsync` → commit acknowledged → (later) flush to data file
- Recovery replays committed transactions from WAL on startup
- Configurable sync modes trade durability for performance:
  | Mode        | Behavior                       | Durability     |
  |-------------|--------------------------------|----------------|
  | `Immediate` | `fsync` after every WAL write  | Highest        |
  | `OnCommit`  | `fsync` on transaction commit  | Good (default) |
  | `Periodic`  | `fsync` every N ms / N records | Lower          |

**Transaction Requirements**
- All writes to globals (`^NAME`) *must* occur within a transaction
- Locals (`NAME`) are memory-only and don't require transactions
- Attempting to write a global outside a transaction is a runtime error

```rust
// Correct: global write inside transaction
db.transaction(|txn| async move {
    txn.set(&Name::Global("PATIENT".into()), &key, val).await?;
    Ok(())
}).await?;

// Locals don't need transactions (no persistence)
db.set_local(&Name::Local("TEMP".into()), &key, val).await?;
```

### RUMPS-Specific Advantages

(See `docs/storage.org` for implementation details and `TODOS/dsl.md` for DSL design.)

**Async-Native from the Ground Up**
- Built on Rust's async ecosystem (`tokio`)
- Every I/O operation—reads, writes, WAL flushes—is non-blocking
- No bolted-on async wrappers; async is baked in from page cache to transaction commit

**Modern Type System**
- Proper boolean type
- Numeric collation that makes sense (`10` < `100`, not `"10"` > `"100"`)
- Optional runtime type hints in the DSL

**WAL-Based Durability with Configurable Sync Modes**
- `Immediate`: `fsync` after every WAL write (highest durability)
- `OnCommit`: `fsync` only on transaction commit (good balance)
- `Periodic`: `fsync` every N ms or N records (best throughput)
- Group commit amortizes expensive `fsync` across multiple operations

**LRU Page Cache**
- In-memory cache avoids disk I/O for hot data
- Pages pinned during active use; LRU eviction when memory pressure
- Writes go to cache first, flushed to disk on checkpoint

**Snapshot Isolation by Default**
- Transactions see a consistent snapshot
- Concurrent readers never block writers
- Conflict detection prevents write-write conflicts

**Rust Performance Guarantees**
- Zero-cost abstractions, no GC pauses, predictable latency
- Memory safety without runtime overhead

**Generous Limits**
- Max database size: ~32 TiB with default 4 KB pages (configurable via `RUMPS_PAGE_SIZE`)
- No hard limit on number of globals (registry pages chain automatically)
- Bitmap allocator with three-level indirection (similar to Unix inodes)

**Two-Phase Commit-Ready Architecture**
- Transaction model designed with future distributed/replicated deployments in mind

**Functional/Declarative DSL** (planned)
- The `COLLECT` primitive provides lazy, composable stream processing
- No manual iteration variables or loop counters
- Stream operations (`WHERE`, `SELECT`, `TAKE`, `AGGREGATE`) compose naturally

**Three-Layer Architecture**
- *Database Layer*: Logical coordination, namespace mapping, WAL logging
- *B-Tree Layer*: Pure tree operations on `NodeId`s, no knowledge of names or I/O
- *Storage Layer*: Page cache (LRU eviction), WAL files, bitmap allocator

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

#### RUMPS as an In-Memory Cache

Beyond testing, in-memory RUMPS can serve as a structured cache layer with benefits you don't get from simpler caches:

**Hierarchical/Nested Cache Data**
- Cache user sessions with nested permissions: `^SESSION(user_id,"PERMS",resource)`
- Config trees, feature flags per tenant, nested JSON-like structures
- Prefix iteration retrieves entire subtrees without serialization overhead
- Compare to Redis where you'd flatten keys (`user:123:perms:read`) or serialize entire objects

**Transactional Cache Updates**
- Update multiple related cache entries atomically
- Invalidate a user's session *and* their permissions *and* their rate-limit counters in one operation
- Rollback on failure—no partial cache state

**Range Queries on Cached Data**
- Leaderboards: iterate top N scores via `$ORDER`
- Time-windowed rate limiting: `^RATE(user_id,timestamp)` with efficient range scans
- Sorted sets without separate index structures
- All O(log n) + scan, backed by B-tree ordering

**API Uniformity**
- Same code works for in-memory locals and persistent globals
- Easy to "promote" hot data to persistence or vice versa
- Prototype with in-memory, switch to persistent by changing one line

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
