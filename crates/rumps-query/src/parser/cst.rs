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

use crate::ast::{BinOp, Intrinsic, JsonAccessKind, Literal, UnOp};
use crate::intern::StringId;
use crate::Span;

/// A shape-agnostic class constraint as parsed from source.
///
/// Stores the class name as a `StringId` and whatever type arguments the user
/// wrote, without classifying them as `Simple`/`Hkt`/`Parameterized`.
/// Shape resolution happens during lowering, where the `ClassRegistry` is
/// available.
#[derive(Clone, Debug)]
pub(crate) struct CstClassConstraint {
    pub(crate) tag: StringId,
    pub(crate) args: SmallVec<[TypeExpr; 1]>,
    pub(crate) span: Span,
}

/// A type parameter with optional class constraints.
///
/// Represents `T` or `T: Class1 + Class2` in type parameter lists.
#[derive(Clone, Debug)]
pub(crate) struct TypeParam {
    pub name: StringId,
    pub constraints: SmallVec<[CstClassConstraint; 2]>,
}

/// Type pattern for the `is` operator (CST version).
///
/// This is the CST equivalent of `ast::TypePattern`. During lowering,
/// CST `TypeExpr` fields are converted to `AstTypeExprId`.
#[derive(Clone, Debug)]
pub(crate) enum TypePattern {
    /// Type check: `is Int`, `is Array[String]`, `is Map[Int, String]`.
    Type(TypeExpr),

    /// Variant check without payload: `is Option.None`.
    Variant(StringId, StringId),

    /// Unqualified variant check without payload: `is .None`.
    NakedVariant(StringId),

    /// Variant check ignoring payload: `is Option.Some(_)`.
    VariantWildcard(StringId, StringId),

    /// Unqualified variant check ignoring payload: `is .Some(_)`.
    NakedVariantWildcard(StringId),

    /// Variant check with binding: `is Option.Some(val)`.
    VariantBind(StringId, StringId, SmallVec<[StringId; 2]>),

    /// Unqualified variant check with binding: `is .Some(val)`.
    NakedVariantBind(StringId, SmallVec<[StringId; 2]>),

    /// Structural object check: `is { name: String, age: Int }`.
    Object(Vec<(StringId, TypeExpr)>),
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
    Field(StringId, Expr),
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
/// `get`, `set`, `kill`, `data`, and `order`.
#[derive(Clone, Debug)]
pub(crate) enum DbRef {
    /// Local B-tree variable: `data{1}`, `data{...keys}`, `data{}`.
    Local(StringId, Vec<SubscriptElem>),
    /// Global B-tree variable: `^PATIENT`, `^DATA{1, ...rest}`.
    Global(StringId, Vec<SubscriptElem>),
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
    Var(StringId),

    /// Database intrinsic: `@get`, `@set`, `@kill`, `@data`, `@order`, `@query`.
    ///
    /// - `Intrinsic`: which operation
    /// - `Box<Expr>`: the reference target
    /// - `Option<Box<Expr>>`: value argument (only for `@set`)
    Intrinsic(Intrinsic, Box<Expr>, Option<Box<Expr>>),

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
    Field(Box<Expr>, StringId),

    /// Optional field access.
    OptionalField(Box<Expr>, StringId),

    /// Variant constructor.
    ///
    /// NOTE: `Expr::Path` exists in AST but not CST. `Path` is reserved for
    /// future module support. The name resolution pass converts field access
    /// on registered types (`Type.Variant`) to `Expr::Variant`; the parser
    /// emits generic `Field` and `Call` nodes.
    Variant(StringId, StringId, Vec<Expr>),

    /// Unqualified variant constructor: `.Variant(args...)` or `.Variant`.
    NakedVariant(StringId, Vec<Expr>),

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
        params: SmallVec<[(StringId, Option<TypeExpr>); 4]>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
    },

    /// Match expression: `match expr { pattern => body, ... }`.
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
    /// Explicit type annotation on an expression. The type checker validates
    /// the edge statically. Runtime only preserves approved `newtype`
    /// representation metadata.
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
    /// Returns `Bool`. The left operand must be `Into[String]`.
    Matches(Box<Expr>, Box<Expr>),

    /// Catch expression: `expr CATCH handler`.
    ///
    /// Evaluates `expr`; on runtime error, calls `handler` with `Error` value.
    /// Handler must be a closure `(Error) -> T` where `T` matches expr's type.
    Catch(Box<Expr>, Box<Expr>),

    /// Write expression: `write expr [JSON] [TO target]`.
    ///
    /// Executes the write side effect and evaluates to `Unit`.
    /// This allows `write` in expression contexts.
    Write(Box<WriteStmt>),

    /// Raise a runtime error: `raise expr`.
    ///
    /// Evaluates `expr` (must be `Into[String]`) and raises a runtime error.
    /// Never returns; can unify with any expected type.
    Raise(Box<Expr>),

    /// Loop expression: `loop seed (state, cont) => body`.
    Loop {
        seed: Box<Expr>,
        state_param: (StringId, Option<TypeExpr>),
        cont_param: (StringId, Option<TypeExpr>),
        body: Box<Expr>,
    },

    /// Transaction block expression: `transaction { ... }`.
    Transaction(Box<TransactionExpr>),

    /// Monoid identity (`mempty`): `_` in expression context.
    ///
    /// Produces the empty/identity value for the inferred `Monoid` type.
    Mempty,

    /// A database reference literal: `data{1, 2}` or `^global{key}`.
    ///
    /// Creates a first-class `Ref` value that can be stored or passed to
    /// functions. Use with intrinsics: `@get r`, `@set r value`.
    RefLit(DbRef),

    /// Placeholder for pipe operator: `.` in call arguments.
    ///
    /// Only valid as an argument in a function call on the RHS of `|>`.
    /// During pipe expression transformation, replaced with the LHS value.
    /// Any remaining placeholders after transformation are errors.
    PipePlaceholder,

    /// Class method call: `Class:method(args)`.
    ///
    /// Dispatches to a typeclass method. Examples:
    /// - `Numeric:add(a, b)` (binary method)
    /// - `Fallible:unwrap(opt)` (unary method)
    /// - `Mappable:map(fn, arr)` (higher-order method)
    ClassMethod(StringId, StringId, Vec<Expr>),

    /// Class method reference: `Class:method` or `Class[T, ...]:method`.
    ///
    /// A first-class function value. Can be assigned to variables and called
    /// later. Example: `let f = Filterable:filter`.
    ///
    /// The optional type arguments (`Vec<TypeExpr>`) are required for convert
    /// methods (`Wrappable:wrap`, `Into:into`, `TryInto:try-into`) when used as
    /// first-class values, to specify the target type.
    ClassMethodRef(StringId, Vec<TypeExpr>, StringId),

    /// Naked class method call: `:method(args)`.
    ///
    /// Like `ClassMethod` but without the class name prefix. The class is
    /// resolved during type checking; ambiguous names are errors.
    NakedClassMethod(StringId, Vec<Expr>),

    /// Naked class method reference: `:method`.
    ///
    /// Like `ClassMethodRef` but without the class name prefix.
    NakedClassMethodRef(StringId),
}

/// The key specification for JSON access (CST form).
#[derive(Clone, Debug)]
pub(crate) enum JsonAccessKey {
    /// Static field name: `data.field` or `data..field`
    Field(StringId),
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
    Bind(StringId),
}

/// Visibility modifier for module members.
///
/// Inside a module, items are private by default. Use `+` prefix to make
/// them public (e.g., `+let`, `+fun`, `+variant`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Visibility {
    /// Private; only accessible within the module (default).
    #[default]
    Private,
    /// Public; accessible from outside the module (`+` prefix).
    Public,
}

/// A binding pattern for destructuring in `let` statements (CST form).
#[derive(Clone, Debug)]
pub(crate) enum BindingPattern {
    /// Simple variable binding: `x`
    Var(StringId),

    /// Tuple destructuring: `(a, b, c)`
    Tuple(Vec<Self>),

    /// Object destructuring: `{ name, age }` or `{ name: n, age: a }`
    Object(Vec<(StringId, Self)>),

    /// Array destructuring: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
    Array(Vec<Self>, Option<RestPattern>),

    /// Wildcard: `_`
    Wildcard,
}

/// An associated type declaration in a class definition (CST form).
///
/// Represents `newtype Element` inside a `class ... { ... }` block.
/// Declaration only; the `= Type` assignment belongs to instances.
#[derive(Clone, Debug)]
pub(crate) struct ClassAssocTypeDecl {
    pub(crate) name: StringId,
    pub(crate) span: Span,
}

/// A method signature in a class definition (CST form).
///
/// Represents `fun method[T](params) -> RetType` inside a class definition.
/// Signature only; no body (bodies belong to instances).
#[derive(Clone, Debug)]
pub(crate) struct ClassMethodSig {
    pub(crate) name: StringId,
    pub(crate) type_params: Vec<TypeParam>,
    pub(crate) params: SmallVec<[(StringId, Option<TypeExpr>); 4]>,
    pub(crate) ret: Option<TypeExpr>,
    pub(crate) span: Span,
}

/// A method definition in a class instance (CST form).
#[derive(Clone, Debug)]
pub(crate) struct InstanceMethodDef {
    pub(crate) name: StringId,
    pub(crate) type_params: Vec<TypeParam>,
    pub(crate) params: SmallVec<[(StringId, Option<TypeExpr>); 4]>,
    pub(crate) ret: Option<TypeExpr>,
    pub(crate) body: Expr,
    pub(crate) span: Span,
}

/// An associated type definition in a class instance (CST form).
///
/// Represents `newtype Index = Int` or `newtype Index: Ord = Int` inside a
/// `class ... FOR ...` block.
#[derive(Clone, Debug)]
pub(crate) struct AssocTypeCst {
    /// Associated type name (e.g., `"Index"`).
    pub(crate) name: StringId,
    /// Optional constraint on the associated type.
    pub(crate) constraint: Option<CstClassConstraint>,
    /// The concrete type this associated type maps to.
    pub(crate) target: TypeExpr,
    pub(crate) span: Span,
}

/// The kind of a CST statement.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub(crate) enum StmtKind {
    /// Lexical binding with destructuring.
    ///
    /// The visibility is only meaningful inside modules (`+let` for public).
    Let(BindingPattern, Option<TypeExpr>, Expr, Visibility),

    /// Write a value with optional format and target.
    Write(WriteStmt),

    /// An expression used as a statement.
    Expr(Expr),

    /// Named function definition.
    ///
    /// The visibility is only meaningful inside modules (`+fun` for public).
    Fun {
        name: StringId,
        type_params: Vec<TypeParam>,
        params: SmallVec<[(StringId, Option<TypeExpr>); 4]>,
        ret: Option<TypeExpr>,
        body: Expr,
        vis: Visibility,
    },

    /// User-defined variant declaration: `variant Name = Variant1 | Variant2(T)`.
    ///
    /// The visibility is only meaningful inside modules (`+variant` for public).
    Type {
        name: StringId,
        type_params: Vec<TypeParam>,
        def: TypeDefCst,
        vis: Visibility,
    },

    /// Newtype declaration: `newtype Name = Type` or `newtype Name[T] = +Type`.
    ///
    /// `type visibility` controls access to the `newtype` name. `repr visibility`
    /// controls external access to representation edges used by
    /// annotation, `as`, `read`, and derived conversion class dispatch.
    Newtype {
        name: StringId,
        type_params: Vec<TypeParam>,
        target: TypeExpr,
        vis: Visibility,
        repr_vis: Visibility,
    },

    /// Union type declaration: `union Name = Type1 | Type2 | ...`.
    ///
    /// Anonymous unions for function parameters etc. use inline syntax
    /// (`x: Int | String`). Named unions are registered in the type registry.
    ///
    /// The visibility is only meaningful inside modules (`+union` for public).
    Union {
        name: StringId,
        type_params: Vec<TypeParam>,
        members: Vec<TypeExpr>,
        vis: Visibility,
    },

    /// User-defined module declaration.
    ///
    /// Modules group related functions, constants, and nested modules.
    /// Contents can include:
    /// - `fun` definitions (registered as module functions)
    /// - `let` bindings (registered as module constants)
    /// - Nested `module` definitions (registered as submodules)
    ///
    /// The body contains `Stmt`s; only `Fun`, `Let`, and `Module` are valid.
    /// This is enforced during typechecking.
    ///
    /// Two forms are supported:
    /// - Inline: `module Name { ... }`
    /// - File import: `module Name FROM "path/to/module.rumps"`
    Module {
        name: StringId,
        source: ModuleSource,
    },

    /// Import members from a module.
    ///
    /// Syntax: `import Module.{ member, ... }` or `import Module.{ ... }`.
    Import(ImportStmt),

    /// User-defined class definition: `class Name[params] SelfVar : Supers { body }`.
    ///
    /// Declares a new typeclass with method signatures and associated types.
    /// The body may contain `fun` signatures (no body) and `newtype` declarations.
    ///
    /// Examples:
    /// - `class MyEq C { fun eq(a: C, b: C) -> Bool }`
    /// - `class Into[T] C { fun into(x: C) -> T }`
    /// - `class Container C { newtype Element; fun get(c: C, idx: Int) -> :Element }`
    ClassDef {
        /// Class name (e.g., `"MyEq"`, `"Container"`).
        name: StringId,
        /// Class-level type parameters (e.g., `[T]` in `class Into[T] C`).
        class_params: Vec<TypeParam>,
        /// The constrained type variable (e.g., `C`).
        self_var: StringId,
        /// Superclass constraints (e.g., `MyEq` in `class MyOrd C : MyEq`).
        supers: SmallVec<[CstClassConstraint; 2]>,
        /// Associated type declarations (e.g., `newtype Element`).
        assoc_types: SmallVec<[ClassAssocTypeDecl; 2]>,
        /// Method signatures (no bodies).
        methods: Vec<ClassMethodSig>,
    },

    /// User-defined class instance: `class ClassName FOR Type { methods }`.
    ///
    /// Implements a builtin class (`Display`, `Into`, `Ord`, etc.) for a user
    /// type (`variant`, `newtype`, or `union`).
    ///
    /// Examples:
    /// - `class Display FOR Point { fun display(p: Point) -> String { ... } }`
    /// - `class Into[String] FOR UserId { fun into(id: UserId) -> String { ... } }`
    /// - `class Display FOR Pair[A, B] WHERE A: Display, B: Display { ... }`
    ClassInstance {
        /// Class name (e.g., `"Display"`, `"Into"`, `"Ord"`).
        class_name: StringId,
        /// Class type arguments (e.g., `[String]` for `Into[String]`).
        class_args: Vec<TypeExpr>,
        /// Type parameters for polymorphic instances (e.g., `[A, B]` in `Pair[A, B]`).
        type_params: Vec<TypeParam>,
        /// The user type implementing the class.
        for_type: TypeExpr,
        /// WHERE clause constraints (e.g., `A: Display, B: Display`).
        ///
        /// Each entry is `(type_param_name, constraints)`.
        constraints: Vec<(StringId, Vec<CstClassConstraint>)>,
        /// Associated type definitions (e.g., `newtype Index = Int`).
        assoc_types: Vec<AssocTypeCst>,
        /// Method implementations.
        methods: Vec<InstanceMethodDef>,
    },
}

/// Source of a module's contents.
#[derive(Clone, Debug)]
pub(crate) enum ModuleSource {
    /// Inline module body: `module Name { ... }`.
    Inline(Vec<Stmt>),
    /// File import: `module Name FROM "path/to/module.rumps"`.
    ///
    /// The path is relative to the importing script's directory, or absolute.
    File(String),
}

/// A single import item.
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
pub(crate) struct ImportStmt {
    /// Module path segments (e.g., `["Module", "Nested"]`).
    pub(crate) path: SmallVec<[StringId; 2]>,
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
    /// Wildcard type: `_`.
    Wildcard,

    /// Simple named type.
    Named(Vec<StringId>),

    /// Parameterized type.
    App(Vec<StringId>, Vec<TypeExpr>),

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
    Object(Vec<(StringId, TypeExpr)>),

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
    /// Used in the `for` clause of class instances to declare a tuple as an
    /// HKT type constructor. Empty positions are element slots (determined by
    /// class kind); filled positions are fixed params.
    TupleConstructor {
        arity: u8,
        fixed: Vec<(u8, TypeExpr)>,
    },
}

/// A variant definition in a user-defined sum type (CST form).
#[derive(Clone, Debug)]
pub(crate) struct VariantCst {
    pub name: StringId,
    pub payloads: Vec<TypeExpr>,
}

/// A type definition body (CST form).
///
/// Note: Only sum types remain; struct aliases now use `newtype`.
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
    Var(StringId),

    /// Literal: `0`, `"hello"`, `true`
    Literal(Literal),

    /// Variant with sub-patterns: `Option.Some(x)`, `Result.Err(e)`
    Variant(Vec<StringId>, StringId, Vec<Self>),

    /// Unqualified variant with sub-patterns: `.Some(x)`, `.Err(e)`.
    NakedVariant(StringId, Vec<Self>),

    /// Object destructuring: `{ name, age }`
    Object(Vec<(StringId, Self)>),

    /// Tuple pattern: `(a, b, c)`
    Tuple(Vec<Self>),

    /// Array pattern: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
    ///
    /// - First vec: patterns for fixed-position elements
    /// - `Option<RestPattern>`: optional rest handling
    Array(Vec<Self>, Option<RestPattern>),

    /// Type-narrowing pattern: `x is Int`, `val is String`
    ///
    /// Matches if the value is of the specified type and binds it to the name.
    Is(StringId, TypeExpr),
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
    /// Raw: preserve escape sequences (e.g. `\n` displays as `\n`).
    Raw,
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
