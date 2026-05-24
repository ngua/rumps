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
//! human-readable string. Used for `write` statements and string concatenation
//! or interpolation.
//!
//! | *Type*   | *Result*                                              |
//! |----------|-------------------------------------------------------|
//! | `Bool`   | `"true"` or `"false"`                                 |
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
//! Storage conversion translates between runtime `Payload` and persistent
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
//!    `type`. These are registered during interpretation, so they cannot be
//!    resolved at parse time. The interpreter checks if a field access like
//!    `Status.Pending` refers to a registered type and constructs the variant.
//!
//! This dual approach is necessary because user-defined types are declared
//! dynamically during script execution, after the parse-time resolution pass.

#![allow(dead_code)]

mod call;
mod class;
mod collections;
mod control;
pub(crate) mod convert;
mod db;
mod hof;
mod hoist;
pub(crate) mod instance;
mod modules;
mod ops;
mod pattern;
mod transaction;
mod types;
mod variant;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_recursion::async_recursion;
use env::Environment;
use ordered_float::OrderedFloat;
use rumps_storage::{Database, Transaction};
use smallvec::SmallVec;

use crate::ast::{
    Ast, AstTypeExpr, AstTypeExprId, BinOp, BindingPattern, Expr, ExprId,
    Import, ImportItem, JsonAccessKey, JsonAccessKind, Literal, NumericLit,
    OutputFormat, OutputTarget, Stmt, StmtId, TxnId, TypeDefAst, TypeParam,
    TypePattern, UnOp, WriteExpr,
};
use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::io::IoContext;
use crate::resolve::{InstanceMap, ResolveCtx};
use crate::typecheck::{ExprAux, ExprInfo, RuntimeTyId};
use crate::value::{
    CapturedEnv, FunctionDef, Payload, TypeDef, TypeId, TypeRegistry,
    ValueArena, ValueId, ValueMeta, VariantDef,
};
use crate::{env, typecheck, ClassId, Error, Result, Span};

/// Resolved `read` target: either an object type (structural) or a named type.
enum ReadTarget {
    Object(indexmap::IndexMap<StringId, typecheck::TyId>),
    Ty(typecheck::TyId),
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

    /// Variable environment for lexical `let` bindings and built-in functions.
    env: Environment,

    /// Database for all `@set`/`@get` operations (owned).
    ///
    /// The interpreter is the natural owner when running `rumps path/to/db script.rumps`.
    /// `Database` is cheap to clone (internal `Arc`), so ownership has low overhead.
    db: Database,

    /// Active transactions indexed by their unique ID.
    ///
    /// Each `transaction` block gets a unique `TxnId` during typecheck; DB
    /// operations use this ID to look up the correct transaction context.
    txns: HashMap<TxnId, Transaction>,

    /// Arena for runtime values with string interning.
    arena: ValueArena,

    /// Type registry for runtime type information.
    registry: TypeRegistry,

    /// Registry of named functions (`fun` definitions).
    functions: HashMap<StringId, FunctionDef>,

    /// I/O context for output operations.
    io: I,

    /// Cache of compiled regex patterns (populated during typechecking).
    ///
    /// `Payload::Regex(idx)` holds an index into this cache.
    regex_cache: Vec<regex::Regex>,

    /// Type arena from typechecking; owns all interned `Ty` values referenced
    /// by `TyId` handles in `checked_exprs`.
    ty_arena: typecheck::TyArena,

    /// Runtime type layer; provides `ValueMeta` constructors for scalar types.
    runtime_types: typecheck::RuntimeTypes,

    /// Unified per-expression type metadata populated during typechecking.
    ///
    /// Replaces the previous separate maps (`numeric_types`, `mempty_types`,
    /// `convert_targets`, `wrap_types`, `bimap_output_types`, `regex_indices`,
    /// `instance_calls`, `resolved_instance_fns`, `naked_method_classes`).
    checked_exprs: HashMap<ExprId, ExprInfo>,

    /// Mapping from AST type expression IDs to their resolved `RuntimeTyId`s.
    ///
    /// Populated from the typechecker's `ast_type_map`; used for `IS` type
    /// patterns, `AS` casts, `READ` conversions, and match `IS` arms.
    ast_type_map: HashMap<AstTypeExprId, RuntimeTyId>,

    /// Maps alias `TypeId`s to their expanded underlying `TyId`.
    ///
    /// Used by `read` to resolve alias targets (e.g., `Person` -> `Object({name: String, age: Int})`).
    alias_expansions: HashMap<AstTypeExprId, typecheck::TyId>,

    /// Registry of class methods for dispatch.
    class_methods: class::ClassMethods,

    /// Registry of module-level HoFs for dispatch.
    module_hofs: hof::ModuleHofs,

    /// Registry of user-defined class instances for runtime dispatch.
    ///
    /// Populated from the typechecker's instance registry when `class`
    /// statements are processed.
    user_instances: instance::RuntimeInstanceRegistry,

    /// Class registry; carries class definitions indexed by `ClassId`.
    class_registry: typecheck::ClassRegistry,

    /// Resolved class instance information from the resolution pass.
    ///
    /// Used during hoisting to register instance methods as functions and
    /// populate `user_instances`. Keyed by `StmtId` so hoisting can look up
    /// the resolved info when processing `Stmt::ClassInstance`.
    resolved_instances: InstanceMap,
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
    /// since interpretation only reads. Set `interactive` to `true` to skip
    /// the `main` function requirement.
    pub(crate) fn new(
        ast: &'a mut Ast,
        stmts: &[StmtId],
        db: Database,
        io: I,
        interactive: bool,
        interner: StringInterner,
    ) -> Result<Self> {
        let mut arena = ValueArena::with_interner(interner);

        // Pre-intern builtin module names so all interner clones (Environment,
        // TypeEnv) share the same `StringId`s.
        env::BUILTIN_MODULE_NAMES.iter().for_each(|n| {
            arena.strings.intern(n);
        });

        let mut registry = TypeRegistry::new(&mut arena);

        // Register user-defined types BEFORE resolution so the resolver can
        // convert `Status.Pending` to `Expr::Variant` for user types
        registry.register_from_ast(ast, stmts, &mut arena);

        // Build a class registry for the resolve pass (name -> `ClassId` mapping).
        // Includes user-defined class stubs so the resolver can map class names
        // to `ClassId`s for instance resolution.
        let mut resolve_class_registry = {
            let mut tmp_arena = typecheck::TyArena::new();
            typecheck::ClassRegistry::builtins(
                &mut |s| arena.strings.intern(s),
                &mut tmp_arena,
            )
        };
        stmts.iter().for_each(|&id| {
            if let Some(Stmt::ClassDef { name, .. }) = ast.get_stmt(id).cloned()
            {
                let _ = resolve_class_registry.register(typecheck::ClassDef {
                    name,
                    shape: typecheck::ClassShape::Concrete { params: 0 },
                    assoc_types: Default::default(),
                    methods: vec![],
                    supers: Default::default(),
                });
            }
        });

        let resolved_instances = ResolveCtx::new(
            ast,
            &mut arena,
            &registry,
            &resolve_class_registry,
        )
        .resolve();

        let env = Environment::with_interner(arena.interner());

        // Run type checking after resolution
        let type_exprs = crate::value::TypeExprArena::new();
        let tc = typecheck::InferCtx::new(
            ast,
            &registry,
            &type_exprs,
            &env,
            arena.interner(),
            interactive,
        )
        .check(stmts, &registry, &arena)?;

        let module_hofs = hof::ModuleHofs::new(&mut arena.strings);
        let class_methods = {
            let mut cm = class::ClassMethods::new();
            cm.register_all(&mut arena.strings);
            cm
        };

        let mut ty_arena = tc.ty_arena;

        // Build the unified checked_exprs map from the typechecker output.
        // Base layer: all expression types from `expr_types`.
        let mut checked_exprs: HashMap<ExprId, ExprInfo> = tc
            .expr_types
            .iter()
            .map(|(&id, &ty)| {
                (
                    id,
                    ExprInfo {
                        ty: RuntimeTyId::from(ty),
                        aux: ExprAux::None,
                    },
                )
            })
            .collect();

        // Overlay side-map entries on top of the base layer.
        tc.numeric_types.iter().for_each(|(&id, &ty)| {
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        tc.mempty_types.iter().for_each(|(&id, &ty)| {
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        tc.wrap_types.iter().for_each(|(&id, &ty)| {
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        tc.convert_targets.iter().for_each(|(&id, &ty)| {
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        tc.bimap_output_types.iter().for_each(|(&id, &ty)| {
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::HofCall {
                        out: RuntimeTyId::from(ty),
                    },
                },
            );
        });

        tc.regex_indices.iter().for_each(|(&id, &idx)| {
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(typecheck::TyArena::REGEX),
                    aux: ExprAux::RegexIndex(idx),
                },
            );
        });

        tc.instance_calls.iter().for_each(|(&id, &tid)| {
            let recv = RuntimeTyId::from(ty_arena.named(tid, SmallVec::new()));
            let fun = tc.resolved_instance_fns.get(&id).copied();
            let ty = checked_exprs
                .get(&id)
                .map_or(RuntimeTyId::from(typecheck::TyArena::UNKNOWN), |e| {
                    e.ty
                });
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty,
                    aux: ExprAux::InstanceCall { recv, fun },
                },
            );
        });

        // Handle resolved_instance_fns entries that are NOT in instance_calls.
        // This can happen for `Expr::ClassMethod` with turbofish where the
        // receiver type is available from the value itself (Payload::Tagged).
        tc.resolved_instance_fns.iter().for_each(|(&id, &fun)| {
            if !tc.instance_calls.contains_key(&id) {
                let ty = checked_exprs.get(&id).map_or(
                    RuntimeTyId::from(typecheck::TyArena::UNKNOWN),
                    |e| e.ty,
                );
                checked_exprs.insert(
                    id,
                    ExprInfo {
                        ty,
                        aux: ExprAux::InstanceCall {
                            recv: RuntimeTyId::UNKNOWN,
                            fun: Some(fun),
                        },
                    },
                );
            }
        });

        tc.naked_method_classes.iter().for_each(|(&id, &class)| {
            let ty = checked_exprs
                .get(&id)
                .map_or(RuntimeTyId::from(typecheck::TyArena::UNKNOWN), |e| {
                    e.ty
                });
            checked_exprs.insert(
                id,
                ExprInfo {
                    ty,
                    aux: ExprAux::NakedMethod { class },
                },
            );
        });

        let ast_type_map: HashMap<AstTypeExprId, RuntimeTyId> = tc
            .ast_type_map
            .iter()
            .map(|(&k, &v)| (k, RuntimeTyId::from(v)))
            .collect();

        let alias_expansions = tc.alias_expansions;

        Ok(Self {
            ast,
            env,
            db,
            txns: HashMap::new(),
            arena,
            registry,
            regex_cache: tc.regex_cache,
            runtime_types: typecheck::RuntimeTypes::new(ty_arena.clone()),
            ty_arena,
            checked_exprs,
            functions: HashMap::new(),
            io,
            ast_type_map,
            alias_expansions,
            class_methods,
            module_hofs,
            user_instances: instance::RuntimeInstanceRegistry::new(),
            class_registry: tc.class_registry,
            resolved_instances,
        })
    }

    /// Run a program (a sequence of statements).
    ///
    /// Consumes and returns the interpreter, allowing continued use after execution.
    /// In interactive mode, executes all statements sequentially. In normal mode,
    /// executes declarations then calls `main`.
    pub(crate) async fn run(
        mut self,
        stmts: &[StmtId],
        interactive: bool,
    ) -> Result<Self> {
        // Pass 1: Hoist function and module declarations for forward references
        self.hoist_declarations(stmts).await?;

        // Auto-import the `Prelude` module so its members are in scope
        {
            let pid = self.arena.strings.intern(env::PRELUDE_MODULE);
            self.import(&Import::wildcard(pid), Span::default())?;
        }

        // Pass 2: Execute statements (mode-dependent)
        if interactive {
            self.stmts(stmts).await?
        } else {
            self.exec_declarations(stmts).await?;
            self.call_main().await?
        };
        Ok(self)
    }

    /// Execute declarations only (non-interactive mode).
    ///
    /// Executes `Let`, `Type`, `NewType`, `Union`, and `Import` statements.
    /// Skips `Expr` (rejected by typechecker), `Fun`, `Module`, and `ClassInstance`
    /// (already hoisted).
    #[async_recursion]
    async fn exec_declarations(&mut self, stmts: &[StmtId]) -> Result<()> {
        match stmts.split_first() {
            None => Ok(()),
            Some((&head, tail)) => {
                let span = self.ast.stmt_span(head).unwrap_or_default();
                let stmt = self
                    .ast
                    .get_stmt(head)
                    .unwrap_or_else(|| invariant!("valid statement id"))
                    .clone();

                match stmt {
                    Stmt::Let(pat, ty_ann, expr_id, _) => {
                        self.r#let(&pat, ty_ann, expr_id, span).await?
                    }
                    // Top-level Expr is rejected by typechecker in non-interactive mode
                    Stmt::Expr(_) => {
                        typechecked!("top-level Stmt::Expr", "inside main")
                    }
                    // Already hoisted
                    Stmt::Fun { .. }
                    | Stmt::Module { .. }
                    | Stmt::ClassInstance { .. }
                    | Stmt::ClassDef { .. } => {}
                    Stmt::Type {
                        name,
                        type_params,
                        def,
                        ..
                    } => {
                        let n = self.arena.strings.resolve(name);
                        self.type_decl(&n, &type_params, &def, span)?
                    }
                    Stmt::NewType {
                        name,
                        type_params,
                        target,
                        ..
                    } => {
                        let n = self.arena.strings.resolve(name);
                        self.newtype_decl(&n, &type_params, target, span)?
                    }
                    Stmt::Union {
                        name,
                        type_params,
                        members,
                        ..
                    } => {
                        let n = self.arena.strings.resolve(name);
                        self.union_decl(&n, &type_params, &members, span)?
                    }
                    Stmt::Import(ref import) => self.import(import, span)?,
                };
                self.exec_declarations(tail).await
            }
        }
    }

    /// Call the `main` function (non-interactive mode entry point).
    async fn call_main(&mut self) -> Result<()> {
        let main = self.arena.intern("main");
        let def = self
            .functions
            .get(&main)
            .cloned()
            .unwrap_or_else(|| typechecked!("main", "defined"));
        self.call_function(main, &def.params, def.body, &[], Span::default())
            .await
            .map(|_| ())
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
        mut arena: ValueArena,
        registry: TypeRegistry,
    ) -> Self {
        let module_hofs = hof::ModuleHofs::new(&mut arena.strings);
        let class_methods = {
            let mut cm = class::ClassMethods::new();
            cm.register_all(&mut arena.strings);
            cm
        };
        let mut ty_arena = typecheck::TyArena::new();
        let class_registry = typecheck::ClassRegistry::builtins(
            &mut |s| arena.strings.intern(s),
            &mut ty_arena,
        );
        Self {
            ast,
            env: Environment::with_interner(arena.interner()),
            db,
            txns: HashMap::new(),
            arena,
            registry,
            regex_cache: Vec::new(),
            runtime_types: typecheck::RuntimeTypes::new(ty_arena.clone()),
            ty_arena,
            checked_exprs: HashMap::new(),
            functions: HashMap::new(),
            io,
            ast_type_map: HashMap::new(),
            alias_expansions: HashMap::new(),
            class_methods,
            module_hofs,
            user_instances: instance::RuntimeInstanceRegistry::new(),
            resolved_instances: HashMap::new(),
            class_registry,
        }
    }

    /// Evaluate an expression.
    #[async_recursion]
    pub(crate) async fn eval(&mut self, id: ExprId) -> Result<Payload> {
        let span = self.ast.expr_span(id).unwrap_or_default();
        let expr = self
            .ast
            .get_expr(id)
            .unwrap_or_else(|| invariant!("valid expression id"))
            .clone();

        match expr {
            Expr::Literal(lit) => Ok(self.literal(id, &lit)),
            Expr::Interpolation(parts) => self.interpolation(&parts).await,
            Expr::Var(name) => {
                let n = self.arena.strings.resolve(name);
                Ok(self.var(&n, span))
            }
            Expr::Intrinsic(op, ref rt, val, txn_id) => {
                self.intrinsic(op, rt, val, txn_id, span).await
            }
            Expr::Binary(lhs, op, rhs) => {
                self.binary(id, lhs, op, rhs, span).await
            }
            Expr::Unary(op, operand) => self.unary(id, op, operand, span).await,
            Expr::Call(callee, args) => self.call(callee, &args, span).await,
            Expr::Object(entries) => self.object(&entries, span).await,
            Expr::Array(elems) => self.array(&elems, span).await,
            Expr::Tuple(elems) => self.tuple(&elems, span).await,
            Expr::MapLit(entries) => self.map_lit(&entries, span).await,
            Expr::TupleIndex(base, idx) => {
                self.tuple_index(base, idx, span).await
            }
            Expr::Index(base, idx) => self.index(base, idx, span).await,
            Expr::OptionalIndex(base, idx) => {
                self.optional_index(base, idx, span).await
            }
            Expr::Field(base, field) => self.field(base, &field, span).await,
            Expr::OptionalField(base, field) => {
                self.optional_field(base, &field, span).await
            }
            Expr::Variant(ref ty, var, ref args) => {
                self.variant(ty, var, args, span).await
            }
            Expr::Path(ref segments) => self.path(segments, span),
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
            } => {
                let p: Vec<(String, Option<AstTypeExprId>)> = params
                    .iter()
                    .map(|(pid, ty)| (self.arena.strings.resolve(*pid), *ty))
                    .collect();
                self.closure(&p, ret, body)
            }
            Expr::Postfix(op, inner) => {
                let val = self.eval(inner).await?;
                if self.checked_exprs.get(&id).is_some_and(|info| {
                    matches!(info.aux, ExprAux::InstanceCall { .. })
                }) {
                    let val_id =
                        self.arena.add_typed(val, self.expr_meta(inner), span);
                    let mid = self.arena.intern("unwrap");
                    self.dispatch_class_method(
                        Some(id),
                        ClassId::FALLIBLE,
                        mid,
                        &[val_id],
                        span,
                    )
                    .await
                } else {
                    self.postfix(op, val, span)
                }
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
                    .checked_exprs
                    .get(&id)
                    .and_then(|info| match info.aux {
                        ExprAux::RegexIndex(i) => Some(i),
                        _ => None,
                    })
                    .unwrap_or_else(|| invariant!("regex compiled"));
                Ok(Payload::Regex(idx))
            }
            Expr::Matches(lhs, rhs) => self.matches(lhs, rhs).await,
            Expr::Catch(expr_id, handler_id) => {
                self.catch(expr_id, handler_id, span).await
            }
            Expr::Write(output) => {
                self.write(&output).await?;
                Ok(Payload::Unit)
            }
            Expr::Raise(inner) => {
                let val = self.eval(inner).await?;
                let msg = if let Payload::String(id) = &val {
                    self.arena.get_str(*id).unwrap_or("").to_owned()
                } else {
                    self.stringify(&val)
                };
                Err(Error::raise(span, msg))
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
            Expr::Mempty => self.mempty(id, span),
            Expr::Ref(ref dbref) => self.ref_lit(dbref, span).await,
            Expr::ClassMethod(ref class, ref method, ref args) => {
                self.class_method_expr(id, *class, *method, args, span)
                    .await
            }
            Expr::ClassMethodRef(ref class, _, ref method) => {
                Ok(Payload::ClassMethodFn {
                    class: *class,
                    method: *method,
                    expr_id: Some(id),
                })
            }
            Expr::NakedClassMethod(ref method, ref args) => {
                let class =
                    match self.checked_exprs.get(&id).map(|info| &info.aux) {
                        Some(ExprAux::NakedMethod { class }) => *class,
                        _ => {
                            typechecked!("naked class method", "resolved class")
                        }
                    };
                self.class_method_expr(id, class, *method, args, span).await
            }
            Expr::NakedClassMethodRef(ref method) => {
                let class =
                    match self.checked_exprs.get(&id).map(|info| &info.aux) {
                        Some(ExprAux::NakedMethod { class }) => *class,
                        _ => typechecked!(
                            "naked class method ref",
                            "resolved class"
                        ),
                    };
                Ok(Payload::ClassMethodFn {
                    class,
                    method: *method,
                    expr_id: Some(id),
                })
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
            .unwrap_or_else(|| invariant!("valid statement id"))
            .clone();

        match stmt {
            Stmt::Let(pat, ty_ann, expr_id, _) => {
                self.r#let(&pat, ty_ann, expr_id, span).await
            }
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
            } => self.fun(name, &params, ret, body, span),
            Stmt::Type {
                name,
                type_params,
                def,
                ..
            } => {
                let n = self.arena.strings.resolve(name);
                self.type_decl(&n, &type_params, &def, span)
            }
            Stmt::NewType {
                name,
                type_params,
                target,
                ..
            } => {
                let n = self.arena.strings.resolve(name);
                self.newtype_decl(&n, &type_params, target, span)
            }
            Stmt::Union {
                name,
                type_params,
                members,
                ..
            } => {
                let n = self.arena.strings.resolve(name);
                self.union_decl(&n, &type_params, &members, span)
            }
            Stmt::Module { name, body } => {
                self.user_module(name, &body, span).await
            }
            Stmt::Import(ref import) => self.import(import, span),
            Stmt::ClassInstance { .. } | Stmt::ClassDef { .. } => {
                // Class instances and definitions are hoisted during
                // `hoist_declarations()`.
                Ok(())
            }
        }
    }

    /// Define a user module.
    ///
    /// Processes the module body, registering functions and constants in a
    /// `UserModule` structure. Nested modules are supported via recursion.
    ///
    /// A new scope is pushed before processing and popped after, so that
    /// closures defined in the module body can capture sibling bindings.
    async fn user_module(
        &mut self,
        name: StringId,
        body: &[StmtId],
        span: Span,
    ) -> Result<()> {
        let mut module = env::UserModule::default();
        let mod_path = self.arena.strings.resolve(name);
        self.env.scopes.push();
        let res = self
            .populate_module(body, &mut module, &mod_path, span)
            .await;
        self.env.scopes.pop();
        res?;
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
        module: &mut env::UserModule,
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
                            body: fn_body,
                            ..
                        } => {
                            let ps: SmallVec<[StringId; 4]> = params
                                .iter()
                                .map(|(pname, _)| *pname)
                                .collect();
                            module.functions.insert(
                                fn_name,
                                FunctionDef {
                                    name: fn_name,
                                    params: ps,
                                    body: fn_body,
                                },
                            );
                        }

                        Stmt::Let(ref pat, _, expr_id, _) => {
                            let val = self.eval(expr_id).await?;
                            let meta = self.expr_meta(expr_id);
                            let val_id =
                                self.arena.add_typed(val, meta, item_span);
                            if let BindingPattern::Var(ref const_name) = pat {
                                module.constants.insert(*const_name, val_id);
                                // Bind in scope so later `let` initializers can
                                // reference earlier constants.
                                self.env.scopes.bind(*const_name, val_id);
                            }
                        }

                        Stmt::Module {
                            name: sub_name,
                            body: sub_body,
                        } => {
                            let mut sub = env::UserModule::default();
                            let sub_path = format!(
                                "{}.{}",
                                mod_path,
                                self.arena.strings.resolve(sub_name)
                            );
                            // Push scope for nested module so its bindings
                            // don't leak into parent
                            self.env.scopes.push();
                            let res = self
                                .populate_module(
                                    &sub_body, &mut sub, &sub_path, item_span,
                                )
                                .await;
                            self.env.scopes.pop();
                            res?;
                            module.submodules.insert(sub_name, sub);
                        }

                        Stmt::Type {
                            name: type_name,
                            type_params,
                            def,
                            ..
                        } => {
                            // Types are already registered with qualified names
                            // by register_from_ast. The idempotent type_decl
                            // will skip if already present.
                            let tn = self.arena.strings.resolve(type_name);
                            let qname = format!("{}.{}", mod_path, tn);
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
                            ..
                        } => {
                            // Aliases are already registered with qualified names
                            // by register_from_ast. The idempotent newtype_decl
                            // will skip if already present.
                            let an = self.arena.strings.resolve(alias_name);
                            let qname = format!("{}.{}", mod_path, an);
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
                            ..
                        } => {
                            // Unions are already registered with qualified names
                            // by register_from_ast. The idempotent union_decl
                            // will skip if already present.
                            let un = self.arena.strings.resolve(union_name);
                            let qname = format!("{}.{}", mod_path, un);
                            self.union_decl(
                                &qname,
                                &type_params,
                                &members,
                                item_span,
                            )?;
                        }

                        Stmt::ClassInstance {
                            for_type, methods, ..
                        } => {
                            self.hoist_class_instance(
                                id, for_type, &methods, item_span,
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

    /// Execute an import statement.
    ///
    /// Binds imported names into the current scope. The typechecker has already
    /// validated that the module exists, members exist, and there are no
    /// conflicts, so no runtime errors are possible.
    fn import(&mut self, import: &Import, span: Span) -> Result<()> {
        // Collect exclusions for wildcard imports
        let excludes: HashSet<StringId> = import
            .items
            .iter()
            .filter_map(|item| match item {
                ImportItem::Exclude(name) => Some(*name),
                _ => None,
            })
            .collect();

        // Check for wildcard
        let has_wildcard = import
            .items
            .iter()
            .any(|item| matches!(item, ImportItem::Wildcard));

        // Check if this is a builtin module (or submodule like Math.Trig)
        if let Some(builtin) = self.env.get_builtin_module_by_path(&import.path)
        {
            // Builtin module
            if has_wildcard {
                // Get all members and bind them (except exclusions)
                let members: Vec<StringId> = builtin
                    .public_members()
                    .into_iter()
                    .map(|(name, _)| name)
                    .filter(|name| !excludes.contains(name))
                    .collect();

                members.iter().for_each(|&name| {
                    self.bind_module_member(&import.path, name, name, span);
                });
            }

            // Process named imports
            import.items.iter().for_each(|item| {
                if let ImportItem::Named { name, alias } = item {
                    let bs = alias.unwrap_or(*name);
                    self.bind_module_member(&import.path, *name, bs, span);
                }
            });
        } else {
            // User module
            if has_wildcard {
                // Get all public members and bind them (except exclusions)
                let members: Vec<(StringId, bool)> = self
                    .env
                    .get_user_module(&import.path)
                    .map(|m| {
                        let fns = m
                            .functions
                            .keys()
                            .map(|n| (*n, true))
                            .filter(|(n, _)| !excludes.contains(n));
                        let consts = m
                            .constants
                            .keys()
                            .map(|n| (*n, false))
                            .filter(|(n, _)| !excludes.contains(n));
                        fns.chain(consts).collect()
                    })
                    .unwrap_or_default();

                members.iter().for_each(|&(name, is_fn)| {
                    self.bind_user_module_member(
                        &import.path,
                        name,
                        name,
                        is_fn,
                        span,
                    );
                });
            }

            // Process named imports
            import.items.iter().for_each(|item| {
                if let ImportItem::Named { name, alias } = item {
                    let bs = alias.unwrap_or(*name);
                    // Check if it's a function or constant
                    let is_fn = self
                        .env
                        .get_user_module(&import.path)
                        .map(|m| m.functions.contains_key(name))
                        .unwrap_or(false);
                    self.bind_user_module_member(
                        &import.path,
                        *name,
                        bs,
                        is_fn,
                        span,
                    );
                }
            });
        }

        Ok(())
    }

    /// Bind a builtin module member to the current scope.
    fn bind_module_member(
        &mut self,
        path: &[StringId],
        name: StringId,
        bind_as: StringId,
        span: Span,
    ) {
        let mut full_path: SmallVec<[StringId; 4]> = path.into();
        full_path.push(name);

        let val = if self.env.module_const_exists(&full_path) {
            Payload::ModuleConst { path: full_path }
        } else {
            Payload::ModuleFn { path: full_path }
        };
        let val_id = self.arena.add_typed(val, ValueMeta::untyped(), span);
        self.env.scopes.bind(bind_as, val_id);
    }

    /// Bind a user module member to the current scope.
    fn bind_user_module_member(
        &mut self,
        path: &[StringId],
        name: StringId,
        bind_as: StringId,
        is_fn: bool,
        span: Span,
    ) {
        if is_fn {
            // For functions, create a ModuleFn reference
            let mut full_path: SmallVec<[StringId; 4]> = path.into();
            full_path.push(name);
            let val = Payload::ModuleFn { path: full_path };
            let val_id = self.arena.add_typed(val, ValueMeta::untyped(), span);
            self.env.scopes.bind(bind_as, val_id);
        } else {
            // For constants, look up the ValueId and bind it directly
            let mut full_path: SmallVec<[StringId; 4]> = path.into();
            full_path.push(name);
            if let Some(const_id) = self.env.get_user_module_const(&full_path) {
                self.env.scopes.bind(bind_as, const_id);
            }
        }
    }

    /// Define a named function.
    ///
    /// Registers the function in the function registry. The function is
    /// immediately available for recursive calls.
    fn fun(
        &mut self,
        name: StringId,
        params: &[(StringId, Option<AstTypeExprId>)],
        _ret: Option<AstTypeExprId>,
        body: ExprId,
        _span: Span,
    ) -> Result<()> {
        let ps: SmallVec<[StringId; 4]> =
            params.iter().map(|(pname, _)| *pname).collect();

        self.functions.insert(
            name,
            FunctionDef {
                name,
                params: ps,
                body,
            },
        );

        Ok(())
    }

    /// Register a user-defined sum type declaration.
    ///
    /// Processes `type Name = Variant1 | Variant2(T) | ...` and registers
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
        if self
            .registry
            .lookup(&QualifiedName::local(name_id))
            .is_none()
        {
            let TypeDefAst::Sum(variants) = def;

            // Validate payload types reference only declared type params
            variants.iter().try_for_each(|v| {
                v.payloads.iter().try_for_each(|ty_id| {
                    self.validate_type_params(*ty_id, type_params, span)
                })
            })?;

            // Build VariantDef entries
            let variant_defs: SmallVec<[VariantDef; 4]> = variants
                .iter()
                .enumerate()
                .map(|(idx, v)| VariantDef {
                    name: v.name,
                    idx: idx as u8,
                    arity: v.payloads.len() as u8,
                    payloads: v.payloads.clone(),
                })
                .collect();

            // Type parameters already have `StringId` names
            let type_param_ids: SmallVec<[StringId; 2]> =
                type_params.iter().map(|tp| tp.name).collect();

            // Register the type
            self.registry.register(
                TypeDef::Sum {
                    name: name_id,
                    type_params: type_param_ids,
                    variants: variant_defs,
                },
                name_id.into(),
            );
        }

        Ok(())
    }

    /// Register a type alias declaration.
    ///
    /// Processes `newtype Name = Type` and registers the alias in the type
    /// registry. The alias is transparent; `newtype I = Int` makes `I`
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
        if self
            .registry
            .lookup(&QualifiedName::local(name_id))
            .is_none()
        {
            // Validate target type references only declared type params
            self.validate_type_params(target, type_params, span)?;

            // Type parameters already have `StringId` names
            let type_param_ids: SmallVec<[StringId; 2]> =
                type_params.iter().map(|tp| tp.name).collect();

            // Register the alias
            self.registry.register(
                TypeDef::Alias {
                    name: name_id,
                    type_params: type_param_ids,
                    target,
                },
                name_id.into(),
            );
        }

        Ok(())
    }

    /// Register a union type declaration.
    ///
    /// Union types define a set of types that a value can be.
    /// Example: `union Storable = Bool | Int | Float | Char | String | Json`
    fn union_decl(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        members: &[AstTypeExprId],
        span: Span,
    ) -> Result<()> {
        let name_id = self.arena.intern(name);

        // Skip if already registered (idempotent; type registered during type-check phase)
        if self
            .registry
            .lookup(&QualifiedName::local(name_id))
            .is_some()
        {
            Ok(())
        } else {
            // Validate member types reference only declared type params
            members.iter().try_for_each(|m| {
                self.validate_type_params(*m, type_params, span)
            })?;

            // Resolve member AST types to `TypeId`s
            let member_ids: SmallVec<[TypeId; 8]> = members
                .iter()
                .filter_map(|&m| {
                    self.ast.get_type_expr(m).and_then(|te| match te {
                        AstTypeExpr::Named(n) => self.registry.lookup(n),
                        _ => None,
                    })
                })
                .collect();

            let type_param_ids: SmallVec<[StringId; 2]> =
                type_params.iter().map(|tp| tp.name).collect();

            self.registry.register(
                TypeDef::Union {
                    name: name_id,
                    type_params: type_param_ids,
                    members: member_ids,
                    member_exprs: members.iter().copied().collect(),
                },
                name_id.into(),
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
            // Wildcard doesn't need validation
            AstTypeExpr::Wildcard => Ok(()),
            AstTypeExpr::Named(n) => {
                let is_registered = self.registry.lookup(n).is_some();
                let is_declared =
                    declared.iter().any(|tp| tp.name == n.local_name());
                if is_registered || is_declared {
                    Ok(())
                } else {
                    typechecked!("type param", "declared")
                }
            }
            AstTypeExpr::App(_, args) | AstTypeExpr::VarApp(_, args) => {
                args.iter().try_for_each(|a| {
                    self.validate_type_params(*a, declared, span)
                })
            }
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
            // Associated types: class name is just a string, nothing to validate
            AstTypeExpr::AssocType { .. } => Ok(()),
            AstTypeExpr::TupleConstructor { .. } => Ok(()),
        })
    }

    /// Add a value to the arena with explicit type metadata.
    fn add_val(&mut self, v: Payload, meta: ValueMeta, span: Span) -> ValueId {
        self.arena.add_typed(v, meta, span)
    }

    /// Look up the `ExprInfo` for `id` and produce a `ValueMeta`.
    ///
    /// Falls back to `ValueMeta::untyped()` if no entry exists (e.g. for
    /// internally-generated expressions with no corresponding AST node).
    fn expr_meta(&self, id: ExprId) -> ValueMeta {
        self.checked_exprs
            .get(&id)
            .map_or(ValueMeta::untyped(), |info| {
                self.runtime_types.meta(info.ty)
            })
    }

    /// Convert an AST literal to a runtime value.
    ///
    /// For numeric literals, looks up the resolved type from the type checker
    /// to convert to the correct runtime type (`Int`, `Word`, or `Float`).
    fn literal(&mut self, id: ExprId, lit: &Literal) -> Payload {
        match lit {
            Literal::Bool(b) => Payload::Bool(*b),
            Literal::Numeric(n) => {
                // Look up the resolved type from typechecking
                let ty = self
                    .checked_exprs
                    .get(&id)
                    .map(|info| self.ty_arena.get(info.ty.raw()));
                match (n, ty) {
                    // Integer literals are polymorphic over Int/Word/Float
                    (NumericLit::Int(v), Some(typecheck::Ty::Int)) => {
                        Payload::Int(*v)
                    }
                    (NumericLit::Int(v), Some(typecheck::Ty::Word)) => {
                        Payload::Word(*v as usize)
                    }
                    (NumericLit::Int(v), Some(typecheck::Ty::Float)) => {
                        Payload::Float(OrderedFloat(*v as f64))
                    }
                    // Float literals are NOT polymorphic; always Float
                    (NumericLit::Float(v), _) => {
                        Payload::Float(OrderedFloat(*v))
                    }
                    // Default integer literal to Int if type not found
                    (NumericLit::Int(v), _) => Payload::Int(*v),
                }
            }
            Literal::Char(c) => Payload::Char(*c),
            Literal::String(s) => Payload::String(self.arena.intern(s)),
            Literal::Null => Payload::Json(Arc::new(serde_json::Value::Null)),
            Literal::Unit => Payload::Unit,
        }
    }

    /// Convert an AST literal to a runtime value for pattern matching.
    ///
    /// For numeric literals, infers the type from the scrutinee value being
    /// matched against. This allows `match w { 10 => ... }` to work when
    /// `w` is a `Word`.
    pub(crate) fn pattern_literal(
        &mut self,
        lit: &Literal,
        scrutinee: &Payload,
    ) -> Payload {
        match lit {
            Literal::Bool(b) => Payload::Bool(*b),
            Literal::Numeric(n) => {
                // Infer type from the scrutinee being matched.
                // Integer literals adapt to scrutinee; float literals stay Float.
                match (n, scrutinee) {
                    (NumericLit::Int(v), Payload::Int(_)) => Payload::Int(*v),
                    (NumericLit::Int(v), Payload::Word(_)) => {
                        Payload::Word(*v as usize)
                    }
                    (NumericLit::Int(v), Payload::Float(_)) => {
                        Payload::Float(OrderedFloat(*v as f64))
                    }
                    // Float literals are NOT polymorphic; always Float
                    (NumericLit::Float(v), _) => {
                        Payload::Float(OrderedFloat(*v))
                    }
                    // Default integer literal to its natural type
                    (NumericLit::Int(v), _) => Payload::Int(*v),
                }
            }
            Literal::Char(c) => Payload::Char(*c),
            Literal::String(s) => Payload::String(self.arena.intern(s)),
            Literal::Null => Payload::Json(Arc::new(serde_json::Value::Null)),
            Literal::Unit => Payload::Unit,
        }
    }

    /// Evaluate string interpolation.
    ///
    /// Evaluates each part: literal strings pass through unchanged, expressions
    /// are stringified. Strings are passed through without quotes.
    ///
    /// Collects parts into a `Vec` then joins, avoiding `O(n^2)` allocations.
    async fn interpolation(&mut self, parts: &[ExprId]) -> Result<Payload> {
        self.interpolation_collect(parts, Vec::with_capacity(parts.len()))
            .await
    }

    /// Accumulator helper for interpolation; collects strings then joins.
    #[async_recursion]
    async fn interpolation_collect(
        &mut self,
        parts: &[ExprId],
        mut acc: Vec<String>,
    ) -> Result<Payload> {
        match parts.split_first() {
            None => {
                let joined = acc.join("");
                Ok(Payload::String(self.arena.intern(&joined)))
            }
            Some((&id, rest)) => {
                let val = self.eval(id).await?;
                // Strings pass through unchanged; other values use stringify
                let s = match &val {
                    Payload::String(sid)
                        if !matches!(
                            self.runtime_types.get(self.expr_meta(id).ty),
                            typecheck::Ty::Named(tid, _)
                                if self.registry.get_def(*tid).is_some_and(
                                    |def| matches!(def, TypeDef::Alias { .. })
                                )
                        ) =>
                    {
                        self.arena
                            .get_str(*sid)
                            .unwrap_or_else(|| invariant!("StringId in arena"))
                            .to_owned()
                    }
                    other => self.stringify(other),
                };
                acc.push(s);
                self.interpolation_collect(rest, acc).await
            }
        }
    }

    /// Create a closure value from AST closure parameters and body.
    ///
    /// Captures the current lexical environment by value.
    fn closure(
        &mut self,
        params: &[(String, Option<AstTypeExprId>)],
        _ret: Option<AstTypeExprId>,
        body: ExprId,
    ) -> Result<Payload> {
        let env = CapturedEnv::capture(self.env.scopes.stack());
        let ps: SmallVec<[StringId; 4]> = params
            .iter()
            .map(|(name, _)| self.arena.intern(name))
            .collect();

        Ok(Payload::Closure {
            params: ps,
            body,
            env: Arc::new(env),
        })
    }

    /// Evaluate a lexical variable reference (`let` bindings only).
    ///
    /// Does NOT fall back to B-tree locals; use `@get` for those.
    fn var(&mut self, name: &str, _span: Span) -> Payload {
        let name_id = self.arena.intern(name);

        // First try lexical scope
        let val = self
            .env
            .scopes
            .lookup(name_id)
            .and_then(|val_id| self.arena.get(val_id).cloned())
            .or_else(|| {
                // If not in scope, check if it's a named function
                self.functions.get(&name_id).map(|def| Payload::Function {
                    name: def.name,
                    params: def.params.clone(),
                    body: def.body,
                })
            })
            .unwrap_or_else(|| typechecked!("var", "Defined"));

        // Resolve ModuleConst to actual value from env.consts
        self.resolve_module_const(val)
    }

    /// Resolve a `ModuleConst` to its actual value.
    ///
    /// If the value is a `ModuleConst`, looks up the path in `env.consts`.
    /// Otherwise returns the value unchanged.
    fn resolve_module_const(&self, val: Payload) -> Payload {
        match val {
            Payload::ModuleConst { ref path } => self
                .env
                .get_module_const(path)
                .and_then(|id| self.env.consts.get(id).cloned())
                .unwrap_or(val),
            other => other,
        }
    }

    /// Evaluate a binary operation.
    ///
    /// Handles short-circuit evaluation for `AND`, `OR`, and `Coalesce`.
    /// For class-dispatched operators (`==`, `+`, `<`, etc.), checks for
    /// user-defined class instances before falling through to builtin dispatch.
    #[async_recursion]
    async fn binary(
        &mut self,
        id: ExprId,
        lhs: ExprId,
        op: BinOp,
        rhs: ExprId,
        span: Span,
    ) -> Result<Payload> {
        match op {
            // Short-circuit AND: if left is false, don't evaluate right
            BinOp::And => {
                let left = self.eval(lhs).await?;
                match left {
                    Payload::Bool(false) => Ok(Payload::Bool(false)),
                    Payload::Bool(true) => {
                        let right = self.eval(rhs).await?;
                        match right {
                            Payload::Bool(b) => Ok(Payload::Bool(b)),
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
                    Payload::Bool(true) => Ok(Payload::Bool(true)),
                    Payload::Bool(false) => {
                        let right = self.eval(rhs).await?;
                        match right {
                            Payload::Bool(b) => Ok(Payload::Bool(b)),
                            _ => typechecked!("||", "Bool"),
                        }
                    }
                    _ => typechecked!("||", "Bool"),
                }
            }
            // Coalesce: unwrap Option.Some/Result.Ok, or evaluate right for None/Err
            BinOp::Coalesce => {
                let left = self.eval(lhs).await?;
                if self.checked_exprs.get(&id).is_some_and(|info| {
                    matches!(info.aux, ExprAux::InstanceCall { .. })
                }) {
                    let val_id =
                        self.arena.add_typed(left, self.expr_meta(lhs), span);
                    let mid = self.arena.intern("unwrap");
                    match self
                        .dispatch_class_method(
                            Some(id),
                            ClassId::FALLIBLE,
                            mid,
                            &[val_id],
                            span,
                        )
                        .await
                    {
                        Ok(v) => Ok(v),
                        Err(e) if e.runtime_variant().is_some() => {
                            self.eval(rhs).await
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    self.coalesce(left, rhs).await
                }
            }
            // Pipeline: both sides evaluated, but requires async function call
            BinOp::Pipe => {
                let left = self.eval(lhs).await?;
                let right = self.eval(rhs).await?;
                self.pipeline(left, right, span).await
            }
            // All other operators: both sides evaluated, sync computation
            // (unless a user-defined class instance exists, which requires
            // async function invocation).
            _ => {
                let left = self.eval(lhs).await?;
                let right = self.eval(rhs).await?;
                if self.checked_exprs.get(&id).is_some_and(|info| {
                    matches!(info.aux, ExprAux::InstanceCall { .. })
                }) {
                    self.dispatch_binop_user(id, &left, op, &right, span).await
                } else {
                    self.apply_binop(&left, op, &right, span)
                }
            }
        }
    }

    /// Evaluate a unary operation.
    #[async_recursion]
    async fn unary(
        &mut self,
        id: ExprId,
        op: UnOp,
        operand: ExprId,
        span: Span,
    ) -> Result<Payload> {
        let val = self.eval(operand).await?;
        if matches!(op, UnOp::Wrap)
            && self.checked_exprs.get(&id).is_some_and(|info| {
                matches!(info.aux, ExprAux::InstanceCall { .. })
            })
        {
            let val_id =
                self.arena.add_typed(val, self.expr_meta(operand), span);
            let mid = self.arena.intern("wrap");
            self.dispatch_class_method(
                Some(id),
                ClassId::WRAPPABLE,
                mid,
                &[val_id],
                span,
            )
            .await
        } else {
            self.apply_unop(id, op, val, span)
        }
    }

    /// Evaluate a type check: `expr is Pattern`.
    ///
    /// Returns `true` if the value matches the pattern, `false` otherwise.
    /// For `VariantBind` patterns, bindings are NOT created here; they are
    /// handled specially by `if_with_bindings` when used as an `if` condition.
    #[async_recursion]
    async fn is(
        &mut self,
        expr: ExprId,
        pattern: &TypePattern,
        span: Span,
    ) -> Result<Payload> {
        let val = self.eval(expr).await?;
        let checked = self.checked_exprs.get(&expr).map(|e| e.ty);
        let matched = self.check_pattern(&val, checked, pattern, span)?;
        Ok(Payload::Bool(matched))
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
    /// Note: `as Storable` is the only infallible union cast. Other unions
    /// require `read` for fallible conversion or `match` for type narrowing.
    #[async_recursion]
    async fn r#as(
        &mut self,
        expr: ExprId,
        ast_ty: AstTypeExprId,
        span: Span,
    ) -> Result<Payload> {
        let val = self.eval(expr).await?;
        let rty = self
            .ast_type_map
            .get(&ast_ty)
            .copied()
            .unwrap_or(RuntimeTyId::UNKNOWN);

        if let Some(target_base) = self.runtime_types.to_type_id(rty) {
            let val_ty = self.payload_runtime_ty(&val);
            let storable = RuntimeTyId::from(typecheck::TyArena::STORABLE);
            if target_base == TypeId::STORABLE
                && self.runtime_types.matches(val_ty, val_ty, storable)
            {
                Ok(val)
            } else {
                self.coerce(&val, target_base, span)
            }
        } else {
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
    ) -> Result<Payload> {
        let val = self.eval(expr).await?;
        let rty =
            self.ast_type_map.get(&ast_ty).copied().unwrap_or_else(|| {
                typechecked!("read", "resolved type in ast_type_map")
            });

        match self.resolve_read_target(ast_ty, rty) {
            ReadTarget::Object(fields) => {
                self.read_to_object(&val, &fields, span)
            }
            ReadTarget::Ty(target) => self.read_ty_value(&val, target, span),
        }
    }

    /// Resolve a `RuntimeTyId` to a `ReadTarget`, handling alias transparency.
    fn resolve_read_target(
        &self,
        ast_ty: AstTypeExprId,
        rty: RuntimeTyId,
    ) -> ReadTarget {
        match self.runtime_types.get(rty) {
            typecheck::Ty::Object(fields) => ReadTarget::Object(fields.clone()),
            typecheck::Ty::Named(_, _) => self
                .alias_expansions
                .get(&ast_ty)
                .and_then(|&expanded| match self.ty_arena.get(expanded) {
                    typecheck::Ty::Object(fields) => {
                        Some(ReadTarget::Object(fields.clone()))
                    }
                    _ => None,
                })
                .unwrap_or(ReadTarget::Ty(rty.raw())),
            _ => ReadTarget::Ty(rty.raw()),
        }
    }

    /// Read using an exact type, preserving type arguments.
    fn read_ty_value(
        &mut self,
        val: &Payload,
        target: typecheck::TyId,
        span: Span,
    ) -> Result<Payload> {
        match self.ty_arena.get(target).clone() {
            typecheck::Ty::Array(elem) => {
                self.read_array_value(val, elem, span)
            }
            typecheck::Ty::Option(inner) => {
                self.read_option_value(val, inner, span)
            }
            typecheck::Ty::Object(fields) => {
                self.read_to_object(val, &fields, span)
            }
            ty => {
                let mid = self.arena.intern("try-into");
                self.dispatch_convert(ClassId::TRY_INTO, mid, val, &ty, span)
            }
        }
    }

    /// Read a JSON array as `Array[T]`.
    fn read_array_value(
        &mut self,
        val: &Payload,
        elem: typecheck::TyId,
        span: Span,
    ) -> Result<Payload> {
        match val {
            Payload::Json(j) => match j.as_ref() {
                serde_json::Value::Array(arr) => {
                    let elems =
                        arr.iter().try_fold(SmallVec::new(), |mut acc, jv| {
                            let fval = Payload::Json(Arc::new(jv.clone()));
                            match self.read_ty_value(&fval, elem, span) {
                                Ok(rv) if rv.is_ok() => {
                                    let inner = self
                                        .unwrap_result_ok(&rv, span)
                                        .unwrap_or(fval);
                                    let id = self.arena.add_typed(
                                        inner,
                                        ValueMeta::untyped(),
                                        span,
                                    );
                                    acc.push(id);
                                    Ok(acc)
                                }
                                Ok(rv) => Err(self.extract_result_err_msg(&rv)),
                                Err(e) => Err(e.to_string()),
                            }
                        });
                    Ok(match elems {
                        Ok(elems) => self.make_result_ok(
                            Payload::Array(Arc::new(elems)),
                            span,
                        ),
                        Err(msg) => self.make_result_err(&msg, span),
                    })
                }
                _ => Ok(self.make_result_err("expected array", span)),
            },
            Payload::Array(elems) => {
                Ok(self.make_result_ok(Payload::Array(elems.clone()), span))
            }
            _ => Ok(self.make_result_err("expected array", span)),
        }
    }

    /// Read a value as `Option[T]`, treating JSON null as `None`.
    fn read_option_value(
        &mut self,
        val: &Payload,
        inner: typecheck::TyId,
        span: Span,
    ) -> Result<Payload> {
        match val {
            Payload::Json(j)
                if matches!(j.as_ref(), serde_json::Value::Null) =>
            {
                Ok(self.make_result_ok(Payload::none(), span))
            }
            _ => match self.read_ty_value(val, inner, span) {
                Ok(rv) if rv.is_ok() => {
                    let inner_val =
                        self.unwrap_result_ok(&rv, span).unwrap_or_else(|_| {
                            typechecked!("Option read", "Result.Ok")
                        });
                    let inner_id = self.arena.add_typed(
                        inner_val,
                        ValueMeta::untyped(),
                        span,
                    );
                    Ok(self.make_result_ok(Payload::some(inner_id), span))
                }
                Ok(rv) => Ok(rv),
                Err(e) => Err(e),
            },
        }
    }

    /// Resolve a field type while reading object fields.
    fn resolve_read_field_target(
        &mut self,
        ty: typecheck::TyId,
    ) -> Option<ReadTarget> {
        match self.ty_arena.get(ty).clone() {
            typecheck::Ty::Object(fields) => Some(ReadTarget::Object(fields)),
            typecheck::Ty::Named(type_id, args) => {
                let alias =
                    self.registry.get_def(type_id).and_then(|def| match def {
                        TypeDef::Alias {
                            type_params,
                            target,
                            ..
                        } => Some((type_params.clone(), *target)),
                        _ => None,
                    });
                match alias {
                    Some((params, target)) => {
                        let subst: indexmap::IndexMap<
                            StringId,
                            typecheck::TyId,
                        > = params
                            .iter()
                            .zip(args.iter())
                            .map(|(&p, &a)| (p, a))
                            .collect();
                        let expanded = self.read_ast_type_to_ty(target, &subst);
                        self.resolve_read_field_target(expanded)
                    }
                    None => Some(ReadTarget::Ty(ty)),
                }
            }
            _ => Some(ReadTarget::Ty(ty)),
        }
    }

    /// Convert an alias target AST type to a runtime `TyId` for `read`.
    fn read_ast_type_to_ty(
        &mut self,
        ty: AstTypeExprId,
        subst: &indexmap::IndexMap<StringId, typecheck::TyId>,
    ) -> typecheck::TyId {
        match self.ast.get_type_expr(ty).cloned() {
            Some(AstTypeExpr::Named(name)) => subst
                .get(&name.local_name())
                .copied()
                .or_else(|| {
                    self.registry.lookup(&name).map(|id| {
                        Self::type_id_to_ty_id(id, &mut self.ty_arena)
                    })
                })
                .unwrap_or(typecheck::TyArena::ERROR),
            Some(AstTypeExpr::App(name, args)) => {
                let arg_tys: SmallVec<[typecheck::TyId; 4]> = args
                    .iter()
                    .map(|&a| self.read_ast_type_to_ty(a, subst))
                    .collect();
                self.registry
                    .lookup(&name)
                    .map(|id| match id {
                        TypeId::ARRAY => arg_tys
                            .first()
                            .map_or(typecheck::TyArena::ERROR, |&a| {
                                self.ty_arena.array(a)
                            }),
                        TypeId::OPTION => arg_tys
                            .first()
                            .map_or(typecheck::TyArena::ERROR, |&a| {
                                self.ty_arena.option(a)
                            }),
                        TypeId::RESULT => {
                            match (arg_tys.first(), arg_tys.get(1)) {
                                (Some(&ok), Some(&err)) => {
                                    self.ty_arena.result(ok, err)
                                }
                                _ => typecheck::TyArena::ERROR,
                            }
                        }
                        TypeId::MAP => {
                            match (arg_tys.first(), arg_tys.get(1)) {
                                (Some(&k), Some(&v)) => {
                                    self.ty_arena.map_ty(k, v)
                                }
                                _ => typecheck::TyArena::ERROR,
                            }
                        }
                        _ => self.ty_arena.named(id, arg_tys),
                    })
                    .unwrap_or(typecheck::TyArena::ERROR)
            }
            Some(AstTypeExpr::Object(fields)) => {
                let fs = fields
                    .iter()
                    .map(|(name, fty)| {
                        (*name, self.read_ast_type_to_ty(*fty, subst))
                    })
                    .collect();
                self.ty_arena.alloc(typecheck::Ty::Object(fs))
            }
            Some(AstTypeExpr::Tuple(elems)) => {
                let ts = elems
                    .iter()
                    .map(|&e| self.read_ast_type_to_ty(e, subst))
                    .collect();
                self.ty_arena.alloc(typecheck::Ty::Tuple(ts))
            }
            _ => typecheck::TyArena::ERROR,
        }
    }

    /// Convert a JSON or Object value to a typed object via `read`.
    fn read_to_object(
        &mut self,
        val: &Payload,
        fields: &indexmap::IndexMap<StringId, typecheck::TyId>,
        span: Span,
    ) -> Result<Payload> {
        match val {
            Payload::Json(j) => match j.as_ref() {
                serde_json::Value::Object(obj) => {
                    self.read_json_object(obj, fields, span)
                }
                _ => {
                    let msg = "cannot read non-object JSON as object";
                    Ok(self.make_result_err(msg, span))
                }
            },
            Payload::Object(obj) => self.read_native_object(obj, fields, span),
            _ => {
                let src = val.type_name(&self.registry, &self.arena);
                let msg = format!("cannot read `{src}` as object");
                Ok(self.make_result_err(&msg, span))
            }
        }
    }

    /// Read fields from a JSON object, converting each field via `read_value`.
    fn read_json_object(
        &mut self,
        obj: &serde_json::Map<std::string::String, serde_json::Value>,
        fields: &indexmap::IndexMap<StringId, typecheck::TyId>,
        span: Span,
    ) -> Result<Payload> {
        let mut result = indexmap::IndexMap::new();
        let mut err = None;

        fields.iter().for_each(|(&fid, &fty)| {
            if err.is_some() {
            } else {
                let fname = self
                    .arena
                    .get_str(fid)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "?".to_owned());

                match obj.get(&fname) {
                    None => {
                        err = Some(format!("missing field `{fname}`"));
                    }
                    Some(jv) => {
                        let fval = Payload::Json(Arc::new(jv.clone()));
                        match self.resolve_read_field_target(fty) {
                            Some(ReadTarget::Ty(tid)) => {
                                match self.read_ty_value(&fval, tid, span) {
                                    Ok(rv) if rv.is_ok() => {
                                        let inner = self
                                            .unwrap_result_ok(&rv, span)
                                            .unwrap_or(fval);
                                        let vid = self.arena.add_typed(
                                            inner,
                                            ValueMeta::untyped(),
                                            span,
                                        );
                                        result.insert(fid, vid);
                                    }
                                    Ok(rv) => {
                                        let msg =
                                            self.extract_result_err_msg(&rv);
                                        err = Some(msg);
                                    }
                                    Err(e) => {
                                        err = Some(e.to_string());
                                    }
                                }
                            }
                            Some(ReadTarget::Object(fields)) => {
                                match self.read_to_object(&fval, &fields, span)
                                {
                                    Ok(rv) if rv.is_ok() => {
                                        let inner = self
                                            .unwrap_result_ok(&rv, span)
                                            .unwrap_or(fval);
                                        let vid = self.arena.add_typed(
                                            inner,
                                            ValueMeta::untyped(),
                                            span,
                                        );
                                        result.insert(fid, vid);
                                    }
                                    Ok(rv) => {
                                        let msg =
                                            self.extract_result_err_msg(&rv);
                                        err = Some(msg);
                                    }
                                    Err(e) => {
                                        err = Some(e.to_string());
                                    }
                                }
                            }
                            None => {
                                err = Some(format!(
                                    "unsupported field type for `{fname}`"
                                ));
                            }
                        }
                    }
                }
            }
        });

        match err {
            Some(msg) => Ok(self.make_result_err(&msg, span)),
            None => {
                let obj = Payload::Object(Arc::new(result));
                Ok(self.make_result_ok(obj, span))
            }
        }
    }

    /// Read fields from a native object, validating field types.
    fn read_native_object(
        &mut self,
        obj: &Arc<indexmap::IndexMap<StringId, ValueId>>,
        fields: &indexmap::IndexMap<StringId, typecheck::TyId>,
        span: Span,
    ) -> Result<Payload> {
        let mut result = indexmap::IndexMap::new();
        let mut err = None;

        fields.iter().for_each(|(&fid, &fty)| {
            if err.is_some() {
            } else {
                match obj.get(&fid) {
                    None => {
                        let fname = self.arena.get_str(fid).unwrap_or("?");
                        err = Some(format!("missing field `{fname}`"));
                    }
                    Some(&vid) => match self.arena.get(vid).cloned() {
                        Some(fval) => {
                            match self.resolve_read_field_target(fty) {
                                Some(ReadTarget::Ty(tid)) => match self
                                    .read_ty_value(&fval, tid, span)
                                {
                                    Ok(rv) if rv.is_ok() => {
                                        let inner = self
                                            .unwrap_result_ok(&rv, span)
                                            .unwrap_or(fval);
                                        let new_vid = self.arena.add_typed(
                                            inner,
                                            ValueMeta::untyped(),
                                            span,
                                        );
                                        result.insert(fid, new_vid);
                                    }
                                    Ok(rv) => {
                                        err = Some(
                                            self.extract_result_err_msg(&rv),
                                        );
                                    }
                                    Err(e) => {
                                        err = Some(e.to_string());
                                    }
                                },
                                Some(ReadTarget::Object(fields)) => {
                                    match self
                                        .read_to_object(&fval, &fields, span)
                                    {
                                        Ok(rv) if rv.is_ok() => {
                                            let inner = self
                                                .unwrap_result_ok(&rv, span)
                                                .unwrap_or(fval);
                                            let new_vid = self.arena.add_typed(
                                                inner,
                                                ValueMeta::untyped(),
                                                span,
                                            );
                                            result.insert(fid, new_vid);
                                        }
                                        Ok(rv) => {
                                            err = Some(
                                                self.extract_result_err_msg(
                                                    &rv,
                                                ),
                                            );
                                        }
                                        Err(e) => {
                                            err = Some(e.to_string());
                                        }
                                    }
                                }
                                None => {
                                    result.insert(fid, vid);
                                }
                            }
                        }
                        None => {
                            err = Some("missing value in arena".to_owned());
                        }
                    },
                }
            }
        });

        match err {
            Some(msg) => Ok(self.make_result_err(&msg, span)),
            None => {
                let obj = Payload::Object(Arc::new(result));
                Ok(self.make_result_ok(obj, span))
            }
        }
    }

    /// Evaluate a type annotation: `(expr) : Type`.
    ///
    /// Type validation is handled statically by the typechecker; at runtime
    /// this is a pass-through that simply evaluates the inner expression.
    #[async_recursion]
    async fn annotate(
        &mut self,
        expr: ExprId,
        _ast_ty: AstTypeExprId,
        _span: Span,
    ) -> Result<Payload> {
        self.eval(expr).await
    }

    /// Execute a `let` binding with destructuring.
    ///
    /// Type annotations are validated statically by the typechecker; at runtime
    /// this simply evaluates the expression and destructures into the pattern.
    #[async_recursion]
    async fn r#let(
        &mut self,
        pat: &BindingPattern,
        _ty_ann: Option<AstTypeExprId>,
        expr_id: ExprId,
        span: Span,
    ) -> Result<()> {
        let val = self.eval(expr_id).await?;
        match pat {
            BindingPattern::Var(name) => {
                let id =
                    self.arena.add_typed(val, self.expr_meta(expr_id), span);
                self.env.scopes.bind(*name, id);
                Ok(())
            }
            _ => self.destructure(pat, &val, span),
        }
    }

    /// No-op wrapping stub; `Payload::Union`/`Payload::Newtype` no longer exist.
    pub(super) fn maybe_wrap_value(
        &mut self,
        _val: &Payload,
        _span: Span,
    ) -> Option<Payload> {
        None
    }

    /// No-op wrapping stub; `Payload::Union`/`Payload::Newtype` no longer exist.
    pub(super) fn maybe_wrap_value_id(
        &mut self,
        val_id: ValueId,
        _span: Span,
    ) -> ValueId {
        val_id
    }

    /// Execute a `write` statement.
    ///
    /// Writes to stdout, stderr, or a file via the I/O context, with optional
    /// `json` or `raw` format modifier.
    #[async_recursion]
    async fn write(&mut self, output: &WriteExpr) -> Result<()> {
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
            OutputFormat::Raw => self.display_raw(&val),
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
    /// Returns `Payload::Json(Object)`.
    #[async_recursion]
    #[allow(clippy::while_let_on_iterator)]
    async fn json(
        &mut self,
        fields: &[(StringId, ExprId)],
        _span: Span,
    ) -> Result<Payload> {
        let mut obj = serde_json::Map::new();
        // Process fields sequentially to maintain order
        let mut it = fields.iter();
        while let Some((key, expr_id)) = it.next() {
            let val = self.eval(*expr_id).await?;
            let json_val = self.jsonify(&val);
            obj.insert(self.arena.strings.resolve(*key).to_owned(), json_val);
        }
        Ok(Payload::Json(Arc::new(serde_json::Value::Object(obj))))
    }

    /// Evaluate JSON field access.
    ///
    /// For `JsonAccessKind::Json` (`.` or `->`): returns `Payload::Json` (null for missing).
    /// For `JsonAccessKind::Scalar` (`..` or `->>`): returns `Option[scalar]`.
    #[async_recursion]
    async fn json_access(
        &mut self,
        base: ExprId,
        kind: JsonAccessKind,
        key: &JsonAccessKey,
        span: Span,
    ) -> Result<Payload> {
        let base_val = self.eval(base).await?;

        // Get the key string
        let key_str = match key {
            JsonAccessKey::Field(name) => self.arena.strings.resolve(*name),
            JsonAccessKey::Expr(expr_id) => {
                let key_val = self.eval(*expr_id).await?;
                match key_val {
                    Payload::String(sid) => {
                        self.arena.get_str(sid).unwrap_or("").to_owned()
                    }
                    Payload::Int(n) => n.to_string(),
                    _ => typechecked!("json[key]", "String | Int"),
                }
            }
        };

        // Access the JSON value
        let json_val = match &base_val {
            Payload::Json(j) => j.get(&key_str).cloned(),
            _ => typechecked!("json access", "Json"),
        };

        match kind {
            // `.` or `->`: return Json (null for missing)
            JsonAccessKind::Json => Ok(Payload::Json(Arc::new(
                json_val.unwrap_or(serde_json::Value::Null),
            ))),
            // `..` or `->>`: extract scalar, return Option[T]
            JsonAccessKind::Scalar => {
                self.json_to_option_scalar(json_val, span)
            }
        }
    }

    /// Convert a JSON value to `Option[Scalar]`.
    ///
    /// Returns `Option[Scalar]` where `Scalar = Bool | Int | Float | String`:
    /// - `None` or `null` -> `Option.None`
    /// - `bool` -> `Option.Some(Bool)`
    /// - `number` -> `Option.Some(Int)` or `Option.Some(Float)`
    /// - `string` -> `Option.Some(String)`
    /// - `array`/`object` -> runtime error
    fn json_to_option_scalar(
        &mut self,
        json: Option<serde_json::Value>,
        span: Span,
    ) -> Result<Payload> {
        match json {
            None | Some(serde_json::Value::Null) => Ok(self.make_none_scalar()),
            Some(serde_json::Value::Bool(b)) => {
                let val_id = self.add_val(
                    Payload::Bool(b),
                    self.runtime_types.meta_bool(),
                    span,
                );
                Ok(self.make_some_scalar(val_id))
            }
            Some(serde_json::Value::Number(n)) => {
                let (val, meta) = n.as_i64().map_or_else(
                    || {
                        let v = Payload::Float(OrderedFloat(
                            n.as_f64().unwrap_or(0.0),
                        ));
                        (v, self.runtime_types.meta_float())
                    },
                    |i| (Payload::Int(i), self.runtime_types.meta_int()),
                );
                let val_id = self.add_val(val, meta, span);
                Ok(self.make_some_scalar(val_id))
            }
            Some(serde_json::Value::String(s)) => {
                let sid = self.arena.intern(&s);
                let val_id = self.add_val(
                    Payload::String(sid),
                    self.runtime_types.meta_string(),
                    span,
                );
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
