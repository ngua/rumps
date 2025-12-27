//! Built-in primitive functions organized by module.
//!
//! Primitives are callable built-in functions registered in the environment.
//! They are implemented as associated functions on module types ([`Array`],
//! [`Str`], etc.), returning a future that resolves to a `ValueId`. Unlike
//! keywords (GET, SET, KILL), primitives use standard function call syntax
//! and are case-insensitive.
//!
//! # Module Organization
//!
//! Each RUMPS module is a separate type implementing the [`Prim`] trait:
//! - [`Array`]: `length`, `push`, `pop`, `head`, `tail`, `reverse`, `sort`,
//!   `slice`, `contains`, `concat`, plus higher-order functions (see below)
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
use crate::Error;

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
    /// Placeholder for higher-order functions (`Array.map`, `Array.filter`, etc.).
    ///
    /// This should never be called directly; `invoke_module_fn` intercepts
    /// these calls and handles them specially. If this is called, it indicates
    /// a bug in the dispatch logic.
    fn placeholder<'a>(
        _ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Err(Error::runtime_no_span(
                "internal error: HoF placeholder called directly; \
                 this should be intercepted by invoke_module_fn",
            ))
        })
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
// NOTE: Array functions (map, filter, reduce) are higher-order and require
// access to the interpreter's closure invocation machinery. They are
// implemented in `interpreter/call.rs` and registered here as placeholders.
//
// The placeholders ensure `Environment::module_fn_exists` returns true during
// name resolution. The actual dispatch is intercepted in `invoke_module_fn`.
pub(crate) struct Array;

impl Prim for Array {}

impl Array {
    /// `Array.length(arr) -> Int`
    ///
    /// Returns the number of elements in the array.
    pub(crate) fn length<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.length", "Array"));

            Ok(ctx.arena.add(Value::Int(elems.len() as i64), ctx.span))
        })
    }

    /// `Array.push(arr, val) -> Array[T]`
    ///
    /// Returns a new array with `val` appended to the end.
    pub(crate) fn push<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, mut elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.push", "Array"));

            elems.push(args[1]);
            Ok(ctx.arena.add(Value::Array(ty, elems), ctx.span))
        })
    }

    /// `Array.pop(arr) -> Array[T]`
    ///
    /// Returns a new array with the last element removed.
    /// Returns an empty array if the input is empty.
    pub(crate) fn pop<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, mut elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.pop", "Array"));

            elems.pop();
            Ok(ctx.arena.add(Value::Array(ty, elems), ctx.span))
        })
    }

    /// `Array.head(arr) -> Option[T]`
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

    /// `Array.tail(arr) -> Array[T]`
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
            Ok(ctx.arena.add(Value::Array(ty, tail), ctx.span))
        })
    }

    /// `Array.reverse(arr) -> Array[T]`
    ///
    /// Returns a new array with elements in reverse order.
    pub(crate) fn reverse<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.reverse", "Array"));

            let reversed: SmallVec<[ValueId; 4]> =
                elems.iter().rev().copied().collect();
            Ok(ctx.arena.add(Value::Array(ty, reversed), ctx.span))
        })
    }

    /// `Array.sort(arr) -> Array[T]`
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

                    // Unit, closures, functions, module functions, and ranges are not comparable
                    Value::Unit
                    | Value::Closure { .. }
                    | Value::Function { .. }
                    | Value::ModuleFn { .. }
                    | Value::Range { .. } => None,
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
            Ok(ctx.arena.add(Value::Array(ty, sorted), ctx.span))
        })
    }

    /// `Array.slice(arr, start, end) -> Array[T]`
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

            Ok(ctx.arena.add(Value::Array(ty, sliced), ctx.span))
        })
    }

    /// `Array.contains(arr, val) -> Bool`
    ///
    /// Returns `true` if the array contains the given value.
    pub(crate) fn contains<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.contains", "Array"));

            let needle = ctx.arena.get(args[1]).ok_or_else(|| {
                ctx.runtime_error("Array.contains: invalid value")
            })?;

            let found = elems.iter().any(|elem_id| {
                ctx.arena.get(*elem_id).is_some_and(|v| v == needle)
            });

            Ok(ctx.arena.add(Value::Bool(found), ctx.span))
        })
    }

    /// `Array.concat(a, b) -> Array[T]`
    ///
    /// Returns a new array with elements of `b` appended to `a`.
    /// Both arrays must have the same element type.
    pub(crate) fn concat<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty_a, elems_a) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Array.concat", "Array"));

            let (_, elems_b) = ctx
                .arena
                .get_array(args[1])
                .unwrap_or_else(|| typechecked!("Array.concat", "Array"));

            // Type checker guarantees both arrays have matching element types
            let mut combined = elems_a;
            combined.extend(elems_b);
            Ok(ctx.arena.add(Value::Array(ty_a, combined), ctx.span))
        })
    }

    /// `Array.zip(a, b) -> Array[(T, U)]`
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

            let pairs: SmallVec<[ValueId; 4]> = elems_a
                .iter()
                .zip(elems_b.iter())
                .map(|(a, b)| {
                    let tup_ty = ctx.type_exprs.tuple(smallvec![ty_a, ty_b]);
                    let tup = Value::Tuple(tup_ty, smallvec![*a, *b]);
                    ctx.arena.add(tup, ctx.span)
                })
                .collect();

            let elem_ty = ctx.type_exprs.tuple(smallvec![ty_a, ty_b]);
            Ok(ctx.arena.add(Value::Array(elem_ty, pairs), ctx.span))
        })
    }

    /// `Array.unzip(arr) -> (Array[T], Array[U])`
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

            let arr_a_id = ctx.arena.add(Value::Array(ty_a, firsts), ctx.span);
            let arr_b_id = ctx.arena.add(Value::Array(ty_b, seconds), ctx.span);

            let arr_ty_a = ctx.type_exprs.app(TypeId::ARRAY, smallvec![ty_a]);
            let arr_ty_b = ctx.type_exprs.app(TypeId::ARRAY, smallvec![ty_b]);
            let tup_ty = ctx.type_exprs.tuple(smallvec![arr_ty_a, arr_ty_b]);

            Ok(ctx.arena.add(
                Value::Tuple(tup_ty, smallvec![arr_a_id, arr_b_id]),
                ctx.span,
            ))
        })
    }

    /// `Array.intersperse(sep, arr) -> Array[T]`
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

            let result: SmallVec<[ValueId; 4]> = elems
                .iter()
                .enumerate()
                .flat_map(|(i, elem)| -> SmallVec<[ValueId; 2]> {
                    if i == 0 {
                        smallvec![*elem]
                    } else {
                        smallvec![sep, *elem]
                    }
                })
                .collect();

            Ok(ctx.arena.add(Value::Array(ty, result), ctx.span))
        })
    }
}

/// Primitives for the `String` module.
///
/// Named `Str` to avoid collision with Rust's `String`.
pub(crate) struct Str;

impl Prim for Str {}

impl Str {
    /// `String.length(s) -> Int`
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

            let s = ctx.arena.get_str(sid).ok_or_else(|| {
                ctx.runtime_error("String.length: invalid string")
            })?;

            let len = s.graphemes(true).count() as i64;
            Ok(ctx.arena.add(Value::Int(len), ctx.span))
        })
    }

    /// `String.upper(s) -> String`
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

            let s = ctx.arena.get_str(sid).ok_or_else(|| {
                ctx.runtime_error("String.upper: invalid string")
            })?;

            let upper = s.to_uppercase();
            let new_sid = ctx.arena.intern(&upper);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `String.lower(s) -> String`
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

            let s = ctx.arena.get_str(sid).ok_or_else(|| {
                ctx.runtime_error("String.lower: invalid string")
            })?;

            let lower = s.to_lowercase();
            let new_sid = ctx.arena.intern(&lower);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `String.trim(s) -> String`
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
            let trimmed = ctx
                .arena
                .get_str(sid)
                .ok_or_else(|| {
                    ctx.runtime_error("String.trim: invalid string")
                })?
                .trim()
                .to_owned();

            let new_sid = ctx.arena.intern(&trimmed);
            Ok(ctx.arena.add(Value::String(new_sid), ctx.span))
        })
    }

    /// `String.split(s, delim) -> Array[String]`
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
            let s = ctx
                .arena
                .get_str(s_sid)
                .ok_or_else(|| {
                    ctx.runtime_error("String.split: invalid string")
                })?
                .to_owned();

            let d = ctx
                .arena
                .get_str(d_sid)
                .ok_or_else(|| {
                    ctx.runtime_error("String.split: invalid delimiter")
                })?
                .to_owned();

            // Split and collect parts; intern each part
            let parts: SmallVec<[ValueId; 4]> = s
                .split(&d)
                .map(|part| {
                    let part_sid = ctx.arena.intern(part);
                    ctx.arena.add(Value::String(part_sid), ctx.span)
                })
                .collect();

            let str_ty = ctx.type_exprs.named(TypeId::STRING);
            Ok(ctx.arena.add(Value::Array(str_ty, parts), ctx.span))
        })
    }

    /// `String.join(arr, delim) -> String`
    ///
    /// Joins an array of strings with the delimiter.
    pub(crate) fn join<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("String.join", "Array"));
            let elems = elems.clone();

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

    /// `String.slice(s, start, end) -> String`
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

            let s = ctx.arena.get_str(sid).ok_or_else(|| {
                ctx.runtime_error("String.slice: invalid string")
            })?;

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

    /// `String.contains(s, sub) -> Bool`
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

            let s = ctx.arena.get_str(s_sid).ok_or_else(|| {
                ctx.runtime_error("String.contains: invalid string")
            })?;

            let sub = ctx.arena.get_str(sub_sid).ok_or_else(|| {
                ctx.runtime_error("String.contains: invalid substring")
            })?;

            Ok(ctx.arena.add(Value::Bool(s.contains(sub)), ctx.span))
        })
    }

    /// `String.replace(s, old, new) -> String`
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

            let s = ctx.arena.get_str(s_sid).ok_or_else(|| {
                ctx.runtime_error("String.replace: invalid string")
            })?;

            let old = ctx.arena.get_str(old_sid).ok_or_else(|| {
                ctx.runtime_error("String.replace: invalid pattern")
            })?;

            let new = ctx.arena.get_str(new_sid).ok_or_else(|| {
                ctx.runtime_error("String.replace: invalid replacement")
            })?;

            let replaced = s.replace(old, new);
            let result_sid = ctx.arena.intern(&replaced);
            Ok(ctx.arena.add(Value::String(result_sid), ctx.span))
        })
    }
}

/// Primitives for the `Math` module.
pub(crate) struct Math;

impl Prim for Math {}

impl Math {
    /// `Math.abs(x) -> Number`
    ///
    /// Returns the absolute value. Works on Int or Float.
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
                Value::Float(f) => Value::Float(OrderedFloat(f.0.abs())),
                _ => typechecked!("Math.abs", "Int or Float"),
            };

            Ok(ctx.arena.add(result, ctx.span))
        })
    }

    /// `Math.min(a, b) -> Number`
    ///
    /// Returns the minimum of two numbers. Coerces to Float if mixed types.
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
                _ => {
                    let x = Self::to_float(ctx, args[0]);
                    let y = Self::to_float(ctx, args[1]);
                    Value::Float(OrderedFloat(x.min(y)))
                }
            };

            Ok(ctx.arena.add(result, ctx.span))
        })
    }

    /// `Math.max(a, b) -> Number`
    ///
    /// Returns the maximum of two numbers. Coerces to Float if mixed types.
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
                _ => {
                    let x = Self::to_float(ctx, args[0]);
                    let y = Self::to_float(ctx, args[1]);
                    Value::Float(OrderedFloat(x.max(y)))
                }
            };

            Ok(ctx.arena.add(result, ctx.span))
        })
    }

    /// `Math.floor(x) -> Int`
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

    /// `Math.ceil(x) -> Int`
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

    /// `Math.round(x) -> Int`
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

    /// `Math.sqrt(x) -> Float`
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

    /// `Math.log(x) -> Float`
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
    /// `Math.Trig.sin(x) -> Float`
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

    /// `Math.Trig.cos(x) -> Float`
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

    /// `Math.Trig.tan(x) -> Float`
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

    /// `Math.Trig.asin(x) -> Float`
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

    /// `Math.Trig.acos(x) -> Float`
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

    /// `Math.Trig.atan(x) -> Float`
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

    /// `Math.Trig.atan2(y, x) -> Float`
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
    /// `Random.random() -> Float`
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

    /// `Random.range(min, max) -> Float`
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

    /// `Random.int(min, max) -> Int`
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

    /// `Random.bool() -> Bool`
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

    /// `Random.choice(arr) -> Option[T]`
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

    /// `Random.shuffle(arr) -> Array[T]`
    ///
    /// Returns a new array with elements in random order.
    pub(crate) fn shuffle<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (ty, mut elems) = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Random.shuffle", "Array"));

            elems.shuffle(&mut rand::thread_rng());
            Ok(ctx.arena.add(Value::Array(ty, elems), ctx.span))
        })
    }

    /// `Random.sample(arr, n) -> Result[Array[T], String]`
    ///
    /// Picks `n` random elements without replacement.
    /// Returns `Result.Err` if `n > Array.length(arr)`.
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
                let arr = ctx.arena.add(Value::Array(ty, sampled), ctx.span);
                Ok(ctx.result_ok(arr))
            }
        })
    }

    /// `Random.uuid() -> String`
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
    /// `Map.empty() -> Map[Unknown, Unknown]`
    ///
    /// Creates an empty map.
    pub(crate) fn empty<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let k_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
            let v_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
            let map = Value::Map(k_ty, v_ty, IndexMap::new());
            Ok(ctx.arena.add(map, ctx.span))
        })
    }

    /// `Map.length(m) -> Int`
    ///
    /// Returns the number of entries in the map.
    pub(crate) fn length<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, _, entries) = ctx
                .arena
                .get_map_ref(args[0])
                .unwrap_or_else(|| typechecked!("Map.length", "Map"));

            Ok(ctx.arena.add(Value::Int(entries.len() as i64), ctx.span))
        })
    }

    /// `Map.keys(m) -> Array[K]`
    ///
    /// Returns an array of all keys in iteration order.
    pub(crate) fn keys<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (k_ty, _, entries) = ctx
                .arena
                .get_map_ref(args[0])
                .unwrap_or_else(|| typechecked!("Map.keys", "Map"));

            // Collect keys before mutating arena
            let key_vals: SmallVec<[MapKey; 8]> =
                entries.keys().cloned().collect();

            let keys: SmallVec<[ValueId; 4]> = key_vals
                .iter()
                .map(|k| ctx.arena.add(k.to_value(), ctx.span))
                .collect();

            Ok(ctx.arena.add(Value::Array(k_ty, keys), ctx.span))
        })
    }

    /// `Map.values(m) -> Array[V]`
    ///
    /// Returns an array of all values in iteration order.
    pub(crate) fn values<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (_, v_ty, entries) = ctx
                .arena
                .get_map_ref(args[0])
                .unwrap_or_else(|| typechecked!("Map.values", "Map"));

            let vals: SmallVec<[ValueId; 4]> =
                entries.values().copied().collect();
            Ok(ctx.arena.add(Value::Array(v_ty, vals), ctx.span))
        })
    }

    /// `Map.entries(m) -> Array[(K, V)]`
    ///
    /// Returns an array of `(key, value)` tuples in iteration order.
    pub(crate) fn entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (k_ty, v_ty, entries) = ctx
                .arena
                .get_map_ref(args[0])
                .unwrap_or_else(|| typechecked!("Map.entries", "Map"));

            // Collect entries before mutating arena
            let entry_pairs: SmallVec<[(MapKey, ValueId); 8]> =
                entries.iter().map(|(k, v)| (k.clone(), *v)).collect();
            let tuple_ty = ctx.type_exprs.tuple(smallvec![k_ty, v_ty]);

            let tuples: SmallVec<[ValueId; 4]> = entry_pairs
                .iter()
                .map(|(k, v_id)| {
                    let k_id = ctx.arena.add(k.to_value(), ctx.span);
                    let tuple = Value::Tuple(tuple_ty, smallvec![k_id, *v_id]);
                    ctx.arena.add(tuple, ctx.span)
                })
                .collect();

            let arr_ty = ctx.type_exprs.app(TypeId::ARRAY, smallvec![tuple_ty]);
            Ok(ctx.arena.add(Value::Array(arr_ty, tuples), ctx.span))
        })
    }

    /// `Map.has(m, k) -> Bool`
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
                .get_map_ref(args[0])
                .unwrap_or_else(|| typechecked!("Map.has", "Map"));

            let exists = entries.contains_key(&map_key);
            Ok(ctx.arena.add(Value::Bool(exists), ctx.span))
        })
    }

    /// `Map.lookup(m, k) -> Option[V]`
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
                .get_map_ref(args[0])
                .unwrap_or_else(|| typechecked!("Map.lookup", "Map"));

            match entries.get(&map_key) {
                Some(v_id) => Ok(ctx.option_some(*v_id)),
                None => Ok(ctx.option_none()),
            }
        })
    }

    /// `Map.insert(m, k, v) -> Map[K, V]`
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
            let (k_ty, v_ty, mut entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.insert", "Map"));

            entries.insert(map_key, args[2]);

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, entries), ctx.span))
        })
    }

    /// `Map.remove(m, k) -> Map[K, V]`
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

            let (k_ty, v_ty, mut entries) = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.remove", "Map"));

            entries.shift_remove(&map_key);

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, entries), ctx.span))
        })
    }

    /// `Map.merge(a, b) -> Map[K, V]`
    ///
    /// Returns a new map with entries from both maps (b overrides a).
    ///
    /// Type checker guarantees both maps have compatible key/value types.
    pub(crate) fn merge<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let (k_ty, v_ty, mut entries_a) =
                ctx.arena.get_map(args[0]).unwrap_or_else(|| {
                    typechecked!("Map.merge", "Map (first arg)")
                });

            let (_, _, entries_b) =
                ctx.arena.get_map(args[1]).unwrap_or_else(|| {
                    typechecked!("Map.merge", "Map (second arg)")
                });

            // Type checker guarantees compatible map types
            entries_a.extend(entries_b);

            let final_k_ty = k_ty;
            let final_v_ty = v_ty;

            Ok(ctx
                .arena
                .add(Value::Map(final_k_ty, final_v_ty, entries_a), ctx.span))
        })
    }

    /// `Map.from-entries(arr) -> Map[K, V]`
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

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, entries), ctx.span))
        })
    }
}

/// Primitives for the `Time` module.
pub(crate) struct Time;

impl Prim for Time {}

impl Time {
    /// `Time.now() -> Time`
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

    /// `Time.epoch() -> Time`
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

    /// `Time.parse(fmt, s) -> Result[Time, String]`
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

    /// `Time.format(fmt, t) -> String`
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

    /// `Time.add-seconds(t, n) -> Time`
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

    /// `Time.diff-seconds(a, b) -> Float`
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

    /// `Time.year(t) -> Int`
    pub(crate) fn year<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.year() as i64), ctx.span))
        })
    }

    /// `Time.month(t) -> Int` (1-12)
    pub(crate) fn month<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.month() as i64), ctx.span))
        })
    }

    /// `Time.day(t) -> Int` (1-31)
    pub(crate) fn day<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.day() as i64), ctx.span))
        })
    }

    /// `Time.hour(t) -> Int` (0-23)
    pub(crate) fn hour<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.hour() as i64), ctx.span))
        })
    }

    /// `Time.minute(t) -> Int` (0-59)
    pub(crate) fn minute<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.minute() as i64), ctx.span))
        })
    }

    /// `Time.second(t) -> Int` (0-59)
    pub(crate) fn second<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let t = Self::get_time(ctx, args[0], "Time");
            Ok(ctx.arena.add(Value::Int(t.second() as i64), ctx.span))
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
    /// `Option.unwrap-or(o, default) -> T`
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
}

/// Primitives for the `Result` module.
///
/// Higher-order functions `Result.map` and `Result.map-err` are intercepted
/// in `invoke_module_fn` (see `call.rs`); only placeholders are registered here.
pub(crate) struct Res;

impl Prim for Res {}

impl Res {
    /// `Result.unwrap-or(r, default) -> T`
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::TypeExprArena;
    use crate::Span;

    fn span() -> Span {
        Span::default()
    }

    fn make_int_array(
        arena: &mut crate::value::ValueArena,
        type_exprs: &mut TypeExprArena,
        vals: &[i64],
    ) -> ValueId {
        let int_ty = type_exprs.named(TypeId::INT);
        let elems: SmallVec<[ValueId; 4]> = vals
            .iter()
            .map(|n| arena.add(Value::Int(*n), span()))
            .collect();
        arena.add(Value::Array(int_ty, elems), span())
    }

    #[tokio::test]
    async fn array_length() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id =
            make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3, 4, 5]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::length(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(5)));
    }

    #[tokio::test]
    async fn array_length_empty() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id = make_int_array(&mut arena, &mut type_exprs, &[]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::length(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(0)));
    }

    #[tokio::test]
    async fn array_push() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id = make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3]);
        let val_id = arena.add(Value::Int(4), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::push(&mut ctx, smallvec![arr_id, val_id])
                .await
                .unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        assert_eq!(elems.len(), 4);
    }

    #[tokio::test]
    async fn array_pop() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id = make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::pop(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        assert_eq!(elems.len(), 2);
    }

    #[tokio::test]
    async fn array_head_some() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id = make_int_array(&mut arena, &mut type_exprs, &[42, 2, 3]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::head(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap();
        assert!(val.is_some(&type_exprs));
    }

    #[tokio::test]
    async fn array_head_none() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id = make_int_array(&mut arena, &mut type_exprs, &[]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::head(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap();
        assert!(val.is_none(&type_exprs));
    }

    #[tokio::test]
    async fn array_tail() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id = make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3, 4]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::tail(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        assert_eq!(elems.len(), 3);
    }

    #[tokio::test]
    async fn array_reverse() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id = make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::reverse(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        let vals: Vec<_> = elems
            .iter()
            .filter_map(|id| arena.get(*id))
            .filter_map(|v| match v {
                Value::Int(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(vals, vec![3, 2, 1]);
    }

    #[tokio::test]
    async fn array_sort() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id =
            make_int_array(&mut arena, &mut type_exprs, &[3, 1, 4, 1, 5]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::sort(&mut ctx, smallvec![arr_id]).await.unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        let vals: Vec<_> = elems
            .iter()
            .filter_map(|id| arena.get(*id))
            .filter_map(|v| match v {
                Value::Int(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(vals, vec![1, 1, 3, 4, 5]);
    }

    #[tokio::test]
    async fn array_slice() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id =
            make_int_array(&mut arena, &mut type_exprs, &[0, 1, 2, 3, 4, 5]);
        let start = arena.add(Value::Int(1), span());
        let end = arena.add(Value::Int(4), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::slice(&mut ctx, smallvec![arr_id, start, end])
                .await
                .unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        assert_eq!(elems.len(), 3);
    }

    #[tokio::test]
    async fn array_contains_found() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id =
            make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3, 4, 5]);
        let needle = arena.add(Value::Int(3), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::contains(&mut ctx, smallvec![arr_id, needle])
                .await
                .unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Bool(true)));
    }

    #[tokio::test]
    async fn array_contains_not_found() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_id =
            make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3, 4, 5]);
        let needle = arena.add(Value::Int(10), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::contains(&mut ctx, smallvec![arr_id, needle])
                .await
                .unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Bool(false)));
    }

    #[tokio::test]
    async fn array_concat() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr_a = make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3]);
        let arr_b = make_int_array(&mut arena, &mut type_exprs, &[4, 5, 6]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::concat(&mut ctx, smallvec![arr_a, arr_b])
                .await
                .unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        assert_eq!(elems.len(), 6);
    }

    // Removed: array_concat_type_mismatch
    // Type checker now catches Array.concat type mismatch at compile time.

    fn make_string(arena: &mut crate::value::ValueArena, s: &str) -> ValueId {
        let sid = arena.intern(s);
        arena.add(Value::String(sid), span())
    }

    #[tokio::test]
    async fn string_length() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "hello");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::length(&mut ctx, smallvec![s]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(5)));
    }

    #[tokio::test]
    async fn string_length_unicode() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        // `café` with combining accent: 4 grapheme clusters
        let s = make_string(&mut arena, "cafe\u{0301}");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::length(&mut ctx, smallvec![s]).await.unwrap()
        };

        // 4 graphemes: c, a, f, é (e + combining acute)
        assert_eq!(arena.get(result), Some(&Value::Int(4)));
    }

    #[tokio::test]
    async fn string_upper() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "hello");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::upper(&mut ctx, smallvec![s]).await.unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("HELLO"));
    }

    #[tokio::test]
    async fn string_lower() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "HELLO");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::lower(&mut ctx, smallvec![s]).await.unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("hello"));
    }

    #[tokio::test]
    async fn string_trim() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "  hello  ");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::trim(&mut ctx, smallvec![s]).await.unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("hello"));
    }

    #[tokio::test]
    async fn string_split() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "a,b,c");
        let d = make_string(&mut arena, ",");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::split(&mut ctx, smallvec![s, d]).await.unwrap()
        };

        let (_, elems) = arena.get_array(result).unwrap();
        assert_eq!(elems.len(), 3);

        let first_sid = arena.get_string_id(elems[0]).unwrap();
        assert_eq!(arena.get_str(first_sid), Some("a"));
    }

    #[tokio::test]
    async fn string_join() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let a = make_string(&mut arena, "a");
        let b = make_string(&mut arena, "b");
        let c = make_string(&mut arena, "c");
        let d = make_string(&mut arena, ",");

        let str_ty = type_exprs.named(TypeId::STRING);
        let arr = arena.add(Value::Array(str_ty, smallvec![a, b, c]), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::join(&mut ctx, smallvec![arr, d]).await.unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("a,b,c"));
    }

    #[tokio::test]
    async fn string_slice() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "hello");
        let start = arena.add(Value::Int(1), span());
        let end = arena.add(Value::Int(4), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::slice(&mut ctx, smallvec![s, start, end])
                .await
                .unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("ell"));
    }

    #[tokio::test]
    async fn string_slice_clamps() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "hello");
        let start = arena.add(Value::Int(-5), span());
        let end = arena.add(Value::Int(100), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::slice(&mut ctx, smallvec![s, start, end])
                .await
                .unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("hello"));
    }

    #[tokio::test]
    async fn string_contains_found() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "hello world");
        let sub = make_string(&mut arena, "wor");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::contains(&mut ctx, smallvec![s, sub]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Bool(true)));
    }

    #[tokio::test]
    async fn string_contains_not_found() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "hello");
        let sub = make_string(&mut arena, "xyz");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::contains(&mut ctx, smallvec![s, sub]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Bool(false)));
    }

    #[tokio::test]
    async fn string_replace() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "foo");
        let old = make_string(&mut arena, "o");
        let new = make_string(&mut arena, "a");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::replace(&mut ctx, smallvec![s, old, new])
                .await
                .unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("faa"));
    }

    #[tokio::test]
    async fn string_replace_all() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let s = make_string(&mut arena, "banana");
        let old = make_string(&mut arena, "a");
        let new = make_string(&mut arena, "o");

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Str::replace(&mut ctx, smallvec![s, old, new])
                .await
                .unwrap()
        };

        let sid = arena.get_string_id(result).unwrap();
        assert_eq!(arena.get_str(sid), Some("bonono"));
    }

    // Math module tests

    #[tokio::test]
    async fn math_abs_int() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Int(-42), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::abs(&mut ctx, smallvec![n]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(42)));
    }

    #[tokio::test]
    async fn math_abs_float() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Float(OrderedFloat(-3.5)), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::abs(&mut ctx, smallvec![n]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Float(OrderedFloat(3.5))));
    }

    #[tokio::test]
    async fn math_min_int() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let a = arena.add(Value::Int(5), span());
        let b = arena.add(Value::Int(3), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::min(&mut ctx, smallvec![a, b]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(3)));
    }

    #[tokio::test]
    async fn math_max_float() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let a = arena.add(Value::Float(OrderedFloat(2.5)), span());
        let b = arena.add(Value::Float(OrderedFloat(7.3)), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::max(&mut ctx, smallvec![a, b]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Float(OrderedFloat(7.3))));
    }

    #[tokio::test]
    async fn math_floor() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Float(OrderedFloat(3.7)), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::floor(&mut ctx, smallvec![n]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(3)));
    }

    #[tokio::test]
    async fn math_ceil() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Float(OrderedFloat(3.2)), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::ceil(&mut ctx, smallvec![n]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(4)));
    }

    #[tokio::test]
    async fn math_round() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Float(OrderedFloat(3.5)), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::round(&mut ctx, smallvec![n]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Int(4)));
    }

    #[tokio::test]
    async fn math_sqrt() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Int(16), span());
        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::sqrt(&mut ctx, smallvec![n]).await.unwrap()
        };

        assert_eq!(arena.get(result), Some(&Value::Float(OrderedFloat(4.0))));
    }

    #[tokio::test]
    async fn trig_sin_cos() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Int(0), span());

        let sin_result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Trig::sin(&mut ctx, smallvec![n]).await.unwrap()
        };

        let cos_result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Trig::cos(&mut ctx, smallvec![n]).await.unwrap()
        };

        assert_eq!(
            arena.get(sin_result),
            Some(&Value::Float(OrderedFloat(0.0)))
        );
        assert_eq!(
            arena.get(cos_result),
            Some(&Value::Float(OrderedFloat(1.0)))
        );
    }

    // Random module tests

    #[tokio::test]
    async fn random_random_in_range() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Random::random(&mut ctx, smallvec![]).await.unwrap()
        };

        match arena.get(result) {
            Some(Value::Float(f)) => {
                assert!(f.0 >= 0.0 && f.0 < 1.0);
            }
            _ => panic!("expected Float"),
        }
    }

    #[tokio::test]
    async fn random_int_in_range() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let min = arena.add(Value::Int(1), span());
        let max = arena.add(Value::Int(10), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Random::int(&mut ctx, smallvec![min, max]).await.unwrap()
        };

        match arena.get(result) {
            Some(Value::Int(n)) => {
                assert!(*n >= 1 && *n <= 10);
            }
            _ => panic!("expected Int"),
        }
    }

    #[tokio::test]
    async fn random_bool_returns_bool() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Random::bool(&mut ctx, smallvec![]).await.unwrap()
        };

        match arena.get(result) {
            Some(Value::Bool(_)) => {}
            _ => panic!("expected Bool"),
        }
    }

    #[tokio::test]
    async fn random_choice_some() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr = make_int_array(&mut arena, &mut type_exprs, &[10, 20, 30]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Random::choice(&mut ctx, smallvec![arr]).await.unwrap()
        };

        // Should be Option.Some(value)
        match arena.get(result) {
            Some(Value::Tagged(_, 1, payloads)) => {
                let inner = arena.get(payloads[0]);
                match inner {
                    Some(Value::Int(n)) => {
                        assert!(*n == 10 || *n == 20 || *n == 30);
                    }
                    _ => panic!("expected Int in Some payload"),
                }
            }
            _ => panic!("expected Tagged (Some)"),
        }
    }

    #[tokio::test]
    async fn random_choice_none() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr = make_int_array(&mut arena, &mut type_exprs, &[]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Random::choice(&mut ctx, smallvec![arr]).await.unwrap()
        };

        // Should be Option.None
        match arena.get(result) {
            Some(Value::Tagged(_, 0, payloads)) => {
                assert!(payloads.is_empty());
            }
            _ => panic!("expected Tagged (None)"),
        }
    }

    #[tokio::test]
    async fn random_shuffle() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let arr = make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3, 4, 5]);

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Random::shuffle(&mut ctx, smallvec![arr]).await.unwrap()
        };

        let (_, elems) = arena.get_array(result).expect("expected array");
        assert_eq!(elems.len(), 5);
    }

    #[tokio::test]
    async fn random_uuid() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Random::uuid(&mut ctx, smallvec![]).await.unwrap()
        };

        match arena.get(result) {
            Some(Value::String(sid)) => {
                let s = arena.get_str(*sid).expect("string");
                // UUID v4 format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
                assert_eq!(s.len(), 36);
                assert_eq!(s.chars().nth(8), Some('-'));
                assert_eq!(s.chars().nth(14), Some('4'));
            }
            _ => panic!("expected String"),
        }
    }
}
