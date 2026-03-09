//! Variable environments for lexical scope and primitive functions.
//!
//! The `Environment` tracks lexical scope for `LET` bindings and callable names.
//! `SET` variables (both local and global) go through the `Database`, not here.

#![allow(dead_code)]

use std::collections::HashMap;

use crate::typecheck::{BuiltinClass, BuiltinClassTag, Scheme, Ty, TyVar};

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
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Named(TypeId::REF, vec![])],
                    Box::new(Ty::Option(Box::new(Ty::Named(
                        TypeId::STORABLE,
                        vec![],
                    )))),
                )),
                txn: TxnReq::None,
            },
            Self::Set => IntrinsicDef {
                name: "@SET",
                ty: Scheme::mono(Ty::Fn(
                    vec![
                        Ty::Named(TypeId::REF, vec![]),
                        Ty::Named(TypeId::STORABLE, vec![]),
                    ],
                    Box::new(Ty::Result(
                        Box::new(Ty::Unit),
                        Box::new(Ty::String),
                    )),
                )),
                txn: TxnReq::Globals,
            },
            Self::Kill => IntrinsicDef {
                name: "@KILL",
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Named(TypeId::REF, vec![])],
                    Box::new(Ty::Result(
                        Box::new(Ty::Unit),
                        Box::new(Ty::String),
                    )),
                )),
                txn: TxnReq::Globals,
            },
            Self::Data => IntrinsicDef {
                name: "@DATA",
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Named(TypeId::REF, vec![])],
                    Box::new(Ty::DataStatus),
                )),
                txn: TxnReq::None,
            },
            Self::Order => IntrinsicDef {
                name: "@ORDER",
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Named(TypeId::REF, vec![])],
                    Box::new(Ty::Option(Box::new(Ty::Named(
                        TypeId::SUBSCRIPT,
                        vec![],
                    )))),
                )),
                txn: TxnReq::None,
            },
            Self::Query => IntrinsicDef {
                name: "@QUERY",
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Named(TypeId::REF, vec![])],
                    Box::new(Ty::Option(Box::new(Ty::Array(Box::new(
                        Ty::Named(TypeId::SUBSCRIPT, vec![]),
                    ))))),
                )),
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
            // Arithmetic: `forall T: Numeric. (T, T) -> T`
            Self::Add => BinOpDef {
                name: "+",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            Self::Sub => BinOpDef {
                name: "-",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            Self::Mul => BinOpDef {
                name: "*",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            Self::Div => BinOpDef {
                name: "/",
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Float, Ty::Float],
                    Box::new(Ty::Float),
                )),
            },
            Self::FloorDiv => BinOpDef {
                name: "//",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            Self::Mod => BinOpDef {
                name: "%",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            Self::Pow => BinOpDef {
                name: "**",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },

            // Comparison: `forall T: Eq. (T, T) -> Bool`
            Self::Eq => BinOpDef {
                name: "==",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Bool),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Eq)
                    )],
                },
            },
            Self::Ne => BinOpDef {
                name: "!=",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Bool),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Eq)
                    )],
                },
            },
            Self::Lt => BinOpDef {
                name: "<",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Bool),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Ord)
                    )],
                },
            },
            Self::Gt => BinOpDef {
                name: ">",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Bool),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Ord)
                    )],
                },
            },
            Self::Le => BinOpDef {
                name: "<=",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Bool),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Ord)
                    )],
                },
            },
            Self::Ge => BinOpDef {
                name: ">=",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Bool),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Ord)
                    )],
                },
            },

            // Logical: `(Bool, Bool) -> Bool`
            Self::And => BinOpDef {
                name: "AND",
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Bool, Ty::Bool],
                    Box::new(Ty::Bool),
                )),
            },
            Self::Or => BinOpDef {
                name: "OR",
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Bool, Ty::Bool],
                    Box::new(Ty::Bool),
                )),
            },

            // Bitwise: `forall T: BitLike. (T, T) -> T`
            Self::BitAnd => BinOpDef {
                name: "&",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::BitLike)
                    )],
                },
            },
            Self::BitOr => BinOpDef {
                name: "|",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::BitLike)
                    )],
                },
            },
            Self::Shl => BinOpDef {
                name: "<<",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::BitLike)
                    )],
                },
            },
            Self::Shr => BinOpDef {
                name: ">>",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::BitLike)
                    )],
                },
            },

            // Concat: `forall T: Monoid. (T, T) -> T`
            Self::Concat => BinOpDef {
                name: "++",
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Monoid)
                    )],
                },
            },

            // Coalesce: `forall T, F: Fallible. (F[T], T) -> T`
            Self::Coalesce => BinOpDef {
                name: "??",
                ty: Scheme {
                    vars: vec![TyVar::new(0), TyVar::new(1)],
                    ty: Ty::Fn(
                        vec![
                            Ty::Apply(
                                TyVar::new(1),
                                vec![Ty::Var(TyVar::new(0))],
                            ),
                            Ty::Var(TyVar::new(0)),
                        ],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(1),
                        BuiltinClass::Hkt(BuiltinClassTag::Fallible, None)
                    )],
                },
            },

            // Pipe: `forall T, U. (T, (T) -> U) -> U`
            Self::Pipe => BinOpDef {
                name: "|>",
                ty: Scheme {
                    vars: vec![TyVar::new(0), TyVar::new(1)],
                    ty: Ty::Fn(
                        vec![
                            Ty::Var(TyVar::new(0)),
                            Ty::Fn(
                                vec![Ty::Var(TyVar::new(0))],
                                Box::new(Ty::Var(TyVar::new(1))),
                            ),
                        ],
                        Box::new(Ty::Var(TyVar::new(1))),
                    ),
                    constraints: smallvec![],
                },
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
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Negatable)
                    )],
                },
            },
            Self::Not => UnOpDef {
                name: "NOT",
                ty: Scheme::mono(Ty::Fn(vec![Ty::Bool], Box::new(Ty::Bool))),
            },
            Self::Wrap => UnOpDef {
                name: "?",
                ty: Scheme {
                    vars: vec![TyVar::new(0), TyVar::new(1)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Apply(
                            TyVar::new(1),
                            vec![Ty::Var(TyVar::new(0))],
                        )),
                    ),
                    constraints: smallvec![(
                        TyVar::new(1),
                        BuiltinClass::Hkt(BuiltinClassTag::Fallible, None)
                    )],
                },
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
                ty: Scheme {
                    vars: vec![TyVar::new(0), TyVar::new(1)],
                    ty: Ty::Fn(
                        vec![Ty::Apply(
                            TyVar::new(1),
                            vec![Ty::Var(TyVar::new(0))],
                        )],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(1),
                        BuiltinClass::Hkt(BuiltinClassTag::Fallible, None)
                    )],
                },
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
            Array, Io, Map, Math, Opt, Prim, Random, Res, Str, Time, Trig,
        };

        self.modules.insert(
            "Array".to_string(),
            Module::from_prims(&[
                // Array-specific primitives (HOFs moved to Iter module)
                PrimDef {
                    name: "push",
                    f: Array::push,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Var(TyVar::new(0)),
                            ],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "pop",
                    f: Array::pop,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "head",
                    f: Array::head,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                            Box::new(Ty::Option(Box::new(Ty::Var(
                                TyVar::new(0),
                            )))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "tail",
                    f: Array::tail,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "sort",
                    f: Array::sort,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "slice",
                    f: Array::slice,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Int,
                                Ty::Int,
                            ],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "concat",
                    f: Array::concat,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                            ],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "sort-by",
                    f: Array::placeholder,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Fn(
                                    vec![
                                        Ty::Var(TyVar::new(0)),
                                        Ty::Var(TyVar::new(0)),
                                    ],
                                    Box::new(Ty::Ordering),
                                ),
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                            ],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "zip",
                    f: Array::zip,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Array(Box::new(Ty::Var(TyVar::new(1)))),
                            ],
                            Box::new(Ty::Array(Box::new(Ty::Tuple(vec![
                                Ty::Var(TyVar::new(0)),
                                Ty::Var(TyVar::new(1)),
                            ])))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "zip-with",
                    f: Array::placeholder,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Fn(
                                    vec![
                                        Ty::Var(TyVar::new(0)),
                                        Ty::Var(TyVar::new(1)),
                                    ],
                                    Box::new(Ty::Var(TyVar::new(2))),
                                ),
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Array(Box::new(Ty::Var(TyVar::new(1)))),
                            ],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                2,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "unzip",
                    f: Array::unzip,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Tuple(vec![
                                Ty::Var(TyVar::new(0)),
                                Ty::Var(TyVar::new(1)),
                            ])))],
                            Box::new(Ty::Tuple(vec![
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Array(Box::new(Ty::Var(TyVar::new(1)))),
                            ])),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "intersperse",
                    f: Array::intersperse,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Var(TyVar::new(0)),
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                            ],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
            ]),
        );

        self.modules.insert(
            "String".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "length",
                    f: Str::length,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::Int),
                    )),
                },
                PrimDef {
                    name: "upper",
                    f: Str::upper,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::String),
                    )),
                },
                PrimDef {
                    name: "lower",
                    f: Str::lower,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::String),
                    )),
                },
                PrimDef {
                    name: "trim",
                    f: Str::trim,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::String),
                    )),
                },
                PrimDef {
                    name: "split",
                    f: Str::split,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String, Ty::String],
                        Box::new(Ty::Array(Box::new(Ty::String))),
                    )),
                },
                PrimDef {
                    name: "join",
                    f: Str::join,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::Array(Box::new(Ty::String)), Ty::String],
                        Box::new(Ty::String),
                    )),
                },
                PrimDef {
                    name: "slice",
                    f: Str::slice,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String, Ty::Int, Ty::Int],
                        Box::new(Ty::String),
                    )),
                },
                PrimDef {
                    name: "contains",
                    f: Str::contains,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String, Ty::String],
                        Box::new(Ty::Bool),
                    )),
                },
                PrimDef {
                    name: "replace",
                    f: Str::replace,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String, Ty::String, Ty::String],
                        Box::new(Ty::String),
                    )),
                },
                PrimDef {
                    name: "escape",
                    f: Str::escape,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::String),
                    )),
                },
            ]),
        );

        // Math module
        let mut math_module = Module::from_prims(&[
            PrimDef {
                name: "abs",
                f: Math::abs,
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            PrimDef {
                name: "min",
                f: Math::min,
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            PrimDef {
                name: "max",
                f: Math::max,
                ty: Scheme {
                    vars: vec![TyVar::new(0)],
                    ty: Ty::Fn(
                        vec![Ty::Var(TyVar::new(0)), Ty::Var(TyVar::new(0))],
                        Box::new(Ty::Var(TyVar::new(0))),
                    ),
                    constraints: smallvec![(
                        TyVar::new(0),
                        BuiltinClass::Simple(BuiltinClassTag::Numeric)
                    )],
                },
            },
            PrimDef {
                name: "floor",
                f: Math::floor,
                ty: Scheme::mono(Ty::Fn(vec![Ty::Float], Box::new(Ty::Int))),
            },
            PrimDef {
                name: "ceil",
                f: Math::ceil,
                ty: Scheme::mono(Ty::Fn(vec![Ty::Float], Box::new(Ty::Int))),
            },
            PrimDef {
                name: "round",
                f: Math::round,
                ty: Scheme::mono(Ty::Fn(vec![Ty::Float], Box::new(Ty::Int))),
            },
            PrimDef {
                name: "sqrt",
                f: Math::sqrt,
                ty: Scheme::mono(Ty::Fn(vec![Ty::Float], Box::new(Ty::Float))),
            },
            PrimDef {
                name: "log",
                f: Math::log,
                ty: Scheme::mono(Ty::Fn(vec![Ty::Float], Box::new(Ty::Float))),
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
                        ty: Scheme::mono(Ty::Fn(
                            vec![Ty::Float],
                            Box::new(Ty::Float),
                        )),
                    },
                    PrimDef {
                        name: "cos",
                        f: Trig::cos,
                        ty: Scheme::mono(Ty::Fn(
                            vec![Ty::Float],
                            Box::new(Ty::Float),
                        )),
                    },
                    PrimDef {
                        name: "tan",
                        f: Trig::tan,
                        ty: Scheme::mono(Ty::Fn(
                            vec![Ty::Float],
                            Box::new(Ty::Float),
                        )),
                    },
                    PrimDef {
                        name: "asin",
                        f: Trig::asin,
                        ty: Scheme::mono(Ty::Fn(
                            vec![Ty::Float],
                            Box::new(Ty::Float),
                        )),
                    },
                    PrimDef {
                        name: "acos",
                        f: Trig::acos,
                        ty: Scheme::mono(Ty::Fn(
                            vec![Ty::Float],
                            Box::new(Ty::Float),
                        )),
                    },
                    PrimDef {
                        name: "atan",
                        f: Trig::atan,
                        ty: Scheme::mono(Ty::Fn(
                            vec![Ty::Float],
                            Box::new(Ty::Float),
                        )),
                    },
                    PrimDef {
                        name: "atan2",
                        f: Trig::atan2,
                        ty: Scheme::mono(Ty::Fn(
                            vec![Ty::Float, Ty::Float],
                            Box::new(Ty::Float),
                        )),
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
                    ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::Float))),
                },
                PrimDef {
                    name: "range",
                    f: Random::range,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::Float, Ty::Float],
                        Box::new(Ty::Float),
                    )),
                },
                PrimDef {
                    name: "int",
                    f: Random::int,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::Int, Ty::Int],
                        Box::new(Ty::Int),
                    )),
                },
                PrimDef {
                    name: "bool",
                    f: Random::bool,
                    ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::Bool))),
                },
                PrimDef {
                    name: "choice",
                    f: Random::choice,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                            Box::new(Ty::Option(Box::new(Ty::Var(
                                TyVar::new(0),
                            )))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "shuffle",
                    f: Random::shuffle,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "sample",
                    f: Random::sample,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Array(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Int,
                            ],
                            Box::new(Ty::Result(
                                Box::new(Ty::Array(Box::new(Ty::Var(
                                    TyVar::new(0),
                                )))),
                                Box::new(Ty::String),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "uuid",
                    f: Random::uuid,
                    ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::String))),
                },
            ]),
        );

        self.modules.insert(
            "Map".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "empty",
                    f: Map::empty,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![],
                            Box::new(Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "length",
                    f: Map::length,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )],
                            Box::new(Ty::Int),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "keys",
                    f: Map::keys,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                0,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "values",
                    f: Map::values,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )],
                            Box::new(Ty::Array(Box::new(Ty::Var(TyVar::new(
                                1,
                            ))))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "entries",
                    f: Map::entries,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )],
                            Box::new(Ty::Array(Box::new(Ty::Tuple(vec![
                                Ty::Var(TyVar::new(0)),
                                Ty::Var(TyVar::new(1)),
                            ])))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "has",
                    f: Map::has,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Map(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                                Ty::Var(TyVar::new(0)),
                            ],
                            Box::new(Ty::Bool),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "lookup",
                    f: Map::get,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Map(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                                Ty::Var(TyVar::new(0)),
                            ],
                            Box::new(Ty::Option(Box::new(Ty::Var(
                                TyVar::new(1),
                            )))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "insert",
                    f: Map::set,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Map(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                                Ty::Var(TyVar::new(0)),
                                Ty::Var(TyVar::new(1)),
                            ],
                            Box::new(Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "remove",
                    f: Map::remove,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Map(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                                Ty::Var(TyVar::new(0)),
                            ],
                            Box::new(Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "merge",
                    f: Map::merge,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Map(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                                Ty::Map(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                            ],
                            Box::new(Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "from-entries",
                    f: Map::from_entries,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Array(Box::new(Ty::Tuple(vec![
                                Ty::Var(TyVar::new(0)),
                                Ty::Var(TyVar::new(1)),
                            ])))],
                            Box::new(Ty::Map(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
            ]),
        );

        self.modules.insert(
            "Time".to_string(),
            Module::from_prims(&[
                PrimDef {
                    name: "now",
                    f: Time::now,
                    ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::Time))),
                },
                PrimDef {
                    name: "epoch",
                    f: Time::epoch,
                    ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::Time))),
                },
                PrimDef {
                    name: "parse",
                    f: Time::parse,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String, Ty::String],
                        Box::new(Ty::Result(
                            Box::new(Ty::Time),
                            Box::new(Ty::String),
                        )),
                    )),
                },
                PrimDef {
                    name: "format",
                    f: Time::format,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String, Ty::Time],
                        Box::new(Ty::String),
                    )),
                },
                PrimDef {
                    name: "add-seconds",
                    f: Time::add_seconds,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::Time, Ty::Int],
                        Box::new(Ty::Time),
                    )),
                },
                PrimDef {
                    name: "diff-seconds",
                    f: Time::diff_seconds,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::Time, Ty::Time],
                        Box::new(Ty::Float),
                    )),
                },
                PrimDef {
                    name: "year",
                    f: Time::year,
                    ty: Scheme::mono(Ty::Fn(vec![Ty::Time], Box::new(Ty::Int))),
                },
                PrimDef {
                    name: "month",
                    f: Time::month,
                    ty: Scheme::mono(Ty::Fn(vec![Ty::Time], Box::new(Ty::Int))),
                },
                PrimDef {
                    name: "day",
                    f: Time::day,
                    ty: Scheme::mono(Ty::Fn(vec![Ty::Time], Box::new(Ty::Int))),
                },
                PrimDef {
                    name: "hour",
                    f: Time::hour,
                    ty: Scheme::mono(Ty::Fn(vec![Ty::Time], Box::new(Ty::Int))),
                },
                PrimDef {
                    name: "minute",
                    f: Time::minute,
                    ty: Scheme::mono(Ty::Fn(vec![Ty::Time], Box::new(Ty::Int))),
                },
                PrimDef {
                    name: "second",
                    f: Time::second,
                    ty: Scheme::mono(Ty::Fn(vec![Ty::Time], Box::new(Ty::Int))),
                },
                PrimDef {
                    name: "sleep",
                    f: Time::sleep,
                    ty: Scheme::mono(Ty::Fn(vec![Ty::Int], Box::new(Ty::Unit))),
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
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Option(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Fn(
                                    vec![Ty::Var(TyVar::new(0))],
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                            ],
                            Box::new(Ty::Option(Box::new(Ty::Var(
                                TyVar::new(1),
                            )))),
                        ),
                        constraints: smallvec![],
                    },
                },
                // Regular primitives
                PrimDef {
                    name: "unwrap-or",
                    f: Opt::unwrap_or,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Option(Box::new(Ty::Var(TyVar::new(0)))),
                                Ty::Var(TyVar::new(0)),
                            ],
                            Box::new(Ty::Var(TyVar::new(0))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "flatten",
                    f: Opt::flatten,
                    ty: Scheme {
                        vars: vec![TyVar::new(0)],
                        ty: Ty::Fn(
                            vec![Ty::Option(Box::new(Ty::Option(Box::new(
                                Ty::Var(TyVar::new(0)),
                            ))))],
                            Box::new(Ty::Option(Box::new(Ty::Var(
                                TyVar::new(0),
                            )))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "note",
                    f: Opt::note,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Var(TyVar::new(1)),
                                Ty::Option(Box::new(Ty::Var(TyVar::new(0)))),
                            ],
                            Box::new(Ty::Result(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
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
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Result(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(2))),
                                ),
                                Ty::Fn(
                                    vec![Ty::Var(TyVar::new(0))],
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                            ],
                            Box::new(Ty::Result(
                                Box::new(Ty::Var(TyVar::new(1))),
                                Box::new(Ty::Var(TyVar::new(2))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "map-err",
                    f: Res::placeholder,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Result(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                                Ty::Fn(
                                    vec![Ty::Var(TyVar::new(1))],
                                    Box::new(Ty::Var(TyVar::new(2))),
                                ),
                            ],
                            Box::new(Ty::Result(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(2))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                // Regular primitives
                PrimDef {
                    name: "unwrap-or",
                    f: Res::unwrap_or,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![
                                Ty::Result(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                ),
                                Ty::Var(TyVar::new(0)),
                            ],
                            Box::new(Ty::Var(TyVar::new(0))),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "flatten",
                    f: Res::flatten,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Result(
                                Box::new(Ty::Result(
                                    Box::new(Ty::Var(TyVar::new(0))),
                                    Box::new(Ty::Var(TyVar::new(1))),
                                )),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )],
                            Box::new(Ty::Result(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )),
                        ),
                        constraints: smallvec![],
                    },
                },
                PrimDef {
                    name: "hush",
                    f: Res::hush,
                    ty: Scheme {
                        vars: vec![TyVar::new(0), TyVar::new(1)],
                        ty: Ty::Fn(
                            vec![Ty::Result(
                                Box::new(Ty::Var(TyVar::new(0))),
                                Box::new(Ty::Var(TyVar::new(1))),
                            )],
                            Box::new(Ty::Option(Box::new(Ty::Var(
                                TyVar::new(0),
                            )))),
                        ),
                        constraints: smallvec![],
                    },
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
                    ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::String))),
                },
                PrimDef {
                    name: "print",
                    f: Io::print,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::Unit),
                    )),
                },
                PrimDef {
                    name: "println",
                    f: Io::println,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::Unit),
                    )),
                },
                PrimDef {
                    name: "eprint",
                    f: Io::eprint,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::Unit),
                    )),
                },
                PrimDef {
                    name: "eprintln",
                    f: Io::eprintln,
                    ty: Scheme::mono(Ty::Fn(
                        vec![Ty::String],
                        Box::new(Ty::Unit),
                    )),
                },
            ])
            .with_submodule("Directory", directory_module),
        );
    }

    /// Build the `Io.Directory` submodule.
    fn build_directory_module(&mut self) -> Module {
        use crate::primitives::Directory;

        // Borrow `consts` to avoid borrow-splitting issues with `self`.
        // TODO: re-enable `scheme!` macro once `Ty` -> `TyId` switch is done
        let consts = &mut self.consts;

        Module::from_prims(&[
            PrimDef {
                name: "list-dir",
                f: Directory::list_dir,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Array(Box::new(Ty::Path))),
                )),
            },
            PrimDef {
                name: "exists",
                f: Directory::exists,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Bool),
                )),
            },
            PrimDef {
                name: "is-file",
                f: Directory::is_file,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Bool),
                )),
            },
            PrimDef {
                name: "is-dir",
                f: Directory::is_dir,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Bool),
                )),
            },
            PrimDef {
                name: "read-file",
                f: Directory::read_file,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::String),
                )),
            },
            PrimDef {
                name: "remove",
                f: Directory::remove,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "remove-all",
                f: Directory::remove_all,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "create-dir",
                f: Directory::create_dir,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "create-dir-all",
                f: Directory::create_dir_all,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "pwd",
                f: Directory::pwd,
                ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::FilePath))),
            },
            PrimDef {
                name: "set-pwd",
                f: Directory::set_pwd,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "get-env",
                f: Directory::get_env,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::String],
                    Box::new(Ty::Option(Box::new(Ty::String))),
                )),
            },
            PrimDef {
                name: "move-path",
                f: Directory::move_path,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Object(indexmap::indexmap! {
                        consts.intern("src") => Ty::FilePath,
                        consts.intern("dest") => Ty::FilePath,
                    })],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "copy-path",
                f: Directory::copy_path,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Object(indexmap::indexmap! {
                        consts.intern("src") => Ty::FilePath,
                        consts.intern("dest") => Ty::FilePath,
                    })],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "write-file",
                f: Directory::write_file,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Object(indexmap::indexmap! {
                        consts.intern("path") => Ty::FilePath,
                        consts.intern("contents") => Ty::String,
                    })],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "append-file",
                f: Directory::append_file,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Object(indexmap::indexmap! {
                        consts.intern("path") => Ty::FilePath,
                        consts.intern("contents") => Ty::String,
                    })],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "set-env",
                f: Directory::set_env,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::Object(indexmap::indexmap! {
                        consts.intern("name") => Ty::String,
                        consts.intern("value") => Ty::String,
                    })],
                    Box::new(Ty::Unit),
                )),
            },
            PrimDef {
                name: "canonicalize",
                f: Directory::canonicalize,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::FilePath),
                )),
            },
            PrimDef {
                name: "parent",
                f: Directory::parent,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Option(Box::new(Ty::FilePath))),
                )),
            },
            PrimDef {
                name: "file-name",
                f: Directory::file_name,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Option(Box::new(Ty::String))),
                )),
            },
            PrimDef {
                name: "extension",
                f: Directory::extension,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath],
                    Box::new(Ty::Option(Box::new(Ty::String))),
                )),
            },
            PrimDef {
                name: "join",
                f: Directory::join,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath, Ty::Array(Box::new(Ty::String))],
                    Box::new(Ty::FilePath),
                )),
            },
            PrimDef {
                name: "temp-dir",
                f: Directory::temp_dir,
                ty: Scheme::mono(Ty::Fn(vec![], Box::new(Ty::FilePath))),
            },
            PrimDef {
                name: "with-extension",
                f: Directory::with_extension,
                ty: Scheme::mono(Ty::Fn(
                    vec![Ty::FilePath, Ty::String],
                    Box::new(Ty::FilePath),
                )),
            },
        ])
    }
}
