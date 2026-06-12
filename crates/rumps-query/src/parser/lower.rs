//! Lowering pass: CST to AST.
//!
//! Converts the boxed CST representation to the arena-allocated AST.
//! This is a straightforward recursive traversal with direct `&mut Ast` access;
//! no `Rc` or `RefCell` required.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::{fs, iter};

use smallvec::{smallvec, SmallVec};

use super::{cst, Parser};
use crate::ast::{
    self, pragma, ArrayElem, AssocTypeDef, Ast, AstTypeExpr, AstTypeExprId,
    BindingPattern, DbRef, Expr, ExprId, Import, ImportItem, JsonAccessKey,
    MatchArm, MatchPattern, MatchPatternId, ObjectEntry, OutputFormat,
    OutputTarget, PostfixOp, RefTarget, RestPattern, Stmt, StmtId,
    SubscriptElem, TransactionModifiers, TypeDefAst, TypeParam, TypePattern,
    VariantAst, Visibility, WriteExpr,
};
use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::typecheck::{
    ClassDef, ClassRegistry, ClassShape, TyArena, TypeClass,
};
use crate::value::TypeId;
use crate::{ClassId, Error, Lexer, Result, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScopeKind {
    Root,
    Module,
    Block,
    Transaction,
    Interpolation,
}

#[derive(Default)]
struct Pending {
    deriving: Option<(cst::pragma::Deriving, Span)>,
    required_methods: Option<(cst::pragma::RequiredMethods, Span)>,
}

/// Context for lowering; owns the AST being built, tracks base directory,
/// files being parsed, and type parameters in scope (for distinguishing
/// `VarApp` from `App`).
pub(crate) struct LowerCtx<'a> {
    /// The AST being constructed.
    ast: Ast,
    /// Base directory for resolving relative module paths.
    /// `None` means relative paths resolve against cwd.
    base_dir: Option<PathBuf>,
    /// Files currently being parsed (for cycle detection).
    in_progress: HashSet<PathBuf>,
    /// Type parameter names currently in scope.
    ///
    /// Used during type expression lowering to distinguish type variable
    /// applications (`F[T]` -> `VarApp`) from type constructor applications
    /// (`Array[T]` -> `App`).
    type_params: HashSet<StringId>,
    /// Stack of ids added per scope; used by `push_type_params`/`pop_type_params`
    /// to avoid cloning the `HashSet`.
    tp_stack: Vec<SmallVec<[StringId; 4]>>,
    /// Known concrete type names (builtins + user-declared `variant`/`newtype`/`union`).
    ///
    /// Used by `collect_type_vars` to distinguish type variables from concrete
    /// type constructors in `App` head position.
    known_types: HashSet<StringId>,
    /// Class registry for shape resolution during lowering.
    registry: ClassRegistry,
    /// String interner for resolving `StringId` to `&str`.
    interner: &'a mut StringInterner,
}

impl<'a> LowerCtx<'a> {
    fn new(
        base_dir: Option<PathBuf>,
        interner: &'a mut StringInterner,
    ) -> Self {
        let known_types = TypeId::ALL_BUILTINS
            .iter()
            .filter_map(|id| id.name())
            .map(|n| interner.intern(n))
            .collect();
        Self {
            ast: Ast::new(),
            base_dir,
            in_progress: HashSet::new(),
            type_params: HashSet::new(),
            tp_stack: Vec::new(),
            known_types,
            registry: ClassRegistry::builtins(
                &mut |s| interner.intern(s),
                &mut TyArena::new(),
            ),
            interner,
        }
    }

    /// Lower a CST program (list of statements) to AST.
    pub(crate) fn program(
        stmts: Vec<cst::Stmt>,
        interner: &'a mut StringInterner,
    ) -> Result<(Ast, Vec<StmtId>, pragma::Program)> {
        Self::program_with_path(stmts, None, interner)
    }

    pub(crate) fn interpolation_program(
        stmts: Vec<cst::Stmt>,
        interner: &'a mut StringInterner,
    ) -> Result<(Ast, Vec<StmtId>)> {
        let mut ctx = Self::new(None, interner);
        let (stmts, _) =
            ctx.normalize_stmts(stmts, ScopeKind::Interpolation)?;
        ctx.prescan_class_defs(&stmts)?;
        let ids = stmts
            .into_iter()
            .map(|s| ctx.stmt(s))
            .collect::<Result<Vec<_>>>()?;
        Ok((ctx.ast, ids))
    }

    /// Lower a CST program with source file context.
    ///
    /// The `src_path` is used to resolve relative module imports.
    pub(crate) fn program_with_path(
        stmts: Vec<cst::Stmt>,
        src_path: Option<&Path>,
        interner: &'a mut StringInterner,
    ) -> Result<(Ast, Vec<StmtId>, pragma::Program)> {
        let base_dir = src_path.and_then(|p| p.parent().map(Path::to_path_buf));
        let mut ctx = Self::new(base_dir, interner);
        let (stmts, pragmas) = ctx.normalize_stmts(stmts, ScopeKind::Root)?;
        ctx.prescan_class_defs(&stmts)?;
        let ids = stmts
            .into_iter()
            .map(|s| ctx.stmt(s))
            .collect::<Result<Vec<_>>>()?;
        Ok((ctx.ast, ids, pragmas))
    }

    fn normalize_stmts(
        &mut self,
        stmts: Vec<cst::Stmt>,
        scope: ScopeKind,
    ) -> Result<(Vec<cst::Stmt>, pragma::Program)> {
        let mut seen_non_options = false;
        let mut pending = Pending::default();
        let mut pragmas = pragma::Program::default();
        let mut seen_opts = HashSet::new();
        let mut out = Vec::with_capacity(stmts.len());

        stmts.into_iter().try_for_each(|mut stmt| match stmt.kind {
            cst::StmtKind::Pragma(p) => {
                match p {
                    cst::pragma::Kind::Options(opts) => {
                        if scope != ScopeKind::Root {
                            Err(Error::static_err(
                                stmt.span,
                                "`options` pragmas are only allowed at the root",
                            ))?
                        }
                        if seen_non_options {
                            Err(Error::static_err(
                                stmt.span,
                                "`options` pragmas must appear before imports and declarations",
                            ))?
                        }
                        self.lower_db_options(opts, &mut seen_opts, &mut pragmas)
                    }
                    cst::pragma::Kind::Deriving(p) => {
                        seen_non_options = true;
                        if pending.deriving.is_some() {
                            Err(Error::static_err(
                                stmt.span,
                                "duplicate `deriving` pragma for declaration",
                            ))?
                        }
                        pending.deriving = Some((p, stmt.span));
                        Ok(())
                    }
                    cst::pragma::Kind::RequiredMethods(p) => {
                        seen_non_options = true;
                        if pending.required_methods.is_some() {
                            Err(Error::static_err(
                                stmt.span,
                                "duplicate `required` pragma for declaration",
                            ))?
                        }
                        pending.required_methods = Some((p, stmt.span));
                        Ok(())
                    }
                    cst::pragma::Kind::DefaultDefinition => Err(Error::static_err(
                        stmt.span,
                        "`default` pragmas are not supported in this phase",
                    )),
                    cst::pragma::Kind::Unknown(name) => Err(Error::parse(
                        name.span,
                        format!("unknown pragma family `{}`", self.name(name.name)),
                        vec![],
                    )),
                }
            }
            kind => {
                seen_non_options = true;
                stmt.kind = kind;
                self.attach_pragmas(&mut stmt, &mut pending)?;
                out.push(stmt);
                Ok(())
            }
        })?;

        self.reject_pending(pending)?;
        Ok((out, pragmas))
    }

    fn attach_pragmas(
        &self,
        stmt: &mut cst::Stmt,
        pending: &mut Pending,
    ) -> Result<()> {
        if let Some((p, span)) = pending.deriving.take() {
            if matches!(
                &stmt.kind,
                cst::StmtKind::Type { .. }
                    | cst::StmtKind::Newtype { .. }
                    | cst::StmtKind::Union { .. }
            ) {
                stmt.pragmas.deriving = Some(p);
            } else {
                Err(Error::static_err(
                    span,
                    "`deriving` pragmas attach only to `variant`, `newtype`, or `union`",
                ))?
            }
        }

        if let Some((p, span)) = pending.required_methods.take() {
            if matches!(&stmt.kind, cst::StmtKind::ClassDef { .. }) {
                stmt.pragmas.required_methods = Some(p);
            } else {
                Err(Error::static_err(
                    span,
                    "`required` pragmas attach only to class definitions",
                ))?
            }
        }

        Ok(())
    }

    fn reject_pending(&self, pending: Pending) -> Result<()> {
        if let Some((_, span)) = pending.deriving {
            Err(Error::static_err(span, "unattached `deriving` pragma"))?
        } else if let Some((_, span)) = pending.required_methods {
            Err(Error::static_err(span, "unattached `required` pragma"))?
        } else {
            Ok(())
        }
    }

    fn lower_db_options(
        &self,
        opts: cst::pragma::Options,
        seen: &mut HashSet<StringId>,
        pragmas: &mut pragma::Program,
    ) -> Result<()> {
        opts.0.into_iter().try_for_each(|opt| {
            if seen.contains(&opt.name.name) {
                let name = self.name(opt.name.name);
                Err(Error::static_err(
                    opt.name.span,
                    format!("duplicate database option `{name}`"),
                ))?
            }
            seen.insert(opt.name.name);
            let lowered = self.db_option(opt)?;
            pragmas.db_options.push(lowered);
            Ok(())
        })
    }

    fn db_option(
        &self,
        opt: cst::pragma::DbOption,
    ) -> Result<pragma::DbOption> {
        let name = self.name(opt.name.name);
        match name.as_str() {
            "cache-size" => self.db_cache_size(opt),
            "sync-mode" => self.db_sync_mode(opt),
            "wal-max-file-size" => self.db_wal_max_file_size(opt),
            "min-degree" | "max-pages" | "max-memory-bytes" => {
                Err(Error::static_err(
                    opt.name.span,
                    format!("database option `{name}` is rebuild-only"),
                ))
            }
            _ => Err(Error::static_err(
                opt.name.span,
                format!("unknown database option `{name}`"),
            )),
        }
    }

    fn db_cache_size(
        &self,
        opt: cst::pragma::DbOption,
    ) -> Result<pragma::DbOption> {
        match opt.value {
            cst::pragma::Value::Int(v, span) => {
                let size = usize::try_from(v).map_err(|_| {
                    Error::static_err(
                        span,
                        "`cache-size` must be an integer greater than `0`",
                    )
                })?;
                if size > 0 && size.is_power_of_two() {
                    Ok(pragma::DbOption::CacheSize {
                        value: size,
                        span: opt.span,
                    })
                } else {
                    Err(Error::static_err(
                        span,
                        "`cache-size` must be greater than `0` and a power of `2`",
                    ))
                }
            }
            v => Err(Error::static_err(
                Self::pragma_value_span(&v),
                "`cache-size` expects an integer value",
            )),
        }
    }

    fn db_sync_mode(
        &self,
        opt: cst::pragma::DbOption,
    ) -> Result<pragma::DbOption> {
        match opt.value {
            cst::pragma::Value::Ident(n) => {
                let value = match self.name(n.name).as_str() {
                    "immediate" => Ok(pragma::SyncMode::Immediate),
                    "on-commit" => Ok(pragma::SyncMode::OnCommit),
                    "relaxed" => Ok(pragma::SyncMode::Relaxed),
                    "periodic" => Err(Error::static_err(
                        n.span,
                        "`sync-mode = periodic` is not supported by pragma syntax yet",
                    )),
                    other => Err(Error::static_err(
                        n.span,
                        format!("unsupported `sync-mode` value `{other}`"),
                    )),
                }?;
                Ok(pragma::DbOption::SyncMode {
                    value,
                    span: opt.span,
                })
            }
            v => Err(Error::static_err(
                Self::pragma_value_span(&v),
                "`sync-mode` expects an identifier value",
            )),
        }
    }

    fn db_wal_max_file_size(
        &self,
        opt: cst::pragma::DbOption,
    ) -> Result<pragma::DbOption> {
        match opt.value {
            cst::pragma::Value::Int(v, span) => {
                let size = u64::try_from(v).map_err(|_| {
                    Error::static_err(
                        span,
                        "`wal-max-file-size` must be an integer greater than `0`",
                    )
                })?;
                if size > 0 {
                    Ok(pragma::DbOption::WalMaxFileSize {
                        value: size,
                        span: opt.span,
                    })
                } else {
                    Err(Error::static_err(
                        span,
                        "`wal-max-file-size` must be greater than `0`",
                    ))
                }
            }
            v => Err(Error::static_err(
                Self::pragma_value_span(&v),
                "`wal-max-file-size` expects an integer value",
            )),
        }
    }

    fn pragma_value_span(v: &cst::pragma::Value) -> Span {
        match v {
            cst::pragma::Value::Ident(n) => n.span,
            cst::pragma::Value::Int(_, span)
            | cst::pragma::Value::String(_, span) => *span,
        }
    }

    fn name(&self, id: StringId) -> String {
        self.interner.get(id).unwrap_or_default().to_owned()
    }

    /// Pre-scan CST for `ClassDef` nodes and register stubs in the
    /// `ClassRegistry` so that `class()` can resolve user class names
    /// during the main lowering pass.
    fn prescan_class_defs(&mut self, stmts: &[cst::Stmt]) -> Result<()> {
        stmts.iter().try_for_each(|s| {
            if let cst::StmtKind::ClassDef {
                name,
                class_params,
                self_var,
                assoc_types,
                methods,
                ..
            } = &s.kind
            {
                let shape = Self::detect_class_shape(
                    class_params,
                    *self_var,
                    methods,
                    s.span,
                )?;
                let assoc_names = assoc_types.iter().map(|a| a.name).collect();
                let stub = ClassDef {
                    name: *name,
                    shape,
                    assoc_types: assoc_names,
                    methods: vec![],
                    supers: SmallVec::new(),
                };
                self.registry.register(stub).map_err(|e| {
                    let nm = self.interner.get(e.name).unwrap_or_default();
                    Error::static_err(
                        s.span,
                        format!("duplicate class definition `{nm}`"),
                    )
                })?;
                Ok(())
            } else {
                Ok(())
            }
        })
    }

    /// Detect the `ClassShape` from a class definition's CST.
    ///
    /// - Both class params and HKT self var -> `Hkt { kind, params: n }`
    /// - Non-empty `class_params` only -> `Concrete { params: n }`
    /// - Kind is inferred from self-var arity
    /// - Otherwise -> `Concrete { params: 0 }`
    fn detect_class_shape(
        class_params: &[cst::TypeParam],
        sv: StringId,
        methods: &[cst::ClassMethodSig],
        span: Span,
    ) -> Result<ClassShape> {
        let is_param = !class_params.is_empty();
        let kind = Self::self_var_hkt_kind(sv, methods, span)?;
        if is_param && kind > 0 {
            Ok(ClassShape::Hkt {
                kind,
                params: class_params.len() as u8,
            })
        } else if is_param {
            Ok(ClassShape::Concrete {
                params: class_params.len() as u8,
            })
        } else if kind > 0 {
            Ok(ClassShape::Hkt { kind, params: 0 })
        } else {
            Ok(ClassShape::Concrete { params: 0 })
        }
    }

    /// Compute the HKT kind of `sv` from method signatures.
    ///
    /// Returns `0` if `sv` is never used as a type constructor, or
    /// `n` if it is consistently applied to `n` type arguments.
    /// Errors if different methods use inconsistent arities.
    fn self_var_hkt_kind(
        sv: StringId,
        methods: &[cst::ClassMethodSig],
        span: Span,
    ) -> Result<u8> {
        let kind = methods
            .iter()
            .flat_map(|m| {
                m.params
                    .iter()
                    .filter_map(|(_, ty)| ty.as_ref())
                    .chain(m.ret.as_ref())
                    .map(|te| Self::type_expr_hkt_arity(sv, te))
            })
            .filter(|&a| a > 0)
            .try_fold(None::<u8>, |acc, arity| match acc {
                None => Ok(Some(arity)),
                Some(prev) if prev == arity => Ok(Some(arity)),
                Some(prev) => Err(Error::static_err(
                    span,
                    format!(
                        "inconsistent HKT arity for self type: \
                         used as kind-{prev} and kind-{arity}"
                    ),
                )),
            })?;
        Ok(kind.unwrap_or(0))
    }

    /// Return the arity of `sv` when used as a type constructor in `te`,
    /// or `0` if it does not appear in head position.
    /// Propagates the max across children.
    fn type_expr_hkt_arity(sv: StringId, te: &cst::TypeExpr) -> u8 {
        match &te.kind {
            cst::TypeExprKind::App(path, args) => {
                let head = if path.len() == 1 && path.first() == Some(&sv) {
                    args.len() as u8
                } else {
                    0
                };
                args.iter()
                    .map(|a| Self::type_expr_hkt_arity(sv, a))
                    .fold(head, u8::max)
            }
            cst::TypeExprKind::Named(_) | cst::TypeExprKind::Wildcard => 0,
            cst::TypeExprKind::AssocType { .. } => 0,
            cst::TypeExprKind::Fn(params, ret) => params
                .iter()
                .map(|p| Self::type_expr_hkt_arity(sv, p))
                .fold(Self::type_expr_hkt_arity(sv, ret), u8::max),
            cst::TypeExprKind::Tuple(elems)
            | cst::TypeExprKind::Union(elems) => elems
                .iter()
                .map(|e| Self::type_expr_hkt_arity(sv, e))
                .fold(0, u8::max),
            cst::TypeExprKind::Object(fields) => fields
                .iter()
                .map(|(_, te)| Self::type_expr_hkt_arity(sv, te))
                .fold(0, u8::max),
            cst::TypeExprKind::TupleConstructor { .. } => 0,
        }
    }

    fn vis(vis: cst::Visibility) -> Visibility {
        match vis {
            cst::Visibility::Private => Visibility::Private,
            cst::Visibility::Public => Visibility::Public,
        }
    }

    /// Resolve a CST class constraint to a `TypeClass<AstTypeExprId>`.
    ///
    /// Looks up the class by name from the registry and validates arity.
    fn class(
        &mut self,
        c: cst::CstClassConstraint,
    ) -> Result<TypeClass<AstTypeExprId>> {
        let name = self.interner.get(c.tag).unwrap_or_default().to_owned();
        let id = self.registry.lookup_by_name(c.tag).ok_or_else(|| {
            Error::static_err(c.span, format!("unknown class `{name}`"))
        })?;
        let shape = self.registry.shape(id);
        match shape {
            ClassShape::Concrete { params: 0 } => {
                if !c.args.is_empty() {
                    Err(Error::static_err(
                        c.span,
                        format!("`{name}` does not accept type arguments"),
                    ))?
                }
                Ok(TypeClass::simple(id))
            }
            ClassShape::Concrete { params } => {
                if c.args.len() != params as usize {
                    Err(Error::static_err(
                        c.span,
                        format!(
                            "`{name}` expects {params} type argument(s), \
                             but received {}",
                            c.args.len()
                        ),
                    ))?
                }
                let params: SmallVec<[AstTypeExprId; 1]> = c
                    .args
                    .into_iter()
                    .map(|a| self.type_expr(a))
                    .collect::<Result<_>>()?;
                Ok(TypeClass::Concrete { id, params })
            }
            ClassShape::Hkt { params: 0, .. } => {
                if !c.args.is_empty() {
                    Err(Error::static_err(
                        c.span,
                        format!(
                            "`{name}` is higher-kinded; \
                             use `C: {name}` and `C[T]` in type position, \
                             not `C: {name}[T]`"
                        ),
                    ))?
                }
                Ok(TypeClass::hkt(id))
            }
            ClassShape::Hkt { params, .. } => {
                if c.args.len() != params as usize {
                    Err(Error::static_err(
                        c.span,
                        format!(
                            "`{name}` expects {params} fixed type argument(s), \
                             but received {}",
                            c.args.len()
                        ),
                    ))?
                }
                let params: SmallVec<[AstTypeExprId; 1]> = c
                    .args
                    .into_iter()
                    .map(|a| self.type_expr(a))
                    .collect::<Result<_>>()?;
                Ok(TypeClass::Hkt {
                    id,
                    elems: smallvec![],
                    params,
                })
            }
        }
    }

    fn type_pragmas(&self, ps: cst::pragma::Attached) -> Result<pragma::Type> {
        let deriving = ps
            .deriving
            .map(|p| self.deriving_pragma(p))
            .transpose()?
            .unwrap_or_default();
        Ok(pragma::Type { deriving })
    }

    fn deriving_pragma(
        &self,
        p: cst::pragma::Deriving,
    ) -> Result<pragma::Deriving> {
        let mut seen = HashSet::new();
        let ids = p
            .0
            .into_iter()
            .map(|n| {
                let name = self.name(n.name);
                if seen.contains(&n.name) {
                    Err(Error::static_err(
                        n.span,
                        format!("duplicate deriving class `{name}`"),
                    ))?
                }
                seen.insert(n.name);
                let id =
                    self.registry.lookup_by_name(n.name).ok_or_else(|| {
                        Error::static_err(
                            n.span,
                            format!("unknown deriving class `{name}`"),
                        )
                    })?;
                if id.idx() < ClassId::BUILTIN_COUNT {
                    Ok(id)
                } else {
                    Err(Error::static_err(
                        n.span,
                        format!("cannot derive user defined class `{name}`"),
                    ))
                }
            })
            .collect::<Result<SmallVec<_>>>()?;
        Ok(pragma::Deriving(ids))
    }

    fn class_pragmas(
        &self,
        methods: &[cst::ClassMethodSig],
        ps: cst::pragma::Attached,
    ) -> Result<pragma::Class> {
        let required_methods = ps
            .required_methods
            .map(|p| self.required_methods_pragma(methods, p))
            .transpose()?
            .unwrap_or_default();
        Ok(pragma::Class { required_methods })
    }

    fn required_methods_pragma(
        &self,
        methods: &[cst::ClassMethodSig],
        p: cst::pragma::RequiredMethods,
    ) -> Result<pragma::RequiredMethods> {
        let declared: HashSet<_> = methods.iter().map(|m| m.name).collect();
        let mut seen = HashSet::new();
        let ids =
            p.0.into_iter()
                .map(|n| {
                    let name = self.name(n.name);
                    if seen.contains(&n.name) {
                        Err(Error::static_err(
                            n.span,
                            format!("duplicate required method `{name}`"),
                        ))?
                    }
                    seen.insert(n.name);
                    if declared.contains(&n.name) {
                        Ok(n.name)
                    } else {
                        Err(Error::static_err(
                            n.span,
                            format!("unknown required method `{name}`"),
                        ))
                    }
                })
                .collect::<Result<SmallVec<_>>>()?;
        Ok(pragma::RequiredMethods(ids))
    }

    fn normalize_expr_stmts(
        &mut self,
        stmts: Vec<cst::Stmt>,
        tail: Option<Box<cst::Expr>>,
        scope: ScopeKind,
    ) -> Result<(Vec<cst::Stmt>, Option<Box<cst::Expr>>)> {
        let has_tail = tail.is_some();
        let mut all = stmts;
        tail.map(|e| {
            let span = e.span;
            all.push(cst::Stmt::new(cst::StmtKind::Expr(*e), span));
        });
        let (mut norm, _) = self.normalize_stmts(all, scope)?;
        let tail = if has_tail {
            norm.pop().and_then(|s| match s.kind {
                cst::StmtKind::Expr(e) => Some(Box::new(e)),
                _ => None,
            })
        } else {
            None
        };
        Ok((norm, tail))
    }

    /// Convert a CST type parameter to an AST type parameter.
    fn type_param(&mut self, tp: cst::TypeParam) -> Result<TypeParam> {
        let constraints = tp
            .constraints
            .into_iter()
            .map(|c| self.class(c))
            .collect::<Result<_>>()?;
        Ok(TypeParam {
            name: tp.name,
            constraints,
        })
    }

    /// Convert a list of CST type parameters to AST type parameters.
    fn type_param_list(
        &mut self,
        cst_tps: Vec<cst::TypeParam>,
    ) -> Result<SmallVec<[TypeParam; 2]>> {
        cst_tps.into_iter().map(|tp| self.type_param(tp)).collect()
    }

    /// Lower a module from a file path.
    ///
    /// Reads the file, parses it, and merges the resulting statements into the
    /// target AST. The file should contain module body statements (`fun`, `let`,
    /// `module`); this is enforced during typechecking.
    fn module_from_file(
        &mut self,
        path: &str,
        span: Span,
    ) -> Result<Vec<StmtId>> {
        // Resolve path: if relative, resolve against base_dir; otherwise use as-is
        let p = Path::new(path);
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base_dir
                .as_ref()
                .map(|base| base.join(p))
                .unwrap_or_else(|| p.to_path_buf())
        };

        // Canonicalize to absolute path
        let canonical = resolved.canonicalize().map_err(|e| {
            Error::parse(
                span,
                format!("cannot resolve module path `{path}`: {e}"),
                vec![],
            )
        })?;

        // Cycle detection
        if self.in_progress.contains(&canonical) {
            Err(Error::parse(
                span,
                format!("circular module import: `{}`", canonical.display()),
                vec![],
            ))?
        }

        // Mark as in-progress
        self.in_progress.insert(canonical.clone());

        // Read file content
        let content = fs::read_to_string(&canonical).map_err(|e| {
            Error::parse(
                span,
                format!(
                    "cannot read module file `{}`: {e}",
                    canonical.display()
                ),
                vec![],
            )
        })?;

        // Lex and parse to CST (not full AST, we need to lower with our context)
        let tokens = Lexer::new(&content).lex().map_err(|e| {
            Error::parse(
                span,
                format!("error in module file `{}`: {e}", canonical.display()),
                vec![],
            )
        })?;

        let cst_stmts =
            Parser::parse_to_cst(tokens, self.interner).map_err(|e| {
                Error::parse(
                    span,
                    format!(
                        "error in module file `{}`: {e}",
                        canonical.display()
                    ),
                    vec![],
                )
            })?;

        // Lower with updated context (use this file's directory as new base)
        let old_base = self.base_dir.take();
        self.base_dir = canonical.parent().map(Path::to_path_buf);

        let (cst_stmts, _) =
            self.normalize_stmts(cst_stmts, ScopeKind::Module)?;

        let ids = cst_stmts
            .into_iter()
            .map(|s| self.stmt(s))
            .collect::<Result<Vec<_>>>();

        // Restore context
        self.base_dir = old_base;
        self.in_progress.remove(&canonical);

        ids
    }

    /// Push type params into scope, recording which ids were newly inserted
    /// so `pop_type_params` can remove exactly those without cloning the set.
    fn push_type_params(&mut self, params: impl Iterator<Item = StringId>) {
        let added: SmallVec<[StringId; 4]> =
            params.filter(|id| self.type_params.insert(*id)).collect();
        self.tp_stack.push(added);
    }

    /// Remove the type params added by the most recent `push_type_params`.
    fn pop_type_params(&mut self) {
        if let Some(added) = self.tp_stack.pop() {
            added.iter().for_each(|id| {
                self.type_params.remove(id);
            });
        }
    }

    /// Recursively collect type variable names from a CST type expression.
    ///
    /// Used to bring type params from `for_type` into scope before lowering
    /// class instance methods. Concrete type names like `Int` appearing as
    /// `Named` are harmless (`Named` does not check `tps`). For `App` heads,
    /// `known_types` is consulted to avoid treating concrete constructors
    /// (e.g. `Array`, `Pair`) as type variables.
    fn collect_type_vars(
        ty: &cst::TypeExpr,
        known: &HashSet<StringId>,
        out: &mut SmallVec<[StringId; 4]>,
    ) {
        match &ty.kind {
            cst::TypeExprKind::Named(segs) => {
                if let [id] = segs.as_slice() {
                    if !known.contains(id) {
                        out.push(*id);
                    }
                }
            }
            cst::TypeExprKind::App(segs, args) => {
                let last = segs.last();
                let is_known = last.is_some_and(|id| known.contains(id));
                if !is_known {
                    if let Some(id) = last.filter(|_| segs.len() == 1) {
                        out.push(*id);
                    }
                }
                args.iter()
                    .for_each(|a| Self::collect_type_vars(a, known, out));
            }
            cst::TypeExprKind::Fn(ps, ret) => {
                ps.iter()
                    .for_each(|p| Self::collect_type_vars(p, known, out));
                Self::collect_type_vars(ret, known, out);
            }
            cst::TypeExprKind::Tuple(es) | cst::TypeExprKind::Union(es) => {
                es.iter()
                    .for_each(|e| Self::collect_type_vars(e, known, out));
            }
            cst::TypeExprKind::Object(fs) => {
                fs.iter()
                    .for_each(|(_, t)| Self::collect_type_vars(t, known, out));
            }
            cst::TypeExprKind::TupleConstructor { fixed, .. } => {
                fixed.iter().for_each(|(_, t)| {
                    Self::collect_type_vars(t, known, out);
                });
            }
            cst::TypeExprKind::Wildcard
            | cst::TypeExprKind::AssocType { .. } => {}
        }
    }

    /// Lower a CST statement to AST.
    fn stmt(&mut self, stmt: cst::Stmt) -> Result<StmtId> {
        let span = stmt.span;
        let pragmas = stmt.pragmas;
        let s = match stmt.kind {
            cst::StmtKind::Pragma(_) => Err(Error::static_err(
                span,
                "pragma was not normalized before lowering",
            ))?,
            cst::StmtKind::Let(pat, ty, expr, vis) => {
                let pat = self.binding_pattern(pat);
                let ty_id = ty.map(|t| self.type_expr(t)).transpose()?;
                let expr_id = self.expr(expr)?;
                Stmt::Let(pat, ty_id, expr_id, Self::vis(vis))
            }
            cst::StmtKind::Write(output) => {
                let inner_id = self.expr(output.expr)?;
                let format = match output.format {
                    cst::OutputFormat::Default => OutputFormat::Default,
                    cst::OutputFormat::Json => OutputFormat::Json,
                    cst::OutputFormat::Raw => OutputFormat::Raw,
                };
                let target = match output.target {
                    cst::OutputTarget::Stdout => OutputTarget::Stdout,
                    cst::OutputTarget::Stderr => OutputTarget::Stderr,
                    cst::OutputTarget::File(path_expr) => {
                        let path_id = self.expr(*path_expr)?;
                        OutputTarget::File(path_id)
                    }
                };
                let expr = Expr::Write(WriteExpr {
                    expr: inner_id,
                    format,
                    target,
                });
                let expr_id = self.ast.add_expr(expr, span)?;
                Stmt::Expr(expr_id)
            }
            cst::StmtKind::Expr(expr) => {
                let expr_id = self.expr(expr)?;
                Stmt::Expr(expr_id)
            }
            cst::StmtKind::Fun {
                name,
                type_params,
                params,
                ret,
                body,
                vis,
            } => {
                // Push type param names into scope
                self.push_type_params(type_params.iter().map(|tp| tp.name));

                let params_lowered = params
                    .into_iter()
                    .map(|(n, t)| {
                        t.map(|te| self.type_expr(te))
                            .transpose()
                            .map(|ty_id| (n, ty_id))
                    })
                    .collect::<Result<SmallVec<_>>>()?;
                let ret_id = ret.map(|t| self.type_expr(t)).transpose()?;
                let tp_lowered = self.type_param_list(type_params)?;

                // Lower body (may contain nested closures that see outer type params)
                let body_id = self.expr(body)?;

                self.pop_type_params();
                Stmt::Fun {
                    name,
                    type_params: tp_lowered,
                    params: params_lowered,
                    ret: ret_id,
                    body: body_id,
                    vis: Self::vis(vis),
                }
            }
            cst::StmtKind::Type {
                name,
                type_params,
                def,
                vis,
            } => {
                let pragmas = self.type_pragmas(pragmas)?;
                self.known_types.insert(name);
                self.push_type_params(type_params.iter().map(|tp| tp.name));
                let def_lowered = self.type_def(def)?;
                let tp_lowered = self.type_param_list(type_params)?;
                self.pop_type_params();
                Stmt::Type {
                    name,
                    type_params: tp_lowered,
                    def: def_lowered,
                    vis: Self::vis(vis),
                    pragmas,
                }
            }
            cst::StmtKind::Newtype {
                name,
                type_params,
                target,
                vis,
                repr_vis,
            } => {
                let pragmas = self.type_pragmas(pragmas)?;
                self.known_types.insert(name);
                self.push_type_params(type_params.iter().map(|tp| tp.name));
                let target_id = self.type_expr(target)?;
                let tp_lowered = self.type_param_list(type_params)?;
                self.pop_type_params();
                Stmt::Newtype {
                    name,
                    type_params: tp_lowered,
                    target: target_id,
                    vis: Self::vis(vis),
                    repr_vis: Self::vis(repr_vis),
                    pragmas,
                }
            }
            cst::StmtKind::Union {
                name,
                type_params,
                members,
                vis,
            } => {
                let pragmas = self.type_pragmas(pragmas)?;
                self.known_types.insert(name);
                self.push_type_params(type_params.iter().map(|tp| tp.name));
                let member_ids = members
                    .into_iter()
                    .map(|t| self.type_expr(t))
                    .collect::<Result<SmallVec<_>>>()?;
                let tp_lowered = self.type_param_list(type_params)?;
                self.pop_type_params();
                Stmt::Union {
                    name,
                    type_params: tp_lowered,
                    members: member_ids,
                    vis: Self::vis(vis),
                    pragmas,
                }
            }
            cst::StmtKind::Module { name, source } => {
                let body_ids = match source {
                    cst::ModuleSource::Inline(body) => {
                        let (body, _) =
                            self.normalize_stmts(body, ScopeKind::Module)?;
                        body.into_iter()
                            .map(|s| self.stmt(s))
                            .collect::<Result<Vec<_>>>()?
                    }
                    cst::ModuleSource::File(path) => {
                        self.module_from_file(&path, span)?
                    }
                };
                Stmt::Module {
                    name,
                    body: body_ids,
                }
            }
            cst::StmtKind::Import(imp) => {
                let items = imp
                    .items
                    .into_iter()
                    .map(|item| match item {
                        cst::ImportItem::Named { name, alias } => {
                            ImportItem::Named { name, alias }
                        }
                        cst::ImportItem::Wildcard => ImportItem::Wildcard,
                        cst::ImportItem::Exclude(n) => ImportItem::Exclude(n),
                    })
                    .collect();
                Stmt::Import(Import {
                    path: imp.path,
                    items,
                })
            }
            cst::StmtKind::ClassDef {
                name,
                class_params,
                self_var,
                supers,
                assoc_types,
                methods,
            } => {
                let pragmas = self.class_pragmas(&methods, pragmas)?;
                self.push_type_params(
                    class_params
                        .iter()
                        .map(|tp| tp.name)
                        .chain(iter::once(self_var)),
                );

                // Push method-local type params for each method
                // (they share the same scope as class params for lowering)
                let methods_lowered = methods
                    .into_iter()
                    .map(|m| {
                        self.push_type_params(
                            m.type_params.iter().map(|tp| tp.name),
                        );
                        let params = m
                            .params
                            .into_iter()
                            .map(|(n, t)| {
                                t.map(|te| self.type_expr(te))
                                    .transpose()
                                    .map(|ty_id| (n, ty_id))
                            })
                            .collect::<Result<SmallVec<_>>>()?;
                        let ret =
                            m.ret.map(|t| self.type_expr(t)).transpose()?;
                        let tp = self.type_param_list(m.type_params)?;
                        self.pop_type_params();
                        Ok(ast::AstClassMethodSig {
                            name: m.name,
                            type_params: tp,
                            params,
                            ret,
                            span: m.span,
                        })
                    })
                    .collect::<Result<SmallVec<_>>>()?;

                let supers_lowered = supers
                    .into_iter()
                    .map(|c| self.class(c))
                    .collect::<Result<SmallVec<_>>>()?;
                let cp_lowered = self.type_param_list(class_params)?;

                let assoc_lowered = assoc_types
                    .into_iter()
                    .map(|a| ast::AstClassAssocTypeDecl {
                        name: a.name,
                        span: a.span,
                    })
                    .collect();

                self.pop_type_params();
                Stmt::ClassDef {
                    name,
                    class_params: cp_lowered,
                    self_var,
                    supers: supers_lowered,
                    assoc_types: assoc_lowered,
                    methods: methods_lowered,
                    pragmas,
                }
            }
            cst::StmtKind::ClassInstance {
                class_name,
                class_args,
                type_params,
                for_type,
                constraints,
                assoc_types,
                methods,
            } => {
                // Collect explicit type params + inferred type vars
                // from `for_type` into scope as two stack frames.
                self.push_type_params(type_params.iter().map(|tp| tp.name));
                let mut tvars = SmallVec::new();
                Self::collect_type_vars(
                    &for_type,
                    &self.known_types,
                    &mut tvars,
                );
                self.push_type_params(tvars.into_iter());

                // Lower type-level items
                let class_args_ids = class_args
                    .into_iter()
                    .map(|t| self.type_expr(t))
                    .collect::<Result<SmallVec<_>>>()?;
                let for_type_id = self.type_expr(for_type)?;
                let constraints_lowered = constraints
                    .into_iter()
                    .map(|(name, classes)| {
                        classes
                            .into_iter()
                            .map(|c| self.class(c))
                            .collect::<Result<SmallVec<_>>>()
                            .map(|cs| (name, cs))
                    })
                    .collect::<Result<SmallVec<_>>>()?;
                let assoc_types_lowered = assoc_types
                    .into_iter()
                    .map(|a| self.assoc_type_def(a))
                    .collect::<Result<SmallVec<_>>>()?;
                let tp_lowered = self.type_param_list(type_params)?;

                // Lower methods (contain bodies that need `&mut self`)
                let methods_lowered = methods
                    .into_iter()
                    .map(|m| self.instance_method(m))
                    .collect::<Result<SmallVec<_>>>()?;

                self.pop_type_params();
                self.pop_type_params();
                Stmt::ClassInstance {
                    class_name,
                    class_args: class_args_ids,
                    type_params: tp_lowered,
                    for_type: for_type_id,
                    constraints: constraints_lowered,
                    assoc_types: assoc_types_lowered,
                    methods: methods_lowered,
                }
            }
        };
        self.ast.add_stmt(s, span)
    }

    /// Lower a CST instance method to AST.
    fn instance_method(
        &mut self,
        m: cst::InstanceMethodDef,
    ) -> Result<ast::InstanceMethodDef> {
        self.push_type_params(m.type_params.iter().map(|tp| tp.name));
        let params = m
            .params
            .into_iter()
            .map(|(n, t)| {
                t.map(|te| self.type_expr(te))
                    .transpose()
                    .map(|ty_id| (n, ty_id))
            })
            .collect::<Result<SmallVec<_>>>()?;
        let ret = m.ret.map(|t| self.type_expr(t)).transpose()?;
        let type_params = self.type_param_list(m.type_params)?;
        let body = self.expr(m.body)?;
        self.pop_type_params();
        Ok(ast::InstanceMethodDef {
            name: m.name,
            type_params,
            params,
            ret,
            body,
            span: m.span,
        })
    }

    /// Lower a CST associated type definition to AST.
    fn assoc_type_def(&mut self, a: cst::AssocTypeCst) -> Result<AssocTypeDef> {
        let constraint = a.constraint.map(|c| self.class(c)).transpose()?;
        let target = self.type_expr(a.target)?;
        Ok(AssocTypeDef {
            name: a.name,
            constraint,
            target,
            span: a.span,
        })
    }

    /// Lower a CST expression to AST.
    fn expr(&mut self, expr: cst::Expr) -> Result<ExprId> {
        let span = expr.span;
        let e = match expr.kind {
            cst::ExprKind::Literal(lit) => Expr::Literal(lit),
            cst::ExprKind::Interpolation(parts) => {
                self.interpolation(parts, span)?
            }
            cst::ExprKind::Var(name) => Expr::Var(name),
            cst::ExprKind::Intrinsic(op, r, value) => {
                let rt = self.ref_arg(*r)?;
                let val =
                    value.map(|v| self.expr(*v)).transpose()?;
                Expr::Intrinsic(op, rt, val, None)
            }
            cst::ExprKind::Binary(lhs, op, rhs) => {
                let lhs_id = self.expr(*lhs)?;
                let rhs_id = self.expr(*rhs)?;
                Expr::Binary(lhs_id, op, rhs_id)
            }
            cst::ExprKind::Unary(op, operand) => {
                let operand_id = self.expr(*operand)?;
                Expr::Unary(op, operand_id)
            }
            cst::ExprKind::Call(callee, args) => {
                let callee_id = self.expr(*callee)?;
                let arg_ids = self.exprs(args)?;
                Expr::Call(callee_id, arg_ids)
            }
            cst::ExprKind::Object(entries) => {
                let lowered = entries
                    .into_iter()
                    .map(|e| self.object_entry(e))
                    .collect::<Result<Vec<_>>>()?;
                Expr::Object(lowered)
            }
            cst::ExprKind::Array(elems) => {
                let lowered = elems
                    .into_iter()
                    .map(|e| self.array_elem(e))
                    .collect::<Result<Vec<_>>>()?;
                Expr::Array(lowered)
            }
            cst::ExprKind::Tuple(elems) => {
                let elem_ids = elems
                    .into_iter()
                    .map(|e| self.expr(e))
                    .collect::<Result<SmallVec<_>>>()?;
                Expr::Tuple(elem_ids)
            }
            cst::ExprKind::MapLit(entries) => {
                let entry_ids = entries
                    .into_iter()
                    .map(|(k, v)| {
                        let k_id = self.expr(k)?;
                        let v_id = self.expr(v)?;
                        Ok((k_id, v_id))
                    })
                    .collect::<Result<SmallVec<_>>>()?;
                Expr::MapLit(entry_ids)
            }
            cst::ExprKind::TupleIndex(base, idx) => {
                let base_id = self.expr(*base)?;
                Expr::TupleIndex(base_id, idx)
            }
            cst::ExprKind::Index(base, idx) => {
                let base_id = self.expr(*base)?;
                let idx_id = self.expr(*idx)?;
                Expr::Index(base_id, idx_id)
            }
            cst::ExprKind::OptionalIndex(base, idx) => {
                let base_id = self.expr(*base)?;
                let idx_id = self.expr(*idx)?;
                Expr::OptionalIndex(base_id, idx_id)
            }
            cst::ExprKind::Field(base, field) => {
                let base_id = self.expr(*base)?;
                Expr::Field(base_id, field)
            }
            cst::ExprKind::OptionalField(base, field) => {
                let base_id = self.expr(*base)?;
                Expr::OptionalField(base_id, field)
            }
            cst::ExprKind::Variant(ty, var, args) => {
                let arg_ids = self.exprs(args)?;
                Expr::Variant(QualifiedName::local(ty), var, arg_ids)
            }
            cst::ExprKind::NakedVariant(var, args) => {
                let arg_ids = self.exprs(args)?;
                Expr::NakedVariant(var, arg_ids)
            }
            cst::ExprKind::Is(inner, pattern) => {
                let inner_id = self.expr(*inner)?;
                let lowered_pat = self.type_pattern(pattern)?;
                Expr::Is(inner_id, lowered_pat)
            }
            cst::ExprKind::As(inner, ty) => {
                let inner_id = self.expr(*inner)?;
                let ty_id = self.type_expr(ty)?;
                Expr::As(inner_id, ty_id)
            }
            cst::ExprKind::Read(inner, ty) => {
                let inner_id = self.expr(*inner)?;
                let ty_id = self.type_expr(ty)?;
                Expr::Read(inner_id, ty_id)
            }
            cst::ExprKind::Block(stmts, tail) => {
                let (stmts, tail) =
                    self.normalize_expr_stmts(stmts, tail, ScopeKind::Block)?;
                let stmt_ids = stmts
                    .into_iter()
                    .map(|s| self.stmt(s))
                    .collect::<Result<Vec<_>>>()?;
                let tail_id = tail
                    .map(|e| self.expr(*e))
                    .transpose()?;
                Expr::Block(stmt_ids, tail_id)
            }
            cst::ExprKind::If(cond, then_br, else_br) => {
                let cond_id = self.expr(*cond)?;
                let then_id = self.expr(*then_br)?;
                let else_id = else_br
                    .map(|e| self.expr(*e))
                    .transpose()?;
                Expr::If(cond_id, then_id, else_id)
            }
            cst::ExprKind::Closure {
                type_params,
                params,
                ret,
                body,
            } => {
                // Push closure type params into scope
                self.push_type_params(
                    type_params.iter().map(|tp| tp.name),
                );

                let params_lowered = params
                    .into_iter()
                    .map(|(n, t)| {
                        t.map(|te| self.type_expr(te))
                            .transpose()
                            .map(|ty_id| (n, ty_id))
                    })
                    .collect::<Result<SmallVec<_>>>()?;
                let ret_id = ret
                    .map(|t| self.type_expr(t))
                    .transpose()?;
                let tp_lowered =
                    self.type_param_list(type_params)?;

                let body_id = self.expr(*body)?;

                self.pop_type_params();
                Expr::Closure {
                    type_params: tp_lowered,
                    params: params_lowered,
                    ret: ret_id,
                    body: body_id,
                }
            }
            cst::ExprKind::Match(scrutinee, arms) => {
                let scrutinee_id = self.expr(*scrutinee)?;
                let arms_lowered = arms
                    .into_iter()
                    .map(|arm| self.match_arm(arm))
                    .collect::<Result<Vec<_>>>()?;
                Expr::Match(scrutinee_id, arms_lowered)
            }
            cst::ExprKind::Unwrap(inner) => {
                let inner_id = self.expr(*inner)?;
                Expr::Postfix(PostfixOp::Unwrap, inner_id)
            }
            cst::ExprKind::Range(start, end, inclusive) => {
                let start_id = self.expr(*start)?;
                let end_id = self.expr(*end)?;
                Expr::Range(start_id, end_id, inclusive)
            }
            cst::ExprKind::Annotate(inner, ty) => {
                let inner_id = self.expr(*inner)?;
                let ty_id = self.type_expr(ty)?;
                Expr::Annotate(inner_id, ty_id)
            }
            cst::ExprKind::Json(fields) => {
                let field_ids = fields
                    .into_iter()
                    .map(|(k, v)| {
                        let kid = self.interner.intern(&k);
                        self.expr(v).map(|id| (kid, id))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Expr::Json(field_ids)
            }
            cst::ExprKind::JsonAccess(base, kind, key) => {
                let base_id = self.expr(*base)?;
                let key_lowered = match key {
                    cst::JsonAccessKey::Field(name) => {
                        JsonAccessKey::Field(name)
                    }
                    cst::JsonAccessKey::Expr(e) => {
                        let e_id = self.expr(*e)?;
                        JsonAccessKey::Expr(e_id)
                    }
                };
                Expr::JsonAccess(base_id, kind, key_lowered)
            }
            cst::ExprKind::Regex(pattern) => {
                Expr::Regex(pattern, None)
            }
            cst::ExprKind::Matches(lhs, rhs) => {
                let lhs_id = self.expr(*lhs)?;
                let rhs_id = self.expr(*rhs)?;
                Expr::Matches(lhs_id, rhs_id)
            }
            cst::ExprKind::Catch(expr, handler) => {
                let expr_id = self.expr(*expr)?;
                let handler_id = self.expr(*handler)?;
                Expr::Catch(expr_id, handler_id)
            }
            cst::ExprKind::Write(output) => {
                let expr_id = self.expr(output.expr)?;
                let format = match output.format {
                    cst::OutputFormat::Default => OutputFormat::Default,
                    cst::OutputFormat::Json => OutputFormat::Json,
                    cst::OutputFormat::Raw => OutputFormat::Raw,
                };
                let target = match output.target {
                    cst::OutputTarget::Stdout => OutputTarget::Stdout,
                    cst::OutputTarget::Stderr => OutputTarget::Stderr,
                    cst::OutputTarget::File(path_expr) => {
                        let path_id =
                            self.expr(*path_expr)?;
                        OutputTarget::File(path_id)
                    }
                };
                Expr::Write(WriteExpr {
                    expr: expr_id,
                    format,
                    target,
                })
            }
            cst::ExprKind::Raise(inner) => {
                let id = self.expr(*inner)?;
                Expr::Raise(id)
            }
            cst::ExprKind::Loop {
                seed,
                state_param,
                cont_param,
                body,
            } => {
                let seed_id = self.expr(*seed)?;
                let state_ty = state_param
                    .1
                    .map(|t| self.type_expr(t))
                    .transpose()?;
                let cont_ty = cont_param
                    .1
                    .map(|t| self.type_expr(t))
                    .transpose()?;
                let body_id = self.expr(*body)?;
                Expr::Loop {
                    seed: seed_id,
                    state_param: (state_param.0, state_ty),
                    cont_param: (cont_param.0, cont_ty),
                    body: body_id,
                }
            }
            cst::ExprKind::Transaction(txn) => {
                let cst::TransactionExpr {
                    stmts,
                    expr,
                    modifiers,
                } = *txn;
                let (stmts, expr) =
                    self.normalize_expr_stmts(stmts, expr, ScopeKind::Transaction)?;
                let stmt_ids = stmts
                    .into_iter()
                    .map(|s| self.stmt(s))
                    .collect::<Result<Vec<_>>>()?;
                let expr = expr.map(|e| self.expr(*e)).transpose()?;
                let modifiers = self.txn_modifiers(modifiers)?;
                Expr::Transaction(ast::TransactionExpr {
                    id: None,
                    stmts: stmt_ids,
                    expr,
                    modifiers,
                })
            }
            cst::ExprKind::DefaultValue => Expr::DefaultValue,
            cst::ExprKind::RefLit(dbref) => {
                let dbref = self.db_ref(dbref)?;
                Expr::Ref(dbref)
            }
            cst::ExprKind::ClassMethod(class, method, args) => {
                let arg_ids = self.exprs(args)?;
                Expr::ClassMethod(class, method, arg_ids)
            }
            cst::ExprKind::ClassMethodRef(class, type_args, method) => {
                let type_arg_ids = type_args
                    .into_iter()
                    .map(|t| self.type_expr(t))
                    .collect::<Result<SmallVec<_>>>()?;
                Expr::ClassMethodRef(class, type_arg_ids, method)
            }
            cst::ExprKind::NakedClassMethod(method, args) => {
                let arg_ids = self.exprs(args)?;
                Expr::NakedClassMethod(method, arg_ids)
            }
            cst::ExprKind::NakedClassMethodRef(method) => {
                Expr::NakedClassMethodRef(method)
            }
            cst::ExprKind::PipePlaceholder => {
                Err(Error::parse(
                    span,
                    "pipe placeholder `.` can only appear in call arguments on RHS of `|>`",
                    vec![],
                ))?
            }
            cst::ExprKind::Error(msg) => {
                Err(Error::parse(span, msg, vec![]))?
            }
        };
        self.ast.add_expr(e, span)
    }

    /// Lower a list of CST expressions to AST, returning a `SmallVec`.
    fn exprs(
        &mut self,
        exprs: Vec<cst::Expr>,
    ) -> Result<SmallVec<[ExprId; 4]>> {
        exprs
            .into_iter()
            .map(|e| self.expr(e))
            .collect::<Result<SmallVec<_>>>()
    }

    /// Lower a CST type expression to AST.
    ///
    /// When `App("F", ...)` is encountered and `"F"` is in `self.type_params`,
    /// it becomes `VarApp("F", ...)` instead, marking it as a type variable
    /// application (HKT).
    fn type_expr(&mut self, ty: cst::TypeExpr) -> Result<AstTypeExprId> {
        let span = ty.span;
        let te = match ty.kind {
            cst::TypeExprKind::Wildcard => AstTypeExpr::Wildcard,
            cst::TypeExprKind::Named(segs) => {
                AstTypeExpr::Named(QualifiedName::new(segs))
            }
            cst::TypeExprKind::App(segs, params) => {
                let param_ids = params
                    .into_iter()
                    .map(|t| self.type_expr(t))
                    .collect::<Result<SmallVec<_>>>()?;
                let is_tv = segs.len() == 1
                    && segs
                        .first()
                        .is_some_and(|id| self.type_params.contains(id));
                let qn = QualifiedName::new(segs);
                if is_tv {
                    AstTypeExpr::VarApp(qn, param_ids)
                } else {
                    AstTypeExpr::App(qn, param_ids)
                }
            }
            cst::TypeExprKind::Fn(params, ret) => {
                let param_ids = params
                    .into_iter()
                    .map(|t| self.type_expr(t))
                    .collect::<Result<SmallVec<_>>>()?;
                let ret_id = self.type_expr(*ret)?;
                AstTypeExpr::Fn(param_ids, ret_id)
            }
            cst::TypeExprKind::Tuple(elems) => {
                let elem_ids = elems
                    .into_iter()
                    .map(|t| self.type_expr(t))
                    .collect::<Result<SmallVec<_>>>()?;
                AstTypeExpr::Tuple(elem_ids)
            }
            cst::TypeExprKind::Union(members) => {
                let member_ids = members
                    .into_iter()
                    .map(|t| self.type_expr(t))
                    .collect::<Result<SmallVec<_>>>()?;
                AstTypeExpr::Union(member_ids)
            }
            cst::TypeExprKind::Object(fields) => {
                let lowered = fields
                    .into_iter()
                    .map(|(name, ty)| self.type_expr(ty).map(|id| (name, id)))
                    .collect::<Result<SmallVec<_>>>()?;
                AstTypeExpr::Object(lowered)
            }
            cst::TypeExprKind::AssocType { class, name } => {
                AstTypeExpr::AssocType { class, name }
            }
            cst::TypeExprKind::TupleConstructor { arity, fixed } => {
                let lowered = fixed
                    .into_iter()
                    .map(|(pos, ty)| self.type_expr(ty).map(|id| (pos, id)))
                    .collect::<Result<SmallVec<_>>>()?;
                AstTypeExpr::TupleConstructor {
                    arity,
                    fixed: lowered,
                }
            }
        };
        self.ast.add_type_expr(te, span)
    }

    /// Lower a CST type pattern to AST.
    fn type_pattern(&mut self, pat: cst::TypePattern) -> Result<TypePattern> {
        Ok(match pat {
            cst::TypePattern::Type(ty) => {
                TypePattern::Type(self.type_expr(ty)?)
            }
            cst::TypePattern::Variant(ty, var) => {
                TypePattern::Variant(QualifiedName::local(ty), var)
            }
            cst::TypePattern::NakedVariant(var) => {
                TypePattern::NakedVariant(var)
            }
            cst::TypePattern::VariantWildcard(ty, var) => {
                TypePattern::VariantWildcard(QualifiedName::local(ty), var)
            }
            cst::TypePattern::NakedVariantWildcard(var) => {
                TypePattern::NakedVariantWildcard(var)
            }
            cst::TypePattern::VariantBind(ty, var, names) => {
                TypePattern::VariantBind(QualifiedName::local(ty), var, names)
            }
            cst::TypePattern::NakedVariantBind(var, names) => {
                TypePattern::NakedVariantBind(var, names)
            }
            cst::TypePattern::Object(fields) => {
                let lowered = fields
                    .into_iter()
                    .map(|(name, ty)| self.type_expr(ty).map(|id| (name, id)))
                    .collect::<Result<SmallVec<_>>>()?;
                TypePattern::Object(lowered)
            }
        })
    }

    /// Lower a CST binding pattern to AST.
    fn binding_pattern(&self, pat: cst::BindingPattern) -> BindingPattern {
        match pat {
            cst::BindingPattern::Var(name) => BindingPattern::Var(name),
            cst::BindingPattern::Tuple(pats) => BindingPattern::Tuple(
                pats.into_iter().map(|p| self.binding_pattern(p)).collect(),
            ),
            cst::BindingPattern::Object(fields) => BindingPattern::Object(
                fields
                    .into_iter()
                    .map(|(k, p)| (k, self.binding_pattern(p)))
                    .collect(),
            ),
            cst::BindingPattern::Array(pats, rest) => BindingPattern::Array(
                pats.into_iter().map(|p| self.binding_pattern(p)).collect(),
                rest.map(|r| self.rest_pattern(r)),
            ),
            cst::BindingPattern::Wildcard => BindingPattern::Wildcard,
        }
    }

    /// Lower a CST rest pattern to AST.
    fn rest_pattern(&self, pat: cst::RestPattern) -> RestPattern {
        match pat {
            cst::RestPattern::Ignore => RestPattern::Ignore,
            cst::RestPattern::Bind(name) => RestPattern::Bind(name),
        }
    }

    /// Lower a CST match pattern to AST, allocating into the pattern arena.
    fn match_pattern(
        &mut self,
        pat: cst::MatchPattern,
    ) -> Result<MatchPatternId> {
        let p = match pat {
            cst::MatchPattern::Wildcard => MatchPattern::Wildcard,
            cst::MatchPattern::Var(name) => MatchPattern::Var(name),
            cst::MatchPattern::Literal(lit) => MatchPattern::Literal(lit),
            cst::MatchPattern::Variant(ty, var, pats) => {
                let sub_ids = pats
                    .into_iter()
                    .map(|p| self.match_pattern(p))
                    .collect::<Result<SmallVec<_>>>()?;
                MatchPattern::Variant(QualifiedName::new(ty), var, sub_ids)
            }
            cst::MatchPattern::NakedVariant(var, pats) => {
                let sub_ids = pats
                    .into_iter()
                    .map(|p| self.match_pattern(p))
                    .collect::<Result<SmallVec<_>>>()?;
                MatchPattern::NakedVariant(var, sub_ids)
            }
            cst::MatchPattern::Object(fields) => {
                let field_ids = fields
                    .into_iter()
                    .map(|(k, p)| self.match_pattern(p).map(|id| (k, id)))
                    .collect::<Result<SmallVec<_>>>()?;
                MatchPattern::Object(field_ids)
            }
            cst::MatchPattern::Tuple(pats) => {
                let elem_ids = pats
                    .into_iter()
                    .map(|p| self.match_pattern(p))
                    .collect::<Result<SmallVec<_>>>()?;
                MatchPattern::Tuple(elem_ids)
            }
            cst::MatchPattern::Array(pats, rest) => {
                let elem_ids = pats
                    .into_iter()
                    .map(|p| self.match_pattern(p))
                    .collect::<Result<SmallVec<_>>>()?;
                MatchPattern::Array(
                    elem_ids,
                    rest.map(|r| self.rest_pattern(r)),
                )
            }
            cst::MatchPattern::Is(name, ty) => {
                let ty_id = self.type_expr(ty)?;
                MatchPattern::Is(name, ty_id)
            }
        };
        self.ast.add_pattern(p)
    }

    /// Lower a CST type definition to AST.
    fn type_def(&mut self, def: cst::TypeDefCst) -> Result<TypeDefAst> {
        match def {
            cst::TypeDefCst::Sum(variants) => {
                let lowered = variants
                    .into_iter()
                    .map(|v| self.variant(v))
                    .collect::<Result<SmallVec<_>>>()?;
                Ok(TypeDefAst::Sum(lowered))
            }
        }
    }

    /// Lower a CST variant to AST.
    fn variant(&mut self, v: cst::VariantCst) -> Result<VariantAst> {
        let payloads = v
            .payloads
            .into_iter()
            .map(|t| self.type_expr(t))
            .collect::<Result<SmallVec<_>>>()?;
        Ok(VariantAst {
            name: v.name,
            payloads,
        })
    }

    /// Lower a CST match arm to AST.
    fn match_arm(&mut self, arm: cst::MatchArm) -> Result<MatchArm> {
        let pattern = self.match_pattern(arm.pattern)?;
        let guard = arm.guard.map(|e| self.expr(e)).transpose()?;
        let body = self.expr(arm.body)?;
        Ok(MatchArm {
            pattern,
            guard,
            body,
        })
    }

    /// Lower a CST array element to AST.
    fn array_elem(&mut self, elem: cst::ArrayElem) -> Result<ArrayElem> {
        match elem {
            cst::ArrayElem::Elem(e) => self.expr(e).map(ArrayElem::Elem),
            cst::ArrayElem::Spread(e) => self.expr(e).map(ArrayElem::Spread),
        }
    }

    /// Lower a CST object entry to AST.
    fn object_entry(&mut self, entry: cst::ObjectEntry) -> Result<ObjectEntry> {
        match entry {
            cst::ObjectEntry::Field(k, v) => {
                self.expr(v).map(|id| ObjectEntry::Field(k, id))
            }
            cst::ObjectEntry::Spread(e) => {
                self.expr(e).map(ObjectEntry::Spread)
            }
        }
    }

    /// Lower a CST subscript element to AST.
    fn subscript_elem(
        &mut self,
        elem: cst::SubscriptElem,
    ) -> Result<SubscriptElem> {
        match elem {
            cst::SubscriptElem::Elem(e) => {
                self.expr(e).map(SubscriptElem::Elem)
            }
            cst::SubscriptElem::Spread(e) => {
                self.expr(e).map(SubscriptElem::Spread)
            }
        }
    }

    /// Lower a list of CST subscript elements to AST.
    fn subscript_elems(
        &mut self,
        elems: Vec<cst::SubscriptElem>,
    ) -> Result<SmallVec<[SubscriptElem; 4]>> {
        elems.into_iter().map(|e| self.subscript_elem(e)).collect()
    }

    /// Lower a CST database reference to AST.
    fn db_ref(&mut self, dbref: cst::DbRef) -> Result<DbRef> {
        match dbref {
            cst::DbRef::Local(name, subs) => {
                let sub_ids = self.subscript_elems(subs)?;
                Ok(DbRef::Local(name, sub_ids))
            }
            cst::DbRef::Global(name, subs) => {
                let sub_ids = self.subscript_elems(subs)?;
                Ok(DbRef::Global(name, sub_ids))
            }
        }
    }

    /// Lower a ref argument expression to `RefTarget`.
    ///
    /// If the expression is a `RefLit`, uses `RefTarget::Inline`; otherwise
    /// lowers the expression and uses `RefTarget::Expr`.
    fn ref_arg(&mut self, expr: cst::Expr) -> Result<RefTarget> {
        match expr.kind {
            cst::ExprKind::RefLit(dbref) => {
                self.db_ref(dbref).map(RefTarget::Inline)
            }
            _ => self.expr(expr).map(RefTarget::Expr),
        }
    }

    /// Lower CST transaction modifiers to AST.
    fn txn_modifiers(
        &mut self,
        m: cst::TransactionModifiers,
    ) -> Result<TransactionModifiers> {
        let conflict = m.conflict.map(|c| match c {
            cst::ConflictModifier::Abort => {
                rumps_storage::ConflictStrategy::Abort
            }
            cst::ConflictModifier::Overwrite => {
                rumps_storage::ConflictStrategy::Overwrite
            }
        });
        let timeout = m.timeout.map(|e| self.expr(*e)).transpose()?;
        let isolation = m.isolation.map(|i| match i {
            cst::IsolationModifier::Snapshot => {
                rumps_storage::IsolationLevel::SnapshotIsolation
            }
        });
        Ok(TransactionModifiers {
            conflict,
            timeout,
            retries: m.retries,
            isolation,
        })
    }

    /// Lower interpolated string parts to an AST expression.
    ///
    /// Takes the alternating literal/expression parts and parses expression
    /// strings into AST nodes. Returns an `Expr::Interpolation` containing the
    /// parsed parts.
    fn interpolation(
        &mut self,
        parts: Vec<String>,
        span: Span,
    ) -> Result<Expr> {
        let ids: Result<SmallVec<[ExprId; 4]>> = parts
            .into_iter()
            .enumerate()
            .map(|(i, part)| {
                if i % 2 == 0 {
                    // Even indices: literal text; create a String literal
                    let lit = ast::Literal::String(part);
                    self.ast.add_expr(Expr::Literal(lit), span)
                } else {
                    // Odd indices: expression source code; parse and merge
                    let tokens =
                        Lexer::new(&part).lex().map_err(|e| {
                            Error::parse(
                                span,
                                e.to_string(),
                                vec![],
                            )
                        })?;

                    let parsed = Parser::parse_tokens(
                        tokens,
                        self.interner,
                    )
                    .map_err(|e| {
                            Error::parse(
                                span,
                                e.to_string(),
                                vec![],
                            )
                        })?;

                    // Should produce exactly one expression statement
                    let expr = parsed
                        .stmts
                        .first()
                        .and_then(|stmt_id| {
                            parsed.ast.get_stmt(*stmt_id)
                        })
                        .and_then(|stmt| match stmt {
                            ast::Stmt::Expr(expr_id) => {
                                Some(*expr_id)
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            let msg = if part.trim().is_empty() {
                                "interpolation requires an expression"
                                    .into()
                            } else {
                                format!(
                                    "interpolation requires an expression, got `{}`",
                                    part
                                )
                            };
                            Error::parse(span, msg, vec![])
                        })?;

                    // Copy the expression from the parsed AST into our AST
                    MergeCtx::new(&mut self.ast, &parsed.ast)
                        .expr(expr, span)
                }
            })
            .collect();

        ids.map(Expr::Interpolation)
    }
}

// MergeCtx: copies AST nodes from a source AST into a target AST.

/// Context for merging nodes from a source `Ast` into a target `Ast`.
///
/// Used by interpolation lowering to copy parsed sub-expressions into the
/// main AST.
struct MergeCtx<'a> {
    target: &'a mut Ast,
    source: &'a Ast,
}

impl<'a> MergeCtx<'a> {
    fn new(target: &'a mut Ast, source: &'a Ast) -> Self {
        Self { target, source }
    }

    /// Merge a `RefTarget` from source AST into target AST.
    fn ref_target(&mut self, rt: &RefTarget, span: Span) -> Result<RefTarget> {
        match rt {
            RefTarget::Inline(dbref) => {
                self.dbref(dbref, span).map(RefTarget::Inline)
            }
            RefTarget::Expr(e) => self.expr(*e, span).map(RefTarget::Expr),
        }
    }

    /// Merge a `DbRef` from source AST into target AST.
    ///
    /// Recursively copies subscript expressions.
    fn dbref(&mut self, dbref: &DbRef, span: Span) -> Result<DbRef> {
        let mut merge =
            |subs: &SmallVec<[SubscriptElem; 4]>| -> Result<SmallVec<_>> {
                subs.iter()
                    .map(|elem| match elem {
                        SubscriptElem::Elem(e) => {
                            self.expr(*e, span).map(SubscriptElem::Elem)
                        }
                        SubscriptElem::Spread(e) => {
                            self.expr(*e, span).map(SubscriptElem::Spread)
                        }
                    })
                    .collect()
            };
        match dbref {
            DbRef::Local(name, subs) => Ok(DbRef::Local(*name, merge(subs)?)),
            DbRef::Global(name, subs) => Ok(DbRef::Global(*name, merge(subs)?)),
        }
    }

    /// Merge an `AstTypeExprId` from source AST into target AST.
    fn type_expr(
        &mut self,
        id: AstTypeExprId,
        span: Span,
    ) -> Result<AstTypeExprId> {
        let te = self
            .source
            .get_type_expr(id)
            .ok_or_else(|| Error::parse(span, "invalid type expr id", vec![]))?
            .clone();
        let new_te = match te {
            AstTypeExpr::Wildcard => AstTypeExpr::Wildcard,
            AstTypeExpr::Named(n) => AstTypeExpr::Named(n),
            AstTypeExpr::App(name, args) => {
                let new_args: Result<SmallVec<_>> =
                    args.iter().map(|&a| self.type_expr(a, span)).collect();
                AstTypeExpr::App(name, new_args?)
            }
            AstTypeExpr::VarApp(name, args) => {
                let new_args: Result<SmallVec<_>> =
                    args.iter().map(|&a| self.type_expr(a, span)).collect();
                AstTypeExpr::VarApp(name, new_args?)
            }
            AstTypeExpr::Fn(params, ret) => {
                let new_params: Result<SmallVec<_>> =
                    params.iter().map(|&p| self.type_expr(p, span)).collect();
                let new_ret = self.type_expr(ret, span)?;
                AstTypeExpr::Fn(new_params?, new_ret)
            }
            AstTypeExpr::Tuple(elems) => {
                let new_elems: Result<SmallVec<_>> =
                    elems.iter().map(|&e| self.type_expr(e, span)).collect();
                AstTypeExpr::Tuple(new_elems?)
            }
            AstTypeExpr::Union(members) => {
                let new_members: Result<SmallVec<_>> =
                    members.iter().map(|&m| self.type_expr(m, span)).collect();
                AstTypeExpr::Union(new_members?)
            }
            AstTypeExpr::Object(fields) => {
                let new_fields: Result<SmallVec<_>> = fields
                    .into_iter()
                    .map(|(name, ty_id)| {
                        self.type_expr(ty_id, span).map(|new_id| (name, new_id))
                    })
                    .collect();
                AstTypeExpr::Object(new_fields?)
            }
            AstTypeExpr::AssocType { class, name } => {
                AstTypeExpr::AssocType { class, name }
            }
            AstTypeExpr::TupleConstructor { arity, fixed } => {
                let new_fixed: Result<SmallVec<_>> = fixed
                    .iter()
                    .map(|&(pos, te)| {
                        self.type_expr(te, span).map(|id| (pos, id))
                    })
                    .collect();
                AstTypeExpr::TupleConstructor {
                    arity,
                    fixed: new_fixed?,
                }
            }
        };
        self.target.add_type_expr(new_te, span)
    }

    /// Merge a `MatchPatternId` from source AST into target AST.
    fn pattern(
        &mut self,
        id: MatchPatternId,
        span: Span,
    ) -> Result<MatchPatternId> {
        let pat = self
            .source
            .get_pattern(id)
            .ok_or_else(|| Error::parse(span, "invalid pattern id", vec![]))?
            .clone();
        let new_pat = match pat {
            MatchPattern::Wildcard => MatchPattern::Wildcard,
            MatchPattern::Var(name) => MatchPattern::Var(name),
            MatchPattern::Literal(lit) => MatchPattern::Literal(lit),
            MatchPattern::Variant(ty, var, pats) => {
                let new_pats: Result<SmallVec<_>> =
                    pats.iter().map(|&p| self.pattern(p, span)).collect();
                MatchPattern::Variant(ty, var, new_pats?)
            }
            MatchPattern::NakedVariant(var, pats) => {
                let new_pats: Result<SmallVec<_>> =
                    pats.iter().map(|&p| self.pattern(p, span)).collect();
                MatchPattern::NakedVariant(var, new_pats?)
            }
            MatchPattern::Object(fields) => {
                let new_fields: Result<SmallVec<_>> = fields
                    .into_iter()
                    .map(|(name, pat_id)| {
                        self.pattern(pat_id, span).map(|new_id| (name, new_id))
                    })
                    .collect();
                MatchPattern::Object(new_fields?)
            }
            MatchPattern::Tuple(pats) => {
                let new_pats: Result<SmallVec<_>> =
                    pats.iter().map(|&p| self.pattern(p, span)).collect();
                MatchPattern::Tuple(new_pats?)
            }
            MatchPattern::Array(pats, rest) => {
                let new_pats: Result<SmallVec<_>> =
                    pats.iter().map(|&p| self.pattern(p, span)).collect();
                MatchPattern::Array(new_pats?, rest)
            }
            MatchPattern::Is(name, ty_id) => {
                let new_ty = self.type_expr(ty_id, span)?;
                MatchPattern::Is(name, new_ty)
            }
        };
        self.target.add_pattern(new_pat)
    }

    /// Merge a `WriteExpr` from source AST into target AST.
    fn write_expr(&mut self, w: &WriteExpr, span: Span) -> Result<WriteExpr> {
        let new_expr = self.expr(w.expr, span)?;
        let new_target = match w.target {
            OutputTarget::Stdout => OutputTarget::Stdout,
            OutputTarget::Stderr => OutputTarget::Stderr,
            OutputTarget::File(e) => OutputTarget::File(self.expr(e, span)?),
        };
        Ok(WriteExpr {
            expr: new_expr,
            format: w.format,
            target: new_target,
        })
    }

    /// Merge a `TypePattern` from source AST into target AST.
    fn type_pattern(
        &mut self,
        pat: &TypePattern,
        span: Span,
    ) -> Result<TypePattern> {
        match pat {
            TypePattern::Type(ty_id) => {
                let new_ty = self.type_expr(*ty_id, span)?;
                Ok(TypePattern::Type(new_ty))
            }
            TypePattern::Variant(ty, var) => {
                Ok(TypePattern::Variant(ty.clone(), *var))
            }
            TypePattern::NakedVariant(var) => {
                Ok(TypePattern::NakedVariant(*var))
            }
            TypePattern::VariantWildcard(ty, var) => {
                Ok(TypePattern::VariantWildcard(ty.clone(), *var))
            }
            TypePattern::NakedVariantWildcard(var) => {
                Ok(TypePattern::NakedVariantWildcard(*var))
            }
            TypePattern::VariantBind(ty, var, binds) => {
                Ok(TypePattern::VariantBind(ty.clone(), *var, binds.clone()))
            }
            TypePattern::NakedVariantBind(var, binds) => {
                Ok(TypePattern::NakedVariantBind(*var, binds.clone()))
            }
            TypePattern::Object(fields) => {
                let new_fields: Result<SmallVec<_>> = fields
                    .iter()
                    .map(|(name, ty_id)| {
                        self.type_expr(*ty_id, span)
                            .map(|new_id| (*name, new_id))
                    })
                    .collect();
                Ok(TypePattern::Object(new_fields?))
            }
        }
    }

    /// Merge a statement from source AST into target AST.
    fn stmt(&mut self, stmt_id: StmtId, span: Span) -> Result<StmtId> {
        let stmt = self
            .source
            .get_stmt(stmt_id)
            .ok_or_else(|| Error::parse(span, "invalid stmt id", vec![]))?
            .clone();
        let new_stmt = match stmt {
            Stmt::Let(pat, ty_ann, expr, vis) => {
                let new_ty =
                    ty_ann.map(|t| self.type_expr(t, span)).transpose()?;
                let new_expr = self.expr(expr, span)?;
                Stmt::Let(pat, new_ty, new_expr, vis)
            }
            Stmt::Expr(e) => {
                let new_e = self.expr(e, span)?;
                Stmt::Expr(new_e)
            }
            Stmt::Fun {
                name,
                type_params,
                params,
                ret,
                body,
                vis,
            } => {
                let new_params: Result<SmallVec<_>> = params
                    .into_iter()
                    .map(|(n, ty_opt)| {
                        let new_ty = ty_opt
                            .map(|t| self.type_expr(t, span))
                            .transpose()?;
                        Ok((n, new_ty))
                    })
                    .collect();
                let new_ret =
                    ret.map(|t| self.type_expr(t, span)).transpose()?;
                let new_body = self.expr(body, span)?;
                Stmt::Fun {
                    name,
                    type_params,
                    params: new_params?,
                    ret: new_ret,
                    body: new_body,
                    vis,
                }
            }
            Stmt::Type {
                name,
                type_params,
                def,
                vis,
                pragmas,
            } => {
                let new_def = match def {
                    TypeDefAst::Sum(variants) => {
                        let new_variants: Result<SmallVec<_>> = variants
                            .into_iter()
                            .map(|v| {
                                let new_payloads: Result<SmallVec<_>> = v
                                    .payloads
                                    .iter()
                                    .map(|&p| self.type_expr(p, span))
                                    .collect();
                                Ok(VariantAst {
                                    name: v.name,
                                    payloads: new_payloads?,
                                })
                            })
                            .collect();
                        TypeDefAst::Sum(new_variants?)
                    }
                };
                Stmt::Type {
                    name,
                    type_params,
                    def: new_def,
                    vis,
                    pragmas,
                }
            }
            Stmt::Newtype {
                name,
                type_params,
                target: ty,
                vis,
                repr_vis,
                pragmas,
            } => {
                let new_ty = self.type_expr(ty, span)?;
                Stmt::Newtype {
                    name,
                    type_params,
                    target: new_ty,
                    vis,
                    repr_vis,
                    pragmas,
                }
            }
            Stmt::Union {
                name,
                type_params,
                members,
                vis,
                pragmas,
            } => {
                let new_members: Result<SmallVec<_>> =
                    members.iter().map(|&m| self.type_expr(m, span)).collect();
                Stmt::Union {
                    name,
                    type_params,
                    members: new_members?,
                    vis,
                    pragmas,
                }
            }
            Stmt::Module { name, body } => {
                let new_body: Result<Vec<_>> =
                    body.iter().map(|&s| self.stmt(s, span)).collect();
                Stmt::Module {
                    name,
                    body: new_body?,
                }
            }
            Stmt::Import(import) => Stmt::Import(import),
            Stmt::ClassDef {
                name,
                class_params,
                self_var,
                supers,
                assoc_types,
                methods,
                pragmas,
            } => {
                let new_methods: Result<SmallVec<_>> = methods
                    .into_iter()
                    .map(|m| {
                        let new_params: Result<SmallVec<_>> = m
                            .params
                            .into_iter()
                            .map(|(n, ty_opt)| {
                                let new_ty = ty_opt
                                    .map(|t| self.type_expr(t, span))
                                    .transpose()?;
                                Ok((n, new_ty))
                            })
                            .collect();
                        let new_ret = m
                            .ret
                            .map(|t| self.type_expr(t, span))
                            .transpose()?;
                        Ok(ast::AstClassMethodSig {
                            name: m.name,
                            type_params: m.type_params,
                            params: new_params?,
                            ret: new_ret,
                            span: m.span,
                        })
                    })
                    .collect();
                // `TypeClass` variants can contain `AstTypeExprId`s that
                // are indices into the source arena; remap them into the
                // target arena so they don't become stale.
                let new_supers: Result<SmallVec<_>> = supers
                    .into_iter()
                    .map(|tc| tc.try_map(|t| self.type_expr(t, span)))
                    .collect();
                Stmt::ClassDef {
                    name,
                    class_params,
                    self_var,
                    supers: new_supers?,
                    assoc_types,
                    methods: new_methods?,
                    pragmas,
                }
            }
            Stmt::ClassInstance {
                class_name,
                class_args,
                type_params,
                for_type,
                constraints,
                assoc_types,
                methods,
            } => {
                let new_class_args: Result<SmallVec<_>> = class_args
                    .iter()
                    .map(|&t| self.type_expr(t, span))
                    .collect();
                let new_for_type = self.type_expr(for_type, span)?;
                // Remap `AstTypeExprId`s inside `TypeClass` variants from
                // the source arena into the target arena.
                let new_constraints: Result<SmallVec<_>> = constraints
                    .into_iter()
                    .map(|(name, cs)| {
                        let new_cs: Result<SmallVec<_>> = cs
                            .into_iter()
                            .map(|tc| tc.try_map(|t| self.type_expr(t, span)))
                            .collect();
                        Ok((name, new_cs?))
                    })
                    .collect();
                let new_assoc_types: Result<SmallVec<_>> = assoc_types
                    .into_iter()
                    .map(|a| {
                        let new_constraint = a
                            .constraint
                            .map(|tc| tc.try_map(|t| self.type_expr(t, span)))
                            .transpose()?;
                        let new_target = self.type_expr(a.target, span)?;
                        Ok(ast::AssocTypeDef {
                            name: a.name,
                            constraint: new_constraint,
                            target: new_target,
                            span: a.span,
                        })
                    })
                    .collect();
                let new_methods: Result<SmallVec<_>> = methods
                    .into_iter()
                    .map(|m| {
                        let new_params: Result<SmallVec<_>> = m
                            .params
                            .into_iter()
                            .map(|(n, ty_opt)| {
                                let new_ty = ty_opt
                                    .map(|t| self.type_expr(t, span))
                                    .transpose()?;
                                Ok((n, new_ty))
                            })
                            .collect();
                        let new_ret = m
                            .ret
                            .map(|t| self.type_expr(t, span))
                            .transpose()?;
                        let new_body = self.expr(m.body, span)?;
                        Ok(ast::InstanceMethodDef {
                            name: m.name,
                            type_params: m.type_params,
                            params: new_params?,
                            ret: new_ret,
                            body: new_body,
                            span: m.span,
                        })
                    })
                    .collect();
                Stmt::ClassInstance {
                    class_name,
                    class_args: new_class_args?,
                    type_params,
                    for_type: new_for_type,
                    constraints: new_constraints?,
                    assoc_types: new_assoc_types?,
                    methods: new_methods?,
                }
            }
        };
        self.target.add_stmt(new_stmt, span)
    }

    /// Merge an expression from a parsed AST into the target AST.
    ///
    /// Recursively copies the expression and all its sub-expressions,
    /// statements, type expressions, and patterns.
    fn expr(&mut self, expr_id: ExprId, span: Span) -> Result<ExprId> {
        let expr = self
            .source
            .get_expr(expr_id)
            .ok_or_else(|| Error::parse(span, "invalid expression id", vec![]))?
            .clone();

        let new_expr = match expr {
            Expr::Literal(lit) => Expr::Literal(lit),
            Expr::Interpolation(parts) => {
                let new_parts: Result<SmallVec<_>> =
                    parts.iter().map(|&id| self.expr(id, span)).collect();
                Expr::Interpolation(new_parts?)
            }
            Expr::Var(name) => Expr::Var(name),
            Expr::Intrinsic(op, ref rt, value, txn) => {
                let new_rt = self.ref_target(rt, span)?;
                let new_val = value.map(|v| self.expr(v, span)).transpose()?;
                Expr::Intrinsic(op, new_rt, new_val, txn)
            }
            Expr::Binary(lhs, op, rhs) => {
                let new_lhs = self.expr(lhs, span)?;
                let new_rhs = self.expr(rhs, span)?;
                Expr::Binary(new_lhs, op, new_rhs)
            }
            Expr::Unary(op, operand) => {
                let new_op = self.expr(operand, span)?;
                Expr::Unary(op, new_op)
            }
            Expr::Call(callee, args) => {
                let new_callee = self.expr(callee, span)?;
                let new_args: Result<SmallVec<_>> =
                    args.iter().map(|&id| self.expr(id, span)).collect();
                Expr::Call(new_callee, new_args?)
            }
            Expr::Object(entries) => {
                let new_entries: Result<Vec<_>> = entries
                    .into_iter()
                    .map(|entry| match entry {
                        ObjectEntry::Field(k, v) => self
                            .expr(v, span)
                            .map(|new_v| ObjectEntry::Field(k, new_v)),
                        ObjectEntry::Spread(e) => {
                            self.expr(e, span).map(ObjectEntry::Spread)
                        }
                    })
                    .collect();
                Expr::Object(new_entries?)
            }
            Expr::Array(elems) => {
                let new_elems: Result<Vec<_>> = elems
                    .into_iter()
                    .map(|elem| match elem {
                        ArrayElem::Elem(e) => {
                            self.expr(e, span).map(ArrayElem::Elem)
                        }
                        ArrayElem::Spread(e) => {
                            self.expr(e, span).map(ArrayElem::Spread)
                        }
                    })
                    .collect();
                Expr::Array(new_elems?)
            }
            Expr::Tuple(elems) => {
                let new_elems: Result<SmallVec<_>> =
                    elems.iter().map(|&id| self.expr(id, span)).collect();
                Expr::Tuple(new_elems?)
            }
            Expr::MapLit(entries) => {
                let new_entries: Result<SmallVec<_>> = entries
                    .into_iter()
                    .map(|(k, v)| {
                        let new_k = self.expr(k, span)?;
                        let new_v = self.expr(v, span)?;
                        Ok((new_k, new_v))
                    })
                    .collect();
                Expr::MapLit(new_entries?)
            }
            Expr::TupleIndex(base, idx) => {
                let new_base = self.expr(base, span)?;
                Expr::TupleIndex(new_base, idx)
            }
            Expr::Index(base, idx) => {
                let new_base = self.expr(base, span)?;
                let new_idx = self.expr(idx, span)?;
                Expr::Index(new_base, new_idx)
            }
            Expr::OptionalIndex(base, idx) => {
                let new_base = self.expr(base, span)?;
                let new_idx = self.expr(idx, span)?;
                Expr::OptionalIndex(new_base, new_idx)
            }
            Expr::Field(base, field) => {
                let new_base = self.expr(base, span)?;
                Expr::Field(new_base, field)
            }
            Expr::OptionalField(base, field) => {
                let new_base = self.expr(base, span)?;
                Expr::OptionalField(new_base, field)
            }
            Expr::Variant(ty, var, args) => {
                let new_args: Result<SmallVec<_>> =
                    args.iter().map(|&id| self.expr(id, span)).collect();
                Expr::Variant(ty, var, new_args?)
            }
            Expr::NakedVariant(var, args) => {
                let new_args: Result<SmallVec<_>> =
                    args.iter().map(|&id| self.expr(id, span)).collect();
                Expr::NakedVariant(var, new_args?)
            }
            Expr::Path(segments) => Expr::Path(segments),
            Expr::ClassMethod(class, method, args) => {
                let new_args: Result<SmallVec<_>> =
                    args.iter().map(|&id| self.expr(id, span)).collect();
                Expr::ClassMethod(class, method, new_args?)
            }
            Expr::ClassMethodRef(class, type_args, method) => {
                let new_type_args: Result<SmallVec<_>> = type_args
                    .iter()
                    .map(|&id| self.type_expr(id, span))
                    .collect();
                Expr::ClassMethodRef(class, new_type_args?, method)
            }
            Expr::NakedClassMethod(method, args) => {
                let new_args: Result<SmallVec<_>> =
                    args.iter().map(|&id| self.expr(id, span)).collect();
                Expr::NakedClassMethod(method, new_args?)
            }
            Expr::NakedClassMethodRef(method) => {
                Expr::NakedClassMethodRef(method)
            }
            Expr::Is(expr, pat) => {
                let new_expr = self.expr(expr, span)?;
                let new_pat = self.type_pattern(&pat, span)?;
                Expr::Is(new_expr, new_pat)
            }
            Expr::As(expr, ty) => {
                let new_expr = self.expr(expr, span)?;
                let new_ty = self.type_expr(ty, span)?;
                Expr::As(new_expr, new_ty)
            }
            Expr::Read(expr, ty) => {
                let new_expr = self.expr(expr, span)?;
                let new_ty = self.type_expr(ty, span)?;
                Expr::Read(new_expr, new_ty)
            }
            Expr::Block(stmts, tail) => {
                let new_stmts: Result<Vec<_>> =
                    stmts.iter().map(|&s| self.stmt(s, span)).collect();
                let new_tail = tail.map(|e| self.expr(e, span)).transpose()?;
                Expr::Block(new_stmts?, new_tail)
            }
            Expr::If(cond, then, els) => {
                let new_cond = self.expr(cond, span)?;
                let new_then = self.expr(then, span)?;
                let new_els = els.map(|e| self.expr(e, span)).transpose()?;
                Expr::If(new_cond, new_then, new_els)
            }
            Expr::Match(scrut, arms) => {
                let new_scrut = self.expr(scrut, span)?;
                let new_arms: Result<Vec<_>> = arms
                    .into_iter()
                    .map(|arm| {
                        let new_pat = self.pattern(arm.pattern, span)?;
                        let new_guard = arm
                            .guard
                            .map(|g| self.expr(g, span))
                            .transpose()?;
                        let new_body = self.expr(arm.body, span)?;
                        Ok(MatchArm {
                            pattern: new_pat,
                            guard: new_guard,
                            body: new_body,
                        })
                    })
                    .collect();
                Expr::Match(new_scrut, new_arms?)
            }
            Expr::Closure {
                type_params,
                params,
                ret,
                body,
            } => {
                let new_params: Result<SmallVec<_>> = params
                    .into_iter()
                    .map(|(n, ty_opt)| {
                        let new_ty = ty_opt
                            .map(|t| self.type_expr(t, span))
                            .transpose()?;
                        Ok((n, new_ty))
                    })
                    .collect();
                let new_ret =
                    ret.map(|t| self.type_expr(t, span)).transpose()?;
                let new_body = self.expr(body, span)?;
                Expr::Closure {
                    type_params,
                    params: new_params?,
                    ret: new_ret,
                    body: new_body,
                }
            }
            Expr::Postfix(op, expr) => {
                let new_expr = self.expr(expr, span)?;
                Expr::Postfix(op, new_expr)
            }
            Expr::Range(start, end, incl) => {
                let new_start = self.expr(start, span)?;
                let new_end = self.expr(end, span)?;
                Expr::Range(new_start, new_end, incl)
            }
            Expr::Annotate(expr, ty) => {
                let new_expr = self.expr(expr, span)?;
                let new_ty = self.type_expr(ty, span)?;
                Expr::Annotate(new_expr, new_ty)
            }
            Expr::Json(entries) => {
                let new_entries: Result<Vec<_>> = entries
                    .into_iter()
                    .map(|(k, v)| self.expr(v, span).map(|new_v| (k, new_v)))
                    .collect();
                Expr::Json(new_entries?)
            }
            Expr::JsonAccess(expr, kind, key) => {
                let new_expr = self.expr(expr, span)?;
                let new_key = match key {
                    JsonAccessKey::Field(f) => JsonAccessKey::Field(f),
                    JsonAccessKey::Expr(e) => {
                        JsonAccessKey::Expr(self.expr(e, span)?)
                    }
                };
                Expr::JsonAccess(new_expr, kind, new_key)
            }
            Expr::Regex(pat, cache_idx) => Expr::Regex(pat, cache_idx),
            Expr::Matches(lhs, rhs) => {
                let new_lhs = self.expr(lhs, span)?;
                let new_rhs = self.expr(rhs, span)?;
                Expr::Matches(new_lhs, new_rhs)
            }
            Expr::Catch(expr, handler) => {
                let new_expr = self.expr(expr, span)?;
                let new_handler = self.expr(handler, span)?;
                Expr::Catch(new_expr, new_handler)
            }
            Expr::Write(out) => {
                let new_out = self.write_expr(&out, span)?;
                Expr::Write(new_out)
            }
            Expr::Raise(expr) => {
                let new_expr = self.expr(expr, span)?;
                Expr::Raise(new_expr)
            }
            Expr::Loop {
                seed,
                state_param,
                cont_param,
                body,
            } => {
                let new_seed = self.expr(seed, span)?;
                let new_state_ty = state_param
                    .1
                    .map(|t| self.type_expr(t, span))
                    .transpose()?;
                let new_cont_ty = cont_param
                    .1
                    .map(|t| self.type_expr(t, span))
                    .transpose()?;
                let new_body = self.expr(body, span)?;
                Expr::Loop {
                    seed: new_seed,
                    state_param: (state_param.0, new_state_ty),
                    cont_param: (cont_param.0, new_cont_ty),
                    body: new_body,
                }
            }
            Expr::Transaction(txn_expr) => {
                let new_stmts: Result<Vec<_>> = txn_expr
                    .stmts
                    .iter()
                    .map(|&s| self.stmt(s, span))
                    .collect();
                let new_tail =
                    txn_expr.expr.map(|e| self.expr(e, span)).transpose()?;
                let new_timeout = txn_expr
                    .modifiers
                    .timeout
                    .map(|e| self.expr(e, span))
                    .transpose()?;
                Expr::Transaction(ast::TransactionExpr {
                    id: txn_expr.id,
                    stmts: new_stmts?,
                    expr: new_tail,
                    modifiers: TransactionModifiers {
                        conflict: txn_expr.modifiers.conflict,
                        timeout: new_timeout,
                        retries: txn_expr.modifiers.retries,
                        isolation: txn_expr.modifiers.isolation,
                    },
                })
            }
            Expr::DefaultValue => Expr::DefaultValue,
            Expr::Ref(ref dbref) => {
                let new_dbref = self.dbref(dbref, span)?;
                Expr::Ref(new_dbref)
            }
        };

        self.target.add_expr(new_expr, span)
    }
}
