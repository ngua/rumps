use std::cmp::Ordering;
use std::sync::Arc;
use std::{iter, ops};

use smallvec::{smallvec, SmallVec};

use super::{
    Compare, Continuation, Eq, ExtremaKind, Registry, ResultMode, SortCmp,
    SortFrame, State, Step,
};
use crate::intern::StringInterner;
use crate::interpreter::class::ClassCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

/// HoF starters for `Array` module functions.
pub(super) struct Fns;

impl Fns {
    pub(super) fn register(reg: &mut Registry, i: &mut StringInterner) {
        let array = i.intern("Array");
        let sort = i.intern("sort");
        let minimum = i.intern("minimum");
        let maximum = i.intern("maximum");
        let contains = i.intern("contains");
        let elem_index = i.intern("elem-index");
        let zip_with = i.intern("zip-with");
        let sort_by = i.intern("sort-by");
        let any = i.intern("any");
        let all = i.intern("all");
        let find = i.intern("find");
        let find_index = i.intern("find-index");
        let find_indices = i.intern("find-indices");
        let take_while = i.intern("take-while");
        let drop_while = i.intern("drop-while");
        let span = i.intern("span");
        let break_ = i.intern("break");
        let partition = i.intern("partition");
        let concat_map = i.intern("concat-map");
        let map_option = i.intern("map-option");
        let adjust_at = i.intern("adjust-at");
        let k = ResultMode::Keep;
        reg.register(array, sort, Self::sort, k);
        reg.register(array, minimum, Self::minimum, k);
        reg.register(array, maximum, Self::maximum, k);
        reg.register(array, contains, Self::contains, k);
        reg.register(array, elem_index, Self::elem_index, k);
        reg.register(array, zip_with, Self::zip_with, k);
        reg.register(array, sort_by, Self::sort_by, k);
        reg.register(array, any, Self::any, k);
        reg.register(array, all, Self::all, k);
        reg.register(array, find, Self::find, k);
        reg.register(array, find_index, Self::find_index, k);
        reg.register(array, find_indices, Self::find_indices, k);
        reg.register(array, take_while, Self::take_while, k);
        reg.register(array, drop_while, Self::drop_while, k);
        reg.register(array, span, Self::span, k);
        reg.register(array, break_, Self::break_, k);
        reg.register(array, partition, Self::partition, k);
        reg.register(array, concat_map, Self::concat_map, k);
        reg.register(array, map_option, Self::map_option, k);
        reg.register(array, adjust_at, Self::adjust_at, k);
    }

    /// `Array.sort(arr)`; sorts array using `Ord:compare`.
    fn sort(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let a = args[0];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.len() <= 1 => {
                Ok(Step::Done(Payload::Array(elems.clone())))
            }
            Some(Payload::Array(elems)) => ctx.resume_sort_by(
                SortCmp::Ord,
                a,
                vec![SortFrame::Sort {
                    lo: 0,
                    hi: elems.len(),
                }],
                None,
            ),
            _ => typechecked!("Array.sort", "Array"),
        }
    }

    /// `Array.minimum(arr)`; scans array using `Ord:compare`.
    fn minimum(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        Self::extrema(ctx, args, ExtremaKind::Min, "Array.minimum")
    }

    /// `Array.maximum(arr)`; scans array using `Ord:compare`.
    fn maximum(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        Self::extrema(ctx, args, ExtremaKind::Max, "Array.maximum")
    }

    fn extrema(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
        kind: ExtremaKind,
        label: &'static str,
    ) -> Result<Step> {
        let a = args[0];

        enum Kind {
            Empty,
            One(ValueId),
            Many(ValueId),
            Other,
        }

        let k = match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) => match elems.split_first() {
                None => Kind::Empty,
                Some((best, [])) => Kind::One(*best),
                Some((best, _)) => Kind::Many(*best),
            },
            _ => Kind::Other,
        };

        match k {
            Kind::Empty => Ok(Step::DoneValue(ctx.option_none())),
            Kind::One(best) => Ok(Step::DoneValue(ctx.option_some(best))),
            Kind::Many(best) => ctx.resume_extrema(a, 1, best, kind, None),
            Kind::Other => typechecked!(label, "Array"),
        }
    }

    /// `Array.contains(arr, needle)`; scans with `Eq:eq`.
    fn contains(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let a = args[0];
        let b = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Bool(false)))
            }
            Some(Payload::Array(_)) => ctx.resume_contains(a, b, 0, None),
            _ => typechecked!("Array.contains", "Array"),
        }
    }

    /// `Array.elem-index(arr, needle)`; scans with `Eq:eq`.
    fn elem_index(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let a = args[0];
        let b = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::DoneValue(ctx.option_none()))
            }
            Some(Payload::Array(_)) => ctx.resume_elem_index(a, b, 0, None),
            _ => typechecked!("Array.elem-index", "Array"),
        }
    }

    /// `Array.zip-with(f, a, b)`; zips two arrays applying `f` to pairs.
    fn zip_with(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];
        let b = args[2];

        enum Kind {
            Empty,
            NonEmpty(ValueId, ValueId), // first_a, first_b
            Other,
        }
        let kind = match (ctx.arena.payload(a), ctx.arena.payload(b)) {
            (Some(Payload::Array(a)), Some(Payload::Array(b))) => {
                match (a.first(), b.first()) {
                    (Some(&x), Some(&y)) => Kind::NonEmpty(x, y),
                    _ => Kind::Empty,
                }
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Empty => {
                Ok(Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Kind::NonEmpty(x, y) => Ok(Step::Invoke(Continuation {
                callee: f,
                args: smallvec![x, y],
                state: State::ArrayZipWith {
                    arr_a: a,
                    arr_b: b,
                    idx: 0,
                    acc: SmallVec::new(),
                },
            })),
            Kind::Other => typechecked!("Array.zip-with", "Arrays"),
        }
    }

    /// `Array.sort-by(f, a)`; sorts array using comparison function.
    ///
    /// Uses stack-based merge sort to avoid recursion.
    fn sort_by(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.len() <= 1 => {
                // Already sorted
                Ok(Step::Done(Payload::Array(elems.clone())))
            }
            Some(Payload::Array(elems)) => {
                let len = elems.len();
                // Kick off merge sort via resume_sort_by with no comparison result
                ctx.resume_sort_by(
                    SortCmp::Fn(f),
                    a,
                    vec![SortFrame::Sort { lo: 0, hi: len }],
                    None,
                )
            }
            _ => typechecked!("Array.sort-by", "Array"),
        }
    }

    /// `Array.any(f, a)`; tests whether any element satisfies `f`.
    fn any(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Bool(false)))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.any first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayAny { source: a, idx: 0 },
                }))
            }
            _ => typechecked!("Array.any", "Array"),
        }
    }

    /// `Array.all(f, a)`; tests whether every element satisfies `f`.
    fn all(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Bool(true)))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.all first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayAll { source: a, idx: 0 },
                }))
            }
            _ => typechecked!("Array.all", "Array"),
        }
    }

    /// `Array.find(f, a)`; returns the first element satisfying `f`.
    fn find(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::DoneValue(ctx.option_none()))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.find first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayFind {
                        source: a,
                        idx: 0,
                        pending: first,
                    },
                }))
            }
            _ => typechecked!("Array.find", "Array"),
        }
    }

    /// `Array.find-index(f, a)`; returns the first matching index.
    fn find_index(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::DoneValue(ctx.option_none()))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.find-index first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayFindIndex { source: a, idx: 0 },
                }))
            }
            _ => typechecked!("Array.find-index", "Array"),
        }
    }

    /// `Array.find-indices(f, a)`; returns all matching indices.
    fn find_indices(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.find-indices first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayFindIndices {
                        source: a,
                        idx: 0,
                        acc: SmallVec::new(),
                    },
                }))
            }
            _ => typechecked!("Array.find-indices", "Array"),
        }
    }

    /// `Array.take-while(f, a)`; keeps the satisfying prefix.
    fn take_while(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.take-while first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayTakeWhile { source: a, idx: 0 },
                }))
            }
            _ => typechecked!("Array.take-while", "Array"),
        }
    }

    /// `Array.drop-while(f, a)`; drops the satisfying prefix.
    fn drop_while(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.drop-while first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayDropWhile { source: a, idx: 0 },
                }))
            }
            _ => typechecked!("Array.drop-while", "Array"),
        }
    }

    /// `Array.span(f, a)`; splits at the first failing element.
    fn span(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(ctx.empty_split())
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.span first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArraySpan { source: a, idx: 0 },
                }))
            }
            _ => typechecked!("Array.span", "Array"),
        }
    }

    /// `Array.break(f, a)`; splits at the first satisfying element.
    fn break_(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(ctx.empty_split())
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.break first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayBreak { source: a, idx: 0 },
                }))
            }
            _ => typechecked!("Array.break", "Array"),
        }
    }

    /// `Array.partition(f, a)`; splits elements by predicate.
    fn partition(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(ctx.empty_split())
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.partition first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayPartition {
                        source: a,
                        idx: 0,
                        yes: SmallVec::new(),
                        no: SmallVec::new(),
                        pending: first,
                    },
                }))
            }
            _ => typechecked!("Array.partition", "Array"),
        }
    }

    /// `Array.concat-map(f, a)`; maps and concatenates arrays.
    fn concat_map(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.concat-map first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayConcatMap {
                        source: a,
                        idx: 0,
                        acc: SmallVec::new(),
                    },
                }))
            }
            _ => typechecked!("Array.concat-map", "Array"),
        }
    }

    /// `Array.map-option(f, a)`; maps and keeps `Option.Some` results.
    fn map_option(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Some(Payload::Array(elems)) => {
                let first = elems
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Array.map-option first"));
                Ok(Step::Invoke(Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: State::ArrayMapOption {
                        source: a,
                        idx: 0,
                        acc: SmallVec::new(),
                    },
                }))
            }
            _ => typechecked!("Array.map-option", "Array"),
        }
    }

    /// `Array.adjust-at(f, a, idx)`; updates the element at a valid index.
    fn adjust_at(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = args[0];
        let a = args[1];
        let b = args[2];
        let n = ctx
            .arena
            .payload(b)
            .and_then(|v| match v {
                Payload::Int(n) => Some(*n),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!("Array.adjust-at", "Int"));

        if n < 0 {
            Ok(Step::DoneValue(ctx.option_none()))
        } else {
            let idx = usize::try_from(n).ok();
            match (ctx.arena.payload(a), idx) {
                (Some(Payload::Array(elems)), Some(idx)) => {
                    match elems.get(idx).copied() {
                        Some(elem) => Ok(Step::Invoke(Continuation {
                            callee: f,
                            args: smallvec![elem],
                            state: State::ArrayAdjustAt { source: a, idx },
                        })),
                        None => Ok(Step::DoneValue(ctx.option_none())),
                    }
                }
                (Some(Payload::Array(_)), None) => {
                    Ok(Step::DoneValue(ctx.option_none()))
                }
                _ => typechecked!("Array.adjust-at", "Array"),
            }
        }
    }
}

enum OptionFlow {
    Some(ValueId),
    None,
}

impl ClassCtx<'_> {
    pub(super) fn resume_array_any(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.any");
        if ok {
            Ok(Step::Done(Payload::Bool(true)))
        } else {
            match self.next_elem(source, idx + 1, "Array.any") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArrayAny {
                        source,
                        idx: idx + 1,
                    },
                )),
                None => Ok(Step::Done(Payload::Bool(false))),
            }
        }
    }

    pub(super) fn resume_array_all(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.all");
        if ok {
            match self.next_elem(source, idx + 1, "Array.all") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArrayAll {
                        source,
                        idx: idx + 1,
                    },
                )),
                None => Ok(Step::Done(Payload::Bool(true))),
            }
        } else {
            Ok(Step::Done(Payload::Bool(false)))
        }
    }

    pub(super) fn resume_array_find(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        pending: ValueId,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.find");
        if ok {
            Ok(Step::DoneValue(self.option_some(pending)))
        } else {
            match self.next_elem(source, idx + 1, "Array.find") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArrayFind {
                        source,
                        idx: idx + 1,
                        pending: elem,
                    },
                )),
                None => Ok(Step::DoneValue(self.option_none())),
            }
        }
    }

    pub(super) fn resume_array_find_index(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.find-index");
        if ok {
            let id = self.idx_value(idx, "Array.find-index");
            Ok(Step::DoneValue(self.option_some(id)))
        } else {
            match self.next_elem(source, idx + 1, "Array.find-index") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArrayFindIndex {
                        source,
                        idx: idx + 1,
                    },
                )),
                None => Ok(Step::DoneValue(self.option_none())),
            }
        }
    }

    pub(super) fn resume_array_find_indices(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        mut acc: SmallVec<[ValueId; 4]>,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.find-indices");
        if ok {
            let id = self.idx_value(idx, "Array.find-indices");
            acc.push(id);
        }
        match self.next_elem(source, idx + 1, "Array.find-indices") {
            Some(elem) => Ok(self.invoke_pred(
                f,
                elem,
                State::ArrayFindIndices {
                    source,
                    idx: idx + 1,
                    acc,
                },
            )),
            None => Ok(Step::Done(Payload::Array(Arc::new(acc)))),
        }
    }

    pub(super) fn resume_array_take_while(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.take-while");
        if ok {
            match self.next_elem(source, idx + 1, "Array.take-while") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArrayTakeWhile {
                        source,
                        idx: idx + 1,
                    },
                )),
                None => Ok(self.prefix(source, idx + 1, "Array.take-while")),
            }
        } else {
            Ok(self.prefix(source, idx, "Array.take-while"))
        }
    }

    pub(super) fn resume_array_drop_while(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.drop-while");
        if ok {
            match self.next_elem(source, idx + 1, "Array.drop-while") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArrayDropWhile {
                        source,
                        idx: idx + 1,
                    },
                )),
                None => Ok(self.suffix(source, idx + 1, "Array.drop-while")),
            }
        } else {
            Ok(self.suffix(source, idx, "Array.drop-while"))
        }
    }

    pub(super) fn resume_array_span(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.span");
        if ok {
            match self.next_elem(source, idx + 1, "Array.span") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArraySpan {
                        source,
                        idx: idx + 1,
                    },
                )),
                None => Ok(self.split(source, idx + 1, "Array.span")),
            }
        } else {
            Ok(self.split(source, idx, "Array.span"))
        }
    }

    pub(super) fn resume_array_break(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let ok = self.bool_result(result, "Array.break");
        if ok {
            Ok(self.split(source, idx, "Array.break"))
        } else {
            match self.next_elem(source, idx + 1, "Array.break") {
                Some(elem) => Ok(self.invoke_pred(
                    f,
                    elem,
                    State::ArrayBreak {
                        source,
                        idx: idx + 1,
                    },
                )),
                None => Ok(self.split(source, idx + 1, "Array.break")),
            }
        }
    }

    pub(super) fn resume_array_partition(
        &mut self,
        f: ValueId,
        state: State,
        result: ValueId,
    ) -> Result<Step> {
        let State::ArrayPartition {
            source,
            idx,
            mut yes,
            mut no,
            pending,
        } = state
        else {
            invariant!("Array.partition state")
        };
        let ok = self.bool_result(result, "Array.partition");
        if ok {
            yes.push(pending);
        } else {
            no.push(pending);
        }
        match self.next_elem(source, idx + 1, "Array.partition") {
            Some(elem) => Ok(self.invoke_pred(
                f,
                elem,
                State::ArrayPartition {
                    source,
                    idx: idx + 1,
                    yes,
                    no,
                    pending: elem,
                },
            )),
            None => Ok(self.arr_pair(yes, no)),
        }
    }

    pub(super) fn resume_array_concat_map(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        mut acc: SmallVec<[ValueId; 4]>,
        result: ValueId,
    ) -> Result<Step> {
        let elems = match self.arena.payload(result) {
            Some(Payload::Array(elems)) => elems.clone(),
            Some(_) => typechecked!("Array.concat-map", "Array"),
            None => invariant!("Array.concat-map result in arena"),
        };
        acc.extend(elems.iter().copied());
        match self.next_elem(source, idx + 1, "Array.concat-map") {
            Some(elem) => Ok(self.invoke_pred(
                f,
                elem,
                State::ArrayConcatMap {
                    source,
                    idx: idx + 1,
                    acc,
                },
            )),
            None => Ok(Step::Done(Payload::Array(Arc::new(acc)))),
        }
    }

    pub(super) fn resume_array_map_option(
        &mut self,
        f: ValueId,
        source: ValueId,
        idx: usize,
        mut acc: SmallVec<[ValueId; 4]>,
        result: ValueId,
    ) -> Result<Step> {
        match self.option_result(result, "Array.map-option") {
            OptionFlow::Some(v) => acc.push(v),
            OptionFlow::None => {}
        }
        match self.next_elem(source, idx + 1, "Array.map-option") {
            Some(elem) => Ok(self.invoke_pred(
                f,
                elem,
                State::ArrayMapOption {
                    source,
                    idx: idx + 1,
                    acc,
                },
            )),
            None => Ok(Step::Done(Payload::Array(Arc::new(acc)))),
        }
    }

    pub(super) fn resume_array_adjust_at(
        &mut self,
        source: ValueId,
        idx: usize,
        result: ValueId,
    ) -> Result<Step> {
        let mut elems = self
            .arena
            .take_array(source)
            .unwrap_or_else(|| invariant!("Array.adjust-at source"));
        match elems.get_mut(idx) {
            Some(slot) => {
                *slot = result;
                let arr = self.add(Payload::Array(Arc::new(elems)));
                Ok(Step::DoneValue(self.option_some(arr)))
            }
            None => invariant!("Array.adjust-at index"),
        }
    }

    fn invoke_pred(&self, f: ValueId, elem: ValueId, state: State) -> Step {
        Step::Invoke(Continuation {
            callee: f,
            args: smallvec![elem],
            state,
        })
    }

    fn next_elem(
        &self,
        source: ValueId,
        idx: usize,
        label: &str,
    ) -> Option<ValueId> {
        match self.arena.payload(source) {
            Some(Payload::Array(elems)) => elems.get(idx).copied(),
            _ => invariant!(format!("{label} source must be Array")),
        }
    }

    fn idx_value(&mut self, idx: usize, label: &str) -> ValueId {
        let n = i64::try_from(idx)
            .unwrap_or_else(|_| invariant!(format!("{label} index")));
        self.add(Payload::Int(n))
    }

    fn prefix(&mut self, source: ValueId, idx: usize, label: &str) -> Step {
        let elems = match self.arena.payload(source) {
            Some(Payload::Array(elems)) => elems.clone(),
            _ => invariant!(format!("{label} source must be Array")),
        };
        let out = elems
            .get(..idx)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!(format!("{label} prefix")));
        Step::Done(Payload::Array(Arc::new(out)))
    }

    fn suffix(&mut self, source: ValueId, idx: usize, label: &str) -> Step {
        let elems = match self.arena.payload(source) {
            Some(Payload::Array(elems)) => elems.clone(),
            _ => invariant!(format!("{label} source must be Array")),
        };
        let out = elems
            .get(idx..)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!(format!("{label} suffix")));
        Step::Done(Payload::Array(Arc::new(out)))
    }

    fn split(&mut self, source: ValueId, idx: usize, label: &str) -> Step {
        let elems = match self.arena.payload(source) {
            Some(Payload::Array(elems)) => elems.clone(),
            _ => invariant!(format!("{label} source must be Array")),
        };
        let a = elems
            .get(..idx)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!(format!("{label} prefix")));
        let b = elems
            .get(idx..)
            .map(SmallVec::from_slice)
            .unwrap_or_else(|| invariant!(format!("{label} suffix")));
        self.arr_pair(a, b)
    }

    fn arr_pair(
        &mut self,
        a: SmallVec<[ValueId; 4]>,
        b: SmallVec<[ValueId; 4]>,
    ) -> Step {
        let left = self.add(Payload::Array(Arc::new(a)));
        let right = self.add(Payload::Array(Arc::new(b)));
        Step::Done(Payload::Tuple(Arc::new(smallvec![left, right])))
    }

    fn empty_split(&mut self) -> Step {
        self.arr_pair(SmallVec::new(), SmallVec::new())
    }

    fn option_result(&self, id: ValueId, label: &str) -> OptionFlow {
        let ty = self.arena.meta(id).and_then(|meta| {
            self.runtime_types
                .to_type_id(meta.repr)
                .or_else(|| self.runtime_types.to_type_id(meta.ty))
        });
        match self.arena.payload(id) {
            Some(Payload::Variant { tag: 0, .. })
                if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                OptionFlow::None
            }
            Some(Payload::Variant { tag: 1, vals })
                if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                let v = vals
                    .first()
                    .copied()
                    .unwrap_or_else(|| typechecked!(label, "Option.Some"));
                OptionFlow::Some(v)
            }
            Some(_) => typechecked!(label, "Option"),
            None => invariant!(format!("{label} result in arena")),
        }
    }

    pub(super) fn resume_extrema(
        &mut self,
        source: ValueId,
        idx: usize,
        best: ValueId,
        kind: ExtremaKind,
        cmp_result: Option<ValueId>,
    ) -> Result<Step> {
        let label = match kind {
            ExtremaKind::Min => "Array.minimum",
            ExtremaKind::Max => "Array.maximum",
        };
        let cmp = cmp_result.map(|result| self.cmp_ord_result(result, label));
        let elems = match self.arena.payload(source) {
            Some(Payload::Array(elems)) => elems,
            _ => invariant!("Array extrema source must be Array"),
        };
        let best = match cmp {
            Some(cmp) => {
                let elem = elems
                    .get(idx)
                    .copied()
                    .unwrap_or_else(|| invariant!("Array extrema index"));
                match kind {
                    ExtremaKind::Min if cmp == Ordering::Greater => elem,
                    ExtremaKind::Max if cmp == Ordering::Less => elem,
                    _ => best,
                }
            }
            None => best,
        };
        let next = if cmp_result.is_some() { idx + 1 } else { idx };

        let next_elem = elems.get(next).copied();

        match next_elem {
            Some(elem) => Ok(Step::Compare(Compare {
                args: smallvec![best, elem],
                state: State::ArrayExtrema {
                    source,
                    idx: next,
                    best,
                    kind,
                },
            })),
            None => Ok(Step::DoneValue(self.option_some(best))),
        }
    }

    pub(super) fn resume_contains(
        &mut self,
        source: ValueId,
        needle: ValueId,
        idx: usize,
        eq_result: Option<ValueId>,
    ) -> Result<Step> {
        enum Flow {
            Done(bool),
            Eq { elem: ValueId, idx: usize },
        }

        let elems = match self.arena.payload(source) {
            Some(Payload::Array(elems)) => elems.clone(),
            _ => invariant!("Array.contains source must be Array"),
        };
        let start = match eq_result {
            Some(result) if self.bool_result(result, "Array.contains") => None,
            Some(_) => Some(idx + 1),
            None => Some(idx),
        };

        let flow =
            start.map_or(ops::ControlFlow::Break(Flow::Done(true)), |i| {
                elems
                    .iter()
                    .copied()
                    .enumerate()
                    .skip(i)
                    .try_fold((), |(), (idx, elem)| {
                        match self.hot_eq(elem, needle) {
                            Some(true) => {
                                ops::ControlFlow::Break(Flow::Done(true))
                            }
                            Some(false) => ops::ControlFlow::Continue(()),
                            None => {
                                ops::ControlFlow::Break(Flow::Eq { elem, idx })
                            }
                        }
                    })
                    .map_break(|flow| flow)
                    .map_continue(|()| Flow::Done(false))
            });

        match flow {
            ops::ControlFlow::Break(Flow::Done(found))
            | ops::ControlFlow::Continue(Flow::Done(found)) => {
                Ok(Step::Done(Payload::Bool(found)))
            }
            ops::ControlFlow::Break(Flow::Eq { elem, idx })
            | ops::ControlFlow::Continue(Flow::Eq { elem, idx }) => {
                Ok(Step::Eq(Eq {
                    args: smallvec![elem, needle],
                    state: State::ArrayContains {
                        source,
                        needle,
                        idx,
                    },
                }))
            }
        }
    }

    pub(super) fn resume_elem_index(
        &mut self,
        source: ValueId,
        needle: ValueId,
        idx: usize,
        eq_result: Option<ValueId>,
    ) -> Result<Step> {
        enum Flow {
            Done(Option<usize>),
            Eq { elem: ValueId, idx: usize },
        }

        let elems = match self.arena.payload(source) {
            Some(Payload::Array(elems)) => elems.clone(),
            _ => invariant!("Array.elem-index source must be Array"),
        };
        let start = match eq_result {
            Some(result) if self.bool_result(result, "Array.elem-index") => {
                None
            }
            Some(_) => Some(idx + 1),
            None => Some(idx),
        };

        let flow =
            start.map_or(ops::ControlFlow::Break(Flow::Done(Some(idx))), |i| {
                elems
                    .iter()
                    .copied()
                    .enumerate()
                    .skip(i)
                    .try_fold((), |(), (idx, elem)| {
                        match self.hot_eq(elem, needle) {
                            Some(true) => {
                                ops::ControlFlow::Break(Flow::Done(Some(idx)))
                            }
                            Some(false) => ops::ControlFlow::Continue(()),
                            None => {
                                ops::ControlFlow::Break(Flow::Eq { elem, idx })
                            }
                        }
                    })
                    .map_break(|flow| flow)
                    .map_continue(|()| Flow::Done(None))
            });

        match flow {
            ops::ControlFlow::Break(Flow::Done(Some(i)))
            | ops::ControlFlow::Continue(Flow::Done(Some(i))) => {
                let n = i64::try_from(i)
                    .unwrap_or_else(|_| invariant!("Array.elem-index index"));
                let id = self.add(Payload::Int(n));
                Ok(Step::DoneValue(self.option_some(id)))
            }
            ops::ControlFlow::Break(Flow::Done(None))
            | ops::ControlFlow::Continue(Flow::Done(None)) => {
                Ok(Step::DoneValue(self.option_none()))
            }
            ops::ControlFlow::Break(Flow::Eq { elem, idx })
            | ops::ControlFlow::Continue(Flow::Eq { elem, idx }) => {
                Ok(Step::Eq(Eq {
                    args: smallvec![elem, needle],
                    state: State::ArrayElemIndex {
                        source,
                        needle,
                        idx,
                    },
                }))
            }
        }
    }

    fn hot_eq(&self, l: ValueId, r: ValueId) -> Option<bool> {
        let lv = self.arena.payload(l)?;
        let rv = self.arena.payload(r)?;
        match (lv, rv) {
            (Payload::Unit, Payload::Unit)
                if self.hot_ty(l, TypeId::UNIT)
                    && self.hot_ty(r, TypeId::UNIT) =>
            {
                Some(true)
            }
            (Payload::Bool(a), Payload::Bool(b))
                if self.hot_ty(l, TypeId::BOOL)
                    && self.hot_ty(r, TypeId::BOOL) =>
            {
                Some(a == b)
            }
            (Payload::Int(a), Payload::Int(b))
                if self.hot_ty(l, TypeId::INT)
                    && self.hot_ty(r, TypeId::INT) =>
            {
                Some(a == b)
            }
            (Payload::Word(a), Payload::Word(b))
                if self.hot_ty(l, TypeId::WORD)
                    && self.hot_ty(r, TypeId::WORD) =>
            {
                Some(a == b)
            }
            (Payload::Float(a), Payload::Float(b))
                if self.hot_ty(l, TypeId::FLOAT)
                    && self.hot_ty(r, TypeId::FLOAT) =>
            {
                Some(a == b)
            }
            (Payload::Char(a), Payload::Char(b))
                if self.hot_ty(l, TypeId::CHAR)
                    && self.hot_ty(r, TypeId::CHAR) =>
            {
                Some(a == b)
            }
            (Payload::String(a), Payload::String(b))
                if self.hot_ty(l, TypeId::STRING)
                    && self.hot_ty(r, TypeId::STRING) =>
            {
                Some(a == b)
            }
            (Payload::Time(a), Payload::Time(b))
                if self.hot_ty(l, TypeId::TIME)
                    && self.hot_ty(r, TypeId::TIME) =>
            {
                Some(a == b)
            }
            (Payload::FilePath(a), Payload::FilePath(b))
                if self.hot_ty(l, TypeId::FILEPATH)
                    && self.hot_ty(r, TypeId::FILEPATH) =>
            {
                Some(a == b)
            }
            (Payload::Json(a), Payload::Json(b))
                if self.hot_ty(l, TypeId::JSON)
                    && self.hot_ty(r, TypeId::JSON) =>
            {
                Some(a == b)
            }
            (
                Payload::Variant { tag: a, vals: av },
                Payload::Variant { tag: b, vals: bv },
            ) if self.hot_ty(l, TypeId::ORDERING)
                && self.hot_ty(r, TypeId::ORDERING) =>
            {
                Some(a == b && av.is_empty() && bv.is_empty())
            }
            (
                Payload::Ref(a_global, a_name, a_subs),
                Payload::Ref(b_global, b_name, b_subs),
            ) if self.hot_ref_ty(l, r) => {
                let same_ref = a_global == b_global
                    && a_name == b_name
                    && a_subs.len() == b_subs.len();
                if same_ref {
                    a_subs
                        .iter()
                        .zip(b_subs.iter())
                        .map(|(a, b)| self.hot_eq(*a, *b))
                        .try_fold(true, |acc, eq| match (acc, eq) {
                            (false, _) => Some(false),
                            (_, Some(true)) => Some(true),
                            (_, Some(false)) => Some(false),
                            (_, None) => None,
                        })
                } else {
                    Some(false)
                }
            }
            _ => None,
        }
    }

    fn hot_ty(&self, id: ValueId, ty: TypeId) -> bool {
        self.actual_ty(id) == Some(ty)
    }

    fn hot_ref_ty(&self, l: ValueId, r: ValueId) -> bool {
        matches!(
            (self.actual_ty(l), self.actual_ty(r)),
            (Some(TypeId::LOCAL), Some(TypeId::LOCAL))
                | (Some(TypeId::GLOBAL), Some(TypeId::GLOBAL))
        )
    }

    fn actual_ty(&self, id: ValueId) -> Option<TypeId> {
        self.arena
            .meta(id)
            .and_then(|meta| self.runtime_types.to_type_id(meta.ty))
    }

    fn cmp_ord_result(&self, result: ValueId, label: &str) -> Ordering {
        match self.arena.value(result) {
            Some(v) => match &v.payload {
                Payload::Int(n) => n.cmp(&0),
                Payload::Variant { tag, .. }
                    if self
                        .runtime_types
                        .to_type_id(v.repr)
                        .or_else(|| self.runtime_types.to_type_id(v.ty))
                        .is_some_and(|ty| ty == TypeId::ORDERING) =>
                {
                    match tag {
                        0 => Ordering::Less,
                        1 => Ordering::Equal,
                        2 => Ordering::Greater,
                        _ => typechecked!(label, "Ord:compare result"),
                    }
                }
                _ => typechecked!(label, "Ord:compare result"),
            },
            None => invariant!("comparison result in arena"),
        }
    }

    /// Resume or advance sort-by algorithm.
    ///
    /// This implements a stack-based merge sort. The algorithm advances until
    /// it needs a comparison (returns `Invoke`) or is done (returns `Done`).
    pub(super) fn resume_sort_by(
        &mut self,
        cmp: SortCmp,
        source: ValueId,
        mut stack: Vec<SortFrame>,
        cmp_result: Option<ValueId>,
    ) -> Result<Step> {
        enum Flow {
            Done(SmallVec<[ValueId; 4]>),
            Invoke { a: ValueId, b: ValueId },
        }

        // Get source array elements
        let elems = match self.arena.payload(source) {
            Some(Payload::Array(e)) => e.clone(),
            _ => invariant!("ArraySortBy source must be Array"),
        };

        // Handle comparison result from previous step
        let mut pending: Option<SmallVec<[ValueId; 4]>> = None;

        if let Some(result) = cmp_result {
            // We were in a Merge; process the comparison result
            match stack.pop() {
                Some(SortFrame::Merge {
                    left,
                    right,
                    li,
                    ri,
                    mut merged,
                }) => {
                    let take_left = self.sort_take_left(cmp, result);
                    if take_left {
                        merged.push(left[li]);
                        let new_li = li + 1;
                        if new_li >= left.len() {
                            // Left exhausted; append rest of right
                            merged.extend(right[ri..].iter().copied());
                            pending = Some(merged);
                        } else {
                            stack.push(SortFrame::Merge {
                                left,
                                right,
                                li: new_li,
                                ri,
                                merged,
                            });
                        }
                    } else {
                        merged.push(right[ri]);
                        let new_ri = ri + 1;
                        if new_ri >= right.len() {
                            // Right exhausted; append rest of left
                            merged.extend(left[li..].iter().copied());
                            pending = Some(merged);
                        } else {
                            stack.push(SortFrame::Merge {
                                left,
                                right,
                                li,
                                ri: new_ri,
                                merged,
                            });
                        }
                    }
                }
                other => {
                    // Put it back; we'll process below
                    if let Some(f) = other {
                        stack.push(f);
                    }
                }
            }
        }

        let flow = iter::repeat(()).try_fold((), |_, _| {
            if let Some(sorted) = pending.take() {
                // If we have a pending result, propagate it up
                match stack.pop() {
                    None => ops::ControlFlow::Break(Flow::Done(sorted)),
                    Some(SortFrame::MergeAfterRight { left, lo, hi }) => {
                        if left.is_empty() {
                            let mid = lo + (hi - lo) / 2;
                            stack.push(SortFrame::MergeAfterRight {
                                left: sorted,
                                lo,
                                hi,
                            });
                            stack.push(SortFrame::Sort { lo: mid, hi });
                        } else {
                            // We have both halves; start merge
                            stack.push(SortFrame::Merge {
                                left,
                                right: sorted,
                                li: 0,
                                ri: 0,
                                merged: SmallVec::new(),
                            });
                        }
                        ops::ControlFlow::Continue(())
                    }
                    Some(other) => {
                        // Shouldn't happen
                        stack.push(other);
                        invariant!(
                            "Unexpected frame when propagating sort result"
                        );
                    }
                }
            } else {
                // Process next frame on stack
                match stack.pop() {
                    None => {
                        // Stack empty with no pending = shouldn't happen
                        invariant!("Sort stack empty unexpectedly");
                    }
                    Some(SortFrame::Sort { lo, hi }) => {
                        if hi - lo <= 1 {
                            pending =
                                Some(elems[lo..hi].iter().copied().collect());
                        } else {
                            let mid = lo + (hi - lo) / 2;
                            stack.push(SortFrame::MergeAfterRight {
                                left: SmallVec::new(),
                                lo,
                                hi,
                            });
                            stack.push(SortFrame::Sort { lo, hi: mid });
                        }
                        ops::ControlFlow::Continue(())
                    }
                    Some(SortFrame::MergeAfterRight { left, lo, hi }) => {
                        // This shouldn't be on top without a pending result
                        stack.push(SortFrame::MergeAfterRight { left, lo, hi });
                        invariant!("MergeAfterRight without pending result");
                    }
                    Some(SortFrame::Merge {
                        left,
                        right,
                        li,
                        ri,
                        merged,
                    }) => {
                        let a = left[li];
                        let b = right[ri];
                        stack.push(SortFrame::Merge {
                            left,
                            right,
                            li,
                            ri,
                            merged,
                        });
                        ops::ControlFlow::Break(Flow::Invoke { a, b })
                    }
                }
            }
        });

        match flow {
            ops::ControlFlow::Break(Flow::Done(sorted)) => {
                Ok(Step::Done(Payload::Array(Arc::new(sorted))))
            }
            ops::ControlFlow::Break(Flow::Invoke { a, b }) => {
                let args = smallvec![a, b];
                match cmp {
                    SortCmp::Ord => Ok(Step::Compare(Compare {
                        args,
                        state: State::ArraySortBy { source, cmp, stack },
                    })),
                    SortCmp::Fn(callee) => Ok(Step::Invoke(Continuation {
                        callee,
                        args,
                        state: State::ArraySortBy { source, cmp, stack },
                    })),
                }
            }
            ops::ControlFlow::Continue(()) => {
                invariant!("Sort driver terminated")
            }
        }
    }

    fn sort_take_left(&self, cmp: SortCmp, result: ValueId) -> bool {
        match (cmp, self.arena.value(result)) {
            (SortCmp::Ord, Some(_)) => {
                self.cmp_ord_result(result, "Array.sort") != Ordering::Greater
            }
            (SortCmp::Fn(_), Some(v))
                if matches!(v.payload, Payload::Int(_)) =>
            {
                match &v.payload {
                    Payload::Int(n) => *n <= 0,
                    _ => invariant!("matched Int payload"),
                }
            }
            (SortCmp::Fn(_), Some(v))
                if self
                    .runtime_types
                    .to_type_id(v.repr)
                    .or_else(|| self.runtime_types.to_type_id(v.ty))
                    .is_some_and(|ty| ty == TypeId::ORDERING) =>
            {
                match &v.payload {
                    Payload::Variant { tag, .. } => *tag <= 1,
                    _ => typechecked!("Array.sort-by", "Ordering"),
                }
            }
            (SortCmp::Fn(_), Some(_)) => {
                typechecked!("Array.sort-by", "Ordering")
            }
            (_, None) => invariant!("sort comparison result in arena"),
        }
    }
}
