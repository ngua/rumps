//! I/O context for interpreter output operations.
//!
//! Abstracts stdout, stderr, and file writes for testability. Production code
//! uses [`Io`] which performs actual I/O; tests use [`TestIo`] which captures
//! output to buffers.

use std::path::Path;

use async_trait::async_trait;
use tokio::io::{stderr, stdout, AsyncWriteExt};

use crate::{Error, Result, Span};

/// I/O context for interpreter output operations.
///
/// Abstracts stdout, stderr, and file writes for testability.
#[async_trait]
pub(crate) trait IoContext: Send {
    /// Write to stdout without appending a newline.
    async fn stdout(&mut self, s: &str, span: Span) -> Result<()>;

    /// Write a line to stdout (appends newline).
    async fn stdoutline(&mut self, s: &str, span: Span) -> Result<()>;

    /// Write to stderr without appending a newline.
    async fn stderr(&mut self, s: &str, span: Span) -> Result<()>;

    /// Write a line to stderr (appends newline).
    async fn stderrline(&mut self, s: &str, span: Span) -> Result<()>;

    /// Write content to a file.
    async fn write(
        &mut self,
        path: &str,
        content: &str,
        span: Span,
    ) -> Result<()>;
}

/// Standard I/O context that writes to actual stdout/files.
pub(crate) struct Io;

#[async_trait]
impl IoContext for Io {
    async fn stdout(&mut self, s: &str, span: Span) -> Result<()> {
        let mut out = stdout();
        let map_err = |e: std::io::Error| {
            Error::runtime(span, format!("output error: {e}"))
        };
        out.write_all(s.as_bytes()).await.map_err(map_err)?;
        out.flush().await.map_err(map_err)
    }

    async fn stdoutline(&mut self, s: &str, span: Span) -> Result<()> {
        let mut out = stdout();
        let map_err = |e: std::io::Error| {
            Error::runtime(span, format!("output error: {e}"))
        };
        out.write_all(s.as_bytes()).await.map_err(map_err)?;
        out.write_all(b"\n").await.map_err(map_err)?;
        out.flush().await.map_err(map_err)
    }

    async fn stderr(&mut self, s: &str, span: Span) -> Result<()> {
        let mut err = stderr();
        let map_err = |e: std::io::Error| {
            Error::runtime(span, format!("stderr output error: {e}"))
        };
        err.write_all(s.as_bytes()).await.map_err(map_err)?;
        err.flush().await.map_err(map_err)
    }

    async fn stderrline(&mut self, s: &str, span: Span) -> Result<()> {
        let mut err = stderr();
        let map_err = |e: std::io::Error| {
            Error::runtime(span, format!("stderr output error: {e}"))
        };
        err.write_all(s.as_bytes()).await.map_err(map_err)?;
        err.write_all(b"\n").await.map_err(map_err)?;
        err.flush().await.map_err(map_err)
    }

    async fn write(
        &mut self,
        path: &str,
        content: &str,
        span: Span,
    ) -> Result<()> {
        tokio::fs::write(Path::new(path), content)
            .await
            .map_err(|e| Error::runtime(span, format!("file write error: {e}")))
    }
}

/// Test I/O context that captures output to buffers.
#[derive(Default)]
pub(crate) struct TestIo {
    /// Captured stdout output.
    pub(crate) stdout_buf: Vec<u8>,
    /// Captured stderr output.
    pub(crate) stderr_buf: Vec<u8>,
    /// Captured file writes: (path, content).
    pub(crate) file_writes: Vec<(String, String)>,
}

impl TestIo {
    /// Create a new test I/O context.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Get captured stdout as a string.
    pub(crate) fn stdout_str(&self) -> &str {
        std::str::from_utf8(&self.stdout_buf).unwrap_or("<invalid utf8>")
    }
}

#[async_trait]
impl IoContext for TestIo {
    async fn stdout(&mut self, s: &str, _span: Span) -> Result<()> {
        self.stdout_buf.extend_from_slice(s.as_bytes());
        Ok(())
    }

    async fn stdoutline(&mut self, s: &str, _span: Span) -> Result<()> {
        self.stdout_buf.extend_from_slice(s.as_bytes());
        self.stdout_buf.push(b'\n');
        Ok(())
    }

    async fn stderr(&mut self, s: &str, _span: Span) -> Result<()> {
        self.stderr_buf.extend_from_slice(s.as_bytes());
        Ok(())
    }

    async fn stderrline(&mut self, s: &str, _span: Span) -> Result<()> {
        self.stderr_buf.extend_from_slice(s.as_bytes());
        self.stderr_buf.push(b'\n');
        Ok(())
    }

    async fn write(
        &mut self,
        path: &str,
        content: &str,
        _span: Span,
    ) -> Result<()> {
        self.file_writes.push((path.to_owned(), content.to_owned()));
        Ok(())
    }
}

/// No-op I/O context for primitive unit tests that don't need I/O.
#[cfg(test)]
pub(crate) struct NoopIo;

#[cfg(test)]
#[async_trait]
impl IoContext for NoopIo {
    async fn stdout(&mut self, _s: &str, _span: Span) -> Result<()> {
        Ok(())
    }

    async fn stdoutline(&mut self, _s: &str, _span: Span) -> Result<()> {
        Ok(())
    }

    async fn stderr(&mut self, _s: &str, _span: Span) -> Result<()> {
        Ok(())
    }

    async fn stderrline(&mut self, _s: &str, _span: Span) -> Result<()> {
        Ok(())
    }

    async fn write(
        &mut self,
        _path: &str,
        _content: &str,
        _span: Span,
    ) -> Result<()> {
        Ok(())
    }
}
