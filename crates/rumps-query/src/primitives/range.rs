use std::iter;
use std::sync::Arc;

use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Range;

impl Prim for Range {}

// Primitives
impl Range {
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

    /// `(Int, Range) -> Bool`
    ///
    /// Returns whether the `Range` contains the given `Int`.
    pub(crate) fn contains<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let n = match ctx
                .arena
                .payload(a)
                .unwrap_or_else(|| typechecked!("Range.contains", "Int"))
            {
                Payload::Int(n) => *n,
                _ => typechecked!("Range.contains", "Int"),
            };
            let range = ctx
                .arena
                .payload(b)
                .unwrap_or_else(|| typechecked!("Range.contains", "Range"));

            let (start, end, inclusive) = match range {
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => (*start, *end, *inclusive),
                _ => typechecked!("Range.contains", "Range"),
            };

            Ok(ctx.add(Payload::Bool(Self::contains_val(
                n, start, end, inclusive,
            ))))
        })
    }

    /// `(Word, Range) -> Range`
    ///
    /// Widens a `Range` by the given non-negative amount.
    pub(crate) fn extend<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let by = match ctx
                .arena
                .payload(a)
                .unwrap_or_else(|| typechecked!("Range.extend", "Word"))
            {
                Payload::Word(by) => *by,
                _ => typechecked!("Range.extend", "Word"),
            };
            let range = ctx
                .arena
                .payload(b)
                .unwrap_or_else(|| typechecked!("Range.extend", "Range"));

            let (start, end, inclusive) = match range {
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => (*start, *end, *inclusive),
                _ => typechecked!("Range.extend", "Range"),
            };

            Ok(ctx.add(Self::extend_payload(by, start, end, inclusive)))
        })
    }

    /// `(Range) -> Bool`
    ///
    /// Returns whether a `Range` contains no values.
    pub(crate) fn is_empty<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let range = ctx
                .arena
                .payload(a)
                .unwrap_or_else(|| typechecked!("Range.is-empty", "Range"));

            let (start, end, inclusive) = match range {
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => (*start, *end, *inclusive),
                _ => typechecked!("Range.is-empty", "Range"),
            };

            Ok(ctx.add(Payload::Bool(Self::len(start, end, inclusive) == 0)))
        })
    }

    /// `(Range) -> Option[Int]`
    ///
    /// Returns the first value in a `Range`, if any.
    pub(crate) fn first<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let range = ctx
                .arena
                .payload(a)
                .unwrap_or_else(|| typechecked!("Range.first", "Range"));

            let (start, end, inclusive) = match range {
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => (*start, *end, *inclusive),
                _ => typechecked!("Range.first", "Range"),
            };

            Ok(match Self::first_val(start, end, inclusive) {
                Some(n) => {
                    let v = ctx.arena.add_typed(
                        Payload::Int(n),
                        ctx.runtime_types.meta_int(),
                        ctx.span,
                    );
                    ctx.option_some(v)
                }
                None => ctx.option_none(),
            })
        })
    }

    /// `(Range) -> Option[Int]`
    ///
    /// Returns the last value in a `Range`, if any.
    pub(crate) fn last<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let range = ctx
                .arena
                .payload(a)
                .unwrap_or_else(|| typechecked!("Range.last", "Range"));

            let (start, end, inclusive) = match range {
                Payload::Range {
                    start,
                    end,
                    inclusive,
                } => (*start, *end, *inclusive),
                _ => typechecked!("Range.last", "Range"),
            };

            Ok(match Self::last_val(start, end, inclusive) {
                Some(n) => {
                    let v = ctx.arena.add_typed(
                        Payload::Int(n),
                        ctx.runtime_types.meta_int(),
                        ctx.span,
                    );
                    ctx.option_some(v)
                }
                None => ctx.option_none(),
            })
        })
    }
}

// Helpers
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

    fn first_val(start: i64, end: i64, inclusive: bool) -> Option<i64> {
        if Self::len(start, end, inclusive) == 0 {
            None
        } else {
            Some(start)
        }
    }

    fn last_val(start: i64, end: i64, inclusive: bool) -> Option<i64> {
        if Self::len(start, end, inclusive) == 0 {
            None
        } else if inclusive {
            Some(end)
        } else if start < end {
            Some(end.saturating_sub(1))
        } else {
            Some(end.saturating_add(1))
        }
    }

    fn contains_val(n: i64, start: i64, end: i64, inclusive: bool) -> bool {
        match (
            Self::first_val(start, end, inclusive),
            Self::last_val(start, end, inclusive),
        ) {
            (Some(first), Some(last)) if first <= last => {
                first <= n && n <= last
            }
            (Some(first), Some(last)) => last <= n && n <= first,
            _ => false,
        }
    }

    fn extend_payload(
        by: usize,
        start: i64,
        end: i64,
        inclusive: bool,
    ) -> Payload {
        let by = i64::try_from(by).unwrap_or(i64::MAX);

        if by == 0 {
            Payload::Range {
                start,
                end,
                inclusive,
            }
        } else {
            match Self::last_val(start, end, inclusive) {
                None => Payload::Range {
                    start: start.saturating_sub(by),
                    end: start.saturating_add(by),
                    inclusive: true,
                },
                Some(last) if start <= last => Payload::Range {
                    start: start.saturating_sub(by),
                    end: end.saturating_add(by),
                    inclusive,
                },
                Some(_) => Payload::Range {
                    start: start.saturating_add(by),
                    end: end.saturating_sub(by),
                    inclusive,
                },
            }
        }
    }
}
