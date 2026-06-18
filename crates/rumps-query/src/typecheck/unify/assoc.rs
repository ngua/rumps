use super::evidence::{Evidence, EvidenceQuery};
use super::*;

impl SolveCtx<'_> {
    pub(super) fn resolve_assoc_type(
        &mut self,
        base: TyId,
        class: &TypeClass<TyId>,
        assoc_name: StringId,
        span: Span,
    ) -> Result<TyId, TypeError> {
        let id = match class {
            TypeClass::Concrete { id, .. } | TypeClass::Hkt { id, .. } => *id,
        };
        if !self.env.class_def(id).assoc_types.contains(&assoc_name) {
            Err(TypeError::NoSuchAssocType {
                class: id,
                name: assoc_name,
                span,
            })
        } else if self.assoc_blocks_self(base, id) {
            Ok(TyArena::ERROR)
        } else {
            match self.ty_arena.get(base).clone() {
                Ty::Array(_) if id == ClassId::INDEXABLE => Ok(TyArena::INT),
                Ty::Map(k, _) if id == ClassId::INDEXABLE => Ok(k),
                Ty::String if id == ClassId::INDEXABLE => Ok(TyArena::INT),
                Ty::Var(_) => Err(TypeError::UnknownAssocType {
                    ty: base,
                    assoc: assoc_name,
                    span,
                }),
                Ty::Error | Ty::Unknown => Ok(TyArena::ERROR),
                _ => {
                    let ev = self.evidence(EvidenceQuery {
                        class,
                        ty: base,
                        span,
                    });
                    match ev {
                        Evidence::Manual { inst, args } => self
                            .resolve_assoc_from_inst(
                                &inst, &args, id, assoc_name, span,
                            ),
                        Evidence::ManualMany { insts, args } => {
                            match insts.first() {
                                Some(inst) => self.resolve_assoc_from_inst(
                                    inst, &args, id, assoc_name, span,
                                ),
                                None => {
                                    invariant!(
                                        "`ManualMany` has at least one instance"
                                    )
                                }
                            }
                        }
                        Evidence::Repr { ty: repr, .. } => self
                            .resolve_assoc_type(repr, class, assoc_name, span),
                        Evidence::BlockedSelf | Evidence::NotImported => {
                            Ok(TyArena::ERROR)
                        }
                        Evidence::Missing
                        | Evidence::Union { .. }
                        | Evidence::DerivedVariant { .. }
                        | Evidence::BuiltinNamed => {
                            Err(TypeError::UnsatisfiedClass(
                                class.clone(),
                                base,
                                span,
                            ))
                        }
                    }
                }
            }
        }
    }

    fn assoc_blocks_self(&self, base: TyId, class: ClassId) -> bool {
        self.ty_to_type_id_and_args(base).is_some_and(|(tid, _)| {
            self.class_context.as_ref().is_some_and(|ctx| {
                ctx.class == class && ctx.type_id == Some(tid)
            })
        })
    }

    fn resolve_assoc_from_inst(
        &mut self,
        inst: &Instance,
        args: &[TyId],
        class: ClassId,
        assoc: StringId,
        span: Span,
    ) -> Result<TyId, TypeError> {
        let sub = self.build_instance_subst(inst, args, span);
        match inst.get_assoc_type(assoc) {
            Some(def) => Ok(self.ty_arena.apply(def.ty, &sub)),
            None => Err(TypeError::MissingAssocType { class, assoc, span }),
        }
    }

    pub(super) fn resolve_assoc_type_by_id(
        &mut self,
        base: TyId,
        class: ClassId,
        assoc_name: StringId,
        span: Span,
    ) -> Result<TyId, TypeError> {
        // Stored projections carry only a `ClassId`; use a placeholder query
        // when no class arguments are available.
        let class =
            TypeClass::placeholder(class, self.env.class_def(class).shape);
        self.resolve_assoc_type(base, &class, assoc_name, span)
    }
}
