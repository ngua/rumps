//! Scalar value types stored in the RUMPS database.
//!
//! # Overview
//!
//! RUMPS stores all data as scalar values at tree nodes. Each node in the
//! hierarchical tree structure can hold a value, descendants, or both. This
//! module defines the supported value types.
//!
//! ## Supported Value Types
//!
//! RUMPS extends traditional MUMPS with explicit type support:
//!
//! - **Boolean**: `true` or `false`
//! - **Integer**: 64-bit signed integers (`i64`)
//! - **Double**: 64-bit floating-point numbers (`f64`)
//! - **String**: UTF-8 encoded strings
//!
//! ## Type System Design
//!
//! Unlike traditional MUMPS which stores everything as strings and coerces on
//! use, RUMPS maintains explicit types. This provides:
//!
//! - **Type safety**: Values retain their type through storage and retrieval
//! - **Efficient storage**: Binary encoding via `bincode` is more compact than text
//! - **Numeric precision**: No loss of precision from string conversion
//!
//! ## Value Ordering
//!
//! Values are fully ordered to support sorting and range queries. The ordering
//! follows the extended RUMPS collation:
//!
//! 1. **Booleans**: `false < true`
//! 2. **Integers**: Numeric order
//! 3. **Doubles**: Numeric order (using `OrderedFloat` for total ordering)
//! 4. **Strings**: Lexicographic order
//!
//! Cross-type comparisons follow: Boolean < Integer < Double < String
//!
//! ## Storage Semantics
//!
//! Values are stored at specific keys in the tree. For example:
//!
//! ```text
//! ^PATIENT(123, "NAME") = String("Alice")
//! ^PATIENT(123, "AGE") = Integer(42)
//! ^PATIENT(123, "ACTIVE") = Boolean(true)
//! ^PATIENT(123, "TEMP") = Double(98.6)
//! ```
//!
//! Each value exists independently at its key path, and the tree structure
//! allows for efficient traversal and range queries.
//!
//! ## Example Usage
//!
//! ```
//! use rumps_types::Value;
//!
//! // Create values of different types
//! let name = Value::String("Alice".to_string());
//! let age = Value::Integer(42);
//! let active = Value::Boolean(true);
//! let temp = Value::Double(98.6.into());
//!
//! // Values can be converted from Rust primitives
//! let v1: Value = "test".into();
//! let v2: Value = 123.into();
//! let v3: Value = true.into();
//! let v4: Value = 3.14.into();
//!
//! // Values are fully ordered
//! assert!(Value::Boolean(true) < Value::Integer(0));
//! assert!(Value::Integer(100) < Value::Double(100.1.into()));
//! assert!(Value::Double(999.9.into()) < Value::String("A".to_string()));
//! ```

use std::{cmp, fmt};

use ordered_float::OrderedFloat;

/// A scalar value stored in the RUMPS database.
///
/// Each node in the RUMPS tree can store a single scalar value. Values are
/// strongly typed and maintain their type through serialization and storage.
///
/// Values are fully ordered, enabling sorting and range queries. The ordering
/// follows: Boolean < Integer < Double < String, with natural ordering within
/// each type.
///
/// # Examples
///
/// ```
/// use rumps_types::Value;
///
/// let str_val = Value::String("Hello".to_string());
/// let int_val = Value::Integer(42);
/// let dbl_val = Value::Double(3.14.into());
/// let bool_val = Value::Boolean(true);
///
/// // Values are ordered
/// assert!(bool_val < int_val);
/// assert!(int_val < dbl_val);
/// assert!(dbl_val < str_val);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    /// A boolean value (false < true).
    Boolean(bool),
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit floating-point number (ordered via OrderedFloat).
    Double(OrderedFloat<f64>),
    /// A UTF-8 encoded string.
    String(String),
}

impl Value {
    /// Returns `true` if this value is a boolean.
    #[inline]
    pub fn is_boolean(&self) -> bool {
        matches!(self, Self::Boolean(_))
    }

    /// Returns `true` if this value is an integer.
    #[inline]
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::Integer(_))
    }

    /// Returns `true` if this value is a double.
    #[inline]
    pub fn is_double(&self) -> bool {
        matches!(self, Self::Double(_))
    }

    /// Returns `true` if this value is a string.
    #[inline]
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    /// Returns the value as a boolean, if it is one.
    #[inline]
    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the value as an integer, if it is one.
    #[inline]
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Returns the value as a double, if it is one.
    #[inline]
    pub fn as_double(&self) -> Option<f64> {
        match self {
            Self::Double(d) => Some(d.into_inner()),
            _ => None,
        }
    }

    /// Returns the value as a string reference, if it is one.
    #[inline]
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        use Value::*;

        match (self, other) {
            // Same variant comparisons
            (Boolean(a), Boolean(b)) => a.cmp(b),
            (Integer(a), Integer(b)) => a.cmp(b),
            (Double(a), Double(b)) => a.cmp(b),
            (String(a), String(b)) => a.cmp(b),

            // Cross-variant comparisons: Boolean < Integer < Double < String
            (Boolean(_), Integer(_)) => cmp::Ordering::Less,
            (Boolean(_), Double(_)) => cmp::Ordering::Less,
            (Boolean(_), String(_)) => cmp::Ordering::Less,
            (Integer(_), Boolean(_)) => cmp::Ordering::Greater,
            (Integer(_), Double(_)) => cmp::Ordering::Less,
            (Integer(_), String(_)) => cmp::Ordering::Less,
            (Double(_), Boolean(_)) => cmp::Ordering::Greater,
            (Double(_), Integer(_)) => cmp::Ordering::Greater,
            (Double(_), String(_)) => cmp::Ordering::Less,
            (String(_), Boolean(_)) => cmp::Ordering::Greater,
            (String(_), Integer(_)) => cmp::Ordering::Greater,
            (String(_), Double(_)) => cmp::Ordering::Greater,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean(b) => write!(f, "{}", b),
            Self::Integer(i) => write!(f, "{}", i),
            Self::Double(d) => write!(f, "{}", d),
            Self::String(s) => write!(f, "{}", s),
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Boolean(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Self::Integer(i)
    }
}

impl From<i32> for Value {
    fn from(i: i32) -> Self {
        Self::Integer(i as i64)
    }
}

impl From<f64> for Value {
    fn from(d: f64) -> Self {
        Self::Double(OrderedFloat(d))
    }
}

impl From<OrderedFloat<f64>> for Value {
    fn from(d: OrderedFloat<f64>) -> Self {
        Self::Double(d)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_creation() {
        let bool_val = Value::Boolean(true);
        let int_val = Value::Integer(42);
        let dbl_val = Value::Double(OrderedFloat(3.14));
        let str_val = Value::String("test".to_string());

        assert!(bool_val.is_boolean());
        assert!(int_val.is_integer());
        assert!(dbl_val.is_double());
        assert!(str_val.is_string());
    }

    #[test]
    fn test_value_type_checks() {
        let bool_val = Value::Boolean(true);
        assert!(bool_val.is_boolean());
        assert!(!bool_val.is_integer());
        assert!(!bool_val.is_double());
        assert!(!bool_val.is_string());

        let int_val = Value::Integer(42);
        assert!(!int_val.is_boolean());
        assert!(int_val.is_integer());
        assert!(!int_val.is_double());
        assert!(!int_val.is_string());

        let dbl_val = Value::Double(OrderedFloat(3.14));
        assert!(!dbl_val.is_boolean());
        assert!(!dbl_val.is_integer());
        assert!(dbl_val.is_double());
        assert!(!dbl_val.is_string());

        let str_val = Value::String("test".to_string());
        assert!(!str_val.is_boolean());
        assert!(!str_val.is_integer());
        assert!(!str_val.is_double());
        assert!(str_val.is_string());
    }

    #[test]
    fn test_value_accessors() {
        let bool_val = Value::Boolean(true);
        assert_eq!(bool_val.as_boolean(), Some(true));
        assert_eq!(bool_val.as_integer(), None);
        assert_eq!(bool_val.as_double(), None);
        assert_eq!(bool_val.as_string(), None);

        let int_val = Value::Integer(42);
        assert_eq!(int_val.as_boolean(), None);
        assert_eq!(int_val.as_integer(), Some(42));
        assert_eq!(int_val.as_double(), None);
        assert_eq!(int_val.as_string(), None);

        let dbl_val = Value::Double(OrderedFloat(3.14));
        assert_eq!(dbl_val.as_boolean(), None);
        assert_eq!(dbl_val.as_integer(), None);
        assert_eq!(dbl_val.as_double(), Some(3.14));
        assert_eq!(dbl_val.as_string(), None);

        let str_val = Value::String("test".to_string());
        assert_eq!(str_val.as_boolean(), None);
        assert_eq!(str_val.as_integer(), None);
        assert_eq!(str_val.as_double(), None);
        assert_eq!(str_val.as_string(), Some("test"));
    }

    #[test]
    fn test_value_ordering() {
        // Same-type ordering
        assert!(Value::Boolean(false) < Value::Boolean(true));
        assert!(Value::Integer(10) < Value::Integer(20));
        assert!(
            Value::Double(OrderedFloat(3.14))
                < Value::Double(OrderedFloat(3.15))
        );
        assert!(
            Value::String("a".to_string()) < Value::String("b".to_string())
        );

        // Cross-type ordering: Boolean < Integer < Double < String
        assert!(Value::Boolean(true) < Value::Integer(0));
        assert!(Value::Integer(100) < Value::Double(OrderedFloat(0.1)));
        assert!(
            Value::Double(OrderedFloat(999.9)) < Value::String("A".to_string())
        );
    }

    #[test]
    fn test_value_display() {
        assert_eq!(Value::Boolean(true).to_string(), "true");
        assert_eq!(Value::Boolean(false).to_string(), "false");
        assert_eq!(Value::Integer(42).to_string(), "42");
        assert_eq!(Value::Integer(-10).to_string(), "-10");
        assert_eq!(Value::Double(OrderedFloat(3.14)).to_string(), "3.14");
        assert_eq!(Value::String("hello".to_string()).to_string(), "hello");
    }

    #[test]
    fn test_value_from_bool() {
        let v: Value = true.into();
        assert_eq!(v, Value::Boolean(true));

        let v: Value = false.into();
        assert_eq!(v, Value::Boolean(false));
    }

    #[test]
    fn test_value_from_i64() {
        let v: Value = 42i64.into();
        assert_eq!(v, Value::Integer(42));

        let v: Value = (-10i64).into();
        assert_eq!(v, Value::Integer(-10));
    }

    #[test]
    fn test_value_from_i32() {
        let v: Value = 42i32.into();
        assert_eq!(v, Value::Integer(42));

        let v: Value = (-10i32).into();
        assert_eq!(v, Value::Integer(-10));
    }

    #[test]
    fn test_value_from_f64() {
        let v: Value = 3.14.into();
        assert_eq!(v, Value::Double(OrderedFloat(3.14)));

        let v: Value = (-2.5).into();
        assert_eq!(v, Value::Double(OrderedFloat(-2.5)));
    }

    #[test]
    fn test_value_from_string() {
        let v: Value = "test".to_string().into();
        assert_eq!(v, Value::String("test".to_string()));

        let v: Value = "hello".into();
        assert_eq!(v, Value::String("hello".to_string()));
    }
}
