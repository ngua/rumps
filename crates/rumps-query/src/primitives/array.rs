use std::iter;
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

    /// `forall T. Array[T] -> Option[T]`
    ///
    /// Returns `Option.Some(last)` for nonempty arrays and `Option.None` for `[]`.
    pub(crate) fn last<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.last", "Array"));

            Ok(match elems.last().copied() {
                Some(v) => ctx.option_some(v),
                None => ctx.option_none(),
            })
        })
    }

    /// `forall T. Array[T] -> Array[T]`
    ///
    /// Returns all elements except the last, or `[]` for arrays with length `0`
    /// or `1`.
    pub(crate) fn init<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.init", "Array"));

            let len = elems.len();
            let init = if len > 1 {
                elems
                    .get(..len - 1)
                    .map(SmallVec::from_slice)
                    .unwrap_or_else(|| invariant!("Array.init slice"))
            } else {
                SmallVec::new()
            };

            Ok(ctx.add(Payload::Array(Arc::new(init))))
        })
    }

    /// `forall T. Array[T] -> Option[(T, Array[T])]`
    ///
    /// Returns the first element and tail for nonempty arrays, or `Option.None`
    /// for `[]`.
    pub(crate) fn uncons<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.uncons", "Array"));

            Ok(match elems.first().copied() {
                Some(v) => {
                    let tail = elems
                        .get(1..)
                        .map(SmallVec::from_slice)
                        .unwrap_or_else(|| invariant!("Array.uncons tail"));
                    let b = ctx.add(Payload::Array(Arc::new(tail)));
                    let tup =
                        ctx.add(Payload::Tuple(Arc::new(smallvec![v, b])));
                    ctx.option_some(tup)
                }
                None => ctx.option_none(),
            })
        })
    }

    /// `forall T. Array[T] -> Option[(Array[T], T)]`
    ///
    /// Returns the init and last element for nonempty arrays, or `Option.None`
    /// for `[]`.
    pub(crate) fn unsnoc<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.unsnoc", "Array"));

            let len = elems.len();
            Ok(match elems.last().copied() {
                Some(v) => {
                    let init = elems
                        .get(..len - 1)
                        .map(SmallVec::from_slice)
                        .unwrap_or_else(|| invariant!("Array.unsnoc init"));
                    let b = ctx.add(Payload::Array(Arc::new(init)));
                    let tup =
                        ctx.add(Payload::Tuple(Arc::new(smallvec![b, v])));
                    ctx.option_some(tup)
                }
                None => ctx.option_none(),
            })
        })
    }

    /// `forall T. (Int, Array[T]) -> Array[T]`
    ///
    /// Takes a prefix of length `n`, with `n` clamped into `0..len`.
    pub(crate) fn take<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = ctx
                .arena
                .payload(a)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.take", "Int"));
            let elems = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.take", "Array"));

            let len = elems.len();
            let idx = if n <= 0 {
                0
            } else {
                usize::try_from(n).map_or(len, |i| i.min(len))
            };
            let taken = elems
                .get(..idx)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.take prefix"));

            Ok(ctx.add(Payload::Array(Arc::new(taken))))
        })
    }

    /// `forall T. (Int, Array[T]) -> Array[T]`
    ///
    /// Drops a prefix of length `n`, with `n` clamped into `0..len`.
    pub(crate) fn drop<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = ctx
                .arena
                .payload(a)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.drop", "Int"));
            let elems = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.drop", "Array"));

            let len = elems.len();
            let idx = if n <= 0 {
                0
            } else {
                usize::try_from(n).map_or(len, |i| i.min(len))
            };
            let dropped = elems
                .get(idx..)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.drop suffix"));

            Ok(ctx.add(Payload::Array(Arc::new(dropped))))
        })
    }

    /// `forall T. (Int, Array[T]) -> (Array[T], Array[T])`
    ///
    /// Splits an array at `n`, with `n` clamped into `0..len`.
    pub(crate) fn split_at<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = ctx
                .arena
                .payload(a)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.split-at", "Int"));
            let elems = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.split-at", "Array"));

            let len = elems.len();
            let idx = if n <= 0 {
                0
            } else {
                usize::try_from(n).map_or(len, |i| i.min(len))
            };
            let l = elems
                .get(..idx)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.split-at prefix"));
            let r = elems
                .get(idx..)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.split-at suffix"));
            let l_id = ctx.add(Payload::Array(Arc::new(l)));
            let r_id = ctx.add(Payload::Array(Arc::new(r)));

            Ok(ctx.add(Payload::Tuple(Arc::new(smallvec![l_id, r_id]))))
        })
    }

    /// `forall T. Array[T] -> Array[(Int, T)]`
    ///
    /// Pairs each element with its zero based index.
    pub(crate) fn indexed<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.indexed", "Array"));

            let vals: SmallVec<[(usize, ValueId); 4]> =
                elems.iter().copied().enumerate().collect();
            let pairs = vals
                .iter()
                .copied()
                .map(|(i, v)| {
                    let idx = i64::try_from(i)
                        .unwrap_or_else(|_| invariant!("Array.indexed index"));
                    let i_id = ctx.add(Payload::Int(idx));
                    ctx.add(Payload::Tuple(Arc::new(smallvec![i_id, v])))
                })
                .collect();

            Ok(ctx.add(Payload::Array(Arc::new(pairs))))
        })
    }

    /// `forall T. T -> Array[T]`
    ///
    /// Returns a one element array.
    pub(crate) fn singleton<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];

            Ok(ctx.add(Payload::Array(Arc::new(smallvec![a]))))
        })
    }

    /// `forall T. (T, Array[T]) -> Array[T]`
    ///
    /// Prepends an element to an array.
    pub(crate) fn cons<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let mut elems = ctx
                .arena
                .take_array(b)
                .unwrap_or_else(|| typechecked!("Array.cons", "Array"));

            elems.insert(0, a);
            Ok(ctx.add(Payload::Array(Arc::new(elems))))
        })
    }

    /// `forall T. (Array[T], Int, T) -> Option[Array[T]]`
    ///
    /// Replaces the element at a valid index, or returns `Option.None`.
    pub(crate) fn set_at<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let c = args[2];
            let n = ctx
                .arena
                .payload(b)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.set-at", "Int"));

            let out = if n < 0 {
                ctx.option_none()
            } else {
                let mut elems = ctx
                    .arena
                    .take_array(a)
                    .unwrap_or_else(|| typechecked!("Array.set-at", "Array"));
                match usize::try_from(n).ok() {
                    Some(i) if elems.get(i).is_some() => {
                        let slot = elems.get_mut(i).unwrap_or_else(|| {
                            invariant!("Array.set-at index")
                        });
                        *slot = c;
                        let arr = ctx.add(Payload::Array(Arc::new(elems)));
                        ctx.option_some(arr)
                    }
                    None => ctx.option_none(),
                    Some(_) => ctx.option_none(),
                }
            };

            Ok(out)
        })
    }

    /// `forall T. (Array[T], Int) -> Option[Array[T]]`
    ///
    /// Removes the element at a valid index, or returns `Option.None`.
    pub(crate) fn remove_at<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = ctx
                .arena
                .payload(b)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.remove-at", "Int"));

            let out = if n < 0 {
                ctx.option_none()
            } else {
                let mut elems = ctx.arena.take_array(a).unwrap_or_else(|| {
                    typechecked!("Array.remove-at", "Array")
                });
                match usize::try_from(n)
                    .ok()
                    .and_then(|i| elems.get(i).map(|_| i))
                {
                    Some(i) => {
                        elems.remove(i);
                        let arr = ctx.add(Payload::Array(Arc::new(elems)));
                        ctx.option_some(arr)
                    }
                    None => ctx.option_none(),
                }
            };

            Ok(out)
        })
    }

    /// `forall T. (Array[T], Int, T) -> Array[T]`
    ///
    /// Inserts an element at `idx` clamped to the array bounds.
    pub(crate) fn insert_at<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let c = args[2];
            let n = ctx
                .arena
                .payload(b)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.insert-at", "Int"));
            let mut elems = ctx
                .arena
                .take_array(a)
                .unwrap_or_else(|| typechecked!("Array.insert-at", "Array"));

            let len = elems.len();
            let idx = if n <= 0 {
                0
            } else {
                usize::try_from(n).map_or(len, |i| i.min(len))
            };
            elems.insert(idx, c);

            Ok(ctx.add(Payload::Array(Arc::new(elems))))
        })
    }

    /// `forall T. Array[Array[T]] -> Array[T]`
    ///
    /// Concatenates nested arrays.
    pub(crate) fn flatten<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let arrs = ctx
                .arena
                .get_array(a)
                .unwrap_or_else(|| typechecked!("Array.flatten", "Array"));

            let flat = arrs
                .iter()
                .copied()
                .map(|id| {
                    ctx.arena.get_array(id).unwrap_or_else(|| {
                        typechecked!("Array.flatten", "Array")
                    })
                })
                .fold(SmallVec::new(), |mut acc, elems| {
                    acc.extend(elems.iter().copied());
                    acc
                });

            Ok(ctx.add(Payload::Array(Arc::new(flat))))
        })
    }

    /// `forall T. (Word, Array[T]) -> Array[Array[T]]`
    ///
    /// Splits an array into consecutive chunks of length `n`.
    pub(crate) fn chunks_of<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = ctx
                .arena
                .payload(a)
                .and_then(|v| match v {
                    Payload::Word(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.chunks-of", "Word"));
            let elems = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.chunks-of", "Array"));

            let parts: SmallVec<[SmallVec<[ValueId; 4]>; 4]> = if n == 0 {
                SmallVec::new()
            } else {
                elems.chunks(n).map(SmallVec::from_slice).collect()
            };
            let chunks = parts
                .into_iter()
                .map(|arr| ctx.add(Payload::Array(Arc::new(arr))))
                .collect();

            Ok(ctx.add(Payload::Array(Arc::new(chunks))))
        })
    }

    /// `forall T. (Word, Array[T]) -> Array[Array[T]]`
    ///
    /// Returns all contiguous windows of length `n`.
    pub(crate) fn windows<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = ctx
                .arena
                .payload(a)
                .and_then(|v| match v {
                    Payload::Word(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.windows", "Word"));
            let elems = ctx
                .arena
                .get_array(b)
                .unwrap_or_else(|| typechecked!("Array.windows", "Array"));

            let parts: SmallVec<[SmallVec<[ValueId; 4]>; 4]> = if n == 0 {
                SmallVec::new()
            } else {
                elems.windows(n).map(SmallVec::from_slice).collect()
            };
            let windows = parts
                .into_iter()
                .map(|arr| ctx.add(Payload::Array(Arc::new(arr))))
                .collect();

            Ok(ctx.add(Payload::Array(Arc::new(windows))))
        })
    }

    /// `forall T. (Int, T) -> Array[T]`
    ///
    /// Returns an array containing `n` copies of a value.
    pub(crate) fn replicate<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = ctx
                .arena
                .payload(a)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.replicate", "Int"));

            let elems = if n <= 0 {
                SmallVec::new()
            } else {
                iter::repeat_n(b, usize::try_from(n).unwrap_or(usize::MAX))
                    .collect()
            };

            Ok(ctx.add(Payload::Array(Arc::new(elems))))
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
