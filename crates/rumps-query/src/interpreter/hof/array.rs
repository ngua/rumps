use std::sync::Arc;
use std::{iter, ops};

use smallvec::{smallvec, SmallVec};

use super::{
    Compare, Continuation, Eq, Registry, ResultMode, SortCmp, SortFrame, State,
    Step,
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
        let contains = i.intern("contains");
        let zip_with = i.intern("zip-with");
        let sort_by = i.intern("sort-by");
        let k = ResultMode::Keep;
        reg.register(array, sort, Self::sort, k);
        reg.register(array, contains, Self::contains, k);
        reg.register(array, zip_with, Self::zip_with, k);
        reg.register(array, sort_by, Self::sort_by, k);
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
}

impl ClassCtx<'_> {
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
            (SortCmp::Ord, Some(v)) => match &v.payload {
                Payload::Int(n) => *n <= 0,
                Payload::Variant { tag, .. }
                    if self
                        .runtime_types
                        .to_type_id(v.repr)
                        .or_else(|| self.runtime_types.to_type_id(v.ty))
                        .is_some_and(|ty| ty == TypeId::ORDERING) =>
                {
                    *tag <= 1
                }
                _ => typechecked!("Array.sort", "Ord:compare result"),
            },
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
