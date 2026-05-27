//! Value conversion methods.
//!
//! These live on Interpreter rather than Value because they need context
//! (arena, registry) that the interpreter owns.
//!
//! Conversion methods dispatch to `Into[T]` class methods internally; the
//! helpers here provide a convenient API for the interpreter to use.

use std::sync::Arc;

use itertools::Itertools;
use ordered_float::OrderedFloat;
use rumps_types::Subscript;
use smallvec::SmallVec;

use super::class::{self, ClassCtx};
use super::Interpreter;
use crate::io::IoContext;
use crate::value::{MapKey, Payload, Value, ValueId};
use crate::Span;

impl<I: IoContext> Interpreter<'_, I> {
    /// Convert a runtime value to a storage value.
    ///
    /// Scalars convert directly; complex values serialize to JSON.
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
            // Serialize to JSON for complex values (closures, module fns,
            // ranges, and continuations will panic in jsonify via typechecked!)
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
                rumps_types::Value::Json(self.jsonify(v))
            }
            Payload::Time(t) => rumps_types::Value::String(t.to_rfc3339()),
            Payload::Ref(..) => typechecked!("store", "Storable (not Ref)"),
        }
    }

    pub(crate) fn store_value(&mut self, v: &Value) -> rumps_types::Value {
        match &v.payload {
            Payload::Object(_)
            | Payload::Array(_)
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
                rumps_types::Value::Json(self.jsonify_value(v))
            }
            payload => self.store(payload),
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

    /// Convert a value to a human-readable display string.
    ///
    /// Used for `write` statements. Quotes strings and file paths so output
    /// is valid RUMPS syntax.
    pub(crate) fn display(&mut self, v: &Payload) -> String {
        self.stringify(v)
    }

    pub(crate) fn display_value(&mut self, v: &Value) -> String {
        self.stringify_value(v)
    }

    /// Convert a value to display string with escape sequences preserved.
    ///
    /// Used for `write expr raw`. Strings are quoted and special characters
    /// (`\n`, `\t`, etc.) are shown as escape sequences rather than rendered.
    pub(crate) fn display_raw(&mut self, v: &Payload) -> String {
        match v {
            Payload::Char(c) => Self::escape_char(*c),
            Payload::String(id) | Payload::FilePath(id) => {
                let s = self.arena.get_str(*id).unwrap_or("");
                format!("\"{}\"", RawDisplay::escape_str(s))
            }
            Payload::Array(elems) => {
                let vals: Vec<_> = elems
                    .iter()
                    .filter_map(|id| self.arena.value(*id).cloned())
                    .collect();
                let items =
                    vals.iter().map(|v| self.display_raw_value(v)).join(", ");
                format!("[ {items} ]")
            }
            Payload::Tuple(elems) => {
                let len = elems.len();
                let vals: Vec<_> = elems
                    .iter()
                    .filter_map(|id| self.arena.value(*id).cloned())
                    .collect();
                let items =
                    vals.iter().map(|v| self.display_raw_value(v)).join(", ");
                let trail = if len == 1 { "," } else { "" };
                format!("({items}{trail})")
            }
            Payload::Object(obj) => {
                // Collect keys and values first to avoid borrow conflicts
                let data: Vec<_> = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key =
                            self.arena.get_str(*k).unwrap_or("?").to_owned();
                        let val = self.arena.value(*vid).cloned();
                        (key, val)
                    })
                    .collect();
                let fields = data
                    .into_iter()
                    .map(|(k, v)| {
                        let vs = v
                            .map(|v| self.display_raw_value(&v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{k}: {vs}")
                    })
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Payload::Map(entries) => {
                // Collect keys and values first to avoid borrow conflicts
                let data: Vec<_> = entries
                    .iter()
                    .map(|(k, vid)| {
                        let key = self.stringify_map_key(k);
                        let val = self.arena.value(*vid).cloned();
                        (key, val)
                    })
                    .collect();
                let items = data
                    .into_iter()
                    .map(|(k, v)| {
                        let vs = v
                            .map(|v| self.display_raw_value(&v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{k} => {vs}")
                    })
                    .join(", ");
                format!("{{ {items} }}")
            }
            Payload::Variant { tag, vals } => {
                if vals.is_empty() {
                    format!("Variant.{tag}")
                } else {
                    let args: Vec<_> = vals
                        .iter()
                        .filter_map(|id| self.arena.value(*id).cloned())
                        .collect();
                    let args_str = args
                        .iter()
                        .map(|v| self.display_raw_value(v))
                        .join(", ");
                    format!("Variant.{tag}({args_str})")
                }
            }
            Payload::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name =
                    self.arena.get_str(*name_id).unwrap_or("?").to_owned();
                if sub_ids.is_empty() {
                    format!("{prefix}{name}")
                } else {
                    let subs: Vec<_> = sub_ids
                        .iter()
                        .filter_map(|id| self.arena.value(*id).cloned())
                        .collect();
                    let subs_str = subs
                        .iter()
                        .map(|v| self.display_raw_value(v))
                        .join(", ");
                    format!("{prefix}{name}{{ {subs_str} }}")
                }
            }
            // Non-string types delegate to normal stringify
            _ => self.stringify(v),
        }
    }

    pub(crate) fn display_raw_value(&mut self, v: &Value) -> String {
        match &v.payload {
            Payload::Variant { tag, vals } => {
                self.display_raw_variant(v, *tag, vals)
            }
            payload => self.display_raw(payload),
        }
    }

    fn display_raw_variant(
        &mut self,
        v: &Value,
        tag: u8,
        vals: &[ValueId],
    ) -> String {
        let type_id = self
            .checked
            .types
            .to_type_id(v.repr)
            .or_else(|| self.checked.types.to_type_id(v.ty));
        let ty_name = type_id
            .and_then(|type_id| self.registry.type_name(type_id, &self.arena))
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Variant".to_owned());
        let var_name = type_id
            .and_then(|type_id| {
                self.registry.variant_name(type_id, tag, &self.arena)
            })
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| tag.to_string());

        if vals.is_empty() {
            format!("{ty_name}.{var_name}")
        } else {
            let values: Vec<_> = vals
                .iter()
                .filter_map(|id| self.arena.value(*id).cloned())
                .collect();
            let args =
                values.iter().map(|v| self.display_raw_value(v)).join(", ");
            format!("{ty_name}.{var_name}({args})")
        }
    }

    /// Coerce a value to a raw string for concatenation.
    ///
    /// Unlike `stringify`, this does not quote strings.
    pub(crate) fn coerce_to_str(&mut self, v: &Payload) -> String {
        match v {
            Payload::String(id) | Payload::FilePath(id) => {
                self.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => self.stringify(v),
        }
    }

    pub(crate) fn coerce_value_to_str(&mut self, v: &Value) -> String {
        match &v.payload {
            Payload::String(id) | Payload::FilePath(id) => {
                self.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => self.stringify_value(v),
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
    pub(crate) fn stringify(&mut self, v: &Payload) -> String {
        let ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span: Span::default(),
        };
        class::Display::format(&ctx, v)
    }

    pub(crate) fn stringify_value(&mut self, v: &Value) -> String {
        let ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span: Span::default(),
        };
        class::Display::format_value(&ctx, v)
    }

    /// Convert a value to JSON via `Into[Json]`.
    ///
    /// Used for JSON output and storage serialization.
    ///
    /// This is a convenience wrapper around `Into::jsonify`; the class
    /// method is used so frequently that constructing a `ClassCtx` at every
    /// call site would be overly verbose.
    pub(crate) fn jsonify(&mut self, v: &Payload) -> serde_json::Value {
        let ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span: Span::default(),
        };
        class::Into::jsonify(&ctx, v)
    }

    pub(crate) fn jsonify_value(&mut self, v: &Value) -> serde_json::Value {
        let ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span: Span::default(),
        };
        class::Into::jsonify_value(&ctx, v)
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

    /// Stringify a map key for display.
    fn stringify_map_key(&self, k: &MapKey) -> String {
        match k {
            MapKey::Bool(b) => b.to_string(),
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
