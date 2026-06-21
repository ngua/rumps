use std::cmp::Ordering;
use std::iter;
use std::sync::Arc;

use futures::future::BoxFuture;
use itertools::Itertools;
use smallvec::{smallvec, SmallVec};

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::{ClassId, Error, Result};

pub(crate) struct Array;

impl Body for Array {}

impl Array {
    /// `forall T. (Array[T], T) -> Array[T]`
    ///
    /// Returns a new array with `val` appended to the end.
    pub(crate) fn push(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let mut e = ctx.vals().take_array(a, "Array.push")?;

        e.push(b);
        Ok(ctx.vals().add(Payload::Array(Arc::new(e))))
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with the last element removed.
    /// Returns an empty array if the input is empty.
    pub(crate) fn pop(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let mut e = ctx.vals().take_array(a, "Array.pop")?;

        e.pop();
        Ok(ctx.vals().add(Payload::Array(Arc::new(e))))
    }

    /// `forall T. (Array[T]) -> Option[T]`
    ///
    /// Returns `Option.Some(first)` if the array is non-empty,
    /// `Option.None` if empty.
    pub(crate) fn head(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let first = ctx.vals().array(a, "Array.head")?.first().copied();
        Ok(match first {
            Some(id) => ctx.vals().option_some(id),
            None => ctx.vals().option_none(),
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with all elements except the first.
    /// Returns an empty array if the input is empty or has one element.
    pub(crate) fn tail(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let tail = ctx
            .vals()
            .array(a, "Array.tail")?
            .get(1..)
            .map(SmallVec::from_slice)
            .unwrap_or_default();
        Ok(ctx.vals().add(Payload::Array(Arc::new(tail))))
    }

    /// `forall T. Array[T] -> Option[T]`
    ///
    /// Returns `Option.Some(last)` for nonempty arrays and `Option.None` for `[]`.
    pub(crate) fn last(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let last = ctx.vals().array(a, "Array.last")?.last().copied();
        Ok(match last {
            Some(id) => ctx.vals().option_some(id),
            None => ctx.vals().option_none(),
        })
    }

    /// `forall T. Array[T] -> Array[T]`
    ///
    /// Returns all elements except the last, or `[]` for arrays with length `0`
    /// or `1`.
    pub(crate) fn init(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let elems: SmallVec<[ValueId; 4]> =
            ctx.vals().array(a, "Array.init")?.iter().copied().collect();
        let len = elems.len();
        let init = if len > 1 {
            elems
                .get(..len - 1)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.init slice"))
        } else {
            SmallVec::new()
        };

        Ok(ctx.vals().add(Payload::Array(Arc::new(init))))
    }

    /// `forall T. Array[T] -> Option[(T, Array[T])]`
    ///
    /// Returns the first element and tail for nonempty arrays, or `Option.None`
    /// for `[]`.
    pub(crate) fn uncons(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let data = {
            let vals = ctx.vals();
            let elems = vals.array(a, "Array.uncons")?;
            elems.first().copied().map(|v| {
                let tail = elems
                    .get(1..)
                    .map(SmallVec::from_slice)
                    .unwrap_or_else(|| invariant!("Array.uncons tail"));
                (v, tail)
            })
        };

        Ok(match data {
            Some((v, tail)) => {
                let b = ctx.vals().add(Payload::Array(Arc::new(tail)));
                let tup =
                    ctx.vals().add(Payload::Tuple(Arc::new(smallvec![v, b])));
                ctx.vals().option_some(tup)
            }
            None => ctx.vals().option_none(),
        })
    }

    /// `forall T. Array[T] -> Option[(Array[T], T)]`
    ///
    /// Returns the init and last element for nonempty arrays, or `Option.None`
    /// for `[]`.
    pub(crate) fn unsnoc(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let data = {
            let vals = ctx.vals();
            let elems = vals.array(a, "Array.unsnoc")?;
            let len = elems.len();
            elems.last().copied().map(|v| {
                let init = elems
                    .get(..len - 1)
                    .map(SmallVec::from_slice)
                    .unwrap_or_else(|| invariant!("Array.unsnoc init"));
                (init, v)
            })
        };

        Ok(match data {
            Some((init, v)) => {
                let b = ctx.vals().add(Payload::Array(Arc::new(init)));
                let tup =
                    ctx.vals().add(Payload::Tuple(Arc::new(smallvec![b, v])));
                ctx.vals().option_some(tup)
            }
            None => ctx.vals().option_none(),
        })
    }

    /// `forall T. (Int, Array[T]) -> Array[T]`
    ///
    /// Takes a prefix of length `n`, with `n` clamped into `0..len`.
    pub(crate) fn take(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n = ctx.vals().int_payload(args[0], "Array.take")?;
        let a = args[1];
        let elems: SmallVec<[ValueId; 4]> =
            ctx.vals().array(a, "Array.take")?.iter().copied().collect();
        let idx = Self::idx(n, elems.len());
        let taken = elems
            .get(..idx)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!("Array.take prefix"));

        Ok(ctx.vals().add(Payload::Array(Arc::new(taken))))
    }

    /// `forall T. (Int, Array[T]) -> Array[T]`
    ///
    /// Drops a prefix of length `n`, with `n` clamped into `0..len`.
    pub(crate) fn drop(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n = ctx.vals().int_payload(args[0], "Array.drop")?;
        let a = args[1];
        let elems: SmallVec<[ValueId; 4]> =
            ctx.vals().array(a, "Array.drop")?.iter().copied().collect();
        let idx = Self::idx(n, elems.len());
        let dropped = elems
            .get(idx..)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!("Array.drop suffix"));

        Ok(ctx.vals().add(Payload::Array(Arc::new(dropped))))
    }

    /// `forall T. (Int, Array[T]) -> (Array[T], Array[T])`
    ///
    /// Splits an array at `n`, with `n` clamped into `0..len`.
    pub(crate) fn split_at(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n = ctx.vals().int_payload(args[0], "Array.split-at")?;
        let a = args[1];
        let elems: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(a, "Array.split-at")?
            .iter()
            .copied()
            .collect();
        let idx = Self::idx(n, elems.len());
        let l = elems
            .get(..idx)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!("Array.split-at prefix"));
        let r = elems
            .get(idx..)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!("Array.split-at suffix"));
        let l_id = ctx.vals().add(Payload::Array(Arc::new(l)));
        let r_id = ctx.vals().add(Payload::Array(Arc::new(r)));

        Ok(ctx
            .vals()
            .add(Payload::Tuple(Arc::new(smallvec![l_id, r_id]))))
    }

    /// `forall T. Array[T] -> Array[(Int, T)]`
    ///
    /// Pairs each element with its zero based index.
    pub(crate) fn indexed(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let vals: SmallVec<[(usize, ValueId); 4]> = ctx
            .vals()
            .array(a, "Array.indexed")?
            .iter()
            .copied()
            .enumerate()
            .collect();
        let pairs = vals
            .iter()
            .copied()
            .map(|(i, v)| {
                let idx = i64::try_from(i)
                    .unwrap_or_else(|_| invariant!("Array.indexed index"));
                let i_id = ctx.vals().add(Payload::Int(idx));
                ctx.vals().add(Payload::Tuple(Arc::new(smallvec![i_id, v])))
            })
            .collect();

        Ok(ctx.vals().add(Payload::Array(Arc::new(pairs))))
    }

    /// `forall T. T -> Array[T]`
    ///
    /// Returns a one element array.
    pub(crate) fn singleton(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        Ok(ctx.vals().add(Payload::Array(Arc::new(smallvec![a]))))
    }

    /// `forall T. (T, Array[T]) -> Array[T]`
    ///
    /// Prepends an element to an array.
    pub(crate) fn cons(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let mut elems = ctx.vals().take_array(b, "Array.cons")?;

        elems.insert(0, a);
        Ok(ctx.vals().add(Payload::Array(Arc::new(elems))))
    }

    /// `forall T. (Array[T], Int, T) -> Option[Array[T]]`
    ///
    /// Replaces the element at a valid index, or returns `Option.None`.
    pub(crate) fn set_at(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let n = ctx.vals().int_payload(args[1], "Array.set-at")?;
        let c = args[2];

        let out = if n < 0 {
            ctx.vals().option_none()
        } else {
            let mut elems = ctx.vals().take_array(a, "Array.set-at")?;
            match usize::try_from(n)
                .ok()
                .and_then(|i| elems.get(i).map(|_| i))
            {
                Some(i) => {
                    let slot = elems
                        .get_mut(i)
                        .unwrap_or_else(|| invariant!("Array.set-at index"));
                    *slot = c;
                    let arr = ctx.vals().add(Payload::Array(Arc::new(elems)));
                    ctx.vals().option_some(arr)
                }
                None => ctx.vals().option_none(),
            }
        };

        Ok(out)
    }

    /// `forall T. (Array[T], Int) -> Option[Array[T]]`
    ///
    /// Removes the element at a valid index, or returns `Option.None`.
    pub(crate) fn remove_at(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let n = ctx.vals().int_payload(args[1], "Array.remove-at")?;

        let out = if n < 0 {
            ctx.vals().option_none()
        } else {
            let mut elems = ctx.vals().take_array(a, "Array.remove-at")?;
            match usize::try_from(n)
                .ok()
                .and_then(|i| elems.get(i).map(|_| i))
            {
                Some(i) => {
                    elems.remove(i);
                    let arr = ctx.vals().add(Payload::Array(Arc::new(elems)));
                    ctx.vals().option_some(arr)
                }
                None => ctx.vals().option_none(),
            }
        };

        Ok(out)
    }

    /// `forall T. (Array[T], Int, T) -> Array[T]`
    ///
    /// Inserts an element at `idx` clamped to the array bounds.
    pub(crate) fn insert_at(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let n = ctx.vals().int_payload(args[1], "Array.insert-at")?;
        let c = args[2];
        let mut elems = ctx.vals().take_array(a, "Array.insert-at")?;
        let idx = Self::idx(n, elems.len());

        elems.insert(idx, c);
        Ok(ctx.vals().add(Payload::Array(Arc::new(elems))))
    }

    /// `forall T. Array[Array[T]] -> Array[T]`
    ///
    /// Concatenates nested arrays.
    pub(crate) fn flatten(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let arrs: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(a, "Array.flatten")?
            .iter()
            .copied()
            .collect();
        let flat =
            arrs.into_iter().try_fold(SmallVec::new(), |mut acc, id| {
                let vals = ctx.vals();
                acc.extend(vals.array(id, "Array.flatten")?.iter().copied());
                Ok::<_, Error>(acc)
            })?;

        Ok(ctx.vals().add(Payload::Array(Arc::new(flat))))
    }

    /// `forall T. (Word, Array[T]) -> Array[Array[T]]`
    ///
    /// Splits an array into consecutive chunks of length `n`.
    pub(crate) fn chunks_of(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n = match ctx.vals().payload(args[0])? {
            Payload::Word(n) => *n,
            _ => typechecked!("Array.chunks-of", "Word"),
        };
        let a = args[1];
        let parts: SmallVec<[SmallVec<[ValueId; 4]>; 4]> = if n == 0 {
            SmallVec::new()
        } else {
            ctx.vals()
                .array(a, "Array.chunks-of")?
                .chunks(n)
                .map(SmallVec::from_slice)
                .collect()
        };
        let chunks = parts
            .into_iter()
            .map(|arr| ctx.vals().add(Payload::Array(Arc::new(arr))))
            .collect();

        Ok(ctx.vals().add(Payload::Array(Arc::new(chunks))))
    }

    /// `forall T. (Word, Array[T]) -> Array[Array[T]]`
    ///
    /// Returns all contiguous windows of length `n`.
    pub(crate) fn windows(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n = match ctx.vals().payload(args[0])? {
            Payload::Word(n) => *n,
            _ => typechecked!("Array.windows", "Word"),
        };
        let a = args[1];
        let parts: SmallVec<[SmallVec<[ValueId; 4]>; 4]> = if n == 0 {
            SmallVec::new()
        } else {
            ctx.vals()
                .array(a, "Array.windows")?
                .windows(n)
                .map(SmallVec::from_slice)
                .collect()
        };
        let windows = parts
            .into_iter()
            .map(|arr| ctx.vals().add(Payload::Array(Arc::new(arr))))
            .collect();

        Ok(ctx.vals().add(Payload::Array(Arc::new(windows))))
    }

    /// `forall T. (Int, T) -> Array[T]`
    ///
    /// Returns an array containing `n` copies of a value.
    pub(crate) fn replicate(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n = ctx.vals().int_payload(args[0], "Array.replicate")?;
        let a = args[1];
        let elems = if n <= 0 {
            SmallVec::new()
        } else {
            iter::repeat_n(a, usize::try_from(n).unwrap_or(usize::MAX))
                .collect()
        };

        Ok(ctx.vals().add(Payload::Array(Arc::new(elems))))
    }

    /// `forall T: Ord. Array[T] -> Array[T]`
    ///
    /// Sorts an array with `Ord:compare`.
    /// Returns a `BoxFuture` because class dispatch may invoke `async` user
    /// code.
    pub(crate) fn sort<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx.vals().array_ids(a, "Array.sort")?;
            let cmp = ctx.vals().intern("compare");
            let ord_ty = ctx.vals().type_id(TypeId::ORDERING);
            let mut runs: Vec<SmallVec<[ValueId; 4]>> =
                elems.iter().copied().map(|v| smallvec![v]).collect();

            while let Some(()) = (runs.len() > 1).then_some(()) {
                let mut it = runs.into_iter();
                let mut next =
                    Vec::with_capacity((it.len().saturating_add(1)) / 2);

                while let Some(left) = it.next() {
                    match it.next() {
                        Some(right) => {
                            let mut lrun = left.into_iter().peekable();
                            let mut rrun = right.into_iter().peekable();
                            let mut merged = SmallVec::new();

                            while let Some((l, r)) =
                                lrun.peek().copied().zip(rrun.peek().copied())
                            {
                                let id = ctx
                                    .class_call(
                                        ClassId::ORD,
                                        cmp,
                                        smallvec![l, r],
                                        Some(ord_ty),
                                    )
                                    .await?;
                                let ord = {
                                    let vals = ctx.vals();
                                    let v = vals.value(id)?;
                                    let ty = vals.value_variant_base_type(v);
                                    match &v.payload {
                                        Payload::Int(n) => n.cmp(&0),
                                        Payload::Variant { tag, .. }
                                            if ty.is_some_and(|ty| {
                                                ty == TypeId::ORDERING
                                            }) =>
                                        {
                                            match tag {
                                                0 => Ordering::Less,
                                                1 => Ordering::Equal,
                                                2 => Ordering::Greater,
                                                _ => typechecked!(
                                                    "Array.sort",
                                                    "Ord:compare result"
                                                ),
                                            }
                                        }
                                        _ => typechecked!(
                                            "Array.sort",
                                            "Ord:compare result"
                                        ),
                                    }
                                };

                                if ord == Ordering::Greater {
                                    let v = rrun.next().unwrap_or_else(|| {
                                        invariant!("Array.sort right")
                                    });
                                    merged.push(v);
                                } else {
                                    let v = lrun.next().unwrap_or_else(|| {
                                        invariant!("Array.sort left")
                                    });
                                    merged.push(v);
                                }
                            }

                            merged.extend(lrun);
                            merged.extend(rrun);
                            next.push(merged);
                        }
                        None => next.push(left),
                    }
                }

                runs = next;
            }

            let elems = runs.pop().unwrap_or_default();
            Ok(ctx.vals().add(Payload::Array(Arc::new(elems))))
        })
    }

    /// `forall T: Ord. Array[T] -> Option[T]`
    ///
    /// Returns the least element according to `Ord:compare`.
    /// Returns a `BoxFuture` because class dispatch may invoke `async` user
    /// code.
    pub(crate) fn minimum<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx.vals().array_ids(a, "Array.minimum")?;
            let cmp = ctx.vals().intern("compare");
            let ord_ty = ctx.vals().type_id(TypeId::ORDERING);
            let mut it = elems.iter().copied();

            match it.next() {
                Some(mut best) => {
                    while let Some(elem) = it.next() {
                        let id = ctx
                            .class_call(
                                ClassId::ORD,
                                cmp,
                                smallvec![best, elem],
                                Some(ord_ty),
                            )
                            .await?;
                        let ord = {
                            let vals = ctx.vals();
                            let v = vals.value(id)?;
                            let ty = vals.value_variant_base_type(v);
                            match &v.payload {
                                Payload::Int(n) => n.cmp(&0),
                                Payload::Variant { tag, .. }
                                    if ty.is_some_and(|ty| {
                                        ty == TypeId::ORDERING
                                    }) =>
                                {
                                    match tag {
                                        0 => Ordering::Less,
                                        1 => Ordering::Equal,
                                        2 => Ordering::Greater,
                                        _ => typechecked!(
                                            "Array.minimum",
                                            "Ord:compare result"
                                        ),
                                    }
                                }
                                _ => typechecked!(
                                    "Array.minimum",
                                    "Ord:compare result"
                                ),
                            }
                        };

                        best = match ord {
                            Ordering::Greater => elem,
                            Ordering::Less | Ordering::Equal => best,
                        };
                    }

                    Ok(ctx.vals().option_some(best))
                }
                None => Ok(ctx.vals().option_none()),
            }
        })
    }

    /// `forall T: Ord. Array[T] -> Option[T]`
    ///
    /// Returns the greatest element according to `Ord:compare`.
    /// Returns a `BoxFuture` because class dispatch may invoke `async` user
    /// code.
    pub(crate) fn maximum<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let elems = ctx.vals().array_ids(a, "Array.maximum")?;
            let cmp = ctx.vals().intern("compare");
            let ord_ty = ctx.vals().type_id(TypeId::ORDERING);
            let mut it = elems.iter().copied();

            match it.next() {
                Some(mut best) => {
                    while let Some(elem) = it.next() {
                        let id = ctx
                            .class_call(
                                ClassId::ORD,
                                cmp,
                                smallvec![best, elem],
                                Some(ord_ty),
                            )
                            .await?;
                        let ord = {
                            let vals = ctx.vals();
                            let v = vals.value(id)?;
                            let ty = vals.value_variant_base_type(v);
                            match &v.payload {
                                Payload::Int(n) => n.cmp(&0),
                                Payload::Variant { tag, .. }
                                    if ty.is_some_and(|ty| {
                                        ty == TypeId::ORDERING
                                    }) =>
                                {
                                    match tag {
                                        0 => Ordering::Less,
                                        1 => Ordering::Equal,
                                        2 => Ordering::Greater,
                                        _ => typechecked!(
                                            "Array.maximum",
                                            "Ord:compare result"
                                        ),
                                    }
                                }
                                _ => typechecked!(
                                    "Array.maximum",
                                    "Ord:compare result"
                                ),
                            }
                        };

                        best = match ord {
                            Ordering::Less => elem,
                            Ordering::Equal | Ordering::Greater => best,
                        };
                    }

                    Ok(ctx.vals().option_some(best))
                }
                None => Ok(ctx.vals().option_none()),
            }
        })
    }

    /// `forall T: Eq. (Array[T], T) -> Bool`
    ///
    /// Returns `true` when the array contains `b`.
    /// Returns a `BoxFuture` because class dispatch may invoke `async` user
    /// code.
    pub(crate) fn contains<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let elems = ctx.vals().array_ids(a, "Array.contains")?;
            let eq = ctx.vals().intern("eq");
            let bool_ty = ctx.vals().type_id(TypeId::BOOL);
            let mut it = elems.iter().copied();
            let mut found = false;

            while let Some(elem) = (!found).then(|| it.next()).flatten() {
                let id = ctx
                    .class_call(
                        ClassId::EQ,
                        eq,
                        smallvec![elem, b],
                        Some(bool_ty),
                    )
                    .await?;
                let ok = ctx.vals().bool_payload(id, "Array.contains")?;

                found = found || ok;
            }

            Ok(ctx.vals().add(Payload::Bool(found)))
        })
    }

    /// `forall T: Eq. (Array[T], T) -> Option[Int]`
    ///
    /// Returns the zero based index of the first matching element.
    /// Returns a `BoxFuture` because class dispatch may invoke `async` user
    /// code.
    pub(crate) fn elem_index<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let elems = ctx.vals().array_ids(a, "Array.elem-index")?;
            let eq = ctx.vals().intern("eq");
            let bool_ty = ctx.vals().type_id(TypeId::BOOL);
            let mut it = elems.iter().copied().enumerate();
            let mut found = None;

            while let Some((idx, elem)) =
                found.is_none().then(|| it.next()).flatten()
            {
                let id = ctx
                    .class_call(
                        ClassId::EQ,
                        eq,
                        smallvec![elem, b],
                        Some(bool_ty),
                    )
                    .await?;
                let ok = ctx.vals().bool_payload(id, "Array.elem-index")?;

                found = if ok { Some(idx) } else { found };
            }

            match found {
                Some(idx) => {
                    let n = i64::try_from(idx)
                        .unwrap_or_else(|_| invariant!("Array.elem-index"));
                    let id = ctx.vals().add(Payload::Int(n));
                    Ok(ctx.vals().option_some(id))
                }
                None => Ok(ctx.vals().option_none()),
            }
        })
    }

    /// `forall T. (Array[T], Int, Int) -> Array[T]`
    ///
    /// Returns a new array containing elements from index `start` (inclusive)
    /// to index `end` (exclusive). Indices are clamped to valid bounds.
    pub(crate) fn slice(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let start = ctx.vals().int_payload(args[1], "Array.slice")?;
        let end = ctx.vals().int_payload(args[2], "Array.slice")?;
        let elems: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(a, "Array.slice")?
            .iter()
            .copied()
            .collect();
        let len = i64::try_from(elems.len())
            .unwrap_or_else(|_| invariant!("Array.slice length"));
        let start_idx = start.max(0).min(len) as usize;
        let end_idx = end.max(0).min(len) as usize;
        let sliced = elems
            .get(start_idx..end_idx)
            .map(SmallVec::from_slice)
            .unwrap_or_default();

        Ok(ctx.vals().add(Payload::Array(Arc::new(sliced))))
    }

    /// `forall T. (Array[T], Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements of `b` appended to `a`.
    /// Both arrays must have the same element type.
    pub(crate) fn concat(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let mut combined = ctx.vals().take_array(a, "Array.concat")?;
        let elems_b: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(b, "Array.concat")?
            .iter()
            .copied()
            .collect();

        combined.extend(elems_b);
        Ok(ctx.vals().add(Payload::Array(Arc::new(combined))))
    }

    /// `forall T. ((T, T) -> Ordering, Array[T]) -> Array[T]`
    ///
    /// Sorts an array with a callback comparator.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn sort_by<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let elems = ctx.vals().array_ids(a, "Array.sort-by")?;
            struct Frame {
                kind: u8,
                lo: usize,
                hi: usize,
                left: SmallVec<[ValueId; 4]>,
                right: SmallVec<[ValueId; 4]>,
                li: usize,
                ri: usize,
                merged: SmallVec<[ValueId; 4]>,
            }

            let sort = 0_u8;
            let after_right = 1_u8;
            let merge = 2_u8;
            let mut stack = Vec::new();
            stack.push(Frame {
                kind: sort,
                lo: 0,
                hi: elems.len(),
                left: SmallVec::new(),
                right: SmallVec::new(),
                li: 0,
                ri: 0,
                merged: SmallVec::new(),
            });
            let mut pending = None;
            let mut done = None;

            while let Some(()) = done.is_none().then_some(()) {
                if let Some(sorted) = pending.take() {
                    match stack.pop() {
                        Some(Frame {
                            kind, lo, hi, left, ..
                        }) if kind == after_right && left.is_empty() => {
                            let mid = lo + (hi - lo) / 2;
                            stack.push(Frame {
                                kind: after_right,
                                lo,
                                hi,
                                left: sorted,
                                right: SmallVec::new(),
                                li: 0,
                                ri: 0,
                                merged: SmallVec::new(),
                            });
                            stack.push(Frame {
                                kind: sort,
                                lo: mid,
                                hi,
                                left: SmallVec::new(),
                                right: SmallVec::new(),
                                li: 0,
                                ri: 0,
                                merged: SmallVec::new(),
                            });
                        }
                        Some(Frame { kind, left, .. })
                            if kind == after_right =>
                        {
                            stack.push(Frame {
                                kind: merge,
                                lo: 0,
                                hi: 0,
                                left,
                                right: sorted,
                                li: 0,
                                ri: 0,
                                merged: SmallVec::new(),
                            });
                        }
                        None => done = Some(sorted),
                        Some(_) => invariant!("Array.sort-by stack"),
                    }
                } else {
                    match stack.pop() {
                        Some(Frame { kind, lo, hi, .. }) if kind == sort => {
                            if hi - lo <= 1 {
                                pending = Some(
                                    elems
                                        .get(lo..hi)
                                        .map(SmallVec::from_slice)
                                        .unwrap_or_else(|| {
                                            invariant!("Array.sort-by slice")
                                        }),
                                );
                            } else {
                                let mid = lo + (hi - lo) / 2;
                                stack.push(Frame {
                                    kind: after_right,
                                    lo,
                                    hi,
                                    left: SmallVec::new(),
                                    right: SmallVec::new(),
                                    li: 0,
                                    ri: 0,
                                    merged: SmallVec::new(),
                                });
                                stack.push(Frame {
                                    kind: sort,
                                    lo,
                                    hi: mid,
                                    left: SmallVec::new(),
                                    right: SmallVec::new(),
                                    li: 0,
                                    ri: 0,
                                    merged: SmallVec::new(),
                                });
                            }
                        }
                        Some(Frame {
                            kind,
                            left,
                            right,
                            li,
                            ri,
                            mut merged,
                            ..
                        }) if kind == merge => {
                            let l =
                                left.get(li).copied().unwrap_or_else(|| {
                                    invariant!("Array.sort-by left")
                                });
                            let r =
                                right.get(ri).copied().unwrap_or_else(|| {
                                    invariant!("Array.sort-by right")
                                });
                            let id = ctx.invoke(f, smallvec![l, r]).await?;
                            let take_l = {
                                let vals = ctx.vals();
                                let v = vals.value(id)?;
                                let ty = vals.value_variant_base_type(v);
                                match &v.payload {
                                    Payload::Int(n) => *n <= 0,
                                    Payload::Variant { tag, .. }
                                        if ty.is_some_and(|ty| {
                                            ty == TypeId::ORDERING
                                        }) =>
                                    {
                                        *tag <= 1
                                    }
                                    _ => {
                                        typechecked!(
                                            "Array.sort-by",
                                            "Ordering"
                                        )
                                    }
                                }
                            };

                            if take_l {
                                merged.push(l);
                                let idx = li + 1;
                                if idx >= left.len() {
                                    merged.extend(
                                        right
                                            .get(ri..)
                                            .unwrap_or_else(|| {
                                                invariant!(
                                                    "Array.sort-by right rest"
                                                )
                                            })
                                            .iter()
                                            .copied(),
                                    );
                                    pending = Some(merged);
                                } else {
                                    stack.push(Frame {
                                        kind: merge,
                                        lo: 0,
                                        hi: 0,
                                        left,
                                        right,
                                        li: idx,
                                        ri,
                                        merged,
                                    });
                                }
                            } else {
                                merged.push(r);
                                let idx = ri + 1;
                                if idx >= right.len() {
                                    merged.extend(
                                        left.get(li..)
                                            .unwrap_or_else(|| {
                                                invariant!(
                                                    "Array.sort-by left rest"
                                                )
                                            })
                                            .iter()
                                            .copied(),
                                    );
                                    pending = Some(merged);
                                } else {
                                    stack.push(Frame {
                                        kind: merge,
                                        lo: 0,
                                        hi: 0,
                                        left,
                                        right,
                                        li,
                                        ri: idx,
                                        merged,
                                    });
                                }
                            }
                        }
                        Some(_) => invariant!("Array.sort-by stack"),
                        None => invariant!("Array.sort-by stack"),
                    }
                }
            }

            let elems = done.unwrap_or_default();
            Ok(ctx.vals().add(Payload::Array(Arc::new(elems))))
        })
    }

    /// `forall T U. (Array[T], Array[U]) -> Array[(T, U)]`
    ///
    /// Pairs elements from two arrays. Result length is the shorter array.
    pub(crate) fn zip(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let elems_a: SmallVec<[ValueId; 4]> =
            ctx.vals().array(a, "Array.zip")?.iter().copied().collect();
        let elems_b: SmallVec<[ValueId; 4]> =
            ctx.vals().array(b, "Array.zip")?.iter().copied().collect();
        let pairs = elems_a
            .iter()
            .zip(elems_b.iter())
            .map(|(l, r)| {
                ctx.vals().add(Payload::Tuple(Arc::new(smallvec![*l, *r])))
            })
            .collect();

        Ok(ctx.vals().add(Payload::Array(Arc::new(pairs))))
    }

    /// `forall T U V. ((T, U) -> V, Array[T], Array[U]) -> Array[V]`
    ///
    /// Zips two arrays and applies a callback to each pair.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn zip_with<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let b = args[2];
            let xs = ctx.vals().array_ids(a, "Array.zip-with")?;
            let ys = ctx.vals().array_ids(b, "Array.zip-with")?;
            let mut it = xs.iter().copied().zip(ys.iter().copied());
            let mut acc = SmallVec::new();

            while let Some((x, y)) = it.next() {
                let v = ctx.invoke(f, smallvec![x, y]).await?;
                acc.push(v);
            }

            Ok(ctx.vals().add(Payload::Array(Arc::new(acc))))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> Bool`
    ///
    /// Tests whether any element satisfies a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn any<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.any")?;
            let mut it = xs.iter().copied();
            let mut found = false;

            while let Some(x) = (!found).then(|| it.next()).flatten() {
                let y = ctx.invoke(f, smallvec![x]).await?;
                found = ctx.vals().bool_payload(y, "Array.any")?;
            }

            Ok(ctx.vals().add(Payload::Bool(found)))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> Bool`
    ///
    /// Tests whether every element satisfies a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn all<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.all")?;
            let mut it = xs.iter().copied();
            let mut ok = true;

            while let Some(x) = ok.then(|| it.next()).flatten() {
                let y = ctx.invoke(f, smallvec![x]).await?;
                ok = ctx.vals().bool_payload(y, "Array.all")?;
            }

            Ok(ctx.vals().add(Payload::Bool(ok)))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> Option[T]`
    ///
    /// Returns the first element satisfying a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn find<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.find")?;
            let mut it = xs.iter().copied();
            let mut found = None;

            while let Some(x) = found.is_none().then(|| it.next()).flatten() {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.find")?;
                found = if ok { Some(x) } else { found };
            }

            Ok(match found {
                Some(v) => ctx.vals().option_some(v),
                None => ctx.vals().option_none(),
            })
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> Option[Int]`
    ///
    /// Returns the first index satisfying a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn find_index<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.find-index")?;
            let mut it = xs.iter().copied().enumerate();
            let mut found = None;

            while let Some((idx, x)) =
                found.is_none().then(|| it.next()).flatten()
            {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.find-index")?;
                found = if ok { Some(idx) } else { found };
            }

            match found {
                Some(idx) => {
                    let n = i64::try_from(idx)
                        .unwrap_or_else(|_| invariant!("Array.find-index"));
                    let id = ctx.vals().add(Payload::Int(n));
                    Ok(ctx.vals().option_some(id))
                }
                None => Ok(ctx.vals().option_none()),
            }
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> Array[Int]`
    ///
    /// Returns every index satisfying a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn find_indices<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.find-indices")?;
            let mut it = xs.iter().copied().enumerate();
            let mut acc = SmallVec::new();

            while let Some((idx, x)) = it.next() {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.find-indices")?;
                let id = if ok {
                    let n = i64::try_from(idx).unwrap_or_else(|_| {
                        invariant!("Array.find-indices index")
                    });
                    Some(ctx.vals().add(Payload::Int(n)))
                } else {
                    None
                };
                acc.extend(id);
            }

            Ok(ctx.vals().add(Payload::Array(Arc::new(acc))))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> Array[T]`
    ///
    /// Keeps the longest prefix satisfying a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn take_while<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.take-while")?;
            let mut it = xs.iter().copied().enumerate();
            let mut end = None;

            while let Some((idx, x)) =
                end.is_none().then(|| it.next()).flatten()
            {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.take-while")?;
                end = if ok { end } else { Some(idx) };
            }

            let idx = end.unwrap_or(xs.len());
            let out = xs
                .get(..idx)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.take-while prefix"));

            Ok(ctx.vals().add(Payload::Array(Arc::new(out))))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> Array[T]`
    ///
    /// Drops the longest prefix satisfying a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn drop_while<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.drop-while")?;
            let mut it = xs.iter().copied().enumerate();
            let mut start = None;

            while let Some((idx, x)) =
                start.is_none().then(|| it.next()).flatten()
            {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.drop-while")?;
                start = if ok { start } else { Some(idx) };
            }

            let idx = start.unwrap_or(xs.len());
            let out = xs
                .get(idx..)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.drop-while suffix"));

            Ok(ctx.vals().add(Payload::Array(Arc::new(out))))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])`
    ///
    /// Splits at the first element not satisfying a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn span<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.span")?;
            let mut it = xs.iter().copied().enumerate();
            let mut split = None;

            while let Some((idx, x)) =
                split.is_none().then(|| it.next()).flatten()
            {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.span")?;
                split = if ok { split } else { Some(idx) };
            }

            let idx = split.unwrap_or(xs.len());
            let l = xs
                .get(..idx)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.span prefix"));
            let r = xs
                .get(idx..)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.span suffix"));
            let l_id = ctx.vals().add(Payload::Array(Arc::new(l)));
            let r_id = ctx.vals().add(Payload::Array(Arc::new(r)));

            Ok(ctx
                .vals()
                .add(Payload::Tuple(Arc::new(smallvec![l_id, r_id]))))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])`
    ///
    /// Splits at the first element satisfying a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn break_<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.break")?;
            let mut it = xs.iter().copied().enumerate();
            let mut split = None;

            while let Some((idx, x)) =
                split.is_none().then(|| it.next()).flatten()
            {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.break")?;
                split = if ok { Some(idx) } else { split };
            }

            let idx = split.unwrap_or(xs.len());
            let l = xs
                .get(..idx)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.break prefix"));
            let r = xs
                .get(idx..)
                .map(SmallVec::from_slice)
                .unwrap_or_else(|| invariant!("Array.break suffix"));
            let l_id = ctx.vals().add(Payload::Array(Arc::new(l)));
            let r_id = ctx.vals().add(Payload::Array(Arc::new(r)));

            Ok(ctx
                .vals()
                .add(Payload::Tuple(Arc::new(smallvec![l_id, r_id]))))
        })
    }

    /// `forall T. ((T) -> Bool, Array[T]) -> (Array[T], Array[T])`
    ///
    /// Splits all elements by a callback predicate.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn partition<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.partition")?;
            let mut it = xs.iter().copied();
            let mut yes = SmallVec::new();
            let mut no = SmallVec::new();

            while let Some(x) = it.next() {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ok = ctx.vals().bool_payload(y, "Array.partition")?;
                if ok {
                    yes.push(x);
                } else {
                    no.push(x);
                }
            }

            let l_id = ctx.vals().add(Payload::Array(Arc::new(yes)));
            let r_id = ctx.vals().add(Payload::Array(Arc::new(no)));

            Ok(ctx
                .vals()
                .add(Payload::Tuple(Arc::new(smallvec![l_id, r_id]))))
        })
    }

    /// `forall T U. ((T) -> Array[U], Array[T]) -> Array[U]`
    ///
    /// Maps each element to an array and concatenates the results.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn concat_map<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.concat-map")?;
            let mut it = xs.iter().copied();
            let mut acc = SmallVec::new();

            while let Some(x) = it.next() {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let ys = ctx.vals().array_ids(y, "Array.concat-map")?;
                acc.extend(ys.iter().copied());
            }

            Ok(ctx.vals().add(Payload::Array(Arc::new(acc))))
        })
    }

    /// `forall T U. ((T) -> Option[U], Array[T]) -> Array[U]`
    ///
    /// Maps each element and keeps `Option.Some` callback results.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn map_option<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Array.map-option")?;
            let mut it = xs.iter().copied();
            let mut acc = SmallVec::new();

            while let Some(x) = it.next() {
                let y = ctx.invoke(f, smallvec![x]).await?;
                let item = {
                    let vals = ctx.vals();
                    let v = vals.value(y)?;
                    let ty = vals.value_variant_base_type(v);
                    match &v.payload {
                        Payload::Variant { tag: 0, .. }
                            if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                        {
                            None
                        }
                        Payload::Variant { tag: 1, vals }
                            if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                        {
                            Some(vals.first().copied().unwrap_or_else(|| {
                                typechecked!("Array.map-option", "Option.Some")
                            }))
                        }
                        _ => typechecked!("Array.map-option", "Option"),
                    }
                };
                if let Some(id) = item {
                    acc.push(id);
                }
            }

            Ok(ctx.vals().add(Payload::Array(Arc::new(acc))))
        })
    }

    /// `forall T. ((T) -> T, Array[T], Int) -> Option[Array[T]]`
    ///
    /// Updates an element at a valid index using a callback.
    /// Returns a `BoxFuture` because callback invocation may run `async` user
    /// code.
    pub(crate) fn adjust_at<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let n = ctx.vals().int_payload(args[2], "Array.adjust-at")?;

            if n < 0 {
                Ok(ctx.vals().option_none())
            } else {
                let mut xs = ctx.vals().take_array(a, "Array.adjust-at")?;
                match usize::try_from(n)
                    .ok()
                    .and_then(|idx| xs.get(idx).copied().map(|x| (idx, x)))
                {
                    Some((idx, x)) => {
                        let y = ctx.invoke(f, smallvec![x]).await?;
                        let slot = xs.get_mut(idx).unwrap_or_else(|| {
                            invariant!("Array.adjust-at index")
                        });
                        *slot = y;
                        let arr = ctx.vals().add(Payload::Array(Arc::new(xs)));
                        Ok(ctx.vals().option_some(arr))
                    }
                    None => Ok(ctx.vals().option_none()),
                }
            }
        })
    }

    /// `forall T U. (Array[(T, U)]) -> (Array[T], Array[U])`
    ///
    /// Splits an array of pairs into a pair of arrays.
    pub(crate) fn unzip(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let pairs: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(a, "Array.unzip")?
            .iter()
            .copied()
            .collect();
        let vals: SmallVec<[(ValueId, ValueId); 4]> = pairs
            .into_iter()
            .map(|id| match ctx.vals().payload(id)? {
                Payload::Tuple(elems) => {
                    let l = elems
                        .first()
                        .copied()
                        .unwrap_or_else(|| invariant!("Array.unzip tuple"));
                    let r = elems
                        .get(1)
                        .copied()
                        .unwrap_or_else(|| invariant!("Array.unzip tuple"));
                    Ok((l, r))
                }
                _ => typechecked!("Array.unzip", "(T, U)"),
            })
            .collect::<Result<_>>()?;
        let (firsts, seconds): (
            SmallVec<[ValueId; 4]>,
            SmallVec<[ValueId; 4]>,
        ) = vals.into_iter().unzip();
        let arr_a_id = ctx.vals().add(Payload::Array(Arc::new(firsts)));
        let arr_b_id = ctx.vals().add(Payload::Array(Arc::new(seconds)));

        Ok(ctx
            .vals()
            .add(Payload::Tuple(Arc::new(smallvec![arr_a_id, arr_b_id]))))
    }

    /// `forall T. (T, Array[T]) -> Array[T]`
    ///
    /// Inserts `sep` between each pair of elements.
    pub(crate) fn intersperse(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let elems: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(b, "Array.intersperse")?
            .iter()
            .copied()
            .collect();
        let result = Itertools::intersperse(elems.into_iter(), a).collect();

        Ok(ctx.vals().add(Payload::Array(Arc::new(result))))
    }

    fn idx(n: i64, len: usize) -> usize {
        if n <= 0 {
            0
        } else {
            usize::try_from(n).map_or(len, |i| i.min(len))
        }
    }
}
