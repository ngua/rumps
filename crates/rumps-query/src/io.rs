//! I/O context for interpreter output operations.
//!
//! Abstracts stdout, file writes, etc. for testability. Production code uses
//! [`Io`] which performs actual I/O; tests use [`TestIo`] which captures
//! output to buffers.

use async_trait::async_trait;
use tokio::io::{stdout, AsyncWriteExt};

use crate::{Error, Result, Span};

/// I/O context for interpreter output operations.
///
/// Abstracts stdout, file writes, etc. for testability.
#[async_trait]
pub trait IoContext: Send {
    /// Write a line to stdout (appends newline).
    async fn stdout(&mut self, s: &str, span: Span) -> Result<()>;

    // /// Write to a file.
    // async fn file(&mut self, path: &Path, s: &str, span: Span) -> Result<()>;
}

/// Standard I/O context that writes to actual stdout/files.
pub struct Io;

#[async_trait]
impl IoContext for Io {
    async fn stdout(&mut self, s: &str, span: Span) -> Result<()> {
        let mut out = stdout();
        let map_err = |e: std::io::Error| {
            Error::runtime(span, format!("output error: {e}"))
        };
        out.write_all(s.as_bytes()).await.map_err(map_err)?;
        out.write_all(b"\n").await.map_err(map_err)?;
        out.flush().await.map_err(map_err)
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

#[async_trait]
impl IoContext for TestIo {
    async fn stdout(&mut self, s: &str, _span: Span) -> Result<()> {
        self.stdout_buf.extend_from_slice(s.as_bytes());
        self.stdout_buf.push(b'\n');
        Ok(())
    }
}
