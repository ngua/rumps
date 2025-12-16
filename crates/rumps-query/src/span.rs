//! Source location tracking for error reporting.

#![allow(dead_code)]

use std::fmt;
use std::ops::Range;

/// A span representing a range of byte offsets in source code, with optional
/// attached metadata.
///
/// The type parameter `M` allows attaching arbitrary metadata to spans. This is
/// useful for tools that need to preserve information beyond position:
///
/// - **Interpreter**: Uses `Span<()>` (the default); no metadata needed.
/// - **Formatter**: Could use `Span<Comments>` to preserve comment attachment.
/// - **LSP**: Could use `Span<Trivia>` for whitespace-preserving transforms.
///
/// Using `u32` limits source files to ~4GB, which is plenty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Span<M = ()> {
    /// Byte offset of the start (inclusive).
    pub(crate) start: u32,
    /// Byte offset of the end (exclusive).
    pub(crate) end: u32,
    /// Optional metadata attached to this span.
    pub(crate) meta: M,
}

/// Constructors for `Span<()>` (no metadata).
impl Span {
    /// Create a new span from start and end byte offsets.
    pub(crate) const fn new(start: u32, end: u32) -> Self {
        Self {
            start,
            end,
            meta: (),
        }
    }

    /// Create a span covering a single byte position.
    pub(crate) const fn point(pos: u32) -> Self {
        Self {
            start: pos,
            end: pos + 1,
            meta: (),
        }
    }
}

/// Methods available for all `Span<M>`.
impl<M> Span<M> {
    /// Create a span with explicit metadata.
    pub(crate) const fn with_meta(start: u32, end: u32, meta: M) -> Self {
        Self { start, end, meta }
    }

    /// The length of this span in bytes.
    pub(crate) const fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Whether this span is empty.
    pub(crate) const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Convert to a `Range<usize>` for slicing.
    pub(crate) fn as_range(&self) -> Range<usize> {
        self.start as usize..self.end as usize
    }

    /// Compute line and column from source text.
    ///
    /// Returns `(line, column)` where both are 1-indexed.
    pub(crate) fn line_col(&self, src: &str) -> (usize, usize) {
        src.get(..self.start as usize)
            .map(|before| {
                let line = before.chars().filter(|&c| c == '\n').count() + 1;
                let col = before
                    .rfind('\n')
                    .map_or(before.len(), |i| before.len() - i - 1)
                    + 1;
                (line, col)
            })
            .unwrap_or((1, 1))
    }

    /// Discard metadata, converting to `Span<()>`.
    pub(crate) fn strip(self) -> Span {
        Span {
            start: self.start,
            end: self.end,
            meta: (),
        }
    }
}

/// Methods for spans with `Default` metadata.
impl<M: Default> Span<M> {
    /// Merge two spans into one covering both, using default metadata.
    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            meta: M::default(),
        }
    }
}

impl<M: Default> Default for Span<M> {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            meta: M::default(),
        }
    }
}

impl<M> fmt::Display for Span<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl From<Range<usize>> for Span {
    fn from(r: Range<usize>) -> Self {
        Self {
            start: r.start as u32,
            end: r.end as u32,
            meta: (),
        }
    }
}

impl<M> From<Span<M>> for Range<usize> {
    fn from(s: Span<M>) -> Self {
        s.as_range()
    }
}

impl chumsky::span::Span for Span {
    type Context = ();
    type Offset = u32;

    fn new(_ctx: (), range: Range<Self::Offset>) -> Self {
        Self {
            start: range.start,
            end: range.end,
            meta: (),
        }
    }

    fn context(&self) -> Self::Context {}

    fn start(&self) -> Self::Offset {
        self.start
    }

    fn end(&self) -> Self::Offset {
        self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_first_line() {
        let src = "hello world";
        let span = Span::new(6, 11);
        assert_eq!(span.line_col(src), (1, 7));
    }

    #[test]
    fn line_col_second_line() {
        let src = "hello\nworld";
        let span = Span::new(6, 11);
        assert_eq!(span.line_col(src), (2, 1));
    }

    #[test]
    fn line_col_multiline() {
        let src = "line1\nline2\nline3";
        let span = Span::new(12, 17);
        assert_eq!(span.line_col(src), (3, 1));
    }

    #[test]
    fn merge_spans() {
        let a = Span::new(5, 10);
        let b = Span::new(15, 20);
        assert_eq!(a.merge(b), Span::new(5, 20));
    }
}
