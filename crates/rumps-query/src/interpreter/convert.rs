//! Storage, subscript, and path conversion methods.

use std::sync::Arc;

use ordered_float::OrderedFloat;
use rumps_types::Subscript;
use smallvec::SmallVec;

use super::Interpreter;
use crate::value::Payload;
use crate::Span;

impl Interpreter<'_, '_> {
    /// Convert a runtime value to a storage value.
    ///
    /// Scalars convert directly. Complex values must already be `Json`.
    pub(crate) fn store(&mut self, v: &Payload) -> rumps_types::Value {
        match v {
            Payload::Unit => typechecked!("store", "Storable (not Unit)"),
            Payload::Bool(b) => rumps_types::Value::Boolean(*b),
            Payload::Int(i) => rumps_types::Value::Integer(*i),
            // Word is stored as Int (converted)
            Payload::Word(w) => rumps_types::Value::Integer(*w as i64),
            Payload::Float(f) => rumps_types::Value::Double(*f),
            Payload::Char(c) => rumps_types::Value::Char(*c),
            Payload::String(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                rumps_types::Value::String(s.to_owned())
            }
            Payload::Json(j) => rumps_types::Value::Json(j.as_ref().clone()),
            Payload::FilePath(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                rumps_types::Value::String(s.to_owned())
            }
            Payload::Regex(_) => typechecked!("store", "Storable (not Regex)"),
            Payload::Array(_)
            | Payload::Object(_)
            | Payload::Tuple(_)
            | Payload::Map(_)
            | Payload::Variant { .. }
            | Payload::VariantCtor { .. }
            | Payload::Closure { .. }
            | Payload::Function { .. }
            | Payload::ModuleFn { .. }
            | Payload::ModuleConst { .. }
            | Payload::Range { .. }
            | Payload::LoopContinuation
            | Payload::LoopContinue(_)
            | Payload::ClassMethodFn { .. }
            | Payload::PartialApp { .. } => {
                typechecked!("store", "Storable")
            }
            Payload::Time(t) => rumps_types::Value::String(t.to_rfc3339()),
            Payload::Ref(..) => typechecked!("store", "Storable (not Ref)"),
        }
    }

    /// Convert a storage value to a runtime value.
    ///
    /// JSON values are loaded as opaque `Payload::Json`; use `read` to convert.
    pub(crate) fn load(&mut self, v: rumps_types::Value) -> Payload {
        match v {
            rumps_types::Value::Boolean(b) => Payload::Bool(b),
            rumps_types::Value::Integer(i) => Payload::Int(i),
            rumps_types::Value::Double(d) => Payload::Float(d),
            rumps_types::Value::Char(c) => Payload::Char(c),
            rumps_types::Value::String(s) => {
                Payload::String(self.arena.intern(&s))
            }
            rumps_types::Value::Json(j) => Payload::Json(Arc::new(j)),
        }
    }

    /// Convert a JSON value to a runtime value.
    pub(crate) fn unjsonify(&mut self, j: serde_json::Value) -> Payload {
        match j {
            serde_json::Value::Null => self.make_none(),
            serde_json::Value::Bool(b) => Payload::Bool(b),
            serde_json::Value::Number(n) => {
                Payload::Float(OrderedFloat(n.as_f64().unwrap_or(0.0)))
            }
            serde_json::Value::String(s) => {
                Payload::String(self.arena.intern(&s))
            }
            serde_json::Value::Array(arr) => {
                let dominated = arr.first().is_none_or(|first| {
                    let tag = Self::json_type_tag(first);
                    arr.iter().skip(1).all(|v| Self::json_type_tag(v) == tag)
                });

                if dominated {
                    let elems: SmallVec<[_; 4]> = arr
                        .into_iter()
                        .map(|v| {
                            let val = self.unjsonify(v);
                            self.add_payload(val, Span::default())
                        })
                        .collect();
                    Payload::Array(Arc::new(elems))
                } else {
                    // Heterogeneous arrays stay as opaque Json
                    Payload::Json(Arc::new(serde_json::Value::Array(arr)))
                }
            }
            serde_json::Value::Object(obj) => {
                let fields = obj
                    .into_iter()
                    .map(|(k, v)| {
                        let key = self.arena.intern(&k);
                        let val = self.unjsonify(v);
                        let val_id = self.add_payload(val, Span::default());
                        (key, val_id)
                    })
                    .collect();
                Payload::Object(Arc::new(fields))
            }
        }
    }

    /// Convert a value to a subscript for key construction.
    ///
    /// Only scalar types (Bool, Int, Word, Float, Char, String, Json) can be subscripts.
    pub(crate) fn subscript(&self, v: &Payload) -> Subscript {
        match v {
            Payload::Bool(b) => Subscript::Boolean(*b),
            Payload::Int(i) => Subscript::Number(OrderedFloat(*i as f64)),
            Payload::Word(w) => Subscript::Number(OrderedFloat(*w as f64)),
            Payload::Float(f) => Subscript::Number(*f),
            Payload::Char(c) => Subscript::String(c.to_string()),
            Payload::String(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                Subscript::String(s.to_owned())
            }
            Payload::Json(j) => Subscript::Json(j.as_ref().clone()),
            Payload::Unit
            | Payload::Array(_)
            | Payload::Object(_)
            | Payload::Tuple(_)
            | Payload::Map(_)
            | Payload::Time(_)
            | Payload::FilePath(_)
            | Payload::Regex(_)
            | Payload::Variant { .. }
            | Payload::VariantCtor { .. }
            | Payload::Closure { .. }
            | Payload::Function { .. }
            | Payload::ModuleFn { .. }
            | Payload::ModuleConst { .. }
            | Payload::Range { .. }
            | Payload::LoopContinuation
            | Payload::LoopContinue(_)
            | Payload::Ref(..)
            | Payload::ClassMethodFn { .. }
            | Payload::PartialApp { .. } => {
                typechecked!("subscript", "Subscriptable")
            }
        }
    }

    /// Convert a subscript from storage to a runtime value.
    ///
    /// Inverse of `subscript`; used by `order` to convert results.
    pub(crate) fn value_from_subscript(&mut self, sub: Subscript) -> Payload {
        match sub {
            Subscript::Boolean(b) => Payload::Bool(b),
            Subscript::Number(n) => {
                // Check if it's a whole number
                let f = n.into_inner();
                #[allow(clippy::float_cmp)]
                let is_int = f.fract() == 0.0
                    && f >= i64::MIN as f64
                    && f <= i64::MAX as f64;
                if is_int {
                    Payload::Int(f as i64)
                } else {
                    Payload::Float(n)
                }
            }
            Subscript::Char(c) => Payload::Char(c),
            Subscript::String(s) => Payload::String(self.arena.intern(&s)),
            Subscript::Json(j) => Payload::Json(Arc::new(j)),
        }
    }

    /// Convert a value to a file path string.
    ///
    /// Accepts `FilePath` or `String` values; returns the path as a `String`.
    pub(super) fn filepath(&self, val: &Payload) -> String {
        match val {
            Payload::FilePath(id) | Payload::String(id) => self
                .arena
                .get_str(*id)
                .unwrap_or_else(|| invariant!("StringId in arena"))
                .to_owned(),
            _ => typechecked!("file path", "FilePath | String"),
        }
    }

    /// Get a discriminant tag for JSON value type (for homogeneity checks).
    fn json_type_tag(v: &serde_json::Value) -> u8 {
        match v {
            serde_json::Value::Null => 0,
            serde_json::Value::Bool(_) => 1,
            serde_json::Value::Number(_) => 2,
            serde_json::Value::String(_) => 3,
            serde_json::Value::Array(_) => 4,
            serde_json::Value::Object(_) => 5,
        }
    }
}

pub(crate) struct RawDisplay;

impl RawDisplay {
    /// Escape special characters in a string for raw display.
    pub(crate) fn escape_str(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        s.chars().for_each(|c| match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        });
        out
    }
}
