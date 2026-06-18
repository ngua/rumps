use super::*;

impl SolveCtx<'_> {
    /// Solve all collected constraints, updating the union-find in-place.
    ///
    /// Processes constraints in order:
    /// 1. `Eq` constraints via unification
    /// 2. `Numeric` constraints (must resolve to `Int` or `Float`)
    /// 3. `Callable` constraints (callee must be `Fn` type)
    /// 4. `Into[String]` constraints (rejects `Fn` types)
    /// 5. `Into[Json]` constraints (rejects `Fn` types)
    /// 6. `Subscript` constraints (must be `Bool | Int | Float | Char | String | Json`)
    /// 7. `Storable` constraints (must be `Bool | Int | Float | Char | String | Json`)
    /// 8. `Fallible` constraints (must be `Option[T]` or `Result[T, E]`; third pass)
    ///
    /// Errors are recorded via `self.errors`; unification continues to collect
    /// as many errors as possible.
    pub(in crate::typecheck) fn solve_constraints(
        &mut self,
        constraints: Vec<PendingConstraint>,
        numeric_vars: &[TyVar],
    ) {
        // Pre-build a map from HKT-constrained type variables to their
        // class ID so `unify_apply` can look up tuple constructor instances
        // for position-aware element decomposition.
        constraints.iter().for_each(|pc| {
            if let Constraint::Class {
                ty,
                class: TypeClass::Hkt { id, .. },
                ..
            } = &pc.c
            {
                if let Ty::Var(v) = self.ty_arena.get(*ty) {
                    self.hkt_var_classes.insert(*v, *id);
                }
            }
        });

        let mut first_indexable = HashSet::new();

        // First pass: process `Unify`, `Callable`, `HasField`, and `Indexable`.
        // These constraints generate type bindings (via union-find) that
        // other constraints (Numeric, Into[String], etc.) depend on.
        constraints.iter().enumerate().for_each(|(i, pc)| {
            self.current_module = pc.module.clone();
            self.class_context = pc.ctx.clone();
            match &pc.c {
                Constraint::Unify(t1, t2, span) => {
                    // Pre-resolve before unifying. While `unify_inner` handles
                    // `Ty::Var` via `unify_var` (which does `find`/`probe`
                    // internally), pre-resolving is still necessary: without it,
                    // error messages on mismatch show unresolved type variables
                    // (e.g. `Option[T]`) instead of concrete types (e.g.
                    // `Option[Int]`).
                    let t1 = self.uf.resolve(*t1, self.ty_arena);
                    let t2 = self.uf.resolve(*t2, self.ty_arena);
                    if let Err(e) = self.unify_types(t1, t2, *span) {
                        self.errors.push(e);
                    }
                }
                Constraint::Callable {
                    callee,
                    args,
                    ret,
                    span,
                } => {
                    let callee = self.uf.resolve(*callee, self.ty_arena);
                    let args: SmallVec<[TyId; 4]> = args
                        .iter()
                        .map(|&t| self.uf.resolve(t, self.ty_arena))
                        .collect();
                    let ret = self.uf.resolve(*ret, self.ty_arena);
                    self.check_callable(callee, &args, ret, *span);
                }
                Constraint::HasField {
                    base,
                    field,
                    field_ty,
                    span,
                } => {
                    let base = self.uf.resolve(*base, self.ty_arena);
                    let field_ty = self.uf.resolve(*field_ty, self.ty_arena);
                    self.check_has_field(base, *field, field_ty, *span);
                }
                Constraint::AssocProjection { .. } => {}
                Constraint::Class { ty, class, span } => match class {
                    TypeClass::Concrete {
                        id: ClassId::INDEXABLE,
                        ..
                    } => {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        let class = class.resolve_inner(self.uf, self.ty_arena);
                        match self.ty_arena.get(ty) {
                            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                            _ => {
                                if self.first_indexable_ready(ty, &class) {
                                    self.satisfies_class(&class, ty, *span);
                                    first_indexable.insert(i);
                                }
                            }
                        }
                    }
                    TypeClass::Hkt { .. } => {}
                    TypeClass::Concrete { .. } => {}
                },
            }
        });

        // Default unresolved numeric type variables to Int after first pass.
        // This ensures subsequent constraint checks (Numeric, Indexable, etc.)
        // see concrete types rather than unresolved type variables.
        // Mimics Haskell's defaulting: `10` becomes `Int` when unconstrained.
        //
        // Important: bind the *resolved* root, not the original. If the
        // numeric type var was unified with another var (e.g., `?N -> ?F`), we
        // must bind `?F -> Int`, not overwrite `?N` (which would lose the link).
        numeric_vars.iter().for_each(|v| {
            let root = self.uf.find(*v);
            if self.uf.probe(root).is_none() {
                self.uf.bind(root, TyArena::INT);
            }
        });

        // Second pass: process simple membership constraints
        constraints.iter().for_each(|pc| {
            self.current_module = pc.module.clone();
            self.class_context = pc.ctx.clone();
            if let Constraint::Class { ty, class, span } = &pc.c {
                match class {
                    TypeClass::Concrete { ref params, .. }
                        if params.is_empty() =>
                    {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        self.satisfies_class(class, ty, *span);
                    }
                    TypeClass::Hkt { .. } | TypeClass::Concrete { .. } => {}
                }
            }
        });

        // Third pass: final check for HKT and parameterized constraints now
        // that numeric type variables have been defaulted and Callable has
        // resolved all type variables through argument unification.
        constraints.iter().enumerate().for_each(|(i, pc)| {
            self.current_module = pc.module.clone();
            self.class_context = pc.ctx.clone();
            match &pc.c {
                Constraint::Class { ty, class, span } => match class {
                    TypeClass::Concrete { ref params, .. }
                        if params.is_empty() => {}
                    TypeClass::Concrete {
                        id: ClassId::INDEXABLE,
                        ..
                    } if first_indexable.contains(&i) => {}
                    TypeClass::Hkt { .. } | TypeClass::Concrete { .. } => {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        let class = class.resolve_inner(self.uf, self.ty_arena);
                        self.satisfies_class(&class, ty, *span);
                    }
                },
                Constraint::AssocProjection {
                    base,
                    class,
                    assoc,
                    ty,
                    span,
                } => {
                    let base = self.uf.resolve(*base, self.ty_arena);
                    let ty = self.uf.resolve(*ty, self.ty_arena);
                    let class = class.resolve_inner(self.uf, self.ty_arena);
                    match self.resolve_assoc_type(base, &class, *assoc, *span) {
                        Ok(resolved) => {
                            if let Err(e) =
                                self.unify_types(ty, resolved, *span)
                            {
                                self.errors.push(e);
                            }
                        }
                        Err(e) => self.errors.push(e),
                    }
                }
                Constraint::Unify(..)
                | Constraint::Callable { .. }
                | Constraint::HasField { .. } => {}
            }
        });
    }

    fn first_indexable_ready(
        &mut self,
        ty: TyId,
        class: &TypeClass<TyId>,
    ) -> bool {
        let target = match class {
            TypeClass::Concrete {
                id: ClassId::INDEXABLE,
                params,
            } => params.first().copied().unwrap_or(TyArena::UNKNOWN),
            TypeClass::Concrete { .. } | TypeClass::Hkt { .. } => {
                TyArena::UNKNOWN
            }
        };
        let target = self.uf.resolve(target, self.ty_arena);
        let open = matches!(
            self.ty_arena.get(target),
            Ty::Var(_) | Ty::Error | Ty::Unknown
        );
        if open {
            !self.open_indexable_ambiguous(ty)
        } else {
            true
        }
    }

    fn open_indexable_ambiguous(&mut self, ty: TyId) -> bool {
        self.ty_to_type_id_and_args(ty).is_some_and(|(tid, args)| {
            let visible: SmallVec<[Instance; 2]> = self
                .instance_registry
                .lookup_all(ClassId::INDEXABLE, tid)
                .iter()
                .filter(|inst| self.inst_visible(inst))
                .cloned()
                .collect();
            if visible.len() > 1 {
                match visible.first() {
                    Some(first) => {
                        let arg = self.c_inst_arg(first, &args);
                        let ats =
                            self.c_inst_assoc(ClassId::INDEXABLE, first, &args);
                        arg.is_none()
                            || visible.iter().any(|inst| {
                                self.c_inst_arg(inst, &args) != arg
                                    || self.c_inst_assoc(
                                        ClassId::INDEXABLE,
                                        inst,
                                        &args,
                                    ) != ats
                            })
                    }
                    None => false,
                }
            } else {
                false
            }
        })
    }

    fn inst_visible(&self, inst: &Instance) -> bool {
        inst.module.as_ref().is_none_or(|qn| {
            self.current_module.as_ref() == Some(qn)
                || self.env.is_module_imported(
                    *qn.segments()
                        .first()
                        .unwrap_or_else(|| invariant!("module has segments")),
                )
        })
    }

    fn c_inst_arg(&mut self, inst: &Instance, args: &[TyId]) -> Option<TyId> {
        inst.class_args.first().map(|&ia| {
            let sub = self.c_inst_sub(inst, args);
            let resolved = self.ty_arena.apply(ia, &sub);
            self.uf.resolve(resolved, self.ty_arena)
        })
    }

    fn c_inst_assoc(
        &mut self,
        class: ClassId,
        inst: &Instance,
        args: &[TyId],
    ) -> SmallVec<[(StringId, Option<TyId>); 2]> {
        let sub = self.c_inst_sub(inst, args);
        self.env
            .class_def(class)
            .assoc_types
            .iter()
            .map(|&name| {
                let ty = inst.get_assoc_type(name).map(|def| {
                    let ty = self.ty_arena.apply(def.ty, &sub);
                    self.uf.resolve(ty, self.ty_arena)
                });
                (name, ty)
            })
            .collect()
    }

    fn c_inst_sub(&self, inst: &Instance, args: &[TyId]) -> Rename {
        let vars: SmallVec<[(TyVar, TyId); 2]> = inst
            .type_params
            .iter()
            .zip(args.iter())
            .filter_map(|(&p, &a)| match self.ty_arena.get(p) {
                Ty::Var(tv) if p != a => Some((*tv, a)),
                _ => None,
            })
            .collect();
        Rename(vars.into_iter().collect())
    }
}
