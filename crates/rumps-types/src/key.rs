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
//! use rumps_types::{global, key};
//!
//! let name = global!("PATIENT");
//! let key = key![123, "NAME"];
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
//! use rumps_types::{global, local, key};
//!
//! // Global variable (persistent)
//! let global_name = global!("PATIENT");
//! assert_eq!(global_name.to_string(), "^PATIENT");
//!
//! // Local variable (ephemeral)
//! let local_name = local!("TEMP");
//! assert_eq!(local_name.to_string(), "TEMP");
//!
//! // Hierarchical key path
//! let key = key![123, "ADDRESS", "CITY"];
//! assert_eq!(key.to_string(), "(123, ADDRESS, CITY)");
//!
//! // Collation ordering ensures numbers sort numerically
//! let key1 = key![2];
//! let key100 = key![100];
//! assert!(key1 < key100);  // NOT "100" < "2" as with strings!
//! ```

use std::hash::{Hash, Hasher};
use std::{cmp, fmt, mem, slice, vec};

use ordered_float::OrderedFloat;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use smol_str::SmolStr;

/// A RUMPS variable name, either Global (persistent) or Local (ephemeral).
///
/// # Examples
///
/// ```
/// use rumps_types::{global, local};
///
/// // Global variable (persistent, prefixed with ^)
/// let g = global!("PATIENT");
/// assert_eq!(g.to_string(), "^PATIENT");
///
/// // Local variable (ephemeral, no prefix)
/// let l = local!("TEMP");
/// assert_eq!(l.to_string(), "TEMP");
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
    ///
    /// Example: `^PATIENT`
    Global(SmolStr),

    /// A local variable (ephemeral, memory-only).
    ///
    /// Example: `PATIENT`
    Local(SmolStr),
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
        // (since only globals are persisted; locals are ephemeral)
        SmolStr::deserialize(deserializer).map(Self::Global)
    }
}

impl Name {
    /// Creates a global variable name from a runtime string.
    ///
    /// Use this for dynamically-constructed names (e.g., from user input or
    /// formatted strings). For string literals known at compile time, prefer
    /// the [`global!`] macro which uses zero-copy storage.
    ///
    /// # When to Use
    ///
    /// - **Use [`global!`]** for string literals: `global!("PATIENT")`
    /// - **Use `Name::global()`** for runtime strings: `Name::global(&format!("TABLE_{}", id))`
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::Name;
    ///
    /// // Runtime string from format!
    /// let id = 123;
    /// let g = Name::global(&format!("PATIENT_{}", id));
    /// assert_eq!(g.to_string(), "^PATIENT_123");
    ///
    /// // Runtime string from user input
    /// let user_input = "CUSTOM_TABLE";
    /// let g2 = Name::global(user_input);
    /// ```
    pub fn global(s: &str) -> Self {
        Self::Global(SmolStr::new(s))
    }

    /// Creates a local variable name from a runtime string.
    ///
    /// Use this for dynamically-constructed names (e.g., from user input or
    /// formatted strings). For string literals known at compile time, prefer
    /// the [`local!`] macro which uses zero-copy storage.
    ///
    /// # When to Use
    ///
    /// - **Use [`local!`]** for string literals: `local!("TEMP")`
    /// - **Use `Name::local()`** for runtime strings: `Name::local(&format!("VAR_{}", id))`
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::Name;
    ///
    /// // Runtime string from format!
    /// let id = 42;
    /// let l = Name::local(&format!("TEMP_{}", id));
    /// assert_eq!(l.to_string(), "TEMP_42");
    /// ```
    pub fn local(s: &str) -> Self {
        Self::Local(SmolStr::new(s))
    }

    /// Returns the inner name string without the namespace prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{global, local};
    ///
    /// let g = global!("PATIENT");
    /// assert_eq!(g.name(), "PATIENT");
    ///
    /// let l = local!("TEMP");
    /// assert_eq!(l.name(), "TEMP");
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
/// 3. Chars: single UTF-8 characters (e.g., 'A' < 'B' < 'Z')
/// 4. Strings: in lexicographic order (e.g., "1" < "10" < "ABC")
/// 5. JSON: structured data (ordered by JSON string representation)
///
/// # Examples
///
/// ```
/// use rumps_types::{subscript, Subscript};
///
/// let bool_sub = subscript!(false);
/// let num_sub = subscript!(123);
/// let char_sub = subscript!('A');
/// let str_sub = subscript!("NAME");
/// let json_sub = Subscript::Json(serde_json::json!({"key": "value"}));
///
/// // Extended RUMPS collation: booleans < numbers < chars < strings < json
/// assert!(bool_sub < num_sub);
/// assert!(num_sub < char_sub);
/// assert!(char_sub < str_sub);
/// assert!(str_sub < json_sub);
/// ```
///
/// # Parser Syntax (future)
///
/// When RUMPS gets a parser, subscripts will use:
/// - JSON: single quotes like `'{"active": true}'`
/// - Char: single quotes with single character like `'A'`
/// - String: double quotes like `"NAME"`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subscript {
    /// A boolean subscript (false < true).
    Boolean(bool),
    /// A numeric subscript (integers and floats in numeric order).
    Number(OrderedFloat<f64>),
    /// A single character subscript.
    Char(char),
    /// A string subscript (lexicographic order).
    String(String),
    /// A JSON subscript (arbitrary nested structure).
    Json(serde_json::Value),
}

// Manual Hash implementation since serde_json::Value doesn't implement Hash
impl Hash for Subscript {
    fn hash<H: Hasher>(&self, state: &mut H) {
        mem::discriminant(self).hash(state);
        match self {
            Self::Boolean(b) => b.hash(state),
            Self::Number(n) => n.hash(state),
            Self::Char(c) => c.hash(state),
            Self::String(s) => s.hash(state),
            Self::Json(j) => j.to_string().hash(state),
        }
    }
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

    /// Returns `true` if this is a char subscript.
    pub fn is_char(&self) -> bool {
        matches!(self, Self::Char(_))
    }

    /// Returns `true` if this is a string subscript.
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    /// Returns `true` if this is a JSON subscript.
    pub fn is_json(&self) -> bool {
        matches!(self, Self::Json(_))
    }

    /// Returns the subscript as a char, if it is one.
    pub fn as_char(&self) -> Option<char> {
        match self {
            Self::Char(c) => Some(*c),
            _ => None,
        }
    }

    /// Returns the subscript as a JSON reference, if it is one.
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(j) => Some(j),
            _ => None,
        }
    }

    /// Converts the subscript to a string representation.
    pub fn as_display_string(&self) -> String {
        match self {
            Self::Boolean(b) => b.to_string(),
            Self::Number(n) => n.to_string(),
            Self::Char(c) => c.to_string(),
            Self::String(s) => s.clone(),
            Self::Json(j) => j.to_string(),
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
            (Char(a), Char(b)) => a.cmp(b),
            (String(a), String(b)) => a.cmp(b),
            (Json(a), Json(b)) => a.to_string().cmp(&b.to_string()),

            // Cross-variant comparisons: Boolean < Number < Char < String < Json
            (Boolean(_), Number(_)) => cmp::Ordering::Less,
            (Boolean(_), Char(_)) => cmp::Ordering::Less,
            (Boolean(_), String(_)) => cmp::Ordering::Less,
            (Boolean(_), Json(_)) => cmp::Ordering::Less,
            (Number(_), Boolean(_)) => cmp::Ordering::Greater,
            (Number(_), Char(_)) => cmp::Ordering::Less,
            (Number(_), String(_)) => cmp::Ordering::Less,
            (Number(_), Json(_)) => cmp::Ordering::Less,
            (Char(_), Boolean(_)) => cmp::Ordering::Greater,
            (Char(_), Number(_)) => cmp::Ordering::Greater,
            (Char(_), String(_)) => cmp::Ordering::Less,
            (Char(_), Json(_)) => cmp::Ordering::Less,
            (String(_), Boolean(_)) => cmp::Ordering::Greater,
            (String(_), Number(_)) => cmp::Ordering::Greater,
            (String(_), Char(_)) => cmp::Ordering::Greater,
            (String(_), Json(_)) => cmp::Ordering::Less,
            (Json(_), Boolean(_)) => cmp::Ordering::Greater,
            (Json(_), Number(_)) => cmp::Ordering::Greater,
            (Json(_), Char(_)) => cmp::Ordering::Greater,
            (Json(_), String(_)) => cmp::Ordering::Greater,
        }
    }
}

impl fmt::Display for Subscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean(b) => write!(f, "{}", b),
            Self::Number(n) => write!(f, "{}", n),
            Self::Char(c) => write!(f, "{}", c),
            Self::String(s) => write!(f, "{}", s),
            Self::Json(j) => write!(f, "{}", j),
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

impl From<char> for Subscript {
    fn from(c: char) -> Self {
        Self::Char(c)
    }
}

impl From<serde_json::Value> for Subscript {
    fn from(j: serde_json::Value) -> Self {
        Self::Json(j)
    }
}

/// Convenience macro for constructing [`Key`] instances from a list of subscript values.
///
/// This macro simplifies the creation of keys by automatically converting values
/// into [`Subscript`] instances. It accepts any types that implement `Into<Subscript>`,
/// including integers, floats, strings, booleans, chars, and `serde_json::Value`.
///
/// # Examples
///
/// ```
/// use rumps_types::{key, json, Key};
///
/// // Simple key with multiple subscripts
/// let k = key![123, "ADDRESS", "CITY"];
/// assert_eq!(k.len(), 3);
/// assert_eq!(k.to_string(), "(123, ADDRESS, CITY)");
///
/// // Empty key
/// let empty = key![];
/// assert_eq!(empty, Key::new());
///
/// // Single subscript
/// let single = key![42];
/// assert_eq!(single.len(), 1);
///
/// // Mixed types with trailing comma
/// let mixed = key![false, 1.5, 'X', "test",];
/// assert_eq!(mixed.len(), 4);
///
/// // With JSON subscript using re-exported json! macro
/// let with_json = key![123, json!({"active": true})];
/// assert_eq!(with_json.len(), 2);
/// ```
#[macro_export]
macro_rules! key {
    ($($item:expr),* $(,)?) => {
        $crate::Key::from(vec![$($crate::Subscript::from($item)),*])
    };
}

// Custom Serialize/Deserialize since we can't derive with serde_json::Value
impl Serialize for Subscript {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Use a tagged helper enum for serialization
        // JSON is serialized as a string for binary format compatibility
        #[derive(Serialize)]
        enum SubscriptHelper<'a> {
            Boolean(bool),
            Number(&'a OrderedFloat<f64>),
            Char(char),
            String(&'a str),
            Json(String), // Store JSON as string for bincode compatibility
        }

        let helper = match self {
            Self::Boolean(b) => SubscriptHelper::Boolean(*b),
            Self::Number(n) => SubscriptHelper::Number(n),
            Self::Char(c) => SubscriptHelper::Char(*c),
            Self::String(s) => SubscriptHelper::String(s),
            Self::Json(j) => SubscriptHelper::Json(j.to_string()),
        };

        helper.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Subscript {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        // Use a tagged helper enum for deserialization
        // JSON is deserialized from a string for binary format compatibility
        #[derive(Deserialize)]
        enum SubscriptHelper {
            Boolean(bool),
            Number(OrderedFloat<f64>),
            Char(char),
            String(String),
            Json(String), // Receive JSON as string for bincode compatibility
        }

        let helper = SubscriptHelper::deserialize(deserializer)?;
        Ok(match helper {
            SubscriptHelper::Boolean(b) => Self::Boolean(b),
            SubscriptHelper::Number(n) => Self::Number(n),
            SubscriptHelper::Char(c) => Self::Char(c),
            SubscriptHelper::String(s) => Self::String(s),
            SubscriptHelper::Json(json_str) => {
                let json_val =
                    serde_json::from_str(&json_str).map_err(|e| {
                        D::Error::custom(format!("Invalid JSON: {}", e))
                    })?;
                Self::Json(json_val)
            }
        })
    }
}

/// A key representing a path through the RUMPS tree structure.
///
/// A `Key` is a sequence of subscripts that define a hierarchical path,
/// like `^PATIENT(123, "NAME")` which would be represented as `key![123, "NAME"]`.
///
/// Keys are ordered lexicographically by their subscripts using the
/// extended RUMPS collation order.
///
/// # Examples
///
/// ```
/// use rumps_types::{key, subscript, Subscript};
///
/// // Create a key with multiple subscripts
/// let key = key![123, "NAME"];
///
/// assert_eq!(key.len(), 2);
/// assert_eq!(key.get(0), Some(&subscript!(123)));
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

    /// Returns all ancestor keys (all prefixes except the full key).
    ///
    /// For a key with subscripts `[a, b, c]`, this returns keys for
    /// prefixes `[a]` and `[a, b]`. Empty keys and single-subscript keys
    /// have no ancestors.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::key;
    ///
    /// let key = key![123, "NAME"];
    ///
    /// let ancestors = key.ancestors();
    /// assert_eq!(ancestors.len(), 1);
    /// assert_eq!(ancestors[0], key![123]);
    /// ```
    pub fn ancestors(&self) -> Vec<Self> {
        (1..self.len())
            .map(|i| Self::from(self.as_slice()[..i].to_vec()))
            .collect()
    }

    /// Returns `true` if this key starts with the given prefix.
    ///
    /// A key starts with a prefix if:
    /// - The prefix length is <= this key's length
    /// - All subscripts in the prefix match the corresponding subscripts in this key
    ///
    /// Note: An empty prefix matches any key, and a key always starts with itself.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{key, Key};
    ///
    /// let key = key![1, 2, 3];
    ///
    /// let prefix = key![1, 2];
    /// assert!(key.starts_with(&prefix));
    ///
    /// let not_prefix = key![1, 9];
    /// assert!(!key.starts_with(&not_prefix));
    ///
    /// // A key starts with itself
    /// assert!(key.starts_with(&key));
    ///
    /// // Empty prefix matches everything
    /// assert!(key.starts_with(&Key::new()));
    /// ```
    #[inline]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.0.len() >= prefix.0.len()
            && self.0[..prefix.0.len()] == prefix.0[..]
    }

    /// Returns the parent key (all subscripts except the last), or None if empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{key, Key};
    ///
    /// let key = key![1, 2, 3];
    ///
    /// let parent = key.parent().unwrap();
    /// assert_eq!(parent, key![1, 2]);
    ///
    /// let single = key![1];
    /// assert_eq!(single.parent(), Some(Key::new())); // Empty key
    ///
    /// let empty = Key::new();
    /// assert_eq!(empty.parent(), None);
    /// ```
    pub fn parent(&self) -> Option<Self> {
        match self.0.len() {
            0 => None,
            n => Some(Self::from(self.0[..n - 1].to_vec())),
        }
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
    type IntoIter = vec::IntoIter<Subscript>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Key {
    type Item = &'a Subscript;
    type IntoIter = slice::Iter<'a, Subscript>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Creates a [`Name::Global`] from a string literal.
///
/// This is the preferred way to create global names when the name is known
/// at compile time. It uses zero-copy storage for the string.
///
/// # When to Use
///
/// - **Use `global!`** for string literals: `global!("PATIENT")`
/// - **Use [`Name::global()`]** for runtime strings: `Name::global(&format!("TABLE_{}", id))`
///
/// # Examples
///
/// ```
/// use rumps_types::global;
///
/// let patients = global!("PATIENT");
/// assert_eq!(patients.to_string(), "^PATIENT");
///
/// // Cloning is cheap (no heap allocation for short names)
/// let patients2 = patients.clone();
/// ```
///
/// # Note
///
/// This macro requires a `&'static str` (i.e., a string literal). For
/// dynamically-constructed names, use [`Name::global()`] instead.
#[macro_export]
macro_rules! global {
    ($name:expr) => {
        $crate::Name::Global($crate::SmolStr::new_static($name))
    };
}

/// Creates a [`Name::Local`] from a string literal.
///
/// This is the preferred way to create local names when the name is known
/// at compile time. It uses zero-copy storage for the string.
///
/// # When to Use
///
/// - **Use `local!`** for string literals: `local!("TEMP")`
/// - **Use [`Name::local()`]** for runtime strings: `Name::local(&format!("VAR_{}", id))`
///
/// # Examples
///
/// ```
/// use rumps_types::local;
///
/// let temp = local!("TEMP");
/// assert_eq!(temp.to_string(), "TEMP");
///
/// // Cloning is cheap (no heap allocation for short names)
/// let temp2 = temp.clone();
/// ```
///
/// # Note
///
/// This macro requires a `&'static str` (i.e., a string literal). For
/// dynamically-constructed names, use [`Name::local()`] instead.
#[macro_export]
macro_rules! local {
    ($name:expr) => {
        $crate::Name::Local($crate::SmolStr::new_static($name))
    };
}

/// Creates a [`Subscript`] from a value.
///
/// This is a convenience macro for `Subscript::from(value)`. It accepts any
/// type that implements `Into<Subscript>`: booleans, integers, floats, chars,
/// strings, and JSON values.
///
/// # Examples
///
/// ```
/// use rumps_types::subscript;
///
/// let b = subscript!(true);
/// let n = subscript!(123);
/// let f = subscript!(1.5);
/// let c = subscript!('X');
/// let s = subscript!("NAME");
/// ```
///
/// # Note
///
/// For building keys with multiple subscripts, prefer the [`key!`] macro
/// which handles the conversion automatically:
///
/// ```
/// use rumps_types::key;
///
/// // Preferred for multi-subscript keys
/// let k = key![123, "NAME"];
///
/// // subscript! is useful for single subscripts or comparisons
/// use rumps_types::subscript;
/// assert_eq!(k.get(1), Some(&subscript!("NAME")));
/// ```
#[macro_export]
macro_rules! subscript {
    ($val:expr) => {
        $crate::Subscript::from($val)
    };
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::json;

    #[test]
    fn test_global_display() {
        let name = global!("PATIENT");
        assert_eq!(name.to_string(), "^PATIENT");
    }

    #[test]
    fn test_local_display() {
        let name = local!("TEMP");
        assert_eq!(name.to_string(), "TEMP");
    }

    #[test]
    fn test_name_accessor() {
        let g = global!("PATIENT");
        assert_eq!(g.name(), "PATIENT");

        let l = local!("TEMP");
        assert_eq!(l.name(), "TEMP");
    }

    #[test]
    fn test_is_global() {
        let g = global!("PATIENT");
        assert!(g.is_global());
        assert!(!g.is_local());
    }

    #[test]
    fn test_is_local() {
        let l = local!("TEMP");
        assert!(l.is_local());
        assert!(!l.is_global());
    }

    #[test]
    fn test_ordering() {
        let g1 = global!("A");
        let g2 = global!("B");
        let l1 = local!("A");
        let l2 = local!("B");

        // Globals should sort before locals (based on enum variant order)
        assert!(g1 < l1);
        assert!(g2 < l2);

        // Within same variant, sort by name
        assert!(g1 < g2);
        assert!(l1 < l2);
    }

    #[test]
    fn test_equality() {
        let g1 = global!("PATIENT");
        let g2 = global!("PATIENT");
        let l = local!("PATIENT");

        assert_eq!(g1, g2);
        assert_ne!(g1, l);
    }

    #[test]
    fn test_serialization() {
        // Test that Global serializes as plain string (no enum tag)
        let g = global!("PATIENT");
        let serialized = bincode::serialize(&g).unwrap();

        // Should be same as serializing the string directly
        let string_serialized = bincode::serialize("PATIENT").unwrap();
        assert_eq!(serialized, string_serialized);

        // Round-trip should work
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(g, deserialized);

        // Test that Local also serializes as plain string
        let l = local!("TEMP");
        let serialized = bincode::serialize(&l).unwrap();
        let string_serialized = bincode::serialize("TEMP").unwrap();
        assert_eq!(serialized, string_serialized);

        // Deserializing always produces Global variant
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized, global!("TEMP"));
        assert_ne!(deserialized, l);
    }

    #[test]
    fn test_name_clone() {
        let g = global!("PATIENT");
        let g_clone = g.clone();
        assert_eq!(g, g_clone);

        let l = local!("TEMP");
        let l_clone = l.clone();
        assert_eq!(l, l_clone);
    }

    #[test]
    fn test_name_hash() {
        use std::collections::HashMap;

        let g1 = global!("PATIENT");
        let g2 = global!("PATIENT");
        let l = local!("PATIENT");

        // Test that Name can be used as HashMap key
        let map =
            HashMap::from([(g1.clone(), "value1"), (l.clone(), "value2")]);

        // Same values should map to the same key
        assert_eq!(map.get(&g2), Some(&"value1"));
        assert_eq!(map.get(&l), Some(&"value2"));

        // Different variants with same string should be different keys
        assert_eq!(map.len(), 2);
        assert_ne!(map.get(&g1), map.get(&l));
    }

    #[test]
    fn test_name_debug() {
        let g = global!("PATIENT");
        let debug_str = format!("{:?}", g);
        assert!(debug_str.contains("Global"));
        assert!(debug_str.contains("PATIENT"));

        let l = local!("TEMP");
        let debug_str = format!("{:?}", l);
        assert!(debug_str.contains("Local"));
        assert!(debug_str.contains("TEMP"));
    }

    #[test]
    fn test_name_with_special_characters() {
        let special = global!("TEST$#@!");
        assert_eq!(special.name(), "TEST$#@!");
        assert_eq!(special.to_string(), "^TEST$#@!");

        let unicode = local!("日本語");
        assert_eq!(unicode.name(), "日本語");
        assert_eq!(unicode.to_string(), "日本語");
    }

    #[test]
    fn test_name_empty_string() {
        let empty_g = global!("");
        assert_eq!(empty_g.name(), "");
        assert_eq!(empty_g.to_string(), "^");

        let empty_l = local!("");
        assert_eq!(empty_l.name(), "");
        assert_eq!(empty_l.to_string(), "");
    }

    #[test]
    fn test_name_serialization_edge_cases() {
        // Test empty string
        let empty = global!("");
        let serialized = bincode::serialize(&empty).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(empty, deserialized);

        // Test special characters
        let special = global!("^$#@!");
        let serialized = bincode::serialize(&special).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(special, deserialized);

        // Test unicode
        let unicode = global!("日本語テスト");
        let serialized = bincode::serialize(&unicode).unwrap();
        let deserialized: Name = bincode::deserialize(&serialized).unwrap();
        assert_eq!(unicode, deserialized);
    }

    #[test]
    fn test_name_comprehensive_ordering() {
        let names = vec![
            global!("A"),
            global!("B"),
            global!("Z"),
            local!("A"),
            local!("B"),
            local!("Z"),
        ];

        // Verify all globals come before all locals
        names[..3].iter().all(|g| g.is_global());
        names[3..].iter().all(|l| l.is_local());

        // Verify ordering is transitive and consistent
        names.windows(2).all(|w| w[0] < w[1]);

        // Test with same name strings
        let g = global!("SAME");
        let l = local!("SAME");
        assert!(g < l);
    }

    #[test]
    fn test_name_partialord_consistency() {
        let g1 = global!("A");
        let g2 = global!("B");

        // PartialOrd should be consistent with Ord
        assert_eq!(g1.partial_cmp(&g2), Some(std::cmp::Ordering::Less));
        assert_eq!(g1.cmp(&g2), std::cmp::Ordering::Less);

        // Test reflexivity
        assert_eq!(g1.partial_cmp(&g1), Some(std::cmp::Ordering::Equal));
    }

    // Subscript tests

    #[test]
    fn test_subscript_boolean_ordering() {
        let f = subscript!(false);
        let t = subscript!(true);

        assert!(f < t);
        assert_eq!(f, subscript!(false));
    }

    #[test]
    fn test_subscript_number_ordering() {
        let n1 = subscript!(-10);
        let n2 = subscript!(0);
        let n3 = subscript!(1.5);
        let n4 = subscript!(10);
        let n5 = subscript!(100);

        // Numeric ordering
        assert!(n1 < n2);
        assert!(n2 < n3);
        assert!(n3 < n4);
        assert!(n4 < n5);
    }

    #[test]
    fn test_subscript_string_ordering() {
        let s1 = subscript!("1");
        let s2 = subscript!("10");
        let s3 = subscript!("ABC");
        let s4 = subscript!("NAME");

        // Lexicographic ordering (note: "1" < "10" as strings)
        assert!(s1 < s2);
        assert!(s2 < s3);
        assert!(s3 < s4);
    }

    #[test]
    fn test_subscript_cross_type_ordering() {
        let bool_false = subscript!(false);
        let bool_true = subscript!(true);
        let num_neg = subscript!(-10);
        let num_zero = subscript!(0);
        let num_pos = subscript!(100);
        let char_a = subscript!('A');
        let char_z = subscript!('Z');
        let str_num = subscript!("1");
        let str_alpha = subscript!("ABC");
        let json_val = Subscript::Json(serde_json::json!({"a": 1}));

        // Boolean < Number < Char < String < Json
        assert!(bool_false < bool_true);
        assert!(bool_true < num_neg);
        assert!(num_neg < num_zero);
        assert!(num_zero < num_pos);
        assert!(num_pos < char_a);
        assert!(char_a < char_z);
        assert!(char_z < str_num);
        assert!(str_num < str_alpha);
        assert!(str_alpha < json_val);
    }

    #[test]
    fn test_subscript_mumps_collation() {
        // This test verifies the RUMPS collation where numeric subscripts
        // are ordered numerically, not lexicographically
        let num_1 = subscript!(1);
        let num_10 = subscript!(10);
        let num_100 = subscript!(100);
        let str_1 = subscript!("1");
        let str_10 = subscript!("10");
        let str_100 = subscript!("100");

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
        assert_eq!(subscript!(false).to_string(), "false");
        assert_eq!(subscript!(true).to_string(), "true");
        assert_eq!(subscript!(123).to_string(), "123");
        assert_eq!(subscript!(1.5).to_string(), "1.5");
        assert_eq!(subscript!('A').to_string(), "A");
        assert_eq!(subscript!("NAME").to_string(), "NAME");
        assert_eq!(
            Subscript::Json(serde_json::json!({"a": 1})).to_string(),
            "{\"a\":1}"
        );
    }

    #[test]
    fn test_subscript_type_checks() {
        let bool_sub = subscript!(false);
        let num_sub = subscript!(123);
        let char_sub = subscript!('X');
        let str_sub = subscript!("ABC");
        let json_sub = Subscript::Json(serde_json::json!({"a": 1}));

        assert!(bool_sub.is_boolean());
        assert!(!bool_sub.is_number());
        assert!(!bool_sub.is_char());
        assert!(!bool_sub.is_string());
        assert!(!bool_sub.is_json());

        assert!(!num_sub.is_boolean());
        assert!(num_sub.is_number());
        assert!(!num_sub.is_char());
        assert!(!num_sub.is_string());
        assert!(!num_sub.is_json());

        assert!(!char_sub.is_boolean());
        assert!(!char_sub.is_number());
        assert!(char_sub.is_char());
        assert!(!char_sub.is_string());
        assert!(!char_sub.is_json());

        assert!(!str_sub.is_boolean());
        assert!(!str_sub.is_number());
        assert!(!str_sub.is_char());
        assert!(str_sub.is_string());
        assert!(!str_sub.is_json());

        assert!(!json_sub.is_boolean());
        assert!(!json_sub.is_number());
        assert!(!json_sub.is_char());
        assert!(!json_sub.is_string());
        assert!(json_sub.is_json());
    }

    #[test]
    fn test_subscript_serialization() {
        // Test each variant round-trips correctly
        let bool_sub = subscript!(true);
        let serialized = bincode::serialize(&bool_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(bool_sub, deserialized);

        let num_sub = subscript!(123.45);
        let serialized = bincode::serialize(&num_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(num_sub, deserialized);

        let char_sub = subscript!('Z');
        let serialized = bincode::serialize(&char_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(char_sub, deserialized);

        let str_sub = subscript!("TEST");
        let serialized = bincode::serialize(&str_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(str_sub, deserialized);

        let json_sub = subscript!(serde_json::json!({"test": 123}));
        let serialized = bincode::serialize(&json_sub).unwrap();
        let deserialized: Subscript =
            bincode::deserialize(&serialized).unwrap();
        assert_eq!(json_sub, deserialized);
    }

    // Key tests

    #[test]
    fn test_key_creation() {
        let k = Key::new();
        assert!(k.is_empty());
        assert_eq!(k.len(), 0);

        let k = key![123, "NAME"];
        assert!(!k.is_empty());
        assert_eq!(k.len(), 2);
    }

    #[test]
    fn test_key_push_pop() {
        let mut key = Key::new();
        key.push(subscript!(123));
        key.push(subscript!("NAME"));

        assert_eq!(key.len(), 2);
        assert_eq!(key.get(0), Some(&subscript!(123)));
        assert_eq!(key.get(1), Some(&subscript!("NAME")));

        assert_eq!(key.pop(), Some(subscript!("NAME")));
        assert_eq!(key.len(), 1);
    }

    #[test]
    fn test_key_ordering() {
        // Lexicographic ordering on subscript sequences
        let key1 = key![1];
        let key2 = key![10];
        let key3 = key![10, "A"];
        let key4 = key![10, "B"];

        assert!(key1 < key2);
        assert!(key2 < key3);
        assert!(key3 < key4);

        // Shorter keys come before longer keys with same prefix
        let short_key = key![1];
        let long_key = key![1, 2];
        assert!(short_key < long_key);
    }

    #[test]
    fn test_key_extended_collation_ordering() {
        // Test that extended RUMPS collation carries through to keys
        let key_bool = key![false];
        let key_num = key![10];
        let key_str = key!["ABC"];

        assert!(key_bool < key_num);
        assert!(key_num < key_str);

        // Multi-level keys
        let key1 = key![10, true];
        let key2 = key![10, 5];
        let key3 = key![10, "A"];

        assert!(key1 < key2);
        assert!(key2 < key3);
    }

    #[test]
    fn test_key_display() {
        let empty_key = key![];
        assert_eq!(empty_key.to_string(), "()");

        let single_key = key![123];
        assert_eq!(single_key.to_string(), "(123)");

        let multi_key = key![123, "NAME"];
        assert_eq!(multi_key.to_string(), "(123, NAME)");
    }

    #[test]
    fn test_key_iteration() {
        let k = key![1, 2, 3];

        let collected: Vec<_> = k.iter().cloned().collect();
        assert_eq!(
            collected,
            vec![subscript!(1), subscript!(2), subscript!(3)]
        );

        // Test IntoIterator for &Key
        let count = (&k).into_iter().count();
        assert_eq!(count, 3);

        // Test IntoIterator for Key
        let key_copy = k.clone();
        let count = key_copy.into_iter().count();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_key_from_iterator() {
        let subscripts = vec![subscript!(1), subscript!(2), subscript!(3)];
        let key: Key = subscripts.into_iter().collect();
        assert_eq!(key.len(), 3);
    }

    #[test]
    fn test_key_serialization() {
        let k = key![123, "NAME", true];

        let serialized = bincode::serialize(&k).unwrap();
        let deserialized: Key = bincode::deserialize(&serialized).unwrap();
        assert_eq!(k, deserialized);

        // Test empty key
        let empty_key = key![];
        let serialized = bincode::serialize(&empty_key).unwrap();
        let deserialized: Key = bincode::deserialize(&serialized).unwrap();
        assert_eq!(empty_key, deserialized);
    }

    #[test]
    fn test_ancestors_empty_key() {
        let k = key![];
        assert_eq!(k.ancestors().len(), 0);
    }

    #[test]
    fn test_ancestors_single_subscript() {
        let k = key![123];
        assert_eq!(k.ancestors().len(), 0);
    }

    #[test]
    fn test_ancestors_two_subscripts() {
        let k = key![123, "NAME"];
        let ancestors = k.ancestors();
        assert_eq!(ancestors.len(), 1);
        assert_eq!(ancestors[0], key![123]);
    }

    #[test]
    fn test_ancestors_deep_nesting() {
        let k = key![1, 2, 3, 4, 5];
        let ancestors = k.ancestors();
        assert_eq!(ancestors.len(), 4);
        assert_eq!(ancestors[0], key![1]);
        assert_eq!(ancestors[1], key![1, 2]);
        assert_eq!(ancestors[2], key![1, 2, 3]);
        assert_eq!(ancestors[3], key![1, 2, 3, 4]);
    }

    #[test]
    fn test_key_macro() {
        // Empty key
        let empty = key![];
        assert_eq!(empty, Key::new());
        assert_eq!(empty.len(), 0);

        // Single subscript
        let single = key![42];
        assert_eq!(single.len(), 1);
        assert_eq!(single.get(0), Some(&subscript!(42)));

        // Multiple subscripts with different types
        let multi = key![123, "ADDRESS", "CITY"];
        assert_eq!(multi.len(), 3);
        assert_eq!(multi.get(0), Some(&subscript!(123)));
        assert_eq!(multi.get(1), Some(&subscript!("ADDRESS")));
        assert_eq!(multi.get(2), Some(&subscript!("CITY")));

        // Mixed types
        let mixed = key![false, 1.5, 'X', "test"];
        assert_eq!(mixed.len(), 4);
        assert!(mixed.get(0).unwrap().is_boolean());
        assert!(mixed.get(1).unwrap().is_number());
        assert!(mixed.get(2).unwrap().is_char());
        assert!(mixed.get(3).unwrap().is_string());

        // Trailing comma
        let trailing = key![1, 2, 3,];
        assert_eq!(trailing.len(), 3);

        // With JSON subscript
        let with_json = key![123, json!({"active": true, "count": 5})];
        assert_eq!(with_json.len(), 2);
        assert!(with_json.get(0).unwrap().is_number());
        assert!(with_json.get(1).unwrap().is_json());
    }
}
