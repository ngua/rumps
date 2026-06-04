use std::sync::Arc;

use itertools::Itertools;
use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::interpreter::convert::RawDisplay;
use crate::value::{Payload, ValueId};

pub(crate) struct Str;

impl Prim for Str {}

impl Str {
    /// `(String) -> Int`
    ///
    /// Returns the number of grapheme clusters in the string.
    pub(crate) fn length<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.length", "String"));

            let s = Self::valid_str(ctx.arena, sid);

            let len = s.graphemes(true).count() as i64;
            Ok(ctx.arena.add_typed(
                Payload::Int(len),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> String`
    ///
    /// Returns the string in uppercase.
    pub(crate) fn upper<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.upper", "String"));

            let s = Self::valid_str(ctx.arena, sid);

            let upper = s.to_uppercase();
            let new_sid = ctx.arena.intern(&upper);
            Ok(ctx.arena.add_typed(
                Payload::String(new_sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> String`
    ///
    /// Returns the string in lowercase.
    pub(crate) fn lower<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.lower", "String"));

            let s = Self::valid_str(ctx.arena, sid);

            let lower = s.to_lowercase();
            let new_sid = ctx.arena.intern(&lower);
            Ok(ctx.arena.add_typed(
                Payload::String(new_sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> String`
    ///
    /// Returns the string with leading and trailing whitespace removed.
    pub(crate) fn trim<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.trim", "String"));

            // Copy to owned String to release borrow before interning
            let trimmed = Self::valid_str(ctx.arena, sid).trim().to_owned();

            let new_sid = ctx.arena.intern(&trimmed);
            Ok(ctx.arena.add_typed(
                Payload::String(new_sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(String, String) -> Array[String]`
    ///
    /// Splits the string by the delimiter, returning an array of substrings.
    pub(crate) fn split<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.split", "String"));

            let d_sid = ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                typechecked!("String.split", "delimiter must be String")
            });

            // Copy strings to owned values to release borrow before iteration
            let s = Self::valid_str(ctx.arena, s_sid).to_owned();
            let d = Self::valid_str(ctx.arena, d_sid).to_owned();

            // Split and collect parts; intern each part
            let parts: SmallVec<[ValueId; 4]> = s
                .split(&d)
                .map(|part| {
                    let part_sid = ctx.arena.intern(part);
                    ctx.arena.add_typed(
                        Payload::String(part_sid),
                        ctx.runtime_types.meta_string(),
                        ctx.span,
                    )
                })
                .collect();

            Ok(ctx.add(Payload::Array(Arc::new(parts))))
        })
    }

    /// `(Array[String], String) -> String`
    ///
    /// Joins an array of strings with the delimiter.
    pub(crate) fn join<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let elems = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("String.join", "Array"));

            let d_sid = ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                typechecked!("String.join", "delimiter must be String")
            });

            // Collect string slices from array elements
            // Type checker guarantees elements are String
            let parts: Vec<&str> = elems
                .iter()
                .map(|id| {
                    ctx.arena
                        .get_string_id(*id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("String.join", "Array[String]")
                        })
                })
                .collect();

            let d = ctx.arena.get_str(d_sid).ok_or_else(|| {
                ctx.runtime_error("String.join: invalid delimiter")
            })?;

            let joined = parts.into_iter().join(d);
            let new_sid = ctx.arena.intern(&joined);
            Ok(ctx.arena.add_typed(
                Payload::String(new_sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(String, Int, Int) -> String`
    ///
    /// Returns a substring from index `start` (inclusive) to `end` (exclusive).
    /// Indices are grapheme-based and clamped to valid bounds.
    pub(crate) fn slice<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.slice", "String"));

            let start = ctx
                .arena
                .payload(args[1])
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    typechecked!("String.slice", "start must be Int")
                });

            let end = ctx
                .arena
                .payload(args[2])
                .and_then(|v| match v {
                    Payload::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    typechecked!("String.slice", "end must be Int")
                });

            let s = Self::valid_str(ctx.arena, sid);

            // Single-pass: skip, take, join graphemes
            let start_idx = start.max(0) as usize;
            let end_idx = end.max(0) as usize;
            let sliced: String = s
                .graphemes(true)
                .skip(start_idx)
                .take(end_idx.saturating_sub(start_idx))
                .collect();

            let new_sid = ctx.arena.intern(&sliced);
            Ok(ctx.arena.add_typed(
                Payload::String(new_sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(String, String) -> Bool`
    ///
    /// Returns `true` if the string contains the substring.
    pub(crate) fn contains<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.contains", "String"));

            let sub_sid =
                ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                    typechecked!("String.contains", "substring must be String")
                });

            let s = Self::valid_str(ctx.arena, s_sid);
            let sub = Self::valid_str(ctx.arena, sub_sid);

            Ok(ctx.arena.add_typed(
                Payload::Bool(s.contains(sub)),
                ctx.runtime_types.meta_bool(),
                ctx.span,
            ))
        })
    }

    /// `(String, String, String) -> String`
    ///
    /// Replaces all occurrences of `old` with `new`.
    pub(crate) fn replace<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.replace", "String"));

            let old_sid =
                ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                    typechecked!("String.replace", "pattern must be String")
                });

            let new_sid =
                ctx.arena.get_string_id(args[2]).unwrap_or_else(|| {
                    typechecked!("String.replace", "replacement must be String")
                });

            let s = Self::valid_str(ctx.arena, s_sid);
            let old = Self::valid_str(ctx.arena, old_sid);
            let new = Self::valid_str(ctx.arena, new_sid);

            let replaced = s.replace(old, new);
            let result_sid = ctx.arena.intern(&replaced);
            Ok(ctx.arena.add_typed(
                Payload::String(result_sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> String`
    ///
    /// Escapes special characters for display. Converts:
    /// - `"` → `\"`
    /// - `\` → `\\`
    /// - newline → `\n`
    /// - tab → `\t`
    /// - carriage return → `\r`
    /// - null → `\0`
    pub(crate) fn escape<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.escape", "String"));

            let s = Self::valid_str(ctx.arena, sid);
            let escaped = RawDisplay::escape_str(s);
            let new_sid = ctx.arena.intern(&escaped);
            Ok(ctx.arena.add_typed(
                Payload::String(new_sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }
}
