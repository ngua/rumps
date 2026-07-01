use super::*;

impl SolveCtx<'_> {
    /// Unify a higher-kinded type application `Apply(tv, args)` with another type.
    ///
    /// Decomposes `other` into constructor + element types, binds `tv` to the
    /// constructor shape, and unifies `args` with the element types.
    pub(super) fn unify_apply(
        &mut self,
        tv: TyVar,
        args: &[TyId],
        other: TyId,
        span: Span,
    ) -> UnifyResult {
        // Clone the shape to release arena borrow
        let shape = self.ty_arena.get(other).clone();
        match shape {
            // Parameterized builtins: decompose into constructor + element
            Ty::Option(inner) => {
                let ctor = self.ty_arena.option(TyArena::ERROR);
                self.unify_apply_inner(tv, args, ctor, &[inner], span)
            }
            Ty::Lazy(inner) => {
                let ctor = self.ty_arena.lazy(TyArena::ERROR);
                self.unify_apply_inner(tv, args, ctor, &[inner], span)
            }
            Ty::Result(ok, err) => {
                if args.len() >= 2 {
                    let ctor =
                        self.ty_arena.result(TyArena::ERROR, TyArena::ERROR);
                    self.unify_apply_inner(tv, args, ctor, &[ok, err], span)
                } else {
                    let ctor = self.ty_arena.result(TyArena::ERROR, err);
                    self.unify_apply_inner(tv, args, ctor, &[ok], span)
                }
            }
            Ty::Array(inner) => {
                let ctor = self.ty_arena.array(TyArena::ERROR);
                self.unify_apply_inner(tv, args, ctor, &[inner], span)
            }
            Ty::Map(k, v) => {
                if args.len() >= 2 {
                    let ctor =
                        self.ty_arena.map_ty(TyArena::ERROR, TyArena::ERROR);
                    self.unify_apply_inner(tv, args, ctor, &[k, v], span)
                } else {
                    let ctor = self.ty_arena.map_ty(TyArena::ERROR, v);
                    self.unify_apply_inner(tv, args, ctor, &[k], span)
                }
            }

            Ty::Union(prov, ref members) => {
                let parts: Option<SmallVec<[(TyId, SmallVec<[TyId; 4]>); 4]>> =
                    members
                        .iter()
                        .map(|&m| match self.ty_arena.get(m).clone() {
                            Ty::Option(inner) => Some((
                                self.ty_arena.option(TyArena::ERROR),
                                smallvec![inner],
                            )),
                            Ty::Lazy(inner) => Some((
                                self.ty_arena.lazy(TyArena::ERROR),
                                smallvec![inner],
                            )),
                            Ty::Result(ok, err) => {
                                if args.len() >= 2 {
                                    Some((
                                        self.ty_arena.result(
                                            TyArena::ERROR,
                                            TyArena::ERROR,
                                        ),
                                        smallvec![ok, err],
                                    ))
                                } else {
                                    Some((
                                        self.ty_arena
                                            .result(TyArena::ERROR, err),
                                        smallvec![ok],
                                    ))
                                }
                            }
                            Ty::Array(inner) => Some((
                                self.ty_arena.array(TyArena::ERROR),
                                smallvec![inner],
                            )),
                            Ty::Map(k, v) => {
                                if args.len() >= 2 {
                                    Some((
                                        self.ty_arena.map_ty(
                                            TyArena::ERROR,
                                            TyArena::ERROR,
                                        ),
                                        smallvec![k, v],
                                    ))
                                } else if self
                                    .hkt_var_classes
                                    .get(&self.uf.find(tv))
                                    .is_some_and(|&id| {
                                        matches!(
                                            id,
                                            ClassId::MAPPABLE
                                                | ClassId::FILTERABLE
                                        )
                                    })
                                {
                                    Some((
                                        self.ty_arena.map_ty(k, TyArena::ERROR),
                                        smallvec![v],
                                    ))
                                } else {
                                    Some((
                                        self.ty_arena.map_ty(TyArena::ERROR, v),
                                        smallvec![k],
                                    ))
                                }
                            }
                            Ty::Named(id, ref type_args) => {
                                if type_args.is_empty() {
                                    None
                                } else {
                                    let start = type_args
                                        .len()
                                        .saturating_sub(args.len());
                                    let elems = type_args
                                        .iter()
                                        .skip(start)
                                        .copied()
                                        .collect();
                                    let placeholder = type_args
                                        .iter()
                                        .enumerate()
                                        .map(|(i, &t)| {
                                            if i >= start {
                                                TyArena::ERROR
                                            } else {
                                                t
                                            }
                                        })
                                        .collect();
                                    Some((
                                        self.ty_arena.named(id, placeholder),
                                        elems,
                                    ))
                                }
                            }
                            Ty::Tuple(ref ts) => {
                                let start = ts.len().saturating_sub(args.len());
                                let elems =
                                    ts.iter().skip(start).copied().collect();
                                let placeholder = ts
                                    .iter()
                                    .enumerate()
                                    .map(|(i, &t)| {
                                        if i >= start {
                                            TyArena::ERROR
                                        } else {
                                            t
                                        }
                                    })
                                    .collect();
                                Some((
                                    self.ty_arena.alloc(Ty::Tuple(placeholder)),
                                    elems,
                                ))
                            }
                            _ => None,
                        })
                        .collect();

                match parts {
                    Some(parts) => {
                        let ctors =
                            parts.iter().map(|(ctor, _)| *ctor).collect();
                        let ctor = self.ty_arena.alloc(Ty::Union(prov, ctors));
                        parts
                            .iter()
                            .find_map(|(_, elems)| {
                                let snap = self.uf.snapshot();
                                match self.unify_apply_inner(
                                    tv, args, ctor, elems, span,
                                ) {
                                    Ok(()) => Some(Ok(())),
                                    Err(_) => {
                                        self.uf.rollback(snap);
                                        None
                                    }
                                }
                            })
                            .unwrap_or_else(|| {
                                let got_args: SmallVec<[TyId; 4]> =
                                    args.iter().copied().collect();
                                let got = self.ty_arena.hkt(tv, got_args);
                                Err(TypeError::Mismatch {
                                    expected: other,
                                    got,
                                    span,
                                })
                            })
                    }
                    None => {
                        let got_args: SmallVec<[TyId; 4]> =
                            args.iter().copied().collect();
                        let got = self.ty_arena.hkt(tv, got_args);
                        Err(TypeError::Mismatch {
                            expected: other,
                            got,
                            span,
                        })
                    }
                }
            }

            // User-defined named types: decompose into constructor + element
            // Element type is the LAST type arg (Haskell curried convention).
            // Non-element (fixed) args are preserved in the constructor placeholder.
            Ty::Named(id, ref type_args) => {
                if type_args.is_empty() {
                    self.unify_var(tv, other, span)
                } else {
                    let start = type_args.len().saturating_sub(args.len());
                    let elems: SmallVec<[TyId; 4]> =
                        type_args.iter().skip(start).copied().collect();
                    let placeholder: SmallVec<[TyId; 4]> = type_args
                        .iter()
                        .enumerate()
                        .map(
                            |(i, &t)| {
                                if i >= start {
                                    TyArena::ERROR
                                } else {
                                    t
                                }
                            },
                        )
                        .collect();
                    let ctor = self.ty_arena.named(id, placeholder);
                    self.unify_apply_inner(tv, args, ctor, &elems, span)
                }
            }

            // Two `Apply` nodes: unify constructors and args pairwise
            Ty::Apply(tv2, ref args2) => {
                if args.len() != args2.len() {
                    let exp = self.ty_arena.hkt(tv2, args2.clone());
                    let got_args: SmallVec<[TyId; 4]> =
                        args.iter().copied().collect();
                    let got = self.ty_arena.hkt(tv, got_args);
                    Err(TypeError::Mismatch {
                        expected: exp,
                        got,
                        span,
                    })
                } else {
                    let a2: SmallVec<[TyId; 4]> = args2.clone();
                    let tv2_id = self.ty_arena.alloc(Ty::Var(tv2));
                    self.unify_var(tv, tv2_id, span)?;
                    self.unify_sequence(
                        args.iter().copied(),
                        a2.iter().copied(),
                        span,
                    )
                }
            }

            Ty::Tuple(ref ts) => {
                // Try to find element positions from an HKT class
                // constraint on `tv`; this handles tuple constructors
                // with interleaved fixed/element positions like `(,T,)`
                let root = self.uf.find(tv);
                let positions = self
                    .hkt_var_classes
                    .get(&root)
                    .and_then(|&cid| {
                        self.instance_registry
                            .lookup_tuple(cid, ts.len())
                            .cloned()
                    })
                    .and_then(|inst| {
                        let ca: SmallVec<[usize; 4]> = inst
                            .type_params
                            .iter()
                            .enumerate()
                            .filter(|(_, &p)| inst.class_args.contains(&p))
                            .map(|(i, _)| i)
                            .collect();
                        (ca.len() == args.len()).then_some(ca)
                    });

                let (elems, placeholder) = match positions {
                    Some(ref pos) => {
                        let e: SmallVec<[TyId; 4]> = pos
                            .iter()
                            .filter_map(|&i| ts.get(i).copied())
                            .collect();
                        let p: SmallVec<[TyId; 4]> = ts
                            .iter()
                            .enumerate()
                            .map(|(i, &t)| {
                                if pos.contains(&i) {
                                    TyArena::ERROR
                                } else {
                                    t
                                }
                            })
                            .collect();
                        (e, p)
                    }
                    None => {
                        let start = ts.len().saturating_sub(args.len());
                        let e: SmallVec<[TyId; 4]> =
                            ts.iter().skip(start).copied().collect();
                        let p: SmallVec<[TyId; 4]> =
                            ts.iter()
                                .enumerate()
                                .map(|(i, &t)| {
                                    if i >= start {
                                        TyArena::ERROR
                                    } else {
                                        t
                                    }
                                })
                                .collect();
                        (e, p)
                    }
                };

                let ctor = self.ty_arena.alloc(Ty::Tuple(placeholder));
                self.unify_apply_inner(tv, args, ctor, &elems, span)
            }

            _ => {
                let got_args: SmallVec<[TyId; 4]> =
                    args.iter().copied().collect();
                let got = self.ty_arena.hkt(tv, got_args);
                Err(TypeError::Mismatch {
                    expected: other,
                    got,
                    span,
                })
            }
        }
    }

    /// Bind `tv` to a constructor shape and unify `Apply` args with
    /// the element types pairwise.
    fn unify_apply_inner(
        &mut self,
        tv: TyVar,
        args: &[TyId],
        ctor: TyId,
        elems: &[TyId],
        span: Span,
    ) -> UnifyResult {
        self.unify_var(tv, ctor, span)?;
        self.unify_hkt_args(args, elems, span)
    }

    fn hkt_arity(exp: usize, got: usize, span: Span) -> TypeError {
        TypeError::ArityMismatch {
            expected: exp,
            got,
            span,
        }
    }

    fn unify_hkt_args(
        &mut self,
        exp: &[TyId],
        got: &[TyId],
        span: Span,
    ) -> UnifyResult {
        if exp.len() == got.len() {
            self.unify_sequence(exp.iter().copied(), got.iter().copied(), span)
        } else {
            Err(Self::hkt_arity(exp.len(), got.len(), span))
        }
    }

    pub(super) fn unify_hkt_known_args(
        &mut self,
        exp: &[TyId],
        got: &[TyId],
        span: Span,
    ) {
        let res = if exp.is_empty() {
            Ok(())
        } else {
            self.unify_hkt_args(exp, got, span)
        };
        if let Err(e) = res {
            self.errors.push(e);
        }
    }

    pub(super) fn unify_hkt_inst_args(
        &mut self,
        exp: &[TyId],
        got: &[TyId],
        subst: &Rename,
        span: Span,
    ) {
        if exp.is_empty() {
            Ok(())
        } else if exp.len() == got.len() {
            exp.iter().zip(got.iter()).try_for_each(|(&e, &g)| {
                let resolved = self.ty_arena.apply(g, subst);
                self.unify_types(e, resolved, span)
            })
        } else {
            Err(Self::hkt_arity(exp.len(), got.len(), span))
        }
        .unwrap_or_else(|e| {
            self.errors.push(e);
        });
    }
}
