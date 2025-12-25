//! Type error definitions for static type checking.
//!
//! These errors are produced during the type checking phase (compile-time),
//! distinct from `Error::RuntimeType` which occurs during interpretation.

use std::fmt;

use thiserror::Error;

use super::ty::{Ty, TyVar};
use crate::intern::StringId;
use crate::{Span, TypeId};

/// Static type errors detected during type checking.
///
/// Wrapped by `crate::Error::Type` for integration with the main error
/// type. Multiple type errors may be collected and reported together.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum TypeError {
    /// Type mismatch: expected one type, got another.
    #[error("type mismatch: expected `{expected}`, got `{got}`")]
    Mismatch { expected: Ty, got: Ty, span: Span },

    /// Reference to undefined variable.
    #[error("undefined variable `{0}`")]
    UndefinedVar(String, Span),

    /// Attempt to call a non-function type.
    #[error("type `{0}` is not callable")]
    NotCallable(Ty, Span),

    /// Function called with wrong number of arguments.
    #[error("arity mismatch: expected {expected} argument(s), got {got}")]
    ArityMismatch {
        expected: usize,
        got: usize,
        span: Span,
    },

    /// Numeric operation on non-numeric type.
    #[error("expected numeric type, got `{0}`")]
    NotNumeric(Ty, Span),

    /// Type cannot be converted to JSON.
    #[error("type `{0}` cannot be converted to JSON")]
    NotJsonable(Ty, Span),

    /// Type cannot be used as database subscript key.
    #[error("type `{0}` cannot be used as subscript key")]
    NotSubscript(Ty, Span),

    /// Type cannot be stored in database.
    #[error("type `{0}` is not storable")]
    NotStorable(Ty, Span),

    /// Struct literal missing a required field.
    #[error("missing required field `{field}` for type `{ty:?}`")]
    MissingField {
        ty: TypeId,
        field: String,
        span: Span,
    },

    /// Struct field has wrong type.
    #[error("field `{field}` has type `{got}`, expected `{expected}`")]
    FieldTypeMismatch {
        #[allow(dead_code)]
        ty: TypeId,
        field: String,
        expected: Ty,
        got: Ty,
        span: Span,
    },

    /// Occurs check failed; would create infinite type.
    #[error("infinite type: `{0}` occurs in `{1}`")]
    InfiniteType(TyVar, Ty, Span),

    /// Type annotation required but not provided.
    #[error("type annotation required")]
    MissingAnnotation(Span),

    /// Reference to unknown type name.
    #[error("unknown type `{0}`")]
    UnknownType(String, Span),

    /// Match expression does not cover all cases.
    #[error("non-exhaustive match")]
    NonExhaustiveMatch(Span),

    /// Postfix `!` on type that is not `Option` or `Result`.
    #[error("type `{0}` cannot be unwrapped; expected `Option` or `Result`")]
    NotUnwrappable(Ty, Span),

    /// Field access on non-object/struct type.
    #[error("type `{0}` has no fields")]
    NotAnObject(Ty, Span),

    /// Field not found on object or struct type.
    #[error("field `{field}` not found on type `{ty}`")]
    FieldNotFound { ty: Ty, field: String, span: Span },

    /// Tuple index on non-tuple type.
    #[error("type `{0}` is not a tuple")]
    NotATuple(Ty, Span),

    /// Tuple index out of bounds.
    #[error("tuple index {idx} is out of bounds for tuple of length {len}")]
    TupleIndexOutOfBounds { idx: u32, len: usize, span: Span },

    /// Index access on non-indexable type.
    #[error("type `{0}` is not indexable")]
    NotIndexable(Ty, Span),

    /// JSON access on non-JSON type.
    #[error("type `{0}` is not JSON; cannot use JSON access operators")]
    NotJson(Ty, Span),

    /// Empty union type.
    #[error("union type must have at least one member")]
    EmptyUnion(Span),

    /// Type is not a member of the union being matched.
    #[error("type `{member}` is not a member of union `{union_ty}`")]
    NotAUnionMember {
        member: Ty,
        union_ty: Ty,
        span: Span,
    },

    /// Invalid type cast.
    ///
    /// The source type cannot be cast to the target type. Suggests alternatives
    /// like `READ` for fallible conversion or `MATCH`/`IS` for narrowing.
    #[error("cannot cast `{from}` to `{to}`; use `READ` for fallible conversion or `MATCH`/`IS` for narrowing")]
    InvalidCast { from: Ty, to: Ty, span: Span },
}

impl TypeError {
    /// Get the source span where this error occurred.
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Mismatch { span, .. }
            | Self::UndefinedVar(_, span)
            | Self::NotCallable(_, span)
            | Self::ArityMismatch { span, .. }
            | Self::NotNumeric(_, span)
            | Self::NotJsonable(_, span)
            | Self::NotSubscript(_, span)
            | Self::NotStorable(_, span)
            | Self::MissingField { span, .. }
            | Self::FieldTypeMismatch { span, .. }
            | Self::InfiniteType(_, _, span)
            | Self::MissingAnnotation(span)
            | Self::UnknownType(_, span)
            | Self::NonExhaustiveMatch(span)
            | Self::NotUnwrappable(_, span)
            | Self::NotAnObject(_, span)
            | Self::FieldNotFound { span, .. }
            | Self::NotATuple(_, span)
            | Self::TupleIndexOutOfBounds { span, .. }
            | Self::NotIndexable(_, span)
            | Self::NotJson(_, span)
            | Self::EmptyUnion(span)
            | Self::NotAUnionMember { span, .. }
            | Self::InvalidCast { span, .. } => *span,
        }
    }
}

/// Display implementation for `Ty` (used in error messages).
impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Var(v) => write!(f, "?{}", v.idx()),
            Self::Bool => write!(f, "Bool"),
            Self::Int => write!(f, "Int"),
            Self::Float => write!(f, "Float"),
            Self::Char => write!(f, "Char"),
            Self::String => write!(f, "String"),
            Self::Unit => write!(f, "Unit"),
            Self::Time => write!(f, "Time"),
            Self::Range => write!(f, "Range"),
            Self::Json => write!(f, "Json"),
            Self::Unknown => write!(f, "Unknown"),
            Self::Error => write!(f, "<error>"),
            Self::Array(t) => write!(f, "Array[{t}]"),
            Self::Option(t) => write!(f, "Option[{t}]"),
            Self::Result(ok, err) => write!(f, "Result[{ok}, {err}]"),
            Self::Map(k, v) => write!(f, "Map[{k}, {v}]"),
            Self::Tuple(ts) => {
                write!(f, "(")?;
                ts.iter().enumerate().try_for_each(|(i, t)| {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")
                })?;
                write!(f, ")")
            }
            Self::Fn(params, ret) => {
                write!(f, "(")?;
                params.iter().enumerate().try_for_each(|(i, t)| {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")
                })?;
                write!(f, ") -> {ret}")
            }
            Self::Object(fields) => {
                write!(f, "{{")?;
                fields.iter().enumerate().try_for_each(|(i, (k, t))| {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    // Display StringId as field index; proper names require interner context
                    write!(f, "#{}: {t}", k.idx())
                })?;
                write!(f, "}}")
            }
            Self::Union(members) => {
                members.iter().enumerate().try_for_each(|(i, t)| {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{t}")
                })
            }
            Self::Named(id, args) => {
                // Use Debug format since TypeId field is private
                write!(f, "{id:?}")?;
                if !args.is_empty() {
                    write!(f, "[")?;
                    args.iter().enumerate().try_for_each(|(i, t)| {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{t}")
                    })?;
                    write!(f, "]")?;
                }
                Ok(())
            }
        }
    }
}

/// Display for `TyVar` (used in error messages).
impl fmt::Display for TyVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "?{}", self.idx())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ty_display_primitives() {
        assert_eq!(Ty::Int.to_string(), "Int");
        assert_eq!(Ty::Bool.to_string(), "Bool");
        assert_eq!(Ty::String.to_string(), "String");
    }

    #[test]
    fn ty_display_parameterized() {
        assert_eq!(Ty::Array(Box::new(Ty::Int)).to_string(), "Array[Int]");
        assert_eq!(
            Ty::Option(Box::new(Ty::String)).to_string(),
            "Option[String]"
        );
        assert_eq!(
            Ty::Result(Box::new(Ty::Int), Box::new(Ty::String)).to_string(),
            "Result[Int, String]"
        );
    }

    #[test]
    fn ty_display_fn() {
        let f = Ty::Fn(vec![Ty::Int, Ty::String], Box::new(Ty::Bool));
        assert_eq!(f.to_string(), "(Int, String) -> Bool");
    }

    #[test]
    fn ty_display_var() {
        assert_eq!(Ty::Var(TyVar::new(42)).to_string(), "?42");
    }

    #[test]
    fn error_span() {
        let span = Span::new(10, 20);
        let err = TypeError::Mismatch {
            expected: Ty::Int,
            got: Ty::String,
            span,
        };
        assert_eq!(err.span(), span);
    }
}
