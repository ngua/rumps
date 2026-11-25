//! Wire encoding types for JSON-based RPC communication.
//!
//! These newtypes wrap core RUMPS types with custom serde implementations
//! that use JSON primitive inference for the common case and `$`-prefixed
//! tags for exotic types (`Char`, `Json`).

use ordered_float::OrderedFloat;
use rumps_types;
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
}
