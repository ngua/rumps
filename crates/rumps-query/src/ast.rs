//! AST definitions for the RUMPS query language.
//!
//! Uses arena allocation with indices instead of `Box` for cache-friendliness
//! and to avoid deep pointer chains. Spans are stored in parallel vectors
//! for cache efficiency; the interpreter rarely needs spans during execution.

#![allow(dead_code)]

use rumps_types::Name;
use smallvec::{smallvec, SmallVec};

use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::typecheck::TypeClass;
use crate::{ClassId, Error, Result, Span};

/// Unique identifier for a `transaction` block.
///
/// Assigned during typechecking; used at runtime to look up the active
/// transaction in a `HashMap<TxnId, Transaction>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TxnId(u32);

impl TxnId {
    pub(crate) const fn new(id: u32) -> Self {
        Self(id)
    }
}

/// A collection of items with parallel span storage.
///
/// Stores items and their spans in separate vectors for cache efficiency;
/// spans are only accessed for error reporting.
#[derive(Clone, Debug)]
pub(crate) struct WithSpans<T> {
    items: Vec<T>,
    spans: Vec<Span>,
}

impl<T> Default for WithSpans<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            spans: Vec::new(),
        }
    }
}

impl<T> WithSpans<T> {
    /// Add an item with its span, returning the index as `u32`.
    ///
    /// Returns an error if the arena exceeds `u32::MAX` items.
    fn add(&mut self, item: T, span: Span) -> Result<u32> {
        let idx = u32::try_from(self.items.len()).map_err(|_| {
            Error::parse(
                span,
                "AST arena overflow: exceeded u32::MAX items",
                vec![],
            )
        })?;
        self.items.push(item);
        self.spans.push(span);
        Ok(idx)
    }

    /// Get an item by index.
    fn get(&self, idx: u32) -> Option<&T> {
        self.items.get(idx as usize)
    }

    /// Get a mutable reference to an item by index.
    fn get_mut(&mut self, idx: u32) -> Option<&mut T> {
        self.items.get_mut(idx as usize)
    }

    /// Get the span of an item by index.
    fn span(&self, idx: u32) -> Option<Span> {
        self.spans.get(idx as usize).copied()
    }

    /// Number of items.
    fn len(&self) -> u32 {
        self.items.len() as u32
    }
}

/// Index into the expression arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct ExprId(u32);

/// Index into the statement arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct StmtId(u32);

/// Index into the type expression arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct AstTypeExprId(u32);

/// Index into the match pattern arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct MatchPatternId(u32);

impl ExprId {
    /// The raw index value.
    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }
}

impl StmtId {
    /// The raw index value.
    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }
}

impl AstTypeExprId {
    /// The raw index value.
    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }
}

impl MatchPatternId {
    /// The raw index value.
    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Class constraints on AST type parameters (e.g., `T: Numeric + Ord`).
pub(crate) type AstClassConstraints = SmallVec<[TypeClass<AstTypeExprId>; 2]>;

/// A type parameter with optional class constraints.
///
/// Represents `T` or `T: Class1 + Class2` in type parameter lists.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TypeParam {
    pub name: StringId,
    pub constraints: AstClassConstraints,
}

/// The AST arena; owns all expressions and statements.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ast {
    exprs: WithSpans<Expr>,
    stmts: WithSpans<Stmt>,
    type_exprs: WithSpans<AstTypeExpr>,
    patterns: Vec<MatchPattern>,
}

impl Ast {
    /// Create an empty AST.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add an expression to the arena.
    pub(crate) fn add_expr(&mut self, e: Expr, span: Span) -> Result<ExprId> {
        self.exprs.add(e, span).map(ExprId)
    }

    /// Add a statement to the arena.
    pub(crate) fn add_stmt(&mut self, s: Stmt, span: Span) -> Result<StmtId> {
        self.stmts.add(s, span).map(StmtId)
    }

    /// Get an expression by ID.
    pub(crate) fn get_expr(&self, id: ExprId) -> Option<&Expr> {
        self.exprs.get(id.0)
    }

    /// Get a statement by ID.
    pub(crate) fn get_stmt(&self, id: StmtId) -> Option<&Stmt> {
        self.stmts.get(id.0)
    }

    /// Get the span of an expression.
    pub(crate) fn expr_span(&self, id: ExprId) -> Option<Span> {
        self.exprs.span(id.0)
    }

    /// Get the span of a statement.
    pub(crate) fn stmt_span(&self, id: StmtId) -> Option<Span> {
        self.stmts.span(id.0)
    }

    /// Number of expressions in the arena.
    pub(crate) fn expr_count(&self) -> usize {
        self.exprs.len() as usize
    }

    /// Number of statements in the arena.
    pub(crate) fn stmt_count(&self) -> usize {
        self.stmts.len() as usize
    }

    /// Add a type expression to the arena.
    pub(crate) fn add_type_expr(
        &mut self,
        te: AstTypeExpr,
        span: Span,
    ) -> Result<AstTypeExprId> {
        self.type_exprs.add(te, span).map(AstTypeExprId)
    }

    /// Get a type expression by ID.
    pub(crate) fn get_type_expr(
        &self,
        id: AstTypeExprId,
    ) -> Option<&AstTypeExpr> {
        self.type_exprs.get(id.0)
    }

    /// Replace an expression in place (for name resolution).
    pub(crate) fn set_expr(&mut self, id: ExprId, e: Expr) {
        if let Some(slot) = self.exprs.get_mut(id.0) {
            *slot = e
        }
    }

    /// Replace a statement in place (for typecheck TxnId assignment).
    pub(crate) fn set_stmt(&mut self, id: StmtId, s: Stmt) {
        if let Some(slot) = self.stmts.get_mut(id.0) {
            *slot = s
        }
    }

    /// Replace a type expression in place (for name resolution).
    pub(crate) fn set_type_expr(&mut self, id: AstTypeExprId, te: AstTypeExpr) {
        if let Some(slot) = self.type_exprs.get_mut(id.0) {
            *slot = te
        }
    }

    /// Iterate over all expression IDs.
    pub(crate) fn expr_ids(&self) -> impl Iterator<Item = ExprId> {
        (0..self.exprs.len()).map(ExprId)
    }

    /// Iterate over all statement IDs.
    pub(crate) fn stmt_ids(&self) -> impl Iterator<Item = StmtId> {
        (0..self.stmts.len()).map(StmtId)
    }

    /// Get the span of a type expression.
    pub(crate) fn type_expr_span(&self, id: AstTypeExprId) -> Option<Span> {
        self.type_exprs.span(id.0)
    }

    /// Add a match pattern to the arena.
    pub(crate) fn add_pattern(
        &mut self,
        pat: MatchPattern,
    ) -> Result<MatchPatternId> {
        let idx = u32::try_from(self.patterns.len()).map_err(|_| {
            Error::parse(
                Span::new(0, 0),
                "pattern arena overflow: exceeded u32::MAX items",
                vec![],
            )
        })?;
        self.patterns.push(pat);
        Ok(MatchPatternId(idx))
    }

    /// Get a match pattern by ID.
    pub(crate) fn get_pattern(
        &self,
        id: MatchPatternId,
    ) -> Option<&MatchPattern> {
        self.patterns.get(id.idx())
    }

    /// Replace a match pattern in place (for name resolution).
    pub(crate) fn set_pattern(&mut self, id: MatchPatternId, p: MatchPattern) {
        if let Some(slot) = self.patterns.get_mut(id.idx()) {
            *slot = p
        }
    }
}

/// A type expression in the AST (for annotations).
///
/// Represents syntactic type expressions before resolution to runtime types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AstTypeExpr {
    /// Wildcard type: `_`; represents "any type" in type argument position.
    ///
    /// Used to avoid specifying concrete type arguments when only the base type
    /// matters, e.g., `x IS Option[_]` matches both `Option.Some(1)` and
    /// `Option.Some("hello")`.
    Wildcard,

    /// Simple named type: `Int`, `String`, `Option`, `Math.Vector`, etc.
    Named(QualifiedName),

    /// Parameterized type: `Array[Int]`, `Option[String]`, `Result[Int, String]`.
    App(QualifiedName, SmallVec<[AstTypeExprId; 2]>),

    /// Type variable application: `F[T]` where `F` is a type parameter.
    ///
    /// Distinguished from `App` during lowering when the name is a known type
    /// parameter. Converted to `Ty::Apply` during typechecking.
    VarApp(QualifiedName, SmallVec<[AstTypeExprId; 2]>),

    /// Function type: `(Int, Int) -> Int`, `Int -> Int`, `() -> String`.
    ///
    /// - First element: parameter types (may be empty for nullary)
    /// - Second element: return type
    Fn(SmallVec<[AstTypeExprId; 4]>, AstTypeExprId),

    /// Tuple type: `(Int, String)`, `(Bool, Int, Float)`.
    ///
    /// Tuple types have two or more element types (single-element tuples require
    /// a trailing comma: `(Int,)`).
    Tuple(SmallVec<[AstTypeExprId; 4]>),

    /// Union type: `Int | String | Bool`.
    ///
    /// Anonymous unions for type annotations. Named union declarations use
    /// `Stmt::Union`.
    Union(SmallVec<[AstTypeExprId; 4]>),

    /// Structural object type: `{ field: Type, ... }`.
    ///
    /// Anonymous structural object type in type position. Uses extensible
    /// record semantics: a value matches if it has at least the specified fields.
    Object(SmallVec<[(StringId, AstTypeExprId); 4]>),

    /// Associated type reference: `:Index` (unqualified) or `Indexable:Index` (qualified).
    ///
    /// - `class: None`: unqualified `:Index`, resolved from class context
    /// - `class: Some("Indexable")`: qualified, names the class explicitly
    AssocType {
        class: Option<StringId>,
        name: StringId,
    },

    /// Tuple constructor: `(,)`, `(T,)`, `(T,,)`, etc.
    ///
    /// Only valid in the `for` clause of a class instance; all other match
    /// sites emit a type error.
    TupleConstructor {
        arity: u8,
        fixed: SmallVec<[(u8, AstTypeExprId); 2]>,
    },
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    // Arithmetic
    Add,      // `+`
    Sub,      // `-`
    Mul,      // `*`
    Div,      // `/`
    FloorDiv, // `//`
    Mod,      // `%`
    Pow,      // `**`

    // Comparison
    Eq, // `==`
    Ne, // `!=`
    Lt, // `<`
    Gt, // `>`
    Le, // `<=`
    Ge, // `>=`

    // Logical
    And, // `AND` or `&&`
    Or,  // `OR` or `||`

    // Bitwise
    BitAnd, // `&`
    BitOr,  // `|`
    Shl,    // `<<`
    Shr,    // `>>`

    // String
    Concat, // `++`

    // Coalesce
    Coalesce, // `??`

    // Pipeline
    Pipe, // `|>`
}

impl BinOp {
    /// Maps a binary operator to its dispatching class and method name.
    ///
    /// Returns `None` for operators that don't dispatch through a class
    /// (e.g. `And`, `Or`, `Coalesce`, `Pipe`, `Div`).
    ///
    /// For derived operators (`!=`, `<`, `>`, `<=`, `>=`), returns the
    /// *base* class method (`"eq"` or `"compare"`); the caller is
    /// responsible for post-processing the result.
    pub(crate) fn class_dispatch(self) -> Option<(ClassId, &'static str)> {
        match self {
            Self::Add => Some((ClassId::NUMERIC, "add")),
            Self::Sub => Some((ClassId::NUMERIC, "sub")),
            Self::Mul => Some((ClassId::NUMERIC, "mul")),
            Self::FloorDiv => Some((ClassId::NUMERIC, "floor-div")),
            Self::Mod => Some((ClassId::NUMERIC, "mod")),
            Self::Pow => Some((ClassId::NUMERIC, "pow")),
            Self::Eq | Self::Ne => Some((ClassId::EQ, "eq")),
            Self::Lt | Self::Gt | Self::Le | Self::Ge => {
                Some((ClassId::ORD, "compare"))
            }
            Self::Concat => Some((ClassId::MONOID, "concat")),
            Self::BitAnd => Some((ClassId::BIT_LIKE, "bit-and")),
            Self::BitOr => Some((ClassId::BIT_LIKE, "bit-or")),
            Self::Shl => Some((ClassId::BIT_LIKE, "shl")),
            Self::Shr => Some((ClassId::BIT_LIKE, "shr")),
            _ => None,
        }
    }
}

/// Unary (prefix) operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Neg, // `-`
    Not, // `NOT` or `!`
    /// Prefix `?` wraps a value in a `Wrappable` type (`Option` or `Result`).
    ///
    /// By default, `?x` produces `Option.Some(x)`. When context expects
    /// `Result[T, E]`, it produces `Result.Ok(x)` instead. Equivalent to
    /// calling `Wrappable:wrap(x)`.
    ///
    /// For nested wrapping, use parens: `?(?x)` produces nested `Some`.
    /// Note that `??x` is parsed as the coalesce operator, not nested wrap.
    Wrap,
}

/// Postfix operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostfixOp {
    /// `!` ; unwrap `Option`/`Result`, producing a runtime error on `None`/`Err`
    Unwrap,
}

/// Rest pattern for array destructuring.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RestPattern {
    /// `..` ; ignore remaining elements
    Ignore,
    /// `...name` ; bind remaining elements to `name`
    Bind(StringId),
}

/// A binding pattern for destructuring in `let` statements.
///
/// Patterns allow extracting values from composite structures (tuples, objects,
/// arrays) and binding them to multiple variables in a single statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BindingPattern {
    /// Simple variable binding: `x`
    Var(StringId),

    /// Tuple destructuring: `(a, b, c)`
    Tuple(Vec<Self>),

    /// Object destructuring: `{ name, age }` or `{ name: n, age: a }`
    ///
    /// Each entry is `(field_name, pattern)`. Shorthand `{ name }` is lowered to
    /// `{ name: name }` (i.e., `("name", Var("name"))`).
    Object(Vec<(StringId, Self)>),

    /// Array destructuring: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
    ///
    /// - First vec: patterns for fixed-position elements
    /// - `Option<RestPattern>`: optional rest handling
    Array(Vec<Self>, Option<RestPattern>),

    /// Wildcard: `_` (ignore this position)
    Wildcard,
}

impl From<StringId> for BindingPattern {
    fn from(s: StringId) -> Self {
        Self::Var(s)
    }
}

/// A type pattern for the `is` operator.
///
/// Used for runtime type checking and variant matching with optional binding.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TypePattern {
    /// Type check: `is Int`, `is Array[String]`, `is Map[Int, String]`.
    Type(AstTypeExprId),

    /// Variant check without payload: `is Option.None`.
    Variant(QualifiedName, StringId),

    /// Variant check ignoring payload: `is Option.Some(_)`.
    VariantWildcard(QualifiedName, StringId),

    /// Variant check with binding: `is Option.Some(val)`.
    ///
    /// Bindings are only visible in the `then` branch of an `if`.
    VariantBind(QualifiedName, StringId, SmallVec<[StringId; 2]>),

    /// Structural object check: `is { name: String, age: Int }`.
    ///
    /// Extensible record semantics: value matches if it has at least these fields.
    Object(SmallVec<[(StringId, AstTypeExprId); 4]>),
}

/// A match pattern for the `match` expression.
///
/// Patterns destructure values and bind variables. Unlike `BindingPattern` (for
/// `let`), match patterns can include literals and variant constructors for
/// exhaustive matching against sum types.
///
/// Uses `MatchPatternId` for recursive references (arena allocation).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MatchPattern {
    /// Wildcard: `_`
    Wildcard,

    /// Variable binding: `x`, `name`
    ///
    /// Matches any value and binds it to the given name.
    Var(StringId),

    /// Literal: `0`, `"hello"`, `true`
    ///
    /// Matches only if the value equals the literal.
    Literal(Literal),

    /// Variant with sub-patterns: `Option.Some(x)`, `Result.Err(e)`
    ///
    /// Matches a tagged value if the type and variant match, then recursively
    /// matches the payloads against the sub-patterns.
    Variant(QualifiedName, StringId, SmallVec<[MatchPatternId; 2]>),

    /// Object destructuring: `{ name, age }`, `{ name, role: "admin" }`
    ///
    /// Each entry is `(field_name, pattern)`. Shorthand `{ name }` desugars to
    /// `{ name: name }` (i.e., match field and bind to same-named variable).
    /// Additional fields in the value are allowed (partial matching).
    Object(SmallVec<[(StringId, MatchPatternId); 4]>),

    /// Tuple pattern: `(a, b, c)`
    ///
    /// Matches a tuple of the same arity and recursively matches elements.
    Tuple(SmallVec<[MatchPatternId; 4]>),

    /// Array pattern: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
    ///
    /// Matches an array by position with optional rest handling.
    /// - First vec: patterns for fixed-position elements
    /// - `Option<RestPattern>`: optional rest handling (`..` or `...name`)
    ///
    /// Without rest: matches arrays of exactly the specified length.
    /// With rest: matches arrays of at least the specified prefix length.
    Array(SmallVec<[MatchPatternId; 4]>, Option<RestPattern>),

    /// Type-narrowing pattern: `x IS Int`, `val IS String`
    ///
    /// Matches if the value is of the specified type and binds it to the name.
    Is(StringId, AstTypeExprId),
}

/// A match arm in a `match` expression.
///
/// Each arm consists of a pattern, an optional guard condition, and a body
/// expression. Arms are tried in order; the first matching arm is evaluated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MatchArm {
    /// The pattern to match against the scrutinee.
    pub(crate) pattern: MatchPatternId,

    /// Optional guard condition: `if cond`.
    ///
    /// If present, the arm only matches if the pattern matches AND the guard
    /// evaluates to `true`. Variables bound by the pattern are visible in the
    /// guard.
    pub(crate) guard: Option<ExprId>,

    /// The body expression to evaluate if this arm matches.
    ///
    /// Variables bound by the pattern are visible in the body.
    pub(crate) body: ExprId,
}

/// A variant definition in a user-defined sum type.
///
/// Each variant has a name and zero or more payload types.
/// Examples: `None` (arity 0), `Some(Int)` (arity 1), `Pair(Int, String)` (arity 2).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VariantAst {
    pub name: StringId,
    pub payloads: SmallVec<[AstTypeExprId; 2]>,
}

/// A type definition body for user-defined sum types.
///
/// Note: Struct aliases now use `Stmt::NewType` instead.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TypeDefAst {
    /// Sum type: `Variant1 | Variant2(T) | ...`
    ///
    /// Each variant is a named constructor with optional payload types.
    Sum(SmallVec<[VariantAst; 4]>),
}

/// An array element: either a single expression or a spread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArrayElem {
    /// A single element: `expr`
    Elem(ExprId),
    /// A spread: `...expr`
    Spread(ExprId),
}

/// An object entry: either a field or a spread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObjectEntry {
    /// A key-value field: `key: expr`
    Field(StringId, ExprId),
    /// A spread: `...expr`
    Spread(ExprId),
}

/// A subscript element: either a single expression or a spread.
///
/// Used in `DbRef` B-tree variable references:
/// - `d(1, "key")` uses `Elem` for each subscript
/// - `d(...keys)` uses `Spread` to expand an `Array[Subscript]`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubscriptElem {
    /// A single subscript: `expr`
    Elem(ExprId),
    /// A spread: `...expr`
    Spread(ExprId),
}

/// A reference to a B-tree variable (local or global) with subscripts.
///
/// This is NOT an expression; it can only appear in database operations like
/// `@get`, `@set`, `@kill`, `@data`, and `@order`. This ensures at the AST level
/// that B-tree references cannot be used as values directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DbRef {
    /// Local B-tree variable: `data`, `data(1)`, `data(...keys)`.
    Local(StringId, SmallVec<[SubscriptElem; 4]>),
    /// Global B-tree variable: `^PATIENT`, `^DATA(1, ...rest)`.
    Global(StringId, SmallVec<[SubscriptElem; 4]>),
}

impl DbRef {
    /// Splits into the storage `Name` and subscript elements.
    ///
    /// Requires the interner to resolve `StringId` to `&str` for `Name`.
    pub(crate) fn split(
        &self,
        interner: &StringInterner,
    ) -> (Name, &SmallVec<[SubscriptElem; 4]>) {
        match self {
            Self::Local(n, s) => {
                (Name::local(interner.get(*n).unwrap_or_default()), s)
            }
            Self::Global(n, s) => {
                (Name::global(interner.get(*n).unwrap_or_default()), s)
            }
        }
    }
}

/// Target of a database intrinsic: either inline `DbRef` syntax or an
/// expression evaluating to `Ref`.
///
/// Intrinsics like `@get`, `@set`, `@kill`, etc., can accept:
/// - Inline syntax: `@get data{1, 2}` or `@get ^global{key}`
/// - Expression: `@get r` where `r: Ref`
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RefTarget {
    /// Inline `DbRef` syntax (subscripts evaluated at intrinsic call).
    Inline(DbRef),
    /// Expression evaluating to `Ref` (subscripts pre-evaluated).
    Expr(ExprId),
}

/// Database intrinsic operation.
///
/// These are special operations with `@` prefix syntax that operate on
/// B-tree references (`RefTarget`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Intrinsic {
    /// `@get`: Read a value from a B-tree variable.
    Get,
    /// `@set`: Write a value to a B-tree variable.
    Set,
    /// `@kill`: Delete a variable and its descendants.
    Kill,
    /// `@data`: Query existence status of a node.
    Data,
    /// `@order`: Return the next subscript at a given level.
    Order,
    /// `@query`: Return the full key path to the next node.
    Query,
}

/// A literal value in the AST.
///
/// This is the compile-time representation; runtime values (with arena
/// allocation and string interning) are defined separately in the `value`
/// module.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Literal {
    Bool(bool),
    /// Polymorphic numeric literal; concrete type determined by inference.
    ///
    /// During type checking, numeric literals get a fresh type variable with
    /// a `Numeric` constraint. The resolved type (`Int`, `Word`, or `Float`)
    /// determines how the interpreter converts this value.
    Numeric(NumericLit),
    Char(char),
    String(String),
    /// JSON `null`; only valid in JSON contexts (arrays, quoted-key objects).
    Null,
    /// The unit value `Unit`.
    Unit,
}

/// A polymorphic numeric literal.
///
/// Stores the original parsed form (integer or float) so the interpreter
/// can convert to the type-checker-determined concrete type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NumericLit {
    /// Integer literal (e.g., `42`, `-10`).
    Int(i64),
    /// Float literal (e.g., `3.14`, `-2.5`).
    Float(f64),
}

/// An expression node.
///
/// All recursive references use `ExprId` indices into the `Ast` arena.
#[derive(Clone, Debug, PartialEq)]
// Note that the largest vs. smallest is not that great in this case, clippy
// is being a bit too conservative IMO
#[allow(clippy::large_enum_variant)]
pub(crate) enum Expr {
    /// A literal value.
    Literal(Literal),

    /// String interpolation: `"text {expr} more text"`.
    ///
    /// Contains alternating literal parts and expression IDs:
    /// - Even indices: literal text segments (as `ExprId` pointing to `Literal(String)`)
    /// - Odd indices: expression IDs
    ///
    /// For example, `"Hello {name}!"` becomes `[Lit("Hello "), VarId, Lit("!")]`.
    Interpolation(SmallVec<[ExprId; 4]>),

    /// A lexical variable reference (let bindings).
    ///
    /// `x` becomes `Var("x")`. Only looks up in lexical scope; does not
    /// fall back to B-tree locals.
    Var(StringId),

    /// Database intrinsic: `@get`, `@set`, `@kill`, `@data`, `@order`, `@query`.
    ///
    /// Unified variant for all DB intrinsics:
    /// - `Intrinsic`: which operation (`Get`, `Set`, `Kill`, `Data`, `Order`, `Query`)
    /// - `RefTarget`: the database reference target (inline `DbRef` or expression)
    /// - `Option<ExprId>`: value argument (only for `@set`)
    /// - `Option<TxnId>`: transaction context (assigned during typecheck)
    Intrinsic(Intrinsic, RefTarget, Option<ExprId>, Option<TxnId>),

    /// A binary operation.
    Binary(ExprId, BinOp, ExprId),

    /// A unary operation.
    Unary(UnOp, ExprId),

    /// A function call: `callee(args...)`.
    ///
    /// The callee is an expression (identifier, field access, another call, etc.).
    /// Most functions have 0-4 arguments, so `SmallVec` avoids heap allocation.
    ///
    /// Examples:
    /// - `foo(1, 2)` -> `Call(Var("foo"), [1, 2])`
    /// - `ops.inc(5)` -> `Call(Field(Var("ops"), "inc"), [5])`
    /// - `make_adder(5)(10)` -> `Call(Call(Var("make_adder"), [5]), [10])`
    Call(ExprId, SmallVec<[ExprId; 4]>),

    /// An object/record literal with potential spread entries.
    ///
    /// Supports both regular fields and spread entries:
    /// - `{ name: "Alice", age: 30 }` (fields only)
    /// - `{ ...base, age: 31 }` (spread + override)
    /// - `{ ...a, ...b }` (merge multiple objects)
    Object(Vec<ObjectEntry>),

    /// An array literal with potential spread elements.
    ///
    /// Supports both regular elements and spread elements:
    /// - `[1, 2, 3]` (elements only)
    /// - `[...arr1, ...arr2]` (spread multiple arrays)
    /// - `[0, ...arr, 4]` (prepend/append)
    Array(Vec<ArrayElem>),

    /// A tuple literal: `(a, b)`, `(x, y, z)`, `(single,)`.
    ///
    /// Tuples are heterogeneous fixed-size sequences. Access by numeric index
    /// (`.0`, `.1`, etc.) is handled by `TupleIndex`.
    Tuple(SmallVec<[ExprId; 4]>),

    /// A map literal: `{ key => value, ... }`.
    ///
    /// Keys can be any expression that evaluates to a scalar (Bool, Int, Float,
    /// Char, String). The `=>` separator distinguishes map literals from object
    /// literals (which use `:`).
    MapLit(SmallVec<[(ExprId, ExprId); 8]>),

    /// Tuple index access: `tuple.0`, `tuple.1`.
    ///
    /// The index is a compile-time constant; runtime indexing uses `Index`.
    TupleIndex(ExprId, u32),

    /// Index access: `expr[index]`.
    Index(ExprId, ExprId),

    /// Optional index access: `expr?[index]`.
    ///
    /// Safe indexing that returns `Option[T]` instead of panicking:
    /// - `Array[T]?[Int]` returns `Option[T]`
    /// - `Map[K, V]?[K]` returns `Option[V]` (always safe; maps already return Option)
    /// - `String?[Int]` returns `Option[Char]`
    OptionalIndex(ExprId, ExprId),

    /// Field access: `expr.field`.
    Field(ExprId, StringId),

    /// Optional field access: `expr?.field`.
    ///
    /// Short-circuits to `Option.None` if base is `None`; otherwise wraps
    /// the field value in `Option.Some`.
    OptionalField(ExprId, StringId),

    /// Variant constructor: `Type.Variant(args...)` or `Type.Variant`.
    ///
    /// Created by the name resolution pass from `Field(Var(type), variant)`
    /// for zero-arity variants, or from `Call(Field(Var(type), variant), args)`
    /// for variants with arguments.
    ///
    /// Examples: `Option.None` (no args), `Option.Some(1)`, `Result.Ok(42)`
    Variant(QualifiedName, StringId, SmallVec<[ExprId; 4]>),

    /// Namespace path for module functions and constants.
    ///
    /// Created by the name resolution pass from `Field(Var(module), name)` when
    /// `module` is a known built-in module (e.g., `Array`, `String`, `Math`).
    ///
    /// Examples: `Array.length`, `String.split`, `Math.PI`
    ///
    /// When evaluated, produces a `Value::ModuleFn` that can be called directly
    /// or used as a first-class value (e.g., in pipelines).
    Path(SmallVec<[StringId; 4]>),

    /// Class method call: `Class:method(args)`.
    ///
    /// Dispatches to a typeclass method. Class name is resolved to `ClassId`
    /// during typechecking.
    ///
    /// Examples: `Numeric:add(a, b)`, `Fallible:unwrap(opt)`, `Mappable:map(fn, arr)`
    ClassMethod(StringId, StringId, SmallVec<[ExprId; 4]>),

    /// Class method reference: `Class:method` or `Class[T, ...]:method`.
    ///
    /// A first-class function value that can be passed around and called later.
    /// Example: `let f = Filterable:filter`
    ///
    /// The type arguments (`SmallVec`) are required for convert methods
    /// (`Wrappable:wrap`, `Into:into`, `TryInto:try-into`) when used as
    /// first-class values, to specify the target type.
    ClassMethodRef(StringId, SmallVec<[AstTypeExprId; 2]>, StringId),

    /// Naked class method call: `:method(args)`.
    ///
    /// Class is resolved during type checking by searching all classes for a
    /// unique match. Ambiguous method names produce a type error.
    NakedClassMethod(StringId, SmallVec<[ExprId; 4]>),

    /// Naked class method reference: `:method`.
    ///
    /// Like `NakedClassMethod` but as a first-class value.
    NakedClassMethodRef(StringId),

    /// Type check: `expr is Pattern`.
    ///
    /// Returns `true` if the value matches the pattern. For `VariantBind`
    /// patterns, bindings are only visible in the `then` branch of an `if`.
    Is(ExprId, TypePattern),

    /// Type cast: `expr as Type`.
    ///
    /// Explicit infallible type conversion. Supported conversions:
    /// - `Int -> Float` (widen)
    /// - `Float -> Int` (truncate)
    /// - `T -> String` (stringify)
    /// - `Bool -> Int` (`false` -> `0`, `true` -> `1`)
    As(ExprId, AstTypeExprId),

    /// Fallible type conversion: `expr read Type`.
    ///
    /// Returns `Result[T, String]` instead of runtime error. Conversions:
    /// - `String -> Int`: parse, `Result.Err` if invalid
    /// - `String -> Float`: parse, `Result.Err` if invalid
    /// - `Int -> Bool`: `0`/`1` only, else `Result.Err`
    Read(ExprId, AstTypeExprId),

    /// A block expression: `{ stmt...; expr }`.
    ///
    /// Executes statements for side effects, then evaluates to the trailing
    /// expression. If no trailing expression, evaluates to `Option.None`.
    Block(Vec<StmtId>, Option<ExprId>),

    /// Conditional expression: `if cond { then } else { else }`.
    ///
    /// Evaluates to the value of the taken branch. If no else branch and
    /// condition is false, evaluates to `Option.None`.
    If(ExprId, ExprId, Option<ExprId>),

    /// Match expression: `match expr { pattern => body, ... }`.
    ///
    /// Evaluates the scrutinee once, then tries each arm in order. The first
    /// arm whose pattern matches (and whose guard, if any, is `true`) has its
    /// body evaluated. Errors if no arm matches.
    Match(ExprId, Vec<MatchArm>),

    /// Closure (anonymous function): `x => expr` or `(a, b) => expr`.
    ///
    /// - type_params: optional type parameters with constraints (e.g., `[T]`, `[T: Numeric]`)
    /// - params: parameter names with optional type annotations
    /// - return type annotation (optional)
    /// - body expression
    ///
    /// Closures capture their lexical environment at creation time (by value).
    Closure {
        type_params: SmallVec<[TypeParam; 2]>,
        params: SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
        ret: Option<AstTypeExprId>,
        body: ExprId,
    },

    /// Postfix operator: `expr!` (unwrap), etc.
    ///
    /// Currently only `Unwrap` (`!`), which extracts the payload from
    /// `Option.Some` or `Result.Ok`; produces a runtime error for
    /// `Option.None` or `Result.Err(e)`.
    Postfix(PostfixOp, ExprId),

    /// Range: `start..end` (exclusive) or `start..=end` (inclusive).
    ///
    /// Creates a lazy iterator of integers from `start` to `end`.
    /// Does not allocate an array; used with collection operations.
    ///
    /// - First `ExprId`: start expression
    /// - Second `ExprId`: end expression
    /// - `bool`: `true` for inclusive (`..=`), `false` for exclusive (`..`)
    Range(ExprId, ExprId, bool),

    /// Type annotation: `(expr) : Type`.
    ///
    /// Explicit type annotation on an expression. The interpreter validates
    /// that the value matches the annotated type at runtime; the type checker
    /// (once implemented) will use this as the expected type.
    Annotate(ExprId, AstTypeExprId),

    /// A JSON object literal: `{ "key": value, ... }`.
    ///
    /// Distinguished from native `Object` by having quoted string keys.
    /// Evaluates to `Value::Json`.
    Json(Vec<(StringId, ExprId)>),

    /// JSON field access operators.
    ///
    /// | Operator | Kind               | Returns                                  |
    /// |----------|--------------------|------------------------------------------|
    /// | `.`      | `Field`            | `Json` (null if missing)                 |
    /// | `..`     | `ScalarField`      | `Option[Bool \| Int \| Float \| String]` |
    /// | `->`     | `Key`              | `Json` (null if missing)                 |
    /// | `->>`    | `ScalarKey`        | `Option[Bool \| Int \| Float \| String]` |
    JsonAccess(ExprId, JsonAccessKind, JsonAccessKey),

    /// Regex literal: `/pattern/`.
    ///
    /// Compiles to a `Value::Regex` at runtime. The pattern is validated
    /// during type checking; invalid patterns produce type errors.
    ///
    /// The `Option<u32>` is filled in during typechecking with the cache
    /// index of the compiled regex.
    Regex(String, Option<u32>),

    /// Regex match: `expr matches pattern`.
    ///
    /// Returns `Bool`. The left operand must be `Into[String]` (convertible to
    /// `String`); the right operand must be `Regex`.
    Matches(ExprId, ExprId),

    /// Catch expression: `expr catch e => handler`.
    ///
    /// Evaluates `expr`; on runtime error, calls handler closure with `Error`
    /// value. Handler must return the same type as `expr`.
    Catch(ExprId, ExprId),

    /// Write expression: `write expr [json] [to target]`.
    ///
    /// Executes the write side effect and evaluates to `Unit`.
    /// This allows `write` in expression contexts like `f(write x)`.
    Write(WriteExpr),

    /// Raise a runtime error: `raise expr`.
    ///
    /// Evaluates `expr` (must be `Into[String]`) and raises a runtime error.
    /// Never returns; can unify with any expected type.
    Raise(ExprId),

    /// Forever loop: `forever seed (state, cont) => body`.
    ///
    /// A functional looping construct using continuation-passing style:
    /// - `seed`: Initial state value
    /// - `state_param`: Name (and optional type) for the state parameter
    /// - `cont_param`: Name (and optional type) for the continuation pseudo-function
    /// - `body`: Loop body expression
    ///
    /// The continuation is a pseudo-function; calling it signals the loop should
    /// continue with the provided value as the new state. If the body evaluates
    /// without calling the continuation, the loop terminates and returns that value.
    Forever {
        seed: ExprId,
        state_param: (StringId, Option<AstTypeExprId>),
        cont_param: (StringId, Option<AstTypeExprId>),
        body: ExprId,
    },

    /// Transaction block expression: `transaction { ... }`.
    Transaction(TransactionExpr),

    /// Monoid identity (`mempty`): `_` in expression context.
    ///
    /// Type-inferred from context to produce the empty value for a `Monoid` type:
    /// - `String`: `""`
    /// - `Array[T]`: `[]`
    /// - `Map[K, V]`: `{}`
    /// - `Option[T]`: `Option.None`
    Mempty,

    /// A database reference literal: `data{1, 2}` or `^global{key}`.
    ///
    /// Creates a `Ref` value that can be passed to intrinsics or stored.
    /// Type: `Ref`.
    Ref(DbRef),
}

/// The kind of JSON access operation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JsonAccessKind {
    /// `.` or `->`: returns `Json` (null if missing)
    Json,
    /// `..` or `->>`: extracts scalar, returns `Option[T]`
    Scalar,
}

/// The key specification for JSON access.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JsonAccessKey {
    /// Static field name: `data.field` or `data..field`
    Field(StringId),
    /// Dynamic key expression: `data->"key"` or `data->>"key"`
    Expr(ExprId),
}

/// Output format modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    /// Default: stringify the value.
    #[default]
    Default,
    /// Convert to JSON before output.
    Json,
    /// `raw`: preserve escape sequences (e.g. `\n` displays as `\n`).
    Raw,
}

/// Output target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputTarget {
    /// Default: stdout.
    #[default]
    Stdout,
    /// Write to stderr.
    Stderr,
    /// Write to a file (path expression).
    File(ExprId),
}

/// Extended write expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WriteExpr {
    pub(crate) expr: ExprId,
    pub(crate) format: OutputFormat,
    pub(crate) target: OutputTarget,
}

/// Transaction block expression.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransactionExpr {
    /// Unique ID for this transaction block (assigned during typecheck).
    pub(crate) id: Option<TxnId>,
    /// Statements in the transaction body.
    pub(crate) stmts: Vec<StmtId>,
    /// Optional trailing expression (return value).
    pub(crate) expr: Option<ExprId>,
    /// Transaction modifiers.
    pub(crate) modifiers: TransactionModifiers,
}

/// Transaction configuration modifiers.
///
/// Uses storage layer types directly for conflict and isolation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TransactionModifiers {
    pub(crate) conflict: Option<rumps_storage::ConflictStrategy>,
    pub(crate) timeout: Option<ExprId>,
    pub(crate) retries: Option<u32>,
    pub(crate) isolation: Option<rumps_storage::IsolationLevel>,
}

/// Visibility modifier for module members.
///
/// Inside a module, items are private by default. Use `+` prefix to make
/// them public (e.g., `+let`, `+fun`, `+type`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Visibility {
    /// Private; only accessible within the module (default).
    #[default]
    Private,
    /// Public; accessible from outside the module (`+` prefix).
    Public,
}

/// A single import item.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ImportItem {
    /// Named import: `member` or `member as alias`.
    Named {
        name: StringId,
        alias: Option<StringId>,
    },
    /// Wildcard import: `...`.
    Wildcard,
    /// Exclusion (only valid after wildcard): `-member`.
    Exclude(StringId),
}

/// Import statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Import {
    /// Module path segments (e.g., `["Module", "Nested"]`).
    pub(crate) path: SmallVec<[StringId; 2]>,
    /// Import items.
    pub(crate) items: Vec<ImportItem>,
}

impl Import {
    /// Construct a wildcard import for the given module name.
    pub(crate) fn wildcard(module: StringId) -> Self {
        Self {
            path: smallvec![module],
            items: vec![ImportItem::Wildcard],
        }
    }
}

/// An associated type declaration in a class definition.
///
/// Represents `newtype Element` inside a `class ... { ... }` block.
/// Declaration only; the `= Type` assignment belongs to instances.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AstClassAssocTypeDecl {
    pub(crate) name: StringId,
    pub(crate) span: Span,
}

/// A method signature in a class definition.
///
/// Represents `fun method[T](params) -> RetType` inside a class definition.
/// Signature only; no body (bodies belong to instances).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AstClassMethodSig {
    pub(crate) name: StringId,
    pub(crate) type_params: SmallVec<[TypeParam; 2]>,
    pub(crate) params: SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
    pub(crate) ret: Option<AstTypeExprId>,
    pub(crate) span: Span,
}

/// A method definition in a class instance.
///
/// Represents `fun method(params) -> RetType { body }` inside a `class ... for ...` block.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InstanceMethodDef {
    pub(crate) name: StringId,
    pub(crate) params: SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
    pub(crate) ret: Option<AstTypeExprId>,
    pub(crate) body: ExprId,
    pub(crate) span: Span,
}

/// An associated type definition in a class instance.
///
/// Represents `newtype Index = Int` or `newtype Index: Ord = Int` inside a
/// `class ... for ...` block.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AssocTypeDef {
    /// Associated type name (e.g., `"Index"`).
    pub(crate) name: StringId,
    /// Optional constraint on the associated type.
    pub(crate) constraint: Option<TypeClass<AstTypeExprId>>,
    /// The concrete type this associated type maps to.
    pub(crate) target: AstTypeExprId,
    pub(crate) span: Span,
}

/// A statement node.
///
/// All recursive references use `ExprId`/`StmtId` indices into the `Ast` arena.
// TODO: `ClassInstance` variant is ~1236 bytes due to nested SmallVecs; consider
// using `Vec` for `methods` or reducing inline capacity to shrink enum size.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stmt {
    /// Lexical binding with destructuring: `let pattern = expr`.
    ///
    /// Supports simple identifiers (`let x = ...`), tuples (`let (a, b) = ...`),
    /// objects (`let { x, y } = ...`), and arrays (`let [h, ...t] = ...`).
    ///
    /// The optional `AstTypeExprId` is the type annotation; if present, the
    /// interpreter validates that the value's type matches (applies to the
    /// entire RHS value, not individual bindings).
    ///
    /// The visibility is only meaningful inside modules (`+let` for public).
    Let(BindingPattern, Option<AstTypeExprId>, ExprId, Visibility),

    /// An expression used as a statement (for side effects).
    ///
    /// Used for effectful expressions: `Expr::If`, `Expr::Block`,
    /// `Expr::Intrinsic`, `Expr::Write`, etc.
    Expr(ExprId),

    /// Named function definition: `fun name (params) { body }`.
    ///
    /// - `name`: the function's identifier
    /// - `type_params`: optional type parameters with constraints (e.g., `[T]`, `[T: Numeric]`)
    /// - `params`: parameter names with optional type annotations
    /// - `ret`: optional return type annotation
    /// - `body`: the function body expression (typically a block)
    ///
    /// Named functions support recursion (the name is visible in the body).
    ///
    /// The visibility is only meaningful inside modules (`+fun` for public).
    Fun {
        name: StringId,
        type_params: SmallVec<[TypeParam; 2]>,
        params: SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
        ret: Option<AstTypeExprId>,
        body: ExprId,
        vis: Visibility,
    },

    /// User-defined sum type declaration: `type Name = Variant1 | Variant2(T)`.
    ///
    /// Examples:
    /// - `type Status = Pending | Active | Completed`
    /// - `type Event = Click(Int, Int) | KeyPress(Char)`
    /// - `type Either[L, R] = Left(L) | Right(R)`
    ///
    /// The visibility is only meaningful inside modules (`+type` for public).
    Type {
        name: StringId,
        type_params: SmallVec<[TypeParam; 2]>,
        def: TypeDefAst,
        vis: Visibility,
    },

    /// Transparent type alias: `newtype Name = Type` or `newtype Name[T] = Type`.
    ///
    /// Creates a fully transparent alias; `newtype I = Int` makes `I`
    /// interchangeable with `Int`. Supports parametric polymorphism.
    ///
    /// Examples:
    /// - `newtype Person = { name: String, age: Int }`
    /// - `newtype I = Int`
    /// - `newtype IntMap[V] = Map[Int, V]`
    ///
    /// The visibility is only meaningful inside modules (`+newtype` for public).
    NewType {
        name: StringId,
        type_params: SmallVec<[TypeParam; 2]>,
        target: AstTypeExprId,
        vis: Visibility,
    },

    /// Union type declaration: `union Name = Type1 | Type2 | ...`.
    ///
    /// Named unions define a type that can be any of the member types.
    /// Unlike sum types (`type`), union members are existing types, not variants.
    ///
    /// Examples:
    /// - `union Storable = Bool | Int | Float | Char | String | Json`
    /// - `union Numeric = Int | Float`
    /// - `union F[T] = Int | Option[T]`
    ///
    /// The visibility is only meaningful inside modules (`+union` for public).
    Union {
        name: StringId,
        type_params: SmallVec<[TypeParam; 2]>,
        members: SmallVec<[AstTypeExprId; 4]>,
        vis: Visibility,
    },

    /// User-defined module: `module Name { ... }`.
    ///
    /// Modules group related functions, constants, and nested modules.
    /// Contents can include:
    /// - `fun` definitions (registered as module functions)
    /// - `let` bindings (registered as module constants)
    /// - Nested `module` definitions (registered as submodules)
    ///
    /// The body contains `StmtId`s; only `Fun`, `Let`, and `Module` are valid.
    /// This is enforced during parsing.
    Module { name: StringId, body: Vec<StmtId> },

    /// Import members from a module: `import Module.{ member, ... }`.
    ///
    /// Syntax variants:
    /// - `import M.{ member }` ; single named import
    /// - `import M.{ m1, m2 }` ; multiple named imports
    /// - `import M.{ member as alias }` ; import with alias
    /// - `import M.{ ... }` ; import all public members
    /// - `import M.{ ..., -excluded }` ; wildcard with exclusions
    Import(Import),

    /// User-defined class definition: `class Name[params] SelfVar : Supers { body }`.
    ///
    /// Declares a new typeclass with method signatures and associated types.
    ///
    /// Examples:
    /// - `class MyEq C { fun eq(a: C, b: C) -> Bool }`
    /// - `class Into[T] C { fun into(x: C) -> T }`
    /// - `class Container C { newtype Element; fun get(c: C, idx: Int) -> :Element }`
    ClassDef {
        /// Class name (e.g., `"MyEq"`, `"Container"`).
        name: StringId,
        /// Class-level type parameters (e.g., `[T]` in `class Into[T] C`).
        class_params: SmallVec<[TypeParam; 2]>,
        /// The constrained type variable (e.g., `C`).
        self_var: StringId,
        /// Superclass constraints.
        supers: SmallVec<[TypeClass<AstTypeExprId>; 2]>,
        /// Associated type declarations (e.g., `newtype Element`).
        assoc_types: SmallVec<[AstClassAssocTypeDecl; 2]>,
        /// Method signatures (no bodies).
        methods: SmallVec<[AstClassMethodSig; 4]>,
    },

    /// User-defined class instance: `class ClassName for Type { methods }`.
    ///
    /// Implements a builtin class (`Display`, `Into`, `Ord`, etc.) for a user
    /// type (`type`, `newtype`, or `union`).
    ///
    /// Examples:
    /// - `class Display for Point { fun display(p: Point) -> String { ... } }`
    /// - `class Into[String] for UserId { fun into(id: UserId) -> String { ... } }`
    /// - `class Display for Pair[A, B] where A: Display, B: Display { ... }`
    ClassInstance {
        /// Class name (e.g., `"Display"`, `"Into"`, `"Ord"`).
        class_name: StringId,
        /// Class type arguments (e.g., `[String]` for `Into[String]`).
        class_args: SmallVec<[AstTypeExprId; 2]>,
        /// Type parameters for polymorphic instances (e.g., `[A, B]` in `Pair[A, B]`).
        type_params: SmallVec<[TypeParam; 2]>,
        /// The user type implementing the class.
        for_type: AstTypeExprId,
        /// `where` clause constraints (e.g., `A: Display, B: Display`).
        ///
        /// Each entry is `(type_param_name, constraints)`.
        constraints: SmallVec<[(StringId, AstClassConstraints); 2]>,
        /// Associated type definitions (e.g., `newtype Index = Int`).
        assoc_types: SmallVec<[AssocTypeDef; 2]>,
        /// Method implementations.
        methods: SmallVec<[InstanceMethodDef; 4]>,
    },
}
