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
//! `foreach`, `contains`, and `reverse` are standalone Prelude functions.
//!
//! When adding a new RUMPS module, create a new type implementing [`Prim`]
//! and add its functions as associated functions.
//!
//! # Primitives with Higher-Order Functions Cannot Go Here
//!
//! **Important**: Any function that needs to invoke closures or user-defined
//! functions (i.e., higher-order functions) MUST be implemented directly on
//! [`Interpreter`] in `call.rs`, NOT here.
//!
//! **Why?** The [`PrimFn`] type signature only receives [`ValueId`]s; it has
//! no access to the interpreter's closure invocation machinery. Calling a
//! closure requires [`Interpreter::invoke_callable`], which isn't available
//! from [`PrimCtx`].
//!
//! **Placeholders**: For each HoF, a placeholder function is registered here
//! so that [`Environment::module_fn_exists`] returns `true` during name
//! resolution. The placeholders are intercepted in
//! [`Interpreter::invoke_module_fn`] before dispatch and never actually called.
//!
//! [`Interpreter`]: crate::interpreter::Interpreter
//! [`PrimFn`]: crate::env::PrimFn
//! [`PrimCtx`]: crate::env::PrimCtx
//! [`Environment::module_fn_exists`]: crate::env::Environment::module_fn_exists
//! [`Interpreter::invoke_callable`]: crate::interpreter::Interpreter::invoke_callable
//! [`Interpreter::invoke_module_fn`]: crate::interpreter::Interpreter::invoke_module_fn

use std::sync::Arc;

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use indexmap::IndexMap;
use itertools::Itertools;
use ordered_float::OrderedFloat;
use rand::seq::SliceRandom;
use rand::Rng;
use smallvec::{smallvec, SmallVec};
use unicode_segmentation::UnicodeSegmentation;

use crate::env::{PrimCtx, PrimResult};
use crate::value::{MapKey, TypeExprArena, TypeId, Value, ValueArena, ValueId};

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
    fn valid_str(arena: &ValueArena, sid: crate::StringId) -> &str {
        arena
            .get_str(sid)
            .unwrap_or_else(|| invariant!("StringId lookup"))
    }

    /// Convert a value to `f64`, accepting `Int` or `Float`.
    ///
    /// Type checker guarantees value is numeric.
    fn to_float(ctx: &PrimCtx<'_>, id: ValueId) -> f64 {
        ctx.arena.get(id).map_or_else(
            || typechecked!("to_float", "ValueId"),
            |v| match v {
                Value::Int(n) => *n as f64,
                Value::Float(f) => f.0,
                _ => typechecked!("to_float", "Numeric"),
            },
        )
    }

    /// Convert a value to `i64`, accepting `Int` only.
    ///
    /// Type checker guarantees value is `Int`.
    fn to_int(ctx: &PrimCtx<'_>, id: ValueId) -> i64 {
        ctx.arena.get(id).map_or_else(
            || typechecked!("to_int", "ValueId"),
            |v| match v {
                Value::Int(n) => *n,
                _ => typechecked!("to_int", "Int"),
            },
        )
    }
}

/// Primitives for the `Array` module.
///
// NOTE: Iterable functions (map, filter, reduce) are higher-order and require
// access to the interpreter's closure invocation machinery. They are
// implemented in `interpreter/call.rs` and registered here as placeholders.
//
// The placeholders ensure `Environment::module_fn_exists` returns true during
// name resolution. The actual dispatch is intercepted in `invoke_module_fn`.
pub(crate) struct Array;

impl Prim for Array {}

impl Array {
    /// `forall T. (Array[T], T) -> Array[T]`
    ///
    /// Returns a new array with `val` appended to the end.
    pub(crate) fn push<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, mut e) = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.push", "Array"));

            e.push(args[1]);
            Ok(ctx.arena.add(Value::Array(ty, Arc::new(e)), ctx.span))
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with the last element removed.
    /// Returns an empty array if the input is empty.
    pub(crate) fn pop<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, mut e) = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.pop", "Array"));

            e.pop();
            Ok(ctx.arena.add(Value::Array(ty, Arc::new(e)), ctx.span))
        })
    }

    /// `forall T. (Array[T]) -> Option[T]`
    ///
    /// Returns `Option.Some(first)` if the array is non-empty,
    /// `Option.None` if empty.
    pub(crate) fn head<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.head", "Array"));

            Ok(match elems.first() {
                Some(first) => ctx.option_some(*first),
                None => ctx.option_none(),
            })
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with all elements except the first.
    /// Returns an empty array if the input is empty or has one element.
    pub(crate) fn tail<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.tail", "Array"));

            let tail: SmallVec<[ValueId; 4]> =
                elems.get(1..).map(SmallVec::from_slice).unwrap_or_default();
            Ok(ctx.arena.add(Value::Array(ty, Arc::new(tail)), ctx.span))
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements sorted in ascending order.
    /// Supports all comparable types including sum types, tuples, and arrays.
    ///
    /// Ordering semantics:
    /// - Scalars: natural ordering (`false < true`, numeric, lexicographic)
    /// - `Option`: `None < Some`
    /// - `Result`: `Err < Ok` (success sorts after failure)
    /// - User-defined sum types: declaration order (variant index)
    /// - Tuples/Arrays: lexicographic
    /// - Objects: lexicographic by (key, value) pairs
    pub(crate) fn sort<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        /// Sort key for `Array.sort`; supports all value types recursively.
        ///
        /// Variant order determines comparison precedence for heterogeneous
        /// arrays (if they were allowed). For homogeneous arrays, only one
        /// variant is used.
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
        enum SortKey<'a> {
            Bool(bool),
            Int(i64),
            Float(OrderedFloat<f64>),
            Char(char),
            String(&'a str),
            /// Tagged value: (sort_priority, recursive payloads)
            ///
            /// Priority from `sort_priority`, then recursive payload keys.
            Tagged(u8, Vec<Self>),
            Tuple(Vec<Self>),
            Array(Vec<Self>),
            Object(Vec<(&'a str, Self)>),
        }

        impl<'a> SortKey<'a> {
            fn from_value(
                v: &Value,
                arena: &'a ValueArena,
                type_exprs: &TypeExprArena,
            ) -> Option<Self> {
                match v {
                    Value::Bool(b) => Some(Self::Bool(*b)),
                    Value::Int(n) => Some(Self::Int(*n)),
                    // Word sorts as Int (coerced)
                    Value::Word(n) => Some(Self::Int(*n as i64)),
                    Value::Float(f) => Some(Self::Float(*f)),
                    Value::Char(c) => Some(Self::Char(*c)),
                    Value::String(sid) => arena.get_str(*sid).map(Self::String),

                    Value::Tagged(ty_expr, idx, payloads) => {
                        let priority =
                            Self::sort_priority(ty_expr, *idx, type_exprs);
                        let sub_keys: Option<Vec<SortKey<'a>>> = payloads
                            .iter()
                            .map(|vid| {
                                arena.get(*vid).and_then(|pv| {
                                    Self::from_value(pv, arena, type_exprs)
                                })
                            })
                            .collect();
                        sub_keys.map(|keys| Self::Tagged(priority, keys))
                    }

                    Value::Tuple(_, elems) => {
                        let sub_keys: Option<Vec<SortKey<'a>>> = elems
                            .iter()
                            .map(|vid| {
                                arena.get(*vid).and_then(|ev| {
                                    Self::from_value(ev, arena, type_exprs)
                                })
                            })
                            .collect();
                        sub_keys.map(Self::Tuple)
                    }

                    Value::Array(_, elems) => {
                        let sub_keys: Option<Vec<SortKey<'a>>> = elems
                            .iter()
                            .map(|vid| {
                                arena.get(*vid).and_then(|ev| {
                                    Self::from_value(ev, arena, type_exprs)
                                })
                            })
                            .collect();
                        sub_keys.map(Self::Array)
                    }

                    Value::Object(map) => {
                        let sub_keys: Option<Vec<(&'a str, SortKey<'a>)>> = map
                            .iter()
                            .map(|(k, vid)| {
                                arena.get_str(*k).and_then(|key_str| {
                                    arena.get(*vid).and_then(|val| {
                                        Self::from_value(val, arena, type_exprs)
                                            .map(|sk| (key_str, sk))
                                    })
                                })
                            })
                            .collect();
                        sub_keys.map(Self::Object)
                    }

                    // Maps, times, and JSON are not directly comparable for sorting
                    Value::Map(_, _, _) | Value::Time(_) | Value::Json(_) => {
                        None
                    }

                    // Unit, closures, functions, module fns/consts, ranges, paths, regex,
                    // refs, and loop continuations are not comparable
                    Value::Unit
                    | Value::FilePath(_)
                    | Value::Regex(_)
                    | Value::Closure { .. }
                    | Value::Function { .. }
                    | Value::ModuleFn { .. }
                    | Value::ModuleConst { .. }
                    | Value::Range { .. }
                    | Value::ForeverContinuation
                    | Value::LoopContinue(_)
                    | Value::Ref(..)
                    | Value::ClassMethodFn { .. }
                    | Value::PartialApp { .. } => None,
                    // TODO(Phase 6): unwrap and build SortKey from inner value
                    Value::Union(_, _) | Value::Newtype(_, _) => {
                        todo!("Phase 6: SortKey::from_value Union/Newtype")
                    }
                }
            }

            /// Compute sort priority for tagged values.
            ///
            /// For Option and Result, we want `Some > None` and `Ok > Err`
            /// semantically. Declaration order is `None, Some` and `Ok, Err`,
            /// so Option already sorts correctly but Result needs inversion.
            fn sort_priority(
                ty_expr: &crate::value::TypeExprId,
                idx: u8,
                type_exprs: &TypeExprArena,
            ) -> u8 {
                match type_exprs.base_type(*ty_expr) {
                    Some(TypeId::OPTION) => idx,
                    Some(TypeId::RESULT) => 1 - idx,
                    _ => idx,
                }
            }
        }

        Box::pin(async move {
            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.sort", "Array"));

            // Collect (ValueId, sortable key) pairs
            let mut pairs: Vec<(ValueId, SortKey)> = elems
                .iter()
                .map(|vid| {
                    // Type checker guarantees array elements are comparable
                    ctx.arena
                        .get(*vid)
                        .ok_or_else(|| {
                            ctx.runtime_error("Array.sort: invalid element")
                        })
                        .map(|v| {
                            let k = SortKey::from_value(
                                v,
                                ctx.arena,
                                ctx.type_exprs,
                            )
                            .unwrap_or_else(|| {
                                typechecked!("Array.sort", "Comparable")
                            });
                            (*vid, k)
                        })
                })
                .collect::<crate::Result<Vec<_>>>()?;

            pairs.sort_by(|(_, a), (_, b)| a.cmp(b));

            let sorted: SmallVec<[ValueId; 4]> =
                pairs.into_iter().map(|(vid, _)| vid).collect();
            Ok(ctx.arena.add(Value::Array(ty, Arc::new(sorted)), ctx.span))
        })
    }

    /// `forall T. (Array[T], Int, Int) -> Array[T]`
    ///
    /// Returns a new array containing elements from index `start` (inclusive)
    /// to index `end` (exclusive). Indices are clamped to valid bounds.
    pub(crate) fn slice<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.slice", "Array"));

            let start = ctx
                .arena
                .get(args[1])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    typechecked!("Array.slice", "start must be Int")
                });

            let end = ctx
                .arena
                .get(args[2])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Array.slice", "Int"));

            let len = elems.len() as i64;
            let start_idx = start.max(0).min(len) as usize;
            let end_idx = end.max(0).min(len) as usize;

            let sliced: SmallVec<[ValueId; 4]> = elems
                .get(start_idx..end_idx)
                .map(SmallVec::from_slice)
                .unwrap_or_default();

            Ok(ctx.arena.add(Value::Array(ty, Arc::new(sliced)), ctx.span))
        })
    }

    /// `forall T. (Array[T], Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements of `b` appended to `a`.
    /// Both arrays must have the same element type.
    pub(crate) fn concat<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty_a, mut combined) = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.concat", "Array"));

            let (_, elems_b) = ctx
                .arena
                .get_array(args[1])
                .unwrap_or_else(|| typechecked!("Array.concat", "Array"));

            // Type checker guarantees both arrays have matching element types
            combined.extend(elems_b.iter().copied());
            Ok(ctx
                .arena
                .add(Value::Array(ty_a, Arc::new(combined)), ctx.span))
        })
    }

    /// `forall T U. (Array[T], Array[U]) -> Array[(T, U)]`
    ///
    /// Pairs elements from two arrays. Result length is the shorter array.
    pub(crate) fn zip<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty_a, elems_a) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.zip", "Array"));

            let (ty_b, elems_b) = ctx
                .arena
                .get_array(args[1])
                .unwrap_or_else(|| typechecked!("Array.zip", "Array"));

            let zipped: SmallVec<[(ValueId, ValueId); 4]> = elems_a
                .iter()
                .zip(elems_b.iter())
                .map(|(a, b)| (*a, *b))
                .collect();

            let pairs: SmallVec<[ValueId; 4]> = zipped
                .iter()
                .map(|(a, b)| {
                    let tup_ty = ctx.type_exprs.tuple(smallvec![ty_a, ty_b]);
                    let tup = Value::Tuple(tup_ty, Arc::new(smallvec![*a, *b]));
                    ctx.arena.add(tup, ctx.span)
                })
                .collect();

            let elem_ty = ctx.type_exprs.tuple(smallvec![ty_a, ty_b]);
            Ok(ctx
                .arena
                .add(Value::Array(elem_ty, Arc::new(pairs)), ctx.span))
        })
    }

    /// `forall T U. (Array[(T, U)]) -> (Array[T], Array[U])`
    ///
    /// Splits an array of pairs into a pair of arrays.
    pub(crate) fn unzip<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, pairs) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.unzip", "Array"));

            // Extract element types from first pair
            let (ty_a, ty_b) = pairs
                .first()
                .and_then(|id| ctx.arena.get(*id))
                .and_then(|v| match v {
                    Value::Tuple(tup_ty, _) => {
                        ctx.type_exprs.tuple_elems(*tup_ty)
                    }
                    _ => None,
                })
                .and_then(|tys| Some((*tys.first()?, *tys.get(1)?)))
                .unwrap_or_else(|| {
                    // Empty array; types don't matter
                    let unk = ctx.type_exprs.named(TypeId::UNKNOWN);
                    (unk, unk)
                });

            // Map pairs to (a, b) and unzip
            let (firsts, seconds): (
                SmallVec<[ValueId; 4]>,
                SmallVec<[ValueId; 4]>,
            ) = pairs
                .iter()
                .map(|id| {
                    ctx.arena.get(*id).unwrap_or_else(|| {
                        typechecked!("Array.unzip", "valid id")
                    })
                })
                .map(|v| match v {
                    Value::Tuple(_, elems) => (elems[0], elems[1]),
                    _ => typechecked!("Array.unzip", "(T, U)"),
                })
                .unzip();

            let arr_a_id = ctx
                .arena
                .add(Value::Array(ty_a, Arc::new(firsts)), ctx.span);
            let arr_b_id = ctx
                .arena
                .add(Value::Array(ty_b, Arc::new(seconds)), ctx.span);

            let arr_ty_a = ctx.type_exprs.app(TypeId::ARRAY, smallvec![ty_a]);
            let arr_ty_b = ctx.type_exprs.app(TypeId::ARRAY, smallvec![ty_b]);
            let tup_ty = ctx.type_exprs.tuple(smallvec![arr_ty_a, arr_ty_b]);

            Ok(ctx.arena.add(
                Value::Tuple(tup_ty, Arc::new(smallvec![arr_a_id, arr_b_id])),
                ctx.span,
            ))
        })
    }

    /// `forall T. (T, Array[T]) -> Array[T]`
    ///
    /// Inserts `sep` between each pair of elements.
    pub(crate) fn intersperse<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sep = args[0];
            let (ty, elems) = ctx
                .arena
                .get_array(args[1])
                .unwrap_or_else(|| typechecked!("Array.intersperse", "Array"));

            let result: SmallVec<[ValueId; 4]> =
                Itertools::intersperse(elems.iter().copied(), sep).collect();

            Ok(ctx.arena.add(Value::Array(ty, Arc::new(result)), ctx.span))
        })
    }
}

/// Primitives for the `String` module.
///
/// Named `Str` to avoid collision with Rust's `String`.
pub(crate) struct Str;

impl Prim for Str {}

impl Str {
    /// `(String) -> Int`
    ///
    /// Returns the number of grapheme clusters in the string.
    pub(crate) fn length<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.length", "String"));

            let s = Self::valid_str(ctx.arena, sid);

            let len = s.graphemes(true).count() as i64;
            Ok(ctx.arena.add(Value::Int(len), ctx.span))
        })
    }

    /// `(String) -> String`
    ///
    /// Returns the string in uppercase.
    pub(crate) fn upper<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.upper", "String"));

            let s = Self::valid_str(ctx.arena, sid);

            let upper = s.to_uppercase();
            let new_sid = ctx.arena.intern(&upper);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `(String) -> String`
    ///
    /// Returns the string in lowercase.
    pub(crate) fn lower<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.lower", "String"));

            let s = Self::valid_str(ctx.arena, sid);

            let lower = s.to_lowercase();
            let new_sid = ctx.arena.intern(&lower);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `(String) -> String`
    ///
    /// Returns the string with leading and trailing whitespace removed.
    pub(crate) fn trim<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.trim", "String"));

            // Copy to owned String to release borrow before interning
            let trimmed = Self::valid_str(ctx.arena, sid).trim().to_owned();

            let new_sid = ctx.arena.intern(&trimmed);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `(String, String) -> Array[String]`
    ///
    /// Splits the string by the delimiter, returning an array of substrings.
    pub(crate) fn split<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.split", "String"));

            let d_sid = ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                typechecked!("String.split", "delimiter must be String")
            });

            // Copy strings to owned values to release borrow before iteration
            let s = Self::valid_str(ctx.arena, s_sid).to_owned();
            let d = Self::valid_str(ctx.arena, d_sid).to_owned();

            // Split and collect parts; intern each part
            let parts: SmallVec<[ValueId; 4]> = s
                .split(&d)
                .map(|part| {
                    let part_sid = ctx.arena.intern(part);
                    ctx.arena.add(Value::String(part_sid), ctx.span)
                })
                .collect();

            let str_ty = ctx.type_exprs.named(TypeId::STRING);
            Ok(ctx
                .arena
                .add(Value::Array(str_ty, Arc::new(parts)), ctx.span))
        })
    }

    /// `(Array[String], String) -> String`
    ///
    /// Joins an array of strings with the delimiter.
    pub(crate) fn join<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, elems) = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("String.join", "Array"));

            let d_sid = ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                typechecked!("String.join", "delimiter must be String")
            });

            // Collect string slices from array elements
            // Type checker guarantees elements are String
            let parts: Vec<&str> = elems
                .iter()
                .map(|id| {
                    ctx.arena
                        .get_string_id(*id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("String.join", "Array[String]")
                        })
                })
                .collect();

            let d = ctx.arena.get_str(d_sid).ok_or_else(|| {
                ctx.runtime_error("String.join: invalid delimiter")
            })?;

            let joined = parts.into_iter().join(d);
            let new_sid = ctx.arena.intern(&joined);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `(String, Int, Int) -> String`
    ///
    /// Returns a substring from index `start` (inclusive) to `end` (exclusive).
    /// Indices are grapheme-based and clamped to valid bounds.
    pub(crate) fn slice<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.slice", "String"));

            let start = ctx
                .arena
                .get(args[1])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    typechecked!("String.slice", "start must be Int")
                });

            let end = ctx
                .arena
                .get(args[2])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    typechecked!("String.slice", "end must be Int")
                });

            let s = Self::valid_str(ctx.arena, sid);

            // Single-pass: skip, take, join graphemes
            let start_idx = start.max(0) as usize;
            let end_idx = end.max(0) as usize;
            let sliced: String = s
                .graphemes(true)
                .skip(start_idx)
                .take(end_idx.saturating_sub(start_idx))
                .collect();

            let new_sid = ctx.arena.intern(&sliced);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `(String, String) -> Bool`
    ///
    /// Returns `true` if the string contains the substring.
    pub(crate) fn contains<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.contains", "String"));

            let sub_sid =
                ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                    typechecked!("String.contains", "substring must be String")
                });

            let s = Self::valid_str(ctx.arena, s_sid);
            let sub = Self::valid_str(ctx.arena, sub_sid);

            Ok(ctx.arena.add(Value::Bool(s.contains(sub)), ctx.span))
        })
    }

    /// `(String, String, String) -> String`
    ///
    /// Replaces all occurrences of `old` with `new`.
    pub(crate) fn replace<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.replace", "String"));

            let old_sid =
                ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                    typechecked!("String.replace", "pattern must be String")
                });

            let new_sid =
                ctx.arena.get_string_id(args[2]).unwrap_or_else(|| {
                    typechecked!("String.replace", "replacement must be String")
                });

            let s = Self::valid_str(ctx.arena, s_sid);
            let old = Self::valid_str(ctx.arena, old_sid);
            let new = Self::valid_str(ctx.arena, new_sid);

            let replaced = s.replace(old, new);
            let result_sid = ctx.arena.intern(&replaced);
            Ok(ctx.arena.add(Value::String(result_sid), ctx.span))
        })
    }

    /// `(String) -> String`
    ///
    /// Escapes special characters for display. Converts:
    /// - `"` → `\"`
    /// - `\` → `\\`
    /// - newline → `\n`
    /// - tab → `\t`
    /// - carriage return → `\r`
    /// - null → `\0`
    pub(crate) fn escape<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("String.escape", "String"));

            let s = Self::valid_str(ctx.arena, sid);
            let escaped = crate::interpreter::convert::escape_str(s);
            let new_sid = ctx.arena.intern(&escaped);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }
}

/// Primitives for the `Math` module.
pub(crate) struct Math;

impl Prim for Math {}

impl Math {
    /// `forall T: Numeric. (T) -> T`
    ///
    /// Returns the absolute value.
    pub(crate) fn abs<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let v = ctx
                .arena
                .get(args[0])
                .ok_or_else(|| ctx.runtime_error("Math.abs: invalid value"))?;

            let result = match v {
                Value::Int(n) => Value::Int(n.abs()),
                Value::Word(n) => Value::Word(*n), // Word is unsigned; abs is identity
                Value::Float(f) => Value::Float(OrderedFloat(f.0.abs())),
                _ => typechecked!("Math.abs", "Numeric"),
            };

            Ok(ctx.arena.add(result, ctx.span))
        })
    }

    /// `forall T: Numeric. (T, T) -> T`
    ///
    /// Returns the minimum of two numbers.
    pub(crate) fn min<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let result = match (ctx.arena.get(args[0]), ctx.arena.get(args[1]))
            {
                (Some(Value::Int(x)), Some(Value::Int(y))) => {
                    Value::Int((*x).min(*y))
                }
                (Some(Value::Word(x)), Some(Value::Word(y))) => {
                    Value::Word((*x).min(*y))
                }
                (Some(Value::Float(x)), Some(Value::Float(y))) => {
                    Value::Float(OrderedFloat(x.0.min(y.0)))
                }
                _ => typechecked!("Math.min", "same Numeric type"),
            };

            Ok(ctx.arena.add(result, ctx.span))
        })
    }

    /// `forall T: Numeric. (T, T) -> T`
    ///
    /// Returns the maximum of two numbers.
    pub(crate) fn max<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let result = match (ctx.arena.get(args[0]), ctx.arena.get(args[1]))
            {
                (Some(Value::Int(x)), Some(Value::Int(y))) => {
                    Value::Int((*x).max(*y))
                }
                (Some(Value::Word(x)), Some(Value::Word(y))) => {
                    Value::Word((*x).max(*y))
                }
                (Some(Value::Float(x)), Some(Value::Float(y))) => {
                    Value::Float(OrderedFloat(x.0.max(y.0)))
                }
                _ => typechecked!("Math.max", "same Numeric type"),
            };

            Ok(ctx.arena.add(result, ctx.span))
        })
    }

    /// `(Float) -> Int`
    ///
    /// Returns the largest integer less than or equal to x.
    pub(crate) fn floor<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add(Value::Int(n.floor() as i64), ctx.span))
        })
    }

    /// `(Float) -> Int`
    ///
    /// Returns the smallest integer greater than or equal to x.
    pub(crate) fn ceil<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add(Value::Int(n.ceil() as i64), ctx.span))
        })
    }

    /// `(Float) -> Int`
    ///
    /// Rounds to the nearest integer (ties round away from zero).
    pub(crate) fn round<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add(Value::Int(n.round() as i64), ctx.span))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the square root. Returns NaN for negative inputs.
    pub(crate) fn sqrt<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Self::to_float(ctx, args[0]);
            Ok(ctx
                .arena
                .add(Value::Float(OrderedFloat(n.sqrt())), ctx.span))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the natural logarithm. Returns NaN for non-positive inputs.
    pub(crate) fn log<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Self::to_float(ctx, args[0]);
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n.ln())), ctx.span))
        })
    }
}

/// Primitives for the `Math.Trig` submodule.
pub(crate) struct Trig;

impl Prim for Trig {}

impl Trig {
    /// `(Float) -> Float`
    ///
    /// Returns the sine of x (x in radians).
    pub(crate) fn sin<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n.sin())), ctx.span))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the cosine of x (x in radians).
    pub(crate) fn cos<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n.cos())), ctx.span))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the tangent of x (x in radians).
    pub(crate) fn tan<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n.tan())), ctx.span))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arcsine of x (result in radians).
    pub(crate) fn asin<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx
                .arena
                .add(Value::Float(OrderedFloat(n.asin())), ctx.span))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arccosine of x (result in radians).
    pub(crate) fn acos<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx
                .arena
                .add(Value::Float(OrderedFloat(n.acos())), ctx.span))
        })
    }

    /// `(Float) -> Float`
    ///
    /// Returns the arctangent of x (result in radians).
    pub(crate) fn atan<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n = Math::to_float(ctx, args[0]);
            Ok(ctx
                .arena
                .add(Value::Float(OrderedFloat(n.atan())), ctx.span))
        })
    }

    /// `(Float, Float) -> Float`
    ///
    /// Returns the arctangent of `y/x` (result in radians), using signs to
    /// determine the correct quadrant.
    pub(crate) fn atan2<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let y = Math::to_float(ctx, args[0]);
            let x = Math::to_float(ctx, args[1]);
            Ok(ctx
                .arena
                .add(Value::Float(OrderedFloat(y.atan2(x))), ctx.span))
        })
    }
}

/// Primitives for the `Random` module.
pub(crate) struct Random;

impl Prim for Random {}

impl Random {
    /// `() -> Float`
    ///
    /// Returns a random float in the range `[0, 1)`.
    pub(crate) fn random<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let n: f64 = rand::thread_rng().gen();
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n)), ctx.span))
        })
    }

    /// `(Float, Float) -> Float`
    ///
    /// Returns a random float in the range `[min, max)`.
    pub(crate) fn range<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let min = Self::to_float(ctx, args[0]);
            let max = Self::to_float(ctx, args[1]);

            let n: f64 = rand::thread_rng().gen_range(min..max);
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n)), ctx.span))
        })
    }

    /// `(Int, Int) -> Int`
    ///
    /// Returns a random integer in the range `[min, max]` (inclusive).
    pub(crate) fn int<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let min = Self::to_int(ctx, args[0]);
            let max = Self::to_int(ctx, args[1]);

            let n: i64 = rand::thread_rng().gen_range(min..=max);
            Ok(ctx.arena.add(Value::Int(n), ctx.span))
        })
    }

    /// `() -> Bool`
    ///
    /// Returns a random boolean.
    pub(crate) fn bool<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let b: bool = rand::thread_rng().gen();
            Ok(ctx.arena.add(Value::Bool(b), ctx.span))
        })
    }

    /// `forall T. (Array[T]) -> Option[T]`
    ///
    /// Picks a random element from the array. Returns `Option.None` if empty.
    pub(crate) fn choice<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Random.choice", "Array"));

            let result = elems
                .choose(&mut rand::thread_rng())
                .copied()
                .map(|v| ctx.option_some(v))
                .unwrap_or_else(|| ctx.option_none());

            Ok(result)
        })
    }

    /// `forall T. (Array[T]) -> Array[T]`
    ///
    /// Returns a new array with elements in random order.
    pub(crate) fn shuffle<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, mut e) = ctx
                .arena
                .take_array(args[0])
                .unwrap_or_else(|| typechecked!("Random.shuffle", "Array"));

            e.shuffle(&mut rand::thread_rng());
            Ok(ctx.arena.add(Value::Array(ty, Arc::new(e)), ctx.span))
        })
    }

    /// `forall T. (Array[T], Int) -> Result[Array[T], String]`
    ///
    /// Picks `n` random elements without replacement.
    /// Returns `Result.Err` if `n > Iter.length(arr)`.
    pub(crate) fn sample<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Random.sample", "Array"));

            let n = Self::to_int(ctx, args[1]) as usize;

            if n > elems.len() {
                let msg_str = format!(
                    "Random.sample: n ({n}) exceeds array length ({})",
                    elems.len()
                );
                let msg = ctx.arena.intern(&msg_str);
                let msg_val = ctx.arena.add(Value::String(msg), ctx.span);
                Ok(ctx.result_err(msg_val))
            } else {
                let sampled: SmallVec<[ValueId; 4]> = elems
                    .choose_multiple(&mut rand::thread_rng(), n)
                    .copied()
                    .collect();
                let arr = ctx
                    .arena
                    .add(Value::Array(ty, Arc::new(sampled)), ctx.span);
                Ok(ctx.result_ok(arr))
            }
        })
    }

    /// `() -> String`
    ///
    /// Generates a random UUID v4 string.
    pub(crate) fn uuid<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let id = uuid::Uuid::new_v4().to_string();
            let sid = ctx.arena.intern(&id);
            Ok(ctx.arena.add(Value::String(sid), ctx.span))
        })
    }
}

/// Primitives for the `Map` module.
pub(crate) struct Map;

impl Prim for Map {}

impl Map {
    /// `forall K V. () -> Map[K, V]`
    ///
    /// Creates an empty map.
    pub(crate) fn empty<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let k_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
            let v_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
            let map = Value::Map(k_ty, v_ty, Arc::new(IndexMap::new()));
            Ok(ctx.arena.add(map, ctx.span))
        })
    }

    /// `forall K V. (Map[K, V]) -> Int`
    ///
    /// Returns the number of entries in the map.
    pub(crate) fn length<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, _, entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.length", "Map"));

            Ok(ctx.arena.add(Value::Int(entries.len() as i64), ctx.span))
        })
    }

    /// `forall K V. (Map[K, V]) -> Array[K]`
    ///
    /// Returns an array of all keys in iteration order.
    pub(crate) fn keys<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (k_ty, _, entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.keys", "Map"));

            // Collect keys before mutating arena
            let key_vals: SmallVec<[MapKey; 8]> =
                entries.keys().cloned().collect();

            let keys: SmallVec<[ValueId; 4]> = key_vals
                .iter()
                .map(|k| ctx.arena.add(k.to_value(), ctx.span))
                .collect();

            Ok(ctx.arena.add(Value::Array(k_ty, Arc::new(keys)), ctx.span))
        })
    }

    /// `forall K V. (Map[K, V]) -> Array[V]`
    ///
    /// Returns an array of all values in iteration order.
    pub(crate) fn values<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, v_ty, entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.values", "Map"));

            let vals: SmallVec<[ValueId; 4]> =
                entries.values().copied().collect();
            Ok(ctx.arena.add(Value::Array(v_ty, Arc::new(vals)), ctx.span))
        })
    }

    /// `forall K V. (Map[K, V]) -> Array[(K, V)]`
    ///
    /// Returns an array of `(key, value)` tuples in iteration order.
    pub(crate) fn entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (k_ty, v_ty, entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.entries", "Map"));

            // Collect entries before mutating arena
            let entry_pairs: SmallVec<[(MapKey, ValueId); 8]> =
                entries.iter().map(|(k, v)| (k.clone(), *v)).collect();
            let tuple_ty = ctx.type_exprs.tuple(smallvec![k_ty, v_ty]);

            let tuples: SmallVec<[ValueId; 4]> = entry_pairs
                .iter()
                .map(|(k, v_id)| {
                    let k_id = ctx.arena.add(k.to_value(), ctx.span);
                    let tuple = Value::Tuple(
                        tuple_ty,
                        Arc::new(smallvec![k_id, *v_id]),
                    );
                    ctx.arena.add(tuple, ctx.span)
                })
                .collect();

            let arr_ty = ctx.type_exprs.app(TypeId::ARRAY, smallvec![tuple_ty]);
            Ok(ctx
                .arena
                .add(Value::Array(arr_ty, Arc::new(tuples)), ctx.span))
        })
    }

    /// `forall K V. (Map[K, V], K) -> Bool`
    ///
    /// Returns `true` if the key exists in the map.
    pub(crate) fn has<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_value(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.has",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            let (_, _, entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.has", "Map"));

            let exists = entries.contains_key(&map_key);
            Ok(ctx.arena.add(Value::Bool(exists), ctx.span))
        })
    }

    /// `forall K V. (Map[K, V], K) -> Option[V]`
    ///
    /// Returns `Option.Some(value)` if the key exists, `Option.None` otherwise.
    pub(crate) fn get<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_value(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.lookup",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            let (_, _, entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.lookup", "Map"));

            match entries.get(&map_key) {
                Some(v_id) => Ok(ctx.option_some(*v_id)),
                None => Ok(ctx.option_none()),
            }
        })
    }

    /// `forall K V. (Map[K, V], K, V) -> Map[K, V]`
    ///
    /// Returns a new map with the key-value pair inserted/updated.
    /// Validates that the key and value types match the map's types.
    pub(crate) fn set<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            // Get key value and convert to MapKey first
            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_value(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.insert",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            // Type checker guarantees key/value types match the map type
            let (k_ty, v_ty, mut e) = ctx
                .arena
                .take_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.insert", "Map"));

            e.insert(map_key, args[2]);

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, Arc::new(e)), ctx.span))
        })
    }

    /// `forall K V. (Map[K, V], K) -> Map[K, V]`
    ///
    /// Returns a new map with the key removed (if it existed).
    pub(crate) fn remove<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_value(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.remove",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            let (k_ty, v_ty, mut e) = ctx
                .arena
                .take_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.remove", "Map"));

            e.shift_remove(&map_key);

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, Arc::new(e)), ctx.span))
        })
    }

    /// `forall K V. (Map[K, V], Map[K, V]) -> Map[K, V]`
    ///
    /// Returns a new map with entries from both maps (b overrides a).
    ///
    /// Type checker guarantees both maps have compatible key/value types.
    pub(crate) fn merge<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (k_ty, v_ty, mut merged) =
                ctx.arena.take_map(args[0]).unwrap_or_else(|| {
                    typechecked!("Map.merge", "Map (first arg)")
                });

            let (_, _, entries_b) =
                ctx.arena.get_map(args[1]).unwrap_or_else(|| {
                    typechecked!("Map.merge", "Map (second arg)")
                });

            // Type checker guarantees compatible map types
            merged.extend(entries_b.iter().map(|(k, v)| (k.clone(), *v)));

            Ok(ctx
                .arena
                .add(Value::Map(k_ty, v_ty, Arc::new(merged)), ctx.span))
        })
    }

    /// `forall K V. (Array[(K, V)]) -> Map[K, V]`
    ///
    /// Constructs a map from an array of `(key, value)` tuples.
    pub(crate) fn from_entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, arr) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Map.from-entries", "Array"));

            // Infer types from first entry
            let first_entry = arr.first().and_then(|id| ctx.arena.get(*id));
            let (k_ty, v_ty) = match first_entry {
                Some(Value::Tuple(_, elems)) if elems.len() == 2 => {
                    let k_ty = elems
                        .first()
                        .and_then(|id| {
                            ctx.arena.base_type_of(*id, ctx.type_exprs)
                        })
                        .map(|ty| ctx.type_exprs.named(ty))
                        .unwrap_or_else(|| {
                            ctx.type_exprs.named(TypeId::UNKNOWN)
                        });
                    let v_ty = elems
                        .get(1)
                        .and_then(|id| {
                            ctx.arena.base_type_of(*id, ctx.type_exprs)
                        })
                        .map(|ty| ctx.type_exprs.named(ty))
                        .unwrap_or_else(|| {
                            ctx.type_exprs.named(TypeId::UNKNOWN)
                        });
                    (k_ty, v_ty)
                }
                _ => {
                    let k_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
                    let v_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
                    (k_ty, v_ty)
                }
            };

            let mut entries = IndexMap::new();

            // Type checker guarantees array elements are 2-tuples
            arr.iter().for_each(|id| {
                let val = ctx.arena.get(*id).unwrap_or_else(|| {
                    typechecked!("Map.from-entries", "ValueId")
                });

                match val {
                    Value::Tuple(_, elems) if elems.len() == 2 => {
                        let k_val =
                            ctx.arena.get(elems[0]).unwrap_or_else(|| {
                                typechecked!("Map.from-entries", "key ValueId")
                            });
                        let map_key =
                            MapKey::from_value(k_val).unwrap_or_else(|| {
                                typechecked!(
                                    "Map.from-entries",
                                    "key must be scalar"
                                )
                            });
                        entries.insert(map_key, elems[1]);
                    }
                    _ => typechecked!("Map.from-entries", "Array[(K, V)]"),
                }
            });

            Ok(ctx
                .arena
                .add(Value::Map(k_ty, v_ty, Arc::new(entries)), ctx.span))
        })
    }
}

/// Primitives for the `Time` module.
pub(crate) struct Time;

impl Prim for Time {}

impl Time {
    /// `() -> Time`
    ///
    /// Returns the current UTC time.
    pub(crate) fn now<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let now = Utc::now();
            Ok(ctx.arena.add(Value::Time(now), ctx.span))
        })
    }

    /// `() -> Time`
    ///
    /// Returns the Unix epoch (1970-01-01 00:00:00 UTC).
    pub(crate) fn epoch<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let epoch = Utc
                .with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
                .single()
                .ok_or_else(|| ctx.runtime_error("failed to create epoch"))?;
            Ok(ctx.arena.add(Value::Time(epoch), ctx.span))
        })
    }

    /// `(String, String) -> Result[Time, String]`
    ///
    /// Parses a string into a time using strftime format.
    pub(crate) fn parse<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let fmt_sid =
                ctx.arena.get_string_id(args[0]).unwrap_or_else(|| {
                    typechecked!("Time.parse", "String (format)")
                });
            let fmt = ctx
                .arena
                .get_str(fmt_sid)
                .ok_or_else(|| ctx.runtime_error("invalid format string"))?;

            let s_sid = ctx.arena.get_string_id(args[1]).unwrap_or_else(|| {
                typechecked!("Time.parse", "String (input)")
            });
            let s = ctx
                .arena
                .get_str(s_sid)
                .ok_or_else(|| ctx.runtime_error("invalid input string"))?;

            match DateTime::parse_from_str(s, fmt) {
                Ok(dt) => {
                    let utc = dt.with_timezone(&Utc);
                    let time_id = ctx.arena.add(Value::Time(utc), ctx.span);
                    Ok(ctx.result_ok(time_id))
                }
                Err(e) => {
                    let msg = format!("parse error: {e}");
                    let msg_id = ctx.arena.intern(&msg);
                    let err_id = ctx.arena.add(Value::String(msg_id), ctx.span);
                    Ok(ctx.result_err(err_id))
                }
            }
        })
    }

    /// `(String, Time) -> String`
    ///
    /// Formats a time using strftime format.
    pub(crate) fn format<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let fmt_sid =
                ctx.arena.get_string_id(args[0]).unwrap_or_else(|| {
                    typechecked!("Time.format", "String (format)")
                });
            let fmt = ctx
                .arena
                .get_str(fmt_sid)
                .ok_or_else(|| ctx.runtime_error("invalid format string"))?;

            let t = Self::get_time(ctx, args[1], "Time");

            let formatted = t.format(fmt).to_string();
            let sid = ctx.arena.intern(&formatted);
            Ok(ctx.arena.add(Value::String(sid), ctx.span))
        })
    }

    /// `(Time, Int) -> Time`
    ///
    /// Returns a new time with `n` seconds added (negative to subtract).
    pub(crate) fn add_seconds<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            let secs = Self::get_float(ctx, args[1]);

            let duration =
                chrono::Duration::milliseconds((secs * 1000.0) as i64);
            let new_time = t + duration;

            Ok(ctx.arena.add(Value::Time(new_time), ctx.span))
        })
    }

    /// `(Time, Time) -> Float`
    ///
    /// Returns the difference in seconds (`a - b`).
    pub(crate) fn diff_seconds<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = Self::get_time(ctx, args[0], "Time");
            let b = Self::get_time(ctx, args[1], "Time");

            let diff = (a - b).num_milliseconds() as f64 / 1000.0;
            Ok(ctx.arena.add(Value::Float(OrderedFloat(diff)), ctx.span))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn year<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.year() as i64), ctx.span))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn month<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.month() as i64), ctx.span))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn day<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.day() as i64), ctx.span))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn hour<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.hour() as i64), ctx.span))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn minute<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.minute() as i64), ctx.span))
        })
    }

    /// `(Time) -> Int`
    pub(crate) fn second<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.second() as i64), ctx.span))
        })
    }

    /// `(Int) -> Unit`
    ///
    /// Sleeps for `us` microseconds. Blocks execution.
    pub(crate) fn sleep<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let us = ctx
                .arena
                .get(args[0])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| typechecked!("Time.sleep", "Int"));

            let dur = tokio::time::Duration::from_micros(us.max(0) as u64);
            tokio::time::sleep(dur).await;

            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// Helper to extract a `Time` value from an argument.
    fn get_time(
        ctx: &PrimCtx<'_>,
        id: ValueId,
        fn_name: &str,
    ) -> DateTime<Utc> {
        // Type checker guarantees value is Time
        ctx.arena
            .get(id)
            .and_then(|v| match v {
                Value::Time(t) => Some(*t),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!(fn_name, "Time"))
    }

    /// Helper to extract a `Float` value from an argument (accepts Int too).
    ///
    /// Type checker guarantees value is numeric.
    fn get_float(ctx: &PrimCtx<'_>, id: ValueId) -> f64 {
        ctx.arena
            .get(id)
            .and_then(|v| match v {
                Value::Float(f) => Some(f.0),
                Value::Int(n) => Some(*n as f64),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!("get_float", "Numeric"))
    }
}

/// Primitives for the `Option` module.
///
/// Higher-order function `Option.map` is intercepted in `invoke_module_fn`
/// (see `call.rs`); only a placeholder is registered here.
pub(crate) struct Opt;

impl Prim for Opt {}

impl Opt {
    /// `forall T. (Option[T], T) -> T`
    ///
    /// Returns the inner value if `Some`, otherwise returns `default`.
    pub(crate) fn unwrap_or<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let opt = ctx.arena.get(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Option.unwrap-or: invalid value")
            })?;

            let is_some = opt.is_some(ctx.type_exprs);
            let is_none = opt.is_none(ctx.type_exprs);

            match (is_some, is_none) {
                (true, false) => {
                    // Option.Some(v) - return the inner value
                    match opt {
                        Value::Tagged(_, _, ref payloads) => {
                            payloads.first().copied().ok_or_else(|| {
                                ctx.runtime_error(
                                    "Option.unwrap-or: Some has no payload",
                                )
                            })
                        }
                        _ => Err(ctx.runtime_error(
                            "Option.unwrap-or: expected Tagged value",
                        )),
                    }
                }
                (false, true) => {
                    // Option.None - return the default
                    Ok(args[1])
                }
                _ => typechecked!("Option.unwrap-or", "Option"),
            }
        })
    }

    /// `forall T. (Option[Option[T]]) -> Option[T]`
    ///
    /// Flattens a nested `Option`. Returns `Some(v)` if input is `Some(Some(v))`,
    /// otherwise returns `None`.
    pub(crate) fn flatten<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let opt =
                ctx.arena.get(args[0]).cloned().unwrap_or_else(|| {
                    typechecked!("Option.flatten", "valid arg")
                });

            let is_some = opt.is_some(ctx.type_exprs);
            let is_none = opt.is_none(ctx.type_exprs);

            match (is_some, is_none) {
                (true, false) => {
                    // Option.Some(inner) - return the inner Option
                    match opt {
                        Value::Tagged(_, _, ref payloads) => {
                            Ok(*payloads.first().unwrap_or_else(|| {
                                typechecked!("Option.flatten", "Some payload")
                            }))
                        }
                        _ => typechecked!("Option.flatten", "Tagged"),
                    }
                }
                (false, true) => Ok(ctx.option_none()),
                _ => typechecked!("Option.flatten", "Option"),
            }
        })
    }

    /// `forall T E. (E, Option[T]) -> Result[T, E]`
    ///
    /// Converts an `Option` to a `Result`, using the provided error if `None`.
    pub(crate) fn note<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let opt =
                ctx.arena.get(args[1]).cloned().unwrap_or_else(|| {
                    typechecked!("Option.note", "valid arg")
                });

            let is_some = opt.is_some(ctx.type_exprs);
            let is_none = opt.is_none(ctx.type_exprs);

            match (is_some, is_none) {
                (true, false) => {
                    // Option.Some(v) -> Result.Ok(v)
                    match opt {
                        Value::Tagged(_, _, ref payloads) => {
                            let inner =
                                *payloads.first().unwrap_or_else(|| {
                                    typechecked!("Option.note", "Some payload")
                                });
                            Ok(ctx.result_ok(inner))
                        }
                        _ => typechecked!("Option.note", "Tagged"),
                    }
                }
                (false, true) => {
                    // Option.None -> Result.Err(e)
                    Ok(ctx.result_err(args[0]))
                }
                _ => typechecked!("Option.note", "Option"),
            }
        })
    }
}

/// Primitives for the `Result` module.
///
/// Higher-order functions `Result.map` and `Result.map-err` are intercepted
/// in `invoke_module_fn` (see `call.rs`); only placeholders are registered here.
pub(crate) struct Res;

impl Prim for Res {}

impl Res {
    /// `forall T E. (Result[T, E], T) -> T`
    ///
    /// Returns the inner value if `Ok`, otherwise returns `default`.
    pub(crate) fn unwrap_or<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let res = ctx.arena.get(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Result.unwrap-or: invalid value")
            })?;

            let is_ok = res.is_ok(ctx.type_exprs);
            let is_err = res.is_err(ctx.type_exprs);

            match (is_ok, is_err) {
                (true, false) => {
                    // Result.Ok(v) - return the inner value
                    match res {
                        Value::Tagged(_, _, ref payloads) => {
                            payloads.first().copied().ok_or_else(|| {
                                ctx.runtime_error(
                                    "Result.unwrap-or: Ok has no payload",
                                )
                            })
                        }
                        _ => Err(ctx.runtime_error(
                            "Result.unwrap-or: expected Tagged value",
                        )),
                    }
                }
                (false, true) => {
                    // Result.Err - return the default
                    Ok(args[1])
                }
                _ => typechecked!("Result.unwrap-or", "Result"),
            }
        })
    }

    /// `forall T E. (Result[Result[T, E], E]) -> Result[T, E]`
    ///
    /// Flattens a nested `Result`. Returns `Ok(v)` if input is `Ok(Ok(v))`,
    /// `Err(e)` if input is `Ok(Err(e))` or `Err(e)`.
    pub(crate) fn flatten<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let res =
                ctx.arena.get(args[0]).cloned().unwrap_or_else(|| {
                    typechecked!("Result.flatten", "valid arg")
                });

            let is_ok = res.is_ok(ctx.type_exprs);
            let is_err = res.is_err(ctx.type_exprs);

            match (is_ok, is_err) {
                (true, false) => {
                    // Result.Ok(inner) - return the inner Result
                    match res {
                        Value::Tagged(_, _, ref payloads) => {
                            Ok(*payloads.first().unwrap_or_else(|| {
                                typechecked!("Result.flatten", "Ok payload")
                            }))
                        }
                        _ => typechecked!("Result.flatten", "Tagged"),
                    }
                }
                (false, true) => Ok(args[0]),
                _ => typechecked!("Result.flatten", "Result"),
            }
        })
    }

    /// `forall T E. (Result[T, E]) -> Option[T]`
    ///
    /// Converts a `Result` to an `Option`, discarding the error if `Err`.
    pub(crate) fn hush<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let res =
                ctx.arena.get(args[0]).cloned().unwrap_or_else(|| {
                    typechecked!("Result.hush", "valid arg")
                });

            let is_ok = res.is_ok(ctx.type_exprs);
            let is_err = res.is_err(ctx.type_exprs);

            match (is_ok, is_err) {
                (true, false) => {
                    // Result.Ok(v) -> Option.Some(v)
                    match res {
                        Value::Tagged(_, _, ref payloads) => {
                            let inner =
                                *payloads.first().unwrap_or_else(|| {
                                    typechecked!("Result.hush", "Ok payload")
                                });
                            Ok(ctx.option_some(inner))
                        }
                        _ => typechecked!("Result.hush", "Tagged"),
                    }
                }
                (false, true) => {
                    // Result.Err(_) -> Option.None
                    Ok(ctx.option_none())
                }
                _ => typechecked!("Result.hush", "Result"),
            }
        })
    }
}

/// Primitives for the `Io` module.
///
/// Provides effectful I/O operations using `tokio` for async stdin/stdout/stderr.
pub(crate) struct Io;

impl Prim for Io {}

impl Io {
    /// `() -> String`
    ///
    /// Reads a line from stdin (blocking until newline). Returns the line
    /// without the trailing newline character.
    pub(crate) fn get_line<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            use tokio::io::AsyncBufReadExt;

            let stdin = tokio::io::stdin();
            let mut reader = tokio::io::BufReader::new(stdin);
            let mut line = String::new();

            reader
                .read_line(&mut line)
                .await
                .map_err(|e| ctx.runtime_error(format!("Io.get-line: {e}")))?;

            // Remove trailing newline
            line.truncate(line.trim_end_matches(['\n', '\r']).len());

            let sid = ctx.arena.intern(&line);
            Ok(ctx.arena.add(Value::String(sid), ctx.span))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stdout without a trailing newline.
    pub(crate) fn print<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("Io.print", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stdout(&s, ctx.span).await?;
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stdout with a trailing newline.
    pub(crate) fn println<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("Io.println", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stdoutline(&s, ctx.span).await?;
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stderr without a trailing newline.
    pub(crate) fn eprint<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("Io.eprint", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stderr(&s, ctx.span).await?;
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(String) -> Unit`
    ///
    /// Prints a string to stderr with a trailing newline.
    pub(crate) fn eprintln<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let sid = ctx
                .arena
                .get_string_id(args[0])
                .unwrap_or_else(|| typechecked!("Io.eprintln", "String"));
            let s = Self::valid_str(ctx.arena, sid).to_owned();

            ctx.io.stderrline(&s, ctx.span).await?;
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }
}

/// Primitives for the `Io.Directory` submodule.
///
/// Provides file system operations using `tokio::fs` for async I/O.
pub(crate) struct Directory;

impl Prim for Directory {}

impl Directory {
    /// Helper: extract path string from a `Value::FilePath`.
    fn get_path_str(ctx: &PrimCtx<'_>, id: ValueId) -> String {
        ctx.arena
            .get(id)
            .and_then(|v| match v {
                Value::FilePath(sid) => ctx.arena.get_str(*sid),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!("Io.Directory", "FilePath"))
            .to_owned()
    }

    /// Helper: create a `Path.File(filepath)` value.
    fn make_path_file(ctx: &mut PrimCtx<'_>, path_str: &str) -> ValueId {
        let sid = ctx.arena.intern(path_str);
        let fp_id = ctx.arena.add(Value::FilePath(sid), ctx.span);
        let path_ty = ctx.type_exprs.named(TypeId::PATH);
        ctx.arena
            .add(Value::Tagged(path_ty, 0, smallvec![fp_id]), ctx.span)
    }

    /// Helper: create a `Path.Dir(filepath)` value.
    fn make_path_dir(ctx: &mut PrimCtx<'_>, path_str: &str) -> ValueId {
        let sid = ctx.arena.intern(path_str);
        let fp_id = ctx.arena.add(Value::FilePath(sid), ctx.span);
        let path_ty = ctx.type_exprs.named(TypeId::PATH);
        ctx.arena
            .add(Value::Tagged(path_ty, 1, smallvec![fp_id]), ctx.span)
    }

    /// `(FilePath) -> Array[Path]`
    ///
    /// Lists the contents of a directory.
    pub(crate) fn list_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let mut entries =
                tokio::fs::read_dir(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.list-dir: {e}"))
                })?;

            let mut paths: SmallVec<[ValueId; 4]> = SmallVec::new();
            loop {
                match entries.next_entry().await {
                    Ok(Some(entry)) => {
                        let p = entry.path();
                        let path_string = p.to_string_lossy().to_string();
                        let path_id = match entry.file_type().await {
                            Ok(ft) if ft.is_dir() => {
                                Self::make_path_dir(ctx, &path_string)
                            }
                            _ => Self::make_path_file(ctx, &path_string),
                        };
                        paths.push(path_id);
                    }
                    Ok(None) => break,
                    Err(e) => Err(ctx
                        .runtime_error(format!("Io.Directory.list-dir: {e}")))?,
                }
            }

            let path_ty = ctx.type_exprs.named(TypeId::PATH);
            Ok(ctx
                .arena
                .add(Value::Array(path_ty, Arc::new(paths)), ctx.span))
        })
    }

    /// `({ src: FilePath, dest: FilePath }) -> Unit`
    ///
    /// Moves or renames a file or directory.
    pub(crate) fn move_path<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.get(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.move-path: invalid object")
            })?;

            let (src, dest) = match obj {
                Value::Object(fields) => {
                    let src_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.move-path: missing src",
                            )
                        })?;
                    let dest_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.move-path: missing dest",
                            )
                        })?;
                    (
                        Self::get_path_str(ctx, src_id),
                        Self::get_path_str(ctx, dest_id).to_string(),
                    )
                }
                _ => typechecked!(
                    "Io.Directory.move-path",
                    "{ src: FilePath, dest: FilePath }"
                ),
            };

            tokio::fs::rename(&src, &dest).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.move-path: {e}"))
            })?;

            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `({ src: FilePath, dest: FilePath }) -> Unit`
    ///
    /// Copies a file. For directories, use recursive copy (not yet implemented).
    pub(crate) fn copy_path<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.get(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.copy-path: invalid object")
            })?;

            let (src, dest) = match obj {
                Value::Object(fields) => {
                    let src_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.copy-path: missing src",
                            )
                        })?;
                    let dest_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.copy-path: missing dest",
                            )
                        })?;
                    (
                        Self::get_path_str(ctx, src_id),
                        Self::get_path_str(ctx, dest_id).to_string(),
                    )
                }
                _ => typechecked!(
                    "Io.Directory.copy-path",
                    "{ src: FilePath, dest: FilePath }"
                ),
            };

            tokio::fs::copy(&src, &dest).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.copy-path: {e}"))
            })?;

            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Removes a file or empty directory.
    pub(crate) fn remove<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);

            // Try removing as file first, then as directory
            let result = tokio::fs::remove_file(&path_str).await;
            match result {
                Ok(()) => Ok(ctx.arena.add(Value::Unit, ctx.span)),
                Err(_) => {
                    tokio::fs::remove_dir(&path_str).await.map_err(|e| {
                        ctx.runtime_error(format!("Io.Directory.remove: {e}"))
                    })?;
                    Ok(ctx.arena.add(Value::Unit, ctx.span))
                }
            }
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Recursively removes a file or directory.
    pub(crate) fn remove_all<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);

            // Try removing as file first
            let result = tokio::fs::remove_file(&path_str).await;
            match result {
                Ok(()) => Ok(ctx.arena.add(Value::Unit, ctx.span)),
                Err(_) => {
                    tokio::fs::remove_dir_all(&path_str).await.map_err(
                        |e| {
                            ctx.runtime_error(format!(
                                "Io.Directory.remove-all: {e}"
                            ))
                        },
                    )?;
                    Ok(ctx.arena.add(Value::Unit, ctx.span))
                }
            }
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path exists.
    pub(crate) fn exists<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let exists =
                tokio::fs::try_exists(&path_str).await.unwrap_or(false);
            Ok(ctx.arena.add(Value::Bool(exists), ctx.span))
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path is a file.
    pub(crate) fn is_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let is_file = tokio::fs::metadata(&path_str)
                .await
                .map(|m| m.is_file())
                .unwrap_or(false);
            Ok(ctx.arena.add(Value::Bool(is_file), ctx.span))
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path is a directory.
    pub(crate) fn is_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let is_dir = tokio::fs::metadata(&path_str)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false);
            Ok(ctx.arena.add(Value::Bool(is_dir), ctx.span))
        })
    }

    /// `(FilePath) -> String`
    ///
    /// Reads the entire contents of a file as a string.
    pub(crate) fn read_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let contents =
                tokio::fs::read_to_string(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.read-file: {e}"))
                })?;
            let sid = ctx.arena.intern(&contents);
            Ok(ctx.arena.add(Value::String(sid), ctx.span))
        })
    }

    /// `({ path: FilePath, contents: String }) -> Unit`
    ///
    /// Writes a string to a file, creating or overwriting it.
    pub(crate) fn write_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.get(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.write-file: invalid object")
            })?;

            let (path, contents) = match obj {
                Value::Object(fields) => {
                    let path_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.write-file: missing path",
                            )
                        })?;
                    let contents_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.write-file: missing contents",
                            )
                        })?;
                    let path_str = Self::get_path_str(ctx, path_id).to_string();
                    let contents_str = ctx
                        .arena
                        .get_string_id(contents_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.write-file", "String")
                        })
                        .to_string();
                    (path_str, contents_str)
                }
                _ => typechecked!(
                    "Io.Directory.write-file",
                    "{ path: FilePath, contents: String }"
                ),
            };

            tokio::fs::write(&path, &contents).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.write-file: {e}"))
            })?;

            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `({ path: FilePath, contents: String }) -> Unit`
    ///
    /// Appends a string to a file.
    pub(crate) fn append_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;

            let obj = ctx.arena.get(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.append-file: invalid object")
            })?;

            let (path, contents) = match obj {
                Value::Object(fields) => {
                    let path_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.append-file: missing path",
                            )
                        })?;
                    let contents_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.append-file: missing contents",
                            )
                        })?;
                    let path_str = Self::get_path_str(ctx, path_id).to_string();
                    let contents_str = ctx
                        .arena
                        .get_string_id(contents_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.append-file", "String")
                        })
                        .to_string();
                    (path_str, contents_str)
                }
                _ => typechecked!(
                    "Io.Directory.append-file",
                    "{ path: FilePath, contents: String }"
                ),
            };

            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.append-file: {e}"))
                })?;

            file.write_all(contents.as_bytes()).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.append-file: {e}"))
            })?;

            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Creates a directory.
    pub(crate) fn create_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            tokio::fs::create_dir(&path_str).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.create-dir: {e}"))
            })?;
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Creates a directory and all parent directories.
    pub(crate) fn create_dir_all<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            tokio::fs::create_dir_all(&path_str).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.create-dir-all: {e}"))
            })?;
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `() -> FilePath`
    ///
    /// Returns the current working directory.
    pub(crate) fn pwd<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let cwd = std::env::current_dir().map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.pwd: {e}"))
            })?;
            let path_str = cwd.to_string_lossy();
            let sid = ctx.arena.intern(&path_str);
            Ok(ctx.arena.add(Value::FilePath(sid), ctx.span))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Changes the current working directory.
    pub(crate) fn set_pwd<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            std::env::set_current_dir(&path_str).map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.set-pwd: {e}"))
            })?;
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(String) -> Option[String]`
    ///
    /// Gets an environment variable.
    pub(crate) fn get_env<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let name_sid =
                ctx.arena.get_string_id(args[0]).unwrap_or_else(|| {
                    typechecked!("Io.Directory.get-env", "String")
                });
            let name = Self::valid_str(ctx.arena, name_sid);

            let str_ty = ctx.type_exprs.named(TypeId::STRING);
            let opt_ty = ctx.type_exprs.app(TypeId::OPTION, smallvec![str_ty]);

            match std::env::var(name) {
                Ok(val) => {
                    let sid = ctx.arena.intern(&val);
                    let val_id = ctx.arena.add(Value::String(sid), ctx.span);
                    Ok(ctx.arena.add(Value::some(opt_ty, val_id), ctx.span))
                }
                Err(_) => Ok(ctx.arena.add(Value::none(opt_ty), ctx.span)),
            }
        })
    }

    /// `({ name: String, value: String }) -> Unit`
    ///
    /// Sets an environment variable.
    pub(crate) fn set_env<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.get(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.set-env: invalid object")
            })?;

            let (name, value) = match obj {
                Value::Object(fields) => {
                    let name_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.set-env: missing name",
                            )
                        })?;
                    let value_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.set-env: missing value",
                            )
                        })?;
                    let name_str = ctx
                        .arena
                        .get_string_id(name_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.set-env", "String")
                        })
                        .to_string();
                    let value_str = ctx
                        .arena
                        .get_string_id(value_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.set-env", "String")
                        })
                        .to_string();
                    (name_str, value_str)
                }
                _ => typechecked!(
                    "Io.Directory.set-env",
                    "{ name: String, value: String }"
                ),
            };

            std::env::set_var(&name, &value);
            Ok(ctx.arena.add(Value::Unit, ctx.span))
        })
    }

    /// `(FilePath) -> FilePath`
    ///
    /// Resolves a path to its absolute, canonical form.
    pub(crate) fn canonicalize<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let canonical =
                tokio::fs::canonicalize(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.canonicalize: {e}"))
                })?;
            let sid = ctx.arena.intern(&canonical.to_string_lossy());
            Ok(ctx.arena.add(Value::FilePath(sid), ctx.span))
        })
    }

    /// `(FilePath) -> Option[FilePath]`
    ///
    /// Returns the parent directory of a path.
    pub(crate) fn parent<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let path = std::path::Path::new(&path_str);

            let fp_ty = ctx.type_exprs.named(TypeId::FILEPATH);
            let opt_ty = ctx.type_exprs.app(TypeId::OPTION, smallvec![fp_ty]);

            match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => {
                    let sid = ctx.arena.intern(&p.to_string_lossy());
                    let fp = ctx.arena.add(Value::FilePath(sid), ctx.span);
                    Ok(ctx.arena.add(Value::some(opt_ty, fp), ctx.span))
                }
                _ => Ok(ctx.arena.add(Value::none(opt_ty), ctx.span)),
            }
        })
    }

    /// `(FilePath) -> Option[String]`
    ///
    /// Returns the final component of a path.
    pub(crate) fn file_name<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let path = std::path::Path::new(&path_str);
            let name_opt =
                path.file_name().map(|n| n.to_string_lossy().to_string());

            let str_ty = ctx.type_exprs.named(TypeId::STRING);
            let opt_ty = ctx.type_exprs.app(TypeId::OPTION, smallvec![str_ty]);

            match name_opt {
                Some(name) => {
                    let sid = ctx.arena.intern(&name);
                    let s = ctx.arena.add(Value::String(sid), ctx.span);
                    Ok(ctx.arena.add(Value::some(opt_ty, s), ctx.span))
                }
                None => Ok(ctx.arena.add(Value::none(opt_ty), ctx.span)),
            }
        })
    }

    /// `(FilePath) -> Option[String]`
    ///
    /// Returns the file extension, if any.
    pub(crate) fn extension<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let path = std::path::Path::new(&path_str);
            let ext_opt =
                path.extension().map(|e| e.to_string_lossy().to_string());

            let str_ty = ctx.type_exprs.named(TypeId::STRING);
            let opt_ty = ctx.type_exprs.app(TypeId::OPTION, smallvec![str_ty]);

            match ext_opt {
                Some(ext) => {
                    let sid = ctx.arena.intern(&ext);
                    let s = ctx.arena.add(Value::String(sid), ctx.span);
                    Ok(ctx.arena.add(Value::some(opt_ty, s), ctx.span))
                }
                None => Ok(ctx.arena.add(Value::none(opt_ty), ctx.span)),
            }
        })
    }

    /// `(FilePath, Array[String]) -> FilePath`
    ///
    /// Joins path components.
    pub(crate) fn join<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let base_str = Self::get_path_str(ctx, args[0]);

            let parts = ctx.arena.get(args[1]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.join: invalid array")
            })?;

            // Collect all parts as owned strings first
            let part_strs: Vec<String> = match parts {
                Value::Array(_, elems) => elems
                    .iter()
                    .map(|elem_id| {
                        let sid = ctx
                            .arena
                            .get_string_id(*elem_id)
                            .unwrap_or_else(|| {
                                typechecked!(
                                    "Io.Directory.join",
                                    "String element"
                                )
                            });
                        Self::valid_str(ctx.arena, sid).to_owned()
                    })
                    .collect(),
                _ => typechecked!("Io.Directory.join", "Array[String]"),
            };

            let mut path = std::path::PathBuf::from(&base_str);
            part_strs.iter().for_each(|p| path.push(p));

            let sid = ctx.arena.intern(&path.to_string_lossy());
            Ok(ctx.arena.add(Value::FilePath(sid), ctx.span))
        })
    }

    /// `() -> FilePath`
    ///
    /// Returns the system temporary directory.
    pub(crate) fn temp_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let tmp = std::env::temp_dir();
            let sid = ctx.arena.intern(&tmp.to_string_lossy());
            Ok(ctx.arena.add(Value::FilePath(sid), ctx.span))
        })
    }

    /// `(FilePath, String) -> FilePath`
    ///
    /// Returns a new path with the given extension.
    pub(crate) fn with_extension<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let ext = ctx
                .arena
                .get_string_id(args[1])
                .and_then(|sid| ctx.arena.get_str(sid))
                .map(String::from)
                .ok_or_else(|| {
                    ctx.runtime_error(
                        "Io.Directory.with-extension: invalid extension",
                    )
                })?;

            let mut path = std::path::PathBuf::from(&path_str);
            path.set_extension(&ext);

            let sid = ctx.arena.intern(&path.to_string_lossy());
            Ok(ctx.arena.add(Value::FilePath(sid), ctx.span))
        })
    }
}

/// Primitives for the `Prelude` module.
///
/// `foreach` is a HoF intercepted in `invoke_module_fn`; only a placeholder
/// is registered here. `contains` and `reverse` are real primitives.
pub(crate) struct Prelude;

impl Prim for Prelude {}

impl Prelude {
    /// `forall T, F: Iterable. (F[T], T) -> Bool`
    ///
    /// Checks if `needle` is contained in the iterable.
    pub(crate) fn contains<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let haystack = ctx
                .arena
                .get(args[0])
                .cloned()
                .unwrap_or_else(|| typechecked!("contains", "haystack"));
            let needle = ctx
                .arena
                .get(args[1])
                .cloned()
                .unwrap_or_else(|| typechecked!("contains", "needle"));
            let found = match &haystack {
                Value::Array(_, elems) => elems.iter().any(|eid| {
                    ctx.arena.get(*eid).is_some_and(|v| *v == needle)
                }),
                Value::Range {
                    start,
                    end,
                    inclusive,
                } => match &needle {
                    Value::Int(n) => {
                        if *inclusive {
                            *n >= *start && *n <= *end
                        } else {
                            *n >= *start && *n < *end
                        }
                    }
                    _ => false,
                },
                _ => typechecked!("contains", "Iterable"),
            };
            Ok(ctx.arena.add(Value::Bool(found), ctx.span))
        })
    }

    /// `forall T. (Array[T] | Range) -> Array[T] | Range`
    ///
    /// Reverses an array or range. Range reversal swaps bounds
    /// without materialization.
    pub(crate) fn reverse<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let v = ctx
                .arena
                .get(args[0])
                .cloned()
                .unwrap_or_else(|| typechecked!("reverse", "value"));
            let res = match v {
                Value::Array(ty, elems) => {
                    let reversed: SmallVec<[ValueId; 4]> =
                        elems.iter().rev().copied().collect();
                    Value::Array(ty, Arc::new(reversed))
                }
                Value::Range {
                    start,
                    end,
                    inclusive,
                } => {
                    if inclusive {
                        Value::Range {
                            start: end,
                            end: start,
                            inclusive: true,
                        }
                    } else {
                        // `start .. end` reversed is `end - 1 ..= start`
                        Value::Range {
                            start: end - 1,
                            end: start,
                            inclusive: true,
                        }
                    }
                }
                _ => typechecked!("reverse", "Array or Range"),
            };
            Ok(ctx.arena.add(res, ctx.span))
        })
    }
}
