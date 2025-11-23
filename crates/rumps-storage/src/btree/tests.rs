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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::Key;
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::Key;
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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
        use rumps_types::{Key, Value};
        use crate::node::NodeData;

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

    /// Tests for `get_internal` operation.
    ///
    /// These tests verify that `get_internal` correctly retrieves values from the tree,
    /// handles non-existent keys/variables, and works correctly after tree splits.
    ///
    /// NOTE: These tests use `get_internal` directly since transaction support
    /// (Phase 5) is not yet implemented. Tests for the public `get` API with
    /// transaction context will be added once Phase 5 is complete.
    mod get_internal_tests {
        use super::*;

        #[tokio::test]
        async fn nonexistent_variable() {
            use rumps_types::{Key, Name};

            let btree = BTree::new(3).unwrap();
            let name = Name::Global("PATIENT".into());
            let key = Key::from(vec![123.into()]);

            // Variable doesn't exist in roots
            let result = btree.get_internal(&name, &key).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn exact_match_single_key() {
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
        async fn exact_match_nested_key() {
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
        async fn nonexistent_key() {
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
        async fn partial_match_no_such_path() {
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
        async fn multiple_keys_same_variable() {
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
        async fn different_variables() {
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
        async fn returns_nodedata_with_flags() {
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
        async fn with_tree_splits() {
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
        async fn deep_nesting() {
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

        #[tokio::test]
        async fn concurrent_reads() {
            use futures::future;
            use rumps_types::{Key, Value};
            use std::sync::Arc;
            use tokio::task;

            let btree = Arc::new(BTree::new(3).unwrap());
            let name = Name::Global("CONCURRENT".into());

            // Insert some test data
            btree
                .set_internal(
                    &name,
                    &Key::from(vec![1.into()]),
                    NodeData::with_value(Value::Integer(100)),
                )
                .await
                .unwrap();
            btree
                .set_internal(
                    &name,
                    &Key::from(vec![2.into()]),
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
                                let key = Key::from(vec![((i % 2) + 1).into()]);
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
    }

    /// Tests for `set_internal` operation and hierarchical semantics.
    ///
    /// These tests verify that `set_internal` correctly maintains the hierarchical
    /// structure of the tree by creating ancestor nodes with `has_descendants` flags.
    ///
    /// NOTE: These tests use `set_internal` directly since transaction support
    /// (Phase 5) is not yet implemented. Tests for the public `set` API will be
    /// added once transaction context is fully functional.
    mod set_internal_tests {
        use super::*;

        #[tokio::test]
        async fn creates_ancestors() {
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
        async fn deep_nesting_creates_all_ancestors() {
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
        async fn intermediate_node_becomes_both() {
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
        async fn preserves_has_descendants_on_update() {
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
        async fn multiple_children_same_parent() {
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
        async fn sibling_paths() {
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
        async fn concurrent_ancestor_creation() {
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
    use crate::node::NodeData;
    use rumps_types::{Key, Name, Value};

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
