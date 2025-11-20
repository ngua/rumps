use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use rumps_types::{Name, Node, NodeId};
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
                match stats.memory_bytes > limit {
                    true => Err(StorageError::MemoryLimitExceeded {
                        used: stats.memory_bytes,
                        limit,
                    }),
                    false => Ok(()),
                }
            }
            None => Ok(()),
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
}
