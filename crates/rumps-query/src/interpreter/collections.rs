//! Collection operations: objects, arrays, tuples, indexing, field access.

use async_recursion::async_recursion;
use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::Interpreter;
use crate::ast::{ArrayElem, Expr, ExprId, ObjectEntry};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::value::{MapKey, TypeExprId, TypeId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate an object literal with potential spread entries.
    ///
    /// Type checker guarantees spreads are on object types.
    #[async_recursion]
    pub(super) async fn object(
        &mut self,
        entries: &[ObjectEntry],
        span: Span,
    ) -> Result<Value> {
        let map = self.object_entries(entries, IndexMap::new(), span).await?;
        Ok(Value::Object(map))
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
                        let val_id = self.arena.add(val, expr_span);
                        acc.insert(*key, val_id);
                    }
                    ObjectEntry::Spread(expr_id) => {
                        let val = self.eval(*expr_id).await?;
                        // Unwrap Union/Newtype to find the inner Object
                        let unwrapped = self.unwrap_value_recursive(&val);
                        let v = unwrapped.as_ref().unwrap_or(&val);
                        // Type checker guarantees this is an Object
                        match v {
                            Value::Object(fields) => {
                                // Merge fields from spread object
                                fields.clone().into_iter().for_each(
                                    |(k, v)| {
                                        acc.insert(k, v);
                                    },
                                );
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
    ) -> Result<Value> {
        match elems.split_first() {
            None => {
                // Empty array has element type `UNKNOWN`
                let elem_ty = self.type_exprs.named(TypeId::UNKNOWN);
                Ok(Value::Array(elem_ty, SmallVec::new()))
            }
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
                            Value::Array(_, elems) => {
                                let vals: SmallVec<[Value; 4]> = elems
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
                    first_vals.iter().any(|v| matches!(v, Value::Json(_)));
                if has_json {
                    // Start in JSON mode
                    let json_acc: Vec<serde_json::Value> = first_vals
                        .into_iter()
                        .map(|v| self.jsonify(&v))
                        .collect();
                    self.array_elems_json_tail_spread(rest, json_acc, span)
                        .await
                } else {
                    let mut iter = first_vals.into_iter();
                    if let Some(first_val) = iter.next() {
                        let elem_ty = self.value_type_expr(&first_val);
                        let first_id = self.arena.add(first_val, first_span);
                        let mut acc = SmallVec::new();
                        acc.push(first_id);

                        // Add rest of first_vals
                        iter.try_for_each(|v| -> Result<()> {
                            let vt = self.value_type_expr(&v);
                            if self.type_exprs.eq(elem_ty, vt) {
                                let vid = self.arena.add(v, first_span);
                                acc.push(vid);
                            }
                            Ok(())
                        })?;

                        self.array_elems_spread(rest, elem_ty, acc, span).await
                    } else {
                        // first_vals was empty (spread of empty array)
                        let unknown_ty = self.type_exprs.named(TypeId::UNKNOWN);
                        self.array_elems_spread(
                            rest,
                            unknown_ty,
                            SmallVec::new(),
                            span,
                        )
                        .await
                    }
                }
            }
        }
    }

    /// Recursively evaluate array elements with spread support.
    #[async_recursion]
    async fn array_elems_spread(
        &mut self,
        elems: &[ArrayElem],
        elem_ty: TypeExprId,
        mut acc: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Array(elem_ty, acc)),
            Some((elem, tail)) => {
                let vals: SmallVec<[Value; 4]> = match elem {
                    ArrayElem::Elem(id) => {
                        let val = self.eval(*id).await?;
                        smallvec::smallvec![val]
                    }
                    ArrayElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        match val {
                            Value::Array(_, elems) => elems
                                .iter()
                                .filter_map(|vid| self.arena.get(*vid).cloned())
                                .collect(),
                            _ => typechecked!("...spread", "Array"),
                        }
                    }
                };

                // Check for Json or type mismatch
                let has_json = vals.iter().any(|v| matches!(v, Value::Json(_)));
                if has_json {
                    // Convert to JSON mode; collect values first to avoid borrow conflict
                    let acc_vals: Vec<Value> = acc
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
                    // Pre-compute value types to avoid borrow conflicts
                    let val_tys: SmallVec<[TypeExprId; 4]> =
                        vals.iter().map(|v| self.value_type_expr(v)).collect();
                    let heterogeneous = val_tys
                        .iter()
                        .any(|vt| !self.type_exprs.eq(elem_ty, *vt));

                    if heterogeneous {
                        // Convert to JSON mode; collect values first to avoid borrow conflict
                        let acc_vals: Vec<Value> = acc
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
                            let vid = self.arena.add(v, span);
                            acc.push(vid);
                        });
                        self.array_elems_spread(tail, elem_ty, acc, span).await
                    }
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
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Json(serde_json::Value::Array(acc))),
            Some((elem, tail)) => {
                match elem {
                    ArrayElem::Elem(id) => {
                        let val = self.eval(*id).await?;
                        acc.push(self.jsonify(&val));
                    }
                    ArrayElem::Spread(id) => {
                        let val = self.eval(*id).await?;
                        match val {
                            Value::Array(_, elems) => {
                                let spread_vals: Vec<Value> = elems
                                    .iter()
                                    .filter_map(|vid| {
                                        self.arena.get(*vid).cloned()
                                    })
                                    .collect();
                                spread_vals.iter().for_each(|v| {
                                    acc.push(self.jsonify(v));
                                });
                            }
                            Value::Json(serde_json::Value::Array(arr)) => {
                                arr.iter().for_each(|v| acc.push(v.clone()));
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
    /// `Value::Array` with the union element type.
    #[async_recursion]
    pub(super) async fn array_with_union_elem(
        &mut self,
        elems: &[ArrayElem],
        elem_ty: TypeExprId,
        span: Span,
    ) -> Result<Value> {
        self.array_union_elems(elems, elem_ty, SmallVec::new(), span)
            .await
    }

    /// Recursively evaluate array elements for union-typed arrays.
    #[async_recursion]
    async fn array_union_elems(
        &mut self,
        elems: &[ArrayElem],
        elem_ty: TypeExprId,
        mut acc: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Array(elem_ty, acc)),
            Some((elem, tail)) => {
                match elem {
                    ArrayElem::Elem(id) => {
                        let s = self.ast.expr_span(*id).unwrap_or(span);
                        let v = self.eval(*id).await?;
                        let vid = self.arena.add(v, s);
                        acc.push(vid);
                    }
                    ArrayElem::Spread(id) => {
                        let v = self.eval(*id).await?;
                        match v {
                            Value::Array(_, arr_elems) => {
                                arr_elems.iter().for_each(|vid| acc.push(*vid));
                            }
                            _ => typechecked!("...spread", "Array"),
                        }
                    }
                }
                self.array_union_elems(tail, elem_ty, acc, span).await
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
                let v_span = self.ast.expr_span(*v_expr).unwrap_or(span);

                let k_val = self.eval(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                let k_ty = self.value_type_expr(&k_val);
                let v_ty = self.value_type_expr(&v_val);

                let map_key = self.value_to_map_key(&k_val);
                let v_id = self.arena.add(v_val, v_span);

                let mut acc = IndexMap::new();
                acc.insert(map_key, v_id);

                self.map_lit_entries(rest, k_ty, v_ty, acc, span).await
            }
        }
    }

    /// Recursively evaluate and type-check map entries.
    ///
    /// Type checker guarantees key/value type homogeneity.
    #[async_recursion]
    async fn map_lit_entries(
        &mut self,
        entries: &[(ExprId, ExprId)],
        k_ty: TypeExprId,
        v_ty: TypeExprId,
        mut acc: IndexMap<MapKey, ValueId>,
        span: Span,
    ) -> Result<Value> {
        match entries.split_first() {
            None => Ok(Value::Map(k_ty, v_ty, acc)),
            Some(((k_expr, v_expr), tail)) => {
                let v_span = self.ast.expr_span(*v_expr).unwrap_or(span);

                let k_val = self.eval(*k_expr).await?;
                let v_val = self.eval(*v_expr).await?;

                // Type checker guarantees key/value homogeneity
                let map_key = self.value_to_map_key(&k_val);
                let v_id = self.arena.add(v_val, v_span);
                acc.insert(map_key, v_id);

                self.map_lit_entries(tail, k_ty, v_ty, acc, span).await
            }
        }
    }

    /// Convert a value to a `MapKey`.
    ///
    /// Type checker guarantees map keys are scalar types.
    fn value_to_map_key(&self, v: &Value) -> MapKey {
        MapKey::from_value(v)
            .unwrap_or_else(|| typechecked!("map key", "Scalar"))
    }

    /// Evaluate tuple index access: `tuple.0`, `tuple.1`, etc.
    ///
    /// Type checker guarantees base is a tuple and index is in bounds.
    /// Wraps the element if its type is a union/newtype.
    #[async_recursion]
    pub(super) async fn tuple_index(
        &mut self,
        base: ExprId,
        idx: u32,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;

        match &base_val {
            Value::Tuple(ty, elems) => {
                // Get element type from tuple type
                let elem_ty = self
                    .type_exprs
                    .tuple_elems(*ty)
                    .and_then(|tys| tys.get(idx as usize).copied());

                let val = elems
                    .get(idx as usize)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| typechecked!(".N", "valid tuple index"));

                // Wrap if element type is union/newtype
                Ok(match elem_ty {
                    Some(ety) => {
                        self.maybe_wrap_value(&val, ety, span).unwrap_or(val)
                    }
                    None => val,
                })
            }
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
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;

        match (&base_val, &idx_val) {
            (Value::Array(elem_ty, elems), Value::Int(i)) => {
                let elem_ty = *elem_ty;
                let index = if *i < 0 {
                    // Negative indexing from end
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| elems.get(idx))
                    .and_then(|id| self.arena.get(*id).cloned())
                    .map(|val| {
                        self.maybe_wrap_value(&val, elem_ty, span)
                            .unwrap_or(val)
                    })
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("array index {i} out of bounds"),
                        )
                    })
            }
            (Value::Map(_, v_ty, entries), key) => {
                let v_ty = *v_ty;
                let map_key = self.value_to_map_key(key);
                entries
                    .get(&map_key)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .map(|val| {
                        self.maybe_wrap_value(&val, v_ty, span).unwrap_or(val)
                    })
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("map key not found: {key:?}"),
                        )
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
            // User-defined Indexable instance (Tagged, Union, or Newtype)
            (Value::Tagged(_, _, _), _)
            | (Value::Union(_, _), _)
            | (Value::Newtype(_, _), _) => {
                let base_id = self.arena.add(base_val, span);
                let idx_id = self.arena.add(idx_val, span);
                let mid = self.arena.intern("index");
                self.dispatch_class_method(
                    Some(base),
                    crate::typecheck::BuiltinClassTag::Indexable,
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
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;

        match (&base_val, &idx_val) {
            (Value::Array(elem_ty, elems), Value::Int(i)) => {
                let elem_ty = *elem_ty;
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                let opt_ty =
                    self.type_exprs.app(TypeId::OPTION, smallvec![elem_ty]);
                Ok(index
                    .and_then(|idx| elems.get(idx))
                    .map(|id| {
                        // Wrap element if elem_ty is union/newtype
                        let wrapped_id =
                            self.maybe_wrap_value_id(*id, elem_ty, span);
                        Value::some(opt_ty, wrapped_id)
                    })
                    .unwrap_or_else(|| Value::none(opt_ty)))
            }
            (Value::Map(_, v_ty, entries), key) => {
                let v_ty = *v_ty;
                // Map indexing already returns Option, so ?[] is the same
                let map_key = self.value_to_map_key(key);
                let opt_ty =
                    self.type_exprs.app(TypeId::OPTION, smallvec![v_ty]);
                Ok(entries
                    .get(&map_key)
                    .map(|id| {
                        // Wrap value if v_ty is union/newtype
                        let wrapped_id =
                            self.maybe_wrap_value_id(*id, v_ty, span);
                        Value::some(opt_ty, wrapped_id)
                    })
                    .unwrap_or_else(|| Value::none(opt_ty)))
            }
            (Value::String(sid), Value::Int(i)) => {
                let s = self.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                let char_ty = self.type_exprs.named(TypeId::CHAR);
                let opt_ty =
                    self.type_exprs.app(TypeId::OPTION, smallvec![char_ty]);
                Ok(s.chars()
                    .nth(index as usize)
                    .map(|c| {
                        let char_id = self.arena.add(Value::Char(c), span);
                        Value::some(opt_ty, char_id)
                    })
                    .unwrap_or_else(|| Value::none(opt_ty)))
            }
            // User-defined Indexable instance (Tagged, Union, or Newtype)
            (Value::Tagged(_, _, _), _)
            | (Value::Union(_, _), _)
            | (Value::Newtype(_, _), _) => {
                let base_id = self.arena.add(base_val, span);
                let idx_id = self.arena.add(idx_val, span);
                let mid = self.arena.intern("get");
                self.dispatch_class_method(
                    Some(base),
                    crate::typecheck::BuiltinClassTag::Indexable,
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
    /// on `Value::Object`. Zero-arity variants like `Option.None` are resolved
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
    ) -> Result<Value> {
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
                Value::Object(obj) => Ok(obj
                    .get(field)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| typechecked!(".field", "field exists"))),
                // JSON field access returns Json (null for missing)
                Value::Json(j) => {
                    let fs = self.arena.strings.get(*field).unwrap_or_default();
                    Ok(Value::Json(
                        j.get(fs).cloned().unwrap_or(serde_json::Value::Null),
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
            // Option.Some(v) -> try field on v; Some(field) if exists, None if not
            Value::Tagged(ty_expr, 1, payload)
                if self
                    .type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
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
        val: &Value,
        field: &StringId,
    ) -> Value {
        match val {
            Value::Object(obj) => obj
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
        val: &Value,
        field: &StringId,
        span: Span,
    ) -> Result<Value> {
        match val {
            Value::Object(obj) => obj
                .get(field)
                .and_then(|id| self.arena.get(*id).cloned())
                .map(|v| {
                    let id = self.arena.add(v, span);
                    self.make_some(id)
                })
                .map_or_else(|| Ok(self.make_none()), Ok),
            _ => typechecked!("?.field", "Object"),
        }
    }
}
