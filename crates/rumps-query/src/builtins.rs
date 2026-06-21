//! Runtime ABI and bodies for builtin implementations.
//!
//! This module is the boundary between builtin bodies and `Interpreter`.
//! Builtin bodies receive `ValueId`s and return a `ValueId`. The caller
//! supplies `OutputMeta`, and `BuiltinCtx::finish` applies the existing output
//! refinement helpers after the body finishes. That keeps expression metadata,
//! type metadata, variant refinement, and payload only calls centralized.
//!
//! `builtins::Impl::Sync` is for cheap builtin bodies that do not need an awaited
//! interpreter service. `Interpreter::invoke_builtin` calls the function
//! pointer directly, so the sync path does not allocate a boxed future.
//! `builtins::Impl::Async` is for builtin bodies that need callbacks, class
//! dispatch, persistent map ordering, or I/O. Only that branch pays for
//! `BoxFuture`.
//!
//! `BuiltinCtx` deliberately exposes capabilities instead of interpreter
//! fields. Use `ctx.vals()` for arena and small value constructors. Use
//! `ctx.maps()` for persistent map algorithms that need async class dispatch.
//! Do not hold `builtins::Values` across `.await`; take the data needed for the
//! awaited operation, then drop the facet before calling `ctx.invoke(...)` or
//! `ctx.class_call(...)`.
//!
//! Typed value extractors such as `builtins::Values::string_payload` are for
//! statically checked builtin arguments. A wrong payload variant is therefore a
//! `typechecked!` failure, not a recoverable runtime error. Runtime checks
//! should stay explicit in the builtin body when a value is genuinely dynamic.

#![allow(dead_code)]

mod array;
mod io;
mod map;
mod math;
mod option;
mod prelude;
mod random;
mod range;
mod result;
mod string;
mod time;

pub(crate) use array::Array;
use futures::future::BoxFuture;
pub(crate) use io::{Directory, Io};
pub(crate) use map::Map;
pub(crate) use math::{Math, Trig};
pub(crate) use option::Opt;
pub(crate) use prelude::Prelude;
pub(crate) use random::Random;
pub(crate) use range::Range;
pub(crate) use result::Res;
use smallvec::SmallVec;
pub(crate) use string::Str;
pub(crate) use time::Time;

use crate::ast::ExprId;
use crate::intern::StringId;
use crate::interpreter::Interpreter;
use crate::typecheck::{RuntimeTyId, Scheme};
use crate::value::{Payload, ValueId, ValueMeta};
use crate::{Result, Span};

/// Runtime implementation shape for a builtin.
///
/// `Sync` preserves the direct call fast path for pure operations. `Async`
/// gives a builtin access to interpreter services that may await.
#[derive(Clone, Copy)]
pub(crate) enum Impl {
    Sync(SyncFn),
    Async(AsyncFn),
}

/// Function pointer ABI for sync builtins.
///
/// The body can read and write arena values through `BuiltinCtx`, but it must
/// not perform awaited interpreter work.
pub(crate) type SyncFn = for<'cx, 'i, 'ast, 'io> fn(
    &'cx mut BuiltinCtx<'i, 'ast, 'io>,
    SmallVec<[ValueId; 4]>,
) -> Result<ValueId>;

/// Function pointer ABI for async builtins.
///
/// Use this only when the body needs callbacks, class dispatch, map comparison,
/// map insertion, map lookup, or I/O.
pub(crate) type AsyncFn =
    for<'cx, 'i, 'ast, 'io> fn(
        &'cx mut BuiltinCtx<'i, 'ast, 'io>,
        SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'cx, Result<ValueId>>;

/// Capability context passed to builtin bodies.
///
/// The interpreter reference stays private. Builtins use methods on `Self`,
/// `Values`, and `Maps` so the ABI surface stays narrow while the
/// migration proceeds.
pub(crate) struct BuiltinCtx<'i, 'ast, 'io> {
    pub(crate) interp: &'i mut Interpreter<'ast, 'io>,
    pub(crate) span: Span,
    pub(crate) output: OutputMeta,
    pub(crate) meta: Option<CallMeta>,
}

/// Value arena facet for builtin bodies.
///
/// This facet owns small constructors and typed value access. It should be
/// short lived; do not keep it across `.await`.
pub(crate) struct Values<'cx, 'i, 'ast, 'io> {
    pub(crate) ctx: &'cx mut BuiltinCtx<'i, 'ast, 'io>,
}

/// Persistent map facet for builtin bodies.
///
/// Map operations may call class methods for ordering and equality, so they are
/// async and remain separate from `Values`.
pub(crate) struct Maps<'cx, 'i, 'ast, 'io> {
    pub(crate) ctx: &'cx mut BuiltinCtx<'i, 'ast, 'io>,
}

/// Selected builtin call target.
///
/// Class dispatch will eventually produce this after it decides that a call
/// should use a builtin rather than a user method.
pub(crate) struct Call {
    pub(crate) imp: Impl,
    pub(crate) args: SmallVec<[ValueId; 4]>,
    pub(crate) span: Span,
    pub(crate) output: OutputMeta,
    pub(crate) meta: Option<CallMeta>,
}

/// Extra metadata supplied by dispatch layers.
///
/// `BuiltinCtx` stores this as `Option<CallMeta>`. Absence implies an
/// ordinary builtin call with no class selected target metadata. Class selected
/// builtins pass `Some(...)` so builtin bodies do not have to reconstruct
/// target metadata from expressions.
#[derive(Clone, Copy)]
pub(crate) enum CallMeta {
    Nullary {
        ty: RuntimeTyId,
    },
    Convert {
        target: RuntimeTyId,
        edge: Option<ValueMeta>,
    },
}

/// Result of selecting a callable target.
///
/// This is scaffold for the later class dispatch migration. It keeps target
/// selection separate from builtin execution.
pub(crate) enum Selected {
    User(StringId),
    Builtin(Call),
}

/// Registration data for a builtin module function.
pub(crate) struct Def {
    pub(crate) name: &'static str,
    pub(crate) imp: Impl,
    pub(crate) ty: Scheme,
}

/// Output refinement requested by the caller.
///
/// Builtin bodies return a `ValueId`, not a final `Value`. `BuiltinCtx::finish`
/// applies this metadata through the existing interpreter output helpers.
#[derive(Clone, Copy)]
pub(crate) enum OutputMeta {
    Expr(ExprId),
    Ty(RuntimeTyId),
    Meta(ValueMeta),
    Payload,
}

impl<'i, 'ast, 'io> BuiltinCtx<'i, 'ast, 'io> {
    /// Creates a `BuiltinCtx` for one builtin invocation.
    pub(crate) fn new(
        interp: &'i mut Interpreter<'ast, 'io>,
        span: Span,
        output: OutputMeta,
        meta: Option<CallMeta>,
    ) -> Self {
        Self {
            interp,
            span,
            output,
            meta,
        }
    }
}

impl OutputMeta {
    /// Returns the explicit output type when this metadata stores one.
    pub(crate) fn ty(self) -> Option<RuntimeTyId> {
        match self {
            Self::Ty(ty) => Some(ty),
            Self::Expr(_) | Self::Meta(_) | Self::Payload => None,
        }
    }
}

/// Shared utilities for builtin function bodies.
///
/// Module types such as [`Array`] and [`Str`] implement this trait to share
/// helper methods while registration still stores plain function pointers.
trait Body {
    /// Convert a value to `f64`, accepting `Int` or `Float`.
    ///
    /// The type checker guarantees the value is numeric.
    fn to_float(ctx: &mut BuiltinCtx<'_, '_, '_>, id: ValueId) -> Result<f64> {
        ctx.vals().payload(id).map(|v| match v {
            Payload::Int(n) => *n as f64,
            Payload::Float(f) => f.0,
            _ => typechecked!("to_float", "Numeric"),
        })
    }
}
