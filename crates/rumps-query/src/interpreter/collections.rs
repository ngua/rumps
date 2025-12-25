//! Collection operations: objects, arrays, tuples, indexing, field access.

use async_recursion::async_recursion;
use indexmap::IndexMap;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{MapKey, TypeExprId, TypeId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate an object literal.
    #[async_recursion]
    pub(super) async fn object(
        &mut self,
        fields: &[(String, ExprId)],
    ) -> Result<Value> {
        let map = self.object_fields(fields, IndexMap::new()).await?;
        Ok(Value::Object(map))
    }

    /// Recursively evaluate object fields.
    #[async_recursion]
    async fn object_fields(
        &mut self,
        fields: &[(String, ExprId)],
        mut acc: IndexMap<StringId, ValueId>,
    ) -> Result<IndexMap<StringId, ValueId>> {
        match fields.split_first() {
            None => Ok(acc),
            Some(((key, expr_id), tail)) => {
                let span = self.ast.expr_span(*expr_id).unwrap_or_default();
                let val = self.eval(*expr_id).await?;
                let key_id = self.arena.intern(key);
                let val_id = self.arena.add(val, span);
                acc.insert(key_id, val_id);
                self.object_fields(tail, acc).await
            }
        }
    }

    /// Evaluate an array literal, enforcing homogeneous element types.
    ///
    /// Arrays containing `Json` values (including `null`) immediately become
    /// JSON arrays, since `Json` is not a native RUMPS type.
    #[async_recursion]
    pub(super) async fn array(&mut self, elems: &[ExprId]) -> Result<Value> {
        match elems.split_first() {
            None => {
                // Empty array has element type `UNKNOWN`
                let elem_ty = self.type_exprs.named(TypeId::UNKNOWN);
                Ok(Value::Array(elem_ty, SmallVec::new()))
            }
            Some((first, rest)) => {
                let first_span = self.ast.expr_span(*first).unwrap_or_default();
                let first_val = self.eval(*first).await?;

                // If first element is Json, entire array becomes Json
                if matches!(&first_val, Value::Json(_)) {
                    self.array_elems_json_start(rest, first_val).await
                } else {
                    let elem_ty = self.value_type_expr(&first_val);
                    let first_id = self.arena.add(first_val, first_span);

                    let mut acc = SmallVec::new();
                    acc.push(first_id);

                    self.array_elems(rest, elem_ty, acc, first_span).await
                }
            }
        }
    }

    /// Recursively evaluate and type-check array elements.
    ///
    /// If all elements have the same type, returns `Value::Array`.
    /// If types are heterogeneous or any element is `Json`, converts to JSON.
    #[async_recursion]
    async fn array_elems(
        &mut self,
        elems: &[ExprId],
        elem_ty: TypeExprId,
        mut acc: SmallVec<[ValueId; 4]>,
        _first_span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Array(elem_ty, acc)),
            Some((expr_id, tail)) => {
                let span = self.ast.expr_span(*expr_id).unwrap_or_default();
                let val = self.eval(*expr_id).await?;

                // Any Json value (including null) triggers JSON array mode
                if matches!(&val, Value::Json(_)) {
                    self.array_elems_json(tail, acc, val).await
                } else {
                    let val_ty = self.value_type_expr(&val);

                    if self.type_exprs.eq(elem_ty, val_ty) {
                        let val_id = self.arena.add(val, span);
                        acc.push(val_id);
                        self.array_elems(tail, elem_ty, acc, _first_span).await
                    } else {
                        // Heterogeneous: convert accumulated + current + rest to JSON
                        self.array_elems_json(tail, acc, val).await
                    }
                }
            }
        }
    }

    /// Continue collecting array elements as JSON (heterogeneous array).
    ///
    /// Called when a type mismatch is detected. Converts all accumulated
    /// values to JSON and continues collecting the rest as JSON.
    #[async_recursion]
    async fn array_elems_json(
        &mut self,
        elems: &[ExprId],
        acc: SmallVec<[ValueId; 4]>,
        current: Value,
    ) -> Result<Value> {
        // Convert accumulated values to JSON
        let mut json_arr: Vec<serde_json::Value> = acc
            .iter()
            .filter_map(|vid| self.arena.get(*vid))
            .map(|v| self.jsonify(v))
            .collect::<Result<Vec<_>>>()?;

        // Add current value
        json_arr.push(self.jsonify(&current)?);

        // Collect remaining elements as JSON
        self.array_elems_json_tail(elems, json_arr).await
    }

    /// Recursively collect remaining array elements as JSON.
    #[async_recursion]
    async fn array_elems_json_tail(
        &mut self,
        elems: &[ExprId],
        mut acc: Vec<serde_json::Value>,
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Json(serde_json::Value::Array(acc))),
            Some((expr_id, tail)) => {
                let val = self.eval(*expr_id).await?;
                acc.push(self.jsonify(&val)?);
                self.array_elems_json_tail(tail, acc).await
            }
        }
    }

    /// Start collecting array elements as JSON from the first element.
    ///
    /// Called when the first element is already a `Json` value.
    #[async_recursion]
    async fn array_elems_json_start(
        &mut self,
        elems: &[ExprId],
        first: Value,
    ) -> Result<Value> {
        let acc = vec![self.jsonify(&first)?];
        self.array_elems_json_tail(elems, acc).await
    }

    /// Evaluate a tuple literal.
    ///
    /// Unlike arrays, tuples are heterogeneous; each element can have a different type.
    #[async_recursion]
    pub(super) async fn tuple(
        &mut self,
        elems: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        self.tuple_elems(elems, SmallVec::new(), SmallVec::new(), span)
            .await
    }

    /// Recursively evaluate tuple elements, collecting values and types.
    #[async_recursion]
    async fn tuple_elems(
        &mut self,
        elems: &[ExprId],
        mut vals: SmallVec<[ValueId; 4]>,
        mut tys: SmallVec<[TypeExprId; 4]>,
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => {
                let ty = self.type_exprs.tuple(tys);
                Ok(Value::Tuple(ty, vals))
            }
            Some((expr_id, tail)) => {
                let elem_span = self.ast.expr_span(*expr_id).unwrap_or(span);
                let val = self.eval(*expr_id).await?;
                let ty = self.value_type_expr(&val);
                let val_id = self.arena.add(val, elem_span);
                vals.push(val_id);
                tys.push(ty);
                self.tuple_elems(tail, vals, tys, span).await
            }
        }
    }

    /// Evaluate a map literal: `{ k1 => v1, k2 => v2, ... }`.
    ///
    /// Keys must be scalar types (Bool, Int, Float, Char, String).
    /// Both keys and values are checked for homogeneity.
    #[async_recursion]
    pub(super) async fn map_lit(
        &mut self,
        entries: &[(ExprId, ExprId)],
        span: Span,
    ) -> Result<Value> {
        match entries.split_first() {
            None => {
                // Empty map has unknown key/value types
                let k_ty = self.type_exprs.named(TypeId::UNKNOWN);
                let v_ty = self.type_exprs.named(TypeId::UNKNOWN);
                Ok(Value::Map(k_ty, v_ty, IndexMap::new()))
            }
            Some(((k_expr, v_expr), rest)) => {
                let k_span = self.ast.expr_span(*k_expr).unwrap_or(span);
                let v_span = self.ast.expr_span(*v_expr).unwrap_or(span);

                let k_val = self.eval(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                let k_ty = self.value_type_expr(&k_val);
                let v_ty = self.value_type_expr(&v_val);

                let map_key = self.value_to_map_key(&k_val, k_span)?;
                let v_id = self.arena.add(v_val, v_span);

                let mut acc = IndexMap::new();
                acc.insert(map_key, v_id);

                self.map_lit_entries(rest, k_ty, v_ty, acc, k_span, span)
                    .await
            }
        }
    }

    /// Recursively evaluate and type-check map entries.
    #[async_recursion]
    async fn map_lit_entries(
        &mut self,
        entries: &[(ExprId, ExprId)],
        k_ty: TypeExprId,
        v_ty: TypeExprId,
        mut acc: IndexMap<MapKey, ValueId>,
        first_k_span: Span,
        span: Span,
    ) -> Result<Value> {
        match entries.split_first() {
            None => Ok(Value::Map(k_ty, v_ty, acc)),
            Some(((k_expr, v_expr), tail)) => {
                let k_span = self.ast.expr_span(*k_expr).unwrap_or(span);
                let v_span = self.ast.expr_span(*v_expr).unwrap_or(span);

                let k_val = self.eval(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                let this_k_ty = self.value_type_expr(&k_val);
                let this_v_ty = self.value_type_expr(&v_val);

                // Check key type homogeneity
                if !self.type_exprs.eq(k_ty, this_k_ty) {
                    let expected = self.type_expr_name(k_ty);
                    let got = self.type_expr_name(this_k_ty);
                    Err(Error::runtime_type(
                        k_span,
                        format!(
                            "map key type mismatch: expected {expected} \
                             (from {}..{}), got {got}",
                            first_k_span.start, first_k_span.end
                        ),
                    ))?;
                }

                // Check value type homogeneity
                if !self.type_exprs.eq(v_ty, this_v_ty) {
                    let expected = self.type_expr_name(v_ty);
                    let got = self.type_expr_name(this_v_ty);
                    Err(Error::runtime_type(
                        v_span,
                        format!(
                            "map value type mismatch: expected {expected}, got {got}"
                        ),
                    ))?;
                }

                let map_key = self.value_to_map_key(&k_val, k_span)?;
                let v_id = self.arena.add(v_val, v_span);
                acc.insert(map_key, v_id);

                self.map_lit_entries(tail, k_ty, v_ty, acc, first_k_span, span)
                    .await
            }
        }
    }

    /// Convert a value to a `MapKey`, or error if not a scalar.
    fn value_to_map_key(&self, v: &Value, span: Span) -> Result<MapKey> {
        MapKey::from_value(v).ok_or_else(|| {
            Error::runtime_type(
                span,
                format!(
                    "map keys must be scalar (Bool, Int, Float, Char, String); \
                     got {}",
                    v.type_name(&self.registry, &self.type_exprs, &self.arena)
                ),
            )
        })
    }

    /// Evaluate tuple index access: `tuple.0`, `tuple.1`, etc.
    #[async_recursion]
    pub(super) async fn tuple_index(
        &mut self,
        base: ExprId,
        idx: u32,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;

        match &base_val {
            Value::Tuple(_, elems) => elems
                .get(idx as usize)
                .and_then(|id| self.arena.get(*id).cloned())
                .ok_or_else(|| {
                    Error::runtime(
                        span,
                        format!(
                            "tuple index `{idx}` out of bounds; tuple has {} element(s)",
                            elems.len()
                        ),
                    )
                }),
            _ => Err(Error::runtime_type(
                span,
                format!(
                    "cannot index `{}` with `.{idx}`; expected tuple",
                    base_val.type_name(&self.registry, &self.type_exprs, &self.arena)
                ),
            )),
        }
    }

    /// Evaluate index access (array, map, or string).
    #[async_recursion]
    pub(super) async fn index(
        &mut self,
        base: ExprId,
        idx: ExprId,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;

        match (&base_val, &idx_val) {
            (Value::Array(_, elems), Value::Int(i)) => {
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
            (Value::Map(_, _, entries), key) => {
                let map_key = self.value_to_map_key(key, span)?;
                entries
                    .get(&map_key)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "key not found in map".to_string())
                    })
            }
            (Value::String(sid), Value::Int(i)) => {
                let s = self.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                s.chars().nth(index as usize).map(Value::Char).ok_or_else(
                    || {
                        Error::runtime(
                            span,
                            format!("string index {i} out of bounds"),
                        )
                    },
                )
            }
            _ => Err(Error::runtime_type(
                span,
                format!(
                    "cannot index {} with {}",
                    base_val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena
                    ),
                    idx_val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena
                    )
                ),
            )),
        }
    }

    /// Evaluate field access on an object value.
    ///
    /// After name resolution, this method is primarily for runtime field access
    /// on `Value::Object`. Zero-arity variants like `Option.None` are resolved
    /// to `Expr::Variant` at parse time.
    ///
    /// For user-defined types registered at runtime, this also handles type
    /// paths that weren't resolved during the parse-time resolution pass.
    #[async_recursion]
    pub(super) async fn field(
        &mut self,
        base: ExprId,
        field: &str,
        span: Span,
    ) -> Result<Value> {
        // Check if base is a type name (for user-defined types registered at runtime)
        let maybe_type_path = self.ast.get_expr(base).and_then(|e| match e {
            Expr::Var(ty_name) => {
                let ty_id = self.arena.intern(ty_name);
                self.registry.lookup(ty_id).and_then(|type_id| {
                    let var_id = self.arena.intern(field);
                    self.registry.lookup_variant(type_id, var_id).and_then(
                        |v| {
                            (v.arity == 0)
                                .then(|| (ty_name.clone(), field.to_string()))
                        },
                    )
                })
            }
            _ => None,
        });

        if let Some((ty_name, var_name)) = maybe_type_path {
            self.path(&[ty_name, var_name], span)
        } else {
            let base_val = self.eval(base).await?;

            match &base_val {
                Value::Object(obj) => {
                    let field_id = self.arena.intern(field);
                    obj.get(&field_id)
                        .and_then(|id| self.arena.get(*id).cloned())
                        .ok_or_else(|| {
                            Error::runtime(
                                span,
                                format!("field `{field}` not found"),
                            )
                        })
                }
                // JSON field access returns Json (null for missing)
                Value::Json(j) => Ok(Value::Json(
                    j.get(field).cloned().unwrap_or(serde_json::Value::Null),
                )),
                _ => Err(Error::runtime_type(
                    span,
                    format!(
                        "cannot access field on {}",
                        base_val.type_name(
                            &self.registry,
                            &self.type_exprs,
                            &self.arena
                        )
                    ),
                )),
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
        field: &str,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;

        match &base_val {
            // Option.None -> Option.None (short-circuit)
            Value::Tagged(ty_expr, 0, _)
                if self
                    .type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                Ok(self.make_none_like(*ty_expr))
            }
            // Option.Some(v) -> access field on v, wrap in Some
            Value::Tagged(ty_expr, 1, payload)
                if self
                    .type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                let inner = payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Option.Some missing payload")
                    })?;
                let result = self.field_access(&inner, field, span)?;
                let result_id = self.arena.add(result, span);
                Ok(self.make_some(result_id))
            }
            // Non-Option value -> access field normally, wrap in Some
            other => {
                let result = self.field_access(other, field, span)?;
                let result_id = self.arena.add(result, span);
                Ok(self.make_some(result_id))
            }
        }
    }

    /// Helper for field access on a value (without wrapping in Option).
    pub(super) fn field_access(
        &mut self,
        val: &Value,
        field: &str,
        span: Span,
    ) -> Result<Value> {
        match val {
            Value::Object(obj) => {
                let field_id = self.arena.intern(field);
                obj.get(&field_id)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("field `{field}` not found"),
                        )
                    })
            }
            _ => Err(Error::runtime_type(
                span,
                format!(
                    "cannot access field on {}",
                    val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena
                    )
                ),
            )),
        }
    }
}
