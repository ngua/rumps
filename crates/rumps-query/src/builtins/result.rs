use futures::future::BoxFuture;
use smallvec::{smallvec, SmallVec};

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

pub(crate) struct Res;

impl Body for Res {}

impl Res {
    /// `forall T, U, V. (Result[T, U], (U) -> V) -> Result[T, V]`
    ///
    /// Maps the error if the input is `Err`, otherwise keeps the `Ok` value.
    /// Returns a `BoxFuture` because invoking `f` may run `async` user code.
    pub(crate) fn map_err<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let f = args[1];
            let err_ty = ctx
                .vals()
                .callable_ret_ty(f)
                .unwrap_or_else(|| typechecked!("Result.map-err", "callable"));
            let val = ctx.vals().value(a)?.clone();
            let ty = ctx.vals().value_variant_base_type(&val);

            match (ty, val.payload) {
                (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                    let ok = *vals.first().unwrap_or_else(|| {
                        typechecked!("Result.map-err", "Ok payload")
                    });
                    let ts = ctx
                        .vals()
                        .variant_args(a, TypeId::RESULT)
                        .unwrap_or_else(|| {
                            typechecked!("Result.map-err", "Result metadata")
                        });
                    let ok_ty = *ts.first().unwrap_or_else(|| {
                        typechecked!("Result.map-err", "Result metadata")
                    });
                    Ok(ctx.vals().add_variant(
                        Payload::ok(ok),
                        TypeId::RESULT,
                        smallvec![ok_ty, err_ty],
                    ))
                }
                (Some(TypeId::RESULT), Payload::Variant { tag: 1, vals }) => {
                    let err = *vals.first().unwrap_or_else(|| {
                        typechecked!("Result.map-err", "Err payload")
                    });
                    let ts = ctx
                        .vals()
                        .variant_args(a, TypeId::RESULT)
                        .unwrap_or_else(|| {
                            typechecked!("Result.map-err", "Result metadata")
                        });
                    let ok_ty = *ts.first().unwrap_or_else(|| {
                        typechecked!("Result.map-err", "Result metadata")
                    });
                    let mapped = ctx.invoke(f, smallvec![err]).await?;
                    let err_ty =
                        ctx.vals().meta(mapped).map(|m| m.ty).unwrap_or_else(
                            || typechecked!("Result.map-err", "result meta"),
                        );
                    Ok(ctx.vals().add_variant(
                        Payload::err(mapped),
                        TypeId::RESULT,
                        smallvec![ok_ty, err_ty],
                    ))
                }
                _ => typechecked!("Result.map-err", "Result"),
            }
        })
    }

    /// `forall T E. (Result[T, E], T) -> T`
    ///
    /// Returns the inner value if `Ok`, otherwise returns `default`.
    pub(crate) fn unwrap_or(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let val = ctx.vals().value(a)?.clone();
        let ty = ctx.vals().value_variant_base_type(&val);

        match (ty, val.payload) {
            (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                vals.first().copied().ok_or_else(|| {
                    ctx.runtime_error("Result.unwrap-or: Ok has no payload")
                })
            }
            (Some(TypeId::RESULT), Payload::Variant { tag: 1, .. }) => Ok(b),
            _ => typechecked!("Result.unwrap-or", "Result"),
        }
    }

    /// `forall T E. (Result[Result[T, E], E]) -> Result[T, E]`
    ///
    /// Flattens a nested `Result`. Returns `Ok(v)` if input is `Ok(Ok(v))`,
    /// `Err(e)` if input is `Ok(Err(e))` or `Err(e)`.
    pub(crate) fn flatten(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let val = ctx.vals().value(a)?.clone();
        let ty = ctx.vals().value_variant_base_type(&val);

        match (ty, val.payload) {
            (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                Ok(*vals.first().unwrap_or_else(|| {
                    typechecked!("Result.flatten", "Ok payload")
                }))
            }
            (Some(TypeId::RESULT), Payload::Variant { tag: 1, .. }) => Ok(a),
            _ => typechecked!("Result.flatten", "Result"),
        }
    }

    /// `forall T E. (Result[T, E]) -> Option[T]`
    ///
    /// Converts a `Result` to an `Option`, discarding the error if `Err`.
    pub(crate) fn hush(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let val = ctx.vals().value(a)?.clone();
        let ty = ctx.vals().value_variant_base_type(&val);

        match (ty, val.payload) {
            (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                let inner = *vals.first().unwrap_or_else(|| {
                    typechecked!("Result.hush", "Ok payload")
                });
                Ok(ctx.vals().option_some(inner))
            }
            (Some(TypeId::RESULT), Payload::Variant { tag: 1, .. }) => {
                Ok(ctx.vals().option_none())
            }
            _ => typechecked!("Result.hush", "Result"),
        }
    }
}
