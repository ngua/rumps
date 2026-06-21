use smallvec::SmallVec;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

pub(crate) struct Opt;

impl Body for Opt {}

impl Opt {
    /// `forall T. (Option[T], T) -> T`
    ///
    /// Returns the inner value if `Some`, otherwise returns `default`.
    pub(crate) fn unwrap_or(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let val = ctx.vals().value(a)?.clone();
        let ty = ctx.vals().value_variant_base_type(&val);

        match (ty, val.payload) {
            (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                vals.first().copied().ok_or_else(|| {
                    ctx.runtime_error("Option.unwrap-or: Some has no payload")
                })
            }
            (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => Ok(b),
            _ => typechecked!("Option.unwrap-or", "Option"),
        }
    }

    /// `forall T. (Option[Option[T]]) -> Option[T]`
    ///
    /// Flattens a nested `Option`. Returns `Some(v)` if input is `Some(Some(v))`,
    /// otherwise returns `None`.
    pub(crate) fn flatten(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let val = ctx.vals().value(a)?.clone();
        let ty = ctx.vals().value_variant_base_type(&val);

        match (ty, val.payload) {
            (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                Ok(*vals.first().unwrap_or_else(|| {
                    typechecked!("Option.flatten", "Some payload")
                }))
            }
            (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => {
                Ok(ctx.vals().option_none())
            }
            _ => typechecked!("Option.flatten", "Option"),
        }
    }

    /// `forall T E. (E, Option[T]) -> Result[T, E]`
    ///
    /// Converts an `Option` to a `Result`, using the provided error if `None`.
    pub(crate) fn note(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let val = ctx.vals().value(b)?.clone();
        let ty = ctx.vals().value_variant_base_type(&val);

        match (ty, val.payload) {
            (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                let inner = *vals.first().unwrap_or_else(|| {
                    typechecked!("Option.note", "Some payload")
                });
                Ok(ctx.vals().result_ok(inner))
            }
            (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => {
                Ok(ctx.vals().result_err(a))
            }
            _ => typechecked!("Option.note", "Option"),
        }
    }
}
