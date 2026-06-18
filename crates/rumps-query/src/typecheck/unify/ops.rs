use super::*;

impl SolveCtx<'_> {
    /// Check that a callee type is callable and unify with expected signature.
    pub(super) fn check_callable(
        &mut self,
        callee: TyId,
        args: &[TyId],
        ret: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(callee).clone();
        match shape {
            Ty::Fn(ref params, fn_ret) => {
                let params: SmallVec<[TyId; 4]> = params.clone();
                if args.len() > params.len() {
                    self.errors.push(TypeError::TooManyArguments {
                        expected: params.len(),
                        got: args.len(),
                        span,
                    });
                } else if args.is_empty() && !params.is_empty() {
                    self.errors.push(TypeError::ZeroArguments {
                        expected: params.len(),
                        span,
                    });
                } else if args.len() < params.len() {
                    // Partial application: unify supplied args with prefix
                    params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                        if let Err(e) = self.unify_types(a, p, span) {
                            self.errors.push(e);
                        }
                    });

                    // Residual function type from remaining params
                    let remaining: SmallVec<[TyId; 4]> =
                        params.iter().skip(args.len()).copied().collect();
                    let residual = self.ty_arena.func(remaining, fn_ret);
                    if let Err(e) = self.unify_types(residual, ret, span) {
                        self.errors.push(e);
                    }
                } else {
                    // Full application
                    params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                        if let Err(e) = self.unify_types(a, p, span) {
                            self.errors.push(e);
                        }
                    });

                    if let Err(e) = self.unify_types(fn_ret, ret, span) {
                        self.errors.push(e);
                    }
                }
            }

            Ty::Var(v) => {
                // Callee is unresolved; create function type and bind
                let fn_ty =
                    self.ty_arena.func(args.iter().copied().collect(), ret);
                if let Err(e) = self.unify_var(v, fn_ty, span) {
                    self.errors.push(e);
                }
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.errors.push(TypeError::NotCallable(callee, span));
            }
        }
    }

    /// Check that a type has a specific field.
    ///
    /// Looks up the field in the resolved base type and unifies the expected
    /// field type with the actual field type. Unlike `unify_named_with_object`,
    /// this only checks the single accessed field, not all object fields.
    pub(super) fn check_has_field(
        &mut self,
        base: TyId,
        field: StringId,
        field_ty: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(base).clone();
        match shape {
            // Structural object: look up field directly
            Ty::Object(ref fields) => match fields.get(&field) {
                Some(&actual_ty) => {
                    if let Err(e) = self.unify_types(field_ty, actual_ty, span)
                    {
                        self.errors.push(e);
                    }
                }
                None => {
                    let name = self
                        .env
                        .get_str(field)
                        .unwrap_or("<unknown>")
                        .to_string();
                    self.errors.push(TypeError::FieldNotFound {
                        ty: base,
                        field: name,
                        span,
                    });
                }
            },

            // Named alias to object: look up field in alias definition
            Ty::Named(type_id, ref type_args) => {
                let type_args: SmallVec<[TyId; 4]> = type_args.clone();
                let def = self.registry.get_def(type_id);
                match def {
                    Some(TypeDef::Alias { type_params, .. }) => {
                        if !self.convert_ctx().can_access_alias_repr(type_id) {
                            self.errors
                                .push(TypeError::NotAnObject(base, span));
                        } else {
                            let target = self.decls.alias_target(type_id);
                            let params: SmallVec<[StringId; 2]> =
                                type_params.clone();
                            match self.ast.get_type_expr(target).cloned() {
                                Some(AstTypeExpr::Object(alias_fields)) => {
                                    let field_str =
                                        self.env.get_str(field).unwrap_or("");
                                    let field_ty_id = alias_fields
                                        .iter()
                                        .find(|(n, _)| *n == field)
                                        .map(|(_, ty)| *ty);
                                    match field_ty_id {
                                        Some(ast_ty_id) => {
                                            let param_subst: IndexMap<_, _> =
                                                params
                                                    .iter()
                                                    .zip(type_args.iter())
                                                    .map(|(p, &a)| (*p, a))
                                                    .collect();
                                            let actual_ty = self
                                                .convert_ctx()
                                                .ast_type_to_ty(
                                                    ast_ty_id,
                                                    &param_subst,
                                                );
                                            if let Err(e) = self.unify_types(
                                                field_ty, actual_ty, span,
                                            ) {
                                                self.errors.push(e);
                                            }
                                        }
                                        None => {
                                            self.errors.push(
                                                TypeError::FieldNotFound {
                                                    ty: base,
                                                    field: field_str
                                                        .to_string(),
                                                    span,
                                                },
                                            );
                                        }
                                    }
                                }
                                _ => {
                                    self.errors.push(TypeError::NotAnObject(
                                        base, span,
                                    ));
                                }
                            }
                        }
                    }
                    _ => {
                        self.errors.push(TypeError::NotAnObject(base, span));
                    }
                }
            }

            // `Json`: any field access is valid and returns `Json`
            Ty::Json => {
                if let Err(e) = self.unify_types(field_ty, TyArena::JSON, span)
                {
                    self.errors.push(e);
                }
            }

            // Union: all members must have the field with compatible types
            Ty::Union(_, ref members) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter().for_each(|&m| {
                    self.check_has_field(m, field, field_ty, span);
                });
            }

            // Type variable: defer until resolved
            Ty::Var(_) => {
                // Type variable not yet resolved; constraint will be checked
                // when the variable is bound.
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.errors.push(TypeError::NotAnObject(base, span));
            }
        }
    }
}
