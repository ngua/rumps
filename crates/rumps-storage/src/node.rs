//! Node-related types for the B-tree storage system.
//!
//! This module defines the data structures used to represent nodes in the RUMPS
//! persistent B-tree storage. The primary types are:
//! - [`NodeId`]: Reference to a node (either on-disk page or in-memory index)
//! - [`Node`]: B-tree node structure with complete keys, children, and values
//! - [`NodeData`]: Data stored at each key in the tree
//!
//! # Storage Model
//!
//! RUMPS uses a B-tree for efficient disk-based storage of hierarchical MUMPS data.
//! While the MUMPS query semantics appear trie-like (hierarchical paths like
//! `^PATIENT(123,"NAME")`), the physical storage uses a flat B-tree where:
//!
//! - **Keys**: Complete paths stored as `Key` (e.g., `[123, "NAME"]`)
//! - **Nodes**: Group multiple key-value pairs for efficient disk I/O
//! - **Pages**: Each node fits in a fixed-size disk page (e.g., `4KB`)
//!
//! Example node contents:
//! ```text
//! Node {
//!   keys: [
//!     Key([123, "ADDR"]),
//!     Key([123, "DOB"]),
//!     Key([123, "NAME"]),
//!     Key([124, "NAME"]),
//!   ],
//!   values: [ ... corresponding NodeData ... ],
//! }
//! ```
//!
//! This allows reading many entries in a single disk operation rather than
//! requiring one I/O per hierarchy level as a trie would.

use std::ops::Deref;
use std::sync::Arc;
use std::{fmt, iter};

use rumps_types::{Key, Value};
use serde::de::{self, Deserializer, Visitor};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Inline capacity for node vectors. For `min_degree=3`, max keys = `2*3-1 = 5`.
/// Use `8` for headroom with larger degrees. Re-exported as [`Node::INLINE_CAP`].
const INLINE_CAP: usize = 8;

/// Keys stored in a B-tree node; inlined up to [`Node::INLINE_CAP`].
pub(crate) type KeyVec = SmallVec<[Key; INLINE_CAP]>;

/// Values stored in a B-tree node; inlined up to [`Node::INLINE_CAP`].
pub(crate) type ValueVec = SmallVec<[Arc<NodeData>; INLINE_CAP]>;

/// Child pointers in a B-tree node; `children = keys + 1`.
pub(crate) type ChildVec = SmallVec<[NodeId; INLINE_CAP + 1]>;

/// Identifier for a node in the B-tree.
///
/// `NodeId` serves as an indirect reference to nodes rather than direct ownership
/// via `Box<Node>`. This design choice enables several critical features:
///
/// # Why `NodeId` Instead of `Box<Node>`?
///
/// 1. **Lazy Loading**: For persistent globals, child nodes can be loaded from disk
///    only when accessed, rather than loading entire subtrees into memory.
///
/// 2. **Scalability**: Large databases can exceed available memory. With `NodeId`,
///    only the working set of nodes needs to be resident in memory at any time.
///
/// 3. **Page Cache Integration**: Each `NodeId` maps to a disk page (for globals)
///    or in-memory slot (for locals), enabling LRU eviction and cache management.
///
/// 4. **Unified Model**: The same `Node` structure works for both:
///    - **Globals**: `NodeId` → `PageId` (disk page offset)
///    - **Locals**: `NodeId` → in-memory index in `HashMap`
///
/// 5. **MVCC Support**: Future snapshot isolation can reference different node
///    versions via `NodeId` without duplicating entire subtrees.
///
/// 6. **Compact Serialization**: Serializes as a single `u64` without wrapper overhead
///    thanks to `#[repr(transparent)]` and serde's transparent serialization.
///
/// # Implementation Notes
///
/// The storage layer (in `rumps-storage`) will provide a `NodeManager` or similar
/// abstraction to resolve `NodeId` → `Node` lookups, handling the distinction between
/// in-memory and on-disk storage transparently.
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::node::NodeId;
///
/// // Create a node ID from a page offset
/// let node_id = NodeId::from(42u64);
/// assert_eq!(*node_id, 42);
/// ```
#[repr(transparent)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize
)]
pub(crate) struct NodeId(u64);

impl From<u64> for NodeId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<NodeId> for u64 {
    fn from(id: NodeId) -> Self {
        *id
    }
}

impl Deref for NodeId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({})", **self)
    }
}

/// Runtime representation of a B-tree node with `Arc`-wrapped values.
///
/// Values are wrapped in `Arc<NodeData>` for efficient hierarchy navigation.
/// MUMPS operations like `$DATA`, `$ORDER`, and internal ancestor maintenance
/// frequently check the `has_descendants` flag without needing ownership.
/// Using `Arc` makes these checks cheap; just a reference count increment.
///
/// Serialization works directly on the `SmallVec` and `Arc` fields with no
/// intermediate allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    /// Complete key paths (sorted) stored in this node
    pub(crate) keys: KeyVec,
    /// References to child nodes (empty for leaf nodes)
    pub(crate) children: ChildVec,
    /// Data associated with each key, wrapped in `Arc` for cheap cloning.
    pub(crate) values: ValueVec,
    /// Whether this is a leaf node (no children)
    pub(crate) is_leaf: bool,
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeSeq, SerializeStruct};
        let mut s = ser.serialize_struct("Node", 4)?;
        s.serialize_field("keys", &self.keys)?;
        s.serialize_field("children", &self.children)?;

        struct Values<'a>(&'a ValueVec);
        impl Serialize for Values<'_> {
            fn serialize<S: Serializer>(
                &self,
                ser: S,
            ) -> Result<S::Ok, S::Error> {
                let mut seq = ser.serialize_seq(Some(self.0.len()))?;
                self.0
                    .iter()
                    .try_for_each(|v| seq.serialize_element(v.as_ref()))?;
                seq.end()
            }
        }

        s.serialize_field("values", &Values(&self.values))?;
        s.serialize_field("is_leaf", &self.is_leaf)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct NodeVisitor;

        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = Node;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Node struct")
            }

            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let keys = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("keys"))?;
                let children = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("children"))?;
                let raw: SmallVec<[NodeData; INLINE_CAP]> = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("values"))?;
                let is_leaf = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("is_leaf"))?;

                Ok(Node {
                    keys,
                    children,
                    values: raw.into_iter().map(Arc::new).collect(),
                    is_leaf,
                })
            }
        }

        de.deserialize_struct(
            "Node",
            &["keys", "children", "values", "is_leaf"],
            NodeVisitor,
        )
    }
}

impl Node {
    /// Inline capacity for `SmallVec` fields (`keys`, `values`, `children`).
    pub(crate) const INLINE_CAP: usize = INLINE_CAP;

    /// Creates a new empty leaf node.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::Node;
    ///
    /// let leaf = Node::new_leaf();
    /// assert!(leaf.is_leaf);
    /// assert!(leaf.keys.is_empty());
    /// assert!(leaf.children.is_empty());
    /// ```
    pub(crate) fn new_leaf() -> Self {
        Self {
            keys: SmallVec::new(),
            children: SmallVec::new(),
            values: SmallVec::new(),
            is_leaf: true,
        }
    }

    /// Creates a new empty internal node.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::Node;
    ///
    /// let internal = Node::new_internal();
    /// assert!(!internal.is_leaf);
    /// assert!(internal.keys.is_empty());
    /// ```
    pub(crate) fn new_internal() -> Self {
        Self {
            keys: SmallVec::new(),
            children: SmallVec::new(),
            values: SmallVec::new(),
            is_leaf: false,
        }
    }

    /// Returns the number of keys in this node.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::{Node, NodeData};
    /// use rumps_types::{Key, Value};
    /// use std::sync::Arc;
    ///
    /// let mut node = Node::new_leaf();
    /// node.keys.push(key!["A"]);
    /// node.values.push(Arc::new(NodeData::with_value(Value::Integer(1))));
    ///
    /// assert_eq!(node.len(), 1);
    /// ```
    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    /// Returns `true` if this node has no keys.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::Node;
    ///
    /// let node = Node::new_leaf();
    /// assert!(node.is_empty());
    /// ```
    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Data stored at a node in the RUMPS tree.
///
/// A node can be in one of four states:
/// 1. **Empty**: No value, no descendants (represents deleted/empty node)
/// 2. **Intermediate**: No value, has descendants (internal tree node)
/// 3. **Leaf**: Has value, no descendants (pure data node)
/// 4. **Both**: Has value, has descendants (data node with children)
///
/// # Examples
///
/// ```ignore
/// use rumps_storage::node::NodeData;
/// use rumps_types::Value;
///
/// // Create a leaf node with a value
/// let leaf = NodeData::with_value(Value::Integer(42));
/// assert!(leaf.has_only_value());
///
/// // Create an intermediate node with descendants
/// let intermediate = NodeData::with_descendants();
/// assert!(intermediate.has_only_descendants());
///
/// // Create a node with both value and descendants
/// let both = NodeData::new(Some(Value::String("root".into())), true);
/// assert!(both.has_value() && both.has_descendants);
///
/// // Create an empty node
/// let empty = NodeData::empty();
/// assert!(empty.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeData {
    /// Optional value stored at this node
    pub(crate) value: Option<Value>,
    /// Flag indicating whether this node has descendants
    pub(crate) has_descendants: bool,
}

/// Tag byte for compact binary encoding of NodeData
#[repr(u8)]
enum NodeDataTag {
    /// No value, no descendants
    Empty = 0x00,
    /// No value, has descendants
    Intermediate = 0x01,
    /// Has value, no descendants
    Leaf = 0x02,
    /// Has value, has descendants
    Both = 0x03,
}

impl NodeData {
    /// Creates new NodeData with given value and descendants flag.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    /// use rumps_types::Value;
    ///
    /// let node = NodeData::new(Some(Value::Integer(42)), false);
    /// assert_eq!(node.value, Some(Value::Integer(42)));
    /// assert_eq!(node.has_descendants, false);
    /// ```
    pub(crate) fn new(value: Option<Value>, has_descendants: bool) -> Self {
        Self {
            value,
            has_descendants,
        }
    }

    /// Creates NodeData with value, no descendants (pure leaf).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    /// use rumps_types::Value;
    ///
    /// let leaf = NodeData::with_value(Value::String("hello".into()));
    /// assert!(leaf.has_only_value());
    /// ```
    pub(crate) fn with_value(value: Value) -> Self {
        Self {
            value: Some(value),
            has_descendants: false,
        }
    }

    /// Creates NodeData with no value, has descendants (intermediate).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    ///
    /// let intermediate = NodeData::with_descendants();
    /// assert!(intermediate.has_only_descendants());
    /// ```
    pub(crate) fn with_descendants() -> Self {
        Self {
            value: None,
            has_descendants: true,
        }
    }

    /// Creates an empty NodeData (no value, no descendants).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    ///
    /// let empty = NodeData::empty();
    /// assert!(empty.is_empty());
    /// ```
    pub(crate) fn empty() -> Self {
        Self {
            value: None,
            has_descendants: false,
        }
    }

    /// Returns `true` if no value and no descendants.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    ///
    /// let empty = NodeData::empty();
    /// assert!(empty.is_empty());
    ///
    /// let leaf = NodeData::with_value(42);
    /// assert!(!leaf.is_empty());
    /// ```
    pub(crate) fn is_empty(&self) -> bool {
        self.value.is_none() && !self.has_descendants
    }

    /// Returns `true` if has a value.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    /// use rumps_types::Value;
    ///
    /// let leaf = NodeData::with_value(Value::Boolean(true));
    /// assert!(leaf.has_value());
    ///
    /// let intermediate = NodeData::with_descendants();
    /// assert!(!intermediate.has_value());
    /// ```
    pub(crate) fn has_value(&self) -> bool {
        self.value.is_some()
    }

    /// Returns `true` if has value but no descendants (pure leaf).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    /// use rumps_types::Value;
    ///
    /// let leaf = NodeData::with_value(Value::Integer(42));
    /// assert!(leaf.has_only_value());
    ///
    /// let both = NodeData::new(Some(Value::Integer(42)), true);
    /// assert!(!both.has_only_value());
    /// ```
    pub(crate) fn has_only_value(&self) -> bool {
        self.value.is_some() && !self.has_descendants
    }

    /// Returns `true` if has descendants but no value (pure intermediate).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rumps_storage::node::NodeData;
    /// use rumps_types::Value;
    ///
    /// let intermediate = NodeData::with_descendants();
    /// assert!(intermediate.has_only_descendants());
    ///
    /// let both = NodeData::new(Some(Value::Integer(42)), true);
    /// assert!(!both.has_only_descendants());
    /// ```
    pub(crate) fn has_only_descendants(&self) -> bool {
        self.value.is_none() && self.has_descendants
    }
}

impl Serialize for NodeData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = match (&self.value, self.has_descendants) {
            (None, false) => vec![NodeDataTag::Empty as u8],
            (None, true) => vec![NodeDataTag::Intermediate as u8],
            (Some(value), false) => {
                let mut bytes = vec![NodeDataTag::Leaf as u8];
                let value_bytes = bincode::serialize(value).map_err(|e| {
                    serde::ser::Error::custom(format!(
                        "Failed to serialize value: {}",
                        e
                    ))
                })?;
                bytes.extend_from_slice(&value_bytes);
                bytes
            }
            (Some(value), true) => {
                let mut bytes = vec![NodeDataTag::Both as u8];
                let value_bytes = bincode::serialize(value).map_err(|e| {
                    serde::ser::Error::custom(format!(
                        "Failed to serialize value: {}",
                        e
                    ))
                })?;
                bytes.extend_from_slice(&value_bytes);
                bytes
            }
        };
        serializer.serialize_bytes(&bytes)
    }
}

impl<'de> Deserialize<'de> for NodeData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NodeDataVisitor;

        impl<'de> Visitor<'de> for NodeDataVisitor {
            type Value = NodeData;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a compact-encoded NodeData")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let tag_byte = *v
                    .first()
                    .ok_or_else(|| E::custom("empty NodeData bytes"))?;

                match tag_byte {
                    tag if tag == NodeDataTag::Empty as u8 => {
                        Ok(NodeData::new(None, false))
                    }
                    tag if tag == NodeDataTag::Intermediate as u8 => {
                        Ok(NodeData::new(None, true))
                    }
                    tag if tag == NodeDataTag::Leaf as u8 => v
                        .len()
                        .checked_sub(2)
                        .ok_or_else(|| {
                            E::custom("leaf NodeData requires value bytes")
                        })
                        .and_then(|_| {
                            bincode::deserialize(&v[1..])
                                .map_err(|e| {
                                    E::custom(format!(
                                        "Failed to deserialize value: {}",
                                        e
                                    ))
                                })
                                .map(|value| NodeData::new(Some(value), false))
                        }),
                    tag if tag == NodeDataTag::Both as u8 => v
                        .len()
                        .checked_sub(2)
                        .ok_or_else(|| {
                            E::custom("both NodeData requires value bytes")
                        })
                        .and_then(|_| {
                            bincode::deserialize(&v[1..])
                                .map_err(|e| {
                                    E::custom(format!(
                                        "Failed to deserialize value: {}",
                                        e
                                    ))
                                })
                                .map(|value| NodeData::new(Some(value), true))
                        }),
                    tag => Err(E::custom(format!(
                        "unknown NodeData tag: 0x{:02x}",
                        tag
                    ))),
                }
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let bytes =
                    iter::from_fn(|| seq.next_element::<u8>().transpose())
                        .collect::<Result<Vec<u8>, _>>()?;
                self.visit_bytes(&bytes)
            }
        }

        deserializer.deserialize_bytes(NodeDataVisitor)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use rumps_types::{key, value};
    use smallvec::smallvec;

    use super::*;

    // Creation Tests

    #[test]
    fn test_node_data_empty() {
        let node = NodeData::empty();
        assert_eq!(node.value, None);
        assert_eq!(node.has_descendants, false);
    }

    #[test]
    fn test_node_data_with_value() {
        let node = NodeData::with_value(value!(42));
        assert_eq!(node.value, Some(value!(42)));
        assert_eq!(node.has_descendants, false);
    }

    #[test]
    fn test_node_data_with_descendants() {
        let node = NodeData::with_descendants();
        assert_eq!(node.value, None);
        assert_eq!(node.has_descendants, true);
    }

    #[test]
    fn test_node_data_new() {
        // Empty
        let empty = NodeData::new(None, false);
        assert_eq!(empty.value, None);
        assert_eq!(empty.has_descendants, false);

        // Intermediate
        let intermediate = NodeData::new(None, true);
        assert_eq!(intermediate.value, None);
        assert_eq!(intermediate.has_descendants, true);

        // Leaf
        let leaf = NodeData::new(Some(value!("test")), false);
        assert_eq!(leaf.value, Some(value!("test")));
        assert_eq!(leaf.has_descendants, false);

        // Both
        let both = NodeData::new(Some(value!(true)), true);
        assert_eq!(both.value, Some(value!(true)));
        assert_eq!(both.has_descendants, true);
    }

    // Accessor Tests

    #[test]
    fn test_node_data_is_empty() {
        assert!(NodeData::empty().is_empty());
        assert!(!NodeData::with_value(value!(1)).is_empty());
        assert!(!NodeData::with_descendants().is_empty());
        assert!(!NodeData::new(Some(value!(1)), true).is_empty());
    }

    #[test]
    fn test_node_data_has_value() {
        assert!(!NodeData::empty().has_value());
        assert!(NodeData::with_value(value!(1)).has_value());
        assert!(!NodeData::with_descendants().has_value());
        assert!(NodeData::new(Some(value!(1)), true).has_value());
    }

    #[test]
    fn test_node_data_has_only_value() {
        assert!(!NodeData::empty().has_only_value());
        assert!(NodeData::with_value(value!(1)).has_only_value());
        assert!(!NodeData::with_descendants().has_only_value());
        assert!(!NodeData::new(Some(value!(1)), true).has_only_value());
    }

    #[test]
    fn test_node_data_has_only_descendants() {
        assert!(!NodeData::empty().has_only_descendants());
        assert!(!NodeData::with_value(value!(1)).has_only_descendants());
        assert!(NodeData::with_descendants().has_only_descendants());
        assert!(!NodeData::new(Some(value!(1)), true).has_only_descendants());
    }

    // Serialization Tests

    #[test]
    fn test_node_data_serialization_empty() {
        let node = NodeData::empty();
        let bytes = bincode::serialize(&node).unwrap();
        assert_eq!(bytes.len(), 9); // 8 bytes length prefix + 1 byte tag
        assert_eq!(bytes[8], NodeDataTag::Empty as u8);
    }

    #[test]
    fn test_node_data_serialization_intermediate() {
        let node = NodeData::with_descendants();
        let bytes = bincode::serialize(&node).unwrap();
        assert_eq!(bytes.len(), 9); // 8 bytes length prefix + 1 byte tag
        assert_eq!(bytes[8], NodeDataTag::Intermediate as u8);
    }

    #[test]
    fn test_node_data_serialization_leaf() {
        let node = NodeData::with_value(value!(42));
        let bytes = bincode::serialize(&node).unwrap();
        // 8 bytes length prefix + 1 byte tag + encoded value
        assert!(bytes.len() > 9);
        assert_eq!(bytes[8], NodeDataTag::Leaf as u8);
    }

    #[test]
    fn test_node_data_serialization_both() {
        let node = NodeData::new(Some(value!("test")), true);
        let bytes = bincode::serialize(&node).unwrap();
        // 8 bytes length prefix + 1 byte tag + encoded value
        assert!(bytes.len() > 9);
        assert_eq!(bytes[8], NodeDataTag::Both as u8);
    }

    #[test]
    fn test_node_data_roundtrip() {
        let test_cases = vec![
            NodeData::empty(),
            NodeData::with_descendants(),
            NodeData::with_value(value!(true)),
            NodeData::with_value(value!(42)),
            NodeData::with_value(value!(3.14)),
            NodeData::with_value(value!('x')),
            NodeData::with_value(value!("hello")),
            NodeData::with_value(value!(serde_json::json!({"key": "value"}))),
            NodeData::new(Some(value!(99)), true),
        ];

        test_cases.into_iter().for_each(|original| {
            let bytes = bincode::serialize(&original).unwrap();
            let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
            assert_eq!(original, deserialized);
        });
    }

    #[test]
    fn test_node_data_serialization_size() {
        // Empty node: 8 bytes length + 1 byte tag = 9 bytes total
        let empty = NodeData::empty();
        let empty_bytes = bincode::serialize(&empty).unwrap();
        assert_eq!(empty_bytes.len(), 9);

        // Intermediate node: 8 bytes length + 1 byte tag = 9 bytes total
        let intermediate = NodeData::with_descendants();
        let intermediate_bytes = bincode::serialize(&intermediate).unwrap();
        assert_eq!(intermediate_bytes.len(), 9);

        // Leaf and Both nodes will be larger due to Value encoding
        let leaf = NodeData::with_value(value!(1));
        let leaf_bytes = bincode::serialize(&leaf).unwrap();
        assert!(leaf_bytes.len() > 9);

        let both = NodeData::new(Some(value!(1)), true);
        let both_bytes = bincode::serialize(&both).unwrap();
        assert!(both_bytes.len() > 9);
    }

    // Edge Cases

    #[test]
    fn test_node_data_empty_string() {
        let node = NodeData::with_value(value!(""));
        let bytes = bincode::serialize(&node).unwrap();
        let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
        assert_eq!(node, deserialized);
    }

    #[test]
    fn test_node_data_all_value_types() {
        let value_types = vec![
            value!(false),
            value!(true),
            value!(i64::MIN),
            value!(i64::MAX),
            value!(0.0),
            value!(-0.0),
            value!('\0'),
            value!('🦀'),
            value!(""),
            value!("multi\nline\tstring"),
            value!(serde_json::json!(null)),
            value!(serde_json::json!({"nested": {"data": [1, 2, 3]}})),
        ];

        value_types.into_iter().for_each(|value| {
            let node = NodeData::with_value(value.clone());
            let bytes = bincode::serialize(&node).unwrap();
            let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, deserialized);
        });
    }

    #[test]
    fn test_node_data_large_values() {
        // Large string
        let large_string = "x".repeat(10000);
        let node = NodeData::with_value(value!(large_string.clone()));
        let bytes = bincode::serialize(&node).unwrap();
        let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
        assert_eq!(node, deserialized);

        // Large JSON
        let large_json = serde_json::json!({
            "data": vec![0; 1000],
        });
        let node = NodeData::with_value(value!(large_json.clone()));
        let bytes = bincode::serialize(&node).unwrap();
        let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
        assert_eq!(node, deserialized);
    }

    // Node Tests

    #[test]
    fn test_node_new_leaf() {
        let leaf = Node::new_leaf();
        assert!(leaf.is_leaf);
        assert!(leaf.keys.is_empty());
        assert!(leaf.children.is_empty());
        assert!(leaf.values.is_empty());
    }

    #[test]
    fn test_node_new_internal() {
        let internal = Node::new_internal();
        assert!(!internal.is_leaf);
        assert!(internal.keys.is_empty());
        assert!(internal.children.is_empty());
        assert!(internal.values.is_empty());
    }

    // Node serde roundtrip tests

    mod node_serde_tests {
        use super::*;

        #[test]
        fn roundtrip_empty_leaf() {
            let node = Node::new_leaf();
            let bytes = bincode::serialize(&node).unwrap();
            let restored: Node = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, restored);
        }

        #[test]
        fn roundtrip_empty_internal() {
            let node = Node::new_internal();
            let bytes = bincode::serialize(&node).unwrap();
            let restored: Node = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, restored);
        }

        #[test]
        fn roundtrip_leaf_with_data() {
            let node = Node {
                keys: smallvec![key![123, "NAME"], key![124, "DOB"]],
                children: smallvec![],
                values: smallvec![
                    Arc::new(NodeData::with_value(value!("John"))),
                    Arc::new(NodeData::with_value(value!("1980"))),
                ],
                is_leaf: true,
            };
            let bytes = bincode::serialize(&node).unwrap();
            let restored: Node = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, restored);
        }

        #[test]
        fn roundtrip_internal_with_children() {
            let node = Node {
                keys: smallvec![key![100], key![200], key![300]],
                children: smallvec![
                    NodeId::from(10u64),
                    NodeId::from(20u64),
                    NodeId::from(30u64),
                    NodeId::from(40u64),
                ],
                values: smallvec![
                    Arc::new(NodeData::with_descendants()),
                    Arc::new(NodeData::with_descendants()),
                    Arc::new(NodeData::with_descendants()),
                ],
                is_leaf: false,
            };
            let bytes = bincode::serialize(&node).unwrap();
            let restored: Node = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, restored);
        }

        #[test]
        fn roundtrip_all_value_types() {
            let node = Node {
                keys: smallvec![
                    key!["bool"],
                    key!["int"],
                    key!["float"],
                    key!["char"],
                    key!["string"],
                    key!["json"],
                ],
                children: smallvec![],
                values: smallvec![
                    Arc::new(NodeData::with_value(value!(true))),
                    Arc::new(NodeData::with_value(value!(-42))),
                    Arc::new(NodeData::with_value(value!(3.14159))),
                    Arc::new(NodeData::with_value(value!('🦀'))),
                    Arc::new(NodeData::with_value(value!("hello"))),
                    Arc::new(NodeData::with_value(value!(
                        serde_json::json!({"nested": [1, 2, 3]})
                    ))),
                ],
                is_leaf: true,
            };
            let bytes = bincode::serialize(&node).unwrap();
            let restored: Node = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, restored);
        }

        #[test]
        fn roundtrip_all_nodedata_states() {
            let node = Node {
                keys: smallvec![
                    key!["empty"],
                    key!["intermediate"],
                    key!["leaf"],
                    key!["both"],
                ],
                children: smallvec![],
                values: smallvec![
                    Arc::new(NodeData::empty()),
                    Arc::new(NodeData::with_descendants()),
                    Arc::new(NodeData::with_value(value!(1))),
                    Arc::new(NodeData::new(Some(value!(2)), true)),
                ],
                is_leaf: true,
            };
            let bytes = bincode::serialize(&node).unwrap();
            let restored: Node = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, restored);
        }

        #[test]
        fn roundtrip_deep_keys() {
            let node = Node {
                keys: smallvec![
                    key!["l1", "l2", "l3", "l4", "l5"],
                    key![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
                ],
                children: smallvec![],
                values: smallvec![
                    Arc::new(NodeData::with_value(value!("deep"))),
                    Arc::new(NodeData::with_value(value!(12345))),
                ],
                is_leaf: true,
            };
            let bytes = bincode::serialize(&node).unwrap();
            let restored: Node = bincode::deserialize(&bytes).unwrap();
            assert_eq!(node, restored);
        }

        #[test]
        fn empty_bytes_error() {
            let result: Result<Node, _> = bincode::deserialize(&[]);
            assert!(result.is_err());
        }

        #[test]
        fn malformed_bytes_error() {
            let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC];
            let result: Result<Node, _> = bincode::deserialize(&garbage);
            assert!(result.is_err());
        }

        #[test]
        fn truncated_bytes_error() {
            let node = Node {
                keys: smallvec![key!["test"]],
                children: smallvec![],
                values: smallvec![Arc::new(NodeData::with_value(value!(42)))],
                is_leaf: true,
            };
            let bytes = bincode::serialize(&node).unwrap();
            let truncated = &bytes[..bytes.len() / 2];
            let result: Result<Node, _> = bincode::deserialize(truncated);
            assert!(result.is_err());
        }
    }
}
