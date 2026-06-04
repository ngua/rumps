use std::sync::Arc;
use std::{iter, ops};

use smallvec::{smallvec, SmallVec};

use super::{Continuation, Registry, ResultMode, SortFrame, State, Step};
use crate::intern::StringInterner;
use crate::interpreter::class::ClassCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

/// HoF starters for `Array` module functions.
pub(super) struct Fns;

impl Fns {
    pub(super) fn register(reg: &mut Registry, i: &mut StringInterner) {
        let array = i.intern("Array");
        let zip_with = i.intern("zip-with");
        let sort_by = i.intern("sort-by");
        let k = ResultMode::Keep;
        reg.register(array, zip_with, Self::zip_with, k);
        reg.register(array, sort_by, Self::sort_by, k);
    }

    /// `Array.zip-with(fn, arr_a, arr_b)` - zips two arrays applying fn to pairs.
    fn zip_with(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let fn_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Array.zip-with", "3 args"));
        let arr_a = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Array.zip-with", "3 args"));
        let arr_b = *args
            .get(2)
            .unwrap_or_else(|| typechecked!("Array.zip-with", "3 args"));

        enum Kind {
            Empty,
            NonEmpty(ValueId, ValueId), // first_a, first_b
            Other,
        }
        let kind = match (ctx.arena.payload(arr_a), ctx.arena.payload(arr_b)) {
            (Some(Payload::Array(a)), Some(Payload::Array(b))) => {
                match (a.first(), b.first()) {
                    (Some(&first_a), Some(&first_b)) => {
                        Kind::NonEmpty(first_a, first_b)
                    }
                    _ => Kind::Empty,
                }
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Empty => {
                Ok(Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Kind::NonEmpty(first_a, first_b) => {
                Ok(Step::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![first_a, first_b],
                    state: State::ArrayZipWith {
                        arr_a,
                        arr_b,
                        idx: 0,
                        acc: SmallVec::new(),
                    },
                }))
            }
            Kind::Other => typechecked!("Array.zip-with", "Arrays"),
        }
    }

    /// `Array.sort-by(cmp_fn, arr)` - sorts array using comparison function.
    ///
    /// Uses stack-based merge sort to avoid recursion.
    fn sort_by(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let cmp_fn = *args
            .first()
            .unwrap_or_else(|| typechecked!("Array.sort-by", "2 args"));
        let arr = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Array.sort-by", "2 args"));

        match ctx.arena.payload(arr) {
            Some(Payload::Array(elems)) if elems.len() <= 1 => {
                // Already sorted
                Ok(Step::Done(Payload::Array(elems.clone())))
            }
            Some(Payload::Array(elems)) => {
                let len = elems.len();
                // Kick off merge sort via resume_sort_by with no comparison result
                ctx.resume_sort_by(
                    cmp_fn,
                    arr,
                    vec![SortFrame::Sort { lo: 0, hi: len }],
                    None,
                )
            }
            _ => typechecked!("Array.sort-by", "Array"),
        }
    }
}

impl ClassCtx<'_> {
    /// Resume or advance sort-by algorithm.
    ///
    /// This implements a stack-based merge sort. The algorithm advances until
    /// it needs a comparison (returns `Invoke`) or is done (returns `Done`).
    pub(super) fn resume_sort_by(
        &mut self,
        cmp_fn: ValueId,
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
                    // Check comparison result (expecting Ordering value)
                    let take_left = match self.arena.value(result) {
                        Some(v)
                            if self
                                .runtime_types
                                .to_type_id(v.repr)
                                .or_else(|| self.runtime_types.to_type_id(v.ty))
                                .is_some_and(|ty| ty == TypeId::ORDERING) =>
                        {
                            match &v.payload {
                                Payload::Variant { tag, .. } => *tag <= 1,
                                _ => true,
                            }
                        }
                        _ => true, // Default to left on unexpected
                    };
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
                Ok(Step::Invoke(Continuation {
                    callee: cmp_fn,
                    args: smallvec![a, b],
                    state: State::ArraySortBy { source, stack },
                }))
            }
            ops::ControlFlow::Continue(()) => {
                invariant!("Sort driver terminated")
            }
        }
    }
}
