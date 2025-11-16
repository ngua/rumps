//! Node-related types for the B-tree storage system.
//!
//! This module defines the data structures used to represent nodes in the RUMPS
//! persistent B-tree storage. The primary type is [`NodeData`], which represents
//! the data stored at a single node in the tree.

use crate::Value;
use serde::{
    de::{self, Deserializer, Visitor},
    ser::Serializer,
    Deserialize, Serialize,
};
use std::{fmt, iter};

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
/// ```
/// use rumps_types::{NodeData, Value};
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
pub struct NodeData {
    /// Optional value stored at this node
    pub value: Option<Value>,
    /// Flag indicating whether this node has descendants
    pub has_descendants: bool,
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
    /// ```
    /// use rumps_types::{NodeData, Value};
    ///
    /// let node = NodeData::new(Some(Value::Integer(42)), false);
    /// assert_eq!(node.value, Some(Value::Integer(42)));
    /// assert_eq!(node.has_descendants, false);
    /// ```
    pub fn new(value: Option<Value>, has_descendants: bool) -> Self {
        Self {
            value,
            has_descendants,
        }
    }

    /// Creates NodeData with value, no descendants (pure leaf).
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{NodeData, Value};
    ///
    /// let leaf = NodeData::with_value(Value::String("hello".into()));
    /// assert!(leaf.has_only_value());
    /// ```
    pub fn with_value(value: Value) -> Self {
        Self {
            value: Some(value),
            has_descendants: false,
        }
    }

    /// Creates NodeData with no value, has descendants (intermediate).
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::NodeData;
    ///
    /// let intermediate = NodeData::with_descendants();
    /// assert!(intermediate.has_only_descendants());
    /// ```
    pub fn with_descendants() -> Self {
        Self {
            value: None,
            has_descendants: true,
        }
    }

    /// Creates an empty NodeData (no value, no descendants).
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::NodeData;
    ///
    /// let empty = NodeData::empty();
    /// assert!(empty.is_empty());
    /// ```
    pub fn empty() -> Self {
        Self {
            value: None,
            has_descendants: false,
        }
    }

    /// Returns true if no value and no descendants.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::NodeData;
    ///
    /// let empty = NodeData::empty();
    /// assert!(empty.is_empty());
    ///
    /// let leaf = NodeData::with_value(42.into());
    /// assert!(!leaf.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.value.is_none() && !self.has_descendants
    }

    /// Returns true if has a value.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{NodeData, Value};
    ///
    /// let leaf = NodeData::with_value(Value::Boolean(true));
    /// assert!(leaf.has_value());
    ///
    /// let intermediate = NodeData::with_descendants();
    /// assert!(!intermediate.has_value());
    /// ```
    pub fn has_value(&self) -> bool {
        self.value.is_some()
    }

    /// Returns true if has value but no descendants (pure leaf).
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{NodeData, Value};
    ///
    /// let leaf = NodeData::with_value(Value::Integer(42));
    /// assert!(leaf.has_only_value());
    ///
    /// let both = NodeData::new(Some(Value::Integer(42)), true);
    /// assert!(!both.has_only_value());
    /// ```
    pub fn has_only_value(&self) -> bool {
        self.value.is_some() && !self.has_descendants
    }

    /// Returns true if has descendants but no value (pure intermediate).
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{NodeData, Value};
    ///
    /// let intermediate = NodeData::with_descendants();
    /// assert!(intermediate.has_only_descendants());
    ///
    /// let both = NodeData::new(Some(Value::Integer(42)), true);
    /// assert!(!both.has_only_descendants());
    /// ```
    pub fn has_only_descendants(&self) -> bool {
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
                let value_bytes = bincode::serialize(value)
                    .map_err(|e| serde::ser::Error::custom(format!("Failed to serialize value: {}", e)))?;
                bytes.extend_from_slice(&value_bytes);
                bytes
            }
            (Some(value), true) => {
                let mut bytes = vec![NodeDataTag::Both as u8];
                let value_bytes = bincode::serialize(value)
                    .map_err(|e| serde::ser::Error::custom(format!("Failed to serialize value: {}", e)))?;
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
                    .get(0)
                    .ok_or_else(|| E::custom("empty NodeData bytes"))?;

                match tag_byte {
                    tag if tag == NodeDataTag::Empty as u8 => Ok(NodeData::new(None, false)),
                    tag if tag == NodeDataTag::Intermediate as u8 => Ok(NodeData::new(None, true)),
                    tag if tag == NodeDataTag::Leaf as u8 => v
                        .len()
                        .checked_sub(2)
                        .ok_or_else(|| E::custom("leaf NodeData requires value bytes"))
                        .and_then(|_| {
                            bincode::deserialize(&v[1..])
                                .map_err(|e| E::custom(format!("Failed to deserialize value: {}", e)))
                                .map(|value| NodeData::new(Some(value), false))
                        }),
                    tag if tag == NodeDataTag::Both as u8 => v
                        .len()
                        .checked_sub(2)
                        .ok_or_else(|| E::custom("both NodeData requires value bytes"))
                        .and_then(|_| {
                            bincode::deserialize(&v[1..])
                                .map_err(|e| E::custom(format!("Failed to deserialize value: {}", e)))
                                .map(|value| NodeData::new(Some(value), true))
                        }),
                    tag => Err(E::custom(format!("unknown NodeData tag: 0x{:02x}", tag))),
                }
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let bytes = iter::from_fn(|| seq.next_element::<u8>().transpose())
                    .collect::<Result<Vec<u8>, _>>()?;
                self.visit_bytes(&bytes)
            }
        }

        deserializer.deserialize_bytes(NodeDataVisitor)
    }
}

#[cfg(test)]
mod tests {
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
        let node = NodeData::with_value(Value::Integer(42));
        assert_eq!(node.value, Some(Value::Integer(42)));
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
        let leaf = NodeData::new(Some(Value::String("test".into())), false);
        assert_eq!(leaf.value, Some(Value::String("test".into())));
        assert_eq!(leaf.has_descendants, false);

        // Both
        let both = NodeData::new(Some(Value::Boolean(true)), true);
        assert_eq!(both.value, Some(Value::Boolean(true)));
        assert_eq!(both.has_descendants, true);
    }

    // Accessor Tests

    #[test]
    fn test_node_data_is_empty() {
        assert!(NodeData::empty().is_empty());
        assert!(!NodeData::with_value(Value::Integer(1)).is_empty());
        assert!(!NodeData::with_descendants().is_empty());
        assert!(!NodeData::new(Some(Value::Integer(1)), true).is_empty());
    }

    #[test]
    fn test_node_data_has_value() {
        assert!(!NodeData::empty().has_value());
        assert!(NodeData::with_value(Value::Integer(1)).has_value());
        assert!(!NodeData::with_descendants().has_value());
        assert!(NodeData::new(Some(Value::Integer(1)), true).has_value());
    }

    #[test]
    fn test_node_data_has_only_value() {
        assert!(!NodeData::empty().has_only_value());
        assert!(NodeData::with_value(Value::Integer(1)).has_only_value());
        assert!(!NodeData::with_descendants().has_only_value());
        assert!(!NodeData::new(Some(Value::Integer(1)), true).has_only_value());
    }

    #[test]
    fn test_node_data_has_only_descendants() {
        assert!(!NodeData::empty().has_only_descendants());
        assert!(!NodeData::with_value(Value::Integer(1)).has_only_descendants());
        assert!(NodeData::with_descendants().has_only_descendants());
        assert!(!NodeData::new(Some(Value::Integer(1)), true).has_only_descendants());
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
        let node = NodeData::with_value(Value::Integer(42));
        let bytes = bincode::serialize(&node).unwrap();
        // 8 bytes length prefix + 1 byte tag + encoded value
        assert!(bytes.len() > 9);
        assert_eq!(bytes[8], NodeDataTag::Leaf as u8);
    }

    #[test]
    fn test_node_data_serialization_both() {
        let node = NodeData::new(Some(Value::String("test".into())), true);
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
            NodeData::with_value(Value::Boolean(true)),
            NodeData::with_value(Value::Integer(42)),
            NodeData::with_value(Value::Double(3.14.into())),
            NodeData::with_value(Value::Char('x')),
            NodeData::with_value(Value::String("hello".into())),
            NodeData::with_value(Value::Json(serde_json::json!({"key": "value"}))),
            NodeData::new(Some(Value::Integer(99)), true),
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
        let leaf = NodeData::with_value(Value::Integer(1));
        let leaf_bytes = bincode::serialize(&leaf).unwrap();
        assert!(leaf_bytes.len() > 9);

        let both = NodeData::new(Some(Value::Integer(1)), true);
        let both_bytes = bincode::serialize(&both).unwrap();
        assert!(both_bytes.len() > 9);
    }

    // Edge Cases

    #[test]
    fn test_node_data_empty_string() {
        let node = NodeData::with_value(Value::String("".into()));
        let bytes = bincode::serialize(&node).unwrap();
        let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
        assert_eq!(node, deserialized);
    }

    #[test]
    fn test_node_data_all_value_types() {
        let value_types = vec![
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Integer(i64::MIN),
            Value::Integer(i64::MAX),
            Value::Double(0.0.into()),
            Value::Double((-0.0).into()),
            Value::Char('\0'),
            Value::Char('🦀'),
            Value::String("".into()),
            Value::String("multi\nline\tstring".into()),
            Value::Json(serde_json::json!(null)),
            Value::Json(serde_json::json!({"nested": {"data": [1, 2, 3]}})),
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
        let node = NodeData::with_value(Value::String(large_string.clone()));
        let bytes = bincode::serialize(&node).unwrap();
        let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
        assert_eq!(node, deserialized);

        // Large JSON
        let large_json = serde_json::json!({
            "data": vec![0; 1000],
        });
        let node = NodeData::with_value(Value::Json(large_json.clone()));
        let bytes = bincode::serialize(&node).unwrap();
        let deserialized: NodeData = bincode::deserialize(&bytes).unwrap();
        assert_eq!(node, deserialized);
    }
}
