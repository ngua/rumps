//! Higher-order function infrastructure for class methods and module functions.
//!
//! This module provides the continuation/trampoline pattern used by HoFs that
//! need to invoke closures. Both class methods (like `Mappable:map`) and module
//! functions (like `Array.sort-by`) use this infrastructure.
//!
//! # Pattern
//!
//! HoF methods return [`Step`] which is either:
//! - `Done(Payload)`: the method completed synchronously
//! - `DoneValue(ValueId)`: the method completed by forwarding an existing value
//! - `Compare(Compare)`: the method needs `Ord:compare` and resume
//! - `Invoke(Continuation)`: the method needs to call a closure and resume
//!
//! The trampoline loop in `call.rs` handles the async closure invocation:
//! ```text
//! loop {
//!     match result {
//!         Done(v) => return v,
//!         Invoke(cont) => {
//!             let r = invoke_callable(cont.callee, cont.args).await;
//!             result = ctx.resume(cont, r);
//!         }
//!     }
//! }
//! ```

pub(crate) mod array;
mod prelude;
mod registry;
mod result;
mod state;

use std::sync::Arc;

pub(crate) use registry::Registry;
use smallvec::smallvec;
pub(crate) use state::{
    ChainWrapper, Compare, Continuation, IterKind, ResultMode, SortCmp,
    SortFrame, State, Step,
};

use super::class::ClassCtx;
use crate::typecheck::{RuntimeTyId, Ty};
use crate::value::{Payload, ValueId};
use crate::Result;

/// Higher-order function method signature.
pub(crate) type MethodFn = fn(&mut ClassCtx<'_>, &[ValueId]) -> Result<Step>;

impl ClassCtx<'_> {
    fn callable_ret_ty(&self, id: ValueId, label: &str) -> RuntimeTyId {
        self.arena
            .ty(id)
            .and_then(|ty| match self.runtime_types.get(ty) {
                Ty::Fn(_, ret) => Some(RuntimeTyId::from(*ret)),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!(label, "callable metadata"))
    }

    fn result_tys(&self, id: ValueId) -> Option<(RuntimeTyId, RuntimeTyId)> {
        self.arena
            .meta(id)
            .map(|meta| meta.repr)
            .or_else(|| self.arena.ty(id))
            .and_then(|ty| match self.runtime_types.get(ty) {
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
        &mut self,
        cont: Continuation,
        result: ValueId,
    ) -> Result<Step> {
        match cont.state {
            State::MapIter { kind, mut acc, .. } => {
                acc.push(result);
                let IterKind::Array { source, idx } = kind;
                let next_idx = idx + 1;
                match self.arena.payload(source) {
                    Some(Payload::Array(elems)) if next_idx >= elems.len() => {
                        Ok(Step::Done(Payload::Array(Arc::new(acc))))
                    }
                    Some(Payload::Array(elems)) => {
                        Ok(Step::Invoke(Continuation {
                            callee: cont.callee,
                            args: smallvec![elems[next_idx]],
                            state: State::MapIter {
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
            State::MapContainer { ctor_ty: _, tag } => {
                Ok(Step::Done(Payload::Variant {
                    tag,
                    vals: smallvec![result],
                }))
            }
            State::MapTuple { first } => {
                Ok(Step::Done(Payload::Tuple(Arc::new(smallvec![
                    first, result
                ]))))
            }
            State::FilterArray {
                source,
                idx,
                mut acc,
                pending,
            } => {
                // Check if predicate returned true
                let keep = matches!(
                    self.arena.payload(result),
                    Some(Payload::Bool(true))
                );
                if keep {
                    acc.push(pending);
                }
                let next_idx = idx + 1;
                match self.arena.payload(source) {
                    Some(Payload::Array(elems)) if next_idx >= elems.len() => {
                        Ok(Step::Done(Payload::Array(Arc::new(acc))))
                    }
                    Some(Payload::Array(elems)) => {
                        let next_elem = elems[next_idx];
                        Ok(Step::Invoke(Continuation {
                            callee: cont.callee,
                            args: smallvec![next_elem],
                            state: State::FilterArray {
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
            State::ReduceArray {
                source,
                idx,
                acc: _,
            } => {
                let next_idx = idx + 1;
                match self.arena.payload(source) {
                    Some(Payload::Array(elems)) if next_idx >= elems.len() => {
                        Ok(Step::DoneValue(result))
                    }
                    Some(Payload::Array(elems)) => {
                        Ok(Step::Invoke(Continuation {
                            callee: cont.callee,
                            args: smallvec![result, elems[next_idx]],
                            state: State::ReduceArray {
                                source,
                                idx: next_idx,
                                acc: result,
                            },
                        }))
                    }
                    _ => invariant!("ReduceArray source must be Array"),
                }
            }
            State::ReduceRange { current, end, acc } => {
                let _ = acc;
                // `current` is the next value to process.
                if current >= end {
                    Ok(Step::DoneValue(result))
                } else {
                    let int_id = self.arena.add_typed(
                        Payload::Int(current),
                        self.runtime_types.meta_int(),
                        self.span,
                    );
                    Ok(Step::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![result, int_id],
                        state: State::ReduceRange {
                            current: current + 1,
                            end,
                            acc: result,
                        },
                    }))
                }
            }
            State::Chain { wrapper } => match wrapper {
                ChainWrapper::OptionSome | ChainWrapper::ResultOk => {
                    Ok(Step::DoneValue(result))
                }
                ChainWrapper::ResultErr(e) => Ok(Step::Done(Payload::err(e))),
            },
            State::ArrayZipWith {
                arr_a,
                arr_b,
                idx,
                mut acc,
                ..
            } => {
                acc.push(result);
                let next_idx = idx + 1;
                let (elems_a, elems_b) = match (
                    self.arena.payload(arr_a),
                    self.arena.payload(arr_b),
                ) {
                    (Some(Payload::Array(a)), Some(Payload::Array(b))) => {
                        (a.clone(), b.clone())
                    }
                    _ => invariant!("ArrayZipWith sources must be Arrays"),
                };
                if next_idx >= elems_a.len() || next_idx >= elems_b.len() {
                    Ok(Step::Done(Payload::Array(Arc::new(acc))))
                } else {
                    Ok(Step::Invoke(Continuation {
                        callee: cont.callee,
                        args: smallvec![elems_a[next_idx], elems_b[next_idx]],
                        state: State::ArrayZipWith {
                            arr_a,
                            arr_b,
                            idx: next_idx,
                            acc,
                        },
                    }))
                }
            }
            State::PreludeForeachArray { source, idx } => {
                let next_idx = idx + 1;
                match self.arena.payload(source) {
                    Some(Payload::Array(elems)) => match elems.get(next_idx) {
                        Some(next_elem) => Ok(Step::Invoke(Continuation {
                            callee: cont.callee,
                            args: smallvec![*next_elem],
                            state: State::PreludeForeachArray {
                                source,
                                idx: next_idx,
                            },
                        })),
                        None => Ok(Step::Done(Payload::Unit)),
                    },
                    _ => {
                        invariant!("PreludeForeachArray source must be Array")
                    }
                }
            }
            State::PreludeForeachOnce => Ok(Step::Done(Payload::Unit)),
            State::ArraySortBy { source, cmp, stack } => {
                self.resume_sort_by(cmp, source, stack, Some(result))
            }
            State::ResultMapErr { ok_ty } => {
                let err_ty =
                    self.arena.meta(result).map(|m| m.ty).unwrap_or_else(
                        || typechecked!("Result.map-err", "result meta"),
                    );
                let ty = self.runtime_types.result(ok_ty, err_ty);
                let id = self.arena.add_typed(
                    Payload::Variant {
                        tag: 1,
                        vals: smallvec![result],
                    },
                    self.runtime_types.meta(ty),
                    self.span,
                );
                Ok(Step::DoneValue(id))
            }
            State::BimapResult { tag } => Ok(Step::Done(Payload::Variant {
                tag,
                vals: smallvec![result],
            })),
            State::BimapTuple {
                second_fn,
                second_elem,
                first_result: None,
            } => Ok(Step::Invoke(Continuation {
                callee: second_fn,
                args: smallvec![second_elem],
                state: State::BimapTuple {
                    second_fn,
                    second_elem,
                    first_result: Some(result),
                },
            })),
            State::BimapTuple {
                first_result: Some(fst),
                ..
            } => {
                Ok(Step::Done(Payload::Tuple(Arc::new(smallvec![fst, result]))))
            }
        }
    }

    pub(crate) fn resume_compare(
        &mut self,
        state: State,
        result: ValueId,
    ) -> Result<Step> {
        match state {
            State::ArraySortBy { source, cmp, stack } => {
                self.resume_sort_by(cmp, source, stack, Some(result))
            }
            _ => invariant!("compare continuation state"),
        }
    }
}
