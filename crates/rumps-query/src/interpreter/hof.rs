//! Higher-order function infrastructure for class methods and module functions.
//!
//! This module provides the continuation/trampoline pattern used by HoFs that
//! need to invoke closures. Both class methods (like `Mappable:map`) and module
//! functions (like `Option.map`, `Array.sort-by`) use this infrastructure.
//!
//! # Pattern
//!
//! HoF methods return [`MethodResult`] which is either:
//! - `Done(Value)`: the method completed synchronously
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

use smallvec::{smallvec, SmallVec};

use super::class::{ClassCtx, Mappable};
use crate::value::{TypeExprId, TypeId, Value, ValueId};
use crate::Result;

/// Result from a HoF method that may need closure invocation.
pub(crate) enum MethodResult {
    /// Method completed synchronously with a value.
    Done(Value),
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

/// Iteration kind for iterable HoFs (Array vs Range).
pub(crate) enum IterKind {
    Array { source: ValueId, idx: usize },
    Range { current: i64, end: i64 },
}

/// HoF-specific state for resumption after closure invocation.
pub(crate) enum HofState {
    /// `Mappable:map` over iterables (Array, Range).
    MapIter {
        kind: IterKind,
        acc: SmallVec<[ValueId; 4]>,
        elem_ty: Option<TypeId>,
    },
    /// `Mappable:map` over single-value containers (Option.Some, Result.Ok).
    MapContainer {
        /// Type constructor (`OPTION` or `RESULT`).
        ctor_ty: TypeId,
        /// Variant tag (`1` for Some, `0` for Ok).
        tag: u8,
        /// Extra type arg (error type for Result; `None` for Option).
        extra_ty: Option<TypeExprId>,
    },
    /// `Filterable:filter` over array.
    FilterArray {
        source: ValueId,
        idx: usize,
        elem_ty: TypeExprId,
        acc: SmallVec<[ValueId; 4]>,
        /// Last element tested (to add to `acc` if predicate was true).
        pending: ValueId,
    },
    /// `Filterable:filter` over range.
    FilterRange {
        current: i64,
        end: i64,
        elem_ty: TypeExprId,
        acc: SmallVec<[ValueId; 4]>,
        pending: i64,
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
    /// `Iterable:foreach` over array.
    ForeachArray { source: ValueId, idx: usize },
    /// `Iterable:foreach` over range.
    ForeachRange { current: i64, end: i64 },
    /// `Fallible:flat-map`; single invocation, wraps result.
    FlatMap { wrapper: FlatMapWrapper },
    /// `Array.zip-with`.
    ZipWith {
        arr_a: ValueId,
        arr_b: ValueId,
        idx: usize,
        acc: SmallVec<[ValueId; 4]>,
        elem_ty: Option<TypeId>,
    },
    /// `Array.sort-by` merge sort; stack-based to avoid recursion.
    SortBy {
        source: ValueId,
        elem_ty: TypeExprId,
        stack: Vec<SortFrame>,
    },
    /// `Result.map-err`; stores Ok type to reconstruct Result type.
    ResultMapErr { ok_ty: TypeExprId },
}

/// Wrapper kind for `Fallible:flat-map` result.
pub(crate) enum FlatMapWrapper {
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

/// Resume a HoF continuation with the result of a closure invocation.
///
/// Called by the trampoline loop after each `invoke_callable`.
pub(crate) fn resume(
    ctx: &mut ClassCtx<'_>,
    cont: Continuation,
    result: ValueId,
) -> Result<MethodResult> {
    match cont.state {
        HofState::MapIter {
            kind,
            mut acc,
            elem_ty,
        } => {
            let ty = elem_ty
                .or_else(|| ctx.arena.base_type_of(result, ctx.type_exprs));
            acc.push(result);
            match kind {
                IterKind::Array { source, idx } => {
                    let next_idx = idx + 1;
                    match ctx.arena.get(source) {
                        Some(Value::Array(_, elems))
                            if next_idx >= elems.len() =>
                        {
                            let arr_ty = ty
                                .map(|t| ctx.type_exprs.named(t))
                                .unwrap_or_else(|| {
                                    ctx.type_exprs.named(TypeId::UNKNOWN)
                                });
                            Ok(MethodResult::Done(Value::Array(arr_ty, acc)))
                        }
                        Some(Value::Array(_, elems)) => {
                            Ok(MethodResult::Invoke(Continuation {
                                callee: cont.callee,
                                args: smallvec![elems[next_idx]],
                                state: HofState::MapIter {
                                    kind: IterKind::Array {
                                        source,
                                        idx: next_idx,
                                    },
                                    acc,
                                    elem_ty: ty,
                                },
                            }))
                        }
                        _ => invariant!("MapIter Array source must be Array"),
                    }
                }
                IterKind::Range { current, end } => {
                    // `current` is the next value to process.
                    if current >= end {
                        let arr_ty =
                            ty.map(|t| ctx.type_exprs.named(t)).unwrap_or_else(
                                || ctx.type_exprs.named(TypeId::UNKNOWN),
                            );
                        Ok(MethodResult::Done(Value::Array(arr_ty, acc)))
                    } else {
                        let int_id =
                            ctx.arena.add(Value::Int(current), ctx.span);
                        Ok(MethodResult::Invoke(Continuation {
                            callee: cont.callee,
                            args: smallvec![int_id],
                            state: HofState::MapIter {
                                kind: IterKind::Range {
                                    current: current + 1,
                                    end,
                                },
                                acc,
                                elem_ty: ty,
                            },
                        }))
                    }
                }
            }
        }
        HofState::MapContainer {
            ctor_ty,
            tag,
            extra_ty,
        } => {
            let result_base = ctx
                .arena
                .base_type_of(result, ctx.type_exprs)
                .unwrap_or(TypeId::UNKNOWN);
            let result_ty_expr = ctx.type_exprs.named(result_base);
            let container_ty = match extra_ty {
                Some(e) => {
                    ctx.type_exprs.app(ctor_ty, smallvec![result_ty_expr, e])
                }
                None => ctx.type_exprs.app(ctor_ty, smallvec![result_ty_expr]),
            };
            Ok(MethodResult::Done(Value::Tagged(
                container_ty,
                tag,
                smallvec![result],
            )))
        }
        HofState::FilterArray {
            source,
            idx,
            elem_ty,
            mut acc,
            pending,
        } => {
            // Check if predicate returned true
            let keep = matches!(ctx.arena.get(result), Some(Value::Bool(true)));
            if keep {
                acc.push(pending);
            }
            let next_idx = idx + 1;
            match ctx.arena.get(source) {
                Some(Value::Array(_, elems)) if next_idx >= elems.len() => {
                    Ok(MethodResult::Done(Value::Array(elem_ty, acc)))
                }
                Some(Value::Array(_, elems)) => {
                    let next_elem = elems[next_idx];
                    Ok(MethodResult::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![next_elem],
                        state: HofState::FilterArray {
                            source,
                            idx: next_idx,
                            elem_ty,
                            acc,
                            pending: next_elem,
                        },
                    }))
                }
                _ => invariant!("FilterArray source must be Array"),
            }
        }
        HofState::FilterRange {
            current,
            end,
            elem_ty,
            mut acc,
            pending,
        } => {
            let keep = matches!(ctx.arena.get(result), Some(Value::Bool(true)));
            if keep {
                let v = ctx.arena.add(Value::Int(pending), ctx.span);
                acc.push(v);
            }
            // `current` is the next value to process.
            if current >= end {
                Ok(MethodResult::Done(Value::Array(elem_ty, acc)))
            } else {
                let int_id = ctx.arena.add(Value::Int(current), ctx.span);
                Ok(MethodResult::Invoke(Continuation {
                    callee: cont.callee,
                    args: smallvec![int_id],
                    state: HofState::FilterRange {
                        current: current + 1,
                        end,
                        elem_ty,
                        acc,
                        pending: current,
                    },
                }))
            }
        }
        HofState::ReduceArray {
            source,
            idx,
            acc: _,
        } => {
            let next_idx = idx + 1;
            match ctx.arena.get(source) {
                Some(Value::Array(_, elems)) if next_idx >= elems.len() => {
                    let v = ctx
                        .arena
                        .get(result)
                        .cloned()
                        .unwrap_or_else(|| invariant!("result in arena"));
                    Ok(MethodResult::Done(v))
                }
                Some(Value::Array(_, elems)) => {
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
                let v = ctx
                    .arena
                    .get(result)
                    .cloned()
                    .unwrap_or_else(|| invariant!("result in arena"));
                Ok(MethodResult::Done(v))
            } else {
                let int_id = ctx.arena.add(Value::Int(current), ctx.span);
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
        HofState::ForeachArray { source, idx } => {
            let next_idx = idx + 1;
            match ctx.arena.get(source) {
                Some(Value::Array(_, elems)) if next_idx >= elems.len() => {
                    Ok(MethodResult::Done(Value::Unit))
                }
                Some(Value::Array(_, elems)) => {
                    Ok(MethodResult::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![elems[next_idx]],
                        state: HofState::ForeachArray {
                            source,
                            idx: next_idx,
                        },
                    }))
                }
                _ => invariant!("ForeachArray source must be Array"),
            }
        }
        HofState::ForeachRange { current, end } => {
            // `current` is the next value to process.
            if current >= end {
                Ok(MethodResult::Done(Value::Unit))
            } else {
                let int_id = ctx.arena.add(Value::Int(current), ctx.span);
                Ok(MethodResult::Invoke(Continuation {
                    callee: cont.callee,
                    args: smallvec![int_id],
                    state: HofState::ForeachRange {
                        current: current + 1,
                        end,
                    },
                }))
            }
        }
        HofState::FlatMap { wrapper } => {
            let inner = ctx
                .arena
                .get(result)
                .cloned()
                .unwrap_or_else(|| invariant!("result in arena"));
            let v = match (wrapper, &inner) {
                // If result is already None/Err, propagate it
                (FlatMapWrapper::OptionSome, Value::Tagged(ty, 0, _)) => {
                    Value::none(*ty)
                }
                (FlatMapWrapper::OptionSome, _) => inner,
                (FlatMapWrapper::ResultOk, Value::Tagged(_ty, 1, _)) => {
                    inner // Already Err, propagate
                }
                (FlatMapWrapper::ResultOk, _) => inner,
                (FlatMapWrapper::ResultErr(e), _) => {
                    // Original was Err; result doesn't matter, return Err
                    ctx.arena
                        .get(e)
                        .cloned()
                        .unwrap_or_else(|| invariant!("err in arena"))
                }
            };
            Ok(MethodResult::Done(v))
        }
        HofState::ZipWith {
            arr_a,
            arr_b,
            idx,
            mut acc,
            elem_ty,
        } => {
            let ty = elem_ty
                .or_else(|| ctx.arena.base_type_of(result, ctx.type_exprs));
            acc.push(result);
            let next_idx = idx + 1;
            let (elems_a, elems_b) =
                match (ctx.arena.get(arr_a), ctx.arena.get(arr_b)) {
                    (Some(Value::Array(_, a)), Some(Value::Array(_, b))) => {
                        (a.clone(), b.clone())
                    }
                    _ => invariant!("ZipWith sources must be Arrays"),
                };
            if next_idx >= elems_a.len() || next_idx >= elems_b.len() {
                let arr_ty = ty
                    .map(|t| ctx.type_exprs.named(t))
                    .unwrap_or_else(|| ctx.type_exprs.named(TypeId::UNKNOWN));
                Ok(MethodResult::Done(Value::Array(arr_ty, acc)))
            } else {
                Ok(MethodResult::Invoke(Continuation {
                    callee: cont.callee,
                    args: smallvec![elems_a[next_idx], elems_b[next_idx]],
                    state: HofState::ZipWith {
                        arr_a,
                        arr_b,
                        idx: next_idx,
                        acc,
                        elem_ty: ty,
                    },
                }))
            }
        }
        HofState::SortBy {
            source,
            elem_ty,
            stack,
        } => resume_sort_by(
            ctx,
            cont.callee,
            source,
            elem_ty,
            stack,
            Some(result),
        ),
        HofState::ResultMapErr { ok_ty } => {
            // The original was Err; wrap mapped error in Result
            let err_base = ctx
                .arena
                .base_type_of(result, ctx.type_exprs)
                .unwrap_or(TypeId::UNKNOWN);
            let err_ty = ctx.type_exprs.named(err_base);
            let res_ty =
                ctx.type_exprs.app(TypeId::RESULT, smallvec![ok_ty, err_ty]);
            Ok(MethodResult::Done(Value::err(res_ty, result)))
        }
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
    elem_ty: TypeExprId,
    mut stack: Vec<SortFrame>,
    cmp_result: Option<ValueId>,
) -> Result<MethodResult> {
    // Get source array elements
    let elems = match ctx.arena.get(source) {
        Some(Value::Array(_, e)) => e.clone(),
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
                let take_left = match ctx.arena.get(result) {
                    Some(Value::Tagged(_, tag, _)) => *tag <= 1, // Less or Equal
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
                    break Ok(MethodResult::Done(Value::Array(
                        elem_ty, sorted,
                    )));
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
                        elem_ty,
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

/// Registry for module-level HoFs (e.g., `Option.map`, `Array.sort-by`).
///
/// Keyed by `(module_name, function_name)` pairs.
pub(crate) struct ModuleHofs {
    fns: std::collections::HashMap<(&'static str, &'static str), HofMethodFn>,
}

impl ModuleHofs {
    pub(crate) fn new() -> Self {
        let mut m = Self {
            fns: std::collections::HashMap::new(),
        };
        m.register_all();
        m
    }

    fn register(
        &mut self,
        module: &'static str,
        name: &'static str,
        f: HofMethodFn,
    ) {
        self.fns.insert((module, name), f);
    }

    /// Look up a module HoF by path.
    ///
    /// Returns `Some(f)` if `path` matches a registered HoF, `None` otherwise.
    pub(crate) fn lookup(&self, path: &[&str]) -> Option<HofMethodFn> {
        match path {
            [module, name] => self.fns.get(&(*module, *name)).copied(),
            _ => None,
        }
    }

    fn register_all(&mut self) {
        self.register("Option", "map", OptionHof::map);
        self.register("Result", "map", ResultHof::map);
        self.register("Result", "map-err", ResultHof::map_err);
        self.register("Array", "zip-with", ArrayHof::zip_with);
        self.register("Array", "sort-by", ArrayHof::sort_by);
    }
}

impl Default for ModuleHofs {
    fn default() -> Self {
        Self::new()
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
        // Delegate to Mappable::map with swapped argument order
        Mappable::map(ctx, &[fn_id, opt])
    }
}

/// HoF starters for `Result` module functions.
pub(crate) struct ResultHof;

impl ResultHof {
    /// `Result.map(res, fn)` - delegates to `Mappable:map` with swapped args.
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
        // Delegate to Mappable::map with swapped argument order
        Mappable::map(ctx, &[fn_id, res])
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
            Ok(Value),
            Err(ValueId, TypeExprId), // inner, ok_ty
            Other,
        }
        let kind = match ctx.arena.get(res) {
            // Result.Ok(v) -> return unchanged
            Some(v @ Value::Tagged(ty, 0, _))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::RESULT) =>
            {
                Kind::Ok(v.clone())
            }
            // Result.Err(e) -> map error
            Some(Value::Tagged(ty, 1, payloads))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::RESULT) =>
            {
                let inner = *payloads
                    .first()
                    .unwrap_or_else(|| invariant!("Err has payload"));
                // Extract ok type from Result[T, E] for result type
                let ok_ty = ctx
                    .type_exprs
                    .type_args(*ty)
                    .and_then(|args| args.first().copied())
                    .unwrap_or_else(|| ctx.type_exprs.named(TypeId::UNKNOWN));
                Kind::Err(inner, ok_ty)
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Ok(v) => Ok(MethodResult::Done(v)),
            Kind::Err(inner, ok_ty) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: HofState::ResultMapErr { ok_ty },
            })),
            Kind::Other => typechecked!("Result.map-err", "Result"),
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
        let kind = match (ctx.arena.get(arr_a), ctx.arena.get(arr_b)) {
            (Some(Value::Array(_, a)), Some(Value::Array(_, b))) => {
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
                let ty = ctx.type_exprs.named(TypeId::UNKNOWN);
                Ok(MethodResult::Done(Value::Array(ty, SmallVec::new())))
            }
            Kind::NonEmpty(first_a, first_b) => {
                Ok(MethodResult::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![first_a, first_b],
                    state: HofState::ZipWith {
                        arr_a,
                        arr_b,
                        idx: 0,
                        acc: SmallVec::new(),
                        elem_ty: None,
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

        match ctx.arena.get(arr) {
            Some(Value::Array(elem_ty, elems)) if elems.len() <= 1 => {
                // Already sorted
                Ok(MethodResult::Done(Value::Array(*elem_ty, elems.clone())))
            }
            Some(Value::Array(elem_ty, elems)) => {
                let len = elems.len();
                // Kick off merge sort via resume_sort_by with no comparison result
                resume_sort_by(
                    ctx,
                    cmp_fn,
                    arr,
                    *elem_ty,
                    vec![SortFrame::Sort { lo: 0, hi: len }],
                    None,
                )
            }
            _ => typechecked!("Array.sort-by", "Array"),
        }
    }
}
