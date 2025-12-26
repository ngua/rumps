//! AST definitions for the RUMPS query language.
//!
//! Uses arena allocation with indices instead of `Box` for cache-friendliness
//! and to avoid deep pointer chains. Spans are stored in parallel vectors
//! for cache efficiency; the interpreter rarely needs spans during execution.

#![allow(dead_code)]

use smallvec::SmallVec;

use crate::{Error, Result, Span};

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

    /// Iterate over all expression IDs.
    pub(crate) fn expr_ids(&self) -> impl Iterator<Item = ExprId> {
        (0..self.exprs.len()).map(ExprId)
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
    /// Simple type check: `is Int`, `is String`.
    Type(String),

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

/// A type definition body for user-defined types.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TypeDefAst {
    /// Sum type: `Variant1 | Variant2(T) | ...`
    ///
    /// Each variant is a named constructor with optional payload types.
    Sum(SmallVec<[VariantAst; 4]>),

    /// Structural object type alias: `{ field1: Type1, field2: Type2, ... }`
    ///
    /// Each entry is `(field_name, field_type)`. At runtime these map to
    /// `Value::Object`; the struct type enables optional validation.
    Struct(Vec<(String, AstTypeExprId)>),
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

    /// A lexical variable reference (LET bindings).
    ///
    /// `x` becomes `Var("x")`. Only looks up in lexical scope; does not
    /// fall back to B-tree locals.
    Var(String),

    /// A local B-tree variable with subscripts.
    ///
    /// `x(1, "KEY")` becomes `Local("x", [1, "KEY"])`.
    /// Requires `GET` to read the value.
    Local(String, SmallVec<[ExprId; 4]>),

    /// A global B-tree variable with subscripts.
    ///
    /// `^PATIENT(123, "NAME")` becomes `Global("PATIENT", [123, "NAME"])`.
    /// Requires `GET` to read the value.
    Global(String, SmallVec<[ExprId; 4]>),

    /// `GET` primitive.
    ///
    /// Reads a value from a B-tree variable (`Local` or `Global`).
    Get(ExprId),

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

    /// An object/record literal: `{ key: value, ... }`.
    Object(Vec<(String, ExprId)>),

    /// An array literal: `[expr, ...]`.
    Array(Vec<ExprId>),

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
    /// - params: parameter names with optional type annotations
    /// - return type annotation (optional)
    /// - body expression
    ///
    /// Closures capture their lexical environment at creation time (by value).
    Closure {
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
    /// | Operator | Kind               | Returns                                |
    /// |----------|--------------------|----------------------------------------|
    /// | `.`      | `Field`            | `Json` (null if missing)               |
    /// | `..`     | `ScalarField`      | `Option[Bool \| Int \| Float \| String]` |
    /// | `->`     | `Key`              | `Json` (null if missing)               |
    /// | `->>`    | `ScalarKey`        | `Option[Bool \| Int \| Float \| String]` |
    JsonAccess(ExprId, JsonAccessKind, JsonAccessKey),
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
    Let(BindingPattern, Option<AstTypeExprId>, ExprId),

    /// B-tree assignment: `SET x(subs...) = expr` or `SET ^NAME(subs...) = expr`.
    ///
    /// The first `ExprId` must be a `Local` or `Global` expression (the target);
    /// the second is the value expression.
    Set(ExprId, ExprId),

    /// Delete a variable or subtree: `KILL x(subs...)` or `KILL ^NAME(subs...)`.
    ///
    /// The `ExprId` must be a `Local` or `Global` expression.
    Kill(ExprId),

    /// Output a value: `OUTPUT expr`.
    Output(ExprId),

    /// An expression used as a statement (for side effects).
    ///
    /// This is the canonical way to use `Expr::If` and `Expr::Block` as statements.
    Expr(ExprId),

    /// Named function definition: `FUN name (params) { body }`.
    ///
    /// - `name`: the function's identifier
    /// - `params`: parameter names with optional type annotations
    /// - `ret`: optional return type annotation
    /// - `body`: the function body expression (typically a block)
    ///
    /// Named functions support recursion (the name is visible in the body).
    Fun {
        name: String,
        params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        ret: Option<AstTypeExprId>,
        body: ExprId,
    },

    /// User-defined type declaration: `TYPE Name = ...` or `TYPE Name[T] = ...`.
    ///
    /// - `name`: the type's identifier (e.g., `Status`, `Event`)
    /// - `type_params`: optional type parameters (e.g., `[T]`, `[L, R]`)
    /// - `def`: the type definition body (sum type or struct)
    ///
    /// Examples:
    /// - `TYPE Status = Pending | Active | Completed`
    /// - `TYPE Event = Click(Int, Int) | KeyPress(Char)`
    /// - `TYPE Either[L, R] = Left(L) | Right(R)`
    Type {
        name: String,
        type_params: SmallVec<[String; 2]>,
        def: TypeDefAst,
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
    Union {
        name: String,
        type_params: SmallVec<[String; 2]>,
        members: SmallVec<[AstTypeExprId; 4]>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_basic() {
        let mut ast = Ast::new();

        let lit = ast
            .add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2))
            .unwrap();
        let var = ast
            .add_expr(Expr::Var("x".into()), Span::new(4, 5))
            .unwrap();

        assert_eq!(ast.expr_count(), 2);
        assert_eq!(ast.get_expr(lit), Some(&Expr::Literal(Literal::Int(42))));
        assert_eq!(ast.get_expr(var), Some(&Expr::Var("x".into())));
        assert_eq!(ast.expr_span(lit), Some(Span::new(0, 2)));
        assert_eq!(ast.expr_span(var), Some(Span::new(4, 5)));
    }

    #[test]
    fn arena_binary_expr() {
        let mut ast = Ast::new();

        // Build: 1 + 2
        let lhs = ast
            .add_expr(Expr::Literal(Literal::Int(1)), Span::new(0, 1))
            .unwrap();
        let rhs = ast
            .add_expr(Expr::Literal(Literal::Int(2)), Span::new(4, 5))
            .unwrap();
        let add = ast
            .add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 5))
            .unwrap();

        assert_eq!(ast.expr_count(), 3);
        assert_eq!(
            ast.get_expr(add),
            Some(&Expr::Binary(lhs, BinOp::Add, rhs))
        );
    }

    #[test]
    fn arena_statements() {
        let mut ast = Ast::new();

        // Build: LET x = 10
        let val = ast
            .add_expr(Expr::Literal(Literal::Int(10)), Span::new(8, 10))
            .unwrap();
        let pat = BindingPattern::Var("x".into());
        let stmt = ast
            .add_stmt(Stmt::Let(pat.clone(), None, val), Span::new(0, 10))
            .unwrap();

        assert_eq!(ast.stmt_count(), 1);
        assert_eq!(ast.get_stmt(stmt), Some(&Stmt::Let(pat, None, val)));
        assert_eq!(ast.stmt_span(stmt), Some(Span::new(0, 10)));
    }

    #[test]
    fn arena_global_with_subscripts() {
        let mut ast = Ast::new();

        // Build: ^PATIENT(123, "NAME")
        let sub1 = ast
            .add_expr(Expr::Literal(Literal::Int(123)), Span::new(9, 12))
            .unwrap();
        let sub2 = ast
            .add_expr(
                Expr::Literal(Literal::String("NAME".into())),
                Span::new(14, 20),
            )
            .unwrap();
        let global = ast
            .add_expr(
                Expr::Global("PATIENT".into(), smallvec::smallvec![sub1, sub2]),
                Span::new(0, 21),
            )
            .unwrap();

        assert_eq!(ast.expr_count(), 3);
        match ast.get_expr(global) {
            Some(Expr::Global(name, subs)) => {
                assert_eq!(name, "PATIENT");
                assert_eq!(subs.len(), 2);
            }
            _ => panic!("expected Global"),
        }
    }

    #[test]
    fn arena_if_expr() {
        let mut ast = Ast::new();

        // Build: IF x > 0 { 1 } ELSE { 0 }
        let x = ast
            .add_expr(Expr::Var("x".into()), Span::new(3, 4))
            .unwrap();
        let zero = ast
            .add_expr(Expr::Literal(Literal::Int(0)), Span::new(7, 8))
            .unwrap();
        let cond = ast
            .add_expr(Expr::Binary(x, BinOp::Gt, zero), Span::new(3, 8))
            .unwrap();

        let one = ast
            .add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13))
            .unwrap();
        let then_blk = ast
            .add_expr(Expr::Block(vec![], Some(one)), Span::new(10, 15))
            .unwrap();

        let zero2 = ast
            .add_expr(Expr::Literal(Literal::Int(0)), Span::new(23, 24))
            .unwrap();
        let else_blk = ast
            .add_expr(Expr::Block(vec![], Some(zero2)), Span::new(21, 26))
            .unwrap();

        let if_expr = ast
            .add_expr(
                Expr::If(cond, then_blk, Some(else_blk)),
                Span::new(0, 26),
            )
            .unwrap();

        assert_eq!(ast.expr_count(), 8);
        match ast.get_expr(if_expr) {
            Some(Expr::If(c, then_br, else_br)) => {
                assert_eq!(*c, cond);
                assert_eq!(*then_br, then_blk);
                assert_eq!(*else_br, Some(else_blk));
            }
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn arena_nested_binary() {
        let mut ast = Ast::new();

        // Build: (1 + 2) * 3
        let one = ast
            .add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2))
            .unwrap();
        let two = ast
            .add_expr(Expr::Literal(Literal::Int(2)), Span::new(5, 6))
            .unwrap();
        let add = ast
            .add_expr(Expr::Binary(one, BinOp::Add, two), Span::new(1, 6))
            .unwrap();

        let three = ast
            .add_expr(Expr::Literal(Literal::Int(3)), Span::new(10, 11))
            .unwrap();
        let mul = ast
            .add_expr(Expr::Binary(add, BinOp::Mul, three), Span::new(0, 11))
            .unwrap();

        assert_eq!(ast.expr_count(), 5);

        // Verify structure
        match ast.get_expr(mul) {
            Some(Expr::Binary(lhs, BinOp::Mul, rhs)) => {
                assert_eq!(
                    ast.get_expr(*lhs),
                    Some(&Expr::Binary(one, BinOp::Add, two))
                );
                assert_eq!(
                    ast.get_expr(*rhs),
                    Some(&Expr::Literal(Literal::Int(3)))
                );
            }
            _ => panic!("expected Binary Mul"),
        }
    }

    #[test]
    fn arena_set_with_subscripts() {
        let mut ast = Ast::new();

        // Build: SET x(1, "ABC") = 30
        let sub1 = ast
            .add_expr(Expr::Literal(Literal::Int(1)), Span::new(6, 7))
            .unwrap();
        let sub2 = ast
            .add_expr(
                Expr::Literal(Literal::String("ABC".into())),
                Span::new(9, 14),
            )
            .unwrap();
        let target = ast
            .add_expr(
                Expr::Local("x".into(), smallvec::smallvec![sub1, sub2]),
                Span::new(4, 15),
            )
            .unwrap();
        let val = ast
            .add_expr(Expr::Literal(Literal::Int(30)), Span::new(18, 20))
            .unwrap();

        let stmt = ast
            .add_stmt(Stmt::Set(target, val), Span::new(0, 20))
            .unwrap();

        match ast.get_stmt(stmt) {
            Some(Stmt::Set(t, v)) => {
                assert_eq!(*t, target);
                assert_eq!(*v, val);
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn arena_out_of_bounds() {
        let ast = Ast::new();
        assert_eq!(ast.get_expr(ExprId(999)), None);
        assert_eq!(ast.get_stmt(StmtId(999)), None);
        assert_eq!(ast.expr_span(ExprId(999)), None);
        assert_eq!(ast.stmt_span(StmtId(999)), None);
    }

    #[test]
    fn literal_variants() {
        assert_eq!(Literal::Bool(true), Literal::Bool(true));
        assert_eq!(Literal::Int(42), Literal::Int(42));
        assert_eq!(Literal::Float(3.14), Literal::Float(3.14));
        assert_eq!(
            Literal::String("hello".into()),
            Literal::String("hello".into())
        );
    }
}
