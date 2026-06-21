use std::cmp::Ordering;
use std::sync::Arc;

use futures::future::BoxFuture;
use smallvec::{smallvec, SmallVec};

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::typecheck::{RuntimeTyId, Ty, TyArena};
use crate::value::{Map as RumpsMap, MapNode, Payload, TypeId, ValueId};
use crate::{ClassId, Result};

pub(crate) struct Map;

impl Body for Map {}

impl Map {
    /// `forall K V. () -> Map[K, V]`
    ///
    /// Creates an empty map in `O(1)`.
    pub(crate) fn empty(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let map = Payload::Map(Arc::new(RumpsMap::new()));
        Ok(ctx.vals().add(map))
    }

    /// `forall K V. (Map[K, V]) -> Int`
    ///
    /// Gives the number of entries in the map in `O(1)`.
    pub(crate) fn length(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let len = Self::get(ctx, a, "Map.length")?.len();
        Ok(ctx.vals().add(Payload::Int(len as i64)))
    }

    /// `forall K V. (Map[K, V]) -> Array[K]`
    ///
    /// Gives all keys in ascending key order. This is `O(n)` and allocates an
    /// `O(n)` array.
    pub(crate) fn keys(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let keys = Self::get(ctx, a, "Map.keys")?.keys();
        Ok(ctx.vals().add(Payload::Array(Arc::new(keys))))
    }

    /// `forall K V. (Map[K, V]) -> Array[V]`
    ///
    /// Gives all values in ascending key order. This is `O(n)` and allocates
    /// an `O(n)` array.
    pub(crate) fn values(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let vals = Self::get(ctx, a, "Map.values")?.values();
        Ok(ctx.vals().add(Payload::Array(Arc::new(vals))))
    }

    /// `forall K V. (Map[K, V]) -> Array[(K, V)]`
    ///
    /// Gives all `(key, value)` pairs in ascending key order. This is `O(n)`
    /// and allocates an `O(n)` array plus one tuple value per entry.
    pub(crate) fn entries(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let entry_pairs = Self::get(ctx, a, "Map.entries")?.entries();
        let tuples: SmallVec<[ValueId; 4]> = entry_pairs
            .into_iter()
            .map(|(k, v_id)| {
                let tuple = Payload::Tuple(Arc::new(smallvec![k, v_id]));
                ctx.vals().add(tuple)
            })
            .collect();

        Ok(ctx.vals().add(Payload::Array(Arc::new(tuples))))
    }

    /// `forall K V. (Map[K, V], K) -> Bool`
    ///
    /// Returns a `BoxFuture` because map lookup calls `Ord:compare` and may
    /// run `async` user class code.
    pub(crate) fn has<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let k = args[1];
            let m = Self::get(ctx, a, "Map.has")?;
            let b = ctx.maps().lookup(&m, k).await?.is_some();
            Ok(ctx.vals().add(Payload::Bool(b)))
        })
    }

    /// `forall K V. (Map[K, V], K) -> Option[V]`
    ///
    /// Returns a `BoxFuture` because map lookup calls `Ord:compare` and may
    /// run `async` user class code.
    pub(crate) fn lookup<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let k = args[1];
            let m = Self::get(ctx, a, "Map.lookup")?;
            let val = ctx
                .vals()
                .meta(a)
                .and_then(|meta| {
                    [meta.ty, meta.repr].into_iter().find_map(|ty| {
                        match ctx.vals().ty(ty) {
                            Ty::Map(_, v) => Some(RuntimeTyId::from(v)),
                            _ => None,
                        }
                    })
                })
                .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
            let val = ctx.vals().runtime_ty(val);
            let v = ctx.maps().lookup(&m, k).await?;
            let payload = v.map(Payload::some).unwrap_or_else(Payload::none);
            Ok(ctx
                .vals()
                .add_variant(payload, TypeId::OPTION, smallvec![val]))
        })
    }

    /// `forall K V. (Map[K, V], K, V) -> Map[K, V]`
    ///
    /// Returns a `BoxFuture` because map insertion calls `Ord:compare` and may
    /// run `async` user class code.
    pub(crate) fn insert<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let k = args[1];
            let v = args[2];
            let m = Self::get(ctx, a, "Map.insert")?;
            let meta = ctx.vals().meta(a);
            let m = ctx.maps().insert(&m, k, v).await?;
            let payload = Payload::Map(Arc::new(m));
            match meta {
                Some(meta) => Ok(ctx.vals().add_meta(payload, meta)),
                None => Ok(ctx.vals().add(payload)),
            }
        })
    }

    /// `forall K V. (Map[K, V], K) -> Map[K, V]`
    ///
    /// Returns a `BoxFuture` because map removal calls `Ord:compare` and may
    /// run `async` user class code.
    pub(crate) fn remove<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let k = args[1];
            let m = Self::get(ctx, a, "Map.remove")?;
            let meta = ctx.vals().meta(a);
            let m = ctx.maps().remove(&m, k).await?;
            let payload = Payload::Map(Arc::new(m));
            match meta {
                Some(meta) => Ok(ctx.vals().add_meta(payload, meta)),
                None => Ok(ctx.vals().add(payload)),
            }
        })
    }

    /// `forall K V. (Map[K, V], Map[K, V]) -> Map[K, V]`
    ///
    /// Returns a `BoxFuture` because map merge calls `Ord:compare` and may run
    /// `async` user class code.
    pub(crate) fn merge<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let b = args[1];
            let l = Self::get(ctx, a, "Map.merge")?;
            let r = Self::get(ctx, b, "Map.merge")?;
            let meta = ctx.vals().meta(a);
            let m = ctx.maps().merge(&l, &r).await?;
            let payload = Payload::Map(Arc::new(m));
            match meta {
                Some(meta) => Ok(ctx.vals().add_meta(payload, meta)),
                None => Ok(ctx.vals().add(payload)),
            }
        })
    }

    /// `forall K V. (Array[(K, V)]) -> Map[K, V]`
    ///
    /// Returns a `BoxFuture` because building the map calls `Ord:compare` and
    /// may run `async` user class code.
    pub(crate) fn from_entries<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let metas = ctx.vals().meta(a).and_then(|meta| {
                [meta.ty, meta.repr].into_iter().find_map(|ty| {
                    match ctx.vals().ty(ty) {
                        Ty::Array(elem) => {
                            let elem =
                                ctx.vals().runtime_ty(RuntimeTyId::from(elem));
                            match ctx.vals().ty(elem) {
                                Ty::Tuple(elems) => {
                                    let k = elems.first().copied()?;
                                    let v = elems.get(1).copied()?;
                                    let k = ctx
                                        .vals()
                                        .runtime_ty(RuntimeTyId::from(k));
                                    let v = ctx
                                        .vals()
                                        .runtime_ty(RuntimeTyId::from(v));
                                    Some((k, v))
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                })
            });
            let ids = ctx.vals().array_ids(a, "Map.from-entries")?;
            let entries: Vec<_> = ids
                .iter()
                .map(|id| {
                    let (k, v) = match ctx.vals().payload(*id)? {
                        Payload::Tuple(ids) => {
                            match (ids.first(), ids.get(1), ids.get(2)) {
                                (Some(k), Some(v), None) => (*k, *v),
                                _ => typechecked!(
                                    "Map.from-entries",
                                    "Array[(K, V)]"
                                ),
                            }
                        }
                        _ => typechecked!("Map.from-entries", "Array[(K, V)]"),
                    };
                    match metas {
                        Some((k_ty, v_ty)) => {
                            let k_meta = ctx.vals().meta_for_ty(k_ty);
                            let v_meta = ctx.vals().meta_for_ty(v_ty);
                            let k = ctx.vals().id_with_meta(k, k_meta);
                            let v = ctx.vals().id_with_meta(v, v_meta);
                            Ok((k, v))
                        }
                        None => Ok((k, v)),
                    }
                })
                .collect::<Result<_>>()?;
            let m = ctx.maps().from_entries(entries).await?;
            Ok(ctx.vals().add(Payload::Map(Arc::new(m))))
        })
    }

    /// `forall K V W. ((V) -> W, Map[K, V]) -> Map[K, W]`
    ///
    /// Applies a callback to each map value and keeps the original keys.
    /// Returns a `BoxFuture` because invoking `f` may run `async` user code.
    pub(crate) fn map<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let es = ctx.vals().map_entries(a, "Map.map")?;
            let mut it = es.iter().copied();
            let mut vals: SmallVec<[ValueId; 4]> = SmallVec::new();

            while let Some((_, v)) = it.next() {
                vals.push(ctx.invoke(f, smallvec![v]).await?);
            }

            let m = Self::rebuild(es.as_slice(), vals.as_slice());
            Ok(ctx.vals().add(Payload::Map(Arc::new(m))))
        })
    }

    /// `forall K V W. ((K, V) -> W, Map[K, V]) -> Map[K, W]`
    ///
    /// Applies a callback to each map key and value pair and keeps the original
    /// keys. Returns a `BoxFuture` because invoking `f` may run `async` user
    /// code.
    pub(crate) fn map_with_key<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let es = ctx.vals().map_entries(a, "Map.map-with-key")?;
            let mut it = es.iter().copied();
            let mut vals: SmallVec<[ValueId; 4]> = SmallVec::new();

            while let Some((k, v)) = it.next() {
                vals.push(ctx.invoke(f, smallvec![k, v]).await?);
            }

            let m = Self::rebuild(es.as_slice(), vals.as_slice());
            Ok(ctx.vals().add(Payload::Map(Arc::new(m))))
        })
    }

    /// `forall K V W. ((V) -> W, Map[K, V]) -> Unit`
    ///
    /// Invokes a callback for each map value and returns `Unit`.
    /// Returns a `BoxFuture` because invoking `f` may run `async` user code.
    pub(crate) fn foreach<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let es = ctx.vals().map_entries(a, "Map.foreach")?;
            let mut it = es.into_iter();

            while let Some((_, v)) = it.next() {
                ctx.invoke(f, smallvec![v]).await?;
            }

            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `forall K V W. ((K, V) -> W, Map[K, V]) -> Unit`
    ///
    /// Invokes a callback for each map key and value pair and returns `Unit`.
    /// Returns a `BoxFuture` because invoking `f` may run `async` user code.
    pub(crate) fn foreach_with_key<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let es = ctx.vals().map_entries(a, "Map.foreach-with-key")?;
            let mut it = es.into_iter();

            while let Some((k, v)) = it.next() {
                ctx.invoke(f, smallvec![k, v]).await?;
            }

            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `forall K V A. ((A, V) -> A, A, Map[K, V]) -> A`
    ///
    /// Left folds over map values in ascending key order.
    /// Returns a `BoxFuture` because invoking `f` may run `async` user code.
    pub(crate) fn fold<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let mut acc = args[1];
            let a = args[2];
            let es = ctx.vals().map_entries(a, "Map.fold")?;
            let mut it = es.into_iter();

            while let Some((_, v)) = it.next() {
                acc = ctx.invoke(f, smallvec![acc, v]).await?;
            }

            Ok(acc)
        })
    }

    /// `forall K V A. ((A, K, V) -> A, A, Map[K, V]) -> A`
    ///
    /// Left folds over map key and value pairs in ascending key order.
    /// Returns a `BoxFuture` because invoking `f` may run `async` user code.
    pub(crate) fn fold_with_key<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let mut acc = args[1];
            let a = args[2];
            let es = ctx.vals().map_entries(a, "Map.fold-with-key")?;
            let mut it = es.into_iter();

            while let Some((k, v)) = it.next() {
                acc = ctx.invoke(f, smallvec![acc, k, v]).await?;
            }

            Ok(acc)
        })
    }

    /// `forall K L V W. ((K, V) -> (L, W), Map[K, V]) -> Map[L, W]`
    ///
    /// Applies a callback to each entry and rebuilds a map from the returned
    /// entries. Duplicate rebuilt keys keep the first stored key and replace
    /// its value, matching `Map.insert` semantics. Returns a `BoxFuture`
    /// because invoking `f` and calling `Ord:compare` may run `async` user
    /// code.
    pub(crate) fn map_entries<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args
                .first()
                .copied()
                .unwrap_or_else(|| typechecked!("Map.map-entries", "mapper"));
            let a = args
                .get(1)
                .copied()
                .unwrap_or_else(|| typechecked!("Map.map-entries", "Map"));
            let es = ctx.vals().map_entries(a, "Map.map-entries")?;
            let mut src = es.into_iter();
            let mut mapped: SmallVec<[(ValueId, ValueId); 8]> = SmallVec::new();

            while let Some((k, v)) = src.next() {
                let id = ctx.invoke(f, smallvec![k, v]).await?;
                let tuple = match ctx.vals().payload(id)? {
                    Payload::Tuple(ids) => {
                        match (ids.first(), ids.get(1), ids.get(2)) {
                            (Some(k), Some(v), None) => (*k, *v),
                            _ => typechecked!("Map.map-entries", "(K, V)"),
                        }
                    }
                    _ => typechecked!("Map.map-entries", "(K, V)"),
                };
                mapped.push(tuple);
            }

            struct Frame {
                dir: Ordering,
                k: ValueId,
                v: ValueId,
                sibling: Option<Arc<MapNode>>,
            }

            let cmp = ctx.vals().intern("compare");
            let ord_ty = ctx.vals().type_id(TypeId::ORDERING);
            let mut m = RumpsMap::new();
            let mut entries = mapped.into_iter();

            while let Some((k, v)) = entries.next() {
                match m.root() {
                    Some(root) => {
                        let mut node = Some(root);
                        let mut path = Vec::new();
                        let mut done = None;

                        while let Some(n) = node {
                            let id = ctx
                                .class_call(
                                    ClassId::ORD,
                                    cmp,
                                    smallvec![k, n.key()],
                                    Some(ord_ty),
                                )
                                .await?;
                            let value = ctx.vals().value(id)?.clone();
                            let ty = ctx.vals().value_variant_base_type(&value);
                            let ord = match &value.payload {
                                Payload::Int(n) => n.cmp(&0),
                                Payload::Variant { tag, .. }
                                    if ty.is_some_and(|ty| {
                                        ty == TypeId::ORDERING
                                    }) =>
                                {
                                    match tag {
                                        0 => Ordering::Less,
                                        1 => Ordering::Equal,
                                        2 => Ordering::Greater,
                                        _ => typechecked!(
                                            "Map.map-entries",
                                            "Ord:compare result"
                                        ),
                                    }
                                }
                                _ => typechecked!(
                                    "Map.map-entries",
                                    "Ord:compare result"
                                ),
                            };

                            match ord {
                                Ordering::Less => match n.left() {
                                    Some(left) => {
                                        path.push(Frame {
                                            dir: Ordering::Less,
                                            k: n.key(),
                                            v: n.val(),
                                            sibling: n.right(),
                                        });
                                        node = Some(left);
                                    }
                                    None => {
                                        let child =
                                            MapNode::new(k, v, None, None);
                                        let root = MapNode::balance(
                                            n.key(),
                                            n.val(),
                                            Some(child),
                                            n.right(),
                                        );
                                        done = Some((root, true));
                                        node = None;
                                    }
                                },
                                Ordering::Equal => {
                                    let root = MapNode::balance(
                                        n.key(),
                                        v,
                                        n.left(),
                                        n.right(),
                                    );
                                    done = Some((root, false));
                                    node = None;
                                }
                                Ordering::Greater => match n.right() {
                                    Some(right) => {
                                        path.push(Frame {
                                            dir: Ordering::Greater,
                                            k: n.key(),
                                            v: n.val(),
                                            sibling: n.left(),
                                        });
                                        node = Some(right);
                                    }
                                    None => {
                                        let child =
                                            MapNode::new(k, v, None, None);
                                        let root = MapNode::balance(
                                            n.key(),
                                            n.val(),
                                            n.left(),
                                            Some(child),
                                        );
                                        done = Some((root, true));
                                        node = None;
                                    }
                                },
                            }
                        }

                        let (mut root, added) = done.unwrap_or_else(|| {
                            invariant!("Map.map-entries insert result")
                        });
                        while let Some(frame) = path.pop() {
                            root = if frame.dir == Ordering::Less {
                                MapNode::balance(
                                    frame.k,
                                    frame.v,
                                    Some(root),
                                    frame.sibling,
                                )
                            } else {
                                MapNode::balance(
                                    frame.k,
                                    frame.v,
                                    frame.sibling,
                                    Some(root),
                                )
                            };
                        }
                        let len = if added { m.len() + 1 } else { m.len() };
                        m = RumpsMap::from_root(Some(root), len);
                    }
                    None => {
                        let root = MapNode::new(k, v, None, None);
                        m = RumpsMap::from_root(Some(root), 1);
                    }
                }
            }

            Ok(ctx.vals().add(Payload::Map(Arc::new(m))))
        })
    }

    fn get(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<Arc<RumpsMap>> {
        match ctx.vals().payload(id)? {
            Payload::Map(m) => Ok(m.clone()),
            _ => typechecked!(label, "Map"),
        }
    }

    fn rebuild(es: &[(ValueId, ValueId)], vals: &[ValueId]) -> RumpsMap {
        fn root(
            es: &[(ValueId, ValueId)],
            vals: &[ValueId],
        ) -> Option<Arc<MapNode>> {
            if es.is_empty() {
                None
            } else {
                let mid = es.len() / 2;
                let (ls, rest) = es.split_at(mid);
                let (l_vals, rest_vals) = vals.split_at(mid);
                let (mid_vals, r_vals) = rest_vals.split_at(1);
                let (k, _) = rest
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Map entries have middle"));
                let v = *mid_vals
                    .first()
                    .unwrap_or_else(|| invariant!("Map values have middle"));
                Some(MapNode::new(
                    k,
                    v,
                    root(ls, l_vals),
                    root(
                        rest.get(1..)
                            .unwrap_or_else(|| invariant!("Map right slice")),
                        r_vals,
                    ),
                ))
            }
        }

        RumpsMap::from_root(root(es, vals), es.len())
    }
}
