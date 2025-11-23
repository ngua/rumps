// TODO: Remove this once Phase 4-5 are implemented and all methods are actually used
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use rumps_types::{DataStatus, Key, Name, Node, NodeData, NodeId};
use tokio::sync::RwLock;

use crate::error::{Result, StorageError};

/// Statistics tracking for B-tree operations.
#[derive(Debug, Clone, Default)]
pub(crate) struct BTreeStats {
    /// Current height of the tree
    pub height: usize,
    /// Total number of nodes
    pub node_count: usize,
    /// Total number of keys across all nodes
    pub key_count: usize,
    /// Average fill factor (keys per node / max keys per node)
    pub avg_fill_factor: f64,
    /// Memory usage in bytes (estimated)
    pub memory_bytes: usize,
    /// Number of splits performed
    pub splits: u64,
    /// Number of merges performed
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

/// In-memory B-tree storage for RUMPS database.
///
/// This structure manages the hierarchical storage of both persistent globals
/// (`^NAME`) and ephemeral locals (`NAME`). All operations are async to support
/// future disk persistence without API changes.
///
/// # B-tree Properties
///
/// For a B-tree of minimum degree `t` (where `min_degree = t`):
/// - Each node (except root) contains `t-1` to `2t-1` keys
/// - Each internal node (except root) has `t` to `2t` children
/// - The root has 1 to `2t-1` keys (can be smaller)
/// - All leaves are at the same depth
/// - Keys within a node are sorted in ascending order
///
/// # Design Decisions
///
/// ## BTreeMap for roots
/// Variable names are stored in a `BTreeMap` to support ordered iteration,
/// enabling MUMPS `$ORDER` semantics over variable names themselves.
///
/// ## `RwLock<HashMap>` for nodes
/// Nodes are stored in an async-aware `RwLock<HashMap>` because:
/// - `NodeId`s are arbitrary internal references (like page IDs)
/// - Logical ordering is maintained by the tree structure, not `NodeId` values
/// - `RwLock` enables concurrent reads with exclusive writes
/// - Async from day one prevents breaking API changes when adding disk I/O
///
/// In Phase 2-3, this is pure in-memory storage. In Phase 4, it becomes
/// a page cache with lazy loading from disk.
///
/// # Scalability Considerations
///
/// ## Root Index Design
///
/// The root index (`roots: RwLock<BTreeMap<Name, NodeId>>`) keeps all variable
/// names in memory. This design assumes the typical MUMPS deployment pattern:
///
/// **Typical**: Hundreds of globals, each with millions of child records
/// - Example: `^PATIENT` with 5 million patient records
/// - Example: `^ORDER` with 10 million order records
/// - Root index: ~100-500 variable names (~20 KB in memory)
///
/// **Not Typical**: Millions of distinct globals
/// - This would require gigabytes of memory just for root names
/// - Would create lock contention on the single `RwLock`
///
/// Real-world MUMPS deployments (hospitals, banks, etc.) follow the "hundreds
/// of globals" pattern, where the data volume comes from deep hierarchical
/// subscripting within each global, not from proliferating global names.
///
/// ## When This Design Breaks Down
///
/// If you need millions of distinct globals, you would need:
/// - Hierarchical root index (disk-backed B-tree of variable names)
/// - Sharding/partitioning of the namespace
/// - Lazy loading of root mappings with an LRU cache
/// - Granular locking (lock striping or optimistic concurrency)
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
/// // (nodes will have 2-5 keys)
/// let btree = Arc::new(BTree::new(3)?);
/// assert_eq!(btree.min_degree(), 3);
/// assert_eq!(btree.node_count().await, 0);
///
/// // Use default configuration (min_degree = 3)
/// let btree = Arc::new(BTree::default());
/// assert_eq!(btree.min_degree(), 3);
/// # Ok::<(), rumps_storage::StorageError>(())
/// # });
/// ```
pub(crate) struct BTree {
    /// Maps variable names to root nodes (maintains sorted order).
    ///
    /// This `BTreeMap` enables ordered iteration over variable names,
    /// supporting MUMPS `$ORDER` semantics. Both `Global` and `Local`
    /// variables are stored in the same map with consistent ordering.
    roots: RwLock<BTreeMap<Name, NodeId>>,

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

impl BTree {
    /// Creates a new empty B-tree with the specified minimum degree.
    ///
    /// # Errors
    ///
    /// Returns an error if `min_degree < 2` (invalid B-tree configuration).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::BTree;
    /// use std::sync::Arc;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = Arc::new(BTree::new(3)?);
    /// assert_eq!(btree.min_degree(), 3);
    /// assert_eq!(btree.node_count().await, 0);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) fn new(min_degree: usize) -> Result<Self> {
        if min_degree >= 2 {
            Ok(Self {
                roots: RwLock::new(BTreeMap::new()),
                nodes: RwLock::new(HashMap::new()),
                allocator: Arc::new(IncrementingAllocator::new()),
                min_degree,
                max_memory_bytes: None,
                stats: RwLock::new(BTreeStats::default()),
            })
        } else {
            Err(StorageError::InvalidConfiguration(
                "min_degree must be >= 2".to_string(),
            ))
        }
    }

    /// Creates a new B-tree with custom configuration.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::BTree;
    /// use std::sync::Arc;
    ///
    /// # tokio_test::block_on(async {
    /// // Create with 10MB memory limit
    /// let btree = Arc::new(BTree::with_config(3, Some(10_000_000))?);
    /// assert_eq!(btree.min_degree(), 3);
    /// assert!(btree.has_memory_limit());
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) fn with_config(
        min_degree: usize,
        max_memory_bytes: Option<usize>,
    ) -> Result<Self> {
        let mut btree = Self::new(min_degree)?;
        btree.max_memory_bytes = max_memory_bytes;
        Ok(btree)
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

    /// Loads a node from cache or disk (Phase 4).
    ///
    /// This is the cache-aware node loading method that will be used throughout
    /// the codebase for retrieving nodes.
    ///
    /// # Current Implementation (Phase 2-3)
    ///
    /// Currently just looks up the node in the in-memory `HashMap`.
    ///
    /// # Future Implementation (Phase 4.5)
    ///
    /// TODO Phase 4.5: Implement cache-aware disk loading:
    /// - Check cache first (`nodes` HashMap)
    /// - Load from disk if cache miss (only for `Name::Global`)
    /// - Keep `Name::Local` entirely in memory
    /// - Add to cache with LRU eviction
    /// - See TODOS/persistence.md Phase 4.5 for details
    ///
    /// # Errors
    ///
    /// Returns `StorageError::NodeNotFound` if the node doesn't exist.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let node = btree.load_node(root_id).await?;
    /// ```
    async fn load_node(&self, id: NodeId) -> Result<Node> {
        // TODO Phase 4.5: Add disk loading logic here
        self.find_node(id).await
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
        let nodes = self.nodes.read().await;
        nodes
            .get(&id)
            .cloned()
            .ok_or(StorageError::NodeNotFound(id))
    }

    /// Splits a full node into two nodes.
    ///
    /// This operation is used when a node reaches maximum capacity
    /// (2*min_degree - 1 keys). The node is split at the median:
    /// - Left half: keys[0..mid] remain in the original node
    /// - Median key: returned to be promoted to parent
    /// - Right half: keys[mid+1..] moved to new node
    ///
    /// For internal nodes, children are also split appropriately:
    /// - Left node gets children[0..=mid]
    /// - Right node gets children[mid+1..]
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
    /// Before split (min_degree=3, node has 5 keys):
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
                let median_key = keys.pop().ok_or_else(|| {
                    StorageError::InvalidOperation(
                        "Failed to extract median key".to_string(),
                    )
                })?;

                // Split values the same way and extract median value
                let right_values = values.split_off(mid + 1);
                let median_value = values.pop().ok_or_else(|| {
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
                    values: right_values,
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
                let mut nodes = self.nodes.write().await;
                nodes.insert(id, left_node);
                nodes.insert(right_id, right_node);
                drop(nodes);

                // Update statistics
                let mut stats = self.stats.write().await;
                stats.splits += 1;
                stats.node_count += 1;

                Ok((median_key, median_value, right_id))
            }
        }
    }

    /// Merges two underfull sibling nodes into one node.
    ///
    /// This operation is the inverse of `split_node` and is used when nodes
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
        left: NodeId,
        separator_key: Key,
        separator_value: Arc<NodeData>,
        right: NodeId,
    ) -> Result<()> {
        // Find both nodes
        let left_node = self.load_node(left).await?;
        let right_node = self.load_node(right).await?;

        // Verify they're compatible (both leaf or both internal)
        if left_node.is_leaf != right_node.is_leaf {
            Err(StorageError::InvalidOperation(
                "Cannot merge leaf and internal nodes".to_string(),
            ))
        } else {
            // Destructure to take ownership of components
            let Node {
                keys: mut left_keys,
                children: mut left_children,
                values: mut left_values,
                is_leaf,
            } = left_node;

            let Node {
                keys: right_keys,
                children: right_children,
                values: right_values,
                is_leaf: _,
            } = right_node;

            // Combine: left + separator + right
            left_keys.push(separator_key);
            left_keys.extend(right_keys);

            left_values.push(Arc::clone(&separator_value));
            left_values.extend(right_values);

            // For internal nodes, merge children
            if !is_leaf {
                left_children.extend(right_children);
            }

            // Create merged node
            let merged_node = Node {
                keys: left_keys,
                children: left_children,
                values: left_values,
                is_leaf,
            };

            // Write merged node and remove right node
            let mut nodes = self.nodes.write().await;
            nodes.insert(left, merged_node);
            nodes.remove(&right);
            drop(nodes);

            // Deallocate right node ID
            self.allocator.deallocate(right).await?;

            // Update statistics
            let mut stats = self.stats.write().await;
            stats.merges += 1;
            stats.node_count = stats.node_count.saturating_sub(1);

            Ok(())
        }
    }
}

impl Default for BTree {
    /// Creates a B-tree with default minimum degree of 3.
    ///
    /// This provides a good balance between tree height and node utilization:
    /// - Nodes contain 2-5 keys
    /// - Internal nodes have 3-6 children
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::default();
    /// assert_eq!(btree.min_degree(), 3);
    /// assert_eq!(btree.node_count().await, 0);
    /// # });
    /// ```
    fn default() -> Self {
        Self::new(3).expect("Default configuration is valid")
    }
}

/// MUMPS primitive operations (SET, GET, KILL, DATA, ORDER).
///
/// Public API - All write operations require a TransactionContext.
/// Read operations can optionally use a TransactionContext for snapshot isolation.
impl BTree {
    /// Sets a value in the tree at the specified variable name and key.
    ///
    /// **Requires a transaction context.** All writes to globals must occur within transactions.
    ///
    /// # Arguments
    ///
    /// * `name` - The variable name (Global or Local)
    /// * `key` - The key path
    /// * `value` - The value to store
    /// * `txn` - Transaction context (required for all writes)
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::{BTree, TransactionContext};
    /// use rumps_types::{Name, Key, Value, TransactionId, TransactionTimestamp};
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3)?;
    /// let txn = TransactionContext::new(
    ///     TransactionId::from(1),
    ///     TransactionTimestamp::from(100),
    /// );
    ///
    /// let name = Name::Global("PATIENT".into());
    /// let key = Key::from(vec![123.into()]);
    /// btree.set(&name, &key, "John Doe".into(), &txn).await?;
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub(crate) async fn set(
        &self,
        name: &Name,
        key: &Key,
        value: rumps_types::Value,
        _ctx: &crate::TransactionContext,
    ) -> Result<()> {
        // TODO Phase 5: Use transaction context for snapshot isolation
        // and buffered writes (e.g., write to transaction buffer instead
        // of directly to tree). For now, we just delegate to `set_internal`.
        self.set_internal(name, key, NodeData::with_value(value))
            .await
    }

    /// Gets a value from the tree.
    ///
    /// Optional transaction context for snapshot isolation (Phase 5).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let value = btree.get(&name, &key, None).await?;
    /// ```
    pub(crate) async fn get(
        &self,
        name: &Name,
        key: &Key,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<Option<rumps_types::Value>> {
        // TODO Phase 5: If txn is Some, use snapshot isolation
        self.get_internal(name, key)
            .await
            .map(|opt| opt.and_then(|data| data.value.clone()))
    }

    /// Deletes a key and all its descendants from the tree.
    ///
    /// **Requires a transaction context.** All writes to globals must occur within transactions.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// btree.kill(&name, &key, &txn).await?;
    /// ```
    pub(crate) async fn kill(
        &self,
        _name: &Name,
        _key: &Key,
        _ctx: &crate::TransactionContext,
    ) -> Result<()> {
        // TODO Phase 2.4: Implement KILL operation
        todo!("KILL operation not yet implemented - see TODOS/persistence.md Phase 2.4")
    }

    /// Checks the data status of a node (MUMPS $DATA).
    ///
    /// Returns information about whether a node has a value and/or descendants.
    /// Optional transaction context for snapshot isolation (Phase 5).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let status = btree.data(&name, &key, None).await?;
    /// ```
    pub(crate) async fn data(
        &self,
        _name: &Name,
        _key: &Key,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<DataStatus> {
        // TODO Phase 2.5: Implement DATA operation
        todo!("DATA operation not yet implemented - see TODOS/persistence.md Phase 2.5")
    }

    /// Returns the next key in lexicographic order (MUMPS $ORDER).
    ///
    /// Optional transaction context for snapshot isolation (Phase 5).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let next_key = btree.order(&name, Some(&key), None).await?;
    /// ```
    pub(crate) async fn order(
        &self,
        _name: &Name,
        _after: Option<&Key>,
        _ctx: Option<&crate::TransactionContext>,
    ) -> Result<Option<Key>> {
        // TODO Phase 2.6: Implement ORDER operation
        todo!("ORDER operation not yet implemented - see TODOS/persistence.md Phase 2.6")
    }
}

/// Private helper methods for B-tree operations.
impl BTree {
    /// Internal GET that returns `Arc<NodeData>` (not just Value).
    ///
    /// Returns an Arc for efficient hierarchy navigation - checking
    /// `has_descendants` flags is much cheaper with Arc::clone() than
    /// cloning the entire `NodeData`.
    ///
    /// The public `get()` method extracts the value by cloning the
    /// `Option<Value>` from the Arc.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Internal use only
    /// let arc_data = btree.get_internal(&name, &key).await?;
    /// if let Some(data) = arc_data {
    ///     println!("has_descendants: {}", data.has_descendants);
    ///     println!("value: {:?}", data.value);
    /// }
    /// ```
    // Internal method for tests/benchmarks - not part of public API
    async fn get_internal(
        &self,
        name: &Name,
        key: &Key,
    ) -> Result<Option<Arc<NodeData>>> {
        match self.roots.read().await.get(name).copied() {
            None => Ok(None),
            Some(root_id) => self.search_from_node(root_id, key).await,
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
    /// - If not found (`Err(pos)`) and leaf node: Key doesn't exist, return None
    /// - If not found (`Err(pos)`) and internal node: Recurse to child at pos
    ///
    /// The Err(pos) from binary_search indicates where the key would be
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
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<Arc<NodeData>>>>
                + Send
                + 'a,
        >,
    > {
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

    /// Internal SET operation that accepts `NodeData` directly.
    ///
    /// This method is used internally for maintaining hierarchical semantics,
    /// particularly when creating ancestor nodes with `has_descendants = true`.
    ///
    /// # Behavior for Existing Keys - Idempotent Merge
    ///
    /// If the key already exists, this method MERGES the `NodeData`:
    /// - `has_descendants`: Performs OR operation (if either old or new is true, result is true)
    /// - `value`: Takes new value if provided, otherwise keeps old value
    ///
    /// **Why idempotent merge is required:**
    /// - Multiple child insertions can race to create the same ancestor node
    /// - Each insertion must be able to set `has_descendants=true` independently
    /// - The operation must be safe regardless of the order or concurrency
    /// - Once `has_descendants=true` is set, it cannot be accidentally cleared
    ///
    /// This ensures that:
    /// 1. Setting `has_descendants=true` is permanent (can't be undone by another set)
    /// 2. Concurrent ancestor creation is safe (multiple operations can set same ancestor)
    /// 3. User can update values without losing `has_descendants` flag
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Create an intermediate node (no value, only descendants)
    /// btree.set_internal(&name, &ancestor_key, NodeData::with_descendants()).await?;
    ///
    /// // Later, add a value to the same node (preserves has_descendants)
    /// btree.set_internal(&name, &ancestor_key, NodeData::with_value(value)).await?;
    /// // Result: NodeData { value: Some(value), has_descendants: true }
    /// ```
    // Internal method for tests/benchmarks - not part of public API
    async fn set_internal(
        &self,
        name: &Name,
        key: &Key,
        data: NodeData,
    ) -> Result<()> {
        // Ensure all ancestors exist with has_descendants=true
        // This is part of the core MUMPS hierarchical semantics
        self.ensure_ancestors(name, key).await?;

        // Delegate to raw insertion (no hierarchy management)
        self.set_node(name, key, data).await
    }

    /// Raw node insertion without hierarchy management.
    ///
    /// This method performs the actual B-tree insertion without calling
    /// `ensure_ancestors`. It's used internally by both `set_internal`
    /// (after ensuring ancestors) and by `ensure_ancestors` itself.
    ///
    /// # Behavior for Existing Keys - Idempotent Merge
    ///
    /// If the key already exists, this method MERGES the `NodeData`:
    /// - `has_descendants`: Performs OR operation (if either old or new is true, result is true)
    /// - `value`: Takes new value if provided, otherwise keeps old value
    async fn set_node(
        &self,
        name: &Name,
        key: &Key,
        data: NodeData,
    ) -> Result<()> {
        // Look up the root node ID for this variable name
        let roots = self.roots.read().await;
        let root_id_opt = roots.get(name).copied();
        drop(roots);

        match root_id_opt {
            None => {
                // Variable doesn't exist - create a new leaf root with the NodeData
                let new_root_id = self.allocator.allocate().await?;
                let new_root = Node {
                    keys: vec![key.clone()],
                    children: vec![],
                    values: vec![Arc::new(data)],
                    is_leaf: true,
                };

                // Insert the new root into storage
                {
                    let mut nodes = self.nodes.write().await;
                    nodes.insert(new_root_id, new_root);
                }

                // Register the root in the roots map
                {
                    let mut roots = self.roots.write().await;
                    roots.insert(name.clone(), new_root_id);
                }

                // Update statistics
                {
                    let mut stats = self.stats.write().await;
                    stats.node_count += 1;
                    stats.key_count += 1;
                    stats.height = 1;
                }

                Ok(())
            }
            Some(root_id) => {
                // Variable exists - navigate tree and insert
                // Check if root is full and needs splitting
                let root = self.load_node(root_id).await?;
                let max_keys = 2 * self.min_degree - 1;

                let new_root_id = match root.keys.len() {
                    n if n == max_keys => {
                        // Root is full, split it and create a new root
                        let (median_key, median_value, right_id) =
                            self.split_node(root_id).await?;

                        // Create new root with the median
                        let new_root_id = self.allocator.allocate().await?;
                        let new_root = Node {
                            keys: vec![median_key],
                            children: vec![root_id, right_id],
                            values: vec![Arc::clone(&median_value)],
                            is_leaf: false,
                        };

                        // Insert new root
                        {
                            let mut nodes = self.nodes.write().await;
                            nodes.insert(new_root_id, new_root);
                        }

                        // Update root reference
                        {
                            let mut roots = self.roots.write().await;
                            roots.insert(name.clone(), new_root_id);
                        }

                        // Update height
                        {
                            let mut stats = self.stats.write().await;
                            stats.height += 1;
                            stats.node_count += 1;
                        }

                        new_root_id
                    }
                    _ => root_id,
                };

                // Insert into the non-full root using NodeData
                self.insert_non_full_with_data(new_root_id, key, data)
                    .await?;

                // Update key count statistics
                {
                    let mut stats = self.stats.write().await;
                    stats.key_count += 1;
                }

                Ok(())
            }
        }
    }

    // Internal method for tests/benchmarks - not part of public API
    async fn kill_internal(&self, _name: &Name, _key: &Key) -> Result<()> {
        // TODO Phase 2.4: Implement KILL operation
        todo!("KILL operation not yet implemented - see TODOS/persistence.md Phase 2.4")
    }

    // Internal method for tests/benchmarks - not part of public API
    async fn data_internal(
        &self,
        _name: &Name,
        _key: &Key,
    ) -> Result<DataStatus> {
        // TODO Phase 2.5: Implement DATA operation
        todo!("DATA operation not yet implemented - see TODOS/persistence.md Phase 2.5")
    }

    // Internal method for tests/benchmarks - not part of public API
    async fn order_internal(
        &self,
        _name: &Name,
        _after: Option<&Key>,
    ) -> Result<Option<Key>> {
        // TODO Phase 2.6: Implement ORDER operation
        todo!("ORDER operation not yet implemented - see TODOS/persistence.md Phase 2.6")
    }

    /// Inserts a key-value pair into a non-full node.
    ///
    /// This is a recursive helper for the SET operation. It assumes the given
    /// node is not full (has fewer than 2*min_degree - 1 keys).
    fn insert_non_full<'a>(
        &'a self,
        node_id: NodeId,
        key: &'a Key,
        value: rumps_types::Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>,
    > {
        Box::pin(async move {
            let node = self.load_node(node_id).await?;

            // Find the position where the key should be inserted
            let pos = node
                .keys
                .binary_search(key)
                .unwrap_or_else(|insert_pos| insert_pos);

            if node.is_leaf {
                // Leaf node: insert or update the key-value pair
                let mut updated_node = node;

                match updated_node.keys.get(pos) {
                    Some(existing_key) if existing_key == key => {
                        // Key exists - CRITICAL: preserve has_descendants flag
                        let existing_value =
                            updated_node.values.get(pos).ok_or_else(|| {
                                StorageError::InvalidOperation(format!(
                                    "Value index {} out of bounds (len {})",
                                    pos,
                                    updated_node.values.len()
                                ))
                            })?;
                        let existing_has_descendants =
                            existing_value.has_descendants;
                        let values_len = updated_node.values.len();
                        *updated_node.values.get_mut(pos).ok_or_else(|| {
                            StorageError::InvalidOperation(format!(
                                "Value index {} out of bounds for mutation (len {})",
                                pos,
                                values_len
                            ))
                        })? = Arc::new(NodeData::new(Some(value), existing_has_descendants));
                    }
                    _ => {
                        // Key doesn't exist, insert with has_descendants=false initially
                        updated_node.keys.insert(pos, key.clone());
                        updated_node
                            .values
                            .insert(pos, Arc::new(NodeData::with_value(value)));
                    }
                }

                // Write the updated node back
                let mut nodes = self.nodes.write().await;
                nodes.insert(node_id, updated_node);

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
                    {
                        let mut nodes = self.nodes.write().await;
                        nodes.insert(node_id, updated_node.clone());
                    }

                    // Determine which child to recurse into
                    let next_child_id = if *key > median_key {
                        new_child_id
                    } else {
                        child_id
                    };

                    self.insert_non_full(next_child_id, key, value).await
                } else {
                    // Child is not full, recurse directly
                    drop(child);
                    drop(node);
                    self.insert_non_full(child_id, key, value).await
                }
            }
        })
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
    /// - `has_descendants`: OR operation (old || new)
    /// - `value`: Takes new value if Some, otherwise keeps old value
    ///
    /// This ensures concurrent ancestor creation is safe and idempotent.
    fn insert_non_full_with_data<'a>(
        &'a self,
        node_id: NodeId,
        key: &'a Key,
        data: NodeData,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>,
    > {
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
                let mut nodes = self.nodes.write().await;
                nodes.insert(node_id, updated_node);

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
                    {
                        let mut nodes = self.nodes.write().await;
                        nodes.insert(node_id, updated_node.clone());
                    }

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

    /// Updates the has_descendants flag for an existing key.
    ///
    /// This is used when an ancestor already exists but needs its flag updated.
    /// Uses `set_internal()` with merged `NodeData` to preserve existing values.
    ///
    /// # Errors
    ///
    /// Returns an error if the key doesn't exist.
    async fn update_descendants_flag(
        &self,
        name: &Name,
        key: &Key,
        value: bool,
    ) -> Result<()> {
        // Get existing NodeData
        let existing_arc = self
            .get_internal(name, key)
            .await?
            .ok_or_else(|| {
                StorageError::InvalidOperation(format!(
                    "Cannot update has_descendants flag: key {:?} does not exist",
                    key
                ))
            })?;

        // Create updated NodeData with new flag value
        let updated_data = NodeData::new(existing_arc.value.clone(), value);

        // Use set_node to avoid recursive ensure_ancestors call
        self.set_node(name, key, updated_data).await
    }

    /// Ensures all ancestor keys exist with `has_descendants = true`.
    ///
    /// This method is called before inserting a new key to maintain the
    /// hierarchical structure. For each ancestor that doesn't exist, it
    /// creates an intermediate node (no value, only descendants).
    ///
    /// # Thread Safety
    ///
    /// This method is safe for concurrent execution. If multiple operations
    /// try to create the same ancestor, `set_internal()` will merge the
    /// `NodeData` using OR semantics on `has_descendants`, making the operation
    /// idempotent.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Before inserting Key([1, 2, 3])
    /// ensure_ancestors(&name, &Key::from(vec![1, 2, 3])).await?;
    /// // Creates: Key([1]) and Key([1, 2]) with has_descendants=true
    /// ```
    async fn ensure_ancestors(&self, name: &Name, key: &Key) -> Result<()> {
        use futures::stream::{self, TryStreamExt};

        let ancestors = key.ancestors();

        // Process each ancestor from root to leaf sequentially
        // Convert iterator to TryStream by mapping items to Ok
        stream::iter(
            ancestors
                .into_iter()
                .map(Ok::<_, crate::error::StorageError>),
        )
        .try_for_each(|ancestor_key| async move {
            match self.get_internal(name, &ancestor_key).await? {
                Some(node_data) => {
                    // Ancestor exists - update has_descendants if needed
                    if !node_data.has_descendants {
                        self.update_descendants_flag(name, &ancestor_key, true)
                            .await
                    } else {
                        Ok(())
                    }
                }
                None => {
                    // Ancestor doesn't exist - create intermediate node
                    // Use set_node to avoid recursive ensure_ancestors call
                    self.set_node(
                        name,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_btree_new() {
        let btree = BTree::new(3);
        assert!(btree.is_ok());
        assert_eq!(btree.unwrap().min_degree(), 3);
    }

    #[test]
    fn test_btree_new_min_degree_2() {
        let btree = BTree::new(2);
        assert!(btree.is_ok());
        assert_eq!(btree.unwrap().min_degree(), 2);
    }

    #[test]
    fn test_btree_new_invalid_min_degree_zero() {
        let btree = BTree::new(0);
        assert!(btree.is_err());
        match btree {
            Err(StorageError::InvalidConfiguration(msg)) => {
                assert!(msg.contains("min_degree must be >= 2"));
            }
            _ => panic!("Expected InvalidConfiguration error"),
        }
    }

    #[test]
    fn test_btree_new_invalid_min_degree_one() {
        let btree = BTree::new(1);
        assert!(btree.is_err());
        match btree {
            Err(StorageError::InvalidConfiguration(msg)) => {
                assert!(msg.contains("min_degree must be >= 2"));
            }
            _ => panic!("Expected InvalidConfiguration error"),
        }
    }

    #[test]
    fn test_btree_default() {
        let btree = BTree::default();
        assert_eq!(btree.min_degree(), 3);
    }

    #[tokio::test]
    async fn test_btree_initial_state() {
        let btree = BTree::new(4).unwrap();
        assert!(btree.roots.read().await.is_empty());
        assert_eq!(btree.node_count().await, 0);
        assert_eq!(btree.allocator.peek_next().await, NodeId::from(0));
    }

    #[tokio::test]
    async fn test_btree_node_count() {
        let btree = BTree::new(3).unwrap();
        assert_eq!(btree.node_count().await, 0);

        // Will add nodes in future phases
    }

    #[tokio::test]
    async fn test_btree_with_memory_limit() {
        let btree = BTree::with_config(3, Some(1_000_000)).unwrap();
        assert!(btree.has_memory_limit());
        assert_eq!(btree.min_degree(), 3);
    }

    #[tokio::test]
    async fn test_btree_stats() {
        let btree = BTree::new(3).unwrap();
        let stats = btree.stats().await;
        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.key_count, 0);
        assert_eq!(stats.height, 0);
        assert_eq!(stats.splits, 0);
        assert_eq!(stats.merges, 0);
    }

    #[tokio::test]
    async fn test_concurrent_readers() {
        use futures::future;
        use tokio::task;

        let btree = Arc::new(BTree::new(3).unwrap());

        // Spawn 10 concurrent reader tasks
        let handles = (0..10)
            .map(|_| {
                let btree_clone = Arc::clone(&btree);
                task::spawn(async move {
                    future::join_all((0..100).map(|_| async {
                        let _count = btree_clone.node_count().await;
                        let _stats = btree_clone.stats().await;
                    }))
                    .await;
                })
            })
            .collect::<Vec<_>>();

        // Wait for all tasks to complete
        future::try_join_all(handles).await.unwrap();
    }

    #[tokio::test]
    async fn test_writer_blocks_readers() {
        use tokio::time::{sleep, Duration};

        let btree = Arc::new(BTree::new(3).unwrap());

        // Acquire write lock and hold it
        let write_guard = btree.nodes.write().await;

        // Try to read concurrently (should block)
        let btree_clone = Arc::clone(&btree);
        let read_task = tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let _count = btree_clone.node_count().await;
            start.elapsed()
        });

        // Hold the write lock for a moment
        sleep(Duration::from_millis(10)).await;

        // Release write lock
        drop(write_guard);

        // Read should now complete
        let elapsed = read_task.await.unwrap();
        assert!(elapsed >= Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_stress_large_tree() {
        use std::sync::Arc;

        use futures::StreamExt;
        use rumps_types::Value;

        let btree = Arc::new(BTree::new(3).unwrap());
        let name = Name::Global("STRESS".into());

        // Insert 1000 keys with various patterns
        let num_keys: usize = 1000;

        // Pattern 1: Sequential integers at depth 1
        futures::stream::iter(0..num_keys / 2)
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    let key = Key::from(vec![(i as i64).into()]);
                    btree.ensure_ancestors(&name, &key).await.unwrap();
                    btree
                        .set_internal(
                            &name,
                            &key,
                            NodeData::with_value(Value::Integer(i as i64)),
                        )
                        .await
                        .unwrap();
                }
            })
            .collect::<Vec<_>>()
            .await;

        // Pattern 2: Nested keys at depth 2
        futures::stream::iter(0..num_keys / 4)
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    let key = Key::from(vec![1000.into(), (i as i64).into()]);
                    btree.ensure_ancestors(&name, &key).await.unwrap();
                    btree
                        .set_internal(
                            &name,
                            &key,
                            NodeData::with_value(Value::String(format!(
                                "nested_{}",
                                i
                            ))),
                        )
                        .await
                        .unwrap();
                }
            })
            .collect::<Vec<_>>()
            .await;

        // Pattern 3: Deep nesting at depth 5
        futures::stream::iter(0..num_keys / 4)
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    let key = Key::from(vec![
                        2000.into(),
                        ((i % 10) as i64).into(),
                        ((i % 5) as i64).into(),
                        ((i % 3) as i64).into(),
                        (i as i64).into(),
                    ]);
                    btree.ensure_ancestors(&name, &key).await.unwrap();
                    btree
                        .set_internal(
                            &name,
                            &key,
                            NodeData::with_value(Value::Integer(i as i64)),
                        )
                        .await
                        .unwrap();
                }
            })
            .collect::<Vec<_>>()
            .await;

        // Verify all Pattern 1 keys can be retrieved
        futures::stream::iter(0..num_keys / 2)
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    let key = Key::from(vec![(i as i64).into()]);
                    let result = btree.get_internal(&name, &key).await.unwrap();
                    assert!(result.is_some());
                    assert_eq!(
                        result.unwrap().value,
                        Some(Value::Integer(i as i64))
                    );
                }
            })
            .collect::<Vec<_>>()
            .await;

        // Verify all Pattern 2 keys can be retrieved
        futures::stream::iter(0..num_keys / 4)
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    let key = Key::from(vec![1000.into(), (i as i64).into()]);
                    let result = btree.get_internal(&name, &key).await.unwrap();
                    assert!(result.is_some());
                    assert_eq!(
                        result.unwrap().value,
                        Some(Value::String(format!("nested_{}", i)))
                    );
                }
            })
            .collect::<Vec<_>>()
            .await;

        // Verify ancestor nodes were created with has_descendants=true
        let ancestor = btree
            .get_internal(&name, &Key::from(vec![1000.into()]))
            .await
            .unwrap();
        assert!(ancestor.is_some());
        assert_eq!(ancestor.as_ref().unwrap().value, None);
        assert!(ancestor.unwrap().has_descendants);

        // Verify deep ancestor chain for Pattern 3
        let deep_ancestor = btree
            .get_internal(&name, &Key::from(vec![2000.into()]))
            .await
            .unwrap();
        assert!(deep_ancestor.is_some());
        assert!(deep_ancestor.unwrap().has_descendants);

        // Verify tree statistics
        let stats = btree.stats().await;
        assert!(stats.node_count > 0);
        assert!(stats.key_count >= num_keys);
        assert!(stats.height > 0);
    }

    #[tokio::test]
    async fn test_find_node_not_found() {
        let btree = BTree::new(3).unwrap();
        let node_id = NodeId::from(42);

        let result = btree.find_node(node_id).await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, node_id);
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_find_node_exists() {
        let btree = BTree::new(3).unwrap();

        // Manually insert a node into the nodes HashMap
        let node_id = NodeId::from(1);
        let test_node = Node::new_leaf();

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, test_node.clone());
        }

        // Now find_node should succeed
        let result = btree.find_node(node_id).await;
        assert!(result.is_ok());
        let found_node = result.unwrap();

        // Verify we got the same node back
        assert_eq!(found_node.is_leaf, test_node.is_leaf);
        assert_eq!(found_node.keys.len(), test_node.keys.len());
    }

    #[tokio::test]
    async fn test_split_node_leaf_odd_keys() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create a leaf node with 5 keys (odd number)
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![
                Key::from(vec![10.into()]),
                Key::from(vec![20.into()]),
                Key::from(vec![30.into()]),
                Key::from(vec![40.into()]),
                Key::from(vec![50.into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(10))),
                Arc::new(NodeData::with_value(Value::Integer(20))),
                Arc::new(NodeData::with_value(Value::Integer(30))),
                Arc::new(NodeData::with_value(Value::Integer(40))),
                Arc::new(NodeData::with_value(Value::Integer(50))),
            ],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, node);
        }

        // Split the node
        let result = btree.split_node(node_id).await;
        assert!(result.is_ok());
        let (median_key, median_value, right_id) = result.unwrap();

        // Verify median key and value
        assert_eq!(median_key, Key::from(vec![30.into()]));
        assert_eq!(median_value.value, Some(Value::Integer(30)));

        // Verify left node (original)
        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.keys.len(), 2);
        assert_eq!(left.keys[0], Key::from(vec![10.into()]));
        assert_eq!(left.keys[1], Key::from(vec![20.into()]));
        assert!(left.is_leaf);

        // Verify right node
        let right = btree.find_node(right_id).await.unwrap();
        assert_eq!(right.keys.len(), 2);
        assert_eq!(right.keys[0], Key::from(vec![40.into()]));
        assert_eq!(right.keys[1], Key::from(vec![50.into()]));
        assert!(right.is_leaf);

        // Verify stats
        // Note: stats.node_count tracks new nodes created by operations,
        // not total nodes in the tree
        let stats = btree.stats().await;
        assert_eq!(stats.splits, 1);
        assert_eq!(stats.node_count, 1); // One new node created (right half)
    }

    #[tokio::test]
    async fn test_split_node_leaf_even_keys() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create a leaf node with 4 keys (even number)
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![
                Key::from(vec![10.into()]),
                Key::from(vec![20.into()]),
                Key::from(vec![30.into()]),
                Key::from(vec![40.into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(10))),
                Arc::new(NodeData::with_value(Value::Integer(20))),
                Arc::new(NodeData::with_value(Value::Integer(30))),
                Arc::new(NodeData::with_value(Value::Integer(40))),
            ],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, node);
        }

        // Split the node
        let result = btree.split_node(node_id).await;
        assert!(result.is_ok());
        let (median_key, median_value, right_id) = result.unwrap();

        // Verify median key and value (middle of 4 keys is index 2)
        assert_eq!(median_key, Key::from(vec![30.into()]));
        assert_eq!(median_value.value, Some(Value::Integer(30)));

        // Verify left node
        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.keys.len(), 2);
        assert_eq!(left.keys[0], Key::from(vec![10.into()]));
        assert_eq!(left.keys[1], Key::from(vec![20.into()]));

        // Verify right node
        let right = btree.find_node(right_id).await.unwrap();
        assert_eq!(right.keys.len(), 1);
        assert_eq!(right.keys[0], Key::from(vec![40.into()]));
    }

    #[tokio::test]
    async fn test_split_node_internal_with_children() {
        use rumps_types::{Key, NodeData};

        let btree = BTree::new(3).unwrap();

        // Create an internal node with 5 keys and 6 children
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![
                Key::from(vec![10.into()]),
                Key::from(vec![20.into()]),
                Key::from(vec![30.into()]),
                Key::from(vec![40.into()]),
                Key::from(vec![50.into()]),
            ],
            children: vec![
                NodeId::from(1),
                NodeId::from(2),
                NodeId::from(3),
                NodeId::from(4),
                NodeId::from(5),
                NodeId::from(6),
            ],
            values: vec![
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
            ],
            is_leaf: false,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, node);
        }

        // Split the node
        let result = btree.split_node(node_id).await;
        assert!(result.is_ok());
        let (median_key, median_value, right_id) = result.unwrap();

        // Verify median key and value (internal nodes have empty values)
        assert_eq!(median_key, Key::from(vec![30.into()]));
        assert_eq!(median_value, Arc::new(NodeData::empty()));

        // Verify left node has correct children
        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.keys.len(), 2);
        assert_eq!(left.children.len(), 3); // mid+1 children
        assert_eq!(left.children[0], NodeId::from(1));
        assert_eq!(left.children[1], NodeId::from(2));
        assert_eq!(left.children[2], NodeId::from(3));
        assert!(!left.is_leaf);

        // Verify right node has correct children
        let right = btree.find_node(right_id).await.unwrap();
        assert_eq!(right.keys.len(), 2);
        assert_eq!(right.children.len(), 3);
        assert_eq!(right.children[0], NodeId::from(4));
        assert_eq!(right.children[1], NodeId::from(5));
        assert_eq!(right.children[2], NodeId::from(6));
        assert!(!right.is_leaf);
    }

    #[tokio::test]
    async fn test_split_node_not_found() {
        let btree = BTree::new(3).unwrap();
        let node_id = NodeId::from(99);

        let result = btree.split_node(node_id).await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, node_id);
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_split_node_empty() {
        let btree = BTree::new(3).unwrap();

        // Create an empty node
        let node_id = NodeId::from(0);
        let node = Node::new_leaf();

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, node);
        }

        // Try to split - should fail
        let result = btree.split_node(node_id).await;
        assert!(result.is_err());
        match result {
            Err(StorageError::InvalidOperation(msg)) => {
                assert!(msg.contains("insufficient keys"));
            }
            _ => panic!("Expected InvalidOperation error"),
        }
    }

    #[tokio::test]
    async fn test_split_node_preserves_values() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create a node with different value types
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![
                Key::from(vec!["A".into()]),
                Key::from(vec!["B".into()]),
                Key::from(vec!["C".into()]),
                Key::from(vec!["D".into()]),
                Key::from(vec!["E".into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::String("Alpha".into()))),
                Arc::new(NodeData::with_value(Value::Integer(42))),
                Arc::new(NodeData::with_value(Value::Boolean(true))),
                Arc::new(NodeData::with_value(Value::Double(3.14.into()))),
                Arc::new(NodeData::with_value(Value::Char('X'))),
            ],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, node);
        }

        // Split the node
        let (median_key, median_value, right_id) =
            btree.split_node(node_id).await.unwrap();
        assert_eq!(median_key, Key::from(vec!["C".into()]));
        assert_eq!(median_value.value, Some(Value::Boolean(true)));

        // Verify left values
        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.values.len(), 2);
        assert_eq!(left.values[0].value, Some(Value::String("Alpha".into())));
        assert_eq!(left.values[1].value, Some(Value::Integer(42)));

        // Verify right values
        let right = btree.find_node(right_id).await.unwrap();
        assert_eq!(right.values.len(), 2);
        assert_eq!(right.values[0].value, Some(Value::Double(3.14.into())));
        assert_eq!(right.values[1].value, Some(Value::Char('X')));
    }

    #[tokio::test]
    async fn test_split_node_stats_update() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create two nodes and split both to verify stats accumulation
        // Use high node IDs to avoid conflicts with allocator
        let node1_id = NodeId::from(100);
        let node1 = Node {
            keys: vec![
                Key::from(vec![1.into()]),
                Key::from(vec![2.into()]),
                Key::from(vec![3.into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(1))),
                Arc::new(NodeData::with_value(Value::Integer(2))),
                Arc::new(NodeData::with_value(Value::Integer(3))),
            ],
            is_leaf: true,
        };

        let node2_id = NodeId::from(101);
        let node2 = Node {
            keys: vec![
                Key::from(vec![4.into()]),
                Key::from(vec![5.into()]),
                Key::from(vec![6.into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(4))),
                Arc::new(NodeData::with_value(Value::Integer(5))),
                Arc::new(NodeData::with_value(Value::Integer(6))),
            ],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node1_id, node1);
            nodes.insert(node2_id, node2);
        }

        // Initial stats
        let stats = btree.stats().await;
        assert_eq!(stats.splits, 0);
        assert_eq!(stats.node_count, 0); // Stats track differently from actual node count

        // Split first node
        btree.split_node(node1_id).await.unwrap();
        let stats = btree.stats().await;
        assert_eq!(stats.splits, 1);
        assert_eq!(stats.node_count, 1);

        // Split second node
        btree.split_node(node2_id).await.unwrap();
        let stats = btree.stats().await;
        assert_eq!(stats.splits, 2);
        assert_eq!(stats.node_count, 2);
    }

    #[tokio::test]
    async fn test_merge_nodes_leaf() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create two leaf nodes and a separator
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![Key::from(vec![10.into()]), Key::from(vec![20.into()])],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(10))),
                Arc::new(NodeData::with_value(Value::Integer(20))),
            ],
            is_leaf: true,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![Key::from(vec![40.into()]), Key::from(vec![50.into()])],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(40))),
                Arc::new(NodeData::with_value(Value::Integer(50))),
            ],
            is_leaf: true,
        };

        let separator_key = Key::from(vec![30.into()]);
        let separator_value =
            Arc::new(NodeData::with_value(Value::Integer(30)));

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left_id, left_node);
            nodes.insert(right_id, right_node);
        }

        // Merge the nodes
        let result = btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await;
        assert!(result.is_ok());

        // Verify merged node contains all keys in order
        let merged = btree.find_node(left_id).await.unwrap();
        assert_eq!(merged.keys.len(), 5);
        assert_eq!(merged.keys[0], Key::from(vec![10.into()]));
        assert_eq!(merged.keys[1], Key::from(vec![20.into()]));
        assert_eq!(merged.keys[2], Key::from(vec![30.into()]));
        assert_eq!(merged.keys[3], Key::from(vec![40.into()]));
        assert_eq!(merged.keys[4], Key::from(vec![50.into()]));
        assert!(merged.is_leaf);

        // Verify all values preserved
        assert_eq!(merged.values.len(), 5);
        assert_eq!(merged.values[0].value, Some(Value::Integer(10)));
        assert_eq!(merged.values[1].value, Some(Value::Integer(20)));
        assert_eq!(merged.values[2].value, Some(Value::Integer(30)));
        assert_eq!(merged.values[3].value, Some(Value::Integer(40)));
        assert_eq!(merged.values[4].value, Some(Value::Integer(50)));

        // Verify right node was removed
        let result = btree.find_node(right_id).await;
        assert!(result.is_err());

        // Verify stats
        let stats = btree.stats().await;
        assert_eq!(stats.merges, 1);
    }

    #[tokio::test]
    async fn test_merge_nodes_internal_with_children() {
        use rumps_types::{Key, NodeData};

        let btree = BTree::new(3).unwrap();

        // Create two internal nodes with children
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![Key::from(vec![10.into()]), Key::from(vec![20.into()])],
            children: vec![NodeId::from(1), NodeId::from(2), NodeId::from(3)],
            values: vec![
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
            ],
            is_leaf: false,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![Key::from(vec![40.into()]), Key::from(vec![50.into()])],
            children: vec![NodeId::from(4), NodeId::from(5), NodeId::from(6)],
            values: vec![
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
            ],
            is_leaf: false,
        };

        let separator_key = Key::from(vec![30.into()]);
        let separator_value = Arc::new(NodeData::empty());

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left_id, left_node);
            nodes.insert(right_id, right_node);
        }

        // Merge the nodes
        btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await
            .unwrap();

        // Verify merged node
        let merged = btree.find_node(left_id).await.unwrap();
        assert_eq!(merged.keys.len(), 5);
        assert_eq!(merged.children.len(), 6);
        assert!(!merged.is_leaf);

        // Verify children are merged correctly
        assert_eq!(merged.children[0], NodeId::from(1));
        assert_eq!(merged.children[1], NodeId::from(2));
        assert_eq!(merged.children[2], NodeId::from(3));
        assert_eq!(merged.children[3], NodeId::from(4));
        assert_eq!(merged.children[4], NodeId::from(5));
        assert_eq!(merged.children[5], NodeId::from(6));
    }

    #[tokio::test]
    async fn test_merge_nodes_incompatible_types() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create one leaf and one internal node
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![Key::from(vec![10.into()])],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![Key::from(vec![20.into()])],
            children: vec![NodeId::from(1), NodeId::from(2)],
            values: vec![Arc::new(NodeData::empty())],
            is_leaf: false,
        };

        let separator_key = Key::from(vec![15.into()]);
        let separator_value = Arc::new(NodeData::empty());

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left_id, left_node);
            nodes.insert(right_id, right_node);
        }

        // Try to merge - should fail
        let result = btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await;
        assert!(result.is_err());
        match result {
            Err(StorageError::InvalidOperation(msg)) => {
                assert!(msg.contains("Cannot merge leaf and internal nodes"));
            }
            _ => panic!("Expected InvalidOperation error"),
        }
    }

    #[tokio::test]
    async fn test_merge_nodes_left_not_found() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        let left_id = NodeId::from(100);
        let right_id = NodeId::from(101);

        // Only insert right node
        let right_node = Node {
            keys: vec![Key::from(vec![10.into()])],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(right_id, right_node);
        }

        let separator_key = Key::from(vec![5.into()]);
        let separator_value = Arc::new(NodeData::empty());

        // Try to merge - should fail
        let result = btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, left_id);
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_merge_nodes_right_not_found() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        let left_id = NodeId::from(100);
        let right_id = NodeId::from(101);

        // Only insert left node
        let left_node = Node {
            keys: vec![Key::from(vec![10.into()])],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left_id, left_node);
        }

        let separator_key = Key::from(vec![15.into()]);
        let separator_value = Arc::new(NodeData::empty());

        // Try to merge - should fail
        let result = btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, right_id);
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_merge_nodes_preserves_value_types() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create nodes with various value types
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![
                Key::from(vec!["A".into()]),
                Key::from(vec!["B".into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::String("Alpha".into()))),
                Arc::new(NodeData::with_value(Value::Integer(42))),
            ],
            is_leaf: true,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![
                Key::from(vec!["D".into()]),
                Key::from(vec!["E".into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Double(3.14.into()))),
                Arc::new(NodeData::with_value(Value::Char('X'))),
            ],
            is_leaf: true,
        };

        let separator_key = Key::from(vec!["C".into()]);
        let separator_value =
            Arc::new(NodeData::with_value(Value::Boolean(true)));

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left_id, left_node);
            nodes.insert(right_id, right_node);
        }

        // Merge nodes
        btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await
            .unwrap();

        // Verify all value types are preserved
        let merged = btree.find_node(left_id).await.unwrap();
        assert_eq!(merged.values.len(), 5);
        assert_eq!(merged.values[0].value, Some(Value::String("Alpha".into())));
        assert_eq!(merged.values[1].value, Some(Value::Integer(42)));
        assert_eq!(merged.values[2].value, Some(Value::Boolean(true)));
        assert_eq!(merged.values[3].value, Some(Value::Double(3.14.into())));
        assert_eq!(merged.values[4].value, Some(Value::Char('X')));
    }

    #[tokio::test]
    async fn test_merge_nodes_stats_update() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create multiple pairs of nodes to merge
        let left1_id = NodeId::from(100);
        let right1_id = NodeId::from(101);
        let left2_id = NodeId::from(102);
        let right2_id = NodeId::from(103);

        let node1_left = Node {
            keys: vec![Key::from(vec![1.into()])],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(1)))],
            is_leaf: true,
        };

        let node1_right = Node {
            keys: vec![Key::from(vec![3.into()])],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(3)))],
            is_leaf: true,
        };

        let node2_left = Node {
            keys: vec![Key::from(vec![10.into()])],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        let node2_right = Node {
            keys: vec![Key::from(vec![30.into()])],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(30)))],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left1_id, node1_left);
            nodes.insert(right1_id, node1_right);
            nodes.insert(left2_id, node2_left);
            nodes.insert(right2_id, node2_right);
        }

        // Initial stats
        let stats = btree.stats().await;
        assert_eq!(stats.merges, 0);

        // Merge first pair
        btree
            .merge_nodes(
                left1_id,
                Key::from(vec![2.into()]),
                Arc::new(NodeData::with_value(Value::Integer(2))),
                right1_id,
            )
            .await
            .unwrap();

        let stats = btree.stats().await;
        assert_eq!(stats.merges, 1);

        // Merge second pair
        btree
            .merge_nodes(
                left2_id,
                Key::from(vec![20.into()]),
                Arc::new(NodeData::with_value(Value::Integer(20))),
                right2_id,
            )
            .await
            .unwrap();

        let stats = btree.stats().await;
        assert_eq!(stats.merges, 2);
    }

    #[tokio::test]
    async fn test_split_and_merge_roundtrip() {
        use rumps_types::{Key, NodeData, Value};

        let btree = BTree::new(3).unwrap();

        // Create a node with 5 keys
        let original_id = NodeId::from(100);
        let original_node = Node {
            keys: vec![
                Key::from(vec![10.into()]),
                Key::from(vec![20.into()]),
                Key::from(vec![30.into()]),
                Key::from(vec![40.into()]),
                Key::from(vec![50.into()]),
            ],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(10))),
                Arc::new(NodeData::with_value(Value::Integer(20))),
                Arc::new(NodeData::with_value(Value::Integer(30))),
                Arc::new(NodeData::with_value(Value::Integer(40))),
                Arc::new(NodeData::with_value(Value::Integer(50))),
            ],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(original_id, original_node);
        }

        // Split the node
        let (median_key, median_value, right_id) =
            btree.split_node(original_id).await.unwrap();

        // Merge back
        btree
            .merge_nodes(original_id, median_key, median_value, right_id)
            .await
            .unwrap();

        // Verify we're back to original state
        let final_node = btree.find_node(original_id).await.unwrap();
        assert_eq!(final_node.keys.len(), 5);
        assert_eq!(final_node.keys[0], Key::from(vec![10.into()]));
        assert_eq!(final_node.keys[1], Key::from(vec![20.into()]));
        assert_eq!(final_node.keys[2], Key::from(vec![30.into()]));
        assert_eq!(final_node.keys[3], Key::from(vec![40.into()]));
        assert_eq!(final_node.keys[4], Key::from(vec![50.into()]));

        assert_eq!(final_node.values.len(), 5);
        assert_eq!(final_node.values[0].value, Some(Value::Integer(10)));
        assert_eq!(final_node.values[1].value, Some(Value::Integer(20)));
        assert_eq!(final_node.values[2].value, Some(Value::Integer(30)));
        assert_eq!(final_node.values[3].value, Some(Value::Integer(40)));
        assert_eq!(final_node.values[4].value, Some(Value::Integer(50)));
    }

    // Tests for get_internal() - Step 3 of hierarchy implementation

    #[tokio::test]
    async fn test_get_internal_nonexistent_variable() {
        use rumps_types::{Key, Name};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());
        let key = Key::from(vec![123.into()]);

        // Variable doesn't exist in roots
        let result = btree.get_internal(&name, &key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_internal_exact_match_single_key() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());
        let key = Key::from(vec![123.into()]);
        let value = Value::String("John Doe".into());

        // Insert using set_internal
        btree
            .set_internal(&name, &key, NodeData::with_value(value.clone()))
            .await
            .unwrap();

        // Retrieve using get_internal
        let result = btree.get_internal(&name, &key).await.unwrap();
        assert!(result.is_some());
        let arc_data = result.unwrap();
        assert_eq!(arc_data.value, Some(value));
        assert!(!arc_data.has_descendants);
    }

    #[tokio::test]
    async fn test_get_internal_exact_match_nested_key() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());
        let key = Key::from(vec![123.into(), "NAME".into()]);
        let value = Value::String("John Doe".into());

        // Insert using set_internal
        btree
            .set_internal(&name, &key, NodeData::with_value(value.clone()))
            .await
            .unwrap();

        // Retrieve using get_internal
        let result = btree.get_internal(&name, &key).await.unwrap();
        assert!(result.is_some());
        let arc_data = result.unwrap();
        assert_eq!(arc_data.value, Some(value));
    }

    #[tokio::test]
    async fn test_get_internal_nonexistent_key() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());

        // Insert some keys
        btree
            .set_internal(
                &name,
                &Key::from(vec![100.into()]),
                NodeData::with_value(Value::Integer(1)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![200.into()]),
                NodeData::with_value(Value::Integer(2)),
            )
            .await
            .unwrap();

        // Search for key between existing keys
        let result = btree
            .get_internal(&name, &Key::from(vec![150.into()]))
            .await
            .unwrap();
        assert!(result.is_none());

        // Search for key before all existing keys
        let result = btree
            .get_internal(&name, &Key::from(vec![50.into()]))
            .await
            .unwrap();
        assert!(result.is_none());

        // Search for key after all existing keys
        let result = btree
            .get_internal(&name, &Key::from(vec![300.into()]))
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_internal_partial_match_no_such_path() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());

        // Insert keys: [1], [1,2,5]
        btree
            .set_internal(
                &name,
                &Key::from(vec![1.into()]),
                NodeData::with_value(Value::Integer(1)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![1.into(), 2.into(), 5.into()]),
                NodeData::with_value(Value::Integer(125)),
            )
            .await
            .unwrap();

        // Search for [1,2,3] - partial match with [1] but not exact
        // Should return None because [1,2,3] doesn't exist
        let result = btree
            .get_internal(&name, &Key::from(vec![1.into(), 2.into(), 3.into()]))
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_internal_multiple_keys_same_variable() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // Insert multiple keys
        btree
            .set_internal(
                &name,
                &Key::from(vec![1.into()]),
                NodeData::with_value(Value::Integer(1)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![2.into()]),
                NodeData::with_value(Value::Integer(2)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![3.into()]),
                NodeData::with_value(Value::Integer(3)),
            )
            .await
            .unwrap();

        // Retrieve all keys
        let result1 = btree
            .get_internal(&name, &Key::from(vec![1.into()]))
            .await
            .unwrap()
            .unwrap();
        let result2 = btree
            .get_internal(&name, &Key::from(vec![2.into()]))
            .await
            .unwrap()
            .unwrap();
        let result3 = btree
            .get_internal(&name, &Key::from(vec![3.into()]))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result1.value, Some(Value::Integer(1)));
        assert_eq!(result2.value, Some(Value::Integer(2)));
        assert_eq!(result3.value, Some(Value::Integer(3)));
    }

    #[tokio::test]
    async fn test_get_internal_different_variables() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name1 = Name::Global("VAR1".into());
        let name2 = Name::Global("VAR2".into());
        let key = Key::from(vec![123.into()]);

        // Insert same key in different variables
        btree
            .set_internal(
                &name1,
                &key,
                NodeData::with_value(Value::String("VAR1 value".into())),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name2,
                &key,
                NodeData::with_value(Value::String("VAR2 value".into())),
            )
            .await
            .unwrap();

        // Retrieve from both variables
        let result1 = btree.get_internal(&name1, &key).await.unwrap().unwrap();
        let result2 = btree.get_internal(&name2, &key).await.unwrap().unwrap();

        assert_eq!(result1.value, Some(Value::String("VAR1 value".into())));
        assert_eq!(result2.value, Some(Value::String("VAR2 value".into())));
    }

    #[tokio::test]
    async fn test_get_internal_returns_nodedata_with_flags() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());
        let key = Key::from(vec![1.into()]);

        btree
            .set_internal(&name, &key, NodeData::with_value(Value::Integer(42)))
            .await
            .unwrap();

        // Get the NodeData
        let arc1 = btree.get_internal(&name, &key).await.unwrap().unwrap();
        let arc2 = btree.get_internal(&name, &key).await.unwrap().unwrap();

        // Both calls should return NodeData with identical content
        assert_eq!(arc1.value, arc2.value);
        assert_eq!(arc1.has_descendants, arc2.has_descendants);
        assert_eq!(arc1.value, Some(Value::Integer(42)));
        assert!(!arc1.has_descendants);
    }

    #[tokio::test]
    async fn test_get_internal_with_tree_splits() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap(); // min_degree=3, max_keys=5
        let name = Name::Global("VAR".into());

        // Insert enough keys to cause splits
        btree
            .set_internal(
                &name,
                &Key::from(vec![10.into()]),
                NodeData::with_value(Value::Integer(10)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![20.into()]),
                NodeData::with_value(Value::Integer(20)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![30.into()]),
                NodeData::with_value(Value::Integer(30)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![40.into()]),
                NodeData::with_value(Value::Integer(40)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![50.into()]),
                NodeData::with_value(Value::Integer(50)),
            )
            .await
            .unwrap();
        btree
            .set_internal(
                &name,
                &Key::from(vec![60.into()]),
                NodeData::with_value(Value::Integer(60)),
            )
            .await
            .unwrap();

        // Retrieve all keys after splits
        let result30 = btree
            .get_internal(&name, &Key::from(vec![30.into()]))
            .await
            .unwrap()
            .unwrap();
        let result60 = btree
            .get_internal(&name, &Key::from(vec![60.into()]))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result30.value, Some(Value::Integer(30)));
        assert_eq!(result60.value, Some(Value::Integer(60)));
    }

    #[tokio::test]
    async fn test_get_internal_deep_nesting() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());

        // Insert deeply nested key
        let key = Key::from(vec![
            123.into(),
            "DEMOGRAPHICS".into(),
            "ADDRESS".into(),
            "STREET".into(),
        ]);
        let value = Value::String("123 Main St".into());

        btree
            .set_internal(&name, &key, NodeData::with_value(value.clone()))
            .await
            .unwrap();

        // Retrieve deeply nested key
        let result = btree.get_internal(&name, &key).await.unwrap().unwrap();
        assert_eq!(result.value, Some(value));
    }

    // Tests for hierarchical semantics - Step 4 implementation

    #[tokio::test]
    async fn test_set_creates_ancestors() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());

        // Set a nested key
        let key = Key::from(vec![123.into(), "NAME".into()]);
        btree.ensure_ancestors(&name, &key).await.unwrap();
        btree
            .set_internal(
                &name,
                &key,
                NodeData::with_value(Value::String("John".into())),
            )
            .await
            .unwrap();

        // Verify ancestor was created
        let ancestor_key = Key::from(vec![123.into()]);
        let ancestor_arc =
            btree.get_internal(&name, &ancestor_key).await.unwrap();

        assert!(ancestor_arc.is_some());
        let data = ancestor_arc.unwrap();
        assert!(data.value.is_none()); // No value on ancestor
        assert!(data.has_descendants); // But has descendants
    }

    #[tokio::test]
    async fn test_set_deep_nesting_creates_all_ancestors() {
        use futures::stream::StreamExt;
        use rumps_types::{Key, Name, Value};

        let btree = Arc::new(BTree::new(3).unwrap());
        let name = Name::Global("VAR".into());

        // Set deeply nested key
        let key =
            Key::from(vec![1.into(), 2.into(), 3.into(), 4.into(), 5.into()]);
        btree.ensure_ancestors(&name, &key).await.unwrap();
        btree
            .set_internal(&name, &key, NodeData::with_value(Value::Integer(42)))
            .await
            .unwrap();

        // Verify all 4 ancestors have has_descendants=true
        let ancestors = key.ancestors();
        assert_eq!(ancestors.len(), 4);

        futures::stream::iter(ancestors)
            .for_each(|ancestor_key| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    let data = btree
                        .get_internal(&name, &ancestor_key)
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(data.has_descendants);
                    assert!(data.value.is_none()); // Intermediate nodes have no value
                }
            })
            .await;
    }

    #[tokio::test]
    async fn test_set_intermediate_node_becomes_both() {
        use rumps_types::{Key, Name, Value};

        // CRITICAL EDGE CASE
        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // 1. Set ^VAR(1,"A") = "child1"
        //    → Creates ^VAR(1) with has_descendants=true, no value
        let key_child = Key::from(vec![1.into(), "A".into()]);
        btree.ensure_ancestors(&name, &key_child).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_child,
                NodeData::with_value(Value::String("child1".into())),
            )
            .await
            .unwrap();

        // Verify ancestor exists
        let key_parent = Key::from(vec![1.into()]);
        let arc = btree
            .get_internal(&name, &key_parent)
            .await
            .unwrap()
            .unwrap();
        assert!(arc.value.is_none());
        assert!(arc.has_descendants);

        // 2. Set ^VAR(1) = "parent_value"
        //    → Must preserve has_descendants=true AND add value
        btree.ensure_ancestors(&name, &key_parent).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_parent,
                NodeData::with_value(Value::String("parent_value".into())),
            )
            .await
            .unwrap();

        // Verify ^VAR(1) has both value and has_descendants=true
        let arc = btree
            .get_internal(&name, &key_parent)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(arc.value, Some(Value::String("parent_value".into())));
        assert!(arc.has_descendants);
    }

    #[tokio::test]
    async fn test_set_preserves_has_descendants_on_update() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        let key_parent = Key::from(vec![1.into()]);
        let key_child = Key::from(vec![1.into(), 2.into()]);

        // 1. Set ^VAR(1) = "first"
        btree.ensure_ancestors(&name, &key_parent).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_parent,
                NodeData::with_value(Value::String("first".into())),
            )
            .await
            .unwrap();

        // 2. Set ^VAR(1,2) = "child" → ^VAR(1).has_descendants becomes true
        btree.ensure_ancestors(&name, &key_child).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_child,
                NodeData::with_value(Value::String("child".into())),
            )
            .await
            .unwrap();

        // Verify flag was set
        let arc = btree
            .get_internal(&name, &key_parent)
            .await
            .unwrap()
            .unwrap();
        assert!(arc.has_descendants);

        // 3. Set ^VAR(1) = "updated"
        btree.ensure_ancestors(&name, &key_parent).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_parent,
                NodeData::with_value(Value::String("updated".into())),
            )
            .await
            .unwrap();

        // Verify ^VAR(1) still has has_descendants=true after update
        let arc = btree
            .get_internal(&name, &key_parent)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(arc.value, Some(Value::String("updated".into())));
        assert!(arc.has_descendants); // MUST still be true
    }

    #[tokio::test]
    async fn test_set_multiple_children_same_parent() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // Set ^VAR(1,"A"), ^VAR(1,"B"), ^VAR(1,"C")
        let key_a = Key::from(vec![1.into(), "A".into()]);
        btree.ensure_ancestors(&name, &key_a).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_a,
                NodeData::with_value(Value::Integer(1)),
            )
            .await
            .unwrap();
        let key_b = Key::from(vec![1.into(), "B".into()]);
        btree.ensure_ancestors(&name, &key_b).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_b,
                NodeData::with_value(Value::Integer(2)),
            )
            .await
            .unwrap();
        let key_c = Key::from(vec![1.into(), "C".into()]);
        btree.ensure_ancestors(&name, &key_c).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_c,
                NodeData::with_value(Value::Integer(3)),
            )
            .await
            .unwrap();

        // Verify ^VAR(1) has has_descendants=true
        let arc = btree
            .get_internal(&name, &Key::from(vec![1.into()]))
            .await
            .unwrap()
            .unwrap();
        assert!(arc.has_descendants);
    }

    #[tokio::test]
    async fn test_set_sibling_paths() {
        use rumps_types::{Key, Name, Value};

        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // Set ^VAR(1,2), ^VAR(1,3), ^VAR(2,2)
        let key_12 = Key::from(vec![1.into(), 2.into()]);
        btree.ensure_ancestors(&name, &key_12).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_12,
                NodeData::with_value(Value::Integer(12)),
            )
            .await
            .unwrap();
        let key_13 = Key::from(vec![1.into(), 3.into()]);
        btree.ensure_ancestors(&name, &key_13).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_13,
                NodeData::with_value(Value::Integer(13)),
            )
            .await
            .unwrap();
        let key_22 = Key::from(vec![2.into(), 2.into()]);
        btree.ensure_ancestors(&name, &key_22).await.unwrap();
        btree
            .set_internal(
                &name,
                &key_22,
                NodeData::with_value(Value::Integer(22)),
            )
            .await
            .unwrap();

        // Verify ^VAR(1) and ^VAR(2) both have has_descendants
        let arc1 = btree
            .get_internal(&name, &Key::from(vec![1.into()]))
            .await
            .unwrap()
            .unwrap();
        let arc2 = btree
            .get_internal(&name, &Key::from(vec![2.into()]))
            .await
            .unwrap()
            .unwrap();
        assert!(arc1.has_descendants);
        assert!(arc2.has_descendants);
    }

    #[tokio::test]
    async fn test_concurrent_ancestor_creation() {
        use futures::future::join_all;
        use rumps_types::{Key, Name, Value};

        let btree = Arc::new(BTree::new(3).unwrap());
        let name = Name::Global("VAR".into());

        // Spawn multiple tasks creating children of same parent concurrently
        let tasks = (0..10).map(|i| {
            let btree = Arc::clone(&btree);
            let name = name.clone();
            tokio::spawn(async move {
                let key = Key::from(vec![1.into(), i.into()]);
                btree.ensure_ancestors(&name, &key).await.unwrap();
                btree
                    .set_internal(
                        &name,
                        &key,
                        NodeData::with_value(Value::Integer(i)),
                    )
                    .await
            })
        });

        // Wait for all to complete
        let results: Vec<_> = join_all(tasks).await;
        results.into_iter().for_each(|result| {
            result.unwrap().unwrap();
        });

        // Verify parent was created exactly once with has_descendants=true
        let arc = btree
            .get_internal(&name, &Key::from(vec![1.into()]))
            .await
            .unwrap()
            .unwrap();
        assert!(arc.has_descendants);
        assert!(arc.value.is_none());
    }
}

#[cfg(feature = "bench")]
pub mod benches {
    //! Benchmark suite for BTree operations.
    //!
    //! This module contains criterion benchmarks for hierarchical operations,
    //! demonstrating performance characteristics across various tree depths.

    use criterion::{BenchmarkId, Criterion};
    use futures::StreamExt;
    use tokio::runtime::Runtime;

    use super::BTree;
    use rumps_types::{Key, Name, NodeData, Value};

    /// Helper function to create a key with the specified depth.
    ///
    /// For depth=3, creates Key([1, 2, 3])
    /// This will result in (depth - 1) ancestors being created.
    fn create_key_at_depth(depth: usize) -> Key {
        Key::from((1..=depth).map(|i| (i as i64).into()).collect::<Vec<_>>())
    }

    /// Benchmark INSERT operations at various depths to show hierarchical semantics performance characteristics.
    ///
    /// This benchmarks the `set()` operation which internally calls `ensure_ancestors()` before insertion,
    /// demonstrating the time complexity as depth increases.
    fn bench_insert_depths(c: &mut Criterion) {
        let mut group = c.benchmark_group("insert_by_depth");
        let rt = Runtime::new().unwrap();

        [2, 3, 4, 5, 10].iter().copied().for_each(|depth| {
            group.bench_with_input(
                BenchmarkId::from_parameter(depth),
                &depth,
                |b, &depth| {
                    b.iter(|| {
                        rt.block_on(async {
                            let btree = BTree::new(3).unwrap();
                            let name = Name::Global("VAR".into());
                            let key = create_key_at_depth(depth);
                            let value = Value::Integer(42);

                            // set_internal includes ensure_ancestors() + insertion
                            btree
                                .set_internal(
                                    &name,
                                    &key,
                                    NodeData::with_value(value),
                                )
                                .await
                                .unwrap();
                        })
                    });
                },
            );
        });

        group.finish();
    }

    /// Benchmark INSERT with existing ancestors to show amortization benefits.
    ///
    /// This tests the scenario where ancestors already exist, which should be faster
    /// than creating them from scratch.
    fn bench_insert_with_existing_ancestors(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();

        c.bench_function("depth_5_with_existing_ancestors", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let btree = BTree::new(3).unwrap();
                    let name = Name::Global("VAR".into());

                    // First insert creates all ancestors
                    let key1 = Key::from(vec![
                        1.into(),
                        2.into(),
                        3.into(),
                        4.into(),
                        100.into(),
                    ]);
                    btree
                        .set_internal(
                            &name,
                            &key1,
                            NodeData::with_value(Value::Integer(42)),
                        )
                        .await
                        .unwrap();

                    // Second insert at same depth should be faster (ancestors exist)
                    let key2 = Key::from(vec![
                        1.into(),
                        2.into(),
                        3.into(),
                        4.into(),
                        200.into(),
                    ]);
                    btree
                        .set_internal(
                            &name,
                            &key2,
                            NodeData::with_value(Value::Integer(43)),
                        )
                        .await
                        .unwrap();
                })
            });
        });
    }

    /// Benchmark ancestor creation in isolation.
    ///
    /// This directly measures ancestor creation performance by repeatedly
    /// creating ancestors without the final key insertion.
    fn bench_ensure_ancestors(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();

        c.bench_function("ensure_ancestors_depth_5", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let btree = BTree::new(3).unwrap();
                    let name = Name::Global("VAR".into());
                    let key = create_key_at_depth(5);

                    // Create all ancestors
                    let ancestors = key.ancestors();
                    futures::stream::iter(ancestors)
                        .for_each(|ancestor| {
                            let btree = &btree;
                            let name = name.clone();
                            async move {
                                btree
                                    .set_internal(
                                        &name,
                                        &ancestor,
                                        NodeData::with_value(Value::String(
                                            "ancestor".into(),
                                        )),
                                    )
                                    .await
                                    .unwrap();
                            }
                        })
                        .await;
                })
            });
        });
    }

    /// Benchmark worst case: deep nesting with completely fresh tree.
    fn bench_worst_case(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();

        c.bench_function("worst_case_depth_10_fresh_tree", |b| {
            b.iter(|| {
                rt.block_on(async {
                    // Fresh tree for each iteration
                    let btree = BTree::new(3).unwrap();
                    let name = Name::Global("DEEP".into());
                    let key = create_key_at_depth(10);

                    btree
                        .set_internal(
                            &name,
                            &key,
                            NodeData::with_value(Value::String("value".into())),
                        )
                        .await
                        .unwrap();
                })
            });
        });
    }

    /// Benchmark best case: shallow nesting (depth 2).
    fn bench_best_case(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();

        c.bench_function("best_case_depth_2_fresh_tree", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let btree = BTree::new(3).unwrap();
                    let name = Name::Global("SHALLOW".into());
                    let key = create_key_at_depth(2);

                    btree
                        .set_internal(
                            &name,
                            &key,
                            NodeData::with_value(Value::String("value".into())),
                        )
                        .await
                        .unwrap();
                })
            });
        });
    }

    /// Main entry point for all benchmarks.
    ///
    /// This function is called from the benchmark wrapper in `benches/btree_bench.rs`.
    pub fn run_benchmarks(c: &mut Criterion) {
        bench_insert_depths(c);
        bench_insert_with_existing_ancestors(c);
        bench_ensure_ancestors(c);
        bench_worst_case(c);
        bench_best_case(c);
    }
}
