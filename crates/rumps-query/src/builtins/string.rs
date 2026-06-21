use std::sync::Arc;

use itertools::Itertools;
use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::interpreter::convert::RawDisplay;
use crate::value::{Payload, ValueId};
use crate::Result;

pub(crate) struct Str;

impl Body for Str {}

impl Str {
    /// `(String) -> Int`
    ///
    /// Returns the number of grapheme clusters in the string.
    pub(crate) fn length(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let sid = ctx.vals().string_id(args[0], "String.length")?;
        let len = ctx.vals().str(sid)?.graphemes(true).count() as i64;
        Ok(ctx.vals().add(Payload::Int(len)))
    }

    /// `(String) -> String`
    ///
    /// Returns the string in uppercase.
    pub(crate) fn upper(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let sid = ctx.vals().string_id(args[0], "String.upper")?;
        let upper = ctx.vals().str(sid)?.to_uppercase();
        let sid = ctx.vals().intern(&upper);
        Ok(ctx.vals().add(Payload::String(sid)))
    }

    /// `(String) -> String`
    ///
    /// Returns the string in lowercase.
    pub(crate) fn lower(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let sid = ctx.vals().string_id(args[0], "String.lower")?;
        let lower = ctx.vals().str(sid)?.to_lowercase();
        let sid = ctx.vals().intern(&lower);
        Ok(ctx.vals().add(Payload::String(sid)))
    }

    /// `(String) -> String`
    ///
    /// Returns the string with leading and trailing whitespace removed.
    pub(crate) fn trim(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let sid = ctx.vals().string_id(args[0], "String.trim")?;
        let trimmed = ctx.vals().str(sid)?.trim().to_owned();
        let sid = ctx.vals().intern(&trimmed);
        Ok(ctx.vals().add(Payload::String(sid)))
    }

    /// `(String, String) -> Array[String]`
    ///
    /// Splits the string by the delimiter, returning an array of substrings.
    pub(crate) fn split(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let s = Self::owned_str(ctx, args[0], "String.split")?;
        let d = Self::owned_str(ctx, args[1], "String.split")?;
        let parts = s
            .split(&d)
            .map(|part| {
                let sid = ctx.vals().intern(part);
                ctx.vals().add(Payload::String(sid))
            })
            .collect();
        Ok(ctx.vals().add(Payload::Array(Arc::new(parts))))
    }

    /// `(Array[String], String) -> String`
    ///
    /// Joins an array of strings with the delimiter.
    pub(crate) fn join(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let elems = ctx.vals().take_array(args[0], "String.join")?;
        let d = Self::owned_str(ctx, args[1], "String.join")?;
        let parts: Result<Vec<String>> = elems
            .iter()
            .map(|id| Self::owned_str(ctx, *id, "String.join"))
            .collect();
        let joined = parts?.into_iter().join(&d);
        let sid = ctx.vals().intern(&joined);
        Ok(ctx.vals().add(Payload::String(sid)))
    }

    /// `(String, Int, Int) -> String`
    ///
    /// Returns a substring from index `start` (inclusive) to `end` (exclusive).
    /// Indices are grapheme-based and clamped to valid bounds.
    pub(crate) fn slice(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let s = Self::owned_str(ctx, args[0], "String.slice")?;
        let start = ctx.vals().int_payload(args[1], "String.slice")?;
        let end = ctx.vals().int_payload(args[2], "String.slice")?;
        let start = start.max(0) as usize;
        let end = end.max(0) as usize;
        let sliced: String = s
            .graphemes(true)
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        let sid = ctx.vals().intern(&sliced);
        Ok(ctx.vals().add(Payload::String(sid)))
    }

    /// `(String, String) -> Bool`
    ///
    /// Returns `true` if the string contains the substring.
    pub(crate) fn contains(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let s = Self::owned_str(ctx, args[0], "String.contains")?;
        let sub = Self::owned_str(ctx, args[1], "String.contains")?;
        Ok(ctx.vals().add(Payload::Bool(s.contains(&sub))))
    }

    /// `(String, String, String) -> String`
    ///
    /// Replaces all occurrences of `old` with `new`.
    pub(crate) fn replace(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let s = Self::owned_str(ctx, args[0], "String.replace")?;
        let old = Self::owned_str(ctx, args[1], "String.replace")?;
        let new = Self::owned_str(ctx, args[2], "String.replace")?;
        let replaced = s.replace(&old, &new);
        let sid = ctx.vals().intern(&replaced);
        Ok(ctx.vals().add(Payload::String(sid)))
    }

    /// `(String) -> String`
    ///
    /// Escapes special characters for display.
    pub(crate) fn escape(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let s = Self::owned_str(ctx, args[0], "String.escape")?;
        let escaped = RawDisplay::escape_str(&s);
        let sid = ctx.vals().intern(&escaped);
        Ok(ctx.vals().add(Payload::String(sid)))
    }

    fn owned_str(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<String> {
        let sid = ctx.vals().string_id(id, label)?;
        ctx.vals().str(sid).map(str::to_owned)
    }
}
