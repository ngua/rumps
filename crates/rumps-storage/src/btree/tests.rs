#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use futures::{future, StreamExt};
    use rumps_types::{key, Value};
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
        assert_eq!(btree.node_count().await, 0);
        assert_eq!(btree.allocator.peek_next().await, NodeId::from(0));
    }

    #[tokio::test]
    async fn test_btree_node_count() {
        let btree = BTree::default();
        assert_eq!(btree.node_count().await, 0);
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

        future::try_join_all(handles).await.unwrap();
    }

    #[tokio::test]
    async fn test_writer_blocks_readers() {
        let btree = Arc::new(BTree::default());

        let write_guard = btree.nodes.write().await;

        let btree_clone = Arc::clone(&btree);
        let read_task = tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let _count = btree_clone.node_count().await;
            start.elapsed()
        });

        sleep(Duration::from_millis(10)).await;
        drop(write_guard);

        let elapsed = read_task.await.unwrap();
        assert!(elapsed >= Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_create_tree() {
        let btree = BTree::default();
        let root = btree.create_tree().await.unwrap();

        assert_eq!(btree.node_count().await, 1);
        let stats = btree.stats().await;
        assert_eq!(stats.height, 1);

        // Root should be an empty leaf
        let node = btree.find_node(root).await.unwrap();
        assert!(node.is_leaf);
        assert!(node.keys.is_empty());
    }

    #[tokio::test]
    async fn test_create_multiple_trees() {
        let btree = BTree::default();

        let root1 = btree.create_tree().await.unwrap();
        let root2 = btree.create_tree().await.unwrap();
        let root3 = btree.create_tree().await.unwrap();

        // Each tree gets its own root
        assert_ne!(root1, root2);
        assert_ne!(root2, root3);
        assert_eq!(btree.node_count().await, 3);
    }

    #[tokio::test]
    async fn test_delete_tree() {
        let btree = BTree::default();
        let root = btree.create_tree().await.unwrap();

        // Insert some data to create more nodes
        let mut r = root;
        r = btree
            .set_internal(r, &key![1], NodeData::with_value(Value::Integer(1)))
            .await
            .unwrap();
        r = btree
            .set_internal(r, &key![2], NodeData::with_value(Value::Integer(2)))
            .await
            .unwrap();

        let node_count_before = btree.node_count().await;
        assert!(node_count_before >= 1);

        let freed = btree.delete_tree(r).await.unwrap();
        assert!(freed >= 1);
        assert_eq!(btree.node_count().await, 0);
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

        let node_id = NodeId::from(1);
        let test_node = Node::new_leaf();

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(node_id, test_node.clone());
        }

        let result = btree.find_node(node_id).await;
        assert!(result.is_ok());
        let found_node = result.unwrap();
        assert_eq!(found_node.is_leaf, test_node.is_leaf);
        assert_eq!(found_node.keys.len(), test_node.keys.len());
    }

    #[tokio::test]
    async fn test_split_node_leaf() {
        let btree = BTree::default();

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

        let result = btree.split_node(node_id).await;
        assert!(result.is_ok());
        let (median_key, median_value, right_id) = result.unwrap();

        assert_eq!(median_key, key![30]);
        assert_eq!(median_value.value, Some(Value::Integer(30)));

        let left = btree.find_node(node_id).await.unwrap();
        assert_eq!(left.keys.len(), 2);
        assert_eq!(left.keys[0], key![10]);
        assert_eq!(left.keys[1], key![20]);

        let right = btree.find_node(right_id).await.unwrap();
        assert_eq!(right.keys.len(), 2);
        assert_eq!(right.keys[0], key![40]);
        assert_eq!(right.keys[1], key![50]);

        let stats = btree.stats().await;
        assert_eq!(stats.splits, 1);
    }

    #[tokio::test]
    async fn test_merge_nodes_leaf() {
        let btree = BTree::default();

        let left_id = NodeId::from(100);
        let right_id = NodeId::from(101);
        let sep_key = key![30];
        let sep_val = Arc::new(NodeData::with_value(Value::Integer(30)));

        let left_node = Node {
            keys: vec![key![10], key![20]],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(10))),
                Arc::new(NodeData::with_value(Value::Integer(20))),
            ],
            is_leaf: true,
        };

        let right_node = Node {
            keys: vec![key![40], key![50]],
            children: vec![],
            values: vec![
                Arc::new(NodeData::with_value(Value::Integer(40))),
                Arc::new(NodeData::with_value(Value::Integer(50))),
            ],
            is_leaf: true,
        };

        {
            let mut nodes = btree.nodes.write().await;
            nodes.insert(left_id, left_node);
            nodes.insert(right_id, right_node);
        }

        btree
            .merge_nodes(left_id, sep_key, sep_val, right_id)
            .await
            .unwrap();

        let merged = btree.find_node(left_id).await.unwrap();
        assert_eq!(merged.keys.len(), 5);
        assert_eq!(merged.keys[0], key![10]);
        assert_eq!(merged.keys[2], key![30]);
        assert_eq!(merged.keys[4], key![50]);

        assert!(btree.find_node(right_id).await.is_err());

        let stats = btree.stats().await;
        assert_eq!(stats.merges, 1);
    }

    #[cfg(test)]
    mod get_internal_tests {
        use super::*;

        #[tokio::test]
        async fn empty_tree_returns_none() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let result = btree.get_internal(root, &key![123]).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn exact_match() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();
            let value = Value::String("test".into());

            let root = btree
                .set_internal(
                    root,
                    &key![123],
                    NodeData::with_value(value.clone()),
                )
                .await
                .unwrap();

            let result = btree.get_internal(root, &key![123]).await.unwrap();
            assert!(result.is_some());
            assert_eq!(result.unwrap().value, Some(value));
        }

        #[tokio::test]
        async fn nonexistent_key() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![100],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![200],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();

            // Key between existing keys
            assert!(btree
                .get_internal(root, &key![150])
                .await
                .unwrap()
                .is_none());
            // Key before all
            assert!(btree
                .get_internal(root, &key![50])
                .await
                .unwrap()
                .is_none());
            // Key after all
            assert!(btree
                .get_internal(root, &key![300])
                .await
                .unwrap()
                .is_none());
        }

        #[tokio::test]
        async fn multiple_keys() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![2],
                    NodeData::with_value(Value::Integer(2)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![3],
                    NodeData::with_value(Value::Integer(3)),
                )
                .await
                .unwrap();

            assert_eq!(
                btree
                    .get_internal(root, &key![1])
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
                Some(Value::Integer(1))
            );
            assert_eq!(
                btree
                    .get_internal(root, &key![2])
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
                Some(Value::Integer(2))
            );
            assert_eq!(
                btree
                    .get_internal(root, &key![3])
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
                Some(Value::Integer(3))
            );
        }

        #[tokio::test]
        async fn nested_keys() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::String("nested".into())),
                )
                .await
                .unwrap();

            let result =
                btree.get_internal(root, &key![1, 2, 3]).await.unwrap();
            assert!(result.is_some());
            assert_eq!(
                result.unwrap().value,
                Some(Value::String("nested".into()))
            );

            // Ancestor should exist with has_descendants
            let ancestor = btree.get_internal(root, &key![1]).await.unwrap();
            assert!(ancestor.is_some());
            assert!(ancestor.unwrap().has_descendants);
        }

        #[tokio::test]
        async fn after_tree_splits() {
            let btree = Arc::new(BTree::default()); // min_degree=3, max_keys=5
            let root = btree.create_tree().await.unwrap();

            let keys_to_insert = [10i64, 20, 30, 40, 50, 60, 70, 80];
            let r = futures::stream::iter(keys_to_insert.iter())
                .fold(root, |acc, &i| {
                    let bt = Arc::clone(&btree);
                    async move {
                        bt.set_internal(
                            acc,
                            &key![i],
                            NodeData::with_value(Value::Integer(i)),
                        )
                        .await
                        .unwrap()
                    }
                })
                .await;

            // Verify keys are retrievable after splits
            assert_eq!(
                btree
                    .get_internal(r, &key![30])
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
                Some(Value::Integer(30))
            );
            assert_eq!(
                btree
                    .get_internal(r, &key![60])
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
                Some(Value::Integer(60))
            );
        }
    }

    #[cfg(test)]
    mod set_internal_tests {
        use super::*;

        #[tokio::test]
        async fn creates_ancestors() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::String("leaf".into())),
                )
                .await
                .unwrap();

            // Leaf should have value
            let leaf = btree
                .get_internal(root, &key![1, 2, 3])
                .await
                .unwrap()
                .unwrap();
            assert_eq!(leaf.value, Some(Value::String("leaf".into())));

            // Ancestors should exist with has_descendants=true
            let anc1 =
                btree.get_internal(root, &key![1]).await.unwrap().unwrap();
            assert!(anc1.has_descendants);
            assert!(anc1.value.is_none());

            let anc2 = btree
                .get_internal(root, &key![1, 2])
                .await
                .unwrap()
                .unwrap();
            assert!(anc2.has_descendants);
            assert!(anc2.value.is_none());
        }

        #[tokio::test]
        async fn update_existing_key() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(100)),
                )
                .await
                .unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(200)),
                )
                .await
                .unwrap();

            let result =
                btree.get_internal(root, &key![1]).await.unwrap().unwrap();
            assert_eq!(result.value, Some(Value::Integer(200)));
        }

        #[tokio::test]
        async fn preserves_has_descendants_on_update() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            // Create a node with descendants
            let root = btree
                .set_internal(
                    root,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();

            // Parent [1] should have has_descendants=true
            let parent =
                btree.get_internal(root, &key![1]).await.unwrap().unwrap();
            assert!(parent.has_descendants);

            // Set a value on parent
            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            // Parent should still have has_descendants=true AND the new value
            let parent =
                btree.get_internal(root, &key![1]).await.unwrap().unwrap();
            assert!(parent.has_descendants);
            assert_eq!(parent.value, Some(Value::Integer(1)));
        }

        #[tokio::test]
        async fn causes_tree_growth() {
            let btree = Arc::new(BTree::new(2).unwrap()); // Small degree to trigger splits
            let root = btree.create_tree().await.unwrap();

            let initial_height = btree.stats().await.height;

            // Insert many keys to cause splits
            let r = futures::stream::iter(0..20i64)
                .fold(root, |acc, i| {
                    let bt = Arc::clone(&btree);
                    async move {
                        bt.set_internal(
                            acc,
                            &key![i],
                            NodeData::with_value(Value::Integer(i)),
                        )
                        .await
                        .unwrap()
                    }
                })
                .await;

            let final_stats = btree.stats().await;
            assert!(final_stats.splits > 0);
            assert!(final_stats.height > initial_height);

            // All keys should still be retrievable
            futures::stream::iter(0..20i64)
                .for_each(|i| {
                    let bt = Arc::clone(&btree);
                    async move {
                        let result =
                            bt.get_internal(r, &key![i]).await.unwrap();
                        assert!(result.is_some(), "Key {} should exist", i);
                    }
                })
                .await;
        }
    }

    #[cfg(test)]
    mod kill_internal_tests {
        use super::*;

        #[tokio::test]
        async fn kill_single_key() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            let result = btree.kill_internal(root, &key![1]).await.unwrap();

            // Tree might be empty now
            match result {
                Some(r) => {
                    assert!(btree
                        .get_internal(r, &key![1])
                        .await
                        .unwrap()
                        .is_none());
                }
                None => {
                    // Tree is empty, which is valid
                }
            }
        }

        #[tokio::test]
        async fn kill_with_descendants() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            // Create: [1], [1,2], [1,2,3]
            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![1, 2, 3],
                    NodeData::with_value(Value::Integer(123)),
                )
                .await
                .unwrap();

            // Kill [1] should remove [1], [1,2], and [1,2,3]
            let result = btree.kill_internal(root, &key![1]).await.unwrap();

            match result {
                Some(r) => {
                    assert!(btree
                        .get_internal(r, &key![1])
                        .await
                        .unwrap()
                        .is_none());
                    assert!(btree
                        .get_internal(r, &key![1, 2])
                        .await
                        .unwrap()
                        .is_none());
                    assert!(btree
                        .get_internal(r, &key![1, 2, 3])
                        .await
                        .unwrap()
                        .is_none());
                }
                None => {
                    // Tree is empty
                }
            }
        }

        #[tokio::test]
        async fn kill_nonexistent_key() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            // Kill nonexistent key should not affect existing data
            let result = btree.kill_internal(root, &key![999]).await.unwrap();

            assert!(result.is_some());
            let r = result.unwrap();
            assert!(btree.get_internal(r, &key![1]).await.unwrap().is_some());
        }
    }

    #[cfg(test)]
    mod data_internal_tests {
        use rumps_types::DataStatus;

        use super::*;

        #[tokio::test]
        async fn no_data() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let status = btree.data_internal(root, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::NoData);
        }

        #[tokio::test]
        async fn has_value_only() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            let status = btree.data_internal(root, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasValue);
        }

        #[tokio::test]
        async fn has_descendants_only() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            // Create child - parent will have has_descendants but no value
            let root = btree
                .set_internal(
                    root,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();

            let status = btree.data_internal(root, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::HasDescendants);
        }

        #[tokio::test]
        async fn has_both() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            // Create child first
            let root = btree
                .set_internal(
                    root,
                    &key![1, 2],
                    NodeData::with_value(Value::Integer(12)),
                )
                .await
                .unwrap();

            // Set value on parent
            let root = btree
                .set_internal(
                    root,
                    &key![1],
                    NodeData::with_value(Value::Integer(1)),
                )
                .await
                .unwrap();

            let status = btree.data_internal(root, &key![1]).await.unwrap();
            assert_eq!(status, DataStatus::Both);
        }
    }

    #[cfg(test)]
    mod order_internal_tests {
        use super::*;

        #[tokio::test]
        async fn empty_tree() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let result = btree.order_internal(root, None).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn first_key() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();

            let result = btree.order_internal(root, None).await.unwrap();
            assert_eq!(result, Some(key![10]));
        }

        #[tokio::test]
        async fn successor() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![20],
                    NodeData::with_value(Value::Integer(20)),
                )
                .await
                .unwrap();
            let root = btree
                .set_internal(
                    root,
                    &key![30],
                    NodeData::with_value(Value::Integer(30)),
                )
                .await
                .unwrap();

            let result =
                btree.order_internal(root, Some(&key![10])).await.unwrap();
            assert_eq!(result, Some(key![20]));

            let result =
                btree.order_internal(root, Some(&key![20])).await.unwrap();
            assert_eq!(result, Some(key![30]));
        }

        #[tokio::test]
        async fn end_of_iteration() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let root = btree
                .set_internal(
                    root,
                    &key![10],
                    NodeData::with_value(Value::Integer(10)),
                )
                .await
                .unwrap();

            let result =
                btree.order_internal(root, Some(&key![10])).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn iterate_all() {
            let btree = Arc::new(BTree::default());
            let root = btree.create_tree().await.unwrap();

            let keys = [5i64, 15, 25, 35, 45];
            let r = futures::stream::iter(keys.iter())
                .fold(root, |acc, &i| {
                    let bt = Arc::clone(&btree);
                    async move {
                        bt.set_internal(
                            acc,
                            &key![i],
                            NodeData::with_value(Value::Integer(i)),
                        )
                        .await
                        .unwrap()
                    }
                })
                .await;

            // Iterate and collect all keys
            let mut collected = Vec::new();
            let mut cursor = None;

            // Manual iteration since we can't use while loop
            let result =
                btree.order_internal(r, cursor.as_ref()).await.unwrap();
            if let Some(k) = result {
                collected.push(k.clone());
                cursor = Some(k);
            }

            // Continue iteration
            let keys_found = futures::stream::iter(0..10)
                .fold((cursor, collected), |(cur, mut acc), _| {
                    let bt = Arc::clone(&btree);
                    async move {
                        match cur {
                            None => (None, acc),
                            Some(ref c) => {
                                match bt
                                    .order_internal(r, Some(c))
                                    .await
                                    .unwrap()
                                {
                                    None => (None, acc),
                                    Some(k) => {
                                        acc.push(k.clone());
                                        (Some(k), acc)
                                    }
                                }
                            }
                        }
                    }
                })
                .await
                .1;

            assert_eq!(keys_found.len(), 5);
            assert_eq!(keys_found[0], key![5]);
            assert_eq!(keys_found[4], key![45]);
        }
    }

    #[cfg(test)]
    mod collects_internal_tests {
        use super::*;

        #[tokio::test]
        async fn empty_tree() {
            let btree = BTree::default();
            let root = btree.create_tree().await.unwrap();

            let results: Vec<Value> = btree
                .collects_vec_at(
                    root,
                    None,
                    |_, _| true,
                    |_, data| data.value.clone(),
                    None,
                )
                .await
                .unwrap();

            assert!(results.is_empty());
        }

        #[tokio::test]
        async fn collect_all() {
            let btree = Arc::new(BTree::default());
            let root = btree.create_tree().await.unwrap();

            let r = futures::stream::iter(1..=5i64)
                .fold(root, |acc, i| {
                    let bt = Arc::clone(&btree);
                    async move {
                        bt.set_internal(
                            acc,
                            &key![i],
                            NodeData::with_value(Value::Integer(i)),
                        )
                        .await
                        .unwrap()
                    }
                })
                .await;

            let results: Vec<Value> = btree
                .collects_vec_at(
                    r,
                    None,
                    |_, data| data.value.is_some(),
                    |_, data| data.value.clone(),
                    None,
                )
                .await
                .unwrap();

            assert_eq!(results.len(), 5);
        }

        #[tokio::test]
        async fn filter_predicate() {
            let btree = Arc::new(BTree::default());
            let root = btree.create_tree().await.unwrap();

            let r = futures::stream::iter(1..=10i64)
                .fold(root, |acc, i| {
                    let bt = Arc::clone(&btree);
                    async move {
                        bt.set_internal(
                            acc,
                            &key![i],
                            NodeData::with_value(Value::Integer(i)),
                        )
                        .await
                        .unwrap()
                    }
                })
                .await;

            // Only collect even numbers
            let results: Vec<i64> = btree
                .collects_vec_at(
                    r,
                    None,
                    |_, data| {
                        data.value
                            .as_ref()
                            .map(|v| {
                                matches!(v, Value::Integer(i) if i % 2 == 0)
                            })
                            .unwrap_or(false)
                    },
                    |_, data| {
                        data.value.as_ref().and_then(|v| match v {
                            Value::Integer(i) => Some(*i),
                            _ => None,
                        })
                    },
                    None,
                )
                .await
                .unwrap();

            assert_eq!(results, vec![2, 4, 6, 8, 10]);
        }
    }

    #[tokio::test]
    async fn stress_many_keys() {
        let btree = Arc::new(BTree::default());
        let root = btree.create_tree().await.unwrap();

        let num_keys = 100i64;

        // Insert many keys
        let r = futures::stream::iter(0..num_keys)
            .fold(root, |acc, i| {
                let bt = Arc::clone(&btree);
                async move {
                    bt.set_internal(
                        acc,
                        &key![i],
                        NodeData::with_value(Value::Integer(i)),
                    )
                    .await
                    .unwrap()
                }
            })
            .await;

        // Verify all keys
        futures::stream::iter(0..num_keys)
            .for_each(|i| {
                let bt = Arc::clone(&btree);
                async move {
                    let result = bt.get_internal(r, &key![i]).await.unwrap();
                    assert!(result.is_some(), "Key {} should exist", i);
                    assert_eq!(result.unwrap().value, Some(Value::Integer(i)));
                }
            })
            .await;

        let stats = btree.stats().await;
        assert!(stats.splits > 0);
    }

    #[tokio::test]
    async fn stress_nested_keys() {
        let btree = Arc::new(BTree::default());
        let root = btree.create_tree().await.unwrap();

        // Create deeply nested structure
        let r = futures::stream::iter(0..20i64)
            .fold(root, |acc, i| {
                let bt = Arc::clone(&btree);
                async move {
                    bt.set_internal(
                        acc,
                        &key![i, i * 10, i * 100],
                        NodeData::with_value(Value::Integer(i)),
                    )
                    .await
                    .unwrap()
                }
            })
            .await;

        // Verify nested keys
        futures::stream::iter(0..20i64)
            .for_each(|i| {
                let bt = Arc::clone(&btree);
                async move {
                    let result = bt
                        .get_internal(r, &key![i, i * 10, i * 100])
                        .await
                        .unwrap();
                    assert!(result.is_some());
                }
            })
            .await;
    }

    #[tokio::test]
    async fn two_separate_trees() {
        let btree = BTree::default();

        let root1 = btree.create_tree().await.unwrap();
        let root2 = btree.create_tree().await.unwrap();

        // Insert into tree1
        let root1 = btree
            .set_internal(
                root1,
                &key![1],
                NodeData::with_value(Value::String("tree1".into())),
            )
            .await
            .unwrap();

        // Insert into tree2
        let root2 = btree
            .set_internal(
                root2,
                &key![1],
                NodeData::with_value(Value::String("tree2".into())),
            )
            .await
            .unwrap();

        // Each tree has its own data
        let val1 = btree.get_internal(root1, &key![1]).await.unwrap().unwrap();
        let val2 = btree.get_internal(root2, &key![1]).await.unwrap().unwrap();

        assert_eq!(val1.value, Some(Value::String("tree1".into())));
        assert_eq!(val2.value, Some(Value::String("tree2".into())));
    }
}

#[cfg(feature = "bench")]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::manual_try_fold)]
/// Benchmark functions for B-tree operations.
pub mod benches {
    use std::sync::Arc;

    use criterion::{black_box, BatchSize, Criterion};
    use futures::FutureExt;
    use rumps_types::{key, Value};
    use tokio::runtime::Runtime;

    use crate::btree::BTree;
    use crate::node::NodeData;

    /// Runs all B-tree benchmarks.
    pub fn run_benchmarks(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();

        // Benchmark: create_tree / delete_tree lifecycle
        c.bench_function("btree_create_delete_tree", |b| {
            b.iter_batched(
                BTree::default,
                |btree| {
                    rt.block_on(async {
                        let root = btree.create_tree().await.unwrap();
                        btree.delete_tree(root).await.unwrap();
                        black_box(())
                    })
                },
                BatchSize::SmallInput,
            )
        });

        // Benchmark: sequential SET operations using root-based API
        c.bench_function("btree_set_at_sequential", |b| {
            b.iter_batched(
                || {
                    let btree = BTree::default();
                    let root = rt.block_on(btree.create_tree()).unwrap();
                    (btree, root)
                },
                |(btree, root)| {
                    rt.block_on(async {
                        (0..100i64)
                            .fold(Ok(root), |acc, i| {
                                acc.and_then(|r| {
                                    btree
                                        .set_internal(
                                            r,
                                            &key![i],
                                            NodeData::with_value(
                                                Value::Integer(i),
                                            ),
                                        )
                                        .now_or_never()
                                        .unwrap()
                                })
                            })
                            .map(|r| black_box(r))
                            .unwrap()
                    })
                },
                BatchSize::SmallInput,
            )
        });

        // Benchmark: GET on existing key using root-based API
        c.bench_function("btree_get_at_existing", |b| {
            let btree = Arc::new(BTree::default());
            let root = rt.block_on(async {
                let root = btree.create_tree().await.unwrap();
                (0..100i64)
                    .fold(Ok(root), |acc, i| {
                        acc.and_then(|r| {
                            btree
                                .set_internal(
                                    r,
                                    &key![i],
                                    NodeData::with_value(Value::Integer(i)),
                                )
                                .now_or_never()
                                .unwrap()
                        })
                    })
                    .unwrap()
            });

            b.iter(|| {
                rt.block_on(async {
                    let result =
                        btree.get_internal(root, &key![50]).await.unwrap();
                    black_box(result)
                })
            })
        });

        // Benchmark: delete_tree with populated tree
        c.bench_function("btree_delete_tree_populated", |b| {
            b.iter_batched(
                || {
                    let btree = BTree::default();
                    let root = rt.block_on(async {
                        let root = btree.create_tree().await.unwrap();
                        (0..50i64)
                            .fold(Ok(root), |acc, i| {
                                acc.and_then(|r| {
                                    btree
                                        .set_internal(
                                            r,
                                            &key![i],
                                            NodeData::with_value(
                                                Value::Integer(i),
                                            ),
                                        )
                                        .now_or_never()
                                        .unwrap()
                                })
                            })
                            .unwrap()
                    });
                    (btree, root)
                },
                |(btree, root)| {
                    rt.block_on(async {
                        let freed = btree.delete_tree(root).await.unwrap();
                        black_box(freed)
                    })
                },
                BatchSize::SmallInput,
            )
        });
    }
}
