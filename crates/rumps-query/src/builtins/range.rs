use std::iter;
use std::sync::Arc;

use smallvec::SmallVec;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, ValueId};
use crate::Result;

pub(crate) struct Range;

impl Body for Range {}

impl Range {
    /// `(Range) -> Array[Int]`
    ///
    /// Materializes a lazy integer `Range` into an `Array[Int]`.
    pub(crate) fn collect(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let (start, end, inc) =
            Self::range_payload(ctx, args[0], "Range.collect")?;
        let elems = Self::vals(start, end, inc)
            .map(|i| ctx.vals().add(Payload::Int(i)))
            .collect();
        Ok(ctx.vals().add(Payload::Array(Arc::new(elems))))
    }

    /// `(Int, Range) -> Bool`
    ///
    /// Returns whether the `Range` contains the given `Int`.
    pub(crate) fn contains(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let n = ctx.vals().int_payload(args[0], "Range.contains")?;
        let (start, end, inc) =
            Self::range_payload(ctx, args[1], "Range.contains")?;
        Ok(ctx
            .vals()
            .add(Payload::Bool(Self::contains_val(n, start, end, inc))))
    }

    /// `(Word, Range) -> Range`
    ///
    /// Widens a `Range` by the given non-negative amount.
    pub(crate) fn extend(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let by = match ctx.vals().payload(args[0])? {
            Payload::Word(by) => *by,
            _ => typechecked!("Range.extend", "Word"),
        };
        let (start, end, inc) =
            Self::range_payload(ctx, args[1], "Range.extend")?;
        Ok(ctx.vals().add(Self::extend_payload(by, start, end, inc)))
    }

    /// `(Range) -> Bool`
    ///
    /// Returns whether a `Range` contains no values.
    pub(crate) fn is_empty(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let (start, end, inc) =
            Self::range_payload(ctx, args[0], "Range.is-empty")?;
        Ok(ctx
            .vals()
            .add(Payload::Bool(Self::len(start, end, inc) == 0)))
    }

    /// `(Range) -> Option[Int]`
    ///
    /// Returns the first value in a `Range`, if any.
    pub(crate) fn first(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let (start, end, inc) =
            Self::range_payload(ctx, args[0], "Range.first")?;
        Ok(match Self::first_val(start, end, inc) {
            Some(n) => {
                let v = ctx.vals().add(Payload::Int(n));
                ctx.vals().option_some(v)
            }
            None => ctx.vals().option_none(),
        })
    }

    /// `(Range) -> Option[Int]`
    ///
    /// Returns the last value in a `Range`, if any.
    pub(crate) fn last(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let (start, end, inc) =
            Self::range_payload(ctx, args[0], "Range.last")?;
        Ok(match Self::last_val(start, end, inc) {
            Some(n) => {
                let v = ctx.vals().add(Payload::Int(n));
                ctx.vals().option_some(v)
            }
            None => ctx.vals().option_none(),
        })
    }
}

impl Range {
    /// Returns the number of values represented by a `Range`.
    pub(crate) fn len(start: i64, end: i64, inc: bool) -> i64 {
        let n = if inc {
            start.abs_diff(end).saturating_add(1)
        } else {
            start.abs_diff(end)
        };

        i64::try_from(n).unwrap_or(i64::MAX)
    }

    /// Returns a `Range` payload whose values are reversed.
    pub(crate) fn rev(start: i64, end: i64, inc: bool) -> Payload {
        if Self::len(start, end, inc) == 0 {
            Payload::Range {
                start,
                end: start,
                inclusive: false,
            }
        } else {
            let last = if inc {
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
        inc: bool,
    ) -> impl Iterator<Item = i64> {
        let last = if inc {
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

    fn range_payload(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<(i64, i64, bool)> {
        match ctx.vals().payload(id)? {
            Payload::Range {
                start,
                end,
                inclusive,
            } => Ok((*start, *end, *inclusive)),
            _ => typechecked!(label, "Range"),
        }
    }

    fn first_val(start: i64, end: i64, inc: bool) -> Option<i64> {
        if Self::len(start, end, inc) == 0 {
            None
        } else {
            Some(start)
        }
    }

    fn last_val(start: i64, end: i64, inc: bool) -> Option<i64> {
        if Self::len(start, end, inc) == 0 {
            None
        } else if inc {
            Some(end)
        } else if start < end {
            Some(end.saturating_sub(1))
        } else {
            Some(end.saturating_add(1))
        }
    }

    fn contains_val(n: i64, start: i64, end: i64, inc: bool) -> bool {
        match (
            Self::first_val(start, end, inc),
            Self::last_val(start, end, inc),
        ) {
            (Some(first), Some(last)) if first <= last => {
                first <= n && n <= last
            }
            (Some(first), Some(last)) => last <= n && n <= first,
            _ => false,
        }
    }

    fn extend_payload(by: usize, start: i64, end: i64, inc: bool) -> Payload {
        let by = i64::try_from(by).unwrap_or(i64::MAX);

        if by == 0 {
            Payload::Range {
                start,
                end,
                inclusive: inc,
            }
        } else {
            match Self::last_val(start, end, inc) {
                None => Payload::Range {
                    start: start.saturating_sub(by),
                    end: start.saturating_add(by),
                    inclusive: true,
                },
                Some(last) if start <= last => Payload::Range {
                    start: start.saturating_sub(by),
                    end: end.saturating_add(by),
                    inclusive: inc,
                },
                Some(_) => Payload::Range {
                    start: start.saturating_add(by),
                    end: end.saturating_sub(by),
                    inclusive: inc,
                },
            }
        }
    }
}
