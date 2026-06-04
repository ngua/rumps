use futures::future::BoxFuture;
use smallvec::SmallVec;

use crate::io::IoContext;
use crate::typecheck::{RuntimeTyId, RuntimeTypes, Scheme, TyArena};
use crate::value::{Payload, ValueArena, ValueId};
use crate::{Error, Result, Span};

/// Result type for primitive function execution.
pub(crate) type PrimResult<'a> = BoxFuture<'a, Result<ValueId>>;

/// Context passed to primitive functions during execution.
///
/// Contains references to the value arena for creating/looking up values,
/// the I/O context for output operations, and the call-site span for error
/// reporting.
pub(crate) struct PrimCtx<'a> {
    pub(crate) arena: &'a mut ValueArena,
    pub(crate) runtime_types: &'a mut RuntimeTypes,
    // Required for `Io.*` operations to work correctly, i.e. use the I/O
    // abstraction used elsewhere
    pub(crate) io: &'a mut dyn IoContext,
    pub(crate) span: Span,
}

impl PrimCtx<'_> {
    /// Create a `Result.Ok(v)` value from a `ValueId` already in the arena.
    pub(crate) fn result_ok(&mut self, v: ValueId) -> ValueId {
        let ok = Payload::ok(v);
        let ok_ty = self
            .arena
            .meta(v)
            .map(|m| m.ty)
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
        let err_ty = RuntimeTyId::from(TyArena::STRING);
        let ty = self.runtime_types.result(ok_ty, err_ty);
        self.arena
            .add_typed(ok, self.runtime_types.meta(ty), self.span)
    }

    /// Create a `Result.Err(msg)` value from a `ValueId` already in the arena.
    pub(crate) fn result_err(&mut self, msg: ValueId) -> ValueId {
        let err = Payload::err(msg);
        let ok_ty = RuntimeTyId::from(TyArena::UNIT);
        let err_ty = self
            .arena
            .meta(msg)
            .map(|m| m.ty)
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::STRING));
        let ty = self.runtime_types.result(ok_ty, err_ty);
        self.arena
            .add_typed(err, self.runtime_types.meta(ty), self.span)
    }

    /// Create an `Option.Some(v)` value from a `ValueId` already in the arena.
    pub(crate) fn option_some(&mut self, v: ValueId) -> ValueId {
        let some = Payload::some(v);
        let elem = self
            .arena
            .meta(v)
            .map(|m| m.ty)
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
        let ty = self.runtime_types.option(elem);
        self.arena
            .add_typed(some, self.runtime_types.meta(ty), self.span)
    }

    /// Create an `Option.None` value.
    pub(crate) fn option_none(&mut self) -> ValueId {
        let none = Payload::none();
        let ty = self.runtime_types.option(RuntimeTyId::from(TyArena::UNIT));
        self.arena
            .add_typed(none, self.runtime_types.meta(ty), self.span)
    }

    pub(crate) fn add(&mut self, v: Payload) -> ValueId {
        let meta = self.runtime_types.meta_for_payload(self.arena, &v);
        self.arena.add_typed(v, meta, self.span)
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
