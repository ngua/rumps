//! Higher-order function infrastructure for class methods and module functions.
//!
//! This module provides the continuation/trampoline pattern used by HoFs that
//! need to invoke closures. Both class methods (like `Mappable:map`) and module
//! functions (like `Option.map`, `Array.sort-by`) use this infrastructure.
//!
//! # Pattern
//!
//! HoF methods return [`MethodResult`] which is either:
//! - `Done(Payload)`: the method completed synchronously
//! - `DoneValue(ValueId)`: the method completed by forwarding an existing value
//! - `Invoke(Continuation)`: the method needs to call a closure and resume
//!
//! The trampoline loop in `call.rs` handles the async closure invocation:
//! ```text
//! loop {
//!     match result {
//!         Done(v) => return v,
//!         Invoke(cont) => {
//!             let r = invoke_callable(cont.callee, cont.args).await;
//!             result = resume(cont, r);
//!         }
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use smallvec::{smallvec, SmallVec};

use super::class::ClassCtx;
use crate::intern::{StringId, StringInterner};
use crate::typecheck::{RuntimeTyId, Ty};
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

/// Result from a HoF method that may need closure invocation.
pub(crate) enum MethodResult {
    /// Method completed synchronously with a value.
    Done(Payload),
    /// Method completed by forwarding a value already stored in the arena.
    DoneValue(ValueId),
    /// Method needs to invoke a callable and continue.
    Invoke(Continuation),
}

/// Continuation for HoF methods; uses zero-copy index tracking.
pub(crate) struct Continuation {
    /// The callable to invoke.
    pub(crate) callee: ValueId,
    /// Arguments to pass to the callable.
    pub(crate) args: SmallVec<[ValueId; 2]>,
    /// HoF-specific state for resumption.
    pub(crate) state: HofState,
}

/// Iteration kind for iterable HoFs.
pub(crate) enum IterKind {
    Array { source: ValueId, idx: usize },
}

/// HoF-specific state for resumption after closure invocation.
pub(crate) enum HofState {
    /// `Mappable:map` over `Array`.
    MapIter {
        kind: IterKind,
        acc: SmallVec<[ValueId; 4]>,
    },
    /// `Mappable:map` over single-value containers (Option.Some, Result.Ok).
    MapContainer {
        /// Type constructor (`OPTION` or `RESULT`).
        ctor_ty: TypeId,
        /// Variant tag (`1` for Some, `0` for Ok).
        tag: u8,
    },
    /// `Option.map` over `Some`; wraps the result back into `Option`.
    MapOption,
    /// `Result.map` over `Ok`; wraps the result back into `Result.Ok`.
    MapResult {
        /// Original `Err` type.
        err_ty: RuntimeTyId,
    },
    /// `Filterable:filter` over array.
    FilterArray {
        source: ValueId,
        idx: usize,
        acc: SmallVec<[ValueId; 4]>,
        /// Last element tested (to add to `acc` if predicate was true).
        pending: ValueId,
    },
    /// `Foldable:reduce` over array.
    ReduceArray {
        source: ValueId,
        idx: usize,
        acc: ValueId,
    },
    /// `Foldable:reduce` over range.
    ReduceRange {
        current: i64,
        end: i64,
        acc: ValueId,
    },
    /// `Chainable:chain`; single invocation, wraps result.
    Chain { wrapper: ChainWrapper },
    /// `Array.zip-with`.
    ZipWith {
        arr_a: ValueId,
        arr_b: ValueId,
        idx: usize,
        acc: SmallVec<[ValueId; 4]>,
    },
    /// `Prelude.foreach` over an array; discards each invocation result.
    ForeachArray { source: ValueId, idx: usize },
    /// `Prelude.foreach` over a single-value container.
    ForeachOnce,
    /// `Array.sort-by` merge sort; stack-based to avoid recursion.
    SortBy {
        source: ValueId,
        stack: Vec<SortFrame>,
    },
    /// `Result.map-err`; wraps mapped error back into `Result.Err`.
    ResultMapErr {
        /// Original `Ok` type.
        ok_ty: RuntimeTyId,
    },
    /// `Bimappable:bimap` over a 2-element container (tuple).
    BimapTuple {
        /// Second function to apply (`g`).
        second_fn: ValueId,
        /// Second element to transform (`b`).
        second_elem: ValueId,
        /// First result (after `f(a)` completes); `None` = awaiting first call.
        first_result: Option<ValueId>,
    },
    /// `Bimappable:bimap` over `Result` (single invocation).
    BimapResult {
        /// Which variant: `0` = Ok, `1` = Err.
        tag: u8,
    },
}

/// Wrapper kind for `Chainable:chain` result.
pub(crate) enum ChainWrapper {
    OptionSome,
    ResultOk,
    ResultErr(ValueId),
}

/// Stack frame for merge sort (replaces recursion).
pub(crate) enum SortFrame {
    /// Need to sort `[lo..hi)` of source array.
    Sort { lo: usize, hi: usize },
    /// Left half sorted; need to sort right half, then merge.
    MergeAfterRight {
        left: SmallVec<[ValueId; 4]>,
        lo: usize,
        hi: usize,
    },
    /// Both halves sorted; merge them.
    Merge {
        left: SmallVec<[ValueId; 4]>,
        right: SmallVec<[ValueId; 4]>,
        li: usize,
        ri: usize,
        merged: SmallVec<[ValueId; 4]>,
    },
}

/// Higher-order function method signature.
///
/// Takes args and returns either a final value or a continuation requesting
/// closure invocation. The trampoline loop handles async invocation.
pub(crate) type HofMethodFn =
    fn(&mut ClassCtx<'_>, &[ValueId]) -> Result<MethodResult>;

fn callable_ret_ty(
    ctx: &ClassCtx<'_>,
    id: ValueId,
    label: &str,
) -> RuntimeTyId {
    ctx.arena
        .ty(id)
        .and_then(|ty| match ctx.runtime_types.get(ty) {
            Ty::Fn(_, ret) => Some(RuntimeTyId::from(*ret)),
            _ => None,
        })
        .unwrap_or_else(|| typechecked!(label, "callable metadata"))
}

fn result_tys(
    ctx: &ClassCtx<'_>,
    id: ValueId,
) -> Option<(RuntimeTyId, RuntimeTyId)> {
    ctx.arena
        .meta(id)
        .map(|meta| meta.repr)
        .or_else(|| ctx.arena.ty(id))
        .and_then(|ty| match ctx.runtime_types.get(ty) {
            Ty::Result(ok, err) => {
                Some((RuntimeTyId::from(*ok), RuntimeTyId::from(*err)))
            }
            _ => None,
        })
}

/// Resume a HoF continuation with the result of a closure invocation.
///
/// Called by the trampoline loop after each `invoke_callable`.
pub(crate) fn resume(
    ctx: &mut ClassCtx<'_>,
    cont: Continuation,
    result: ValueId,
) -> Result<MethodResult> {
    match cont.state {
        HofState::MapIter { kind, mut acc, .. } => {
            acc.push(result);
            let IterKind::Array { source, idx } = kind;
            let next_idx = idx + 1;
            match ctx.arena.payload(source) {
                Some(Payload::Array(elems)) if next_idx >= elems.len() => {
                    Ok(MethodResult::Done(Payload::Array(Arc::new(acc))))
                }
                Some(Payload::Array(elems)) => {
                    Ok(MethodResult::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![elems[next_idx]],
                        state: HofState::MapIter {
                            kind: IterKind::Array {
                                source,
                                idx: next_idx,
                            },
                            acc,
                        },
                    }))
                }
                _ => invariant!("MapIter Array source must be Array"),
            }
        }
        HofState::MapContainer { ctor_ty: _, tag } => {
            Ok(MethodResult::Done(Payload::Variant {
                tag,
                vals: smallvec![result],
            }))
        }
        HofState::MapOption => {
            let elem =
                ctx.arena.meta(result).map(|m| m.ty).unwrap_or_else(|| {
                    typechecked!("Option.map", "result meta")
                });
            let ty = ctx.runtime_types.option(elem);
            let id = ctx.arena.add_typed(
                Payload::Variant {
                    tag: 1,
                    vals: smallvec![result],
                },
                ctx.runtime_types.meta(ty),
                ctx.span,
            );
            Ok(MethodResult::DoneValue(id))
        }
        HofState::MapResult { err_ty } => {
            let ok_ty =
                ctx.arena.meta(result).map(|m| m.ty).unwrap_or_else(|| {
                    typechecked!("Result.map", "result meta")
                });
            let ty = ctx.runtime_types.result(ok_ty, err_ty);
            let id = ctx.arena.add_typed(
                Payload::Variant {
                    tag: 0,
                    vals: smallvec![result],
                },
                ctx.runtime_types.meta(ty),
                ctx.span,
            );
            Ok(MethodResult::DoneValue(id))
        }
        HofState::FilterArray {
            source,
            idx,
            mut acc,
            pending,
        } => {
            // Check if predicate returned true
            let keep =
                matches!(ctx.arena.payload(result), Some(Payload::Bool(true)));
            if keep {
                acc.push(pending);
            }
            let next_idx = idx + 1;
            match ctx.arena.payload(source) {
                Some(Payload::Array(elems)) if next_idx >= elems.len() => {
                    Ok(MethodResult::Done(Payload::Array(Arc::new(acc))))
                }
                Some(Payload::Array(elems)) => {
                    let next_elem = elems[next_idx];
                    Ok(MethodResult::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![next_elem],
                        state: HofState::FilterArray {
                            source,
                            idx: next_idx,
                            acc,
                            pending: next_elem,
                        },
                    }))
                }
                _ => invariant!("FilterArray source must be Array"),
            }
        }
        HofState::ReduceArray {
            source,
            idx,
            acc: _,
        } => {
            let next_idx = idx + 1;
            match ctx.arena.payload(source) {
                Some(Payload::Array(elems)) if next_idx >= elems.len() => {
                    Ok(MethodResult::DoneValue(result))
                }
                Some(Payload::Array(elems)) => {
                    Ok(MethodResult::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![result, elems[next_idx]],
                        state: HofState::ReduceArray {
                            source,
                            idx: next_idx,
                            acc: result,
                        },
                    }))
                }
                _ => invariant!("ReduceArray source must be Array"),
            }
        }
        HofState::ReduceRange { current, end, acc } => {
            let _ = acc;
            // `current` is the next value to process.
            if current >= end {
                Ok(MethodResult::DoneValue(result))
            } else {
                let int_id = ctx.arena.add_typed(
                    Payload::Int(current),
                    ctx.runtime_types.meta_int(),
                    ctx.span,
                );
                Ok(MethodResult::Invoke(Continuation {
                    callee: cont.callee,
                    args: smallvec![result, int_id],
                    state: HofState::ReduceRange {
                        current: current + 1,
                        end,
                        acc: result,
                    },
                }))
            }
        }
        HofState::Chain { wrapper } => match wrapper {
            ChainWrapper::OptionSome | ChainWrapper::ResultOk => {
                Ok(MethodResult::DoneValue(result))
            }
            ChainWrapper::ResultErr(e) => {
                Ok(MethodResult::Done(Payload::err(e)))
            }
        },
        HofState::ZipWith {
            arr_a,
            arr_b,
            idx,
            mut acc,
            ..
        } => {
            acc.push(result);
            let next_idx = idx + 1;
            let (elems_a, elems_b) =
                match (ctx.arena.payload(arr_a), ctx.arena.payload(arr_b)) {
                    (Some(Payload::Array(a)), Some(Payload::Array(b))) => {
                        (a.clone(), b.clone())
                    }
                    _ => invariant!("ZipWith sources must be Arrays"),
                };
            if next_idx >= elems_a.len() || next_idx >= elems_b.len() {
                Ok(MethodResult::Done(Payload::Array(Arc::new(acc))))
            } else {
                Ok(MethodResult::Invoke(Continuation {
                    callee: cont.callee,
                    args: smallvec![elems_a[next_idx], elems_b[next_idx]],
                    state: HofState::ZipWith {
                        arr_a,
                        arr_b,
                        idx: next_idx,
                        acc,
                    },
                }))
            }
        }
        HofState::ForeachArray { source, idx } => {
            let next_idx = idx + 1;
            match ctx.arena.payload(source) {
                Some(Payload::Array(elems)) => match elems.get(next_idx) {
                    Some(next_elem) => Ok(MethodResult::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![*next_elem],
                        state: HofState::ForeachArray {
                            source,
                            idx: next_idx,
                        },
                    })),
                    None => Ok(MethodResult::Done(Payload::Unit)),
                },
                _ => invariant!("ForeachArray source must be Array"),
            }
        }
        HofState::ForeachOnce => Ok(MethodResult::Done(Payload::Unit)),
        HofState::SortBy { source, stack } => {
            resume_sort_by(ctx, cont.callee, source, stack, Some(result))
        }
        HofState::ResultMapErr { ok_ty } => {
            let err_ty =
                ctx.arena.meta(result).map(|m| m.ty).unwrap_or_else(|| {
                    typechecked!("Result.map-err", "result meta")
                });
            let ty = ctx.runtime_types.result(ok_ty, err_ty);
            let id = ctx.arena.add_typed(
                Payload::Variant {
                    tag: 1,
                    vals: smallvec![result],
                },
                ctx.runtime_types.meta(ty),
                ctx.span,
            );
            Ok(MethodResult::DoneValue(id))
        }
        HofState::BimapResult { tag } => {
            Ok(MethodResult::Done(Payload::Variant {
                tag,
                vals: smallvec![result],
            }))
        }
        HofState::BimapTuple {
            second_fn,
            second_elem,
            first_result: None,
        } => Ok(MethodResult::Invoke(Continuation {
            callee: second_fn,
            args: smallvec![second_elem],
            state: HofState::BimapTuple {
                second_fn,
                second_elem,
                first_result: Some(result),
            },
        })),
        HofState::BimapTuple {
            first_result: Some(fst),
            ..
        } => Ok(MethodResult::Done(Payload::Tuple(Arc::new(smallvec![
            fst, result
        ])))),
    }
}

/// Resume or advance sort-by algorithm.
///
/// This implements a stack-based merge sort. The algorithm advances until
/// it needs a comparison (returns `Invoke`) or is done (returns `Done`).
pub(crate) fn resume_sort_by(
    ctx: &mut ClassCtx<'_>,
    cmp_fn: ValueId,
    source: ValueId,
    mut stack: Vec<SortFrame>,
    cmp_result: Option<ValueId>,
) -> Result<MethodResult> {
    // Get source array elements
    let elems = match ctx.arena.payload(source) {
        Some(Payload::Array(e)) => e.clone(),
        _ => invariant!("SortBy source must be Array"),
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
                let take_left = match ctx.arena.value(result) {
                    Some(v)
                        if ctx
                            .runtime_types
                            .to_type_id(v.repr)
                            .or_else(|| ctx.runtime_types.to_type_id(v.ty))
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

    // Main loop: advance until we need a comparison or are done
    loop {
        // If we have a pending result, propagate it up
        if let Some(sorted) = pending.take() {
            match stack.pop() {
                None => {
                    // Done! Return sorted array
                    break Ok(MethodResult::Done(Payload::Array(Arc::new(
                        sorted,
                    ))));
                }
                Some(SortFrame::MergeAfterRight { left, lo, hi }) => {
                    if left.is_empty() {
                        // This was waiting for left half; now sort right
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
                }
                Some(other) => {
                    // Shouldn't happen
                    stack.push(other);
                    invariant!("Unexpected frame when propagating sort result");
                }
            }
            continue;
        }

        // Process next frame on stack
        match stack.pop() {
            None => {
                // Stack empty with no pending = shouldn't happen
                invariant!("Sort stack empty unexpectedly");
            }
            Some(SortFrame::Sort { lo, hi }) => {
                if hi - lo <= 1 {
                    // Base case
                    pending = Some(elems[lo..hi].iter().copied().collect());
                } else {
                    // Split and sort left first
                    let mid = lo + (hi - lo) / 2;
                    stack.push(SortFrame::MergeAfterRight {
                        left: SmallVec::new(),
                        lo,
                        hi,
                    });
                    stack.push(SortFrame::Sort { lo, hi: mid });
                }
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
                // Need to compare left[li] and right[ri]
                let a = left[li];
                let b = right[ri];
                break Ok(MethodResult::Invoke(Continuation {
                    callee: cmp_fn,
                    args: smallvec![a, b],
                    state: HofState::SortBy {
                        source,
                        stack: {
                            stack.push(SortFrame::Merge {
                                left,
                                right,
                                li,
                                ri,
                                merged,
                            });
                            stack
                        },
                    },
                }));
            }
        }
    }
}

/// Whether to keep or discard a module HoF's result after the trampoline
/// completes. Used to implement functions like `foreach` that delegate to an
/// existing HoF (e.g. `Mappable:map`) but discard the produced value.
#[derive(Clone, Copy)]
pub(crate) enum HofResult {
    /// Return the value produced by the HoF directly.
    Keep,
    /// Discard the result, returning `Unit`.
    Discard,
}

/// Registry for module-level HoFs (e.g., `Option.map`, `Array.sort-by`).
///
/// Keyed by `(module_name, function_name)` pairs as `StringId`s.
pub(crate) struct ModuleHofs {
    fns: HashMap<(StringId, StringId), (HofMethodFn, HofResult)>,
}

impl ModuleHofs {
    pub(crate) fn new(interner: &mut StringInterner) -> Self {
        let mut m = Self {
            fns: HashMap::new(),
        };
        m.register_all(interner);
        m
    }

    fn register(
        &mut self,
        module: StringId,
        name: StringId,
        f: HofMethodFn,
        result: HofResult,
    ) {
        self.fns.insert((module, name), (f, result));
    }

    /// Look up a module HoF by path.
    pub(crate) fn lookup(
        &self,
        path: &[StringId],
    ) -> Option<(HofMethodFn, HofResult)> {
        match path {
            [module, name] => self.fns.get(&(*module, *name)).copied(),
            _ => None,
        }
    }

    fn register_all(&mut self, interner: &mut StringInterner) {
        let option = interner.intern("Option");
        let result = interner.intern("Result");
        let array = interner.intern("Array");
        let prelude = interner.intern("Prelude");
        let map = interner.intern("map");
        let map_err = interner.intern("map-err");
        let zip_with = interner.intern("zip-with");
        let sort_by = interner.intern("sort-by");
        let foreach = interner.intern("foreach");

        let k = HofResult::Keep;
        self.register(option, map, OptionHof::map, k);
        self.register(result, map, ResultHof::map, k);
        self.register(result, map_err, ResultHof::map_err, k);
        self.register(array, zip_with, ArrayHof::zip_with, k);
        self.register(array, sort_by, ArrayHof::sort_by, k);
        self.register(
            prelude,
            foreach,
            PreludeHof::foreach,
            HofResult::Discard,
        );
    }
}

/// HoF starters for `Option` module functions.
pub(crate) struct OptionHof;

impl OptionHof {
    /// `Option.map(opt, fn)` - delegates to `Mappable:map` with swapped args.
    pub(crate) fn map(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let opt = *args
            .first()
            .unwrap_or_else(|| typechecked!("Option.map", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Option.map", "2 args"));

        enum Kind {
            Some(ValueId),
            None(RuntimeTyId),
            Other,
        }

        let out_ty = callable_ret_ty(ctx, fn_id, "Option.map");
        let opt_ty = ctx.arena.meta(opt).and_then(|m| {
            ctx.runtime_types
                .to_type_id(m.repr)
                .or_else(|| ctx.runtime_types.to_type_id(m.ty))
        });
        let kind = match ctx.arena.payload(opt) {
            Some(Payload::Variant { tag: 1, vals })
                if opt_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Kind::Some(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Some has payload")),
                )
            }
            Some(Payload::Variant { tag: 0, .. })
                if opt_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Kind::None(out_ty)
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Some(inner) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: HofState::MapOption,
            })),
            Kind::None(elem) => {
                let ty = ctx.runtime_types.option(elem);
                let id = ctx.arena.add_typed(
                    Payload::none(),
                    ctx.runtime_types.meta(ty),
                    ctx.span,
                );
                Ok(MethodResult::DoneValue(id))
            }
            Kind::Other => typechecked!("Option.map", "Option"),
        }
    }
}

/// HoF starters for `Result` module functions.
pub(crate) struct ResultHof;

impl ResultHof {
    /// `Result.map(res, fn)` - maps the value if Ok, passes through Err.
    pub(crate) fn map(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let res = *args
            .first()
            .unwrap_or_else(|| typechecked!("Result.map", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Result.map", "2 args"));

        enum Kind {
            Ok(ValueId, RuntimeTyId),
            Err(ValueId, RuntimeTyId, RuntimeTyId),
            Other,
        }

        let out_ty = callable_ret_ty(ctx, fn_id, "Result.map");
        let tys = result_tys(ctx, res);
        let res_ty = ctx.arena.meta(res).and_then(|m| {
            ctx.runtime_types
                .to_type_id(m.repr)
                .or_else(|| ctx.runtime_types.to_type_id(m.ty))
        });
        let kind = match (tys, ctx.arena.payload(res)) {
            (Some((_, err_ty)), Some(Payload::Variant { tag: 0, vals }))
                if res_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Ok(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Ok has payload")),
                    err_ty,
                )
            }
            (Some((_, err_ty)), Some(Payload::Variant { tag: 1, vals }))
                if res_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Err(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Err has payload")),
                    out_ty,
                    err_ty,
                )
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Ok(inner, err_ty) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: HofState::MapResult { err_ty },
            })),
            Kind::Err(err, ok_ty, err_ty) => {
                let ty = ctx.runtime_types.result(ok_ty, err_ty);
                let id = ctx.arena.add_typed(
                    Payload::err(err),
                    ctx.runtime_types.meta(ty),
                    ctx.span,
                );
                Ok(MethodResult::DoneValue(id))
            }
            Kind::Other => typechecked!("Result.map", "Result"),
        }
    }

    /// `Result.map-err(res, fn)` - maps the error if Err, passes through Ok.
    pub(crate) fn map_err(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let res = *args
            .first()
            .unwrap_or_else(|| typechecked!("Result.map-err", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Result.map-err", "2 args"));

        enum Kind {
            Ok(ValueId, RuntimeTyId, RuntimeTyId),
            Err(ValueId, RuntimeTyId),
            Other,
        }
        let out_ty = callable_ret_ty(ctx, fn_id, "Result.map-err");
        let tys = result_tys(ctx, res);
        let res_ty = ctx.arena.meta(res).and_then(|m| {
            ctx.runtime_types
                .to_type_id(m.repr)
                .or_else(|| ctx.runtime_types.to_type_id(m.ty))
        });
        let kind = match (tys, ctx.arena.payload(res)) {
            // Result.Ok(v) -> return unchanged
            (Some((ok_ty, _)), Some(Payload::Variant { tag: 0, vals }))
                if res_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Ok(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Ok has payload")),
                    ok_ty,
                    out_ty,
                )
            }
            // Result.Err(e) -> map error
            (Some((ok_ty, _)), Some(Payload::Variant { tag: 1, vals }))
                if res_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                let inner = *vals
                    .first()
                    .unwrap_or_else(|| invariant!("Err has payload"));
                Kind::Err(inner, ok_ty)
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Ok(ok, ok_ty, err_ty) => {
                let ty = ctx.runtime_types.result(ok_ty, err_ty);
                let id = ctx.arena.add_typed(
                    Payload::ok(ok),
                    ctx.runtime_types.meta(ty),
                    ctx.span,
                );
                Ok(MethodResult::DoneValue(id))
            }
            Kind::Err(inner, ok_ty) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: HofState::ResultMapErr { ok_ty },
            })),
            Kind::Other => typechecked!("Result.map-err", "Result"),
        }
    }
}

/// HoF starters for `Prelude` module functions.
pub(crate) struct PreludeHof;

impl PreludeHof {
    /// `Prelude.foreach(fn, src)` - invokes `fn` for effects and returns `Unit`.
    pub(crate) fn foreach(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let fn_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Prelude.foreach", "2 args"));
        let src = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Prelude.foreach", "2 args"));

        enum Kind {
            Empty,
            Array(ValueId),
            Once(ValueId),
            Other,
        }

        let src_ty = ctx.arena.meta(src).and_then(|m| {
            ctx.runtime_types
                .to_type_id(m.repr)
                .or_else(|| ctx.runtime_types.to_type_id(m.ty))
        });
        let kind = match ctx.arena.payload(src) {
            Some(Payload::Array(elems)) => match elems.first() {
                Some(first) => Kind::Array(*first),
                None => Kind::Empty,
            },
            Some(Payload::Variant { tag: 1, vals })
                if src_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Kind::Once(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Some has payload")),
                )
            }
            Some(Payload::Variant { tag: 0, .. })
                if src_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Kind::Empty
            }
            Some(Payload::Variant { tag: 0, vals })
                if src_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Once(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Ok has payload")),
                )
            }
            Some(Payload::Variant { tag: 1, .. })
                if src_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Empty
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Empty => Ok(MethodResult::Done(Payload::Unit)),
            Kind::Array(first) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![first],
                state: HofState::ForeachArray {
                    source: src,
                    idx: 0,
                },
            })),
            Kind::Once(inner) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: HofState::ForeachOnce,
            })),
            Kind::Other => typechecked!("Prelude.foreach", "Mappable"),
        }
    }
}

/// HoF starters for `Array` module functions.
pub(crate) struct ArrayHof;

impl ArrayHof {
    /// `Array.zip-with(fn, arr_a, arr_b)` - zips two arrays applying fn to pairs.
    pub(crate) fn zip_with(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
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
            Kind::Empty => Ok(MethodResult::Done(Payload::Array(Arc::new(
                SmallVec::new(),
            )))),
            Kind::NonEmpty(first_a, first_b) => {
                Ok(MethodResult::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![first_a, first_b],
                    state: HofState::ZipWith {
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
    pub(crate) fn sort_by(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let cmp_fn = *args
            .first()
            .unwrap_or_else(|| typechecked!("Array.sort-by", "2 args"));
        let arr = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Array.sort-by", "2 args"));

        match ctx.arena.payload(arr) {
            Some(Payload::Array(elems)) if elems.len() <= 1 => {
                // Already sorted
                Ok(MethodResult::Done(Payload::Array(elems.clone())))
            }
            Some(Payload::Array(elems)) => {
                let len = elems.len();
                // Kick off merge sort via resume_sort_by with no comparison result
                resume_sort_by(
                    ctx,
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
