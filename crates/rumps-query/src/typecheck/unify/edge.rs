use super::*;

#[derive(Clone, Copy)]
struct NewtypeEdgeCtx {
    from: TyId,
    to: TyId,
    span: Span,
}

#[derive(Clone, Copy)]
struct NewtypeAlias<'a> {
    id: TypeId,
    args: &'a [TyId],
    repr: TyId,
}

#[derive(Clone, Copy)]
enum NewtypeEdgeMode {
    Accessible,
    Any,
}

#[derive(Clone)]
struct EdgeState {
    ctx: NewtypeEdgeCtx,
    seen: HashSet<TypeId>,
    cycle: Option<TyId>,
    mode: NewtypeEdgeMode,
}

struct ObjRepr<'a> {
    alias: TypeId,
    args: &'a [TyId],
    repr: TyId,
    fields: &'a SmallVec<[(StringId, AstTypeExprId); 4]>,
    other: TyId,
}

struct EdgeUnionBt<'a> {
    rhs: &'a [TyId],
    avail: Vec<usize>,
}

impl SolveCtx<'_> {
    pub(in crate::typecheck) fn newtype_edge(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> Option<NewtypeEdge> {
        let (edge, cycle) = self.newtype_edge_with_mode(
            from,
            to,
            span,
            NewtypeEdgeMode::Accessible,
        );
        if edge.is_none() {
            if let Some(ty) = cycle {
                self.report_recursive_newtype_edge(ty, span);
            }
        }
        edge
    }

    pub(in crate::typecheck) fn newtype_edge_any(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> Option<NewtypeEdge> {
        self.newtype_edge_with_mode(from, to, span, NewtypeEdgeMode::Any)
            .0
    }

    pub(in crate::typecheck) fn newtype_edge_status(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> NewtypeEdgeStatus {
        let (edge, cycle) = self.newtype_edge_with_mode(
            from,
            to,
            span,
            NewtypeEdgeMode::Accessible,
        );
        match edge {
            Some(_) => NewtypeEdgeStatus::Allowed,
            None => {
                let snap = self.uf.snapshot();
                let err_len = self.errors.len();
                let (blocked, any_cycle) = self.newtype_edge_with_mode(
                    from,
                    to,
                    span,
                    NewtypeEdgeMode::Any,
                );
                self.uf.rollback(snap);
                self.errors.truncate(err_len);
                match blocked {
                    Some(_) => NewtypeEdgeStatus::Blocked,
                    None => {
                        if let Some(ty) = cycle.or(any_cycle) {
                            self.report_recursive_newtype_edge(ty, span);
                        }
                        NewtypeEdgeStatus::Missing
                    }
                }
            }
        }
    }

    pub(super) fn newtype_edge_blocked(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> bool {
        let snap = self.uf.snapshot();
        let err_len = self.errors.len();
        let blocked = self.newtype_edge_status(from, to, span)
            == NewtypeEdgeStatus::Blocked;
        self.uf.rollback(snap);
        self.errors.truncate(err_len);
        blocked
    }

    fn newtype_edge_with_mode(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
        mode: NewtypeEdgeMode,
    ) -> (Option<NewtypeEdge>, Option<TyId>) {
        let from = self.uf.resolve(from, self.ty_arena);
        let to = self.uf.resolve(to, self.ty_arena);

        let ctx = NewtypeEdgeCtx { from, to, span };
        let mut st = EdgeState {
            ctx,
            seen: HashSet::new(),
            cycle: None,
            mode,
        };
        let edge = self
            .newtype_edge_candidate(from, to, &mut st)
            .or_else(|| self.newtype_edge_candidate(to, from, &mut st));
        (edge, st.cycle)
    }

    fn newtype_edge_candidate(
        &mut self,
        alias_ty: TyId,
        other: TyId,
        st: &mut EdgeState,
    ) -> Option<NewtypeEdge> {
        let snap = self.uf.snapshot();
        let err_len = self.errors.len();
        st.seen.clear();
        let edge = self.newtype_edge_inner(alias_ty, other, st);
        if edge.is_some() && self.errors.len() == err_len {
            edge
        } else {
            self.uf.rollback(snap);
            self.errors.truncate(err_len);
            None
        }
    }

    fn newtype_edge_inner(
        &mut self,
        alias_ty: TyId,
        other: TyId,
        st: &mut EdgeState,
    ) -> Option<NewtypeEdge> {
        match self.alias_parts(alias_ty) {
            Some((alias, args)) => {
                if !st.seen.insert(alias) {
                    st.cycle = Some(alias_ty);
                    None
                } else if matches!(st.mode, NewtypeEdgeMode::Accessible)
                    && !self.convert_ctx().can_access_alias_repr(alias)
                {
                    st.seen.remove(&alias);
                    None
                } else {
                    let repr = self.alias_repr(alias, &args);
                    let edge = match self.newtype_repr_matches(
                        NewtypeAlias {
                            id: alias,
                            args: &args,
                            repr,
                        },
                        other,
                        st,
                    ) {
                        Ok(()) => Some(NewtypeEdge {
                            alias,
                            from: st.ctx.from,
                            to: st.ctx.to,
                            repr,
                        }),
                        Err(_) => None,
                    };
                    st.seen.remove(&alias);
                    edge
                }
            }
            None => None,
        }
    }

    fn newtype_repr_matches(
        &mut self,
        alias: NewtypeAlias<'_>,
        other: TyId,
        st: &mut EdgeState,
    ) -> UnifyResult {
        match self
            .ast
            .get_type_expr(self.decls.alias_target(alias.id))
            .cloned()
        {
            Some(AstTypeExpr::Object(fields)) => self
                .newtype_object_repr_matches(
                    ObjRepr {
                        alias: alias.id,
                        args: alias.args,
                        repr: alias.repr,
                        fields: &fields,
                        other,
                    },
                    st,
                ),
            _ => self.newtype_structural_match(alias.repr, other, st),
        }
    }

    fn newtype_object_repr_matches(
        &mut self,
        req: ObjRepr<'_>,
        st: &mut EdgeState,
    ) -> UnifyResult {
        match self.ty_arena.get(req.other).clone() {
            Ty::Object(obj_fields) => {
                let ps = match self.registry.get_def(req.alias) {
                    Some(TypeDef::Alias { type_params, .. }) => {
                        type_params.clone()
                    }
                    _ => typechecked!("newtype edge", "alias declaration"),
                };
                let subst: IndexMap<StringId, TyId> = ps
                    .iter()
                    .zip(req.args.iter())
                    .map(|(&p, &a)| (p, a))
                    .collect();
                req.fields.iter().try_for_each(|(name, ast_ty)| {
                    let exp =
                        self.convert_ctx().ast_type_to_ty(*ast_ty, &subst);
                    match obj_fields.get(name) {
                        Some(&got) => {
                            self.newtype_structural_match(exp, got, st)
                        }
                        None => Err(TypeError::MissingField {
                            ty: req.alias,
                            field: self.env.resolve_string(*name),
                            span: st.ctx.span,
                        }),
                    }
                })
            }
            _ => Err(TypeError::Mismatch {
                expected: req.repr,
                got: req.other,
                span: st.ctx.span,
            }),
        }
    }

    fn newtype_structural_match(
        &mut self,
        t1: TyId,
        t2: TyId,
        st: &mut EdgeState,
    ) -> UnifyResult {
        if t1 == t2 {
            Ok(())
        } else {
            let ty1 = self.ty_arena.get(t1).clone();
            let ty2 = self.ty_arena.get(t2).clone();
            match (ty1, ty2) {
                (Ty::Error, _) | (_, Ty::Error) => Ok(()),
                (Ty::Unknown, _) | (_, Ty::Unknown) => Ok(()),
                (Ty::Named(id, _), _) if self.decls.is_alias(id) => {
                    self.newtype_edge_inner(t1, t2, st).map_or_else(
                        || {
                            Err(TypeError::Mismatch {
                                expected: t2,
                                got: t1,
                                span: st.ctx.span,
                            })
                        },
                        |_| Ok(()),
                    )
                }
                (_, Ty::Named(id, _)) if self.decls.is_alias(id) => {
                    self.newtype_edge_inner(t2, t1, st).map_or_else(
                        || {
                            Err(TypeError::Mismatch {
                                expected: t2,
                                got: t1,
                                span: st.ctx.span,
                            })
                        },
                        |_| Ok(()),
                    )
                }

                (Ty::Var(v), _) => self.unify_var(v, t2, st.ctx.span),
                (_, Ty::Var(v)) => self.unify_var(v, t1, st.ctx.span),

                (Ty::Bool, Ty::Bool)
                | (Ty::Int, Ty::Int)
                | (Ty::Word, Ty::Word)
                | (Ty::Float, Ty::Float)
                | (Ty::Char, Ty::Char)
                | (Ty::String, Ty::String)
                | (Ty::Unit, Ty::Unit)
                | (Ty::Time, Ty::Time)
                | (Ty::Range, Ty::Range)
                | (Ty::Json, Ty::Json)
                | (Ty::Ordering, Ty::Ordering)
                | (Ty::DataStatus, Ty::DataStatus)
                | (Ty::FilePath, Ty::FilePath)
                | (Ty::Path, Ty::Path)
                | (Ty::Regex, Ty::Regex)
                | (Ty::RuntimeError, Ty::RuntimeError)
                | (Ty::Local, Ty::Local)
                | (Ty::Global, Ty::Global) => Ok(()),

                (Ty::Array(a), Ty::Array(b))
                | (Ty::Option(a), Ty::Option(b))
                | (Ty::Lazy(a), Ty::Lazy(b)) => {
                    self.newtype_structural_match(a, b, st)
                }
                (Ty::Result(ok1, err1), Ty::Result(ok2, err2)) => {
                    self.newtype_structural_match(ok1, ok2, st)?;
                    self.newtype_structural_match(err1, err2, st)
                }
                (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                    self.newtype_structural_match(k1, k2, st)?;
                    self.newtype_structural_match(v1, v2, st)
                }
                (Ty::Tuple(ts1), Ty::Tuple(ts2)) => {
                    if ts1.len() == ts2.len() {
                        self.newtype_structural_sequence(
                            ts1.iter().copied(),
                            ts2.iter().copied(),
                            st,
                        )
                    } else {
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span: st.ctx.span,
                        })
                    }
                }
                (Ty::Fn(ps1, ret1), Ty::Fn(ps2, ret2)) => {
                    if ps1.len() == ps2.len() {
                        self.newtype_structural_sequence(
                            ps1.iter().copied(),
                            ps2.iter().copied(),
                            st,
                        )?;
                        self.newtype_structural_match(ret1, ret2, st)
                    } else {
                        Err(TypeError::ArityMismatch {
                            expected: ps2.len(),
                            got: ps1.len(),
                            span: st.ctx.span,
                        })
                    }
                }
                (Ty::Object(fs1), Ty::Object(fs2)) => {
                    self.newtype_structural_objects(&fs1, &fs2, st)
                }
                (Ty::Union(_, ms1), Ty::Union(_, ms2)) => {
                    if ms1.len() == ms2.len() {
                        let avail: Vec<usize> = (0..ms2.len()).collect();
                        self.newtype_structural_union(
                            &ms1,
                            EdgeUnionBt { rhs: &ms2, avail },
                            st,
                        )
                        .unwrap_or_else(|| {
                            Err(TypeError::Mismatch {
                                expected: t2,
                                got: t1,
                                span: st.ctx.span,
                            })
                        })
                    } else {
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span: st.ctx.span,
                        })
                    }
                }
                (Ty::Named(id1, args1), Ty::Named(id2, args2)) => {
                    if id1 == id2 && args1.len() == args2.len() {
                        self.newtype_structural_sequence(
                            args1.iter().copied(),
                            args2.iter().copied(),
                            st,
                        )
                    } else {
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span: st.ctx.span,
                        })
                    }
                }
                _ => Err(TypeError::Mismatch {
                    expected: t2,
                    got: t1,
                    span: st.ctx.span,
                }),
            }
        }
    }

    fn newtype_structural_sequence(
        &mut self,
        ts1: impl Iterator<Item = TyId>,
        ts2: impl Iterator<Item = TyId>,
        st: &mut EdgeState,
    ) -> UnifyResult {
        ts1.zip(ts2)
            .try_for_each(|(t1, t2)| self.newtype_structural_match(t1, t2, st))
    }

    fn newtype_structural_objects(
        &mut self,
        fields1: &IndexMap<StringId, TyId>,
        fields2: &IndexMap<StringId, TyId>,
        st: &mut EdgeState,
    ) -> UnifyResult {
        fields1.iter().try_for_each(|(name, t1)| {
            fields2.get(name).map_or(Ok(()), |&t2| {
                self.newtype_structural_match(*t1, t2, st)
            })
        })
    }

    fn newtype_structural_union(
        &mut self,
        lhs: &[TyId],
        bt: EdgeUnionBt<'_>,
        st: &mut EdgeState,
    ) -> Option<UnifyResult> {
        match lhs.split_first() {
            None => Some(Ok(())),
            Some((&first, rest)) => {
                self.newtype_structural_union_matches(first, rest, bt, st, 0)
            }
        }
    }

    fn newtype_structural_union_matches(
        &mut self,
        first: TyId,
        rest: &[TyId],
        bt: EdgeUnionBt<'_>,
        st: &mut EdgeState,
        start: usize,
    ) -> Option<UnifyResult> {
        match bt.avail.get(start).copied() {
            Some(idx) => {
                let m2 = *bt.rhs.get(idx)?;
                let snap = self.uf.snapshot();
                let err_len = self.errors.len();
                let mut branch = st.clone();
                match self.newtype_structural_match(first, m2, &mut branch) {
                    Ok(()) => {
                        let next = EdgeUnionBt {
                            rhs: bt.rhs,
                            avail: bt
                                .avail
                                .iter()
                                .copied()
                                .filter(|&i| i != idx)
                                .collect(),
                        };
                        match self.newtype_structural_union(
                            rest,
                            next,
                            &mut branch,
                        ) {
                            Some(Ok(())) => {
                                *st = branch;
                                Some(Ok(()))
                            }
                            _ => {
                                if st.cycle.is_none() {
                                    st.cycle = branch.cycle;
                                }
                                self.uf.rollback(snap);
                                self.errors.truncate(err_len);
                                self.newtype_structural_union_matches(
                                    first,
                                    rest,
                                    bt,
                                    st,
                                    start + 1,
                                )
                            }
                        }
                    }
                    Err(_) => {
                        if st.cycle.is_none() {
                            st.cycle = branch.cycle;
                        }
                        self.uf.rollback(snap);
                        self.errors.truncate(err_len);
                        self.newtype_structural_union_matches(
                            first,
                            rest,
                            bt,
                            st,
                            start + 1,
                        )
                    }
                }
            }
            None => None,
        }
    }
}
