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

use rumps_types::Name;
use tokio::sync::RwLock;

use crate::btree::BTree;
use crate::engine::{AsyncStorageEngine, FileStorageEngine, StorageConfig};
use crate::error::Result;
use crate::node::NodeId;

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
        Self::with_btree(Arc::new(BTree::new(3)?))
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

        let btree = Arc::new(BTree::with_storage(
            3,
            Arc::clone(&storage) as Arc<dyn crate::engine::AsyncStorageEngine>,
        )?);

        Ok(Self {
            roots: RwLock::new(BTreeMap::new()),
            btree,
            storage: Some(storage),
        })
    }

    /// Opens an existing persistent database from the specified path.
    ///
    /// Loads the database from disk. Globals are lazy-loaded from the
    /// registry on first access.
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

        let btree = Arc::new(BTree::with_storage(
            3,
            Arc::clone(&storage) as Arc<dyn crate::engine::AsyncStorageEngine>,
        )?);

        Ok(Self {
            roots: RwLock::new(BTreeMap::new()),
            btree,
            storage: Some(storage),
        })
    }

    /// Returns the underlying B-tree.
    pub(crate) fn btree(&self) -> Arc<BTree> {
        Arc::clone(&self.btree)
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
        if let (Name::Global(g), Some(_storage)) = (name, self.storage.as_ref())
        {
            self.storage.as_ref().unwrap().registry_remove(g).await?;
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

    /// Flushes all dirty pages and metadata to disk.
    ///
    /// For persistent databases, this ensures all pending writes are
    /// committed to disk. For in-memory databases, this is a no-op.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// db.flush().await?;
    /// ```
    pub(crate) async fn flush(&self) -> Result<()> {
        if let Some(storage) = self.storage.as_ref() {
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
}
