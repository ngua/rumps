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
mod transaction;
mod types;
mod variant;

use std::collections::HashMap;

use async_recursion::async_recursion;
use ordered_float::OrderedFloat;
use rumps_storage::{Database, Transaction};
use smallvec::SmallVec;

use crate::ast::{
    Ast, AstTypeExpr, AstTypeExprId, BinOp, BindingPattern, Expr, ExprId,
    JsonAccessKey, JsonAccessKind, Literal, OutputFormat, OutputStmt,
    OutputTarget, Stmt, StmtId, TxnId, TypeDefAst, TypeParam, TypePattern,
    UnOp,
};
use crate::env::Environment;
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{
    CapturedEnv, FunctionDef, TypeExprArena, TypeExprId, TypeId, TypeRegistry,
    Value, ValueArena,
};
use crate::{Result, Span};

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

    /// Active transactions indexed by their unique ID.
    ///
    /// Each `TRANSACTION` block gets a unique `TxnId` during typecheck; DB
    /// operations use this ID to look up the correct transaction context.
    txns: HashMap<TxnId, Transaction>,

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

    /// Cache of compiled regex patterns (populated during typechecking).
    ///
    /// `Value::Regex(idx)` holds an index into this cache.
    regex_cache: Vec<regex::Regex>,

    /// Mapping from regex expression IDs to cache indices.
    regex_indices: HashMap<ExprId, u32>,
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
        let mut registry = TypeRegistry::new(&mut arena, &mut type_exprs);

        // Register user-defined types BEFORE resolution so the resolver can
        // convert `Status.Pending` to `Expr::Variant` for user types
        registry.register_from_ast(ast, stmts, &mut arena, &mut type_exprs);

        crate::resolve::resolve(ast, &mut arena, &registry);

        let env = Environment::new();

        // Run type checking after resolution
        let (regex_cache, regex_indices) = crate::typecheck::check(
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
            txns: HashMap::new(),
            arena,
            registry,
            regex_cache,
            regex_indices,
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
    /// bypassing name resolution and typechecking.
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
            txns: HashMap::new(),
            arena,
            registry,
            regex_cache: Vec::new(),
            regex_indices: HashMap::new(),
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
            .unwrap_or_else(|| invariant!("valid expression id"))
            .clone();

        match expr {
            Expr::Literal(lit) => Ok(self.literal(&lit)),
            Expr::Interpolation(parts) => self.interpolation(&parts).await,
            Expr::Var(name) => Ok(self.var(&name, span)),
            Expr::Get(ref dbref, txn_id) => self.get(dbref, txn_id, span).await,
            Expr::Data(ref dbref, txn_id) => self.data(dbref, txn_id).await,
            Expr::Order(ref dbref, txn_id) => {
                self.order(dbref, txn_id, span).await
            }
            Expr::Query(ref dbref, txn_id) => {
                self.query(dbref, txn_id, span).await
            }
            Expr::Binary(lhs, op, rhs) => self.binary(lhs, op, rhs, span).await,
            Expr::Unary(op, operand) => self.unary(op, operand, span).await,
            Expr::Call(callee, args) => self.call(callee, &args, span).await,
            Expr::Object(entries) => self.object(&entries, span).await,
            Expr::Array(elems) => self.array(&elems, span).await,
            Expr::Tuple(elems) => self.tuple(&elems, span).await,
            Expr::MapLit(entries) => self.map_lit(&entries, span).await,
            Expr::TupleIndex(base, idx) => self.tuple_index(base, idx).await,
            Expr::Index(base, idx) => self.index(base, idx, span).await,
            Expr::OptionalIndex(base, idx) => {
                self.optional_index(base, idx, span).await
            }
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
            Expr::Closure {
                params, ret, body, ..
            } => self.closure(&params, ret, body),
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
            Expr::Regex(_, _) => {
                // Look up the cache index set during typechecking
                let idx = self
                    .regex_indices
                    .get(&id)
                    .copied()
                    .unwrap_or_else(|| invariant!("regex compiled"));
                Ok(Value::Regex(idx))
            }
            Expr::Matches(lhs, rhs) => self.matches(lhs, rhs).await,
            Expr::Catch(expr_id, handler_id) => {
                self.catch(expr_id, handler_id, span).await
            }
            Expr::Output(output) => {
                self.output(&output).await?;
                Ok(Value::Unit)
            }
            Expr::Set(ref dbref, value, txn_id) => {
                self.set(dbref, value, txn_id, span).await
            }
            Expr::Kill(ref dbref, txn_id) => {
                self.kill(dbref, txn_id, span).await
            }
            Expr::Raise(inner) => {
                let val = self.eval(inner).await?;
                let msg = if let Value::String(id) = &val {
                    self.arena.get_str(*id).unwrap_or("").to_owned()
                } else {
                    self.stringify(&val)
                };
                Err(crate::Error::raise(span, msg))
            }
            Expr::Forever {
                seed,
                state_param,
                cont_param,
                body,
            } => {
                self.forever(seed, state_param, cont_param, body, span)
                    .await
            }
            Expr::Transaction(ref txn) => self.transaction(txn, span).await,
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
            .unwrap_or_else(|| invariant!("valid statement id"))
            .clone();

        match stmt {
            Stmt::Let(pat, ty_ann, expr_id) => {
                self.r#let(&pat, ty_ann, expr_id, span).await
            }
            Stmt::Set(ref dbref, expr_id, txn_id) => {
                self.set(dbref, expr_id, txn_id, span).await.map(|_| ())
            }
            Stmt::Kill(ref dbref, txn_id) => {
                self.kill(dbref, txn_id, span).await.map(|_| ())
            }
            Stmt::Output(output) => self.output(&output).await,
            Stmt::Expr(expr_id) => {
                // Evaluate for side effects, discard result
                self.eval(expr_id).await.map(|_| ())
            }
            Stmt::Fun {
                name,
                params,
                ret,
                body,
                ..
            } => self.fun(&name, &params, ret, body, span),
            Stmt::Type {
                name,
                type_params,
                def,
            } => self.type_decl(&name, &type_params, &def, span),
            Stmt::NewType {
                name,
                type_params,
                target,
            } => self.newtype_decl(&name, &type_params, target, span),
            Stmt::Union {
                name,
                type_params,
                members,
            } => self.union_decl(&name, &type_params, &members, span),
            Stmt::Module { name, body } => {
                self.user_module(&name, &body, span).await
            }
        }
    }

    /// Define a user module.
    ///
    /// Processes the module body, registering functions and constants in a
    /// `UserModule` structure. Nested modules are supported via recursion.
    async fn user_module(
        &mut self,
        name: &str,
        body: &[StmtId],
        span: Span,
    ) -> Result<()> {
        let mut module = crate::env::UserModule::default();
        self.populate_module(body, &mut module, name, span).await?;
        self.env.register_user_module(name, module);
        Ok(())
    }

    /// Populate a module with functions, constants, submodules, and types.
    ///
    /// Recursively processes statement IDs, adding items to the module.
    /// The `mod_path` tracks the qualified module path for type registration.
    #[async_recursion]
    async fn populate_module(
        &mut self,
        ids: &[StmtId],
        module: &mut crate::env::UserModule,
        mod_path: &str,
        span: Span,
    ) -> Result<()> {
        match ids.split_first() {
            None => Ok(()),
            Some((&id, rest)) => {
                let item_span = self.ast.stmt_span(id).unwrap_or(span);
                if let Some(stmt) = self.ast.get_stmt(id).cloned() {
                    match stmt {
                        Stmt::Fun {
                            name: fn_name,
                            params,
                            ret,
                            body: fn_body,
                            ..
                        } => {
                            let closure =
                                self.closure(&params, ret, fn_body)?;
                            let val_id = self.arena.add(closure, item_span);
                            module.functions.insert(fn_name, val_id);
                        }

                        Stmt::Let(ref pat, _, expr_id) => {
                            let val = self.eval(expr_id).await?;
                            let val_id = self.arena.add(val, item_span);
                            if let BindingPattern::Var(ref const_name) = pat {
                                module
                                    .constants
                                    .insert(const_name.clone(), val_id);
                            }
                        }

                        Stmt::Module {
                            name: sub_name,
                            body: sub_body,
                        } => {
                            let mut sub = crate::env::UserModule::default();
                            let sub_path = format!("{}.{}", mod_path, sub_name);
                            self.populate_module(
                                &sub_body, &mut sub, &sub_path, item_span,
                            )
                            .await?;
                            module.submodules.insert(sub_name, sub);
                        }

                        Stmt::Type {
                            name: type_name,
                            type_params,
                            def,
                        } => {
                            // Types are already registered with qualified names
                            // by register_from_ast. The idempotent type_decl
                            // will skip if already present.
                            let qname = format!("{}.{}", mod_path, type_name);
                            self.type_decl(
                                &qname,
                                &type_params,
                                &def,
                                item_span,
                            )?;
                        }

                        Stmt::NewType {
                            name: alias_name,
                            type_params,
                            target,
                        } => {
                            // Aliases are already registered with qualified names
                            // by register_from_ast. The idempotent newtype_decl
                            // will skip if already present.
                            let qname = format!("{}.{}", mod_path, alias_name);
                            self.newtype_decl(
                                &qname,
                                &type_params,
                                target,
                                item_span,
                            )?;
                        }

                        Stmt::Union {
                            name: union_name,
                            type_params,
                            members,
                        } => {
                            // Unions are already registered with qualified names
                            // by register_from_ast. The idempotent union_decl
                            // will skip if already present.
                            let qname = format!("{}.{}", mod_path, union_name);
                            self.union_decl(
                                &qname,
                                &type_params,
                                &members,
                                item_span,
                            )?;
                        }

                        // Other statements are rejected by the typechecker
                        _ => {}
                    }
                }
                self.populate_module(rest, module, mod_path, span).await
            }
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
        // Use try_resolve_type_expr to handle type parameters gracefully
        let resolved_params: Result<
            SmallVec<[(StringId, Option<TypeExprId>); 4]>,
        > = params
            .iter()
            .map(|(pname, ty)| {
                let pname_id = self.arena.intern(pname);
                let ty_id = ty
                    .map(|ast_id| {
                        let s = self.ast.type_expr_span(ast_id).unwrap_or(span);
                        self.try_resolve_type_expr(ast_id, s)
                    })
                    .transpose()?
                    .flatten();
                Ok((pname_id, ty_id))
            })
            .collect();

        // Resolve return type (returns None if it contains type parameters)
        let resolved_ret = ret
            .map(|ast_id| {
                let s = self.ast.type_expr_span(ast_id).unwrap_or(span);
                self.try_resolve_type_expr(ast_id, s)
            })
            .transpose()?
            .flatten();

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

    /// Register a user-defined sum type declaration.
    ///
    /// Processes `TYPE Name = Variant1 | Variant2(T) | ...` and registers
    /// the type in the type registry. Errors if a type with the same name
    /// already exists or if referenced types are undeclared.
    fn type_decl(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        def: &TypeDefAst,
        span: Span,
    ) -> Result<()> {
        let name_id = self.arena.intern(name);

        // Skip if already registered (from register_from_ast before type
        // checking). This makes type registration idempotent.
        if self.registry.lookup(name_id).is_none() {
            let TypeDefAst::Sum(variants) = def;

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

            // Intern type parameters (constraints are ignored at runtime)
            let type_param_ids: SmallVec<[StringId; 2]> = type_params
                .iter()
                .map(|tp| self.arena.intern(&tp.name))
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

        Ok(())
    }

    /// Register a type alias declaration.
    ///
    /// Processes `NEWTYPE Name = Type` and registers the alias in the type
    /// registry. The alias is transparent; `NEWTYPE I = Int` makes `I`
    /// interchangeable with `Int`.
    fn newtype_decl(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        target: AstTypeExprId,
        span: Span,
    ) -> Result<()> {
        let name_id = self.arena.intern(name);

        // Skip if already registered (idempotent)
        if self.registry.lookup(name_id).is_none() {
            // Validate target type references only declared type params
            self.validate_type_params(target, type_params, span)?;

            // Intern type parameters
            let type_param_ids: SmallVec<[StringId; 2]> = type_params
                .iter()
                .map(|tp| self.arena.intern(&tp.name))
                .collect();

            // Register the alias
            self.registry.register(
                crate::value::TypeDef::Alias {
                    name: name_id,
                    type_params: type_param_ids,
                    target,
                },
                name_id,
            );
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
        type_params: &[TypeParam],
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

            // Intern type parameters (constraints are ignored at runtime)
            let type_param_ids: SmallVec<[StringId; 2]> = type_params
                .iter()
                .map(|tp| self.arena.intern(&tp.name))
                .collect();

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
        declared: &[TypeParam],
        span: Span,
    ) -> Result<()> {
        self.ast.get_type_expr(ty_id).map_or(Ok(()), |ty| match ty {
            AstTypeExpr::Named(n) => {
                let name_id = self.arena.intern(n);
                let is_registered = self.registry.lookup(name_id).is_some();
                let is_declared = declared.iter().any(|tp| tp.name == *n);
                if is_registered || is_declared {
                    Ok(())
                } else {
                    typechecked!("type param", "declared")
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

    /// Evaluate string interpolation.
    ///
    /// Evaluates each part: literal strings pass through unchanged, expressions
    /// are stringified. Strings are passed through without quotes.
    ///
    /// Collects parts into a `Vec` then joins, avoiding `O(n^2)` allocations.
    async fn interpolation(&mut self, parts: &[ExprId]) -> Result<Value> {
        self.interpolation_collect(parts, Vec::with_capacity(parts.len()))
            .await
    }

    /// Accumulator helper for interpolation; collects strings then joins.
    #[async_recursion]
    async fn interpolation_collect(
        &mut self,
        parts: &[ExprId],
        mut acc: Vec<String>,
    ) -> Result<Value> {
        match parts.split_first() {
            None => {
                let joined = acc.join("");
                Ok(Value::String(self.arena.intern(&joined)))
            }
            Some((&id, rest)) => {
                let val = self.eval(id).await?;
                // Strings pass through unchanged; other values use stringify
                let s = match &val {
                    Value::String(sid) => self
                        .arena
                        .get_str(*sid)
                        .unwrap_or_else(|| invariant!("StringId in arena"))
                        .to_owned(),
                    other => self.stringify(other),
                };
                acc.push(s);
                self.interpolation_collect(rest, acc).await
            }
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
        // Use try_resolve_type_expr to handle type parameters gracefully
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
                        self.try_resolve_type_expr(ast_id, span)
                    })
                    .transpose()?
                    .flatten();
                Ok((name_id, ty_id))
            })
            .collect();

        // Resolve return type (returns None if it contains type parameters)
        let resolved_ret = ret
            .map(|ast_id| {
                let span = self.ast.type_expr_span(ast_id).unwrap_or_default();
                self.try_resolve_type_expr(ast_id, span)
            })
            .transpose()?
            .flatten();

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
    fn var(&mut self, name: &str, _span: Span) -> Value {
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
            .unwrap_or_else(|| typechecked!("var", "Defined"))
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
                            _ => typechecked!("&&", "Bool"),
                        }
                    }
                    _ => typechecked!("&&", "Bool"),
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
                            _ => typechecked!("||", "Bool"),
                        }
                    }
                    _ => typechecked!("||", "Bool"),
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
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(operand).await?;
        Ok(self.apply_unop(op, val, span))
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

        // Check for named types (like Storable, Int, etc.)
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
            // For compound types (tuples, objects, inline unions), the typechecker
            // only allows identity casts; just return the value unchanged
            Ok(val)
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
    ///
    /// Special case: when `expr` is an array literal and annotation is
    /// `Array[UnionType]`, constructs the array with the union element type
    /// instead of falling back to JSON for heterogeneous elements.
    #[async_recursion]
    async fn annotate(
        &mut self,
        expr: ExprId,
        ast_ty: AstTypeExprId,
        span: Span,
    ) -> Result<Value> {
        let expected_ty = self.resolve_type_expr(ast_ty, span)?;

        // Try special case: array literal with `Array[UnionType]`
        // Must clone elems since we can't hold AST borrow across await
        let special = (self.type_exprs.base_type(expected_ty)
            == Some(TypeId::ARRAY))
        .then_some(())
        .and_then(|_| self.ast.get_expr(expr))
        .and_then(|e| match e {
            Expr::Array(elems) => Some(elems.clone()),
            _ => None,
        })
        .and_then(|elems| {
            self.type_exprs
                .type_args(expected_ty)
                .and_then(|args| args.first().copied())
                .filter(|&ty| self.is_union_type(ty))
                .map(|elem_ty| (elems, elem_ty))
        });

        if let Some((elems, elem_ty)) = special {
            self.array_with_union_elem(&elems, elem_ty, span).await
        } else {
            let val = self.eval(expr).await?;
            self.validate_type(&val, expected_ty, span)?;
            Ok(self.refine_type(val, expected_ty))
        }
    }

    /// Execute a `LET` binding with destructuring.
    ///
    /// If a type annotation is present, validates that the value's type matches
    /// before destructuring.
    ///
    /// Special case: when RHS is an array literal and annotation is
    /// `Array[UnionType]`, constructs the array with the union element type
    /// instead of falling back to JSON for heterogeneous elements.
    #[async_recursion]
    async fn r#let(
        &mut self,
        pat: &BindingPattern,
        ty_ann: Option<AstTypeExprId>,
        expr_id: ExprId,
        span: Span,
    ) -> Result<()> {
        // Check type annotation if present (applies to the entire value)
        // Also refine UNKNOWN type parameters (e.g., empty array gets concrete
        // element type)
        let val = match ty_ann {
            Some(ast_ty_id) => {
                let expected_ty = self.resolve_type_expr(ast_ty_id, span)?;

                // Try special case: array literal with `Array[UnionType]`
                // Must clone elems since we can't hold AST borrow across await
                let special = (self.type_exprs.base_type(expected_ty)
                    == Some(TypeId::ARRAY))
                .then_some(())
                .and_then(|_| self.ast.get_expr(expr_id))
                .and_then(|e| match e {
                    Expr::Array(elems) => Some(elems.clone()),
                    _ => None,
                })
                .and_then(|elems| {
                    self.type_exprs
                        .type_args(expected_ty)
                        .and_then(|args| args.first().copied())
                        .filter(|&ty| self.is_union_type(ty))
                        .map(|elem_ty| (elems, elem_ty))
                });

                if let Some((elems, elem_ty)) = special {
                    self.array_with_union_elem(&elems, elem_ty, span).await?
                } else {
                    let val = self.eval(expr_id).await?;
                    self.validate_type(&val, expected_ty, span)?;
                    self.refine_type(val, expected_ty)
                }
            }
            None => self.eval(expr_id).await?,
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
            // Int -> Word coercion (type checker validates non-negative)
            (Value::Int(n), Some(TypeId::WORD), _) => Value::Word(*n as usize),
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

    /// Execute an `@OUTPUT` statement.
    ///
    /// Writes to stdout, stderr, or a file via the I/O context, with optional
    /// JSON formatting.
    #[async_recursion]
    async fn output(&mut self, output: &OutputStmt) -> Result<()> {
        let span = self.ast.expr_span(output.expr).unwrap_or_default();
        let val = self.eval(output.expr).await?;

        // Apply format
        let text = match output.format {
            OutputFormat::Default => self.display(&val),
            OutputFormat::Json => {
                let json = self.jsonify(&val);
                serde_json::to_string_pretty(&json)
                    .unwrap_or_else(|_| invariant!("JSON serializable"))
            }
        };

        // Write to target
        match output.target {
            OutputTarget::Stdout => self.io.stdoutline(&text, span).await,
            OutputTarget::Stderr => self.io.stderrline(&text, span).await,
            OutputTarget::File(path_expr) => {
                let path_val = self.eval(path_expr).await?;
                let path = self.filepath(&path_val);
                self.io.write(&path, &text, span).await
            }
        }
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
            let json_val = self.jsonify(&val);
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
                    _ => typechecked!("json[key]", "String | Int"),
                }
            }
        };

        // Access the JSON value
        let json_val = match &base_val {
            Value::Json(j) => j.get(&key_str).cloned(),
            _ => typechecked!("json access", "Json"),
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
            Some(serde_json::Value::Array(_)) => {
                typechecked!("json scalar access", "Scalar")
            }
            Some(serde_json::Value::Object(_)) => {
                typechecked!("json scalar access", "Scalar")
            }
        }
    }
}
