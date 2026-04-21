//! Type error definitions for static type checking.
//!
//! These errors are produced during the type checking phase (compile-time),
//! distinct from `Error::RuntimeType` which occurs during interpretation.

use std::fmt;

use thiserror::Error;

use super::ty::{ClassRegistry, Ty, TyArena, TyId, TyVar, TypeClass};
use crate::intern::StringInterner;
use crate::value::{TypeRegistry, ValueArena};
use crate::{ClassId, Span, StringId, TypeId};

/// Context for pretty-printing types in error messages.
///
/// Provides access to type names (via `TypeRegistry`) and field/variable names
/// (via `StringInterner`).
pub(crate) struct TyPrinter<'a> {
    registry: &'a TypeRegistry,
    val_arena: &'a ValueArena,
    pub(crate) ty_arena: &'a TyArena,
    pub(crate) strings: &'a StringInterner,
    class_registry: &'a ClassRegistry,
    numeric_vars: &'a [TyVar],
}

impl<'a> TyPrinter<'a> {
    /// Create a new type printer with context.
    ///
    /// The `strings` interner should be the one used during type checking
    /// (from `TypeEnv`), as it may contain strings interned after the
    /// `ValueArena` was created. The `numeric_vars` are type variables from
    /// integer literals; they display as `Int` (the default) in errors.
    pub(crate) fn new(
        registry: &'a TypeRegistry,
        val_arena: &'a ValueArena,
        ty_arena: &'a TyArena,
        strings: &'a StringInterner,
        class_registry: &'a ClassRegistry,
        numeric_vars: &'a [TyVar],
    ) -> Self {
        Self {
            registry,
            val_arena,
            ty_arena,
            strings,
            class_registry,
            numeric_vars,
        }
    }

    /// Resolve a `ClassId` to a human-readable name.
    pub(crate) fn class_name(&self, id: ClassId) -> &str {
        self.strings
            .get(self.class_registry.name(id))
            .unwrap_or(id.name())
    }

    /// Format a type as a human-readable string.
    pub(crate) fn format(&self, id: TyId) -> String {
        self.format_inner(id, &mut TyVarNamer::new())
    }

    fn format_inner(&self, id: TyId, namer: &mut TyVarNamer) -> String {
        match self.ty_arena.get(id) {
            // Numeric vars (from integer literals) display as Int (the default)
            Ty::Var(v) if self.numeric_vars.contains(v) => "Int".to_owned(),
            Ty::Var(v) => namer.name(*v),
            Ty::Bool => "Bool".to_owned(),
            Ty::Int => "Int".to_owned(),
            Ty::Word => "Word".to_owned(),
            Ty::Float => "Float".to_owned(),
            Ty::Char => "Char".to_owned(),
            Ty::String => "String".to_owned(),
            Ty::Unit => "Unit".to_owned(),
            Ty::Time => "Time".to_owned(),
            Ty::Range => "Range".to_owned(),
            Ty::Json => "Json".to_owned(),
            Ty::Ordering => "Ordering".to_owned(),
            Ty::DataStatus => "DataStatus".to_owned(),
            Ty::FilePath => "FilePath".to_owned(),
            Ty::Path => "Path".to_owned(),
            Ty::Regex => "Regex".to_owned(),
            Ty::RuntimeError => "Error".to_owned(),
            Ty::Unknown => "_".to_owned(),
            Ty::Error => "<error>".to_owned(),
            Ty::Array(t) => {
                format!("Array[{}]", self.format_inner(*t, namer))
            }
            Ty::Option(t) => {
                format!("Option[{}]", self.format_inner(*t, namer))
            }
            Ty::Result(ok, err) => {
                format!(
                    "Result[{}, {}]",
                    self.format_inner(*ok, namer),
                    self.format_inner(*err, namer)
                )
            }
            Ty::Map(k, v) => {
                format!(
                    "Map[{}, {}]",
                    self.format_inner(*k, namer),
                    self.format_inner(*v, namer)
                )
            }
            Ty::Tuple(ts) => {
                let parts: Vec<_> =
                    ts.iter().map(|&t| self.format_inner(t, namer)).collect();
                // Single-element tuples need trailing comma: `(Int,)`
                let trail = if ts.len() == 1 { "," } else { "" };
                format!("({}{})", parts.join(", "), trail)
            }
            Ty::Fn(params, ret) => {
                let ps: Vec<_> = params
                    .iter()
                    .map(|&t| self.format_inner(t, namer))
                    .collect();
                format!(
                    "({}) -> {}",
                    ps.join(", "),
                    self.format_inner(*ret, namer)
                )
            }
            Ty::Object(fields) => {
                let parts: Vec<_> = fields
                    .iter()
                    .map(|(&k, &t)| {
                        let name = self.strings.get(k).unwrap_or("<unknown>");
                        format!("{}: {}", name, self.format_inner(t, namer))
                    })
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            Ty::Union(prov, members) => {
                if let Some(id) = prov {
                    self.registry
                        .type_name(*id, self.val_arena)
                        .unwrap_or("<unknown type>")
                        .to_owned()
                } else {
                    let parts: Vec<_> = members
                        .iter()
                        .map(|&t| self.format_inner(t, namer))
                        .collect();
                    parts.join(" | ")
                }
            }
            Ty::Named(id, args) => {
                let name = self
                    .registry
                    .type_name(*id, self.val_arena)
                    .unwrap_or("<unknown type>");
                if args.is_empty() {
                    name.to_owned()
                } else {
                    let ps: Vec<_> = args
                        .iter()
                        .map(|&t| self.format_inner(t, namer))
                        .collect();
                    format!("{}[{}]", name, ps.join(", "))
                }
            }
            Ty::Local => "Local".to_owned(),
            Ty::Global => "Global".to_owned(),
            Ty::Apply(v, args) => {
                let vname = namer.name(*v);
                let ps: Vec<_> =
                    args.iter().map(|&t| self.format_inner(t, namer)).collect();
                format!("{}[{}]", vname, ps.join(", "))
            }
            Ty::AssocType(v, _class, name) => {
                let vname = namer.name(*v);
                let assoc_name = self.strings.get(*name).unwrap_or("<unknown>");
                format!("{}.{}", vname, assoc_name)
            }
        }
    }

    /// Format a `TypeId` as a type name.
    pub(crate) fn type_name(&self, id: TypeId) -> String {
        self.registry
            .type_name(id, self.val_arena)
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
    Mismatch {
        expected: TyId,
        got: TyId,
        span: Span,
    },

    /// Reference to undefined variable.
    #[error("undefined variable `{0}`")]
    UndefinedVar(String, Span),

    /// Attempt to call a non-function type.
    #[error("type `{0}` is not callable")]
    NotCallable(TyId, Span),

    /// Function called with wrong number of arguments.
    #[error("arity mismatch: expected {expected} argument(s), got {got}")]
    ArityMismatch {
        expected: usize,
        got: usize,
        span: Span,
    },

    /// Function called with too many arguments (more than arity).
    #[error("too many arguments: expected at most {expected}, got {got}")]
    TooManyArguments {
        expected: usize,
        got: usize,
        span: Span,
    },

    /// Function called with zero arguments when it expects some.
    #[error("missing arguments: expected {expected} argument(s), got 0")]
    ZeroArguments { expected: usize, span: Span },

    /// Type does not satisfy a class (Numeric, Into[Json], etc.).
    #[error("type `{1}` does not satisfy `{0}` class")]
    UnsatisfiedClass(TypeClass<TyId>, TyId, Span),

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
        expected: TyId,
        got: TyId,
        span: Span,
    },

    /// Occurs check failed; would create infinite type.
    #[error("infinite type: `{0}` occurs in `{1}`")]
    InfiniteType(TyVar, TyId, Span),

    /// Type annotation required but not provided.
    #[error("type annotation required")]
    MissingAnnotation(Span),

    /// Reference to unknown type name.
    #[error("unknown type `{0}`")]
    UnknownType(String, Span),

    /// Wrong number of type arguments for parameterized type.
    #[error("type `{name}` expects {expected} type argument(s), got {got}")]
    TypeArityMismatch {
        name: String,
        expected: usize,
        got: usize,
        span: Span,
    },

    /// Match expression does not cover all cases.
    #[error("non-exhaustive match")]
    NonExhaustiveMatch(Span),

    /// Field access on non-object type.
    #[error("type `{0}` has no fields")]
    NotAnObject(TyId, Span),

    /// Field not found on object type.
    #[error("field `{field}` not found on type `{ty}`")]
    FieldNotFound { ty: TyId, field: String, span: Span },

    /// Tuple index on non-tuple type.
    #[error("type `{0}` is not a tuple")]
    NotATuple(TyId, Span),

    /// Array pattern in let binding (only allowed in match).
    #[error("array destructuring is only allowed in `match` expressions")]
    ArrayPatternInLet(Span),

    /// Tuple index out of bounds.
    #[error("tuple index {idx} is out of bounds for tuple of length {len}")]
    TupleIndexOutOfBounds { idx: u32, len: usize, span: Span },

    /// Spread on non-array type.
    #[error("cannot spread type `{0}` in array literal; expected `Array`")]
    NotAnArray(TyId, Span),

    /// Spread on non-object type.
    #[error("cannot spread type `{0}` in object literal; expected `Object`")]
    NotAnObjectSpread(TyId, Span),

    /// JSON access on non-JSON type.
    #[error("type `{0}` is not JSON; cannot use JSON access operators")]
    NotJson(TyId, Span),

    /// Empty union type.
    #[error("union type must have at least one member")]
    EmptyUnion(Span),

    /// Negative literal assigned to `Word` type.
    #[error("`Word` cannot hold negative values")]
    NegativeWord(Span),

    /// Type is not a member of the union being matched.
    #[error("type `{member}` is not a member of union `{union_ty}`")]
    NotAUnionMember {
        member: TyId,
        union_ty: TyId,
        span: Span,
    },

    /// Variant pattern incompatible with scrutinee type.
    ///
    /// Example: `if x is Option.Some(v)` where `x: F` and `F: Fallible[T]`.
    /// Type variables cannot be refined by variant patterns since the concrete
    /// type is unknown at compile time.
    #[error(
        "cannot match `{pattern_ty}` pattern against type `{scrutinee_ty}`"
    )]
    IncompatibleVariantPattern {
        pattern_ty: String,
        scrutinee_ty: TyId,
        span: Span,
    },

    /// Attempt to narrow a union type via type annotation.
    ///
    /// Union types cannot be narrowed to a member type through `let`
    /// annotations; use `match`/`is` for runtime type narrowing instead.
    #[error(
        "cannot narrow union `{union_ty}` to `{narrow_ty}` via type annotation"
    )]
    UnionNarrowing {
        union_ty: TyId,
        narrow_ty: TyId,
        span: Span,
    },

    /// Invalid type cast.
    ///
    /// The source type cannot be cast to the target type. Suggests alternatives
    /// like `read` for fallible conversion or `match`/`is` for narrowing.
    #[error("cannot cast `{from}` to `{to}`; use `read` for fallible conversion or `match`/`is` for narrowing")]
    InvalidCast { from: TyId, to: TyId, span: Span },

    /// Invalid `read` conversion.
    ///
    /// The source type cannot be fallibly converted to the target type via `read`.
    /// Function types, regex, and refs cannot be source or target of `read`.
    #[error("cannot `read` `{from}` as `{to}`")]
    InvalidRead { from: TyId, to: TyId, span: Span },

    /// Custom error with a message.
    ///
    /// Used for errors that don't fit into the other categories.
    #[error("{msg}")]
    Custom { msg: String, span: Span },

    /// Invalid regex pattern.
    #[error("invalid regex pattern `{0}`: {1}")]
    InvalidRegex(String, String, Span),

    /// Module member not found (i.e. module resolves correctly but user calls or
    /// references something that is not defined in that module).
    #[error("`{name}` not found in module `{module}`")]
    NotFoundInModule {
        module: String,
        name: String,
        span: Span,
    },

    /// Private module member access from outside the module.
    #[error("`{name}` is private in module `{module}`")]
    PrivateAccess {
        module: String,
        name: String,
        span: Span,
    },

    /// Unknown class name in class method call.
    #[error("unknown class `{0}`")]
    UnknownClass(String, Span),

    /// Unknown method name for a class (with `StringId`s).
    #[error("class has no such method")]
    UnknownMethodId {
        class: StringId,
        method: StringId,
        span: Span,
    },

    /// Convert method used as first-class value without type parameters.
    ///
    /// Convert methods (`Wrappable:wrap`, `Into:into`, `TryInto:try-into`) require
    /// explicit type parameters when used as values because the target type
    /// cannot be inferred from the reference site alone.
    #[error("convert method `{class}:{method}` requires type parameter")]
    ConvertMethodNeedsType {
        class: String,
        method: String,
        span: Span,
    },

    /// Duplicate class instance declaration.
    ///
    /// A type can only have one instance of each class.
    #[error("duplicate `{class}` instance for type `{type_id:?}`")]
    DuplicateInstance {
        class: ClassId,
        type_id: TypeId,
        span: Span,
    },

    /// Attempt to implement a class for a builtin type.
    ///
    /// Users can only implement classes for their own types (`type`, `newtype`, `union`).
    #[error("cannot implement `{class}` for builtin type `{type_id:?}`")]
    BuiltinInstanceForbidden {
        class: ClassId,
        type_id: TypeId,
        span: Span,
    },

    /// Missing required method in class instance declaration.
    ///
    /// All methods defined by a class must be implemented.
    #[error("missing required method `{method}` for class `{class}`")]
    MissingInstanceMethod {
        class: ClassId,
        method: String,
        required_hint: String,
        span: Span,
    },

    /// Method signature mismatch in class instance declaration.
    ///
    /// The user-provided method signature does not match the class definition.
    #[error("method `{method}` of class `{class}` has wrong arity: expected {expected} parameter(s), got {got}")]
    MethodSignatureMismatch {
        class: ClassId,
        method: String,
        expected: usize,
        got: usize,
        span: Span,
    },

    /// Missing required associated type in instance definition.
    ///
    /// Classes like `Indexable` require associated type definitions (e.g., `newtype Index = Int`).
    #[error("missing required associated type for class `{class}`")]
    MissingAssocType {
        class: ClassId,
        assoc: StringId,
        span: Span,
    },

    /// Unknown associated type for a class.
    ///
    /// The instance defines an associated type that doesn't exist in the class.
    #[error("class `{class}` has no associated type `{assoc}`")]
    UnknownAssocTypeForClass {
        class: ClassId,
        assoc: String,
        span: Span,
    },

    /// Type does not have the specified associated type.
    ///
    /// Attempting to project an associated type from a type that doesn't support it.
    #[error("type `{ty}` has no associated type `#{}`", assoc.idx())]
    UnknownAssocType {
        ty: TyId,
        assoc: StringId,
        span: Span,
    },

    /// Associated type constraint not satisfied.
    ///
    /// The concrete type for an associated type doesn't satisfy the required constraint.
    #[error(
        "associated type `{assoc}` must satisfy `{constraint}`, got `{actual}`"
    )]
    AssocTypeConstraint {
        assoc: String,
        constraint: TypeClass<TyId>,
        actual: TyId,
        span: Span,
    },

    /// Bare associated type reference outside class context.
    ///
    /// Unqualified associated types like `:Index` can only be used inside
    /// `class ... FOR ...` instance definitions where the class context is known.
    #[error(
        "associated type `:{name}` can only be used inside a class instance"
    )]
    AssocTypeOutsideClass { name: String, span: Span },

    /// Class does not define the specified associated type.
    ///
    /// The referenced associated type doesn't exist in the class.
    #[error("class `{class}` has no associated type `#{}`", name.idx())]
    NoSuchAssocType {
        class: ClassId,
        name: StringId,
        span: Span,
    },

    /// Class instance exists but is not in scope because its module is not imported.
    ///
    /// Emitted when a class method is called on a type that has an instance
    /// defined in a module, but that module has not been imported.
    #[error("no `{class}` instance for `{type_id:?}` in scope")]
    InstanceNotImported {
        class: ClassId,
        type_id: TypeId,
        module: String,
        span: Span,
    },

    /// Missing required superclass instance.
    ///
    /// When implementing a subclass (e.g., `Chainable`), all transitive
    /// superclasses (e.g., `Wrappable`, `Fallible`) must already have instances.
    #[error("cannot implement `{class}` for `{type_id:?}`; missing required superclass instance `{superclass}`")]
    MissingSuperclassInstance {
        class: ClassId,
        superclass: ClassId,
        type_id: TypeId,
        span: Span,
    },

    /// Simple or HKT class given type arguments it does not accept.
    #[error("class does not accept type arguments")]
    ClassRejectsArg { class: ClassId, span: Span },

    /// Parameterized class missing required type arguments.
    #[error("class requires type arguments")]
    ClassRequiresArg { class: ClassId, span: Span },

    /// Top-level expression statement outside of `main`.
    ///
    /// Scripts must define a `main` function as the entry point. Expression
    /// statements (including `write`, function calls, etc.) must appear inside
    /// `main` or other functions, not at the top level.
    #[error("top-level expression statements are not allowed; move code into `main`")]
    TopLevelExpr(Span),

    /// Missing required `main` function.
    ///
    /// Every script must define a `main` function as the entry point.
    /// Use `--interactive` mode to run without a `main` function.
    #[error("missing required `main` function")]
    MissingMain(Span),

    /// Invalid `main` function signature.
    ///
    /// The `main` function must have signature `() -> Unit`; it takes no
    /// arguments and returns nothing.
    #[error("`main` must have signature `() -> Unit`; got `{got}`")]
    InvalidMainSignature { got: TyId, span: Span },

    /// Function type used in an `is` pattern.
    ///
    /// Runtime type narrowing via `is` cannot inspect the signature of an
    /// opaque callable (class method reference, module function reference,
    /// partial application, etc.), so function types are not permitted as
    /// `is` targets. Bind the value and call it instead.
    #[error("function types cannot appear in `is` patterns")]
    FnTypeInPattern(Span),

    /// Body of a function uses a class operation on an explicitly-declared
    /// type parameter that does not have the corresponding constraint.
    #[error("type parameter `{param}` requires `{class}` constraint")]
    MissingTypeParamConstraint {
        param: String,
        class: TypeClass<TyId>,
        span: Span,
    },
}

impl TypeError {
    /// Get the source span where this error occurred.
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Mismatch { span, .. }
            | Self::UndefinedVar(_, span)
            | Self::NotCallable(_, span)
            | Self::ArityMismatch { span, .. }
            | Self::TooManyArguments { span, .. }
            | Self::ZeroArguments { span, .. }
            | Self::UnsatisfiedClass(_, _, span)
            | Self::MissingField { span, .. }
            | Self::FieldTypeMismatch { span, .. }
            | Self::InfiniteType(_, _, span)
            | Self::MissingAnnotation(span)
            | Self::UnknownType(_, span)
            | Self::TypeArityMismatch { span, .. }
            | Self::NonExhaustiveMatch(span)
            | Self::NotAnObject(_, span)
            | Self::FieldNotFound { span, .. }
            | Self::NotATuple(_, span)
            | Self::ArrayPatternInLet(span)
            | Self::TupleIndexOutOfBounds { span, .. }
            | Self::NotAnArray(_, span)
            | Self::NotAnObjectSpread(_, span)
            | Self::NotJson(_, span)
            | Self::EmptyUnion(span)
            | Self::NegativeWord(span)
            | Self::NotAUnionMember { span, .. }
            | Self::IncompatibleVariantPattern { span, .. }
            | Self::UnionNarrowing { span, .. }
            | Self::InvalidCast { span, .. }
            | Self::InvalidRead { span, .. }
            | Self::Custom { span, .. }
            | Self::InvalidRegex(_, _, span)
            | Self::NotFoundInModule { span, .. }
            | Self::PrivateAccess { span, .. }
            | Self::UnknownClass(_, span)
            | Self::UnknownMethodId { span, .. }
            | Self::ConvertMethodNeedsType { span, .. }
            | Self::DuplicateInstance { span, .. }
            | Self::BuiltinInstanceForbidden { span, .. }
            | Self::MissingInstanceMethod { span, .. }
            | Self::MethodSignatureMismatch { span, .. }
            | Self::MissingAssocType { span, .. }
            | Self::UnknownAssocTypeForClass { span, .. }
            | Self::UnknownAssocType { span, .. }
            | Self::AssocTypeConstraint { span, .. }
            | Self::AssocTypeOutsideClass { span, .. }
            | Self::NoSuchAssocType { span, .. }
            | Self::InstanceNotImported { span, .. }
            | Self::MissingSuperclassInstance { span, .. }
            | Self::ClassRejectsArg { span, .. }
            | Self::ClassRequiresArg { span, .. }
            | Self::TopLevelExpr(span)
            | Self::MissingMain(span)
            | Self::InvalidMainSignature { span, .. }
            | Self::FnTypeInPattern(span)
            | Self::MissingTypeParamConstraint { span, .. } => *span,
        }
    }

    /// Format this error with type names resolved using the given context.
    pub(crate) fn format_with(&self, p: &TyPrinter<'_>) -> FormattedTypeError {
        let (msg, help) = match self {
            Self::Mismatch { expected, got, .. } => (
                format!(
                    "type mismatch: expected `{}`, got `{}`",
                    p.format(*expected),
                    p.format(*got)
                ),
                None,
            ),
            Self::UndefinedVar(name, _) => {
                (format!("undefined variable `{name}`"), None)
            }
            Self::NotCallable(ty, _) => {
                (format!("type `{}` is not callable", p.format(*ty)), None)
            }
            Self::ArityMismatch { expected, got, .. } => (
                format!("expected {expected} argument(s), got {got}"),
                None,
            ),
            Self::TooManyArguments { expected, got, .. } => (
                format!(
                    "too many arguments: expected at most {expected}, got {got}"
                ),
                None,
            ),
            Self::ZeroArguments { expected, .. } => (
                format!(
                    "missing arguments: expected {expected} argument(s), got 0"
                ),
                None,
            ),
            Self::UnsatisfiedClass(class, ty, _) => (
                format!(
                    "type `{}` does not satisfy `{}` class",
                    p.format(*ty),
                    p.class_name(class.tag())
                ),
                None,
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
                    p.format(*got),
                    p.format(*expected)
                ),
                None,
            ),
            Self::InfiniteType(v, ty, _) => {
                let mut namer = TyVarNamer::new();
                (
                    format!(
                        "infinite type: `{}` occurs in `{}`",
                        namer.name(*v),
                        p.format(*ty)
                    ),
                    Some("this would create a recursive type".to_owned()),
                )
            }
            Self::MissingAnnotation(_) => (
                "cannot infer type; add a type annotation".to_owned(),
                Some("e.g., `let x: MyType = ...`".to_owned()),
            ),
            Self::UnknownType(name, _) => {
                (format!("unknown type `{name}`"), None)
            }
            Self::TypeArityMismatch {
                name,
                expected,
                got,
                ..
            } => (
                format!(
                    "type `{name}` expects {expected} type argument(s), got {got}"
                ),
                None,
            ),
            Self::NonExhaustiveMatch(_) => (
                "non-exhaustive match".to_owned(),
                Some("add a `_` pattern to handle remaining cases".to_owned()),
            ),
            Self::NotAnObject(ty, _) => {
                (format!("type `{}` has no fields", p.format(*ty)), None)
            }
            Self::FieldNotFound { ty, field, .. } => (
                format!(
                    "field `{}` not found on type `{}`",
                    field,
                    p.format(*ty)
                ),
                None,
            ),
            Self::NotATuple(ty, _) => (
                format!("type `{}` is not a tuple", p.format(*ty)),
                None,
            ),
            Self::ArrayPatternInLet(_) => (
                "array destructuring is only allowed in `match` expressions".to_owned(),
                Some("use `match arr { [a, b, ..] => ... }` instead".to_owned()),
            ),
            Self::TupleIndexOutOfBounds { idx, len, .. } => (
                format!(
                    "tuple index {idx} is out of bounds for tuple of length {len}"
                ),
                None,
            ),
            Self::NotAnArray(ty, _) => (
                format!(
                    "cannot spread type `{}` in array literal",
                    p.format(*ty)
                ),
                Some("spread requires an `Array` type".to_owned()),
            ),
            Self::NotAnObjectSpread(ty, _) => (
                format!(
                    "cannot spread type `{}` in object literal",
                    p.format(*ty)
                ),
                Some("spread requires an `Object` type".to_owned()),
            ),
            Self::NotJson(ty, _) => (
                format!(
                    "type `{}` is not JSON; cannot use JSON access operators",
                    p.format(*ty)
                ),
                Some("use `.field` for objects or `[idx]` for arrays".to_owned()),
            ),
            Self::EmptyUnion(_) => (
                "union type must have at least one member".to_owned(),
                None,
            ),
            Self::NegativeWord(_) => (
                "`Word` cannot hold negative values".to_owned(),
                Some("use `Int` for signed integers".to_owned()),
            ),
            Self::NotAUnionMember { member, union_ty, .. } => (
                format!(
                    "type `{}` is not a member of union `{}`",
                    p.format(*member),
                    p.format(*union_ty)
                ),
                None,
            ),
            Self::IncompatibleVariantPattern {
                pattern_ty,
                scrutinee_ty,
                ..
            } => {
                let scrutinee_str = p.format(*scrutinee_ty);
                let help =
                    if matches!(p.ty_arena.get(*scrutinee_ty), Ty::Var(_)) {
                        Some(format!(
                            "type variables cannot be refined by variant patterns; \
                             `{scrutinee_str}` could be any type satisfying its constraints"
                        ))
                    } else {
                        Some(format!(
                            "expected `{pattern_ty}` type, found `{scrutinee_str}`"
                        ))
                    };
                (
                    format!(
                        "cannot match `{pattern_ty}` pattern against type `{scrutinee_str}`"
                    ),
                    help,
                )
            }
            Self::UnionNarrowing { union_ty, narrow_ty, .. } => (
                format!(
                    "cannot narrow union `{}` to `{}` via type annotation",
                    p.format(*union_ty),
                    p.format(*narrow_ty)
                ),
                Some("use `match`/`is` for runtime type narrowing".to_owned()),
            ),
            Self::InvalidCast { from, to, .. } => (
                format!(
                    "cannot cast `{}` to `{}`",
                    p.format(*from),
                    p.format(*to)
                ),
                Some("use `read` for fallible conversion or `match`/`is` for narrowing".to_owned()),
            ),
            Self::InvalidRead { from, to, .. } => (
                format!(
                    "cannot `read` `{}` as `{}`",
                    p.format(*from),
                    p.format(*to)
                ),
                Some("function types, regex, and refs cannot be used with `read`".to_owned()),
            ),
            Self::Custom { msg, .. } => (msg.clone(), None),
            Self::InvalidRegex(pattern, err, _) => (
                format!("invalid regex pattern `/{pattern}/`: {err}"),
                None,
            ),
            Self::NotFoundInModule { module, name, .. } => (
                format!("`{name}` not found in module `{module}`"),
                None,
            ),
            Self::PrivateAccess { module, name, .. } => (
                format!("`{name}` is private in module `{module}`"),
                Some("use `+` prefix to make it public (e.g., `+let`, `+fun`)".to_owned()),
            ),
            Self::UnknownClass(name, _) => (
                format!("unknown class `{name}`"),
                Some("valid classes: Numeric, Monoid, Ord, Fallible, Indexable, etc.".to_owned()),
            ),
            Self::UnknownMethodId { class, method, .. } => {
                let cn = p.strings.get(*class).unwrap_or("<unknown>");
                let mn = p.strings.get(*method).unwrap_or("<unknown>");
                (
                    format!("class `{cn}` has no method `{mn}`"),
                    None,
                )
            }
            Self::ConvertMethodNeedsType { class, method, .. } => (
                format!("convert method `{class}:{method}` requires type parameter"),
                Some(format!("use `{class}[TargetType]:{method}`")),
            ),
            Self::DuplicateInstance { class, type_id, .. } => (
                format!(
                    "duplicate `{}` instance for type `{}`",
                    p.class_name(*class),
                    p.type_name(*type_id)
                ),
                Some("a type can only have one instance of each class".to_owned()),
            ),
            Self::BuiltinInstanceForbidden { class, type_id, .. } => (
                format!(
                    "cannot implement `{}` for builtin type `{}`",
                    p.class_name(*class),
                    p.type_name(*type_id)
                ),
                Some("class instances can only be defined for user types (`type`, `newtype`, `union`)".to_owned()),
            ),
            Self::MissingInstanceMethod {
                class,
                method,
                required_hint,
                ..
            } => (
                format!(
                    "missing required method `{}` for class `{}`",
                    method,
                    p.class_name(*class)
                ),
                Some(format!("required methods: {}", required_hint)),
            ),
            Self::MethodSignatureMismatch {
                class,
                method,
                expected,
                got,
                ..
            } => (
                format!(
                    "method `{}` of class `{}` has wrong arity: expected {} parameter(s), got {}",
                    method,
                    p.class_name(*class),
                    expected,
                    got
                ),
                None,
            ),
            Self::MissingAssocType { class, assoc, .. } => {
                let name = p.strings.get(*assoc).unwrap_or("<unknown>");
                (
                    format!(
                        "missing required associated type `{name}` for class `{}`",
                        p.class_name(*class)
                    ),
                    Some(format!("add `newtype {name} = <type>` to the instance")),
                )
            }
            Self::UnknownAssocTypeForClass { class, assoc, .. } => (
                format!(
                    "class `{}` has no associated type `{assoc}`",
                    p.class_name(*class)
                ),
                None,
            ),
            Self::UnknownAssocType { ty, assoc, .. } => {
                let name = p.strings.get(*assoc).unwrap_or("<unknown>");
                (
                    format!(
                        "type `{}` has no associated type `{name}`",
                        p.format(*ty)
                    ),
                    None,
                )
            }
            Self::AssocTypeConstraint {
                assoc,
                constraint,
                actual,
                ..
            } => (
                format!(
                    "associated type `{assoc}` must satisfy `{constraint}`, got `{}`",
                    p.format(*actual)
                ),
                None,
            ),
            Self::AssocTypeOutsideClass { name, .. } => (
                format!(
                    "associated type `:{name}` can only be used inside a class instance"
                ),
                Some("use qualified form `ClassName:AssocType` outside class instances".to_owned()),
            ),
            Self::NoSuchAssocType { class, name, .. } => {
                let assoc = p.strings.get(*name).unwrap_or("<unknown>");
                (
                    format!(
                        "class `{}` has no associated type `{assoc}`",
                        p.class_name(*class)
                    ),
                    None,
                )
            }
            Self::InstanceNotImported {
                class,
                type_id,
                module,
                ..
            } => (
                format!(
                    "no `{}` instance for `{}` in scope",
                    p.class_name(*class),
                    p.type_name(*type_id)
                ),
                Some(format!(
                    "an instance is defined in module `{module}`; try adding `import {module}.{{ }}`"
                )),
            ),
            Self::MissingSuperclassInstance {
                class,
                superclass,
                type_id,
                ..
            } => (
                format!(
                    "cannot implement `{}` for `{}`; missing required superclass instance `{}`",
                    p.class_name(*class),
                    p.type_name(*type_id),
                    p.class_name(*superclass)
                ),
                None,
            ),
            Self::ClassRejectsArg { class, .. } => (
                format!("class `{}` does not accept type arguments", p.class_name(*class)),
                None,
            ),
            Self::ClassRequiresArg { class, .. } => (
                format!("class `{}` requires type arguments", p.class_name(*class)),
                None,
            ),
            Self::TopLevelExpr(_) => (
                "top-level expression statements are not allowed".to_owned(),
                Some("move code into a `fun main() { ... }` function".to_owned()),
            ),
            Self::MissingMain(_) => (
                "missing required `main` function".to_owned(),
                Some("add `fun main() { ... }` or use `--interactive` mode".to_owned()),
            ),
            Self::InvalidMainSignature { got, .. } => (
                format!(
                    "`main` must have signature `() -> Unit`; got `{}`",
                    p.format(*got)
                ),
                None,
            ),
            Self::FnTypeInPattern(_) => (
                "function types cannot appear in `is` patterns".to_owned(),
                Some(
                    "bind the value and call it, or use a typed `let` to check the signature"
                        .to_owned(),
                ),
            ),
            Self::MissingTypeParamConstraint { param, class, .. } => (
                format!(
                    "type parameter `{param}` requires `{}` constraint",
                    p.class_name(class.tag())
                ),
                Some(format!(
                    "add `{param}: {}` to the type parameter list",
                    p.class_name(class.tag())
                )),
            ),
        };

        FormattedTypeError {
            message: msg,
            help,
            span: self.span(),
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

/// Debug display for `Ty` (inner types shown as `TyId` handles).
///
/// For human-readable output, use `TyPrinter::format` which resolves
/// `TyId` handles through the arena.
impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Var(v) => write!(f, "?{}", v.idx()),
            Self::Bool => write!(f, "Bool"),
            Self::Int => write!(f, "Int"),
            Self::Word => write!(f, "Word"),
            Self::Float => write!(f, "Float"),
            Self::Char => write!(f, "Char"),
            Self::String => write!(f, "String"),
            Self::Unit => write!(f, "Unit"),
            Self::Time => write!(f, "Time"),
            Self::Range => write!(f, "Range"),
            Self::Json => write!(f, "Json"),
            Self::Ordering => write!(f, "Ordering"),
            Self::DataStatus => write!(f, "DataStatus"),
            Self::FilePath => write!(f, "FilePath"),
            Self::Path => write!(f, "Path"),
            Self::Regex => write!(f, "Regex"),
            Self::RuntimeError => write!(f, "Error"),
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
                if ts.len() == 1 {
                    write!(f, ",")?;
                }
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
                    write!(f, "#{}: {t}", k.idx())
                })?;
                write!(f, "}}")
            }
            Self::Union(prov, members) => {
                if let Some(id) = prov {
                    write!(f, "{id:?}")
                } else {
                    members.iter().enumerate().try_for_each(|(i, t)| {
                        if i > 0 {
                            write!(f, " | ")?;
                        }
                        write!(f, "{t}")
                    })
                }
            }
            Self::Named(id, args) => {
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
            Self::Local => write!(f, "Local"),
            Self::Global => write!(f, "Global"),
            Self::Apply(v, args) => {
                write!(f, "?{}[", v.idx())?;
                args.iter().enumerate().try_for_each(|(i, t)| {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")
                })?;
                write!(f, "]")
            }
            Self::AssocType(v, _class, name) => {
                write!(f, "?{}.#{}", v.idx(), name.idx())
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
        // Inner types are `TyId` now; Display shows `#N` handles
        assert_eq!(Ty::Array(TyId::from_raw(1)).to_string(), "Array[#1]");
        assert_eq!(Ty::Option(TyId::from_raw(5)).to_string(), "Option[#5]");
    }

    #[test]
    fn ty_display_var() {
        assert_eq!(Ty::Var(TyVar::new(42)).to_string(), "?42");
    }

    #[test]
    fn error_span() {
        let span = Span::new(10, 20);
        let err = TypeError::Mismatch {
            expected: TyArena::INT,
            got: TyArena::STRING,
            span,
        };
        assert_eq!(err.span(), span);
    }
}
