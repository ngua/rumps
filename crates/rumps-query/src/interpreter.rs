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
//!
//! # Name Resolution Architecture
//!
//! Type-qualified paths (e.g., `Option.None`, `Status.Pending`) are resolved
//! in two places:
//!
//! 1. **Parse-time** (`resolve.rs`): For built-in types (`Option`, `Result`)
//!    that exist before interpretation begins. Converts `Expr::Field` to
//!    `Expr::Variant` (regardless of arity).
//!
//! 2. **Runtime** (`field()`, `call()`): For user-defined types declared via
//!    `TYPE`. These are registered during interpretation, so they cannot be
//!    resolved at parse time. The interpreter checks if a field access like
//!    `Status.Pending` refers to a registered type and constructs the variant.
//!
//! This dual approach is necessary because user-defined types are declared
//! dynamically during script execution, after the parse-time resolution pass.

#![allow(dead_code)]

mod call;
mod collections;
mod control;
mod convert;
mod db;
mod modules;
mod ops;
mod pattern;
mod types;
mod variant;

use std::collections::HashMap;

use async_recursion::async_recursion;
use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use rumps_storage::{Database, Transaction};
use smallvec::SmallVec;

use crate::ast::{
    Ast, AstTypeExpr, AstTypeExprId, BinOp, BindingPattern, Expr, ExprId,
    JsonAccessKey, JsonAccessKind, Literal, Stmt, StmtId, TypeDefAst,
    TypePattern, UnOp,
};
use crate::env::Environment;
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{
    CapturedEnv, FunctionDef, TypeExprArena, TypeExprId, TypeId, TypeRegistry,
    Value, ValueArena,
};
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

    /// Variable environment for lexical `LET` bindings and built-in functions.
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
    /// registry, runs name resolution on the AST, runs type checking, and
    /// sets up the interpreter.
    ///
    /// Takes `&mut Ast` because resolution mutates it, but stores `&Ast`
    /// since interpretation only reads.
    pub(crate) fn new(
        ast: &'a mut Ast,
        stmts: &[StmtId],
        db: Database,
        io: I,
    ) -> Result<Self> {
        let mut arena = ValueArena::new();
        let mut type_exprs = TypeExprArena::new();
        let mut registry = TypeRegistry::new(&mut arena, &mut type_exprs)?;

        // Register user-defined types BEFORE resolution so the resolver can
        // convert `Status.Pending` to `Expr::Variant` for user types
        registry.register_from_ast(ast, stmts, &mut arena, &mut type_exprs)?;

        crate::resolve::resolve(ast, &mut arena, &registry);

        let env = Environment::new();

        // Run type checking after resolution
        crate::typecheck::check(
            ast,
            stmts,
            &registry,
            &type_exprs,
            &env,
            &arena,
            arena.interner(),
        )?;

        Ok(Self {
            ast,
            env,
            db,
            txn: None,
            arena,
            registry,
            type_exprs,
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
        type_exprs: TypeExprArena,
    ) -> Self {
        Self {
            ast,
            env: Environment::new(),
            db,
            txn: None,
            arena,
            registry,
            type_exprs,
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
            Expr::MapLit(entries) => self.map_lit(&entries, span).await,
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
            Expr::Match(scrutinee, arms) => {
                self.r#match(scrutinee, &arms, span).await
            }
            Expr::Closure { params, ret, body } => {
                self.closure(&params, ret, body)
            }
            Expr::Unwrap(inner) => {
                let val = self.eval(inner).await?;
                self.unwrap(val, span)
            }
            Expr::Range(start_id, end_id, inclusive) => {
                self.range(start_id, end_id, inclusive, span).await
            }
            Expr::Annotate(inner, ty) => self.annotate(inner, ty, span).await,
            Expr::Json(fields) => self.json(&fields, span).await,
            Expr::JsonAccess(base, kind, key) => {
                self.json_access(base, kind, &key, span).await
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
            Stmt::Let(pat, ty_ann, expr_id) => {
                self.r#let(&pat, ty_ann, expr_id, span).await
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
            Stmt::Type {
                name,
                type_params,
                def,
            } => self.type_decl(&name, &type_params, &def, span),
            Stmt::Union {
                name,
                type_params,
                members,
            } => self.union_decl(&name, &type_params, &members, span),
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

    /// Register a user-defined type declaration.
    ///
    /// Processes `TYPE Name = Variant1 | Variant2(T) | ...` (sum type) or
    /// `TYPE Name = { field: Type, ... }` (struct type) and registers
    /// the type in the type registry. Errors if a type with the same name
    /// already exists or if referenced types are undeclared.
    fn type_decl(
        &mut self,
        name: &str,
        type_params: &[String],
        def: &TypeDefAst,
        span: Span,
    ) -> Result<()> {
        let name_id = self.arena.intern(name);

        // Skip if already registered (from register_from_ast before type
        // checking). This makes type registration idempotent.
        if self.registry.lookup(name_id).is_none() {
            match def {
                TypeDefAst::Sum(variants) => {
                    // Validate payload types reference only declared type params
                    variants.iter().try_for_each(|v| {
                        v.payloads.iter().try_for_each(|ty_id| {
                            self.validate_type_params(*ty_id, type_params, span)
                        })
                    })?;

                    // Build VariantDef entries
                    let variant_defs: SmallVec<[crate::value::VariantDef; 4]> =
                        variants
                            .iter()
                            .enumerate()
                            .map(|(idx, v)| {
                                let vname_id = self.arena.intern(&v.name);
                                crate::value::VariantDef {
                                    name: vname_id,
                                    idx: idx as u8,
                                    arity: v.payloads.len() as u8,
                                    payloads: v.payloads.clone(),
                                }
                            })
                            .collect();

                    // Intern type parameters
                    let type_param_ids: SmallVec<[StringId; 2]> = type_params
                        .iter()
                        .map(|p| self.arena.intern(p))
                        .collect();

                    // Register the type
                    self.registry.register(
                        crate::value::TypeDef::Sum {
                            name: name_id,
                            type_params: type_param_ids,
                            variants: variant_defs,
                        },
                        name_id,
                    );
                }
                TypeDefAst::Struct(fields) => {
                    // Validate field types reference only declared type params
                    fields.iter().try_for_each(|(_, ty_id)| {
                        self.validate_type_params(*ty_id, type_params, span)
                    })?;

                    // Build field map with AST type expressions (not resolved);
                    // resolution happens at usage site with type param substitution
                    let field_map: IndexMap<StringId, AstTypeExprId> = fields
                        .iter()
                        .map(|(fname, ast_ty_id)| {
                            let fname_id = self.arena.intern(fname);
                            (fname_id, *ast_ty_id)
                        })
                        .collect();

                    // Intern type parameters
                    let type_param_ids: SmallVec<[StringId; 2]> = type_params
                        .iter()
                        .map(|p| self.arena.intern(p))
                        .collect();

                    // Register the struct type
                    self.registry.register(
                        crate::value::TypeDef::Struct {
                            name: name_id,
                            type_params: type_param_ids,
                            fields: field_map,
                        },
                        name_id,
                    );
                }
            }
        }

        Ok(())
    }

    /// Register a union type declaration.
    ///
    /// Union types define a set of types that a value can be.
    /// Example: `UNION Storable = Bool | Int | Float | Char | String | Json`
    fn union_decl(
        &mut self,
        name: &str,
        type_params: &[String],
        members: &[AstTypeExprId],
        span: Span,
    ) -> Result<()> {
        let name_id = self.arena.intern(name);

        // Skip if already registered (idempotent; type registered during type-check phase)
        if self.registry.lookup(name_id).is_some() {
            Ok(())
        } else {
            // Validate member types reference only declared type params
            members.iter().try_for_each(|m| {
                self.validate_type_params(*m, type_params, span)
            })?;

            // Resolve member types to TypeExprIds
            let member_exprs: Result<SmallVec<[TypeExprId; 8]>> = members
                .iter()
                .map(|&m| self.resolve_type_expr(m, span))
                .collect();

            // Intern type parameters
            let type_param_ids: SmallVec<[StringId; 2]> =
                type_params.iter().map(|p| self.arena.intern(p)).collect();

            // Register the union type
            self.registry.register(
                crate::value::TypeDef::Union {
                    name: name_id,
                    type_params: type_param_ids,
                    members: member_exprs?,
                },
                name_id,
            );

            Ok(())
        }
    }

    /// Validate that a type expression only references declared type parameters.
    ///
    /// For `Named` types, checks if the name is either a registered type or
    /// a declared type parameter. Recursively validates nested types.
    fn validate_type_params(
        &mut self,
        ty_id: AstTypeExprId,
        declared: &[String],
        span: Span,
    ) -> Result<()> {
        self.ast.get_type_expr(ty_id).map_or(Ok(()), |ty| match ty {
            AstTypeExpr::Named(n) => {
                let name_id = self.arena.intern(n);
                let is_registered = self.registry.lookup(name_id).is_some();
                let is_declared = declared.iter().any(|p| p == n);
                if is_registered || is_declared {
                    Ok(())
                } else {
                    Err(Error::runtime(
                        span,
                        format!("undeclared type parameter `{n}`"),
                    ))
                }
            }
            AstTypeExpr::App(_, args) => args.iter().try_for_each(|a| {
                self.validate_type_params(*a, declared, span)
            }),
            AstTypeExpr::Fn(params, ret) => {
                params.iter().try_for_each(|p| {
                    self.validate_type_params(*p, declared, span)
                })?;
                self.validate_type_params(*ret, declared, span)
            }
            AstTypeExpr::Tuple(elems) => elems.iter().try_for_each(|e| {
                self.validate_type_params(*e, declared, span)
            }),
            AstTypeExpr::Union(members) => members.iter().try_for_each(|m| {
                self.validate_type_params(*m, declared, span)
            }),
            AstTypeExpr::Object(fields) => {
                fields.iter().try_for_each(|(_, ty)| {
                    self.validate_type_params(*ty, declared, span)
                })
            }
        })
    }

    /// Convert an AST literal to a runtime value.
    fn literal(&mut self, lit: &Literal) -> Value {
        match lit {
            Literal::Bool(b) => Value::Bool(*b),
            Literal::Int(n) => Value::Int(*n),
            Literal::Float(f) => Value::Float(OrderedFloat(*f)),
            Literal::Char(c) => Value::Char(*c),
            Literal::String(s) => Value::String(self.arena.intern(s)),
            Literal::Null => Value::Json(serde_json::Value::Null),
            Literal::Unit => Value::Unit,
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
                            _ => Err(Error::runtime_type(
                                span,
                                format!(
                                    "logical AND requires booleans; got Bool and {}",
                                    right.type_name(&self.registry, &self.type_exprs, &self.arena)
                                ),
                            )),
                        }
                    }
                    _ => Err(Error::runtime_type(
                        span,
                        format!(
                            "logical AND requires booleans; got {}",
                            left.type_name(
                                &self.registry,
                                &self.type_exprs,
                                &self.arena
                            )
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
                            _ => Err(Error::runtime_type(
                                span,
                                format!(
                                    "logical OR requires booleans; got Bool and {}",
                                    right.type_name(&self.registry, &self.type_exprs, &self.arena)
                                ),
                            )),
                        }
                    }
                    _ => Err(Error::runtime_type(
                        span,
                        format!(
                            "logical OR requires booleans; got {}",
                            left.type_name(
                                &self.registry,
                                &self.type_exprs,
                                &self.arena
                            )
                        ),
                    )),
                }
            }
            // Coalesce: unwrap Option.Some/Result.Ok, or evaluate right for None/Err
            BinOp::Coalesce => {
                let left = self.eval(lhs).await?;
                self.coalesce(left, rhs).await
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

    /// Evaluate a unary operation.
    #[async_recursion]
    async fn unary(
        &mut self,
        op: UnOp,
        operand: ExprId,
        _span: Span,
    ) -> Result<Value> {
        let val = self.eval(operand).await?;
        Ok(self.apply_unop(op, &val))
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
    /// - `T -> Storable` (identity if T is a Storable member type)
    ///
    /// Note: `AS Storable` is the only infallible union cast. Other unions
    /// require `READ` for fallible conversion or `MATCH` for type narrowing.
    #[async_recursion]
    async fn r#as(
        &mut self,
        expr: ExprId,
        ast_ty: AstTypeExprId,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        let target_ty = self.resolve_type_expr(ast_ty, span)?;

        // Check for named union types (like Storable)
        if let Some(target_base) = self.type_exprs.base_type(target_ty) {
            // Special case: `AS Storable` is infallible if value is already Storable
            if target_base == TypeId::STORABLE
                && self.value_matches_type(&val, TypeId::STORABLE)
            {
                Ok(val)
            } else {
                self.coerce(&val, target_base, span)
            }
        } else {
            // For non-named types (function types, tuple types, inline unions),
            // AS is not supported; use READ instead
            Err(Error::runtime(
                span,
                "cannot use AS with compound types; use READ for fallible conversion",
            ))
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
        self.read_value_expr(&val, target_ty, span)
    }

    /// Evaluate a type annotation: `(expr) : Type`.
    ///
    /// Validates that the value matches the annotated type at runtime.
    /// Returns the value unchanged if it matches; errors otherwise.
    #[async_recursion]
    async fn annotate(
        &mut self,
        expr: ExprId,
        ast_ty: AstTypeExprId,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        let expected_ty = self.resolve_type_expr(ast_ty, span)?;
        self.validate_type(&val, expected_ty, span)?;
        Ok(self.refine_type(val, expected_ty))
    }

    /// Execute a `LET` binding with destructuring.
    ///
    /// If a type annotation is present, validates that the value's type matches
    /// before destructuring.
    #[async_recursion]
    async fn r#let(
        &mut self,
        pat: &BindingPattern,
        ty_ann: Option<AstTypeExprId>,
        expr_id: ExprId,
        span: Span,
    ) -> Result<()> {
        let val = self.eval(expr_id).await?;

        // Check type annotation if present (applies to the entire value)
        // Also refine UNKNOWN type parameters (e.g., empty array gets concrete element type)
        let val = match ty_ann {
            Some(ast_ty_id) => {
                let expected_ty = self.resolve_type_expr(ast_ty_id, span)?;
                self.validate_type(&val, expected_ty, span)?;
                self.refine_type(val, expected_ty)
            }
            None => val,
        };

        self.destructure(pat, &val, span)
    }

    /// Refine a value's internal type to match an annotation.
    ///
    /// For parameterized types like `Array` and `Map`, if the value has `UNKNOWN`
    /// type parameters (e.g., empty array), replaces them with concrete types
    /// from the annotation.
    fn refine_type(&self, val: Value, expected: TypeExprId) -> Value {
        let args = self.type_exprs.type_args(expected).map(SmallVec::as_slice);
        match (&val, self.type_exprs.base_type(expected), args) {
            (
                Value::Array(elem_ty, elems),
                Some(TypeId::ARRAY),
                Some(&[ann_elem]),
            ) => {
                if self.type_exprs.base_type(*elem_ty) == Some(TypeId::UNKNOWN)
                {
                    Value::Array(ann_elem, elems.clone())
                } else {
                    val
                }
            }
            (
                Value::Map(k_ty, v_ty, entries),
                Some(TypeId::MAP),
                Some(&[ann_k, ann_v]),
            ) => {
                let k = if self.type_exprs.base_type(*k_ty)
                    == Some(TypeId::UNKNOWN)
                {
                    ann_k
                } else {
                    *k_ty
                };
                let v = if self.type_exprs.base_type(*v_ty)
                    == Some(TypeId::UNKNOWN)
                {
                    ann_v
                } else {
                    *v_ty
                };
                if k != *k_ty || v != *v_ty {
                    Value::Map(k, v, entries.clone())
                } else {
                    val
                }
            }
            _ => val,
        }
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

    /// Evaluate a JSON object literal.
    ///
    /// Evaluates each field expression and converts to JSON via `jsonify`.
    /// Returns `Value::Json(Object)`.
    #[async_recursion]
    #[allow(clippy::while_let_on_iterator)]
    async fn json(
        &mut self,
        fields: &[(String, ExprId)],
        _span: Span,
    ) -> Result<Value> {
        let mut obj = serde_json::Map::new();
        // Process fields sequentially to maintain order
        let mut it = fields.iter();
        while let Some((key, expr_id)) = it.next() {
            let val = self.eval(*expr_id).await?;
            let json_val = self.jsonify(&val)?;
            obj.insert(key.clone(), json_val);
        }
        Ok(Value::Json(serde_json::Value::Object(obj)))
    }

    /// Evaluate JSON field access.
    ///
    /// For `JsonAccessKind::Json` (`.` or `->`): returns `Value::Json` (null for missing).
    /// For `JsonAccessKind::Scalar` (`..` or `->>`): returns `Option[scalar]`.
    #[async_recursion]
    async fn json_access(
        &mut self,
        base: ExprId,
        kind: JsonAccessKind,
        key: &JsonAccessKey,
        span: Span,
    ) -> Result<Value> {
        let base_val = self.eval(base).await?;

        // Get the key string
        let key_str = match key {
            JsonAccessKey::Field(name) => name.clone(),
            JsonAccessKey::Expr(expr_id) => {
                let key_val = self.eval(*expr_id).await?;
                match key_val {
                    Value::String(sid) => {
                        self.arena.get_str(sid).unwrap_or("").to_owned()
                    }
                    Value::Int(n) => n.to_string(),
                    _ => {
                        let ty = key_val.type_name(
                            &self.registry,
                            &self.type_exprs,
                            &self.arena,
                        );
                        Err(Error::runtime_type(
                            span,
                            format!("JSON key must be String or Int; got {ty}"),
                        ))?
                    }
                }
            }
        };

        // Access the JSON value
        let json_val = match &base_val {
            Value::Json(j) => j.get(&key_str).cloned(),
            _ => {
                let ty = base_val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                Err(Error::runtime_type(
                    span,
                    format!("JSON access requires Json; got {ty}"),
                ))?
            }
        };

        match kind {
            // `.` or `->`: return Json (null for missing)
            JsonAccessKind::Json => {
                Ok(Value::Json(json_val.unwrap_or(serde_json::Value::Null)))
            }
            // `..` or `->>`: extract scalar, return Option[T]
            JsonAccessKind::Scalar => {
                self.json_to_option_scalar(json_val, span)
            }
        }
    }

    /// Convert a JSON value to `Option[Scalar]`.
    ///
    /// Returns `Option[Scalar]` where `Scalar = Bool | Int | Float | String`:
    /// - `None` or `null` → `Option.None`
    /// - `bool` → `Option.Some(Bool)`
    /// - `number` → `Option.Some(Int)` or `Option.Some(Float)`
    /// - `string` → `Option.Some(String)`
    /// - `array`/`object` → runtime error
    fn json_to_option_scalar(
        &mut self,
        json: Option<serde_json::Value>,
        span: Span,
    ) -> Result<Value> {
        match json {
            None | Some(serde_json::Value::Null) => Ok(self.make_none_scalar()),
            Some(serde_json::Value::Bool(b)) => {
                let val_id = self.arena.add(Value::Bool(b), span);
                Ok(self.make_some_scalar(val_id))
            }
            Some(serde_json::Value::Number(n)) => {
                let val = n.as_i64().map_or_else(
                    || Value::Float(OrderedFloat(n.as_f64().unwrap_or(0.0))),
                    Value::Int,
                );
                let val_id = self.arena.add(val, span);
                Ok(self.make_some_scalar(val_id))
            }
            Some(serde_json::Value::String(s)) => {
                let sid = self.arena.intern(&s);
                let val_id = self.arena.add(Value::String(sid), span);
                Ok(self.make_some_scalar(val_id))
            }
            Some(serde_json::Value::Array(_)) => Err(Error::runtime_type(
                span,
                "JSON scalar access on array; use READ to convert",
            )),
            Some(serde_json::Value::Object(_)) => Err(Error::runtime_type(
                span,
                "JSON scalar access on object; use READ to convert",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
