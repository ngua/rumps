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

use crate::ast::{BinOp, Literal, TypePattern, UnOp};
use crate::Span;

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

    /// An object literal.
    Object(Vec<(String, Expr)>),

    /// An array literal.
    Array(Vec<Expr>),

    /// A tuple literal.
    Tuple(Vec<Expr>),

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
        params: SmallVec<[(String, Option<TypeExpr>); 4]>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
    },

    /// Match expression: `MATCH expr { pattern => body, ... }`.
    Match(Box<Expr>, Vec<MatchArm>),
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

    /// Output a value.
    Output(Expr),

    /// An expression used as a statement.
    Expr(Expr),

    /// Named function definition.
    Fun {
        name: String,
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
}

/// A match arm (CST form).
#[derive(Clone, Debug)]
pub(crate) struct MatchArm {
    pub(crate) pattern: MatchPattern,
    pub(crate) guard: Option<Expr>,
    pub(crate) body: Expr,
}
