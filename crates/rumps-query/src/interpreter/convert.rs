//! Value conversion methods.
//!
//! These live on Interpreter rather than Value because they need context
//! (arena, registry) that the interpreter owns.
//!
//! Conversion methods dispatch to `Into[T]` class methods internally; the
//! helpers here provide a convenient API for the interpreter to use.

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
    pub(crate) fn store(&mut self, v: &Value) -> rumps_types::Value {
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
            | Value::LoopContinue(_)
            | Value::ClassMethodFn { .. } => {
                rumps_types::Value::Json(self.jsonify(v))
            }
            Value::Time(t) => rumps_types::Value::String(t.to_rfc3339()),
            Value::Ref(..) => typechecked!("store", "Storable (not Ref)"),
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => self
                .arena
                .get(*inner_id)
                .cloned()
                .map(|inner| self.store(&inner))
                .unwrap_or_else(|| invariant!("inner value in arena")),
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
    pub(crate) fn display(&mut self, v: &Value) -> String {
        self.stringify(v)
    }

    /// Convert a value to display string with escape sequences preserved.
    ///
    /// Used for `WRITE expr RAW`. Strings are quoted and special characters
    /// (`\n`, `\t`, etc.) are shown as escape sequences rather than rendered.
    pub(crate) fn display_raw(&mut self, v: &Value) -> String {
        match v {
            Value::Char(c) => escape_char(*c),
            Value::String(id) | Value::FilePath(id) => {
                let s = self.arena.get_str(*id).unwrap_or("");
                format!("\"{}\"", escape_str(s))
            }
            Value::Array(_, elems) => {
                let vals: Vec<_> = elems
                    .iter()
                    .filter_map(|id| self.arena.get(*id).cloned())
                    .collect();
                let items = vals
                    .iter()
                    .map(|v| self.display_raw(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[ {items} ]")
            }
            Value::Tuple(_, elems) => {
                let len = elems.len();
                let vals: Vec<_> = elems
                    .iter()
                    .filter_map(|id| self.arena.get(*id).cloned())
                    .collect();
                let items = vals
                    .iter()
                    .map(|v| self.display_raw(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                let trail = if len == 1 { "," } else { "" };
                format!("({items}{trail})")
            }
            Value::Object(obj) => {
                // Collect keys and values first to avoid borrow conflicts
                let data: Vec<_> = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key =
                            self.arena.get_str(*k).unwrap_or("?").to_owned();
                        let val = self.arena.get(*vid).cloned();
                        (key, val)
                    })
                    .collect();
                let fields = data
                    .into_iter()
                    .map(|(k, v)| {
                        let vs = v
                            .map(|v| self.display_raw(&v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{k}: {vs}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Value::Map(_, _, entries) => {
                // Collect keys and values first to avoid borrow conflicts
                let data: Vec<_> = entries
                    .iter()
                    .map(|(k, vid)| {
                        let key = self.stringify_map_key(k);
                        let val = self.arena.get(*vid).cloned();
                        (key, val)
                    })
                    .collect();
                let items = data
                    .into_iter()
                    .map(|(k, v)| {
                        let vs = v
                            .map(|v| self.display_raw(&v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{k} => {vs}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {items} }}")
            }
            Value::Tagged(ty_expr, idx, payloads) => {
                let base_ty = self.type_exprs.base_type(*ty_expr);
                let ty_name = base_ty
                    .and_then(|ty| self.registry.type_name(ty, &self.arena))
                    .unwrap_or("?")
                    .to_owned();
                let var_name = base_ty
                    .and_then(|ty| {
                        self.registry.variant_name(ty, *idx, &self.arena)
                    })
                    .unwrap_or("?")
                    .to_owned();
                if payloads.is_empty() {
                    format!("{ty_name}.{var_name}")
                } else {
                    let args: Vec<_> = payloads
                        .iter()
                        .filter_map(|id| self.arena.get(*id).cloned())
                        .collect();
                    let args_str = args
                        .iter()
                        .map(|v| self.display_raw(v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{ty_name}.{var_name}({args_str})")
                }
            }
            Value::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name =
                    self.arena.get_str(*name_id).unwrap_or("?").to_owned();
                let subs: Vec<_> = sub_ids
                    .iter()
                    .filter_map(|id| self.arena.get(*id).cloned())
                    .collect();
                if subs.is_empty() {
                    format!("{prefix}{name}")
                } else {
                    let subs_str = subs
                        .iter()
                        .map(|v| self.display_raw(v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{prefix}{name}{{ {subs_str} }}")
                }
            }
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => {
                let inner = self
                    .arena
                    .get(*inner_id)
                    .cloned()
                    .unwrap_or_else(|| invariant!("inner value in arena"));
                self.display_raw(&inner)
            }
            // Non-string types delegate to normal stringify
            _ => self.stringify(v),
        }
    }

    /// Coerce a value to a raw string for concatenation.
    ///
    /// Unlike `stringify`, this does not quote strings.
    pub(crate) fn coerce_to_str(&mut self, v: &Value) -> String {
        match v {
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => self
                .arena
                .get(*inner_id)
                .cloned()
                .map(|inner_val| self.coerce_to_str(&inner_val))
                .unwrap_or_else(|| self.stringify(v)),
            Value::String(id) | Value::FilePath(id) => {
                self.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => self.stringify(v),
        }
    }

    /// Stringify a value for display via `Display:display`.
    ///
    /// Produces valid RUMPS syntax; strings and file paths are quoted.
    /// This is distinct from `Into[String]` which produces raw strings.
    ///
    /// This is a convenience wrapper around `Display::format`; the class
    /// method is used so frequently that constructing a `ClassCtx` at every
    /// call site would be overly verbose.
    pub(crate) fn stringify(&mut self, v: &Value) -> String {
        let ctx = super::class::ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            ty_arena: &self.ty_arena,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span: Span::default(),
        };
        super::class::Display::format(&ctx, v)
    }

    /// Convert a value to JSON via `Into[Json]`.
    ///
    /// Used for JSON output and storage serialization.
    ///
    /// This is a convenience wrapper around `Into::jsonify`; the class
    /// method is used so frequently that constructing a `ClassCtx` at every
    /// call site would be overly verbose.
    pub(crate) fn jsonify(&mut self, v: &Value) -> serde_json::Value {
        let ctx = super::class::ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            ty_arena: &self.ty_arena,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span: Span::default(),
        };
        super::class::Into::jsonify(&ctx, v)
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
            | Value::LoopContinue(_)
            | Value::Ref(..)
            | Value::ClassMethodFn { .. } => {
                typechecked!("subscript", "Subscriptable")
            }
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => self
                .arena
                .get(*inner_id)
                .cloned()
                .map(|inner| self.subscript(&inner))
                .unwrap_or_else(|| invariant!("inner value in arena")),
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

/// Escape a char for raw display.
fn escape_char(c: char) -> String {
    match c {
        '\'' => "'\\''".to_owned(),
        '\\' => "'\\\\'".to_owned(),
        '\n' => "'\\n'".to_owned(),
        '\t' => "'\\t'".to_owned(),
        '\r' => "'\\r'".to_owned(),
        '\0' => "'\\0'".to_owned(),
        c => format!("'{c}'"),
    }
}
