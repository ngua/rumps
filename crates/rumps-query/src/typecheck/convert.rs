//! Shared type conversion context for inference and constraint solving.
//!
//! `ConvertCtx` is the single source of truth for converting AST type
//! expressions to `TyId`s. Both `InferCtx` (inference phase) and `SolveCtx`
//! (solve phase) construct a `ConvertCtx` on demand to perform conversions.

use std::iter;

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::decl::TypeDeclRegistry;
use super::env::TypeEnv;
use super::error::TypeError;
use super::infer::ClassContext;
use super::ty::{Ty, TyArena, TyId, TypeClass};
use super::uf::UnionFind;
use crate::ast::{Ast, AstTypeExpr, AstTypeExprId, Visibility};
use crate::intern::{QualifiedName, StringId};
use crate::value::{TypeDef, TypeId, TypeRegistry};
use crate::{ClassId, Span};

/// Shared context for AST-to-`TyId` conversion.
///
/// Both `InferCtx` and `SolveCtx` construct this on demand. The `rewrite_ast`
/// flag controls whether `ast_type_to_ty` rewrites the AST when it
/// module-qualifies an unqualified name: `true` during inference (names
/// are being resolved), `false` during solving (names already resolved).
pub(super) struct ConvertCtx<'a> {
    pub(super) ty_arena: &'a mut TyArena,
    pub(super) uf: &'a mut UnionFind,
    pub(super) registry: &'a TypeRegistry,
    pub(super) decls: &'a TypeDeclRegistry,
    pub(super) env: &'a TypeEnv,
    pub(super) ast: &'a mut Ast,
    pub(super) errors: &'a mut Vec<TypeError>,
    pub(super) current_module: &'a Option<QualifiedName>,
    pub(super) class_context: &'a Option<ClassContext>,
    /// If `true`, `ast_type_to_ty` rewrites the AST when it module-qualifies
    /// an unqualified name. Set by `InferCtx` (inference phase); cleared by
    /// `SolveCtx` (solve phase; names already resolved).
    pub(super) rewrite_ast: bool,
}

pub(super) fn is_in_module(
    site: &Option<QualifiedName>,
    def: &Option<QualifiedName>,
) -> bool {
    match (site, def) {
        (None, None) => true,
        (Some(site), Some(def)) => {
            site == def || site.segments().starts_with(def.segments())
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

impl ConvertCtx<'_> {
    /// Resolve a type name to its `TypeId` and effective qualified name.
    ///
    /// Resolution order:
    /// 1. Check imported types first
    /// 2. Try exact name (already qualified or top-level)
    /// 3. If inside a module, try prefixing with current module, then parent
    pub(super) fn resolve_type_name(
        &self,
        name: &QualifiedName,
    ) -> Option<(TypeId, QualifiedName)> {
        let local = name.local_name();
        let eff_qn = self
            .env
            .lookup_imported_type(local)
            .cloned()
            .unwrap_or_else(|| name.clone());

        self.registry
            .lookup(&eff_qn)
            .map(|id| (id, eff_qn.clone()))
            .or_else(|| {
                if eff_qn.is_qualified() {
                    None
                } else {
                    let eff_local = eff_qn.local_name();
                    self.current_module.as_ref().and_then(|mod_qn| {
                        iter::once(mod_qn.clone())
                            .chain(mod_qn.ancestors())
                            .find_map(|prefix| {
                                let qn = prefix.child(eff_local);
                                self.registry.lookup(&qn).map(|id| (id, qn))
                            })
                    })
                }
            })
    }

    /// Check visibility for a module-qualified type name.
    ///
    /// Returns `true` if the type is accessible, `false` if private.
    /// Emits a `PrivateAccess` error for private types.
    fn check_type_visibility(
        &mut self,
        qn: &QualifiedName,
        span: Span,
    ) -> bool {
        let def = qn.parent();
        let is_private = match self.env.lookup_user_module_type_vis(qn) {
            Some(Visibility::Private) => {
                !is_in_module(self.current_module, &def)
            }
            Some(Visibility::Public) | None => false,
        };
        if is_private {
            let local = self.env.resolve_str(qn.local_name()).to_owned();
            let module = qn
                .parent()
                .map_or_else(String::new, |p| p.display(&self.env.strings));
            self.errors.push(TypeError::PrivateAccess {
                module,
                name: local,
                span,
            });
        }
        !is_private
    }

    /// Check if `name` refers to a known (builtin or user-defined) type,
    /// as opposed to a type variable.
    fn is_known_type_name(&self, name: &QualifiedName) -> bool {
        let s = name.display(&self.env.strings);
        Self::builtin_type_from_name(&s).is_some()
            || Self::expected_type_arity(&s).is_some()
            || self.resolve_type_name(name).is_some()
    }

    /// Convert a `TypeId` to a primitive `TyId`, `Ty::Union`, or `Ty::Named`.
    ///
    /// Builtin and user-defined unions are expanded to
    /// `Ty::Union(Some(id), members)` at construction time, so all
    /// union-related logic goes through a single code path. `Ty::Named` is
    /// reserved for sum types and aliases.
    fn type_id_to_ty(&mut self, id: TypeId) -> TyId {
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
            TypeId::REF => self.ty_arena.ref_ty(),
            TypeId::STORABLE => self.ty_arena.storable(),
            TypeId::SCALAR => self.ty_arena.scalar(),
            TypeId::SUBSCRIPT => self.ty_arena.subscript(),
            _ => match self.registry.get_def(id) {
                Some(TypeDef::Union { members, .. }) => {
                    let members = members.clone();
                    let member_exprs =
                        self.decls.union_member_exprs(id).cloned();
                    let member_tys = if let Some(member_exprs) =
                        member_exprs.filter(|ms| !ms.is_empty())
                    {
                        member_exprs
                            .iter()
                            .map(|&m| self.ast_type_to_ty(m, &IndexMap::new()))
                            .collect()
                    } else {
                        members.iter().map(|&m| self.type_id_to_ty(m)).collect()
                    };
                    self.ty_arena.alloc(Ty::Union(Some(id), member_tys))
                }
                _ => self.ty_arena.named(id, smallvec![]),
            },
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
            Ty::Named(id, _) if id == TypeId::LAZY => {
                if args.len() == 1 {
                    self.ty_arena.lazy(args[0])
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

    pub(super) fn can_access_alias_repr(&self, id: TypeId) -> bool {
        match self.decls.alias_repr_vis(id) {
            Visibility::Public => true,
            Visibility::Private => {
                let def = self.decls.alias_module(id).cloned();
                is_in_module(self.current_module, &def)
            }
        }
    }

    /// Convert a simple named type to a `TyId`.
    pub(super) fn named_type_to_ty(&mut self, name: &str) -> TyId {
        Self::builtin_type_from_name(name).unwrap_or_else(|| {
            self.env
                .lookup_str(name)
                .and_then(|id| self.registry.lookup(&QualifiedName::local(id)))
                .map_or(TyArena::UNKNOWN, |ty_id| {
                    self.ty_arena.named(ty_id, smallvec![])
                })
        })
    }

    /// Convert a parameterized type to a `TyId`.
    fn parameterized_type_to_ty(
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
            "Lazy" => args
                .first()
                .map_or(TyArena::ERROR, |&t| self.ty_arena.lazy(t)),
            "Result" => args.first().map_or(TyArena::ERROR, |&ok| {
                args.get(1).map_or(TyArena::ERROR, |&err| {
                    self.ty_arena.result(ok, err)
                })
            }),
            "Map" => args.first().map_or(TyArena::ERROR, |&k| {
                args.get(1)
                    .map_or(TyArena::ERROR, |&v| self.ty_arena.map_ty(k, v))
            }),
            _ => self
                .env
                .lookup_str(name)
                .and_then(|id| self.registry.lookup(&QualifiedName::local(id)))
                .map_or(TyArena::UNKNOWN, |ty_id| {
                    self.ty_arena.named(ty_id, args)
                }),
        }
    }

    /// Convert an AST type expression to a `TyId`.
    ///
    /// The `subst` map substitutes type parameter names with concrete types;
    /// used for generic struct field resolution.
    pub(super) fn ast_type_to_ty(
        &mut self,
        id: AstTypeExprId,
        subst: &IndexMap<StringId, TyId>,
    ) -> TyId {
        match self.ast.get_type_expr(id).cloned() {
            None => {
                let span = self.ast.type_expr_span(id).unwrap_or_default();
                self.errors.push(TypeError::UnknownType(
                    "<unknown>".to_string(),
                    span,
                ));
                TyArena::ERROR
            }
            Some(te) => match &te {
                AstTypeExpr::Wildcard => {
                    let v = self.uf.fresh();
                    self.ty_arena.alloc(Ty::Var(v))
                }
                AstTypeExpr::Named(name) => subst
                    .get(&name.local_name())
                    .copied()
                    .unwrap_or_else(|| {
                        let span =
                            self.ast.type_expr_span(id).unwrap_or_default();
                        let resolved = self.resolve_type_name(name);
                        match resolved {
                            Some((type_id, qid)) => {
                                if !self.check_type_visibility(&qid, span) {
                                    TyArena::ERROR
                                } else {
                                    let eff = qid.display(&self.env.strings);
                                    let exp = self
                                        .registry
                                        .type_param_count(type_id)
                                        .or_else(|| {
                                            Self::expected_type_arity(&eff)
                                        })
                                        .unwrap_or(0);
                                    if self.rewrite_ast && qid != *name {
                                        self.ast.set_type_expr(
                                            id,
                                            AstTypeExpr::Named(qid),
                                        );
                                    }
                                    if exp > 0 {
                                        self.errors.push(
                                            TypeError::TypeArityMismatch {
                                                name: eff,
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
                                let name_s = name.display(&self.env.strings);
                                let expected =
                                    Self::expected_type_arity(&name_s);
                                if let Some(exp) = expected {
                                    if exp > 0 {
                                        self.errors.push(
                                            TypeError::TypeArityMismatch {
                                                name: name_s,
                                                expected: exp,
                                                got: 0,
                                                span,
                                            },
                                        );
                                        TyArena::ERROR
                                    } else {
                                        self.named_type_to_ty(&name_s)
                                    }
                                } else {
                                    let ty = self.named_type_to_ty(&name_s);
                                    if ty == TyArena::UNKNOWN {
                                        self.errors.push(
                                            TypeError::UnknownType(
                                                name_s, span,
                                            ),
                                        );
                                        TyArena::ERROR
                                    } else {
                                        ty
                                    }
                                }
                            }
                        }
                    }),
                AstTypeExpr::App(name, args) => {
                    let span = self.ast.type_expr_span(id).unwrap_or_default();
                    let resolved = self.resolve_type_name(name);
                    let user_def = resolved.and_then(|(tid, qid)| {
                        self.registry
                            .type_param_count(tid)
                            .map(|exp| (tid, qid, exp))
                    });
                    match user_def {
                        Some((type_id, qid, exp)) => {
                            if !self.check_type_visibility(&qid, span) {
                                TyArena::ERROR
                            } else {
                                let n = qid.display(&self.env.strings);
                                if self.rewrite_ast && qid != *name {
                                    self.ast.set_type_expr(
                                        id,
                                        AstTypeExpr::App(qid, args.clone()),
                                    );
                                }
                                if args.len() != exp {
                                    self.errors.push(
                                        TypeError::TypeArityMismatch {
                                            name: n,
                                            expected: exp,
                                            got: args.len(),
                                            span,
                                        },
                                    );
                                    TyArena::ERROR
                                } else {
                                    let arg_tys: SmallVec<[TyId; 4]> = args
                                        .iter()
                                        .map(|a| self.ast_type_to_ty(*a, subst))
                                        .collect();
                                    let union = self
                                        .registry
                                        .get_def(type_id)
                                        .and_then(|def| match def {
                                            TypeDef::Union {
                                                type_params,
                                                ..
                                            } => self
                                                .decls
                                                .union_member_exprs(type_id)
                                                .filter(|member_exprs| {
                                                    !member_exprs.is_empty()
                                                })
                                                .map(|member_exprs| {
                                                    (
                                                        type_params.clone(),
                                                        member_exprs.clone(),
                                                    )
                                                }),
                                            _ => None,
                                        });
                                    match union {
                                        Some((ps, ms)) => {
                                            let subst: IndexMap<
                                                StringId,
                                                TyId,
                                            > = ps
                                                .iter()
                                                .zip(arg_tys.iter())
                                                .map(|(&p, &a)| (p, a))
                                                .collect();
                                            let member_tys = ms
                                                .iter()
                                                .map(|&m| {
                                                    self.ast_type_to_ty(
                                                        m, &subst,
                                                    )
                                                })
                                                .collect();
                                            self.ty_arena.alloc(Ty::Union(
                                                Some(type_id),
                                                member_tys,
                                            ))
                                        }
                                        None => {
                                            let base =
                                                self.type_id_to_ty(type_id);
                                            self.apply_type_args(base, arg_tys)
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            let name_s = name.display(&self.env.strings);
                            let expected = Self::expected_type_arity(&name_s);
                            if let Some(exp) = expected {
                                if args.len() != exp {
                                    self.errors.push(
                                        TypeError::TypeArityMismatch {
                                            name: name_s,
                                            expected: exp,
                                            got: args.len(),
                                            span,
                                        },
                                    );
                                    TyArena::ERROR
                                } else {
                                    let arg_tys: SmallVec<[TyId; 4]> = args
                                        .iter()
                                        .map(|a| self.ast_type_to_ty(*a, subst))
                                        .collect();
                                    self.parameterized_type_to_ty(
                                        &name_s, arg_tys,
                                    )
                                }
                            } else {
                                let arg_tys: SmallVec<[TyId; 4]> = args
                                    .iter()
                                    .map(|a| self.ast_type_to_ty(*a, subst))
                                    .collect();
                                let ty = self
                                    .parameterized_type_to_ty(&name_s, arg_tys);
                                if ty == TyArena::UNKNOWN {
                                    self.errors.push(TypeError::UnknownType(
                                        name_s, span,
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
                        self.errors.push(TypeError::EmptyUnion(span));
                        TyArena::ERROR
                    } else {
                        let member_tys: SmallVec<[TyId; 4]> = members
                            .iter()
                            .map(|m| self.ast_type_to_ty(*m, subst))
                            .collect();
                        self.ty_arena.alloc(Ty::Union(None, member_tys))
                    }
                }
                AstTypeExpr::Object(fields) => {
                    let field_tys = fields
                        .iter()
                        .map(|(name, ty_id)| {
                            let ty = self.ast_type_to_ty(*ty_id, subst);
                            (*name, ty)
                        })
                        .collect();
                    self.ty_arena.alloc(Ty::Object(field_tys))
                }
                AstTypeExpr::VarApp(name, args) => {
                    let span = self.ast.type_expr_span(id).unwrap_or_default();
                    let arg_tys: SmallVec<[TyId; 4]> = args
                        .iter()
                        .map(|&a| self.ast_type_to_ty(a, subst))
                        .collect();
                    match subst.get(&name.local_name()).copied() {
                        None => {
                            self.errors.push(TypeError::UnknownType(
                                name.display(&self.env.strings),
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
                AstTypeExpr::TupleConstructor { .. } => {
                    let span = self.ast.type_expr_span(id).unwrap_or_default();
                    self.errors.push(TypeError::Custom {
                        msg: "tuple constructors (`(,)`) can only appear \
                              in the `for` clause of a class instance"
                            .into(),
                        span,
                    });
                    TyArena::ERROR
                }
                AstTypeExpr::AssocType { class, name } => {
                    let span = self.ast.type_expr_span(id).unwrap_or_default();
                    match class {
                        None => self
                            .class_context
                            .as_ref()
                            .and_then(|ctx| ctx.assoc_types.get(name).copied())
                            .unwrap_or_else(|| {
                                self.errors.push(
                                    TypeError::AssocTypeOutsideClass {
                                        name: self.env.resolve_string(*name),
                                        span,
                                    },
                                );
                                TyArena::ERROR
                            }),
                        Some(class_name) => self
                            .env
                            .class_registry()
                            .lookup_by_name(*class_name)
                            .map(|kind| {
                                let tv = self.uf.fresh();
                                self.ty_arena
                                    .alloc(Ty::AssocType(tv, kind, *name))
                            })
                            .unwrap_or_else(|| {
                                let cls_s =
                                    self.env.resolve_string(*class_name);
                                self.errors
                                    .push(TypeError::UnknownClass(cls_s, span));
                                TyArena::ERROR
                            }),
                    }
                }
            },
        }
    }

    /// Convert a `TypeClass<AstTypeExprId>` to a `TypeClass<TyId>`,
    /// resolving type parameter references via `subst`.
    pub(super) fn ast_class_to_ty_class(
        &mut self,
        c: &TypeClass<AstTypeExprId>,
        subst: &IndexMap<StringId, TyId>,
    ) -> TypeClass<TyId> {
        c.map_ref(|id| self.ast_type_to_ty(*id, subst))
    }

    /// Collect type variable names from an AST type expression.
    ///
    /// Recursively walks the type expression tree, returning names that
    /// are type variables (i.e. not known/builtin types). Used to extract
    /// implicit type parameters from `for_type` in class instances.
    fn collect_type_vars_from_ast(
        &self,
        id: AstTypeExprId,
    ) -> SmallVec<[StringId; 4]> {
        let mut out = SmallVec::new();
        self.collect_type_vars_rec(id, &mut out);
        out
    }

    fn collect_type_vars_rec(
        &self,
        id: AstTypeExprId,
        out: &mut SmallVec<[StringId; 4]>,
    ) {
        if let Some(te) = self.ast.get_type_expr(id).cloned() {
            match te {
                AstTypeExpr::Named(name) => {
                    if !self.is_known_type_name(&name) {
                        out.push(name.local_name());
                    }
                }
                AstTypeExpr::VarApp(name, args) => {
                    out.push(name.local_name());
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
                AstTypeExpr::TupleConstructor { fixed, .. } => {
                    fixed.iter().for_each(|(_, te)| {
                        self.collect_type_vars_rec(*te, out);
                    });
                }
                AstTypeExpr::Wildcard | AstTypeExpr::AssocType { .. } => {}
            }
        }
    }

    /// Merge type variables from a `for_type` AST node into a substitution
    /// map.
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
            .for_each(|id| {
                if !subst.contains_key(&id) {
                    let tv = self.uf.fresh();
                    subst.insert(id, self.ty_arena.alloc(Ty::Var(tv)));
                }
            });
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

    /// Returns the expected type argument arity for builtin parameterized
    /// types.
    ///
    /// Returns `None` for user-defined types (arity checked elsewhere).
    fn expected_type_arity(name: &str) -> Option<usize> {
        match name {
            "Array" | "Option" | "Lazy" => Some(1),
            "Result" | "Map" => Some(2),
            _ => None,
        }
    }
}
