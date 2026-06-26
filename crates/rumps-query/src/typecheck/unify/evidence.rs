// Centralizes declaration controlled class evidence for named types.

use super::*;

pub(super) struct EvidenceQuery<'a> {
    pub(super) class: &'a TypeClass<TyId>,
    pub(super) ty: TyId,
    pub(super) span: Span,
}

impl EvidenceQuery<'_> {
    fn target(&self) -> TyId {
        match self.class {
            TypeClass::Concrete { params, .. }
            | TypeClass::Hkt { params, .. } => {
                params.first().copied().unwrap_or(TyArena::UNKNOWN)
            }
        }
    }
}

#[allow(clippy::large_enum_variant)]
pub(super) enum Evidence {
    Manual {
        inst: Instance,
        args: SmallVec<[TyId; 4]>,
    },
    ManualMany {
        insts: SmallVec<[Instance; 2]>,
        args: SmallVec<[TyId; 4]>,
    },
    DerivedVariant {
        payloads: SmallVec<[TyId; 8]>,
    },
    Repr {
        ty: TyId,
        kind: ReprEvidence,
    },
    Union {
        id: Option<TypeId>,
        members: SmallVec<[TyId; 4]>,
    },
    BuiltinNamed,
    BlockedSelf,
    Missing,
    NotImported,
}

#[derive(Clone, Copy)]
pub(super) enum ReprEvidence {
    Derived,
    Transparent,
}

impl SolveCtx<'_> {
    /// Check that a type satisfies a class constraint.
    ///
    /// This is the unified constraint checking method that handles all class
    /// constraints. The `class` parameter contains any associated types, e.g.,
    /// `Into(target)` or `Indexable(elem)`. The union-find is updated in-place
    /// when the constraint involves unification.
    pub(in crate::typecheck) fn satisfies_class(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(ty).clone();
        if let Ty::AssocType(tv, assoc_class, name) = shape {
            if let Some(base) = self.uf.resolve_var(tv, self.ty_arena) {
                if let Ok(resolved) =
                    self.resolve_assoc_type_by_id(base, assoc_class, name, span)
                {
                    self.satisfies_class(class, resolved, span);
                }
            }
        } else {
            self.satisfies_class_inner(class, ty, span);
        }
    }

    fn satisfies_class_inner(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        match class {
            // Keep this before `Into`, `TryInto`, and `Indexable`.
            // `EvidenceQuery::target` treats a missing parameter as `Unknown`,
            // but bare classes are simple class queries.
            TypeClass::Concrete { id, ref params } if params.is_empty() => {
                self.satisfy_simple(*id, class, ty, span)
            }
            TypeClass::Concrete {
                id: ClassId::INTO, ..
            } => self.satisfy_into(EvidenceQuery { class, ty, span }),
            TypeClass::Concrete {
                id: ClassId::TRY_INTO,
                ..
            } => self.satisfy_try_into(EvidenceQuery { class, ty, span }),
            TypeClass::Concrete {
                id: ClassId::INDEXABLE,
                ..
            } => self.satisfy_indexable(EvidenceQuery { class, ty, span }),
            TypeClass::Concrete { id, ref params }
                if id.idx() >= ClassId::BUILTIN_COUNT && !params.is_empty() =>
            {
                self.satisfy_user_param(params, class, ty, span)
            }
            TypeClass::Hkt {
                id: ClassId::ITERABLE,
                ..
            } => {
                self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                ));
            }
            TypeClass::Hkt { id, ref elems, .. }
                if matches!(
                    *id,
                    ClassId::MAPPABLE
                        | ClassId::FILTERABLE
                        | ClassId::FOLDABLE
                        | ClassId::BIMAPPABLE
                ) =>
            {
                self.satisfy_builtin_hkt(*id, elems, class, ty, span)
            }
            TypeClass::Hkt { id, ref elems, .. }
                if id.idx() < ClassId::BUILTIN_COUNT =>
            {
                self.satisfy_hkt_stack(elems, class, ty, span);
            }
            TypeClass::Hkt { elems, .. } => {
                self.satisfy_user_hkt(elems, class, ty, span);
            }
            _ => {}
        }
    }

    pub(super) fn evidence(&mut self, q: EvidenceQuery<'_>) -> Evidence {
        let class = match q.class {
            TypeClass::Concrete { id, .. } | TypeClass::Hkt { id, .. } => *id,
        };
        let params = match q.class {
            TypeClass::Concrete { params, .. }
            | TypeClass::Hkt { params, .. } => params,
        };
        match self.ty_arena.get(q.ty).clone() {
            Ty::Union(id, members) => {
                let manual = id.map(|tid| match params.first().copied() {
                    Some(target) => self.matching_param_inst(
                        class,
                        tid,
                        &[],
                        target,
                        q.span,
                    ),
                    None => {
                        self.visible_insts(class, tid, SmallVec::new(), q.span)
                    }
                });
                match manual {
                    Some(Evidence::Manual { inst, args }) => {
                        Evidence::Manual { inst, args }
                    }
                    Some(Evidence::ManualMany { insts, args }) => {
                        Evidence::ManualMany { insts, args }
                    }
                    Some(Evidence::BlockedSelf) => Evidence::BlockedSelf,
                    Some(Evidence::NotImported) => Evidence::NotImported,
                    _ => Evidence::Union { id, members },
                }
            }
            Ty::Named(id, args) => {
                let manual = match params.first().copied() {
                    Some(target) => self
                        .matching_param_inst(class, id, &args, target, q.span),
                    None => self.visible_insts(class, id, args.clone(), q.span),
                };
                match manual {
                    Evidence::Manual { inst, args } => {
                        Evidence::Manual { inst, args }
                    }
                    Evidence::ManualMany { insts, args } => {
                        Evidence::ManualMany { insts, args }
                    }
                    Evidence::BlockedSelf => Evidence::BlockedSelf,
                    Evidence::NotImported => Evidence::NotImported,
                    Evidence::Missing
                        if class.idx() < ClassId::BUILTIN_COUNT =>
                    {
                        self.derived_variant_evidence(id, &args, class)
                            .or_else(|| {
                                self.repr_evidence(q.class, q.ty, q.span)
                            })
                            .unwrap_or(Evidence::Missing)
                    }
                    _ => Evidence::Missing,
                }
            }
            _ => match self.ty_to_type_id_and_args(q.ty) {
                Some((tid, args)) => match params.first().copied() {
                    Some(target) => self
                        .matching_param_inst(class, tid, &args, target, q.span),
                    None => self.visible_insts(class, tid, args, q.span),
                },
                None => Evidence::Missing,
            },
        }
    }

    pub(super) fn visible_insts(
        &mut self,
        class: ClassId,
        tid: TypeId,
        args: SmallVec<[TyId; 4]>,
        span: Span,
    ) -> Evidence {
        if self.blocks_self_instance(class, tid) {
            self.errors.push(TypeError::SelfInstanceUse {
                class,
                type_id: tid,
                span,
            });
            Evidence::BlockedSelf
        } else {
            let insts = self.instance_registry.lookup_all(class, tid);
            if insts.is_empty() {
                Evidence::Missing
            } else {
                let visible: SmallVec<[Instance; 2]> = insts
                    .iter()
                    .filter(|inst| self.inst_in_scope(inst))
                    .cloned()
                    .collect();
                match visible.as_slice() {
                    [] => {
                        insts
                            .first()
                            .and_then(|inst| inst.module.as_ref())
                            .map(|qn| {
                                self.push_inst_not_imported(
                                    class, tid, qn, span,
                                )
                            });
                        Evidence::NotImported
                    }
                    _ if tid == TypeId::TUPLE && !args.is_empty() => {
                        let matched: SmallVec<[Instance; 2]> = visible
                            .iter()
                            .filter(|inst| inst.type_params.len() == args.len())
                            .cloned()
                            .collect();
                        match matched.as_slice() {
                            [] => Evidence::Missing,
                            [inst] => Evidence::Manual {
                                inst: inst.clone(),
                                args,
                            },
                            _ => Evidence::ManualMany {
                                insts: matched,
                                args,
                            },
                        }
                    }
                    [inst] => Evidence::Manual {
                        inst: inst.clone(),
                        args,
                    },
                    _ => Evidence::ManualMany {
                        insts: visible,
                        args,
                    },
                }
            }
        }
    }

    /// `Into(target)`: `as` casts.
    fn satisfy_into(&mut self, q: EvidenceQuery<'_>) {
        let ty = q.ty;
        let to = q.target();
        let span = q.span;
        let ty = self.uf.resolve(ty, self.ty_arena);
        let to = self.uf.resolve(to, self.ty_arena);
        let ty_shape = self.ty_arena.get(ty).clone();
        let to_shape = self.ty_arena.get(to).clone();
        let unresolved = matches!(
            (&ty_shape, &to_shape),
            (Ty::Var(_), _)
                | (_, Ty::Var(_))
                | (Ty::Error, _)
                | (_, Ty::Error)
                | (Ty::Unknown, _)
                | (_, Ty::Unknown)
        );

        if unresolved || ty == to {
        } else {
            match self.newtype_edge_status(ty, to, span) {
                NewtypeEdgeStatus::Allowed => {}
                NewtypeEdgeStatus::Blocked => {
                    self.errors.push(TypeError::PrivateReprCast {
                        from: ty,
                        to,
                        span,
                    });
                }
                NewtypeEdgeStatus::Missing => match (&ty_shape, &to_shape) {
                    (Ty::Fn(_, _), Ty::String) => {
                        self.errors.push(TypeError::InvalidCast {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Union(_, members), Ty::String) => {
                        let class = TypeClass::param(ClassId::INTO, to);
                        self.satisfy_param_union(&class, members, ty, span);
                    }
                    // The `Into[String]` set is intentionally narrow. This
                    // is separate from `Display`, which is debugging/user-facing
                    // stringification. These are compact string serializations.
                    (
                        Ty::Bool
                        | Ty::Int
                        | Ty::Word
                        | Ty::Float
                        | Ty::Char
                        | Ty::String
                        | Ty::Time
                        | Ty::Json
                        | Ty::FilePath,
                        Ty::String,
                    ) => {}
                    (Ty::Fn(_, _), Ty::Json)
                    | (Ty::Regex, Ty::Json)
                    | (Ty::Local, Ty::Json)
                    | (Ty::Global, Ty::Json) => {
                        self.errors.push(TypeError::InvalidCast {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Array(elem), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::INTO, TyArena::JSON),
                        *elem,
                        span,
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::INTO, TyArena::JSON),
                        *inner,
                        span,
                    ),
                    (Ty::Result(ok, err), Ty::Json) => {
                        let (ok, err) = (*ok, *err);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            ok,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            err,
                            span,
                        );
                    }
                    (Ty::Map(k, v), Ty::Json) => {
                        let (k, v) = (*k, *v);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            k,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            v,
                            span,
                        );
                    }
                    (Ty::Tuple(elems), Ty::Json) => {
                        let es: SmallVec<[TyId; 4]> = elems.clone();
                        es.iter().for_each(|e| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *e,
                                span,
                            )
                        });
                    }
                    (Ty::Object(fields), Ty::Json) => {
                        let vals: SmallVec<[TyId; 4]> =
                            fields.values().copied().collect();
                        vals.iter().for_each(|t| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *t,
                                span,
                            )
                        });
                    }
                    (Ty::Union(_, members), Ty::Json) => {
                        let class = TypeClass::param(ClassId::INTO, to);
                        self.satisfy_param_union(&class, members, ty, span);
                    }
                    // Note that `Into[Json]` is fairly broad because we want
                    // specific types (e.g. `Ordering`, `DataStatus`) to be
                    // storable as JSON in DB nodes with (de)serialization.
                    // This unlike the more restrictive `Into[String]` above
                    (
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
                        | Ty::Path,
                        Ty::Json,
                    ) => {}
                    (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {}
                    (Ty::Word, Ty::Int) | (Ty::Word, Ty::Float) => {}
                    (Ty::Bool, Ty::Int) | (Ty::Int, Ty::Bool) => {}
                    (Ty::DataStatus, Ty::Int) => {}
                    (Ty::String, Ty::FilePath) => {}
                    (Ty::Path, Ty::FilePath) => {}
                    (Ty::Named(id, _), Ty::FilePath) if *id == TypeId::PATH => {
                    }
                    (Ty::Range, Ty::Array(elem)) if *elem == TyArena::INT => {}
                    (Ty::Union(Some(id), _), _) if *id == TypeId::STORABLE => {
                        if !TyArena::STORABLE_MEMBERS.contains(&to) {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }
                    (_, Ty::Union(Some(id), _))
                        if *id == TypeId::STORABLE
                            || *id == TypeId::SCALAR
                            || *id == TypeId::SUBSCRIPT =>
                    {
                        let uid = *id;
                        let is_mem = if uid == TypeId::STORABLE {
                            TyArena::STORABLE_MEMBERS.contains(&ty)
                        } else if uid == TypeId::SCALAR {
                            TyArena::SCALAR_MEMBERS.contains(&ty)
                        } else {
                            TyArena::SUBSCRIPT_MEMBERS.contains(&ty)
                        };
                        if !is_mem {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }
                    (Ty::Union(_, members), _) => {
                        let class = TypeClass::param(ClassId::INTO, to);
                        self.satisfy_param_union(&class, members, ty, span);
                    }
                    (Ty::Named(_, _), _) => {
                        let class = TypeClass::param(ClassId::INTO, to);
                        let q = EvidenceQuery {
                            class: &class,
                            ty,
                            span,
                        };
                        let ev = self.evidence(q);
                        if !self.apply_param_evidence(ev, &class, span) {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }
                    _ => {
                        let handled =
                            self.ty_to_type_id_and_args(ty).is_some_and(|_| {
                                let class = TypeClass::param(ClassId::INTO, to);
                                let q = EvidenceQuery {
                                    class: &class,
                                    ty,
                                    span,
                                };
                                let ev = self.evidence(q);
                                self.apply_param_evidence(ev, &class, span)
                            });
                        if !handled {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }
                },
            }
        }
    }

    /// `TryInto(target)`: `read` casts.
    fn satisfy_try_into(&mut self, q: EvidenceQuery<'_>) {
        let ty = q.ty;
        let to = q.target();
        let span = q.span;
        let ty = self.uf.resolve(ty, self.ty_arena);
        let to = self.uf.resolve(to, self.ty_arena);
        let ty_shape = self.ty_arena.get(ty).clone();
        let to_shape = self.ty_arena.get(to).clone();
        match (&ty_shape, &to_shape) {
            (Ty::Var(_), _) | (_, Ty::Var(_)) => {}
            (Ty::Error, _) | (_, Ty::Error) => {}
            (Ty::Unknown, _) | (_, Ty::Unknown) => {}
            _ if ty == to => {}
            _ => match self.newtype_edge_status(ty, to, span) {
                NewtypeEdgeStatus::Allowed | NewtypeEdgeStatus::Blocked => {
                    let class = TypeClass::param(ClassId::TRY_INTO, to);
                    let q = EvidenceQuery {
                        class: &class,
                        ty,
                        span,
                    };
                    let ev = self.evidence(q);
                    if !self.apply_param_evidence(ev, &class, span) {
                        match self.repr_evidence(&class, ty, span) {
                            Some(Evidence::Repr { ty: repr, .. }) => {
                                self.satisfies_class(&class, repr, span)
                            }
                            _ => self.errors.push(
                                TypeError::NewtypeReprReadRequiresTryInto {
                                    from: ty,
                                    to,
                                    span,
                                },
                            ),
                        }
                    }
                }
                NewtypeEdgeStatus::Missing => match (&ty_shape, &to_shape) {
                    (Ty::Fn(_, _), _) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (_, Ty::Fn(_, _))
                    | (_, Ty::Regex)
                    | (_, Ty::Local)
                    | (_, Ty::Global) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Regex, Ty::Json)
                    | (Ty::Local, Ty::Json)
                    | (Ty::Global, Ty::Json) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    _ => {
                        let class = TypeClass::param(ClassId::TRY_INTO, to);
                        let q = EvidenceQuery {
                            class: &class,
                            ty,
                            span,
                        };
                        let ev = self.evidence(q);
                        if self.apply_param_evidence(ev, &class, span) {
                        } else {
                            match (&ty_shape, &to_shape) {
                                (Ty::Array(elem), Ty::Range)
                                    if *elem == TyArena::INT => {}
                                (_, Ty::Json) => {
                                    self.errors.push(TypeError::InvalidRead {
                                        from: ty,
                                        to,
                                        span,
                                    });
                                }
                                (Ty::Named(_, _), _) => {
                                    match self.repr_evidence(&class, ty, span) {
                                        Some(Evidence::Repr {
                                            ty: repr,
                                            ..
                                        }) => self.satisfies_class(
                                            &class, repr, span,
                                        ),
                                        _ => self.errors.push(
                                            TypeError::InvalidRead {
                                                from: ty,
                                                to,
                                                span,
                                            },
                                        ),
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                },
            },
        };
    }

    fn satisfy_indexable(&mut self, q: EvidenceQuery<'_>) {
        let span = q.span;
        let ty = self.uf.resolve(q.ty, self.ty_arena);
        let elem = self.uf.resolve(q.target(), self.ty_arena);
        let class = TypeClass::param(ClassId::INDEXABLE, elem);
        match self.ty_arena.get(ty).clone() {
            Ty::Array(inner) => {
                if let Err(e) = self.unify_types(elem, inner, span) {
                    self.errors.push(e);
                }
            }
            Ty::Map(key, val) => {
                if let Err(e) = self.unify_types(elem, val, span) {
                    self.errors.push(e);
                }
                self.satisfies_class(
                    &TypeClass::simple(ClassId::ORD),
                    key,
                    span,
                );
            }
            Ty::String => {
                if let Err(e) = self.unify_types(elem, TyArena::CHAR, span) {
                    self.errors.push(e);
                }
            }
            Ty::Union(_, members) => {
                self.satisfy_param_union(&class, &members, ty, span);
            }
            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
            _ => {
                let q = EvidenceQuery {
                    class: &class,
                    ty,
                    span,
                };
                let ev = self.evidence(q);
                if self.apply_param_evidence(ev, &class, span) {
                } else {
                    let bare = TypeClass::simple(ClassId::INDEXABLE);
                    match self.repr_evidence(&bare, ty, span) {
                        Some(Evidence::Repr { ty: repr, .. }) => {
                            self.satisfies_class(&class, repr, span)
                        }
                        _ => self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        )),
                    }
                }
            }
        }
    }

    fn satisfy_user_param(
        &mut self,
        params: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        match self.ty_arena.get(ty).clone() {
            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
            _ => {
                let q = EvidenceQuery { class, ty, span };
                match self.evidence(q) {
                    Evidence::Manual { inst, args } => {
                        self.apply_param_inst(&inst, &args, params, span)
                    }
                    Evidence::ManualMany { insts, args } => {
                        match insts.first() {
                            Some(inst) => {
                                self.apply_param_inst(inst, &args, params, span)
                            }
                            None => {
                                invariant!(
                                    "`ManualMany` has at least one instance"
                                )
                            }
                        }
                    }
                    Evidence::Union { members, .. } => {
                        members.iter().for_each(|&m| {
                            self.satisfies_class(class, m, span)
                        });
                    }
                    Evidence::BlockedSelf | Evidence::NotImported => {}
                    Evidence::Missing
                    | Evidence::DerivedVariant { .. }
                    | Evidence::Repr { .. }
                    | Evidence::BuiltinNamed => {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }
        }
    }

    fn satisfy_param_union(
        &mut self,
        class: &TypeClass<TyId>,
        members: &[TyId],
        ty: TyId,
        span: Span,
    ) {
        let q = EvidenceQuery { class, ty, span };
        let ev = self.evidence(q);
        match ev {
            Evidence::Manual { inst, args } => {
                let params = Self::class_params(class);
                self.apply_param_inst(&inst, &args, params, span);
            }
            Evidence::ManualMany { insts, args } => match insts.first() {
                Some(inst) => {
                    let params = Self::class_params(class);
                    self.apply_param_inst(inst, &args, params, span)
                }
                None => {
                    invariant!("`ManualMany` has at least one instance")
                }
            },
            Evidence::BlockedSelf | Evidence::NotImported => {}
            _ => members
                .iter()
                .for_each(|&m| self.satisfies_class(class, m, span)),
        }
    }

    fn apply_param_evidence(
        &mut self,
        ev: Evidence,
        class: &TypeClass<TyId>,
        span: Span,
    ) -> bool {
        match ev {
            Evidence::Manual { inst, args } => {
                let params = Self::class_params(class);
                self.apply_param_inst(&inst, &args, params, span);
                true
            }
            Evidence::ManualMany { insts, args } => {
                match insts.first() {
                    Some(inst) => {
                        let params = Self::class_params(class);
                        self.apply_param_inst(inst, &args, params, span)
                    }
                    None => {
                        invariant!("`ManualMany` has at least one instance")
                    }
                }
                true
            }
            Evidence::Repr { ty: repr, .. } => {
                self.satisfies_class(class, repr, span);
                true
            }
            Evidence::Union { members, .. } => {
                members
                    .iter()
                    .for_each(|&m| self.satisfies_class(class, m, span));
                true
            }
            Evidence::BlockedSelf | Evidence::NotImported => true,
            Evidence::BuiltinNamed
            | Evidence::DerivedVariant { .. }
            | Evidence::Missing => false,
        }
    }

    fn apply_param_inst(
        &mut self,
        inst: &Instance,
        args: &[TyId],
        params: &[TyId],
        span: Span,
    ) {
        let subst = self.build_instance_subst(inst, args, span);
        inst.class_args
            .iter()
            .zip(params.iter())
            .for_each(|(&ia, &p)| {
                let resolved = self.ty_arena.apply(ia, &subst);
                if let Err(e) = self.unify_types(p, resolved, span) {
                    self.errors.push(e);
                }
            });
        self.apply_inst_constraints(inst, args, span, Some(&subst));
    }

    fn class_params(class: &TypeClass<TyId>) -> &[TyId] {
        match class {
            TypeClass::Concrete { params, .. }
            | TypeClass::Hkt { params, .. } => params,
        }
    }

    fn derived_variant_evidence(
        &mut self,
        id: TypeId,
        args: &[TyId],
        class_id: ClassId,
    ) -> Option<Evidence> {
        let is_variant = self
            .registry
            .get_def(id)
            .is_some_and(|def| matches!(def, TypeDef::Sum { .. }));
        if is_variant
            && self.decls.derives_simple(id, class_id)
            && matches!(
                class_id,
                ClassId::EQ
                    | ClassId::ORD
                    | ClassId::DISPLAY
                    | ClassId::FORMATTABLE
            )
        {
            let subst: IndexMap<StringId, TyId> = self
                .registry
                .get_def(id)
                .map(|def| match def {
                    TypeDef::Sum { type_params, .. }
                    | TypeDef::Alias { type_params, .. }
                    | TypeDef::Union { type_params, .. } => type_params,
                    TypeDef::Builtin(_) => {
                        typechecked!("type parameters", "user declaration")
                    }
                })
                .into_iter()
                .flat_map(|ps| ps.iter().copied().zip(args.iter().copied()))
                .collect();
            let payloads: SmallVec<[AstTypeExprId; 8]> =
                self.decls.variant_payload_exprs(id).collect();
            Some(Evidence::DerivedVariant {
                payloads: payloads
                    .iter()
                    .map(|&p| self.convert_ctx().ast_type_to_ty(p, &subst))
                    .collect(),
            })
        } else {
            None
        }
    }

    fn repr_evidence(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) -> Option<Evidence> {
        self.repr_evidence_inner(class, ty, span, None, &mut HashSet::new())
    }

    fn repr_evidence_inner(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
        kind: Option<ReprEvidence>,
        seen: &mut HashSet<TypeId>,
    ) -> Option<Evidence> {
        match self.alias_parts(ty) {
            Some((alias, _)) if !seen.insert(alias) => {
                self.report_recursive_newtype_edge(ty, span);
                None
            }
            Some((alias, args)) => {
                match self.repr_once_evidence(class, ty, alias, &args, span) {
                    Some((next, k)) if next == ty => {
                        self.report_recursive_newtype_edge(ty, span);
                        None
                    }
                    Some((next, k)) => self.repr_evidence_inner(
                        class,
                        next,
                        span,
                        kind.or(Some(k)),
                        seen,
                    ),
                    None => None,
                }
            }
            None => kind.map(|k| Evidence::Repr { ty, kind: k }),
        }
    }

    fn repr_once_evidence(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        alias: TypeId,
        args: &[TyId],
        span: Span,
    ) -> Option<(TyId, ReprEvidence)> {
        match self.registry.get_def(alias) {
            Some(TypeDef::Alias { .. }) => {
                let target = self.decls.alias_target(alias);
                let is_obj = self
                    .ast
                    .get_type_expr(target)
                    .is_some_and(|te| matches!(te, AstTypeExpr::Object(_)));
                if is_obj {
                    None
                } else {
                    let repr = self.alias_repr(alias, args);
                    self.repr_kind(class, alias, args).and_then(|kind| {
                        self.newtype_edge(ty, repr, span)
                            .filter(|edge| edge.alias == alias)
                            .map(|edge| (edge.repr, kind))
                    })
                }
            }
            _ => None,
        }
    }

    fn repr_kind(
        &mut self,
        class: &TypeClass<TyId>,
        alias: TypeId,
        args: &[TyId],
    ) -> Option<ReprEvidence> {
        let id = match class {
            TypeClass::Concrete { id, .. } | TypeClass::Hkt { id, .. } => *id,
        };
        let has_inst = self.instance_registry.lookup(id, alias).is_some();
        let checks_self = self
            .class_context
            .as_ref()
            .is_some_and(|ctx| ctx.class == id && ctx.type_id == Some(alias));
        if has_inst || checks_self || id.idx() >= ClassId::BUILTIN_COUNT {
            None
        } else {
            let decls = self.decls;
            let reg = self.registry;
            let params = match class {
                TypeClass::Concrete { params, .. }
                | TypeClass::Hkt { params, .. } => params,
            };
            let derived_tag = params.iter().all(|&p| p == TyArena::UNKNOWN)
                && self
                    .decls
                    .derived_instances(alias)
                    .iter()
                    .any(|d| d.class == id);
            let derived = derived_tag
                || decls.derived_instance_matches(
                    reg,
                    alias,
                    class,
                    args,
                    |te, sub| self.convert_ctx().ast_type_to_ty(te, sub),
                );
            if derived {
                Some(ReprEvidence::Derived)
            } else if self.decls.is_transparent(alias) {
                Some(ReprEvidence::Transparent)
            } else {
                None
            }
        }
    }

    fn inst_in_scope(&self, inst: &Instance) -> bool {
        inst.module.as_ref().is_none_or(|qn| {
            self.current_module.as_ref() == Some(qn)
                || self.env.is_module_imported(
                    *qn.segments()
                        .first()
                        .unwrap_or_else(|| invariant!("module has segments")),
                )
        })
    }

    pub(super) fn matching_param_inst(
        &mut self,
        class: ClassId,
        tid: TypeId,
        args: &[TyId],
        target: TyId,
        span: Span,
    ) -> Evidence {
        let target = self.uf.resolve(target, self.ty_arena);
        if self.blocks_self_instance(class, tid) {
            self.errors.push(TypeError::SelfInstanceUse {
                class,
                type_id: tid,
                span,
            });
            Evidence::BlockedSelf
        } else {
            let insts: SmallVec<[Instance; 2]> = self
                .instance_registry
                .lookup_all(class, tid)
                .iter()
                .cloned()
                .collect();
            let open = matches!(
                self.ty_arena.get(target),
                Ty::Var(_) | Ty::Error | Ty::Unknown
            );
            let matches: SmallVec<[Instance; 2]> = if open {
                insts.clone()
            } else {
                insts
                    .iter()
                    .filter(|inst| {
                        inst.class_args.first().is_some_and(|&ia| {
                            let subst =
                                self.build_instance_subst_readonly(inst, args);
                            let resolved = self.ty_arena.apply(ia, &subst);
                            self.uf.resolve(resolved, self.ty_arena) == target
                        })
                    })
                    .cloned()
                    .collect()
            };
            if matches.is_empty() {
                Evidence::Missing
            } else {
                let visible: SmallVec<[Instance; 2]> = matches
                    .iter()
                    .filter(|inst| self.inst_in_scope(inst))
                    .cloned()
                    .collect();
                let ambiguous = if open && visible.len() > 1 {
                    match visible.first() {
                        Some(first) => {
                            let arg = self.inst_arg(first, args);
                            let ats = self.inst_assoc_tys(class, first, args);
                            arg.is_none()
                                || visible.iter().any(|inst| {
                                    self.inst_arg(inst, args) != arg
                                        || self
                                            .inst_assoc_tys(class, inst, args)
                                            != ats
                                })
                        }
                        None => false,
                    }
                } else {
                    false
                };
                if ambiguous {
                    Evidence::Missing
                } else {
                    match visible.as_slice() {
                        [] => {
                            matches
                                .first()
                                .and_then(|inst| inst.module.as_ref())
                                .map(|qn| {
                                    self.push_inst_not_imported(
                                        class, tid, qn, span,
                                    )
                                });
                            Evidence::NotImported
                        }
                        [inst] => Evidence::Manual {
                            inst: inst.clone(),
                            args: SmallVec::from_slice(args),
                        },
                        _ => Evidence::ManualMany {
                            insts: visible,
                            args: SmallVec::from_slice(args),
                        },
                    }
                }
            }
        }
    }

    fn inst_arg(&mut self, inst: &Instance, args: &[TyId]) -> Option<TyId> {
        inst.class_args.first().map(|&ia| {
            let subst = self.build_instance_subst_readonly(inst, args);
            let resolved = self.ty_arena.apply(ia, &subst);
            self.uf.resolve(resolved, self.ty_arena)
        })
    }

    fn inst_assoc_tys(
        &mut self,
        class: ClassId,
        inst: &Instance,
        args: &[TyId],
    ) -> SmallVec<[(StringId, Option<TyId>); 2]> {
        let subst = self.build_instance_subst_readonly(inst, args);
        self.env
            .class_def(class)
            .assoc_types
            .iter()
            .map(|&name| {
                let ty = inst.get_assoc_type(name).map(|def| {
                    let ty = self.ty_arena.apply(def.ty, &subst);
                    self.uf.resolve(ty, self.ty_arena)
                });
                (name, ty)
            })
            .collect()
    }

    fn push_inst_not_imported(
        &mut self,
        class: ClassId,
        tid: TypeId,
        qn: &QualifiedName,
        span: Span,
    ) {
        let exists = self.errors.iter().any(|e| {
            matches!(
                e,
                TypeError::InstanceNotImported {
                    class: c,
                    type_id,
                    ..
                } if *c == class && *type_id == tid
            )
        });
        if !exists {
            self.errors.push(TypeError::InstanceNotImported {
                class,
                type_id: tid,
                module: qn.display(&self.env.strings),
                span,
            });
        }
    }

    fn blocks_self_instance(&self, class: ClassId, tid: TypeId) -> bool {
        self.class_context
            .as_ref()
            .is_some_and(|ctx| ctx.class == class && ctx.type_id == Some(tid))
    }

    fn build_instance_subst_readonly(
        &self,
        inst: &Instance,
        type_args: &[TyId],
    ) -> Rename {
        let vars: SmallVec<[(TyVar, TyId); 2]> = inst
            .type_params
            .iter()
            .zip(type_args.iter())
            .filter_map(|(&p, &a)| match self.ty_arena.get(p) {
                Ty::Var(tv) if p != a => Some((*tv, a)),
                _ => None,
            })
            .collect();
        Rename(vars.into_iter().collect())
    }
}
