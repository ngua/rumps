use std::sync::Arc;

use smallvec::{smallvec, SmallVec};

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Map as RumpsMap, Payload, ValueId};

pub(crate) struct Map;

impl Prim for Map {}

impl Map {
    /// `forall K V. () -> Map[K, V]`
    ///
    /// Creates an empty map in `O(1)`.
    pub(crate) fn empty<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let map = Payload::Map(Arc::new(RumpsMap::new()));
            Ok(ctx.add(map))
        })
    }

    /// `forall K V. (Map[K, V]) -> Int`
    ///
    /// Gives the number of entries in the map in `O(1)`.
    pub(crate) fn length<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let entries = ctx
                .arena
                .get_map(a)
                .unwrap_or_else(|| typechecked!("Map.length", "Map"));

            Ok(ctx.arena.add_typed(
                Payload::Int(entries.len() as i64),
                ctx.runtime_types.meta_int(),
                ctx.span,
            ))
        })
    }

    /// `forall K V. (Map[K, V]) -> Array[K]`
    ///
    /// Gives all keys in ascending key order. This is `O(n)` and allocates an
    /// `O(n)` array.
    pub(crate) fn keys<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let entries = ctx
                .arena
                .get_map(a)
                .unwrap_or_else(|| typechecked!("Map.keys", "Map"));

            Ok(ctx.add(Payload::Array(Arc::new(entries.keys()))))
        })
    }

    /// `forall K V. (Map[K, V]) -> Array[V]`
    ///
    /// Gives all values in ascending key order. This is `O(n)` and allocates
    /// an `O(n)` array.
    pub(crate) fn values<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let entries = ctx
                .arena
                .get_map(a)
                .unwrap_or_else(|| typechecked!("Map.values", "Map"));

            Ok(ctx.add(Payload::Array(Arc::new(entries.values()))))
        })
    }

    /// `forall K V. (Map[K, V]) -> Array[(K, V)]`
    ///
    /// Gives all `(key, value)` pairs in ascending key order. This is `O(n)`
    /// and allocates an `O(n)` array plus one tuple value per entry.
    pub(crate) fn entries<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let a = args[0];
            let entries = ctx
                .arena
                .get_map(a)
                .unwrap_or_else(|| typechecked!("Map.entries", "Map"));

            let entry_pairs = entries.entries();

            let tuples: SmallVec<[ValueId; 4]> = entry_pairs
                .into_iter()
                .map(|(k, v_id)| {
                    let tuple = Payload::Tuple(Arc::new(smallvec![k, v_id]));
                    ctx.add(tuple)
                })
                .collect();

            Ok(ctx.add(Payload::Array(Arc::new(tuples))))
        })
    }
}
