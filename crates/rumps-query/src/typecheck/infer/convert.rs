//! Type conversion utilities for the type checker.
//!
//! Contains methods for converting between AST type expressions, runtime type
//! representations, and static `Ty` types.

use std::collections::HashMap;

use super::{Constraint, InferCtx};
use crate::ast::{AstTypeExpr, AstTypeExprId};
use crate::intern::StringId;
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, Ty};
use crate::value::{TypeDef, TypeId};
use crate::Span;

impl InferCtx<'_> {
    /// Check if a type contains unresolved type variables that require
    /// annotation.
    ///
    /// Polymorphic types like `Option[?t]`, `Result[?t, ?e]`, `Array[?t]`, and
    /// `Fn([?t], ?u)` are allowed with unresolved type parameters; they
    /// represent values like `None`, `Err`, `[]`, or closures passed to HOFs
    /// where the type parameter doesn't affect runtime behavior or is
    /// determined by the calling context.
    pub(super) fn has_unresolved_vars(ty: &Ty) -> bool {
        match ty {
            // Ty::Unknown indicates true ambiguity that requires annotation.
            // Ty::Var is allowed; unresolved type variables are OK for
            // polymorphic expressions (e.g., `id` function, HOF results).
            Ty::Unknown => true,
            Ty::Var(_)
            | Ty::Bool
            | Ty::Int
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
            | Ty::Error => false,
            // Option, Result, Array, Map, and Fn with unresolved type params are
            // OK. These are intentionally polymorphic (e.g., `Option.None`,
            // `Result.Err("msg")`, `[]`, `Map.empty()`, closures passed to HOFs).
            Ty::Option(_)
            | Ty::Result(_, _)
            | Ty::Array(_)
            | Ty::Map(..)
            | Ty::Fn(..) => false,
            Ty::Tuple(ts) | Ty::Union(ts) => {
                ts.iter().any(Self::has_unresolved_vars)
            }
            Ty::Object(fields) => {
                fields.values().any(Self::has_unresolved_vars)
            }
            Ty::Named(_, args) => args.iter().any(Self::has_unresolved_vars),
        }
    }

    /// Convert a runtime `TypeExprId` to a static `Ty`.
    ///
    /// Used to convert union member types from the `TypeExprArena` (runtime
    /// representation) to `Ty` (static type representation).
    pub(super) fn type_expr_to_ty(&self, id: crate::value::TypeExprId) -> Ty {
        self.type_exprs.base_type(id).map_or(Ty::Unknown, |base| {
            let args: Vec<Ty> = self
                .type_exprs
                .type_args(id)
                .map(|a| a.iter().map(|p| self.type_expr_to_ty(*p)).collect())
                .unwrap_or_default();
            self.apply_type_args(self.type_id_to_ty(base), args)
        })
    }

    /// Convert a `TypeId` to a primitive `Ty` or `Ty::Named`.
    pub(super) fn type_id_to_ty(&self, id: TypeId) -> Ty {
        match id {
            TypeId::BOOL => Ty::Bool,
            TypeId::INT => Ty::Int,
            TypeId::FLOAT => Ty::Float,
            TypeId::CHAR => Ty::Char,
            TypeId::STRING => Ty::String,
            TypeId::UNIT => Ty::Unit,
            TypeId::TIME => Ty::Time,
            TypeId::RANGE => Ty::Range,
            TypeId::JSON => Ty::Json,
            _ => Ty::Named(id, vec![]),
        }
    }

    /// Apply type arguments to a base type.
    ///
    /// Converts generic `Ty::Named` types to their specialized forms
    /// (e.g., `Named(ARRAY, [Int])` -> `Array(Int)`).
    pub(super) fn apply_type_args(&self, base: Ty, args: Vec<Ty>) -> Ty {
        match base {
            Ty::Named(id, _) if id == TypeId::ARRAY => <[_; 1]>::try_from(args)
                .map_or_else(
                    |v| Ty::Named(id, v),
                    |[e]| Ty::Array(Box::new(e)),
                ),
            Ty::Named(id, _) if id == TypeId::OPTION => <[_; 1]>::try_from(
                args,
            )
            .map_or_else(|v| Ty::Named(id, v), |[e]| Ty::Option(Box::new(e))),
            Ty::Named(id, _) if id == TypeId::MAP => <[_; 2]>::try_from(args)
                .map_or_else(
                    |v| Ty::Named(id, v),
                    |[k, v]| Ty::Map(Box::new(k), Box::new(v)),
                ),
            Ty::Named(id, _) if id == TypeId::RESULT => {
                <[_; 2]>::try_from(args).map_or_else(
                    |v| Ty::Named(id, v),
                    |[ok, err]| Ty::Result(Box::new(ok), Box::new(err)),
                )
            }
            Ty::Named(id, _) if id == TypeId::TUPLE => Ty::Tuple(args),
            Ty::Named(id, _) => Ty::Named(id, args),
            _ => base,
        }
    }

    /// Expand a union type to its member types.
    ///
    /// Returns `Some(members)` for union types, `None` for non-unions.
    ///
    /// # Union Representations
    ///
    /// - **Anonymous unions** (`Ty::Union`): Members returned directly.
    /// - **Named unions** (`Ty::Named` with `TypeDef::Union`): Members looked up
    ///   from registry. Builtin unions (`Storable`, `Scalar`) are hardcoded;
    ///   user-defined unions are resolved via `type_expr_to_ty`.
    ///
    /// Named unions use `Ty::Named` (not `Ty::Union`) to preserve nominal
    /// identity. This matters for `Storable`'s special `AS` semantics: casting
    /// `x AS Storable` is infallible at compile time but may fail at runtime
    /// with `Error::RuntimeType`. See `Ty::Union` docs for full rationale.
    pub(crate) fn expand_union_members(&self, ty: &Ty) -> Option<Vec<Ty>> {
        match ty {
            Ty::Union(members) => Some(members.clone()),
            Ty::Named(id, _params) => {
                // Handle builtin unions by their known members
                if *id == TypeId::STORABLE {
                    Some(Ty::STORABLE_MEMBERS.to_vec())
                } else if *id == TypeId::SCALAR {
                    Some(Ty::SCALAR_MEMBERS.to_vec())
                } else {
                    // Check if it's a user-defined union
                    self.registry.get_def(*id).and_then(|def| match def {
                        TypeDef::Union { members, .. } => Some(
                            members
                                .iter()
                                .map(|m| self.type_expr_to_ty(*m))
                                .collect(),
                        ),
                        _ => None,
                    })
                }
            }
            _ => None,
        }
    }

    /// Check if a type is a member of a union.
    ///
    /// Returns `true` if `member` is one of the types in `union_ty`.
    pub(crate) fn is_union_member(&self, union_ty: &Ty, member: &Ty) -> bool {
        self.expand_union_members(union_ty)
            .is_some_and(|members| members.iter().any(|m| m == member))
    }

    /// Check if two types are compatible (can unify without error).
    ///
    /// Used to detect heterogeneous arrays before generating constraints.
    pub(super) fn types_compatible(&self, a: &Ty, b: &Ty) -> bool {
        match (a, b) {
            // Same primitive types
            (Ty::Bool, Ty::Bool)
            | (Ty::Int, Ty::Int)
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
            (Ty::Array(a), Ty::Array(b)) => self.types_compatible(a, b),

            // Options: inner types must be compatible
            (Ty::Option(a), Ty::Option(b)) => self.types_compatible(a, b),

            // Tuples: same length and pairwise compatible
            (Ty::Tuple(a), Ty::Tuple(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|(x, y)| self.types_compatible(x, y))
            }

            // Named types: same TypeId
            (Ty::Named(a, _), Ty::Named(b, _)) => a == b,

            // Objects: same field names and compatible field types
            (Ty::Object(a), Ty::Object(b)) => {
                a.len() == b.len()
                    && a.keys().all(|k| b.contains_key(k))
                    && a.iter().all(|(k, ty_a)| {
                        b.get(k).is_some_and(|ty_b| {
                            self.types_compatible(ty_a, ty_b)
                        })
                    })
            }

            // Results: both inner types must be compatible
            (Ty::Result(ok_a, err_a), Ty::Result(ok_b, err_b)) => {
                self.types_compatible(ok_a, ok_b)
                    && self.types_compatible(err_a, err_b)
            }

            // Different concrete types are incompatible
            _ => false,
        }
    }

    /// Convert an AST type expression to a `Ty`.
    ///
    /// The `subst` map substitutes type parameter names with concrete types;
    /// used for generic struct field resolution.
    pub(crate) fn ast_type_to_ty(
        &mut self,
        id: AstTypeExprId,
        subst: &HashMap<StringId, Ty>,
    ) -> Ty {
        // Clone to avoid borrow issues with mutable ast reference
        match self.ast.get_type_expr(id).cloned() {
            None => {
                let span = self.ast.type_expr_span(id).unwrap_or_default();
                self.error(TypeError::UnknownType(
                    "<unknown>".to_string(),
                    span,
                ));
                Ty::Error
            }
            Some(te) => match &te {
                AstTypeExpr::Named(name) => {
                    let name_id = self.env.intern(name);
                    // Check substitution first (for type params)
                    let ty = subst
                        .get(&name_id)
                        .cloned()
                        .unwrap_or_else(|| self.named_type_to_ty(name));
                    // Emit error for unknown types
                    if ty == Ty::Unknown {
                        let span =
                            self.ast.type_expr_span(id).unwrap_or_default();
                        self.error(TypeError::UnknownType(name.clone(), span));
                        Ty::Error
                    } else {
                        ty
                    }
                }
                AstTypeExpr::App(name, args) => {
                    let arg_tys: Vec<_> = args
                        .iter()
                        .map(|a| self.ast_type_to_ty(*a, subst))
                        .collect();
                    let ty = self.parameterized_type_to_ty(name, arg_tys);
                    // Emit error for unknown parameterized types
                    if ty == Ty::Unknown {
                        let span =
                            self.ast.type_expr_span(id).unwrap_or_default();
                        self.error(TypeError::UnknownType(name.clone(), span));
                        Ty::Error
                    } else {
                        ty
                    }
                }
                AstTypeExpr::Fn(params, ret) => {
                    let param_tys: Vec<_> = params
                        .iter()
                        .map(|p| self.ast_type_to_ty(*p, subst))
                        .collect();
                    let ret_ty = self.ast_type_to_ty(*ret, subst);
                    Ty::Fn(param_tys, Box::new(ret_ty))
                }
                AstTypeExpr::Tuple(elems) => {
                    let elem_tys: Vec<_> = elems
                        .iter()
                        .map(|e| self.ast_type_to_ty(*e, subst))
                        .collect();
                    Ty::Tuple(elem_tys)
                }
                AstTypeExpr::Union(members) => {
                    if members.is_empty() {
                        let span = self
                            .ast
                            .type_expr_span(id)
                            .unwrap_or(Span::new(0, 0));
                        self.error(TypeError::EmptyUnion(span));
                        Ty::Error
                    } else {
                        let member_tys: Vec<_> = members
                            .iter()
                            .map(|m| self.ast_type_to_ty(*m, subst))
                            .collect();
                        Ty::Union(member_tys)
                    }
                }
                AstTypeExpr::Object(fields) => {
                    let field_tys = fields
                        .iter()
                        .map(|(name, ty_id)| {
                            let name_id = self.env.intern(name);
                            let ty = self.ast_type_to_ty(*ty_id, subst);
                            (name_id, ty)
                        })
                        .collect();
                    Ty::Object(field_tys)
                }
            },
        }
    }

    /// Convert a simple named type to `Ty`.
    pub(super) fn named_type_to_ty(&self, name: &str) -> Ty {
        match name {
            "Bool" => Ty::Bool,
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Char" => Ty::Char,
            "String" => Ty::String,
            "Unit" => Ty::Unit,
            "Time" => Ty::Time,
            "Range" => Ty::Range,
            "Json" => Ty::Json,
            "Ordering" => Ty::Ordering,
            "DataStatus" => Ty::DataStatus,
            "FilePath" => Ty::FilePath,
            "Path" => Ty::Path,
            _ => {
                // Look up in registry
                self.env
                    .lookup_str(name)
                    .and_then(|id| self.registry.lookup(id))
                    .map_or(Ty::Unknown, |ty_id| Ty::Named(ty_id, vec![]))
            }
        }
    }

    /// Convert a parameterized type to `Ty`.
    pub(super) fn parameterized_type_to_ty(
        &self,
        name: &str,
        args: Vec<Ty>,
    ) -> Ty {
        match name {
            "Array" => args
                .into_iter()
                .next()
                .map_or(Ty::Error, |t| Ty::Array(Box::new(t))),
            "Option" => args
                .into_iter()
                .next()
                .map_or(Ty::Error, |t| Ty::Option(Box::new(t))),
            "Result" => {
                let mut it = args.into_iter();
                it.next().map_or(Ty::Error, |ok| {
                    it.next().map_or(Ty::Error, |err| {
                        Ty::Result(Box::new(ok), Box::new(err))
                    })
                })
            }
            "Map" => {
                let mut it = args.into_iter();
                it.next().map_or(Ty::Error, |k| {
                    it.next().map_or(Ty::Error, |v| {
                        Ty::Map(Box::new(k), Box::new(v))
                    })
                })
            }
            _ => {
                // User-defined parameterized type
                self.env
                    .lookup_str(name)
                    .and_then(|id| self.registry.lookup(id))
                    .map_or(Ty::Unknown, |ty_id| Ty::Named(ty_id, args))
            }
        }
    }

    /// Extract the type of a field from a type.
    ///
    /// Handles structural objects, named structs, and type variables.
    pub(super) fn field_type(
        &mut self,
        base_ty: &Ty,
        field: &str,
        span: Span,
    ) -> Ty {
        match base_ty {
            // Structural object: look up field in IndexMap
            Ty::Object(fields) => {
                let field_id = self.env.intern(field);
                fields.get(&field_id).cloned().unwrap_or_else(|| {
                    self.error(TypeError::FieldNotFound {
                        ty: base_ty.clone(),
                        field: field.to_string(),
                        span,
                    });
                    Ty::Error
                })
            }

            // Named type: check if it's a struct and look up field
            Ty::Named(type_id, type_args) => {
                let field_id = self.env.intern(field);
                let def = self.registry.get_def(*type_id);
                match def {
                    Some(TypeDef::Struct {
                        type_params,
                        fields,
                        ..
                    }) => {
                        // Copy what we need before borrowing self mutably
                        let field_ty_id = fields.get(&field_id).copied();
                        let params: smallvec::SmallVec<[_; 2]> =
                            type_params.clone();
                        match field_ty_id {
                            Some(ty_id) => {
                                let subst: HashMap<_, _> = params
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(p, a)| (*p, a.clone()))
                                    .collect();
                                self.ast_type_to_ty(ty_id, &subst)
                            }
                            None => {
                                self.error(TypeError::FieldNotFound {
                                    ty: base_ty.clone(),
                                    field: field.to_string(),
                                    span,
                                });
                                Ty::Error
                            }
                        }
                    }
                    Some(_) => {
                        self.error(TypeError::NotAnObject(
                            base_ty.clone(),
                            span,
                        ));
                        Ty::Error
                    }
                    None => {
                        self.error(TypeError::NotAnObject(
                            base_ty.clone(),
                            span,
                        ));
                        Ty::Error
                    }
                }
            }

            // Type variable: create a HasField constraint. This only requires
            // the accessed field to exist, not all fields of the eventual type.
            Ty::Var(_) => {
                let field_ty = self.fresh();
                let field_id = self.env.intern(field);
                self.constrain(Constraint::HasField {
                    base: base_ty.clone(),
                    field: field_id,
                    field_ty: field_ty.clone(),
                    span,
                });
                field_ty
            }

            // Json is dynamic: any field access returns Json
            Ty::Json => Ty::Json,

            // Error recovery: propagate error
            Ty::Error => Ty::Error,

            // Unknown: field access might succeed at runtime
            Ty::Unknown => self.fresh(),

            // All other types don't have fields
            _ => {
                self.error(TypeError::NotAnObject(base_ty.clone(), span));
                Ty::Error
            }
        }
    }
}
