//! Database layer providing namespace management over the B-tree.
//!
//! The `Database` struct owns the mapping from variable names (`Name`) to
//! B-tree roots (`NodeId`). This separates namespace concerns from the core
//! B-tree implementation, which operates purely on `NodeId`s.
//!
//! # Architecture
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

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use futures::stream::{self, Stream, StreamExt, TryStreamExt};
use rumps_types::{DataStatus, Key, Name, Value};
use tokio::sync::RwLock;

use crate::btree::{BTree, BTreeBuilder};
use crate::engine::{AsyncStorageEngine, FileStorageEngine, StorageConfig};
use crate::error::{Result, StorageError};
use crate::node::{NodeData, NodeId};
use crate::transaction::{
    Transaction, TransactionBuilder, TransactionContext, TransactionId,
    TransactionManager, TransactionTimestamp,
};
use crate::wal::{WalOp, WalReader, WalRecord};

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
}

// Public API
impl Database {
    /// Creates a new in-memory database.
    ///
    /// This database has no persistent storage - all data exists only in
    /// memory and is discarded when the database is dropped.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let db = Database::in_memory()?;
    /// ```
    pub(crate) fn in_memory() -> Result<Self> {
        Self::with_btree(
            Arc::new(BTreeBuilder::default().build()?),
            Arc::new(TransactionManager::default()),
        )
    }

    /// Creates a new persistent database at the specified path.
    ///
    /// Initializes a new database with disk-backed storage. Globals will be
    /// persisted to disk, while locals remain in-memory only.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let db = Database::create("./data").await?;
    /// ```
    pub async fn create(path: impl AsRef<Path>) -> Result<Self> {
        let storage = Arc::new(
            FileStorageEngine::create(path.as_ref(), StorageConfig::default())
                .await?,
        );

        let btree = Arc::new(
            BTreeBuilder::default()
                .storage(Arc::clone(&storage)
                    as Arc<dyn crate::engine::AsyncStorageEngine>)
                .build()?,
        );

        Ok(Self {
            roots: Arc::new(RwLock::new(BTreeMap::new())),
            btree,
            storage: Some(storage),
            txn_manager: Arc::new(TransactionManager::default()),
        })
    }

    /// Opens an existing persistent database from the specified path.
    ///
    /// Loads the database from disk, runs WAL recovery, and replays committed
    /// operations. Globals are lazy-loaded from the registry on first access.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let db = Database::open("./data").await?;
    /// ```
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let storage = Arc::new(
            FileStorageEngine::open(path.as_ref(), StorageConfig::default())
                .await?,
        );

        let btree = Arc::new(
            BTreeBuilder::default()
                .storage(Arc::clone(&storage)
                    as Arc<dyn crate::engine::AsyncStorageEngine>)
                .build()?,
        );

        let db = Self {
            roots: Arc::new(RwLock::new(BTreeMap::new())),
            btree,
            storage: Some(Arc::clone(&storage)),
            txn_manager: Arc::new(TransactionManager::default()),
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
    /// # Examples
    ///
    /// ```ignore
    /// db.close().await?;
    /// ```
    pub(crate) async fn close(self) -> Result<()> {
        // Flush all pending writes
        self.flush().await?;

        // Shutdown periodic sync task if running
        if let Some(storage) = self.storage.as_ref() {
            storage.shutdown_sync_task();
        }

        // Storage engine will be dropped, closing files
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
    /// ```ignore
    /// // For locals (no transaction needed):
    /// db.set(&local!("TEMP"), &key![1], Value::from("test")).await?;
    ///
    /// // For globals (use transaction):
    /// db.transaction(|txn| async move {
    ///     txn.set(&global!("PATIENT"), &key![123], Value::from("Bob")).await?;
    ///     Ok(())
    /// }).await?;
    /// ```
    pub(crate) async fn set(
        &self,
        name: &Name,
        key: &Key,
        val: Value,
    ) -> Result<()> {
        // Globals require transactions
        if matches!(name, Name::Global(_)) {
            Err(StorageError::GlobalRequiresTransaction)
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

    /// Deletes a key and all its descendants from the database.
    ///
    /// **Note**: Writes to globals require a transaction. Use `db.transaction()`
    /// or create a `Transaction` manually. Direct calls for globals will error.
    ///
    /// For locals (in-memory only), this deletes from the B-tree directly.
    ///
    /// # Errors
    ///
    /// Returns `GlobalRequiresTransaction` if attempting to write to a global
    /// variable without a transaction.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // For locals (no transaction needed):
    /// db.kill(&local!("TEMP"), &key![1]).await?;
    ///
    /// // For globals (use transaction):
    /// db.transaction(|txn| async move {
    ///     txn.kill(&global!("PATIENT"), &key![123]).await?;
    ///     Ok(())
    /// }).await?;
    /// ```
    pub(crate) async fn kill(&self, name: &Name, key: &Key) -> Result<()> {
        // Globals require transactions
        if matches!(name, Name::Global(_)) {
            Err(StorageError::GlobalRequiresTransaction)
        } else {
            // Locals can be deleted directly (no WAL, no persistence)
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

    /// Gets a value from the database (read-only, no WAL logging).
    ///
    /// Returns `None` if the variable or key doesn't exist.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let name = db.get(&global!("PATIENT"), &key![123, "NAME"]).await?;
    /// ```
    pub(crate) async fn get(
        &self,
        name: &Name,
        key: &Key,
    ) -> Result<Option<Value>> {
        let opt_root = self.get_root(name).await?;
        match opt_root {
            Some(root) => self.btree.get_at(root, key, None).await,
            None => Ok(None),
        }
    }

    /// Checks the data status of a node (MUMPS `$DATA`).
    ///
    /// Returns information about whether a node has a value and/or descendants.
    /// Read-only operation - no WAL logging.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let status = db.data(&global!("PATIENT"), &key![123]).await?;
    /// ```
    pub(crate) async fn data(
        &self,
        name: &Name,
        key: &Key,
    ) -> Result<DataStatus> {
        let opt_root = self.get_root(name).await?;
        match opt_root {
            Some(root) => self.btree.data_at(root, key, None).await,
            None => Ok(DataStatus::NoData),
        }
    }

    /// Returns the next key in lexicographic order (MUMPS `$ORDER`).
    ///
    /// Pass `None` as `after` to get the first key. Returns `None` when
    /// there are no more keys. Read-only operation - no WAL logging.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Get first key
    /// let first = db.order(&global!("PATIENT"), None).await?;
    ///
    /// // Get next key after [123]
    /// let next = db.order(&global!("PATIENT"), Some(&key![123])).await?;
    /// ```
    pub(crate) async fn order(
        &self,
        name: &Name,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        let opt_root = self.get_root(name).await?;
        match opt_root {
            Some(root) => self.btree.order_at(root, after, None).await,
            None => Ok(None),
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
    /// * `P` - Predicate: `(&Key, &NodeData) -> bool` (include if `true`)
    /// * `F` - Extract: `(&Key, &NodeData) -> Option<T>` (transform entry)
    /// * `T` - Output type yielded by the stream
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use futures::stream::StreamExt;
    ///
    /// let stream = db.collects(
    ///     &global!("PATIENT"),
    ///     None,
    ///     |key, _| key.len() == 2,  // Only keys with 2 subscripts
    ///     |key, data| data.value.clone(),  // Extract value
    /// ).await?;
    ///
    /// while let Some(value) = stream.next().await {
    ///     let v = value?;
    ///     println!("Value: {:?}", v);
    /// }
    /// ```
    pub async fn collects<'a, P, F, T>(
        &'a self,
        name: &'a Name,
        start: Option<&'a Key>,
        pred: P,
        extract: F,
    ) -> Result<impl Stream<Item = Result<T>> + Send + 'a>
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync + 'a,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        let opt_root = self.get_root(name).await?;
        let s = match opt_root {
            Some(root) => self
                .btree
                .collects_at(root, start, pred, extract, None)
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
        P: Fn(&Key, &NodeData) -> bool + Send + Sync,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync,
        T: Send,
    {
        let opt_root = self.get_root(name).await?;
        match opt_root {
            Some(root) => {
                self.btree
                    .collects_vec_at(root, start, pred, extract, None)
                    .await
            }
            None => Ok(Vec::new()),
        }
    }

    /// Flushes all dirty pages and metadata to disk.
    ///
    /// For persistent databases, this:
    /// 1. Syncs the WAL to disk (durability!)
    /// 2. Flushes all dirty pages to the data file
    ///
    /// This does NOT write any transaction records - it's a general-purpose
    /// flush for housekeeping (e.g., before `close()`). Transaction commits
    /// use `flush_with_txn()` which writes the appropriate `TxnCommit` record.
    ///
    /// For in-memory databases, this is a no-op.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// db.flush().await?;
    /// ```
    pub(crate) async fn flush(&self) -> Result<()> {
        if let Some(storage) = self.storage.as_ref() {
            storage.wal_sync().await?;
            storage.flush().await?;
        }
        Ok(())
    }

    /// Executes a function within a transaction context.
    ///
    /// The transaction auto-commits if the closure returns `Ok`, and
    /// auto-rollbacks if it returns `Err`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// db.transaction(|txn| async move {
    ///     txn.set(&global!("PATIENT"), &key![123, "NAME"], Value::from("Bob")).await?;
    ///     Ok(())
    /// }).await?;
    /// ```
    pub async fn transaction<F, Fut, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(Transaction) -> Fut,
        Fut: std::future::Future<Output = Result<R>>,
    {
        let txn = TransactionBuilder::default().begin(self).await?;
        match f(txn.clone()).await {
            Ok(result) => {
                txn.commit().await?;
                Ok(result)
            }
            Err(e) => {
                txn.rollback().await?;
                Err(e)
            }
        }
    }

    /// Executes a function within a configured transaction context.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let builder = TransactionBuilder::default()
    ///     .timeout(5000)
    ///     .priority(TransactionPriority::High);
    ///
    /// db.transaction_with(builder, |txn| async move {
    ///     txn.set(&global!("PATIENT"), &key![123, "NAME"], Value::from("Bob")).await?;
    ///     Ok(())
    /// }).await?;
    /// ```
    pub async fn transaction_with<F, Fut, R>(
        &self,
        builder: TransactionBuilder,
        f: F,
    ) -> Result<R>
    where
        F: FnOnce(Transaction) -> Fut,
        Fut: std::future::Future<Output = Result<R>>,
    {
        let txn = builder.begin(self).await?;
        match f(txn.clone()).await {
            Ok(result) => {
                txn.commit().await?;
                Ok(result)
            }
            Err(e) => {
                txn.rollback().await?;
                Err(e)
            }
        }
    }

    /// Creates a transaction builder for custom configuration.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let txn = db.build_transaction()
    ///     .timeout(5000)
    ///     .begin(&db)
    ///     .await?;
    /// ```
    pub(crate) fn build_transaction(&self) -> TransactionBuilder {
        TransactionBuilder::default()
    }
}

// Internal methods for transaction support
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
    ) -> Result<()> {
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
    ) -> Result<()> {
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
                            .collect::<Result<Vec<_>>>()?;

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
    ) -> Result<()> {
        if let Some(storage) = self.storage.as_ref() {
            storage.wal_append(&WalRecord::TxnCommit { txn_id }).await?;

            storage.wal_sync().await?;
            storage.flush().await?;
        }
        Ok(())
    }
}

// Private helpers
impl Database {
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
    ) -> Result<Self> {
        Ok(Self {
            roots: Arc::new(RwLock::new(BTreeMap::new())),
            btree,
            storage: None,
            txn_manager,
        })
    }

    /// Runs WAL recovery and replays committed operations.
    ///
    /// This is called during `open()` to bring the database to a consistent
    /// state after a crash or unclean shutdown.
    async fn recover(&self, path: &Path) -> Result<()> {
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
    async fn get_root(&self, name: &Name) -> Result<Option<NodeId>> {
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
    async fn ensure_root(&self, name: &Name) -> Result<NodeId> {
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
    async fn remove_root(&self, name: &Name) -> Result<Option<NodeId>> {
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
    async fn update_root(&self, name: &Name, new_root: NodeId) -> Result<()> {
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
/// - Uses `Arc::strong_count` to detect if this is the last handle
/// - Uses `tokio::task::block_in_place` to safely block on async I/O
/// - Errors are printed to stderr (no panic in drop)
///
/// # Explicit close
///
/// For proper error handling, call `db.close().await` explicitly. The `Drop`
/// impl is a best-effort fallback, not a replacement for explicit cleanup.
impl Drop for Database {
    fn drop(&mut self) {
        // Only flush if we're the last Database handle to this storage.
        self.storage
            .as_ref()
            .filter(|s| Arc::strong_count(s) == 1)
            .into_iter()
            .for_each(|storage| {
                let result = tokio::runtime::Handle::try_current()
                    .map_err(|e| StorageError::InvalidOperation(e.to_string()))
                    .and_then(|handle| {
                        tokio::task::block_in_place(|| {
                            handle.block_on(async {
                                storage.wal_sync().await?;
                                storage.flush().await
                            })
                        })
                    });

                if let Err(e) = result {
                    eprintln!("rumps: failed to flush database on drop: {e}");
                }
                storage.shutdown_sync_task();
            });
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
            Err(StorageError::GlobalRequiresTransaction)
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
                Err(StorageError::InvalidOperation(
                    "intentional failure".into(),
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
        let name = rumps_types::Name::global("TEST");
        let key = rumps_types::key![1];

        // Start transaction A (will commit second)
        let txn_a = db.build_transaction().begin(&db).await.unwrap();

        // Start transaction B (will commit first)
        let txn_b = db.build_transaction().begin(&db).await.unwrap();

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
        let name = rumps_types::Name::global("TEST");

        // Start both transactions
        let txn_a = db.build_transaction().begin(&db).await.unwrap();
        let txn_b = db.build_transaction().begin(&db).await.unwrap();

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
}
