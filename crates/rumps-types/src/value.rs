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

use std::{cmp, fmt, io};

use ordered_float::OrderedFloat;
use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};

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

// Compact serialization format:
// 0x00 = Boolean false
// 0x01 = Boolean true
// 0x02-0x81 = Small positive integers 0-127
// 0x82-0xF1 = Small negative integers -1 to -112
// 0xF2 = Large integer (followed by LEB128)
// 0xF3 = Double (followed by 8 bytes)
// 0xF4 = String (followed by varint length + content)

const TAG_FALSE: u8 = 0x00;
const TAG_TRUE: u8 = 0x01;
const TAG_SMALL_POS_START: u8 = 0x02;
const TAG_SMALL_POS_END: u8 = 0x81;
const TAG_SMALL_NEG_START: u8 = 0x82;
const TAG_SMALL_NEG_END: u8 = 0xF1;
const TAG_LARGE_INT: u8 = 0xF2;
const TAG_DOUBLE: u8 = 0xF3;
const TAG_STRING: u8 = 0xF4;

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Build the full byte representation
        let bytes = match self {
            Value::Boolean(false) => vec![TAG_FALSE],
            Value::Boolean(true) => vec![TAG_TRUE],
            Value::Integer(i) if *i >= 0 && *i <= 127 => {
                vec![TAG_SMALL_POS_START + *i as u8]
            }
            Value::Integer(i) if *i >= -112 && *i < 0 => {
                vec![TAG_SMALL_NEG_START + ((-1 - *i) as u8)]
            }
            Value::Integer(i) => {
                let mut bytes = Vec::with_capacity(10);
                bytes.push(TAG_LARGE_INT);
                write_leb128_signed(&mut bytes, *i);
                bytes
            }
            Value::Double(d) => {
                let mut bytes = Vec::with_capacity(9);
                bytes.push(TAG_DOUBLE);
                bytes.extend_from_slice(&d.into_inner().to_le_bytes());
                bytes
            }
            Value::String(s) => {
                let len = s.len();
                let mut bytes = Vec::with_capacity(1 + varint_size(len) + len);
                bytes.push(TAG_STRING);
                write_varint(&mut bytes, len);
                bytes.extend_from_slice(s.as_bytes());
                bytes
            }
        };

        // Serialize as a byte slice
        serializer.serialize_bytes(&bytes)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a compact-encoded Value")
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if v.is_empty() {
            return Err(E::custom("empty value bytes"));
        }

        let tag = v[0];
        match tag {
            TAG_FALSE => Ok(Value::Boolean(false)),
            TAG_TRUE => Ok(Value::Boolean(true)),
            TAG_SMALL_POS_START..=TAG_SMALL_POS_END => {
                Ok(Value::Integer((tag - TAG_SMALL_POS_START) as i64))
            }
            TAG_SMALL_NEG_START..=TAG_SMALL_NEG_END => {
                let offset = tag - TAG_SMALL_NEG_START;
                Ok(Value::Integer(-1 - offset as i64))
            }
            TAG_LARGE_INT => {
                let (value, _) = read_leb128_signed(&v[1..])
                    .map_err(|e| E::custom(format!("invalid LEB128: {}", e)))?;
                Ok(Value::Integer(value))
            }
            TAG_DOUBLE => {
                if v.len() < 9 {
                    return Err(E::custom("double requires 9 bytes"));
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&v[1..9]);
                let d = f64::from_le_bytes(bytes);
                Ok(Value::Double(OrderedFloat(d)))
            }
            TAG_STRING => {
                let (len, offset) = read_varint(&v[1..])
                    .map_err(|e| E::custom(format!("invalid varint: {}", e)))?;
                let start = 1 + offset;
                let end = start + len;
                if end > v.len() {
                    return Err(E::custom("string extends beyond buffer"));
                }
                let s = std::str::from_utf8(&v[start..end])
                    .map_err(|e| E::custom(format!("invalid UTF-8: {}", e)))?;
                Ok(Value::String(s.to_string()))
            }
            _ => Err(E::custom(format!("unknown value tag: 0x{:02x}", tag))),
        }
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        // Collect bytes functionally using unfold-like pattern
        let bytes = std::iter::from_fn(|| seq.next_element::<u8>().transpose())
            .collect::<Result<Vec<u8>, _>>()?;
        self.visit_bytes(&bytes)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Since we serialize as bytes, deserialize as bytes
        deserializer.deserialize_bytes(ValueVisitor)
    }
}

// Helper functions for variable-length integer encoding

#[inline]
fn write_varint(buf: &mut Vec<u8>, value: usize) {
    // Generate varint bytes functionally using successors
    let bytes: Vec<u8> = std::iter::successors(Some(value), |&v| (v > 0).then(|| v >> 7))
        .enumerate()
        .take_while(|(i, v)| *i == 0 || *v > 0)
        .map(|(_, v)| {
            let mut byte = (v & 0x7F) as u8;
            if v >> 7 != 0 {
                byte |= 0x80;
            }
            byte
        })
        .collect();

    buf.extend(bytes);
}

#[inline]
fn read_varint(buf: &[u8]) -> Result<(usize, usize), io::Error> {
    buf.iter()
        .enumerate()
        .scan((0usize, 0usize), |(value, shift), (offset, &byte)| {
            if *shift >= 64 {
                return Some(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "varint too large",
                )));
            }

            *value |= ((byte & 0x7F) as usize) << *shift;
            let offset = offset + 1;

            if byte & 0x80 == 0 {
                Some(Ok((*value, offset)))
            } else {
                *shift += 7;
                Some(Err(io::Error::new(io::ErrorKind::Other, ""))) // Continue scanning
            }
        })
        .find_map(|result| result.ok())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete varint")
        })
}

#[inline]
fn varint_size(value: usize) -> usize {
    std::iter::successors(Some(value), |&v| (v >= 128).then(|| v >> 7))
        .count()
}

#[inline]
fn write_leb128_signed(buf: &mut Vec<u8>, value: i64) {
    let bytes: Vec<u8> = std::iter::successors(Some(value), |&v| {
        let byte = (v & 0x7F) as u8;
        let shifted = v >> 7;
        let done = (shifted == 0 && byte & 0x40 == 0) || (shifted == -1 && byte & 0x40 != 0);
        (!done).then_some(shifted)
    })
    .zip(std::iter::repeat(()))
    .map(|(v, _)| {
        let byte = (v & 0x7F) as u8;
        let shifted = v >> 7;
        let done = (shifted == 0 && byte & 0x40 == 0) || (shifted == -1 && byte & 0x40 != 0);
        if done {
            byte
        } else {
            byte | 0x80
        }
    })
    .collect();

    buf.extend(bytes);
}

#[inline]
fn read_leb128_signed(buf: &[u8]) -> Result<(i64, usize), io::Error> {
    buf.iter()
        .enumerate()
        .scan((0i64, 0usize), |(value, shift), (offset, &byte)| {
            if *shift >= 64 {
                return Some(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LEB128 too large",
                )));
            }

            *value |= ((byte & 0x7F) as i64) << *shift;
            let offset = offset + 1;
            let next_shift = *shift + 7;

            if byte & 0x80 == 0 {
                // Sign-extend if necessary
                if next_shift < 64 && byte & 0x40 != 0 {
                    *value |= !0 << next_shift;
                }
                Some(Ok((*value, offset)))
            } else {
                *shift = next_shift;
                Some(Err(io::Error::new(io::ErrorKind::Other, ""))) // Continue scanning
            }
        })
        .find_map(|result| result.ok())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete LEB128")
        })
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

    #[test]
    fn test_value_serialization_roundtrip() {
        // Test each variant round-trips correctly
        let test_values = vec![
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Integer(0),
            Value::Integer(1),
            Value::Integer(127),
            Value::Integer(128),
            Value::Integer(-1),
            Value::Integer(-112),
            Value::Integer(-113),
            Value::Integer(1000000),
            Value::Integer(-1000000),
            Value::Integer(i64::MAX),
            Value::Integer(i64::MIN),
            Value::Double(OrderedFloat(0.0)),
            Value::Double(OrderedFloat(3.14)),
            Value::Double(OrderedFloat(-2.5)),
            Value::Double(OrderedFloat(f64::MAX)),
            Value::Double(OrderedFloat(f64::MIN)),
            Value::String(String::new()),
            Value::String("hello".to_string()),
            Value::String("a".repeat(1000)),
        ];

        test_values.iter().for_each(|value| {
            let serialized = bincode::serialize(value).unwrap();
            let deserialized: Value = bincode::deserialize(&serialized).unwrap();
            assert_eq!(
                value, &deserialized,
                "Failed to round-trip: {:?}",
                value
            );
        });
    }

    #[test]
    fn test_value_serialization_size() {
        // Verify compact encoding sizes
        // NOTE: bincode adds an 8-byte length prefix to byte arrays,
        // so actual sizes are 8 bytes larger than our compact encoding

        // Booleans: 1 byte data + 8 byte bincode overhead = 9 bytes
        assert_eq!(bincode::serialize(&Value::Boolean(false)).unwrap().len(), 9);
        assert_eq!(bincode::serialize(&Value::Boolean(true)).unwrap().len(), 9);

        // Small positive integers: 1 byte data + 8 byte overhead = 9 bytes
        assert_eq!(bincode::serialize(&Value::Integer(0)).unwrap().len(), 9);
        assert_eq!(bincode::serialize(&Value::Integer(1)).unwrap().len(), 9);
        assert_eq!(bincode::serialize(&Value::Integer(127)).unwrap().len(), 9);

        // Small negative integers: 1 byte data + 8 byte overhead = 9 bytes
        assert_eq!(bincode::serialize(&Value::Integer(-1)).unwrap().len(), 9);
        assert_eq!(bincode::serialize(&Value::Integer(-112)).unwrap().len(), 9);

        // Larger integers: (2-3 bytes data) + 8 byte overhead
        assert_eq!(bincode::serialize(&Value::Integer(128)).unwrap().len(), 11); // 3 + 8
        assert_eq!(bincode::serialize(&Value::Integer(-113)).unwrap().len(), 11); // 3 + 8

        // Doubles: 9 bytes data + 8 byte overhead = 17 bytes
        assert_eq!(bincode::serialize(&Value::Double(OrderedFloat(0.0))).unwrap().len(), 17);
        assert_eq!(bincode::serialize(&Value::Double(OrderedFloat(3.14))).unwrap().len(), 17);

        // Strings: (tag + varint length + content) + 8 byte overhead
        assert_eq!(bincode::serialize(&Value::String(String::new())).unwrap().len(), 10); // 2 + 8
        assert_eq!(bincode::serialize(&Value::String("hello".to_string())).unwrap().len(), 15); // 7 + 8
    }

    #[test]
    fn test_value_serialization_edge_cases() {
        // Test boundary values
        let boundary_values = vec![
            // Boundary between small and large positive integers
            Value::Integer(126),
            Value::Integer(127),
            Value::Integer(128),
            Value::Integer(129),
            // Boundary between small and large negative integers
            Value::Integer(-111),
            Value::Integer(-112),
            Value::Integer(-113),
            Value::Integer(-114),
        ];

        boundary_values.iter().for_each(|value| {
            let serialized = bincode::serialize(value).unwrap();
            let deserialized: Value = bincode::deserialize(&serialized).unwrap();
            assert_eq!(value, &deserialized);
        });
    }

    #[test]
    fn test_varint_encoding() {
        // Test varint helper functions
        let test_cases = vec![
            (0usize, 1),
            (127, 1),
            (128, 2),
            (16383, 2),
            (16384, 3),
            (2097151, 3),
            (2097152, 4),
        ];

        test_cases.iter().for_each(|(value, expected_size)| {
            let mut buf = Vec::new();
            write_varint(&mut buf, *value);
            assert_eq!(buf.len(), *expected_size, "varint size for {}", value);
            assert_eq!(varint_size(*value), *expected_size);

            let (decoded, offset) = read_varint(&buf).unwrap();
            assert_eq!(decoded, *value);
            assert_eq!(offset, *expected_size);
        });
    }

    #[test]
    fn test_leb128_encoding() {
        // Test LEB128 signed encoding
        let test_cases = vec![
            (0i64, vec![0x00]),
            (1, vec![0x01]),
            (63, vec![0x3F]),
            (64, vec![0xC0, 0x00]),
            (127, vec![0xFF, 0x00]),
            (128, vec![0x80, 0x01]),
            (-1, vec![0x7F]),
            (-64, vec![0x40]),
            (-65, vec![0xBF, 0x7F]),
            (-128, vec![0x80, 0x7F]),
            (-129, vec![0xFF, 0x7E]),
        ];

        test_cases.iter().for_each(|(value, expected)| {
            let mut buf = Vec::new();
            write_leb128_signed(&mut buf, *value);
            assert_eq!(
                buf, *expected,
                "LEB128 encoding for {} failed",
                value
            );

            let (decoded, offset) = read_leb128_signed(&buf).unwrap();
            assert_eq!(decoded, *value);
            assert_eq!(offset, expected.len());
        });
    }
}
