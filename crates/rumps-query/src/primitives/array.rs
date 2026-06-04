use std::sync::Arc;

use itertools::Itertools;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::typecheck::{RuntimeTyId, RuntimeTypes};
use crate::value::{Payload, TypeId, Value, ValueArena, ValueId};
use crate::Result;

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
            let mut e = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.push", "Array"));

            e.push(args[1]);
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
            let mut e = ctx
                .arena
                .take_array(args[0])
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
            let elems = ctx
                .arena
                .get_array(args[0])
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
            let elems = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.tail", "Array"));

            let tail: SmallVec<[ValueId; 4]> =
                elems.get(1..).map(SmallVec::from_slice).unwrap_or_default();
            Ok(ctx.add(Payload::Array(Arc::new(tail))))
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements sorted in ascending order.
    /// Supports all comparable types including sum types, tuples, and arrays.
    ///
    /// Ordering semantics:
    /// - Scalars: natural ordering (`false < true`, numeric, lexicographic)
    /// - `Option`: `None < Some`
    /// - `Result`: `Err < Ok` (success sorts after failure)
    /// - User-defined sum types: declaration order (variant index)
    /// - Tuples/Arrays: lexicographic
    /// - Objects: lexicographic by (key, value) pairs
    pub(crate) fn sort<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        /// Sort key for `Array.sort`; supports all value types recursively.
        ///
        /// Variant order determines comparison precedence for heterogeneous
        /// arrays (if they were allowed). For homogeneous arrays, only one
        /// variant is used.
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
        enum SortKey<'a> {
            Bool(bool),
            Int(i64),
            Float(OrderedFloat<f64>),
            Char(char),
            String(&'a str),
            /// Variant value: (sort_priority, recursive payloads)
            ///
            /// Priority from `sort_priority`, then recursive payload keys.
            Variant(u8, Vec<Self>),
            Tuple(Vec<Self>),
            Array(Vec<Self>),
            Object(Vec<(&'a str, Self)>),
        }

        impl<'a> SortKey<'a> {
            fn from_value(
                v: &'a Value,
                arena: &'a ValueArena,
                tys: &RuntimeTypes,
            ) -> Option<Self> {
                match &v.payload {
                    Payload::Bool(b) => Some(Self::Bool(*b)),
                    Payload::Int(n) => Some(Self::Int(*n)),
                    // Word sorts as Int (coerced)
                    Payload::Word(n) => Some(Self::Int(*n as i64)),
                    Payload::Float(f) => Some(Self::Float(*f)),
                    Payload::Char(c) => Some(Self::Char(*c)),
                    Payload::String(sid) => {
                        arena.get_str(*sid).map(Self::String)
                    }

                    Payload::Variant { tag, vals } => {
                        let priority =
                            Self::sort_priority(tys, v.repr, v.ty, *tag);
                        let sub_keys: Option<Vec<SortKey<'a>>> = vals
                            .iter()
                            .map(|vid| {
                                arena.value(*vid).and_then(|pv| {
                                    Self::from_value(pv, arena, tys)
                                })
                            })
                            .collect();
                        sub_keys.map(|keys| Self::Variant(priority, keys))
                    }

                    Payload::Tuple(elems) => {
                        let sub_keys: Option<Vec<SortKey<'a>>> = elems
                            .iter()
                            .map(|vid| {
                                arena.value(*vid).and_then(|ev| {
                                    Self::from_value(ev, arena, tys)
                                })
                            })
                            .collect();
                        sub_keys.map(Self::Tuple)
                    }

                    Payload::Array(elems) => {
                        let sub_keys: Option<Vec<SortKey<'a>>> = elems
                            .iter()
                            .map(|vid| {
                                arena.value(*vid).and_then(|ev| {
                                    Self::from_value(ev, arena, tys)
                                })
                            })
                            .collect();
                        sub_keys.map(Self::Array)
                    }

                    Payload::Object(map) => {
                        let sub_keys: Option<Vec<(&'a str, SortKey<'a>)>> = map
                            .iter()
                            .map(|(k, vid)| {
                                arena.get_str(*k).and_then(|key_str| {
                                    arena.value(*vid).and_then(|val| {
                                        Self::from_value(val, arena, tys)
                                            .map(|sk| (key_str, sk))
                                    })
                                })
                            })
                            .collect();
                        sub_keys.map(Self::Object)
                    }

                    // Maps, times, and JSON are not directly comparable for sorting
                    Payload::Map(_) | Payload::Time(_) | Payload::Json(_) => {
                        None
                    }

                    // Unit, closures, functions, module fns/consts, ranges, paths, regex,
                    // refs, and loop continuations are not comparable
                    Payload::Unit
                    | Payload::FilePath(_)
                    | Payload::Regex(_)
                    | Payload::VariantCtor { .. }
                    | Payload::Closure { .. }
                    | Payload::Function { .. }
                    | Payload::ModuleFn { .. }
                    | Payload::ModuleConst { .. }
                    | Payload::Range { .. }
                    | Payload::LoopContinuation
                    | Payload::LoopContinue(_)
                    | Payload::Ref(..)
                    | Payload::ClassMethodFn { .. }
                    | Payload::PartialApp { .. } => None,
                }
            }

            /// Compute sort priority for tagged values.
            ///
            /// For Option and Result, we want `Some > None` and `Ok > Err`
            /// semantically. Declaration order is `None, Some` and `Ok, Err`,
            /// so Option already sorts correctly but Result needs inversion.
            fn sort_priority(
                tys: &RuntimeTypes,
                repr: RuntimeTyId,
                ty: RuntimeTyId,
                idx: u8,
            ) -> u8 {
                let type_id =
                    tys.to_type_id(repr).or_else(|| tys.to_type_id(ty));
                if type_id.is_some_and(|id| id == TypeId::RESULT) {
                    1 - idx
                } else {
                    idx
                }
            }
        }

        Box::pin(async move {
            let elems = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.sort", "Array"));

            // Collect (ValueId, sortable key) pairs
            let mut pairs: Vec<(ValueId, SortKey)> = elems
                .iter()
                .map(|vid| {
                    // Type checker guarantees array elements are comparable
                    ctx.arena
                        .value(*vid)
                        .ok_or_else(|| {
                            ctx.runtime_error("Array.sort: invalid element")
                        })
                        .map(|v| {
                            let k = SortKey::from_value(
                                v,
                                ctx.arena,
                                ctx.runtime_types,
                            )
                            .unwrap_or_else(|| {
                                typechecked!("Array.sort", "Comparable")
                            });
                            (*vid, k)
                        })
                })
                .collect::<Result<Vec<_>>>()?;

            pairs.sort_by(|(_, a), (_, b)| a.cmp(b));

            let sorted: SmallVec<[ValueId; 4]> =
                pairs.into_iter().map(|(vid, _)| vid).collect();
            Ok(ctx.add(Payload::Array(Arc::new(sorted))))
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
            let elems = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.slice", "Array"));

            let start = ctx
                .arena
                .payload(args[1])
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    typechecked!("Array.slice", "start must be Int")
                });

            let end = ctx
                .arena
                .payload(args[2])
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
            let mut combined = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.concat", "Array"));

            let elems_b = ctx
                .arena
                .get_array(args[1])
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
            let elems_a = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.zip", "Array"));

            let elems_b = ctx
                .arena
                .get_array(args[1])
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
            let pairs = ctx
                .arena
                .get_array(args[0])
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
            let sep = args[0];
            let elems = ctx
                .arena
                .get_array(args[1])
                .unwrap_or_else(|| typechecked!("Array.intersperse", "Array"));

            let result: SmallVec<[ValueId; 4]> =
                Itertools::intersperse(elems.iter().copied(), sep).collect();

            Ok(ctx.add(Payload::Array(Arc::new(result))))
        })
    }
}
