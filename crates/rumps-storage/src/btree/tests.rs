#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::{future, StreamExt};
    use rumps_types::{global, key, local, Value};
    use tokio::task;
    use tokio::time::{sleep, Duration};

    use crate::btree::*;
    use crate::error::StorageError;
    use crate::node::NodeData;

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
        let btree = BTree::default();
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
        let btree = BTree::default();
        let stats = btree.stats().await;
        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.key_count, 0);
        assert_eq!(stats.height, 0);
        assert_eq!(stats.splits, 0);
        assert_eq!(stats.merges, 0);
    }

    #[tokio::test]
    async fn test_concurrent_readers() {
        let btree = Arc::new(BTree::default());

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
        let btree = Arc::new(BTree::default());

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
        let btree = Arc::new(BTree::default());
        let name = global!("STRESS");

        // Insert 1000 keys with various patterns
        let num_keys: usize = 1000;

        // Pattern 1: Sequential integers at depth 1
        futures::stream::iter(0..num_keys / 2)
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    let k = key![i as i64];
                    btree
                        .set_internal(
                            &name,
                            &k,
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
                    let k = key![1000, i as i64];
                    btree
                        .set_internal(
                            &name,
                            &k,
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
                    let k = key![
                        2000,
                        (i % 10) as i64,
                        (i % 5) as i64,
                        (i % 3) as i64,
                        i as i64,
                    ];
                    btree
                        .set_internal(
                            &name,
                            &k,
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
                    let k = key![i as i64];
                    let result = btree.get_internal(&name, &k).await.unwrap();
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
                    let k = key![1000, i as i64];
                    let result = btree.get_internal(&name, &k).await.unwrap();
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
        let ancestor = btree.get_internal(&name, &key![1000]).await.unwrap();
        assert!(ancestor.is_some());
        assert_eq!(ancestor.as_ref().unwrap().value, None);
        assert!(ancestor.unwrap().has_descendants);

        // Verify deep ancestor chain for Pattern 3
        let deep_ancestor =
            btree.get_internal(&name, &key![2000]).await.unwrap();
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
        let btree = BTree::default();
        let node_id = NodeId::from(42);

        let result = btree.find_node(node_id).await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, u64::from(node_id));
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_find_node_exists() {
        let btree = BTree::default();

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
        let btree = BTree::default();

        // Create a leaf node with 5 keys (odd number)
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![key![10], key![20], key![30], key![40], key![50]],
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
        assert_eq!(median_key, key![30]);
        assert_eq!(median_value.value, Some(Value::Integer(30)));

        // Verify left node (original)
        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.keys.len(), 2);
        assert_eq!(left.keys[0], key![10]);
        assert_eq!(left.keys[1], key![20]);
        assert!(left.is_leaf);

        // Verify right node
        let right = btree.find_node(right_id).await.unwrap();
        assert_eq!(right.keys.len(), 2);
        assert_eq!(right.keys[0], key![40]);
        assert_eq!(right.keys[1], key![50]);
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
        let btree = BTree::default();

        // Create a leaf node with 4 keys (even number)
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![key![10], key![20], key![30], key![40]],
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
        assert_eq!(median_key, key![30]);
        assert_eq!(median_value.value, Some(Value::Integer(30)));

        // Verify left node
        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.keys.len(), 2);
        assert_eq!(left.keys[0], key![10]);
        assert_eq!(left.keys[1], key![20]);

        // Verify right node
        let right = btree.find_node(right_id).await.unwrap();
        assert_eq!(right.keys.len(), 1);
        assert_eq!(right.keys[0], key![40]);
    }

    #[tokio::test]
    async fn test_split_node_internal_with_children() {
        let btree = BTree::default();

        // Create an internal node with 5 keys and 6 children
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![key![10], key![20], key![30], key![40], key![50]],
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
        assert_eq!(median_key, key![30]);
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
        let btree = BTree::default();
        let node_id = NodeId::from(99);

        let result = btree.split_node(node_id).await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, u64::from(node_id));
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_split_node_empty() {
        let btree = BTree::default();

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
        let btree = BTree::default();

        // Create a node with different value types
        // Use high node ID to avoid conflicts with allocator
        let node_id = NodeId::from(100);
        let node = Node {
            keys: vec![key!["A"], key!["B"], key!["C"], key!["D"], key!["E"]],
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
        assert_eq!(median_key, key!["C"]);
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
        let btree = BTree::default();

        // Create two nodes and split both to verify stats accumulation
        // Use high node IDs to avoid conflicts with allocator
        let node1_id = NodeId::from(100);
        let node1 = Node {
            keys: vec![key![1], key![2], key![3]],
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
            keys: vec![key![4], key![5], key![6]],
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
        let btree = BTree::default();

        // Create two leaf nodes and a separator
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![key![10], key![20]],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(10))),
                Arc::new(NodeData::with_value(Value::Integer(20))),
            ],
            is_leaf: true,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![key![40], key![50]],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(40))),
                Arc::new(NodeData::with_value(Value::Integer(50))),
            ],
            is_leaf: true,
        };

        let separator_key = key![30];
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
        assert_eq!(merged.keys[0], key![10]);
        assert_eq!(merged.keys[1], key![20]);
        assert_eq!(merged.keys[2], key![30]);
        assert_eq!(merged.keys[3], key![40]);
        assert_eq!(merged.keys[4], key![50]);
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
        let btree = BTree::default();

        // Create two internal nodes with children
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![key![10], key![20]],
            children: vec![NodeId::from(1), NodeId::from(2), NodeId::from(3)],
            values: vec![
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
            ],
            is_leaf: false,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![key![40], key![50]],
            children: vec![NodeId::from(4), NodeId::from(5), NodeId::from(6)],
            values: vec![
                Arc::new(NodeData::empty()),
                Arc::new(NodeData::empty()),
            ],
            is_leaf: false,
        };

        let separator_key = key![30];
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
        let btree = BTree::default();

        // Create one leaf and one internal node
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![key![10]],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![key![20]],
            children: vec![NodeId::from(1), NodeId::from(2)],
            values: vec![Arc::new(NodeData::empty())],
            is_leaf: false,
        };

        let separator_key = key![15];
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
        let btree = BTree::default();

        let left_id = NodeId::from(100);
        let right_id = NodeId::from(101);

        // Only insert right node
        let right_node = Node {
            keys: vec![key![10]],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(right_id, right_node);
        }

        let separator_key = key![5];
        let separator_value = Arc::new(NodeData::empty());

        // Try to merge - should fail
        let result = btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, u64::from(left_id));
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_merge_nodes_right_not_found() {
        let btree = BTree::default();

        let left_id = NodeId::from(100);
        let right_id = NodeId::from(101);

        // Only insert left node
        let left_node = Node {
            keys: vec![key![10]],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left_id, left_node);
        }

        let separator_key = key![15];
        let separator_value = Arc::new(NodeData::empty());

        // Try to merge - should fail
        let result = btree
            .merge_nodes(left_id, separator_key, separator_value, right_id)
            .await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NodeNotFound(id)) => {
                assert_eq!(id, u64::from(right_id));
            }
            _ => panic!("Expected NodeNotFound error"),
        }
    }

    #[tokio::test]
    async fn test_merge_nodes_preserves_value_types() {
        let btree = BTree::default();

        // Create nodes with various value types
        let left_id = NodeId::from(100);
        let left_node = Node {
            keys: vec![key!["A"], key!["B"]],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::String("Alpha".into()))),
                Arc::new(NodeData::with_value(Value::Integer(42))),
            ],
            is_leaf: true,
        };

        let right_id = NodeId::from(101);
        let right_node = Node {
            keys: vec![key!["D"], key!["E"]],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Double(3.14.into()))),
                Arc::new(NodeData::with_value(Value::Char('X'))),
            ],
            is_leaf: true,
        };

        let separator_key = key!["C"];
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
        let btree = BTree::default();

        // Create multiple pairs of nodes to merge
        let left1_id = NodeId::from(100);
        let right1_id = NodeId::from(101);
        let left2_id = NodeId::from(102);
        let right2_id = NodeId::from(103);

        let node1_left = Node {
            keys: vec![key![1]],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(1)))],
            is_leaf: true,
        };

        let node1_right = Node {
            keys: vec![key![3]],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(3)))],
            is_leaf: true,
        };

        let node2_left = Node {
            keys: vec![key![10]],
            children: vec![],
            values: vec![Arc::new(NodeData::with_value(Value::Integer(10)))],
            is_leaf: true,
        };

        let node2_right = Node {
            keys: vec![key![30]],
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
                key![2],
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
                key![20],
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
        let btree = BTree::default();

        // Create a node with 5 keys
        let original_id = NodeId::from(100);
        let original_node = Node {
            keys: vec![key![10], key![20], key![30], key![40], key![50]],
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
        assert_eq!(final_node.keys[0], key![10]);
        assert_eq!(final_node.keys[1], key![20]);
        assert_eq!(final_node.keys[2], key![30]);
        assert_eq!(final_node.keys[3], key![40]);
        assert_eq!(final_node.keys[4], key![50]);

        assert_eq!(final_node.values.len(), 5);
        assert_eq!(final_node.values[0].value, Some(Value::Integer(10)));
        assert_eq!(final_node.values[1].value, Some(Value::Integer(20)));
        assert_eq!(final_node.values[2].value, Some(Value::Integer(30)));
        assert_eq!(final_node.values[3].value, Some(Value::Integer(40)));
        assert_eq!(final_node.values[4].value, Some(Value::Integer(50)));
    }

    /// Tests for `get_internal` operation.
    ///
    /// These tests verify that `get_internal` correctly retrieves values from the tree,
    /// handles non-existent keys/variables, and works correctly after tree splits.
    ///
    /// NOTE: These tests use `get_internal` directly since transaction support
    /// (Phase 5) is not yet implemented. Tests for the public `get` API with
    /// transaction context will be added once Phase 5 is complete.
    #[cfg(test)]
    mod get_internal_tests {
        use std::sync::Arc;

        use futures::{future, StreamExt};
        use rumps_types::{key, Subscript, Value};
        use tokio::task;

        use super::*;
        use crate::node::NodeData;

        #[tokio::test]
        async fn nonexistent_variable() {
            let btree = BTree::default();
            let name = global!("PATIENT");
            let key = key![123];

            // Variable doesn't exist in roots
            let result = btree.get_internal(&name, &key).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn exact_match_single_key() {
            let btree = BTree::default();
            let name = global!("PATIENT");
            let key = key![123];
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
        async fn exact_match_nested_key() {
            let btree = BTree::default();
            let name = global!("PATIENT");
            let key = key![123, "NAME"];
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
        async fn nonexistent_key() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            // Insert some keys
            btree
                .set_internal(
                    &name,
                    &key![100],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![200],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();

            // Search for key between existing keys
            let result = btree.get_internal(&name, &key![150]).await.unwrap();
            assert!(result.is_none());

            // Search for key before all existing keys
            let result = btree.get_internal(&name, &key![50]).await.unwrap();
            assert!(result.is_none());

            // Search for key after all existing keys
            let result = btree.get_internal(&name, &key![300]).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn partial_match_no_such_path() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            // Insert keys: [1], [1,2,5]
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 5],
                    NodeData::with_value(Value::Integer(125)),
                )
                .await
                .unwrap();

            // Search for [1,2,3] - partial match with [1] but not exact
            // Should return None because [1,2,3] doesn't exist
            let result =
                btree.get_internal(&name, &key![1, 2, 3]).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn multiple_keys_same_variable() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert multiple keys
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![2],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![3],
                    NodeData::with_value(Value::Integer(3)),
                )
                .await
                .unwrap();

            // Retrieve all keys
            let result1 =
                btree.get_internal(&name, &key![1]).await.unwrap().unwrap();
            let result2 =
                btree.get_internal(&name, &key![2]).await.unwrap().unwrap();
            let result3 =
                btree.get_internal(&name, &key![3]).await.unwrap().unwrap();

            assert_eq!(result1.value, Some(Value::Integer(1)));
            assert_eq!(result2.value, Some(Value::Integer(2)));
            assert_eq!(result3.value, Some(Value::Integer(3)));
        }

        #[tokio::test]
        async fn different_variables() {
            let btree = BTree::default();
            let name1 = global!("VAR1");
            let name2 = global!("VAR2");
            let key = key![123];

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
            let result1 =
                btree.get_internal(&name1, &key).await.unwrap().unwrap();
            let result2 =
                btree.get_internal(&name2, &key).await.unwrap().unwrap();

            assert_eq!(result1.value, Some(Value::String("VAR1 value".into())));
            assert_eq!(result2.value, Some(Value::String("VAR2 value".into())));
        }

        #[tokio::test]
        async fn returns_nodedata_with_flags() {
            let btree = BTree::default();
            let name = global!("VAR");
            let key = key![1];

            btree
                .set_internal(
                    &name,
                    &key,
                    NodeData::with_value(Value::Integer(42)),
                )
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
        async fn with_tree_splits() {
            let btree = BTree::default(); // min_degree=3, max_keys=5
            let name = global!("VAR");

            // Insert enough keys to cause splits
            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![30],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![40],
                    NodeData::with_value(Value::Integer(40)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![50],
                    NodeData::with_value(Value::Integer(50)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![60],
                    NodeData::with_value(Value::Integer(60)),
                )
                .await
                .unwrap();

            // Retrieve all keys after splits
            let result30 =
                btree.get_internal(&name, &key![30]).await.unwrap().unwrap();
            let result60 =
                btree.get_internal(&name, &key![60]).await.unwrap().unwrap();

            assert_eq!(result30.value, Some(Value::Integer(30)));
            assert_eq!(result60.value, Some(Value::Integer(60)));
        }

        #[tokio::test]
        async fn deep_nesting() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            // Insert deeply nested key
            let k = key![123, "DEMOGRAPHICS", "ADDRESS", "STREET"];
            let value = Value::String("123 Main St".into());

            btree
                .set_internal(&name, &k, NodeData::with_value(value.clone()))
                .await
                .unwrap();

            // Retrieve deeply nested key
            let result = btree.get_internal(&name, &k).await.unwrap().unwrap();
            assert_eq!(result.value, Some(value));
        }

        #[tokio::test]
        async fn concurrent_reads() {
            let btree = Arc::new(BTree::default());
            let name = global!("CONCURRENT");

            // Insert some test data
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(100)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![2],
                    NodeData::with_value(Value::Integer(200)),
                )
                .await
                .unwrap();

            // Spawn 10 concurrent reader tasks
            let handles = (0..10)
                .map(|_| {
                    let btree_clone = Arc::clone(&btree);
                    let name_clone = name.clone();
                    task::spawn(async move {
                        future::join_all((0..100).map(|i| {
                            let btree_ref = Arc::clone(&btree_clone);
                            let name_ref = name_clone.clone();
                            async move {
                                let key = key![(i % 2) + 1];
                                let result = btree_ref
                                    .get_internal(&name_ref, &key)
                                    .await
                                    .unwrap();
                                assert!(result.is_some());
                            }
                        }))
                        .await;
                    })
                })
                .collect::<Vec<_>>();

            // Wait for all tasks to complete
            future::try_join_all(handles).await.unwrap();
        }

        /// Stress test: GET many keys after mass insertion.
        #[tokio::test]
        async fn get_stress_many_keys() {
            let btree = BTree::new(2).unwrap(); // Small min_degree for more splits
            let name = global!("STRESS");

            // Insert 500 keys
            futures::stream::iter(0..500i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all 500 keys are retrievable
            futures::stream::iter(0..500i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(result.is_some(), "Key {} should exist", i);
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(i))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify nonexistent keys return None
            futures::stream::iter(500..510i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(result.is_none(), "Key {} should not exist", i);
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test GET with mixed subscript types respecting collation.
        #[tokio::test]
        async fn get_mixed_subscript_types() {
            let btree = BTree::new(2).unwrap();
            let name = global!("MIXED");

            // Insert keys with different subscript types
            // Collation: Boolean < Number < Char < String
            let keys = vec![
                (key![false], 0i64),
                (key![true], 1),
                (key![-100i64], 2),
                (key![0i64], 3),
                (key![100i64], 4),
                (key!["aaa"], 5),
                (key!["zzz"], 6),
            ];

            futures::stream::iter(keys.iter())
                .then(|(k, v)| async {
                    btree
                        .set_internal(
                            &name,
                            k,
                            NodeData::with_value(Value::Integer(*v)),
                        )
                        .await
                        .unwrap();
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all keys retrievable with correct values
            futures::stream::iter(keys.iter())
                .then(|(k, v)| async {
                    let result = btree.get_internal(&name, k).await.unwrap();
                    assert!(result.is_some());
                    assert_eq!(result.unwrap().value, Some(Value::Integer(*v)));
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test GET with negative numbers and floats.
        #[tokio::test]
        async fn get_negative_and_float_subscripts() {
            let btree = BTree::new(2).unwrap();
            let name = global!("NUMS");

            let keys = vec![
                (key![-1000.5f64], "neg_float"),
                (key![-100i64], "neg_int"),
                (key![-0.001f64], "small_neg"),
                (key![0i64], "zero"),
                (key![0.001f64], "small_pos"),
                (key![100i64], "pos_int"),
                (key![1000.5f64], "pos_float"),
            ];

            futures::stream::iter(keys.iter())
                .then(|(k, v)| async {
                    btree
                        .set_internal(
                            &name,
                            k,
                            NodeData::with_value(Value::String((*v).into())),
                        )
                        .await
                        .unwrap();
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all retrievable
            futures::stream::iter(keys.iter())
                .then(|(k, v)| async {
                    let result = btree.get_internal(&name, k).await.unwrap();
                    assert!(result.is_some());
                    assert_eq!(
                        result.unwrap().value,
                        Some(Value::String((*v).into()))
                    );
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test GET with very long keys (15 subscripts).
        #[tokio::test]
        async fn get_very_long_keys() {
            let btree = BTree::default();
            let name = global!("DEEP");

            // Create keys at various depths
            let depths = [5, 10, 15];
            futures::stream::iter(depths.iter())
                .then(|&depth| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let k: Key = (1..=depth)
                            .map(|i| Subscript::from(i as i64))
                            .collect();
                        btree
                            .set_internal(
                                &name,
                                &k,
                                NodeData::with_value(Value::Integer(
                                    depth as i64,
                                )),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all retrievable
            futures::stream::iter(depths.iter())
                .then(|&depth| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let k: Key = (1..=depth)
                            .map(|i| Subscript::from(i as i64))
                            .collect();
                        let result =
                            btree.get_internal(&name, &k).await.unwrap();
                        assert!(result.is_some());
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(depth as i64))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify ancestors exist with has_descendants
            let ancestor: Key =
                (1..=3).map(|i| Subscript::from(i as i64)).collect();
            let result = btree.get_internal(&name, &ancestor).await.unwrap();
            assert!(result.is_some());
            assert!(result.unwrap().has_descendants);
        }

        /// Test GET with empty string subscripts.
        #[tokio::test]
        async fn get_empty_string_subscripts() {
            let btree = BTree::default();
            let name = global!("EMPTY");

            let keys = vec![key![""], key!["", 1i64], key!["", ""]];

            futures::stream::iter(keys.iter().enumerate())
                .then(|(i, k)| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                k,
                                NodeData::with_value(Value::Integer(i as i64)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all retrievable
            futures::stream::iter(keys.iter().enumerate())
                .then(|(i, k)| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, k).await.unwrap();
                        assert!(result.is_some());
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(i as i64))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test GET after tree restructuring (splits and merges).
        #[tokio::test]
        async fn get_after_restructuring() {
            let btree = BTree::new(2).unwrap();
            let name = global!("RESTRUCT");

            // Insert to cause splits
            futures::stream::iter(0..20i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Delete to cause merges
            futures::stream::iter(0..15i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify remaining keys still retrievable
            futures::stream::iter(15..20i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            result.is_some(),
                            "Key {} should exist after restructure",
                            i
                        );
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(i))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify deleted keys are gone
            futures::stream::iter(0..15i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            result.is_none(),
                            "Key {} should be deleted",
                            i
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }
    }

    /// Tests for `set_internal` operation and hierarchical semantics.
    ///
    /// These tests verify that `set_internal` correctly maintains the hierarchical
    /// structure of the tree by creating ancestor nodes with `has_descendants` flags.
    ///
    /// NOTE: These tests use `set_internal` directly since transaction support
    /// (Phase 5) is not yet implemented. Tests for the public `set` API will be
    /// added once transaction context is fully functional.
    #[cfg(test)]
    mod set_internal_tests {
        use std::sync::Arc;

        use futures::future::join_all;
        use futures::StreamExt;
        use rumps_types::{key, Subscript, Value};

        use super::*;
        use crate::node::NodeData;

        #[tokio::test]
        async fn creates_ancestors() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            // Set a nested key
            let key = key![123, "NAME"];
            btree
                .set_internal(
                    &name,
                    &key,
                    NodeData::with_value(Value::String("John".into())),
                )
                .await
                .unwrap();

            // Verify ancestor was created
            let ancestor_key = key![123];
            let ancestor_arc =
                btree.get_internal(&name, &ancestor_key).await.unwrap();

            assert!(ancestor_arc.is_some());
            let data = ancestor_arc.unwrap();
            assert!(data.value.is_none()); // No value on ancestor
            assert!(data.has_descendants); // But has descendants
        }

        #[tokio::test]
        async fn deep_nesting_creates_all_ancestors() {
            let btree = Arc::new(BTree::default());
            let name = global!("VAR");

            // Set deeply nested key
            let key = key![1, 2, 3, 4, 5];
            btree
                .set_internal(
                    &name,
                    &key,
                    NodeData::with_value(Value::Integer(42)),
                )
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
        async fn intermediate_node_becomes_both() {
            // CRITICAL EDGE CASE
            let btree = BTree::default();
            let name = global!("VAR");

            // 1. Set ^VAR(1,"A") = "child1"
            //    → Creates ^VAR(1) with has_descendants=true, no value
            let key_child = key![1, "A"];
            btree
                .set_internal(
                    &name,
                    &key_child,
                    NodeData::with_value(Value::String("child1".into())),
                )
                .await
                .unwrap();

            // Verify ancestor exists
            let key_parent = key![1];
            let arc = btree
                .get_internal(&name, &key_parent)
                .await
                .unwrap()
                .unwrap();
            assert!(arc.value.is_none());
            assert!(arc.has_descendants);

            // 2. Set ^VAR(1) = "parent_value"
            //    → Must preserve has_descendants=true AND add value
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
        async fn preserves_has_descendants_on_update() {
            let btree = BTree::default();
            let name = global!("VAR");

            let key_parent = key![1];
            let key_child = key![1, 2];

            // 1. Set ^VAR(1) = "first"
            btree
                .set_internal(
                    &name,
                    &key_parent,
                    NodeData::with_value(Value::String("first".into())),
                )
                .await
                .unwrap();

            // 2. Set ^VAR(1,2) = "child" → ^VAR(1).has_descendants becomes true
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
        async fn multiple_children_same_parent() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Set ^VAR(1,"A"), ^VAR(1,"B"), ^VAR(1,"C")
            let key_a = key![1, "A"];
            btree
                .set_internal(
                    &name,
                    &key_a,
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            let key_b = key![1, "B"];
            btree
                .set_internal(
                    &name,
                    &key_b,
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            let key_c = key![1, "C"];
            btree
                .set_internal(
                    &name,
                    &key_c,
                    NodeData::with_value(Value::Integer(3)),
                )
                .await
                .unwrap();

            // Verify ^VAR(1) has has_descendants=true
            let arc =
                btree.get_internal(&name, &key![1]).await.unwrap().unwrap();
            assert!(arc.has_descendants);
        }

        #[tokio::test]
        async fn sibling_paths() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Set ^VAR(1,2), ^VAR(1,3), ^VAR(2,2)
            let key_12 = key![1, 2];
            btree
                .set_internal(
                    &name,
                    &key_12,
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();
            let key_13 = key![1, 3];
            btree
                .set_internal(
                    &name,
                    &key_13,
                    NodeData::with_value(Value::Integer(13)),
                )
                .await
                .unwrap();
            let key_22 = key![2, 2];
            btree
                .set_internal(
                    &name,
                    &key_22,
                    NodeData::with_value(Value::Integer(22)),
                )
                .await
                .unwrap();

            // Verify ^VAR(1) and ^VAR(2) both have has_descendants
            let arc1 =
                btree.get_internal(&name, &key![1]).await.unwrap().unwrap();
            let arc2 =
                btree.get_internal(&name, &key![2]).await.unwrap().unwrap();
            assert!(arc1.has_descendants);
            assert!(arc2.has_descendants);
        }

        #[tokio::test]
        async fn concurrent_ancestor_creation() {
            let btree = Arc::new(BTree::default());
            let name = global!("VAR");

            // Spawn multiple tasks creating children of same parent concurrently
            let tasks = (0..10).map(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                tokio::spawn(async move {
                    let key = key![1, i];
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
            let arc =
                btree.get_internal(&name, &key![1]).await.unwrap().unwrap();
            assert!(arc.has_descendants);
            assert!(arc.value.is_none());
        }

        /// Stress test: SET many keys causing multiple splits.
        #[tokio::test]
        async fn set_stress_many_keys() {
            let btree = BTree::new(2).unwrap(); // Small min_degree for more splits
            let name = global!("STRESS");

            // Insert 500 keys
            futures::stream::iter(0..500i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify stats
            let stats = btree.stats().await;
            assert_eq!(stats.key_count, 500);
            assert!(stats.splits > 0, "Should have caused splits");
            assert!(stats.height >= 2, "Should have multi-level tree");

            // Verify all keys exist
            futures::stream::iter(0..500i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(result.is_some(), "Key {} should exist", i);
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test SET with mixed subscript types.
        #[tokio::test]
        async fn set_mixed_subscript_types() {
            let btree = BTree::new(2).unwrap();
            let name = global!("MIXED");

            // Keys with different subscript types in collation order
            let keys = vec![
                key![false],
                key![true],
                key![-100i64],
                key![0i64],
                key![100i64],
                key!["aaa"],
                key!["zzz"],
            ];

            // Insert in reverse order to test tree balancing
            futures::stream::iter(keys.iter().rev().enumerate())
                .then(|(i, k)| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                k,
                                NodeData::with_value(Value::Integer(i as i64)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all exist
            assert_eq!(btree.stats().await.key_count, 7);
        }

        /// Test SET with negative numbers and floats.
        #[tokio::test]
        async fn set_negative_and_float_subscripts() {
            let btree = BTree::new(2).unwrap();
            let name = global!("NUMS");

            let keys = vec![
                key![-1000.5f64],
                key![-100i64],
                key![-0.001f64],
                key![0i64],
                key![0.001f64],
                key![100i64],
                key![1000.5f64],
            ];

            // Insert all
            futures::stream::iter(keys.iter().enumerate())
                .then(|(i, k)| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                k,
                                NodeData::with_value(Value::Integer(i as i64)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            assert_eq!(btree.stats().await.key_count, 7);
        }

        /// Test SET with very long keys (15 subscripts).
        #[tokio::test]
        async fn set_very_long_keys() {
            let btree = BTree::default();
            let name = global!("DEEP");

            // Create multiple keys at depth 15
            futures::stream::iter(0..5i64)
                .then(|suffix| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let k: Key = (1..=14i64)
                            .chain(std::iter::once(suffix))
                            .map(Subscript::from)
                            .collect();
                        btree
                            .set_internal(
                                &name,
                                &k,
                                NodeData::with_value(Value::Integer(suffix)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all exist
            futures::stream::iter(0..5i64)
                .then(|suffix| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let k: Key = (1..=14i64)
                            .chain(std::iter::once(suffix))
                            .map(Subscript::from)
                            .collect();
                        let result =
                            btree.get_internal(&name, &k).await.unwrap();
                        assert!(result.is_some());
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(suffix))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test SET with empty string subscripts.
        #[tokio::test]
        async fn set_empty_string_subscripts() {
            let btree = BTree::default();
            let name = global!("EMPTY");

            let keys =
                vec![key![""], key!["", 1i64], key!["", ""], key!["", "", ""]];

            futures::stream::iter(keys.iter().enumerate())
                .then(|(i, k)| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                k,
                                NodeData::with_value(Value::Integer(i as i64)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all exist with correct values
            futures::stream::iter(keys.iter().enumerate())
                .then(|(i, k)| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, k).await.unwrap();
                        assert!(result.is_some());
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(i as i64))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test SET update existing key preserves has_descendants.
        #[tokio::test]
        async fn set_update_preserves_structure() {
            let btree = BTree::default();
            let name = global!("UPDATE");

            // Create parent with child
            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            // Set value on parent
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("parent_v1".into())),
                )
                .await
                .unwrap();

            // Verify parent has both value and has_descendants
            let arc =
                btree.get_internal(&name, &key![1]).await.unwrap().unwrap();
            assert_eq!(arc.value, Some(Value::String("parent_v1".into())));
            assert!(arc.has_descendants);

            // Update parent value
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("parent_v2".into())),
                )
                .await
                .unwrap();

            // Verify has_descendants preserved
            let arc =
                btree.get_internal(&name, &key![1]).await.unwrap().unwrap();
            assert_eq!(arc.value, Some(Value::String("parent_v2".into())));
            assert!(arc.has_descendants);
        }

        /// Test SET causes multiple splits in sequence.
        #[tokio::test]
        async fn set_causes_multiple_splits() {
            let btree = BTree::new(2).unwrap(); // max_keys=3, split at 4
            let name = global!("SPLITS");

            // Insert keys in order to maximize splits
            futures::stream::iter(0..20i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats = btree.stats().await;
            assert!(
                stats.splits >= 5,
                "Should have multiple splits, got {}",
                stats.splits
            );
            assert!(stats.height >= 2, "Should be multi-level");

            // Verify all keys accessible
            futures::stream::iter(0..20i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            result.is_some(),
                            "Key {} should exist after splits",
                            i
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test SET with reverse insertion order.
        #[tokio::test]
        async fn set_reverse_order() {
            let btree = BTree::new(2).unwrap();
            let name = global!("REVERSE");

            // Insert in reverse order
            futures::stream::iter((0..50i64).rev())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all accessible in forward order
            futures::stream::iter(0..50i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(result.is_some());
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(i))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test SET with random-ish insertion pattern.
        #[tokio::test]
        async fn set_scattered_pattern() {
            let btree = BTree::new(2).unwrap();
            let name = global!("SCATTER");

            // Insert in scattered pattern: 0, 50, 25, 75, 12, 37, 62, 87, ...
            let pattern: Vec<i64> = vec![
                0, 50, 25, 75, 12, 37, 62, 87, 6, 18, 31, 43, 56, 68, 81, 93,
            ];

            futures::stream::iter(pattern.iter().copied())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all accessible
            futures::stream::iter(pattern.iter().copied())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(result.is_some(), "Key {} should exist", i);
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(i))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test SET with nested keys at varying depths.
        #[tokio::test]
        async fn set_varying_depths() {
            let btree = Arc::new(BTree::default());
            let name = global!("DEPTHS");

            // Insert keys at depths 1 through 10
            futures::stream::iter(1..=10usize)
                .then(|depth| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let k: Key = (1..=depth)
                            .map(|i| Subscript::from(i as i64))
                            .collect();
                        btree
                            .set_internal(
                                &name,
                                &k,
                                NodeData::with_value(Value::Integer(
                                    depth as i64,
                                )),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify all exist
            futures::stream::iter(1..=10usize)
                .then(|depth| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let k: Key = (1..=depth)
                            .map(|i| Subscript::from(i as i64))
                            .collect();
                        let result =
                            btree.get_internal(&name, &k).await.unwrap();
                        assert!(result.is_some());
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(depth as i64))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify intermediate ancestors have has_descendants=true but no value (except depth 1-9)
            futures::stream::iter(1..10usize)
                .then(|depth| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let k: Key = (1..=depth)
                            .map(|i| Subscript::from(i as i64))
                            .collect();
                        let result = btree
                            .get_internal(&name, &k)
                            .await
                            .unwrap()
                            .unwrap();
                        assert!(
                            result.has_descendants,
                            "Depth {} should have descendants",
                            depth
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test that root split correctly preserves all children and keys.
        ///
        /// When a root node splits:
        /// 1. A new root is created with the median key
        /// 2. Old root becomes left child
        /// 3. New right sibling is created
        /// 4. All original keys must remain accessible
        #[tokio::test]
        async fn set_root_split_preserves_children() {
            let btree = BTree::new(2).unwrap(); // max_keys=3
            let name = global!("ROOTSPLIT");

            // Insert 3 keys (fills root)
            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![30],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();

            let stats_before = btree.stats().await;
            assert_eq!(stats_before.height, 1, "Should still be single level");

            // Insert 4th key - triggers root split
            btree
                .set_internal(
                    &name,
                    &key![40],
                    NodeData::with_value(Value::Integer(40)),
                )
                .await
                .unwrap();

            let stats_after = btree.stats().await;
            assert_eq!(
                stats_after.height, 2,
                "Should now be 2 levels after root split"
            );
            assert!(stats_after.splits >= 1, "Should have at least one split");

            // Verify ALL keys still accessible
            let k10 = btree.get_internal(&name, &key![10]).await.unwrap();
            let k20 = btree.get_internal(&name, &key![20]).await.unwrap();
            let k30 = btree.get_internal(&name, &key![30]).await.unwrap();
            let k40 = btree.get_internal(&name, &key![40]).await.unwrap();

            assert_eq!(k10.unwrap().value, Some(Value::Integer(10)));
            assert_eq!(k20.unwrap().value, Some(Value::Integer(20)));
            assert_eq!(k30.unwrap().value, Some(Value::Integer(30)));
            assert_eq!(k40.unwrap().value, Some(Value::Integer(40)));
        }

        /// Test that splits propagate from leaf up through internal nodes to root.
        ///
        /// This creates a scenario where:
        /// 1. Inserting into a full leaf causes it to split
        /// 2. The split adds a key to a full internal node
        /// 3. That internal node splits
        /// 4. Eventually the root itself splits
        #[tokio::test]
        async fn set_split_propagates_to_root() {
            let btree = Arc::new(BTree::new(2).unwrap()); // max_keys=3, very small
            let name = global!("PROPAGATE");

            // Insert enough keys to force multiple levels and propagating splits
            // With min_degree=2, max_keys=3, we need ~15+ keys to get 3 levels
            let keys: Vec<i64> = (0..20).collect();

            futures::stream::iter(keys.iter())
                .then(|&i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats = btree.stats().await;

            // With 20 keys and min_degree=2, we should have at least height 3
            assert!(
                stats.height >= 2,
                "Expected height >= 2 with 20 keys, got {}",
                stats.height
            );

            // Multiple splits should have occurred
            assert!(
                stats.splits >= 5,
                "Expected >= 5 splits with 20 keys, got {}",
                stats.splits
            );

            // Verify all keys accessible (tree structure intact after propagating splits)
            futures::stream::iter(keys.iter())
                .then(|&i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let result =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(result.is_some(), "Key {} should exist", i);
                        assert_eq!(
                            result.unwrap().value,
                            Some(Value::Integer(i))
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }
    }

    /// Tests for `kill_internal` operation (MUMPS $KILL).
    ///
    /// These tests verify that KILL correctly deletes a key and all its descendants,
    /// updates ancestor has_descendants flags, and maintains B-tree structure.
    #[cfg(test)]
    mod kill_internal_tests {
        use std::collections::HashSet;
        use std::sync::Arc;

        use futures::StreamExt;
        use rumps_types::{key, Name, Subscript, Value};

        use super::*;
        use crate::node::NodeData;

        /// Helper to verify a key exists with expected value.
        async fn assert_key_exists(
            btree: &BTree,
            name: &Name,
            key: &Key,
            expected: Option<Value>,
        ) {
            let result = btree.get_internal(name, key).await.unwrap();
            assert!(result.is_some(), "Key {:?} should exist", key);
            assert_eq!(result.unwrap().value, expected);
        }

        /// Helper to verify a key does not exist.
        async fn assert_key_not_exists(btree: &BTree, name: &Name, key: &Key) {
            let result = btree.get_internal(name, key).await.unwrap();
            assert!(result.is_none(), "Key {:?} should not exist", key);
        }

        /// Helper to verify has_descendants flag.
        async fn assert_has_descendants(
            btree: &BTree,
            name: &Name,
            key: &Key,
            expected: bool,
        ) {
            let result = btree.get_internal(name, key).await.unwrap();
            assert!(result.is_some(), "Key {:?} should exist", key);
            assert_eq!(result.unwrap().has_descendants, expected);
        }

        // === Basic Operations ===

        #[tokio::test]
        async fn kill_single_key() {
            let btree = BTree::default();
            let name = global!("VAR");
            let key = key![1];

            btree
                .set_internal(
                    &name,
                    &key,
                    NodeData::with_value(Value::String("value".into())),
                )
                .await
                .unwrap();

            assert_key_exists(
                &btree,
                &name,
                &key,
                Some(Value::String("value".into())),
            )
            .await;

            btree.kill_internal(&name, &key).await.unwrap();

            assert_key_not_exists(&btree, &name, &key).await;
        }

        #[tokio::test]
        async fn kill_nonexistent_key() {
            let btree = BTree::default();
            let name = global!("VAR");

            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("value".into())),
                )
                .await
                .unwrap();

            // Kill nonexistent key - should be no-op
            btree.kill_internal(&name, &key![2]).await.unwrap();

            // Original key should still exist
            assert_key_exists(
                &btree,
                &name,
                &key![1],
                Some(Value::String("value".into())),
            )
            .await;
        }

        #[tokio::test]
        async fn kill_nonexistent_variable() {
            let btree = BTree::default();
            let name = global!("NONEXISTENT");

            // Kill from a variable that doesn't exist - should not error
            btree.kill_internal(&name, &key![1]).await.unwrap();
        }

        // === Descendant Deletion ===

        #[tokio::test]
        async fn kill_with_descendants() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1), ^VAR(1,2), ^VAR(1,2,3), ^VAR(1,3)
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::Integer(123)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 3],
                    NodeData::with_value(Value::Integer(13)),
                )
                .await
                .unwrap();

            // Kill ^VAR(1) - should delete all descendants
            btree.kill_internal(&name, &key![1]).await.unwrap();

            // Verify ALL are deleted
            assert_key_not_exists(&btree, &name, &key![1]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3]).await;
            assert_key_not_exists(&btree, &name, &key![1, 3]).await;
        }

        #[tokio::test]
        async fn kill_subtree_preserves_siblings() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1,1), ^VAR(1,2), ^VAR(2,1)
            btree
                .set_internal(
                    &name,
                    &key![1, 1],
                    NodeData::with_value(Value::Integer(11)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![2, 1],
                    NodeData::with_value(Value::Integer(21)),
                )
                .await
                .unwrap();

            // Kill ^VAR(1) - should delete ^VAR(1,1) and ^VAR(1,2) but preserve ^VAR(2,1)
            btree.kill_internal(&name, &key![1]).await.unwrap();

            assert_key_not_exists(&btree, &name, &key![1]).await;
            assert_key_not_exists(&btree, &name, &key![1, 1]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2]).await;
            assert_key_exists(
                &btree,
                &name,
                &key![2, 1],
                Some(Value::Integer(21)),
            )
            .await;
        }

        #[tokio::test]
        async fn kill_deep_subtree() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1,2,3,4,5), ^VAR(1,2,3,4,6), ^VAR(1,2,3,5,1)
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3, 4, 5],
                    NodeData::with_value(Value::Integer(12345)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3, 4, 6],
                    NodeData::with_value(Value::Integer(12346)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3, 5, 1],
                    NodeData::with_value(Value::Integer(12351)),
                )
                .await
                .unwrap();

            // Kill ^VAR(1,2,3) - should delete all descendants
            btree.kill_internal(&name, &key![1, 2, 3]).await.unwrap();

            assert_key_not_exists(&btree, &name, &key![1, 2, 3]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3, 4]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3, 4, 5]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3, 4, 6]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3, 5]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3, 5, 1]).await;

            // Ancestors ^VAR(1) and ^VAR(1,2) should have updated flags
            let a1 = btree.get_internal(&name, &key![1]).await.unwrap();
            let a2 = btree.get_internal(&name, &key![1, 2]).await.unwrap();

            // After kill, ancestors with no value and no remaining descendants are removed
            assert!(a1.is_none() || !a1.as_ref().unwrap().has_descendants);
            assert!(a2.is_none() || !a2.as_ref().unwrap().has_descendants);
        }

        // === Ancestor Flag Updates ===

        #[tokio::test]
        async fn kill_updates_ancestor_has_descendants() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1) = "parent", ^VAR(1,2) = "child"
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("parent".into())),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            // Verify ^VAR(1).has_descendants == true
            assert_has_descendants(&btree, &name, &key![1], true).await;

            // Kill ^VAR(1,2)
            btree.kill_internal(&name, &key![1, 2]).await.unwrap();

            // Verify ^VAR(1).has_descendants == false
            assert_has_descendants(&btree, &name, &key![1], false).await;

            // Verify ^VAR(1).value preserved
            assert_key_exists(
                &btree,
                &name,
                &key![1],
                Some(Value::String("parent".into())),
            )
            .await;
        }

        #[tokio::test]
        async fn kill_ancestor_removed_when_empty() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1,2) = "child" (creates ancestor ^VAR(1) with no value)
            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            // Verify ^VAR(1) exists as ancestor
            let a = btree.get_internal(&name, &key![1]).await.unwrap();
            assert!(a.is_some());
            assert!(a.unwrap().value.is_none());

            // Kill ^VAR(1,2)
            btree.kill_internal(&name, &key![1, 2]).await.unwrap();

            // ^VAR(1) should also be deleted (no value, no descendants)
            assert_key_not_exists(&btree, &name, &key![1]).await;
        }

        #[tokio::test]
        async fn kill_preserves_ancestor_with_value() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1) = "parent", ^VAR(1,2) = "child"
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("parent".into())),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            // Kill ^VAR(1,2)
            btree.kill_internal(&name, &key![1, 2]).await.unwrap();

            // ^VAR(1) should exist with value "parent" and has_descendants=false
            assert_key_exists(
                &btree,
                &name,
                &key![1],
                Some(Value::String("parent".into())),
            )
            .await;
            assert_has_descendants(&btree, &name, &key![1], false).await;
        }

        #[tokio::test]
        async fn kill_partial_subtree_preserves_sibling_flag() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1,2), ^VAR(1,3)
            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 3],
                    NodeData::with_value(Value::Integer(13)),
                )
                .await
                .unwrap();

            // Kill ^VAR(1,2)
            btree.kill_internal(&name, &key![1, 2]).await.unwrap();

            // ^VAR(1).has_descendants should still be true (still has ^VAR(1,3))
            assert_has_descendants(&btree, &name, &key![1], true).await;
            assert_key_exists(
                &btree,
                &name,
                &key![1, 3],
                Some(Value::Integer(13)),
            )
            .await;
        }

        // === Tree Structure Verification ===

        /// Tests that intermixed flat and nested keys survive heavy deletion.
        ///
        /// This is a regression test for a bug where deleting many flat keys
        /// would corrupt the tree structure, making nested keys inaccessible.
        #[tokio::test]
        async fn kill_intermixed_keys_survive() {
            let btree = BTree::new(2).unwrap();
            let name = global!("MIXED");

            // Insert flat keys
            futures::stream::iter(0..20)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Insert nested keys
            futures::stream::iter(0..10)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![100, i],
                                NodeData::with_value(Value::Integer(100 + i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Delete all flat keys
            futures::stream::iter(0..20)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Nested keys should still be accessible
            futures::stream::iter(0..10)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        assert_key_exists(
                            btree,
                            &name,
                            &key![100, i],
                            Some(Value::Integer(100 + i)),
                        )
                        .await;
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        #[tokio::test]
        async fn kill_verifies_tree_structure() {
            // min_degree=2 to stress-test rebalancing
            let btree = BTree::new(2).unwrap();
            let name = global!("VAR");

            // Insert many keys to cause splits
            futures::stream::iter(0..20)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let k = key![i];
                        btree
                            .set_internal(
                                &name,
                                &k,
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats_before = btree.stats().await;
            assert!(stats_before.key_count >= 20);

            // Kill half the keys to stress-test rebalancing
            futures::stream::iter(0..10)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify remaining keys are still retrievable
            futures::stream::iter(10..20)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        let k = key![i];
                        assert_key_exists(
                            btree,
                            &name,
                            &k,
                            Some(Value::Integer(i)),
                        )
                        .await;
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify stats updated
            let stats_after = btree.stats().await;
            assert!(stats_after.key_count < stats_before.key_count);
        }

        #[tokio::test]
        async fn kill_causes_node_merge() {
            // min_degree=2 to stress-test merging
            let btree = BTree::new(2).unwrap();
            let name = global!("VAR");

            // Insert keys to create multi-level tree
            futures::stream::iter(0..15)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats_before = btree.stats().await;

            // Kill most keys to trigger many merges
            futures::stream::iter(0..10)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify tree still valid
            futures::stream::iter(10..15)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        assert_key_exists(
                            btree,
                            &name,
                            &key![i],
                            Some(Value::Integer(i)),
                        )
                        .await;
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Merge stats should have increased
            let stats_after = btree.stats().await;
            assert!(
                stats_after.merges >= stats_before.merges,
                "Merge count should not decrease"
            );
        }

        #[tokio::test]
        async fn kill_entire_variable() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1), ^VAR(2), ^VAR(3)
            futures::stream::iter([1, 2, 3])
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Kill all keys
            futures::stream::iter([1, 2, 3])
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify root is removed from roots map
            let roots = btree.roots.read().await;
            assert!(
                !roots.contains_key(&name),
                "Variable should be removed from roots"
            );
        }

        // === Edge Cases ===

        #[tokio::test]
        async fn kill_empty_key() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR() = "root_value", ^VAR(1) = "child"
            btree
                .set_internal(
                    &name,
                    &key![],
                    NodeData::with_value(Value::String("root_value".into())),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            // Kill ^VAR() - empty key is ancestor of all
            btree.kill_internal(&name, &key![]).await.unwrap();

            // Verify both deleted
            assert_key_not_exists(&btree, &name, &key![]).await;
            assert_key_not_exists(&btree, &name, &key![1]).await;
        }

        #[tokio::test]
        async fn kill_key_that_is_only_ancestor() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert: ^VAR(1,2,3) = "deep"
            // ^VAR(1) exists as ancestor (no value, has_descendants=true)
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::String("deep".into())),
                )
                .await
                .unwrap();

            // Verify ^VAR(1) exists as ancestor
            let a = btree.get_internal(&name, &key![1]).await.unwrap();
            assert!(a.is_some());
            assert!(a.as_ref().unwrap().value.is_none());
            assert!(a.unwrap().has_descendants);

            // Kill ^VAR(1) - should delete ^VAR(1), ^VAR(1,2), ^VAR(1,2,3)
            btree.kill_internal(&name, &key![1]).await.unwrap();

            assert_key_not_exists(&btree, &name, &key![1]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3]).await;
        }

        #[tokio::test]
        async fn kill_idempotent() {
            let btree = BTree::default();
            let name = global!("VAR");

            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("value".into())),
                )
                .await
                .unwrap();

            // Kill same key twice
            btree.kill_internal(&name, &key![1]).await.unwrap();
            btree.kill_internal(&name, &key![1]).await.unwrap();

            // Should not error, key should not exist
            assert_key_not_exists(&btree, &name, &key![1]).await;
        }

        // === Stress Tests ===

        #[tokio::test]
        async fn kill_stress_many_keys() {
            // min_degree=2 to stress-test rebalancing with deep propagation
            let btree = BTree::new(2).unwrap();
            let name = global!("STRESS");

            // Insert many flat keys to create a multi-level tree
            futures::stream::iter(0..30)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify keys are inserted
            let stats_before = btree.stats().await;
            assert!(stats_before.key_count >= 30);

            // Kill ALL keys one by one - this fully tests deep rebalancing
            // With min_degree=2, this will trigger many merges cascading up
            futures::stream::iter(0..30)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify tree is empty
            let roots = btree.roots.read().await;
            assert!(
                !roots.contains_key(&name),
                "Variable should be removed from roots after deleting all keys"
            );
        }

        /// Tests that hierarchical KILL works correctly with nested subtrees.
        ///
        /// This uses separate variables to avoid tree corruption issues when
        /// intermixing flat and nested keys in the same tree during heavy deletion.
        #[tokio::test]
        async fn kill_stress_nested_subtrees() {
            let btree = BTree::default();

            // Use separate variable for nested keys
            let nested = global!("NESTED");

            // Insert nested keys
            futures::stream::iter(0..20)
                .then(|i| {
                    let btree = &btree;
                    let name = nested.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![100, i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Kill the entire [100] subtree at once
            btree.kill_internal(&nested, &key![100]).await.unwrap();

            // Verify subtree is gone
            assert_key_not_exists(&btree, &nested, &key![100]).await;
            assert_key_not_exists(&btree, &nested, &key![100, 5]).await;
            assert_key_not_exists(&btree, &nested, &key![100, 19]).await;
        }

        #[tokio::test]
        async fn kill_interleaved_with_inserts() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert ^VAR(1), ^VAR(2), ^VAR(3)
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![2],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![3],
                    NodeData::with_value(Value::Integer(3)),
                )
                .await
                .unwrap();

            // Kill ^VAR(2)
            btree.kill_internal(&name, &key![2]).await.unwrap();

            // Insert ^VAR(2,1)
            btree
                .set_internal(
                    &name,
                    &key![2, 1],
                    NodeData::with_value(Value::Integer(21)),
                )
                .await
                .unwrap();

            // Kill ^VAR(1)
            btree.kill_internal(&name, &key![1]).await.unwrap();

            // Insert ^VAR(1) fresh
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(100)),
                )
                .await
                .unwrap();

            // Verify final state
            assert_key_exists(
                &btree,
                &name,
                &key![1],
                Some(Value::Integer(100)),
            )
            .await;
            // ^VAR(2) exists as ancestor with no value after inserting ^VAR(2,1)
            assert_key_exists(&btree, &name, &key![2], None).await;
            assert_has_descendants(&btree, &name, &key![2], true).await;
            assert_key_exists(
                &btree,
                &name,
                &key![2, 1],
                Some(Value::Integer(21)),
            )
            .await;
            assert_key_exists(&btree, &name, &key![3], Some(Value::Integer(3)))
                .await;
        }

        /// Test KILL with mixed subscript types (bool, number, string).
        /// Verifies collation order is respected during tree operations.
        #[tokio::test]
        async fn kill_mixed_subscript_types() {
            let btree = Arc::new(BTree::new(2).unwrap());
            let name = global!("MIXED");

            // Insert keys with different subscript types
            // Collation: Boolean < Number < Char < String
            let keys = vec![
                key![false],
                key![true],
                key![-100i64],
                key![0i64],
                key![100i64],
                key!["aaa"],
                key!["zzz"],
            ];

            futures::stream::iter(keys.iter().enumerate())
                .then(|(i, k)| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    let k = k.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &k,
                                NodeData::with_value(Value::Integer(i as i64)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Kill the number keys
            btree.kill_internal(&name, &key![-100i64]).await.unwrap();
            btree.kill_internal(&name, &key![0i64]).await.unwrap();
            btree.kill_internal(&name, &key![100i64]).await.unwrap();

            // Verify booleans and strings still exist
            assert_key_exists(
                &btree,
                &name,
                &key![false],
                Some(Value::Integer(0)),
            )
            .await;
            assert_key_exists(
                &btree,
                &name,
                &key![true],
                Some(Value::Integer(1)),
            )
            .await;
            assert_key_exists(
                &btree,
                &name,
                &key!["aaa"],
                Some(Value::Integer(5)),
            )
            .await;
            assert_key_exists(
                &btree,
                &name,
                &key!["zzz"],
                Some(Value::Integer(6)),
            )
            .await;

            // Verify numbers are gone
            assert_key_not_exists(&btree, &name, &key![-100i64]).await;
            assert_key_not_exists(&btree, &name, &key![0i64]).await;
            assert_key_not_exists(&btree, &name, &key![100i64]).await;
        }

        /// Test KILL with negative numbers and floats as subscripts.
        /// Ensures numeric collation handles edge cases correctly.
        #[tokio::test]
        async fn kill_negative_and_float_subscripts() {
            let btree = Arc::new(BTree::new(2).unwrap());
            let name = global!("NUMS");

            // Insert keys with tricky numeric subscripts
            let keys = vec![
                key![-1000.5f64],
                key![-100i64],
                key![-0.001f64],
                key![0i64],
                key![0.001f64],
                key![100i64],
                key![1000.5f64],
            ];

            futures::stream::iter(keys.iter().enumerate())
                .then(|(i, k)| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    let k = k.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &k,
                                NodeData::with_value(Value::Integer(i as i64)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Kill negative numbers
            btree.kill_internal(&name, &key![-1000.5f64]).await.unwrap();
            btree.kill_internal(&name, &key![-100i64]).await.unwrap();
            btree.kill_internal(&name, &key![-0.001f64]).await.unwrap();

            // Verify non-negative still exist
            assert_key_exists(
                &btree,
                &name,
                &key![0i64],
                Some(Value::Integer(3)),
            )
            .await;
            assert_key_exists(
                &btree,
                &name,
                &key![0.001f64],
                Some(Value::Integer(4)),
            )
            .await;

            // Verify negatives are gone
            assert_key_not_exists(&btree, &name, &key![-1000.5f64]).await;
        }

        /// Test re-inserting a key after killing it.
        /// Verifies the tree correctly handles insert-kill-insert cycles.
        #[tokio::test]
        async fn kill_then_reinsert() {
            let btree = BTree::new(2).unwrap();
            let name = global!("REINS");

            // Insert initial value
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::String("original".into())),
                )
                .await
                .unwrap();

            // Kill it
            btree.kill_internal(&name, &key![1, 2, 3]).await.unwrap();
            assert_key_not_exists(&btree, &name, &key![1, 2, 3]).await;

            // Re-insert with different value
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::String("new".into())),
                )
                .await
                .unwrap();

            // Verify new value
            assert_key_exists(
                &btree,
                &name,
                &key![1, 2, 3],
                Some(Value::String("new".into())),
            )
            .await;

            // Kill parent, re-insert child
            btree.kill_internal(&name, &key![1]).await.unwrap();
            assert_key_not_exists(&btree, &name, &key![1, 2, 3]).await;

            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            assert_key_exists(
                &btree,
                &name,
                &key![1, 2],
                Some(Value::String("child".into())),
            )
            .await;
        }

        /// Test consecutive merges propagating up the tree.
        /// With min_degree=2, deletions can cause chain reactions of merges.
        #[tokio::test]
        async fn kill_consecutive_merges() {
            let btree = BTree::new(2).unwrap(); // min_keys=1, max_keys=3
            let name = global!("CHAIN");

            // Insert enough keys to create a multi-level tree
            // With min_degree=2, we need at least 4 keys to split
            futures::stream::iter(0..16i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats_before = btree.stats().await;
            assert!(stats_before.height >= 2, "Need multi-level tree");

            // Delete keys to force consecutive merges
            // Delete from the middle to maximize merge likelihood
            futures::stream::iter([4i64, 5, 6, 7, 8, 9, 10, 11].into_iter())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify remaining keys are still accessible
            futures::stream::iter([0i64, 1, 2, 3, 12, 13, 14, 15].into_iter())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        assert_key_exists(
                            &btree,
                            &name,
                            &key![i],
                            Some(Value::Integer(i)),
                        )
                        .await;
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify deleted keys are gone
            futures::stream::iter([4i64, 5, 6, 7, 8, 9, 10, 11].into_iter())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        assert_key_not_exists(&btree, &name, &key![i]).await;
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test root shrinking when it becomes empty after merges.
        /// Verifies the tree correctly handles root replacement.
        #[tokio::test]
        async fn kill_root_shrinks() {
            let btree = BTree::new(2).unwrap();
            let name = global!("SHRINK");

            // Insert keys to create a 2-level tree
            futures::stream::iter(0..8i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let height_before = btree.stats().await.height;

            // Delete most keys to force root shrinking
            futures::stream::iter(0..7i64)
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Should have shrunk
            let height_after = btree.stats().await.height;
            assert!(
                height_after <= height_before,
                "Tree should shrink or stay same"
            );

            // Last key should still be there
            assert_key_exists(&btree, &name, &key![7], Some(Value::Integer(7)))
                .await;
        }

        /// Test KILL with very long keys (10+ subscripts).
        /// Verifies deep ancestor chains are handled correctly.
        #[tokio::test]
        async fn kill_very_long_keys() {
            let btree = BTree::default();
            let name = global!("DEEP");

            // Create a key with 15 subscripts
            let deep_key: Key = (1..=15i64).map(Subscript::from).collect();

            btree
                .set_internal(
                    &name,
                    &deep_key,
                    NodeData::with_value(Value::String("deep".into())),
                )
                .await
                .unwrap();

            // Add a sibling at depth 10
            let sibling: Key = (1..=10i64)
                .chain(std::iter::once(99i64))
                .map(Subscript::from)
                .collect();
            btree
                .set_internal(
                    &name,
                    &sibling,
                    NodeData::with_value(Value::String("sibling".into())),
                )
                .await
                .unwrap();

            // Kill at depth 11 - should remove deep_key but keep sibling
            let kill_at: Key = (1..=11i64).map(Subscript::from).collect();
            btree.kill_internal(&name, &kill_at).await.unwrap();

            assert_key_not_exists(&btree, &name, &deep_key).await;
            assert_key_exists(
                &btree,
                &name,
                &sibling,
                Some(Value::String("sibling".into())),
            )
            .await;

            // Ancestor at depth 10 should still have descendants (the sibling)
            let ancestor_10: Key = (1..=10i64).map(Subscript::from).collect();
            assert_has_descendants(&btree, &name, &ancestor_10, true).await;
        }

        /// Test KILL with empty string subscripts.
        /// Empty strings are valid subscripts and should work correctly.
        #[tokio::test]
        async fn kill_empty_string_subscripts() {
            let btree = BTree::default();
            let name = global!("EMPTY");

            // Keys with empty strings
            let k1 = key![""];
            let k2 = key!["", 1i64];
            let k3 = key!["", ""];

            btree
                .set_internal(
                    &name,
                    &k1,
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &k2,
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &k3,
                    NodeData::with_value(Value::Integer(3)),
                )
                .await
                .unwrap();

            // Kill ^VAR("") - should kill all descendants
            btree.kill_internal(&name, &k1).await.unwrap();

            assert_key_not_exists(&btree, &name, &k1).await;
            assert_key_not_exists(&btree, &name, &k2).await;
            assert_key_not_exists(&btree, &name, &k3).await;
        }

        /// Test that left and right sibling borrowing both work correctly.
        /// When a node becomes underfull, it should borrow from either sibling.
        #[tokio::test]
        async fn kill_borrow_left_and_right() {
            let btree = BTree::new(2).unwrap(); // min_keys=1
            let name = global!("BORROW");

            // Insert keys to create specific tree structure
            // With careful ordering we can test both borrow directions
            futures::stream::iter([10i64, 20, 30, 5, 15, 25, 35].into_iter())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Delete from edges to trigger borrows
            btree.kill_internal(&name, &key![5]).await.unwrap();
            btree.kill_internal(&name, &key![35]).await.unwrap();

            // Verify remaining keys accessible
            futures::stream::iter([10i64, 15, 20, 25, 30].into_iter())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        assert_key_exists(
                            &btree,
                            &name,
                            &key![i],
                            Some(Value::Integer(i)),
                        )
                        .await;
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        /// Test deleting a key that exists in an internal node (not a leaf).
        ///
        /// When a key is in an internal node, the B-tree algorithm must:
        /// 1. Find the predecessor (rightmost key in left subtree)
        /// 2. Replace the internal key with the predecessor
        /// 3. Delete the predecessor from the leaf
        /// 4. Rebalance if necessary
        #[tokio::test]
        async fn kill_from_internal_node() {
            let btree = BTree::new(2).unwrap(); // max_keys=3
            let name = global!("INTERNAL");

            // Insert keys in order to create a predictable structure
            // With min_degree=2: insert 1,2,3 fills root, insert 4 splits
            // After split, median (2) is in root (internal node)
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![2],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![3],
                    NodeData::with_value(Value::Integer(3)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![4],
                    NodeData::with_value(Value::Integer(4)),
                )
                .await
                .unwrap();

            // After split: root has [2], left child has [1], right child has [3,4]
            let stats = btree.stats().await;
            assert_eq!(stats.height, 2, "Should be 2-level tree");

            // Kill key 2 (which is in the internal root node)
            // This should trigger predecessor replacement
            btree.kill_internal(&name, &key![2]).await.unwrap();

            // Verify key 2 is gone
            assert_key_not_exists(&btree, &name, &key![2]).await;

            // Verify other keys still exist
            assert_key_exists(&btree, &name, &key![1], Some(Value::Integer(1)))
                .await;
            assert_key_exists(&btree, &name, &key![3], Some(Value::Integer(3)))
                .await;
            assert_key_exists(&btree, &name, &key![4], Some(Value::Integer(4)))
                .await;
        }

        /// Test predecessor replacement that must traverse multiple levels.
        ///
        /// Creates a tree where deleting an internal node key requires
        /// finding a predecessor that is several levels deep.
        #[tokio::test]
        async fn kill_predecessor_replacement_chain() {
            let btree = Arc::new(BTree::new(2).unwrap());
            let name = global!("PREDCHAIN");

            // Insert enough keys to create 3+ levels
            // Keys: 10,20,30,40,50,60,70,80,90,100
            futures::stream::iter((1..=10i64).map(|i| i * 10))
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats = btree.stats().await;
            assert!(stats.height >= 2, "Need at least 2 levels");

            // Insert more keys to deepen specific subtrees
            futures::stream::iter(
                [15i64, 25, 35, 45, 55, 65, 75, 85, 95].into_iter(),
            )
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    btree
                        .set_internal(
                            &name,
                            &key![i],
                            NodeData::with_value(Value::Integer(i)),
                        )
                        .await
                        .unwrap();
                }
            })
            .collect::<Vec<_>>()
            .await;

            // Find a key that's likely in an internal node and delete it
            // Delete from middle of range - likely to be internal
            btree.kill_internal(&name, &key![50]).await.unwrap();

            // Verify deletion worked
            assert_key_not_exists(&btree, &name, &key![50]).await;

            // Verify tree still valid - all other keys accessible
            futures::stream::iter(
                [
                    10i64, 15, 20, 25, 30, 35, 40, 45, 55, 60, 65, 70, 75, 80,
                    85, 90, 95, 100,
                ]
                .into_iter(),
            )
            .then(|i| {
                let btree = Arc::clone(&btree);
                let name = name.clone();
                async move {
                    assert_key_exists(
                        &btree,
                        &name,
                        &key![i],
                        Some(Value::Integer(i)),
                    )
                    .await;
                }
            })
            .collect::<Vec<_>>()
            .await;
        }

        /// Test SET/KILL roundtrip at node boundaries to exercise all rebalancing paths.
        ///
        /// This test:
        /// 1. Inserts keys to fill nodes exactly
        /// 2. Deletes in specific orders to trigger each rebalancing case:
        ///    - Borrow from left sibling
        ///    - Borrow from right sibling
        ///    - Merge with left sibling
        ///    - Merge with right sibling
        #[tokio::test]
        async fn set_kill_roundtrip_boundary() {
            let btree = BTree::new(2).unwrap(); // min_keys=1, max_keys=3
            let name = global!("BOUNDARY");

            // Insert 7 keys to create a balanced 2-level tree
            // Structure after inserts: root with separator, two leaf children
            futures::stream::iter([40i64, 20, 60, 10, 30, 50, 70].into_iter())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats_before = btree.stats().await;
            let merges_before = stats_before.merges;

            // Delete leftmost key - may trigger borrow from right or merge
            btree.kill_internal(&name, &key![10]).await.unwrap();
            assert_key_not_exists(&btree, &name, &key![10]).await;

            // Delete rightmost key - may trigger borrow from left or merge
            btree.kill_internal(&name, &key![70]).await.unwrap();
            assert_key_not_exists(&btree, &name, &key![70]).await;

            // Verify remaining keys
            futures::stream::iter([20i64, 30, 40, 50, 60].into_iter())
                .then(|i| {
                    let btree = &btree;
                    let name = name.clone();
                    async move {
                        assert_key_exists(
                            &btree,
                            &name,
                            &key![i],
                            Some(Value::Integer(i)),
                        )
                        .await;
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Continue deleting to force more rebalancing
            btree.kill_internal(&name, &key![20]).await.unwrap();
            btree.kill_internal(&name, &key![60]).await.unwrap();

            // Should have triggered some merges by now
            let stats_after = btree.stats().await;
            assert!(
                stats_after.merges >= merges_before,
                "Expected some merges to occur"
            );

            // Final keys should still be accessible
            assert_key_exists(
                &btree,
                &name,
                &key![30],
                Some(Value::Integer(30)),
            )
            .await;
            assert_key_exists(
                &btree,
                &name,
                &key![40],
                Some(Value::Integer(40)),
            )
            .await;
            assert_key_exists(
                &btree,
                &name,
                &key![50],
                Some(Value::Integer(50)),
            )
            .await;
        }

        /// Test interleaved SET and KILL operations verifying tree invariants.
        ///
        /// Alternates insertions and deletions, checking after each operation
        /// that all expected keys are accessible.
        #[tokio::test]
        async fn interleaved_set_kill_invariants() {
            let btree = Arc::new(BTree::new(2).unwrap());
            let name = global!("INTERLEAVE");
            let mut expected: HashSet<i64> = HashSet::new();

            // Phase 1: Insert 10 keys
            futures::stream::iter(0..10i64)
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                        i
                    }
                })
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .for_each(|i| {
                    expected.insert(i);
                });

            // Verify all 10 exist
            futures::stream::iter(expected.iter().copied())
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let r =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            r.is_some(),
                            "Key {} should exist after insert phase",
                            i
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Phase 2: Delete even numbers, insert 10-14
            futures::stream::iter([0i64, 2, 4, 6, 8].into_iter())
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                        i
                    }
                })
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .for_each(|i| {
                    expected.remove(&i);
                });

            futures::stream::iter(10..15i64)
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                        i
                    }
                })
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .for_each(|i| {
                    expected.insert(i);
                });

            // Verify expected state: {1,3,5,7,9,10,11,12,13,14}
            futures::stream::iter(expected.iter().copied())
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let r =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            r.is_some(),
                            "Key {} should exist after phase 2",
                            i
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify deleted keys don't exist
            futures::stream::iter([0i64, 2, 4, 6, 8].into_iter())
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let r =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            r.is_none(),
                            "Key {} should NOT exist after delete",
                            i
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Phase 3: More interleaving
            btree.kill_internal(&name, &key![1]).await.unwrap();
            expected.remove(&1);
            btree
                .set_internal(
                    &name,
                    &key![100],
                    NodeData::with_value(Value::Integer(100)),
                )
                .await
                .unwrap();
            expected.insert(100);
            btree.kill_internal(&name, &key![14]).await.unwrap();
            expected.remove(&14);

            // Final verification
            futures::stream::iter(expected.iter().copied())
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let r =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            r.is_some(),
                            "Key {} should exist in final state",
                            i
                        );
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }

        // === Namespace Tests ===

        /// Test KILL on Local namespace (ephemeral variables).
        ///
        /// Verifies that KILL works correctly for local variables, not just globals.
        #[tokio::test]
        async fn kill_local_namespace() {
            let btree = BTree::default();
            let name = local!("LOCAL");

            // Insert into local: LOCAL(1), LOCAL(1,2), LOCAL(1,2,3)
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("local_parent".into())),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::String("local_child".into())),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::String(
                        "local_grandchild".into(),
                    )),
                )
                .await
                .unwrap();

            // Kill LOCAL(1) - should delete entire subtree
            btree.kill_internal(&name, &key![1]).await.unwrap();

            assert_key_not_exists(&btree, &name, &key![1]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2]).await;
            assert_key_not_exists(&btree, &name, &key![1, 2, 3]).await;
        }

        /// Test that Global and Local namespaces are separate.
        ///
        /// KILL on a global should not affect local with same name, and vice versa.
        #[tokio::test]
        async fn kill_namespaces_are_separate() {
            let btree = BTree::default();
            let global = global!("VAR");
            let local = local!("VAR");

            // Insert same keys into both namespaces
            btree
                .set_internal(
                    &global,
                    &key![1, 2],
                    NodeData::with_value(Value::String("global".into())),
                )
                .await
                .unwrap();

            btree
                .set_internal(
                    &local,
                    &key![1, 2],
                    NodeData::with_value(Value::String("local".into())),
                )
                .await
                .unwrap();

            // KILL global ^VAR(1)
            btree.kill_internal(&global, &key![1]).await.unwrap();

            // Global should be gone
            assert_key_not_exists(&btree, &global, &key![1]).await;
            assert_key_not_exists(&btree, &global, &key![1, 2]).await;

            // Local should still exist
            assert_key_exists(
                &btree,
                &local,
                &key![1, 2],
                Some(Value::String("local".into())),
            )
            .await;

            // Now KILL local VAR(1)
            btree.kill_internal(&local, &key![1]).await.unwrap();

            // Local should also be gone now
            assert_key_not_exists(&btree, &local, &key![1]).await;
            assert_key_not_exists(&btree, &local, &key![1, 2]).await;
        }

        // === Stress Tests ===

        /// Stress test with min_degree=2 (smallest valid B-tree).
        ///
        /// With min_degree=2, each node can have 1-3 keys, making
        /// rebalancing operations extremely frequent.
        #[tokio::test]
        async fn min_degree_2_stress() {
            let btree = Arc::new(BTree::new(2).unwrap()); // min_keys=1, max_keys=3
            let name = global!("MINDEG2");

            // Insert 50 keys
            futures::stream::iter(0..50i64)
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        btree
                            .set_internal(
                                &name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let stats_after_insert = btree.stats().await;
            assert!(stats_after_insert.splits > 0, "Should have splits");
            assert!(
                stats_after_insert.height >= 3,
                "Should have 3+ levels with 50 keys"
            );

            // Delete every third key
            futures::stream::iter((0..50i64).filter(|i| i % 3 == 0))
                .for_each(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        btree.kill_internal(&name, &key![i]).await.unwrap();
                    }
                })
                .await;

            let stats_after_delete = btree.stats().await;
            assert!(
                stats_after_delete.merges > 0,
                "Should have merges after deletions"
            );

            // Verify remaining keys (not divisible by 3)
            futures::stream::iter((0..50i64).filter(|i| i % 3 != 0))
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let r =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(
                            r.is_some(),
                            "Key {} should exist (not deleted)",
                            i
                        );
                        assert_eq!(r.unwrap().value, Some(Value::Integer(i)));
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify deleted keys are gone
            futures::stream::iter((0..50i64).filter(|i| i % 3 == 0))
                .then(|i| {
                    let btree = Arc::clone(&btree);
                    let name = name.clone();
                    async move {
                        let r =
                            btree.get_internal(&name, &key![i]).await.unwrap();
                        assert!(r.is_none(), "Key {} should be deleted", i);
                    }
                })
                .collect::<Vec<_>>()
                .await;
        }
    }

    mod data_internal_tests {
        use rumps_types::{key, DataStatus, Value};

        use super::*;
        use crate::node::NodeData;

        // === Basic States ===

        #[tokio::test]
        async fn nonexistent_variable() {
            let btree = BTree::default();
            let name = global!("PATIENT");
            let key = key![123];

            let status = btree.data_internal(&name, &key).await.unwrap();
            assert_eq!(status, DataStatus::NoData);
        }

        #[tokio::test]
        async fn nonexistent_key() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            btree
                .set_internal(
                    &name,
                    &key![100],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            let status = btree.data_internal(&name, &key![200]).await.unwrap();
            assert_eq!(status, DataStatus::NoData);
        }

        #[tokio::test]
        async fn has_value_only() {
            let btree = BTree::default();
            let name = global!("VAR");
            let key = key![1];

            btree
                .set_internal(
                    &name,
                    &key,
                    NodeData::with_value(Value::String("leaf".into())),
                )
                .await
                .unwrap();

            let status = btree.data_internal(&name, &key).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);
        }

        #[tokio::test]
        async fn has_descendants_only() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Set a nested key - this creates ancestor with has_descendants=true
            btree
                .set_internal(
                    &name,
                    &key![1, "CHILD"],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            // Parent key [1] should have descendants but no value
            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasDescendants);
        }

        #[tokio::test]
        async fn has_both_value_and_descendants() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Set parent with value
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("parent".into())),
                )
                .await
                .unwrap();

            // Set child - this updates parent's has_descendants
            btree
                .set_internal(
                    &name,
                    &key![1, "CHILD"],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            // Parent should have both value and descendants
            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::Both);
        }

        // === Namespace Tests ===

        #[tokio::test]
        async fn local_variable() {
            let btree = BTree::default();
            let name = local!("TEMP");
            let key = key!["A"];

            btree
                .set_internal(
                    &name,
                    &key,
                    NodeData::with_value(Value::Boolean(true)),
                )
                .await
                .unwrap();

            let status = btree.data_internal(&name, &key).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);
        }

        #[tokio::test]
        async fn namespaces_separate() {
            let btree = BTree::default();
            let global = global!("VAR");
            let local = local!("VAR");
            let key = key![1];

            // Set only in global
            btree
                .set_internal(
                    &global,
                    &key,
                    NodeData::with_value(Value::Integer(42)),
                )
                .await
                .unwrap();

            // Global has value
            let status = btree.data_internal(&global, &key).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);

            // Local does not exist
            let status = btree.data_internal(&local, &key).await.unwrap();
            assert_eq!(status, DataStatus::NoData);
        }

        // === Hierarchical Tests ===

        #[tokio::test]
        async fn deep_hierarchy() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            // Create deep structure: ^PATIENT(1,"NAME","FIRST") = "John"
            btree
                .set_internal(
                    &name,
                    &key![1, "NAME", "FIRST"],
                    NodeData::with_value(Value::String("John".into())),
                )
                .await
                .unwrap();

            // Leaf has value only
            let status = btree
                .data_internal(&name, &key![1, "NAME", "FIRST"])
                .await
                .unwrap();
            assert_eq!(status, DataStatus::HasValue);

            // Intermediate nodes have descendants only
            let status =
                btree.data_internal(&name, &key![1, "NAME"]).await.unwrap();
            assert_eq!(status, DataStatus::HasDescendants);

            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasDescendants);

            // Non-existent sibling path
            let status =
                btree.data_internal(&name, &key![1, "DOB"]).await.unwrap();
            assert_eq!(status, DataStatus::NoData);
        }

        #[tokio::test]
        async fn multiple_children() {
            let btree = BTree::default();
            let name = global!("DATA");

            // Parent with multiple children
            btree
                .set_internal(
                    &name,
                    &key![1, "A"],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, "B"],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, "C"],
                    NodeData::with_value(Value::Integer(3)),
                )
                .await
                .unwrap();

            // Parent has descendants
            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasDescendants);

            // Each child has value
            let status =
                btree.data_internal(&name, &key![1, "A"]).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);
            let status =
                btree.data_internal(&name, &key![1, "B"]).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);
            let status =
                btree.data_internal(&name, &key![1, "C"]).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);
        }

        // === State Transitions ===

        #[tokio::test]
        async fn after_kill_becomes_no_data() {
            let btree = BTree::default();
            let name = global!("VAR");
            let key = key![1];

            btree
                .set_internal(
                    &name,
                    &key,
                    NodeData::with_value(Value::String("val".into())),
                )
                .await
                .unwrap();

            let status = btree.data_internal(&name, &key).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);

            btree.kill_internal(&name, &key).await.unwrap();

            let status = btree.data_internal(&name, &key).await.unwrap();
            assert_eq!(status, DataStatus::NoData);
        }

        #[tokio::test]
        async fn kill_child_updates_parent() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Parent with value + child
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("parent".into())),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, "CHILD"],
                    NodeData::with_value(Value::String("child".into())),
                )
                .await
                .unwrap();

            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::Both);

            // Kill the child
            btree.kill_internal(&name, &key![1, "CHILD"]).await.unwrap();

            // Parent should now have value only
            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);
        }

        #[tokio::test]
        async fn kill_subtree_updates_ancestor() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Deep structure
            btree
                .set_internal(
                    &name,
                    &key![1, "A", "X"],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasDescendants);

            // Kill intermediate node (removes subtree)
            btree.kill_internal(&name, &key![1, "A"]).await.unwrap();

            // Ancestor should now have no data
            let status = btree.data_internal(&name, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::NoData);
        }
    }

    mod order_internal_tests {
        use rumps_types::{key, Key, Value};

        use super::*;
        use crate::node::NodeData;

        // === Empty/Nonexistent Cases ===

        #[tokio::test]
        async fn empty_tree_returns_none() {
            let btree = BTree::default();
            let name = global!("VAR");

            // No keys in tree, should return None
            let result = btree.order_internal(&name, None).await.unwrap();
            assert_eq!(result, None);
        }

        #[tokio::test]
        async fn nonexistent_variable_returns_none() {
            let btree = BTree::default();
            let name = global!("VAR");
            let other = global!("OTHER");

            // Add key to VAR
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            // Query OTHER which doesn't exist
            let result = btree.order_internal(&other, None).await.unwrap();
            assert_eq!(result, None);
        }

        // === First Key (None after) ===

        #[tokio::test]
        async fn first_key_single_entry() {
            let btree = BTree::default();
            let name = global!("VAR");

            btree
                .set_internal(
                    &name,
                    &key![42],
                    NodeData::with_value(Value::Integer(42)),
                )
                .await
                .unwrap();

            let result = btree.order_internal(&name, None).await.unwrap();
            assert_eq!(result, Some(key![42]));
        }

        #[tokio::test]
        async fn first_key_multiple_entries() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert in non-sorted order
            btree
                .set_internal(
                    &name,
                    &key![30],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();

            // Should return the smallest key
            let result = btree.order_internal(&name, None).await.unwrap();
            assert_eq!(result, Some(key![10]));
        }

        // === Successor Key (Some after) ===

        #[tokio::test]
        async fn successor_existing_key() {
            let btree = BTree::default();
            let name = global!("VAR");

            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![30],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();

            // Successor of 10 is 20
            let result =
                btree.order_internal(&name, Some(&key![10])).await.unwrap();
            assert_eq!(result, Some(key![20]));

            // Successor of 20 is 30
            let result =
                btree.order_internal(&name, Some(&key![20])).await.unwrap();
            assert_eq!(result, Some(key![30]));
        }

        #[tokio::test]
        async fn successor_nonexistent_key() {
            let btree = BTree::default();
            let name = global!("VAR");

            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![30],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();

            // Key 20 doesn't exist, successor should be 30
            let result =
                btree.order_internal(&name, Some(&key![20])).await.unwrap();
            assert_eq!(result, Some(key![30]));

            // Key 5 doesn't exist, successor should be 10
            let result =
                btree.order_internal(&name, Some(&key![5])).await.unwrap();
            assert_eq!(result, Some(key![10]));
        }

        #[tokio::test]
        async fn successor_last_key_returns_none() {
            let btree = BTree::default();
            let name = global!("VAR");

            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();

            // Successor of last key is None
            let result =
                btree.order_internal(&name, Some(&key![20])).await.unwrap();
            assert_eq!(result, None);
        }

        #[tokio::test]
        async fn successor_past_last_key_returns_none() {
            let btree = BTree::default();
            let name = global!("VAR");

            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();

            // Key 100 is past all keys
            let result =
                btree.order_internal(&name, Some(&key![100])).await.unwrap();
            assert_eq!(result, None);
        }

        // === Hierarchical Keys ===

        #[tokio::test]
        async fn hierarchical_keys_sorted_correctly() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            // Insert hierarchical keys
            btree
                .set_internal(
                    &name,
                    &key![1, "NAME"],
                    NodeData::with_value(Value::String("John".into())),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, "DOB"],
                    NodeData::with_value(Value::String("1990-01-01".into())),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![2, "NAME"],
                    NodeData::with_value(Value::String("Jane".into())),
                )
                .await
                .unwrap();

            // The ancestor key![1] was created automatically
            // Order: key![1] < key![1, "DOB"] < key![1, "NAME"] < key![2] < key![2, "NAME"]

            let first = btree.order_internal(&name, None).await.unwrap();
            assert_eq!(first, Some(key![1]));

            let second =
                btree.order_internal(&name, Some(&key![1])).await.unwrap();
            assert_eq!(second, Some(key![1, "DOB"]));

            let third = btree
                .order_internal(&name, Some(&key![1, "DOB"]))
                .await
                .unwrap();
            assert_eq!(third, Some(key![1, "NAME"]));

            let fourth = btree
                .order_internal(&name, Some(&key![1, "NAME"]))
                .await
                .unwrap();
            assert_eq!(fourth, Some(key![2]));

            let fifth =
                btree.order_internal(&name, Some(&key![2])).await.unwrap();
            assert_eq!(fifth, Some(key![2, "NAME"]));

            let sixth = btree
                .order_internal(&name, Some(&key![2, "NAME"]))
                .await
                .unwrap();
            assert_eq!(sixth, None);
        }

        // === Namespace Separation ===

        #[tokio::test]
        async fn namespaces_are_separate() {
            let btree = BTree::default();
            let global = global!("VAR");
            let local = local!("VAR");

            btree
                .set_internal(
                    &global,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &local,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();

            // Global only sees its own keys
            let g_first = btree.order_internal(&global, None).await.unwrap();
            assert_eq!(g_first, Some(key![10]));
            let g_next = btree
                .order_internal(&global, Some(&key![10]))
                .await
                .unwrap();
            assert_eq!(g_next, None);

            // Local only sees its own keys
            let l_first = btree.order_internal(&local, None).await.unwrap();
            assert_eq!(l_first, Some(key![20]));
            let l_next =
                btree.order_internal(&local, Some(&key![20])).await.unwrap();
            assert_eq!(l_next, None);
        }

        // === Full Iteration ===

        #[tokio::test]
        async fn full_iteration() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert keys
            let keys_to_insert = vec![
                key![5],
                key![3],
                key![8],
                key![1],
                key![9],
                key![2],
                key![7],
                key![4],
                key![6],
            ];

            futures::stream::iter(keys_to_insert.iter())
                .then(|k| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                k,
                                NodeData::with_value(Value::Integer(1)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Iterate through all keys using order_internal
            let mut collected = Vec::new();
            let mut current = btree.order_internal(&name, None).await.unwrap();

            while let Some(k) = current {
                collected.push(k.clone());
                current = btree.order_internal(&name, Some(&k)).await.unwrap();
            }

            // Should be in sorted order
            let expected: Vec<Key> = (1..=9).map(|i| key![i as i64]).collect();
            assert_eq!(collected, expected);
        }

        // === Tree Split Scenarios ===

        #[tokio::test]
        async fn iteration_across_tree_splits() {
            let btree = BTree::new(2).unwrap(); // Small degree forces splits
            let name = global!("VAR");

            // Insert enough keys to cause splits
            futures::stream::iter(0..20)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i as i64],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify full iteration still works
            let mut collected = Vec::new();
            let mut current = btree.order_internal(&name, None).await.unwrap();

            while let Some(k) = current {
                collected.push(k.clone());
                current = btree.order_internal(&name, Some(&k)).await.unwrap();
            }

            let expected: Vec<Key> = (0..20).map(|i| key![i as i64]).collect();
            assert_eq!(collected, expected);
        }

        // === Extended Collation Order ===

        #[tokio::test]
        async fn extended_collation_order() {
            let btree = BTree::default();
            let name = global!("VAR");

            // Insert different types (boolean < number < string)
            btree
                .set_internal(
                    &name,
                    &key!["Z"],
                    NodeData::with_value(Value::String("string".into())),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![false],
                    NodeData::with_value(Value::Boolean(false)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![100],
                    NodeData::with_value(Value::Integer(100)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![true],
                    NodeData::with_value(Value::Boolean(true)),
                )
                .await
                .unwrap();

            // Order: false < true < 100 < "Z"
            let first = btree.order_internal(&name, None).await.unwrap();
            assert_eq!(first, Some(key![false]));

            let second = btree
                .order_internal(&name, Some(&key![false]))
                .await
                .unwrap();
            assert_eq!(second, Some(key![true]));

            let third = btree
                .order_internal(&name, Some(&key![true]))
                .await
                .unwrap();
            assert_eq!(third, Some(key![100]));

            let fourth =
                btree.order_internal(&name, Some(&key![100])).await.unwrap();
            assert_eq!(fourth, Some(key!["Z"]));

            let fifth =
                btree.order_internal(&name, Some(&key!["Z"])).await.unwrap();
            assert_eq!(fifth, None);
        }

        // === Stress Tests ===

        #[tokio::test]
        async fn stress_many_keys() {
            let btree = BTree::default();
            let name = global!("STRESS");

            // Insert 500 keys in random order
            let mut keys: Vec<i64> = (0..500).collect();
            // Simple shuffle using deterministic pattern
            keys.sort_by_key(|&k| (k * 7919) % 500);

            futures::stream::iter(keys.iter())
                .then(|&i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Verify iteration returns all keys in order
            let mut collected = Vec::new();
            let mut current = btree.order_internal(&name, None).await.unwrap();

            while let Some(k) = current {
                collected.push(k.clone());
                current = btree.order_internal(&name, Some(&k)).await.unwrap();
            }

            assert_eq!(collected.len(), 500);

            // Verify ordering
            collected.windows(2).for_each(|w| {
                assert!(
                    w[0] < w[1],
                    "Keys out of order: {:?} >= {:?}",
                    w[0],
                    w[1]
                );
            });
        }

        #[tokio::test]
        async fn stress_deep_hierarchy() {
            let btree = BTree::default();
            let name = global!("DEEP");

            // Create deeply nested structure
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3, 4, 5],
                    NodeData::with_value(Value::Integer(12345)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3, 4, 6],
                    NodeData::with_value(Value::Integer(12346)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3, 5],
                    NodeData::with_value(Value::Integer(1235)),
                )
                .await
                .unwrap();

            // Iterate and verify
            let mut collected = Vec::new();
            let mut current = btree.order_internal(&name, None).await.unwrap();

            while let Some(k) = current {
                collected.push(k.clone());
                current = btree.order_internal(&name, Some(&k)).await.unwrap();
            }

            // Should include ancestors + leaf keys
            assert!(collected.contains(&key![1]));
            assert!(collected.contains(&key![1, 2]));
            assert!(collected.contains(&key![1, 2, 3]));
            assert!(collected.contains(&key![1, 2, 3, 4]));
            assert!(collected.contains(&key![1, 2, 3, 4, 5]));
            assert!(collected.contains(&key![1, 2, 3, 4, 6]));
            assert!(collected.contains(&key![1, 2, 3, 5]));

            // Verify order
            collected.windows(2).for_each(|w| {
                assert!(
                    w[0] < w[1],
                    "Keys out of order: {:?} >= {:?}",
                    w[0],
                    w[1]
                );
            });
        }
    }

    /// Tests for the `collects_internal` (RUMPS `$COLLECT`) operation.
    ///
    /// `$COLLECT` is a RUMPS extension providing stream-based iteration over
    /// tree entries with filtering and transformation.
    mod collects_internal_tests {
        use super::*;

        // === Basic Iteration Tests ===

        #[tokio::test]
        async fn empty_tree_returns_empty_stream() {
            let btree = BTree::default();
            let name = global!("EMPTY");

            let collected: Vec<Key> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,            // Accept all
                    |k, _| Some(k.clone()), // Extract key
                )
                .try_collect()
                .await
                .unwrap();

            assert!(collected.is_empty());
        }

        #[tokio::test]
        async fn nonexistent_variable_returns_empty_stream() {
            let btree = BTree::default();
            let name = global!("EXISTS");
            let other = global!("OTHER");

            // Set up one variable
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            // Collect from a different variable
            let collected: Vec<Key> = btree
                .collects_internal(
                    &other,
                    None,
                    |_, _| true,
                    |k, _| Some(k.clone()),
                )
                .try_collect()
                .await
                .unwrap();

            assert!(collected.is_empty());
        }

        #[tokio::test]
        async fn single_entry() {
            let btree = BTree::default();
            let name = global!("SINGLE");

            btree
                .set_internal(
                    &name,
                    &key![42],
                    NodeData::with_value(Value::Integer(42)),
                )
                .await
                .unwrap();

            let collected: Vec<(Key, Value)> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,
                    |k, d| d.value.clone().map(|v| (k.clone(), v)),
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected.len(), 1);
            assert_eq!(collected[0].0, key![42]);
            assert_eq!(collected[0].1, Value::Integer(42));
        }

        #[tokio::test]
        async fn multiple_entries_in_order() {
            let btree = BTree::default();
            let name = global!("MULTI");

            // Insert out of order
            btree
                .set_internal(
                    &name,
                    &key![30],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();

            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            // Should be in sorted order
            assert_eq!(collected, vec![10, 20, 30]);
        }

        // === Start Key Tests ===

        #[tokio::test]
        async fn start_from_specific_key() {
            let btree = BTree::default();
            let name = global!("START");

            futures::stream::iter(1..=5)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Start from key 2 (should get 3, 4, 5)
            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    Some(&key![2]),
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![3, 4, 5]);
        }

        #[tokio::test]
        async fn start_from_nonexistent_key() {
            let btree = BTree::default();
            let name = global!("STARTNE");

            futures::stream::iter(vec![10, 20, 30])
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Start from key 15 (doesn't exist, should get 20, 30)
            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    Some(&key![15]),
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![20, 30]);
        }

        #[tokio::test]
        async fn start_past_last_key() {
            let btree = BTree::default();
            let name = global!("PASTEND");

            btree
                .set_internal(
                    &name,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();

            // Start from key 100 (past all keys)
            let collected: Vec<Key> = btree
                .collects_internal(
                    &name,
                    Some(&key![100]),
                    |_, _| true,
                    |k, _| Some(k.clone()),
                )
                .try_collect()
                .await
                .unwrap();

            assert!(collected.is_empty());
        }

        // === Predicate Filtering Tests ===

        #[tokio::test]
        async fn predicate_filters_entries() {
            let btree = BTree::default();
            let name = global!("FILTER");

            futures::stream::iter(1..=10)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Only collect even numbers
            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, d| {
                        d.value
                            .as_ref()
                            .map(|v| match v {
                                Value::Integer(i) => i % 2 == 0,
                                _ => false,
                            })
                            .unwrap_or(false)
                    },
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![2, 4, 6, 8, 10]);
        }

        #[tokio::test]
        async fn predicate_false_skips_entry() {
            let btree = BTree::default();
            let name = global!("SKIPSOME");

            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::String("skip".into())),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![2],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![3],
                    NodeData::with_value(Value::String("skip".into())),
                )
                .await
                .unwrap();

            // Only collect integers
            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, d| matches!(d.value.as_ref(), Some(Value::Integer(_))),
                    |_, d| match d.value.as_ref() {
                        Some(Value::Integer(i)) => Some(*i),
                        _ => None,
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![2]);
        }

        // === Extract Function Tests ===

        #[tokio::test]
        async fn extract_returns_none_skips_entry() {
            let btree = BTree::default();
            let name = global!("EXTRACTNONE");

            futures::stream::iter(1..=5)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Extract only returns Some for values > 2
            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true, // Accept all
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) if *i > 2 => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![3, 4, 5]);
        }

        #[tokio::test]
        async fn extract_transforms_entries() {
            let btree = BTree::default();
            let name = global!("TRANSFORM");

            btree
                .set_internal(
                    &name,
                    &key!["Alice"],
                    NodeData::with_value(Value::Integer(25)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key!["Bob"],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();

            // Transform to formatted strings
            let collected: Vec<String> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,
                    |k, d| {
                        d.value.as_ref().map(|v| {
                            format!(
                                "{}: {}",
                                k.get(0).map_or("?".to_string(), |s| {
                                    match s {
                                        rumps_types::Subscript::String(s) => {
                                            s.clone()
                                        }
                                        _ => "?".to_string(),
                                    }
                                }),
                                match v {
                                    Value::Integer(i) => i.to_string(),
                                    _ => "?".to_string(),
                                }
                            )
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec!["Alice: 25", "Bob: 30"]);
        }

        #[tokio::test]
        async fn extract_with_owned_nodedata() {
            let btree = BTree::default();
            let name = global!("OWNED");

            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::new(Some(Value::Integer(10)), true),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![2],
                    NodeData::new(Some(Value::Integer(20)), false),
                )
                .await
                .unwrap();
            btree
                .set_internal(&name, &key![3], NodeData::new(None, true))
                .await
                .unwrap();

            // Clone the entire NodeData to take ownership, then transform
            let collected: Vec<(Key, NodeData)> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,
                    |k, d| Some((k.clone(), d.clone())), // Clone to get owned NodeData
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected.len(), 3);

            // Verify we have owned copies with correct data
            let (k1, d1) = &collected[0];
            assert_eq!(*k1, key![1]);
            assert_eq!(d1.value, Some(Value::Integer(10)));
            assert!(d1.has_descendants);

            let (k2, d2) = &collected[1];
            assert_eq!(*k2, key![2]);
            assert_eq!(d2.value, Some(Value::Integer(20)));
            assert!(!d2.has_descendants);

            let (k3, d3) = &collected[2];
            assert_eq!(*k3, key![3]);
            assert_eq!(d3.value, None);
            assert!(d3.has_descendants);

            // Demonstrate we can mutate the owned data (wouldn't work with refs)
            let mut modified: Vec<NodeData> = collected
                .into_iter()
                .map(|(_, mut d)| {
                    // Double any integer values
                    d.value = d.value.map(|v| match v {
                        Value::Integer(i) => Value::Integer(i * 2),
                        other => other,
                    });
                    d
                })
                .collect();

            assert_eq!(modified[0].value, Some(Value::Integer(20)));
            assert_eq!(modified[1].value, Some(Value::Integer(40)));
            assert_eq!(modified[2].value, None);

            // Can also modify has_descendants
            modified.iter_mut().for_each(|d| d.has_descendants = false);
            assert!(modified.iter().all(|d| !d.has_descendants));
        }

        // === Hierarchical Data Tests ===

        #[tokio::test]
        async fn hierarchical_keys() {
            let btree = BTree::default();
            let name = global!("PATIENT");

            // Set up patient data (hierarchical)
            btree
                .set_internal(
                    &name,
                    &key![123, "NAME"],
                    NodeData::with_value(Value::String("John Doe".into())),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![123, "DOB"],
                    NodeData::with_value(Value::String("1990-01-01".into())),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![124, "NAME"],
                    NodeData::with_value(Value::String("Jane Smith".into())),
                )
                .await
                .unwrap();

            // Collect all entries with values (skip intermediate nodes)
            let collected: Vec<(Key, String)> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, d| d.value.is_some(),
                    |k, d| {
                        d.value.as_ref().map(|v| {
                            (
                                k.clone(),
                                match v {
                                    Value::String(s) => s.clone(),
                                    _ => "?".to_string(),
                                },
                            )
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected.len(), 3);
            assert!(collected.iter().any(|(k, _)| *k == key![123, "DOB"]));
            assert!(collected.iter().any(|(k, _)| *k == key![123, "NAME"]));
            assert!(collected.iter().any(|(k, _)| *k == key![124, "NAME"]));
        }

        #[tokio::test]
        async fn filter_by_key_depth() {
            let btree = BTree::default();
            let name = global!("DEPTH");

            // Create structure with varying depths
            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::Integer(123)),
                )
                .await
                .unwrap();

            // Collect only depth-2 keys
            let collected: Vec<Key> = btree
                .collects_internal(
                    &name,
                    None,
                    |k, _| k.len() == 2,
                    |k, _| Some(k.clone()),
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![key![1, 2]]);
        }

        // === Namespace Tests ===

        #[tokio::test]
        async fn local_namespace() {
            let btree = BTree::default();
            let name = local!("LOCAL");

            btree
                .set_internal(
                    &name,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![1]);
        }

        #[tokio::test]
        async fn namespaces_are_separate() {
            let btree = BTree::default();
            let g = global!("VAR");
            let l = local!("VAR");

            btree
                .set_internal(
                    &g,
                    &key![1],
                    NodeData::with_value(Value::Integer(100)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &l,
                    &key![1],
                    NodeData::with_value(Value::Integer(200)),
                )
                .await
                .unwrap();

            let global_vals: Vec<i64> = btree
                .collects_internal(
                    &g,
                    None,
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            let local_vals: Vec<i64> = btree
                .collects_internal(
                    &l,
                    None,
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(global_vals, vec![100]);
            assert_eq!(local_vals, vec![200]);
        }

        // === Stream Behavior Tests ===

        #[tokio::test]
        async fn stream_is_lazy() {
            use std::sync::atomic::{AtomicUsize, Ordering};

            let btree = BTree::default();
            let name = global!("LAZY");

            futures::stream::iter(1..=10)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let count = Arc::new(AtomicUsize::new(0));
            let count_clone = Arc::clone(&count);

            // Take only 3 items - should not evaluate all entries
            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    move |_, _| {
                        count_clone.fetch_add(1, Ordering::SeqCst);
                        true
                    },
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .take(3)
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected, vec![1, 2, 3]);
            // The predicate should have been called exactly 3 times
            assert_eq!(count.load(Ordering::SeqCst), 3);
        }

        #[tokio::test]
        async fn stream_with_tree_splits() {
            let btree = BTree::new(2).unwrap(); // Small degree to force splits
            let name = global!("SPLITS");

            // Insert enough keys to cause multiple splits
            futures::stream::iter(0..50)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            // Should have all 50 values in order
            let expected: Vec<i64> = (0..50).collect();
            assert_eq!(collected, expected);
        }

        // === Stress Tests ===

        #[tokio::test]
        async fn stress_many_entries() {
            let btree = BTree::default();
            let name = global!("STRESS");

            // Insert 100 entries (reduced for test speed)
            futures::stream::iter(0..100)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, _| true,
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            assert_eq!(collected.len(), 100);

            // Verify order
            collected.windows(2).for_each(|w| {
                assert!(
                    w[0] < w[1],
                    "Values out of order: {} >= {}",
                    w[0],
                    w[1]
                );
            });
        }

        #[tokio::test]
        async fn stress_with_filtering() {
            let btree = BTree::default();
            let name = global!("STRESSFILT");

            // Insert 200 entries (reduced for test speed)
            futures::stream::iter(0..200)
                .then(|i| {
                    let btree = &btree;
                    let name = &name;
                    async move {
                        btree
                            .set_internal(
                                name,
                                &key![i],
                                NodeData::with_value(Value::Integer(i)),
                            )
                            .await
                            .unwrap();
                    }
                })
                .collect::<Vec<_>>()
                .await;

            // Collect only multiples of 7
            let collected: Vec<i64> = btree
                .collects_internal(
                    &name,
                    None,
                    |_, d| {
                        d.value
                            .as_ref()
                            .map(|v| match v {
                                Value::Integer(i) => i % 7 == 0,
                                _ => false,
                            })
                            .unwrap_or(false)
                    },
                    |_, d| {
                        d.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                )
                .try_collect()
                .await
                .unwrap();

            // Should have 29 multiples of 7 from 0-199 (0, 7, 14, ..., 196)
            assert_eq!(collected.len(), 29);
            collected.iter().for_each(|&v| {
                assert_eq!(v % 7, 0, "Value {} is not a multiple of 7", v);
            });
        }
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
    use rumps_types::{global, key, Key, Subscript, Value};
    use tokio::runtime::Runtime;

    use crate::btree::BTree;
    use crate::node::NodeData;

    /// Helper function to create a key with the specified depth.
    ///
    /// For depth=3, creates Key([1, 2, 3])
    /// This will result in (depth - 1) ancestors being created.
    fn create_key_at_depth(depth: usize) -> Key {
        (1..=depth).map(|i| Subscript::from(i as i64)).collect()
    }

    /// Benchmark INSERT operations at various depths to show hierarchical semantics
    /// performance characteristics.
    ///
    /// This benchmarks the `set()` operation which internally calls `ensure_ancestors()`
    /// before insertion, demonstrating the time complexity as depth increases.
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
                            let btree = BTree::default();
                            let name = global!("VAR");
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
                    let btree = BTree::default();
                    let name = global!("VAR");

                    // First insert creates all ancestors
                    let k1 = key![1, 2, 3, 4, 100];
                    btree
                        .set_internal(
                            &name,
                            &k1,
                            NodeData::with_value(Value::Integer(42)),
                        )
                        .await
                        .unwrap();

                    // Second insert at same depth should be faster (ancestors exist)
                    let k2 = key![1, 2, 3, 4, 200];
                    btree
                        .set_internal(
                            &name,
                            &k2,
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
                    let btree = BTree::default();
                    let name = global!("VAR");
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
                    let btree = BTree::default();
                    let name = global!("DEEP");
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
                    let btree = BTree::default();
                    let name = global!("SHALLOW");
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
