//! Error types for lexing, parsing, and runtime.

#![allow(dead_code)]

use std::fmt;

use chumsky::error::Simple;
use miette::{Diagnostic, LabeledSpan};
use nonempty::NonEmpty;
use thiserror::Error;

use crate::intern::StringInterner;
use crate::typecheck::{FormattedTypeError, TypeError};
use crate::{Span, Token};

/// Crate-wide result type.
pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Errors produced during lexing, parsing, or interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
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

    #[error("RAISE at {span}: {msg}")]
    Raise { span: Span, msg: String },

    #[error("runtime type error at {span}: {msg}")]
    RuntimeType { span: Span, msg: String },

    #[error("cannot coerce {from} to {to}: {msg}")]
    Coercion {
        from: &'static str,
        to: &'static str,
        msg: String,
    },

    #[error("{}", fmt_multiple(errors))]
    Multiple { errors: NonEmpty<Box<Self>> },

    /// Static type error from the type checker (unformatted; legacy).
    #[error("{0}")]
    #[allow(private_interfaces)]
    Type(#[from] TypeError),

    /// Formatted static type error with resolved type names.
    #[error("{}", .0.message)]
    #[allow(private_interfaces)]
    FormattedType(FormattedTypeError),
}

fn fmt_expected(expected: &[String]) -> String {
    (!expected.is_empty())
        .then(|| format!(" (expected: {})", expected.join(", ")))
        .unwrap_or_default()
}

fn fmt_span(span: &Option<Span>) -> String {
    span.map_or(String::new(), |s| format!(" at {s}"))
}

fn fmt_multiple(errors: &NonEmpty<Box<Error>>) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n")
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

    pub(crate) fn runtime_type(span: Span, msg: impl Into<String>) -> Self {
        Self::RuntimeType {
            span,
            msg: msg.into(),
        }
    }

    pub(crate) fn raise(span: Span, msg: impl Into<String>) -> Self {
        Self::Raise {
            span,
            msg: msg.into(),
        }
    }

    pub(crate) fn coercion(
        from: &'static str,
        to: &'static str,
        msg: impl Into<String>,
    ) -> Self {
        Self::Coercion {
            from,
            to,
            msg: msg.into(),
        }
    }

    /// Returns the variant index and message for catchable runtime errors.
    ///
    /// Variant indices match `TypeId::ERROR` registration order:
    /// 0 = Runtime, 1 = Raise, 2 = Type, 3 = Coerce.
    pub(crate) fn runtime_variant(&self) -> Option<(u8, &str)> {
        match self {
            Self::Runtime { msg, .. } => Some((0, msg)),
            Self::Raise { msg, .. } => Some((1, msg)),
            Self::RuntimeType { msg, .. } => Some((2, msg)),
            Self::Coercion { msg, .. } => Some((3, msg)),
            _ => None,
        }
    }

    /// Create an error from one or more errors.
    ///
    /// If exactly one error, returns it directly; otherwise wraps in `Multiple`.
    pub(crate) fn multiple(errors: NonEmpty<Self>) -> Self {
        if errors.tail.is_empty() {
            errors.head
        } else {
            Self::Multiple {
                errors: errors.map(Box::new),
            }
        }
    }

    pub(crate) fn span(&self) -> Option<Span> {
        match self {
            Self::Lex { span, .. }
            | Self::Parse { span, .. }
            | Self::Raise { span, .. }
            | Self::RuntimeType { span, .. } => Some(*span),
            Self::Runtime { span, .. } => *span,
            Self::Type(e) => Some(e.span()),
            Self::FormattedType(e) => Some(e.span),
            Self::Coercion { .. } | Self::Multiple { .. } => None,
        }
    }

    /// Create a parse error from a chumsky error, using `interner` to display
    /// actual token content instead of generic labels.
    pub(crate) fn from_parse_rich(
        e: Simple<Token, Span>,
        interner: &StringInterner,
    ) -> Self {
        let span = e.span();
        let msg = e
            .found()
            .map(|t| format!("unexpected `{}`", t.display_resolved(interner)))
            .unwrap_or_else(|| "unexpected end of input".into());
        let expected = e
            .expected()
            .filter_map(|exp| {
                exp.as_ref().map(|t| t.display_resolved(interner))
            })
            .collect();
        Self::parse(span, msg, expected)
    }

    pub(crate) fn display_with_source<'a>(
        &'a self,
        src: &'a str,
    ) -> ErrorDisplay<'a> {
        ErrorDisplay { err: self, src }
    }
}

impl From<rumps_types::StorageError> for Error {
    fn from(e: rumps_types::StorageError) -> Self {
        Self::runtime_no_span(e.to_string())
    }
}

impl Diagnostic for Error {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let code: &'static str = match self {
            Self::Lex { .. } => "rumps::lex",
            Self::Parse { .. } => "rumps::parse",
            Self::Runtime { .. } => "rumps::runtime",
            Self::Raise { .. } => "rumps::raise",
            Self::RuntimeType { .. } => "rumps::runtime_type",
            Self::Coercion { .. } => "rumps::coercion",
            Self::Multiple { .. } => "rumps::multiple",
            Self::Type(_) | Self::FormattedType(_) => "rumps::type",
        };
        Some(Box::new(code))
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span_to_label = |s: Span, msg: &str| {
            LabeledSpan::new_with_span(Some(msg.to_owned()), s)
        };

        match self {
            Self::Lex { span, .. } => {
                Some(Box::new(std::iter::once(span_to_label(*span, "here"))))
            }
            Self::Parse { span, .. } => {
                Some(Box::new(std::iter::once(span_to_label(*span, "here"))))
            }
            Self::RuntimeType { span, .. } => {
                Some(Box::new(std::iter::once(span_to_label(*span, "here"))))
            }
            Self::Raise { span, .. } => {
                Some(Box::new(std::iter::once(span_to_label(*span, "RAISE"))))
            }
            Self::Runtime { span: Some(s), .. } => {
                Some(Box::new(std::iter::once(span_to_label(*s, "here"))))
            }
            Self::Runtime { span: None, .. } | Self::Coercion { .. } => None,
            Self::Type(e) => {
                Some(Box::new(std::iter::once(span_to_label(e.span(), "here"))))
            }
            Self::FormattedType(e) => {
                Some(Box::new(std::iter::once(span_to_label(e.span, "error"))))
            }
            Self::Multiple { errors } => {
                // Collect labels with individual error messages
                let labels: Vec<_> = errors
                    .iter()
                    .filter_map(|e| {
                        e.span().map(|s| {
                            let msg = match e.as_ref() {
                                Self::FormattedType(fe) => fe.message.clone(),
                                _ => "error".to_owned(),
                            };
                            span_to_label(s, &msg)
                        })
                    })
                    .collect();
                (!labels.is_empty()).then(|| {
                    Box::new(labels.into_iter())
                        as Box<dyn Iterator<Item = LabeledSpan>>
                })
            }
        }
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        match self {
            Self::FormattedType(e) => e
                .help
                .as_ref()
                .map(|h| Box::new(h.as_str()) as Box<dyn fmt::Display>),
            Self::Multiple { errors } => {
                // Collect help messages from formatted errors
                let helps: Vec<_> = errors
                    .iter()
                    .filter_map(|e| match e.as_ref() {
                        Self::FormattedType(fe) => fe.help.as_ref(),
                        _ => None,
                    })
                    .collect();
                helps
                    .first()
                    .map(|h| Box::new(h.as_str()) as Box<dyn fmt::Display>)
            }
            _ => None,
        }
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
            Error::Raise { span, msg } => {
                write!(f, "RAISE at {}: {msg}", loc(*span))
            }
            Error::RuntimeType { span, msg } => {
                write!(f, "runtime type error at {}: {msg}", loc(*span))
            }
            Error::Coercion { from, to, msg } => {
                write!(f, "cannot coerce {from} to {to}: {msg}")
            }
            Error::Type(e) => {
                write!(f, "type error at {}: {e}", loc(e.span()))
            }
            Error::FormattedType(e) => {
                write!(f, "type error at {}: {}", loc(e.span), e.message)
            }
            Error::Multiple { errors } => {
                let formatted: Vec<_> = errors
                    .iter()
                    .map(|e| e.display_with_source(self.src).to_string())
                    .collect();
                write!(f, "{}", formatted.join("\n"))
            }
        }
    }
}
