//! Type representation for static type checking.
//!
//! Defines the core types: `Ty` (types), `TyVar` (type variables), `Scheme`
//! (polymorphic type schemes), and `Rename` (local type variable renames).

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::{fmt, iter, mem, result};

use indexmap::IndexMap;
use rumps_query_macros::scheme;
use smallvec::{smallvec, SmallVec};

use super::error::TypeError;
use super::uf::UnionFind;
use crate::intern::StringId;
use crate::{ClassId, Span, TypeId};

/// The "shape" of a builtin class constraint.
///
/// Determines how many and what kind of type parameters the class carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClassShape {
    /// Kind `*`; optionally carries fixed type params in constraints.
    Concrete { params: u8 },
    /// Kind `* -> *` (or higher); element types from usage sites, plus optional
    /// fixed params.
    Hkt { kind: u8, params: u8 },
}

/// What kind of type tracking a method requires for runtime dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackKind {
    /// Track return type in expression metadata (`Default:default`).
    Default,
    /// Track return type in expression metadata (`Additive:zero`).
    Zero,
    /// Track return type in expression metadata (`Multiplicative:one`).
    One,
    /// Track return type in expression metadata (`Into:into`, `Wrappable:wrap`).
    Convert,
    /// Track inner type of `Result` return in expression metadata (`TryInto:try-into`).
    ConvertResultInner,
}

/// Method specification returned by `ClassDef::method`.
#[derive(Clone, Debug)]
pub(crate) enum MethodSpec {
    /// Standard method; just instantiate scheme and unify.
    Standard(Scheme),
    /// Method that needs type tracking for runtime dispatch.
    Tracked { scheme: Scheme, track: TrackKind },
}

impl MethodSpec {
    pub(crate) fn scheme(&self) -> &Scheme {
        match self {
            Self::Standard(s) | Self::Tracked { scheme: s, .. } => s,
        }
    }
}

/// Full definition of a type class, keyed by `ClassId`.
#[derive(Clone)]
pub(crate) struct ClassDef {
    pub(crate) name: StringId,
    pub(crate) shape: ClassShape,
    pub(crate) assoc_types: SmallVec<[StringId; 2]>,
    pub(crate) methods: Vec<(StringId, MethodSpec)>,
    pub(crate) supers: SmallVec<[ClassId; 2]>,
}

impl ClassDef {
    /// Look up a method by name.
    pub(crate) fn method(
        &self,
        name: StringId,
        span: Span,
    ) -> Result<&MethodSpec, TypeError> {
        self.methods
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, spec)| spec)
            .ok_or(TypeError::UnknownMethodId {
                class: self.name,
                method: name,
                span,
            })
    }

    /// All method names (all methods are required).
    pub(crate) fn method_names(&self) -> impl Iterator<Item = StringId> + '_ {
        self.methods.iter().map(|(n, _)| *n)
    }
}

/// Registry of all known type classes, indexed by `ClassId`.
#[derive(Clone)]
pub(crate) struct ClassRegistry {
    defs: Vec<ClassDef>,
    by_name: HashMap<StringId, ClassId>,
}

impl ClassRegistry {
    /// Build the registry with all `ClassId::BUILTIN_COUNT` builtin classes.
    pub(crate) fn builtins(
        intern: &mut impl FnMut(&str) -> StringId,
        arena: &mut TyArena,
    ) -> Self {
        let idx_name = intern("Index");

        // Shorthand for interning
        let s = &mut *intern;

        let defs = vec![
            // `0`: `Numeric`
            ClassDef {
                name: s("Numeric"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![
                    ClassId::ADDITIVE,
                    ClassId::SUBTRACTIVE,
                    ClassId::MULTIPLICATIVE,
                    ClassId::DIVISIBLE,
                    ClassId::FLOOR_DIVISIBLE,
                    ClassId::POWERABLE,
                ],
                methods: vec![],
            },
            // 1: Iterable
            ClassDef {
                name: s("Iterable"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![
                    (
                        s("length"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: Iterable. (T) -> Int
                        )),
                    ),
                    (
                        s("reverse"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: Iterable. (T) -> T
                        )),
                    ),
                ],
            },
            // 2: Default
            ClassDef {
                name: s("Default"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("default"),
                    MethodSpec::Tracked {
                        scheme: scheme!(arena, forall T: Default. () -> T),
                        track: TrackKind::Default,
                    },
                )],
            },
            // 3: Concatable
            ClassDef {
                name: s("Concatable"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("concat"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Concatable. (T, T) -> T
                    )),
                )],
            },
            // 4: BitLike
            ClassDef {
                name: s("BitLike"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![
                    (
                        s("bit-and"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: BitLike. (T, T) -> T
                        )),
                    ),
                    (
                        s("bit-or"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: BitLike. (T, T) -> T
                        )),
                    ),
                    (
                        s("shl"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: BitLike. (T, T) -> T
                        )),
                    ),
                    (
                        s("shr"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: BitLike. (T, T) -> T
                        )),
                    ),
                ],
            },
            // 5: Negatable
            ClassDef {
                name: s("Negatable"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("neg"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Negatable. (T) -> T
                    )),
                )],
            },
            // 6: Fallible
            ClassDef {
                name: s("Fallible"),
                shape: ClassShape::Hkt { kind: 1, params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![ClassId::WRAPPABLE],
                methods: vec![(
                    s("unwrap"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T, F: Fallible. (F[T]) -> T
                    )),
                )],
            },
            // 7: Into
            ClassDef {
                name: s("Into"),
                shape: ClassShape::Concrete { params: 1 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("into"),
                    MethodSpec::Tracked {
                        scheme: scheme!(arena, forall T: Into[U], U. (T) -> U),
                        track: TrackKind::Convert,
                    },
                )],
            },
            // 8: TryInto
            ClassDef {
                name: s("TryInto"),
                shape: ClassShape::Concrete { params: 1 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("try-into"),
                    MethodSpec::Tracked {
                        scheme: scheme!(
                            arena,
                            forall T: TryInto[U], U. (T) -> Result[U, String]
                        ),
                        track: TrackKind::ConvertResultInner,
                    },
                )],
            },
            // 9: Indexable
            ClassDef {
                name: s("Indexable"),
                shape: ClassShape::Concrete { params: 1 },
                assoc_types: smallvec![idx_name],
                supers: smallvec![],
                methods: vec![
                    (
                        s("index"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            s,
                            forall T: Indexable[E], E. (T, T:Indexable:Index) -> E
                        )),
                    ),
                    (
                        s("get"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            s,
                            forall T: Indexable[E], E. (T, T:Indexable:Index) -> Option[E]
                        )),
                    ),
                ],
            },
            // 10: Ord
            ClassDef {
                name: s("Ord"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("compare"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Ord. (T, T) -> Ordering
                    )),
                )],
            },
            // 11: Mappable
            ClassDef {
                name: s("Mappable"),
                shape: ClassShape::Hkt { kind: 1, params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("map"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T, U, F: Mappable. ((T) -> U, F[T]) -> F[U]
                    )),
                )],
            },
            // 12: Foldable
            ClassDef {
                name: s("Foldable"),
                shape: ClassShape::Hkt { kind: 1, params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("fold"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T, U, F: Foldable. ((U, T) -> U, U, F[T]) -> U
                    )),
                )],
            },
            // 13: Filterable
            ClassDef {
                name: s("Filterable"),
                shape: ClassShape::Hkt { kind: 1, params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("filter"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T, F: Filterable. ((T) -> Bool, F[T]) -> F[T]
                    )),
                )],
            },
            // 14: Display
            ClassDef {
                name: s("Display"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("display"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Display. (T) -> String
                    )),
                )],
            },
            // 15: Eq
            ClassDef {
                name: s("Eq"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("eq"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Eq. (T, T) -> Bool
                    )),
                )],
            },
            // 16: Wrappable
            ClassDef {
                name: s("Wrappable"),
                shape: ClassShape::Hkt { kind: 1, params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("wrap"),
                    MethodSpec::Tracked {
                        scheme: scheme!(
                            arena,
                            forall T, F: Wrappable. (T) -> F[T]
                        ),
                        track: TrackKind::Convert,
                    },
                )],
            },
            // 17: Chainable
            ClassDef {
                name: s("Chainable"),
                shape: ClassShape::Hkt { kind: 1, params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![ClassId::WRAPPABLE],
                methods: vec![(
                    s("chain"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T, U, F: Chainable. (F[T], (T) -> F[U]) -> F[U]
                    )),
                )],
            },
            // 18: Bimappable
            ClassDef {
                name: s("Bimappable"),
                shape: ClassShape::Hkt { kind: 2, params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![(
                    s("bimap"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall A, B, C, D, F: Bimappable.
                            ((A) -> C, (B) -> D, F[A, B]) -> F[C, D]
                    )),
                )],
            },
            // `19`: `Additive`
            ClassDef {
                name: s("Additive"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![
                    (
                        s("zero"),
                        MethodSpec::Tracked {
                            scheme: scheme!(arena, forall T: Additive. () -> T),
                            track: TrackKind::Zero,
                        },
                    ),
                    (
                        s("add"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: Additive. (T, T) -> T
                        )),
                    ),
                ],
            },
            // `20`: `Subtractive`
            ClassDef {
                name: s("Subtractive"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![ClassId::ADDITIVE],
                methods: vec![(
                    s("sub"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Subtractive. (T, T) -> T
                    )),
                )],
            },
            // `21`: `Multiplicative`
            ClassDef {
                name: s("Multiplicative"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![],
                methods: vec![
                    (
                        s("one"),
                        MethodSpec::Tracked {
                            scheme: scheme!(
                                arena,
                                forall T: Multiplicative. () -> T
                            ),
                            track: TrackKind::One,
                        },
                    ),
                    (
                        s("mul"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: Multiplicative. (T, T) -> T
                        )),
                    ),
                ],
            },
            // `22`: `Divisible`
            ClassDef {
                name: s("Divisible"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![ClassId::MULTIPLICATIVE],
                methods: vec![(
                    s("div"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Divisible. (T, T) -> T
                    )),
                )],
            },
            // `23`: `FloorDivisible`
            ClassDef {
                name: s("FloorDivisible"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![ClassId::DIVISIBLE],
                methods: vec![
                    (
                        s("floor-div"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: FloorDivisible. (T, T) -> T
                        )),
                    ),
                    (
                        s("mod"),
                        MethodSpec::Standard(scheme!(
                            arena,
                            forall T: FloorDivisible. (T, T) -> T
                        )),
                    ),
                ],
            },
            // `24`: `Powerable`
            ClassDef {
                name: s("Powerable"),
                shape: ClassShape::Concrete { params: 0 },
                assoc_types: smallvec![],
                supers: smallvec![ClassId::MULTIPLICATIVE],
                methods: vec![(
                    s("pow"),
                    MethodSpec::Standard(scheme!(
                        arena,
                        forall T: Powerable. (T, T) -> T
                    )),
                )],
            },
        ];

        let by_name = defs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.name, ClassId::new(i as u32)))
            .collect();

        Self { defs, by_name }
    }

    /// Empty registry with no class definitions.
    pub(crate) fn empty() -> Self {
        Self {
            defs: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    pub(crate) fn get(&self, id: ClassId) -> &ClassDef {
        &self.defs[id.idx()]
    }

    pub(crate) fn get_mut(&mut self, id: ClassId) -> &mut ClassDef {
        &mut self.defs[id.idx()]
    }

    pub(crate) fn lookup_by_name(&self, s: StringId) -> Option<ClassId> {
        self.by_name.get(&s).copied()
    }

    /// Register a new user-defined class. Returns `Err` if the name
    /// conflicts with an existing class.
    pub(crate) fn register(
        &mut self,
        def: ClassDef,
    ) -> result::Result<ClassId, DuplicateClassError> {
        if let Some(&existing) = self.by_name.get(&def.name) {
            Err(DuplicateClassError {
                name: def.name,
                existing,
            })
        } else {
            let id = ClassId::new(self.defs.len() as u32);
            self.by_name.insert(def.name, id);
            self.defs.push(def);
            Ok(id)
        }
    }

    /// Find all classes that define a method with the given name.
    pub(crate) fn lookup_by_method(
        &self,
        method: StringId,
    ) -> SmallVec<[ClassId; 2]> {
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, def)| def.methods.iter().any(|(n, _)| *n == method))
            .map(|(i, _)| ClassId::new(i as u32))
            .collect()
    }

    pub(crate) fn name(&self, id: ClassId) -> StringId {
        self.get(id).name
    }

    pub(crate) fn shape(&self, id: ClassId) -> ClassShape {
        self.get(id).shape
    }

    pub(crate) fn supers(&self, id: ClassId) -> &[ClassId] {
        &self.get(id).supers
    }

    /// All transitive superclasses in dependency order (parents before children).
    pub(crate) fn transitive_supers(&self, id: ClassId) -> Vec<ClassId> {
        let mut acc = Vec::new();
        self.supers(id)
            .iter()
            .for_each(|&sup| self.collect_supers(sup, &mut acc));
        acc
    }

    fn collect_supers(&self, id: ClassId, acc: &mut Vec<ClassId>) {
        if !acc.contains(&id) {
            self.supers(id)
                .iter()
                .for_each(|&sup| self.collect_supers(sup, acc));
            acc.push(id);
        }
    }
}

/// Error when registering a class whose name already exists.
pub(crate) struct DuplicateClassError {
    pub(crate) name: StringId,
    pub(crate) existing: ClassId,
}

/// Lightweight, cloneable class constraint reference, generic over the type
/// representation.
///
/// Layer instantiations:
/// - CST: `TypeClass<TypeExpr>`
/// - AST: `TypeClass<AstTypeExprId>`
/// - Ty:  `TypeClass<TyId>`
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypeClass<T> {
    /// Kind `*` class with optional fixed type params.
    Concrete {
        id: ClassId,
        params: SmallVec<[T; 1]>,
    },
    /// Kind `* -> *` (or higher) class; `elems` from usage sites, `params`
    /// are fixed type params.
    Hkt {
        id: ClassId,
        elems: SmallVec<[T; 1]>,
        params: SmallVec<[T; 1]>,
    },
}

impl<T: Clone> TypeClass<T> {
    /// Preserves the HKT inner types while swapping the class tag.
    /// Returns `None` if `self` is not an HKT class.
    pub(crate) fn with_tag(&self, tag: ClassId) -> Option<Self> {
        match self {
            Self::Hkt { elems, params, .. } => Some(Self::Hkt {
                id: tag,
                elems: elems.clone(),
                params: params.clone(),
            }),
            _ => None,
        }
    }
}

impl<T> TypeClass<T> {
    pub(crate) fn simple(id: ClassId) -> Self {
        Self::Concrete {
            id,
            params: smallvec![],
        }
    }

    pub(crate) fn hkt(id: ClassId) -> Self {
        Self::Hkt {
            id,
            elems: smallvec![],
            params: smallvec![],
        }
    }

    pub(crate) fn hkt_elem(id: ClassId, e: T) -> Self {
        Self::Hkt {
            id,
            elems: smallvec![e],
            params: smallvec![],
        }
    }

    pub(crate) fn param(id: ClassId, p: T) -> Self {
        Self::Concrete {
            id,
            params: smallvec![p],
        }
    }

    /// Extract the class id for dispatch/lookup.
    pub(crate) fn tag(&self) -> ClassId {
        match self {
            Self::Concrete { id, .. } | Self::Hkt { id, .. } => *id,
        }
    }

    /// Map over inner types (for layer conversion).
    pub(crate) fn map<U>(self, mut f: impl FnMut(T) -> U) -> TypeClass<U> {
        match self {
            Self::Concrete { id, params } => TypeClass::Concrete {
                id,
                params: params.into_iter().map(&mut f).collect(),
            },
            Self::Hkt { id, elems, params } => TypeClass::Hkt {
                id,
                elems: elems.into_iter().map(&mut f).collect(),
                params: params.into_iter().map(&mut f).collect(),
            },
        }
    }

    /// Map over inner types fallibly (for layer conversion with `Result`).
    pub(crate) fn try_map<U, E>(
        self,
        mut f: impl FnMut(T) -> result::Result<U, E>,
    ) -> result::Result<TypeClass<U>, E> {
        match self {
            Self::Concrete { id, params } => Ok(TypeClass::Concrete {
                id,
                params: params
                    .into_iter()
                    .map(&mut f)
                    .collect::<result::Result<_, _>>()?,
            }),
            Self::Hkt { id, elems, params } => Ok(TypeClass::Hkt {
                id,
                elems: elems
                    .into_iter()
                    .map(&mut f)
                    .collect::<result::Result<_, _>>()?,
                params: params
                    .into_iter()
                    .map(&mut f)
                    .collect::<result::Result<_, _>>()?,
            }),
        }
    }

    /// Map over inner types by reference (no `Clone` bound needed).
    pub(crate) fn map_ref<U>(
        &self,
        mut f: impl FnMut(&T) -> U,
    ) -> TypeClass<U> {
        match self {
            Self::Concrete { id, params } => TypeClass::Concrete {
                id: *id,
                params: params.iter().map(&mut f).collect(),
            },
            Self::Hkt { id, elems, params } => TypeClass::Hkt {
                id: *id,
                elems: elems.iter().map(&mut f).collect(),
                params: params.iter().map(&mut f).collect(),
            },
        }
    }

    /// Construct from `tag` + `shape` + `args`; validates shape.
    pub(crate) fn from_tag(
        tag: ClassId,
        shape: ClassShape,
        args: SmallVec<[T; 1]>,
        span: Span,
    ) -> Result<Self, TypeError> {
        match shape {
            ClassShape::Concrete { params: 0 } => {
                if args.is_empty() {
                    Ok(Self::Concrete {
                        id: tag,
                        params: smallvec![],
                    })
                } else {
                    Err(TypeError::ClassRejectsArg { class: tag, span })
                }
            }
            ClassShape::Concrete { params: n } => {
                if args.len() == n as usize {
                    Ok(Self::Concrete {
                        id: tag,
                        params: args,
                    })
                } else {
                    Err(TypeError::ClassRequiresArg { class: tag, span })
                }
            }
            ClassShape::Hkt { params: n, .. } => {
                if args.len() == n as usize {
                    Ok(Self::Hkt {
                        id: tag,
                        elems: smallvec![],
                        params: args,
                    })
                } else if n == 0 {
                    Err(TypeError::ClassRejectsArg { class: tag, span })
                } else {
                    Err(TypeError::ClassRequiresArg { class: tag, span })
                }
            }
        }
    }
}

impl TypeClass<TyId> {
    /// Apply a local rename to any inner types.
    pub(crate) fn apply(&self, rename: &Rename, arena: &mut TyArena) -> Self {
        match self {
            Self::Concrete { id, params } => Self::Concrete {
                id: *id,
                params: params
                    .iter()
                    .map(|&p| arena.apply(p, rename))
                    .collect(),
            },
            Self::Hkt { id, elems, params } => Self::Hkt {
                id: *id,
                elems: elems.iter().map(|&e| arena.apply(e, rename)).collect(),
                params: params
                    .iter()
                    .map(|&p| arena.apply(p, rename))
                    .collect(),
            },
        }
    }

    /// Resolve inner types through the union-find.
    pub(crate) fn resolve_inner(
        &self,
        uf: &mut UnionFind,
        arena: &mut TyArena,
    ) -> Self {
        match self {
            Self::Concrete { id, params } => Self::Concrete {
                id: *id,
                params: params.iter().map(|&p| uf.resolve(p, arena)).collect(),
            },
            Self::Hkt { id, elems, params } => Self::Hkt {
                id: *id,
                elems: elems.iter().map(|&e| uf.resolve(e, arena)).collect(),
                params: params.iter().map(|&p| uf.resolve(p, arena)).collect(),
            },
        }
    }

    /// Collect free type variables from any inner types.
    ///
    /// Chases through UF bindings.
    pub(crate) fn free_vars(
        &self,
        arena: &TyArena,
        uf: &mut UnionFind,
    ) -> HashSet<TyVar> {
        match self {
            Self::Concrete { params, .. } => params
                .iter()
                .flat_map(|&p| uf.free_vars(p, arena))
                .collect(),
            Self::Hkt { elems, params, .. } => elems
                .iter()
                .chain(params.iter())
                .flat_map(|&t| uf.free_vars(t, arena))
                .collect(),
        }
    }

    /// Create a placeholder constraint for error messages.
    pub(crate) fn placeholder(tag: ClassId, shape: ClassShape) -> Self {
        match shape {
            ClassShape::Concrete { params: 0 } => Self::Concrete {
                id: tag,
                params: smallvec![],
            },
            ClassShape::Concrete { params: n } => Self::Concrete {
                id: tag,
                params: smallvec![TyArena::UNKNOWN; n as usize],
            },
            ClassShape::Hkt { params: n, .. } => Self::Hkt {
                id: tag,
                elems: smallvec![],
                params: smallvec![TyArena::UNKNOWN; n as usize],
            },
        }
    }
}

impl<T: fmt::Display> fmt::Display for TypeClass<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Concrete { id, params } if params.is_empty() => {
                write!(f, "{id}")
            }
            Self::Concrete { id, params } => {
                write!(f, "{id}[")?;
                params.iter().enumerate().try_for_each(|(i, p)| {
                    if i > 0 {
                        write!(f, ", {p}")
                    } else {
                        write!(f, "{p}")
                    }
                })?;
                write!(f, "]")
            }
            Self::Hkt { id, elems, params }
                if elems.is_empty() && params.is_empty() =>
            {
                write!(f, "{id}")
            }
            Self::Hkt { id, elems, params } => {
                write!(f, "{id}[")?;
                elems.iter().chain(params.iter()).enumerate().try_for_each(
                    |(i, t)| {
                        if i > 0 {
                            write!(f, ", {t}")
                        } else {
                            write!(f, "{t}")
                        }
                    },
                )?;
                write!(f, "]")
            }
        }
    }
}

/// A type variable; placeholder for an unknown type during inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TyVar(u32);

impl TyVar {
    /// Create a new type variable with the given index.
    pub(crate) const fn new(idx: u32) -> Self {
        Self(idx)
    }

    /// Get the index of this type variable.
    pub(crate) const fn idx(self) -> u32 {
        self.0
    }
}

/// Interned handle to a `Ty` stored in a `TyArena`.
///
/// `Copy` and `Eq`; eliminates deep cloning of recursive type trees.
/// Use `TyArena::get` to retrieve the underlying `Ty`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct TyId(u32);

impl TyId {
    /// Get the raw arena index.
    pub(crate) const fn idx(self) -> u32 {
        self.0
    }

    /// Construct from a raw index (test/debug use only).
    #[cfg(test)]
    pub(crate) const fn from_raw(idx: u32) -> Self {
        Self(idx)
    }
}

impl fmt::Display for TyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Static types used during type checking.
///
/// These include type variables (`Var`) for inference and structural object
/// types.
///
/// NOTE: `PartialEq` is derived intentionally; in particular, two `Union`s
/// with identical members but different provenance (`None` vs `Some(id)`)
/// are considered distinct. This is desired because provenance tracks
/// whether a union originated from a named type definition, which affects
/// display, error messages, and cast semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    /// Unresolved type variable (from inference).
    Var(TyVar),

    // Primitives
    Bool,
    Int,
    Word,
    Float,
    Char,
    String,
    Unit,
    Time,
    Range,
    Json,
    Ordering,
    DataStatus,
    FilePath,
    Path,
    Regex,
    RuntimeError,
    /// Local database variable reference (e.g., `data{1}`).
    ///
    /// Part of the builtin `Ref = Local | Global` union. See [`Self::is_ref`].
    Local,
    /// Global database variable reference (e.g., `^info{1}`).
    ///
    /// Part of the builtin `Ref = Local | Global` union. See [`Self::is_ref`].
    Global,

    // Parameterized builtins
    Array(TyId),
    Option(TyId),
    Result(TyId, TyId),
    Map(TyId, TyId),

    // Compound types
    Tuple(SmallVec<[TyId; 4]>),
    Fn(SmallVec<[TyId; 4]>, TyId),

    /// Anonymous structural record; compatible if fields match.
    Object(IndexMap<StringId, TyId>),

    /// Union type; value is one of the member types.
    ///
    /// The optional `TypeId` is the provenance: `Some(id)` for named unions
    /// (`Storable`, `Scalar`, user-defined `union`), `None` for anonymous
    /// inline unions (`Int | String`).
    ///
    /// Provenance preserves nominal identity for:
    /// - **`AS` semantics**: `Storable` has infallible `AS` casts that may
    ///   fail at runtime with `Error::RuntimeType`.
    /// - **Error messages**: named unions display their registered name
    ///   rather than the expanded member list.
    Union(Option<TypeId>, SmallVec<[TyId; 4]>),

    /// User-defined type (sum types, aliases) with type parameters.
    Named(TypeId, SmallVec<[TyId; 4]>),

    /// Higher-kinded type application: `F[U]` where `F` is a type variable.
    ///
    /// Used for polymorphism over type constructors. When `F: Fallible[T]`
    /// and `F` resolves to `Option[T]`, then `Apply(F, [U])` becomes `Option[U]`.
    /// For `Result[T, E]`, it becomes `Result[U, E]` (preserving error type).
    ///
    /// Resolved during substitution: when the type variable is bound to a
    /// concrete type constructor, the application is evaluated.
    Apply(TyVar, SmallVec<[TyId; 4]>),

    /// Associated type projection: `T.Index` where `T: Indexable[E]`.
    ///
    /// Represents a type that is determined by a class instance. For example,
    /// `Array[Int].Index` resolves to `Int`, `Map[String, Int].Index` resolves
    /// to `String`.
    ///
    /// Fields:
    /// - `TyVar`: the type variable with the class constraint
    /// - `ClassId`: which class defines the associated type
    /// - `StringId`: the associated type name (e.g., `"Index"`)
    AssocType(TyVar, ClassId, StringId),

    /// Unresolved; database reads before inference narrows.
    Unknown,

    /// Error recovery sentinel; unifies with anything.
    Error,
}

impl Hash for Ty {
    fn hash<H: Hasher>(&self, state: &mut H) {
        mem::discriminant(self).hash(state);
        match self {
            Self::Var(v) => v.hash(state),
            Self::Array(id) => id.hash(state),
            Self::Option(id) => id.hash(state),
            Self::Result(a, b) => {
                a.hash(state);
                b.hash(state);
            }
            Self::Map(k, v) => {
                k.hash(state);
                v.hash(state);
            }
            Self::Tuple(ts) => ts.hash(state),
            Self::Fn(params, ret) => {
                params.hash(state);
                ret.hash(state);
            }
            Self::Object(fields) => {
                // Sort by key so hash is consistent with `PartialEq`
                // (which ignores `IndexMap` insertion order).
                let mut pairs: Vec<_> = fields.iter().collect();
                pairs.sort_by_key(|(&k, _)| k);
                pairs.hash(state);
            }
            Self::Union(prov, ms) => {
                prov.hash(state);
                ms.hash(state);
            }
            Self::Named(id, args) => {
                id.hash(state);
                args.hash(state);
            }
            Self::Apply(v, args) => {
                v.hash(state);
                args.hash(state);
            }
            Self::AssocType(v, cls, name) => {
                v.hash(state);
                cls.hash(state);
                name.hash(state);
            }
            // Primitives: discriminant already hashed above.
            Self::Bool
            | Self::Int
            | Self::Word
            | Self::Float
            | Self::Char
            | Self::String
            | Self::Unit
            | Self::Time
            | Self::Range
            | Self::Json
            | Self::Ordering
            | Self::DataStatus
            | Self::FilePath
            | Self::Path
            | Self::Regex
            | Self::RuntimeError
            | Self::Local
            | Self::Global
            | Self::Unknown
            | Self::Error => {}
        }
    }
}

impl Ty {
    /// Check if this type is a database reference type.
    ///
    /// Returns `true` for `Local`, `Global`, or the `Ref` union.
    pub(crate) fn is_ref(&self) -> bool {
        matches!(
            self,
            Self::Local | Self::Global | Self::Union(Some(TypeId::REF), _)
        )
    }
}

/// Arena for interned types. All `Ty` values live here; consumers hold
/// lightweight `TyId` handles. Primitives are pre-interned at known indices.
#[derive(Clone)]
pub(crate) struct TyArena {
    tys: Vec<Ty>,
    /// Reverse lookup for deduplication; ensures `==` on `TyId` is structural.
    index: HashMap<Ty, TyId>,
}

impl TyArena {
    // Pre-interned primitive `TyId`s (order must match `new()`).
    pub(crate) const BOOL: TyId = TyId(0);
    pub(crate) const INT: TyId = TyId(1);
    pub(crate) const WORD: TyId = TyId(2);
    pub(crate) const FLOAT: TyId = TyId(3);
    pub(crate) const CHAR: TyId = TyId(4);
    pub(crate) const STRING: TyId = TyId(5);
    pub(crate) const UNIT: TyId = TyId(6);
    pub(crate) const TIME: TyId = TyId(7);
    pub(crate) const RANGE: TyId = TyId(8);
    pub(crate) const JSON: TyId = TyId(9);
    pub(crate) const ORDERING: TyId = TyId(10);
    pub(crate) const DATA_STATUS: TyId = TyId(11);
    pub(crate) const FILEPATH: TyId = TyId(12);
    pub(crate) const PATH: TyId = TyId(13);
    pub(crate) const REGEX: TyId = TyId(14);
    pub(crate) const RUNTIME_ERROR: TyId = TyId(15);
    pub(crate) const LOCAL: TyId = TyId(16);
    pub(crate) const GLOBAL: TyId = TyId(17);
    pub(crate) const UNKNOWN: TyId = TyId(18);
    pub(crate) const ERROR: TyId = TyId(19);

    // Pre-interned builtin union `TyId`s (order must match `new()`).
    pub(crate) const STORABLE: TyId = TyId(20);
    pub(crate) const SCALAR: TyId = TyId(21);
    pub(crate) const SUBSCRIPT: TyId = TyId(22);
    pub(crate) const REF: TyId = TyId(23);

    /// Member types of the `Storable` union.
    pub(crate) const STORABLE_MEMBERS: &'static [TyId] = &[
        Self::BOOL,
        Self::INT,
        Self::FLOAT,
        Self::CHAR,
        Self::STRING,
        Self::JSON,
    ];

    /// Member types of the `Scalar` union.
    pub(crate) const SCALAR_MEMBERS: &'static [TyId] =
        &[Self::BOOL, Self::INT, Self::FLOAT, Self::STRING];

    /// Member types of the `Subscript` union.
    pub(crate) const SUBSCRIPT_MEMBERS: &'static [TyId] = &[
        Self::BOOL,
        Self::INT,
        Self::FLOAT,
        Self::CHAR,
        Self::STRING,
        Self::JSON,
    ];

    /// Member types of the `Ref` union.
    pub(crate) const REF_MEMBERS: &'static [TyId] =
        &[Self::LOCAL, Self::GLOBAL];

    pub(crate) fn new() -> Self {
        let tys = vec![
            Ty::Bool,         // 0
            Ty::Int,          // 1
            Ty::Word,         // 2
            Ty::Float,        // 3
            Ty::Char,         // 4
            Ty::String,       // 5
            Ty::Unit,         // 6
            Ty::Time,         // 7
            Ty::Range,        // 8
            Ty::Json,         // 9
            Ty::Ordering,     // 10
            Ty::DataStatus,   // 11
            Ty::FilePath,     // 12
            Ty::Path,         // 13
            Ty::Regex,        // 14
            Ty::RuntimeError, // 15
            Ty::Local,        // 16
            Ty::Global,       // 17
            Ty::Unknown,      // 18
            Ty::Error,        // 19
            Ty::Union(
                Some(TypeId::STORABLE),
                Self::STORABLE_MEMBERS.iter().copied().collect(),
            ), // 20
            Ty::Union(
                Some(TypeId::SCALAR),
                Self::SCALAR_MEMBERS.iter().copied().collect(),
            ), // 21
            Ty::Union(
                Some(TypeId::SUBSCRIPT),
                Self::SUBSCRIPT_MEMBERS.iter().copied().collect(),
            ), // 22
            Ty::Union(
                Some(TypeId::REF),
                Self::REF_MEMBERS.iter().copied().collect(),
            ), // 23
        ];
        let index = tys
            .iter()
            .enumerate()
            .map(|(i, ty)| (ty.clone(), TyId(i as u32)))
            .collect();
        Self { tys, index }
    }

    /// Allocate a type, deduplicating so structurally equal types share a `TyId`.
    pub(crate) fn alloc(&mut self, ty: Ty) -> TyId {
        if let Some(&id) = self.index.get(&ty) {
            id
        } else {
            let id = TyId(self.tys.len() as u32);
            self.index.insert(ty.clone(), id);
            self.tys.push(ty);
            id
        }
    }

    /// Retrieve the `Ty` for a handle.
    pub(crate) fn get(&self, id: TyId) -> &Ty {
        &self.tys[id.0 as usize]
    }

    // --- Convenience constructors ---

    /// Allocate `Ty::Var(TyVar::new(idx))`.
    pub(crate) fn var(&mut self, idx: u32) -> TyId {
        self.alloc(Ty::Var(TyVar::new(idx)))
    }

    /// Allocate `Ty::Fn(params, ret)`.
    pub(crate) fn func(
        &mut self,
        params: SmallVec<[TyId; 4]>,
        ret: TyId,
    ) -> TyId {
        self.alloc(Ty::Fn(params, ret))
    }

    /// Allocate `Ty::Array(elem)`.
    pub(crate) fn array(&mut self, elem: TyId) -> TyId {
        self.alloc(Ty::Array(elem))
    }

    /// Allocate `Ty::Option(inner)`.
    pub(crate) fn option(&mut self, inner: TyId) -> TyId {
        self.alloc(Ty::Option(inner))
    }

    /// Allocate `Ty::Result(ok, err)`.
    pub(crate) fn result(&mut self, ok: TyId, err: TyId) -> TyId {
        self.alloc(Ty::Result(ok, err))
    }

    /// Allocate `Ty::Map(k, v)`.
    pub(crate) fn map_ty(&mut self, k: TyId, v: TyId) -> TyId {
        self.alloc(Ty::Map(k, v))
    }

    /// Allocate `Ty::Apply(tv, args)`.
    pub(crate) fn hkt(&mut self, tv: TyVar, args: SmallVec<[TyId; 4]>) -> TyId {
        self.alloc(Ty::Apply(tv, args))
    }

    /// Allocate `Ty::Named(id, args)`.
    pub(crate) fn named(
        &mut self,
        id: TypeId,
        args: SmallVec<[TyId; 4]>,
    ) -> TyId {
        self.alloc(Ty::Named(id, args))
    }

    /// Pre-interned `TyId` for the `Storable` union.
    pub(crate) fn storable(&self) -> TyId {
        Self::STORABLE
    }

    /// Pre-interned `TyId` for the `Scalar` union.
    pub(crate) fn scalar(&self) -> TyId {
        Self::SCALAR
    }

    /// Pre-interned `TyId` for the `Subscript` union.
    pub(crate) fn subscript(&self) -> TyId {
        Self::SUBSCRIPT
    }

    /// Pre-interned `TyId` for the `Ref` union.
    pub(crate) fn ref_ty(&self) -> TyId {
        Self::REF
    }

    // --- Recursive operations ---

    /// Check if type variable `v` occurs anywhere in the type tree.
    pub(crate) fn occurs(&self, id: TyId, v: TyVar) -> bool {
        match self.get(id) {
            Ty::Var(w) => *w == v,
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Time
            | Ty::Range
            | Ty::Json
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::Regex
            | Ty::RuntimeError
            | Ty::Local
            | Ty::Global
            | Ty::Unknown
            | Ty::Error => false,
            Ty::Array(t) | Ty::Option(t) => self.occurs(*t, v),
            Ty::Result(ok, err) => self.occurs(*ok, v) || self.occurs(*err, v),
            Ty::Map(k, val) => self.occurs(*k, v) || self.occurs(*val, v),
            Ty::Tuple(ts) => ts.iter().any(|&t| self.occurs(t, v)),
            Ty::Fn(params, ret) => {
                params.iter().any(|&t| self.occurs(t, v))
                    || self.occurs(*ret, v)
            }
            Ty::Object(fields) => fields.values().any(|&t| self.occurs(t, v)),
            Ty::Union(_, members) => members.iter().any(|&t| self.occurs(t, v)),
            Ty::Named(_, args) => args.iter().any(|&t| self.occurs(t, v)),
            Ty::Apply(w, args) => {
                *w == v || args.iter().any(|&t| self.occurs(t, v))
            }
            Ty::AssocType(w, _, _) => *w == v,
        }
    }

    /// Occurs check that chases through union-find bindings.
    ///
    /// Like `occurs`, but for each `Ty::Var(w)`, follows UF links: if `w`
    /// is bound to a type, recurses into that type; if unbound, checks
    /// canonical root against `v`.
    pub(crate) fn occurs_uf(
        &self,
        id: TyId,
        v: TyVar,
        uf: &mut UnionFind,
    ) -> bool {
        match self.get(id) {
            Ty::Var(w) => {
                let root = uf.find(*w);
                match uf.probe(root) {
                    Some(bound) => self.occurs_uf(bound, v, uf),
                    None => root == v,
                }
            }
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Time
            | Ty::Range
            | Ty::Json
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::Regex
            | Ty::RuntimeError
            | Ty::Local
            | Ty::Global
            | Ty::Unknown
            | Ty::Error => false,
            Ty::Array(t) | Ty::Option(t) => self.occurs_uf(*t, v, uf),
            Ty::Result(ok, err) => {
                self.occurs_uf(*ok, v, uf) || self.occurs_uf(*err, v, uf)
            }
            Ty::Map(k, val) => {
                self.occurs_uf(*k, v, uf) || self.occurs_uf(*val, v, uf)
            }
            Ty::Tuple(ts) => ts.iter().any(|&t| self.occurs_uf(t, v, uf)),
            Ty::Fn(params, ret) => {
                params.iter().any(|&t| self.occurs_uf(t, v, uf))
                    || self.occurs_uf(*ret, v, uf)
            }
            Ty::Object(fields) => {
                fields.values().any(|&t| self.occurs_uf(t, v, uf))
            }
            Ty::Union(_, members) => {
                members.iter().any(|&t| self.occurs_uf(t, v, uf))
            }
            Ty::Named(_, args) => {
                args.iter().any(|&t| self.occurs_uf(t, v, uf))
            }
            Ty::Apply(w, args) => {
                let root = uf.find(*w);
                match uf.probe(root) {
                    Some(bound) => {
                        self.occurs_uf(bound, v, uf)
                            || args.iter().any(|&t| self.occurs_uf(t, v, uf))
                    }
                    None => {
                        root == v
                            || args.iter().any(|&t| self.occurs_uf(t, v, uf))
                    }
                }
            }
            Ty::AssocType(w, _, _) => {
                let root = uf.find(*w);
                match uf.probe(root) {
                    Some(bound) => self.occurs_uf(bound, v, uf),
                    None => root == v,
                }
            }
        }
    }

    /// Collect all free type variables in a type.
    pub(crate) fn free_vars(&self, id: TyId) -> HashSet<TyVar> {
        let mut acc = HashSet::new();
        self.collect_free_vars(id, &mut acc);
        acc
    }

    fn collect_free_vars(&self, id: TyId, acc: &mut HashSet<TyVar>) {
        match self.get(id) {
            Ty::Var(v) => {
                acc.insert(*v);
            }
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Time
            | Ty::Range
            | Ty::Json
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::Regex
            | Ty::RuntimeError
            | Ty::Local
            | Ty::Global
            | Ty::Unknown
            | Ty::Error => {}
            Ty::Array(t) | Ty::Option(t) => {
                self.collect_free_vars(*t, acc);
            }
            Ty::Result(ok, err) => {
                self.collect_free_vars(*ok, acc);
                self.collect_free_vars(*err, acc);
            }
            Ty::Map(k, v) => {
                self.collect_free_vars(*k, acc);
                self.collect_free_vars(*v, acc);
            }
            Ty::Tuple(ts) => {
                ts.iter().for_each(|&t| self.collect_free_vars(t, acc));
            }
            Ty::Fn(params, ret) => {
                params.iter().for_each(|&t| self.collect_free_vars(t, acc));
                self.collect_free_vars(*ret, acc);
            }
            Ty::Object(fields) => {
                fields
                    .values()
                    .for_each(|&t| self.collect_free_vars(t, acc));
            }
            Ty::Union(_, members) => {
                members.iter().for_each(|&t| self.collect_free_vars(t, acc));
            }
            Ty::Named(_, args) => {
                args.iter().for_each(|&t| self.collect_free_vars(t, acc));
            }
            Ty::Apply(v, args) => {
                acc.insert(*v);
                args.iter().for_each(|&t| self.collect_free_vars(t, acc));
            }
            Ty::AssocType(v, _, _) => {
                acc.insert(*v);
            }
        }
    }

    /// Apply a local rename, returning a (possibly new) `TyId`.
    ///
    /// Returns the original `id` when no rename applies.
    pub(crate) fn apply(&mut self, id: TyId, rename: &Rename) -> TyId {
        if rename.is_empty() {
            id
        } else {
            // Clone the shallow `Ty` to avoid borrow conflicts with
            // recursive `&mut self` calls. Cheap: variants now hold
            // `TyId` (Copy) / `SmallVec<[TyId; 4]>`.
            let ty = self.get(id).clone();
            self.apply_inner(id, ty, rename)
        }
    }

    fn apply_inner(&mut self, id: TyId, ty: Ty, rename: &Rename) -> TyId {
        match ty {
            Ty::Var(v) => {
                rename.0.get(&v).map_or(id, |&t| self.apply(t, rename))
            }
            // Primitives: no change
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Time
            | Ty::Range
            | Ty::Json
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::Regex
            | Ty::RuntimeError
            | Ty::Local
            | Ty::Global
            | Ty::Unknown
            | Ty::Error => id,
            Ty::Array(inner) => {
                let n = self.apply(inner, rename);
                if n == inner {
                    id
                } else {
                    self.alloc(Ty::Array(n))
                }
            }
            Ty::Option(inner) => {
                let n = self.apply(inner, rename);
                if n == inner {
                    id
                } else {
                    self.alloc(Ty::Option(n))
                }
            }
            Ty::Result(ok, err) => {
                let nok = self.apply(ok, rename);
                let nerr = self.apply(err, rename);
                if nok == ok && nerr == err {
                    id
                } else {
                    self.alloc(Ty::Result(nok, nerr))
                }
            }
            Ty::Map(k, v) => {
                let nk = self.apply(k, rename);
                let nv = self.apply(v, rename);
                if nk == k && nv == v {
                    id
                } else {
                    self.alloc(Ty::Map(nk, nv))
                }
            }
            Ty::Tuple(ref ts) => {
                let nts: SmallVec<[TyId; 4]> =
                    ts.iter().map(|&t| self.apply(t, rename)).collect();
                if nts == *ts {
                    id
                } else {
                    self.alloc(Ty::Tuple(nts))
                }
            }
            Ty::Fn(ref params, ret) => {
                let np: SmallVec<[TyId; 4]> =
                    params.iter().map(|&t| self.apply(t, rename)).collect();
                let nr = self.apply(ret, rename);
                if np == *params && nr == ret {
                    id
                } else {
                    self.alloc(Ty::Fn(np, nr))
                }
            }
            Ty::Object(ref fields) => {
                let nf: IndexMap<StringId, TyId> = fields
                    .iter()
                    .map(|(&k, &t)| (k, self.apply(t, rename)))
                    .collect();
                if nf == *fields {
                    id
                } else {
                    self.alloc(Ty::Object(nf))
                }
            }
            Ty::Union(prov, ref members) => {
                let nm: SmallVec<[TyId; 4]> =
                    members.iter().map(|&t| self.apply(t, rename)).collect();
                if nm == *members {
                    id
                } else {
                    self.alloc(Ty::Union(prov, nm))
                }
            }
            Ty::Named(type_id, ref args) => {
                let na: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.apply(t, rename)).collect();
                if na == *args {
                    id
                } else {
                    self.alloc(Ty::Named(type_id, na))
                }
            }
            Ty::Apply(v, ref args) => {
                let na: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.apply(t, rename)).collect();
                // Resolve the type variable through the rename chain
                let ctor_id = rename.0.get(&v).map(|&t| self.apply(t, rename));
                match ctor_id {
                    None => {
                        if na == *args {
                            id
                        } else {
                            self.alloc(Ty::Apply(v, na))
                        }
                    }
                    Some(cid) => {
                        let ctor = self.get(cid).clone();
                        match ctor {
                            Ty::Var(w) => self.alloc(Ty::Apply(w, na)),
                            Ty::Option(_) => {
                                na.first().map_or(Self::ERROR, |&a| {
                                    self.alloc(Ty::Option(a))
                                })
                            }
                            Ty::Result(_, e) => match na.len() {
                                1 => na.first().map_or(Self::ERROR, |&a| {
                                    self.alloc(Ty::Result(a, e))
                                }),
                                _ => match (na.first(), na.get(1)) {
                                    (Some(&a), Some(&b)) => {
                                        self.alloc(Ty::Result(a, b))
                                    }
                                    _ => Self::ERROR,
                                },
                            },
                            Ty::Array(_) => {
                                na.first().map_or(Self::ERROR, |&a| {
                                    self.alloc(Ty::Array(a))
                                })
                            }
                            Ty::Map(_, mv) => match na.len() {
                                1 => na.first().map_or(Self::ERROR, |&a| {
                                    self.alloc(Ty::Map(a, mv))
                                }),
                                _ => match (na.first(), na.get(1)) {
                                    (Some(&a), Some(&b)) => {
                                        self.alloc(Ty::Map(a, b))
                                    }
                                    _ => Self::ERROR,
                                },
                            },
                            // `Named`: element positions are always a suffix;
                            // preserve the fixed prefix and replace the tail
                            Ty::Named(tid, orig) => {
                                let keep = orig.len().saturating_sub(na.len());
                                let new_args: SmallVec<[TyId; 4]> = orig
                                    .iter()
                                    .take(keep)
                                    .chain(na.iter())
                                    .copied()
                                    .collect();
                                self.alloc(Ty::Named(tid, new_args))
                            }
                            Ty::Tuple(ts) => {
                                let mut na_iter = na.iter().copied();
                                let filled: SmallVec<[TyId; 4]> = ts
                                    .iter()
                                    .map(|&t| {
                                        if t == Self::ERROR {
                                            na_iter
                                                .next()
                                                .unwrap_or(Self::ERROR)
                                        } else {
                                            t
                                        }
                                    })
                                    .collect();
                                self.alloc(Ty::Tuple(filled))
                            }
                            Ty::Union(prov, members) => {
                                let filled: SmallVec<[TyId; 4]> = members
                                    .iter()
                                    .map(|&m| match self.get(m).clone() {
                                        Ty::Option(_) => na
                                            .first()
                                            .map_or(Self::ERROR, |&a| {
                                                self.alloc(Ty::Option(a))
                                            }),
                                        Ty::Result(_, e) => match na.len() {
                                            1 => na.first().map_or(
                                                Self::ERROR,
                                                |&a| {
                                                    self.alloc(Ty::Result(a, e))
                                                },
                                            ),
                                            _ => {
                                                match (na.first(), na.get(1)) {
                                                    (Some(&a), Some(&b)) => {
                                                        self.alloc(Ty::Result(
                                                            a, b,
                                                        ))
                                                    }
                                                    _ => Self::ERROR,
                                                }
                                            }
                                        },
                                        Ty::Array(_) => na
                                            .first()
                                            .map_or(Self::ERROR, |&a| {
                                                self.alloc(Ty::Array(a))
                                            }),
                                        Ty::Map(_, mv) => match na.len() {
                                            1 => na
                                                .first()
                                                .map_or(Self::ERROR, |&a| {
                                                    self.alloc(Ty::Map(a, mv))
                                                }),
                                            _ => {
                                                match (na.first(), na.get(1)) {
                                                    (Some(&a), Some(&b)) => {
                                                        self.alloc(Ty::Map(
                                                            a, b,
                                                        ))
                                                    }
                                                    _ => Self::ERROR,
                                                }
                                            }
                                        },
                                        Ty::Range => Self::RANGE,
                                        Ty::Named(tid, orig) => {
                                            let keep = orig
                                                .len()
                                                .saturating_sub(na.len());
                                            let new_args: SmallVec<[TyId; 4]> =
                                                orig.iter()
                                                    .take(keep)
                                                    .chain(na.iter())
                                                    .copied()
                                                    .collect();
                                            self.alloc(Ty::Named(tid, new_args))
                                        }
                                        Ty::Tuple(ts) => {
                                            let mut na_iter =
                                                na.iter().copied();
                                            let filled: SmallVec<[TyId; 4]> =
                                                ts.iter()
                                                    .map(|&t| {
                                                        if t == Self::ERROR {
                                                            na_iter
                                                                .next()
                                                                .unwrap_or(
                                                                    Self::ERROR,
                                                                )
                                                        } else {
                                                            t
                                                        }
                                                    })
                                                    .collect();
                                            self.alloc(Ty::Tuple(filled))
                                        }
                                        _ => Self::ERROR,
                                    })
                                    .collect();
                                if filled.contains(&Self::ERROR) {
                                    Self::ERROR
                                } else {
                                    self.alloc(Ty::Union(prov, filled))
                                }
                            }
                            _ if na.is_empty() => cid,
                            _ => Self::ERROR,
                        }
                    }
                }
            }
            Ty::AssocType(v, class, name) => match rename.0.get(&v) {
                Some(&vid) => {
                    let resolved = self.get(vid).clone();
                    match resolved {
                        Ty::Var(w) => self.alloc(Ty::AssocType(w, class, name)),
                        _ => id,
                    }
                }
                None => id,
            },
        }
    }
}

/// A polymorphic type scheme: `forall vars. ty`.
///
/// For example, `forall a. Array[a] -> Int` is the scheme for `Iter.length`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scheme {
    /// Universally quantified type variables.
    pub(crate) vars: SmallVec<[TyVar; 4]>,
    /// The body type (may contain the quantified variables).
    pub(crate) ty: TyId,
    /// User-specified class constraints on type variables.
    ///
    /// These are re-emitted when the scheme is instantiated at call sites.
    /// Each tuple is `(type_var, class)`. Parameterized classes carry any
    /// fixed type arguments directly.
    pub(crate) constraints: SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
}

impl Scheme {
    /// Create a monomorphic scheme (no quantified variables).
    pub(crate) fn mono(ty: TyId) -> Self {
        Self {
            vars: SmallVec::new(),
            ty,
            constraints: SmallVec::new(),
        }
    }

    /// Polymorphic with 1 type variable: `forall T. ...`
    pub(crate) fn poly(
        arena: &mut TyArena,
        f: impl FnOnce(TyId, &mut TyArena) -> TyId,
    ) -> Self {
        let t = arena.var(0);
        Self {
            vars: smallvec![TyVar(0)],
            ty: f(t, arena),
            constraints: SmallVec::new(),
        }
    }

    /// Polymorphic with 2 type variables: `forall T U. ...`
    pub(crate) fn poly2(
        arena: &mut TyArena,
        f: impl FnOnce(TyId, TyId, &mut TyArena) -> TyId,
    ) -> Self {
        let t = arena.var(0);
        let u = arena.var(1);
        Self {
            vars: smallvec![TyVar(0), TyVar(1)],
            ty: f(t, u, arena),
            constraints: SmallVec::new(),
        }
    }

    /// Polymorphic with 3 type variables: `forall T U V. ...`
    pub(crate) fn poly3(
        arena: &mut TyArena,
        f: impl FnOnce(TyId, TyId, TyId, &mut TyArena) -> TyId,
    ) -> Self {
        let t = arena.var(0);
        let u = arena.var(1);
        let v = arena.var(2);
        Self {
            vars: smallvec![TyVar(0), TyVar(1), TyVar(2)],
            ty: f(t, u, v, arena),
            constraints: SmallVec::new(),
        }
    }

    /// Extract the return type if the scheme body is a function type.
    pub(crate) fn return_ty(&self, arena: &TyArena) -> Option<TyId> {
        match arena.get(self.ty) {
            Ty::Fn(_, ret) => Some(*ret),
            _ => None,
        }
    }

    /// Extract the parameter types if the scheme body is a function type.
    pub(crate) fn params<'a>(
        &self,
        arena: &'a TyArena,
    ) -> Option<&'a SmallVec<[TyId; 4]>> {
        match arena.get(self.ty) {
            Ty::Fn(params, _) => Some(params),
            _ => None,
        }
    }

    /// Get the arity (number of parameters) if the scheme body is a function type.
    pub(crate) fn arity(&self, arena: &TyArena) -> Option<usize> {
        self.params(arena).map(SmallVec::len)
    }

    /// Instantiate the scheme with fresh type variables.
    ///
    /// Allocates fresh `TyVar`s via the union-find. Returns:
    /// - The `TyId` with all quantified variables replaced by fresh ones
    /// - The class constraints with type variables substituted
    pub(crate) fn instantiate(
        &self,
        uf: &mut UnionFind,
        arena: &mut TyArena,
    ) -> (TyId, SmallVec<[(TyId, TypeClass<TyId>); 2]>) {
        let (ty, cs, _) = self.instantiate_tracked(uf, arena);
        (ty, cs)
    }

    /// Like `instantiate`, but also returns the old-var -> new-var mapping.
    ///
    /// Used during forward-ref replay to record which scheme vars map to
    /// which fresh vars, enabling virtual edges in `enrich_finalized_schemes`.
    pub(crate) fn instantiate_tracked(
        &self,
        uf: &mut UnionFind,
        arena: &mut TyArena,
    ) -> (
        TyId,
        SmallVec<[(TyId, TypeClass<TyId>); 2]>,
        SmallVec<[(TyVar, TyVar); 4]>,
    ) {
        if self.vars.is_empty() {
            (self.ty, SmallVec::new(), SmallVec::new())
        } else {
            // Ensure fresh vars don't overlap with scheme vars to avoid
            // infinite loops during `apply` (which recursively substitutes)
            let max_scheme =
                self.vars.iter().map(|v| v.idx()).max().unwrap_or(0);
            uf.reserve_through(max_scheme);

            let mut var_map: SmallVec<[(TyVar, TyVar); 4]> = SmallVec::new();
            let rename = Rename(
                self.vars
                    .iter()
                    .map(|v| {
                        let fresh = uf.fresh();
                        var_map.push((*v, fresh));
                        (*v, arena.alloc(Ty::Var(fresh)))
                    })
                    .collect(),
            );
            let ty = arena.apply(self.ty, &rename);
            let constraints = self
                .constraints
                .iter()
                .map(|(v, class)| {
                    let ty = rename
                        .0
                        .get(v)
                        .copied()
                        .unwrap_or_else(|| arena.alloc(Ty::Var(*v)));
                    let class = class.apply(&rename, arena);
                    (ty, class)
                })
                .collect();
            (ty, constraints, var_map)
        }
    }

    /// Collect free type variables (excludes quantified variables).
    ///
    /// Chases through UF bindings so that bound variables are not
    /// reported as free.
    pub(crate) fn free_vars(
        &self,
        arena: &TyArena,
        uf: &mut UnionFind,
    ) -> HashSet<TyVar> {
        let mut fv = uf.free_vars(self.ty, arena);
        self.constraints.iter().for_each(|(v, class)| {
            uf.free_vars_for_var(*v, arena).into_iter().for_each(|v| {
                fv.insert(v);
            });
            class
                .free_vars(arena, uf)
                .into_iter()
                .map(|v| uf.find(v))
                .for_each(|v| {
                    fv.insert(v);
                });
        });
        self.vars.iter().for_each(|v| {
            fv.remove(&uf.find(*v));
        });
        fv
    }
}

/// A local type variable rename; used for alpha-renaming in scheme
/// instantiation and instance constraint checking, not for global
/// constraint solving (which uses `UnionFind`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rename(pub(crate) HashMap<TyVar, TyId>);

impl Rename {
    /// Rename mapping a single variable.
    pub(crate) fn singleton(v: TyVar, ty: TyId) -> Self {
        Self(iter::once((v, ty)).collect())
    }

    /// Check if this rename is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_vars_primitive() {
        let a = TyArena::new();
        assert!(a.free_vars(TyArena::INT).is_empty());
        assert!(a.free_vars(TyArena::BOOL).is_empty());
    }

    #[test]
    fn free_vars_var() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let id = a.var(0);
        let fv = a.free_vars(id);
        assert!(fv.contains(&v));
        assert_eq!(fv.len(), 1);
    }

    #[test]
    fn free_vars_array() {
        let mut a = TyArena::new();
        let v = TyVar::new(1);
        let inner = a.alloc(Ty::Var(v));
        let arr = a.array(inner);
        let fv = a.free_vars(arr);
        assert!(fv.contains(&v));
    }

    #[test]
    fn free_vars_fn() {
        let mut a = TyArena::new();
        let va = TyVar::new(0);
        let vb = TyVar::new(1);
        let pa = a.var(0);
        let rb = a.var(1);
        let f = a.func(smallvec![pa], rb);
        let fv = a.free_vars(f);
        assert!(fv.contains(&va));
        assert!(fv.contains(&vb));
        assert_eq!(fv.len(), 2);
    }

    #[test]
    fn occurs_check() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        assert!(a.occurs(vid, v));
        assert!(!a.occurs(TyArena::INT, v));
        let arr_v = a.array(vid);
        assert!(a.occurs(arr_v, v));
        let arr_int = a.array(TyArena::INT);
        assert!(!a.occurs(arr_int, v));
    }

    #[test]
    fn apply_rename_var() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let rename = Rename::singleton(v, TyArena::INT);
        let res = a.apply(vid, &rename);
        assert_eq!(res, TyArena::INT);
    }

    #[test]
    fn apply_rename_nested() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let arr = a.array(vid);
        let rename = Rename::singleton(v, TyArena::STRING);
        let res = a.apply(arr, &rename);
        let applied_vid = a.apply(vid, &rename);
        assert_eq!(*a.get(res), Ty::Array(applied_vid));
    }

    #[test]
    fn apply_rename_no_match() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let _w = TyVar::new(1);
        let wid = a.var(1);
        let rename = Rename::singleton(v, TyArena::INT);
        let res = a.apply(wid, &rename);
        // No rename for `w`; should return same id
        assert_eq!(res, wid);
    }

    #[test]
    fn scheme_mono() {
        let s = Scheme::mono(TyArena::INT);
        assert!(s.vars.is_empty());
        assert_eq!(s.ty, TyArena::INT);
    }

    #[test]
    fn scheme_instantiate() {
        let mut a = TyArena::new();
        let mut uf = UnionFind::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let arr = a.array(vid);
        let s = Scheme {
            vars: smallvec![v],
            ty: arr,
            constraints: SmallVec::new(),
        };
        let (inst, constraints) = s.instantiate(&mut uf, &mut a);
        // Scheme var is `0`, so `reserve_through(0)` pads to len `1`.
        // Fresh var is `1`.
        match a.get(inst) {
            Ty::Array(inner) => match a.get(*inner) {
                Ty::Var(tv) => assert_eq!(tv.idx(), 1),
                other => panic!("expected Var, got {other:?}"),
            },
            other => panic!("expected Array, got {other:?}"),
        }
        assert!(constraints.is_empty());
    }

    #[test]
    fn scheme_free_vars_excludes_bound() {
        let mut a = TyArena::new();
        let va = TyVar::new(0);
        let vb = TyVar::new(1);
        let pa = a.var(0);
        let rb = a.var(1);
        let f = a.func(smallvec![pa], rb);
        let s = Scheme {
            vars: smallvec![va],
            ty: f,
            constraints: SmallVec::new(),
        };
        let mut uf = UnionFind::new();
        uf.reserve_through(1);
        let fv = s.free_vars(&a, &mut uf);
        assert!(!fv.contains(&va)); // bound
        assert!(fv.contains(&vb)); // free
    }

    // --- Union type tests ---

    #[test]
    fn union_free_vars_empty() {
        let mut a = TyArena::new();
        let u =
            a.alloc(Ty::Union(None, smallvec![TyArena::INT, TyArena::STRING]));
        assert!(a.free_vars(u).is_empty());
    }

    #[test]
    fn union_free_vars_with_var() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let u = a.alloc(Ty::Union(
            None,
            smallvec![TyArena::INT, vid, TyArena::STRING],
        ));
        let fv = a.free_vars(u);
        assert!(fv.contains(&v));
        assert_eq!(fv.len(), 1);
    }

    #[test]
    fn union_free_vars_multiple_vars() {
        let mut a = TyArena::new();
        let va = TyVar::new(0);
        let vb = TyVar::new(1);
        let aid = a.var(0);
        let bid = a.var(1);
        let u = a.alloc(Ty::Union(None, smallvec![aid, bid]));
        let fv = a.free_vars(u);
        assert!(fv.contains(&va));
        assert!(fv.contains(&vb));
        assert_eq!(fv.len(), 2);
    }

    #[test]
    fn union_occurs_positive() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let u = a.alloc(Ty::Union(None, smallvec![TyArena::INT, vid]));
        assert!(a.occurs(u, v));
    }

    #[test]
    fn union_occurs_negative() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let u =
            a.alloc(Ty::Union(None, smallvec![TyArena::INT, TyArena::STRING]));
        assert!(!a.occurs(u, v));
    }

    #[test]
    fn union_occurs_nested() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let arr = a.array(vid);
        let u = a.alloc(Ty::Union(None, smallvec![TyArena::INT, arr]));
        assert!(a.occurs(u, v));
    }

    #[test]
    fn union_apply_rename() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let u = a.alloc(Ty::Union(None, smallvec![TyArena::INT, vid]));
        let rename = Rename::singleton(v, TyArena::BOOL);
        let res = a.apply(u, &rename);
        match a.get(res) {
            Ty::Union(_, ms) => {
                assert_eq!(ms.len(), 2);
                assert_eq!(ms[0], TyArena::INT);
                assert_eq!(ms[1], TyArena::BOOL);
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn union_apply_rename_no_match() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let wid = a.var(1);
        let u = a.alloc(Ty::Union(None, smallvec![TyArena::INT, wid]));
        let rename = Rename::singleton(v, TyArena::BOOL);
        let res = a.apply(u, &rename);
        // No change; `w` not in rename
        assert_eq!(res, u);
    }

    #[test]
    fn union_apply_rename_nested() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let opt = a.option(vid);
        let u = a.alloc(Ty::Union(None, smallvec![TyArena::INT, opt]));
        let rename = Rename::singleton(v, TyArena::STRING);
        let res = a.apply(u, &rename);
        match a.get(res) {
            Ty::Union(_, ms) => {
                assert_eq!(ms[0], TyArena::INT);
                assert_eq!(*a.get(ms[1]), Ty::Option(TyArena::STRING));
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    // --- TyArena::func tests ---

    #[test]
    fn func_helper_empty_params() {
        let mut a = TyArena::new();
        let f = a.func(smallvec![], TyArena::INT);
        assert_eq!(*a.get(f), Ty::Fn(smallvec![], TyArena::INT));
    }

    #[test]
    fn func_helper_single_param() {
        let mut a = TyArena::new();
        let f = a.func(smallvec![TyArena::STRING], TyArena::BOOL);
        assert_eq!(
            *a.get(f),
            Ty::Fn(smallvec![TyArena::STRING], TyArena::BOOL)
        );
    }

    #[test]
    fn func_helper_multiple_params() {
        let mut a = TyArena::new();
        let f = a.func(
            smallvec![TyArena::INT, TyArena::STRING, TyArena::BOOL],
            TyArena::FLOAT,
        );
        assert_eq!(
            *a.get(f),
            Ty::Fn(
                smallvec![TyArena::INT, TyArena::STRING, TyArena::BOOL],
                TyArena::FLOAT,
            )
        );
    }

    // --- Scheme::poly tests ---

    #[test]
    fn scheme_poly_creates_one_var() {
        let mut a = TyArena::new();
        let s = Scheme::poly(&mut a, |t, a| a.array(t));
        assert_eq!(s.vars.as_slice(), [TyVar::new(0)]);
        let v0 = TyVar::new(0);
        match a.get(s.ty) {
            Ty::Array(inner) => {
                assert_eq!(*a.get(*inner), Ty::Var(v0));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn scheme_poly_fn_type() {
        // forall T. Array[T] -> Int
        let mut a = TyArena::new();
        let s = Scheme::poly(&mut a, |t, a| {
            let arr = a.array(t);
            a.func(smallvec![arr], TyArena::INT)
        });
        assert_eq!(s.vars.as_slice(), [TyVar::new(0)]);
        match a.get(s.ty) {
            Ty::Fn(params, ret) => {
                assert_eq!(params.len(), 1);
                assert!(matches!(a.get(params[0]), Ty::Array(_)));
                assert_eq!(*ret, TyArena::INT);
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    #[test]
    fn scheme_poly2_creates_two_vars() {
        let mut a = TyArena::new();
        // forall T U. (T, U) -> (U, T)
        let s = Scheme::poly2(&mut a, |t, u, a| {
            let p = a.alloc(Ty::Tuple(smallvec![t, u]));
            let r = a.alloc(Ty::Tuple(smallvec![u, t]));
            a.func(smallvec![p], r)
        });
        assert_eq!(s.vars.as_slice(), [TyVar::new(0), TyVar::new(1)]);
    }

    #[test]
    fn scheme_poly2_map_type() {
        // forall T U. (Array[T], (T -> U)) -> Array[U]
        let mut a = TyArena::new();
        let s = Scheme::poly2(&mut a, |t, u, a| {
            let arr_t = a.array(t);
            let f_tu = a.func(smallvec![t], u);
            let arr_u = a.array(u);
            a.func(smallvec![arr_t, f_tu], arr_u)
        });
        assert_eq!(s.vars.as_slice(), [TyVar::new(0), TyVar::new(1)]);
    }

    #[test]
    fn scheme_poly3_creates_three_vars() {
        let mut a = TyArena::new();
        // forall T U V. (T, U, V) -> T
        let s = Scheme::poly3(&mut a, |t, u, v, a| {
            let tup = a.alloc(Ty::Tuple(smallvec![t, u, v]));
            a.func(smallvec![tup], t)
        });
        assert_eq!(
            s.vars.as_slice(),
            [TyVar::new(0), TyVar::new(1), TyVar::new(2)]
        );
    }

    #[test]
    fn scheme_poly_instantiate() {
        let mut a = TyArena::new();
        let mut uf = UnionFind::new();
        let s = Scheme::poly(&mut a, |t, a| a.array(t));
        let (inst, constraints) = s.instantiate(&mut uf, &mut a);
        // `poly` uses `TyVar(0)`, so fresh starts at `1`
        match a.get(inst) {
            Ty::Array(inner) => match a.get(*inner) {
                Ty::Var(tv) => assert_eq!(tv.idx(), 1),
                other => panic!("expected Var, got {other:?}"),
            },
            other => panic!("expected Array, got {other:?}"),
        }
        assert!(constraints.is_empty());
    }

    #[test]
    fn scheme_poly2_instantiate() {
        let mut a = TyArena::new();
        let mut uf = UnionFind::new();
        let s = Scheme::poly2(&mut a, |t, u, a| {
            a.alloc(Ty::Tuple(smallvec![t, u]))
        });
        let (inst, constraints) = s.instantiate(&mut uf, &mut a);
        // `poly2` uses `TyVar(0)` and `TyVar(1)`, so fresh starts at `2`
        match a.get(inst) {
            Ty::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(a.get(elems[0]), Ty::Var(v) if v.idx() == 2));
                assert!(matches!(a.get(elems[1]), Ty::Var(v) if v.idx() == 3));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
        assert!(constraints.is_empty());
    }
}
