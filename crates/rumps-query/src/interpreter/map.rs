use std::cmp::Ordering;
use std::sync::Arc;

use async_recursion::async_recursion;
use smallvec::smallvec;

use super::call::ClassDispatch;
use super::Interpreter;
use crate::intern::StringId;
use crate::io::IoContext;
use crate::typecheck::{RuntimeTyId, Ty, TyArena};
use crate::value::{Map, MapNode, Payload, TypeId, Value, ValueId, ValueMeta};
use crate::{ClassId, Result, Span};

/// Async `Map` operations use `c` for the cost of one RUMPS `Ord:compare`.
/// Builtin scalar ordering keeps `c` small, but user `Ord` instances can run
/// RUMPS code.
///
/// `Map.has`, `Map.lookup`, `Map.insert`, and `Map.remove` are
/// `O(log n * c)` on an AVL tree. Updates allocate `O(log n)` new nodes and
/// preserve the old map by sharing unchanged subtrees.
///
/// `Map.from-entries` folds entries left to right with repeated insertions, so
/// it is `O(n log n * c)`. `Map.merge(l, r)` inserts each of the `m` entries in
/// `r` into `l`, so it is `O(m log(n + m) * c)`.
///
/// `Map:Eq` and `Map:Ord` currently materialize sorted entries before
/// comparing. They use `O(n)` temporary storage, then compare entries from left
/// to right.
///
/// `Map` is a convenient language value. For large persistent datasets or hot
/// local mutation paths, prefer RUMPS storage operations or mutable refs.
impl<I: IoContext> Interpreter<'_, I> {
    pub(super) async fn invoke_async_map_module_fn(
        &mut self,
        path: &[StringId],
        args: &[ValueId],
        span: Span,
    ) -> Result<Option<Value>> {
        let map = self.arena.intern("Map");
        let method = path.get(1).and_then(|id| self.arena.get_str(*id));

        if path.first().copied() == Some(map) {
            match method {
                Some("has") => {
                    let m = self.map_arg(args, 0, "Map.has")?.clone();
                    let k = self.value_arg(args, 1, "Map.has")?;
                    let b = self.map_lookup_id(&m, k, span).await?.is_some();
                    Ok(Some(self.value_from_meta(
                        Payload::Bool(b),
                        self.checked.types.meta_bool(),
                    )))
                }
                Some("lookup") => {
                    let m_id = self.value_arg(args, 0, "Map.lookup")?;
                    let m = self
                        .arena
                        .get_map(m_id)
                        .unwrap_or_else(|| typechecked!("Map.lookup", "Map"))
                        .clone();
                    let k = self.value_arg(args, 1, "Map.lookup")?;
                    let v = self.map_lookup_id(&m, k, span).await?;
                    let meta = self.map_lookup_meta(m_id);
                    Ok(Some(self.value_from_meta(
                        v.map(Payload::some).unwrap_or_else(Payload::none),
                        meta,
                    )))
                }
                Some("insert") => {
                    let m_id = self.value_arg(args, 0, "Map.insert")?;
                    let m = self
                        .arena
                        .get_map(m_id)
                        .unwrap_or_else(|| typechecked!("Map.insert", "Map"))
                        .clone();
                    let k = self.value_arg(args, 1, "Map.insert")?;
                    let v = self.value_arg(args, 2, "Map.insert")?;
                    let m = self.map_insert_id(&m, k, v, span).await?;
                    Ok(Some(self.value_from_map_payload(m, Some(m_id))))
                }
                Some("remove") => {
                    let m_id = self.value_arg(args, 0, "Map.remove")?;
                    let m = self
                        .arena
                        .get_map(m_id)
                        .unwrap_or_else(|| typechecked!("Map.remove", "Map"))
                        .clone();
                    let k = self.value_arg(args, 1, "Map.remove")?;
                    let m = self.map_remove_id(&m, k, span).await?;
                    Ok(Some(self.value_from_map_payload(m, Some(m_id))))
                }
                Some("merge") => {
                    let l_id = self.value_arg(args, 0, "Map.merge")?;
                    let l = self
                        .arena
                        .get_map(l_id)
                        .unwrap_or_else(|| typechecked!("Map.merge", "Map"))
                        .clone();
                    let r = self.map_arg(args, 1, "Map.merge")?.clone();
                    let m = self.map_merge_maps(&l, &r, span).await?;
                    Ok(Some(self.value_from_map_payload(m, Some(l_id))))
                }
                Some("from-entries") => {
                    let arr_id = self.value_arg(args, 0, "Map.from-entries")?;
                    let metas = self.entry_metas(arr_id);
                    let ids = self
                        .arena
                        .get_array(arr_id)
                        .unwrap_or_else(|| {
                            typechecked!("Map.from-entries", "Array")
                        })
                        .clone();
                    let entries: Vec<_> = ids
                        .iter()
                        .map(|id| {
                            let elems = self
                                .arena
                                .get_tuple(*id)
                                .unwrap_or_else(|| {
                                    typechecked!(
                                        "Map.from-entries",
                                        "Array[(K, V)]"
                                    )
                                });
                            let k = *elems.first().unwrap_or_else(|| {
                                typechecked!("Map.from-entries", "key")
                            });
                            let v = *elems.get(1).unwrap_or_else(|| {
                                typechecked!("Map.from-entries", "value")
                            });
                            match metas {
                                Some((k_meta, v_meta)) => {
                                    let k = self.id_with_meta(k, k_meta, span);
                                    let v = self.id_with_meta(v, v_meta, span);
                                    (k, v)
                                }
                                None => (k, v),
                            }
                        })
                        .collect();
                    let m = self
                        .map_from_entries(entries.into_iter(), span)
                        .await?;
                    Ok(Some(self.value_from_map_payload(m, None)))
                }
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    fn value_from_map_payload(
        &mut self,
        map: Map,
        src_id: Option<ValueId>,
    ) -> Value {
        let payload = Payload::Map(Arc::new(map));
        match src_id.and_then(|id| self.arena.meta(id)) {
            Some(meta) => self.value_from_meta(payload, meta),
            None => self.value_from_payload(payload),
        }
    }

    fn map_lookup_meta(&mut self, id: ValueId) -> ValueMeta {
        let val = self
            .arena
            .meta(id)
            .and_then(|meta| {
                [meta.ty, meta.repr].into_iter().find_map(|ty| {
                    match self.checked.types.get(ty) {
                        Ty::Map(_, v) => Some(RuntimeTyId::from(*v)),
                        _ => None,
                    }
                })
            })
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
        let val = self.runtime_ty(val);
        let opt = self.checked.types.option(val);
        self.checked.types.meta(opt)
    }

    fn entry_metas(&mut self, id: ValueId) -> Option<(ValueMeta, ValueMeta)> {
        let meta = self.arena.meta(id)?;
        [meta.ty, meta.repr]
            .into_iter()
            .find_map(|ty| self.entry_metas_ty(ty))
    }

    fn entry_metas_ty(
        &mut self,
        ty: RuntimeTyId,
    ) -> Option<(ValueMeta, ValueMeta)> {
        let ty = self.runtime_ty(ty);
        match self.checked.types.get(ty).clone() {
            Ty::Array(elem) => {
                let elem = self.runtime_ty(RuntimeTyId::from(elem));
                match self.checked.types.get(elem).clone() {
                    Ty::Tuple(elems) => {
                        let k = RuntimeTyId::from(*elems.first()?);
                        let v = RuntimeTyId::from(*elems.get(1)?);
                        let k = self.runtime_ty(k);
                        let v = self.runtime_ty(v);
                        Some((
                            self.checked.types.meta(k),
                            self.checked.types.meta(v),
                        ))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn id_with_meta(
        &mut self,
        id: ValueId,
        meta: ValueMeta,
        span: Span,
    ) -> ValueId {
        let val = self
            .arena
            .value(id)
            .cloned()
            .unwrap_or_else(|| invariant!("map entry value in arena"));
        let span = self.arena.span(id).unwrap_or(span);
        let val = self.value_with_context_meta(val, meta);
        self.add_value(val, span)
    }

    fn value_arg(
        &self,
        args: &[ValueId],
        idx: usize,
        ctx: &'static str,
    ) -> Result<ValueId> {
        args.get(idx)
            .copied()
            .ok_or_else(|| typechecked!(ctx, "argument"))
    }

    fn map_arg(
        &self,
        args: &[ValueId],
        idx: usize,
        ctx: &'static str,
    ) -> Result<&Map> {
        self.arena
            .get_map(self.value_arg(args, idx, ctx)?)
            .ok_or_else(|| typechecked!(ctx, "Map"))
    }

    pub(super) async fn cmp_value_ids(
        &mut self,
        l: ValueId,
        r: ValueId,
        span: Span,
    ) -> Result<Ordering> {
        let method = self.arena.intern("compare");
        let value = self
            .dispatch_class_method_value(ClassDispatch {
                dispatch_expr_id: None,
                output_expr_id: None,
                output_ty: Some(RuntimeTyId::from(TyArena::ORDERING)),
                class: ClassId::ORD,
                method,
                args: smallvec![l, r],
                span,
            })
            .await?;
        let ty = self
            .checked
            .types
            .to_type_id(value.repr)
            .or_else(|| self.checked.types.to_type_id(value.ty));

        match value.payload {
            Payload::Variant { tag: 0, .. }
                if ty.is_some_and(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Less)
            }
            Payload::Variant { tag: 1, .. }
                if ty.is_some_and(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Equal)
            }
            Payload::Variant { tag: 2, .. }
                if ty.is_some_and(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Greater)
            }
            Payload::Int(n) if n < 0 => Ok(Ordering::Less),
            Payload::Int(0) => Ok(Ordering::Equal),
            Payload::Int(_) => Ok(Ordering::Greater),
            _ => typechecked!("Ord:compare", "Ordering | Int"),
        }
    }

    pub(super) async fn eq_value_ids(
        &mut self,
        l: ValueId,
        r: ValueId,
        span: Span,
    ) -> Result<bool> {
        let method = self.arena.intern("eq");
        let value = self
            .dispatch_class_method_value(ClassDispatch {
                dispatch_expr_id: None,
                output_expr_id: None,
                output_ty: Some(RuntimeTyId::from(TyArena::BOOL)),
                class: ClassId::EQ,
                method,
                args: smallvec![l, r],
                span,
            })
            .await?;

        match value.payload {
            Payload::Bool(b) => Ok(b),
            _ => typechecked!("Eq:eq", "Bool"),
        }
    }

    pub(super) async fn map_lookup_id(
        &mut self,
        map: &Map,
        key: ValueId,
        span: Span,
    ) -> Result<Option<ValueId>> {
        self.map_lookup_node(map.root(), key, span).await
    }

    pub(super) async fn map_insert_id(
        &mut self,
        map: &Map,
        key: ValueId,
        val: ValueId,
        span: Span,
    ) -> Result<Map> {
        let (root, added) =
            self.map_insert_node(map.root(), key, val, span).await?;
        let len = if added {
            map.len().saturating_add(1)
        } else {
            map.len()
        };
        Ok(Map::from_root(root, len))
    }

    pub(super) async fn map_remove_id(
        &mut self,
        map: &Map,
        key: ValueId,
        span: Span,
    ) -> Result<Map> {
        let (root, removed) =
            self.map_remove_node(map.root(), key, span).await?;
        let len = if removed {
            map.len().saturating_sub(1)
        } else {
            map.len()
        };
        Ok(Map::from_root(root, len))
    }

    pub(super) async fn map_merge_maps(
        &mut self,
        l: &Map,
        r: &Map,
        span: Span,
    ) -> Result<Map> {
        let entries = r.entries();
        self.map_insert_entries(l.clone(), entries.as_slice(), span)
            .await
    }

    pub(super) async fn map_from_entries(
        &mut self,
        entries: impl IntoIterator<Item = (ValueId, ValueId)>,
        span: Span,
    ) -> Result<Map> {
        let entries: Vec<_> = entries.into_iter().collect();
        self.map_insert_entries(Map::new(), entries.as_slice(), span)
            .await
    }

    pub(super) async fn map_eq_maps(
        &mut self,
        l: &Map,
        r: &Map,
        span: Span,
    ) -> Result<bool> {
        if l.len() == r.len() {
            let l_entries = l.entries();
            let r_entries = r.entries();
            self.map_eq_entries(
                l_entries.as_slice(),
                r_entries.as_slice(),
                span,
            )
            .await
        } else {
            Ok(false)
        }
    }

    pub(super) async fn map_cmp_maps(
        &mut self,
        l: &Map,
        r: &Map,
        span: Span,
    ) -> Result<Ordering> {
        let l_entries = l.entries();
        let r_entries = r.entries();
        self.map_cmp_entries(l_entries.as_slice(), r_entries.as_slice(), span)
            .await
    }

    #[async_recursion]
    async fn map_eq_entries(
        &mut self,
        l: &[(ValueId, ValueId)],
        r: &[(ValueId, ValueId)],
        span: Span,
    ) -> Result<bool> {
        match (l.split_first(), r.split_first()) {
            (Some(((lk, lv), l_rest)), Some(((rk, rv), r_rest))) => {
                let keys_eq = self.cmp_value_ids(*lk, *rk, span).await?
                    == Ordering::Equal;
                let vals_eq = self.eq_value_ids(*lv, *rv, span).await?;
                if keys_eq && vals_eq {
                    self.map_eq_entries(l_rest, r_rest, span).await
                } else {
                    Ok(false)
                }
            }
            (None, None) => Ok(true),
            _ => Ok(false),
        }
    }

    #[async_recursion]
    async fn map_cmp_entries(
        &mut self,
        l: &[(ValueId, ValueId)],
        r: &[(ValueId, ValueId)],
        span: Span,
    ) -> Result<Ordering> {
        match (l.split_first(), r.split_first()) {
            (Some(((lk, lv), l_rest)), Some(((rk, rv), r_rest))) => {
                match self.cmp_value_ids(*lk, *rk, span).await? {
                    Ordering::Equal => {
                        match self.cmp_value_ids(*lv, *rv, span).await? {
                            Ordering::Equal => {
                                self.map_cmp_entries(l_rest, r_rest, span).await
                            }
                            ord => Ok(ord),
                        }
                    }
                    ord => Ok(ord),
                }
            }
            (Some(_), None) => Ok(Ordering::Greater),
            (None, Some(_)) => Ok(Ordering::Less),
            (None, None) => Ok(Ordering::Equal),
        }
    }

    #[async_recursion]
    async fn map_insert_entries(
        &mut self,
        acc: Map,
        entries: &[(ValueId, ValueId)],
        span: Span,
    ) -> Result<Map> {
        match entries.split_first() {
            Some(((key, val), rest)) => {
                let acc = self.map_insert_id(&acc, *key, *val, span).await?;
                self.map_insert_entries(acc, rest, span).await
            }
            None => Ok(acc),
        }
    }

    #[async_recursion]
    async fn map_lookup_node(
        &mut self,
        node: Option<Arc<MapNode>>,
        key: ValueId,
        span: Span,
    ) -> Result<Option<ValueId>> {
        match node {
            Some(n) => match self.cmp_value_ids(key, n.key(), span).await? {
                Ordering::Less => {
                    self.map_lookup_node(n.left(), key, span).await
                }
                Ordering::Equal => Ok(Some(n.val())),
                Ordering::Greater => {
                    self.map_lookup_node(n.right(), key, span).await
                }
            },
            None => Ok(None),
        }
    }

    #[async_recursion]
    async fn map_insert_node(
        &mut self,
        node: Option<Arc<MapNode>>,
        key: ValueId,
        val: ValueId,
        span: Span,
    ) -> Result<(Option<Arc<MapNode>>, bool)> {
        match node {
            Some(n) => match self.cmp_value_ids(key, n.key(), span).await? {
                Ordering::Less => {
                    let (left, added) =
                        self.map_insert_node(n.left(), key, val, span).await?;
                    Ok((
                        Some(MapNode::balance(
                            n.key(),
                            n.val(),
                            left,
                            n.right(),
                        )),
                        added,
                    ))
                }
                Ordering::Equal => Ok((
                    Some(MapNode::balance(n.key(), val, n.left(), n.right())),
                    false,
                )),
                Ordering::Greater => {
                    let (right, added) =
                        self.map_insert_node(n.right(), key, val, span).await?;
                    Ok((
                        Some(MapNode::balance(
                            n.key(),
                            n.val(),
                            n.left(),
                            right,
                        )),
                        added,
                    ))
                }
            },
            None => Ok((Some(MapNode::new(key, val, None, None)), true)),
        }
    }

    #[async_recursion]
    async fn map_remove_node(
        &mut self,
        node: Option<Arc<MapNode>>,
        key: ValueId,
        span: Span,
    ) -> Result<(Option<Arc<MapNode>>, bool)> {
        match node {
            Some(n) => match self.cmp_value_ids(key, n.key(), span).await? {
                Ordering::Less => {
                    let (left, removed) =
                        self.map_remove_node(n.left(), key, span).await?;
                    Ok((
                        Some(MapNode::balance(
                            n.key(),
                            n.val(),
                            left,
                            n.right(),
                        )),
                        removed,
                    ))
                }
                Ordering::Equal => Ok((Self::map_remove_root(n), true)),
                Ordering::Greater => {
                    let (right, removed) =
                        self.map_remove_node(n.right(), key, span).await?;
                    Ok((
                        Some(MapNode::balance(
                            n.key(),
                            n.val(),
                            n.left(),
                            right,
                        )),
                        removed,
                    ))
                }
            },
            None => Ok((None, false)),
        }
    }

    fn map_remove_root(n: Arc<MapNode>) -> Option<Arc<MapNode>> {
        match (n.left(), n.right()) {
            (None, None) => None,
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (Some(left), Some(right)) => {
                let (right, key, val) = Self::map_remove_min(right);
                Some(MapNode::balance(key, val, Some(left), right))
            }
        }
    }

    fn map_remove_min(
        n: Arc<MapNode>,
    ) -> (Option<Arc<MapNode>>, ValueId, ValueId) {
        match n.left() {
            Some(left) => {
                let (left, key, val) = Self::map_remove_min(left);
                (
                    Some(MapNode::balance(n.key(), n.val(), left, n.right())),
                    key,
                    val,
                )
            }
            None => (n.right(), n.key(), n.val()),
        }
    }
}
