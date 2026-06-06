//! Runtime `Map` representation.
//!
//! `Map` is an immutable AVL tree over `ValueId` keys. The tree stores shape
//! and traversal order only. Key ordering is delegated to interpreter owned
//! `Ord:compare`, because user instances can define RUMPS ordering.
//!
//! Cloning a `Map` is `O(1)`. Updates path copy `O(log n)` nodes plus the cost
//! of key comparison. `entries`, `keys`, and `values` are `O(n)` and allocate
//! `O(n)` result storage. Comparator dependent operations live in
//! `interpreter::map`.

use std::sync::Arc;

use smallvec::SmallVec;

use super::ValueId;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Map {
    root: Option<Arc<MapNode>>,
    len: usize,
}

impl Map {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn from_root(root: Option<Arc<MapNode>>, len: usize) -> Self {
        Self { root, len }
    }

    pub(crate) fn root(&self) -> Option<Arc<MapNode>> {
        self.root.clone()
    }

    pub(crate) fn entries(&self) -> SmallVec<[(ValueId, ValueId); 8]> {
        fn push(
            n: &Option<Arc<MapNode>>,
            out: &mut SmallVec<[(ValueId, ValueId); 8]>,
        ) {
            if let Some(n) = n {
                push(&n.left, out);
                out.push((n.key, n.val));
                push(&n.right, out);
            }
        }

        let mut out = SmallVec::new();
        push(&self.root, &mut out);
        out
    }

    pub(crate) fn keys(&self) -> SmallVec<[ValueId; 4]> {
        self.entries().into_iter().map(|(k, _)| k).collect()
    }

    pub(crate) fn values(&self) -> SmallVec<[ValueId; 4]> {
        self.entries().into_iter().map(|(_, v)| v).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MapNode {
    key: ValueId,
    val: ValueId,
    left: Option<Arc<Self>>,
    right: Option<Arc<Self>>,
    height: u8,
}

impl MapNode {
    pub(crate) fn new(
        key: ValueId,
        val: ValueId,
        left: Option<Arc<Self>>,
        right: Option<Arc<Self>>,
    ) -> Arc<Self> {
        let h = Self::height(&left)
            .max(Self::height(&right))
            .saturating_add(1);
        Arc::new(Self {
            key,
            val,
            left,
            right,
            height: h,
        })
    }

    pub(crate) fn balance(
        key: ValueId,
        val: ValueId,
        left: Option<Arc<Self>>,
        right: Option<Arc<Self>>,
    ) -> Arc<Self> {
        let lh = Self::height(&left);
        let rh = Self::height(&right);

        if lh > rh.saturating_add(1) {
            match left {
                Some(l) => {
                    if Self::height(&l.left) >= Self::height(&l.right) {
                        Self::new(
                            l.key,
                            l.val,
                            l.left.clone(),
                            Some(Self::new(key, val, l.right.clone(), right)),
                        )
                    } else {
                        match l.right.clone() {
                            Some(lr) => Self::new(
                                lr.key,
                                lr.val,
                                Some(Self::new(
                                    l.key,
                                    l.val,
                                    l.left.clone(),
                                    lr.left.clone(),
                                )),
                                Some(Self::new(
                                    key,
                                    val,
                                    lr.right.clone(),
                                    right,
                                )),
                            ),
                            None => Self::new(key, val, Some(l), right),
                        }
                    }
                }
                None => Self::new(key, val, None, right),
            }
        } else if rh > lh.saturating_add(1) {
            match right {
                Some(r) => {
                    if Self::height(&r.right) >= Self::height(&r.left) {
                        Self::new(
                            r.key,
                            r.val,
                            Some(Self::new(key, val, left, r.left.clone())),
                            r.right.clone(),
                        )
                    } else {
                        match r.left.clone() {
                            Some(rl) => Self::new(
                                rl.key,
                                rl.val,
                                Some(Self::new(
                                    key,
                                    val,
                                    left,
                                    rl.left.clone(),
                                )),
                                Some(Self::new(
                                    r.key,
                                    r.val,
                                    rl.right.clone(),
                                    r.right.clone(),
                                )),
                            ),
                            None => Self::new(key, val, left, Some(r)),
                        }
                    }
                }
                None => Self::new(key, val, left, None),
            }
        } else {
            Self::new(key, val, left, right)
        }
    }

    pub(crate) fn key(&self) -> ValueId {
        self.key
    }

    pub(crate) fn val(&self) -> ValueId {
        self.val
    }

    pub(crate) fn left(&self) -> Option<Arc<Self>> {
        self.left.clone()
    }

    pub(crate) fn right(&self) -> Option<Arc<Self>> {
        self.right.clone()
    }

    fn height(n: &Option<Arc<Self>>) -> u8 {
        n.as_ref().map_or(0, |n| n.height)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn id(n: u32) -> ValueId {
        ValueId(n)
    }

    fn leaf(k: u32) -> Arc<MapNode> {
        MapNode::new(id(k), id(k + 10), None, None)
    }

    fn assert_root(root: &MapNode, k: u32, l: Option<u32>, r: Option<u32>) {
        assert_eq!(root.key(), id(k));
        assert_eq!(root.left().map(|n| n.key()), l.map(id));
        assert_eq!(root.right().map(|n| n.key()), r.map(id));
    }

    #[test]
    fn map_balance_rotates_left_left() {
        let left = MapNode::new(id(2), id(12), Some(leaf(1)), None);
        let root = MapNode::balance(id(3), id(13), Some(left), None);

        assert_root(&root, 2, Some(1), Some(3));
        assert_eq!(MapNode::height(&Some(root)), 2);
    }

    #[test]
    fn map_balance_rotates_left_right() {
        let left = MapNode::new(id(1), id(11), None, Some(leaf(2)));
        let root = MapNode::balance(id(3), id(13), Some(left), None);

        assert_root(&root, 2, Some(1), Some(3));
        assert_eq!(MapNode::height(&Some(root)), 2);
    }

    #[test]
    fn map_balance_rotates_right_right() {
        let right = MapNode::new(id(2), id(12), None, Some(leaf(3)));
        let root = MapNode::balance(id(1), id(11), None, Some(right));

        assert_root(&root, 2, Some(1), Some(3));
        assert_eq!(MapNode::height(&Some(root)), 2);
    }

    #[test]
    fn map_balance_rotates_right_left() {
        let right = MapNode::new(id(3), id(13), Some(leaf(2)), None);
        let root = MapNode::balance(id(1), id(11), None, Some(right));

        assert_root(&root, 2, Some(1), Some(3));
        assert_eq!(MapNode::height(&Some(root)), 2);
    }
}
