//! Type representation for static type checking.
//!
//! Defines the core types: `Ty` (types), `TyVar` (type variables), `Scheme`
//! (polymorphic type schemes), and `Subst` (type substitutions).

use std::collections::{HashMap, HashSet};
use std::fmt;

use indexmap::IndexMap;
use rumps_query_macros::scheme;
use smallvec::SmallVec;

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
    Hkt { kind_arity: u8 },
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
            | Self::Filterable => ClassShape::Hkt { kind_arity: 1 },

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

/// Shared metadata for a builtin class definition.
pub(crate) struct BuiltinClassInfo {
    pub(crate) tag: BuiltinClassTag,
    pub(crate) name: &'static str,
    pub(crate) assoc_types: &'static [&'static str],
    /// Method specs; all methods are required.
    pub(crate) methods: Vec<(&'static str, MethodSpec)>,
    pub(crate) help: Option<&'static str>,
}

/// Full definition of a builtin class; shape is the enum variant, identity
/// is data inside `BuiltinClassInfo`.
pub(crate) enum BuiltinClassDef {
    /// Kind `*` class; no type parameters.
    Simple(BuiltinClassInfo),
    /// Kind `* -> *` (or higher) class.
    Hkt {
        info: BuiltinClassInfo,
        kind_arity: u8,
    },
    /// Parameterized class with explicit type args.
    Parameterized { info: BuiltinClassInfo, params: u8 },
}

/// Array of all builtin class definitions, indexed by `BuiltinClassTag as usize`.
pub(crate) type BuiltinClassDefs = [BuiltinClassDef; BuiltinClassTag::COUNT];

impl BuiltinClassDef {
    /// Shared info regardless of shape.
    pub(crate) fn info(&self) -> &BuiltinClassInfo {
        match self {
            Self::Simple(i)
            | Self::Hkt { info: i, .. }
            | Self::Parameterized { info: i, .. } => i,
        }
    }

    pub(crate) fn tag(&self) -> BuiltinClassTag {
        self.info().tag
    }

    pub(crate) fn name(&self) -> &'static str {
        self.info().name
    }

    /// Look up a method by name.
    pub(crate) fn method(
        &self,
        name: &str,
        span: Span,
    ) -> Result<&MethodSpec, TypeError> {
        self.info()
            .methods
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, spec)| spec)
            .ok_or_else(|| TypeError::UnknownMethod {
                class: self.name().to_string(),
                method: name.to_string(),
                span,
            })
    }

    /// All method names (all methods are required).
    pub(crate) fn method_names(
        &self,
    ) -> impl Iterator<Item = &'static str> + '_ {
        self.info().methods.iter().map(|(n, _)| *n)
    }

    /// Build all `15` builtin class definitions.
    ///
    /// The `intern` closure is used for associated type names in method
    /// schemes (currently only `Indexable`'s `"Index"`).
    pub(crate) fn build_all(
        intern: &mut impl FnMut(&str) -> StringId,
    ) -> BuiltinClassDefs {
        [
            // Simple classes
            Self::Simple(BuiltinClassInfo {
                tag: BuiltinClassTag::Numeric,
                name: "Numeric",
                assoc_types: &[],
                methods: vec![
                    ("add", MethodSpec::Standard(scheme!(forall T: Numeric. (T, T) -> T))),
                    ("sub", MethodSpec::Standard(scheme!(forall T: Numeric. (T, T) -> T))),
                    ("mul", MethodSpec::Standard(scheme!(forall T: Numeric. (T, T) -> T))),
                    ("floor-div", MethodSpec::Standard(scheme!(forall T: Numeric. (T, T) -> T))),
                    ("mod", MethodSpec::Standard(scheme!(forall T: Numeric. (T, T) -> T))),
                    ("pow", MethodSpec::Standard(scheme!(forall T: Numeric. (T, T) -> T))),
                ],
                help: Some("numeric types are `Int`, `Word`, and `Float`"),
            }),
            // Iterable (HKT)
            Self::Hkt {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::Iterable,
                    name: "Iterable",
                    assoc_types: &[],
                    methods: vec![
                        ("length", MethodSpec::Standard(scheme!(forall T, I: Iterable. (I[T]) -> Int))),
                        ("contains", MethodSpec::Standard(scheme!(forall T, I: Iterable. (I[T], T) -> Bool))),
                        ("reverse", MethodSpec::Standard(scheme!(forall T, I: Iterable. (I[T]) -> Array[T]))),
                        ("foreach", MethodSpec::Standard(scheme!(forall T, I: Iterable. ((T) -> Unit, I[T]) -> Unit))),
                    ],
                    help: Some("iterable types are `Array` and `Range`"),
                },
                kind_arity: 1,
            },
            // Monoid (Simple)
            Self::Simple(BuiltinClassInfo {
                tag: BuiltinClassTag::Monoid,
                name: "Monoid",
                assoc_types: &[],
                methods: vec![
                    ("identity", MethodSpec::Tracked {
                        scheme: scheme!(forall T: Monoid. () -> T),
                        track: TrackKind::Mempty,
                    }),
                    ("concat", MethodSpec::Standard(scheme!(forall T: Monoid. (T, T) -> T))),
                ],
                help: Some("`++` works on `String`, `Array`, `Map`, and `Option`"),
            }),
            // BitLike (Simple)
            Self::Simple(BuiltinClassInfo {
                tag: BuiltinClassTag::BitLike,
                name: "BitLike",
                assoc_types: &[],
                methods: vec![
                    ("bit-and", MethodSpec::Standard(scheme!(forall T: BitLike. (T, T) -> T))),
                    ("bit-or", MethodSpec::Standard(scheme!(forall T: BitLike. (T, T) -> T))),
                    ("shl", MethodSpec::Standard(scheme!(forall T: BitLike. (T, T) -> T))),
                    ("shr", MethodSpec::Standard(scheme!(forall T: BitLike. (T, T) -> T))),
                ],
                help: Some("bitwise types are `Bool`, `Int`, and `Word`"),
            }),
            // Negatable (Simple)
            Self::Simple(BuiltinClassInfo {
                tag: BuiltinClassTag::Negatable,
                name: "Negatable",
                assoc_types: &[],
                methods: vec![
                    ("neg", MethodSpec::Standard(scheme!(forall T: Negatable. (T) -> T))),
                ],
                help: Some("negatable types are `Int` and `Float`"),
            }),
            // Fallible (HKT)
            Self::Hkt {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::Fallible,
                    name: "Fallible",
                    assoc_types: &[],
                    methods: vec![
                        ("unwrap", MethodSpec::Standard(scheme!(forall T, F: Fallible. (F[T]) -> T))),
                        ("wrap", MethodSpec::Tracked {
                            scheme: scheme!(forall T, F: Fallible. (T) -> F[T]),
                            track: TrackKind::Convert,
                        }),
                        ("flat-map", MethodSpec::Standard(scheme!(forall T, U, F: Fallible. (F[T], (T) -> F[U]) -> F[U]))),
                    ],
                    help: Some("fallible types are `Option` and `Result`"),
                },
                kind_arity: 1,
            },
            // Into (Parameterized)
            Self::Parameterized {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::Into,
                    name: "Into",
                    assoc_types: &[],
                    methods: vec![
                        ("into", MethodSpec::Tracked {
                            scheme: scheme!(forall T: Into[U], U. (T) -> U),
                            track: TrackKind::Convert,
                        }),
                    ],
                    help: None,
                },
                params: 1,
            },
            // TryInto (Parameterized)
            Self::Parameterized {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::TryInto,
                    name: "TryInto",
                    assoc_types: &[],
                    methods: vec![
                        ("try-into", MethodSpec::Tracked {
                            scheme: scheme!(forall T: TryInto[U], U. (T) -> Result[U, String]),
                            track: TrackKind::ConvertResultInner,
                        }),
                    ],
                    help: None,
                },
                params: 1,
            },
            // Indexable (Parameterized, with assoc type `Index`)
            Self::Parameterized {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::Indexable,
                    name: "Indexable",
                    assoc_types: &["Index"],
                    methods: vec![
                        ("index", MethodSpec::Standard(scheme!(forall B: Indexable[E], E. (B, B:Indexable:Index) -> E))),
                        ("get", MethodSpec::Standard(scheme!(forall B: Indexable[E], E. (B, B:Indexable:Index) -> Option[E]))),
                    ],
                    help: Some("indexable types are `Array`, `Map`, and `String`"),
                },
                params: 1,
            },
            // Ord (Simple)
            Self::Simple(BuiltinClassInfo {
                tag: BuiltinClassTag::Ord,
                name: "Ord",
                assoc_types: &[],
                methods: vec![
                    ("compare", MethodSpec::Standard(scheme!(forall T: Ord. (T, T) -> Ordering))),
                ],
                help: Some("orderable types are `Bool`, `Int`, `Word`, `Float`, `Char`, and `String`"),
            }),
            // Mappable (HKT)
            Self::Hkt {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::Mappable,
                    name: "Mappable",
                    assoc_types: &[],
                    methods: vec![
                        ("map", MethodSpec::Standard(scheme!(forall T, U, M: Mappable. ((T) -> U, M[T]) -> Array[U]))),
                    ],
                    help: Some("mappable types are `Option`, `Result`, `Array`, and `Range`"),
                },
                kind_arity: 1,
            },
            // Foldable (HKT)
            Self::Hkt {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::Foldable,
                    name: "Foldable",
                    assoc_types: &[],
                    methods: vec![
                        ("reduce", MethodSpec::Standard(scheme!(forall T, U, F: Foldable. ((U, T) -> U, U, F[T]) -> U))),
                    ],
                    help: Some("foldable types are `Option`, `Result`, `Array`, and `Range`"),
                },
                kind_arity: 1,
            },
            // Filterable (HKT)
            Self::Hkt {
                info: BuiltinClassInfo {
                    tag: BuiltinClassTag::Filterable,
                    name: "Filterable",
                    assoc_types: &[],
                    methods: vec![
                        ("filter", MethodSpec::Standard(scheme!(forall T, F: Filterable. ((T) -> Bool, F[T]) -> Array[T]))),
                    ],
                    help: Some("filterable types are `Option`, `Result`, and `Array`"),
                },
                kind_arity: 1,
            },
            // Display (Simple)
            Self::Simple(BuiltinClassInfo {
                tag: BuiltinClassTag::Display,
                name: "Display",
                assoc_types: &[],
                methods: vec![
                    ("display", MethodSpec::Standard(scheme!(forall T: Display. (T) -> String))),
                ],
                help: None,
            }),
            // Eq (Simple)
            Self::Simple(BuiltinClassInfo {
                tag: BuiltinClassTag::Eq,
                name: "Eq",
                assoc_types: &[],
                methods: vec![
                    ("eq", MethodSpec::Standard(scheme!(forall T: Eq. (T, T) -> Bool))),
                ],
                help: Some("equality types are primitives, containers (if elements are `Eq`), and user types with `CLASS Eq`"),
            }),
        ]
    }
}

/// User-facing type class constraint (Haskell-style).
///
/// Unlike `ast::Class` which carries `AstTypeExprId` for parameterized
/// variants, this carries resolved `Ty` types. Used in `Scheme` storage
/// and during constraint solving.
///
/// HKT classes (kind `* -> *`) use `Option<Ty>` because the element type
/// is specified at usage sites (`F[T]`), not in the constraint (`F: Fallible`).
/// `None` means polymorphic over element type; `Some(ty)` means specific.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Type is iterable (`Array[T]` or `Range`).
    Iterable(Option<Ty>),
    /// Type is fallible (`Option[T]` or `Result[T, E]`).
    Fallible(Option<Ty>),
    /// Type is a functor; supports structure-preserving `map`.
    Mappable(Option<Ty>),
    /// Type supports `fold`/`reduce` operations.
    Foldable(Option<Ty>),
    /// Type supports `filter` operations.
    Filterable(Option<Ty>),
    /// Type can be converted to another type.
    Into(Ty),
    /// Type can be fallibly converted to another type.
    TryInto(Ty),
    /// Type supports indexing (`[]` access).
    ///
    /// The `Ty` is the element type. The index type is accessed via the
    /// associated type `Index` (e.g., `Array.Index = Int`, `Map[K,V].Index = K`).
    Indexable(Ty),
    /// Type is `Int`, `Word`, or `Float`.
    Numeric,
    /// Type supports monoidal concatenation (`++`).
    Monoid,
    /// Type supports bitwise operations (`&`, `|`, `<<`, `>>`).
    BitLike,
    /// Type can be negated with unary `-`.
    Negatable,
    /// Type supports ordering comparisons (`<`, `>`, `<=`, `>=`).
    Ord,
    /// Type supports equality comparisons (`==`, `!=`).
    Eq,
    /// Type can be displayed as RUMPS syntax (for `WRITE`).
    Display,
}

impl Class {
    /// Apply a substitution to any inner types.
    pub(crate) fn apply(&self, subst: &Subst) -> Self {
        match self {
            Self::Iterable(opt) => {
                Self::Iterable(opt.as_ref().map(|t| t.apply(subst)))
            }
            Self::Fallible(opt) => {
                Self::Fallible(opt.as_ref().map(|t| t.apply(subst)))
            }
            Self::Mappable(opt) => {
                Self::Mappable(opt.as_ref().map(|t| t.apply(subst)))
            }
            Self::Foldable(opt) => {
                Self::Foldable(opt.as_ref().map(|t| t.apply(subst)))
            }
            Self::Filterable(opt) => {
                Self::Filterable(opt.as_ref().map(|t| t.apply(subst)))
            }
            Self::Into(t) => Self::Into(t.apply(subst)),
            Self::TryInto(t) => Self::TryInto(t.apply(subst)),
            Self::Indexable(e) => Self::Indexable(e.apply(subst)),
            Self::Numeric
            | Self::Monoid
            | Self::BitLike
            | Self::Negatable
            | Self::Ord
            | Self::Eq
            | Self::Display => self.clone(),
        }
    }

    /// Returns the name of this class for error messages.
    pub(crate) fn name(&self) -> &'static str {
        self.kind().name()
    }

    /// Returns a help message describing what types satisfy this class.
    pub(crate) fn help(&self) -> Option<&'static str> {
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
            Self::Iterable(_) => Some("iterable types are `Array` and `Range`"),
            Self::Fallible(_) => {
                Some("fallible types are `Option` and `Result`")
            }
            Self::Indexable(_) => {
                Some("indexable types are `Array`, `Map`, and `String`")
            }
            Self::Ord => {
                Some("orderable types are `Bool`, `Int`, `Word`, `Float`, `Char`, and `String`")
            }
            Self::Eq => {
                Some("equality types are primitives, containers (if elements are `Eq`), and user types with `CLASS Eq`")
            }
            Self::Mappable(_) => {
                Some("mappable types are `Option`, `Result`, `Array`, and `Range`")
            }
            Self::Foldable(_) => {
                Some("foldable types are `Option`, `Result`, `Array`, and `Range`")
            }
            Self::Filterable(_) => {
                Some("filterable types are `Option`, `Result`, and `Array`")
            }
            Self::Into(_) | Self::TryInto(_) | Self::Display => None,
        }
    }

    /// Collect free type variables from any inner types.
    pub(crate) fn free_vars(&self) -> HashSet<TyVar> {
        match self {
            Self::Iterable(opt)
            | Self::Fallible(opt)
            | Self::Mappable(opt)
            | Self::Foldable(opt)
            | Self::Filterable(opt) => {
                opt.as_ref().map_or_else(HashSet::new, Ty::free_vars)
            }
            Self::Into(t) | Self::TryInto(t) | Self::Indexable(t) => {
                t.free_vars()
            }
            Self::Numeric
            | Self::Monoid
            | Self::BitLike
            | Self::Negatable
            | Self::Ord
            | Self::Eq
            | Self::Display => HashSet::new(),
        }
    }

    /// Get the `BuiltinClassTag` for dispatch table lookup.
    pub(crate) const fn kind(&self) -> BuiltinClassTag {
        match self {
            Self::Iterable(_) => BuiltinClassTag::Iterable,
            Self::Fallible(_) => BuiltinClassTag::Fallible,
            Self::Mappable(_) => BuiltinClassTag::Mappable,
            Self::Foldable(_) => BuiltinClassTag::Foldable,
            Self::Filterable(_) => BuiltinClassTag::Filterable,
            Self::Into(_) => BuiltinClassTag::Into,
            Self::TryInto(_) => BuiltinClassTag::TryInto,
            Self::Indexable(_) => BuiltinClassTag::Indexable,
            Self::Numeric => BuiltinClassTag::Numeric,
            Self::Monoid => BuiltinClassTag::Monoid,
            Self::BitLike => BuiltinClassTag::BitLike,
            Self::Negatable => BuiltinClassTag::Negatable,
            Self::Ord => BuiltinClassTag::Ord,
            Self::Eq => BuiltinClassTag::Eq,
            Self::Display => BuiltinClassTag::Display,
        }
    }

    /// Create a placeholder constraint for error messages.
    pub(crate) fn placeholder(tag: BuiltinClassTag) -> Self {
        match tag.shape() {
            ClassShape::Simple => match tag {
                BuiltinClassTag::Numeric => Self::Numeric,
                BuiltinClassTag::Monoid => Self::Monoid,
                BuiltinClassTag::BitLike => Self::BitLike,
                BuiltinClassTag::Negatable => Self::Negatable,
                BuiltinClassTag::Ord => Self::Ord,
                BuiltinClassTag::Eq => Self::Eq,
                BuiltinClassTag::Display => Self::Display,
                _ => Self::Display, // unreachable for simple tags
            },
            ClassShape::Hkt { .. } => match tag {
                BuiltinClassTag::Iterable => Self::Iterable(None),
                BuiltinClassTag::Fallible => Self::Fallible(None),
                BuiltinClassTag::Mappable => Self::Mappable(None),
                BuiltinClassTag::Foldable => Self::Foldable(None),
                BuiltinClassTag::Filterable => Self::Filterable(None),
                _ => Self::Display, // unreachable for HKT tags
            },
            ClassShape::Parameterized { .. } => match tag {
                BuiltinClassTag::Into => Self::Into(Ty::Unknown),
                BuiltinClassTag::TryInto => Self::TryInto(Ty::Unknown),
                BuiltinClassTag::Indexable => Self::Indexable(Ty::Unknown),
                _ => Self::Display, // unreachable for parameterized tags
            },
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
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
    Array(Box<Self>),
    Option(Box<Self>),
    Result(Box<Self>, Box<Self>),
    Map(Box<Self>, Box<Self>),

    // Compound types
    Tuple(Vec<Self>),
    Fn(Vec<Self>, Box<Self>),

    /// Anonymous structural record; compatible if fields match.
    Object(IndexMap<StringId, Self>),

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
    Union(Vec<Self>),

    /// User-defined type (sum types, aliases, unions) with type parameters.
    Named(TypeId, Vec<Self>),

    /// Higher-kinded type application: `F[U]` where `F` is a type variable.
    ///
    /// Used for polymorphism over type constructors. When `F: Fallible[T]`
    /// and `F` resolves to `Option[T]`, then `Apply(F, [U])` becomes `Option[U]`.
    /// For `Result[T, E]`, it becomes `Result[U, E]` (preserving error type).
    ///
    /// Resolved during substitution: when the type variable is bound to a
    /// concrete type constructor, the application is evaluated.
    Apply(TyVar, Vec<Self>),

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

impl Ty {
    /// Member types of the `Storable` union: values that can be stored in the database.
    ///
    /// Matches `UNION Storable = Bool | Int | Float | Char | String | Json`.
    pub(crate) const STORABLE_MEMBERS: &'static [Self] = &[
        Self::Bool,
        Self::Int,
        Self::Float,
        Self::Char,
        Self::String,
        Self::Json,
    ];

    /// Member types of the `Scalar` union: JSON scalar extraction results.
    ///
    /// Matches `UNION Scalar = Bool | Int | Float | String`.
    /// Excludes `Char` (JSON has no char type) and `Null` (handled by `Option`).
    pub(crate) const SCALAR_MEMBERS: &'static [Self] =
        &[Self::Bool, Self::Int, Self::Float, Self::String];

    /// Member types of the `Subscript` union: DB subscript key types.
    ///
    /// Matches `UNION Subscript = Bool | Int | Float | Char | String | Json`.
    /// Semantically distinct from `Storable`, though currently identical.
    pub(crate) const SUBSCRIPT_MEMBERS: &'static [Self] = &[
        Self::Bool,
        Self::Int,
        Self::Float,
        Self::Char,
        Self::String,
        Self::Json,
    ];

    /// Member types of the builtin `Ref` union: database variable references.
    ///
    /// `Ref = Local | Global` is a builtin union that abstracts over the
    /// local/global distinction. Unlike anonymous unions, `Ref` is nominal;
    /// the named type `Ref` unifies with both `Local` and `Global`.
    ///
    /// # Why `Ref` Preserves Concrete Types
    ///
    /// Type annotations with `Local` or `Global` preserve the concrete type
    /// through inference rather than widening to `Ref`. This is critical for
    /// operations like `@SET` which require transaction context for globals;
    /// writing globals outside transactions must be a static type error.
    ///
    /// When the user annotates `LET x: Ref = ...`, the RHS type (`Local` or
    /// `Global`) is preserved in the expression type map, enabling correct
    /// transaction checking even when the binding has type `Ref`.
    pub(crate) const REF_MEMBERS: &'static [Self] =
        &[Self::Local, Self::Global];

    /// Check if this type is a database reference type.
    ///
    /// Returns `true` for `Local`, `Global`, or the named union `Ref`.
    pub(crate) fn is_ref(&self) -> bool {
        matches!(
            self,
            Self::Local | Self::Global | Self::Named(TypeId::REF, _)
        )
    }

    /// Construct a function type: `Fn([A, B, ...], R)`.
    pub(crate) fn func(params: impl Into<Vec<Self>>, ret: Self) -> Self {
        Self::Fn(params.into(), Box::new(ret))
    }

    /// Collect all free type variables in this type.
    pub(crate) fn free_vars(&self) -> HashSet<TyVar> {
        let mut acc = HashSet::new();
        self.collect_free_vars(&mut acc);
        acc
    }

    fn collect_free_vars(&self, acc: &mut HashSet<TyVar>) {
        match self {
            Self::Var(v) => {
                acc.insert(*v);
            }
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
            Self::Array(t) | Self::Option(t) => t.collect_free_vars(acc),
            Self::Result(ok, err) => {
                ok.collect_free_vars(acc);
                err.collect_free_vars(acc);
            }
            Self::Map(k, v) => {
                k.collect_free_vars(acc);
                v.collect_free_vars(acc);
            }
            Self::Tuple(ts) => ts.iter().for_each(|t| t.collect_free_vars(acc)),
            Self::Fn(params, ret) => {
                params.iter().for_each(|t| t.collect_free_vars(acc));
                ret.collect_free_vars(acc);
            }
            Self::Object(fields) => {
                fields.values().for_each(|t| t.collect_free_vars(acc));
            }
            Self::Union(members) => {
                members.iter().for_each(|t| t.collect_free_vars(acc));
            }
            Self::Named(_, args) => {
                args.iter().for_each(|t| t.collect_free_vars(acc));
            }
            Self::Apply(v, args) => {
                acc.insert(*v);
                args.iter().for_each(|t| t.collect_free_vars(acc));
            }
            Self::AssocType(v, _, _) => {
                acc.insert(*v);
            }
        }
    }

    /// Check if type variable `v` occurs anywhere in this type (occurs check).
    pub(crate) fn occurs(&self, v: TyVar) -> bool {
        match self {
            Self::Var(w) => *w == v,
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
            | Self::Error => false,
            Self::Array(t) | Self::Option(t) => t.occurs(v),
            Self::Result(ok, err) => ok.occurs(v) || err.occurs(v),
            Self::Map(k, val) => k.occurs(v) || val.occurs(v),
            Self::Tuple(ts) => ts.iter().any(|t| t.occurs(v)),
            Self::Fn(params, ret) => {
                params.iter().any(|t| t.occurs(v)) || ret.occurs(v)
            }
            Self::Object(fields) => fields.values().any(|t| t.occurs(v)),
            Self::Union(members) => members.iter().any(|t| t.occurs(v)),
            Self::Named(_, args) => args.iter().any(|t| t.occurs(v)),
            Self::Apply(w, args) => *w == v || args.iter().any(|t| t.occurs(v)),
            Self::AssocType(w, _, _) => *w == v,
        }
    }

    /// Apply a substitution, replacing type variables with their bindings.
    pub(crate) fn apply(&self, subst: &Subst) -> Self {
        match self {
            Self::Var(v) => subst
                .0
                .get(v)
                .map_or_else(|| self.clone(), |t| t.apply(subst)),
            Self::Bool => Self::Bool,
            Self::Int => Self::Int,
            Self::Word => Self::Word,
            Self::Float => Self::Float,
            Self::Char => Self::Char,
            Self::String => Self::String,
            Self::Unit => Self::Unit,
            Self::Time => Self::Time,
            Self::Range => Self::Range,
            Self::Json => Self::Json,
            Self::Ordering => Self::Ordering,
            Self::DataStatus => Self::DataStatus,
            Self::FilePath => Self::FilePath,
            Self::Path => Self::Path,
            Self::Regex => Self::Regex,
            Self::RuntimeError => Self::RuntimeError,
            Self::Local => Self::Local,
            Self::Global => Self::Global,
            Self::Unknown => Self::Unknown,
            Self::Error => Self::Error,
            Self::Array(t) => Self::Array(Box::new(t.apply(subst))),
            Self::Option(t) => Self::Option(Box::new(t.apply(subst))),
            Self::Result(ok, err) => Self::Result(
                Box::new(ok.apply(subst)),
                Box::new(err.apply(subst)),
            ),
            Self::Map(k, v) => {
                Self::Map(Box::new(k.apply(subst)), Box::new(v.apply(subst)))
            }
            Self::Tuple(ts) => {
                Self::Tuple(ts.iter().map(|t| t.apply(subst)).collect())
            }
            Self::Fn(params, ret) => Self::Fn(
                params.iter().map(|t| t.apply(subst)).collect(),
                Box::new(ret.apply(subst)),
            ),
            Self::Object(fields) => Self::Object(
                fields.iter().map(|(k, t)| (*k, t.apply(subst))).collect(),
            ),
            Self::Union(members) => {
                Self::Union(members.iter().map(|t| t.apply(subst)).collect())
            }
            Self::Named(id, args) => {
                Self::Named(*id, args.iter().map(|t| t.apply(subst)).collect())
            }
            Self::Apply(v, args) => {
                let args: Vec<_> =
                    args.iter().map(|t| t.apply(subst)).collect();
                // Resolve the type variable fully through the substitution
                // chain (e.g. `F1 -> Var(F2) -> Option(...)`)
                let ctor = Self::Var(*v).apply(subst);
                match &ctor {
                    // Still unresolved; keep as `Apply`
                    Self::Var(w) => Self::Apply(*w, args),
                    // Parameterized builtins: apply constructor to args
                    Self::Option(_) => args.first().map_or(Self::Error, |a| {
                        Self::Option(Box::new(a.clone()))
                    }),
                    Self::Result(_, e) => {
                        args.first().map_or(Self::Error, |a| {
                            Self::Result(Box::new(a.clone()), e.clone())
                        })
                    }
                    Self::Array(_) => args.first().map_or(Self::Error, |a| {
                        Self::Array(Box::new(a.clone()))
                    }),
                    Self::Map(_, mv) => args.first().map_or(Self::Error, |a| {
                        Self::Map(Box::new(a.clone()), mv.clone())
                    }),
                    // User-defined named types: replace first N
                    // type args with `Apply` args, preserve the rest
                    Self::Named(id, orig_args) => {
                        let new_args: Vec<_> = args
                            .iter()
                            .chain(orig_args.iter().skip(args.len()))
                            .cloned()
                            .collect();
                        Self::Named(*id, new_args)
                    }
                    // Non-parameterized types (e.g. `Range`): just use
                    // the resolved type directly (args are handled by
                    // constraint validation, not constructor application)
                    other if args.is_empty() => other.clone(),
                    _ => Self::Error,
                }
            }
            Self::AssocType(v, class, name) => {
                // If the base type var is bound to another var, update the projection.
                // If bound to a concrete type, keep as-is; resolution happens in unification.
                match subst.0.get(v) {
                    Some(Self::Var(w)) => Self::AssocType(*w, *class, *name),
                    _ => Self::AssocType(*v, *class, *name),
                }
            }
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
    pub(crate) ty: Ty,
    /// User-specified class constraints on type variables.
    ///
    /// These are re-emitted when the scheme is instantiated at call sites.
    /// Each tuple is `(type_var, class)` where parameterized classes like
    /// `Iterable(T)` carry the element type directly.
    pub(crate) constraints: SmallVec<[(TyVar, Class); 2]>,
}

impl Scheme {
    /// Create a monomorphic scheme (no quantified variables).
    pub(crate) fn mono(ty: Ty) -> Self {
        Self {
            vars: vec![],
            ty,
            constraints: SmallVec::new(),
        }
    }

    /// Polymorphic with 1 type variable: `forall T. ...`
    pub(crate) fn poly(f: impl FnOnce(Ty) -> Ty) -> Self {
        let t = Ty::Var(TyVar(0));
        Self {
            vars: vec![TyVar(0)],
            ty: f(t),
            constraints: SmallVec::new(),
        }
    }

    /// Polymorphic with 2 type variables: `forall T U. ...`
    pub(crate) fn poly2(f: impl FnOnce(Ty, Ty) -> Ty) -> Self {
        let t = Ty::Var(TyVar(0));
        let u = Ty::Var(TyVar(1));
        Self {
            vars: vec![TyVar(0), TyVar(1)],
            ty: f(t, u),
            constraints: SmallVec::new(),
        }
    }

    /// Polymorphic with 3 type variables: `forall T U V. ...`
    pub(crate) fn poly3(f: impl FnOnce(Ty, Ty, Ty) -> Ty) -> Self {
        let t = Ty::Var(TyVar(0));
        let u = Ty::Var(TyVar(1));
        let v = Ty::Var(TyVar(2));
        Self {
            vars: vec![TyVar(0), TyVar(1), TyVar(2)],
            ty: f(t, u, v),
            constraints: SmallVec::new(),
        }
    }

    /// Extract the return type if the scheme body is a function type.
    pub(crate) fn return_ty(&self) -> Option<&Ty> {
        match &self.ty {
            Ty::Fn(_, ret) => Some(ret),
            _ => None,
        }
    }

    /// Extract the parameter types if the scheme body is a function type.
    pub(crate) fn params(&self) -> Option<&[Ty]> {
        match &self.ty {
            Ty::Fn(params, _) => Some(params),
            _ => None,
        }
    }

    /// Get the arity (number of parameters) if the scheme body is a function type.
    pub(crate) fn arity(&self) -> Option<usize> {
        self.params().map(|p| p.len())
    }

    /// Instantiate the scheme with fresh type variables.
    ///
    /// Takes a mutable counter for generating fresh `TyVar`s. Returns:
    /// - The concrete `Ty` with all quantified variables replaced by fresh ones
    /// - The class constraints with type variables substituted, to be re-emitted
    pub(crate) fn instantiate(
        &self,
        next: &mut u32,
    ) -> (Ty, SmallVec<[(Ty, Class); 2]>) {
        if self.vars.is_empty() {
            (self.ty.clone(), SmallVec::new())
        } else {
            // Ensure fresh vars don't overlap with scheme vars to avoid
            // transitive collapse during apply (since apply recursively
            // substitutes, {v0 -> v1, v1 -> v2} would map v0 to v2)
            //
            // Note that this caused actual bugs previously
            let max_scheme = self.vars.iter().map(|v| v.0).max().unwrap_or(0);
            *next = (*next).max(max_scheme + 1);

            let subst = Subst(
                self.vars
                    .iter()
                    .map(|v| {
                        let fresh = TyVar::new(*next);
                        *next += 1;
                        (*v, Ty::Var(fresh))
                    })
                    .collect(),
            );
            let ty = self.ty.apply(&subst);
            let constraints = self
                .constraints
                .iter()
                .map(|(v, class)| {
                    let ty = subst.0.get(v).cloned().unwrap_or(Ty::Var(*v));
                    let class = class.apply(&subst);
                    (ty, class)
                })
                .collect();
            (ty, constraints)
        }
    }

    /// Apply a substitution to the scheme's body.
    ///
    /// Only substitutes free variables; quantified ones are shadowed.
    pub(crate) fn apply(&self, subst: &Subst) -> Self {
        let filtered = Subst(
            subst
                .0
                .iter()
                .filter(|(v, _)| !self.vars.contains(v))
                .map(|(v, t)| (*v, t.clone()))
                .collect(),
        );
        Self {
            vars: self.vars.clone(),
            ty: self.ty.apply(&filtered),
            constraints: self.constraints.clone(),
        }
    }

    /// Collect free type variables (excludes quantified variables).
    pub(crate) fn free_vars(&self) -> HashSet<TyVar> {
        let mut fv = self.ty.free_vars();
        self.vars.iter().for_each(|v| {
            fv.remove(v);
        });
        fv
    }
}

/// A substitution mapping type variables to types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Subst(pub(crate) HashMap<TyVar, Ty>);

impl Subst {
    /// Empty substitution.
    pub(crate) fn empty() -> Self {
        Self(HashMap::new())
    }

    /// Substitution mapping a single variable.
    pub(crate) fn singleton(v: TyVar, ty: Ty) -> Self {
        Self(std::iter::once((v, ty)).collect())
    }

    /// Apply this substitution to a type.
    pub(crate) fn apply(&self, ty: &Ty) -> Ty {
        ty.apply(self)
    }

    /// Compose two substitutions: `self . other`.
    ///
    /// Applying the result is equivalent to applying `other` then `self`.
    pub(crate) fn compose(&self, other: &Self) -> Self {
        let applied: HashMap<TyVar, Ty> =
            other.0.iter().map(|(v, t)| (*v, t.apply(self))).collect();
        let mut merged = self.0.clone();
        applied.into_iter().for_each(|(v, t)| {
            merged.entry(v).or_insert(t);
        });
        Self(merged)
    }

    /// Extend this substitution with a new binding.
    pub(crate) fn extend(&mut self, v: TyVar, ty: Ty) {
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
        assert!(Ty::Int.free_vars().is_empty());
        assert!(Ty::Bool.free_vars().is_empty());
    }

    #[test]
    fn free_vars_var() {
        let v = TyVar::new(0);
        let fv = Ty::Var(v).free_vars();
        assert!(fv.contains(&v));
        assert_eq!(fv.len(), 1);
    }

    #[test]
    fn free_vars_array() {
        let v = TyVar::new(1);
        let arr = Ty::Array(Box::new(Ty::Var(v)));
        let fv = arr.free_vars();
        assert!(fv.contains(&v));
    }

    #[test]
    fn free_vars_fn() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let f = Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(b)));
        let fv = f.free_vars();
        assert!(fv.contains(&a));
        assert!(fv.contains(&b));
        assert_eq!(fv.len(), 2);
    }

    #[test]
    fn occurs_check() {
        let v = TyVar::new(0);
        assert!(Ty::Var(v).occurs(v));
        assert!(!Ty::Int.occurs(v));
        assert!(Ty::Array(Box::new(Ty::Var(v))).occurs(v));
        assert!(!Ty::Array(Box::new(Ty::Int)).occurs(v));
    }

    #[test]
    fn apply_subst_var() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::Int);
        assert_eq!(Ty::Var(v).apply(&subst), Ty::Int);
    }

    #[test]
    fn apply_subst_nested() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::String);
        let arr = Ty::Array(Box::new(Ty::Var(v)));
        assert_eq!(arr.apply(&subst), Ty::Array(Box::new(Ty::String)));
    }

    #[test]
    fn apply_subst_no_match() {
        let v = TyVar::new(0);
        let w = TyVar::new(1);
        let subst = Subst::singleton(v, Ty::Int);
        assert_eq!(Ty::Var(w).apply(&subst), Ty::Var(w));
    }

    #[test]
    fn scheme_mono() {
        let s = Scheme::mono(Ty::Int);
        assert!(s.vars.is_empty());
        assert_eq!(s.ty, Ty::Int);
    }

    #[test]
    fn scheme_instantiate() {
        let v = TyVar::new(0);
        let s = Scheme {
            vars: vec![v],
            ty: Ty::Array(Box::new(Ty::Var(v))),
            constraints: SmallVec::new(),
        };
        let mut next = 100;
        let (inst, constraints) = s.instantiate(&mut next);
        // Should have replaced `v` with fresh var `TyVar::new(100)`
        assert_eq!(next, 101);
        assert_eq!(inst, Ty::Array(Box::new(Ty::Var(TyVar::new(100)))));
        assert!(constraints.is_empty());
    }

    #[test]
    fn scheme_free_vars_excludes_bound() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let s = Scheme {
            vars: vec![a],
            ty: Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(b))),
            constraints: SmallVec::new(),
        };
        let fv = s.free_vars();
        assert!(!fv.contains(&a)); // bound
        assert!(fv.contains(&b)); // free
    }

    #[test]
    fn subst_compose() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        // s1: a -> Int
        // s2: b -> a
        // composed: b -> Int, a -> Int
        let s1 = Subst::singleton(a, Ty::Int);
        let s2 = Subst::singleton(b, Ty::Var(a));
        let composed = s1.compose(&s2);
        assert_eq!(composed.apply(&Ty::Var(b)), Ty::Int);
        assert_eq!(composed.apply(&Ty::Var(a)), Ty::Int);
    }

    #[test]
    fn subst_extend() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let mut s = Subst::singleton(a, Ty::Int);
        s.extend(b, Ty::String);
        assert_eq!(s.apply(&Ty::Var(a)), Ty::Int);
        assert_eq!(s.apply(&Ty::Var(b)), Ty::String);
    }

    // --- Union type tests ---

    #[test]
    fn union_free_vars_empty() {
        let u = Ty::Union(vec![Ty::Int, Ty::String]);
        assert!(u.free_vars().is_empty());
    }

    #[test]
    fn union_free_vars_with_var() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(v), Ty::String]);
        let fv = u.free_vars();
        assert!(fv.contains(&v));
        assert_eq!(fv.len(), 1);
    }

    #[test]
    fn union_free_vars_multiple_vars() {
        let a = TyVar::new(0);
        let b = TyVar::new(1);
        let u = Ty::Union(vec![Ty::Var(a), Ty::Var(b)]);
        let fv = u.free_vars();
        assert!(fv.contains(&a));
        assert!(fv.contains(&b));
        assert_eq!(fv.len(), 2);
    }

    #[test]
    fn union_occurs_positive() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(v)]);
        assert!(u.occurs(v));
    }

    #[test]
    fn union_occurs_negative() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::String]);
        assert!(!u.occurs(v));
    }

    #[test]
    fn union_occurs_nested() {
        let v = TyVar::new(0);
        let u = Ty::Union(vec![Ty::Int, Ty::Array(Box::new(Ty::Var(v)))]);
        assert!(u.occurs(v));
    }

    #[test]
    fn union_apply_subst() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::Bool);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(v)]);
        let result = u.apply(&subst);
        assert_eq!(result, Ty::Union(vec![Ty::Int, Ty::Bool]));
    }

    #[test]
    fn union_apply_subst_no_match() {
        let v = TyVar::new(0);
        let w = TyVar::new(1);
        let subst = Subst::singleton(v, Ty::Bool);
        let u = Ty::Union(vec![Ty::Int, Ty::Var(w)]);
        let result = u.apply(&subst);
        assert_eq!(result, Ty::Union(vec![Ty::Int, Ty::Var(w)]));
    }

    #[test]
    fn union_apply_subst_nested() {
        let v = TyVar::new(0);
        let subst = Subst::singleton(v, Ty::String);
        let u = Ty::Union(vec![Ty::Int, Ty::Option(Box::new(Ty::Var(v)))]);
        let result = u.apply(&subst);
        assert_eq!(
            result,
            Ty::Union(vec![Ty::Int, Ty::Option(Box::new(Ty::String))])
        );
    }

    // --- Ty::func tests ---

    #[test]
    fn func_helper_empty_params() {
        let f = Ty::func([], Ty::Int);
        assert_eq!(f, Ty::Fn(vec![], Box::new(Ty::Int)));
    }

    #[test]
    fn func_helper_single_param() {
        let f = Ty::func([Ty::String], Ty::Bool);
        assert_eq!(f, Ty::Fn(vec![Ty::String], Box::new(Ty::Bool)));
    }

    #[test]
    fn func_helper_multiple_params() {
        let f = Ty::func([Ty::Int, Ty::String, Ty::Bool], Ty::Float);
        assert_eq!(
            f,
            Ty::Fn(vec![Ty::Int, Ty::String, Ty::Bool], Box::new(Ty::Float))
        );
    }

    // --- Scheme::poly tests ---

    #[test]
    fn scheme_poly_creates_one_var() {
        let s = Scheme::poly(|t| Ty::Array(Box::new(t)));
        assert_eq!(s.vars, vec![TyVar::new(0)]);
        assert_eq!(s.ty, Ty::Array(Box::new(Ty::Var(TyVar::new(0)))));
    }

    #[test]
    fn scheme_poly_fn_type() {
        // forall T. Array[T] -> Int
        let s = Scheme::poly(|t| Ty::func([Ty::Array(Box::new(t))], Ty::Int));
        assert_eq!(s.vars, vec![TyVar::new(0)]);
        assert_eq!(
            s.ty,
            Ty::Fn(
                vec![Ty::Array(Box::new(Ty::Var(TyVar::new(0))))],
                Box::new(Ty::Int)
            )
        );
    }

    #[test]
    fn scheme_poly2_creates_two_vars() {
        // forall T U. (T, U) -> (U, T)
        let s = Scheme::poly2(|t, u| {
            Ty::func(
                [Ty::Tuple(vec![t.clone(), u.clone()])],
                Ty::Tuple(vec![u, t]),
            )
        });
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1)]);
    }

    #[test]
    fn scheme_poly2_map_type() {
        // forall T U. (Array[T], (T -> U)) -> Array[U]
        let s = Scheme::poly2(|t, u| {
            Ty::func(
                [Ty::Array(Box::new(t.clone())), Ty::func([t], u.clone())],
                Ty::Array(Box::new(u)),
            )
        });
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1)]);
        let t = Ty::Var(TyVar::new(0));
        let u = Ty::Var(TyVar::new(1));
        assert_eq!(
            s.ty,
            Ty::Fn(
                vec![
                    Ty::Array(Box::new(t.clone())),
                    Ty::Fn(vec![t], Box::new(u.clone())),
                ],
                Box::new(Ty::Array(Box::new(u)))
            )
        );
    }

    #[test]
    fn scheme_poly3_creates_three_vars() {
        // forall T U V. (T, U, V) -> T
        let s = Scheme::poly3(|t, u, v| {
            Ty::func([Ty::Tuple(vec![t.clone(), u, v])], t)
        });
        assert_eq!(s.vars, vec![TyVar::new(0), TyVar::new(1), TyVar::new(2)]);
    }

    #[test]
    fn scheme_poly_instantiate() {
        let s = Scheme::poly(|t| Ty::Array(Box::new(t)));
        let mut next = 100;
        let (inst, constraints) = s.instantiate(&mut next);
        // Should replace TyVar(0) with fresh TyVar(100)
        assert_eq!(next, 101);
        assert_eq!(inst, Ty::Array(Box::new(Ty::Var(TyVar::new(100)))));
        assert!(constraints.is_empty());
    }

    #[test]
    fn scheme_poly2_instantiate() {
        let s = Scheme::poly2(|t, u| Ty::Tuple(vec![t, u]));
        let mut next = 50;
        let (inst, constraints) = s.instantiate(&mut next);
        assert_eq!(next, 52);
        assert_eq!(
            inst,
            Ty::Tuple(vec![Ty::Var(TyVar::new(50)), Ty::Var(TyVar::new(51))])
        );
        assert!(constraints.is_empty());
    }
}
