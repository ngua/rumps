use std::sync::Arc;

use itertools::Itertools;
use smallvec::{smallvec, SmallVec};

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Array;

impl Prim for Array {}

impl Array {
    /// `forall T. (Array[T], T) -> Array[T]`
    ///
    /// Returns a new array with `val` appended to the end.
    pub(crate) fn push<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let mut e = ctx
                .arena
                .take_array(a)
                .unwrap_or_else(|| typechecked!("Array.push", "Array"));

            e.push(b);
            Ok(ctx.add(Payload::Array(Arc::new(e))))
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with the last element removed.
    /// Returns an empty array if the input is empty.
    pub(crate) fn pop<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let mut e = ctx
                .arena
                .take_array(a)
                .unwrap_or_else(|| typechecked!("Array.pop", "Array"));

            e.pop();
            Ok(ctx.add(Payload::Array(Arc::new(e))))
        })
    }

    /// `forall T. (Array[T]) -> Option[T]`
    ///
    /// Returns `Option.Some(first)` if the array is non-empty,
    /// `Option.None` if empty.
    pub(crate) fn head<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.head", "Array"));

            Ok(match elems.first() {
                Some(first) => ctx.option_some(*first),
                None => ctx.option_none(),
            })
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with all elements except the first.
    /// Returns an empty array if the input is empty or has one element.
    pub(crate) fn tail<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.tail", "Array"));

            let tail: SmallVec<[ValueId; 4]> =
                elems.get(1..).map(SmallVec::from_slice).unwrap_or_default();
            Ok(ctx.add(Payload::Array(Arc::new(tail))))
        })
    }

    /// `forall T. (Array[T], Int, Int) -> Array[T]`
    ///
    /// Returns a new array containing elements from index `start` (inclusive)
    /// to index `end` (exclusive). Indices are clamped to valid bounds.
    pub(crate) fn slice<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let c = args[2];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.slice", "Array"));

            let start = ctx
                .arena
                .payload(b)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    typechecked!("Array.slice", "start must be Int")
                });

            let end = ctx
                .arena
                .payload(c)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.slice", "Int"));

            let len = elems.len() as i64;
            let start_idx = start.max(0).min(len) as usize;
            let end_idx = end.max(0).min(len) as usize;

            let sliced: SmallVec<[ValueId; 4]> = elems
                .get(start_idx..end_idx)
                .map(SmallVec::from_slice)
                .unwrap_or_default();

            Ok(ctx.add(Payload::Array(Arc::new(sliced))))
        })
    }

    /// `forall T. (Array[T], Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements of `b` appended to `a`.
    /// Both arrays must have the same element type.
    pub(crate) fn concat<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let mut combined = ctx
                .arena
                .take_array(a)
                .unwrap_or_else(|| typechecked!("Array.concat", "Array"));

            let elems_b = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.concat", "Array"));

            // Type checker guarantees both arrays have matching element types
            combined.extend(elems_b.iter().copied());
            Ok(ctx.add(Payload::Array(Arc::new(combined))))
        })
    }

    /// `forall T U. (Array[T], Array[U]) -> Array[(T, U)]`
    ///
    /// Pairs elements from two arrays. Result length is the shorter array.
    pub(crate) fn zip<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let elems_a = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.zip", "Array"));

            let elems_b = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.zip", "Array"));

            let zipped: SmallVec<[(ValueId, ValueId); 4]> = elems_a
                .iter()
                .zip(elems_b.iter())
                .map(|(a, b)| (*a, *b))
                .collect();

            let pairs: SmallVec<[ValueId; 4]> = zipped
                .iter()
                .map(|(a, b)| {
                    let tup = Payload::Tuple(Arc::new(smallvec![*a, *b]));
                    ctx.add(tup)
                })
                .collect();

            Ok(ctx.add(Payload::Array(Arc::new(pairs))))
        })
    }

    /// `forall T U. (Array[(T, U)]) -> (Array[T], Array[U])`
    ///
    /// Splits an array of pairs into a pair of arrays.
    pub(crate) fn unzip<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let pairs = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.unzip", "Array"));

            // Map pairs to (a, b) and unzip
            let (firsts, seconds): (
                SmallVec<[ValueId; 4]>,
                SmallVec<[ValueId; 4]>,
            ) = pairs
                .iter()
                .map(|id| {
                    ctx.arena.payload(*id).unwrap_or_else(|| {
                        typechecked!("Array.unzip", "valid id")
                    })
                })
                .map(|v| match v {
                    Payload::Tuple(elems) => (elems[0], elems[1]),
                    _ => typechecked!("Array.unzip", "(T, U)"),
                })
                .unzip();

            let arr_a_id = ctx.add(Payload::Array(Arc::new(firsts)));
            let arr_b_id = ctx.add(Payload::Array(Arc::new(seconds)));

            Ok(
                ctx.add(Payload::Tuple(Arc::new(smallvec![
                    arr_a_id, arr_b_id
                ]))),
            )
        })
    }

    /// `forall T. (T, Array[T]) -> Array[T]`
    ///
    /// Inserts `sep` between each pair of elements.
    pub(crate) fn intersperse<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let elems = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.intersperse", "Array"));

            let result: SmallVec<[ValueId; 4]> =
                Itertools::intersperse(elems.iter().copied(), a).collect();

            Ok(ctx.add(Payload::Array(Arc::new(result))))
        })
    }
}
