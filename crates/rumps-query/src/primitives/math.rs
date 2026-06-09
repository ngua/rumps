mod trig;

use ordered_float::OrderedFloat;
use smallvec::SmallVec;
pub(crate) use trig::Trig;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Math;

impl Prim for Math {}

impl Math {
    /// `forall T: Numeric. (T) -> T`
    ///
    /// Returns the absolute value.
    pub(crate) fn abs<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let v = ctx
                .arena
                .payload(a)
                .ok_or_else(|| ctx.runtime_error("Math.abs: invalid value"))?;

            let result = match v {
                Payload::Int(n) => Payload::Int(n.abs()),
                Payload::Word(n) => Payload::Word(*n), // Word is unsigned; abs is identity
                Payload::Float(f) => Payload::Float(OrderedFloat(f.0.abs())),
                _ => typechecked!("Math.abs", "Numeric"),
            };

            Ok(ctx.add(result))
        })
    }

    /// `forall T: Numeric. (T, T) -> T`
    ///
    /// Returns the minimum of two numbers.
    pub(crate) fn min<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let result = match (ctx.arena.payload(a), ctx.arena.payload(b)) {
                (Some(Payload::Int(x)), Some(Payload::Int(y))) => {
                    Payload::Int((*x).min(*y))
                }
                (Some(Payload::Word(x)), Some(Payload::Word(y))) => {
                    Payload::Word((*x).min(*y))
                }
                (Some(Payload::Float(x)), Some(Payload::Float(y))) => {
                    Payload::Float(OrderedFloat(x.0.min(y.0)))
                }
                _ => typechecked!("Math.min", "same Numeric type"),
            };

            Ok(ctx.add(result))
        })
    }

    /// `forall T: Numeric. (T, T) -> T`
    ///
    /// Returns the maximum of two numbers.
    pub(crate) fn max<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let result = match (ctx.arena.payload(a), ctx.arena.payload(b)) {
                (Some(Payload::Int(x)), Some(Payload::Int(y))) => {
                    Payload::Int((*x).max(*y))
                }
                (Some(Payload::Word(x)), Some(Payload::Word(y))) => {
                    Payload::Word((*x).max(*y))
                }
                (Some(Payload::Float(x)), Some(Payload::Float(y))) => {
                    Payload::Float(OrderedFloat(x.0.max(y.0)))
                }
                _ => typechecked!("Math.max", "same Numeric type"),
            };

            Ok(ctx.add(result))
        })
    }

    /// `(Float) -> Int`
    ///
    /// Returns the largest integer less than or equal to x.
    pub(crate) fn floor<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Int(a.floor() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Int`
    ///
    /// Returns the smallest integer greater than or equal to x.
    pub(crate) fn ceil<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Int(a.ceil() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Int`
    ///
    /// Rounds to the nearest integer (ties round away from zero).
    pub(crate) fn round<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Int(a.round() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the square root. Returns NaN for negative inputs.
    pub(crate) fn sqrt<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.sqrt())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the natural logarithm. Returns NaN for non-positive inputs.
    pub(crate) fn log<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(a.ln())),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }
}
