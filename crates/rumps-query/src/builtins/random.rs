use std::sync::Arc;

use ordered_float::OrderedFloat;
use rand::seq::SliceRandom;
use rand::Rng;
use smallvec::SmallVec;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, ValueId};
use crate::Result;

pub(crate) struct Random;

impl Body for Random {}

impl Random {
    /// `() -> Float`
    ///
    /// Returns a random float in the range `[0, 1)`.
    pub(crate) fn random(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n: f64 = rand::thread_rng().gen();
        Ok(ctx.vals().add(Payload::Float(OrderedFloat(n))))
    }

    /// `(Float, Float) -> Float`
    ///
    /// Returns a random float in the range `[min, max)`.
    pub(crate) fn range(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = ctx.vals().float_payload(args[0], "Random.range")?;
        let b = ctx.vals().float_payload(args[1], "Random.range")?;
        let n: f64 = rand::thread_rng().gen_range(a..b);

        Ok(ctx.vals().add(Payload::Float(OrderedFloat(n))))
    }

    /// `(Int, Int) -> Int`
    ///
    /// Returns a random integer in the range `[min, max]` (inclusive).
    pub(crate) fn int(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = ctx.vals().int_payload(args[0], "Random.int")?;
        let b = ctx.vals().int_payload(args[1], "Random.int")?;
        let n: i64 = rand::thread_rng().gen_range(a..=b);

        Ok(ctx.vals().add(Payload::Int(n)))
    }

    /// `() -> Bool`
    ///
    /// Returns a random boolean.
    pub(crate) fn bool(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let b: bool = rand::thread_rng().gen();
        Ok(ctx.vals().add(Payload::Bool(b)))
    }

    /// `forall T. (Array[T]) -> Option[T]`
    ///
    /// Picks a random element from the array. Returns `Option.None` if empty.
    pub(crate) fn choice(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let elems: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(a, "Random.choice")?
            .iter()
            .copied()
            .collect();
        let result = elems.choose(&mut rand::thread_rng()).copied();

        Ok(match result {
            Some(v) => ctx.vals().option_some(v),
            None => ctx.vals().option_none(),
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements in random order.
    pub(crate) fn shuffle(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let mut e = ctx.vals().take_array(a, "Random.shuffle")?;

        e.shuffle(&mut rand::thread_rng());
        Ok(ctx.vals().add(Payload::Array(Arc::new(e))))
    }

    /// `forall T. (Array[T], Int) -> Result[Array[T], String]`
    ///
    /// Picks `n` random elements without replacement.
    /// Returns `Result.Err` if `n > Iter.length(arr)`.
    pub(crate) fn sample(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let n_raw = ctx.vals().int_payload(args[1], "Random.sample")?;
        let elems: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(a, "Random.sample")?
            .iter()
            .copied()
            .collect();
        let n = usize::try_from(n_raw).unwrap_or(usize::MAX);

        if n > elems.len() {
            let msg_str = format!(
                "Random.sample: n ({n}) exceeds array length ({})",
                elems.len()
            );
            let msg = ctx.vals().intern(&msg_str);
            let msg_val = ctx.vals().add(Payload::String(msg));
            Ok(ctx.vals().result_err(msg_val))
        } else {
            let sampled: SmallVec<[ValueId; 4]> = elems
                .choose_multiple(&mut rand::thread_rng(), n)
                .copied()
                .collect();
            let arr = ctx.vals().add(Payload::Array(Arc::new(sampled)));
            Ok(ctx.vals().result_ok(arr))
        }
    }

    /// `() -> String`
    ///
    /// Generates a random UUID v4 string.
    pub(crate) fn uuid(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let id = uuid::Uuid::new_v4().to_string();
        let sid = ctx.vals().intern(&id);
        Ok(ctx.vals().add(Payload::String(sid)))
    }
}
