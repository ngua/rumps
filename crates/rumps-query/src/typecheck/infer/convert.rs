//! Inference-specific type conversion and utility helpers.
//!
//! The `convert()` method constructs a `ConvertCtx` for AST-to-`TyId`
//! conversions; inference-specific helpers remain directly on `InferCtx`.

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::{Constraint, InferCtx};
use crate::ast::{AstTypeExpr, AstTypeExprId};
use crate::intern::StringId;
use crate::typecheck::convert::ConvertCtx;
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Ty, TyArena, TyId};
use crate::value::{TypeDef, TypeId};
use crate::Span;

impl InferCtx<'_> {
    pub(super) fn convert(&mut self) -> ConvertCtx<'_> {
        ConvertCtx {
            ty_arena: &mut self.ty_arena,
            uf: &mut self.uf,
            registry: self.registry,
            decls: &self.decls,
            env: &self.env,
            ast: self.ast,
            errors: &mut self.errors,
            current_module: &self.current_module,
            class_context: &self.class_context,
            rewrite_ast: true,
        }
    }

    pub(super) fn object_alias(
        &mut self,
        type_id: TypeId,
    ) -> Option<(
        SmallVec<[StringId; 2]>,
        SmallVec<[(StringId, AstTypeExprId); 4]>,
    )> {
        if self.convert().can_access_alias_repr(type_id) {
            self.registry.get_def(type_id).and_then(|def| match def {
                TypeDef::Alias { type_params, .. } => {
                    let ps = type_params.clone();
                    let target = self.decls.alias_target(type_id);
                    self.ast.get_type_expr(target).and_then(|te| match te {
                        AstTypeExpr::Object(fields) => {
                            Some((ps, fields.clone()))
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
        } else {
            None
        }
    }

    /// Check if a type contains unresolved type variables that require
    /// annotation.
    ///
    /// Polymorphic types like `Option[?t]`, `Result[?t, ?e]`, `Array[?t]`, and
    /// `Fn([?t], ?u)` are allowed with unresolved type parameters; they
    /// represent values like `None`, `Err`, `[]`, or closures passed to HOFs
    /// where the type parameter doesn't affect runtime behavior or is
    /// determined by the calling context.
    pub(super) fn has_unresolved_vars(id: TyId, arena: &TyArena) -> bool {
        match arena.get(id) {
            // `Ty::Unknown` indicates true ambiguity that requires annotation.
            // `Ty::Var` is allowed; unresolved type variables are OK for
            // polymorphic expressions (e.g., `id` function, HOF results).
            Ty::Unknown => true,
            Ty::Var(_)
            | Ty::Bool
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
            | Ty::Error => false,
            // Option, Result, Array, Map, and Fn with unresolved type params are
            // OK. These are intentionally polymorphic (e.g., `Option.None`,
            // `Result.Err("msg")`, `[]`, `Map.empty()`, closures passed to HOFs).
            Ty::Option(_)
            | Ty::Lazy(_)
            | Ty::Result(_, _)
            | Ty::Array(_)
            | Ty::Map(..)
            | Ty::Fn(..) => false,
            Ty::Tuple(ts) | Ty::Union(_, ts) => {
                let ts = ts.clone();
                ts.iter().any(|&t| Self::has_unresolved_vars(t, arena))
            }
            Ty::Object(fields) => {
                let vals: SmallVec<[TyId; 4]> =
                    fields.values().copied().collect();
                vals.iter().any(|&t| Self::has_unresolved_vars(t, arena))
            }
            Ty::Named(_, args) => {
                let args = args.clone();
                args.iter().any(|&t| Self::has_unresolved_vars(t, arena))
            }
            // Apply is polymorphic; check the arguments
            Ty::Apply(_, args) => {
                let args = args.clone();
                args.iter().any(|&t| Self::has_unresolved_vars(t, arena))
            }
            // AssocType is polymorphic; resolved when base type is known
            Ty::AssocType(_, _, _) => false,
        }
    }

    /// Check whether `ty` contains a function type anywhere in its structure.
    ///
    /// Walks structural containers (`Array`, `Option`, `Result`, `Map`,
    /// `Tuple`, `Union`, `Object`) and `Named` type arguments recursively,
    /// reporting `true` the moment a `Ty::Fn` is encountered. Used by the
    /// `is` pattern checker to reject function types nested in containers
    /// (e.g. `[(Int) -> Int]`, `(Int, (Int) -> Int)`, `Int | (Int) -> Int`,
    /// `{ cb: (Int) -> Int }`) for the same reason top-level fn types are
    /// rejected: opaque callables (class method refs, module fn refs,
    /// partial applications) cannot be introspected at runtime, so
    /// `fn_value_matches` has no meaningful answer for them.
    ///
    /// Does NOT expand `Ty::Named` aliases; smuggling a fn type through
    /// a transparent alias like `type F = (Int) -> Int` is a narrower
    /// corner case and is left to a follow-up.
    pub(super) fn type_contains_fn(ty: TyId, arena: &TyArena) -> bool {
        match arena.get(ty) {
            Ty::Fn(_, _) => true,
            Ty::Array(inner) | Ty::Option(inner) | Ty::Lazy(inner) => {
                Self::type_contains_fn(*inner, arena)
            }
            Ty::Result(ok, err) => {
                Self::type_contains_fn(*ok, arena)
                    || Self::type_contains_fn(*err, arena)
            }
            Ty::Map(k, v) => {
                Self::type_contains_fn(*k, arena)
                    || Self::type_contains_fn(*v, arena)
            }
            Ty::Tuple(ts) | Ty::Union(_, ts) => {
                let ts = ts.clone();
                ts.iter().any(|&t| Self::type_contains_fn(t, arena))
            }
            Ty::Object(fields) => {
                let vals: SmallVec<[TyId; 4]> =
                    fields.values().copied().collect();
                vals.iter().any(|&t| Self::type_contains_fn(t, arena))
            }
            Ty::Named(_, args) | Ty::Apply(_, args) => {
                let args = args.clone();
                args.iter().any(|&t| Self::type_contains_fn(t, arena))
            }
            Ty::Var(_)
            | Ty::Bool
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
            | Ty::AssocType(_, _, _)
            | Ty::Unknown
            | Ty::Error => false,
        }
    }

    /// Expand a union type to its member types.
    ///
    /// Returns `Some(members)` for union types (`Ty::Union`), `None` for
    /// non-unions. All unions (builtin and user-defined) are expanded to
    /// `Ty::Union` at construction time in `type_id_to_ty`, so this is a
    /// simple pattern match.
    pub(super) fn expand_union_members(
        &mut self,
        ty: TyId,
    ) -> Option<SmallVec<[TyId; 4]>> {
        match self.ty_arena.get(ty).clone() {
            Ty::Union(_, members) => Some(members),
            _ => None,
        }
    }

    /// Check if a type is a member of a union.
    ///
    /// Returns `true` if `member` is one of the types in `union_ty`.
    pub(super) fn is_union_member(
        &mut self,
        union_ty: TyId,
        member: TyId,
    ) -> bool {
        self.expand_union_members(union_ty)
            .is_some_and(|members| members.contains(&member))
    }

    /// Check if two types are compatible (can unify without error).
    ///
    /// Used to detect heterogeneous arrays before generating constraints.
    pub(super) fn types_compatible(&self, a: TyId, b: TyId) -> bool {
        if a == b {
            true
        } else {
            let (ta, tb) =
                (self.ty_arena.get(a).clone(), self.ty_arena.get(b).clone());
            self.types_compatible_inner(&ta, &tb)
        }
    }

    fn types_compatible_inner(&self, a: &Ty, b: &Ty) -> bool {
        match (a, b) {
            // Same primitive types
            (Ty::Bool, Ty::Bool)
            | (Ty::Int, Ty::Int)
            | (Ty::Word, Ty::Word)
            | (Ty::Float, Ty::Float)
            | (Ty::Char, Ty::Char)
            | (Ty::String, Ty::String)
            | (Ty::Unit, Ty::Unit)
            | (Ty::Time, Ty::Time)
            | (Ty::Range, Ty::Range)
            | (Ty::Json, Ty::Json)
            | (Ty::Ordering, Ty::Ordering)
            | (Ty::DataStatus, Ty::DataStatus)
            | (Ty::FilePath, Ty::FilePath)
            | (Ty::Path, Ty::Path) => true,

            // Type variables are compatible with anything
            (Ty::Var(_), _) | (_, Ty::Var(_)) => true,

            // Unknown is compatible with anything
            (Ty::Unknown, _) | (_, Ty::Unknown) => true,

            // Error propagates
            (Ty::Error, _) | (_, Ty::Error) => true,

            // Arrays: element types must be compatible
            (Ty::Array(a), Ty::Array(b)) => self.types_compatible(*a, *b),

            // Options: inner types must be compatible
            (Ty::Option(a), Ty::Option(b)) => self.types_compatible(*a, *b),

            // `Lazy`: inner types must be compatible
            (Ty::Lazy(a), Ty::Lazy(b)) => self.types_compatible(*a, *b),

            // Tuples: same length and pairwise compatible
            (Ty::Tuple(a), Ty::Tuple(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(&x, &y)| self.types_compatible(x, y))
            }

            // Named types: same TypeId
            (Ty::Named(a, _), Ty::Named(b, _)) => a == b,

            // Objects: same field names and compatible field types
            (Ty::Object(a), Ty::Object(b)) => {
                a.len() == b.len()
                    && a.keys().all(|k| b.contains_key(k))
                    && a.iter().all(|(k, &ty_a)| {
                        b.get(k).is_some_and(|&ty_b| {
                            self.types_compatible(ty_a, ty_b)
                        })
                    })
            }

            // Results: both inner types must be compatible
            (Ty::Result(ok_a, err_a), Ty::Result(ok_b, err_b)) => {
                self.types_compatible(*ok_a, *ok_b)
                    && self.types_compatible(*err_a, *err_b)
            }

            // HKT applications: compatible (unresolved type constructors)
            (Ty::Apply(_, _), _) | (_, Ty::Apply(_, _)) => true,

            // Different concrete types are incompatible
            _ => false,
        }
    }

    /// Extract the type of a field from a type.
    ///
    /// Handles structural objects, named structs, and type variables.
    pub(super) fn field_type(
        &mut self,
        base_ty: TyId,
        field: &str,
        span: Span,
    ) -> TyId {
        let resolved = self.ty_arena.get(base_ty).clone();
        match resolved {
            // Structural object: look up field in IndexMap
            Ty::Object(fields) => {
                let field_id = self.env.intern(field);
                fields.get(&field_id).copied().unwrap_or_else(|| {
                    self.error(TypeError::FieldNotFound {
                        ty: base_ty,
                        field: field.to_string(),
                        span,
                    });
                    TyArena::ERROR
                })
            }

            // Named type: check if it's an accessible alias to object and look up field
            Ty::Named(type_id, type_args) => {
                let field_id = self.env.intern(field);
                match self.object_alias(type_id) {
                    Some((params, fields)) => {
                        let field_ty_id = fields
                            .iter()
                            .find(|(n, _)| *n == field_id)
                            .map(|(_, ty)| *ty);
                        match field_ty_id {
                            Some(ty_id) => {
                                let subst: IndexMap<_, _> = params
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(p, &a)| (*p, a))
                                    .collect();
                                self.convert().ast_type_to_ty(ty_id, &subst)
                            }
                            None => {
                                self.error(TypeError::FieldNotFound {
                                    ty: base_ty,
                                    field: field.to_string(),
                                    span,
                                });
                                TyArena::ERROR
                            }
                        }
                    }
                    None => {
                        self.error(TypeError::NotAnObject(base_ty, span));
                        TyArena::ERROR
                    }
                }
            }

            // Type variable: create a HasField constraint. This only requires
            // the accessed field to exist, not all fields of the eventual type.
            Ty::Var(_) => {
                let field_ty = self.fresh();
                let field_id = self.env.intern(field);
                self.constrain(Constraint::HasField {
                    base: base_ty,
                    field: field_id,
                    field_ty,
                    span,
                });
                field_ty
            }

            // Json is dynamic: any field access returns Json
            Ty::Json => TyArena::JSON,

            // Error recovery: propagate error
            Ty::Error => TyArena::ERROR,

            // Unknown: field access might succeed at runtime
            Ty::Unknown => self.fresh(),

            // All other types don't have fields
            _ => {
                self.error(TypeError::NotAnObject(base_ty, span));
                TyArena::ERROR
            }
        }
    }

    /// Like `field_type`, but for optional field access (`?.`).
    ///
    /// For objects, missing fields return `Unknown` instead of erroring.
    /// For other types, delegates to `field_type`.
    pub(super) fn optional_field_type(
        &mut self,
        base_ty: TyId,
        field: &str,
        span: Span,
    ) -> TyId {
        let resolved = self.ty_arena.get(base_ty).clone();
        match resolved {
            Ty::Object(fields) => {
                let field_id = self.env.intern(field);
                fields.get(&field_id).copied().unwrap_or(TyArena::UNKNOWN)
            }
            _ => self.field_type(base_ty, field, span),
        }
    }
}
