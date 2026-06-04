use std::sync::Arc;

use ordered_float::OrderedFloat;
use rand::seq::SliceRandom;
use rand::Rng;
use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Random;

impl Prim for Random {}

impl Random {
    /// `() -> Float`
    ///
    /// Returns a random float in the range `[0, 1)`.
    pub(crate) fn random<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n: f64 = rand::thread_rng().gen();
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n)),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Float, Float) -> Float`
    ///
    /// Returns a random float in the range `[min, max)`.
    pub(crate) fn range<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let min = Self::to_float(ctx, args[0]);
            let max = Self::to_float(ctx, args[1]);

            let n: f64 = rand::thread_rng().gen_range(min..max);
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(n)),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Int, Int) -> Int`
    ///
    /// Returns a random integer in the range `[min, max]` (inclusive).
    pub(crate) fn int<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let min = Self::to_int(ctx, args[0]);
            let max = Self::to_int(ctx, args[1]);

            let n: i64 = rand::thread_rng().gen_range(min..=max);
            Ok(ctx.arena.add_typed(
                Payload::Int(n),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `() -> Bool`
    ///
    /// Returns a random boolean.
    pub(crate) fn bool<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let b: bool = rand::thread_rng().gen();
            Ok(ctx.arena.add_typed(
                Payload::Bool(b),
                ctx.runtime_types.meta_bool(),
                ctx.span,
            ))
        })
    }

    /// `forall T. (Array[T]) -> Option[T]`
    ///
    /// Picks a random element from the array. Returns `Option.None` if empty.
    pub(crate) fn choice<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let elems = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Random.choice", "Array"));

            let result = elems
                .choose(&mut rand::thread_rng())
                .copied()
                .map(|v| ctx.option_some(v))
                .unwrap_or_else(|| ctx.option_none());

            Ok(result)
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements in random order.
    pub(crate) fn shuffle<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let mut e = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("Random.shuffle", "Array"));

            e.shuffle(&mut rand::thread_rng());
            Ok(ctx.add(Payload::Array(Arc::new(e))))
        })
    }

    /// `forall T. (Array[T], Int) -> Result[Array[T], String]`
    ///
    /// Picks `n` random elements without replacement.
    /// Returns `Result.Err` if `n > Iter.length(arr)`.
    pub(crate) fn sample<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let elems = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Random.sample", "Array"));

            let n = Self::to_int(ctx, args[1]) as usize;

            if n > elems.len() {
                let msg_str = format!(
                    "Random.sample: n ({n}) exceeds array length ({})",
                    elems.len()
                );
                let msg = ctx.arena.intern(&msg_str);
                let msg_val = ctx.arena.add_typed(
                    Payload::String(msg),
                    ctx.runtime_types.meta_string(),
                    ctx.span,
                );
                Ok(ctx.result_err(msg_val))
            } else {
                let sampled: SmallVec<[ValueId; 4]> = elems
                    .choose_multiple(&mut rand::thread_rng(), n)
                    .copied()
                    .collect();
                let arr = ctx.add(Payload::Array(Arc::new(sampled)));
                Ok(ctx.result_ok(arr))
            }
        })
    }

    /// `() -> String`
    ///
    /// Generates a random UUID v4 string.
    pub(crate) fn uuid<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let id = uuid::Uuid::new_v4().to_string();
            let sid = ctx.arena.intern(&id);
            Ok(ctx.arena.add_typed(
                Payload::String(sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }
}
