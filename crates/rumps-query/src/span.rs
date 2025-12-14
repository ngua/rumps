//! Source location tracking for error reporting.

#![allow(dead_code)]

use std::fmt;
use std::ops::Range;

/// A span representing a range of byte offsets in source code.
///
/// Using `u32` limits source files to ~4GB, which is plenty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) struct Span {
    /// Byte offset of the start (inclusive).
    pub(crate) start: u32,
    /// Byte offset of the end (exclusive).
    pub(crate) end: u32,
}

impl Span {
    /// Create a new span from start and end byte offsets.
    pub(crate) const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Create a span covering a single byte position.
    pub(crate) const fn point(pos: u32) -> Self {
        Self {
            start: pos,
            end: pos + 1,
        }
    }

    /// Merge two spans into one covering both.
    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
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
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl From<Range<usize>> for Span {
    fn from(r: Range<usize>) -> Self {
        Self {
            start: r.start as u32,
            end: r.end as u32,
        }
    }
}

impl From<Span> for Range<usize> {
    fn from(s: Span) -> Self {
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
