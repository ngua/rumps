//! Conversions between Rust types and RUMPS [`Value`]s.

use ordered_float::OrderedFloat;

use crate::{StorageError, Value};

/// Convert to a RUMPS value.
pub trait ToValue {
    /// Converts `self` to a [`Value`].
    fn to_val(&self) -> Value;
}

/// Parse from a RUMPS value.
pub trait FromValue: Sized {
    /// Attempts to convert a [`Value`] to `Self`.
    fn from_val(v: &Value) -> Result<Self, StorageError>;
}

impl ToValue for bool {
    fn to_val(&self) -> Value {
        Value::Boolean(*self)
    }
}

impl FromValue for bool {
    fn from_val(v: &Value) -> Result<Self, StorageError> {
        match v {
            Value::Boolean(b) => Ok(*b),
            other => Err(StorageError::WrongValueType {
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
                fn from_val(v: &Value) -> Result<Self, StorageError> {
                    match v {
                        Value::Integer(i) => {
                            <$t>::try_from(*i).map_err(|_| StorageError::IntegerOverflow {
                                value: *i,
                                target: stringify!($t),
                            })
                        }
                        other => Err(StorageError::WrongValueType {
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
    fn from_val(v: &Value) -> Result<Self, StorageError> {
        match v {
            Value::Double(d) => Ok(d.into_inner() as Self),
            Value::Integer(i) => Ok(*i as Self),
            other => Err(StorageError::WrongValueType {
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
    fn from_val(v: &Value) -> Result<Self, StorageError> {
        match v {
            Value::Double(d) => Ok(d.into_inner()),
            Value::Integer(i) => Ok(*i as Self),
            other => Err(StorageError::WrongValueType {
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
    fn from_val(v: &Value) -> Result<Self, StorageError> {
        match v {
            Value::Char(c) => Ok(*c),
            other => Err(StorageError::WrongValueType {
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
    fn from_val(v: &Value) -> Result<Self, StorageError> {
        match v {
            Value::String(s) => Ok(s.clone()),
            other => Err(StorageError::WrongValueType {
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
    fn from_val(v: &Value) -> Result<Self, StorageError> {
        match v {
            Value::Json(j) => Ok(j.clone()),
            other => Err(StorageError::WrongValueType {
                expected: "Json",
                actual: format!("{:?}", other),
            }),
        }
    }
}

impl ToValue for Vec<u8> {
    fn to_val(&self) -> Value {
        Value::Json(serde_json::Value::Array(
            self.iter().map(|b| serde_json::Value::from(*b)).collect(),
        ))
    }
}

impl FromValue for Vec<u8> {
    fn from_val(v: &Value) -> Result<Self, StorageError> {
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
                    StorageError::Decode("invalid byte array".into())
                }),
            other => Err(StorageError::WrongValueType {
                expected: "Json(Array)",
                actual: format!("{:?}", other),
            }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
}
