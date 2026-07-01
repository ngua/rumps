//! Variable environments for lexical scope and builtin functions.
//!
//! The `Environment` tracks lexical scope for `let` bindings and callable names.
//! `set` variables (both local and global) go through the `Database`, not here.

#![allow(dead_code)]

mod builtins;
mod modules;
mod scopes;

use std::collections::HashMap;

pub(crate) use modules::{Module, UserModule};
use rumps_query_macros::scheme;
pub(crate) use scopes::Scopes;

use crate::ast::{BinOp, Intrinsic, PostfixOp, UnOp};
use crate::builtins as runtime_builtins;
use crate::intern::{StringId, StringInterner};
use crate::typecheck::{Scheme, TyArena, TyId};
use crate::value::{FunctionDef, ValueArena, ValueId};

/// Names of built-in modules.
///
/// This is the single source of truth for which module names are recognized
/// during resolution and registered at interpreter startup.
pub(crate) const BUILTIN_MODULE_NAMES: &[&str] = &[
    "Array", "String", "Math", "Random", "Map", "Time", "Option", "Result",
    "Io", "Prelude", "Range",
];

/// Name of the module that is automatically imported into every scope.
pub(crate) const PRELUDE_MODULE: &str = "Prelude";

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
/// Similar to `builtins::Def` but for DB intrinsics which have special syntax
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
        match self {
            Self::Get => IntrinsicDef {
                name: "@get",
                ty: scheme!(a, (Ref) -> Option[Storable]),
                txn: TxnReq::None,
            },
            Self::Set => IntrinsicDef {
                name: "@set",
                ty: scheme!(a, (Ref, Storable) -> Result[Unit, String]),
                txn: TxnReq::Globals,
            },
            Self::Kill => IntrinsicDef {
                name: "@kill",
                ty: scheme!(a, (Ref) -> Result[Unit, String]),
                txn: TxnReq::Globals,
            },
            Self::Data => IntrinsicDef {
                name: "@data",
                ty: scheme!(a, (Ref) -> DataStatus),
                txn: TxnReq::None,
            },
            Self::Order => IntrinsicDef {
                name: "@order",
                ty: scheme!(a, (Ref) -> Option[Subscript]),
                txn: TxnReq::None,
            },
            Self::Query => IntrinsicDef {
                name: "@query",
                ty: scheme!(a, (Ref) -> Option[Array[Subscript]]),
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
    pub(crate) fn def(self, a: &mut TyArena) -> BinOpDef {
        match self {
            // Arithmetic: capability-specific constraints.
            Self::Add => BinOpDef {
                name: "+",
                ty: scheme!(a, forall T: Additive. (T, T) -> T),
            },
            Self::Sub => BinOpDef {
                name: "-",
                ty: scheme!(a, forall T: Subtractive. (T, T) -> T),
            },
            Self::Mul => BinOpDef {
                name: "*",
                ty: scheme!(a, forall T: Multiplicative. (T, T) -> T),
            },
            Self::Div => BinOpDef {
                name: "/",
                ty: scheme!(a, forall T: Divisible. (T, T) -> T),
            },
            Self::FloorDiv => BinOpDef {
                name: "//",
                ty: scheme!(a, forall T: FloorDivisible. (T, T) -> T),
            },
            Self::Mod => BinOpDef {
                name: "%",
                ty: scheme!(a, forall T: FloorDivisible. (T, T) -> T),
            },
            Self::Pow => BinOpDef {
                name: "**",
                ty: scheme!(a, forall T: Powerable. (T, T) -> T),
            },

            // Comparison: `forall T: Eq. (T, T) -> Bool`
            Self::Eq => BinOpDef {
                name: "==",
                ty: scheme!(a, forall T: Eq. (T, T) -> Bool),
            },
            Self::Ne => BinOpDef {
                name: "!=",
                ty: scheme!(a, forall T: Eq. (T, T) -> Bool),
            },
            Self::Lt => BinOpDef {
                name: "<",
                ty: scheme!(a, forall T: Ord. (T, T) -> Bool),
            },
            Self::Gt => BinOpDef {
                name: ">",
                ty: scheme!(a, forall T: Ord. (T, T) -> Bool),
            },
            Self::Le => BinOpDef {
                name: "<=",
                ty: scheme!(a, forall T: Ord. (T, T) -> Bool),
            },
            Self::Ge => BinOpDef {
                name: ">=",
                ty: scheme!(a, forall T: Ord. (T, T) -> Bool),
            },

            // Logical: `(Bool, Bool) -> Bool`
            Self::And => BinOpDef {
                name: "and",
                ty: scheme!(a, (Bool, Bool) -> Bool),
            },
            Self::Or => BinOpDef {
                name: "or",
                ty: scheme!(a, (Bool, Bool) -> Bool),
            },

            // Bitwise: `forall T: BitLike. (T, T) -> T`
            Self::BitAnd => BinOpDef {
                name: "&",
                ty: scheme!(a, forall T: BitLike. (T, T) -> T),
            },
            Self::BitOr => BinOpDef {
                name: "|",
                ty: scheme!(a, forall T: BitLike. (T, T) -> T),
            },
            Self::Shl => BinOpDef {
                name: "<<",
                ty: scheme!(a, forall T: BitLike. (T, T) -> T),
            },
            Self::Shr => BinOpDef {
                name: ">>",
                ty: scheme!(a, forall T: BitLike. (T, T) -> T),
            },

            // Concat: `forall T: Concatable. (T, T) -> T`
            Self::Concat => BinOpDef {
                name: "++",
                ty: scheme!(a, forall T: Concatable. (T, T) -> T),
            },

            // Coalesce: `forall C: Coalescable[T], T. (C, T) -> T`
            Self::Coalesce => BinOpDef {
                name: "??",
                ty: scheme!(a, forall C: Coalescable[T], T. (C, T) -> T),
            },

            // Pipe: `forall T, U. (T, (T) -> U) -> U`
            Self::Pipe => BinOpDef {
                name: "|>",
                ty: scheme!(a, forall T, U. (T, (T) -> U) -> U),
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
    pub(crate) fn def(self, a: &mut TyArena) -> UnOpDef {
        match self {
            Self::Neg => UnOpDef {
                name: "-",
                ty: scheme!(a, forall T: Negatable. (T) -> T),
            },
            Self::Not => UnOpDef {
                name: "not",
                ty: scheme!(a, (Bool) -> Bool),
            },
            Self::Wrap => UnOpDef {
                name: "?",
                ty: scheme!(a, forall T, F: Wrappable. (T) -> F[T]),
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
    pub(crate) fn def(self, a: &mut TyArena) -> PostfixOpDef {
        match self {
            Self::Unwrap => PostfixOpDef {
                name: "!",
                ty: scheme!(a, forall T, F: Fallible. (F[T]) -> T),
            },
        }
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
    modules: HashMap<StringId, Module>,

    /// User-defined modules.
    user_modules: HashMap<StringId, UserModule>,

    /// Arena backing module constant `ValueId`s.
    pub(crate) consts: ValueArena,

    /// Type arena for builtin scheme types.
    pub(crate) ty_arena: TyArena,
}

impl Environment {
    /// Create a new environment seeded with an existing interner.
    ///
    /// Module name `StringId`s will be compatible with the caller's interner,
    /// so lookups using AST `StringId`s will match the registered module keys.
    pub(crate) fn with_interner(interner: StringInterner) -> Self {
        let mut env = Self {
            scopes: Scopes::new(),
            modules: HashMap::new(),
            user_modules: HashMap::new(),
            consts: ValueArena::with_interner(interner),
            ty_arena: TyArena::new(),
        };
        env.register_builtins();
        env
    }

    /// Check if a top-level module exists (builtin or user-defined).
    pub(crate) fn has_module(&self, name: StringId) -> bool {
        self.modules.contains_key(&name)
            || self.user_modules.contains_key(&name)
    }

    /// Register a user-defined module.
    pub(crate) fn register_user_module(
        &mut self,
        name: StringId,
        module: UserModule,
    ) {
        self.user_modules.insert(name, module);
    }

    /// Get a mutable reference to a user module, creating it if it doesn't exist.
    pub(crate) fn get_or_create_user_module(
        &mut self,
        name: StringId,
    ) -> &mut UserModule {
        self.user_modules.entry(name).or_default()
    }

    /// Look up a user module by path.
    ///
    /// The path is the module path segments (e.g., `["Counter"]` or
    /// `["Counter", "Inner"]` for nested modules).
    pub(crate) fn get_user_module(
        &self,
        path: &[StringId],
    ) -> Option<&UserModule> {
        path.split_first().and_then(|(first, rest)| {
            self.user_modules.get(first).and_then(|m| {
                rest.iter().try_fold(m, |acc, seg| acc.submodules.get(seg))
            })
        })
    }

    /// Look up a user module function by path.
    pub(crate) fn get_user_module_fn(
        &self,
        path: &[StringId],
    ) -> Option<&FunctionDef> {
        path.split_first().and_then(|(module, rest)| {
            self.user_modules.get(module).and_then(|m| m.get_fn(rest))
        })
    }

    /// Look up a user module constant by path.
    pub(crate) fn get_user_module_const(
        &self,
        path: &[StringId],
    ) -> Option<ValueId> {
        path.split_first().and_then(|(module, rest)| {
            self.user_modules
                .get(module)
                .and_then(|m| m.get_const(rest))
        })
    }

    /// Check if a path resolves to a user module function.
    pub(crate) fn user_module_fn_exists(&self, path: &[StringId]) -> bool {
        self.get_user_module_fn(path).is_some()
    }

    /// Check if a path resolves to a user module constant.
    pub(crate) fn user_module_const_exists(&self, path: &[StringId]) -> bool {
        self.get_user_module_const(path).is_some()
    }

    /// Check if a path resolves to a module function.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    pub(crate) fn module_fn_exists(&self, path: &[StringId]) -> bool {
        path.split_first().is_some_and(|(module, rest)| {
            self.modules
                .get(module)
                .is_some_and(|m| m.contains_fn(rest))
        })
    }

    /// Check if a path resolves to a module constant.
    pub(crate) fn module_const_exists(&self, path: &[StringId]) -> bool {
        path.split_first().is_some_and(|(module, rest)| {
            self.modules
                .get(module)
                .is_some_and(|m| m.contains_const(rest))
        })
    }

    /// Check if a name is a builtin module.
    pub(crate) fn is_builtin_module(&self, name: StringId) -> bool {
        self.modules.contains_key(&name)
    }

    /// Get a builtin module by name.
    pub(crate) fn get_builtin_module(&self, name: StringId) -> Option<&Module> {
        self.modules.get(&name)
    }

    /// Get a builtin module or submodule by path.
    ///
    /// The path should be like `["Math"]` or `["Math", "Trig"]`.
    pub(crate) fn get_builtin_module_by_path(
        &self,
        path: &[StringId],
    ) -> Option<&Module> {
        path.split_first().and_then(|(first, rest)| {
            self.modules.get(first).and_then(|m| m.get_submodule(rest))
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
    pub(crate) fn get_module_fn(
        &self,
        path: &[StringId],
    ) -> Option<&runtime_builtins::Impl> {
        path.split_first().and_then(|(module, rest)| {
            self.modules.get(module).and_then(|m| m.get_fn(rest))
        })
    }

    /// Look up a function's type scheme by its full path.
    ///
    /// The path must have at least two segments: the first is the module name,
    /// and the remaining segments form the path within that module.
    pub(crate) fn get_module_fn_type(
        &self,
        path: &[StringId],
    ) -> Option<&Scheme> {
        path.split_first().and_then(|(module, rest)| {
            self.modules.get(module).and_then(|m| m.get_fn_type(rest))
        })
    }

    /// Look up a constant by its full path.
    ///
    /// Returns the `ValueId` indexing into `self.consts`.
    pub(crate) fn get_module_const(
        &self,
        path: &[StringId],
    ) -> Option<ValueId> {
        path.split_first().and_then(|(module, rest)| {
            self.modules.get(module).and_then(|m| m.get_const(rest))
        })
    }

    /// Look up a constant's type by its full path.
    pub(crate) fn get_module_const_type(
        &self,
        path: &[StringId],
    ) -> Option<TyId> {
        path.split_first().and_then(|(module, rest)| {
            self.modules
                .get(module)
                .and_then(|m| m.get_const_type(rest))
        })
    }

    pub(crate) fn builtin_module_fn_types(
        &self,
    ) -> HashMap<Vec<StringId>, Scheme> {
        let mut out = HashMap::new();
        self.modules.iter().for_each(|(&name, m)| {
            let mut path = vec![name];
            m.collect_fn_types(&mut path, &mut out);
        });
        out
    }

    pub(crate) fn builtin_module_const_types(
        &self,
    ) -> HashMap<Vec<StringId>, TyId> {
        let mut out = HashMap::new();
        self.modules.iter().for_each(|(&name, m)| {
            let mut path = vec![name];
            m.collect_const_types(&mut path, &mut out);
        });
        out
    }
}
