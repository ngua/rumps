use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Time;

impl Prim for Time {}

impl Time {
    /// `() -> Time`
    ///
    /// Returns the current UTC time.
    pub(crate) fn now<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let now = Utc::now();
            Ok(ctx.arena.add_typed(
                Payload::Time(now),
                ctx.runtime_types.meta_time(),
                ctx.span,
            ))
        })
    }

    /// `() -> Time`
    ///
    /// Returns the Unix epoch (1970-01-01 00:00:00 UTC).
    pub(crate) fn epoch<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let epoch = Utc
                .with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
                .single()
                .ok_or_else(|| ctx.runtime_error("failed to create epoch"))?;
            Ok(ctx.arena.add_typed(
                Payload::Time(epoch),
                ctx.runtime_types.meta_time(),
                ctx.span,
            ))
        })
    }

    /// `(String, String) -> Result[Time, String]`
    ///
    /// Parses a string into a time using strftime format.
    pub(crate) fn parse<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let fmt_sid = ctx.arena.get_string_id(a).unwrap_or_else(|| {
                typechecked!("Time.parse", "String (format)")
            });
            let fmt = ctx
                .arena
                .get_str(fmt_sid)
                .ok_or_else(|| ctx.runtime_error("invalid format string"))?;

            let s_sid = ctx.arena.get_string_id(b).unwrap_or_else(|| {
                typechecked!("Time.parse", "String (input)")
            });
            let s = ctx
                .arena
                .get_str(s_sid)
                .ok_or_else(|| ctx.runtime_error("invalid input string"))?;

            match DateTime::parse_from_str(s, fmt) {
                Ok(dt) => {
                    let utc = dt.with_timezone(&Utc);
                    let time_id = ctx.arena.add_typed(
                        Payload::Time(utc),
                        ctx.runtime_types.meta_time(),
                        ctx.span,
                    );
                    Ok(ctx.result_ok(time_id))
                }
                Err(e) => {
                    let msg = format!("parse error: {e}");
                    let msg_id = ctx.arena.intern(&msg);
                    let err_id = ctx.arena.add_typed(
                        Payload::String(msg_id),
                        ctx.runtime_types.meta_string(),
                        ctx.span,
                    );
                    Ok(ctx.result_err(err_id))
                }
            }
        })
    }

    /// `(String, Time) -> String`
    ///
    /// Formats a time using strftime format.
    pub(crate) fn format<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let fmt_sid = ctx.arena.get_string_id(a).unwrap_or_else(|| {
                typechecked!("Time.format", "String (format)")
            });
            let fmt = ctx
                .arena
                .get_str(fmt_sid)
                .ok_or_else(|| ctx.runtime_error("invalid format string"))?;

            let t = Self::get_time(ctx, b, "Time");

            let formatted = t.format(fmt).to_string();
            let sid = ctx.arena.intern(&formatted);
            Ok(ctx.arena.add_typed(
                Payload::String(sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(Time, Int) -> Time`
    ///
    /// Returns a new time with `n` seconds added (negative to subtract).
    pub(crate) fn add_seconds<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let t = Self::get_time(ctx, a, "Time");
            let secs = Self::get_float(ctx, b);

            let duration =
                chrono::Duration::milliseconds((secs * 1000.0) as i64);
            let new_time = t + duration;

            Ok(ctx.arena.add_typed(
                Payload::Time(new_time),
                ctx.runtime_types.meta_time(),
                ctx.span,
            ))
        })
    }

    /// `(Time, Time) -> Float`
    ///
    /// Returns the difference in seconds (`a - b`).
    pub(crate) fn diff_seconds<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Self::get_time(ctx, args[0], "Time");
            let b = Self::get_time(ctx, args[1], "Time");

            let diff = (a - b).num_milliseconds() as f64 / 1000.0;
            Ok(ctx.arena.add_typed(
                Payload::Float(OrderedFloat(diff)),
                ctx.runtime_types.meta_float(),
                ctx.span,
            ))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn year<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let t = Self::get_time(ctx, a, "Time");
            Ok(ctx.arena.add_typed(
                Payload::Int(t.year() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn month<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let t = Self::get_time(ctx, a, "Time");
            Ok(ctx.arena.add_typed(
                Payload::Int(t.month() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn day<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let t = Self::get_time(ctx, a, "Time");
            Ok(ctx.arena.add_typed(
                Payload::Int(t.day() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn hour<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let t = Self::get_time(ctx, a, "Time");
            Ok(ctx.arena.add_typed(
                Payload::Int(t.hour() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn minute<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let t = Self::get_time(ctx, a, "Time");
            Ok(ctx.arena.add_typed(
                Payload::Int(t.minute() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn second<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let t = Self::get_time(ctx, a, "Time");
            Ok(ctx.arena.add_typed(
                Payload::Int(t.second() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(Int) -> Unit`
    ///
    /// Sleeps for `us` microseconds. Blocks execution.
    pub(crate) fn sleep<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let us = ctx
                .arena
                .payload(a)
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Time.sleep", "Int"));

            let dur = tokio::time::Duration::from_micros(us.max(0) as u64);
            tokio::time::sleep(dur).await;

            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// Helper to extract a `Time` value from an argument.
    fn get_time(
        ctx: &PrimCtx<'_>,
        id: ValueId,
        fn_name: &str,
    ) -> DateTime<Utc> {
        // Type checker guarantees value is Time
        ctx.arena
            .payload(id)
            .and_then(|v| match v {
                Payload::Time(t) => Some(*t),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!(fn_name, "Time"))
    }

    /// Helper to extract a `Float` value from an argument (accepts Int too).
    ///
    /// Type checker guarantees value is numeric.
    fn get_float(ctx: &PrimCtx<'_>, id: ValueId) -> f64 {
        ctx.arena
            .payload(id)
            .and_then(|v| match v {
                Payload::Float(f) => Some(f.0),
                Payload::Int(n) => Some(*n as f64),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!("get_float", "Numeric"))
    }
}
