//! Union-find (disjoint set) for type variable binding.
//!
//! Replaces the naive substitution (`HashMap<TyVar, TyId>`) with a near-linear
//! amortized data structure using path compression and union-by-rank.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::ty::{Ty, TyArena, TyId, TyVar};
use crate::intern::StringId;

/// Internal state for a single union-find slot.
#[derive(Clone, Copy, Debug)]
enum Entry {
    /// Unbound root with rank (for union-by-rank).
    Root(u8),
    /// Links to another variable (union-find parent pointer).
    Link(TyVar),
    /// Resolved to a concrete type.
    Bound(TyId),
}

/// Union-find for type variable constraint solving.
///
/// Each type variable is a slot. `find` with path compression gives the
/// canonical root; `probe` checks if the root is bound to a concrete type.
///
/// Supports snapshot/rollback for backtracking (e.g. union bijection
/// matching). When `snap_depth > 0`, mutations are automatically recorded
/// to the undo log.
pub(crate) struct UnionFind {
    entries: Vec<Entry>,
    /// Undo log for snapshot/rollback. Only populated when `snap_depth > 0`.
    undo: Vec<(usize, Entry)>,
    /// Number of active snapshots. When `> 0`, mutations are recorded.
    snap_depth: u32,
    /// Memoization cache for `resolve`; only valid when bindings are frozen
    /// (post-solve). Cleared by `enable_zonk_cache` / `disable_zonk_cache`.
    zonk_cache: Option<HashMap<TyId, TyId>>,
}

/// Opaque snapshot handle for backtracking.
pub(crate) struct Snapshot {
    entries_len: usize,
    undo_len: usize,
}

impl UnionFind {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            undo: Vec::new(),
            snap_depth: 0,
            zonk_cache: None,
        }
    }

    /// Set `entries[idx]` to `val`, recording the old value if snapshots
    /// are active.
    fn set(&mut self, idx: usize, val: Entry) {
        if self.snap_depth > 0 {
            if let Some(&old) = self.entries.get(idx) {
                self.undo.push((idx, old));
            }
        }
        if let Some(slot) = self.entries.get_mut(idx) {
            *slot = val;
        }
    }

    /// Allocate a fresh type variable, returning its `TyVar`.
    pub(crate) fn fresh(&mut self) -> TyVar {
        let idx = self.entries.len() as u32;
        self.entries.push(Entry::Root(0));
        TyVar::new(idx)
    }

    /// Ensure the next `fresh()` call returns an index strictly greater than
    /// `max_idx`. Pads with dummy `Root(0)` entries if needed.
    ///
    /// Used by `Scheme::instantiate` to avoid overlap between scheme vars
    /// and freshly allocated vars (which would cause infinite loops in
    /// `TyArena::apply`).
    pub(crate) fn reserve_through(&mut self, max_idx: u32) {
        let needed = (max_idx + 1) as usize;
        if self.entries.len() < needed {
            self.entries.resize(needed, Entry::Root(0));
        }
    }

    /// Find the canonical root for `v`, with path compression.
    pub(crate) fn find(&mut self, v: TyVar) -> TyVar {
        let idx = v.idx() as usize;
        match self.entries.get(idx) {
            Some(Entry::Link(parent)) => {
                let parent = *parent;
                let root = self.find(parent);
                if root != parent {
                    self.set(idx, Entry::Link(root));
                }
                root
            }
            _ => v,
        }
    }

    /// Find the canonical root and check if it is bound to a type.
    pub(crate) fn probe(&mut self, v: TyVar) -> Option<TyId> {
        let root = self.find(v);
        match self.entries.get(root.idx() as usize) {
            Some(Entry::Bound(ty)) => Some(*ty),
            _ => None,
        }
    }

    /// Resolve a type variable to its bound type (if any), fully resolving
    /// through chains. If unbound, returns `None`.
    pub(crate) fn resolve_var(
        &mut self,
        v: TyVar,
        arena: &mut TyArena,
    ) -> Option<TyId> {
        let root = self.find(v);
        self.probe(root).map(|bound| self.resolve(bound, arena))
    }

    /// Merge two unbound roots by rank.
    ///
    /// Both `a` and `b` must already be canonical roots (call `find` first).
    pub(crate) fn union(&mut self, a: TyVar, b: TyVar) {
        if a != b {
            let ra = match self.entries.get(a.idx() as usize) {
                Some(Entry::Root(r)) => *r,
                _ => 0,
            };
            let rb = match self.entries.get(b.idx() as usize) {
                Some(Entry::Root(r)) => *r,
                _ => 0,
            };
            if ra >= rb {
                self.set(b.idx() as usize, Entry::Link(a));
                if ra == rb {
                    self.set(a.idx() as usize, Entry::Root(ra + 1));
                }
            } else {
                self.set(a.idx() as usize, Entry::Link(b));
            }
        }
    }

    /// Bind a canonical root to a concrete type.
    ///
    /// Caller must `find` first to get the root.
    pub(crate) fn bind(&mut self, v: TyVar, ty: TyId) {
        self.set(v.idx() as usize, Entry::Bound(ty));
    }

    /// Recursively resolve all `Ty::Var`s in `ty` through the union-find,
    /// re-interning the result in `arena`.
    ///
    /// This is the replacement for `TyArena::apply(ty, &subst)`.
    pub(crate) fn resolve(&mut self, id: TyId, arena: &mut TyArena) -> TyId {
        if let Some(ref cache) = self.zonk_cache {
            if let Some(&cached) = cache.get(&id) {
                cached
            } else {
                let ty = arena.get(id).clone();
                let resolved = self.resolve_inner(id, ty, arena);
                if let Some(ref mut cache) = self.zonk_cache {
                    cache.insert(id, resolved);
                }
                resolved
            }
        } else {
            let ty = arena.get(id).clone();
            self.resolve_inner(id, ty, arena)
        }
    }

    fn resolve_inner(&mut self, id: TyId, ty: Ty, arena: &mut TyArena) -> TyId {
        match ty {
            Ty::Var(v) => {
                let root = self.find(v);
                match self.probe(root) {
                    Some(bound) => self.resolve(bound, arena),
                    None => {
                        // Re-intern with canonical root
                        if root == v {
                            id
                        } else {
                            arena.alloc(Ty::Var(root))
                        }
                    }
                }
            }
            // Primitives: no change
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Time
            | Ty::Range
            | Ty::Json
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::Regex
            | Ty::RuntimeError
            | Ty::Local
            | Ty::Global
            | Ty::Unknown
            | Ty::Error => id,
            Ty::Array(inner) => {
                let n = self.resolve(inner, arena);
                if n == inner {
                    id
                } else {
                    arena.alloc(Ty::Array(n))
                }
            }
            Ty::Option(inner) => {
                let n = self.resolve(inner, arena);
                if n == inner {
                    id
                } else {
                    arena.alloc(Ty::Option(n))
                }
            }
            Ty::Result(ok, err) => {
                let nok = self.resolve(ok, arena);
                let nerr = self.resolve(err, arena);
                if nok == ok && nerr == err {
                    id
                } else {
                    arena.alloc(Ty::Result(nok, nerr))
                }
            }
            Ty::Map(k, v) => {
                let nk = self.resolve(k, arena);
                let nv = self.resolve(v, arena);
                if nk == k && nv == v {
                    id
                } else {
                    arena.alloc(Ty::Map(nk, nv))
                }
            }
            Ty::Tuple(ref ts) => {
                let nts: SmallVec<[TyId; 4]> =
                    ts.iter().map(|&t| self.resolve(t, arena)).collect();
                if nts == *ts {
                    id
                } else {
                    arena.alloc(Ty::Tuple(nts))
                }
            }
            Ty::Fn(ref params, ret) => {
                let np: SmallVec<[TyId; 4]> =
                    params.iter().map(|&t| self.resolve(t, arena)).collect();
                let nr = self.resolve(ret, arena);
                if np == *params && nr == ret {
                    id
                } else {
                    arena.alloc(Ty::Fn(np, nr))
                }
            }
            Ty::Object(ref fields) => {
                let nf: IndexMap<StringId, TyId> = fields
                    .iter()
                    .map(|(&k, &t)| (k, self.resolve(t, arena)))
                    .collect();
                if nf == *fields {
                    id
                } else {
                    arena.alloc(Ty::Object(nf))
                }
            }
            Ty::Union(prov, ref members) => {
                let nm: SmallVec<[TyId; 4]> =
                    members.iter().map(|&t| self.resolve(t, arena)).collect();
                if nm == *members {
                    id
                } else {
                    arena.alloc(Ty::Union(prov, nm))
                }
            }
            Ty::Named(type_id, ref args) => {
                let na: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.resolve(t, arena)).collect();
                if na == *args {
                    id
                } else {
                    arena.alloc(Ty::Named(type_id, na))
                }
            }
            Ty::Apply(v, ref args) => {
                let na: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.resolve(t, arena)).collect();
                let root = self.find(v);
                match self.probe(root) {
                    None => {
                        if na == *args && root == v {
                            id
                        } else {
                            arena.alloc(Ty::Apply(root, na))
                        }
                    }
                    Some(ctor_id) => {
                        let ctor_resolved = self.resolve(ctor_id, arena);
                        let ctor = arena.get(ctor_resolved).clone();
                        match ctor {
                            Ty::Var(w) => arena.alloc(Ty::Apply(w, na)),
                            Ty::Option(_) => {
                                na.first().map_or(TyArena::ERROR, |&a| {
                                    arena.alloc(Ty::Option(a))
                                })
                            }
                            Ty::Result(_, e) => {
                                na.first().map_or(TyArena::ERROR, |&a| {
                                    arena.alloc(Ty::Result(a, e))
                                })
                            }
                            Ty::Array(_) => {
                                na.first().map_or(TyArena::ERROR, |&a| {
                                    arena.alloc(Ty::Array(a))
                                })
                            }
                            Ty::Map(_, mv) => {
                                na.first().map_or(TyArena::ERROR, |&a| {
                                    arena.alloc(Ty::Map(a, mv))
                                })
                            }
                            Ty::Union(prov, members) => {
                                let filled: SmallVec<[TyId; 4]> = members
                                    .iter()
                                    .map(|&m| match arena.get(m).clone() {
                                        Ty::Option(_) => na
                                            .first()
                                            .map_or(TyArena::ERROR, |&a| {
                                                arena.alloc(Ty::Option(a))
                                            }),
                                        Ty::Result(_, e) => {
                                            match (na.first(), na.get(1)) {
                                                (Some(&a), Some(&b)) => arena
                                                    .alloc(Ty::Result(a, b)),
                                                (Some(&a), None) => arena
                                                    .alloc(Ty::Result(a, e)),
                                                _ => TyArena::ERROR,
                                            }
                                        }
                                        Ty::Array(_) => na
                                            .first()
                                            .map_or(TyArena::ERROR, |&a| {
                                                arena.alloc(Ty::Array(a))
                                            }),
                                        Ty::Map(_, mv) => {
                                            match (na.first(), na.get(1)) {
                                                (Some(&a), Some(&b)) => {
                                                    arena.alloc(Ty::Map(a, b))
                                                }
                                                (Some(&a), None) => {
                                                    arena.alloc(Ty::Map(a, mv))
                                                }
                                                _ => TyArena::ERROR,
                                            }
                                        }
                                        Ty::Range => TyArena::RANGE,
                                        Ty::Named(tid, orig) => {
                                            let keep = orig
                                                .len()
                                                .saturating_sub(na.len());
                                            let new_args: SmallVec<[TyId; 4]> =
                                                orig.iter()
                                                    .take(keep)
                                                    .chain(na.iter())
                                                    .copied()
                                                    .collect();
                                            arena
                                                .alloc(Ty::Named(tid, new_args))
                                        }
                                        Ty::Tuple(ts) => {
                                            let mut na_iter =
                                                na.iter().copied();
                                            let filled: SmallVec<[TyId; 4]> =
                                                ts.iter()
                                                    .map(|&t| {
                                                        if t == TyArena::ERROR {
                                                            na_iter
                                                                .next()
                                                                .unwrap_or(
                                                                TyArena::ERROR,
                                                            )
                                                        } else {
                                                            t
                                                        }
                                                    })
                                                    .collect();
                                            arena.alloc(Ty::Tuple(filled))
                                        }
                                        _ => TyArena::ERROR,
                                    })
                                    .collect();
                                if filled.contains(&TyArena::ERROR) {
                                    TyArena::ERROR
                                } else {
                                    arena.alloc(Ty::Union(prov, filled))
                                }
                            }
                            Ty::Named(tid, orig) => {
                                let new_args: SmallVec<[TyId; 4]> = na
                                    .iter()
                                    .chain(orig.iter().skip(na.len()))
                                    .copied()
                                    .collect();
                                arena.alloc(Ty::Named(tid, new_args))
                            }
                            _ if na.is_empty() => ctor_resolved,
                            _ => TyArena::ERROR,
                        }
                    }
                }
            }
            Ty::AssocType(v, class, name) => {
                let root = self.find(v);
                match self.probe(root) {
                    Some(bound) => {
                        let resolved = self.resolve(bound, arena);
                        let inner = arena.get(resolved).clone();
                        match inner {
                            Ty::Var(w) => {
                                arena.alloc(Ty::AssocType(w, class, name))
                            }
                            _ => id,
                        }
                    }
                    None => {
                        if root == v {
                            id
                        } else {
                            arena.alloc(Ty::AssocType(root, class, name))
                        }
                    }
                }
            }
        }
    }

    /// Collect all free type variables in `id`, chasing through UF bindings.
    ///
    /// Unlike `TyArena::free_vars`, this resolves variables through the
    /// union-find: bound variables are replaced by their binding's free vars.
    pub(crate) fn free_vars(
        &mut self,
        id: TyId,
        arena: &TyArena,
    ) -> HashSet<TyVar> {
        let mut acc = HashSet::new();
        self.collect_free_vars(id, arena, &mut acc);
        acc
    }

    /// Collect free vars for a type variable, chasing UF bindings.
    pub(crate) fn free_vars_for_var(
        &mut self,
        v: TyVar,
        arena: &TyArena,
    ) -> HashSet<TyVar> {
        let mut acc = HashSet::new();
        self.collect_free_var(v, arena, &mut acc);
        acc
    }

    fn collect_free_var(
        &mut self,
        v: TyVar,
        arena: &TyArena,
        acc: &mut HashSet<TyVar>,
    ) {
        let root = self.find(v);
        match self.probe(root) {
            Some(bound) => self.collect_free_vars(bound, arena, acc),
            None => {
                acc.insert(root);
            }
        }
    }

    fn collect_free_vars(
        &mut self,
        id: TyId,
        arena: &TyArena,
        acc: &mut HashSet<TyVar>,
    ) {
        let ty = arena.get(id).clone();
        match ty {
            Ty::Var(v) => {
                self.collect_free_var(v, arena, acc);
            }
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Time
            | Ty::Range
            | Ty::Json
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::Regex
            | Ty::RuntimeError
            | Ty::Local
            | Ty::Global
            | Ty::Unknown
            | Ty::Error => {}
            Ty::Array(t) | Ty::Option(t) => {
                self.collect_free_vars(t, arena, acc);
            }
            Ty::Result(ok, err) => {
                self.collect_free_vars(ok, arena, acc);
                self.collect_free_vars(err, arena, acc);
            }
            Ty::Map(k, v) => {
                self.collect_free_vars(k, arena, acc);
                self.collect_free_vars(v, arena, acc);
            }
            Ty::Tuple(ts) => {
                ts.iter()
                    .for_each(|&t| self.collect_free_vars(t, arena, acc));
            }
            Ty::Fn(params, ret) => {
                params
                    .iter()
                    .for_each(|&t| self.collect_free_vars(t, arena, acc));
                self.collect_free_vars(ret, arena, acc);
            }
            Ty::Object(fields) => {
                fields
                    .values()
                    .for_each(|&t| self.collect_free_vars(t, arena, acc));
            }
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|&t| self.collect_free_vars(t, arena, acc));
            }
            Ty::Named(_, args) => {
                args.iter()
                    .for_each(|&t| self.collect_free_vars(t, arena, acc));
            }
            Ty::Apply(v, args) => {
                let root = self.find(v);
                match self.probe(root) {
                    Some(bound) => self.collect_free_vars(bound, arena, acc),
                    None => {
                        acc.insert(root);
                    }
                }
                args.iter()
                    .for_each(|&t| self.collect_free_vars(t, arena, acc));
            }
            Ty::AssocType(v, _, _) => {
                let root = self.find(v);
                match self.probe(root) {
                    Some(bound) => self.collect_free_vars(bound, arena, acc),
                    None => {
                        acc.insert(root);
                    }
                }
            }
        }
    }

    /// Enable the zonking cache. Call after constraint solving is complete
    /// and no more bindings will be added.
    pub(crate) fn enable_zonk_cache(&mut self) {
        self.zonk_cache = Some(HashMap::new());
    }

    /// Disable and clear the zonking cache.
    pub(crate) fn disable_zonk_cache(&mut self) {
        self.zonk_cache = None;
    }

    /// Take a snapshot for backtracking. All subsequent mutations (via
    /// `find`, `union`, `bind`) are automatically recorded until
    /// `rollback` is called.
    pub(crate) fn snapshot(&mut self) -> Snapshot {
        self.zonk_cache = None;
        self.snap_depth += 1;
        Snapshot {
            entries_len: self.entries.len(),
            undo_len: self.undo.len(),
        }
    }

    /// Rollback to a previous snapshot, undoing all mutations since it.
    pub(crate) fn rollback(&mut self, snap: Snapshot) {
        self.zonk_cache = None;
        // Restore mutated entries in reverse order
        self.undo
            .drain(snap.undo_len..)
            .rev()
            .for_each(|(idx, entry)| {
                if let Some(slot) = self.entries.get_mut(idx) {
                    *slot = entry;
                }
            });
        // Truncate any entries added since the snapshot
        self.entries.truncate(snap.entries_len);
        self.snap_depth -= 1;
    }

    /// Check if the union-find has active snapshots (for testing).
    #[cfg(test)]
    fn has_snapshots(&self) -> bool {
        self.snap_depth > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_allocates_sequential() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        let c = uf.fresh();
        assert_eq!(a.idx(), 0);
        assert_eq!(b.idx(), 1);
        assert_eq!(c.idx(), 2);
    }

    #[test]
    fn find_root_is_self() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        assert_eq!(uf.find(a), a);
    }

    #[test]
    fn probe_unbound_is_none() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        assert_eq!(uf.probe(a), None);
    }

    #[test]
    fn bind_and_probe() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        uf.bind(a, TyArena::INT);
        assert_eq!(uf.probe(a), Some(TyArena::INT));
    }

    #[test]
    fn union_merges_roots() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        uf.union(a, b);
        assert_eq!(uf.find(a), uf.find(b));
    }

    #[test]
    fn union_preserves_binding() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        uf.union(a, b);
        let root = uf.find(a);
        uf.bind(root, TyArena::STRING);
        assert_eq!(uf.probe(a), Some(TyArena::STRING));
        assert_eq!(uf.probe(b), Some(TyArena::STRING));
    }

    #[test]
    fn find_with_path_compression() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        let c = uf.fresh();
        // Chain: c -> b -> a
        uf.union(a, b);
        let root_a = uf.find(a);
        uf.union(root_a, c);
        let root = uf.find(c);
        // After find, `c` should point directly to root
        assert_eq!(uf.find(c), root);
        assert_eq!(uf.find(b), root);
        assert_eq!(uf.find(a), root);
    }

    #[test]
    fn resolve_var_bound() {
        let mut arena = TyArena::new();
        let mut uf = UnionFind::new();
        let v = uf.fresh();
        let vid = arena.alloc(Ty::Var(v));
        uf.bind(v, TyArena::INT);
        let resolved = uf.resolve(vid, &mut arena);
        assert_eq!(resolved, TyArena::INT);
    }

    #[test]
    fn resolve_var_unbound_canonical() {
        let mut arena = TyArena::new();
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        uf.union(a, b);
        let root = uf.find(a);
        let vid = arena.alloc(Ty::Var(b));
        let resolved = uf.resolve(vid, &mut arena);
        // Should be `Var(root)`
        match arena.get(resolved) {
            Ty::Var(v) => assert_eq!(*v, root),
            other => panic!("expected Var, got {other:?}"),
        }
    }

    #[test]
    fn resolve_array() {
        let mut arena = TyArena::new();
        let mut uf = UnionFind::new();
        let v = uf.fresh();
        let vid = arena.alloc(Ty::Var(v));
        let arr = arena.alloc(Ty::Array(vid));
        uf.bind(v, TyArena::STRING);
        let resolved = uf.resolve(arr, &mut arena);
        match arena.get(resolved) {
            Ty::Array(inner) => assert_eq!(*inner, TyArena::STRING),
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn resolve_no_change() {
        let mut arena = TyArena::new();
        let mut uf = UnionFind::new();
        let resolved = uf.resolve(TyArena::INT, &mut arena);
        assert_eq!(resolved, TyArena::INT);
    }

    #[test]
    fn snapshot_and_rollback() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        let snap = uf.snapshot();
        // Make some changes (auto-recorded by snapshot)
        let c = uf.fresh();
        uf.union(a, b);
        let root = uf.find(a);
        uf.bind(root, TyArena::INT);
        // Rollback
        uf.rollback(snap);
        // `c` should no longer exist (truncated)
        assert_eq!(uf.entries.len(), 2);
        // `a` and `b` should be unbound roots
        assert_eq!(uf.probe(a), None);
        assert_eq!(uf.probe(b), None);
        assert_ne!(uf.find(a), uf.find(b));
        assert!(!uf.has_snapshots());
        let _ = c;
    }

    #[test]
    fn snapshot_bind_and_rollback() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let snap = uf.snapshot();
        uf.bind(a, TyArena::INT);
        assert_eq!(uf.probe(a), Some(TyArena::INT));
        uf.rollback(snap);
        assert_eq!(uf.probe(a), None);
    }

    #[test]
    fn nested_snapshots() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        let snap1 = uf.snapshot();
        uf.bind(a, TyArena::INT);
        let snap2 = uf.snapshot();
        uf.bind(b, TyArena::STRING);
        assert_eq!(uf.probe(b), Some(TyArena::STRING));
        // Roll back inner snapshot; `a` still bound, `b` unbound
        uf.rollback(snap2);
        assert_eq!(uf.probe(a), Some(TyArena::INT));
        assert_eq!(uf.probe(b), None);
        // Roll back outer snapshot; both unbound
        uf.rollback(snap1);
        assert_eq!(uf.probe(a), None);
        assert_eq!(uf.probe(b), None);
    }

    #[test]
    fn resolve_var_method_bound() {
        let mut arena = TyArena::new();
        let mut uf = UnionFind::new();
        let v = uf.fresh();
        uf.bind(v, TyArena::INT);
        assert_eq!(uf.resolve_var(v, &mut arena), Some(TyArena::INT));
    }

    #[test]
    fn resolve_var_method_unbound() {
        let mut arena = TyArena::new();
        let mut uf = UnionFind::new();
        let v = uf.fresh();
        assert_eq!(uf.resolve_var(v, &mut arena), None);
    }

    #[test]
    fn resolve_var_method_through_chain() {
        let mut arena = TyArena::new();
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        let b = uf.fresh();
        uf.union(a, b);
        let root = uf.find(a);
        uf.bind(root, TyArena::STRING);
        // Resolving the non-root var should chase through to the binding
        let non_root = if root == a { b } else { a };
        assert_eq!(uf.resolve_var(non_root, &mut arena), Some(TyArena::STRING));
    }

    #[test]
    fn no_undo_recording_without_snapshot() {
        let mut uf = UnionFind::new();
        let a = uf.fresh();
        uf.bind(a, TyArena::INT);
        // Undo log should be empty when no snapshots are active
        assert!(uf.undo.is_empty());
    }
}
