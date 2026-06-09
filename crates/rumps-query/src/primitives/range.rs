use std::iter;
use std::sync::Arc;

use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Range;

impl Prim for Range {}

impl Range {
    /// Returns the number of values represented by a `Range`.
    pub(crate) fn len(start: i64, end: i64, inclusive: bool) -> i64 {
        let n = if inclusive {
            start.abs_diff(end).saturating_add(1)
        } else {
            start.abs_diff(end)
        };

        i64::try_from(n).unwrap_or(i64::MAX)
    }

    /// Returns a `Range` payload whose values are reversed.
    pub(crate) fn rev(start: i64, end: i64, inclusive: bool) -> Payload {
        if Self::len(start, end, inclusive) == 0 {
            Payload::Range {
                start,
                end: start,
                inclusive: false,
            }
        } else {
            let last = if inclusive {
                end
            } else if start < end {
                end.saturating_sub(1)
            } else {
                end.saturating_add(1)
            };

            Payload::Range {
                start: last,
                end: start,
                inclusive: true,
            }
        }
    }

    pub(crate) fn vals(
        start: i64,
        end: i64,
        inclusive: bool,
    ) -> impl Iterator<Item = i64> {
        let last = if inclusive {
            Some(end)
        } else if start < end {
            end.checked_sub(1)
        } else if start > end {
            end.checked_add(1)
        } else {
            None
        };

        last.into_iter().flat_map(move |last| {
            let step = if start <= last { 1 } else { -1 };
            iter::successors(Some(start), move |&n| {
                if n == last {
                    None
                } else {
                    Some(n + step)
                }
            })
        })
    }

    /// `(Range) -> Array[Int]`
    ///
    /// Materializes a lazy integer `Range` into an `Array[Int]`.
    pub(crate) fn collect<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let range = ctx
                .arena
                .payload(a)
                .unwrap_or_else(|| typechecked!("Range.collect", "Range"));

            let (start, end, inclusive) = match range {
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => (*start, *end, *inclusive),
                _ => typechecked!("Range.collect", "Range"),
            };

            let elems: SmallVec<[ValueId; 4]> =
                Self::vals(start, end, inclusive)
                    .map(|i| {
                        ctx.arena.add_typed(
                            Payload::Int(i),
                            ctx.runtime_types.meta_int(),
                            ctx.span,
                        )
                    })
                    .collect();

            Ok(ctx.add(Payload::Array(Arc::new(elems))))
        })
    }
}
