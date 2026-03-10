//! Variable environments for lexical scope and primitive functions.
//!
//! The `Environment` tracks lexical scope for `let` bindings and callable names.
//! `set` variables (both local and global) go through the `Database`, not here.

#![allow(dead_code)]

use std::collections::HashMap;

use crate::typecheck::{
    BuiltinClass, BuiltinClassTag, Scheme, Ty, TyArena, TyId, TyVar,
};

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

use crate::ast::{BinOp, Intrinsic, PostfixOp, UnOp};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{FunctionDef, TypeId, Value, ValueArena, ValueId};
use crate::{Error, Result, Span};

/// Stack of lexical scopes for `let` bindings.
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
/// Note: database intrinsics (`@get`, `@set`, `@kill`, etc.) have special
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
    pub(crate) fn def(self, a: &mut TyArena) -> IntrinsicDef {
        let ref_ty = a.named(TypeId::REF, smallvec![]);
        match self {
            Self::Get => {
                let storable = a.named(TypeId::STORABLE, smallvec![]);
                let opt = a.option(storable);
                let ty = a.func(smallvec![ref_ty], opt);
                IntrinsicDef {
                    name: "@GET",
                    ty: Scheme::mono(ty),
                    txn: TxnReq::None,
                }
            }
            Self::Set => {
                let storable = a.named(TypeId::STORABLE, smallvec![]);
                let ret = a.result(TyArena::UNIT, TyArena::STRING);
                let ty = a.func(smallvec![ref_ty, storable], ret);
                IntrinsicDef {
                    name: "@SET",
                    ty: Scheme::mono(ty),
                    txn: TxnReq::Globals,
                }
            }
            Self::Kill => {
                let ret = a.result(TyArena::UNIT, TyArena::STRING);
                let ty = a.func(smallvec![ref_ty], ret);
                IntrinsicDef {
                    name: "@KILL",
                    ty: Scheme::mono(ty),
                    txn: TxnReq::Globals,
                }
            }
            Self::Data => {
                let ty = a.func(smallvec![ref_ty], TyArena::DATA_STATUS);
                IntrinsicDef {
                    name: "@DATA",
                    ty: Scheme::mono(ty),
                    txn: TxnReq::None,
                }
            }
            Self::Order => {
                let subscript = a.named(TypeId::SUBSCRIPT, smallvec![]);
                let opt = a.option(subscript);
                let ty = a.func(smallvec![ref_ty], opt);
                IntrinsicDef {
                    name: "@ORDER",
                    ty: Scheme::mono(ty),
                    txn: TxnReq::None,
                }
            }
            Self::Query => {
                let subscript = a.named(TypeId::SUBSCRIPT, smallvec![]);
                let arr = a.array(subscript);
                let opt = a.option(arr);
                let ty = a.func(smallvec![ref_ty], opt);
                IntrinsicDef {
                    name: "@QUERY",
                    ty: Scheme::mono(ty),
                    txn: TxnReq::None,
                }
            }
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
    pub(crate) fn def(self, a: &mut TyArena) -> BinOpDef {
        let v0 = a.var(0);

        // Helper: `forall T: C. (T, T) -> T`
        let binary_constrained = |a: &mut TyArena, v0: TyId, tag| {
            let ty = a.func(smallvec![v0, v0], v0);
            Scheme {
                vars: vec![TyVar::new(0)],
                ty,
                constraints: smallvec![(
                    TyVar::new(0),
                    BuiltinClass::Simple(tag)
                )],
            }
        };

        // Helper: `forall T: C. (T, T) -> Bool`
        let cmp_constrained = |a: &mut TyArena, v0: TyId, tag| {
            let ty = a.func(smallvec![v0, v0], TyArena::BOOL);
            Scheme {
                vars: vec![TyVar::new(0)],
                ty,
                constraints: smallvec![(
                    TyVar::new(0),
                    BuiltinClass::Simple(tag)
                )],
            }
        };

        match self {
            // Arithmetic: `forall T: Numeric. (T, T) -> T`
            Self::Add => BinOpDef {
                name: "+",
                ty: binary_constrained(a, v0, BuiltinClassTag::Numeric),
            },
            Self::Sub => BinOpDef {
                name: "-",
                ty: binary_constrained(a, v0, BuiltinClassTag::Numeric),
            },
            Self::Mul => BinOpDef {
                name: "*",
                ty: binary_constrained(a, v0, BuiltinClassTag::Numeric),
            },
            Self::Div => {
                let ty = a.func(
                    smallvec![TyArena::FLOAT, TyArena::FLOAT],
                    TyArena::FLOAT,
                );
                BinOpDef {
                    name: "/",
                    ty: Scheme::mono(ty),
                }
            }
            Self::FloorDiv => BinOpDef {
                name: "//",
                ty: binary_constrained(a, v0, BuiltinClassTag::Numeric),
            },
            Self::Mod => BinOpDef {
                name: "%",
                ty: binary_constrained(a, v0, BuiltinClassTag::Numeric),
            },
            Self::Pow => BinOpDef {
                name: "**",
                ty: binary_constrained(a, v0, BuiltinClassTag::Numeric),
            },

            // Comparison: `forall T: Eq. (T, T) -> Bool`
            Self::Eq => BinOpDef {
                name: "==",
                ty: cmp_constrained(a, v0, BuiltinClassTag::Eq),
            },
            Self::Ne => BinOpDef {
                name: "!=",
                ty: cmp_constrained(a, v0, BuiltinClassTag::Eq),
            },
            Self::Lt => BinOpDef {
                name: "<",
                ty: cmp_constrained(a, v0, BuiltinClassTag::Ord),
            },
            Self::Gt => BinOpDef {
                name: ">",
                ty: cmp_constrained(a, v0, BuiltinClassTag::Ord),
            },
            Self::Le => BinOpDef {
                name: "<=",
                ty: cmp_constrained(a, v0, BuiltinClassTag::Ord),
            },
            Self::Ge => BinOpDef {
                name: ">=",
                ty: cmp_constrained(a, v0, BuiltinClassTag::Ord),
            },

            // Logical: `(Bool, Bool) -> Bool`
            Self::And => {
                let ty = a.func(
                    smallvec![TyArena::BOOL, TyArena::BOOL],
                    TyArena::BOOL,
                );
                BinOpDef {
                    name: "AND",
                    ty: Scheme::mono(ty),
                }
            }
            Self::Or => {
                let ty = a.func(
                    smallvec![TyArena::BOOL, TyArena::BOOL],
                    TyArena::BOOL,
                );
                BinOpDef {
                    name: "OR",
                    ty: Scheme::mono(ty),
                }
            }

            // Bitwise: `forall T: BitLike. (T, T) -> T`
            Self::BitAnd => BinOpDef {
                name: "&",
                ty: binary_constrained(a, v0, BuiltinClassTag::BitLike),
            },
            Self::BitOr => BinOpDef {
                name: "|",
                ty: binary_constrained(a, v0, BuiltinClassTag::BitLike),
            },
            Self::Shl => BinOpDef {
                name: "<<",
                ty: binary_constrained(a, v0, BuiltinClassTag::BitLike),
            },
            Self::Shr => BinOpDef {
                name: ">>",
                ty: binary_constrained(a, v0, BuiltinClassTag::BitLike),
            },

            // Concat: `forall T: Monoid. (T, T) -> T`
            Self::Concat => BinOpDef {
                name: "++",
                ty: binary_constrained(a, v0, BuiltinClassTag::Monoid),
            },

            // Coalesce: `forall T, F: Fallible. (F[T], T) -> T`
            Self::Coalesce => {
                let hkt = a.hkt(TyVar::new(1), smallvec![v0]);
                let ty = a.func(smallvec![hkt, v0], v0);
                BinOpDef {
                    name: "??",
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty,
                        constraints: smallvec![(
                            TyVar::new(1),
                            BuiltinClass::Hkt(BuiltinClassTag::Fallible, None,)
                        )],
                    },
                }
            }

            // Pipe: `forall T, U. (T, (T) -> U) -> U`
            Self::Pipe => {
                let v1 = a.var(1);
                let cb = a.func(smallvec![v0], v1);
                let ty = a.func(smallvec![v0, cb], v1);
                BinOpDef {
                    name: "|>",
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty,
                        constraints: smallvec![],
                    },
                }
            }
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
    pub(crate) fn def(self, a: &mut TyArena) -> UnOpDef {
        match self {
            Self::Neg => {
                let v0 = a.var(0);
                let ty = a.func(smallvec![v0], v0);
                UnOpDef {
                    name: "-",
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty,
                        constraints: smallvec![(
                            TyVar::new(0),
                            BuiltinClass::Simple(BuiltinClassTag::Negatable)
                        )],
                    },
                }
            }
            Self::Not => {
                let ty = a.func(smallvec![TyArena::BOOL], TyArena::BOOL);
                UnOpDef {
                    name: "NOT",
                    ty: Scheme::mono(ty),
                }
            }
            Self::Wrap => {
                let v0 = a.var(0);
                let hkt = a.hkt(TyVar::new(1), smallvec![v0]);
                let ty = a.func(smallvec![v0], hkt);
                UnOpDef {
                    name: "?",
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty,
                        constraints: smallvec![(
                            TyVar::new(1),
                            BuiltinClass::Hkt(BuiltinClassTag::Fallible, None,)
                        )],
                    },
                }
            }
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
    pub(crate) fn def(self, a: &mut TyArena) -> PostfixOpDef {
        match self {
            Self::Unwrap => {
                let v0 = a.var(0);
                let hkt = a.hkt(TyVar::new(1), smallvec![v0]);
                let ty = a.func(smallvec![hkt], v0);
                PostfixOpDef {
                    name: "!",
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty,
                        constraints: smallvec![(
                            TyVar::new(1),
                            BuiltinClass::Hkt(BuiltinClassTag::Fallible, None,)
                        )],
                    },
                }
            }
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
    constants: HashMap<String, (ValueId, TyId)>,

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
        ty: TyId,
    ) -> Self {
        self.constants.insert(name.to_string(), (id, ty));
        self
    }

    /// Mutably add a constant with its type.
    pub(crate) fn add_const(&mut self, name: &str, id: ValueId, ty: TyId) {
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
    pub(crate) fn get_const_type(&self, path: &[&str]) -> Option<TyId> {
        match path {
            [] => None,
            [name] => self.constants.get(*name).map(|(_, ty)| *ty),
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
            .map(|(name, (_, ty))| (name.clone(), Scheme::mono(*ty)));
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
/// - Lexical scopes for `let` bindings (via `Scopes`)
/// - Built-in modules containing primitive functions (e.g., `Array`, `String`)
/// - User-defined modules containing closures and constants
/// - Module constants (e.g., `Math.pi`, `Math.e`)
///
/// Note: `set` variables (both local and global) are stored in the `Database`,
/// not in the environment. Only `let` bindings live here. You can `get` a `set`
/// (local or global), but not a `let`; `let`s can be referenced by name directly.
pub(crate) struct Environment {
    /// Lexical scope stack for `let` bindings.
    pub(crate) scopes: Scopes,

    /// Built-in modules (e.g., `Object`, `Array`).
    modules: HashMap<String, Module>,

    /// User-defined modules.
    user_modules: HashMap<String, UserModule>,

    /// Arena backing module constant `ValueId`s.
    pub(crate) consts: ValueArena,

    /// Type arena for builtin scheme types.
    pub(crate) ty_arena: TyArena,
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
            ty_arena: TyArena::new(),
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
    pub(crate) fn get_module_const_type(&self, path: &[&str]) -> Option<TyId> {
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
            Array, Io, Map, Math, Opt, Prim, Random, Res, Str, Time, Trig,
        };

        let a = &mut self.ty_arena;

        // Pre-allocate common type variables and compound types
        let v0 = a.var(0);
        let v1 = a.var(1);
        let v2 = a.var(2);
        let arr_v0 = a.array(v0);
        let arr_v1 = a.array(v1);
        let opt_v0 = a.option(v0);

        // Helper: unconstrained poly1 scheme
        let poly1 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0)],
            ty,
            constraints: smallvec![],
        };

        // Helper: unconstrained poly2 scheme
        let poly2 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };

        // Helper: unconstrained poly3 scheme
        let poly3 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
            ty,
            constraints: smallvec![],
        };

        // --- Array module ---
        // `push: (Array[T], T) -> Array[T]`
        let push_ty = a.func(smallvec![arr_v0, v0], arr_v0);
        // `pop: (Array[T]) -> Array[T]`
        let pop_ty = a.func(smallvec![arr_v0], arr_v0);
        // `head: (Array[T]) -> Option[T]`
        let head_ty = a.func(smallvec![arr_v0], opt_v0);
        // `tail: (Array[T]) -> Array[T]`
        let tail_ty = pop_ty;
        // `sort: (Array[T]) -> Array[T]`
        let sort_ty = pop_ty;
        // `slice: (Array[T], Int, Int) -> Array[T]`
        let slice_ty =
            a.func(smallvec![arr_v0, TyArena::INT, TyArena::INT], arr_v0);
        // `concat: (Array[T], Array[T]) -> Array[T]`
        let concat_ty = a.func(smallvec![arr_v0, arr_v0], arr_v0);
        // `sort-by: ((T, T) -> Ordering, Array[T]) -> Array[T]`
        let cmp_cb = a.func(smallvec![v0, v0], TyArena::ORDERING);
        let sort_by_ty = a.func(smallvec![cmp_cb, arr_v0], arr_v0);
        // `zip: (Array[T], Array[U]) -> Array[(T, U)]`
        let pair_tu = a.alloc(Ty::Tuple(smallvec![v0, v1]));
        let arr_pair = a.array(pair_tu);
        let zip_ty = a.func(smallvec![arr_v0, arr_v1], arr_pair);
        // `zip-with: ((T, U) -> V, Array[T], Array[U]) -> Array[V]`
        let arr_v2 = a.array(v2);
        let zip_cb = a.func(smallvec![v0, v1], v2);
        let zip_with_ty = a.func(smallvec![zip_cb, arr_v0, arr_v1], arr_v2);
        // `unzip: (Array[(T, U)]) -> (Array[T], Array[U])`
        let arr_pair_in = a.array(pair_tu);
        let tup_out = a.alloc(Ty::Tuple(smallvec![arr_v0, arr_v1]));
        let unzip_ty = a.func(smallvec![arr_pair_in], tup_out);
        // `intersperse: (T, Array[T]) -> Array[T]`
        let intersperse_ty = a.func(smallvec![v0, arr_v0], arr_v0);

        self.modules.insert(
            "Array".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "push",
                    f: Array::push,
                    ty: poly1(push_ty),
                },
                PrimDef {
                    name: "pop",
                    f: Array::pop,
                    ty: poly1(pop_ty),
                },
                PrimDef {
                    name: "head",
                    f: Array::head,
                    ty: poly1(head_ty),
                },
                PrimDef {
                    name: "tail",
                    f: Array::tail,
                    ty: poly1(tail_ty),
                },
                PrimDef {
                    name: "sort",
                    f: Array::sort,
                    ty: poly1(sort_ty),
                },
                PrimDef {
                    name: "slice",
                    f: Array::slice,
                    ty: poly1(slice_ty),
                },
                PrimDef {
                    name: "concat",
                    f: Array::concat,
                    ty: poly1(concat_ty),
                },
                PrimDef {
                    name: "sort-by",
                    f: Array::placeholder,
                    ty: poly1(sort_by_ty),
                },
                PrimDef {
                    name: "zip",
                    f: Array::zip,
                    ty: poly2(zip_ty),
                },
                PrimDef {
                    name: "zip-with",
                    f: Array::placeholder,
                    ty: poly3(zip_with_ty),
                },
                PrimDef {
                    name: "unzip",
                    f: Array::unzip,
                    ty: poly2(unzip_ty),
                },
                PrimDef {
                    name: "intersperse",
                    f: Array::intersperse,
                    ty: poly1(intersperse_ty),
                },
            ]),
        );

        // --- String module ---
        let a = &mut self.ty_arena;
        let str_to_int = a.func(smallvec![TyArena::STRING], TyArena::INT);
        let str_to_str = a.func(smallvec![TyArena::STRING], TyArena::STRING);
        let str2_to_bool =
            a.func(smallvec![TyArena::STRING, TyArena::STRING], TyArena::BOOL);
        let str2_to_arr = {
            let arr_s = a.array(TyArena::STRING);
            a.func(smallvec![TyArena::STRING, TyArena::STRING], arr_s)
        };
        let join_ty = {
            let arr_s = a.array(TyArena::STRING);
            a.func(smallvec![arr_s, TyArena::STRING], TyArena::STRING)
        };
        let str_slice_ty = a.func(
            smallvec![TyArena::STRING, TyArena::INT, TyArena::INT],
            TyArena::STRING,
        );
        let str_replace_ty = a.func(
            smallvec![TyArena::STRING, TyArena::STRING, TyArena::STRING],
            TyArena::STRING,
        );

        self.modules.insert(
            "String".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "length",
                    f: Str::length,
                    ty: Scheme::mono(str_to_int),
                },
                PrimDef {
                    name: "upper",
                    f: Str::upper,
                    ty: Scheme::mono(str_to_str),
                },
                PrimDef {
                    name: "lower",
                    f: Str::lower,
                    ty: Scheme::mono(str_to_str),
                },
                PrimDef {
                    name: "trim",
                    f: Str::trim,
                    ty: Scheme::mono(str_to_str),
                },
                PrimDef {
                    name: "split",
                    f: Str::split,
                    ty: Scheme::mono(str2_to_arr),
                },
                PrimDef {
                    name: "join",
                    f: Str::join,
                    ty: Scheme::mono(join_ty),
                },
                PrimDef {
                    name: "slice",
                    f: Str::slice,
                    ty: Scheme::mono(str_slice_ty),
                },
                PrimDef {
                    name: "contains",
                    f: Str::contains,
                    ty: Scheme::mono(str2_to_bool),
                },
                PrimDef {
                    name: "replace",
                    f: Str::replace,
                    ty: Scheme::mono(str_replace_ty),
                },
                PrimDef {
                    name: "escape",
                    f: Str::escape,
                    ty: Scheme::mono(str_to_str),
                },
            ]),
        );

        // --- Math module ---
        let a = &mut self.ty_arena;
        let v0 = a.var(0);

        // `forall T: Numeric. (T) -> T`
        let num_unary = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0)],
            ty,
            constraints: smallvec![(
                TyVar::new(0),
                BuiltinClass::Simple(BuiltinClassTag::Numeric)
            )],
        };
        // `forall T: Numeric. (T, T) -> T`
        let num_binary = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0)],
            ty,
            constraints: smallvec![(
                TyVar::new(0),
                BuiltinClass::Simple(BuiltinClassTag::Numeric)
            )],
        };

        let abs_ty = a.func(smallvec![v0], v0);
        let minmax_ty = a.func(smallvec![v0, v0], v0);
        let float_to_int = a.func(smallvec![TyArena::FLOAT], TyArena::INT);
        let float_to_float = a.func(smallvec![TyArena::FLOAT], TyArena::FLOAT);

        let mut math_module = Module::from_prims(&[
            PrimDef {
                name: "abs",
                f: Math::abs,
                ty: num_unary(abs_ty),
            },
            PrimDef {
                name: "min",
                f: Math::min,
                ty: num_binary(minmax_ty),
            },
            PrimDef {
                name: "max",
                f: Math::max,
                ty: num_binary(minmax_ty),
            },
            PrimDef {
                name: "floor",
                f: Math::floor,
                ty: Scheme::mono(float_to_int),
            },
            PrimDef {
                name: "ceil",
                f: Math::ceil,
                ty: Scheme::mono(float_to_int),
            },
            PrimDef {
                name: "round",
                f: Math::round,
                ty: Scheme::mono(float_to_int),
            },
            PrimDef {
                name: "sqrt",
                f: Math::sqrt,
                ty: Scheme::mono(float_to_float),
            },
            PrimDef {
                name: "log",
                f: Math::log,
                ty: Scheme::mono(float_to_float),
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
            math_module.add_const(name, id, TyArena::FLOAT);
        });

        // Trig submodule
        let a = &mut self.ty_arena;
        let float_to_float = a.func(smallvec![TyArena::FLOAT], TyArena::FLOAT);
        let float2_to_float =
            a.func(smallvec![TyArena::FLOAT, TyArena::FLOAT], TyArena::FLOAT);

        self.modules.insert(
            "Math".to_string(),
            math_module.with_submodule(
                "Trig",
                Module::from_prims(&[
                    PrimDef {
                        name: "sin",
                        f: Trig::sin,
                        ty: Scheme::mono(float_to_float),
                    },
                    PrimDef {
                        name: "cos",
                        f: Trig::cos,
                        ty: Scheme::mono(float_to_float),
                    },
                    PrimDef {
                        name: "tan",
                        f: Trig::tan,
                        ty: Scheme::mono(float_to_float),
                    },
                    PrimDef {
                        name: "asin",
                        f: Trig::asin,
                        ty: Scheme::mono(float_to_float),
                    },
                    PrimDef {
                        name: "acos",
                        f: Trig::acos,
                        ty: Scheme::mono(float_to_float),
                    },
                    PrimDef {
                        name: "atan",
                        f: Trig::atan,
                        ty: Scheme::mono(float_to_float),
                    },
                    PrimDef {
                        name: "atan2",
                        f: Trig::atan2,
                        ty: Scheme::mono(float2_to_float),
                    },
                ]),
            ),
        );

        // --- Random module ---
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let arr_v0 = a.array(v0);
        let opt_v0 = a.option(v0);

        let thunk_float = a.func(smallvec![], TyArena::FLOAT);
        let float2_to_float =
            a.func(smallvec![TyArena::FLOAT, TyArena::FLOAT], TyArena::FLOAT);
        let int2_to_int =
            a.func(smallvec![TyArena::INT, TyArena::INT], TyArena::INT);
        let thunk_bool = a.func(smallvec![], TyArena::BOOL);
        let choice_ty = a.func(smallvec![arr_v0], opt_v0);
        let shuffle_ty = a.func(smallvec![arr_v0], arr_v0);
        let sample_ret = {
            let r = a.result(arr_v0, TyArena::STRING);
            a.func(smallvec![arr_v0, TyArena::INT], r)
        };
        let thunk_str = a.func(smallvec![], TyArena::STRING);

        // Reuse poly1 closure (it was moved; redefine)
        let poly1 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0)],
            ty,
            constraints: smallvec![],
        };
        let poly2 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };

        self.modules.insert(
            "Random".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "random",
                    f: Random::random,
                    ty: Scheme::mono(thunk_float),
                },
                PrimDef {
                    name: "range",
                    f: Random::range,
                    ty: Scheme::mono(float2_to_float),
                },
                PrimDef {
                    name: "int",
                    f: Random::int,
                    ty: Scheme::mono(int2_to_int),
                },
                PrimDef {
                    name: "bool",
                    f: Random::bool,
                    ty: Scheme::mono(thunk_bool),
                },
                PrimDef {
                    name: "choice",
                    f: Random::choice,
                    ty: poly1(choice_ty),
                },
                PrimDef {
                    name: "shuffle",
                    f: Random::shuffle,
                    ty: poly1(shuffle_ty),
                },
                PrimDef {
                    name: "sample",
                    f: Random::sample,
                    ty: poly1(sample_ret),
                },
                PrimDef {
                    name: "uuid",
                    f: Random::uuid,
                    ty: Scheme::mono(thunk_str),
                },
            ]),
        );

        // --- Map module ---
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let v1 = a.var(1);
        let map_kv = a.map_ty(v0, v1);
        let arr_v0 = a.array(v0);
        let arr_v1 = a.array(v1);
        let opt_v1 = a.option(v1);

        let map_empty_ty = a.func(smallvec![], map_kv);
        let map_length_ty = a.func(smallvec![map_kv], TyArena::INT);
        let map_keys_ty = a.func(smallvec![map_kv], arr_v0);
        let map_values_ty = a.func(smallvec![map_kv], arr_v1);
        let pair_kv = a.alloc(Ty::Tuple(smallvec![v0, v1]));
        let arr_pair_kv = a.array(pair_kv);
        let map_entries_ty = a.func(smallvec![map_kv], arr_pair_kv);
        let map_has_ty = a.func(smallvec![map_kv, v0], TyArena::BOOL);
        let map_lookup_ty = a.func(smallvec![map_kv, v0], opt_v1);
        let map_insert_ty = a.func(smallvec![map_kv, v0, v1], map_kv);
        let map_remove_ty = a.func(smallvec![map_kv, v0], map_kv);
        let map_merge_ty = a.func(smallvec![map_kv, map_kv], map_kv);
        let map_from_entries_ty = a.func(smallvec![arr_pair_kv], map_kv);

        self.modules.insert(
            "Map".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "empty",
                    f: Map::empty,
                    ty: poly2(map_empty_ty),
                },
                PrimDef {
                    name: "length",
                    f: Map::length,
                    ty: poly2(map_length_ty),
                },
                PrimDef {
                    name: "keys",
                    f: Map::keys,
                    ty: poly2(map_keys_ty),
                },
                PrimDef {
                    name: "values",
                    f: Map::values,
                    ty: poly2(map_values_ty),
                },
                PrimDef {
                    name: "entries",
                    f: Map::entries,
                    ty: poly2(map_entries_ty),
                },
                PrimDef {
                    name: "has",
                    f: Map::has,
                    ty: poly2(map_has_ty),
                },
                PrimDef {
                    name: "lookup",
                    f: Map::get,
                    ty: poly2(map_lookup_ty),
                },
                PrimDef {
                    name: "insert",
                    f: Map::set,
                    ty: poly2(map_insert_ty),
                },
                PrimDef {
                    name: "remove",
                    f: Map::remove,
                    ty: poly2(map_remove_ty),
                },
                PrimDef {
                    name: "merge",
                    f: Map::merge,
                    ty: poly2(map_merge_ty),
                },
                PrimDef {
                    name: "from-entries",
                    f: Map::from_entries,
                    ty: poly2(map_from_entries_ty),
                },
            ]),
        );

        // --- Time module ---
        let a = &mut self.ty_arena;
        let thunk_time = a.func(smallvec![], TyArena::TIME);
        let time_parse_ret = a.result(TyArena::TIME, TyArena::STRING);
        let time_parse_ty =
            a.func(smallvec![TyArena::STRING, TyArena::STRING], time_parse_ret);
        let time_format_ty =
            a.func(smallvec![TyArena::STRING, TyArena::TIME], TyArena::STRING);
        let time_add_ty =
            a.func(smallvec![TyArena::TIME, TyArena::INT], TyArena::TIME);
        let time_diff_ty =
            a.func(smallvec![TyArena::TIME, TyArena::TIME], TyArena::FLOAT);
        let time_to_int = a.func(smallvec![TyArena::TIME], TyArena::INT);
        let int_to_unit = a.func(smallvec![TyArena::INT], TyArena::UNIT);

        self.modules.insert(
            "Time".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "now",
                    f: Time::now,
                    ty: Scheme::mono(thunk_time),
                },
                PrimDef {
                    name: "epoch",
                    f: Time::epoch,
                    ty: Scheme::mono(thunk_time),
                },
                PrimDef {
                    name: "parse",
                    f: Time::parse,
                    ty: Scheme::mono(time_parse_ty),
                },
                PrimDef {
                    name: "format",
                    f: Time::format,
                    ty: Scheme::mono(time_format_ty),
                },
                PrimDef {
                    name: "add-seconds",
                    f: Time::add_seconds,
                    ty: Scheme::mono(time_add_ty),
                },
                PrimDef {
                    name: "diff-seconds",
                    f: Time::diff_seconds,
                    ty: Scheme::mono(time_diff_ty),
                },
                PrimDef {
                    name: "year",
                    f: Time::year,
                    ty: Scheme::mono(time_to_int),
                },
                PrimDef {
                    name: "month",
                    f: Time::month,
                    ty: Scheme::mono(time_to_int),
                },
                PrimDef {
                    name: "day",
                    f: Time::day,
                    ty: Scheme::mono(time_to_int),
                },
                PrimDef {
                    name: "hour",
                    f: Time::hour,
                    ty: Scheme::mono(time_to_int),
                },
                PrimDef {
                    name: "minute",
                    f: Time::minute,
                    ty: Scheme::mono(time_to_int),
                },
                PrimDef {
                    name: "second",
                    f: Time::second,
                    ty: Scheme::mono(time_to_int),
                },
                PrimDef {
                    name: "sleep",
                    f: Time::sleep,
                    ty: Scheme::mono(int_to_unit),
                },
            ]),
        );

        // --- Option module ---
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let v1 = a.var(1);
        let opt_v0 = a.option(v0);
        let opt_v1 = a.option(v1);

        // `map: (Option[T], (T) -> U) -> Option[U]`
        let opt_map_cb = a.func(smallvec![v0], v1);
        let opt_map_ty = a.func(smallvec![opt_v0, opt_map_cb], opt_v1);
        // `unwrap-or: (Option[T], T) -> T`
        let opt_unwrap_or_ty = a.func(smallvec![opt_v0, v0], v0);
        // `flatten: (Option[Option[T]]) -> Option[T]`
        let opt_opt_v0 = a.option(opt_v0);
        let opt_flatten_ty = a.func(smallvec![opt_opt_v0], opt_v0);
        // `note: (U, Option[T]) -> Result[T, U]`
        let res_tu = a.result(v0, v1);
        let opt_note_ty = a.func(smallvec![v1, opt_v0], res_tu);

        let poly1 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0)],
            ty,
            constraints: smallvec![],
        };
        let poly2 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };

        self.modules.insert(
            "Option".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "map",
                    f: Opt::placeholder,
                    ty: poly2(opt_map_ty),
                },
                PrimDef {
                    name: "unwrap-or",
                    f: Opt::unwrap_or,
                    ty: poly1(opt_unwrap_or_ty),
                },
                PrimDef {
                    name: "flatten",
                    f: Opt::flatten,
                    ty: poly1(opt_flatten_ty),
                },
                PrimDef {
                    name: "note",
                    f: Opt::note,
                    ty: poly2(opt_note_ty),
                },
            ]),
        );

        // --- Result module ---
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let v1 = a.var(1);
        let v2 = a.var(2);
        let opt_v0 = a.option(v0);

        // `map: (Result[T, V], (T) -> U) -> Result[U, V]`
        let res_tv = a.result(v0, v2);
        let res_uv = a.result(v1, v2);
        let res_map_cb = a.func(smallvec![v0], v1);
        let res_map_ty = a.func(smallvec![res_tv, res_map_cb], res_uv);
        // `map-err: (Result[T, U], (U) -> V) -> Result[T, V]`
        let res_tu = a.result(v0, v1);
        let res_tw = a.result(v0, v2);
        let res_map_err_cb = a.func(smallvec![v1], v2);
        let res_map_err_ty = a.func(smallvec![res_tu, res_map_err_cb], res_tw);
        // `unwrap-or: (Result[T, U], T) -> T`
        let res_unwrap_or_ty = a.func(smallvec![res_tu, v0], v0);
        // `flatten: (Result[Result[T, U], U]) -> Result[T, U]`
        let inner_res = a.result(v0, v1);
        let outer_res = a.result(inner_res, v1);
        let res_flatten_ty = a.func(smallvec![outer_res], inner_res);
        // `hush: (Result[T, U]) -> Option[T]`
        let res_hush_ty = a.func(smallvec![res_tu], opt_v0);

        let poly2 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };
        let poly3 = |ty: TyId| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
            ty,
            constraints: smallvec![],
        };

        self.modules.insert(
            "Result".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "map",
                    f: Res::placeholder,
                    ty: poly3(res_map_ty),
                },
                PrimDef {
                    name: "map-err",
                    f: Res::placeholder,
                    ty: poly3(res_map_err_ty),
                },
                PrimDef {
                    name: "unwrap-or",
                    f: Res::unwrap_or,
                    ty: poly2(res_unwrap_or_ty),
                },
                PrimDef {
                    name: "flatten",
                    f: Res::flatten,
                    ty: poly2(res_flatten_ty),
                },
                PrimDef {
                    name: "hush",
                    f: Res::hush,
                    ty: poly2(res_hush_ty),
                },
            ]),
        );

        // Build Directory submodule first to avoid double mutable borrow
        let directory_module = self.build_directory_module();

        // --- Io module ---
        let a = &mut self.ty_arena;
        let thunk_str = a.func(smallvec![], TyArena::STRING);
        let str_to_unit = a.func(smallvec![TyArena::STRING], TyArena::UNIT);

        self.modules.insert(
            "Io".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "get-line",
                    f: Io::get_line,
                    ty: Scheme::mono(thunk_str),
                },
                PrimDef {
                    name: "print",
                    f: Io::print,
                    ty: Scheme::mono(str_to_unit),
                },
                PrimDef {
                    name: "println",
                    f: Io::println,
                    ty: Scheme::mono(str_to_unit),
                },
                PrimDef {
                    name: "eprint",
                    f: Io::eprint,
                    ty: Scheme::mono(str_to_unit),
                },
                PrimDef {
                    name: "eprintln",
                    f: Io::eprintln,
                    ty: Scheme::mono(str_to_unit),
                },
            ])
            .with_submodule("Directory", directory_module),
        );
    }

    /// Build the `Io.Directory` submodule.
    fn build_directory_module(&mut self) -> Module {
        use crate::primitives::Directory;

        let a = &mut self.ty_arena;
        let consts = &mut self.consts;

        // Common types for this module
        let fp_to_bool = a.func(smallvec![TyArena::FILEPATH], TyArena::BOOL);
        let fp_to_unit = a.func(smallvec![TyArena::FILEPATH], TyArena::UNIT);
        let list_dir_ret = a.array(TyArena::PATH);
        let list_dir_ty = a.func(smallvec![TyArena::FILEPATH], list_dir_ret);
        let read_file_ty =
            a.func(smallvec![TyArena::FILEPATH], TyArena::STRING);
        let pwd_ty = a.func(smallvec![], TyArena::FILEPATH);
        let get_env_ret = a.option(TyArena::STRING);
        let get_env_ty = a.func(smallvec![TyArena::STRING], get_env_ret);
        let canonicalize_ty =
            a.func(smallvec![TyArena::FILEPATH], TyArena::FILEPATH);
        let parent_ret = a.option(TyArena::FILEPATH);
        let parent_ty = a.func(smallvec![TyArena::FILEPATH], parent_ret);
        let file_name_ret = a.option(TyArena::STRING);
        let file_name_ty = a.func(smallvec![TyArena::FILEPATH], file_name_ret);
        let extension_ret = a.option(TyArena::STRING);
        let extension_ty = a.func(smallvec![TyArena::FILEPATH], extension_ret);
        let arr_str = a.array(TyArena::STRING);
        let join_ty =
            a.func(smallvec![TyArena::FILEPATH, arr_str], TyArena::FILEPATH);
        let temp_dir_ty = a.func(smallvec![], TyArena::FILEPATH);
        let with_ext_ty = a.func(
            smallvec![TyArena::FILEPATH, TyArena::STRING],
            TyArena::FILEPATH,
        );

        // Object types for path-pair operations
        let src_sid = consts.intern("src");
        let dest_sid = consts.intern("dest");
        let path_pair_obj = a.alloc(Ty::Object(indexmap::indexmap! {
            src_sid => TyArena::FILEPATH,
            dest_sid => TyArena::FILEPATH,
        }));
        let move_ty = a.func(smallvec![path_pair_obj], TyArena::UNIT);
        let copy_ty = a.func(smallvec![path_pair_obj], TyArena::UNIT);

        // Object types for file write operations
        let path_sid = consts.intern("path");
        let contents_sid = consts.intern("contents");
        let file_obj = a.alloc(Ty::Object(indexmap::indexmap! {
            path_sid => TyArena::FILEPATH,
            contents_sid => TyArena::STRING,
        }));
        let write_ty = a.func(smallvec![file_obj], TyArena::UNIT);
        let append_ty = a.func(smallvec![file_obj], TyArena::UNIT);

        // Object type for set-env
        let name_sid = consts.intern("name");
        let value_sid = consts.intern("value");
        let env_obj = a.alloc(Ty::Object(indexmap::indexmap! {
            name_sid => TyArena::STRING,
            value_sid => TyArena::STRING,
        }));
        let set_env_ty = a.func(smallvec![env_obj], TyArena::UNIT);

        Module::from_prims(&[
            PrimDef {
                name: "list-dir",
                f: Directory::list_dir,
                ty: Scheme::mono(list_dir_ty),
            },
            PrimDef {
                name: "exists",
                f: Directory::exists,
                ty: Scheme::mono(fp_to_bool),
            },
            PrimDef {
                name: "is-file",
                f: Directory::is_file,
                ty: Scheme::mono(fp_to_bool),
            },
            PrimDef {
                name: "is-dir",
                f: Directory::is_dir,
                ty: Scheme::mono(fp_to_bool),
            },
            PrimDef {
                name: "read-file",
                f: Directory::read_file,
                ty: Scheme::mono(read_file_ty),
            },
            PrimDef {
                name: "remove",
                f: Directory::remove,
                ty: Scheme::mono(fp_to_unit),
            },
            PrimDef {
                name: "remove-all",
                f: Directory::remove_all,
                ty: Scheme::mono(fp_to_unit),
            },
            PrimDef {
                name: "create-dir",
                f: Directory::create_dir,
                ty: Scheme::mono(fp_to_unit),
            },
            PrimDef {
                name: "create-dir-all",
                f: Directory::create_dir_all,
                ty: Scheme::mono(fp_to_unit),
            },
            PrimDef {
                name: "pwd",
                f: Directory::pwd,
                ty: Scheme::mono(pwd_ty),
            },
            PrimDef {
                name: "set-pwd",
                f: Directory::set_pwd,
                ty: Scheme::mono(fp_to_unit),
            },
            PrimDef {
                name: "get-env",
                f: Directory::get_env,
                ty: Scheme::mono(get_env_ty),
            },
            PrimDef {
                name: "move-path",
                f: Directory::move_path,
                ty: Scheme::mono(move_ty),
            },
            PrimDef {
                name: "copy-path",
                f: Directory::copy_path,
                ty: Scheme::mono(copy_ty),
            },
            PrimDef {
                name: "write-file",
                f: Directory::write_file,
                ty: Scheme::mono(write_ty),
            },
            PrimDef {
                name: "append-file",
                f: Directory::append_file,
                ty: Scheme::mono(append_ty),
            },
            PrimDef {
                name: "set-env",
                f: Directory::set_env,
                ty: Scheme::mono(set_env_ty),
            },
            PrimDef {
                name: "canonicalize",
                f: Directory::canonicalize,
                ty: Scheme::mono(canonicalize_ty),
            },
            PrimDef {
                name: "parent",
                f: Directory::parent,
                ty: Scheme::mono(parent_ty),
            },
            PrimDef {
                name: "file-name",
                f: Directory::file_name,
                ty: Scheme::mono(file_name_ty),
            },
            PrimDef {
                name: "extension",
                f: Directory::extension,
                ty: Scheme::mono(extension_ty),
            },
            PrimDef {
                name: "join",
                f: Directory::join,
                ty: Scheme::mono(join_ty),
            },
            PrimDef {
                name: "temp-dir",
                f: Directory::temp_dir,
                ty: Scheme::mono(temp_dir_ty),
            },
            PrimDef {
                name: "with-extension",
                f: Directory::with_extension,
                ty: Scheme::mono(with_ext_ty),
            },
        ])
    }
}
