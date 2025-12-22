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

use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use crate::env::{PrimCtx, PrimResult};
use crate::value::{StringId, TypeId, Value, ValueArena, ValueId};
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
                .ok_or_else(|| ctx.error("Object.keys expects Object"))?;

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
                .ok_or_else(|| ctx.error("Object.values expects Object"))?;

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
                .ok_or_else(|| ctx.error("Object.entries expects Object"))?;

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
                ctx.error("Object.from-entries expects Array")
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
                    .ok_or_else(|| ctx.error("Object.values: invalid value"))?;

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
                        ctx.error("Object.entries: invalid value")
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
            let elem = ctx
                .arena
                .get(*elem_id)
                .ok_or_else(|| ctx.error("Object.from-entries: invalid element"))?;

            match elem {
                Value::Tuple(_, parts) if parts.len() == 2 => {
                    // Safe: we checked len() == 2 above
                    let key_id = parts[0];
                    let val_id = parts[1];

                    let key_val = ctx
                        .arena
                        .get(key_id)
                        .ok_or_else(|| ctx.error("Object.from-entries: invalid key"))?;

                    match key_val {
                        Value::String(s) => {
                            obj.insert(*s, val_id);
                            Ok(())
                        }
                        _ => Err(ctx.error(
                            "Object.from-entries: tuple key must be String",
                        )),
                    }
                }
                _ => Err(ctx.error(
                    "Object.from-entries: array must contain (String, T) tuples",
                )),
            }
        })?;

        let result = Value::Object(obj);
        Ok(ctx.arena.add(result, ctx.span))
    }
}

/// Primitives for the `Array` module.
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
                .ok_or_else(|| ctx.error("Array.length expects Array"))?;

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

            let (ty, mut elems) =
                ctx.arena.get_array(args[0]).ok_or_else(|| {
                    ctx.error("Array.push expects Array as first argument")
                })?;

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
                .ok_or_else(|| ctx.error("Array.pop expects Array"))?;

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
                .ok_or_else(|| ctx.error("Array.head expects Array"))?;

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
                .ok_or_else(|| ctx.error("Array.tail expects Array"))?;

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
                .ok_or_else(|| ctx.error("Array.reverse expects Array"))?;

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
            /// sort_priority handles semantic ordering:
            /// - Option: None=0, Some=1 (ascending → Some > None)
            /// - Result: Err=0, Ok=1 (ascending → Ok > Err)
            /// - Others: variant index (declaration order)
            Tagged(i16, Vec<Self>),
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

                    // Closures, functions, and module functions are not comparable
                    Value::Closure { .. }
                    | Value::Function { .. }
                    | Value::ModuleFn { .. } => None,
                }
            }

            /// Compute sort priority for tagged values.
            ///
            /// - Option: None=0, Some=1 (Some > None)
            /// - Result: Err=0, Ok=1 (Ok > Err; note: idx 0=Ok, idx 1=Err)
            /// - Others: variant index (declaration order)
            fn sort_priority(
                ty_expr: &crate::value::TypeExprId,
                idx: u8,
                type_exprs: &TypeExprArena,
            ) -> i16 {
                match type_exprs.base_type(*ty_expr) {
                    Some(TypeId::OPTION) => idx as i16, // None=0, Some=1
                    Some(TypeId::RESULT) => 1 - idx as i16, // Ok(0)→1, Err(1)→0
                    _ => idx as i16,
                }
            }
        }

        Box::pin(async move {
            Self::check_arity("Array.sort", &args, 1, ctx.span)?;

            let (ty, elems) = ctx
                .arena
                .get_array(args[0])
                .ok_or_else(|| ctx.error("Array.sort expects Array"))?;

            // Collect (ValueId, sortable key) pairs
            let mut pairs: Vec<(ValueId, SortKey)> = elems
                .iter()
                .map(|vid| {
                    ctx.arena
                        .get(*vid)
                        .ok_or_else(|| ctx.error("Array.sort: invalid element"))
                        .and_then(|v| {
                            SortKey::from_value(v, ctx.arena, ctx.type_exprs)
                                .ok_or_else(|| {
                                    ctx.error(
                                        "Array.sort: element is not comparable",
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

            let (ty, elems) =
                ctx.arena.get_array(args[0]).ok_or_else(|| {
                    ctx.error("Array.slice expects Array as first argument")
                })?;

            let start = ctx
                .arena
                .get(args[1])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .ok_or_else(|| ctx.error("Array.slice: start must be Int"))?;

            let end = ctx
                .arena
                .get(args[2])
                .and_then(|v| match v {
                    Value::Int(n) => Some(*n),
                    _ => None,
                })
                .ok_or_else(|| ctx.error("Array.slice: end must be Int"))?;

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

            let (_, elems) = ctx.arena.get_array(args[0]).ok_or_else(|| {
                ctx.error("Array.contains expects Array as first argument")
            })?;

            let needle = ctx
                .arena
                .get(args[1])
                .ok_or_else(|| ctx.error("Array.contains: invalid value"))?;

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

            let (ty_a, elems_a) =
                ctx.arena.get_array(args[0]).ok_or_else(|| {
                    ctx.error("Array.concat expects Array as first argument")
                })?;

            let (ty_b, elems_b) =
                ctx.arena.get_array(args[1]).ok_or_else(|| {
                    ctx.error("Array.concat expects Array as second argument")
                })?;

            // Check element types match using the stored TypeExprId
            if ctx.type_exprs.eq(ty_a, ty_b) {
                let mut combined = elems_a;
                combined.extend(elems_b);
                Ok(ctx.arena.add(Value::Array(ty_a, combined), ctx.span))
            } else {
                Err(ctx
                    .error("Array.concat: arrays have different element types"))
            }
        })
    }
}

// NOTE: Array functions (map, filter, reduce) are higher-order and require
// access to the interpreter's closure invocation machinery. They are
// implemented in `interpreter/call.rs` and registered here as placeholders.
//
// The placeholders ensure `Environment::module_fn_exists` returns true during
// name resolution. The actual dispatch is intercepted in `invoke_module_fn`.
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
}
