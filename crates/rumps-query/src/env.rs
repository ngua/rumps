//! Variable environments for lexical scope and primitive functions.
//!
//! The `Environment` tracks lexical scope for `LET` bindings and callable names.
//! `SET` variables (both local and global) go through the `Database`, not here.

#![allow(dead_code)]

use std::collections::HashMap;

use rumps_query_macros::scheme;

use crate::typecheck::{Scheme, Ty};

/// Names of built-in modules.
///
/// This is the single source of truth for which module names are recognized
/// during resolution and registered at interpreter startup.
pub(crate) const BUILTIN_MODULE_NAMES: &[&str] = &[
    "Array", "String", "Math", "Random", "Map", "Time", "Option", "Result",
    "Io",
];

use futures::future::BoxFuture;
use smallvec::{smallvec, SmallVec};

use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{TypeId, Value, ValueArena, ValueId};
use crate::{Error, Result, Span};

/// Stack of lexical scopes for `LET` bindings.
///
/// Each frame is a `HashMap` mapping variable names to values.
/// The stack grows downward; the top (last) frame is the current scope.
pub(crate) struct Scopes {
    stack: Vec<HashMap<StringId, ValueId>>,
}

impl Default for Scopes {
    fn default() -> Self {
        Self::new()
    }
}

impl Scopes {
    /// Create a new scope stack with a single empty frame.
    pub(crate) fn new() -> Self {
        Self {
            stack: vec![HashMap::new()],
        }
    }

    /// Bind a name to a value in the current (top) frame.
    pub(crate) fn bind(&mut self, name: StringId, val: ValueId) {
        // Note that there's always a root stack frame (i.e. stack is never
        // empty), we could `expect` or `unwrap` here, but `map` is fine IMO
        self.stack.last_mut().map(|frame| frame.insert(name, val));
    }

    /// Look up a name, searching from the top frame down.
    ///
    /// Returns the first binding found, or `None` if not bound.
    pub(crate) fn lookup(&self, name: StringId) -> Option<ValueId> {
        self.stack
            .iter()
            .rev()
            .find_map(|frame| frame.get(&name).copied())
    }

    /// Push a new empty frame onto the stack (enter nested scope).
    pub(crate) fn push(&mut self) {
        self.stack.push(HashMap::new());
    }

    /// Pop the top frame from the stack (exit scope).
    ///
    /// Returns the popped frame, or `None` if only one frame remains
    /// (we never pop the root frame).
    pub(crate) fn pop(&mut self) -> Option<HashMap<StringId, ValueId>> {
        (self.stack.len() > 1).then(|| self.stack.pop()).flatten()
    }

    /// Current depth of the scope stack.
    fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Get a reference to the scope stack (for closure capture).
    pub(crate) fn stack(&self) -> &[HashMap<StringId, ValueId>] {
        &self.stack
    }

    /// Save the current scope stack (for closure calls).
    ///
    /// Returns the saved stack, leaving the current stack with a single empty frame.
    pub(crate) fn save(&mut self) -> Vec<HashMap<StringId, ValueId>> {
        std::mem::replace(&mut self.stack, vec![HashMap::new()])
    }

    /// Restore a previously saved scope stack.
    pub(crate) fn restore(&mut self, saved: Vec<HashMap<StringId, ValueId>>) {
        self.stack = saved;
    }

    /// Restore scope from a captured environment (for closure calls).
    ///
    /// Creates a fresh scope stack with the captured bindings.
    pub(crate) fn restore_from_captured(
        &mut self,
        env: &crate::value::CapturedEnv,
    ) {
        let frame = env.bindings().iter().map(|(k, v)| (*k, *v)).collect();
        self.stack = vec![frame];
    }
}

/// Result type for primitive function execution.
pub(crate) type PrimResult<'a> = BoxFuture<'a, Result<ValueId>>;

/// Context passed to primitive functions during execution.
///
/// Contains references to the value arena for creating/looking up values,
/// the type expression arena for constructing type annotations, the I/O
/// context for output operations, and the call-site span for error reporting.
pub(crate) struct PrimCtx<'a> {
    pub(crate) arena: &'a mut ValueArena,
    pub(crate) type_exprs: &'a mut crate::value::TypeExprArena,
    // Required for `Io.* operations to work correctly, i.e. use the I/O`
    // abstraction used elsewhere
    pub(crate) io: &'a mut dyn IoContext,
    pub(crate) span: Span,
}

impl PrimCtx<'_> {
    /// Create a `Result.Ok(v)` value from a `ValueId` already in the arena.
    pub(crate) fn result_ok(&mut self, v: ValueId) -> ValueId {
        let base_ty = self
            .arena
            .base_type_of(v, self.type_exprs)
            .unwrap_or(TypeId::INT);
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let val_ty = self.type_exprs.named(base_ty);
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![val_ty, unknown]);
        let ok = Value::ok(res_ty, v);
        self.arena.add(ok, self.span)
    }

    /// Create a `Result.Err(msg)` value from a `ValueId` already in the arena.
    pub(crate) fn result_err(&mut self, msg: ValueId) -> ValueId {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let str_ty = self.type_exprs.named(TypeId::STRING);
        let res_ty = self
            .type_exprs
            .app(TypeId::RESULT, smallvec![unknown, str_ty]);
        let err = Value::err(res_ty, msg);
        self.arena.add(err, self.span)
    }

    /// Create an `Option.Some(v)` value from a `ValueId` already in the arena.
    pub(crate) fn option_some(&mut self, v: ValueId) -> ValueId {
        let base_ty = self
            .arena
            .base_type_of(v, self.type_exprs)
            .unwrap_or(TypeId::UNKNOWN);
        let val_ty = self.type_exprs.named(base_ty);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![val_ty]);
        let some = Value::some(opt_ty, v);
        self.arena.add(some, self.span)
    }

    /// Create an `Option.None` value.
    pub(crate) fn option_none(&mut self) -> ValueId {
        let unknown = self.type_exprs.named(TypeId::UNKNOWN);
        let opt_ty = self.type_exprs.app(TypeId::OPTION, smallvec![unknown]);
        let none = Value::none(opt_ty);
        self.arena.add(none, self.span)
    }

    /// Create a runtime error with span information.
    pub(crate) fn runtime_error(&self, msg: impl Into<String>) -> Error {
        Error::runtime(self.span, msg)
    }
}

/// A built-in primitive function.
///
/// Primitives are callable built-in functions like `Array.map`, `String.split`,
/// etc. They take a context and arguments, returning a future that resolves
/// to a `ValueId`.
///
/// Note: `GET`/`SET`/`KILL` are keywords with special syntax, so they are
/// AST constructs (`Expr::Get`, `Stmt::Set`, `Stmt::Kill`), not primitives.
pub(crate) type PrimFn =
    for<'a> fn(&'a mut PrimCtx<'a>, SmallVec<[ValueId; 4]>) -> PrimResult<'a>;

/// A primitive function definition with its type scheme.
///
/// Colocates the runtime function with its static type, ensuring every
/// registered primitive has a corresponding type for the type checker.
pub(crate) struct PrimDef {
    pub(crate) name: &'static str,
    pub(crate) f: PrimFn,
    pub(crate) ty: Scheme,
}

/// A built-in module containing primitive functions, constants, and submodules.
///
/// Modules group related functions under a namespace (e.g., `Array.map`,
/// `String.split`). Supports nested modules for future extensibility
/// (e.g., `Math.Trig.sin`).
///
/// Constants are module-level values (e.g., `Math.pi`, `Math.e`) that are
/// evaluated to their stored `ValueId` when accessed.
///
/// Built-in modules are registered at interpreter startup; user-defined
/// modules will be supported in a future phase.
#[derive(Default)]
pub(crate) struct Module {
    /// Functions in this module: `(primitive_fn, type_scheme)`.
    functions: HashMap<String, (PrimFn, Scheme)>,

    /// Constants in this module: `(value_id, type)`.
    /// `ValueId`s index into `Environment::consts`.
    constants: HashMap<String, (ValueId, Ty)>,

    /// Submodules, keyed by submodule name.
    submodules: HashMap<String, Self>,
}

impl Module {
    /// Create a module from primitive definitions.
    pub(crate) fn from_prims(prims: &[PrimDef]) -> Self {
        let functions = prims
            .iter()
            .map(|p| (p.name.to_string(), (p.f, p.ty.clone())))
            .collect();
        Self {
            functions,
            constants: HashMap::new(),
            submodules: HashMap::new(),
        }
    }

    /// Builder method to add a submodule.
    pub(crate) fn with_submodule(mut self, name: &str, m: Self) -> Self {
        self.submodules.insert(name.to_string(), m);
        self
    }

    /// Builder method to add a constant with its type.
    pub(crate) fn with_const(
        mut self,
        name: &str,
        id: ValueId,
        ty: Ty,
    ) -> Self {
        self.constants.insert(name.to_string(), (id, ty));
        self
    }

    /// Mutably add a constant with its type.
    pub(crate) fn add_const(&mut self, name: &str, id: ValueId, ty: Ty) {
        self.constants.insert(name.to_string(), (id, ty));
    }

    /// Look up a function by path within this module.
    ///
    /// For a single-segment path, looks up the function directly.
    /// For multi-segment paths, traverses submodules.
    pub(crate) fn get_fn(&self, path: &[&str]) -> Option<&PrimFn> {
        match path {
            [] => None,
            [name] => self.functions.get(*name).map(|(f, _)| f),
            [first, rest @ ..] => {
                self.submodules.get(*first).and_then(|m| m.get_fn(rest))
            }
        }
    }

    /// Look up a function's type scheme by path within this module.
    pub(crate) fn get_fn_type(&self, path: &[&str]) -> Option<&Scheme> {
        match path {
            [] => None,
            [name] => self.functions.get(*name).map(|(_, ty)| ty),
            [first, rest @ ..] => self
                .submodules
                .get(*first)
                .and_then(|m| m.get_fn_type(rest)),
        }
    }

    /// Look up a constant by path within this module.
    pub(crate) fn get_const(&self, path: &[&str]) -> Option<ValueId> {
        match path {
            [] => None,
            [name] => self.constants.get(*name).map(|(id, _)| *id),
            [first, rest @ ..] => {
                self.submodules.get(*first).and_then(|m| m.get_const(rest))
            }
        }
    }

    /// Look up a constant's type by path within this module.
    pub(crate) fn get_const_type(&self, path: &[&str]) -> Option<&Ty> {
        match path {
            [] => None,
            [name] => self.constants.get(*name).map(|(_, ty)| ty),
            [first, rest @ ..] => self
                .submodules
                .get(*first)
                .and_then(|m| m.get_const_type(rest)),
        }
    }

    /// Check if a path resolves to a function within this module.
    pub(crate) fn contains_fn(&self, path: &[&str]) -> bool {
        self.get_fn(path).is_some()
    }

    /// Check if a path resolves to a constant within this module.
    pub(crate) fn contains_const(&self, path: &[&str]) -> bool {
        self.get_const(path).is_some()
    }
}

/// A user-defined module containing functions and constants.
///
/// Unlike builtin `Module`s which use `PrimFn`, user modules store:
/// - Functions as `ValueId`s pointing to closure values
/// - Constants as `ValueId`s pointing to evaluated values
#[derive(Default, Clone)]
pub(crate) struct UserModule {
    /// Functions in this module, keyed by function name.
    /// `ValueId`s point to closure values in a `ValueArena`.
    pub(crate) functions: HashMap<String, ValueId>,

    /// Constants in this module, keyed by constant name.
    pub(crate) constants: HashMap<String, ValueId>,

    /// Submodules, keyed by submodule name.
    pub(crate) submodules: HashMap<String, Self>,
}

impl UserModule {
    /// Look up a function by path within this module.
    pub(crate) fn get_fn(&self, path: &[&str]) -> Option<ValueId> {
        match path {
            [] => None,
            [name] => self.functions.get(*name).copied(),
            [first, rest @ ..] => {
                self.submodules.get(*first).and_then(|m| m.get_fn(rest))
            }
        }
    }

    /// Look up a constant by path within this module.
    pub(crate) fn get_const(&self, path: &[&str]) -> Option<ValueId> {
        match path {
            [] => None,
            [name] => self.constants.get(*name).copied(),
            [first, rest @ ..] => {
                self.submodules.get(*first).and_then(|m| m.get_const(rest))
            }
        }
    }

    /// Check if a path resolves to a function within this module.
    pub(crate) fn contains_fn(&self, path: &[&str]) -> bool {
        self.get_fn(path).is_some()
    }

    /// Check if a path resolves to a constant within this module.
    pub(crate) fn contains_const(&self, path: &[&str]) -> bool {
        self.get_const(path).is_some()
    }
}

/// Variable environment for the interpreter.
///
/// Tracks:
/// - Lexical scopes for `LET` bindings (via `Scopes`)
/// - Built-in modules containing primitive functions (e.g., `Array`, `String`)
/// - User-defined modules containing closures and constants
/// - Module constants (e.g., `Math.pi`, `Math.e`)
///
/// Note: `SET` variables (both local and global) are stored in the `Database`,
/// not in the environment. Only `LET` bindings live here. You can `GET` a `SET`
/// (local or global), but not a `LET`; `LET`s can be referenced by name directly.
pub(crate) struct Environment {
    /// Lexical scope stack for `LET` bindings.
    pub(crate) scopes: Scopes,

    /// Built-in modules (e.g., `Object`, `Array`).
    modules: HashMap<String, Module>,

    /// User-defined modules.
    user_modules: HashMap<String, UserModule>,

    /// Arena backing module constant `ValueId`s.
    pub(crate) consts: ValueArena,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    /// Create a new environment with built-in modules registered.
    pub(crate) fn new() -> Self {
        let mut env = Self {
            scopes: Scopes::new(),
            modules: HashMap::new(),
            user_modules: HashMap::new(),
            consts: ValueArena::new(),
        };
        env.register_builtins();
        env
    }

    /// Check if a top-level module exists (builtin or user-defined).
    pub(crate) fn has_module(&self, name: &str) -> bool {
        self.modules.contains_key(name) || self.user_modules.contains_key(name)
    }

    /// Register a user-defined module.
    pub(crate) fn register_user_module(
        &mut self,
        name: &str,
        module: UserModule,
    ) {
        self.user_modules.insert(name.to_string(), module);
    }

    /// Get a mutable reference to a user module, creating it if it doesn't exist.
    pub(crate) fn get_or_create_user_module(
        &mut self,
        name: &str,
    ) -> &mut UserModule {
        self.user_modules.entry(name.to_string()).or_default()
    }

    /// Look up a user module function by path.
    pub(crate) fn get_user_module_fn(&self, path: &[&str]) -> Option<ValueId> {
        path.split_first().and_then(|(module, rest)| {
            self.user_modules.get(*module).and_then(|m| m.get_fn(rest))
        })
    }

    /// Look up a user module constant by path.
    pub(crate) fn get_user_module_const(
        &self,
        path: &[&str],
    ) -> Option<ValueId> {
        path.split_first().and_then(|(module, rest)| {
            self.user_modules
                .get(*module)
                .and_then(|m| m.get_const(rest))
        })
    }

    /// Check if a path resolves to a user module function.
    pub(crate) fn user_module_fn_exists(&self, path: &[&str]) -> bool {
        self.get_user_module_fn(path).is_some()
    }

    /// Check if a path resolves to a user module constant.
    pub(crate) fn user_module_const_exists(&self, path: &[&str]) -> bool {
        self.get_user_module_const(path).is_some()
    }

    /// Check if a path resolves to a module function.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    pub(crate) fn module_fn_exists(&self, path: &[&str]) -> bool {
        path.split_first().is_some_and(|(module, rest)| {
            self.modules
                .get(*module)
                .is_some_and(|m| m.contains_fn(rest))
        })
    }

    /// Check if a path resolves to a module constant.
    pub(crate) fn module_const_exists(&self, path: &[&str]) -> bool {
        path.split_first().is_some_and(|(module, rest)| {
            self.modules
                .get(*module)
                .is_some_and(|m| m.contains_const(rest))
        })
    }

    /// Look up a function by its full path.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    ///
    /// Examples:
    /// - `["Array", "length"]` → `Array.length`
    /// - `["Math", "Trig", "sin"]` → `Math.Trig.sin`
    pub(crate) fn get_module_fn(&self, path: &[&str]) -> Option<&PrimFn> {
        path.split_first().and_then(|(module, rest)| {
            self.modules.get(*module).and_then(|m| m.get_fn(rest))
        })
    }

    /// Look up a function's type scheme by its full path.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    pub(crate) fn get_module_fn_type(&self, path: &[&str]) -> Option<&Scheme> {
        path.split_first().and_then(|(module, rest)| {
            self.modules.get(*module).and_then(|m| m.get_fn_type(rest))
        })
    }

    /// Look up a constant by its full path.
    ///
    /// Returns the `ValueId` indexing into `self.consts`.
    pub(crate) fn get_module_const(&self, path: &[&str]) -> Option<ValueId> {
        path.split_first().and_then(|(module, rest)| {
            self.modules.get(*module).and_then(|m| m.get_const(rest))
        })
    }

    /// Look up a constant's type by its full path.
    pub(crate) fn get_module_const_type(&self, path: &[&str]) -> Option<&Ty> {
        path.split_first().and_then(|(module, rest)| {
            self.modules
                .get(*module)
                .and_then(|m| m.get_const_type(rest))
        })
    }

    /// Register built-in modules.
    ///
    /// Built-in modules provide primitive functions grouped by category:
    /// - `Array`: `length`, `push`, `pop`, `head`, `tail`, `reverse`, `sort`,
    ///   `slice`, `contains`, `concat`, plus HoF placeholders
    /// - `String`: `length`, `upper`, `lower`, `trim`, `split`, `join`,
    ///   `slice`, `contains`, `replace`
    /// - `Math`: `abs`, `min`, `max`, `floor`, `ceil`, `round`, `sqrt`, `log`,
    ///   `sin`, `cos`
    /// - `Random`: `random`, `range`, `int`, `bool`, `choice`, `shuffle`,
    ///   `sample`, `uuid`
    ///
    /// Note: Array higher-order functions (`map`, `filter`, `reduce`, `foreach`)
    /// are handled specially by the interpreter. We register placeholders here
    /// so that `module_fn_exists` returns true for name resolution.
    fn register_builtins(&mut self) {
        use crate::primitives::{
            Array, Io, Map, Math, Opt, Prim, Random, Res, Str, Time, Trig,
        };

        self.modules.insert(
            "Array".to_string(),
            Module::from_prims(&[
                // Higher-order functions (Range coerces to Array[Int] in unify)
                PrimDef {
                    name: "map",
                    f: Array::placeholder,
                    ty: scheme!(forall T U. ((T) -> U, Array[T]) -> Array[U]),
                },
                PrimDef {
                    name: "filter",
                    f: Array::placeholder,
                    ty: scheme!(forall T. ((T) -> Bool, Array[T]) -> Array[T]),
                },
                PrimDef {
                    name: "reduce",
                    f: Array::placeholder,
                    ty: scheme!(forall T U. ((U, T) -> U, U, Array[T]) -> U),
                },
                PrimDef {
                    name: "foreach",
                    f: Array::placeholder,
                    ty: scheme!(forall T. ((T) -> Unit, Array[T]) -> Unit),
                },
                // Regular primitives
                PrimDef {
                    name: "length",
                    f: Array::length,
                    ty: scheme!(forall T. (Array[T]) -> Int),
                },
                PrimDef {
                    name: "push",
                    f: Array::push,
                    ty: scheme!(forall T. (Array[T], T) -> Array[T]),
                },
                PrimDef {
                    name: "pop",
                    f: Array::pop,
                    ty: scheme!(forall T. (Array[T]) -> Array[T]),
                },
                PrimDef {
                    name: "head",
                    f: Array::head,
                    ty: scheme!(forall T. (Array[T]) -> Option[T]),
                },
                PrimDef {
                    name: "tail",
                    f: Array::tail,
                    ty: scheme!(forall T. (Array[T]) -> Array[T]),
                },
                PrimDef {
                    name: "reverse",
                    f: Array::reverse,
                    ty: scheme!(forall T. (Array[T]) -> Array[T]),
                },
                PrimDef {
                    name: "sort",
                    f: Array::sort,
                    ty: scheme!(forall T. (Array[T]) -> Array[T]),
                },
                PrimDef {
                    name: "slice",
                    f: Array::slice,
                    ty: scheme!(forall T. (Array[T], Int, Int) -> Array[T]),
                },
                PrimDef {
                    name: "contains",
                    f: Array::contains,
                    ty: scheme!(forall T. (Array[T], T) -> Bool),
                },
                PrimDef {
                    name: "concat",
                    f: Array::concat,
                    ty: scheme!(forall T. (Array[T], Array[T]) -> Array[T]),
                },
                // New array utilities
                PrimDef {
                    name: "sort-by",
                    f: Array::placeholder,
                    ty: scheme!(forall T. ((T, T) -> Ordering, Array[T]) -> Array[T]),
                },
                PrimDef {
                    name: "zip",
                    f: Array::zip,
                    ty: scheme!(forall T U. (Array[T], Array[U]) -> Array[(T, U)]),
                },
                PrimDef {
                    name: "zip-with",
                    f: Array::placeholder,
                    ty: scheme!(forall T U V. ((T, U) -> V, Array[T], Array[U]) -> Array[V]),
                },
                PrimDef {
                    name: "unzip",
                    f: Array::unzip,
                    ty: scheme!(forall T U. (Array[(T, U)]) -> (Array[T], Array[U])),
                },
                PrimDef {
                    name: "intersperse",
                    f: Array::intersperse,
                    ty: scheme!(forall T. (T, Array[T]) -> Array[T]),
                },
            ]),
        );

        self.modules.insert(
            "String".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "length",
                    f: Str::length,
                    ty: scheme!((String) -> Int),
                },
                PrimDef {
                    name: "upper",
                    f: Str::upper,
                    ty: scheme!((String) -> String),
                },
                PrimDef {
                    name: "lower",
                    f: Str::lower,
                    ty: scheme!((String) -> String),
                },
                PrimDef {
                    name: "trim",
                    f: Str::trim,
                    ty: scheme!((String) -> String),
                },
                PrimDef {
                    name: "split",
                    f: Str::split,
                    ty: scheme!((String, String) -> Array[String]),
                },
                PrimDef {
                    name: "join",
                    f: Str::join,
                    ty: scheme!((Array[String], String) -> String),
                },
                PrimDef {
                    name: "slice",
                    f: Str::slice,
                    ty: scheme!((String, Int, Int) -> String),
                },
                PrimDef {
                    name: "contains",
                    f: Str::contains,
                    ty: scheme!((String, String) -> Bool),
                },
                PrimDef {
                    name: "replace",
                    f: Str::replace,
                    ty: scheme!((String, String, String) -> String),
                },
            ]),
        );

        // Math module
        let mut math_module = Module::from_prims(&[
            PrimDef {
                name: "abs",
                f: Math::abs,
                ty: scheme!((Float) -> Float),
            },
            PrimDef {
                name: "min",
                f: Math::min,
                ty: scheme!((Float, Float) -> Float),
            },
            PrimDef {
                name: "max",
                f: Math::max,
                ty: scheme!((Float, Float) -> Float),
            },
            PrimDef {
                name: "floor",
                f: Math::floor,
                ty: scheme!((Float) -> Int),
            },
            PrimDef {
                name: "ceil",
                f: Math::ceil,
                ty: scheme!((Float) -> Int),
            },
            PrimDef {
                name: "round",
                f: Math::round,
                ty: scheme!((Float) -> Int),
            },
            PrimDef {
                name: "sqrt",
                f: Math::sqrt,
                ty: scheme!((Float) -> Float),
            },
            PrimDef {
                name: "log",
                f: Math::log,
                ty: scheme!((Float) -> Float),
            },
        ]);

        // Math constants (intern into `consts` arena)
        use ordered_float::OrderedFloat;
        [
            ("pi", std::f64::consts::PI),
            ("e", std::f64::consts::E),
            ("tau", std::f64::consts::TAU),
            ("inf", f64::INFINITY),
            ("neg-inf", f64::NEG_INFINITY),
        ]
        .iter()
        .for_each(|&(name, val)| {
            let id = self
                .consts
                .add(Value::Float(OrderedFloat(val)), Span::MODULE_CONST);
            math_module.add_const(name, id, Ty::Float);
        });

        self.modules.insert(
            "Math".to_string(),
            math_module.with_submodule(
                "Trig",
                Module::from_prims(&[
                    PrimDef {
                        name: "sin",
                        f: Trig::sin,
                        ty: scheme!((Float) -> Float),
                    },
                    PrimDef {
                        name: "cos",
                        f: Trig::cos,
                        ty: scheme!((Float) -> Float),
                    },
                    PrimDef {
                        name: "tan",
                        f: Trig::tan,
                        ty: scheme!((Float) -> Float),
                    },
                    PrimDef {
                        name: "asin",
                        f: Trig::asin,
                        ty: scheme!((Float) -> Float),
                    },
                    PrimDef {
                        name: "acos",
                        f: Trig::acos,
                        ty: scheme!((Float) -> Float),
                    },
                    PrimDef {
                        name: "atan",
                        f: Trig::atan,
                        ty: scheme!((Float) -> Float),
                    },
                    PrimDef {
                        name: "atan2",
                        f: Trig::atan2,
                        ty: scheme!((Float, Float) -> Float),
                    },
                ]),
            ),
        );

        self.modules.insert(
            "Random".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "random",
                    f: Random::random,
                    ty: scheme!(() -> Float),
                },
                PrimDef {
                    name: "range",
                    f: Random::range,
                    ty: scheme!((Float, Float) -> Float),
                },
                PrimDef {
                    name: "int",
                    f: Random::int,
                    ty: scheme!((Int, Int) -> Int),
                },
                PrimDef {
                    name: "bool",
                    f: Random::bool,
                    ty: scheme!(() -> Bool),
                },
                PrimDef {
                    name: "choice",
                    f: Random::choice,
                    ty: scheme!(forall T. (Array[T]) -> Option[T]),
                },
                PrimDef {
                    name: "shuffle",
                    f: Random::shuffle,
                    ty: scheme!(forall T. (Array[T]) -> Array[T]),
                },
                PrimDef {
                    name: "sample",
                    f: Random::sample,
                    ty: scheme!(forall T. (Array[T], Int) -> Result[Array[T], String]),
                },
                PrimDef {
                    name: "uuid",
                    f: Random::uuid,
                    ty: scheme!(() -> String),
                },
            ]),
        );

        self.modules.insert(
            "Map".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "empty",
                    f: Map::empty,
                    ty: scheme!(forall K V. () -> Map[K, V]),
                },
                PrimDef {
                    name: "length",
                    f: Map::length,
                    ty: scheme!(forall K V. (Map[K, V]) -> Int),
                },
                PrimDef {
                    name: "keys",
                    f: Map::keys,
                    ty: scheme!(forall K V. (Map[K, V]) -> Array[K]),
                },
                PrimDef {
                    name: "values",
                    f: Map::values,
                    ty: scheme!(forall K V. (Map[K, V]) -> Array[V]),
                },
                PrimDef {
                    name: "entries",
                    f: Map::entries,
                    ty: scheme!(forall K V. (Map[K, V]) -> Array[(K, V)]),
                },
                PrimDef {
                    name: "has",
                    f: Map::has,
                    ty: scheme!(forall K V. (Map[K, V], K) -> Bool),
                },
                PrimDef {
                    name: "lookup",
                    f: Map::get,
                    ty: scheme!(forall K V. (Map[K, V], K) -> Option[V]),
                },
                PrimDef {
                    name: "insert",
                    f: Map::set,
                    ty: scheme!(forall K V. (Map[K, V], K, V) -> Map[K, V]),
                },
                PrimDef {
                    name: "remove",
                    f: Map::remove,
                    ty: scheme!(forall K V. (Map[K, V], K) -> Map[K, V]),
                },
                PrimDef {
                    name: "merge",
                    f: Map::merge,
                    ty: scheme!(forall K V. (Map[K, V], Map[K, V]) -> Map[K, V]),
                },
                PrimDef {
                    name: "from-entries",
                    f: Map::from_entries,
                    ty: scheme!(forall K V. (Array[(K, V)]) -> Map[K, V]),
                },
            ]),
        );

        self.modules.insert(
            "Time".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "now",
                    f: Time::now,
                    ty: scheme!(() -> Time),
                },
                PrimDef {
                    name: "epoch",
                    f: Time::epoch,
                    ty: scheme!(() -> Time),
                },
                PrimDef {
                    name: "parse",
                    f: Time::parse,
                    ty: scheme!((String, String) -> Result[Time, String]),
                },
                PrimDef {
                    name: "format",
                    f: Time::format,
                    ty: scheme!((String, Time) -> String),
                },
                PrimDef {
                    name: "add-seconds",
                    f: Time::add_seconds,
                    ty: scheme!((Time, Int) -> Time),
                },
                PrimDef {
                    name: "diff-seconds",
                    f: Time::diff_seconds,
                    ty: scheme!((Time, Time) -> Float),
                },
                PrimDef {
                    name: "year",
                    f: Time::year,
                    ty: scheme!((Time) -> Int),
                },
                PrimDef {
                    name: "month",
                    f: Time::month,
                    ty: scheme!((Time) -> Int),
                },
                PrimDef {
                    name: "day",
                    f: Time::day,
                    ty: scheme!((Time) -> Int),
                },
                PrimDef {
                    name: "hour",
                    f: Time::hour,
                    ty: scheme!((Time) -> Int),
                },
                PrimDef {
                    name: "minute",
                    f: Time::minute,
                    ty: scheme!((Time) -> Int),
                },
                PrimDef {
                    name: "second",
                    f: Time::second,
                    ty: scheme!((Time) -> Int),
                },
                PrimDef {
                    name: "sleep",
                    f: Time::sleep,
                    ty: scheme!((Int) -> Unit),
                },
            ]),
        );

        self.modules.insert(
            "Option".to_string(),
            Module::from_prims(&[
                // Higher-order function placeholders
                PrimDef {
                    name: "map",
                    f: Opt::placeholder,
                    ty: scheme!(forall T U. (Option[T], (T) -> U) -> Option[U]),
                },
                PrimDef {
                    name: "flat-map",
                    f: Opt::placeholder,
                    ty: scheme!(forall T U. (Option[T], (T) -> Option[U]) -> Option[U]),
                },
                // Regular primitives
                PrimDef {
                    name: "unwrap-or",
                    f: Opt::unwrap_or,
                    ty: scheme!(forall T. (Option[T], T) -> T),
                },
            ]),
        );

        self.modules.insert(
            "Result".to_string(),
            Module::from_prims(&[
                // Higher-order function placeholders
                PrimDef {
                    name: "map",
                    f: Res::placeholder,
                    ty: scheme!(forall T U E. (Result[T, E], (T) -> U) -> Result[U, E]),
                },
                PrimDef {
                    name: "map-err",
                    f: Res::placeholder,
                    ty: scheme!(forall T E F. (Result[T, E], (E) -> F) -> Result[T, F]),
                },
                PrimDef {
                    name: "flat-map",
                    f: Res::placeholder,
                    ty: scheme!(forall T U E. (Result[T, E], (T) -> Result[U, E]) -> Result[U, E]),
                },
                // Regular primitives
                PrimDef {
                    name: "unwrap-or",
                    f: Res::unwrap_or,
                    ty: scheme!(forall T E. (Result[T, E], T) -> T),
                },
            ]),
        );

        // Build Directory submodule first to avoid double mutable borrow
        let directory_module = self.build_directory_module();

        self.modules.insert(
            "Io".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "get-line",
                    f: Io::get_line,
                    ty: scheme!(() -> String),
                },
                PrimDef {
                    name: "print",
                    f: Io::print,
                    ty: scheme!((String) -> Unit),
                },
                PrimDef {
                    name: "println",
                    f: Io::println,
                    ty: scheme!((String) -> Unit),
                },
                PrimDef {
                    name: "eprint",
                    f: Io::eprint,
                    ty: scheme!((String) -> Unit),
                },
                PrimDef {
                    name: "eprintln",
                    f: Io::eprintln,
                    ty: scheme!((String) -> Unit),
                },
            ])
            .with_submodule("Directory", directory_module),
        );
    }

    /// Build the `Io.Directory` submodule.
    fn build_directory_module(&mut self) -> Module {
        use crate::primitives::Directory;

        // Alias for macro context
        let consts = &mut self.consts;

        Module::from_prims(&[
            PrimDef {
                name: "list-dir",
                f: Directory::list_dir,
                ty: scheme!((FilePath) -> Array[Path]),
            },
            PrimDef {
                name: "exists",
                f: Directory::exists,
                ty: scheme!((FilePath) -> Bool),
            },
            PrimDef {
                name: "is-file",
                f: Directory::is_file,
                ty: scheme!((FilePath) -> Bool),
            },
            PrimDef {
                name: "is-dir",
                f: Directory::is_dir,
                ty: scheme!((FilePath) -> Bool),
            },
            PrimDef {
                name: "read-file",
                f: Directory::read_file,
                ty: scheme!((FilePath) -> String),
            },
            PrimDef {
                name: "remove",
                f: Directory::remove,
                ty: scheme!((FilePath) -> Unit),
            },
            PrimDef {
                name: "remove-all",
                f: Directory::remove_all,
                ty: scheme!((FilePath) -> Unit),
            },
            PrimDef {
                name: "create-dir",
                f: Directory::create_dir,
                ty: scheme!((FilePath) -> Unit),
            },
            PrimDef {
                name: "create-dir-all",
                f: Directory::create_dir_all,
                ty: scheme!((FilePath) -> Unit),
            },
            PrimDef {
                name: "pwd",
                f: Directory::pwd,
                ty: scheme!(() -> FilePath),
            },
            PrimDef {
                name: "set-pwd",
                f: Directory::set_pwd,
                ty: scheme!((FilePath) -> Unit),
            },
            PrimDef {
                name: "get-env",
                f: Directory::get_env,
                ty: scheme!((String) -> Option[String]),
            },
            PrimDef {
                name: "move-path",
                f: Directory::move_path,
                ty: scheme!(consts; ({ src: FilePath, dest: FilePath }) -> Unit),
            },
            PrimDef {
                name: "copy-path",
                f: Directory::copy_path,
                ty: scheme!(consts; ({ src: FilePath, dest: FilePath }) -> Unit),
            },
            PrimDef {
                name: "write-file",
                f: Directory::write_file,
                ty: scheme!(consts; ({ path: FilePath, contents: String }) -> Unit),
            },
            PrimDef {
                name: "append-file",
                f: Directory::append_file,
                ty: scheme!(consts; ({ path: FilePath, contents: String }) -> Unit),
            },
            PrimDef {
                name: "set-env",
                f: Directory::set_env,
                ty: scheme!(consts; ({ name: String, value: String }) -> Unit),
            },
            PrimDef {
                name: "canonicalize",
                f: Directory::canonicalize,
                ty: scheme!((FilePath) -> FilePath),
            },
            PrimDef {
                name: "parent",
                f: Directory::parent,
                ty: scheme!((FilePath) -> Option[FilePath]),
            },
            PrimDef {
                name: "file-name",
                f: Directory::file_name,
                ty: scheme!((FilePath) -> Option[String]),
            },
            PrimDef {
                name: "extension",
                f: Directory::extension,
                ty: scheme!((FilePath) -> Option[String]),
            },
            PrimDef {
                name: "join",
                f: Directory::join,
                ty: scheme!((FilePath, Array[String]) -> FilePath),
            },
            PrimDef {
                name: "temp-dir",
                f: Directory::temp_dir,
                ty: scheme!(() -> FilePath),
            },
            PrimDef {
                name: "with-extension",
                f: Directory::with_extension,
                ty: scheme!((FilePath, String) -> FilePath),
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Span, Value};

    fn setup() -> (ValueArena, Scopes) {
        (ValueArena::new(), Scopes::new())
    }

    #[test]
    fn scope_bind_and_lookup() {
        let (mut arena, mut scopes) = setup();

        let name = arena.intern("x");
        let val = arena.add(Value::Int(42), Span::new(0, 2));

        scopes.bind(name, val);

        assert_eq!(scopes.lookup(name), Some(val));
    }

    #[test]
    fn scope_lookup_not_found() {
        let (mut arena, scopes) = setup();

        let name = arena.intern("undefined");
        assert_eq!(scopes.lookup(name), None);
    }

    #[test]
    fn scope_nested_lookup() {
        let (mut arena, mut scopes) = setup();

        let x = arena.intern("x");
        let outer_val = arena.add(Value::Int(1), Span::new(0, 1));
        scopes.bind(x, outer_val);

        scopes.push();

        // Inner scope can see outer binding
        assert_eq!(scopes.lookup(x), Some(outer_val));

        // Shadow with inner binding
        let inner_val = arena.add(Value::Int(2), Span::new(2, 3));
        scopes.bind(x, inner_val);
        assert_eq!(scopes.lookup(x), Some(inner_val));

        // Pop inner scope; outer binding restored
        scopes.pop();
        assert_eq!(scopes.lookup(x), Some(outer_val));
    }

    #[test]
    fn scope_pop_root_prevented() {
        let (_, mut scopes) = setup();

        assert_eq!(scopes.depth(), 1);
        assert!(scopes.pop().is_none());
        assert_eq!(scopes.depth(), 1);
    }

    #[test]
    fn scope_multiple_bindings() {
        let (mut arena, mut scopes) = setup();

        let x = arena.intern("x");
        let y = arena.intern("y");
        let z = arena.intern("z");

        let v1 = arena.add(Value::Int(1), Span::new(0, 1));
        let v2 = arena.add(Value::Int(2), Span::new(2, 3));
        let v3 = arena.add(Value::Int(3), Span::new(4, 5));

        scopes.bind(x, v1);
        scopes.bind(y, v2);
        scopes.bind(z, v3);

        assert_eq!(scopes.lookup(x), Some(v1));
        assert_eq!(scopes.lookup(y), Some(v2));
        assert_eq!(scopes.lookup(z), Some(v3));
    }

    #[test]
    fn environment_creation() {
        let env = Environment::new();

        assert_eq!(env.scopes.depth(), 1);
        // Array module is registered with placeholder functions
        assert!(env.has_module("Array"));
        assert!(env.get_module_fn(&["Array", "map"]).is_some());
        assert!(env.module_fn_exists(&["Array", "map"]));
        assert!(env.module_fn_exists(&["Array", "filter"]));
        assert!(env.module_fn_exists(&["Array", "reduce"]));
        assert!(env.module_fn_exists(&["Array", "foreach"]));
        // String module is registered with functions
        assert!(env.has_module("String"));
        assert!(env.get_module_fn(&["String", "length"]).is_some());
        assert!(env.module_fn_exists(&["String", "upper"]));
        assert!(env.module_fn_exists(&["String", "split"]));
        assert!(env.module_fn_exists(&["String", "join"]));
        // Math module is registered with functions
        assert!(env.has_module("Math"));
        assert!(env.module_fn_exists(&["Math", "abs"]));
        assert!(env.module_fn_exists(&["Math", "min"]));
        assert!(env.module_fn_exists(&["Math", "sqrt"]));
        // Trig functions are in the Trig submodule
        assert!(env.module_fn_exists(&["Math", "Trig", "sin"]));
        assert!(env.module_fn_exists(&["Math", "Trig", "cos"]));
        assert!(env.module_fn_exists(&["Math", "Trig", "tan"]));
        // Random module is registered with functions
        assert!(env.has_module("Random"));
        assert!(env.module_fn_exists(&["Random", "random"]));
        assert!(env.module_fn_exists(&["Random", "int"]));
        assert!(env.module_fn_exists(&["Random", "choice"]));
        assert!(env.module_fn_exists(&["Random", "uuid"]));
        // Io module is registered with functions
        assert!(env.has_module("Io"));
        assert!(env.module_fn_exists(&["Io", "get-line"]));
        assert!(env.module_fn_exists(&["Io", "print"]));
        assert!(env.module_fn_exists(&["Io", "println"]));
        assert!(env.module_fn_exists(&["Io", "eprint"]));
        assert!(env.module_fn_exists(&["Io", "eprintln"]));
    }

    #[test]
    fn environment_scope_operations() {
        let mut arena = ValueArena::new();
        let mut env = Environment::new();

        let name = arena.intern("test");
        let val = arena.add(Value::Bool(true), Span::new(0, 4));

        env.scopes.bind(name, val);
        assert_eq!(env.scopes.lookup(name), Some(val));
    }

    // Dummy primitive for testing
    fn dummy_prim<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move { Ok(ctx.arena.add(Value::Int(42), ctx.span)) })
    }

    fn dummy_def(name: &'static str) -> PrimDef {
        PrimDef {
            name,
            f: dummy_prim,
            ty: Scheme::mono(Ty::Int),
        }
    }

    #[test]
    fn module_submodule_lookup() {
        // Create a module with a submodule: Math.Trig.sin
        let trig = Module::from_prims(&[dummy_def("sin"), dummy_def("cos")]);

        let math = Module::from_prims(&[dummy_def("sqrt"), dummy_def("abs")])
            .with_submodule("Trig", trig);

        // Direct function lookup
        assert!(math.get_fn(&["sqrt"]).is_some());
        assert!(math.get_fn(&["abs"]).is_some());
        assert!(math.get_fn(&["unknown"]).is_none());

        // Submodule function lookup
        assert!(math.get_fn(&["Trig", "sin"]).is_some());
        assert!(math.get_fn(&["Trig", "cos"]).is_some());
        assert!(math.get_fn(&["Trig", "tan"]).is_none());

        // contains_fn
        assert!(math.contains_fn(&["sqrt"]));
        assert!(math.contains_fn(&["Trig", "sin"]));
        assert!(!math.contains_fn(&["Trig", "tan"]));
        assert!(!math.contains_fn(&["Unknown", "fn"]));
    }

    #[test]
    fn module_deeply_nested_lookup() {
        // Create deeply nested: A.B.C.fn
        let c = Module::from_prims(&[dummy_def("fn")]);
        let b = Module::default().with_submodule("C", c);
        let a = Module::default().with_submodule("B", b);

        // Should find A.B.C.fn
        assert!(a.get_fn(&["B", "C", "fn"]).is_some());
        assert!(a.contains_fn(&["B", "C", "fn"]));

        // Should not find partial paths
        assert!(a.get_fn(&["B"]).is_none());
        assert!(a.get_fn(&["B", "C"]).is_none());

        // Should not find wrong paths
        assert!(a.get_fn(&["B", "C", "other"]).is_none());
        assert!(a.get_fn(&["B", "D", "fn"]).is_none());
    }

    #[test]
    fn environment_module_fn_exists_with_path() {
        let env = Environment::new();

        // Valid paths
        assert!(env.module_fn_exists(&["Array", "length"]));
        assert!(env.module_fn_exists(&["Array", "map"]));
        assert!(env.module_fn_exists(&["String", "split"]));
        assert!(env.module_fn_exists(&["Math", "sqrt"]));

        // Invalid paths
        assert!(!env.module_fn_exists(&["Array", "unknown"]));
        assert!(!env.module_fn_exists(&["Unknown", "keys"]));
        assert!(!env.module_fn_exists(&[]));
        assert!(!env.module_fn_exists(&["Array"]));
    }

    #[test]
    fn module_constants() {
        let mut arena = ValueArena::new();
        let span = Span::new(0, 0);

        // Create a module with constants
        let pi = arena.add(Value::Int(314), span);
        let e = arena.add(Value::Int(271), span);

        let math = Module::from_prims(&[dummy_def("sqrt")])
            .with_const("pi", pi, Ty::Int)
            .with_const("e", e, Ty::Int);

        // Lookup constants
        assert_eq!(math.get_const(&["pi"]), Some(pi));
        assert_eq!(math.get_const(&["e"]), Some(e));
        assert_eq!(math.get_const(&["tau"]), None);

        // contains_const
        assert!(math.contains_const(&["pi"]));
        assert!(math.contains_const(&["e"]));
        assert!(!math.contains_const(&["tau"]));

        // Functions are NOT constants
        assert!(!math.contains_const(&["sqrt"]));
        // Constants are NOT functions
        assert!(!math.contains_fn(&["pi"]));
    }

    #[test]
    fn environment_module_const_exists() {
        let env = Environment::new();

        // Math module has constants
        assert!(env.module_const_exists(&["Math", "pi"]));
        assert!(env.module_const_exists(&["Math", "e"]));
        assert!(env.module_const_exists(&["Math", "tau"]));
        assert!(env.module_const_exists(&["Math", "inf"]));
        assert!(env.module_const_exists(&["Math", "neg-inf"]));

        // Math.sqrt is a function, not a constant
        assert!(!env.module_const_exists(&["Math", "sqrt"]));

        // Can retrieve constant values
        let pi_id = env.get_module_const(&["Math", "pi"]).unwrap();
        let pi_val = env.consts.get(pi_id).unwrap();
        assert!(
            matches!(pi_val, Value::Float(f) if (*f - std::f64::consts::PI).abs() < 1e-10)
        );
    }

    // --- PrimDef and type scheme tests ---

    #[test]
    fn module_from_prims_registers_types() {
        let m = Module::from_prims(&[
            PrimDef {
                name: "foo",
                f: dummy_prim,
                ty: Scheme::mono(Ty::func([Ty::Int], Ty::Bool)),
            },
            PrimDef {
                name: "bar",
                f: dummy_prim,
                ty: Scheme::poly(|t| {
                    Ty::func([Ty::Array(Box::new(t))], Ty::Int)
                }),
            },
        ]);

        // Functions are registered
        assert!(m.contains_fn(&["foo"]));
        assert!(m.contains_fn(&["bar"]));

        // Types are registered
        assert!(m.get_fn_type(&["foo"]).is_some());
        assert!(m.get_fn_type(&["bar"]).is_some());
        assert!(m.get_fn_type(&["baz"]).is_none());
    }

    #[test]
    fn module_get_fn_type_monomorphic() {
        let m = Module::from_prims(&[PrimDef {
            name: "sqrt",
            f: dummy_prim,
            ty: Scheme::mono(Ty::func([Ty::Float], Ty::Float)),
        }]);

        let scheme = m.get_fn_type(&["sqrt"]).unwrap();
        assert!(scheme.vars.is_empty()); // monomorphic
        assert_eq!(scheme.ty, Ty::Fn(vec![Ty::Float], Box::new(Ty::Float)));
    }

    #[test]
    fn module_get_fn_type_polymorphic() {
        use crate::typecheck::TyVar;

        let m = Module::from_prims(&[PrimDef {
            name: "length",
            f: dummy_prim,
            ty: Scheme::poly(|t| Ty::func([Ty::Array(Box::new(t))], Ty::Int)),
        }]);

        let scheme = m.get_fn_type(&["length"]).unwrap();
        assert_eq!(scheme.vars, vec![TyVar::new(0)]);
        assert_eq!(
            scheme.ty,
            Ty::Fn(
                vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                Box::new(Ty::Int)
            )
        );
    }

    #[test]
    fn module_get_fn_type_submodule() {
        let sub = Module::from_prims(&[PrimDef {
            name: "sin",
            f: dummy_prim,
            ty: Scheme::mono(Ty::func([Ty::Float], Ty::Float)),
        }]);
        let m = Module::default().with_submodule("Trig", sub);

        // Submodule function type lookup
        let scheme = m.get_fn_type(&["Trig", "sin"]).unwrap();
        assert!(scheme.vars.is_empty());
        assert_eq!(scheme.ty, Ty::Fn(vec![Ty::Float], Box::new(Ty::Float)));

        // Non-existent paths
        assert!(m.get_fn_type(&["sin"]).is_none());
        assert!(m.get_fn_type(&["Trig", "cos"]).is_none());
    }

    #[test]
    fn environment_get_module_fn_type() {
        let env = Environment::new();

        // Array.length has a polymorphic type
        let length_ty = env.get_module_fn_type(&["Array", "length"]).unwrap();
        assert_eq!(length_ty.vars.len(), 1);

        // String.length has a monomorphic type
        let str_len_ty = env.get_module_fn_type(&["String", "length"]).unwrap();
        assert!(str_len_ty.vars.is_empty());

        // Math.Trig.sin has a monomorphic type
        let sin_ty = env.get_module_fn_type(&["Math", "Trig", "sin"]).unwrap();
        assert!(sin_ty.vars.is_empty());

        // Non-existent paths return None
        assert!(env.get_module_fn_type(&["Unknown", "fn"]).is_none());
        assert!(env.get_module_fn_type(&["Array", "unknown"]).is_none());
    }

    #[test]
    fn environment_builtin_types_correct() {
        let env = Environment::new();

        // Array.map: forall T U. (Array[T], T -> U) -> Array[U]
        let map_ty = env.get_module_fn_type(&["Array", "map"]).unwrap();
        assert_eq!(map_ty.vars.len(), 2);

        // Array.filter: forall T. (Array[T], T -> Bool) -> Array[T]
        let filter_ty = env.get_module_fn_type(&["Array", "filter"]).unwrap();
        assert_eq!(filter_ty.vars.len(), 1);

        // Map.lookup: forall K V. (Map[K, V], K) -> Option[V]
        let lookup_ty = env.get_module_fn_type(&["Map", "lookup"]).unwrap();
        assert_eq!(lookup_ty.vars.len(), 2);

        // Result.map: forall T U E. (Result[T, E], T -> U) -> Result[U, E]
        let res_map_ty = env.get_module_fn_type(&["Result", "map"]).unwrap();
        assert_eq!(res_map_ty.vars.len(), 3);

        // Time.now: () -> Time (monomorphic)
        let now_ty = env.get_module_fn_type(&["Time", "now"]).unwrap();
        assert!(now_ty.vars.is_empty());
        assert_eq!(now_ty.ty, Ty::Fn(vec![], Box::new(Ty::Time)));
    }
}
