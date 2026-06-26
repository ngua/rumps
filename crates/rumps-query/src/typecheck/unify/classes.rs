use super::evidence::{Evidence, EvidenceQuery};
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
            // `Display` and `Formattable` must NOT wildcard these shapes; they
            // need to fall through to the dispatcher; instance lookup for
            // `Named`/`Union`, silent for `Var`/`Error`/`Unknown`, error for
            // `Fn`.
            (
                ClassId::DISPLAY | ClassId::FORMATTABLE,
                Ty::Fn(_, _)
                | Ty::Var(_)
                | Ty::Error
                | Ty::Unknown
                | Ty::Union(_, _)
                | Ty::Named(_, _),
            ) => None,
            (ClassId::DISPLAY | ClassId::FORMATTABLE, _) => {
                Some(Satisfaction::Direct)
            }
            _ => None,
        }
    }

    /// Check a "simple" class (`Numeric`, numeric capabilities, `BitLike`,
    /// `Negatable`, `Default`, `Concatable`, `Ord`, `Eq`, `Display`,
    /// `Formattable`) against `ty`.
    ///
    /// Per-class dispatch rules: Numeric capabilities on a `Union` succeed if
    /// any one member directly satisfies using the "any" strategy; on a
    /// `Named` with no instance, fall back to alias expansion. `BitLike` on a
    /// `Named` with no instance falls back to alias expansion. All others
    /// require every `Union` member to satisfy, and a `Named` with no instance
    /// is an error.
    pub(in crate::typecheck::unify) fn satisfy_simple(
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
                Ty::Union(_, _) => {
                    match self.evidence(EvidenceQuery { class, ty, span }) {
                        Evidence::Manual { inst, args } => self
                            .apply_inst_constraints(&inst, &args, span, None),
                        Evidence::ManualMany { insts, args } => {
                            match insts.first() {
                                Some(inst) => self.apply_inst_constraints(
                                    inst, &args, span, None,
                                ),
                                None => {
                                    invariant!(
                                        "`ManualMany` has at least one instance"
                                    )
                                }
                            }
                        }
                        Evidence::BlockedSelf | Evidence::NotImported => {}
                        Evidence::Union { members, .. } => {
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
                        Evidence::Missing => {
                            self.errors.push(TypeError::UnsatisfiedClass(
                                class.clone(),
                                ty,
                                span,
                            ))
                        }
                        Evidence::DerivedVariant { .. }
                        | Evidence::Repr { .. }
                        | Evidence::BuiltinNamed => {
                            self.errors.push(TypeError::UnsatisfiedClass(
                                class.clone(),
                                ty,
                                span,
                            ))
                        }
                    }
                }
                Ty::Named(_, _) => {
                    match self.evidence(EvidenceQuery { class, ty, span }) {
                        Evidence::Manual { inst, args } => self
                            .apply_inst_constraints(&inst, &args, span, None),
                        Evidence::ManualMany { insts, args } => {
                            match insts.first() {
                                Some(inst) => self.apply_inst_constraints(
                                    inst, &args, span, None,
                                ),
                                None => {
                                    invariant!(
                                        "`ManualMany` has at least one instance"
                                    )
                                }
                            }
                        }
                        Evidence::DerivedVariant { payloads } => {
                            payloads.iter().for_each(|&p| {
                                self.satisfies_class(class, p, span)
                            });
                        }
                        Evidence::Repr { ty: repr, .. } => {
                            self.satisfies_class(class, repr, span);
                        }
                        Evidence::BlockedSelf | Evidence::NotImported => {}
                        Evidence::Missing
                        | Evidence::Union { .. }
                        | Evidence::BuiltinNamed => {
                            self.errors.push(TypeError::UnsatisfiedClass(
                                class.clone(),
                                ty,
                                span,
                            ))
                        }
                    }
                }
                // User classes: handle parameterized builtins via instance lookup
                _ if class_id.idx() >= ClassId::BUILTIN_COUNT => {
                    match self.evidence(EvidenceQuery { class, ty, span }) {
                        Evidence::Manual { inst, args } => self
                            .apply_inst_constraints(&inst, &args, span, None),
                        Evidence::ManualMany { insts, args } => {
                            match insts.first() {
                                Some(inst) => self.apply_inst_constraints(
                                    inst, &args, span, None,
                                ),
                                None => {
                                    invariant!(
                                        "`ManualMany` has at least one instance"
                                    )
                                }
                            }
                        }
                        Evidence::BlockedSelf | Evidence::NotImported => {}
                        Evidence::Missing
                        | Evidence::Union { .. }
                        | Evidence::DerivedVariant { .. }
                        | Evidence::Repr { .. }
                        | Evidence::BuiltinNamed => {
                            self.errors.push(TypeError::UnsatisfiedClass(
                                class.clone(),
                                ty,
                                span,
                            ))
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
            },
        }
    }

    /// Check an HKT class (`Mappable`, `Filterable`, `Foldable`, `Bimappable`)
    /// against `ty`, optionally unifying element types with `elems`.
    pub(in crate::typecheck::unify) fn satisfy_builtin_hkt(
        &mut self,
        class_id: ClassId,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
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
                Ty::Named(_, _) => {
                    self.apply_hkt_evidence(elems, class, ty, span, true);
                }
                Ty::Tuple(_) => {
                    if self.apply_hkt_evidence(elems, class, ty, span, false) {
                    } else {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
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
    /// All three handle the same set of types, including `Option`, `Result`,
    /// `Tuple`, `Union`, `Var` defaulting to `Option`, `Apply`, and `Named`
    /// via evidence. They differ only in which tag is used for evidence lookup
    /// and error messages.
    pub(in crate::typecheck::unify) fn satisfy_hkt_stack(
        &mut self,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
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
                if self.apply_hkt_evidence(elems, class, ty, span, false) {
                } else {
                    self.unify_hkt_known_args(elems, &ts, span);
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
            Ty::Named(_, _) => {
                self.apply_hkt_evidence(elems, class, ty, span, true);
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

    /// Check a user-defined HKT class constraint via instance lookup.
    ///
    /// Unlike `satisfy_hkt_stack`, does NOT default `Ty::Var` to `Option`
    /// and does NOT hardcode `Ty::Option`/`Ty::Result` as satisfying.
    pub(in crate::typecheck::unify) fn satisfy_user_hkt(
        &mut self,
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
                Some(_) => {
                    if self.apply_hkt_evidence(elems, class, ty, span, false) {
                    } else {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
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

    fn apply_hkt_evidence(
        &mut self,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
        allow_repr: bool,
    ) -> bool {
        match self.evidence(EvidenceQuery { class, ty, span }) {
            Evidence::Manual { inst, args } => {
                self.apply_hkt_inst(&inst, elems, &args, span);
                true
            }
            Evidence::ManualMany { insts, args } => match insts.first() {
                Some(inst) => {
                    self.apply_hkt_inst(inst, elems, &args, span);
                    true
                }
                None => {
                    invariant!("`ManualMany` has at least one instance")
                }
            },
            Evidence::Repr { ty: repr, .. } if allow_repr => {
                self.satisfies_class(class, repr, span);
                true
            }
            Evidence::BlockedSelf | Evidence::NotImported => true,
            Evidence::Missing
            | Evidence::Union { .. }
            | Evidence::DerivedVariant { .. }
            | Evidence::Repr { .. }
            | Evidence::BuiltinNamed => false,
        }
    }

    fn apply_hkt_inst(
        &mut self,
        inst: &Instance,
        elems: &[TyId],
        args: &[TyId],
        span: Span,
    ) {
        let subst = self.build_instance_subst(inst, args, span);
        self.unify_hkt_inst_args(elems, &inst.class_args, &subst, span);
        self.apply_inst_constraints(inst, args, span, Some(&subst));
    }
}
