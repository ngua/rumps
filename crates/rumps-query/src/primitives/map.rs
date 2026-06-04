use std::sync::Arc;

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{MapKey, Payload, ValueId};

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
            let map = Payload::Map(Arc::new(IndexMap::new()));
            Ok(ctx.add(map))
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
            let entries = ctx
                .arena
                .get_map(args[0])
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
    /// Returns an array of all keys in iteration order.
    pub(crate) fn keys<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let entries = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.keys", "Map"));

            // Collect keys before mutating arena
            let key_vals: SmallVec<[MapKey; 8]> =
                entries.keys().cloned().collect();

            let keys: SmallVec<[ValueId; 4]> =
                key_vals.iter().map(|k| ctx.add(k.to_payload())).collect();

            Ok(ctx.add(Payload::Array(Arc::new(keys))))
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
            let entries = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.values", "Map"));

            let vals: SmallVec<[ValueId; 4]> =
                entries.values().copied().collect();
            Ok(ctx.add(Payload::Array(Arc::new(vals))))
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
            let entries = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.entries", "Map"));

            // Collect entries before mutating arena
            let entry_pairs: SmallVec<[(MapKey, ValueId); 8]> =
                entries.iter().map(|(k, v)| (k.clone(), *v)).collect();

            let tuples: SmallVec<[ValueId; 4]> = entry_pairs
                .iter()
                .map(|(k, v_id)| {
                    let k_id = ctx.add(k.to_payload());
                    let tuple =
                        Payload::Tuple(Arc::new(smallvec![k_id, *v_id]));
                    ctx.add(tuple)
                })
                .collect();

            Ok(ctx.add(Payload::Array(Arc::new(tuples))))
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
                .payload(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_payload(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.has",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            let entries = ctx
                .arena
                .get_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.has", "Map"));

            let exists = entries.contains_key(&map_key);
            Ok(ctx.arena.add_typed(
                Payload::Bool(exists),
                ctx.runtime_types.meta_bool(),
                ctx.span,
            ))
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
                .payload(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_payload(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.lookup",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            let entries = ctx
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
                .payload(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_payload(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.insert",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            // Type checker guarantees key/value types match the map type
            let mut e = ctx
                .arena
                .take_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.insert", "Map"));

            e.insert(map_key, args[2]);

            Ok(ctx.add(Payload::Map(Arc::new(e))))
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
                .payload(args[1])
                .ok_or_else(|| ctx.runtime_error("invalid key value id"))?;

            let map_key = MapKey::from_payload(key).unwrap_or_else(|| {
                typechecked!(
                    "Map.remove",
                    "key must be scalar (Bool, Int, Float, Char, String)"
                )
            });

            let mut e = ctx
                .arena
                .take_map(args[0])
                .unwrap_or_else(|| typechecked!("Map.remove", "Map"));

            e.shift_remove(&map_key);

            Ok(ctx.add(Payload::Map(Arc::new(e))))
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
            let mut merged = ctx.arena.take_map(args[0]).unwrap_or_else(|| {
                typechecked!("Map.merge", "Map (first arg)")
            });

            let entries_b = ctx.arena.get_map(args[1]).unwrap_or_else(|| {
                typechecked!("Map.merge", "Map (second arg)")
            });

            // Type checker guarantees compatible map types
            merged.extend(entries_b.iter().map(|(k, v)| (k.clone(), *v)));

            Ok(ctx.add(Payload::Map(Arc::new(merged))))
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
            let arr = ctx
                .arena
                .get_array(args[0])
                .unwrap_or_else(|| typechecked!("Map.from-entries", "Array"));

            let mut entries = IndexMap::new();

            // Type checker guarantees array elements are 2-tuples
            arr.iter().for_each(|id| {
                let val = ctx.arena.payload(*id).unwrap_or_else(|| {
                    typechecked!("Map.from-entries", "ValueId")
                });

                match val {
                    Payload::Tuple(elems) if elems.len() == 2 => {
                        let k_val =
                            ctx.arena.payload(elems[0]).unwrap_or_else(|| {
                                typechecked!("Map.from-entries", "key ValueId")
                            });
                        let map_key = MapKey::from_payload(k_val)
                            .unwrap_or_else(|| {
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

            Ok(ctx.add(Payload::Map(Arc::new(entries))))
        })
    }
}
