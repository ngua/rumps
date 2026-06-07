use super::*;

impl SolveCtx<'_> {
    /// `Into(target)`: `as` casts.
    pub(super) fn check_into(&mut self, ty: TyId, to: TyId, span: Span) {
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
                    // Functions cannot be stringified
                    (Ty::Fn(_, _), Ty::String) => {
                        self.errors.push(TypeError::InvalidCast {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Union(_, members), Ty::String) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, to),
                                *m,
                                span,
                            )
                        });
                    }
                    (_, Ty::String) => {}

                    // Functions, regex, refs cannot be Json-serialized
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
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *m,
                                span,
                            )
                        });
                    }
                    (Ty::Named(_, args), Ty::Json) => {
                        let as_: SmallVec<[TyId; 4]> = args.clone();
                        as_.iter().for_each(|a| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *a,
                                span,
                            )
                        });
                    }
                    (_, Ty::Json) => {}

                    // Numeric coercions
                    (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {}
                    (Ty::Word, Ty::Int) | (Ty::Word, Ty::Float) => {}
                    (Ty::Bool, Ty::Int) | (Ty::Int, Ty::Bool) => {}

                    // Special conversions
                    (Ty::DataStatus, Ty::Int) => {}
                    (Ty::String, Ty::FilePath) => {}
                    (Ty::Path, Ty::FilePath) => {}
                    (Ty::Named(id, _), Ty::FilePath) if *id == TypeId::PATH => {
                    }
                    (Ty::Range, Ty::Array(elem)) if *elem == TyArena::INT => {}

                    // `Storable` to member type
                    (Ty::Union(Some(id), _), _) if *id == TypeId::STORABLE => {
                        if !TyArena::STORABLE_MEMBERS.contains(&to) {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }

                    // Member to union type
                    (_, Ty::Union(Some(id), _))
                        if *id == TypeId::STORABLE
                            || *id == TypeId::SCALAR
                            || *id == TypeId::SUBSCRIPT =>
                    {
                        let uid = *id;
                        let is_member = if uid == TypeId::STORABLE {
                            TyArena::STORABLE_MEMBERS.contains(&ty)
                        } else if uid == TypeId::SCALAR {
                            TyArena::SCALAR_MEMBERS.contains(&ty)
                        } else {
                            TyArena::SUBSCRIPT_MEMBERS.contains(&ty)
                        };
                        if !is_member {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }

                    // Union handling
                    (Ty::Union(prov, members), _) => {
                        let inst =
                            prov.and_then(|id| self.find_into_instance(id, to));
                        match inst {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                let ms: SmallVec<[TyId; 4]> = members.clone();
                                ms.iter().for_each(|m| {
                                    self.satisfies_class(
                                        &TypeClass::param(ClassId::INTO, to),
                                        *m,
                                        span,
                                    )
                                });
                            }
                        }
                    }

                    // User type with `Into` instance
                    (Ty::Named(id, type_args), _) => {
                        let (id, type_args) = (*id, type_args.clone());
                        match self.find_into_instance(id, to) {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &type_args, span, None,
                                );
                            }
                            None => {
                                self.errors.push(TypeError::InvalidCast {
                                    from: ty,
                                    to,
                                    span,
                                });
                            }
                        }
                    }

                    // Builtin type with user-defined `Into[UserType]` instance.
                    // E.g., `class Into[UserId] FOR Int { ... }`.
                    _ => {
                        let type_id =
                            Self::primitive_type_id(self.ty_arena.get(ty));
                        match type_id
                            .and_then(|id| self.find_into_instance(id, to))
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                self.errors.push(TypeError::InvalidCast {
                                    from: ty,
                                    to,
                                    span,
                                });
                            }
                        }
                    }
                },
            }
        }
    }
    /// `TryInto(target)`: `read` casts.
    pub(super) fn check_try_into(&mut self, ty: TyId, to: TyId, span: Span) {
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
                    if !self.check_try_into_instance(ty, to, span) {
                        self.errors.push(
                            TypeError::NewtypeReprReadRequiresTryInto {
                                from: ty,
                                to,
                                span,
                            },
                        );
                    }
                }
                NewtypeEdgeStatus::Missing => match (&ty_shape, &to_shape) {
                    // Function types cannot be source for `READ`
                    (Ty::Fn(_, _), _) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }

                    // Cannot `READ` into function, regex, or refs
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

                    // `READ Json` requires source to be `Into[Json]`
                    (Ty::Regex, Ty::Json)
                    | (Ty::Local, Ty::Json)
                    | (Ty::Global, Ty::Json) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    _ if self.check_try_into_instance(ty, to, span) => {}
                    (Ty::Array(elem), Ty::Range) if *elem == TyArena::INT => {}
                    (Ty::Array(elem), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                        *elem,
                        span,
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                        *inner,
                        span,
                    ),
                    (Ty::Result(ok, err), Ty::Json) => {
                        let (ok, err) = (*ok, *err);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            ok,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            err,
                            span,
                        );
                    }
                    (Ty::Map(k, v), Ty::Json) => {
                        let (k, v) = (*k, *v);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            k,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            v,
                            span,
                        );
                    }
                    (Ty::Tuple(elems), Ty::Json) => {
                        let es: SmallVec<[TyId; 4]> = elems.clone();
                        es.iter().for_each(|e| {
                            self.satisfies_class(
                                &TypeClass::param(
                                    ClassId::TRY_INTO,
                                    TyArena::JSON,
                                ),
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
                                &TypeClass::param(
                                    ClassId::TRY_INTO,
                                    TyArena::JSON,
                                ),
                                *t,
                                span,
                            )
                        });
                    }
                    (Ty::Named(_, args), Ty::Json) => {
                        let as_: SmallVec<[TyId; 4]> = args.clone();
                        as_.iter().for_each(|a| {
                            self.satisfies_class(
                                &TypeClass::param(
                                    ClassId::TRY_INTO,
                                    TyArena::JSON,
                                ),
                                *a,
                                span,
                            )
                        });
                    }

                    // Union handling
                    (Ty::Union(prov, members), _) => {
                        let has_inst = prov
                            .and_then(|_| self.find_try_into_instance(ty, to))
                            .is_some();
                        if has_inst {
                            self.check_try_into_instance(ty, to, span);
                        } else {
                            let ms: SmallVec<[TyId; 4]> = members.clone();
                            ms.iter().for_each(|m| {
                                self.satisfies_class(
                                    &TypeClass::param(ClassId::TRY_INTO, to),
                                    *m,
                                    span,
                                )
                            });
                        }
                    }

                    // User type with `TryInto` instance
                    (Ty::Named(id, type_args), _) => {
                        let (id, type_args) = (*id, type_args.clone());
                        let inst = self
                            .find_try_into_instance_for(id, &type_args, to)
                            .map(|inst| (inst, type_args));
                        if let Some((inst, type_args)) = inst {
                            self.check_instance_constraints(
                                &inst, &type_args, span, None,
                            );
                        }
                    }

                    // All other combinations are valid for `READ`
                    _ => {}
                },
            },
        };
    }
    /// `Indexable(elem)`: `Array[T]`, `Map[K,V]`, `String`.
    ///
    /// The index type is now accessed via the associated type `.Index`; only
    /// the element type is unified here.
    pub(super) fn check_indexable(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        elem: TyId,
        span: Span,
    ) {
        match self.ty_arena.get(ty).clone() {
            Ty::Array(inner) => {
                // `Array[T]`: `elem = T` (index type is `Int`, via `.Index`)
                if let Err(e) = self.unify_types(elem, inner, span) {
                    self.errors.push(e);
                }
            }
            Ty::Map(key, val) => {
                // `Map[K, V]`: `elem = V` (index type is `K`, via `.Index`)
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
                // `String`: `elem = Char` (index type is `Int`, via `.Index`)
                if let Err(e) = self.unify_types(elem, TyArena::CHAR, span) {
                    self.errors.push(e);
                }
            }
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|m| self.satisfies_class(class, *m, span));
            }
            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
            Ty::Named(id, type_args) => {
                match self.instance_for(
                    InstanceUse::Evidence,
                    ClassId::INDEXABLE,
                    id,
                    span,
                ) {
                    InstanceLookup::Found(inst) => {
                        let param_subst =
                            self.build_instance_subst(&inst, &type_args, span);
                        // `class_args[0]` is the element type
                        let inst_elem =
                            inst.class_args.first().copied().unwrap_or_else(
                                || {
                                    invariant!(
                                        "`Indexable` instance has a class arg"
                                    )
                                },
                            );
                        let resolved =
                            self.ty_arena.apply(inst_elem, &param_subst);
                        if let Err(e) = self.unify_types(elem, resolved, span) {
                            self.errors.push(e);
                        }
                        self.check_instance_constraints(
                            &inst,
                            &type_args,
                            span,
                            Some(&param_subst),
                        );
                    }
                    InstanceLookup::Missing => {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                    InstanceLookup::NotImported => {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                    InstanceLookup::BlockedSelf => {}
                }
            }
            _ => {
                self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                ));
            }
        }
    }
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

            // Json: any field access is valid and returns Json
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
                // when the variable is bound. For now, this is allowed.
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.errors.push(TypeError::NotAnObject(base, span));
            }
        }
    }
}
