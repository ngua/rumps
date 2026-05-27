//! Collection operations: objects, arrays, tuples, indexing, field access.

use std::sync::Arc;

use async_recursion::async_recursion;
use indexmap::IndexMap;
use smallvec::SmallVec;

use super::call::ClassDispatch;
use super::Interpreter;
use crate::ast::{ArrayElem, Expr, ExprId, ObjectEntry};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::typecheck::{RuntimeTyId, Ty};
use crate::value::{
    MapKey, Payload, TypeDef, TypeId, Value, ValueId, ValueMeta,
};
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
                        let val_id = self.add_value(val, expr_span);
                        acc.insert(*key, val_id);
                    }
                    ObjectEntry::Spread(expr_id) => {
                        let val = self.eval_payload(*expr_id).await?;
                        // Type checker guarantees this is an Object
                        match &val {
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
        id: ExprId,
        elems: &[ArrayElem],
        span: Span,
    ) -> Result<Payload> {
        let union_elem = self.array_union_elem(id);
        self.array_elems_spread(elems, SmallVec::new(), union_elem, span)
            .await
    }

    fn array_union_elem(&self, id: ExprId) -> Option<RuntimeTyId> {
        match self.checked.types.get(self.checked.expr(id).ty) {
            Ty::Array(elem) => {
                let elem = RuntimeTyId::from(*elem);
                matches!(self.checked.types.get(elem), Ty::Union(..))
                    .then_some(elem)
            }
            _ => None,
        }
    }

    fn array_elem_meta(
        &self,
        id: ExprId,
        union_elem: Option<RuntimeTyId>,
    ) -> ValueMeta {
        let meta = self.expr_meta(id);
        union_elem
            .map_or(meta, |ty| self.checked.types.union_meta(ty, meta.repr))
    }

    fn spread_elem_meta(
        &self,
        id: ValueId,
        union_elem: Option<RuntimeTyId>,
    ) -> ValueMeta {
        let meta = self
            .arena
            .meta(id)
            .unwrap_or_else(|| typechecked!("array spread", "value metadata"));
        union_elem
            .map_or(meta, |ty| self.checked.types.union_meta(ty, meta.repr))
    }

    fn spread_elem_value(
        &self,
        id: ValueId,
        union_elem: Option<RuntimeTyId>,
    ) -> Value {
        let v = self
            .arena
            .value(id)
            .cloned()
            .unwrap_or_else(|| invariant!("array element in arena"));
        let meta = self.spread_elem_meta(id, union_elem);
        self.value_with_context_meta(v, meta)
    }

    fn array_ids_to_json(&mut self, ids: &[ValueId]) -> Vec<serde_json::Value> {
        let vals: Vec<Value> = ids
            .iter()
            .filter_map(|vid| self.arena.value(*vid).cloned())
            .collect();
        vals.iter().map(|v| self.jsonify_value(v)).collect()
    }

    /// Recursively evaluate array elements with spread support.
    #[async_recursion]
    async fn array_elems_spread(
        &mut self,
        elems: &[ArrayElem],
        mut acc: SmallVec<[ValueId; 4]>,
        union_elem: Option<RuntimeTyId>,
        span: Span,
    ) -> Result<Payload> {
        match elems.split_first() {
            None => Ok(Payload::Array(Arc::new(acc))),
            Some((elem, tail)) => match elem {
                ArrayElem::Elem(id) => {
                    let s = self.ast.expr_span(*id).unwrap_or(span);
                    let v = self.eval(*id).await?;
                    if matches!(v.payload, Payload::Json(_)) {
                        let mut json_arr = self.array_ids_to_json(&acc);
                        json_arr.push(self.jsonify_value(&v));
                        self.array_elems_json_tail_spread(tail, json_arr, span)
                            .await
                    } else {
                        let meta = self.array_elem_meta(*id, union_elem);
                        let v = self.value_with_context_meta(v, meta);
                        let vid = self.add_value(v, s);
                        acc.push(vid);
                        self.array_elems_spread(tail, acc, union_elem, span)
                            .await
                    }
                }
                ArrayElem::Spread(id) => {
                    let val = self.eval(*id).await?;
                    match &val.payload {
                        Payload::Array(arr) => {
                            let vals: SmallVec<[Value; 4]> = arr
                                .iter()
                                .filter_map(|vid| {
                                    self.arena.value(*vid).cloned()
                                })
                                .collect();
                            let has_json = vals
                                .iter()
                                .any(|v| matches!(v.payload, Payload::Json(_)));
                            if has_json {
                                let mut json_arr = self.array_ids_to_json(&acc);
                                vals.iter().for_each(|v| {
                                    json_arr.push(self.jsonify_value(v));
                                });
                                self.array_elems_json_tail_spread(
                                    tail, json_arr, span,
                                )
                                .await
                            } else if union_elem.is_some() {
                                let vals: SmallVec<[Value; 4]> = arr
                                    .iter()
                                    .map(|&vid| {
                                        self.spread_elem_value(vid, union_elem)
                                    })
                                    .collect();
                                vals.into_iter().for_each(|v| {
                                    let vid = self.add_value(v, span);
                                    acc.push(vid);
                                });
                                self.array_elems_spread(
                                    tail, acc, union_elem, span,
                                )
                                .await
                            } else {
                                acc.extend(arr.iter().copied());
                                self.array_elems_spread(
                                    tail, acc, union_elem, span,
                                )
                                .await
                            }
                        }
                        _ => typechecked!("...spread", "Array"),
                    }
                }
            },
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
                        acc.push(self.jsonify_value(&val));
                    }
                    ArrayElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        match &val.payload {
                            Payload::Array(elems) => {
                                let spread_vals: Vec<Value> = elems
                                    .iter()
                                    .filter_map(|vid| {
                                        self.arena.value(*vid).cloned()
                                    })
                                    .collect();
                                spread_vals.iter().for_each(|v| {
                                    acc.push(self.jsonify_value(v));
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
    /// Used when typecheck metadata gives the array a union element type.
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
                        let v = self.value_with_context_meta(v, meta);
                        let vid = self.add_value(v, s);
                        acc.push(vid);
                    }
                    ArrayElem::Spread(id) => {
                        let v = self.eval_payload(*id).await?;
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
                let val_id = self.add_value(val, elem_span);
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

                let k_val = self.eval_payload(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                let map_key = self.value_to_map_key(&k_val);
                let v_id = self.add_value(v_val, v_span);

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

                let k_val = self.eval_payload(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                let map_key = self.value_to_map_key(&k_val);
                let v_id = self.add_value(v_val, v_span);
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

    fn value_type_id(&self, v: &Value) -> Option<TypeId> {
        self.checked
            .types
            .to_type_id(v.repr)
            .or_else(|| self.checked.types.to_type_id(v.ty))
    }

    fn is_sum_value(&self, v: &Value) -> bool {
        self.value_type_id(v)
            .and_then(|ty| self.registry.get_def(ty))
            .is_some_and(|def| matches!(def, TypeDef::Sum { .. }))
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
    ) -> Result<Value> {
        let base_val = self.eval_payload(base).await?;

        match &base_val {
            Payload::Tuple(elems) => Ok(elems
                .get(idx as usize)
                .and_then(|id| self.arena.value(*id).cloned())
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
        expr_id: ExprId,
        base: ExprId,
        idx: ExprId,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;
        let base_payload = base_val.payload.clone();
        let idx_payload = idx_val.payload.clone();

        match (&base_payload, &idx_payload) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let index = if *i < 0 {
                    // Negative indexing from end
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| elems.get(idx))
                    .and_then(|id| self.arena.value(*id).cloned())
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
                    .and_then(|id| self.arena.value(*id).cloned())
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
                s.chars()
                    .nth(index as usize)
                    .map(|c| self.value_for_expr(expr_id, Payload::Char(c)))
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("string index {i} out of bounds"),
                        )
                    })
            }
            // Sum-type `Indexable` instance.
            (Payload::Variant { .. }, _) if self.is_sum_value(&base_val) => {
                let base_id = self.add_value(base_val, span);
                let idx_id = self.add_value(idx_val, span);
                let mid = self.arena.intern("index");
                self.dispatch_class_method_value(ClassDispatch {
                    dispatch_expr_id: Some(expr_id),
                    output_expr_id: Some(expr_id),
                    class: ClassId::INDEXABLE,
                    method: mid,
                    args: SmallVec::from_slice(&[base_id, idx_id]),
                    span,
                })
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
        expr_id: ExprId,
        base: ExprId,
        idx: ExprId,
        span: Span,
    ) -> Result<Payload> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;
        let base_payload = base_val.payload.clone();
        let idx_payload = idx_val.payload.clone();

        match (&base_payload, &idx_payload) {
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
                            self.checked.types.meta_char(),
                            span,
                        );
                        Payload::some(char_id)
                    })
                    .unwrap_or_else(Payload::none))
            }
            // Sum-type `Indexable` instance.
            (Payload::Variant { .. }, _) if self.is_sum_value(&base_val) => {
                let base_id = self.add_value(base_val, span);
                let idx_id = self.add_value(idx_val, span);
                let mid = self.arena.intern("get");
                self.dispatch_class_method(
                    Some(expr_id),
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
        expr_id: ExprId,
        base: ExprId,
        field: &StringId,
        span: Span,
    ) -> Result<Value> {
        // Check if base is a type name (for user-defined types registered at runtime)
        let maybe_type_path = self.ast.get_expr(base).and_then(|e| match e {
            Expr::Var(ty_name) => self
                .registry
                .lookup(&QualifiedName::local(*ty_name))
                .and_then(|type_id| {
                    self.registry.lookup_variant(type_id, *field).map(|v| {
                        (QualifiedName::local(*ty_name), *field, v.arity)
                    })
                }),
            _ => None,
        });

        if let Some((ty, var, 0)) = maybe_type_path {
            self.path(&[ty.local_name(), var], span)
                .map(|payload| self.value_for_expr(expr_id, payload))
        } else if let Some((ty, var, _)) = maybe_type_path {
            Ok(self.value_for_expr(expr_id, Payload::VariantCtor { ty, var }))
        } else {
            let base_val = self.eval_payload(base).await?;

            match &base_val {
                Payload::Object(obj) => Ok(obj
                    .get(field)
                    .and_then(|id| self.arena.value(*id).cloned())
                    .unwrap_or_else(|| typechecked!(".field", "field exists"))),
                // JSON field access returns Json (null for missing)
                Payload::Json(j) => {
                    let fs = self.arena.strings.get(*field).unwrap_or_default();
                    Ok(self.value_for_expr(
                        expr_id,
                        Payload::Json(Arc::new(
                            j.get(fs)
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        )),
                    ))
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
        let base_ty = self.value_type_id(&base_val);

        match &base_val.payload {
            // Option.None -> Option.None (short-circuit)
            Payload::Variant { tag: 0, .. }
                if base_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Ok(Payload::none())
            }
            // Option.Some(v) -> try field on v; Some(field) if exists, None if not
            Payload::Variant { tag: 1, vals }
                if base_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                let inner = vals
                    .first()
                    .and_then(|id| self.arena.payload(*id).cloned())
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
                .and_then(|id| self.arena.payload(*id).cloned())
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
        _span: Span,
    ) -> Result<Payload> {
        match val {
            Payload::Object(obj) => Ok(obj
                .get(field)
                .copied()
                .map(Payload::some)
                .unwrap_or_else(Payload::none)),
            _ => typechecked!("?.field", "Object"),
        }
    }
}
