mod trig;

use ordered_float::OrderedFloat;
use smallvec::SmallVec;
pub(crate) use trig::Trig;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, ValueId};
use crate::Result;

pub(crate) struct Math;

impl Body for Math {}

impl Math {
    /// `forall T: Numeric. (T) -> T`
    ///
    /// Returns the absolute value.
    pub(crate) fn abs(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let result = match ctx.vals().payload(args[0])? {
            Payload::Int(n) => Payload::Int(n.abs()),
            Payload::Word(n) => Payload::Word(*n),
            Payload::Float(f) => Payload::Float(OrderedFloat(f.0.abs())),
            _ => typechecked!("Math.abs", "Numeric"),
        };
        Ok(ctx.vals().add(result))
    }

    /// `forall T: Numeric. (T, T) -> T`
    ///
    /// Returns the minimum of two numbers.
    pub(crate) fn min(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let l = ctx.vals().payload(args[0])?.clone();
        let r = ctx.vals().payload(args[1])?.clone();
        let result = match (l, r) {
            (Payload::Int(x), Payload::Int(y)) => Payload::Int(x.min(y)),
            (Payload::Word(x), Payload::Word(y)) => Payload::Word(x.min(y)),
            (Payload::Float(x), Payload::Float(y)) => {
                Payload::Float(OrderedFloat(x.0.min(y.0)))
            }
            _ => typechecked!("Math.min", "same Numeric type"),
        };
        Ok(ctx.vals().add(result))
    }

    /// `forall T: Numeric. (T, T) -> T`
    ///
    /// Returns the maximum of two numbers.
    pub(crate) fn max(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let l = ctx.vals().payload(args[0])?.clone();
        let r = ctx.vals().payload(args[1])?.clone();
        let result = match (l, r) {
            (Payload::Int(x), Payload::Int(y)) => Payload::Int(x.max(y)),
            (Payload::Word(x), Payload::Word(y)) => Payload::Word(x.max(y)),
            (Payload::Float(x), Payload::Float(y)) => {
                Payload::Float(OrderedFloat(x.0.max(y.0)))
            }
            _ => typechecked!("Math.max", "same Numeric type"),
        };
        Ok(ctx.vals().add(result))
    }

    /// `(Float) -> Int`
    ///
    /// Returns the largest integer less than or equal to x.
    pub(crate) fn floor(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Self::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Int(a.floor() as i64)))
    }

    /// `(Float) -> Int`
    ///
    /// Returns the smallest integer greater than or equal to x.
    pub(crate) fn ceil(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Self::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Int(a.ceil() as i64)))
    }

    /// `(Float) -> Int`
    ///
    /// Rounds to the nearest integer (ties round away from zero).
    pub(crate) fn round(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Self::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Int(a.round() as i64)))
    }

    /// `(Float) -> Float`
    ///
    /// Returns the square root. Returns NaN for negative inputs.
    pub(crate) fn sqrt(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Self::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.sqrt()))))
    }

    /// `(Float) -> Float`
    ///
    /// Returns the natural logarithm. Returns NaN for non-positive inputs.
    pub(crate) fn log(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = Self::to_float(ctx, args[0])?;
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(a.ln()))))
    }
}
