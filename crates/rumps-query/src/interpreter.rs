//! AST interpreter for the RUMPS query language.
//!
//! The interpreter is async because `Database` and `Transaction` methods are async.
//! All variable access (both locals and globals) goes through async `Database` methods.
//!
//! # Runtime Conversion Boundary
//!
//! Language conversions use class method dispatch. `write`, interpolation,
//! `matches`, `raise`, `as String`, `as Json`, and JSON construction are
//! checked as ordinary class method calls before runtime evaluation.
//!
//! Stored values are already known to be `Storable`. Stored JSON is loaded via
//! [`Interpreter::unjsonify`].
//!
//! ## Numeric Coercion
//!
//! For comparison and arithmetic operators, mixed numeric types are coerced:
//!
//! - `Int` vs `Float`: The `Int` is promoted to `Float`
//! - Comparisons (`==`, `<`, etc.) work across `Int`/`Float` boundaries
//! - Division always produces `Float` (use `//` for floor division)
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
mod hoist;
pub(crate) mod instance;
mod modules;
mod ops;
mod pattern;
mod transaction;
mod types;
mod variant;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_recursion::async_recursion;
use convert::RawDisplay;
use env::Environment;
use ordered_float::OrderedFloat;
use rumps_storage::{Database, Transaction};
use smallvec::SmallVec;

use crate::ast::{
    pragma, Ast, AstClassMethod, BinOp, BindingPattern, Expr, ExprId, Import,
    ImportItem, JsonAccessKey, JsonAccessKind, Literal, NumericLit,
    OutputFormat, OutputTarget, Stmt, StmtId, TxnId, TypeDefAst, TypeParam,
    TypePattern, UnOp, WriteExpr,
};
use crate::builtins::{BuiltinCtx, CallMeta, Maps, OutputMeta, Values};
use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::io::IoContext;
use crate::resolve::{InstanceMap, ResolveCtx};
use crate::typecheck::{
    CheckedProgram, ClassDef, ClassRegistry, ClassShape, ExprAux, MethodSpec,
    RuntimeTyId, Scheme, Ty, TyArena, TyVar,
};
use crate::value::{
    CapturedEnv, FunctionDef, Map, MapNode, Payload, TypeDef, TypeId,
    TypeRegistry, Value, ValueArena, ValueId, ValueMeta, VariantDef,
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
pub(crate) struct Interpreter<'ast, 'io> {
    /// The parsed AST (borrowed; immutable during interpretation).
    ast: &'ast Ast,

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
    io: &'io mut dyn IoContext,

    /// Checked program metadata produced by typechecking.
    checked: CheckedProgram,

    /// Registry of class methods for dispatch.
    class_methods: class::ClassMethods,

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

    /// Parsed program pragmas; inert in pragma phase `1`.
    program_pragmas: pragma::Program,
}

// Public API
impl<'ast, 'io> Interpreter<'ast, 'io> {
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
        ast: &'ast mut Ast,
        stmts: &[StmtId],
        db: Database,
        io: &'io mut dyn IoContext,
        interactive: bool,
        interner: StringInterner,
        program_pragmas: pragma::Program,
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
        let resolve_class_registry =
            Self::resolve_class_registry(ast, stmts, &mut arena);

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
            program_pragmas.clone(),
        )
        .check(stmts, &registry, &arena)?;

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
            user_instances: instance::RuntimeInstanceRegistry::new(),
            resolved_instances,
            runtime_ty_substs: Vec::new(),
            program_pragmas,
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
                    Stmt::Variant {
                        name,
                        type_params,
                        def,
                        ..
                    } => {
                        let n = self.arena.strings.resolve(name);
                        self.variant_decl(&n, &type_params, &def)?
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

    /// Create an interpreter with a pre-created arena and registry.
    ///
    /// Used by tests that need direct control over the arena/registry,
    /// bypassing name resolution and typechecking.
    #[cfg(test)]
    pub(crate) fn with_arena(
        ast: &'ast Ast,
        db: Database,
        io: &'io mut dyn IoContext,
        mut arena: ValueArena,
        registry: TypeRegistry,
    ) -> Self {
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
            program_pragmas: pragma::Program::default(),
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
            user_instances: instance::RuntimeInstanceRegistry::new(),
            resolved_instances: HashMap::new(),
            runtime_ty_substs: Vec::new(),
            program_pragmas: pragma::Program::default(),
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
                    self.dispatch_class_method_value(class::Dispatch {
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
                    self.postfix(op, val, span).await
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
                let Payload::String(id) = val.payload else {
                    typechecked!("raise", "Display:display returned String")
                };
                let msg = self.arena.get_str(id).unwrap_or("").to_owned();
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
                self.default_value(id, span).await.map(Evaluated::Payload)
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

    async fn eval_payload(&mut self, id: ExprId) -> Result<Payload> {
        self.eval(id).await.map(|v| v.payload)
    }
}

// Private helpers
impl Interpreter<'_, '_> {
    fn resolve_class_registry(
        ast: &Ast,
        stmts: &[StmtId],
        arena: &mut ValueArena,
    ) -> ClassRegistry {
        let mut ty_arena = TyArena::new();
        let mut reg = ClassRegistry::builtins(
            &mut |s| arena.strings.intern(s),
            &mut ty_arena,
        );
        Self::register_resolve_classes(ast, stmts, &mut reg);
        reg
    }

    fn register_resolve_classes(
        ast: &Ast,
        stmts: &[StmtId],
        reg: &mut ClassRegistry,
    ) {
        stmts.iter().for_each(|&id| {
            ast.get_stmt(id).into_iter().for_each(|stmt| match stmt {
                Stmt::ClassDef {
                    name,
                    methods,
                    pragmas,
                    ..
                } => {
                    let _ = reg.register(Self::resolve_class_def(
                        *name, methods, pragmas,
                    ));
                }
                Stmt::Module { body, .. } => {
                    Self::register_resolve_classes(ast, body, reg);
                }
                _ => {}
            });
        });
    }

    fn resolve_class_def(
        name: StringId,
        methods: &[AstClassMethod],
        pragmas: &pragma::Class,
    ) -> ClassDef {
        let required_methods = if pragmas.required_methods.0.is_empty() {
            methods.iter().map(|m| m.sig.name).collect()
        } else {
            pragmas.required_methods.0.iter().copied().collect()
        };
        ClassDef {
            name,
            shape: ClassShape::Concrete { params: 0 },
            assoc_types: Default::default(),
            methods: methods
                .iter()
                .map(|m| {
                    (
                        m.sig.name,
                        MethodSpec::Standard(Scheme::mono(TyArena::ERROR)),
                    )
                })
                .collect(),
            required_methods,
            supers: Default::default(),
        }
    }

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
            Stmt::Variant {
                name,
                type_params,
                def,
                ..
            } => {
                let n = self.arena.strings.resolve(name);
                self.variant_decl(&n, &type_params, &def)
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
    async fn populate_module(
        &mut self,
        ids: &[StmtId],
        module: &mut env::UserModule,
        mod_path: &str,
        span: Span,
    ) -> Result<()> {
        self.populate_module_defaults(ids)?;
        self.populate_module_instances(ids).await?;
        self.populate_module_items(ids, module, mod_path, span)
            .await
    }

    fn populate_module_defaults(&mut self, ids: &[StmtId]) -> Result<()> {
        ids.iter().try_for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::ClassDef { name, methods, .. }) = stmt {
                self.checked
                    .class_registry
                    .lookup_by_name(name)
                    .into_iter()
                    .try_for_each(|class| {
                        self.hoist_class_defaults(class, &methods)
                    })
            } else {
                Ok(())
            }
        })
    }

    #[async_recursion]
    async fn populate_module_items(
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

                        Stmt::Variant {
                            name: type_name,
                            type_params,
                            def,
                            ..
                        } => {
                            // Types are already registered with qualified names
                            // by register_from_ast. The idempotent variant_decl
                            // will skip if already present.
                            let tn = self.arena.strings.resolve(type_name);
                            let qname = format!("{}.{}", mod_path, tn);
                            self.variant_decl(&qname, &type_params, &def)?;
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

                        Stmt::ClassInstance { .. } | Stmt::ClassDef { .. } => {}

                        // Other statements are rejected by the typechecker
                        _ => {}
                    }
                }
                self.populate_module_items(rest, module, mod_path, span)
                    .await
            }
        }
    }

    #[async_recursion]
    async fn populate_module_instances(
        &mut self,
        ids: &[StmtId],
    ) -> Result<()> {
        match ids.split_first() {
            None => Ok(()),
            Some((&id, rest)) => {
                let stmt = self.ast.get_stmt(id).cloned();
                if let Some(Stmt::ClassInstance { methods, .. }) = stmt {
                    self.hoist_class_instance(id, &methods)?;
                }
                self.populate_module_instances(rest).await
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
    fn variant_decl(
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
            typecheck::Ty::Lazy(elem) => {
                let elem = self.runtime_ty(RuntimeTyId::from(elem));
                self.checked.types.lazy(elem)
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
            | (typecheck::Ty::Option(f), typecheck::Ty::Option(a))
            | (typecheck::Ty::Lazy(f), typecheck::Ty::Lazy(a)) => self
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
    /// Evaluates each part as a rewritten string expression.
    ///
    /// Collects parts into a `Vec` then joins, avoiding `O(n^2)` allocations.
    async fn interpolation(&mut self, parts: &[ExprId]) -> Result<Payload> {
        let mut acc = Vec::with_capacity(parts.len());
        let mut rest = parts;

        while let Some((&id, tail)) = rest.split_first() {
            let val = self.eval(id).await?;
            let s = match &val.payload {
                Payload::String(sid) => self
                    .arena
                    .get_str(*sid)
                    .unwrap_or_else(|| invariant!("StringId in arena"))
                    .to_owned(),
                _ => typechecked!(
                    "interpolation",
                    "Formattable:format returned String"
                ),
            };
            acc.push(s);
            rest = tail;
        }

        let joined = acc.join("");
        Ok(Payload::String(self.arena.intern(&joined)))
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
    /// Handles short-circuit evaluation for `and` and `or`.
    /// For class-dispatched operators (`==`, `+`, `<`, etc.), checks for
    /// user-defined class instances before falling through to builtin dispatch.
    async fn binary(
        &mut self,
        id: ExprId,
        lhs: ExprId,
        op: BinOp,
        rhs: ExprId,
        span: Span,
    ) -> Result<Value> {
        match op {
            // Short-circuit `and`; if left is false, don't evaluate right.
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
            // Short-circuit `or`; if left is true, don't evaluate right.
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
            BinOp::Coalesce => {
                let left = self.eval(lhs).await?;
                let right = self.eval(rhs).await?;
                let l = self.add_value(left, span);
                let r = self.add_value(right, span);
                let mid = self.arena.intern("coalesce");
                self.dispatch_class_method_value(class::Dispatch {
                    dispatch_expr_id: Some(id),
                    output_expr_id: Some(id),
                    output_ty: None,
                    class: ClassId::COALESCABLE,
                    method: mid,
                    args: SmallVec::from_slice(&[l, r]),
                    span,
                })
                .await
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
                        .await
                        .map(|payload| self.value_for_expr(id, payload))
                }
            }
        }
    }

    /// Evaluate a unary operation.
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
            self.dispatch_class_method_value(class::Dispatch {
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
                .await
                .map(|payload| self.value_for_expr(id, payload))
        }
    }

    /// Evaluate a type check: `expr is Pattern`.
    ///
    /// Returns `true` if the value matches the pattern, `false` otherwise.
    /// For `VariantBind` patterns, bindings are NOT created here; they are
    /// handled specially by `if_with_bindings` when used as an `if` condition.
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
    /// - `T -> String` through `Into[String]` class dispatch
    /// - `Bool -> Int` (`false` -> `0`, `true` -> `1`)
    /// - `T -> Storable` (identity if T is a Storable member type)
    ///
    /// Note: `as Storable` is the only infallible union cast. Other unions
    /// require `read` for fallible conversion or `match` for type narrowing.
    /// `newtype` representation casts use metadata approved by static
    /// `type visibility` and `repr visibility` checks.
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
                typechecked!("as String", "synthetic Into[String] call")
            } else if target_base == TypeId::JSON {
                typechecked!("as Json", "synthetic Into[Json] call")
            } else {
                let target = self.checked.types.get(rty).clone();
                self.coerce_value(&val, target_base, &target, span)
                    .await
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
            self.dispatch_class_method_value(class::Dispatch {
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
                .await
                .map(|payload| self.value_for_expr(id, payload))
        }
    }

    #[async_recursion]
    async fn read_target_value(
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
                        .await
                }
                ReadTarget::Ty { target, convert } => {
                    let rv =
                        self.read_ty_runtime_value(val, convert, span).await?;
                    Ok(self.retype_read_ok(rv, target, span))
                }
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
    async fn read_ty_runtime_value(
        &mut self,
        val: &Value,
        target: RuntimeTyId,
        span: Span,
    ) -> Result<Payload> {
        match self.checked.types.get(target).clone() {
            typecheck::Ty::Array(elem) => {
                self.read_array_value(val, RuntimeTyId::from(elem), span)
                    .await
            }
            typecheck::Ty::Option(inner) => {
                self.read_option_value(val, RuntimeTyId::from(inner), span)
                    .await
            }
            typecheck::Ty::Object(fields) => {
                self.read_to_object(&val.payload, target, &fields, span)
                    .await
            }
            typecheck::Ty::Range => self.read_range_value(val, span),
            typecheck::Ty::Json => {
                let mid = self.arena.intern("try-into");
                self.class_convert_value(
                    ClassId::TRY_INTO,
                    mid,
                    val,
                    &typecheck::Ty::Json,
                    span,
                )
                .await
            }
            ty => {
                let mid = self.arena.intern("try-into");
                self.class_convert_value(ClassId::TRY_INTO, mid, val, &ty, span)
                    .await
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
    async fn read_array_value(
        &mut self,
        val: &Value,
        elem: RuntimeTyId,
        span: Span,
    ) -> Result<Payload> {
        match &val.payload {
            Payload::Json(j) => match j.as_ref() {
                serde_json::Value::Array(arr) => {
                    let mut elems = SmallVec::new();
                    let mut err = None;
                    let mut vals = arr.iter();
                    while let Some(jv) = vals.next() {
                        if err.is_some() {
                        } else {
                            let fval = self.value_from_meta(
                                Payload::Json(Arc::new(jv.clone())),
                                self.checked.types.meta_json(),
                            );
                            match self
                                .read_target_value(&fval, elem, span)
                                .await
                            {
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
                                    elems.push(id);
                                }
                                Ok(rv) => {
                                    err =
                                        Some(self.extract_result_err_msg(&rv));
                                }
                                Err(e) => {
                                    err = Some(e.to_string());
                                }
                            }
                        }
                    }
                    Ok(match elems {
                        elems if err.is_none() => {
                            let ty = self.checked.types.array(elem);
                            self.make_result_ok_typed(
                                Payload::Array(Arc::new(elems)),
                                ty,
                                span,
                            )
                        }
                        _ => self.make_result_err(
                            &err.unwrap_or_else(|| "unknown error".to_owned()),
                            span,
                        ),
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
    async fn read_option_value(
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
            _ => match self.read_target_value(val, inner, span).await {
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
    #[async_recursion]
    async fn read_to_object(
        &mut self,
        val: &Payload,
        target: RuntimeTyId,
        fields: &indexmap::IndexMap<StringId, typecheck::TyId>,
        span: Span,
    ) -> Result<Payload> {
        match val {
            Payload::Json(j) => match j.as_ref() {
                serde_json::Value::Object(obj) => {
                    self.read_json_object(obj, target, fields, span).await
                }
                _ => {
                    let msg = "cannot read non-object JSON as object";
                    Ok(self.make_result_err(msg, span))
                }
            },
            Payload::Object(obj) => {
                self.read_native_object(obj, target, fields, span).await
            }
            _ => {
                let src = val.type_name(&self.registry, &self.arena);
                let msg = format!("cannot read `{src}` as object");
                Ok(self.make_result_err(&msg, span))
            }
        }
    }

    /// Read fields from a JSON object, converting each field via `read_value`.
    async fn read_json_object(
        &mut self,
        obj: &serde_json::Map<std::string::String, serde_json::Value>,
        target: RuntimeTyId,
        fields: &indexmap::IndexMap<StringId, typecheck::TyId>,
        span: Span,
    ) -> Result<Payload> {
        let mut result = indexmap::IndexMap::new();
        let mut err = None;

        let mut entries = fields.iter();
        while let Some((&fid, &fty)) = entries.next() {
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
                                match self
                                    .read_target_value(
                                        &fval,
                                        RuntimeTyId::from(fty),
                                        span,
                                    )
                                    .await
                                {
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
                                )
                                .await
                            {
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
        }

        match err {
            Some(msg) => Ok(self.make_result_err(&msg, span)),
            None => {
                let obj = Payload::Object(Arc::new(result));
                Ok(self.make_result_ok_typed(obj, target, span))
            }
        }
    }

    /// Read fields from a native object, validating field types.
    async fn read_native_object(
        &mut self,
        obj: &Arc<indexmap::IndexMap<StringId, ValueId>>,
        target: RuntimeTyId,
        fields: &indexmap::IndexMap<StringId, typecheck::TyId>,
        span: Span,
    ) -> Result<Payload> {
        let mut result = indexmap::IndexMap::new();
        let mut err = None;

        let mut entries = fields.iter();
        while let Some((&fid, &fty)) = entries.next() {
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
                                    )
                                    .await
                                {
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
                                    match self
                                        .read_to_object(
                                            &fval.payload,
                                            target,
                                            &fields,
                                            span,
                                        )
                                        .await
                                    {
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
        }

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
    async fn write(&mut self, output: &WriteExpr) -> Result<()> {
        let span = self.ast.expr_span(output.expr).unwrap_or_default();
        let val = self.eval_payload(output.expr).await?;
        let text = match val {
            Payload::String(id) => {
                let s = self.arena.get_str(id).unwrap_or_default();
                match output.format {
                    OutputFormat::Raw => RawDisplay::escape_str(s),
                    OutputFormat::Default | OutputFormat::Json => s.to_owned(),
                }
            }
            _ => typechecked!("write", "Into[String] returned String"),
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
    /// Evaluates each field expression as `Json`.
    /// Returns `Payload::Json(Object)`.
    #[allow(clippy::while_let_on_iterator)]
    async fn json(&mut self, fields: &[(StringId, ExprId)]) -> Result<Payload> {
        let mut obj = serde_json::Map::new();
        // Process fields sequentially to maintain order
        let mut it = fields.iter();
        while let Some((key, expr_id)) = it.next() {
            let val = self.eval_payload(*expr_id).await?;
            let Payload::Json(json) = val else {
                typechecked!("Json literal", "Into[Json] returned Json")
            };
            obj.insert(
                self.arena.strings.resolve(*key).to_owned(),
                json.as_ref().clone(),
            );
        }
        Ok(Payload::Json(Arc::new(serde_json::Value::Object(obj))))
    }

    /// Evaluate JSON field access.
    ///
    /// For `JsonAccessKind::Json` (`.` or `->`): returns `Payload::Json` (null for missing).
    /// For `JsonAccessKind::Scalar` (`..` or `->>`): returns `Option[scalar]`.
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

impl<'i, 'ast, 'io> BuiltinCtx<'i, 'ast, 'io> {
    /// Opens the value arena facet.
    ///
    /// Keep the returned `Values` short lived and do not hold it across
    /// `.await`.
    pub(super) fn vals(&mut self) -> Values<'_, 'i, 'ast, 'io> {
        Values { ctx: self }
    }

    /// Opens the persistent map facet.
    ///
    /// Use this when map operations need ordering or equality dispatch.
    pub(super) fn maps(&mut self) -> Maps<'_, 'i, 'ast, 'io> {
        Maps { ctx: self }
    }

    /// Returns the caller supplied output type when one was provided.
    pub(super) fn output_ty(&self) -> Option<RuntimeTyId> {
        match self.output {
            OutputMeta::Ty(ty) => Some(ty),
            _ => None,
        }
    }

    /// Returns the target type for nullary class builtins.
    pub(super) fn nullary_ty(&self) -> Result<RuntimeTyId> {
        match self.meta {
            Some(CallMeta::Nullary { ty }) => Ok(ty),
            _ => Err(self.runtime_error("missing nullary target type")),
        }
    }

    /// Returns the target type for conversion class builtins.
    pub(super) fn convert_target(&self) -> Result<RuntimeTyId> {
        match self.meta {
            Some(CallMeta::Convert { target, .. }) => Ok(target),
            _ => Err(self.runtime_error("missing conversion target type")),
        }
    }

    /// Returns the approved conversion edge metadata, if dispatch supplied it.
    pub(super) fn approved_edge(&self) -> Option<ValueMeta> {
        match self.meta {
            Some(CallMeta::Convert { edge, .. }) => edge,
            _ => None,
        }
    }

    /// Returns the call site `Span` for diagnostics and value metadata.
    pub(super) fn span(&self) -> Span {
        self.span
    }

    /// Builds a runtime error at this builtin call site.
    pub(super) fn runtime_error(&self, msg: impl Into<String>) -> Error {
        Error::runtime(self.span, msg)
    }

    /// Returns the interpreter `I/O` capability.
    pub(super) fn io(&mut self) -> &mut dyn IoContext {
        self.interp.io
    }

    /// Invokes a callable value with already allocated argument values.
    pub(super) async fn invoke(
        &mut self,
        f: ValueId,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        self.interp.invoke_callable(f, &args, self.span).await
    }

    /// Dispatches a class method and stores the returned `Value`.
    pub(super) async fn class_call(
        &mut self,
        class: ClassId,
        method: StringId,
        args: SmallVec<[ValueId; 4]>,
        output_ty: Option<RuntimeTyId>,
    ) -> Result<ValueId> {
        let value = self
            .interp
            .dispatch_class_method_value(class::Dispatch::internal(
                class, method, args, output_ty, self.span,
            ))
            .await?;
        Ok(self.interp.add_value(value, self.span))
    }

    /// Applies caller requested output refinement to `id`.
    pub(super) fn finish(&mut self, id: ValueId) -> Result<Value> {
        let value = self
            .interp
            .arena
            .value(id)
            .cloned()
            .unwrap_or_else(|| invariant!("ValueId in arena"));
        let output =
            self.interp
                .refine_variant_output(self.output, &value.payload, &[]);
        Ok(self.interp.value_for_output_value(output, value))
    }
}

impl Values<'_, '_, '_, '_> {
    /// Adds a `Payload` using metadata inferred from the payload shape.
    pub(super) fn add(&mut self, v: Payload) -> ValueId {
        let meta = self
            .ctx
            .interp
            .checked
            .types
            .meta_for_payload(&self.ctx.interp.arena, &v);
        self.ctx.interp.arena.add_typed(v, meta, self.ctx.span)
    }

    /// Adds a complete `Value` without changing its existing metadata.
    pub(super) fn add_value(&mut self, v: Value) -> ValueId {
        self.ctx.interp.arena.add(v, self.ctx.span)
    }

    /// Adds a `Payload` with metadata for an explicit runtime type.
    pub(super) fn add_typed(&mut self, v: Payload, ty: RuntimeTyId) -> ValueId {
        let meta = self.ctx.interp.checked.types.meta(ty);
        self.ctx.interp.arena.add_typed(v, meta, self.ctx.span)
    }

    /// Returns value metadata for an explicit runtime type.
    pub(super) fn meta_for_ty(&mut self, ty: RuntimeTyId) -> ValueMeta {
        self.ctx.interp.checked.types.meta(ty)
    }

    /// Adds a `Payload` with existing value metadata.
    pub(super) fn add_meta(&mut self, v: Payload, meta: ValueMeta) -> ValueId {
        let value = self.ctx.interp.value_from_meta(v, meta);
        self.add_value(value)
    }

    /// Copies a value and applies existing value metadata.
    pub(super) fn id_with_meta(
        &mut self,
        id: ValueId,
        meta: ValueMeta,
    ) -> ValueId {
        let value = self
            .ctx
            .interp
            .arena
            .value(id)
            .cloned()
            .unwrap_or_else(|| invariant!("ValueId in arena"));
        let span = self.ctx.interp.arena.span(id).unwrap_or(self.ctx.span);
        let value = self.ctx.interp.value_with_context_meta(value, meta);
        self.ctx.interp.arena.add(value, span)
    }

    /// Adds a variant payload with metadata for a variant `TypeId` and args.
    pub(super) fn add_variant(
        &mut self,
        v: Payload,
        id: TypeId,
        args: SmallVec<[RuntimeTyId; 4]>,
    ) -> ValueId {
        let ty = match id {
            TypeId::OPTION => match args.as_slice() {
                [inner] => self.ctx.interp.checked.types.option(*inner),
                _ => invariant!("Option variant args"),
            },
            TypeId::LAZY => match args.as_slice() {
                [inner] => self.ctx.interp.checked.types.lazy(*inner),
                _ => invariant!("Lazy variant args"),
            },
            TypeId::RESULT => match args.as_slice() {
                [ok, err] => self.ctx.interp.checked.types.result(*ok, *err),
                _ => invariant!("Result variant args"),
            },
            _ => self.ctx.interp.checked.types.named(id, args),
        };
        self.add_typed(v, ty)
    }

    /// Returns the complete `Value` for a valid `ValueId`.
    pub(super) fn value(&self, id: ValueId) -> Result<&Value> {
        self.ctx
            .interp
            .arena
            .value(id)
            .ok_or_else(|| invariant!("ValueId in arena"))
    }

    /// Returns the `Payload` for a valid `ValueId`.
    pub(super) fn payload(&self, id: ValueId) -> Result<&Payload> {
        self.ctx
            .interp
            .arena
            .payload(id)
            .ok_or_else(|| invariant!("ValueId in arena"))
    }

    /// Returns stored metadata for a value, if present.
    pub(super) fn meta(&self, id: ValueId) -> Option<ValueMeta> {
        self.ctx.interp.arena.meta(id)
    }

    /// Interns a string in the value arena.
    pub(super) fn intern(&mut self, s: &str) -> StringId {
        self.ctx.interp.arena.intern(s)
    }

    /// Extracts a statically checked `Bool` payload.
    pub(super) fn bool_payload(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<bool> {
        match self.payload(id)? {
            Payload::Bool(b) => Ok(*b),
            _ => typechecked!(label, "Bool"),
        }
    }

    /// Extracts a statically checked string payload id.
    pub(super) fn string_payload(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<StringId> {
        match self.payload(id)? {
            Payload::String(s) => Ok(*s),
            _ => typechecked!(label, "String"),
        }
    }

    /// Extracts a string id through the arena string coercion helper.
    pub(super) fn string_id(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<StringId> {
        self.ctx
            .interp
            .arena
            .get_string_id(id)
            .ok_or_else(|| typechecked!(label, "String"))
    }

    /// Returns an interned string slice for a valid `StringId`.
    pub(super) fn str(&self, id: StringId) -> Result<&str> {
        self.ctx
            .interp
            .arena
            .get_str(id)
            .ok_or_else(|| invariant!("StringId in arena"))
    }

    /// Extracts a statically checked `Int` payload.
    pub(super) fn int_payload(&self, id: ValueId, label: &str) -> Result<i64> {
        match self.payload(id)? {
            Payload::Int(n) => Ok(*n),
            _ => typechecked!(label, "Int"),
        }
    }

    /// Extracts a statically checked `Float` payload.
    pub(super) fn float_payload(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<f64> {
        match self.payload(id)? {
            Payload::Float(n) => Ok(n.0),
            _ => typechecked!(label, "Float"),
        }
    }

    /// Returns the source language type id when value metadata provides one.
    pub(super) fn value_base_type(&self, id: ValueId) -> Option<TypeId> {
        self.meta(id).and_then(|meta| {
            self.runtime_base_type(meta.repr)
                .or_else(|| self.runtime_base_type(meta.ty))
        })
    }

    /// Converts a runtime type id back to a source language type id.
    pub(super) fn runtime_base_type(&self, ty: RuntimeTyId) -> Option<TypeId> {
        self.ctx.interp.checked.types.to_type_id(ty)
    }

    /// Normalizes generic runtime type references through current substitutions.
    pub(super) fn runtime_ty(&mut self, ty: RuntimeTyId) -> RuntimeTyId {
        self.ctx.interp.runtime_ty(ty)
    }

    /// Returns the normalized shape of a runtime type.
    pub(super) fn ty(&mut self, ty: RuntimeTyId) -> Ty {
        let ty = self.runtime_ty(ty);
        self.ctx.interp.checked.types.get(ty).clone()
    }

    /// Converts a source language type id to a runtime type id.
    pub(super) fn type_id(&mut self, id: TypeId) -> RuntimeTyId {
        self.ctx.interp.checked.types.type_id(id)
    }

    /// Returns the source language variant base type for a complete `Value`.
    pub(super) fn value_variant_base_type(
        &self,
        value: &Value,
    ) -> Option<TypeId> {
        self.runtime_base_type(value.repr)
            .or_else(|| self.runtime_base_type(value.ty))
    }

    /// Returns the source language type name for `id`.
    pub(super) fn type_name(&self, id: TypeId) -> Option<&str> {
        self.ctx
            .interp
            .registry
            .type_name(id, &self.ctx.interp.arena)
    }

    /// Returns the source language variant name for `id` and `tag`.
    pub(super) fn variant_name(&self, id: TypeId, tag: u8) -> Option<&str> {
        self.ctx
            .interp
            .registry
            .variant_name(id, tag, &self.ctx.interp.arena)
    }

    /// Returns the compiled regex pattern for `idx`.
    pub(super) fn regex_pattern(&self, idx: u32) -> Option<&str> {
        self.ctx
            .interp
            .checked
            .regex_cache
            .get(idx as usize)
            .map(|re| re.as_str())
    }

    /// Returns the statically known return type of a callable value.
    pub(super) fn callable_ret_ty(&self, id: ValueId) -> Option<RuntimeTyId> {
        self.value(id)
            .ok()
            .and_then(|value| self.ctx.interp.callable_ret(value.ty))
    }

    /// Returns type args for a value of the expected variant `TypeId`.
    pub(super) fn variant_args(
        &self,
        id: ValueId,
        expected: TypeId,
    ) -> Option<SmallVec<[RuntimeTyId; 4]>> {
        self.meta(id)
            .map(|meta| meta.repr)
            .or_else(|| self.ctx.interp.arena.ty(id))
            .and_then(|ty| match self.ctx.interp.checked.types.get(ty) {
                Ty::Option(inner) if expected == TypeId::OPTION => {
                    Some([RuntimeTyId::from(*inner)].into_iter().collect())
                }
                Ty::Lazy(inner) if expected == TypeId::LAZY => {
                    Some([RuntimeTyId::from(*inner)].into_iter().collect())
                }
                Ty::Result(ok, err) if expected == TypeId::RESULT => Some(
                    [RuntimeTyId::from(*ok), RuntimeTyId::from(*err)]
                        .into_iter()
                        .collect(),
                ),
                Ty::Named(id, args) if *id == expected => {
                    Some(args.iter().copied().map(RuntimeTyId::from).collect())
                }
                _ => None,
            })
    }

    /// Returns an owned array id snapshot for async traversal.
    pub(super) fn array_ids(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<Arc<SmallVec<[ValueId; 4]>>> {
        match self.payload(id)? {
            Payload::Array(ids) => Ok(ids.clone()),
            _ => typechecked!(label, "Array"),
        }
    }

    /// Returns a borrowed array payload for immediate sync use.
    pub(super) fn array(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<&SmallVec<[ValueId; 4]>> {
        self.ctx
            .interp
            .arena
            .get_array(id)
            .ok_or_else(|| typechecked!(label, "Array"))
    }

    /// Takes an owned array payload for update operations.
    pub(super) fn take_array(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<SmallVec<[ValueId; 4]>> {
        match self.payload(id)? {
            Payload::Array(ids) => Ok(Arc::unwrap_or_clone(ids.clone())),
            _ => typechecked!(label, "Array"),
        }
    }

    /// Materializes map entries once for builtin traversal.
    pub(super) fn map_entries(
        &self,
        id: ValueId,
        label: &str,
    ) -> Result<SmallVec<[(ValueId, ValueId); 8]>> {
        match self.payload(id)? {
            Payload::Map(m) => Ok(m.entries()),
            _ => typechecked!(label, "Map"),
        }
    }

    /// Builds a `Result.Ok` value with metadata from `v`.
    pub(super) fn result_ok(&mut self, v: ValueId) -> ValueId {
        let ok = Payload::ok(v);
        let ok_ty = self
            .ctx
            .interp
            .arena
            .meta(v)
            .map(|m| m.ty)
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
        let err_ty = RuntimeTyId::from(TyArena::STRING);
        let ty = self.ctx.interp.checked.types.result(ok_ty, err_ty);
        let meta = self.ctx.interp.checked.types.meta(ty);
        self.ctx.interp.arena.add_typed(ok, meta, self.ctx.span)
    }

    /// Builds a `Result.Err` value with metadata from `msg`.
    pub(super) fn result_err(&mut self, msg: ValueId) -> ValueId {
        let err = Payload::err(msg);
        let ok_ty = RuntimeTyId::from(TyArena::UNIT);
        let err_ty = self
            .ctx
            .interp
            .arena
            .meta(msg)
            .map(|m| m.ty)
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::STRING));
        let ty = self.ctx.interp.checked.types.result(ok_ty, err_ty);
        let meta = self.ctx.interp.checked.types.meta(ty);
        self.ctx.interp.arena.add_typed(err, meta, self.ctx.span)
    }

    /// Builds an `Option.Some` value with metadata from `v`.
    pub(super) fn option_some(&mut self, v: ValueId) -> ValueId {
        let some = Payload::some(v);
        let elem = self
            .ctx
            .interp
            .arena
            .meta(v)
            .map(|m| m.ty)
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
        let ty = self.ctx.interp.checked.types.option(elem);
        let meta = self.ctx.interp.checked.types.meta(ty);
        self.ctx.interp.arena.add_typed(some, meta, self.ctx.span)
    }

    /// Builds an `Option.None` value with the default option metadata.
    pub(super) fn option_none(&mut self) -> ValueId {
        let none = Payload::none();
        let ty = self
            .ctx
            .interp
            .checked
            .types
            .option(RuntimeTyId::from(TyArena::UNIT));
        let meta = self.ctx.interp.checked.types.meta(ty);
        self.ctx.interp.arena.add_typed(none, meta, self.ctx.span)
    }
}

impl Maps<'_, '_, '_, '_> {
    /// Looks up a key using class backed map ordering.
    pub(super) async fn lookup(
        &mut self,
        map: &Map,
        key: ValueId,
    ) -> Result<Option<ValueId>> {
        self.lookup_node(map.root(), key).await
    }

    /// Inserts a key and value using class backed map ordering.
    pub(super) async fn insert(
        &mut self,
        map: &Map,
        key: ValueId,
        val: ValueId,
    ) -> Result<Map> {
        let (root, added) = self.insert_node(map.root(), key, val).await?;
        let len = if added {
            map.len().saturating_add(1)
        } else {
            map.len()
        };
        Ok(Map::from_root(root, len))
    }

    /// Removes a key using class backed map ordering.
    pub(super) async fn remove(
        &mut self,
        map: &Map,
        key: ValueId,
    ) -> Result<Map> {
        let (root, removed) = self.remove_node(map.root(), key).await?;
        let len = if removed {
            map.len().saturating_sub(1)
        } else {
            map.len()
        };
        Ok(Map::from_root(root, len))
    }

    /// Merges two maps using class backed key ordering.
    pub(super) async fn merge(&mut self, l: &Map, r: &Map) -> Result<Map> {
        let entries = r.entries();
        self.insert_entries(l.clone(), entries.as_slice()).await
    }

    /// Builds a map from entries using class backed key ordering.
    #[allow(clippy::wrong_self_convention)]
    pub(super) async fn from_entries(
        &mut self,
        entries: impl IntoIterator<Item = (ValueId, ValueId)>,
    ) -> Result<Map> {
        let entries: Vec<_> = entries.into_iter().collect();
        self.insert_entries(Map::new(), entries.as_slice()).await
    }

    /// Compares two maps for equality through class dispatch.
    pub(super) async fn eq(&mut self, l: &Map, r: &Map) -> Result<bool> {
        if l.len() == r.len() {
            let l_entries = l.entries();
            let r_entries = r.entries();
            self.eq_entries(l_entries.as_slice(), r_entries.as_slice())
                .await
        } else {
            Ok(false)
        }
    }

    /// Compares two maps for ordering through class dispatch.
    pub(super) async fn cmp(&mut self, l: &Map, r: &Map) -> Result<Ordering> {
        let l_entries = l.entries();
        let r_entries = r.entries();
        self.cmp_entries(l_entries.as_slice(), r_entries.as_slice())
            .await
    }

    async fn cmp_value_ids(
        &mut self,
        l: ValueId,
        r: ValueId,
    ) -> Result<Ordering> {
        let method = self.ctx.interp.arena.intern("compare");
        let value = self
            .ctx
            .interp
            .dispatch_class_method_value(class::Dispatch {
                dispatch_expr_id: None,
                output_expr_id: None,
                output_ty: Some(RuntimeTyId::from(TyArena::ORDERING)),
                class: ClassId::ORD,
                method,
                args: SmallVec::from_slice(&[l, r]),
                span: self.ctx.span,
            })
            .await?;
        let ty = self
            .ctx
            .interp
            .checked
            .types
            .to_type_id(value.repr)
            .or_else(|| self.ctx.interp.checked.types.to_type_id(value.ty));

        match value.payload {
            Payload::Variant { tag: 0, .. }
                if ty.is_some_and(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Less)
            }
            Payload::Variant { tag: 1, .. }
                if ty.is_some_and(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Equal)
            }
            Payload::Variant { tag: 2, .. }
                if ty.is_some_and(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Greater)
            }
            Payload::Int(n) if n < 0 => Ok(Ordering::Less),
            Payload::Int(0) => Ok(Ordering::Equal),
            Payload::Int(_) => Ok(Ordering::Greater),
            _ => typechecked!("Ord:compare", "Ordering | Int"),
        }
    }

    async fn eq_value_ids(&mut self, l: ValueId, r: ValueId) -> Result<bool> {
        let method = self.ctx.interp.arena.intern("eq");
        let value = self
            .ctx
            .interp
            .dispatch_class_method_value(class::Dispatch {
                dispatch_expr_id: None,
                output_expr_id: None,
                output_ty: Some(RuntimeTyId::from(TyArena::BOOL)),
                class: ClassId::EQ,
                method,
                args: SmallVec::from_slice(&[l, r]),
                span: self.ctx.span,
            })
            .await?;

        match value.payload {
            Payload::Bool(b) => Ok(b),
            _ => typechecked!("Eq:eq", "Bool"),
        }
    }

    async fn eq_entries(
        &mut self,
        l: &[(ValueId, ValueId)],
        r: &[(ValueId, ValueId)],
    ) -> Result<bool> {
        let mut pairs = l.iter().copied().zip(r.iter().copied());
        let mut ok = l.len() == r.len();

        while let Some(((lk, lv), (rk, rv))) =
            if ok { pairs.next() } else { None }
        {
            let keys_eq = self.cmp_value_ids(lk, rk).await? == Ordering::Equal;
            let vals_eq = self.eq_value_ids(lv, rv).await?;
            ok = keys_eq && vals_eq;
        }

        Ok(ok)
    }

    async fn cmp_entries(
        &mut self,
        l: &[(ValueId, ValueId)],
        r: &[(ValueId, ValueId)],
    ) -> Result<Ordering> {
        let mut pairs = l.iter().copied().zip(r.iter().copied());
        let mut out = Ordering::Equal;

        while let Some(((lk, lv), (rk, rv))) = if out == Ordering::Equal {
            pairs.next()
        } else {
            None
        } {
            out = match self.cmp_value_ids(lk, rk).await? {
                Ordering::Equal => self.cmp_value_ids(lv, rv).await?,
                ord => ord,
            };
        }

        Ok(if out == Ordering::Equal {
            l.len().cmp(&r.len())
        } else {
            out
        })
    }

    async fn insert_entries(
        &mut self,
        mut acc: Map,
        entries: &[(ValueId, ValueId)],
    ) -> Result<Map> {
        let mut it = entries.iter().copied();

        while let Some((key, val)) = it.next() {
            acc = self.insert(&acc, key, val).await?;
        }

        Ok(acc)
    }

    async fn lookup_node(
        &mut self,
        node: Option<Arc<MapNode>>,
        key: ValueId,
    ) -> Result<Option<ValueId>> {
        let mut cur = node;
        let mut out = None;

        while let Some(n) = cur.take() {
            match self.cmp_value_ids(key, n.key()).await? {
                Ordering::Less => {
                    cur = n.left();
                }
                Ordering::Equal => {
                    out = Some(n.val());
                    cur = None;
                }
                Ordering::Greater => {
                    cur = n.right();
                }
            }
        }

        Ok(out)
    }

    async fn insert_node(
        &mut self,
        node: Option<Arc<MapNode>>,
        key: ValueId,
        val: ValueId,
    ) -> Result<(Option<Arc<MapNode>>, bool)> {
        struct Frame {
            dir: Ordering,
            k: ValueId,
            v: ValueId,
            sibling: Option<Arc<MapNode>>,
        }

        if let Some(root) = node {
            let mut cur = Some(root);
            let mut path = Vec::new();
            let mut done = None;

            while let Some(n) = cur.take() {
                match self.cmp_value_ids(key, n.key()).await? {
                    Ordering::Less => match n.left() {
                        Some(left) => {
                            path.push(Frame {
                                dir: Ordering::Less,
                                k: n.key(),
                                v: n.val(),
                                sibling: n.right(),
                            });
                            cur = Some(left);
                        }
                        None => {
                            let child = MapNode::new(key, val, None, None);
                            let root = MapNode::balance(
                                n.key(),
                                n.val(),
                                Some(child),
                                n.right(),
                            );
                            done = Some((Some(root), true));
                            cur = None;
                        }
                    },
                    Ordering::Equal => {
                        let root =
                            MapNode::balance(n.key(), val, n.left(), n.right());
                        done = Some((Some(root), false));
                        cur = None;
                    }
                    Ordering::Greater => match n.right() {
                        Some(right) => {
                            path.push(Frame {
                                dir: Ordering::Greater,
                                k: n.key(),
                                v: n.val(),
                                sibling: n.left(),
                            });
                            cur = Some(right);
                        }
                        None => {
                            let child = MapNode::new(key, val, None, None);
                            let root = MapNode::balance(
                                n.key(),
                                n.val(),
                                n.left(),
                                Some(child),
                            );
                            done = Some((Some(root), true));
                            cur = None;
                        }
                    },
                }
            }

            let (mut root, added) =
                done.unwrap_or_else(|| invariant!("map insert result"));
            while let Some(frame) = path.pop() {
                root = Some(
                    if frame.dir == Ordering::Less {
                        MapNode::balance(frame.k, frame.v, root, frame.sibling)
                    } else {
                        MapNode::balance(frame.k, frame.v, frame.sibling, root)
                    },
                );
            }

            Ok((root, added))
        } else {
            Ok((Some(MapNode::new(key, val, None, None)), true))
        }
    }

    async fn remove_node(
        &mut self,
        node: Option<Arc<MapNode>>,
        key: ValueId,
    ) -> Result<(Option<Arc<MapNode>>, bool)> {
        struct Frame {
            dir: Ordering,
            k: ValueId,
            v: ValueId,
            sibling: Option<Arc<MapNode>>,
        }

        if let Some(root) = node {
            let mut cur = Some(root);
            let mut path = Vec::new();
            let mut done = None;

            while let Some(n) = cur.take() {
                match self.cmp_value_ids(key, n.key()).await? {
                    Ordering::Less => match n.left() {
                        Some(left) => {
                            path.push(Frame {
                                dir: Ordering::Less,
                                k: n.key(),
                                v: n.val(),
                                sibling: n.right(),
                            });
                            cur = Some(left);
                        }
                        None => {
                            done = Some((Some(n), false));
                            cur = None;
                        }
                    },
                    Ordering::Equal => {
                        let root = match (n.left(), n.right()) {
                            (None, None) => None,
                            (Some(left), None) => Some(left),
                            (None, Some(right)) => Some(right),
                            (Some(left), Some(right)) => {
                                struct MinFrame {
                                    k: ValueId,
                                    v: ValueId,
                                    right: Option<Arc<MapNode>>,
                                }

                                let mut cur = Some(right);
                                let mut min_path = Vec::new();
                                let mut min = None;

                                while let Some(m) = cur.take() {
                                    match m.left() {
                                        Some(next) => {
                                            min_path.push(MinFrame {
                                                k: m.key(),
                                                v: m.val(),
                                                right: m.right(),
                                            });
                                            cur = Some(next);
                                        }
                                        None => {
                                            min = Some((
                                                m.right(),
                                                m.key(),
                                                m.val(),
                                            ));
                                            cur = None;
                                        }
                                    }
                                }

                                let (mut right, key, val) =
                                    min.unwrap_or_else(|| {
                                        invariant!("map remove min result")
                                    });
                                while let Some(frame) = min_path.pop() {
                                    right = Some(MapNode::balance(
                                        frame.k,
                                        frame.v,
                                        right,
                                        frame.right,
                                    ));
                                }

                                Some(MapNode::balance(
                                    key,
                                    val,
                                    Some(left),
                                    right,
                                ))
                            }
                        };
                        done = Some((root, true));
                        cur = None;
                    }
                    Ordering::Greater => match n.right() {
                        Some(right) => {
                            path.push(Frame {
                                dir: Ordering::Greater,
                                k: n.key(),
                                v: n.val(),
                                sibling: n.left(),
                            });
                            cur = Some(right);
                        }
                        None => {
                            done = Some((Some(n), false));
                            cur = None;
                        }
                    },
                }
            }

            let (mut root, removed) =
                done.unwrap_or_else(|| invariant!("map remove result"));
            while let Some(frame) = path.pop() {
                root = Some(
                    if frame.dir == Ordering::Less {
                        MapNode::balance(frame.k, frame.v, root, frame.sibling)
                    } else {
                        MapNode::balance(frame.k, frame.v, frame.sibling, root)
                    },
                );
            }

            Ok((root, removed))
        } else {
            Ok((None, false))
        }
    }
}
