use std::mem;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use futures::stream::FuturesUnordered;
use futures::{Stream, StreamExt, TryStreamExt};
use rumps_types::{DataStatus, Key, Subscript};
use smallvec::SmallVec;
#[cfg(feature = "debug")]
use tokio::sync::RwLock;

use crate::engine;
use crate::error::{Result, StorageError};
use crate::node::{ChildVec, KeyVec, Node, NodeData, NodeId, ValueVec};

/// Conditionally updates B-tree statistics when the `debug` feature is enabled.
///
/// When `debug` is disabled, this macro expands to nothing, eliminating
/// the write lock acquisition on every insert/delete operation.
///
/// # Usage
///
/// ```ignore
/// update_stats!(self, |stats| {
///     stats.node_count += 1;
///     stats.height = 1;
/// });
/// ```
macro_rules! update_stats {
    ($self:expr, |$stats:ident| $body:block) => {{
        #[cfg(feature = "debug")]
        {
            let mut $stats = $self.stats.write().await;
            $body
        }
        #[cfg(not(feature = "debug"))]
        {
            let _ = &$self; // suppress unused warning
        }
    }};
}

/// Lock-free node cache using `DashMap`.
///
/// Operations are synchronous (no `.await` needed) because `DashMap` uses
/// internal fine-grained sharding. Concurrent readers never block each other,
/// and operations complete immediately without yielding to the async runtime.
struct NodeCache {
    inner: DashMap<NodeId, Arc<Node>>,
}

impl NodeCache {
    fn new() -> Self {
        Self {
            inner: DashMap::with_capacity(1024),
        }
    }

    fn get(&self, id: NodeId) -> Option<Arc<Node>> {
        self.inner.get(&id).map(|r| Arc::clone(&*r))
    }

    fn insert(&self, id: NodeId, node: Arc<Node>) {
        self.inner.insert(id, node);
    }

    fn remove(&self, id: NodeId) -> Option<Arc<Node>> {
        self.inner.remove(&id).map(|(_, v)| v)
    }

    /// Modifies a node in-place using entry API.
    fn modify<F, T>(&self, id: NodeId, f: F) -> Result<T>
    where
        F: FnOnce(&mut Node) -> T,
    {
        self.inner
            .get_mut(&id)
            .map(|mut r| f(Arc::make_mut(&mut *r)))
            .ok_or_else(|| StorageError::NodeNotFound(id.into()))
    }

    /// Tries to modify a node in-place with a fallible closure.
    fn try_modify<F, T>(&self, id: NodeId, f: F) -> Result<T>
    where
        F: FnOnce(&mut Node) -> Result<T>,
    {
        self.inner
            .get_mut(&id)
            .ok_or_else(|| StorageError::NodeNotFound(id.into()))
            .and_then(|mut r| f(Arc::make_mut(&mut *r)))
    }

    fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests;

/// Statistics tracking for B-tree operations.
///
/// **Note**: Requires the `debug` feature to be enabled. Without it, all
/// fields will be zero (tracking is disabled to avoid lock contention).
#[derive(Debug, Clone, Default)]
pub struct BTreeStats {
    /// Current height of the tree.
    pub height: usize,
    /// Total number of nodes.
    pub node_count: usize,
    /// Total number of keys across all nodes.
    pub key_count: usize,
    /// Average fill factor (keys per node / max keys per node).
    pub avg_fill_factor: f64,
    /// Memory usage in bytes (estimated).
    pub memory_bytes: usize,
    /// Number of splits performed.
    pub splits: u64,
    /// Number of merges performed.
    pub merges: u64,
}

/// Node ID allocation strategy; either in-memory (incrementing) or
/// disk-backed (delegates to the storage engine's page allocator).
pub(crate) enum NodeAllocator {
    InMemory {
        next_id: AtomicU64,
    },
    Disk {
        storage: Arc<engine::FileStorageEngine>,
    },
}

impl NodeAllocator {
    pub(crate) fn in_memory() -> Self {
        Self::InMemory {
            next_id: AtomicU64::new(0),
        }
    }

    pub(crate) fn disk(storage: Arc<engine::FileStorageEngine>) -> Self {
        Self::Disk { storage }
    }

    pub(crate) async fn allocate(&self) -> Result<NodeId> {
        match self {
            Self::InMemory { next_id } => {
                Ok(NodeId::from(next_id.fetch_add(1, Ordering::Relaxed)))
            }
            Self::Disk { storage } => storage.allocate().await,
        }
    }

    pub(crate) async fn deallocate(&self, id: NodeId) -> Result<()> {
        match self {
            Self::InMemory { .. } => Ok(()),
            Self::Disk { storage } => storage.deallocate(id).await,
        }
    }

    pub(crate) async fn peek_next(&self) -> NodeId {
        match self {
            Self::InMemory { next_id } => {
                NodeId::from(next_id.load(Ordering::Relaxed))
            }
            Self::Disk { .. } => NodeId::from(0),
        }
    }
}

/// In-memory B-tree storage for RUMPS database.
///
/// This structure manages the hierarchical storage of B-tree nodes. It operates
/// purely on `NodeId`s—namespace management (mapping variable names to roots)
/// is handled by the `Database` layer.
///
/// # B-tree Properties
///
/// For a B-tree of minimum degree `t` (where `min_degree = t`):
/// - Each node (except root) contains `t-1` to `2t-1` keys
/// - Each internal node (except root) has `t` to `2t` children
/// - The root has `1` to `2t-1` keys (can be smaller)
/// - All leaves are at the same depth
/// - Keys within a node are sorted in ascending order
///
/// # Design Decisions
///
/// ## Root-Based API
///
/// All operations take a `root: NodeId` parameter instead of a `Name`. The
/// `Database` layer is responsible for mapping names to root `NodeId`s and
/// updating the mapping when tree operations change the root (splits/merges).
///
/// ## `DashMap` for nodes
///
/// Nodes are stored in a lock-free `DashMap`:
/// - `NodeId`s are arbitrary internal references (like page IDs)
/// - Logical ordering is maintained by the tree structure, not `NodeId` values
/// - `DashMap` uses internal fine-grained sharding for concurrent access
/// - Operations are synchronous, avoiding async scheduling overhead
///
/// # Thread Safety
///
/// The `BTree` is designed to be shared across threads using `Arc<BTree>`.
/// `DashMap` provides safe concurrent access with minimal contention.
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::BTree;
/// use std::sync::Arc;
///
/// # tokio_test::block_on(async {
/// // Create a B-tree with minimum degree 3
/// let btree = Arc::new(BTree::new(3)?);
/// assert_eq!(btree.min_degree(), 3);
///
/// // Create an empty tree and get its root
/// let root = btree.create_tree().await?;
///
/// // Operations use root-based API
/// let new_root = btree.set_at(root, &key, value, &ctx).await?;
/// # Ok::<(), rumps_storage::StorageError>(())
/// # });
/// ```
pub(crate) struct BTree {
    /// Lock-free node cache using `DashMap`.
    ///
    /// Operations are synchronous; `DashMap` uses internal fine-grained
    /// sharding for concurrent access without async overhead.
    ///
    /// `NodeId`s have no semantic ordering; the tree's logical ordering
    /// is maintained by parent-child links and sorted keys within each node.
    nodes: NodeCache,

    /// Optional storage engine for persistence.
    ///
    /// When `None`, the B-tree operates entirely in memory.
    /// When `Some`, nodes are persisted to disk and the `nodes` map
    /// becomes a page cache with lazy loading.
    storage: Option<Arc<engine::FileStorageEngine>>,

    /// Node ID allocator strategy.
    allocator: NodeAllocator,

    /// Minimum degree `t` of the B-tree.
    ///
    /// Nodes (except root) contain `t-1` to `2t-1` keys.
    /// Internal nodes (except root) have `t` to `2t` children.
    min_degree: usize,

    /// Optional memory limit in bytes.
    ///
    /// When set, operations will fail if memory usage exceeds this limit.
    /// This prevents unbounded growth in memory-constrained environments.
    max_memory_bytes: Option<usize>,

    /// Statistics tracking for monitoring and debugging.
    ///
    /// Only compiled when the `debug` feature is enabled. Tracking stats
    /// requires a write lock on every insert/delete, which adds significant
    /// overhead in write-heavy workloads.
    #[cfg(feature = "debug")]
    stats: RwLock<BTreeStats>,
}

/// Builder for constructing `BTree` instances.
///
/// This is the only way to create a `BTree`. Use the builder pattern to configure
/// the tree before calling `build()`.
///
/// # Examples
///
/// ```ignore
/// // In-memory tree with defaults (min_degree=3)
/// let btree = BTreeBuilder::default().build()?;
///
/// // In-memory tree with custom degree
/// let btree = BTreeBuilder::default()
///     .min_degree(5)
///     .build()?;
///
/// // Disk-backed tree
/// let btree = BTreeBuilder::default()
///     .min_degree(3)
///     .storage(storage_engine)
///     .build()?;
///
/// // Tree with memory limit
/// let btree = BTreeBuilder::default()
///     .max_memory_bytes(10_000_000)
///     .build()?;
/// ```
#[derive(Default)]
pub(crate) struct BTreeBuilder {
    min_degree: Option<usize>,
    storage: Option<Arc<engine::FileStorageEngine>>,
    max_memory_bytes: Option<usize>,
}

impl BTreeBuilder {
    /// Sets the minimum degree `t` of the B-tree.
    ///
    /// Nodes (except root) contain `t-1` to `2t-1` keys.
    /// Internal nodes (except root) have `t` to `2t` children.
    ///
    /// Default: `3` (nodes contain 2-5 keys, internal nodes have 3-6 children).
    ///
    /// # Panics
    ///
    /// Will return an error from `build()` if `min_degree < 2`.
    pub(crate) const fn min_degree(mut self, deg: usize) -> Self {
        self.min_degree = Some(deg);
        self
    }

    /// Sets the storage engine for disk persistence.
    ///
    /// When set, the tree becomes disk-backed with lazy-loading and page caching.
    /// When `None` (default), the tree operates entirely in memory.
    pub(crate) fn storage(mut self, s: Arc<engine::FileStorageEngine>) -> Self {
        self.storage = Some(s);
        self
    }

    /// Sets an optional memory limit in bytes.
    ///
    /// When set, operations will fail if memory usage exceeds this limit.
    /// Default: `None` (unlimited).
    pub(crate) const fn max_memory_bytes(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = Some(bytes);
        self
    }

    /// Builds the `BTree` with the configured settings. Fails if `min_degree < 2`.
    pub(crate) fn build(self) -> Result<BTree> {
        let min_degree = self.min_degree.unwrap_or(3);

        if min_degree < 2 {
            Err(StorageError::InvalidConfiguration(
                "min_degree must be >= 2".to_string(),
            ))
        } else {
            let allocator = match &self.storage {
                Some(storage) => NodeAllocator::disk(Arc::clone(storage)),
                None => NodeAllocator::in_memory(),
            };

            Ok(BTree {
                nodes: NodeCache::new(),
                storage: self.storage,
                allocator,
                min_degree,
                max_memory_bytes: self.max_memory_bytes,
                #[cfg(feature = "debug")]
                stats: RwLock::new(BTreeStats::default()),
            })
        }
    }
}

/// MUMPS primitive operations (`SET`, `GET`, `KILL`, `DATA`, `ORDER`).
///
/// Root-based API - All operations take a `root: NodeId` parameter.
/// Write operations may return a new root if the tree structure changes.
impl BTree {
    /// Sets a value at the specified key in the tree rooted at `root`.
    ///
    /// Returns the (possibly new) root `NodeId`. The root may change if
    /// the tree grows due to node splitting.
    ///
    /// **Requires a transaction context.** All writes to globals must
    /// occur within transactions.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let new_root = btree.set_at(root, &key, "John Doe".into(), &ctx).await?;
    /// // Update the root in the Database layer if it changed
    /// if new_root != root {
    ///     db.update_root(&name, new_root).await;
    /// }
    /// ```
    pub(crate) async fn set_at(
        &self,
        root: NodeId,
        key: &Key,
        value: rumps_types::Value,
        _ctx: &crate::TransactionContext,
    ) -> Result<NodeId> {
        // NOTE: BTree doesn't implement transaction logic. Snapshot isolation happens
        // at the Transaction layer (write buffering + read-from-buffer-first).
        // Context is accepted for metadata only (stores `txn_id` for future MVCC).
        self.set_internal(root, key, NodeData::with_value(value))
            .await
    }

    /// Gets a value from the tree rooted at `root`.
    ///
    /// Always reads committed state. Optional transaction context is for metadata
    /// only (snapshot isolation happens at Transaction layer).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let value = btree.get_at(root, &key, None).await?;
    /// ```
    pub(crate) async fn get_at(
        &self,
        root: NodeId,
        key: &Key,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<Option<rumps_types::Value>> {
        // NOTE: BTree always reads committed state. Snapshot isolation happens at
        // Transaction layer (`Transaction.get()` checks write buffer first, then calls
        // this).
        self.get_internal(root, key)
            .await
            .map(|opt| opt.and_then(|data| data.value.clone()))
    }

    /// Deletes a key and all its descendants from the tree rooted at `root`.
    ///
    /// Returns the (possibly new) root `NodeId`, or `None` if the tree
    /// became empty after the deletion.
    ///
    /// Modifies committed state directly. Transaction context is for metadata
    /// only (write buffering happens at Transaction layer).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// match btree.kill_at(root, &key, &ctx).await? {
    ///     Some(new_root) => db.update_root(&name, new_root).await,
    ///     None => db.remove_root(&name).await,
    /// }
    /// ```
    pub(crate) async fn kill_at(
        &self,
        root: NodeId,
        key: &Key,
        _ctx: &crate::TransactionContext,
    ) -> Result<Option<NodeId>> {
        // NOTE: BTree modifies committed state directly. Buffering happens at
        // Transaction layer (`Transaction.kill()` buffers, commit applies via `db.kill()`).
        // Context accepted for metadata only (stores `txn_id` for future MVCC).
        self.kill_internal(root, key).await
    }

    /// Checks the data status of a node (MUMPS `$DATA`).
    ///
    /// Returns information about whether a node has a value and/or descendants.
    /// Always reads committed state. Transaction context for metadata only.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let status = btree.data_at(root, &key, None).await?;
    /// ```
    pub(crate) async fn data_at(
        &self,
        root: NodeId,
        key: &Key,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<DataStatus> {
        // NOTE: Snapshot isolation at Transaction layer (checks write buffer).
        self.data_internal(root, key).await
    }

    /// Returns the next full key in lexicographic order (MUMPS `$QUERY`).
    ///
    /// Unlike `$ORDER` which returns the next subscript at a level, `$QUERY`
    /// returns the complete path to the next node with a value.
    ///
    /// # Intermediate Node Skipping
    ///
    /// Traditional MUMPS uses a B+-tree where values exist only at leaves,
    /// so iteration naturally visits only nodes with values. RUMPS uses a
    /// B-tree where intermediate ancestor nodes are stored explicitly (with
    /// `has_descendants = true` but `value = None`) to maintain hierarchical
    /// structure. This method skips such intermediate nodes to match MUMPS
    /// `$QUERY` semantics.
    ///
    /// For example, inserting `key![1, "name"]` creates an intermediate node
    /// at `key![1]`. Calling `$QUERY` will skip `[1]` and return `[1, "name"]`.
    ///
    /// Always reads committed state. Transaction context for metadata only.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let next_key = btree.query_at(root, Some(&key), None).await?;
    /// ```
    pub(crate) async fn query_at(
        &self,
        root: NodeId,
        after: Option<&Key>,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<Option<Key>> {
        // NOTE: Snapshot isolation at Transaction layer (merges write buffer
        // with committed state).
        self.query_with_value(root, after).await
    }

    /// Finds the next key that has a value, skipping intermediate ancestor nodes.
    ///
    /// Unlike a B+-tree where all keys at leaves have values, our B-tree stores
    /// intermediate nodes (ancestors with `has_descendants = true` but no value).
    /// This method uses `find_*_entry` to get key and data in a single traversal,
    /// then skips entries without values to match MUMPS `$QUERY` semantics.
    async fn query_with_value(
        &self,
        root: NodeId,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        let mut cur = after.cloned();
        let mut out = None;
        let mut again = Some(());

        while again.take().is_some() {
            let entry = match cur.as_ref() {
                None => self.find_leftmost_entry(root).await?,
                Some(key) => self.find_successor_entry(root, key).await?,
            };
            match entry {
                None => out = None,
                Some((key, data)) if data.value.is_some() => out = Some(key),
                Some((key, _)) => {
                    cur = Some(key);
                    again = Some(());
                }
            }
        }

        Ok(out)
    }

    /// Returns the next subscript at a specific level (MUMPS `$ORDER`).
    ///
    /// Unlike `$QUERY` which returns the full key path, `$ORDER` returns just
    /// the next subscript value at the level defined by `prefix`.
    ///
    /// Always reads committed state. Transaction context for metadata only.
    ///
    /// The `prefix` is the parent key path defining the level to iterate;
    /// `after` is the `Subscript` to start after, or `None` to get the first.
    ///
    /// # Examples
    ///
    /// ```text
    /// Tree contents of variable (i.e. local/global):
    ///   [1, "A"] = ...
    ///   [1, "B"] = ...
    ///   [2, "C"] = ...
    ///
    /// order_at(root, [], None)       → Some(1)    ; first subscript at root
    /// order_at(root, [], Some(1))    → Some(2)    ; next subscript at root
    /// order_at(root, [1], None)      → Some("A")  ; first subscript under [1]
    /// order_at(root, [1], Some("A")) → Some("B")  ; next subscript under [1]
    /// order_at(root, [1], Some("B")) → None       ; no more under [1]
    /// ```
    pub(crate) async fn order_at(
        &self,
        root: NodeId,
        prefix: &Key,
        after: Option<&Subscript>,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<Option<Subscript>> {
        self.order_internal(root, prefix, after).await
    }

    /// Creates a stream of key-value pairs from the tree (RUMPS `$COLLECT`).
    ///
    /// This is a RUMPS extension (not in traditional MUMPS) that provides
    /// stream-based iteration over tree entries. The stream yields entries
    /// that match the predicate, transformed by the extract function.
    ///
    /// Always reads committed state. Transaction context for metadata only.
    pub(crate) fn collects_at<'a, P, F, T>(
        &'a self,
        root: NodeId,
        start: Option<&'a Key>,
        pred: P,
        extract: F,
        _ctx: Option<&'a crate::TransactionContext>,
    ) -> impl Stream<Item = Result<T>> + Send + 'a
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync + 'a,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        self.collects_internal(root, start, pred, extract)
    }

    /// Collects all matching entries into a `Vec`.
    ///
    /// Convenience wrapper around `collects_at` that consumes the entire stream.
    /// Use `collects_at` directly for large datasets to avoid loading everything
    /// into memory.
    pub(crate) async fn collects_vec_at<P, F, T>(
        &self,
        root: NodeId,
        start: Option<&Key>,
        pred: P,
        extract: F,
        ctx: Option<&crate::TransactionContext>,
    ) -> Result<Vec<T>>
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync,
        T: Send,
    {
        self.collects_at(root, start, pred, extract, ctx)
            .try_collect()
            .await
    }

    /// Collects entries matching a key prefix into a `Vec`.
    ///
    /// Unlike `collects_vec_at`, this method **stops iteration** as soon as
    /// a key is encountered that doesn't start with the prefix. This is much
    /// more efficient for prefix-based queries because it doesn't scan the
    /// entire tree after the prefix range.
    pub(crate) async fn collects_prefix_vec_at<F, T>(
        &self,
        root: NodeId,
        prefix: &Key,
        extract: F,
    ) -> Result<Vec<T>>
    where
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync,
        T: Send,
    {
        self.collects_prefix_at(root, prefix, extract)
            .try_collect()
            .await
    }

    /// Creates a stream of entries matching a key prefix.
    ///
    /// This method efficiently iterates entries whose keys start with `prefix`,
    /// terminating as soon as a non-matching key is encountered. The iteration
    /// seeks to the prefix position first (O(log n)), then yields matching
    /// entries until the prefix range ends.
    pub(crate) fn collects_prefix_at<'a, F, T>(
        &'a self,
        root: NodeId,
        prefix: &'a Key,
        extract: F,
    ) -> impl Stream<Item = Result<T>> + Send + 'a
    where
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        self.collects_prefix_internal(root, prefix, extract)
    }
}

// Public utilities
impl BTree {
    /// Loads a node from cache or disk.
    ///
    /// First checks the in-memory `nodes` cache. On cache miss, loads from
    /// storage (if available) and adds to cache. Returns
    /// `StorageError::NodeNotFound` if absent from both.
    async fn load_node(&self, id: NodeId) -> Result<Arc<Node>> {
        // Check cache first; on miss, load from disk
        match self.nodes.get(id) {
            Some(node) => Ok(node),
            None => {
                let storage = self
                    .storage
                    .as_ref()
                    .ok_or(StorageError::NodeNotFound(*id))?;

                let node = storage.read(id).await?;
                self.nodes.insert(id, Arc::clone(&node));
                Ok(node)
            }
        }
    }

    /// Saves a node to the in-memory cache and marks it dirty.
    ///
    /// **BTree is purely in-memory**; it does NOT write to disk directly.
    /// Disk writes happen during checkpoint/flush at the Database layer.
    ///
    /// For disk-backed trees, the page is marked dirty in the storage engine.
    /// For in-memory trees, just updates the node map.
    async fn save_node(&self, id: NodeId, node: Node) -> Result<()> {
        let node = Arc::new(node);

        if let Some(storage) = self.storage.as_ref() {
            storage.mark_dirty(id);
        }

        self.nodes.insert(id, node);
        Ok(())
    }

    /// Resolves a node from the in-memory cache (`DashMap`).
    ///
    /// Used by the flush path to get node data for dirty pages.
    pub(crate) fn resolve_node(&self, id: NodeId) -> Option<Arc<Node>> {
        self.nodes.get(id)
    }

    /// Creates a new empty tree and returns its root `NodeId`.
    ///
    /// This allocates an empty leaf node that serves as the root of a new tree.
    /// The `Database` layer should call this when creating a new variable.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let root = btree.create_tree().await?;
    /// // Now use `root` with `set_at()`, `get_at()`, etc.
    /// ```
    pub(crate) async fn create_tree(&self) -> Result<NodeId> {
        let id = self.allocator.allocate().await?;
        let node = Node::new_leaf();

        self.save_node(id, node).await?;

        update_stats!(self, |stats| {
            stats.node_count += 1;
            stats.height = 1;
        });

        Ok(id)
    }

    /// Deletes an entire tree rooted at `root`, deallocating all nodes.
    ///
    /// Returns the number of nodes that were deallocated. After this call,
    /// the `root` `NodeId` is invalid and must not be used.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let freed = btree.delete_tree(root).await?;
    /// println!("Freed {} nodes", freed);
    /// ```
    pub(crate) async fn delete_tree(&self, root: NodeId) -> Result<usize> {
        self.delete_subtree(root).await
    }

    /// Deletes a subtree rooted at `node_id`, deallocating all nodes.
    async fn delete_subtree(&self, node_id: NodeId) -> Result<usize> {
        let mut cur = vec![node_id];
        let mut n = 0;

        while let Some(batch) =
            Some(mem::take(&mut cur)).filter(|b| !b.is_empty())
        {
            let loads: FuturesUnordered<_> = batch
                .iter()
                .map(|&id| async move { (id, self.load_node(id).await) })
                .collect();

            let results: Vec<_> = loads.collect().await;

            results.into_iter().try_for_each(|(id, res)| match res {
                Err(StorageError::NodeNotFound(_)) => Ok(()),
                Err(e) => Err(e),
                Ok(node) => {
                    self.nodes.remove(id);
                    if !node.is_leaf {
                        cur.extend(node.children.iter().copied());
                    }

                    update_stats!(self, |stats| {
                        stats.node_count = stats.node_count.saturating_sub(1);
                        stats.key_count =
                            stats.key_count.saturating_sub(node.keys.len());
                    });

                    n += 1;
                    Ok(())
                }
            })?;

            let deallocs: FuturesUnordered<_> = batch
                .iter()
                .map(|&id| self.allocator.deallocate(id))
                .collect();
            deallocs.try_collect::<Vec<_>>().await?;
        }

        Ok(n)
    }

    /// Returns the minimum degree of this B-tree.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(4)?;
    /// assert_eq!(btree.min_degree(), 4);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) fn min_degree(&self) -> usize {
        self.min_degree
    }

    /// Returns the total number of nodes in this B-tree.
    ///
    /// This is an async method because it requires acquiring a read lock
    /// on the nodes collection.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3)?;
    /// assert_eq!(btree.node_count(), 0);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Returns whether a memory limit is configured.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3)?;
    /// assert!(!btree.has_memory_limit());
    ///
    /// let btree = BTree::with_config(3, Some(1000000))?;
    /// assert!(btree.has_memory_limit());
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) fn has_memory_limit(&self) -> bool {
        self.max_memory_bytes.is_some()
    }

    /// Returns the current statistics for this B-tree.
    ///
    /// When the `debug` feature is disabled, returns default (zero) stats.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3)?;
    /// let stats = btree.stats().await;
    /// assert_eq!(stats.node_count, 0);
    /// assert_eq!(stats.key_count, 0);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) async fn stats(&self) -> BTreeStats {
        #[cfg(feature = "debug")]
        {
            self.stats.read().await.clone()
        }
        #[cfg(not(feature = "debug"))]
        {
            BTreeStats::default()
        }
    }
}

/// Direction for sibling borrowing during B-tree rebalancing.
/// (used in private `impl` methods below)
enum BorrowDir {
    Left,
    Right,
}

/// One level in a `Cursor`'s descent stack.
#[derive(Clone, Copy)]
pub(crate) struct StackEntry {
    pub(crate) node: NodeId,
    pub(crate) idx: usize,
    pub(crate) len: usize,
}

/// A cursor for in-order traversal of a B-tree.
///
/// Holds a stack of `StackEntry` frames from root to the current
/// position. Nodes are loaded on demand via `BTree::load_node`;
/// the `DashMap` cache makes re-loads `O(1)`.
pub(crate) struct Cursor<'a> {
    stack: SmallVec<[StackEntry; 8]>,
    tree: &'a BTree,
    exhausted: bool,
}

impl<'a> Cursor<'a> {
    /// Positions the cursor at the first key `>= target`, or the
    /// leftmost key when `target` is `None`.
    pub(crate) async fn seek(
        tree: &'a BTree,
        root: NodeId,
        target: Option<&Key>,
    ) -> Result<Cursor<'a>> {
        let mut stack: SmallVec<[StackEntry; 8]> = SmallVec::new();

        // Both `None` (leftmost) and `Some(key)` (first `>=`) follow the
        // same descent pattern: load node, compute position, push, maybe
        // descend into a child. The only difference is how `pos` and the
        // "keep descending" condition are determined.
        let mut cur = Some(root);
        while let Some(id) = cur.take() {
            let node = tree.load_node(id).await?;
            let (pos, descend) = match target {
                None => (0, !node.is_leaf),
                Some(key) => {
                    let res = node.keys.binary_search(key);
                    let pos = res.unwrap_or_else(|p| p);
                    (pos, res.is_err() && !node.is_leaf)
                }
            };
            stack.push(StackEntry {
                node: id,
                idx: pos,
                len: node.keys.len(),
            });
            cur = if descend {
                Some(node.children.get(pos).copied().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Child index out of bounds during seek".into(),
                    )
                })?)
            } else {
                None
            };
        }

        // Pop entries past the end of their node's keys.
        while stack.last().is_some_and(|e| e.idx >= e.len) {
            stack.pop();
        }
        let exhausted = stack.is_empty();

        Ok(Cursor {
            stack,
            tree,
            exhausted,
        })
    }
}

/// Private utilities for B-tree operations
impl BTree {
    /// Internal GET that returns `Arc<NodeData>` (not just `Value`).
    ///
    /// Returns an `Arc` for efficient hierarchy navigation - checking
    /// `has_descendants` flags is much cheaper with `Arc::clone()` than
    /// cloning the entire `NodeData`.
    ///
    /// The public `get_at()` method extracts the value by cloning the
    /// `Option<Value>` from the `Arc`.
    ///
    /// This is pub(crate) so Database can use it to get full NodeData for WAL logging.
    pub(crate) async fn get_internal(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<Option<Arc<NodeData>>> {
        self.search_from_node(root, key).await
    }

    /// Internal SET operation that accepts `NodeData` directly.
    ///
    /// Returns the (possibly new) root `NodeId`. The root may change if
    /// the tree grows due to node splitting.
    ///
    /// # Behavior for Existing Keys - Idempotent Merge
    ///
    /// If the key already exists, this method MERGES the `NodeData`:
    /// - `has_descendants`: Performs OR operation (if either old or new is `true`, result is `true`)
    /// - `value`: Takes new value if provided, otherwise keeps old value
    ///
    /// This ensures that:
    /// 1. Setting `has_descendants = true` is permanent (can't be undone by another set)
    /// 2. Concurrent ancestor creation is safe (multiple operations can set same ancestor)
    /// 3. User can update values without losing `has_descendants` flag
    // Internal method for tests/benchmarks - not part of public API
    async fn set_internal(
        &self,
        root: NodeId,
        key: &Key,
        data: NodeData,
    ) -> Result<NodeId> {
        // Ensure all ancestors exist with `has_descendants = true`
        // This is part of the core MUMPS hierarchical semantics
        let root = self.ensure_ancestors(root, key).await?;

        // Delegate to raw insertion (no hierarchy management)
        self.set_at_node(root, key, data).await
    }

    /// Internal KILL operation that deletes a key and all its descendants.
    ///
    /// Returns the (possibly new) root `NodeId`, or `None` if the tree
    /// became empty after the deletion.
    ///
    /// Implements MUMPS `KILL` semantics via single-pass subtree drop.
    ///
    /// # Previous implementation
    ///
    /// Collected every prefix-matching key into a `Vec`, sorted deepest-first,
    /// then called `delete_key` per key; each deletion was a full root-to-leaf
    /// traversal plus rebalance. For N matching keys: O(N * log N) tree ops.
    ///
    /// # Current approach
    ///
    /// A single top-down walk (`remove_prefix_range`) that exploits the fact
    /// that prefix-matching keys form a **contiguous range** in the B-tree's
    /// sorted key space:
    ///
    /// 1. At each internal node, identify matching keys `[start, end)`.
    /// 2. Children `c[start+1..end]` lie entirely within the range; deallocate
    ///    their whole subtrees via `delete_subtree` (no per-key work).
    /// 3. The two **boundary** children `c[start]` and `c[end]` may partially
    ///    overlap the range; descend into them (iterative explicit stack).
    /// 4. At leaves, batch-drain the matching slice in one operation.
    /// 5. On the way back up (`Fixup`), merge the two boundary children
    ///    (separators between them were removed) and rebalance.
    ///
    /// Complexity: O(height * t) individual key removals for the boundary
    /// nodes at each level, plus O(M) to deallocate M interior B-tree nodes
    /// wholesale. The bulk of the keys (those inside interior subtrees) are
    /// never individually traversed.
    ///
    /// After removal, `shrink_root_chain` collapses any empty root chain, and
    /// `update_ancestors_after_kill` fixes `has_descendants` flags on the
    /// MUMPS-level ancestors (O(key_depth) lookups).
    async fn kill_internal(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<Option<NodeId>> {
        let removed = self.remove_prefix_range(root, key).await?;

        if removed == 0 {
            Ok(Some(root))
        } else {
            let new_root = self.shrink_root_chain(root).await?;
            match new_root {
                None => Ok(None),
                Some(r) => self.update_ancestors_after_kill(r, key).await,
            }
        }
    }

    /// Removes all keys matching `prefix` from the subtree rooted at `node_id`.
    /// Returns the total number of keys removed.
    ///
    /// # Algorithm (iterative explicit-stack DFS)
    ///
    /// Two stack-frame types mirror the "descend / return" phases of the
    /// conceptual recursion:
    ///
    /// **`Visit(nid)`** (descend phase):
    ///   Binary-search the node's keys to find the contiguous matching range
    ///   `keys[start..end]`. Three child categories follow from B-tree ordering:
    ///
    ///   - **Boundary left** `c[start]`: contains keys `< keys[start]`, some of
    ///     which may still match the prefix (e.g. prefix `[1]` with separator
    ///     `[1,"a"]`; child may hold `[1]` itself). Must descend.
    ///   - **Interior** `c[start+1..end]`: sandwiched between two matching
    ///     separators, so *every* key in these subtrees matches the prefix.
    ///     Deallocated wholesale via `delete_subtree`; no per-key work.
    ///   - **Boundary right** `c[end]`: contains keys `> keys[end-1]`, some of
    ///     which may still match. Must descend. Only exists when `n_match > 0`.
    ///
    ///   Pushes `[Fixup, Visit(right), Visit(left)]` so LIFO order processes
    ///   left first, right second, fixup last.
    ///
    /// **`Fixup { .. }`** (return phase):
    ///   Runs after both boundary children are clean. Removes the matching
    ///   separators from this node and merges the two boundary children (whose
    ///   separators were just removed, leaving them adjacent with no key between
    ///   them) via `merge_boundary_pair`. Then rebalances the merged child if
    ///   it ended up underfull.
    async fn remove_prefix_range(
        &self,
        node_id: NodeId,
        prefix: &Key,
    ) -> Result<usize> {
        enum Op {
            Visit(NodeId),
            Fixup {
                id: NodeId,
                start: usize,
                end: usize,
                n_match: usize,
                left: NodeId,
                right: Option<NodeId>,
            },
        }

        let mut stack = vec![Op::Visit(node_id)];
        let mut total = 0usize;

        while let Some(op) = stack.pop() {
            match op {
                Op::Visit(nid) => {
                    let node = self.load_node(nid).await?;
                    let start =
                        node.keys.binary_search(prefix).unwrap_or_else(|p| p);
                    let n_match = node
                        .keys
                        .iter()
                        .skip(start)
                        .take_while(|k| k.starts_with(prefix))
                        .count();
                    let end = start + n_match;

                    if node.is_leaf {
                        if n_match > 0 {
                            self.nodes.modify(nid, |n| {
                                n.keys.drain(start..end);
                                n.values.drain(start..end);
                            })?;
                            update_stats!(self, |stats| {
                                stats.key_count =
                                    stats.key_count.saturating_sub(n_match);
                            });
                            total += n_match;
                        }
                    } else {
                        let left =
                            *node.children.get(start).ok_or_else(|| {
                                StorageError::InvalidOperation(
                                    "Left boundary child missing".into(),
                                )
                            })?;
                        let right = (n_match > 0)
                            .then(|| {
                                node.children.get(end).copied().ok_or_else(
                                    || {
                                        StorageError::InvalidOperation(
                                            "Right boundary child missing"
                                                .into(),
                                        )
                                    },
                                )
                            })
                            .transpose()?;
                        let interior: Vec<NodeId> = ((start + 1)..end)
                            .filter_map(|i| node.children.get(i).copied())
                            .collect();
                        drop(node);

                        // All keys in interior subtrees match the prefix
                        // (they sit between two matching separators in the
                        // parent), so we can deallocate them wholesale.
                        total += futures::stream::iter(
                            interior.into_iter().map(Ok::<_, StorageError>),
                        )
                        .try_fold(0usize, |acc, c| async move {
                            self.delete_subtree(c).await.map(|n| acc + n)
                        })
                        .await?;

                        // LIFO: left visits first, then right, then fixup
                        stack.push(Op::Fixup {
                            id: nid,
                            start,
                            end,
                            n_match,
                            left,
                            right,
                        });
                        if let Some(rc) = right {
                            stack.push(Op::Visit(rc));
                        }
                        stack.push(Op::Visit(left));
                    }
                }
                Op::Fixup {
                    id,
                    start,
                    end,
                    n_match,
                    left,
                    right,
                } => {
                    if n_match > 0 {
                        // Matching separators and their (now-deallocated)
                        // interior children must be removed from this node.
                        // The two boundary children, stripped of matching
                        // keys by the visits above, are merged back together.
                        match right {
                            Some(rc) => {
                                self.merge_boundary_pair(
                                    id, start, end, left, rc,
                                )
                                .await?;
                            }
                            None => {
                                // `right` is `None` only when `n_match == 0`,
                                // which this branch doesn't reach. Defensive
                                // fallback: remove keys + interior children.
                                self.nodes.modify(id, |n| {
                                    n.keys.drain(start..end);
                                    n.values.drain(start..end);
                                    if end > start + 1 {
                                        n.children.drain((start + 1)..end);
                                    }
                                })?;
                            }
                        }

                        update_stats!(self, |stats| {
                            stats.key_count =
                                stats.key_count.saturating_sub(n_match);
                        });
                        total += n_match;
                    }

                    // The merged (or sole boundary) child at `start` may be
                    // underfull after losing its prefix-matching keys. Fix it
                    // now so the invariant holds when our caller inspects us.
                    let parent = self.load_node(id).await?;
                    if let Some(&cid) = parent.children.get(start) {
                        let underfull = self.load_node(cid).await?.keys.len()
                            < self.min_degree - 1;
                        let has_keys = !parent.keys.is_empty();
                        drop(parent);
                        if underfull && has_keys {
                            self.fix_underfull_node(id, start).await?;
                        }
                    }
                }
            }
        }

        Ok(total)
    }

    /// Merges two boundary children after prefix-range removal from their parent.
    ///
    /// When a contiguous range of separators `keys[start..end]` is removed from
    /// an internal node, the boundary children `c[start]` and `c[end]` become
    /// adjacent with no separator between them; that violates the B-tree
    /// invariant (N+1 children for N keys). This function restores it.
    ///
    /// **Leaf case**: direct concatenation (or median split on overflow).
    ///
    /// **Internal case**: delegates to `merge_inner_boundary`, which descends
    /// along the inner boundary (left's rightmost child, right's leftmost child)
    /// to the leaf level, concatenates there, then absorbs each right node into
    /// its left counterpart on the way back up. At each internal level, right's
    /// first key provides the separator; passthroughs (0 keys, 1 child left
    /// after prefix removal) contribute nothing and are simply deallocated.
    /// If any level overflows, it is split and the extra child propagated up.
    async fn merge_boundary_pair(
        &self,
        parent_id: NodeId,
        start: usize,
        end: usize,
        left_id: NodeId,
        right_id: NodeId,
    ) -> Result<()> {
        let l = Arc::unwrap_or_clone(self.load_node(left_id).await?);

        if !l.is_leaf {
            // Internal nodes cannot be concatenated directly: N keys on
            // each side give (N+1)+(N+1) children for only 2N keys,
            // violating the B-tree invariant.  Instead, descend to the
            // inner boundary (left's rightmost child, right's leftmost
            // child) until reaching leaves, merge there, then absorb
            // right into left at each level going back up.
            drop(l);
            let pending = self.merge_inner_boundary(left_id, right_id).await?;

            self.nodes.modify(parent_id, |p| {
                p.keys.drain(start..end);
                p.values.drain(start..end);
                p.children.drain((start + 1)..=end);
                if let Some((sk, sv, nr)) = pending {
                    p.keys.insert(start, sk);
                    p.values.insert(start, sv);
                    p.children.insert(start + 1, nr);
                }
            })?;

            Ok(())
        } else {
            // Leaves: direct concatenation (or split on overflow).
            let r = Arc::unwrap_or_clone(self.load_node(right_id).await?);
            let max = 2 * self.min_degree - 1;
            let total = l.keys.len() + r.keys.len();

            if total <= max {
                let Node {
                    keys: rk,
                    values: rv,
                    ..
                } = r;

                self.nodes.modify(left_id, |n| {
                    n.keys.extend(rk);
                    n.values.extend(rv);
                })?;
                self.nodes.remove(right_id);
                self.allocator.deallocate(right_id).await?;

                update_stats!(self, |stats| {
                    stats.node_count = stats.node_count.saturating_sub(1);
                });

                self.nodes.modify(parent_id, |p| {
                    p.keys.drain(start..end);
                    p.values.drain(start..end);
                    p.children.drain((start + 1)..=end);
                })?;

                Ok(())
            } else {
                let mut keys: KeyVec = l.keys;
                keys.extend(r.keys);
                let mut vals: ValueVec = l.values;
                vals.extend(r.values);

                let mid = keys.len() / 2;

                let rk: KeyVec = keys.drain((mid + 1)..).collect();
                let rv: ValueVec = vals.drain((mid + 1)..).collect();

                let sep_key = keys.pop().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Empty keys during boundary split".into(),
                    )
                })?;
                let sep_val = vals.pop().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Empty vals during boundary split".into(),
                    )
                })?;

                self.save_node(
                    left_id,
                    Node {
                        keys,
                        children: ChildVec::new(),
                        values: vals,
                        is_leaf: true,
                    },
                )
                .await?;
                self.save_node(
                    right_id,
                    Node {
                        keys: rk,
                        children: ChildVec::new(),
                        values: rv,
                        is_leaf: true,
                    },
                )
                .await?;

                self.nodes.modify(parent_id, |p| {
                    p.keys.drain(start..end);
                    p.values.drain(start..end);
                    if end > start + 1 {
                        p.children.drain((start + 1)..end);
                    }
                    p.keys.insert(start, sep_key);
                    p.values.insert(start, sep_val);
                })?;

                Ok(())
            }
        }
    }

    /// Merges two internal boundary nodes by descending their inner
    /// boundary (left's rightmost child, right's leftmost child) to
    /// the leaf level, concatenating there, then absorbing each right
    /// node into its left counterpart on the way back up.
    ///
    /// At each internal level the right node's first key serves as the
    /// separator between the two children sets; passthroughs (0 keys,
    /// 1 child) contribute nothing and are simply deallocated.
    ///
    /// Returns `Some(sep, val, new_right)` if the top-level result
    /// overflowed and was split; `None` if everything fit into `left`.
    async fn merge_inner_boundary(
        &self,
        left_id: NodeId,
        right_id: NodeId,
    ) -> Result<Option<(Key, Arc<NodeData>, NodeId)>> {
        let max = 2 * self.min_degree - 1;

        // Collect the descent spine: (left, right) at each internal level.
        let mut spine: Vec<(NodeId, NodeId)> = Vec::new();
        let mut cur_l = Some(left_id);
        let mut cur_r = Some(right_id);
        let mut leaf_pair = None;

        while let (Some(lid), Some(rid)) = (cur_l.take(), cur_r.take()) {
            let ln = self.load_node(lid).await?;
            if ln.is_leaf {
                leaf_pair = Some((lid, rid));
            } else {
                let rn = self.load_node(rid).await?;
                cur_l = ln.children.last().copied();
                cur_r = rn.children.first().copied();
                spine.push((lid, rid));
            }
        }

        let (ll, rl) = leaf_pair.ok_or_else(|| {
            StorageError::InvalidOperation(
                "Inner boundary descent found no leaves".into(),
            )
        })?;

        // Leaf level: left absorbs right.
        {
            let r = Arc::unwrap_or_clone(self.load_node(rl).await?);
            self.nodes.modify(ll, |n| {
                n.keys.extend(r.keys);
                n.values.extend(r.values);
            })?;
        }
        self.nodes.remove(rl);
        self.allocator.deallocate(rl).await?;
        update_stats!(self, |stats| {
            stats.node_count = stats.node_count.saturating_sub(1);
        });

        // Split the merged leaf if it overflowed.
        let mut pending: Option<(Key, Arc<NodeData>, NodeId)> = {
            let n = self.load_node(ll).await?;
            if n.keys.len() > max {
                drop(n);
                Some(self.split_node(ll).await?)
            } else {
                None
            }
        };

        // Walk back up the spine, absorbing right into left at each
        // level.  Right's first child was merged at the level below,
        // so its first key becomes the separator between the two
        // children sets; the remaining keys and children are appended.
        while let Some((lid, rid)) = spine.pop() {
            let r = Arc::unwrap_or_clone(self.load_node(rid).await?);

            let absorb = if r.keys.is_empty() {
                None
            } else {
                let Node {
                    mut keys,
                    mut values,
                    mut children,
                    ..
                } = r;
                let sk = keys.remove(0);
                let sv = values.remove(0);
                if !children.is_empty() {
                    children.remove(0);
                }
                Some((sk, sv, keys, values, children))
            };

            let p = pending.take();
            self.nodes.modify(lid, |l| {
                if let Some((pk, pv, pc)) = p {
                    l.keys.push(pk);
                    l.values.push(pv);
                    l.children.push(pc);
                }
                if let Some((sk, sv, rk, rv, rc)) = absorb {
                    l.keys.push(sk);
                    l.values.push(sv);
                    l.keys.extend(rk);
                    l.values.extend(rv);
                    l.children.extend(rc);
                }
            })?;

            self.nodes.remove(rid);
            self.allocator.deallocate(rid).await?;
            update_stats!(self, |stats| {
                stats.node_count = stats.node_count.saturating_sub(1);
            });

            let n = self.load_node(lid).await?;
            pending = if n.keys.len() > max {
                drop(n);
                Some(self.split_node(lid).await?)
            } else {
                None
            };
        }

        Ok(pending)
    }

    /// Repeatedly promotes single-child empty root until the root has keys
    /// or the tree is empty.
    ///
    /// Unlike `check_and_shrink_root` (which handles one level), this loops
    /// because `remove_prefix_range` can leave multiple levels of empty
    /// passthrough roots when a large prefix range spans the full width of
    /// several tree levels.
    async fn shrink_root_chain(&self, root: NodeId) -> Result<Option<NodeId>> {
        let mut flow = ControlFlow::Continue(root);

        while let ControlFlow::Continue(cur) = flow {
            let node = self.load_node(cur).await?;
            flow = match (
                node.keys.is_empty(),
                node.is_leaf,
                node.children.first().copied(),
            ) {
                (true, true, _) => {
                    self.nodes.remove(cur);
                    self.allocator.deallocate(cur).await?;
                    update_stats!(self, |stats| {
                        stats.height = 0;
                        stats.node_count = stats.node_count.saturating_sub(1);
                    });
                    ControlFlow::Break(None)
                }
                (true, false, Some(child)) => {
                    self.nodes.remove(cur);
                    self.allocator.deallocate(cur).await?;
                    update_stats!(self, |stats| {
                        stats.height = stats.height.saturating_sub(1);
                        stats.node_count = stats.node_count.saturating_sub(1);
                    });
                    ControlFlow::Continue(child)
                }
                _ => ControlFlow::Break(Some(cur)),
            };
        }

        match flow {
            ControlFlow::Break(result) => Ok(result),
            ControlFlow::Continue(_) => unreachable!(),
        }
    }

    /// Internal DATA operation returning MUMPS `$DATA` status.
    ///
    /// Maps the `NodeData` to `DataStatus`:
    /// - `NoData` (0): Node doesn't exist or has neither value nor descendants
    /// - `HasValue` (1): Node has value but no descendants
    /// - `HasDescendants` (10): Node has descendants but no value
    /// - `Both` (11): Node has both value and descendants
    async fn data_internal(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<DataStatus> {
        self.get_internal(root, key).await.map(|opt| {
            opt.map_or(DataStatus::NoData, |nd| {
                match (nd.value.is_some(), nd.has_descendants) {
                    (false, false) => DataStatus::NoData,
                    (true, false) => DataStatus::HasValue,
                    (false, true) => DataStatus::HasDescendants,
                    (true, true) => DataStatus::Both,
                }
            })
        })
    }

    /// Internal QUERY operation returning the next full key in lexicographic order.
    ///
    /// # Algorithm
    ///
    /// 1. If `after` is `None`, return the leftmost (smallest) key
    /// 2. Otherwise, find the smallest key strictly greater than `after`
    async fn query_internal(
        &self,
        root: NodeId,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        match after {
            None => self.find_leftmost_key(root).await,
            Some(key) => self.find_successor_key(root, key).await,
        }
    }

    /// Internal ORDER operation returning the next subscript at a level.
    ///
    /// # Algorithm
    ///
    /// 1. Find candidate key starting from `prefix ++ [after]` (or `prefix` if `None`)
    /// 2. Extract subscript at `prefix.len()`
    /// 3. If subscript equals `after`, skip to next key (recursively)
    /// 4. Return first subscript that's actually greater than `after`
    async fn order_internal(
        &self,
        root: NodeId,
        prefix: &Key,
        after: Option<&Subscript>,
    ) -> Result<Option<Subscript>> {
        let candidate = match after {
            None => self.find_first_with_prefix(root, prefix).await?,
            Some(sub) => {
                let mut search_key = prefix.clone();
                search_key.push(sub.clone());
                self.find_successor_with_prefix(root, &search_key, prefix)
                    .await?
            }
        };

        match (candidate, after) {
            (None, _) => Ok(None),
            (Some(k), None) => Ok(k.get(prefix.len()).cloned()),
            (Some(k), Some(after_sub)) => {
                // Check if found subscript is actually > after
                match k.get(prefix.len()) {
                    None => Ok(None),
                    Some(sub) if sub > after_sub => Ok(Some(sub.clone())),
                    Some(_) => {
                        // Found key is a descendant of prefix++[after]; skip it
                        self.order_skip_same(root, prefix, &k, after_sub).await
                    }
                }
            }
        }
    }

    /// Skip past keys with same subscript at `prefix.len()` as `skip_sub`.
    async fn order_skip_same(
        &self,
        root: NodeId,
        prefix: &Key,
        after_key: &Key,
        skip_sub: &Subscript,
    ) -> Result<Option<Subscript>> {
        let mut cur = after_key.clone();
        let mut out = None;
        let mut again = Some(());

        while again.take().is_some() {
            match self.find_successor_with_prefix(root, &cur, prefix).await? {
                None => out = None,
                Some(k) => match k.get(prefix.len()) {
                    None => out = None,
                    Some(sub) if sub > skip_sub => out = Some(sub.clone()),
                    Some(_) => {
                        cur = k;
                        again = Some(());
                    }
                },
            }
        }

        Ok(out)
    }

    /// Internal operation returning next key and its data in a single traversal.
    async fn get_next_internal(
        &self,
        root: NodeId,
        after: Option<&Key>,
    ) -> Result<Option<(Key, Arc<NodeData>)>> {
        match after {
            None => self.find_leftmost_entry(root).await,
            Some(key) => self.find_successor_entry(root, key).await,
        }
    }

    /// Internal COLLECT operation returning a stream of key-value pairs.
    ///
    /// Creates a stream that iterates over tree entries in lexicographic order,
    /// filtering by predicate and transforming with extract function.
    ///
    /// # Stream Semantics
    ///
    /// - **Lazy**: Entries are fetched on-demand as the stream is consumed
    /// - **Memory-efficient**: Only one entry is held at a time
    /// - **Cancellable**: Dropping the stream stops iteration immediately
    fn collects_internal<'a, P, F, T>(
        &'a self,
        root: NodeId,
        start: Option<&'a Key>,
        pred: P,
        extract: F,
    ) -> impl Stream<Item = Result<T>> + Send + 'a
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync + 'a,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        // Wrap closures in Arc for shared ownership across async iterations
        let pred = Arc::new(pred);
        let extract = Arc::new(extract);

        // State: `Some(key)` = last key yielded, `None` = start from beginning
        // We use `Option<Option<Key>>` where outer `None` signals stream end
        let init_state: Option<Option<Key>> = Some(start.cloned());

        futures::stream::unfold(init_state, move |state| {
            let pred = Arc::clone(&pred);
            let extract = Arc::clone(&extract);

            async move {
                // `None` state means stream is exhausted
                let cursor = state?;
                self.collects_find_next(
                    root,
                    cursor,
                    pred.as_ref(),
                    extract.as_ref(),
                )
                .await
            }
        })
    }

    /// Recursive helper for `collects_internal` that finds the next matching entry.
    async fn collects_find_next<P, F, T>(
        &self,
        root: NodeId,
        cursor: Option<Key>,
        pred: &P,
        extract: &F,
    ) -> Option<(Result<T>, Option<Option<Key>>)>
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync,
        T: Send,
    {
        let mut cur = cursor;
        let mut out = None;
        let mut again = Some(());

        while again.take().is_some() {
            match self.get_next_internal(root, cur.as_ref()).await {
                Ok(None) => out = None,
                Err(e) => out = Some((Err(e), None)),
                Ok(Some((key, data))) => {
                    match pred(&key, &data)
                        .then(|| extract(&key, &data))
                        .flatten()
                    {
                        Some(val) => out = Some((Ok(val), Some(Some(key)))),
                        None => {
                            cur = Some(key);
                            again = Some(());
                        }
                    }
                }
            }
        }

        out
    }

    /// Internal implementation for prefix-based collection with early termination.
    ///
    /// This method:
    /// 1. First checks and yields the entry at the exact prefix key (if exists)
    /// 2. Then iterates entries after the prefix, yielding while keys match
    /// 3. Terminates as soon as a key doesn't start with the prefix
    fn collects_prefix_internal<'a, F, T>(
        &'a self,
        root: NodeId,
        prefix: &'a Key,
        extract: F,
    ) -> impl Stream<Item = Result<T>> + Send + 'a
    where
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        let extract = Arc::new(extract);

        // State machine:
        // - `Some(None)` = haven't checked the prefix key yet
        // - `Some(Some(key))` = last key yielded, check successor
        // - `None` = stream exhausted
        let init_state: Option<Option<Key>> = Some(None);

        futures::stream::unfold(init_state, move |state| {
            let extract = Arc::clone(&extract);

            async move {
                match state {
                    None => None, // Stream exhausted
                    Some(None) => {
                        // First iteration: check the exact prefix key
                        self.collects_prefix_first(
                            root,
                            prefix,
                            extract.as_ref(),
                        )
                        .await
                    }
                    Some(Some(cursor)) => {
                        // Subsequent iterations: find next entry after cursor
                        self.collects_prefix_next(
                            root,
                            prefix,
                            &cursor,
                            extract.as_ref(),
                        )
                        .await
                    }
                }
            }
        })
    }

    /// Helper for `collects_prefix_internal`: handle the first entry (exact prefix).
    async fn collects_prefix_first<F, T>(
        &self,
        root: NodeId,
        prefix: &Key,
        extract: &F,
    ) -> Option<(Result<T>, Option<Option<Key>>)>
    where
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync,
        T: Send,
    {
        // Check if exact prefix key exists
        match self.get_internal(root, prefix).await {
            Err(e) => Some((Err(e), None)), // Error, terminate
            Ok(None) => {
                // No entry at prefix, try to find first entry after prefix
                self.collects_prefix_next(root, prefix, prefix, extract)
                    .await
            }
            Ok(Some(data)) => {
                // Entry exists at prefix
                match extract(prefix, &data) {
                    Some(val) => {
                        // Yield and continue from prefix
                        Some((Ok(val), Some(Some(prefix.clone()))))
                    }
                    None => {
                        // Skip this entry, find next
                        self.collects_prefix_next(root, prefix, prefix, extract)
                            .await
                    }
                }
            }
        }
    }

    /// Helper for `collects_prefix_internal`: find next entry after cursor.
    async fn collects_prefix_next<F, T>(
        &self,
        root: NodeId,
        prefix: &Key,
        cursor: &Key,
        extract: &F,
    ) -> Option<(Result<T>, Option<Option<Key>>)>
    where
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync,
        T: Send,
    {
        let mut cur = cursor.clone();
        let mut out = None;
        let mut again = Some(());

        while again.take().is_some() {
            match self.get_next_internal(root, Some(&cur)).await {
                Err(e) => out = Some((Err(e), None)),
                Ok(None) => out = None,
                Ok(Some((key, data))) if key.starts_with(prefix) => {
                    match extract(&key, &data) {
                        Some(val) => out = Some((Ok(val), Some(Some(key)))),
                        None => {
                            cur = key;
                            again = Some(());
                        }
                    }
                }
                Ok(Some(_)) => out = None,
            }
        }

        out
    }

    /// Finds the leftmost (smallest) key in the subtree rooted at `node_id`.
    ///
    /// This traverses down the left spine of the tree to find the minimum key.
    async fn find_leftmost_key(&self, node_id: NodeId) -> Result<Option<Key>> {
        self.find_leftmost_entry(node_id)
            .await
            .map(|o| o.map(|(k, _)| k))
    }

    /// Finds the smallest key strictly greater than `target` in the subtree.
    ///
    /// This is the core of the `$QUERY` implementation. It navigates the B-tree
    /// to find the successor key, handling transitions between leaf nodes.
    async fn find_successor_key(
        &self,
        node_id: NodeId,
        target: &Key,
    ) -> Result<Option<Key>> {
        self.find_successor_entry(node_id, target)
            .await
            .map(|o| o.map(|(k, _)| k))
    }

    /// Finds the leftmost (smallest) entry in the subtree, returning key and data.
    ///
    /// Returns the `NodeData` too, avoiding a separate lookup when we need to
    /// check if the node has a value.
    async fn find_leftmost_entry(
        &self,
        node_id: NodeId,
    ) -> Result<Option<(Key, Arc<NodeData>)>> {
        let mut cur = Some(node_id);
        let mut out = None;

        while let Some(id) = cur.take() {
            let node = self.load_node(id).await?;

            match node.children.first() {
                Some(&c) if !node.is_leaf => cur = Some(c),
                _ => {
                    out = node
                        .keys
                        .first()
                        .cloned()
                        .zip(node.values.first().cloned());
                }
            }
        }

        Ok(out)
    }

    /// Finds the successor entry (key + data) strictly greater than `target`.
    ///
    /// Descends towards `target`, remembering the deepest candidate key in an
    /// ancestor to fall back on if the subtree has no successor.
    async fn find_successor_entry(
        &self,
        node_id: NodeId,
        target: &Key,
    ) -> Result<Option<(Key, Arc<NodeData>)>> {
        let mut cur = Some(node_id);
        let mut tgt = Some(target);
        let mut fb = None;
        let mut out = None;

        while let Some(id) = cur.take() {
            let node = self.load_node(id).await?;

            let at = |i: usize| {
                node.keys.get(i).cloned().zip(node.values.get(i).cloned())
            };

            match tgt {
                None => match node.children.first() {
                    Some(&c) if !node.is_leaf => cur = Some(c),
                    _ => out = at(0),
                },
                Some(t) => {
                    let res = node.keys.binary_search(t);

                    match (node.is_leaf, res) {
                        (true, Ok(p)) => out = at(p + 1),
                        (true, Err(p)) => out = at(p),
                        (false, _) => {
                            let p = match res {
                                Ok(p) => p + 1,
                                Err(p) => p,
                            };
                            tgt = tgt.filter(|_| res.is_err());
                            fb = at(p).or(fb.take());

                            match node.children.get(p) {
                                Some(&c) => cur = Some(c),
                                None => out = at(p),
                            }
                        }
                    }
                }
            }
        }

        Ok(out.or(fb))
    }

    /// Finds the first key > `after` that starts with `prefix` and is longer.
    ///
    /// Used by `$ORDER` to find the next subscript at a specific level.
    /// Returns `None` if no such key exists (either no successor, or successor
    /// doesn't share the prefix).
    ///
    /// Note: The key must be strictly longer than `prefix` (have at least one
    /// more subscript) since we're looking for descendants, not the prefix key itself.
    async fn find_successor_with_prefix(
        &self,
        root: NodeId,
        after: &Key,
        prefix: &Key,
    ) -> Result<Option<Key>> {
        self.find_successor_key(root, after).await.map(|opt| {
            opt.filter(|k| k.starts_with(prefix) && k.len() > prefix.len())
        })
    }

    /// Finds the first key that starts with `prefix` and is longer.
    ///
    /// For `$ORDER`, we need keys that are proper descendants of the prefix,
    /// not the prefix key itself. Returns `None` if no such key exists.
    async fn find_first_with_prefix(
        &self,
        root: NodeId,
        prefix: &Key,
    ) -> Result<Option<Key>> {
        // If prefix is empty, just return the leftmost key
        if prefix.is_empty() {
            self.find_leftmost_key(root).await
        } else {
            // Find the first key > prefix that starts with prefix
            // We use find_successor_key to skip past the prefix itself
            self.find_successor_key(root, prefix)
                .await
                .map(|opt| opt.filter(|k| k.starts_with(prefix)))
        }
    }

    /// Finds and returns a node by its ID from the in-memory cache.
    ///
    /// This is an internal helper method used by tree traversal operations.
    /// It looks up the node in the in-memory `HashMap` and clones it.
    ///
    /// For cache-aware loading that supports disk persistence,
    /// use `load_node()` instead.
    async fn find_node(&self, id: NodeId) -> Result<Arc<Node>> {
        self.load_node(id).await
    }

    /// Splits a full node into two nodes.
    ///
    /// This operation is used when a node reaches maximum capacity
    /// (`2*min_degree - 1` keys). The node is split at the median:
    /// - Left half: `keys[0..mid]` remain in the original node
    /// - Median key: returned to be promoted to parent
    /// - Right half: `keys[mid+1..]` moved to new node
    ///
    /// For internal nodes, children are also split appropriately:
    /// - Left node gets `children[0..=mid]`
    /// - Right node gets `children[mid+1..]`
    ///
    /// # Design Note: B-tree vs B+-tree Semantics
    ///
    /// This implementation follows **B-tree semantics**, not B+-tree semantics.
    /// The median key-value pair is promoted to the parent node, meaning data
    /// can exist in both internal and leaf nodes.
    ///
    /// This is an **intentional divergence** from the original MUMPS implementation,
    /// which uses B+-tree semantics (all data in leaves, internal nodes contain
    /// only keys for navigation). The B-tree approach simplifies implementation
    /// while maintaining the same asymptotic performance characteristics.
    ///
    /// # B-tree Split Example
    ///
    /// Before split (`min_degree=3`, node has 5 keys):
    /// ```text
    /// Node: [10, 20, 30, 40, 50]
    /// ```
    ///
    /// After split:
    /// ```text
    /// Left:  [10, 20]
    /// Median: 30 (to be promoted to parent)
    /// Right: [40, 50]
    /// ```
    ///
    /// Returns `(median_key, median_data, right_node_id)`. The original
    /// node is modified in place to contain only the left half.
    async fn split_node(
        &self,
        id: NodeId,
    ) -> Result<(Key, Arc<NodeData>, NodeId)> {
        // Find the node to split and take ownership
        let node = Arc::unwrap_or_clone(self.load_node(id).await?);

        // Calculate the median index
        let mid = node.keys.len() / 2;

        // Verify we have enough keys to split
        match node.keys.get(mid) {
            None => Err(StorageError::InvalidOperation(
                "Node has insufficient keys for splitting".to_string(),
            )),
            Some(_) => {
                // Destructure to take ownership of the node's components
                let Node {
                    mut keys,
                    mut children,
                    mut values,
                    is_leaf,
                } = node;

                // Allocate ID for the new right node
                let right_id = self.allocator.allocate().await?;

                // Split keys using drain (SmallVec equivalent of split_off)
                // keys = [0..mid, mid, mid+1..end]
                // After drain: keys = [0..mid, mid], right_keys = [mid+1..end]
                let right_keys: smallvec::SmallVec<_> =
                    keys.drain(mid + 1..).collect();
                // Pop the median from left side: keys = [0..mid]
                let med_key = keys.pop().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Failed to extract median key".to_string(),
                    )
                })?;

                // Split values the same way and extract median value
                let right_vals: smallvec::SmallVec<_> =
                    values.drain(mid + 1..).collect();
                let med_val = values.pop().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Failed to extract median value".to_string(),
                    )
                })?;

                // Split children for internal nodes
                // For n keys, there are n+1 children
                // Left node (mid keys) needs mid+1 children: [0..=mid]
                // Right node needs remaining children: [mid+1..end]
                let right_children: smallvec::SmallVec<_> = if is_leaf {
                    smallvec::SmallVec::new()
                } else {
                    children.drain(mid + 1..).collect()
                };

                // Create the right node
                let right_node = Node {
                    keys: right_keys,
                    children: right_children,
                    values: right_vals,
                    is_leaf,
                };

                // Create the left node (reusing the split vectors)
                let left_node = Node {
                    keys,
                    children,
                    values,
                    is_leaf,
                };

                // Write both nodes to storage
                self.save_node(id, left_node).await?;
                self.save_node(right_id, right_node).await?;

                // Update statistics
                update_stats!(self, |stats| {
                    stats.splits += 1;
                    stats.node_count += 1;
                });

                Ok((med_key, med_val, right_id))
            }
        }
    }

    /// Merges two underfull sibling nodes into one node.
    ///
    /// This operation is the inverse of `split_node()` and is used when nodes
    /// become underfull (fewer than `min_degree - 1` keys). The merge combines:
    /// - All keys from the left node
    /// - The separator key (and its value) from the parent
    /// - All keys from the right node
    ///
    /// After merging, the right node is deallocated and the left node contains
    /// all combined keys and values.
    ///
    /// # Design Note: B-tree vs B+-tree Semantics
    ///
    /// Following **B-tree semantics**, the separator value from the parent is
    /// included in the merge. This is necessary because internal nodes contain
    /// data in B-trees (unlike B+-trees where internal nodes only contain keys).
    ///
    /// The caller is responsible for:
    /// 1. Removing the separator key from the parent
    /// 2. Updating the parent's child pointer to reference only the merged node
    ///
    /// # B-tree Merge Example
    ///
    /// Before merge:
    /// ```text
    /// Parent: [..., 30, ...]
    ///             /  \
    /// Left:   [10, 20]
    /// Right:  [40, 50]
    /// ```
    ///
    /// After merge (left node):
    /// ```text
    /// Merged: [10, 20, 30, 40, 50]
    /// ```
    ///
    /// Merges into the left node; the right node is deallocated. The caller
    /// must remove the separator from the parent and update its child pointer.
    async fn merge_nodes(
        &self,
        left_id: NodeId,
        sep_key: Key,
        sep_val: Arc<NodeData>,
        right_id: NodeId,
    ) -> Result<()> {
        // Find both nodes
        let left_arc = self.load_node(left_id).await?;
        let right_arc = self.load_node(right_id).await?;

        // Verify they're compatible (both leaf or both internal)
        if left_arc.is_leaf != right_arc.is_leaf {
            Err(StorageError::InvalidOperation(
                "Cannot merge leaf and internal nodes".to_string(),
            ))
        } else {
            // Take ownership of components
            let left = Arc::unwrap_or_clone(left_arc);
            let right = Arc::unwrap_or_clone(right_arc);

            let Node {
                keys: mut left_keys,
                children: mut left_children,
                values: mut left_vals,
                is_leaf,
            } = left;

            let Node {
                keys: right_keys,
                children: right_children,
                values: right_vals,
                is_leaf: _,
            } = right;

            // Combine: left + separator + right
            left_keys.push(sep_key);
            left_keys.extend(right_keys);

            left_vals.push(Arc::clone(&sep_val));
            left_vals.extend(right_vals);

            // For internal nodes, merge children
            if !is_leaf {
                left_children.extend(right_children);
            }

            // Create merged node
            let merged = Node {
                keys: left_keys,
                children: left_children,
                values: left_vals,
                is_leaf,
            };

            // Write merged node
            self.save_node(left_id, merged).await?;

            // Remove right node from cache and deallocate
            self.nodes.remove(right_id);
            self.allocator.deallocate(right_id).await?;

            // Update statistics
            update_stats!(self, |stats| {
                stats.merges += 1;
                stats.node_count = stats.node_count.saturating_sub(1);
            });

            Ok(())
        }
    }

    /// Recursively search for a key starting from the given node.
    ///
    /// Returns `Arc<NodeData>` for cheap cloning during hierarchy navigation.
    ///
    /// # Algorithm
    ///
    /// Uses binary search to find the key position:
    /// - If exact match found (`Ok(pos)`): Return the value at that position
    /// - If not found (`Err(pos)`) and leaf node: Key doesn't exist, return `None`
    /// - If not found (`Err(pos)`) and internal node: Recurse to child at `pos`
    ///
    /// The `Err(pos)` from `binary_search` indicates where the key would be
    /// inserted, which corresponds to the correct child pointer to follow.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Internal use only
    /// let result = btree.search_from_node(root_id, &key).await?;
    /// ```
    async fn search_from_node(
        &self,
        node_id: NodeId,
        key: &Key,
    ) -> Result<Option<Arc<NodeData>>> {
        let mut cur = Some(node_id);
        let mut out = None;

        while let Some(id) = cur.take() {
            let node = self.load_node(id).await?;

            match node.keys.binary_search(key) {
                Ok(pos) => {
                    let value = node.values.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Value index {} out of bounds (len {})",
                            pos,
                            node.values.len()
                        ))
                    })?;
                    out = Some(Arc::clone(value));
                }
                Err(_) if node.is_leaf => {}
                Err(pos) => {
                    cur = Some(*node.children.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Child index {} out of bounds (len {})",
                            pos,
                            node.children.len()
                        ))
                    })?);
                }
            }
        }

        Ok(out)
    }

    /// Raw node insertion without hierarchy management.
    ///
    /// This method performs the actual B-tree insertion without calling
    /// `ensure_ancestors()`. It's used internally by both `set_internal()`
    /// (after ensuring ancestors) and by `ensure_ancestors()` itself.
    ///
    /// Returns the (possibly new) root `NodeId`. The root may change if the
    /// tree grows due to node splitting.
    ///
    /// # Behavior for Existing Keys - Idempotent Merge
    ///
    /// If the key already exists, this method MERGES the `NodeData`:
    /// - `has_descendants`: Performs OR operation (if either old or new is `true`, result is `true`)
    /// - `value`: Takes new value if provided, otherwise keeps old value
    async fn set_at_node(
        &self,
        root: NodeId,
        key: &Key,
        data: NodeData,
    ) -> Result<NodeId> {
        // Check if root is full and needs splitting
        let root_node = self.load_node(root).await?;
        let max_keys = 2 * self.min_degree - 1;

        let new_root = if root_node.keys.len() == max_keys {
            // Root is full, split it and create a new root
            let (med_key, med_val, right_id) = self.split_node(root).await?;

            // Create new root with the median
            let new_root_id = self.allocator.allocate().await?;
            let new_root_node = Node {
                keys: smallvec::smallvec![med_key],
                children: smallvec::smallvec![root, right_id],
                values: smallvec::smallvec![Arc::clone(&med_val)],
                is_leaf: false,
            };

            // Insert new root
            self.save_node(new_root_id, new_root_node).await?;

            // Update height
            update_stats!(self, |stats| {
                stats.height += 1;
                stats.node_count += 1;
            });

            new_root_id
        } else {
            root
        };

        // Insert into the non-full root using NodeData
        self.insert_non_full_with_data(new_root, key, data).await?;

        // Update key count statistics
        update_stats!(self, |stats| {
            stats.key_count += 1;
        });

        Ok(new_root)
    }

    /// Deletes a single key from the tree.
    ///
    /// Returns the (possibly new) root `NodeId`, or `None` if the tree became
    /// empty after deletion.
    ///
    /// This handles the B-tree deletion algorithm:
    /// 1. Find the key in the tree
    /// 2. If in a leaf, remove it directly
    /// 3. If in an internal node, replace with predecessor/successor
    /// 4. Rebalance if node becomes underfull
    async fn delete_key(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<Option<NodeId>> {
        self.delete_key_from_node(root, key, vec![]).await?;

        // Check if root is now empty and shrink tree if needed
        self.check_and_shrink_root(root).await
    }

    /// Recursively deletes a key from a subtree rooted at `node_id`.
    ///
    /// `ancestors` is the chain from root to parent: `[(grandparent, idx), (parent, idx)]`
    /// This allows us to propagate rebalancing upward through the tree.
    async fn delete_key_from_node(
        &self,
        node_id: NodeId,
        key: &Key,
        ancestors: Vec<(NodeId, usize)>,
    ) -> Result<()> {
        let mut cur = Some(node_id);
        let mut k = key.clone();
        let mut anc = ancestors;

        while let Some(id) = cur.take() {
            let node = self.load_node(id).await?;

            match node.keys.binary_search(&k) {
                Ok(pos) if node.is_leaf => {
                    self.nodes.modify(id, |n| {
                        n.keys.remove(pos);
                        n.values.remove(pos);
                    })?;
                    update_stats!(self, |stats| {
                        stats.key_count = stats.key_count.saturating_sub(1);
                    });
                    self.rebalance_with_ancestors(id, mem::take(&mut anc))
                        .await?;
                }
                Ok(pos) => {
                    let left_child =
                        *node.children.get(pos).ok_or_else(|| {
                            StorageError::InvalidOperation(format!(
                                "Child index {} out of bounds",
                                pos
                            ))
                        })?;
                    let (pred_key, pred_val) =
                        self.find_predecessor(left_child).await?;

                    self.nodes.try_modify(id, |n| {
                        *n.keys.get_mut(pos).ok_or_else(|| {
                            StorageError::InvalidOperation(format!(
                                "Key index {} out of bounds",
                                pos
                            ))
                        })? = pred_key.clone();
                        *n.values.get_mut(pos).ok_or_else(|| {
                            StorageError::InvalidOperation(format!(
                                "Value index {} out of bounds",
                                pos
                            ))
                        })? = pred_val;
                        Ok(())
                    })?;

                    anc.push((id, pos));
                    k = pred_key;
                    cur = Some(left_child);
                }
                Err(_) if node.is_leaf => {}
                Err(pos) => {
                    let child_id =
                        node.children.get(pos).copied().ok_or_else(|| {
                            StorageError::InvalidOperation(format!(
                                "Child index {} out of bounds",
                                pos
                            ))
                        })?;
                    drop(node);
                    anc.push((id, pos));
                    cur = Some(child_id);
                }
            }
        }

        Ok(())
    }

    /// Finds the predecessor (rightmost key in subtree).
    async fn find_predecessor(
        &self,
        node_id: NodeId,
    ) -> Result<(Key, Arc<NodeData>)> {
        let mut cur = Some(node_id);
        let mut out = None;

        while let Some(id) = cur.take() {
            let node = self.load_node(id).await?;
            if node.is_leaf {
                let i = node.keys.len().checked_sub(1).ok_or_else(|| {
                    StorageError::InvalidOperation("Empty node".to_string())
                })?;
                let k = node.keys.get(i).ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Key index out of bounds".to_string(),
                    )
                })?;
                let v = node.values.get(i).ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Value index out of bounds".to_string(),
                    )
                })?;
                out = Some((k.clone(), Arc::clone(v)));
            } else {
                cur = Some(*node.children.last().ok_or_else(|| {
                    StorageError::InvalidOperation("No children".to_string())
                })?);
            }
        }

        out.ok_or_else(|| {
            StorageError::InvalidOperation("Empty node".to_string())
        })
    }

    /// Rebalances a node if underfull, propagating fixes up the ancestor chain.
    async fn rebalance_with_ancestors(
        &self,
        node_id: NodeId,
        mut ancestors: Vec<(NodeId, usize)>,
    ) -> Result<()> {
        let min_keys = self.min_degree - 1;
        let mut cur = Some(node_id);

        while let Some(id) = cur.take() {
            let under = self.load_node(id).await?.keys.len() < min_keys;

            cur = match ancestors.pop() {
                Some((parent_id, child_idx)) if under => self
                    .fix_underfull_node(parent_id, child_idx)
                    .await?
                    .then_some(parent_id),
                _ => None,
            };
        }

        Ok(())
    }

    /// Fixes an underfull node by borrowing from sibling or merging.
    ///
    /// Returns `true` if a merge was performed (parent lost a key).
    async fn fix_underfull_node(
        &self,
        parent_id: NodeId,
        child_idx: usize,
    ) -> Result<bool> {
        let parent = self.load_node(parent_id).await?;
        let min_keys = self.min_degree - 1;
        let err = |i| {
            StorageError::InvalidOperation(format!("Index {} out of bounds", i))
        };

        // Check if we can borrow from left sibling
        let borrow_left = if child_idx > 0 {
            let left_id = *parent
                .children
                .get(child_idx - 1)
                .ok_or_else(|| err(child_idx - 1))?;
            let left = self.load_node(left_id).await?;
            left.keys.len() > min_keys
        } else {
            false
        };

        // Check if we can borrow from right sibling
        let borrow_right = if !borrow_left
            && child_idx < parent.children.len().saturating_sub(1)
        {
            let right_id = *parent
                .children
                .get(child_idx + 1)
                .ok_or_else(|| err(child_idx + 1))?;
            let right = self.load_node(right_id).await?;
            right.keys.len() > min_keys
        } else {
            false
        };

        if borrow_left {
            // Borrow from left sibling
            let left_id = *parent
                .children
                .get(child_idx - 1)
                .ok_or_else(|| err(child_idx - 1))?;
            drop(parent);
            self.borrow_from_sibling(
                parent_id,
                child_idx,
                left_id,
                BorrowDir::Left,
            )
            .await?;
            Ok(false) // No merge, parent unchanged
        } else if borrow_right {
            // Borrow from right sibling
            let right_id = *parent
                .children
                .get(child_idx + 1)
                .ok_or_else(|| err(child_idx + 1))?;
            drop(parent);
            self.borrow_from_sibling(
                parent_id,
                child_idx,
                right_id,
                BorrowDir::Right,
            )
            .await?;
            Ok(false) // No merge, parent unchanged
        } else {
            // Must merge - prefer left sibling
            let (left_id, right_id, sep_idx) = if child_idx > 0 {
                (
                    *parent
                        .children
                        .get(child_idx - 1)
                        .ok_or_else(|| err(child_idx - 1))?,
                    *parent
                        .children
                        .get(child_idx)
                        .ok_or_else(|| err(child_idx))?,
                    child_idx - 1,
                )
            } else {
                (
                    *parent
                        .children
                        .get(child_idx)
                        .ok_or_else(|| err(child_idx))?,
                    *parent
                        .children
                        .get(child_idx + 1)
                        .ok_or_else(|| err(child_idx + 1))?,
                    child_idx,
                )
            };
            let sep_key = parent
                .keys
                .get(sep_idx)
                .ok_or_else(|| err(sep_idx))?
                .clone();
            let sep_val = Arc::clone(
                parent.values.get(sep_idx).ok_or_else(|| err(sep_idx))?,
            );
            drop(parent);

            self.merge_nodes(left_id, sep_key, sep_val, right_id)
                .await?;
            self.remove_separator_from_parent(parent_id, sep_idx)
                .await?;

            Ok(true) // Merged, parent lost a key
        }
    }

    /// Borrows a key from a sibling.
    async fn borrow_from_sibling(
        &self,
        parent_id: NodeId,
        child_idx: usize,
        sib_id: NodeId,
        dir: BorrowDir,
    ) -> Result<()> {
        let err = |i| {
            StorageError::InvalidOperation(format!("Index {} out of bounds", i))
        };

        // Load parent to get child_id and separator
        let parent = self.load_node(parent_id).await?;
        let child_id = *parent
            .children
            .get(child_idx)
            .ok_or_else(|| err(child_idx))?;
        let sep_idx = match dir {
            BorrowDir::Left => child_idx - 1,
            BorrowDir::Right => child_idx,
        };
        let sep_key = parent
            .keys
            .get(sep_idx)
            .ok_or_else(|| err(sep_idx))?
            .clone();
        let sep_val =
            Arc::clone(parent.values.get(sep_idx).ok_or_else(|| err(sep_idx))?);

        // Extract from sibling (pop from end if left, drain first if right)
        let (new_sep_key, new_sep_val, borrowed_child) =
            self.nodes.try_modify(sib_id, |sib| {
                let empty =
                    || StorageError::InvalidOperation("Empty sibling".into());
                match dir {
                    BorrowDir::Left => Ok((
                        sib.keys.pop().ok_or_else(empty)?,
                        sib.values.pop().ok_or_else(empty)?,
                        if sib.is_leaf {
                            None
                        } else {
                            sib.children.pop()
                        },
                    )),
                    BorrowDir::Right => Ok((
                        sib.keys.drain(..1).next().ok_or_else(empty)?,
                        sib.values.drain(..1).next().ok_or_else(empty)?,
                        if sib.is_leaf {
                            None
                        } else {
                            sib.children.drain(..1).next()
                        },
                    )),
                }
            })?;

        // Insert separator into child (front if left, end if right)
        self.nodes.modify(child_id, |child| match dir {
            BorrowDir::Left => {
                child.keys.insert(0, sep_key);
                child.values.insert(0, sep_val);
                borrowed_child
                    .into_iter()
                    .for_each(|c| child.children.insert(0, c));
            }
            BorrowDir::Right => {
                child.keys.push(sep_key);
                child.values.push(sep_val);
                borrowed_child
                    .into_iter()
                    .for_each(|c| child.children.push(c));
            }
        })?;

        // Update separator in parent
        self.nodes.try_modify(parent_id, |p| {
            *p.keys.get_mut(sep_idx).ok_or_else(|| err(sep_idx))? = new_sep_key;
            *p.values.get_mut(sep_idx).ok_or_else(|| err(sep_idx))? =
                new_sep_val;
            Ok(())
        })
    }

    /// Removes the separator key and child pointer from parent after merge.
    async fn remove_separator_from_parent(
        &self,
        parent_id: NodeId,
        sep_idx: usize,
    ) -> Result<()> {
        self.nodes.modify(parent_id, |p| {
            p.keys.remove(sep_idx);
            p.values.remove(sep_idx);
            p.children.remove(sep_idx + 1);
        })?;

        // Update key count
        update_stats!(self, |stats| {
            stats.key_count = stats.key_count.saturating_sub(1);
        });

        // If parent is root and now empty, it was handled by shrink_root_if_needed
        // Otherwise, we might need to recursively fix parent
        // For now, we'll rely on the caller to check
        Ok(())
    }

    /// Checks if root is empty after deletion and shrinks tree if needed.
    ///
    /// Returns the (possibly new) root `NodeId`, or `None` if the tree became
    /// empty (root was an empty leaf).
    async fn check_and_shrink_root(
        &self,
        root: NodeId,
    ) -> Result<Option<NodeId>> {
        let root_node = self.load_node(root).await?;

        // If root has no keys but has one child, promote that child
        match (
            root_node.keys.is_empty(),
            root_node.is_leaf,
            root_node.children.first().copied(),
        ) {
            (true, false, Some(new_root)) => {
                // Promote the only child to be the new root
                // Deallocate old root
                self.nodes.remove(root);
                self.allocator.deallocate(root).await?;

                // Update height
                update_stats!(self, |stats| {
                    stats.height = stats.height.saturating_sub(1);
                    stats.node_count = stats.node_count.saturating_sub(1);
                });

                Ok(Some(new_root))
            }
            (true, true, _) => {
                // Root is empty leaf - tree is now empty
                // Deallocate the root node
                self.nodes.remove(root);
                self.allocator.deallocate(root).await?;

                // Update stats
                update_stats!(self, |stats| {
                    stats.height = 0;
                    stats.node_count = stats.node_count.saturating_sub(1);
                });

                Ok(None)
            }
            _ => Ok(Some(root)),
        }
    }

    /// Updates ancestor `has_descendants` flags after a KILL operation.
    ///
    /// For each ancestor of the killed key, checks if it still has any descendants.
    /// If not, sets `has_descendants` to `false`. If the ancestor has no value and
    /// no descendants, it is removed entirely.
    ///
    /// Returns the (possibly new) root `NodeId`, or `None` if the tree became empty.
    async fn update_ancestors_after_kill(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<Option<NodeId>> {
        // Process ancestors from deepest to shallowest
        let mut ancestors = key.ancestors();
        ancestors.reverse();

        // Use fold to track root changes through ancestor processing
        futures::stream::iter(ancestors.into_iter().map(Ok::<_, StorageError>))
            .try_fold(Some(root), |cur_root, anc_key| async move {
                match cur_root {
                    None => Ok(None),
                    Some(r) => {
                        match self.get_internal(r, &anc_key).await? {
                            None => Ok(Some(r)), // Ancestor doesn't exist
                            Some(data) => {
                                // Check if ancestor still has any descendants
                                let has_desc = self
                                    .has_any_descendants(r, &anc_key)
                                    .await?;

                                match (has_desc, data.value.is_some()) {
                                    (true, _) => {
                                        // Still has descendants, ensure flag is set
                                        if data.has_descendants {
                                            Ok(Some(r))
                                        } else {
                                            self.update_descendants_flag(
                                                r, &anc_key, true,
                                            )
                                            .await?;
                                            Ok(Some(r))
                                        }
                                    }
                                    (false, true) => {
                                        // No descendants but has value - update flag
                                        self.update_descendants_flag(
                                            r, &anc_key, false,
                                        )
                                        .await?;
                                        Ok(Some(r))
                                    }
                                    (false, false) => {
                                        // No descendants and no value - remove the node
                                        self.delete_key(r, &anc_key).await
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .await
    }

    /// Checks if a key has any descendants in the tree.
    ///
    /// Returns `true` if there exists any key `K'` where `K'.starts_with(key)` and `K' != key`.
    async fn has_any_descendants(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<bool> {
        self.check_descendants_from_node(root, key).await
    }

    /// Recursively checks if any key with the given prefix exists (excluding exact match).
    async fn check_descendants_from_node(
        &self,
        node_id: NodeId,
        prefix: &Key,
    ) -> Result<bool> {
        let mut cur = vec![node_id];
        let mut found = false;

        while let Some(batch) =
            Some(mem::take(&mut cur)).filter(|b| !b.is_empty() && !found)
        {
            let loads: FuturesUnordered<_> =
                batch.iter().map(|&id| self.load_node(id)).collect();

            let nodes: Vec<_> = loads.try_collect().await?;

            found = nodes.iter().any(|node| {
                node.keys
                    .iter()
                    .skip(node.keys.binary_search(prefix).unwrap_or_else(|p| p))
                    .take_while(|k| k.starts_with(prefix))
                    .any(|k| k != prefix)
            });

            if !found {
                nodes.iter().for_each(|node| {
                    let start =
                        node.keys.binary_search(prefix).unwrap_or_else(|p| p);
                    let cnt = node
                        .keys
                        .iter()
                        .skip(start)
                        .take_while(|k| k.starts_with(prefix))
                        .count();

                    if !node.is_leaf {
                        cur.extend(
                            (start..start + cnt + 1)
                                .filter_map(|i| node.children.get(i).copied()),
                        );
                    }
                });
            }
        }

        Ok(found)
    }

    /// Inserts a key-value pair into a non-full node.
    ///
    /// Delegates to `insert_non_full_with_data()` with appropriate `NodeData`.
    async fn insert_non_full(
        &self,
        node_id: NodeId,
        key: &Key,
        value: rumps_types::Value,
    ) -> Result<()> {
        self.insert_non_full_with_data(
            node_id,
            key,
            NodeData::with_value(value),
        )
        .await
    }

    /// Inserts a key with `NodeData` into a non-full node, with merge semantics.
    ///
    /// This is similar to `insert_non_full()` but accepts `NodeData` directly
    /// and implements merge semantics for existing keys (required for idempotent
    /// ancestor creation).
    ///
    /// # Merge Behavior
    ///
    /// When the key already exists:
    /// - `has_descendants`: OR operation (`old || new`)
    /// - `value`: Takes new value if `Some`, otherwise keeps old value
    ///
    /// This ensures concurrent ancestor creation is safe and idempotent.
    async fn insert_non_full_with_data(
        &self,
        node_id: NodeId,
        key: &Key,
        data: NodeData,
    ) -> Result<()> {
        let max_keys = 2 * self.min_degree - 1;
        let mut cur = Some(node_id);
        let mut leaf = None;

        while let Some(id) = cur.take() {
            let node_arc = self.load_node(id).await?;

            let pos = node_arc
                .keys
                .binary_search(key)
                .unwrap_or_else(|insert_pos| insert_pos);

            if node_arc.is_leaf {
                leaf = Some((id, node_arc, pos));
            } else {
                let child_id =
                    *node_arc.children.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Child index {} out of bounds (len {})",
                            pos,
                            node_arc.children.len()
                        ))
                    })?;

                let child = self.load_node(child_id).await?;

                cur = Some(
                    if child.keys.len() == max_keys {
                        let (median_key, median_value, new_child_id) =
                            self.split_node(child_id).await?;

                        let mut updated_node = Arc::unwrap_or_clone(node_arc);
                        updated_node.keys.insert(pos, median_key.clone());
                        updated_node
                            .values
                            .insert(pos, Arc::clone(&median_value));
                        updated_node.children.insert(pos + 1, new_child_id);

                        self.save_node(id, updated_node).await?;

                        if *key > median_key {
                            new_child_id
                        } else {
                            child_id
                        }
                    } else {
                        drop(child);
                        drop(node_arc);
                        child_id
                    },
                );
            }
        }

        let (id, node_arc, pos) = leaf.ok_or_else(|| {
            StorageError::InvalidOperation("No leaf reached".to_string())
        })?;

        let mut updated_node = Arc::unwrap_or_clone(node_arc);

        match updated_node.keys.get(pos) {
            Some(existing_key) if existing_key == key => {
                let existing_data =
                    updated_node.values.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Value index {} out of bounds (len {})",
                            pos,
                            updated_node.values.len()
                        ))
                    })?;
                let merged_data = NodeData::new(
                    data.value.or_else(|| existing_data.value.clone()),
                    existing_data.has_descendants || data.has_descendants,
                );
                let values_len = updated_node.values.len();
                *updated_node.values.get_mut(pos).ok_or_else(|| {
                    StorageError::InvalidOperation(format!(
                        "Value index {} out of bounds for mutation (len {})",
                        pos, values_len
                    ))
                })? = Arc::new(merged_data);
            }
            _ => {
                updated_node.keys.insert(pos, key.clone());
                updated_node.values.insert(pos, Arc::new(data));
            }
        }

        self.save_node(id, updated_node).await
    }

    /// Updates the `has_descendants` flag for an existing key. Fails if the
    /// key doesn't exist.
    async fn update_descendants_flag(
        &self,
        root: NodeId,
        key: &Key,
        flag: bool,
    ) -> Result<()> {
        // Find the node containing this key and update directly.
        // We can't use `set_at_node()` because its OR merge semantics prevent
        // setting `has_descendants` to `false`.
        self.update_flag_in_node(root, key, flag).await
    }

    /// Recursively finds and updates the `has_descendants` flag for a key.
    async fn update_flag_in_node(
        &self,
        node_id: NodeId,
        key: &Key,
        flag: bool,
    ) -> Result<()> {
        let mut cur = Some(node_id);

        while let Some(id) = cur.take() {
            let node = self.load_node(id).await?;
            let pos = node.keys.binary_search(key).unwrap_or_else(|p| p);

            match node.keys.get(pos) {
                Some(k) if k == key => self.nodes.try_modify(id, |n| {
                    let old = n.values.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Value index {} out of bounds",
                            pos
                        ))
                    })?;
                    let updated = NodeData::new(old.value.clone(), flag);
                    *n.values.get_mut(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Value index {} out of bounds for mutation",
                            pos
                        ))
                    })? = Arc::new(updated);
                    Ok(())
                })?,
                _ if node.is_leaf => Err(StorageError::InvalidOperation(
                    format!("Key {:?} not found for flag update", key),
                ))?,
                _ => {
                    cur = Some(*node.children.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Child index {} out of bounds",
                            pos
                        ))
                    })?);
                }
            }
        }

        Ok(())
    }

    /// Ensures all ancestor keys exist with `has_descendants = true`.
    ///
    /// This method is called before inserting a new key to maintain the
    /// hierarchical structure. For each ancestor that doesn't exist, it
    /// creates an intermediate node (no value, only descendants).
    ///
    /// Returns the (possibly new) root `NodeId`. The root may change if
    /// ancestors are created and cause tree growth.
    ///
    /// # Thread Safety
    ///
    /// This method is safe for concurrent execution. If multiple operations
    /// try to create the same ancestor, `set_at_node()` will merge the
    /// `NodeData` using OR semantics on `has_descendants`, making the operation
    /// idempotent.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Before inserting Key([1, 2, 3])
    /// let root = ensure_ancestors(root, &key![1, 2, 3]).await?;
    /// // Creates: Key([1]) and Key([1, 2]) with has_descendants=true
    /// ```
    async fn ensure_ancestors(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<NodeId> {
        let ancestors = key.ancestors();

        // Process each ancestor from root to leaf sequentially
        // Use fold to track root changes through ancestor creation
        futures::stream::iter(
            ancestors
                .into_iter()
                .map(Ok::<_, crate::error::StorageError>),
        )
        .try_fold(root, |cur_root, ancestor_key| async move {
            match self.get_internal(cur_root, &ancestor_key).await? {
                Some(node_data) => {
                    // Ancestor exists - update has_descendants if needed
                    if !node_data.has_descendants {
                        self.update_descendants_flag(
                            cur_root,
                            &ancestor_key,
                            true,
                        )
                        .await?;
                    }
                    Ok(cur_root)
                }
                None => {
                    // Ancestor doesn't exist - create intermediate node
                    // Use set_at_node to avoid recursive ensure_ancestors call
                    self.set_at_node(
                        cur_root,
                        &ancestor_key,
                        NodeData::with_descendants(),
                    )
                    .await
                }
            }
        })
        .await
    }
}
