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
use crate::Span;

impl<I: IoContext> Interpreter<'_, I> {
    /// Convert a runtime value to a storage value.
    ///
    /// Scalars convert directly; complex values serialize to JSON.
    pub(crate) fn store(&self, v: &Value) -> rumps_types::Value {
        match v {
            Value::Unit => typechecked!("store", "Storable (not Unit)"),
            Value::Bool(b) => rumps_types::Value::Boolean(*b),
            Value::Int(i) => rumps_types::Value::Integer(*i),
            // Word is stored as Int (converted)
            Value::Word(w) => rumps_types::Value::Integer(*w as i64),
            Value::Float(f) => rumps_types::Value::Double(*f),
            Value::Char(c) => rumps_types::Value::Char(*c),
            Value::String(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                rumps_types::Value::String(s.to_owned())
            }
            Value::Json(j) => rumps_types::Value::Json(j.clone()),
            Value::FilePath(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                rumps_types::Value::String(s.to_owned())
            }
            Value::Regex(_) => typechecked!("store", "Storable (not Regex)"),
            // Serialize to JSON for complex values (closures, module fns,
            // ranges, and continuations will panic in jsonify via typechecked!)
            Value::Array(_, _)
            | Value::Object(_)
            | Value::Tuple(_, _)
            | Value::Map(_, _, _)
            | Value::Tagged(_, _, _)
            | Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. }
            | Value::ModuleConst { .. }
            | Value::Range { .. }
            | Value::ForeverContinuation
            | Value::LoopContinue(_) => {
                rumps_types::Value::Json(self.jsonify(v))
            }
            Value::Time(t) => rumps_types::Value::String(t.to_rfc3339()),
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
    /// Used for WRITE statements. Quotes strings and file paths so output
    /// is valid RUMPS syntax.
    pub(crate) fn display(&self, v: &Value) -> String {
        self.stringify(v)
    }

    /// Coerce a value to a raw string for concatenation.
    ///
    /// Unlike `stringify`, this does not quote strings.
    pub(crate) fn coerce_to_str(&self, v: &Value) -> String {
        match v {
            Value::String(id) | Value::FilePath(id) => {
                self.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => self.stringify(v),
        }
    }

    /// Recursive stringify helper.
    ///
    /// Produces valid RUMPS syntax; strings and file paths are quoted.
    pub(crate) fn stringify(&self, v: &Value) -> String {
        match v {
            Value::Unit => "Unit".into(),
            // Usually keywords are represented as uppercase, so this will
            // produce `TRUE`/`FALSE`, even though they are not really keywords
            Value::Bool(b) => b.to_string().to_uppercase(),
            Value::Int(n) => n.to_string(),
            Value::Word(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Char(c) => format!("'{c}'"),
            Value::String(id) | Value::FilePath(id) => {
                let s = self.arena.get_str(*id).unwrap_or("");
                format!("\"{s}\"")
            }
            Value::Regex(idx) => {
                let re =
                    self.regex_cache.get(*idx as usize).unwrap_or_else(|| {
                        typechecked!("stringify Regex", "valid cache index")
                    });
                format!("/{}/", re.as_str())
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
            // Functions cannot be stringified (rejected by type checker)
            Value::Closure { .. } => {
                typechecked!("stringify", "Stringable (not Closure)")
            }
            Value::Function { .. } => {
                typechecked!("stringify", "Stringable (not Function)")
            }
            Value::ModuleFn { .. } => {
                typechecked!("stringify", "Stringable (not ModuleFn)")
            }
            // Module constants should be resolved before stringify; if not,
            // display the path as a fallback
            Value::ModuleConst { path } => {
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
            // Internal loop control values; should not be stringified by user code
            Value::ForeverContinuation => "<continuation>".into(),
            Value::LoopContinue(_) => "<loop-continue>".into(),
        }
    }

    /// Convert a value to JSON.
    ///
    /// Used for JSON output and storage serialization.
    pub(crate) fn jsonify(&self, v: &Value) -> serde_json::Value {
        match v {
            Value::Unit => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Int(n) => serde_json::json!(*n),
            Value::Word(n) => serde_json::json!(*n),
            Value::Float(f) => serde_json::json!(f.0),
            Value::Char(c) => serde_json::Value::String(c.to_string()),
            Value::String(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Value::FilePath(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Value::Array(_, arr) => {
                let elems: Vec<_> = arr
                    .iter()
                    .map(|id| {
                        self.arena
                            .get(*id)
                            .unwrap_or_else(|| invariant!("ValueId in arena"))
                    })
                    .map(|v| self.jsonify(v))
                    .collect();
                serde_json::Value::Array(elems)
            }
            Value::Tuple(_, elems) => {
                let items: Vec<_> = elems
                    .iter()
                    .map(|id| {
                        self.arena
                            .get(*id)
                            .unwrap_or_else(|| invariant!("ValueId in arena"))
                    })
                    .map(|v| self.jsonify(v))
                    .collect();
                serde_json::Value::Array(items)
            }
            Value::Object(obj) => {
                let map: serde_json::Map<_, _> = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = self
                            .arena
                            .get_str(*k)
                            .unwrap_or_else(|| invariant!("StringId in arena"));
                        let val = self
                            .arena
                            .get(*vid)
                            .unwrap_or_else(|| invariant!("ValueId in arena"));
                        (key.to_owned(), self.jsonify(val))
                    })
                    .collect();
                serde_json::Value::Object(map)
            }
            // Sum type encoding: tagged object (with special handling for Option)
            Value::Tagged(ty_expr, idx, payloads) => {
                let base_ty = self.type_exprs.base_type(*ty_expr);

                // Option encodes as null/value rather than tagged object
                if base_ty.is_some_and(|ty| ty == TypeId::OPTION) {
                    if *idx == 0 {
                        // Option.None -> null
                        serde_json::Value::Null
                    } else {
                        // Option.Some(v) -> jsonify(v)
                        payloads
                            .first()
                            .and_then(|id| self.arena.get(*id))
                            .map(|v| self.jsonify(v))
                            .unwrap_or(serde_json::Value::Null)
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
                    let payload_json: Vec<_> = payloads
                        .iter()
                        .map(|id| {
                            self.arena.get(*id).unwrap_or_else(|| {
                                invariant!("ValueId in arena")
                            })
                        })
                        .map(|v| self.jsonify(v))
                        .collect();

                    serde_json::json!({
                        "_type": ty_name,
                        "_variant": var_name,
                        "_payload": payload_json
                    })
                }
            }
            Value::Map(_, _, entries) => {
                let map: serde_json::Map<_, _> = entries
                    .iter()
                    .map(|(k, vid)| {
                        let key = self.stringify_map_key(k);
                        let val = self
                            .arena
                            .get(*vid)
                            .unwrap_or_else(|| invariant!("ValueId in arena"));
                        (key, self.jsonify(val))
                    })
                    .collect();
                serde_json::Value::Object(map)
            }
            Value::Time(t) => serde_json::Value::String(t.to_rfc3339()),
            Value::Json(j) => j.clone(),
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                let end = if *inclusive { *end + 1 } else { *end };
                let arr: Vec<_> = (*start..end)
                    .map(|n| serde_json::Value::Number(n.into()))
                    .collect();
                serde_json::Value::Array(arr)
            }
            Value::Closure { .. } => {
                typechecked!("jsonify", "Jsonable (not Closure)")
            }
            Value::Function { .. } => {
                typechecked!("jsonify", "Jsonable (not Function)")
            }
            Value::ModuleFn { .. } => {
                typechecked!("jsonify", "Jsonable (not ModuleFn)")
            }
            Value::ModuleConst { .. } => {
                typechecked!("jsonify", "Jsonable (not ModuleConst)")
            }
            Value::Regex(_) => typechecked!("jsonify", "Jsonable (not Regex)"),
            Value::ForeverContinuation | Value::LoopContinue(_) => {
                typechecked!("jsonify", "Jsonable (not continuation)")
            }
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
    /// Only scalar types (Bool, Int, Word, Float, Char, String, Json) can be subscripts.
    pub(crate) fn subscript(&self, v: &Value) -> Subscript {
        match v {
            Value::Bool(b) => Subscript::Boolean(*b),
            Value::Int(i) => Subscript::Number(OrderedFloat(*i as f64)),
            Value::Word(w) => Subscript::Number(OrderedFloat(*w as f64)),
            Value::Float(f) => Subscript::Number(*f),
            Value::Char(c) => Subscript::String(c.to_string()),
            Value::String(id) => {
                let s = self
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                Subscript::String(s.to_owned())
            }
            Value::Json(j) => Subscript::Json(j.clone()),
            Value::Unit
            | Value::Array(_, _)
            | Value::Object(_)
            | Value::Tuple(_, _)
            | Value::Map(_, _, _)
            | Value::Time(_)
            | Value::FilePath(_)
            | Value::Regex(_)
            | Value::Tagged(_, _, _)
            | Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. }
            | Value::ModuleConst { .. }
            | Value::Range { .. }
            | Value::ForeverContinuation
            | Value::LoopContinue(_) => {
                typechecked!("subscript", "Subscriptable")
            }
        }
    }

    /// Convert a subscript from storage to a runtime value.
    ///
    /// Inverse of `subscript`; used by `ORDER` to convert results.
    pub(crate) fn value_from_subscript(&mut self, sub: Subscript) -> Value {
        match sub {
            Subscript::Boolean(b) => Value::Bool(b),
            Subscript::Number(n) => {
                // Check if it's a whole number
                let f = n.into_inner();
                #[allow(clippy::float_cmp)]
                let is_int = f.fract() == 0.0
                    && f >= i64::MIN as f64
                    && f <= i64::MAX as f64;
                if is_int {
                    Value::Int(f as i64)
                } else {
                    Value::Float(n)
                }
            }
            Subscript::Char(c) => Value::Char(c),
            Subscript::String(s) => Value::String(self.arena.intern(&s)),
            Subscript::Json(j) => Value::Json(j),
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

    /// Convert a value to a file path string.
    ///
    /// Accepts `FilePath` or `String` values; returns the path as a `String`.
    pub(super) fn filepath(&self, val: &Value) -> String {
        match val {
            Value::FilePath(id) | Value::String(id) => self
                .arena
                .get_str(*id)
                .unwrap_or_else(|| invariant!("StringId in arena"))
                .to_owned(),
            _ => typechecked!("file path", "FilePath | String"),
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
