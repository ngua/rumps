use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, TypeId, ValueId};

pub(crate) struct Res;

impl Prim for Res {}

impl Res {
    /// `forall T E. (Result[T, E], T) -> T`
    ///
    /// Returns the inner value if `Ok`, otherwise returns `default`.
    pub(crate) fn unwrap_or<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let res = ctx.arena.value(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Result.unwrap-or: invalid value")
            })?;
            let res_ty = ctx.runtime_types.meta_type_id(&res);

            match (res_ty, res.payload) {
                (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                    // Result.Ok(v) - return the inner value
                    vals.first().copied().ok_or_else(|| {
                        ctx.runtime_error("Result.unwrap-or: Ok has no payload")
                    })
                }
                (Some(TypeId::RESULT), Payload::Variant { tag: 1, .. }) => {
                    // Result.Err - return the default
                    Ok(args[1])
                }
                _ => typechecked!("Result.unwrap-or", "Result"),
            }
        })
    }

    /// `forall T E. (Result[Result[T, E], E]) -> Result[T, E]`
    ///
    /// Flattens a nested `Result`. Returns `Ok(v)` if input is `Ok(Ok(v))`,
    /// `Err(e)` if input is `Ok(Err(e))` or `Err(e)`.
    pub(crate) fn flatten<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let res =
                ctx.arena.value(args[0]).cloned().unwrap_or_else(|| {
                    typechecked!("Result.flatten", "valid arg")
                });
            let res_ty = ctx.runtime_types.meta_type_id(&res);

            match (res_ty, res.payload) {
                (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                    // Result.Ok(inner) - return the inner Result
                    Ok(*vals.first().unwrap_or_else(|| {
                        typechecked!("Result.flatten", "Ok payload")
                    }))
                }
                (Some(TypeId::RESULT), Payload::Variant { tag: 1, .. }) => {
                    Ok(args[0])
                }
                _ => typechecked!("Result.flatten", "Result"),
            }
        })
    }

    /// `forall T E. (Result[T, E]) -> Option[T]`
    ///
    /// Converts a `Result` to an `Option`, discarding the error if `Err`.
    pub(crate) fn hush<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let res =
                ctx.arena.value(args[0]).cloned().unwrap_or_else(|| {
                    typechecked!("Result.hush", "valid arg")
                });
            let res_ty = ctx.runtime_types.meta_type_id(&res);

            match (res_ty, res.payload) {
                (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                    // Result.Ok(v) -> Option.Some(v)
                    let inner = *vals.first().unwrap_or_else(|| {
                        typechecked!("Result.hush", "Ok payload")
                    });
                    Ok(ctx.option_some(inner))
                }
                (Some(TypeId::RESULT), Payload::Variant { tag: 1, .. }) => {
                    // Result.Err(_) -> Option.None
                    Ok(ctx.option_none())
                }
                _ => typechecked!("Result.hush", "Result"),
            }
        })
    }
}
