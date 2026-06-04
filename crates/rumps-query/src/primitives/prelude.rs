use std::sync::Arc;

use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Prelude;

impl Prim for Prelude {}

impl Prelude {
    /// `forall A. (A) -> A`
    pub(crate) fn identity<'a>(
        _ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move { Ok(args[0]) })
    }

    /// `forall T, F: Iterable. (F[T], T) -> Bool`
    ///
    /// Checks if `needle` is contained in the iterable.
    pub(crate) fn contains<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let haystack = ctx
                .arena
                .payload(args[0])
                .cloned()
                .unwrap_or_else(|| typechecked!("contains", "haystack"));
            let needle = ctx
                .arena
                .payload(args[1])
                .cloned()
                .unwrap_or_else(|| typechecked!("contains", "needle"));
            let found = match &haystack {
                Payload::Array(elems) => elems.iter().any(|eid| {
                    ctx.arena.payload(*eid).is_some_and(|v| *v == needle)
                }),
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => match &needle {
                    Payload::Int(n) => {
                        if *inclusive {
                            *n >= *start && *n <= *end
                        } else {
                            *n >= *start && *n < *end
                        }
                    }
                    _ => false,
                },
                _ => typechecked!("contains", "Iterable"),
            };
            Ok(ctx.arena.add_typed(
                Payload::Bool(found),
                ctx.runtime_types.meta_bool(),
                ctx.span,
            ))
        })
    }

    /// `forall T. (Array[T] | Range) -> Array[T] | Range`
    ///
    /// Reverses an array or range. Range reversal swaps bounds
    /// without materialization.
    pub(crate) fn reverse<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let v = ctx
                .arena
                .payload(args[0])
                .cloned()
                .unwrap_or_else(|| typechecked!("reverse", "value"));
            let res = match v {
                Payload::Array(elems) => {
                    let reversed: SmallVec<[ValueId; 4]> =
                        elems.iter().rev().copied().collect();
                    Payload::Array(Arc::new(reversed))
                }
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => {
                    if inclusive {
                        Payload::Range {
                            start: end,
                            end: start,
                            inclusive: true,
                        }
                    } else {
                        // `start .. end` reversed is `end - 1 ..= start`
                        Payload::Range {
                            start: end - 1,
                            end: start,
                            inclusive: true,
                        }
                    }
                }
                _ => typechecked!("reverse", "Array or Range"),
            };
            Ok(ctx.add(res))
        })
    }
}
