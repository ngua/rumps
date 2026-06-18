use super::*;

impl SolveCtx<'_> {
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
                    Ty::Var(tv) if p != a => vs.push((*tv, a)),
                    Ty::Var(_) => {}
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

    /// Check that a user instance's WHERE constraints are satisfied.
    /// Accepts an optional pre-built `Rename` to avoid redundant
    /// `build_instance_subst` calls at sites that already have one.
    pub(super) fn apply_inst_constraints(
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
