//! Variant construction and Option/Result helpers.

use async_recursion::async_recursion;
use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::Interpreter;
use crate::ast::ExprId;
use crate::io::IoContext;
use crate::typecheck::Ty;
use crate::value::{TypeExprId, TypeId, Value, ValueId};
use crate::{Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Create an `Option.None` value with unknown type parameter.
    pub(super) fn make_none(&mut self) -> Value {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![unknown]);
        Value::none(opt_ty)
    }

    /// Create an `Option.Some(v)` value, inferring the type from the inner value.
    pub(super) fn make_some(&mut self, inner: ValueId) -> Value {
        let inner_val = self.arena.get(inner).cloned().unwrap_or(Value::Int(0));
        let inner_ty = self.value_type_expr(&inner_val);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![inner_ty]);
        Value::some(opt_ty, inner)
    }

    /// Create an `Option.None` with the same type parameter as another Option.
    pub(super) fn make_none_like(&mut self, other_opt_ty: TypeExprId) -> Value {
        Value::none(other_opt_ty)
    }

    /// Create an `Option.None` typed as `Option[Scalar]`.
    pub(super) fn make_none_scalar(&mut self) -> Value {
        let scalar_ty = self.type_exprs.named(TypeId::SCALAR);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![scalar_ty]);
        Value::none(opt_ty)
    }

    /// Create an `Option.Some(v)` typed as `Option[Scalar]`.
    pub(super) fn make_some_scalar(&mut self, inner: ValueId) -> Value {
        let scalar_ty = self.type_exprs.named(TypeId::SCALAR);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![scalar_ty]);
        Value::some(opt_ty, inner)
    }

    /// Create an `Option.None` typed as `Option[Storable]`.
    pub(super) fn make_none_storable(&mut self) -> Value {
        let storable_ty = self.type_exprs.named(TypeId::STORABLE);
        let opt_ty =
            self.type_exprs.app(TypeId::OPTION, smallvec![storable_ty]);
        Value::none(opt_ty)
    }

    /// Create an `Option.Some(v)` typed as `Option[Storable]`.
    pub(super) fn make_some_storable(&mut self, inner: ValueId) -> Value {
        let storable_ty = self.type_exprs.named(TypeId::STORABLE);
        let opt_ty =
            self.type_exprs.app(TypeId::OPTION, smallvec![storable_ty]);
        Value::some(opt_ty, inner)
    }

    /// Create a `Result.Ok(v)` value.
    pub(super) fn make_result_ok(&mut self, v: Value, span: Span) -> Value {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let val_ty = self.value_type_expr(&v);
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![val_ty, unknown]);
        let val_id = self.arena.add(v, span);
        Value::ok(res_ty, val_id)
    }

    /// Create a `Result.Err(msg)` value.
    pub(super) fn make_result_err(&mut self, msg: &str, span: Span) -> Value {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let str_ty = self.type_exprs.named(TypeId::STRING);
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![unknown, str_ty]);
        let msg_id = self.arena.intern(msg);
        let msg_val = self.arena.add(Value::String(msg_id), span);
        Value::err(res_ty, msg_val)
    }

    /// Evaluate a variant constructor: `Type.Variant(args...)`.
    #[async_recursion]
    pub(super) async fn variant(
        &mut self,
        ty_name: &str,
        var_name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        let ty_id = self.arena.intern(ty_name);
        let var_id = self.arena.intern(var_name);

        // Typechecker validates type names
        let type_id = self
            .registry
            .lookup(ty_id)
            .unwrap_or_else(|| typechecked!("variant", "known type"));

        // Typechecker validates variant names
        let var_def = self
            .registry
            .lookup_variant(type_id, var_id)
            .unwrap_or_else(|| typechecked!("variant", "known variant"));

        // Typechecker validates arity
        if var_def.arity as usize != args.len() {
            typechecked!("variant arity", "correct");
        }

        let idx = var_def.idx;

        // Evaluate arguments and collect their values and types
        let (payloads, payload_types) =
            self.eval_variant_args(args, span).await?;

        // Build the type expression with inferred type parameters
        let ty_expr =
            self.build_variant_type_expr(type_id, idx, &payload_types);

        Ok(Value::Tagged(ty_expr, idx, payloads))
    }

    /// Evaluate variant arguments and return (values, types).
    #[async_recursion]
    pub(super) async fn eval_variant_args(
        &mut self,
        args: &[ExprId],
        span: Span,
    ) -> Result<(SmallVec<[ValueId; 4]>, SmallVec<[TypeExprId; 4]>)> {
        match args.split_first() {
            None => Ok((SmallVec::new(), SmallVec::new())),
            Some((head, tail)) => {
                let val = self.eval(*head).await?;
                let val_ty = self.value_type_expr(&val);
                let val_id = self.arena.add(val, span);
                let (mut rest_vals, mut rest_tys) =
                    self.eval_variant_args(tail, span).await?;
                // Prepend since we're building from head
                let mut vals = smallvec![val_id];
                vals.append(&mut rest_vals);
                let mut tys = smallvec![val_ty];
                tys.append(&mut rest_tys);
                Ok((vals, tys))
            }
        }
    }

    /// Build a `TypeExprId` for a variant, inferring type params from payloads.
    ///
    /// For `Option.Some(42)` -> `Option[Int]`
    /// For `Option.None` -> `Option[Unknown]`
    /// For `Result.Ok(42)` -> `Result[Int, Unknown]`
    /// For `Result.Err("x")` -> `Result[Unknown, String]`
    pub(super) fn build_variant_type_expr(
        &mut self,
        type_id: TypeId,
        var_idx: u8,
        payload_types: &[TypeExprId],
    ) -> TypeExprId {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);

        // Special handling for built-in types Option and Result
        if type_id == TypeId::OPTION {
            // Option[T]: None has no payload, Some has T
            let t = payload_types.first().copied().unwrap_or(unknown);
            self.type_exprs.app(TypeId::OPTION, smallvec![t])
        } else if type_id == TypeId::RESULT {
            // Result[T, E]: Ok has T, Err has E
            let (t, e) = if var_idx == 0 {
                // Ok(v) -> Result[type_of(v), Unknown]
                (payload_types.first().copied().unwrap_or(unknown), unknown)
            } else {
                // Err(e) -> Result[Unknown, type_of(e)]
                (unknown, payload_types.first().copied().unwrap_or(unknown))
            };
            self.type_exprs.app(TypeId::RESULT, smallvec![t, e])
        } else {
            // For other types, use Unknown for all type params
            // (Future: read type_params from TypeDef and infer properly)
            self.type_exprs.named(type_id)
        }
    }

    /// Evaluate a `Mempty` expression (monoid identity: `_`).
    ///
    /// Looks up the inferred type from the type checker and produces the
    /// appropriate empty value:
    /// - `String` -> `""`
    /// - `Array[T]` -> `[]`
    /// - `Map[K, V]` -> `{}`
    /// - `Option[T]` -> `Option.None`
    pub(super) fn mempty(
        &mut self,
        id: crate::ast::ExprId,
    ) -> crate::Result<Value> {
        let ty = self
            .mempty_types
            .get(&id)
            .cloned()
            .unwrap_or_else(|| typechecked!("mempty", "resolved type"));

        match ty {
            Ty::String => Ok(Value::String(self.arena.intern(""))),
            Ty::Array(_) => {
                let elem_ty = self.type_exprs.named(TypeId::UNKNOWN);
                Ok(Value::Array(elem_ty, SmallVec::new()))
            }
            Ty::Map(_, _) => {
                let k_ty = self.type_exprs.named(TypeId::UNKNOWN);
                let v_ty = self.type_exprs.named(TypeId::UNKNOWN);
                Ok(Value::Map(k_ty, v_ty, IndexMap::new()))
            }
            Ty::Option(_) => Ok(self.make_none()),
            _ => typechecked!("mempty", "monoid type"),
        }
    }
}
