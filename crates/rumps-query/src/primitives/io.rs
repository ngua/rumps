mod directory;

pub(crate) use directory::Directory;
use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

pub(crate) struct Io;

impl Prim for Io {}

impl Io {
    /// `() -> String`
    ///
    /// Reads a line from stdin (blocking until newline). Returns the line
    /// without the trailing newline character.
    pub(crate) fn get_line<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            use tokio::io::AsyncBufReadExt;

            let stdin = tokio::io::stdin();
            let mut reader = tokio::io::BufReader::new(stdin);
            let mut line = String::new();

            reader
                .read_line(&mut line)
                .await
                .map_err(|e| ctx.runtime_error(format!("Io.get-line: {e}")))?;

            // Remove trailing newline
            line.truncate(line.trim_end_matches(['\n', '\r']).len());

            let sid = ctx.arena.intern(&line);
            Ok(ctx.arena.add_typed(
                Payload::String(sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stdout without a trailing newline.
    pub(crate) fn print<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let sid = ctx
                .arena
                .get_string_id(a)
                .unwrap_or_else(|| typechecked!("Io.print", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stdout(&s, ctx.span).await?;
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stdout with a trailing newline.
    pub(crate) fn println<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let sid = ctx
                .arena
                .get_string_id(a)
                .unwrap_or_else(|| typechecked!("Io.println", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stdoutline(&s, ctx.span).await?;
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stderr without a trailing newline.
    pub(crate) fn eprint<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let sid = ctx
                .arena
                .get_string_id(a)
                .unwrap_or_else(|| typechecked!("Io.eprint", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stderr(&s, ctx.span).await?;
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stderr with a trailing newline.
    pub(crate) fn eprintln<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let sid = ctx
                .arena
                .get_string_id(a)
                .unwrap_or_else(|| typechecked!("Io.eprintln", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stderrline(&s, ctx.span).await?;
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }
}
