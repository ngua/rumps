//! Variant construction and Option/Result helpers.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::class::ClassCtx;
use super::Interpreter;
use crate::ast::ExprId;
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::typecheck::RuntimeTyId;
use crate::value::{Payload, Value, ValueId};
use crate::{ClassId, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Create an `Option.None` value.
    pub(super) fn make_none(&self) -> Payload {
        Payload::none()
    }

    /// Create an `Option.Some(v)` value.
    pub(super) fn make_some(&self, inner: ValueId) -> Payload {
        Payload::some(inner)
    }

    /// Create an `Option.None` with the same type as another `Option`.
    pub(super) fn make_none_like(&self) -> Payload {
        Payload::none()
    }

    /// Create an `Option.None` typed as `Option[Scalar]`.
    pub(super) fn make_none_scalar(&self) -> Payload {
        Payload::none()
    }

    /// Create an `Option.Some(v)` typed as `Option[Scalar]`.
    pub(super) fn make_some_scalar(&self, inner: ValueId) -> Payload {
        Payload::some(inner)
    }

    /// Create an `Option.None` typed as `Option[Storable]`.
    pub(super) fn make_none_storable(&self) -> Payload {
        Payload::none()
    }

    /// Create an `Option.Some(v)` typed as `Option[Storable]`.
    pub(super) fn make_some_storable(&self, inner: ValueId) -> Payload {
        Payload::some(inner)
    }

    /// Create a `Result.Ok(v)` value.
    pub(super) fn make_result_ok(&mut self, v: Payload, span: Span) -> Payload {
        let val_id = self.add_payload(v, span);
        Payload::ok(val_id)
    }

    /// Create a `Result.Ok(v)` value, preserving existing runtime metadata.
    pub(super) fn make_result_ok_value(
        &mut self,
        v: Value,
        span: Span,
    ) -> Payload {
        let val_id = self.add_value(v, span);
        Payload::ok(val_id)
    }

    /// Create a `Result.Ok(v)` value with explicit metadata for `v`.
    pub(super) fn make_result_ok_typed(
        &mut self,
        v: Payload,
        ty: RuntimeTyId,
        span: Span,
    ) -> Payload {
        let meta = self.checked.types.meta(ty);
        let value = self.value_from_meta(v, meta);
        self.make_result_ok_value(value, span)
    }

    /// Create a `Result.Err(msg)` value.
    pub(super) fn make_result_err(&mut self, msg: &str, span: Span) -> Payload {
        let msg_id = self.arena.intern(msg);
        let msg_val = self.add_val(
            Payload::String(msg_id),
            self.checked.types.meta_string(),
            span,
        );
        Payload::err(msg_val)
    }

    /// Evaluate a variant constructor: `Type.Variant(args...)`.
    #[async_recursion]
    pub(super) async fn variant(
        &mut self,
        expr_id: ExprId,
        ty_name: &QualifiedName,
        var_name: StringId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Payload> {
        // Typechecker validates type names
        let type_id = self
            .registry
            .lookup(ty_name)
            .unwrap_or_else(|| typechecked!("variant", "known type"));
        let meta = self.expr_meta(expr_id);
        let checked_type_id = self
            .checked
            .types
            .to_type_id(meta.repr)
            .or_else(|| self.checked.types.to_type_id(meta.ty))
            .unwrap_or_else(|| {
                typechecked!("variant", "checked type metadata")
            });
        if checked_type_id != type_id {
            typechecked!("variant", "checked type metadata")
        }

        // Typechecker validates variant names
        let var_def = self
            .registry
            .lookup_variant(type_id, var_name)
            .unwrap_or_else(|| typechecked!("variant", "known variant"));

        // Typechecker validates arity
        if var_def.arity as usize != args.len() {
            typechecked!("variant arity", "correct");
        }

        let idx = var_def.idx;
        let payloads = self.eval_variant_args(args, span).await?;

        Ok(Payload::Variant {
            tag: idx,
            vals: payloads,
        })
    }

    /// Evaluate variant arguments and return their `ValueId`s.
    #[async_recursion]
    pub(super) async fn eval_variant_args(
        &mut self,
        args: &[ExprId],
        span: Span,
    ) -> Result<SmallVec<[ValueId; 4]>> {
        match args.split_first() {
            None => Ok(SmallVec::new()),
            Some((head, tail)) => {
                let val = self.eval(*head).await?;
                let val_id = self.add_value(val, span);
                let mut rest = self.eval_variant_args(tail, span).await?;
                let mut vals = SmallVec::new();
                vals.push(val_id);
                vals.append(&mut rest);
                Ok(vals)
            }
        }
    }

    /// Evaluate a default-value expression: `_`.
    ///
    /// Dispatches with the inferred type to produce the appropriate default value.
    pub(super) fn default_value(
        &mut self,
        id: ExprId,
        span: Span,
    ) -> Result<Payload> {
        let ty_id = self.checked.expr(id).ty;
        let ty = self.checked.types.get(ty_id).clone();

        let mid = self.arena.intern("default");
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span,
        };
        self.class_methods.dispatch_nullary(
            ClassId::DEFAULT,
            mid,
            &mut ctx,
            &ty,
        )
    }
}
