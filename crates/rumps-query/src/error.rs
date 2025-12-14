//! Error types for lexing, parsing, and runtime.

#![allow(dead_code)]

use std::fmt;

use thiserror::Error;

use crate::Span;

/// Errors produced during lexing, parsing, or interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum Error {
    #[error("lex error at {span}: {msg}")]
    Lex { span: Span, msg: String },

    #[error("parse error at {span}: {msg}{}", fmt_expected(expected))]
    Parse {
        span: Span,
        msg: String,
        expected: Vec<String>,
    },

    #[error("runtime error{}: {msg}", fmt_span(span))]
    Runtime { span: Option<Span>, msg: String },
}

fn fmt_expected(expected: &[String]) -> String {
    (!expected.is_empty())
        .then(|| format!(" (expected: {})", expected.join(", ")))
        .unwrap_or_default()
}

fn fmt_span(span: &Option<Span>) -> String {
    span.map_or(String::new(), |s| format!(" at {s}"))
}

impl Error {
    pub(crate) fn lex(span: Span, msg: impl Into<String>) -> Self {
        Self::Lex {
            span,
            msg: msg.into(),
        }
    }

    pub(crate) fn parse(
        span: Span,
        msg: impl Into<String>,
        expected: Vec<String>,
    ) -> Self {
        Self::Parse {
            span,
            msg: msg.into(),
            expected,
        }
    }

    pub(crate) fn runtime(span: Span, msg: impl Into<String>) -> Self {
        Self::Runtime {
            span: Some(span),
            msg: msg.into(),
        }
    }

    pub(crate) fn runtime_no_span(msg: impl Into<String>) -> Self {
        Self::Runtime {
            span: None,
            msg: msg.into(),
        }
    }

    pub(crate) fn span(&self) -> Option<Span> {
        match self {
            Self::Lex { span, .. } | Self::Parse { span, .. } => Some(*span),
            Self::Runtime { span, .. } => *span,
        }
    }

    pub(crate) fn display_with_source<'a>(
        &'a self,
        src: &'a str,
    ) -> ErrorDisplay<'a> {
        ErrorDisplay { err: self, src }
    }
}

/// Helper for displaying an error with source context.
pub(crate) struct ErrorDisplay<'a> {
    err: &'a Error,
    src: &'a str,
}

impl fmt::Display for ErrorDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let loc = |span: Span| {
            let (line, col) = span.line_col(self.src);
            format!("{line}:{col}")
        };

        match self.err {
            Error::Lex { span, msg } => {
                write!(f, "lex error at {}: {msg}", loc(*span))
            }
            Error::Parse {
                span,
                msg,
                expected,
            } => {
                write!(f, "parse error at {}: {msg}", loc(*span))?;
                if !expected.is_empty() {
                    write!(f, " (expected: {})", expected.join(", "))?;
                }
                Ok(())
            }
            Error::Runtime { span: Some(s), msg } => {
                write!(f, "runtime error at {}: {msg}", loc(*s))
            }
            Error::Runtime { span: None, msg } => {
                write!(f, "runtime error: {msg}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_error_display() {
        let err = Error::lex(Span::new(0, 5), "unexpected character");
        assert_eq!(err.to_string(), "lex error at 0..5: unexpected character");
    }

    #[test]
    fn parse_error_display() {
        let err = Error::parse(
            Span::new(10, 15),
            "unexpected token",
            vec!["SET".into(), "OUTPUT".into()],
        );
        assert_eq!(
            err.to_string(),
            "parse error at 10..15: unexpected token (expected: SET, OUTPUT)"
        );
    }

    #[test]
    fn runtime_error_display() {
        let err = Error::runtime(Span::new(20, 25), "division by zero");
        assert_eq!(
            err.to_string(),
            "runtime error at 20..25: division by zero"
        );
    }

    #[test]
    fn runtime_error_no_span_display() {
        let err = Error::runtime_no_span("unknown variable");
        assert_eq!(err.to_string(), "runtime error: unknown variable");
    }

    #[test]
    fn error_with_source_display() {
        let src = "SET x = 10\nOUTPUT y";
        let err = Error::runtime(Span::new(11, 17), "undefined variable `y`");
        assert_eq!(
            err.display_with_source(src).to_string(),
            "runtime error at 2:1: undefined variable `y`"
        );
    }
}
