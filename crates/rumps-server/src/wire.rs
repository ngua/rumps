//! Wire encoding types for JSON-based RPC communication.
//!
//! These newtypes wrap core RUMPS types with custom serde implementations
//! that use JSON primitive inference for the common case and `$`-prefixed
//! tags for exotic types (`Char`, `Json`).

use ordered_float::OrderedFloat;
use serde::ser::SerializeMap;
use serde::{de, Deserialize, Serialize};

/// Wire-format wrapper for [`rumps_types::Subscript`] with JSON-native serialization.
///
/// Deserialization infers the type from JSON primitives:
/// - `true`/`false` -> `rumps_types::Subscript::Boolean`
/// - integer -> `rumps_types::Subscript::Number` (as `f64`)
/// - float -> `rumps_types::Subscript::Number`
/// - string -> `rumps_types::Subscript::String`
/// - object -> parsed as `$`-prefixed tag; error if unrecognized
///
/// Serialization uses bare JSON for `Boolean`, `Number`, `String`,
/// and `$`-prefixed tags for `Char` and `Json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Subscript(rumps_types::Subscript);

impl From<rumps_types::Subscript> for Subscript {
    fn from(s: rumps_types::Subscript) -> Self {
        Self(s)
    }
}

impl From<Subscript> for rumps_types::Subscript {
    fn from(w: Subscript) -> Self {
        w.0
    }
}

impl Serialize for Subscript {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            rumps_types::Subscript::Boolean(b) => ser.serialize_bool(*b),
            rumps_types::Subscript::Number(n) => {
                ser.serialize_f64(n.into_inner())
            }
            rumps_types::Subscript::String(s) => ser.serialize_str(s),
            rumps_types::Subscript::Char(c) => {
                let mut map = ser.serialize_map(Some(1))?;
                map.serialize_entry("$char", &c.to_string())?;
                map.end()
            }
            rumps_types::Subscript::Json(j) => {
                let mut map = ser.serialize_map(Some(1))?;
                map.serialize_entry("$json", j)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Subscript {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;

        match v {
            serde_json::Value::Bool(b) => {
                Ok(Self(rumps_types::Subscript::Boolean(b)))
            }
            serde_json::Value::Number(n) => {
                Ok(Self(rumps_types::Subscript::Number(OrderedFloat(
                    n.as_f64().ok_or_else(|| {
                        de::Error::custom("numeric value out of `f64` range")
                    })?,
                ))))
            }
            serde_json::Value::String(s) => {
                Ok(Self(rumps_types::Subscript::String(s)))
            }
            serde_json::Value::Object(map) if map.len() == 1 => {
                let (tag, val) = map
                    .into_iter()
                    .next()
                    .ok_or_else(|| de::Error::custom("empty tagged object"))?;

                match tag.as_str() {
                    "$bool" => val
                        .as_bool()
                        .map(|b| Self(rumps_types::Subscript::Boolean(b)))
                        .ok_or_else(|| {
                            de::Error::custom("`$bool` requires a boolean")
                        }),
                    "$number" => val
                        .as_f64()
                        .map(|n| {
                            Self(rumps_types::Subscript::Number(OrderedFloat(
                                n,
                            )))
                        })
                        .ok_or_else(|| {
                            de::Error::custom("`$number` requires a number")
                        }),
                    "$str" => match val {
                        serde_json::Value::String(s) => {
                            Ok(Self(rumps_types::Subscript::String(s)))
                        }
                        _ => Err(de::Error::custom("`$str` requires a string")),
                    },
                    "$char" => {
                        let s = val.as_str().ok_or_else(|| {
                            de::Error::custom("`$char` requires a string")
                        })?;
                        let mut chars = s.chars();
                        let c = chars.next().ok_or_else(|| {
                            de::Error::custom(
                                "`$char` requires exactly one character",
                            )
                        })?;
                        if chars.next().is_some() {
                            Err(de::Error::custom(
                                "`$char` requires exactly one character",
                            ))
                        } else {
                            Ok(Self(rumps_types::Subscript::Char(c)))
                        }
                    }
                    "$json" => Ok(Self(rumps_types::Subscript::Json(val))),
                    other => Err(de::Error::custom(format!(
                        "unrecognized tag `{other}`"
                    ))),
                }
            }
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                Err(de::Error::custom(
                    "objects and arrays must use `$json` tag for subscripts",
                ))
            }
            serde_json::Value::Null => {
                Err(de::Error::custom("`null` is not a valid subscript"))
            }
        }
    }
}

/// Wire-format wrapper for [`rumps_types::Value`] with JSON-native serialization.
///
/// Deserialization infers the type from JSON primitives:
/// - `true`/`false` -> `rumps_types::Value::Boolean`
/// - integer (no fractional part) -> `rumps_types::Value::Integer`
/// - float (has fractional part) -> `rumps_types::Value::Double`
/// - string -> `rumps_types::Value::String`
/// - object -> parsed as `$`-prefixed tag; error if unrecognized
///
/// Serialization uses bare JSON for `Boolean`, `Integer`, `Double`, `String`,
/// and `$`-prefixed tags for `Char` and `Json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Value(rumps_types::Value);

impl From<rumps_types::Value> for Value {
    fn from(v: rumps_types::Value) -> Self {
        Self(v)
    }
}

impl From<Value> for rumps_types::Value {
    fn from(w: Value) -> Self {
        w.0
    }
}

impl Serialize for Value {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            rumps_types::Value::Boolean(b) => ser.serialize_bool(*b),
            rumps_types::Value::Integer(n) => ser.serialize_i64(*n),
            rumps_types::Value::Double(n) => ser.serialize_f64(n.into_inner()),
            rumps_types::Value::String(s) => ser.serialize_str(s),
            rumps_types::Value::Char(c) => {
                let mut map = ser.serialize_map(Some(1))?;
                map.serialize_entry("$char", &c.to_string())?;
                map.end()
            }
            rumps_types::Value::Json(j) => {
                let mut map = ser.serialize_map(Some(1))?;
                map.serialize_entry("$json", j)?;
                map.end()
            }
        }
    }
}

// Note that this impl is extremely close to that of `Subscript`, only differing
// in having numeric type tags `$float`/`$int` rather than a `$number`. This
// is for consistency with RUMPS semantics
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;

        match v {
            serde_json::Value::Bool(b) => {
                Ok(Self(rumps_types::Value::Boolean(b)))
            }
            serde_json::Value::Number(n) => {
                if n.is_i64() {
                    Ok(Self(rumps_types::Value::Integer(
                        n.as_i64().ok_or_else(|| {
                            de::Error::custom(
                                "numeric value out of `i64` range",
                            )
                        })?,
                    )))
                } else {
                    Ok(Self(rumps_types::Value::Double(OrderedFloat(
                        n.as_f64().ok_or_else(|| {
                            de::Error::custom(
                                "numeric value out of `f64` range",
                            )
                        })?,
                    ))))
                }
            }
            serde_json::Value::String(s) => {
                Ok(Self(rumps_types::Value::String(s)))
            }
            serde_json::Value::Object(map) if map.len() == 1 => {
                let (tag, val) = map
                    .into_iter()
                    .next()
                    .ok_or_else(|| de::Error::custom("empty tagged object"))?;

                match tag.as_str() {
                    "$bool" => val
                        .as_bool()
                        .map(|b| Self(rumps_types::Value::Boolean(b)))
                        .ok_or_else(|| {
                            de::Error::custom("`$bool` requires a boolean")
                        }),
                    "$int" => val
                        .as_i64()
                        .map(|n| Self(rumps_types::Value::Integer(n)))
                        .ok_or_else(|| {
                            de::Error::custom("`$int` requires an integer")
                        }),
                    "$float" => val
                        .as_f64()
                        .map(|n| {
                            Self(rumps_types::Value::Double(OrderedFloat(n)))
                        })
                        .ok_or_else(|| {
                            de::Error::custom("`$float` requires a number")
                        }),
                    "$str" => match val {
                        serde_json::Value::String(s) => {
                            Ok(Self(rumps_types::Value::String(s)))
                        }
                        _ => Err(de::Error::custom("`$str` requires a string")),
                    },
                    "$char" => {
                        let s = val.as_str().ok_or_else(|| {
                            de::Error::custom("`$char` requires a string")
                        })?;
                        let mut chars = s.chars();
                        let c = chars.next().ok_or_else(|| {
                            de::Error::custom(
                                "`$char` requires exactly one character",
                            )
                        })?;
                        if chars.next().is_some() {
                            Err(de::Error::custom(
                                "`$char` requires exactly one character",
                            ))
                        } else {
                            Ok(Self(rumps_types::Value::Char(c)))
                        }
                    }
                    "$json" => Ok(Self(rumps_types::Value::Json(val))),
                    other => Err(de::Error::custom(format!(
                        "unrecognized tag `{other}`"
                    ))),
                }
            }
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                Err(de::Error::custom(
                    "objects and arrays must use `$json` tag for values",
                ))
            }
            serde_json::Value::Null => {
                Err(de::Error::custom("`null` is not a valid value"))
            }
        }
    }
}

/// Wire-format wrapper for [`rumps_types::Key`].
///
/// Serialized as a JSON array of [`Subscript`] values. An empty array
/// represents `Key::new()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Key(Vec<Subscript>);

impl From<rumps_types::Key> for Key {
    fn from(k: rumps_types::Key) -> Self {
        Self(k.into_iter().map(Subscript::from).collect())
    }
}

impl From<Key> for rumps_types::Key {
    fn from(w: Key) -> Self {
        w.0.into_iter().map(rumps_types::Subscript::from).collect()
    }
}

/// Wire-format wrapper for [`rumps_types::Increment`].
///
/// Deserialization infers from JSON numbers: integers -> `Integer`,
/// floats -> `Double`. Explicit `$int`/`$float` tags are also supported.
/// Serialization uses bare JSON integers/floats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Increment(rumps_types::Increment);

impl From<rumps_types::Increment> for Increment {
    fn from(i: rumps_types::Increment) -> Self {
        Self(i)
    }
}

impl From<Increment> for rumps_types::Increment {
    fn from(w: Increment) -> Self {
        w.0
    }
}

impl Serialize for Increment {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            rumps_types::Increment::Integer(n) => ser.serialize_i64(*n),
            rumps_types::Increment::Double(n) => {
                ser.serialize_f64(n.into_inner())
            }
        }
    }
}

impl<'de> Deserialize<'de> for Increment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;

        match v {
            serde_json::Value::Number(n) => {
                if n.is_i64() {
                    Ok(Self(rumps_types::Increment::Integer(
                        n.as_i64().ok_or_else(|| {
                            de::Error::custom(
                                "numeric value out of `i64` range",
                            )
                        })?,
                    )))
                } else {
                    Ok(Self(rumps_types::Increment::Double(OrderedFloat(
                        n.as_f64().ok_or_else(|| {
                            de::Error::custom(
                                "numeric value out of `f64` range",
                            )
                        })?,
                    ))))
                }
            }
            serde_json::Value::Object(map) if map.len() == 1 => {
                let (tag, val) = map
                    .into_iter()
                    .next()
                    .ok_or_else(|| de::Error::custom("empty tagged object"))?;

                match tag.as_str() {
                    "$int" => val
                        .as_i64()
                        .map(|n| Self(rumps_types::Increment::Integer(n)))
                        .ok_or_else(|| {
                            de::Error::custom("`$int` requires an integer")
                        }),
                    "$float" => val
                        .as_f64()
                        .map(|n| {
                            Self(rumps_types::Increment::Double(OrderedFloat(
                                n,
                            )))
                        })
                        .ok_or_else(|| {
                            de::Error::custom("`$float` requires a number")
                        }),
                    other => Err(de::Error::custom(format!(
                        "unrecognized tag `{other}`"
                    ))),
                }
            }
            _ => Err(de::Error::custom(
                "increment must be a number or a `$int`/`$float` tag",
            )),
        }
    }
}

/// Wire-format wrapper for [`rumps_types::DataStatus`].
///
/// Serialized as the MUMPS `$DATA` return code: `0` (no data), `1` (has
/// value), `10` (has descendants), `11` (has both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DataStatus(rumps_types::DataStatus);

impl From<rumps_types::DataStatus> for DataStatus {
    fn from(d: rumps_types::DataStatus) -> Self {
        Self(d)
    }
}

impl From<DataStatus> for rumps_types::DataStatus {
    fn from(w: DataStatus) -> Self {
        w.0
    }
}

impl Serialize for DataStatus {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ser.serialize_u8(self.0 as u8)
    }
}

impl<'de> Deserialize<'de> for DataStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let n = u8::deserialize(deserializer)?;

        match n {
            0 => Ok(Self(rumps_types::DataStatus::NoData)),
            1 => Ok(Self(rumps_types::DataStatus::HasValue)),
            10 => Ok(Self(rumps_types::DataStatus::HasDescendants)),
            11 => Ok(Self(rumps_types::DataStatus::Both)),
            _ => Err(de::Error::custom(format!(
                "invalid `data` code `{n}`; expected `0`, `1`, `10`, or `11`"
            ))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn deser_bool() {
        let ws: Subscript = serde_json::from_value(json!(true)).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Boolean(true));

        let ws: Subscript = serde_json::from_value(json!(false)).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Boolean(false));
    }

    #[test]
    fn deser_integer_as_number() {
        let ws: Subscript = serde_json::from_value(json!(123)).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Number(OrderedFloat(123.0)));
    }

    #[test]
    fn deser_float_as_number() {
        let ws: Subscript = serde_json::from_value(json!(1.5)).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Number(OrderedFloat(1.5)));
    }

    #[test]
    fn deser_string() {
        let ws: Subscript = serde_json::from_value(json!("NAME")).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::String("NAME".into()));
    }

    #[test]
    fn deser_explicit_bool_tag() {
        let ws: Subscript =
            serde_json::from_value(json!({"$bool": true})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Boolean(true));
    }

    #[test]
    fn deser_explicit_number_tag() {
        let ws: Subscript =
            serde_json::from_value(json!({"$number": 42})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Number(OrderedFloat(42.0)));

        let ws: Subscript =
            serde_json::from_value(json!({"$number": 3.14})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Number(OrderedFloat(3.14)));
    }

    #[test]
    fn deser_explicit_str_tag() {
        let ws: Subscript =
            serde_json::from_value(json!({"$str": "hello"})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::String("hello".into()));
    }

    #[test]
    fn deser_explicit_char_tag() {
        let ws: Subscript =
            serde_json::from_value(json!({"$char": "A"})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Char('A'));
    }

    #[test]
    fn deser_char_tag_multi_char_rejected() {
        let r: Result<Subscript, _> =
            serde_json::from_value(json!({"$char": "AB"}));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("exactly one character"));
    }

    #[test]
    fn deser_char_tag_non_string_rejected() {
        let r: Result<Subscript, _> =
            serde_json::from_value(json!({"$char": 65}));
        assert!(r.is_err());
    }

    #[test]
    fn deser_explicit_json_tag_object() {
        let ws: Subscript =
            serde_json::from_value(json!({"$json": {"a": 1}})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Json(json!({"a": 1})));
    }

    #[test]
    fn deser_explicit_json_tag_array() {
        let ws: Subscript =
            serde_json::from_value(json!({"$json": [1, 2]})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Json(json!([1, 2])));
    }

    #[test]
    fn deser_explicit_json_tag_null() {
        let ws: Subscript =
            serde_json::from_value(json!({"$json": null})).unwrap();
        assert_eq!(ws.0, rumps_types::Subscript::Json(serde_json::Value::Null));
    }

    #[test]
    fn deser_bare_object_rejected() {
        let r: Result<Subscript, _> = serde_json::from_value(json!({"a": 1}));
        assert!(r.is_err());
    }

    #[test]
    fn deser_bare_array_rejected() {
        let r: Result<Subscript, _> = serde_json::from_value(json!([1, 2]));
        assert!(r.is_err());
    }

    #[test]
    fn deser_null_rejected() {
        let r: Result<Subscript, _> = serde_json::from_value(json!(null));
        assert!(r.is_err());
    }

    #[test]
    fn deser_unrecognized_tag_rejected() {
        let r: Result<Subscript, _> =
            serde_json::from_value(json!({"$unknown": 1}));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("unrecognized tag"));
    }

    #[test]
    fn ser_bool() {
        let ws = Subscript(rumps_types::Subscript::Boolean(true));
        assert_eq!(serde_json::to_value(&ws).unwrap(), json!(true));
    }

    #[test]
    fn ser_number_integer() {
        let ws = Subscript(rumps_types::Subscript::Number(OrderedFloat(42.0)));
        assert_eq!(serde_json::to_value(&ws).unwrap(), json!(42.0));
    }

    #[test]
    fn ser_number_float() {
        let ws = Subscript(rumps_types::Subscript::Number(OrderedFloat(3.14)));
        assert_eq!(serde_json::to_value(&ws).unwrap(), json!(3.14));
    }

    #[test]
    fn ser_string() {
        let ws = Subscript(rumps_types::Subscript::String("foo".into()));
        assert_eq!(serde_json::to_value(&ws).unwrap(), json!("foo"));
    }

    #[test]
    fn ser_char_uses_tag() {
        let ws = Subscript(rumps_types::Subscript::Char('Z'));
        assert_eq!(serde_json::to_value(&ws).unwrap(), json!({"$char": "Z"}));
    }

    #[test]
    fn ser_json_uses_tag() {
        let ws = Subscript(rumps_types::Subscript::Json(json!({"a": 1})));
        assert_eq!(
            serde_json::to_value(&ws).unwrap(),
            json!({"$json": {"a": 1}})
        );
    }

    #[test]
    fn round_trip_all_variants() {
        let cases = vec![
            Subscript(rumps_types::Subscript::Boolean(false)),
            Subscript(rumps_types::Subscript::Number(OrderedFloat(-10.0))),
            Subscript(rumps_types::Subscript::Number(OrderedFloat(0.0))),
            Subscript(rumps_types::Subscript::Number(OrderedFloat(1.5))),
            Subscript(rumps_types::Subscript::Char('X')),
            Subscript(rumps_types::Subscript::String("hello".into())),
            Subscript(rumps_types::Subscript::Json(json!({"nested": [1, 2]}))),
        ];

        cases.iter().for_each(|ws| {
            let json = serde_json::to_value(ws).unwrap();
            let back: Subscript = serde_json::from_value(json).unwrap();
            assert_eq!(&back, ws);
        });
    }

    #[test]
    fn value_deser_bool() {
        let wv: Value = serde_json::from_value(json!(true)).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Boolean(true));

        let wv: Value = serde_json::from_value(json!(false)).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Boolean(false));
    }

    #[test]
    fn value_deser_integer() {
        let wv: Value = serde_json::from_value(json!(42)).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Integer(42));
    }

    #[test]
    fn value_deser_negative_integer() {
        let wv: Value = serde_json::from_value(json!(-7)).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Integer(-7));
    }

    #[test]
    fn value_deser_float() {
        let wv: Value = serde_json::from_value(json!(3.14)).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Double(OrderedFloat(3.14)));
    }

    #[test]
    fn value_deser_string() {
        let wv: Value = serde_json::from_value(json!("Alice")).unwrap();
        assert_eq!(wv.0, rumps_types::Value::String("Alice".into()));
    }

    #[test]
    fn value_deser_explicit_bool_tag() {
        let wv: Value = serde_json::from_value(json!({"$bool": true})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Boolean(true));
    }

    #[test]
    fn value_deser_explicit_int_tag() {
        let wv: Value = serde_json::from_value(json!({"$int": 42})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Integer(42));
    }

    #[test]
    fn value_deser_explicit_float_tag() {
        let wv: Value = serde_json::from_value(json!({"$float": 42})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Double(OrderedFloat(42.0)));

        let wv: Value =
            serde_json::from_value(json!({"$float": 3.14})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Double(OrderedFloat(3.14)));
    }

    #[test]
    fn value_deser_explicit_str_tag() {
        let wv: Value =
            serde_json::from_value(json!({"$str": "hello"})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::String("hello".into()));
    }

    #[test]
    fn value_deser_explicit_char_tag() {
        let wv: Value = serde_json::from_value(json!({"$char": "X"})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Char('X'));
    }

    #[test]
    fn value_deser_char_tag_multi_char_rejected() {
        let r: Result<Value, _> =
            serde_json::from_value(json!({"$char": "AB"}));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("exactly one character"));
    }

    #[test]
    fn value_deser_char_tag_non_string_rejected() {
        let r: Result<Value, _> = serde_json::from_value(json!({"$char": 65}));
        assert!(r.is_err());
    }

    #[test]
    fn value_deser_explicit_json_tag_object() {
        let wv: Value =
            serde_json::from_value(json!({"$json": {"a": 1}})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Json(json!({"a": 1})));
    }

    #[test]
    fn value_deser_explicit_json_tag_array() {
        let wv: Value =
            serde_json::from_value(json!({"$json": [1, 2, 3]})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Json(json!([1, 2, 3])));
    }

    #[test]
    fn value_deser_explicit_json_tag_null() {
        let wv: Value = serde_json::from_value(json!({"$json": null})).unwrap();
        assert_eq!(wv.0, rumps_types::Value::Json(serde_json::Value::Null));
    }

    #[test]
    fn value_deser_bare_object_rejected() {
        let r: Result<Value, _> = serde_json::from_value(json!({"a": 1}));
        assert!(r.is_err());
    }

    #[test]
    fn value_deser_bare_array_rejected() {
        let r: Result<Value, _> = serde_json::from_value(json!([1, 2]));
        assert!(r.is_err());
    }

    #[test]
    fn value_deser_null_rejected() {
        let r: Result<Value, _> = serde_json::from_value(json!(null));
        assert!(r.is_err());
    }

    #[test]
    fn value_deser_unrecognized_tag_rejected() {
        let r: Result<Value, _> =
            serde_json::from_value(json!({"$unknown": 1}));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("unrecognized tag"));
    }

    #[test]
    fn value_deser_int_tag_rejects_non_integer() {
        let r: Result<Value, _> =
            serde_json::from_value(json!({"$int": "nope"}));
        assert!(r.is_err());
    }

    #[test]
    fn value_deser_float_tag_rejects_non_number() {
        let r: Result<Value, _> =
            serde_json::from_value(json!({"$float": "nope"}));
        assert!(r.is_err());
    }

    #[test]
    fn value_ser_bool() {
        let wv = Value(rumps_types::Value::Boolean(true));
        assert_eq!(serde_json::to_value(&wv).unwrap(), json!(true));
    }

    #[test]
    fn value_ser_integer() {
        let wv = Value(rumps_types::Value::Integer(42));
        assert_eq!(serde_json::to_value(&wv).unwrap(), json!(42));
    }

    #[test]
    fn value_ser_double() {
        let wv = Value(rumps_types::Value::Double(OrderedFloat(3.14)));
        assert_eq!(serde_json::to_value(&wv).unwrap(), json!(3.14));
    }

    #[test]
    fn value_ser_string() {
        let wv = Value(rumps_types::Value::String("foo".into()));
        assert_eq!(serde_json::to_value(&wv).unwrap(), json!("foo"));
    }

    #[test]
    fn value_ser_char_uses_tag() {
        let wv = Value(rumps_types::Value::Char('Z'));
        assert_eq!(serde_json::to_value(&wv).unwrap(), json!({"$char": "Z"}));
    }

    #[test]
    fn value_ser_json_uses_tag() {
        let wv = Value(rumps_types::Value::Json(json!({"a": 1})));
        assert_eq!(
            serde_json::to_value(&wv).unwrap(),
            json!({"$json": {"a": 1}})
        );
    }

    #[test]
    fn value_round_trip_all_variants() {
        let cases = vec![
            Value(rumps_types::Value::Boolean(false)),
            Value(rumps_types::Value::Integer(-10)),
            Value(rumps_types::Value::Integer(0)),
            Value(rumps_types::Value::Integer(42)),
            Value(rumps_types::Value::Double(OrderedFloat(1.5))),
            Value(rumps_types::Value::Double(OrderedFloat(-0.001))),
            Value(rumps_types::Value::Char('X')),
            Value(rumps_types::Value::String("hello".into())),
            Value(rumps_types::Value::Json(json!({"nested": [1, 2]}))),
            Value(rumps_types::Value::Json(json!(null))),
        ];

        cases.iter().for_each(|wv| {
            let json = serde_json::to_value(wv).unwrap();
            let back: Value = serde_json::from_value(json).unwrap();
            assert_eq!(&back, wv);
        });
    }

    #[test]
    fn key_deser_empty() {
        let wk: Key = serde_json::from_value(json!([])).unwrap();
        let k: rumps_types::Key = wk.into();
        assert!(k.is_empty());
    }

    #[test]
    fn key_deser_multi_subscript() {
        let wk: Key = serde_json::from_value(json!([123, "NAME"])).unwrap();
        let k: rumps_types::Key = wk.into();
        assert_eq!(k.len(), 2);
        assert_eq!(
            k.get(0),
            Some(&rumps_types::Subscript::Number(OrderedFloat(123.0)))
        );
        assert_eq!(
            k.get(1),
            Some(&rumps_types::Subscript::String("NAME".into()))
        );
    }

    #[test]
    fn key_deser_mixed_types() {
        let wk: Key = serde_json::from_value(
            json!([true, 1, {"$char": "A"}, "foo", {"$json": null}]),
        )
        .unwrap();
        let k: rumps_types::Key = wk.into();
        assert_eq!(k.len(), 5);
        assert_eq!(k.get(0), Some(&rumps_types::Subscript::Boolean(true)));
        assert_eq!(
            k.get(1),
            Some(&rumps_types::Subscript::Number(OrderedFloat(1.0)))
        );
        assert_eq!(k.get(2), Some(&rumps_types::Subscript::Char('A')));
        assert_eq!(
            k.get(3),
            Some(&rumps_types::Subscript::String("foo".into()))
        );
        assert_eq!(
            k.get(4),
            Some(&rumps_types::Subscript::Json(serde_json::Value::Null))
        );
    }

    #[test]
    fn key_ser_empty() {
        let wk = Key::from(rumps_types::Key::new());
        assert_eq!(serde_json::to_value(&wk).unwrap(), json!([]));
    }

    #[test]
    fn key_ser_multi_subscript() {
        let k = rumps_types::Key::from(vec![
            rumps_types::Subscript::Number(OrderedFloat(123.0)),
            rumps_types::Subscript::String("NAME".into()),
        ]);
        let wk = Key::from(k);
        assert_eq!(serde_json::to_value(&wk).unwrap(), json!([123.0, "NAME"]));
    }

    #[test]
    fn key_round_trip() {
        let k = rumps_types::Key::from(vec![
            rumps_types::Subscript::Boolean(false),
            rumps_types::Subscript::Number(OrderedFloat(42.0)),
            rumps_types::Subscript::Char('Z'),
            rumps_types::Subscript::String("test".into()),
            rumps_types::Subscript::Json(json!([1])),
        ]);
        let wk = Key::from(k.clone());
        let json = serde_json::to_value(&wk).unwrap();
        let back: Key = serde_json::from_value(json).unwrap();
        let k2: rumps_types::Key = back.into();
        assert_eq!(k, k2);
    }

    #[test]
    fn key_from_into_roundtrip() {
        let k = rumps_types::Key::from(vec![
            rumps_types::Subscript::Number(OrderedFloat(1.0)),
            rumps_types::Subscript::String("x".into()),
        ]);
        let wk = Key::from(k.clone());
        let k2: rumps_types::Key = wk.into();
        assert_eq!(k, k2);
    }

    #[test]
    fn incr_deser_integer() {
        let wi: Increment = serde_json::from_value(json!(5)).unwrap();
        assert_eq!(wi.0, rumps_types::Increment::Integer(5));
    }

    #[test]
    fn incr_deser_negative_integer() {
        let wi: Increment = serde_json::from_value(json!(-3)).unwrap();
        assert_eq!(wi.0, rumps_types::Increment::Integer(-3));
    }

    #[test]
    fn incr_deser_float() {
        let wi: Increment = serde_json::from_value(json!(1.5)).unwrap();
        assert_eq!(wi.0, rumps_types::Increment::Double(OrderedFloat(1.5)));
    }

    #[test]
    fn incr_deser_explicit_int_tag() {
        let wi: Increment =
            serde_json::from_value(json!({"$int": 42})).unwrap();
        assert_eq!(wi.0, rumps_types::Increment::Integer(42));
    }

    #[test]
    fn incr_deser_explicit_float_tag() {
        let wi: Increment =
            serde_json::from_value(json!({"$float": 1})).unwrap();
        assert_eq!(wi.0, rumps_types::Increment::Double(OrderedFloat(1.0)));
    }

    #[test]
    fn incr_deser_string_rejected() {
        let r: Result<Increment, _> = serde_json::from_value(json!("nope"));
        assert!(r.is_err());
    }

    #[test]
    fn incr_deser_bool_rejected() {
        let r: Result<Increment, _> = serde_json::from_value(json!(true));
        assert!(r.is_err());
    }

    #[test]
    fn incr_deser_null_rejected() {
        let r: Result<Increment, _> = serde_json::from_value(json!(null));
        assert!(r.is_err());
    }

    #[test]
    fn incr_deser_unrecognized_tag_rejected() {
        let r: Result<Increment, _> =
            serde_json::from_value(json!({"$str": "1"}));
        assert!(r.is_err());
    }

    #[test]
    fn incr_ser_integer() {
        let wi = Increment(rumps_types::Increment::Integer(5));
        assert_eq!(serde_json::to_value(&wi).unwrap(), json!(5));
    }

    #[test]
    fn incr_ser_double() {
        let wi = Increment(rumps_types::Increment::Double(OrderedFloat(1.5)));
        assert_eq!(serde_json::to_value(&wi).unwrap(), json!(1.5));
    }

    #[test]
    fn incr_round_trip() {
        let cases = vec![
            Increment(rumps_types::Increment::Integer(0)),
            Increment(rumps_types::Increment::Integer(-100)),
            Increment(rumps_types::Increment::Double(OrderedFloat(3.14))),
        ];

        cases.iter().for_each(|wi| {
            let json = serde_json::to_value(wi).unwrap();
            let back: Increment = serde_json::from_value(json).unwrap();
            assert_eq!(&back, wi);
        });
    }

    #[test]
    fn data_status_ser_all() {
        let cases = [
            (rumps_types::DataStatus::NoData, 0),
            (rumps_types::DataStatus::HasValue, 1),
            (rumps_types::DataStatus::HasDescendants, 10),
            (rumps_types::DataStatus::Both, 11),
        ];
        cases.iter().for_each(|(status, code)| {
            let ws = DataStatus::from(*status);
            assert_eq!(serde_json::to_value(&ws).unwrap(), json!(code));
        });
    }

    #[test]
    fn data_status_deser_all() {
        let cases = [
            (0, rumps_types::DataStatus::NoData),
            (1, rumps_types::DataStatus::HasValue),
            (10, rumps_types::DataStatus::HasDescendants),
            (11, rumps_types::DataStatus::Both),
        ];
        cases.iter().for_each(|(code, expected)| {
            let ws: DataStatus = serde_json::from_value(json!(code)).unwrap();
            assert_eq!(ws.0, *expected);
        });
    }

    #[test]
    fn data_status_deser_invalid_code_rejected() {
        [2, 3, 12, 100, 255].iter().for_each(|code| {
            let r: Result<DataStatus, _> = serde_json::from_value(json!(code));
            assert!(r.is_err());
        });
    }

    #[test]
    fn data_status_deser_non_integer_rejected() {
        let r: Result<DataStatus, _> = serde_json::from_value(json!("both"));
        assert!(r.is_err());

        let r: Result<DataStatus, _> = serde_json::from_value(json!(1.5));
        assert!(r.is_err());
    }

    #[test]
    fn data_status_round_trip() {
        [
            rumps_types::DataStatus::NoData,
            rumps_types::DataStatus::HasValue,
            rumps_types::DataStatus::HasDescendants,
            rumps_types::DataStatus::Both,
        ]
        .iter()
        .for_each(|status| {
            let ws = DataStatus::from(*status);
            let json = serde_json::to_value(&ws).unwrap();
            let back: DataStatus = serde_json::from_value(json).unwrap();
            assert_eq!(ws, back);
        });
    }
}
