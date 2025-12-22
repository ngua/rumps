//! Built-in primitive functions organized by module.
//!
//! Primitives are callable built-in functions registered in the environment.
//! They are implemented as associated functions on module types ([`Object`],
//! [`Array`], etc.), returning a future that resolves to a `ValueId`. Unlike
//! keywords (GET, SET, KILL), primitives use standard function call syntax
//! and are case-insensitive.
//!
//! # Module Organization
//!
//! Each RUMPS module is a separate type implementing the [`Prim`] trait:
//! - [`Object`]: `keys`, `values`, `entries`, `from-entries`
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
use crate::value::{MapKey, StringId, TypeId, Value, ValueArena, ValueId};
use crate::Error;

/// Shared utilities for primitive function implementations.
///
/// Module types ([`Object`], [`Array`], etc.) implement this trait to gain
/// access to common helpers like arity checking and the HoF placeholder.
///
/// # Why a trait with associated functions?
///
/// `PrimFn` uses a higher-ranked trait bound (HRTB) so it can be stored in a
/// `HashMap` without lifetime parameters, yet work with any `PrimCtx<'a>`
/// lifetime when called. Methods on `impl<'a> PrimCtx<'a>` bind the lifetime
/// parameter to the struct's lifetime, which doesn't satisfy the HRTB
/// `for<'a>` requirement. Associated functions on separate types sidestep
/// this by not binding the lifetime in the impl block.
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

    /// Arity check helper; returns `Err` if wrong number of arguments.
    ///
    /// After this check passes, direct indexing `args[i]` for `i < expected`
    /// is safe. This is an intentional exception to the general "no indexing"
    /// rule since arity is statically validated.
    fn check_arity(
        name: &str,
        args: &SmallVec<[ValueId; 4]>,
        expected: usize,
        span: crate::Span,
    ) -> crate::Result<()> {
        if args.len() == expected {
            Ok(())
        } else {
            Err(Error::runtime(
                span,
                format!(
                    "`{name}` expects {expected} argument(s), got {}",
                    args.len()
                ),
            ))
        }
    }

    /// Convert a value to `f64`, accepting `Int` or `Float`.
    fn to_float(
        ctx: &PrimCtx<'_>,
        id: ValueId,
        fn_name: &str,
    ) -> crate::Result<f64> {
        ctx.arena
            .get(id)
            .ok_or_else(|| {
                ctx.runtime_error(format!("{fn_name}: invalid value"))
            })
            .and_then(|v| match v {
                Value::Int(n) => Ok(*n as f64),
                Value::Float(f) => Ok(f.0),
                _ => Err(ctx.type_error(fn_name, "Int or Float")),
            })
    }

    /// Convert a value to `i64`, accepting `Int` only.
    fn to_int(
        ctx: &PrimCtx<'_>,
        id: ValueId,
        fn_name: &str,
    ) -> crate::Result<i64> {
        ctx.arena
            .get(id)
            .ok_or_else(|| {
                ctx.runtime_error(format!("{fn_name}: invalid value"))
            })
            .and_then(|v| match v {
                Value::Int(n) => Ok(*n),
                _ => Err(ctx.type_error(fn_name, "Int")),
            })
    }
}

/// Primitives for the `Object` module.
pub(crate) struct Object;

impl Prim for Object {}

impl Object {
    /// `Object.keys(obj) -> Array[String]`
    ///
    /// Returns an array of the object's field names in iteration order.
    pub(crate) fn keys<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Object.keys", &args, 1, ctx.span)?;

            let map = ctx
                .arena
                .get_object(args[0])
                .ok_or_else(|| ctx.type_error("Object.keys", "Object"))?;

            let keys: SmallVec<[ValueId; 4]> = map
                .keys()
                .map(|k| ctx.arena.add(Value::String(*k), ctx.span))
                .collect();

            let str_ty = ctx.type_exprs.named(TypeId::STRING);
            let arr = Value::Array(str_ty, keys);
            Ok(ctx.arena.add(arr, ctx.span))
        })
    }

    /// `Object.values(obj) -> Result[Array[T], String]`
    ///
    /// Returns `Result.Ok(array)` if all values have the same type,
    /// or `Result.Err(msg)` if values are heterogeneous.
    pub(crate) fn values<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Object.values", &args, 1, ctx.span)?;

            let map = ctx
                .arena
                .get_object(args[0])
                .ok_or_else(|| ctx.type_error("Object.values", "Object"))?;

            Self::values_from_map(ctx, &map)
        })
    }

    /// `Object.entries(obj) -> Result[Array[(String, T)], String]`
    ///
    /// Returns `Result.Ok(array)` of `(key, value)` tuples if all values have
    /// the same type, or `Result.Err(msg)` if values are heterogeneous.
    pub(crate) fn entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Object.entries", &args, 1, ctx.span)?;

            let map = ctx
                .arena
                .get_object(args[0])
                .ok_or_else(|| ctx.type_error("Object.entries", "Object"))?;

            Self::entries_from_map(ctx, &map)
        })
    }

    /// `Object.from-entries(arr) -> Object`
    ///
    /// Constructs an object from an array of `(key, value)` tuples.
    /// Later entries override earlier ones for duplicate keys.
    pub(crate) fn from_entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Object.from-entries", &args, 1, ctx.span)?;

            let (_, elems) = ctx.arena.get_array(args[0]).ok_or_else(|| {
                ctx.type_error("Object.from-entries", "Array")
            })?;

            Self::from_entries_impl(ctx, &elems)
        })
    }

    /// Helper: extract values from object map, checking homogeneity.
    fn values_from_map(
        ctx: &mut PrimCtx<'_>,
        map: &IndexMap<StringId, ValueId>,
    ) -> crate::Result<ValueId> {
        let maybe_first = map.values().next().copied();

        match maybe_first {
            None => {
                let unknown = ctx.type_exprs.named(TypeId::UNKNOWN);
                let arr = Value::Array(unknown, SmallVec::new());
                let arr_id = ctx.arena.add(arr, ctx.span);
                Ok(ctx.result_ok(arr_id))
            }
            Some(first_id) => {
                let first_ty = ctx
                    .arena
                    .base_type_of(first_id, ctx.type_exprs)
                    .ok_or_else(|| {
                        ctx.runtime_error("Object.values: invalid value")
                    })?;

                let heterogeneous = map.values().skip(1).any(|vid| {
                    ctx.arena
                        .base_type_of(*vid, ctx.type_exprs)
                        .map(|ty| ty != first_ty)
                        .unwrap_or(true)
                });

                if heterogeneous {
                    let msg = ctx.arena.intern(
                        "Object.values: object contains heterogeneous types",
                    );
                    let msg_val = ctx.arena.add(Value::String(msg), ctx.span);
                    Ok(ctx.result_err(msg_val))
                } else {
                    let vals: SmallVec<[ValueId; 4]> =
                        map.values().copied().collect();
                    let elem_ty = ctx.type_exprs.named(first_ty);
                    let arr = Value::Array(elem_ty, vals);
                    let arr_id = ctx.arena.add(arr, ctx.span);
                    Ok(ctx.result_ok(arr_id))
                }
            }
        }
    }

    /// Helper: extract entries from object map as `(key, value)` tuples.
    fn entries_from_map(
        ctx: &mut PrimCtx<'_>,
        map: &IndexMap<StringId, ValueId>,
    ) -> crate::Result<ValueId> {
        let maybe_first = map.values().next().copied();

        match maybe_first {
            None => {
                let unknown = ctx.type_exprs.named(TypeId::UNKNOWN);
                let arr = Value::Array(unknown, SmallVec::new());
                let arr_id = ctx.arena.add(arr, ctx.span);
                Ok(ctx.result_ok(arr_id))
            }
            Some(first_id) => {
                let first_ty = ctx
                    .arena
                    .base_type_of(first_id, ctx.type_exprs)
                    .ok_or_else(|| {
                        ctx.runtime_error("Object.entries: invalid value")
                    })?;

                let heterogeneous = map.values().skip(1).any(|vid| {
                    ctx.arena
                        .base_type_of(*vid, ctx.type_exprs)
                        .map(|ty| ty != first_ty)
                        .unwrap_or(true)
                });

                if heterogeneous {
                    let msg = ctx.arena.intern(
                        "Object.entries: object contains heterogeneous types",
                    );
                    let msg_val = ctx.arena.add(Value::String(msg), ctx.span);
                    Ok(ctx.result_err(msg_val))
                } else {
                    let str_ty = ctx.type_exprs.named(TypeId::STRING);
                    let val_ty = ctx.type_exprs.named(first_ty);
                    let tuple_ty =
                        ctx.type_exprs.tuple(smallvec![str_ty, val_ty]);

                    let entries: Vec<_> =
                        map.iter().map(|(k, v)| (*k, *v)).collect();

                    let tuples: SmallVec<[ValueId; 4]> = entries
                        .iter()
                        .map(|(k, v)| {
                            let key_val =
                                ctx.arena.add(Value::String(*k), ctx.span);
                            let tup =
                                Value::Tuple(tuple_ty, smallvec![key_val, *v]);
                            ctx.arena.add(tup, ctx.span)
                        })
                        .collect();

                    let arr = Value::Array(tuple_ty, tuples);
                    let arr_id = ctx.arena.add(arr, ctx.span);
                    Ok(ctx.result_ok(arr_id))
                }
            }
        }
    }

    /// Helper: build object from array of tuples.
    fn from_entries_impl(
        ctx: &mut PrimCtx<'_>,
        elems: &SmallVec<[ValueId; 4]>,
    ) -> crate::Result<ValueId> {
        let mut obj: IndexMap<StringId, ValueId> = IndexMap::new();

        elems.iter().try_for_each(|elem_id| {
            let elem = ctx.arena.get(*elem_id).ok_or_else(|| {
                ctx.runtime_error("Object.from-entries: invalid element")
            })?;

            match elem {
                Value::Tuple(_, parts) if parts.len() == 2 => {
                    // Safe: we checked len() == 2 above
                    let key_id = parts[0];
                    let val_id = parts[1];

                    let key_val = ctx.arena.get(key_id).ok_or_else(|| {
                        ctx.runtime_error("Object.from-entries: invalid key")
                    })?;

                    match key_val {
                        Value::String(s) => {
                            obj.insert(*s, val_id);
                            Ok(())
                        }
                        _ => Err(ctx.type_error_msg(
                            "Object.from-entries",
                            "key must be String",
                        )),
                    }
                }
                _ => Err(
                    ctx.type_error("Object.from-entries", "(String, T) tuples")
                ),
            }
        })?;

        let result = Value::Object(obj);
        Ok(ctx.arena.add(result, ctx.span))
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
            Self::check_arity("Array.length", &args, 1, ctx.span)?;

            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.length", "Array"))?;

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
            Self::check_arity("Array.push", &args, 2, ctx.span)?;

            let (ty, mut elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.push", "Array"))?;

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
            Self::check_arity("Array.pop", &args, 1, ctx.span)?;

            let (ty, mut elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.pop", "Array"))?;

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
            Self::check_arity("Array.head", &args, 1, ctx.span)?;

            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.head", "Array"))?;

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
            Self::check_arity("Array.tail", &args, 1, ctx.span)?;

            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.tail", "Array"))?;

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
            Self::check_arity("Array.reverse", &args, 1, ctx.span)?;

            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.reverse", "Array"))?;

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
        use crate::value::TypeExprArena;

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

                    // Maps and times are not directly comparable for sorting
                    Value::Map(_, _, _) | Value::Time(_) => None,

                    // Closures, functions, and module functions are not comparable
                    Value::Closure { .. }
                    | Value::Function { .. }
                    | Value::ModuleFn { .. } => None,
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
            Self::check_arity("Array.sort", &args, 1, ctx.span)?;

            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.sort", "Array"))?;

            // Collect (ValueId, sortable key) pairs
            let mut pairs: Vec<(ValueId, SortKey)> = elems
                .iter()
                .map(|vid| {
                    ctx.arena
                        .get(*vid)
                        .ok_or_else(|| {
                            ctx.runtime_error("Array.sort: invalid element")
                        })
                        .and_then(|v| {
                            SortKey::from_value(v, ctx.arena, ctx.type_exprs)
                                .ok_or_else(|| {
                                    ctx.type_error_msg(
                                        "Array.sort",
                                        "element not comparable",
                                    )
                                })
                                .map(|k| (*vid, k))
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
            Self::check_arity("Array.slice", &args, 3, ctx.span)?;

            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.slice", "Array"))?;

            let start = ctx
                .arena
                .get(args[1])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .ok_or_else(|| {
                    ctx.type_error_msg("Array.slice", "start must be Int")
                })?;

            let end = ctx
                .arena
                .get(args[2])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .ok_or_else(|| {
                    ctx.type_error_msg("Array.slice", "end must be Int")
                })?;

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
            Self::check_arity("Array.contains", &args, 2, ctx.span)?;

            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.contains", "Array"))?;

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
            Self::check_arity("Array.concat", &args, 2, ctx.span)?;

            let (ty_a, elems_a) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Array.concat", "Array"))?;

            let (ty_b, elems_b) = ctx
                .arena
                .get_array(args[1])
                .ok_or_else(|| ctx.type_error("Array.concat", "Array"))?;

            // Check element types match using the stored TypeExprId
            if ctx.type_exprs.eq(ty_a, ty_b) {
                let mut combined = elems_a;
                combined.extend(elems_b);
                Ok(ctx.arena.add(Value::Array(ty_a, combined), ctx.span))
            } else {
                Err(ctx.type_error_msg("Array.concat", "element types differ"))
            }
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
            Self::check_arity("String.length", &args, 1, ctx.span)?;

            let sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.length", "String"))?;

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
            Self::check_arity("String.upper", &args, 1, ctx.span)?;

            let sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.upper", "String"))?;

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
            Self::check_arity("String.lower", &args, 1, ctx.span)?;

            let sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.lower", "String"))?;

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
            Self::check_arity("String.trim", &args, 1, ctx.span)?;

            let sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.trim", "String"))?;

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
            Self::check_arity("String.split", &args, 2, ctx.span)?;

            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.split", "String"))?;

            let d_sid = ctx.arena.get_string_id(args[1]).ok_or_else(|| {
                ctx.type_error_msg("String.split", "delimiter must be String")
            })?;

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
            Self::check_arity("String.join", &args, 2, ctx.span)?;

            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("String.join", "Array"))?;
            let elems = elems.clone();

            let d_sid = ctx.arena.get_string_id(args[1]).ok_or_else(|| {
                ctx.type_error_msg("String.join", "delimiter must be String")
            })?;

            // Collect string slices from array elements
            let parts: crate::Result<Vec<&str>> = elems
                .iter()
                .map(|id| {
                    ctx.arena
                        .get_string_id(*id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .ok_or_else(|| {
                            ctx.type_error("String.join", "Array[String]")
                        })
                })
                .collect();

            let d = ctx.arena.get_str(d_sid).ok_or_else(|| {
                ctx.runtime_error("String.join: invalid delimiter")
            })?;

            let joined = parts?.into_iter().join(d);
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
            Self::check_arity("String.slice", &args, 3, ctx.span)?;

            let sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.slice", "String"))?;

            let start = ctx
                .arena
                .get(args[1])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .ok_or_else(|| {
                    ctx.type_error_msg("String.slice", "start must be Int")
                })?;

            let end = ctx
                .arena
                .get(args[2])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .ok_or_else(|| {
                    ctx.type_error_msg("String.slice", "end must be Int")
                })?;

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
            Self::check_arity("String.contains", &args, 2, ctx.span)?;

            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.contains", "String"))?;

            let sub_sid =
                ctx.arena.get_string_id(args[1]).ok_or_else(|| {
                    ctx.type_error_msg(
                        "String.contains",
                        "substring must be String",
                    )
                })?;

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
            Self::check_arity("String.replace", &args, 3, ctx.span)?;

            let s_sid = ctx
                .arena
                .get_string_id(args[0])
                .ok_or_else(|| ctx.type_error("String.replace", "String"))?;

            let old_sid =
                ctx.arena.get_string_id(args[1]).ok_or_else(|| {
                    ctx.type_error_msg(
                        "String.replace",
                        "pattern must be String",
                    )
                })?;

            let new_sid =
                ctx.arena.get_string_id(args[2]).ok_or_else(|| {
                    ctx.type_error_msg(
                        "String.replace",
                        "replacement must be String",
                    )
                })?;

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
            Self::check_arity("Math.abs", &args, 1, ctx.span)?;

            let v = ctx
                .arena
                .get(args[0])
                .ok_or_else(|| ctx.runtime_error("Math.abs: invalid value"))?;

            let result = match v {
                Value::Int(n) => Value::Int(n.abs()),
                Value::Float(f) => Value::Float(OrderedFloat(f.0.abs())),
                _ => Err(ctx.type_error("Math.abs", "Int or Float"))?,
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
            Self::check_arity("Math.min", &args, 2, ctx.span)?;

            let result = match (ctx.arena.get(args[0]), ctx.arena.get(args[1]))
            {
                (Some(Value::Int(x)), Some(Value::Int(y))) => {
                    Value::Int((*x).min(*y))
                }
                _ => {
                    let x = Self::to_float(ctx, args[0], "Math.min")?;
                    let y = Self::to_float(ctx, args[1], "Math.min")?;
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
            Self::check_arity("Math.max", &args, 2, ctx.span)?;

            let result = match (ctx.arena.get(args[0]), ctx.arena.get(args[1]))
            {
                (Some(Value::Int(x)), Some(Value::Int(y))) => {
                    Value::Int((*x).max(*y))
                }
                _ => {
                    let x = Self::to_float(ctx, args[0], "Math.max")?;
                    let y = Self::to_float(ctx, args[1], "Math.max")?;
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
            Self::check_arity("Math.floor", &args, 1, ctx.span)?;

            let n = Self::to_float(ctx, args[0], "Math.floor")?;
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
            Self::check_arity("Math.ceil", &args, 1, ctx.span)?;

            let n = Self::to_float(ctx, args[0], "Math.ceil")?;
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
            Self::check_arity("Math.round", &args, 1, ctx.span)?;

            let n = Self::to_float(ctx, args[0], "Math.round")?;
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
            Self::check_arity("Math.sqrt", &args, 1, ctx.span)?;

            let n = Self::to_float(ctx, args[0], "Math.sqrt")?;
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
            Self::check_arity("Math.log", &args, 1, ctx.span)?;

            let n = Self::to_float(ctx, args[0], "Math.log")?;
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n.ln())), ctx.span))
        })
    }

    /// `Math.sin(x) -> Float`
    ///
    /// Returns the sine of x (x in radians).
    pub(crate) fn sin<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Math.sin", &args, 1, ctx.span)?;

            let n = Self::to_float(ctx, args[0], "Math.sin")?;
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n.sin())), ctx.span))
        })
    }

    /// `Math.cos(x) -> Float`
    ///
    /// Returns the cosine of x (x in radians).
    pub(crate) fn cos<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Math.cos", &args, 1, ctx.span)?;

            let n = Self::to_float(ctx, args[0], "Math.cos")?;
            Ok(ctx.arena.add(Value::Float(OrderedFloat(n.cos())), ctx.span))
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
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Random.random", &args, 0, ctx.span)?;

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
            Self::check_arity("Random.range", &args, 2, ctx.span)?;

            let min = Self::to_float(ctx, args[0], "Random.range")?;
            let max = Self::to_float(ctx, args[1], "Random.range")?;

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
            Self::check_arity("Random.int", &args, 2, ctx.span)?;

            let min = Self::to_int(ctx, args[0], "Random.int")?;
            let max = Self::to_int(ctx, args[1], "Random.int")?;

            let n: i64 = rand::thread_rng().gen_range(min..=max);
            Ok(ctx.arena.add(Value::Int(n), ctx.span))
        })
    }

    /// `Random.bool() -> Bool`
    ///
    /// Returns a random boolean.
    pub(crate) fn bool<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Random.bool", &args, 0, ctx.span)?;

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
            Self::check_arity("Random.choice", &args, 1, ctx.span)?;

            let (_, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Random.choice", "Array"))?;

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
            Self::check_arity("Random.shuffle", &args, 1, ctx.span)?;

            let (ty, mut elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Random.shuffle", "Array"))?;

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
            Self::check_arity("Random.sample", &args, 2, ctx.span)?;

            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Random.sample", "Array"))?;

            let n = Self::to_int(ctx, args[1], "Random.sample")? as usize;

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
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Random.uuid", &args, 0, ctx.span)?;

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
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Map.empty", &args, 0, ctx.span)?;

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
            Self::check_arity("Map.length", &args, 1, ctx.span)?;

            let (_, _, entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.length", "Map"))?;

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
            Self::check_arity("Map.keys", &args, 1, ctx.span)?;

            let (k_ty, _, entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.keys", "Map"))?;

            let keys: SmallVec<[ValueId; 4]> = entries
                .keys()
                .map(|k| {
                    let v = Self::map_key_to_value(k, ctx.arena);
                    ctx.arena.add(v, ctx.span)
                })
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
            Self::check_arity("Map.values", &args, 1, ctx.span)?;

            let (_, v_ty, entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.values", "Map"))?;

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
            Self::check_arity("Map.entries", &args, 1, ctx.span)?;

            let (k_ty, v_ty, entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.entries", "Map"))?;

            let tuple_ty = ctx.type_exprs.tuple(smallvec![k_ty, v_ty]);

            let tuples: SmallVec<[ValueId; 4]> = entries
                .iter()
                .map(|(k, v_id)| {
                    let k_val = Self::map_key_to_value(k, ctx.arena);
                    let k_id = ctx.arena.add(k_val, ctx.span);
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
            Self::check_arity("Map.has", &args, 2, ctx.span)?;

            let (_, _, entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.has", "Map"))?;

            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = Self::value_to_map_key(key, ctx)?;
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
            Self::check_arity("Map.lookup", &args, 2, ctx.span)?;

            let (_, _, entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.lookup", "Map"))?;

            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = Self::value_to_map_key(key, ctx)?;

            match entries.get(&map_key) {
                Some(v_id) => Ok(ctx.option_some(*v_id)),
                None => Ok(ctx.option_none()),
            }
        })
    }

    /// `Map.insert(m, k, v) -> Map[K, V]`
    ///
    /// Returns a new map with the key-value pair inserted/updated.
    pub(crate) fn set<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Map.insert", &args, 3, ctx.span)?;

            let (k_ty, v_ty, mut entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.insert", "Map"))?;

            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = Self::value_to_map_key(key, ctx)?;
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
            Self::check_arity("Map.remove", &args, 2, ctx.span)?;

            let (k_ty, v_ty, mut entries) = ctx
                .arena
                .get_map(args[0])
                .ok_or_else(|| ctx.type_error("Map.remove", "Map"))?;

            let key = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = Self::value_to_map_key(key, ctx)?;
            entries.shift_remove(&map_key);

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, entries), ctx.span))
        })
    }

    /// `Map.merge(a, b) -> Map[K, V]`
    ///
    /// Returns a new map with entries from both maps (b overrides a).
    pub(crate) fn merge<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Map.merge", &args, 2, ctx.span)?;

            let (k_ty, v_ty, mut entries_a) =
                ctx.arena.get_map(args[0]).ok_or_else(|| {
                    ctx.type_error("Map.merge", "Map (first arg)")
                })?;

            let (_, _, entries_b) =
                ctx.arena.get_map(args[1]).ok_or_else(|| {
                    ctx.type_error("Map.merge", "Map (second arg)")
                })?;

            entries_a.extend(entries_b);

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, entries_a), ctx.span))
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
            Self::check_arity("Map.from-entries", &args, 1, ctx.span)?;

            let (_, arr) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.type_error("Map.from-entries", "Array"))?;

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

            arr.iter().try_for_each(|id| {
                let val = ctx
                    .arena
                    .get(*id)
                    .ok_or_else(|| ctx.runtime_error("invalid entry id"))?;

                match val {
                    Value::Tuple(_, elems) if elems.len() == 2 => {
                        let k_val =
                            ctx.arena.get(elems[0]).ok_or_else(|| {
                                ctx.runtime_error("invalid key id")
                            })?;
                        let map_key = Self::value_to_map_key(k_val, ctx)?;
                        entries.insert(map_key, elems[1]);
                        Ok(())
                    }
                    _ => Err(ctx.type_error_msg(
                        "Map.from-entries",
                        "array elements must be 2-tuples",
                    )),
                }
            })?;

            Ok(ctx.arena.add(Value::Map(k_ty, v_ty, entries), ctx.span))
        })
    }

    /// Convert a `MapKey` back to a `Value`.
    fn map_key_to_value(k: &MapKey, _arena: &mut ValueArena) -> Value {
        match k {
            MapKey::Bool(b) => Value::Bool(*b),
            MapKey::Int(n) => Value::Int(*n),
            MapKey::Float(f) => Value::Float(*f),
            MapKey::Char(c) => Value::Char(*c),
            MapKey::String(sid) => Value::String(*sid),
        }
    }

    /// Convert a `Value` to a `MapKey`, or return an error.
    fn value_to_map_key(v: &Value, ctx: &PrimCtx<'_>) -> crate::Result<MapKey> {
        match v {
            Value::Bool(b) => Ok(MapKey::Bool(*b)),
            Value::Int(n) => Ok(MapKey::Int(*n)),
            Value::Float(f) => Ok(MapKey::Float(*f)),
            Value::Char(c) => Ok(MapKey::Char(*c)),
            Value::String(sid) => Ok(MapKey::String(*sid)),
            _ => Err(ctx.type_error_msg(
                "Map",
                "keys must be scalar (Bool, Int, Float, Char, String)",
            )),
        }
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
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Time.now", &args, 0, ctx.span)?;

            let now = Utc::now();
            Ok(ctx.arena.add(Value::Time(now), ctx.span))
        })
    }

    /// `Time.epoch() -> Time`
    ///
    /// Returns the Unix epoch (1970-01-01 00:00:00 UTC).
    pub(crate) fn epoch<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Time.epoch", &args, 0, ctx.span)?;

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
            Self::check_arity("Time.parse", &args, 2, ctx.span)?;

            let fmt_sid =
                ctx.arena.get_string_id(args[0]).ok_or_else(|| {
                    ctx.type_error("Time.parse", "String (format)")
                })?;
            let fmt = ctx
                .arena
                .get_str(fmt_sid)
                .ok_or_else(|| ctx.runtime_error("invalid format string"))?;

            let s_sid = ctx.arena.get_string_id(args[1]).ok_or_else(|| {
                ctx.type_error("Time.parse", "String (input)")
            })?;
            let s = ctx
                .arena
                .get_str(s_sid)
                .ok_or_else(|| ctx.runtime_error("invalid input string"))?;

            match DateTime::parse_from_str(&s, &fmt) {
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
            Self::check_arity("Time.format", &args, 2, ctx.span)?;

            let fmt_sid =
                ctx.arena.get_string_id(args[0]).ok_or_else(|| {
                    ctx.type_error("Time.format", "String (format)")
                })?;
            let fmt = ctx
                .arena
                .get_str(fmt_sid)
                .ok_or_else(|| ctx.runtime_error("invalid format string"))?;

            let t = Self::get_time(ctx, args[1], "Time.format")?;

            let formatted = t.format(&fmt).to_string();
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
            Self::check_arity("Time.add-seconds", &args, 2, ctx.span)?;

            let t = Self::get_time(ctx, args[0], "Time.add-seconds")?;
            let secs = Self::get_float(ctx, args[1], "Time.add-seconds")?;

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
            Self::check_arity("Time.diff-seconds", &args, 2, ctx.span)?;

            let a = Self::get_time(ctx, args[0], "Time.diff-seconds (first)")?;
            let b = Self::get_time(ctx, args[1], "Time.diff-seconds (second)")?;

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
            Self::check_arity("Time.year", &args, 1, ctx.span)?;
            let t = Self::get_time(ctx, args[0], "Time.year")?;
            Ok(ctx.arena.add(Value::Int(t.year() as i64), ctx.span))
        })
    }

    /// `Time.month(t) -> Int` (1-12)
    pub(crate) fn month<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Time.month", &args, 1, ctx.span)?;
            let t = Self::get_time(ctx, args[0], "Time.month")?;
            Ok(ctx.arena.add(Value::Int(t.month() as i64), ctx.span))
        })
    }

    /// `Time.day(t) -> Int` (1-31)
    pub(crate) fn day<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Time.day", &args, 1, ctx.span)?;
            let t = Self::get_time(ctx, args[0], "Time.day")?;
            Ok(ctx.arena.add(Value::Int(t.day() as i64), ctx.span))
        })
    }

    /// `Time.hour(t) -> Int` (0-23)
    pub(crate) fn hour<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Time.hour", &args, 1, ctx.span)?;
            let t = Self::get_time(ctx, args[0], "Time.hour")?;
            Ok(ctx.arena.add(Value::Int(t.hour() as i64), ctx.span))
        })
    }

    /// `Time.minute(t) -> Int` (0-59)
    pub(crate) fn minute<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Time.minute", &args, 1, ctx.span)?;
            let t = Self::get_time(ctx, args[0], "Time.minute")?;
            Ok(ctx.arena.add(Value::Int(t.minute() as i64), ctx.span))
        })
    }

    /// `Time.second(t) -> Int` (0-59)
    pub(crate) fn second<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            Self::check_arity("Time.second", &args, 1, ctx.span)?;
            let t = Self::get_time(ctx, args[0], "Time.second")?;
            Ok(ctx.arena.add(Value::Int(t.second() as i64), ctx.span))
        })
    }

    /// Helper to extract a `Time` value from an argument.
    fn get_time(
        ctx: &PrimCtx<'_>,
        id: ValueId,
        fn_name: &str,
    ) -> crate::Result<DateTime<Utc>> {
        ctx.arena
            .get(id)
            .and_then(|v| match v {
                Value::Time(t) => Some(*t),
                _ => None,
            })
            .ok_or_else(|| ctx.type_error(fn_name, "Time"))
    }

    /// Helper to extract a `Float` value from an argument (accepts Int too).
    fn get_float(
        ctx: &PrimCtx<'_>,
        id: ValueId,
        fn_name: &str,
    ) -> crate::Result<f64> {
        ctx.arena
            .get(id)
            .and_then(|v| match v {
                Value::Float(f) => Some(f.0),
                Value::Int(n) => Some(*n as f64),
                _ => None,
            })
            .ok_or_else(|| ctx.type_error(fn_name, "Float or Int"))
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

    #[tokio::test]
    async fn keys_empty_object() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let obj = Value::Object(IndexMap::new());
        let obj_id = arena.add(obj, span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Object::keys(&mut ctx, smallvec![obj_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap();
        match val {
            Value::Array(_, elems) => assert!(elems.is_empty()),
            _ => panic!("expected Array"),
        }
    }

    #[tokio::test]
    async fn keys_with_fields() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let k1 = arena.intern("a");
        let k2 = arena.intern("b");
        let v1 = arena.add(Value::Int(1), span());
        let v2 = arena.add(Value::Int(2), span());

        let mut map = IndexMap::new();
        map.insert(k1, v1);
        map.insert(k2, v2);

        let obj = Value::Object(map);
        let obj_id = arena.add(obj, span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Object::keys(&mut ctx, smallvec![obj_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap();
        match val {
            Value::Array(_, elems) => {
                assert_eq!(elems.len(), 2);
                let first_id = elems.first().copied().unwrap();
                let first = arena.get(first_id).unwrap();
                match first {
                    Value::String(s) => {
                        assert_eq!(arena.get_str(*s), Some("a"));
                    }
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[tokio::test]
    async fn values_homogeneous() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let k1 = arena.intern("x");
        let k2 = arena.intern("y");
        let v1 = arena.add(Value::Int(10), span());
        let v2 = arena.add(Value::Int(20), span());

        let mut map = IndexMap::new();
        map.insert(k1, v1);
        map.insert(k2, v2);

        let obj = Value::Object(map);
        let obj_id = arena.add(obj, span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Object::values(&mut ctx, smallvec![obj_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap().clone();

        // Should be Result.Ok(array)
        match val {
            Value::Tagged(_, 0, payload) => {
                let arr_id = payload.first().copied().unwrap();
                let arr = arena.get(arr_id).unwrap();
                match arr {
                    Value::Array(_, elems) => assert_eq!(elems.len(), 2),
                    _ => panic!("expected Array inside Ok"),
                }
            }
            _ => panic!("expected Result.Ok"),
        }
    }

    #[tokio::test]
    async fn values_heterogeneous() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let k1 = arena.intern("x");
        let k2 = arena.intern("y");
        let v1 = arena.add(Value::Int(10), span());
        let s = arena.intern("hello");
        let v2 = arena.add(Value::String(s), span());

        let mut map = IndexMap::new();
        map.insert(k1, v1);
        map.insert(k2, v2);

        let obj = Value::Object(map);
        let obj_id = arena.add(obj, span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Object::values(&mut ctx, smallvec![obj_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap();

        // Should be Result.Err(msg)
        match val {
            Value::Tagged(_, 1, _) => (),
            _ => panic!("expected Result.Err for heterogeneous object"),
        }
    }

    #[tokio::test]
    async fn from_entries_roundtrip() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        // Create tuples: [("a", 1), ("b", 2)]
        let str_ty = type_exprs.named(TypeId::STRING);
        let int_ty = type_exprs.named(TypeId::INT);
        let tup_ty = type_exprs.tuple(smallvec![str_ty, int_ty]);

        let k1 = arena.intern("a");
        let k2 = arena.intern("b");
        let key1 = arena.add(Value::String(k1), span());
        let key2 = arena.add(Value::String(k2), span());
        let val1 = arena.add(Value::Int(1), span());
        let val2 = arena.add(Value::Int(2), span());

        let tup1 = Value::Tuple(tup_ty, smallvec![key1, val1]);
        let tup2 = Value::Tuple(tup_ty, smallvec![key2, val2]);
        let t1_id = arena.add(tup1, span());
        let t2_id = arena.add(tup2, span());

        let arr = Value::Array(tup_ty, smallvec![t1_id, t2_id]);
        let arr_id = arena.add(arr, span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Object::from_entries(&mut ctx, smallvec![arr_id])
                .await
                .unwrap()
        };

        let val = arena.get(result).unwrap().clone();

        match val {
            Value::Object(map) => {
                assert_eq!(map.len(), 2);
                let a_key = arena.lookup_string("a").unwrap();
                let a_val_id = map.get(&a_key).unwrap();
                let a_val = arena.get(*a_val_id).unwrap();
                assert_eq!(a_val, &Value::Int(1));
            }
            _ => panic!("expected Object"),
        }
    }

    // ============ Array module tests ============

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

    #[tokio::test]
    async fn array_concat_type_mismatch() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        // Array of ints
        let arr_a = make_int_array(&mut arena, &mut type_exprs, &[1, 2, 3]);

        // Array of strings
        let str_ty = type_exprs.named(TypeId::STRING);
        let s = arena.intern("hello");
        let str_val = arena.add(Value::String(s), span());
        let arr_b = arena.add(Value::Array(str_ty, smallvec![str_val]), span());

        let result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Array::concat(&mut ctx, smallvec![arr_a, arr_b]).await
        };

        assert!(result.is_err());
    }

    // ============ String module tests ============

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
    async fn math_sin_cos() {
        let mut arena = crate::value::ValueArena::new();
        let mut type_exprs = TypeExprArena::new();

        let n = arena.add(Value::Int(0), span());

        let sin_result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::sin(&mut ctx, smallvec![n]).await.unwrap()
        };

        let cos_result = {
            let mut ctx = PrimCtx {
                arena: &mut arena,
                type_exprs: &mut type_exprs,
                span: span(),
            };
            Math::cos(&mut ctx, smallvec![n]).await.unwrap()
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
