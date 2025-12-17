//! I/O context for interpreter output operations.
//!
//! Abstracts stdout, file writes, etc. for testability. Production code uses
//! [`Io`] which performs actual I/O; tests use [`TestIo`] which captures
//! output to buffers.

use futures::future::BoxFuture;
use tokio::io::{stdout, AsyncWriteExt};

use crate::{Error, Result, Span};

/// I/O context for interpreter output operations.
///
/// Abstracts stdout, file writes, etc. for testability.
pub trait IoContext: Send {
    /// Write a line to stdout (appends newline).
    fn stdout(&mut self, s: &str, span: Span) -> BoxFuture<'_, Result<()>>;

    // /// Write to a file.
    // fn file(&mut self, path: &Path, s: &str, span: Span) -> BoxFuture<'_, Result<()>>;
}

/// Standard I/O context that writes to actual stdout/files.
pub struct Io;

impl IoContext for Io {
    fn stdout(&mut self, s: &str, span: Span) -> BoxFuture<'_, Result<()>> {
        let s = s.to_owned();
        Box::pin(async move {
            let mut out = stdout();
            let map_err = |e: std::io::Error| {
                Error::runtime(span, format!("output error: {e}"))
            };
            out.write_all(s.as_bytes()).await.map_err(map_err)?;
            out.write_all(b"\n").await.map_err(map_err)?;
            out.flush().await.map_err(map_err)
        })
    }
}

/// Test I/O context that captures output to buffers.
#[derive(Default)]
pub struct TestIo {
    /// Captured stdout output.
    pub stdout_buf: Vec<u8>,
}

impl TestIo {
    /// Create a new test I/O context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get captured stdout as a string.
    pub fn stdout_str(&self) -> &str {
        std::str::from_utf8(&self.stdout_buf).unwrap_or("<invalid utf8>")
    }
}

impl IoContext for TestIo {
    fn stdout(&mut self, s: &str, _span: Span) -> BoxFuture<'_, Result<()>> {
        self.stdout_buf.extend_from_slice(s.as_bytes());
        self.stdout_buf.push(b'\n');
        Box::pin(async { Ok(()) })
    }
}
