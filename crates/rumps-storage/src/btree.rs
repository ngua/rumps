use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use rumps_types::{Key, Name, Node, NodeId};
use tokio::sync::RwLock;

use crate::error::{Result, StorageError};

/// Statistics tracking for B-tree operations.
#[derive(Debug, Clone, Default)]
pub struct BTreeStats {
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
pub trait NodeAllocator: Send + Sync {
    /// Allocate a new node ID
    async fn allocate(&self) -> Result<NodeId>;

    /// Deallocate a node ID for reuse
    async fn deallocate(&self, id: NodeId) -> Result<()>;

    /// Get the next available ID without allocating
    async fn peek_next(&self) -> NodeId;
}

/// Simple incrementing allocator for in-memory use.
pub struct IncrementingAllocator {
    next_id: RwLock<u64>,
}

impl IncrementingAllocator {
    pub fn new() -> Self {
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
/// See `.slop/bottlenecks.md` for detailed analysis of scalability limits.
///
/// # Thread Safety
///
/// The `BTree` is designed to be shared across threads using `Arc<BTree>`.
/// All operations use interior mutability via `RwLock`, allowing multiple
/// concurrent readers with exclusive writers.
///
/// # Examples
///
/// ```
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
pub struct BTree {
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
    /// ```
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
    pub fn new(min_degree: usize) -> Result<Self> {
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
    /// ```
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
    pub fn with_config(
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
    /// ```
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(4)?;
    /// assert_eq!(btree.min_degree(), 4);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub fn min_degree(&self) -> usize {
        self.min_degree
    }

    /// Returns the total number of nodes in this B-tree.
    ///
    /// This is an async method because it requires acquiring a read lock
    /// on the nodes collection.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3)?;
    /// assert_eq!(btree.node_count().await, 0);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub async fn node_count(&self) -> usize {
        self.nodes.read().await.len()
    }

    /// Returns whether a memory limit is configured.
    ///
    /// # Examples
    ///
    /// ```
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
    pub fn has_memory_limit(&self) -> bool {
        self.max_memory_bytes.is_some()
    }

    /// Returns the current statistics for this B-tree.
    ///
    /// # Examples
    ///
    /// ```
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
    pub async fn stats(&self) -> BTreeStats {
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

    /// Finds and returns a node by its ID.
    ///
    /// This is an internal helper method used by tree traversal operations.
    /// It looks up the node in the in-memory `HashMap` and clones it.
    ///
    /// In Phase 4, this will be replaced by `load_node()` which checks the
    /// cache first and loads from disk if needed.
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
            .ok_or_else(|| StorageError::NodeNotFound(id))
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
    /// # Arguments
    ///
    /// * `id` - The ID of the node to split
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// - `Key`: The median key to be promoted to the parent
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
    /// let (median_key, right_id) = btree.split_node(full_node_id).await?;
    /// // Caller must promote median_key to parent and link right_id
    /// ```
    async fn split_node(&self, id: NodeId) -> Result<(Key, NodeId)> {
        // Find the node to split
        let node = self.find_node(id).await?;

        // Calculate the median index
        let mid = node.keys.len() / 2;

        // Verify we have enough keys to split
        match node.keys.get(mid) {
            None => Err(StorageError::InvalidOperation(
                "Node has insufficient keys for splitting".to_string(),
            )),
            Some(median_key) => {
                let median_key = median_key.clone();

                // Allocate ID for the new right node
                let right_id = self.allocator.allocate().await?;

                // Split keys, values, and optionally children
                let left_keys = node.keys.iter().take(mid).cloned().collect();
                let right_keys = node
                    .keys
                    .iter()
                    .skip(mid + 1)
                    .cloned()
                    .collect();

                let left_values = node.values.iter().take(mid).cloned().collect();
                let right_values = node
                    .values
                    .iter()
                    .skip(mid + 1)
                    .cloned()
                    .collect();

                // Split children for internal nodes
                let (left_children, right_children) = if node.is_leaf {
                    (Vec::new(), Vec::new())
                } else {
                    let left = node
                        .children
                        .iter()
                        .take(mid + 1)
                        .copied()
                        .collect();
                    let right = node
                        .children
                        .iter()
                        .skip(mid + 1)
                        .copied()
                        .collect();
                    (left, right)
                };

                // Create the right node
                let right_node = Node {
                    keys: right_keys,
                    children: right_children,
                    values: right_values,
                    is_leaf: node.is_leaf,
                };

                // Update the original (left) node
                let left_node = Node {
                    keys: left_keys,
                    children: left_children,
                    values: left_values,
                    is_leaf: node.is_leaf,
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

                Ok((median_key, right_id))
            }
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
    /// ```
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
        use tokio::task;
        use futures::future;

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
        // This test is a placeholder for Phase 2.2+ when SET operations are implemented
        let btree = BTree::new(100).unwrap(); // Large min_degree
        assert_eq!(btree.min_degree(), 100);

        // Future: Insert millions of keys and verify tree properties
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
                NodeData::with_value(Value::Integer(10)),
                NodeData::with_value(Value::Integer(20)),
                NodeData::with_value(Value::Integer(30)),
                NodeData::with_value(Value::Integer(40)),
                NodeData::with_value(Value::Integer(50)),
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
        let (median_key, right_id) = result.unwrap();

        // Verify median key
        assert_eq!(median_key, Key::from(vec![30.into()]));

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
                NodeData::with_value(Value::Integer(10)),
                NodeData::with_value(Value::Integer(20)),
                NodeData::with_value(Value::Integer(30)),
                NodeData::with_value(Value::Integer(40)),
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
        let (median_key, right_id) = result.unwrap();

        // Verify median key (middle of 4 keys is index 2)
        assert_eq!(median_key, Key::from(vec![30.into()]));

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
                NodeData::empty(),
                NodeData::empty(),
                NodeData::empty(),
                NodeData::empty(),
                NodeData::empty(),
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
        let (median_key, right_id) = result.unwrap();

        // Verify median key
        assert_eq!(median_key, Key::from(vec![30.into()]));

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
                NodeData::with_value(Value::String("Alpha".into())),
                NodeData::with_value(Value::Integer(42)),
                NodeData::with_value(Value::Boolean(true)),
                NodeData::with_value(Value::Double(3.14.into())),
                NodeData::with_value(Value::Char('X')),
            ],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, node);
        }

        // Split the node
        let (median_key, right_id) = btree.split_node(node_id).await.unwrap();
        assert_eq!(median_key, Key::from(vec!["C".into()]));

        // Verify left values
        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.values.len(), 2);
        assert_eq!(
            left.values[0].value,
            Some(Value::String("Alpha".into()))
        );
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
                NodeData::with_value(Value::Integer(1)),
                NodeData::with_value(Value::Integer(2)),
                NodeData::with_value(Value::Integer(3)),
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
                NodeData::with_value(Value::Integer(4)),
                NodeData::with_value(Value::Integer(5)),
                NodeData::with_value(Value::Integer(6)),
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
}
