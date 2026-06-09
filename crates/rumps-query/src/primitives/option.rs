use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, TypeId, ValueId};

pub(crate) struct Opt;

impl Prim for Opt {}

impl Opt {
    /// `forall T. (Option[T], T) -> T`
    ///
    /// Returns the inner value if `Some`, otherwise returns `default`.
    pub(crate) fn unwrap_or<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let val = ctx.arena.value(a).cloned().ok_or_else(|| {
                ctx.runtime_error("Option.unwrap-or: invalid value")
            })?;
            let ty = ctx.runtime_types.meta_type_id(&val);

            match (ty, val.payload) {
                (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                    // Option.Some(v) - return the inner value
                    vals.first().copied().ok_or_else(|| {
                        ctx.runtime_error(
                            "Option.unwrap-or: Some has no payload",
                        )
                    })
                }
                (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => {
                    // Option.None - return the default
                    Ok(b)
                }
                _ => typechecked!("Option.unwrap-or", "Option"),
            }
        })
    }

    /// `forall T. (Option[Option[T]]) -> Option[T]`
    ///
    /// Flattens a nested `Option`. Returns `Some(v)` if input is `Some(Some(v))`,
    /// otherwise returns `None`.
    pub(crate) fn flatten<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let val =
                ctx.arena.value(a).cloned().unwrap_or_else(|| {
                    typechecked!("Option.flatten", "valid arg")
                });
            let ty = ctx.runtime_types.meta_type_id(&val);

            match (ty, val.payload) {
                (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                    // Option.Some(inner) - return the inner Option
                    Ok(*vals.first().unwrap_or_else(|| {
                        typechecked!("Option.flatten", "Some payload")
                    }))
                }
                (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => {
                    Ok(ctx.option_none())
                }
                _ => typechecked!("Option.flatten", "Option"),
            }
        })
    }

    /// `forall T E. (E, Option[T]) -> Result[T, E]`
    ///
    /// Converts an `Option` to a `Result`, using the provided error if `None`.
    pub(crate) fn note<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let val =
                ctx.arena.value(b).cloned().unwrap_or_else(|| {
                    typechecked!("Option.note", "valid arg")
                });
            let ty = ctx.runtime_types.meta_type_id(&val);

            match (ty, val.payload) {
                (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                    // Option.Some(v) -> Result.Ok(v)
                    let inner = *vals.first().unwrap_or_else(|| {
                        typechecked!("Option.note", "Some payload")
                    });
                    Ok(ctx.result_ok(inner))
                }
                (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => {
                    // Option.None -> Result.Err(e)
                    Ok(ctx.result_err(a))
                }
                _ => typechecked!("Option.note", "Option"),
            }
        })
    }
}
