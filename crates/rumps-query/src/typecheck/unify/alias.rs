use super::*;

impl SolveCtx<'_> {
    pub(super) fn alias_parts(
        &self,
        ty: TyId,
    ) -> Option<(TypeId, SmallVec<[TyId; 4]>)> {
        match self.ty_arena.get(ty).clone() {
            Ty::Named(id, args) if self.decls.is_alias(id) => Some((id, args)),
            _ => None,
        }
    }
    pub(super) fn alias_repr(&mut self, alias: TypeId, args: &[TyId]) -> TyId {
        match self.registry.get_def(alias) {
            Some(TypeDef::Alias { type_params, .. }) => {
                let ps = type_params.clone();
                let subst: IndexMap<StringId, TyId> =
                    ps.iter().zip(args.iter()).map(|(&p, &a)| (p, a)).collect();
                let target = self.decls.alias_target(alias);
                self.convert_ctx().ast_type_to_ty(target, &subst)
            }
            _ => typechecked!("newtype edge", "alias declaration"),
        }
    }
    pub(super) fn report_recursive_newtype_edge(
        &mut self,
        ty: TyId,
        span: Span,
    ) {
        let v = self.uf.fresh();
        self.errors.push(TypeError::InfiniteType(v, ty, span));
    }
    /// Expand a `Ty::Named` alias fully to its target type.
    ///
    /// Recursively expands chained aliases (e.g., `A = B`, `B = Int`) until
    /// reaching a non-alias type. Object aliases are NOT expanded; they need
    /// special handling in `unify_named_with_object`.
    pub(super) fn expand_alias_fully(
        &mut self,
        ty: TyId,
        other: TyId,
        span: Span,
    ) -> Option<TyId> {
        self.expand_alias_fully_inner(ty, other, span, false)
    }
    fn expand_alias_fully_inner(
        &mut self,
        current: TyId,
        other: TyId,
        span: Span,
        expanded: bool,
    ) -> Option<TyId> {
        match self.expand_alias_once(current, other, span) {
            Some(next) => {
                self.expand_alias_fully_inner(next, other, span, true)
            }
            None => expanded.then_some(current),
        }
    }
    /// Expand a `Ty::Named` alias one level.
    ///
    /// If `ty` is `Ty::Named(id, args)` where `id` refers to a `TypeDef::Alias`,
    /// returns the expanded target type with type args substituted. Otherwise
    /// returns `None`.
    ///
    /// Note: Aliases to object types are NOT expanded here; they need special
    /// handling in `unify_named_with_object` to check all required fields.
    fn expand_alias_once(
        &mut self,
        ty: TyId,
        other: TyId,
        span: Span,
    ) -> Option<TyId> {
        let type_id = match self.ty_arena.get(ty) {
            Ty::Named(id, _) => *id,
            _ => None?,
        };
        match self.registry.get_def(type_id) {
            Some(TypeDef::Alias { .. }) => {
                if self
                    .alias_parts(other)
                    .is_some_and(|(other_id, _)| other_id == type_id)
                {
                    None
                } else {
                    let target = self.decls.alias_target(type_id);
                    // Don't expand if target is an object type; let
                    // `unify_named_with_object` handle it for proper
                    // required-field checking
                    let is_obj = self
                        .ast
                        .get_type_expr(target)
                        .is_some_and(|te| matches!(te, AstTypeExpr::Object(_)));
                    if is_obj {
                        None
                    } else {
                        self.newtype_edge(ty, other, span)
                            .filter(|edge| edge.alias == type_id)
                            .map(|edge| edge.repr)
                    }
                }
            }
            _ => None,
        }
    }
    pub(super) fn expand_alias_fully_for_class(
        &mut self,
        ty: TyId,
        span: Span,
    ) -> Option<TyId> {
        self.expand_alias_fully_for_class_inner(
            ty,
            span,
            false,
            &mut HashSet::new(),
        )
    }
    fn expand_alias_fully_for_class_inner(
        &mut self,
        current: TyId,
        span: Span,
        expanded: bool,
        seen: &mut HashSet<TypeId>,
    ) -> Option<TyId> {
        match self.alias_parts(current) {
            Some((alias, _)) if !seen.insert(alias) => {
                self.report_recursive_newtype_edge(current, span);
                None
            }
            Some(_) => match self.expand_alias_once_for_class(current, span) {
                Some(next) if next == current => {
                    self.report_recursive_newtype_edge(current, span);
                    None
                }
                Some(next) => self
                    .expand_alias_fully_for_class_inner(next, span, true, seen),
                None => None,
            },
            None => expanded.then_some(current),
        }
    }
    fn expand_alias_once_for_class(
        &mut self,
        ty: TyId,
        span: Span,
    ) -> Option<TyId> {
        let (type_id, args) = self.alias_parts(ty)?;
        match self.registry.get_def(type_id) {
            Some(TypeDef::Alias { .. }) => {
                let target = self.decls.alias_target(type_id);
                let is_obj = self
                    .ast
                    .get_type_expr(target)
                    .is_some_and(|te| matches!(te, AstTypeExpr::Object(_)));
                if is_obj {
                    None
                } else {
                    let repr = self.alias_repr(type_id, &args);
                    self.newtype_edge(ty, repr, span)
                        .filter(|edge| edge.alias == type_id)
                        .map(|edge| edge.repr)
                }
            }
            _ => None,
        }
    }
}
