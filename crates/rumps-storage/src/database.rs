//! Database layer for RUMPS.
//!
//! The [`Database`] struct provides the main entry point for interacting with
//! RUMPS storage. It supports two namespaces—persistent **globals** (`^NAME`)
//! and ephemeral **locals** (`NAME`)—and requires explicit transactions for
//! writes to globals.
//!
//! # Quick Start
//!
//! ```
//! # tokio_test::block_on(async {
//! use rumps_storage::Database;
//! use rumps_types::{global, local, key, Value};
//!
//! // Create an in-memory database (no disk I/O)
//! let db = Database::in_memory()?;
//!
//! // Locals can be set directly (no transaction needed)
//! db.set(&local!("TEMP"), &key![1, "NAME"], Value::from("Alice")).await?;
//!
//! // Read back the value
//! let val = db.get(&local!("TEMP"), &key![1, "NAME"]).await?;
//! assert_eq!(val, Some(Value::from("Alice")));
//!
//! // Globals require transactions
//! db.transaction(|txn| async move {
//!     txn.set(&global!("PATIENT"), &key![123, "NAME"], Value::from("Bob")).await?;
//!     txn.set(&global!("PATIENT"), &key![123, "AGE"], Value::from(42)).await?;
//!     Ok(())
//! }).await?;
//!
//! // Read from global (no transaction needed for reads)
//! let name = db.get(&global!("PATIENT"), &key![123, "NAME"]).await?;
//! assert_eq!(name, Some(Value::from("Bob")));
//! # Ok::<(), rumps_storage::Error>(())
//! # });
//! ```
//!
//! # Persistence
//!
//! For disk-backed storage, use [`Database::create()`] for new databases
//! or [`Database::open()`] for existing ones:
//!
//! ```no_run
//! # tokio_test::block_on(async {
//! use rumps_storage::Database;
//!
//! // Create a new persistent database
//! let db = Database::create("./my_data").await?;
//!
//! // Later, reopen it
//! let db = Database::open("./my_data").await?;
//! # Ok::<(), rumps_storage::Error>(())
//! # });
//! ```
//!
//! # Advanced Configuration
//!
//! Use [`DatabaseBuilder`] for fine-grained control over cache size, WAL
//! settings, and B-tree parameters:
//!
//! ```no_run
//! # tokio_test::block_on(async {
//! use rumps_storage::{Database, SyncMode};
//!
//! let db = Database::builder()
//!     .cache_size(4096)              // 4096 pages (~16 MiB)
//!     .sync_mode(SyncMode::Immediate)
//!     .min_degree(5)
//!     .create("./data")
//!     .await?;
//! # Ok::<(), rumps_storage::Error>(())
//! # });
//! ```
//!
//! # Two Namespaces
//!
//! - **Globals** (`^NAME`): Persistent, backed by disk storage. All writes
//!   MUST occur within a transaction.
//! - **Locals** (`NAME`): Ephemeral, memory-only. Can be modified directly
//!   without transactions.
//!
//! # Architecture Details
//!
//! Internally, `Database` maps variable names ([`Name`]) to B-tree root
//! [`NodeId`]s. The B-tree itself operates purely on `NodeId`s, with no
//! knowledge of variable names.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │  Database                       BTree                                   │
//! │  ────────                       ─────                                   │
//! │  roots: BTreeMap<Name, NodeId>  nodes: HashMap<NodeId, Node>            │
//! │  (lazy-loaded from registry)    (no names, no roots!)                   │
//! │         │                              │                                │
//! │         │ lookup/create root           │ load/save nodes                │
//! │         ▼                              ▼                                │
//! │    ┌─────────┐                   ┌───────────┐                          │
//! │    │ NodeId  │ ───────────────── │  BTree    │                          │
//! │    └─────────┘                   │  *_at()   │                          │
//! │                                  └───────────┘                          │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! [`Name`]: rumps_types::Name

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, RwLock as StdRwLock};

use futures::stream::{self, Stream, StreamExt, TryStreamExt};
use rumps_types::{DataStatus, Key, Name, Result, Value};
use tokio::sync::RwLock;

use crate::btree::{BTree, BTreeBuilder, BTreeStats};
use crate::engine::{
    AsyncStorageEngine, FileStorageEngine, StorageConfig, StorageMetadata,
};
use crate::error::StorageError;
use crate::node::{NodeData, NodeId};
use crate::page::PageCacheStats;
use crate::transaction::{
    Transaction, TransactionBuilder, TransactionContext, TransactionId,
    TransactionManager, TransactionTimestamp,
};
use crate::wal::{SyncMode, WalOp, WalReader, WalRecord};

/// Diagnostic statistics for a [`Database`].
///
/// Aggregates statistics from the B-tree, page cache, storage engine, and
/// transaction manager. Use [`Database::debug()`] to obtain this.
#[derive(Debug, Clone)]
pub struct DatabaseStats {
    /// Number of root variables (globals + locals) currently cached.
    pub root_count: usize,
    /// B-tree statistics.
    pub btree: BTreeStats,
    /// Page cache statistics (persistent DBs only).
    pub cache: Option<PageCacheStats>,
    /// Storage metadata (persistent DBs only).
    pub storage: Option<StorageMetadata>,
    /// Number of active transactions.
    pub active_txns: usize,
    /// Whether this is an in-memory database.
    pub in_memory: bool,
}

/// Builder for creating [`Database`] instances with custom configuration.
///
/// For most use cases, prefer the simple constructors:
/// - [`Database::in_memory()`] - ephemeral in-memory database
/// - [`Database::create()`] - new persistent database with defaults
/// - [`Database::open()`] - open existing persistent database
///
/// Use the builder for advanced configuration when **creating** a database:
///
/// ```no_run
/// # tokio_test::block_on(async {
/// use rumps_storage::{Database, SyncMode};
///
/// let db = Database::builder()
///     .cache_size(4096)              // 4096 pages (~16 MiB)
///     .sync_mode(SyncMode::Immediate)
///     .min_degree(5)                 // B-tree branching factor
///     .create("./data")
///     .await?;
///
/// // Later, just open - config is restored automatically
/// let db = Database::open("./data").await?;
/// # Ok::<(), rumps_storage::Error>(())
/// # });
/// ```
///
/// For in-memory databases with custom B-tree settings:
///
/// ```
/// use rumps_storage::Database;
///
/// let db = Database::builder()
///     .min_degree(5)
///     .in_memory()?;
/// # Ok::<(), rumps_storage::Error>(())
/// ```
///
/// # Configuration Persistence
///
/// All configuration is persisted to the database's metadata page when
/// creating. On reopen via [`Database::open()`], the stored configuration
/// is restored automatically.
///
/// # Why No `open()` Method?
///
/// The builder intentionally only has [`create()`] and [`in_memory()`].
/// Since all configuration is persisted at creation time and restored
/// automatically on open, there's no need to specify config when opening.
/// Use [`Database::open()`] directly to open an existing database.
///
/// [`create()`]: Self::create
/// [`in_memory()`]: Self::in_memory
#[derive(Debug, Clone, Default)]
pub struct DatabaseBuilder {
    storage_config: StorageConfig,
    min_degree: Option<usize>,
    max_memory_bytes: Option<usize>,
}

impl DatabaseBuilder {
    /// Sets the page cache size in pages.
    ///
    /// Default: `1024` pages (~4 MiB at 4KB page size).
    pub fn cache_size(mut self, pages: usize) -> Self {
        self.storage_config.cache_size = pages;
        self
    }

    /// Sets maximum database size in pages.
    ///
    /// `None` means unlimited growth. Default: `None`.
    pub fn max_pages(mut self, pages: u64) -> Self {
        self.storage_config.max_pages = Some(pages);
        self
    }

    /// Sets WAL sync mode.
    ///
    /// Default: [`SyncMode::OnCommit`].
    pub fn sync_mode(mut self, mode: SyncMode) -> Self {
        self.storage_config.wal_config.sync_mode = mode;
        self
    }

    /// Sets WAL file rotation size in bytes.
    ///
    /// Default: `64` MiB.
    pub fn wal_max_file_size(mut self, bytes: u64) -> Self {
        self.storage_config.wal_config.max_file_size = bytes;
        self
    }

    /// Sets B-tree minimum degree (branching factor).
    ///
    /// Nodes contain `t-1` to `2t-1` keys. Default: `3`.
    pub fn min_degree(mut self, deg: usize) -> Self {
        self.min_degree = Some(deg);
        self
    }

    /// Sets memory limit for in-memory operations.
    ///
    /// Default: unlimited.
    pub fn max_memory_bytes(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = Some(bytes);
        self
    }

    /// Creates a new in-memory database with these settings.
    ///
    /// In-memory databases ignore storage configuration (cache size, WAL, etc.)
    /// but respect B-tree settings (`min_degree`, `max_memory_bytes`).
    pub fn in_memory(self) -> Result<Database> {
        let mut builder = BTreeBuilder::default();
        if let Some(deg) = self.min_degree {
            builder = builder.min_degree(deg);
        }
        if let Some(bytes) = self.max_memory_bytes {
            builder = builder.max_memory_bytes(bytes);
        }
        Ok(Database::with_btree(
            Arc::new(builder.build()?),
            Arc::new(TransactionManager::default()),
        )?)
    }

    /// Creates a new persistent database at the specified path.
    ///
    /// All configuration is persisted to the database's metadata page,
    /// so subsequent calls to [`Database::open()`] will restore the same
    /// configuration without needing to specify it again.
    pub async fn create(self, path: impl AsRef<Path>) -> Result<Database> {
        let deg = self.min_degree.unwrap_or(3) as u16;
        let storage = Arc::new(
            FileStorageEngine::create(
                path.as_ref(),
                self.storage_config,
                deg,
                self.max_memory_bytes,
            )
            .await?,
        );

        let mut builder = BTreeBuilder::default()
            .storage(Arc::clone(&storage) as Arc<dyn AsyncStorageEngine>)
            .min_degree(deg as usize);
        if let Some(bytes) = self.max_memory_bytes {
            builder = builder.max_memory_bytes(bytes);
        }

        Ok(Database {
            roots: Arc::new(RwLock::new(BTreeMap::new())),
            btree: Arc::new(builder.build()?),
            storage: Some(storage),
            txn_manager: Arc::new(TransactionManager::default()),
            closed: Arc::new(StdRwLock::new(false)),
        })
    }
}

/// Database providing namespace management over a B-tree.
///
/// `Database` maps variable names (`Name::Global` and `Name::Local`) to
/// B-tree root `NodeId`s. The B-tree itself operates only on `NodeId`s,
/// with no knowledge of variable names or the registry.
///
/// # Two Namespaces
///
/// - **Globals** (`^NAME`): Persistent, backed by disk storage. Roots are
///   lazy-loaded from the registry and cached in memory.
/// - **Locals** (`NAME`): Ephemeral, memory-only. Created on first access,
///   discarded when the database closes.
///
/// # Thread Safety
///
/// `Database` is designed for shared access via `Arc<Database>`. All
/// operations use interior mutability with `RwLock` for concurrent access.
#[derive(Clone)]
pub struct Database {
    /// Name → root `NodeId` mapping.
    ///
    /// For globals with persistent storage, entries are lazy-loaded from
    /// the registry on first access and cached here. For locals and
    /// in-memory databases, entries are created on demand.
    ///
    /// Uses `BTreeMap` for ordered iteration (MUMPS `$ORDER` over names).
    roots: Arc<RwLock<BTreeMap<Name, NodeId>>>,

    /// The underlying B-tree (operates on `NodeId`s only).
    btree: Arc<BTree>,

    /// Optional storage engine for persistence.
    ///
    /// When `Some`, globals are persisted to disk and lazy-loaded from
    /// the registry. When `None`, all data is in-memory only.
    storage: Option<Arc<crate::engine::FileStorageEngine>>,

    /// Transaction manager for coordinating concurrent transactions.
    pub(crate) txn_manager: Arc<TransactionManager>,

    /// Whether [`close()`] has been called.
    ///
    /// Used by `Drop` to avoid redundant cleanup. Uses `StdRwLock`
    /// (not tokio) so it can be checked synchronously in `Drop`.
    ///
    /// [`close()`]: Self::close
    closed: Arc<StdRwLock<bool>>,
}

// Public API
impl Database {
    /// Returns a builder for advanced configuration.
    ///
    /// For most use cases, prefer [`in_memory()`], [`create()`], or [`open()`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::Database;
    ///
    /// let db = Database::builder()
    ///     .min_degree(5)
    ///     .in_memory()?;
    /// # Ok::<(), rumps_storage::Error>(())
    /// ```
    ///
    /// [`in_memory()`]: Self::in_memory
    /// [`create()`]: Self::create
    /// [`open()`]: Self::open
    pub fn builder() -> DatabaseBuilder {
        DatabaseBuilder::default()
    }

    /// Creates a new in-memory database.
    ///
    /// This database has no persistent storage - all data exists only in
    /// memory and is discarded when the database is dropped. Useful for
    /// caching, temporary workspaces, or testing.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{local, key, Value};
    ///
    /// let db = Database::in_memory()?;
    ///
    /// // Use as a cache for computed values
    /// db.set(&local!("CACHE"), &key!["user", 123], Value::from("cached_data")).await?;
    ///
    /// let val = db.get(&local!("CACHE"), &key!["user", 123]).await?;
    /// assert_eq!(val, Some(Value::from("cached_data")));
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub fn in_memory() -> Result<Self> {
        Ok(Self::with_btree(
            Arc::new(BTreeBuilder::default().build()?),
            Arc::new(TransactionManager::default()),
        )?)
    }

    /// Creates a new persistent database at the specified path.
    ///
    /// Initializes a new database with disk-backed storage and default
    /// configuration. Globals will be persisted to disk, while locals
    /// remain in-memory only.
    ///
    /// The default configuration is persisted to the database's metadata,
    /// so subsequent calls to [`open()`] will restore the same settings.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{global, key, Value};
    ///
    /// let db = Database::create("./my_data").await?;
    ///
    /// // Globals are persisted to disk
    /// db.transaction(|txn| async move {
    ///     txn.set(&global!("CONFIG"), &key!["version"], Value::from(1)).await?;
    ///     Ok(())
    /// }).await?;
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    ///
    /// [`open()`]: Self::open
    pub async fn create(path: impl AsRef<Path>) -> Result<Self> {
        let cfg = StorageConfig::default();
        let deg = 3u16; // default min_degree
        let storage = Arc::new(
            FileStorageEngine::create(path.as_ref(), cfg, deg, None).await?,
        );

        let btree = Arc::new(
            BTreeBuilder::default()
                .storage(Arc::clone(&storage)
                    as Arc<dyn crate::engine::AsyncStorageEngine>)
                .min_degree(deg as usize)
                .build()?,
        );

        Ok(Self {
            roots: Arc::new(RwLock::new(BTreeMap::new())),
            btree,
            storage: Some(storage),
            txn_manager: Arc::new(TransactionManager::default()),
            closed: Arc::new(StdRwLock::new(false)),
        })
    }

    /// Opens an existing persistent database from the specified path.
    ///
    /// Loads the database from disk, runs WAL recovery, and replays committed
    /// operations. Globals are lazy-loaded from the registry on first access.
    ///
    /// The configuration is restored from the database's stored metadata,
    /// so no configuration parameters are needed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{global, key};
    ///
    /// let db = Database::open("./my_data").await?;
    ///
    /// // Read previously stored data
    /// let version = db.get(&global!("CONFIG"), &key!["version"]).await?;
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let storage = Arc::new(FileStorageEngine::open(path.as_ref()).await?);

        // Use stored config from metadata
        let deg = storage.min_degree().await as usize;
        let mut builder = BTreeBuilder::default()
            .storage(Arc::clone(&storage)
                as Arc<dyn crate::engine::AsyncStorageEngine>)
            .min_degree(deg);
        if let Some(bytes) = storage.max_memory_bytes().await {
            builder = builder.max_memory_bytes(bytes);
        }

        let btree = Arc::new(builder.build()?);

        let db = Self {
            roots: Arc::new(RwLock::new(BTreeMap::new())),
            btree,
            storage: Some(Arc::clone(&storage)),
            txn_manager: Arc::new(TransactionManager::default()),
            closed: Arc::new(StdRwLock::new(false)),
        };

        // Run WAL recovery and replay committed operations
        db.recover(path.as_ref()).await?;

        Ok(db)
    }

    /// Closes the database, flushing all data and releasing resources.
    ///
    /// This method consumes `self` to ensure the database cannot be used
    /// after closing. All pending writes are flushed to disk before closing.
    ///
    /// Note that calling this explicitly is essentially the same as `drop`ping
    /// the DB; once the DB is closed, it's `Drop` implementation will not
    /// run (to avoid rundundancy)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    ///
    /// let db = Database::create("./my_data").await?;
    /// // ... use database ...
    /// db.close().await?;
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn close(self) -> crate::error::Result<()> {
        // Mark as closed so Drop doesn't do redundant work.
        // If poisoned, proceed anyway - the flag is still usable.
        *self.closed.write().unwrap_or_else(|e| e.into_inner()) = true;

        self.flush().await?;

        if let Some(storage) = self.storage.as_ref() {
            storage.shutdown_sync_task();
        }

        Ok(())
    }

    /// Sets a value in the database.
    ///
    /// **Note**: Writes to globals require a transaction. Use `db.transaction()`
    /// or create a `Transaction` manually. Direct calls for globals will error.
    ///
    /// For locals (in-memory only), this modifies the B-tree directly.
    ///
    /// # Errors
    ///
    /// Returns `GlobalRequiresTransaction` if attempting to write to a global
    /// variable without a transaction.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{local, global, key, Value};
    ///
    /// let db = Database::in_memory()?;
    ///
    /// // For locals (no transaction needed):
    /// db.set(&local!("TEMP"), &key![1], Value::from("test")).await?;
    ///
    /// // For globals (use transaction):
    /// db.transaction(|txn| async move {
    ///     txn.set(&global!("PATIENT"), &key![123], Value::from("Bob")).await?;
    ///     Ok(())
    /// }).await?;
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn set(&self, name: &Name, key: &Key, val: Value) -> Result<()> {
        // Globals require transactions
        if matches!(name, Name::Global(_)) {
            Err(StorageError::GlobalRequiresTransaction.into())
        } else {
            // Locals can be modified directly (no WAL, no persistence)
            let root = self.ensure_root(name).await?;

            let ctx = TransactionContext::new(
                TransactionId::IMPLICIT,
                TransactionTimestamp::from(0),
            );

            let new_root = self.btree.set_at(root, key, val, &ctx).await?;

            if new_root != root {
                self.update_root(name, new_root).await?;
            }

            Ok(())
        }
    }

    /// Gets a value from the database (read-only, no WAL logging).
    ///
    /// Returns `None` if the variable or key doesn't exist.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{local, key, Value};
    ///
    /// let db = Database::in_memory()?;
    /// db.set(&local!("DATA"), &key![1, "NAME"], Value::from("Alice")).await?;
    ///
    /// let name = db.get(&local!("DATA"), &key![1, "NAME"]).await?;
    /// assert_eq!(name, Some(Value::from("Alice")));
    ///
    /// let missing = db.get(&local!("DATA"), &key![999]).await?;
    /// assert_eq!(missing, None);
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
        let opt_root = self.get_root(name).await?;
        Ok(match opt_root {
            Some(root) => self.btree.get_at(root, key, None).await?,
            None => None,
        })
    }

    /// Checks the data status of a node (MUMPS `$DATA`).
    ///
    /// Returns information about whether a node has a value and/or descendants.
    /// Read-only operation - no WAL logging.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{local, key, Value, DataStatus};
    ///
    /// let db = Database::in_memory()?;
    /// db.set(&local!("DATA"), &key![1, "A"], Value::from("val")).await?;
    ///
    /// // Key `[1]` has descendants but no value
    /// let status = db.data(&local!("DATA"), &key![1]).await?;
    /// assert_eq!(status, DataStatus::HasDescendants);
    ///
    /// // Key `[1, "A"]` has a value
    /// let status = db.data(&local!("DATA"), &key![1, "A"]).await?;
    /// assert_eq!(status, DataStatus::HasValue);
    ///
    /// // Non-existent key
    /// let status = db.data(&local!("DATA"), &key![999]).await?;
    /// assert_eq!(status, DataStatus::NoData);
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus> {
        let opt_root = self.get_root(name).await?;
        Ok(match opt_root {
            Some(root) => self.btree.data_at(root, key, None).await?,
            None => DataStatus::NoData,
        })
    }

    /// Returns the next key in lexicographic order (MUMPS `$ORDER`).
    ///
    /// Pass `None` as `after` to get the first key. Returns `None` when
    /// there are no more keys. Read-only operation - no WAL logging.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{local, key, Value};
    ///
    /// let db = Database::in_memory()?;
    /// db.set(&local!("DATA"), &key![1], Value::from("a")).await?;
    /// db.set(&local!("DATA"), &key![2], Value::from("b")).await?;
    /// db.set(&local!("DATA"), &key![10], Value::from("c")).await?;
    ///
    /// // Get first key
    /// let first = db.order(&local!("DATA"), None).await?;
    /// assert_eq!(first, Some(key![1]));
    ///
    /// // Get next key after `[1]`
    /// let next = db.order(&local!("DATA"), Some(&key![1])).await?;
    /// assert_eq!(next, Some(key![2]));
    ///
    /// // Numeric ordering: `2 < 10`
    /// let next = db.order(&local!("DATA"), Some(&key![2])).await?;
    /// assert_eq!(next, Some(key![10]));
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn order(
        &self,
        name: &Name,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        let opt_root = self.get_root(name).await?;
        Ok(match opt_root {
            Some(root) => self.btree.order_at(root, after, None).await?,
            None => None,
        })
    }

    /// Returns a list of all global variable names in the database.
    ///
    /// For persistent databases, this returns globals from the registry.
    /// For in-memory databases, this returns globals from the cached roots.
    /// Names are returned in sorted order.
    pub async fn list_globals(&self) -> Vec<String> {
        match self.storage.as_ref() {
            Some(s) => s
                .registry_entries()
                .await
                .into_iter()
                .map(|(name, _)| name)
                .collect(),
            None => self
                .roots
                .read()
                .await
                .keys()
                .filter_map(|n| match n {
                    Name::Global(g) => Some(g.clone()),
                    Name::Local(_) => None,
                })
                .collect(),
        }
    }

    /// Creates a stream of entries from the tree (RUMPS `$COLLECT`).
    ///
    /// This is a RUMPS extension providing stream-based iteration.
    /// The stream yields entries that match the predicate, transformed
    /// by the extract function. Read-only operation - no WAL logging.
    ///
    /// # Type Parameters
    ///
    /// * `P` - Predicate: `(&Key, &Option<Value>) -> bool` (include if `true`)
    /// * `F` - Extract: `(&Key, &Option<Value>) -> Option<T>` (transform entry)
    /// * `T` - Output type yielded by the stream
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{local, key, Value};
    /// use futures::stream::StreamExt;
    ///
    /// let db = Database::in_memory()?;
    /// let name = local!("DATA");
    /// db.set(&name, &key![1, "A"], Value::from("x")).await?;
    /// db.set(&name, &key![1, "B"], Value::from("y")).await?;
    /// db.set(&name, &key![2, "A"], Value::from("z")).await?;
    ///
    /// // Collect all values where key starts with `[1]`
    /// let mut stream = db.collects(
    ///     &name,
    ///     None,
    ///     |k, _| k.len() == 2 && k.get(0) == Some(&1.into()),
    ///     |_, val| val.clone(),
    /// ).await?;
    ///
    /// let mut results = Vec::new();
    /// while let Some(val) = stream.next().await {
    ///     results.push(val?);
    /// }
    /// assert_eq!(results.len(), 2);
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn collects<'a, P, F, T>(
        &'a self,
        name: &'a Name,
        start: Option<&'a Key>,
        pred: P,
        extract: F,
    ) -> Result<impl Stream<Item = Result<T>> + Send + 'a>
    where
        P: Fn(&Key, &Option<Value>) -> bool + Send + Sync + 'a,
        F: Fn(&Key, &Option<Value>) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        // Wrap user's closures to adapt to internal NodeData interface
        let pred_wrap = move |k: &Key, data: &NodeData| pred(k, &data.value);
        let extract_wrap =
            move |k: &Key, data: &NodeData| extract(k, &data.value);

        let opt_root = self.get_root(name).await?;
        let s = match opt_root {
            Some(root) => self
                .btree
                .collects_at(root, start, pred_wrap, extract_wrap, None)
                .map(|r| r.map_err(Into::into))
                .boxed(),
            None => stream::empty().boxed(),
        };
        Ok(s)
    }

    /// Collects all matching entries into a `Vec`.
    ///
    /// This is a convenience wrapper around `collects()` that collects
    /// the stream into a vector. Useful when you need all results at once.
    ///
    /// # Parameters
    ///
    /// * `name` - The variable name (global or local)
    /// * `start` - Optional key to start iteration after (exclusive)
    /// * `pred` - Predicate returning `true` to include entry, `false` to skip
    /// * `extract` - Extractor returning `Some(T)` to yield, `None` to skip
    pub async fn collects_vec<P, F, T>(
        &self,
        name: &Name,
        start: Option<&Key>,
        pred: P,
        extract: F,
    ) -> Result<Vec<T>>
    where
        P: Fn(&Key, &Option<Value>) -> bool + Send + Sync,
        F: Fn(&Key, &Option<Value>) -> Option<T> + Send + Sync,
        T: Send,
    {
        // Wrap user's closures to adapt to internal NodeData interface
        let pred_wrap = move |k: &Key, data: &NodeData| pred(k, &data.value);
        let extract_wrap =
            move |k: &Key, data: &NodeData| extract(k, &data.value);

        let opt_root = self.get_root(name).await?;
        Ok(match opt_root {
            Some(root) => {
                self.btree
                    .collects_vec_at(root, start, pred_wrap, extract_wrap, None)
                    .await?
            }
            None => Vec::new(),
        })
    }

    /// Collects all entries matching a key prefix into a `Vec`.
    ///
    /// This method is optimized for prefix-based queries: it seeks directly
    /// to the prefix position and **stops iteration** as soon as a key is
    /// encountered that doesn't start with the prefix. This is much more
    /// efficient than `collects_vec` with a prefix predicate for sparse data.
    ///
    /// # Parameters
    ///
    /// * `name` - The variable name (global or local)
    /// * `prefix` - The key prefix to match
    /// * `extract` - Extractor returning `Some(T)` to yield, `None` to skip
    pub(crate) async fn collects_prefix_vec<F, T>(
        &self,
        name: &Name,
        prefix: &Key,
        extract: F,
    ) -> Result<Vec<T>>
    where
        F: Fn(&Key, &Option<Value>) -> Option<T> + Send + Sync,
        T: Send,
    {
        let extract_wrap =
            move |k: &Key, data: &NodeData| extract(k, &data.value);

        let opt_root = self.get_root(name).await?;
        Ok(match opt_root {
            Some(root) => {
                self.btree
                    .collects_prefix_vec_at(root, prefix, extract_wrap)
                    .await?
            }
            None => Vec::new(),
        })
    }

    /// Creates a stream of entries matching a key prefix.
    ///
    /// This method is optimized for prefix-based queries: it seeks directly
    /// to the prefix position and **stops iteration** as soon as a key is
    /// encountered that doesn't start with the prefix.
    ///
    /// # Parameters
    ///
    /// * `name` - The variable name (global or local)
    /// * `prefix` - The key prefix to match
    /// * `extract` - Extractor returning `Some(T)` to yield, `None` to skip
    pub(crate) async fn collects_prefix<'a, F, T>(
        &'a self,
        name: &'a Name,
        prefix: &'a Key,
        extract: F,
    ) -> Result<impl Stream<Item = Result<T>> + Send + 'a>
    where
        F: Fn(&Key, &Option<Value>) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        let extract_wrap =
            move |k: &Key, data: &NodeData| extract(k, &data.value);

        let opt_root = self.get_root(name).await?;
        let s = match opt_root {
            Some(root) => self
                .btree
                .collects_prefix_at(root, prefix, extract_wrap)
                .map(|r| r.map_err(Into::into))
                .boxed(),
            None => stream::empty().boxed(),
        };
        Ok(s)
    }

    /// Executes a function within a transaction context.
    ///
    /// The transaction auto-commits if the closure returns `Ok`, and
    /// auto-rollbacks if it returns `Err`.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    /// use rumps_types::{global, key, Value};
    ///
    /// let db = Database::in_memory()?;
    ///
    /// db.transaction(|txn| async move {
    ///     txn.set(&global!("PATIENT"), &key![123, "NAME"], Value::from("Bob")).await?;
    ///     txn.set(&global!("PATIENT"), &key![123, "AGE"], Value::from(42)).await?;
    ///     Ok(())
    /// }).await?;
    ///
    /// // Values are visible after commit
    /// let name = db.get(&global!("PATIENT"), &key![123, "NAME"]).await?;
    /// assert_eq!(name, Some(Value::from("Bob")));
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn transaction<F, Fut, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(Transaction) -> Fut,
        Fut: Future<Output = Result<R>>,
    {
        self.build_transaction().begin(f).await
    }

    /// Returns diagnostic statistics for the database.
    ///
    /// Use this method for debugging and monitoring. The returned
    /// [`DatabaseStats`] aggregates statistics from all components.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::Database;
    ///
    /// let db = Database::in_memory()?;
    /// let stats = db.debug().await;
    ///
    /// assert!(stats.in_memory);
    /// assert_eq!(stats.active_txns, 0);
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub async fn debug(&self) -> DatabaseStats {
        let cache = match &self.storage {
            Some(s) => Some(s.cache_stats().await),
            None => None,
        };
        let storage = match &self.storage {
            Some(s) => Some(s.metadata().await),
            None => None,
        };
        DatabaseStats {
            root_count: self.roots.read().await.len(),
            btree: self.btree.stats().await,
            cache,
            storage,
            active_txns: self.txn_manager.active_count().await,
            in_memory: self.storage.is_none(),
        }
    }
}

// Internal methods
impl Database {
    /// Sets a value with an explicit transaction ID.
    ///
    /// This is the internal version used by `Transaction::commit()`.
    /// The transaction ID is used for WAL logging and BTree context.
    pub(crate) async fn set_with_txn(
        &self,
        name: &Name,
        key: &Key,
        val: Value,
        txn_id: TransactionId,
        start_ts: TransactionTimestamp,
    ) -> crate::error::Result<()> {
        let root = self.ensure_root(name).await?;

        let old_data = {
            let opt_arc = self.btree.get_internal(root, key).await?;
            opt_arc.map(|arc| (*arc).clone())
        };

        if let (Name::Global(_), Some(storage)) = (name, self.storage.as_ref())
        {
            storage
                .wal_append(&WalRecord::Set {
                    txn_id,
                    name: name.clone(),
                    key: key.clone(),
                    old: old_data,
                    new: NodeData::with_value(val.clone()),
                })
                .await?;
        }

        let ctx = TransactionContext::new(txn_id, start_ts);
        let new_root = self.btree.set_at(root, key, val, &ctx).await?;

        if new_root != root {
            self.update_root(name, new_root).await?;
        }

        Ok(())
    }

    /// Kills a key/subtree with an explicit transaction ID.
    ///
    /// This is the internal version used by `Transaction::commit()`.
    pub(crate) async fn kill_with_txn(
        &self,
        name: &Name,
        key: &Key,
        txn_id: TransactionId,
        start_ts: TransactionTimestamp,
    ) -> crate::error::Result<()> {
        let opt_root = self.get_root(name).await?;
        match opt_root {
            None => Ok(()),
            Some(root) => {
                let to_delete = match (name, self.storage.as_ref()) {
                    (Name::Global(_), Some(_)) => {
                        let mut entries = Vec::new();

                        if let Some(arc_data) =
                            self.btree.get_internal(root, key).await?
                        {
                            entries.push((key.clone(), (*arc_data).clone()));
                        }

                        let descendants: Vec<(Key, NodeData)> = self
                            .btree
                            .collects_at(
                                root,
                                Some(key),
                                |k, _| k.starts_with(key),
                                |k, data| Some((k.clone(), data.clone())),
                                None,
                            )
                            .collect::<Vec<_>>()
                            .await
                            .into_iter()
                            .collect::<crate::error::Result<Vec<_>>>()?;

                        entries.extend(descendants);
                        Some(entries)
                    }
                    _ => None,
                };

                if let (Some(entries), Some(storage)) =
                    (to_delete, self.storage.as_ref())
                {
                    stream::iter(entries.iter().map(Ok))
                        .try_for_each(|(k, data)| async move {
                            storage
                                .wal_append(&WalRecord::KillEntry {
                                    txn_id,
                                    name: name.clone(),
                                    key: k.clone(),
                                    data: data.clone(),
                                })
                                .await
                                .map(|_| ())
                        })
                        .await?;
                }

                let ctx = TransactionContext::new(txn_id, start_ts);
                let opt_new_root = self.btree.kill_at(root, key, &ctx).await?;

                match opt_new_root {
                    Some(new_root) if new_root != root => {
                        self.update_root(name, new_root).await
                    }
                    None => self.remove_root(name).await.map(|_| ()),
                    _ => Ok(()),
                }
            }
        }
    }

    /// Flushes with an explicit transaction ID.
    ///
    /// This is the internal version used by `Transaction::commit()`.
    pub(crate) async fn flush_with_txn(
        &self,
        txn_id: TransactionId,
    ) -> crate::error::Result<()> {
        if let Some(storage) = self.storage.as_ref() {
            storage.wal_append(&WalRecord::TxnCommit { txn_id }).await?;

            storage.wal_sync().await?;
            storage.flush().await?;
        }
        Ok(())
    }

    /// Deletes a key and all its descendants from the database.
    ///
    /// **Note**: Writes to globals require a transaction. Use `db.transaction()`
    /// or create a `Transaction` manually. Direct calls for globals will error.
    ///
    /// For locals (in-memory only), this deletes from the B-tree directly.
    pub(crate) async fn kill(
        &self,
        name: &Name,
        key: &Key,
    ) -> crate::error::Result<()> {
        if matches!(name, Name::Global(_)) {
            Err(StorageError::GlobalRequiresTransaction)
        } else {
            let opt_root = self.get_root(name).await?;
            match opt_root {
                None => Ok(()),
                Some(root) => {
                    let ctx = TransactionContext::new(
                        TransactionId::IMPLICIT,
                        TransactionTimestamp::from(0),
                    );

                    let opt_new_root =
                        self.btree.kill_at(root, key, &ctx).await?;

                    match opt_new_root {
                        Some(new_root) if new_root != root => {
                            self.update_root(name, new_root).await
                        }
                        None => self.remove_root(name).await.map(|_| ()),
                        _ => Ok(()),
                    }
                }
            }
        }
    }

    /// Flushes all dirty pages and metadata to disk.
    ///
    /// For persistent databases, this syncs the WAL and flushes dirty pages.
    /// For in-memory databases, this is a no-op.
    pub(crate) async fn flush(&self) -> crate::error::Result<()> {
        if let Some(storage) = self.storage.as_ref() {
            storage.wal_sync().await?;
            storage.flush().await?;
        }
        Ok(())
    }

    /// Creates a transaction builder for custom configuration.
    ///
    /// Returns a [`BoundTransactionBuilder`] that provides a fluent API for
    /// configuring and executing transactions.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio_test::block_on(async {
    /// use rumps_storage::{Database, TransactionPriority, ConflictStrategy};
    /// use rumps_types::{global, key, value};
    ///
    /// let db = Database::in_memory()?;
    ///
    /// db.build_transaction()
    ///     .timeout(5000)
    ///     .priority(TransactionPriority::High)
    ///     .conflict(ConflictStrategy::Retry(3))
    ///     .begin(|txn| async move {
    ///         txn.set(&global!("DATA"), &key![1], value!("test")).await?;
    ///         Ok(())
    ///     })
    ///     .await?;
    /// # Ok::<(), rumps_storage::Error>(())
    /// # });
    /// ```
    pub fn build_transaction(&self) -> TransactionBuilder {
        TransactionBuilder::new(self.clone())
    }

    /// Returns the underlying B-tree.
    fn btree(&self) -> Arc<BTree> {
        Arc::clone(&self.btree)
    }

    /// Creates a database with a custom B-tree.
    ///
    /// Useful for testing with specific B-tree configurations.
    fn with_btree(
        btree: Arc<BTree>,
        txn_manager: Arc<TransactionManager>,
    ) -> crate::error::Result<Self> {
        Ok(Self {
            roots: Arc::new(RwLock::new(BTreeMap::new())),
            btree,
            storage: None,
            txn_manager,
            closed: Arc::new(StdRwLock::new(false)),
        })
    }

    /// Runs WAL recovery and replays committed operations.
    ///
    /// This is called during `open()` to bring the database to a consistent
    /// state after a crash or unclean shutdown.
    async fn recover(&self, path: &Path) -> crate::error::Result<()> {
        let wal_dir = path.join("wal");

        // Run WAL recovery
        let (recovery, _reader) =
            WalReader::open(&wal_dir).await?.recover().await?;

        // Apply committed operations
        stream::iter(recovery.committed_ops.iter().map(Ok))
            .try_for_each(|committed_op| async move {
                match &committed_op.op {
                    WalOp::Set {
                        name,
                        key,
                        new,
                        old: _,
                    } => {
                        // Ensure root exists
                        let root = self.ensure_root(name).await?;

                        // Create transaction context
                        let ctx = TransactionContext::new(
                            committed_op.txn_id,
                            TransactionTimestamp::from(0),
                        );

                        // Apply set operation (value from new NodeData)
                        let new_val = new.value.clone().ok_or_else(|| {
                            StorageError::InvalidConfiguration(
                                "Set operation in WAL has no value".into(),
                            )
                        })?;
                        let new_root =
                            self.btree.set_at(root, key, new_val, &ctx).await?;

                        // Update root if changed
                        if new_root != root {
                            self.update_root(name, new_root).await?;
                        }

                        Ok(())
                    }
                    WalOp::KillEntry { name, key, data: _ } => {
                        let opt_root = self.get_root(name).await?;
                        match opt_root {
                            Some(root) => {
                                // Create transaction context
                                let ctx = TransactionContext::new(
                                    committed_op.txn_id,
                                    TransactionTimestamp::from(0),
                                );

                                // Apply kill operation
                                let opt_new_root =
                                    self.btree.kill_at(root, key, &ctx).await?;

                                // Update or remove root
                                match opt_new_root {
                                    Some(new_root) if new_root != root => {
                                        self.update_root(name, new_root).await
                                    }
                                    None => {
                                        self.remove_root(name).await.map(|_| ())
                                    }
                                    _ => Ok(()),
                                }
                            }
                            None => Ok(()), // No root = nothing to kill
                        }
                    }
                }
            })
            .await?;

        Ok(())
    }

    /// Looks up the root `NodeId` for a variable name.
    ///
    /// For in-memory databases, simply checks the local cache.
    /// For persistent databases, lazy-loads globals from the registry
    /// if not already cached.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(root))` - Root exists
    /// - `Ok(None)` - Variable doesn't exist
    async fn get_root(
        &self,
        name: &Name,
    ) -> crate::error::Result<Option<NodeId>> {
        // Check cache first
        let cached = {
            let roots = self.roots.read().await;
            roots.get(name).copied()
        };

        match cached {
            Some(root) => Ok(Some(root)),
            None => {
                // Cache miss - for globals with storage, lazy-load from registry
                match (name, self.storage.as_ref()) {
                    (Name::Global(g), Some(storage)) => {
                        let opt_page = storage.registry_get(g).await;

                        // Convert PageId to NodeId and cache if found
                        if let Some(page_id) = opt_page {
                            let node_id = NodeId::from(page_id);
                            // Cache the loaded root
                            {
                                let mut roots_guard = self.roots.write().await;
                                roots_guard
                                    .entry(name.clone())
                                    .or_insert(node_id);
                            }
                            Ok(Some(node_id))
                        } else {
                            Ok(None)
                        }
                    }
                    _ => Ok(None), // Locals or no storage
                }
            }
        }
    }

    /// Gets or creates a root for a variable name.
    ///
    /// If the variable doesn't exist, creates a new empty tree and returns
    /// its root. For persistent globals, also registers the new root in
    /// the storage registry.
    ///
    /// # Returns
    ///
    /// The root `NodeId` for the variable (existing or newly created).
    async fn ensure_root(&self, name: &Name) -> crate::error::Result<NodeId> {
        // Check cache first, and lazy-load from registry if needed
        let existing = self.get_root(name).await?;

        match existing {
            Some(root) => Ok(root),
            None => {
                // Create new tree
                let root = self.btree.create_tree().await?;

                // Cache the new root
                {
                    let mut roots = self.roots.write().await;
                    // Double-check in case another task created it
                    roots.entry(name.clone()).or_insert(root);
                }

                // For globals with storage, register in registry
                if let (Name::Global(g), Some(storage)) =
                    (name, self.storage.as_ref())
                {
                    storage
                        .registry_insert(
                            g.clone(),
                            crate::page::PageId::from(root),
                        )
                        .await?;
                }

                Ok(root)
            }
        }
    }

    /// Removes a variable's root from the cache.
    ///
    /// This does NOT delete the tree's nodes - use `delete_tree()` on
    /// the B-tree for that. This only removes the name → root mapping.
    ///
    /// For persistent globals, also removes from the registry.
    async fn remove_root(
        &self,
        name: &Name,
    ) -> crate::error::Result<Option<NodeId>> {
        let removed = self.roots.write().await.remove(name);

        // For globals with storage, remove from registry
        if let (Name::Global(g), Some(storage)) = (name, self.storage.as_ref())
        {
            storage.registry_remove(g).await?;
        }

        Ok(removed)
    }

    /// Updates the root for a variable name.
    ///
    /// Called after operations that change the tree structure (splits,
    /// merges) which may result in a new root `NodeId`.
    async fn update_root(
        &self,
        name: &Name,
        new_root: NodeId,
    ) -> crate::error::Result<()> {
        {
            let mut roots = self.roots.write().await;
            roots.insert(name.clone(), new_root);
        }

        // For globals with storage, update registry
        if let (Name::Global(g), Some(storage)) = (name, self.storage.as_ref())
        {
            storage
                .registry_insert(g.clone(), crate::page::PageId::from(new_root))
                .await?;
        }

        Ok(())
    }

    /// Returns the number of cached roots (variables).
    async fn root_count(&self) -> usize {
        self.roots.read().await.len()
    }
}

/// Automatic cleanup on drop.
///
/// When the last `Database` handle to a storage engine is dropped, this
/// implementation flushes all pending writes to disk. This provides a
/// safety net for cleanup, similar to how dropping a file handle closes it.
///
/// # How it works
///
/// - Skips cleanup if [`close()`] was already called
/// - Uses `Arc::strong_count` to detect if this is the last handle
/// - Uses `tokio::task::block_in_place` to safely block on async I/O
/// - Errors are printed to stderr (no panic in drop)
///
/// # Explicit close
///
/// For proper error handling, call [`close()`] explicitly. The `Drop`
/// impl is a best-effort fallback, not a replacement for explicit cleanup.
///
/// [`close()`]: Self::close
impl Drop for Database {
    fn drop(&mut self) {
        // Skip if `Self::close` was already called
        let already_closed = self.closed.read().map(|g| *g).unwrap_or(false);

        if !already_closed {
            // Only flush if we're the last `Database` handle to this storage.
            self.storage
                .as_ref()
                .filter(|s| Arc::strong_count(s) == 1)
                .into_iter()
                .for_each(|storage| {
                    let result = tokio::runtime::Handle::try_current()
                        .map_err(|e| {
                            StorageError::InvalidOperation(e.to_string())
                        })
                        .and_then(|handle| {
                            tokio::task::block_in_place(|| {
                                handle.block_on(async {
                                    storage.wal_sync().await?;
                                    storage.flush().await
                                })
                            })
                        });

                    if let Err(e) = result {
                        eprintln!(
                            "rumps: failed to flush database on drop: {e}"
                        );
                    }
                    storage.shutdown_sync_task();
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use rumps_types::{global, local};

    use super::*;

    #[tokio::test]
    async fn in_memory_creates_empty_database() {
        let db = Database::in_memory().unwrap();
        assert_eq!(db.root_count().await, 0);
    }

    #[tokio::test]
    async fn get_root_returns_none_for_unknown() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");
        assert!(db.get_root(&name).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ensure_root_creates_and_caches() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");

        let root1 = db.ensure_root(&name).await.unwrap();
        let root2 = db.ensure_root(&name).await.unwrap();

        // Same root returned
        assert_eq!(root1, root2);
        assert_eq!(db.root_count().await, 1);
    }

    #[tokio::test]
    async fn ensure_root_returns_cached_on_second_call() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");

        let root1 = db.ensure_root(&name).await.unwrap();
        assert!(db.get_root(&name).await.unwrap().is_some());

        let root2 = db.ensure_root(&name).await.unwrap();
        assert_eq!(root1, root2);
    }

    #[tokio::test]
    async fn remove_root_removes_from_cache() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");

        let root = db.ensure_root(&name).await.unwrap();
        assert_eq!(db.root_count().await, 1);

        let removed = db.remove_root(&name).await.unwrap();
        assert_eq!(removed, Some(root));
        assert_eq!(db.root_count().await, 0);
        assert!(db.get_root(&name).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn global_and_local_have_separate_roots() {
        let db = Database::in_memory().unwrap();
        let global = global!("X");
        let local = local!("X");

        let global_root = db.ensure_root(&global).await.unwrap();
        let local_root = db.ensure_root(&local).await.unwrap();

        // Different roots for same name in different namespaces
        assert_ne!(global_root, local_root);
        assert_eq!(db.root_count().await, 2);
    }

    #[tokio::test]
    async fn update_root_changes_mapping() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");

        let root1 = db.ensure_root(&name).await.unwrap();
        let new_root = NodeId::from(999u64);

        db.update_root(&name, new_root).await.unwrap();

        let fetched = db.get_root(&name).await.unwrap();
        assert_eq!(fetched, Some(new_root));
        assert_ne!(fetched, Some(root1));
    }

    #[tokio::test]
    async fn disk_persistence_create_and_reopen() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Create database and add some data
        let root = {
            let db = Database::create(&db_path).await.unwrap();
            let name = global!("PATIENT");

            let root = db.ensure_root(&name).await.unwrap();
            assert_eq!(db.root_count().await, 1);

            // Flush and close
            db.flush().await.unwrap();
            db.close().await.unwrap();

            root
        };

        // Reopen and verify the root was persisted
        {
            let db = Database::open(&db_path).await.unwrap();
            let name = global!("PATIENT");

            // Root should be lazy-loaded from registry
            let loaded_root = db.get_root(&name).await.unwrap();
            assert_eq!(loaded_root, Some(root));
            assert_eq!(db.root_count().await, 1);

            db.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn disk_persistence_multiple_globals() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Create database with multiple globals
        let (root1, root2, root3) = {
            let db = Database::create(&db_path).await.unwrap();

            let r1 = db.ensure_root(&global!("VAR1")).await.unwrap();
            let r2 = db.ensure_root(&global!("VAR2")).await.unwrap();
            let r3 = db.ensure_root(&global!("VAR3")).await.unwrap();

            assert_eq!(db.root_count().await, 3);

            db.flush().await.unwrap();
            db.close().await.unwrap();

            (r1, r2, r3)
        };

        // Reopen and verify all roots
        {
            let db = Database::open(&db_path).await.unwrap();

            assert_eq!(
                db.get_root(&global!("VAR1")).await.unwrap(),
                Some(root1)
            );
            assert_eq!(
                db.get_root(&global!("VAR2")).await.unwrap(),
                Some(root2)
            );
            assert_eq!(
                db.get_root(&global!("VAR3")).await.unwrap(),
                Some(root3)
            );

            db.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn database_operations_with_wal() {
        use rumps_types::{key, Value};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Create database and perform operations using transactions
        {
            let db = Database::create(&db_path).await.unwrap();
            let name = global!("PATIENT");

            // SET operations within a transaction
            let name_clone = name.clone();
            db.transaction(|txn| async move {
                txn.set(&name_clone, &key![1, "NAME"], Value::from("Alice"))
                    .await?;
                txn.set(&name_clone, &key![1, "AGE"], Value::from(30))
                    .await?;
                txn.set(&name_clone, &key![2, "NAME"], Value::from("Bob"))
                    .await?;
                Ok(())
            })
            .await
            .unwrap();

            // Verify GET (reads work without transaction)
            assert_eq!(
                db.get(&name, &key![1, "NAME"]).await.unwrap(),
                Some(Value::from("Alice"))
            );
            assert_eq!(
                db.get(&name, &key![1, "AGE"]).await.unwrap(),
                Some(Value::from(30))
            );

            // Verify DATA
            let status = db.data(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasDescendants);

            // KILL operation within a transaction
            let name_clone = name.clone();
            db.transaction(|txn| async move {
                txn.kill(&name_clone, &key![1]).await?;
                Ok(())
            })
            .await
            .unwrap();

            // Verify deletion
            assert_eq!(db.get(&name, &key![1, "NAME"]).await.unwrap(), None);
            assert_eq!(db.get(&name, &key![1, "AGE"]).await.unwrap(), None);

            // Verify patient 2 still exists
            assert_eq!(
                db.get(&name, &key![2, "NAME"]).await.unwrap(),
                Some(Value::from("Bob"))
            );

            db.close().await.unwrap();
        }

        // Reopen and verify persistence
        {
            let db = Database::open(&db_path).await.unwrap();
            let name = global!("PATIENT");

            // Patient 1 should be deleted
            assert_eq!(db.get(&name, &key![1, "NAME"]).await.unwrap(), None);

            // Patient 2 should still exist
            assert_eq!(
                db.get(&name, &key![2, "NAME"]).await.unwrap(),
                Some(Value::from("Bob"))
            );

            db.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn global_set_without_transaction_fails() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");

        // Direct set on global should fail
        let result = db
            .set(&name, &rumps_types::key![1], rumps_types::Value::from(42))
            .await;
        assert!(matches!(
            result,
            Err(rumps_types::Error::Storage(
                StorageError::GlobalRequiresTransaction
            ))
        ));
    }

    #[tokio::test]
    async fn local_set_without_transaction_works() {
        let db = Database::in_memory().unwrap();
        let name = local!("TEMP");

        // Direct set on local should work
        db.set(&name, &rumps_types::key![1], rumps_types::Value::from(42))
            .await
            .unwrap();

        // Verify the value was set
        let val = db.get(&name, &rumps_types::key![1]).await.unwrap();
        assert_eq!(val, Some(rumps_types::Value::from(42)));
    }

    #[tokio::test]
    async fn transaction_commit_applies_changes() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");

        // Set within transaction
        let name_clone = name.clone();
        db.transaction(|txn| async move {
            txn.set(
                &name_clone,
                &rumps_types::key![1],
                rumps_types::Value::from(100),
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Value should be visible after commit
        let val = db.get(&name, &rumps_types::key![1]).await.unwrap();
        assert_eq!(val, Some(rumps_types::Value::from(100)));
    }

    #[tokio::test]
    async fn transaction_rollback_discards_changes() {
        let db = Database::in_memory().unwrap();
        let name = global!("TEST");

        // First, set an initial value
        let name_clone = name.clone();
        db.transaction(|txn| async move {
            txn.set(
                &name_clone,
                &rumps_types::key![1],
                rumps_types::Value::from(100),
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();

        // Transaction that fails should rollback
        let name_clone = name.clone();
        let result: Result<()> = db
            .transaction(|txn| async move {
                txn.set(
                    &name_clone,
                    &rumps_types::key![1],
                    rumps_types::Value::from(200),
                )
                .await?;
                // Simulate error
                Err(rumps_types::Error::Storage(
                    StorageError::InvalidOperation(
                        "intentional failure".into(),
                    ),
                ))
            })
            .await;

        assert!(result.is_err());

        // Value should still be 100 (rollback preserved original)
        let val = db.get(&name, &rumps_types::key![1]).await.unwrap();
        assert_eq!(val, Some(rumps_types::Value::from(100)));
    }

    #[tokio::test]
    async fn transaction_manager_allocates_unique_ids() {
        let db = Database::in_memory().unwrap();

        // Create multiple transactions and verify they get unique IDs
        let id1 = db.txn_manager.allocate_txn_id().await;
        let id2 = db.txn_manager.allocate_txn_id().await;
        let id3 = db.txn_manager.allocate_txn_id().await;

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);

        // IDs should be increasing
        assert!(*id1 < *id2);
        assert!(*id2 < *id3);
    }

    #[tokio::test]
    async fn write_write_conflict_detection() {
        let db = Database::in_memory().unwrap();
        let name = rumps_types::global!("TEST");
        let key = rumps_types::key![1];

        // Start transaction A (will commit second)
        let txn_a = db.build_transaction().start().await.unwrap();

        // Start transaction B (will commit first)
        let txn_b = db.build_transaction().start().await.unwrap();

        // Both write to the same key
        txn_a
            .set(&name, &key, rumps_types::Value::from("A"))
            .await
            .unwrap();
        txn_b
            .set(&name, &key, rumps_types::Value::from("B"))
            .await
            .unwrap();

        // B commits first - should succeed
        txn_b.commit().await.unwrap();

        // A commits second - should fail with WriteConflict
        let result = txn_a.commit().await;
        assert!(
            matches!(result, Err(StorageError::WriteConflict { .. })),
            "Expected WriteConflict, got {:?}",
            result
        );

        // Verify B's value persisted
        let val = db.get(&name, &key).await.unwrap();
        assert_eq!(val, Some(rumps_types::Value::from("B")));
    }

    #[tokio::test]
    async fn non_overlapping_writes_no_conflict() {
        let db = Database::in_memory().unwrap();
        let name = rumps_types::global!("TEST");

        // Start both transactions
        let txn_a = db.build_transaction().start().await.unwrap();
        let txn_b = db.build_transaction().start().await.unwrap();

        // Write to different keys
        txn_a
            .set(&name, &rumps_types::key![1], rumps_types::Value::from("A"))
            .await
            .unwrap();
        txn_b
            .set(&name, &rumps_types::key![2], rumps_types::Value::from("B"))
            .await
            .unwrap();

        // Both should commit successfully (no conflict)
        txn_b.commit().await.unwrap();
        txn_a.commit().await.unwrap();

        // Verify both values persisted
        let val1 = db.get(&name, &rumps_types::key![1]).await.unwrap();
        let val2 = db.get(&name, &rumps_types::key![2]).await.unwrap();
        assert_eq!(val1, Some(rumps_types::Value::from("A")));
        assert_eq!(val2, Some(rumps_types::Value::from("B")));
    }

    mod fluent_builder {
        use super::*;

        #[tokio::test]
        async fn basic_transaction_works() {
            let db = Database::in_memory().unwrap();
            let name = rumps_types::global!("TEST");
            let key = rumps_types::key![1];

            let (n, k) = (name.clone(), key.clone());
            db.build_transaction()
                .begin(|txn| async move {
                    txn.set(&n, &k, rumps_types::Value::from("hello")).await?;
                    Ok(())
                })
                .await
                .unwrap();

            let val = db.get(&name, &key).await.unwrap();
            assert_eq!(val, Some(rumps_types::Value::from("hello")));
        }

        #[tokio::test]
        async fn transaction_with_timeout() {
            let db = Database::in_memory().unwrap();
            let name = rumps_types::global!("TEST");
            let key = rumps_types::key![1];

            let (n, k) = (name.clone(), key.clone());
            db.build_transaction()
                .timeout(5000)
                .begin(|txn| async move {
                    txn.set(&n, &k, rumps_types::Value::from("timed")).await?;
                    Ok(())
                })
                .await
                .unwrap();

            let val = db.get(&name, &key).await.unwrap();
            assert_eq!(val, Some(rumps_types::Value::from("timed")));
        }

        #[tokio::test]
        async fn transaction_with_priority() {
            use crate::TransactionPriority;

            let db = Database::in_memory().unwrap();
            let name = rumps_types::global!("TEST");
            let key = rumps_types::key![1];

            let (n, k) = (name.clone(), key.clone());
            db.build_transaction()
                .priority(TransactionPriority::High)
                .begin(|txn| async move {
                    txn.set(&n, &k, rumps_types::Value::from("high")).await?;
                    Ok(())
                })
                .await
                .unwrap();

            let val = db.get(&name, &key).await.unwrap();
            assert_eq!(val, Some(rumps_types::Value::from("high")));
        }

        #[tokio::test]
        async fn chained_config_works() {
            use crate::{ConflictStrategy, TransactionPriority};

            let db = Database::in_memory().unwrap();
            let name = rumps_types::global!("TEST");
            let key = rumps_types::key![1];

            let (n, k) = (name.clone(), key.clone());
            db.build_transaction()
                .timeout(10000)
                .priority(TransactionPriority::High)
                .conflict(ConflictStrategy::Retry(3))
                .retries(5)
                .begin(|txn| async move {
                    txn.set(&n, &k, rumps_types::Value::from("chained"))
                        .await?;
                    Ok(())
                })
                .await
                .unwrap();

            let val = db.get(&name, &key).await.unwrap();
            assert_eq!(val, Some(rumps_types::Value::from("chained")));
        }

        #[tokio::test]
        async fn transaction_rollback_on_error() {
            let db = Database::in_memory().unwrap();
            let name = rumps_types::global!("TEST");
            let key = rumps_types::key![1];

            // First set a value
            let (n, k) = (name.clone(), key.clone());
            db.build_transaction()
                .begin(|txn| async move {
                    txn.set(&n, &k, rumps_types::Value::from("original"))
                        .await?;
                    Ok(())
                })
                .await
                .unwrap();

            // Try to update but fail
            let (n, k) = (name.clone(), key.clone());
            let result: Result<()> = db
                .build_transaction()
                .begin(|txn| async move {
                    txn.set(&n, &k, rumps_types::Value::from("updated"))
                        .await?;
                    Err(rumps_types::Error::Storage(
                        StorageError::InvalidOperation("intentional".into()),
                    ))
                })
                .await;

            assert!(result.is_err());

            // Value should still be original
            let val = db.get(&name, &key).await.unwrap();
            assert_eq!(val, Some(rumps_types::Value::from("original")));
        }

        #[tokio::test]
        async fn transaction_returns_value() {
            let db = Database::in_memory().unwrap();
            let name = rumps_types::global!("TEST");
            let key = rumps_types::key![1];

            let (n, k) = (name.clone(), key.clone());
            db.build_transaction()
                .begin(|txn| async move {
                    txn.set(&n, &k, rumps_types::Value::from(42i64)).await?;
                    Ok(())
                })
                .await
                .unwrap();

            let (n, k) = (name.clone(), key.clone());
            let result: i64 = db
                .build_transaction()
                .begin(|txn| async move {
                    let val = txn.get(&n, &k).await?;
                    match val {
                        Some(rumps_types::Value::Integer(n)) => Ok(n),
                        _ => Ok(0),
                    }
                })
                .await
                .unwrap();

            assert_eq!(result, 42);
        }
    }

    mod config_persistence {
        use std::time::Duration;

        use tempfile::TempDir;

        use super::*;

        #[tokio::test]
        async fn full_roundtrip_custom_config() {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("test.db");

            // Create with custom config
            {
                let db = Database::builder()
                    .cache_size(2048)
                    .max_pages(5000)
                    .sync_mode(SyncMode::Immediate)
                    .wal_max_file_size(32 * 1024 * 1024)
                    .min_degree(5)
                    .max_memory_bytes(1024 * 1024)
                    .create(&path)
                    .await
                    .unwrap();
                db.close().await.unwrap();
            }

            // Reopen with simple `Database::open()` - config restored
            {
                let db = Database::open(&path).await.unwrap();

                // Verify it opens and works
                let stats = db.debug().await;
                assert!(stats.storage.is_some());

                db.close().await.unwrap();
            }
        }

        #[tokio::test]
        async fn periodic_sync_mode_roundtrip() {
            let temp = TempDir::new().unwrap();
            let path = temp.path().join("test.db");

            // Create with SyncMode::Periodic
            {
                let db = Database::builder()
                    .sync_mode(SyncMode::Periodic(Duration::from_millis(500)))
                    .create(&path)
                    .await
                    .unwrap();
                db.close().await.unwrap();
            }

            // Reopen and verify it opens successfully
            {
                let db = Database::open(&path).await.unwrap();
                let stats = db.debug().await;
                assert!(stats.storage.is_some());
                db.close().await.unwrap();
            }
        }
    }

    /// Phase 4 tests: Concurrent Transaction Commits
    mod concurrent_commits {
        use std::sync::Arc;

        use super::*;

        /// N transactions writing to disjoint keys should all succeed.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn concurrent_commits_no_conflict() {
            let db = Arc::new(Database::in_memory().unwrap());
            let n = 10i64;
            let name = rumps_types::global!("TEST");

            // Process N transactions writing to disjoint keys
            let mut results = Vec::with_capacity(n as usize);
            (0..n).into_iter().for_each(|i| {
                let db = Arc::clone(&db);
                let name = name.clone();
                let r = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        db.build_transaction()
                            .begin(|txn| {
                                let name = name.clone();
                                async move {
                                    txn.set(
                                        &name,
                                        &rumps_types::key![i],
                                        rumps_types::Value::from(i),
                                    )
                                    .await?;
                                    Ok(())
                                }
                            })
                            .await
                    })
                });
                results.push(r);
            });

            // All should succeed
            results.iter().for_each(|r| {
                assert!(r.is_ok(), "All disjoint transactions should commit");
            });

            // Verify all values persisted
            (0..n).into_iter().for_each(|i| {
                let db = Arc::clone(&db);
                let name = name.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let val =
                            db.get(&name, &rumps_types::key![i]).await.unwrap();
                        assert_eq!(val, Some(rumps_types::Value::from(i)));
                    });
                });
            });
        }

        /// N transactions writing to same key; first-committer wins.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn concurrent_commits_with_conflict() {
            let db = Arc::new(Database::in_memory().unwrap());
            let n = 5i64;
            let name = rumps_types::global!("TEST");
            let key = rumps_types::key![1];

            // Start N transactions, all targeting the same key
            let mut txns = Vec::with_capacity(n as usize);
            (0..n).into_iter().for_each(|i| {
                let db = Arc::clone(&db);
                let txn = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(db.build_transaction().start())
                        .unwrap()
                });
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        txn.set(&name, &key, rumps_types::Value::from(i))
                            .await
                            .unwrap();
                    });
                });
                txns.push((i, txn));
            });

            // Commit them sequentially; first succeeds, rest should fail
            let mut results = Vec::with_capacity(n as usize);
            txns.into_iter().for_each(|(i, txn)| {
                let r = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(txn.commit())
                });
                results.push((i, r));
            });

            // Count successes and failures
            let (successes, failures): (Vec<_>, Vec<_>) =
                results.into_iter().partition(|(_, r)| r.is_ok());

            // Exactly one should succeed (first-committer-wins)
            assert_eq!(
                successes.len(),
                1,
                "Exactly one transaction should succeed"
            );
            assert_eq!(
                failures.len(),
                (n - 1) as usize,
                "All other transactions should fail"
            );

            // All failures should be WriteConflict
            failures.iter().for_each(|(_, r)| {
                assert!(
                    matches!(r, Err(StorageError::WriteConflict { .. })),
                    "Expected WriteConflict, got {:?}",
                    r
                );
            });

            // The committed value should be from the first transaction
            let first_idx = successes.first().unwrap().0;
            let val = db.get(&name, &key).await.unwrap();
            assert_eq!(val, Some(rumps_types::Value::from(first_idx)));
        }

        /// Stress test: many threads hammering random keys.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn stress_concurrent_random_keys() {
            let db = Arc::new(Database::in_memory().unwrap());
            let n_txns = 50i64;
            let n_keys = 20i64;
            let name = rumps_types::global!("STRESS");

            // Process transactions writing to various keys
            let mut results = Vec::with_capacity(n_txns as usize);
            (0..n_txns).into_iter().for_each(|i| {
                let key_idx = i % n_keys;
                let db = Arc::clone(&db);
                let name = name.clone();
                let r = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        db.build_transaction()
                            .begin(|txn| {
                                let name = name.clone();
                                async move {
                                    txn.set(
                                        &name,
                                        &rumps_types::key![key_idx],
                                        rumps_types::Value::from(i),
                                    )
                                    .await?;
                                    Ok(())
                                }
                            })
                            .await
                    })
                });
                results.push(r);
            });

            // Some should succeed, some may fail due to conflicts
            let (successes, failures): (Vec<_>, Vec<_>) =
                results.into_iter().partition(|r| r.is_ok());

            // At least one should succeed per unique key
            assert!(
                !successes.is_empty(),
                "At least some transactions should succeed"
            );

            // All failures should be WriteConflict
            failures.iter().for_each(|r| {
                assert!(
                    matches!(
                        r,
                        Err(rumps_types::Error::Storage(
                            StorageError::WriteConflict { .. }
                        ))
                    ),
                    "Expected WriteConflict, got {:?}",
                    r
                );
            });
        }

        /// Test that atomic validation+recording prevents the race condition.
        ///
        /// This test verifies that when two transactions race to commit the same
        /// key, exactly one succeeds (first-committer-wins). The old code had a
        /// race condition where both could pass validation before either recorded.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn atomic_validation_prevents_race() {
            use std::sync::atomic::{AtomicUsize, Ordering};
            use std::sync::Barrier;

            let db = Arc::new(Database::in_memory().unwrap());
            let name = rumps_types::global!("RACE");
            let key = rumps_types::key![1];
            let n_trials = 20i64;

            // Run multiple trials to increase chance of catching race conditions
            let mut both_succeeded = 0usize;
            let mut one_succeeded = 0usize;

            (0..n_trials).into_iter().for_each(|trial| {
                let db = Arc::clone(&db);
                let name = name.clone();
                let key = key.clone();

                // Use a barrier to synchronize the two commits
                let barrier = Arc::new(Barrier::new(2));
                let success_count = Arc::new(AtomicUsize::new(0));

                // Start two transactions writing to the same key
                let txn_a = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(db.build_transaction().start())
                        .unwrap()
                });
                let txn_b = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(db.build_transaction().start())
                        .unwrap()
                });

                // Both write to same key
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        txn_a
                            .set(
                                &name,
                                &key,
                                rumps_types::Value::from(trial * 2),
                            )
                            .await
                            .unwrap();
                        txn_b
                            .set(
                                &name,
                                &key,
                                rumps_types::Value::from(trial * 2 + 1),
                            )
                            .await
                            .unwrap();
                    });
                });

                // Spawn two threads that will try to commit at the same time
                let b1 = Arc::clone(&barrier);
                let sc1 = Arc::clone(&success_count);
                let handle_a = std::thread::spawn(move || {
                    b1.wait(); // Synchronize with other thread
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = rt.block_on(txn_a.commit());
                    if result.is_ok() {
                        sc1.fetch_add(1, Ordering::SeqCst);
                    }
                });

                let b2 = Arc::clone(&barrier);
                let sc2 = Arc::clone(&success_count);
                let handle_b = std::thread::spawn(move || {
                    b2.wait(); // Synchronize with other thread
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = rt.block_on(txn_b.commit());
                    if result.is_ok() {
                        sc2.fetch_add(1, Ordering::SeqCst);
                    }
                });

                handle_a.join().unwrap();
                handle_b.join().unwrap();

                let successes = success_count.load(Ordering::SeqCst);
                if successes == 2 {
                    both_succeeded += 1;
                } else if successes == 1 {
                    one_succeeded += 1;
                }
            });

            // With atomic validation, exactly one should succeed each trial.
            // If the race condition existed, we'd see both_succeeded > 0.
            assert_eq!(
                both_succeeded, 0,
                "Race condition detected! Both transactions succeeded {} times out of {}",
                both_succeeded, n_trials
            );
            assert_eq!(
                one_succeeded, n_trials as usize,
                "Expected exactly one success per trial"
            );
        }

        /// Concurrent commits across multiple globals should parallelize.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn concurrent_commits_multi_global() {
            let db = Arc::new(Database::in_memory().unwrap());
            let n = 10i64;

            // Each transaction writes to its own global
            let mut results = Vec::with_capacity(n as usize);
            (0..n).into_iter().for_each(|i| {
                let db = Arc::clone(&db);
                let r = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let name =
                            rumps_types::Name::Global(format!("GLOBAL{}", i));
                        db.build_transaction()
                            .begin(|txn| {
                                let name = name.clone();
                                async move {
                                    txn.set(
                                        &name,
                                        &rumps_types::key![1],
                                        rumps_types::Value::from(i),
                                    )
                                    .await?;
                                    Ok(())
                                }
                            })
                            .await
                    })
                });
                results.push(r);
            });

            // All should succeed (no overlapping keys)
            results.iter().for_each(|r| {
                assert!(
                    r.is_ok(),
                    "All multi-global transactions should commit"
                );
            });

            // Verify values
            (0..n).into_iter().for_each(|i| {
                let db = Arc::clone(&db);
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let name =
                            rumps_types::Name::Global(format!("GLOBAL{}", i));
                        let val =
                            db.get(&name, &rumps_types::key![1]).await.unwrap();
                        assert_eq!(val, Some(rumps_types::Value::from(i)));
                    });
                });
            });
        }
    }
}
