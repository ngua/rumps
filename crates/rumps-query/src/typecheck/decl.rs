use std::collections::HashMap;

use smallvec::SmallVec;

use crate::ast::{Ast, AstTypeExprId, Stmt, StmtId, TypeDefAst};
use crate::intern::{QualifiedName, StringId};
use crate::value::{TypeId, TypeRegistry};

#[derive(Clone, Debug, PartialEq, Eq)]
struct VariantTypeDecl {
    name: StringId,
    payloads: SmallVec<[AstTypeExprId; 2]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TypeDeclMeta {
    Sum {
        variants: SmallVec<[VariantTypeDecl; 4]>,
    },
    Alias {
        target: AstTypeExprId,
    },
    Union {
        member_exprs: SmallVec<[AstTypeExprId; 8]>,
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

    pub(super) fn alias_target(&self, id: TypeId) -> Option<AstTypeExprId> {
        self.decls.get(&id).and_then(|decl| match decl {
            TypeDeclMeta::Alias { target } => Some(*target),
            TypeDeclMeta::Sum { .. } | TypeDeclMeta::Union { .. } => None,
        })
    }

    pub(super) fn union_member_exprs(
        &self,
        id: TypeId,
    ) -> Option<&SmallVec<[AstTypeExprId; 8]>> {
        self.decls.get(&id).and_then(|decl| match decl {
            TypeDeclMeta::Union { member_exprs } => Some(member_exprs),
            TypeDeclMeta::Sum { .. } | TypeDeclMeta::Alias { .. } => None,
        })
    }

    pub(super) fn variant_payloads(
        &self,
        ty: TypeId,
        name: StringId,
    ) -> Option<&SmallVec<[AstTypeExprId; 2]>> {
        self.decls.get(&ty).and_then(|decl| match decl {
            TypeDeclMeta::Sum { variants } => variants
                .iter()
                .find(|v| v.name == name)
                .map(|v| &v.payloads),
            TypeDeclMeta::Alias { .. } | TypeDeclMeta::Union { .. } => None,
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
                    self.register(reg, &qn, TypeDeclMeta::Sum { variants });
                }
                Stmt::Union { name, members, .. } => {
                    let qn = Self::child_name(prefix, *name);
                    self.register(
                        reg,
                        &qn,
                        TypeDeclMeta::Union {
                            member_exprs: members.iter().copied().collect(),
                        },
                    );
                }
                Stmt::NewType { name, target, .. } => {
                    let qn = Self::child_name(prefix, *name);
                    self.register(
                        reg,
                        &qn,
                        TypeDeclMeta::Alias { target: *target },
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

    fn child_name(
        prefix: Option<&QualifiedName>,
        name: StringId,
    ) -> QualifiedName {
        prefix.map_or_else(|| QualifiedName::local(name), |p| p.child(name))
    }
}
