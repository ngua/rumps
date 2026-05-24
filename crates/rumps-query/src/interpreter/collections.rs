//! Collection operations: objects, arrays, tuples, indexing, field access.

use std::sync::Arc;

use async_recursion::async_recursion;
use indexmap::IndexMap;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{ArrayElem, Expr, ExprId, ObjectEntry};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::value::{MapKey, Payload, TypeId, ValueId, ValueMeta};
use crate::{ClassId, Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate an object literal with potential spread entries.
    ///
    /// Type checker guarantees spreads are on object types.
    #[async_recursion]
    pub(super) async fn object(
        &mut self,
        entries: &[ObjectEntry],
        span: Span,
    ) -> Result<Payload> {
        let map = self.object_entries(entries, IndexMap::new(), span).await?;
        Ok(Payload::Object(Arc::new(map)))
    }

    /// Recursively evaluate object entries (fields and spreads).
    #[async_recursion]
    async fn object_entries(
        &mut self,
        entries: &[ObjectEntry],
        mut acc: IndexMap<StringId, ValueId>,
        span: Span,
    ) -> Result<IndexMap<StringId, ValueId>> {
        match entries.split_first() {
            None => Ok(acc),
            Some((entry, tail)) => {
                match entry {
                    ObjectEntry::Field(key, expr_id) => {
                        let expr_span =
                            self.ast.expr_span(*expr_id).unwrap_or(span);
                        let val = self.eval(*expr_id).await?;
                        let meta = self.expr_meta(*expr_id);
                        let val_id = self.arena.add_typed(val, meta, expr_span);
                        acc.insert(*key, val_id);
                    }
                    ObjectEntry::Spread(expr_id) => {
                        let val = self.eval(*expr_id).await?;
                        // Unwrap Union/Newtype to find the inner Object
                        let unwrapped = self.unwrap_value_recursive(&val);
                        let v = unwrapped.as_ref().unwrap_or(&val);
                        // Type checker guarantees this is an Object
                        match v {
                            Payload::Object(fields) => {
                                // Merge fields from spread object
                                fields.iter().for_each(|(k, v)| {
                                    acc.insert(*k, *v);
                                });
                            }
                            _ => typechecked!("...spread", "Object"),
                        }
                    }
                }
                self.object_entries(tail, acc, span).await
            }
        }
    }

    /// Evaluate an array literal with potential spread elements.
    ///
    /// Arrays containing `Json` values (including `null`) immediately become
    /// JSON arrays, since `Json` is not a native RUMPS type.
    /// Type checker guarantees spreads are on array types.
    #[async_recursion]
    pub(super) async fn array(
        &mut self,
        elems: &[ArrayElem],
        span: Span,
    ) -> Result<Payload> {
        match elems.split_first() {
            None => Ok(Payload::Array(Arc::new(SmallVec::new()))),
            Some((first, rest)) => {
                // Get first value(s) from first element or spread
                let (first_vals, first_span) = match first {
                    ArrayElem::Elem(id) => {
                        let s = self.ast.expr_span(*id).unwrap_or(span);
                        let v = self.eval(*id).await?;
                        (smallvec::smallvec![v], s)
                    }
                    ArrayElem::Spread(id) => {
                        let s = self.ast.expr_span(*id).unwrap_or(span);
                        let v = self.eval(*id).await?;
                        // Type checker guarantees this is an Array
                        match v {
                            Payload::Array(elems) => {
                                let vals: SmallVec<[Payload; 4]> = elems
                                    .iter()
                                    .filter_map(|vid| {
                                        self.arena.get(*vid).cloned()
                                    })
                                    .collect();
                                (vals, s)
                            }
                            _ => typechecked!("...spread", "Array"),
                        }
                    }
                };

                // Check if all first values are Json
                let has_json =
                    first_vals.iter().any(|v| matches!(v, Payload::Json(_)));
                if has_json {
                    // Start in JSON mode
                    let json_acc: Vec<serde_json::Value> = first_vals
                        .into_iter()
                        .map(|v| self.jsonify(&v))
                        .collect();
                    self.array_elems_json_tail_spread(rest, json_acc, span)
                        .await
                } else {
                    let acc: SmallVec<[ValueId; 4]> = first_vals
                        .into_iter()
                        .map(|v| {
                            self.arena.add_typed(
                                v,
                                ValueMeta::untyped(),
                                first_span,
                            )
                        })
                        .collect();
                    self.array_elems_spread(rest, acc, span).await
                }
            }
        }
    }

    /// Recursively evaluate array elements with spread support.
    #[async_recursion]
    async fn array_elems_spread(
        &mut self,
        elems: &[ArrayElem],
        mut acc: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Payload> {
        match elems.split_first() {
            None => Ok(Payload::Array(Arc::new(acc))),
            Some((elem, tail)) => {
                let vals: SmallVec<[Payload; 4]> = match elem {
                    ArrayElem::Elem(id) => {
                        let val = self.eval(*id).await?;
                        smallvec::smallvec![val]
                    }
                    ArrayElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        match val {
                            Payload::Array(elems) => elems
                                .iter()
                                .filter_map(|vid| self.arena.get(*vid).cloned())
                                .collect(),
                            _ => typechecked!("...spread", "Array"),
                        }
                    }
                };

                // Check for Json
                let has_json =
                    vals.iter().any(|v| matches!(v, Payload::Json(_)));
                if has_json {
                    // Convert to JSON mode; collect values first to avoid borrow conflict
                    let acc_vals: Vec<Payload> = acc
                        .iter()
                        .filter_map(|vid| self.arena.get(*vid).cloned())
                        .collect();
                    let mut json_arr: Vec<serde_json::Value> =
                        acc_vals.iter().map(|v| self.jsonify(v)).collect();
                    vals.iter().for_each(|v| {
                        json_arr.push(self.jsonify(v));
                    });
                    self.array_elems_json_tail_spread(tail, json_arr, span)
                        .await
                } else {
                    // Add all values
                    vals.into_iter().for_each(|v| {
                        let vid =
                            self.arena.add_typed(v, ValueMeta::untyped(), span);
                        acc.push(vid);
                    });
                    self.array_elems_spread(tail, acc, span).await
                }
            }
        }
    }

    /// Recursively collect remaining array elements as JSON with spread support.
    #[async_recursion]
    #[allow(clippy::only_used_in_recursion)]
    async fn array_elems_json_tail_spread(
        &mut self,
        elems: &[ArrayElem],
        mut acc: Vec<serde_json::Value>,
        span: Span,
    ) -> Result<Payload> {
        match elems.split_first() {
            None => Ok(Payload::Json(Arc::new(serde_json::Value::Array(acc)))),
            Some((elem, tail)) => {
                match elem {
                    ArrayElem::Elem(id) => {
                        let val = self.eval(*id).await?;
                        acc.push(self.jsonify(&val));
                    }
                    ArrayElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        match val {
                            Payload::Array(elems) => {
                                let spread_vals: Vec<Payload> = elems
                                    .iter()
                                    .filter_map(|vid| {
                                        self.arena.get(*vid).cloned()
                                    })
                                    .collect();
                                spread_vals.iter().for_each(|v| {
                                    acc.push(self.jsonify(v));
                                });
                            }
                            Payload::Json(j)
                                if matches!(
                                    j.as_ref(),
                                    serde_json::Value::Array(_)
                                ) =>
                            {
                                if let serde_json::Value::Array(arr) =
                                    j.as_ref()
                                {
                                    arr.iter()
                                        .for_each(|v| acc.push(v.clone()));
                                }
                            }
                            _ => typechecked!("...spread", "Array"),
                        }
                    }
                }
                self.array_elems_json_tail_spread(tail, acc, span).await
            }
        }
    }

    /// Evaluate an array literal with expected union element type.
    ///
    /// Used when the array has a type annotation like `[1, "a"]: Array[Subscript]`.
    /// Unlike `array()`, this does NOT fall back to JSON for heterogeneous elements;
    /// instead, it validates each element is a member of the union and constructs
    /// `Payload::Array` with the union element type.
    #[async_recursion]
    pub(super) async fn array_with_union_elem(
        &mut self,
        elems: &[ArrayElem],
        span: Span,
    ) -> Result<Payload> {
        self.array_union_elems(elems, SmallVec::new(), span).await
    }

    /// Recursively evaluate array elements for union-typed arrays.
    #[async_recursion]
    async fn array_union_elems(
        &mut self,
        elems: &[ArrayElem],
        mut acc: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Payload> {
        match elems.split_first() {
            None => Ok(Payload::Array(Arc::new(acc))),
            Some((elem, tail)) => {
                match elem {
                    ArrayElem::Elem(id) => {
                        let s = self.ast.expr_span(*id).unwrap_or(span);
                        let v = self.eval(*id).await?;
                        let meta = self.expr_meta(*id);
                        let vid = self.arena.add_typed(v, meta, s);
                        acc.push(vid);
                    }
                    ArrayElem::Spread(id) => {
                        let v = self.eval(*id).await?;
                        match v {
                            Payload::Array(arr_elems) => {
                                arr_elems.iter().for_each(|vid| acc.push(*vid));
                            }
                            _ => typechecked!("...spread", "Array"),
                        }
                    }
                }
                self.array_union_elems(tail, acc, span).await
            }
        }
    }

    /// Evaluate a tuple literal.
    ///
    /// Unlike arrays, tuples are heterogeneous; each element can have a different type.
    #[async_recursion]
    pub(super) async fn tuple(
        &mut self,
        elems: &[ExprId],
        span: Span,
    ) -> Result<Payload> {
        self.tuple_elems(elems, SmallVec::new(), span).await
    }

    /// Recursively evaluate tuple elements, collecting values.
    #[async_recursion]
    async fn tuple_elems(
        &mut self,
        elems: &[ExprId],
        mut vals: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Payload> {
        match elems.split_first() {
            None => Ok(Payload::Tuple(Arc::new(vals))),
            Some((expr_id, tail)) => {
                let elem_span = self.ast.expr_span(*expr_id).unwrap_or(span);
                let val = self.eval(*expr_id).await?;
                let meta = self.expr_meta(*expr_id);
                let val_id = self.arena.add_typed(val, meta, elem_span);
                vals.push(val_id);
                self.tuple_elems(tail, vals, span).await
            }
        }
    }

    /// Evaluate a map literal: `{ k1 => v1, k2 => v2, ... }`.
    ///
    /// Keys must be scalar types (Bool, Int, Float, Char, String).
    /// Type checker guarantees key/value type homogeneity.
    #[async_recursion]
    pub(super) async fn map_lit(
        &mut self,
        entries: &[(ExprId, ExprId)],
        span: Span,
    ) -> Result<Payload> {
        match entries.split_first() {
            None => Ok(Payload::Map(Arc::new(IndexMap::new()))),
            Some(((k_expr, v_expr), rest)) => {
                let v_span = self.ast.expr_span(*v_expr).unwrap_or(span);

                let k_val = self.eval(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                let map_key = self.value_to_map_key(&k_val);
                let meta = self.expr_meta(*v_expr);
                let v_id = self.arena.add_typed(v_val, meta, v_span);

                let mut acc = IndexMap::new();
                acc.insert(map_key, v_id);

                self.map_lit_entries(rest, acc, span).await
            }
        }
    }

    /// Recursively evaluate map entries.
    ///
    /// Type checker guarantees key/value type homogeneity.
    #[async_recursion]
    async fn map_lit_entries(
        &mut self,
        entries: &[(ExprId, ExprId)],
        mut acc: IndexMap<MapKey, ValueId>,
        span: Span,
    ) -> Result<Payload> {
        match entries.split_first() {
            None => Ok(Payload::Map(Arc::new(acc))),
            Some(((k_expr, v_expr), tail)) => {
                let v_span = self.ast.expr_span(*v_expr).unwrap_or(span);

                let k_val = self.eval(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                let map_key = self.value_to_map_key(&k_val);
                let meta = self.expr_meta(*v_expr);
                let v_id = self.arena.add_typed(v_val, meta, v_span);
                acc.insert(map_key, v_id);

                self.map_lit_entries(tail, acc, span).await
            }
        }
    }

    /// Convert a value to a `MapKey`.
    ///
    /// Type checker guarantees map keys are scalar types.
    fn value_to_map_key(&self, v: &Payload) -> MapKey {
        MapKey::from_payload(v)
            .unwrap_or_else(|| typechecked!("map key", "Scalar"))
    }

    /// Evaluate tuple index access: `tuple.0`, `tuple.1`, etc.
    ///
    /// Type checker guarantees base is a tuple and index is in bounds.
    #[async_recursion]
    pub(super) async fn tuple_index(
        &mut self,
        base: ExprId,
        idx: u32,
        _span: Span,
    ) -> Result<Payload> {
        let base_val = self.eval(base).await?;

        match &base_val {
            Payload::Tuple(elems) => Ok(elems
                .get(idx as usize)
                .and_then(|id| self.arena.get(*id).cloned())
                .unwrap_or_else(|| typechecked!(".N", "valid tuple index"))),
            _ => typechecked!(".N", "Tuple"),
        }
    }

    /// Evaluate index access (array, map, or string).
    ///
    /// Type checker guarantees base/index types are compatible.
    /// Index out of bounds and key not found remain runtime errors.
    #[async_recursion]
    pub(super) async fn index(
        &mut self,
        base: ExprId,
        idx: ExprId,
        span: Span,
    ) -> Result<Payload> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;

        match (&base_val, &idx_val) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let index = if *i < 0 {
                    // Negative indexing from end
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| elems.get(idx))
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("array index {i} out of bounds"),
                        )
                    })
            }
            (Payload::Map(entries), key) => {
                let map_key = self.value_to_map_key(key);
                entries
                    .get(&map_key)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("map key not found: {key:?}"),
                        )
                    })
            }
            (Payload::String(sid), Payload::Int(i)) => {
                let s = self.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                s.chars().nth(index as usize).map(Payload::Char).ok_or_else(
                    || {
                        Error::runtime(
                            span,
                            format!("string index {i} out of bounds"),
                        )
                    },
                )
            }
            // User-defined Indexable instance (Tagged)
            (Payload::Tagged(_, _, _), _) => {
                let base_id =
                    self.arena.add_typed(base_val, self.expr_meta(base), span);
                let idx_id =
                    self.arena.add_typed(idx_val, self.expr_meta(idx), span);
                let mid = self.arena.intern("index");
                self.dispatch_class_method(
                    Some(base),
                    ClassId::INDEXABLE,
                    mid,
                    &[base_id, idx_id],
                    span,
                )
                .await
            }
            _ => typechecked!("[]", "Indexable"),
        }
    }

    /// Evaluate optional index access (safe indexing).
    ///
    /// Returns `Option.Some(value)` on success, `Option.None` on out-of-bounds.
    /// Unlike `index`, this never raises a runtime error for bounds issues.
    #[async_recursion]
    pub(super) async fn optional_index(
        &mut self,
        base: ExprId,
        idx: ExprId,
        span: Span,
    ) -> Result<Payload> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;

        match (&base_val, &idx_val) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                Ok(index
                    .and_then(|idx| elems.get(idx).copied())
                    .map(Payload::some)
                    .unwrap_or_else(Payload::none))
            }
            (Payload::Map(entries), key) => {
                let map_key = self.value_to_map_key(key);
                Ok(entries
                    .get(&map_key)
                    .copied()
                    .map(Payload::some)
                    .unwrap_or_else(Payload::none))
            }
            (Payload::String(sid), Payload::Int(i)) => {
                let s = self.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                Ok(s.chars()
                    .nth(index as usize)
                    .map(|c| {
                        let char_id = self.add_val(
                            Payload::Char(c),
                            self.runtime_types.meta_char(),
                            span,
                        );
                        Payload::some(char_id)
                    })
                    .unwrap_or_else(Payload::none))
            }
            // User-defined Indexable instance (Tagged)
            (Payload::Tagged(_, _, _), _) => {
                let base_id =
                    self.arena.add_typed(base_val, self.expr_meta(base), span);
                let idx_id =
                    self.arena.add_typed(idx_val, self.expr_meta(idx), span);
                let mid = self.arena.intern("get");
                self.dispatch_class_method(
                    Some(base),
                    ClassId::INDEXABLE,
                    mid,
                    &[base_id, idx_id],
                    span,
                )
                .await
            }
            _ => typechecked!("?[]", "Indexable"),
        }
    }

    /// Evaluate field access on an object value.
    ///
    /// After name resolution, this method is primarily for runtime field access
    /// on `Payload::Object`. Zero-arity variants like `Option.None` are resolved
    /// to `Expr::Variant` at parse time.
    ///
    /// For user-defined types registered at runtime, this also handles type
    /// paths that weren't resolved during the parse-time resolution pass.
    #[async_recursion]
    pub(super) async fn field(
        &mut self,
        base: ExprId,
        field: &StringId,
        span: Span,
    ) -> Result<Payload> {
        // Check if base is a type name (for user-defined types registered at runtime)
        let maybe_type_path = self.ast.get_expr(base).and_then(|e| match e {
            Expr::Var(ty_name) => self
                .registry
                .lookup(&QualifiedName::local(*ty_name))
                .and_then(|type_id| {
                    self.registry.lookup_variant(type_id, *field).and_then(
                        |v| (v.arity == 0).then_some((*ty_name, *field)),
                    )
                }),
            _ => None,
        });

        if let Some((ty_id, var_id)) = maybe_type_path {
            self.path(&[ty_id, var_id], span)
        } else {
            let base_val = self.eval(base).await?;
            // Unwrap Union/Newtype to find the inner Object/Json
            let unwrapped = self.unwrap_value_recursive(&base_val);
            let v = unwrapped.as_ref().unwrap_or(&base_val);

            match v {
                Payload::Object(obj) => Ok(obj
                    .get(field)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| typechecked!(".field", "field exists"))),
                // JSON field access returns Json (null for missing)
                Payload::Json(j) => {
                    let fs = self.arena.strings.get(*field).unwrap_or_default();
                    Ok(Payload::Json(Arc::new(
                        j.get(fs).cloned().unwrap_or(serde_json::Value::Null),
                    )))
                }
                // Type checker guarantees field access is on Object or Json
                _ => typechecked!(".field", "Object | Json"),
            }
        }
    }

    /// Evaluate optional field access: `expr?.field`.
    ///
    /// - If base is `Option.None`, returns `Option.None`
    /// - If base is `Option.Some(v)`, accesses field on `v`, wraps in `Some`
    /// - If base is any other value, accesses field normally, wraps in `Some`
    #[async_recursion]
    pub(super) async fn optional_field(
        &mut self,
        base: ExprId,
        field: &StringId,
        span: Span,
    ) -> Result<Payload> {
        let base_val = self.eval(base).await?;

        match &base_val {
            // Option.None -> Option.None (short-circuit)
            Payload::Tagged(ty_id, 0, _) if *ty_id == TypeId::OPTION => {
                Ok(Payload::none())
            }
            // Option.Some(v) -> try field on v; Some(field) if exists, None if not
            Payload::Tagged(ty_id, 1, payload) if *ty_id == TypeId::OPTION => {
                let inner = payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("?.field", "Option.Some has payload")
                    });
                self.try_field_access(&inner, field, span)
            }
            // Non-Option value -> try field; Some(field) if exists, None if not
            other => self.try_field_access(other, field, span),
        }
    }

    /// Helper for field access on a value (without wrapping in Option).
    ///
    /// Type checker guarantees val is Object and field exists.
    pub(super) fn field_access(
        &mut self,
        val: &Payload,
        field: &StringId,
    ) -> Payload {
        match val {
            Payload::Object(obj) => obj
                .get(field)
                .and_then(|id| self.arena.get(*id).cloned())
                .unwrap_or_else(|| typechecked!(".field", "field exists")),
            // Type checker guarantees field access is on Object
            _ => typechecked!(".field", "Object"),
        }
    }

    /// Try to access a field; returns `Some(field)` or `None` if missing.
    ///
    /// For optional field access (`?.`) where field may not exist.
    fn try_field_access(
        &mut self,
        val: &Payload,
        field: &StringId,
        span: Span,
    ) -> Result<Payload> {
        match val {
            Payload::Object(obj) => Ok(obj
                .get(field)
                .and_then(|id| self.arena.get(*id).cloned())
                .map(|v| {
                    let id =
                        self.arena.add_typed(v, ValueMeta::untyped(), span);
                    Payload::some(id)
                })
                .unwrap_or_else(Payload::none)),
            _ => typechecked!("?.field", "Object"),
        }
    }
}
