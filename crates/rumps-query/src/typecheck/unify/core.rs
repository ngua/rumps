use super::*;

struct UnionBt<'a> {
    rhs: &'a [TyId],
    avail: Vec<usize>,
    span: Span,
}

impl SolveCtx<'_> {
    /// Unify two types, recording bindings in the union-find.
    ///
    /// # Unification Rules
    ///
    /// 1. `Var(v) ~ t` binds `v` to `t` (if `v` not in `fv(t)`; occurs check)
    /// 2. `t ~ Var(v)` binds `v` to `t` (symmetric)
    /// 3. `Array[a] ~ Array[b]` recurses into `unify(a, b)`
    /// 4. `Fn[p1] -> r1 ~ Fn[p2] -> r2` recurses into params and return types
    /// 5. `{ f1 } ~ { f2 }` unifies common fields (structural objects)
    /// 6. `Named(id, args1) ~ Named(id, args2)` unifies corresponding args
    /// 7. `Unknown ~ _` or `_ ~ Unknown` succeeds (unifies with anything)
    /// 8. `Error ~ _` or `_ ~ Error` succeeds (error recovery)
    /// 9. `T ~ T` succeeds (primitives equal)
    /// 10. Otherwise: error
    ///
    /// # No Implicit Numeric Coercion
    ///
    /// Numeric types (`Int`, `Float`, `Word`) do NOT implicitly coerce.
    /// Use explicit `AS` casts to convert between them.
    pub(super) fn unify_types(
        &mut self,
        t1: TyId,
        t2: TyId,
        span: Span,
    ) -> UnifyResult {
        self.unify_inner(t1, t2, span)
    }

    /// Core unification logic.
    fn unify_inner(&mut self, t1: TyId, t2: TyId, span: Span) -> UnifyResult {
        // Equal TyIds are trivially unified
        if t1 == t2 {
            Ok(())
        } else {
            // Expand aliases through approved `newtype` representation edges.
            let t1 = self.expand_alias_fully(t1, t2, span).unwrap_or(t1);
            let t2 = self.expand_alias_fully(t2, t1, span).unwrap_or(t2);

            if t1 == t2 {
                Ok(())
            } else {
                // Clone both `Ty`s to release the arena borrow.
                let ty1 = self.ty_arena.get(t1).clone();
                let ty2 = self.ty_arena.get(t2).clone();
                self.unify_inner_dispatch(t1, t2, &ty1, &ty2, span)
            }
        }
    }

    /// Dispatch on cloned `Ty` pair; `t1`/`t2` are the original `TyId`s for
    /// error messages, `ty1`/`ty2` are the cloned shapes for matching.
    fn unify_inner_dispatch(
        &mut self,
        t1: TyId,
        t2: TyId,
        ty1: &Ty,
        ty2: &Ty,
        span: Span,
    ) -> UnifyResult {
        match (ty1, ty2) {
            // Error recovery: Error unifies with anything
            (Ty::Error, _) | (_, Ty::Error) => Ok(()),

            // Unknown unifies with anything (database reads before narrowing)
            (Ty::Unknown, _) | (_, Ty::Unknown) => Ok(()),

            // Type variable on left: bind it
            (Ty::Var(v), _) => self.unify_var(*v, t2, span),

            // Type variable on right: symmetric
            (_, Ty::Var(v)) => self.unify_var(*v, t1, span),

            // Identical primitives (handled by TyId equality above for
            // pre-interned constants, but needed for dynamically allocated
            // duplicates)
            (Ty::Bool, Ty::Bool)
            | (Ty::Unit, Ty::Unit)
            | (Ty::Char, Ty::Char)
            | (Ty::String, Ty::String)
            | (Ty::Time, Ty::Time)
            | (Ty::Range, Ty::Range)
            | (Ty::Json, Ty::Json)
            | (Ty::Ordering, Ty::Ordering)
            | (Ty::DataStatus, Ty::DataStatus)
            | (Ty::FilePath, Ty::FilePath)
            | (Ty::Path, Ty::Path)
            | (Ty::Regex, Ty::Regex)
            | (Ty::RuntimeError, Ty::RuntimeError) => Ok(()),

            // Local and Global are distinct types; use Ref union for either
            (Ty::Local, Ty::Local) | (Ty::Global, Ty::Global) => Ok(()),

            // Numeric types: same type only (no implicit coercion)
            (Ty::Int, Ty::Int)
            | (Ty::Word, Ty::Word)
            | (Ty::Float, Ty::Float) => Ok(()),

            // A `newtype` edge exists, but private `repr visibility` blocks
            // this external annotation or unification site.
            _ if self.newtype_edge_blocked(t1, t2, span) => {
                Err(TypeError::PrivateReprAnnotation {
                    from: t1,
                    to: t2,
                    span,
                })
            }

            // Array: unify element types
            (Ty::Array(a), Ty::Array(b)) => self.unify_inner(*a, *b, span),

            // Option: unify inner types
            (Ty::Option(a), Ty::Option(b)) => self.unify_inner(*a, *b, span),

            // Result: unify both ok and err types
            (Ty::Result(ok1, err1), Ty::Result(ok2, err2)) => {
                let (ok1, err1, ok2, err2) = (*ok1, *err1, *ok2, *err2);
                self.unify_inner(ok1, ok2, span)?;
                self.unify_inner(err1, err2, span)
            }

            // Map: unify key and value types
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                let (k1, v1, k2, v2) = (*k1, *v1, *k2, *v2);
                self.unify_inner(k1, k2, span)?;
                self.unify_inner(v1, v2, span)
            }

            // Tuple: unify element-wise (must have same length)
            (Ty::Tuple(ts1), Ty::Tuple(ts2)) => {
                if ts1.len() != ts2.len() {
                    Err(TypeError::Mismatch {
                        expected: t2,
                        got: t1,
                        span,
                    })
                } else {
                    let v1: SmallVec<[TyId; 4]> = ts1.clone();
                    let v2: SmallVec<[TyId; 4]> = ts2.clone();
                    self.unify_sequence(
                        v1.iter().copied(),
                        v2.iter().copied(),
                        span,
                    )
                }
            }

            // Function: unify params and return type
            (Ty::Fn(params1, ret1), Ty::Fn(params2, ret2)) => {
                if params1.len() != params2.len() {
                    Err(TypeError::ArityMismatch {
                        expected: params1.len(),
                        got: params2.len(),
                        span,
                    })
                } else {
                    let p1: SmallVec<[TyId; 4]> = params1.clone();
                    let p2: SmallVec<[TyId; 4]> = params2.clone();
                    let (r1, r2) = (*ret1, *ret2);
                    self.unify_sequence(
                        p1.iter().copied(),
                        p2.iter().copied(),
                        span,
                    )?;
                    self.unify_inner(r1, r2, span)
                }
            }

            // Structural objects: unify common fields
            (Ty::Object(fields1), Ty::Object(fields2)) => {
                let f1 = fields1.clone();
                let f2 = fields2.clone();
                self.unify_objects(&f1, &f2, span)
            }

            // Named type with structural object (extensible record check)
            (Ty::Named(id, args), Ty::Object(obj_fields))
            | (Ty::Object(obj_fields), Ty::Named(id, args)) => {
                let id = *id;
                let args: SmallVec<[TyId; 4]> = args.clone();
                let obj = obj_fields.clone();
                self.unify_named_with_object(id, &args, &obj, span)
            }

            // Named types: same TypeId, unify type arguments
            (Ty::Named(id1, args1), Ty::Named(id2, args2)) => {
                if id1 != id2 || args1.len() != args2.len() {
                    Err(TypeError::Mismatch {
                        expected: t2,
                        got: t1,
                        span,
                    })
                } else {
                    let a1: SmallVec<[TyId; 4]> = args1.clone();
                    let a2: SmallVec<[TyId; 4]> = args2.clone();
                    self.unify_sequence(
                        a1.iter().copied(),
                        a2.iter().copied(),
                        span,
                    )
                }
            }

            // HKT application against a union must keep the union as the
            // constructor. The generic union matcher below would pick one
            // member and collapse `Flex` to `Option`/`Array`.
            (Ty::Apply(tv, args), Ty::Union(..)) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t2, span)
            }
            (Ty::Union(..), Ty::Apply(tv, args)) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t1, span)
            }

            // Union types: structural equality (same members, order-independent)
            (Ty::Union(_, members1), Ty::Union(_, members2)) => {
                if members1.len() != members2.len() {
                    Err(TypeError::Mismatch {
                        expected: t2,
                        got: t1,
                        span,
                    })
                } else {
                    let m1: SmallVec<[TyId; 4]> = members1.clone();
                    let m2: SmallVec<[TyId; 4]> = members2.clone();
                    // Find a bijective matching between union members
                    let bt = UnionBt {
                        rhs: &m2,
                        avail: (0..m2.len()).collect(),
                        span,
                    };
                    self.unify_union_bijection(&m1, bt).unwrap_or({
                        Err(TypeError::Mismatch {
                            expected: t1,
                            got: t2,
                            span,
                        })
                    })
                }
            }

            // Concrete type with union: T unifies if it matches any member
            (_, Ty::Union(_, members)) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter()
                    .find_map(|&m| {
                        let snap = self.uf.snapshot();
                        match self.unify_inner(t1, m, span) {
                            ok @ Ok(()) => Some(ok),
                            _ => {
                                self.uf.rollback(snap);
                                None
                            }
                        }
                    })
                    .unwrap_or({
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    })
            }
            (Ty::Union(_, members), _) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter()
                    .find_map(|&m| {
                        let snap = self.uf.snapshot();
                        match self.unify_inner(m, t2, span) {
                            ok @ Ok(()) => Some(ok),
                            _ => {
                                self.uf.rollback(snap);
                                None
                            }
                        }
                    })
                    .unwrap_or({
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    })
            }

            // HKT type application: `F[T]` where `F` is a type variable.
            //
            // Decompose the other type into constructor + element,
            // bind the type variable to the constructor shape, and
            // unify args with the element types.
            //
            // Must appear before the `Named` catch-all so that
            // `(Apply, Named)` is decomposed rather than rejected.
            (Ty::Apply(tv, args), _) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t2, span)
            }
            (_, Ty::Apply(tv, args)) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t1, span)
            }

            // Named (sum/alias) vs anything else: mismatch.
            // (Unions are `Ty::Union` and handled above.)
            (_, Ty::Named(..)) | (Ty::Named(..), _) => {
                Err(TypeError::Mismatch {
                    expected: t2,
                    got: t1,
                    span,
                })
            }

            // Associated type projection: resolve and unify
            (Ty::AssocType(tv, class, name), _) => {
                let (tv, class, name) = (*tv, *class, *name);
                let base = self.ty_arena.alloc(Ty::Var(tv));
                match self.resolve_assoc_type(base, class, name, span) {
                    Ok(resolved) => self.unify_inner(resolved, t2, span),
                    Err(_) => {
                        // Base type is unresolved (type variable); defer
                        Ok(())
                    }
                }
            }
            (_, Ty::AssocType(tv, class, name)) => {
                let (tv, class, name) = (*tv, *class, *name);
                let base = self.ty_arena.alloc(Ty::Var(tv));
                match self.resolve_assoc_type(base, class, name, span) {
                    Ok(resolved) => self.unify_inner(t1, resolved, span),
                    Err(_) => Ok(()),
                }
            }

            // All other combinations are type mismatches
            _ => Err(TypeError::Mismatch {
                expected: t2,
                got: t1,
                span,
            }),
        }
    }

    /// Unify a type variable with a type.
    ///
    /// Finds the canonical root via UF, probes for existing binding, and
    /// either follows through to the bound type, unions two vars, or binds
    /// the root. Uses UF-aware occurs check.
    pub(super) fn unify_var(
        &mut self,
        v: TyVar,
        t: TyId,
        span: Span,
    ) -> UnifyResult {
        let root = self.uf.find(v);
        if let Some(bound) = self.uf.probe(root) {
            // Already bound; unify the bound type with `t`
            self.unify_inner(bound, t, span)
        } else if let Ty::Var(w) = self.ty_arena.get(t) {
            let w_root = self.uf.find(*w);
            if root == w_root {
                Ok(())
            } else if let Some(w_bound) = self.uf.probe(w_root) {
                // `w` is bound; unify root with the bound type
                if self.ty_arena.occurs_uf(w_bound, root, self.uf) {
                    Err(TypeError::InfiniteType(root, w_bound, span))
                } else {
                    self.uf.bind(root, w_bound);
                    Ok(())
                }
            } else {
                self.uf.union(root, w_root);
                Ok(())
            }
        } else if self.ty_arena.occurs_uf(t, root, self.uf) {
            Err(TypeError::InfiniteType(root, t, span))
        } else {
            self.uf.bind(root, t);
            Ok(())
        }
    }

    /// Unify two sequences of types element-wise.
    pub(super) fn unify_sequence(
        &mut self,
        ts1: impl Iterator<Item = TyId>,
        ts2: impl Iterator<Item = TyId>,
        span: Span,
    ) -> UnifyResult {
        ts1.zip(ts2)
            .try_for_each(|(t1, t2)| self.unify_inner(t1, t2, span))
    }

    /// Find a bijective matching between union members via backtracking.
    ///
    /// Tries to match each member of `lhs` to a unique member of `bt.rhs`
    /// using indices in `bt.avail`. Uses `UF` snapshot/rollback for
    /// backtracking instead of cloning substitutions.
    fn unify_union_bijection(
        &mut self,
        lhs: &[TyId],
        bt: UnionBt<'_>,
    ) -> Option<UnifyResult> {
        match lhs.split_first() {
            None => Some(Ok(())),
            Some((&first, rest)) => {
                // Try each available index, backtracking on failure
                self.try_union_matches(first, rest, bt, 0)
            }
        }
    }

    /// Helper for `unify_union_bijection`: try matching `first` with each
    /// available member starting at `start`.
    fn try_union_matches(
        &mut self,
        first: TyId,
        rest: &[TyId],
        bt: UnionBt<'_>,
        start: usize,
    ) -> Option<UnifyResult> {
        match bt.avail.get(start).copied() {
            Some(idx) => {
                let m2 = *bt.rhs.get(idx)?;
                let snap = self.uf.snapshot();

                match self.unify_inner(first, m2, bt.span) {
                    Ok(()) => {
                        let next = UnionBt {
                            rhs: bt.rhs,
                            avail: bt
                                .avail
                                .iter()
                                .copied()
                                .filter(|&i| i != idx)
                                .collect(),
                            span: bt.span,
                        };

                        match self.unify_union_bijection(rest, next) {
                            Some(Ok(())) => Some(Ok(())),
                            // Backtrack: rollback and try next
                            _ => {
                                self.uf.rollback(snap);
                                self.try_union_matches(
                                    first,
                                    rest,
                                    bt,
                                    start + 1,
                                )
                            }
                        }
                    }
                    // This match failed; rollback and try next
                    Err(_) => {
                        self.uf.rollback(snap);
                        self.try_union_matches(first, rest, bt, start + 1)
                    }
                }
            }
            None => None,
        }
    }

    /// Unify two structural object types.
    ///
    /// Uses extensible record semantics: an object matches if it has at least
    /// the required fields with matching types. Extra fields are allowed.
    fn unify_objects(
        &mut self,
        fields1: &IndexMap<StringId, TyId>,
        fields2: &IndexMap<StringId, TyId>,
        span: Span,
    ) -> UnifyResult {
        // Collect all field names from both objects
        let all_keys: Vec<StringId> = fields1
            .keys()
            .chain(fields2.keys())
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // Unify fields present in both; extra fields are allowed
        all_keys.iter().try_for_each(|key| {
            match (fields1.get(key), fields2.get(key)) {
                (Some(&t1), Some(&t2)) => self.unify_inner(t1, t2, span),
                // Field only in one object; extensible, so OK
                _ => Ok(()),
            }
        })
    }

    /// Unify a named object alias type with a structural object type.
    ///
    /// The alias must have all required fields present in the object.
    /// Extra fields in the object are allowed (extensible record semantics).
    fn unify_named_with_object(
        &mut self,
        type_id: TypeId,
        type_args: &[TyId],
        obj_fields: &IndexMap<StringId, TyId>,
        span: Span,
    ) -> UnifyResult {
        // Look up alias definition
        let def = self.registry.get_def(type_id);

        match def {
            Some(TypeDef::Alias { type_params, .. }) => {
                let target = self.decls.alias_target(type_id);
                // Check if target is an object type
                let type_params = type_params.clone();
                match self.ast.get_type_expr(target).cloned() {
                    Some(AstTypeExpr::Object(alias_fields)) => {
                        let named = self.ty_arena.named(
                            type_id,
                            type_args.iter().copied().collect(),
                        );
                        let obj =
                            self.ty_arena.alloc(Ty::Object(obj_fields.clone()));
                        if self.newtype_edge(named, obj, span).is_some() {
                            Ok(())
                        } else if !self
                            .convert_ctx()
                            .can_access_alias_repr(type_id)
                        {
                            Err(TypeError::Mismatch {
                                expected: named,
                                got: obj,
                                span,
                            })
                        } else {
                            // Build substitution from type params to type args
                            let param_subst: IndexMap<StringId, TyId> =
                                type_params
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(p, a)| (*p, *a))
                                    .collect();

                            // Pre-intern field names before the fold
                            let fields_with_ids: Vec<_> = alias_fields
                                .iter()
                                .map(|(name, ty)| (*name, *ty))
                                .collect();

                            // Check that object has all required fields
                            fields_with_ids.iter().try_for_each(
                                |(field_name, field_ty_id)| {
                                    let expected_ty =
                                        self.convert_ctx().ast_type_to_ty(
                                            *field_ty_id,
                                            &param_subst,
                                        );

                                    match obj_fields.get(field_name) {
                                        Some(&obj_ty) => self.unify_inner(
                                            expected_ty,
                                            obj_ty,
                                            span,
                                        ),
                                        None => {
                                            // Missing required field
                                            Err(TypeError::MissingField {
                                                ty: type_id,
                                                field: self.env.resolve_string(
                                                    *field_name,
                                                ),
                                                span,
                                            })
                                        }
                                    }
                                },
                            )
                        }
                    }
                    _ => {
                        // Not an object alias, can't unify with object
                        let named = self.ty_arena.named(
                            type_id,
                            type_args.iter().copied().collect(),
                        );
                        let obj =
                            self.ty_arena.alloc(Ty::Object(obj_fields.clone()));
                        Err(TypeError::Mismatch {
                            expected: named,
                            got: obj,
                            span,
                        })
                    }
                }
            }

            Some(TypeDef::Union { .. })
            | Some(TypeDef::Sum { .. })
            | Some(TypeDef::Builtin(_))
            | None => {
                let named = self
                    .ty_arena
                    .named(type_id, type_args.iter().copied().collect());
                let obj = self.ty_arena.alloc(Ty::Object(obj_fields.clone()));
                Err(TypeError::Mismatch {
                    expected: named,
                    got: obj,
                    span,
                })
            }
        }
    }
}
