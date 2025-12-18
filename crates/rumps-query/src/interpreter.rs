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

use async_recursion::async_recursion;
use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use rumps_storage::{Database, Transaction};

use crate::ast::{Ast, BinOp, Expr, ExprId, Literal, Stmt, StmtId, UnOp};
use crate::env::Environment;
use crate::io::IoContext;
use crate::value::{StringId, TypeRegistry, Value, ValueArena, ValueId};
use crate::{Error, Result, Span};

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

    /// I/O context for output operations.
    io: I,
}

// Public API
impl<'a, I: IoContext> Interpreter<'a, I> {
    /// Create a new interpreter for the given AST, database, and I/O context.
    pub(crate) fn new(ast: &'a Ast, db: Database, io: I) -> Result<Self> {
        let mut arena = ValueArena::new();
        let registry = TypeRegistry::new(&mut arena)?;

        Ok(Self {
            ast,
            env: Environment::new(),
            db,
            txn: None,
            arena,
            registry,
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
            Expr::Call(name, args) => self.call(&name, &args, span).await,
            Expr::Object(fields) => self.object(&fields).await,
            Expr::Array(elems) => self.array(&elems).await,
            Expr::Index(base, idx) => self.index(base, idx, span).await,
            Expr::Field(base, field) => self.field(base, &field, span).await,
            Expr::Block(stmts, tail) => self.block(&stmts, tail).await,
            Expr::If(cond, then_br, else_br) => {
                self.r#if(cond, then_br, else_br).await
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
            Stmt::Let(name, expr_id) => self.r#let(&name, expr_id).await,
            Stmt::Set(target, expr_id) => self.set(target, expr_id, span).await,
            Stmt::Kill(target) => self.kill(target, span).await,
            Stmt::Output(expr_id) => self.output(expr_id).await,
            Stmt::Expr(expr_id) => {
                // Evaluate for side effects, discard result
                self.eval(expr_id).await.map(|_| ())
            }
        }
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

    /// Evaluate a lexical variable reference (LET bindings only).
    ///
    /// Does NOT fall back to B-tree locals; use `GET` for those.
    fn var(&mut self, name: &str, span: Span) -> Result<Value> {
        self.arena
            .lookup_string(name)
            .and_then(|id| self.env.scopes.lookup(id))
            .and_then(|val_id| self.arena.get(val_id).cloned())
            .ok_or_else(|| {
                Error::runtime(span, format!("undefined variable `{name}`"))
            })
    }

    /// Evaluate a binary operation.
    ///
    /// Handles short-circuit evaluation for `AND` and `OR`.
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
                                    right.type_name(&self.registry)
                                ),
                            )),
                        }
                    }
                    _ => Err(Error::type_err(
                        span,
                        format!(
                            "logical AND requires booleans; got {}",
                            left.type_name(&self.registry)
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
                                    right.type_name(&self.registry)
                                ),
                            )),
                        }
                    }
                    _ => Err(Error::type_err(
                        span,
                        format!(
                            "logical OR requires booleans; got {}",
                            left.type_name(&self.registry)
                        ),
                    )),
                }
            }
            // All other operators evaluate both sides
            _ => {
                let left = self.eval(lhs).await?;
                let right = self.eval(rhs).await?;
                self.apply_binop(&left, op, &right, span)
            }
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

    /// Evaluate a function call.
    #[async_recursion]
    async fn call(
        &mut self,
        _name: &str,
        _args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        // TODO: Function calls will be implemented in later phases
        Err(Error::runtime(span, "function calls not yet implemented"))
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

    /// Evaluate an array literal.
    #[async_recursion]
    async fn array(&mut self, elems: &[ExprId]) -> Result<Value> {
        let vec = self
            .array_elems(elems, Vec::with_capacity(elems.len()))
            .await?;
        Ok(Value::Array(vec))
    }

    /// Recursively evaluate array elements.
    #[async_recursion]
    async fn array_elems(
        &mut self,
        elems: &[ExprId],
        mut acc: Vec<ValueId>,
    ) -> Result<Vec<ValueId>> {
        match elems.split_first() {
            None => Ok(acc),
            Some((expr_id, tail)) => {
                let span = self.ast.expr_span(*expr_id).unwrap_or_default();
                let val = self.eval(*expr_id).await?;
                let val_id = self.arena.add(val, span);
                acc.push(val_id);
                self.array_elems(tail, acc).await
            }
        }
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
            (Value::Array(arr), Value::Int(i)) => {
                let index = if *i < 0 {
                    // Negative indexing from end
                    arr.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| arr.get(idx))
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
                    base_val.type_name(&self.registry),
                    idx_val.type_name(&self.registry)
                ),
            )),
        }
    }

    /// Evaluate field access.
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
                    base_val.type_name(&self.registry)
                ),
            )),
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
                None => Ok(Value::none()),
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
    #[async_recursion]
    async fn r#if(
        &mut self,
        cond: ExprId,
        then_br: ExprId,
        else_br: Option<ExprId>,
    ) -> Result<Value> {
        let cond_val = self.eval(cond).await?;
        if cond_val.is_truthy(&self.arena) {
            self.eval(then_br).await
        } else {
            match else_br {
                Some(e) => self.eval(e).await,
                None => Ok(Value::none()),
            }
        }
    }

    /// Execute a `LET` binding.
    #[async_recursion]
    async fn r#let(&mut self, name: &str, expr_id: ExprId) -> Result<()> {
        let span = self.ast.expr_span(expr_id).unwrap_or_default();
        let val = self.eval(expr_id).await?;
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
    fn test_interp(ast: &Ast) -> Interpreter<'_, TestIo> {
        let db = Database::in_memory().expect("in-memory db");
        Interpreter::new(ast, db, TestIo::new()).expect("interpreter")
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
            ast.add_stmt(Stmt::Let("x".into(), val), Span::new(0, 11));
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
        let let1 = ast.add_stmt(Stmt::Let("x".into(), val1), Span::new(0, 10));

        // Block expr with LET x = 20, returning x
        let val2 =
            ast.add_expr(Expr::Literal(Literal::Int(20)), Span::new(20, 22));
        let let2 = ast.add_stmt(Stmt::Let("x".into(), val2), Span::new(12, 22));
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
            Value::Array(ids) => {
                assert_eq!(ids.len(), 3);
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
        let let_result =
            ast.add_stmt(Stmt::Let("result".into(), zero), Span::new(0, 14));

        // IF true { LET result = 1 }
        let cond =
            ast.add_expr(Expr::Literal(Literal::Bool(true)), Span::new(3, 7));
        let one =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(25, 26));
        let set_one =
            ast.add_stmt(Stmt::Let("result".into(), one), Span::new(10, 26));
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
        let let_x = ast.add_stmt(Stmt::Let("x".into(), val), Span::new(22, 32));
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
        let let_x = ast.add_stmt(Stmt::Let("x".into(), v1), Span::new(0, 10));

        // LET y = 20
        let v2 =
            ast.add_expr(Expr::Literal(Literal::Int(20)), Span::new(20, 22));
        let let_y = ast.add_stmt(Stmt::Let("y".into(), v2), Span::new(12, 22));

        // LET sum = x + y
        let x = ast.add_expr(Expr::Var("x".into()), Span::new(34, 35));
        let y = ast.add_expr(Expr::Var("y".into()), Span::new(38, 39));
        let add =
            ast.add_expr(Expr::Binary(x, BinOp::Add, y), Span::new(34, 39));
        let let_sum =
            ast.add_stmt(Stmt::Let("sum".into(), add), Span::new(24, 39));

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
        assert!(result.is_none());
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

        let let_x =
            ast.add_stmt(Stmt::Let("x".into(), if_expr), Span::new(0, 35));
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
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn block_expr_with_stmts() {
        // { LET x = 10; x + 1 }
        let mut ast = Ast::new();
        let ten =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(10, 12));
        let let_x = ast.add_stmt(Stmt::Let("x".into(), ten), Span::new(2, 12));

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
            ast.add_stmt(Stmt::Let("x".into(), one), Span::new(0, 9));

        // inner block: { LET x = 10; x }
        let ten =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(22, 24));
        let let_inner =
            ast.add_stmt(Stmt::Let("x".into(), ten), Span::new(13, 24));
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
}
