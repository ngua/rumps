use std::cmp::Ordering;
use std::sync::Arc;

use smallvec::{smallvec, SmallVec};

use super::{
    Compare, Continuation, MapInsertFrame, Registry, ResultMode, State, Step,
};
use crate::intern::StringInterner;
use crate::interpreter::class::ClassCtx;
use crate::value::{Map, MapNode, Payload, ValueId};
use crate::Result;

/// HoF starters for `Map` module functions.
pub(super) struct Fns;

impl Fns {
    pub(super) fn register(reg: &mut Registry, i: &mut StringInterner) {
        let map = i.intern("Map");
        let map_fn = i.intern("map");
        let map_with_key = i.intern("map-with-key");
        let foreach = i.intern("foreach");
        let foreach_with_key = i.intern("foreach-with-key");
        let map_entries = i.intern("map-entries");
        let k = ResultMode::Keep;
        let d = ResultMode::Discard;

        reg.register(map, map_fn, Self::map, k);
        reg.register(map, map_with_key, Self::map_with_key, k);
        reg.register(map, foreach, Self::foreach, d);
        reg.register(map, foreach_with_key, Self::foreach_with_key, d);
        reg.register(map, map_entries, Self::map_entries, k);
    }

    fn map(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        Self::start_map(ctx, args, false, "Map.map")
    }

    fn map_with_key(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        Self::start_map(ctx, args, true, "Map.map-with-key")
    }

    fn foreach(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        Self::start_foreach(ctx, args, false, "Map.foreach")
    }

    fn foreach_with_key(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<Step> {
        Self::start_foreach(ctx, args, true, "Map.foreach-with-key")
    }

    fn map_entries(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let f = *args
            .first()
            .unwrap_or_else(|| typechecked!("Map.map-entries", "2 args"));
        let m = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Map.map-entries", "2 args"));
        let es = ctx
            .arena
            .get_map(m)
            .map(Map::entries)
            .unwrap_or_else(|| typechecked!("Map.map-entries", "Map"));

        match es.first().copied() {
            Some((k, v)) => Ok(Step::Invoke(Continuation {
                callee: f,
                args: smallvec![k, v],
                state: State::MapModuleEntriesCollect {
                    entries: es,
                    idx: 0,
                    acc: SmallVec::new(),
                },
            })),
            None => Ok(Step::Done(Payload::Map(Arc::new(Map::new())))),
        }
    }

    fn start_map(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
        with_key: bool,
        label: &'static str,
    ) -> Result<Step> {
        let f = *args
            .first()
            .unwrap_or_else(|| typechecked!(label, "2 args"));
        let m = *args.get(1).unwrap_or_else(|| typechecked!(label, "2 args"));
        let es = ctx
            .arena
            .get_map(m)
            .map(Map::entries)
            .unwrap_or_else(|| typechecked!(label, "Map"));

        match es.first().copied() {
            Some(e) => Ok(Step::Invoke(Continuation {
                callee: f,
                args: Self::args(e, with_key),
                state: State::MapModuleMap {
                    source: m,
                    entries: es,
                    idx: 0,
                    acc: SmallVec::new(),
                    with_key,
                },
            })),
            None => Ok(Step::Done(Payload::Map(Arc::new(Map::new())))),
        }
    }

    fn start_foreach(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
        with_key: bool,
        label: &'static str,
    ) -> Result<Step> {
        let f = *args
            .first()
            .unwrap_or_else(|| typechecked!(label, "2 args"));
        let m = *args.get(1).unwrap_or_else(|| typechecked!(label, "2 args"));
        let es = ctx
            .arena
            .get_map(m)
            .map(Map::entries)
            .unwrap_or_else(|| typechecked!(label, "Map"));

        match es.first().copied() {
            Some(e) => Ok(Step::Invoke(Continuation {
                callee: f,
                args: Self::args(e, with_key),
                state: State::MapModuleForeach {
                    source: m,
                    entries: es,
                    idx: 0,
                    with_key,
                },
            })),
            None => Ok(Step::Done(Payload::Unit)),
        }
    }

    pub(super) fn args(
        (k, v): (ValueId, ValueId),
        with_key: bool,
    ) -> SmallVec<[ValueId; 2]> {
        if with_key {
            smallvec![k, v]
        } else {
            smallvec![v]
        }
    }

    pub(super) fn rebuild(es: &[(ValueId, ValueId)], vals: &[ValueId]) -> Map {
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

        Map::from_root(root(es, vals), es.len())
    }

    pub(super) fn entry(ctx: &ClassCtx<'_>, id: ValueId) -> (ValueId, ValueId) {
        let elems = ctx
            .arena
            .get_tuple(id)
            .unwrap_or_else(|| typechecked!("Map.map-entries", "(K, V)"));
        match (elems.first(), elems.get(1), elems.get(2)) {
            (Some(k), Some(v), None) => (*k, *v),
            _ => typechecked!("Map.map-entries", "(K, V)"),
        }
    }

    pub(super) fn insert_all(
        m: Map,
        es: SmallVec<[(ValueId, ValueId); 8]>,
        idx: usize,
    ) -> Result<Step> {
        match es.get(idx).copied() {
            Some((k, v)) => match m.root() {
                Some(n) => Ok(Self::compare_insert(m, es, idx, n, Vec::new())),
                None => {
                    let m =
                        Map::from_root(Some(MapNode::new(k, v, None, None)), 1);
                    Self::insert_all(m, es, idx + 1)
                }
            },
            None => Ok(Step::Done(Payload::Map(Arc::new(m)))),
        }
    }

    pub(super) fn resume_insert(
        ord: Ordering,
        m: Map,
        es: SmallVec<[(ValueId, ValueId); 8]>,
        idx: usize,
        n: Arc<MapNode>,
        path: Vec<MapInsertFrame>,
    ) -> Result<Step> {
        let (k, v) = es
            .get(idx)
            .copied()
            .unwrap_or_else(|| invariant!("Map.map-entries insert entry"));
        match ord {
            Ordering::Less => match n.left() {
                Some(left) => {
                    let mut path = path;
                    path.push(MapInsertFrame::Left {
                        key: n.key(),
                        val: n.val(),
                        right: n.right(),
                    });
                    Ok(Self::compare_insert(m, es, idx, left, path))
                }
                None => {
                    let child = MapNode::new(k, v, None, None);
                    let root = MapNode::balance(
                        n.key(),
                        n.val(),
                        Some(child),
                        n.right(),
                    );
                    let root = Self::finish_insert(path.as_slice(), root);
                    let m = Map::from_root(Some(root), m.len() + 1);
                    Self::insert_all(m, es, idx + 1)
                }
            },
            Ordering::Equal => {
                let root = MapNode::balance(n.key(), v, n.left(), n.right());
                let root = Self::finish_insert(path.as_slice(), root);
                let m = Map::from_root(Some(root), m.len());
                Self::insert_all(m, es, idx + 1)
            }
            Ordering::Greater => match n.right() {
                Some(right) => {
                    let mut path = path;
                    path.push(MapInsertFrame::Right {
                        key: n.key(),
                        val: n.val(),
                        left: n.left(),
                    });
                    Ok(Self::compare_insert(m, es, idx, right, path))
                }
                None => {
                    let child = MapNode::new(k, v, None, None);
                    let root = MapNode::balance(
                        n.key(),
                        n.val(),
                        n.left(),
                        Some(child),
                    );
                    let root = Self::finish_insert(path.as_slice(), root);
                    let m = Map::from_root(Some(root), m.len() + 1);
                    Self::insert_all(m, es, idx + 1)
                }
            },
        }
    }

    fn compare_insert(
        m: Map,
        es: SmallVec<[(ValueId, ValueId); 8]>,
        idx: usize,
        n: Arc<MapNode>,
        path: Vec<MapInsertFrame>,
    ) -> Step {
        let (k, _) = es
            .get(idx)
            .copied()
            .unwrap_or_else(|| invariant!("Map.map-entries insert entry"));
        Step::Compare(Compare {
            args: smallvec![k, n.key()],
            state: State::MapModuleEntriesInsert {
                map: m,
                entries: es,
                idx,
                node: n,
                path,
            },
        })
    }

    fn finish_insert(path: &[MapInsertFrame], n: Arc<MapNode>) -> Arc<MapNode> {
        match path.split_last() {
            Some((MapInsertFrame::Left { key, val, right }, rest)) => {
                let n = MapNode::balance(*key, *val, Some(n), right.clone());
                Self::finish_insert(rest, n)
            }
            Some((MapInsertFrame::Right { key, val, left }, rest)) => {
                let n = MapNode::balance(*key, *val, left.clone(), Some(n));
                Self::finish_insert(rest, n)
            }
            None => n,
        }
    }
}
