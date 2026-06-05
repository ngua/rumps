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
        constraints: Vec<(Constraint, Option<QualifiedName>)>,
        numeric_vars: &[TyVar],
    ) {
        // Pre-build a map from HKT-constrained type variables to their
        // class ID so `unify_apply` can look up tuple constructor instances
        // for position-aware element decomposition.
        constraints.iter().for_each(|(c, _)| {
            if let Constraint::Class {
                ty,
                class: TypeClass::Hkt { id, .. },
                ..
            } = c
            {
                if let Ty::Var(v) = self.ty_arena.get(*ty) {
                    self.hkt_var_classes.insert(*v, *id);
                }
            }
        });

        // First pass: process `Unify`, `Callable`, `HasField`, and `Indexable`.
        // These constraints generate type bindings (via union-find) that
        // other constraints (Numeric, Into[String], etc.) depend on.
        constraints.iter().for_each(|(c, module)| {
            self.current_module = module.clone();
            match c {
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
                Constraint::Class { ty, class, span } => match class {
                    TypeClass::Concrete {
                        id: ClassId::INDEXABLE,
                        ..
                    } => {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        let class = class.resolve_inner(self.uf, self.ty_arena);
                        self.satisfies_class(&class, ty, *span);
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
        constraints.iter().for_each(|(c, module)| {
            self.current_module = module.clone();
            if let Constraint::Class { ty, class, span } = c {
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
        constraints.iter().for_each(|(c, module)| {
            self.current_module = module.clone();
            if let Constraint::Class { ty, class, span } = c {
                match class {
                    TypeClass::Concrete { ref params, .. }
                        if params.is_empty() => {}
                    TypeClass::Hkt { .. } | TypeClass::Concrete { .. } => {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        let class = class.resolve_inner(self.uf, self.ty_arena);
                        self.satisfies_class(&class, ty, *span);
                    }
                }
            }
        });
    }
}
