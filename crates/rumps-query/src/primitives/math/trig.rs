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
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n.sin())),
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
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n.cos())),
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
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n.tan())),
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
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n.asin())),
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
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n.acos())),
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
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n.atan())),
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
            let y = Math::to_float(ctx, args[0]);
            let x = Math::to_float(ctx, args[1]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(y.atan2(x))),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }
}
