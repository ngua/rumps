//! Variable naming and key path types for the RUMPS database.
//!
//! # Overview
//!
//! RUMPS organizes data in persistent, sparse, multi-dimensional trees backed by
//! B-tree storage. This module defines the fundamental types for navigating and
//! addressing data within these trees.
//!
//! ## Type Hierarchy
//!
//! ```text
//! Name (^PATIENT or TEMP)
//!   └─> Key ((123, "NAME"))
//!         └─> Subscript sequence [Number(123), String("NAME")]
//! ```
//!
//! ## Two Namespaces: Globals and Locals
//!
//! RUMPS distinguishes between two types of variables:
//!
//! - **Globals** (`^NAME`): Persistent variables stored on disk, prefixed with `^`
//! - **Locals** (`NAME`): Ephemeral variables kept in memory only, no prefix
//!
//! Both support the same hierarchical tree operations (SET, GET, KILL, etc.),
//! but only globals are written to persistent storage.
//!
//! ## Extended Collation Order
//!
//! RUMPS extends traditional database collation with a three-tier ordering system
//! for subscripts, ensuring numeric values sort correctly:
//!
//! 1. **Booleans**: `false < true`
//! 2. **Numbers**: Numeric ordering (e.g., `1 < 10 < 100`, not `"1" < "100" < "10"`)
//! 3. **Strings**: Lexicographic ordering (e.g., `"A" < "B" < "Z"`)
//!
//! This prevents the common pitfall where string-sorted numbers produce incorrect
//! orderings like `"100" < "2"`.
//!
//! ## Keys as Hierarchical Paths
//!
//! A [`Key`] represents a path through the tree structure. For example, the RUMPS
//! expression `^PATIENT(123, "NAME")` maps to:
//!
//! ```
//! use rumps_types::{Name, Key, Subscript};
//!
//! let name = Name::Global("PATIENT".to_string());
//! let key = Key::from(vec![
//!     Subscript::from(123),
//!     Subscript::from("NAME"),
//! ]);
//! // Represents: ^PATIENT(123, "NAME")
//! ```
//!
//! Keys are ordered lexicographically using the extended collation order,
//! which ensures predictable tree traversal and efficient range queries.
//!
//! ## Storage Mapping
//!
//! - **In-memory**: Both globals and locals use the same tree structure
//! - **On-disk**: Only globals are serialized to persistent B-tree storage
//! - **Serialization**: Types use `bincode` for compact binary encoding
//!
//! ## Example Usage
//!
//! ```
//! use rumps_types::{Name, Key, Subscript};
//!
//! // Global variable (persistent)
//! let global_name = Name::Global("PATIENT".to_string());
//! assert_eq!(global_name.to_string(), "^PATIENT");
//!
//! // Local variable (ephemeral)
//! let local_name = Name::Local("TEMP".to_string());
//! assert_eq!(local_name.to_string(), "TEMP");
//!
//! // Hierarchical key path
//! let key = Key::from(vec![
//!     Subscript::from(123),        // Number
//!     Subscript::from("ADDRESS"),  // String
//!     Subscript::from("CITY"),     // String
//! ]);
//! assert_eq!(key.to_string(), "(123, ADDRESS, CITY)");
//!
//! // Collation ordering ensures numbers sort numerically
//! let key1 = Key::from(vec![Subscript::from(2)]);
//! let key100 = Key::from(vec![Subscript::from(100)]);
//! assert!(key1 < key100);  // NOT "100" < "2" as with strings!
//! ```

use std::{cmp, fmt};

use ordered_float::OrderedFloat;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A RUMPS variable name, either Global (persistent) or Local (ephemeral).
///
/// # Examples
///
/// ```
/// use rumps_types::Name;
///
/// // Global variable (persistent, prefixed with ^)
/// let global = Name::Global("PATIENT".to_string());
/// assert_eq!(global.to_string(), "^PATIENT");
///
/// // Local variable (ephemeral, no prefix)
/// let local = Name::Local("TEMP".to_string());
/// assert_eq!(local.to_string(), "TEMP");
/// ```
///
/// # Serialization
///
/// `Name` serializes directly as a string without enum tags. Since only
/// `Name::Global` entries are persisted to disk, deserialization always
/// produces a `Name::Global` variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Name {
    /// A global variable (persistent, stored on disk).
    /// Example: `^PATIENT`
    Global(String),

    /// A local variable (ephemeral, memory-only).
    /// Example: `PATIENT`
    Local(String),
}

impl Serialize for Name {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Serialize just the inner string, without enum tag
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Deserialize as string, always create Global variant
        // (since only globals are persisted)
        let name = String::deserialize(deserializer)?;
        Ok(Self::Global(name))
    }
}

impl Name {
    /// Returns the inner name string without the namespace prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::Name;
    ///
    /// let global = Name::Global("PATIENT".to_string());
    /// assert_eq!(global.name(), "PATIENT");
    ///
    /// let local = Name::Local("TEMP".to_string());
    /// assert_eq!(local.name(), "TEMP");
    /// ```
    pub fn name(&self) -> &str {
        match self {
            Self::Global(name) | Self::Local(name) => name,
        }
    }

    /// Returns `true` if this is a global variable.
    pub fn is_global(&self) -> bool {
        matches!(self, Self::Global(_))
    }

    /// Returns `true` if this is a local variable.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global(name) => write!(f, "^{}", name),
            Self::Local(name) => write!(f, "{}", name),
        }
    }
}

/// A single subscript in a RUMPS key path.
///
/// Subscripts define a path through the hierarchical tree structure and
/// follow an extended RUMPS collation order:
///
/// 1. Booleans: `false` < `true`
/// 2. Numbers: in numeric order (e.g., -10 < 0 < 1.5 < 10 < 100)
/// 3. Strings: in lexicographic order (e.g., "1" < "10" < "ABC")
///
/// # Examples
///
/// ```
/// use rumps_types::Subscript;
///
/// let bool_sub = Subscript::from(false);
/// let num_sub = Subscript::from(123);
/// let str_sub = Subscript::from("NAME");
///
/// // Extended RUMPS collation: booleans < numbers < strings
/// assert!(bool_sub < num_sub);
/// assert!(num_sub < str_sub);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Subscript {
    /// A boolean subscript (false < true).
    Boolean(bool),
    /// A numeric subscript (integers and floats in numeric order).
    Number(OrderedFloat<f64>),
    /// A string subscript (lexicographic order).
    String(String),
}

impl Subscript {
    /// Returns `true` if this is a boolean subscript.
    pub fn is_boolean(&self) -> bool {
        matches!(self, Self::Boolean(_))
    }

    /// Returns `true` if this is a numeric subscript.
    pub fn is_number(&self) -> bool {
        matches!(self, Self::Number(_))
    }

    /// Returns `true` if this is a string subscript.
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    /// Converts the subscript to a string representation.
    pub fn as_display_string(&self) -> String {
        match self {
            Self::Boolean(b) => b.to_string(),
            Self::Number(n) => n.to_string(),
            Self::String(s) => s.clone(),
        }
    }
}

impl PartialOrd for Subscript {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Subscript {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        use Subscript::*;

        match (self, other) {
            // Same variant comparisons
            (Boolean(a), Boolean(b)) => a.cmp(b),
            (Number(a), Number(b)) => a.cmp(b),
            (String(a), String(b)) => a.cmp(b),

            // Cross-variant comparisons: Boolean < Number < String
            (Boolean(_), Number(_)) => cmp::Ordering::Less,
            (Boolean(_), String(_)) => cmp::Ordering::Less,
            (Number(_), Boolean(_)) => cmp::Ordering::Greater,
            (Number(_), String(_)) => cmp::Ordering::Less,
            (String(_), Boolean(_)) => cmp::Ordering::Greater,
            (String(_), Number(_)) => cmp::Ordering::Greater,
        }
    }
}

impl fmt::Display for Subscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean(b) => write!(f, "{}", b),
            Self::Number(n) => write!(f, "{}", n),
            Self::String(s) => write!(f, "{}", s),
        }
    }
}

impl From<bool> for Subscript {
    fn from(b: bool) -> Self {
        Self::Boolean(b)
    }
}

impl From<i64> for Subscript {
    fn from(n: i64) -> Self {
        Self::Number(OrderedFloat(n as f64))
    }
}

impl From<f64> for Subscript {
    fn from(n: f64) -> Self {
        Self::Number(OrderedFloat(n))
    }
}

impl From<String> for Subscript {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for Subscript {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

/// A key representing a path through the RUMPS tree structure.
///
/// A `Key` is a sequence of subscripts that define a hierarchical path,
/// like `^PATIENT(123, "NAME")` which would be represented as
/// `Key::from(vec![123.into(), "NAME".into()])`.
///
/// Keys are ordered lexicographically by their subscripts using the
/// extended RUMPS collation order.
///
/// # Examples
///
/// ```
/// use rumps_types::{Key, Subscript};
///
/// // Create a key with multiple subscripts
/// let key = Key::from(vec![
///     Subscript::from(123),
///     Subscript::from("NAME"),
/// ]);
///
/// assert_eq!(key.len(), 2);
/// assert_eq!(key.get(0), Some(&Subscript::from(123)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Key(Vec<Subscript>);

impl Key {
    /// Creates an empty key with no subscripts.
    #[inline]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Creates a key with the given capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    /// Returns the number of subscripts in this key.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the key has no subscripts.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns a reference to the subscript at the given index.
    #[inline]
    pub fn get(&self, index: usize) -> Option<&Subscript> {
        self.0.get(index)
    }

    /// Returns an iterator over the subscripts.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Subscript> {
        self.0.iter()
    }

    /// Adds a subscript to the end of this key.
    #[inline]
    pub fn push(&mut self, subscript: Subscript) {
        self.0.push(subscript);
    }

    /// Removes and returns the last subscript, or None if empty.
    #[inline]
    pub fn pop(&mut self) -> Option<Subscript> {
        self.0.pop()
    }

    /// Returns the subscripts as a slice.
    #[inline]
    pub fn as_slice(&self) -> &[Subscript] {
        &self.0
    }
}

impl Default for Key {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        // Lexicographic ordering on the sequence of subscripts
        self.0.cmp(&other.0)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(")?;
        self.0.iter().enumerate().try_for_each(|(i, sub)| {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", sub)
        })?;
        write!(f, ")")
    }
}

impl From<Vec<Subscript>> for Key {
    fn from(subscripts: Vec<Subscript>) -> Self {
        Self(subscripts)
    }
}

impl From<Key> for Vec<Subscript> {
    fn from(key: Key) -> Self {
        key.0
    }
}

impl FromIterator<Subscript> for Key {
    fn from_iter<T: IntoIterator<Item = Subscript>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl IntoIterator for Key {
    type Item = Subscript;
    type IntoIter = std::vec::IntoIter<Subscript>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Key {
    type Item = &'a Subscript;
    type IntoIter = std::slice::Iter<'a, Subscript>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_display() {
        let name = Name::Global("PATIENT".to_string());
        assert_eq!(name.to_string(), "^PATIENT");
    }

    #[test]
    fn test_local_display() {
        let name = Name::Local("TEMP".to_string());
        assert_eq!(name.to_string(), "TEMP");
    }

    #[test]
    fn test_name_accessor() {
        let global = Name::Global("PATIENT".to_string());
        assert_eq!(global.name(), "PATIENT");

        let local = Name::Local("TEMP".to_string());
        assert_eq!(local.name(), "TEMP");
    }

    #[test]
    fn test_is_global() {
        let global = Name::Global("PATIENT".to_string());
        assert!(global.is_global());
        assert!(!global.is_local());
    }

    #[test]
    fn test_is_local() {
        let local = Name::Local("TEMP".to_string());
        assert!(local.is_local());
        assert!(!local.is_global());
    }

    #[test]
    fn test_ordering() {
        let global1 = Name::Global("A".to_string());
        let global2 = Name::Global("B".to_string());
        let local1 = Name::Local("A".to_string());
        let local2 = Name::Local("B".to_string());

        // Globals should sort before locals (based on enum variant order)
        assert!(global1 < local1);
        assert!(global2 < local2);

        // Within same variant, sort by name
        assert!(global1 < global2);
        assert!(local1 < local2);
    }

    #[test]
    fn test_equality() {
        let global1 = Name::Global("PATIENT".to_string());
        let global2 = Name::Global("PATIENT".to_string());
        let local = Name::Local("PATIENT".to_string());

        assert_eq!(global1, global2);
        assert_ne!(global1, local);
    }

    #[test]
    fn test_serialization() {
        // Test that Global serializes as plain string (no enum tag)
        let global = Name::Global("PATIENT".to_string());
        let serialized = bincode::serialize(&global).unwrap();

        // Should be same as serializing the string directly
        let string_serialized = bincode::serialize("PATIENT").unwrap();
        assert_eq!(serialized, string_serialized);

        // Round-trip should work
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(global, deserialized);

        // Test that Local also serializes as plain string
        let local = Name::Local("TEMP".to_string());
        let serialized = bincode::serialize(&local).unwrap();
        let string_serialized = bincode::serialize("TEMP").unwrap();
        assert_eq!(serialized, string_serialized);

        // Deserializing always produces Global variant
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized, Name::Global("TEMP".to_string()));
        assert_ne!(deserialized, local);
    }

    #[test]
    fn test_name_clone() {
        let global = Name::Global("PATIENT".to_string());
        let global_clone = global.clone();
        assert_eq!(global, global_clone);

        let local = Name::Local("TEMP".to_string());
        let local_clone = local.clone();
        assert_eq!(local, local_clone);
    }

    #[test]
    fn test_name_hash() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let global1 = Name::Global("PATIENT".to_string());
        let global2 = Name::Global("PATIENT".to_string());
        let local = Name::Local("PATIENT".to_string());

        let hash_value = |name: &Name| -> u64 {
            let mut hasher = DefaultHasher::new();
            name.hash(&mut hasher);
            hasher.finish()
        };

        // Same values should hash identically
        assert_eq!(hash_value(&global1), hash_value(&global2));

        // Different variants with same string should hash differently
        assert_ne!(hash_value(&global1), hash_value(&local));
    }

    #[test]
    fn test_name_debug() {
        let global = Name::Global("PATIENT".to_string());
        let debug_str = format!("{:?}", global);
        assert!(debug_str.contains("Global"));
        assert!(debug_str.contains("PATIENT"));

        let local = Name::Local("TEMP".to_string());
        let debug_str = format!("{:?}", local);
        assert!(debug_str.contains("Local"));
        assert!(debug_str.contains("TEMP"));
    }

    #[test]
    fn test_name_with_special_characters() {
        let special = Name::Global("TEST$#@!".to_string());
        assert_eq!(special.name(), "TEST$#@!");
        assert_eq!(special.to_string(), "^TEST$#@!");

        let unicode = Name::Local("日本語".to_string());
        assert_eq!(unicode.name(), "日本語");
        assert_eq!(unicode.to_string(), "日本語");
    }

    #[test]
    fn test_name_empty_string() {
        let empty_global = Name::Global("".to_string());
        assert_eq!(empty_global.name(), "");
        assert_eq!(empty_global.to_string(), "^");

        let empty_local = Name::Local("".to_string());
        assert_eq!(empty_local.name(), "");
        assert_eq!(empty_local.to_string(), "");
    }

    #[test]
    fn test_name_serialization_edge_cases() {
        // Test empty string
        let empty = Name::Global("".to_string());
        let serialized = bincode::serialize(&empty).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(empty, deserialized);

        // Test special characters
        let special = Name::Global("^$#@!".to_string());
        let serialized = bincode::serialize(&special).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(special, deserialized);

        // Test unicode
        let unicode = Name::Global("日本語テスト".to_string());
        let serialized = bincode::serialize(&unicode).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(unicode, deserialized);
    }

    #[test]
    fn test_name_comprehensive_ordering() {
        let names = vec![
            Name::Global("A".to_string()),
            Name::Global("B".to_string()),
            Name::Global("Z".to_string()),
            Name::Local("A".to_string()),
            Name::Local("B".to_string()),
            Name::Local("Z".to_string()),
        ];

        // Verify all globals come before all locals
        names[..3].iter().all(|g| g.is_global());
        names[3..].iter().all(|l| l.is_local());

        // Verify ordering is transitive and consistent
        names.windows(2).all(|w| w[0] < w[1]);

        // Test with same name strings
        let g = Name::Global("SAME".to_string());
        let l = Name::Local("SAME".to_string());
        assert!(g < l);
    }

    #[test]
    fn test_name_partialord_consistency() {
        let g1 = Name::Global("A".to_string());
        let g2 = Name::Global("B".to_string());

        // PartialOrd should be consistent with Ord
        assert_eq!(g1.partial_cmp(&g2), Some(std::cmp::Ordering::Less));
        assert_eq!(g1.cmp(&g2), std::cmp::Ordering::Less);

        // Test reflexivity
        assert_eq!(g1.partial_cmp(&g1), Some(std::cmp::Ordering::Equal));
    }

    // Subscript tests

    #[test]
    fn test_subscript_boolean_ordering() {
        let f = Subscript::from(false);
        let t = Subscript::from(true);

        assert!(f < t);
        assert_eq!(f, Subscript::from(false));
    }

    #[test]
    fn test_subscript_number_ordering() {
        let n1 = Subscript::from(-10);
        let n2 = Subscript::from(0);
        let n3 = Subscript::from(1.5);
        let n4 = Subscript::from(10);
        let n5 = Subscript::from(100);

        // Numeric ordering
        assert!(n1 < n2);
        assert!(n2 < n3);
        assert!(n3 < n4);
        assert!(n4 < n5);
    }

    #[test]
    fn test_subscript_string_ordering() {
        let s1 = Subscript::from("1");
        let s2 = Subscript::from("10");
        let s3 = Subscript::from("ABC");
        let s4 = Subscript::from("NAME");

        // Lexicographic ordering (note: "1" < "10" as strings)
        assert!(s1 < s2);
        assert!(s2 < s3);
        assert!(s3 < s4);
    }

    #[test]
    fn test_subscript_cross_type_ordering() {
        let bool_false = Subscript::from(false);
        let bool_true = Subscript::from(true);
        let num_neg = Subscript::from(-10);
        let num_zero = Subscript::from(0);
        let num_pos = Subscript::from(100);
        let str_num = Subscript::from("1");
        let str_alpha = Subscript::from("ABC");

        // Boolean < Number < String
        assert!(bool_false < bool_true);
        assert!(bool_true < num_neg);
        assert!(num_neg < num_zero);
        assert!(num_zero < num_pos);
        assert!(num_pos < str_num);
        assert!(str_num < str_alpha);
    }

    #[test]
    fn test_subscript_mumps_collation() {
        // This test verifies the RUMPS collation where numeric subscripts
        // are ordered numerically, not lexicographically
        let num_1 = Subscript::from(1);
        let num_10 = Subscript::from(10);
        let num_100 = Subscript::from(100);
        let str_1 = Subscript::from("1");
        let str_10 = Subscript::from("10");
        let str_100 = Subscript::from("100");

        // Numeric ordering: 1 < 10 < 100
        assert!(num_1 < num_10);
        assert!(num_10 < num_100);

        // String (lexicographic) ordering: "1" < "10" < "100"
        assert!(str_1 < str_10);
        assert!(str_10 < str_100);

        // All numbers come before all strings
        assert!(num_100 < str_1);
    }

    #[test]
    fn test_subscript_display() {
        assert_eq!(Subscript::from(false).to_string(), "false");
        assert_eq!(Subscript::from(true).to_string(), "true");
        assert_eq!(Subscript::from(123).to_string(), "123");
        assert_eq!(Subscript::from(1.5).to_string(), "1.5");
        assert_eq!(Subscript::from("NAME").to_string(), "NAME");
    }

    #[test]
    fn test_subscript_type_checks() {
        let bool_sub = Subscript::from(false);
        let num_sub = Subscript::from(123);
        let str_sub = Subscript::from("ABC");

        assert!(bool_sub.is_boolean());
        assert!(!bool_sub.is_number());
        assert!(!bool_sub.is_string());

        assert!(!num_sub.is_boolean());
        assert!(num_sub.is_number());
        assert!(!num_sub.is_string());

        assert!(!str_sub.is_boolean());
        assert!(!str_sub.is_number());
        assert!(str_sub.is_string());
    }

    #[test]
    fn test_subscript_serialization() {
        // Test each variant round-trips correctly
        let bool_sub = Subscript::from(true);
        let serialized = bincode::serialize(&bool_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(bool_sub, deserialized);

        let num_sub = Subscript::from(123.45);
        let serialized = bincode::serialize(&num_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(num_sub, deserialized);

        let str_sub = Subscript::from("TEST");
        let serialized = bincode::serialize(&str_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(str_sub, deserialized);
    }

    // Key tests

    #[test]
    fn test_key_creation() {
        let key = Key::new();
        assert!(key.is_empty());
        assert_eq!(key.len(), 0);

        let key =
            Key::from(vec![Subscript::from(123), Subscript::from("NAME")]);
        assert!(!key.is_empty());
        assert_eq!(key.len(), 2);
    }

    #[test]
    fn test_key_push_pop() {
        let mut key = Key::new();
        key.push(Subscript::from(123));
        key.push(Subscript::from("NAME"));

        assert_eq!(key.len(), 2);
        assert_eq!(key.get(0), Some(&Subscript::from(123)));
        assert_eq!(key.get(1), Some(&Subscript::from("NAME")));

        assert_eq!(key.pop(), Some(Subscript::from("NAME")));
        assert_eq!(key.len(), 1);
    }

    #[test]
    fn test_key_ordering() {
        // Lexicographic ordering on subscript sequences
        let key1 = Key::from(vec![Subscript::from(1)]);
        let key2 = Key::from(vec![Subscript::from(10)]);
        let key3 = Key::from(vec![Subscript::from(10), Subscript::from("A")]);
        let key4 = Key::from(vec![Subscript::from(10), Subscript::from("B")]);

        assert!(key1 < key2);
        assert!(key2 < key3);
        assert!(key3 < key4);

        // Shorter keys come before longer keys with same prefix
        let short_key = Key::from(vec![Subscript::from(1)]);
        let long_key = Key::from(vec![Subscript::from(1), Subscript::from(2)]);
        assert!(short_key < long_key);
    }

    #[test]
    fn test_key_extended_collation_ordering() {
        // Test that extended RUMPS collation carries through to keys
        let key_bool = Key::from(vec![Subscript::from(false)]);
        let key_num = Key::from(vec![Subscript::from(10)]);
        let key_str = Key::from(vec![Subscript::from("ABC")]);

        assert!(key_bool < key_num);
        assert!(key_num < key_str);

        // Multi-level keys
        let key1 = Key::from(vec![Subscript::from(10), Subscript::from(true)]);
        let key2 = Key::from(vec![Subscript::from(10), Subscript::from(5)]);
        let key3 = Key::from(vec![Subscript::from(10), Subscript::from("A")]);

        assert!(key1 < key2);
        assert!(key2 < key3);
    }

    #[test]
    fn test_key_display() {
        let empty_key = Key::new();
        assert_eq!(empty_key.to_string(), "()");

        let single_key = Key::from(vec![Subscript::from(123)]);
        assert_eq!(single_key.to_string(), "(123)");

        let multi_key =
            Key::from(vec![Subscript::from(123), Subscript::from("NAME")]);
        assert_eq!(multi_key.to_string(), "(123, NAME)");
    }

    #[test]
    fn test_key_iteration() {
        let key = Key::from(vec![
            Subscript::from(1),
            Subscript::from(2),
            Subscript::from(3),
        ]);

        let collected: Vec<_> = key.iter().cloned().collect();
        assert_eq!(
            collected,
            vec![Subscript::from(1), Subscript::from(2), Subscript::from(3)]
        );

        // Test IntoIterator for &Key
        let count = (&key).into_iter().count();
        assert_eq!(count, 3);

        // Test IntoIterator for Key
        let key_copy = key.clone();
        let count = key_copy.into_iter().count();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_key_from_iterator() {
        let subscripts =
            vec![Subscript::from(1), Subscript::from(2), Subscript::from(3)];
        let key: Key = subscripts.into_iter().collect();
        assert_eq!(key.len(), 3);
    }

    #[test]
    fn test_key_serialization() {
        let key = Key::from(vec![
            Subscript::from(123),
            Subscript::from("NAME"),
            Subscript::from(true),
        ]);

        let serialized = bincode::serialize(&key).unwrap();
        let deserialized: Key = bincode::deserialize(&serialized).unwrap();
        assert_eq!(key, deserialized);

        // Test empty key
        let empty_key = Key::new();
        let serialized = bincode::serialize(&empty_key).unwrap();
        let deserialized: Key = bincode::deserialize(&serialized).unwrap();
        assert_eq!(empty_key, deserialized);
    }
}
