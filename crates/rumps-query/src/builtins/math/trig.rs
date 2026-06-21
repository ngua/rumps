use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::{Body, Math};
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, ValueId};
use crate::Result;

/// Builtins for the `Math.Trig` submodule.
pub(crate) struct Trig;

impl Body for Trig {}

impl Trig {
    /// `(Float) -> Float`
    ///
    /// Returns the sine of x (x in radians).
    pub(crate) fn sin(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Math::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.sin()))))
    }

    /// `(Float) -> Float`
    ///
    /// Returns the cosine of x (x in radians).
    pub(crate) fn cos(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Math::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.cos()))))
    }

    /// `(Float) -> Float`
    ///
    /// Returns the tangent of x (x in radians).
    pub(crate) fn tan(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Math::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.tan()))))
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arcsine of x (result in radians).
    pub(crate) fn asin(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Math::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.asin()))))
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arccosine of x (result in radians).
    pub(crate) fn acos(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Math::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.acos()))))
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arctangent of x (result in radians).
    pub(crate) fn atan(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Math::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.atan()))))
    }

    /// `(Float, Float) -> Float`
    ///
    /// Returns the arctangent of `y/x` (result in radians), using signs to
    /// determine the correct quadrant.
    pub(crate) fn atan2(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Math::to_float(ctx, args[0])?;
        let b = Math::to_float(ctx, args[1])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.atan2(b)))))
    }
}
