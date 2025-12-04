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
use crate::error::Result;
use crate::node::{NodeData, NodeId};
use crate::transaction::{
    TransactionContext, TransactionId, TransactionTimestamp,
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
pub(crate) struct Database {
    /// Name → root `NodeId` mapping.
    ///
    /// For globals with persistent storage, entries are lazy-loaded from
    /// the registry on first access and cached here. For locals and
    /// in-memory databases, entries are created on demand.
    ///
    /// Uses `BTreeMap` for ordered iteration (MUMPS `$ORDER` over names).
    roots: RwLock<BTreeMap<Name, NodeId>>,

    /// The underlying B-tree (operates on `NodeId`s only).
    btree: Arc<BTree>,

    /// Optional storage engine for persistence.
    ///
    /// When `Some`, globals are persisted to disk and lazy-loaded from
    /// the registry. When `None`, all data is in-memory only.
    storage: Option<Arc<crate::engine::FileStorageEngine>>,
}

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
        Self::with_btree(Arc::new(BTreeBuilder::default().build()?))
    }

    /// Creates a database with a custom B-tree.
    ///
    /// Useful for testing with specific B-tree configurations.
    pub(crate) fn with_btree(btree: Arc<BTree>) -> Result<Self> {
        Ok(Self {
            roots: RwLock::new(BTreeMap::new()),
            btree,
            storage: None,
        })
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
    pub(crate) async fn create(path: impl AsRef<Path>) -> Result<Self> {
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
            roots: RwLock::new(BTreeMap::new()),
            btree,
            storage: Some(storage),
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
    pub(crate) async fn open(path: impl AsRef<Path>) -> Result<Self> {
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
            roots: RwLock::new(BTreeMap::new()),
            btree,
            storage: Some(Arc::clone(&storage)),
        };

        // Run WAL recovery and replay committed operations
        db.recover(path.as_ref()).await?;

        Ok(db)
    }

    /// Returns the underlying B-tree.
    pub(crate) fn btree(&self) -> Arc<BTree> {
        Arc::clone(&self.btree)
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
                            crate::error::StorageError::InvalidConfiguration(
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
    pub(crate) async fn get_root(&self, name: &Name) -> Result<Option<NodeId>> {
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
    pub(crate) async fn ensure_root(&self, name: &Name) -> Result<NodeId> {
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
    pub(crate) async fn remove_root(
        &self,
        name: &Name,
    ) -> Result<Option<NodeId>> {
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
    pub(crate) async fn update_root(
        &self,
        name: &Name,
        new_root: NodeId,
    ) -> Result<()> {
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
    pub(crate) async fn root_count(&self) -> usize {
        self.roots.read().await.len()
    }

    /// Sets a value in the database with WAL logging.
    ///
    /// For persistent globals, this:
    /// 1. Reads the old value for WAL undo log
    /// 2. Writes a SET record to the WAL (write-ahead!)
    /// 3. Modifies the in-memory B-tree
    /// 4. Updates the root if it changed
    ///
    /// For in-memory databases and locals, only step 3 happens (no WAL).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// db.set(&global!("PATIENT"), &key![123, "NAME"], Value::from("Bob")).await?;
    /// ```
    pub(crate) async fn set(
        &self,
        name: &Name,
        key: &Key,
        val: Value,
    ) -> Result<()> {
        // Ensure root exists
        let root = self.ensure_root(name).await?;

        // Get old value for WAL (only for globals with storage)
        let old_data = {
            let opt_arc = self.btree.get_internal(root, key).await?;
            opt_arc.map(|arc| (*arc).clone())
        };

        // Log to WAL before modifying tree (write-ahead!)
        if let (Name::Global(_), Some(storage)) = (name, self.storage.as_ref())
        {
            storage
                .wal_append(&WalRecord::Set {
                    // TODO Phase 5: Replace with actual transaction ID from context
                    txn_id: TransactionId::IMPLICIT,
                    name: name.clone(),
                    key: key.clone(),
                    old: old_data,
                    new: NodeData::with_value(val.clone()),
                })
                .await?;
        }

        // Create transaction context for BTree operation
        // TODO Phase 5: Replace with actual transaction context from user
        let ctx = TransactionContext::new(
            TransactionId::IMPLICIT,
            TransactionTimestamp::from(0),
        );

        // Modify in-memory tree
        let new_root = self.btree.set_at(root, key, val, &ctx).await?;

        // Update root if it changed
        if new_root != root {
            self.update_root(name, new_root).await?;
        }

        Ok(())
    }

    /// Deletes a key and all its descendants from the database with WAL logging.
    ///
    /// For persistent globals, this:
    /// 1. Collects all entries that will be deleted (key + descendants)
    /// 2. Writes a KillEntry record to WAL for each deleted entry (write-ahead!)
    /// 3. Deletes from the in-memory B-tree
    /// 4. Updates or removes the root
    ///
    /// For in-memory databases and locals, only step 3 happens (no WAL).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// db.kill(&global!("PATIENT"), &key![123]).await?;
    /// ```
    pub(crate) async fn kill(&self, name: &Name, key: &Key) -> Result<()> {
        let opt_root = self.get_root(name).await?;
        match opt_root {
            None => Ok(()), // No root = nothing to delete
            Some(root) => {
                // Collect all entries to be deleted for WAL logging
                let to_delete = match (name, self.storage.as_ref()) {
                    (Name::Global(_), Some(_)) => {
                        let mut entries = Vec::new();

                        // First, check if the key itself has data
                        if let Some(arc_data) =
                            self.btree.get_internal(root, key).await?
                        {
                            entries.push((key.clone(), (*arc_data).clone()));
                        }

                        // Then collect all descendants
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
                    _ => None, // Locals or in-memory - no WAL
                };

                // Log to WAL before deleting (write-ahead!)
                if let (Some(entries), Some(storage)) =
                    (to_delete, self.storage.as_ref())
                {
                    stream::iter(entries.iter().map(Ok))
                        .try_for_each(|(k, data)| async move {
                            storage
                                .wal_append(&WalRecord::KillEntry {
                                    // TODO Phase 5: Replace with actual transaction ID from context
                                    txn_id: TransactionId::IMPLICIT,
                                    name: name.clone(),
                                    key: k.clone(),
                                    data: data.clone(),
                                })
                                .await
                                .map(|_| ())
                        })
                        .await?;
                }

                // Create transaction context for BTree operation
                // TODO Phase 5: Replace with actual transaction context from user
                let ctx = TransactionContext::new(
                    TransactionId::IMPLICIT,
                    TransactionTimestamp::from(0),
                );

                // Delete from in-memory tree
                let opt_new_root = self.btree.kill_at(root, key, &ctx).await?;

                // Update or remove root
                match opt_new_root {
                    Some(new_root) if new_root != root => {
                        self.update_root(name, new_root).await
                    }
                    None => self.remove_root(name).await.map(|_| ()),
                    _ => Ok(()), // Root unchanged
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
    pub(crate) async fn collects<'a, P, F, T>(
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

    /// Flushes all dirty pages and metadata to disk.
    ///
    /// For persistent databases, this:
    /// 1. Writes a TxnCommit record to the WAL
    /// 2. Syncs the WAL to disk (durability!)
    /// 3. Flushes all dirty pages to the data file
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
            // Write commit record to WAL
            // TODO Phase 5: Replace with actual transaction ID from context
            storage
                .wal_append(&WalRecord::TxnCommit {
                    txn_id: TransactionId::IMPLICIT,
                })
                .await?;

            // Sync WAL to disk (durability!)
            // TODO Phase 5: Check storage.config.sync_mode:
            //   - OnCommit (default): call wal_sync() here
            //   - Immediate: already synced by WalWriter on each append
            //   - Periodic: skip sync here, external task handles it
            // For Phase 4.6 with IMPLICIT transaction, always sync (OnCommit behavior).
            storage.wal_sync().await?;

            // Flush dirty pages to data file
            storage.flush().await?;
        }
        Ok(())
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

        // Storage engine will be dropped, closing files
        Ok(())
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

        // Create database and perform operations
        {
            let db = Database::create(&db_path).await.unwrap();
            let name = global!("PATIENT");

            // SET operations
            db.set(&name, &key![1, "NAME"], Value::from("Alice"))
                .await
                .unwrap();
            db.set(&name, &key![1, "AGE"], Value::from(30))
                .await
                .unwrap();
            db.set(&name, &key![2, "NAME"], Value::from("Bob"))
                .await
                .unwrap();

            // Verify GET
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

            // KILL operation
            db.kill(&name, &key![1]).await.unwrap();

            // Verify deletion
            assert_eq!(db.get(&name, &key![1, "NAME"]).await.unwrap(), None);
            assert_eq!(db.get(&name, &key![1, "AGE"]).await.unwrap(), None);

            // Verify patient 2 still exists
            assert_eq!(
                db.get(&name, &key![2, "NAME"]).await.unwrap(),
                Some(Value::from("Bob"))
            );

            // Flush (writes TxnCommit + syncs WAL)
            db.flush().await.unwrap();
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
}
