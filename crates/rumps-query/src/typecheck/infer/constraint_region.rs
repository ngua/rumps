use std::collections::HashMap;
use std::ops::Range;

use smallvec::SmallVec;

use super::Constraint;
use crate::intern::QualifiedName;
use crate::typecheck::ty::{Rename, Ty, TyArena, TyId, TyVar, TypeClass};
use crate::typecheck::uf::UnionFind;
use crate::{ClassId, Span};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ConstraintKey {
    Class(TyId, ClassKey),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ClassKey {
    Concrete {
        id: ClassId,
        params: SmallVec<[TyId; 1]>,
    },
    Hkt {
        id: ClassId,
        elems: SmallVec<[TyId; 1]>,
        params: SmallVec<[TyId; 1]>,
    },
}

pub(super) struct ConstraintRegion;

impl ConstraintRegion {
    pub(super) fn keys(
        cs: &[(TyId, TypeClass<TyId>)],
        uf: &mut UnionFind,
        tys: &mut TyArena,
    ) -> Vec<ConstraintKey> {
        let mut keys: Vec<_> = cs
            .iter()
            .map(|(ty, cls)| {
                let ty = uf.resolve(*ty, tys);
                ConstraintKey::Class(ty, Self::class_key(cls, uf, tys))
            })
            .collect();
        keys.sort_unstable();
        keys
    }

    pub(super) fn build_unions(
        cs: &[(Constraint, Option<QualifiedName>)],
        rg: Range<usize>,
        uf: &mut UnionFind,
        tys: &TyArena,
    ) {
        cs.get(rg)
            .unwrap_or_else(|| invariant!("constraint region range in bounds"))
            .iter()
            .for_each(|(c, _)| match c {
                Constraint::Unify(a, b, _) => {
                    Self::union_vars(*a, *b, uf, tys);
                    if let (Ty::Fn(pa, ra), Ty::Fn(pb, rb)) =
                        (tys.get(*a).clone(), tys.get(*b).clone())
                    {
                        pa.iter().zip(pb.iter()).for_each(|(&x, &y)| {
                            Self::union_vars(x, y, uf, tys);
                        });
                        Self::union_vars(ra, rb, uf, tys);
                    }
                }
                Constraint::Callable {
                    callee, args, ret, ..
                } => match tys.get(*callee).clone() {
                    Ty::Fn(params, fn_ret) => {
                        params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                            Self::union_vars(p, a, uf, tys);
                        });
                        Self::union_vars(fn_ret, *ret, uf, tys);
                    }
                    Ty::Var(vc) => {
                        let rc = uf.find(vc);
                        if uf.probe(rc).is_none() {
                            if let Ty::Var(vr) = tys.get(*ret) {
                                let rr = uf.find(*vr);
                                uf.union(rc, rr);
                            }
                            args.iter().for_each(|&a| {
                                if let Ty::Var(va) = tys.get(a) {
                                    let ra = uf.find(*va);
                                    uf.union(rc, ra);
                                }
                            });
                        }
                    }
                    _ => {}
                },
                _ => {}
            });
    }

    pub(super) fn root_vars(
        vars: &[TyVar],
        uf: &mut UnionFind,
    ) -> HashMap<TyVar, TyVar> {
        let mut roots = HashMap::with_capacity(vars.len());
        vars.iter().for_each(|&v| {
            roots.entry(uf.find(v)).or_insert(v);
        });
        roots
    }

    pub(super) fn root_tys(
        map: &HashMap<TyVar, TyId>,
        uf: &mut UnionFind,
    ) -> HashMap<TyVar, TyId> {
        map.iter().map(|(&v, &ty)| (uf.find(v), ty)).collect()
    }

    pub(super) fn reachable_constraint_pairs(
        cs: &[(Constraint, Option<QualifiedName>)],
        rg: Range<usize>,
        map: &HashMap<TyVar, TyId>,
        rename: &Rename,
        uf: &mut UnionFind,
        tys: &mut TyArena,
    ) -> SmallVec<[(TyId, TypeClass<TyId>); 2]> {
        let roots = Self::root_tys(map, uf);
        Self::reachable_classes(cs, rg, &roots, uf, tys)
            .into_iter()
            .map(|(ty, cls, _)| {
                let cls = cls.apply(rename, tys).resolve_inner(uf, tys);
                (ty, cls)
            })
            .collect()
    }

    pub(super) fn reachable_classes<K>(
        cs: &[(Constraint, Option<QualifiedName>)],
        rg: Range<usize>,
        roots: &HashMap<TyVar, K>,
        uf: &mut UnionFind,
        tys: &TyArena,
    ) -> SmallVec<[(K, TypeClass<TyId>, Span); 2]>
    where
        K: Copy,
    {
        cs.get(rg)
            .unwrap_or_else(|| invariant!("constraint region range in bounds"))
            .iter()
            .filter_map(|(c, _)| match c {
                Constraint::Class { ty, class, span } => match tys.get(*ty) {
                    Ty::Var(tv) => roots
                        .get(&uf.find(*tv))
                        .map(|&orig| (orig, class.clone(), *span)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn class_key(
        cls: &TypeClass<TyId>,
        uf: &mut UnionFind,
        tys: &mut TyArena,
    ) -> ClassKey {
        match cls {
            TypeClass::Concrete { id, params } => ClassKey::Concrete {
                id: *id,
                params: Self::type_keys(params, uf, tys),
            },
            TypeClass::Hkt { id, elems, params } => ClassKey::Hkt {
                id: *id,
                elems: Self::type_keys(elems, uf, tys),
                params: Self::type_keys(params, uf, tys),
            },
        }
    }

    fn type_keys(
        tys_in: &[TyId],
        uf: &mut UnionFind,
        tys: &mut TyArena,
    ) -> SmallVec<[TyId; 1]> {
        tys_in.iter().map(|&ty| uf.resolve(ty, tys)).collect()
    }

    fn union_vars(a: TyId, b: TyId, uf: &mut UnionFind, tys: &TyArena) {
        if let (Ty::Var(va), Ty::Var(vb)) = (tys.get(a), tys.get(b)) {
            let ra = uf.find(*va);
            let rb = uf.find(*vb);
            uf.union(ra, rb);
        }
    }
}
