mod directory;

pub(crate) use directory::Directory;
use futures::future::BoxFuture;
use smallvec::SmallVec;
use tokio::io::AsyncBufReadExt;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, ValueId};
use crate::Result;

pub(crate) struct Io;

impl Body for Io {}

impl Io {
    /// `() -> String`
    ///
    /// Reads a line from stdin (blocking until newline). Returns the line
    /// without the trailing newline character.
    pub(crate) fn get_line<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let stdin = tokio::io::stdin();
            let mut reader = tokio::io::BufReader::new(stdin);
            let mut line = String::new();

            reader
                .read_line(&mut line)
                .await
                .map_err(|e| ctx.runtime_error(format!("Io.get-line: {e}")))?;

            line.truncate(line.trim_end_matches(['\n', '\r']).len());

            let sid = ctx.vals().intern(&line);
            Ok(ctx.vals().add(Payload::String(sid)))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stdout without a trailing newline.
    pub(crate) fn print<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let sid = ctx.vals().string_id(args[0], "Io.print")?;
            let s = ctx.vals().str(sid)?.to_owned();
            let span = ctx.span();

            ctx.io().stdout(&s, span).await?;
            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stdout with a trailing newline.
    pub(crate) fn println<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let sid = ctx.vals().string_id(args[0], "Io.println")?;
            let s = ctx.vals().str(sid)?.to_owned();
            let span = ctx.span();

            ctx.io().stdoutline(&s, span).await?;
            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stderr without a trailing newline.
    pub(crate) fn eprint<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let sid = ctx.vals().string_id(args[0], "Io.eprint")?;
            let s = ctx.vals().str(sid)?.to_owned();
            let span = ctx.span();

            ctx.io().stderr(&s, span).await?;
            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stderr with a trailing newline.
    pub(crate) fn eprintln<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let sid = ctx.vals().string_id(args[0], "Io.eprintln")?;
            let s = ctx.vals().str(sid)?.to_owned();
            let span = ctx.span();

            ctx.io().stderrline(&s, span).await?;
            Ok(ctx.vals().add(Payload::Unit))
        })
    }
}
