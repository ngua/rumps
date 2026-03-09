//! Type representation for static type checking.
//!
//! Defines the core types: `Ty` (types), `TyVar` (type variables), `Scheme`
//! (polymorphic type schemes), and `Subst` (type substitutions).

use std::collections::{HashMap, HashSet};
use std::fmt;

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::error::TypeError;
use crate::intern::StringId;
use crate::{Span, TypeId};

/// Flat discriminant for builtin type classes.
///
/// Exists only for dispatch table indexing and efficient keying.
/// Class metadata (methods, assoc types, help) lives in `BuiltinClassDef`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BuiltinClassTag {
    Numeric = 0,
    Iterable = 1,
    Monoid = 2,
    BitLike = 3,
    Negatable = 4,
    Fallible = 5,
    Into = 6,
    TryInto = 7,
    Indexable = 8,
    Ord = 9,
    Mappable = 10,
    Foldable = 11,
    Filterable = 12,
    Display = 13,
    Eq = 14,
}

/// The "shape" of a builtin class constraint.
///
/// Determines how many and what kind of type parameters the class carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClassShape {
    /// Kind `*`; no type params in constraint.
    Simple,
    /// Kind `* -> *` (or higher); element type comes from usage sites (`F[T]`).
    Hkt { kind: u8 },
    /// Kind `*`; has explicit type params in constraint (e.g., `Into[T]`).
    Parameterized { params: u8 },
}

/// What kind of type tracking a method requires for runtime dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackKind {
    /// Track return type in `mempty_types` (`Monoid:identity`).
    Mempty,
    /// Track return type in `convert_targets` (`Into:into`, `Fallible:wrap`).
    Convert,
    /// Track inner type of `Result` return in `convert_targets` (`TryInto:try-into`).
    ConvertResultInner,
}

/// Method specification returned by `BuiltinClassTag::method`.
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

impl BuiltinClassTag {
    /// Number of builtin class tags (for array sizing).
    pub(crate) const COUNT: usize = 15;

    /// Parse a class name string into a `BuiltinClassTag`.
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "Numeric" => Some(Self::Numeric),
            "Iterable" => Some(Self::Iterable),
            "Monoid" => Some(Self::Monoid),
            "BitLike" => Some(Self::BitLike),
            "Negatable" => Some(Self::Negatable),
            "Fallible" => Some(Self::Fallible),
            "Into" => Some(Self::Into),
            "TryInto" => Some(Self::TryInto),
            "Indexable" => Some(Self::Indexable),
            "Ord" => Some(Self::Ord),
            "Mappable" => Some(Self::Mappable),
            "Foldable" => Some(Self::Foldable),
            "Filterable" => Some(Self::Filterable),
            "Display" => Some(Self::Display),
            "Eq" => Some(Self::Eq),
            _ => None,
        }
    }

    /// Returns the name of this class tag.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Numeric => "Numeric",
            Self::Iterable => "Iterable",
            Self::Monoid => "Monoid",
            Self::BitLike => "BitLike",
            Self::Negatable => "Negatable",
            Self::Fallible => "Fallible",
            Self::Into => "Into",
            Self::TryInto => "TryInto",
            Self::Indexable => "Indexable",
            Self::Ord => "Ord",
            Self::Eq => "Eq",
            Self::Mappable => "Mappable",
            Self::Foldable => "Foldable",
            Self::Filterable => "Filterable",
            Self::Display => "Display",
        }
    }

    /// Returns static help text for this class, if any.
    pub(crate) const fn help(self) -> Option<&'static str> {
        match self {
            Self::Numeric => {
                Some("numeric types are `Int`, `Word`, and `Float`")
            }
            Self::Monoid => {
                Some("`++` works on `String`, `Array`, `Map`, and `Option`")
            }
            Self::BitLike => {
                Some("bitwise types are `Bool`, `Int`, and `Word`")
            }
            Self::Negatable => Some("negatable types are `Int` and `Float`"),
            Self::Iterable => {
                Some("iterable types are `Array` and `Range`")
            }
            Self::Fallible => {
                Some("fallible types are `Option` and `Result`")
            }
            Self::Indexable => {
                Some("indexable types are `Array`, `Map`, and `String`")
            }
            Self::Ord => {
                Some("orderable types are `Bool`, `Int`, `Word`, `Float`, `Char`, and `String`")
            }
            Self::Eq => {
                Some("equality types are primitives, containers (if elements are `Eq`), and user types with `CLASS Eq`")
            }
            Self::Mappable => {
                Some("mappable types are `Option`, `Result`, `Array`, and `Range`")
            }
            Self::Foldable => {
                Some("foldable types are `Option`, `Result`, `Array`, and `Range`")
            }
            Self::Filterable => {
                Some("filterable types are `Option`, `Result`, and `Array`")
            }
            Self::Into | Self::TryInto | Self::Display => None,
        }
    }

    /// Returns the shape of this class constraint.
    pub(crate) const fn shape(self) -> ClassShape {
        match self {
            Self::Numeric
            | Self::Monoid
            | Self::BitLike
            | Self::Negatable
            | Self::Ord
            | Self::Eq
            | Self::Display => ClassShape::Simple,

            Self::Iterable
            | Self::Fallible
            | Self::Mappable
            | Self::Foldable
            | Self::Filterable => ClassShape::Hkt { kind: 1 },

            Self::Into | Self::TryInto | Self::Indexable => {
                ClassShape::Parameterized { params: 1 }
            }
        }
    }
}

impl fmt::Display for BuiltinClassTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Full definition of a builtin class. Shape information is derived from
/// `self.tag.shape()` when needed; there is no need to duplicate the shape
/// as an enum variant.
pub(crate) struct BuiltinClassDef {
    pub(crate) tag: BuiltinClassTag,
    pub(crate) name: &'static str,
    pub(crate) assoc_types: &'static [&'static str],
    /// Method specs; all methods are required.
    pub(crate) methods: Vec<(&'static str, MethodSpec)>,
}

/// Array of all builtin class definitions, indexed by `BuiltinClassTag as usize`.
pub(crate) type BuiltinClassDefs = [BuiltinClassDef; BuiltinClassTag::COUNT];

impl BuiltinClassDef {
    /// Look up a method by name.
    pub(crate) fn method(
        &self,
        name: &str,
        span: Span,
    ) -> Result<&MethodSpec, TypeError> {
        self.methods
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, spec)| spec)
            .ok_or_else(|| TypeError::UnknownMethod {
                class: self.name.to_string(),
                method: name.to_string(),
                span,
            })
    }

    /// All method names (all methods are required).
    pub(crate) fn method_names(
        &self,
    ) -> impl Iterator<Item = &'static str> + '_ {
        self.methods.iter().map(|(n, _)| *n)
    }

    /// Build all `BuiltinClassTag::COUNT` builtin class definitions.
    ///
    /// The `intern` closure is used for associated type names in method
    /// schemes (currently only `Indexable`'s `"Index"`).
    pub(crate) fn build_all(
        intern: &mut impl FnMut(&str) -> StringId,
        arena: &mut TyArena,
    ) -> BuiltinClassDefs {
        // Pre-allocate common type variable ids
        let v0 = arena.var(0);
        let v1 = arena.var(1);
        // HKT applications: `F[T]` for different type-variable positions
        let tv1_of_v0 = arena.hkt(TyVar::new(1), smallvec![v0]); // F1[T0]
        let tv2_of_v0 = arena.hkt(TyVar::new(2), smallvec![v0]); // F2[T0]
        let tv2_of_v1 = arena.hkt(TyVar::new(2), smallvec![v1]); // F2[T1]

        // Common compound types
        let array_v0 = arena.array(v0);

        // Common function shapes
        let binary_v0 = arena.func(smallvec![v0, v0], v0); // (T, T) -> T
        let unary_v0 = arena.func(smallvec![v0], v0); // (T) -> T

        // Scheme constructor helpers (captured vars are all Copy)
        let simple1 = |ty: TyId, tag: BuiltinClassTag| Scheme {
            vars: vec![TyVar::new(0)],
            ty,
            constraints: smallvec![(TyVar::new(0), BuiltinClass::Simple(tag))],
        };
        let hkt2 = |ty: TyId, tag: BuiltinClassTag| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![(
                TyVar::new(1),
                BuiltinClass::Hkt(tag, None)
            )],
        };
        let hkt3 = |ty: TyId, tag: BuiltinClassTag| Scheme {
            vars: vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
            ty,
            constraints: smallvec![(
                TyVar::new(2),
                BuiltinClass::Hkt(tag, None)
            )],
        };

        // Indexable needs a shared `AssocType` id for its two methods.
        let idx_name = intern("Index");
        let assoc_idx = arena.alloc(Ty::AssocType(
            TyVar::new(0),
            BuiltinClassTag::Indexable,
            idx_name,
        ));

        [
            // Numeric: `forall T: Numeric. (T, T) -> T`
            Self {
                tag: BuiltinClassTag::Numeric,
                name: "Numeric",
                assoc_types: &[],
                methods: vec![
                    (
                        "add",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::Numeric,
                        )),
                    ),
                    (
                        "sub",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::Numeric,
                        )),
                    ),
                    (
                        "mul",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::Numeric,
                        )),
                    ),
                    (
                        "floor-div",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::Numeric,
                        )),
                    ),
                    (
                        "mod",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::Numeric,
                        )),
                    ),
                    (
                        "pow",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::Numeric,
                        )),
                    ),
                ],
            },
            // Iterable
            Self {
                tag: BuiltinClassTag::Iterable,
                name: "Iterable",
                assoc_types: &[],
                methods: vec![
                    (
                        "length",
                        MethodSpec::Standard(hkt2(
                            arena.func(smallvec![tv1_of_v0], TyArena::INT),
                            BuiltinClassTag::Iterable,
                        )),
                    ),
                    (
                        "contains",
                        MethodSpec::Standard(hkt2(
                            arena.func(smallvec![tv1_of_v0, v0], TyArena::BOOL),
                            BuiltinClassTag::Iterable,
                        )),
                    ),
                    (
                        "reverse",
                        MethodSpec::Standard(hkt2(
                            arena.func(smallvec![tv1_of_v0], array_v0),
                            BuiltinClassTag::Iterable,
                        )),
                    ),
                    (
                        "foreach",
                        MethodSpec::Standard({
                            let cb = arena.func(smallvec![v0], TyArena::UNIT);
                            hkt2(
                                arena.func(
                                    smallvec![cb, tv1_of_v0],
                                    TyArena::UNIT,
                                ),
                                BuiltinClassTag::Iterable,
                            )
                        }),
                    ),
                    (
                        "collect",
                        MethodSpec::Standard(hkt2(
                            arena.func(smallvec![tv1_of_v0], array_v0),
                            BuiltinClassTag::Iterable,
                        )),
                    ),
                ],
            },
            // Monoid
            Self {
                tag: BuiltinClassTag::Monoid,
                name: "Monoid",
                assoc_types: &[],
                methods: vec![
                    (
                        "identity",
                        MethodSpec::Tracked {
                            scheme: simple1(
                                arena.func(smallvec![], v0),
                                BuiltinClassTag::Monoid,
                            ),
                            track: TrackKind::Mempty,
                        },
                    ),
                    (
                        "concat",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::Monoid,
                        )),
                    ),
                ],
            },
            // BitLike: `forall T: BitLike. (T, T) -> T`
            Self {
                tag: BuiltinClassTag::BitLike,
                name: "BitLike",
                assoc_types: &[],
                methods: vec![
                    (
                        "bit-and",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::BitLike,
                        )),
                    ),
                    (
                        "bit-or",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::BitLike,
                        )),
                    ),
                    (
                        "shl",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::BitLike,
                        )),
                    ),
                    (
                        "shr",
                        MethodSpec::Standard(simple1(
                            binary_v0,
                            BuiltinClassTag::BitLike,
                        )),
                    ),
                ],
            },
            // Negatable: `forall T: Negatable. (T) -> T`
            Self {
                tag: BuiltinClassTag::Negatable,
                name: "Negatable",
                assoc_types: &[],
                methods: vec![(
                    "neg",
                    MethodSpec::Standard(simple1(
                        unary_v0,
                        BuiltinClassTag::Negatable,
                    )),
                )],
            },
            // Fallible
            Self {
                tag: BuiltinClassTag::Fallible,
                name: "Fallible",
                assoc_types: &[],
                methods: vec![
                    (
                        "unwrap",
                        MethodSpec::Standard(hkt2(
                            arena.func(smallvec![tv1_of_v0], v0),
                            BuiltinClassTag::Fallible,
                        )),
                    ),
                    (
                        "wrap",
                        MethodSpec::Tracked {
                            scheme: hkt2(
                                arena.func(smallvec![v0], tv1_of_v0),
                                BuiltinClassTag::Fallible,
                            ),
                            track: TrackKind::Convert,
                        },
                    ),
                    (
                        "flat-map",
                        MethodSpec::Standard({
                            let cb = arena.func(smallvec![v0], tv2_of_v1);
                            hkt3(
                                arena.func(smallvec![tv2_of_v0, cb], tv2_of_v1),
                                BuiltinClassTag::Fallible,
                            )
                        }),
                    ),
                ],
            },
            // Into: `forall T: Into[U], U. (T) -> U`
            Self {
                tag: BuiltinClassTag::Into,
                name: "Into",
                assoc_types: &[],
                methods: vec![(
                    "into",
                    MethodSpec::Tracked {
                        scheme: Scheme {
                            vars: vec![TyVar::new(0), TyVar::new(1)],
                            ty: arena.func(smallvec![v0], v1),
                            constraints: smallvec![(
                                TyVar::new(0),
                                BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    v1,
                                )
                            )],
                        },
                        track: TrackKind::Convert,
                    },
                )],
            },
            // TryInto: `forall T: TryInto[U], U. (T) -> Result[U, String]`
            Self {
                tag: BuiltinClassTag::TryInto,
                name: "TryInto",
                assoc_types: &[],
                methods: vec![(
                    "try-into",
                    MethodSpec::Tracked {
                        scheme: Scheme {
                            vars: vec![TyVar::new(0), TyVar::new(1)],
                            ty: {
                                let ret = arena.result(v1, TyArena::STRING);
                                arena.func(smallvec![v0], ret)
                            },
                            constraints: smallvec![(
                                TyVar::new(0),
                                BuiltinClass::Parameterized(
                                    BuiltinClassTag::TryInto,
                                    v1,
                                )
                            )],
                        },
                        track: TrackKind::ConvertResultInner,
                    },
                )],
            },
            // Indexable
            Self {
                tag: BuiltinClassTag::Indexable,
                name: "Indexable",
                assoc_types: &["Index"],
                methods: vec![
                    (
                        "index",
                        MethodSpec::Standard(Scheme {
                            vars: vec![TyVar::new(0), TyVar::new(1)],
                            ty: arena.func(smallvec![v0, assoc_idx], v1),
                            constraints: smallvec![(
                                TyVar::new(0),
                                BuiltinClass::Parameterized(
                                    BuiltinClassTag::Indexable,
                                    v1,
                                )
                            )],
                        }),
                    ),
                    (
                        "get",
                        MethodSpec::Standard(Scheme {
                            vars: vec![TyVar::new(0), TyVar::new(1)],
                            ty: {
                                let ret = arena.option(v1);
                                arena.func(smallvec![v0, assoc_idx], ret)
                            },
                            constraints: smallvec![(
                                TyVar::new(0),
                                BuiltinClass::Parameterized(
                                    BuiltinClassTag::Indexable,
                                    v1,
                                )
                            )],
                        }),
                    ),
                ],
            },
            // Ord: `forall T: Ord. (T, T) -> Ordering`
            Self {
                tag: BuiltinClassTag::Ord,
                name: "Ord",
                assoc_types: &[],
                methods: vec![(
                    "compare",
                    MethodSpec::Standard(simple1(
                        arena.func(smallvec![v0, v0], TyArena::ORDERING),
                        BuiltinClassTag::Ord,
                    )),
                )],
            },
            // Mappable: `forall T, U, F: Mappable. ((T) -> U, F[T]) -> F[U]`
            Self {
                tag: BuiltinClassTag::Mappable,
                name: "Mappable",
                assoc_types: &[],
                methods: vec![(
                    "map",
                    MethodSpec::Standard({
                        let cb = arena.func(smallvec![v0], v1);
                        hkt3(
                            arena.func(smallvec![cb, tv2_of_v0], tv2_of_v1),
                            BuiltinClassTag::Mappable,
                        )
                    }),
                )],
            },
            // Foldable: `forall T, U, F: Foldable. ((U, T) -> U, U, F[T]) -> U`
            Self {
                tag: BuiltinClassTag::Foldable,
                name: "Foldable",
                assoc_types: &[],
                methods: vec![(
                    "reduce",
                    MethodSpec::Standard({
                        let cb = arena.func(smallvec![v1, v0], v1);
                        hkt3(
                            arena.func(smallvec![cb, v1, tv2_of_v0], v1),
                            BuiltinClassTag::Foldable,
                        )
                    }),
                )],
            },
            // Filterable: `forall T, F: Filterable. ((T) -> Bool, F[T]) -> Array[T]`
            Self {
                tag: BuiltinClassTag::Filterable,
                name: "Filterable",
                assoc_types: &[],
                methods: vec![(
                    "filter",
                    MethodSpec::Standard({
                        let pred = arena.func(smallvec![v0], TyArena::BOOL);
                        hkt2(
                            arena.func(smallvec![pred, tv1_of_v0], array_v0),
                            BuiltinClassTag::Filterable,
                        )
                    }),
                )],
            },
            // Display: `forall T: Display. (T) -> String`
            Self {
                tag: BuiltinClassTag::Display,
                name: "Display",
                assoc_types: &[],
                methods: vec![(
                    "display",
                    MethodSpec::Standard(simple1(
                        arena.func(smallvec![v0], TyArena::STRING),
                        BuiltinClassTag::Display,
                    )),
                )],
            },
            // Eq: `forall T: Eq. (T, T) -> Bool`
            Self {
                tag: BuiltinClassTag::Eq,
                name: "Eq",
                assoc_types: &[],
                methods: vec![(
                    "eq",
                    MethodSpec::Standard(simple1(
                        arena.func(smallvec![v0, v0], TyArena::BOOL),
                        BuiltinClassTag::Eq,
                    )),
                )],
            },
        ]
    }
}

/// Lightweight, cloneable class constraint reference, generic over the type
/// representation. Replaces per-layer `Class` enums (`cst::Class`,
/// `ty::Class`) with a single shape-based enum.
///
/// Layer instantiations:
/// - CST: `BuiltinClass<TypeExpr>`
/// - AST: `BuiltinClass<AstTypeExprId>`
/// - Ty:  `BuiltinClass<Ty>`
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinClass<T> {
    /// Kind `*` class; no type parameters.
    Simple(BuiltinClassTag),
    /// Kind `* -> *` class; `None` = polymorphic, `Some` = resolved element type.
    Hkt(BuiltinClassTag, Option<T>),
    /// Parameterized class; always carries explicit type arg(s).
    Parameterized(BuiltinClassTag, T),
}

impl<T> BuiltinClass<T> {
    /// Extract the tag for dispatch/lookup.
    pub(crate) fn tag(&self) -> BuiltinClassTag {
        match self {
            Self::Simple(t) | Self::Hkt(t, _) | Self::Parameterized(t, _) => *t,
        }
    }

    /// Map over inner types (for layer conversion).
    pub(crate) fn map<U>(self, mut f: impl FnMut(T) -> U) -> BuiltinClass<U> {
        match self {
            Self::Simple(t) => BuiltinClass::Simple(t),
            Self::Hkt(t, opt) => BuiltinClass::Hkt(t, opt.map(&mut f)),
            Self::Parameterized(t, arg) => {
                BuiltinClass::Parameterized(t, f(arg))
            }
        }
    }

    /// Map over inner types fallibly (for layer conversion with `Result`).
    pub(crate) fn try_map<U, E>(
        self,
        mut f: impl FnMut(T) -> std::result::Result<U, E>,
    ) -> std::result::Result<BuiltinClass<U>, E> {
        match self {
            Self::Simple(t) => Ok(BuiltinClass::Simple(t)),
            Self::Hkt(t, opt) => {
                Ok(BuiltinClass::Hkt(t, opt.map(&mut f).transpose()?))
            }
            Self::Parameterized(t, arg) => {
                Ok(BuiltinClass::Parameterized(t, f(arg)?))
            }
        }
    }

    /// Returns the name of this class for error messages.
    pub(crate) fn name(&self) -> &'static str {
        self.tag().name()
    }

    /// Map over inner types by reference (no `Clone` bound needed).
    pub(crate) fn map_ref<U>(
        &self,
        mut f: impl FnMut(&T) -> U,
    ) -> BuiltinClass<U> {
        match self {
            Self::Simple(t) => BuiltinClass::Simple(*t),
            Self::Hkt(t, opt) => {
                BuiltinClass::Hkt(*t, opt.as_ref().map(&mut f))
            }
            Self::Parameterized(t, arg) => {
                BuiltinClass::Parameterized(*t, f(arg))
            }
        }
    }

    /// Construct from tag + optional arg; validates shape.
    ///
    /// Convenience constructor for the typecheck layer. The parser uses
    /// shape-matching directly with its own error type.
    pub(crate) fn from_tag(
        tag: BuiltinClassTag,
        arg: Option<T>,
        span: Span,
    ) -> Result<Self, TypeError> {
        match tag.shape() {
            ClassShape::Simple => {
                if arg.is_some() {
                    Err(TypeError::ClassRejectsArg {
                        class: tag.name(),
                        span,
                    })
                } else {
                    Ok(Self::Simple(tag))
                }
            }
            ClassShape::Hkt { .. } => Ok(Self::Hkt(tag, arg)),
            ClassShape::Parameterized { .. } => arg.map_or_else(
                || {
                    Err(TypeError::ClassRequiresArg {
                        class: tag.name(),
                        span,
                    })
                },
                |a| Ok(Self::Parameterized(tag, a)),
            ),
        }
    }
}

impl BuiltinClass<TyId> {
    /// Apply a substitution to any inner types.
    pub(crate) fn apply(&self, subst: &Subst, arena: &mut TyArena) -> Self {
        match *self {
            Self::Simple(t) => Self::Simple(t),
            Self::Hkt(t, opt) => {
                Self::Hkt(t, opt.map(|id| arena.apply(id, subst)))
            }
            Self::Parameterized(t, id) => {
                Self::Parameterized(t, arena.apply(id, subst))
            }
        }
    }

    /// Collect free type variables from any inner types.
    pub(crate) fn free_vars(&self, arena: &TyArena) -> HashSet<TyVar> {
        match *self {
            Self::Simple(_) | Self::Hkt(_, None) => HashSet::new(),
            Self::Hkt(_, Some(id)) => arena.free_vars(id),
            Self::Parameterized(_, id) => arena.free_vars(id),
        }
    }

    /// Create a placeholder constraint for error messages.
    pub(crate) fn placeholder(tag: BuiltinClassTag) -> Self {
        match tag.shape() {
            ClassShape::Simple => Self::Simple(tag),
            ClassShape::Hkt { .. } => Self::Hkt(tag, None),
            ClassShape::Parameterized { .. } => {
                Self::Parameterized(tag, TyArena::UNKNOWN)
            }
        }
    }
}

impl<T: fmt::Display> fmt::Display for BuiltinClass<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Simple(tag) => write!(f, "{tag}"),
            Self::Hkt(tag, None) => write!(f, "{tag}"),
            Self::Hkt(tag, Some(elem)) => write!(f, "{tag}[{elem}]"),
            Self::Parameterized(tag, arg) => write!(f, "{tag}[{arg}]"),
        }
    }
}

/// A type variable; placeholder for an unknown type during inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
/// Unlike runtime `TypeExpr`, these include type variables (`Var`) for
/// inference and structural object types.
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

    /// Anonymous union type; value is one of the member types.
    ///
    /// For inline `Int | String` syntax. Named unions (`Storable`, `Scalar`,
    /// user-defined `UNION`) use `Named(TypeId, params)` instead.
    ///
    /// # Why Named Unions Are Separate
    ///
    /// Named unions preserve nominal identity, which matters for:
    ///
    /// 1. **Special `AS` semantics**: `Storable` has infallible `AS` casts that
    ///    may fail at runtime with `Error::RuntimeType`. Anonymous unions don't
    ///    have this special case; `x AS T` on an anonymous union is a static error.
    ///
    /// 2. **Type parameters**: Named unions can be generic (`UNION F[T] = Int | Option[T]`),
    ///    requiring parameter substitution during type checking.
    ///
    /// 3. **Error messages**: Named unions display their registered name (`Storable`)
    ///    rather than the expanded member list.
    Union(SmallVec<[TyId; 4]>),

    /// User-defined type (sum types, aliases, unions) with type parameters.
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
    /// - `BuiltinClassTag`: which class defines the associated type
    /// - `StringId`: the associated type name (e.g., `"Index"`)
    AssocType(TyVar, BuiltinClassTag, StringId),

    /// Unresolved; database reads before inference narrows.
    Unknown,

    /// Error recovery sentinel; unifies with anything.
    Error,
}

impl std::hash::Hash for Ty {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
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
            Self::Union(ms) => ms.hash(state),
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
    /// Returns `true` for `Local`, `Global`, or the named union `Ref`.
    pub(crate) fn is_ref(&self) -> bool {
        matches!(
            self,
            Self::Local | Self::Global | Self::Named(TypeId::REF, _)
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
            Ty::Union(members) => members.iter().any(|&t| self.occurs(t, v)),
            Ty::Named(_, args) => args.iter().any(|&t| self.occurs(t, v)),
            Ty::Apply(w, args) => {
                *w == v || args.iter().any(|&t| self.occurs(t, v))
            }
            Ty::AssocType(w, _, _) => *w == v,
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
            Ty::Union(members) => {
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

    /// Apply a substitution, returning a (possibly new) `TyId`.
    ///
    /// Returns the original `id` when no substitution applies.
    pub(crate) fn apply(&mut self, id: TyId, subst: &Subst) -> TyId {
        if subst.is_empty() {
            id
        } else {
            // Clone the shallow `Ty` to avoid borrow conflicts with
            // recursive `&mut self` calls. Cheap: variants now hold
            // `TyId` (Copy) / `SmallVec<[TyId; 4]>`.
            let ty = self.get(id).clone();
            self.apply_inner(id, ty, subst)
        }
    }

    fn apply_inner(&mut self, id: TyId, ty: Ty, subst: &Subst) -> TyId {
        match ty {
            Ty::Var(v) => subst.0.get(&v).map_or(id, |&t| self.apply(t, subst)),
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
                let n = self.apply(inner, subst);
                if n == inner {
                    id
                } else {
                    self.alloc(Ty::Array(n))
                }
            }
            Ty::Option(inner) => {
                let n = self.apply(inner, subst);
                if n == inner {
                    id
                } else {
                    self.alloc(Ty::Option(n))
                }
            }
            Ty::Result(ok, err) => {
                let nok = self.apply(ok, subst);
                let nerr = self.apply(err, subst);
                if nok == ok && nerr == err {
                    id
                } else {
                    self.alloc(Ty::Result(nok, nerr))
                }
            }
            Ty::Map(k, v) => {
                let nk = self.apply(k, subst);
                let nv = self.apply(v, subst);
                if nk == k && nv == v {
                    id
                } else {
                    self.alloc(Ty::Map(nk, nv))
                }
            }
            Ty::Tuple(ref ts) => {
                let nts: SmallVec<[TyId; 4]> =
                    ts.iter().map(|&t| self.apply(t, subst)).collect();
                if nts == *ts {
                    id
                } else {
                    self.alloc(Ty::Tuple(nts))
                }
            }
            Ty::Fn(ref params, ret) => {
                let np: SmallVec<[TyId; 4]> =
                    params.iter().map(|&t| self.apply(t, subst)).collect();
                let nr = self.apply(ret, subst);
                if np == *params && nr == ret {
                    id
                } else {
                    self.alloc(Ty::Fn(np, nr))
                }
            }
            Ty::Object(ref fields) => {
                let nf: IndexMap<StringId, TyId> = fields
                    .iter()
                    .map(|(&k, &t)| (k, self.apply(t, subst)))
                    .collect();
                if nf == *fields {
                    id
                } else {
                    self.alloc(Ty::Object(nf))
                }
            }
            Ty::Union(ref members) => {
                let nm: SmallVec<[TyId; 4]> =
                    members.iter().map(|&t| self.apply(t, subst)).collect();
                if nm == *members {
                    id
                } else {
                    self.alloc(Ty::Union(nm))
                }
            }
            Ty::Named(type_id, ref args) => {
                let na: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.apply(t, subst)).collect();
                if na == *args {
                    id
                } else {
                    self.alloc(Ty::Named(type_id, na))
                }
            }
            Ty::Apply(v, ref args) => {
                let na: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.apply(t, subst)).collect();
                // Resolve the type variable through the substitution chain
                let ctor_id = subst.0.get(&v).map(|&t| self.apply(t, subst));
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
                            Ty::Result(_, e) => {
                                na.first().map_or(Self::ERROR, |&a| {
                                    self.alloc(Ty::Result(a, e))
                                })
                            }
                            Ty::Array(_) => {
                                na.first().map_or(Self::ERROR, |&a| {
                                    self.alloc(Ty::Array(a))
                                })
                            }
                            Ty::Map(_, mv) => {
                                na.first().map_or(Self::ERROR, |&a| {
                                    self.alloc(Ty::Map(a, mv))
                                })
                            }
                            Ty::Named(tid, orig) => {
                                let new_args: SmallVec<[TyId; 4]> = na
                                    .iter()
                                    .chain(orig.iter().skip(na.len()))
                                    .copied()
                                    .collect();
                                self.alloc(Ty::Named(tid, new_args))
                            }
                            _ if na.is_empty() => cid,
                            _ => Self::ERROR,
                        }
                    }
                }
            }
            Ty::AssocType(v, class, name) => match subst.0.get(&v) {
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
    pub(crate) vars: Vec<TyVar>,
    /// The body type (may contain the quantified variables).
    pub(crate) ty: TyId,
    /// User-specified class constraints on type variables.
    ///
    /// These are re-emitted when the scheme is instantiated at call sites.
    /// Each tuple is `(type_var, class)` where parameterized classes like
    /// `Iterable` carry the element type directly.
    pub(crate) constraints: SmallVec<[(TyVar, BuiltinClass<TyId>); 2]>,
}

impl Scheme {
    /// Create a monomorphic scheme (no quantified variables).
    pub(crate) fn mono(ty: TyId) -> Self {
        Self {
            vars: vec![],
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
            vars: vec![TyVar(0)],
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
            vars: vec![TyVar(0), TyVar(1)],
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
            vars: vec![TyVar(0), TyVar(1), TyVar(2)],
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
    /// Takes a mutable counter for generating fresh `TyVar`s. Returns:
    /// - The `TyId` with all quantified variables replaced by fresh ones
    /// - The class constraints with type variables substituted
    pub(crate) fn instantiate(
        &self,
        next: &mut u32,
        arena: &mut TyArena,
    ) -> (TyId, SmallVec<[(TyId, BuiltinClass<TyId>); 2]>) {
        if self.vars.is_empty() {
            (self.ty, SmallVec::new())
        } else {
            // Ensure fresh vars don't overlap with scheme vars to avoid
            // transitive collapse during apply (since apply recursively
            // substitutes, {v0 -> v1, v1 -> v2} would map v0 to v2)
            let max_scheme = self.vars.iter().map(|v| v.0).max().unwrap_or(0);
            *next = (*next).max(max_scheme + 1);

            let subst = Subst(
                self.vars
                    .iter()
                    .map(|v| {
                        let fresh = TyVar::new(*next);
                        *next += 1;
                        (*v, arena.alloc(Ty::Var(fresh)))
                    })
                    .collect(),
            );
            let ty = arena.apply(self.ty, &subst);
            let constraints = self
                .constraints
                .iter()
                .map(|(v, class)| {
                    let ty = subst
                        .0
                        .get(v)
                        .copied()
                        .unwrap_or_else(|| arena.alloc(Ty::Var(*v)));
                    let class = class.apply(&subst, arena);
                    (ty, class)
                })
                .collect();
            (ty, constraints)
        }
    }

    /// Apply a substitution to the scheme's body.
    ///
    /// Only substitutes free variables; quantified ones are shadowed.
    pub(crate) fn apply(&self, subst: &Subst, arena: &mut TyArena) -> Self {
        let filtered = Subst(
            subst
                .0
                .iter()
                .filter(|(v, _)| !self.vars.contains(v))
                .map(|(v, &t)| (*v, t))
                .collect(),
        );
        Self {
            vars: self.vars.clone(),
            ty: arena.apply(self.ty, &filtered),
            constraints: self.constraints.clone(),
        }
    }

    /// Collect free type variables (excludes quantified variables).
    pub(crate) fn free_vars(&self, arena: &TyArena) -> HashSet<TyVar> {
        let mut fv = arena.free_vars(self.ty);
        self.vars.iter().for_each(|v| {
            fv.remove(v);
        });
        fv
    }
}

/// A substitution mapping type variables to interned types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Subst(pub(crate) HashMap<TyVar, TyId>);

impl Subst {
    /// Empty substitution.
    pub(crate) fn empty() -> Self {
        Self(HashMap::new())
    }

    /// Substitution mapping a single variable.
    pub(crate) fn singleton(v: TyVar, ty: TyId) -> Self {
        Self(std::iter::once((v, ty)).collect())
    }

    /// Compose two substitutions: `self . other`.
    ///
    /// Applying the result is equivalent to applying `other` then `self`.
    pub(crate) fn compose(&self, other: &Self, arena: &mut TyArena) -> Self {
        let applied: HashMap<TyVar, TyId> = other
            .0
            .iter()
            .map(|(v, &t)| (*v, arena.apply(t, self)))
            .collect();
        let mut merged = self.0.clone();
        applied.into_iter().for_each(|(v, t)| {
            merged.entry(v).or_insert(t);
        });
        Self(merged)
    }

    /// Extend this substitution with a new binding.
    pub(crate) fn extend(&mut self, v: TyVar, ty: TyId) {
        self.0.insert(v, ty);
    }

    /// Check if this substitution is empty.
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
    fn apply_subst_var() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let subst = Subst::singleton(v, TyArena::INT);
        let res = a.apply(vid, &subst);
        assert_eq!(res, TyArena::INT);
    }

    #[test]
    fn apply_subst_nested() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let arr = a.array(vid);
        let subst = Subst::singleton(v, TyArena::STRING);
        let res = a.apply(arr, &subst);
        let applied_vid = a.apply(vid, &subst);
        assert_eq!(*a.get(res), Ty::Array(applied_vid));
    }

    #[test]
    fn apply_subst_no_match() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let _w = TyVar::new(1);
        let wid = a.var(1);
        let subst = Subst::singleton(v, TyArena::INT);
        let res = a.apply(wid, &subst);
        // No substitution for `w`; should return same id
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
        let v = TyVar::new(0);
        let vid = a.var(0);
        let arr = a.array(vid);
        let s = Scheme {
            vars: vec![v],
            ty: arr,
            constraints: SmallVec::new(),
        };
        let mut next = 100;
        let (inst, constraints) = s.instantiate(&mut next, &mut a);
        assert_eq!(next, 101);
        // Should be `Array[?100]`
        match a.get(inst) {
            Ty::Array(inner) => match a.get(*inner) {
                Ty::Var(tv) => assert_eq!(tv.idx(), 100),
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
            vars: vec![va],
            ty: f,
            constraints: SmallVec::new(),
        };
        let fv = s.free_vars(&a);
        assert!(!fv.contains(&va)); // bound
        assert!(fv.contains(&vb)); // free
    }

    #[test]
    fn subst_compose() {
        let mut a = TyArena::new();
        let va = TyVar::new(0);
        let vb = TyVar::new(1);
        let va_id = a.var(0);
        let vb_id = a.var(1);
        // s1: a -> Int, s2: b -> a
        // composed: b -> Int, a -> Int
        let s1 = Subst::singleton(va, TyArena::INT);
        let s2 = Subst::singleton(vb, va_id);
        let composed = s1.compose(&s2, &mut a);
        let res_b = a.apply(vb_id, &composed);
        let res_a = a.apply(va_id, &composed);
        assert_eq!(res_b, TyArena::INT);
        assert_eq!(res_a, TyArena::INT);
    }

    #[test]
    fn subst_extend() {
        let mut a = TyArena::new();
        let va = TyVar::new(0);
        let vb = TyVar::new(1);
        let va_id = a.var(0);
        let vb_id = a.var(1);
        let mut s = Subst::singleton(va, TyArena::INT);
        s.extend(vb, TyArena::STRING);
        assert_eq!(a.apply(va_id, &s), TyArena::INT);
        assert_eq!(a.apply(vb_id, &s), TyArena::STRING);
    }

    // --- Union type tests ---

    #[test]
    fn union_free_vars_empty() {
        let mut a = TyArena::new();
        let u = a.alloc(Ty::Union(smallvec![TyArena::INT, TyArena::STRING]));
        assert!(a.free_vars(u).is_empty());
    }

    #[test]
    fn union_free_vars_with_var() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let u =
            a.alloc(Ty::Union(smallvec![TyArena::INT, vid, TyArena::STRING]));
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
        let u = a.alloc(Ty::Union(smallvec![aid, bid]));
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
        let u = a.alloc(Ty::Union(smallvec![TyArena::INT, vid]));
        assert!(a.occurs(u, v));
    }

    #[test]
    fn union_occurs_negative() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let u = a.alloc(Ty::Union(smallvec![TyArena::INT, TyArena::STRING]));
        assert!(!a.occurs(u, v));
    }

    #[test]
    fn union_occurs_nested() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let arr = a.array(vid);
        let u = a.alloc(Ty::Union(smallvec![TyArena::INT, arr]));
        assert!(a.occurs(u, v));
    }

    #[test]
    fn union_apply_subst() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let u = a.alloc(Ty::Union(smallvec![TyArena::INT, vid]));
        let subst = Subst::singleton(v, TyArena::BOOL);
        let res = a.apply(u, &subst);
        match a.get(res) {
            Ty::Union(ms) => {
                assert_eq!(ms.len(), 2);
                assert_eq!(ms[0], TyArena::INT);
                assert_eq!(ms[1], TyArena::BOOL);
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn union_apply_subst_no_match() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let wid = a.var(1);
        let u = a.alloc(Ty::Union(smallvec![TyArena::INT, wid]));
        let subst = Subst::singleton(v, TyArena::BOOL);
        let res = a.apply(u, &subst);
        // No change; `w` not in subst
        assert_eq!(res, u);
    }

    #[test]
    fn union_apply_subst_nested() {
        let mut a = TyArena::new();
        let v = TyVar::new(0);
        let vid = a.var(0);
        let opt = a.option(vid);
        let u = a.alloc(Ty::Union(smallvec![TyArena::INT, opt]));
        let subst = Subst::singleton(v, TyArena::STRING);
        let res = a.apply(u, &subst);
        match a.get(res) {
            Ty::Union(ms) => {
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
        assert_eq!(s.vars, vec![TyVar::new(0)]);
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
        assert_eq!(s.vars, vec![TyVar::new(0)]);
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
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1)]);
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
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1)]);
    }

    #[test]
    fn scheme_poly3_creates_three_vars() {
        let mut a = TyArena::new();
        // forall T U V. (T, U, V) -> T
        let s = Scheme::poly3(&mut a, |t, u, v, a| {
            let tup = a.alloc(Ty::Tuple(smallvec![t, u, v]));
            a.func(smallvec![tup], t)
        });
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)]);
    }

    #[test]
    fn scheme_poly_instantiate() {
        let mut a = TyArena::new();
        let s = Scheme::poly(&mut a, |t, a| a.array(t));
        let mut next = 100;
        let (inst, constraints) = s.instantiate(&mut next, &mut a);
        assert_eq!(next, 101);
        match a.get(inst) {
            Ty::Array(inner) => match a.get(*inner) {
                Ty::Var(tv) => assert_eq!(tv.idx(), 100),
                other => panic!("expected Var, got {other:?}"),
            },
            other => panic!("expected Array, got {other:?}"),
        }
        assert!(constraints.is_empty());
    }

    #[test]
    fn scheme_poly2_instantiate() {
        let mut a = TyArena::new();
        let s = Scheme::poly2(&mut a, |t, u, a| {
            a.alloc(Ty::Tuple(smallvec![t, u]))
        });
        let mut next = 50;
        let (inst, constraints) = s.instantiate(&mut next, &mut a);
        assert_eq!(next, 52);
        match a.get(inst) {
            Ty::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(a.get(elems[0]), Ty::Var(v) if v.idx() == 50));
                assert!(matches!(a.get(elems[1]), Ty::Var(v) if v.idx() == 51));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
        assert!(constraints.is_empty());
    }
}
