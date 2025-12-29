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

    /// A lexical variable reference.
    Var(String),

    /// A local B-tree variable with subscripts.
    Local(String, Vec<Expr>),

    /// A global B-tree variable with subscripts.
    Global(String, Vec<Expr>),

    /// `GET` primitive.
    Get(Box<Expr>),

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
        type_params: Vec<String>,
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

    /// Data query: `DATA var`.
    ///
    /// Queries the existence status of a node. The inner expression must be
    /// a `Local` or `Global`. Returns `DataStatus` enum.
    Data(Box<Expr>),
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
#[derive(Clone, Debug)]
pub(crate) enum StmtKind {
    /// Lexical binding with destructuring.
    Let(BindingPattern, Option<TypeExpr>, Expr),

    /// B-tree assignment.
    Set(Expr, Expr),

    /// Delete a variable or subtree.
    Kill(Expr),

    /// Output a value with optional format and target.
    Output(OutputStmt),

    /// An expression used as a statement.
    Expr(Expr),

    /// Named function definition.
    Fun {
        name: String,
        type_params: Vec<String>,
        params: SmallVec<[(String, Option<TypeExpr>); 4]>,
        ret: Option<TypeExpr>,
        body: Expr,
    },

    /// User-defined type declaration.
    Type {
        name: String,
        type_params: Vec<String>,
        def: TypeDefCst,
    },

    /// Union type declaration: `UNION Name = Type1 | Type2 | ...`.
    ///
    /// Anonymous unions for function parameters etc. use inline syntax
    /// (`x: Int | String`). Named unions are registered in the type registry.
    Union {
        name: String,
        type_params: Vec<String>,
        members: Vec<TypeExpr>,
    },

    /// User-defined module declaration: `MODULE Name { ... }`.
    ///
    /// Modules group related functions, constants, and nested modules.
    /// Contents can include:
    /// - `FUN` definitions (registered as module functions)
    /// - `LET` bindings (registered as module constants)
    /// - Nested `MODULE` definitions (registered as submodules)
    ///
    /// The body contains `Stmt`s; only `Fun`, `Let`, and `Module` are valid.
    /// This is enforced during parsing.
    Module { name: String, body: Vec<Stmt> },
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
#[derive(Clone, Debug)]
pub(crate) enum TypeDefCst {
    /// Sum type: `Variant1 | Variant2(T) | ...`
    Sum(Vec<VariantCst>),

    /// Structural object type alias: `{ field1: Type1, field2: Type2, ... }`
    Struct(Vec<(String, TypeExpr)>),
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

/// Extended output statement.
#[derive(Clone, Debug)]
pub(crate) struct OutputStmt {
    pub(crate) expr: Expr,
    pub(crate) format: OutputFormat,
    pub(crate) target: OutputTarget,
}
