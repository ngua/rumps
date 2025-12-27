//! Value conversion methods.
//!
//! These live on Interpreter rather than Value because they need context
//! (arena, registry) that the interpreter owns.

use ordered_float::OrderedFloat;
use rumps_types::Subscript;
use smallvec::SmallVec;

use super::Interpreter;
use crate::io::IoContext;
use crate::value::{MapKey, TypeExprArena, TypeExprId, TypeId, Value};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Convert a runtime value to a storage value.
    ///
    /// Scalars convert directly; complex values serialize to JSON.
    pub(crate) fn store(&self, v: &Value) -> Result<rumps_types::Value> {
        match v {
            Value::Unit => Err(Error::runtime_no_span("Unit cannot be stored")),
            Value::Bool(b) => Ok(rumps_types::Value::Boolean(*b)),
            Value::Int(i) => Ok(rumps_types::Value::Integer(*i)),
            Value::Float(f) => Ok(rumps_types::Value::Double(*f)),
            Value::Char(c) => Ok(rumps_types::Value::Char(*c)),
            Value::String(id) => self
                .arena
                .get_str(*id)
                .map(|s| rumps_types::Value::String(s.to_owned()))
                .ok_or_else(|| Error::runtime_no_span("invalid string id")),
            // Json values store directly
            Value::Json(j) => Ok(rumps_types::Value::Json(j.clone())),
            // Serialize to JSON for complex values (closures, module fns,
            // and ranges will error in jsonify)
            Value::Array(_, _)
            | Value::Object(_)
            | Value::Tuple(_, _)
            | Value::Map(_, _, _)
            | Value::Tagged(_, _, _)
            | Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. }
            | Value::Range { .. } => {
                self.jsonify(v).map(rumps_types::Value::Json)
            }
            Value::Time(t) => {
                // Store time as ISO 8601 string
                Ok(rumps_types::Value::String(t.to_rfc3339()))
            }
        }
    }

    /// Convert a storage value to a runtime value.
    ///
    /// JSON values are loaded as opaque `Value::Json`; use `READ` to convert.
    pub(crate) fn load(&mut self, v: rumps_types::Value) -> Value {
        match v {
            rumps_types::Value::Boolean(b) => Value::Bool(b),
            rumps_types::Value::Integer(i) => Value::Int(i),
            rumps_types::Value::Double(d) => Value::Float(d),
            rumps_types::Value::Char(c) => Value::Char(c),
            rumps_types::Value::String(s) => {
                Value::String(self.arena.intern(&s))
            }
            rumps_types::Value::Json(j) => Value::Json(j),
        }
    }

    /// Convert a value to a human-readable display string.
    ///
    /// Used for OUTPUT statements.
    pub(crate) fn display(&self, v: &Value) -> String {
        self.stringify(v)
    }

    /// Recursive stringify helper.
    pub(crate) fn stringify(&self, v: &Value) -> String {
        match v {
            Value::Unit => "Unit".into(),
            // Usually keywords are represented as uppercase, so this will
            // produce `TRUE`/`FALSE`, even though they are not really keywords
            Value::Bool(b) => b.to_string().to_uppercase(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Char(c) => format!("'{c}'"),
            Value::String(id) => {
                self.arena.get_str(*id).unwrap_or("").to_owned()
            }
            Value::Array(_, elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| self.arena.get(*id))
                    .map(|v| self.stringify(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[ {items} ]")
            }
            Value::Tuple(_, elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| self.arena.get(*id))
                    .map(|v| self.stringify(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                // Single-element tuples need trailing comma to distinguish from
                // parenthesized expressions
                let trail = if elems.len() == 1 { "," } else { "" };
                format!("({items}{trail})")
            }
            Value::Object(obj) => {
                let fields = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = self.arena.get_str(*k).unwrap_or("?");
                        let val = self
                            .arena
                            .get(*vid)
                            .map(|v| self.stringify(v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{key}: {val}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Value::Map(_, _, entries) => {
                let items = entries
                    .iter()
                    .map(|(k, vid)| {
                        let key = self.stringify_map_key(k);
                        let val = self
                            .arena
                            .get(*vid)
                            .map(|v| self.stringify(v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{key} => {val}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {items} }}")
            }
            Value::Time(t) => t.to_rfc3339(),
            Value::Json(j) => j.to_string(),
            Value::Tagged(ty_expr, idx, payloads) => {
                let base_ty = self.type_exprs.base_type(*ty_expr);
                let ty_name = base_ty
                    .and_then(|ty| self.registry.type_name(ty, &self.arena))
                    .unwrap_or("?");
                let var_name = base_ty
                    .and_then(|ty| {
                        self.registry.variant_name(ty, *idx, &self.arena)
                    })
                    .unwrap_or("?");

                if payloads.is_empty() {
                    format!("{ty_name}.{var_name}")
                } else {
                    let args = payloads
                        .iter()
                        .filter_map(|id| self.arena.get(*id))
                        .map(|v| self.stringify(v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{ty_name}.{var_name}({args})")
                }
            }
            Value::Closure { params, .. } => {
                // Display as <closure(n)> where n is the number of parameters
                format!("<closure({})>", params.len())
            }
            Value::Function { name, params, .. } => {
                // Display as <function name(n)> where n is the number of parameters
                let fn_name = self.arena.get_str(*name).unwrap_or("?");
                format!("<function {}({})>", fn_name, params.len())
            }
            Value::ModuleFn { path } => {
                let path_str: String = path
                    .iter()
                    .filter_map(|id| self.arena.get_str(*id))
                    .collect::<Vec<_>>()
                    .join(".");
                format!("<{path_str}>")
            }
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                if *inclusive {
                    format!("{start}..={end}")
                } else {
                    format!("{start}..{end}")
                }
            }
        }
    }

    /// Convert a value to JSON.
    ///
    /// Used for JSON output and storage serialization.
    /// Returns an error for values that cannot be serialized (e.g., closures).
    pub(crate) fn jsonify(&self, v: &Value) -> Result<serde_json::Value> {
        match v {
            Value::Unit => Ok(serde_json::Value::Null),
            Value::Bool(b) => Ok(serde_json::Value::Bool(*b)),
            Value::Int(n) => Ok(serde_json::json!(*n)),
            Value::Float(f) => Ok(serde_json::json!(f.0)),
            Value::Char(c) => Ok(serde_json::Value::String(c.to_string())),
            Value::String(id) => {
                let s = self.arena.get_str(*id).unwrap_or("");
                Ok(serde_json::Value::String(s.to_owned()))
            }
            Value::Array(_, arr) => {
                let elems: Result<Vec<_>> = arr
                    .iter()
                    .filter_map(|id| self.arena.get(*id))
                    .map(|v| self.jsonify(v))
                    .collect();
                Ok(serde_json::Value::Array(elems?))
            }
            Value::Tuple(_, elems) => {
                // Tuples serialize as JSON arrays
                let items: Result<Vec<_>> = elems
                    .iter()
                    .filter_map(|id| self.arena.get(*id))
                    .map(|v| self.jsonify(v))
                    .collect();
                Ok(serde_json::Value::Array(items?))
            }
            Value::Object(obj) => {
                let map: Result<serde_json::Map<_, _>> = obj
                    .iter()
                    .filter_map(|(k, vid)| {
                        let key = self.arena.get_str(*k)?;
                        let val = self.arena.get(*vid)?;
                        Some((key.to_owned(), val))
                    })
                    .map(|(k, v)| self.jsonify(v).map(|jv| (k, jv)))
                    .collect();
                Ok(serde_json::Value::Object(map?))
            }
            // Sum type encoding: tagged object (with special handling for Option)
            Value::Tagged(ty_expr, idx, payloads) => {
                let base_ty = self.type_exprs.base_type(*ty_expr);

                // Option encodes as null/value rather than tagged object
                if base_ty.is_some_and(|ty| ty == TypeId::OPTION) {
                    if *idx == 0 {
                        // Option.None -> null
                        Ok(serde_json::Value::Null)
                    } else {
                        // Option.Some(v) -> jsonify(v)
                        payloads
                            .first()
                            .and_then(|id| self.arena.get(*id))
                            .map(|v| self.jsonify(v))
                            .unwrap_or(Ok(serde_json::Value::Null))
                    }
                } else {
                    let ty_name = base_ty
                        .and_then(|ty| self.registry.type_name(ty, &self.arena))
                        .unwrap_or("?");
                    let var_name = base_ty
                        .and_then(|ty| {
                            self.registry.variant_name(ty, *idx, &self.arena)
                        })
                        .unwrap_or("?");
                    let payload_json: Result<Vec<_>> = payloads
                        .iter()
                        .filter_map(|id| self.arena.get(*id))
                        .map(|v| self.jsonify(v))
                        .collect();

                    Ok(serde_json::json!({
                        "_type": ty_name,
                        "_variant": var_name,
                        "_payload": payload_json?
                    }))
                }
            }
            Value::Map(_, _, entries) => {
                // Maps serialize as JSON objects with stringified keys
                let map: Result<serde_json::Map<_, _>> = entries
                    .iter()
                    .map(|(k, vid)| {
                        let key = self.stringify_map_key(k);
                        let val = self.arena.get(*vid).ok_or_else(|| {
                            Error::runtime_no_span("invalid value id")
                        })?;
                        self.jsonify(val).map(|jv| (key, jv))
                    })
                    .collect();
                Ok(serde_json::Value::Object(map?))
            }
            Value::Time(t) => {
                // Times serialize as ISO 8601 strings
                Ok(serde_json::Value::String(t.to_rfc3339()))
            }
            // Json is already JSON
            Value::Json(j) => Ok(j.clone()),
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                let end = if *inclusive { *end + 1 } else { *end };
                let arr: Vec<_> = (*start..end)
                    .map(|n| serde_json::Value::Number(n.into()))
                    .collect();
                Ok(serde_json::Value::Array(arr))
            }
            Value::Closure { .. } => Err(Error::runtime_no_span(
                "closures cannot be serialized to JSON",
            )),
            Value::Function { .. } => Err(Error::runtime_no_span(
                "functions cannot be serialized to JSON",
            )),
            Value::ModuleFn { .. } => Err(Error::runtime_no_span(
                "module functions cannot be serialized to JSON",
            )),
        }
    }

    /// Convert a JSON value to a runtime value.
    pub(crate) fn unjsonify(&mut self, j: serde_json::Value) -> Value {
        match j {
            serde_json::Value::Null => self.make_none(),
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                Value::Float(OrderedFloat(n.as_f64().unwrap_or(0.0)))
            }
            serde_json::Value::String(s) => {
                Value::String(self.arena.intern(&s))
            }
            serde_json::Value::Array(arr) => {
                let dominated = arr.first().is_none_or(|first| {
                    let tag = json_type_tag(first);
                    arr.iter().skip(1).all(|v| json_type_tag(v) == tag)
                });

                if dominated {
                    // Get element type from first element, or UNKNOWN for empty
                    let elem_ty = match arr.first() {
                        None => self.type_exprs.named(TypeId::UNKNOWN),
                        Some(first) => {
                            json_type_expr(first, &mut self.type_exprs)
                        }
                    };

                    let elems: SmallVec<[_; 4]> = arr
                        .into_iter()
                        .map(|v| {
                            let val = self.unjsonify(v);
                            self.arena.add(val, Span::default())
                        })
                        .collect();
                    Value::Array(elem_ty, elems)
                } else {
                    // Heterogeneous arrays stay as opaque Json
                    Value::Json(serde_json::Value::Array(arr))
                }
            }
            serde_json::Value::Object(obj) => {
                let fields = obj
                    .into_iter()
                    .map(|(k, v)| {
                        let key = self.arena.intern(&k);
                        let val = self.unjsonify(v);
                        let val_id = self.arena.add(val, Span::default());
                        (key, val_id)
                    })
                    .collect();
                Value::Object(fields)
            }
        }
    }

    /// Convert a value to a subscript for key construction.
    ///
    /// Only scalar types (Bool, Int, Float, Char, String) can be subscripts.
    pub(crate) fn subscript(&self, v: &Value) -> Result<Subscript> {
        match v {
            Value::Bool(b) => Ok(Subscript::Boolean(*b)),
            Value::Int(i) => Ok(Subscript::Number(OrderedFloat(*i as f64))),
            Value::Float(f) => Ok(Subscript::Number(*f)),
            Value::Char(c) => Ok(Subscript::String(c.to_string())),
            Value::String(id) => self
                .arena
                .get_str(*id)
                .map(|s| Subscript::String(s.to_owned()))
                .ok_or_else(|| Error::runtime_no_span("invalid string id")),
            Value::Unit
            | Value::Array(_, _)
            | Value::Object(_)
            | Value::Tuple(_, _)
            | Value::Map(_, _, _)
            | Value::Time(_)
            | Value::Json(_)
            | Value::Tagged(_, _, _)
            | Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. }
            | Value::Range { .. } => Err(Error::runtime_no_span(
                "complex values cannot be used as subscripts",
            )),
        }
    }

    /// Stringify a map key for display.
    fn stringify_map_key(&self, k: &MapKey) -> String {
        match k {
            MapKey::Bool(b) => b.to_string().to_uppercase(),
            MapKey::Int(n) => n.to_string(),
            MapKey::Float(f) => f.to_string(),
            MapKey::Char(c) => format!("'{c}'"),
            MapKey::String(id) => self
                .arena
                .get_str(*id)
                .map(|s| format!("\"{s}\""))
                .unwrap_or_else(|| "\"?\"".to_owned()),
        }
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

/// Get a `TypeExprId` for a JSON value's type.
fn json_type_expr(
    v: &serde_json::Value,
    arena: &mut TypeExprArena,
) -> TypeExprId {
    match v {
        serde_json::Value::Null => arena.named(TypeId::OPTION),
        serde_json::Value::Bool(_) => arena.named(TypeId::BOOL),
        serde_json::Value::Number(_) => arena.named(TypeId::FLOAT),
        serde_json::Value::String(_) => arena.named(TypeId::STRING),
        serde_json::Value::Array(arr) => {
            let elem_ty = match arr.first() {
                None => arena.named(TypeId::UNKNOWN),
                Some(first) => json_type_expr(first, arena),
            };
            arena.app(TypeId::ARRAY, smallvec::smallvec![elem_ty])
        }
        serde_json::Value::Object(_) => arena.named(TypeId::OBJECT),
    }
}
