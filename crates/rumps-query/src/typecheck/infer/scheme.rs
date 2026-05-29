//! Qualified scheme construction.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use smallvec::SmallVec;

use super::constraint_region::ConstraintRegion;
use super::{Constraint, InferCtx};
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, Ty, TyId, TyVar, TypeClass};
use crate::Span;

#[derive(Clone, Copy)]
pub(super) enum SchemePolicy<'a> {
    InferredLet,
    InferredFun,
    ExplicitCallable {
        vars: &'a HashSet<TyVar>,
        names: &'a HashMap<TyVar, StringId>,
    },
}

pub(super) struct SchemeOut {
    pub(super) scheme: Scheme,
    /// Constraints from the requested region that stay in the global solver.
    pub(super) residual: Vec<(Constraint, Option<QualifiedName>)>,
}

enum RootPolicy {
    InferredLet,
    InferredFun,
    ExplicitCallable {
        vars: HashSet<TyVar>,
        names: HashMap<TyVar, StringId>,
    },
}

impl InferCtx<'_> {
    pub(super) fn qualified_scheme(
        &mut self,
        ty: TyId,
        rg: Range<usize>,
        policy: SchemePolicy<'_>,
        base_cs: SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
    ) -> SchemeOut {
        let env_fv = self.env.free_vars(&self.ty_arena, &mut self.uf);
        self.qualified_scheme_in_env(ty, rg, policy, base_cs, &env_fv)
    }

    pub(super) fn qualified_scheme_in_env(
        &mut self,
        ty: TyId,
        rg: Range<usize>,
        policy: SchemePolicy<'_>,
        base_cs: SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
        env_fv: &HashSet<TyVar>,
    ) -> SchemeOut {
        let reg: Vec<_> = self
            .constraints
            .get(rg.clone())
            .unwrap_or_else(|| invariant!("constraint region range in bounds"))
            .to_vec();
        let snap = self.uf.snapshot();
        self.apply_scheme_shapes(&reg);
        ConstraintRegion::build_unions(
            &self.constraints,
            rg,
            &mut self.uf,
            &self.ty_arena,
        );

        let policy = self.root_policy(policy);
        let ty = self.uf.resolve(ty, &mut self.ty_arena);
        let mut vars = self.seed_vars(ty, &policy, env_fv);
        let mut cs = self.norm_base_cs(base_cs);
        self.extend_cs_vars(&cs, env_fv, &mut vars);
        let mut cap = CaptureState {
            vars: &mut vars,
            cs: &mut cs,
            ix: HashSet::new(),
            rejected: SmallVec::new(),
        };
        self.capture_region_classes(&reg, &policy, env_fv, &mut cap);
        let cap_ix = cap.ix;

        let mut vars: SmallVec<[TyVar; 4]> = vars.into_iter().collect();
        vars.sort_unstable();
        let residual = reg
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !cap_ix.contains(i))
            .map(|(_, c)| c)
            .collect();
        self.uf.rollback(snap);

        SchemeOut {
            scheme: Scheme {
                vars,
                ty,
                constraints: cs,
            },
            residual,
        }
    }

    fn root_policy(&mut self, policy: SchemePolicy<'_>) -> RootPolicy {
        match policy {
            SchemePolicy::InferredLet => RootPolicy::InferredLet,
            SchemePolicy::InferredFun => RootPolicy::InferredFun,
            SchemePolicy::ExplicitCallable { vars, names } => {
                let vars = vars.iter().map(|&v| self.uf.find(v)).collect();
                let mut root_names = HashMap::with_capacity(names.len());
                names.iter().for_each(|(&v, &name)| {
                    root_names.entry(self.uf.find(v)).or_insert(name);
                });
                RootPolicy::ExplicitCallable {
                    vars,
                    names: root_names,
                }
            }
        }
    }

    fn seed_vars(
        &mut self,
        ty: TyId,
        policy: &RootPolicy,
        env_fv: &HashSet<TyVar>,
    ) -> HashSet<TyVar> {
        let mut vars = self.uf.free_vars(ty, &self.ty_arena);
        if let RootPolicy::ExplicitCallable { vars: explicit, .. } = policy {
            vars.extend(explicit.iter().copied());
        }
        vars.into_iter()
            .map(|v| self.uf.find(v))
            .filter(|v| !env_fv.contains(v))
            .collect()
    }

    fn norm_base_cs(
        &mut self,
        cs: SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
    ) -> SmallVec<[(TyVar, TypeClass<TyId>); 2]> {
        cs.into_iter()
            .map(|(v, class)| {
                (
                    self.uf.find(v),
                    class.resolve_inner(&mut self.uf, &mut self.ty_arena),
                )
            })
            .collect()
    }

    fn extend_cs_vars(
        &mut self,
        cs: &[(TyVar, TypeClass<TyId>)],
        env_fv: &HashSet<TyVar>,
        vars: &mut HashSet<TyVar>,
    ) {
        cs.iter().for_each(|(v, class)| {
            let v = self.uf.find(*v);
            if !env_fv.contains(&v) {
                vars.insert(v);
            }
            class
                .free_vars(&self.ty_arena, &mut self.uf)
                .into_iter()
                .map(|fv| self.uf.find(fv))
                .filter(|fv| !env_fv.contains(fv))
                .for_each(|fv| {
                    vars.insert(fv);
                });
        });
    }

    fn capture_region_classes(
        &mut self,
        reg: &[(Constraint, Option<QualifiedName>)],
        policy: &RootPolicy,
        env_fv: &HashSet<TyVar>,
        cap: &mut CaptureState<'_>,
    ) {
        (0..reg.len()).for_each(|_| {
            reg.iter().enumerate().for_each(|(i, (c, _))| {
                if cap.ix.contains(&i) {
                } else if let Constraint::Class { ty, class, span } = c {
                    if let Some(v) = self.constraint_subject_var(*ty) {
                        if cap.vars.contains(&v) {
                            let class = class.resolve_inner(
                                &mut self.uf,
                                &mut self.ty_arena,
                            );
                            let entry = (v, class.clone());
                            if cap.cs.contains(&entry) {
                                cap.ix.insert(i);
                            } else if cap.rejected.contains(&entry) {
                            } else if self
                                .explicit_missing(v, &class, *span, policy)
                            {
                                cap.rejected.push(entry);
                            } else {
                                class
                                    .free_vars(&self.ty_arena, &mut self.uf)
                                    .into_iter()
                                    .map(|fv| self.uf.find(fv))
                                    .filter(|fv| !env_fv.contains(fv))
                                    .for_each(|fv| {
                                        cap.vars.insert(fv);
                                    });
                                cap.cs.push(entry);
                                cap.ix.insert(i);
                            }
                        }
                    }
                }
            });
        });
    }

    fn explicit_missing(
        &mut self,
        v: TyVar,
        class: &TypeClass<TyId>,
        span: Span,
        policy: &RootPolicy,
    ) -> bool {
        match policy {
            RootPolicy::ExplicitCallable { vars, names }
                if vars.contains(&v) =>
            {
                let param = names
                    .get(&v)
                    .map(|&n| self.env.resolve_string(n))
                    .unwrap_or_else(|| "?".to_owned());
                self.errors.push(TypeError::MissingTypeParamConstraint {
                    param,
                    class: class.clone(),
                    span,
                });
                true
            }
            RootPolicy::InferredLet => {
                self.inferred_let_missing(v, class, span)
            }
            RootPolicy::InferredFun | RootPolicy::ExplicitCallable { .. } => {
                false
            }
        }
    }

    fn inferred_let_missing(
        &mut self,
        v: TyVar,
        class: &TypeClass<TyId>,
        span: Span,
    ) -> bool {
        let v = self.uf.find(v);
        let name = self
            .let_tv_names
            .iter()
            .map(|(&tv, &name)| (tv, name))
            .find(|(tv, _)| self.uf.find(*tv) == v)
            .map(|(_, name)| name);

        match name {
            Some(name) if !self.let_tv_declared(v, class) => {
                let param = self.env.resolve_string(name);
                self.errors.push(TypeError::MissingTypeParamConstraint {
                    param,
                    class: class.clone(),
                    span,
                });
                true
            }
            _ => false,
        }
    }

    fn let_tv_declared(&mut self, v: TyVar, class: &TypeClass<TyId>) -> bool {
        let class = class.resolve_inner(&mut self.uf, &mut self.ty_arena);
        self.let_tv_cs.clone().into_iter().any(|(tv, c)| {
            self.uf.find(tv) == v
                && c.resolve_inner(&mut self.uf, &mut self.ty_arena) == class
        })
    }

    fn constraint_subject_var(&mut self, ty: TyId) -> Option<TyVar> {
        let ty = self.uf.resolve(ty, &mut self.ty_arena);
        match self.ty_arena.get(ty) {
            Ty::Var(v) => Some(self.uf.find(*v)),
            _ => None,
        }
    }

    fn apply_scheme_shapes(
        &mut self,
        reg: &[(Constraint, Option<QualifiedName>)],
    ) {
        reg.iter().for_each(|(c, _)| match c {
            Constraint::Unify(a, b, _) => self.scheme_unify(*a, *b),
            Constraint::Callable {
                callee, args, ret, ..
            } => self.scheme_callable(*callee, args, *ret),
            _ => {}
        });
    }

    fn scheme_callable(&mut self, callee: TyId, args: &[TyId], ret: TyId) {
        let callee = self.uf.resolve(callee, &mut self.ty_arena);
        match self.ty_arena.get(callee).clone() {
            Ty::Fn(params, fn_ret) => {
                params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                    self.scheme_unify(p, a);
                });
                self.scheme_unify(fn_ret, ret);
            }
            Ty::Var(v) => {
                let fn_ty =
                    self.ty_arena.func(args.iter().copied().collect(), ret);
                self.scheme_bind_var(v, fn_ty);
            }
            _ => {}
        }
    }

    fn scheme_unify(&mut self, a: TyId, b: TyId) {
        let a = self.uf.resolve(a, &mut self.ty_arena);
        let b = self.uf.resolve(b, &mut self.ty_arena);
        match (self.ty_arena.get(a).clone(), self.ty_arena.get(b).clone()) {
            (Ty::Var(v), _) => self.scheme_bind_var(v, b),
            (_, Ty::Var(v)) => self.scheme_bind_var(v, a),
            (Ty::Array(x), Ty::Array(y)) | (Ty::Option(x), Ty::Option(y)) => {
                self.scheme_unify(x, y);
            }
            (Ty::Result(ok1, err1), Ty::Result(ok2, err2)) => {
                self.scheme_unify(ok1, ok2);
                self.scheme_unify(err1, err2);
            }
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                self.scheme_unify(k1, k2);
                self.scheme_unify(v1, v2);
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                xs.iter().zip(ys.iter()).for_each(|(&x, &y)| {
                    self.scheme_unify(x, y);
                });
            }
            (Ty::Fn(ps1, r1), Ty::Fn(ps2, r2)) if ps1.len() == ps2.len() => {
                ps1.iter().zip(ps2.iter()).for_each(|(&p1, &p2)| {
                    self.scheme_unify(p1, p2);
                });
                self.scheme_unify(r1, r2);
            }
            (Ty::Object(fs1), Ty::Object(fs2)) => {
                fs1.iter().for_each(|(name, &t1)| {
                    if let Some(&t2) = fs2.get(name) {
                        self.scheme_unify(t1, t2);
                    }
                });
            }
            (Ty::Named(n1, xs), Ty::Named(n2, ys))
                if n1 == n2 && xs.len() == ys.len() =>
            {
                xs.iter().zip(ys.iter()).for_each(|(&x, &y)| {
                    self.scheme_unify(x, y);
                });
            }
            _ => {}
        }
    }

    fn scheme_bind_var(&mut self, v: TyVar, ty: TyId) {
        let root = self.uf.find(v);
        if let Some(bound) = self.uf.probe(root) {
            self.scheme_unify(bound, ty);
        } else if let Ty::Var(w) = self.ty_arena.get(ty) {
            let wr = self.uf.find(*w);
            if root == wr {
            } else if let Some(bound) = self.uf.probe(wr) {
                self.scheme_bind_var(root, bound);
            } else {
                self.uf.union(root, wr);
            }
        } else if self.ty_arena.occurs_uf(ty, root, &mut self.uf) {
        } else {
            self.uf.bind(root, ty);
        }
    }
}

struct CaptureState<'a> {
    vars: &'a mut HashSet<TyVar>,
    cs: &'a mut SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
    ix: HashSet<usize>,
    rejected: SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
}
