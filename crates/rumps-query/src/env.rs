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
    "Array", "Iter", "String", "Math", "Random", "Map", "Time", "Option",
    "Result", "Io",
];

use futures::future::BoxFuture;
use smallvec::{smallvec, SmallVec};

use crate::ast::{BinOp, Intrinsic, PostfixOp, UnOp};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{FunctionDef, TypeId, Value, ValueArena, ValueId};
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
/// Primitives are callable built-in functions like `Iter.map`, `String.split`,
/// etc. They take a context and arguments, returning a future that resolves
/// to a `ValueId`.
///
/// Note: database intrinsics (`@GET`, `@SET`, `@KILL`, etc.) have special
/// syntax and are represented as `Expr::Intrinsic`, not primitives.
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

/// When a transaction is required for an intrinsic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxnReq {
    /// No transaction required (read-only intrinsics).
    None,
    /// Transaction required when targeting globals.
    Globals,
}

/// Definition of a database intrinsic with its type signature.
///
/// Similar to `PrimDef` but for DB intrinsics which have special syntax
/// (`@` prefix) and take `RefTarget` arguments rather than expressions.
pub(crate) struct IntrinsicDef {
    pub(crate) name: &'static str,
    /// Type scheme; only the return type is used (arguments are `RefTarget`).
    pub(crate) ty: Scheme,
    pub(crate) txn: TxnReq,
}

impl Intrinsic {
    /// Get the definition for this intrinsic.
    pub(crate) fn def(self) -> IntrinsicDef {
        match self {
            Self::Get => IntrinsicDef {
                name: "@GET",
                ty: scheme!((Ref) -> Option[Storable]),
                txn: TxnReq::None,
            },
            Self::Set => IntrinsicDef {
                name: "@SET",
                ty: scheme!((Ref, Storable) -> Result[Unit, String]),
                txn: TxnReq::Globals,
            },
            Self::Kill => IntrinsicDef {
                name: "@KILL",
                ty: scheme!((Ref) -> Result[Unit, String]),
                txn: TxnReq::Globals,
            },
            Self::Data => IntrinsicDef {
                name: "@DATA",
                ty: scheme!((Ref) -> DataStatus),
                txn: TxnReq::None,
            },
            Self::Order => IntrinsicDef {
                name: "@ORDER",
                ty: scheme!((Ref) -> Option[Subscript]),
                txn: TxnReq::None,
            },
            Self::Query => IntrinsicDef {
                name: "@QUERY",
                ty: scheme!((Ref) -> Option[Array[Subscript]]),
                txn: TxnReq::None,
            },
        }
    }
}

/// Definition of a binary operator with its type signature.
pub(crate) struct BinOpDef {
    pub(crate) name: &'static str,
    pub(crate) ty: Scheme,
}

impl BinOp {
    /// Get the definition for this binary operator.
    pub(crate) fn def(self) -> BinOpDef {
        match self {
            // Arithmetic: forall T: Numeric. (T, T) -> T
            Self::Add => BinOpDef {
                name: "+",
                ty: scheme!(forall T: Numeric. (T, T) -> T),
            },
            Self::Sub => BinOpDef {
                name: "-",
                ty: scheme!(forall T: Numeric. (T, T) -> T),
            },
            Self::Mul => BinOpDef {
                name: "*",
                ty: scheme!(forall T: Numeric. (T, T) -> T),
            },
            Self::Div => BinOpDef {
                name: "/",
                ty: scheme!((Float, Float) -> Float),
            },
            Self::FloorDiv => BinOpDef {
                name: "//",
                ty: scheme!(forall T: Numeric. (T, T) -> T),
            },
            Self::Mod => BinOpDef {
                name: "%",
                ty: scheme!(forall T: Numeric. (T, T) -> T),
            },
            Self::Pow => BinOpDef {
                name: "**",
                ty: scheme!(forall T: Numeric. (T, T) -> T),
            },

            // Comparison: forall T. (T, T) -> Bool
            Self::Eq => BinOpDef {
                name: "==",
                ty: scheme!(forall T. (T, T) -> Bool),
            },
            Self::Ne => BinOpDef {
                name: "!=",
                ty: scheme!(forall T. (T, T) -> Bool),
            },
            Self::Lt => BinOpDef {
                name: "<",
                ty: scheme!(forall T. (T, T) -> Bool),
            },
            Self::Gt => BinOpDef {
                name: ">",
                ty: scheme!(forall T. (T, T) -> Bool),
            },
            Self::Le => BinOpDef {
                name: "<=",
                ty: scheme!(forall T. (T, T) -> Bool),
            },
            Self::Ge => BinOpDef {
                name: ">=",
                ty: scheme!(forall T. (T, T) -> Bool),
            },

            // Logical: (Bool, Bool) -> Bool
            Self::And => BinOpDef {
                name: "AND",
                ty: scheme!((Bool, Bool) -> Bool),
            },
            Self::Or => BinOpDef {
                name: "OR",
                ty: scheme!((Bool, Bool) -> Bool),
            },

            // Bitwise: forall T: BitLike. (T, T) -> T
            Self::BitAnd => BinOpDef {
                name: "&",
                ty: scheme!(forall T: BitLike. (T, T) -> T),
            },
            Self::BitOr => BinOpDef {
                name: "|",
                ty: scheme!(forall T: BitLike. (T, T) -> T),
            },
            Self::Shl => BinOpDef {
                name: "<<",
                ty: scheme!(forall T: BitLike. (T, T) -> T),
            },
            Self::Shr => BinOpDef {
                name: ">>",
                ty: scheme!(forall T: BitLike. (T, T) -> T),
            },

            // Concat: forall T: Monoid. (T, T) -> T
            Self::Concat => BinOpDef {
                name: "++",
                ty: scheme!(forall T: Monoid. (T, T) -> T),
            },

            // Coalesce: forall T, F: Fallible[T]. (F, T) -> T
            Self::Coalesce => BinOpDef {
                name: "??",
                ty: scheme!(forall T, F: Fallible[T]. (F, T) -> T),
            },

            // Pipe: forall T, U. (T, (T) -> U) -> U
            Self::Pipe => BinOpDef {
                name: "|>",
                ty: scheme!(forall T, U. (T, (T) -> U) -> U),
            },
        }
    }
}

/// Definition of a unary (prefix) operator with its type signature.
pub(crate) struct UnOpDef {
    pub(crate) name: &'static str,
    pub(crate) ty: Scheme,
}

impl UnOp {
    /// Get the definition for this unary operator.
    pub(crate) fn def(self) -> UnOpDef {
        match self {
            Self::Neg => UnOpDef {
                name: "-",
                ty: scheme!(forall T: Numeric. (T) -> T),
            },
            Self::Not => UnOpDef {
                name: "NOT",
                ty: scheme!((Bool) -> Bool),
            },
            Self::Wrap => UnOpDef {
                name: "?",
                ty: scheme!(forall T. (T) -> Option[T]),
            },
        }
    }
}

/// Definition of a postfix operator with its type signature.
pub(crate) struct PostfixOpDef {
    pub(crate) name: &'static str,
    pub(crate) ty: Scheme,
}

impl PostfixOp {
    /// Get the definition for this postfix operator.
    pub(crate) fn def(self) -> PostfixOpDef {
        match self {
            Self::Unwrap => PostfixOpDef {
                name: "!",
                ty: scheme!(forall T, F: Fallible[T]. (F) -> T),
            },
        }
    }
}

/// A built-in module containing primitive functions, constants, and submodules.
///
/// Modules group related functions under a namespace (e.g., `Iter.map`,
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

    /// Get all public members (functions and constants) with their type schemes.
    ///
    /// Returns `(name, scheme)` pairs for all top-level members.
    /// All builtin module members are public.
    pub(crate) fn public_members(&self) -> Vec<(String, Scheme)> {
        let fns = self
            .functions
            .iter()
            .map(|(name, (_, scheme))| (name.clone(), scheme.clone()));
        let consts = self
            .constants
            .iter()
            .map(|(name, (_, ty))| (name.clone(), Scheme::mono(ty.clone())));
        fns.chain(consts).collect()
    }

    /// Navigate to a submodule by path.
    ///
    /// An empty path returns `self`. Otherwise, navigates through submodules.
    pub(crate) fn get_submodule(&self, path: &[&str]) -> Option<&Self> {
        match path {
            [] => Some(self),
            [first, rest @ ..] => self
                .submodules
                .get(*first)
                .and_then(|m| m.get_submodule(rest)),
        }
    }
}

/// A user-defined module containing functions and constants.
///
/// Unlike builtin `Module`s which use `PrimFn`, user modules store:
/// - Functions as `FunctionDef`s (not closures; siblings are bound at call time)
/// - Constants as `ValueId`s pointing to evaluated values
#[derive(Default, Clone)]
pub(crate) struct UserModule {
    /// Functions in this module, keyed by function name.
    /// Stored as `FunctionDef`s so sibling lookup happens at call time,
    /// enabling mutual recursion between module functions.
    pub(crate) functions: HashMap<String, FunctionDef>,

    /// Constants in this module, keyed by constant name.
    pub(crate) constants: HashMap<String, ValueId>,

    /// Submodules, keyed by submodule name.
    pub(crate) submodules: HashMap<String, Self>,
}

impl UserModule {
    /// Look up a function by path within this module.
    pub(crate) fn get_fn(&self, path: &[&str]) -> Option<&FunctionDef> {
        match path {
            [] => None,
            [name] => self.functions.get(*name),
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

    /// Look up a user module by path.
    ///
    /// The path is the module path segments (e.g., `["Counter"]` or
    /// `["Counter", "Inner"]` for nested modules).
    pub(crate) fn get_user_module(&self, path: &[&str]) -> Option<&UserModule> {
        path.split_first().and_then(|(first, rest)| {
            self.user_modules.get(*first).and_then(|m| {
                rest.iter().try_fold(m, |acc, seg| acc.submodules.get(*seg))
            })
        })
    }

    /// Look up a user module function by path.
    pub(crate) fn get_user_module_fn(
        &self,
        path: &[&str],
    ) -> Option<&FunctionDef> {
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

    /// Check if a name is a builtin module.
    pub(crate) fn is_builtin_module(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// Get a builtin module by name.
    pub(crate) fn get_builtin_module(&self, name: &str) -> Option<&Module> {
        self.modules.get(name)
    }

    /// Get a builtin module or submodule by path.
    ///
    /// The path should be like `["Math"]` or `["Math", "Trig"]`.
    pub(crate) fn get_builtin_module_by_path(
        &self,
        path: &[&str],
    ) -> Option<&Module> {
        path.split_first().and_then(|(first, rest)| {
            self.modules.get(*first).and_then(|m| m.get_submodule(rest))
        })
    }

    /// Look up a function by its full path.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    ///
    /// Examples:
    /// - `["Iter", "length"]` -> `Iter.length`
    /// - `["Math", "Trig", "sin"]` -> `Math.Trig.sin`
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
    /// Note: Iterable higher-order functions (`map`, `filter`, `reduce`, `foreach`)
    /// are handled specially by the interpreter. We register placeholders here
    /// so that `module_fn_exists` returns true for name resolution.
    fn register_builtins(&mut self) {
        use crate::primitives::{
            Array, Io, Iter, Map, Math, Opt, Prim, Random, Res, Str, Time, Trig,
        };

        self.modules.insert(
            "Array".to_string(),
            Module::from_prims(&[
                // Array-specific primitives (HOFs moved to Iter module)
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
                    name: "concat",
                    f: Array::concat,
                    ty: scheme!(forall T. (Array[T], Array[T]) -> Array[T]),
                },
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
            "Iter".to_string(),
            Module::from_prims(&[
                // Higher-order functions; handled by interpreter (placeholders)
                PrimDef {
                    name: "map",
                    f: Iter::placeholder,
                    ty: scheme!(forall I: Iterable[T], T U. ((T) -> U, I) -> Array[U]),
                },
                PrimDef {
                    name: "filter",
                    f: Iter::placeholder,
                    ty: scheme!(forall I: Iterable[T], T. ((T) -> Bool, I) -> Array[T]),
                },
                PrimDef {
                    name: "reduce",
                    f: Iter::placeholder,
                    ty: scheme!(forall I: Iterable[T], T U. ((U, T) -> U, U, I) -> U),
                },
                PrimDef {
                    name: "foreach",
                    f: Iter::placeholder,
                    ty: scheme!(forall I: Iterable[T], T. ((T) -> Unit, I) -> Unit),
                },
                // Regular primitives
                PrimDef {
                    name: "length",
                    f: Iter::length,
                    ty: scheme!(forall I: Iterable[T], T. (I) -> Int),
                },
                PrimDef {
                    name: "reverse",
                    f: Iter::reverse,
                    ty: scheme!(forall I: Iterable[T], T. (I) -> Array[T]),
                },
                PrimDef {
                    name: "contains",
                    f: Iter::contains,
                    ty: scheme!(forall I: Iterable[T], T. (I, T) -> Bool),
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
                ty: scheme!(forall T: Numeric. (T) -> T),
            },
            PrimDef {
                name: "min",
                f: Math::min,
                ty: scheme!(forall T: Numeric. (T, T) -> T),
            },
            PrimDef {
                name: "max",
                f: Math::max,
                ty: scheme!(forall T: Numeric. (T, T) -> T),
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
                PrimDef {
                    name: "flatten",
                    f: Opt::flatten,
                    ty: scheme!(forall T. (Option[Option[T]]) -> Option[T]),
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
                PrimDef {
                    name: "flatten",
                    f: Res::flatten,
                    ty: scheme!(forall T E. (Result[Result[T, E], E]) -> Result[T, E]),
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
