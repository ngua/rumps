//! Type conversion utilities for the type checker.
//!
//! Contains methods for converting between AST type expressions, runtime type
//! representations, and static `Ty` / `TyId` types.

use std::borrow::Cow;

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::{Constraint, InferCtx};
use crate::ast::{AstTypeExpr, AstTypeExprId, Visibility};
use crate::intern::StringId;
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{BuiltinClass, BuiltinClassTag, Ty, TyArena, TyId};
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
            | Ty::Result(_, _)
            | Ty::Array(_)
            | Ty::Map(..)
            | Ty::Fn(..) => false,
            Ty::Tuple(ts) | Ty::Union(ts) => {
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

    /// Convert a runtime `TypeExprId` to a `TyId`.
    ///
    /// Used to convert union member types from the `TypeExprArena` (runtime
    /// representation) to `TyId` (static type representation).
    pub(crate) fn type_expr_to_ty(
        &mut self,
        id: crate::value::TypeExprId,
    ) -> TyId {
        let base = self.type_exprs.base_type(id);
        let arg_ids: SmallVec<[crate::value::TypeExprId; 4]> = self
            .type_exprs
            .type_args(id)
            .map(|a| a.iter().copied().collect())
            .unwrap_or_default();
        base.map_or(TyArena::UNKNOWN, |base| {
            let args: SmallVec<[TyId; 4]> =
                arg_ids.iter().map(|&p| self.type_expr_to_ty(p)).collect();
            let base_ty = self.type_id_to_ty(base);
            self.apply_type_args(base_ty, args)
        })
    }

    /// Convert a `TypeId` to a primitive `TyId` or `Ty::Named`.
    pub(super) fn type_id_to_ty(&mut self, id: TypeId) -> TyId {
        match id {
            TypeId::BOOL => TyArena::BOOL,
            TypeId::INT => TyArena::INT,
            TypeId::WORD => TyArena::WORD,
            TypeId::FLOAT => TyArena::FLOAT,
            TypeId::CHAR => TyArena::CHAR,
            TypeId::STRING => TyArena::STRING,
            TypeId::UNIT => TyArena::UNIT,
            TypeId::TIME => TyArena::TIME,
            TypeId::RANGE => TyArena::RANGE,
            TypeId::JSON => TyArena::JSON,
            TypeId::ORDERING => TyArena::ORDERING,
            TypeId::DATA_STATUS => TyArena::DATA_STATUS,
            TypeId::FILEPATH => TyArena::FILEPATH,
            TypeId::PATH => TyArena::PATH,
            TypeId::REGEX => TyArena::REGEX,
            TypeId::ERROR => TyArena::RUNTIME_ERROR,
            TypeId::LOCAL => TyArena::LOCAL,
            TypeId::GLOBAL => TyArena::GLOBAL,
            // Ref is a union type (Local | Global); return Named
            TypeId::REF => self.ty_arena.named(TypeId::REF, smallvec![]),
            _ => self.ty_arena.named(id, smallvec![]),
        }
    }

    /// Apply type arguments to a base type.
    ///
    /// Converts generic `Ty::Named` types to their specialized forms
    /// (e.g., `Named(ARRAY, [Int])` -> `Array(Int)`).
    pub(super) fn apply_type_args(
        &mut self,
        base: TyId,
        args: SmallVec<[TyId; 4]>,
    ) -> TyId {
        let base_ty = self.ty_arena.get(base).clone();
        match base_ty {
            Ty::Named(id, _) if id == TypeId::ARRAY => {
                if args.len() == 1 {
                    self.ty_arena.array(args[0])
                } else {
                    self.ty_arena.named(id, args)
                }
            }
            Ty::Named(id, _) if id == TypeId::OPTION => {
                if args.len() == 1 {
                    self.ty_arena.option(args[0])
                } else {
                    self.ty_arena.named(id, args)
                }
            }
            Ty::Named(id, _) if id == TypeId::MAP => {
                if args.len() == 2 {
                    self.ty_arena.map_ty(args[0], args[1])
                } else {
                    self.ty_arena.named(id, args)
                }
            }
            Ty::Named(id, _) if id == TypeId::RESULT => {
                if args.len() == 2 {
                    self.ty_arena.result(args[0], args[1])
                } else {
                    self.ty_arena.named(id, args)
                }
            }
            Ty::Named(id, _) if id == TypeId::TUPLE => {
                self.ty_arena.alloc(Ty::Tuple(args))
            }
            Ty::Named(id, _) => self.ty_arena.named(id, args),
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
    pub(crate) fn expand_union_members(
        &mut self,
        ty: TyId,
    ) -> Option<SmallVec<[TyId; 4]>> {
        let resolved = self.ty_arena.get(ty).clone();
        match resolved {
            Ty::Union(members) => Some(members),
            Ty::Named(id, _params) => {
                // Handle builtin unions by their known members
                if id == TypeId::STORABLE {
                    Some(TyArena::STORABLE_MEMBERS.iter().copied().collect())
                } else if id == TypeId::SCALAR {
                    Some(TyArena::SCALAR_MEMBERS.iter().copied().collect())
                } else {
                    // Check if it's a user-defined union
                    let members_opt =
                        self.registry.get_def(id).and_then(|def| match def {
                            TypeDef::Union { members, .. } => {
                                Some(members.clone())
                            }
                            _ => None,
                        });
                    members_opt.map(|members| {
                        members
                            .iter()
                            .map(|m| self.type_expr_to_ty(*m))
                            .collect()
                    })
                }
            }
            _ => None,
        }
    }

    /// Check if a type is a member of a union.
    ///
    /// Returns `true` if `member` is one of the types in `union_ty`.
    pub(crate) fn is_union_member(
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

    /// Convert an AST type expression to a `TyId`.
    ///
    /// The `subst` map substitutes type parameter names with concrete types;
    /// used for generic struct field resolution.
    pub(crate) fn ast_type_to_ty(
        &mut self,
        id: AstTypeExprId,
        subst: &IndexMap<StringId, TyId>,
    ) -> TyId {
        // Clone to avoid borrow issues with mutable ast reference
        match self.ast.get_type_expr(id).cloned() {
            None => {
                let span = self.ast.type_expr_span(id).unwrap_or_default();
                self.error(TypeError::UnknownType(
                    "<unknown>".to_string(),
                    span,
                ));
                TyArena::ERROR
            }
            Some(te) => match &te {
                AstTypeExpr::Wildcard => self.fresh(),
                AstTypeExpr::Named(name) => {
                    let name_id = self.env.intern(name);
                    // Check substitution first (for type params)
                    subst.get(&name_id).copied().unwrap_or_else(|| {
                        let span =
                            self.ast.type_expr_span(id).unwrap_or_default();

                        // Try module-aware resolution for user types
                        // Extract resolution data; only allocate when rewrite needed
                        let resolved =
                            self.resolve_type_name(name).map(|(tid, cow)| {
                                let rewrite = &*cow != name;
                                // Only allocate on rewrite; otherwise use `None`
                                // and reference `name` later
                                let qname = if rewrite {
                                    Some(cow.into_owned())
                                } else {
                                    None
                                };
                                (tid, qname)
                            });
                        match resolved {
                            Some((type_id, qname)) => {
                                // Effective name for checks (borrow or owned)
                                let eff = qname.as_deref().unwrap_or(name);
                                // Check visibility
                                if !self.check_type_visibility(eff, span) {
                                    TyArena::ERROR
                                } else {
                                    // Rewrite AST if name was resolved differently
                                    if let Some(ref q) = qname {
                                        self.ast.set_type_expr(
                                            id,
                                            AstTypeExpr::Named(q.clone()),
                                        );
                                    }
                                    // Check arity; builtins use expected_type_arity
                                    let exp = self
                                        .registry
                                        .type_param_count(type_id)
                                        .or_else(|| {
                                            Self::expected_type_arity(eff)
                                        })
                                        .unwrap_or(0);
                                    if exp > 0 {
                                        self.error(
                                            TypeError::TypeArityMismatch {
                                                name: qname.unwrap_or_else(
                                                    || name.to_string(),
                                                ),
                                                expected: exp,
                                                got: 0,
                                                span,
                                            },
                                        );
                                        TyArena::ERROR
                                    } else {
                                        self.type_id_to_ty(type_id)
                                    }
                                }
                            }
                            None => {
                                // Try builtin types
                                let expected = Self::expected_type_arity(name);
                                if let Some(exp) = expected {
                                    if exp > 0 {
                                        self.error(
                                            TypeError::TypeArityMismatch {
                                                name: name.clone(),
                                                expected: exp,
                                                got: 0,
                                                span,
                                            },
                                        );
                                        TyArena::ERROR
                                    } else {
                                        self.named_type_to_ty(name)
                                    }
                                } else {
                                    let ty = self.named_type_to_ty(name);
                                    if ty == TyArena::UNKNOWN {
                                        self.error(TypeError::UnknownType(
                                            name.clone(),
                                            span,
                                        ));
                                        TyArena::ERROR
                                    } else {
                                        ty
                                    }
                                }
                            }
                        }
                    })
                }
                AstTypeExpr::App(name, args) => {
                    let span = self.ast.type_expr_span(id).unwrap_or_default();

                    // Try module-aware resolution for user types
                    // Only allocate when rewrite needed
                    let resolved =
                        self.resolve_type_name(name).map(|(tid, cow)| {
                            let rewrite = &*cow != name;
                            let qname = if rewrite {
                                Some(cow.into_owned())
                            } else {
                                None
                            };
                            (tid, qname)
                        });
                    // Check for user-defined type (not builtin)
                    let user_def =
                        resolved.as_ref().and_then(|(tid, qname)| {
                            let eff = qname.as_deref().unwrap_or(name);
                            self.registry
                                .type_param_count(*tid)
                                .map(|exp| (*tid, eff, qname.clone(), exp))
                        });
                    match user_def {
                        Some((type_id, eff, qname, exp)) => {
                            // User-defined parameterized type
                            if !self.check_type_visibility(eff, span) {
                                TyArena::ERROR
                            } else {
                                // Rewrite AST if name was resolved differently
                                if let Some(ref q) = qname {
                                    self.ast.set_type_expr(
                                        id,
                                        AstTypeExpr::App(
                                            q.clone(),
                                            args.clone(),
                                        ),
                                    );
                                }
                                // Check arity
                                if args.len() != exp {
                                    self.error(TypeError::TypeArityMismatch {
                                        name: qname.unwrap_or_else(|| {
                                            name.to_string()
                                        }),
                                        expected: exp,
                                        got: args.len(),
                                        span,
                                    });
                                    TyArena::ERROR
                                } else {
                                    let arg_tys: SmallVec<[TyId; 4]> = args
                                        .iter()
                                        .map(|a| self.ast_type_to_ty(*a, subst))
                                        .collect();
                                    let base = self.type_id_to_ty(type_id);
                                    self.apply_type_args(base, arg_tys)
                                }
                            }
                        }
                        None => {
                            // Builtin or unknown parameterized type
                            let expected = Self::expected_type_arity(name);
                            if let Some(exp) = expected {
                                if args.len() != exp {
                                    self.error(TypeError::TypeArityMismatch {
                                        name: name.clone(),
                                        expected: exp,
                                        got: args.len(),
                                        span,
                                    });
                                    TyArena::ERROR
                                } else {
                                    let arg_tys: SmallVec<[TyId; 4]> = args
                                        .iter()
                                        .map(|a| self.ast_type_to_ty(*a, subst))
                                        .collect();
                                    self.parameterized_type_to_ty(name, arg_tys)
                                }
                            } else {
                                let arg_tys: SmallVec<[TyId; 4]> = args
                                    .iter()
                                    .map(|a| self.ast_type_to_ty(*a, subst))
                                    .collect();
                                let ty = self
                                    .parameterized_type_to_ty(name, arg_tys);
                                if ty == TyArena::UNKNOWN {
                                    self.error(TypeError::UnknownType(
                                        name.clone(),
                                        span,
                                    ));
                                    TyArena::ERROR
                                } else {
                                    ty
                                }
                            }
                        }
                    }
                }
                AstTypeExpr::Fn(params, ret) => {
                    let param_tys: SmallVec<[TyId; 4]> = params
                        .iter()
                        .map(|p| self.ast_type_to_ty(*p, subst))
                        .collect();
                    let ret_ty = self.ast_type_to_ty(*ret, subst);
                    self.ty_arena.func(param_tys, ret_ty)
                }
                AstTypeExpr::Tuple(elems) => {
                    let elem_tys: SmallVec<[TyId; 4]> = elems
                        .iter()
                        .map(|e| self.ast_type_to_ty(*e, subst))
                        .collect();
                    self.ty_arena.alloc(Ty::Tuple(elem_tys))
                }
                AstTypeExpr::Union(members) => {
                    if members.is_empty() {
                        let span =
                            self.ast.type_expr_span(id).unwrap_or_default();
                        self.error(TypeError::EmptyUnion(span));
                        TyArena::ERROR
                    } else {
                        let member_tys: SmallVec<[TyId; 4]> = members
                            .iter()
                            .map(|m| self.ast_type_to_ty(*m, subst))
                            .collect();
                        self.ty_arena.alloc(Ty::Union(member_tys))
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
                    self.ty_arena.alloc(Ty::Object(field_tys))
                }
                AstTypeExpr::VarApp(name, args) => {
                    let name_id = self.env.intern(name);
                    let span = self.ast.type_expr_span(id).unwrap_or_default();
                    let arg_tys: SmallVec<[TyId; 4]> = args
                        .iter()
                        .map(|&a| self.ast_type_to_ty(a, subst))
                        .collect();
                    match subst.get(&name_id).copied() {
                        None => {
                            self.error(TypeError::UnknownType(
                                name.clone(),
                                span,
                            ));
                            TyArena::ERROR
                        }
                        Some(tid) => {
                            let resolved = self.ty_arena.get(tid).clone();
                            match resolved {
                                Ty::Var(tv) => self.ty_arena.hkt(tv, arg_tys),
                                _ => self.apply_type_args(tid, arg_tys),
                            }
                        }
                    }
                }
                AstTypeExpr::AssocType { class, name } => {
                    let span = self.ast.type_expr_span(id).unwrap_or_default();
                    let name_id = self.env.intern(name);

                    match class {
                        // Unqualified `:Index`: resolve from class context
                        None => self
                            .class_context
                            .as_ref()
                            .and_then(|ctx| {
                                ctx.assoc_types.get(&name_id).copied()
                            })
                            .unwrap_or_else(|| {
                                self.error(TypeError::AssocTypeOutsideClass {
                                    name: name.clone(),
                                    span,
                                });
                                TyArena::ERROR
                            }),

                        // Qualified `Indexable:Index`: create AssocType
                        // Validation happens during resolution in unify.rs
                        Some(class_name) => {
                            BuiltinClassTag::from_str(class_name)
                                .map(|kind| {
                                    let tv = self.fresh_var();
                                    self.ty_arena
                                        .alloc(Ty::AssocType(tv, kind, name_id))
                                })
                                .unwrap_or_else(|| {
                                    self.error(TypeError::UnknownClass(
                                        class_name.clone(),
                                        span,
                                    ));
                                    TyArena::ERROR
                                })
                        }
                    }
                }
            },
        }
    }

    /// Convert a simple named type to a `TyId`.
    pub(super) fn named_type_to_ty(&mut self, name: &str) -> TyId {
        Self::builtin_type_from_name(name).unwrap_or_else(|| {
            // Look up in registry
            self.env
                .lookup_str(name)
                .and_then(|id| self.registry.lookup(id))
                .map_or(TyArena::UNKNOWN, |ty_id| {
                    self.expand_alias_or_named(ty_id, smallvec![])
                })
        })
    }

    /// Map builtin type names to their `TyId` representation.
    ///
    /// Returns `None` for non-builtin names. This is the single source of
    /// truth for simple (non-parameterized) builtin type names.
    fn builtin_type_from_name(name: &str) -> Option<TyId> {
        match name {
            "Bool" => Some(TyArena::BOOL),
            "Int" => Some(TyArena::INT),
            "Word" => Some(TyArena::WORD),
            "Float" => Some(TyArena::FLOAT),
            "Char" => Some(TyArena::CHAR),
            "String" => Some(TyArena::STRING),
            "Unit" => Some(TyArena::UNIT),
            "Time" => Some(TyArena::TIME),
            "Range" => Some(TyArena::RANGE),
            "Json" => Some(TyArena::JSON),
            "Ordering" => Some(TyArena::ORDERING),
            "DataStatus" => Some(TyArena::DATA_STATUS),
            "FilePath" => Some(TyArena::FILEPATH),
            "Path" => Some(TyArena::PATH),
            "Regex" => Some(TyArena::REGEX),
            "Error" => Some(TyArena::RUNTIME_ERROR),
            "Local" => Some(TyArena::LOCAL),
            "Global" => Some(TyArena::GLOBAL),
            "Ref" => None, // Requires arena allocation; handled by caller
            _ => None,
        }
    }

    /// Convert a `TypeId` to `Ty::Named`.
    ///
    /// Aliases are NOT expanded here; they remain as `Ty::Named` so we can
    /// detect them later (e.g., for extensible record semantics). Expansion
    /// happens in unification and field access as needed.
    fn expand_alias_or_named(
        &mut self,
        type_id: TypeId,
        args: SmallVec<[TyId; 4]>,
    ) -> TyId {
        self.ty_arena.named(type_id, args)
    }

    /// Convert a `BuiltinClass<AstTypeExprId>` to a `BuiltinClass<TyId>`,
    /// resolving type parameter references via `subst`.
    pub(super) fn ast_class_to_ty_class(
        &mut self,
        c: &BuiltinClass<AstTypeExprId>,
        subst: &IndexMap<StringId, TyId>,
    ) -> BuiltinClass<TyId> {
        c.map_ref(|id| self.ast_type_to_ty(*id, subst))
    }

    /// Convert a parameterized type to a `TyId`.
    pub(super) fn parameterized_type_to_ty(
        &mut self,
        name: &str,
        args: SmallVec<[TyId; 4]>,
    ) -> TyId {
        match name {
            "Array" => args
                .first()
                .map_or(TyArena::ERROR, |&t| self.ty_arena.array(t)),
            "Option" => args
                .first()
                .map_or(TyArena::ERROR, |&t| self.ty_arena.option(t)),
            "Result" => args.first().map_or(TyArena::ERROR, |&ok| {
                args.get(1).map_or(TyArena::ERROR, |&err| {
                    self.ty_arena.result(ok, err)
                })
            }),
            "Map" => args.first().map_or(TyArena::ERROR, |&k| {
                args.get(1)
                    .map_or(TyArena::ERROR, |&v| self.ty_arena.map_ty(k, v))
            }),
            _ => {
                // User-defined parameterized type
                self.env
                    .lookup_str(name)
                    .and_then(|id| self.registry.lookup(id))
                    .map_or(TyArena::UNKNOWN, |ty_id| {
                        self.expand_alias_or_named(ty_id, args)
                    })
            }
        }
    }

    /// Returns the expected type argument arity for builtin parameterized types.
    ///
    /// Returns `None` for user-defined types (arity checked elsewhere).
    fn expected_type_arity(name: &str) -> Option<usize> {
        match name {
            "Array" | "Option" => Some(1),
            "Result" | "Map" => Some(2),
            _ => None,
        }
    }

    /// Check visibility for a module-qualified type name.
    ///
    /// Returns `true` if the type is accessible, `false` if private.
    /// Emits a `PrivateAccess` error for private types.
    ///
    /// Non-module types (no `.` in name) always return `true`.
    fn check_type_visibility(&mut self, name: &str, span: Span) -> bool {
        // Only check visibility for module-qualified types
        name.contains('.')
            .then(|| {
                self.env
                    .lookup_user_module_type_vis(name)
                    .is_none_or(|vis| {
                        if vis == Visibility::Private {
                            // Extract module path and type name for error
                            let parts: Vec<_> = name.split('.').collect();
                            let (type_name, module_parts) = parts
                                .split_last()
                                .map_or(("", vec![]), |(t, m)| {
                                    (*t, m.to_vec())
                                });
                            let module = module_parts.join(".");
                            self.error(TypeError::PrivateAccess {
                                module,
                                name: type_name.to_string(),
                                span,
                            });
                            false
                        } else {
                            true
                        }
                    })
            })
            .unwrap_or(true)
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

            // Named type: check if it's an alias to object and look up field
            Ty::Named(type_id, type_args) => {
                let field_id = self.env.intern(field);
                let def = self.registry.get_def(type_id);
                match def {
                    Some(TypeDef::Alias {
                        type_params,
                        target,
                        ..
                    }) => {
                        // Check if target is an object type
                        let target = *target;
                        let params: smallvec::SmallVec<[_; 2]> =
                            type_params.clone();
                        match self.ast.get_type_expr(target).cloned() {
                            Some(AstTypeExpr::Object(fields)) => {
                                // Find field in object
                                let field_ty_id = fields
                                    .iter()
                                    .find(|(n, _)| {
                                        self.env.intern(n) == field_id
                                    })
                                    .map(|(_, ty)| *ty);
                                match field_ty_id {
                                    Some(ty_id) => {
                                        let subst: IndexMap<_, _> = params
                                            .iter()
                                            .zip(type_args.iter())
                                            .map(|(p, &a)| (*p, a))
                                            .collect();
                                        self.ast_type_to_ty(ty_id, &subst)
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
                            _ => {
                                self.error(TypeError::NotAnObject(
                                    base_ty, span,
                                ));
                                TyArena::ERROR
                            }
                        }
                    }
                    Some(_) => {
                        self.error(TypeError::NotAnObject(base_ty, span));
                        TyArena::ERROR
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

    /// Resolve a type name, returning `(TypeId, qualified_name)` if found.
    ///
    /// Resolution order:
    /// 1. Check imported types first
    /// 2. Try exact name (already qualified or top-level)
    /// 3. If inside a module, try prefixing with current module, then parent
    pub(super) fn resolve_type_name<'a>(
        &'a self,
        name: &'a str,
    ) -> Option<(TypeId, Cow<'a, str>)> {
        // 1. Check imported types
        let effective: Cow<str> = self
            .env
            .lookup_imported_type(name)
            .map(Cow::Borrowed)
            .unwrap_or(Cow::Borrowed(name));

        // 2. Try exact lookup
        self.try_lookup_type(&effective)
            .map(|id| (id, effective.clone()))
            .or_else(|| {
                // 3. Try module prefixes (only if not already qualified)
                if effective.contains('.') {
                    None
                } else {
                    self.current_module.as_ref().and_then(|mod_path| {
                        std::iter::successors(Some(mod_path.as_str()), |p| {
                            p.rsplit_once('.').map(|(parent, _)| parent)
                        })
                        .find_map(|prefix| {
                            let qname = format!("{}.{}", prefix, effective);
                            self.try_lookup_type(&qname)
                                .map(|id| (id, Cow::Owned(qname)))
                        })
                    })
                }
            })
    }

    /// Try to look up a type by exact name.
    fn try_lookup_type(&self, name: &str) -> Option<TypeId> {
        self.env
            .lookup_str(name)
            .and_then(|id| self.registry.lookup(id))
    }

    /// Check if `name` refers to a known (builtin or user-defined) type,
    /// as opposed to a type variable.
    pub(super) fn is_known_type_name(&self, name: &str) -> bool {
        Self::builtin_type_from_name(name).is_some()
            || Self::expected_type_arity(name).is_some()
            || self.resolve_type_name(name).is_some()
    }

    /// Collect type variable names from an AST type expression.
    ///
    /// Recursively walks the type expression tree, returning names that
    /// are type variables (i.e. not known/builtin types). Used to extract
    /// implicit type parameters from `for_type` in class instances.
    pub(super) fn collect_type_vars_from_ast(
        &self,
        id: AstTypeExprId,
    ) -> SmallVec<[String; 4]> {
        let mut out = SmallVec::new();
        self.collect_type_vars_rec(id, &mut out);
        out
    }

    /// Merge type variables from a `for_type` AST node into a substitution map.
    ///
    /// Type variables appearing in `for_type` (e.g. `T` in `X[T]`) that are
    /// not already present in `subst` get fresh type variables allocated.
    pub(super) fn merge_for_type_vars(
        &mut self,
        for_type: AstTypeExprId,
        subst: &mut IndexMap<StringId, TyId>,
    ) {
        self.collect_type_vars_from_ast(for_type)
            .into_iter()
            .for_each(|name| {
                let id = self.env.intern(&name);
                if !subst.contains_key(&id) {
                    let tv = self.fresh_var();
                    subst.insert(id, self.ty_arena.alloc(Ty::Var(tv)));
                }
            });
    }

    fn collect_type_vars_rec(
        &self,
        id: AstTypeExprId,
        out: &mut SmallVec<[String; 4]>,
    ) {
        if let Some(te) = self.ast.get_type_expr(id).cloned() {
            match te {
                AstTypeExpr::Named(name) => {
                    if !self.is_known_type_name(&name) {
                        out.push(name);
                    }
                }
                AstTypeExpr::VarApp(name, args) => {
                    out.push(name);
                    args.iter()
                        .for_each(|a| self.collect_type_vars_rec(*a, out));
                }
                AstTypeExpr::App(_, args) => {
                    args.iter()
                        .for_each(|a| self.collect_type_vars_rec(*a, out));
                }
                AstTypeExpr::Fn(params, ret) => {
                    params
                        .iter()
                        .for_each(|p| self.collect_type_vars_rec(*p, out));
                    self.collect_type_vars_rec(ret, out);
                }
                AstTypeExpr::Tuple(es) | AstTypeExpr::Union(es) => {
                    es.iter().for_each(|e| self.collect_type_vars_rec(*e, out));
                }
                AstTypeExpr::Object(fs) => {
                    fs.iter().for_each(|(_, t)| {
                        self.collect_type_vars_rec(*t, out);
                    });
                }
                AstTypeExpr::Wildcard | AstTypeExpr::AssocType { .. } => {}
            }
        }
    }
}
