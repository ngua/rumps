use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::{Math, Prim};
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

/// Primitives for the `Math.Trig` submodule.
pub(crate) struct Trig;

impl Prim for Trig {}

impl Trig {
    /// `(Float) -> Float`
    ///
    /// Returns the sine of x (x in radians).
    pub(crate) fn sin<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.sin())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the cosine of x (x in radians).
    pub(crate) fn cos<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.cos())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the tangent of x (x in radians).
    pub(crate) fn tan<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.tan())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arcsine of x (result in radians).
    pub(crate) fn asin<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.asin())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arccosine of x (result in radians).
    pub(crate) fn acos<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.acos())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arctangent of x (result in radians).
    pub(crate) fn atan<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.atan())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float, Float) -> Float`
    ///
    /// Returns the arctangent of `y/x` (result in radians), using signs to
    /// determine the correct quadrant.
    pub(crate) fn atan2<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Math::to_float(ctx, args[0]);
            let b = Math::to_float(ctx, args[1]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.atan2(b))),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }
}
