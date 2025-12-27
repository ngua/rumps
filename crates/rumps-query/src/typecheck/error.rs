//! Type error definitions for static type checking.
//!
//! These errors are produced during the type checking phase (compile-time),
//! distinct from `Error::RuntimeType` which occurs during interpretation.

use std::fmt;

use thiserror::Error;

use super::ty::{Ty, TyVar};
use crate::intern::{StringId, StringInterner};
use crate::value::{TypeRegistry, ValueArena};
use crate::{Span, TypeId};

/// Context for pretty-printing types in error messages.
///
/// Provides access to type names (via `TypeRegistry`) and field/variable names
/// (via `StringInterner`).
pub(crate) struct TyPrinter<'a> {
    registry: &'a TypeRegistry,
    arena: &'a ValueArena,
    strings: &'a StringInterner,
}

impl<'a> TyPrinter<'a> {
    /// Create a new type printer with context.
    ///
    /// The `strings` interner should be the one used during type checking
    /// (from `TypeEnv`), as it may contain strings interned after the
    /// `ValueArena` was created.
    pub(crate) fn new(
        registry: &'a TypeRegistry,
        arena: &'a ValueArena,
        strings: &'a StringInterner,
    ) -> Self {
        Self {
            registry,
            arena,
            strings,
        }
    }

    /// Format a type as a human-readable string.
    pub(crate) fn format(&self, ty: &Ty) -> String {
        self.format_inner(ty, &mut TyVarNamer::new())
    }

    fn format_inner(&self, ty: &Ty, namer: &mut TyVarNamer) -> String {
        match ty {
            Ty::Var(v) => namer.name(*v),
            Ty::Bool => "Bool".to_owned(),
            Ty::Int => "Int".to_owned(),
            Ty::Float => "Float".to_owned(),
            Ty::Char => "Char".to_owned(),
            Ty::String => "String".to_owned(),
            Ty::Unit => "Unit".to_owned(),
            Ty::Time => "Time".to_owned(),
            Ty::Range => "Range".to_owned(),
            Ty::Json => "Json".to_owned(),
            Ty::Ordering => "Ordering".to_owned(),
            Ty::Unknown => "_".to_owned(),
            Ty::Error => "<error>".to_owned(),
            Ty::Array(t) => format!("Array[{}]", self.format_inner(t, namer)),
            Ty::Option(t) => format!("Option[{}]", self.format_inner(t, namer)),
            Ty::Result(ok, err) => {
                format!(
                    "Result[{}, {}]",
                    self.format_inner(ok, namer),
                    self.format_inner(err, namer)
                )
            }
            Ty::Map(k, v) => {
                format!(
                    "Map[{}, {}]",
                    self.format_inner(k, namer),
                    self.format_inner(v, namer)
                )
            }
            Ty::Tuple(ts) => {
                let parts: Vec<_> =
                    ts.iter().map(|t| self.format_inner(t, namer)).collect();
                format!("({})", parts.join(", "))
            }
            Ty::Fn(params, ret) => {
                let ps: Vec<_> = params
                    .iter()
                    .map(|t| self.format_inner(t, namer))
                    .collect();
                format!(
                    "({}) -> {}",
                    ps.join(", "),
                    self.format_inner(ret, namer)
                )
            }
            Ty::Object(fields) => {
                let parts: Vec<_> = fields
                    .iter()
                    .map(|(k, t)| {
                        let name = self.strings.get(*k).unwrap_or("<unknown>");
                        format!("{}: {}", name, self.format_inner(t, namer))
                    })
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            Ty::Union(members) => {
                let parts: Vec<_> = members
                    .iter()
                    .map(|t| self.format_inner(t, namer))
                    .collect();
                parts.join(" | ")
            }
            Ty::Named(id, args) => {
                let name = self
                    .registry
                    .type_name(*id, self.arena)
                    .unwrap_or("<unknown type>");
                if args.is_empty() {
                    name.to_owned()
                } else {
                    let ps: Vec<_> = args
                        .iter()
                        .map(|t| self.format_inner(t, namer))
                        .collect();
                    format!("{}[{}]", name, ps.join(", "))
                }
            }
        }
    }

    /// Format a `TypeId` as a type name.
    pub(crate) fn type_name(&self, id: TypeId) -> String {
        self.registry
            .type_name(id, self.arena)
            .map(str::to_owned)
            .unwrap_or_else(|| "<unknown type>".to_owned())
    }
}

/// Helper for assigning readable names to type variables.
///
/// Maps `TyVar(0)` to `T`, `TyVar(1)` to `U`, etc. Falls back to `T0`, `T1`, etc.
/// for large indices.
struct TyVarNamer {
    /// Maps type variable indices to assigned names.
    names: std::collections::HashMap<u32, String>,
    /// Next letter to assign (starts at 'T').
    next: u8,
}

impl TyVarNamer {
    const LETTERS: &'static [u8] = b"TUVWXYZABCDEFGHIJKLMNOPQRS";

    fn new() -> Self {
        Self {
            names: std::collections::HashMap::new(),
            next: 0,
        }
    }

    fn name(&mut self, v: TyVar) -> String {
        let idx = v.idx();
        self.names
            .entry(idx)
            .or_insert_with(|| {
                let n = self.next as usize;
                self.next += 1;
                Self::LETTERS.get(n).map_or_else(
                    || format!("T{}", n - Self::LETTERS.len()),
                    |&c| String::from(c as char),
                )
            })
            .clone()
    }
}

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

    /// Custom error with a message.
    ///
    /// Used for errors that don't fit into the other categories.
    #[error("{msg}")]
    Custom { msg: String, span: Span },
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
            | Self::InvalidCast { span, .. }
            | Self::Custom { span, .. } => *span,
        }
    }

    /// Format this error with type names resolved using the given context.
    pub(crate) fn format_with(&self, p: &TyPrinter<'_>) -> FormattedTypeError {
        let (msg, help) = match self {
            Self::Mismatch { expected, got, .. } => (
                format!(
                    "type mismatch: expected `{}`, got `{}`",
                    p.format(expected),
                    p.format(got)
                ),
                Self::mismatch_help(expected, got),
            ),
            Self::UndefinedVar(name, _) => {
                (format!("undefined variable `{name}`"), None)
            }
            Self::NotCallable(ty, _) => {
                (format!("type `{}` is not callable", p.format(ty)), None)
            }
            Self::ArityMismatch { expected, got, .. } => (
                format!("expected {expected} argument(s), got {got}"),
                None,
            ),
            Self::NotNumeric(ty, _) => (
                format!("expected numeric type, got `{}`", p.format(ty)),
                Self::numeric_help(ty),
            ),
            Self::NotJsonable(ty, _) => (
                format!("type `{}` cannot be converted to JSON", p.format(ty)),
                None,
            ),
            Self::NotSubscript(ty, _) => (
                format!(
                    "type `{}` cannot be used as subscript key",
                    p.format(ty)
                ),
                Some("subscript keys must be `Bool`, `Int`, `Float`, `Char`, `String`, or `Json`".to_owned()),
            ),
            Self::NotStorable(ty, _) => (
                format!("type `{}` is not storable", p.format(ty)),
                Some("storable types are `Bool`, `Int`, `Float`, `Char`, `String`, or `Json`".to_owned()),
            ),
            Self::MissingField { ty, field, .. } => (
                format!(
                    "missing required field `{}` for type `{}`",
                    field,
                    p.type_name(*ty)
                ),
                None,
            ),
            Self::FieldTypeMismatch {
                field,
                expected,
                got,
                ..
            } => (
                format!(
                    "field `{}` has type `{}`, expected `{}`",
                    field,
                    p.format(got),
                    p.format(expected)
                ),
                None,
            ),
            Self::InfiniteType(v, ty, _) => {
                let mut namer = TyVarNamer::new();
                (
                    format!(
                        "infinite type: `{}` occurs in `{}`",
                        namer.name(*v),
                        p.format(ty)
                    ),
                    Some("this would create a recursive type".to_owned()),
                )
            }
            Self::MissingAnnotation(_) => (
                "cannot infer type; add a type annotation".to_owned(),
                Some("e.g., `LET x: MyType = ...`".to_owned()),
            ),
            Self::UnknownType(name, _) => {
                (format!("unknown type `{name}`"), None)
            }
            Self::NonExhaustiveMatch(_) => (
                "non-exhaustive match".to_owned(),
                Some("add a `_` pattern to handle remaining cases".to_owned()),
            ),
            Self::NotUnwrappable(ty, _) => (
                format!(
                    "type `{}` cannot be unwrapped",
                    p.format(ty)
                ),
                Some("only `Option` and `Result` can be unwrapped with `!`".to_owned()),
            ),
            Self::NotAnObject(ty, _) => {
                (format!("type `{}` has no fields", p.format(ty)), None)
            }
            Self::FieldNotFound { ty, field, .. } => (
                format!("field `{}` not found on type `{}`", field, p.format(ty)),
                None,
            ),
            Self::NotATuple(ty, _) => (
                format!("type `{}` is not a tuple", p.format(ty)),
                None,
            ),
            Self::TupleIndexOutOfBounds { idx, len, .. } => (
                format!(
                    "tuple index {idx} is out of bounds for tuple of length {len}"
                ),
                None,
            ),
            Self::NotIndexable(ty, _) => (
                format!("type `{}` is not indexable", p.format(ty)),
                Some("indexable types are `Array`, `Map`, `String`, and `Json`".to_owned()),
            ),
            Self::NotJson(ty, _) => (
                format!(
                    "type `{}` is not JSON; cannot use JSON access operators",
                    p.format(ty)
                ),
                Some("use `.field` for objects or `[idx]` for arrays".to_owned()),
            ),
            Self::EmptyUnion(_) => (
                "union type must have at least one member".to_owned(),
                None,
            ),
            Self::NotAUnionMember { member, union_ty, .. } => (
                format!(
                    "type `{}` is not a member of union `{}`",
                    p.format(member),
                    p.format(union_ty)
                ),
                None,
            ),
            Self::InvalidCast { from, to, .. } => (
                format!(
                    "cannot cast `{}` to `{}`",
                    p.format(from),
                    p.format(to)
                ),
                Some("use `READ` for fallible conversion or `MATCH`/`IS` for narrowing".to_owned()),
            ),
            Self::Custom { msg, .. } => (msg.clone(), None),
        };

        FormattedTypeError {
            message: msg,
            help,
            span: self.span(),
        }
    }

    /// Generate help text for type mismatch errors.
    fn mismatch_help(expected: &Ty, got: &Ty) -> Option<String> {
        match (expected, got) {
            // String concatenation hint
            (Ty::Int | Ty::Float, Ty::String)
            | (Ty::String, Ty::Int | Ty::Float) => {
                Some("use `++` for string concatenation, not `+`".to_owned())
            }
            // Unwrap hint
            (inner, Ty::Option(opt_inner)) if inner == opt_inner.as_ref() => {
                Some(
                    "use `!` to unwrap the `Option`, or handle with `MATCH`"
                        .to_owned(),
                )
            }
            (inner, Ty::Result(ok_inner, _)) if inner == ok_inner.as_ref() => {
                Some(
                    "use `!` to unwrap the `Result`, or handle with `MATCH`"
                        .to_owned(),
                )
            }
            _ => None,
        }
    }

    /// Generate help text for numeric type errors.
    fn numeric_help(ty: &Ty) -> Option<String> {
        match ty {
            Ty::String => Some(
                "cannot use `+` on strings; use `++` for concatenation"
                    .to_owned(),
            ),
            Ty::Bool => {
                Some("cannot use arithmetic operators on `Bool`".to_owned())
            }
            _ => None,
        }
    }
}

/// A formatted type error with resolved type names.
///
/// Created from `TypeError` when context (registry, interner) is available.
/// Used for display in error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormattedTypeError {
    /// The formatted error message.
    pub(crate) message: String,
    /// Optional help/suggestion text.
    pub(crate) help: Option<String>,
    /// Source span where the error occurred.
    pub(crate) span: Span,
}

impl fmt::Display for FormattedTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FormattedTypeError {}

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
            Self::Ordering => write!(f, "Ordering"),
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
