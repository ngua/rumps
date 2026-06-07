use super::*;

impl SolveCtx<'_> {
    /// Resolve an associated type projection to a concrete type.
    ///
    /// Given a base type (`Array[Int]`, `Map[String, Int]`, etc.) and a class
    /// with an associated type (`Indexable:Index`), returns the concrete type
    /// that the associated type resolves to.
    ///
    /// # Builtin Rules
    ///
    /// - `Array[T]` with `Indexable:Index` -> `Int`
    /// - `Map[K, V]` with `Indexable:Index` -> `K`
    /// - `String` with `Indexable:Index` -> `Int`
    ///
    /// # User Types
    ///
    /// For user instances, associated type definitions are interpreted in the
    /// instance type parameter scope. For `class C for Box[K, V] { newtype A = K }`,
    /// projecting `C:A` from `Box[String, Int]` resolves to `String`.
    ///
    pub(super) fn resolve_assoc_type(
        &mut self,
        base: TyId,
        class: ClassId,
        assoc_name: StringId,
        span: Span,
    ) -> Result<TyId, TypeError> {
        // Validate that assoc_name is a valid associated type for this class
        let assoc_types = &self.env.class_def(class).assoc_types;
        if !assoc_types.contains(&assoc_name) {
            Err(TypeError::NoSuchAssocType {
                class,
                name: assoc_name,
                span,
            })
        } else {
            let shape = self.ty_arena.get(base).clone();
            match shape {
                // Builtin: Array[T] with Indexable:Index = Int
                Ty::Array(_) if class == ClassId::INDEXABLE => Ok(TyArena::INT),

                // Builtin: Map[K, V] with Indexable:Index = K
                Ty::Map(k, _) if class == ClassId::INDEXABLE => Ok(k),

                // Builtin: String with Indexable:Index = Int
                Ty::String if class == ClassId::INDEXABLE => Ok(TyArena::INT),

                // User type: look up instance in registry
                Ty::Named(type_id, ref type_args) => {
                    let type_args: SmallVec<[TyId; 4]> = type_args.clone();
                    match self.instance_registry.lookup(class, type_id).cloned()
                    {
                        Some(inst) => {
                            let param_rename = self
                                .build_instance_subst(&inst, &type_args, span);
                            // Find the associated type definition
                            match inst.get_assoc_type(assoc_name) {
                                Some(assoc_def) => {
                                    let assoc_ty = assoc_def.ty;
                                    Ok(self
                                        .ty_arena
                                        .apply(assoc_ty, &param_rename))
                                }
                                None => Err(TypeError::MissingAssocType {
                                    class,
                                    assoc: assoc_name,
                                    span,
                                }),
                            }
                        }
                        None => Err(TypeError::UnsatisfiedClass(
                            TypeClass::placeholder(
                                class,
                                self.env.class_def(class).shape,
                            ),
                            base,
                            span,
                        )),
                    }
                }

                // Type variable: cannot resolve yet (defer resolution)
                Ty::Var(_) => Err(TypeError::UnknownAssocType {
                    ty: base,
                    assoc: assoc_name,
                    span,
                }),

                // Error/Unknown: propagate
                Ty::Error | Ty::Unknown => Ok(TyArena::ERROR),

                // User classes: handle parameterized builtins via instance lookup
                _ if class.idx() >= ClassId::BUILTIN_COUNT => {
                    match self.ty_to_type_id_and_args(base) {
                        Some((tid, type_args)) => {
                            match self
                                .instance_registry
                                .lookup(class, tid)
                                .cloned()
                            {
                                Some(inst) => {
                                    let param_rename = self
                                        .build_instance_subst(
                                            &inst, &type_args, span,
                                        );
                                    match inst.get_assoc_type(assoc_name) {
                                        Some(assoc_def) => {
                                            Ok(self.ty_arena.apply(
                                                assoc_def.ty,
                                                &param_rename,
                                            ))
                                        }
                                        None => {
                                            Err(TypeError::MissingAssocType {
                                                class,
                                                assoc: assoc_name,
                                                span,
                                            })
                                        }
                                    }
                                }
                                None => Err(TypeError::UnsatisfiedClass(
                                    TypeClass::placeholder(
                                        class,
                                        self.env.class_def(class).shape,
                                    ),
                                    base,
                                    span,
                                )),
                            }
                        }
                        None => Err(TypeError::UnsatisfiedClass(
                            TypeClass::placeholder(
                                class,
                                self.env.class_def(class).shape,
                            ),
                            base,
                            span,
                        )),
                    }
                }

                // Other types: no instance for this class
                _ => Err(TypeError::UnsatisfiedClass(
                    TypeClass::placeholder(
                        class,
                        self.env.class_def(class).shape,
                    ),
                    base,
                    span,
                )),
            }
        }
    }
}
