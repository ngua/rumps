// TODO: Remove this once Phase 4-5 are implemented and all methods are actually used
#![allow(dead_code)]
#![allow(clippy::only_used_in_recursion)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::{Stream, TryStreamExt};
use rumps_types::{DataStatus, Key};
use tokio::sync::RwLock;

use crate::engine;
use crate::error::{Result, StorageError};
use crate::node::{Node, NodeData, NodeId};

#[cfg(test)]
mod tests;

/// Statistics tracking for B-tree operations.
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

/// Trait for node ID allocation strategies.
#[async_trait]
pub(crate) trait NodeAllocator: Send + Sync {
    /// Allocate a new node ID
    async fn allocate(&self) -> Result<NodeId>;

    /// Deallocate a node ID for reuse
    async fn deallocate(&self, id: NodeId) -> Result<()>;

    /// Get the next available ID without allocating
    async fn peek_next(&self) -> NodeId;
}

/// Simple incrementing allocator for in-memory use.
pub(crate) struct IncrementingAllocator {
    next_id: RwLock<u64>,
}

impl IncrementingAllocator {
    pub(crate) fn new() -> Self {
        Self {
            next_id: RwLock::new(0),
        }
    }
}

#[async_trait]
impl NodeAllocator for IncrementingAllocator {
    async fn allocate(&self) -> Result<NodeId> {
        let mut next = self.next_id.write().await;
        let id = NodeId::from(*next);
        *next += 1;
        Ok(id)
    }

    async fn deallocate(&self, _id: NodeId) -> Result<()> {
        // No-op for simple allocator
        Ok(())
    }

    async fn peek_next(&self) -> NodeId {
        NodeId::from(*self.next_id.read().await)
    }
}

/// Disk-backed allocator that delegates to storage engine.
///
/// Used when the B-tree is backed by persistent storage. Delegates all
/// allocation/deallocation to the underlying `AsyncStorageEngine`, which
/// manages page allocation via a free list or bitmap.
pub(crate) struct DiskNodeAllocator {
    storage: Arc<dyn engine::AsyncStorageEngine>,
}

impl DiskNodeAllocator {
    /// Creates a new disk allocator backed by the given storage engine.
    pub(crate) fn new(storage: Arc<dyn engine::AsyncStorageEngine>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl NodeAllocator for DiskNodeAllocator {
    async fn allocate(&self) -> Result<NodeId> {
        self.storage.allocate().await
    }

    async fn deallocate(&self, id: NodeId) -> Result<()> {
        self.storage.deallocate(id).await
    }

    async fn peek_next(&self) -> NodeId {
        // For disk-backed storage, peek_next isn't meaningful since
        // pages can be allocated non-sequentially from a free list.
        // Return a sentinel value.
        NodeId::from(0)
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
/// ## `RwLock<HashMap>` for nodes
///
/// Nodes are stored in an async-aware `RwLock<HashMap>` because:
/// - `NodeId`s are arbitrary internal references (like page IDs)
/// - Logical ordering is maintained by the tree structure, not `NodeId` values
/// - `RwLock` enables concurrent reads with exclusive writes
/// - Async from day one prevents breaking API changes when adding disk I/O
///
/// In Phase 2-3, this is pure in-memory storage. In Phase 4, it becomes
/// a page cache with lazy loading from disk.
///
/// # Thread Safety
///
/// The `BTree` is designed to be shared across threads using `Arc<BTree>`.
/// All operations use interior mutability via `RwLock`, allowing multiple
/// concurrent readers with exclusive writers.
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
    /// Async-aware node storage pool.
    ///
    /// In Phase 2-3, this holds all nodes in memory. In Phase 4, it becomes
    /// a page cache with LRU eviction, where nodes are lazy-loaded from disk.
    ///
    /// Uses `tokio::sync::RwLock` for async-compatible concurrent access:
    /// - Multiple readers can access simultaneously
    /// - Writers get exclusive access
    /// - Works seamlessly with `async`/`await`
    ///
    /// `NodeId`s have no semantic ordering—they're internal references.
    /// The tree's logical ordering is maintained by parent-child links
    /// and sorted keys within each node.
    nodes: RwLock<HashMap<NodeId, Node>>,

    /// Optional storage engine for persistence.
    ///
    /// When `None`, the B-tree operates entirely in memory.
    /// When `Some`, nodes are persisted to disk and the `nodes` map
    /// becomes a page cache with lazy loading.
    storage: Option<Arc<dyn engine::AsyncStorageEngine>>,

    /// Node ID allocator strategy.
    allocator: Arc<dyn NodeAllocator>,

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
    storage: Option<Arc<dyn engine::AsyncStorageEngine>>,
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
    pub(crate) fn storage(
        mut self,
        s: Arc<dyn engine::AsyncStorageEngine>,
    ) -> Self {
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

    /// Builds the `BTree` with the configured settings.
    ///
    /// # Errors
    ///
    /// Returns an error if `min_degree < 2`.
    pub(crate) fn build(self) -> Result<BTree> {
        let min_degree = self.min_degree.unwrap_or(3);

        if min_degree < 2 {
            Err(StorageError::InvalidConfiguration(
                "min_degree must be >= 2".to_string(),
            ))
        } else {
            let allocator: Arc<dyn NodeAllocator> = match &self.storage {
                Some(storage) => {
                    Arc::new(DiskNodeAllocator::new(Arc::clone(storage)))
                }
                None => Arc::new(IncrementingAllocator::new()),
            };

            Ok(BTree {
                nodes: RwLock::new(HashMap::new()),
                storage: self.storage,
                allocator,
                min_degree,
                max_memory_bytes: self.max_memory_bytes,
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
        // NOTE: BTree doesn't implement transaction logic. Phase 5 snapshot isolation
        // happens at the Transaction layer (write buffering + read-from-buffer-first).
        // Context is accepted for metadata only (stores txn_id for future MVCC).
        //
        // Future MVCC: Will store version chains (multiple versions per key) and use
        // ctx.start_timestamp to determine which version to overwrite/update.
        self.set_internal(root, key, NodeData::with_value(value))
            .await
    }

    /// Gets a value from the tree rooted at `root`.
    ///
    /// Always reads committed state. Optional transaction context is for metadata
    /// only (Phase 5 snapshot isolation happens at Transaction layer).
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
        // NOTE: BTree always reads committed state. Phase 5 snapshot isolation happens
        // at Transaction layer (Transaction.get() checks write buffer first, then calls
        // this).
        //
        // Future MVCC: Will store version chains and use ctx.start_timestamp to select
        // the most recent version visible to the transaction (filter out versions created
        // after start_timestamp or by uncommitted transactions).
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
    /// only (Phase 5 write buffering happens at Transaction layer).
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
        // NOTE: BTree modifies committed state directly. Phase 5 buffering happens at
        // Transaction layer (Transaction.kill() buffers, commit applies via db.kill()).
        // Context accepted for metadata only (stores txn_id for future MVCC).
        //
        // Future MVCC: Will mark versions as deleted (tombstones) with ctx.txn_id and
        // timestamp rather than physically removing them immediately. Vacuum process will
        // clean up versions no longer visible to any active transaction.
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
        // NOTE: Phase 5 snapshot isolation at Transaction layer (checks write buffer).
        // Future MVCC: Will use ctx.start_timestamp to select visible version.
        self.data_internal(root, key).await
    }

    /// Returns the next key in lexicographic order (MUMPS `$ORDER`).
    ///
    /// Always reads committed state. Transaction context for metadata only.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let next_key = btree.order_at(root, Some(&key), None).await?;
    /// ```
    pub(crate) async fn order_at(
        &self,
        root: NodeId,
        after: Option<&Key>,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<Option<Key>> {
        // NOTE: Phase 5 snapshot isolation at Transaction layer (merges write buffer
        // with committed state). Future MVCC: Will filter visible versions by timestamp.
        self.order_internal(root, after).await
    }

    /// Creates a stream of key-value pairs from the tree (RUMPS `$COLLECT`).
    ///
    /// This is a RUMPS extension (not in traditional MUMPS) that provides
    /// stream-based iteration over tree entries. The stream yields entries
    /// that match the predicate, transformed by the extract function.
    ///
    /// Always reads committed state. Transaction context for metadata only.
    ///
    /// # Type Parameters
    ///
    /// * `P` - Predicate function: `(&Key, &NodeData) -> bool`
    ///   - Returns `true` to include the entry in the stream
    ///   - Returns `false` to skip the entry (iteration continues)
    /// * `F` - Extract function: `(&Key, &NodeData) -> Option<T>`
    ///   - Transforms matching entries into output type `T`
    ///   - Returns `None` to skip (entry matched predicate but shouldn't be yielded)
    ///   - Clone `NodeData` or its fields if ownership is needed
    /// * `T` - Output type yielded by the stream
    ///
    /// # Arguments
    ///
    /// * `root` - Root `NodeId` of the tree
    /// * `start` - Optional starting key (`None` starts from beginning)
    /// * `pred` - Function that determines whether to include entries
    /// * `extract` - Function that transforms entries into output type
    /// * `ctx` - Optional transaction context for snapshot isolation
    ///
    /// # Returns
    ///
    /// A `Stream` that yields `Result<T>` for each matching entry.
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
        // Phase 5.4 will add transaction snapshot isolation here
        self.collects_internal(root, start, pred, extract)
    }

    /// Collects all matching entries into a `Vec`.
    ///
    /// Convenience wrapper around `collects_at` that consumes the entire stream.
    /// Use `collects_at` directly for large datasets to avoid loading everything
    /// into memory.
    ///
    /// # Arguments
    ///
    /// * `root` - Root `NodeId` of the tree
    /// * `start` - Optional key to start iteration after (exclusive)
    /// * `pred` - Predicate returning `true` to include entry, `false` to skip
    /// * `extract` - Extractor returning `Some(T)` to yield, `None` to skip
    /// * `ctx` - Optional transaction context (currently unused)
    ///
    /// # Returns
    ///
    /// A `Vec<T>` containing all extracted values from matching entries.
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
    ///
    /// # Arguments
    ///
    /// * `root` - Root `NodeId` of the tree
    /// * `prefix` - The key prefix to match
    /// * `extract` - Extractor returning `Some(T)` to yield, `None` to skip
    ///
    /// # Returns
    ///
    /// A `Vec<T>` containing all extracted values from entries whose keys
    /// start with `prefix`.
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
    ///
    /// # Type Parameters
    ///
    /// * `F` - Extract function: `(&Key, &NodeData) -> Option<T>`
    /// * `T` - Output type yielded by the stream
    ///
    /// # Arguments
    ///
    /// * `root` - Root `NodeId` of the tree
    /// * `prefix` - The key prefix to match
    /// * `extract` - Transforms matching entries into output type `T`
    ///
    /// # Returns
    ///
    /// A `Stream` that yields `Result<T>` for each entry whose key starts
    /// with `prefix`.
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
    /// storage (if available) and adds to cache.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NodeNotFound` if the node doesn't exist in
    /// cache or storage.
    async fn load_node(&self, id: NodeId) -> Result<Node> {
        // Check cache first
        let cached = {
            let nodes = self.nodes.read().await;
            nodes.get(&id).cloned()
        };

        match cached {
            Some(node) => Ok(node),
            None => {
                // Load from disk if storage is configured
                let storage = self
                    .storage
                    .as_ref()
                    .ok_or(StorageError::NodeNotFound(*id))?;

                let node = storage.read(id).await?;

                // Add to cache
                {
                    let mut nodes = self.nodes.write().await;
                    nodes.insert(id, node.clone());
                    // TODO Phase 4.7: Implement LRU eviction if cache is full
                }

                Ok(node)
            }
        }
    }

    /// Saves a node to the in-memory cache and marks it dirty.
    ///
    /// **BTree is purely in-memory** - it does NOT write to disk directly.
    /// Disk writes happen during checkpoint/flush at the Database layer.
    ///
    /// For disk-backed trees, the page is marked dirty in the storage cache.
    /// For in-memory trees, just updates the node map.
    async fn save_node(&self, id: NodeId, node: Node) -> Result<()> {
        // Update in-memory cache
        {
            let mut nodes = self.nodes.write().await;
            nodes.insert(id, node.clone());
        }

        // Mark dirty in storage cache (if configured)
        if let Some(storage) = self.storage.as_ref() {
            storage.mark_dirty(id, &node).await?;
        }

        Ok(())
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

        {
            let mut stats = self.stats.write().await;
            stats.node_count += 1;
            stats.height = 1;
        }

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

    /// Recursively deletes a subtree rooted at `node_id`.
    fn delete_subtree<'a>(
        &'a self,
        node_id: NodeId,
    ) -> BoxFuture<'a, Result<usize>> {
        Box::pin(async move {
            // Try to load node (might be only on disk, not in cache)
            let node_result = self.load_node(node_id).await;

            // Handle node not found gracefully
            let node = match node_result {
                Err(StorageError::NodeNotFound(_)) => {
                    // Node doesn't exist - this is ok for delete
                    None
                }
                Err(e) => {
                    // Other errors should propagate
                    Some(Err(e))
                }
                Ok(n) => Some(Ok(n)),
            };

            match node {
                None => Ok(0),
                Some(Err(e)) => Err(e),
                Some(Ok(node)) => {
                    // Remove from cache
                    {
                        let mut nodes = self.nodes.write().await;
                        nodes.remove(&node_id);
                    }

                    // Deallocate from storage
                    self.allocator.deallocate(node_id).await?;

                    // If internal node, recursively delete children
                    let child_counts: usize = if node.is_leaf {
                        0
                    } else {
                        let counts: Vec<usize> = futures::future::try_join_all(
                            node.children
                                .iter()
                                .map(|&c| self.delete_subtree(c)),
                        )
                        .await?;
                        counts.into_iter().sum()
                    };

                    // Update stats
                    {
                        let mut stats = self.stats.write().await;
                        stats.node_count = stats.node_count.saturating_sub(1);
                        stats.key_count =
                            stats.key_count.saturating_sub(node.keys.len());
                    }

                    Ok(1 + child_counts)
                }
            }
        })
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
    /// assert_eq!(btree.node_count().await, 0);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) async fn node_count(&self) -> usize {
        self.nodes.read().await.len()
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
        self.stats.read().await.clone()
    }

    /// Checks if the current memory usage is within limits.
    async fn check_memory_limit(&self) -> Result<()> {
        match self.max_memory_bytes {
            Some(limit) => {
                let stats = self.stats.read().await;
                if stats.memory_bytes > limit {
                    Err(StorageError::MemoryLimitExceeded {
                        used: stats.memory_bytes,
                        limit,
                    })
                } else {
                    Ok(())
                }
            }
            None => Ok(()),
        }
    }
}

/// Direction for sibling borrowing during B-tree rebalancing.
/// (used in private `impl` methods below)
enum BorrowDir {
    Left,
    Right,
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
    /// This method implements MUMPS `KILL` semantics:
    /// 1. Deletes the specified key (if it exists)
    /// 2. Deletes all descendants (keys that start with the given key as prefix)
    /// 3. Updates ancestor `has_descendants` flags
    /// 4. Handles tree rebalancing (node merging when underfull)
    // Internal method for tests/benchmarks - not part of public API
    async fn kill_internal(
        &self,
        root: NodeId,
        key: &Key,
    ) -> Result<Option<NodeId>> {
        // Step 1: Collect all keys to delete (the key and its descendants)
        let mut keys_to_delete =
            self.collect_keys_with_prefix(root, key).await?;

        if keys_to_delete.is_empty() {
            Ok(Some(root))
        } else {
            // Step 2: Delete each key from the tree
            // We delete in reverse order (deepest first) to minimize rebalancing
            keys_to_delete
                .sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| b.cmp(a)));

            // Use fold to process deletions sequentially, tracking root changes
            let final_root = futures::stream::iter(
                keys_to_delete.iter().map(Ok::<_, StorageError>),
            )
            .try_fold(Some(root), |cur_root, k| async move {
                match cur_root {
                    None => Ok(None),
                    Some(r) => self.delete_key(r, k).await,
                }
            })
            .await?;

            // Step 3: Update ancestor has_descendants flags
            match final_root {
                None => Ok(None),
                Some(r) => self.update_ancestors_after_kill(r, key).await,
            }
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

    /// Internal ORDER operation returning the next key in lexicographic order.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(key))` - The next key in lexicographic order
    /// * `Ok(None)` - No more keys (end of iteration or empty tree)
    ///
    /// # Algorithm
    ///
    /// 1. If `after` is `None`, return the leftmost (smallest) key
    /// 2. Otherwise, find the smallest key strictly greater than `after`
    async fn order_internal(
        &self,
        root: NodeId,
        after: Option<&Key>,
    ) -> Result<Option<Key>> {
        match after {
            None => self.find_leftmost_key(root).await,
            Some(key) => self.find_successor_key(root, key).await,
        }
    }

    /// Internal operation returning next key and its data in one call.
    ///
    /// Combines `order_internal` and `get_internal` for efficiency in iteration.
    ///
    /// # Arguments
    ///
    /// * `root` - Root `NodeId` of the tree
    /// * `after` - The key to start after, or `None` to get the first entry
    ///
    /// # Returns
    ///
    /// * `Ok(Some((key, data)))` - Next entry found
    /// * `Ok(None)` - No more entries
    /// * `Err(...)` - Error during traversal
    async fn get_next_internal(
        &self,
        root: NodeId,
        after: Option<&Key>,
    ) -> Result<Option<(Key, Arc<NodeData>)>> {
        match self.order_internal(root, after).await? {
            None => Ok(None),
            Some(key) => self
                .get_internal(root, &key)
                .await
                .map(|opt| opt.map(|data| (key, data))),
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
    fn collects_find_next<'a, P, F, T>(
        &'a self,
        root: NodeId,
        cursor: Option<Key>,
        pred: &'a P,
        extract: &'a F,
    ) -> BoxFuture<'a, Option<(Result<T>, Option<Option<Key>>)>>
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync + 'a,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        Box::pin(async move {
            // Get next key and data in one call
            match self.get_next_internal(root, cursor.as_ref()).await {
                Ok(None) => None,               // No more entries - end stream
                Err(e) => Some((Err(e), None)), // Yield error and end stream
                Ok(Some((key, data))) => {
                    self.collects_apply_filters(root, key, data, pred, extract)
                        .await
                }
            }
        })
    }

    /// Apply predicate and extract for `collects_find_next`.
    fn collects_apply_filters<'a, P, F, T>(
        &'a self,
        root: NodeId,
        key: Key,
        data: Arc<NodeData>,
        pred: &'a P,
        extract: &'a F,
    ) -> BoxFuture<'a, Option<(Result<T>, Option<Option<Key>>)>>
    where
        P: Fn(&Key, &NodeData) -> bool + Send + Sync + 'a,
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        Box::pin(async move {
            if pred(&key, &data) {
                match extract(&key, &data) {
                    Some(val) => Some((Ok(val), Some(Some(key)))),
                    None => {
                        // Extract returned None - recurse to skip
                        self.collects_find_next(root, Some(key), pred, extract)
                            .await
                    }
                }
            } else {
                // Predicate returned false - recurse to skip
                self.collects_find_next(root, Some(key), pred, extract)
                    .await
            }
        })
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
    fn collects_prefix_first<'a, F, T>(
        &'a self,
        root: NodeId,
        prefix: &'a Key,
        extract: &'a F,
    ) -> BoxFuture<'a, Option<(Result<T>, Option<Option<Key>>)>>
    where
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        Box::pin(async move {
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
                            self.collects_prefix_next(
                                root, prefix, prefix, extract,
                            )
                            .await
                        }
                    }
                }
            }
        })
    }

    /// Helper for `collects_prefix_internal`: find next entry after cursor.
    fn collects_prefix_next<'a, F, T>(
        &'a self,
        root: NodeId,
        prefix: &'a Key,
        cursor: &'a Key,
        extract: &'a F,
    ) -> BoxFuture<'a, Option<(Result<T>, Option<Option<Key>>)>>
    where
        F: Fn(&Key, &NodeData) -> Option<T> + Send + Sync + 'a,
        T: Send + 'a,
    {
        Box::pin(async move {
            // Find entry after cursor
            match self.get_next_internal(root, Some(cursor)).await {
                Err(e) => Some((Err(e), None)), // Error, terminate
                Ok(None) => None,               // No more entries, terminate
                Ok(Some((key, data))) => {
                    // Check if key still starts with prefix
                    if key.starts_with(prefix) {
                        match extract(&key, &data) {
                            Some(val) => Some((Ok(val), Some(Some(key)))),
                            None => {
                                // Skip, find next
                                self.collects_prefix_next(
                                    root, prefix, &key, extract,
                                )
                                .await
                            }
                        }
                    } else {
                        // Key doesn't match prefix - TERMINATE (the key optimization!)
                        None
                    }
                }
            }
        })
    }

    /// Finds the leftmost (smallest) key in the subtree rooted at `node_id`.
    ///
    /// This traverses down the left spine of the tree to find the minimum key.
    fn find_leftmost_key<'a>(
        &'a self,
        node_id: NodeId,
    ) -> BoxFuture<'a, Result<Option<Key>>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            if node.is_leaf {
                // Return the first key in the leaf, if any
                Ok(node.keys.first().cloned())
            } else {
                // Recurse to the leftmost child
                match node.children.first() {
                    Some(&child_id) => self.find_leftmost_key(child_id).await,
                    None => {
                        // Internal node with no children - shouldn't happen in valid B-tree
                        // Fall back to first key in this node
                        Ok(node.keys.first().cloned())
                    }
                }
            }
        })
    }

    /// Finds the smallest key strictly greater than `target` in the subtree.
    ///
    /// This is the core of the `$ORDER` implementation. It navigates the B-tree
    /// to find the successor key, handling transitions between leaf nodes.
    ///
    /// # Algorithm
    ///
    /// For each node visited:
    /// 1. Binary search to find position where `target` would be inserted
    /// 2. If we find an exact match at position `pos`:
    ///    - If internal node and `pos+1` child exists: successor is leftmost key in that subtree
    ///    - If there's a key at `pos+1` in this node: check if it could be the answer
    /// 3. If no exact match:
    ///    - The insertion point tells us where to look for the successor
    fn find_successor_key<'a>(
        &'a self,
        node_id: NodeId,
        target: &'a Key,
    ) -> BoxFuture<'a, Result<Option<Key>>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            // Binary search: find the position where target would be inserted
            let search_result = node.keys.binary_search(target);

            if node.is_leaf {
                // In a leaf node, we need to find the first key > target
                let pos = match search_result {
                    Ok(p) => p + 1, // Found exact match, successor is at p+1
                    Err(p) => p,    // Not found, first key >= target is at p
                };

                // Return the key at that position if it exists
                Ok(node.keys.get(pos).cloned())
            } else {
                // Internal node: need to navigate children
                match search_result {
                    Ok(pos) => {
                        // Found exact match at `pos`
                        // The successor could be:
                        // 1. Leftmost key in the right subtree (child at pos+1)
                        // 2. If no such child/key exists, the next key in this node
                        match node.children.get(pos + 1) {
                            Some(&child_id) => {
                                // Try to find leftmost in right subtree
                                let left =
                                    self.find_leftmost_key(child_id).await?;
                                match left {
                                    Some(k) => Ok(Some(k)),
                                    None => {
                                        // Right subtree is empty, try next key
                                        Ok(node.keys.get(pos + 1).cloned())
                                    }
                                }
                            }
                            None => {
                                // No right child, try next key in node
                                Ok(node.keys.get(pos + 1).cloned())
                            }
                        }
                    }
                    Err(pos) => {
                        // Key not found; `pos` is insertion point
                        // The successor could be:
                        // 1. In the child at `pos` (keys < key at pos)
                        // 2. The key at `pos` itself
                        // 3. In a subtree to the right
                        match node.children.get(pos) {
                            Some(&child_id) => {
                                // Search in the appropriate child first
                                let child_result = self
                                    .find_successor_key(child_id, target)
                                    .await?;
                                match child_result {
                                    Some(k) => Ok(Some(k)),
                                    None => {
                                        // Child had no successor, try key at `pos`
                                        Ok(node.keys.get(pos).cloned())
                                    }
                                }
                            }
                            None => {
                                // No child at pos, return key at pos if it exists
                                Ok(node.keys.get(pos).cloned())
                            }
                        }
                    }
                }
            }
        })
    }

    /// Finds and returns a node by its ID from the in-memory cache.
    ///
    /// This is an internal helper method used by tree traversal operations.
    /// It looks up the node in the in-memory `HashMap` and clones it.
    ///
    /// For cache-aware loading that will support disk persistence in Phase 4,
    /// use `load_node()` instead.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NodeNotFound` if the node doesn't exist.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Internal use only - not exposed in public API
    /// let node = btree.find_node(node_id).await?;
    /// ```
    async fn find_node(&self, id: NodeId) -> Result<Node> {
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
    /// # Returns
    ///
    /// A tuple of:
    /// - `Key`: The median key to be promoted to the parent
    /// - `NodeData`: The median's associated data value
    /// - `NodeId`: The ID of the newly created right node
    ///
    /// The original node (identified by `id`) is modified in place to
    /// contain only the left half of the keys.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The node doesn't exist (`NodeNotFound`)
    /// - The node is not full enough to split
    /// - Node allocation fails
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Internal use only - used during SET operations
    /// let (median_key, median_data, right_id) = btree.split_node(full_node_id).await?;
    /// // Caller must promote median_key and median_data to parent and link right_id
    /// ```
    async fn split_node(
        &self,
        id: NodeId,
    ) -> Result<(Key, Arc<NodeData>, NodeId)> {
        // Find the node to split (returns owned node)
        let node = self.load_node(id).await?;

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

                // Split keys efficiently using split_off
                // keys = [0..mid, mid, mid+1..end]
                // After split_off: keys = [0..mid, mid], right_keys = [mid+1..end]
                let right_keys = keys.split_off(mid + 1);
                // Pop the median from left side: keys = [0..mid]
                let med_key = keys.pop().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Failed to extract median key".to_string(),
                    )
                })?;

                // Split values the same way and extract median value
                let right_vals = values.split_off(mid + 1);
                let med_val = values.pop().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Failed to extract median value".to_string(),
                    )
                })?;

                // Split children for internal nodes
                // For n keys, there are n+1 children
                // Left node (mid keys) needs mid+1 children: [0..=mid]
                // Right node needs remaining children: [mid+1..end]
                let right_children = if is_leaf {
                    Vec::new()
                } else {
                    children.split_off(mid + 1)
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
                let mut stats = self.stats.write().await;
                stats.splits += 1;
                stats.node_count += 1;

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
    /// # Parameters
    ///
    /// - `left_id`: ID of the left sibling node (will contain merged result)
    /// - `separator_key`: The key from the parent between these siblings
    /// - `separator_value`: The value associated with the separator key
    /// - `right_id`: ID of the right sibling node (will be deallocated)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Either node doesn't exist (`NodeNotFound`)
    /// - Nodes are incompatible (one leaf, one internal)
    /// - Deallocation fails
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Internal use only - used during KILL operations
    /// btree.merge_nodes(left_id, separator_key, separator_value, right_id).await?;
    /// // Caller must remove separator from parent and update child pointer
    /// ```
    async fn merge_nodes(
        &self,
        left_id: NodeId,
        sep_key: Key,
        sep_val: Arc<NodeData>,
        right_id: NodeId,
    ) -> Result<()> {
        // Find both nodes
        let left = self.load_node(left_id).await?;
        let right = self.load_node(right_id).await?;

        // Verify they're compatible (both leaf or both internal)
        if left.is_leaf != right.is_leaf {
            Err(StorageError::InvalidOperation(
                "Cannot merge leaf and internal nodes".to_string(),
            ))
        } else {
            // Destructure to take ownership of components
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
            {
                let mut nodes = self.nodes.write().await;
                nodes.remove(&right_id);
            }
            self.allocator.deallocate(right_id).await?;

            // Update statistics
            let mut stats = self.stats.write().await;
            stats.merges += 1;
            stats.node_count = stats.node_count.saturating_sub(1);

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
    fn search_from_node<'a>(
        &'a self,
        node_id: NodeId,
        key: &'a Key,
    ) -> BoxFuture<'a, Result<Option<Arc<NodeData>>>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            match node.keys.binary_search(key) {
                Ok(pos) => {
                    let value = node.values.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Value index {} out of bounds (len {})",
                            pos,
                            node.values.len()
                        ))
                    })?;
                    Ok(Some(Arc::clone(value)))
                }
                Err(pos) => {
                    if node.is_leaf {
                        Ok(None)
                    } else {
                        let child_id =
                            node.children.get(pos).ok_or_else(|| {
                                StorageError::InvalidOperation(format!(
                                    "Child index {} out of bounds (len {})",
                                    pos,
                                    node.children.len()
                                ))
                            })?;
                        self.search_from_node(*child_id, key).await
                    }
                }
            }
        })
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
                keys: vec![med_key],
                children: vec![root, right_id],
                values: vec![Arc::clone(&med_val)],
                is_leaf: false,
            };

            // Insert new root
            self.save_node(new_root_id, new_root_node).await?;

            // Update height
            {
                let mut stats = self.stats.write().await;
                stats.height += 1;
                stats.node_count += 1;
            }

            new_root_id
        } else {
            root
        };

        // Insert into the non-full root using NodeData
        self.insert_non_full_with_data(new_root, key, data).await?;

        // Update key count statistics
        {
            let mut stats = self.stats.write().await;
            stats.key_count += 1;
        }

        Ok(new_root)
    }

    /// Collects all keys that start with the given prefix.
    ///
    /// Returns a vector of all keys (including the prefix itself if it exists)
    /// that have the prefix as their starting subscripts.
    async fn collect_keys_with_prefix(
        &self,
        root: NodeId,
        prefix: &Key,
    ) -> Result<Vec<Key>> {
        self.collect_keys_from_node(root, prefix).await
    }

    /// Recursively collects keys from a node that match the prefix.
    fn collect_keys_from_node<'a>(
        &'a self,
        node_id: NodeId,
        prefix: &'a Key,
    ) -> BoxFuture<'a, Result<Vec<Key>>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            // Binary search to find starting position for prefix range
            let start = node.keys.binary_search(prefix).unwrap_or_else(|p| p);

            // Collect matching keys from this node
            let matching: Vec<Key> = node
                .keys
                .iter()
                .skip(start)
                .take_while(|k| k.starts_with(prefix))
                .cloned()
                .collect();

            // For internal nodes, also check children that could contain matches
            if node.is_leaf {
                Ok(matching)
            } else {
                // Children at indices [start, start + matching.len()] could contain matches
                let end = start + matching.len() + 1;
                let child_results = futures::future::try_join_all(
                    (start..end)
                        .filter_map(|i| node.children.get(i).copied())
                        .map(|c| self.collect_keys_from_node(c, prefix)),
                )
                .await?;

                Ok(child_results.into_iter().fold(matching, |mut acc, keys| {
                    acc.extend(keys);
                    acc
                }))
            }
        })
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
    fn delete_key_from_node<'a>(
        &'a self,
        node_id: NodeId,
        key: &'a Key,
        ancestors: Vec<(NodeId, usize)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            match node.keys.binary_search(key) {
                Ok(pos) => {
                    if node.is_leaf {
                        // Case 1: Key is in a leaf - remove it directly
                        {
                            let mut nodes = self.nodes.write().await;
                            let n =
                                nodes.get_mut(&node_id).ok_or_else(|| {
                                    StorageError::NodeNotFound(node_id.into())
                                })?;
                            n.keys.remove(pos);
                            n.values.remove(pos);
                        }
                        {
                            let mut stats = self.stats.write().await;
                            stats.key_count = stats.key_count.saturating_sub(1);
                        }
                        self.rebalance_with_ancestors(node_id, ancestors).await
                    } else {
                        // Case 2: Key is in an internal node - replace with predecessor
                        let left_child =
                            *node.children.get(pos).ok_or_else(|| {
                                StorageError::InvalidOperation(format!(
                                    "Child index {} out of bounds",
                                    pos
                                ))
                            })?;
                        let (pred_key, pred_val) =
                            self.find_predecessor(left_child).await?;

                        // Replace key with predecessor
                        {
                            let mut nodes = self.nodes.write().await;
                            let n =
                                nodes.get_mut(&node_id).ok_or_else(|| {
                                    StorageError::NodeNotFound(node_id.into())
                                })?;
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
                        }

                        // Delete predecessor from left subtree
                        // Pass ancestors WITH current node so rebalancing propagates
                        // correctly through this node and up to the root.
                        // Do NOT call rebalance_with_ancestors again after - the
                        // recursive delete already handles it.
                        let mut child_ancestors = ancestors;
                        child_ancestors.push((node_id, pos));
                        self.delete_key_from_node(
                            left_child,
                            &pred_key,
                            child_ancestors,
                        )
                        .await
                    }
                }
                Err(pos) => {
                    if node.is_leaf {
                        Ok(()) // Key not found, nothing to delete
                    } else {
                        let child_id =
                            node.children.get(pos).copied().ok_or_else(
                                || {
                                    StorageError::InvalidOperation(format!(
                                        "Child index {} out of bounds",
                                        pos
                                    ))
                                },
                            )?;
                        drop(node);
                        let mut child_ancestors = ancestors;
                        child_ancestors.push((node_id, pos));
                        self.delete_key_from_node(
                            child_id,
                            key,
                            child_ancestors,
                        )
                        .await
                    }
                }
            }
        })
    }

    /// Finds the predecessor (rightmost key in subtree).
    fn find_predecessor<'a>(
        &'a self,
        node_id: NodeId,
    ) -> BoxFuture<'a, Result<(Key, Arc<NodeData>)>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;
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
                Ok((k.clone(), Arc::clone(v)))
            } else {
                let child = *node.children.last().ok_or_else(|| {
                    StorageError::InvalidOperation("No children".to_string())
                })?;
                self.find_predecessor(child).await
            }
        })
    }

    /// Rebalances a node if underfull, propagating fixes up the ancestor chain.
    fn rebalance_with_ancestors<'a>(
        &'a self,
        node_id: NodeId,
        mut ancestors: Vec<(NodeId, usize)>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;
            let min_keys = self.min_degree - 1;

            // Root can have fewer keys, or node is not underfull
            match ancestors.pop() {
                None => Ok(()), // This is the root
                Some((parent_id, child_idx)) if node.keys.len() < min_keys => {
                    drop(node);
                    let merged =
                        self.fix_underfull_node(parent_id, child_idx).await?;
                    // If we merged, parent lost a key - check if parent needs fixing
                    if merged {
                        self.rebalance_with_ancestors(parent_id, ancestors)
                            .await
                    } else {
                        Ok(())
                    }
                }
                _ => Ok(()),
            }
        })
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
        let mut nodes = self.nodes.write().await;
        let err = |i| {
            StorageError::InvalidOperation(format!("Index {} out of bounds", i))
        };

        let parent = nodes
            .get(&parent_id)
            .ok_or_else(|| StorageError::NodeNotFound(parent_id.into()))?
            .clone();

        let child_id = *parent
            .children
            .get(child_idx)
            .ok_or_else(|| err(child_idx))?;
        let sep_idx = match dir {
            BorrowDir::Left => child_idx - 1,
            BorrowDir::Right => child_idx,
        };

        // Get separator from parent
        let sep_key = parent
            .keys
            .get(sep_idx)
            .ok_or_else(|| err(sep_idx))?
            .clone();
        let sep_val =
            Arc::clone(parent.values.get(sep_idx).ok_or_else(|| err(sep_idx))?);

        // Extract from sibling (pop from end if left, drain first if right)
        let (new_sep_key, new_sep_val, borrowed_child) = {
            let sib = nodes
                .get_mut(&sib_id)
                .ok_or_else(|| StorageError::NodeNotFound(sib_id.into()))?;
            let empty =
                || StorageError::InvalidOperation("Empty sibling".into());
            match dir {
                BorrowDir::Left => (
                    sib.keys.pop().ok_or_else(empty)?,
                    sib.values.pop().ok_or_else(empty)?,
                    if sib.is_leaf {
                        None
                    } else {
                        sib.children.pop()
                    },
                ),
                BorrowDir::Right => (
                    sib.keys.drain(..1).next().ok_or_else(empty)?,
                    sib.values.drain(..1).next().ok_or_else(empty)?,
                    if sib.is_leaf {
                        None
                    } else {
                        sib.children.drain(..1).next()
                    },
                ),
            }
        };

        // Insert separator into child (front if left, end if right)
        {
            let child = nodes
                .get_mut(&child_id)
                .ok_or_else(|| StorageError::NodeNotFound(child_id.into()))?;
            match dir {
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
            }
        }

        // Update separator in parent
        {
            let p = nodes
                .get_mut(&parent_id)
                .ok_or_else(|| StorageError::NodeNotFound(parent_id.into()))?;
            *p.keys.get_mut(sep_idx).ok_or_else(|| err(sep_idx))? = new_sep_key;
            *p.values.get_mut(sep_idx).ok_or_else(|| err(sep_idx))? =
                new_sep_val;
        }

        Ok(())
    }

    /// Removes the separator key and child pointer from parent after merge.
    async fn remove_separator_from_parent(
        &self,
        parent_id: NodeId,
        sep_idx: usize,
    ) -> Result<()> {
        let mut nodes = self.nodes.write().await;
        let parent = nodes
            .get_mut(&parent_id)
            .ok_or_else(|| StorageError::NodeNotFound(parent_id.into()))?;

        parent.keys.remove(sep_idx);
        parent.values.remove(sep_idx);
        parent.children.remove(sep_idx + 1);

        // Update key count
        drop(nodes);
        let mut stats = self.stats.write().await;
        stats.key_count = stats.key_count.saturating_sub(1);
        drop(stats);

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
                {
                    let mut nodes = self.nodes.write().await;
                    nodes.remove(&root);
                }
                self.allocator.deallocate(root).await?;

                // Update height
                {
                    let mut stats = self.stats.write().await;
                    stats.height = stats.height.saturating_sub(1);
                    stats.node_count = stats.node_count.saturating_sub(1);
                }

                Ok(Some(new_root))
            }
            (true, true, _) => {
                // Root is empty leaf - tree is now empty
                // Deallocate the root node
                {
                    let mut nodes = self.nodes.write().await;
                    nodes.remove(&root);
                }
                self.allocator.deallocate(root).await?;

                // Update stats
                {
                    let mut stats = self.stats.write().await;
                    stats.height = 0;
                    stats.node_count = stats.node_count.saturating_sub(1);
                }

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
    fn check_descendants_from_node<'a>(
        &'a self,
        node_id: NodeId,
        prefix: &'a Key,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            // Binary search to find starting position
            let start = node.keys.binary_search(prefix).unwrap_or_else(|p| p);

            // Check keys in this node (excluding exact match)
            let found = node
                .keys
                .iter()
                .skip(start)
                .take_while(|k| k.starts_with(prefix))
                .any(|k| k != prefix);

            if found {
                Ok(true)
            } else if node.is_leaf {
                Ok(false)
            } else {
                // Check children that might contain descendants
                // Count matching keys to determine child range
                let matching_count = node
                    .keys
                    .iter()
                    .skip(start)
                    .take_while(|k| k.starts_with(prefix))
                    .count();
                let end = start + matching_count + 1;

                futures::stream::iter(
                    (start..end)
                        .filter_map(|i| node.children.get(i).copied())
                        .map(Ok::<_, StorageError>),
                )
                .try_fold(false, |acc, child_id| async move {
                    if acc {
                        Ok(true)
                    } else {
                        self.check_descendants_from_node(child_id, prefix).await
                    }
                })
                .await
            }
        })
    }

    /// Inserts a key-value pair into a non-full node.
    ///
    /// Delegates to `insert_non_full_with_data()` with appropriate `NodeData`.
    fn insert_non_full<'a>(
        &'a self,
        node_id: NodeId,
        key: &'a Key,
        value: rumps_types::Value,
    ) -> BoxFuture<'a, Result<()>> {
        self.insert_non_full_with_data(
            node_id,
            key,
            NodeData::with_value(value),
        )
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
    fn insert_non_full_with_data<'a>(
        &'a self,
        node_id: NodeId,
        key: &'a Key,
        data: NodeData,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            // Find the position where the key should be inserted
            let pos = node
                .keys
                .binary_search(key)
                .unwrap_or_else(|insert_pos| insert_pos);

            if node.is_leaf {
                // Leaf node: insert or merge the key-value pair
                let mut updated_node = node;

                match updated_node.keys.get(pos) {
                    Some(existing_key) if existing_key == key => {
                        // Key exists - MERGE NodeData with OR semantics
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
                            existing_data.has_descendants
                                || data.has_descendants,
                        );
                        let values_len = updated_node.values.len();
                        *updated_node.values.get_mut(pos).ok_or_else(|| {
                            StorageError::InvalidOperation(format!(
                                "Value index {} out of bounds for mutation (len {})",
                                pos,
                                values_len
                            ))
                        })? = Arc::new(merged_data);
                    }
                    _ => {
                        // Key doesn't exist, insert new NodeData
                        updated_node.keys.insert(pos, key.clone());
                        updated_node.values.insert(pos, Arc::new(data));
                    }
                }

                // Write the updated node back
                self.save_node(node_id, updated_node).await?;

                Ok(())
            } else {
                // Internal node: recurse to the appropriate child
                let child_id = *node.children.get(pos).ok_or_else(|| {
                    StorageError::InvalidOperation(format!(
                        "Child index {} out of bounds (len {})",
                        pos,
                        node.children.len()
                    ))
                })?;

                // Check if child is full
                let child = self.load_node(child_id).await?;
                let max_keys = 2 * self.min_degree - 1;

                if child.keys.len() == max_keys {
                    // Child is full, split it first
                    let (median_key, median_value, new_child_id) =
                        self.split_node(child_id).await?;

                    // Insert median into this node
                    let mut updated_node = node;
                    updated_node.keys.insert(pos, median_key.clone());
                    updated_node.values.insert(pos, Arc::clone(&median_value));
                    updated_node.children.insert(pos + 1, new_child_id);

                    // Write updated parent
                    self.save_node(node_id, updated_node.clone()).await?;

                    // Determine which child to recurse into
                    let next_child_id = if *key > median_key {
                        new_child_id
                    } else {
                        child_id
                    };

                    self.insert_non_full_with_data(next_child_id, key, data)
                        .await
                } else {
                    // Child is not full, recurse directly
                    drop(child);
                    drop(node);
                    self.insert_non_full_with_data(child_id, key, data).await
                }
            }
        })
    }

    /// Updates the `has_descendants` flag for an existing key.
    ///
    /// This is used when an ancestor already exists but needs its flag updated.
    ///
    /// # Errors
    ///
    /// Returns an error if the key doesn't exist.
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
    fn update_flag_in_node<'a>(
        &'a self,
        node_id: NodeId,
        key: &'a Key,
        flag: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;
            let pos = node.keys.binary_search(key).unwrap_or_else(|p| p);

            match node.keys.get(pos) {
                Some(k) if k == key => {
                    // Found the key - update the flag directly
                    let mut nodes = self.nodes.write().await;
                    let n = nodes.get_mut(&node_id).ok_or_else(|| {
                        StorageError::NodeNotFound(node_id.into())
                    })?;
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
                }
                _ if node.is_leaf => Err(StorageError::InvalidOperation(
                    format!("Key {:?} not found for flag update", key),
                )),
                _ => {
                    let child = *node.children.get(pos).ok_or_else(|| {
                        StorageError::InvalidOperation(format!(
                            "Child index {} out of bounds",
                            pos
                        ))
                    })?;
                    self.update_flag_in_node(child, key, flag).await
                }
            }
        })
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
