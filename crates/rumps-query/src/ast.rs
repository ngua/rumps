//! AST definitions for the RUMPS query language.
//!
//! Uses arena allocation with indices instead of `Box` for cache-friendliness
//! and to avoid deep pointer chains. Spans are stored in parallel vectors
//! for cache efficiency; the interpreter rarely needs spans during execution.

#![allow(dead_code)]

use rumps_types::Name;
use smallvec::SmallVec;

use crate::{Error, Result, Span};

/// Unique identifier for a `TRANSACTION` block.
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
}

/// A type expression in the AST (for annotations).
///
/// Represents syntactic type expressions before resolution to runtime types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AstTypeExpr {
    /// Simple named type: `Int`, `String`, `Option`, etc.
    Named(String),

    /// Parameterized type: `Array[Int]`, `Option[String]`, `Result[Int, String]`.
    App(String, SmallVec<[AstTypeExprId; 2]>),

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
    Object(SmallVec<[(String, AstTypeExprId); 4]>),
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

/// Unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Neg, // `-`
    Not, // `NOT` or `!`
    /// Prefix `?` wraps a value in `Option.Some`.
    ///
    /// `?x` produces `Option.Some(x)`. For nested wrapping, use parens:
    /// `?(?x)` produces `Option.Some(Option.Some(x))`. Note that `??x`
    /// is parsed as the coalesce operator, not nested wrap.
    Wrap,
}

/// Rest pattern for array destructuring.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RestPattern {
    /// `..` ; ignore remaining elements
    Ignore,
    /// `...name` ; bind remaining elements to `name`
    Bind(String),
}

/// A binding pattern for destructuring in `LET` statements.
///
/// Patterns allow extracting values from composite structures (tuples, objects,
/// arrays) and binding them to multiple variables in a single statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BindingPattern {
    /// Simple variable binding: `x`
    Var(String),

    /// Tuple destructuring: `(a, b, c)`
    Tuple(Vec<Self>),

    /// Object destructuring: `{ name, age }` or `{ name: n, age: a }`
    ///
    /// Each entry is `(field_name, pattern)`. Shorthand `{ name }` is lowered to
    /// `{ name: name }` (i.e., `("name", Var("name"))`).
    Object(Vec<(String, Self)>),

    /// Array destructuring: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
    ///
    /// - First vec: patterns for fixed-position elements
    /// - `Option<RestPattern>`: optional rest handling
    Array(Vec<Self>, Option<RestPattern>),

    /// Wildcard: `_` (ignore this position)
    Wildcard,
}

impl From<&str> for BindingPattern {
    fn from(s: &str) -> Self {
        Self::Var(s.into())
    }
}

impl From<String> for BindingPattern {
    fn from(s: String) -> Self {
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
    Variant(String, String),

    /// Variant check ignoring payload: `is Option.Some(_)`.
    VariantWildcard(String, String),

    /// Variant check with binding: `is Option.Some(val)`.
    ///
    /// Bindings are only visible in the `then` branch of an `IF`.
    VariantBind(String, String, SmallVec<[String; 2]>),

    /// Structural object check: `is { name: String, age: Int }`.
    ///
    /// Extensible record semantics: value matches if it has at least these fields.
    Object(SmallVec<[(String, AstTypeExprId); 4]>),
}

/// A match pattern for the `MATCH` expression.
///
/// Patterns destructure values and bind variables. Unlike `BindingPattern` (for
/// `LET`), match patterns can include literals and variant constructors for
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
    Var(String),

    /// Literal: `0`, `"hello"`, `true`
    ///
    /// Matches only if the value equals the literal.
    Literal(Literal),

    /// Variant with sub-patterns: `Option.Some(x)`, `Result.Err(e)`
    ///
    /// Matches a tagged value if the type and variant match, then recursively
    /// matches the payloads against the sub-patterns.
    Variant(String, String, SmallVec<[MatchPatternId; 2]>),

    /// Object destructuring: `{ name, age }`, `{ name, role: "admin" }`
    ///
    /// Each entry is `(field_name, pattern)`. Shorthand `{ name }` desugars to
    /// `{ name: name }` (i.e., match field and bind to same-named variable).
    /// Additional fields in the value are allowed (partial matching).
    Object(SmallVec<[(String, MatchPatternId); 4]>),

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
    Is(String, AstTypeExprId),
}

/// A match arm in a `MATCH` expression.
///
/// Each arm consists of a pattern, an optional guard condition, and a body
/// expression. Arms are tried in order; the first matching arm is evaluated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MatchArm {
    /// The pattern to match against the scrutinee.
    pub(crate) pattern: MatchPatternId,

    /// Optional guard condition: `IF cond`.
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
    pub name: String,
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
    Field(String, ExprId),
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
/// `GET`, `SET`, `KILL`, `DATA`, and `ORDER`. This ensures at the AST level
/// that B-tree references cannot be used as values directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DbRef {
    /// Local B-tree variable: `data`, `data(1)`, `data(...keys)`.
    Local(String, SmallVec<[SubscriptElem; 4]>),
    /// Global B-tree variable: `^PATIENT`, `^DATA(1, ...rest)`.
    Global(String, SmallVec<[SubscriptElem; 4]>),
}

impl DbRef {
    /// Splits into the storage `Name` and subscript elements.
    pub(crate) fn split(&self) -> (Name, &SmallVec<[SubscriptElem; 4]>) {
        match self {
            Self::Local(n, s) => (Name::local(n), s),
            Self::Global(n, s) => (Name::global(n), s),
        }
    }
}

/// A literal value in the AST.
///
/// This is the compile-time representation; runtime values (with arena
/// allocation and string interning) are defined separately in the `value`
/// module.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Literal {
    Bool(bool),
    Int(i64),
    Float(f64),
    Char(char),
    String(String),
    /// JSON `null`; only valid in JSON contexts (arrays, quoted-key objects).
    Null,
    /// The unit value `Unit`.
    Unit,
}

/// An expression node.
///
/// All recursive references use `ExprId` indices into the `Ast` arena.
#[derive(Clone, Debug, PartialEq)]
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

    /// A lexical variable reference (LET bindings).
    ///
    /// `x` becomes `Var("x")`. Only looks up in lexical scope; does not
    /// fall back to B-tree locals.
    Var(String),

    /// `GET` primitive.
    ///
    /// Reads a value from a B-tree variable. The `DbRef` specifies the
    /// variable name and subscripts. The `Option<TxnId>` is assigned during
    /// typecheck; `Some(id)` means use transaction `id`, `None` means direct DB.
    Get(DbRef, Option<TxnId>),

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
    Field(ExprId, String),

    /// Optional field access: `expr?.field`.
    ///
    /// Short-circuits to `Option.None` if base is `None`; otherwise wraps
    /// the field value in `Option.Some`.
    OptionalField(ExprId, String),

    /// Variant constructor: `Type.Variant(args...)` or `Type.Variant`.
    ///
    /// Created by the name resolution pass from `Field(Var(type), variant)`
    /// for zero-arity variants, or from `Call(Field(Var(type), variant), args)`
    /// for variants with arguments.
    ///
    /// Examples: `Option.None` (no args), `Option.Some(1)`, `Result.Ok(42)`
    Variant(String, String, SmallVec<[ExprId; 4]>),

    /// Namespace path for module functions and constants.
    ///
    /// Created by the name resolution pass from `Field(Var(module), name)` when
    /// `module` is a known built-in module (e.g., `Array`, `String`, `Math`).
    ///
    /// Examples: `Array.length`, `String.split`, `Math.PI`
    ///
    /// When evaluated, produces a `Value::ModuleFn` that can be called directly
    /// or used as a first-class value (e.g., in pipelines).
    Path(SmallVec<[String; 4]>),

    /// Type check: `expr is Pattern`.
    ///
    /// Returns `true` if the value matches the pattern. For `VariantBind`
    /// patterns, bindings are only visible in the `then` branch of an `IF`.
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

    /// Conditional expression: `IF cond { then } ELSE { else }`.
    ///
    /// Evaluates to the value of the taken branch. If no else branch and
    /// condition is false, evaluates to `Option.None`.
    If(ExprId, ExprId, Option<ExprId>),

    /// Match expression: `MATCH expr { pattern => body, ... }`.
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
        params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        ret: Option<AstTypeExprId>,
        body: ExprId,
    },

    /// Unwrap: `expr!`
    ///
    /// Extracts the payload from `Option.Some` or `Result.Ok`; produces a
    /// runtime error for `Option.None` or `Result.Err(e)` (where `e` is
    /// stringified in the error message).
    Unwrap(ExprId),

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
    Json(Vec<(String, ExprId)>),

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

    /// Regex match: `expr MATCHES pattern`.
    ///
    /// Returns `Bool`. The left operand must be `Stringable` (convertible to
    /// `String`); the right operand must be `Regex`.
    Matches(ExprId, ExprId),

    /// Catch expression: `expr CATCH e => handler`.
    ///
    /// Evaluates `expr`; on runtime error, calls handler closure with `Error`
    /// value. Handler must return the same type as `expr`.
    Catch(ExprId, ExprId),

    /// Data query: `DATA var`.
    ///
    /// Queries the existence status of a node. Returns `DataStatus` enum.
    /// The `Option<TxnId>` is assigned during typecheck.
    Data(DbRef, Option<TxnId>),

    /// Order query: `ORDER var`.
    ///
    /// Returns the next subscript at a given level. Returns `Option[Subscript]`.
    /// The `Option<TxnId>` is assigned during typecheck.
    Order(DbRef, Option<TxnId>),

    /// Query: `@QUERY var`.
    ///
    /// Returns the full key path to the next node with a value.
    /// Returns `Option[Array[Subscript]]`.
    /// The `Option<TxnId>` is assigned during typecheck.
    Query(DbRef, Option<TxnId>),

    /// Write expression: `WRITE expr [JSON] [TO target]`.
    ///
    /// Executes the write side effect and evaluates to `Unit`.
    /// This allows `WRITE` in expression contexts like `f(WRITE x)`.
    Write(WriteExpr),

    /// Set expression: `@SET target = value`.
    ///
    /// Executes the B-tree assignment and evaluates to `Unit`.
    /// This allows `@SET` in expression contexts like `f(@SET x = 1)`.
    /// The `Option<TxnId>` is assigned during typecheck; globals require it.
    Set(DbRef, ExprId, Option<TxnId>),

    /// Kill expression: `@KILL target`.
    ///
    /// Deletes a variable or subtree and evaluates to `Unit`.
    /// This allows `@KILL` in expression contexts like `f(@KILL x)`.
    /// The `Option<TxnId>` is assigned during typecheck; globals require it.
    Kill(DbRef, Option<TxnId>),

    /// Raise a runtime error: `RAISE expr`.
    ///
    /// Evaluates `expr` (must be `Stringable`) and raises a runtime error.
    /// Never returns; can unify with any expected type.
    Raise(ExprId),

    /// Forever loop: `FOREVER seed (state, cont) => body`.
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
        state_param: (String, Option<AstTypeExprId>),
        cont_param: (String, Option<AstTypeExprId>),
        body: ExprId,
    },

    /// Transaction block expression: `TRANSACTION { ... }`.
    Transaction(TransactionExpr),

    /// Monoid identity (`mempty`): `_` in expression context.
    ///
    /// Type-inferred from context to produce the empty value for a `Monoid` type:
    /// - `String`: `""`
    /// - `Array[T]`: `[]`
    /// - `Map[K, V]`: `{}`
    /// - `Option[T]`: `Option.None`
    Mempty,
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
    Field(String),
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
/// them public (e.g., `+LET`, `+FUN`, `+TYPE`).
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
    /// Named import: `member` or `member AS alias`.
    Named { name: String, alias: Option<String> },
    /// Wildcard import: `...`.
    Wildcard,
    /// Exclusion (only valid after wildcard): `-member`.
    Exclude(String),
}

/// Import statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Import {
    /// Module path segments (e.g., `["Module", "Nested"]`).
    pub(crate) path: Vec<String>,
    /// Import items.
    pub(crate) items: Vec<ImportItem>,
}

/// A statement node.
///
/// All recursive references use `ExprId`/`StmtId` indices into the `Ast` arena.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stmt {
    /// Lexical binding with destructuring: `LET pattern = expr`.
    ///
    /// Supports simple identifiers (`LET x = ...`), tuples (`LET (a, b) = ...`),
    /// objects (`LET { x, y } = ...`), and arrays (`LET [h, ...t] = ...`).
    ///
    /// The optional `AstTypeExprId` is the type annotation; if present, the
    /// interpreter validates that the value's type matches (applies to the
    /// entire RHS value, not individual bindings).
    ///
    /// The visibility is only meaningful inside modules (`+LET` for public).
    Let(BindingPattern, Option<AstTypeExprId>, ExprId, Visibility),

    /// An expression used as a statement (for side effects).
    ///
    /// Used for effectful expressions: `Expr::If`, `Expr::Block`, `Expr::Set`,
    /// `Expr::Kill`, `Expr::Write`, etc.
    Expr(ExprId),

    /// Named function definition: `FUN name (params) { body }`.
    ///
    /// - `name`: the function's identifier
    /// - `type_params`: optional type parameters with constraints (e.g., `[T]`, `[T: Numeric]`)
    /// - `params`: parameter names with optional type annotations
    /// - `ret`: optional return type annotation
    /// - `body`: the function body expression (typically a block)
    ///
    /// Named functions support recursion (the name is visible in the body).
    ///
    /// The visibility is only meaningful inside modules (`+FUN` for public).
    Fun {
        name: String,
        type_params: SmallVec<[TypeParam; 2]>,
        params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        ret: Option<AstTypeExprId>,
        body: ExprId,
        vis: Visibility,
    },

    /// User-defined sum type declaration: `TYPE Name = Variant1 | Variant2(T)`.
    ///
    /// Examples:
    /// - `TYPE Status = Pending | Active | Completed`
    /// - `TYPE Event = Click(Int, Int) | KeyPress(Char)`
    /// - `TYPE Either[L, R] = Left(L) | Right(R)`
    ///
    /// The visibility is only meaningful inside modules (`+TYPE` for public).
    Type {
        name: String,
        type_params: SmallVec<[TypeParam; 2]>,
        def: TypeDefAst,
        vis: Visibility,
    },

    /// Transparent type alias: `NEWTYPE Name = Type` or `NEWTYPE Name[T] = Type`.
    ///
    /// Creates a fully transparent alias; `NEWTYPE I = Int` makes `I`
    /// interchangeable with `Int`. Supports parametric polymorphism.
    ///
    /// Examples:
    /// - `NEWTYPE Person = { name: String, age: Int }`
    /// - `NEWTYPE I = Int`
    /// - `NEWTYPE IntMap[V] = Map[Int, V]`
    ///
    /// The visibility is only meaningful inside modules (`+NEWTYPE` for public).
    NewType {
        name: String,
        type_params: SmallVec<[TypeParam; 2]>,
        target: AstTypeExprId,
        vis: Visibility,
    },

    /// Union type declaration: `UNION Name = Type1 | Type2 | ...`.
    ///
    /// Named unions define a type that can be any of the member types.
    /// Unlike sum types (`TYPE`), union members are existing types, not variants.
    ///
    /// Examples:
    /// - `UNION Storable = Bool | Int | Float | Char | String | Json`
    /// - `UNION Numeric = Int | Float`
    /// - `UNION F[T] = Int | Option[T]`
    ///
    /// The visibility is only meaningful inside modules (`+UNION` for public).
    Union {
        name: String,
        type_params: SmallVec<[TypeParam; 2]>,
        members: SmallVec<[AstTypeExprId; 4]>,
        vis: Visibility,
    },

    /// User-defined module: `MODULE Name { ... }`.
    ///
    /// Modules group related functions, constants, and nested modules.
    /// Contents can include:
    /// - `FUN` definitions (registered as module functions)
    /// - `LET` bindings (registered as module constants)
    /// - Nested `MODULE` definitions (registered as submodules)
    ///
    /// The body contains `StmtId`s; only `Fun`, `Let`, and `Module` are valid.
    /// This is enforced during parsing.
    Module { name: String, body: Vec<StmtId> },

    /// Import members from a module: `IMPORT Module.{ member, ... }`.
    ///
    /// Syntax variants:
    /// - `IMPORT M.{ member }` ; single named import
    /// - `IMPORT M.{ m1, m2 }` ; multiple named imports
    /// - `IMPORT M.{ member AS alias }` ; import with alias
    /// - `IMPORT M.{ ... }` ; import all public members
    /// - `IMPORT M.{ ..., -excluded }` ; wildcard with exclusions
    Import(Import),
}
