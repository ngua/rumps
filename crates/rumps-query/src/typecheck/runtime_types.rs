//! Post-typecheck type layer for the interpreter.
//!
//! Provides `RuntimeTyId` (a solved `TyId` with no inference artifacts),
//! `RuntimeTypes` (an owned `TyArena` with runtime query methods), and
//! `CheckedProgram` (per-expression type info populated by the checker).

use std::collections::HashMap;

use super::ty::{ClassRegistry, Ty, TyArena, TyId};
use crate::ast::{AstTypeExprId, ExprId};
use crate::intern::StringId;
use crate::value::ValueMeta;
use crate::TypeId;

/// A solved `TyId`; guaranteed by the caller (the typechecker) to contain no
/// `Ty::Var`, `Ty::Apply`, `Ty::AssocType`, or unresolved `Ty::Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RuntimeTyId(TyId);

impl From<TyId> for RuntimeTyId {
    fn from(id: TyId) -> Self {
        Self(id)
    }
}

impl RuntimeTyId {
    /// Sentinel for unknown / unresolved types during migration.
    pub(crate) const UNKNOWN: Self = Self(TyArena::UNKNOWN);

    /// Access the underlying `TyId`.
    pub(crate) fn raw(self) -> TyId {
        self.0
    }
}

/// Holds a `TyArena` and provides runtime type queries for the interpreter.
pub(crate) struct RuntimeTypes {
    arena: TyArena,
}

impl RuntimeTypes {
    pub(crate) fn new(arena: TyArena) -> Self {
        Self { arena }
    }

    /// Look up the `Ty` for a `RuntimeTyId`.
    pub(crate) fn get(&self, t: RuntimeTyId) -> &Ty {
        self.arena.get(t.0)
    }

    /// If the type is `Ty::Named(id, _)`, return `Some(id)`. Otherwise `None`.
    pub(crate) fn base_type(&self, t: RuntimeTyId) -> Option<TypeId> {
        match self.get(t) {
            Ty::Named(id, _) => Some(*id),
            _ => None,
        }
    }

    /// Convert a `RuntimeTyId` to its corresponding `TypeId`.
    ///
    /// Handles primitives (`Ty::Int` -> `TypeId::INT`), parameterized builtins
    /// (`Ty::Array` -> `TypeId::ARRAY`), and named types (`Ty::Named`).
    /// Returns `None` for structural types without a `TypeId` (tuples, objects,
    /// functions, unions, type variables).
    pub(crate) fn to_type_id(&self, t: RuntimeTyId) -> Option<TypeId> {
        match self.get(t) {
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
            Ty::Named(id, _) => Some(*id),
            _ => None,
        }
    }

    /// Unwrap newtype wrappers to find the transparent representation type.
    ///
    /// For now, this returns the input unchanged since we do not yet have
    /// newtype metadata. Will be fleshed out in later phases.
    pub(crate) fn repr(&self, t: RuntimeTyId) -> RuntimeTyId {
        t
    }

    /// Check if `actual` (with representation `repr`) matches `expected`.
    ///
    /// Returns `true` if:
    /// - `actual == expected` or `repr == expected` (identity / newtype transparency)
    /// - structural equality holds between the types
    /// - `expected` is a union and `actual` or `repr` is a member (structurally)
    pub(crate) fn matches(
        &self,
        actual: RuntimeTyId,
        repr: RuntimeTyId,
        expected: RuntimeTyId,
    ) -> bool {
        actual == expected
            || repr == expected
            || self.types_equal(actual.0, expected.0)
            || self.types_equal(repr.0, expected.0)
            || self.union_contains(expected, actual.0)
            || self.union_contains(expected, repr.0)
    }

    /// Check if `expected` is a union that structurally contains `member`.
    fn union_contains(&self, expected: RuntimeTyId, member: TyId) -> bool {
        self.union_members(expected).is_some_and(|members| {
            members
                .iter()
                .any(|&m| m == member || self.types_equal(m, member))
        })
    }

    /// Structural type equality; recursively compares `Ty` values through the
    /// arena. Handles the case where structurally identical types were interned
    /// separately (the arena is append-only with no deduplication).
    fn types_equal(&self, a: TyId, b: TyId) -> bool {
        if a == b {
            true
        } else {
            match (self.arena.get(a), self.arena.get(b)) {
                (Ty::Unknown, _)
                | (_, Ty::Unknown)
                | (Ty::Var(_), _)
                | (_, Ty::Var(_)) => true,
                (Ty::Unit, Ty::Tuple(ts)) | (Ty::Tuple(ts), Ty::Unit) => {
                    ts.is_empty()
                }
                (Ty::Array(e1), Ty::Array(e2)) => self.types_equal(*e1, *e2),
                (Ty::Option(e1), Ty::Option(e2)) => self.types_equal(*e1, *e2),
                (Ty::Result(t1, e1), Ty::Result(t2, e2)) => {
                    self.types_equal(*t1, *t2) && self.types_equal(*e1, *e2)
                }
                (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                    self.types_equal(*k1, *k2) && self.types_equal(*v1, *v2)
                }
                (Ty::Tuple(ts1), Ty::Tuple(ts2)) => {
                    ts1.len() == ts2.len()
                        && ts1
                            .iter()
                            .zip(ts2.iter())
                            .all(|(&a, &b)| self.types_equal(a, b))
                }
                (Ty::Fn(p1, r1), Ty::Fn(p2, r2)) => {
                    p1.len() == p2.len()
                        && self.types_equal(*r1, *r2)
                        && p1
                            .iter()
                            .zip(p2.iter())
                            .all(|(&a, &b)| self.types_equal(a, b))
                }
                (Ty::Object(f1), Ty::Object(f2)) => {
                    f1.len() == f2.len()
                        && f1.iter().zip(f2.iter()).all(
                            |((n1, &t1), (n2, &t2))| {
                                n1 == n2 && self.types_equal(t1, t2)
                            },
                        )
                }
                (Ty::Named(id1, a1), Ty::Named(id2, a2)) => {
                    id1 == id2
                        && a1.len() == a2.len()
                        && a1
                            .iter()
                            .zip(a2.iter())
                            .all(|(&a, &b)| self.types_equal(a, b))
                }
                (Ty::Union(p1, m1), Ty::Union(p2, m2)) => {
                    p1 == p2
                        && m1.len() == m2.len()
                        && m1
                            .iter()
                            .zip(m2.iter())
                            .all(|(&a, &b)| self.types_equal(a, b))
                }
                _ => false,
            }
        }
    }

    /// If the type is `Ty::Union(_, members)`, return the members slice.
    /// Otherwise `None`.
    pub(crate) fn union_members(&self, t: RuntimeTyId) -> Option<&[TyId]> {
        match self.get(t) {
            Ty::Union(_, members) => Some(members.as_slice()),
            _ => None,
        }
    }

    /// Check if a type contains any unresolved type variables (`Ty::Var`).
    ///
    /// Used to detect wildcard types (`_`) that were not constrained during
    /// inference and remain as `Ty::Var` after resolution.
    pub(crate) fn contains_var(&self, t: RuntimeTyId) -> bool {
        self.ty_contains_var(t.0)
    }

    fn ty_contains_var(&self, id: TyId) -> bool {
        match self.arena.get(id) {
            Ty::Var(_) => true,
            Ty::Array(e) | Ty::Option(e) => self.ty_contains_var(*e),
            Ty::Result(a, b) | Ty::Map(a, b) => {
                self.ty_contains_var(*a) || self.ty_contains_var(*b)
            }
            Ty::Tuple(ts) | Ty::Union(_, ts) => {
                ts.iter().any(|&t| self.ty_contains_var(t))
            }
            Ty::Fn(ps, r) => {
                self.ty_contains_var(*r)
                    || ps.iter().any(|&t| self.ty_contains_var(t))
            }
            Ty::Object(fs) => fs.values().any(|&t| self.ty_contains_var(t)),
            Ty::Named(_, args) => args.iter().any(|&t| self.ty_contains_var(t)),
            _ => false,
        }
    }

    /// Intern a `Ty` into the arena and wrap in `RuntimeTyId`.
    pub(crate) fn intern(&mut self, ty: Ty) -> RuntimeTyId {
        RuntimeTyId(self.arena.alloc(ty))
    }

    /// Build `ValueMeta` from a semantic type, computing `repr` automatically.
    pub(crate) fn meta(&self, ty: RuntimeTyId) -> ValueMeta {
        ValueMeta {
            ty,
            repr: self.repr(ty),
        }
    }

    /// `ValueMeta` for `Unit`.
    pub(crate) fn meta_unit(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::UNIT))
    }

    /// `ValueMeta` for `Bool`.
    pub(crate) fn meta_bool(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::BOOL))
    }

    /// `ValueMeta` for `Int`.
    pub(crate) fn meta_int(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::INT))
    }

    /// `ValueMeta` for `Word`.
    pub(crate) fn meta_word(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::WORD))
    }

    /// `ValueMeta` for `Float`.
    pub(crate) fn meta_float(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::FLOAT))
    }

    /// `ValueMeta` for `Char`.
    pub(crate) fn meta_char(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::CHAR))
    }

    /// `ValueMeta` for `String`.
    pub(crate) fn meta_string(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::STRING))
    }

    /// `ValueMeta` for `Json`.
    pub(crate) fn meta_json(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::JSON))
    }

    /// `ValueMeta` for `Time`.
    pub(crate) fn meta_time(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::TIME))
    }

    /// `ValueMeta` for `Range`.
    pub(crate) fn meta_range(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::RANGE))
    }

    /// `ValueMeta` for `Regex`.
    pub(crate) fn meta_regex(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::REGEX))
    }

    /// `ValueMeta` for `FilePath`.
    pub(crate) fn meta_filepath(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::FILEPATH))
    }

    /// `ValueMeta` for `Path`.
    pub(crate) fn meta_path(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::PATH))
    }

    /// `ValueMeta` for `Ordering`.
    pub(crate) fn meta_ordering(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::ORDERING))
    }

    /// `ValueMeta` for `DataStatus`.
    pub(crate) fn meta_data_status(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::DATA_STATUS))
    }

    /// `ValueMeta` for `RuntimeError`.
    pub(crate) fn meta_runtime_error(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::RUNTIME_ERROR))
    }

    /// `ValueMeta` for `Error`.
    pub(crate) fn meta_error(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::ERROR))
    }

    /// `ValueMeta` for `Local`.
    pub(crate) fn meta_local(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::LOCAL))
    }

    /// `ValueMeta` for `Global`.
    pub(crate) fn meta_global(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::GLOBAL))
    }

    /// `ValueMeta` for `Storable`.
    pub(crate) fn meta_storable(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::STORABLE))
    }

    /// `ValueMeta` for `Scalar`.
    pub(crate) fn meta_scalar(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::SCALAR))
    }

    /// `ValueMeta` for `Subscript`.
    pub(crate) fn meta_subscript(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::SUBSCRIPT))
    }

    /// `ValueMeta` for `Ref`.
    pub(crate) fn meta_ref(&self) -> ValueMeta {
        self.meta(RuntimeTyId::from(TyArena::REF))
    }
}

/// Full post-typecheck program metadata, combining solved types with
/// per-expression info, compiled regex patterns, and class definitions.
pub(crate) struct CheckedProgram {
    pub(crate) types: RuntimeTypes,
    pub(crate) exprs: HashMap<ExprId, ExprInfo>,
    pub(crate) regex_cache: Vec<regex::Regex>,
    pub(crate) class_registry: ClassRegistry,
    pub(crate) ast_type_map: HashMap<AstTypeExprId, RuntimeTyId>,
    pub(crate) alias_expansions: HashMap<AstTypeExprId, RuntimeTyId>,
    pub(crate) alias_type_expansions: HashMap<RuntimeTyId, RuntimeTyId>,
}

impl CheckedProgram {
    /// Look up the `ExprInfo` for `id`. Panics via `invariant!` if absent.
    pub(crate) fn expr(&self, id: ExprId) -> &ExprInfo {
        self.exprs
            .get(&id)
            .unwrap_or_else(|| invariant!("ExprId in checked program"))
    }
}

/// Per-expression type annotation produced by the checker.
pub(crate) struct ExprInfo {
    pub(crate) ty: RuntimeTyId,
    pub(crate) aux: ExprAux,
}

/// Auxiliary metadata attached to specific expression kinds.
pub(crate) enum ExprAux {
    /// No extra metadata.
    None,
    /// Index into the compiled regex cache.
    RegexIndex(u32),
    /// Higher-order function call with a known output type.
    HofCall { out: RuntimeTyId },
    /// Method call dispatched through a class instance.
    InstanceCall {
        recv: RuntimeTyId,
        fun: Option<StringId>,
    },
    /// Naked (`:method`) reference resolved to a specific class.
    NakedMethod { class: StringId },
}
