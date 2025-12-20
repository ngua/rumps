//! AST interpreter for the RUMPS query language.
//!
//! The interpreter is async because `Database` and `Transaction` methods are async.
//! All variable access (both locals and globals) goes through async `Database` methods.
//!
//! # Type Coercions
//!
//! The interpreter performs various type coercions for operations like string
//! concatenation, comparison, and storage. All conversion methods live on
//! [`Interpreter`] since they require access to the arena and type registry.
//!
//! ## String Coercion
//!
//! String coercion (via [`Interpreter::display`]) converts any value to a
//! human-readable string. Used for `OUTPUT` statements and string concatenation
//! or interpolation.
//!
//! | *Type*   | *Result*                                              |
//! |----------|-------------------------------------------------------|
//! | `Bool`   | `"TRUE"` or `"FALSE"`                                 |
//! | `Int`    | Decimal representation (e.g., `"42"`)                 |
//! | `Float`  | Decimal representation (e.g., `"3.14"`)               |
//! | `String` | The string itself                                     |
//! | `Array`  | `"[ elem1, elem2, ... ]"` (recursive)                 |
//! | `Object` | `"{ key1: val1, key2: val2, ... }"` (recursive)       |
//! | `Tagged` | `"TypeName.Variant"` or `"TypeName.Variant(args...)"` |
//!
//! ## JSON Coercion
//!
//! JSON conversion is used for storage serialization of complex values.
//!
//! **To JSON** ([`Interpreter::jsonify`]):
//! - Scalars map directly (`Bool`, `Int`, `Float`, `String`)
//! - `Array` becomes a JSON array
//! - `Object` becomes a JSON object
//! - `Tagged` becomes `{"_type": "...", "_variant": "...", "_payload": [...]}`
//!   (provisional encoding)
//!
//! **From JSON** ([`Interpreter::unjsonify`]):
//! - `null` becomes `Option.None`
//! - `bool` becomes `Bool`
//! - `number` becomes `Float` (JSON has no int/float distinction)
//! - `string` becomes `String`
//! - `array` becomes `Array` if homogeneous (all elements same JSON type);
//!   (FIXME) heterogeneous arrays are not yet supported
//! - `object` becomes `Object`
//!
//! ## Numeric Coercion
//!
//! For comparison and arithmetic operators, mixed numeric types are coerced:
//!
//! - `Int` vs `Float`: The `Int` is promoted to `Float`
//! - Comparisons (`==`, `<`, etc.) work across `Int`/`Float` boundaries
//! - Division always produces `Float` (use `//` for floor division)
//!
//! ## Storage Coercion
//!
//! Storage conversion translates between runtime `Value` and persistent
//! `rumps_types::Value`.
//!
//! **To storage** ([`Interpreter::store`]):
//! - `Bool`, `Int`, `Float`, `String` map directly
//! - `Array`, `Object`, `Tagged` are serialized as JSON
//!
//! **From storage** ([`Interpreter::load`]):
//! - Direct types map back to their runtime equivalents
//! - JSON is parsed via [`Interpreter::unjsonify`]
//!
//! ## Subscript Coercion
//!
//! Only scalar types can be used as database key subscripts
//! ([`Interpreter::subscript`]):
//!
//! - `Bool`, `Int`, `Float`, `String` are valid subscripts
//! - `Array`, `Object`, `Tagged` cannot be subscripts (returns error)

#![allow(dead_code)]

mod convert;
mod db;
mod ops;

use std::collections::HashMap;

use async_recursion::async_recursion;
use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use rumps_storage::{Database, Transaction};
use smallvec::{smallvec, SmallVec};

use crate::ast::{
    Ast, AstTypeExpr, AstTypeExprId, BinOp, Expr, ExprId, Literal, Stmt,
    StmtId, TypePattern, UnOp,
};
use crate::env::Environment;
use crate::io::IoContext;
use crate::value::{
    CapturedEnv, StringId, TypeExprArena, TypeExprId, TypeId, TypeRegistry,
    Value, ValueArena, ValueId,
};
use crate::{Error, Result, Span};

/// A named function definition stored in the function registry.
#[derive(Clone, Debug)]
struct FunctionDef {
    name: StringId,
    params: SmallVec<[(StringId, Option<TypeExprId>); 4]>,
    ret: Option<TypeExprId>,
    body: ExprId,
}

/// The RUMPS interpreter.
///
/// Walks the AST and evaluates expressions/executes statements. Owns the
/// runtime state (value arena, type registry, environment) and has access
/// to the database for persistent storage operations.
///
/// Generic over `I: IoContext` to support both real I/O and test captures.
pub(crate) struct Interpreter<'a, I: IoContext> {
    /// The parsed AST (borrowed; immutable during interpretation).
    ast: &'a Ast,

    /// Variable environment for lexical `LET` bindings and primitives.
    env: Environment,

    /// Database for all `SET`/`GET` operations (owned).
    ///
    /// The interpreter is the natural owner when running `rumps path/to/db script.rumps`.
    /// `Database` is cheap to clone (internal `Arc`), so ownership has low overhead.
    db: Database,

    /// Active transaction, if any.
    ///
    /// Global writes require a transaction; local writes can happen outside.
    txn: Option<Transaction>,

    /// Arena for runtime values with string interning.
    arena: ValueArena,

    /// Type registry for runtime type information.
    registry: TypeRegistry,

    /// Arena for type expressions (e.g., `Array[Int]`).
    type_exprs: TypeExprArena,

    /// Registry of named functions (FUN definitions).
    functions: HashMap<StringId, FunctionDef>,

    /// I/O context for output operations.
    io: I,
}

// Public API
impl<'a, I: IoContext> Interpreter<'a, I> {
    /// Create a new interpreter for the given AST, database, and I/O context.
    ///
    /// This is the main constructor. It creates the value arena and type
    /// registry, runs name resolution on the AST, and sets up the interpreter.
    /// Takes `&mut Ast` because resolution mutates it, but stores `&Ast`
    /// since interpretation only reads.
    pub(crate) fn new(ast: &'a mut Ast, db: Database, io: I) -> Result<Self> {
        let mut arena = ValueArena::new();
        let registry = TypeRegistry::new(&mut arena)?;
        crate::resolve::resolve(ast, &mut arena, &registry);

        Ok(Self {
            ast,
            env: Environment::new(),
            db,
            txn: None,
            arena,
            registry,
            type_exprs: TypeExprArena::new(),
            functions: HashMap::new(),
            io,
        })
    }

    /// Run a program (a sequence of statements).
    ///
    /// Consumes and returns the interpreter, allowing continued use after execution.
    pub(crate) async fn run(mut self, stmts: &[StmtId]) -> Result<Self> {
        self.stmts(stmts).await?;
        Ok(self)
    }

    /// Consume the interpreter and return the I/O context.
    pub(crate) fn into_io(self) -> I {
        self.io
    }

    /// Create an interpreter with a pre-created arena and registry.
    ///
    /// Used by tests that need direct control over the arena/registry,
    /// bypassing name resolution.
    #[cfg(test)]
    pub(crate) fn with_arena(
        ast: &'a Ast,
        db: Database,
        io: I,
        arena: ValueArena,
        registry: TypeRegistry,
    ) -> Self {
        Self {
            ast,
            env: Environment::new(),
            db,
            txn: None,
            arena,
            registry,
            type_exprs: TypeExprArena::new(),
            functions: HashMap::new(),
            io,
        }
    }

    /// Evaluate an expression.
    #[async_recursion]
    pub(crate) async fn eval(&mut self, id: ExprId) -> Result<Value> {
        let span = self.ast.expr_span(id).unwrap_or_default();
        let expr = self
            .ast
            .get_expr(id)
            .ok_or_else(|| Error::runtime(span, "invalid expression id"))?
            .clone();

        match expr {
            Expr::Literal(lit) => Ok(self.literal(&lit)),
            Expr::Var(name) => self.var(&name, span),
            Expr::Local(_, _) => {
                Err(Error::runtime(span, "cannot use local as value; use GET"))
            }
            Expr::Global(_, _) => {
                Err(Error::runtime(span, "cannot use global as value; use GET"))
            }
            Expr::Get(inner) => self.get(inner, span).await,
            Expr::Binary(lhs, op, rhs) => self.binary(lhs, op, rhs, span).await,
            Expr::Unary(op, operand) => self.unary(op, operand, span).await,
            Expr::Call(callee, args) => self.call(callee, &args, span).await,
            Expr::Object(fields) => self.object(&fields).await,
            Expr::Array(elems) => self.array(&elems).await,
            Expr::Tuple(elems) => self.tuple(&elems, span).await,
            Expr::TupleIndex(base, idx) => {
                self.tuple_index(base, idx, span).await
            }
            Expr::Index(base, idx) => self.index(base, idx, span).await,
            Expr::Field(base, field) => self.field(base, &field, span).await,
            Expr::OptionalField(base, field) => {
                self.optional_field(base, &field, span).await
            }
            Expr::Variant(ty, var, args) => {
                self.variant(&ty, &var, &args, span).await
            }
            Expr::Path(segments) => self.path(&segments, span),
            Expr::Is(expr, pattern) => self.is(expr, &pattern, span).await,
            Expr::As(expr, ty) => self.r#as(expr, ty, span).await,
            Expr::Read(expr, ty) => self.read(expr, ty, span).await,
            Expr::Block(stmts, tail) => self.block(&stmts, tail).await,
            Expr::If(cond, then_br, else_br) => {
                self.r#if(cond, then_br, else_br).await
            }
            Expr::Closure { params, ret, body } => {
                self.closure(&params, ret, body)
            }
        }
    }
}

// Private helpers
impl<I: IoContext> Interpreter<'_, I> {
    /// Execute a sequence of statements.
    ///
    /// Uses async recursion over the slice instead of iteration.
    #[async_recursion]
    async fn stmts(&mut self, stmts: &[StmtId]) -> Result<()> {
        match stmts.split_first() {
            None => Ok(()),
            Some((head, tail)) => {
                self.exec(*head).await?;
                self.stmts(tail).await
            }
        }
    }

    /// Execute a single statement.
    #[async_recursion]
    async fn exec(&mut self, id: StmtId) -> Result<()> {
        let span = self.ast.stmt_span(id).unwrap_or_default();
        let stmt = self
            .ast
            .get_stmt(id)
            .ok_or_else(|| Error::runtime(span, "invalid statement id"))?
            .clone();

        match stmt {
            Stmt::Let(name, ty_ann, expr_id) => {
                self.r#let(&name, ty_ann, expr_id, span).await
            }
            Stmt::Set(target, expr_id) => self.set(target, expr_id, span).await,
            Stmt::Kill(target) => self.kill(target, span).await,
            Stmt::Output(expr_id) => self.output(expr_id).await,
            Stmt::Expr(expr_id) => {
                // Evaluate for side effects, discard result
                self.eval(expr_id).await.map(|_| ())
            }
            Stmt::Fun {
                name,
                params,
                ret,
                body,
            } => self.fun(&name, &params, ret, body, span),
        }
    }

    /// Define a named function.
    ///
    /// Registers the function in the function registry. The function is
    /// immediately available for recursive calls.
    fn fun(
        &mut self,
        name: &str,
        params: &[(String, Option<AstTypeExprId>)],
        ret: Option<AstTypeExprId>,
        body: ExprId,
        span: Span,
    ) -> Result<()> {
        let name_id = self.arena.intern(name);

        // Resolve parameter types
        let resolved_params: Result<
            SmallVec<[(StringId, Option<TypeExprId>); 4]>,
        > = params
            .iter()
            .map(|(pname, ty)| {
                let pname_id = self.arena.intern(pname);
                let ty_id = ty
                    .map(|ast_id| {
                        let s = self.ast.type_expr_span(ast_id).unwrap_or(span);
                        self.resolve_type_expr(ast_id, s)
                    })
                    .transpose()?;
                Ok((pname_id, ty_id))
            })
            .collect();

        // Resolve return type
        let resolved_ret = ret
            .map(|ast_id| {
                let s = self.ast.type_expr_span(ast_id).unwrap_or(span);
                self.resolve_type_expr(ast_id, s)
            })
            .transpose()?;

        // Register the function
        self.functions.insert(
            name_id,
            FunctionDef {
                name: name_id,
                params: resolved_params?,
                ret: resolved_ret,
                body,
            },
        );

        Ok(())
    }

    /// Convert an AST literal to a runtime value.
    fn literal(&mut self, lit: &Literal) -> Value {
        match lit {
            Literal::Bool(b) => Value::Bool(*b),
            Literal::Int(n) => Value::Int(*n),
            Literal::Float(f) => Value::Float(OrderedFloat(*f)),
            Literal::String(s) => Value::String(self.arena.intern(s)),
        }
    }

    /// Create a closure value from AST closure parameters and body.
    ///
    /// Captures the current lexical environment by value. Parameter and return
    /// type annotations are resolved to runtime type expressions.
    fn closure(
        &mut self,
        params: &[(String, Option<AstTypeExprId>)],
        ret: Option<AstTypeExprId>,
        body: ExprId,
    ) -> Result<Value> {
        // Capture the current lexical environment
        let env = CapturedEnv::capture(self.env.scopes.stack());

        // Resolve parameter types and intern names
        let resolved_params: Result<
            SmallVec<[(StringId, Option<TypeExprId>); 4]>,
        > = params
            .iter()
            .map(|(name, ty)| {
                let name_id = self.arena.intern(name);
                let ty_id = ty
                    .map(|ast_id| {
                        let span =
                            self.ast.type_expr_span(ast_id).unwrap_or_default();
                        self.resolve_type_expr(ast_id, span)
                    })
                    .transpose()?;
                Ok((name_id, ty_id))
            })
            .collect();

        // Resolve return type
        let resolved_ret = ret
            .map(|ast_id| {
                let span = self.ast.type_expr_span(ast_id).unwrap_or_default();
                self.resolve_type_expr(ast_id, span)
            })
            .transpose()?;

        Ok(Value::Closure {
            params: resolved_params?,
            ret: resolved_ret,
            body,
            env,
        })
    }

    /// Evaluate a lexical variable reference (LET bindings only).
    ///
    /// Does NOT fall back to B-tree locals; use `GET` for those.
    fn var(&mut self, name: &str, span: Span) -> Result<Value> {
        let name_id = self.arena.intern(name);

        // First try lexical scope
        self.env
            .scopes
            .lookup(name_id)
            .and_then(|val_id| self.arena.get(val_id).cloned())
            .or_else(|| {
                // If not in scope, check if it's a named function
                self.functions.get(&name_id).map(|def| Value::Function {
                    name: def.name,
                    params: def.params.clone(),
                    ret: def.ret,
                    body: def.body,
                })
            })
            .ok_or_else(|| {
                Error::runtime(span, format!("undefined variable `{name}`"))
            })
    }

    /// Evaluate a binary operation.
    ///
    /// Handles short-circuit evaluation for `AND`, `OR`, and `Coalesce`.
    #[async_recursion]
    async fn binary(
        &mut self,
        lhs: ExprId,
        op: BinOp,
        rhs: ExprId,
        span: Span,
    ) -> Result<Value> {
        match op {
            // Short-circuit AND: if left is false, don't evaluate right
            BinOp::And => {
                let left = self.eval(lhs).await?;
                match left {
                    Value::Bool(false) => Ok(Value::Bool(false)),
                    Value::Bool(true) => {
                        let right = self.eval(rhs).await?;
                        match right {
                            Value::Bool(b) => Ok(Value::Bool(b)),
                            _ => Err(Error::type_err(
                                span,
                                format!(
                                    "logical AND requires booleans; got Bool and {}",
                                    right.type_name(&self.registry, &self.type_exprs)
                                ),
                            )),
                        }
                    }
                    _ => Err(Error::type_err(
                        span,
                        format!(
                            "logical AND requires booleans; got {}",
                            left.type_name(&self.registry, &self.type_exprs)
                        ),
                    )),
                }
            }
            // Short-circuit OR: if left is true, don't evaluate right
            BinOp::Or => {
                let left = self.eval(lhs).await?;
                match left {
                    Value::Bool(true) => Ok(Value::Bool(true)),
                    Value::Bool(false) => {
                        let right = self.eval(rhs).await?;
                        match right {
                            Value::Bool(b) => Ok(Value::Bool(b)),
                            _ => Err(Error::type_err(
                                span,
                                format!(
                                    "logical OR requires booleans; got Bool and {}",
                                    right.type_name(&self.registry, &self.type_exprs)
                                ),
                            )),
                        }
                    }
                    _ => Err(Error::type_err(
                        span,
                        format!(
                            "logical OR requires booleans; got {}",
                            left.type_name(&self.registry, &self.type_exprs)
                        ),
                    )),
                }
            }
            // Coalesce: unwrap Option.Some/Result.Ok, or evaluate right for None/Err
            BinOp::Coalesce => {
                let left = self.eval(lhs).await?;
                self.coalesce(left, rhs, span).await
            }
            // Pipeline: both sides evaluated, but requires async function call
            BinOp::Pipe => {
                let left = self.eval(lhs).await?;
                let right = self.eval(rhs).await?;
                self.pipeline(left, right, span).await
            }
            // All other operators: both sides evaluated, sync computation
            _ => {
                let left = self.eval(lhs).await?;
                let right = self.eval(rhs).await?;
                self.apply_binop(&left, op, &right, span)
            }
        }
    }

    /// Coalesce operator implementation.
    ///
    /// - `Option.Some(v)` -> `v` (unwrapped)
    /// - `Option.None` -> evaluate and return rhs
    /// - `Result.Ok(v)` -> `v` (unwrapped)
    /// - `Result.Err(_)` -> evaluate and return rhs (error discarded)
    /// - Other types -> type error
    #[async_recursion]
    async fn coalesce(
        &mut self,
        left: Value,
        rhs: ExprId,
        span: Span,
    ) -> Result<Value> {
        // Helper to check if type expression has a given base type
        let is_option = |ty_expr: TypeExprId| {
            self.type_exprs
                .base_type(ty_expr)
                .is_some_and(|t| t == TypeId::OPTION)
        };
        let is_result = |ty_expr: TypeExprId| {
            self.type_exprs
                .base_type(ty_expr)
                .is_some_and(|t| t == TypeId::RESULT)
        };

        match &left {
            // Option.Some(v) -> unwrap to v
            Value::Tagged(ty_expr, 1, payload) if is_option(*ty_expr) => {
                payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Option.Some missing payload")
                    })
            }
            // Option.None -> evaluate rhs
            Value::Tagged(ty_expr, 0, _) if is_option(*ty_expr) => {
                self.eval(rhs).await
            }
            // Result.Ok(v) -> unwrap to v
            Value::Tagged(ty_expr, 0, payload) if is_result(*ty_expr) => {
                payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Result.Ok missing payload")
                    })
            }
            // Result.Err(_) -> evaluate rhs (error discarded)
            Value::Tagged(ty_expr, 1, _) if is_result(*ty_expr) => {
                self.eval(rhs).await
            }
            // Other types -> type error
            _ => Err(Error::type_err(
                span,
                format!(
                    "`??` requires Option or Result; got {}",
                    left.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Pipeline operator implementation.
    ///
    /// Applies the right operand (function/closure) to the left operand (value):
    /// `value |> func` becomes `func(value)`
    #[async_recursion]
    async fn pipeline(
        &mut self,
        left: Value,
        right: Value,
        span: Span,
    ) -> Result<Value> {
        // Intern left value as argument
        let arg_id = self.arena.add(left, span);

        match right {
            Value::Closure {
                params,
                ret,
                body,
                env,
            } => {
                self.call_closure_with_vals(
                    &params,
                    ret,
                    body,
                    &env,
                    &[arg_id],
                    span,
                )
                .await
            }
            Value::Function {
                params, ret, body, ..
            } => {
                self.call_function_with_vals(
                    &params,
                    ret,
                    body,
                    &[arg_id],
                    span,
                )
                .await
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "`|>` requires function on right side; got {}",
                    right.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Call a closure with pre-evaluated arguments.
    #[async_recursion]
    async fn call_closure_with_vals(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        env: &CapturedEnv,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Save current scope stack and replace with captured environment
            let saved = self.env.scopes.save();
            self.env.scopes.restore_from_captured(env);

            // Push new scope for parameters
            self.env.scopes.push();
            self.bind_params(params, args, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Restore original scope stack
            self.env.scopes.restore(saved);

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Call a named function with pre-evaluated arguments.
    #[async_recursion]
    async fn call_function_with_vals(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Push new scope for parameters
            self.env.scopes.push();
            self.bind_params(params, args, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Pop parameter scope
            self.env.scopes.pop();

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Evaluate a unary operation.
    #[async_recursion]
    async fn unary(
        &mut self,
        op: UnOp,
        operand: ExprId,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(operand).await?;
        self.apply_unop(op, &val, span)
    }

    /// Call a function with an expression-based callee.
    ///
    /// The callee can be:
    /// - A variable (`foo(x)`) resolved via name-based lookup
    /// - A field access (`obj.method(x)`) evaluated then called
    /// - Another call (`make_adder(5)(10)`) for chained calls
    /// - A closure literal (`(x => x * 2)(5)`) for IIFE
    #[async_recursion]
    async fn call(
        &mut self,
        callee: ExprId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        let callee_expr =
            self.ast.get_expr(callee).cloned().ok_or_else(|| {
                Error::runtime(span, "invalid callee expression")
            })?;

        // For variable callees, use name-based resolution (functions first)
        match callee_expr {
            Expr::Var(ref name) => self.call_by_name(name, args, span).await,
            _ => {
                // Evaluate callee expression and call the result
                let callee_val = self.eval(callee).await?;
                self.call_value(callee_val, args, span).await
            }
        }
    }

    /// Call a function by name (for `Var` callees).
    ///
    /// Resolution order:
    /// 1. Named functions (from FUN definitions)
    /// 2. Lexical scope (may be a bound closure)
    #[async_recursion]
    async fn call_by_name(
        &mut self,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        let name_id = self.arena.intern(name);

        // Clone function def to avoid borrow issues with async
        let func_def = self.functions.get(&name_id).cloned();
        let scope_val = func_def.as_ref().map_or_else(
            || {
                self.env
                    .scopes
                    .lookup(name_id)
                    .and_then(|val_id| self.arena.get(val_id).cloned())
            },
            |_| None,
        );

        match (func_def, scope_val) {
            (Some(def), _) => {
                self.call_function(&def.params, def.ret, def.body, args, span)
                    .await
            }
            (None, Some(callee)) => self.call_value(callee, args, span).await,
            (None, None) => Err(Error::runtime(
                span,
                format!("undefined function `{name}`"),
            )),
        }
    }

    /// Call a function or closure value.
    #[async_recursion]
    async fn call_value(
        &mut self,
        callee: Value,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        match callee {
            Value::Closure {
                params,
                ret,
                body,
                env,
            } => {
                self.call_closure(&params, ret, body, &env, args, span)
                    .await
            }
            Value::Function {
                params, ret, body, ..
            } => self.call_function(&params, ret, body, args, span).await,
            _ => Err(Error::runtime(
                span,
                format!(
                    "cannot call non-function value of type {}",
                    callee.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Call a named function (no captured environment).
    #[async_recursion]
    async fn call_function(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        // Check arity
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Evaluate arguments
            let arg_vals = self.eval_args(args).await?;

            // Push new scope and bind parameters
            self.env.scopes.push();
            self.bind_params(params, &arg_vals, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Pop scope
            self.env.scopes.pop();

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Call a closure (with captured environment).
    #[async_recursion]
    async fn call_closure(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        env: &CapturedEnv,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        // Check arity
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Evaluate arguments in current environment
            let arg_vals = self.eval_args(args).await?;

            // Save current scope stack and replace with captured environment
            let saved_scopes = self.env.scopes.save();
            self.env.scopes.restore_from_captured(env);

            // Push new scope for parameters
            self.env.scopes.push();
            self.bind_params(params, &arg_vals, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Restore original scope stack
            self.env.scopes.restore(saved_scopes);

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Check that a return value matches the declared return type.
    fn check_return_type(
        &self,
        val: Value,
        ret: Option<TypeExprId>,
        span: Span,
    ) -> Result<Value> {
        ret.map_or(Ok(val.clone()), |expected_ty| {
            if self.value_matches_type_expr(&val, expected_ty) {
                Ok(val)
            } else {
                let expected = self.format_type_expr(expected_ty);
                let actual = val.type_name(&self.registry, &self.type_exprs);
                Err(Error::type_err(
                    span,
                    format!(
                        "expected return type `{expected}`, got `{actual}`"
                    ),
                ))
            }
        })
    }

    /// Evaluate a list of argument expressions.
    #[async_recursion]
    async fn eval_args(&mut self, args: &[ExprId]) -> Result<Vec<ValueId>> {
        self.eval_args_rec(args, Vec::with_capacity(args.len()))
            .await
    }

    #[async_recursion]
    async fn eval_args_rec(
        &mut self,
        args: &[ExprId],
        mut acc: Vec<ValueId>,
    ) -> Result<Vec<ValueId>> {
        match args.split_first() {
            None => Ok(acc),
            Some((head, tail)) => {
                let span = self.ast.expr_span(*head).unwrap_or_default();
                let val = self.eval(*head).await?;
                let val_id = self.arena.add(val, span);
                acc.push(val_id);
                self.eval_args_rec(tail, acc).await
            }
        }
    }

    /// Bind parameters to argument values in the current scope.
    ///
    /// Validates each argument against its declared type (if any).
    fn bind_params(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        args: &[ValueId],
        span: Span,
    ) -> Result<()> {
        params
            .iter()
            .zip(args.iter())
            .try_for_each(|((name, ty), val_id)| {
                // Validate type if annotated
                ty.map_or(Ok(()), |expected_ty| {
                    self.arena.get(*val_id).map_or(Ok(()), |val| {
                        if self.value_matches_type_expr(val, expected_ty) {
                            Ok(())
                        } else {
                            let pname =
                                self.arena.get_str(*name).unwrap_or("?");
                            let expected = self.format_type_expr(expected_ty);
                            let actual =
                                val.type_name(&self.registry, &self.type_exprs);
                            Err(Error::type_err(
                                span,
                                format!(
                                    "expected `{expected}`, got `{actual}` \
                                     for parameter `{pname}`"
                                ),
                            ))
                        }
                    })
                })?;
                self.env.scopes.bind(*name, *val_id);
                Ok(())
            })
    }

    /// Evaluate an object literal.
    #[async_recursion]
    async fn object(&mut self, fields: &[(String, ExprId)]) -> Result<Value> {
        let map = self.object_fields(fields, IndexMap::new()).await?;
        Ok(Value::Object(map))
    }

    /// Recursively evaluate object fields.
    #[async_recursion]
    async fn object_fields(
        &mut self,
        fields: &[(String, ExprId)],
        mut acc: IndexMap<StringId, ValueId>,
    ) -> Result<IndexMap<StringId, ValueId>> {
        match fields.split_first() {
            None => Ok(acc),
            Some(((key, expr_id), tail)) => {
                let span = self.ast.expr_span(*expr_id).unwrap_or_default();
                let val = self.eval(*expr_id).await?;
                let key_id = self.arena.intern(key);
                let val_id = self.arena.add(val, span);
                acc.insert(key_id, val_id);
                self.object_fields(tail, acc).await
            }
        }
    }

    /// Evaluate an array literal, enforcing homogeneous element types.
    #[async_recursion]
    async fn array(&mut self, elems: &[ExprId]) -> Result<Value> {
        match elems.split_first() {
            None => {
                // Empty array has element type `UNKNOWN`
                let elem_ty = self.type_exprs.named(TypeId::UNKNOWN);
                Ok(Value::Array(elem_ty, SmallVec::new()))
            }
            Some((first, rest)) => {
                let first_span = self.ast.expr_span(*first).unwrap_or_default();
                let first_val = self.eval(*first).await?;
                let elem_ty = self.value_type_expr(&first_val);
                let first_id = self.arena.add(first_val, first_span);

                let mut acc = SmallVec::new();
                acc.push(first_id);

                self.array_elems(rest, elem_ty, acc, first_span).await
            }
        }
    }

    /// Recursively evaluate and type-check array elements.
    #[async_recursion]
    async fn array_elems(
        &mut self,
        elems: &[ExprId],
        elem_ty: TypeExprId,
        mut acc: SmallVec<[ValueId; 4]>,
        first_span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Array(elem_ty, acc)),
            Some((expr_id, tail)) => {
                let span = self.ast.expr_span(*expr_id).unwrap_or_default();
                let val = self.eval(*expr_id).await?;
                let val_ty = self.value_type_expr(&val);

                if self.type_exprs.eq(elem_ty, val_ty) {
                    let val_id = self.arena.add(val, span);
                    acc.push(val_id);
                    self.array_elems(tail, elem_ty, acc, first_span).await
                } else {
                    Err(Error::type_err(
                        span,
                        format!(
                            "array element type mismatch: expected {} (from {}..{}), got {}",
                            self.type_expr_name(elem_ty),
                            first_span.start,
                            first_span.end,
                            self.type_expr_name(val_ty)
                        ),
                    ))
                }
            }
        }
    }

    /// Evaluate a tuple literal.
    ///
    /// Unlike arrays, tuples are heterogeneous; each element can have a different type.
    #[async_recursion]
    async fn tuple(&mut self, elems: &[ExprId], span: Span) -> Result<Value> {
        self.tuple_elems(elems, SmallVec::new(), SmallVec::new(), span)
            .await
    }

    /// Recursively evaluate tuple elements, collecting values and types.
    #[async_recursion]
    async fn tuple_elems(
        &mut self,
        elems: &[ExprId],
        mut vals: SmallVec<[ValueId; 4]>,
        mut tys: SmallVec<[TypeExprId; 4]>,
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => {
                let ty = self.type_exprs.tuple(tys);
                Ok(Value::Tuple(ty, vals))
            }
            Some((expr_id, tail)) => {
                let elem_span = self.ast.expr_span(*expr_id).unwrap_or(span);
                let val = self.eval(*expr_id).await?;
                let ty = self.value_type_expr(&val);
                let val_id = self.arena.add(val, elem_span);
                vals.push(val_id);
                tys.push(ty);
                self.tuple_elems(tail, vals, tys, span).await
            }
        }
    }

    /// Evaluate tuple index access: `tuple.0`, `tuple.1`, etc.
    #[async_recursion]
    async fn tuple_index(
        &mut self,
        base: ExprId,
        idx: u32,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;

        match &base_val {
            Value::Tuple(_, elems) => elems
                .get(idx as usize)
                .and_then(|id| self.arena.get(*id).cloned())
                .ok_or_else(|| {
                    Error::runtime(
                        span,
                        format!(
                            "tuple index `{idx}` out of bounds; tuple has {} element(s)",
                            elems.len()
                        ),
                    )
                }),
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot index `{}` with `.{idx}`; expected tuple",
                    base_val.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Get the type expression for a runtime value.
    fn value_type_expr(&mut self, v: &Value) -> TypeExprId {
        match v {
            Value::Bool(_) => self.type_exprs.named(TypeId::BOOL),
            Value::Int(_) => self.type_exprs.named(TypeId::INT),
            Value::Float(_) => self.type_exprs.named(TypeId::FLOAT),
            Value::Char(_) => self.type_exprs.named(TypeId::CHAR),
            Value::String(_) => self.type_exprs.named(TypeId::STRING),
            Value::Array(elem_ty, _) => {
                // Array[elem_ty]
                self.type_exprs
                    .app(TypeId::ARRAY, smallvec::smallvec![*elem_ty])
            }
            Value::Object(_) => self.type_exprs.named(TypeId::OBJECT),
            Value::Tuple(ty, _) => *ty,
            Value::Tagged(ty_expr, _, _) => *ty_expr,
            Value::Closure { params, ret, .. }
            | Value::Function { params, ret, .. } => {
                // Build function type from params and return type
                let param_tys: SmallVec<[TypeExprId; 4]> = params
                    .iter()
                    .map(|(_, ty)| {
                        ty.unwrap_or_else(|| {
                            self.type_exprs.named(TypeId::UNKNOWN)
                        })
                    })
                    .collect();
                let ret_ty = ret
                    .unwrap_or_else(|| self.type_exprs.named(TypeId::UNKNOWN));
                self.type_exprs.fn_type(param_tys, ret_ty)
            }
        }
    }

    /// Get a human-readable name for a type expression (for error messages).
    fn type_expr_name(&self, id: TypeExprId) -> String {
        self.type_exprs
            .format(id, |ty| {
                self.registry
                    .type_name(ty, &self.arena)
                    .unwrap_or("?")
                    .to_owned()
            })
            .unwrap_or_else(|| "?".to_owned())
    }

    /// Resolve an AST type expression to a runtime `TypeExprId`.
    ///
    /// Looks up type names in the registry and builds the runtime type.
    fn resolve_type_expr(
        &mut self,
        ast_id: AstTypeExprId,
        span: Span,
    ) -> Result<TypeExprId> {
        let ast_ty =
            self.ast.get_type_expr(ast_id).cloned().ok_or_else(|| {
                Error::runtime(span, "invalid type expression id")
            })?;

        match ast_ty {
            AstTypeExpr::Named(name) => {
                let name_id = self.arena.intern(&name);
                let ty_id = self.registry.lookup(name_id).ok_or_else(|| {
                    Error::type_err(span, format!("unknown type: {name}"))
                })?;
                Ok(self.type_exprs.named(ty_id))
            }
            AstTypeExpr::App(name, params) => {
                let name_id = self.arena.intern(&name);
                let ty_id = self.registry.lookup(name_id).ok_or_else(|| {
                    Error::type_err(span, format!("unknown type: {name}"))
                })?;
                // Recursively resolve type parameters
                let resolved: Result<SmallVec<[TypeExprId; 2]>> = params
                    .iter()
                    .map(|&p| self.resolve_type_expr(p, span))
                    .collect();
                Ok(self.type_exprs.app(ty_id, resolved?))
            }
            AstTypeExpr::Fn(params, ret) => {
                // Recursively resolve param types
                let resolved_params: Result<SmallVec<[TypeExprId; 4]>> = params
                    .iter()
                    .map(|&p| self.resolve_type_expr(p, span))
                    .collect();
                // Resolve return type
                let resolved_ret = self.resolve_type_expr(ret, span)?;
                Ok(self.type_exprs.fn_type(resolved_params?, resolved_ret))
            }
            AstTypeExpr::Tuple(elems) => {
                // Recursively resolve element types
                let resolved: Result<SmallVec<[TypeExprId; 4]>> = elems
                    .iter()
                    .map(|&e| self.resolve_type_expr(e, span))
                    .collect();
                Ok(self.type_exprs.tuple(resolved?))
            }
        }
    }

    /// Create an `Option.None` value with unknown type parameter.
    fn make_none(&mut self) -> Value {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![unknown]);
        Value::none(opt_ty)
    }

    /// Create an `Option.Some(v)` value, inferring the type from the inner value.
    fn make_some(&mut self, inner: ValueId) -> Value {
        let inner_val = self.arena.get(inner).cloned().unwrap_or(Value::Int(0));
        let inner_ty = self.value_type_expr(&inner_val);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![inner_ty]);
        Value::some(opt_ty, inner)
    }

    /// Create an `Option.None` with the same type parameter as another Option.
    fn make_none_like(&mut self, other_opt_ty: TypeExprId) -> Value {
        Value::none(other_opt_ty)
    }

    /// Evaluate index access (array or object).
    #[async_recursion]
    async fn index(
        &mut self,
        base: ExprId,
        idx: ExprId,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;
        let idx_val = self.eval(idx).await?;

        match (&base_val, &idx_val) {
            (Value::Array(_, elems), Value::Int(i)) => {
                let index = if *i < 0 {
                    // Negative indexing from end
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| elems.get(idx))
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("array index {i} out of bounds"),
                        )
                    })
            }
            (Value::Object(obj), Value::String(key)) => obj
                .get(key)
                .and_then(|id| self.arena.get(*id).cloned())
                .ok_or_else(|| {
                    let key_str = self.arena.get_str(*key).unwrap_or("?");
                    Error::runtime(span, format!("key `{key_str}` not found"))
                }),
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot index {} with {}",
                    base_val.type_name(&self.registry, &self.type_exprs),
                    idx_val.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Evaluate a resolved namespace path: `Type.Variant` for zero-arity variants.
    ///
    /// Created by the name resolution pass from `Expr::Field(Var(type), variant)`.
    /// Currently only handles paths of length 2 (type + variant).
    fn path(&mut self, segments: &[String], span: Span) -> Result<Value> {
        match segments {
            [ty_name, var_name] => {
                let ty_id = self.arena.intern(ty_name);
                let var_id = self.arena.intern(var_name);

                let type_id = self.registry.lookup(ty_id).ok_or_else(|| {
                    Error::runtime(span, format!("unknown type `{ty_name}`"))
                })?;

                let v = self
                    .registry
                    .lookup_variant(type_id, var_id)
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!(
                                "type `{ty_name}` has no variant `{var_name}`"
                            ),
                        )
                    })?;

                let idx = v.idx;
                let ty_expr = self.build_variant_type_expr(type_id, idx, &[]);
                Ok(Value::Tagged(ty_expr, idx, smallvec::SmallVec::new()))
            }
            _ => Err(Error::runtime(
                span,
                format!("unsupported path length: {}", segments.len()),
            )),
        }
    }

    /// Evaluate field access on an object value.
    ///
    /// After name resolution, this method is purely for runtime field access
    /// on `Value::Object`. Zero-arity variants like `Option.None` are handled
    /// by `Expr::Path` (resolved at parse time).
    #[async_recursion]
    async fn field(
        &mut self,
        base: ExprId,
        field: &str,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;

        match &base_val {
            Value::Object(obj) => {
                let field_id = self.arena.intern(field);
                obj.get(&field_id)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("field `{field}` not found"),
                        )
                    })
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot access field on {}",
                    base_val.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Evaluate optional field access: `expr?.field`.
    ///
    /// - If base is `Option.None`, returns `Option.None`
    /// - If base is `Option.Some(v)`, accesses field on `v`, wraps in `Some`
    /// - If base is any other value, accesses field normally, wraps in `Some`
    #[async_recursion]
    async fn optional_field(
        &mut self,
        base: ExprId,
        field: &str,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;

        match &base_val {
            // Option.None -> Option.None (short-circuit)
            Value::Tagged(ty_expr, 0, _)
                if self
                    .type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                Ok(self.make_none_like(*ty_expr))
            }
            // Option.Some(v) -> access field on v, wrap in Some
            Value::Tagged(ty_expr, 1, payload)
                if self
                    .type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                let inner = payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Option.Some missing payload")
                    })?;
                let result = self.field_access(&inner, field, span)?;
                let result_id = self.arena.add(result, span);
                Ok(self.make_some(result_id))
            }
            // Non-Option value -> access field normally, wrap in Some
            other => {
                let result = self.field_access(other, field, span)?;
                let result_id = self.arena.add(result, span);
                Ok(self.make_some(result_id))
            }
        }
    }

    /// Evaluate a type check: `expr is Pattern`.
    ///
    /// Returns `true` if the value matches the pattern, `false` otherwise.
    /// For `VariantBind` patterns, bindings are NOT created here; they are
    /// handled specially by `if_with_bindings` when used as an `IF` condition.
    #[async_recursion]
    async fn is(
        &mut self,
        expr: ExprId,
        pattern: &TypePattern,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        let matched = self.check_pattern(&val, pattern, span)?;
        Ok(Value::Bool(matched))
    }

    /// Evaluate a type cast: `expr as Type`.
    ///
    /// Infallible conversions:
    /// - `Int -> Float` (widen)
    /// - `Float -> Int` (truncate)
    /// - `T -> String` (stringify)
    /// - `Bool -> Int` (`false` -> `0`, `true` -> `1`)
    #[async_recursion]
    async fn r#as(
        &mut self,
        expr: ExprId,
        ast_ty: AstTypeExprId,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        let target_ty = self.resolve_type_expr(ast_ty, span)?;
        let target_base =
            self.type_exprs.base_type(target_ty).ok_or_else(|| {
                Error::runtime(span, "invalid target type in cast")
            })?;

        self.coerce(&val, target_base, span)
    }

    /// Perform type coercion for `as` casts.
    fn coerce(
        &mut self,
        val: &Value,
        target: TypeId,
        span: Span,
    ) -> Result<Value> {
        match (val, target) {
            // Identity casts
            (Value::Int(_), TypeId::INT)
            | (Value::Float(_), TypeId::FLOAT)
            | (Value::Bool(_), TypeId::BOOL)
            | (Value::Char(_), TypeId::CHAR)
            | (Value::String(_), TypeId::STRING) => Ok(val.clone()),

            // Int -> Float (widen)
            (Value::Int(n), TypeId::FLOAT) => {
                Ok(Value::Float(OrderedFloat(*n as f64)))
            }

            // Float -> Int (truncate)
            (Value::Float(f), TypeId::INT) => Ok(Value::Int(f.0 as i64)),

            // Bool -> Int
            (Value::Bool(b), TypeId::INT) => {
                Ok(Value::Int(if *b { 1 } else { 0 }))
            }

            // T -> String (stringify anything)
            (_, TypeId::STRING) => {
                let s = self.stringify(val);
                let id = self.arena.intern(&s);
                Ok(Value::String(id))
            }

            // Unsupported conversion
            _ => {
                let src_name = val.type_name(&self.registry, &self.type_exprs);
                let tgt_name = self
                    .registry
                    .type_name(target, &self.arena)
                    .unwrap_or("Unknown");
                Err(Error::type_err(
                    span,
                    format!("cannot cast {src_name} as {tgt_name}"),
                ))
            }
        }
    }

    /// Evaluate a fallible conversion: `expr read Type`.
    ///
    /// Returns `Result[T, String]` (as a RUMPS value), NOT `Err(crate::Error)`.
    /// Conversions:
    /// - `String -> Int`: parse, `Result.Err` if invalid
    /// - `String -> Float`: parse, `Result.Err` if invalid
    /// - `Int -> Bool`: `0`/`1` only, else `Result.Err`
    #[async_recursion]
    async fn read(
        &mut self,
        expr: ExprId,
        ast_ty: AstTypeExprId,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        let target_ty = self.resolve_type_expr(ast_ty, span)?;
        let target_base =
            self.type_exprs.base_type(target_ty).ok_or_else(|| {
                Error::runtime(span, "invalid target type in read")
            })?;

        self.try_convert(&val, target_base, span)
    }

    /// Perform fallible type conversion for `read`.
    ///
    /// Returns a RUMPS `Result[T, String]` value.
    fn try_convert(
        &mut self,
        val: &Value,
        target: TypeId,
        span: Span,
    ) -> Result<Value> {
        match (val, target) {
            // String -> Int
            (Value::String(sid), TypeId::INT) => {
                let s = self
                    .arena
                    .get_str(*sid)
                    .map(str::to_owned)
                    .unwrap_or_default();
                match s.parse::<i64>() {
                    Ok(n) => Ok(self.make_result_ok(Value::Int(n), span)),
                    Err(_) => {
                        let msg = format!("invalid integer: {s}");
                        Ok(self.make_result_err(&msg, span))
                    }
                }
            }

            // String -> Float
            (Value::String(sid), TypeId::FLOAT) => {
                let s = self
                    .arena
                    .get_str(*sid)
                    .map(str::to_owned)
                    .unwrap_or_default();
                match s.parse::<f64>() {
                    Ok(n) => Ok(self
                        .make_result_ok(Value::Float(OrderedFloat(n)), span)),
                    Err(_) => {
                        let msg = format!("invalid float: {s}");
                        Ok(self.make_result_err(&msg, span))
                    }
                }
            }

            // Int -> Bool (strict: only 0 and 1)
            (Value::Int(n), TypeId::BOOL) => match *n {
                0 => Ok(self.make_result_ok(Value::Bool(false), span)),
                1 => Ok(self.make_result_ok(Value::Bool(true), span)),
                _ => {
                    let msg = format!("expected 0 or 1 for Bool, got {n}");
                    Ok(self.make_result_err(&msg, span))
                }
            },

            // Unsupported conversion
            _ => {
                let src_name = val.type_name(&self.registry, &self.type_exprs);
                let tgt_name = self
                    .registry
                    .type_name(target, &self.arena)
                    .unwrap_or("Unknown");
                Err(Error::type_err(
                    span,
                    format!("cannot read {src_name} as {tgt_name}"),
                ))
            }
        }
    }

    /// Create a `Result.Ok(v)` value.
    fn make_result_ok(&mut self, v: Value, span: Span) -> Value {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let val_ty = self.value_type_expr(&v);
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![val_ty, unknown]);
        let val_id = self.arena.add(v, span);
        Value::ok(res_ty, val_id)
    }

    /// Create a `Result.Err(msg)` value.
    fn make_result_err(&mut self, msg: &str, span: Span) -> Value {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let str_ty = self.type_exprs.named(TypeId::STRING);
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![unknown, str_ty]);
        let msg_id = self.arena.intern(msg);
        let msg_val = self.arena.add(Value::String(msg_id), span);
        Value::err(res_ty, msg_val)
    }

    /// Check if a value matches a type pattern (without binding).
    fn check_pattern(
        &self,
        val: &Value,
        pattern: &TypePattern,
        span: Span,
    ) -> Result<bool> {
        match pattern {
            TypePattern::Type(ty_name) => {
                // Simple type check: `is Int`, `is String`, etc.
                let ty_id = self.arena.lookup_string(ty_name);
                let type_id = ty_id.and_then(|id| self.registry.lookup(id));
                type_id
                    .map(|tid| self.value_matches_type(val, tid))
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("unknown type `{ty_name}`"),
                        )
                    })
            }
            TypePattern::Variant(ty_name, var_name) => {
                // Variant check (zero-arity only): `is Option.None`
                // Variants with payloads must use `(_)` or `(name)`
                self.check_variant_zero_arity(val, ty_name, var_name, span)
            }
            TypePattern::VariantWildcard(ty_name, var_name) => {
                // Variant check ignoring payload: `is Option.Some(_)`
                self.check_variant(val, ty_name, var_name, span)
            }
            TypePattern::VariantBind(ty_name, var_name, _) => {
                // Variant check (bindings handled elsewhere): `is Option.Some(val)`
                self.check_variant(val, ty_name, var_name, span)
            }
        }
    }

    /// Check variant match, requiring zero-arity.
    ///
    /// Used for `is Type.Variant` without parens; variants with payloads
    /// must use `is Type.Variant(_)` or `is Type.Variant(name)`.
    fn check_variant_zero_arity(
        &self,
        val: &Value,
        ty_name: &str,
        var_name: &str,
        span: Span,
    ) -> Result<bool> {
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        // Enforce zero-arity for bare variant patterns
        (var_def.arity == 0).then_some(()).ok_or_else(|| {
            Error::runtime(
                span,
                format!(
                    "`{ty_name}.{var_name}` has {} payload(s); use `{ty_name}.{var_name}(_)` or bind with `{ty_name}.{var_name}(name)`",
                    var_def.arity
                ),
            )
        })?;

        Ok(match val {
            Value::Tagged(ty_expr, idx, _) => {
                self.type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == type_id)
                    && *idx == var_def.idx
            }
            _ => false,
        })
    }

    /// Check if a value is a Tagged variant matching the given type and variant.
    fn check_variant(
        &self,
        val: &Value,
        ty_name: &str,
        var_name: &str,
        span: Span,
    ) -> Result<bool> {
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        Ok(match val {
            Value::Tagged(ty_expr, idx, _) => {
                self.type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == type_id)
                    && *idx == var_def.idx
            }
            _ => false,
        })
    }

    /// Look up a type and variant, returning their IDs.
    fn lookup_variant(
        &self,
        ty_name: &str,
        var_name: &str,
        span: Span,
    ) -> Result<(TypeId, crate::value::VariantDef)> {
        let ty_id = self.arena.lookup_string(ty_name);
        let var_id = self.arena.lookup_string(var_name);

        let type_id = ty_id
            .and_then(|id| self.registry.lookup(id))
            .ok_or_else(|| {
                Error::runtime(span, format!("unknown type `{ty_name}`"))
            })?;

        let var_def = var_id
            .and_then(|id| self.registry.lookup_variant(type_id, id))
            .cloned()
            .ok_or_else(|| {
                Error::runtime(
                    span,
                    format!("unknown variant `{ty_name}.{var_name}`"),
                )
            })?;

        Ok((type_id, var_def))
    }

    /// Check if a value matches a simple type (non-variant).
    fn value_matches_type(&self, val: &Value, type_id: TypeId) -> bool {
        match val {
            Value::Bool(_) => type_id == TypeId::BOOL,
            Value::Int(_) => type_id == TypeId::INT,
            Value::Float(_) => type_id == TypeId::FLOAT,
            Value::Char(_) => type_id == TypeId::CHAR,
            Value::String(_) => type_id == TypeId::STRING,
            Value::Array(_, _) => type_id == TypeId::ARRAY,
            Value::Object(_) => type_id == TypeId::OBJECT,
            Value::Tuple(_, _) => type_id == TypeId::TUPLE,
            Value::Tagged(ty_expr, _, _) => self
                .type_exprs
                .base_type(*ty_expr)
                .is_some_and(|t| t == type_id),
            // Closures and functions don't have a simple TypeId; use function type expressions
            Value::Closure { .. } | Value::Function { .. } => false,
        }
    }

    /// Check if a value matches a type expression.
    ///
    /// For simple types, delegates to `value_matches_type`.
    /// For function types, checks arity and param/return type compatibility.
    fn value_matches_type_expr(&self, val: &Value, ty: TypeExprId) -> bool {
        // Try simple type first
        self.type_exprs.base_type(ty).map_or_else(
            || {
                // Function type: check if value is a function/closure with matching signature
                self.type_exprs.fn_parts(ty).is_some_and(|(params, ret)| {
                    self.fn_value_matches(val, params, ret)
                })
            },
            |type_id| self.value_matches_type(val, type_id),
        )
    }

    /// Check if a function/closure value matches a function type.
    fn fn_value_matches(
        &self,
        val: &Value,
        expected_params: &[TypeExprId],
        expected_ret: TypeExprId,
    ) -> bool {
        match val {
            Value::Closure { params, ret, .. }
            | Value::Function { params, ret, .. } => {
                // Check arity
                params.len() == expected_params.len()
                    // Check param types (if annotated)
                    && params.iter().zip(expected_params.iter()).all(
                        |((_, actual_ty), expected_ty)| {
                            actual_ty.is_none_or(|a| self.type_exprs.eq(a, *expected_ty))
                        },
                    )
                    // Check return type (if annotated)
                    && ret.is_none_or(|r| self.type_exprs.eq(r, expected_ret))
            }
            _ => false,
        }
    }

    /// Format a type expression for error messages.
    fn format_type_expr(&self, ty: TypeExprId) -> String {
        self.type_exprs
            .format(ty, |tid| {
                self.registry
                    .type_name(tid, &self.arena)
                    .unwrap_or("?")
                    .to_owned()
            })
            .unwrap_or_else(|| "?".to_owned())
    }

    /// Helper for field access on a value (without wrapping in Option).
    fn field_access(
        &mut self,
        val: &Value,
        field: &str,
        span: Span,
    ) -> Result<Value> {
        match val {
            Value::Object(obj) => {
                let field_id = self.arena.intern(field);
                obj.get(&field_id)
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("field `{field}` not found"),
                        )
                    })
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot access field on {}",
                    val.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Evaluate a variant constructor: `Type.Variant(args...)`.
    #[async_recursion]
    async fn variant(
        &mut self,
        ty_name: &str,
        var_name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        let ty_id = self.arena.intern(ty_name);
        let var_id = self.arena.intern(var_name);

        let type_id = self.registry.lookup(ty_id).ok_or_else(|| {
            Error::runtime(span, format!("unknown type `{ty_name}`"))
        })?;

        let var_def = self
            .registry
            .lookup_variant(type_id, var_id)
            .ok_or_else(|| {
                Error::runtime(
                    span,
                    format!("unknown variant `{ty_name}.{var_name}`"),
                )
            })?;

        // Validate arity
        let expected = var_def.arity as usize;
        let got = args.len();
        (expected == got).then_some(()).ok_or_else(|| {
            Error::runtime(
                span,
                format!(
                    "`{ty_name}.{var_name}` expects {expected} argument(s), got {got}"
                ),
            )
        })?;

        let idx = var_def.idx;

        // Evaluate arguments and collect their values and types
        let (payloads, payload_types) =
            self.eval_variant_args(args, span).await?;

        // Build the type expression with inferred type parameters
        let ty_expr =
            self.build_variant_type_expr(type_id, idx, &payload_types);

        Ok(Value::Tagged(ty_expr, idx, payloads))
    }

    /// Evaluate variant arguments and return (values, types).
    #[async_recursion]
    async fn eval_variant_args(
        &mut self,
        args: &[ExprId],
        span: Span,
    ) -> Result<(SmallVec<[ValueId; 4]>, SmallVec<[TypeExprId; 4]>)> {
        match args.split_first() {
            None => Ok((SmallVec::new(), SmallVec::new())),
            Some((head, tail)) => {
                let val = self.eval(*head).await?;
                let val_ty = self.value_type_expr(&val);
                let val_id = self.arena.add(val, span);
                let (mut rest_vals, mut rest_tys) =
                    self.eval_variant_args(tail, span).await?;
                // Prepend since we're building from head
                let mut vals = smallvec::smallvec![val_id];
                vals.append(&mut rest_vals);
                let mut tys = smallvec::smallvec![val_ty];
                tys.append(&mut rest_tys);
                Ok((vals, tys))
            }
        }
    }

    /// Build a `TypeExprId` for a variant, inferring type params from payloads.
    ///
    /// For `Option.Some(42)` → `Option[Int]`
    /// For `Option.None` → `Option[Unknown]`
    /// For `Result.Ok(42)` → `Result[Int, Unknown]`
    /// For `Result.Err("x")` → `Result[Unknown, String]`
    fn build_variant_type_expr(
        &mut self,
        type_id: TypeId,
        var_idx: u8,
        payload_types: &[TypeExprId],
    ) -> TypeExprId {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);

        // Special handling for built-in types Option and Result
        if type_id == TypeId::OPTION {
            // Option[T]: None has no payload, Some has T
            let t = payload_types.first().copied().unwrap_or(unknown);
            self.type_exprs.app(TypeId::OPTION, smallvec::smallvec![t])
        } else if type_id == TypeId::RESULT {
            // Result[T, E]: Ok has T, Err has E
            let (t, e) = if var_idx == 0 {
                // Ok(v) → Result[type_of(v), Unknown]
                (payload_types.first().copied().unwrap_or(unknown), unknown)
            } else {
                // Err(e) → Result[Unknown, type_of(e)]
                (unknown, payload_types.first().copied().unwrap_or(unknown))
            };
            self.type_exprs
                .app(TypeId::RESULT, smallvec::smallvec![t, e])
        } else {
            // For other types, use Unknown for all type params
            // (Future: read type_params from TypeDef and infer properly)
            self.type_exprs.named(type_id)
        }
    }

    /// Evaluate a block expression.
    ///
    /// Executes statements, then evaluates the trailing expression (if any).
    /// Returns `Option.None` if no trailing expression.
    #[async_recursion]
    async fn block(
        &mut self,
        stmts: &[StmtId],
        tail: Option<ExprId>,
    ) -> Result<Value> {
        self.env.scopes.push();
        let result = self.block_inner(stmts, tail).await;
        self.env.scopes.pop();
        result
    }

    /// Inner helper for block expression evaluation.
    #[async_recursion]
    async fn block_inner(
        &mut self,
        stmts: &[StmtId],
        tail: Option<ExprId>,
    ) -> Result<Value> {
        match stmts.split_first() {
            None => match tail {
                Some(e) => self.eval(e).await,
                None => Ok(self.make_none()),
            },
            Some((head, rest)) => {
                self.exec(*head).await?;
                self.block_inner(rest, tail).await
            }
        }
    }

    /// Evaluate an `IF` expression.
    ///
    /// Returns the value of the taken branch. If no else branch and condition
    /// is false, returns `Option.None`.
    ///
    /// Special handling for `is` conditions with bindings: if the condition is
    /// `expr is Pattern(bindings)`, the bindings are only visible in the then
    /// branch, not in the else branch.
    #[async_recursion]
    async fn r#if(
        &mut self,
        cond: ExprId,
        then_br: ExprId,
        else_br: Option<ExprId>,
    ) -> Result<Value> {
        // Check if condition is `Expr::Is` with bindings
        let cond_expr = self.ast.get_expr(cond).cloned();
        match cond_expr {
            Some(Expr::Is(expr, TypePattern::VariantBind(ty, var, names))) => {
                self.if_with_bindings(expr, &ty, &var, &names, then_br, else_br)
                    .await
            }
            _ => {
                let cond_val = self.eval(cond).await?;
                if cond_val.is_truthy(&self.arena, &self.type_exprs) {
                    self.eval(then_br).await
                } else {
                    match else_br {
                        Some(e) => self.eval(e).await,
                        None => Ok(self.make_none()),
                    }
                }
            }
        }
    }

    /// Handle `IF expr is Type.Variant(bindings) { then } ELSE { else }`.
    ///
    /// Bindings are only visible in the then branch.
    #[async_recursion]
    async fn if_with_bindings(
        &mut self,
        expr: ExprId,
        ty_name: &str,
        var_name: &str,
        names: &[String],
        then_br: ExprId,
        else_br: Option<ExprId>,
    ) -> Result<Value> {
        let span = self.ast.expr_span(expr).unwrap_or_default();
        let val = self.eval(expr).await?;

        // Check if the value matches the variant
        let matched = self.check_variant(&val, ty_name, var_name, span)?;

        if matched {
            // Extract payloads and bind them
            let payloads = match &val {
                Value::Tagged(_, _, p) => p.clone(),
                _ => SmallVec::new(),
            };

            // Validate arity
            (payloads.len() == names.len()).then_some(()).ok_or_else(|| {
                Error::runtime(
                    span,
                    format!(
                        "`{ty_name}.{var_name}` has {} payload(s), but {} binding(s) provided",
                        payloads.len(),
                        names.len()
                    ),
                )
            })?;

            // Push scope, bind, evaluate, pop
            self.env.scopes.push();
            self.bind_payloads(names, &payloads, span);
            let result = self.eval(then_br).await;
            self.env.scopes.pop();
            result
        } else {
            // No match; evaluate else branch (without bindings)
            match else_br {
                Some(e) => self.eval(e).await,
                None => Ok(self.make_none()),
            }
        }
    }

    /// Bind payload values to names in the current scope.
    fn bind_payloads(
        &mut self,
        names: &[String],
        payloads: &[ValueId],
        span: Span,
    ) {
        // Fallback value if arena lookup fails (shouldn't happen normally)
        let fallback = self.make_none();
        names
            .iter()
            .zip(payloads.iter())
            .for_each(|(name, &val_id)| {
                let name_id = self.arena.intern(name);
                // Re-add the value to get a fresh ValueId in case it matters
                let val =
                    self.arena.get(val_id).cloned().unwrap_or(fallback.clone());
                let new_val_id = self.arena.add(val, span);
                self.env.scopes.bind(name_id, new_val_id);
            });
    }

    /// Execute a `LET` binding.
    ///
    /// If a type annotation is present, validates that the value's type matches.
    #[async_recursion]
    async fn r#let(
        &mut self,
        name: &str,
        ty_ann: Option<AstTypeExprId>,
        expr_id: ExprId,
        span: Span,
    ) -> Result<()> {
        let val = self.eval(expr_id).await?;

        // Check type annotation if present
        if let Some(ast_ty_id) = ty_ann {
            let expected_ty = self.resolve_type_expr(ast_ty_id, span)?;
            let actual_ty = self.value_type_expr(&val);

            if !self.type_exprs.eq(expected_ty, actual_ty) {
                let expected = self.type_expr_name(expected_ty);
                let actual = self.type_expr_name(actual_ty);
                return Err(Error::type_err(
                    span,
                    format!("type mismatch: expected {expected}, got {actual}"),
                ));
            }
        }

        let name_id = self.arena.intern(name);
        let val_id = self.arena.add(val, span);
        self.env.scopes.bind(name_id, val_id);
        Ok(())
    }

    /// Execute an `OUTPUT` statement.
    ///
    /// Writes to stdout via the I/O context.
    ///
    /// TODO Add more targets; stderr, file, etc...
    #[async_recursion]
    async fn output(&mut self, expr_id: ExprId) -> Result<()> {
        let span = self.ast.expr_span(expr_id).unwrap_or_default();
        let val = self.eval(expr_id).await?;
        let s = self.display(&val);
        self.io.stdout(&s, span).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::io::TestIo;

    /// Create a test interpreter with an in-memory database.
    ///
    /// Uses `with_arena` since tests build ASTs directly (no parsing/resolution).
    fn test_interp(ast: &Ast) -> Interpreter<'_, TestIo> {
        let db = Database::in_memory().expect("in-memory db");
        let mut arena = ValueArena::new();
        let registry = TypeRegistry::new(&mut arena).expect("registry");
        Interpreter::with_arena(ast, db, TestIo::new(), arena, registry)
    }

    /// Build a simple AST with a single expression.
    fn ast_with_expr(expr: Expr) -> (Ast, ExprId) {
        let mut ast = Ast::new();
        let id = ast.add_expr(expr, Span::new(0, 10));
        (ast, id)
    }

    #[tokio::test]
    async fn eval_int_literal() {
        let (ast, id) = ast_with_expr(Expr::Literal(Literal::Int(42)));
        let mut interp = test_interp(&ast);
        let result = interp.eval(id).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn eval_float_literal() {
        let (ast, id) = ast_with_expr(Expr::Literal(Literal::Float(3.14)));
        let mut interp = test_interp(&ast);
        let result = interp.eval(id).await.unwrap();
        assert_eq!(result, Value::Float(OrderedFloat(3.14)));
    }

    #[tokio::test]
    async fn eval_bool_literal() {
        let (ast, id) = ast_with_expr(Expr::Literal(Literal::Bool(true)));
        let mut interp = test_interp(&ast);
        let result = interp.eval(id).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_string_literal() {
        let (ast, id) =
            ast_with_expr(Expr::Literal(Literal::String("hello".into())));
        let mut interp = test_interp(&ast);
        let result = interp.eval(id).await.unwrap();
        match result {
            Value::String(id) => {
                assert_eq!(interp.arena.get_str(id), Some("hello"));
            }
            _ => panic!("expected string"),
        }
    }

    #[tokio::test]
    async fn eval_add_int() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Int(20)), Span::new(5, 7));
        let add =
            ast.add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 7));

        let mut interp = test_interp(&ast);
        let result = interp.eval(add).await.unwrap();
        assert_eq!(result, Value::Int(30));
    }

    #[tokio::test]
    async fn eval_add_float() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Float(1.5)), Span::new(0, 3));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Float(2.5)), Span::new(6, 9));
        let add =
            ast.add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 9));

        let mut interp = test_interp(&ast);
        let result = interp.eval(add).await.unwrap();
        assert_eq!(result, Value::Float(OrderedFloat(4.0)));
    }

    #[tokio::test]
    async fn eval_add_mixed_coercion() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Float(2.5)), Span::new(5, 8));
        let add =
            ast.add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 8));

        let mut interp = test_interp(&ast);
        let result = interp.eval(add).await.unwrap();
        assert_eq!(result, Value::Float(OrderedFloat(12.5)));
    }

    #[tokio::test]
    async fn eval_sub() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(50)), Span::new(0, 2));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Int(30)), Span::new(5, 7));
        let sub =
            ast.add_expr(Expr::Binary(lhs, BinOp::Sub, rhs), Span::new(0, 7));

        let mut interp = test_interp(&ast);
        let result = interp.eval(sub).await.unwrap();
        assert_eq!(result, Value::Int(20));
    }

    #[tokio::test]
    async fn eval_mul() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(6)), Span::new(0, 1));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(7)), Span::new(4, 5));
        let mul =
            ast.add_expr(Expr::Binary(lhs, BinOp::Mul, rhs), Span::new(0, 5));

        let mut interp = test_interp(&ast);
        let result = interp.eval(mul).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn eval_div() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(4)), Span::new(5, 6));
        let div =
            ast.add_expr(Expr::Binary(lhs, BinOp::Div, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(div).await.unwrap();
        assert_eq!(result, Value::Float(OrderedFloat(2.5)));
    }

    #[tokio::test]
    async fn eval_div_by_zero() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(5, 6));
        let div =
            ast.add_expr(Expr::Binary(lhs, BinOp::Div, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(div).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn eval_floor_div() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(3)), Span::new(5, 6));
        let div = ast
            .add_expr(Expr::Binary(lhs, BinOp::FloorDiv, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(div).await.unwrap();
        assert_eq!(result, Value::Int(3));
    }

    #[tokio::test]
    async fn eval_mod() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(3)), Span::new(5, 6));
        let m =
            ast.add_expr(Expr::Binary(lhs, BinOp::Mod, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(m).await.unwrap();
        assert_eq!(result, Value::Int(1));
    }

    #[tokio::test]
    async fn eval_pow_int() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(0, 1));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(5, 7));
        let pow =
            ast.add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 7));

        let mut interp = test_interp(&ast);
        let result = interp.eval(pow).await.unwrap();
        assert_eq!(result, Value::Int(1024));
    }

    #[tokio::test]
    async fn eval_pow_float() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Float(2.0)), Span::new(0, 3));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Float(0.5)), Span::new(7, 10));
        let pow =
            ast.add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 10));

        let mut interp = test_interp(&ast);
        let result = interp.eval(pow).await.unwrap();
        // 2.0 ** 0.5 = sqrt(2) ≈ 1.41421...
        match result {
            Value::Float(f) => assert!((f.0 - 1.41421356).abs() < 0.0001),
            _ => panic!("expected Float"),
        }
    }

    #[tokio::test]
    async fn eval_pow_negative_exp() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(0, 1));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Int(-1)), Span::new(5, 7));
        let pow =
            ast.add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 7));

        let mut interp = test_interp(&ast);
        let result = interp.eval(pow).await.unwrap();
        // 2 ** -1 = 0.5
        assert_eq!(result, Value::Float(OrderedFloat(0.5)));
    }

    #[tokio::test]
    async fn eval_pow_mixed() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(4)), Span::new(0, 1));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Float(0.5)), Span::new(5, 8));
        let pow =
            ast.add_expr(Expr::Binary(lhs, BinOp::Pow, rhs), Span::new(0, 8));

        let mut interp = test_interp(&ast);
        let result = interp.eval(pow).await.unwrap();
        // 4 ** 0.5 = 2.0
        assert_eq!(result, Value::Float(OrderedFloat(2.0)));
    }

    #[tokio::test]
    async fn eval_eq() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(5, 7));
        let eq =
            ast.add_expr(Expr::Binary(lhs, BinOp::Eq, rhs), Span::new(0, 7));

        let mut interp = test_interp(&ast);
        let result = interp.eval(eq).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_eq_mixed_numeric() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Float(42.0)), Span::new(5, 9));
        let eq =
            ast.add_expr(Expr::Binary(lhs, BinOp::Eq, rhs), Span::new(0, 9));

        let mut interp = test_interp(&ast);
        let result = interp.eval(eq).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_ne() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(0, 1));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(5, 6));
        let ne =
            ast.add_expr(Expr::Binary(lhs, BinOp::Ne, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(ne).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_lt() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(4, 6));
        let lt =
            ast.add_expr(Expr::Binary(lhs, BinOp::Lt, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(lt).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_gt() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(5, 6));
        let gt =
            ast.add_expr(Expr::Binary(lhs, BinOp::Gt, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(gt).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_le() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(5, 6));
        let le =
            ast.add_expr(Expr::Binary(lhs, BinOp::Le, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(le).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_ge() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(0, 2));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(5, 6));
        let ge =
            ast.add_expr(Expr::Binary(lhs, BinOp::Ge, rhs), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(ge).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_and() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(0, 4));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(8, 13));
        let and =
            ast.add_expr(Expr::Binary(lhs, BinOp::And, rhs), Span::new(0, 13));

        let mut interp = test_interp(&ast);
        let result = interp.eval(and).await.unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[tokio::test]
    async fn eval_or() {
        let mut ast = Ast::new();
        let lhs =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(0, 5));
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(9, 13));
        let or =
            ast.add_expr(Expr::Binary(lhs, BinOp::Or, rhs), Span::new(0, 13));

        let mut interp = test_interp(&ast);
        let result = interp.eval(or).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn eval_concat() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(
            Expr::Literal(Literal::String("Hello".into())),
            Span::new(0, 7),
        );
        let rhs = ast.add_expr(
            Expr::Literal(Literal::String(" World".into())),
            Span::new(11, 19),
        );
        let cat = ast
            .add_expr(Expr::Binary(lhs, BinOp::Concat, rhs), Span::new(0, 19));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cat).await.unwrap();
        match result {
            Value::String(id) => {
                assert_eq!(interp.arena.get_str(id), Some("Hello World"));
            }
            _ => panic!("expected string"),
        }
    }

    #[tokio::test]
    async fn eval_concat_coercion() {
        let mut ast = Ast::new();
        let lhs = ast.add_expr(
            Expr::Literal(Literal::String("value: ".into())),
            Span::new(0, 9),
        );
        let rhs =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(13, 15));
        let cat = ast
            .add_expr(Expr::Binary(lhs, BinOp::Concat, rhs), Span::new(0, 15));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cat).await.unwrap();
        match result {
            Value::String(id) => {
                assert_eq!(interp.arena.get_str(id), Some("value: 42"));
            }
            _ => panic!("expected string"),
        }
    }

    #[tokio::test]
    async fn eval_neg_int() {
        let mut ast = Ast::new();
        let operand =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(1, 3));
        let neg =
            ast.add_expr(Expr::Unary(UnOp::Neg, operand), Span::new(0, 3));

        let mut interp = test_interp(&ast);
        let result = interp.eval(neg).await.unwrap();
        assert_eq!(result, Value::Int(-42));
    }

    #[tokio::test]
    async fn eval_neg_float() {
        let mut ast = Ast::new();
        let operand =
            ast.add_expr(Expr::Literal(Literal::Float(3.14)), Span::new(1, 5));
        let neg =
            ast.add_expr(Expr::Unary(UnOp::Neg, operand), Span::new(0, 5));

        let mut interp = test_interp(&ast);
        let result = interp.eval(neg).await.unwrap();
        assert_eq!(result, Value::Float(OrderedFloat(-3.14)));
    }

    #[tokio::test]
    async fn eval_not() {
        let mut ast = Ast::new();
        let operand =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(1, 5));
        let not =
            ast.add_expr(Expr::Unary(UnOp::Not, operand), Span::new(0, 5));

        let mut interp = test_interp(&ast);
        let result = interp.eval(not).await.unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[tokio::test]
    async fn let_and_lookup() {
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(100)), Span::new(8, 11));
        let let_stmt =
            ast.add_stmt(Stmt::Let("x".into(), None, val), Span::new(0, 11));
        let var = ast.add_expr(Expr::Var("x".into()), Span::new(0, 1));

        let mut interp = test_interp(&ast);
        interp.exec(let_stmt).await.unwrap();
        let result = interp.eval(var).await.unwrap();
        assert_eq!(result, Value::Int(100));
    }

    #[tokio::test]
    async fn let_shadowing() {
        let mut ast = Ast::new();

        // LET x = 10
        let val1 =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(8, 10));
        let let1 =
            ast.add_stmt(Stmt::Let("x".into(), None, val1), Span::new(0, 10));

        // Block expr with LET x = 20, returning x
        let val2 =
            ast.add_expr(Expr::Literal(Literal::Int(20)), Span::new(20, 22));
        let let2 =
            ast.add_stmt(Stmt::Let("x".into(), None, val2), Span::new(12, 22));
        let x_ref = ast.add_expr(Expr::Var("x".into()), Span::new(24, 25));
        let blk_expr = ast
            .add_expr(Expr::Block(vec![let2], Some(x_ref)), Span::new(10, 26));
        let blk_stmt = ast.add_stmt(Stmt::Expr(blk_expr), Span::new(10, 26));

        // Reference outer x
        let var = ast.add_expr(Expr::Var("x".into()), Span::new(28, 29));

        let mut interp = test_interp(&ast);
        interp.exec(let1).await.unwrap();
        interp.exec(blk_stmt).await.unwrap();
        // After block exits, x should be 10 again
        let result = interp.eval(var).await.unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[tokio::test]
    async fn array() {
        let mut ast = Ast::new();
        let e1 = ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2));
        let e2 = ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(4, 5));
        let e3 = ast.add_expr(Expr::Literal(Literal::Int(3)), Span::new(7, 8));
        let arr = ast.add_expr(Expr::Array(vec![e1, e2, e3]), Span::new(0, 9));

        let mut interp = test_interp(&ast);
        let result = interp.eval(arr).await.unwrap();
        match result {
            Value::Array(_, elems) => {
                assert_eq!(elems.len(), 3);
            }
            _ => panic!("expected array"),
        }
    }

    #[tokio::test]
    async fn object() {
        let mut ast = Ast::new();
        let v1 = ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8));
        let v2 = ast.add_expr(
            Expr::Literal(Literal::String("John".into())),
            Span::new(17, 23),
        );
        let obj = ast.add_expr(
            Expr::Object(vec![("id".into(), v1), ("name".into(), v2)]),
            Span::new(0, 25),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(obj).await.unwrap();
        match result {
            Value::Object(map) => {
                assert_eq!(map.len(), 2);
            }
            _ => panic!("expected object"),
        }
    }

    #[tokio::test]
    async fn index_array() {
        let mut ast = Ast::new();
        let e1 = ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(1, 3));
        let e2 = ast.add_expr(Expr::Literal(Literal::Int(20)), Span::new(5, 7));
        let arr = ast.add_expr(Expr::Array(vec![e1, e2]), Span::new(0, 8));
        let idx =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(9, 10));
        let access = ast.add_expr(Expr::Index(arr, idx), Span::new(0, 11));

        let mut interp = test_interp(&ast);
        let result = interp.eval(access).await.unwrap();
        assert_eq!(result, Value::Int(20));
    }

    #[tokio::test]
    async fn field_access() {
        let mut ast = Ast::new();
        let v = ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8));
        let obj =
            ast.add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(0, 10));
        let field =
            ast.add_expr(Expr::Field(obj, "x".into()), Span::new(0, 12));

        let mut interp = test_interp(&ast);
        let result = interp.eval(field).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn if_true() {
        let mut ast = Ast::new();

        // LET result = 0
        let zero =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(13, 14));
        let let_result = ast
            .add_stmt(Stmt::Let("result".into(), None, zero), Span::new(0, 14));

        // IF true { LET result = 1 }
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7));
        let one =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(25, 26));
        let set_one = ast
            .add_stmt(Stmt::Let("result".into(), None, one), Span::new(10, 26));
        let then_blk =
            ast.add_expr(Expr::Block(vec![set_one], None), Span::new(8, 28));
        let if_expr =
            ast.add_expr(Expr::If(cond, then_blk, None), Span::new(0, 28));
        let if_stmt = ast.add_stmt(Stmt::Expr(if_expr), Span::new(0, 28));

        let var = ast.add_expr(Expr::Var("result".into()), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        interp.exec(let_result).await.unwrap();
        interp.exec(if_stmt).await.unwrap();
        let result = interp.eval(var).await.unwrap();
        // In the block, we shadowed result. After block exit, it's 0 again.
        assert_eq!(result, Value::Int(0));
    }

    #[tokio::test]
    async fn if_else() {
        let mut ast = Ast::new();

        // IF false { } ELSE { LET x = 42 }
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(3, 8));
        let then_blk =
            ast.add_expr(Expr::Block(vec![], None), Span::new(9, 12));
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(30, 32));
        let let_x =
            ast.add_stmt(Stmt::Let("x".into(), None, val), Span::new(22, 32));
        let else_blk =
            ast.add_expr(Expr::Block(vec![let_x], None), Span::new(18, 35));
        let if_expr = ast.add_expr(
            Expr::If(cond, then_blk, Some(else_blk)),
            Span::new(0, 35),
        );
        let if_stmt = ast.add_stmt(Stmt::Expr(if_expr), Span::new(0, 35));

        // After IF, check x
        let var = ast.add_expr(Expr::Var("x".into()), Span::new(0, 1));

        let mut interp = test_interp(&ast);
        interp.exec(if_stmt).await.unwrap();
        // x was set in else block which exited, so x is not visible
        let result = interp.eval(var).await;
        assert!(result.is_err()); // x is not defined outside the block
    }

    #[tokio::test]
    async fn run_program() {
        let mut ast = Ast::new();

        // LET x = 10
        let v1 =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(8, 10));
        let let_x =
            ast.add_stmt(Stmt::Let("x".into(), None, v1), Span::new(0, 10));

        // LET y = 20
        let v2 =
            ast.add_expr(Expr::Literal(Literal::Int(20)), Span::new(20, 22));
        let let_y =
            ast.add_stmt(Stmt::Let("y".into(), None, v2), Span::new(12, 22));

        // LET sum = x + y
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(34, 35));
        let y = ast.add_expr(Expr::Var("y".into()), Span::new(38, 39));
        let add =
            ast.add_expr(Expr::Binary(x, BinOp::Add, y), Span::new(34, 39));
        let let_sum =
            ast.add_stmt(Stmt::Let("sum".into(), None, add), Span::new(24, 39));

        // Reference to check result (create before interpreter borrows ast)
        let sum_var = ast.add_expr(Expr::Var("sum".into()), Span::new(0, 3));

        let stmts = vec![let_x, let_y, let_sum];

        let interp = test_interp(&ast);
        let mut interp = interp.run(&stmts).await.unwrap();

        // Check sum
        let result = interp.eval(sum_var).await.unwrap();
        assert_eq!(result, Value::Int(30));
    }

    #[tokio::test]
    async fn if_expr_true_branch() {
        // IF true { 42 } ELSE { 0 }
        let mut ast = Ast::new();
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7));
        let then_val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(10, 12));
        let then_blk =
            ast.add_expr(Expr::Block(vec![], Some(then_val)), Span::new(9, 14));
        let else_val =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(22, 23));
        let else_blk = ast
            .add_expr(Expr::Block(vec![], Some(else_val)), Span::new(21, 25));
        let if_expr = ast.add_expr(
            Expr::If(cond, then_blk, Some(else_blk)),
            Span::new(0, 25),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(if_expr).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn if_expr_false_branch() {
        // IF false { 42 } ELSE { 0 }
        let mut ast = Ast::new();
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(3, 8));
        let then_val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(11, 13));
        let then_blk = ast
            .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(10, 15));
        let else_val =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(23, 24));
        let else_blk = ast
            .add_expr(Expr::Block(vec![], Some(else_val)), Span::new(22, 26));
        let if_expr = ast.add_expr(
            Expr::If(cond, then_blk, Some(else_blk)),
            Span::new(0, 26),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(if_expr).await.unwrap();
        assert_eq!(result, Value::Int(0));
    }

    #[tokio::test]
    async fn if_expr_no_else_true() {
        // IF true { 42 }  (no else)
        let mut ast = Ast::new();
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7));
        let then_val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(10, 12));
        let then_blk =
            ast.add_expr(Expr::Block(vec![], Some(then_val)), Span::new(9, 14));
        let if_expr =
            ast.add_expr(Expr::If(cond, then_blk, None), Span::new(0, 14));

        let mut interp = test_interp(&ast);
        let result = interp.eval(if_expr).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn if_expr_no_else_false() {
        // IF false { 42 }  (no else, returns Option.None)
        let mut ast = Ast::new();
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(3, 8));
        let then_val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(11, 13));
        let then_blk = ast
            .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(10, 15));
        let if_expr =
            ast.add_expr(Expr::If(cond, then_blk, None), Span::new(0, 15));

        let mut interp = test_interp(&ast);
        let result = interp.eval(if_expr).await.unwrap();
        assert!(result.is_none(&interp.type_exprs));
    }

    #[tokio::test]
    async fn if_expr_as_value() {
        // LET x = IF true { 10 } ELSE { 20 }
        let mut ast = Ast::new();
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(12, 16));
        let then_val =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(19, 21));
        let then_blk = ast
            .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(18, 23));
        let else_val =
            ast.add_expr(Expr::Literal(Literal::Int(20)), Span::new(31, 33));
        let else_blk = ast
            .add_expr(Expr::Block(vec![], Some(else_val)), Span::new(30, 35));
        let if_expr = ast.add_expr(
            Expr::If(cond, then_blk, Some(else_blk)),
            Span::new(8, 35),
        );

        let let_x = ast
            .add_stmt(Stmt::Let("x".into(), None, if_expr), Span::new(0, 35));
        let x_var = ast.add_expr(Expr::Var("x".into()), Span::new(0, 1));

        let mut interp = test_interp(&ast);
        interp.exec(let_x).await.unwrap();
        let result = interp.eval(x_var).await.unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[tokio::test]
    async fn block_expr_with_tail() {
        // { 42 }
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(2, 4));
        let blk = ast.add_expr(Expr::Block(vec![], Some(val)), Span::new(0, 6));

        let mut interp = test_interp(&ast);
        let result = interp.eval(blk).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn block_expr_no_tail() {
        // { } (empty block, returns Option.None)
        let mut ast = Ast::new();
        let blk = ast.add_expr(Expr::Block(vec![], None), Span::new(0, 3));

        let mut interp = test_interp(&ast);
        let result = interp.eval(blk).await.unwrap();
        assert!(result.is_none(&interp.type_exprs));
    }

    #[tokio::test]
    async fn block_expr_with_stmts() {
        // { LET x = 10; x + 1 }
        let mut ast = Ast::new();
        let ten =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(10, 12));
        let let_x =
            ast.add_stmt(Stmt::Let("x".into(), None, ten), Span::new(2, 12));

        let x = ast.add_expr(Expr::Var("x".into()), Span::new(14, 15));
        let one =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(18, 19));
        let tail =
            ast.add_expr(Expr::Binary(x, BinOp::Add, one), Span::new(14, 19));

        let blk = ast
            .add_expr(Expr::Block(vec![let_x], Some(tail)), Span::new(0, 21));

        let mut interp = test_interp(&ast);
        let result = interp.eval(blk).await.unwrap();
        assert_eq!(result, Value::Int(11));
    }

    #[tokio::test]
    async fn block_expr_scope_isolated() {
        // LET x = 1; { LET x = 10; x } evaluates to 10, outer x still 1
        let mut ast = Ast::new();

        // outer LET x = 1
        let one = ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(8, 9));
        let let_outer =
            ast.add_stmt(Stmt::Let("x".into(), None, one), Span::new(0, 9));

        // inner block: { LET x = 10; x }
        let ten =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(22, 24));
        let let_inner =
            ast.add_stmt(Stmt::Let("x".into(), None, ten), Span::new(13, 24));
        let x_inner = ast.add_expr(Expr::Var("x".into()), Span::new(26, 27));
        let blk = ast.add_expr(
            Expr::Block(vec![let_inner], Some(x_inner)),
            Span::new(11, 29),
        );

        // outer x reference
        let x_outer = ast.add_expr(Expr::Var("x".into()), Span::new(31, 32));

        let mut interp = test_interp(&ast);
        interp.exec(let_outer).await.unwrap();
        let blk_result = interp.eval(blk).await.unwrap();
        assert_eq!(blk_result, Value::Int(10));

        let outer_result = interp.eval(x_outer).await.unwrap();
        assert_eq!(outer_result, Value::Int(1));
    }

    #[tokio::test]
    async fn coalesce_option_none() {
        // (IF false { 42 }) ?? 0 -> 0
        // IF false without else returns Option.None
        let mut ast = Ast::new();

        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(4, 9));
        let then_val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14));
        let then_blk = ast
            .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(11, 16));
        let if_expr =
            ast.add_expr(Expr::If(cond, then_blk, None), Span::new(1, 17));

        let fallback =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(22, 23));
        let coalesce = ast.add_expr(
            Expr::Binary(if_expr, BinOp::Coalesce, fallback),
            Span::new(0, 23),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(coalesce).await.unwrap();
        assert_eq!(result, Value::Int(0));
    }

    #[tokio::test]
    async fn coalesce_non_option_error() {
        // 42 ?? 0 -> type error (Int is not Option or Result)
        let mut ast = Ast::new();

        let lhs =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(6, 7));
        let coalesce = ast
            .add_expr(Expr::Binary(lhs, BinOp::Coalesce, rhs), Span::new(0, 7));

        let mut interp = test_interp(&ast);
        let result = interp.eval(coalesce).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Option or Result"));
    }

    #[tokio::test]
    async fn coalesce_string_error() {
        // "hello" ?? "fallback" -> type error
        let mut ast = Ast::new();

        let lhs = ast.add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(0, 7),
        );
        let rhs = ast.add_expr(
            Expr::Literal(Literal::String("fallback".into())),
            Span::new(11, 21),
        );
        let coalesce = ast.add_expr(
            Expr::Binary(lhs, BinOp::Coalesce, rhs),
            Span::new(0, 21),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(coalesce).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn coalesce_short_circuit() {
        // Option.None ?? (side effect not visible, but rhs is evaluated)
        // We test that rhs IS evaluated when lhs is None
        // (IF false { 1 }) ?? 99 -> 99
        let mut ast = Ast::new();

        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(4, 9));
        let then_val =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13));
        let then_blk = ast
            .add_expr(Expr::Block(vec![], Some(then_val)), Span::new(11, 15));
        let if_expr =
            ast.add_expr(Expr::If(cond, then_blk, None), Span::new(1, 16));

        let fallback =
            ast.add_expr(Expr::Literal(Literal::Int(99)), Span::new(21, 23));
        let coalesce = ast.add_expr(
            Expr::Binary(if_expr, BinOp::Coalesce, fallback),
            Span::new(0, 23),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(coalesce).await.unwrap();
        assert_eq!(result, Value::Int(99));
    }

    #[tokio::test]
    async fn coalesce_chain() {
        // (IF false { 1 }) ?? (IF false { 2 }) ?? 3 -> 3
        let mut ast = Ast::new();

        // First: IF false { 1 } -> None
        let cond1 =
            ast.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(4, 9));
        let val1 =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13));
        let blk1 =
            ast.add_expr(Expr::Block(vec![], Some(val1)), Span::new(11, 15));
        let if1 = ast.add_expr(Expr::If(cond1, blk1, None), Span::new(1, 16));

        // Second: IF false { 2 } -> None
        let cond2 = ast
            .add_expr(Expr::Literal(Literal::Bool(false)), Span::new(24, 29));
        let val2 =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(32, 33));
        let blk2 =
            ast.add_expr(Expr::Block(vec![], Some(val2)), Span::new(31, 35));
        let if2 = ast.add_expr(Expr::If(cond2, blk2, None), Span::new(21, 36));

        // Third: literal 3
        let three =
            ast.add_expr(Expr::Literal(Literal::Int(3)), Span::new(41, 42));

        // Build: (if1 ?? if2) ?? 3
        let c1 = ast.add_expr(
            Expr::Binary(if1, BinOp::Coalesce, if2),
            Span::new(0, 37),
        );
        let c2 = ast.add_expr(
            Expr::Binary(c1, BinOp::Coalesce, three),
            Span::new(0, 42),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(c2).await.unwrap();
        assert_eq!(result, Value::Int(3));
    }

    #[tokio::test]
    async fn variant_option_some() {
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14));
        let variant = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 15),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(variant).await.unwrap();
        match result {
            Value::Tagged(ty_expr, idx, payloads) => {
                let base = interp.type_exprs.base_type(ty_expr);
                assert_eq!(base, Some(crate::value::TypeId::OPTION));
                assert_eq!(idx, 1); // Some is index 1
                assert_eq!(payloads.len(), 1);
            }
            _ => panic!("expected Tagged"),
        }
    }

    #[tokio::test]
    async fn variant_option_none_via_path() {
        // Option.None is resolved to Expr::Path by the resolution pass
        let mut ast = Ast::new();
        let path = ast.add_expr(
            Expr::Path(smallvec::smallvec!["Option".into(), "None".into()]),
            Span::new(0, 11),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(path).await.unwrap();
        match result {
            Value::Tagged(ty_expr, idx, payloads) => {
                let base = interp.type_exprs.base_type(ty_expr);
                assert_eq!(base, Some(crate::value::TypeId::OPTION));
                assert_eq!(idx, 0); // None is index 0
                assert!(payloads.is_empty());
            }
            _ => panic!("expected Tagged"),
        }
    }

    #[tokio::test]
    async fn variant_result_ok() {
        let mut ast = Ast::new();
        let val = ast.add_expr(
            Expr::Literal(Literal::String("success".into())),
            Span::new(10, 19),
        );
        let variant = ast.add_expr(
            Expr::Variant(
                "Result".into(),
                "Ok".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 20),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(variant).await.unwrap();
        match result {
            Value::Tagged(ty_expr, idx, payloads) => {
                let base = interp.type_exprs.base_type(ty_expr);
                assert_eq!(base, Some(crate::value::TypeId::RESULT));
                assert_eq!(idx, 0); // Ok is index 0
                assert_eq!(payloads.len(), 1);
            }
            _ => panic!("expected Tagged"),
        }
    }

    #[tokio::test]
    async fn variant_result_err() {
        let mut ast = Ast::new();
        let val = ast.add_expr(
            Expr::Literal(Literal::String("oops".into())),
            Span::new(11, 17),
        );
        let variant = ast.add_expr(
            Expr::Variant(
                "Result".into(),
                "Err".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 18),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(variant).await.unwrap();
        match result {
            Value::Tagged(ty_expr, idx, payloads) => {
                let base = interp.type_exprs.base_type(ty_expr);
                assert_eq!(base, Some(crate::value::TypeId::RESULT));
                assert_eq!(idx, 1); // Err is index 1
                assert_eq!(payloads.len(), 1);
            }
            _ => panic!("expected Tagged"),
        }
    }

    #[tokio::test]
    async fn variant_unknown_type_error() {
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11));
        let variant = ast.add_expr(
            Expr::Variant(
                "Unknown".into(),
                "Foo".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 12),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(variant).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn variant_unknown_variant_error() {
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11));
        let variant = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Foo".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 12),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(variant).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn variant_arity_mismatch_error() {
        // Option.Some expects 1 arg, giving 0
        let mut ast = Ast::new();
        let variant = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![],
            ),
            Span::new(0, 11),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(variant).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn variant_some_requires_args() {
        // Accessing Option.Some without args (as field) is an error
        let mut ast = Ast::new();
        let base = ast.add_expr(Expr::Var("Option".into()), Span::new(0, 6));
        let field =
            ast.add_expr(Expr::Field(base, "Some".into()), Span::new(0, 11));

        let mut interp = test_interp(&ast);
        let result = interp.eval(field).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn optional_field_on_object() {
        // { x: 42 }?.x -> Option.Some(42)
        let mut ast = Ast::new();
        let v = ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8));
        let obj =
            ast.add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(0, 10));
        let opt_field = ast
            .add_expr(Expr::OptionalField(obj, "x".into()), Span::new(0, 13));

        let mut interp = test_interp(&ast);
        let result = interp.eval(opt_field).await.unwrap();
        assert!(result.is_some(&interp.type_exprs));
        // Unwrap the Some to get 42
        match result {
            Value::Tagged(_, 1, payloads) => {
                let inner = interp.arena.get(payloads[0]).unwrap();
                assert_eq!(*inner, Value::Int(42));
            }
            _ => panic!("expected Option.Some"),
        }
    }

    #[tokio::test]
    async fn optional_field_on_none() {
        // Option.None?.x -> Option.None
        let mut ast = Ast::new();
        let none = ast.add_expr(
            Expr::Path(smallvec::smallvec!["Option".into(), "None".into()]),
            Span::new(0, 11),
        );
        let opt_field = ast
            .add_expr(Expr::OptionalField(none, "x".into()), Span::new(0, 14));

        let mut interp = test_interp(&ast);
        let result = interp.eval(opt_field).await.unwrap();
        assert!(result.is_none(&interp.type_exprs));
    }

    #[tokio::test]
    async fn optional_field_on_some_with_object() {
        // Option.Some({ x: 99 })?.x -> Option.Some(99)
        let mut ast = Ast::new();
        let v =
            ast.add_expr(Expr::Literal(Literal::Int(99)), Span::new(20, 22));
        let obj = ast
            .add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(12, 24));
        let some = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![obj],
            ),
            Span::new(0, 25),
        );
        let opt_field = ast
            .add_expr(Expr::OptionalField(some, "x".into()), Span::new(0, 28));

        let mut interp = test_interp(&ast);
        let result = interp.eval(opt_field).await.unwrap();
        assert!(result.is_some(&interp.type_exprs));
        match result {
            Value::Tagged(_, 1, payloads) => {
                let inner = interp.arena.get(payloads[0]).unwrap();
                assert_eq!(*inner, Value::Int(99));
            }
            _ => panic!("expected Option.Some"),
        }
    }

    #[tokio::test]
    async fn optional_field_missing_field() {
        // { x: 42 }?.y -> error (field not found)
        let mut ast = Ast::new();
        let v = ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(6, 8));
        let obj =
            ast.add_expr(Expr::Object(vec![("x".into(), v)]), Span::new(0, 10));
        let opt_field = ast
            .add_expr(Expr::OptionalField(obj, "y".into()), Span::new(0, 13));

        let mut interp = test_interp(&ast);
        let result = interp.eval(opt_field).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn optional_field_on_non_object() {
        // 42?.x -> type error
        let mut ast = Ast::new();
        let num =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let opt_field =
            ast.add_expr(Expr::OptionalField(num, "x".into()), Span::new(0, 5));

        let mut interp = test_interp(&ast);
        let result = interp.eval(opt_field).await;
        assert!(result.is_err());
    }

    // ===== `is` operator tests =====

    #[tokio::test]
    async fn is_simple_type_int() {
        // 42 is Int -> true
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let is_expr = ast.add_expr(
            Expr::Is(val, TypePattern::Type("Int".into())),
            Span::new(0, 8),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn is_simple_type_mismatch() {
        // 42 is String -> false
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let is_expr = ast.add_expr(
            Expr::Is(val, TypePattern::Type("String".into())),
            Span::new(0, 11),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await.unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[tokio::test]
    async fn is_variant_none() {
        // Option.None is Option.None -> true
        let mut ast = Ast::new();
        let none = ast.add_expr(
            Expr::Path(smallvec::smallvec!["Option".into(), "None".into()]),
            Span::new(0, 11),
        );
        let is_expr = ast.add_expr(
            Expr::Is(
                none,
                TypePattern::Variant("Option".into(), "None".into()),
            ),
            Span::new(0, 26),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn is_variant_some_wildcard() {
        // Option.Some(42) is Option.Some(_) -> true
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14));
        let some = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 15),
        );
        let is_expr = ast.add_expr(
            Expr::Is(
                some,
                TypePattern::VariantWildcard("Option".into(), "Some".into()),
            ),
            Span::new(0, 30),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn is_variant_mismatch() {
        // Option.None is Option.Some(_) -> false
        let mut ast = Ast::new();
        let none = ast.add_expr(
            Expr::Path(smallvec::smallvec!["Option".into(), "None".into()]),
            Span::new(0, 11),
        );
        let is_expr = ast.add_expr(
            Expr::Is(
                none,
                TypePattern::VariantWildcard("Option".into(), "Some".into()),
            ),
            Span::new(0, 26),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await.unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[tokio::test]
    async fn is_variant_bind_in_if() {
        // IF Option.Some(42) is Option.Some(val) { val } ELSE { 0 }
        // -> 42
        let mut ast = Ast::new();

        // Option.Some(42)
        let forty_two =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14));
        let some = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![forty_two],
            ),
            Span::new(0, 15),
        );

        // is Option.Some(val)
        let is_expr = ast.add_expr(
            Expr::Is(
                some,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec::smallvec!["val".into()],
                ),
            ),
            Span::new(0, 35),
        );

        // then: { val }
        let val_ref = ast.add_expr(Expr::Var("val".into()), Span::new(38, 41));
        let then_blk =
            ast.add_expr(Expr::Block(vec![], Some(val_ref)), Span::new(37, 43));

        // else: { 0 }
        let zero =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(51, 52));
        let else_blk =
            ast.add_expr(Expr::Block(vec![], Some(zero)), Span::new(50, 54));

        // IF
        let if_expr = ast.add_expr(
            Expr::If(is_expr, then_blk, Some(else_blk)),
            Span::new(0, 54),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(if_expr).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn is_variant_bind_else_branch() {
        // IF Option.None is Option.Some(val) { val } ELSE { 99 }
        // -> 99 (bindings not visible in else)
        let mut ast = Ast::new();

        // Option.None (resolved to Path)
        let none = ast.add_expr(
            Expr::Path(smallvec::smallvec!["Option".into(), "None".into()]),
            Span::new(3, 14),
        );

        // is Option.Some(val)
        let is_expr = ast.add_expr(
            Expr::Is(
                none,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec::smallvec!["val".into()],
                ),
            ),
            Span::new(0, 35),
        );

        // then: { val }
        let val_ref = ast.add_expr(Expr::Var("val".into()), Span::new(38, 41));
        let then_blk =
            ast.add_expr(Expr::Block(vec![], Some(val_ref)), Span::new(37, 43));

        // else: { 99 }
        let ninety_nine =
            ast.add_expr(Expr::Literal(Literal::Int(99)), Span::new(51, 53));
        let else_blk = ast.add_expr(
            Expr::Block(vec![], Some(ninety_nine)),
            Span::new(50, 55),
        );

        // IF
        let if_expr = ast.add_expr(
            Expr::If(is_expr, then_blk, Some(else_blk)),
            Span::new(0, 55),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(if_expr).await.unwrap();
        assert_eq!(result, Value::Int(99));
    }

    #[tokio::test]
    async fn is_variant_bind_scope_isolated() {
        // `val` should NOT be visible after the IF
        // IF Option.Some(42) is Option.Some(val) { val } ELSE { 0 }
        // val  // should error
        let mut ast = Ast::new();

        // Option.Some(42)
        let forty_two =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14));
        let some = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![forty_two],
            ),
            Span::new(0, 15),
        );

        // is Option.Some(val)
        let is_expr = ast.add_expr(
            Expr::Is(
                some,
                TypePattern::VariantBind(
                    "Option".into(),
                    "Some".into(),
                    smallvec::smallvec!["val".into()],
                ),
            ),
            Span::new(0, 35),
        );

        // then: { val }
        let val_ref1 = ast.add_expr(Expr::Var("val".into()), Span::new(38, 41));
        let then_blk = ast
            .add_expr(Expr::Block(vec![], Some(val_ref1)), Span::new(37, 43));

        // else: { 0 }
        let zero =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(51, 52));
        let else_blk =
            ast.add_expr(Expr::Block(vec![], Some(zero)), Span::new(50, 54));

        // IF
        let if_expr = ast.add_expr(
            Expr::If(is_expr, then_blk, Some(else_blk)),
            Span::new(0, 54),
        );
        let if_stmt = ast.add_stmt(Stmt::Expr(if_expr), Span::new(0, 54));

        // val (after if)
        let val_ref2 = ast.add_expr(Expr::Var("val".into()), Span::new(56, 59));

        let mut interp = test_interp(&ast);
        interp.exec(if_stmt).await.unwrap();
        // val should NOT be visible
        let result = interp.eval(val_ref2).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn is_unknown_type_error() {
        // 42 is Unknown -> error
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let is_expr = ast.add_expr(
            Expr::Is(val, TypePattern::Type("Unknown".into())),
            Span::new(0, 12),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn is_result_ok() {
        // Result.Ok(1) is Result.Ok(_) -> true
        let mut ast = Ast::new();
        let one =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11));
        let ok = ast.add_expr(
            Expr::Variant(
                "Result".into(),
                "Ok".into(),
                smallvec::smallvec![one],
            ),
            Span::new(0, 12),
        );
        let is_expr = ast.add_expr(
            Expr::Is(
                ok,
                TypePattern::VariantWildcard("Result".into(), "Ok".into()),
            ),
            Span::new(0, 25),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn is_result_err() {
        // Result.Err("oops") is Result.Ok(_) -> false
        let mut ast = Ast::new();
        let msg = ast.add_expr(
            Expr::Literal(Literal::String("oops".into())),
            Span::new(11, 17),
        );
        let err = ast.add_expr(
            Expr::Variant(
                "Result".into(),
                "Err".into(),
                smallvec::smallvec![msg],
            ),
            Span::new(0, 18),
        );
        let is_expr = ast.add_expr(
            Expr::Is(
                err,
                TypePattern::VariantWildcard("Result".into(), "Ok".into()),
            ),
            Span::new(0, 30),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await.unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[tokio::test]
    async fn is_variant_with_payload_requires_parens() {
        // `is Option.Some` without parens is an error (must use `(_)` or `(name)`)
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(12, 14));
        let some = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![val],
            ),
            Span::new(0, 15),
        );
        let is_expr = ast.add_expr(
            Expr::Is(
                some,
                TypePattern::Variant("Option".into(), "Some".into()),
            ),
            Span::new(0, 30),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(is_expr).await;
        assert!(result.is_err());
    }

    // ===== Structural type equality tests =====

    #[tokio::test]
    async fn tagged_values_structural_equality() {
        // Two Option.Some(1) values created separately should be equal,
        // even though they have different TypeExprIds.
        let mut ast = Ast::new();
        let one_a =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13));
        let some_a = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![one_a],
            ),
            Span::new(0, 14),
        );
        let one_b =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(32, 33));
        let some_b = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![one_b],
            ),
            Span::new(20, 34),
        );
        let eq_expr = ast.add_expr(
            Expr::Binary(some_a, BinOp::Eq, some_b),
            Span::new(0, 40),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(eq_expr).await.unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[tokio::test]
    async fn tagged_values_different_payloads_not_equal() {
        // Option.Some(1) != Option.Some(2)
        let mut ast = Ast::new();
        let one =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13));
        let some_one = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![one],
            ),
            Span::new(0, 14),
        );
        let two =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(32, 33));
        let some_two = ast.add_expr(
            Expr::Variant(
                "Option".into(),
                "Some".into(),
                smallvec::smallvec![two],
            ),
            Span::new(20, 34),
        );
        let eq_expr = ast.add_expr(
            Expr::Binary(some_one, BinOp::Eq, some_two),
            Span::new(0, 40),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(eq_expr).await.unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    // ===== Type cast (as) tests =====

    #[tokio::test]
    async fn as_int_to_float() {
        // 42 as Float -> 42.0
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let ty = ast.add_type_expr(
            AstTypeExpr::Named("Float".into()),
            Span::new(6, 11),
        );
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 11));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await.unwrap();
        assert_eq!(result, Value::Float(OrderedFloat(42.0)));
    }

    #[tokio::test]
    async fn as_float_to_int_truncates() {
        // 3.7 as Int -> 3
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Float(3.7)), Span::new(0, 3));
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(7, 10));
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 10));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await.unwrap();
        assert_eq!(result, Value::Int(3));
    }

    #[tokio::test]
    async fn as_bool_to_int() {
        // true as Int -> 1
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(0, 4));
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(8, 11));
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 11));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await.unwrap();
        assert_eq!(result, Value::Int(1));

        // false as Int -> 0
        let mut ast2 = Ast::new();
        let val2 =
            ast2.add_expr(Expr::Literal(Literal::Bool(false)), Span::new(0, 5));
        let ty2 = ast2
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(9, 12));
        let cast2 = ast2.add_expr(Expr::As(val2, ty2), Span::new(0, 12));

        let mut interp2 = test_interp(&ast2);
        let result2 = interp2.eval(cast2).await.unwrap();
        assert_eq!(result2, Value::Int(0));
    }

    #[tokio::test]
    async fn as_int_to_string() {
        // 42 as String -> "42"
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let ty = ast.add_type_expr(
            AstTypeExpr::Named("String".into()),
            Span::new(6, 12),
        );
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 12));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await.unwrap();
        match result {
            Value::String(id) => {
                assert_eq!(interp.arena.get_str(id), Some("42"));
            }
            _ => panic!("expected String"),
        }
    }

    #[tokio::test]
    async fn as_float_to_string() {
        // 3.14 as String -> "3.14"
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Float(3.14)), Span::new(0, 4));
        let ty = ast.add_type_expr(
            AstTypeExpr::Named("String".into()),
            Span::new(8, 14),
        );
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 14));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await.unwrap();
        match result {
            Value::String(id) => {
                assert_eq!(interp.arena.get_str(id), Some("3.14"));
            }
            _ => panic!("expected String"),
        }
    }

    #[tokio::test]
    async fn as_bool_to_string() {
        // true as String -> "TRUE"
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(0, 4));
        let ty = ast.add_type_expr(
            AstTypeExpr::Named("String".into()),
            Span::new(8, 14),
        );
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 14));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await.unwrap();
        match result {
            Value::String(id) => {
                assert_eq!(interp.arena.get_str(id), Some("TRUE"));
            }
            _ => panic!("expected String"),
        }
    }

    #[tokio::test]
    async fn as_identity_int() {
        // 42 as Int -> 42 (identity)
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(6, 9));
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 9));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await.unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn as_unsupported_conversion_error() {
        // "hello" as Int -> error (use `read` for fallible conversions)
        let mut ast = Ast::new();
        let val = ast.add_expr(
            Expr::Literal(Literal::String("hello".into())),
            Span::new(0, 7),
        );
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(11, 14));
        let cast = ast.add_expr(Expr::As(val, ty), Span::new(0, 14));

        let mut interp = test_interp(&ast);
        let result = interp.eval(cast).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("cannot cast"));
    }

    // ===== Fallible conversion (read) tests =====

    #[tokio::test]
    async fn read_string_to_int_ok() {
        // "42" read Int -> Result.Ok(42)
        let mut ast = Ast::new();
        let val = ast.add_expr(
            Expr::Literal(Literal::String("42".into())),
            Span::new(0, 4),
        );
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(10, 13));
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 13));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await.unwrap();

        // Should be Result.Ok(42)
        assert!(result.is_ok(&interp.type_exprs));
        match &result {
            Value::Tagged(_, 0, payload) => {
                let inner = interp.arena.get(payload[0]).unwrap();
                assert_eq!(inner, &Value::Int(42));
            }
            _ => panic!("expected Result.Ok"),
        }
    }

    #[tokio::test]
    async fn read_string_to_int_err() {
        // "abc" read Int -> Result.Err("invalid integer: abc")
        let mut ast = Ast::new();
        let val = ast.add_expr(
            Expr::Literal(Literal::String("abc".into())),
            Span::new(0, 5),
        );
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(11, 14));
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 14));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await.unwrap();

        // Should be Result.Err
        assert!(result.is_err(&interp.type_exprs));
        match &result {
            Value::Tagged(_, 1, payload) => {
                let inner = interp.arena.get(payload[0]).unwrap();
                match inner {
                    Value::String(sid) => {
                        let msg = interp.arena.get_str(*sid).unwrap();
                        assert!(msg.contains("invalid integer"));
                    }
                    _ => panic!("expected error message string"),
                }
            }
            _ => panic!("expected Result.Err"),
        }
    }

    #[tokio::test]
    async fn read_string_to_float_ok() {
        // "3.14" read Float -> Result.Ok(3.14)
        let mut ast = Ast::new();
        let val = ast.add_expr(
            Expr::Literal(Literal::String("3.14".into())),
            Span::new(0, 6),
        );
        let ty = ast.add_type_expr(
            AstTypeExpr::Named("Float".into()),
            Span::new(12, 17),
        );
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 17));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await.unwrap();

        // Should be Result.Ok(3.14)
        assert!(result.is_ok(&interp.type_exprs));
        match &result {
            Value::Tagged(_, 0, payload) => {
                let inner = interp.arena.get(payload[0]).unwrap();
                assert_eq!(inner, &Value::Float(OrderedFloat(3.14)));
            }
            _ => panic!("expected Result.Ok"),
        }
    }

    #[tokio::test]
    async fn read_string_to_float_err() {
        // "xyz" read Float -> Result.Err(...)
        let mut ast = Ast::new();
        let val = ast.add_expr(
            Expr::Literal(Literal::String("xyz".into())),
            Span::new(0, 5),
        );
        let ty = ast.add_type_expr(
            AstTypeExpr::Named("Float".into()),
            Span::new(11, 16),
        );
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 16));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await.unwrap();

        // Should be Result.Err
        assert!(result.is_err(&interp.type_exprs));
    }

    #[tokio::test]
    async fn read_int_to_bool_zero() {
        // 0 read Bool -> Result.Ok(false)
        let mut ast = Ast::new();
        let val = ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(0, 1));
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Bool".into()), Span::new(7, 11));
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 11));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await.unwrap();

        assert!(result.is_ok(&interp.type_exprs));
        match &result {
            Value::Tagged(_, 0, payload) => {
                let inner = interp.arena.get(payload[0]).unwrap();
                assert_eq!(inner, &Value::Bool(false));
            }
            _ => panic!("expected Result.Ok"),
        }
    }

    #[tokio::test]
    async fn read_int_to_bool_one() {
        // 1 read Bool -> Result.Ok(true)
        let mut ast = Ast::new();
        let val = ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(0, 1));
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Bool".into()), Span::new(7, 11));
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 11));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await.unwrap();

        assert!(result.is_ok(&interp.type_exprs));
        match &result {
            Value::Tagged(_, 0, payload) => {
                let inner = interp.arena.get(payload[0]).unwrap();
                assert_eq!(inner, &Value::Bool(true));
            }
            _ => panic!("expected Result.Ok"),
        }
    }

    #[tokio::test]
    async fn read_int_to_bool_invalid() {
        // 42 read Bool -> Result.Err(...)
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Bool".into()), Span::new(8, 12));
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 12));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await.unwrap();

        // Should be Result.Err
        assert!(result.is_err(&interp.type_exprs));
        match &result {
            Value::Tagged(_, 1, payload) => {
                let inner = interp.arena.get(payload[0]).unwrap();
                match inner {
                    Value::String(sid) => {
                        let msg = interp.arena.get_str(*sid).unwrap();
                        assert!(msg.contains("expected 0 or 1"));
                    }
                    _ => panic!("expected error message string"),
                }
            }
            _ => panic!("expected Result.Err"),
        }
    }

    #[tokio::test]
    async fn read_unsupported_conversion_error() {
        // 42 read Int -> runtime error (not a fallible conversion)
        let mut ast = Ast::new();
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(8, 11));
        let read = ast.add_expr(Expr::Read(val, ty), Span::new(0, 11));

        let mut interp = test_interp(&ast);
        let result = interp.eval(read).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("cannot read"));
    }

    // ---- Closure tests ----

    #[tokio::test]
    async fn closure_creation_simple() {
        // x => x * 2
        let mut ast = Ast::new();
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(5, 6));
        let two =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(9, 10));
        let body =
            ast.add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(5, 10));
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 10),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(closure).await.unwrap();

        match result {
            Value::Closure { params, ret, .. } => {
                assert_eq!(params.len(), 1);
                assert!(ret.is_none());
            }
            _ => panic!("expected Closure"),
        }
    }

    #[tokio::test]
    async fn closure_creation_with_types() {
        // (x: Int) -> Int => x * x
        let mut ast = Ast::new();
        let int_ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(4, 7));
        let ret_ty = ast
            .add_type_expr(AstTypeExpr::Named("Int".into()), Span::new(12, 15));
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(19, 20));
        let x2 = ast.add_expr(Expr::Var("x".into()), Span::new(23, 24));
        let body =
            ast.add_expr(Expr::Binary(x, BinOp::Mul, x2), Span::new(19, 24));
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), Some(int_ty))],
                ret: Some(ret_ty),
                body,
            },
            Span::new(0, 24),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(closure).await.unwrap();

        match result {
            Value::Closure { params, ret, .. } => {
                assert_eq!(params.len(), 1);
                assert!(params[0].1.is_some()); // has type annotation
                assert!(ret.is_some()); // has return type
            }
            _ => panic!("expected Closure"),
        }
    }

    #[tokio::test]
    async fn closure_captures_environment() {
        // LET factor = 3
        // LET triple = x => x * factor
        // triple is a closure that captures `factor`
        let mut ast = Ast::new();
        let three =
            ast.add_expr(Expr::Literal(Literal::Int(3)), Span::new(13, 14));
        let let_factor = ast.add_stmt(
            Stmt::Let("factor".into(), None, three),
            Span::new(0, 14),
        );

        let x = ast.add_expr(Expr::Var("x".into()), Span::new(25, 26));
        let factor =
            ast.add_expr(Expr::Var("factor".into()), Span::new(29, 35));
        let body = ast
            .add_expr(Expr::Binary(x, BinOp::Mul, factor), Span::new(25, 35));
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(16, 35),
        );

        let mut interp = test_interp(&ast);
        interp.exec(let_factor).await.unwrap();
        let result = interp.eval(closure).await.unwrap();

        match &result {
            Value::Closure { env, .. } => {
                // The closure should have captured `factor`
                let factor_id = interp.arena.lookup_string("factor").unwrap();
                assert!(env.lookup(factor_id).is_some());
            }
            _ => panic!("expected Closure"),
        }
    }

    #[tokio::test]
    async fn closure_display() {
        // Closure displays as <closure(n)>
        let mut ast = Ast::new();
        let body =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(5, 7));
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![
                    ("x".into(), None),
                    ("y".into(), None)
                ],
                ret: None,
                body,
            },
            Span::new(0, 7),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(closure).await.unwrap();
        let displayed = interp.display(&result);
        assert_eq!(displayed, "<closure(2)>");
    }

    // ---- FUN statement tests ----

    #[tokio::test]
    async fn fun_definition_simple() {
        // FUN double (x) { x * 2 }
        let mut ast = Ast::new();
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(17, 18));
        let two =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(21, 22));
        let body_expr =
            ast.add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(17, 22));
        let body = ast
            .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(15, 24));
        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "double".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 24),
        );

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();

        // Function should be registered
        let name_id = interp.arena.intern("double");
        assert!(interp.functions.contains_key(&name_id));
    }

    #[tokio::test]
    async fn fun_call_simple() {
        // FUN double (x) { x * 2 }
        // double(21)
        let mut ast = Ast::new();

        // Function body: x * 2
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(17, 18));
        let two =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(21, 22));
        let body_expr =
            ast.add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(17, 22));
        let body = ast
            .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(15, 24));
        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "double".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 24),
        );

        // Call: double(21)
        let arg =
            ast.add_expr(Expr::Literal(Literal::Int(21)), Span::new(32, 34));
        let callee =
            ast.add_expr(Expr::Var("double".into()), Span::new(26, 32));
        let call = ast.add_expr(
            Expr::Call(callee, smallvec::smallvec![arg]),
            Span::new(26, 35),
        );

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(call).await.unwrap();

        assert_eq!(result, Value::Int(42));
    }

    #[tokio::test]
    async fn fun_as_value() {
        // FUN square (x) { x * x }
        // LET f = square
        // f should be a Value::Function
        let mut ast = Ast::new();

        let x1 = ast.add_expr(Expr::Var("x".into()), Span::new(17, 18));
        let x2 = ast.add_expr(Expr::Var("x".into()), Span::new(21, 22));
        let body_expr =
            ast.add_expr(Expr::Binary(x1, BinOp::Mul, x2), Span::new(17, 22));
        let body = ast
            .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(15, 24));
        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "square".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 24),
        );

        // Reference: square (no call)
        let square_ref =
            ast.add_expr(Expr::Var("square".into()), Span::new(36, 42));

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(square_ref).await.unwrap();

        assert!(matches!(result, Value::Function { .. }));
    }

    #[tokio::test]
    async fn fun_recursive_factorial() {
        // FUN factorial (n) {
        //   IF n <= 1 { 1 }
        //   ELSE { n * factorial(n - 1) }
        // }
        // factorial(5) should be 120
        let mut ast = Ast::new();

        // n <= 1
        let n1 = ast.add_expr(Expr::Var("n".into()), Span::new(0, 1));
        let one1 =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(5, 6));
        let cond =
            ast.add_expr(Expr::Binary(n1, BinOp::Le, one1), Span::new(0, 6));

        // Then: 1
        let then_expr =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(10, 11));
        let then_block = ast
            .add_expr(Expr::Block(vec![], Some(then_expr)), Span::new(8, 12));

        // Else: n * factorial(n - 1)
        let n2 = ast.add_expr(Expr::Var("n".into()), Span::new(20, 21));
        let n3 = ast.add_expr(Expr::Var("n".into()), Span::new(35, 36));
        let one2 =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(39, 40));
        let n_minus_1 =
            ast.add_expr(Expr::Binary(n3, BinOp::Sub, one2), Span::new(35, 40));
        let rec_callee =
            ast.add_expr(Expr::Var("factorial".into()), Span::new(24, 33));
        let rec_call = ast.add_expr(
            Expr::Call(rec_callee, smallvec::smallvec![n_minus_1]),
            Span::new(24, 41),
        );
        let else_expr = ast.add_expr(
            Expr::Binary(n2, BinOp::Mul, rec_call),
            Span::new(20, 41),
        );
        let else_block = ast
            .add_expr(Expr::Block(vec![], Some(else_expr)), Span::new(18, 43));

        // IF expr
        let if_expr = ast.add_expr(
            Expr::If(cond, then_block, Some(else_block)),
            Span::new(0, 43),
        );
        let body =
            ast.add_expr(Expr::Block(vec![], Some(if_expr)), Span::new(0, 45));

        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "factorial".into(),
                params: smallvec::smallvec![("n".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 50),
        );

        // Call: factorial(5)
        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(60, 61));
        let callee =
            ast.add_expr(Expr::Var("factorial".into()), Span::new(52, 61));
        let call = ast.add_expr(
            Expr::Call(callee, smallvec::smallvec![five]),
            Span::new(52, 62),
        );

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(call).await.unwrap();

        assert_eq!(result, Value::Int(120));
    }

    #[tokio::test]
    async fn fun_display() {
        // Function displays as <function name(n)>
        let mut ast = Ast::new();
        let body =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(15, 17));
        let body_block =
            ast.add_expr(Expr::Block(vec![], Some(body)), Span::new(13, 19));
        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "test".into(),
                params: smallvec::smallvec![
                    ("x".into(), None),
                    ("y".into(), None)
                ],
                ret: None,
                body: body_block,
            },
            Span::new(0, 19),
        );

        let fun_ref = ast.add_expr(Expr::Var("test".into()), Span::new(20, 24));

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(fun_ref).await.unwrap();
        let displayed = interp.display(&result);
        assert_eq!(displayed, "<function test(2)>");
    }

    // ---- Expression-based callees tests ----

    #[tokio::test]
    async fn call_field_closure() {
        // LET ops = { inc: x => x + 1 }
        // ops.inc(5) should be 6
        let mut ast = Ast::new();

        // Closure: x => x + 1
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(0, 1));
        let one = ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(4, 5));
        let body =
            ast.add_expr(Expr::Binary(x, BinOp::Add, one), Span::new(0, 5));
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 10),
        );

        // Object: { inc: closure }
        let obj = ast.add_expr(
            Expr::Object(vec![("inc".into(), closure)]),
            Span::new(10, 30),
        );

        // LET ops = obj
        let let_ops =
            ast.add_stmt(Stmt::Let("ops".into(), None, obj), Span::new(0, 35));

        // ops.inc
        let ops_var = ast.add_expr(Expr::Var("ops".into()), Span::new(40, 43));
        let field_access =
            ast.add_expr(Expr::Field(ops_var, "inc".into()), Span::new(40, 47));

        // ops.inc(5)
        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(48, 49));
        let call = ast.add_expr(
            Expr::Call(field_access, smallvec::smallvec![five]),
            Span::new(40, 50),
        );

        let mut interp = test_interp(&ast);
        interp.exec(let_ops).await.unwrap();
        let result = interp.eval(call).await.unwrap();

        assert_eq!(result, Value::Int(6));
    }

    #[tokio::test]
    async fn call_chained() {
        // FUN make_adder (n) { x => x + n }
        // make_adder(5)(10) should be 15
        let mut ast = Ast::new();

        // Closure body: x + n
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(0, 1));
        let n = ast.add_expr(Expr::Var("n".into()), Span::new(4, 5));
        let add_expr =
            ast.add_expr(Expr::Binary(x, BinOp::Add, n), Span::new(0, 5));

        // Closure: x => x + n
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: add_expr,
            },
            Span::new(0, 10),
        );

        // Function body block containing closure
        let body =
            ast.add_expr(Expr::Block(vec![], Some(closure)), Span::new(0, 15));

        // FUN make_adder (n) { ... }
        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "make_adder".into(),
                params: smallvec::smallvec![("n".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 20),
        );

        // make_adder(5)
        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(30, 31));
        let callee1 =
            ast.add_expr(Expr::Var("make_adder".into()), Span::new(25, 35));
        let call1 = ast.add_expr(
            Expr::Call(callee1, smallvec::smallvec![five]),
            Span::new(25, 32),
        );

        // make_adder(5)(10)
        let ten =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(33, 35));
        let call2 = ast.add_expr(
            Expr::Call(call1, smallvec::smallvec![ten]),
            Span::new(25, 36),
        );

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(call2).await.unwrap();

        assert_eq!(result, Value::Int(15));
    }

    #[tokio::test]
    async fn call_iife() {
        // (x => x * 2)(21) should be 42
        let mut ast = Ast::new();

        // Closure body: x * 2
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(0, 1));
        let two = ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(4, 5));
        let body =
            ast.add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(0, 5));

        // Closure: x => x * 2
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 10),
        );

        // (closure)(21)
        let arg =
            ast.add_expr(Expr::Literal(Literal::Int(21)), Span::new(12, 14));
        let call = ast.add_expr(
            Expr::Call(closure, smallvec::smallvec![arg]),
            Span::new(0, 15),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(call).await.unwrap();

        assert_eq!(result, Value::Int(42));
    }

    // ---- Type checking tests ----

    #[tokio::test]
    async fn type_check_param_error() {
        // FUN add (a: Int, b: Int) { a + b }
        // add("x", 1) should fail
        let mut ast = Ast::new();

        // Type expression for Int
        let int_ty = ast.add_type_expr(
            crate::ast::AstTypeExpr::Named("Int".into()),
            Span::new(0, 3),
        );

        // Function body: a + b
        let a = ast.add_expr(Expr::Var("a".into()), Span::new(20, 21));
        let b = ast.add_expr(Expr::Var("b".into()), Span::new(24, 25));
        let body_expr =
            ast.add_expr(Expr::Binary(a, BinOp::Add, b), Span::new(20, 25));
        let body = ast
            .add_expr(Expr::Block(vec![], Some(body_expr)), Span::new(18, 27));

        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "add".into(),
                params: smallvec::smallvec![
                    ("a".into(), Some(int_ty)),
                    ("b".into(), Some(int_ty))
                ],
                ret: None,
                body,
            },
            Span::new(0, 30),
        );

        // Call: add("x", 1)
        let str_arg = ast.add_expr(
            Expr::Literal(Literal::String("x".into())),
            Span::new(35, 38),
        );
        let int_arg =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(40, 41));
        let callee = ast.add_expr(Expr::Var("add".into()), Span::new(32, 35));
        let call = ast.add_expr(
            Expr::Call(callee, smallvec::smallvec![str_arg, int_arg]),
            Span::new(32, 42),
        );

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(call).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("expected"));
        assert!(err.to_string().contains("Int"));
    }

    #[tokio::test]
    async fn type_check_return_error() {
        // FUN bad () -> Int { "not an int" }
        // bad() should fail return type check
        let mut ast = Ast::new();

        // Type expression for Int
        let int_ty = ast.add_type_expr(
            crate::ast::AstTypeExpr::Named("Int".into()),
            Span::new(0, 3),
        );

        // Function body: "not an int"
        let str_lit = ast.add_expr(
            Expr::Literal(Literal::String("not an int".into())),
            Span::new(20, 32),
        );
        let body =
            ast.add_expr(Expr::Block(vec![], Some(str_lit)), Span::new(18, 34));

        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "bad".into(),
                params: smallvec::smallvec![],
                ret: Some(int_ty),
                body,
            },
            Span::new(0, 35),
        );

        // Call: bad()
        let callee = ast.add_expr(Expr::Var("bad".into()), Span::new(40, 43));
        let call = ast.add_expr(
            Expr::Call(callee, smallvec::smallvec![]),
            Span::new(40, 45),
        );

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(call).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("expected return type"));
    }

    // ===== Pipeline (|>) tests =====

    #[tokio::test]
    async fn pipe_with_closure() {
        // 5 |> (x => x * 2) -> 10
        let mut ast = Ast::new();

        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1));

        // Closure: x => x * 2
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(6, 7));
        let two =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(12, 13));
        let mul =
            ast.add_expr(Expr::Binary(x, BinOp::Mul, two), Span::new(6, 13));
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: mul,
            },
            Span::new(4, 14),
        );

        let pipe = ast.add_expr(
            Expr::Binary(five, BinOp::Pipe, closure),
            Span::new(0, 14),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(pipe).await.unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[tokio::test]
    async fn pipe_with_named_function() {
        // FUN double (x) { x * 2 }
        // 5 |> double -> 10
        let mut ast = Ast::new();

        // Function body: x * 2
        let x_body = ast.add_expr(Expr::Var("x".into()), Span::new(18, 19));
        let two =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(22, 23));
        let mul = ast
            .add_expr(Expr::Binary(x_body, BinOp::Mul, two), Span::new(18, 23));
        let body =
            ast.add_expr(Expr::Block(vec![], Some(mul)), Span::new(16, 25));

        let fun = ast.add_stmt(
            Stmt::Fun {
                name: "double".into(),
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body,
            },
            Span::new(0, 26),
        );

        // 5 |> double
        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(30, 31));
        let func_ref =
            ast.add_expr(Expr::Var("double".into()), Span::new(35, 41));
        let pipe = ast.add_expr(
            Expr::Binary(five, BinOp::Pipe, func_ref),
            Span::new(30, 41),
        );

        let mut interp = test_interp(&ast);
        interp.exec(fun).await.unwrap();
        let result = interp.eval(pipe).await.unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[tokio::test]
    async fn pipe_chain() {
        // 5 |> (x => x * 2) |> (x => x + 1) -> 11
        let mut ast = Ast::new();

        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1));

        // Closure 1: x => x * 2
        let x1 = ast.add_expr(Expr::Var("x".into()), Span::new(6, 7));
        let two =
            ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(12, 13));
        let mul =
            ast.add_expr(Expr::Binary(x1, BinOp::Mul, two), Span::new(6, 13));
        let c1 = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: mul,
            },
            Span::new(4, 14),
        );

        // Closure 2: x => x + 1
        let x2 = ast.add_expr(Expr::Var("x".into()), Span::new(22, 23));
        let one =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(28, 29));
        let add =
            ast.add_expr(Expr::Binary(x2, BinOp::Add, one), Span::new(22, 29));
        let c2 = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![("x".into(), None)],
                ret: None,
                body: add,
            },
            Span::new(20, 30),
        );

        // (5 |> c1) |> c2
        let p1 =
            ast.add_expr(Expr::Binary(five, BinOp::Pipe, c1), Span::new(0, 15));
        let p2 =
            ast.add_expr(Expr::Binary(p1, BinOp::Pipe, c2), Span::new(0, 31));

        let mut interp = test_interp(&ast);
        let result = interp.eval(p2).await.unwrap();
        assert_eq!(result, Value::Int(11));
    }

    #[tokio::test]
    async fn pipe_non_function_error() {
        // 5 |> 10 -> error (10 is not a function)
        let mut ast = Ast::new();

        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1));
        let ten =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(5, 7));
        let pipe =
            ast.add_expr(Expr::Binary(five, BinOp::Pipe, ten), Span::new(0, 7));

        let mut interp = test_interp(&ast);
        let result = interp.eval(pipe).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("|>"));
        assert!(err.to_string().contains("function"));
    }

    #[tokio::test]
    async fn pipe_arity_mismatch_error() {
        // 5 |> ((a, b) => a + b) -> error (arity mismatch)
        let mut ast = Ast::new();

        let five =
            ast.add_expr(Expr::Literal(Literal::Int(5)), Span::new(0, 1));

        // Closure: (a, b) => a + b (expects 2 args)
        let a = ast.add_expr(Expr::Var("a".into()), Span::new(12, 13));
        let b = ast.add_expr(Expr::Var("b".into()), Span::new(16, 17));
        let add =
            ast.add_expr(Expr::Binary(a, BinOp::Add, b), Span::new(12, 17));
        let closure = ast.add_expr(
            Expr::Closure {
                params: smallvec::smallvec![
                    ("a".into(), None),
                    ("b".into(), None)
                ],
                ret: None,
                body: add,
            },
            Span::new(5, 18),
        );

        let pipe = ast.add_expr(
            Expr::Binary(five, BinOp::Pipe, closure),
            Span::new(0, 18),
        );

        let mut interp = test_interp(&ast);
        let result = interp.eval(pipe).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("expected 2 arguments"));
    }
}
