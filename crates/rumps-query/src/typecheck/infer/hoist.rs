//! Declaration hoisting for forward references.
//!
//! Implements Pass `1` of the two-pass type inference: traverse statements and
//! register function/module names with provisional types before any body
//! inference. This enables forward references and mutual recursion.

use std::collections::{HashMap, HashSet};

use super::{ClassInstanceInput, InferCtx};
use crate::ast::{
    ArrayElem, AstTypeExprId, BindingPattern, DbRef, Expr, ExprId, Import,
    ImportItem, JsonAccessKey, MatchArm, MatchPattern, MatchPatternId,
    ObjectEntry, OutputTarget, RefTarget, RestPattern, Stmt, StmtId,
    SubscriptElem, TransactionExpr, TypePattern, Visibility, WriteExpr,
};
use crate::env::PRELUDE_MODULE;
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, TyArena};
use crate::Span;

#[derive(Clone)]
struct LetInfo {
    stmt: StmtId,
    name: StringId,
    ann: Option<AstTypeExprId>,
    rhs: ExprId,
    vis: Visibility,
    span: Span,
}

#[derive(Clone)]
struct ModuleInfo {
    stmt: StmtId,
    path: QualifiedName,
    body: Vec<StmtId>,
    span: Span,
}

#[derive(Clone)]
struct StaticScope {
    module: Option<QualifiedName>,
    stmts: Vec<StmtId>,
    lets: Vec<LetInfo>,
    mods: Vec<ModuleInfo>,
    local_names: HashMap<StringId, StmtId>,
    providers: ProviderIndex,
}

#[derive(Clone, Default)]
struct ProviderIndex {
    by_mod: HashMap<QualifiedName, ModuleLetProvider>,
}

#[derive(Clone)]
struct ModuleLetProvider {
    root: StmtId,
    lets: HashMap<StringId, Visibility>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum StaticRef {
    Name(StringId),
    Path(QualifiedName, StringId),
}

#[derive(Clone, Copy)]
enum StaticRefMode<'a> {
    Local,
    Module(&'a ProviderIndex),
}

#[derive(Clone)]
enum StaticNode {
    Let(LetInfo),
    Mod(ModuleInfo),
}

struct StaticGraph {
    nodes: HashMap<StmtId, StaticNode>,
    deps: HashMap<StmtId, HashSet<StmtId>>,
    roots: Vec<StmtId>,
    pos: HashMap<StmtId, usize>,
}

#[derive(Clone, Copy)]
enum StaticGraphKind {
    Let,
    Mod,
    Static,
}

impl StaticGraphKind {
    fn has_lets(self) -> bool {
        matches!(self, Self::Let | Self::Static)
    }

    fn has_mods(self) -> bool {
        matches!(self, Self::Mod | Self::Static)
    }

    fn is_static(self) -> bool {
        matches!(self, Self::Static)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LetMark {
    Visiting,
    Done,
}

mod decls;

impl InferCtx<'_> {
    /// Pass `1`: Register all function/module declarations with provisional types.
    ///
    /// This enables forward references: functions can call other functions
    /// defined later in the same scope, and modules can be referenced before
    /// their definition.
    ///
    /// Hoisting first registers modules and imports types, then hoists
    /// functions and class instances, then infers final top-level and module
    /// `let` schemes.
    pub(crate) fn hoist_declarations(&mut self, stmts: &[StmtId]) {
        let static_scope = self.env.scope_depth() == 1;

        // Phase `0`: Register user-defined class stubs through all static scopes
        // so class names are available for constraint resolution and method lookup.
        self.register_class_stubs(stmts, None);

        // Phase `1`: Register module names before module bodies process imports.
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::Module { ref name, ref body }) = stmt {
                self.register_module_tree(QualifiedName::local(*name), body);
            }
        });

        // Phase `1`: Hoist modules only (registers module types)
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::Module { ref name, ref body }) = stmt {
                let span = self.ast.stmt_span(id).unwrap_or_default();
                self.hoist_module(
                    QualifiedName::local(*name),
                    body,
                    stmts,
                    span,
                );
            }
        });

        // Phase `2`: Process imports (populates imported_types).
        // Auto-import the `Prelude` module first so its members are always
        // in scope, then process user imports (which may shadow them).
        {
            let pid = self.env.intern(PRELUDE_MODULE);
            self.import(&Import::wildcard(pid), Span::default());
        }
        let prev_defers = self.defer_missing_import_members;
        self.defer_missing_import_members = prev_defers || static_scope;
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::Import(ref import)) = stmt {
                let span = self.ast.stmt_span(id).unwrap_or_default();
                self.import(import, span);
            }
        });
        self.defer_missing_import_members = prev_defers;

        // Phase `3`: Hoist functions and classes before static `let`s.
        stmts.iter().for_each(|&id| self.hoist_non_module(id));
        self.hoist_class_defs(stmts, None);
        self.hoist_class_instances(stmts, None);

        // Phase `4`: infer static `let`s after declarations are available, but
        // before function bodies can reference final schemes.
        if static_scope {
            if self.interactive {
                let scope = self.collect_static_scope(stmts, None);
                self.infer_static_scope(scope, stmts, StaticGraphKind::Mod);
                self.replay_deferred_imports_for(None);
            } else {
                let scope = self.collect_static_scope(stmts, None);
                self.infer_static_scope(scope, stmts, StaticGraphKind::Static);
            }
        }
    }

    /// Infer top-level simple `let` bindings before function bodies.
    ///
    /// The dependency graph is built from free RHS references to sibling
    /// top-level `let`s. Acyclic bindings are inferred in dependency order;
    /// cyclic ordinary `let` groups are rejected.
    pub(crate) fn infer_toplevel_lets(&mut self, stmts: &[StmtId]) {
        let scope = self.collect_static_scope(stmts, None);
        self.infer_static_scope(scope, stmts, StaticGraphKind::Let);
    }

    fn infer_module_lets(
        &mut self,
        mod_path: QualifiedName,
        body: &[StmtId],
        root: &[StmtId],
    ) {
        let prev_module = self.current_module.replace(mod_path.clone());

        let scope = self.collect_static_scope(body, Some(&mod_path));
        self.infer_static_scope(scope, root, StaticGraphKind::Static);

        self.current_module = prev_module;
    }

    fn infer_static_scope(
        &mut self,
        scope: StaticScope,
        root: &[StmtId],
        kind: StaticGraphKind,
    ) {
        let graph = self.build_static_graph(&scope, kind);
        let order = self.schedule_static_graph(&graph);

        order
            .into_iter()
            .for_each(|id| match graph.nodes.get(&id).cloned() {
                Some(StaticNode::Let(i)) => {
                    self.infer_simple_let(i, scope.module.as_ref());
                }
                Some(StaticNode::Mod(m)) => {
                    self.infer_module_lets(m.path, &m.body, root);
                    if let StaticGraphKind::Static = kind {
                        let paths =
                            self.provider_paths_for(&scope.providers, id);
                        self.replay_deferred_imports_for_paths(
                            scope.module.as_ref(),
                            &paths,
                        );
                    }
                }
                None => {}
            });
        if let StaticGraphKind::Static = kind {
            self.replay_deferred_imports_for(scope.module.as_ref());
            if scope.module.is_some() {
                scope.lets.iter().for_each(|info| {
                    self.env.lookup(info.name).cloned().into_iter().for_each(
                        |scheme| {
                            self.env.bind(info.name, scheme);
                        },
                    );
                });
            }
        }
    }

    fn collect_static_scope(
        &mut self,
        stmts: &[StmtId],
        module: Option<&QualifiedName>,
    ) -> StaticScope {
        let lets: Vec<_> = stmts
            .iter()
            .filter_map(|&id| self.simple_let_info(id))
            .collect();

        let local_names = lets.iter().map(|i| (i.name, i.stmt)).collect();
        let mods: Vec<_> = stmts
            .iter()
            .filter_map(|&id| self.module_info(id, module))
            .collect();
        let providers = self.provider_index(&mods);

        StaticScope {
            module: module.cloned(),
            stmts: stmts.to_vec(),
            lets,
            mods,
            local_names,
            providers,
        }
    }

    fn simple_let_info(&self, id: StmtId) -> Option<LetInfo> {
        let span = self.ast.stmt_span(id).unwrap_or_default();
        match self.ast.get_stmt(id).cloned() {
            Some(Stmt::Let(BindingPattern::Var(name), ann, rhs, vis)) => {
                Some(LetInfo {
                    stmt: id,
                    name,
                    ann,
                    rhs,
                    vis,
                    span,
                })
            }
            _ => None,
        }
    }

    fn module_info(
        &self,
        id: StmtId,
        parent: Option<&QualifiedName>,
    ) -> Option<ModuleInfo> {
        let span = self.ast.stmt_span(id).unwrap_or_default();
        match self.ast.get_stmt(id) {
            Some(Stmt::Module { name, body }) => {
                let path = parent.map_or_else(
                    || QualifiedName::local(*name),
                    |p| p.child(*name),
                );
                Some(ModuleInfo {
                    stmt: id,
                    path,
                    body: body.clone(),
                    span,
                })
            }
            _ => None,
        }
    }

    fn register_module_tree(&mut self, path: QualifiedName, body: &[StmtId]) {
        self.env.register_user_module(path.clone());
        body.iter().for_each(|&id| {
            if let Some(Stmt::Module { name, body }) =
                self.ast.get_stmt(id).cloned()
            {
                self.register_module_tree(path.child(name), &body);
            }
        });
    }

    fn provider_index(&self, mods: &[ModuleInfo]) -> ProviderIndex {
        let mut out = ProviderIndex::default();
        mods.iter().for_each(|m| {
            self.add_provider(m, m.stmt, &mut out);
        });
        out
    }

    fn add_provider(
        &self,
        m: &ModuleInfo,
        root: StmtId,
        out: &mut ProviderIndex,
    ) {
        let lets = m
            .body
            .iter()
            .filter_map(|&id| match self.ast.get_stmt(id) {
                Some(Stmt::Let(BindingPattern::Var(name), _, _, vis)) => {
                    Some((*name, *vis))
                }
                _ => None,
            })
            .collect();
        out.by_mod
            .insert(m.path.clone(), ModuleLetProvider { root, lets });

        m.body
            .iter()
            .filter_map(|&id| self.module_info(id, Some(&m.path)))
            .for_each(|child| {
                self.add_provider(&child, root, out);
            });
    }

    fn collect_module_deps(
        &self,
        m: &ModuleInfo,
        providers: &ProviderIndex,
        names: &HashMap<StringId, StmtId>,
        acc: &mut HashSet<StmtId>,
    ) {
        let mut refs = HashSet::new();
        self.collect_module_body_refs(&m.body, providers, &mut refs);
        refs.iter().for_each(|r| {
            self.add_ref_dep(r, names, None, Some(providers), acc);
        });
        acc.remove(&m.stmt);
    }

    fn build_static_graph(
        &self,
        scope: &StaticScope,
        kind: StaticGraphKind,
    ) -> StaticGraph {
        let has_lets = kind.has_lets();
        let has_mods = kind.has_mods();
        let let_nodes = scope
            .lets
            .iter()
            .filter(|_| has_lets)
            .map(|i| (i.stmt, StaticNode::Let(i.clone())));
        let mod_nodes = scope
            .mods
            .iter()
            .filter(|_| has_mods)
            .map(|m| (m.stmt, StaticNode::Mod(m.clone())));
        let nodes: HashMap<StmtId, StaticNode> =
            let_nodes.chain(mod_nodes).collect();
        let roots: Vec<StmtId> = scope
            .lets
            .iter()
            .filter(|_| has_lets)
            .map(|i| i.stmt)
            .chain(scope.mods.iter().filter(|_| has_mods).map(|m| m.stmt))
            .collect();
        let pos = roots
            .iter()
            .copied()
            .enumerate()
            .map(|(idx, id)| (id, idx))
            .collect();
        let final_names = self.final_let_names(scope);
        let import_names = if kind.is_static() {
            self.import_dep_names(scope)
        } else {
            HashMap::new()
        };
        let mod_names = if kind.is_static() {
            scope.local_names.clone()
        } else {
            HashMap::new()
        };
        let let_deps = scope.lets.iter().filter(|_| has_lets).map(|i| {
            let mut acc = HashSet::new();
            let mut names = self.visible_let_names(scope, i.stmt);
            import_names.iter().for_each(|(name, sid)| {
                names.entry(*name).or_insert(*sid);
            });
            final_names.iter().for_each(|(name, sid)| {
                if *sid != i.stmt {
                    names.entry(*name).or_insert(*sid);
                }
            });
            let providers = if kind.is_static() {
                Some(&scope.providers)
            } else {
                None
            };
            self.collect_expr_deps(
                i.rhs,
                &names,
                scope.module.as_ref(),
                providers,
                &HashSet::new(),
                &mut acc,
            );
            (i.stmt, acc)
        });
        let mod_deps = scope.mods.iter().filter(|_| has_mods).map(|m| {
            let mut acc = HashSet::new();
            self.collect_module_deps(m, &scope.providers, &mod_names, &mut acc);
            acc.remove(&m.stmt);
            (m.stmt, acc)
        });
        let mut deps: HashMap<StmtId, HashSet<StmtId>> =
            let_deps.chain(mod_deps).collect();

        deps.values_mut().for_each(|ids| {
            ids.retain(|id| nodes.contains_key(id));
        });

        StaticGraph {
            nodes,
            deps,
            roots,
            pos,
        }
    }

    fn visible_let_names(
        &self,
        scope: &StaticScope,
        stmt: StmtId,
    ) -> HashMap<StringId, StmtId> {
        scope
            .lets
            .iter()
            .take_while(|i| i.stmt != stmt)
            .map(|i| (i.name, i.stmt))
            .collect()
    }

    fn final_let_names(
        &self,
        scope: &StaticScope,
    ) -> HashMap<StringId, StmtId> {
        scope.lets.iter().map(|i| (i.name, i.stmt)).collect()
    }

    fn provider_paths_for(
        &self,
        providers: &ProviderIndex,
        root: StmtId,
    ) -> HashSet<QualifiedName> {
        providers
            .by_mod
            .iter()
            .filter_map(|(path, p)| {
                if p.root == root {
                    Some(path.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn schedule_static_graph(&mut self, graph: &StaticGraph) -> Vec<StmtId> {
        let mut marks = HashMap::new();
        let mut stack = Vec::new();
        let mut order = Vec::new();

        graph.roots.iter().copied().for_each(|id| {
            self.schedule_static_node(
                id, graph, &mut marks, &mut stack, &mut order,
            );
        });

        order
    }

    fn schedule_static_node(
        &mut self,
        id: StmtId,
        graph: &StaticGraph,
        marks: &mut HashMap<StmtId, LetMark>,
        stack: &mut Vec<StmtId>,
        order: &mut Vec<StmtId>,
    ) {
        match marks.get(&id).copied() {
            Some(LetMark::Done) => {}
            Some(LetMark::Visiting) => {
                self.reject_static_cycle(id, graph, marks, stack);
            }
            None => {
                marks.insert(id, LetMark::Visiting);
                stack.push(id);
                graph.deps.get(&id).into_iter().for_each(|ids| {
                    let mut deps: Vec<StmtId> = ids.iter().copied().collect();
                    deps.sort_by_key(|dep| {
                        graph.pos.get(dep).copied().unwrap_or(usize::MAX)
                    });
                    deps.into_iter().for_each(|dep| {
                        self.schedule_static_node(
                            dep, graph, marks, stack, order,
                        );
                    });
                });
                stack.pop();
                if marks.get(&id).copied() == Some(LetMark::Visiting) {
                    marks.insert(id, LetMark::Done);
                    order.push(id);
                }
            }
        }
    }

    fn reject_static_cycle(
        &mut self,
        id: StmtId,
        graph: &StaticGraph,
        marks: &mut HashMap<StmtId, LetMark>,
        stack: &[StmtId],
    ) {
        let cyc: Vec<StmtId> =
            stack.iter().copied().skip_while(|&sid| sid != id).collect();
        let msg = self.static_cycle_msg(&cyc, graph);
        let span = graph
            .nodes
            .get(&id)
            .map(|node| match node {
                StaticNode::Let(i) => i.span,
                StaticNode::Mod(m) => m.span,
            })
            .unwrap_or_default();

        self.error(TypeError::Custom { msg, span });
        cyc.into_iter().for_each(|sid| {
            marks.insert(sid, LetMark::Done);
            if let Some(StaticNode::Let(i)) = graph.nodes.get(&sid) {
                let scheme = Scheme::mono(TyArena::ERROR);
                self.hoist
                    .final_let_schemes
                    .insert(sid, (i.name, scheme.clone(), None));
                self.env.bind(i.name, scheme);
            }
        });
    }

    fn static_cycle_msg(&self, ids: &[StmtId], graph: &StaticGraph) -> String {
        let names: Vec<String> = ids
            .iter()
            .filter_map(|id| graph.nodes.get(id))
            .map(|node| match node {
                StaticNode::Let(i) => {
                    format!("`{}`", self.env.resolve_string(i.name))
                }
                StaticNode::Mod(m) => {
                    format!("`{}`", m.path.display(&self.env.strings))
                }
            })
            .collect();
        let has_mod = ids
            .iter()
            .any(|id| matches!(graph.nodes.get(id), Some(StaticNode::Mod(_))));

        if names.is_empty() {
            if has_mod {
                "cyclic ordinary `let` dependency".to_owned()
            } else {
                "cyclic ordinary `let` binding".to_owned()
            }
        } else if has_mod {
            format!("cyclic ordinary `let` dependencies: {}", names.join(", "))
        } else {
            format!("cyclic ordinary `let` bindings: {}", names.join(", "))
        }
    }

    fn import_dep_names(
        &self,
        scope: &StaticScope,
    ) -> HashMap<StringId, StmtId> {
        let mut out = HashMap::new();
        scope.stmts.iter().for_each(|&id| {
            if let Some(Stmt::Import(import)) = self.ast.get_stmt(id) {
                let qn = QualifiedName::new(import.path.to_vec());
                scope.providers.by_mod.get(&qn).into_iter().for_each(|p| {
                    Self::provider_import_lets(import, p, |_, bind| {
                        out.entry(bind).or_insert(p.root);
                    });
                });
            }
        });
        out
    }

    fn infer_simple_let(
        &mut self,
        info: LetInfo,
        module: Option<&QualifiedName>,
    ) {
        self.r#let(
            info.stmt,
            &BindingPattern::Var(info.name),
            info.ann.as_ref(),
            info.rhs,
            info.span,
        );

        if let Some(scheme) = self.env.lookup(info.name).cloned() {
            let origin = self.env.lookup_method_ref_origin(info.name);
            self.hoist
                .final_let_schemes
                .insert(info.stmt, (info.name, scheme.clone(), origin));

            module.into_iter().for_each(|mod_path| {
                self.env.register_user_module_member(
                    mod_path.clone(),
                    info.name,
                    scheme.clone(),
                    info.vis,
                );
                origin.into_iter().for_each(|origin| {
                    self.env.set_user_module_member_method_origin(
                        mod_path, info.name, origin,
                    );
                });
            });
        }
    }

    pub(super) fn final_let_done(&self, id: StmtId) -> bool {
        self.hoist.final_let_schemes.contains_key(&id)
    }

    pub(super) fn restore_final_let(&mut self, id: StmtId) {
        self.hoist
            .final_let_schemes
            .get(&id)
            .cloned()
            .into_iter()
            .for_each(|(name, scheme, origin)| {
                self.env.bind(name, scheme);
                origin.into_iter().for_each(|origin| {
                    self.env.bind_method_ref_origin(name, origin);
                });
            });
    }

    pub(super) fn install_final_let_schemes(&mut self, stmts: &[StmtId]) {
        let ids: Vec<_> = stmts
            .iter()
            .copied()
            .filter(|id| self.hoist.final_let_schemes.contains_key(id))
            .collect();

        ids.into_iter().for_each(|id| self.restore_final_let(id));
    }

    fn collect_expr_deps(
        &self,
        id: ExprId,
        names: &HashMap<StringId, StmtId>,
        module: Option<&QualifiedName>,
        providers: Option<&ProviderIndex>,
        bound: &HashSet<StringId>,
        acc: &mut HashSet<StmtId>,
    ) {
        let mut refs = HashSet::new();
        self.collect_expr_refs(id, bound, &mut refs);
        refs.iter().for_each(|r| {
            self.add_ref_dep(r, names, module, providers, acc);
        });
    }

    fn add_ref_dep(
        &self,
        r: &StaticRef,
        names: &HashMap<StringId, StmtId>,
        module: Option<&QualifiedName>,
        providers: Option<&ProviderIndex>,
        acc: &mut HashSet<StmtId>,
    ) {
        match r {
            StaticRef::Name(name) => {
                names.get(name).into_iter().for_each(|sid| {
                    acc.insert(*sid);
                });
            }
            StaticRef::Path(path, member) => {
                module
                    .filter(|mod_path| *mod_path == path)
                    .and_then(|_| names.get(member))
                    .into_iter()
                    .for_each(|sid| {
                        acc.insert(*sid);
                    });
                providers
                    .and_then(|p| p.by_mod.get(path))
                    .filter(|p| Self::public_provider_let(p, *member))
                    .into_iter()
                    .for_each(|p| {
                        acc.insert(p.root);
                    });
            }
        }
    }

    fn public_provider_let(p: &ModuleLetProvider, name: StringId) -> bool {
        p.lets.get(&name) == Some(&Visibility::Public)
    }

    fn provider_import_lets<F>(import: &Import, p: &ModuleLetProvider, mut f: F)
    where
        F: FnMut(StringId, StringId),
    {
        let excluded: HashSet<StringId> = import
            .items
            .iter()
            .filter_map(|item| match item {
                ImportItem::Exclude(name) => Some(*name),
                _ => None,
            })
            .collect();
        import.items.iter().for_each(|item| match item {
            ImportItem::Named { name, alias }
                if Self::public_provider_let(p, *name) =>
            {
                f(*name, alias.unwrap_or(*name));
            }
            ImportItem::Wildcard => {
                p.lets
                    .iter()
                    .filter(|(name, vis)| {
                        **vis == Visibility::Public && !excluded.contains(name)
                    })
                    .for_each(|(name, _)| {
                        f(*name, *name);
                    });
            }
            ImportItem::Named { .. } | ImportItem::Exclude(_) => {}
        });
    }

    fn collect_module_body_refs(
        &self,
        body: &[StmtId],
        providers: &ProviderIndex,
        acc: &mut HashSet<StaticRef>,
    ) {
        let mut local = HashSet::new();
        self.static_decl_names(body, &mut local);
        body.iter().for_each(|&id| {
            self.collect_stmt_refs(
                id,
                &mut local,
                StaticRefMode::Module(providers),
                acc,
            );
        });
    }

    fn collect_expr_refs(
        &self,
        id: ExprId,
        bound: &HashSet<StringId>,
        acc: &mut HashSet<StaticRef>,
    ) {
        if let Some(expr) = self.ast.get_expr(id) {
            match expr {
                Expr::Var(name) => {
                    if !bound.contains(name) {
                        acc.insert(StaticRef::Name(*name));
                    }
                }

                Expr::Interpolation(parts) | Expr::Tuple(parts) => {
                    parts.iter().for_each(|part| {
                        self.collect_expr_refs(*part, bound, acc);
                    });
                }

                Expr::Intrinsic(_, target, val, _) => {
                    self.collect_ref_target_refs(target, bound, acc);
                    val.iter().for_each(|v| {
                        self.collect_expr_refs(*v, bound, acc);
                    });
                }

                Expr::Binary(l, _, r)
                | Expr::Index(l, r)
                | Expr::OptionalIndex(l, r)
                | Expr::Range(l, r, _)
                | Expr::Matches(l, r)
                | Expr::Catch(l, r) => {
                    self.collect_expr_refs(*l, bound, acc);
                    self.collect_expr_refs(*r, bound, acc);
                }

                Expr::Unary(_, inner)
                | Expr::TupleIndex(inner, _)
                | Expr::Field(inner, _)
                | Expr::OptionalField(inner, _)
                | Expr::Is(inner, _)
                | Expr::As(inner, _)
                | Expr::Read(inner, _)
                | Expr::Postfix(_, inner)
                | Expr::Annotate(inner, _)
                | Expr::Raise(inner) => {
                    self.collect_expr_refs(*inner, bound, acc);
                }

                Expr::Call(callee, args) => {
                    self.collect_expr_refs(*callee, bound, acc);
                    args.iter().for_each(|arg| {
                        self.collect_expr_refs(*arg, bound, acc);
                    });
                }

                Expr::Object(entries) => {
                    entries.iter().for_each(|entry| match entry {
                        ObjectEntry::Field(_, expr)
                        | ObjectEntry::Spread(expr) => {
                            self.collect_expr_refs(*expr, bound, acc);
                        }
                    });
                }

                Expr::Array(elems) => {
                    elems.iter().for_each(|elem| match elem {
                        ArrayElem::Elem(expr) | ArrayElem::Spread(expr) => {
                            self.collect_expr_refs(*expr, bound, acc);
                        }
                    });
                }

                Expr::MapLit(entries) => {
                    entries.iter().for_each(|(k, v)| {
                        self.collect_expr_refs(*k, bound, acc);
                        self.collect_expr_refs(*v, bound, acc);
                    });
                }

                Expr::Variant(_, _, args)
                | Expr::NakedVariant(_, args)
                | Expr::ClassMethod(_, _, args)
                | Expr::NakedClassMethod(_, args) => {
                    args.iter().for_each(|arg| {
                        self.collect_expr_refs(*arg, bound, acc);
                    });
                }

                Expr::Path(segs) => {
                    segs.split_last().into_iter().for_each(|(member, path)| {
                        if !path.is_empty() {
                            acc.insert(StaticRef::Path(
                                QualifiedName::new(path.to_vec()),
                                *member,
                            ));
                        }
                    });
                }

                Expr::Block(stmts, tail) => {
                    let mut local = bound.clone();
                    self.local_decl_names(stmts, &mut local);
                    stmts.iter().for_each(|stmt| {
                        self.collect_stmt_refs(
                            *stmt,
                            &mut local,
                            StaticRefMode::Local,
                            acc,
                        );
                    });
                    tail.iter().for_each(|expr| {
                        self.collect_expr_refs(*expr, &local, acc);
                    });
                }

                Expr::If(cond, then, els) => {
                    self.collect_expr_refs(*cond, bound, acc);
                    let mut local = bound.clone();
                    self.if_cond_binding_names(*cond, &mut local);
                    self.collect_expr_refs(*then, &local, acc);
                    els.iter().for_each(|expr| {
                        self.collect_expr_refs(*expr, bound, acc);
                    });
                }

                Expr::Match(scrutinee, arms) => {
                    self.collect_expr_refs(*scrutinee, bound, acc);
                    arms.iter().for_each(|arm| {
                        self.collect_match_arm_refs(arm, bound, acc);
                    });
                }

                Expr::Closure { params, body, .. } => {
                    let mut local = bound.clone();
                    params.iter().for_each(|(name, _)| {
                        local.insert(*name);
                    });
                    self.collect_expr_refs(*body, &local, acc);
                }

                Expr::Json(entries) => {
                    entries.iter().for_each(|(_, expr)| {
                        self.collect_expr_refs(*expr, bound, acc);
                    });
                }

                Expr::Loop {
                    seed,
                    state_param,
                    cont_param,
                    body,
                } => {
                    self.collect_expr_refs(*seed, bound, acc);
                    let mut local = bound.clone();
                    local.insert(state_param.0);
                    local.insert(cont_param.0);
                    self.collect_expr_refs(*body, &local, acc);
                }

                Expr::Transaction(txn) => {
                    self.collect_txn_refs(txn, bound, acc);
                }

                Expr::Write(w) => {
                    self.collect_write_refs(w, bound, acc);
                }

                Expr::Ref(r) => {
                    self.collect_db_ref_refs(r, bound, acc);
                }

                Expr::JsonAccess(inner, _, key) => {
                    self.collect_expr_refs(*inner, bound, acc);
                    if let JsonAccessKey::Expr(expr) = key {
                        self.collect_expr_refs(*expr, bound, acc);
                    }
                }

                Expr::Literal(_)
                | Expr::ClassMethodRef(_, _, _)
                | Expr::NakedClassMethodRef(_)
                | Expr::Regex(_, _)
                | Expr::Mempty => {}
            }
        }
    }

    fn collect_stmt_refs(
        &self,
        id: StmtId,
        bound: &mut HashSet<StringId>,
        mode: StaticRefMode<'_>,
        acc: &mut HashSet<StaticRef>,
    ) {
        if let Some(stmt) = self.ast.get_stmt(id) {
            match stmt {
                Stmt::Let(pat, _, rhs, _) => match mode {
                    StaticRefMode::Local => {
                        self.collect_expr_refs(*rhs, bound, acc);
                        Self::binding_names(pat, bound);
                    }
                    StaticRefMode::Module(_) => {
                        if let BindingPattern::Var(_) = pat {
                            self.collect_expr_refs(*rhs, bound, acc);
                        }
                    }
                },

                Stmt::Expr(expr) => {
                    if let StaticRefMode::Local = mode {
                        self.collect_expr_refs(*expr, bound, acc);
                    }
                }

                Stmt::Fun {
                    name, params, body, ..
                } => {
                    let mut local = bound.clone();
                    local.insert(*name);
                    params.iter().for_each(|(param, _)| {
                        local.insert(*param);
                    });
                    if let StaticRefMode::Local = mode {
                        self.collect_expr_refs(*body, &local, acc);
                        bound.insert(*name);
                    }
                }

                Stmt::ClassInstance { methods, .. } => {
                    if let StaticRefMode::Local = mode {
                        methods.iter().for_each(|m| {
                            let mut local = bound.clone();
                            m.params.iter().for_each(|(param, _)| {
                                local.insert(*param);
                            });
                            self.collect_expr_refs(m.body, &local, acc);
                        });
                    }
                }

                Stmt::Module { name, body } => match mode {
                    StaticRefMode::Local => {
                        bound.insert(*name);
                    }
                    StaticRefMode::Module(providers) => {
                        self.collect_module_body_refs(body, providers, acc);
                    }
                },

                Stmt::Import(import) => {
                    if let StaticRefMode::Module(providers) = mode {
                        self.collect_import_refs(import, providers, acc);
                    }
                }

                Stmt::Type { .. }
                | Stmt::Union { .. }
                | Stmt::Newtype { .. }
                | Stmt::ClassDef { .. } => {}
            }
        }
    }

    fn collect_match_arm_refs(
        &self,
        arm: &MatchArm,
        bound: &HashSet<StringId>,
        acc: &mut HashSet<StaticRef>,
    ) {
        let mut local = bound.clone();
        self.match_pattern_names(arm.pattern, &mut local);
        arm.guard.iter().for_each(|guard| {
            self.collect_expr_refs(*guard, &local, acc);
        });
        self.collect_expr_refs(arm.body, &local, acc);
    }

    fn collect_txn_refs(
        &self,
        txn: &TransactionExpr,
        bound: &HashSet<StringId>,
        acc: &mut HashSet<StaticRef>,
    ) {
        let mut local = bound.clone();
        self.local_decl_names(&txn.stmts, &mut local);
        txn.stmts.iter().for_each(|stmt| {
            self.collect_stmt_refs(
                *stmt,
                &mut local,
                StaticRefMode::Local,
                acc,
            );
        });
        txn.expr.iter().for_each(|expr| {
            self.collect_expr_refs(*expr, &local, acc);
        });
        txn.modifiers.timeout.iter().for_each(|expr| {
            self.collect_expr_refs(*expr, bound, acc);
        });
    }

    fn local_decl_names(&self, stmts: &[StmtId], out: &mut HashSet<StringId>) {
        stmts.iter().for_each(|&id| match self.ast.get_stmt(id) {
            Some(Stmt::Fun { name, .. }) | Some(Stmt::Module { name, .. }) => {
                out.insert(*name);
            }
            _ => {}
        });
    }

    fn static_decl_names(&self, stmts: &[StmtId], out: &mut HashSet<StringId>) {
        stmts.iter().for_each(|&id| match self.ast.get_stmt(id) {
            Some(Stmt::Let(BindingPattern::Var(name), _, _, _))
            | Some(Stmt::Fun { name, .. })
            | Some(Stmt::Module { name, .. }) => {
                out.insert(*name);
            }
            _ => {}
        });
    }

    fn collect_write_refs(
        &self,
        w: &WriteExpr,
        bound: &HashSet<StringId>,
        acc: &mut HashSet<StaticRef>,
    ) {
        self.collect_expr_refs(w.expr, bound, acc);
        if let OutputTarget::File(expr) = w.target {
            self.collect_expr_refs(expr, bound, acc);
        }
    }

    fn collect_ref_target_refs(
        &self,
        target: &RefTarget,
        bound: &HashSet<StringId>,
        acc: &mut HashSet<StaticRef>,
    ) {
        match target {
            RefTarget::Inline(r) => {
                self.collect_db_ref_refs(r, bound, acc);
            }
            RefTarget::Expr(expr) => {
                self.collect_expr_refs(*expr, bound, acc);
            }
        }
    }

    fn collect_db_ref_refs(
        &self,
        r: &DbRef,
        bound: &HashSet<StringId>,
        acc: &mut HashSet<StaticRef>,
    ) {
        let subs = match r {
            DbRef::Local(_, subs) | DbRef::Global(_, subs) => subs,
        };
        subs.iter().for_each(|sub| match sub {
            SubscriptElem::Elem(expr) | SubscriptElem::Spread(expr) => {
                self.collect_expr_refs(*expr, bound, acc);
            }
        });
    }

    fn collect_import_refs(
        &self,
        import: &Import,
        providers: &ProviderIndex,
        acc: &mut HashSet<StaticRef>,
    ) {
        let qn = QualifiedName::new(import.path.to_vec());
        providers.by_mod.get(&qn).into_iter().for_each(|p| {
            Self::provider_import_lets(import, p, |name, _| {
                acc.insert(StaticRef::Path(qn.clone(), name));
            });
        });
    }

    fn binding_names(pat: &BindingPattern, out: &mut HashSet<StringId>) {
        match pat {
            BindingPattern::Var(name) => {
                out.insert(*name);
            }
            BindingPattern::Tuple(pats) => {
                pats.iter().for_each(|p| Self::binding_names(p, out));
            }
            BindingPattern::Object(fields) => {
                fields.iter().for_each(|(_, p)| Self::binding_names(p, out));
            }
            BindingPattern::Array(pats, rest) => {
                pats.iter().for_each(|p| Self::binding_names(p, out));
                if let Some(RestPattern::Bind(name)) = rest {
                    out.insert(*name);
                }
            }
            BindingPattern::Wildcard => {}
        }
    }

    fn if_cond_binding_names(&self, cond: ExprId, out: &mut HashSet<StringId>) {
        self.ast.get_expr(cond).into_iter().for_each(|expr| {
            if let Expr::Is(_, pat) = expr {
                Self::type_pattern_names(pat, out);
            }
        });
    }

    fn type_pattern_names(pat: &TypePattern, out: &mut HashSet<StringId>) {
        match pat {
            TypePattern::VariantBind(_, _, names)
            | TypePattern::NakedVariantBind(_, names) => {
                names.iter().for_each(|name| {
                    out.insert(*name);
                });
            }
            TypePattern::Type(_)
            | TypePattern::Variant(_, _)
            | TypePattern::NakedVariant(_)
            | TypePattern::VariantWildcard(_, _)
            | TypePattern::NakedVariantWildcard(_)
            | TypePattern::Object(_) => {}
        }
    }

    fn match_pattern_names(
        &self,
        id: MatchPatternId,
        out: &mut HashSet<StringId>,
    ) {
        self.ast
            .get_pattern(id)
            .into_iter()
            .for_each(|pat| match pat {
                MatchPattern::Var(name) | MatchPattern::Is(name, _) => {
                    out.insert(*name);
                }
                MatchPattern::Variant(_, _, pats)
                | MatchPattern::NakedVariant(_, pats) => {
                    pats.iter().for_each(|p| self.match_pattern_names(*p, out));
                }
                MatchPattern::Tuple(pats) => {
                    pats.iter().for_each(|p| self.match_pattern_names(*p, out));
                }
                MatchPattern::Object(fields) => {
                    fields
                        .iter()
                        .for_each(|(_, p)| self.match_pattern_names(*p, out));
                }
                MatchPattern::Array(pats, rest) => {
                    pats.iter().for_each(|p| self.match_pattern_names(*p, out));
                    if let Some(RestPattern::Bind(name)) = rest {
                        out.insert(*name);
                    }
                }
                MatchPattern::Wildcard | MatchPattern::Literal(_) => {}
            });
    }
}
