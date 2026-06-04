//! Built-in primitive functions organized by module.
//!
//! Primitives are callable built-in functions registered in the environment.
//! They are implemented as associated functions on module types ([`Array`],
//! [`Str`], etc.), returning a future that resolves to a `ValueId`. Unlike
//! keywords (`@get`, `@set`, `@kill`), primitives use standard function call syntax
//! and are case-sensitive.
//!
//! # Module Organization
//!
//! Each RUMPS module is a separate type implementing the [`Prim`] trait:
//! - [`Array`]: `push`, `pop`, `head`, `tail`, `sort`, `slice`, `concat`,
//!   `sort-by`, `zip`, `zip-with`, `unzip`, `intersperse`
//!
//! Iterable operations (`length`, `collect`, `map`, `filter`, `reduce`)
//! are handled via class method syntax (e.g. `Iterable:length`, `Mappable:map`).
//! `foreach` and `contains` are standalone `Prelude` functions.
//!
//! When adding a new RUMPS module, create a new type implementing [`Prim`]
//! and add its functions as associated functions.
//!
//! # Primitives with Higher-Order Functions Cannot Go Here
//!
//! **Important**: Any function that needs to invoke closures or user-defined
//! functions (i.e., higher-order functions) MUST be implemented through interpreter module dispatch, NOT here.
//!
//! **Why?** The [`PrimFn`] type signature only receives [`ValueId`]s; it has
//! no access to the interpreter's closure invocation machinery. Calling a
//! closure requires [`Interpreter::invoke_callable`], which isn't available
//! from [`PrimCtx`].
//!
//! **Placeholders**: For each HoF, a placeholder function is registered here
//! so that [`Environment::module_fn_exists`] returns `true` during name
//! resolution. The placeholders are intercepted by interpreter module dispatch before
//! primitive dispatch and never actually called.
//!
//! [`Interpreter`]: crate::interpreter::Interpreter
//! [`PrimFn`]: crate::env::PrimFn
//! [`PrimCtx`]: crate::env::PrimCtx
//! [`Environment::module_fn_exists`]: crate::env::Environment::module_fn_exists
//! [`Interpreter::invoke_callable`]: crate::interpreter::Interpreter::invoke_callable
//! [`Interpreter::invoke_module_fn`]: crate::interpreter::Interpreter::invoke_module_fn

mod array;
mod io;
mod map;
mod math;
mod option;
mod prelude;
mod random;
mod result;
mod string;
mod time;

pub(crate) use array::Array;
pub(crate) use io::{Directory, Io};
pub(crate) use map::Map;
pub(crate) use math::{Math, Trig};
pub(crate) use option::Opt;
pub(crate) use prelude::Prelude;
pub(crate) use random::Random;
pub(crate) use result::Res;
use smallvec::SmallVec;
pub(crate) use string::Str;
pub(crate) use time::Time;

use crate::env::{PrimCtx, PrimResult};
use crate::typecheck::RuntimeTypes;
use crate::value::{Payload, TypeId, Value, ValueArena, ValueId};
use crate::StringId;

impl RuntimeTypes {
    fn meta_type_id(&self, v: &Value) -> Option<TypeId> {
        self.to_type_id(v.repr).or_else(|| self.to_type_id(v.ty))
    }
}

/// Shared utilities for primitive function implementations.
///
/// Module types ([`Array`], [`Str`], etc.) implement this trait to gain
/// access to common helpers like the HoF placeholder.
///
/// # Why a trait with associated functions?
///
/// `PrimFn` uses a higher-ranked trait bound (HRTB) so it can be stored in a
/// `HashMap` without lifetime parameters, yet work with any `PrimCtx<'a>`
/// lifetime when called. Methods on `impl<'a> PrimCtx<'a>` bind the lifetime
/// parameter to the struct's lifetime, which doesn't satisfy the HRTB
/// `for<'a>` requirement. Associated functions on separate types sidestep
/// this by not binding the lifetime in the impl block.
///
/// # Note on Type Safety
///
/// Prior to the static type checker, primitives needed runtime arity and type
/// checks. The type checker now guarantees these constraints at compile time:
/// - Arity is enforced by the `Callable` constraint
/// - Argument types are enforced by function type signatures
///
/// Primitives can now directly index `args[i]` and pattern-match on values
/// without runtime checks. Use `typechecked!` for impossible branches.
pub(crate) trait Prim {
    /// Placeholder for higher-order functions (`Iter.map`, `Iterable.filter`, etc.).
    ///
    /// This should never be called directly; `invoke_module_fn` intercepts
    /// these calls and handles them specially. If this is called, it indicates
    /// a bug in the dispatch logic.
    fn placeholder<'a>(
        _ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        invariant!("HoF placeholder intercepted by invoke_module_fn")
    }

    /// Look up an interned string by `StringId`.
    ///
    /// Since the `StringId` is already validated (via `get_string_id` +
    /// `typechecked!`), the lookup should always succeed.
    fn valid_str(arena: &ValueArena, sid: StringId) -> &str {
        arena
            .get_str(sid)
            .unwrap_or_else(|| invariant!("StringId lookup"))
    }

    /// Convert a value to `f64`, accepting `Int` or `Float`.
    ///
    /// Type checker guarantees value is numeric.
    fn to_float(ctx: &PrimCtx<'_>, id: ValueId) -> f64 {
        ctx.arena.payload(id).map_or_else(
            || typechecked!("to_float", "ValueId"),
            |v| match v {
                Payload::Int(n) => *n as f64,
                Payload::Float(f) => f.0,
                _ => typechecked!("to_float", "Numeric"),
            },
        )
    }

    /// Convert a value to `i64`, accepting `Int` only.
    ///
    /// Type checker guarantees value is `Int`.
    fn to_int(ctx: &PrimCtx<'_>, id: ValueId) -> i64 {
        ctx.arena.payload(id).map_or_else(
            || typechecked!("to_int", "ValueId"),
            |v| match v {
                Payload::Int(n) => *n,
                _ => typechecked!("to_int", "Int"),
            },
        )
    }
}
