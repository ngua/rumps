use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use futures::future::BoxFuture;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, ValueId};
use crate::Result;

pub(crate) struct Time;

impl Body for Time {}

impl Time {
    /// `() -> Time`
    ///
    /// Returns the current UTC time.
    pub(crate) fn now(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        Ok(ctx.vals().add(Payload::Time(Utc::now())))
    }

    /// `() -> Time`
    ///
    /// Returns the Unix epoch (`1970-01-01 00:00:00 UTC`).
    pub(crate) fn epoch(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let epoch = Utc
            .with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
            .single()
            .ok_or_else(|| ctx.runtime_error("failed to create epoch"))?;
        Ok(ctx.vals().add(Payload::Time(epoch)))
    }

    /// `(String, String) -> Result[Time, String]`
    ///
    /// Parses a string into a time using strftime format.
    pub(crate) fn parse(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let fmt_id = ctx.vals().string_id(args[0], "Time.parse")?;
        let fmt = ctx.vals().str(fmt_id)?.to_owned();
        let sid = ctx.vals().string_id(args[1], "Time.parse")?;
        let s = ctx.vals().str(sid)?.to_owned();

        match DateTime::parse_from_str(&s, &fmt) {
            Ok(dt) => {
                let time_id =
                    ctx.vals().add(Payload::Time(dt.with_timezone(&Utc)));
                Ok(ctx.vals().result_ok(time_id))
            }
            Err(e) => {
                let msg = format!("parse error: {e}");
                let msg_id = ctx.vals().intern(&msg);
                let err_id = ctx.vals().add(Payload::String(msg_id));
                Ok(ctx.vals().result_err(err_id))
            }
        }
    }

    /// `(String, Time) -> String`
    ///
    /// Formats a time using strftime format.
    pub(crate) fn format(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let fmt_id = ctx.vals().string_id(args[0], "Time.format")?;
        let fmt = ctx.vals().str(fmt_id)?.to_owned();
        let b = args[1];
        let t = Self::time(ctx, b, "Time.format")?;
        let formatted = t.format(&fmt).to_string();
        let sid = ctx.vals().intern(&formatted);

        Ok(ctx.vals().add(Payload::String(sid)))
    }

    /// `(Time, Int) -> Time`
    ///
    /// Returns a new time with `n` seconds added, negative to subtract.
    pub(crate) fn add_seconds(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let t = Self::time(ctx, a, "Time.add-seconds")?;
        let secs = Self::float(ctx, b, "Time.add-seconds")?;
        let dur = chrono::Duration::milliseconds((secs * 1000.0) as i64);

        Ok(ctx.vals().add(Payload::Time(t + dur)))
    }

    /// `(Time, Time) -> Float`
    ///
    /// Returns the difference in seconds, `a - b`.
    pub(crate) fn diff_seconds(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let l = Self::time(ctx, a, "Time.diff-seconds")?;
        let r = Self::time(ctx, b, "Time.diff-seconds")?;
        let diff = (l - r).num_milliseconds() as f64 / 1000.0;

        Ok(ctx.vals().add(Payload::Float(OrderedFloat(diff))))
    }

    /// `(Time) -> Int`
    pub(crate) fn year(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        Self::time_part(ctx, args[0], "Time.year", |t| t.year() as i64)
    }

    /// `(Time) -> Int`
    pub(crate) fn month(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        Self::time_part(ctx, args[0], "Time.month", |t| t.month() as i64)
    }

    /// `(Time) -> Int`
    pub(crate) fn day(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        Self::time_part(ctx, args[0], "Time.day", |t| t.day() as i64)
    }

    /// `(Time) -> Int`
    pub(crate) fn hour(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        Self::time_part(ctx, args[0], "Time.hour", |t| t.hour() as i64)
    }

    /// `(Time) -> Int`
    pub(crate) fn minute(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        Self::time_part(ctx, args[0], "Time.minute", |t| t.minute() as i64)
    }

    /// `(Time) -> Int`
    pub(crate) fn second(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        Self::time_part(ctx, args[0], "Time.second", |t| t.second() as i64)
    }

    /// `(Int) -> Unit`
    ///
    /// Sleeps for `us` microseconds.
    pub(crate) fn sleep<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let us = ctx.vals().int_payload(a, "Time.sleep")?;
            let dur = tokio::time::Duration::from_micros(us.max(0) as u64);

            tokio::time::sleep(dur).await;
            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    fn time(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<DateTime<Utc>> {
        match ctx.vals().payload(id)? {
            Payload::Time(t) => Ok(*t),
            _ => typechecked!(label, "Time"),
        }
    }

    fn float(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<f64> {
        match ctx.vals().payload(id)? {
            Payload::Float(f) => Ok(f.0),
            Payload::Int(n) => Ok(*n as f64),
            _ => typechecked!(label, "Numeric"),
        }
    }

    fn time_part(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
        f: impl FnOnce(DateTime<Utc>) -> i64,
    ) -> Result<ValueId> {
        let t = Self::time(ctx, id, label)?;
        Ok(ctx.vals().add(Payload::Int(f(t))))
    }
}
