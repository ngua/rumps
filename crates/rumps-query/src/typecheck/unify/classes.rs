use super::*;

enum Satisfaction {
    /// Type directly satisfies (no recursion needed).
    Direct,
    /// Recurse into inner types (for containers like `Array[T]`).
    Recurse(SmallVec<[TyId; 4]>),
    /// Check specific classes on inner types.
    Require(SmallVec<[(TyId, ClassId); 4]>),
}

impl SolveCtx<'_> {
    fn is_numeric_capability(id: ClassId) -> bool {
        matches!(
            id,
            ClassId::NUMERIC
                | ClassId::ADDITIVE
                | ClassId::SUBTRACTIVE
                | ClassId::MULTIPLICATIVE
                | ClassId::DIVISIBLE
                | ClassId::FLOOR_DIVISIBLE
                | ClassId::POWERABLE
        )
    }
    /// Map a primitive `Ty` shape to its `TypeId`, if applicable.
    pub(super) fn primitive_type_id(ty: &Ty) -> Option<TypeId> {
        match ty {
            Ty::Bool => Some(TypeId::BOOL),
            Ty::Int => Some(TypeId::INT),
            Ty::Word => Some(TypeId::WORD),
            Ty::Float => Some(TypeId::FLOAT),
            Ty::Char => Some(TypeId::CHAR),
            Ty::String => Some(TypeId::STRING),
            Ty::Unit => Some(TypeId::UNIT),
            Ty::Time => Some(TypeId::TIME),
            Ty::Range => Some(TypeId::RANGE),
            Ty::Json => Some(TypeId::JSON),
            Ty::Ordering => Some(TypeId::ORDERING),
            Ty::DataStatus => Some(TypeId::DATA_STATUS),
            Ty::FilePath => Some(TypeId::FILEPATH),
            Ty::Path => Some(TypeId::PATH),
            Ty::Regex => Some(TypeId::REGEX),
            Ty::Local => Some(TypeId::LOCAL),
            Ty::Global => Some(TypeId::GLOBAL),
            Ty::Array(_) => Some(TypeId::ARRAY),
            Ty::Option(_) => Some(TypeId::OPTION),
            Ty::Result(_, _) => Some(TypeId::RESULT),
            Ty::Map(_, _) => Some(TypeId::MAP),
            Ty::Tuple(_) => Some(TypeId::TUPLE),
            _ => None,
        }
    }
    /// Map a `Ty` to its `(TypeId, type_args)` pair for instance lookup.
    ///
    /// Handles both primitive types (no args) and parameterized builtins
    /// (`Array[T]`, `Option[T]`, `Result[Ok, Err]`, `Map[K, V]`), as well
    /// as `Ty::Named`.
    pub(super) fn ty_to_type_id_and_args(
        &self,
        ty: TyId,
    ) -> Option<(TypeId, SmallVec<[TyId; 4]>)> {
        match self.ty_arena.get(ty).clone() {
            Ty::Named(id, args) => Some((id, args)),
            Ty::Array(e) => Some((TypeId::ARRAY, smallvec![e])),
            Ty::Option(e) => Some((TypeId::OPTION, smallvec![e])),
            Ty::Result(ok, err) => Some((TypeId::RESULT, smallvec![ok, err])),
            Ty::Map(k, v) => Some((TypeId::MAP, smallvec![k, v])),
            Ty::Tuple(ts) => Some((TypeId::TUPLE, ts)),
            ref shape => {
                Self::primitive_type_id(shape).map(|id| (id, smallvec![]))
            }
        }
    }
    /// Check that a type satisfies a class constraint.
    ///
    /// This is the unified constraint checking method that handles all class
    /// constraints. The `class` parameter contains any associated types (e.g.,
    /// `Into(target)`, `Indexable(elem)`). The union-find is updated in-place
    /// when the constraint involves unification (e.g., `Fallible`, `Indexable`,
    /// `Indexable`).
    pub(super) fn satisfies_class(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        // Handle associated types: resolve to concrete type before checking
        let shape = self.ty_arena.get(ty).clone();
        if let Ty::AssocType(tv, assoc_class, name) = shape {
            // Resolve the base type variable through union-find
            match self.uf.resolve_var(tv, self.ty_arena) {
                Some(base) => {
                    if let Ok(resolved) =
                        self.resolve_assoc_type(base, assoc_class, name, span)
                    {
                        self.satisfies_class(class, resolved, span);
                    }
                }
                None => {
                    // Base type still unresolved; defer constraint
                }
            }
        } else {
            self.satisfies_class_inner(class, ty, span);
        }
    }
    /// Inner implementation of class constraint checking.
    fn satisfies_class_inner(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        match class {
            TypeClass::Concrete { id, ref params } if params.is_empty() => {
                self.check_simple_class(*id, class, ty, span)
            }
            TypeClass::Concrete {
                id: ClassId::INTO,
                ref params,
            } => {
                let to = params.first().copied().unwrap_or(TyArena::UNKNOWN);
                self.check_into(ty, to, span)
            }
            TypeClass::Concrete {
                id: ClassId::TRY_INTO,
                ref params,
            } => {
                let to = params.first().copied().unwrap_or(TyArena::UNKNOWN);
                self.check_try_into(ty, to, span)
            }
            TypeClass::Concrete {
                id: ClassId::INDEXABLE,
                ref params,
            } => {
                let elem = params.first().copied().unwrap_or(TyArena::UNKNOWN);
                self.check_indexable(class, ty, elem, span)
            }
            TypeClass::Concrete { id, ref params }
                if id.idx() >= ClassId::BUILTIN_COUNT && !params.is_empty() =>
            {
                self.check_user_parameterized(*id, params, class, ty, span)
            }
            TypeClass::Hkt {
                id: ClassId::ITERABLE,
                ..
            } => {
                self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                ));
            }
            TypeClass::Hkt { id, ref elems, .. }
                if matches!(
                    *id,
                    ClassId::MAPPABLE
                        | ClassId::FILTERABLE
                        | ClassId::FOLDABLE
                        | ClassId::BIMAPPABLE
                ) =>
            {
                self.check_hkt_class(*id, elems, class, ty, span)
            }
            TypeClass::Hkt { id, ref elems, .. }
                if id.idx() < ClassId::BUILTIN_COUNT =>
            {
                self.satisfies_hkt_class(*id, elems, class, ty, span);
            }
            TypeClass::Hkt { id, ref elems, .. } => {
                self.check_user_hkt(*id, elems, class, ty, span);
            }
            _ => {}
        }
    }
    /// How the given `shape` satisfies `class_id` as a builtin.
    ///
    /// Returns `None` if the builtin table has no entry; the caller then
    /// falls through to handling `Var`/`Union`/`Named`/etc.
    fn builtin_satisfaction(
        class_id: ClassId,
        shape: &Ty,
    ) -> Option<Satisfaction> {
        match (class_id, shape) {
            (id, Ty::Int | Ty::Word | Ty::Float)
                if Self::is_numeric_capability(id) =>
            {
                Some(Satisfaction::Direct)
            }
            (ClassId::BIT_LIKE, Ty::Bool | Ty::Int | Ty::Word) => {
                Some(Satisfaction::Direct)
            }
            (ClassId::NEGATABLE, Ty::Int | Ty::Float) => {
                Some(Satisfaction::Direct)
            }
            (
                ClassId::DEFAULT,
                Ty::Unit
                | Ty::Bool
                | Ty::String
                | Ty::Array(_)
                | Ty::Option(_)
                | Ty::Ordering
                | Ty::FilePath,
            ) => Some(Satisfaction::Direct),
            (ClassId::DEFAULT, Ty::Map(k, _)) => {
                Some(Satisfaction::Require(smallvec![(*k, ClassId::ORD)]))
            }
            (
                ClassId::CONCATABLE,
                Ty::String | Ty::Array(_) | Ty::Option(_),
            ) => Some(Satisfaction::Direct),
            (ClassId::CONCATABLE, Ty::Map(k, _)) => {
                Some(Satisfaction::Require(smallvec![(*k, ClassId::ORD)]))
            }
            (ClassId::ITERABLE, Ty::Array(_) | Ty::Range) => {
                Some(Satisfaction::Direct)
            }
            (
                ClassId::ORD,
                Ty::Bool
                | Ty::Int
                | Ty::Word
                | Ty::Float
                | Ty::Char
                | Ty::String
                | Ty::Time
                | Ty::Ordering,
            ) => Some(Satisfaction::Direct),
            (ClassId::ORD, Ty::Array(e)) => {
                Some(Satisfaction::Recurse(smallvec![*e]))
            }
            (ClassId::ORD, Ty::Tuple(es)) => {
                Some(Satisfaction::Recurse(SmallVec::from_slice(es)))
            }
            (ClassId::ORD, Ty::Option(e)) => {
                Some(Satisfaction::Recurse(smallvec![*e]))
            }
            (ClassId::ORD, Ty::Result(a, b)) => {
                Some(Satisfaction::Recurse(smallvec![*a, *b]))
            }
            (ClassId::ORD, Ty::Map(k, v)) => {
                Some(Satisfaction::Require(smallvec![
                    (*k, ClassId::ORD),
                    (*v, ClassId::ORD)
                ]))
            }
            (
                ClassId::EQ,
                Ty::Unit
                | Ty::Bool
                | Ty::Int
                | Ty::Word
                | Ty::Float
                | Ty::Char
                | Ty::String
                | Ty::Time
                | Ty::FilePath
                | Ty::Json
                | Ty::Local
                | Ty::Global
                | Ty::Ordering,
            ) => Some(Satisfaction::Direct),
            (ClassId::EQ, Ty::Array(e) | Ty::Option(e)) => {
                Some(Satisfaction::Recurse(smallvec![*e]))
            }
            (ClassId::EQ, Ty::Tuple(es)) => {
                Some(Satisfaction::Recurse(SmallVec::from_slice(es)))
            }
            (ClassId::EQ, Ty::Result(a, b)) => {
                Some(Satisfaction::Recurse(smallvec![*a, *b]))
            }
            (ClassId::EQ, Ty::Map(k, v)) => {
                Some(Satisfaction::Require(smallvec![
                    (*k, ClassId::ORD),
                    (*v, ClassId::EQ)
                ]))
            }
            (ClassId::EQ, Ty::Object(fields)) => {
                Some(Satisfaction::Recurse(fields.values().copied().collect()))
            }
            // `Display` must NOT wildcard these shapes; they need to fall
            // through to the dispatcher (instance lookup for `Named`/`Union`,
            // silent for `Var`/`Error`/`Unknown`, error for `Fn`).
            (
                ClassId::DISPLAY,
                Ty::Fn(_, _)
                | Ty::Var(_)
                | Ty::Error
                | Ty::Unknown
                | Ty::Union(_, _)
                | Ty::Named(_, _),
            ) => None,
            (ClassId::DISPLAY, _) => Some(Satisfaction::Direct),
            _ => None,
        }
    }
    /// Check a "simple" class (`Numeric`, numeric capabilities, `BitLike`,
    /// `Negatable`, `Default`, `Concatable`, `Ord`, `Eq`, `Display`) against `ty`.
    ///
    /// Per-class dispatch rules: Numeric capabilities on a `Union` succeed if
    /// any one member directly satisfies using the "any" strategy; on a
    /// `Named` with no instance, fall back to alias expansion. `BitLike` on a
    /// `Named` with no instance falls back to alias expansion. All others
    /// require every `Union` member to satisfy, and a `Named` with no instance
    /// is an error.
    fn check_simple_class(
        &mut self,
        class_id: ClassId,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(ty).clone();
        match Self::builtin_satisfaction(class_id, &shape) {
            Some(Satisfaction::Direct) => {}
            Some(Satisfaction::Recurse(inners)) => {
                inners
                    .iter()
                    .for_each(|&t| self.satisfies_class(class, t, span));
            }
            Some(Satisfaction::Require(reqs)) => {
                reqs.iter().for_each(|&(t, c)| {
                    self.satisfies_class(&TypeClass::simple(c), t, span)
                });
            }
            None => match shape {
                Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                Ty::Union(prov, members) => {
                    let lookup = prov.map(|id| {
                        self.instance_for(
                            InstanceUse::Evidence,
                            class_id,
                            id,
                            span,
                        )
                    });
                    match lookup {
                        Some(InstanceLookup::Found(inst)) => self
                            .check_instance_constraints(&inst, &[], span, None),
                        Some(InstanceLookup::BlockedSelf) => {}
                        Some(InstanceLookup::Missing)
                        | Some(InstanceLookup::NotImported)
                        | None => {
                            if Self::is_numeric_capability(class_id) {
                                let any_sat = members.iter().any(|&m| {
                                    let sh = self.ty_arena.get(m).clone();
                                    matches!(
                                        Self::builtin_satisfaction(
                                            class_id, &sh
                                        ),
                                        Some(Satisfaction::Direct)
                                    )
                                });
                                if !any_sat {
                                    self.errors.push(
                                        TypeError::UnsatisfiedClass(
                                            class.clone(),
                                            ty,
                                            span,
                                        ),
                                    );
                                }
                            } else {
                                members.iter().for_each(|&m| {
                                    self.satisfies_class(class, m, span)
                                });
                            }
                        }
                    }
                }
                Ty::Named(id, args) => {
                    match self.instance_for(
                        InstanceUse::Evidence,
                        class_id,
                        id,
                        span,
                    ) {
                        InstanceLookup::Found(inst) => self
                            .check_instance_constraints(
                                &inst, &args, span, None,
                            ),
                        InstanceLookup::BlockedSelf => {}
                        InstanceLookup::Missing
                        | InstanceLookup::NotImported => {
                            let expanded =
                                if Self::is_numeric_capability(class_id)
                                    || class_id == ClassId::BIT_LIKE
                                {
                                    self.expand_alias_fully_for_class(
                                        class_id, ty, span,
                                    )
                                } else {
                                    None
                                };
                            match expanded {
                                Some(e) => self.satisfies_class(class, e, span),
                                None => self.errors.push(
                                    TypeError::UnsatisfiedClass(
                                        class.clone(),
                                        ty,
                                        span,
                                    ),
                                ),
                            }
                        }
                    }
                }
                // User classes: handle parameterized builtins via instance lookup
                _ if class_id.idx() >= ClassId::BUILTIN_COUNT => {
                    match self.ty_to_type_id_and_args(ty) {
                        Some((tid, args)) => {
                            match self.instance_for(
                                InstanceUse::Evidence,
                                class_id,
                                tid,
                                span,
                            ) {
                                InstanceLookup::Found(inst) => self
                                    .check_instance_constraints(
                                        &inst, &args, span, None,
                                    ),
                                InstanceLookup::BlockedSelf => {}
                                InstanceLookup::Missing
                                | InstanceLookup::NotImported => self
                                    .errors
                                    .push(TypeError::UnsatisfiedClass(
                                        class.clone(),
                                        ty,
                                        span,
                                    )),
                            }
                        }
                        None => self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        )),
                    }
                }
                _ => {
                    self.errors.push(TypeError::UnsatisfiedClass(
                        class.clone(),
                        ty,
                        span,
                    ));
                }
            },
        }
    }
    /// Check an HKT class (`Mappable`, `Filterable`, `Foldable`, `Bimappable`)
    /// against `ty`, optionally unifying element types with `elems`.
    fn check_hkt_class(
        &mut self,
        class_id: ClassId,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let ty = self
            .expand_alias_fully_for_class(class_id, ty, span)
            .unwrap_or(ty);
        let shape = self.ty_arena.get(ty).clone();
        let builtin_elems: Option<SmallVec<[TyId; 2]>> =
            match (class_id, &shape) {
                (
                    ClassId::MAPPABLE | ClassId::FILTERABLE | ClassId::FOLDABLE,
                    Ty::Array(e),
                ) => Some(smallvec![*e]),
                (ClassId::MAPPABLE, Ty::Tuple(ts)) if ts.len() == 2 => {
                    ts.get(1).copied().map(|e| smallvec![e])
                }
                (ClassId::MAPPABLE, Ty::Option(e)) => Some(smallvec![*e]),
                (ClassId::MAPPABLE, Ty::Result(ok, _)) => Some(smallvec![*ok]),
                (ClassId::BIMAPPABLE, Ty::Result(ok, err)) => {
                    Some(smallvec![*ok, *err])
                }
                (ClassId::BIMAPPABLE, Ty::Tuple(ts)) if ts.len() == 2 => {
                    Some(ts.iter().copied().collect())
                }
                _ => None,
            };
        match builtin_elems {
            Some(ref inner) => {
                self.unify_hkt_known_args(elems, inner, span);
            }
            None => match shape {
                Ty::Union(_, members) => {
                    members
                        .iter()
                        .for_each(|&m| self.satisfies_class(class, m, span));
                }
                Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
                Ty::Named(id, type_args) => {
                    match self.instance_for(
                        InstanceUse::Evidence,
                        class_id,
                        id,
                        span,
                    ) {
                        InstanceLookup::Found(inst) => {
                            let param_subst = self
                                .build_instance_subst(&inst, &type_args, span);
                            self.unify_hkt_inst_args(
                                elems,
                                &inst.class_args,
                                &param_subst,
                                span,
                            );
                            self.check_instance_constraints(
                                &inst,
                                &type_args,
                                span,
                                Some(&param_subst),
                            );
                        }
                        InstanceLookup::BlockedSelf => {}
                        InstanceLookup::Missing
                        | InstanceLookup::NotImported => {
                            self.errors.push(TypeError::UnsatisfiedClass(
                                class.clone(),
                                ty,
                                span,
                            ));
                        }
                    }
                }
                Ty::Tuple(ts) => {
                    match self.instances_for(
                        InstanceUse::Evidence,
                        class_id,
                        TypeId::TUPLE,
                        span,
                    ) {
                        InstancesLookup::Found(insts) => {
                            match insts
                                .into_iter()
                                .find(|i| i.type_params.len() == ts.len())
                            {
                                Some(inst) => {
                                    let subst = self
                                        .build_instance_subst(&inst, &ts, span);
                                    self.unify_hkt_inst_args(
                                        elems,
                                        &inst.class_args,
                                        &subst,
                                        span,
                                    );
                                    self.check_instance_constraints(
                                        &inst,
                                        &ts,
                                        span,
                                        Some(&subst),
                                    );
                                }
                                None => self.errors.push(
                                    TypeError::UnsatisfiedClass(
                                        class.clone(),
                                        ty,
                                        span,
                                    ),
                                ),
                            }
                        }
                        InstancesLookup::BlockedSelf => {}
                    }
                }
                _ => {
                    self.errors.push(TypeError::UnsatisfiedClass(
                        class.clone(),
                        ty,
                        span,
                    ));
                }
            },
        }
    }
    /// Shared HKT class satisfaction logic for `Fallible`, `Wrappable`, and `Chainable`.
    ///
    /// All three handle the same set of types (`Option`, `Result`, `Tuple`, `Union`,
    /// `Var` defaulting to `Option`, `Apply`, `Named` via instance registry) and differ
    /// only in which tag is used for registry lookups and error messages.
    fn satisfies_hkt_class(
        &mut self,
        tag: ClassId,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let ty = self
            .expand_alias_fully_for_class(tag, ty, span)
            .unwrap_or(ty);

        match self.ty_arena.get(ty).clone() {
            Ty::Option(opt_elem) => {
                self.unify_hkt_known_args(elems, &[opt_elem], span);
            }
            Ty::Result(ok, err) => {
                let builtin: SmallVec<[TyId; 2]> = if elems.len() <= 1 {
                    smallvec![ok]
                } else {
                    smallvec![ok, err]
                };
                self.unify_hkt_known_args(elems, &builtin, span);
            }
            Ty::Tuple(ts) => {
                match self.instances_for(
                    InstanceUse::Evidence,
                    tag,
                    TypeId::TUPLE,
                    span,
                ) {
                    InstancesLookup::Found(insts) => {
                        let inst = insts
                            .into_iter()
                            .find(|i| i.type_params.len() == ts.len());
                        match inst {
                            Some(inst) => {
                                let subst =
                                    self.build_instance_subst(&inst, &ts, span);
                                self.unify_hkt_inst_args(
                                    elems,
                                    &inst.class_args,
                                    &subst,
                                    span,
                                );
                                self.check_instance_constraints(
                                    &inst,
                                    &ts,
                                    span,
                                    Some(&subst),
                                );
                            }
                            None => {
                                // Fallback for fully-unapplied tuple constructors.
                                self.unify_hkt_known_args(elems, &ts, span);
                            }
                        }
                    }
                    InstancesLookup::BlockedSelf => {}
                }
            }
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|m| self.satisfies_class(class, *m, span));
            }
            Ty::Var(v) => {
                if elems.len() <= 1 {
                    let elem = elems.first().copied().unwrap_or_else(|| {
                        let fv = self.uf.fresh();
                        self.ty_arena.alloc(Ty::Var(fv))
                    });
                    let opt_id = self.ty_arena.option(elem);
                    let root = self.uf.find(v);
                    self.uf.bind(root, opt_id);
                }
            }
            Ty::Apply(_, _) => {}
            Ty::Error | Ty::Unknown => {}
            Ty::Named(id, type_args) => {
                match self.instance_for(InstanceUse::Evidence, tag, id, span) {
                    InstanceLookup::Found(inst) => {
                        let param_subst =
                            self.build_instance_subst(&inst, &type_args, span);
                        self.unify_hkt_inst_args(
                            elems,
                            &inst.class_args,
                            &param_subst,
                            span,
                        );
                        self.check_instance_constraints(
                            &inst,
                            &type_args,
                            span,
                            Some(&param_subst),
                        );
                    }
                    InstanceLookup::BlockedSelf => {}
                    InstanceLookup::Missing | InstanceLookup::NotImported => {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
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
    /// Check a parameterized user class constraint via instance lookup.
    fn check_user_parameterized(
        &mut self,
        class_id: ClassId,
        params: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let class_arg = params.first().copied().unwrap_or(TyArena::UNKNOWN);
        let shape = self.ty_arena.get(ty).clone();
        match shape {
            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
            Ty::Union(prov, members) => {
                match prov.map(|id| {
                    self.instances_for(
                        InstanceUse::Evidence,
                        class_id,
                        id,
                        span,
                    )
                }) {
                    Some(InstancesLookup::BlockedSelf) => {}
                    Some(InstancesLookup::Found(insts))
                        if !insts.is_empty() =>
                    {
                        match self.find_matching_instance(
                            &insts,
                            class_arg,
                            &[],
                        ) {
                            Some(inst) => self.check_instance_constraints(
                                &inst,
                                &[],
                                span,
                                None,
                            ),
                            None => members.iter().for_each(|&m| {
                                self.satisfies_class(class, m, span)
                            }),
                        }
                    }
                    Some(InstancesLookup::Found(_)) | None => {
                        members.iter().for_each(|&m| {
                            self.satisfies_class(class, m, span)
                        });
                    }
                }
            }
            _ => match self.ty_to_type_id_and_args(ty) {
                Some((tid, args)) => {
                    match self.instances_for(
                        InstanceUse::Evidence,
                        class_id,
                        tid,
                        span,
                    ) {
                        InstancesLookup::Found(insts) => {
                            match self.find_matching_instance(
                                &insts, class_arg, &args,
                            ) {
                                Some(inst) => {
                                    let subst = self.build_instance_subst(
                                        &inst, &args, span,
                                    );
                                    if let Some(&ia) = inst.class_args.first() {
                                        let resolved =
                                            self.ty_arena.apply(ia, &subst);
                                        if let Err(e) = self.unify_types(
                                            class_arg, resolved, span,
                                        ) {
                                            self.errors.push(e);
                                        }
                                    }
                                    self.check_instance_constraints(
                                        &inst,
                                        &args,
                                        span,
                                        Some(&subst),
                                    );
                                }
                                None => self.errors.push(
                                    TypeError::UnsatisfiedClass(
                                        class.clone(),
                                        ty,
                                        span,
                                    ),
                                ),
                            }
                        }
                        InstancesLookup::BlockedSelf => {}
                    }
                }
                None => self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                )),
            },
        }
    }
    /// Check a user-defined HKT class constraint via instance lookup.
    ///
    /// Unlike `satisfies_hkt_class`, does NOT default `Ty::Var` to `Option`
    /// and does NOT hardcode `Ty::Option`/`Ty::Result` as satisfying.
    fn check_user_hkt(
        &mut self,
        class_id: ClassId,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(ty).clone();
        match shape {
            Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|&m| self.satisfies_class(class, m, span));
            }
            _ => match self.ty_to_type_id_and_args(ty) {
                Some((tid, args)) => {
                    if tid == TypeId::TUPLE {
                        match self.instances_for(
                            InstanceUse::Evidence,
                            class_id,
                            TypeId::TUPLE,
                            span,
                        ) {
                            InstancesLookup::Found(insts) => {
                                match insts
                                    .into_iter()
                                    .find(|i| i.type_params.len() == args.len())
                                {
                                    Some(inst) => {
                                        let subst = self.build_instance_subst(
                                            &inst, &args, span,
                                        );
                                        self.unify_hkt_inst_args(
                                            elems,
                                            &inst.class_args,
                                            &subst,
                                            span,
                                        );
                                        self.check_instance_constraints(
                                            &inst,
                                            &args,
                                            span,
                                            Some(&subst),
                                        );
                                    }
                                    None => self.errors.push(
                                        TypeError::UnsatisfiedClass(
                                            class.clone(),
                                            ty,
                                            span,
                                        ),
                                    ),
                                }
                            }
                            InstancesLookup::BlockedSelf => {}
                        }
                    } else {
                        match self.instance_for(
                            InstanceUse::Evidence,
                            class_id,
                            tid,
                            span,
                        ) {
                            InstanceLookup::Found(inst) => {
                                let subst = self
                                    .build_instance_subst(&inst, &args, span);
                                self.unify_hkt_inst_args(
                                    elems,
                                    &inst.class_args,
                                    &subst,
                                    span,
                                );
                                self.check_instance_constraints(
                                    &inst,
                                    &args,
                                    span,
                                    Some(&subst),
                                );
                            }
                            InstanceLookup::BlockedSelf => {}
                            InstanceLookup::Missing
                            | InstanceLookup::NotImported => {
                                self.errors.push(TypeError::UnsatisfiedClass(
                                    class.clone(),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                }
                None => self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                )),
            },
        }
    }
}
