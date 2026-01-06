//! Concrete Syntax Tree (CST) for the RUMPS query language.
//!
//! The CST is an intermediate representation between parsing and the
//! arena-allocated AST. It exists to eliminate the `Rc<RefCell<Ast>>` pattern
//! required by chumsky's `Clone` constraints on parsers.
//!
//! # Architecture
//!
//! ```text
//! Tokens  -->  CST (owned, boxed)  -->  AST (arena-allocated)
//!              ^^^^^^^^^^^^^^^^^        ^^^^^^^^^^^^^^^^^^^
//!              chumsky produces         lowering pass produces
//! ```
//!
//! # Why a CST?
//!
//! Chumsky parsers must be `Clone`, which forced the previous implementation
//! to use `Rc<RefCell<Ast>>` with pervasive `Rc::clone()` calls (62+ clones,
//! numbered variables like `ast2`, `ast3`, etc.). The CST decouples parsing
//! from arena allocation:
//!
//! 1. **Parsing**: Chumsky parsers return owned CST nodes. Since CST uses
//!    `Box<Expr>` for recursion, no shared state is needed.
//!
//! 2. **Lowering**: A single pass converts CST to AST with direct `&mut Ast`
//!    access. No `Rc` cloning, no numbered variables.
//!
//! # Trade-offs
//!
//! - **Extra allocation**: CST nodes are heap-allocated before being lowered
//!   to the arena. This is negligible for typical program sizes.
//! - **Two traversals**: Parsing builds the CST, then lowering rebuilds as AST.
//!   Again, negligible overhead for a query language where I/O dominates.
//! - **Duplicate definitions**: CST enums mirror AST enums. This is intentional
//!   duplication to maintain separation of concerns.
//!
//! # CST vs AST
//!
//! | **Aspect** | **CST**               | **AST**                      |
//! |------------|-----------------------|------------------------------|
//! | Recursion  | `Box<cst::Expr>`      | `ExprId` (arena index)       |
//! | Spans      | Inline (`Span` field) | Parallel vectors             |
//! | Ownership  | Owned tree            | Arena-backed IDs             |
//! | Mutability | Immutable after parse | Built via `&mut Ast`         |

use smallvec::SmallVec;

use crate::ast::{BinOp, JsonAccessKind, Literal, UnOp};
use crate::Span;

/// Constraint for type parameters.
///
/// This is a subset of the internal `Constraint` enum from the typechecker.
/// Not all internal constraints are exposed to users; see the design doc
/// at `TODOS/dsl/type-constraints.md` for rationale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParamConstraint {
    /// Type is `Int` or `Float`.
    Numeric,
    /// Type can be converted to string.
    Stringable,
    /// Type can be serialized to JSON.
    Jsonable,
    /// Type can be used as a DB subscript key.
    Subscriptable,
    /// Type can be stored in the database.
    Storable,
    /// Type is iterable (`Array[T]` or `Range`).
    ///
    /// The optional string is the name of another type parameter that
    /// represents the element type (e.g., `Iterable[T]` stores `Some("T")`).
    /// If `None`, element type is unconstrained (fresh variable).
    Iterable(Option<String>),
    /// Type supports monoidal concatenation (`++`).
    ///
    /// Satisfied by `String`, `Array[T]`, and `Map[K, V]`.
    Monoid,
    /// Type supports bitwise operations (`&`, `|`, `<<`, `>>`).
    ///
    /// Satisfied by `Bool`, `Int`, and `Word`.
    BitLike,
}

/// A type parameter with optional constraints.
///
/// Represents `T` or `T: Constraint1 + Constraint2` in type parameter lists.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TypeParam {
    pub name: String,
    pub constraints: SmallVec<[ParamConstraint; 2]>,
}

/// Type pattern for the `IS` operator (CST version).
///
/// This is the CST equivalent of `ast::TypePattern`. During lowering,
/// CST `TypeExpr` fields are converted to `AstTypeExprId`.
#[derive(Clone, Debug)]
pub(crate) enum TypePattern {
    /// Type check: `is Int`, `is Array[String]`, `is Map[Int, String]`.
    Type(TypeExpr),

    /// Variant check without payload: `is Option.None`.
    Variant(String, String),

    /// Variant check ignoring payload: `is Option.Some(_)`.
    VariantWildcard(String, String),

    /// Variant check with binding: `is Option.Some(val)`.
    VariantBind(String, String, SmallVec<[String; 2]>),

    /// Structural object check: `is { name: String, age: Int }`.
    Object(Vec<(String, TypeExpr)>),
}

/// An array element: either a single expression or a spread.
#[derive(Clone, Debug)]
pub(crate) enum ArrayElem {
    /// A single element: `expr`
    Elem(Expr),
    /// A spread: `...expr`
    Spread(Expr),
}

/// An object entry: either a field or a spread.
#[derive(Clone, Debug)]
pub(crate) enum ObjectEntry {
    /// A key-value field: `key: expr`
    Field(String, Expr),
    /// A spread: `...expr`
    Spread(Expr),
}

/// A subscript element: either a single expression or a spread.
///
/// Used in `DbRef` B-tree variable references:
/// - `d(1, "key")` uses `Elem` for each subscript
/// - `d(...keys)` uses `Spread` to expand an `Array[Subscript]`
#[derive(Clone, Debug)]
pub(crate) enum SubscriptElem {
    /// A single subscript: `expr`
    Elem(Expr),
    /// A spread: `...expr`
    Spread(Expr),
}

/// A reference to a B-tree variable (local or global) with subscripts.
///
/// This is NOT an expression; it can only appear in database operations like
/// `GET`, `SET`, `KILL`, `DATA`, and `ORDER`.
#[derive(Clone, Debug)]
pub(crate) enum DbRef {
    /// Local B-tree variable: `data`, `data(1)`, `data(...keys)`.
    Local(String, Vec<SubscriptElem>),
    /// Global B-tree variable: `^PATIENT`, `^DATA(1, ...rest)`.
    Global(String, Vec<SubscriptElem>),
}

/// A CST expression node with inline span.
#[derive(Clone, Debug)]
pub(crate) struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    /// Create a new CST expression.
    pub(crate) fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Update the span (used when wrapping in outer context).
    pub(crate) fn with_span(mut self, span: Span) -> Self {
        self.span = span;
        self
    }
}

/// The kind of a CST expression.
// NOTE: `Closure` is large due to `SmallVec` inline storage for params. This is
// intentional; closures with 1-4 params avoid allocation. CST is short-lived.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub(crate) enum ExprKind {
    /// A literal value.
    Literal(Literal),

    /// String interpolation: `"text {expr} more text"`.
    ///
    /// Contains alternating literal parts and expression source strings:
    /// - Even indices: literal text segments
    /// - Odd indices: expression source code (to be parsed during lowering)
    ///
    /// For example, `"Hello {name}!"` becomes `["Hello ", "name", "!"]`.
    Interpolation(Vec<String>),

    /// A lexical variable reference.
    Var(String),

    /// `GET` primitive.
    Get(DbRef),

    /// A binary operation.
    Binary(Box<Expr>, BinOp, Box<Expr>),

    /// A unary operation.
    Unary(UnOp, Box<Expr>),

    /// A function call.
    Call(Box<Expr>, Vec<Expr>),

    /// An object literal with potential spread entries.
    Object(Vec<ObjectEntry>),

    /// An array literal with potential spread elements.
    Array(Vec<ArrayElem>),

    /// A tuple literal.
    Tuple(Vec<Expr>),

    /// A map literal: `{ k => v, ... }`.
    MapLit(Vec<(Expr, Expr)>),

    /// Tuple index access: `tuple.0`, `tuple.1`.
    TupleIndex(Box<Expr>, u32),

    /// Index access.
    Index(Box<Expr>, Box<Expr>),

    /// Optional index access: `expr?[index]`.
    OptionalIndex(Box<Expr>, Box<Expr>),

    /// Field access.
    Field(Box<Expr>, String),

    /// Optional field access.
    OptionalField(Box<Expr>, String),

    /// Variant constructor.
    ///
    /// NOTE: `Expr::Path` exists in AST but not CST. `Path` is reserved for
    /// future module support. The name resolution pass converts field access
    /// on registered types (`Type.Variant`) to `Expr::Variant`; the parser
    /// emits generic `Field` and `Call` nodes.
    Variant(String, String, Vec<Expr>),

    /// Type check.
    Is(Box<Expr>, TypePattern),

    /// Type cast.
    As(Box<Expr>, TypeExpr),

    /// Fallible conversion.
    Read(Box<Expr>, TypeExpr),

    /// A block expression.
    Block(Vec<Stmt>, Option<Box<Expr>>),

    /// Conditional expression.
    If(Box<Expr>, Box<Expr>, Option<Box<Expr>>),

    /// Closure.
    Closure {
        type_params: Vec<TypeParam>,
        params: SmallVec<[(String, Option<TypeExpr>); 4]>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
    },

    /// Match expression: `MATCH expr { pattern => body, ... }`.
    Match(Box<Expr>, Vec<MatchArm>),

    /// Unwrap: `expr!`
    ///
    /// Extracts the payload from `Option.Some` or `Result.Ok`; produces a
    /// runtime error for `Option.None` or `Result.Err`.
    Unwrap(Box<Expr>),

    /// Range: `start..end` (exclusive) or `start..=end` (inclusive).
    ///
    /// - First `Box<Expr>`: start expression
    /// - Second `Box<Expr>`: end expression
    /// - `bool`: `true` for inclusive (`..=`), `false` for exclusive (`..`)
    Range(Box<Expr>, Box<Expr>, bool),

    /// Type annotation: `(expr) : Type`.
    ///
    /// Explicit type annotation on an expression. The interpreter validates
    /// that the value matches the annotated type at runtime; the type checker
    /// (once implemented) will use this as the expected type.
    Annotate(Box<Expr>, TypeExpr),

    /// A parse error detected during CST construction.
    ///
    /// This allows the parser to emit a structured CST while deferring error
    /// reporting to the lowering pass, where we have access to error handling.
    Error(String),

    /// A JSON object literal: `{ "key": value, ... }`.
    ///
    /// Distinguished from native `Object` by having quoted string keys.
    Json(Vec<(String, Expr)>),

    /// JSON field access operators.
    ///
    /// - `JsonAccessKind::Json`: `.` or `->` (returns Json)
    /// - `JsonAccessKind::Scalar`: `..` or `->>` (returns Option[scalar])
    JsonAccess(Box<Expr>, JsonAccessKind, JsonAccessKey),

    /// Regex literal: `/pattern/`.
    ///
    /// The pattern string is stored as-is; validation happens during
    /// type checking (invalid patterns produce type errors).
    Regex(String),

    /// Regex match: `expr MATCHES pattern`.
    ///
    /// Returns `Bool`. The left operand must be `Stringable`.
    Matches(Box<Expr>, Box<Expr>),

    /// Catch expression: `expr CATCH handler`.
    ///
    /// Evaluates `expr`; on runtime error, calls `handler` with `Error` value.
    /// Handler must be a closure `(Error) -> T` where `T` matches expr's type.
    Catch(Box<Expr>, Box<Expr>),

    /// Data query: `DATA var`.
    ///
    /// Queries the existence status of a node. Returns `DataStatus` enum.
    Data(DbRef),

    /// Order query: `ORDER var`.
    ///
    /// Returns the next subscript at a given level. Returns `Option[Subscript]`.
    Order(DbRef),

    /// Query: `@QUERY var`.
    ///
    /// Returns the full key path to the next node with a value.
    /// Returns `Option[Array[Subscript]]`.
    Query(DbRef),

    /// Write expression: `WRITE expr [JSON] [TO target]`.
    ///
    /// Executes the write side effect and evaluates to `Unit`.
    /// This allows `WRITE` in expression contexts.
    Write(Box<WriteStmt>),

    /// Set expression: `@SET target = value`.
    ///
    /// Executes the B-tree assignment and evaluates to `Unit`.
    Set(DbRef, Box<Expr>),

    /// Kill expression: `@KILL target`.
    ///
    /// Deletes a variable or subtree and evaluates to `Unit`.
    Kill(DbRef),

    /// Raise a runtime error: `RAISE expr`.
    ///
    /// Evaluates `expr` (must be `Stringable`) and raises a runtime error.
    /// Never returns; can unify with any expected type.
    Raise(Box<Expr>),

    /// Forever loop: `FOREVER seed (state, cont) => body`.
    Forever {
        seed: Box<Expr>,
        state_param: (String, Option<TypeExpr>),
        cont_param: (String, Option<TypeExpr>),
        body: Box<Expr>,
    },

    /// Transaction block expression: `TRANSACTION { ... }`.
    Transaction(Box<TransactionExpr>),

    /// Monoid identity (`mempty`): `_` in expression context.
    ///
    /// Produces the empty/identity value for the inferred `Monoid` type.
    Mempty,
}

/// The key specification for JSON access (CST form).
#[derive(Clone, Debug)]
pub(crate) enum JsonAccessKey {
    /// Static field name: `data.field` or `data..field`
    Field(String),
    /// Dynamic key expression: `data->"key"` or `data->>"key"`
    Expr(Box<Expr>),
}

/// A CST statement node with inline span.
#[derive(Clone, Debug)]
pub(crate) struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

impl Stmt {
    /// Create a new CST statement.
    pub(crate) fn new(kind: StmtKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// Rest pattern for array destructuring (CST form).
#[derive(Clone, Debug)]
pub(crate) enum RestPattern {
    /// `..` ; ignore remaining elements
    Ignore,
    /// `...name` ; bind remaining elements to `name`
    Bind(String),
}

/// Visibility modifier for module members.
///
/// Inside a module, items are private by default. Use `+` prefix to make
/// them public (e.g., `+LET`, `+FUN`, `+TYPE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Visibility {
    /// Private; only accessible within the module (default).
    #[default]
    Private,
    /// Public; accessible from outside the module (`+` prefix).
    Public,
}

/// A binding pattern for destructuring in `LET` statements (CST form).
#[derive(Clone, Debug)]
pub(crate) enum BindingPattern {
    /// Simple variable binding: `x`
    Var(String),

    /// Tuple destructuring: `(a, b, c)`
    Tuple(Vec<Self>),

    /// Object destructuring: `{ name, age }` or `{ name: n, age: a }`
    Object(Vec<(String, Self)>),

    /// Array destructuring: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
    Array(Vec<Self>, Option<RestPattern>),

    /// Wildcard: `_`
    Wildcard,
}

/// The kind of a CST statement.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub(crate) enum StmtKind {
    /// Lexical binding with destructuring.
    ///
    /// The visibility is only meaningful inside modules (`+LET` for public).
    Let(BindingPattern, Option<TypeExpr>, Expr, Visibility),

    /// B-tree assignment.
    Set(DbRef, Expr),

    /// Delete a variable or subtree.
    Kill(DbRef),

    /// Write a value with optional format and target.
    Write(WriteStmt),

    /// An expression used as a statement.
    Expr(Expr),

    /// Named function definition.
    ///
    /// The visibility is only meaningful inside modules (`+FUN` for public).
    Fun {
        name: String,
        type_params: Vec<TypeParam>,
        params: SmallVec<[(String, Option<TypeExpr>); 4]>,
        ret: Option<TypeExpr>,
        body: Expr,
        vis: Visibility,
    },

    /// User-defined sum type declaration: `TYPE Name = Variant1 | Variant2(T)`.
    ///
    /// The visibility is only meaningful inside modules (`+TYPE` for public).
    Type {
        name: String,
        type_params: Vec<TypeParam>,
        def: TypeDefCst,
        vis: Visibility,
    },

    /// Transparent type alias: `NEWTYPE Name = Type` or `NEWTYPE Name[T] = Type`.
    ///
    /// The visibility is only meaningful inside modules (`+NEWTYPE` for public).
    NewType {
        name: String,
        type_params: Vec<TypeParam>,
        target: TypeExpr,
        vis: Visibility,
    },

    /// Union type declaration: `UNION Name = Type1 | Type2 | ...`.
    ///
    /// Anonymous unions for function parameters etc. use inline syntax
    /// (`x: Int | String`). Named unions are registered in the type registry.
    ///
    /// The visibility is only meaningful inside modules (`+UNION` for public).
    Union {
        name: String,
        type_params: Vec<TypeParam>,
        members: Vec<TypeExpr>,
        vis: Visibility,
    },

    /// User-defined module declaration.
    ///
    /// Modules group related functions, constants, and nested modules.
    /// Contents can include:
    /// - `FUN` definitions (registered as module functions)
    /// - `LET` bindings (registered as module constants)
    /// - Nested `MODULE` definitions (registered as submodules)
    ///
    /// The body contains `Stmt`s; only `Fun`, `Let`, and `Module` are valid.
    /// This is enforced during typechecking.
    ///
    /// Two forms are supported:
    /// - Inline: `MODULE Name { ... }`
    /// - File import: `MODULE Name FROM "path/to/module.rumps"`
    Module { name: String, source: ModuleSource },

    /// Import members from a module.
    ///
    /// Syntax: `IMPORT Module.{ member, ... }` or `IMPORT Module.{ ... }`.
    Import(ImportStmt),
}

/// Source of a module's contents.
#[derive(Clone, Debug)]
pub(crate) enum ModuleSource {
    /// Inline module body: `MODULE Name { ... }`.
    Inline(Vec<Stmt>),
    /// File import: `MODULE Name FROM "path/to/module.rumps"`.
    ///
    /// The path is relative to the importing script's directory, or absolute.
    File(String),
}

/// A single import item.
#[derive(Clone, Debug)]
pub(crate) enum ImportItem {
    /// Named import: `member` or `member AS alias`.
    Named { name: String, alias: Option<String> },
    /// Wildcard import: `...`.
    Wildcard,
    /// Exclusion (only valid after wildcard): `-member`.
    Exclude(String),
}

/// Import statement.
#[derive(Clone, Debug)]
pub(crate) struct ImportStmt {
    /// Module path segments (e.g., `["Module", "Nested"]`).
    pub(crate) path: Vec<String>,
    /// Import items.
    pub(crate) items: Vec<ImportItem>,
}

/// A CST type expression with inline span.
#[derive(Clone, Debug)]
pub(crate) struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

impl TypeExpr {
    /// Create a new CST type expression.
    pub(crate) fn new(kind: TypeExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The kind of a CST type expression.
#[derive(Clone, Debug)]
pub(crate) enum TypeExprKind {
    /// Simple named type.
    Named(String),

    /// Parameterized type.
    App(String, Vec<TypeExpr>),

    /// Function type.
    Fn(Vec<TypeExpr>, Box<TypeExpr>),

    /// Tuple type: `(Int, String)`, `(Bool, Int, Float)`.
    Tuple(Vec<TypeExpr>),

    /// Union type: `Int | String | Bool`.
    ///
    /// Anonymous unions for type annotations. For named union declarations,
    /// see `StmtKind::Union`.
    Union(Vec<TypeExpr>),

    /// Structural object type: `{ field: Type, ... }`.
    ///
    /// Anonymous structural object types in type position. Uses extensible
    /// record semantics: an object matches if it has at least these fields.
    Object(Vec<(String, TypeExpr)>),
}

/// A variant definition in a user-defined sum type (CST form).
#[derive(Clone, Debug)]
pub(crate) struct VariantCst {
    pub name: String,
    pub payloads: Vec<TypeExpr>,
}

/// A type definition body (CST form).
///
/// Note: Only sum types remain; struct aliases now use `NEWTYPE`.
#[derive(Clone, Debug)]
pub(crate) enum TypeDefCst {
    /// Sum type: `Variant1 | Variant2(T) | ...`
    Sum(Vec<VariantCst>),
}

/// A match pattern (CST form).
#[derive(Clone, Debug)]
pub(crate) enum MatchPattern {
    /// Wildcard: `_`
    Wildcard,

    /// Variable binding: `x`, `name`
    Var(String),

    /// Literal: `0`, `"hello"`, `true`
    Literal(crate::ast::Literal),

    /// Variant with sub-patterns: `Option.Some(x)`, `Result.Err(e)`
    Variant(String, String, Vec<Self>),

    /// Object destructuring: `{ name, age }`
    Object(Vec<(String, Self)>),

    /// Tuple pattern: `(a, b, c)`
    Tuple(Vec<Self>),

    /// Array pattern: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
    ///
    /// - First vec: patterns for fixed-position elements
    /// - `Option<RestPattern>`: optional rest handling
    Array(Vec<Self>, Option<RestPattern>),

    /// Type-narrowing pattern: `x IS Int`, `val IS String`
    ///
    /// Matches if the value is of the specified type and binds it to the name.
    Is(String, TypeExpr),
}

/// A match arm (CST form).
#[derive(Clone, Debug)]
pub(crate) struct MatchArm {
    pub(crate) pattern: MatchPattern,
    pub(crate) guard: Option<Expr>,
    pub(crate) body: Expr,
}

/// Output format modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    /// Default: stringify the value.
    #[default]
    Default,
    /// Convert to JSON before output.
    Json,
}

/// Output target.
#[derive(Clone, Debug, Default)]
pub(crate) enum OutputTarget {
    /// Default: stdout.
    #[default]
    Stdout,
    /// Write to stderr.
    Stderr,
    /// Write to a file (path expression).
    File(Box<Expr>),
}

/// Extended write statement.
#[derive(Clone, Debug)]
pub(crate) struct WriteStmt {
    pub(crate) expr: Expr,
    pub(crate) format: OutputFormat,
    pub(crate) target: OutputTarget,
}

/// Transaction block expression.
#[derive(Clone, Debug)]
pub(crate) struct TransactionExpr {
    /// Statements in the transaction body.
    pub(crate) stmts: Vec<Stmt>,
    /// Optional trailing expression (return value).
    pub(crate) expr: Option<Box<Expr>>,
    /// Transaction modifiers (added in phase 6.2).
    pub(crate) modifiers: TransactionModifiers,
}

/// Transaction configuration modifiers.
#[derive(Clone, Debug, Default)]
pub(crate) struct TransactionModifiers {
    pub(crate) conflict: Option<ConflictModifier>,
    pub(crate) timeout: Option<Box<Expr>>,
    pub(crate) retries: Option<u32>,
    pub(crate) isolation: Option<IsolationModifier>,
}

/// Conflict resolution strategy.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ConflictModifier {
    Abort,
    Overwrite,
}

/// Isolation level.
#[derive(Clone, Copy, Debug)]
pub(crate) enum IsolationModifier {
    Snapshot,
}
