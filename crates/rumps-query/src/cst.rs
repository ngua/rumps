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
//!    `Box<CstExpr>` for recursion, no shared state is needed.
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
//! | Aspect | CST | AST |
//! |--------|-----|-----|
//! | Recursion | `Box<CstExpr>` | `ExprId` (arena index) |
//! | Spans | Inline (`Span` field) | Parallel vectors |
//! | Ownership | Owned tree | Arena-backed IDs |
//! | Mutability | Immutable after parse | Built via `&mut Ast` |

use smallvec::SmallVec;

use crate::ast::{BinOp, Literal, TypePattern, UnOp};
use crate::Span;

/// A CST expression node with inline span.
#[derive(Clone, Debug)]
pub(crate) struct CstExpr {
    pub kind: CstExprKind,
    pub span: Span,
}

impl CstExpr {
    /// Create a new CST expression.
    pub(crate) fn new(kind: CstExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The kind of a CST expression.
#[derive(Clone, Debug)]
pub(crate) enum CstExprKind {
    /// A literal value.
    Literal(Literal),

    /// A lexical variable reference.
    Var(String),

    /// A local B-tree variable with subscripts.
    Local(String, Vec<CstExpr>),

    /// A global B-tree variable with subscripts.
    Global(String, Vec<CstExpr>),

    /// `GET` primitive.
    Get(Box<CstExpr>),

    /// A binary operation.
    Binary(Box<CstExpr>, BinOp, Box<CstExpr>),

    /// A unary operation.
    Unary(UnOp, Box<CstExpr>),

    /// A function call.
    Call(Box<CstExpr>, Vec<CstExpr>),

    /// An object literal.
    Object(Vec<(String, CstExpr)>),

    /// An array literal.
    Array(Vec<CstExpr>),

    /// Index access.
    Index(Box<CstExpr>, Box<CstExpr>),

    /// Field access.
    Field(Box<CstExpr>, String),

    /// Optional field access.
    OptionalField(Box<CstExpr>, String),

    /// Variant constructor.
    Variant(String, String, Vec<CstExpr>),

    /// Type check.
    Is(Box<CstExpr>, TypePattern),

    /// Type cast.
    As(Box<CstExpr>, CstTypeExpr),

    /// Fallible conversion.
    Read(Box<CstExpr>, CstTypeExpr),

    /// A block expression.
    Block(Vec<CstStmt>, Option<Box<CstExpr>>),

    /// Conditional expression.
    If(Box<CstExpr>, Box<CstExpr>, Option<Box<CstExpr>>),

    /// Closure.
    Closure {
        params: SmallVec<[(String, Option<CstTypeExpr>); 4]>,
        ret: Option<CstTypeExpr>,
        body: Box<CstExpr>,
    },
}

/// A CST statement node with inline span.
#[derive(Clone, Debug)]
pub(crate) struct CstStmt {
    pub kind: CstStmtKind,
    pub span: Span,
}

impl CstStmt {
    /// Create a new CST statement.
    pub(crate) fn new(kind: CstStmtKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The kind of a CST statement.
#[derive(Clone, Debug)]
pub(crate) enum CstStmtKind {
    /// Lexical binding.
    Let(String, Option<CstTypeExpr>, CstExpr),

    /// B-tree assignment.
    Set(CstExpr, CstExpr),

    /// Delete a variable or subtree.
    Kill(CstExpr),

    /// Output a value.
    Output(CstExpr),

    /// An expression used as a statement.
    Expr(CstExpr),

    /// Named function definition.
    Fun {
        name: String,
        params: SmallVec<[(String, Option<CstTypeExpr>); 4]>,
        ret: Option<CstTypeExpr>,
        body: CstExpr,
    },
}

/// A CST type expression with inline span.
#[derive(Clone, Debug)]
pub(crate) struct CstTypeExpr {
    pub kind: CstTypeExprKind,
    pub span: Span,
}

impl CstTypeExpr {
    /// Create a new CST type expression.
    pub(crate) fn new(kind: CstTypeExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The kind of a CST type expression.
#[derive(Clone, Debug)]
pub(crate) enum CstTypeExprKind {
    /// Simple named type.
    Named(String),

    /// Parameterized type.
    App(String, Vec<CstTypeExpr>),

    /// Function type.
    Fn(Vec<CstTypeExpr>, Box<CstTypeExpr>),
}
