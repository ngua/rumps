use super::*;

impl SolveCtx<'_> {
    pub(super) fn find_try_into_instance(
        &mut self,
        ty: TyId,
        to: TyId,
    ) -> Option<Instance> {
        match self.ty_arena.get(ty).clone() {
            Ty::Union(Some(id), _) => {
                self.find_try_into_instance_for(id, &[], to)
            }
            _ => self.ty_to_type_id_and_args(ty).and_then(|(id, args)| {
                self.find_try_into_instance_for(id, &args, to)
            }),
        }
    }
    pub(super) fn find_try_into_instance_for(
        &mut self,
        id: TypeId,
        args: &[TyId],
        to: TyId,
    ) -> Option<Instance> {
        let insts: Vec<Instance> = self
            .instance_registry
            .lookup_all(ClassId::TRY_INTO, id)
            .to_vec();
        let to = self.uf.resolve(to, self.ty_arena);
        insts.into_iter().find(|inst| {
            inst.class_args.first().is_some_and(|&ia| {
                let subst = self.build_instance_subst_readonly(inst, args);
                let resolved = self.ty_arena.apply(ia, &subst);
                self.uf.resolve(resolved, self.ty_arena) == to
            })
        })
    }
    pub(super) fn check_try_into_instance(
        &mut self,
        ty: TyId,
        to: TyId,
        span: Span,
    ) -> bool {
        let inst = match self.ty_arena.get(ty).clone() {
            Ty::Union(Some(id), _) => self
                .find_try_into_instance_for(id, &[], to)
                .map(|inst| (inst, SmallVec::new())),
            _ => self.ty_to_type_id_and_args(ty).and_then(|(id, args)| {
                self.find_try_into_instance_for(id, &args, to)
                    .map(|inst| (inst, args))
            }),
        };
        match inst {
            Some((inst, args)) => {
                let subst = self.build_instance_subst(&inst, &args, span);
                inst.class_args.first().copied().into_iter().for_each(|ia| {
                    let resolved = self.ty_arena.apply(ia, &subst);
                    if let Err(e) = self.unify_types(to, resolved, span) {
                        self.errors.push(e);
                    }
                });
                self.check_instance_constraints(
                    &inst,
                    &args,
                    span,
                    Some(&subst),
                );
                true
            }
            None => false,
        }
    }
    /// Find an instance whose `class_args` match the expected `class_arg`.
    ///
    /// If only one instance exists, returns it directly.
    /// For multiple instances, builds each instance's substitution and
    /// checks if the resolved class arg matches `class_arg`.
    pub(super) fn find_matching_instance(
        &mut self,
        insts: &[Instance],
        class_arg: TyId,
        type_args: &[TyId],
    ) -> Option<Instance> {
        match insts {
            [] => None,
            [single] => Some(single.clone()),
            many => {
                let resolved_arg = self.uf.resolve(class_arg, self.ty_arena);
                many.iter()
                    .find(|inst| {
                        inst.class_args.first().is_some_and(|&ia| {
                            let subst = self
                                .build_instance_subst_readonly(inst, type_args);
                            let resolved = self.ty_arena.apply(ia, &subst);
                            resolved == resolved_arg
                        })
                    })
                    .cloned()
            }
        }
    }
    /// Find an `Into[T]` instance for a type whose `class_args` target matches `to`.
    pub(super) fn find_into_instance(
        &self,
        type_id: TypeId,
        to: TyId,
    ) -> Option<Instance> {
        let insts = self.instance_registry.lookup_all(ClassId::INTO, type_id);
        insts
            .iter()
            .find(|i| i.class_args.first().copied() == Some(to))
            .cloned()
    }
    /// Build a `Rename` from an instance's `type_params` and the actual
    /// `type_args` at a use site. For `Ty::Var` entries, adds the mapping
    /// to the rename. For concrete entries, unifies with the corresponding
    /// `type_arg` to verify they match.
    pub(super) fn build_instance_subst(
        &mut self,
        inst: &Instance,
        type_args: &[TyId],
        span: Span,
    ) -> Rename {
        // Partition into var mappings and concrete pairs first to avoid
        // borrow-checker issues with `ty_arena` vs `unify_types`.
        let (vars, concretes): (
            SmallVec<[(TyVar, TyId); 2]>,
            SmallVec<[(TyId, TyId); 2]>,
        ) = inst.type_params.iter().zip(type_args.iter()).fold(
            (SmallVec::new(), SmallVec::new()),
            |(mut vs, mut cs), (&p, &a)| {
                match self.ty_arena.get(p) {
                    Ty::Var(tv) => vs.push((*tv, a)),
                    _ => cs.push((p, a)),
                }
                (vs, cs)
            },
        );
        concretes.into_iter().for_each(|(p, a)| {
            if let Err(e) = self.unify_types(p, a, span) {
                self.errors.push(e);
            }
        });
        Rename(vars.into_iter().collect())
    }
    /// Like `build_instance_subst` but only builds the var-to-type mapping
    /// without performing unification on concrete entries. Used when
    /// probing for a matching instance among several candidates.
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
                Ty::Var(tv) => Some((*tv, a)),
                _ => None,
            })
            .collect();
        Rename(vars.into_iter().collect())
    }
    /// Check that a user instance's WHERE constraints are satisfied.
    /// Accepts an optional pre-built `Rename` to avoid redundant
    /// `build_instance_subst` calls at sites that already have one.
    pub(super) fn check_instance_constraints(
        &mut self,
        inst: &Instance,
        type_args: &[TyId],
        span: Span,
        subst: Option<&Rename>,
    ) {
        let fallback;
        let inst_subst = match subst {
            Some(r) => r,
            None => {
                fallback = self.build_instance_subst(inst, type_args, span);
                &fallback
            }
        };
        // Collect constraints to avoid borrow conflict
        let constraints: SmallVec<[(TyVar, TypeClass<TyId>); 2]> =
            inst.constraints.clone();
        constraints.iter().for_each(|(var, class)| {
            let var_id = self.ty_arena.alloc(Ty::Var(*var));
            let ty = self.ty_arena.apply(var_id, inst_subst);
            let class = class.apply(inst_subst, self.ty_arena);
            self.satisfies_class(&class, ty, span);
        });
    }
}
