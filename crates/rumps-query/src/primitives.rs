//! Built-in primitive functions (KEYS, VALUES, ENTRIES, FROM-ENTRIES, etc.).
//!
//! Primitives are callable built-in functions registered in the environment.
//! They are implemented as associated functions on `Prim`, returning a future
//! that resolves to a `ValueId`. Unlike keywords (GET, SET, KILL), primitives
//! use standard function call syntax and are case-insensitive.

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use crate::env::{PrimCtx, PrimResult};
use crate::value::{StringId, TypeId, Value, ValueId};
use crate::Error;

/// Namespace for built-in primitive functions.
///
/// Each associated function has signature matching `PrimFn`:
/// `for<'a> fn(&'a mut PrimCtx<'a>, SmallVec<[ValueId; 4]>) -> PrimResult<'a>`
///
/// # Why associated functions instead of methods on `PrimCtx`?
///
/// `PrimFn` uses a higher-ranked trait bound (HRTB) so it can be stored in a
/// `HashMap` without lifetime parameters, yet work with any `PrimCtx<'a>`
/// lifetime when called. Methods on `impl<'a> PrimCtx<'a>` bind the lifetime
/// parameter to the struct's lifetime, which doesn't satisfy the HRTB
/// `for<'a>` requirement. Associated functions on a separate type sidestep
/// this by not binding the lifetime in the impl block.
pub(crate) struct Prim;

/// Arity check helper; returns `Err` if wrong number of arguments.
fn check_arity(
    name: &str,
    args: &SmallVec<[ValueId; 4]>,
    expected: usize,
) -> crate::Result<()> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(Error::runtime_no_span(format!(
            "`{name}` expects {expected} argument(s), got {}",
            args.len()
        )))
    }
}

/// Get a simplified base type for homogeneity checking.
fn value_base_type(v: &Value) -> TypeId {
    match v {
        Value::Bool(_) => TypeId::BOOL,
        Value::Int(_) => TypeId::INT,
        Value::Float(_) => TypeId::FLOAT,
        Value::Char(_) => TypeId::CHAR,
        Value::String(_) => TypeId::STRING,
        Value::Array(..) => TypeId::ARRAY,
        Value::Object(_) => TypeId::OBJECT,
        Value::Tuple(..) => TypeId::TUPLE,
        Value::Tagged(_, _, _) => TypeId::UNKNOWN,
        Value::Closure { .. }
        | Value::Function { .. }
        | Value::ModuleFn { .. } => TypeId::UNKNOWN,
    }
}

impl Prim {
    /// `KEYS(obj) -> Array[String]`
    ///
    /// Returns an array of the object's field names (strings) in iteration
    /// order.
    pub(crate) fn keys<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            check_arity("KEYS", &args, 1)?;

            let obj_id = *args.get(0).ok_or_else(|| {
                Error::runtime_no_span("KEYS: missing argument")
            })?;

            let obj = ctx.arena.get(obj_id).cloned().ok_or_else(|| {
                Error::runtime_no_span("KEYS: invalid argument")
            })?;

            match obj {
                Value::Object(map) => {
                    let keys: SmallVec<[ValueId; 4]> = map
                        .keys()
                        .map(|k| ctx.arena.add(Value::String(*k), ctx.span))
                        .collect();

                    let str_ty = ctx.type_exprs.named(TypeId::STRING);
                    let arr = Value::Array(str_ty, keys);
                    Ok(ctx.arena.add(arr, ctx.span))
                }
                other => Err(Error::runtime_no_span(format!(
                    "KEYS expects Object, got {:?}",
                    std::mem::discriminant(&other)
                ))),
            }
        })
    }

    /// `VALUES(obj) -> Result[Array[T], String]`
    ///
    /// Returns `Result.Ok(array)` if all values have the same type,
    /// or `Result.Err(msg)` if values are heterogeneous.
    pub(crate) fn values<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            check_arity("VALUES", &args, 1)?;

            let obj_id = *args.get(0).ok_or_else(|| {
                Error::runtime_no_span("VALUES: missing argument")
            })?;

            let obj = ctx.arena.get(obj_id).cloned().ok_or_else(|| {
                Error::runtime_no_span("VALUES: invalid argument")
            })?;

            match obj {
                Value::Object(map) => values_from_map(ctx, &map),
                _ => Err(Error::runtime_no_span("VALUES expects Object")),
            }
        })
    }

    /// `ENTRIES(obj) -> Result[Array[(String, T)], String]`
    ///
    /// Returns `Result.Ok(array)` of `(key, value)` tuples if all values have
    /// the same type, or `Result.Err(msg)` if values are heterogeneous.
    pub(crate) fn entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            check_arity("ENTRIES", &args, 1)?;

            let obj_id = *args.get(0).ok_or_else(|| {
                Error::runtime_no_span("ENTRIES: missing argument")
            })?;

            let obj = ctx.arena.get(obj_id).cloned().ok_or_else(|| {
                Error::runtime_no_span("ENTRIES: invalid argument")
            })?;

            match obj {
                Value::Object(map) => entries_from_map(ctx, &map),
                _ => Err(Error::runtime_no_span("ENTRIES expects Object")),
            }
        })
    }

    /// `FROM-ENTRIES(arr) -> Object`
    ///
    /// Constructs an object from an array of `(key, value)` tuples.
    /// Later entries override earlier ones for duplicate keys.
    pub(crate) fn from_entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            check_arity("FROM-ENTRIES", &args, 1)?;

            let arr_id = *args.get(0).ok_or_else(|| {
                Error::runtime_no_span("FROM-ENTRIES: missing argument")
            })?;

            let arr = ctx.arena.get(arr_id).cloned().ok_or_else(|| {
                Error::runtime_no_span("FROM-ENTRIES: invalid argument")
            })?;

            match arr {
                Value::Array(_, elems) => from_entries_impl(ctx, &elems),
                _ => Err(Error::runtime_no_span("FROM-ENTRIES expects Array")),
            }
        })
    }
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
            let first_val =
                ctx.arena.get(first_id).cloned().ok_or_else(|| {
                    Error::runtime_no_span("VALUES: invalid value in object")
                })?;
            let first_ty = value_base_type(&first_val);

            let heterogeneous = map.values().skip(1).any(|vid| {
                ctx.arena
                    .get(*vid)
                    .map(|v| value_base_type(v) != first_ty)
                    .unwrap_or(true)
            });

            if heterogeneous {
                let msg = ctx
                    .arena
                    .intern("VALUES: object contains heterogeneous types");
                let msg_val = ctx.arena.add(Value::String(msg), ctx.span);
                Ok(ctx.result_err(msg_val))
            } else {
                let vals: SmallVec<[ValueId; 4]> =
                    map.values().copied().collect();
                let elem_ty = ctx.type_exprs.named(value_base_type(&first_val));
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
            let first_val =
                ctx.arena.get(first_id).cloned().ok_or_else(|| {
                    Error::runtime_no_span("ENTRIES: invalid value")
                })?;
            let first_ty = value_base_type(&first_val);

            let heterogeneous = map.values().skip(1).any(|vid| {
                ctx.arena
                    .get(*vid)
                    .map(|v| value_base_type(v) != first_ty)
                    .unwrap_or(true)
            });

            if heterogeneous {
                let msg = ctx
                    .arena
                    .intern("ENTRIES: object contains heterogeneous types");
                let msg_val = ctx.arena.add(Value::String(msg), ctx.span);
                Ok(ctx.result_err(msg_val))
            } else {
                let str_ty = ctx.type_exprs.named(TypeId::STRING);
                let val_ty = ctx.type_exprs.named(value_base_type(&first_val));
                let tuple_ty = ctx.type_exprs.tuple(smallvec![str_ty, val_ty]);

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
            Error::runtime_no_span("FROM-ENTRIES: invalid element")
        })?;

        match elem {
            Value::Tuple(_, parts) if parts.len() == 2 => {
                let key_id = *parts.get(0).ok_or_else(|| {
                    Error::runtime_no_span("FROM-ENTRIES: missing key")
                })?;
                let val_id = *parts.get(1).ok_or_else(|| {
                    Error::runtime_no_span("FROM-ENTRIES: missing value")
                })?;

                let key_val = ctx.arena.get(key_id).ok_or_else(|| {
                    Error::runtime_no_span("FROM-ENTRIES: invalid key")
                })?;

                match key_val {
                    Value::String(s) => {
                        obj.insert(*s, val_id);
                        Ok(())
                    }
                    _ => Err(Error::runtime_no_span(
                        "FROM-ENTRIES: tuple key must be String",
                    )),
                }
            }
            _ => Err(Error::runtime_no_span(
                "FROM-ENTRIES: array must contain (String, T) tuples",
            )),
        }
    })?;

    let result = Value::Object(obj);
    Ok(ctx.arena.add(result, ctx.span))
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
            Prim::keys(&mut ctx, smallvec![obj_id]).await.unwrap()
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
            Prim::keys(&mut ctx, smallvec![obj_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap();
        match val {
            Value::Array(_, elems) => {
                assert_eq!(elems.len(), 2);
                let first_id = elems.get(0).copied().unwrap();
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
            Prim::values(&mut ctx, smallvec![obj_id]).await.unwrap()
        };

        let val = arena.get(result).unwrap().clone();

        // Should be Result.Ok(array)
        match val {
            Value::Tagged(_, 0, payload) => {
                let arr_id = payload.get(0).copied().unwrap();
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
            Prim::values(&mut ctx, smallvec![obj_id]).await.unwrap()
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
            Prim::from_entries(&mut ctx, smallvec![arr_id])
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
}
