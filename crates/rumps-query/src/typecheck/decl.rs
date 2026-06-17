use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::SmallVec;

use crate::ast::{
    pragma, Ast, AstTypeExprId, Stmt, StmtId, TypeDefAst, Visibility,
};
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::TypeClass;
use crate::value::{TypeDef, TypeId, TypeRegistry};
use crate::{ClassId, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
struct VariantTypeDecl {
    name: StringId,
    payloads: SmallVec<[AstTypeExprId; 2]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TypeDeclPragmas {
    deriving: pragma::Deriving,
    transparent: Option<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewtypeClassMode {
    Deriving,
    Transparent,
    DerivingTransparent,
    Neither,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TypeDeclKind {
    Variant,
    Newtype,
    Union,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TypePragmaError {
    pub(super) msg: String,
    pub(super) span: Span,
}

pub(super) struct TypePragmaPolicy;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TypeDeclMeta {
    Sum {
        variants: SmallVec<[VariantTypeDecl; 4]>,
        pragmas: TypeDeclPragmas,
    },
    Alias {
        target: AstTypeExprId,
        repr_vis: Visibility,
        module: Option<QualifiedName>,
        pragmas: TypeDeclPragmas,
    },
    Union {
        member_exprs: SmallVec<[AstTypeExprId; 8]>,
        pragmas: TypeDeclPragmas,
    },
}

#[derive(Clone, Debug, Default)]
pub(super) struct TypeDeclRegistry {
    decls: HashMap<TypeId, TypeDeclMeta>,
}

impl TypeDeclRegistry {
    pub(super) fn from_ast(
        ast: &Ast,
        stmts: &[StmtId],
        reg: &TypeRegistry,
    ) -> Self {
        let mut decls = Self::default();
        decls.register_stmts(ast, stmts, None, reg);
        decls
    }

    pub(super) fn alias_target(&self, id: TypeId) -> AstTypeExprId {
        match self.alias_meta(id) {
            TypeDeclMeta::Alias { target, .. } => *target,
            TypeDeclMeta::Sum { .. } | TypeDeclMeta::Union { .. } => {
                typechecked!("alias target", "alias declaration")
            }
        }
    }

    pub(super) fn newtype_repr_expr(&self, id: TypeId) -> AstTypeExprId {
        self.alias_target(id)
    }

    pub(super) fn alias_repr_vis(&self, id: TypeId) -> Visibility {
        match self.alias_meta(id) {
            TypeDeclMeta::Alias { repr_vis, .. } => *repr_vis,
            TypeDeclMeta::Sum { .. } | TypeDeclMeta::Union { .. } => {
                typechecked!("alias repr visibility", "alias declaration")
            }
        }
    }

    pub(super) fn alias_module(&self, id: TypeId) -> Option<&QualifiedName> {
        match self.alias_meta(id) {
            TypeDeclMeta::Alias { module, .. } => module.as_ref(),
            TypeDeclMeta::Sum { .. } | TypeDeclMeta::Union { .. } => {
                typechecked!("alias module", "alias declaration")
            }
        }
    }

    pub(super) fn is_alias(&self, id: TypeId) -> bool {
        self.decls
            .get(&id)
            .is_some_and(|decl| matches!(decl, TypeDeclMeta::Alias { .. }))
    }

    pub(super) fn union_member_exprs(
        &self,
        id: TypeId,
    ) -> Option<&SmallVec<[AstTypeExprId; 8]>> {
        self.decls.get(&id).and_then(|decl| match decl {
            TypeDeclMeta::Union { member_exprs, .. } => Some(member_exprs),
            TypeDeclMeta::Sum { .. } | TypeDeclMeta::Alias { .. } => None,
        })
    }

    pub(super) fn variant_payloads(
        &self,
        ty: TypeId,
        name: StringId,
    ) -> Option<&SmallVec<[AstTypeExprId; 2]>> {
        self.decls.get(&ty).and_then(|decl| match decl {
            TypeDeclMeta::Sum { variants, .. } => variants
                .iter()
                .find(|v| v.name == name)
                .map(|v| &v.payloads),
            TypeDeclMeta::Alias { .. } | TypeDeclMeta::Union { .. } => None,
        })
    }

    pub(super) fn variant_payload_exprs(
        &self,
        id: TypeId,
    ) -> impl Iterator<Item = AstTypeExprId> + '_ {
        self.decls
            .get(&id)
            .into_iter()
            .filter_map(|decl| match decl {
                TypeDeclMeta::Sum { variants, .. } => Some(variants),
                TypeDeclMeta::Alias { .. } | TypeDeclMeta::Union { .. } => None,
            })
            .flat_map(|variants| variants.iter())
            .flat_map(|v| v.payloads.iter().copied())
    }

    pub(super) fn derives_simple(&self, id: TypeId, class: ClassId) -> bool {
        self.derived_instances(id)
            .iter()
            .any(|d| d.class == class && d.args.is_empty())
    }

    pub(super) fn derived_instances(
        &self,
        id: TypeId,
    ) -> &[pragma::DerivedClass<AstTypeExprId>] {
        self.decls
            .get(&id)
            .and_then(|decl| match decl {
                TypeDeclMeta::Sum { pragmas, .. }
                | TypeDeclMeta::Alias { pragmas, .. } => {
                    Some(pragmas.deriving.0.as_slice())
                }
                TypeDeclMeta::Union { .. } => None,
            })
            .unwrap_or(&[])
    }

    pub(super) fn raw_derived_instances(
        &self,
        id: TypeId,
    ) -> &[pragma::DerivedClass<AstTypeExprId>] {
        self.pragmas(id)
            .map(|ps| ps.deriving.0.as_slice())
            .unwrap_or(&[])
    }

    pub(super) fn is_transparent(&self, id: TypeId) -> bool {
        self.transparent_span(id).is_some()
    }

    pub(super) fn transparent_span(&self, id: TypeId) -> Option<Span> {
        self.pragmas(id).and_then(|ps| ps.transparent)
    }

    pub(super) fn newtype_class_mode(&self, id: TypeId) -> NewtypeClassMode {
        match self.decls.get(&id) {
            Some(TypeDeclMeta::Alias { pragmas, .. }) => {
                let deriving = !pragmas.deriving.0.is_empty();
                let transparent = pragmas.transparent.is_some();
                match (deriving, transparent) {
                    (true, true) => NewtypeClassMode::DerivingTransparent,
                    (true, false) => NewtypeClassMode::Deriving,
                    (false, true) => NewtypeClassMode::Transparent,
                    (false, false) => NewtypeClassMode::Neither,
                }
            }
            _ => typechecked!("newtype class mode", "alias declaration"),
        }
    }

    pub(super) fn derived_instance_matches<T: Copy + Eq>(
        &self,
        reg: &TypeRegistry,
        id: TypeId,
        class: &TypeClass<T>,
        args: &[T],
        mut conv: impl FnMut(AstTypeExprId, &IndexMap<StringId, T>) -> T,
    ) -> bool {
        let subst = self.type_param_subst(reg, id, args);
        self.derived_instances(id).iter().any(|d| {
            d.class == class.tag()
                && Self::derived_args_match(d, class, &subst, &mut conv)
        })
    }

    fn register_stmts(
        &mut self,
        ast: &Ast,
        stmts: &[StmtId],
        prefix: Option<&QualifiedName>,
        reg: &TypeRegistry,
    ) {
        stmts.iter().for_each(|id| {
            ast.get_stmt(*id).cloned().inspect(|stmt| match stmt {
                Stmt::Type {
                    name,
                    def: TypeDefAst::Sum(variants),
                    pragmas,
                    ..
                } => {
                    let qn = Self::child_name(prefix, *name);
                    let variants = variants
                        .iter()
                        .map(|v| VariantTypeDecl {
                            name: v.name,
                            payloads: v.payloads.clone(),
                        })
                        .collect();
                    self.register(
                        reg,
                        &qn,
                        TypeDeclMeta::Sum {
                            variants,
                            pragmas: TypeDeclPragmas::from(pragmas),
                        },
                    );
                }
                Stmt::Union {
                    name,
                    members,
                    pragmas,
                    ..
                } => {
                    let qn = Self::child_name(prefix, *name);
                    self.register(
                        reg,
                        &qn,
                        TypeDeclMeta::Union {
                            member_exprs: members.iter().copied().collect(),
                            pragmas: TypeDeclPragmas::from(pragmas),
                        },
                    );
                }
                Stmt::Newtype {
                    name,
                    target,
                    repr_vis,
                    pragmas,
                    ..
                } => {
                    let qn = Self::child_name(prefix, *name);
                    self.register(
                        reg,
                        &qn,
                        TypeDeclMeta::Alias {
                            target: *target,
                            repr_vis: *repr_vis,
                            module: qn.parent(),
                            pragmas: TypeDeclPragmas::from(pragmas),
                        },
                    );
                }
                Stmt::Module { name, body } => {
                    let qn = Self::child_name(prefix, *name);
                    self.register_stmts(ast, body, Some(&qn), reg);
                }
                _ => {}
            });
        });
    }

    fn register(
        &mut self,
        reg: &TypeRegistry,
        qn: &QualifiedName,
        meta: TypeDeclMeta,
    ) {
        if let Some(id) = reg.lookup(qn) {
            self.decls.insert(id, meta);
        }
    }

    fn alias_meta(&self, id: TypeId) -> &TypeDeclMeta {
        self.decls
            .get(&id)
            .unwrap_or_else(|| typechecked!("alias declaration", "registered"))
    }

    fn pragmas(&self, id: TypeId) -> Option<&TypeDeclPragmas> {
        self.decls.get(&id).map(|decl| match decl {
            TypeDeclMeta::Sum { pragmas, .. }
            | TypeDeclMeta::Alias { pragmas, .. }
            | TypeDeclMeta::Union { pragmas, .. } => pragmas,
        })
    }

    fn type_param_subst<T: Copy>(
        &self,
        reg: &TypeRegistry,
        id: TypeId,
        args: &[T],
    ) -> IndexMap<StringId, T> {
        reg.get_def(id)
            .map(|def| match def {
                TypeDef::Sum { type_params, .. }
                | TypeDef::Alias { type_params, .. }
                | TypeDef::Union { type_params, .. } => type_params,
                TypeDef::Builtin(_) => {
                    typechecked!("type parameters", "user declaration")
                }
            })
            .into_iter()
            .flat_map(|ps| ps.iter().copied().zip(args.iter().copied()))
            .collect()
    }

    fn derived_args_match<T: Copy + Eq>(
        d: &pragma::DerivedClass<AstTypeExprId>,
        class: &TypeClass<T>,
        subst: &IndexMap<StringId, T>,
        conv: &mut impl FnMut(AstTypeExprId, &IndexMap<StringId, T>) -> T,
    ) -> bool {
        let params = match class {
            TypeClass::Concrete { params, .. }
            | TypeClass::Hkt { params, .. } => params,
        };
        d.args.len() == params.len()
            && d.args
                .iter()
                .zip(params.iter())
                .all(|(&arg, param)| conv(arg, subst) == *param)
    }

    fn child_name(
        prefix: Option<&QualifiedName>,
        name: StringId,
    ) -> QualifiedName {
        prefix.map_or_else(|| QualifiedName::local(name), |p| p.child(name))
    }
}

impl TypePragmaPolicy {
    pub(super) fn check(
        kind: TypeDeclKind,
        ps: &pragma::Type,
    ) -> SmallVec<[TypePragmaError; 2]> {
        let mut errs = SmallVec::new();
        ps.deriving
            .0
            .iter()
            .filter_map(|d| Self::deriving_err(kind, d))
            .for_each(|err| errs.push(err));
        Self::transparent_err(kind, ps)
            .into_iter()
            .for_each(|err| errs.push(err));
        errs
    }

    fn deriving_err(
        kind: TypeDeclKind,
        d: &pragma::DerivedClass<AstTypeExprId>,
    ) -> Option<TypePragmaError> {
        if d.class.idx() >= ClassId::BUILTIN_COUNT {
            Some(TypePragmaError {
                msg: "cannot derive user defined classes".to_string(),
                span: d.span,
            })
        } else {
            match kind {
                TypeDeclKind::Variant => {
                    Self::variant_deriving_err(d.class, d.span)
                }
                TypeDeclKind::Newtype => {
                    Self::newtype_deriving_err(d.class, d.args.len(), d.span)
                }
                TypeDeclKind::Union => Some(TypePragmaError {
                    msg: "`deriving` pragmas are not supported on `union` declarations"
                        .to_string(),
                    span: d.span,
                }),
            }
        }
    }

    fn variant_deriving_err(
        class: ClassId,
        span: Span,
    ) -> Option<TypePragmaError> {
        match class {
            ClassId::EQ | ClassId::ORD | ClassId::DISPLAY => None,
            _ => Some(TypePragmaError {
                msg: format!(
                    "`variant` declarations can only derive `Eq`, `Ord`, or \
                     `Display`, not `{}`",
                    class.name()
                ),
                span,
            }),
        }
    }

    fn newtype_deriving_err(
        class: ClassId,
        argc: usize,
        span: Span,
    ) -> Option<TypePragmaError> {
        match class {
            ClassId::INTO | ClassId::TRY_INTO if argc != 1 => {
                Some(TypePragmaError {
                    msg: format!(
                        "`{}` deriving requires exactly `1` type argument",
                        class.name()
                    ),
                    span,
                })
            }
            _ => None,
        }
    }

    fn transparent_err(
        kind: TypeDeclKind,
        ps: &pragma::Type,
    ) -> Option<TypePragmaError> {
        ps.transparent.and_then(|span| match kind {
            TypeDeclKind::Newtype if ps.deriving.0.is_empty() => None,
            TypeDeclKind::Newtype => Some(TypePragmaError {
                msg: "`newtype` declarations cannot use both `transparent` and `deriving` pragmas"
                    .to_string(),
                span,
            }),
            TypeDeclKind::Variant | TypeDeclKind::Union => {
                Some(TypePragmaError {
                    msg: "`transparent` pragmas are only supported on `newtype` declarations"
                        .to_string(),
                    span,
                })
            }
        })
    }
}

impl From<&pragma::Type> for TypeDeclPragmas {
    fn from(ps: &pragma::Type) -> Self {
        Self {
            deriving: ps.deriving.clone(),
            transparent: ps.transparent,
        }
    }
}
