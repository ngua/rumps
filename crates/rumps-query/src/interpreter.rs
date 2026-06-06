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
//! | `Variant` | `"TypeName.Variant"` or `"TypeName.Variant(args...)"` |
//!
//! ## JSON Coercion
//!
//! JSON conversion is used for storage serialization of complex values.
//!
//! **To JSON** ([`Interpreter::jsonify`]):
//! - Scalars map directly (`Bool`, `Int`, `Float`, `String`)
//! - `Array` becomes a JSON array
//! - `Object` becomes a JSON object
//! - `Variant` becomes `{"_type": "...", "_variant": "...", "_payload": [...]}`
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
//! Storage conversion translates between runtime `Value` payload data and
//! persistent `rumps_types::Value`.
//!
//! **To storage** ([`Interpreter::store`]):
//! - `Bool`, `Int`, `Float`, `String` map directly
//! - `Array`, `Object`, `Variant` are serialized as JSON
//!
//! **From storage** ([`Interpreter::load`]):
//! - Direct types map back to their runtime equivalents
//! - JSON is parsed via [`Interpreter::unjsonify`]
//!
//! ## Subscript Coercion
//!
//! Database subscripts support the payloads accepted by
//! ([`Interpreter::subscript`]):
//!
//! - `Bool`, `Int`, `Float`, `String` are valid subscripts
//! - `Array`, `Object`, `Variant` cannot be subscripts (returns error)
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
//!    `variant`. These are registered during interpretation, so they cannot be
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
mod map;
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
    Ast, BinOp, BindingPattern, Expr, ExprId, Import, ImportItem,
    JsonAccessKey, JsonAccessKind, Literal, NumericLit, OutputFormat,
    OutputTarget, Stmt, StmtId, TxnId, TypeDefAst, TypeParam, TypePattern,
    UnOp, WriteExpr,
};
use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::io::IoContext;
use crate::resolve::{InstanceMap, ResolveCtx};
use crate::typecheck::{CheckedProgram, ExprAux, RuntimeTyId, TyVar};
use crate::value::{
    CapturedEnv, FunctionDef, Payload, TypeDef, TypeId, TypeRegistry, Value,
    ValueArena, ValueId, ValueMeta, VariantDef,
};
use crate::{env, typecheck, ClassId, Error, Result, Span};

/// Resolved `read` target: either an object type (structural) or a named type.
enum ReadTarget {
    Object {
        target: RuntimeTyId,
        fields: indexmap::IndexMap<StringId, typecheck::TyId>,
    },
    Ty {
        target: RuntimeTyId,
        convert: RuntimeTyId,
    },
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

    /// Checked program metadata produced by typechecking.
    checked: CheckedProgram,

    /// Registry of class methods for dispatch.
    class_methods: class::ClassMethods,

    /// Registry of module-level HoFs for dispatch.
    module_hofs: hof::Registry,

    /// Registry of user-defined class instances for runtime dispatch.
    ///
    /// Populated from the typechecker's instance registry when `class`
    /// statements are processed.
    user_instances: instance::RuntimeInstanceRegistry,

    /// Resolved class instance information from the resolution pass.
    ///
    /// Used during hoisting to register instance methods as functions and
    /// populate `user_instances`. Keyed by `StmtId` so hoisting can look up
    /// the resolved info when processing `Stmt::ClassInstance`.
    resolved_instances: InstanceMap,

    /// Runtime substitutions for generic callable body evaluation.
    runtime_ty_substs: Vec<HashMap<TyVar, RuntimeTyId>>,
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
        let tc = typecheck::InferCtx::new(
            ast,
            &registry,
            &env,
            arena.interner(),
            interactive,
        )
        .check(stmts, &registry, &arena)?;

        let module_hofs = hof::Registry::new(&mut arena.strings);
        let class_methods = {
            let mut cm = class::ClassMethods::new();
            cm.register_all(&mut arena.strings);
            cm
        };

        let checked = tc.to_checked();

        Ok(Self {
            ast,
            env,
            db,
            txns: HashMap::new(),
            arena,
            registry,
            checked,
            functions: HashMap::new(),
            io,
            class_methods,
            module_hofs,
            user_instances: instance::RuntimeInstanceRegistry::new(),
            resolved_instances,
            runtime_ty_substs: Vec::new(),
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
    /// Executes `Let`, `Type`, `Newtype`, `Union`, and `Import` statements.
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
                    Stmt::Let(pat, _, expr_id, _) => {
                        self.r#let(&pat, expr_id, span).await?
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
                        self.type_decl(&n, &type_params, &def)?
                    }
                    Stmt::Newtype {
                        name, type_params, ..
                    } => {
                        let n = self.arena.strings.resolve(name);
                        self.newtype_decl(&n, &type_params)?
                    }
                    Stmt::Union { name, .. } => {
                        let n = self.arena.strings.resolve(name);
                        self.union_decl(&n)?
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
        self.invoke_function(&def.params, def.body, &[], Span::default())
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
        let module_hofs = hof::Registry::new(&mut arena.strings);
        let class_methods = {
            let mut cm = class::ClassMethods::new();
            cm.register_all(&mut arena.strings);
            cm
        };
        let mut types = typecheck::TyArena::new();
        let class_registry = typecheck::ClassRegistry::builtins(
            &mut |s| arena.strings.intern(s),
            &mut types,
        );
        let checked = CheckedProgram {
            types: typecheck::RuntimeTypes::new(types, HashMap::new()),
            exprs: HashMap::new(),
            regex_cache: Vec::new(),
            class_registry,
            function_types: HashMap::new(),
            module_fns: HashMap::new(),
            module_consts: HashMap::new(),
            expr_targets: HashMap::new(),
            approved_newtype_edges: HashMap::new(),
            is_patterns: HashMap::new(),
            let_targets: HashMap::new(),
            match_targets: HashMap::new(),
        };
        Self {
            ast,
            env: Environment::with_interner(arena.interner()),
            db,
            txns: HashMap::new(),
            arena,
            registry,
            checked,
            functions: HashMap::new(),
            io,
            class_methods,
            module_hofs,
            user_instances: instance::RuntimeInstanceRegistry::new(),
            resolved_instances: HashMap::new(),
            runtime_ty_substs: Vec::new(),
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

        enum Evaluated {
            Value(Value),
            Payload(Payload),
        }

        let evaluated = match expr {
            Expr::Literal(lit) => {
                Ok(Evaluated::Payload(self.literal(id, &lit)))
            }
            Expr::Interpolation(parts) => {
                self.interpolation(&parts).await.map(Evaluated::Payload)
            }
            Expr::Var(name) => {
                let n = self.arena.strings.resolve(name);
                self.var_value(id, &n).map(Evaluated::Value)
            }
            Expr::Intrinsic(op, ref rt, val, txn_id) => self
                .intrinsic(op, rt, val, txn_id, span)
                .await
                .map(Evaluated::Payload),
            Expr::Binary(lhs, op, rhs) => self
                .binary(id, lhs, op, rhs, span)
                .await
                .map(Evaluated::Value),
            Expr::Unary(op, operand) => self
                .unary(id, op, operand, span)
                .await
                .map(Evaluated::Value),
            Expr::Call(callee, args) => self
                .call(id, callee, &args, span)
                .await
                .map(Evaluated::Value),
            Expr::Object(entries) => {
                self.object(&entries, span).await.map(Evaluated::Payload)
            }
            Expr::Array(elems) => {
                self.array(id, &elems, span).await.map(Evaluated::Payload)
            }
            Expr::Tuple(elems) => {
                self.tuple(&elems, span).await.map(Evaluated::Payload)
            }
            Expr::MapLit(entries) => {
                self.map_lit(&entries, span).await.map(Evaluated::Payload)
            }
            Expr::TupleIndex(base, idx) => {
                self.tuple_index(base, idx).await.map(Evaluated::Value)
            }
            Expr::Index(base, idx) => {
                self.index(id, base, idx, span).await.map(Evaluated::Value)
            }
            Expr::OptionalIndex(base, idx) => self
                .optional_index(id, base, idx, span)
                .await
                .map(Evaluated::Payload),
            Expr::Field(base, field) => {
                self.field(id, base, &field).await.map(Evaluated::Value)
            }
            Expr::OptionalField(base, field) => self
                .optional_field(base, &field)
                .await
                .map(Evaluated::Payload),
            Expr::Variant(ref ty, var, ref args) => {
                let is_ctor = args.is_empty()
                    && matches!(
                        self.checked.types.get(self.checked.expr(id).ty),
                        typecheck::Ty::Fn(_, _)
                    );
                if is_ctor {
                    Ok(Evaluated::Payload(Payload::VariantCtor {
                        ty: ty.clone(),
                        var,
                    }))
                } else {
                    self.variant(id, ty, var, args, span)
                        .await
                        .map(Evaluated::Payload)
                }
            }
            Expr::NakedVariant(..) => {
                typechecked!("naked variant constructor", "resolved type")
            }
            Expr::Path(ref segments) => {
                self.path_value(id, segments).map(Evaluated::Value)
            }
            Expr::Is(expr, pattern) => {
                self.is(id, expr, &pattern).await.map(Evaluated::Payload)
            }
            Expr::As(expr, _) => {
                self.r#as(id, expr, span).await.map(Evaluated::Value)
            }
            Expr::Read(expr, _) => {
                self.read(id, expr, span).await.map(Evaluated::Value)
            }
            Expr::Block(stmts, tail) => {
                self.block(&stmts, tail).await.map(Evaluated::Value)
            }
            Expr::If(cond, then_br, else_br) => self
                .r#if(cond, then_br, else_br)
                .await
                .map(Evaluated::Value),
            Expr::Match(scrutinee, arms) => self
                .r#match(scrutinee, &arms, span)
                .await
                .map(Evaluated::Value),
            Expr::Closure { params, body, .. } => {
                let ps = params.iter().map(|(pid, _)| *pid).collect();
                self.closure(ps, body).map(Evaluated::Payload)
            }
            Expr::Postfix(op, inner) => {
                let val = self.eval(inner).await?;
                if matches!(
                    &self.checked.expr(id).aux,
                    ExprAux::InstanceCall { .. }
                ) {
                    let val_id = self.add_value(val, span);
                    let mid = self.arena.intern("unwrap");
                    self.dispatch_class_method_value(call::ClassDispatch {
                        dispatch_expr_id: Some(id),
                        output_expr_id: Some(id),
                        output_ty: None,
                        class: ClassId::FALLIBLE,
                        method: mid,
                        args: SmallVec::from_slice(&[val_id]),
                        span,
                    })
                    .await
                } else {
                    self.postfix(op, val, span)
                }
                .map(Evaluated::Value)
            }
            Expr::Range(start_id, end_id, inclusive) => self
                .range(start_id, end_id, inclusive)
                .await
                .map(Evaluated::Payload),
            Expr::Annotate(inner, _) => {
                self.annotate(id, inner).await.map(Evaluated::Value)
            }
            Expr::Json(fields) => {
                self.json(&fields).await.map(Evaluated::Payload)
            }
            Expr::JsonAccess(base, kind, key) => self
                .json_access(base, kind, &key, span)
                .await
                .map(Evaluated::Payload),
            Expr::Regex(_, _) => {
                let idx = match &self.checked.expr(id).aux {
                    ExprAux::RegexIndex(i) => *i,
                    _ => invariant!("regex compiled"),
                };
                Ok(Evaluated::Payload(Payload::Regex(idx)))
            }
            Expr::Matches(lhs, rhs) => {
                self.matches(lhs, rhs).await.map(Evaluated::Payload)
            }
            Expr::Catch(expr_id, handler_id) => self
                .catch(expr_id, handler_id, span)
                .await
                .map(Evaluated::Value),
            Expr::Write(output) => {
                self.write(&output).await?;
                Ok(Evaluated::Payload(Payload::Unit))
            }
            Expr::Raise(inner) => {
                let val = self.eval(inner).await?;
                let msg = if let Payload::String(id) = &val.payload {
                    self.arena.get_str(*id).unwrap_or("").to_owned()
                } else {
                    self.stringify_value(&val)
                };
                Err(Error::raise(span, msg))
            }
            Expr::Loop {
                seed,
                state_param,
                cont_param,
                body,
            } => self
                .r#loop(seed, state_param.0, cont_param.0, body, span)
                .await
                .map(Evaluated::Value),
            Expr::Transaction(ref txn) => {
                self.transaction(txn, span).await.map(Evaluated::Payload)
            }
            Expr::DefaultValue => {
                self.default_value(id, span).map(Evaluated::Payload)
            }
            Expr::Ref(ref dbref) => {
                self.ref_lit(dbref, span).await.map(Evaluated::Payload)
            }
            Expr::ClassMethod(ref class, ref method, ref args) => self
                .class_method_expr(id, *class, *method, args, span)
                .await
                .map(Evaluated::Value),
            Expr::ClassMethodRef(ref class, _, ref method) => {
                Ok(Evaluated::Payload(Payload::ClassMethodFn {
                    class: *class,
                    method: *method,
                    expr_id: Some(id),
                }))
            }
            Expr::NakedClassMethod(ref method, ref args) => {
                let class = match &self.checked.expr(id).aux {
                    ExprAux::NakedMethod { class }
                    | ExprAux::InstanceCall {
                        class: Some(class), ..
                    }
                    | ExprAux::HofCall {
                        class: Some(class), ..
                    } => *class,
                    _ => typechecked!("naked class method", "resolved class"),
                };
                self.class_method_expr(id, class, *method, args, span)
                    .await
                    .map(Evaluated::Value)
            }
            Expr::NakedClassMethodRef(ref method) => {
                let class = match &self.checked.expr(id).aux {
                    ExprAux::NakedMethod { class }
                    | ExprAux::InstanceCall {
                        class: Some(class), ..
                    }
                    | ExprAux::HofCall {
                        class: Some(class), ..
                    } => *class,
                    _ => {
                        typechecked!("naked class method ref", "resolved class")
                    }
                };
                Ok(Evaluated::Payload(Payload::ClassMethodFn {
                    class,
                    method: *method,
                    expr_id: Some(id),
                }))
            }
        }?;

        match evaluated {
            Evaluated::Value(value) => Ok(value),
            Evaluated::Payload(payload) => Ok(self.value_for_expr(id, payload)),
        }
    }

    #[async_recursion]
    async fn eval_payload(&mut self, id: ExprId) -> Result<Payload> {
        self.eval(id).await.map(|v| v.payload)
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
            Stmt::Let(pat, _, expr_id, _) => {
                self.r#let(&pat, expr_id, span).await
            }
            Stmt::Expr(expr_id) => {
                // Evaluate for side effects, discard result
                self.eval(expr_id).await.map(|_| ())
            }
            Stmt::Fun {
                name, params, body, ..
            } => self.fun(
                name,
                params.iter().map(|(name, _)| *name).collect(),
                body,
            ),
            Stmt::Type {
                name,
                type_params,
                def,
                ..
            } => {
                let n = self.arena.strings.resolve(name);
                self.type_decl(&n, &type_params, &def)
            }
            Stmt::Newtype {
                name, type_params, ..
            } => {
                let n = self.arena.strings.resolve(name);
                self.newtype_decl(&n, &type_params)
            }
            Stmt::Union { name, .. } => {
                let n = self.arena.strings.resolve(name);
                self.union_decl(&n)
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
                            let ps = self.function_params(
                                params.iter().map(|(name, _)| *name).collect(),
                                fn_body,
                            );
                            module.functions.insert(
                                fn_name,
                                FunctionDef {
                                    name: fn_name,
                                    params: ps,
                                    ret: self.function_ret(fn_body),
                                    body: fn_body,
                                },
                            );
                        }

                        Stmt::Let(ref pat, _, expr_id, _) => {
                            let val = self.eval(expr_id).await?;
                            let val = self.value_for_binding(expr_id, val);
                            let val_id = self.add_value(val, item_span);
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
                            self.type_decl(&qname, &type_params, &def)?;
                        }

                        Stmt::Newtype {
                            name: alias_name,
                            type_params,
                            ..
                        } => {
                            // Aliases are already registered with qualified names
                            // by register_from_ast. The idempotent newtype_decl
                            // will skip if already present.
                            let an = self.arena.strings.resolve(alias_name);
                            let qname = format!("{}.{}", mod_path, an);
                            self.newtype_decl(&qname, &type_params)?;
                        }

                        Stmt::Union {
                            name: union_name, ..
                        } => {
                            // Unions are already registered with qualified names
                            // by register_from_ast. The idempotent union_decl
                            // will skip if already present.
                            let un = self.arena.strings.resolve(union_name);
                            let qname = format!("{}.{}", mod_path, un);
                            self.union_decl(&qname)?;
                        }

                        Stmt::ClassInstance { methods, .. } => {
                            self.hoist_class_instance(id, &methods)?;
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

        let (val, meta) = if self.env.module_const_exists(&full_path) {
            let meta = self.module_const_meta(&full_path);
            (Payload::ModuleConst { path: full_path }, meta)
        } else {
            let meta = self.module_fn_meta(&full_path);
            (Payload::ModuleFn { path: full_path }, meta)
        };
        let val_id = self.add_val(val, meta, span);
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
            let meta = self.module_fn_meta(&full_path);
            let val = Payload::ModuleFn { path: full_path };
            let val_id = self.add_val(val, meta, span);
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
        params: SmallVec<[StringId; 4]>,
        body: ExprId,
    ) -> Result<()> {
        let ps = self.function_params(params, body);

        self.functions.insert(
            name,
            FunctionDef {
                name,
                params: ps,
                ret: self.function_ret(body),
                body,
            },
        );

        Ok(())
    }

    /// Register a user-defined variant declaration.
    ///
    /// Processes `variant Name = Variant1 | Variant2(T) | ...` and registers
    /// the type in the type registry if it was not pre-registered.
    fn type_decl(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        def: &TypeDefAst,
    ) -> Result<()> {
        let qn = self.qname(name);
        let name_id = self.arena.intern(name);

        // Skip if already registered (from register_from_ast before type
        // checking). This makes type registration idempotent.
        if self.registry.lookup(&qn).is_none() {
            let TypeDefAst::Sum(variants) = def;

            // Build VariantDef entries
            let variant_defs: SmallVec<[VariantDef; 4]> = variants
                .iter()
                .enumerate()
                .map(|(idx, v)| VariantDef {
                    name: v.name,
                    idx: idx as u8,
                    arity: v.payloads.len() as u8,
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
                qn,
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
    ) -> Result<()> {
        let qn = self.qname(name);
        let name_id = self.arena.intern(name);

        // Skip if already registered (idempotent)
        if self.registry.lookup(&qn).is_none() {
            // Type parameters already have `StringId` names
            let type_param_ids: SmallVec<[StringId; 2]> =
                type_params.iter().map(|tp| tp.name).collect();

            // Register the alias
            self.registry.register(
                TypeDef::Alias {
                    name: name_id,
                    type_params: type_param_ids,
                },
                qn,
            );
        }

        Ok(())
    }

    /// Register a union type declaration.
    ///
    /// Union types define a set of types that a value can be.
    /// Example: `union Storable = Bool | Int | Float | Char | String | Json`
    fn union_decl(&mut self, name: &str) -> Result<()> {
        let qn = self.qname(name);

        if self.registry.lookup(&qn).is_some() {
            Ok(())
        } else {
            typechecked!("union declaration", "pre-registered type")
        }
    }

    fn qname(&mut self, name: &str) -> QualifiedName {
        QualifiedName::new(
            name.split('.')
                .map(|seg| self.arena.intern(seg))
                .collect::<SmallVec<[StringId; 3]>>(),
        )
    }

    /// Add a value to the arena with explicit type metadata.
    fn add_val(&mut self, v: Payload, meta: ValueMeta, span: Span) -> ValueId {
        let meta = self.runtime_meta(meta);
        self.arena.add_typed(v, meta, span)
    }

    pub(super) fn add_value(&mut self, v: Value, span: Span) -> ValueId {
        self.arena.add(v, span)
    }

    pub(super) fn value_from_meta(
        &mut self,
        payload: Payload,
        meta: ValueMeta,
    ) -> Value {
        let meta = self.runtime_meta(meta);
        Value {
            ty: meta.ty,
            repr: meta.repr,
            payload,
        }
    }

    pub(super) fn value_for_expr(
        &mut self,
        id: ExprId,
        payload: Payload,
    ) -> Value {
        let meta = self.runtime_meta(self.expr_meta(id));
        let meta = self.meta_for_runtime_payload(meta, &payload);
        self.value_from_meta(payload, meta)
    }

    fn meta_for_runtime_payload(
        &self,
        meta: ValueMeta,
        payload: &Payload,
    ) -> ValueMeta {
        if matches!(payload, Payload::Json(_)) {
            let json = self.checked.types.meta_json();
            match self.checked.types.get(meta.ty) {
                typecheck::Ty::Json => json,
                typecheck::Ty::Union(_, _)
                    if self
                        .checked
                        .types
                        .matches(json.ty, json.repr, meta.ty) =>
                {
                    self.checked.types.union_meta(meta.ty, json.repr)
                }
                typecheck::Ty::Named(_, _) => meta,
                _ => json,
            }
        } else {
            meta
        }
    }

    pub(super) fn value_from_payload(&mut self, payload: Payload) -> Value {
        let meta = self.payload_meta(&payload);
        self.value_from_meta(payload, meta)
    }

    pub(super) fn value_with_context_meta(
        &mut self,
        mut value: Value,
        meta: ValueMeta,
    ) -> Value {
        let meta = self.runtime_meta(meta);
        let meta = match self.checked.types.get(meta.ty) {
            typecheck::Ty::Union(_, _) => {
                let repr = self.runtime_ty(value.repr);
                self.checked.types.union_meta(meta.ty, repr)
            }
            _ => meta,
        };
        value.ty = meta.ty;
        value.repr = meta.repr;
        value
    }

    pub(super) fn approved_newtype_edge_meta(
        &self,
        id: ExprId,
    ) -> Option<ValueMeta> {
        self.checked
            .approved_newtype_edge(id)
            .map(|info| ValueMeta {
                ty: info.to,
                repr: info.repr,
            })
    }

    fn value_for_binding(&mut self, expr: ExprId, value: Value) -> Value {
        let value = match self.approved_newtype_edge_meta(expr) {
            Some(meta)
                if self.checked.types.matches(
                    meta.ty,
                    meta.repr,
                    self.expr_meta(expr).ty,
                ) =>
            {
                self.value_with_context_meta(value, meta)
            }
            _ => value,
        };
        match self.checked.let_target(expr) {
            None => value,
            Some(ty) => {
                let meta = match self.checked.types.get(ty) {
                    typecheck::Ty::Union(_, _) => {
                        self.checked.types.union_meta(ty, value.repr)
                    }
                    _ => self.checked.types.meta(ty),
                };
                self.value_with_context_meta(value, meta)
            }
        }
    }

    /// Add a value with metadata derived from its payload and children.
    fn add_payload(&mut self, v: Payload, span: Span) -> ValueId {
        let meta = self.payload_meta(&v);
        self.arena.add_typed(v, meta, span)
    }

    /// Metadata for a payload that lacks an enclosing expression id.
    fn payload_meta(&mut self, v: &Payload) -> ValueMeta {
        match v {
            Payload::Closure { params, ret, .. }
            | Payload::Function { params, ret, .. } => {
                self.callable_meta(params, *ret, 0)
            }
            Payload::ModuleFn { path } => self.module_fn_meta(path),
            Payload::ClassMethodFn {
                expr_id: Some(id), ..
            } => self.expr_meta(*id),
            Payload::ClassMethodFn { expr_id: None, .. } => typechecked!(
                "class method payload metadata",
                "checked expression id"
            ),
            Payload::PartialApp { callee, bound, .. } => {
                self.partial_app_meta(*callee, bound.len())
            }
            Payload::ModuleConst { path } => self.module_const_meta(path),
            _ => self.checked.types.meta_for_payload(&self.arena, v),
        }
    }

    fn callable_meta(
        &mut self,
        params: &[(StringId, RuntimeTyId)],
        ret: RuntimeTyId,
        bound: usize,
    ) -> ValueMeta {
        if bound > params.len() {
            typechecked!("callable metadata", "bound args <= arity")
        }
        let ps: SmallVec<[RuntimeTyId; 4]> =
            params.iter().skip(bound).map(|(_, ty)| *ty).collect();
        let ps: SmallVec<[RuntimeTyId; 4]> =
            ps.into_iter().map(|ty| self.runtime_ty(ty)).collect();
        let ret = self.runtime_ty(ret);
        let ty = self.checked.types.func(ps, ret);
        self.checked.types.meta(ty)
    }

    fn partial_app_meta(&mut self, callee: ValueId, bound: usize) -> ValueMeta {
        let callee =
            self.arena.payload(callee).cloned().unwrap_or_else(|| {
                typechecked!("partial app", "callee payload")
            });
        match callee {
            Payload::Closure { params, ret, .. }
            | Payload::Function { params, ret, .. } => {
                self.callable_meta(&params, ret, bound)
            }
            Payload::ModuleFn { path } => {
                self.module_fn_partial_meta(&path, bound)
            }
            Payload::ClassMethodFn {
                expr_id: Some(id), ..
            } => {
                let ty = self.expr_meta(id).ty;
                self.partial_meta_from_ty(ty, bound)
            }
            Payload::ClassMethodFn { expr_id: None, .. } => {
                typechecked!("partial app", "class method expression metadata")
            }
            Payload::PartialApp {
                callee,
                bound: more,
                ..
            } => {
                self.partial_app_meta(callee, bound.saturating_add(more.len()))
            }
            _ => typechecked!("partial app", "callable callee"),
        }
    }

    fn module_fn_partial_meta(
        &mut self,
        path: &[StringId],
        bound: usize,
    ) -> ValueMeta {
        if let Some(def) = self.env.get_user_module_fn(path).cloned() {
            self.callable_meta(&def.params, def.ret, bound)
        } else {
            let meta = self.module_fn_meta(path);
            self.partial_meta_from_ty(meta.ty, bound)
        }
    }

    fn partial_meta_from_ty(
        &mut self,
        ty: RuntimeTyId,
        bound: usize,
    ) -> ValueMeta {
        let ty = self.runtime_ty(ty);
        match self.checked.types.get(ty).clone() {
            typecheck::Ty::Fn(params, ret) => {
                if bound > params.len() {
                    typechecked!("partial app", "bound args <= arity")
                }
                let ps: SmallVec<[RuntimeTyId; 4]> = params
                    .iter()
                    .skip(bound)
                    .copied()
                    .map(RuntimeTyId::from)
                    .map(|ty| self.runtime_ty(ty))
                    .collect();
                let ret = self.runtime_ty(RuntimeTyId::from(ret));
                let ty = self.checked.types.func(ps, ret);
                self.checked.types.meta(ty)
            }
            _ => typechecked!("partial app", "function type metadata"),
        }
    }

    pub(super) fn function_ret(&self, body: ExprId) -> RuntimeTyId {
        let ty = self
            .checked
            .function_types
            .get(&body)
            .copied()
            .unwrap_or_else(|| {
                typechecked!(
                    "function payload metadata",
                    "checked function type"
                )
            });
        match self.checked.types.get(ty) {
            typecheck::Ty::Fn(_, ret) => RuntimeTyId::from(*ret),
            _ => typechecked!("function payload metadata", "function type"),
        }
    }

    fn function_params(
        &self,
        params: SmallVec<[StringId; 4]>,
        body: ExprId,
    ) -> SmallVec<[(StringId, RuntimeTyId); 4]> {
        let ty = self
            .checked
            .function_types
            .get(&body)
            .copied()
            .unwrap_or_else(|| {
                typechecked!(
                    "function payload metadata",
                    "checked function type"
                )
            });
        match self.checked.types.get(ty) {
            typecheck::Ty::Fn(param_tys, _) => {
                if param_tys.len() != params.len() {
                    typechecked!("function payload metadata", "function arity")
                }
                params
                    .into_iter()
                    .zip(param_tys.iter().copied().map(RuntimeTyId::from))
                    .collect()
            }
            _ => typechecked!("function payload metadata", "function type"),
        }
    }

    fn module_fn_meta(&mut self, path: &[StringId]) -> ValueMeta {
        if let Some(meta) = self.checked.module_fn_meta(path) {
            meta
        } else if let Some(def) = self.env.get_user_module_fn(path).cloned() {
            self.callable_meta(&def.params, def.ret, 0)
        } else {
            typechecked!("module function metadata", "known function")
        }
    }

    fn module_const_meta(&self, path: &[StringId]) -> ValueMeta {
        self.checked
            .module_const_meta(path)
            .or_else(|| {
                self.env
                    .get_user_module_const(path)
                    .and_then(|id| self.arena.meta(id))
            })
            .unwrap_or_else(|| {
                typechecked!("module const metadata", "known const")
            })
    }

    fn checked_expr_meta(&self, id: ExprId) -> ValueMeta {
        let info = self.checked.expr(id);
        info.repr.map_or_else(
            || self.checked.types.meta(info.ty),
            |repr| self.checked.types.union_meta(info.ty, repr),
        )
    }

    /// Look up the `ExprInfo` for `id` and produce a `ValueMeta`.
    ///
    fn expr_meta(&self, id: ExprId) -> ValueMeta {
        self.checked_expr_meta(id)
    }

    fn runtime_meta(&mut self, meta: ValueMeta) -> ValueMeta {
        let ty = self.runtime_ty(meta.ty);
        let repr = self.runtime_ty(meta.repr);
        match self.checked.types.get(ty) {
            typecheck::Ty::Union(_, _) => {
                self.checked.types.union_meta(ty, repr)
            }
            _ => ValueMeta {
                ty,
                repr: self.checked.types.repr(repr),
            },
        }
    }

    fn runtime_ty(&mut self, ty: RuntimeTyId) -> RuntimeTyId {
        match self.checked.types.get(ty).clone() {
            typecheck::Ty::Var(v) => {
                let sub = self
                    .runtime_ty_substs
                    .iter()
                    .rev()
                    .find_map(|sub| sub.get(&v).copied())
                    .unwrap_or(ty);
                if sub == ty {
                    ty
                } else {
                    self.runtime_ty(sub)
                }
            }
            typecheck::Ty::Array(elem) => {
                let elem = self.runtime_ty(RuntimeTyId::from(elem));
                self.checked.types.array(elem)
            }
            typecheck::Ty::Option(elem) => {
                let elem = self.runtime_ty(RuntimeTyId::from(elem));
                self.checked.types.option(elem)
            }
            typecheck::Ty::Result(ok, err) => {
                let ok = self.runtime_ty(RuntimeTyId::from(ok));
                let err = self.runtime_ty(RuntimeTyId::from(err));
                self.checked.types.result(ok, err)
            }
            typecheck::Ty::Map(key, val) => {
                let key = self.runtime_ty(RuntimeTyId::from(key));
                let val = self.runtime_ty(RuntimeTyId::from(val));
                self.checked.types.map(key, val)
            }
            typecheck::Ty::Tuple(elems) => {
                let elems = elems
                    .iter()
                    .map(|&elem| self.runtime_ty(RuntimeTyId::from(elem)))
                    .collect();
                self.checked.types.tuple(elems)
            }
            typecheck::Ty::Fn(params, ret) => {
                let params = params
                    .iter()
                    .map(|&param| self.runtime_ty(RuntimeTyId::from(param)))
                    .collect();
                let ret = self.runtime_ty(RuntimeTyId::from(ret));
                self.checked.types.func(params, ret)
            }
            typecheck::Ty::Object(fields) => {
                let fields = fields
                    .iter()
                    .map(|(&name, &field)| {
                        (name, self.runtime_ty(RuntimeTyId::from(field)))
                    })
                    .collect();
                self.checked.types.object(fields)
            }
            typecheck::Ty::Union(name, members) => {
                let members = members
                    .iter()
                    .map(|&member| self.runtime_ty(RuntimeTyId::from(member)))
                    .collect();
                self.checked.types.union(name, members)
            }
            typecheck::Ty::Named(id, args) => {
                let args = args
                    .iter()
                    .map(|&arg| self.runtime_ty(RuntimeTyId::from(arg)))
                    .collect();
                self.checked.types.named(id, args)
            }
            _ => ty,
        }
    }

    fn runtime_ty_subst(
        &mut self,
        params: &[(StringId, RuntimeTyId)],
        args: &[ValueId],
    ) -> HashMap<TyVar, RuntimeTyId> {
        params
            .iter()
            .zip(args.iter())
            .filter_map(|((_, ty), val_id)| {
                self.arena.meta(*val_id).map(|meta| (*ty, meta.ty))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|(ty, actual)| self.runtime_ty_pairs(ty, actual))
            .collect()
    }

    fn runtime_ty_pairs(
        &mut self,
        formal: RuntimeTyId,
        actual: RuntimeTyId,
    ) -> Vec<(TyVar, RuntimeTyId)> {
        let formal = self.runtime_ty(formal);
        let actual = self.runtime_ty(actual);
        match (
            self.checked.types.get(formal).clone(),
            self.checked.types.get(actual).clone(),
        ) {
            (typecheck::Ty::Var(v), _) => vec![(v, actual)],
            (typecheck::Ty::Array(f), typecheck::Ty::Array(a))
            | (typecheck::Ty::Option(f), typecheck::Ty::Option(a)) => self
                .runtime_ty_pairs(RuntimeTyId::from(f), RuntimeTyId::from(a)),
            (typecheck::Ty::Result(fa, fb), typecheck::Ty::Result(aa, ab))
            | (typecheck::Ty::Map(fa, fb), typecheck::Ty::Map(aa, ab)) => self
                .runtime_ty_pairs(RuntimeTyId::from(fa), RuntimeTyId::from(aa))
                .into_iter()
                .chain(self.runtime_ty_pairs(
                    RuntimeTyId::from(fb),
                    RuntimeTyId::from(ab),
                ))
                .collect(),
            (typecheck::Ty::Tuple(fs), typecheck::Ty::Tuple(as_))
            | (typecheck::Ty::Union(_, fs), typecheck::Ty::Union(_, as_)) => fs
                .iter()
                .zip(as_.iter())
                .flat_map(|(&f, &a)| {
                    self.runtime_ty_pairs(
                        RuntimeTyId::from(f),
                        RuntimeTyId::from(a),
                    )
                })
                .collect(),
            (typecheck::Ty::Fn(fps, fr), typecheck::Ty::Fn(aps, ar)) => {
                let mut pairs: Vec<(TyVar, RuntimeTyId)> = fps
                    .iter()
                    .zip(aps.iter())
                    .flat_map(|(&f, &a)| {
                        self.runtime_ty_pairs(
                            RuntimeTyId::from(f),
                            RuntimeTyId::from(a),
                        )
                    })
                    .collect();
                pairs.extend(self.runtime_ty_pairs(
                    RuntimeTyId::from(fr),
                    RuntimeTyId::from(ar),
                ));
                pairs
            }
            (typecheck::Ty::Object(fs), typecheck::Ty::Object(as_)) => fs
                .iter()
                .filter_map(|(name, &f)| as_.get(name).map(|&a| (f, a)))
                .flat_map(|(f, a)| {
                    self.runtime_ty_pairs(
                        RuntimeTyId::from(f),
                        RuntimeTyId::from(a),
                    )
                })
                .collect(),
            (typecheck::Ty::Named(fid, fs), typecheck::Ty::Named(aid, as_))
                if fid == aid =>
            {
                fs.iter()
                    .zip(as_.iter())
                    .flat_map(|(&f, &a)| {
                        self.runtime_ty_pairs(
                            RuntimeTyId::from(f),
                            RuntimeTyId::from(a),
                        )
                    })
                    .collect()
            }
            (typecheck::Ty::Apply(fv, fs), typecheck::Ty::Apply(av, as_))
                if fv == av =>
            {
                fs.iter()
                    .zip(as_.iter())
                    .flat_map(|(&f, &a)| {
                        self.runtime_ty_pairs(
                            RuntimeTyId::from(f),
                            RuntimeTyId::from(a),
                        )
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn payload_for_runtime_ty(
        &self,
        payload: Payload,
        ty: RuntimeTyId,
    ) -> Payload {
        match (payload, self.checked.types.get(ty)) {
            (Payload::Int(n), typecheck::Ty::Float) => {
                Payload::Float(OrderedFloat(n as f64))
            }
            (Payload::Int(n), typecheck::Ty::Word) if n >= 0 => {
                Payload::Word(n as usize)
            }
            (payload, _) => payload,
        }
    }

    fn numeric_binop_payloads(
        &mut self,
        id: ExprId,
        op: BinOp,
        left: Payload,
        right: Payload,
    ) -> (Payload, Payload) {
        if matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::FloorDiv
                | BinOp::Mod
                | BinOp::Pow
        ) {
            let ty = self.runtime_ty(self.expr_meta(id).ty);
            let left = self.payload_for_runtime_ty(left, ty);
            let right = self.payload_for_runtime_ty(right, ty);
            match (&left, &right) {
                (Payload::Float(_), Payload::Int(n)) => {
                    (left, Payload::Float(OrderedFloat(*n as f64)))
                }
                (Payload::Int(n), Payload::Float(_)) => {
                    (Payload::Float(OrderedFloat(*n as f64)), right)
                }
                (Payload::Word(_), Payload::Int(n)) if *n >= 0 => {
                    (left, Payload::Word(*n as usize))
                }
                (Payload::Int(n), Payload::Word(_)) if *n >= 0 => {
                    (Payload::Word(*n as usize), right)
                }
                _ => (left, right),
            }
        } else {
            (left, right)
        }
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
                let ty = self.runtime_ty(self.checked.expr(id).ty);
                let ty = self.checked.types.get(ty);
                match (n, ty) {
                    // Integer literals are polymorphic over Int/Word/Float
                    (NumericLit::Int(v), typecheck::Ty::Int) => {
                        Payload::Int(*v)
                    }
                    (NumericLit::Int(v), typecheck::Ty::Word) => {
                        Payload::Word(*v as usize)
                    }
                    (NumericLit::Int(v), typecheck::Ty::Float) => {
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
                let s = match &val.payload {
                    Payload::String(sid)
                        if !matches!(
                            self.checked.types.get(self.expr_meta(id).ty),
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
                    _ => self.stringify_value(&val),
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
        params: SmallVec<[StringId; 4]>,
        body: ExprId,
    ) -> Result<Payload> {
        let env = CapturedEnv::capture(self.env.scopes.stack());
        let ps = self.function_params(params, body);

        Ok(Payload::Closure {
            params: ps,
            ret: self.function_ret(body),
            body,
            env: Arc::new(env),
        })
    }

    /// Evaluate a lexical variable reference (`let` bindings only).
    ///
    /// Does NOT fall back to B-tree locals; use `@get` for those.
    fn var_value(&mut self, expr_id: ExprId, name: &str) -> Result<Value> {
        let name_id = self.arena.intern(name);

        // First try lexical scope
        if let Some(val) = self
            .env
            .scopes
            .lookup(name_id)
            .and_then(|val_id| self.arena.value(val_id).cloned())
        {
            Ok(self.resolve_module_const_value(val))
        } else if let Some(def) = self.functions.get(&name_id).cloned() {
            let payload = Payload::Function {
                name: def.name,
                params: def.params.clone(),
                ret: def.ret,
                body: def.body,
            };
            Ok(self.value_for_expr(expr_id, payload))
        } else {
            typechecked!("var", "Defined")
        }
    }

    /// Resolve a `ModuleConst` to its actual value.
    ///
    /// If the value is a `ModuleConst`, looks up the path in `env.consts`.
    /// Otherwise returns the value unchanged.
    fn resolve_module_const_value(&self, val: Value) -> Value {
        match &val.payload {
            Payload::ModuleConst { ref path } => self
                .env
                .get_module_const(path)
                .and_then(|id| self.env.consts.value(id).cloned())
                .unwrap_or(val),
            _ => val,
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
    ) -> Result<Value> {
        match op {
            // Short-circuit AND: if left is false, don't evaluate right
            BinOp::And => {
                let left = self.eval_payload(lhs).await?;
                match left {
                    Payload::Bool(false) => {
                        Ok(self.value_for_expr(id, Payload::Bool(false)))
                    }
                    Payload::Bool(true) => {
                        let right = self.eval_payload(rhs).await?;
                        match right {
                            Payload::Bool(b) => {
                                Ok(self.value_for_expr(id, Payload::Bool(b)))
                            }
                            _ => typechecked!("&&", "Bool"),
                        }
                    }
                    _ => typechecked!("&&", "Bool"),
                }
            }
            // Short-circuit OR: if left is true, don't evaluate right
            BinOp::Or => {
                let left = self.eval_payload(lhs).await?;
                match left {
                    Payload::Bool(true) => {
                        Ok(self.value_for_expr(id, Payload::Bool(true)))
                    }
                    Payload::Bool(false) => {
                        let right = self.eval_payload(rhs).await?;
                        match right {
                            Payload::Bool(b) => {
                                Ok(self.value_for_expr(id, Payload::Bool(b)))
                            }
                            _ => typechecked!("||", "Bool"),
                        }
                    }
                    _ => typechecked!("||", "Bool"),
                }
            }
            // Coalesce: unwrap Option.Some/Result.Ok, or evaluate right for None/Err
            BinOp::Coalesce => {
                let left = self.eval(lhs).await?;
                if matches!(
                    &self.checked.expr(id).aux,
                    ExprAux::InstanceCall { .. }
                ) {
                    let val_id = self.add_value(left, span);
                    let mid = self.arena.intern("unwrap");
                    match self
                        .dispatch_class_method_value(call::ClassDispatch {
                            dispatch_expr_id: Some(id),
                            output_expr_id: Some(id),
                            output_ty: None,
                            class: ClassId::FALLIBLE,
                            method: mid,
                            args: SmallVec::from_slice(&[val_id]),
                            span,
                        })
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
                let right = self.eval_payload(rhs).await?;
                self.pipeline(id, left, right, span).await
            }
            // All other operators: both sides evaluated, sync computation
            // (unless a user-defined class instance exists, which requires
            // async function invocation).
            _ => {
                if matches!(
                    &self.checked.expr(id).aux,
                    ExprAux::InstanceCall { .. }
                ) {
                    let left = self.eval(lhs).await?;
                    let right = self.eval(rhs).await?;
                    self.dispatch_binop_user(id, left, op, right, span).await
                } else if matches!(
                    op,
                    BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Gt
                        | BinOp::Le
                        | BinOp::Ge
                        | BinOp::Concat
                ) {
                    let left = self.eval(lhs).await?;
                    let right = self.eval(rhs).await?;
                    self.apply_value_binop_async(left, op, right, span)
                        .await
                        .map(|payload| self.value_for_expr(id, payload))
                } else {
                    let left = self.eval_payload(lhs).await?;
                    let right = self.eval_payload(rhs).await?;
                    let (left, right) =
                        self.numeric_binop_payloads(id, op, left, right);
                    self.apply_binop(&left, op, &right, span)
                        .map(|payload| self.value_for_expr(id, payload))
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
    ) -> Result<Value> {
        if matches!(op, UnOp::Wrap)
            && matches!(
                &self.checked.expr(id).aux,
                ExprAux::InstanceCall { .. }
            )
        {
            let val = self.eval(operand).await?;
            let val_id = self.add_value(val, span);
            let mid = self.arena.intern("wrap");
            self.dispatch_class_method_value(call::ClassDispatch {
                dispatch_expr_id: Some(id),
                output_expr_id: Some(id),
                output_ty: None,
                class: ClassId::WRAPPABLE,
                method: mid,
                args: SmallVec::from_slice(&[val_id]),
                span,
            })
            .await
        } else {
            let val = self.eval_payload(operand).await?;
            self.apply_unop(id, op, val, span)
                .map(|payload| self.value_for_expr(id, payload))
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
        id: ExprId,
        expr: ExprId,
        pattern: &TypePattern,
    ) -> Result<Payload> {
        let val = self.eval(expr).await?;
        let info = self.checked.is_patterns.get(&id).cloned();
        let matched = self.check_pattern(&val, pattern, info.as_ref())?;
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
    /// `newtype` representation casts use metadata approved by static
    /// `type visibility` and `repr visibility` checks.
    #[async_recursion]
    async fn r#as(
        &mut self,
        id: ExprId,
        expr: ExprId,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        let rty = self.checked.expr_target(id, "as");

        if let Some(meta) = self.approved_newtype_edge_meta(id) {
            Ok(self.value_with_context_meta(val, meta))
        } else if let Some(target_base) = self.checked.types.to_type_id(rty) {
            let storable = RuntimeTyId::from(typecheck::TyArena::STORABLE);
            if target_base == TypeId::STORABLE
                && self.checked.types.matches(val.ty, val.repr, storable)
            {
                let meta = self.checked.types.union_meta(rty, val.repr);
                Ok(self.value_with_context_meta(val, meta))
            } else if target_base == TypeId::STRING {
                let s = self.coerce_value_to_str(&val);
                let sid = self.arena.intern(&s);
                Ok(self.value_for_expr(id, Payload::String(sid)))
            } else if target_base == TypeId::JSON {
                let json = self.jsonify_value(&val);
                Ok(self.value_for_expr(id, Payload::Json(Arc::new(json))))
            } else {
                let target = self.checked.types.get(rty).clone();
                self.coerce_value(&val, target_base, &target, span)
                    .map(|payload| self.value_for_expr(id, payload))
            }
        } else if matches!(
            self.checked.types.get(rty),
            typecheck::Ty::Union(_, _)
        ) {
            let meta = self.checked.types.union_meta(rty, val.repr);
            Ok(self.value_with_context_meta(val, meta))
        } else {
            let meta = self.checked.types.meta(rty);
            Ok(self.value_with_context_meta(val, meta))
        }
    }

    /// Evaluate a fallible conversion: `expr read Type`.
    ///
    /// Returns `Result[T, String]` (as a RUMPS value), NOT `Err(crate::Error)`.
    /// Conversions:
    /// - `String -> Int`: parse, `Result.Err` if invalid
    /// - `String -> Float`: parse, `Result.Err` if invalid
    /// - `Int -> Bool`: `0`/`1` only, else `Result.Err`
    ///
    /// `newtype` representation reads use checker metadata. If private
    /// `repr visibility` blocks an external edge, the checker requires an explicit
    /// `TryInto` instance instead.
    #[async_recursion]
    async fn read(
        &mut self,
        id: ExprId,
        expr: ExprId,
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        let rty = self.checked.expr_target(id, "read");
        let has_inst =
            matches!(&self.checked.expr(id).aux, ExprAux::InstanceCall { .. });
        if let Some(meta) = self.approved_newtype_edge_meta(id) {
            let val = self.value_with_context_meta(val, meta);
            let payload = self.make_result_ok_value(val, span);
            Ok(self.value_for_expr(id, payload))
        } else if has_inst {
            let val_id = self.add_value(val, span);
            let mid = self.arena.intern("try-into");
            self.dispatch_class_method_value(call::ClassDispatch {
                dispatch_expr_id: Some(id),
                output_expr_id: Some(id),
                output_ty: None,
                class: ClassId::TRY_INTO,
                method: mid,
                args: SmallVec::from_slice(&[val_id]),
                span,
            })
            .await
        } else {
            self.read_target_value(&val, rty, span)
                .map(|payload| self.value_for_expr(id, payload))
        }
    }

    fn read_target_value(
        &mut self,
        val: &Value,
        target: RuntimeTyId,
        span: Span,
    ) -> Result<Payload> {
        if self.checked.types.matches(val.ty, val.repr, target) {
            let val = self.value_with_context_meta(
                val.clone(),
                self.checked.types.meta(target),
            );
            Ok(self.make_result_ok_value(val, span))
        } else {
            match self.resolve_read_target(target) {
                ReadTarget::Object { target, fields } => {
                    self.read_to_object(&val.payload, target, &fields, span)
                }
                ReadTarget::Ty { target, convert } => self
                    .read_ty_runtime_value(val, convert, span)
                    .map(|rv| self.retype_read_ok(rv, target, span)),
            }
        }
    }

    fn retype_read_ok(
        &mut self,
        result: Payload,
        target: RuntimeTyId,
        span: Span,
    ) -> Payload {
        if self.result_payload_is_ok(&result) {
            let inner = self
                .unwrap_result_ok(&result)
                .unwrap_or_else(|_| typechecked!("read", "Result.Ok"));
            self.make_result_ok_typed(inner, target, span)
        } else {
            result
        }
    }

    /// Resolve a `RuntimeTyId` to a `ReadTarget`, handling alias transparency.
    fn resolve_read_target(&self, rty: RuntimeTyId) -> ReadTarget {
        self.checked.types.object_fields(rty).map_or_else(
            || ReadTarget::Ty {
                target: rty,
                convert: self.checked.types.repr(rty),
            },
            |fields| ReadTarget::Object {
                target: rty,
                fields: fields.clone(),
            },
        )
    }

    /// Read using an exact type, preserving type arguments.
    fn read_ty_runtime_value(
        &mut self,
        val: &Value,
        target: RuntimeTyId,
        span: Span,
    ) -> Result<Payload> {
        match self.checked.types.get(target).clone() {
            typecheck::Ty::Array(elem) => {
                self.read_array_value(val, RuntimeTyId::from(elem), span)
            }
            typecheck::Ty::Option(inner) => {
                self.read_option_value(val, RuntimeTyId::from(inner), span)
            }
            typecheck::Ty::Object(fields) => {
                self.read_to_object(&val.payload, target, &fields, span)
            }
            typecheck::Ty::Range => self.read_range_value(val, span),
            typecheck::Ty::Json => {
                let mut ctx = class::ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                class::TryInto::try_into_value(
                    &mut ctx,
                    val,
                    &typecheck::Ty::Json,
                )
            }
            ty => {
                let mid = self.arena.intern("try-into");
                self.dispatch_convert_value(
                    ClassId::TRY_INTO,
                    mid,
                    val,
                    &ty,
                    span,
                )
            }
        }
    }

    fn read_range_value(&mut self, val: &Value, span: Span) -> Result<Payload> {
        match &val.payload {
            Payload::Array(elems) => {
                let vals: SmallVec<[i64; 4]> = elems
                    .iter()
                    .map(|id| match self.arena.payload(*id) {
                        Some(Payload::Int(n)) => *n,
                        _ => typechecked!("Array[Int] read Range", "Int"),
                    })
                    .collect();
                let step = vals.windows(2).next().map(|w| match w {
                    [a, b] => (*b as i128) - (*a as i128),
                    _ => typechecked!("Array[Int] read Range", "pair window"),
                });
                let contiguous = step.is_none_or(|s| {
                    matches!(s, 1 | -1)
                        && vals.windows(2).all(|w| match w {
                            [a, b] => (*b as i128) - (*a as i128) == s,
                            _ => {
                                typechecked!(
                                    "Array[Int] read Range",
                                    "pair window"
                                )
                            }
                        })
                });
                Ok(if contiguous {
                    let range =
                        vals.first().copied().zip(vals.last().copied()).map_or(
                            Payload::Range {
                                start: 0,
                                end: 0,
                                inclusive: false,
                            },
                            |(start, end)| Payload::Range {
                                start,
                                end,
                                inclusive: true,
                            },
                        );
                    self.make_result_ok_typed(
                        range,
                        RuntimeTyId::from(typecheck::TyArena::RANGE),
                        span,
                    )
                } else {
                    self.make_result_err("expected contiguous range", span)
                })
            }
            _ => Ok(self.make_result_err("expected array", span)),
        }
    }

    /// Read a JSON array as `Array[T]`.
    fn read_array_value(
        &mut self,
        val: &Value,
        elem: RuntimeTyId,
        span: Span,
    ) -> Result<Payload> {
        match &val.payload {
            Payload::Json(j) => match j.as_ref() {
                serde_json::Value::Array(arr) => {
                    let elems =
                        arr.iter().try_fold(SmallVec::new(), |mut acc, jv| {
                            let fval = self.value_from_meta(
                                Payload::Json(Arc::new(jv.clone())),
                                self.checked.types.meta_json(),
                            );
                            match self.read_target_value(&fval, elem, span) {
                                Ok(rv) if self.result_payload_is_ok(&rv) => {
                                    let inner = self
                                        .unwrap_result_ok(&rv)
                                        .unwrap_or_else(|_| {
                                            fval.payload.clone()
                                        });
                                    let id = self.arena.add_typed(
                                        inner,
                                        self.checked.types.meta(elem),
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
                        Ok(elems) => {
                            let ty = self.checked.types.array(elem);
                            self.make_result_ok_typed(
                                Payload::Array(Arc::new(elems)),
                                ty,
                                span,
                            )
                        }
                        Err(msg) => self.make_result_err(&msg, span),
                    })
                }
                _ => Ok(self.make_result_err("expected array", span)),
            },
            Payload::Array(elems) => {
                let ty = self.checked.types.array(elem);
                Ok(self.make_result_ok_typed(
                    Payload::Array(elems.clone()),
                    ty,
                    span,
                ))
            }
            _ => Ok(self.make_result_err("expected array", span)),
        }
    }

    /// Read a value as `Option[T]`, treating JSON null as `None`.
    fn read_option_value(
        &mut self,
        val: &Value,
        inner: RuntimeTyId,
        span: Span,
    ) -> Result<Payload> {
        match &val.payload {
            Payload::Json(j)
                if matches!(j.as_ref(), serde_json::Value::Null) =>
            {
                let ty = self.checked.types.option(inner);
                Ok(self.make_result_ok_typed(Payload::none(), ty, span))
            }
            _ => match self.read_target_value(val, inner, span) {
                Ok(rv) if self.result_payload_is_ok(&rv) => {
                    let inner_val =
                        self.unwrap_result_ok(&rv).unwrap_or_else(|_| {
                            typechecked!("Option read", "Result.Ok")
                        });
                    let inner_id = self.arena.add_typed(
                        inner_val,
                        self.checked.types.meta(inner),
                        span,
                    );
                    let ty = self.checked.types.option(inner);
                    Ok(self.make_result_ok_typed(
                        Payload::some(inner_id),
                        ty,
                        span,
                    ))
                }
                Ok(rv) => Ok(rv),
                Err(e) => Err(e),
            },
        }
    }

    fn resolve_read_field_target(&self, ty: RuntimeTyId) -> ReadTarget {
        self.resolve_read_target(ty)
    }

    /// Convert a JSON or Object value to a typed object via `read`.
    fn read_to_object(
        &mut self,
        val: &Payload,
        target: RuntimeTyId,
        fields: &indexmap::IndexMap<StringId, typecheck::TyId>,
        span: Span,
    ) -> Result<Payload> {
        match val {
            Payload::Json(j) => match j.as_ref() {
                serde_json::Value::Object(obj) => {
                    self.read_json_object(obj, target, fields, span)
                }
                _ => {
                    let msg = "cannot read non-object JSON as object";
                    Ok(self.make_result_err(msg, span))
                }
            },
            Payload::Object(obj) => {
                self.read_native_object(obj, target, fields, span)
            }
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
        target: RuntimeTyId,
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
                        let fval = self.value_from_meta(
                            Payload::Json(Arc::new(jv.clone())),
                            self.checked.types.meta_json(),
                        );
                        match self
                            .resolve_read_field_target(RuntimeTyId::from(fty))
                        {
                            ReadTarget::Ty { .. } => {
                                match self.read_target_value(
                                    &fval,
                                    RuntimeTyId::from(fty),
                                    span,
                                ) {
                                    Ok(rv)
                                        if self.result_payload_is_ok(&rv) =>
                                    {
                                        let inner = self
                                            .unwrap_result_ok(&rv)
                                            .unwrap_or_else(|_| {
                                                fval.payload.clone()
                                            });
                                        let vid = self.arena.add_typed(
                                            inner,
                                            self.checked
                                                .types
                                                .meta(RuntimeTyId::from(fty)),
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
                            ReadTarget::Object { target, fields } => match self
                                .read_to_object(
                                    &fval.payload,
                                    target,
                                    &fields,
                                    span,
                                ) {
                                Ok(rv) if self.result_payload_is_ok(&rv) => {
                                    let inner = self
                                        .unwrap_result_ok(&rv)
                                        .unwrap_or_else(|_| {
                                            fval.payload.clone()
                                        });
                                    let vid = self.arena.add_typed(
                                        inner,
                                        self.checked
                                            .types
                                            .meta(RuntimeTyId::from(fty)),
                                        span,
                                    );
                                    result.insert(fid, vid);
                                }
                                Ok(rv) => {
                                    let msg = self.extract_result_err_msg(&rv);
                                    err = Some(msg);
                                }
                                Err(e) => {
                                    err = Some(e.to_string());
                                }
                            },
                        }
                    }
                }
            }
        });

        match err {
            Some(msg) => Ok(self.make_result_err(&msg, span)),
            None => {
                let obj = Payload::Object(Arc::new(result));
                Ok(self.make_result_ok_typed(obj, target, span))
            }
        }
    }

    /// Read fields from a native object, validating field types.
    fn read_native_object(
        &mut self,
        obj: &Arc<indexmap::IndexMap<StringId, ValueId>>,
        target: RuntimeTyId,
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
                    Some(&vid) => match self.arena.value(vid).cloned() {
                        Some(fval) => {
                            match self.resolve_read_field_target(
                                RuntimeTyId::from(fty),
                            ) {
                                ReadTarget::Ty { .. } => match self
                                    .read_target_value(
                                        &fval,
                                        RuntimeTyId::from(fty),
                                        span,
                                    ) {
                                    Ok(rv)
                                        if self.result_payload_is_ok(&rv) =>
                                    {
                                        let inner = self
                                            .unwrap_result_ok(&rv)
                                            .unwrap_or_else(|_| {
                                                fval.payload.clone()
                                            });
                                        let new_vid = self.arena.add_typed(
                                            inner,
                                            self.checked
                                                .types
                                                .meta(RuntimeTyId::from(fty)),
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
                                ReadTarget::Object { target, fields } => {
                                    match self.read_to_object(
                                        &fval.payload,
                                        target,
                                        &fields,
                                        span,
                                    ) {
                                        Ok(rv)
                                            if self
                                                .result_payload_is_ok(&rv) =>
                                        {
                                            let inner = self
                                                .unwrap_result_ok(&rv)
                                                .unwrap_or_else(|_| {
                                                    fval.payload.clone()
                                                });
                                            let new_vid = self.arena.add_typed(
                                                inner,
                                                self.checked.types.meta(
                                                    RuntimeTyId::from(fty),
                                                ),
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
                Ok(self.make_result_ok_typed(obj, target, span))
            }
        }
    }

    /// Evaluate a type annotation: `(expr) : Type`.
    ///
    /// Type validation is handled statically by the typechecker; at runtime
    /// this is a pass-through that simply evaluates the inner expression.
    /// Approved `newtype` annotation edges can still update value metadata so
    /// runtime class dispatch sees the checked `newtype` type.
    #[async_recursion]
    async fn annotate(&mut self, id: ExprId, expr: ExprId) -> Result<Value> {
        match self.approved_newtype_edge_meta(id) {
            Some(meta) => {
                let val = self.eval(expr).await?;
                Ok(self.value_with_context_meta(val, meta))
            }
            None => {
                let payload = self.eval_payload(expr).await?;
                Ok(self.value_for_expr(id, payload))
            }
        }
    }

    /// Execute a `let` binding with destructuring.
    ///
    /// Type annotations are validated statically by the typechecker; at runtime
    /// this simply evaluates the expression and destructures into the pattern.
    #[async_recursion]
    async fn r#let(
        &mut self,
        pat: &BindingPattern,
        expr_id: ExprId,
        span: Span,
    ) -> Result<()> {
        let val = self.eval(expr_id).await?;
        match pat {
            BindingPattern::Var(name) => {
                let val = self.value_for_binding(expr_id, val);
                let id = self.add_value(val, span);
                self.env.scopes.bind(*name, id);
                Ok(())
            }
            _ => self.destructure(pat, &val, span),
        }
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
            OutputFormat::Default => self.display_value(&val),
            OutputFormat::Json => {
                let json = self.jsonify_value(&val);
                serde_json::to_string_pretty(&json)
                    .unwrap_or_else(|_| invariant!("JSON serializable"))
            }
            OutputFormat::Raw => self.display_raw_value(&val),
        };

        // Write to target
        match output.target {
            OutputTarget::Stdout => self.io.stdoutline(&text, span).await,
            OutputTarget::Stderr => self.io.stderrline(&text, span).await,
            OutputTarget::File(path_expr) => {
                let path_val = self.eval_payload(path_expr).await?;
                let path = self.filepath(&path_val);
                self.io.write(&path, &text, span).await
            }
        }
    }

    /// Evaluate a JSON object literal.
    ///
    /// Evaluates each field expression and converts to JSON via `jsonify_value`.
    /// Returns `Payload::Json(Object)`.
    #[async_recursion]
    #[allow(clippy::while_let_on_iterator)]
    async fn json(&mut self, fields: &[(StringId, ExprId)]) -> Result<Payload> {
        let mut obj = serde_json::Map::new();
        // Process fields sequentially to maintain order
        let mut it = fields.iter();
        while let Some((key, expr_id)) = it.next() {
            let val = self.eval(*expr_id).await?;
            let json_val = self.jsonify_value(&val);
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
        let base_val = self.eval_payload(base).await?;

        // Get the key string
        let key_str = match key {
            JsonAccessKey::Field(name) => self.arena.strings.resolve(*name),
            JsonAccessKey::Expr(expr_id) => {
                let key_val = self.eval_payload(*expr_id).await?;
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
                    self.checked.types.meta_bool(),
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
                        (v, self.checked.types.meta_float())
                    },
                    |i| (Payload::Int(i), self.checked.types.meta_int()),
                );
                let val_id = self.add_val(val, meta, span);
                Ok(self.make_some_scalar(val_id))
            }
            Some(serde_json::Value::String(s)) => {
                let sid = self.arena.intern(&s);
                let val_id = self.add_val(
                    Payload::String(sid),
                    self.checked.types.meta_string(),
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
