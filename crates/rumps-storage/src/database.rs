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
use std::sync::Arc;

use rumps_types::Name;
use tokio::sync::RwLock;

use crate::btree::BTree;
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
    // TODO Phase 4.6: Add storage for persistence
    // storage: Option<Arc<FileStorageEngine>>,
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
        })
    }

    /// Returns the underlying B-tree.
    pub(crate) fn btree(&self) -> Arc<BTree> {
        Arc::clone(&self.btree)
    }

    /// Looks up the root `NodeId` for a variable name.
    ///
    /// For in-memory databases, simply checks the local cache.
    /// For persistent databases (future), will lazy-load globals from
    /// the registry if not already cached.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(root))` - Root exists
    /// - `Ok(None)` - Variable doesn't exist
    pub(crate) async fn get_root(&self, name: &Name) -> Result<Option<NodeId>> {
        let roots = self.roots.read().await;
        Ok(roots.get(name).copied())
        // TODO Phase 4.6: For globals with storage, lazy-load from registry
    }

    /// Gets or creates a root for a variable name.
    ///
    /// If the variable doesn't exist, creates a new empty tree and returns
    /// its root. For persistent globals (future), also registers the new
    /// root in the storage registry.
    ///
    /// # Returns
    ///
    /// The root `NodeId` for the variable (existing or newly created).
    pub(crate) async fn ensure_root(&self, name: &Name) -> Result<NodeId> {
        // Check cache first
        // TODO Phase 4.6: For globals with storage, lazy-load from registry first
        let cached = {
            let roots = self.roots.read().await;
            roots.get(name).copied()
        };

        match cached {
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

                // TODO Phase 4.6: For globals with storage, register in registry
                Ok(root)
            }
        }
    }

    /// Removes a variable's root from the cache.
    ///
    /// This does NOT delete the tree's nodes - use `delete_tree()` on
    /// the B-tree for that. This only removes the name → root mapping.
    ///
    /// For persistent globals (future), also removes from the registry.
    pub(crate) async fn remove_root(&self, name: &Name) -> Option<NodeId> {
        // TODO Phase 4.6: For globals with storage, remove from registry
        self.roots.write().await.remove(name)
    }

    /// Updates the root for a variable name.
    ///
    /// Called after operations that change the tree structure (splits,
    /// merges) which may result in a new root `NodeId`.
    pub(crate) async fn update_root(&self, name: &Name, new_root: NodeId) {
        let mut roots = self.roots.write().await;
        roots.insert(name.clone(), new_root);
        // TODO Phase 4.6: For globals with storage, update registry
    }

    /// Returns the number of cached roots (variables).
    pub(crate) async fn root_count(&self) -> usize {
        self.roots.read().await.len()
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

        let removed = db.remove_root(&name).await;
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

        db.update_root(&name, new_root).await;

        let fetched = db.get_root(&name).await.unwrap();
        assert_eq!(fetched, Some(new_root));
        assert_ne!(fetched, Some(root1));
    }
}
