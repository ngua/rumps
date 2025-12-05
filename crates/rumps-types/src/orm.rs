//! ORM-like traits for converting Rust types to/from RUMPS storage format.
//!
//! This module provides traits for automatic conversion between Rust types
//! and RUMPS's hierarchical key-value storage format.
//!
//! # Trait Hierarchy
//!
//! Three layers of traits serve different purposes:
//!
//! 1. **Subscript Conversion** ([`ToSubscript`], [`FromSubscript`]):
//!    Types that can be a single subscript in a key path.
//!
//! 2. **Value Conversion** ([`ToValue`], [`FromValue`]):
//!    Types that can be stored as a terminal value.
//!
//! 3. **Tree Conversion** ([`ToRumps`], [`FromRumps`]):
//!    Types that expand into a tree of key-value pairs (in `rumps-storage`).
//!
//! # Example
//!
//! ```
//! use rumps_types::{Subscript, Value, Key};
//! use rumps_types::orm::{ToSubscript, FromSubscript, ToValue, FromValue, IntoKey};
//!
//! // Primitives implement ToSubscript/FromSubscript
//! let sub: Subscript = 42i64.to_sub();
//! let val: i64 = i64::from_sub(&sub).unwrap();
//! assert_eq!(val, 42);
//!
//! // Primitives implement ToValue/FromValue
//! let v: Value = "hello".to_val();
//! let s: String = String::from_val(&v).unwrap();
//! assert_eq!(s, "hello");
//!
//! // Tuples implement IntoKey
//! let key: Key = (123, "name").into_key();
//! assert_eq!(key.len(), 2);
//! ```

use std::fmt;

use ordered_float::OrderedFloat;

use crate::{Key, Subscript, Value};

/// Error when decoding from RUMPS types.
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeError {
    /// Expected a different subscript type.
    WrongSubscriptType {
        /// Expected type name.
        expected: &'static str,
        /// Actual subscript.
        actual: String,
    },
    /// Expected a different value type.
    WrongValueType {
        /// Expected type name.
        expected: &'static str,
        /// Actual value.
        actual: String,
    },
    /// Integer overflow during conversion.
    IntegerOverflow {
        /// The value that overflowed.
        value: i64,
        /// Target type name.
        target: &'static str,
    },
    /// Float conversion error.
    FloatConversion {
        /// The value that failed to convert.
        value: f64,
        /// Target type name.
        target: &'static str,
    },
    /// Missing required field.
    MissingField {
        /// Field name.
        field: &'static str,
    },
    /// Custom error message.
    Custom(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongSubscriptType { expected, actual } => {
                write!(f, "expected {} subscript, got {}", expected, actual)
            }
            Self::WrongValueType { expected, actual } => {
                write!(f, "expected {} value, got {}", expected, actual)
            }
            Self::IntegerOverflow { value, target } => {
                write!(f, "integer {} overflows {}", value, target)
            }
            Self::FloatConversion { value, target } => {
                write!(f, "cannot convert {} to {}", value, target)
            }
            Self::MissingField { field } => {
                write!(f, "missing required field: {}", field)
            }
            Self::Custom(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Convert to a subscript (part of a key).
pub trait ToSubscript {
    /// Converts `self` to a [`Subscript`].
    fn to_sub(&self) -> Subscript;
}

/// Parse from a subscript.
pub trait FromSubscript: Sized {
    /// Attempts to convert a [`Subscript`] to `Self`.
    fn from_sub(s: &Subscript) -> Result<Self, DecodeError>;
}

impl ToSubscript for bool {
    fn to_sub(&self) -> Subscript {
        Subscript::Boolean(*self)
    }
}

impl FromSubscript for bool {
    fn from_sub(s: &Subscript) -> Result<Self, DecodeError> {
        match s {
            Subscript::Boolean(b) => Ok(*b),
            other => Err(DecodeError::WrongSubscriptType {
                expected: "Boolean",
                actual: format!("{:?}", other),
            }),
        }
    }
}

macro_rules! impl_to_sub_int {
    ($($t:ty),*) => {
        $(
            impl ToSubscript for $t {
                fn to_sub(&self) -> Subscript {
                    Subscript::Number(OrderedFloat(*self as f64))
                }
            }
        )*
    };
}

impl_to_sub_int!(i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);

macro_rules! impl_from_sub_int {
    ($($t:ty),*) => {
        $(
            impl FromSubscript for $t {
                fn from_sub(s: &Subscript) -> Result<Self, DecodeError> {
                    match s {
                        Subscript::Number(n) => {
                            let f = n.into_inner();
                            // Check if it's a whole number
                            let i = f as i64;
                            if (i as f64 - f).abs() > f64::EPSILON {
                                Err(DecodeError::FloatConversion {
                                    value: f,
                                    target: stringify!($t),
                                })
                            } else {
                                <$t>::try_from(i).map_err(|_| DecodeError::IntegerOverflow {
                                    value: i,
                                    target: stringify!($t),
                                })
                            }
                        }
                        other => Err(DecodeError::WrongSubscriptType {
                            expected: "Number",
                            actual: format!("{:?}", other),
                        }),
                    }
                }
            }
        )*
    };
}

impl_from_sub_int!(i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);

impl ToSubscript for f32 {
    fn to_sub(&self) -> Subscript {
        Subscript::Number(OrderedFloat(*self as f64))
    }
}

impl FromSubscript for f32 {
    fn from_sub(s: &Subscript) -> Result<Self, DecodeError> {
        match s {
            Subscript::Number(n) => Ok(n.into_inner() as Self),
            other => Err(DecodeError::WrongSubscriptType {
                expected: "Number",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToSubscript for f64 {
    fn to_sub(&self) -> Subscript {
        Subscript::Number(OrderedFloat(*self))
    }
}

impl FromSubscript for f64 {
    fn from_sub(s: &Subscript) -> Result<Self, DecodeError> {
        match s {
            Subscript::Number(n) => Ok(n.into_inner()),
            other => Err(DecodeError::WrongSubscriptType {
                expected: "Number",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToSubscript for char {
    fn to_sub(&self) -> Subscript {
        Subscript::Char(*self)
    }
}

impl FromSubscript for char {
    fn from_sub(s: &Subscript) -> Result<Self, DecodeError> {
        match s {
            Subscript::Char(c) => Ok(*c),
            other => Err(DecodeError::WrongSubscriptType {
                expected: "Char",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToSubscript for String {
    fn to_sub(&self) -> Subscript {
        Subscript::String(self.clone())
    }
}

impl ToSubscript for &str {
    fn to_sub(&self) -> Subscript {
        Subscript::String((*self).to_string())
    }
}

impl FromSubscript for String {
    fn from_sub(s: &Subscript) -> Result<Self, DecodeError> {
        match s {
            Subscript::String(st) => Ok(st.clone()),
            other => Err(DecodeError::WrongSubscriptType {
                expected: "String",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToSubscript for serde_json::Value {
    fn to_sub(&self) -> Subscript {
        Subscript::Json(self.clone())
    }
}

impl FromSubscript for serde_json::Value {
    fn from_sub(s: &Subscript) -> Result<Self, DecodeError> {
        match s {
            Subscript::Json(j) => Ok(j.clone()),
            other => Err(DecodeError::WrongSubscriptType {
                expected: "Json",
                actual: format!("{:?}", other),
            }),
        }
    }
}

/// Convert to a RUMPS value.
pub trait ToValue {
    /// Converts `self` to a [`Value`].
    fn to_val(&self) -> Value;
}

/// Parse from a RUMPS value.
pub trait FromValue: Sized {
    /// Attempts to convert a [`Value`] to `Self`.
    fn from_val(v: &Value) -> Result<Self, DecodeError>;
}

impl ToValue for bool {
    fn to_val(&self) -> Value {
        Value::Boolean(*self)
    }
}

impl FromValue for bool {
    fn from_val(v: &Value) -> Result<Self, DecodeError> {
        match v {
            Value::Boolean(b) => Ok(*b),
            other => Err(DecodeError::WrongValueType {
                expected: "Boolean",
                actual: format!("{:?}", other),
            }),
        }
    }
}

macro_rules! impl_to_val_int {
    ($($t:ty),*) => {
        $(
            impl ToValue for $t {
                fn to_val(&self) -> Value {
                    Value::Integer(*self as i64)
                }
            }
        )*
    };
}

impl_to_val_int!(i8, i16, i32, i64, u8, u16, u32);

// `u64` and `usize` need special handling (potential overflow to i64)
impl ToValue for u64 {
    fn to_val(&self) -> Value {
        Value::Integer(*self as i64)
    }
}

impl ToValue for usize {
    fn to_val(&self) -> Value {
        Value::Integer(*self as i64)
    }
}

impl ToValue for isize {
    fn to_val(&self) -> Value {
        Value::Integer(*self as i64)
    }
}

macro_rules! impl_from_val_int {
    ($($t:ty),*) => {
        $(
            impl FromValue for $t {
                fn from_val(v: &Value) -> Result<Self, DecodeError> {
                    match v {
                        Value::Integer(i) => {
                            <$t>::try_from(*i).map_err(|_| DecodeError::IntegerOverflow {
                                value: *i,
                                target: stringify!($t),
                            })
                        }
                        other => Err(DecodeError::WrongValueType {
                            expected: "Integer",
                            actual: format!("{:?}", other),
                        }),
                    }
                }
            }
        )*
    };
}

impl_from_val_int!(i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);

impl ToValue for f32 {
    fn to_val(&self) -> Value {
        Value::Double(OrderedFloat(*self as f64))
    }
}

impl FromValue for f32 {
    fn from_val(v: &Value) -> Result<Self, DecodeError> {
        match v {
            Value::Double(d) => Ok(d.into_inner() as Self),
            Value::Integer(i) => Ok(*i as Self),
            other => Err(DecodeError::WrongValueType {
                expected: "Double",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToValue for f64 {
    fn to_val(&self) -> Value {
        Value::Double(OrderedFloat(*self))
    }
}

impl FromValue for f64 {
    fn from_val(v: &Value) -> Result<Self, DecodeError> {
        match v {
            Value::Double(d) => Ok(d.into_inner()),
            Value::Integer(i) => Ok(*i as Self),
            other => Err(DecodeError::WrongValueType {
                expected: "Double",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToValue for char {
    fn to_val(&self) -> Value {
        Value::Char(*self)
    }
}

impl FromValue for char {
    fn from_val(v: &Value) -> Result<Self, DecodeError> {
        match v {
            Value::Char(c) => Ok(*c),
            other => Err(DecodeError::WrongValueType {
                expected: "Char",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToValue for String {
    fn to_val(&self) -> Value {
        Value::String(self.clone())
    }
}

impl ToValue for &str {
    fn to_val(&self) -> Value {
        Value::String((*self).to_string())
    }
}

impl FromValue for String {
    fn from_val(v: &Value) -> Result<Self, DecodeError> {
        match v {
            Value::String(s) => Ok(s.clone()),
            other => Err(DecodeError::WrongValueType {
                expected: "String",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToValue for serde_json::Value {
    fn to_val(&self) -> Value {
        Value::Json(self.clone())
    }
}

impl FromValue for serde_json::Value {
    fn from_val(v: &Value) -> Result<Self, DecodeError> {
        match v {
            Value::Json(j) => Ok(j.clone()),
            other => Err(DecodeError::WrongValueType {
                expected: "Json",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToValue for Vec<u8> {
    fn to_val(&self) -> Value {
        // Store bytes as JSON array of integers
        Value::Json(serde_json::Value::Array(
            self.iter().map(|b| serde_json::Value::from(*b)).collect(),
        ))
    }
}

impl FromValue for Vec<u8> {
    fn from_val(v: &Value) -> Result<Self, DecodeError> {
        match v {
            Value::Json(serde_json::Value::Array(arr)) => arr
                .iter()
                .map(|v| match v {
                    serde_json::Value::Number(n) => {
                        n.as_u64().and_then(|n| u8::try_from(n).ok())
                    }
                    _ => None,
                })
                .collect::<Option<Self>>()
                .ok_or_else(|| {
                    DecodeError::Custom("invalid byte array".into())
                }),
            other => Err(DecodeError::WrongValueType {
                expected: "Json(Array)",
                actual: format!("{:?}", other),
            }),
        }
    }
}

/// Convert into a [`Key`] for ergonomic key construction.
///
/// Implemented for single values and tuples of values that implement
/// [`ToSubscript`].
///
/// # Examples
///
/// ```
/// use rumps_types::Key;
/// use rumps_types::orm::IntoKey;
///
/// // Single value
/// let key: Key = 123.into_key();
/// assert_eq!(key.len(), 1);
///
/// // Tuple
/// let key: Key = (123, "name").into_key();
/// assert_eq!(key.len(), 2);
///
/// // 3-tuple
/// let key: Key = (1, 2, 3).into_key();
/// assert_eq!(key.len(), 3);
/// ```
pub trait IntoKey {
    /// Converts `self` into a [`Key`].
    fn into_key(self) -> Key;
}

// Single values
impl<A: ToSubscript> IntoKey for A {
    fn into_key(self) -> Key {
        Key::from(vec![self.to_sub()])
    }
}

// 1-tuple (explicit, since single value covers most cases)
impl<A: ToSubscript> IntoKey for (A,) {
    fn into_key(self) -> Key {
        Key::from(vec![self.0.to_sub()])
    }
}

// 2-tuple
impl<A: ToSubscript, B: ToSubscript> IntoKey for (A, B) {
    fn into_key(self) -> Key {
        Key::from(vec![self.0.to_sub(), self.1.to_sub()])
    }
}

// 3-tuple
impl<A: ToSubscript, B: ToSubscript, C: ToSubscript> IntoKey for (A, B, C) {
    fn into_key(self) -> Key {
        Key::from(vec![self.0.to_sub(), self.1.to_sub(), self.2.to_sub()])
    }
}

// 4-tuple
impl<A: ToSubscript, B: ToSubscript, C: ToSubscript, D: ToSubscript> IntoKey
    for (A, B, C, D)
{
    fn into_key(self) -> Key {
        Key::from(vec![
            self.0.to_sub(),
            self.1.to_sub(),
            self.2.to_sub(),
            self.3.to_sub(),
        ])
    }
}

// 5-tuple
impl<
        A: ToSubscript,
        B: ToSubscript,
        C: ToSubscript,
        D: ToSubscript,
        E: ToSubscript,
    > IntoKey for (A, B, C, D, E)
{
    fn into_key(self) -> Key {
        Key::from(vec![
            self.0.to_sub(),
            self.1.to_sub(),
            self.2.to_sub(),
            self.3.to_sub(),
            self.4.to_sub(),
        ])
    }
}

// 6-tuple
impl<
        A: ToSubscript,
        B: ToSubscript,
        C: ToSubscript,
        D: ToSubscript,
        E: ToSubscript,
        F: ToSubscript,
    > IntoKey for (A, B, C, D, E, F)
{
    fn into_key(self) -> Key {
        Key::from(vec![
            self.0.to_sub(),
            self.1.to_sub(),
            self.2.to_sub(),
            self.3.to_sub(),
            self.4.to_sub(),
            self.5.to_sub(),
        ])
    }
}

// 7-tuple
impl<
        A: ToSubscript,
        B: ToSubscript,
        C: ToSubscript,
        D: ToSubscript,
        E: ToSubscript,
        F: ToSubscript,
        G: ToSubscript,
    > IntoKey for (A, B, C, D, E, F, G)
{
    fn into_key(self) -> Key {
        Key::from(vec![
            self.0.to_sub(),
            self.1.to_sub(),
            self.2.to_sub(),
            self.3.to_sub(),
            self.4.to_sub(),
            self.5.to_sub(),
            self.6.to_sub(),
        ])
    }
}

// 8-tuple
impl<
        A: ToSubscript,
        B: ToSubscript,
        C: ToSubscript,
        D: ToSubscript,
        E: ToSubscript,
        F: ToSubscript,
        G: ToSubscript,
        H: ToSubscript,
    > IntoKey for (A, B, C, D, E, F, G, H)
{
    fn into_key(self) -> Key {
        Key::from(vec![
            self.0.to_sub(),
            self.1.to_sub(),
            self.2.to_sub(),
            self.3.to_sub(),
            self.4.to_sub(),
            self.5.to_sub(),
            self.6.to_sub(),
            self.7.to_sub(),
        ])
    }
}

// Also implement for Key itself (identity)
impl IntoKey for Key {
    fn into_key(self) -> Key {
        self
    }
}

impl IntoKey for &Key {
    fn into_key(self) -> Key {
        self.clone()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ToSubscript / FromSubscript tests

    #[test]
    fn test_bool_subscript_roundtrip() {
        let sub = true.to_sub();
        assert_eq!(bool::from_sub(&sub).unwrap(), true);

        let sub = false.to_sub();
        assert_eq!(bool::from_sub(&sub).unwrap(), false);
    }

    #[test]
    fn test_integer_subscript_roundtrip() {
        let sub = 42i64.to_sub();
        assert_eq!(i64::from_sub(&sub).unwrap(), 42);

        let sub = (-100i32).to_sub();
        assert_eq!(i32::from_sub(&sub).unwrap(), -100);

        let sub = 255u8.to_sub();
        assert_eq!(u8::from_sub(&sub).unwrap(), 255);
    }

    #[test]
    fn test_float_subscript_roundtrip() {
        let sub = 3.14f64.to_sub();
        let back = f64::from_sub(&sub).unwrap();
        assert!((back - 3.14).abs() < f64::EPSILON);

        let sub = 2.5f32.to_sub();
        let back = f32::from_sub(&sub).unwrap();
        assert!((back - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_char_subscript_roundtrip() {
        let sub = 'A'.to_sub();
        assert_eq!(char::from_sub(&sub).unwrap(), 'A');

        let sub = '😀'.to_sub();
        assert_eq!(char::from_sub(&sub).unwrap(), '😀');
    }

    #[test]
    fn test_string_subscript_roundtrip() {
        let sub = "hello".to_sub();
        assert_eq!(String::from_sub(&sub).unwrap(), "hello");

        let sub = String::from("world").to_sub();
        assert_eq!(String::from_sub(&sub).unwrap(), "world");
    }

    #[test]
    fn test_subscript_type_mismatch() {
        let sub = 42i64.to_sub();
        assert!(String::from_sub(&sub).is_err());

        let sub = "hello".to_sub();
        assert!(i64::from_sub(&sub).is_err());
    }

    #[test]
    fn test_integer_overflow() {
        let sub = 1000i64.to_sub();
        assert!(u8::from_sub(&sub).is_err());

        let sub = (-1i64).to_sub();
        assert!(u32::from_sub(&sub).is_err());
    }

    // ToValue / FromValue tests

    #[test]
    fn test_bool_value_roundtrip() {
        let val = true.to_val();
        assert_eq!(bool::from_val(&val).unwrap(), true);
    }

    #[test]
    fn test_integer_value_roundtrip() {
        let val = 42i64.to_val();
        assert_eq!(i64::from_val(&val).unwrap(), 42);

        let val = 100u32.to_val();
        assert_eq!(u32::from_val(&val).unwrap(), 100);
    }

    #[test]
    fn test_float_value_roundtrip() {
        let val = 3.14f64.to_val();
        let back = f64::from_val(&val).unwrap();
        assert!((back - 3.14).abs() < f64::EPSILON);

        // Integer can be read as float
        let val = 42i64.to_val();
        let back = f64::from_val(&val).unwrap();
        assert!((back - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_string_value_roundtrip() {
        let val = "hello".to_val();
        assert_eq!(String::from_val(&val).unwrap(), "hello");
    }

    #[test]
    fn test_bytes_value_roundtrip() {
        let bytes = vec![1u8, 2, 3, 4, 5];
        let val = bytes.to_val();
        let back = Vec::<u8>::from_val(&val).unwrap();
        assert_eq!(back, vec![1, 2, 3, 4, 5]);
    }

    // IntoKey tests

    #[test]
    fn test_single_into_key() {
        let key: Key = 123i64.into_key();
        assert_eq!(key.len(), 1);
    }

    #[test]
    fn test_tuple_into_key() {
        let key: Key = (123, "name").into_key();
        assert_eq!(key.len(), 2);

        let key: Key = (1, 2, 3).into_key();
        assert_eq!(key.len(), 3);

        let key: Key = (1, 2, 3, 4).into_key();
        assert_eq!(key.len(), 4);

        let key: Key = (1, 2, 3, 4, 5, 6, 7, 8).into_key();
        assert_eq!(key.len(), 8);
    }

    #[test]
    fn test_key_identity() {
        let original =
            Key::from(vec![Subscript::from(1), Subscript::from("test")]);
        let key: Key = original.clone().into_key();
        assert_eq!(key, original);
    }
}
