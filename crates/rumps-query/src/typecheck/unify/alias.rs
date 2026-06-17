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
        class: ClassId,
        ty: TyId,
        span: Span,
    ) -> Option<TyId> {
        self.expand_alias_fully_for_type_class(
            &TypeClass::simple(class),
            ty,
            span,
        )
    }

    pub(super) fn expand_alias_fully_for_type_class(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) -> Option<TyId> {
        self.expand_alias_fully_for_class_inner(
            class,
            ty,
            span,
            false,
            &mut HashSet::new(),
        )
    }

    fn expand_alias_fully_for_class_inner(
        &mut self,
        class: &TypeClass<TyId>,
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
            Some(_) => {
                match self.expand_alias_once_for_class(class, current, span) {
                    Some(next) if next == current => {
                        self.report_recursive_newtype_edge(current, span);
                        None
                    }
                    Some(next) => self.expand_alias_fully_for_class_inner(
                        class, next, span, true, seen,
                    ),
                    None => None,
                }
            }
            None => expanded.then_some(current),
        }
    }

    fn expand_alias_once_for_class(
        &mut self,
        class: &TypeClass<TyId>,
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
                    if self.can_use_newtype_repr(class, type_id, &args) {
                        self.newtype_edge(ty, repr, span)
                            .filter(|edge| edge.alias == type_id)
                            .map(|edge| edge.repr)
                    } else {
                        None
                    }
                }
            }
            _ => None,
        }
    }

    pub(super) fn newtype_repr_for_assoc(
        &mut self,
        class: ClassId,
        ty: TyId,
        span: Span,
    ) -> Option<TyId> {
        let query =
            TypeClass::placeholder(class, self.env.class_def(class).shape);
        self.expand_alias_fully_for_class_inner(
            &query,
            ty,
            span,
            false,
            &mut HashSet::new(),
        )
    }

    fn can_use_newtype_repr(
        &mut self,
        class: &TypeClass<TyId>,
        alias: TypeId,
        args: &[TyId],
    ) -> bool {
        let id = class.tag();
        let has_inst = self.instance_registry.lookup(id, alias).is_some();
        let checks_self = self
            .class_context
            .as_ref()
            .is_some_and(|ctx| ctx.class == id && ctx.type_id == Some(alias));
        if has_inst || checks_self || id.idx() >= ClassId::BUILTIN_COUNT {
            false
        } else {
            let decls = self.decls;
            let reg = self.registry;
            let derived_tag =
                self.derived_instance_tag_matches(alias, id, class);
            derived_tag
                || decls.derived_instance_matches(
                    reg,
                    alias,
                    class,
                    args,
                    |te, sub| self.convert_ctx().ast_type_to_ty(te, sub),
                )
                || self.decls.is_transparent(alias)
        }
    }

    fn derived_instance_tag_matches(
        &self,
        alias: TypeId,
        id: ClassId,
        class: &TypeClass<TyId>,
    ) -> bool {
        let params = match class {
            TypeClass::Concrete { params, .. }
            | TypeClass::Hkt { params, .. } => params,
        };
        params.iter().all(|&p| p == TyArena::UNKNOWN)
            && self
                .decls
                .derived_instances(alias)
                .iter()
                .any(|d| d.class == id)
    }
}
