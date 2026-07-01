//! Post-typecheck type layer for the interpreter.
//!
//! Provides `RuntimeTyId` (a solved `TyId` with no inference artifacts),
//! `RuntimeTypes` (an owned `TyArena` with runtime query methods), and
//! `CheckedProgram` (per-expression type info populated by the checker).

use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::ty::{ClassRegistry, Ty, TyArena, TyId};
use super::Scheme;
use crate::ast::{pragma, ExprId, MatchPatternId};
use crate::intern::StringId;
use crate::value::{Payload, Value, ValueArena, ValueMeta};
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
    /// Access the underlying `TyId`.
    pub(crate) fn raw(self) -> TyId {
        self.0
    }
}

/// Holds a `TyArena` and provides runtime type queries for the interpreter.
pub(crate) struct RuntimeTypes {
    arena: TyArena,
    alias_type_expansions: HashMap<RuntimeTyId, RuntimeTyId>,
}

impl RuntimeTypes {
    pub(crate) fn new(
        arena: TyArena,
        alias_type_expansions: HashMap<RuntimeTyId, RuntimeTyId>,
    ) -> Self {
        Self {
            arena,
            alias_type_expansions,
        }
    }

    /// Look up the `Ty` for a `RuntimeTyId`.
    pub(crate) fn get(&self, t: RuntimeTyId) -> &Ty {
        self.arena.get(t.0)
    }

    /// Look up the `Ty` for a checked `TyId`.
    pub(crate) fn raw(&self, t: TyId) -> &Ty {
        self.arena.get(t)
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
            Ty::RuntimeError => Some(TypeId::ERROR),
            Ty::Local => Some(TypeId::LOCAL),
            Ty::Global => Some(TypeId::GLOBAL),
            Ty::Array(_) => Some(TypeId::ARRAY),
            Ty::Option(_) => Some(TypeId::OPTION),
            Ty::Lazy(_) => Some(TypeId::LAZY),
            Ty::Result(_, _) => Some(TypeId::RESULT),
            Ty::Map(_, _) => Some(TypeId::MAP),
            Ty::Tuple(_) => Some(TypeId::TUPLE),
            Ty::Named(id, _) => Some(*id),
            _ => None,
        }
    }

    /// Unwrap `newtype` wrappers to find the runtime representation type.
    ///
    /// This map is built only from statically approved metadata. Private
    /// `repr visibility` is checked before runtime sees an edge.
    pub(crate) fn repr(&self, t: RuntimeTyId) -> RuntimeTyId {
        self.alias_type_expansions
            .get(&t)
            .map_or(t, |&expanded| self.repr(expanded))
    }

    /// Build `ValueMeta` for a value widened into a union.
    pub(crate) fn union_meta(
        &self,
        ty: RuntimeTyId,
        member: RuntimeTyId,
    ) -> ValueMeta {
        ValueMeta {
            ty,
            repr: self.repr(member),
        }
    }

    /// Check if `actual` (with representation `repr`) matches `expected`.
    ///
    /// Returns `true` if:
    /// - `actual`/`repr` match `expected` or its representation
    ///   (identity / newtype transparency)
    /// - structural equality holds between the types
    /// - `expected` is a union and `actual` or `repr` is a member (structurally)
    pub(crate) fn matches(
        &self,
        actual: RuntimeTyId,
        repr: RuntimeTyId,
        expected: RuntimeTyId,
    ) -> bool {
        let expected_repr = self.repr(expected);
        actual == expected
            || actual == expected_repr
            || repr == expected
            || repr == expected_repr
            || self.types_equal(actual.0, expected.0)
            || self.types_equal(actual.0, expected_repr.0)
            || self.types_equal(repr.0, expected.0)
            || self.types_equal(repr.0, expected_repr.0)
            || self.structural_repr_matches(actual, expected_repr)
            || self.structural_repr_matches(repr, expected_repr)
            || self.union_contains(expected, actual)
            || self.union_contains(expected, repr)
            || self.union_contains(expected_repr, actual)
            || self.union_contains(expected_repr, repr)
    }

    fn structural_repr_matches(
        &self,
        actual: RuntimeTyId,
        expected: RuntimeTyId,
    ) -> bool {
        match self.get(expected) {
            Ty::Object(fields) => self.object_type_contains(actual.0, fields),
            Ty::Array(_)
            | Ty::Lazy(_)
            | Ty::Map(_, _)
            | Ty::Tuple(_)
            | Ty::Fn(_, _) => self.types_equal(actual.0, expected.0),
            _ => false,
        }
    }

    fn object_type_contains(
        &self,
        actual: TyId,
        expected: &IndexMap<StringId, TyId>,
    ) -> bool {
        match self.arena.get(self.repr(RuntimeTyId::from(actual)).0) {
            Ty::Object(fields) => expected.iter().all(|(name, &ty)| {
                fields
                    .get(name)
                    .is_some_and(|&actual| self.field_type_matches(actual, ty))
            }),
            _ => false,
        }
    }

    fn field_type_matches(&self, actual: TyId, expected: TyId) -> bool {
        let actual = self.repr(RuntimeTyId::from(actual)).0;
        let expected = self.repr(RuntimeTyId::from(expected)).0;
        match self.arena.get(expected) {
            Ty::Object(fields) => self.object_type_contains(actual, fields),
            _ => self.types_equal(actual, expected),
        }
    }

    /// Check if an object value has fields matching the checked type pattern.
    pub(crate) fn object_matches(
        &self,
        arena: &ValueArena,
        val: &Value,
        fields: &[(StringId, RuntimeTyId)],
    ) -> bool {
        match &val.payload {
            Payload::Object(obj) => fields.iter().all(|(name, ty)| {
                obj.get(name).and_then(|id| arena.value(*id)).is_some_and(
                    |field| {
                        self.matches(field.ty, field.repr, *ty)
                            || self.object_fields(*ty).is_some_and(|fields| {
                                let fields: Vec<_> = fields
                                    .iter()
                                    .map(|(name, &ty)| {
                                        (*name, RuntimeTyId::from(ty))
                                    })
                                    .collect();
                                self.object_matches(arena, field, &fields)
                            })
                    },
                )
            }),
            _ => false,
        }
    }

    /// Return object fields for `t` after approved representation lookup.
    pub(crate) fn object_fields(
        &self,
        t: RuntimeTyId,
    ) -> Option<&IndexMap<StringId, TyId>> {
        match self.get(self.repr(t)) {
            Ty::Object(fields) => Some(fields),
            _ => None,
        }
    }

    /// Check if `expected` is a union that structurally contains `member`.
    fn union_contains(
        &self,
        expected: RuntimeTyId,
        member: RuntimeTyId,
    ) -> bool {
        self.union_members(expected).is_some_and(|members| {
            members
                .iter()
                .any(|&m| self.member_matches(member, RuntimeTyId::from(m)))
        })
    }

    fn member_matches(
        &self,
        actual: RuntimeTyId,
        expected: RuntimeTyId,
    ) -> bool {
        let actual_repr = self.repr(actual);
        let expected_repr = self.repr(expected);
        actual == expected
            || actual == expected_repr
            || actual_repr == expected
            || actual_repr == expected_repr
            || self.types_equal(actual.0, expected.0)
            || self.types_equal(actual.0, expected_repr.0)
            || self.types_equal(actual_repr.0, expected.0)
            || self.types_equal(actual_repr.0, expected_repr.0)
            || self.structural_repr_matches(actual, expected_repr)
            || self.structural_repr_matches(actual_repr, expected_repr)
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
                (Ty::Lazy(e1), Ty::Lazy(e2)) => self.types_equal(*e1, *e2),
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

    /// Intern a `Ty` into the arena and wrap in `RuntimeTyId`.
    pub(crate) fn intern(&mut self, ty: Ty) -> RuntimeTyId {
        RuntimeTyId(self.arena.alloc(ty))
    }

    /// Convert a runtime `TypeId` to a solved runtime type.
    pub(crate) fn type_id(&mut self, id: TypeId) -> RuntimeTyId {
        RuntimeTyId(match id {
            TypeId::UNIT => TyArena::UNIT,
            TypeId::BOOL => TyArena::BOOL,
            TypeId::INT => TyArena::INT,
            TypeId::WORD => TyArena::WORD,
            TypeId::FLOAT => TyArena::FLOAT,
            TypeId::CHAR => TyArena::CHAR,
            TypeId::STRING => TyArena::STRING,
            TypeId::FILEPATH => TyArena::FILEPATH,
            TypeId::JSON => TyArena::JSON,
            TypeId::TIME => TyArena::TIME,
            TypeId::RANGE => TyArena::RANGE,
            TypeId::ORDERING => TyArena::ORDERING,
            TypeId::DATA_STATUS => TyArena::DATA_STATUS,
            TypeId::PATH => TyArena::PATH,
            TypeId::REGEX => TyArena::REGEX,
            TypeId::ERROR => TyArena::RUNTIME_ERROR,
            TypeId::LOCAL => TyArena::LOCAL,
            TypeId::GLOBAL => TyArena::GLOBAL,
            TypeId::STORABLE => TyArena::STORABLE,
            TypeId::SCALAR => TyArena::SCALAR,
            TypeId::SUBSCRIPT => TyArena::SUBSCRIPT,
            TypeId::REF => TyArena::REF,
            other => self.arena.alloc(Ty::Named(other, SmallVec::new())),
        })
    }

    /// Build a named runtime type in this arena.
    pub(crate) fn named(
        &mut self,
        id: TypeId,
        args: SmallVec<[RuntimeTyId; 4]>,
    ) -> RuntimeTyId {
        self.intern(Ty::Named(id, args.iter().map(|t| t.raw()).collect()))
    }

    /// Build a tuple runtime type in this arena.
    pub(crate) fn tuple(
        &mut self,
        elems: SmallVec<[RuntimeTyId; 4]>,
    ) -> RuntimeTyId {
        self.intern(Ty::Tuple(elems.iter().map(|t| t.raw()).collect()))
    }

    /// Build an object runtime type in this arena.
    pub(crate) fn object(
        &mut self,
        fields: IndexMap<StringId, RuntimeTyId>,
    ) -> RuntimeTyId {
        self.intern(Ty::Object(
            fields.into_iter().map(|(n, t)| (n, t.raw())).collect(),
        ))
    }

    /// Build a union runtime type in this arena.
    pub(crate) fn union(
        &mut self,
        name: Option<TypeId>,
        members: SmallVec<[RuntimeTyId; 4]>,
    ) -> RuntimeTyId {
        self.intern(Ty::Union(name, members.iter().map(|t| t.raw()).collect()))
    }

    /// Build an array runtime type in this arena.
    pub(crate) fn array(&mut self, elem: RuntimeTyId) -> RuntimeTyId {
        self.intern(Ty::Array(elem.raw()))
    }

    /// Build an option runtime type in this arena.
    pub(crate) fn option(&mut self, elem: RuntimeTyId) -> RuntimeTyId {
        self.intern(Ty::Option(elem.raw()))
    }

    /// Build a lazy runtime type in this arena.
    pub(crate) fn lazy(&mut self, elem: RuntimeTyId) -> RuntimeTyId {
        self.intern(Ty::Lazy(elem.raw()))
    }

    /// Build a result runtime type in this arena.
    pub(crate) fn result(
        &mut self,
        ok: RuntimeTyId,
        err: RuntimeTyId,
    ) -> RuntimeTyId {
        self.intern(Ty::Result(ok.raw(), err.raw()))
    }

    /// Build a map runtime type in this arena.
    pub(crate) fn map(
        &mut self,
        key: RuntimeTyId,
        val: RuntimeTyId,
    ) -> RuntimeTyId {
        self.intern(Ty::Map(key.raw(), val.raw()))
    }

    /// Build a function runtime type in this arena.
    pub(crate) fn func(
        &mut self,
        params: SmallVec<[RuntimeTyId; 4]>,
        ret: RuntimeTyId,
    ) -> RuntimeTyId {
        self.intern(Ty::Fn(params.iter().map(|t| t.raw()).collect(), ret.raw()))
    }

    /// Copy a type from another arena into this runtime arena.
    pub(crate) fn import_ty(
        &mut self,
        source: &TyArena,
        ty: TyId,
    ) -> RuntimeTyId {
        match source.get(ty).clone() {
            Ty::Var(v) => self.intern(Ty::Var(v)),
            Ty::Bool => RuntimeTyId::from(TyArena::BOOL),
            Ty::Int => RuntimeTyId::from(TyArena::INT),
            Ty::Word => RuntimeTyId::from(TyArena::WORD),
            Ty::Float => RuntimeTyId::from(TyArena::FLOAT),
            Ty::Char => RuntimeTyId::from(TyArena::CHAR),
            Ty::String => RuntimeTyId::from(TyArena::STRING),
            Ty::Unit => RuntimeTyId::from(TyArena::UNIT),
            Ty::Time => RuntimeTyId::from(TyArena::TIME),
            Ty::Range => RuntimeTyId::from(TyArena::RANGE),
            Ty::Json => RuntimeTyId::from(TyArena::JSON),
            Ty::Ordering => RuntimeTyId::from(TyArena::ORDERING),
            Ty::DataStatus => RuntimeTyId::from(TyArena::DATA_STATUS),
            Ty::FilePath => RuntimeTyId::from(TyArena::FILEPATH),
            Ty::Path => RuntimeTyId::from(TyArena::PATH),
            Ty::Regex => RuntimeTyId::from(TyArena::REGEX),
            Ty::RuntimeError => RuntimeTyId::from(TyArena::RUNTIME_ERROR),
            Ty::Local => RuntimeTyId::from(TyArena::LOCAL),
            Ty::Global => RuntimeTyId::from(TyArena::GLOBAL),
            Ty::Array(elem) => {
                let elem = self.import_ty(source, elem);
                self.array(elem)
            }
            Ty::Option(elem) => {
                let elem = self.import_ty(source, elem);
                self.option(elem)
            }
            Ty::Lazy(elem) => {
                let elem = self.import_ty(source, elem);
                self.lazy(elem)
            }
            Ty::Result(ok, err) => {
                let ok = self.import_ty(source, ok);
                let err = self.import_ty(source, err);
                self.result(ok, err)
            }
            Ty::Map(key, val) => {
                let key = self.import_ty(source, key);
                let val = self.import_ty(source, val);
                self.map(key, val)
            }
            Ty::Tuple(elems) => {
                let elems = elems
                    .iter()
                    .map(|&elem| self.import_ty(source, elem))
                    .collect();
                self.tuple(elems)
            }
            Ty::Fn(params, ret) => {
                let params = params
                    .iter()
                    .map(|&param| self.import_ty(source, param))
                    .collect();
                let ret = self.import_ty(source, ret);
                self.func(params, ret)
            }
            Ty::Object(fields) => {
                let fields = fields
                    .into_iter()
                    .map(|(name, field)| (name, self.import_ty(source, field)))
                    .collect();
                self.object(fields)
            }
            Ty::Union(name, members) => {
                let members = members
                    .iter()
                    .map(|&member| self.import_ty(source, member).raw())
                    .collect();
                self.intern(Ty::Union(name, members))
            }
            Ty::Named(id, args) => {
                let args = args
                    .iter()
                    .map(|&arg| self.import_ty(source, arg))
                    .collect();
                self.named(id, args)
            }
            Ty::Apply(var, args) => {
                let args = args
                    .iter()
                    .map(|&arg| self.import_ty(source, arg).raw())
                    .collect();
                self.intern(Ty::Apply(var, args))
            }
            Ty::AssocType(var, class, name) => {
                self.intern(Ty::AssocType(var, class, name))
            }
            Ty::Unknown | Ty::Error => {
                typechecked!("runtime type import", "solved concrete type")
            }
        }
    }

    /// Derive metadata from a runtime payload and available runtime type metadata.
    pub(crate) fn meta_for_payload(
        &mut self,
        arena: &ValueArena,
        v: &Payload,
    ) -> ValueMeta {
        let ty = self.ty_for_payload(arena, v);
        self.meta(ty)
    }

    fn ty_for_payload(
        &mut self,
        arena: &ValueArena,
        v: &Payload,
    ) -> RuntimeTyId {
        match v {
            Payload::Unit => RuntimeTyId::from(TyArena::UNIT),
            Payload::Bool(_) => RuntimeTyId::from(TyArena::BOOL),
            Payload::Int(_) => RuntimeTyId::from(TyArena::INT),
            Payload::Word(_) => RuntimeTyId::from(TyArena::WORD),
            Payload::Float(_) => RuntimeTyId::from(TyArena::FLOAT),
            Payload::Char(_) => RuntimeTyId::from(TyArena::CHAR),
            Payload::String(_) => RuntimeTyId::from(TyArena::STRING),
            Payload::FilePath(_) => RuntimeTyId::from(TyArena::FILEPATH),
            Payload::Regex(_) => RuntimeTyId::from(TyArena::REGEX),
            Payload::Time(_) => RuntimeTyId::from(TyArena::TIME),
            Payload::Json(_) => RuntimeTyId::from(TyArena::JSON),
            Payload::Range { .. } => RuntimeTyId::from(TyArena::RANGE),
            Payload::LoopContinuation | Payload::LoopContinue(_) => {
                RuntimeTyId::from(TyArena::UNIT)
            }
            Payload::Ref(is_global, ..) => {
                if *is_global {
                    RuntimeTyId::from(TyArena::GLOBAL)
                } else {
                    RuntimeTyId::from(TyArena::LOCAL)
                }
            }
            Payload::Array(vals) => {
                let elem = vals
                    .iter()
                    .find_map(|id| arena.meta(*id).map(|m| m.ty))
                    .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
                self.array(elem)
            }
            Payload::Tuple(vals) => self.tuple(
                vals.iter()
                    .map(|id| {
                        arena
                            .meta(*id)
                            .map(|m| m.ty)
                            .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT))
                    })
                    .collect(),
            ),
            Payload::Object(fields) => self.object(
                fields
                    .iter()
                    .map(|(&name, &id)| {
                        let ty =
                            arena.meta(id).map(|m| m.ty).unwrap_or_else(|| {
                                RuntimeTyId::from(TyArena::UNIT)
                            });
                        (name, ty)
                    })
                    .collect(),
            ),
            Payload::Map(entries) => {
                let (key, val) = entries
                    .entries()
                    .into_iter()
                    .find_map(|(k, v)| {
                        arena
                            .meta(k)
                            .zip(arena.meta(v))
                            .map(|(k, v)| (k.ty, v.ty))
                    })
                    .unwrap_or_else(|| {
                        (
                            RuntimeTyId::from(TyArena::UNIT),
                            RuntimeTyId::from(TyArena::UNIT),
                        )
                    });
                self.map(key, val)
            }
            Payload::Variant { .. } => {
                typechecked!(
                    "runtime payload metadata",
                    "explicit variant metadata"
                )
            }
            Payload::Closure { params, ret, .. }
            | Payload::Function { params, ret, .. } => {
                self.func(params.iter().map(|(_, ty)| *ty).collect(), *ret)
            }
            Payload::VariantCtor { .. }
            | Payload::ModuleFn { .. }
            | Payload::ClassMethodFn { .. }
            | Payload::PartialApp { .. } => {
                typechecked!(
                    "runtime payload metadata",
                    "explicit callable metadata"
                )
            }
            Payload::ModuleConst { .. } => {
                typechecked!(
                    "runtime payload metadata",
                    "explicit module constant metadata"
                )
            }
        }
    }

    /// Read a scheme arity using this runtime arena.
    pub(crate) fn scheme_arity(&self, s: &Scheme) -> Option<usize> {
        s.arity(&self.arena)
    }

    /// Read function parameter types from a scheme using this runtime arena.
    pub(crate) fn scheme_params<'a>(
        &'a self,
        s: &Scheme,
    ) -> Option<&'a SmallVec<[TyId; 4]>> {
        s.params(&self.arena)
    }

    /// Read a function type arity using this runtime arena.
    pub(crate) fn fn_arity(&self, ty: RuntimeTyId) -> Option<usize> {
        match self.get(ty) {
            Ty::Fn(params, _) => Some(params.len()),
            _ => None,
        }
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

/// Runtime info for a statically approved `newtype` representation edge.
///
/// The checker records this only when `type visibility` and `repr visibility`
/// permit the source expression to cross the edge.
#[derive(Clone, Copy)]
pub(crate) struct NewtypeEdgeRuntimeInfo {
    pub(crate) from: RuntimeTyId,
    pub(crate) to: RuntimeTyId,
    pub(crate) repr: RuntimeTyId,
}

/// Full post-typecheck program metadata, combining solved types with
/// per-expression info, compiled regex patterns, and class definitions.
pub(crate) struct CheckedProgram {
    pub(crate) types: RuntimeTypes,
    pub(crate) exprs: HashMap<ExprId, ExprInfo>,
    pub(crate) regex_cache: Vec<regex::Regex>,
    pub(crate) class_registry: ClassRegistry,
    pub(crate) function_types: HashMap<ExprId, RuntimeTyId>,
    pub(crate) module_fns: HashMap<Vec<StringId>, ValueMeta>,
    pub(crate) module_consts: HashMap<Vec<StringId>, ValueMeta>,
    pub(crate) expr_targets: HashMap<ExprId, RuntimeTyId>,
    /// Approved `newtype` representation edges keyed by use site expression.
    pub(crate) approved_newtype_edges: HashMap<ExprId, NewtypeEdgeRuntimeInfo>,
    pub(crate) is_patterns: HashMap<ExprId, TypePatternInfo>,
    pub(crate) let_targets: HashMap<ExprId, RuntimeTyId>,
    pub(crate) match_targets: HashMap<MatchPatternId, RuntimeTyId>,
    pub(crate) program_pragmas: pragma::Program,
}

impl CheckedProgram {
    /// Look up the `ExprInfo` for `id`. Panics via `invariant!` if absent.
    pub(crate) fn expr(&self, id: ExprId) -> &ExprInfo {
        self.exprs
            .get(&id)
            .unwrap_or_else(|| invariant!("ExprId in checked program"))
    }

    pub(crate) fn module_fn_meta(
        &self,
        path: &[StringId],
    ) -> Option<ValueMeta> {
        self.module_fns.get(path).copied()
    }

    pub(crate) fn module_fn_arity(&self, path: &[StringId]) -> Option<usize> {
        self.module_fn_meta(path)
            .and_then(|meta| self.types.fn_arity(meta.ty))
    }

    pub(crate) fn module_const_meta(
        &self,
        path: &[StringId],
    ) -> Option<ValueMeta> {
        self.module_consts.get(path).copied()
    }

    pub(crate) fn expr_target(
        &self,
        id: ExprId,
        ctx: &'static str,
    ) -> RuntimeTyId {
        self.expr_targets
            .get(&id)
            .copied()
            .unwrap_or_else(|| typechecked!(ctx, "checked expression target"))
    }

    pub(crate) fn approved_newtype_edge(
        &self,
        id: ExprId,
    ) -> Option<NewtypeEdgeRuntimeInfo> {
        self.approved_newtype_edges.get(&id).copied()
    }

    pub(crate) fn let_target(&self, id: ExprId) -> Option<RuntimeTyId> {
        self.let_targets.get(&id).copied()
    }

    pub(crate) fn match_target(&self, id: MatchPatternId) -> RuntimeTyId {
        self.match_targets
            .get(&id)
            .copied()
            .unwrap_or_else(|| typechecked!("match IS", "checked target type"))
    }
}

/// Per-expression type annotation produced by the checker.
pub(crate) struct ExprInfo {
    pub(crate) ty: RuntimeTyId,
    pub(crate) repr: Option<RuntimeTyId>,
    pub(crate) aux: ExprAux,
}

/// Auxiliary metadata attached to specific expression kinds.
pub(crate) enum ExprAux {
    /// No extra metadata.
    None,
    /// Index into the compiled regex cache.
    RegexIndex(u32),
    /// Higher-order function call with a known output type.
    HofCall {
        out: RuntimeTyId,
        class: Option<StringId>,
    },
    /// Method call dispatched through a class instance.
    InstanceCall {
        recv: Option<RuntimeTyId>,
        fun: Option<StringId>,
        class: Option<StringId>,
    },
    /// Naked (`:method`) reference resolved to a specific class.
    NakedMethod { class: StringId },
}

#[derive(Clone)]
pub(crate) enum TypePatternInfo {
    Type(RuntimeTyId),
    Object(Vec<(StringId, RuntimeTyId)>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias_types() -> (RuntimeTypes, RuntimeTyId) {
        let mut arena = TyArena::new();
        let alias =
            RuntimeTyId::from(arena.named(TypeId::FILEPATH, SmallVec::new()));
        let mut reps = HashMap::new();
        reps.insert(alias, RuntimeTyId::from(TyArena::STRING));
        (RuntimeTypes::new(arena, reps), alias)
    }

    #[test]
    fn phase_11_repr_unwraps_newtype_expansion() {
        let (tys, alias) = alias_types();

        assert_eq!(tys.repr(alias), RuntimeTyId::from(TyArena::STRING));
    }

    #[test]
    fn phase_11_union_meta_uses_union_ty_and_member_repr() {
        let (tys, alias) = alias_types();
        let union = RuntimeTyId::from(TyArena::STORABLE);
        let meta = tys.union_meta(union, alias);

        assert_eq!(meta.ty, union);
        assert_eq!(meta.repr, RuntimeTyId::from(TyArena::STRING));
    }
}
